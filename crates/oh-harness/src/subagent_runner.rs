//! Engine-bound subagent runner injected into the `SubagentManager`.
//!
//! This is the seam that keeps the dependency graph acyclic. `oh-services`
//! (where `SubagentManager` lives) sits below `oh-engine`, so it cannot build a
//! `QueryContext` or call [`oh_engine::run_subagent`] itself. The harness sits
//! on top of everything, so it constructs this [`oh_types::subagent::SubagentRunner`]
//! and hands it to the manager. When the manager drives an in-process spawn it
//! calls [`EngineSubagentRunner::run`], which builds a CHILD `QueryContext` and
//! runs the nested query.
//!
//! The child context inherits from the parent:
//! - a tool-registry **view** filtered by the resolved agent definition's
//!   `ToolPolicy` (already intersected with the parent's tools by the manager
//!   and passed in as `allowed_tools`),
//! - the SAME `hook_executor` (so blocks/webhooks apply to the child), fanned in
//!   with a fresh [`oh_services::subagent::TrajectoryRecorder`] for the child
//!   session whose `parent_session_id` is the parent's session,
//! - the SAME `permission_checker`,
//! - `parent_id` = the spawning agent.

use std::sync::Arc;

use async_trait::async_trait;
use oh_api::StreamingApiClient;
use oh_engine::{run_subagent, QueryContext};
use oh_hooks::{AggregatedHookResult, HookEvent, HookExecutorTrait};
use oh_permissions::PermissionChecker;
use oh_services::sessions::{SessionRecord, SessionStatus, SessionStore};
use oh_services::subagent::TrajectoryRecorder;
use oh_tools::{create_default_tool_registry, ToolRegistry};
use oh_types::subagent::{AgentId, SpawnRequest, SubagentRunner};
use std::path::PathBuf;
use std::time::SystemTime;
use uuid::Uuid;

/// Fans a lifecycle event out to two [`HookExecutorTrait`]s: the parent's
/// executor (blocks/webhooks) and the child's trajectory recorder.
///
/// A block from EITHER aborts the action, since `AggregatedHookResult::blocked`
/// is the OR of all results.
struct FanoutHookExecutor {
    parent: Arc<dyn HookExecutorTrait>,
    recorder: Arc<dyn HookExecutorTrait>,
}

#[async_trait]
impl HookExecutorTrait for FanoutHookExecutor {
    async fn execute(&self, event: HookEvent, payload: serde_json::Value) -> AggregatedHookResult {
        // Record first (never blocks), then run the parent's hooks (may block).
        let _ = self.recorder.execute(event, payload.clone()).await;
        self.parent.execute(event, payload).await
    }
}

/// Builds child `QueryContext`s and runs nested subagent queries.
pub struct EngineSubagentRunner {
    api_client: Arc<dyn StreamingApiClient>,
    permission_checker: Arc<PermissionChecker>,
    hook_executor: Option<Arc<dyn HookExecutorTrait>>,
    session_store: Option<Arc<SessionStore>>,
    parent_id: AgentId,
    parent_session_id: Option<String>,
    cwd: PathBuf,
    model: String,
    system_prompt: String,
    max_tokens: u32,
    max_turns: u32,
}

#[allow(clippy::too_many_arguments)]
impl EngineSubagentRunner {
    pub fn new(
        api_client: Arc<dyn StreamingApiClient>,
        permission_checker: Arc<PermissionChecker>,
        hook_executor: Option<Arc<dyn HookExecutorTrait>>,
        session_store: Option<Arc<SessionStore>>,
        parent_id: AgentId,
        parent_session_id: Option<String>,
        cwd: PathBuf,
        model: String,
        system_prompt: String,
        max_tokens: u32,
        max_turns: u32,
    ) -> Self {
        Self {
            api_client,
            permission_checker,
            hook_executor,
            session_store,
            parent_id,
            parent_session_id,
            cwd,
            model,
            system_prompt,
            max_tokens,
            max_turns,
        }
    }

    /// Build the child tool-registry view, scoped to `allowed_tools`.
    fn child_registry(allowed_tools: &[String]) -> ToolRegistry {
        let mut registry = create_default_tool_registry();
        if !allowed_tools.is_empty() {
            registry.retain(|name| allowed_tools.contains(&name.to_string()));
        }
        registry
    }
}

#[async_trait]
impl SubagentRunner for EngineSubagentRunner {
    async fn run(&self, req: SpawnRequest, allowed_tools: Vec<String>) -> Result<String, String> {
        // Fresh child session id, linked to the parent.
        let child_session_id = format!("session-{}", Uuid::new_v4());

        // Build the hook executor for the child: the parent's executor plus a
        // trajectory recorder (when a session store is available).
        let hook_executor: Option<Arc<dyn HookExecutorTrait>> = match (
            &self.hook_executor,
            &self.session_store,
        ) {
            (Some(parent), Some(store)) => {
                // Create the child session row so recorded messages have a home.
                let rec = SessionRecord {
                    id: child_session_id.clone(),
                    name: None,
                    project_root: self.cwd.clone(),
                    model: req.model.clone().unwrap_or_else(|| self.model.clone()),
                    created_at: SystemTime::now(),
                    updated_at: SystemTime::now(),
                    message_count: 0,
                    status: SessionStatus::Active,
                    parent_session_id: self.parent_session_id.clone(),
                };
                if let Err(e) = store.create_session(&rec).await {
                    tracing::warn!("subagent: failed to create child session: {e}");
                }
                let recorder: Arc<dyn HookExecutorTrait> = Arc::new(TrajectoryRecorder::new(
                    Arc::clone(store),
                    child_session_id.clone(),
                ));
                Some(Arc::new(FanoutHookExecutor {
                    parent: Arc::clone(parent),
                    recorder,
                }))
            }
            (Some(parent), None) => Some(Arc::clone(parent)),
            _ => None,
        };

        let registry = Self::child_registry(&allowed_tools);

        let mut tool_metadata = std::collections::HashMap::new();
        tool_metadata.insert(
            "subagent_type".to_string(),
            serde_json::json!(req.subagent_type),
        );

        let ctx = QueryContext {
            api_client: Arc::clone(&self.api_client),
            tool_registry: Arc::new(registry),
            permission_checker: Arc::clone(&self.permission_checker),
            cwd: self.cwd.clone(),
            model: req.model.clone().unwrap_or_else(|| self.model.clone()),
            system_prompt: self.system_prompt.clone(),
            max_tokens: self.max_tokens,
            permission_prompt: None,
            ask_user_prompt: None,
            max_turns: self.max_turns,
            hook_executor,
            tool_metadata,
            agent_id: req.agent_id.clone(),
            parent_id: Some(self.parent_id.clone()),
            session_id: Some(child_session_id),
            // A subagent does not (in Phase 1) spawn further subagents or own a
            // task control plane; those stay None to avoid unbounded recursion.
            subagents: None,
            tasks: None,
        };

        run_subagent(ctx, req.prompt).await.map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::Stream;
    use oh_config::PermissionSettings;
    use oh_types::api::*;
    use oh_types::messages::*;
    use std::pin::Pin;

    /// Fake client that yields a fixed assistant message.
    struct FakeClient {
        text: String,
    }

    #[async_trait]
    impl StreamingApiClient for FakeClient {
        async fn stream_message(
            &self,
            _request: ApiMessageRequest,
        ) -> Result<
            Pin<Box<dyn Stream<Item = Result<ApiStreamEvent, ApiError>> + Send + '_>>,
            ApiError,
        > {
            let msg = ConversationMessage {
                role: Role::Assistant,
                content: vec![ContentBlock::Text(TextBlock::new(self.text.clone()))],
            };
            let events = vec![Ok(ApiStreamEvent::MessageComplete(ApiMessageCompleteEvent {
                message: msg,
                usage: UsageSnapshot::default(),
                stop_reason: Some("end_turn".into()),
            }))];
            Ok(Box::pin(futures::stream::iter(events)))
        }
    }

    fn runner(client: Arc<dyn StreamingApiClient>) -> EngineSubagentRunner {
        EngineSubagentRunner::new(
            client,
            Arc::new(PermissionChecker::new(PermissionSettings::default())),
            None,
            None,
            AgentId::new("main"),
            None,
            PathBuf::from("/tmp"),
            "test-model".into(),
            "system".into(),
            1024,
            5,
        )
    }

    #[tokio::test]
    async fn test_runner_returns_assistant_text() {
        let r = runner(Arc::new(FakeClient {
            text: "child result".into(),
        }));
        let req = SpawnRequest::new(AgentId::new("sub-1"), "general-purpose", "do it");
        let out = r.run(req, Vec::new()).await.unwrap();
        assert_eq!(out, "child result");
    }

    #[test]
    fn test_child_registry_respects_allow_list() {
        let reg = EngineSubagentRunner::child_registry(&["Bash".to_string()]);
        let names = reg.tool_names();
        assert_eq!(names, vec!["Bash".to_string()]);
    }

    #[test]
    fn test_child_registry_empty_keeps_all() {
        let reg = EngineSubagentRunner::child_registry(&[]);
        assert!(reg.tool_names().len() > 1);
    }
}

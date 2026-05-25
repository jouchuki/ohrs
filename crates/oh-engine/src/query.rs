//! Core tool-aware query loop.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use futures::future::join_all;
use oh_api::StreamingApiClient;
use oh_hooks::{HookEvent, HookExecutorTrait};
use oh_permissions::PermissionChecker;
use oh_tools::ToolRegistry;
use oh_types::api::*;
use oh_types::messages::*;
use oh_types::permissions::PermissionRequest;
use oh_types::stream_events::*;
use oh_types::subagent::{AgentId, BackgroundTasks, SubagentSpawner};
use oh_types::tools::ToolExecutionContext;
use opentelemetry::KeyValue;
use tracing::{info_span, Instrument};

/// Callback type for permission prompts.
pub type PermissionPromptFn =
    Arc<dyn Fn(String, String) -> std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send>> + Send + Sync>;

/// Callback type for asking the user.
pub type AskUserPromptFn =
    Arc<dyn Fn(String) -> std::pin::Pin<Box<dyn std::future::Future<Output = String> + Send>> + Send + Sync>;

/// Context shared across a query run.
pub struct QueryContext {
    pub api_client: Arc<dyn StreamingApiClient>,
    pub tool_registry: Arc<ToolRegistry>,
    pub permission_checker: Arc<PermissionChecker>,
    pub cwd: PathBuf,
    pub model: String,
    pub system_prompt: String,
    pub max_tokens: u32,
    pub permission_prompt: Option<PermissionPromptFn>,
    pub ask_user_prompt: Option<AskUserPromptFn>,
    pub max_turns: u32,
    pub hook_executor: Option<Arc<dyn HookExecutorTrait>>,
    pub tool_metadata: std::collections::HashMap<String, serde_json::Value>,
    /// Identity of this agent run. The top-level/main agent uses
    /// `AgentId("main")`; subagents get their own id.
    pub agent_id: AgentId,
    /// The id of the agent that spawned this one, if any.
    pub parent_id: Option<AgentId>,
    /// The persistent session id this run records into (when recording is on).
    pub session_id: Option<String>,
    /// Handle for spawning subagents, threaded into the per-tool
    /// [`ToolExecutionContext`] so the `Agent` tool can reach the orchestrator.
    /// `None` outside the harness (e.g. unit tests).
    pub subagents: Option<Arc<dyn SubagentSpawner>>,
    /// Handle for the background-task control plane, threaded into the per-tool
    /// [`ToolExecutionContext`] for the `Task*` tools.
    pub tasks: Option<Arc<dyn BackgroundTasks>>,
}

impl QueryContext {
    /// Fire a hook if a hook executor is configured. No-op otherwise.
    async fn fire_hook(&self, event: HookEvent, payload: serde_json::Value) {
        if let Some(ref hook_executor) = self.hook_executor {
            hook_executor.execute(event, payload).await;
        }
    }
}

/// Run the conversation loop until the model stops requesting tools.
pub async fn run_query(
    context: &QueryContext,
    messages: &mut Vec<ConversationMessage>,
) -> Result<Vec<(StreamEvent, Option<UsageSnapshot>)>, EngineError> {
    let mut events = Vec::new();

    for turn in 0..context.max_turns {
        let span = info_span!("query_turn", turn.number = turn);

        let turn_events = async {
            context.fire_hook(HookEvent::QueryTurnStart, serde_json::json!({"turn": turn})).await;
            context.fire_hook(HookEvent::PreApiRequest, serde_json::json!({"model": context.model, "turn": turn})).await;

            // Stream API request
            let request = ApiMessageRequest {
                model: context.model.clone(),
                messages: messages.clone(),
                system_prompt: Some(context.system_prompt.clone()),
                max_tokens: context.max_tokens,
                tools: context.tool_registry.to_api_schema(),
            };

            let start = Instant::now();
            let mut stream = context
                .api_client
                .stream_message(request)
                .await
                .map_err(|e| EngineError::ApiError(e.to_string()))?;

            use futures::StreamExt;
            let mut final_message: Option<ConversationMessage> = None;
            let mut usage = UsageSnapshot::default();
            let mut turn_events = Vec::new();

            while let Some(event) = stream.next().await {
                match event {
                    Ok(ApiStreamEvent::TextDelta(delta)) => {
                        turn_events.push((
                            StreamEvent::AssistantTextDelta(AssistantTextDelta {
                                text: delta.text,
                            }),
                            None,
                        ));
                    }
                    Ok(ApiStreamEvent::MessageComplete(complete)) => {
                        final_message = Some(complete.message);
                        usage = complete.usage;
                    }
                    Ok(ApiStreamEvent::ToolUseDelta(_)) => {
                        // Tool-use delta events are not yet processed; skip.
                    }
                    Err(e) => {
                        return Err(EngineError::ApiError(e.to_string()));
                    }
                }
            }

            context.fire_hook(HookEvent::PostApiResponse, serde_json::json!({
                "model": context.model,
                "input_tokens": usage.input_tokens,
                "output_tokens": usage.output_tokens,
            })).await;

            let final_message = final_message
                .ok_or_else(|| EngineError::NoResponse)?;

            context.fire_hook(HookEvent::PrePushMessage, serde_json::json!({"role": "assistant", "turn": turn})).await;
            messages.push(final_message.clone());
            context.fire_hook(HookEvent::PostPushMessage, serde_json::json!({"role": "assistant", "turn": turn})).await;

            turn_events.push((
                StreamEvent::AssistantTurnComplete(AssistantTurnComplete {
                    message: final_message.clone(),
                    usage: usage.clone(),
                }),
                Some(usage.clone()),
            ));

            // Check for tool calls
            let tool_calls = final_message.tool_uses();
            if tool_calls.is_empty() {
                context.fire_hook(HookEvent::QueryTurnEnd, serde_json::json!({"turn": turn, "has_tool_calls": false})).await;
                return Ok((turn_events, true)); // done
            }

            // Execute tool calls
            let tool_results = if tool_calls.len() == 1 {
                let tc = &tool_calls[0];
                turn_events.push((
                    StreamEvent::ToolExecutionStarted(ToolExecutionStarted {
                        tool_name: tc.name.clone(),
                        tool_input: tc.input.clone(),
                    }),
                    None,
                ));

                let result = execute_tool_call(context, &tc.name, &tc.id, &tc.input).await;

                turn_events.push((
                    StreamEvent::ToolExecutionCompleted(ToolExecutionCompleted {
                        tool_name: tc.name.clone(),
                        output: result.content.clone(),
                        is_error: result.is_error,
                    }),
                    None,
                ));

                vec![result]
            } else {
                // Emit start events
                for tc in &tool_calls {
                    turn_events.push((
                        StreamEvent::ToolExecutionStarted(ToolExecutionStarted {
                            tool_name: tc.name.clone(),
                            tool_input: tc.input.clone(),
                        }),
                        None,
                    ));
                }

                // Execute concurrently
                let futures: Vec<_> = tool_calls
                    .iter()
                    .map(|tc| execute_tool_call(context, &tc.name, &tc.id, &tc.input))
                    .collect();
                let results = join_all(futures).await;

                // Emit completion events
                for (tc, result) in tool_calls.iter().zip(results.iter()) {
                    turn_events.push((
                        StreamEvent::ToolExecutionCompleted(ToolExecutionCompleted {
                            tool_name: tc.name.clone(),
                            output: result.content.clone(),
                            is_error: result.is_error,
                        }),
                        None,
                    ));
                }

                results
            };

            // Add tool results as user message
            let tool_result_content: Vec<ContentBlock> = tool_results
                .into_iter()
                .map(|r| ContentBlock::ToolResult(r))
                .collect();

            context.fire_hook(HookEvent::PrePushMessage, serde_json::json!({"role": "user", "content_type": "tool_results", "turn": turn})).await;
            messages.push(ConversationMessage {
                role: Role::User,
                content: tool_result_content,
            });
            context.fire_hook(HookEvent::PostPushMessage, serde_json::json!({"role": "user", "content_type": "tool_results", "turn": turn})).await;
            context.fire_hook(HookEvent::QueryTurnEnd, serde_json::json!({"turn": turn, "has_tool_calls": true})).await;

            Ok((turn_events, false)) // not done, continue
        }
        .instrument(span)
        .await;

        let (turn_events, done) = turn_events?;
        events.extend(turn_events);

        if done {
            return Ok(events);
        }
    }

    Err(EngineError::MaxTurnsExceeded(context.max_turns))
}

/// Run a subagent: a nested [`run_query`] carrying its own [`QueryContext`]
/// (same `hook_executor` + `permission_checker` as the parent), seeded with
/// `prompt`. Fires [`HookEvent::SubagentStart`] before the body and
/// [`HookEvent::SubagentStop`] after it, then returns the final assistant text.
///
/// Phase 0: this delegates to `run_query` with a single seeded user message and
/// returns the concatenated text of the last assistant turn. The Start/Stop
/// hooks bracket the body so blocks/webhooks/recording ride the existing
/// pipeline.
pub async fn run_subagent(ctx: QueryContext, prompt: String) -> Result<String, EngineError> {
    ctx.fire_hook(
        HookEvent::SubagentStart,
        serde_json::json!({
            "agent_id": ctx.agent_id.as_str(),
            "parent_id": ctx.parent_id.as_ref().map(|p| p.as_str()),
            "subagent_type": ctx.tool_metadata.get("subagent_type"),
            "session_id": ctx.session_id,
            "prompt": prompt,
        }),
    )
    .await;

    let mut messages = vec![ConversationMessage::from_user_text(&prompt)];
    let result = run_query(&ctx, &mut messages).await;

    // Extract the text of the final assistant message produced by the run.
    let final_text = messages
        .iter()
        .rev()
        .find(|m| m.role == Role::Assistant)
        .map(|m| m.text())
        .unwrap_or_default();

    // Turn count = number of assistant messages the run produced.
    let turns = messages
        .iter()
        .filter(|m| m.role == Role::Assistant)
        .count();

    ctx.fire_hook(
        HookEvent::SubagentStop,
        serde_json::json!({
            "agent_id": ctx.agent_id.as_str(),
            "parent_id": ctx.parent_id.as_ref().map(|p| p.as_str()),
            "subagent_type": ctx.tool_metadata.get("subagent_type"),
            "session_id": ctx.session_id,
            "ok": result.is_ok(),
            "turns": turns,
            "result": final_text,
        }),
    )
    .await;

    result.map(|_| final_text)
}

/// Execute a single tool call with permission checks and hooks.
async fn execute_tool_call(
    context: &QueryContext,
    tool_name: &str,
    tool_use_id: &str,
    tool_input: &std::collections::HashMap<String, serde_json::Value>,
) -> ToolResultBlock {
    let span = info_span!("tool_call", tool.name = %tool_name);

    let start = Instant::now();

    let result = async {
        let input_value = serde_json::to_value(tool_input).unwrap_or_default();

        // Pre-tool-use hook
        if let Some(ref hook_executor) = context.hook_executor {
            let pre_hooks = hook_executor
                .execute(
                    HookEvent::PreToolUse,
                    serde_json::json!({
                        "tool_name": tool_name,
                        "tool_input": input_value,
                        "event": "pre_tool_use",
                    }),
                )
                .await;
            if pre_hooks.blocked() {
                return ToolResultBlock::new(
                    tool_use_id,
                    pre_hooks.reason().to_string(),
                    true,
                );
            }
        }

        // Look up tool
        let tool = match context.tool_registry.get(tool_name) {
            Some(t) => t,
            None => {
                return ToolResultBlock::new(
                    tool_use_id,
                    format!("Unknown tool: {tool_name}"),
                    true,
                )
            }
        };

        // Permission check
        let file_path = tool_input
            .get("file_path")
            .and_then(|v| v.as_str())
            .map(String::from);
        let command = tool_input
            .get("command")
            .and_then(|v| v.as_str())
            .map(String::from);

        context.fire_hook(HookEvent::PrePermissionCheck, serde_json::json!({"tool_name": tool_name})).await;

        let decision = context.permission_checker.evaluate(&PermissionRequest {
            tool_name,
            is_read_only: tool.is_read_only(&input_value),
            file_path: file_path.as_deref(),
            command: command.as_deref(),
        });

        context.fire_hook(HookEvent::PostPermissionCheck, serde_json::json!({
            "tool_name": tool_name,
            "allowed": decision.allowed,
            "requires_confirmation": decision.requires_confirmation,
        })).await;

        if !decision.allowed {
            let denied = if decision.requires_confirmation {
                if let Some(ref prompt_fn) = context.permission_prompt {
                    let confirmed = prompt_fn(
                        tool_name.to_string(),
                        decision.reason.clone(),
                    )
                    .await;
                    if confirmed { false } else {
                        context.fire_hook(HookEvent::PermissionDenied, serde_json::json!({"tool_name": tool_name, "reason": "user_rejected"})).await;
                        true
                    }
                } else {
                    context.fire_hook(HookEvent::PermissionDenied, serde_json::json!({"tool_name": tool_name, "reason": "no_prompt_fn"})).await;
                    true
                }
            } else {
                context.fire_hook(HookEvent::PermissionDenied, serde_json::json!({"tool_name": tool_name, "reason": decision.reason})).await;
                return ToolResultBlock::new(tool_use_id, decision.reason, true);
            };
            if denied {
                return ToolResultBlock::new(
                    tool_use_id,
                    format!("Permission denied for {tool_name}"),
                    true,
                );
            }
        }

        // Execute tool
        let tool_ctx = ToolExecutionContext {
            cwd: context.cwd.clone(),
            metadata: context.tool_metadata.clone(),
            subagents: context.subagents.clone(),
            tasks: context.tasks.clone(),
        };
        let result = tool.execute(input_value, &tool_ctx).await;

        // Apply hook mutations if the tool returned a hook_action
        if let Some(hook_action) = result.metadata.get("hook_action") {
            apply_hook_action(context, hook_action).await;
        }

        let tool_result = ToolResultBlock::new(tool_use_id, &result.output, result.is_error);

        if result.is_error {
            context.fire_hook(HookEvent::ToolError_, serde_json::json!({
                "tool_name": tool_name,
                "error": result.output,
            })).await;
        }

        context.fire_hook(HookEvent::PostToolUse, serde_json::json!({
            "tool_name": tool_name,
            "tool_input": serde_json::to_value(tool_input).unwrap_or_default(),
            "tool_output": tool_result.content,
            "tool_is_error": tool_result.is_error,
            "event": "post_tool_use",
        })).await;

        tool_result
    }
    .instrument(span)
    .await;

    let elapsed = start.elapsed().as_secs_f64();
    oh_telemetry::TOOL_CALL_DURATION.record(
        elapsed,
        &[
            KeyValue::new("tool_name", tool_name.to_string()),
            KeyValue::new("success", (!result.is_error).to_string()),
        ],
    );

    if result.is_error {
        oh_telemetry::TOOL_ERROR_COUNT.add(
            1,
            &[KeyValue::new("tool_name", tool_name.to_string())],
        );
    }

    result
}

/// Apply a hook mutation from a tool result (e.g., HookManage tool).
async fn apply_hook_action(context: &QueryContext, action: &serde_json::Value) {
    let action_type = action.get("action").and_then(|v| v.as_str()).unwrap_or("");

    match action_type {
        "add" => {
            if let (Some(event_str), Some(hook_value)) = (
                action.get("event").and_then(|v| v.as_str()),
                action.get("hook"),
            ) {
                if let (Ok(event), Ok(hook)) = (
                    serde_json::from_value::<HookEvent>(serde_json::Value::String(event_str.to_string())),
                    serde_json::from_value::<oh_types::hooks::HookDefinition>(hook_value.clone()),
                ) {
                    if let Some(ref executor) = context.hook_executor {
                        // Access the registry via the executor's public handle
                        // For now, fire a hook to signal the addition
                        executor.execute(
                            HookEvent::PluginLoaded,
                            serde_json::json!({
                                "source": "hook_manage",
                                "event": event_str,
                                "hook_type": hook.hook_type(),
                            }),
                        ).await;
                    }
                    tracing::info!(event = %event_str, hook_type = hook.hook_type(), "Hook registered via HookManage tool");
                }
            }
        }
        "clear_event" => {
            if let Some(event_str) = action.get("event").and_then(|v| v.as_str()) {
                tracing::info!(event = %event_str, "Hooks cleared for event via HookManage tool");
            }
        }
        "clear_all" => {
            tracing::info!("All hooks cleared via HookManage tool");
        }
        _ => {}
    }
}

/// Engine errors.
#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("API error: {0}")]
    ApiError(String),
    #[error("model stream finished without a final message")]
    NoResponse,
    #[error("exceeded maximum turn limit ({0})")]
    MaxTurnsExceeded(u32),
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use futures::Stream;
    use oh_config::PermissionSettings;
    use oh_hooks::AggregatedHookResult;
    use oh_permissions::PermissionChecker;
    use std::pin::Pin;
    use std::sync::Mutex as StdMutex;

    /// Fake client that yields a single assistant text message.
    struct FakeApiClient {
        text: String,
    }

    #[async_trait]
    impl StreamingApiClient for FakeApiClient {
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

    /// Hook executor that records every event it sees.
    struct RecordingHookExecutor {
        events: Arc<StdMutex<Vec<HookEvent>>>,
    }

    #[async_trait]
    impl HookExecutorTrait for RecordingHookExecutor {
        async fn execute(
            &self,
            event: HookEvent,
            _payload: serde_json::Value,
        ) -> AggregatedHookResult {
            self.events.lock().unwrap().push(event);
            AggregatedHookResult::default()
        }
    }

    fn make_ctx(text: &str, events: Arc<StdMutex<Vec<HookEvent>>>) -> QueryContext {
        QueryContext {
            api_client: Arc::new(FakeApiClient { text: text.into() }),
            tool_registry: Arc::new(ToolRegistry::new()),
            permission_checker: Arc::new(PermissionChecker::new(PermissionSettings::default())),
            cwd: PathBuf::from("/tmp"),
            model: "test-model".into(),
            system_prompt: "test".into(),
            max_tokens: 1024,
            permission_prompt: None,
            ask_user_prompt: None,
            max_turns: 5,
            hook_executor: Some(Arc::new(RecordingHookExecutor { events })),
            tool_metadata: std::collections::HashMap::new(),
            agent_id: AgentId::new("sub-1"),
            parent_id: Some(AgentId::new("main")),
            session_id: Some("sess-1".into()),
            subagents: None,
            tasks: None,
        }
    }

    #[tokio::test]
    async fn test_run_subagent_returns_assistant_text() {
        let events = Arc::new(StdMutex::new(Vec::new()));
        let ctx = make_ctx("subagent done", Arc::clone(&events));
        let out = run_subagent(ctx, "do the thing".into()).await.unwrap();
        assert_eq!(out, "subagent done");
    }

    #[tokio::test]
    async fn test_run_subagent_fires_start_and_stop_hooks() {
        let events = Arc::new(StdMutex::new(Vec::new()));
        let ctx = make_ctx("ok", Arc::clone(&events));
        let _ = run_subagent(ctx, "go".into()).await.unwrap();

        let seen = events.lock().unwrap();
        assert!(
            seen.contains(&HookEvent::SubagentStart),
            "SubagentStart not fired: {seen:?}"
        );
        assert!(
            seen.contains(&HookEvent::SubagentStop),
            "SubagentStop not fired: {seen:?}"
        );
        // Start must precede Stop.
        let start = seen.iter().position(|e| *e == HookEvent::SubagentStart);
        let stop = seen.iter().position(|e| *e == HookEvent::SubagentStop);
        assert!(start < stop, "Start should precede Stop: {seen:?}");
    }

    // ── Control-plane proof tests ────────────────────────────────────────────
    //
    // A subagent is just a nested `run_query` carrying the SAME hook executor as
    // its parent, so it inherits blocks and webhooks for free. These tests prove
    // both ride the existing pipeline.

    use oh_hooks::HookResult;
    use oh_types::tools::{ToolExecutionContext, ToolResult};

    /// Fake client that issues exactly one `bash` tool call on the first turn,
    /// then returns a final text message on the second turn. This lets us drive
    /// the `PreToolUse` gate from within a subagent run.
    struct ToolThenTextClient {
        turn: std::sync::atomic::AtomicUsize,
        final_text: String,
    }

    #[async_trait]
    impl StreamingApiClient for ToolThenTextClient {
        async fn stream_message(
            &self,
            _request: ApiMessageRequest,
        ) -> Result<
            Pin<Box<dyn Stream<Item = Result<ApiStreamEvent, ApiError>> + Send + '_>>,
            ApiError,
        > {
            use std::sync::atomic::Ordering;
            let n = self.turn.fetch_add(1, Ordering::SeqCst);
            let msg = if n == 0 {
                ConversationMessage {
                    role: Role::Assistant,
                    content: vec![ContentBlock::ToolUse(ToolUseBlock::new(
                        "bash",
                        std::collections::HashMap::new(),
                    ))],
                }
            } else {
                ConversationMessage {
                    role: Role::Assistant,
                    content: vec![ContentBlock::Text(TextBlock::new(self.final_text.clone()))],
                }
            };
            let events = vec![Ok(ApiStreamEvent::MessageComplete(ApiMessageCompleteEvent {
                message: msg,
                usage: UsageSnapshot::default(),
                stop_reason: Some("end_turn".into()),
            }))];
            Ok(Box::pin(futures::stream::iter(events)))
        }
    }

    /// A trivial tool that records when it executes, so a block can be detected
    /// by the *absence* of execution.
    struct SpyTool {
        executed: Arc<std::sync::atomic::AtomicBool>,
    }

    #[async_trait]
    impl oh_tools::Tool for SpyTool {
        fn name(&self) -> &str {
            "bash"
        }
        fn description(&self) -> &str {
            "spy"
        }
        fn input_schema(&self) -> serde_json::Value {
            serde_json::json!({"type": "object", "properties": {}})
        }
        fn is_read_only(&self, _a: &serde_json::Value) -> bool {
            true
        }
        async fn execute(&self, _a: serde_json::Value, _c: &ToolExecutionContext) -> ToolResult {
            self.executed
                .store(true, std::sync::atomic::Ordering::SeqCst);
            ToolResult::success("ran")
        }
    }

    /// Hook executor that BLOCKS a configured event (mirrors `block_on_failure`)
    /// and records every event it sees.
    struct BlockingHookExecutor {
        block_on: HookEvent,
        events: Arc<StdMutex<Vec<HookEvent>>>,
    }

    #[async_trait]
    impl HookExecutorTrait for BlockingHookExecutor {
        async fn execute(
            &self,
            event: HookEvent,
            _payload: serde_json::Value,
        ) -> AggregatedHookResult {
            self.events.lock().unwrap().push(event);
            if event == self.block_on {
                AggregatedHookResult {
                    results: vec![HookResult {
                        success: false,
                        blocked: true,
                        reason: "blocked by policy".into(),
                        ..Default::default()
                    }],
                }
            } else {
                AggregatedHookResult::default()
            }
        }
    }

    fn make_ctx_with_spy(
        executed: Arc<std::sync::atomic::AtomicBool>,
        hook_executor: Arc<dyn HookExecutorTrait>,
    ) -> QueryContext {
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(SpyTool { executed }));
        QueryContext {
            api_client: Arc::new(ToolThenTextClient {
                turn: std::sync::atomic::AtomicUsize::new(0),
                final_text: "done".into(),
            }),
            tool_registry: Arc::new(registry),
            permission_checker: Arc::new(PermissionChecker::new(PermissionSettings::default())),
            cwd: PathBuf::from("/tmp"),
            model: "test-model".into(),
            system_prompt: "test".into(),
            max_tokens: 1024,
            permission_prompt: None,
            ask_user_prompt: None,
            max_turns: 5,
            hook_executor: Some(hook_executor),
            tool_metadata: std::collections::HashMap::new(),
            agent_id: AgentId::new("sub-blocking"),
            parent_id: Some(AgentId::new("main")),
            session_id: Some("sess-block".into()),
            subagents: None,
            tasks: None,
        }
    }

    /// (a) A blocking `PreToolUse` hook aborts the subagent's tool action: the
    /// tool never runs and the tool result carries the block reason.
    #[tokio::test]
    async fn test_blocking_pretooluse_hook_aborts_subagent_action() {
        let executed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let events = Arc::new(StdMutex::new(Vec::new()));
        let hooks = Arc::new(BlockingHookExecutor {
            block_on: HookEvent::PreToolUse,
            events: Arc::clone(&events),
        });
        let ctx = make_ctx_with_spy(Arc::clone(&executed), hooks);

        // The subagent runs to completion (the block becomes a tool error, which
        // the model "sees" and then finishes), but the underlying tool body must
        // never have executed.
        let out = run_subagent(ctx, "use the tool".into()).await.unwrap();
        assert_eq!(out, "done");
        assert!(
            !executed.load(std::sync::atomic::Ordering::SeqCst),
            "tool body ran despite a blocking PreToolUse hook"
        );
        let seen = events.lock().unwrap();
        assert!(seen.contains(&HookEvent::PreToolUse));
        assert!(seen.contains(&HookEvent::SubagentStart));
    }

    /// (b) A webhook-style hook executor is invoked on subagent lifecycle events.
    /// We assert the stub received `SubagentStart` AND `SubagentStop` (the same
    /// dispatch path an `HttpHookDefinition` would ride).
    #[tokio::test]
    async fn test_webhook_style_hook_invoked_on_subagent_lifecycle() {
        let events = Arc::new(StdMutex::new(Vec::new()));
        // A non-blocking executor stands in for an HTTP/webhook hook: it simply
        // observes every lifecycle event.
        let ctx = make_ctx("hello", Arc::clone(&events));
        let _ = run_subagent(ctx, "go".into()).await.unwrap();

        let seen = events.lock().unwrap();
        assert!(
            seen.iter().filter(|e| **e == HookEvent::SubagentStart).count() == 1,
            "webhook should be invoked once on SubagentStart: {seen:?}"
        );
        assert!(
            seen.iter().filter(|e| **e == HookEvent::SubagentStop).count() == 1,
            "webhook should be invoked once on SubagentStop: {seen:?}"
        );
    }
}

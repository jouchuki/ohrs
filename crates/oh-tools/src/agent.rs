//! Spawn subagents for complex tasks tool.

use async_trait::async_trait;
use oh_types::subagent::{AgentId, SpawnRequest, SubagentIsolation};
use oh_types::tools::{ToolExecutionContext, ToolResult};
use uuid::Uuid;

pub struct AgentTool;

#[async_trait]
impl crate::traits::Tool for AgentTool {
    fn name(&self) -> &str {
        "Agent"
    }

    fn description(&self) -> &str {
        "Spawn a local background agent task for complex delegated work."
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "prompt": {
                    "type": "string",
                    "description": "Full prompt for the sub-agent"
                },
                "description": {
                    "type": "string",
                    "description": "Short description of the delegated work"
                },
                "model": {
                    "type": "string",
                    "description": "Optional model override for the sub-agent"
                },
                "subagent_type": {
                    "type": "string",
                    "description": "Type of sub-agent to spawn",
                    "default": "general-purpose"
                },
                "run_in_background": {
                    "type": "boolean",
                    "description": "Whether to run the agent in the background"
                },
                "isolation": {
                    "type": "string",
                    "description": "Isolation mode (e.g. 'worktree')"
                }
            },
            "required": ["prompt"]
        })
    }

    fn is_read_only(&self, _arguments: &serde_json::Value) -> bool {
        false
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        context: &ToolExecutionContext,
    ) -> ToolResult {
        let prompt = match arguments.get("prompt").and_then(|v| v.as_str()) {
            Some(p) => p,
            None => return ToolResult::error("Missing required parameter: prompt"),
        };

        let subagent_type = arguments
            .get("subagent_type")
            .and_then(|v| v.as_str())
            .unwrap_or("general-purpose");

        let model = arguments
            .get("model")
            .and_then(|v| v.as_str())
            .map(String::from);

        let run_in_background = arguments
            .get("run_in_background")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let isolation = match arguments.get("isolation").and_then(|v| v.as_str()) {
            Some("worktree") => SubagentIsolation::Worktree,
            Some("subprocess") => SubagentIsolation::Subprocess,
            _ => SubagentIsolation::InProcess,
        };

        // Reach the orchestrator via the injected handle.
        let spawner = match context.subagents.as_ref() {
            Some(s) => s,
            None => {
                return ToolResult::error(
                    "Subagent spawning is not available in this context \
                     (no SubagentSpawner injected).",
                )
            }
        };

        let agent_id = AgentId::new(format!("agent-{}", Uuid::new_v4()));
        let req = SpawnRequest {
            agent_id: agent_id.clone(),
            subagent_type: subagent_type.to_string(),
            prompt: prompt.to_string(),
            model,
            run_in_background,
            isolation,
        };

        match spawner.spawn(req).await {
            Ok(res) => ToolResult::success(
                serde_json::json!({
                    "agent_id": res.agent_id.as_str(),
                    "task_id": res.task_id,
                })
                .to_string(),
            ),
            Err(e) => ToolResult::error(format!("Failed to spawn subagent: {e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::traits::Tool;
    use oh_types::subagent::{SpawnResult, SubagentError, SubagentSpawner};
    use oh_types::tools::ToolExecutionContext;
    use std::sync::Arc;

    fn ctx() -> ToolExecutionContext {
        ToolExecutionContext::new(std::env::current_dir().unwrap())
    }

    /// Stub spawner that echoes the request back as a deterministic task id.
    struct StubSpawner;
    #[async_trait]
    impl SubagentSpawner for StubSpawner {
        async fn spawn(&self, req: SpawnRequest) -> Result<SpawnResult, SubagentError> {
            Ok(SpawnResult {
                agent_id: req.agent_id,
                task_id: format!("task-for-{}", req.subagent_type),
            })
        }
    }

    fn ctx_with_spawner() -> ToolExecutionContext {
        let mut c = ctx();
        c.subagents = Some(Arc::new(StubSpawner));
        c
    }

    #[test]
    fn test_schema_has_required_prompt() {
        let tool = AgentTool;
        let schema = tool.input_schema();
        let required = schema["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v == "prompt"));
    }

    #[test]
    fn test_schema_has_optional_fields() {
        let tool = AgentTool;
        let schema = tool.input_schema();
        let props = schema["properties"].as_object().unwrap();
        assert!(props.contains_key("description"));
        assert!(props.contains_key("model"));
        assert!(props.contains_key("subagent_type"));
        assert!(props.contains_key("run_in_background"));
        assert!(props.contains_key("isolation"));
    }

    #[test]
    fn test_is_read_only_returns_false() {
        let tool = AgentTool;
        assert!(!tool.is_read_only(&serde_json::json!({})));
    }

    #[tokio::test]
    async fn test_execute_missing_prompt() {
        let tool = AgentTool;
        let result = tool
            .execute(serde_json::json!({}), &ctx_with_spawner())
            .await;
        assert!(result.is_error);
        assert!(result.output.contains("prompt"));
    }

    #[tokio::test]
    async fn test_execute_without_spawner_errors() {
        let tool = AgentTool;
        let result = tool
            .execute(serde_json::json!({"prompt": "do something"}), &ctx())
            .await;
        assert!(result.is_error);
        assert!(result.output.contains("not available"));
    }

    #[tokio::test]
    async fn test_execute_with_prompt_returns_handle_json() {
        let tool = AgentTool;
        let result = tool
            .execute(
                serde_json::json!({"prompt": "do something", "description": "test task"}),
                &ctx_with_spawner(),
            )
            .await;
        assert!(!result.is_error, "output: {}", result.output);
        let v: serde_json::Value = serde_json::from_str(&result.output).unwrap();
        assert!(v["agent_id"].as_str().unwrap().starts_with("agent-"));
        assert_eq!(v["task_id"], "task-for-general-purpose");
    }

    #[tokio::test]
    async fn test_execute_defaults_subagent_type() {
        let tool = AgentTool;
        let result = tool
            .execute(serde_json::json!({"prompt": "hello"}), &ctx_with_spawner())
            .await;
        assert!(!result.is_error);
        let v: serde_json::Value = serde_json::from_str(&result.output).unwrap();
        assert_eq!(v["task_id"], "task-for-general-purpose");
    }
}

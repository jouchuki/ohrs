//! Spawn subagents for complex tasks tool.

use async_trait::async_trait;
use oh_types::tools::{ToolExecutionContext, ToolResult};

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
        _context: &ToolExecutionContext,
    ) -> ToolResult {
        let prompt = match arguments.get("prompt").and_then(|v| v.as_str()) {
            Some(p) => p,
            None => return ToolResult::error("Missing required parameter: prompt"),
        };

        let description = arguments
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("agent task");

        let subagent_type = arguments
            .get("subagent_type")
            .and_then(|v| v.as_str())
            .unwrap_or("general-purpose");

        let model = arguments
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or("default");

        let run_in_background = arguments
            .get("run_in_background")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let isolation = arguments
            .get("isolation")
            .and_then(|v| v.as_str())
            .unwrap_or("none");

        ToolResult::success(format!(
            "Spawned agent task: {description}\n\
             Type: {subagent_type}\n\
             Model: {model}\n\
             Background: {run_in_background}\n\
             Isolation: {isolation}\n\
             Prompt: {prompt}"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::traits::Tool;
    use oh_types::tools::ToolExecutionContext;

    fn ctx() -> ToolExecutionContext {
        ToolExecutionContext::new(std::env::current_dir().unwrap())
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
        let result = tool.execute(serde_json::json!({}), &ctx()).await;
        assert!(result.is_error);
        assert!(result.output.contains("prompt"));
    }

    #[tokio::test]
    async fn test_execute_with_prompt() {
        let tool = AgentTool;
        let result = tool
            .execute(
                serde_json::json!({"prompt": "do something", "description": "test task"}),
                &ctx(),
            )
            .await;
        assert!(!result.is_error);
        assert!(result.output.contains("Spawned agent task: test task"));
        assert!(result.output.contains("do something"));
    }

    #[tokio::test]
    async fn test_execute_defaults() {
        let tool = AgentTool;
        let result = tool
            .execute(serde_json::json!({"prompt": "hello"}), &ctx())
            .await;
        assert!(!result.is_error);
        assert!(result.output.contains("general-purpose"));
        assert!(result.output.contains("Model: default"));
    }
}

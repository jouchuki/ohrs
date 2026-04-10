//! Create a background task tool.

use async_trait::async_trait;
use oh_types::tools::{ToolExecutionContext, ToolResult};

pub struct TaskCreateTool;

#[async_trait]
impl crate::traits::Tool for TaskCreateTool {
    fn name(&self) -> &str {
        "TaskCreate"
    }

    fn description(&self) -> &str {
        "Create a background shell or local-agent task"
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "description": {
                    "type": "string",
                    "description": "Short task description"
                },
                "command": {
                    "type": "string",
                    "description": "Shell command for a local_bash task"
                },
                "prompt": {
                    "type": "string",
                    "description": "Prompt for a local_agent task"
                }
            },
            "required": ["description"]
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
        let description = match arguments.get("description").and_then(|v| v.as_str()) {
            Some(d) => d,
            None => return ToolResult::error("Missing required parameter: description"),
        };

        let command = arguments.get("command").and_then(|v| v.as_str());
        let prompt = arguments.get("prompt").and_then(|v| v.as_str());

        if command.is_none() && prompt.is_none() {
            return ToolResult::error(
                "Either 'command' (for shell tasks) or 'prompt' (for agent tasks) must be provided",
            );
        }

        if command.is_some() && prompt.is_some() {
            return ToolResult::error(
                "Provide either 'command' or 'prompt', not both",
            );
        }

        if context.metadata.get("task_manager").is_none() {
            return ToolResult::error("Task manager not available");
        }

        // The actual task creation is wired at the CLI level; this is a placeholder.
        let task_type = if command.is_some() { "local_bash" } else { "local_agent" };
        ToolResult::success(
            serde_json::json!({
                "id": "pending",
                "status": "pending",
                "type": task_type,
                "description": description
            })
            .to_string(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::traits::Tool;
    use std::path::PathBuf;

    fn ctx() -> ToolExecutionContext {
        ToolExecutionContext::new(PathBuf::from("/tmp"))
    }

    #[test]
    fn test_schema_requires_description() {
        let tool = TaskCreateTool;
        let schema = tool.input_schema();
        let required = schema["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v.as_str() == Some("description")));
    }

    #[test]
    fn test_schema_has_command_and_prompt() {
        let tool = TaskCreateTool;
        let schema = tool.input_schema();
        assert!(schema["properties"]["command"].is_object());
        assert!(schema["properties"]["prompt"].is_object());
    }

    #[test]
    fn test_name() {
        assert_eq!(TaskCreateTool.name(), "TaskCreate");
    }

    #[test]
    fn test_is_not_read_only() {
        assert!(!TaskCreateTool.is_read_only(&serde_json::json!({})));
    }

    #[tokio::test]
    async fn test_missing_description() {
        let result = TaskCreateTool
            .execute(serde_json::json!({}), &ctx())
            .await;
        assert!(result.is_error);
        assert!(result.output.contains("description"));
    }

    #[tokio::test]
    async fn test_missing_command_and_prompt() {
        let result = TaskCreateTool
            .execute(
                serde_json::json!({"description": "test task"}),
                &ctx(),
            )
            .await;
        assert!(result.is_error);
        assert!(result.output.contains("command"));
    }

    #[tokio::test]
    async fn test_both_command_and_prompt() {
        let result = TaskCreateTool
            .execute(
                serde_json::json!({"description": "x", "command": "ls", "prompt": "do stuff"}),
                &ctx(),
            )
            .await;
        assert!(result.is_error);
        assert!(result.output.contains("not both"));
    }

    #[tokio::test]
    async fn test_task_manager_not_available() {
        let result = TaskCreateTool
            .execute(
                serde_json::json!({"description": "x", "command": "ls"}),
                &ctx(),
            )
            .await;
        assert!(result.is_error);
        assert!(result.output.contains("Task manager not available"));
    }

    #[tokio::test]
    async fn test_success_with_task_manager() {
        let mut context = ctx();
        context
            .metadata
            .insert("task_manager".to_string(), serde_json::json!(true));
        let result = TaskCreateTool
            .execute(
                serde_json::json!({"description": "run tests", "command": "cargo test"}),
                &context,
            )
            .await;
        assert!(!result.is_error);
        assert!(result.output.contains("local_bash"));
    }
}

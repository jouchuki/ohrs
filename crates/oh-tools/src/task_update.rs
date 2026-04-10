//! Update a task tool.

use async_trait::async_trait;
use oh_types::tools::{ToolExecutionContext, ToolResult};

pub struct TaskUpdateTool;

#[async_trait]
impl crate::traits::Tool for TaskUpdateTool {
    fn name(&self) -> &str {
        "TaskUpdate"
    }

    fn description(&self) -> &str {
        "Update a task description"
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "id": {
                    "type": "string",
                    "description": "Task identifier"
                },
                "description": {
                    "type": "string",
                    "description": "Updated task description"
                }
            },
            "required": ["id"]
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
        let id = match arguments.get("id").and_then(|v| v.as_str()) {
            Some(id) => id,
            None => return ToolResult::error("Missing required parameter: id"),
        };

        if context.metadata.get("task_manager").is_none() {
            return ToolResult::error("Task manager not available");
        }

        let description = arguments
            .get("description")
            .and_then(|v| v.as_str());

        // Actual update is wired at the CLI level.
        let mut parts = vec![format!("Updated task {id}")];
        if let Some(desc) = description {
            parts.push(format!("description={desc}"));
        }
        ToolResult::success(parts.join(" "))
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
    fn test_schema_requires_id() {
        let tool = TaskUpdateTool;
        let schema = tool.input_schema();
        let required = schema["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v.as_str() == Some("id")));
    }

    #[test]
    fn test_schema_has_description() {
        let tool = TaskUpdateTool;
        let schema = tool.input_schema();
        assert!(schema["properties"]["description"].is_object());
    }

    #[test]
    fn test_name() {
        assert_eq!(TaskUpdateTool.name(), "TaskUpdate");
    }

    #[test]
    fn test_is_not_read_only() {
        assert!(!TaskUpdateTool.is_read_only(&serde_json::json!({})));
    }

    #[tokio::test]
    async fn test_missing_id() {
        let result = TaskUpdateTool
            .execute(serde_json::json!({}), &ctx())
            .await;
        assert!(result.is_error);
        assert!(result.output.contains("id"));
    }

    #[tokio::test]
    async fn test_task_manager_not_available() {
        let result = TaskUpdateTool
            .execute(serde_json::json!({"id": "abc"}), &ctx())
            .await;
        assert!(result.is_error);
        assert!(result.output.contains("Task manager not available"));
    }

    #[tokio::test]
    async fn test_success_with_description() {
        let mut context = ctx();
        context
            .metadata
            .insert("task_manager".to_string(), serde_json::json!(true));
        let result = TaskUpdateTool
            .execute(
                serde_json::json!({"id": "task-1", "description": "new desc"}),
                &context,
            )
            .await;
        assert!(!result.is_error);
        assert!(result.output.contains("Updated task task-1"));
        assert!(result.output.contains("description=new desc"));
    }

    #[tokio::test]
    async fn test_success_without_description() {
        let mut context = ctx();
        context
            .metadata
            .insert("task_manager".to_string(), serde_json::json!(true));
        let result = TaskUpdateTool
            .execute(serde_json::json!({"id": "task-1"}), &context)
            .await;
        assert!(!result.is_error);
        assert!(result.output.contains("Updated task task-1"));
        assert!(!result.output.contains("description="));
    }
}

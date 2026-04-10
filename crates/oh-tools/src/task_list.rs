//! List background tasks tool.

use async_trait::async_trait;
use oh_types::tools::{ToolExecutionContext, ToolResult};

pub struct TaskListTool;

#[async_trait]
impl crate::traits::Tool for TaskListTool {
    fn name(&self) -> &str {
        "TaskList"
    }

    fn description(&self) -> &str {
        "List background tasks"
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "status": {
                    "type": "string",
                    "description": "Optional status filter (pending, running, completed, failed, killed)"
                }
            }
        })
    }

    fn is_read_only(&self, _arguments: &serde_json::Value) -> bool {
        true
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        context: &ToolExecutionContext,
    ) -> ToolResult {
        // Validate status if provided
        if let Some(status) = arguments.get("status").and_then(|v| v.as_str()) {
            let valid = ["pending", "running", "completed", "failed", "killed"];
            if !valid.contains(&status) {
                return ToolResult::error(format!(
                    "Invalid status filter: {status}. Valid values: {}",
                    valid.join(", ")
                ));
            }
        }

        if context.metadata.get("task_manager").is_none() {
            return ToolResult::error("Task manager not available");
        }

        // Actual listing is wired at the CLI level.
        ToolResult::success("(no tasks)")
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
    fn test_schema_has_status_property() {
        let tool = TaskListTool;
        let schema = tool.input_schema();
        assert!(schema["properties"]["status"].is_object());
    }

    #[test]
    fn test_schema_no_required_fields() {
        let tool = TaskListTool;
        let schema = tool.input_schema();
        assert!(schema.get("required").is_none());
    }

    #[test]
    fn test_name() {
        assert_eq!(TaskListTool.name(), "TaskList");
    }

    #[test]
    fn test_is_read_only() {
        assert!(TaskListTool.is_read_only(&serde_json::json!({})));
    }

    #[tokio::test]
    async fn test_invalid_status() {
        let result = TaskListTool
            .execute(serde_json::json!({"status": "bogus"}), &ctx())
            .await;
        assert!(result.is_error);
        assert!(result.output.contains("Invalid status filter"));
    }

    #[tokio::test]
    async fn test_task_manager_not_available() {
        let result = TaskListTool
            .execute(serde_json::json!({}), &ctx())
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
        let result = TaskListTool
            .execute(serde_json::json!({}), &context)
            .await;
        assert!(!result.is_error);
    }

    #[tokio::test]
    async fn test_valid_status_filter() {
        let mut context = ctx();
        context
            .metadata
            .insert("task_manager".to_string(), serde_json::json!(true));
        let result = TaskListTool
            .execute(serde_json::json!({"status": "running"}), &context)
            .await;
        assert!(!result.is_error);
    }
}

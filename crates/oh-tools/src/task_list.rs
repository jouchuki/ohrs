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
        // Validate status if provided, mapping to TaskStatus.
        let status_filter = match arguments.get("status").and_then(|v| v.as_str()) {
            Some("pending") => Some(oh_types::tasks::TaskStatus::Pending),
            Some("running") => Some(oh_types::tasks::TaskStatus::Running),
            Some("completed") => Some(oh_types::tasks::TaskStatus::Completed),
            Some("failed") => Some(oh_types::tasks::TaskStatus::Failed),
            Some("killed") => Some(oh_types::tasks::TaskStatus::Killed),
            Some(other) => {
                return ToolResult::error(format!(
                    "Invalid status filter: {other}. Valid values: \
                     pending, running, completed, failed, killed"
                ));
            }
            None => None,
        };

        let tasks = match context.tasks.as_ref() {
            Some(t) => t,
            None => return ToolResult::error("Task manager not available"),
        };

        let records = tasks.list(status_filter).await;
        let items: Vec<serde_json::Value> = records
            .iter()
            .map(|r| {
                serde_json::json!({
                    "id": r.id,
                    "type": r.task_type,
                    "status": format!("{:?}", r.status).to_lowercase(),
                    "description": r.description,
                })
            })
            .collect();
        ToolResult::success(serde_json::json!({ "tasks": items }).to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::traits::Tool;
    use oh_services::tasks::BackgroundTaskManager;
    use std::path::PathBuf;
    use std::sync::Arc;

    fn ctx() -> ToolExecutionContext {
        ToolExecutionContext::new(PathBuf::from("/tmp"))
    }

    fn ctx_with_tasks() -> ToolExecutionContext {
        let mut c = ctx();
        c.tasks = Some(Arc::new(BackgroundTaskManager::new()));
        c
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
        let result = TaskListTool.execute(serde_json::json!({}), &ctx()).await;
        assert!(result.is_error);
        assert!(result.output.contains("Task manager not available"));
    }

    #[tokio::test]
    async fn test_success_with_task_manager() {
        let result = TaskListTool
            .execute(serde_json::json!({}), &ctx_with_tasks())
            .await;
        assert!(!result.is_error);
        let v: serde_json::Value = serde_json::from_str(&result.output).unwrap();
        assert!(v["tasks"].is_array());
    }

    #[tokio::test]
    async fn test_valid_status_filter() {
        let result = TaskListTool
            .execute(serde_json::json!({"status": "running"}), &ctx_with_tasks())
            .await;
        assert!(!result.is_error);
    }
}

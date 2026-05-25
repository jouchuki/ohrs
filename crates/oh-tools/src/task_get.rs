//! Get task details tool.

use async_trait::async_trait;
use oh_types::tools::{ToolExecutionContext, ToolResult};

pub struct TaskGetTool;

#[async_trait]
impl crate::traits::Tool for TaskGetTool {
    fn name(&self) -> &str {
        "TaskGet"
    }

    fn description(&self) -> &str {
        "Get details for a background task"
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "id": {
                    "type": "string",
                    "description": "Task identifier"
                }
            },
            "required": ["id"]
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
        let id = match arguments.get("id").and_then(|v| v.as_str()) {
            Some(id) => id,
            None => return ToolResult::error("Missing required parameter: id"),
        };

        let tasks = match context.tasks.as_ref() {
            Some(t) => t,
            None => return ToolResult::error("Task manager not available"),
        };

        match tasks.get(id).await {
            Some(record) => ToolResult::success(
                serde_json::json!({
                    "id": record.id,
                    "type": record.task_type,
                    "status": format!("{:?}", record.status).to_lowercase(),
                    "description": record.description,
                    "return_code": record.return_code,
                })
                .to_string(),
            ),
            None => ToolResult::error(format!("Task not found: {id}")),
        }
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

    #[test]
    fn test_schema_requires_id() {
        let tool = TaskGetTool;
        let schema = tool.input_schema();
        let required = schema["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v.as_str() == Some("id")));
    }

    #[test]
    fn test_name() {
        assert_eq!(TaskGetTool.name(), "TaskGet");
    }

    #[test]
    fn test_is_read_only() {
        assert!(TaskGetTool.is_read_only(&serde_json::json!({})));
    }

    #[tokio::test]
    async fn test_missing_id() {
        let result = TaskGetTool.execute(serde_json::json!({}), &ctx()).await;
        assert!(result.is_error);
        assert!(result.output.contains("id"));
    }

    #[tokio::test]
    async fn test_task_manager_not_available() {
        let result = TaskGetTool
            .execute(serde_json::json!({"id": "abc"}), &ctx())
            .await;
        assert!(result.is_error);
        assert!(result.output.contains("Task manager not available"));
    }

    #[tokio::test]
    async fn test_success_with_task_manager() {
        let mgr = Arc::new(BackgroundTaskManager::new());
        let record = mgr.create_shell_task("echo hi", "t", "/tmp").await;
        let mut context = ctx();
        context.tasks = Some(mgr);
        let result = TaskGetTool
            .execute(serde_json::json!({"id": record.id}), &context)
            .await;
        assert!(!result.is_error, "output: {}", result.output);
        assert!(result.output.contains(&record.id));
    }

    #[tokio::test]
    async fn test_unknown_task_id_errors() {
        let mut context = ctx();
        context.tasks = Some(Arc::new(BackgroundTaskManager::new()));
        let result = TaskGetTool
            .execute(serde_json::json!({"id": "nope"}), &context)
            .await;
        assert!(result.is_error);
        assert!(result.output.contains("not found"));
    }
}

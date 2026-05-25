//! Stop a background task tool.

use async_trait::async_trait;
use oh_types::tools::{ToolExecutionContext, ToolResult};

pub struct TaskStopTool;

#[async_trait]
impl crate::traits::Tool for TaskStopTool {
    fn name(&self) -> &str {
        "TaskStop"
    }

    fn description(&self) -> &str {
        "Stop a background task"
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

        let tasks = match context.tasks.as_ref() {
            Some(t) => t,
            None => return ToolResult::error("Task manager not available"),
        };

        match tasks.stop(id).await {
            Some(record) => ToolResult::success(
                serde_json::json!({
                    "id": record.id,
                    "status": format!("{:?}", record.status).to_lowercase(),
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
        let tool = TaskStopTool;
        let schema = tool.input_schema();
        let required = schema["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v.as_str() == Some("id")));
    }

    #[test]
    fn test_name() {
        assert_eq!(TaskStopTool.name(), "TaskStop");
    }

    #[test]
    fn test_is_not_read_only() {
        assert!(!TaskStopTool.is_read_only(&serde_json::json!({})));
    }

    #[tokio::test]
    async fn test_missing_id() {
        let result = TaskStopTool
            .execute(serde_json::json!({}), &ctx())
            .await;
        assert!(result.is_error);
        assert!(result.output.contains("id"));
    }

    #[tokio::test]
    async fn test_task_manager_not_available() {
        let result = TaskStopTool
            .execute(serde_json::json!({"id": "abc"}), &ctx())
            .await;
        assert!(result.is_error);
        assert!(result.output.contains("Task manager not available"));
    }

    #[tokio::test]
    async fn test_success() {
        let mgr = Arc::new(BackgroundTaskManager::new());
        let record = mgr.create_shell_task("sleep 30", "long", "/tmp").await;
        let mut context = ctx();
        context.tasks = Some(mgr);
        let result = TaskStopTool
            .execute(serde_json::json!({"id": record.id}), &context)
            .await;
        assert!(!result.is_error, "output: {}", result.output);
        let v: serde_json::Value = serde_json::from_str(&result.output).unwrap();
        assert_eq!(v["status"], "killed");
    }

    #[tokio::test]
    async fn test_unknown_task_errors() {
        let mut context = ctx();
        context.tasks = Some(Arc::new(BackgroundTaskManager::new()));
        let result = TaskStopTool
            .execute(serde_json::json!({"id": "nope"}), &context)
            .await;
        assert!(result.is_error);
        assert!(result.output.contains("not found"));
    }
}

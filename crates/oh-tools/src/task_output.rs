//! Read task output tool.

use async_trait::async_trait;
use oh_types::tools::{ToolExecutionContext, ToolResult};

pub struct TaskOutputTool;

#[async_trait]
impl crate::traits::Tool for TaskOutputTool {
    fn name(&self) -> &str {
        "TaskOutput"
    }

    fn description(&self) -> &str {
        "Read the output log for a background task"
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "id": {
                    "type": "string",
                    "description": "Task identifier"
                },
                "max_bytes": {
                    "type": "integer",
                    "description": "Maximum bytes to read (default 12000)",
                    "default": 12000
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

        let max_bytes = arguments
            .get("max_bytes")
            .and_then(|v| v.as_u64())
            .unwrap_or(12000) as usize;

        if max_bytes == 0 {
            return ToolResult::error("max_bytes must be greater than 0");
        }

        let tasks = match context.tasks.as_ref() {
            Some(t) => t,
            None => return ToolResult::error("Task manager not available"),
        };

        match tasks.read_output(id, max_bytes).await {
            Ok(output) => ToolResult::success(output),
            Err(e) => ToolResult::error(format!("Failed to read task output: {e}")),
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

    async fn ctx_with_completed_task() -> (ToolExecutionContext, String) {
        let mgr = Arc::new(BackgroundTaskManager::new());
        let record = mgr.create_shell_task("echo task-output-xyz", "t", "/tmp").await;
        // Poll for completion so the log is populated.
        for _ in 0..50 {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            let r = mgr.get_task(&record.id).await.unwrap();
            if r.status != oh_types::tasks::TaskStatus::Running {
                break;
            }
        }
        let mut c = ctx();
        c.tasks = Some(mgr);
        (c, record.id)
    }

    #[test]
    fn test_schema_requires_id() {
        let tool = TaskOutputTool;
        let schema = tool.input_schema();
        let required = schema["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v.as_str() == Some("id")));
    }

    #[test]
    fn test_schema_has_max_bytes() {
        let tool = TaskOutputTool;
        let schema = tool.input_schema();
        assert!(schema["properties"]["max_bytes"].is_object());
        assert_eq!(schema["properties"]["max_bytes"]["default"], 12000);
    }

    #[test]
    fn test_name() {
        assert_eq!(TaskOutputTool.name(), "TaskOutput");
    }

    #[test]
    fn test_is_read_only() {
        assert!(TaskOutputTool.is_read_only(&serde_json::json!({})));
    }

    #[tokio::test]
    async fn test_missing_id() {
        let result = TaskOutputTool
            .execute(serde_json::json!({}), &ctx())
            .await;
        assert!(result.is_error);
        assert!(result.output.contains("id"));
    }

    #[tokio::test]
    async fn test_task_manager_not_available() {
        let result = TaskOutputTool
            .execute(serde_json::json!({"id": "abc"}), &ctx())
            .await;
        assert!(result.is_error);
        assert!(result.output.contains("Task manager not available"));
    }

    #[tokio::test]
    async fn test_success_with_default_max_bytes() {
        let (context, id) = ctx_with_completed_task().await;
        let result = TaskOutputTool
            .execute(serde_json::json!({"id": id}), &context)
            .await;
        assert!(!result.is_error, "output: {}", result.output);
        assert!(result.output.contains("task-output-xyz"));
    }

    #[tokio::test]
    async fn test_success_with_custom_max_bytes() {
        let (context, id) = ctx_with_completed_task().await;
        let result = TaskOutputTool
            .execute(
                serde_json::json!({"id": id, "max_bytes": 5000}),
                &context,
            )
            .await;
        assert!(!result.is_error);
    }
}

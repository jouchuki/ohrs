//! Manage task lists tool (in-memory).

use async_trait::async_trait;
use oh_types::tools::{ToolExecutionContext, ToolResult};

pub struct TodoWriteTool;

#[async_trait]
impl crate::traits::Tool for TodoWriteTool {
    fn name(&self) -> &str {
        "TodoWrite"
    }

    fn description(&self) -> &str {
        "Manage in-memory task lists."
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "todos": {
                    "type": "array",
                    "description": "Array of todo objects",
                    "items": {
                        "type": "object",
                        "properties": {
                            "content": {"type": "string"},
                            "status": {"type": "string"},
                            "activeForm": {"type": "string"}
                        }
                    }
                }
            },
            "required": ["todos"]
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
        let todos = match arguments.get("todos") {
            Some(t) if t.is_array() => t.clone(),
            Some(_) => return ToolResult::error("Parameter 'todos' must be a JSON array"),
            None => return ToolResult::error("Missing required parameter: todos"),
        };

        let mut result = ToolResult::success("Todos have been modified successfully.");
        result
            .metadata
            .insert("todos".to_string(), todos);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::traits::Tool;
    use oh_types::tools::ToolExecutionContext;
    use std::path::PathBuf;

    #[tokio::test]
    async fn test_todo_write_success() {
        let tool = TodoWriteTool;
        let ctx = ToolExecutionContext::new(PathBuf::from("/tmp"));
        let result = tool
            .execute(
                serde_json::json!({
                    "todos": [
                        {"content": "Fix bug", "status": "pending", "activeForm": "task"},
                        {"content": "Write tests", "status": "done", "activeForm": "task"}
                    ]
                }),
                &ctx,
            )
            .await;
        assert!(!result.is_error);
        assert_eq!(result.output, "Todos have been modified successfully.");
        assert!(result.metadata.contains_key("todos"));
    }

    #[tokio::test]
    async fn test_todo_write_missing_todos() {
        let tool = TodoWriteTool;
        let ctx = ToolExecutionContext::new(PathBuf::from("/tmp"));
        let result = tool.execute(serde_json::json!({}), &ctx).await;
        assert!(result.is_error);
    }

    #[tokio::test]
    async fn test_todo_write_invalid_todos_type() {
        let tool = TodoWriteTool;
        let ctx = ToolExecutionContext::new(PathBuf::from("/tmp"));
        let result = tool
            .execute(serde_json::json!({"todos": "not an array"}), &ctx)
            .await;
        assert!(result.is_error);
    }

    #[test]
    fn test_todo_write_is_not_read_only() {
        let tool = TodoWriteTool;
        assert!(!tool.is_read_only(&serde_json::json!({})));
    }
}

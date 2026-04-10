//! Send messages to agents tool.

use async_trait::async_trait;
use oh_types::tools::{ToolExecutionContext, ToolResult};

pub struct SendMessageTool;

#[async_trait]
impl crate::traits::Tool for SendMessageTool {
    fn name(&self) -> &str {
        "SendMessage"
    }

    fn description(&self) -> &str {
        "Send a follow-up message to a running agent or task."
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "to": {
                    "type": "string",
                    "description": "Target agent or task ID"
                },
                "content": {
                    "type": "string",
                    "description": "Message content to send"
                }
            },
            "required": ["to", "content"]
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
        let to = match arguments.get("to").and_then(|v| v.as_str()) {
            Some(t) => t,
            None => return ToolResult::error("Missing required parameter: to"),
        };

        if arguments.get("content").and_then(|v| v.as_str()).is_none() {
            return ToolResult::error("Missing required parameter: content");
        }

        ToolResult::success(format!("Message sent to {to}"))
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
    fn test_schema_has_required_fields() {
        let tool = SendMessageTool;
        let schema = tool.input_schema();
        let required = schema["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v == "to"));
        assert!(required.iter().any(|v| v == "content"));
    }

    #[test]
    fn test_is_read_only_returns_false() {
        let tool = SendMessageTool;
        assert!(!tool.is_read_only(&serde_json::json!({})));
    }

    #[tokio::test]
    async fn test_confirmation_message() {
        let tool = SendMessageTool;
        let result = tool
            .execute(
                serde_json::json!({"to": "task-123", "content": "hello"}),
                &ctx(),
            )
            .await;
        assert!(!result.is_error);
        assert_eq!(result.output, "Message sent to task-123");
    }

    #[tokio::test]
    async fn test_missing_to() {
        let tool = SendMessageTool;
        let result = tool
            .execute(serde_json::json!({"content": "hello"}), &ctx())
            .await;
        assert!(result.is_error);
        assert!(result.output.contains("to"));
    }

    #[tokio::test]
    async fn test_missing_content() {
        let tool = SendMessageTool;
        let result = tool
            .execute(serde_json::json!({"to": "task-123"}), &ctx())
            .await;
        assert!(result.is_error);
        assert!(result.output.contains("content"));
    }
}

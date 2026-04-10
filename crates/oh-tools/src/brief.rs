//! Truncate content tool.

use async_trait::async_trait;
use oh_types::tools::{ToolExecutionContext, ToolResult};

pub struct BriefTool;

#[async_trait]
impl crate::traits::Tool for BriefTool {
    fn name(&self) -> &str {
        "Brief"
    }

    fn description(&self) -> &str {
        "Truncate content for compact display."
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "content": {
                    "type": "string",
                    "description": "Content to truncate"
                },
                "max_chars": {
                    "type": "integer",
                    "description": "Maximum characters to keep",
                    "default": 2000
                }
            },
            "required": ["content"]
        })
    }

    fn is_read_only(&self, _arguments: &serde_json::Value) -> bool {
        true
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        _context: &ToolExecutionContext,
    ) -> ToolResult {
        let content = match arguments.get("content").and_then(|v| v.as_str()) {
            Some(c) => c,
            None => return ToolResult::error("Missing required parameter: content"),
        };

        let max_chars = arguments
            .get("max_chars")
            .and_then(|v| v.as_u64())
            .unwrap_or(2000) as usize;

        if content.len() <= max_chars {
            ToolResult::success(content)
        } else {
            // Find a char boundary at or before max_chars to avoid panicking on multi-byte UTF-8
            let end = content
                .char_indices()
                .map(|(i, _)| i)
                .take_while(|&i| i <= max_chars)
                .last()
                .unwrap_or(0);
            let truncated = &content[..end];
            ToolResult::success(format!("{}...", truncated.trim_end()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::traits::Tool;
    use oh_types::tools::ToolExecutionContext;
    use std::path::PathBuf;

    #[tokio::test]
    async fn test_brief_no_truncation() {
        let tool = BriefTool;
        let ctx = ToolExecutionContext::new(PathBuf::from("/tmp"));
        let result = tool
            .execute(serde_json::json!({"content": "short text"}), &ctx)
            .await;
        assert!(!result.is_error);
        assert_eq!(result.output, "short text");
    }

    #[tokio::test]
    async fn test_brief_truncates_long_content() {
        let tool = BriefTool;
        let ctx = ToolExecutionContext::new(PathBuf::from("/tmp"));
        let long_content = "a".repeat(100);
        let result = tool
            .execute(
                serde_json::json!({"content": long_content, "max_chars": 10}),
                &ctx,
            )
            .await;
        assert!(!result.is_error);
        assert_eq!(result.output, "aaaaaaaaaa...");
    }

    #[tokio::test]
    async fn test_brief_missing_content() {
        let tool = BriefTool;
        let ctx = ToolExecutionContext::new(PathBuf::from("/tmp"));
        let result = tool.execute(serde_json::json!({}), &ctx).await;
        assert!(result.is_error);
    }

    #[test]
    fn test_brief_is_read_only() {
        let tool = BriefTool;
        assert!(tool.is_read_only(&serde_json::json!({})));
    }
}

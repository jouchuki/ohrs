//! Authenticate with MCP servers tool.

use async_trait::async_trait;
use oh_types::tools::{ToolExecutionContext, ToolResult};
use tracing::instrument;

pub struct McpAuthTool;

#[async_trait]
impl crate::traits::Tool for McpAuthTool {
    fn name(&self) -> &str {
        "McpAuth"
    }

    fn description(&self) -> &str {
        "Configure authentication for an MCP server"
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "server_name": {
                    "type": "string",
                    "description": "Name of the MCP server to authenticate with"
                }
            },
            "required": ["server_name"]
        })
    }

    fn is_read_only(&self, _arguments: &serde_json::Value) -> bool {
        false
    }

    #[instrument(skip(self, _context), fields(tool = "McpAuth"))]
    async fn execute(
        &self,
        arguments: serde_json::Value,
        _context: &ToolExecutionContext,
    ) -> ToolResult {
        let server_name = match arguments.get("server_name").and_then(|v| v.as_str()) {
            Some(s) => s,
            None => return ToolResult::error("Missing required parameter: server_name"),
        };

        ToolResult::success(format!(
            "MCP authentication for {} not yet configured. Configure OAuth or API key in settings.",
            server_name
        ))
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

    #[tokio::test]
    async fn test_returns_placeholder_message() {
        let tool = McpAuthTool;
        let result = tool
            .execute(serde_json::json!({"server_name": "my-server"}), &ctx())
            .await;
        assert!(!result.is_error);
        assert!(result.output.contains("my-server"));
        assert!(result.output.contains("not yet configured"));
    }

    #[tokio::test]
    async fn test_missing_server_name() {
        let tool = McpAuthTool;
        let result = tool.execute(serde_json::json!({}), &ctx()).await;
        assert!(result.is_error);
        assert!(result.output.contains("server_name"));
    }

    #[test]
    fn test_schema_has_required_server_name() {
        let tool = McpAuthTool;
        let schema = tool.input_schema();
        assert_eq!(schema["type"], "object");
        let required = schema["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v == "server_name"));
    }

    #[test]
    fn test_is_read_only_returns_false() {
        let tool = McpAuthTool;
        assert!(!tool.is_read_only(&serde_json::json!({})));
    }
}

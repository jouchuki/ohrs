//! Call MCP server tools tool.

use async_trait::async_trait;
use oh_types::tools::{ToolExecutionContext, ToolResult};
use tracing::instrument;

pub struct McpTool;

#[async_trait]
impl crate::traits::Tool for McpTool {
    fn name(&self) -> &str {
        "McpTool"
    }

    fn description(&self) -> &str {
        "Call a tool on a connected MCP server"
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "server_name": {
                    "type": "string",
                    "description": "Name of the MCP server"
                },
                "tool_name": {
                    "type": "string",
                    "description": "Name of the tool to call on the server"
                },
                "arguments": {
                    "type": "object",
                    "description": "Arguments to pass to the tool"
                }
            },
            "required": ["server_name", "tool_name"]
        })
    }

    fn is_read_only(&self, _arguments: &serde_json::Value) -> bool {
        false
    }

    #[instrument(skip(self, context), fields(tool = "McpTool"))]
    async fn execute(
        &self,
        arguments: serde_json::Value,
        context: &ToolExecutionContext,
    ) -> ToolResult {
        let server_name = match arguments.get("server_name").and_then(|v| v.as_str()) {
            Some(s) => s,
            None => return ToolResult::error("Missing required parameter: server_name"),
        };

        let tool_name = match arguments.get("tool_name").and_then(|v| v.as_str()) {
            Some(s) => s,
            None => return ToolResult::error("Missing required parameter: tool_name"),
        };

        let _tool_args = arguments
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| serde_json::json!({}));

        if context.metadata.get("mcp_manager").is_none() {
            return ToolResult::error("MCP manager not available");
        }

        // TODO: dispatch via McpClientManager::call_tool when engine wiring is complete
        ToolResult::error(format!(
            "MCP call_tool({}, {}) not yet connected — configure and connect the MCP server first",
            server_name, tool_name
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
    async fn test_without_mcp_manager_returns_error() {
        let tool = McpTool;
        let result = tool
            .execute(
                serde_json::json!({"server_name": "test", "tool_name": "ping"}),
                &ctx(),
            )
            .await;
        assert!(result.is_error);
        assert!(result.output.contains("MCP manager not available"));
    }

    #[tokio::test]
    async fn test_missing_server_name() {
        let tool = McpTool;
        let result = tool
            .execute(serde_json::json!({"tool_name": "ping"}), &ctx())
            .await;
        assert!(result.is_error);
        assert!(result.output.contains("server_name"));
    }

    #[tokio::test]
    async fn test_missing_tool_name() {
        let tool = McpTool;
        let result = tool
            .execute(serde_json::json!({"server_name": "test"}), &ctx())
            .await;
        assert!(result.is_error);
        assert!(result.output.contains("tool_name"));
    }

    #[test]
    fn test_schema_has_required_fields() {
        let tool = McpTool;
        let schema = tool.input_schema();
        assert_eq!(schema["type"], "object");
        let required = schema["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v == "server_name"));
        assert!(required.iter().any(|v| v == "tool_name"));
        assert!(schema["properties"]["arguments"].is_object());
    }

    #[test]
    fn test_is_read_only_returns_false() {
        let tool = McpTool;
        assert!(!tool.is_read_only(&serde_json::json!({})));
    }
}

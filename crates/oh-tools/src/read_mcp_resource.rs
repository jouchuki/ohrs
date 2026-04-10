//! Read an MCP resource tool.

use async_trait::async_trait;
use oh_types::tools::{ToolExecutionContext, ToolResult};
use tracing::instrument;

pub struct ReadMcpResourceTool;

#[async_trait]
impl crate::traits::Tool for ReadMcpResourceTool {
    fn name(&self) -> &str {
        "ReadMcpResource"
    }

    fn description(&self) -> &str {
        "Read an MCP resource by server and URI"
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "server_name": {
                    "type": "string",
                    "description": "Name of the MCP server"
                },
                "uri": {
                    "type": "string",
                    "description": "Resource URI to read"
                }
            },
            "required": ["server_name", "uri"]
        })
    }

    fn is_read_only(&self, _arguments: &serde_json::Value) -> bool {
        true
    }

    #[instrument(skip(self, context), fields(tool = "ReadMcpResource"))]
    async fn execute(
        &self,
        arguments: serde_json::Value,
        context: &ToolExecutionContext,
    ) -> ToolResult {
        let server_name = match arguments.get("server_name").and_then(|v| v.as_str()) {
            Some(s) => s,
            None => return ToolResult::error("Missing required parameter: server_name"),
        };

        let uri = match arguments.get("uri").and_then(|v| v.as_str()) {
            Some(s) => s,
            None => return ToolResult::error("Missing required parameter: uri"),
        };

        if context.metadata.get("mcp_manager").is_none() {
            return ToolResult::error("MCP manager not available");
        }

        // TODO: dispatch via McpClientManager::read_resource when engine wiring is complete
        ToolResult::error(format!(
            "MCP read_resource({}, {}) not yet connected — configure and connect the MCP server first",
            server_name, uri
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
        let tool = ReadMcpResourceTool;
        let result = tool
            .execute(
                serde_json::json!({"server_name": "test", "uri": "file:///readme"}),
                &ctx(),
            )
            .await;
        assert!(result.is_error);
        assert!(result.output.contains("MCP manager not available"));
    }

    #[tokio::test]
    async fn test_missing_server_name() {
        let tool = ReadMcpResourceTool;
        let result = tool
            .execute(serde_json::json!({"uri": "file:///readme"}), &ctx())
            .await;
        assert!(result.is_error);
        assert!(result.output.contains("server_name"));
    }

    #[tokio::test]
    async fn test_missing_uri() {
        let tool = ReadMcpResourceTool;
        let result = tool
            .execute(serde_json::json!({"server_name": "test"}), &ctx())
            .await;
        assert!(result.is_error);
        assert!(result.output.contains("uri"));
    }

    #[test]
    fn test_schema_has_required_fields() {
        let tool = ReadMcpResourceTool;
        let schema = tool.input_schema();
        assert_eq!(schema["type"], "object");
        let required = schema["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v == "server_name"));
        assert!(required.iter().any(|v| v == "uri"));
    }

    #[test]
    fn test_is_read_only_returns_true() {
        let tool = ReadMcpResourceTool;
        assert!(tool.is_read_only(&serde_json::json!({})));
    }
}

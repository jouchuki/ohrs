//! List MCP resources tool.

use async_trait::async_trait;
use oh_types::tools::{ToolExecutionContext, ToolResult};
use tracing::instrument;

pub struct ListMcpResourcesTool;

#[async_trait]
impl crate::traits::Tool for ListMcpResourcesTool {
    fn name(&self) -> &str {
        "ListMcpResources"
    }

    fn description(&self) -> &str {
        "List MCP resources available from connected servers"
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "server_name": {
                    "type": "string",
                    "description": "Optional server name to filter resources by"
                }
            }
        })
    }

    fn is_read_only(&self, _arguments: &serde_json::Value) -> bool {
        true
    }

    #[instrument(skip(self, context), fields(tool = "ListMcpResources"))]
    async fn execute(
        &self,
        arguments: serde_json::Value,
        context: &ToolExecutionContext,
    ) -> ToolResult {
        let _server_filter = arguments.get("server_name").and_then(|v| v.as_str());

        if context.metadata.get("mcp_manager").is_none() {
            return ToolResult::success("No MCP servers connected");
        }

        // TODO: call list_resources(), filter by server_name, format as readable list
        ToolResult::success("No MCP servers connected")
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
    async fn test_without_manager_returns_no_servers() {
        let tool = ListMcpResourcesTool;
        let result = tool.execute(serde_json::json!({}), &ctx()).await;
        assert!(!result.is_error);
        assert!(result.output.contains("No MCP servers"));
    }

    #[tokio::test]
    async fn test_with_server_filter() {
        let tool = ListMcpResourcesTool;
        let result = tool
            .execute(serde_json::json!({"server_name": "my-server"}), &ctx())
            .await;
        assert!(!result.is_error);
        assert!(result.output.contains("No MCP servers"));
    }

    #[test]
    fn test_schema_correctness() {
        let tool = ListMcpResourcesTool;
        let schema = tool.input_schema();
        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["server_name"].is_object());
        // server_name is optional — no "required" array
        assert!(schema.get("required").is_none());
    }

    #[test]
    fn test_is_read_only_returns_true() {
        let tool = ListMcpResourcesTool;
        assert!(tool.is_read_only(&serde_json::json!({})));
    }
}

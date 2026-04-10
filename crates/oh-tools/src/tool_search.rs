//! Search available tools tool.

use async_trait::async_trait;
use oh_types::tools::{ToolExecutionContext, ToolResult};

pub struct ToolSearchTool;

#[async_trait]
impl crate::traits::Tool for ToolSearchTool {
    fn name(&self) -> &str {
        "ToolSearch"
    }

    fn description(&self) -> &str {
        "Search the available tool list by name or description."
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Substring to search in tool names and descriptions"
                },
                "max_results": {
                    "type": "integer",
                    "description": "Maximum number of results to return",
                    "default": 5
                }
            },
            "required": ["query"]
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
        let query = match arguments.get("query").and_then(|v| v.as_str()) {
            Some(q) => q.to_lowercase(),
            None => return ToolResult::error("Missing required parameter: query"),
        };

        let max_results = arguments
            .get("max_results")
            .and_then(|v| v.as_u64())
            .unwrap_or(5) as usize;

        // Get tool_registry from context metadata
        let registry_value = match context.metadata.get("tool_registry") {
            Some(v) => v,
            None => return ToolResult::error("Tool registry context not available"),
        };

        // The registry is stored as a JSON array of {name, description} objects
        let tools = match registry_value.as_array() {
            Some(arr) => arr,
            None => return ToolResult::error("Tool registry has invalid format"),
        };

        let matches: Vec<(String, String)> = tools
            .iter()
            .filter_map(|tool| {
                let name = tool.get("name")?.as_str()?;
                let desc = tool.get("description")?.as_str()?;
                if name.to_lowercase().contains(&query)
                    || desc.to_lowercase().contains(&query)
                {
                    Some((name.to_string(), desc.to_string()))
                } else {
                    None
                }
            })
            .take(max_results)
            .collect();

        if matches.is_empty() {
            return ToolResult::success("(no matches)");
        }

        let output = matches
            .iter()
            .enumerate()
            .map(|(i, (name, desc))| format!("{}. {}: {}", i + 1, name, desc))
            .collect::<Vec<_>>()
            .join("\n");

        ToolResult::success(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::traits::Tool;
    use oh_types::tools::ToolExecutionContext;
    use std::path::PathBuf;

    fn ctx_with_registry() -> ToolExecutionContext {
        let mut ctx = ToolExecutionContext::new(PathBuf::from("/tmp"));
        ctx.metadata.insert(
            "tool_registry".to_string(),
            serde_json::json!([
                {"name": "Bash", "description": "Run shell commands"},
                {"name": "FileRead", "description": "Read a file"},
                {"name": "FileWrite", "description": "Write a file"},
                {"name": "Sleep", "description": "Pause execution"}
            ]),
        );
        ctx
    }

    #[tokio::test]
    async fn test_search_finds_matching_tools() {
        let tool = ToolSearchTool;
        let ctx = ctx_with_registry();
        let result = tool.execute(serde_json::json!({"query": "file"}), &ctx).await;
        assert!(!result.is_error);
        assert!(result.output.contains("FileRead"));
        assert!(result.output.contains("FileWrite"));
    }

    #[tokio::test]
    async fn test_search_no_matches() {
        let tool = ToolSearchTool;
        let ctx = ctx_with_registry();
        let result = tool
            .execute(serde_json::json!({"query": "nonexistent"}), &ctx)
            .await;
        assert!(!result.is_error);
        assert_eq!(result.output, "(no matches)");
    }

    #[tokio::test]
    async fn test_search_missing_registry() {
        let tool = ToolSearchTool;
        let ctx = ToolExecutionContext::new(PathBuf::from("/tmp"));
        let result = tool.execute(serde_json::json!({"query": "bash"}), &ctx).await;
        assert!(result.is_error);
    }

    #[tokio::test]
    async fn test_search_max_results() {
        let tool = ToolSearchTool;
        let ctx = ctx_with_registry();
        let result = tool
            .execute(serde_json::json!({"query": "file", "max_results": 1}), &ctx)
            .await;
        assert!(!result.is_error);
        // Should only have one numbered entry
        assert!(result.output.starts_with("1."));
        assert!(!result.output.contains("2."));
    }
}

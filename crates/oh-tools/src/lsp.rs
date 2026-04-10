//! Language Server Protocol operations tool.

use async_trait::async_trait;
use oh_types::tools::{ToolExecutionContext, ToolResult};
use tracing::instrument;

pub struct LspTool;

#[async_trait]
impl crate::traits::Tool for LspTool {
    fn name(&self) -> &str {
        "Lsp"
    }

    fn description(&self) -> &str {
        "Perform Language Server Protocol operations such as hover, definition, references, and diagnostics"
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "description": "LSP action to perform: hover, definition, references, or diagnostics",
                    "enum": ["hover", "definition", "references", "diagnostics"]
                },
                "file": {
                    "type": "string",
                    "description": "Path to the source file"
                },
                "line": {
                    "type": "integer",
                    "description": "1-based line number"
                },
                "character": {
                    "type": "integer",
                    "description": "1-based character offset"
                }
            },
            "required": ["action", "file"]
        })
    }

    fn is_read_only(&self, _arguments: &serde_json::Value) -> bool {
        true
    }

    #[instrument(skip(self, _context), fields(tool = "Lsp"))]
    async fn execute(
        &self,
        arguments: serde_json::Value,
        _context: &ToolExecutionContext,
    ) -> ToolResult {
        let action = match arguments.get("action").and_then(|v| v.as_str()) {
            Some(a) => a,
            None => return ToolResult::error("Missing required parameter: action"),
        };

        let _file = match arguments.get("file").and_then(|v| v.as_str()) {
            Some(f) => f,
            None => return ToolResult::error("Missing required parameter: file"),
        };

        let _line = arguments.get("line").and_then(|v| v.as_i64());
        let _character = arguments.get("character").and_then(|v| v.as_i64());

        ToolResult::success(format!(
            "LSP {} not yet implemented. Configure an LSP server first.",
            action
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
        let tool = LspTool;
        let result = tool
            .execute(
                serde_json::json!({"action": "hover", "file": "main.rs"}),
                &ctx(),
            )
            .await;
        assert!(!result.is_error);
        assert!(result.output.contains("LSP hover not yet implemented"));
        assert!(result.output.contains("Configure an LSP server first"));
    }

    #[tokio::test]
    async fn test_definition_action() {
        let tool = LspTool;
        let result = tool
            .execute(
                serde_json::json!({"action": "definition", "file": "lib.rs", "line": 10, "character": 5}),
                &ctx(),
            )
            .await;
        assert!(!result.is_error);
        assert!(result.output.contains("LSP definition not yet implemented"));
    }

    #[tokio::test]
    async fn test_missing_action() {
        let tool = LspTool;
        let result = tool
            .execute(serde_json::json!({"file": "main.rs"}), &ctx())
            .await;
        assert!(result.is_error);
        assert!(result.output.contains("action"));
    }

    #[tokio::test]
    async fn test_missing_file() {
        let tool = LspTool;
        let result = tool
            .execute(serde_json::json!({"action": "hover"}), &ctx())
            .await;
        assert!(result.is_error);
        assert!(result.output.contains("file"));
    }

    #[test]
    fn test_schema_has_required_fields() {
        let tool = LspTool;
        let schema = tool.input_schema();
        assert_eq!(schema["type"], "object");
        let required = schema["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v == "action"));
        assert!(required.iter().any(|v| v == "file"));
        // Optional fields exist
        assert!(schema["properties"]["line"].is_object());
        assert!(schema["properties"]["character"].is_object());
        // Action has enum constraint
        assert!(schema["properties"]["action"]["enum"].is_array());
    }

    #[test]
    fn test_is_read_only_returns_true() {
        let tool = LspTool;
        assert!(tool.is_read_only(&serde_json::json!({})));
    }
}

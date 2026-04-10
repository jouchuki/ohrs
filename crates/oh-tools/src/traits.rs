//! Core Tool trait — the interface all tools implement.

use async_trait::async_trait;
use oh_types::tools::{ToolExecutionContext, ToolResult, ToolSchema};

/// Base trait for all OpenHarness tools.
#[async_trait]
pub trait Tool: Send + Sync {
    /// Unique tool name.
    fn name(&self) -> &str;

    /// Human-readable description.
    fn description(&self) -> &str;

    /// JSON Schema for tool input parameters.
    fn input_schema(&self) -> serde_json::Value;

    /// Whether this specific invocation is read-only.
    fn is_read_only(&self, _arguments: &serde_json::Value) -> bool {
        false
    }

    /// Execute the tool.
    async fn execute(
        &self,
        arguments: serde_json::Value,
        context: &ToolExecutionContext,
    ) -> ToolResult;

    /// Return the schema in Anthropic API format.
    fn to_api_schema(&self) -> ToolSchema {
        ToolSchema {
            name: self.name().to_string(),
            description: self.description().to_string(),
            input_schema: self.input_schema(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oh_types::tools::{ToolExecutionContext, ToolResult};

    struct DummyTool;

    #[async_trait]
    impl Tool for DummyTool {
        fn name(&self) -> &str {
            "dummy"
        }
        fn description(&self) -> &str {
            "A dummy tool for testing"
        }
        fn input_schema(&self) -> serde_json::Value {
            serde_json::json!({
                "type": "object",
                "properties": {
                    "arg1": { "type": "string" }
                },
                "required": ["arg1"]
            })
        }
        async fn execute(
            &self,
            _arguments: serde_json::Value,
            _context: &ToolExecutionContext,
        ) -> ToolResult {
            ToolResult::success("ok")
        }
    }

    #[test]
    fn test_to_api_schema_has_name() {
        let tool = DummyTool;
        let schema = tool.to_api_schema();
        assert_eq!(schema.name, "dummy");
    }

    #[test]
    fn test_to_api_schema_has_description() {
        let tool = DummyTool;
        let schema = tool.to_api_schema();
        assert_eq!(schema.description, "A dummy tool for testing");
    }

    #[test]
    fn test_to_api_schema_has_input_schema() {
        let tool = DummyTool;
        let schema = tool.to_api_schema();
        assert_eq!(schema.input_schema["type"], "object");
        assert!(schema.input_schema["properties"]["arg1"].is_object());
        assert_eq!(schema.input_schema["required"][0], "arg1");
    }

    #[test]
    fn test_is_read_only_default_false() {
        let tool = DummyTool;
        assert!(!tool.is_read_only(&serde_json::json!({})));
    }
}

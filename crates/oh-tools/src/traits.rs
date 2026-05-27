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

    /// Filesystem paths this tool would touch given these arguments.
    ///
    /// The permission gate canonicalizes each returned path and checks it
    /// against the sensitive-path blocklist and `allowed_roots` BEFORE
    /// execution (TOOL-1 / contract C3). The default reads the conventional
    /// `file_path` key; tools whose path argument differs override this.
    fn path_args(&self, input: &serde_json::Value) -> Vec<String> {
        input
            .get("file_path")
            .and_then(|v| v.as_str())
            .map(|s| vec![s.to_string()])
            .unwrap_or_default()
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

    #[test]
    fn test_path_args_default_reads_file_path() {
        let tool = DummyTool;
        let args = serde_json::json!({ "file_path": "/etc/hosts" });
        assert_eq!(tool.path_args(&args), vec!["/etc/hosts".to_string()]);
    }

    #[test]
    fn test_path_args_default_empty_when_absent() {
        let tool = DummyTool;
        assert!(tool.path_args(&serde_json::json!({})).is_empty());
    }
}

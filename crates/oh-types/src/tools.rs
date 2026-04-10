//! Tool abstractions and registry types.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

/// Shared execution context for tool invocations.
#[derive(Debug, Clone)]
pub struct ToolExecutionContext {
    pub cwd: PathBuf,
    pub metadata: HashMap<String, serde_json::Value>,
}

impl ToolExecutionContext {
    pub fn new(cwd: PathBuf) -> Self {
        Self {
            cwd,
            metadata: HashMap::new(),
        }
    }
}

/// Normalized tool execution result.
#[derive(Debug, Clone)]
pub struct ToolResult {
    pub output: String,
    pub is_error: bool,
    pub metadata: HashMap<String, serde_json::Value>,
}

impl ToolResult {
    pub fn success(output: impl Into<String>) -> Self {
        Self {
            output: output.into(),
            is_error: false,
            metadata: HashMap::new(),
        }
    }

    pub fn error(output: impl Into<String>) -> Self {
        Self {
            output: output.into(),
            is_error: true,
            metadata: HashMap::new(),
        }
    }
}

/// Tool schema in the Anthropic API format.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSchema {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tool_result_success() {
        let result = ToolResult::success("output text");
        assert_eq!(result.output, "output text");
        assert!(!result.is_error);
        assert!(result.metadata.is_empty());
    }

    #[test]
    fn test_tool_result_error() {
        let result = ToolResult::error("something failed");
        assert_eq!(result.output, "something failed");
        assert!(result.is_error);
        assert!(result.metadata.is_empty());
    }

    #[test]
    fn test_tool_execution_context_new() {
        let ctx = ToolExecutionContext::new(PathBuf::from("/tmp"));
        assert_eq!(ctx.cwd, PathBuf::from("/tmp"));
        assert!(ctx.metadata.is_empty());
    }

    #[test]
    fn test_tool_schema_serde_roundtrip() {
        let schema = ToolSchema {
            name: "bash".into(),
            description: "Run a command".into(),
            input_schema: serde_json::json!({"type": "object"}),
        };
        let json = serde_json::to_string(&schema).unwrap();
        let deser: ToolSchema = serde_json::from_str(&json).unwrap();
        assert_eq!(deser.name, "bash");
        assert_eq!(deser.description, "Run a command");
    }
}

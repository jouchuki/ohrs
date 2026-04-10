//! Get or set configuration tool.

use async_trait::async_trait;
use oh_types::tools::{ToolExecutionContext, ToolResult};

pub struct ConfigTool;

#[async_trait]
impl crate::traits::Tool for ConfigTool {
    fn name(&self) -> &str {
        "Config"
    }

    fn description(&self) -> &str {
        "Get or set configuration settings."
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "description": "Action to perform: 'get' or 'set'",
                    "enum": ["get", "set"]
                },
                "key": {
                    "type": "string",
                    "description": "Configuration key (required for 'set')"
                },
                "value": {
                    "type": "string",
                    "description": "Configuration value (required for 'set')"
                }
            },
            "required": ["action"]
        })
    }

    fn is_read_only(&self, arguments: &serde_json::Value) -> bool {
        arguments
            .get("action")
            .and_then(|v| v.as_str())
            .map(|a| a == "get")
            .unwrap_or(true)
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        context: &ToolExecutionContext,
    ) -> ToolResult {
        let action = match arguments.get("action").and_then(|v| v.as_str()) {
            Some(a) => a,
            None => return ToolResult::error("Missing required parameter: action"),
        };

        match action {
            "get" => {
                // Return current settings from metadata
                let settings = context
                    .metadata
                    .get("config")
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!({}));
                match serde_json::to_string_pretty(&settings) {
                    Ok(json) => ToolResult::success(json),
                    Err(e) => ToolResult::error(format!("Failed to serialize config: {e}")),
                }
            }
            "set" => {
                // Check if config is read-only
                if let Some(readonly) = context.metadata.get("config_readonly") {
                    if readonly.as_bool().unwrap_or(false) {
                        return ToolResult::error("Configuration is read-only");
                    }
                }

                let key = match arguments.get("key").and_then(|v| v.as_str()) {
                    Some(k) => k,
                    None => return ToolResult::error("Missing required parameter: key"),
                };

                let value = match arguments.get("value").and_then(|v| v.as_str()) {
                    Some(v) => v,
                    None => return ToolResult::error("Missing required parameter: value"),
                };

                ToolResult::success(format!("Updated config: {key} = {value}"))
            }
            other => ToolResult::error(format!(
                "Unknown action: {other}. Use 'get' or 'set'."
            )),
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
    async fn test_config_get_empty() {
        let tool = ConfigTool;
        let ctx = ToolExecutionContext::new(PathBuf::from("/tmp"));
        let result = tool.execute(serde_json::json!({"action": "get"}), &ctx).await;
        assert!(!result.is_error);
        assert_eq!(result.output, "{}");
    }

    #[tokio::test]
    async fn test_config_get_with_data() {
        let tool = ConfigTool;
        let mut ctx = ToolExecutionContext::new(PathBuf::from("/tmp"));
        ctx.metadata.insert(
            "config".to_string(),
            serde_json::json!({"model": "claude-3", "temperature": 0.7}),
        );
        let result = tool.execute(serde_json::json!({"action": "get"}), &ctx).await;
        assert!(!result.is_error);
        assert!(result.output.contains("model"));
        assert!(result.output.contains("claude-3"));
    }

    #[tokio::test]
    async fn test_config_set_readonly() {
        let tool = ConfigTool;
        let mut ctx = ToolExecutionContext::new(PathBuf::from("/tmp"));
        ctx.metadata
            .insert("config_readonly".to_string(), serde_json::json!(true));
        let result = tool
            .execute(
                serde_json::json!({"action": "set", "key": "model", "value": "gpt-4"}),
                &ctx,
            )
            .await;
        assert!(result.is_error);
        assert!(result.output.contains("read-only"));
    }

    #[tokio::test]
    async fn test_config_set_success() {
        let tool = ConfigTool;
        let ctx = ToolExecutionContext::new(PathBuf::from("/tmp"));
        let result = tool
            .execute(
                serde_json::json!({"action": "set", "key": "model", "value": "claude-3"}),
                &ctx,
            )
            .await;
        assert!(!result.is_error);
        assert!(result.output.contains("model"));
    }

    #[test]
    fn test_config_is_read_only_for_get() {
        let tool = ConfigTool;
        assert!(tool.is_read_only(&serde_json::json!({"action": "get"})));
        assert!(!tool.is_read_only(&serde_json::json!({"action": "set"})));
    }
}

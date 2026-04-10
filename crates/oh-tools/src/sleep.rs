//! Delay execution tool.

use async_trait::async_trait;
use oh_types::tools::{ToolExecutionContext, ToolResult};
use std::time::Duration;

pub struct SleepTool;

#[async_trait]
impl crate::traits::Tool for SleepTool {
    fn name(&self) -> &str {
        "Sleep"
    }

    fn description(&self) -> &str {
        "Sleep for a short duration."
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "seconds": {
                    "type": "number",
                    "description": "Duration to sleep in seconds (max 30)",
                    "default": 1.0
                }
            }
        })
    }

    fn is_read_only(&self, _arguments: &serde_json::Value) -> bool {
        true
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        _context: &ToolExecutionContext,
    ) -> ToolResult {
        let seconds = arguments
            .get("seconds")
            .and_then(|v| v.as_f64())
            .unwrap_or(1.0)
            .min(30.0)
            .max(0.0);

        tokio::time::sleep(Duration::from_secs_f64(seconds)).await;
        ToolResult::success(format!("Slept for {seconds} seconds"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::traits::Tool;
    use oh_types::tools::ToolExecutionContext;
    use std::path::PathBuf;

    #[tokio::test]
    async fn test_sleep_default() {
        let tool = SleepTool;
        let ctx = ToolExecutionContext::new(PathBuf::from("/tmp"));
        let result = tool.execute(serde_json::json!({}), &ctx).await;
        assert!(!result.is_error);
        assert_eq!(result.output, "Slept for 1 seconds");
    }

    #[tokio::test]
    async fn test_sleep_custom_duration() {
        let tool = SleepTool;
        let ctx = ToolExecutionContext::new(PathBuf::from("/tmp"));
        let result = tool.execute(serde_json::json!({"seconds": 0.01}), &ctx).await;
        assert!(!result.is_error);
        assert_eq!(result.output, "Slept for 0.01 seconds");
    }

    #[test]
    fn test_sleep_name() {
        let tool = SleepTool;
        assert_eq!(tool.name(), "Sleep");
    }

    #[test]
    fn test_sleep_is_read_only() {
        let tool = SleepTool;
        assert!(tool.is_read_only(&serde_json::json!({})));
    }
}

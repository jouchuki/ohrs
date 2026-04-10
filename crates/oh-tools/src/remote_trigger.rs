//! Trigger remote agents tool.

use async_trait::async_trait;
use oh_types::tools::{ToolExecutionContext, ToolResult};

pub struct RemoteTriggerTool;

#[async_trait]
impl crate::traits::Tool for RemoteTriggerTool {
    fn name(&self) -> &str {
        "RemoteTrigger"
    }

    fn description(&self) -> &str {
        "Trigger remote agents"
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "trigger_id": {"type": "string", "description": "Remote trigger ID"}
            },
            "required": ["trigger_id"]
        })
    }

    fn is_read_only(&self, _arguments: &serde_json::Value) -> bool {
        false
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        _context: &ToolExecutionContext,
    ) -> ToolResult {
        let trigger_id = match arguments.get("trigger_id").and_then(|v| v.as_str()) {
            Some(id) => id,
            None => return ToolResult::error("Missing required parameter: trigger_id"),
        };

        ToolResult::success(format!("Remote trigger {trigger_id} acknowledged"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::traits::Tool;
    use std::path::PathBuf;

    #[tokio::test]
    async fn test_remote_trigger_acknowledged() {
        let tool = RemoteTriggerTool;
        let ctx = ToolExecutionContext::new(PathBuf::from("/tmp"));
        let result = tool
            .execute(serde_json::json!({"trigger_id": "abc-123"}), &ctx)
            .await;
        assert!(!result.is_error);
        assert_eq!(result.output, "Remote trigger abc-123 acknowledged");
    }

    #[tokio::test]
    async fn test_remote_trigger_missing_id() {
        let tool = RemoteTriggerTool;
        let ctx = ToolExecutionContext::new(PathBuf::from("/tmp"));
        let result = tool.execute(serde_json::json!({}), &ctx).await;
        assert!(result.is_error);
    }

    #[test]
    fn test_remote_trigger_name() {
        let tool = RemoteTriggerTool;
        assert_eq!(tool.name(), "RemoteTrigger");
    }

    #[test]
    fn test_remote_trigger_not_read_only() {
        let tool = RemoteTriggerTool;
        assert!(!tool.is_read_only(&serde_json::json!({})));
    }
}

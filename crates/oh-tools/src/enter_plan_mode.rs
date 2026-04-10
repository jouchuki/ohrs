//! Enter plan mode for design tool.

use async_trait::async_trait;
use oh_types::tools::{ToolExecutionContext, ToolResult};

pub struct EnterPlanModeTool;

#[async_trait]
impl crate::traits::Tool for EnterPlanModeTool {
    fn name(&self) -> &str {
        "EnterPlanMode"
    }

    fn description(&self) -> &str {
        "Enter plan mode for design"
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({"type": "object", "properties": {}})
    }

    fn is_read_only(&self, _arguments: &serde_json::Value) -> bool {
        false
    }

    async fn execute(
        &self,
        _arguments: serde_json::Value,
        _context: &ToolExecutionContext,
    ) -> ToolResult {
        let mut result = ToolResult::success("Entered plan mode.");
        result
            .metadata
            .insert("plan_mode".to_string(), serde_json::Value::Bool(true));
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::traits::Tool;
    use oh_types::tools::ToolExecutionContext;
    use std::path::PathBuf;

    #[tokio::test]
    async fn test_enter_plan_mode_sets_flag() {
        let tool = EnterPlanModeTool;
        let ctx = ToolExecutionContext::new(PathBuf::from("/tmp"));
        let result = tool.execute(serde_json::json!({}), &ctx).await;
        assert!(!result.is_error);
        assert_eq!(result.output, "Entered plan mode.");
        assert_eq!(result.metadata.get("plan_mode"), Some(&serde_json::Value::Bool(true)));
    }
}

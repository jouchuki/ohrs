//! Exit plan mode tool.

use async_trait::async_trait;
use oh_types::tools::{ToolExecutionContext, ToolResult};

pub struct ExitPlanModeTool;

#[async_trait]
impl crate::traits::Tool for ExitPlanModeTool {
    fn name(&self) -> &str {
        "ExitPlanMode"
    }

    fn description(&self) -> &str {
        "Exit plan mode"
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
        let mut result = ToolResult::success("Exited plan mode.");
        result
            .metadata
            .insert("plan_mode".to_string(), serde_json::Value::Bool(false));
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
    async fn test_exit_plan_mode_clears_flag() {
        let tool = ExitPlanModeTool;
        let ctx = ToolExecutionContext::new(PathBuf::from("/tmp"));
        let result = tool.execute(serde_json::json!({}), &ctx).await;
        assert!(!result.is_error);
        assert_eq!(result.output, "Exited plan mode.");
        assert_eq!(result.metadata.get("plan_mode"), Some(&serde_json::Value::Bool(false)));
    }
}

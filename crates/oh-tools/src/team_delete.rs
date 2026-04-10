//! Delete a team tool.

use async_trait::async_trait;
use oh_types::tools::{ToolExecutionContext, ToolResult};

pub struct TeamDeleteTool;

#[async_trait]
impl crate::traits::Tool for TeamDeleteTool {
    fn name(&self) -> &str {
        "TeamDelete"
    }

    fn description(&self) -> &str {
        "Delete a team"
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "name": {"type": "string", "description": "Team name to delete"}
            },
            "required": ["name"]
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
        let name = match arguments.get("name").and_then(|v| v.as_str()) {
            Some(n) => n,
            None => return ToolResult::error("Missing required parameter: name"),
        };

        let registry = oh_services::coordinator::get_team_registry();
        let mut reg = registry.lock().unwrap();
        match reg.delete_team(name) {
            Ok(()) => ToolResult::success(format!("Deleted team: {name}")),
            Err(e) => ToolResult::error(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::traits::Tool;
    use std::path::PathBuf;

    #[tokio::test]
    async fn test_team_delete_success() {
        let ctx = ToolExecutionContext::new(PathBuf::from("/tmp"));
        let name = format!("test_team_del_{}", uuid::Uuid::new_v4());

        // Create first
        let registry = oh_services::coordinator::get_team_registry();
        registry.lock().unwrap().create_team(&name, "").unwrap();

        // Delete
        let tool = TeamDeleteTool;
        let result = tool
            .execute(serde_json::json!({"name": name}), &ctx)
            .await;
        assert!(!result.is_error, "Expected success, got: {}", result.output);
        assert_eq!(result.output, format!("Deleted team: {name}"));
    }

    #[tokio::test]
    async fn test_team_delete_nonexistent() {
        let tool = TeamDeleteTool;
        let ctx = ToolExecutionContext::new(PathBuf::from("/tmp"));
        let result = tool
            .execute(serde_json::json!({"name": "nonexistent_team_xyz"}), &ctx)
            .await;
        assert!(result.is_error);
    }

    #[tokio::test]
    async fn test_team_delete_missing_name() {
        let tool = TeamDeleteTool;
        let ctx = ToolExecutionContext::new(PathBuf::from("/tmp"));
        let result = tool.execute(serde_json::json!({}), &ctx).await;
        assert!(result.is_error);
    }

    #[test]
    fn test_team_delete_name() {
        let tool = TeamDeleteTool;
        assert_eq!(tool.name(), "TeamDelete");
    }
}

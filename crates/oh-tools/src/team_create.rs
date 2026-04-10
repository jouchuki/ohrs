//! Create a team tool.

use async_trait::async_trait;
use oh_types::tools::{ToolExecutionContext, ToolResult};

pub struct TeamCreateTool;

#[async_trait]
impl crate::traits::Tool for TeamCreateTool {
    fn name(&self) -> &str {
        "TeamCreate"
    }

    fn description(&self) -> &str {
        "Create a team"
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "name": {"type": "string", "description": "Team name"},
                "description": {"type": "string", "description": "Optional team description"}
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
        let description = arguments
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let registry = oh_services::coordinator::get_team_registry();
        let mut reg = registry.lock().unwrap();
        match reg.create_team(name, description) {
            Ok(_) => ToolResult::success(format!("Created team: {name}")),
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
    async fn test_team_create_success() {
        let tool = TeamCreateTool;
        let ctx = ToolExecutionContext::new(PathBuf::from("/tmp"));
        let name = format!("test_team_create_{}", uuid::Uuid::new_v4());
        let result = tool
            .execute(serde_json::json!({"name": name}), &ctx)
            .await;
        assert!(!result.is_error, "Expected success, got: {}", result.output);
        assert_eq!(result.output, format!("Created team: {name}"));

        // Clean up
        let registry = oh_services::coordinator::get_team_registry();
        registry.lock().unwrap().delete_team(&name).ok();
    }

    #[tokio::test]
    async fn test_team_create_duplicate_error() {
        let tool = TeamCreateTool;
        let ctx = ToolExecutionContext::new(PathBuf::from("/tmp"));
        let name = format!("test_team_dup_{}", uuid::Uuid::new_v4());

        // Create first
        let r1 = tool
            .execute(serde_json::json!({"name": name}), &ctx)
            .await;
        assert!(!r1.is_error);

        // Duplicate should fail
        let r2 = tool
            .execute(serde_json::json!({"name": name}), &ctx)
            .await;
        assert!(r2.is_error);

        // Clean up
        let registry = oh_services::coordinator::get_team_registry();
        registry.lock().unwrap().delete_team(&name).ok();
    }

    #[tokio::test]
    async fn test_team_create_missing_name() {
        let tool = TeamCreateTool;
        let ctx = ToolExecutionContext::new(PathBuf::from("/tmp"));
        let result = tool.execute(serde_json::json!({}), &ctx).await;
        assert!(result.is_error);
    }

    #[test]
    fn test_team_create_name() {
        let tool = TeamCreateTool;
        assert_eq!(tool.name(), "TeamCreate");
    }

    #[test]
    fn test_team_create_not_read_only() {
        let tool = TeamCreateTool;
        assert!(!tool.is_read_only(&serde_json::json!({})));
    }
}

//! Exit a git worktree tool.

use async_trait::async_trait;
use oh_types::tools::{ToolExecutionContext, ToolResult};

pub struct ExitWorktreeTool;

#[async_trait]
impl crate::traits::Tool for ExitWorktreeTool {
    fn name(&self) -> &str {
        "ExitWorktree"
    }

    fn description(&self) -> &str {
        "Remove a git worktree"
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
        context: &ToolExecutionContext,
    ) -> ToolResult {
        let worktree_path = match context.metadata.get("worktree_path") {
            Some(serde_json::Value::String(p)) => p.clone(),
            _ => return ToolResult::error("No active worktree found in context metadata"),
        };

        let output = tokio::process::Command::new("git")
            .args(["worktree", "remove", "--force", &worktree_path])
            .current_dir(&context.cwd)
            .output()
            .await;

        match output {
            Ok(out) => {
                if out.status.success() {
                    let mut result =
                        ToolResult::success(format!("Removed worktree at {}", worktree_path));
                    result.metadata.insert(
                        "worktree_path".to_string(),
                        serde_json::Value::Null,
                    );
                    result
                } else {
                    let stderr = String::from_utf8_lossy(&out.stderr);
                    ToolResult::error(format!("git worktree remove failed: {}", stderr.trim()))
                }
            }
            Err(e) => ToolResult::error(format!("Failed to run git: {}", e)),
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
    async fn test_exit_worktree_without_metadata_returns_error() {
        let tool = ExitWorktreeTool;
        let ctx = ToolExecutionContext::new(PathBuf::from("/tmp"));
        let result = tool.execute(serde_json::json!({}), &ctx).await;
        assert!(result.is_error);
        assert!(result.output.contains("No active worktree"));
    }
}

//! Enter a git worktree tool.

use async_trait::async_trait;
use oh_types::tools::{ToolExecutionContext, ToolResult};

pub struct EnterWorktreeTool;

#[async_trait]
impl crate::traits::Tool for EnterWorktreeTool {
    fn name(&self) -> &str {
        "EnterWorktree"
    }

    fn description(&self) -> &str {
        "Create a git worktree and return its path"
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "branch": {
                    "type": "string",
                    "description": "Branch name for the worktree. Auto-generated if not provided."
                }
            }
        })
    }

    fn is_read_only(&self, _arguments: &serde_json::Value) -> bool {
        false
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        context: &ToolExecutionContext,
    ) -> ToolResult {
        let branch = arguments
            .get("branch")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| format!("worktree-{}", uuid::Uuid::new_v4().as_simple()));

        let worktree_path = context
            .cwd
            .join(".openharness")
            .join("worktrees")
            .join(&branch);

        let output = tokio::process::Command::new("git")
            .args(["worktree", "add", "-b", &branch])
            .arg(&worktree_path)
            .current_dir(&context.cwd)
            .output()
            .await;

        match output {
            Ok(out) => {
                if out.status.success() {
                    let mut result = ToolResult::success(format!(
                        "Created worktree at {} on branch {}",
                        worktree_path.display(),
                        branch
                    ));
                    result.metadata.insert(
                        "worktree_path".to_string(),
                        serde_json::Value::String(worktree_path.to_string_lossy().to_string()),
                    );
                    result
                } else {
                    let stderr = String::from_utf8_lossy(&out.stderr);
                    ToolResult::error(format!("git worktree add failed: {}", stderr.trim()))
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
    use std::path::PathBuf;

    fn build_worktree_add_args(branch: &str, worktree_path: &str) -> Vec<String> {
        vec![
            "worktree".to_string(),
            "add".to_string(),
            "-b".to_string(),
            branch.to_string(),
            worktree_path.to_string(),
        ]
    }

    #[test]
    fn test_build_worktree_add_args() {
        let args = build_worktree_add_args("my-branch", "/tmp/worktrees/my-branch");
        assert_eq!(args, vec!["worktree", "add", "-b", "my-branch", "/tmp/worktrees/my-branch"]);
    }

    #[test]
    fn test_worktree_path_construction() {
        let cwd = PathBuf::from("/repo");
        let branch = "feature-x";
        let path = cwd.join(".openharness").join("worktrees").join(branch);
        assert_eq!(path, PathBuf::from("/repo/.openharness/worktrees/feature-x"));
    }

    #[test]
    fn test_input_schema_has_branch() {
        let tool = EnterWorktreeTool;
        let schema = tool.input_schema();
        assert!(schema["properties"]["branch"].is_object());
    }
}

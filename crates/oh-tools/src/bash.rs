//! Execute shell commands tool.

use async_trait::async_trait;
use oh_types::tools::{ToolExecutionContext, ToolResult};
use tracing::instrument;

pub struct BashTool;

#[async_trait]
impl crate::traits::Tool for BashTool {
    fn name(&self) -> &str {
        "Bash"
    }

    fn description(&self) -> &str {
        "Execute a given bash command and return its output."
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The command to execute"
                },
                "timeout": {
                    "type": "integer",
                    "description": "Optional timeout in milliseconds (max 600000)"
                }
            },
            "required": ["command"]
        })
    }

    fn is_read_only(&self, _arguments: &serde_json::Value) -> bool {
        false
    }

    #[instrument(skip(self, context), fields(tool = "Bash"))]
    async fn execute(
        &self,
        arguments: serde_json::Value,
        context: &ToolExecutionContext,
    ) -> ToolResult {
        let command = match arguments.get("command").and_then(|v| v.as_str()) {
            Some(cmd) => cmd,
            None => return ToolResult::error("Missing required parameter: command"),
        };

        let timeout_ms = arguments
            .get("timeout")
            .and_then(|v| v.as_u64())
            .unwrap_or(120_000)
            .min(600_000);

        let result = tokio::time::timeout(
            std::time::Duration::from_millis(timeout_ms),
            tokio::process::Command::new("/bin/bash")
                .arg("-lc")
                .arg(command)
                .current_dir(&context.cwd)
                .output(),
        )
        .await;

        match result {
            Ok(Ok(output)) => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);
                let combined = if stderr.is_empty() {
                    stdout.to_string()
                } else if stdout.is_empty() {
                    stderr.to_string()
                } else {
                    format!("{stdout}\n{stderr}")
                };

                if output.status.success() {
                    ToolResult::success(combined)
                } else {
                    ToolResult::error(format!(
                        "Command exited with code {}\n{}",
                        output.status.code().unwrap_or(-1),
                        combined
                    ))
                }
            }
            Ok(Err(e)) => ToolResult::error(format!("Failed to execute command: {e}")),
            Err(_) => ToolResult::error(format!(
                "Command timed out after {timeout_ms}ms"
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::traits::Tool;
    use oh_types::tools::ToolExecutionContext;

    fn ctx() -> ToolExecutionContext {
        ToolExecutionContext::new(std::env::current_dir().unwrap())
    }

    #[tokio::test]
    async fn test_execute_echo_hello() {
        let tool = BashTool;
        let result = tool
            .execute(serde_json::json!({"command": "echo hello"}), &ctx())
            .await;
        assert!(!result.is_error);
        assert!(result.output.contains("hello"));
    }

    #[tokio::test]
    async fn test_execute_exit_1_is_error() {
        let tool = BashTool;
        let result = tool
            .execute(serde_json::json!({"command": "exit 1"}), &ctx())
            .await;
        assert!(result.is_error);
    }

    #[tokio::test]
    async fn test_execute_with_custom_cwd() {
        let dir = tempfile::tempdir().unwrap();
        let context = ToolExecutionContext::new(dir.path().to_path_buf());
        let tool = BashTool;
        let result = tool
            .execute(serde_json::json!({"command": "pwd"}), &context)
            .await;
        assert!(!result.is_error);
        // The resolved path may differ due to symlinks, so use canonical comparison
        let expected = std::fs::canonicalize(dir.path()).unwrap();
        let actual_trimmed = result.output.trim();
        let actual = std::fs::canonicalize(actual_trimmed).unwrap_or_else(|_| actual_trimmed.into());
        assert_eq!(actual, expected);
    }

    #[tokio::test]
    async fn test_execute_missing_command_arg() {
        let tool = BashTool;
        let result = tool.execute(serde_json::json!({}), &ctx()).await;
        assert!(result.is_error);
        assert!(result.output.contains("command"));
    }

    #[test]
    fn test_schema_has_required_command() {
        let tool = BashTool;
        let schema = tool.input_schema();
        let required = schema["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v == "command"));
    }

    #[test]
    fn test_is_read_only_returns_false() {
        let tool = BashTool;
        assert!(!tool.is_read_only(&serde_json::json!({})));
    }
}

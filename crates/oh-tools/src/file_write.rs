//! Write files to the filesystem tool.

use async_trait::async_trait;
use oh_types::tools::{ToolExecutionContext, ToolResult};
use std::path::Path;

pub struct FileWriteTool;

#[async_trait]
impl crate::traits::Tool for FileWriteTool {
    fn name(&self) -> &str {
        "Write"
    }

    fn description(&self) -> &str {
        "Create or overwrite a text file"
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "file_path": { "type": "string", "description": "Path of the file to write" },
                "content": { "type": "string", "description": "Full file contents" }
            },
            "required": ["file_path", "content"]
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
        let file_path = match arguments.get("file_path").and_then(|v| v.as_str()) {
            Some(p) => p,
            None => return ToolResult::error("Missing required parameter: file_path"),
        };
        let content = match arguments.get("content").and_then(|v| v.as_str()) {
            Some(c) => c,
            None => return ToolResult::error("Missing required parameter: content"),
        };

        let path = Path::new(file_path);

        if let Some(parent) = path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                return ToolResult::error(format!(
                    "Failed to create directories: {}",
                    e
                ));
            }
        }

        match std::fs::write(path, content.as_bytes()) {
            Ok(()) => ToolResult::success(format!("Successfully wrote to {}", file_path)),
            Err(e) => ToolResult::error(format!("Failed to write file: {}", e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::traits::Tool;
    use oh_types::tools::ToolExecutionContext;
    use std::path::PathBuf;
    use tempfile::tempdir;

    fn ctx() -> ToolExecutionContext {
        ToolExecutionContext::new(PathBuf::from("/tmp"))
    }

    #[tokio::test]
    async fn test_write_creates_file() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("new.txt");

        let tool = FileWriteTool;
        let result = tool
            .execute(
                serde_json::json!({
                    "file_path": file.to_str().unwrap(),
                    "content": "hello world"
                }),
                &ctx(),
            )
            .await;

        assert!(!result.is_error);
        assert!(result.output.contains("Successfully wrote"));
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "hello world");
    }

    #[tokio::test]
    async fn test_write_creates_nested_dirs() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("a").join("b").join("c").join("deep.txt");

        let tool = FileWriteTool;
        let result = tool
            .execute(
                serde_json::json!({
                    "file_path": file.to_str().unwrap(),
                    "content": "nested"
                }),
                &ctx(),
            )
            .await;

        assert!(!result.is_error);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "nested");
    }

    #[tokio::test]
    async fn test_write_overwrites_existing() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("existing.txt");
        std::fs::write(&file, "old content").unwrap();

        let tool = FileWriteTool;
        let result = tool
            .execute(
                serde_json::json!({
                    "file_path": file.to_str().unwrap(),
                    "content": "new content"
                }),
                &ctx(),
            )
            .await;

        assert!(!result.is_error);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "new content");
    }

    #[test]
    fn test_is_not_read_only() {
        let tool = FileWriteTool;
        assert!(!tool.is_read_only(&serde_json::json!({})));
    }
}

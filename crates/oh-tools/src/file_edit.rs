//! Perform exact string replacements in files tool.

use async_trait::async_trait;
use oh_types::tools::{ToolExecutionContext, ToolResult};
use std::path::Path;

pub struct FileEditTool;

#[async_trait]
impl crate::traits::Tool for FileEditTool {
    fn name(&self) -> &str {
        "Edit"
    }

    fn description(&self) -> &str {
        "Perform exact string replacements in files"
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "file_path": { "type": "string", "description": "Path of the file to edit" },
                "old_string": { "type": "string", "description": "Existing text to replace" },
                "new_string": { "type": "string", "description": "Replacement text" },
                "replace_all": { "type": "boolean", "default": false, "description": "Replace all occurrences" }
            },
            "required": ["file_path", "old_string", "new_string"]
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
        let old_string = match arguments.get("old_string").and_then(|v| v.as_str()) {
            Some(s) => s,
            None => return ToolResult::error("Missing required parameter: old_string"),
        };
        let new_string = match arguments.get("new_string").and_then(|v| v.as_str()) {
            Some(s) => s,
            None => return ToolResult::error("Missing required parameter: new_string"),
        };
        let replace_all = arguments
            .get("replace_all")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        if old_string == new_string {
            return ToolResult::error("old_string and new_string are identical");
        }

        let path = Path::new(file_path);
        if !path.exists() {
            return ToolResult::error(format!("File not found: {}", file_path));
        }

        let original = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) => return ToolResult::error(format!("Failed to read file: {}", e)),
        };

        if !original.contains(old_string) {
            return ToolResult::error("old_string was not found in the file");
        }

        let updated = if replace_all {
            original.replace(old_string, new_string)
        } else {
            original.replacen(old_string, new_string, 1)
        };

        match std::fs::write(path, updated.as_bytes()) {
            Ok(()) => ToolResult::success(format!("Successfully edited {}", file_path)),
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
    async fn test_edit_replace_first_occurrence() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("test.txt");
        std::fs::write(&file, "foo bar foo baz").unwrap();

        let tool = FileEditTool;
        let result = tool
            .execute(
                serde_json::json!({
                    "file_path": file.to_str().unwrap(),
                    "old_string": "foo",
                    "new_string": "qux"
                }),
                &ctx(),
            )
            .await;

        assert!(!result.is_error);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "qux bar foo baz");
    }

    #[tokio::test]
    async fn test_edit_replace_all() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("test.txt");
        std::fs::write(&file, "foo bar foo baz").unwrap();

        let tool = FileEditTool;
        let result = tool
            .execute(
                serde_json::json!({
                    "file_path": file.to_str().unwrap(),
                    "old_string": "foo",
                    "new_string": "qux",
                    "replace_all": true
                }),
                &ctx(),
            )
            .await;

        assert!(!result.is_error);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "qux bar qux baz");
    }

    #[tokio::test]
    async fn test_edit_old_string_not_found() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("test.txt");
        std::fs::write(&file, "hello world").unwrap();

        let tool = FileEditTool;
        let result = tool
            .execute(
                serde_json::json!({
                    "file_path": file.to_str().unwrap(),
                    "old_string": "missing",
                    "new_string": "replaced"
                }),
                &ctx(),
            )
            .await;

        assert!(result.is_error);
        assert!(result.output.contains("not found in the file"));
    }

    #[tokio::test]
    async fn test_edit_file_not_found() {
        let tool = FileEditTool;
        let result = tool
            .execute(
                serde_json::json!({
                    "file_path": "/tmp/nonexistent_oh_test_edit.txt",
                    "old_string": "a",
                    "new_string": "b"
                }),
                &ctx(),
            )
            .await;

        assert!(result.is_error);
        assert!(result.output.contains("File not found"));
    }

    #[tokio::test]
    async fn test_edit_same_strings() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("test.txt");
        std::fs::write(&file, "hello").unwrap();

        let tool = FileEditTool;
        let result = tool
            .execute(
                serde_json::json!({
                    "file_path": file.to_str().unwrap(),
                    "old_string": "hello",
                    "new_string": "hello"
                }),
                &ctx(),
            )
            .await;

        assert!(result.is_error);
        assert!(result.output.contains("identical"));
    }

    #[test]
    fn test_is_not_read_only() {
        let tool = FileEditTool;
        assert!(!tool.is_read_only(&serde_json::json!({})));
    }
}

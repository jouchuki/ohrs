//! Read files from the filesystem tool.

use async_trait::async_trait;
use oh_types::tools::{ToolExecutionContext, ToolResult};
use std::path::Path;

pub struct FileReadTool;

#[async_trait]
impl crate::traits::Tool for FileReadTool {
    fn name(&self) -> &str {
        "Read"
    }

    fn description(&self) -> &str {
        "Read a UTF-8 text file with line numbers"
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "file_path": { "type": "string", "description": "Path of the file to read" },
                "offset": { "type": "integer", "default": 0, "description": "Zero-based starting line" },
                "limit": { "type": "integer", "default": 2000, "description": "Number of lines to return" }
            },
            "required": ["file_path"]
        })
    }

    fn is_read_only(&self, _arguments: &serde_json::Value) -> bool {
        true
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
        let offset = arguments
            .get("offset")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as usize;
        let limit = arguments
            .get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(2000) as usize;

        let path = Path::new(file_path);
        if !path.exists() {
            return ToolResult::error(format!("File not found: {}", file_path));
        }
        if path.is_dir() {
            return ToolResult::error(format!("Cannot read directory: {}", file_path));
        }

        let raw = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) => return ToolResult::error(format!("Failed to read file: {}", e)),
        };

        // Check for null bytes in first 8KB to detect binary files
        let check_len = raw.len().min(8192);
        if raw[..check_len].contains(&0) {
            return ToolResult::error(format!(
                "Binary file cannot be read as text: {}",
                file_path
            ));
        }

        let text = String::from_utf8_lossy(&raw);
        let lines: Vec<&str> = text.lines().collect();
        let end = (offset + limit).min(lines.len());
        if offset >= lines.len() {
            return ToolResult::success(format!(
                "(no content in selected range for {})",
                file_path
            ));
        }
        let selected = &lines[offset..end];
        let numbered: Vec<String> = selected
            .iter()
            .enumerate()
            .map(|(i, line)| format!("{:>6}\t{}", offset + i + 1, line))
            .collect();
        ToolResult::success(numbered.join("\n"))
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
    async fn test_read_file_with_line_numbers() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("test.txt");
        std::fs::write(&file, "line1\nline2\nline3\n").unwrap();

        let tool = FileReadTool;
        let result = tool
            .execute(
                serde_json::json!({ "file_path": file.to_str().unwrap() }),
                &ctx(),
            )
            .await;

        assert!(!result.is_error);
        assert!(result.output.contains("1\tline1"));
        assert!(result.output.contains("2\tline2"));
        assert!(result.output.contains("3\tline3"));
    }

    #[tokio::test]
    async fn test_read_file_with_offset() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("test.txt");
        std::fs::write(&file, "a\nb\nc\nd\ne\n").unwrap();

        let tool = FileReadTool;
        let result = tool
            .execute(
                serde_json::json!({ "file_path": file.to_str().unwrap(), "offset": 2 }),
                &ctx(),
            )
            .await;

        assert!(!result.is_error);
        // Offset 2 means starting from the 3rd line (0-indexed), so line number 3
        assert!(result.output.contains("3\tc"));
        assert!(!result.output.contains("1\ta"));
    }

    #[tokio::test]
    async fn test_read_file_with_limit() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("test.txt");
        std::fs::write(&file, "a\nb\nc\nd\ne\n").unwrap();

        let tool = FileReadTool;
        let result = tool
            .execute(
                serde_json::json!({ "file_path": file.to_str().unwrap(), "limit": 2 }),
                &ctx(),
            )
            .await;

        assert!(!result.is_error);
        assert!(result.output.contains("1\ta"));
        assert!(result.output.contains("2\tb"));
        assert!(!result.output.contains("3\tc"));
    }

    #[tokio::test]
    async fn test_read_missing_file() {
        let tool = FileReadTool;
        let result = tool
            .execute(
                serde_json::json!({ "file_path": "/tmp/nonexistent_oh_test_file.txt" }),
                &ctx(),
            )
            .await;

        assert!(result.is_error);
        assert!(result.output.contains("File not found"));
    }

    #[tokio::test]
    async fn test_read_binary_file() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("binary.bin");
        let mut data = vec![0u8; 100];
        data[0] = 0x89; // PNG-like header
        data[1] = 0x00; // null byte
        std::fs::write(&file, &data).unwrap();

        let tool = FileReadTool;
        let result = tool
            .execute(
                serde_json::json!({ "file_path": file.to_str().unwrap() }),
                &ctx(),
            )
            .await;

        assert!(result.is_error);
        assert!(result.output.contains("Binary file"));
    }

    #[test]
    fn test_is_read_only() {
        let tool = FileReadTool;
        assert!(tool.is_read_only(&serde_json::json!({})));
    }
}

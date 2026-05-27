//! Read files from the filesystem tool.

use async_trait::async_trait;
use oh_types::tools::{ToolExecutionContext, ToolResult};
use std::io::{BufRead, BufReader};
use std::path::Path;

pub struct FileReadTool;

/// Default number of lines returned when no explicit limit is supplied.
const DEFAULT_LINE_LIMIT: usize = 2000;
/// Hard cap on bytes inspected for binary detection (null-byte scan).
const BINARY_SNIFF_BYTES: usize = 8192;
/// Hard cap on total bytes streamed into memory for a single read. This bounds
/// the worst case (TOOL-9) even when the requested line range is enormous or
/// the file has pathologically long lines.
const MAX_READ_BYTES: usize = 10 * 1024 * 1024;

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

    fn path_args(&self, input: &serde_json::Value) -> Vec<String> {
        input
            .get("file_path")
            .and_then(|v| v.as_str())
            .map(|s| vec![s.to_string()])
            .unwrap_or_default()
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        context: &ToolExecutionContext,
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
            .unwrap_or(DEFAULT_LINE_LIMIT as u64) as usize;

        // TOOL-3 / TOOL-4: canonicalize the real target and confine to roots.
        let path = match crate::pathsafe::resolve_and_confine(
            &context.cwd,
            file_path,
            &context.allowed_roots,
        ) {
            Ok(p) => p,
            Err(e) => return ToolResult::error(e.to_string()),
        };

        if !path.exists() {
            return ToolResult::error(format!("File not found: {}", file_path));
        }
        if path.is_dir() {
            return ToolResult::error(format!("Cannot read directory: {}", file_path));
        }

        match read_line_range(&path, offset, limit) {
            Ok(ReadOutcome::Binary) => {
                ToolResult::error(format!("Binary file cannot be read as text: {}", file_path))
            }
            Ok(ReadOutcome::OutOfRange) => ToolResult::success(format!(
                "(no content in selected range for {})",
                file_path
            )),
            Ok(ReadOutcome::Lines { numbered, truncated }) => {
                let mut body = numbered.join("\n");
                if truncated {
                    body.push_str("\n...[truncated: read byte cap reached]");
                }
                ToolResult::success(body)
            }
            Err(e) => ToolResult::error(format!("Failed to read file: {}", e)),
        }
    }
}

/// Result of a bounded line-range read.
enum ReadOutcome {
    /// File detected as binary (null byte within the sniff window).
    Binary,
    /// The requested offset is past the end of the file.
    OutOfRange,
    /// Numbered lines and whether the byte cap forced an early stop.
    Lines {
        numbered: Vec<String>,
        truncated: bool,
    },
}

/// Stream a file line-by-line, materializing only `[offset, offset+limit)` and
/// never reading more than [`MAX_READ_BYTES`] total (TOOL-9). Detects binary
/// content from the leading bytes before emitting any text.
fn read_line_range(path: &Path, offset: usize, limit: usize) -> std::io::Result<ReadOutcome> {
    let file = std::fs::File::open(path)?;
    let mut reader = BufReader::new(file);

    // Binary sniff on the leading bytes without consuming the stream position
    // beyond what we re-read below.
    {
        use std::io::Read;
        let mut sniff = vec![0u8; BINARY_SNIFF_BYTES];
        let n = (&mut reader).take(BINARY_SNIFF_BYTES as u64).read(&mut sniff)?;
        if sniff[..n].contains(&0) {
            return Ok(ReadOutcome::Binary);
        }
    }

    // Re-open to read from the start now that the sniff consumed bytes.
    let file = std::fs::File::open(path)?;
    let reader = BufReader::new(file);

    let end = offset.saturating_add(limit);
    let mut numbered: Vec<String> = Vec::new();
    let mut bytes_read: usize = 0;
    let mut truncated = false;
    let mut saw_any = false;

    for (idx, line_result) in reader.lines().enumerate() {
        saw_any = true;
        if idx >= end {
            break;
        }
        let line = match line_result {
            Ok(l) => l,
            Err(_) => String::from_utf8_lossy(b"").to_string(),
        };
        bytes_read = bytes_read.saturating_add(line.len()).saturating_add(1);
        if bytes_read > MAX_READ_BYTES {
            truncated = true;
            break;
        }
        if idx >= offset {
            numbered.push(format!("{:>6}\t{}", idx + 1, line));
        }
    }

    if saw_any && numbered.is_empty() && !truncated {
        return Ok(ReadOutcome::OutOfRange);
    }

    Ok(ReadOutcome::Lines { numbered, truncated })
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

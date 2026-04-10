//! Search file contents with regex tool.

use async_trait::async_trait;
use oh_types::tools::{ToolExecutionContext, ToolResult};
use std::path::{Path, PathBuf};

pub struct GrepTool;

const DEFAULT_HEAD_LIMIT: usize = 250;

fn resolve_path(base: &Path, candidate: Option<&str>) -> PathBuf {
    match candidate {
        Some(s) if !s.is_empty() => {
            let p = PathBuf::from(s);
            if p.is_absolute() {
                p
            } else {
                base.join(p)
            }
        }
        _ => base.to_path_buf(),
    }
}

/// Check if data looks binary (contains null bytes in first 8KB).
fn is_binary(data: &[u8]) -> bool {
    let check_len = data.len().min(8192);
    data[..check_len].contains(&0)
}

/// Map common file type names to extensions.
fn type_to_extensions(file_type: &str) -> Vec<String> {
    match file_type {
        "js" => vec!["js".into(), "jsx".into(), "mjs".into()],
        "ts" => vec!["ts".into(), "tsx".into()],
        "py" => vec!["py".into(), "pyi".into()],
        "rust" | "rs" => vec!["rs".into()],
        "go" => vec!["go".into()],
        "java" => vec!["java".into()],
        "c" => vec!["c".into(), "h".into()],
        "cpp" => vec!["cpp".into(), "cc".into(), "cxx".into(), "hpp".into(), "hxx".into()],
        "rb" => vec!["rb".into()],
        "md" => vec!["md".into()],
        "json" => vec!["json".into()],
        "yaml" | "yml" => vec!["yaml".into(), "yml".into()],
        "toml" => vec!["toml".into()],
        "html" => vec!["html".into(), "htm".into()],
        "css" => vec!["css".into()],
        other => vec![other.into()],
    }
}

/// Recursively collect files from a directory.
fn walk_files(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    walk_files_recursive(dir, &mut files);
    files.sort();
    files
}

fn walk_files_recursive(dir: &Path, files: &mut Vec<PathBuf>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            // Skip hidden directories
            if path
                .file_name()
                .and_then(|n| n.to_str())
                .map_or(false, |n| n.starts_with('.'))
            {
                continue;
            }
            walk_files_recursive(&path, files);
        } else if path.is_file() {
            files.push(path);
        }
    }
}

fn matches_glob_filter(path: &Path, glob_pattern: &str) -> bool {
    let file_name = match path.file_name().and_then(|n| n.to_str()) {
        Some(n) => n,
        None => return false,
    };

    if let Ok(pattern) = glob::Pattern::new(glob_pattern) {
        let path_str = path.to_string_lossy();
        pattern.matches(&path_str) || pattern.matches(file_name)
    } else {
        false
    }
}

fn matches_type_filter(path: &Path, extensions: &[String]) -> bool {
    match path.extension().and_then(|e| e.to_str()) {
        Some(ext) => extensions.iter().any(|e| e == ext),
        None => false,
    }
}

#[async_trait]
impl crate::traits::Tool for GrepTool {
    fn name(&self) -> &str {
        "Grep"
    }

    fn description(&self) -> &str {
        "Search file contents with regex"
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "Regex pattern to search for" },
                "path": { "type": "string", "description": "Directory to search in (defaults to cwd)" },
                "glob": { "type": "string", "description": "File glob filter (e.g. *.rs)" },
                "type": { "type": "string", "description": "File type filter (e.g. rs, py, js)" },
                "-i": { "type": "boolean", "description": "Case insensitive search" },
                "output_mode": {
                    "type": "string",
                    "enum": ["content", "files_with_matches", "count"],
                    "description": "Output mode (default: files_with_matches)"
                },
                "head_limit": { "type": "integer", "description": "Max results (default: 250)" },
                "-n": { "type": "boolean", "description": "Show line numbers (default: true)" },
                "-A": { "type": "integer", "description": "Lines of context after match" },
                "-B": { "type": "integer", "description": "Lines of context before match" },
                "-C": { "type": "integer", "description": "Lines of context before and after match" }
            },
            "required": ["pattern"]
        })
    }

    fn is_read_only(&self, _arguments: &serde_json::Value) -> bool {
        true
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        context: &ToolExecutionContext,
    ) -> ToolResult {
        let pattern_str = match arguments.get("pattern").and_then(|v| v.as_str()) {
            Some(p) => p,
            None => return ToolResult::error("Missing required parameter: pattern"),
        };

        let case_insensitive = arguments.get("-i").and_then(|v| v.as_bool()).unwrap_or(false);
        let output_mode = arguments
            .get("output_mode")
            .and_then(|v| v.as_str())
            .unwrap_or("files_with_matches");
        let head_limit = arguments
            .get("head_limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(DEFAULT_HEAD_LIMIT as u64) as usize;
        let show_line_numbers = arguments.get("-n").and_then(|v| v.as_bool()).unwrap_or(true);
        let context_after = arguments
            .get("-A")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as usize;
        let context_before = arguments
            .get("-B")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as usize;
        let context_around = arguments
            .get("-C")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as usize;

        let after = context_after.max(context_around);
        let before = context_before.max(context_around);

        let regex_pattern = if case_insensitive {
            format!("(?i){pattern_str}")
        } else {
            pattern_str.to_string()
        };

        let re = match regex::Regex::new(&regex_pattern) {
            Ok(r) => r,
            Err(e) => return ToolResult::error(format!("Invalid regex: {e}")),
        };

        let search_dir = resolve_path(
            &context.cwd,
            arguments.get("path").and_then(|v| v.as_str()),
        );

        let glob_filter = arguments.get("glob").and_then(|v| v.as_str());
        let type_filter = arguments.get("type").and_then(|v| v.as_str());
        let type_extensions: Option<Vec<String>> = type_filter.map(|t| type_to_extensions(t));

        let files = walk_files(&search_dir);
        let mut output_lines: Vec<String> = Vec::new();

        for file_path in &files {
            if output_lines.len() >= head_limit {
                break;
            }

            let rel_path = file_path
                .strip_prefix(&search_dir)
                .unwrap_or(file_path);

            if let Some(glob_pat) = glob_filter {
                if !matches_glob_filter(rel_path, glob_pat) {
                    continue;
                }
            }

            if let Some(ref exts) = type_extensions {
                if !matches_type_filter(file_path, exts) {
                    continue;
                }
            }

            let raw = match std::fs::read(file_path) {
                Ok(data) => data,
                Err(_) => continue,
            };

            if is_binary(&raw) {
                continue;
            }

            let text = String::from_utf8_lossy(&raw);
            let all_lines: Vec<&str> = text.lines().collect();
            let rel_display = rel_path.to_string_lossy();

            match output_mode {
                "files_with_matches" => {
                    if all_lines.iter().any(|line| re.is_match(line)) {
                        output_lines.push(rel_display.to_string());
                        if output_lines.len() >= head_limit {
                            break;
                        }
                    }
                }
                "count" => {
                    let count = all_lines.iter().filter(|line| re.is_match(line)).count();
                    if count > 0 {
                        output_lines.push(format!("{rel_display}:{count}"));
                        if output_lines.len() >= head_limit {
                            break;
                        }
                    }
                }
                _ => {
                    let mut match_indices: Vec<usize> = Vec::new();
                    for (i, line) in all_lines.iter().enumerate() {
                        if re.is_match(line) {
                            match_indices.push(i);
                        }
                    }

                    let mut emitted: std::collections::HashSet<usize> = std::collections::HashSet::new();
                    for &idx in &match_indices {
                        let start = idx.saturating_sub(before);
                        let end = (idx + after + 1).min(all_lines.len());
                        for i in start..end {
                            if emitted.insert(i) {
                                let line_no = i + 1;
                                let line = all_lines[i];
                                if show_line_numbers {
                                    output_lines.push(format!("{rel_display}:{line_no}:{line}"));
                                } else {
                                    output_lines.push(format!("{rel_display}:{line}"));
                                }
                                if output_lines.len() >= head_limit {
                                    break;
                                }
                            }
                        }
                        if output_lines.len() >= head_limit {
                            break;
                        }
                    }
                }
            }
        }

        if output_lines.is_empty() {
            return ToolResult::success("(no matches)");
        }

        ToolResult::success(output_lines.join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::traits::Tool;
    use oh_types::tools::ToolExecutionContext;
    use std::fs;

    fn setup_test_dir() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("hello.rs"), "fn main() {\n    println!(\"hello\");\n}\n").unwrap();
        fs::write(dir.path().join("lib.rs"), "pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n").unwrap();
        fs::write(dir.path().join("notes.txt"), "Hello World\nfoo bar\nHELLO again\n").unwrap();
        dir
    }

    #[tokio::test]
    async fn test_grep_regex_finds_matches() {
        let dir = setup_test_dir();
        let ctx = ToolExecutionContext::new(dir.path().to_path_buf());
        let tool = GrepTool;
        let result = tool
            .execute(
                serde_json::json!({"pattern": "fn\\s+\\w+", "output_mode": "content"}),
                &ctx,
            )
            .await;

        assert!(!result.is_error);
        assert!(result.output.contains("fn main"), "output: {}", result.output);
        assert!(result.output.contains("fn add"), "output: {}", result.output);
    }

    #[tokio::test]
    async fn test_grep_case_insensitive() {
        let dir = setup_test_dir();
        let ctx = ToolExecutionContext::new(dir.path().to_path_buf());
        let tool = GrepTool;
        let result = tool
            .execute(
                serde_json::json!({"pattern": "hello", "-i": true, "output_mode": "content"}),
                &ctx,
            )
            .await;

        assert!(!result.is_error);
        // Should match "Hello World", "hello", and "HELLO again"
        let lines: Vec<&str> = result.output.lines().collect();
        assert!(lines.len() >= 3, "expected at least 3 matches, got: {}", result.output);
    }

    #[tokio::test]
    async fn test_grep_files_with_matches_mode() {
        let dir = setup_test_dir();
        let ctx = ToolExecutionContext::new(dir.path().to_path_buf());
        let tool = GrepTool;
        let result = tool
            .execute(
                serde_json::json!({"pattern": "fn", "output_mode": "files_with_matches"}),
                &ctx,
            )
            .await;

        assert!(!result.is_error);
        assert!(result.output.contains("hello.rs"), "output: {}", result.output);
        assert!(result.output.contains("lib.rs"), "output: {}", result.output);
        assert!(!result.output.contains("notes.txt"), "output: {}", result.output);
    }

    #[tokio::test]
    async fn test_grep_content_mode_with_line_numbers() {
        let dir = setup_test_dir();
        let ctx = ToolExecutionContext::new(dir.path().to_path_buf());
        let tool = GrepTool;
        let result = tool
            .execute(
                serde_json::json!({"pattern": "println", "output_mode": "content", "-n": true}),
                &ctx,
            )
            .await;

        assert!(!result.is_error);
        // Should contain path:line_no:content
        assert!(result.output.contains(":2:"), "expected line number 2, output: {}", result.output);
    }

    #[tokio::test]
    async fn test_grep_count_mode() {
        let dir = setup_test_dir();
        let ctx = ToolExecutionContext::new(dir.path().to_path_buf());
        let tool = GrepTool;
        let result = tool
            .execute(
                serde_json::json!({"pattern": "hello", "-i": true, "output_mode": "count"}),
                &ctx,
            )
            .await;

        assert!(!result.is_error);
        // notes.txt should have matches
        assert!(result.output.contains("notes.txt:"), "output: {}", result.output);
    }

    #[tokio::test]
    async fn test_grep_no_matches() {
        let dir = setup_test_dir();
        let ctx = ToolExecutionContext::new(dir.path().to_path_buf());
        let tool = GrepTool;
        let result = tool
            .execute(
                serde_json::json!({"pattern": "zzzznonexistent"}),
                &ctx,
            )
            .await;

        assert!(!result.is_error);
        assert_eq!(result.output, "(no matches)");
    }

    #[tokio::test]
    async fn test_grep_skips_binary() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("binary.bin"), b"hello\x00world").unwrap();
        fs::write(dir.path().join("text.txt"), "hello world").unwrap();

        let ctx = ToolExecutionContext::new(dir.path().to_path_buf());
        let tool = GrepTool;
        let result = tool
            .execute(
                serde_json::json!({"pattern": "hello", "output_mode": "files_with_matches"}),
                &ctx,
            )
            .await;

        assert!(!result.is_error);
        assert!(result.output.contains("text.txt"));
        assert!(!result.output.contains("binary.bin"));
    }

    #[tokio::test]
    async fn test_grep_type_filter() {
        let dir = setup_test_dir();
        let ctx = ToolExecutionContext::new(dir.path().to_path_buf());
        let tool = GrepTool;
        let result = tool
            .execute(
                serde_json::json!({"pattern": "fn|Hello", "type": "rs", "output_mode": "files_with_matches"}),
                &ctx,
            )
            .await;

        assert!(!result.is_error);
        assert!(result.output.contains("hello.rs") || result.output.contains("lib.rs"));
        assert!(!result.output.contains("notes.txt"));
    }
}

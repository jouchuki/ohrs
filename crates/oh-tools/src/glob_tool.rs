//! Pattern-based file search tool.

use async_trait::async_trait;
use oh_types::tools::{ToolExecutionContext, ToolResult};
use std::path::{Path, PathBuf};

pub struct GlobTool;

const MAX_RESULTS: usize = 200;

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

#[async_trait]
impl crate::traits::Tool for GlobTool {
    fn name(&self) -> &str {
        "Glob"
    }

    fn description(&self) -> &str {
        "Pattern-based file search"
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "Glob pattern to match files" },
                "path": { "type": "string", "description": "Directory to search in (defaults to cwd)" }
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
        let pattern = match arguments.get("pattern").and_then(|v| v.as_str()) {
            Some(p) => p,
            None => return ToolResult::error("Missing required parameter: pattern"),
        };

        let search_dir = resolve_path(&context.cwd, arguments.get("path").and_then(|v| v.as_str()));

        let full_pattern = search_dir.join(pattern);
        let full_pattern_str = match full_pattern.to_str() {
            Some(s) => s.to_string(),
            None => return ToolResult::error("Invalid pattern path (non-UTF-8)"),
        };

        let entries = match glob::glob(&full_pattern_str) {
            Ok(paths) => paths,
            Err(e) => return ToolResult::error(format!("Invalid glob pattern: {e}")),
        };

        let mut matches: Vec<String> = Vec::new();
        for entry in entries {
            match entry {
                Ok(path) => {
                    let rel = path
                        .strip_prefix(&search_dir)
                        .unwrap_or(&path)
                        .to_string_lossy()
                        .to_string();
                    matches.push(rel);
                }
                Err(_) => continue,
            }
        }

        matches.sort();

        if matches.is_empty() {
            return ToolResult::success("(no matches)");
        }

        matches.truncate(MAX_RESULTS);
        ToolResult::success(matches.join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::traits::Tool;
    use oh_types::tools::ToolExecutionContext;
    use std::fs;

    #[tokio::test]
    async fn test_glob_matches_files() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("foo.rs"), "fn main() {}").unwrap();
        fs::write(dir.path().join("bar.rs"), "fn bar() {}").unwrap();
        fs::write(dir.path().join("baz.txt"), "hello").unwrap();

        let ctx = ToolExecutionContext::new(dir.path().to_path_buf());
        let tool = GlobTool;
        let result = tool
            .execute(serde_json::json!({"pattern": "*.rs"}), &ctx)
            .await;

        assert!(!result.is_error);
        let output = &result.output;
        assert!(output.contains("foo.rs"), "expected foo.rs in output: {output}");
        assert!(output.contains("bar.rs"), "expected bar.rs in output: {output}");
        assert!(!output.contains("baz.txt"), "should not contain baz.txt: {output}");
    }

    #[tokio::test]
    async fn test_glob_no_matches() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ToolExecutionContext::new(dir.path().to_path_buf());
        let tool = GlobTool;
        let result = tool
            .execute(serde_json::json!({"pattern": "*.xyz"}), &ctx)
            .await;

        assert!(!result.is_error);
        assert_eq!(result.output, "(no matches)");
    }

    #[tokio::test]
    async fn test_glob_with_path() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("sub");
        fs::create_dir(&sub).unwrap();
        fs::write(sub.join("a.rs"), "").unwrap();

        let ctx = ToolExecutionContext::new(dir.path().to_path_buf());
        let tool = GlobTool;
        let result = tool
            .execute(
                serde_json::json!({"pattern": "*.rs", "path": "sub"}),
                &ctx,
            )
            .await;

        assert!(!result.is_error);
        assert!(result.output.contains("a.rs"));
    }

    #[tokio::test]
    async fn test_glob_cap_at_200() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..250 {
            fs::write(dir.path().join(format!("file_{i:04}.txt")), "").unwrap();
        }

        let ctx = ToolExecutionContext::new(dir.path().to_path_buf());
        let tool = GlobTool;
        let result = tool
            .execute(serde_json::json!({"pattern": "*.txt"}), &ctx)
            .await;

        assert!(!result.is_error);
        let count = result.output.lines().count();
        assert_eq!(count, 200);
    }
}

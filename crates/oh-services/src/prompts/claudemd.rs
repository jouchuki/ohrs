//! CLAUDE.md discovery, frontmatter stripping, and loading.

use std::path::{Path, PathBuf};

/// Walk from `cwd` up to the filesystem root, collecting CLAUDE.md files.
/// Also checks `~/.claude/CLAUDE.md` as a global layer.
pub fn discover(cwd: &Path) -> Vec<PathBuf> {
    let mut results: Vec<PathBuf> = Vec::new();
    let mut seen: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();

    // Walk upward from cwd
    let start = cwd.to_path_buf();
    let mut dir: Option<&Path> = Some(start.as_path());
    while let Some(current) = dir {
        for candidate in &[
            current.join("CLAUDE.md"),
            current.join(".claude").join("CLAUDE.md"),
        ] {
            if candidate.exists() {
                let canon = candidate
                    .canonicalize()
                    .unwrap_or_else(|_| candidate.clone());
                if seen.insert(canon.clone()) {
                    results.push(candidate.clone());
                }
            }
        }
        dir = current.parent();
    }

    // Global layer: ~/.claude/CLAUDE.md
    if let Some(home) = dirs::home_dir() {
        let global = home.join(".claude").join("CLAUDE.md");
        if global.exists() {
            let canon = global.canonicalize().unwrap_or_else(|_| global.clone());
            if seen.insert(canon) {
                results.push(global);
            }
        }
    }

    results
}

/// Parse optional YAML frontmatter (`---\n…\n---\n`) and return the body.
///
/// The frontmatter values are currently discarded; we only strip it so the
/// body is clean Markdown.  Callers that need frontmatter data can extend
/// this in the future.
pub fn strip_frontmatter(content: &str) -> &str {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return content;
    }
    // Skip the opening `---` line
    let rest = &trimmed["---".len()..];
    let rest = rest.trim_start_matches('\n').trim_start_matches('\r');
    // Find the closing `---`
    if let Some(end) = rest.find("\n---") {
        let after = &rest[end + "\n---".len()..];
        // Consume one optional newline after the closing fence
        after.trim_start_matches('\n').trim_start_matches('\r')
    } else {
        content
    }
}

/// Load all discovered CLAUDE.md files into a single prompt section.
///
/// Returns `None` when no files are found so the caller can skip the section.
pub fn load_prompt(cwd: &Path) -> Option<String> {
    let files = discover(cwd);
    if files.is_empty() {
        return None;
    }
    const MAX_CHARS: usize = 12_000;

    let mut lines = vec!["# Project Instructions".to_owned()];
    for path in &files {
        let raw = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) => {
                // Warn but continue — missing one file is not fatal.
                tracing::warn!(path = %path.display(), error = %e, "Failed to read CLAUDE.md");
                continue;
            }
        };
        let body = strip_frontmatter(&raw);
        let body = if body.len() > MAX_CHARS {
            &body[..MAX_CHARS]
        } else {
            body
        };
        lines.push(String::new());
        lines.push(format!("## {}", path.display()));
        lines.push("```md".to_owned());
        lines.push(body.trim().to_owned());
        lines.push("```".to_owned());
    }
    Some(lines.join("\n"))
}

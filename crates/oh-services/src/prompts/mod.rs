//! System prompt construction from base template + CLAUDE.md + environment.

pub mod base;
pub mod claudemd;
pub mod context;
pub mod environment;

use std::path::Path;

/// Builds the system prompt for a session.
///
/// Use `with_override` to short-circuit when the user passes `--system-prompt`.
/// Use `bare(true)` to skip CLAUDE.md injection (honours the `--bare` CLI flag).
pub struct PromptBuilder<'a> {
    cwd: &'a Path,
    override_prompt: Option<&'a str>,
    /// Extra text appended after the assembled prompt — mirrors `--append-system-prompt`.
    append_prompt: Option<&'a str>,
    /// When true, CLAUDE.md files are not loaded — mirrors the `--bare` CLI flag.
    bare: bool,
}

impl<'a> PromptBuilder<'a> {
    pub fn new(cwd: &'a Path) -> Self {
        Self { cwd, override_prompt: None, append_prompt: None, bare: false }
    }

    /// If `override_prompt` is Some, `build()` returns it verbatim (skips all layers).
    pub fn with_override(mut self, override_prompt: Option<&'a str>) -> Self {
        self.override_prompt = override_prompt;
        self
    }

    /// Append additional text after the assembled prompt (honours `--append-system-prompt`).
    pub fn with_append(mut self, append: Option<&'a str>) -> Self {
        self.append_prompt = append;
        self
    }

    /// Skip CLAUDE.md discovery when bare is true.
    pub fn bare(mut self, bare: bool) -> Self {
        self.bare = bare;
        self
    }

    /// Build the assembled prompt as an owned String.
    pub fn build(self) -> String {
        if let Some(p) = self.override_prompt {
            return p.to_owned();
        }
        let mut prompt = context::assemble(self.cwd, self.bare);
        if let Some(extra) = self.append_prompt {
            if !extra.is_empty() {
                prompt.push_str("\n\n");
                prompt.push_str(extra);
            }
        }
        prompt
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn tmpdir() -> TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    #[test]
    fn test_with_claude_md_rules() {
        let dir = tmpdir();
        fs::write(dir.path().join("CLAUDE.md"), "# Rules\n- be terse\n").unwrap();
        let prompt = PromptBuilder::new(dir.path()).build();
        assert!(prompt.contains("be terse"), "expected CLAUDE.md rule in prompt");
    }

    #[test]
    fn test_without_claude_md() {
        let dir = tmpdir();
        // No CLAUDE.md written — should still produce base + env without panic.
        let prompt = PromptBuilder::new(dir.path()).build();
        assert!(prompt.contains("# Environment"), "expected environment section");
        assert!(prompt.contains("ohrs"), "expected base template mention");
    }

    #[test]
    fn test_frontmatter_stripped_and_body_included() {
        let dir = tmpdir();
        let content = "---\npriority: high\n---\n# Body\nsome rule here\n";
        fs::write(dir.path().join("CLAUDE.md"), content).unwrap();
        let prompt = PromptBuilder::new(dir.path()).build();
        assert!(prompt.contains("some rule here"), "body should be included");
        // Frontmatter key should not appear verbatim in prompt output
        assert!(!prompt.contains("priority: high"), "frontmatter should be stripped");
    }

    #[test]
    fn test_override_is_verbatim() {
        let dir = tmpdir();
        let prompt = PromptBuilder::new(dir.path())
            .with_override(Some("custom override"))
            .build();
        assert_eq!(prompt.as_str(), "custom override");
    }

    #[test]
    fn test_bare_mode_skips_claude_md() {
        let dir = tmpdir();
        fs::write(dir.path().join("CLAUDE.md"), "# Rules\n- do not appear\n").unwrap();
        // bare = true must suppress CLAUDE.md injection
        let prompt = PromptBuilder::new(dir.path()).bare(true).build();
        assert!(!prompt.contains("do not appear"), "bare mode must skip CLAUDE.md");
        assert!(prompt.contains("# Environment"), "base + env still expected in bare mode");
    }
}

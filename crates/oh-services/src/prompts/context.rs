//! Assembles base + environment + CLAUDE.md into the final system prompt.

use std::path::Path;

use crate::prompts::{base, claudemd, environment};

/// Assemble all layers into one prompt string.
///
/// `bare = true` skips CLAUDE.md injection.
pub fn assemble(cwd: &Path, bare: bool) -> String {
    let env = environment::gather(cwd);
    let env_section = environment::format_section(&env);

    let mut sections: Vec<String> = vec![base::BASE_SYSTEM_PROMPT.to_owned(), env_section];

    if !bare {
        if let Some(claude_md) = claudemd::load_prompt(cwd) {
            sections.push(claude_md);
        }
    }

    sections.join("\n\n")
}

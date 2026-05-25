//! Skill loader — reads `.claude/skills/<name>/SKILL.md` files and builds a registry.
//!
//! ## Lookup order
//! 1. `<project_root>/.claude/skills/` — project-local skills (wins on collision)
//! 2. `~/.claude/skills/` — user-global skills
//!
//! ## SKILL.md format
//! ```markdown
//! ---
//! name: review-pr
//! description: Review a pull request
//! enabled: true   # optional, defaults to true
//! ---
//! # Body
//! Instructions to follow when invoked…
//! ```

use std::{collections::HashMap, path::Path, path::PathBuf};

use serde::Deserialize;
use thiserror::Error;
use tracing::warn;

// ── Error ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum SkillError {
    #[error("I/O error reading skill at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to read skill directory {path}: {source}")]
    ReadDir {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// A SKILL.md file was present but its frontmatter was missing or invalid.
    /// This is non-fatal — the file is skipped — but the variant lets callers
    /// inspect the reason programmatically when needed.
    #[error("invalid frontmatter in {path}: {reason}")]
    InvalidFrontmatter { path: PathBuf, reason: String },
}

// ── Domain types ──────────────────────────────────────────────────────────────

/// A single loaded skill.
#[derive(Debug, Clone)]
pub struct Skill {
    pub name: String,
    pub description: String,
    /// Whether this skill is available for invocation (default: true).
    pub enabled: bool,
    /// The body of the SKILL.md file after the frontmatter block.
    pub content: String,
    /// Absolute path to the SKILL.md file this was loaded from.
    pub source: PathBuf,
}

// ── Raw frontmatter shape ─────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct Frontmatter {
    name: Option<String>,
    description: Option<String>,
    enabled: Option<bool>,
}

// ── Registry ──────────────────────────────────────────────────────────────────

/// Name → [`Skill`] map produced by [`SkillLoader::load_all`].
pub struct SkillRegistry {
    skills: HashMap<String, Skill>,
}

impl SkillRegistry {
    fn new() -> Self {
        Self {
            skills: HashMap::new(),
        }
    }

    /// Register one skill. An existing entry with the same name is **not**
    /// overwritten — the first registration wins (project root loads first).
    fn register(&mut self, skill: Skill) {
        self.skills.entry(skill.name.clone()).or_insert(skill);
    }

    /// Look up a skill by name.
    pub fn get(&self, name: &str) -> Option<&Skill> {
        self.skills.get(name)
    }

    /// All skills, sorted by name for deterministic output.
    pub fn list(&self) -> Vec<&Skill> {
        let mut v: Vec<&Skill> = self.skills.values().collect();
        v.sort_by(|a, b| a.name.cmp(&b.name));
        v
    }

    /// Skill names, sorted.
    pub fn names(&self) -> Vec<&str> {
        self.list().into_iter().map(|s| s.name.as_str()).collect()
    }

    /// Produce the shape that `oh-tools/src/skill.rs` reads from
    /// `context.metadata["skill_registry"]`:
    /// ```json
    /// { "<name>": { "content": "<body>" } }
    /// ```
    pub fn to_tool_metadata(&self) -> serde_json::Map<String, serde_json::Value> {
        self.skills
            .values()
            .filter(|s| s.enabled)
            .map(|s| {
                let entry = serde_json::json!({ "content": s.content });
                (s.name.clone(), entry)
            })
            .collect()
    }
}

// ── Loader ────────────────────────────────────────────────────────────────────

/// Loads skills from one or more root directories.
///
/// Each root is expected to contain sub-directories, one per skill, each
/// containing a `SKILL.md` file.  The first root in `roots` has the highest
/// priority: if two roots provide a skill with the same name, the entry from
/// the earlier root is kept.
pub struct SkillLoader {
    roots: Vec<PathBuf>,
}

impl SkillLoader {
    /// Convenience constructor: sets roots to
    /// `[project_root/.claude/skills, ~/.claude/skills]`.
    pub fn new(project_root: &Path) -> Self {
        let mut roots = Vec::new();

        // Project-local root (highest priority)
        roots.push(project_root.join(".claude").join("skills"));

        // User-global root
        if let Some(home) = dirs::home_dir() {
            let user_root = home.join(".claude").join("skills");
            roots.push(user_root);
        }

        Self { roots }
    }

    /// Full control over which roots are searched (and their priority order).
    pub fn with_roots(roots: Vec<PathBuf>) -> Self {
        Self { roots }
    }

    /// Walk every root in priority order and build a [`SkillRegistry`].
    ///
    /// Errors that apply only to individual skill files are logged as warnings
    /// and skipped; only directory-level I/O errors are returned.
    pub async fn load_all(&self) -> Result<SkillRegistry, SkillError> {
        let mut registry = SkillRegistry::new();

        for root in &self.roots {
            if !root.exists() {
                continue;
            }

            let mut read_dir =
                tokio::fs::read_dir(root)
                    .await
                    .map_err(|e| SkillError::ReadDir {
                        path: root.clone(),
                        source: e,
                    })?;

            // Collect subdirectory entries first so we can sort them.
            let mut entries: Vec<PathBuf> = Vec::new();
            while let Some(entry) =
                read_dir
                    .next_entry()
                    .await
                    .map_err(|e| SkillError::ReadDir {
                        path: root.clone(),
                        source: e,
                    })?
            {
                let path = entry.path();
                if path.is_dir() {
                    entries.push(path);
                }
            }
            entries.sort();

            for dir in entries {
                let skill_md = dir.join("SKILL.md");
                if !skill_md.exists() {
                    continue;
                }

                match load_skill_file(&skill_md, &dir).await {
                    Ok(Some(skill)) => registry.register(skill),
                    Ok(None) => {
                        // warn already emitted inside load_skill_file
                    }
                    Err(e) => {
                        warn!("Skipping skill at {}: {}", skill_md.display(), e);
                    }
                }
            }
        }

        Ok(registry)
    }
}

// ── File-level parsing ────────────────────────────────────────────────────────

/// Read and parse a single `SKILL.md` file.
///
/// On success returns `Some(Skill)`.  Returns `Ok(None)` when the file lacks
/// valid frontmatter — a [`SkillError::InvalidFrontmatter`] is constructed for
/// context and logged as a warning, but the file is intentionally skipped rather
/// than failing the entire load.  I/O failures propagate as `Err`.
async fn load_skill_file(path: &Path, dir: &Path) -> Result<Option<Skill>, SkillError> {
    let raw = tokio::fs::read_to_string(path)
        .await
        .map_err(|e| SkillError::Io {
            path: path.to_path_buf(),
            source: e,
        })?;

    let default_name = dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_string();

    match parse_skill_markdown(&raw, &default_name) {
        Ok(skill_data) => Ok(Some(Skill {
            name: skill_data.name,
            description: skill_data.description,
            enabled: skill_data.enabled,
            content: skill_data.content,
            source: path.to_path_buf(),
        })),
        Err(reason) => {
            let err = SkillError::InvalidFrontmatter {
                path: path.to_path_buf(),
                reason,
            };
            warn!("{err}");
            Ok(None)
        }
    }
}

// Intermediate parsed result before we build a full `Skill`.
struct ParsedSkill {
    name: String,
    description: String,
    enabled: bool,
    content: String,
}

/// Parse a SKILL.md file's content.
///
/// Returns `Ok(ParsedSkill)` on success, or `Err(reason_string)` describing
/// exactly why the file was rejected.  All reasons are also surfaced as a typed
/// `SkillError::InvalidFrontmatter` by the caller.
fn parse_skill_markdown(raw: &str, default_name: &str) -> Result<ParsedSkill, String> {
    // Normalise CRLF → LF so the rest of the parser only has to deal with LF.
    let normalised;
    let text: &str = if raw.contains('\r') {
        normalised = raw.replace("\r\n", "\n").replace('\r', "\n");
        &normalised
    } else {
        raw
    };

    // Require the file to start with a YAML frontmatter block ("---\n…\n---\n")
    if !text.starts_with("---\n") {
        return Err("no YAML frontmatter block (file must start with '---')".into());
    }

    // Find the closing "---" delimiter (must be on its own line)
    let after_open = &text[4..]; // skip the leading "---\n"
    let close_pos = after_open
        .find("\n---\n")
        .ok_or_else(|| "frontmatter block not closed (missing closing '---')".to_string())?;

    let yaml_block = &after_open[..close_pos];
    let body_start = 4 /* "---\n" */ + close_pos + 5 /* "\n---\n" */;
    let content = if body_start <= text.len() {
        text[body_start..].to_string()
    } else {
        String::new()
    };

    let fm: Frontmatter =
        serde_yaml::from_str(yaml_block).map_err(|e| format!("YAML parse error: {e}"))?;

    let name = fm
        .name
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| default_name.to_string());

    // description is required to be non-empty
    let description = fm
        .description
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.trim().to_string())
        .ok_or_else(|| "missing required 'description' field in frontmatter".to_string())?;

    let enabled = fm.enabled.unwrap_or(true);

    Ok(ParsedSkill {
        name,
        description,
        enabled,
        content,
    })
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    // Helper: write a SKILL.md under <root>/<skill_name>/SKILL.md
    fn write_skill(root: &Path, skill_name: &str, content: &str) {
        let dir = root.join(skill_name);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("SKILL.md"), content).unwrap();
    }

    #[tokio::test]
    async fn skills_load_name_description_body() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        write_skill(
            root,
            "foo",
            "---\nname: foo\ndescription: Does foo things\n---\n# Foo\nDo the foo.\n",
        );
        write_skill(
            root,
            "bar",
            "---\nname: bar\ndescription: Does bar things\n---\n# Bar\nDo the bar.\n",
        );

        let loader = SkillLoader::with_roots(vec![root.to_path_buf()]);
        let registry = loader.load_all().await.unwrap();

        let foo = registry.get("foo").expect("foo not found");
        assert_eq!(foo.name, "foo");
        assert_eq!(foo.description, "Does foo things");
        assert!(foo.content.contains("Do the foo."));
        assert!(foo.enabled);

        let bar = registry.get("bar").expect("bar not found");
        assert_eq!(bar.name, "bar");
        assert_eq!(bar.description, "Does bar things");
        assert!(bar.content.contains("Do the bar."));
    }

    #[tokio::test]
    async fn missing_frontmatter_skipped_not_crash() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // No frontmatter at all
        write_skill(root, "broken", "# Just a heading\nNo frontmatter here.\n");
        // Valid skill alongside it
        write_skill(
            root,
            "good",
            "---\nname: good\ndescription: Good skill\n---\nBody text.\n",
        );

        let loader = SkillLoader::with_roots(vec![root.to_path_buf()]);
        let registry = loader.load_all().await.unwrap();

        assert!(registry.get("broken").is_none(), "broken should be skipped");
        assert!(registry.get("good").is_some(), "good should load");
    }

    #[tokio::test]
    async fn project_root_overrides_home_root() {
        let home_tmp = TempDir::new().unwrap();
        let project_tmp = TempDir::new().unwrap();

        // Same skill name in both roots
        write_skill(
            home_tmp.path(),
            "shared",
            "---\nname: shared\ndescription: From home\n---\nHome body.\n",
        );
        write_skill(
            project_tmp.path(),
            "shared",
            "---\nname: shared\ndescription: From project\n---\nProject body.\n",
        );

        // Project root is first => wins
        let loader = SkillLoader::with_roots(vec![
            project_tmp.path().to_path_buf(),
            home_tmp.path().to_path_buf(),
        ]);
        let registry = loader.load_all().await.unwrap();

        let skill = registry.get("shared").expect("shared not found");
        assert_eq!(skill.description, "From project", "project root should win");
        assert!(skill.content.contains("Project body."));
    }

    #[tokio::test]
    async fn to_tool_metadata_matches_expected_shape() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        write_skill(
            root,
            "greet",
            "---\nname: greet\ndescription: Greet someone\n---\nHello $ARGUMENTS!\n",
        );

        let loader = SkillLoader::with_roots(vec![root.to_path_buf()]);
        let registry = loader.load_all().await.unwrap();
        let meta = registry.to_tool_metadata();

        let entry = meta.get("greet").expect("greet not in metadata");
        let content = entry
            .get("content")
            .and_then(|v| v.as_str())
            .expect("content field missing");
        assert!(
            content.contains("Hello $ARGUMENTS!"),
            "content should include body"
        );
    }

    #[tokio::test]
    async fn disabled_skill_excluded_from_tool_metadata() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        write_skill(
            root,
            "hidden",
            "---\nname: hidden\ndescription: Hidden skill\nenabled: false\n---\nSecret.\n",
        );
        write_skill(
            root,
            "visible",
            "---\nname: visible\ndescription: Visible skill\n---\nShown.\n",
        );

        let loader = SkillLoader::with_roots(vec![root.to_path_buf()]);
        let registry = loader.load_all().await.unwrap();

        // The disabled skill still exists in the registry…
        assert!(registry.get("hidden").is_some());
        assert!(!registry.get("hidden").unwrap().enabled);

        // …but is excluded from the tool metadata map.
        let meta = registry.to_tool_metadata();
        assert!(
            meta.get("hidden").is_none(),
            "disabled skill should not appear in metadata"
        );
        assert!(meta.get("visible").is_some());
    }

    #[tokio::test]
    async fn empty_root_returns_empty_registry() {
        let tmp = TempDir::new().unwrap();
        let loader = SkillLoader::with_roots(vec![tmp.path().to_path_buf()]);
        let registry = loader.load_all().await.unwrap();
        assert!(registry.list().is_empty());
    }

    #[tokio::test]
    async fn nonexistent_root_is_silently_skipped() {
        let loader =
            SkillLoader::with_roots(vec![PathBuf::from("/nonexistent/path/that/does/not/exist")]);
        let result = loader.load_all().await;
        assert!(result.is_ok(), "nonexistent root should not error");
    }

    #[tokio::test]
    async fn crlf_frontmatter_parses_correctly() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // Simulate a SKILL.md written on Windows (CRLF line endings)
        let crlf_content = "---\r\nname: crlf-skill\r\ndescription: CRLF frontmatter test\r\n---\r\nBody line.\r\n";
        write_skill(root, "crlf-skill", crlf_content);

        let loader = SkillLoader::with_roots(vec![root.to_path_buf()]);
        let registry = loader.load_all().await.unwrap();

        let skill = registry.get("crlf-skill").expect("crlf-skill not found");
        assert_eq!(skill.name, "crlf-skill");
        assert_eq!(skill.description, "CRLF frontmatter test");
        assert!(skill.content.contains("Body line."));
    }
}

//! Persistent, file-based memory store backed by `~/.claude/projects/<slug>/memory/`.
//!
//! Memory files are Markdown with YAML frontmatter; `MEMORY.md` is the index.

use std::{path::PathBuf, time::SystemTime};

use regex::Regex;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::fs;
use tracing::warn;

// ── Error type ────────────────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum MemoryError {
    #[error("home directory not found")]
    NoHomeDir,
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("entry not found: {0}")]
    NotFound(String),
    #[error("persist error: {0}")]
    Persist(#[from] tempfile::PersistError),
    #[error("background task panicked")]
    Panic,
}

// ── Types ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MemoryType {
    User,
    Feedback,
    Project,
    Reference,
}

impl MemoryType {
    fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "user" => Self::User,
            "feedback" => Self::Feedback,
            "project" => Self::Project,
            "reference" => Self::Reference,
            _ => Self::User,
        }
    }

    fn as_str(&self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Feedback => "feedback",
            Self::Project => "project",
            Self::Reference => "reference",
        }
    }
}

#[derive(Debug, Clone)]
pub struct MemoryEntry {
    pub name: String,
    pub title: String,
    pub description: String,
    pub memory_type: MemoryType,
    pub path: PathBuf,
    pub modified: SystemTime,
}

// ── Internal frontmatter serde helper ─────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
struct Frontmatter {
    #[serde(default)]
    name: String,
    #[serde(default)]
    description: String,
    #[serde(rename = "type", default)]
    memory_type: String,
}

// ── Store ─────────────────────────────────────────────────────────────────────

pub struct MemoryStore {
    root: PathBuf,
}

impl MemoryStore {
    /// Resolve the memory root.
    ///
    /// Resolution order:
    /// 1. `$OPENHARNESSRS_DATA_DIR/memory/` if the env var is set (per-job isolation
    ///    when ohrs is spawned as a subprocess by a platform like capelle).
    /// 2. `$OPENHARNESS_DATA_DIR/memory/` (legacy env var).
    /// 3. `~/.claude/projects/<slug>/memory/` (default for local CLI use).
    pub fn new(project_slug: &str) -> Result<Self, MemoryError> {
        let root = if let Ok(data_dir) = std::env::var("OPENHARNESSRS_DATA_DIR") {
            PathBuf::from(data_dir).join("memory")
        } else if let Ok(data_dir) = std::env::var("OPENHARNESS_DATA_DIR") {
            PathBuf::from(data_dir).join("memory")
        } else {
            let home = dirs::home_dir().ok_or(MemoryError::NoHomeDir)?;
            home.join(".claude")
                .join("projects")
                .join(project_slug)
                .join("memory")
        };
        std::fs::create_dir_all(&root)?;
        Ok(Self { root })
    }

    /// Inject a custom root; used in unit tests.
    pub fn with_root(root: PathBuf) -> Self {
        Self { root }
    }

    // ── write ─────────────────────────────────────────────────────────────────

    /// Atomically persist `entry` with `body` as `<name>.md`.
    pub async fn save(&self, entry: &MemoryEntry, body: &str) -> Result<(), MemoryError> {
        fs::create_dir_all(&self.root).await?;
        let content = format_file(entry, body);
        let dest = self.root.join(format!("{}.md", entry.name));
        atomic_write(&self.root, &dest, content.as_bytes()).await?;
        Ok(())
    }

    // ── read ──────────────────────────────────────────────────────────────────

    /// Load a single entry by slug name (without `.md`).
    pub async fn load(&self, name: &str) -> Result<(MemoryEntry, String), MemoryError> {
        let path = self.root.join(format!("{}.md", name));
        let raw = fs::read_to_string(&path)
            .await
            .map_err(|_| MemoryError::NotFound(name.to_owned()))?;
        let modified = fs::metadata(&path).await?.modified()?;
        let (fm, body) = split_frontmatter(&raw);
        let entry = MemoryEntry {
            name: name.to_owned(),
            title: fm.name.clone(),
            description: fm.description.clone(),
            memory_type: MemoryType::from_str(&fm.memory_type),
            path,
            modified,
        };
        Ok((entry, body))
    }

    // ── list ──────────────────────────────────────────────────────────────────

    /// Return all entries sorted by modification time desc, then name asc for
    /// determinism when multiple files share the same mtime.
    pub async fn list(&self) -> Result<Vec<MemoryEntry>, MemoryError> {
        self.list_with_bodies()
            .await
            .map(|v| v.into_iter().map(|(e, _)| e).collect())
    }

    // ── search ────────────────────────────────────────────────────────────────

    /// Full-text regex search; returns up to 3 matching line excerpts per entry.
    ///
    /// Reuses bodies already read by list to avoid a second full-corpus read.
    pub async fn search(
        &self,
        pattern: &Regex,
    ) -> Result<Vec<(MemoryEntry, Vec<String>)>, MemoryError> {
        let all = self.list_with_bodies().await?;
        let mut results = Vec::new();

        for (entry, body) in all {
            let excerpts: Vec<String> = body
                .lines()
                .filter(|l| pattern.is_match(l))
                .take(3)
                .map(|l| l.trim().to_owned())
                .collect();
            if !excerpts.is_empty() {
                results.push((entry, excerpts));
            }
        }

        Ok(results)
    }

    // ── delete ────────────────────────────────────────────────────────────────

    /// Remove `<name>.md`; silent no-op if the file does not exist.
    pub async fn delete(&self, name: &str) -> Result<(), MemoryError> {
        let path = self.root.join(format!("{}.md", name));
        match fs::remove_file(&path).await {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    // ── index ─────────────────────────────────────────────────────────────────

    /// Regenerate `MEMORY.md` from the current entry set (sorted newest first).
    pub async fn rebuild_index(&self) -> Result<(), MemoryError> {
        let entries = self.list().await?;
        let mut lines = vec!["# Memory Index".to_owned()];
        for e in &entries {
            lines.push(format!(
                "- [{}]({}.md) — {}",
                e.title, e.name, e.description
            ));
        }
        lines.push(String::new()); // trailing newline
        let content = lines.join("\n");
        let dest = self.root.join("MEMORY.md");
        atomic_write(&self.root, &dest, content.as_bytes()).await?;
        Ok(())
    }

    // ── private helpers ───────────────────────────────────────────────────────

    /// Read dir once, returning (entry, body) pairs; bodies are cached here so
    /// `search` can consume them without a second round of file reads.
    async fn list_with_bodies(&self) -> Result<Vec<(MemoryEntry, String)>, MemoryError> {
        fs::create_dir_all(&self.root).await?;
        let mut rd = fs::read_dir(&self.root).await?;
        let mut pairs: Vec<(MemoryEntry, String)> = Vec::new();

        while let Some(de) = rd.next_entry().await? {
            let p = de.path();
            if !is_memory_file(&p) {
                continue;
            }
            let raw = match fs::read_to_string(&p).await {
                Ok(s) => s,
                Err(e) => {
                    warn!("memory: cannot read {}: {e}", p.display());
                    continue;
                }
            };
            let meta = match fs::metadata(&p).await {
                Ok(m) => m,
                Err(e) => {
                    warn!("memory: cannot stat {}: {e}", p.display());
                    continue;
                }
            };
            let modified = meta.modified()?;
            let name = p
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            let (fm, body) = split_frontmatter(&raw);
            let entry = MemoryEntry {
                name,
                title: fm.name,
                description: fm.description,
                memory_type: MemoryType::from_str(&fm.memory_type),
                path: p,
                modified,
            };
            pairs.push((entry, body));
        }

        // Primary: newest mtime first. Secondary: name ascending for determinism
        // when multiple entries share the same mtime (e.g. in fast tests).
        pairs.sort_by(|(a, _), (b, _)| {
            b.modified
                .cmp(&a.modified)
                .then_with(|| a.name.cmp(&b.name))
        });
        Ok(pairs)
    }
}

// ── file-level helpers ────────────────────────────────────────────────────────

/// True for `.md` files that are not the index file.
fn is_memory_file(p: &std::path::Path) -> bool {
    let name = p.file_name().unwrap_or_default().to_string_lossy();
    name.ends_with(".md") && name != "MEMORY.md"
}

/// Split raw file content into parsed frontmatter + body.
///
/// - Normalises CRLF before splitting so byte offsets are consistent.
/// - Gracefully handles a missing closing `---` by treating the rest as body.
/// - Warns (but does not crash) on malformed YAML.
fn split_frontmatter(raw: &str) -> (Frontmatter, String) {
    // Normalise line endings so the line-by-line scan is uniform.
    let normalised: std::borrow::Cow<str> = if raw.contains('\r') {
        raw.replace("\r\n", "\n").replace('\r', "\n").into()
    } else {
        raw.into()
    };

    let mut iter = normalised.splitn(2, '\n');
    let first = iter.next().unwrap_or("").trim();
    if first != "---" {
        return (default_fm(), normalised.into_owned());
    }

    let rest = iter.next().unwrap_or("");
    // Find the closing delimiter; treat its absence as "no frontmatter found".
    match rest.find("\n---\n").or_else(|| {
        // Closing `---` at end of string without trailing newline.
        rest.strip_suffix("\n---").map(|_| rest.len() - 4)
    }) {
        None => (default_fm(), normalised.into_owned()),
        Some(close_pos) => {
            let yaml_src = &rest[..close_pos];
            let body_start = close_pos + "\n---\n".len();
            let body = if body_start <= rest.len() {
                rest[body_start..].to_owned()
            } else {
                String::new()
            };
            let fm: Frontmatter = match serde_yaml::from_str(yaml_src) {
                Ok(f) => f,
                Err(e) => {
                    warn!("memory: bad frontmatter — {e}; using defaults");
                    return (default_fm(), body);
                }
            };
            (fm, body)
        }
    }
}

fn default_fm() -> Frontmatter {
    Frontmatter {
        name: String::new(),
        description: String::new(),
        memory_type: String::new(),
    }
}

/// Serialise an entry + body into the on-disk file format.
fn format_file(entry: &MemoryEntry, body: &str) -> String {
    format!(
        "---\nname: {}\ndescription: {}\ntype: {}\n---\n{}",
        entry.title,
        entry.description,
        entry.memory_type.as_str(),
        body,
    )
}

/// Write `data` to `dest` atomically: write to a temp file in the same
/// directory, fsync the temp file, rename into place, then fsync the
/// parent directory so the directory entry is durable too.
async fn atomic_write(
    dir: &std::path::Path,
    dest: &std::path::Path,
    data: &[u8],
) -> Result<(), MemoryError> {
    let dir = dir.to_owned();
    let dest = dest.to_owned();
    let data = data.to_owned();

    tokio::task::spawn_blocking(move || -> Result<(), MemoryError> {
        use std::io::Write;

        let mut tmp = tempfile::NamedTempFile::new_in(&dir)?;
        tmp.write_all(&data)?;
        // Flush kernel buffers to storage before rename so the data is durable.
        tmp.as_file().sync_all()?;
        tmp.persist(&dest)?;

        // Fsync the directory so the rename (directory entry) is also durable.
        let dir_file = std::fs::File::open(&dir)?;
        dir_file.sync_all()?;

        Ok(())
    })
    .await
    .map_err(|_| MemoryError::Panic)?
}

// ── unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use regex::Regex;
    use tempfile::TempDir;

    fn tmp_store() -> (TempDir, MemoryStore) {
        let dir = TempDir::new().unwrap();
        let store = MemoryStore::with_root(dir.path().to_owned());
        (dir, store)
    }

    fn sample_entry(name: &str) -> MemoryEntry {
        MemoryEntry {
            name: name.to_owned(),
            title: "Test Title".to_owned(),
            description: "A short description".to_owned(),
            memory_type: MemoryType::User,
            path: PathBuf::new(),
            modified: SystemTime::UNIX_EPOCH,
        }
    }

    // ── round-trip ────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn save_load_roundtrip() {
        let (_dir, store) = tmp_store();
        let entry = sample_entry("my_note");
        let body = "This is the memory body.\nSecond line.";
        store.save(&entry, body).await.unwrap();

        let (loaded, loaded_body) = store.load("my_note").await.unwrap();
        assert_eq!(loaded.title, entry.title);
        assert_eq!(loaded.description, entry.description);
        assert_eq!(loaded.memory_type, entry.memory_type);
        assert!(loaded_body.contains("This is the memory body."));
        assert!(loaded_body.contains("Second line."));
    }

    // ── list ordering ─────────────────────────────────────────────────────────

    #[tokio::test]
    async fn list_returns_both_entries() {
        let (_dir, store) = tmp_store();
        store.save(&sample_entry("alpha"), "body a").await.unwrap();
        store.save(&sample_entry("beta"), "body b").await.unwrap();

        let list = store.list().await.unwrap();
        assert_eq!(list.len(), 2);
        assert!(list.iter().any(|e| e.name == "alpha"));
        assert!(list.iter().any(|e| e.name == "beta"));
    }

    #[tokio::test]
    async fn list_tie_broken_by_name() {
        // Write two entries; on fast filesystems they may share the same mtime.
        // The tie-breaker (name ascending) must produce a stable order.
        let (_dir, store) = tmp_store();
        store.save(&sample_entry("zzz"), "body z").await.unwrap();
        store.save(&sample_entry("aaa"), "body a").await.unwrap();

        let list1 = store.list().await.unwrap();
        let list2 = store.list().await.unwrap();
        let names1: Vec<_> = list1.iter().map(|e| &e.name).collect();
        let names2: Vec<_> = list2.iter().map(|e| &e.name).collect();
        assert_eq!(names1, names2, "list order must be deterministic");
    }

    // ── search ────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn search_returns_excerpts() {
        let (_dir, store) = tmp_store();
        store
            .save(
                &sample_entry("note_a"),
                "Rust is great\nC++ is fast\nGo is simple",
            )
            .await
            .unwrap();
        store
            .save(&sample_entry("note_b"), "Python is easy\nRuby is fun")
            .await
            .unwrap();

        let re = Regex::new(r"(?i)rust").unwrap();
        let results = store.search(&re).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0.name, "note_a");
        assert!(results[0].1[0].contains("Rust"));
    }

    #[tokio::test]
    async fn search_no_match() {
        let (_dir, store) = tmp_store();
        store
            .save(&sample_entry("quiet"), "silence is golden")
            .await
            .unwrap();
        let re = Regex::new(r"noise").unwrap();
        let results = store.search(&re).await.unwrap();
        assert!(results.is_empty());
    }

    // ── rebuild_index ─────────────────────────────────────────────────────────

    #[tokio::test]
    async fn rebuild_index_contains_all_entries() {
        let (_dir, store) = tmp_store();
        store
            .save(&sample_entry("entry_one"), "body 1")
            .await
            .unwrap();
        store
            .save(&sample_entry("entry_two"), "body 2")
            .await
            .unwrap();
        store.rebuild_index().await.unwrap();

        let content = fs::read_to_string(store.root.join("MEMORY.md"))
            .await
            .unwrap();
        assert!(content.contains("entry_one.md"));
        assert!(content.contains("entry_two.md"));
        assert!(content.contains("# Memory Index"));
    }

    #[tokio::test]
    async fn rebuild_index_deterministic() {
        let (_dir, store) = tmp_store();
        store
            .save(&sample_entry("z_entry"), "body z")
            .await
            .unwrap();
        store
            .save(&sample_entry("a_entry"), "body a")
            .await
            .unwrap();
        store.rebuild_index().await.unwrap();
        let c1 = fs::read_to_string(store.root.join("MEMORY.md"))
            .await
            .unwrap();
        store.rebuild_index().await.unwrap();
        let c2 = fs::read_to_string(store.root.join("MEMORY.md"))
            .await
            .unwrap();
        assert_eq!(c1, c2, "rebuild_index must be idempotent");
    }

    // ── delete ────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn delete_removes_entry() {
        let (_dir, store) = tmp_store();
        store
            .save(&sample_entry("to_remove"), "body")
            .await
            .unwrap();
        assert_eq!(store.list().await.unwrap().len(), 1);
        store.delete("to_remove").await.unwrap();
        assert!(store.list().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn delete_nonexistent_is_ok() {
        let (_dir, store) = tmp_store();
        store.delete("ghost").await.unwrap();
    }

    // ── frontmatter robustness ────────────────────────────────────────────────

    #[tokio::test]
    async fn invalid_frontmatter_skipped_gracefully() {
        let (_dir, store) = tmp_store();
        let bad_path = store.root.join("bad.md");
        fs::write(&bad_path, "---\n: invalid: {{{ yaml\n---\nbody text")
            .await
            .unwrap();
        // Must not panic; entry may be included with empty fields or skipped.
        let _ = store.list().await.unwrap();
    }

    #[tokio::test]
    async fn crlf_frontmatter_parses_correctly() {
        let (_dir, store) = tmp_store();
        let content =
            "---\r\nname: CRLF Entry\r\ndescription: test\r\ntype: project\r\n---\r\nBody here.";
        fs::write(store.root.join("crlf.md"), content)
            .await
            .unwrap();
        let (entry, body) = store.load("crlf").await.unwrap();
        assert_eq!(entry.title, "CRLF Entry");
        assert_eq!(entry.memory_type, MemoryType::Project);
        assert!(body.contains("Body here."));
    }

    #[tokio::test]
    async fn no_closing_delimiter_handled() {
        let (_dir, store) = tmp_store();
        // Frontmatter without a closing `---` — should not crash.
        fs::write(
            store.root.join("nocloser.md"),
            "---\nname: Oops\ndescription: missing close\n",
        )
        .await
        .unwrap();
        let _ = store.list().await.unwrap();
    }
}

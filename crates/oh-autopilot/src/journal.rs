use crate::{error::AutopilotError, types::JournalEntry};
use std::path::{Path, PathBuf};
use tokio::{
    fs::{self, OpenOptions},
    io::AsyncWriteExt,
};

/// Append-only JSONL journal.  One file per task: `<root>/journal/<id>.jsonl`.
pub struct Journal {
    pub(crate) root: PathBuf,
}

impl Journal {
    pub async fn new(root: &Path) -> Result<Self, AutopilotError> {
        fs::create_dir_all(root.join("journal")).await?;
        Ok(Self { root: root.to_path_buf() })
    }

    fn entry_path(&self, task_id: &str) -> PathBuf {
        self.root.join("journal").join(format!("{task_id}.jsonl"))
    }

    /// Append a single entry.
    pub async fn append(&self, task_id: &str, entry: &JournalEntry) -> Result<(), AutopilotError> {
        let path = self.entry_path(task_id);
        let mut line = serde_json::to_string(entry)?;
        line.push('\n');
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .await?;
        file.write_all(line.as_bytes()).await?;
        Ok(())
    }

    /// Read all entries for a task.
    pub async fn read(&self, task_id: &str) -> Result<Vec<JournalEntry>, AutopilotError> {
        let path = self.entry_path(task_id);
        let text = match fs::read_to_string(&path).await {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
            Err(e) => return Err(e.into()),
        };
        let mut entries = Vec::new();
        for line in text.lines() {
            if line.trim().is_empty() {
                continue;
            }
            entries.push(serde_json::from_str(line)?);
        }
        Ok(entries)
    }
}

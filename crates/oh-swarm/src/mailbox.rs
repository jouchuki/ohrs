/// File-based per-agent mailbox.
///
/// Each message is an individual JSON file named `{nanos}_{uuid}.json`
/// inside `<team_root>/agents/<agent_id>/inbox/`.  Writes are atomic:
/// `tempfile::NamedTempFile` → `persist` (which uses `rename` on POSIX,
/// `MoveFileEx` on Windows).  Because rename is atomic on same-filesystem
/// moves, concurrent senders are safe without an advisory lock.
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use tokio::fs;
use uuid::Uuid;

use crate::error::SwarmError;
use crate::types::{Message, TeammateId};

/// File-based mailbox for a single agent.
#[derive(Debug, Clone)]
pub struct Mailbox {
    root: PathBuf,
}

impl Mailbox {
    /// Construct a mailbox whose inbox directory is
    /// `<team_root>/agents/<agent_id>/inbox/`.
    pub fn for_agent(team_root: &Path, agent: &TeammateId) -> Self {
        let root = team_root.join("agents").join(&agent.0).join("inbox");
        Mailbox { root }
    }

    /// Ensure the inbox directory exists (creates parent dirs if needed).
    pub async fn ensure_dir(&self) -> Result<(), SwarmError> {
        fs::create_dir_all(&self.root).await?;
        Ok(())
    }

    /// Atomically write `msg` to the inbox.
    ///
    /// The file is written to a temp file in the *same directory* as the
    /// inbox (same filesystem guarantees atomic `rename`), then renamed into
    /// place.  The filename is `{nanos}_{uuid}.json` so lexicographic sort
    /// == arrival order.
    pub async fn send(&self, msg: &Message) -> Result<(), SwarmError> {
        self.ensure_dir().await?;

        let nanos = msg
            .sent_at
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let file_name = format!("{:020}_{}.json", nanos, Uuid::new_v4());
        let final_path = self.root.join(&file_name);

        let json = serde_json::to_vec_pretty(msg)?;

        // Write via tempfile in the *same directory* so rename stays on the
        // same filesystem and is therefore atomic.
        let root = self.root.clone();
        let final_path2 = final_path.clone();
        tokio::task::spawn_blocking(move || -> Result<(), SwarmError> {
            let mut tmp = tempfile::NamedTempFile::new_in(&root)?;
            use std::io::Write;
            tmp.write_all(&json)?;
            tmp.flush()?;
            tmp.persist(&final_path2)
                .map_err(|e| SwarmError::Persist(e.to_string()))?;
            Ok(())
        })
        .await
        .map_err(|e| SwarmError::Other(e.to_string()))??;

        Ok(())
    }

    /// Pop (read + delete) the oldest message from the inbox.
    ///
    /// Returns `None` if the inbox is empty.
    pub async fn recv_one(&self) -> Result<Option<Message>, SwarmError> {
        self.ensure_dir().await?;

        let root = self.root.clone();
        tokio::task::spawn_blocking(move || -> Result<Option<Message>, SwarmError> {
            let mut entries: Vec<_> = std::fs::read_dir(&root)?
                .filter_map(|e| e.ok())
                .filter(|e| {
                    let name = e.file_name();
                    let n = name.to_string_lossy();
                    n.ends_with(".json") && !n.starts_with('.')
                })
                .collect();

            if entries.is_empty() {
                return Ok(None);
            }

            // Sort by filename → sort by arrival time
            entries.sort_by_key(|e| e.file_name());

            let entry = &entries[0];
            let path = entry.path();
            let data = std::fs::read(&path)?;
            let msg: Message = serde_json::from_slice(&data)?;
            // Delete *after* parse to avoid losing data on parse error (we
            // return the error and leave the file in place).
            std::fs::remove_file(&path)?;
            Ok(Some(msg))
        })
        .await
        .map_err(|e| SwarmError::Other(e.to_string()))?
    }

    /// Return all messages in arrival order without consuming them.
    pub async fn peek_all(&self) -> Result<Vec<Message>, SwarmError> {
        self.ensure_dir().await?;

        let root = self.root.clone();
        tokio::task::spawn_blocking(move || -> Result<Vec<Message>, SwarmError> {
            let mut entries: Vec<_> = std::fs::read_dir(&root)?
                .filter_map(|e| e.ok())
                .filter(|e| {
                    let name = e.file_name();
                    let n = name.to_string_lossy();
                    n.ends_with(".json") && !n.starts_with('.')
                })
                .collect();

            entries.sort_by_key(|e| e.file_name());

            let mut msgs = Vec::with_capacity(entries.len());
            for entry in entries {
                let data = std::fs::read(entry.path())?;
                let msg: Message = serde_json::from_slice(&data)?;
                msgs.push(msg);
            }
            Ok(msgs)
        })
        .await
        .map_err(|e| SwarmError::Other(e.to_string()))?
    }
}

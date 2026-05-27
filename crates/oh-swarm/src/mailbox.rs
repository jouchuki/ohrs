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
    ///
    /// Concurrency (SWARM-3): a naive `list → read → remove_file` lets two
    /// concurrent consumers both return the same message. Instead, each call
    /// gets a unique per-consumer `processing/<id>/` directory and *claims* the
    /// chosen file by renaming it there. `rename` is atomic, so exactly one
    /// consumer wins the claim; the loser observes `ENOENT` and retries with the
    /// next-oldest file. The claimed file is read from `processing/` and only
    /// then deleted — a parse error leaves it in `processing/` (not lost),
    /// rather than back in the contended inbox.
    pub async fn recv_one(&self) -> Result<Option<Message>, SwarmError> {
        self.ensure_dir().await?;

        let inbox = self.root.clone();
        // Sibling of the inbox: `<…>/inbox` → `<…>/processing/<consumer-id>`.
        let processing = self
            .root
            .parent()
            .map(|p| p.join("processing"))
            .unwrap_or_else(|| self.root.join("processing"))
            .join(Uuid::new_v4().to_string());

        tokio::task::spawn_blocking(move || -> Result<Option<Message>, SwarmError> {
            std::fs::create_dir_all(&processing)?;

            loop {
                let mut entries: Vec<_> = std::fs::read_dir(&inbox)?
                    .filter_map(|e| e.ok())
                    .filter(|e| {
                        let name = e.file_name();
                        let n = name.to_string_lossy();
                        n.ends_with(".json") && !n.starts_with('.')
                    })
                    .collect();

                if entries.is_empty() {
                    // Nothing left to claim. Clean up our (empty) processing dir.
                    let _ = std::fs::remove_dir(&processing);
                    return Ok(None);
                }

                // Sort by filename → oldest-first arrival order.
                entries.sort_by_key(|e| e.file_name());

                // Try to claim the oldest file by atomic rename. If the rename
                // fails because another consumer already took it, fall through
                // to the next candidate; re-list if we exhaust this snapshot.
                let mut claimed: Option<PathBuf> = None;
                for entry in &entries {
                    let src = entry.path();
                    let dst = processing.join(entry.file_name());
                    match std::fs::rename(&src, &dst) {
                        Ok(()) => {
                            claimed = Some(dst);
                            break;
                        }
                        // Lost the race for this file — try the next one.
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                        Err(e) => return Err(SwarmError::Io(e)),
                    }
                }

                let path = match claimed {
                    Some(p) => p,
                    // Every candidate was claimed by someone else between the
                    // listing and our rename; re-snapshot and try again.
                    None => continue,
                };

                let data = std::fs::read(&path)?;
                let msg: Message = serde_json::from_slice(&data)?;
                // Read succeeded: remove the claimed copy. On parse error we
                // return early and leave the file in `processing/` for inspection
                // (it is out of the inbox, so it won't be re-delivered).
                std::fs::remove_file(&path)?;
                let _ = std::fs::remove_dir(&processing);
                return Ok(Some(msg));
            }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::sync::Arc;

    use crate::types::{Message, MessageKind};

    fn mailbox() -> (tempfile::TempDir, Mailbox) {
        let dir = tempfile::tempdir().unwrap();
        let mb = Mailbox::for_agent(dir.path(), &TeammateId::new("worker"));
        (dir, mb)
    }

    fn msg(n: usize) -> Message {
        Message::new(
            TeammateId::new("sender"),
            TeammateId::new("worker"),
            MessageKind::Status,
            serde_json::json!({ "n": n }),
        )
    }

    #[tokio::test]
    async fn send_then_recv_one_returns_message() {
        let (_dir, mb) = mailbox();
        mb.send(&msg(1)).await.unwrap();

        let got = mb.recv_one().await.unwrap().expect("a message");
        assert_eq!(got.body["n"], 1);
        // Inbox is now empty.
        assert!(mb.recv_one().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn recv_one_empty_inbox_returns_none() {
        let (_dir, mb) = mailbox();
        assert!(mb.recv_one().await.unwrap().is_none());
    }

    /// SWARM-3 regression: with many messages and many concurrent consumers,
    /// every message must be delivered to EXACTLY one consumer — no message
    /// double-delivered, none lost.
    #[tokio::test]
    async fn concurrent_recv_one_no_double_delivery() {
        let (_dir, mb) = mailbox();
        let mb = Arc::new(mb);

        const MESSAGES: usize = 64;
        const CONSUMERS: usize = 8;

        for n in 0..MESSAGES {
            mb.send(&msg(n)).await.unwrap();
            // Distinct nanos timestamps keep filenames unique; the sender also
            // appends a uuid, so collisions are not relied upon.
        }

        let mut handles = Vec::with_capacity(CONSUMERS);
        for _ in 0..CONSUMERS {
            let mb = mb.clone();
            handles.push(tokio::spawn(async move {
                let mut received = Vec::new();
                // Drain until the inbox is empty.
                while let Some(m) = mb.recv_one().await.unwrap() {
                    received.push(m.body["n"].as_u64().unwrap() as usize);
                }
                received
            }));
        }

        let mut all = Vec::new();
        for h in handles {
            all.extend(h.await.unwrap());
        }

        let unique: HashSet<usize> = all.iter().copied().collect();
        assert_eq!(
            all.len(),
            MESSAGES,
            "expected {MESSAGES} total deliveries, got {} (double-delivery or loss)",
            all.len()
        );
        assert_eq!(
            unique.len(),
            MESSAGES,
            "a message was delivered more than once"
        );
        for n in 0..MESSAGES {
            assert!(unique.contains(&n), "message {n} was never delivered");
        }
    }
}

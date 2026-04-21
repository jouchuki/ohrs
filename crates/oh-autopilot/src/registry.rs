use crate::{AutopilotError, TaskCard, TaskStatus};
use std::path::{Path, PathBuf};
use tokio::fs;

/// Filesystem-backed task card store.  One JSON file per card: `<root>/cards/<id>.json`.
pub struct Registry {
    pub(crate) root: PathBuf,
}

impl Registry {
    pub async fn new(root: &Path) -> Result<Self, AutopilotError> {
        let cards_dir = root.join("cards");
        fs::create_dir_all(&cards_dir).await?;
        Ok(Self { root: root.to_path_buf() })
    }

    fn card_path(&self, id: &str) -> PathBuf {
        self.root.join("cards").join(format!("{id}.json"))
    }

    /// Persist a card atomically (tempfile → rename).
    pub async fn save(&self, card: &TaskCard) -> Result<(), AutopilotError> {
        let path = self.card_path(&card.id);
        let bytes = serde_json::to_vec_pretty(card)?;
        atomic_write(&path, &bytes).await
    }

    /// Load a single card by id.
    pub async fn load(&self, id: &str) -> Result<TaskCard, AutopilotError> {
        let path = self.card_path(id);
        let bytes = fs::read(&path)
            .await
            .map_err(|_| AutopilotError::NotFound(id.to_string()))?;
        Ok(serde_json::from_slice(&bytes)?)
    }

    /// List all cards, optionally filtered by status, sorted by `created_at` (FIFO).
    pub async fn list(&self, filter: Option<TaskStatus>) -> Result<Vec<TaskCard>, AutopilotError> {
        let cards_dir = self.root.join("cards");
        let mut entries = fs::read_dir(&cards_dir).await?;
        let mut cards: Vec<TaskCard> = Vec::new();
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let bytes = fs::read(&path).await?;
            let card: TaskCard = serde_json::from_slice(&bytes)?;
            if filter.map_or(true, |s| card.status == s) {
                cards.push(card);
            }
        }
        // FIFO: earliest created_at first
        cards.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        Ok(cards)
    }

    /// Find a card whose `source_ref` matches (returns the first hit).
    pub async fn find_by_source_ref(&self, source_ref: &str) -> Result<Option<TaskCard>, AutopilotError> {
        let all = self.list(None).await?;
        Ok(all.into_iter().find(|c| c.source_ref.as_deref() == Some(source_ref)))
    }

    /// Find a card with the given fingerprint.
    pub async fn find_by_fingerprint(&self, fp: &str) -> Result<Option<TaskCard>, AutopilotError> {
        let all = self.list(None).await?;
        Ok(all.into_iter().find(|c| c.fingerprint == fp))
    }
}

/// Write bytes atomically: write to a sibling temp file then rename.
pub(crate) async fn atomic_write(dest: &Path, bytes: &[u8]) -> Result<(), AutopilotError> {
    let parent = dest.parent().expect("dest must have a parent");
    let tmp = parent.join(format!(".tmp.{}", uuid_hex()));
    fs::write(&tmp, bytes).await?;
    fs::rename(&tmp, dest).await?;
    Ok(())
}

fn uuid_hex() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let ns = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();
    format!("{:x}", ns)
}

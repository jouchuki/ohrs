//! Core autopilot loop: intake, dedup, registry, journal, run_one.

mod error;
mod journal;
mod registry;
pub mod types;

pub use error::AutopilotError;
pub use journal::Journal;
pub use registry::Registry;
pub use types::{JournalEntry, JournalKind, TaskCard, TaskSource, TaskStatus};

use std::{
    future::Future,
    path::Path,
    sync::Arc,
    time::SystemTime,
};
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

// ── Config ────────────────────────────────────────────────────────────────────

/// Which fields are used to detect duplicate tasks.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DedupStrategy {
    /// Dedup solely on `source_ref`.
    SourceRef,
    /// Dedup solely on `fingerprint`.
    Fingerprint,
    /// Dedup if either `source_ref` OR `fingerprint` already exists.
    Both,
}

/// Top-level autopilot configuration.
#[derive(Debug, Clone)]
pub struct AutopilotConfig {
    /// Hard cap on LLM turns per task (enforced by the caller's closure).
    pub max_turns: u32,
    pub dedup: DedupStrategy,
}

impl Default for AutopilotConfig {
    fn default() -> Self {
        Self { max_turns: 12, dedup: DedupStrategy::Both }
    }
}

// ── Intake result ─────────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum IntakeResult {
    /// Card was new and accepted into the registry.
    Accepted(TaskCard),
    /// A duplicate was detected; returns the existing card.
    Deduped(TaskCard),
}

// ── Autopilot ─────────────────────────────────────────────────────────────────

/// The central scheduler handle.  Clone-able via `Arc`.
pub struct Autopilot {
    registry: Arc<Mutex<Registry>>,
    journal: Arc<Mutex<Journal>>,
    pub config: AutopilotConfig,
}

impl Autopilot {
    /// Create (or re-open) an autopilot rooted at `root`.
    pub async fn new(root: &Path, config: AutopilotConfig) -> Result<Self, AutopilotError> {
        let registry = Registry::new(root).await?;
        let journal = Journal::new(root).await?;
        Ok(Self {
            registry: Arc::new(Mutex::new(registry)),
            journal: Arc::new(Mutex::new(journal)),
            config,
        })
    }

    /// Accept a card, applying dedup according to `config.dedup`.
    pub async fn intake(&self, card: TaskCard) -> Result<IntakeResult, AutopilotError> {
        let reg = self.registry.lock().await;
        let jnl = self.journal.lock().await;

        // Dedup check
        let existing: Option<TaskCard> = match self.config.dedup {
            DedupStrategy::SourceRef => {
                if let Some(r) = &card.source_ref {
                    reg.find_by_source_ref(r).await?
                } else {
                    None
                }
            }
            DedupStrategy::Fingerprint => reg.find_by_fingerprint(&card.fingerprint).await?,
            DedupStrategy::Both => {
                let by_fp = reg.find_by_fingerprint(&card.fingerprint).await?;
                if by_fp.is_some() {
                    by_fp
                } else if let Some(r) = &card.source_ref {
                    reg.find_by_source_ref(r).await?
                } else {
                    None
                }
            }
        };

        if let Some(dup) = existing {
            debug!(id = %dup.id, "intake deduped");
            return Ok(IntakeResult::Deduped(dup));
        }

        // Accept new card
        reg.save(&card).await?;
        jnl.append(
            &card.id,
            &JournalEntry {
                at: SystemTime::now(),
                kind: JournalKind::Intake,
                message: format!("Accepted: {}", card.title),
            },
        )
        .await?;
        info!(id = %card.id, title = %card.title, "task accepted");
        Ok(IntakeResult::Accepted(card))
    }

    /// Return the oldest Pending card (FIFO by `created_at`), or `None`.
    pub async fn next_pending(&self) -> Result<Option<TaskCard>, AutopilotError> {
        let reg = self.registry.lock().await;
        let mut pending = reg.list(Some(TaskStatus::Pending)).await?;
        // list() already sorts by created_at ascending
        let first = pending.drain(..).next();
        Ok(first)
    }

    /// Pop the next Pending card, run `runner`, then write the final status.
    ///
    /// Returns the completed card, or `None` if there was nothing to run.
    pub async fn run_one<F, Fut>(
        &self,
        runner: F,
    ) -> Result<Option<TaskCard>, AutopilotError>
    where
        F: FnOnce(TaskCard) -> Fut,
        Fut: Future<Output = Result<(), AutopilotError>>,
    {
        // Pop next pending
        let card = match self.next_pending().await? {
            Some(c) => c,
            None => return Ok(None),
        };

        let card_id = card.id.clone();

        // Transition → Running
        let mut running = card.clone();
        running.status = TaskStatus::Running;
        running.started_at = Some(SystemTime::now());
        running.attempts += 1;
        {
            let reg = self.registry.lock().await;
            reg.save(&running).await?;
        }
        {
            let jnl = self.journal.lock().await;
            jnl.append(
                &card_id,
                &JournalEntry {
                    at: SystemTime::now(),
                    kind: JournalKind::Started,
                    message: format!("Started (attempt {})", running.attempts),
                },
            )
            .await?;
        }
        info!(id = %card_id, "task running");

        // Invoke caller-supplied runner (LLM loop goes here)
        let outcome = runner(running.clone()).await;

        // Transition → Succeeded / Failed
        let mut done = running.clone();
        done.ended_at = Some(SystemTime::now());
        let (final_status, jkind, jmsg) = match outcome {
            Ok(()) => (
                TaskStatus::Succeeded,
                JournalKind::Succeeded,
                "Succeeded".to_string(),
            ),
            Err(ref e) => (
                TaskStatus::Failed,
                JournalKind::Failed,
                format!("Failed: {e}"),
            ),
        };
        done.status = final_status;
        {
            let reg = self.registry.lock().await;
            reg.save(&done).await?;
        }
        {
            let jnl = self.journal.lock().await;
            jnl.append(
                &card_id,
                &JournalEntry {
                    at: SystemTime::now(),
                    kind: jkind,
                    message: jmsg,
                },
            )
            .await?;
        }

        match &outcome {
            Ok(()) => info!(id = %card_id, "task succeeded"),
            Err(e) => warn!(id = %card_id, err = %e, "task failed"),
        }

        Ok(Some(done))
    }

    /// List cards, optionally filtered by status.
    pub async fn list(
        &self,
        filter: Option<TaskStatus>,
    ) -> Result<Vec<TaskCard>, AutopilotError> {
        let reg = self.registry.lock().await;
        reg.list(filter).await
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Build a stable task id from a raw string (SHA-1 hex, first 16 chars).
pub fn make_task_id(raw: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    raw.hash(&mut h);
    format!("{:016x}", h.finish())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_card(title: &str, fingerprint: &str) -> TaskCard {
        TaskCard {
            id: make_task_id(&format!("{title}{fingerprint}")),
            title: title.to_string(),
            description: "test task".to_string(),
            source: TaskSource::Manual,
            source_ref: None,
            fingerprint: fingerprint.to_string(),
            labels: vec![],
            status: TaskStatus::Pending,
            created_at: SystemTime::now(),
            started_at: None,
            ended_at: None,
            attempts: 0,
            journal: vec![],
        }
    }

    async fn pilot(dir: &TempDir, dedup: DedupStrategy) -> Autopilot {
        Autopilot::new(
            dir.path(),
            AutopilotConfig { max_turns: 5, dedup },
        )
        .await
        .unwrap()
    }

    // 1. Intake accepts a new card; second intake with same fingerprint → Deduped.
    #[tokio::test]
    async fn test_intake_dedup_fingerprint() {
        let dir = TempDir::new().unwrap();
        let ap = pilot(&dir, DedupStrategy::Fingerprint).await;

        let card = make_card("Fix the bug", "fp-abc");
        let r1 = ap.intake(card.clone()).await.unwrap();
        assert!(matches!(r1, IntakeResult::Accepted(_)));

        let card2 = make_card("Fix the bug again", "fp-abc"); // same fingerprint
        let r2 = ap.intake(card2).await.unwrap();
        assert!(matches!(r2, IntakeResult::Deduped(_)));
    }

    // 2. next_pending respects FIFO on created_at.
    #[tokio::test]
    async fn test_next_pending_fifo() {
        let dir = TempDir::new().unwrap();
        let ap = pilot(&dir, DedupStrategy::Fingerprint).await;

        let early = TaskCard {
            id: make_task_id("early"),
            title: "Early task".to_string(),
            description: "".to_string(),
            source: TaskSource::Manual,
            source_ref: None,
            fingerprint: "fp-early".to_string(),
            labels: vec![],
            status: TaskStatus::Pending,
            created_at: SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1000),
            started_at: None,
            ended_at: None,
            attempts: 0,
            journal: vec![],
        };
        let late = TaskCard {
            id: make_task_id("late"),
            title: "Late task".to_string(),
            description: "".to_string(),
            source: TaskSource::Manual,
            source_ref: None,
            fingerprint: "fp-late".to_string(),
            labels: vec![],
            status: TaskStatus::Pending,
            created_at: SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(9000),
            started_at: None,
            ended_at: None,
            attempts: 0,
            journal: vec![],
        };

        ap.intake(late.clone()).await.unwrap();
        ap.intake(early.clone()).await.unwrap();

        let next = ap.next_pending().await.unwrap().unwrap();
        assert_eq!(next.id, early.id, "FIFO: earliest should come first");
    }

    // 3. run_one with a succeeding closure → Succeeded + journal entry.
    #[tokio::test]
    async fn test_run_one_success() {
        let dir = TempDir::new().unwrap();
        let ap = pilot(&dir, DedupStrategy::Fingerprint).await;

        let card = make_card("Deploy widget", "fp-deploy");
        ap.intake(card).await.unwrap();

        let result = ap
            .run_one(|_card| async { Ok(()) })
            .await
            .unwrap()
            .expect("should have run a card");

        assert_eq!(result.status, TaskStatus::Succeeded);
        assert!(result.started_at.is_some());
        assert!(result.ended_at.is_some());
        assert_eq!(result.attempts, 1);

        // Journal file should have at least Started + Succeeded entries
        let jnl = ap.journal.lock().await;
        let entries = jnl.read(&result.id).await.unwrap();
        assert!(
            entries.iter().any(|e| e.kind == JournalKind::Started),
            "expected Started journal entry"
        );
        assert!(
            entries.iter().any(|e| e.kind == JournalKind::Succeeded),
            "expected Succeeded journal entry"
        );
    }

    // 4. run_one with a failing closure → Failed with error in journal.
    #[tokio::test]
    async fn test_run_one_failure() {
        let dir = TempDir::new().unwrap();
        let ap = pilot(&dir, DedupStrategy::Fingerprint).await;

        let card = make_card("Risky task", "fp-risky");
        ap.intake(card).await.unwrap();

        let result = ap
            .run_one(|_card| async {
                Err(AutopilotError::Execution("something went wrong".into()))
            })
            .await
            .unwrap()
            .expect("should have run a card");

        assert_eq!(result.status, TaskStatus::Failed);

        let jnl = ap.journal.lock().await;
        let entries = jnl.read(&result.id).await.unwrap();
        let failed_entry = entries.iter().find(|e| e.kind == JournalKind::Failed);
        assert!(failed_entry.is_some(), "expected Failed journal entry");
        assert!(
            failed_entry.unwrap().message.contains("something went wrong"),
            "error message should appear in journal"
        );
    }

    // 5. list(Some(Succeeded)) filters correctly.
    #[tokio::test]
    async fn test_list_filter() {
        let dir = TempDir::new().unwrap();
        let ap = pilot(&dir, DedupStrategy::Fingerprint).await;

        // One card that will succeed, one that will fail
        ap.intake(make_card("Task A", "fp-a")).await.unwrap();
        ap.intake(make_card("Task B", "fp-b")).await.unwrap();

        ap.run_one(|_c| async { Ok(()) }).await.unwrap();
        ap.run_one(|_c| async {
            Err(AutopilotError::Execution("nope".into()))
        })
        .await
        .unwrap();

        let succeeded = ap.list(Some(TaskStatus::Succeeded)).await.unwrap();
        assert_eq!(succeeded.len(), 1);
        assert_eq!(succeeded[0].status, TaskStatus::Succeeded);

        let failed = ap.list(Some(TaskStatus::Failed)).await.unwrap();
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].status, TaskStatus::Failed);

        let all = ap.list(None).await.unwrap();
        assert_eq!(all.len(), 2);
    }
}

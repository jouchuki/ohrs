use serde::{Deserialize, Serialize};
use std::time::SystemTime;

/// Where a task originated.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TaskSource {
    Manual,
    GitHub { issue: u64 },
    Slack { channel: String, ts: String },
    Cron { job: String },
}

/// Lifecycle states for a task card.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Pending,
    Running,
    Succeeded,
    Failed,
    Skipped,
}

/// Semantic category of a journal event.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum JournalKind {
    Intake,
    Started,
    Output,
    Error,
    Succeeded,
    Failed,
}

/// One append-only journal line.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JournalEntry {
    #[serde(with = "system_time_serde")]
    pub at: SystemTime,
    pub kind: JournalKind,
    pub message: String,
}

/// A normalized work item.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskCard {
    /// Stable SHA-1 hash of (source, source_ref, fingerprint).
    pub id: String,
    pub title: String,
    pub description: String,
    pub source: TaskSource,
    pub source_ref: Option<String>,
    /// Content-based dedup key supplied by the caller.
    pub fingerprint: String,
    pub labels: Vec<String>,
    pub status: TaskStatus,
    #[serde(with = "system_time_serde")]
    pub created_at: SystemTime,
    #[serde(with = "option_system_time_serde")]
    pub started_at: Option<SystemTime>,
    #[serde(with = "option_system_time_serde")]
    pub ended_at: Option<SystemTime>,
    pub attempts: u32,
    pub journal: Vec<JournalEntry>,
}

// ── SystemTime serde helpers ──────────────────────────────────────────────────

pub(crate) mod system_time_serde {
    use serde::{Deserializer, Serializer};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    pub fn serialize<S: Serializer>(t: &SystemTime, s: S) -> Result<S::Ok, S::Error> {
        let secs = t
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::ZERO)
            .as_secs_f64();
        s.serialize_f64(secs)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<SystemTime, D::Error> {
        let secs: f64 = serde::Deserialize::deserialize(d)?;
        Ok(UNIX_EPOCH + Duration::from_secs_f64(secs))
    }
}

pub(crate) mod option_system_time_serde {
    use serde::{Deserializer, Serializer};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    pub fn serialize<S: Serializer>(t: &Option<SystemTime>, s: S) -> Result<S::Ok, S::Error> {
        match t {
            Some(inner) => {
                let secs = inner
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or(Duration::ZERO)
                    .as_secs_f64();
                s.serialize_some(&secs)
            }
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        d: D,
    ) -> Result<Option<SystemTime>, D::Error> {
        let opt: Option<f64> = serde::Deserialize::deserialize(d)?;
        Ok(opt.map(|secs| UNIX_EPOCH + Duration::from_secs_f64(secs)))
    }
}

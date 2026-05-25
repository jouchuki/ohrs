/// Core identity and message types for the oh-swarm crate.
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Identity wrappers
// ---------------------------------------------------------------------------

/// Opaque identifier for a single teammate agent.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TeammateId(pub String);

impl TeammateId {
    pub fn new(s: impl Into<String>) -> Self {
        TeammateId(s.into())
    }
}

impl std::fmt::Display for TeammateId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Opaque identifier for a swarm team.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TeamId(pub String);

impl TeamId {
    pub fn new(s: impl Into<String>) -> Self {
        TeamId(s.into())
    }
}

impl std::fmt::Display for TeamId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

// ---------------------------------------------------------------------------
// Message types
// ---------------------------------------------------------------------------

/// Discriminant for swarm messages.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MessageKind {
    UserTurn,
    AgentReply,
    Status,
    Stop,
    /// Posted by a finished subagent: carries its final result text and a
    /// `stats` JSON blob (e.g. turn count, ok flag). The parent reads this from
    /// the recipient's mailbox to learn the outcome of an in-process spawn.
    IdleNotification,
    Custom(String),
}

/// A message exchanged between teammates via file-based mailboxes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub from: TeammateId,
    pub to: TeammateId,
    pub kind: MessageKind,
    pub body: serde_json::Value,
    /// Wall-clock time the message was created.
    #[serde(with = "system_time_serde")]
    pub sent_at: SystemTime,
}

impl Message {
    pub fn new(
        from: TeammateId,
        to: TeammateId,
        kind: MessageKind,
        body: serde_json::Value,
    ) -> Self {
        Message {
            from,
            to,
            kind,
            body,
            sent_at: SystemTime::now(),
        }
    }
}

// ---------------------------------------------------------------------------
// SystemTime serde helper (nanos since UNIX_EPOCH as u128)
// ---------------------------------------------------------------------------

mod system_time_serde {
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S>(t: &SystemTime, s: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let nanos = t
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::ZERO)
            .as_nanos();
        s.serialize_u128(nanos)
    }

    pub fn deserialize<'de, D>(d: D) -> Result<SystemTime, D::Error>
    where
        D: Deserializer<'de>,
    {
        let nanos = u128::deserialize(d)?;
        Ok(UNIX_EPOCH + Duration::from_nanos(nanos as u64))
    }
}

// ---------------------------------------------------------------------------
// Spawn configuration
// ---------------------------------------------------------------------------

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use crate::mailbox::Mailbox;

/// Async body signature for an in-process teammate task.
pub type TaskBody = Box<
    dyn Fn(CancellationToken, Mailbox) -> Pin<Box<dyn Future<Output = ()> + Send>>
        + Send
        + Sync,
>;

/// Configuration for spawning a teammate via a [`Backend`](crate::backend::Backend).
pub struct TeammateConfig {
    /// Optional human-readable display name.
    pub display_name: Option<String>,
    /// The async closure to run as this teammate. `None` is allowed for
    /// backends that don't use an in-process body (e.g. future subprocess backend).
    pub body: Option<Arc<TaskBody>>,
}

impl std::fmt::Debug for TeammateConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TeammateConfig")
            .field("display_name", &self.display_name)
            .field("body", &self.body.as_ref().map(|_| "<async fn>"))
            .finish()
    }
}

impl TeammateConfig {
    /// Convenience constructor for configs that carry an in-process body.
    pub fn with_body<F, Fut>(display_name: impl Into<String>, f: F) -> Self
    where
        F: Fn(CancellationToken, Mailbox) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        TeammateConfig {
            display_name: Some(display_name.into()),
            body: Some(Arc::new(Box::new(move |tok, mb| {
                Box::pin(f(tok, mb))
            }))),
        }
    }

    /// Config with no body (for deferred backends).
    pub fn headless(display_name: impl Into<String>) -> Self {
        TeammateConfig {
            display_name: Some(display_name.into()),
            body: None,
        }
    }
}

// ---------------------------------------------------------------------------
// TeammateHandle
// ---------------------------------------------------------------------------

/// Handle returned when a teammate is successfully spawned.
#[derive(Debug, Clone)]
pub struct TeammateHandle {
    pub id: TeammateId,
    /// Token that, when cancelled, signals this teammate to stop.
    pub cancel: CancellationToken,
}

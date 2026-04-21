//! Core message types shared across all channel adapters.

use std::time::SystemTime;

use serde::{Deserialize, Serialize};

/// Identifies a specific channel on a specific platform.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ChannelId {
    /// The platform/kind this channel belongs to.
    pub kind: ChannelKind,
    /// Platform-specific channel identifier (e.g. Slack channel ID `C01234`).
    pub channel: String,
    /// Thread timestamp for Slack threads; `None` means the top-level channel.
    pub thread_ts: Option<String>,
}

impl ChannelId {
    /// Convenience constructor.
    pub fn new(kind: ChannelKind, channel: impl Into<String>) -> Self {
        Self {
            kind,
            channel: channel.into(),
            thread_ts: None,
        }
    }

    /// Builder-style setter for thread_ts.
    pub fn with_thread(mut self, ts: impl Into<String>) -> Self {
        self.thread_ts = Some(ts.into());
        self
    }
}

/// Supported messaging platforms.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelKind {
    Slack,
    Discord,
    Matrix,
    Telegram,
    /// Escape-hatch for future/custom platforms.
    Other(String),
}

impl std::fmt::Display for ChannelKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ChannelKind::Slack => write!(f, "slack"),
            ChannelKind::Discord => write!(f, "discord"),
            ChannelKind::Matrix => write!(f, "matrix"),
            ChannelKind::Telegram => write!(f, "telegram"),
            ChannelKind::Other(s) => write!(f, "{s}"),
        }
    }
}

/// A message that arrived from a channel (inbound → agent).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InboundMessage {
    /// Where this message came from.
    pub channel: ChannelId,
    /// Sender's platform user ID.
    pub user_id: String,
    /// Sender's display name, if available.
    pub user_name: Option<String>,
    /// Plain-text message content.
    pub text: String,
    /// When the message was received.
    #[serde(with = "system_time_serde")]
    pub at: SystemTime,
    /// The raw platform payload (for adapter-specific logic).
    pub raw: serde_json::Value,
}

/// A message that should be delivered to a channel (outbound ← agent).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutboundMessage {
    /// Which channel to deliver to.
    pub channel: ChannelId,
    /// Plain-text fallback content.
    pub text: String,
    /// Optional platform-specific rich content (Slack Block Kit JSON, etc.).
    pub blocks: Option<serde_json::Value>,
}

// ---------------------------------------------------------------------------
// Minimal serde helper for SystemTime (seconds since UNIX epoch)
// ---------------------------------------------------------------------------
mod system_time_serde {
    use serde::{Deserialize, Deserializer, Serializer};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    pub fn serialize<S: Serializer>(t: &SystemTime, ser: S) -> Result<S::Ok, S::Error> {
        let secs = t
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::ZERO)
            .as_secs();
        ser.serialize_u64(secs)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(de: D) -> Result<SystemTime, D::Error> {
        let secs = u64::deserialize(de)?;
        Ok(UNIX_EPOCH + Duration::from_secs(secs))
    }
}

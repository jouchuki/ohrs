//! Slack channel adapter — Events API webhook + `chat.postMessage` outbound pump.
//!
//! # Architecture
//!
//! ```text
//! Internet ──POST /slack/events──► axum server ──► InboundMessage ──► MessageBus
//!                                                                          │
//!                                                                          ▼
//!                               chat.postMessage ◄── OutboundMessage ◄── agent
//! ```
//!
//! `SlackAdapter::start` spawns two tasks:
//!  * An axum HTTP server that receives and verifies Slack Events API webhooks.
//!  * An outbound pump that subscribes to the bus and calls `chat.postMessage`.

pub mod client;
pub mod events;

use std::net::SocketAddr;
use std::sync::Arc;

use async_trait::async_trait;
use futures::StreamExt;
use tokio::sync::oneshot;
use tracing::{error, info};

use crate::{
    adapter::{Adapter, AdapterHandle, ChannelError},
    bus::MessageBus,
    types::ChannelKind,
};

use self::{
    client::{ReqwestSlackClient, SlackApiClient},
    events::{events_router, EventsState},
};

// ---------------------------------------------------------------------------
// SlackAdapter
// ---------------------------------------------------------------------------

/// Slack channel adapter.
///
/// Listens for events via the Slack Events API (HTTP webhook) and sends
/// messages via `chat.postMessage`.
pub struct SlackAdapter {
    /// Slack bot token (`xoxb-…`)
    pub bot_token: String,
    /// Slack signing secret (used to verify webhook signatures).
    pub signing_secret: String,
    /// Address to bind the webhook server to.  Defaults to `0.0.0.0:3000`.
    pub bind: SocketAddr,
}

impl SlackAdapter {
    /// Create a new `SlackAdapter` bound to `0.0.0.0:3000`.
    pub fn new(bot_token: impl Into<String>, signing_secret: impl Into<String>) -> Self {
        Self {
            bot_token: bot_token.into(),
            signing_secret: signing_secret.into(),
            bind: "0.0.0.0:3000".parse().unwrap(),
        }
    }

    /// Override the bind address.
    pub fn with_bind(mut self, addr: SocketAddr) -> Self {
        self.bind = addr;
        self
    }
}

#[async_trait]
impl Adapter for SlackAdapter {
    fn name(&self) -> &str {
        "slack"
    }

    async fn start(&self, bus: Arc<dyn MessageBus>) -> Result<AdapterHandle, ChannelError> {
        let (kill_tx, kill_rx) = oneshot::channel::<()>();

        // --- Inbound: axum webhook server ---
        let state = EventsState {
            signing_secret: self.signing_secret.clone(),
            bus: Arc::clone(&bus),
        };
        let router = events_router(state);
        let bind = self.bind;

        // --- Outbound: pump from bus to Slack ---
        let api_client: Arc<dyn SlackApiClient> =
            Arc::new(ReqwestSlackClient::new(self.bot_token.clone()));
        let mut outbound_stream = bus.subscribe_outbound().await;

        tokio::spawn(async move {
            info!("slack: starting webhook server on {bind}");
            let listener = match tokio::net::TcpListener::bind(bind).await {
                Ok(l) => l,
                Err(e) => {
                    error!("slack: failed to bind {bind}: {e}");
                    return;
                }
            };
            // axum::serve runs until the kill signal.
            tokio::select! {
                result = axum::serve(listener, router) => {
                    if let Err(e) = result {
                        error!("slack: axum server error: {e}");
                    }
                }
                _ = kill_rx => {
                    info!("slack: webhook server stopped (kill signal)");
                }
            }
        });

        tokio::spawn(async move {
            while let Some(msg) = outbound_stream.next().await {
                // Only handle Slack-destined messages.
                if msg.channel.kind != ChannelKind::Slack {
                    continue;
                }
                if let Err(e) = api_client.post_message(&msg).await {
                    error!("slack: failed to send outbound message: {e}");
                }
            }
        });

        Ok(AdapterHandle { kill: kill_tx })
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{bus::InMemoryBus, types::ChannelId};

    #[tokio::test]
    async fn adapter_name_is_slack() {
        let a = SlackAdapter::new("token", "secret");
        assert_eq!(a.name(), "slack");
    }

    #[tokio::test]
    async fn outbound_pump_delivers_slack_messages() {
        use crate::types::{ChannelKind, OutboundMessage};
        use futures::StreamExt;

        let bus: Arc<dyn MessageBus> = Arc::new(InMemoryBus::new());
        let mut rx = bus.subscribe_outbound().await;

        let msg = OutboundMessage {
            channel: ChannelId::new(ChannelKind::Slack, "C999"),
            text: "pump test".into(),
            blocks: None,
        };
        bus.publish_outbound(msg).await.unwrap();

        let received = rx.next().await.expect("should receive");
        assert_eq!(received.text, "pump test");
        assert_eq!(received.channel.kind, ChannelKind::Slack);
    }
}

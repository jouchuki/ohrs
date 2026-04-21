//! `MessageBus` trait and `InMemoryBus` implementation.
//!
//! The bus decouples channel adapters from the agent core.  Inbound messages
//! flow from adapters → bus → agent; outbound messages flow from agent → bus →
//! adapters.
//!
//! `InMemoryBus` uses `tokio::sync::broadcast` so that **multiple subscribers**
//! on the same direction all receive every message (fan-out).  The channel
//! capacity is 256 items.

use async_trait::async_trait;
use futures::stream::{Stream, StreamExt};
use thiserror::Error;
use tokio::sync::broadcast;
use tokio_stream::wrappers::BroadcastStream;

use crate::types::{InboundMessage, OutboundMessage};

const BUS_CAPACITY: usize = 256;

/// Errors that can be returned by `MessageBus` operations.
#[derive(Debug, Error)]
pub enum BusError {
    #[error("bus is closed")]
    Closed,
    #[error("bus send failed: channel full or no receivers")]
    SendFailed,
}

/// The core messaging abstraction: a bidirectional broadcast bus.
///
/// Implementors must be `Send + Sync` so they can be wrapped in `Arc<dyn MessageBus>`
/// and shared across tasks.
#[async_trait]
pub trait MessageBus: Send + Sync {
    /// Broadcast an inbound message to all current inbound subscribers.
    async fn publish_inbound(&self, msg: InboundMessage) -> Result<(), BusError>;

    /// Subscribe to the inbound stream.  Each call returns an independent
    /// stream that starts from the *next* published message.
    async fn subscribe_inbound(
        &self,
    ) -> Box<dyn Stream<Item = InboundMessage> + Send + Unpin>;

    /// Broadcast an outbound message to all current outbound subscribers.
    async fn publish_outbound(&self, msg: OutboundMessage) -> Result<(), BusError>;

    /// Subscribe to the outbound stream.  Same fan-out semantics as inbound.
    async fn subscribe_outbound(
        &self,
    ) -> Box<dyn Stream<Item = OutboundMessage> + Send + Unpin>;
}

// ---------------------------------------------------------------------------
// InMemoryBus
// ---------------------------------------------------------------------------

/// In-process broadcast bus backed by `tokio::sync::broadcast`.
///
/// Multiple subscribers each receive every message published after they
/// subscribed (classic broadcast / pub-sub behaviour).  Lagging receivers
/// will have messages dropped by tokio — use a larger capacity if needed.
#[derive(Debug)]
pub struct InMemoryBus {
    inbound_tx: broadcast::Sender<InboundMessage>,
    outbound_tx: broadcast::Sender<OutboundMessage>,
}

impl InMemoryBus {
    /// Create a new bus with a broadcast channel capacity of 256.
    pub fn new() -> Self {
        let (inbound_tx, _) = broadcast::channel(BUS_CAPACITY);
        let (outbound_tx, _) = broadcast::channel(BUS_CAPACITY);
        Self {
            inbound_tx,
            outbound_tx,
        }
    }
}

impl Default for InMemoryBus {
    fn default() -> Self {
        Self::new()
    }
}

// Helper: convert a BroadcastStream (which yields Result<T, RecvError>) into
// a plain Stream<Item = T> by silently dropping lagged/error frames.
fn unwrap_broadcast<T: Clone + Send + 'static>(
    rx: broadcast::Receiver<T>,
) -> Box<dyn Stream<Item = T> + Send + Unpin> {
    let s = BroadcastStream::new(rx)
        .filter_map(|r: Result<T, _>| async move { r.ok() });
    Box::new(Box::pin(s))
}

#[async_trait]
impl MessageBus for InMemoryBus {
    async fn publish_inbound(&self, msg: InboundMessage) -> Result<(), BusError> {
        self.inbound_tx
            .send(msg)
            .map(|_| ())
            .map_err(|_| BusError::SendFailed)
    }

    async fn subscribe_inbound(
        &self,
    ) -> Box<dyn Stream<Item = InboundMessage> + Send + Unpin> {
        unwrap_broadcast(self.inbound_tx.subscribe())
    }

    async fn publish_outbound(&self, msg: OutboundMessage) -> Result<(), BusError> {
        self.outbound_tx
            .send(msg)
            .map(|_| ())
            .map_err(|_| BusError::SendFailed)
    }

    async fn subscribe_outbound(
        &self,
    ) -> Box<dyn Stream<Item = OutboundMessage> + Send + Unpin> {
        unwrap_broadcast(self.outbound_tx.subscribe())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ChannelId, ChannelKind};
    use futures::StreamExt;
    use std::time::SystemTime;

    fn make_inbound(text: &str) -> InboundMessage {
        InboundMessage {
            channel: ChannelId::new(ChannelKind::Slack, "C001"),
            user_id: "U001".into(),
            user_name: None,
            text: text.into(),
            at: SystemTime::now(),
            raw: serde_json::Value::Null,
        }
    }

    fn make_outbound(text: &str) -> OutboundMessage {
        OutboundMessage {
            channel: ChannelId::new(ChannelKind::Slack, "C001"),
            text: text.into(),
            blocks: None,
        }
    }

    #[tokio::test]
    async fn inbound_roundtrip() {
        let bus = InMemoryBus::new();
        let mut rx = bus.subscribe_inbound().await;
        bus.publish_inbound(make_inbound("hello")).await.unwrap();
        let msg = rx.next().await.expect("should receive message");
        assert_eq!(msg.text, "hello");
    }

    #[tokio::test]
    async fn outbound_roundtrip() {
        let bus = InMemoryBus::new();
        let mut rx = bus.subscribe_outbound().await;
        bus.publish_outbound(make_outbound("world")).await.unwrap();
        let msg = rx.next().await.expect("should receive message");
        assert_eq!(msg.text, "world");
    }

    #[tokio::test]
    async fn multiple_inbound_subscribers_all_receive() {
        let bus = InMemoryBus::new();
        let mut rx1 = bus.subscribe_inbound().await;
        let mut rx2 = bus.subscribe_inbound().await;
        bus.publish_inbound(make_inbound("broadcast")).await.unwrap();
        let m1 = rx1.next().await.expect("rx1 should receive");
        let m2 = rx2.next().await.expect("rx2 should receive");
        assert_eq!(m1.text, "broadcast");
        assert_eq!(m2.text, "broadcast");
    }

    #[tokio::test]
    async fn multiple_outbound_subscribers_all_receive() {
        let bus = InMemoryBus::new();
        let mut rx1 = bus.subscribe_outbound().await;
        let mut rx2 = bus.subscribe_outbound().await;
        bus.publish_outbound(make_outbound("fanout")).await.unwrap();
        let m1 = rx1.next().await.expect("rx1 should receive");
        let m2 = rx2.next().await.expect("rx2 should receive");
        assert_eq!(m1.text, "fanout");
        assert_eq!(m2.text, "fanout");
    }

    #[tokio::test]
    async fn publish_inbound_no_subscriber_returns_send_failed() {
        let bus = InMemoryBus::new();
        // No subscriber → broadcast returns error
        let result = bus.publish_inbound(make_inbound("nobody")).await;
        assert!(result.is_err());
    }
}

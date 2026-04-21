//! `Adapter` trait — the contract every channel implementation must satisfy.
//!
//! An adapter bridges one external messaging platform to the shared
//! `MessageBus`.  Call `start` to spawn the adapter; it returns an
//! `AdapterHandle` that lets you shut it down cleanly.

use std::sync::Arc;

use async_trait::async_trait;
use thiserror::Error;
use tokio::sync::oneshot;

use crate::bus::MessageBus;

/// Errors that can surface from adapter operations.
#[derive(Debug, Error)]
pub enum ChannelError {
    #[error("adapter already running")]
    AlreadyRunning,
    #[error("configuration error: {0}")]
    Config(String),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("adapter error: {0}")]
    Other(String),
}

/// A handle returned by `Adapter::start`.  Drop it or call `kill.send(())`
/// to request a graceful shutdown of the adapter's background tasks.
pub struct AdapterHandle {
    /// Send `()` on this channel to signal the adapter to stop.
    pub kill: oneshot::Sender<()>,
}

/// The main adapter trait.
///
/// Every channel implementation (Slack, Discord, Matrix, …) implements this.
/// The trait is object-safe and `Send + Sync`, so adapters can be stored in
/// `Vec<Box<dyn Adapter>>` or `Arc<dyn Adapter>`.
#[async_trait]
pub trait Adapter: Send + Sync {
    /// Human-readable adapter name, e.g. `"slack"` or `"discord"`.
    fn name(&self) -> &str;

    /// Spawn the adapter's background tasks (webhook server, socket client,
    /// outbound pump, etc.) and connect them to `bus`.
    ///
    /// Returns an `AdapterHandle` that can be used to request shutdown.
    async fn start(&self, bus: Arc<dyn MessageBus>) -> Result<AdapterHandle, ChannelError>;
}

// ---------------------------------------------------------------------------
// Tests — mock adapter
// ---------------------------------------------------------------------------

#[cfg(test)]
pub mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    /// A minimal no-op adapter for testing the trait surface.
    pub struct MockAdapter {
        pub started: Arc<AtomicBool>,
    }

    impl MockAdapter {
        pub fn new() -> Self {
            MockAdapter {
                started: Arc::new(AtomicBool::new(false)),
            }
        }
    }

    #[async_trait]
    impl Adapter for MockAdapter {
        fn name(&self) -> &str {
            "mock"
        }

        async fn start(
            &self,
            _bus: Arc<dyn MessageBus>,
        ) -> Result<AdapterHandle, ChannelError> {
            self.started.store(true, Ordering::SeqCst);
            let (kill, _rx) = oneshot::channel();
            Ok(AdapterHandle { kill })
        }
    }

    #[tokio::test]
    async fn mock_adapter_starts() {
        use crate::bus::InMemoryBus;

        let adapter = MockAdapter::new();
        let bus: Arc<dyn MessageBus> = Arc::new(InMemoryBus::new());
        let handle = adapter.start(bus).await.unwrap();
        assert!(adapter.started.load(Ordering::SeqCst));
        // Dropping the handle signals shutdown.
        drop(handle);
    }

    #[tokio::test]
    async fn adapter_trait_is_object_safe() {
        use crate::bus::InMemoryBus;

        // Ensure we can erase to Box<dyn Adapter>.
        let adapters: Vec<Box<dyn Adapter>> = vec![Box::new(MockAdapter::new())];
        let bus: Arc<dyn MessageBus> = Arc::new(InMemoryBus::new());
        for a in &adapters {
            assert_eq!(a.name(), "mock");
            let _handle = a.start(Arc::clone(&bus)).await.unwrap();
        }
    }
}

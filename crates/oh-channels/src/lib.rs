//! # oh-channels
//!
//! Channel adapters for OpenHarness.
//!
//! ## Quick start
//!
//! ```rust,no_run
//! use std::sync::Arc;
//! use oh_channels::{bus::InMemoryBus, slack::SlackAdapter, adapter::Adapter};
//!
//! #[tokio::main]
//! async fn main() {
//!     let bus: Arc<dyn oh_channels::bus::MessageBus> = Arc::new(InMemoryBus::new());
//!     let adapter = SlackAdapter::new("xoxb-YOUR-TOKEN", "YOUR-SIGNING-SECRET");
//!     let _handle = adapter.start(bus).await.unwrap();
//!     // handle.kill.send(()) to stop the adapter
//! }
//! ```

pub mod adapter;
pub mod bus;
pub mod types;

pub mod slack;

// Convenience re-exports
pub use adapter::{Adapter, AdapterHandle, ChannelError};
pub use bus::{BusError, InMemoryBus, MessageBus};
pub use types::{ChannelId, ChannelKind, InboundMessage, OutboundMessage};

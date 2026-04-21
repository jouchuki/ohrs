//! `oh-auth` — credential storage and OAuth flows for ohrs providers.
//!
//! # Quick start
//!
//! ```rust,no_run
//! use oh_auth::{AuthManager, Credential, Provider};
//!
//! #[tokio::main]
//! async fn main() -> Result<(), oh_auth::AuthError> {
//!     let mgr = AuthManager::with_default_backend()?;
//!     mgr.add(Credential::new(Provider::Anthropic, "sk-ant-...")).await?;
//!     let cred = mgr.get(&Provider::Anthropic, None).await?.unwrap();
//!     println!("stored {} cred", cred.provider);
//!     Ok(())
//! }
//! ```
//!
//! # Backends
//!
//! `AuthManager::with_default_backend()` tries the OS keyring first
//! (`keyring-backend` feature, enabled by default) and silently falls back to
//! a plain-JSON file at `~/.config/ohrs/credentials.json` (mode 0600) when
//! no keyring daemon is available (containers, CI, WSL).
//!
//! # OAuth2 device-code flow
//!
//! See [`device_code::DeviceCodeFlow`] for GitHub Copilot / generic OAuth2
//! device-code support.

pub mod device_code;
pub mod error;
pub mod manager;
pub mod providers;
pub mod storage;

// Convenience re-exports at crate root.
pub use error::AuthError;
pub use manager::AuthManager;
pub use providers::Provider;
pub use storage::{Backend, Credential, FileBackend};

#[cfg(feature = "keyring-backend")]
pub use storage::KeyringBackend;

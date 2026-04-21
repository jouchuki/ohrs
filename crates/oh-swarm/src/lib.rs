/// `oh-swarm` — in-process swarm backend, file mailbox, and team lifecycle.
///
/// # First slice (this crate)
///
/// * [`types`] — `TeammateId`, `TeamId`, `Message`, `TeammateConfig`, `TeammateHandle`
/// * [`error`] — `SwarmError`
/// * [`mailbox`] — file-based, atomic-write JSON mailbox
/// * [`backend`] — `Backend` trait + `TeammateStatus`
/// * [`in_process`] — `InProcessBackend` (tokio tasks + `CancellationToken`)
/// * [`team`] — `TeamManager` (file-backed team + member registry)
pub mod backend;
pub mod error;
pub mod in_process;
pub mod mailbox;
pub mod team;
pub mod types;

// Convenience re-exports
pub use backend::{Backend, TeammateStatus};
pub use error::SwarmError;
pub use in_process::InProcessBackend;
pub use mailbox::Mailbox;
pub use team::TeamManager;
pub use types::{
    Message, MessageKind, TaskBody, TeamId, TeammateConfig, TeammateHandle, TeammateId,
};

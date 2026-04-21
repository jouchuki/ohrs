/// `Backend` trait — abstraction for teammate execution environments.
///
/// Implementors: [`InProcessBackend`](crate::in_process::InProcessBackend).
/// Future implementors: subprocess backend, git-worktree backend.
use async_trait::async_trait;

use crate::error::SwarmError;
use crate::types::{TeammateConfig, TeammateHandle, TeammateId};

/// Lifecycle status of a teammate managed by a [`Backend`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TeammateStatus {
    /// Task has been registered but the tokio task has not yet started.
    Spawning,
    /// Task is actively running.
    Running,
    /// Task completed (normally or via cancellation).
    Stopped,
    /// Task exited with an error.
    Errored(String),
}

/// Execution backend for teammate tasks.
///
/// The trait is intentionally minimal so that subprocess, git-worktree, and
/// in-process backends all conform to the same interface.
#[async_trait]
pub trait Backend: Send + Sync {
    /// Spawn a new teammate and return a handle to it.
    async fn spawn(
        &self,
        id: TeammateId,
        config: TeammateConfig,
    ) -> Result<TeammateHandle, SwarmError>;

    /// Signal or forcibly terminate a teammate.
    ///
    /// `graceful = true` → cancel the `CancellationToken` and await task
    /// completion; `graceful = false` → abort immediately.
    async fn kill(&self, id: &TeammateId, graceful: bool) -> Result<(), SwarmError>;

    /// Query the current status of a teammate.
    async fn status(&self, id: &TeammateId) -> Result<TeammateStatus, SwarmError>;
}

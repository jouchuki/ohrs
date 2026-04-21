/// In-process teammate backend.
///
/// Each teammate runs as a `tokio::task` with its own `CancellationToken`.
/// The `DashMap` allows concurrent spawn/kill without a global lock.
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use dashmap::DashMap;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::debug;

use crate::backend::{Backend, TeammateStatus};
use crate::error::SwarmError;
use crate::mailbox::Mailbox;
use crate::types::{TeammateConfig, TeammateHandle, TeammateId};

// ---------------------------------------------------------------------------
// Internal registry entry
// ---------------------------------------------------------------------------

struct Entry {
    cancel: CancellationToken,
    handle: JoinHandle<()>,
    #[allow(dead_code)]
    started_at: Instant,
}

// ---------------------------------------------------------------------------
// InProcessBackend
// ---------------------------------------------------------------------------

/// Runs teammate agents as `tokio::task`s inside the current process.
///
/// State is stored in an `Arc<DashMap>` so the struct can be cheaply cloned
/// and shared across async contexts.
#[derive(Clone)]
pub struct InProcessBackend {
    /// Root directory used to derive each agent's [`Mailbox`].
    team_root: PathBuf,
    tasks: Arc<DashMap<TeammateId, Entry>>,
}

impl InProcessBackend {
    /// Create a new backend whose mailboxes live under `team_root`.
    pub fn new(team_root: impl Into<PathBuf>) -> Self {
        InProcessBackend {
            team_root: team_root.into(),
            tasks: Arc::new(DashMap::new()),
        }
    }

    /// Return the number of currently-registered teammates (running or not).
    pub fn len(&self) -> usize {
        self.tasks.len()
    }

    /// `true` if no teammates are registered.
    pub fn is_empty(&self) -> bool {
        self.tasks.is_empty()
    }
}

#[async_trait]
impl Backend for InProcessBackend {
    async fn spawn(
        &self,
        id: TeammateId,
        config: TeammateConfig,
    ) -> Result<TeammateHandle, SwarmError> {
        if let Some(entry) = self.tasks.get(&id) {
            if !entry.handle.is_finished() {
                return Err(SwarmError::AlreadyRunning(id.0.clone()));
            }
        }

        let cancel = CancellationToken::new();
        let mailbox = Mailbox::for_agent(&self.team_root, &id);

        let cancel_clone = cancel.clone();
        let id_str = id.0.clone();

        // Spawn the user body (or a no-op if none was supplied).
        let handle: JoinHandle<()> = if let Some(body) = config.body {
            let fut = body(cancel_clone, mailbox);
            tokio::spawn(async move {
                debug!(teammate = %id_str, "in-process teammate started");
                fut.await;
                debug!(teammate = %id_str, "in-process teammate finished");
            })
        } else {
            // Headless: just wait for cancellation (useful for future backends
            // that only need the lifecycle tracking).
            let cancel2 = cancel.clone();
            tokio::spawn(async move {
                debug!(teammate = %id_str, "headless teammate waiting for cancel");
                cancel2.cancelled().await;
                debug!(teammate = %id_str, "headless teammate cancelled");
            })
        };

        let entry = Entry {
            cancel: cancel.clone(),
            handle,
            started_at: Instant::now(),
        };

        self.tasks.insert(id.clone(), entry);

        Ok(TeammateHandle {
            id,
            cancel,
        })
    }

    async fn kill(&self, id: &TeammateId, graceful: bool) -> Result<(), SwarmError> {
        let entry = self
            .tasks
            .get(id)
            .ok_or_else(|| SwarmError::TeammateNotFound(id.0.clone()))?;

        if graceful {
            // Signal cancellation and wait for the task to finish.
            entry.cancel.cancel();
            // We must drop the dashmap ref before awaiting to avoid a deadlock.
            let handle = entry.handle.is_finished();
            drop(entry);
            if !handle {
                // Poll briefly; the task will observe the token.
                // (We don't await indefinitely here — callers that need
                // guaranteed completion should await the JoinHandle directly.)
                tokio::task::yield_now().await;
            }
        } else {
            // Forceful: abort the tokio task immediately.
            entry.cancel.cancel();
            entry.handle.abort();
            drop(entry);
        }

        Ok(())
    }

    async fn status(&self, id: &TeammateId) -> Result<TeammateStatus, SwarmError> {
        let entry = self
            .tasks
            .get(id)
            .ok_or_else(|| SwarmError::TeammateNotFound(id.0.clone()))?;

        let status = if entry.handle.is_finished() {
            TeammateStatus::Stopped
        } else if entry.cancel.is_cancelled() {
            // Token was cancelled but task hasn't finished yet → still running
            TeammateStatus::Running
        } else {
            TeammateStatus::Running
        };

        Ok(status)
    }
}

//! Subprocess teammate backend.
//!
//! Each teammate runs as an OS child process — in practice the `oh` binary in
//! its `oh run` one-shot mode, with the prompt and options passed as
//! command-line flags (no stdin framing). The `DashMap` allows concurrent
//! spawn/kill without a global lock, mirroring
//! [`InProcessBackend`](crate::in_process::InProcessBackend).
//!
//! The backend is constructed with a command template (program + base args).
//! Phase 3's `SubagentManager` builds the per-spawn `oh run --prompt …` argument
//! vector and selects this backend via the `BackendRegistry`; this crate only
//! provides a correct, lifecycle-managed [`Backend`] implementation.
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use dashmap::DashMap;
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use tracing::debug;

use crate::backend::{Backend, TeammateStatus};
use crate::error::SwarmError;
use crate::types::{TeammateConfig, TeammateHandle, TeammateId};

// ---------------------------------------------------------------------------
// Internal registry entry
// ---------------------------------------------------------------------------

struct Entry {
    /// The spawned child. Wrapped in `Arc<Mutex<…>>` because `kill`/`status`
    /// need `&mut Child` (to signal and to poll `try_wait`) while the backend is
    /// shared behind an `Arc`. The `Arc` lets callers cheaply clone the handle
    /// out of the `DashMap` and DROP the shard guard *before* awaiting on the
    /// child (see `SWARM-1`); holding a `dashmap::Ref` across `.await` would
    /// stall every teammate hashing to the same shard.
    child: Arc<Mutex<Child>>,
    #[allow(dead_code)]
    started_at: Instant,
    /// Set once the child has been observed exited, so `status` is stable even
    /// after the OS has reaped the process. Shared (`Arc`) so the flag survives
    /// being cloned out of the map alongside `child`.
    finished: Arc<std::sync::atomic::AtomicBool>,
}

// ---------------------------------------------------------------------------
// SubprocessBackend
// ---------------------------------------------------------------------------

/// Runs teammate agents as OS child processes via [`tokio::process::Command`].
///
/// State is stored in an `Arc<DashMap>` so the struct can be cheaply cloned and
/// shared across async contexts.
#[derive(Clone)]
pub struct SubprocessBackend {
    /// Program to exec (e.g. the path to the `oh` binary).
    program: OsString,
    /// Base arguments prepended to every spawn (e.g. `["run"]`). The caller
    /// appends the per-teammate flags (`--prompt …`) when configuring the
    /// teammate's command via [`SubprocessBackend::spawn_command`].
    base_args: Vec<OsString>,
    /// Optional working directory for spawned children. `None` inherits the
    /// parent's cwd; [`WorktreeBackend`](crate::worktree::WorktreeBackend) sets
    /// this to a freshly-created git worktree so it can reuse this backend's
    /// child management unchanged.
    cwd: Option<PathBuf>,
    tasks: Arc<DashMap<TeammateId, Entry>>,
}

impl SubprocessBackend {
    /// Create a backend that launches `program` with `base_args` prepended to
    /// each spawn. For the real swarm, `program` is the `oh` binary and
    /// `base_args` is `["run"]`.
    pub fn new<P, A, S>(program: P, base_args: A) -> Self
    where
        P: Into<OsString>,
        A: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        SubprocessBackend {
            program: program.into(),
            base_args: base_args.into_iter().map(Into::into).collect(),
            cwd: None,
            tasks: Arc::new(DashMap::new()),
        }
    }

    /// Return a copy of this backend that spawns children in `cwd`.
    ///
    /// Used by [`WorktreeBackend`](crate::worktree::WorktreeBackend) to reuse
    /// this backend's child lifecycle management while running the teammate in a
    /// git worktree, instead of duplicating the spawn/kill/status logic.
    pub fn with_cwd(mut self, cwd: impl Into<PathBuf>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    /// Build the `Command` a spawn will run: `program <base_args...>`.
    ///
    /// Exposed so callers can append per-teammate flags before spawning and so
    /// tests can assert the constructed command.
    fn spawn_command(&self) -> Command {
        let mut cmd = Command::new(&self.program);
        cmd.args(&self.base_args);
        if let Some(ref dir) = self.cwd {
            cmd.current_dir(dir);
        }
        // Don't leak the parent's stdin into the child; the design passes
        // everything via flags.
        cmd.stdin(std::process::Stdio::null());
        cmd
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
impl Backend for SubprocessBackend {
    async fn spawn(
        &self,
        id: TeammateId,
        config: TeammateConfig,
    ) -> Result<TeammateHandle, SwarmError> {
        // Clone the existing child handle out and DROP the DashMap ref before
        // awaiting the lock (SWARM-1): holding the shard guard across `.await`
        // would block concurrent ops on teammates in the same shard.
        let existing = self.tasks.get(&id).and_then(|entry| {
            if entry.finished.load(std::sync::atomic::Ordering::SeqCst) {
                None
            } else {
                Some(entry.child.clone())
            }
        });
        if let Some(child) = existing {
            let mut child = child.lock().await;
            if matches!(child.try_wait(), Ok(None)) {
                return Err(SwarmError::AlreadyRunning(id.0.clone()));
            }
        }

        let _ = &config; // display_name is advisory for this backend.

        let mut cmd = self.spawn_command();
        debug!(teammate = %id, "spawning subprocess teammate");
        let child = cmd.spawn().map_err(SwarmError::Io)?;

        let entry = Entry {
            child: Arc::new(Mutex::new(child)),
            started_at: Instant::now(),
            finished: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        };
        self.tasks.insert(id.clone(), entry);

        // The cancellation token is unused by the subprocess backend (kill goes
        // through the OS), but the handle shape is shared across all backends.
        Ok(TeammateHandle {
            id,
            cancel: tokio_util::sync::CancellationToken::new(),
        })
    }

    async fn kill(&self, id: &TeammateId, graceful: bool) -> Result<(), SwarmError> {
        // Clone the child + finished handles out, then DROP the DashMap ref
        // before awaiting the kill/wait (SWARM-1) — never hold a shard guard
        // across `.await`.
        let (child, finished) = {
            let entry = self
                .tasks
                .get(id)
                .ok_or_else(|| SwarmError::TeammateNotFound(id.0.clone()))?;
            (entry.child.clone(), entry.finished.clone())
        };

        let mut child = child.lock().await;
        if graceful {
            // Best-effort graceful stop: ask the kernel to terminate, then wait
            // for the child to be reaped. `Child::kill` sends SIGKILL on Unix;
            // tokio has no portable SIGTERM, so "graceful" still awaits the
            // exit rather than abandoning the process.
            let _ = child.start_kill();
            let _ = child.wait().await;
        } else {
            // Forceful: kill and reap immediately.
            let _ = child.kill().await;
        }
        finished.store(true, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }

    async fn status(&self, id: &TeammateId) -> Result<TeammateStatus, SwarmError> {
        // Clone handles out and drop the DashMap ref before awaiting (SWARM-1).
        let (child, finished) = {
            let entry = self
                .tasks
                .get(id)
                .ok_or_else(|| SwarmError::TeammateNotFound(id.0.clone()))?;
            (entry.child.clone(), entry.finished.clone())
        };

        if finished.load(std::sync::atomic::Ordering::SeqCst) {
            return Ok(TeammateStatus::Stopped);
        }

        let mut child = child.lock().await;
        match child.try_wait() {
            Ok(Some(exit)) => {
                finished.store(true, std::sync::atomic::Ordering::SeqCst);
                if exit.success() {
                    Ok(TeammateStatus::Stopped)
                } else {
                    Ok(TeammateStatus::Errored(format!("child exited with {exit}")))
                }
            }
            Ok(None) => Ok(TeammateStatus::Running),
            Err(e) => Ok(TeammateStatus::Errored(e.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::time::{sleep, Duration};

    #[tokio::test]
    async fn spawn_then_status_reports_stopped_for_true() {
        // `true` exits 0 immediately.
        let backend = SubprocessBackend::new("true", Vec::<String>::new());
        let id = TeammateId::new("ok-1");
        backend
            .spawn(id.clone(), TeammateConfig::headless("ok-1"))
            .await
            .unwrap();
        assert_eq!(backend.len(), 1);

        // Give the child a moment to exit and be observable.
        sleep(Duration::from_millis(50)).await;
        assert_eq!(backend.status(&id).await.unwrap(), TeammateStatus::Stopped);
    }

    #[tokio::test]
    async fn status_reports_errored_for_false() {
        // `false` exits non-zero.
        let backend = SubprocessBackend::new("false", Vec::<String>::new());
        let id = TeammateId::new("err-1");
        backend
            .spawn(id.clone(), TeammateConfig::headless("err-1"))
            .await
            .unwrap();

        sleep(Duration::from_millis(50)).await;
        assert!(matches!(
            backend.status(&id).await.unwrap(),
            TeammateStatus::Errored(_)
        ));
    }

    #[tokio::test]
    async fn kill_terminates_long_running_child() {
        // `sleep 30` keeps running until we kill it.
        let backend = SubprocessBackend::new("sleep", ["30".to_string()]);
        let id = TeammateId::new("sleeper");
        backend
            .spawn(id.clone(), TeammateConfig::headless("sleeper"))
            .await
            .unwrap();

        // Should be running before kill.
        assert_eq!(backend.status(&id).await.unwrap(), TeammateStatus::Running);

        backend.kill(&id, false).await.unwrap();
        assert_eq!(backend.status(&id).await.unwrap(), TeammateStatus::Stopped);
    }

    #[tokio::test]
    async fn graceful_kill_waits_for_exit() {
        let backend = SubprocessBackend::new("sleep", ["30".to_string()]);
        let id = TeammateId::new("graceful");
        backend
            .spawn(id.clone(), TeammateConfig::headless("graceful"))
            .await
            .unwrap();

        backend.kill(&id, true).await.unwrap();
        assert_eq!(backend.status(&id).await.unwrap(), TeammateStatus::Stopped);
    }

    #[tokio::test]
    async fn duplicate_spawn_of_running_child_errors() {
        let backend = SubprocessBackend::new("sleep", ["30".to_string()]);
        let id = TeammateId::new("dup");
        backend
            .spawn(id.clone(), TeammateConfig::headless("dup"))
            .await
            .unwrap();
        let result = backend
            .spawn(id.clone(), TeammateConfig::headless("dup"))
            .await;
        assert!(
            matches!(result, Err(SwarmError::AlreadyRunning(_))),
            "expected AlreadyRunning, got {result:?}"
        );
        // Clean up the long-running child.
        backend.kill(&id, false).await.unwrap();
    }

    #[tokio::test]
    async fn kill_unknown_teammate_errors() {
        let backend = SubprocessBackend::new("true", Vec::<String>::new());
        let result = backend.kill(&TeammateId::new("ghost"), false).await;
        assert!(matches!(result, Err(SwarmError::TeammateNotFound(_))));
    }
}

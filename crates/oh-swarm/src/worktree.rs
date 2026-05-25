//! Git-worktree teammate backend.
//!
//! Each teammate runs as an OS child process (the `oh` binary in `oh run`
//! one-shot mode) whose working directory is a *fresh git worktree* created
//! under a temporary root. The worktree gives the teammate an isolated checkout
//! of the repository it can edit without disturbing the parent's working tree.
//!
//! Child-process lifecycle (spawn / kill / status) is **not** reimplemented
//! here: this backend composes a [`SubprocessBackend`] configured with the
//! worktree as its cwd ([`SubprocessBackend::with_cwd`]) and delegates every
//! [`Backend`] method to it. The only extra responsibility is creating the
//! worktree on `spawn` and removing it on `kill`.
//!
//! Maps to [`SubagentIsolation::Worktree`](oh_types::subagent::SubagentIsolation)
//! in the service-layer `BackendRegistry`.
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use dashmap::DashMap;
use tokio::process::Command;
use tracing::{debug, warn};

use crate::backend::{Backend, TeammateStatus};
use crate::error::SwarmError;
use crate::subprocess::SubprocessBackend;
use crate::types::{TeammateConfig, TeammateHandle, TeammateId};

// ---------------------------------------------------------------------------
// WorktreeBackend
// ---------------------------------------------------------------------------

/// Runs teammate agents as subprocesses inside freshly-created git worktrees.
///
/// State is held in an `Arc` so the struct can be cheaply cloned and shared
/// across async contexts, mirroring [`SubprocessBackend`].
#[derive(Clone)]
pub struct WorktreeBackend {
    /// Repository the worktrees are checked out from. `git worktree add` is run
    /// with this as the cwd.
    repo: PathBuf,
    /// Optional git ref each worktree checks out (branch / commit). `None`
    /// detaches at the repo's current `HEAD`.
    base_ref: Option<String>,
    /// Directory under which per-teammate worktrees are created.
    worktrees_root: PathBuf,
    /// Command template forwarded to each per-teammate [`SubprocessBackend`]:
    /// the `oh` binary path + base args (e.g. `["run"]`).
    program: OsString,
    base_args: Vec<OsString>,
    /// Tracks each teammate's worktree path and the subprocess backend that
    /// owns its child.
    entries: Arc<DashMap<TeammateId, Entry>>,
}

#[derive(Clone)]
struct Entry {
    worktree: PathBuf,
    backend: SubprocessBackend,
}

impl WorktreeBackend {
    /// Build a worktree backend that checks worktrees out of `repo`, places
    /// them under `worktrees_root`, and launches `program` (with `base_args`
    /// prepended) inside each one. For the real swarm, `program` is the `oh`
    /// binary and `base_args` is `["run"]`.
    pub fn new<P, A, S>(
        repo: impl Into<PathBuf>,
        worktrees_root: impl Into<PathBuf>,
        program: P,
        base_args: A,
    ) -> Self
    where
        P: Into<OsString>,
        A: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        WorktreeBackend {
            repo: repo.into(),
            base_ref: None,
            worktrees_root: worktrees_root.into(),
            program: program.into(),
            base_args: base_args.into_iter().map(Into::into).collect(),
            entries: Arc::new(DashMap::new()),
        }
    }

    /// Check each worktree out at `base_ref` (a branch name or commit-ish)
    /// instead of detaching at the repo's current `HEAD`.
    pub fn with_base_ref(mut self, base_ref: impl Into<String>) -> Self {
        self.base_ref = Some(base_ref.into());
        self
    }

    /// Number of currently-registered teammates (worktrees).
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// `true` if no teammates are registered.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn worktree_path_for(&self, id: &TeammateId) -> PathBuf {
        self.worktrees_root.join(&id.0)
    }
}

/// Create a fresh git worktree of `repo` at `dir`.
///
/// `base_ref` selects the branch / commit-ish to check out; `None` detaches at
/// the repo's current `HEAD` so no branch is claimed. Parent directories are
/// created as needed. Shared by [`WorktreeBackend`] and the service-layer
/// subagent manager so the `git worktree add` invocation lives in one place.
pub async fn add_worktree(
    repo: &Path,
    dir: &Path,
    base_ref: Option<&str>,
) -> Result<(), SwarmError> {
    if let Some(parent) = dir.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(repo).arg("worktree").arg("add");
    match base_ref {
        Some(r) => {
            cmd.arg(dir).arg(r);
        }
        None => {
            // No ref → detach at HEAD so we don't move/claim a branch.
            cmd.arg("--detach").arg(dir);
        }
    }
    run_git(cmd, "worktree add").await
}

/// Remove the git worktree rooted at `dir` (best-effort).
///
/// Runs `git -C <repo> worktree remove --force <dir>` and, if that fails,
/// falls back to deleting the directory so the worktree is never leaked.
pub async fn remove_worktree(repo: &Path, dir: &Path) {
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(repo)
        .arg("worktree")
        .arg("remove")
        .arg("--force")
        .arg(dir);
    if let Err(e) = run_git(cmd, "worktree remove").await {
        warn!(worktree = %dir.display(), "failed to remove worktree: {e}");
        // Fall back to removing the directory directly so we don't leak it.
        let _ = tokio::fs::remove_dir_all(dir).await;
    }
}

/// Run a `git` command, mapping a non-zero exit (or spawn failure) to a
/// [`SwarmError`].
async fn run_git(mut cmd: Command, what: &str) -> Result<(), SwarmError> {
    let output = cmd.output().await.map_err(SwarmError::Io)?;
    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(SwarmError::Other(format!(
            "git {what} failed: {}",
            stderr.trim()
        )))
    }
}

#[async_trait]
impl Backend for WorktreeBackend {
    async fn spawn(
        &self,
        id: TeammateId,
        config: TeammateConfig,
    ) -> Result<TeammateHandle, SwarmError> {
        if self.entries.contains_key(&id) {
            return Err(SwarmError::AlreadyRunning(id.0.clone()));
        }

        // 1. Create the isolated checkout.
        let worktree = self.worktree_path_for(&id);
        add_worktree(&self.repo, &worktree, self.base_ref.as_deref()).await?;
        debug!(teammate = %id, worktree = %worktree.display(), "worktree created");

        // 2. Delegate child management to a SubprocessBackend rooted in the
        //    worktree — no duplication of spawn/kill/status logic.
        let backend = SubprocessBackend::new(self.program.clone(), self.base_args.clone())
            .with_cwd(worktree.clone());

        let handle = match backend.spawn(id.clone(), config).await {
            Ok(h) => h,
            Err(e) => {
                // Roll back the worktree if the child failed to launch.
                remove_worktree(&self.repo, &worktree).await;
                return Err(e);
            }
        };

        self.entries.insert(id, Entry { worktree, backend });
        Ok(handle)
    }

    async fn kill(&self, id: &TeammateId, graceful: bool) -> Result<(), SwarmError> {
        let entry = self
            .entries
            .get(id)
            .ok_or_else(|| SwarmError::TeammateNotFound(id.0.clone()))?
            .clone();

        // Stop the child via the inner backend, then tear down the worktree.
        let kill_result = entry.backend.kill(id, graceful).await;
        remove_worktree(&self.repo, &entry.worktree).await;
        self.entries.remove(id);
        kill_result
    }

    async fn status(&self, id: &TeammateId) -> Result<TeammateStatus, SwarmError> {
        let entry = self
            .entries
            .get(id)
            .ok_or_else(|| SwarmError::TeammateNotFound(id.0.clone()))?
            .clone();
        entry.backend.status(id).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use tokio::time::{sleep, Duration};

    /// Initialise a git repo with one commit in a temp dir and return it.
    async fn init_repo() -> TempDir {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();
        let run = |args: &[&str]| {
            let status = std::process::Command::new("git")
                .args(args)
                .current_dir(&path)
                .output()
                .unwrap();
            assert!(
                status.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&status.stderr)
            );
        };
        run(&["init", "-q"]);
        run(&["config", "user.email", "test@example.com"]);
        run(&["config", "user.name", "Test"]);
        std::fs::write(path.join("README.md"), b"hello\n").unwrap();
        run(&["add", "."]);
        run(&["commit", "-q", "-m", "init"]);
        dir
    }

    fn worktrees_root() -> TempDir {
        tempfile::tempdir().unwrap()
    }

    #[tokio::test]
    async fn spawn_creates_worktree_and_runs_child() {
        let repo = init_repo().await;
        let wt_root = worktrees_root();
        // `true` exits 0 immediately; we only care that the worktree is created
        // and the child runs in it.
        let backend = WorktreeBackend::new(
            repo.path(),
            wt_root.path().join("wt"),
            "true",
            Vec::<String>::new(),
        );
        let id = TeammateId::new("wt-ok");

        backend
            .spawn(id.clone(), TeammateConfig::headless("wt-ok"))
            .await
            .unwrap();

        // The worktree directory exists and contains the checked-out file.
        let wt = wt_root.path().join("wt").join("wt-ok");
        assert!(wt.exists(), "worktree dir should exist");
        assert!(
            wt.join("README.md").exists(),
            "checkout should be populated"
        );
        assert_eq!(backend.len(), 1);

        sleep(Duration::from_millis(50)).await;
        assert_eq!(backend.status(&id).await.unwrap(), TeammateStatus::Stopped);

        // Cleanup removes the worktree directory.
        backend.kill(&id, false).await.unwrap();
        assert!(!wt.exists(), "worktree dir should be removed on kill");
        assert!(backend.is_empty());
    }

    #[tokio::test]
    async fn kill_removes_worktree_for_long_running_child() {
        let repo = init_repo().await;
        let wt_root = worktrees_root();
        let backend = WorktreeBackend::new(
            repo.path(),
            wt_root.path().join("wt"),
            "sleep",
            ["30".to_string()],
        );
        let id = TeammateId::new("wt-sleeper");

        backend
            .spawn(id.clone(), TeammateConfig::headless("wt-sleeper"))
            .await
            .unwrap();
        let wt = wt_root.path().join("wt").join("wt-sleeper");
        assert!(wt.exists());
        assert_eq!(backend.status(&id).await.unwrap(), TeammateStatus::Running);

        backend.kill(&id, false).await.unwrap();
        assert!(!wt.exists(), "worktree should be cleaned up after kill");
        // Status on an unknown (removed) teammate errors.
        assert!(matches!(
            backend.status(&id).await,
            Err(SwarmError::TeammateNotFound(_))
        ));
    }

    #[tokio::test]
    async fn duplicate_spawn_errors() {
        let repo = init_repo().await;
        let wt_root = worktrees_root();
        let backend = WorktreeBackend::new(
            repo.path(),
            wt_root.path().join("wt"),
            "sleep",
            ["30".to_string()],
        );
        let id = TeammateId::new("wt-dup");

        backend
            .spawn(id.clone(), TeammateConfig::headless("wt-dup"))
            .await
            .unwrap();
        let result = backend
            .spawn(id.clone(), TeammateConfig::headless("wt-dup"))
            .await;
        assert!(matches!(result, Err(SwarmError::AlreadyRunning(_))));

        backend.kill(&id, false).await.unwrap();
    }

    #[tokio::test]
    async fn kill_unknown_teammate_errors() {
        let repo = init_repo().await;
        let wt_root = worktrees_root();
        let backend = WorktreeBackend::new(
            repo.path(),
            wt_root.path().join("wt"),
            "true",
            Vec::<String>::new(),
        );
        let result = backend.kill(&TeammateId::new("ghost"), false).await;
        assert!(matches!(result, Err(SwarmError::TeammateNotFound(_))));
    }

    #[tokio::test]
    async fn spawn_in_nonrepo_fails_and_leaves_no_entry() {
        // worktrees_root points into a non-git dir → `git worktree add` fails.
        let not_a_repo = tempfile::tempdir().unwrap();
        let wt_root = worktrees_root();
        let backend = WorktreeBackend::new(
            not_a_repo.path(),
            wt_root.path().join("wt"),
            "true",
            Vec::<String>::new(),
        );
        let id = TeammateId::new("wt-norepo");
        let result = backend
            .spawn(id.clone(), TeammateConfig::headless("wt-norepo"))
            .await;
        assert!(result.is_err(), "spawn should fail outside a git repo");
        assert!(backend.is_empty(), "no entry should be recorded on failure");
    }
}

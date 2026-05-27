/// Team lifecycle manager.
///
/// The team's state is persisted to `<root>/<team_id>/members.json` so that
/// a `TeamManager` can re-attach to an existing team after a restart.
use std::path::{Path, PathBuf};
use std::sync::Arc;

use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use tokio::fs;
use tokio::sync::Mutex;
use tracing::debug;

use crate::backend::Backend;
use crate::error::SwarmError;
use crate::mailbox::Mailbox;
use crate::types::{TeamId, TeammateConfig, TeammateHandle, TeammateId};

// ---------------------------------------------------------------------------
// Persisted state
// ---------------------------------------------------------------------------

/// Metadata written to `members.json`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct MembersFile {
    members: Vec<String>,
}

impl MembersFile {
    fn path_for(team_root: &Path) -> PathBuf {
        team_root.join("members.json")
    }

    async fn load(team_root: &Path) -> Result<Self, SwarmError> {
        let p = Self::path_for(team_root);
        if !p.exists() {
            return Ok(Self::default());
        }
        let data = fs::read(&p).await?;
        let f: MembersFile = serde_json::from_slice(&data)?;
        Ok(f)
    }

    async fn save(&self, team_root: &Path) -> Result<(), SwarmError> {
        let p = Self::path_for(team_root);
        let json = serde_json::to_vec_pretty(self)?;
        // Atomic write via tempfile + rename.
        let parent = p.parent().unwrap_or(team_root);
        let team_root2 = parent.to_path_buf();
        let p2 = p.clone();
        tokio::task::spawn_blocking(move || -> Result<(), SwarmError> {
            let mut tmp = tempfile::NamedTempFile::new_in(&team_root2)?;
            use std::io::Write;
            tmp.write_all(&json)?;
            tmp.flush()?;
            tmp.persist(&p2)
                .map_err(|e| SwarmError::Persist(e.to_string()))?;
            Ok(())
        })
        .await
        .map_err(|e| SwarmError::Other(e.to_string()))??;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// TeamManager
// ---------------------------------------------------------------------------

/// Manages the lifecycle of swarm teams and their members.
///
/// State is file-backed (`<root>/<team_id>/members.json`) so the manager
/// can re-attach to existing teams after a process restart.
pub struct TeamManager {
    root: PathBuf,
    backend: Arc<dyn Backend>,
    /// Per-team mutex serializing the `members.json` load-modify-save cycle.
    ///
    /// Without it, concurrent `add_member`/`register_member`/`remove_member`
    /// calls race on the read-modify-write and silently drop members
    /// (last-writer-wins). See `SWARM-2`. The lock is created lazily and keyed
    /// by [`TeamId`]; the `DashMap` itself only guards the per-team `Arc<Mutex>`
    /// handles, so the critical section that touches the file is held by the
    /// inner `Mutex`, not the map shard.
    member_locks: Arc<DashMap<TeamId, Arc<Mutex<()>>>>,
}

impl TeamManager {
    pub fn new(root: PathBuf, backend: Arc<dyn Backend>) -> Self {
        TeamManager {
            root,
            backend,
            member_locks: Arc::new(DashMap::new()),
        }
    }

    fn team_root(&self, team: &TeamId) -> PathBuf {
        self.root.join(&team.0)
    }

    /// Return the per-team mutex guarding `members.json` mutations, creating it
    /// on first use. The `Arc` is cloned out so the `DashMap` shard guard is not
    /// held across the (awaited) lock acquisition.
    fn member_lock(&self, team: &TeamId) -> Arc<Mutex<()>> {
        self.member_locks
            .entry(team.clone())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    /// Create a new team directory and initialise an empty `members.json`.
    /// If the team already exists this is a no-op.
    pub async fn create_team(&self, team: TeamId) -> Result<(), SwarmError> {
        let team_root = self.team_root(&team);
        fs::create_dir_all(&team_root).await?;

        let mf = MembersFile::load(&team_root).await?;
        // Save to ensure the file exists even if it was just created.
        mf.save(&team_root).await?;

        debug!(team = %team, "team created / confirmed");
        Ok(())
    }

    /// Spawn a new teammate and register it in the team's `members.json`.
    pub async fn add_member(
        &self,
        team: &TeamId,
        id: TeammateId,
        config: TeammateConfig,
    ) -> Result<TeammateHandle, SwarmError> {
        let team_root = self.team_root(team);
        if !team_root.exists() {
            return Err(SwarmError::TeamNotFound(team.0.clone()));
        }

        let handle = self.backend.spawn(id.clone(), config).await?;

        // Persist the member list under the per-team lock so concurrent
        // add/register/remove calls don't lose members (SWARM-2).
        let lock = self.member_lock(team);
        let _guard = lock.lock().await;
        let mut mf = MembersFile::load(&team_root).await?;
        if !mf.members.contains(&id.0) {
            mf.members.push(id.0.clone());
            mf.save(&team_root).await?;
        }

        debug!(team = %team, teammate = %id, "member added");
        Ok(handle)
    }

    /// Register a member id in the team's `members.json` *without* spawning a
    /// backend teammate.
    ///
    /// Used by the service-layer team bridge, where the in-memory
    /// `coordinator::TeamRegistry` records an agent/task id against a team for
    /// bookkeeping and the persisted `members.json` is the source of truth.
    /// `add_member` is the spawning variant; this is the persistence half only.
    pub async fn register_member(&self, team: &TeamId, id: &TeammateId) -> Result<(), SwarmError> {
        let team_root = self.team_root(team);
        if !team_root.exists() {
            return Err(SwarmError::TeamNotFound(team.0.clone()));
        }
        let lock = self.member_lock(team);
        let _guard = lock.lock().await;
        let mut mf = MembersFile::load(&team_root).await?;
        if !mf.members.contains(&id.0) {
            mf.members.push(id.0.clone());
            mf.save(&team_root).await?;
        }
        Ok(())
    }

    /// Delete a team: remove its directory (and thus `members.json` + mailboxes)
    /// from disk. No-op if the team directory does not exist.
    pub async fn delete_team(&self, team: &TeamId) -> Result<(), SwarmError> {
        let team_root = self.team_root(team);
        if team_root.exists() {
            fs::remove_dir_all(&team_root).await?;
        }
        debug!(team = %team, "team deleted");
        Ok(())
    }

    /// Return all registered member IDs for a team (from the persisted file).
    pub async fn list_members(&self, team: &TeamId) -> Result<Vec<TeammateId>, SwarmError> {
        let team_root = self.team_root(team);
        if !team_root.exists() {
            return Err(SwarmError::TeamNotFound(team.0.clone()));
        }

        let mf = MembersFile::load(&team_root).await?;
        Ok(mf.members.into_iter().map(TeammateId).collect())
    }

    /// Kill a teammate and remove it from the team's `members.json`.
    pub async fn remove_member(&self, team: &TeamId, id: &TeammateId) -> Result<(), SwarmError> {
        let team_root = self.team_root(team);
        if !team_root.exists() {
            return Err(SwarmError::TeamNotFound(team.0.clone()));
        }

        // Best-effort kill — the task may already be gone.
        match self.backend.kill(id, true).await {
            Ok(_) => {}
            Err(SwarmError::TeammateNotFound(_)) => {}
            Err(e) => return Err(e),
        }

        let lock = self.member_lock(team);
        let _guard = lock.lock().await;
        let mut mf = MembersFile::load(&team_root).await?;
        mf.members.retain(|m| m != &id.0);
        mf.save(&team_root).await?;

        debug!(team = %team, teammate = %id, "member removed");
        Ok(())
    }

    /// Return a [`Mailbox`] for a specific team member.
    pub async fn mailbox_for(
        &self,
        team: &TeamId,
        agent: &TeammateId,
    ) -> Result<Mailbox, SwarmError> {
        let team_root = self.team_root(team);
        if !team_root.exists() {
            return Err(SwarmError::TeamNotFound(team.0.clone()));
        }
        Ok(Mailbox::for_agent(&team_root, agent))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::TeammateStatus;
    use async_trait::async_trait;

    /// Backend that never spawns anything — the bridge tests only exercise the
    /// file-backed persistence half (`create_team`/`register_member`/`delete`).
    struct NullBackend;

    #[async_trait]
    impl Backend for NullBackend {
        async fn spawn(
            &self,
            id: TeammateId,
            _config: TeammateConfig,
        ) -> Result<TeammateHandle, SwarmError> {
            Ok(TeammateHandle {
                id,
                cancel: tokio_util::sync::CancellationToken::new(),
            })
        }
        async fn kill(&self, _id: &TeammateId, _graceful: bool) -> Result<(), SwarmError> {
            Ok(())
        }
        async fn status(&self, _id: &TeammateId) -> Result<TeammateStatus, SwarmError> {
            Ok(TeammateStatus::Stopped)
        }
    }

    fn manager() -> (tempfile::TempDir, TeamManager) {
        let dir = tempfile::tempdir().unwrap();
        let mgr = TeamManager::new(dir.path().to_path_buf(), Arc::new(NullBackend));
        (dir, mgr)
    }

    #[tokio::test]
    async fn create_then_register_member_persists() {
        let (_dir, mgr) = manager();
        let team = TeamId::new("alpha");
        mgr.create_team(team.clone()).await.unwrap();

        mgr.register_member(&team, &TeammateId::new("a1"))
            .await
            .unwrap();
        // Idempotent.
        mgr.register_member(&team, &TeammateId::new("a1"))
            .await
            .unwrap();
        mgr.register_member(&team, &TeammateId::new("a2"))
            .await
            .unwrap();

        let members = mgr.list_members(&team).await.unwrap();
        assert_eq!(members.len(), 2);
        assert!(members.contains(&TeammateId::new("a1")));
        assert!(members.contains(&TeammateId::new("a2")));
    }

    /// SWARM-2 regression: many concurrent `register_member` calls must all be
    /// persisted. Without the per-team lock the load-modify-save races and the
    /// final `members.json` drops members (last-writer-wins).
    #[tokio::test]
    async fn concurrent_register_member_persists_all() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = Arc::new(TeamManager::new(
            dir.path().to_path_buf(),
            Arc::new(NullBackend),
        ));
        let team = TeamId::new("swarm2");
        mgr.create_team(team.clone()).await.unwrap();

        const N: usize = 32;
        let mut handles = Vec::with_capacity(N);
        for i in 0..N {
            let mgr = mgr.clone();
            let team = team.clone();
            handles.push(tokio::spawn(async move {
                mgr.register_member(&team, &TeammateId::new(format!("a{i}")))
                    .await
                    .unwrap();
            }));
        }
        for h in handles {
            h.await.unwrap();
        }

        let members = mgr.list_members(&team).await.unwrap();
        assert_eq!(
            members.len(),
            N,
            "lost members under concurrency: got {} of {N}",
            members.len()
        );
        for i in 0..N {
            assert!(members.contains(&TeammateId::new(format!("a{i}"))));
        }
    }

    #[tokio::test]
    async fn register_member_on_missing_team_errors() {
        let (_dir, mgr) = manager();
        let err = mgr
            .register_member(&TeamId::new("ghost"), &TeammateId::new("a1"))
            .await
            .unwrap_err();
        assert!(matches!(err, SwarmError::TeamNotFound(_)));
    }

    #[tokio::test]
    async fn delete_team_removes_dir_and_is_idempotent() {
        let (dir, mgr) = manager();
        let team = TeamId::new("beta");
        mgr.create_team(team.clone()).await.unwrap();
        assert!(dir.path().join("beta").exists());

        mgr.delete_team(&team).await.unwrap();
        assert!(!dir.path().join("beta").exists());
        // Deleting again is a no-op.
        mgr.delete_team(&team).await.unwrap();
    }
}

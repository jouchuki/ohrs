/// Team lifecycle manager.
///
/// The team's state is persisted to `<root>/<team_id>/members.json` so that
/// a `TeamManager` can re-attach to an existing team after a restart.
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::fs;
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
}

impl TeamManager {
    pub fn new(root: PathBuf, backend: Arc<dyn Backend>) -> Self {
        TeamManager { root, backend }
    }

    fn team_root(&self, team: &TeamId) -> PathBuf {
        self.root.join(&team.0)
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

        // Persist the member list.
        let mut mf = MembersFile::load(&team_root).await?;
        if !mf.members.contains(&id.0) {
            mf.members.push(id.0.clone());
            mf.save(&team_root).await?;
        }

        debug!(team = %team, teammate = %id, "member added");
        Ok(handle)
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
    pub async fn remove_member(
        &self,
        team: &TeamId,
        id: &TeammateId,
    ) -> Result<(), SwarmError> {
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

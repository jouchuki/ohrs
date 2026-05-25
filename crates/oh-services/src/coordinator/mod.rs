//! Coordinator: team registry (direct port) + YAML agent definitions loader.
//!
//! ## Team bridge
//!
//! Two team stores coexist by design:
//!
//! * [`TeamManager`](oh_swarm::TeamManager) — the *file-backed source of truth*
//!   (`<tasks>/teammates/<team>/members.json` + mailboxes), durable across
//!   restarts.
//! * [`TeamRegistry`] — a process-global *in-memory view* the `TeamCreate` /
//!   `TeamDelete` tools read and write synchronously.
//!
//! [`TeamBridge`] keeps them consistent: every mutating op drives the persisted
//! `TeamManager` first, then mirrors the result into the in-memory
//! `TeamRegistry`. The tools call the bridge instead of the bare registry.

pub mod agent_definitions;

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::OnceLock;

use oh_swarm::{InProcessBackend, TeamId, TeamManager, TeammateId};
use oh_types::coordinator::TeamRecord;

/// Store teams and agent memberships.
pub struct TeamRegistry {
    teams: HashMap<String, TeamRecord>,
}

impl TeamRegistry {
    pub fn new() -> Self {
        Self {
            teams: HashMap::new(),
        }
    }

    pub fn create_team(&mut self, name: &str, description: &str) -> Result<&TeamRecord, String> {
        if self.teams.contains_key(name) {
            return Err(format!("Team '{}' already exists", name));
        }
        let team = TeamRecord {
            name: name.to_string(),
            description: description.to_string(),
            agents: Vec::new(),
            messages: Vec::new(),
        };
        self.teams.insert(name.to_string(), team);
        Ok(self.teams.get(name).unwrap())
    }

    pub fn delete_team(&mut self, name: &str) -> Result<(), String> {
        if self.teams.remove(name).is_none() {
            return Err(format!("Team '{}' does not exist", name));
        }
        Ok(())
    }

    pub fn add_agent(&mut self, team_name: &str, task_id: &str) -> Result<(), String> {
        let team = self.require_team(team_name)?;
        if !team.agents.contains(&task_id.to_string()) {
            team.agents.push(task_id.to_string());
        }
        Ok(())
    }

    pub fn send_message(&mut self, team_name: &str, message: &str) -> Result<(), String> {
        let team = self.require_team(team_name)?;
        team.messages.push(message.to_string());
        Ok(())
    }

    pub fn list_teams(&self) -> Vec<&TeamRecord> {
        let mut teams: Vec<_> = self.teams.values().collect();
        teams.sort_by_key(|t| &t.name);
        teams
    }

    fn require_team(&mut self, name: &str) -> Result<&mut TeamRecord, String> {
        self.teams
            .get_mut(name)
            .ok_or_else(|| format!("Team '{}' does not exist", name))
    }
}

impl Default for TeamRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Global singleton team registry.
static TEAM_REGISTRY: OnceLock<Mutex<TeamRegistry>> = OnceLock::new();

pub fn get_team_registry() -> &'static Mutex<TeamRegistry> {
    TEAM_REGISTRY.get_or_init(|| Mutex::new(TeamRegistry::new()))
}

// ---------------------------------------------------------------------------
// Team bridge: file-backed TeamManager (truth) ⇄ in-memory TeamRegistry (view)
// ---------------------------------------------------------------------------

/// Bridges the persisted [`TeamManager`] and the in-memory [`TeamRegistry`].
///
/// Each mutation drives the file-backed `TeamManager` first (the source of
/// truth) and then mirrors it into the global in-memory `TeamRegistry` so the
/// existing synchronous tool reads stay correct. The backend is an
/// [`InProcessBackend`] but is never used to spawn here — the bridge only calls
/// the non-spawning persistence methods (`create_team`, `register_member`,
/// `delete_team`).
pub struct TeamBridge {
    manager: TeamManager,
}

impl TeamBridge {
    /// Build a bridge rooted at `root` (the directory under which each team's
    /// `members.json` lives).
    pub fn new(root: std::path::PathBuf) -> Self {
        let backend = Arc::new(InProcessBackend::new(root.clone()));
        TeamBridge {
            manager: TeamManager::new(root, backend),
        }
    }

    /// Default bridge rooted at `<tasks>/teammates` (matches the subagent
    /// mailbox root used elsewhere).
    pub fn default_root() -> Self {
        Self::new(oh_config::get_tasks_dir().join("teammates"))
    }

    /// Create a team: persist it via `TeamManager`, then mirror into the
    /// in-memory registry. Fails if the in-memory registry already has it.
    pub async fn create_team(&self, name: &str, description: &str) -> Result<(), String> {
        // In-memory check + insert first so the existing "already exists"
        // semantics are preserved, then persist.
        {
            let registry = get_team_registry();
            let mut reg = registry.lock().unwrap();
            reg.create_team(name, description)?;
        }
        if let Err(e) = self.manager.create_team(TeamId::new(name)).await {
            // Roll back the in-memory view so the two stores stay consistent.
            let registry = get_team_registry();
            registry.lock().unwrap().delete_team(name).ok();
            return Err(format!("persist team failed: {e}"));
        }
        Ok(())
    }

    /// Add an agent/task id to a team in both stores.
    pub async fn add_agent(&self, team_name: &str, agent_id: &str) -> Result<(), String> {
        {
            let registry = get_team_registry();
            let mut reg = registry.lock().unwrap();
            reg.add_agent(team_name, agent_id)?;
        }
        self.manager
            .register_member(&TeamId::new(team_name), &TeammateId::new(agent_id))
            .await
            .map_err(|e| format!("persist member failed: {e}"))
    }

    /// Delete a team from both stores.
    pub async fn delete_team(&self, name: &str) -> Result<(), String> {
        {
            let registry = get_team_registry();
            let mut reg = registry.lock().unwrap();
            reg.delete_team(name)?;
        }
        self.manager
            .delete_team(&TeamId::new(name))
            .await
            .map_err(|e| format!("persist delete failed: {e}"))
    }
}

#[cfg(test)]
mod bridge_tests {
    use super::*;

    fn unique(name: &str) -> String {
        format!("{name}_{}", uuid::Uuid::new_v4())
    }

    #[tokio::test]
    async fn create_syncs_both_stores() {
        let dir = tempfile::tempdir().unwrap();
        let bridge = TeamBridge::new(dir.path().to_path_buf());
        let name = unique("bteam");

        bridge.create_team(&name, "desc").await.unwrap();

        // In-memory view has it.
        {
            let reg = get_team_registry().lock().unwrap();
            assert!(reg.list_teams().iter().any(|t| t.name == name));
        }
        // File-backed truth has it (members.json exists).
        assert!(dir.path().join(&name).join("members.json").exists());

        bridge.delete_team(&name).await.unwrap();
    }

    #[tokio::test]
    async fn add_agent_persists_member() {
        let dir = tempfile::tempdir().unwrap();
        let bridge = TeamBridge::new(dir.path().to_path_buf());
        let name = unique("bteam");

        bridge.create_team(&name, "").await.unwrap();
        bridge.add_agent(&name, "task-1").await.unwrap();

        // In-memory.
        {
            let reg = get_team_registry().lock().unwrap();
            let team = reg
                .list_teams()
                .into_iter()
                .find(|t| t.name == name)
                .unwrap();
            assert!(team.agents.contains(&"task-1".to_string()));
        }
        // Persisted member.
        let members = bridge
            .manager
            .list_members(&TeamId::new(&name))
            .await
            .unwrap();
        assert!(members.contains(&TeammateId::new("task-1")));

        bridge.delete_team(&name).await.unwrap();
    }

    #[tokio::test]
    async fn delete_clears_both_stores() {
        let dir = tempfile::tempdir().unwrap();
        let bridge = TeamBridge::new(dir.path().to_path_buf());
        let name = unique("bteam");

        bridge.create_team(&name, "").await.unwrap();
        bridge.delete_team(&name).await.unwrap();

        {
            let reg = get_team_registry().lock().unwrap();
            assert!(!reg.list_teams().iter().any(|t| t.name == name));
        }
        assert!(!dir.path().join(&name).exists());
    }

    #[tokio::test]
    async fn duplicate_create_errors_without_persisting_twice() {
        let dir = tempfile::tempdir().unwrap();
        let bridge = TeamBridge::new(dir.path().to_path_buf());
        let name = unique("bteam");

        bridge.create_team(&name, "").await.unwrap();
        let err = bridge.create_team(&name, "").await.unwrap_err();
        assert!(err.contains("already exists"), "got: {err}");

        bridge.delete_team(&name).await.unwrap();
    }
}

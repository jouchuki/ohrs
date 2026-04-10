//! Minimal coordinator/team registry — direct port of Python.

use oh_types::coordinator::TeamRecord;
use std::collections::HashMap;
use std::sync::OnceLock;
use std::sync::Mutex;

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

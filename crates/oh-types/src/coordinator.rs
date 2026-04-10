//! Multi-agent team coordination types.

use serde::{Deserialize, Serialize};

/// A lightweight in-memory team.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamRecord {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub agents: Vec<String>,
    #[serde(default)]
    pub messages: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_team_record_serde_roundtrip() {
        let team = TeamRecord {
            name: "alpha".into(),
            description: "Alpha team".into(),
            agents: vec!["agent-1".into(), "agent-2".into()],
            messages: vec!["hello".into()],
        };
        let json = serde_json::to_string(&team).unwrap();
        let deser: TeamRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(deser.name, "alpha");
        assert_eq!(deser.agents.len(), 2);
    }

    #[test]
    fn test_team_record_deserialize_defaults() {
        let json = r#"{"name":"beta"}"#;
        let team: TeamRecord = serde_json::from_str(json).unwrap();
        assert_eq!(team.name, "beta");
        assert!(team.description.is_empty());
        assert!(team.agents.is_empty());
        assert!(team.messages.is_empty());
    }
}

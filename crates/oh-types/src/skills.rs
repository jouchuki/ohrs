//! Skill data models.

use serde::{Deserialize, Serialize};

/// A loaded skill definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillDefinition {
    pub name: String,
    pub description: String,
    pub content: String,
    pub source: String,
    pub path: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_skill_definition_serde_roundtrip() {
        let skill = SkillDefinition {
            name: "commit".into(),
            description: "Create a git commit".into(),
            content: "# Instructions\n...".into(),
            source: "builtin".into(),
            path: Some("/skills/commit.md".into()),
        };
        let json = serde_json::to_string(&skill).unwrap();
        let deser: SkillDefinition = serde_json::from_str(&json).unwrap();
        assert_eq!(deser.name, "commit");
        assert_eq!(deser.path, Some("/skills/commit.md".into()));
    }

    #[test]
    fn test_skill_definition_serde_null_path() {
        let skill = SkillDefinition {
            name: "test".into(),
            description: "desc".into(),
            content: "body".into(),
            source: "plugin".into(),
            path: None,
        };
        let json = serde_json::to_string(&skill).unwrap();
        let deser: SkillDefinition = serde_json::from_str(&json).unwrap();
        assert_eq!(deser.path, None);
    }
}

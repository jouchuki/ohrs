//! Skill tool — discovers and invokes named skills from plugins, project, or user config.

use async_trait::async_trait;
use oh_types::tools::{ToolExecutionContext, ToolResult};
use std::sync::RwLock;

/// A registered skill entry visible to the model via the tool schema.
#[derive(Clone, Debug)]
pub struct SkillEntry {
    pub name: String,
    pub description: String,
}

pub struct SkillTool {
    /// Known skills, populated after plugin loading. The model sees these
    /// in the tool's description and input_schema so it knows what to invoke.
    entries: RwLock<Vec<SkillEntry>>,
}

impl SkillTool {
    pub fn new() -> Self {
        Self {
            entries: RwLock::new(Vec::new()),
        }
    }

    /// Register skills so they appear in the tool schema sent to the API.
    /// Call this after plugin loading, before the first API request.
    pub fn set_available_skills(&self, skills: Vec<SkillEntry>) {
        let mut entries = self.entries.write().unwrap();
        *entries = skills;
    }

    fn get_entries(&self) -> Vec<SkillEntry> {
        self.entries.read().unwrap().clone()
    }
}

#[async_trait]
impl crate::traits::Tool for SkillTool {
    fn name(&self) -> &str {
        "Skill"
    }

    fn description(&self) -> &str {
        // Static part — the dynamic skill list is in input_schema
        "Invoke a named skill. Skills are reusable prompt/instruction sets from plugins or project config. When the user types /<skill-name>, use this tool."
    }

    fn input_schema(&self) -> serde_json::Value {
        let entries = self.get_entries();

        if entries.is_empty() {
            return serde_json::json!({
                "type": "object",
                "properties": {
                    "skill": {
                        "type": "string",
                        "description": "The skill name to invoke"
                    },
                    "args": {
                        "type": "string",
                        "description": "Optional arguments for the skill"
                    }
                },
                "required": ["skill"]
            });
        }

        // Build enum + descriptions so the model knows exactly what's available
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        let skill_list: Vec<String> = entries
            .iter()
            .map(|e| {
                if e.description.is_empty() {
                    e.name.clone()
                } else {
                    format!("{}: {}", e.name, e.description)
                }
            })
            .collect();
        let skill_description = format!(
            "The skill name to invoke. Available skills:\n{}",
            skill_list.join("\n")
        );

        serde_json::json!({
            "type": "object",
            "properties": {
                "skill": {
                    "type": "string",
                    "enum": names,
                    "description": skill_description
                },
                "args": {
                    "type": "string",
                    "description": "Optional arguments for the skill"
                }
            },
            "required": ["skill"]
        })
    }

    fn is_read_only(&self, _arguments: &serde_json::Value) -> bool {
        true
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        context: &ToolExecutionContext,
    ) -> ToolResult {
        let skill_name = match arguments.get("skill").and_then(|v| v.as_str()) {
            Some(s) => s,
            None => return ToolResult::error("Missing required parameter: skill"),
        };

        // Look up the skill in context metadata
        if let Some(registry) = context.metadata.get("skill_registry") {
            if let Some(obj) = registry.as_object() {
                let content = obj
                    .get(skill_name)
                    .or_else(|| obj.get(&skill_name.to_lowercase()));

                if let Some(val) = content {
                    // Substitute $ARGUMENTS with the args parameter
                    let args = arguments
                        .get("args")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");

                    let text = if let Some(t) = val.as_str() {
                        t.to_string()
                    } else if let Some(t) = val.get("content").and_then(|v| v.as_str()) {
                        t.to_string()
                    } else {
                        return ToolResult::error(format!(
                            "Skill '{skill_name}' has invalid content format"
                        ));
                    };

                    let text = text.replace("$ARGUMENTS", args);
                    return ToolResult::success(&text);
                }
            }
        }

        // List available skills in error message
        let entries = self.get_entries();
        if entries.is_empty() {
            ToolResult::error(format!("Skill not found: {skill_name}. No skills are registered."))
        } else {
            let available: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
            ToolResult::error(format!(
                "Skill not found: {skill_name}. Available skills: {}",
                available.join(", ")
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::traits::Tool;
    use oh_types::tools::ToolExecutionContext;

    fn ctx() -> ToolExecutionContext {
        ToolExecutionContext::new(std::env::current_dir().unwrap())
    }

    #[test]
    fn test_schema_has_required_skill() {
        let tool = SkillTool::new();
        let schema = tool.input_schema();
        let required = schema["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v == "skill"));
    }

    #[test]
    fn test_schema_without_skills_has_no_enum() {
        let tool = SkillTool::new();
        let schema = tool.input_schema();
        assert!(schema["properties"]["skill"]["enum"].is_null());
    }

    #[test]
    fn test_schema_with_skills_has_enum() {
        let tool = SkillTool::new();
        tool.set_available_skills(vec![
            SkillEntry { name: "commit".into(), description: "Create a git commit".into() },
            SkillEntry { name: "review".into(), description: "Review code".into() },
        ]);
        let schema = tool.input_schema();
        let enum_vals = schema["properties"]["skill"]["enum"].as_array().unwrap();
        assert_eq!(enum_vals.len(), 2);
        assert!(enum_vals.contains(&serde_json::json!("commit")));
        assert!(enum_vals.contains(&serde_json::json!("review")));
    }

    #[test]
    fn test_schema_description_lists_skills() {
        let tool = SkillTool::new();
        tool.set_available_skills(vec![
            SkillEntry { name: "deploy".into(), description: "Deploy to prod".into() },
        ]);
        let schema = tool.input_schema();
        let desc = schema["properties"]["skill"]["description"].as_str().unwrap();
        assert!(desc.contains("deploy: Deploy to prod"));
    }

    #[test]
    fn test_is_read_only_returns_true() {
        let tool = SkillTool::new();
        assert!(tool.is_read_only(&serde_json::json!({})));
    }

    #[tokio::test]
    async fn test_skill_not_found_lists_available() {
        let tool = SkillTool::new();
        tool.set_available_skills(vec![
            SkillEntry { name: "foo".into(), description: "".into() },
        ]);
        let result = tool
            .execute(serde_json::json!({"skill": "nonexistent"}), &ctx())
            .await;
        assert!(result.is_error);
        assert!(result.output.contains("foo"));
    }

    #[tokio::test]
    async fn test_missing_skill_param() {
        let tool = SkillTool::new();
        let result = tool.execute(serde_json::json!({}), &ctx()).await;
        assert!(result.is_error);
        assert!(result.output.contains("skill"));
    }

    #[tokio::test]
    async fn test_skill_found_in_registry() {
        let tool = SkillTool::new();
        let mut context = ctx();
        context.metadata.insert(
            "skill_registry".to_string(),
            serde_json::json!({
                "commit": "Instructions for committing code..."
            }),
        );
        let result = tool
            .execute(serde_json::json!({"skill": "commit"}), &context)
            .await;
        assert!(!result.is_error);
        assert_eq!(result.output, "Instructions for committing code...");
    }

    #[tokio::test]
    async fn test_skill_found_case_insensitive() {
        let tool = SkillTool::new();
        let mut context = ctx();
        context.metadata.insert(
            "skill_registry".to_string(),
            serde_json::json!({
                "commit": "Commit instructions"
            }),
        );
        let result = tool
            .execute(serde_json::json!({"skill": "Commit"}), &context)
            .await;
        assert!(!result.is_error);
        assert_eq!(result.output, "Commit instructions");
    }

    #[tokio::test]
    async fn test_skill_with_content_field() {
        let tool = SkillTool::new();
        let mut context = ctx();
        context.metadata.insert(
            "skill_registry".to_string(),
            serde_json::json!({
                "review": {"content": "Review skill content"}
            }),
        );
        let result = tool
            .execute(serde_json::json!({"skill": "review"}), &context)
            .await;
        assert!(!result.is_error);
        assert_eq!(result.output, "Review skill content");
    }

    #[tokio::test]
    async fn test_skill_substitutes_arguments() {
        let tool = SkillTool::new();
        let mut context = ctx();
        context.metadata.insert(
            "skill_registry".to_string(),
            serde_json::json!({
                "greet": {"content": "Hello $ARGUMENTS!"}
            }),
        );
        let result = tool
            .execute(
                serde_json::json!({"skill": "greet", "args": "world"}),
                &context,
            )
            .await;
        assert!(!result.is_error);
        assert_eq!(result.output, "Hello world!");
    }
}

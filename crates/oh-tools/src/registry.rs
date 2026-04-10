//! Tool registry — maps tool names to implementations.

use crate::traits::Tool;
use oh_types::tools::ToolSchema;
use std::collections::HashMap;

/// Map tool names to implementations.
pub struct ToolRegistry {
    tools: HashMap<String, Box<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
        }
    }

    /// Register a tool instance.
    pub fn register(&mut self, tool: Box<dyn Tool>) {
        self.tools.insert(tool.name().to_string(), tool);
    }

    /// Return a registered tool by name.
    pub fn get(&self, name: &str) -> Option<&dyn Tool> {
        self.tools.get(name).map(|t| t.as_ref())
    }

    /// Return all registered tools.
    pub fn list_tools(&self) -> Vec<&dyn Tool> {
        self.tools.values().map(|t| t.as_ref()).collect()
    }

    /// Return all tool schemas in API format.
    pub fn to_api_schemas(&self) -> Vec<ToolSchema> {
        self.tools.values().map(|t| t.to_api_schema()).collect()
    }

    /// Return all tool schemas as serde_json Values for the API.
    pub fn to_api_schema(&self) -> Vec<serde_json::Value> {
        self.tools
            .values()
            .map(|t| {
                let schema = t.to_api_schema();
                serde_json::json!({
                    "name": schema.name,
                    "description": schema.description,
                    "input_schema": schema.input_schema,
                })
            })
            .collect()
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use oh_types::tools::{ToolExecutionContext, ToolResult};

    struct FakeTool {
        tool_name: &'static str,
    }

    #[async_trait]
    impl Tool for FakeTool {
        fn name(&self) -> &str {
            self.tool_name
        }
        fn description(&self) -> &str {
            "fake tool"
        }
        fn input_schema(&self) -> serde_json::Value {
            serde_json::json!({"type": "object", "properties": {}})
        }
        async fn execute(
            &self,
            _arguments: serde_json::Value,
            _context: &ToolExecutionContext,
        ) -> ToolResult {
            ToolResult::success("ok")
        }
    }

    #[test]
    fn test_new_registry_is_empty() {
        let registry = ToolRegistry::new();
        assert!(registry.list_tools().is_empty());
    }

    #[test]
    fn test_register_and_get() {
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(FakeTool { tool_name: "alpha" }));
        let tool = registry.get("alpha");
        assert!(tool.is_some());
        assert_eq!(tool.unwrap().name(), "alpha");
    }

    #[test]
    fn test_get_returns_none_for_unknown() {
        let registry = ToolRegistry::new();
        assert!(registry.get("nonexistent").is_none());
    }

    #[test]
    fn test_list_tools_returns_all() {
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(FakeTool { tool_name: "a" }));
        registry.register(Box::new(FakeTool { tool_name: "b" }));
        let tools = registry.list_tools();
        assert_eq!(tools.len(), 2);
        let mut names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
        names.sort();
        assert_eq!(names, vec!["a", "b"]);
    }

    #[test]
    fn test_to_api_schema_returns_json_values() {
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(FakeTool { tool_name: "t1" }));
        let schemas = registry.to_api_schema();
        assert_eq!(schemas.len(), 1);
        assert_eq!(schemas[0]["name"], "t1");
        assert_eq!(schemas[0]["description"], "fake tool");
        assert!(schemas[0]["input_schema"].is_object());
    }

    #[test]
    fn test_to_api_schemas_returns_tool_schema_vec() {
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(FakeTool { tool_name: "x" }));
        registry.register(Box::new(FakeTool { tool_name: "y" }));
        let schemas = registry.to_api_schemas();
        assert_eq!(schemas.len(), 2);
        let mut names: Vec<&str> = schemas.iter().map(|s| s.name.as_str()).collect();
        names.sort();
        assert_eq!(names, vec!["x", "y"]);
    }
}

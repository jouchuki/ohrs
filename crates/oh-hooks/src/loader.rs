//! Hook registry: loads hooks from settings and plugins.

use oh_types::hooks::{HookDefinition, HookEvent};
use std::collections::HashMap;

/// Registry mapping events to their hook definitions.
#[derive(Debug, Default, Clone)]
pub struct HookRegistry {
    hooks: HashMap<HookEvent, Vec<HookDefinition>>,
}

impl HookRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a hook for a given event.
    pub fn register(&mut self, event: HookEvent, hook: HookDefinition) {
        self.hooks.entry(event).or_default().push(hook);
    }

    /// Get all hooks for an event.
    pub fn get(&self, event: &HookEvent) -> &[HookDefinition] {
        self.hooks.get(event).map(|v| v.as_slice()).unwrap_or(&[])
    }

    /// Return a summary of registered hooks.
    pub fn summary(&self) -> String {
        let total: usize = self.hooks.values().map(|v| v.len()).sum();
        if total == 0 {
            return "No hooks configured".into();
        }
        let mut lines = Vec::new();
        for (event, hooks) in &self.hooks {
            for hook in hooks {
                lines.push(format!("  {} → {} hook", event, hook.hook_type()));
            }
        }
        format!("{total} hooks configured:\n{}", lines.join("\n"))
    }

    /// Remove all hooks for a given event.
    pub fn clear_event(&mut self, event: &HookEvent) {
        self.hooks.remove(event);
    }

    /// Remove all hooks.
    pub fn clear_all(&mut self) {
        self.hooks.clear();
    }

    /// Return all events and their hook counts.
    pub fn list_all(&self) -> Vec<(HookEvent, usize)> {
        let mut result: Vec<_> = self
            .hooks
            .iter()
            .map(|(event, hooks)| (*event, hooks.len()))
            .collect();
        result.sort_by_key(|(e, _)| format!("{e}"));
        result
    }

    /// Merge hooks from a map (e.g., from settings or plugins).
    pub fn merge_from_map(&mut self, map: &HashMap<String, Vec<HookDefinition>>) {
        for (event_str, hooks) in map {
            if let Ok(event) = serde_json::from_value::<HookEvent>(
                serde_json::Value::String(event_str.clone()),
            ) {
                for hook in hooks {
                    self.register(event, hook.clone());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oh_types::hooks::CommandHookDefinition;

    fn make_command_hook(command: &str) -> HookDefinition {
        HookDefinition::Command(CommandHookDefinition {
            r#type: "command".into(),
            command: command.into(),
            timeout_seconds: 30,
            matcher: None,
            block_on_failure: false,
        })
    }

    #[test]
    fn test_new_registry_is_empty() {
        let reg = HookRegistry::new();
        assert_eq!(reg.get(&HookEvent::PreToolUse).len(), 0);
        assert_eq!(reg.summary(), "No hooks configured");
    }

    #[test]
    fn test_register_and_get() {
        let mut reg = HookRegistry::new();
        let hook = make_command_hook("echo test");
        reg.register(HookEvent::PreToolUse, hook);

        let hooks = reg.get(&HookEvent::PreToolUse);
        assert_eq!(hooks.len(), 1);
        assert_eq!(hooks[0].hook_type(), "command");
    }

    #[test]
    fn test_get_unregistered_event_returns_empty() {
        let mut reg = HookRegistry::new();
        reg.register(HookEvent::PreToolUse, make_command_hook("echo"));
        assert!(reg.get(&HookEvent::SessionStart).is_empty());
    }

    #[test]
    fn test_summary_with_hooks() {
        let mut reg = HookRegistry::new();
        reg.register(HookEvent::PreToolUse, make_command_hook("echo a"));
        reg.register(HookEvent::PreToolUse, make_command_hook("echo b"));

        let summary = reg.summary();
        assert!(summary.starts_with("2 hooks configured:"));
        assert!(summary.contains("command hook"));
    }

    #[test]
    fn test_merge_from_map() {
        let mut reg = HookRegistry::new();
        let mut map = HashMap::new();
        map.insert(
            "pre_tool_use".to_string(),
            vec![make_command_hook("echo merged")],
        );
        reg.merge_from_map(&map);

        let hooks = reg.get(&HookEvent::PreToolUse);
        assert_eq!(hooks.len(), 1);
    }

    #[test]
    fn test_merge_from_map_invalid_event_ignored() {
        let mut reg = HookRegistry::new();
        let mut map = HashMap::new();
        map.insert(
            "not_a_real_event".to_string(),
            vec![make_command_hook("echo nope")],
        );
        reg.merge_from_map(&map);

        assert_eq!(reg.summary(), "No hooks configured");
    }
}

//! Hook matcher logic — fnmatch-style glob matching.

use oh_types::hooks::HookDefinition;

/// Check if a hook matches the given payload.
pub fn matches_hook(hook: &HookDefinition, payload: &serde_json::Value) -> bool {
    let matcher = match hook.matcher() {
        Some(m) if !m.is_empty() => m,
        _ => return true,
    };

    let subject = payload
        .get("tool_name")
        .or_else(|| payload.get("prompt"))
        .or_else(|| payload.get("event"))
        .and_then(|v| v.as_str())
        .unwrap_or("");

    glob_match(matcher, subject)
}

fn glob_match(pattern: &str, text: &str) -> bool {
    globset::Glob::new(pattern)
        .ok()
        .map(|g| g.compile_matcher().is_match(text))
        .unwrap_or(false)
}

/// Inject `$ARGUMENTS` placeholder in a template string.
pub fn inject_arguments(template: &str, payload: &serde_json::Value) -> String {
    template.replace("$ARGUMENTS", &payload.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use oh_types::hooks::CommandHookDefinition;

    fn make_hook(matcher: Option<&str>) -> HookDefinition {
        HookDefinition::Command(CommandHookDefinition {
            r#type: "command".into(),
            command: "echo test".into(),
            timeout_seconds: 30,
            matcher: matcher.map(String::from),
            block_on_failure: false,
        })
    }

    #[test]
    fn test_matches_hook_no_matcher_returns_true() {
        let hook = make_hook(None);
        let payload = serde_json::json!({"tool_name": "anything"});
        assert!(matches_hook(&hook, &payload));
    }

    #[test]
    fn test_matches_hook_glob_matches() {
        let hook = make_hook(Some("bash*"));
        let payload = serde_json::json!({"tool_name": "bash"});
        assert!(matches_hook(&hook, &payload));
    }

    #[test]
    fn test_matches_hook_glob_no_match() {
        let hook = make_hook(Some("read_*"));
        let payload = serde_json::json!({"tool_name": "bash"});
        assert!(!matches_hook(&hook, &payload));
    }

    #[test]
    fn test_matches_hook_empty_matcher_returns_true() {
        let hook = make_hook(Some(""));
        let payload = serde_json::json!({"tool_name": "bash"});
        assert!(matches_hook(&hook, &payload));
    }

    #[test]
    fn test_inject_arguments_replaces_placeholder() {
        let payload = serde_json::json!({"key": "value"});
        let result = inject_arguments("echo $ARGUMENTS", &payload);
        assert!(result.contains(r#""key":"value""#));
        assert!(!result.contains("$ARGUMENTS"));
    }

    #[test]
    fn test_inject_arguments_no_placeholder() {
        let payload = serde_json::json!({"key": "value"});
        let result = inject_arguments("echo hello", &payload);
        assert_eq!(result, "echo hello");
    }
}

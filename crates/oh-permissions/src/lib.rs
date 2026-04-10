//! Permission checking for tool execution in OpenHarness.

use oh_config::PermissionSettings;
use oh_types::permissions::{PermissionDecision, PermissionMode, PermissionRequest, PathRule};
use opentelemetry::KeyValue;
use tracing::instrument;

/// Evaluate tool usage against the configured permission mode and rules.
pub struct PermissionChecker {
    settings: PermissionSettings,
    path_rules: Vec<PathRule>,
}

impl PermissionChecker {
    pub fn new(settings: PermissionSettings) -> Self {
        let path_rules = settings
            .path_rules
            .iter()
            .map(|r| PathRule {
                pattern: r.pattern.clone(),
                allow: r.allow,
            })
            .collect();
        Self {
            settings,
            path_rules,
        }
    }

    /// Return whether the tool may run immediately.
    #[instrument(skip(self), fields(tool = %request.tool_name, mode = ?self.settings.mode))]
    pub fn evaluate(&self, request: &PermissionRequest) -> PermissionDecision {
        oh_telemetry::PERMISSION_CHECK_TOTAL.add(
            1,
            &[KeyValue::new("tool", request.tool_name.to_string())],
        );

        // Explicit tool deny list
        if self.settings.denied_tools.iter().any(|t| t == request.tool_name) {
            oh_telemetry::PERMISSION_DENIED_COUNT.add(
                1,
                &[KeyValue::new("tool", request.tool_name.to_string())],
            );
            return PermissionDecision::deny(format!(
                "{} is explicitly denied",
                request.tool_name
            ));
        }

        // Explicit tool allow list
        if self.settings.allowed_tools.iter().any(|t| t == request.tool_name) {
            return PermissionDecision::allow(format!(
                "{} is explicitly allowed",
                request.tool_name
            ));
        }

        // Check path-level rules
        if let Some(file_path) = request.file_path {
            for rule in &self.path_rules {
                if glob_match(&rule.pattern, file_path) && !rule.allow {
                    oh_telemetry::PERMISSION_DENIED_COUNT.add(
                        1,
                        &[KeyValue::new("tool", request.tool_name.to_string())],
                    );
                    return PermissionDecision::deny(format!(
                        "Path {} matches deny rule: {}",
                        file_path, rule.pattern
                    ));
                }
            }
        }

        // Check command deny patterns
        if let Some(command) = request.command {
            for pattern in &self.settings.denied_commands {
                if glob_match(pattern, command) {
                    oh_telemetry::PERMISSION_DENIED_COUNT.add(
                        1,
                        &[KeyValue::new("tool", request.tool_name.to_string())],
                    );
                    return PermissionDecision::deny(format!(
                        "Command matches deny pattern: {}",
                        pattern
                    ));
                }
            }
        }

        // Full auto: allow everything
        if self.settings.mode == PermissionMode::FullAuto {
            return PermissionDecision::allow("Auto mode allows all tools");
        }

        // Read-only tools always allowed
        if request.is_read_only {
            return PermissionDecision::allow("read-only tools are allowed");
        }

        // Plan mode: block mutating tools
        if self.settings.mode == PermissionMode::Plan {
            oh_telemetry::PERMISSION_DENIED_COUNT.add(
                1,
                &[KeyValue::new("tool", request.tool_name.to_string())],
            );
            return PermissionDecision::deny(
                "Plan mode blocks mutating tools until the user exits plan mode",
            );
        }

        // Default mode: require confirmation for mutating tools
        PermissionDecision::confirm(
            "Mutating tools require user confirmation in default mode",
        )
    }
}

/// Simple glob matching (fnmatch-style).
fn glob_match(pattern: &str, text: &str) -> bool {
    globset::Glob::new(pattern)
        .ok()
        .and_then(|g| g.compile_matcher().is_match(text).then_some(()))
        .is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use oh_config::{PathRuleConfig, PermissionSettings};
    use oh_types::permissions::{PermissionMode, PermissionRequest};

    fn request<'a>(tool: &'a str, read_only: bool) -> PermissionRequest<'a> {
        PermissionRequest {
            tool_name: tool,
            is_read_only: read_only,
            file_path: None,
            command: None,
        }
    }

    fn settings(mode: PermissionMode) -> PermissionSettings {
        PermissionSettings {
            mode,
            ..Default::default()
        }
    }

    // --- Default mode ---

    #[test]
    fn default_mode_allows_read_only() {
        let checker = PermissionChecker::new(settings(PermissionMode::Default));
        let decision = checker.evaluate(&request("read_file", true));
        assert!(decision.allowed);
    }

    #[test]
    fn default_mode_requires_confirmation_for_mutation() {
        let checker = PermissionChecker::new(settings(PermissionMode::Default));
        let decision = checker.evaluate(&request("write_file", false));
        assert!(!decision.allowed);
        assert!(decision.requires_confirmation);
    }

    // --- Plan mode ---

    #[test]
    fn plan_mode_blocks_mutating_tools() {
        let checker = PermissionChecker::new(settings(PermissionMode::Plan));
        let decision = checker.evaluate(&request("bash", false));
        assert!(!decision.allowed);
        assert!(
            decision.reason.to_lowercase().contains("plan mode"),
            "reason should mention plan mode, got: {}",
            decision.reason
        );
    }

    #[test]
    fn plan_mode_allows_read_only() {
        let checker = PermissionChecker::new(settings(PermissionMode::Plan));
        let decision = checker.evaluate(&request("read_file", true));
        assert!(decision.allowed);
    }

    // --- FullAuto mode ---

    #[test]
    fn full_auto_allows_mutating_tools() {
        let checker = PermissionChecker::new(settings(PermissionMode::FullAuto));
        let decision = checker.evaluate(&request("bash", false));
        assert!(decision.allowed);
    }

    #[test]
    fn full_auto_allows_read_only() {
        let checker = PermissionChecker::new(settings(PermissionMode::FullAuto));
        let decision = checker.evaluate(&request("read_file", true));
        assert!(decision.allowed);
    }

    // --- Explicit denied_tools ---

    #[test]
    fn denied_tool_is_blocked_even_in_full_auto() {
        let s = PermissionSettings {
            mode: PermissionMode::FullAuto,
            denied_tools: vec!["dangerous_tool".into()],
            ..Default::default()
        };
        let checker = PermissionChecker::new(s);
        let decision = checker.evaluate(&request("dangerous_tool", false));
        assert!(!decision.allowed);
        assert!(decision.reason.contains("explicitly denied"));
    }

    #[test]
    fn denied_tool_is_blocked_when_read_only() {
        let s = PermissionSettings {
            mode: PermissionMode::Default,
            denied_tools: vec!["sneaky_reader".into()],
            ..Default::default()
        };
        let checker = PermissionChecker::new(s);
        let decision = checker.evaluate(&request("sneaky_reader", true));
        assert!(!decision.allowed);
    }

    // --- Explicit allowed_tools ---

    #[test]
    fn allowed_tool_passes_in_default_mode() {
        let s = PermissionSettings {
            mode: PermissionMode::Default,
            allowed_tools: vec!["special_tool".into()],
            ..Default::default()
        };
        let checker = PermissionChecker::new(s);
        let decision = checker.evaluate(&request("special_tool", false));
        assert!(decision.allowed);
        assert!(decision.reason.contains("explicitly allowed"));
    }

    #[test]
    fn allowed_tool_passes_in_plan_mode() {
        let s = PermissionSettings {
            mode: PermissionMode::Plan,
            allowed_tools: vec!["special_tool".into()],
            ..Default::default()
        };
        let checker = PermissionChecker::new(s);
        let decision = checker.evaluate(&request("special_tool", false));
        assert!(decision.allowed);
    }

    #[test]
    fn denied_takes_precedence_over_allowed() {
        let s = PermissionSettings {
            mode: PermissionMode::FullAuto,
            allowed_tools: vec!["tool_x".into()],
            denied_tools: vec!["tool_x".into()],
            ..Default::default()
        };
        let checker = PermissionChecker::new(s);
        let decision = checker.evaluate(&request("tool_x", false));
        assert!(!decision.allowed, "deny list should take precedence over allow list");
    }

    // --- Path glob rules ---

    #[test]
    fn path_deny_rule_blocks_matching_path() {
        let s = PermissionSettings {
            mode: PermissionMode::FullAuto,
            path_rules: vec![PathRuleConfig {
                pattern: "/etc/**".into(),
                allow: false,
            }],
            ..Default::default()
        };
        let checker = PermissionChecker::new(s);
        let req = PermissionRequest {
            tool_name: "write_file",
            is_read_only: false,
            file_path: Some("/etc/passwd"),
            command: None,
        };
        let decision = checker.evaluate(&req);
        assert!(!decision.allowed);
        assert!(decision.reason.contains("deny rule"));
    }

    #[test]
    fn path_deny_rule_does_not_block_non_matching_path() {
        let s = PermissionSettings {
            mode: PermissionMode::FullAuto,
            path_rules: vec![PathRuleConfig {
                pattern: "/etc/**".into(),
                allow: false,
            }],
            ..Default::default()
        };
        let checker = PermissionChecker::new(s);
        let req = PermissionRequest {
            tool_name: "write_file",
            is_read_only: false,
            file_path: Some("/home/user/file.txt"),
            command: None,
        };
        let decision = checker.evaluate(&req);
        assert!(decision.allowed);
    }

    // --- Command deny patterns ---

    #[test]
    fn command_deny_pattern_blocks_matching_command() {
        let s = PermissionSettings {
            mode: PermissionMode::FullAuto,
            denied_commands: vec!["rm -rf *".into()],
            ..Default::default()
        };
        let checker = PermissionChecker::new(s);
        let req = PermissionRequest {
            tool_name: "bash",
            is_read_only: false,
            file_path: None,
            command: Some("rm -rf *"),
        };
        let decision = checker.evaluate(&req);
        assert!(!decision.allowed);
        assert!(decision.reason.contains("deny pattern"));
    }

    #[test]
    fn command_deny_pattern_does_not_block_non_matching() {
        let s = PermissionSettings {
            mode: PermissionMode::FullAuto,
            denied_commands: vec!["rm -rf *".into()],
            ..Default::default()
        };
        let checker = PermissionChecker::new(s);
        let req = PermissionRequest {
            tool_name: "bash",
            is_read_only: false,
            file_path: None,
            command: Some("ls -la"),
        };
        let decision = checker.evaluate(&req);
        assert!(decision.allowed);
    }

    // --- Read-only bypass across modes ---

    #[test]
    fn read_only_allowed_in_all_modes() {
        for mode in [PermissionMode::Default, PermissionMode::Plan, PermissionMode::FullAuto] {
            let checker = PermissionChecker::new(settings(mode));
            let decision = checker.evaluate(&request("any_tool", true));
            assert!(
                decision.allowed,
                "read-only should be allowed in {:?} mode",
                mode
            );
        }
    }
}

//! Permission mode definitions and decision types.

use serde::{Deserialize, Serialize};

/// Supported permission modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionMode {
    Default,
    Plan,
    FullAuto,
}

impl Default for PermissionMode {
    fn default() -> Self {
        Self::Default
    }
}

impl std::fmt::Display for PermissionMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Default => write!(f, "default"),
            Self::Plan => write!(f, "plan"),
            Self::FullAuto => write!(f, "full_auto"),
        }
    }
}

/// Result of checking whether a tool invocation may run.
#[derive(Debug, Clone)]
pub struct PermissionDecision {
    pub allowed: bool,
    pub requires_confirmation: bool,
    pub reason: String,
}

impl PermissionDecision {
    pub fn allow(reason: impl Into<String>) -> Self {
        Self {
            allowed: true,
            requires_confirmation: false,
            reason: reason.into(),
        }
    }

    pub fn deny(reason: impl Into<String>) -> Self {
        Self {
            allowed: false,
            requires_confirmation: false,
            reason: reason.into(),
        }
    }

    pub fn confirm(reason: impl Into<String>) -> Self {
        Self {
            allowed: false,
            requires_confirmation: true,
            reason: reason.into(),
        }
    }
}

/// A glob-based path permission rule.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PathRule {
    pub pattern: String,
    pub allow: bool,
}

/// A permission check request.
#[derive(Debug)]
pub struct PermissionRequest<'a> {
    pub tool_name: &'a str,
    pub is_read_only: bool,
    pub file_path: Option<&'a str>,
    pub command: Option<&'a str>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_permission_mode_default() {
        let mode = PermissionMode::default();
        assert_eq!(mode, PermissionMode::Default);
    }

    #[test]
    fn test_permission_mode_display() {
        assert_eq!(format!("{}", PermissionMode::Default), "default");
        assert_eq!(format!("{}", PermissionMode::Plan), "plan");
        assert_eq!(format!("{}", PermissionMode::FullAuto), "full_auto");
    }

    #[test]
    fn test_permission_mode_serde_roundtrip() {
        for mode in [PermissionMode::Default, PermissionMode::Plan, PermissionMode::FullAuto] {
            let json = serde_json::to_string(&mode).unwrap();
            let deser: PermissionMode = serde_json::from_str(&json).unwrap();
            assert_eq!(deser, mode);
        }
    }

    #[test]
    fn test_permission_mode_serde_values() {
        assert_eq!(serde_json::to_string(&PermissionMode::Default).unwrap(), "\"default\"");
        assert_eq!(serde_json::to_string(&PermissionMode::Plan).unwrap(), "\"plan\"");
        assert_eq!(serde_json::to_string(&PermissionMode::FullAuto).unwrap(), "\"full_auto\"");
    }

    #[test]
    fn test_permission_decision_allow() {
        let decision = PermissionDecision::allow("safe tool");
        assert!(decision.allowed);
        assert!(!decision.requires_confirmation);
        assert_eq!(decision.reason, "safe tool");
    }

    #[test]
    fn test_permission_decision_deny() {
        let decision = PermissionDecision::deny("blocked");
        assert!(!decision.allowed);
        assert!(!decision.requires_confirmation);
        assert_eq!(decision.reason, "blocked");
    }

    #[test]
    fn test_permission_decision_confirm() {
        let decision = PermissionDecision::confirm("needs approval");
        assert!(!decision.allowed);
        assert!(decision.requires_confirmation);
        assert_eq!(decision.reason, "needs approval");
    }

    #[test]
    fn test_path_rule_serde_roundtrip() {
        let rule = PathRule {
            pattern: "*.rs".into(),
            allow: true,
        };
        let json = serde_json::to_string(&rule).unwrap();
        let deser: PathRule = serde_json::from_str(&json).unwrap();
        assert_eq!(deser.pattern, "*.rs");
        assert!(deser.allow);
    }
}

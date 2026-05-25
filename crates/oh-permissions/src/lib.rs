//! Permission checking for tool execution in OpenHarness.

use globset::{GlobSet, GlobSetBuilder};
use oh_config::PermissionSettings;
use oh_types::permissions::{PathRule, PermissionDecision, PermissionMode, PermissionRequest};
use opentelemetry::KeyValue;
use std::sync::OnceLock;
use tracing::instrument;

// ---------------------------------------------------------------------------
// Hardcoded sensitive-path blocklist
// ---------------------------------------------------------------------------

/// Glob patterns for paths that are **always** denied, regardless of the
/// configured permission mode or any user-supplied allow rules.
///
/// The list covers SSH keys, cloud-provider credential files, GPG keys, Docker
/// and Kubernetes configs, and common secret-bearing files (.env, *.pem, etc.).
/// It is checked *before* any user-configured path rules and *before*
/// FullAuto / read-only bypass logic, so an LLM cannot be tricked into
/// accessing these files even via prompt injection.
///
/// Users who genuinely need to access a specific sensitive path can add a
/// fine-grained glob to `PermissionSettings::allow_sensitive_override`.
const SENSITIVE_PATH_PATTERNS: &[&str] = &[
    // SSH keys and agent sockets
    "*/.ssh/*",
    "*/.ssh",
    // AWS credentials
    "*/.aws/credentials",
    "*/.aws/config",
    // GCP / Google credentials
    "*/.config/gcloud/credentials.json",
    "*/.config/gcloud/application_default_credentials.json",
    // Azure credentials
    "*/.azure/*",
    // GPG keyring
    "*/.gnupg/*",
    // Docker credentials
    "*/.docker/config.json",
    // Kubernetes credentials
    "*/.kube/config",
    // OpenHarness own credential stores
    "*/.openharness/credentials.json",
    "*/.openharness/copilot_auth.json",
    // Raw private keys (any directory)
    "**/id_rsa",
    "**/id_ed25519",
    "**/id_ecdsa",
    "**/id_dsa",
    "**/*.pem",
    "**/*.key",
    // Environment / secret variable files
    "**/.env",
    "**/.env.*",
    // Generic credential / service-account JSON files
    "**/credentials.json",
    "**/service-account*.json",
    // macOS keychain
    "**/login.keychain-db",
    "**/login.keychain",
    // 1Password / Bitwarden local vaults
    "**/*.1pux",
    "**/*.bitwarden.json",
    // netrc (often stores passwords)
    "**/.netrc",
    // Git credentials helper store
    "**/.git-credentials",
    // GitHub CLI auth
    "**/.config/gh/hosts.yml",
    // npm authentication tokens
    "**/.npmrc",
    // PyPI upload credentials
    "**/.pypirc",
    // Terraform credentials
    "**/.terraformrc",
    "**/terraform.rc",
    "**/.terraform.d/credentials.tfrc.json",
    // PostgreSQL password file
    "**/.pgpass",
    // MySQL client password file
    "**/.my.cnf",
    // AWS/S3 CLI tools credential stores
    "**/.s3cfg",
    "**/.boto",
    // PKCS#12 / PFX certificate bundles (contain private key)
    "**/*.p12",
    "**/*.pfx",
    // direnv secret files (commonly hold tokens)
    "**/.envrc",
];

/// Compile `SENSITIVE_PATH_PATTERNS` into a single `GlobSet` once.
fn sensitive_globset() -> &'static GlobSet {
    static SENSITIVE: OnceLock<GlobSet> = OnceLock::new();
    SENSITIVE.get_or_init(|| {
        let mut builder = GlobSetBuilder::new();
        for pat in SENSITIVE_PATH_PATTERNS {
            // Unwrap is safe: all patterns are compile-time constants that are
            // valid globset syntax.
            builder.add(globset::Glob::new(pat).expect("invalid sensitive-path glob"));
        }
        builder.build().expect("failed to build sensitive globset")
    })
}

// ---------------------------------------------------------------------------
// PermissionChecker
// ---------------------------------------------------------------------------

/// Evaluate tool usage against the configured permission mode and rules.
pub struct PermissionChecker {
    settings: PermissionSettings,
    path_rules: Vec<PathRule>,
    /// Compiled override globset (may be empty when no overrides are configured).
    override_globset: Option<GlobSet>,
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

        let override_globset = if settings.allow_sensitive_override.is_empty() {
            None
        } else {
            let mut builder = GlobSetBuilder::new();
            for pat in &settings.allow_sensitive_override {
                if let Ok(g) = globset::Glob::new(pat) {
                    builder.add(g);
                }
            }
            builder.build().ok()
        };

        Self {
            settings,
            path_rules,
            override_globset,
        }
    }

    // -----------------------------------------------------------------------
    // Sensitive-path check (public for testing/introspection)
    // -----------------------------------------------------------------------

    /// Return `true` if `path` matches any hardcoded sensitive-path pattern
    /// AND is not whitelisted by `allow_sensitive_override`.
    pub fn is_sensitive_path(&self, path: &str) -> bool {
        if !sensitive_globset().is_match(path) {
            return false;
        }
        // Override check: if the user has explicitly allowlisted this path,
        // it is no longer treated as sensitive.
        if let Some(ref ov) = self.override_globset {
            if ov.is_match(path) {
                return false;
            }
        }
        true
    }

    // -----------------------------------------------------------------------
    // Main evaluation
    // -----------------------------------------------------------------------

    /// Return whether the tool may run immediately.
    #[instrument(skip(self), fields(tool = %request.tool_name, mode = ?self.settings.mode))]
    pub fn evaluate(&self, request: &PermissionRequest) -> PermissionDecision {
        oh_telemetry::PERMISSION_CHECK_TOTAL
            .add(1, &[KeyValue::new("tool", request.tool_name.to_string())]);

        // ------------------------------------------------------------------
        // 1. Sensitive-path check — runs FIRST, before anything else.
        //    Applies in every mode, including FullAuto, and also to read-only
        //    tools.  This is the whole point of the feature.
        // ------------------------------------------------------------------
        if let Some(file_path) = request.file_path {
            if self.is_sensitive_path(file_path) {
                oh_telemetry::PERMISSION_DENIED_COUNT
                    .add(1, &[KeyValue::new("tool", request.tool_name.to_string())]);
                return PermissionDecision::deny(format!(
                    "Access to sensitive path denied: {}",
                    file_path
                ));
            }
        }

        // ------------------------------------------------------------------
        // 2. Explicit tool deny list
        // ------------------------------------------------------------------
        if self
            .settings
            .denied_tools
            .iter()
            .any(|t| t == request.tool_name)
        {
            oh_telemetry::PERMISSION_DENIED_COUNT
                .add(1, &[KeyValue::new("tool", request.tool_name.to_string())]);
            return PermissionDecision::deny(format!("{} is explicitly denied", request.tool_name));
        }

        // ------------------------------------------------------------------
        // 3. Explicit tool allow list
        // ------------------------------------------------------------------
        if self
            .settings
            .allowed_tools
            .iter()
            .any(|t| t == request.tool_name)
        {
            return PermissionDecision::allow(format!(
                "{} is explicitly allowed",
                request.tool_name
            ));
        }

        // ------------------------------------------------------------------
        // 4. Check path-level rules
        // ------------------------------------------------------------------
        if let Some(file_path) = request.file_path {
            for rule in &self.path_rules {
                if glob_match(&rule.pattern, file_path) && !rule.allow {
                    oh_telemetry::PERMISSION_DENIED_COUNT
                        .add(1, &[KeyValue::new("tool", request.tool_name.to_string())]);
                    return PermissionDecision::deny(format!(
                        "Path {} matches deny rule: {}",
                        file_path, rule.pattern
                    ));
                }
            }
        }

        // ------------------------------------------------------------------
        // 5. Check command deny patterns
        // ------------------------------------------------------------------
        if let Some(command) = request.command {
            for pattern in &self.settings.denied_commands {
                if glob_match(pattern, command) {
                    oh_telemetry::PERMISSION_DENIED_COUNT
                        .add(1, &[KeyValue::new("tool", request.tool_name.to_string())]);
                    return PermissionDecision::deny(format!(
                        "Command matches deny pattern: {}",
                        pattern
                    ));
                }
            }
        }

        // ------------------------------------------------------------------
        // 6. Full auto: allow everything (non-sensitive paths already passed)
        // ------------------------------------------------------------------
        if self.settings.mode == PermissionMode::FullAuto {
            return PermissionDecision::allow("Auto mode allows all tools");
        }

        // ------------------------------------------------------------------
        // 7. Read-only tools always allowed
        // ------------------------------------------------------------------
        if request.is_read_only {
            return PermissionDecision::allow("read-only tools are allowed");
        }

        // ------------------------------------------------------------------
        // 8. Plan mode: block mutating tools
        // ------------------------------------------------------------------
        if self.settings.mode == PermissionMode::Plan {
            oh_telemetry::PERMISSION_DENIED_COUNT
                .add(1, &[KeyValue::new("tool", request.tool_name.to_string())]);
            return PermissionDecision::deny(
                "Plan mode blocks mutating tools until the user exits plan mode",
            );
        }

        // ------------------------------------------------------------------
        // 9. Default mode: require confirmation for mutating tools
        // ------------------------------------------------------------------
        PermissionDecision::confirm("Mutating tools require user confirmation in default mode")
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
        assert!(
            !decision.allowed,
            "deny list should take precedence over allow list"
        );
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
        for mode in [
            PermissionMode::Default,
            PermissionMode::Plan,
            PermissionMode::FullAuto,
        ] {
            let checker = PermissionChecker::new(settings(mode));
            let decision = checker.evaluate(&request("any_tool", true));
            assert!(
                decision.allowed,
                "read-only should be allowed in {:?} mode",
                mode
            );
        }
    }

    // =========================================================================
    // Sensitive-path blocklist tests
    // =========================================================================

    fn req_with_path<'a>(tool: &'a str, read_only: bool, path: &'a str) -> PermissionRequest<'a> {
        PermissionRequest {
            tool_name: tool,
            is_read_only: read_only,
            file_path: Some(path),
            command: None,
        }
    }

    // --- ~/.ssh/id_rsa denied even in FullAuto (read) ---

    #[test]
    fn sensitive_ssh_id_rsa_denied_read_full_auto() {
        let checker = PermissionChecker::new(settings(PermissionMode::FullAuto));
        let decision =
            checker.evaluate(&req_with_path("read_file", true, "/home/alice/.ssh/id_rsa"));
        assert!(
            !decision.allowed,
            "id_rsa should be denied in FullAuto read"
        );
        assert!(
            decision.reason.contains("sensitive"),
            "reason should mention sensitive path, got: {}",
            decision.reason
        );
    }

    // --- ~/.aws/credentials denied for write ---

    #[test]
    fn sensitive_aws_credentials_denied_write() {
        let checker = PermissionChecker::new(settings(PermissionMode::FullAuto));
        let decision = checker.evaluate(&req_with_path(
            "write_file",
            false,
            "/home/alice/.aws/credentials",
        ));
        assert!(
            !decision.allowed,
            ".aws/credentials should be denied for write"
        );
    }

    // --- .env in project dir denied read ---

    #[test]
    fn sensitive_dotenv_denied_read() {
        let checker = PermissionChecker::new(settings(PermissionMode::FullAuto));
        let decision = checker.evaluate(&req_with_path(
            "read_file",
            true,
            "/home/alice/projects/myapp/.env",
        ));
        assert!(!decision.allowed, ".env should be denied for read");
    }

    // --- Sensitive path denied in Default mode too ---

    #[test]
    fn sensitive_path_denied_in_default_mode() {
        let checker = PermissionChecker::new(settings(PermissionMode::Default));
        let decision = checker.evaluate(&req_with_path(
            "read_file",
            true,
            "/home/alice/.ssh/id_ed25519",
        ));
        assert!(!decision.allowed);
    }

    // --- Sensitive path denied in Plan mode ---

    #[test]
    fn sensitive_path_denied_in_plan_mode() {
        let checker = PermissionChecker::new(settings(PermissionMode::Plan));
        let decision = checker.evaluate(&req_with_path(
            "read_file",
            true,
            "/home/alice/.gnupg/private-keys-v1.d/key.gpg",
        ));
        assert!(!decision.allowed);
    }

    // --- *.pem denied ---

    #[test]
    fn sensitive_pem_denied() {
        let checker = PermissionChecker::new(settings(PermissionMode::FullAuto));
        let decision = checker.evaluate(&req_with_path(
            "read_file",
            true,
            "/etc/ssl/private/server.pem",
        ));
        assert!(!decision.allowed);
    }

    // --- Allowlist override: ~/.ssh/known_hosts is readable when overridden ---

    #[test]
    fn sensitive_override_allows_known_hosts() {
        let s = PermissionSettings {
            mode: PermissionMode::FullAuto,
            allow_sensitive_override: vec!["**/.ssh/known_hosts".into()],
            ..Default::default()
        };
        let checker = PermissionChecker::new(s);
        let decision = checker.evaluate(&req_with_path(
            "read_file",
            true,
            "/home/alice/.ssh/known_hosts",
        ));
        assert!(
            decision.allowed,
            "known_hosts should be allowed when explicitly overridden; got: {}",
            decision.reason
        );
    }

    // --- Override does NOT lift the block on other SSH files ---

    #[test]
    fn sensitive_override_does_not_affect_other_ssh_files() {
        let s = PermissionSettings {
            mode: PermissionMode::FullAuto,
            allow_sensitive_override: vec!["**/.ssh/known_hosts".into()],
            ..Default::default()
        };
        let checker = PermissionChecker::new(s);
        // id_rsa is NOT in the override list — must still be denied
        let decision =
            checker.evaluate(&req_with_path("read_file", true, "/home/alice/.ssh/id_rsa"));
        assert!(!decision.allowed, "id_rsa should still be denied");
    }

    // --- Non-sensitive paths are unaffected ---

    #[test]
    fn non_sensitive_tmp_file_allowed_full_auto() {
        let checker = PermissionChecker::new(settings(PermissionMode::FullAuto));
        let decision = checker.evaluate(&req_with_path("read_file", true, "/tmp/foo.txt"));
        assert!(decision.allowed, "/tmp/foo.txt should not be sensitive");
    }

    #[test]
    fn non_sensitive_documents_allowed_full_auto() {
        let checker = PermissionChecker::new(settings(PermissionMode::FullAuto));
        let decision = checker.evaluate(&req_with_path(
            "read_file",
            true,
            "/home/alice/Documents/notes.md",
        ));
        assert!(
            decision.allowed,
            "~/Documents/notes.md should not be sensitive"
        );
    }

    // --- is_sensitive_path unit tests ---

    #[test]
    fn is_sensitive_path_detects_ssh_dir() {
        let checker = PermissionChecker::new(settings(PermissionMode::Default));
        assert!(checker.is_sensitive_path("/home/user/.ssh/config"));
        assert!(checker.is_sensitive_path("/root/.ssh/authorized_keys"));
    }

    #[test]
    fn is_sensitive_path_detects_env_files() {
        let checker = PermissionChecker::new(settings(PermissionMode::Default));
        assert!(checker.is_sensitive_path("/app/.env"));
        assert!(checker.is_sensitive_path("/app/.env.production"));
        assert!(checker.is_sensitive_path("/app/.env.local"));
    }

    #[test]
    fn is_sensitive_path_detects_service_account_json() {
        let checker = PermissionChecker::new(settings(PermissionMode::Default));
        assert!(checker.is_sensitive_path("/home/user/service-account-prod.json"));
    }

    #[test]
    fn is_sensitive_path_clear_for_normal_files() {
        let checker = PermissionChecker::new(settings(PermissionMode::Default));
        assert!(!checker.is_sensitive_path("/home/user/main.rs"));
        assert!(!checker.is_sensitive_path("/tmp/test.txt"));
        assert!(!checker.is_sensitive_path("/var/log/app.log"));
    }

    #[test]
    fn is_sensitive_path_override_applies() {
        let s = PermissionSettings {
            allow_sensitive_override: vec!["**/.ssh/known_hosts".into()],
            ..Default::default()
        };
        let checker = PermissionChecker::new(s);
        assert!(
            !checker.is_sensitive_path("/home/user/.ssh/known_hosts"),
            "known_hosts should not be sensitive when overridden"
        );
        assert!(
            checker.is_sensitive_path("/home/user/.ssh/id_rsa"),
            "id_rsa should still be sensitive"
        );
    }
}

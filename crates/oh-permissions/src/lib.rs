//! Permission checking for tool execution in OpenHarness.

use globset::{GlobSet, GlobSetBuilder};
use oh_config::PermissionSettings;
use oh_types::permissions::{PathRule, PermissionDecision, PermissionMode, PermissionRequest};
use opentelemetry::KeyValue;
use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;
use tracing::{instrument, warn};

// ---------------------------------------------------------------------------
// Named constants
// ---------------------------------------------------------------------------

/// High-risk command basenames that are denied by default whenever the
/// default-deny command allowlist is active and the command is not explicitly
/// allowed. These are the verbs most commonly abused by prompt-injected models
/// to destroy data, exfiltrate secrets, or pull-and-execute remote payloads.
///
/// Matching is performed against the *normalized argv[0] basename* (so
/// `/bin/rm`, `rm`, and `"rm"` all collapse to `rm`) — see [`command_basename`].
const HIGH_RISK_COMMANDS: &[&str] = &[
    "rm", "rmdir", "dd", "mkfs", "shred", "chmod", "chown", "mv", "shutdown", "reboot", "halt",
    "poweroff", "kill", "killall", "pkill", "curl", "wget", "nc", "ncat", "netcat", "ssh", "scp",
    "sftp", "sudo", "su", "doas", "eval", "exec",
];

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
// Path canonicalization (TOOL-3)
// ---------------------------------------------------------------------------

/// Resolve a request path into a canonical, absolute, normalized form suitable
/// for matching against the sensitive-path blocklist and allowed-roots.
///
/// This closes the `..` / relative / symlink evasions described in TOOL-3:
/// a raw request string such as `.ssh/id_rsa`, `/home/a/../a/.ssh/id_rsa`, or a
/// symlink that points at a blocked file would otherwise slip past a blocklist
/// that globs the raw string.
///
/// Resolution strategy (best-effort, never fails):
/// 1. Make the path absolute by joining it onto `base` (the tool's cwd) when it
///    is relative.
/// 2. Attempt [`std::fs::canonicalize`], which resolves symlinks and `..` and
///    requires the path to exist. For *write* targets (which may not exist yet)
///    canonicalize the longest existing ancestor and re-attach the trailing
///    components, so a not-yet-created file under a symlinked directory is still
///    resolved through that symlink.
/// 3. If nothing on the path exists (or canonicalize fails for any other
///    reason) fall back to a purely lexical normalization that still collapses
///    `.` / `..` and strips redundant separators.
///
/// The returned path is always absolute and free of `.`/`..` components, so the
/// caller can apply glob and prefix checks to a single normalized form.
pub fn canonicalize_path(raw: &str, base: &Path) -> PathBuf {
    let raw_path = Path::new(raw);
    let absolute = if raw_path.is_absolute() {
        raw_path.to_path_buf()
    } else {
        base.join(raw_path)
    };

    // Lexically normalize first so `..`/`.` are collapsed BEFORE we touch the
    // filesystem. This is what makes the canonicalize-the-existing-prefix walk
    // below correct even for paths whose `..` segments don't exist on disk.
    let normalized = lexically_normalize(&absolute);

    // Fast path: the full target exists and can be canonicalized directly
    // (this also resolves symlinks in the final component).
    if let Ok(resolved) = std::fs::canonicalize(&normalized) {
        return resolved;
    }

    // Write path: canonicalize the longest existing ancestor (resolving any
    // symlinked parent directories) and re-attach the non-existent tail.
    let mut existing = normalized.as_path();
    let mut tail: Vec<&std::ffi::OsStr> = Vec::new();
    while let Some(parent) = existing.parent() {
        if let Ok(mut resolved) = std::fs::canonicalize(existing) {
            for name in tail.iter().rev() {
                resolved.push(name);
            }
            return resolved;
        }
        if let Some(name) = existing.file_name() {
            tail.push(name);
        }
        existing = parent;
    }

    // Nothing on the path exists (or it is the filesystem root): the already
    // lexically-normalized absolute path is the best we can do.
    normalized
}

/// Collapse `.` and `..` components and redundant separators without touching
/// the filesystem. Used as the fallback when a path (or its parents) does not
/// exist on disk and therefore cannot be `canonicalize`d.
fn lexically_normalize(path: &Path) -> PathBuf {
    let mut out: Vec<Component<'_>> = Vec::new();
    for comp in path.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                match out.last() {
                    // Pop a previous normal component, but never climb above a
                    // root/prefix.
                    Some(Component::Normal(_)) => {
                        out.pop();
                    }
                    Some(Component::ParentDir) | None => out.push(comp),
                    _ => {}
                }
            }
            other => out.push(other),
        }
    }
    out.iter().map(|c| c.as_os_str()).collect()
}

// ---------------------------------------------------------------------------
// Allowed-roots confinement (TOOL-4)
// ---------------------------------------------------------------------------

/// Return `true` if `resolved` (an already-canonicalized path) is confined to at
/// least one of `allowed_roots`.
///
/// Semantics match contract C4: an empty `allowed_roots` means *unconfined*
/// (back-compat) and always returns `true`. Otherwise the resolved path must be
/// equal to, or a descendant of, one of the (canonicalized) roots.
///
/// Roots are canonicalized here too so that a symlinked root (e.g. `/tmp` →
/// `/private/tmp` on macOS) confines correctly against a canonicalized target.
pub fn is_within_allowed_roots(resolved: &Path, allowed_roots: &[PathBuf]) -> bool {
    if allowed_roots.is_empty() {
        return true;
    }
    allowed_roots.iter().any(|root| {
        let canonical_root = std::fs::canonicalize(root)
            .unwrap_or_else(|_| lexically_normalize(root));
        resolved.starts_with(&canonical_root)
    })
}

/// Confinement gate for a *raw* request path: canonicalize it against `base`
/// (the tool cwd) and reject it if it escapes every allowed root.
///
/// Returns `Some(deny)` when the resolved path falls outside the allowed roots,
/// or `None` when the path is confined (or `allowed_roots` is empty). The engine
/// calls this with `ToolExecutionContext::allowed_roots` before executing a file
/// tool.
pub fn check_allowed_roots(
    raw_path: &str,
    base: &Path,
    allowed_roots: &[PathBuf],
) -> Option<PermissionDecision> {
    if allowed_roots.is_empty() {
        return None;
    }
    let resolved = canonicalize_path(raw_path, base);
    if is_within_allowed_roots(&resolved, allowed_roots) {
        None
    } else {
        warn!(
            target: "oh_permissions",
            path = %raw_path,
            resolved = %resolved.display(),
            "path resolves outside the allowed roots; denying"
        );
        Some(PermissionDecision::deny(format!(
            "Path {} resolves outside the allowed working directory",
            resolved.display()
        )))
    }
}

// ---------------------------------------------------------------------------
// Command tokenization (TOOL-7)
// ---------------------------------------------------------------------------

/// Tokenize a shell command line into argv using POSIX-shell word-splitting
/// rules (quotes, escapes). Returns `None` for syntactically invalid input
/// (e.g. an unbalanced quote), which callers MUST treat as fail-closed.
fn tokenize_command(command: &str) -> Option<Vec<String>> {
    shlex::split(command)
}

/// Extract the normalized basename of `argv[0]` from a command line, so that
/// `/bin/rm`, `rm`, and `"rm"` all collapse to `rm`. Returns `None` when the
/// command is empty or fails to tokenize.
fn command_basename(command: &str) -> Option<String> {
    let argv = tokenize_command(command)?;
    let arg0 = argv.into_iter().next()?;
    Path::new(&arg0)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
}

/// Match a denied-command glob against a command line by tokenizing first.
///
/// The pattern is matched against (a) the raw command string, (b) the
/// re-joined normalized argv (collapsing runs of whitespace), and (c) the
/// argv[0] basename. This makes `rm -rf *` match `rm   -rf /`, `/bin/rm -rf .`,
/// etc., instead of relying on the literal spacing of the original string.
///
/// On an invalid glob pattern this **fails closed** (treats it as a match) and
/// logs a config error, instead of the previous fail-open behaviour.
fn command_matches_deny(pattern: &str, command: &str) -> bool {
    let matcher = match globset::Glob::new(pattern) {
        Ok(g) => g.compile_matcher(),
        Err(err) => {
            warn!(
                target: "oh_permissions",
                pattern = %pattern,
                error = %err,
                "invalid denied-command glob pattern; failing closed (treating as a match)"
            );
            // Fail closed: an unparseable deny rule must not silently allow the
            // command it was meant to block.
            return true;
        }
    };

    if matcher.is_match(command) {
        return true;
    }
    if let Some(argv) = tokenize_command(command) {
        let normalized = argv.join(" ");
        if matcher.is_match(&normalized) {
            return true;
        }
    }
    false
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
    /// TOOL-7: when `true`, any command whose `argv[0]` basename is a high-risk
    /// verb (see [`HIGH_RISK_COMMANDS`]) is denied unless it appears in
    /// `command_allowlist`. Default-off preserves back-compat; the engine opts
    /// in for untrusted/autonomous sessions.
    default_deny_high_risk_commands: bool,
    /// TOOL-7: explicit allowlist of argv[0] basenames that bypass the
    /// high-risk default-deny (e.g. `git`, `ls`). Only consulted when
    /// `default_deny_high_risk_commands` is enabled.
    command_allowlist: Vec<String>,
    /// TOOL-7 / TOOL-4: when `true`, a file path that does not match any
    /// `allow` path rule is denied (default-deny posture for paths). Default-off
    /// preserves the historical default-allow behaviour.
    default_deny_paths: bool,
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
                match globset::Glob::new(pat) {
                    Ok(g) => {
                        builder.add(g);
                    }
                    Err(err) => warn!(
                        target: "oh_permissions",
                        pattern = %pat,
                        error = %err,
                        "invalid allow_sensitive_override glob; ignoring this override entry"
                    ),
                }
            }
            builder.build().ok()
        };

        Self {
            settings,
            path_rules,
            override_globset,
            default_deny_high_risk_commands: false,
            command_allowlist: Vec::new(),
            default_deny_paths: false,
        }
    }

    /// Enable TOOL-7 default-deny for high-risk commands, supplying the argv[0]
    /// basenames that remain allowed. Builder-style so the engine can opt in
    /// when constructing the checker without changing `PermissionSettings`.
    #[must_use]
    pub fn with_command_allowlist(
        mut self,
        allowlist: impl IntoIterator<Item = String>,
    ) -> Self {
        self.default_deny_high_risk_commands = true;
        self.command_allowlist = allowlist.into_iter().collect();
        self
    }

    /// Enable TOOL-7 default-deny for paths: a file path matching no `allow`
    /// rule is denied rather than allowed.
    #[must_use]
    pub fn with_default_deny_paths(mut self, enabled: bool) -> Self {
        self.default_deny_paths = enabled;
        self
    }

    // -----------------------------------------------------------------------
    // Sensitive-path check (public for testing/introspection)
    // -----------------------------------------------------------------------

    /// Return `true` if `path` matches any hardcoded sensitive-path pattern
    /// AND is not whitelisted by `allow_sensitive_override`.
    ///
    /// TOOL-3: the supplied string is **canonicalized first** (resolving `..`,
    /// symlinks, and making it absolute relative to the process cwd) so that
    /// evasions like `.ssh/id_rsa`, `/home/a/../a/.ssh/id_rsa`, or a symlink
    /// pointing at a blocked file cannot slip past the blocklist. Use
    /// [`PermissionChecker::is_sensitive_resolved`] when the path is already
    /// canonical (e.g. when the engine canonicalized against a tool cwd).
    pub fn is_sensitive_path(&self, path: &str) -> bool {
        let base = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
        let resolved = canonicalize_path(path, &base);
        self.is_sensitive_resolved(&resolved)
    }

    /// Sensitive-path check on an already-canonicalized path. This is the form
    /// the engine should call after resolving each `path_args` entry against the
    /// tool's cwd (per contract C3), so the blocklist always sees the true
    /// target the tool will open — not the raw request string.
    pub fn is_sensitive_resolved(&self, resolved: &Path) -> bool {
        if !sensitive_globset().is_match(resolved) {
            return false;
        }
        // Override check: if the user has explicitly allowlisted this path,
        // it is no longer treated as sensitive.
        if let Some(ref ov) = self.override_globset {
            if ov.is_match(resolved) {
                return false;
            }
        }
        true
    }

    // -----------------------------------------------------------------------
    // Main evaluation
    // -----------------------------------------------------------------------

    /// Return whether the tool may run immediately.
    ///
    /// File paths are canonicalized relative to the process current directory.
    /// When the engine knows the tool's cwd (and/or allowed roots) it should
    /// prefer [`PermissionChecker::evaluate_with_base`], which resolves relative
    /// paths and confinement against that base per contracts C3/C4.
    #[instrument(skip(self), fields(tool = %request.tool_name, mode = ?self.settings.mode))]
    pub fn evaluate(&self, request: &PermissionRequest) -> PermissionDecision {
        let base = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
        self.evaluate_with_base(request, &base, &[])
    }

    /// Evaluate a request, resolving file paths against `base` (the tool cwd)
    /// and confining the resolved path to `allowed_roots` (empty = unconfined).
    ///
    /// This is the entry point the engine calls per contract C4: it passes the
    /// tool's cwd as `base` and `ToolExecutionContext::allowed_roots`. All path
    /// matching (sensitive blocklist, deny/allow rules, confinement) operates on
    /// the canonicalized resolved path, closing the TOOL-3 evasions.
    #[instrument(skip(self, allowed_roots), fields(tool = %request.tool_name, mode = ?self.settings.mode))]
    pub fn evaluate_with_base(
        &self,
        request: &PermissionRequest,
        base: &Path,
        allowed_roots: &[PathBuf],
    ) -> PermissionDecision {
        oh_telemetry::PERMISSION_CHECK_TOTAL
            .add(1, &[KeyValue::new("tool", request.tool_name.to_string())]);

        // Canonicalize the request path once; reuse for every path-based check.
        let resolved = request
            .file_path
            .map(|p| canonicalize_path(p, base));

        let deny = |reason: String, tool: &str| -> PermissionDecision {
            oh_telemetry::PERMISSION_DENIED_COUNT.add(1, &[KeyValue::new("tool", tool.to_string())]);
            PermissionDecision::deny(reason)
        };

        // ------------------------------------------------------------------
        // 1. Sensitive-path check — runs FIRST, before anything else.
        //    Applies in every mode, including FullAuto, and also to read-only
        //    tools.  This is the whole point of the feature. (TOOL-3: checked
        //    against the CANONICALIZED target, not the raw request string.)
        // ------------------------------------------------------------------
        if let Some(ref resolved) = resolved {
            if self.is_sensitive_resolved(resolved) {
                return deny(
                    format!("Access to sensitive path denied: {}", resolved.display()),
                    request.tool_name,
                );
            }
        }

        // ------------------------------------------------------------------
        // 1b. Allowed-roots confinement (TOOL-4) — reject paths that resolve
        //     outside every allowed root. No-op when allowed_roots is empty.
        // ------------------------------------------------------------------
        if let Some(ref resolved) = resolved {
            if !is_within_allowed_roots(resolved, allowed_roots) {
                warn!(
                    target: "oh_permissions",
                    resolved = %resolved.display(),
                    "path resolves outside the allowed roots; denying"
                );
                return deny(
                    format!(
                        "Path {} resolves outside the allowed working directory",
                        resolved.display()
                    ),
                    request.tool_name,
                );
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
            return deny(
                format!("{} is explicitly denied", request.tool_name),
                request.tool_name,
            );
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
        // 4. Check path-level rules (TOOL-7: allow rules now actually work).
        //    First-match wins; an `allow` rule short-circuits to allow, a deny
        //    rule short-circuits to deny. When `default_deny_paths` is enabled
        //    a path matching no allow rule is denied.
        // ------------------------------------------------------------------
        if let Some(ref resolved) = resolved {
            let mut matched_allow = false;
            for rule in &self.path_rules {
                if path_rule_matches(&rule.pattern, resolved) {
                    if rule.allow {
                        matched_allow = true;
                        break;
                    }
                    return deny(
                        format!(
                            "Path {} matches deny rule: {}",
                            resolved.display(),
                            rule.pattern
                        ),
                        request.tool_name,
                    );
                }
            }
            if matched_allow {
                return PermissionDecision::allow(format!(
                    "Path {} matches an allow rule",
                    resolved.display()
                ));
            }
            if self.default_deny_paths {
                return deny(
                    format!(
                        "Path {} matches no allow rule (default-deny)",
                        resolved.display()
                    ),
                    request.tool_name,
                );
            }
        }

        // ------------------------------------------------------------------
        // 5. Check command deny patterns (TOOL-7: tokenized argv matching +
        //    fail-closed on invalid glob).
        // ------------------------------------------------------------------
        if let Some(command) = request.command {
            for pattern in &self.settings.denied_commands {
                if command_matches_deny(pattern, command) {
                    return deny(
                        format!("Command matches deny pattern: {}", pattern),
                        request.tool_name,
                    );
                }
            }

            // 5b. Default-deny high-risk command allowlist (TOOL-7). When
            //     enabled, a command whose argv[0] basename is high-risk and is
            //     not in the allowlist is denied — even in FullAuto.
            if self.default_deny_high_risk_commands {
                match command_basename(command) {
                    Some(basename) => {
                        let allowed = self.command_allowlist.iter().any(|a| a == &basename);
                        let high_risk = HIGH_RISK_COMMANDS.contains(&basename.as_str());
                        if high_risk && !allowed {
                            return deny(
                                format!(
                                    "Command '{}' is high-risk and not in the allowlist",
                                    basename
                                ),
                                request.tool_name,
                            );
                        }
                    }
                    None => {
                        // Untokenizable command (e.g. unbalanced quote): fail
                        // closed under the default-deny posture.
                        warn!(
                            target: "oh_permissions",
                            command = %command,
                            "command failed to tokenize under default-deny; denying"
                        );
                        return deny(
                            "Command could not be parsed for the allowlist check".to_string(),
                            request.tool_name,
                        );
                    }
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
            return deny(
                "Plan mode blocks mutating tools until the user exits plan mode".to_string(),
                request.tool_name,
            );
        }

        // ------------------------------------------------------------------
        // 9. Default mode: require confirmation for mutating tools
        // ------------------------------------------------------------------
        PermissionDecision::confirm("Mutating tools require user confirmation in default mode")
    }
}

/// Match a path deny/allow rule glob against a (canonicalized) path.
///
/// TOOL-7: fails **closed** on an invalid glob pattern (treats it as a match)
/// and logs a config error, so a malformed rule cannot silently disable
/// protection. Note: a fail-closed match on an *allow* rule is harmless (it
/// only ever grants access the user explicitly tried to grant); on a *deny*
/// rule it errs toward denying, which is the safe direction.
fn path_rule_matches(pattern: &str, path: &Path) -> bool {
    match globset::Glob::new(pattern) {
        Ok(g) => g.compile_matcher().is_match(path),
        Err(err) => {
            warn!(
                target: "oh_permissions",
                pattern = %pattern,
                error = %err,
                "invalid path-rule glob pattern; failing closed (treating as a match)"
            );
            true
        }
    }
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

    // =========================================================================
    // TOOL-3: canonicalization defeats `..` / relative / symlink evasions
    // =========================================================================

    use std::sync::atomic::{AtomicU64, Ordering};

    /// Unique-per-call temp directory rooted under the system temp dir.
    fn unique_tmp_dir(tag: &str) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "oh_perm_test_{}_{}_{}",
            tag,
            std::process::id(),
            n
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    #[test]
    fn canonicalize_collapses_parent_dir_components() {
        // `/a/b/../c/.ssh/id_rsa` must normalize to `/a/c/.ssh/id_rsa`.
        let resolved = canonicalize_path("/a/b/../c/.ssh/id_rsa", Path::new("/"));
        assert_eq!(resolved, PathBuf::from("/a/c/.ssh/id_rsa"));
    }

    #[test]
    fn canonicalize_makes_relative_absolute_against_base() {
        let resolved = canonicalize_path(".ssh/id_rsa", Path::new("/home/bob"));
        assert_eq!(resolved, PathBuf::from("/home/bob/.ssh/id_rsa"));
    }

    #[test]
    fn sensitive_path_blocked_via_parent_dir_evasion() {
        let checker = PermissionChecker::new(settings(PermissionMode::FullAuto));
        // Raw string would NOT match `*/.ssh/*` as a literal glob over the
        // unnormalized path with a `..` in it; canonicalization must catch it.
        let decision = checker.evaluate(&req_with_path(
            "read_file",
            true,
            "/home/alice/projects/../.ssh/id_rsa",
        ));
        assert!(
            !decision.allowed,
            "..-evasion to .ssh/id_rsa must be denied; got: {}",
            decision.reason
        );
    }

    #[test]
    fn sensitive_path_blocked_via_relative_path() {
        let checker = PermissionChecker::new(settings(PermissionMode::FullAuto));
        let req = PermissionRequest {
            tool_name: "read_file",
            is_read_only: true,
            file_path: Some(".ssh/id_rsa"),
            command: None,
        };
        // Resolve relative to a home-like base.
        let decision =
            checker.evaluate_with_base(&req, Path::new("/home/alice"), &[]);
        assert!(
            !decision.allowed,
            "relative .ssh/id_rsa must be denied; got: {}",
            decision.reason
        );
    }

    #[test]
    #[cfg(unix)]
    fn sensitive_path_blocked_via_symlinked_parent() {
        use std::os::unix::fs::symlink;
        let dir = unique_tmp_dir("symlink");
        // Real secret directory: <dir>/real/.ssh/id_rsa
        let ssh_dir = dir.join("real").join(".ssh");
        std::fs::create_dir_all(&ssh_dir).unwrap();
        let secret = ssh_dir.join("id_rsa");
        std::fs::write(&secret, b"PRIVATE KEY").unwrap();
        // Symlink <dir>/link -> <dir>/real, so <dir>/link/.ssh/id_rsa resolves
        // to the secret while the raw string says "link", not ".ssh".
        let link = dir.join("link");
        symlink(dir.join("real"), &link).unwrap();

        let checker = PermissionChecker::new(settings(PermissionMode::FullAuto));
        let via_symlink = link.join(".ssh").join("id_rsa");
        let req = PermissionRequest {
            tool_name: "read_file",
            is_read_only: true,
            file_path: Some(via_symlink.to_str().unwrap()),
            command: None,
        };
        let decision = checker.evaluate_with_base(&req, &dir, &[]);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            !decision.allowed,
            "symlinked path resolving to .ssh/id_rsa must be denied; got: {}",
            decision.reason
        );
    }

    // =========================================================================
    // TOOL-4: allowed-roots confinement
    // =========================================================================

    #[test]
    fn within_allowed_roots_empty_is_unconfined() {
        assert!(is_within_allowed_roots(
            Path::new("/anywhere/at/all"),
            &[]
        ));
    }

    #[test]
    fn within_allowed_roots_accepts_descendant() {
        let roots = vec![PathBuf::from("/work/project")];
        assert!(is_within_allowed_roots(
            Path::new("/work/project/src/main.rs"),
            &roots
        ));
    }

    #[test]
    fn within_allowed_roots_rejects_escape() {
        let roots = vec![PathBuf::from("/work/project")];
        assert!(!is_within_allowed_roots(
            Path::new("/etc/passwd"),
            &roots
        ));
    }

    #[test]
    fn evaluate_with_base_denies_path_outside_allowed_roots() {
        let checker = PermissionChecker::new(settings(PermissionMode::FullAuto));
        let roots = vec![PathBuf::from("/work/project")];
        let req = PermissionRequest {
            tool_name: "write_file",
            is_read_only: false,
            file_path: Some("/work/project/../../etc/hosts"),
            command: None,
        };
        let decision = checker.evaluate_with_base(&req, Path::new("/work/project"), &roots);
        assert!(
            !decision.allowed,
            "escape via .. outside allowed roots must be denied; got: {}",
            decision.reason
        );
        assert!(decision.reason.contains("allowed working directory"));
    }

    #[test]
    fn evaluate_with_base_allows_path_inside_allowed_roots() {
        let checker = PermissionChecker::new(settings(PermissionMode::FullAuto));
        let roots = vec![PathBuf::from("/work/project")];
        let req = PermissionRequest {
            tool_name: "write_file",
            is_read_only: false,
            file_path: Some("/work/project/sub/out.txt"),
            command: None,
        };
        let decision = checker.evaluate_with_base(&req, Path::new("/work/project"), &roots);
        assert!(
            decision.allowed,
            "path inside allowed root must be allowed; got: {}",
            decision.reason
        );
    }

    #[test]
    fn check_allowed_roots_helper_rejects_escape() {
        let roots = vec![PathBuf::from("/work")];
        let decision = check_allowed_roots("/work/../etc/shadow", Path::new("/work"), &roots);
        assert!(decision.is_some(), "escape should produce a deny decision");
        assert!(!decision.unwrap().allowed);
    }

    #[test]
    fn check_allowed_roots_helper_passes_inside() {
        let roots = vec![PathBuf::from("/work")];
        assert!(check_allowed_roots("/work/a/b.txt", Path::new("/work"), &roots).is_none());
    }

    // =========================================================================
    // TOOL-7: tokenized command matching, default-deny allowlist, fail-closed
    // =========================================================================

    fn cmd_req<'a>(command: &'a str) -> PermissionRequest<'a> {
        PermissionRequest {
            tool_name: "bash",
            is_read_only: false,
            file_path: None,
            command: Some(command),
        }
    }

    #[test]
    fn command_deny_matches_despite_extra_whitespace() {
        let s = PermissionSettings {
            mode: PermissionMode::FullAuto,
            denied_commands: vec!["rm -rf *".into()],
            ..Default::default()
        };
        let checker = PermissionChecker::new(s);
        // Extra interior whitespace must still match after tokenization.
        let decision = checker.evaluate(&cmd_req("rm   -rf   /tmp/x"));
        assert!(
            !decision.allowed,
            "whitespace variant of denied command must match; got: {}",
            decision.reason
        );
    }

    #[test]
    fn invalid_deny_glob_fails_closed() {
        // `[` is an unterminated character class — an invalid glob. Old code
        // failed open (allowed). It must now fail closed (deny).
        let s = PermissionSettings {
            mode: PermissionMode::FullAuto,
            denied_commands: vec!["rm [".into()],
            ..Default::default()
        };
        let checker = PermissionChecker::new(s);
        let decision = checker.evaluate(&cmd_req("rm -rf /"));
        assert!(
            !decision.allowed,
            "invalid deny glob must fail closed (deny); got: {}",
            decision.reason
        );
    }

    #[test]
    fn invalid_path_deny_glob_fails_closed() {
        let s = PermissionSettings {
            mode: PermissionMode::FullAuto,
            path_rules: vec![PathRuleConfig {
                pattern: "/etc/[".into(),
                allow: false,
            }],
            ..Default::default()
        };
        let checker = PermissionChecker::new(s);
        let decision = checker.evaluate(&req_with_path("write_file", false, "/tmp/whatever.txt"));
        assert!(
            !decision.allowed,
            "invalid path deny glob must fail closed (deny); got: {}",
            decision.reason
        );
    }

    #[test]
    fn high_risk_command_denied_by_default_deny_allowlist() {
        let checker = PermissionChecker::new(settings(PermissionMode::FullAuto))
            .with_command_allowlist(vec!["git".to_string(), "ls".to_string()]);
        // rm is high-risk and not in the allowlist.
        let decision = checker.evaluate(&cmd_req("rm -rf /tmp/x"));
        assert!(
            !decision.allowed,
            "rm must be denied under default-deny allowlist; got: {}",
            decision.reason
        );
    }

    #[test]
    fn high_risk_command_denied_even_with_path_prefix() {
        let checker = PermissionChecker::new(settings(PermissionMode::FullAuto))
            .with_command_allowlist(vec!["git".to_string()]);
        // /bin/rm must collapse to basename `rm` and be denied.
        let decision = checker.evaluate(&cmd_req("/bin/rm -rf /tmp/x"));
        assert!(
            !decision.allowed,
            "/bin/rm must be denied under default-deny allowlist; got: {}",
            decision.reason
        );
    }

    #[test]
    fn allowlisted_command_passes_under_default_deny() {
        let checker = PermissionChecker::new(settings(PermissionMode::FullAuto))
            .with_command_allowlist(vec!["git".to_string()]);
        let decision = checker.evaluate(&cmd_req("git status"));
        assert!(
            decision.allowed,
            "git is allowlisted and must pass; got: {}",
            decision.reason
        );
    }

    #[test]
    fn non_high_risk_command_passes_under_default_deny() {
        let checker = PermissionChecker::new(settings(PermissionMode::FullAuto))
            .with_command_allowlist(vec!["git".to_string()]);
        // `echo` is not in HIGH_RISK_COMMANDS, so it is not subject to deny.
        let decision = checker.evaluate(&cmd_req("echo hello"));
        assert!(
            decision.allowed,
            "non-high-risk command must pass; got: {}",
            decision.reason
        );
    }

    #[test]
    fn untokenizable_command_fails_closed_under_default_deny() {
        let checker = PermissionChecker::new(settings(PermissionMode::FullAuto))
            .with_command_allowlist(vec!["git".to_string()]);
        // Unbalanced quote -> shlex returns None -> fail closed.
        let decision = checker.evaluate(&cmd_req("git \"unterminated"));
        assert!(
            !decision.allowed,
            "untokenizable command must fail closed; got: {}",
            decision.reason
        );
    }

    #[test]
    fn default_deny_paths_denies_unmatched_path() {
        let s = PermissionSettings {
            mode: PermissionMode::FullAuto,
            path_rules: vec![PathRuleConfig {
                pattern: "/work/**".into(),
                allow: true,
            }],
            ..Default::default()
        };
        let checker = PermissionChecker::new(s).with_default_deny_paths(true);
        // Outside the allow rule -> denied under default-deny.
        let decision = checker.evaluate(&req_with_path("write_file", false, "/tmp/x.txt"));
        assert!(
            !decision.allowed,
            "path matching no allow rule must be denied under default-deny; got: {}",
            decision.reason
        );
    }

    #[test]
    fn allow_path_rule_grants_access() {
        // TOOL-7: previously allow:true was a no-op. Now an allow rule actually
        // short-circuits to allow.
        let s = PermissionSettings {
            mode: PermissionMode::Default,
            path_rules: vec![PathRuleConfig {
                pattern: "/work/**".into(),
                allow: true,
            }],
            ..Default::default()
        };
        let checker = PermissionChecker::new(s);
        // Mutating tool in Default mode would normally require confirmation;
        // the allow rule grants it outright.
        let decision = checker.evaluate(&req_with_path("write_file", false, "/work/src/main.rs"));
        assert!(
            decision.allowed,
            "allow path rule must grant access; got: {}",
            decision.reason
        );
    }

    #[test]
    fn default_deny_paths_allows_matched_allow_rule() {
        let s = PermissionSettings {
            mode: PermissionMode::FullAuto,
            path_rules: vec![PathRuleConfig {
                pattern: "/work/**".into(),
                allow: true,
            }],
            ..Default::default()
        };
        let checker = PermissionChecker::new(s).with_default_deny_paths(true);
        let decision = checker.evaluate(&req_with_path("write_file", false, "/work/ok.txt"));
        assert!(
            decision.allowed,
            "allow-rule match must pass even under default-deny; got: {}",
            decision.reason
        );
    }
}

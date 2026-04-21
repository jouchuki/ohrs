//! Path validation helpers that prevent obvious sandbox escapes.
//!
//! Mounts pointing at sensitive system directories (e.g. `/etc`, `/root`,
//! `$HOME/.ssh`) are rejected before the backend ever sees them.

use std::path::{Path, PathBuf};

use crate::SandboxError;

/// Prefixes that are always refused as mount targets regardless of context.
///
/// The list mirrors the spirit of the Python reference implementation in
/// `sandbox/path_validator.py`, but is intentionally conservative: when in
/// doubt, reject.
static SENSITIVE_PREFIXES: &[&str] = &[
    "/etc",
    "/root",
    "/boot",
    "/sys",
    "/proc",
    "/dev",
    "/run/secrets",
    "/var/run/secrets", // Kubernetes service-account tokens
    "/usr/lib/sudo",
];

/// Suffixes / sub-paths that are always refused regardless of their parent.
static SENSITIVE_SUFFIXES: &[&str] = &[
    ".ssh",
    ".gnupg",
    ".aws",
    ".kube",
    "credentials",
    ".netrc",
    ".pgpass",
];

/// Validate a single path before it is used as a sandbox mount.
///
/// Returns `Ok(())` when the path is acceptable, or
/// `Err(SandboxError::PathValidation(_))` with a human-readable reason.
pub fn validate_mount_path(path: &Path) -> Result<(), SandboxError> {
    // Resolve symlinks where possible; fall back to lexical canonicalisation.
    let resolved = path
        .canonicalize()
        .unwrap_or_else(|_| lexical_canonicalize(path));

    let resolved_str = resolved.to_string_lossy();

    // Check against known sensitive prefixes.
    for prefix in SENSITIVE_PREFIXES {
        if resolved_str == *prefix || resolved_str.starts_with(&format!("{}/", prefix)) {
            return Err(SandboxError::PathValidation(format!(
                "path '{}' is under sensitive prefix '{}'",
                resolved.display(),
                prefix
            )));
        }
    }

    // Check the HOME/.ssh style patterns.
    // Walk all ancestors looking for sensitive directory names.
    for component in resolved.components() {
        let name = component.as_os_str().to_string_lossy();
        for suffix in SENSITIVE_SUFFIXES {
            if name.as_ref() == *suffix {
                return Err(SandboxError::PathValidation(format!(
                    "path '{}' contains sensitive component '{}'",
                    resolved.display(),
                    suffix
                )));
            }
        }
    }

    // Also reject absolute paths that start with $HOME/.ssh even when HOME
    // is non-standard.
    if let Some(home) = home_dir() {
        let ssh_dir = home.join(".ssh");
        let aws_dir = home.join(".aws");
        let kube_dir = home.join(".kube");
        let gnupg_dir = home.join(".gnupg");
        for sensitive in &[ssh_dir, aws_dir, kube_dir, gnupg_dir] {
            if resolved.starts_with(sensitive) {
                return Err(SandboxError::PathValidation(format!(
                    "path '{}' is under sensitive home directory '{}'",
                    resolved.display(),
                    sensitive.display()
                )));
            }
        }
    }

    Ok(())
}

/// Validate all paths in a list, returning the first error encountered.
pub fn validate_mount_paths(paths: &[PathBuf]) -> Result<(), SandboxError> {
    for p in paths {
        validate_mount_path(p)?;
    }
    Ok(())
}

/// Lexicально simplify a path (collapse `.` / `..`) without hitting the FS.
fn lexical_canonicalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

/// Return the current user's home directory (best-effort).
fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn rejects_etc() {
        assert!(validate_mount_path(Path::new("/etc")).is_err());
        assert!(validate_mount_path(Path::new("/etc/passwd")).is_err());
        assert!(validate_mount_path(Path::new("/etc/ssh/sshd_config")).is_err());
    }

    #[test]
    fn rejects_root_home() {
        assert!(validate_mount_path(Path::new("/root")).is_err());
        assert!(validate_mount_path(Path::new("/root/.bashrc")).is_err());
    }

    #[test]
    fn rejects_ssh_dir_anywhere() {
        assert!(validate_mount_path(Path::new("/home/user/.ssh")).is_err());
        assert!(validate_mount_path(Path::new("/home/user/.ssh/id_rsa")).is_err());
    }

    #[test]
    fn rejects_aws_credentials() {
        assert!(validate_mount_path(Path::new("/home/user/.aws")).is_err());
    }

    #[test]
    fn rejects_proc_sys() {
        assert!(validate_mount_path(Path::new("/proc/1/mem")).is_err());
        assert!(validate_mount_path(Path::new("/sys/kernel")).is_err());
    }

    #[test]
    fn accepts_tmp() {
        assert!(validate_mount_path(Path::new("/tmp")).is_ok());
        assert!(validate_mount_path(Path::new("/tmp/workdir")).is_ok());
    }

    #[test]
    fn accepts_home_workspace() {
        // A regular project directory in HOME must be accepted.
        assert!(validate_mount_path(Path::new("/home/user/projects/foo")).is_ok());
    }

    #[test]
    fn accepts_var_log() {
        assert!(validate_mount_path(Path::new("/var/log")).is_ok());
    }

    #[test]
    fn rejects_path_traversal_to_etc() {
        // /tmp/../etc should resolve to /etc and be rejected.
        let path = Path::new("/tmp/../etc/passwd");
        assert!(validate_mount_path(path).is_err());
    }
}

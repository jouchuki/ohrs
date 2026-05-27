//! Path canonicalization and working-directory confinement helpers.
//!
//! This module implements the tool-side primitives for two audit findings:
//!
//! * **TOOL-3** — canonicalize a request path to its real on-disk target before
//!   the syscall so the permission gate (and the tool itself) never operate on a
//!   `..`/relative/symlink-evaded string. The engine gate re-checks; the tool
//!   canonicalizes immediately before touching the filesystem to close the
//!   TOCTOU window as far as is possible in user space.
//! * **TOOL-4** — given a non-empty set of `allowed_roots`, reject any path that
//!   resolves outside *every* root. An empty set means "unconfined" for
//!   backwards compatibility (see contract C4).
//!
//! Canonicalization of a path whose final component does not yet exist (e.g. a
//! `Write` creating a new file) is handled by canonicalizing the deepest
//! existing ancestor and re-appending the trailing components. This lets us
//! confine *creates* without requiring the target to pre-exist.

use std::path::{Component, Path, PathBuf};

/// Error returned when a path cannot be safely resolved or is confined out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathSafetyError {
    /// The path (or its existing ancestor) could not be canonicalized.
    Unresolvable { path: String, reason: String },
    /// The resolved path lies outside every configured allowed root.
    OutsideAllowedRoots { resolved: String },
}

impl std::fmt::Display for PathSafetyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PathSafetyError::Unresolvable { path, reason } => {
                write!(f, "Cannot resolve path '{path}': {reason}")
            }
            PathSafetyError::OutsideAllowedRoots { resolved } => {
                write!(
                    f,
                    "Path '{resolved}' resolves outside the allowed working directory"
                )
            }
        }
    }
}

impl std::error::Error for PathSafetyError {}

/// Resolve `candidate` (which may be relative to `base`) into an absolute,
/// lexically normalized path WITHOUT touching the filesystem.
///
/// This collapses `.` and `..` components purely textually. It is the fallback
/// used when no on-disk canonicalization is possible. It does NOT resolve
/// symlinks — [`canonicalize_target`] does that for the parts that exist.
fn lexically_normalize(base: &Path, candidate: &Path) -> PathBuf {
    let joined = if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        base.join(candidate)
    };

    let mut normalized = PathBuf::new();
    for component in joined.components() {
        match component {
            Component::ParentDir => {
                // Pop the last real segment; never pop past the root.
                if !normalized.pop() {
                    // Defensive: keep going rather than escaping above root.
                }
            }
            Component::CurDir => {}
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

/// Canonicalize the real on-disk target for `candidate`.
///
/// Relative paths are resolved against `base`. Symlinks are followed for every
/// component that exists. If the target itself does not yet exist (a create),
/// the deepest existing ancestor is canonicalized and the remaining components
/// are appended, so symlink evasion through existing parents is still defeated.
pub fn canonicalize_target(base: &Path, candidate: &str) -> Result<PathBuf, PathSafetyError> {
    let candidate_path = Path::new(candidate);
    let absolute = if candidate_path.is_absolute() {
        candidate_path.to_path_buf()
    } else {
        base.join(candidate_path)
    };

    // Fast path: the whole thing already exists.
    if let Ok(real) = std::fs::canonicalize(&absolute) {
        return Ok(real);
    }

    // Walk up to the deepest existing ancestor, canonicalize it, then re-append
    // the trailing (not-yet-existing) components after normalizing them.
    let mut existing = absolute.as_path();
    let mut trailing: Vec<&std::ffi::OsStr> = Vec::new();
    loop {
        match existing.parent() {
            Some(parent) => {
                if let Some(name) = existing.file_name() {
                    trailing.push(name);
                }
                existing = parent;
                if let Ok(real_parent) = std::fs::canonicalize(existing) {
                    let mut resolved = real_parent;
                    for name in trailing.iter().rev() {
                        resolved.push(name);
                    }
                    // Lexically normalize once more to fold any `..` that slipped
                    // into the trailing (non-existent) portion.
                    return Ok(lexically_normalize(Path::new("/"), &resolved));
                }
            }
            None => {
                // No existing ancestor at all (shouldn't happen with an absolute
                // path on a normal filesystem). Fall back to lexical norm.
                return Ok(lexically_normalize(base, candidate_path));
            }
        }
    }
}

/// Returns `true` if `resolved` lies within at least one of `roots`.
///
/// Each root is canonicalized for the comparison; a root that cannot be
/// canonicalized is normalized lexically as a best effort.
fn is_within_roots(resolved: &Path, roots: &[PathBuf]) -> bool {
    roots.iter().any(|root| {
        let real_root = std::fs::canonicalize(root)
            .unwrap_or_else(|_| lexically_normalize(Path::new("/"), root));
        resolved.starts_with(&real_root)
    })
}

/// Resolve `candidate` and enforce `allowed_roots` confinement in one step.
///
/// * Canonicalizes the target (TOOL-3).
/// * If `allowed_roots` is non-empty, rejects targets outside every root
///   (TOOL-4). An empty slice means unconfined.
///
/// Returns the canonical [`PathBuf`] the caller should use for the syscall.
pub fn resolve_and_confine(
    base: &Path,
    candidate: &str,
    allowed_roots: &[PathBuf],
) -> Result<PathBuf, PathSafetyError> {
    let resolved = canonicalize_target(base, candidate)?;

    if !allowed_roots.is_empty() && !is_within_roots(&resolved, allowed_roots) {
        return Err(PathSafetyError::OutsideAllowedRoots {
            resolved: resolved.display().to_string(),
        });
    }

    Ok(resolved)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_lexical_normalize_collapses_parent() {
        let base = Path::new("/home/user");
        let got = lexically_normalize(base, Path::new("a/../b/c"));
        assert_eq!(got, PathBuf::from("/home/user/b/c"));
    }

    #[test]
    fn test_lexical_normalize_does_not_escape_root() {
        let base = Path::new("/");
        let got = lexically_normalize(base, Path::new("../../../etc/passwd"));
        assert_eq!(got, PathBuf::from("/etc/passwd"));
    }

    #[test]
    fn test_canonicalize_existing_file() {
        let dir = tempdir().unwrap();
        let f = dir.path().join("real.txt");
        std::fs::write(&f, "x").unwrap();
        let resolved = canonicalize_target(dir.path(), "real.txt").unwrap();
        assert_eq!(resolved, std::fs::canonicalize(&f).unwrap());
    }

    #[test]
    fn test_canonicalize_nonexistent_target_uses_ancestor() {
        let dir = tempdir().unwrap();
        // Parent exists, file does not.
        let resolved = canonicalize_target(dir.path(), "newfile.txt").unwrap();
        let expected = std::fs::canonicalize(dir.path()).unwrap().join("newfile.txt");
        assert_eq!(resolved, expected);
    }

    #[test]
    fn test_canonicalize_defeats_dotdot_through_existing_parent() {
        let dir = tempdir().unwrap();
        let sub = dir.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        // sub/../escaped.txt should resolve to <dir>/escaped.txt
        let resolved = canonicalize_target(&sub, "../escaped.txt").unwrap();
        let expected = std::fs::canonicalize(dir.path()).unwrap().join("escaped.txt");
        assert_eq!(resolved, expected);
    }

    #[test]
    fn test_confine_allows_path_in_root() {
        let dir = tempdir().unwrap();
        let f = dir.path().join("ok.txt");
        std::fs::write(&f, "x").unwrap();
        let roots = vec![dir.path().to_path_buf()];
        let got = resolve_and_confine(dir.path(), "ok.txt", &roots);
        assert!(got.is_ok());
    }

    #[test]
    fn test_confine_rejects_path_outside_root() {
        let dir = tempdir().unwrap();
        let roots = vec![dir.path().to_path_buf()];
        let err = resolve_and_confine(dir.path(), "/etc/passwd", &roots).unwrap_err();
        assert!(matches!(err, PathSafetyError::OutsideAllowedRoots { .. }));
    }

    #[test]
    fn test_confine_rejects_dotdot_escape() {
        let dir = tempdir().unwrap();
        let sub = dir.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        let roots = vec![sub.clone()];
        // From sub, ../ escapes the confined root.
        let err = resolve_and_confine(&sub, "../escaped.txt", &roots).unwrap_err();
        assert!(matches!(err, PathSafetyError::OutsideAllowedRoots { .. }));
    }

    #[test]
    fn test_empty_roots_is_unconfined() {
        let dir = tempdir().unwrap();
        let roots: Vec<PathBuf> = Vec::new();
        // Absolute path outside dir is allowed when unconfined.
        let got = resolve_and_confine(dir.path(), "/etc", &roots);
        assert!(got.is_ok());
    }

    #[test]
    fn test_symlink_target_is_resolved() {
        let dir = tempdir().unwrap();
        let secret_dir = dir.path().join("secret");
        std::fs::create_dir(&secret_dir).unwrap();
        let secret = secret_dir.join("data.txt");
        std::fs::write(&secret, "x").unwrap();

        let link_dir = dir.path().join("public");
        std::fs::create_dir(&link_dir).unwrap();
        let link = link_dir.join("link.txt");
        // public/link.txt -> secret/data.txt
        std::os::unix::fs::symlink(&secret, &link).unwrap();

        // Confine to public/ only: the symlink resolves into secret/, so reject.
        let roots = vec![link_dir.clone()];
        let err = resolve_and_confine(&link_dir, "link.txt", &roots).unwrap_err();
        assert!(matches!(err, PathSafetyError::OutsideAllowedRoots { .. }));
    }
}

//! Plugin installation and uninstallation.

use std::path::{Component, Path, PathBuf};

/// Validate that `name` is a single, traversal-free path component (TOOL-11).
///
/// Rejects anything containing a path separator, a parent/current-dir reference,
/// a root/prefix, or an empty string — i.e. only a single [`Component::Normal`]
/// is accepted. This prevents a caller-supplied name like `../../etc` from
/// escaping the plugins directory before the path is ever joined.
fn validate_plugin_name(name: &str) -> Result<&str, String> {
    if name.is_empty() {
        return Err("Plugin name must not be empty".to_string());
    }

    let mut components = Path::new(name).components();
    match (components.next(), components.next()) {
        // Exactly one component, and it must be a normal file/dir name.
        (Some(Component::Normal(_)), None) => Ok(name),
        _ => Err(format!(
            "Invalid plugin name '{name}': must be a single path component \
             without separators or '..'"
        )),
    }
}

/// Resolve `<plugins_dir>/<name>` and assert the canonical result stays inside
/// the canonical plugins directory (TOOL-11). Guards against symlinks or any
/// residual traversal that survived [`validate_plugin_name`].
fn resolve_within_plugins_dir(name: &str) -> Result<PathBuf, String> {
    let name = validate_plugin_name(name)?;
    let plugins_dir = crate::discovery::get_user_plugins_dir();

    let canonical_root = plugins_dir
        .canonicalize()
        .map_err(|e| format!("Failed to resolve plugins directory: {e}"))?;

    let target = canonical_root.join(name);
    let canonical_target = target
        .canonicalize()
        .map_err(|e| format!("Plugin not found: {name} ({e})"))?;

    if !canonical_target.starts_with(&canonical_root) {
        return Err(format!(
            "Refusing to operate on '{name}': resolves outside the plugins directory"
        ));
    }

    Ok(canonical_target)
}

/// Install a plugin from a path (copy to user plugins directory).
pub fn install_plugin_from_path(source: &str) -> Result<String, String> {
    let source_path = Path::new(source);
    if !source_path.exists() {
        return Err(format!("Source path does not exist: {source}"));
    }

    let dest_dir = crate::discovery::get_user_plugins_dir();
    let plugin_name = source_path
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| format!("Source path has no valid file name: {source}"))?;

    // Validate the derived name is a single traversal-free component before
    // joining it onto the plugins dir (TOOL-11).
    let plugin_name = validate_plugin_name(plugin_name)?;

    let canonical_root = dest_dir
        .canonicalize()
        .map_err(|e| format!("Failed to resolve plugins directory: {e}"))?;
    let dest = canonical_root.join(plugin_name);

    if dest.exists() {
        return Err(format!("Plugin already exists: {plugin_name}"));
    }

    // Defense in depth: the join must not escape the plugins root.
    if !dest.starts_with(&canonical_root) {
        return Err(format!(
            "Refusing to install '{plugin_name}': resolves outside the plugins directory"
        ));
    }

    // Copy directory recursively, without following symlinks (TOOL-11).
    copy_dir_recursive(source_path, &dest).map_err(|e| format!("Failed to copy plugin: {e}"))?;

    Ok(format!("Installed plugin: {plugin_name}"))
}

/// Uninstall a plugin by name.
pub fn uninstall_plugin(name: &str) -> Result<(), String> {
    // Validate + canonicalize + assert containment before any destructive op
    // (TOOL-11): a name like `../../foo` can never reach `remove_dir_all`.
    let dir = resolve_within_plugins_dir(name)?;
    std::fs::remove_dir_all(&dir).map_err(|e| format!("Failed to remove plugin: {e}"))?;
    Ok(())
}

/// Recursively copy `src` into `dst` WITHOUT following symbolic links.
///
/// Symlinks encountered in the source tree are skipped rather than traversed, so
/// a malicious source plugin cannot use a symlink to read or clobber files
/// outside the destination (TOOL-11).
fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let path = entry.path();
        let dest_path = dst.join(entry.file_name());

        // Use symlink-aware metadata: do NOT follow links.
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            tracing::warn!(
                path = %path.display(),
                "skipping symlink while copying plugin (not followed)"
            );
            continue;
        }
        if file_type.is_dir() {
            copy_dir_recursive(&path, &dest_path)?;
        } else {
            std::fs::copy(&path, &dest_path)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_plugin_name_accepts_simple() {
        assert!(validate_plugin_name("my-plugin").is_ok());
        assert!(validate_plugin_name("plugin_42").is_ok());
    }

    #[test]
    fn test_validate_plugin_name_rejects_empty() {
        assert!(validate_plugin_name("").is_err());
    }

    #[test]
    fn test_validate_plugin_name_rejects_parent_traversal() {
        assert!(validate_plugin_name("..").is_err());
        assert!(validate_plugin_name("../etc").is_err());
        assert!(validate_plugin_name("../../etc/passwd").is_err());
    }

    #[test]
    fn test_validate_plugin_name_rejects_separators() {
        assert!(validate_plugin_name("foo/bar").is_err());
        assert!(validate_plugin_name("a/b/c").is_err());
    }

    #[test]
    fn test_validate_plugin_name_rejects_absolute() {
        assert!(validate_plugin_name("/etc/passwd").is_err());
        assert!(validate_plugin_name("/").is_err());
    }

    #[test]
    fn test_validate_plugin_name_rejects_current_dir() {
        assert!(validate_plugin_name(".").is_err());
    }

    #[test]
    fn test_uninstall_rejects_traversal_name() {
        // Traversal is rejected at validation, before any filesystem mutation.
        let err = uninstall_plugin("../../../tmp").unwrap_err();
        assert!(
            err.contains("single path component") || err.contains("outside"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn test_uninstall_rejects_separator_name() {
        let err = uninstall_plugin("foo/bar").unwrap_err();
        assert!(err.contains("single path component"), "unexpected error: {err}");
    }

    #[test]
    fn test_install_rejects_nonexistent_source() {
        let err = install_plugin_from_path("/nonexistent/path/xyz").unwrap_err();
        assert!(err.contains("does not exist"), "unexpected error: {err}");
    }

    #[test]
    fn test_copy_dir_recursive_skips_symlinks() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        let dst = tmp.path().join("dst");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("real.txt"), b"hello").unwrap();

        // Create a symlink inside the source tree pointing outside it.
        let outside = tmp.path().join("secret.txt");
        std::fs::write(&outside, b"secret").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, src.join("link.txt")).unwrap();

        copy_dir_recursive(&src, &dst).unwrap();

        assert!(dst.join("real.txt").exists());
        // The symlink must not have been copied/followed.
        #[cfg(unix)]
        assert!(!dst.join("link.txt").exists());
    }
}

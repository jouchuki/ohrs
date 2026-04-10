//! Plugin directory discovery.

use std::path::{Path, PathBuf};

/// Return the user plugin directory (~/.openharnessrs/plugins/).
pub fn get_user_plugins_dir() -> PathBuf {
    let dir = oh_config::get_config_dir().join("plugins");
    std::fs::create_dir_all(&dir).ok();
    dir
}

/// Return the project plugin directory (.openharnessrs/plugins/).
pub fn get_project_plugins_dir(cwd: &Path) -> PathBuf {
    let dir = cwd.join(".openharnessrs").join("plugins");
    std::fs::create_dir_all(&dir).ok();
    dir
}

/// Find plugin directories from user and project locations.
pub fn discover_plugin_paths(cwd: &Path) -> Vec<PathBuf> {
    let roots = [get_user_plugins_dir(), get_project_plugins_dir(cwd)];
    let mut paths = Vec::new();

    for root in &roots {
        if !root.exists() {
            continue;
        }
        let mut entries: Vec<_> = std::fs::read_dir(root)
            .into_iter()
            .flatten()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_dir())
            .filter(|e| find_manifest(&e.path()).is_some() || has_dylib(&e.path()))
            .map(|e| e.path())
            .collect();
        entries.sort();
        paths.extend(entries);
    }

    paths
}

/// Find plugin.json in standard or .claude-plugin/ locations.
pub fn find_manifest(plugin_dir: &Path) -> Option<PathBuf> {
    let candidates = [
        plugin_dir.join("plugin.json"),
        plugin_dir.join(".claude-plugin").join("plugin.json"),
    ];
    candidates.into_iter().find(|c| c.exists())
}

/// Check if a directory contains a native plugin (.so, .dll, or .dylib).
pub fn has_dylib(dir: &Path) -> bool {
    if !dir.is_dir() {
        return false;
    }
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .any(|e| {
            let path = e.path();
            matches!(
                path.extension().and_then(|s| s.to_str()),
                Some("so") | Some("dll") | Some("dylib")
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_find_manifest_in_root() {
        let dir = tempfile::tempdir().unwrap();
        let plugin_dir = dir.path().join("my-plugin");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(plugin_dir.join("plugin.json"), r#"{"name":"test"}"#).unwrap();

        let result = find_manifest(&plugin_dir);
        assert!(result.is_some());
        assert_eq!(result.unwrap(), plugin_dir.join("plugin.json"));
    }

    #[test]
    fn test_find_manifest_in_claude_plugin_subdir() {
        let dir = tempfile::tempdir().unwrap();
        let plugin_dir = dir.path().join("my-plugin");
        let subdir = plugin_dir.join(".claude-plugin");
        std::fs::create_dir_all(&subdir).unwrap();
        std::fs::write(subdir.join("plugin.json"), r#"{"name":"test"}"#).unwrap();

        let result = find_manifest(&plugin_dir);
        assert!(result.is_some());
        assert_eq!(result.unwrap(), subdir.join("plugin.json"));
    }

    #[test]
    fn test_find_manifest_returns_none_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let plugin_dir = dir.path().join("empty-plugin");
        std::fs::create_dir_all(&plugin_dir).unwrap();

        assert!(find_manifest(&plugin_dir).is_none());
    }

    #[test]
    fn test_has_dylib_returns_false_for_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!has_dylib(dir.path()));
    }

    #[test]
    fn test_has_dylib_returns_true_for_so_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("plugin.so"), b"").unwrap();
        assert!(has_dylib(dir.path()));
    }

    #[test]
    fn test_has_dylib_returns_true_for_dll_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("plugin.dll"), b"").unwrap();
        assert!(has_dylib(dir.path()));
    }

    #[test]
    fn test_has_dylib_returns_false_for_non_dir() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("not_a_dir.txt");
        std::fs::write(&file, "hello").unwrap();
        assert!(!has_dylib(&file));
    }

    #[test]
    fn test_discover_plugin_paths_empty_dirs() {
        let dir = tempfile::tempdir().unwrap();
        // Use a subdirectory as cwd so project plugins dir is inside tempdir
        let cwd = dir.path().join("project");
        std::fs::create_dir_all(&cwd).unwrap();

        // discover_plugin_paths also checks user plugins dir which we can't
        // easily control, but at minimum the project plugins should be empty.
        let paths = discover_plugin_paths(&cwd);
        // The project plugins dir was just created and is empty, so no
        // plugin paths should come from it. We just verify no panic occurs
        // and the result is a vec (may include user-level plugins if any exist).
        assert!(paths.iter().all(|p| p.is_dir()));
    }
}

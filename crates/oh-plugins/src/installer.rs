//! Plugin installation and uninstallation.

use std::path::Path;

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
        .unwrap_or("unknown");
    let dest = dest_dir.join(plugin_name);

    if dest.exists() {
        return Err(format!("Plugin already exists: {plugin_name}"));
    }

    // Copy directory recursively
    copy_dir_recursive(source_path, &dest)
        .map_err(|e| format!("Failed to copy plugin: {e}"))?;

    Ok(format!("Installed plugin: {plugin_name}"))
}

/// Uninstall a plugin by name.
pub fn uninstall_plugin(name: &str) -> Result<(), String> {
    let dir = crate::discovery::get_user_plugins_dir().join(name);
    if !dir.exists() {
        return Err(format!("Plugin not found: {name}"));
    }
    std::fs::remove_dir_all(&dir).map_err(|e| format!("Failed to remove plugin: {e}"))?;
    Ok(())
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let path = entry.path();
        let dest_path = dst.join(entry.file_name());
        if path.is_dir() {
            copy_dir_recursive(&path, &dest_path)?;
        } else {
            std::fs::copy(&path, &dest_path)?;
        }
    }
    Ok(())
}

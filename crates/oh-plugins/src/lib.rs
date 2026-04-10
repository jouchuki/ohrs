//! Plugin loading: dylib (.so/.dll) and JSON/markdown (static) plugins.

pub mod discovery;
pub mod json_loader;
pub mod dylib_loader;
pub mod installer;

pub use oh_types::plugin::LoadedPlugin;

use std::collections::HashMap;
use std::path::Path;

/// Load all plugins from user and project directories.
pub fn load_all_plugins(
    cwd: &Path,
    enabled_plugins: &HashMap<String, bool>,
) -> Vec<LoadedPlugin> {
    let paths = discovery::discover_plugin_paths(cwd);
    let mut plugins = Vec::new();

    for path in paths {
        // Try native first, then static
        if discovery::has_dylib(&path) {
            match dylib_loader::load_native_plugin(&path, enabled_plugins) {
                Ok(p) => {
                    plugins.push(p);
                    continue;
                }
                Err(e) => {
                    tracing::warn!(path = %path.display(), error = %e, "Failed to load native plugin");
                }
            }
        }

        match json_loader::load_static_plugin(&path, enabled_plugins) {
            Ok(p) => plugins.push(p),
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "Failed to load static plugin");
            }
        }
    }

    plugins
}

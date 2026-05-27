//! Native dylib plugin loading via libloading.

use libloading::{Library, Symbol};
use oh_plugin_abi::*;
use oh_types::plugin::{LoadedPlugin, PluginKind, PluginManifest};
use oh_types::skills::SkillDefinition;
use opentelemetry::KeyValue;
use std::collections::HashMap;
use std::path::Path;
use std::time::Instant;

/// Load a native plugin from a directory containing a .so/.dll/.dylib.
pub fn load_native_plugin(
    path: &Path,
    enabled_plugins: &HashMap<String, bool>,
) -> Result<LoadedPlugin, NativePluginError> {
    let dylib_path =
        find_dylib(path).ok_or_else(|| NativePluginError::NoDylib(path.display().to_string()))?;

    let start = Instant::now();

    // Safety: loading shared libraries is inherently unsafe.
    let lib = unsafe {
        Library::new(&dylib_path).map_err(|e| NativePluginError::LoadFailed(e.to_string()))?
    };

    // Look up the vtable symbol
    let vtable: *const PluginVTable = unsafe {
        let func: Symbol<unsafe extern "C" fn() -> *const PluginVTable> = lib
            .get(PLUGIN_INIT_SYMBOL.as_bytes())
            .map_err(|e| NativePluginError::SymbolNotFound(e.to_string()))?;
        func()
    };

    if vtable.is_null() {
        return Err(NativePluginError::NullVTable);
    }

    // --- ABI capability/version handshake (TOOL-6) ---
    // Performed BEFORE invoking any other vtable function pointer, so an
    // incompatibly-built plugin is rejected without ever being called into.
    //
    // SAFETY: `vtable` is non-null and points to a `PluginVTable` returned by the
    // plugin's entry point. We only read the leading POD fields here.
    let abi_major = unsafe { (*vtable).abi_version_major };
    if abi_major != ABI_VERSION_MAJOR {
        return Err(NativePluginError::AbiMismatch {
            expected: ABI_VERSION_MAJOR,
            got: abi_major,
        });
    }

    let capabilities = unsafe { (*vtable).capabilities };
    if capabilities & OH_REQUIRED_CAPABILITIES != OH_REQUIRED_CAPABILITIES {
        let missing = OH_REQUIRED_CAPABILITIES & !capabilities;
        tracing::warn!(
            plugin_path = %dylib_path.display(),
            required = OH_REQUIRED_CAPABILITIES,
            advertised = capabilities,
            missing,
            "rejecting plugin: missing required ABI capabilities (e.g. panic=unwind)"
        );
        return Err(NativePluginError::MissingCapabilities {
            required: OH_REQUIRED_CAPABILITIES,
            got: capabilities,
        });
    }

    // Get manifest. Validate UTF-8 (TOOL-5) and free the plugin allocation
    // through the plugin's own free fn afterwards (TOOL-5/TOOL-6).
    let manifest_json = unsafe { ((*vtable).get_manifest_json)() };
    let manifest_str = match unsafe { manifest_json.as_str() } {
        Ok(s) => s.to_string(),
        Err(e) => {
            unsafe { ((*vtable).free_string)(manifest_json) };
            return Err(NativePluginError::InvalidUtf8 {
                field: "manifest_json",
                source: e,
            });
        }
    };
    unsafe { ((*vtable).free_string)(manifest_json) };
    let manifest: PluginManifest = serde_json::from_str(&manifest_str)
        .map_err(|e| NativePluginError::ManifestParse(e.to_string()))?;

    let enabled = enabled_plugins
        .get(&manifest.name)
        .copied()
        .unwrap_or(manifest.enabled_by_default);

    // Initialize
    let config = serde_json::json!({}).to_string();
    let init_result = unsafe { ((*vtable).init)(config.as_ptr(), config.len()) };
    if init_result != OH_OK {
        return Err(NativePluginError::InitFailed(init_result));
    }

    // Get skills. Copy each field out (validating UTF-8 lossily — a bad string
    // degrades that one field, it must never crash the host), then hand the whole
    // slice back to the plugin to free with its own allocator (TOOL-5).
    let skills_slice = unsafe { ((*vtable).get_skills)() };
    let skills = if skills_slice.ptr.is_null() || skills_slice.len == 0 {
        Vec::new()
    } else {
        let mut skills = Vec::with_capacity(skills_slice.len);
        for i in 0..skills_slice.len {
            // SAFETY: handshake passed; the plugin returned `len` valid elements.
            let skill = unsafe { &*skills_slice.ptr.add(i) };
            let name = unsafe { skill.name.as_str_lossy() }.into_owned();
            let description = unsafe { skill.description.as_str_lossy() }.into_owned();
            let content = unsafe { skill.content.as_str_lossy() }.into_owned();
            skills.push(SkillDefinition {
                name,
                description,
                content,
                source: "native_plugin".into(),
                path: Some(path.display().to_string()),
            });
        }
        skills
    };
    // Free the skills slice (backing array + nested strings) via the plugin.
    unsafe { ((*vtable).free_skills)(skills_slice) };

    // Get hooks. Same copy-then-free discipline.
    let hooks_slice = unsafe { ((*vtable).get_hooks)() };
    let hooks = if hooks_slice.ptr.is_null() || hooks_slice.len == 0 {
        HashMap::new()
    } else {
        let mut hooks: HashMap<String, Vec<oh_types::hooks::HookDefinition>> = HashMap::new();
        for i in 0..hooks_slice.len {
            // SAFETY: handshake passed; the plugin returned `len` valid elements.
            let hook_def = unsafe { &*hooks_slice.ptr.add(i) };
            let event = unsafe { hook_def.event.as_str_lossy() }.into_owned();
            let hook_json = unsafe { hook_def.hook_json.as_str_lossy() };
            match serde_json::from_str(&hook_json) {
                Ok(hook) => hooks.entry(event).or_default().push(hook),
                Err(e) => tracing::warn!(
                    plugin = %manifest.name,
                    event = %event,
                    error = %e,
                    "skipping malformed hook definition from native plugin"
                ),
            }
        }
        hooks
    };
    unsafe { ((*vtable).free_hooks)(hooks_slice) };

    // Get MCP configs. Validate UTF-8 lossily, then free via the plugin.
    let mcp_json = unsafe { ((*vtable).get_mcp_configs_json)() };
    let mcp_str = unsafe { mcp_json.as_str_lossy() }.into_owned();
    unsafe { ((*vtable).free_string)(mcp_json) };
    let mcp_servers = if mcp_str.is_empty() || mcp_str == "null" {
        HashMap::new()
    } else {
        serde_json::from_str::<oh_types::mcp::McpJsonConfig>(&mcp_str)
            .map(|c| c.mcp_servers)
            .unwrap_or_default()
    };

    let elapsed = start.elapsed().as_secs_f64();
    oh_telemetry::PLUGIN_LOAD_DURATION.record(
        elapsed,
        &[KeyValue::new("plugin_name", manifest.name.clone())],
    );

    // Keep the library alive
    std::mem::forget(lib);

    Ok(LoadedPlugin {
        manifest,
        path: path.to_path_buf(),
        enabled,
        skills: skills.clone(),
        hooks,
        mcp_servers,
        commands: skills,
        kind: PluginKind::Native,
    })
}

/// Find a .so/.dll/.dylib in a directory.
fn find_dylib(dir: &Path) -> Option<std::path::PathBuf> {
    std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .find(|p| {
            matches!(
                p.extension().and_then(|s| s.to_str()),
                Some("so") | Some("dll") | Some("dylib")
            )
        })
}

/// Native plugin load errors.
#[derive(Debug, thiserror::Error)]
pub enum NativePluginError {
    #[error("no .so/.dll/.dylib found in: {0}")]
    NoDylib(String),
    #[error("failed to load library: {0}")]
    LoadFailed(String),
    #[error("symbol not found: {0}")]
    SymbolNotFound(String),
    #[error("plugin returned null vtable")]
    NullVTable,
    #[error("ABI version mismatch: expected {expected}, got {got}")]
    AbiMismatch { expected: u32, got: u32 },
    #[error("plugin missing required ABI capabilities: required {required:#x}, got {got:#x}")]
    MissingCapabilities { required: u64, got: u64 },
    #[error("plugin returned invalid UTF-8 in field '{field}': {source}")]
    InvalidUtf8 {
        field: &'static str,
        source: std::str::Utf8Error,
    },
    #[error("manifest parse error: {0}")]
    ManifestParse(String),
    #[error("plugin init failed with code: {0}")]
    InitFailed(i32),
}

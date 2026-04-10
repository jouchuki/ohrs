//! Proc-macro crate for OpenHarness plugins.
//!
//! Provides `#[openharness_plugin]` which generates the `extern "C"` vtable
//! and `oh_string_free` symbol from a struct implementing `OpenHarnessPlugin`.

use proc_macro::TokenStream;
use quote::quote;
use syn::{parse_macro_input, DeriveInput};

/// Attribute macro that generates FFI glue for an `OpenHarnessPlugin` implementation.
///
/// # Usage
/// ```ignore
/// #[openharness_plugin]
/// struct MyPlugin;
///
/// impl OpenHarnessPlugin for MyPlugin {
///     fn manifest(&self) -> PluginManifest { /* ... */ }
///     fn init(&mut self, _config: serde_json::Value) -> Result<(), String> { Ok(()) }
/// }
/// ```
///
/// This generates:
/// - A `static PLUGIN_INSTANCE` guarded by `OnceLock`
/// - `#[no_mangle] extern "C" fn oh_plugin_vtable() -> *const PluginVTable`
/// - `#[no_mangle] extern "C" fn oh_string_free(s: OhString)`
/// - All vtable function implementations with `catch_unwind` wrappers
#[proc_macro_attribute]
pub fn openharness_plugin(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as DeriveInput);
    let name = &input.ident;

    let expanded = quote! {
        #input

        // --- Generated FFI glue ---

        static __OH_PLUGIN: ::std::sync::OnceLock<::std::sync::Mutex<#name>> =
            ::std::sync::OnceLock::new();

        fn __oh_get_plugin() -> &'static ::std::sync::Mutex<#name> {
            __OH_PLUGIN.get_or_init(|| ::std::sync::Mutex::new(#name::default()))
        }

        unsafe extern "C" fn __oh_get_manifest_json() -> ::oh_plugin_abi::OhString {
            let result = ::std::panic::catch_unwind(|| {
                let plugin = __oh_get_plugin().lock().unwrap();
                let manifest = <#name as ::oh_plugin_abi::OpenHarnessPlugin>::manifest(&*plugin);
                ::serde_json::to_string(&manifest).unwrap_or_default()
            });
            match result {
                Ok(s) => ::oh_plugin_abi::OhString::from_string(s),
                Err(_) => ::oh_plugin_abi::OhString::empty(),
            }
        }

        unsafe extern "C" fn __oh_init(
            config_json: *const u8,
            config_len: usize,
        ) -> ::oh_plugin_abi::OhResult {
            let result = ::std::panic::catch_unwind(|| {
                let slice = unsafe { ::std::slice::from_raw_parts(config_json, config_len) };
                let config: ::serde_json::Value =
                    ::serde_json::from_slice(slice).unwrap_or(::serde_json::Value::Null);
                let mut plugin = __oh_get_plugin().lock().unwrap();
                match <#name as ::oh_plugin_abi::OpenHarnessPlugin>::init(&mut *plugin, config) {
                    Ok(()) => ::oh_plugin_abi::OH_OK,
                    Err(_) => ::oh_plugin_abi::OH_ERR_INIT,
                }
            });
            result.unwrap_or(::oh_plugin_abi::OH_ERR_INTERNAL)
        }

        unsafe extern "C" fn __oh_get_skills() -> ::oh_plugin_abi::OhSlice<::oh_plugin_abi::OhSkillDef> {
            let result = ::std::panic::catch_unwind(|| {
                let plugin = __oh_get_plugin().lock().unwrap();
                let skills = <#name as ::oh_plugin_abi::OpenHarnessPlugin>::skills(&*plugin);
                let ffi_skills: Vec<::oh_plugin_abi::OhSkillDef> = skills
                    .into_iter()
                    .map(|s| ::oh_plugin_abi::OhSkillDef {
                        name: ::oh_plugin_abi::OhString::from_string(s.name),
                        description: ::oh_plugin_abi::OhString::from_string(s.description),
                        content: ::oh_plugin_abi::OhString::from_string(s.content),
                    })
                    .collect();
                ::oh_plugin_abi::OhSlice::from_vec(ffi_skills)
            });
            result.unwrap_or_else(|_| ::oh_plugin_abi::OhSlice::empty())
        }

        unsafe extern "C" fn __oh_get_hooks() -> ::oh_plugin_abi::OhSlice<::oh_plugin_abi::OhHookDef> {
            let result = ::std::panic::catch_unwind(|| {
                let plugin = __oh_get_plugin().lock().unwrap();
                let hooks = <#name as ::oh_plugin_abi::OpenHarnessPlugin>::hooks(&*plugin);
                let ffi_hooks: Vec<::oh_plugin_abi::OhHookDef> = hooks
                    .into_iter()
                    .map(|h| ::oh_plugin_abi::OhHookDef {
                        event: ::oh_plugin_abi::OhString::from_string(h.event),
                        hook_json: ::oh_plugin_abi::OhString::from_string(
                            ::serde_json::to_string(&h.config).unwrap_or_default(),
                        ),
                    })
                    .collect();
                ::oh_plugin_abi::OhSlice::from_vec(ffi_hooks)
            });
            result.unwrap_or_else(|_| ::oh_plugin_abi::OhSlice::empty())
        }

        unsafe extern "C" fn __oh_get_mcp_configs_json() -> ::oh_plugin_abi::OhString {
            let result = ::std::panic::catch_unwind(|| {
                let plugin = __oh_get_plugin().lock().unwrap();
                let configs = <#name as ::oh_plugin_abi::OpenHarnessPlugin>::mcp_configs(&*plugin);
                ::serde_json::to_string(&configs).unwrap_or_default()
            });
            match result {
                Ok(s) => ::oh_plugin_abi::OhString::from_string(s),
                Err(_) => ::oh_plugin_abi::OhString::empty(),
            }
        }

        unsafe extern "C" fn __oh_execute_command(
            command_name: *const u8,
            command_name_len: usize,
            args_json: *const u8,
            args_json_len: usize,
        ) -> ::oh_plugin_abi::OhString {
            let result = ::std::panic::catch_unwind(|| {
                let cmd = unsafe {
                    ::std::str::from_utf8_unchecked(
                        ::std::slice::from_raw_parts(command_name, command_name_len),
                    )
                };
                let args_slice = unsafe { ::std::slice::from_raw_parts(args_json, args_json_len) };
                let args: ::serde_json::Value =
                    ::serde_json::from_slice(args_slice).unwrap_or(::serde_json::Value::Null);
                let plugin = __oh_get_plugin().lock().unwrap();
                match <#name as ::oh_plugin_abi::OpenHarnessPlugin>::execute_command(&*plugin, cmd, args) {
                    Ok(r) => ::serde_json::to_string(&r).unwrap_or_default(),
                    Err(e) => ::serde_json::json!({"output": e, "is_error": true}).to_string(),
                }
            });
            match result {
                Ok(s) => ::oh_plugin_abi::OhString::from_string(s),
                Err(_) => ::oh_plugin_abi::OhString::from_string(
                    r#"{"output":"plugin panicked","is_error":true}"#.to_string(),
                ),
            }
        }

        unsafe extern "C" fn __oh_shutdown() {
            let _ = ::std::panic::catch_unwind(|| {
                let mut plugin = __oh_get_plugin().lock().unwrap();
                <#name as ::oh_plugin_abi::OpenHarnessPlugin>::shutdown(&mut *plugin);
            });
        }

        static __OH_VTABLE: ::oh_plugin_abi::PluginVTable = ::oh_plugin_abi::PluginVTable {
            abi_version_major: ::oh_plugin_abi::ABI_VERSION_MAJOR,
            abi_version_minor: ::oh_plugin_abi::ABI_VERSION_MINOR,
            get_manifest_json: __oh_get_manifest_json,
            init: __oh_init,
            get_skills: __oh_get_skills,
            get_hooks: __oh_get_hooks,
            get_mcp_configs_json: __oh_get_mcp_configs_json,
            execute_command: __oh_execute_command,
            shutdown: __oh_shutdown,
        };

        #[no_mangle]
        pub extern "C" fn oh_plugin_vtable() -> *const ::oh_plugin_abi::PluginVTable {
            &__OH_VTABLE as *const _
        }

        #[no_mangle]
        pub unsafe extern "C" fn oh_string_free(s: ::oh_plugin_abi::OhString) {
            if !s.ptr.is_null() {
                let _ = unsafe { Vec::from_raw_parts(s.ptr, s.len, s.cap) };
            }
        }
    };

    TokenStream::from(expanded)
}

//! C-compatible FFI types for the plugin ABI boundary.
//!
//! # Plugin build requirements (TOOL-6)
//!
//! The host loads plugins as native shared libraries and calls into them across
//! a `#[repr(C)]` boundary. For this to be **sound**, every plugin MUST be built
//! with the following assumptions, which the host verifies at load time via the
//! capability handshake in [`PluginVTable`]:
//!
//! - **`panic = "unwind"`** (the default profile setting). The host wraps every
//!   plugin entry point in `catch_unwind`; that only works if the plugin unwinds
//!   on panic. A plugin compiled with `panic = "abort"` would tear down the whole
//!   host process on any panic, so such plugins are rejected at load.
//! - **Matching ABI major version** ([`ABI_VERSION_MAJOR`]).
//! - **Allocator ownership**: memory allocated by the plugin (every [`OhString`]
//!   / [`OhSlice`] it returns) is freed by the plugin via `oh_string_free` /
//!   `oh_slice_free`; memory allocated by the host is freed by the host. Neither
//!   side ever calls `Vec::from_raw_parts` on the other's allocation (that is UB
//!   across an allocator mismatch). The `#[openharness_plugin]` derive macro and
//!   the host loader cooperate to uphold this.

/// ABI version — bump the major on breaking changes.
///
/// Bumped to 2 when the capability handshake fields and `oh_slice_free` symbol
/// were added to [`PluginVTable`] (TOOL-6). A plugin built against ABI 1 has an
/// incompatible vtable layout and is rejected.
pub const ABI_VERSION_MAJOR: u32 = 2;
pub const ABI_VERSION_MINOR: u32 = 0;

/// Entry point symbol every plugin `.so` must export.
pub const PLUGIN_INIT_SYMBOL: &str = "oh_plugin_vtable";

/// Free function symbol for releasing [`OhString`] allocations.
pub const STRING_FREE_SYMBOL: &str = "oh_string_free";

/// Free function symbol for releasing [`OhSlice`] allocations.
pub const SLICE_FREE_SYMBOL: &str = "oh_slice_free";

/// Capability bit: the plugin was built with `panic = "unwind"`, so the host's
/// `catch_unwind` boundary is sound. The host refuses to load a plugin that does
/// not assert this bit (TOOL-6).
pub const OH_CAP_PANIC_UNWIND: u64 = 1 << 0;

/// The capability bits the host requires every plugin to assert. A plugin whose
/// `capabilities` field does not contain all of these is rejected at load.
pub const OH_REQUIRED_CAPABILITIES: u64 = OH_CAP_PANIC_UNWIND;

/// Heap-allocated, UTF-8 string returned across FFI.
/// The host calls `oh_string_free` to release it.
#[repr(C)]
pub struct OhString {
    pub ptr: *mut u8,
    pub len: usize,
    pub cap: usize,
}

impl OhString {
    /// Create an empty OhString.
    pub fn empty() -> Self {
        Self {
            ptr: std::ptr::null_mut(),
            len: 0,
            cap: 0,
        }
    }

    /// Create an OhString from a Rust String, transferring ownership.
    pub fn from_string(s: String) -> Self {
        let mut s = s.into_bytes();
        let oh = Self {
            ptr: s.as_mut_ptr(),
            len: s.len(),
            cap: s.capacity(),
        };
        std::mem::forget(s);
        oh
    }

    /// Borrow the bytes as `&str`, **validating** UTF-8.
    ///
    /// The bytes originate from a third-party plugin and are therefore untrusted:
    /// they may not be valid UTF-8. This returns `Err` on invalid input rather
    /// than invoking undefined behaviour (TOOL-5). Use [`as_str_lossy`] when a
    /// best-effort string is acceptable.
    ///
    /// [`as_str_lossy`]: OhString::as_str_lossy
    ///
    /// # Safety
    /// The pointer must either be null or point to `len` readable bytes that
    /// remain valid for the lifetime of the returned reference. The host upholds
    /// this by only calling this on strings returned by a loaded plugin and
    /// before freeing them.
    pub unsafe fn as_str(&self) -> Result<&str, std::str::Utf8Error> {
        if self.ptr.is_null() || self.len == 0 {
            return Ok("");
        }
        // SAFETY: caller guarantees `ptr`/`len` describe a valid readable region.
        let slice = unsafe { std::slice::from_raw_parts(self.ptr, self.len) };
        std::str::from_utf8(slice)
    }

    /// Borrow the bytes as a lossy `Cow<str>`, replacing any invalid UTF-8
    /// sequences with U+FFFD. Never invokes undefined behaviour on bad input.
    ///
    /// # Safety
    /// Same contract as [`as_str`](OhString::as_str).
    pub unsafe fn as_str_lossy(&self) -> std::borrow::Cow<'_, str> {
        if self.ptr.is_null() || self.len == 0 {
            return std::borrow::Cow::Borrowed("");
        }
        // SAFETY: caller guarantees `ptr`/`len` describe a valid readable region.
        let slice = unsafe { std::slice::from_raw_parts(self.ptr, self.len) };
        String::from_utf8_lossy(slice)
    }

    /// Reclaim the allocation as an owned `String`, consuming the `OhString`.
    ///
    /// Invalid UTF-8 is repaired lossily (U+FFFD) rather than triggering UB.
    ///
    /// # Safety
    /// The pointer must have been produced by [`from_string`](OhString::from_string)
    /// (or an allocator-compatible equivalent) **in this same binary** — the
    /// reconstructed `Vec` is freed by this binary's allocator, so calling it on a
    /// plugin-allocated `OhString` would free across an allocator boundary (UB).
    /// The host therefore only calls this on strings it allocated; plugin-returned
    /// strings are released via the plugin's `oh_string_free`.
    pub unsafe fn into_string(self) -> String {
        if self.ptr.is_null() {
            return String::new();
        }
        // SAFETY: caller guarantees the allocation came from this binary's
        // allocator via `from_string`, so the parts are valid to reconstruct.
        let vec = unsafe { Vec::from_raw_parts(self.ptr, self.len, self.cap) };
        String::from_utf8_lossy(&vec).into_owned()
    }
}

/// Result code across FFI.
pub type OhResult = i32;
pub const OH_OK: OhResult = 0;
pub const OH_ERR_INIT: OhResult = 1;
pub const OH_ERR_INVALID_INPUT: OhResult = 2;
pub const OH_ERR_INTERNAL: OhResult = 3;

/// A C-compatible array slice.
#[repr(C)]
pub struct OhSlice<T> {
    pub ptr: *mut T,
    pub len: usize,
}

impl<T> OhSlice<T> {
    pub fn empty() -> Self {
        Self {
            ptr: std::ptr::null_mut(),
            len: 0,
        }
    }

    /// Create from a Vec, transferring ownership.
    pub fn from_vec(mut v: Vec<T>) -> Self {
        let slice = Self {
            ptr: v.as_mut_ptr(),
            len: v.len(),
        };
        std::mem::forget(v);
        slice
    }
}

/// A C-compatible skill definition.
#[repr(C)]
pub struct OhSkillDef {
    pub name: OhString,
    pub description: OhString,
    pub content: OhString,
}

/// A C-compatible hook definition.
#[repr(C)]
pub struct OhHookDef {
    pub event: OhString,
    pub hook_json: OhString,
}

/// The vtable every native plugin exports.
///
/// The leading version + capability fields form the load-time handshake
/// (TOOL-6): the host checks them before calling any function pointer, so a
/// plugin with an incompatible build (wrong ABI, `panic = "abort"`) is rejected
/// without ever being invoked.
#[repr(C)]
pub struct PluginVTable {
    pub abi_version_major: u32,
    pub abi_version_minor: u32,

    /// Bitmask of [`OH_CAP_PANIC_UNWIND`] and friends, asserting the plugin's
    /// build assumptions. The host requires all of [`OH_REQUIRED_CAPABILITIES`].
    pub capabilities: u64,

    /// Release a string allocated by THIS plugin. The host calls this (never
    /// `into_string`) on every [`OhString`] the plugin returns, so each side
    /// frees only what its own allocator allocated (TOOL-5 / TOOL-6).
    pub free_string: unsafe extern "C" fn(OhString),

    /// Release a skills slice (backing array + every nested [`OhString`])
    /// allocated by this plugin. Frees with the plugin's allocator.
    pub free_skills: unsafe extern "C" fn(OhSlice<OhSkillDef>),

    /// Release a hooks slice (backing array + every nested [`OhString`])
    /// allocated by this plugin. Frees with the plugin's allocator.
    pub free_hooks: unsafe extern "C" fn(OhSlice<OhHookDef>),

    /// Return plugin metadata as JSON.
    pub get_manifest_json: unsafe extern "C" fn() -> OhString,

    /// Initialize the plugin. `config_json` is host-provided config.
    pub init: unsafe extern "C" fn(config_json: *const u8, config_len: usize) -> OhResult,

    /// Return skills provided by this plugin.
    pub get_skills: unsafe extern "C" fn() -> OhSlice<OhSkillDef>,

    /// Return hooks provided by this plugin.
    pub get_hooks: unsafe extern "C" fn() -> OhSlice<OhHookDef>,

    /// Return MCP server configs as a JSON string.
    pub get_mcp_configs_json: unsafe extern "C" fn() -> OhString,

    /// Execute a command defined by this plugin.
    pub execute_command: unsafe extern "C" fn(
        command_name: *const u8,
        command_name_len: usize,
        args_json: *const u8,
        args_json_len: usize,
    ) -> OhString,

    /// Teardown. Called once before unloading.
    pub shutdown: unsafe extern "C" fn(),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ohstring_from_string_as_str() {
        let oh = OhString::from_string("hello".to_string());
        let s = unsafe { oh.as_str() }.expect("valid utf-8");
        assert_eq!(s, "hello");
        // Reclaim to avoid leak
        unsafe { oh.into_string() };
    }

    #[test]
    fn test_ohstring_from_string_into_string() {
        let oh = OhString::from_string("hello".to_string());
        let s = unsafe { oh.into_string() };
        assert_eq!(s, "hello");
    }

    #[test]
    fn test_ohstring_empty_as_str() {
        let oh = OhString::empty();
        let s = unsafe { oh.as_str() }.expect("empty is valid");
        assert_eq!(s, "");
    }

    /// TOOL-5: invalid UTF-8 from a plugin must be reported, never UB.
    #[test]
    fn test_ohstring_invalid_utf8_as_str_errs() {
        // 0xFF is never valid in UTF-8.
        let mut bytes = vec![b'o', b'k', 0xFF, 0xFE];
        let oh = OhString {
            ptr: bytes.as_mut_ptr(),
            len: bytes.len(),
            cap: bytes.capacity(),
        };
        std::mem::forget(bytes);
        assert!(unsafe { oh.as_str() }.is_err());
        // Reclaim raw bytes (not as a String — they aren't valid UTF-8).
        let _ = unsafe { Vec::from_raw_parts(oh.ptr, oh.len, oh.cap) };
    }

    /// TOOL-5: lossy accessor repairs invalid UTF-8 instead of UB.
    #[test]
    fn test_ohstring_invalid_utf8_as_str_lossy() {
        let mut bytes = vec![b'a', 0xFF, b'b'];
        let oh = OhString {
            ptr: bytes.as_mut_ptr(),
            len: bytes.len(),
            cap: bytes.capacity(),
        };
        std::mem::forget(bytes);
        let lossy = unsafe { oh.as_str_lossy() };
        assert!(lossy.contains('a') && lossy.contains('b'));
        assert!(lossy.contains('\u{FFFD}'));
        let _ = unsafe { Vec::from_raw_parts(oh.ptr, oh.len, oh.cap) };
    }

    /// TOOL-5: into_string repairs invalid UTF-8 lossily instead of UB.
    #[test]
    fn test_ohstring_invalid_utf8_into_string_lossy() {
        let mut bytes = vec![b'x', 0x80, b'y'];
        let oh = OhString {
            ptr: bytes.as_mut_ptr(),
            len: bytes.len(),
            cap: bytes.capacity(),
        };
        std::mem::forget(bytes);
        let s = unsafe { oh.into_string() };
        assert!(s.contains('x') && s.contains('y') && s.contains('\u{FFFD}'));
    }

    #[test]
    fn test_ohstring_empty_into_string() {
        let oh = OhString::empty();
        let s = unsafe { oh.into_string() };
        assert_eq!(s, "");
    }

    #[test]
    fn test_ohstring_empty_fields() {
        let oh = OhString::empty();
        assert!(oh.ptr.is_null());
        assert_eq!(oh.len, 0);
        assert_eq!(oh.cap, 0);
    }

    #[test]
    fn test_ohstring_roundtrip_unicode() {
        let original = "こんにちは世界 🌍".to_string();
        let oh = OhString::from_string(original.clone());
        assert_eq!(oh.len, original.len());
        let recovered = unsafe { oh.into_string() };
        assert_eq!(recovered, original);
    }

    #[test]
    fn test_ohslice_empty() {
        let s = OhSlice::<u32>::empty();
        assert!(s.ptr.is_null());
        assert_eq!(s.len, 0);
    }

    #[test]
    fn test_ohslice_from_vec() {
        let s = OhSlice::from_vec(vec![1u32, 2, 3]);
        assert_eq!(s.len, 3);
        assert!(!s.ptr.is_null());
        // Reclaim to avoid leak
        unsafe { Vec::from_raw_parts(s.ptr, s.len, s.len) };
    }

    #[test]
    fn test_ohslice_from_empty_vec() {
        let s = OhSlice::from_vec(Vec::<u8>::new());
        assert_eq!(s.len, 0);
    }

    #[test]
    fn test_abi_version_constants() {
        assert_eq!(ABI_VERSION_MAJOR, 2);
        assert_eq!(ABI_VERSION_MINOR, 0);
    }

    #[test]
    fn test_required_capabilities_include_panic_unwind() {
        assert_ne!(OH_REQUIRED_CAPABILITIES & OH_CAP_PANIC_UNWIND, 0);
    }

    #[test]
    fn test_slice_free_symbol() {
        assert_eq!(SLICE_FREE_SYMBOL, "oh_slice_free");
    }

    #[test]
    fn test_result_codes() {
        assert_eq!(OH_OK, 0);
        assert_eq!(OH_ERR_INIT, 1);
        assert_eq!(OH_ERR_INVALID_INPUT, 2);
        assert_eq!(OH_ERR_INTERNAL, 3);
    }

    #[test]
    fn test_plugin_init_symbol() {
        assert_eq!(PLUGIN_INIT_SYMBOL, "oh_plugin_vtable");
    }

    #[test]
    fn test_string_free_symbol() {
        assert_eq!(STRING_FREE_SYMBOL, "oh_string_free");
    }
}

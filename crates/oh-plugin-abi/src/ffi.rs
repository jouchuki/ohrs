//! C-compatible FFI types for the plugin ABI boundary.

/// ABI version — bump on breaking changes.
pub const ABI_VERSION_MAJOR: u32 = 1;
pub const ABI_VERSION_MINOR: u32 = 0;

/// Entry point symbol every plugin `.so` must export.
pub const PLUGIN_INIT_SYMBOL: &str = "oh_plugin_vtable";

/// Free function symbol for releasing OhString allocations.
pub const STRING_FREE_SYMBOL: &str = "oh_string_free";

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

    /// Convert to a &str (unsafe: caller must ensure validity).
    ///
    /// # Safety
    /// The pointer must be valid and the bytes must be valid UTF-8.
    pub unsafe fn as_str(&self) -> &str {
        if self.ptr.is_null() || self.len == 0 {
            return "";
        }
        let slice = unsafe { std::slice::from_raw_parts(self.ptr, self.len) };
        std::str::from_utf8_unchecked(slice)
    }

    /// Convert to an owned String, consuming the OhString.
    ///
    /// # Safety
    /// The pointer must have been created by `from_string` or equivalent.
    pub unsafe fn into_string(self) -> String {
        if self.ptr.is_null() {
            return String::new();
        }
        let vec = unsafe { Vec::from_raw_parts(self.ptr, self.len, self.cap) };
        String::from_utf8_unchecked(vec)
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
#[repr(C)]
pub struct PluginVTable {
    pub abi_version_major: u32,
    pub abi_version_minor: u32,

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
        let s = unsafe { oh.as_str() };
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
        let s = unsafe { oh.as_str() };
        assert_eq!(s, "");
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
        assert_eq!(ABI_VERSION_MAJOR, 1);
        assert_eq!(ABI_VERSION_MINOR, 0);
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

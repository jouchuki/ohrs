//! Stable C FFI ABI for OpenHarness dylib plugins.
//!
//! This crate defines the `#[repr(C)]` types that cross the plugin boundary.
//! Plugin authors implement [`OpenHarnessPlugin`] and use the `oh-plugin-derive`
//! proc-macro to generate the `extern "C"` glue.

pub mod ffi;
pub mod traits;

pub use ffi::*;
pub use traits::*;

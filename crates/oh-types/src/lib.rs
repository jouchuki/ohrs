//! Shared domain types for the OpenHarness agent harness.
//!
//! This crate contains zero-IO, serde-only types used across all other crates.

pub mod api;
pub mod bridge;
pub mod coordinator;
pub mod hooks;
pub mod mcp;
pub mod messages;
pub mod permissions;
pub mod plugin;
pub mod skills;
pub mod state;
pub mod stream_events;
pub mod tasks;
pub mod tools;
pub mod ui;

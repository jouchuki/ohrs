//! API client types: requests, events, usage, errors.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::messages::ConversationMessage;

/// Token usage returned by the model provider.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct UsageSnapshot {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    /// Tokens written to the KV cache (billed at 1.25x).
    #[serde(default)]
    pub cache_creation_input_tokens: u64,
    /// Tokens read from KV cache (billed at 0.1x).
    #[serde(default)]
    pub cache_read_input_tokens: u64,
}

impl UsageSnapshot {
    pub fn total_tokens(&self) -> u64 {
        self.input_tokens + self.output_tokens
    }

    /// Total context tokens processed this turn (fresh + cache read + cache creation).
    pub fn context_tokens(&self) -> u64 {
        self.input_tokens + self.cache_read_input_tokens + self.cache_creation_input_tokens
    }

    /// Effective billed input tokens (cache reads at 0.1x, cache creation at 1.25x, fresh at 1x).
    pub fn effective_input_tokens(&self) -> f64 {
        let fresh = self.input_tokens as f64;
        let created = self.cache_creation_input_tokens as f64 * 1.25;
        let cached = self.cache_read_input_tokens as f64 * 0.1;
        fresh + created + cached
    }
}

/// Input parameters for a model invocation.
#[derive(Debug, Clone)]
pub struct ApiMessageRequest {
    pub model: String,
    pub messages: Vec<ConversationMessage>,
    pub system_prompt: Option<String>,
    pub max_tokens: u32,
    pub tools: Vec<serde_json::Value>,
}

impl Default for ApiMessageRequest {
    fn default() -> Self {
        Self {
            model: String::new(),
            messages: Vec::new(),
            system_prompt: None,
            max_tokens: 4096,
            tools: Vec::new(),
        }
    }
}

/// Incremental text produced by the model.
#[derive(Debug, Clone)]
pub struct ApiTextDeltaEvent {
    pub text: String,
}

/// Terminal event containing the full assistant message.
#[derive(Debug, Clone)]
pub struct ApiMessageCompleteEvent {
    pub message: ConversationMessage,
    pub usage: UsageSnapshot,
    pub stop_reason: Option<String>,
}

/// Union of streamed API events.
#[derive(Debug, Clone)]
pub enum ApiStreamEvent {
    TextDelta(ApiTextDeltaEvent),
    MessageComplete(ApiMessageCompleteEvent),
}

/// API error types for OpenHarness.
#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("authentication failure: {0}")]
    Authentication(String),

    #[error("rate limit exceeded: {0}")]
    RateLimit(String),

    #[error("request failure: {0}")]
    Request(String),

    #[error("network error: {0}")]
    Network(String),
}

/// Retry configuration constants.
pub const MAX_RETRIES: u32 = 3;
pub const BASE_DELAY_SECS: f64 = 1.0;
pub const MAX_DELAY_SECS: f64 = 30.0;
pub const RETRYABLE_STATUS_CODES: &[u16] = &[429, 500, 502, 503, 529];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_usage_snapshot_total_tokens() {
        let usage = UsageSnapshot {
            input_tokens: 100,
            output_tokens: 50,
            ..Default::default()
        };
        assert_eq!(usage.total_tokens(), 150);
    }

    #[test]
    fn test_usage_snapshot_default() {
        let usage = UsageSnapshot::default();
        assert_eq!(usage.input_tokens, 0);
        assert_eq!(usage.output_tokens, 0);
        assert_eq!(usage.total_tokens(), 0);
    }

    #[test]
    fn test_usage_snapshot_serde_roundtrip() {
        let usage = UsageSnapshot {
            input_tokens: 42,
            output_tokens: 58,
            ..Default::default()
        };
        let json = serde_json::to_string(&usage).unwrap();
        let deser: UsageSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(usage, deser);
    }

    #[test]
    fn test_usage_snapshot_deserialize_missing_fields() {
        let deser: UsageSnapshot = serde_json::from_str("{}").unwrap();
        assert_eq!(deser.input_tokens, 0);
        assert_eq!(deser.output_tokens, 0);
    }

    #[test]
    fn test_api_error_authentication_display() {
        let err = ApiError::Authentication("bad key".into());
        assert_eq!(format!("{}", err), "authentication failure: bad key");
    }

    #[test]
    fn test_api_error_rate_limit_display() {
        let err = ApiError::RateLimit("retry later".into());
        assert_eq!(format!("{}", err), "rate limit exceeded: retry later");
    }

    #[test]
    fn test_api_error_request_display() {
        let err = ApiError::Request("bad request".into());
        assert_eq!(format!("{}", err), "request failure: bad request");
    }

    #[test]
    fn test_api_error_network_display() {
        let err = ApiError::Network("timeout".into());
        assert_eq!(format!("{}", err), "network error: timeout");
    }

    #[test]
    fn test_api_message_request_default() {
        let req = ApiMessageRequest::default();
        assert_eq!(req.model, "");
        assert!(req.messages.is_empty());
        assert!(req.system_prompt.is_none());
        assert_eq!(req.max_tokens, 4096);
        assert!(req.tools.is_empty());
    }

    #[test]
    fn test_retry_constants() {
        assert_eq!(MAX_RETRIES, 3);
        assert!(RETRYABLE_STATUS_CODES.contains(&429));
        assert!(RETRYABLE_STATUS_CODES.contains(&529));
        assert!(!RETRYABLE_STATUS_CODES.contains(&200));
    }
}

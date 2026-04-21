//! Slack Events API webhook handling.
//!
//! Exposes an axum router at `POST /slack/events` that:
//!  1. Verifies the Slack request signature (HMAC-SHA256, timing-safe).
//!  2. Handles `url_verification` challenge handshakes.
//!  3. Converts `event_callback` / `message` events to `InboundMessage` and
//!     publishes them on the bus.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::post,
    Json, Router,
};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use subtle::ConstantTimeEq;
use tracing::{debug, warn};

use crate::{
    bus::MessageBus,
    types::{ChannelId, ChannelKind, InboundMessage},
};

// ---------------------------------------------------------------------------
// Shared state injected into the axum router
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct EventsState {
    pub signing_secret: String,
    pub bus: Arc<dyn MessageBus>,
}

// ---------------------------------------------------------------------------
// Public: build the axum Router
// ---------------------------------------------------------------------------

/// Returns an axum `Router` that handles `POST /slack/events`.
pub fn events_router(state: EventsState) -> Router {
    Router::new()
        .route("/slack/events", post(handle_event))
        .with_state(state)
}

// ---------------------------------------------------------------------------
// Signature verification
// ---------------------------------------------------------------------------

/// Verify a Slack request signature (HMAC-SHA256, constant-time compare).
///
/// Returns `Ok(())` when the signature is valid.
pub fn verify_signature(
    signing_secret: &str,
    timestamp_header: &str,
    body: &[u8],
    signature_header: &str,
) -> Result<(), SignatureError> {
    // Reject stale requests (> 5 minutes)
    let ts: u64 = timestamp_header
        .parse()
        .map_err(|_| SignatureError::InvalidTimestamp)?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs();
    if now.saturating_sub(ts) > 300 || ts.saturating_sub(now) > 300 {
        return Err(SignatureError::StaleTimestamp);
    }

    // Construct the base string: "v0:{timestamp}:{body}"
    let base = format!("v0:{}:{}", timestamp_header, String::from_utf8_lossy(body));

    // Compute HMAC-SHA256
    let mut mac = Hmac::<Sha256>::new_from_slice(signing_secret.as_bytes())
        .map_err(|_| SignatureError::CryptoError)?;
    mac.update(base.as_bytes());
    let computed = hex::encode(mac.finalize().into_bytes());
    let expected = format!("v0={computed}");

    // Constant-time comparison to prevent timing attacks
    if expected.as_bytes().ct_eq(signature_header.as_bytes()).into() {
        Ok(())
    } else {
        Err(SignatureError::Mismatch)
    }
}

/// Errors from signature verification.
#[derive(Debug, thiserror::Error)]
pub enum SignatureError {
    #[error("invalid or missing timestamp")]
    InvalidTimestamp,
    #[error("stale timestamp (replay protection)")]
    StaleTimestamp,
    #[error("HMAC initialisation failed")]
    CryptoError,
    #[error("signature mismatch")]
    Mismatch,
}

// ---------------------------------------------------------------------------
// axum handler
// ---------------------------------------------------------------------------

async fn handle_event(
    State(state): State<EventsState>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    // --- 1. Verify signature ---
    let timestamp = match headers
        .get("X-Slack-Request-Timestamp")
        .and_then(|v| v.to_str().ok())
    {
        Some(ts) => ts.to_owned(),
        None => {
            warn!("slack: missing X-Slack-Request-Timestamp");
            return (StatusCode::UNAUTHORIZED, "missing timestamp").into_response();
        }
    };
    let signature = match headers
        .get("X-Slack-Signature")
        .and_then(|v| v.to_str().ok())
    {
        Some(sig) => sig.to_owned(),
        None => {
            warn!("slack: missing X-Slack-Signature");
            return (StatusCode::UNAUTHORIZED, "missing signature").into_response();
        }
    };

    if let Err(e) = verify_signature(&state.signing_secret, &timestamp, &body, &signature) {
        warn!("slack: signature verification failed: {e}");
        return (StatusCode::UNAUTHORIZED, "bad signature").into_response();
    }

    // --- 2. Parse JSON payload ---
    let payload: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            warn!("slack: failed to parse body: {e}");
            return (StatusCode::BAD_REQUEST, "invalid json").into_response();
        }
    };

    let event_type = payload.get("type").and_then(|v| v.as_str()).unwrap_or("");

    // --- 3. Handle url_verification challenge ---
    if event_type == "url_verification" {
        let challenge = payload
            .get("challenge")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        return Json(serde_json::json!({ "challenge": challenge })).into_response();
    }

    // --- 4. Handle event_callback ---
    if event_type == "event_callback" {
        let event = match payload.get("event") {
            Some(e) => e,
            None => {
                return (StatusCode::BAD_REQUEST, "missing event").into_response();
            }
        };

        let msg_type = event.get("type").and_then(|v| v.as_str()).unwrap_or("");
        if msg_type == "message" || msg_type == "app_mention" {
            // Ignore bot messages and edited messages
            if event.get("subtype").is_some() {
                debug!("slack: ignoring message with subtype");
                return StatusCode::OK.into_response();
            }

            let user_id = match event.get("user").and_then(|v| v.as_str()) {
                Some(u) => u.to_owned(),
                None => {
                    debug!("slack: ignoring event without user");
                    return StatusCode::OK.into_response();
                }
            };
            let channel = match event.get("channel").and_then(|v| v.as_str()) {
                Some(c) => c.to_owned(),
                None => {
                    debug!("slack: ignoring event without channel");
                    return StatusCode::OK.into_response();
                }
            };
            let text = event
                .get("text")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_owned();
            let thread_ts = event
                .get("thread_ts")
                .and_then(|v| v.as_str())
                .map(str::to_owned);

            let channel_id = ChannelId {
                kind: ChannelKind::Slack,
                channel,
                thread_ts,
            };

            let inbound = InboundMessage {
                channel: channel_id,
                user_id,
                user_name: None,
                text,
                at: SystemTime::now(),
                raw: event.clone(),
            };

            if let Err(e) = state.bus.publish_inbound(inbound).await {
                warn!("slack: failed to publish inbound message: {e}");
            }
        }
    }

    StatusCode::OK.into_response()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn good_signature(secret: &str, ts: u64, body: &[u8]) -> String {
        let base = format!("v0:{}:{}", ts, String::from_utf8_lossy(body));
        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(base.as_bytes());
        format!("v0={}", hex::encode(mac.finalize().into_bytes()))
    }

    fn now_ts() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }

    #[test]
    fn good_sig_passes() {
        let secret = "8f742231b10e8888abcd99yyyzzz85a5";
        let body = b"{\"type\":\"url_verification\"}";
        let ts = now_ts();
        let sig = good_signature(secret, ts, body);
        assert!(verify_signature(secret, &ts.to_string(), body, &sig).is_ok());
    }

    #[test]
    fn bad_sig_fails() {
        let secret = "8f742231b10e8888abcd99yyyzzz85a5";
        let body = b"{\"type\":\"url_verification\"}";
        let ts = now_ts();
        let bad_sig = "v0=deadbeefdeadbeef";
        let result = verify_signature(secret, &ts.to_string(), body, bad_sig);
        assert!(matches!(result, Err(SignatureError::Mismatch)));
    }

    #[test]
    fn stale_timestamp_rejected() {
        let secret = "secret";
        let body = b"{}";
        let old_ts = now_ts() - 400; // 400 s ago — over the 300 s limit
        let sig = good_signature(secret, old_ts, body);
        let result = verify_signature(secret, &old_ts.to_string(), body, &sig);
        assert!(matches!(result, Err(SignatureError::StaleTimestamp)));
    }

    #[test]
    fn future_timestamp_rejected() {
        let secret = "secret";
        let body = b"{}";
        let future_ts = now_ts() + 400;
        let sig = good_signature(secret, future_ts, body);
        let result = verify_signature(secret, &future_ts.to_string(), body, &sig);
        assert!(matches!(result, Err(SignatureError::StaleTimestamp)));
    }

    #[test]
    fn wrong_secret_fails() {
        let body = b"hello";
        let ts = now_ts();
        let sig = good_signature("correct-secret", ts, body);
        let result = verify_signature("wrong-secret", &ts.to_string(), body, &sig);
        assert!(matches!(result, Err(SignatureError::Mismatch)));
    }

    #[test]
    fn tampered_body_fails() {
        let secret = "s3cr3t";
        let original_body = b"original";
        let tampered_body = b"tampered";
        let ts = now_ts();
        let sig = good_signature(secret, ts, original_body);
        let result = verify_signature(secret, &ts.to_string(), tampered_body, &sig);
        assert!(matches!(result, Err(SignatureError::Mismatch)));
    }
}

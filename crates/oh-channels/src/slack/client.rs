//! Slack Web API client — `chat.postMessage`.
//!
//! Factored behind a `SlackApiClient` trait so tests can inject a fake
//! without hitting live Slack or needing a running HTTP server.

use async_trait::async_trait;
use serde_json::json;
use thiserror::Error;
use tracing::{debug, warn};

use crate::types::OutboundMessage;

/// Errors from the Slack Web API.
#[derive(Debug, Error)]
pub enum SlackClientError {
    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("Slack API returned error: {0}")]
    Api(String),
}

// ---------------------------------------------------------------------------
// Trait: allows substituting a fake in tests
// ---------------------------------------------------------------------------

/// Minimal Slack API surface used by the adapter.
#[async_trait]
pub trait SlackApiClient: Send + Sync {
    /// Post a message to a Slack channel.
    async fn post_message(&self, msg: &OutboundMessage) -> Result<(), SlackClientError>;
}

// ---------------------------------------------------------------------------
// Real implementation backed by reqwest
// ---------------------------------------------------------------------------

/// Production client that calls `https://slack.com/api/chat.postMessage`.
#[derive(Clone)]
pub struct ReqwestSlackClient {
    bot_token: String,
    http: reqwest::Client,
    /// Base URL for the Slack API; overridable in tests.
    base_url: String,
}

impl ReqwestSlackClient {
    /// Create a new client using the given bot token.
    pub fn new(bot_token: impl Into<String>) -> Self {
        Self {
            bot_token: bot_token.into(),
            http: reqwest::Client::new(),
            base_url: "https://slack.com".to_owned(),
        }
    }

    /// Create a client pointing at a custom base URL (useful for wiremock tests).
    pub fn with_base_url(bot_token: impl Into<String>, base_url: impl Into<String>) -> Self {
        Self {
            bot_token: bot_token.into(),
            http: reqwest::Client::new(),
            base_url: base_url.into(),
        }
    }
}

#[async_trait]
impl SlackApiClient for ReqwestSlackClient {
    async fn post_message(&self, msg: &OutboundMessage) -> Result<(), SlackClientError> {
        let channel = &msg.channel.channel;

        let mut body = json!({
            "channel": channel,
            "text": msg.text,
        });

        // Include thread_ts for threaded replies
        if let Some(ts) = &msg.channel.thread_ts {
            body["thread_ts"] = json!(ts);
        }

        // Include Block Kit blocks if provided
        if let Some(blocks) = &msg.blocks {
            body["blocks"] = blocks.clone();
        }

        debug!("slack: posting message to channel {channel}");

        let resp = self
            .http
            .post(format!("{}/api/chat.postMessage", self.base_url))
            .bearer_auth(&self.bot_token)
            .json(&body)
            .send()
            .await?;

        let status = resp.status();
        let json: serde_json::Value = resp.json().await.map_err(SlackClientError::Http)?;

        if !status.is_success() {
            return Err(SlackClientError::Api(format!("HTTP {status}")));
        }
        if json.get("ok").and_then(|v| v.as_bool()) != Some(true) {
            let err = json
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_owned();
            warn!("slack: chat.postMessage returned ok=false: {err}");
            return Err(SlackClientError::Api(err));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests — using wiremock to mock the Slack API
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ChannelId, ChannelKind};
    use wiremock::{
        matchers::{method, path},
        Mock, MockServer, ResponseTemplate,
    };

    fn make_outbound(channel: &str, text: &str) -> OutboundMessage {
        OutboundMessage {
            channel: ChannelId::new(ChannelKind::Slack, channel),
            text: text.into(),
            blocks: None,
        }
    }

    #[tokio::test]
    async fn post_message_success() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/chat.postMessage"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({ "ok": true, "ts": "123.456" })),
            )
            .mount(&server)
            .await;

        let client = ReqwestSlackClient::with_base_url("xoxb-test", server.uri());
        let msg = make_outbound("C001", "hello world");
        client.post_message(&msg).await.unwrap();
    }

    #[tokio::test]
    async fn post_message_api_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/chat.postMessage"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({ "ok": false, "error": "channel_not_found" })),
            )
            .mount(&server)
            .await;

        let client = ReqwestSlackClient::with_base_url("xoxb-test", server.uri());
        let msg = make_outbound("CBAD", "oops");
        let err = client.post_message(&msg).await.unwrap_err();
        assert!(matches!(err, SlackClientError::Api(_)));
        let msg_str = err.to_string();
        assert!(msg_str.contains("channel_not_found"), "got: {msg_str}");
    }

    #[tokio::test]
    async fn post_message_with_thread_ts() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/chat.postMessage"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({ "ok": true })),
            )
            .mount(&server)
            .await;

        let client = ReqwestSlackClient::with_base_url("xoxb-test", server.uri());
        let mut msg = make_outbound("C001", "threaded reply");
        msg.channel.thread_ts = Some("111.222".into());
        client.post_message(&msg).await.unwrap();
    }
}

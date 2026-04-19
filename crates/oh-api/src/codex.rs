//! Codex ChatGPT OAuth provider — OpenAI Responses API via ChatGPT backend.
//!
//! Implements `StreamingApiClient` by speaking the OpenAI Responses API
//! protocol (`POST /responses`) used by the ChatGPT backend at
//! `chatgpt.com/backend-api/codex`. This lets ChatGPT Plus / Pro subscribers
//! use their subscription quota for API access without an `OPENAI_API_KEY`.
//!
//! Token lifecycle:
//! - Tokens loaded from env vars (`CODEX_ACCESS_TOKEN`, `CODEX_REFRESH_TOKEN`).
//! - The access token is a short-lived JWT; `exp` claim is parsed for expiry.
//! - On 401 (or pre-expiry), the refresh token is exchanged at
//!   `POST https://auth.openai.com/oauth/token` with
//!   `grant_type=refresh_token` and the Codex CLI client_id.
//!
//! # Warning
//!
//! The ChatGPT backend endpoint is a **private, undocumented API**. Using
//! subscriber OAuth tokens from a third-party application may violate
//! OpenAI's Terms of Service. Provided as-is.

use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use eventsource_stream::Eventsource;
use futures::{Stream, StreamExt};
use oh_types::api::*;
use oh_types::messages::{ContentBlock, ConversationMessage, Role, TextBlock, ToolUseBlock};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::{Mutex, RwLock};
use tracing::{info_span, warn, Instrument};

use crate::client::StreamingApiClient;

const CHATGPT_BACKEND_URL: &str = "https://chatgpt.com/backend-api/codex";
const REFRESH_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
/// Refresh the access token if it expires within this many seconds.
const REFRESH_LEEWAY_SECS: u64 = 120;

/// Errors raised when constructing the provider.
#[derive(Debug, thiserror::Error)]
pub enum CodexError {
    #[error("missing env var {0}")]
    MissingEnv(&'static str),
}

/// Tokens held behind an RwLock so concurrent requests can refresh safely.
#[derive(Debug, Clone)]
struct CodexTokens {
    access_token: String,
    refresh_token: String,
    /// Unix seconds at which the JWT expires, if known.
    expires_at: Option<u64>,
}

/// OpenAI Codex ChatGPT provider (OAuth flat-rate API access).
pub struct CodexApiClient {
    http: reqwest::Client,
    base_url: String,
    tokens: Arc<RwLock<CodexTokens>>,
    /// Serializes concurrent refresh attempts so only one request hits the
    /// refresh endpoint at a time.
    refresh_lock: Mutex<()>,
}

impl CodexApiClient {
    /// Construct from env vars: `CODEX_ACCESS_TOKEN`, `CODEX_REFRESH_TOKEN`,
    /// optional `CODEX_BASE_URL` (defaults to the ChatGPT backend).
    pub fn from_env() -> Result<Self, CodexError> {
        let access = std::env::var("CODEX_ACCESS_TOKEN")
            .map_err(|_| CodexError::MissingEnv("CODEX_ACCESS_TOKEN"))?;
        let refresh = std::env::var("CODEX_REFRESH_TOKEN")
            .map_err(|_| CodexError::MissingEnv("CODEX_REFRESH_TOKEN"))?;
        let base_url = std::env::var("CODEX_BASE_URL")
            .unwrap_or_else(|_| CHATGPT_BACKEND_URL.to_string());
        let expires_at = jwt_expiry(&access);
        Ok(Self {
            http: reqwest::Client::new(),
            base_url: base_url.trim_end_matches('/').to_string(),
            tokens: Arc::new(RwLock::new(CodexTokens {
                access_token: access,
                refresh_token: refresh,
                expires_at,
            })),
            refresh_lock: Mutex::new(()),
        })
    }

    /// Return the current access token, refreshing proactively if near expiry.
    async fn current_access_token(&self) -> Result<String, ApiError> {
        // Fast path: read under a read lock.
        let (access, expires_at) = {
            let t = self.tokens.read().await;
            (t.access_token.clone(), t.expires_at)
        };

        if !should_refresh(expires_at) {
            return Ok(access);
        }

        // Acquire refresh lock. Double-check expiry after acquiring (another
        // task may have refreshed in the meantime).
        let _guard = self.refresh_lock.lock().await;
        let (access2, expires_at2, refresh_token) = {
            let t = self.tokens.read().await;
            (
                t.access_token.clone(),
                t.expires_at,
                t.refresh_token.clone(),
            )
        };
        if !should_refresh(expires_at2) {
            return Ok(access2);
        }

        match self.do_refresh(&refresh_token).await {
            Ok((new_access, new_refresh)) => {
                let new_exp = jwt_expiry(&new_access);
                let mut t = self.tokens.write().await;
                t.access_token = new_access.clone();
                if let Some(rt) = new_refresh {
                    t.refresh_token = rt;
                }
                t.expires_at = new_exp;
                Ok(new_access)
            }
            Err(e) => {
                warn!("Codex OAuth refresh failed: {e}; using existing token");
                Ok(access2)
            }
        }
    }

    /// POST to the OAuth refresh endpoint, returning `(access, refresh)`.
    async fn do_refresh(
        &self,
        refresh_token: &str,
    ) -> Result<(String, Option<String>), String> {
        let body = json!({
            "client_id": CLIENT_ID,
            "grant_type": "refresh_token",
            "refresh_token": refresh_token,
        });

        let resp = self
            .http
            .post(REFRESH_TOKEN_URL)
            .header("Content-Type", "application/json")
            .json(&body)
            .timeout(Duration::from_secs(10))
            .send()
            .await
            .map_err(|e| format!("refresh request failed: {e}"))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(format!("refresh HTTP {status}: {body}"));
        }

        #[derive(Deserialize)]
        struct RefreshResponse {
            access_token: String,
            refresh_token: Option<String>,
        }
        let parsed: RefreshResponse = resp
            .json()
            .await
            .map_err(|e| format!("refresh JSON parse failed: {e}"))?;
        Ok((parsed.access_token, parsed.refresh_token))
    }

    /// Single attempt at calling `/responses` with the current token.
    async fn complete_once(
        &self,
        request: &ApiMessageRequest,
    ) -> Result<(ConversationMessage, UsageSnapshot, Option<String>), ApiError> {
        let span = info_span!("codex_api_request", model = %request.model);

        async {
            let url = format!("{}/responses", self.base_url);
            let body = build_responses_body(request);

            // Fresh token (proactively refreshed if needed).
            let access = self.current_access_token().await?;
            let resp = self.send_once(&url, &access, &body).await?;

            if resp.status().as_u16() == 401 {
                // Reactive refresh on 401.
                let _guard = self.refresh_lock.lock().await;
                let (existing_access, existing_refresh) = {
                    let t = self.tokens.read().await;
                    (t.access_token.clone(), t.refresh_token.clone())
                };

                // If another task already rotated the token since we sent the
                // request, retry directly with the new token.
                if existing_access != access {
                    drop(resp);
                    let retry = self.send_once(&url, &existing_access, &body).await?;
                    return Self::handle_response(retry, &url).await;
                }

                match self.do_refresh(&existing_refresh).await {
                    Ok((new_access, new_refresh)) => {
                        let new_exp = jwt_expiry(&new_access);
                        {
                            let mut t = self.tokens.write().await;
                            t.access_token = new_access.clone();
                            if let Some(rt) = new_refresh {
                                t.refresh_token = rt;
                            }
                            t.expires_at = new_exp;
                        }
                        drop(resp);
                        let retry = self.send_once(&url, &new_access, &body).await?;
                        return Self::handle_response(retry, &url).await;
                    }
                    Err(e) => {
                        let body_text = resp.text().await.unwrap_or_default();
                        return Err(ApiError::Authentication(format!(
                            "refresh failed ({e}); original 401 body: {body_text}"
                        )));
                    }
                }
            }

            Self::handle_response(resp, &url).await
        }
        .instrument(span)
        .await
    }

    async fn send_once(
        &self,
        url: &str,
        access_token: &str,
        body: &Value,
    ) -> Result<reqwest::Response, ApiError> {
        self.http
            .post(url)
            .bearer_auth(access_token)
            .header("Content-Type", "application/json")
            .header("Accept", "text/event-stream")
            .json(body)
            .timeout(Duration::from_secs(180))
            .send()
            .await
            .map_err(|e| ApiError::Network(e.to_string()))
    }

    async fn handle_response(
        resp: reqwest::Response,
        url: &str,
    ) -> Result<(ConversationMessage, UsageSnapshot, Option<String>), ApiError> {
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            if status.as_u16() == 401 || status.as_u16() == 403 {
                return Err(ApiError::Authentication(text));
            }
            if status.as_u16() == 429 {
                return Err(ApiError::RateLimit(text));
            }
            return Err(ApiError::Request(format!(
                "status {} from {url}: {text}",
                status.as_u16()
            )));
        }

        let stream = resp.bytes_stream().map(|chunk| chunk.map_err(|e| e.to_string()));
        parse_sse_stream(stream).await
    }
}

#[async_trait]
impl StreamingApiClient for CodexApiClient {
    async fn stream_message(
        &self,
        request: ApiMessageRequest,
    ) -> Result<
        Pin<Box<dyn Stream<Item = Result<ApiStreamEvent, ApiError>> + Send + '_>>,
        ApiError,
    > {
        let mut last_error: Option<ApiError> = None;

        for attempt in 0..=MAX_RETRIES {
            match self.complete_once(&request).await {
                Ok((message, usage, stop_reason)) => {
                    let events = vec![Ok(ApiStreamEvent::MessageComplete(
                        ApiMessageCompleteEvent {
                            message,
                            usage,
                            stop_reason,
                        },
                    ))];
                    return Ok(Box::pin(futures::stream::iter(events)));
                }
                Err(e) => {
                    if attempt >= MAX_RETRIES || !is_retryable(&e) {
                        return Err(e);
                    }
                    let delay = get_retry_delay(attempt);
                    warn!(
                        attempt = attempt + 1,
                        max_retries = MAX_RETRIES + 1,
                        delay_secs = delay,
                        "Codex API request failed, retrying"
                    );
                    tokio::time::sleep(Duration::from_secs_f64(delay)).await;
                    last_error = Some(e);
                }
            }
        }

        Err(last_error.unwrap_or(ApiError::Request("unknown error".into())))
    }
}

// ── Request shape: Anthropic-internal → Responses API ──────────────────

fn build_responses_body(request: &ApiMessageRequest) -> Value {
    // system_prompt → instructions (top-level)
    let instructions = request.system_prompt.clone().unwrap_or_default();

    // Messages → input items
    let mut input: Vec<Value> = Vec::new();
    for msg in &request.messages {
        append_input_items(msg, &mut input);
    }

    // Tools: Anthropic format → Responses API function format.
    // Note: Responses API uses flat { type, name, description, parameters },
    // NOT nested under "function".
    let tools: Vec<Value> = request.tools.iter().map(convert_tool).collect();

    let mut body = json!({
        "model": request.model,
        "instructions": instructions,
        "input": input,
        "stream": true,
        "store": false,
    });

    if !tools.is_empty() {
        body["tools"] = Value::Array(tools);
        body["tool_choice"] = json!("auto");
    }

    body
}

fn convert_tool(tool: &Value) -> Value {
    // If the tool was already produced as OpenAI Chat Completions "function"
    // nested form, unwrap it to Responses flat form.
    if tool.get("type").and_then(|v| v.as_str()) == Some("function") {
        if let Some(func) = tool.get("function") {
            return json!({
                "type": "function",
                "name": func.get("name").cloned().unwrap_or(json!("")),
                "description": func.get("description").cloned().unwrap_or(json!("")),
                "parameters": func
                    .get("parameters")
                    .cloned()
                    .unwrap_or(json!({"type": "object", "properties": {}})),
            });
        }
        // Already flat — pass through.
        return tool.clone();
    }

    // Anthropic format: { name, description, input_schema }
    let name = tool.get("name").cloned().unwrap_or(json!(""));
    let description = tool.get("description").cloned().unwrap_or(json!(""));
    let parameters = tool
        .get("input_schema")
        .cloned()
        .unwrap_or(json!({"type": "object", "properties": {}}));
    json!({
        "type": "function",
        "name": name,
        "description": description,
        "parameters": parameters,
    })
}

fn append_input_items(msg: &ConversationMessage, out: &mut Vec<Value>) {
    match msg.role {
        Role::User => {
            // Tool results become function_call_output items.
            // Text blocks become one user `message` with input_text content.
            let mut texts: Vec<String> = Vec::new();
            for block in &msg.content {
                match block {
                    ContentBlock::Text(t) => texts.push(t.text.clone()),
                    ContentBlock::ToolResult(tr) => {
                        out.push(json!({
                            "type": "function_call_output",
                            "call_id": tr.tool_use_id,
                            "output": tr.content,
                        }));
                    }
                    _ => {}
                }
            }
            if !texts.is_empty() {
                out.push(json!({
                    "type": "message",
                    "role": "user",
                    "content": [{
                        "type": "input_text",
                        "text": texts.join(""),
                    }],
                }));
            }
        }
        Role::Assistant => {
            // Text blocks → assistant message (output_text).
            // ToolUse blocks → function_call items.
            let mut texts: Vec<String> = Vec::new();
            let mut tool_uses: Vec<&ToolUseBlock> = Vec::new();
            for block in &msg.content {
                match block {
                    ContentBlock::Text(t) => texts.push(t.text.clone()),
                    ContentBlock::ToolUse(tu) => tool_uses.push(tu),
                    _ => {}
                }
            }
            let text = texts.join("");
            if !text.is_empty() {
                out.push(json!({
                    "type": "message",
                    "role": "assistant",
                    "content": [{
                        "type": "output_text",
                        "text": text,
                    }],
                }));
            }
            for tu in tool_uses {
                let args = serde_json::to_string(&tu.input).unwrap_or_else(|_| "{}".into());
                out.push(json!({
                    "type": "function_call",
                    "name": tu.name,
                    "arguments": args,
                    "call_id": tu.id,
                }));
            }
        }
    }
}

// ── SSE parsing ────────────────────────────────────────────────────────

#[derive(Default)]
struct ResponsesResult {
    text: String,
    /// Keyed by item_id (e.g. "fc_...") which delta events reference.
    pending: std::collections::HashMap<String, PendingToolCall>,
    input_tokens: u64,
    output_tokens: u64,
    stop_reason: Option<String>,
}

struct PendingToolCall {
    call_id: String,
    name: String,
    arguments: String,
}

async fn parse_sse_stream<S>(
    stream: S,
) -> Result<(ConversationMessage, UsageSnapshot, Option<String>), ApiError>
where
    S: Stream<Item = Result<bytes::Bytes, String>> + Unpin,
{
    let mut result = ResponsesResult::default();
    let mut stream = stream.eventsource();
    let idle = Duration::from_secs(120);

    loop {
        match tokio::time::timeout(idle, stream.next()).await {
            Ok(Some(Ok(event))) => {
                let data = event.data.trim();
                if data.is_empty() {
                    continue;
                }
                let parsed: Value = match serde_json::from_str(data) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                if handle_event(&mut result, event.event.as_str(), &parsed) {
                    break;
                }
            }
            Ok(Some(Err(e))) => {
                return Err(ApiError::Network(format!("SSE read error: {e}")));
            }
            Ok(None) => break,
            Err(_) => {
                return Err(ApiError::Network(format!(
                    "timed out waiting for SSE event after {}s",
                    idle.as_secs()
                )));
            }
        }
    }

    // Build the ConversationMessage.
    let mut blocks: Vec<ContentBlock> = Vec::new();
    if !result.text.is_empty() {
        blocks.push(ContentBlock::Text(TextBlock::new(result.text)));
    }
    for tc in result.pending.into_values() {
        let input: std::collections::HashMap<String, serde_json::Value> =
            serde_json::from_str(&tc.arguments).unwrap_or_default();
        blocks.push(ContentBlock::ToolUse(ToolUseBlock {
            r#type: "tool_use".into(),
            id: tc.call_id,
            name: tc.name,
            input,
        }));
    }

    let usage = UsageSnapshot {
        input_tokens: result.input_tokens,
        output_tokens: result.output_tokens,
        ..Default::default()
    };
    let stop_reason = result.stop_reason.or_else(|| {
        // If we produced tool calls, reflect that; otherwise end_turn.
        if blocks.iter().any(|b| matches!(b, ContentBlock::ToolUse(_))) {
            Some("tool_use".into())
        } else {
            Some("end_turn".into())
        }
    });

    Ok((
        ConversationMessage {
            role: Role::Assistant,
            content: blocks,
        },
        usage,
        stop_reason,
    ))
}

/// Returns true when the stream should terminate (response.completed).
fn handle_event(result: &mut ResponsesResult, event_type: &str, parsed: &Value) -> bool {
    match event_type {
        "response.output_text.delta" => {
            if let Some(delta) = parsed.get("delta").and_then(|d| d.as_str()) {
                result.text.push_str(delta);
            }
        }
        "response.output_item.added" => {
            let item = parsed.get("item").unwrap_or(parsed);
            if item.get("type").and_then(|t| t.as_str()) == Some("function_call") {
                let item_id = item
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let call_id = item
                    .get("call_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let name = item
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                result.pending.entry(item_id).or_insert(PendingToolCall {
                    call_id,
                    name,
                    arguments: String::new(),
                });
            }
        }
        "response.function_call_arguments.delta" => {
            if let (Some(item_id), Some(delta)) = (
                parsed.get("item_id").and_then(|v| v.as_str()),
                parsed.get("delta").and_then(|d| d.as_str()),
            ) {
                if let Some(entry) = result.pending.get_mut(item_id) {
                    entry.arguments.push_str(delta);
                }
            }
        }
        "response.completed" => {
            if let Some(resp) = parsed.get("response") {
                if let Some(usage) = resp.get("usage") {
                    if let Some(v) = usage.get("input_tokens").and_then(|v| v.as_u64()) {
                        result.input_tokens = v;
                    }
                    if let Some(v) = usage.get("output_tokens").and_then(|v| v.as_u64()) {
                        result.output_tokens = v;
                    }
                }
                if let Some(status) = resp.get("status").and_then(|v| v.as_str()) {
                    result.stop_reason = Some(match status {
                        "completed" => "end_turn".into(),
                        other => other.into(),
                    });
                }
            }
            return true;
        }
        _ => {}
    }
    false
}

// ── JWT / time helpers ─────────────────────────────────────────────────

/// Extract the `exp` claim from a JWT without signature verification.
fn jwt_expiry(jwt: &str) -> Option<u64> {
    let payload_b64 = jwt.split('.').nth(1)?;
    let decoded = URL_SAFE_NO_PAD.decode(payload_b64).ok()?;
    let v: Value = serde_json::from_slice(&decoded).ok()?;
    v.get("exp").and_then(|e| e.as_u64())
}

fn should_refresh(expires_at: Option<u64>) -> bool {
    match expires_at {
        Some(exp) => {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            now + REFRESH_LEEWAY_SECS >= exp
        }
        None => false,
    }
}

// ── Retry helpers (mirrors anthropic.rs/openai.rs) ─────────────────────

fn is_retryable(err: &ApiError) -> bool {
    matches!(err, ApiError::RateLimit(_) | ApiError::Network(_))
        || matches!(err, ApiError::Request(msg) if {
            RETRYABLE_STATUS_CODES.iter().any(|code| msg.contains(&code.to_string()))
        })
}

fn get_retry_delay(attempt: u32) -> f64 {
    let delay = (BASE_DELAY_SECS * 2.0_f64.powi(attempt as i32)).min(MAX_DELAY_SECS);
    let jitter = delay * 0.25 * rand_fraction();
    delay + jitter
}

fn rand_fraction() -> f64 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();
    (nanos as f64) / (u32::MAX as f64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn user(text: &str) -> ConversationMessage {
        ConversationMessage::from_user_text(text)
    }

    #[test]
    fn test_build_responses_body_basic() {
        let req = ApiMessageRequest {
            model: "gpt-5.4".into(),
            messages: vec![user("hello")],
            system_prompt: Some("You are helpful.".into()),
            max_tokens: 4096,
            tools: vec![],
        };
        let body = build_responses_body(&req);
        assert_eq!(body["model"], "gpt-5.4");
        assert_eq!(body["instructions"], "You are helpful.");
        assert_eq!(body["store"], false);
        assert_eq!(body["stream"], true);
        assert!(body.get("temperature").is_none());
        assert!(body.get("max_output_tokens").is_none());
        let input = body["input"].as_array().unwrap();
        assert_eq!(input.len(), 1);
        assert_eq!(input[0]["type"], "message");
        assert_eq!(input[0]["role"], "user");
        assert_eq!(input[0]["content"][0]["type"], "input_text");
        assert_eq!(input[0]["content"][0]["text"], "hello");
    }

    #[test]
    fn test_build_responses_body_tool_result_roundtrip() {
        let msg_user = ConversationMessage {
            role: Role::User,
            content: vec![ContentBlock::ToolResult(
                oh_types::messages::ToolResultBlock::new("call_abc", "result text", false),
            )],
        };
        let req = ApiMessageRequest {
            model: "gpt-5.4".into(),
            messages: vec![msg_user],
            system_prompt: None,
            max_tokens: 4096,
            tools: vec![],
        };
        let body = build_responses_body(&req);
        let input = body["input"].as_array().unwrap();
        assert_eq!(input.len(), 1);
        assert_eq!(input[0]["type"], "function_call_output");
        assert_eq!(input[0]["call_id"], "call_abc");
        assert_eq!(input[0]["output"], "result text");
    }

    #[test]
    fn test_build_responses_body_assistant_tool_use() {
        let mut inp = HashMap::new();
        inp.insert("query".into(), json!("rust"));
        let tu = ToolUseBlock {
            r#type: "tool_use".into(),
            id: "call_xyz".into(),
            name: "search".into(),
            input: inp,
        };
        let msg = ConversationMessage {
            role: Role::Assistant,
            content: vec![
                ContentBlock::Text(TextBlock::new("let me look")),
                ContentBlock::ToolUse(tu),
            ],
        };
        let req = ApiMessageRequest {
            model: "gpt-5.4".into(),
            messages: vec![msg],
            system_prompt: None,
            max_tokens: 4096,
            tools: vec![],
        };
        let body = build_responses_body(&req);
        let input = body["input"].as_array().unwrap();
        assert_eq!(input.len(), 2);
        assert_eq!(input[0]["type"], "message");
        assert_eq!(input[0]["role"], "assistant");
        assert_eq!(input[0]["content"][0]["type"], "output_text");
        assert_eq!(input[1]["type"], "function_call");
        assert_eq!(input[1]["name"], "search");
        assert_eq!(input[1]["call_id"], "call_xyz");
    }

    #[test]
    fn test_convert_tool_anthropic_schema() {
        let tool = json!({
            "name": "bash",
            "description": "run a command",
            "input_schema": {
                "type": "object",
                "properties": {"cmd": {"type": "string"}},
                "required": ["cmd"]
            }
        });
        let converted = convert_tool(&tool);
        assert_eq!(converted["type"], "function");
        assert_eq!(converted["name"], "bash");
        assert_eq!(converted["description"], "run a command");
        // Flat parameters (NOT nested under "function")
        assert!(converted.get("function").is_none());
        assert_eq!(converted["parameters"]["type"], "object");
    }

    #[test]
    fn test_convert_tool_openai_nested_unwraps() {
        let tool = json!({
            "type": "function",
            "function": {
                "name": "search",
                "description": "search web",
                "parameters": {"type": "object"}
            }
        });
        let converted = convert_tool(&tool);
        assert_eq!(converted["type"], "function");
        assert_eq!(converted["name"], "search");
        assert!(converted.get("function").is_none());
    }

    #[tokio::test]
    async fn test_parse_sse_text_only() {
        let chunks: Vec<Result<bytes::Bytes, String>> = vec![
            Ok(bytes::Bytes::from_static(
                b"event: response.output_text.delta\ndata: {\"delta\":\"Hello\"}\n\n",
            )),
            Ok(bytes::Bytes::from_static(
                b"event: response.output_text.delta\ndata: {\"delta\":\" world\"}\n\n",
            )),
            Ok(bytes::Bytes::from_static(
                b"event: response.completed\ndata: {\"response\":{\"usage\":{\"input_tokens\":3,\"output_tokens\":2}}}\n\n",
            )),
        ];
        let stream = futures::stream::iter(chunks);
        let (msg, usage, stop) = parse_sse_stream(stream).await.unwrap();
        assert_eq!(msg.text(), "Hello world");
        assert_eq!(usage.input_tokens, 3);
        assert_eq!(usage.output_tokens, 2);
        assert_eq!(stop.as_deref(), Some("end_turn"));
    }

    #[tokio::test]
    async fn test_parse_sse_tool_call() {
        let chunks: Vec<Result<bytes::Bytes, String>> = vec![
            Ok(bytes::Bytes::from_static(
                b"event: response.output_item.added\ndata: {\"item\":{\"id\":\"fc_1\",\"type\":\"function_call\",\"call_id\":\"call_1\",\"name\":\"search\"}}\n\n",
            )),
            Ok(bytes::Bytes::from_static(
                b"event: response.function_call_arguments.delta\ndata: {\"item_id\":\"fc_1\",\"delta\":\"{\\\"q\\\":\"}\n\n",
            )),
            Ok(bytes::Bytes::from_static(
                b"event: response.function_call_arguments.delta\ndata: {\"item_id\":\"fc_1\",\"delta\":\"\\\"rust\\\"}\"}\n\n",
            )),
            Ok(bytes::Bytes::from_static(
                b"event: response.completed\ndata: {\"response\":{\"usage\":{\"input_tokens\":10,\"output_tokens\":5}}}\n\n",
            )),
        ];
        let stream = futures::stream::iter(chunks);
        let (msg, _usage, stop) = parse_sse_stream(stream).await.unwrap();
        assert_eq!(msg.tool_uses().len(), 1);
        let tu = msg.tool_uses()[0];
        assert_eq!(tu.name, "search");
        assert_eq!(tu.id, "call_1");
        assert_eq!(tu.input.get("q").and_then(|v| v.as_str()), Some("rust"));
        assert_eq!(stop.as_deref(), Some("tool_use"));
    }

    #[test]
    fn test_jwt_expiry_extracts_exp() {
        // Header+payload+sig, base64 URL-safe, no padding. Payload: {"exp":1234567890}
        // {"alg":"HS256"} → eyJhbGciOiJIUzI1NiJ9
        // {"exp":1234567890} → eyJleHAiOjEyMzQ1Njc4OTB9
        let jwt = "eyJhbGciOiJIUzI1NiJ9.eyJleHAiOjEyMzQ1Njc4OTB9.sig";
        assert_eq!(jwt_expiry(jwt), Some(1234567890));
    }

    #[test]
    fn test_jwt_expiry_missing_returns_none() {
        let jwt = "eyJhbGciOiJIUzI1NiJ9.eyJmb28iOiJiYXIifQ.sig";
        assert_eq!(jwt_expiry(jwt), None);
    }

    #[test]
    fn test_should_refresh_past_expiry() {
        let past = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            - 100;
        assert!(should_refresh(Some(past)));
    }

    #[test]
    fn test_should_refresh_far_future() {
        let future = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 3600;
        assert!(!should_refresh(Some(future)));
    }

    #[test]
    fn test_should_refresh_within_leeway() {
        let almost = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 60; // within 120s leeway
        assert!(should_refresh(Some(almost)));
    }

    #[test]
    fn test_should_refresh_none_returns_false() {
        assert!(!should_refresh(None));
    }

    #[test]
    fn test_codex_error_missing_env_display() {
        let e = CodexError::MissingEnv("CODEX_ACCESS_TOKEN");
        assert_eq!(format!("{e}"), "missing env var CODEX_ACCESS_TOKEN");
    }
}

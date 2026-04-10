//! Hook execution engine — dispatches command, http, prompt, and agent hooks.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use oh_types::api::{ApiMessageRequest, UsageSnapshot};
use oh_types::hooks::*;
use oh_types::messages::ConversationMessage;
use opentelemetry::KeyValue;
use tokio::sync::RwLock;
use tracing::{info_span, warn, Instrument};

use crate::loader::HookRegistry;
use crate::matching::{inject_arguments, matches_hook};
use crate::HookExecutorTrait;
use oh_api::StreamingApiClient;

/// Context passed into hook execution.
pub struct HookExecutionContext {
    pub cwd: PathBuf,
    pub api_client: Arc<dyn StreamingApiClient>,
    pub default_model: String,
}

/// Execute hooks for lifecycle events.
pub struct HookExecutor {
    registry: Arc<RwLock<HookRegistry>>,
    context: HookExecutionContext,
}

impl HookExecutor {
    pub fn new(registry: HookRegistry, context: HookExecutionContext) -> Self {
        Self {
            registry: Arc::new(RwLock::new(registry)),
            context,
        }
    }

    /// Get a clone of the Arc to the registry (for tools that need to modify hooks).
    pub fn registry_handle(&self) -> Arc<RwLock<HookRegistry>> {
        self.registry.clone()
    }

    /// Replace the active hook registry (for hot-reload).
    pub async fn update_registry(&self, registry: HookRegistry) {
        *self.registry.write().await = registry;
    }

    async fn run_command_hook(
        &self,
        hook: &CommandHookDefinition,
        event: HookEvent,
        payload: &serde_json::Value,
    ) -> HookResult {
        let command = inject_arguments(&hook.command, payload);
        let mut env_vars = std::collections::HashMap::new();
        env_vars.insert(
            "OPENHARNESS_HOOK_EVENT".to_string(),
            event.to_string(),
        );
        env_vars.insert(
            "OPENHARNESS_HOOK_PAYLOAD".to_string(),
            payload.to_string(),
        );

        let result = tokio::time::timeout(
            std::time::Duration::from_secs(hook.timeout_seconds as u64),
            async {
                let output = tokio::process::Command::new("/bin/bash")
                    .arg("-lc")
                    .arg(&command)
                    .current_dir(&self.context.cwd)
                    .envs(env_vars)
                    .output()
                    .await;

                match output {
                    Ok(output) => {
                        let stdout =
                            String::from_utf8_lossy(&output.stdout).trim().to_string();
                        let stderr =
                            String::from_utf8_lossy(&output.stderr).trim().to_string();
                        let combined = [stdout, stderr]
                            .iter()
                            .filter(|s| !s.is_empty())
                            .cloned()
                            .collect::<Vec<_>>()
                            .join("\n");
                        let success = output.status.success();
                        HookResult {
                            hook_type: "command".into(),
                            success,
                            output: combined.clone(),
                            blocked: hook.block_on_failure && !success,
                            reason: if success {
                                String::new()
                            } else {
                                combined
                            },
                            metadata: {
                                let mut m = HashMap::new();
                                m.insert(
                                    "returncode".into(),
                                    serde_json::json!(output.status.code().unwrap_or(-1)),
                                );
                                m
                            },
                        }
                    }
                    Err(e) => HookResult {
                        hook_type: "command".into(),
                        success: false,
                        output: e.to_string(),
                        blocked: hook.block_on_failure,
                        reason: e.to_string(),
                        ..Default::default()
                    },
                }
            },
        )
        .await;

        match result {
            Ok(r) => r,
            Err(_) => HookResult {
                hook_type: "command".into(),
                success: false,
                blocked: hook.block_on_failure,
                reason: format!(
                    "command hook timed out after {}s",
                    hook.timeout_seconds
                ),
                ..Default::default()
            },
        }
    }

    async fn run_http_hook(
        &self,
        hook: &HttpHookDefinition,
        event: HookEvent,
        payload: &serde_json::Value,
    ) -> HookResult {
        let client = reqwest::Client::new();
        let body = serde_json::json!({
            "event": event.to_string(),
            "payload": payload,
        });

        let result = tokio::time::timeout(
            std::time::Duration::from_secs(hook.timeout_seconds as u64),
            async {
                let mut req = client.post(&hook.url).json(&body);
                for (k, v) in &hook.headers {
                    req = req.header(k.as_str(), v.as_str());
                }
                req.send().await
            },
        )
        .await;

        match result {
            Ok(Ok(response)) => {
                let success = response.status().is_success();
                let status_code = response.status().as_u16();
                let output = response.text().await.unwrap_or_default();
                HookResult {
                    hook_type: "http".into(),
                    success,
                    output: output.clone(),
                    blocked: hook.block_on_failure && !success,
                    reason: if success {
                        String::new()
                    } else {
                        format!("http hook returned {status_code}")
                    },
                    metadata: {
                        let mut m = HashMap::new();
                        m.insert("status_code".into(), serde_json::json!(status_code));
                        m
                    },
                }
            }
            Ok(Err(e)) => HookResult {
                hook_type: "http".into(),
                success: false,
                blocked: hook.block_on_failure,
                reason: e.to_string(),
                ..Default::default()
            },
            Err(_) => HookResult {
                hook_type: "http".into(),
                success: false,
                blocked: hook.block_on_failure,
                reason: format!("http hook timed out after {}s", hook.timeout_seconds),
                ..Default::default()
            },
        }
    }

    async fn run_prompt_hook(
        &self,
        prompt_text: &str,
        model: Option<&str>,
        timeout_seconds: u32,
        block_on_failure: bool,
        agent_mode: bool,
        event: HookEvent,
        payload: &serde_json::Value,
    ) -> HookResult {
        let prompt = inject_arguments(prompt_text, payload);
        let hook_type = if agent_mode { "agent" } else { "prompt" };

        let mut system = "You are validating whether a hook condition passes in OpenHarness. \
            Return strict JSON: {\"ok\": true} or {\"ok\": false, \"reason\": \"...\"}."
            .to_string();
        if agent_mode {
            system += " Be more thorough and reason over the payload before deciding.";
        }

        let request = ApiMessageRequest {
            model: model
                .unwrap_or(&self.context.default_model)
                .to_string(),
            messages: vec![ConversationMessage::from_user_text(&prompt)],
            system_prompt: Some(system),
            max_tokens: 512,
            tools: Vec::new(),
        };

        let result = tokio::time::timeout(
            std::time::Duration::from_secs(timeout_seconds as u64),
            self.context.api_client.stream_message(request),
        )
        .await;

        let text = match result {
            Ok(Ok(mut stream)) => {
                use futures::StreamExt;
                let mut text = String::new();
                while let Some(event) = stream.next().await {
                    if let Ok(oh_types::api::ApiStreamEvent::MessageComplete(e)) = event {
                        text = e.message.text();
                    }
                }
                text
            }
            Ok(Err(e)) => {
                return HookResult {
                    hook_type: hook_type.into(),
                    success: false,
                    blocked: block_on_failure,
                    reason: e.to_string(),
                    ..Default::default()
                };
            }
            Err(_) => {
                return HookResult {
                    hook_type: hook_type.into(),
                    success: false,
                    blocked: block_on_failure,
                    reason: format!("{hook_type} hook timed out after {timeout_seconds}s"),
                    ..Default::default()
                };
            }
        };

        let parsed = parse_hook_json(&text);
        if parsed
            .get("ok")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            HookResult {
                hook_type: hook_type.into(),
                success: true,
                output: text,
                ..Default::default()
            }
        } else {
            HookResult {
                hook_type: hook_type.into(),
                success: false,
                output: text,
                blocked: block_on_failure,
                reason: parsed
                    .get("reason")
                    .and_then(|v| v.as_str())
                    .unwrap_or("hook rejected the event")
                    .to_string(),
                ..Default::default()
            }
        }
    }
}

#[async_trait]
impl HookExecutorTrait for HookExecutor {
    async fn execute(
        &self,
        event: HookEvent,
        payload: serde_json::Value,
    ) -> AggregatedHookResult {
        let registry = self.registry.read().await;
        let hooks = registry.get(&event);
        let mut results = Vec::new();

        for hook in hooks {
            if !matches_hook(hook, &payload) {
                continue;
            }

            let span = info_span!(
                "hook_execution",
                hook.type_ = hook.hook_type(),
                hook.event = %event,
            );

            let start = Instant::now();

            let result = async {
                match hook {
                    HookDefinition::Command(h) => {
                        self.run_command_hook(h, event, &payload).await
                    }
                    HookDefinition::Http(h) => {
                        self.run_http_hook(h, event, &payload).await
                    }
                    HookDefinition::Prompt(h) => {
                        self.run_prompt_hook(
                            &h.prompt,
                            h.model.as_deref(),
                            h.timeout_seconds,
                            h.block_on_failure,
                            false,
                            event,
                            &payload,
                        )
                        .await
                    }
                    HookDefinition::Agent(h) => {
                        self.run_prompt_hook(
                            &h.prompt,
                            h.model.as_deref(),
                            h.timeout_seconds,
                            h.block_on_failure,
                            true,
                            event,
                            &payload,
                        )
                        .await
                    }
                }
            }
            .instrument(span)
            .await;

            let elapsed = start.elapsed().as_secs_f64();
            oh_telemetry::HOOK_EXECUTION_DURATION.record(
                elapsed,
                &[
                    KeyValue::new("hook_type", hook.hook_type().to_string()),
                    KeyValue::new("event", event.to_string()),
                ],
            );

            if result.blocked {
                oh_telemetry::HOOK_BLOCKED_COUNT.add(
                    1,
                    &[KeyValue::new("event", event.to_string())],
                );
            }

            results.push(result);
        }

        AggregatedHookResult { results }
    }
}

/// Parse hook JSON response (public for testing).
pub(crate) fn parse_hook_json(text: &str) -> serde_json::Value {
    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(text) {
        if parsed.get("ok").and_then(|v| v.as_bool()).is_some() {
            return parsed;
        }
    }
    let lowered = text.trim().to_lowercase();
    if matches!(lowered.as_str(), "ok" | "true" | "yes") {
        serde_json::json!({"ok": true})
    } else {
        serde_json::json!({"ok": false, "reason": text.trim()})
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loader::HookRegistry;
    use oh_api::StreamingApiClient;

    /// Stub API client for tests that only exercise command hooks.
    struct StubApiClient;

    #[async_trait]
    impl StreamingApiClient for StubApiClient {
        async fn stream_message(
            &self,
            _request: oh_types::api::ApiMessageRequest,
        ) -> Result<
            std::pin::Pin<
                Box<
                    dyn futures::Stream<
                            Item = Result<oh_types::api::ApiStreamEvent, oh_types::api::ApiError>,
                        > + Send
                        + '_,
                >,
            >,
            oh_types::api::ApiError,
        > {
            Err(oh_types::api::ApiError::Request("stub".into()))
        }
    }

    fn make_command_hook(command: &str, timeout: u32, block: bool) -> HookDefinition {
        HookDefinition::Command(CommandHookDefinition {
            r#type: "command".into(),
            command: command.into(),
            timeout_seconds: timeout,
            matcher: None,
            block_on_failure: block,
        })
    }

    fn make_executor(registry: HookRegistry) -> HookExecutor {
        let context = HookExecutionContext {
            cwd: std::env::temp_dir(),
            api_client: Arc::new(StubApiClient),
            default_model: "test-model".into(),
        };
        HookExecutor::new(registry, context)
    }

    // -- parse_hook_json tests --

    #[test]
    fn test_parse_hook_json_valid_ok_true() {
        let result = parse_hook_json(r#"{"ok": true}"#);
        assert_eq!(result.get("ok").and_then(|v| v.as_bool()), Some(true));
    }

    #[test]
    fn test_parse_hook_json_true_string() {
        let result = parse_hook_json("true");
        assert_eq!(result.get("ok").and_then(|v| v.as_bool()), Some(true));
    }

    #[test]
    fn test_parse_hook_json_invalid_text() {
        let result = parse_hook_json("something weird");
        assert_eq!(result.get("ok").and_then(|v| v.as_bool()), Some(false));
        assert!(result.get("reason").is_some());
    }

    #[test]
    fn test_parse_hook_json_ok_false_with_reason() {
        let result = parse_hook_json(r#"{"ok": false, "reason": "denied"}"#);
        assert_eq!(result.get("ok").and_then(|v| v.as_bool()), Some(false));
        assert_eq!(result.get("reason").and_then(|v| v.as_str()), Some("denied"));
    }

    // -- command hook execution tests --

    #[tokio::test]
    async fn test_command_hook_echo_success() {
        let mut reg = HookRegistry::new();
        reg.register(HookEvent::PreToolUse, make_command_hook("/bin/echo hello", 30, false));
        let executor = make_executor(reg);

        let result = executor
            .execute(HookEvent::PreToolUse, serde_json::json!({}))
            .await;

        assert_eq!(result.results.len(), 1);
        assert!(result.results[0].success);
        assert!(result.results[0].output.contains("hello"));
        assert!(!result.blocked());
    }

    #[tokio::test]
    async fn test_command_hook_timeout_blocks() {
        let mut reg = HookRegistry::new();
        reg.register(
            HookEvent::PreToolUse,
            make_command_hook("sleep 60", 1, true),
        );
        let executor = make_executor(reg);

        let result = executor
            .execute(HookEvent::PreToolUse, serde_json::json!({}))
            .await;

        assert_eq!(result.results.len(), 1);
        assert!(!result.results[0].success);
        assert!(result.results[0].blocked);
        assert!(result.blocked());
    }

    #[tokio::test]
    async fn test_command_hook_failure_no_block() {
        let mut reg = HookRegistry::new();
        reg.register(
            HookEvent::PreToolUse,
            make_command_hook("exit 1", 5, false),
        );
        let executor = make_executor(reg);

        let result = executor
            .execute(HookEvent::PreToolUse, serde_json::json!({}))
            .await;

        assert_eq!(result.results.len(), 1);
        assert!(!result.results[0].success);
        assert!(!result.results[0].blocked);
        assert!(!result.blocked());
    }

    // -- AggregatedHookResult tests --

    #[test]
    fn test_aggregated_result_not_blocked() {
        let agg = AggregatedHookResult {
            results: vec![HookResult {
                success: true,
                ..Default::default()
            }],
        };
        assert!(!agg.blocked());
        assert_eq!(agg.reason(), "");
    }

    #[test]
    fn test_aggregated_result_blocked_with_reason() {
        let agg = AggregatedHookResult {
            results: vec![HookResult {
                success: false,
                blocked: true,
                reason: "not allowed".into(),
                ..Default::default()
            }],
        };
        assert!(agg.blocked());
        assert_eq!(agg.reason(), "not allowed");
    }
}

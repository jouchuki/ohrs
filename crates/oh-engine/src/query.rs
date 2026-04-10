//! Core tool-aware query loop.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use futures::future::join_all;
use oh_api::StreamingApiClient;
use oh_hooks::{HookEvent, HookExecutorTrait};
use oh_permissions::PermissionChecker;
use oh_tools::ToolRegistry;
use oh_types::api::*;
use oh_types::messages::*;
use oh_types::permissions::PermissionRequest;
use oh_types::stream_events::*;
use oh_types::tools::ToolExecutionContext;
use opentelemetry::KeyValue;
use tracing::{info_span, Instrument};

/// Callback type for permission prompts.
pub type PermissionPromptFn =
    Arc<dyn Fn(String, String) -> std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send>> + Send + Sync>;

/// Callback type for asking the user.
pub type AskUserPromptFn =
    Arc<dyn Fn(String) -> std::pin::Pin<Box<dyn std::future::Future<Output = String> + Send>> + Send + Sync>;

/// Context shared across a query run.
pub struct QueryContext {
    pub api_client: Arc<dyn StreamingApiClient>,
    pub tool_registry: Arc<ToolRegistry>,
    pub permission_checker: Arc<PermissionChecker>,
    pub cwd: PathBuf,
    pub model: String,
    pub system_prompt: String,
    pub max_tokens: u32,
    pub permission_prompt: Option<PermissionPromptFn>,
    pub ask_user_prompt: Option<AskUserPromptFn>,
    pub max_turns: u32,
    pub hook_executor: Option<Arc<dyn HookExecutorTrait>>,
    pub tool_metadata: std::collections::HashMap<String, serde_json::Value>,
}

impl QueryContext {
    /// Fire a hook if a hook executor is configured. No-op otherwise.
    async fn fire_hook(&self, event: HookEvent, payload: serde_json::Value) {
        if let Some(ref hook_executor) = self.hook_executor {
            hook_executor.execute(event, payload).await;
        }
    }
}

/// Run the conversation loop until the model stops requesting tools.
pub async fn run_query(
    context: &QueryContext,
    messages: &mut Vec<ConversationMessage>,
) -> Result<Vec<(StreamEvent, Option<UsageSnapshot>)>, EngineError> {
    let mut events = Vec::new();

    for turn in 0..context.max_turns {
        let span = info_span!("query_turn", turn.number = turn);

        let turn_events = async {
            context.fire_hook(HookEvent::QueryTurnStart, serde_json::json!({"turn": turn})).await;
            context.fire_hook(HookEvent::PreApiRequest, serde_json::json!({"model": context.model, "turn": turn})).await;

            // Stream API request
            let request = ApiMessageRequest {
                model: context.model.clone(),
                messages: messages.clone(),
                system_prompt: Some(context.system_prompt.clone()),
                max_tokens: context.max_tokens,
                tools: context.tool_registry.to_api_schema(),
            };

            let start = Instant::now();
            let mut stream = context
                .api_client
                .stream_message(request)
                .await
                .map_err(|e| EngineError::ApiError(e.to_string()))?;

            use futures::StreamExt;
            let mut final_message: Option<ConversationMessage> = None;
            let mut usage = UsageSnapshot::default();
            let mut turn_events = Vec::new();

            while let Some(event) = stream.next().await {
                match event {
                    Ok(ApiStreamEvent::TextDelta(delta)) => {
                        turn_events.push((
                            StreamEvent::AssistantTextDelta(AssistantTextDelta {
                                text: delta.text,
                            }),
                            None,
                        ));
                    }
                    Ok(ApiStreamEvent::MessageComplete(complete)) => {
                        final_message = Some(complete.message);
                        usage = complete.usage;
                    }
                    Err(e) => {
                        return Err(EngineError::ApiError(e.to_string()));
                    }
                }
            }

            context.fire_hook(HookEvent::PostApiResponse, serde_json::json!({
                "model": context.model,
                "input_tokens": usage.input_tokens,
                "output_tokens": usage.output_tokens,
            })).await;

            let final_message = final_message
                .ok_or_else(|| EngineError::NoResponse)?;

            context.fire_hook(HookEvent::PrePushMessage, serde_json::json!({"role": "assistant", "turn": turn})).await;
            messages.push(final_message.clone());
            context.fire_hook(HookEvent::PostPushMessage, serde_json::json!({"role": "assistant", "turn": turn})).await;

            turn_events.push((
                StreamEvent::AssistantTurnComplete(AssistantTurnComplete {
                    message: final_message.clone(),
                    usage: usage.clone(),
                }),
                Some(usage.clone()),
            ));

            // Check for tool calls
            let tool_calls = final_message.tool_uses();
            if tool_calls.is_empty() {
                context.fire_hook(HookEvent::QueryTurnEnd, serde_json::json!({"turn": turn, "has_tool_calls": false})).await;
                return Ok((turn_events, true)); // done
            }

            // Execute tool calls
            let tool_results = if tool_calls.len() == 1 {
                let tc = &tool_calls[0];
                turn_events.push((
                    StreamEvent::ToolExecutionStarted(ToolExecutionStarted {
                        tool_name: tc.name.clone(),
                        tool_input: tc.input.clone(),
                    }),
                    None,
                ));

                let result = execute_tool_call(context, &tc.name, &tc.id, &tc.input).await;

                turn_events.push((
                    StreamEvent::ToolExecutionCompleted(ToolExecutionCompleted {
                        tool_name: tc.name.clone(),
                        output: result.content.clone(),
                        is_error: result.is_error,
                    }),
                    None,
                ));

                vec![result]
            } else {
                // Emit start events
                for tc in &tool_calls {
                    turn_events.push((
                        StreamEvent::ToolExecutionStarted(ToolExecutionStarted {
                            tool_name: tc.name.clone(),
                            tool_input: tc.input.clone(),
                        }),
                        None,
                    ));
                }

                // Execute concurrently
                let futures: Vec<_> = tool_calls
                    .iter()
                    .map(|tc| execute_tool_call(context, &tc.name, &tc.id, &tc.input))
                    .collect();
                let results = join_all(futures).await;

                // Emit completion events
                for (tc, result) in tool_calls.iter().zip(results.iter()) {
                    turn_events.push((
                        StreamEvent::ToolExecutionCompleted(ToolExecutionCompleted {
                            tool_name: tc.name.clone(),
                            output: result.content.clone(),
                            is_error: result.is_error,
                        }),
                        None,
                    ));
                }

                results
            };

            // Add tool results as user message
            let tool_result_content: Vec<ContentBlock> = tool_results
                .into_iter()
                .map(|r| ContentBlock::ToolResult(r))
                .collect();

            context.fire_hook(HookEvent::PrePushMessage, serde_json::json!({"role": "user", "content_type": "tool_results", "turn": turn})).await;
            messages.push(ConversationMessage {
                role: Role::User,
                content: tool_result_content,
            });
            context.fire_hook(HookEvent::PostPushMessage, serde_json::json!({"role": "user", "content_type": "tool_results", "turn": turn})).await;
            context.fire_hook(HookEvent::QueryTurnEnd, serde_json::json!({"turn": turn, "has_tool_calls": true})).await;

            Ok((turn_events, false)) // not done, continue
        }
        .instrument(span)
        .await;

        let (turn_events, done) = turn_events?;
        events.extend(turn_events);

        if done {
            return Ok(events);
        }
    }

    Err(EngineError::MaxTurnsExceeded(context.max_turns))
}

/// Execute a single tool call with permission checks and hooks.
async fn execute_tool_call(
    context: &QueryContext,
    tool_name: &str,
    tool_use_id: &str,
    tool_input: &std::collections::HashMap<String, serde_json::Value>,
) -> ToolResultBlock {
    let span = info_span!("tool_call", tool.name = %tool_name);

    let start = Instant::now();

    let result = async {
        let input_value = serde_json::to_value(tool_input).unwrap_or_default();

        // Pre-tool-use hook
        if let Some(ref hook_executor) = context.hook_executor {
            let pre_hooks = hook_executor
                .execute(
                    HookEvent::PreToolUse,
                    serde_json::json!({
                        "tool_name": tool_name,
                        "tool_input": input_value,
                        "event": "pre_tool_use",
                    }),
                )
                .await;
            if pre_hooks.blocked() {
                return ToolResultBlock::new(
                    tool_use_id,
                    pre_hooks.reason().to_string(),
                    true,
                );
            }
        }

        // Look up tool
        let tool = match context.tool_registry.get(tool_name) {
            Some(t) => t,
            None => {
                return ToolResultBlock::new(
                    tool_use_id,
                    format!("Unknown tool: {tool_name}"),
                    true,
                )
            }
        };

        // Permission check
        let file_path = tool_input
            .get("file_path")
            .and_then(|v| v.as_str())
            .map(String::from);
        let command = tool_input
            .get("command")
            .and_then(|v| v.as_str())
            .map(String::from);

        context.fire_hook(HookEvent::PrePermissionCheck, serde_json::json!({"tool_name": tool_name})).await;

        let decision = context.permission_checker.evaluate(&PermissionRequest {
            tool_name,
            is_read_only: tool.is_read_only(&input_value),
            file_path: file_path.as_deref(),
            command: command.as_deref(),
        });

        context.fire_hook(HookEvent::PostPermissionCheck, serde_json::json!({
            "tool_name": tool_name,
            "allowed": decision.allowed,
            "requires_confirmation": decision.requires_confirmation,
        })).await;

        if !decision.allowed {
            let denied = if decision.requires_confirmation {
                if let Some(ref prompt_fn) = context.permission_prompt {
                    let confirmed = prompt_fn(
                        tool_name.to_string(),
                        decision.reason.clone(),
                    )
                    .await;
                    if confirmed { false } else {
                        context.fire_hook(HookEvent::PermissionDenied, serde_json::json!({"tool_name": tool_name, "reason": "user_rejected"})).await;
                        true
                    }
                } else {
                    context.fire_hook(HookEvent::PermissionDenied, serde_json::json!({"tool_name": tool_name, "reason": "no_prompt_fn"})).await;
                    true
                }
            } else {
                context.fire_hook(HookEvent::PermissionDenied, serde_json::json!({"tool_name": tool_name, "reason": decision.reason})).await;
                return ToolResultBlock::new(tool_use_id, decision.reason, true);
            };
            if denied {
                return ToolResultBlock::new(
                    tool_use_id,
                    format!("Permission denied for {tool_name}"),
                    true,
                );
            }
        }

        // Execute tool
        let tool_ctx = ToolExecutionContext {
            cwd: context.cwd.clone(),
            metadata: context.tool_metadata.clone(),
        };
        let result = tool.execute(input_value, &tool_ctx).await;

        // Apply hook mutations if the tool returned a hook_action
        if let Some(hook_action) = result.metadata.get("hook_action") {
            apply_hook_action(context, hook_action).await;
        }

        let tool_result = ToolResultBlock::new(tool_use_id, &result.output, result.is_error);

        if result.is_error {
            context.fire_hook(HookEvent::ToolError_, serde_json::json!({
                "tool_name": tool_name,
                "error": result.output,
            })).await;
        }

        context.fire_hook(HookEvent::PostToolUse, serde_json::json!({
            "tool_name": tool_name,
            "tool_input": serde_json::to_value(tool_input).unwrap_or_default(),
            "tool_output": tool_result.content,
            "tool_is_error": tool_result.is_error,
            "event": "post_tool_use",
        })).await;

        tool_result
    }
    .instrument(span)
    .await;

    let elapsed = start.elapsed().as_secs_f64();
    oh_telemetry::TOOL_CALL_DURATION.record(
        elapsed,
        &[
            KeyValue::new("tool_name", tool_name.to_string()),
            KeyValue::new("success", (!result.is_error).to_string()),
        ],
    );

    if result.is_error {
        oh_telemetry::TOOL_ERROR_COUNT.add(
            1,
            &[KeyValue::new("tool_name", tool_name.to_string())],
        );
    }

    result
}

/// Apply a hook mutation from a tool result (e.g., HookManage tool).
async fn apply_hook_action(context: &QueryContext, action: &serde_json::Value) {
    let action_type = action.get("action").and_then(|v| v.as_str()).unwrap_or("");

    match action_type {
        "add" => {
            if let (Some(event_str), Some(hook_value)) = (
                action.get("event").and_then(|v| v.as_str()),
                action.get("hook"),
            ) {
                if let (Ok(event), Ok(hook)) = (
                    serde_json::from_value::<HookEvent>(serde_json::Value::String(event_str.to_string())),
                    serde_json::from_value::<oh_types::hooks::HookDefinition>(hook_value.clone()),
                ) {
                    if let Some(ref executor) = context.hook_executor {
                        // Access the registry via the executor's public handle
                        // For now, fire a hook to signal the addition
                        executor.execute(
                            HookEvent::PluginLoaded,
                            serde_json::json!({
                                "source": "hook_manage",
                                "event": event_str,
                                "hook_type": hook.hook_type(),
                            }),
                        ).await;
                    }
                    tracing::info!(event = %event_str, hook_type = hook.hook_type(), "Hook registered via HookManage tool");
                }
            }
        }
        "clear_event" => {
            if let Some(event_str) = action.get("event").and_then(|v| v.as_str()) {
                tracing::info!(event = %event_str, "Hooks cleared for event via HookManage tool");
            }
        }
        "clear_all" => {
            tracing::info!("All hooks cleared via HookManage tool");
        }
        _ => {}
    }
}

/// Engine errors.
#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("API error: {0}")]
    ApiError(String),
    #[error("model stream finished without a final message")]
    NoResponse,
    #[error("exceeded maximum turn limit ({0})")]
    MaxTurnsExceeded(u32),
}

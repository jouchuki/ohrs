//! Core tool-aware query loop.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use futures::future::join_all;
use oh_api::StreamingApiClient;
use oh_hooks::loader::HookRegistry;
use oh_hooks::{AggregatedHookResult, HookEvent, HookExecutorTrait};
use oh_permissions::{canonicalize_path, PermissionChecker};
use oh_services::compact::{CompactRequest, CompactSummarizer, Compactor};
use oh_tools::ToolRegistry;
use oh_types::api::*;
use oh_types::messages::*;
use oh_types::permissions::PermissionRequest;
use oh_types::stream_events::*;
use oh_types::subagent::{AgentId, BackgroundTasks, SubagentSpawner};
use oh_types::tools::ToolExecutionContext;
use opentelemetry::KeyValue;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;
use tracing::{info_span, Instrument};

/// Engine-level backstop cap on a single tool result's output (TOOL-9). Tools
/// SHOULD cap their own output, but the engine enforces a uniform ceiling so a
/// misbehaving tool can't balloon memory or the token budget. Chosen generously
/// (~256 KiB) so legitimate large reads survive while pathological output
/// (`cat /dev/urandom`, `yes`) is bounded.
const MAX_TOOL_OUTPUT_BYTES: usize = 256 * 1024;

/// Marker appended when a tool result is truncated by the engine backstop.
const TOOL_OUTPUT_TRUNCATION_MARKER: &str = "\n…[output truncated by engine backstop]";

/// Number of recent messages the compactor preserves verbatim when it triggers
/// (ENG-1). Everything older is replaced by a single summary message.
const COMPACT_KEEP_LAST_N: usize = 6;

/// Truncate `output` to the engine backstop cap (TOOL-9), snapping to a UTF-8
/// char boundary so we never split a multi-byte sequence.
fn enforce_output_cap(mut output: String) -> String {
    if output.len() <= MAX_TOOL_OUTPUT_BYTES {
        return output;
    }
    let mut end = MAX_TOOL_OUTPUT_BYTES;
    while end > 0 && !output.is_char_boundary(end) {
        end -= 1;
    }
    output.truncate(end);
    output.push_str(TOOL_OUTPUT_TRUNCATION_MARKER);
    output
}

/// Adapts the engine's [`StreamingApiClient`] into a [`CompactSummarizer`] so the
/// compactor can summarize history with the same provider the run uses (ENG-1).
struct ApiClientSummarizer {
    api_client: Arc<dyn StreamingApiClient>,
    model: String,
}

#[async_trait]
impl CompactSummarizer for ApiClientSummarizer {
    async fn summarize(
        &self,
        system: &str,
        history: &str,
    ) -> Result<String, oh_services::compact::CompactError> {
        let request = ApiMessageRequest {
            model: self.model.clone(),
            messages: vec![ConversationMessage::from_user_text(history)],
            system_prompt: Some(system.to_string()),
            max_tokens: 2048,
            tools: Vec::new(),
        };
        let mut stream = self
            .api_client
            .stream_message(request)
            .await
            .map_err(|e| oh_services::compact::CompactError::Summarizer(e.to_string()))?;

        use futures::StreamExt;
        let mut text = String::new();
        while let Some(event) = stream.next().await {
            if let Ok(ApiStreamEvent::MessageComplete(complete)) = event {
                text = complete.message.text();
            }
        }
        if text.is_empty() {
            return Err(oh_services::compact::CompactError::Summarizer(
                "summarizer returned no text".into(),
            ));
        }
        Ok(text)
    }
}

/// Callback type for permission prompts.
pub type PermissionPromptFn = Arc<
    dyn Fn(String, String) -> std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send>>
        + Send
        + Sync,
>;

/// Callback type for asking the user.
pub type AskUserPromptFn = Arc<
    dyn Fn(String) -> std::pin::Pin<Box<dyn std::future::Future<Output = String> + Send>>
        + Send
        + Sync,
>;

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
    /// Identity of this agent run. The top-level/main agent uses
    /// `AgentId("main")`; subagents get their own id.
    pub agent_id: AgentId,
    /// The id of the agent that spawned this one, if any.
    pub parent_id: Option<AgentId>,
    /// The persistent session id this run records into (when recording is on).
    pub session_id: Option<String>,
    /// Handle for spawning subagents, threaded into the per-tool
    /// [`ToolExecutionContext`] so the `Agent` tool can reach the orchestrator.
    /// `None` outside the harness (e.g. unit tests).
    pub subagents: Option<Arc<dyn SubagentSpawner>>,
    /// Handle for the background-task control plane, threaded into the per-tool
    /// [`ToolExecutionContext`] for the `Task*` tools.
    pub tasks: Option<Arc<dyn BackgroundTasks>>,
    /// Cooperative cancellation token (ENG-2 / contract C6). When cancelled, the
    /// stream-consume loop and the tool-dispatch step abort gracefully. The
    /// default token is never cancelled, so existing callers are unaffected.
    pub cancel: CancellationToken,
    /// Live hook registry (HOOK-1 / contract C7). When present,
    /// [`apply_hook_action`] mutates it under the write lock so the `HookManage`
    /// tool's add/clear actually take effect at runtime. `None` disables runtime
    /// mutation (the action is logged only).
    pub hook_registry: Option<Arc<RwLock<HookRegistry>>>,
    /// Proactive context compactor (ENG-1). When present, the loop checks
    /// [`Compactor::should_compact_full`] before each turn and, if over the
    /// threshold, replaces the history with the compacted form and fires
    /// [`HookEvent::ContextCompacted`]. `None` disables compaction.
    pub compactor: Option<Arc<Compactor>>,
}

impl QueryContext {
    /// Fire a hook if a hook executor is configured, returning the aggregate
    /// result so callers can honor `.blocked()` (HOOK-2 / contract C7). Returns
    /// an empty (non-blocking) aggregate when no executor is configured.
    async fn fire_hook(
        &self,
        event: HookEvent,
        payload: serde_json::Value,
    ) -> AggregatedHookResult {
        if let Some(ref hook_executor) = self.hook_executor {
            hook_executor.execute(event, payload).await
        } else {
            AggregatedHookResult::default()
        }
    }
}

/// Run the conversation loop until the model stops requesting tools.
pub async fn run_query(
    context: &QueryContext,
    messages: &mut Vec<ConversationMessage>,
) -> Result<Vec<(StreamEvent, Option<UsageSnapshot>)>, EngineError> {
    let mut events = Vec::new();

    // Record the user message that opened this run (the latest user message in
    // the conversation). This covers both the print-mode prompt and a
    // subagent's seeded prompt, and feeds the trajectory recorder.
    if let Some(user_text) = messages
        .iter()
        .rev()
        .find(|m| m.role == Role::User)
        .map(|m| m.text())
    {
        context
            .fire_hook(
                HookEvent::PostUserMessage,
                serde_json::json!({ "text": user_text }),
            )
            .await;
    }

    for turn in 0..context.max_turns {
        // ENG-2: bail out before starting a turn if cancellation was requested.
        if context.cancel.is_cancelled() {
            context
                .fire_hook(
                    HookEvent::QueryTurnEnd,
                    serde_json::json!({"turn": turn, "cancelled": true}),
                )
                .await;
            return Err(EngineError::Cancelled);
        }

        // ENG-1: proactive context compaction. Before sending the next turn,
        // check whether the full request (history + system prompt + tool schemas)
        // would exceed the threshold; if so, replace the history with the
        // compacted form and fire `ContextCompacted`.
        maybe_compact(context, messages).await;

        let span = info_span!("query_turn", turn.number = turn);

        let turn_events = async {
            context.fire_hook(HookEvent::QueryTurnStart, serde_json::json!({"turn": turn})).await;

            // HOOK-2: a blocking PreApiRequest hook aborts the turn before any
            // provider call (cost/latency/tokens) is incurred.
            let pre_api = context.fire_hook(HookEvent::PreApiRequest, serde_json::json!({"model": context.model, "turn": turn})).await;
            if pre_api.blocked() {
                context.fire_hook(HookEvent::QueryTurnEnd, serde_json::json!({"turn": turn, "blocked": "pre_api_request"})).await;
                return Err(EngineError::HookBlocked(pre_api.reason().to_string()));
            }

            // Stream API request
            let request = ApiMessageRequest {
                model: context.model.clone(),
                messages: messages.clone(),
                system_prompt: Some(context.system_prompt.clone()),
                max_tokens: context.max_tokens,
                tools: context.tool_registry.to_api_schema(),
            };

            let _start = Instant::now();
            let mut stream = context
                .api_client
                .stream_message(request)
                .await
                .map_err(|e| EngineError::ApiError(e.to_string()))?;

            use futures::StreamExt;
            let mut final_message: Option<ConversationMessage> = None;
            let mut usage = UsageSnapshot::default();
            let mut turn_events = Vec::new();

            // ENG-2: consume the stream under cancellation. If the token fires
            // mid-stream we drop the stream (closing the connection) and abort.
            loop {
                tokio::select! {
                    biased;
                    _ = context.cancel.cancelled() => {
                        tracing::info!(turn, "query cancelled while consuming stream");
                        return Err(EngineError::Cancelled);
                    }
                    next = stream.next() => {
                        match next {
                            None => break,
                            Some(Ok(ApiStreamEvent::TextDelta(delta))) => {
                                turn_events.push((
                                    StreamEvent::AssistantTextDelta(AssistantTextDelta {
                                        text: delta.text,
                                    }),
                                    None,
                                ));
                            }
                            Some(Ok(ApiStreamEvent::MessageComplete(complete))) => {
                                final_message = Some(complete.message);
                                usage = complete.usage;
                            }
                            Some(Ok(ApiStreamEvent::ToolUseDelta(_))) => {
                                // Tool-use delta events are not yet processed; skip.
                            }
                            Some(Err(e)) => {
                                return Err(EngineError::ApiError(e.to_string()));
                            }
                        }
                    }
                }
            }

            let final_message = final_message
                .ok_or(EngineError::NoResponse)?;

            // Enrich PostApiResponse with the assistant message's text and any
            // tool_use blocks so the trajectory recorder captures real content.
            let assistant_tool_uses: Vec<serde_json::Value> = final_message
                .tool_uses()
                .iter()
                .map(|tu| {
                    serde_json::json!({
                        "id": tu.id,
                        "name": tu.name,
                        "input": tu.input,
                    })
                })
                .collect();
            context.fire_hook(HookEvent::PostApiResponse, serde_json::json!({
                "model": context.model,
                "input_tokens": usage.input_tokens,
                "output_tokens": usage.output_tokens,
                "text": final_message.text(),
                "tool_uses": assistant_tool_uses,
                "content": final_message.content.iter().map(oh_types::messages::serialize_content_block).collect::<Vec<_>>(),
            })).await;

            // HOOK-2: a blocking PrePushMessage hook prevents appending the
            // assistant message and aborts the turn (e.g. an output-policy hook).
            let pre_push = context.fire_hook(HookEvent::PrePushMessage, serde_json::json!({"role": "assistant", "turn": turn})).await;
            if pre_push.blocked() {
                context.fire_hook(HookEvent::QueryTurnEnd, serde_json::json!({"turn": turn, "blocked": "pre_push_assistant"})).await;
                return Err(EngineError::HookBlocked(pre_push.reason().to_string()));
            }
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

                let result = tokio::select! {
                    biased;
                    _ = context.cancel.cancelled() => {
                        tracing::info!(turn, "query cancelled during tool dispatch");
                        return Err(EngineError::Cancelled);
                    }
                    result = execute_tool_call(context, &tc.name, &tc.id, &tc.input) => result,
                };

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

                // Execute concurrently, under cancellation (ENG-2): if the token
                // fires while tools are running we drop the in-flight dispatch
                // and abort the turn.
                let futures: Vec<_> = tool_calls
                    .iter()
                    .map(|tc| execute_tool_call(context, &tc.name, &tc.id, &tc.input))
                    .collect();
                let results = tokio::select! {
                    biased;
                    _ = context.cancel.cancelled() => {
                        tracing::info!(turn, "query cancelled during tool dispatch");
                        return Err(EngineError::Cancelled);
                    }
                    results = join_all(futures) => results,
                };

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
                .map(ContentBlock::ToolResult)
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

/// ENG-1: proactive context compaction. If a compactor is configured and the
/// estimated full-request size exceeds its threshold, summarize the older
/// history, replace `messages` with the compacted form, and fire
/// [`HookEvent::ContextCompacted`]. A compaction failure (too few messages, or a
/// summarizer error) is logged and left non-fatal — the turn proceeds with the
/// uncompacted history and the provider's own limit becomes the backstop.
async fn maybe_compact(context: &QueryContext, messages: &mut Vec<ConversationMessage>) {
    let Some(ref compactor) = context.compactor else {
        return;
    };

    // Approximate the serialized tool-schema size so the estimate reflects the
    // whole request, not just the message history.
    let tool_schemas_chars: usize = context
        .tool_registry
        .to_api_schema()
        .iter()
        .map(|s| s.to_string().len())
        .sum();

    if !compactor.should_compact_full(messages, &context.system_prompt, tool_schemas_chars) {
        return;
    }

    let summarizer = ApiClientSummarizer {
        api_client: context.api_client.clone(),
        model: context.model.clone(),
    };

    let req = CompactRequest {
        messages,
        keep_last_n: COMPACT_KEEP_LAST_N,
        system_prompt: &context.system_prompt,
    };

    match compactor.compact(req, &summarizer).await {
        Ok(result) => {
            tracing::info!(
                before = result.estimated_tokens_before,
                after = result.estimated_tokens_after,
                "context compacted"
            );
            context
                .fire_hook(
                    HookEvent::ContextCompacted,
                    serde_json::json!({
                        "estimated_tokens_before": result.estimated_tokens_before,
                        "estimated_tokens_after": result.estimated_tokens_after,
                        "kept_messages": result.kept_messages.len(),
                    }),
                )
                .await;
            *messages = result.kept_messages;
        }
        Err(e) => {
            tracing::warn!(error = %e, "context compaction skipped");
        }
    }
}

/// Run a subagent: a nested [`run_query`] carrying its own [`QueryContext`]
/// (same `hook_executor` + `permission_checker` as the parent), seeded with
/// `prompt`. Fires [`HookEvent::SubagentStart`] before the body and
/// [`HookEvent::SubagentStop`] after it, then returns the final assistant text.
///
/// Phase 0: this delegates to `run_query` with a single seeded user message and
/// returns the concatenated text of the last assistant turn. The Start/Stop
/// hooks bracket the body so blocks/webhooks/recording ride the existing
/// pipeline.
pub async fn run_subagent(ctx: QueryContext, prompt: String) -> Result<String, EngineError> {
    ctx.fire_hook(
        HookEvent::SubagentStart,
        serde_json::json!({
            "agent_id": ctx.agent_id.as_str(),
            "parent_id": ctx.parent_id.as_ref().map(|p| p.as_str()),
            "subagent_type": ctx.tool_metadata.get("subagent_type"),
            "session_id": ctx.session_id,
            "prompt": prompt,
        }),
    )
    .await;

    let mut messages = vec![ConversationMessage::from_user_text(&prompt)];
    let result = run_query(&ctx, &mut messages).await;

    // Extract the text of the final assistant message produced by the run.
    let final_text = messages
        .iter()
        .rev()
        .find(|m| m.role == Role::Assistant)
        .map(|m| m.text())
        .unwrap_or_default();

    // Turn count = number of assistant messages the run produced.
    let turns = messages
        .iter()
        .filter(|m| m.role == Role::Assistant)
        .count();

    ctx.fire_hook(
        HookEvent::SubagentStop,
        serde_json::json!({
            "agent_id": ctx.agent_id.as_str(),
            "parent_id": ctx.parent_id.as_ref().map(|p| p.as_str()),
            "subagent_type": ctx.tool_metadata.get("subagent_type"),
            "session_id": ctx.session_id,
            "ok": result.is_ok(),
            "turns": turns,
            "result": final_text,
        }),
    )
    .await;

    result.map(|_| final_text)
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

        // Permission check (TOOL-1/TOOL-3/TOOL-4, contract C3/C4).
        //
        // Enumerate EVERY filesystem path the tool would touch via the trait
        // method `path_args` (not just the conventional `file_path` key, which
        // tools like NotebookEdit don't use), and check each — canonicalized
        // against the tool cwd and confined to `allowed_roots` — through
        // `evaluate_with_base`. The first denial wins. Tools with no path args
        // still get one mode/command evaluation.
        let command = tool_input
            .get("command")
            .and_then(|v| v.as_str())
            .map(String::from);
        let is_read_only = tool.is_read_only(&input_value);
        let path_args = tool.path_args(&input_value);
        let allowed_roots = vec![context.cwd.clone()];

        context.fire_hook(HookEvent::PrePermissionCheck, serde_json::json!({"tool_name": tool_name, "paths": path_args})).await;

        let decision = if path_args.is_empty() {
            // No path-bearing args: a single mode/command evaluation.
            context.permission_checker.evaluate_with_base(
                &PermissionRequest {
                    tool_name,
                    is_read_only,
                    file_path: None,
                    command: command.as_deref(),
                },
                &context.cwd,
                &allowed_roots,
            )
        } else {
            // Check each path; deny on the first that fails. The command is
            // carried on the first request so command deny rules still apply.
            let mut first_allow: Option<oh_types::permissions::PermissionDecision> = None;
            let mut denied: Option<oh_types::permissions::PermissionDecision> = None;
            for (i, raw) in path_args.iter().enumerate() {
                // Canonicalize for the trace/log; the checker re-canonicalizes
                // internally against the same base.
                let resolved = canonicalize_path(raw, &context.cwd);
                tracing::debug!(tool = tool_name, path = %raw, resolved = %resolved.display(), "permission path check");
                let d = context.permission_checker.evaluate_with_base(
                    &PermissionRequest {
                        tool_name,
                        is_read_only,
                        file_path: Some(raw.as_str()),
                        command: if i == 0 { command.as_deref() } else { None },
                    },
                    &context.cwd,
                    &allowed_roots,
                );
                if !d.allowed {
                    denied = Some(d);
                    break;
                }
                if first_allow.is_none() {
                    first_allow = Some(d);
                }
            }
            denied
                .or(first_allow)
                .unwrap_or_else(|| oh_types::permissions::PermissionDecision::allow("no paths"))
        };

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

        // Execute tool. The per-tool context carries the cwd as the single
        // allowed root (contract C4) so file tools confine themselves to the job
        // working directory in addition to the engine-side gate above.
        let tool_ctx = ToolExecutionContext {
            cwd: context.cwd.clone(),
            metadata: context.tool_metadata.clone(),
            subagents: context.subagents.clone(),
            tasks: context.tasks.clone(),
            allowed_roots: allowed_roots.clone(),
        };
        let result = tool.execute(input_value, &tool_ctx).await;

        // Apply hook mutations if the tool returned a hook_action
        if let Some(hook_action) = result.metadata.get("hook_action") {
            apply_hook_action(context, hook_action).await;
        }

        // TOOL-9: engine-level backstop cap on tool output, regardless of
        // whether the tool capped its own output.
        let is_error = result.is_error;
        let capped_output = enforce_output_cap(result.output);
        let tool_result = ToolResultBlock::new(tool_use_id, &capped_output, is_error);

        if is_error {
            context.fire_hook(HookEvent::ToolError_, serde_json::json!({
                "tool_name": tool_name,
                "error": capped_output,
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
        oh_telemetry::TOOL_ERROR_COUNT.add(1, &[KeyValue::new("tool_name", tool_name.to_string())]);
    }

    result
}

/// Apply a hook mutation from a tool result (e.g., HookManage tool).
///
/// HOOK-1 (contract C7): `add`/`clear_event`/`clear_all` actually mutate the
/// live [`HookRegistry`] under its write lock when a `hook_registry` handle is
/// threaded into the [`QueryContext`]. Without a handle the action is logged
/// only (no-op), preserving back-compat for contexts that don't expose the
/// registry. The `clear` direction is the dangerous one — a `clear` that reports
/// success while a blocking safety hook stays live would be a false guarantee —
/// so it now genuinely removes hooks.
async fn apply_hook_action(context: &QueryContext, action: &serde_json::Value) {
    let action_type = action.get("action").and_then(|v| v.as_str()).unwrap_or("");

    match action_type {
        "add" => {
            if let (Some(event_str), Some(hook_value)) = (
                action.get("event").and_then(|v| v.as_str()),
                action.get("hook"),
            ) {
                if let (Ok(event), Ok(hook)) = (
                    serde_json::from_value::<HookEvent>(serde_json::Value::String(
                        event_str.to_string(),
                    )),
                    serde_json::from_value::<oh_types::hooks::HookDefinition>(hook_value.clone()),
                ) {
                    let hook_type = hook.hook_type().to_string();
                    if let Some(ref registry) = context.hook_registry {
                        registry.write().await.register(event, hook);
                        tracing::info!(event = %event_str, hook_type = %hook_type, "Hook registered via HookManage tool (registry mutated)");
                        if let Some(ref executor) = context.hook_executor {
                            executor
                                .execute(
                                    HookEvent::PluginLoaded,
                                    serde_json::json!({
                                        "source": "hook_manage",
                                        "event": event_str,
                                        "hook_type": hook_type,
                                    }),
                                )
                                .await;
                        }
                    } else {
                        tracing::warn!(event = %event_str, hook_type, "HookManage add ignored: no registry handle in QueryContext");
                    }
                }
            }
        }
        "clear_event" => {
            if let Some(event_str) = action.get("event").and_then(|v| v.as_str()) {
                if let Ok(event) = serde_json::from_value::<HookEvent>(serde_json::Value::String(
                    event_str.to_string(),
                )) {
                    if let Some(ref registry) = context.hook_registry {
                        registry.write().await.clear_event(&event);
                        tracing::info!(event = %event_str, "Hooks cleared for event via HookManage tool (registry mutated)");
                    } else {
                        tracing::warn!(event = %event_str, "HookManage clear_event ignored: no registry handle in QueryContext");
                    }
                }
            }
        }
        "clear_all" => {
            if let Some(ref registry) = context.hook_registry {
                registry.write().await.clear_all();
                tracing::info!("All hooks cleared via HookManage tool (registry mutated)");
            } else {
                tracing::warn!("HookManage clear_all ignored: no registry handle in QueryContext");
            }
        }
        "fire_event" => {
            // A tool requests that a lifecycle hook be fired with a payload —
            // used by the inter-agent messaging tool to raise
            // `SubagentMessage` through the same hook executor everything else
            // uses (the tool itself has no executor handle).
            if let Some(event_str) = action.get("event").and_then(|v| v.as_str()) {
                if let Ok(event) = serde_json::from_value::<HookEvent>(serde_json::Value::String(
                    event_str.to_string(),
                )) {
                    let payload = action
                        .get("payload")
                        .cloned()
                        .unwrap_or(serde_json::json!({}));
                    if let Some(ref executor) = context.hook_executor {
                        executor.execute(event, payload).await;
                    }
                }
            }
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
    /// The run was cancelled cooperatively via the [`QueryContext`] token
    /// (ENG-2 / contract C6).
    #[error("query cancelled")]
    Cancelled,
    /// A blocking `Pre*` hook aborted the turn (HOOK-2 / contract C7).
    #[error("blocked by hook: {0}")]
    HookBlocked(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use futures::Stream;
    use oh_config::PermissionSettings;
    use oh_hooks::AggregatedHookResult;
    use oh_permissions::PermissionChecker;
    use std::pin::Pin;
    use std::sync::Mutex as StdMutex;

    /// Fake client that yields a single assistant text message.
    struct FakeApiClient {
        text: String,
    }

    #[async_trait]
    impl StreamingApiClient for FakeApiClient {
        async fn stream_message(
            &self,
            _request: ApiMessageRequest,
        ) -> Result<
            Pin<Box<dyn Stream<Item = Result<ApiStreamEvent, ApiError>> + Send + '_>>,
            ApiError,
        > {
            let msg = ConversationMessage {
                role: Role::Assistant,
                content: vec![ContentBlock::Text(TextBlock::new(self.text.clone()))],
            };
            let events = vec![Ok(ApiStreamEvent::MessageComplete(
                ApiMessageCompleteEvent {
                    message: msg,
                    usage: UsageSnapshot::default(),
                    stop_reason: Some("end_turn".into()),
                },
            ))];
            Ok(Box::pin(futures::stream::iter(events)))
        }
    }

    /// Hook executor that records every event it sees.
    struct RecordingHookExecutor {
        events: Arc<StdMutex<Vec<HookEvent>>>,
    }

    #[async_trait]
    impl HookExecutorTrait for RecordingHookExecutor {
        async fn execute(
            &self,
            event: HookEvent,
            _payload: serde_json::Value,
        ) -> AggregatedHookResult {
            self.events.lock().unwrap().push(event);
            AggregatedHookResult::default()
        }
    }

    fn make_ctx(text: &str, events: Arc<StdMutex<Vec<HookEvent>>>) -> QueryContext {
        QueryContext {
            api_client: Arc::new(FakeApiClient { text: text.into() }),
            tool_registry: Arc::new(ToolRegistry::new()),
            permission_checker: Arc::new(PermissionChecker::new(PermissionSettings::default())),
            cwd: PathBuf::from("/tmp"),
            model: "test-model".into(),
            system_prompt: "test".into(),
            max_tokens: 1024,
            permission_prompt: None,
            ask_user_prompt: None,
            max_turns: 5,
            hook_executor: Some(Arc::new(RecordingHookExecutor { events })),
            tool_metadata: std::collections::HashMap::new(),
            agent_id: AgentId::new("sub-1"),
            parent_id: Some(AgentId::new("main")),
            session_id: Some("sess-1".into()),
            subagents: None,
            tasks: None,
            cancel: CancellationToken::new(),
            hook_registry: None,
            compactor: None,
        }
    }

    #[tokio::test]
    async fn test_run_subagent_returns_assistant_text() {
        let events = Arc::new(StdMutex::new(Vec::new()));
        let ctx = make_ctx("subagent done", Arc::clone(&events));
        let out = run_subagent(ctx, "do the thing".into()).await.unwrap();
        assert_eq!(out, "subagent done");
    }

    #[tokio::test]
    async fn test_run_subagent_fires_start_and_stop_hooks() {
        let events = Arc::new(StdMutex::new(Vec::new()));
        let ctx = make_ctx("ok", Arc::clone(&events));
        let _ = run_subagent(ctx, "go".into()).await.unwrap();

        let seen = events.lock().unwrap();
        assert!(
            seen.contains(&HookEvent::SubagentStart),
            "SubagentStart not fired: {seen:?}"
        );
        assert!(
            seen.contains(&HookEvent::SubagentStop),
            "SubagentStop not fired: {seen:?}"
        );
        // Start must precede Stop.
        let start = seen.iter().position(|e| *e == HookEvent::SubagentStart);
        let stop = seen.iter().position(|e| *e == HookEvent::SubagentStop);
        assert!(start < stop, "Start should precede Stop: {seen:?}");
    }

    // ── Control-plane proof tests ────────────────────────────────────────────
    //
    // A subagent is just a nested `run_query` carrying the SAME hook executor as
    // its parent, so it inherits blocks and webhooks for free. These tests prove
    // both ride the existing pipeline.

    use oh_hooks::HookResult;
    use oh_types::tools::{ToolExecutionContext, ToolResult};

    /// Fake client that issues exactly one `bash` tool call on the first turn,
    /// then returns a final text message on the second turn. This lets us drive
    /// the `PreToolUse` gate from within a subagent run.
    struct ToolThenTextClient {
        turn: std::sync::atomic::AtomicUsize,
        final_text: String,
    }

    #[async_trait]
    impl StreamingApiClient for ToolThenTextClient {
        async fn stream_message(
            &self,
            _request: ApiMessageRequest,
        ) -> Result<
            Pin<Box<dyn Stream<Item = Result<ApiStreamEvent, ApiError>> + Send + '_>>,
            ApiError,
        > {
            use std::sync::atomic::Ordering;
            let n = self.turn.fetch_add(1, Ordering::SeqCst);
            let msg = if n == 0 {
                ConversationMessage {
                    role: Role::Assistant,
                    content: vec![ContentBlock::ToolUse(ToolUseBlock::new(
                        "bash",
                        std::collections::HashMap::new(),
                    ))],
                }
            } else {
                ConversationMessage {
                    role: Role::Assistant,
                    content: vec![ContentBlock::Text(TextBlock::new(self.final_text.clone()))],
                }
            };
            let events = vec![Ok(ApiStreamEvent::MessageComplete(
                ApiMessageCompleteEvent {
                    message: msg,
                    usage: UsageSnapshot::default(),
                    stop_reason: Some("end_turn".into()),
                },
            ))];
            Ok(Box::pin(futures::stream::iter(events)))
        }
    }

    /// A trivial tool that records when it executes, so a block can be detected
    /// by the *absence* of execution.
    struct SpyTool {
        executed: Arc<std::sync::atomic::AtomicBool>,
    }

    #[async_trait]
    impl oh_tools::Tool for SpyTool {
        fn name(&self) -> &str {
            "bash"
        }
        fn description(&self) -> &str {
            "spy"
        }
        fn input_schema(&self) -> serde_json::Value {
            serde_json::json!({"type": "object", "properties": {}})
        }
        fn is_read_only(&self, _a: &serde_json::Value) -> bool {
            true
        }
        async fn execute(&self, _a: serde_json::Value, _c: &ToolExecutionContext) -> ToolResult {
            self.executed
                .store(true, std::sync::atomic::Ordering::SeqCst);
            ToolResult::success("ran")
        }
    }

    /// Hook executor that BLOCKS a configured event (mirrors `block_on_failure`)
    /// and records every event it sees.
    struct BlockingHookExecutor {
        block_on: HookEvent,
        events: Arc<StdMutex<Vec<HookEvent>>>,
    }

    #[async_trait]
    impl HookExecutorTrait for BlockingHookExecutor {
        async fn execute(
            &self,
            event: HookEvent,
            _payload: serde_json::Value,
        ) -> AggregatedHookResult {
            self.events.lock().unwrap().push(event);
            if event == self.block_on {
                AggregatedHookResult {
                    results: vec![HookResult {
                        success: false,
                        blocked: true,
                        reason: "blocked by policy".into(),
                        ..Default::default()
                    }],
                }
            } else {
                AggregatedHookResult::default()
            }
        }
    }

    fn make_ctx_with_spy(
        executed: Arc<std::sync::atomic::AtomicBool>,
        hook_executor: Arc<dyn HookExecutorTrait>,
    ) -> QueryContext {
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(SpyTool { executed }));
        QueryContext {
            api_client: Arc::new(ToolThenTextClient {
                turn: std::sync::atomic::AtomicUsize::new(0),
                final_text: "done".into(),
            }),
            tool_registry: Arc::new(registry),
            permission_checker: Arc::new(PermissionChecker::new(PermissionSettings::default())),
            cwd: PathBuf::from("/tmp"),
            model: "test-model".into(),
            system_prompt: "test".into(),
            max_tokens: 1024,
            permission_prompt: None,
            ask_user_prompt: None,
            max_turns: 5,
            hook_executor: Some(hook_executor),
            tool_metadata: std::collections::HashMap::new(),
            agent_id: AgentId::new("sub-blocking"),
            parent_id: Some(AgentId::new("main")),
            session_id: Some("sess-block".into()),
            subagents: None,
            tasks: None,
            cancel: CancellationToken::new(),
            hook_registry: None,
            compactor: None,
        }
    }

    /// (a) A blocking `PreToolUse` hook aborts the subagent's tool action: the
    /// tool never runs and the tool result carries the block reason.
    #[tokio::test]
    async fn test_blocking_pretooluse_hook_aborts_subagent_action() {
        let executed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let events = Arc::new(StdMutex::new(Vec::new()));
        let hooks = Arc::new(BlockingHookExecutor {
            block_on: HookEvent::PreToolUse,
            events: Arc::clone(&events),
        });
        let ctx = make_ctx_with_spy(Arc::clone(&executed), hooks);

        // The subagent runs to completion (the block becomes a tool error, which
        // the model "sees" and then finishes), but the underlying tool body must
        // never have executed.
        let out = run_subagent(ctx, "use the tool".into()).await.unwrap();
        assert_eq!(out, "done");
        assert!(
            !executed.load(std::sync::atomic::Ordering::SeqCst),
            "tool body ran despite a blocking PreToolUse hook"
        );
        let seen = events.lock().unwrap();
        assert!(seen.contains(&HookEvent::PreToolUse));
        assert!(seen.contains(&HookEvent::SubagentStart));
    }

    /// (b) A webhook-style hook executor is invoked on subagent lifecycle events.
    /// We assert the stub received `SubagentStart` AND `SubagentStop` (the same
    /// dispatch path an `HttpHookDefinition` would ride).
    #[tokio::test]
    async fn test_webhook_style_hook_invoked_on_subagent_lifecycle() {
        let events = Arc::new(StdMutex::new(Vec::new()));
        // A non-blocking executor stands in for an HTTP/webhook hook: it simply
        // observes every lifecycle event.
        let ctx = make_ctx("hello", Arc::clone(&events));
        let _ = run_subagent(ctx, "go".into()).await.unwrap();

        let seen = events.lock().unwrap();
        assert!(
            seen.iter()
                .filter(|e| **e == HookEvent::SubagentStart)
                .count()
                == 1,
            "webhook should be invoked once on SubagentStart: {seen:?}"
        );
        assert!(
            seen.iter()
                .filter(|e| **e == HookEvent::SubagentStop)
                .count()
                == 1,
            "webhook should be invoked once on SubagentStop: {seen:?}"
        );
    }

    // ── ENG-2: cancellation aborts a turn ────────────────────────────────────

    /// A client whose stream never completes (yields a delta then pends
    /// forever), so the only way `run_query` returns is via cancellation.
    struct HangingClient;

    #[async_trait]
    impl StreamingApiClient for HangingClient {
        async fn stream_message(
            &self,
            _request: ApiMessageRequest,
        ) -> Result<
            Pin<Box<dyn Stream<Item = Result<ApiStreamEvent, ApiError>> + Send + '_>>,
            ApiError,
        > {
            use futures::StreamExt;
            // One delta, then a pending-forever stream: no MessageComplete.
            let head = futures::stream::iter(vec![Ok(ApiStreamEvent::TextDelta(
                ApiTextDeltaEvent { text: "…".into() },
            ))]);
            let tail = futures::stream::pending::<Result<ApiStreamEvent, ApiError>>();
            Ok(Box::pin(head.chain(tail)))
        }
    }

    fn cancellable_ctx(cancel: CancellationToken) -> QueryContext {
        QueryContext {
            api_client: Arc::new(HangingClient),
            tool_registry: Arc::new(ToolRegistry::new()),
            permission_checker: Arc::new(PermissionChecker::new(PermissionSettings::default())),
            cwd: PathBuf::from("/tmp"),
            model: "test-model".into(),
            system_prompt: "test".into(),
            max_tokens: 1024,
            permission_prompt: None,
            ask_user_prompt: None,
            max_turns: 5,
            hook_executor: None,
            tool_metadata: std::collections::HashMap::new(),
            agent_id: AgentId::new("cancel"),
            parent_id: None,
            session_id: None,
            subagents: None,
            tasks: None,
            cancel,
            hook_registry: None,
            compactor: None,
        }
    }

    /// ENG-2: cancelling the token while a turn's stream is in flight aborts the
    /// run with `EngineError::Cancelled` instead of hanging forever.
    #[tokio::test]
    async fn test_cancellation_aborts_in_flight_turn() {
        let cancel = CancellationToken::new();
        let ctx = cancellable_ctx(cancel.clone());
        let mut messages = vec![ConversationMessage::from_user_text("hi")];

        // Cancel shortly after the run starts consuming the (hanging) stream.
        let canceller = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            canceller.cancel();
        });

        let result = run_query(&ctx, &mut messages).await;
        assert!(
            matches!(result, Err(EngineError::Cancelled)),
            "expected Cancelled, got {result:?}"
        );
    }

    /// ENG-2: a token cancelled BEFORE the run starts aborts at the turn guard,
    /// before any provider call.
    #[tokio::test]
    async fn test_precancelled_token_aborts_before_first_turn() {
        let cancel = CancellationToken::new();
        cancel.cancel();
        let ctx = cancellable_ctx(cancel);
        let mut messages = vec![ConversationMessage::from_user_text("hi")];
        let result = run_query(&ctx, &mut messages).await;
        assert!(matches!(result, Err(EngineError::Cancelled)));
    }

    // ── HOOK-2: a blocking PreApiRequest hook aborts the turn ────────────────

    /// HOOK-2: a hook that blocks `PreApiRequest` must abort the turn with
    /// `HookBlocked` — the provider is never called, and no assistant message is
    /// appended.
    #[tokio::test]
    async fn test_blocking_pre_api_request_aborts_turn() {
        let events = Arc::new(StdMutex::new(Vec::new()));
        let hooks = Arc::new(BlockingHookExecutor {
            block_on: HookEvent::PreApiRequest,
            events: Arc::clone(&events),
        });
        // FakeApiClient would return text if reached; the block must prevent that.
        let ctx = QueryContext {
            api_client: Arc::new(FakeApiClient {
                text: "should not appear".into(),
            }),
            tool_registry: Arc::new(ToolRegistry::new()),
            permission_checker: Arc::new(PermissionChecker::new(PermissionSettings::default())),
            cwd: PathBuf::from("/tmp"),
            model: "test-model".into(),
            system_prompt: "test".into(),
            max_tokens: 1024,
            permission_prompt: None,
            ask_user_prompt: None,
            max_turns: 5,
            hook_executor: Some(hooks),
            tool_metadata: std::collections::HashMap::new(),
            agent_id: AgentId::new("hook-block"),
            parent_id: None,
            session_id: None,
            subagents: None,
            tasks: None,
            cancel: CancellationToken::new(),
            hook_registry: None,
            compactor: None,
        };
        let mut messages = vec![ConversationMessage::from_user_text("hi")];
        let result = run_query(&ctx, &mut messages).await;
        assert!(
            matches!(result, Err(EngineError::HookBlocked(_))),
            "expected HookBlocked, got {result:?}"
        );
        // No assistant message should have been appended.
        assert!(
            !messages.iter().any(|m| m.role == Role::Assistant),
            "assistant message appended despite PreApiRequest block"
        );
    }

    // ── ENG-1: proactive compaction triggers ─────────────────────────────────

    /// ENG-1: with a compactor whose threshold is below the seeded history size,
    /// `run_query` compacts the history before the first turn, fires
    /// `ContextCompacted`, and shrinks the message count.
    #[tokio::test]
    async fn test_compaction_triggers_before_turn() {
        use oh_services::compact::Compactor;

        let events = Arc::new(StdMutex::new(Vec::new()));
        // A compactor with a tiny threshold so the seeded history exceeds it.
        // keep_last_n in run_query is COMPACT_KEEP_LAST_N (6); seed enough
        // messages that compaction has something to summarize. The summarizer is
        // the run's own api_client (ApiClientSummarizer); FakeApiClient returns
        // fixed text, which serves as both the summary and the assistant reply.
        let compactor = Arc::new(Compactor::new(1, 1));

        let mut ctx = make_ctx("final answer", Arc::clone(&events));
        ctx.compactor = Some(compactor);

        // Seed a long history (> keep_last_n) so compaction has a summarize span.
        let mut messages: Vec<ConversationMessage> = (0..12)
            .map(|i| {
                if i % 2 == 0 {
                    ConversationMessage::from_user_text(format!("user turn {i} {}", "x".repeat(50)))
                } else {
                    ConversationMessage {
                        role: Role::Assistant,
                        content: vec![ContentBlock::Text(TextBlock::new(format!(
                            "assistant turn {i} {}",
                            "y".repeat(50)
                        )))],
                    }
                }
            })
            .collect();
        let before = messages.len();

        let _ = run_query(&ctx, &mut messages).await.unwrap();

        let seen = events.lock().unwrap();
        assert!(
            seen.contains(&HookEvent::ContextCompacted),
            "ContextCompacted hook should have fired: {seen:?}"
        );
        // After compaction the history is the summary + kept tail (+ the new
        // assistant turn from this run), which is smaller than the seeded 12.
        assert!(
            messages.len() < before + 1,
            "history should have shrunk via compaction (before={before}, after={})",
            messages.len()
        );
    }
}

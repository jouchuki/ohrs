//! Context-window auto-compaction for long sessions.
//!
//! Two compaction modes are supported:
//! - **Proactive**: token estimate of the full request (messages + system prompt + tool schemas)
//!   exceeds `threshold_tokens` before sending the next turn.
//! - **Reactive**: the API returns a prompt-too-long error; catch, compact, retry.
//!
//! # Token estimation
//! Exact tokenisation (e.g. tiktoken) would require a heavy dependency and provider-specific
//! vocabularies.  We use the industry-standard heuristic: `total_chars / 4`, plus a fixed
//! overhead of 10 tokens per `ToolUse` block to account for JSON schema serialisation.  This
//! is intentionally approximate — the goal is to trigger compaction *before* the provider
//! rejects the request, so false negatives are more costly than false positives.
//!
//! # Split-point safety
//! The split point is snapped backward to the start of the nearest **complete tool-call pair**
//! (i.e. an assistant `ToolUse` message followed immediately by a user `ToolResult` message).
//! This prevents the preserved tail from containing an orphaned `ToolResult` whose corresponding
//! `ToolUse` was summarized away — a structural violation that most providers reject.

use async_trait::async_trait;
use thiserror::Error;

use oh_types::{
    api::ApiError,
    messages::{ContentBlock, ConversationMessage, Role, TextBlock},
};

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Errors that can occur during compaction.
#[derive(Debug, Error)]
pub enum CompactError {
    #[error("summarizer returned an error: {0}")]
    Summarizer(String),

    #[error("not enough messages to compact (need at least keep_last_n + 1)")]
    TooFewMessages,
}

// ---------------------------------------------------------------------------
// Summarizer trait
// ---------------------------------------------------------------------------

/// Pluggable summarizer — caller supplies an implementation (e.g. wrapping the
/// existing Anthropic client).  The trait is intentionally LLM-agnostic.
#[async_trait]
pub trait CompactSummarizer: Send + Sync {
    /// Summarize `history` (a human-readable transcript of messages to compress)
    /// given the current `system` prompt for context.  Returns the narrative summary.
    async fn summarize(&self, system: &str, history: &str) -> Result<String, CompactError>;
}

// ---------------------------------------------------------------------------
// Request / result types
// ---------------------------------------------------------------------------

/// Parameters for a single compaction run.
pub struct CompactRequest<'a> {
    /// Full conversation history to consider for compaction.
    pub messages: &'a [ConversationMessage],
    /// Always preserve (at least) the last `keep_last_n` messages verbatim in the output.
    /// The actual number preserved may be larger if snapping to a tool-call boundary requires it.
    pub keep_last_n: usize,
    /// The active system prompt (passed to the summarizer for context, and included in the
    /// full-request token estimate used by `should_compact_full`).
    pub system_prompt: &'a str,
}

/// Output of a successful compaction.
#[derive(Debug)]
pub struct CompactResult {
    /// Narrative summary of the portion that was compressed.
    pub summary: String,
    /// Replacement message list: synthetic summary message + tail verbatim messages.
    /// Length >= `keep_last_n + 1` (the `+1` is the synthetic user message containing
    /// the summary; more messages may be kept if the split was snapped to a boundary).
    pub kept_messages: Vec<ConversationMessage>,
    /// Estimated token count of the *original* message list (before compaction).
    pub estimated_tokens_before: u32,
    /// Estimated token count of `kept_messages` (after compaction).
    pub estimated_tokens_after: u32,
}

// ---------------------------------------------------------------------------
// Compactor
// ---------------------------------------------------------------------------

/// Stateless compactor — holds configuration, performs token estimation and
/// orchestrates the summarizer.
pub struct Compactor {
    /// Compact when the estimated token count exceeds this value.
    pub threshold_tokens: u32,
    /// Hint to the summarizer: target length of the produced summary in tokens.
    /// Not enforced mechanically; passed as context in the transcript header.
    pub summary_target_tokens: u32,
}

impl Compactor {
    /// Create a new `Compactor` with the given thresholds.
    pub fn new(threshold_tokens: u32, summary_target_tokens: u32) -> Self {
        Self {
            threshold_tokens,
            summary_target_tokens,
        }
    }

    // -----------------------------------------------------------------------
    // Token estimation
    // -----------------------------------------------------------------------

    /// Estimate the number of tokens in a slice of messages.
    ///
    /// **Heuristic** (not exact): `total_chars / 4 + tool_use_blocks * 10`.
    ///
    /// The `/ 4` approximation holds well for English prose.  The `* 10` overhead
    /// per `ToolUse` block accounts for the JSON structure (name, id, input keys)
    /// that is invisible in plain-text character counts.  Even tiktoken would be
    /// inaccurate across providers (Anthropic vs OpenAI vocabularies differ), so
    /// we prefer the simpler, dependency-free approach and document the trade-off.
    pub fn estimate_tokens(messages: &[ConversationMessage]) -> u32 {
        let mut total_chars: usize = 0;
        let mut tool_use_count: usize = 0;

        for msg in messages {
            for block in &msg.content {
                match block {
                    ContentBlock::Text(t) => {
                        total_chars += t.text.len();
                    }
                    ContentBlock::ToolUse(t) => {
                        tool_use_count += 1;
                        // Also count the stringified input JSON
                        total_chars += t.name.len();
                        total_chars += t.input.len() * 20; // rough JSON overhead per key
                    }
                    ContentBlock::ToolResult(r) => {
                        total_chars += r.content.len();
                    }
                }
            }
        }

        let char_tokens = (total_chars / 4) as u32;
        let tool_tokens = (tool_use_count * 10) as u32;
        char_tokens + tool_tokens
    }

    /// Estimate tokens for the **full API request**: messages + system prompt + tool schemas.
    ///
    /// This is the value that should be compared against `threshold_tokens` for proactive
    /// compaction, because providers count the entire request against the context window —
    /// not just the message history.
    ///
    /// `tool_schemas_chars` is the approximate character count of the serialised tool
    /// definitions sent in the request (pass `0` if no tools are in use).
    pub fn estimate_tokens_full(
        messages: &[ConversationMessage],
        system_prompt: &str,
        tool_schemas_chars: usize,
    ) -> u32 {
        let msg_tokens = Self::estimate_tokens(messages);
        let system_tokens = (system_prompt.len() / 4) as u32;
        let tool_tokens = (tool_schemas_chars / 4) as u32;
        msg_tokens + system_tokens + tool_tokens
    }

    // -----------------------------------------------------------------------
    // Threshold check
    // -----------------------------------------------------------------------

    /// Return `true` when proactive compaction should be triggered based on message tokens only.
    ///
    /// Prefer [`should_compact_full`] when you have access to the system prompt and tool schemas,
    /// as those can contribute significantly to total request size.
    pub fn should_compact(&self, messages: &[ConversationMessage]) -> bool {
        Self::estimate_tokens(messages) >= self.threshold_tokens
    }

    /// Return `true` when proactive compaction should be triggered, accounting for the full
    /// API request (messages + system prompt + serialised tool schemas).
    pub fn should_compact_full(
        &self,
        messages: &[ConversationMessage],
        system_prompt: &str,
        tool_schemas_chars: usize,
    ) -> bool {
        Self::estimate_tokens_full(messages, system_prompt, tool_schemas_chars)
            >= self.threshold_tokens
    }

    // -----------------------------------------------------------------------
    // Compact
    // -----------------------------------------------------------------------

    /// Compact the conversation.
    ///
    /// Splits `req.messages` into:
    /// - `to_summarize`: everything *except* the last `keep_last_n` messages.
    /// - `to_keep`: the last `keep_last_n` messages (preserved verbatim).
    ///
    /// The split point is snapped backward to the start of the nearest complete
    /// tool-call pair so the preserved tail never starts with an orphaned
    /// `ToolResult` message.
    ///
    /// Calls `summarizer.summarize()` on a human-readable transcript of
    /// `to_summarize`, then returns a `CompactResult` whose `kept_messages` is:
    ///
    /// ```text
    /// [ synthetic_user_message(summary), to_keep[0], to_keep[1], … ]
    /// ```
    ///
    /// This gives the model full context of recent activity while the earlier
    /// history is represented by the summary.
    pub async fn compact(
        &self,
        req: CompactRequest<'_>,
        summarizer: &dyn CompactSummarizer,
    ) -> Result<CompactResult, CompactError> {
        let total = req.messages.len();
        let keep = req.keep_last_n;

        // Need at least one message to summarize.
        if total <= keep {
            return Err(CompactError::TooFewMessages);
        }

        let naive_split = total - keep;

        // Snap the split point backward to a safe tool-call boundary.
        // A boundary is safe when the message at `split_at` is NOT a ToolResult
        // whose ToolUse is in the summarized portion.  We scan backward from the
        // naive split until we reach a non-ToolResult message or the beginning.
        let split_at = Self::safe_split_point(req.messages, naive_split);

        // After snapping, ensure we still have something to summarize.
        if split_at == 0 {
            return Err(CompactError::TooFewMessages);
        }

        let to_summarize = &req.messages[..split_at];
        let to_keep = &req.messages[split_at..];

        let estimated_tokens_before = Self::estimate_tokens(req.messages);

        // Build a human-readable transcript for the summarizer.
        let transcript = Self::format_transcript(to_summarize, self.summary_target_tokens);

        // Ask the summarizer for a narrative summary.
        let summary = summarizer.summarize(req.system_prompt, &transcript).await?;

        // Construct the synthetic "summary" message as a user-role message so it
        // fits the alternating user/assistant pattern expected by all providers.
        let summary_message = ConversationMessage {
            role: Role::User,
            content: vec![ContentBlock::Text(TextBlock::new(format!(
                "[Context summary — earlier conversation compressed]\n\n{summary}"
            )))],
        };

        let mut kept_messages = Vec::with_capacity(to_keep.len() + 1);
        kept_messages.push(summary_message);
        kept_messages.extend_from_slice(to_keep);

        let estimated_tokens_after = Self::estimate_tokens(&kept_messages);

        Ok(CompactResult {
            summary,
            kept_messages,
            estimated_tokens_before,
            estimated_tokens_after,
        })
    }

    // -----------------------------------------------------------------------
    // Error pattern matching
    // -----------------------------------------------------------------------

    /// Return `true` when `err` indicates the model rejected the request because
    /// the context window was exceeded.
    ///
    /// Pattern-matches against known strings from all three providers:
    /// - **Anthropic**: `"context_window_exceeded"`, `"context window"`, `"prompt too long"`
    /// - **OpenAI**: `"context_length_exceeded"`, `"maximum context length"`, `"context length"`
    /// - **Codex / generic**: `"too many tokens"`, `"too large for the model"`, `"maximum context"`
    ///
    /// All provider errors are currently wrapped in `ApiError::Request(body)` by the API
    /// layer; the other variants (`Authentication`, `RateLimit`, `Network`) are not context
    /// errors and will correctly return `false`.
    pub fn is_prompt_too_long_error(err: &ApiError) -> bool {
        let text = err.to_string().to_lowercase();
        // Patterns drawn from Python reference implementation plus Anthropic / OpenAI
        // error message strings observed in production.
        const NEEDLES: &[&str] = &[
            "context_window_exceeded",
            "context_length_exceeded",
            "context window",
            "context length",
            "maximum context length",
            "maximum context",
            "prompt too long",
            "too many tokens",
            "too large for the model",
        ];
        NEEDLES.iter().any(|&needle| text.contains(needle))
    }

    // -----------------------------------------------------------------------
    // Internal helpers
    // -----------------------------------------------------------------------

    /// Compute a safe split index.
    ///
    /// Starting from `naive_split`, walk backward until the message at that index is
    /// not a `ToolResult`-only message.  This ensures the preserved tail always starts
    /// with either an assistant message or a genuine user-text message, never with an
    /// orphaned tool result whose `ToolUse` was compressed away.
    fn safe_split_point(messages: &[ConversationMessage], naive_split: usize) -> usize {
        let mut split = naive_split;
        while split > 0 && Self::is_tool_result_message(&messages[split]) {
            split -= 1;
        }
        split
    }

    /// Return true if the message consists entirely of `ToolResult` blocks.
    fn is_tool_result_message(msg: &ConversationMessage) -> bool {
        !msg.content.is_empty()
            && msg.content.iter().all(|b| matches!(b, ContentBlock::ToolResult(_)))
    }

    /// Format a message slice as a human-readable transcript for the summarizer.
    fn format_transcript(messages: &[ConversationMessage], target_tokens: u32) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "Please summarize the following conversation transcript in approximately \
             {target_tokens} tokens, preserving key decisions, tool outputs, and \
             context that would be needed to continue the session.\n\n---\n\n"
        ));

        for (i, msg) in messages.iter().enumerate() {
            let role_label = match msg.role {
                Role::User => "USER",
                Role::Assistant => "ASSISTANT",
            };
            out.push_str(&format!("[Turn {i}] {role_label}:\n"));

            for block in &msg.content {
                match block {
                    ContentBlock::Text(t) => {
                        out.push_str(&t.text);
                        out.push('\n');
                    }
                    ContentBlock::ToolUse(t) => {
                        out.push_str(&format!(
                            "<tool_use name=\"{}\" id=\"{}\"/>\n",
                            t.name, t.id
                        ));
                    }
                    ContentBlock::ToolResult(r) => {
                        let preview = if r.content.len() > 200 {
                            format!("{}…[truncated]", &r.content[..200])
                        } else {
                            r.content.clone()
                        };
                        out.push_str(&format!(
                            "<tool_result id=\"{}\" error={}>{}</tool_result>\n",
                            r.tool_use_id, r.is_error, preview
                        ));
                    }
                }
            }
            out.push('\n');
        }

        out
    }
}

// ---------------------------------------------------------------------------
// MockSummarizer (available in tests and for callers that need a stub)
// ---------------------------------------------------------------------------

/// Test / stub summarizer that always returns a fixed string.
pub struct MockSummarizer {
    pub response: String,
}

impl MockSummarizer {
    pub fn new(response: impl Into<String>) -> Self {
        Self {
            response: response.into(),
        }
    }
}

#[async_trait]
impl CompactSummarizer for MockSummarizer {
    async fn summarize(&self, _system: &str, _history: &str) -> Result<String, CompactError> {
        Ok(self.response.clone())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use oh_types::messages::{ContentBlock, TextBlock, ToolResultBlock, ToolUseBlock};
    use std::collections::HashMap;

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    fn text_msg(role: Role, text: &str) -> ConversationMessage {
        ConversationMessage {
            role,
            content: vec![ContentBlock::Text(TextBlock::new(text))],
        }
    }

    fn tool_result_msg(tool_use_id: &str) -> ConversationMessage {
        ConversationMessage {
            role: Role::User,
            content: vec![ContentBlock::ToolResult(ToolResultBlock::new(
                tool_use_id,
                "output",
                false,
            ))],
        }
    }

    fn make_history(n: usize) -> Vec<ConversationMessage> {
        (0..n)
            .map(|i| {
                if i % 2 == 0 {
                    text_msg(Role::User, &"a".repeat(400))
                } else {
                    text_msg(Role::Assistant, &"b".repeat(400))
                }
            })
            .collect()
    }

    // -----------------------------------------------------------------------
    // estimate_tokens
    // -----------------------------------------------------------------------

    #[test]
    fn estimate_tokens_proportional_to_chars() {
        let short = vec![text_msg(Role::User, "hello")];
        let long = vec![text_msg(Role::User, &"hello".repeat(100))];
        let t_short = Compactor::estimate_tokens(&short);
        let t_long = Compactor::estimate_tokens(&long);
        assert!(t_long > t_short * 50, "token estimate should scale with content");
    }

    #[test]
    fn estimate_tokens_accounts_for_tool_use_blocks() {
        let text_only = vec![text_msg(Role::User, &"x".repeat(400))];
        let with_tool = vec![ConversationMessage {
            role: Role::User,
            content: vec![
                ContentBlock::Text(TextBlock::new("x".repeat(400))),
                ContentBlock::ToolUse(ToolUseBlock::new("bash", HashMap::new())),
            ],
        }];
        let t_text = Compactor::estimate_tokens(&text_only);
        let t_tool = Compactor::estimate_tokens(&with_tool);
        // 10 extra tokens for the tool_use block overhead
        assert_eq!(t_tool, t_text + 10 + ("bash".len() / 4) as u32);
    }

    #[test]
    fn estimate_tokens_empty_is_zero() {
        assert_eq!(Compactor::estimate_tokens(&[]), 0);
    }

    #[test]
    fn estimate_tokens_tool_result_content_counted() {
        let msg = ConversationMessage {
            role: Role::User,
            content: vec![ContentBlock::ToolResult(ToolResultBlock::new(
                "id1",
                "x".repeat(400),
                false,
            ))],
        };
        let t = Compactor::estimate_tokens(&[msg]);
        // 400 chars / 4 = 100 tokens
        assert_eq!(t, 100);
    }

    #[test]
    fn estimate_tokens_full_adds_system_and_tools() {
        let msgs = vec![text_msg(Role::User, &"x".repeat(400))];
        let msg_only = Compactor::estimate_tokens(&msgs);
        let full = Compactor::estimate_tokens_full(&msgs, &"s".repeat(400), 400);
        // system: 100, tools: 100, so full should be msg_only + 200
        assert_eq!(full, msg_only + 200);
    }

    // -----------------------------------------------------------------------
    // should_compact
    // -----------------------------------------------------------------------

    #[test]
    fn should_compact_false_below_threshold() {
        let compactor = Compactor::new(10_000, 500);
        let msgs = make_history(2); // ~200 tokens
        assert!(!compactor.should_compact(&msgs));
    }

    #[test]
    fn should_compact_true_above_threshold() {
        let compactor = Compactor::new(50, 20); // very low threshold
        let msgs = make_history(4); // 4 * 400 chars / 4 = 400 tokens >> 50
        assert!(compactor.should_compact(&msgs));
    }

    #[test]
    fn should_compact_false_on_empty() {
        let compactor = Compactor::new(1_000, 200);
        assert!(!compactor.should_compact(&[]));
    }

    #[test]
    fn should_compact_full_triggers_on_system_prompt() {
        let compactor = Compactor::new(200, 50);
        // messages alone: ~100 tokens; with large system prompt: >>200
        let msgs = make_history(1);
        assert!(!compactor.should_compact(&msgs));
        assert!(compactor.should_compact_full(&msgs, &"s".repeat(4000), 0));
    }

    // -----------------------------------------------------------------------
    // compact
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn compact_kept_messages_length() {
        let compactor = Compactor::new(100, 50);
        let msgs = make_history(10);
        let keep_last_n = 3usize;
        let summarizer = MockSummarizer::new("This is a test summary.");

        let req = CompactRequest {
            messages: &msgs,
            keep_last_n,
            system_prompt: "You are a helpful assistant.",
        };

        let result = compactor.compact(req, &summarizer).await.unwrap();
        // kept_messages = 1 synthetic summary msg + keep_last_n verbatim msgs
        assert_eq!(result.kept_messages.len(), keep_last_n + 1);
    }

    #[tokio::test]
    async fn compact_first_message_is_summary() {
        let compactor = Compactor::new(100, 50);
        let msgs = make_history(6);
        let summarizer = MockSummarizer::new("Summary content here.");

        let req = CompactRequest {
            messages: &msgs,
            keep_last_n: 2,
            system_prompt: "System.",
        };

        let result = compactor.compact(req, &summarizer).await.unwrap();
        let first = &result.kept_messages[0];
        assert_eq!(first.role, Role::User);
        let text = first.text();
        assert!(
            text.contains("Summary content here."),
            "first kept message should embed the summary"
        );
        assert!(
            text.contains("[Context summary"),
            "first kept message should have summary header"
        );
    }

    #[tokio::test]
    async fn compact_tail_messages_preserved_verbatim() {
        let compactor = Compactor::new(100, 50);
        let keep_last_n = 2;
        let msgs = make_history(6);
        let tail: Vec<ConversationMessage> = msgs[msgs.len() - keep_last_n..].to_vec();
        let summarizer = MockSummarizer::new("summary");

        let req = CompactRequest {
            messages: &msgs,
            keep_last_n,
            system_prompt: "sys",
        };

        let result = compactor.compact(req, &summarizer).await.unwrap();
        // Skip the synthetic summary message (index 0); compare tail
        for (i, expected) in tail.iter().enumerate() {
            assert_eq!(&result.kept_messages[i + 1], expected);
        }
    }

    #[tokio::test]
    async fn compact_tokens_after_less_than_before() {
        let compactor = Compactor::new(50, 20);
        let msgs = make_history(10);
        let summarizer = MockSummarizer::new("short summary");

        let req = CompactRequest {
            messages: &msgs,
            keep_last_n: 2,
            system_prompt: "sys",
        };

        let result = compactor.compact(req, &summarizer).await.unwrap();
        assert!(
            result.estimated_tokens_after < result.estimated_tokens_before,
            "compaction should reduce estimated token count"
        );
    }

    #[tokio::test]
    async fn compact_too_few_messages_returns_error() {
        let compactor = Compactor::new(100, 50);
        let msgs = make_history(3);
        let summarizer = MockSummarizer::new("x");

        let req = CompactRequest {
            messages: &msgs,
            keep_last_n: 5, // more than total
            system_prompt: "sys",
        };

        let err = compactor.compact(req, &summarizer).await.unwrap_err();
        assert!(matches!(err, CompactError::TooFewMessages));
    }

    #[tokio::test]
    async fn compact_snaps_split_past_orphaned_tool_result() {
        // History: [user, assistant+tool_use, user+tool_result, user, assistant]
        // keep_last_n=3: naive split=2 lands on the user+tool_result message.
        // The snap should move it to 1 (before the tool_result), preserving the
        // pair [assistant+tool_use, user+tool_result] together in the kept tail.
        let mut msgs = Vec::new();
        msgs.push(text_msg(Role::User, "initial question"));
        // assistant with a tool_use
        msgs.push(ConversationMessage {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse(ToolUseBlock::new(
                "bash",
                HashMap::new(),
            ))],
        });
        // user tool_result
        msgs.push(tool_result_msg("some-id"));
        msgs.push(text_msg(Role::User, "follow-up"));
        msgs.push(text_msg(Role::Assistant, "answer"));

        let summarizer = MockSummarizer::new("summary");
        let compactor = Compactor::new(100, 50);
        let req = CompactRequest {
            messages: &msgs,
            keep_last_n: 3, // naive_split = 5-3 = 2 → lands on tool_result_msg
            system_prompt: "sys",
        };
        let result = compactor.compact(req, &summarizer).await.unwrap();
        // The split snapped to index 1, so kept = msgs[1..] (4 messages) + summary = 5
        // but msgs[1] is the tool_use, not a ToolResult, so split stays at 2 only if
        // msgs[2] is not a ToolResult — but it IS, so split backs up to 1.
        // kept_messages[0] = summary, then msgs[1..5] = 4 messages
        assert_eq!(result.kept_messages.len(), 5);
        // The second message in kept should be the assistant+tool_use
        assert_eq!(result.kept_messages[1].role, Role::Assistant);
    }

    // -----------------------------------------------------------------------
    // is_prompt_too_long_error
    // -----------------------------------------------------------------------

    #[test]
    fn is_prompt_too_long_anthropic_context_window_exceeded() {
        let err = ApiError::Request(
            "anthropic error: context_window_exceeded: your prompt is too long".into(),
        );
        assert!(Compactor::is_prompt_too_long_error(&err));
    }

    #[test]
    fn is_prompt_too_long_openai_context_length_exceeded() {
        let err = ApiError::Request(
            "openai error: context_length_exceeded: This model's maximum context length is 128000 tokens".into(),
        );
        assert!(Compactor::is_prompt_too_long_error(&err));
    }

    #[test]
    fn is_prompt_too_long_openai_maximum_context_length() {
        let err = ApiError::Request(
            "maximum context length exceeded for model gpt-4o".into(),
        );
        assert!(Compactor::is_prompt_too_long_error(&err));
    }

    #[test]
    fn is_prompt_too_long_generic_too_many_tokens() {
        let err = ApiError::Request("too many tokens in request".into());
        assert!(Compactor::is_prompt_too_long_error(&err));
    }

    #[test]
    fn is_prompt_too_long_generic_prompt_too_long() {
        let err = ApiError::Request("prompt too long for this model".into());
        assert!(Compactor::is_prompt_too_long_error(&err));
    }

    #[test]
    fn is_prompt_too_long_too_large_for_model() {
        let err = ApiError::Request("request is too large for the model".into());
        assert!(Compactor::is_prompt_too_long_error(&err));
    }

    #[test]
    fn is_prompt_too_long_false_for_rate_limit() {
        let err = ApiError::RateLimit("429 Too Many Requests".into());
        assert!(!Compactor::is_prompt_too_long_error(&err));
    }

    #[test]
    fn is_prompt_too_long_false_for_auth_error() {
        let err = ApiError::Authentication("invalid api key".into());
        assert!(!Compactor::is_prompt_too_long_error(&err));
    }

    #[test]
    fn is_prompt_too_long_false_for_network_error() {
        let err = ApiError::Network("connection timed out".into());
        assert!(!Compactor::is_prompt_too_long_error(&err));
    }

    #[test]
    fn is_prompt_too_long_context_window_phrase() {
        let err = ApiError::Request(
            "your request exceeded the context window for claude-3-opus".into(),
        );
        assert!(Compactor::is_prompt_too_long_error(&err));
    }

    // -----------------------------------------------------------------------
    // MockSummarizer
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn mock_summarizer_returns_fixed_string() {
        let s = MockSummarizer::new("fixed response");
        let out = s.summarize("sys", "history").await.unwrap();
        assert_eq!(out, "fixed response");
    }

    #[tokio::test]
    async fn mock_summarizer_ignores_inputs() {
        let s = MockSummarizer::new("always same");
        let r1 = s.summarize("sys1", "h1").await.unwrap();
        let r2 = s.summarize("sys2", "h2").await.unwrap();
        assert_eq!(r1, r2);
    }
}

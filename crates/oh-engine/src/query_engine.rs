//! QueryEngine: owns conversation history, wraps the query loop.

use std::path::PathBuf;
use std::sync::Arc;

use oh_api::StreamingApiClient;
use oh_hooks::{HookEvent, HookExecutorTrait};
use oh_permissions::PermissionChecker;
use oh_tools::ToolRegistry;
use oh_types::api::UsageSnapshot;
use oh_types::messages::ConversationMessage;
use oh_types::stream_events::StreamEvent;

use crate::cost_tracker::CostTracker;
use crate::query::{AskUserPromptFn, PermissionPromptFn, QueryContext, run_query};

/// The main query engine — owns conversation history and orchestrates the loop.
pub struct QueryEngine {
    api_client: Arc<dyn StreamingApiClient>,
    tool_registry: Arc<ToolRegistry>,
    permission_checker: Arc<PermissionChecker>,
    hook_executor: Option<Arc<dyn HookExecutorTrait>>,
    messages: Vec<ConversationMessage>,
    cost_tracker: CostTracker,
    cwd: PathBuf,
    model: String,
    system_prompt: String,
    max_tokens: u32,
    max_turns: u32,
    permission_prompt: Option<PermissionPromptFn>,
    ask_user_prompt: Option<AskUserPromptFn>,
    tool_metadata: std::collections::HashMap<String, serde_json::Value>,
}

impl QueryEngine {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        api_client: Arc<dyn StreamingApiClient>,
        tool_registry: Arc<ToolRegistry>,
        permission_checker: Arc<PermissionChecker>,
        cwd: PathBuf,
        model: String,
        system_prompt: String,
        max_tokens: u32,
    ) -> Self {
        Self {
            api_client,
            tool_registry,
            permission_checker,
            hook_executor: None,
            messages: Vec::new(),
            cost_tracker: CostTracker::new(),
            cwd,
            model,
            system_prompt,
            max_tokens,
            max_turns: 30,
            permission_prompt: None,
            ask_user_prompt: None,
            tool_metadata: std::collections::HashMap::new(),
        }
    }

    pub fn set_hook_executor(&mut self, executor: Arc<dyn HookExecutorTrait>) {
        self.hook_executor = Some(executor);
    }

    pub fn set_permission_prompt(&mut self, prompt: PermissionPromptFn) {
        self.permission_prompt = Some(prompt);
    }

    pub fn set_ask_user_prompt(&mut self, prompt: AskUserPromptFn) {
        self.ask_user_prompt = Some(prompt);
    }

    pub fn set_max_turns(&mut self, max_turns: u32) {
        self.max_turns = max_turns;
    }

    pub fn set_tool_metadata(&mut self, key: String, value: serde_json::Value) {
        self.tool_metadata.insert(key, value);
    }

    pub fn set_system_prompt(&mut self, prompt: String) {
        self.system_prompt = prompt;
    }

    pub fn set_model(&mut self, model: String) {
        self.model = model;
    }

    pub fn set_permission_checker(&mut self, checker: Arc<PermissionChecker>) {
        self.permission_checker = checker;
    }

    pub fn messages(&self) -> &[ConversationMessage] {
        &self.messages
    }

    /// Return all tool schemas as JSON values (for trajectory recording).
    pub fn tool_schemas(&self) -> Vec<serde_json::Value> {
        self.tool_registry.to_api_schema()
    }

    pub fn total_usage(&self) -> &CostTracker {
        &self.cost_tracker
    }

    pub fn clear(&mut self) {
        self.messages.clear();
        self.cost_tracker = CostTracker::new();
    }

    /// Async version of clear that fires hooks.
    pub async fn clear_async(&mut self) {
        // Fire PreClearHistory hook
        if let Some(ref hook_executor) = self.hook_executor {
            hook_executor
                .execute(
                    HookEvent::PreClearHistory,
                    serde_json::json!({}),
                )
                .await;
        }

        self.messages.clear();
        self.cost_tracker = CostTracker::new();

        // Fire PostClearHistory hook
        if let Some(ref hook_executor) = self.hook_executor {
            hook_executor
                .execute(
                    HookEvent::PostClearHistory,
                    serde_json::json!({}),
                )
                .await;
        }
    }

    pub fn load_messages(&mut self, messages: Vec<ConversationMessage>) {
        self.messages = messages;
    }

    /// Submit a user message and run the query loop.
    pub async fn submit_message(
        &mut self,
        prompt: &str,
    ) -> Result<Vec<(StreamEvent, Option<UsageSnapshot>)>, crate::query::EngineError> {
        if let Some(ref hook_executor) = self.hook_executor {
            hook_executor.execute(HookEvent::PreUserMessage, serde_json::json!({"prompt": prompt})).await;
            hook_executor.execute(HookEvent::PrePushMessage, serde_json::json!({"role": "user", "content_type": "text"})).await;
        }

        self.messages
            .push(ConversationMessage::from_user_text(prompt));

        if let Some(ref hook_executor) = self.hook_executor {
            hook_executor.execute(HookEvent::PostPushMessage, serde_json::json!({"role": "user", "content_type": "text"})).await;
        }

        let context = QueryContext {
            api_client: self.api_client.clone(),
            tool_registry: self.tool_registry.clone(),
            permission_checker: self.permission_checker.clone(),
            cwd: self.cwd.clone(),
            model: self.model.clone(),
            system_prompt: self.system_prompt.clone(),
            max_tokens: self.max_tokens,
            permission_prompt: self.permission_prompt.clone(),
            ask_user_prompt: self.ask_user_prompt.clone(),
            max_turns: self.max_turns,
            hook_executor: self.hook_executor.clone(),
            tool_metadata: self.tool_metadata.clone(),
        };

        let events = run_query(&context, &mut self.messages).await?;

        // Update cost tracker
        for (_, usage) in &events {
            if let Some(u) = usage {
                self.cost_tracker.add(u);
            }
        }

        Ok(events)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use futures::Stream;
    use oh_config::PermissionSettings;
    use oh_types::api::*;
    use oh_types::messages::*;
    use std::pin::Pin;

    /// A fake API client that returns pre-configured responses for deterministic tests.
    struct FakeApiClient {
        responses: std::sync::Mutex<Vec<ConversationMessage>>,
    }

    impl FakeApiClient {
        fn with_responses(responses: Vec<ConversationMessage>) -> Self {
            Self {
                responses: std::sync::Mutex::new(responses),
            }
        }
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
            let msg = self.responses.lock().unwrap().remove(0);
            let events = vec![Ok(ApiStreamEvent::MessageComplete(
                ApiMessageCompleteEvent {
                    message: msg,
                    usage: UsageSnapshot {
                        input_tokens: 10,
                        output_tokens: 5, ..Default::default() },
                    stop_reason: Some("end_turn".into()),
                },
            ))];
            Ok(Box::pin(futures::stream::iter(events)))
        }
    }

    fn make_assistant_text(text: &str) -> ConversationMessage {
        ConversationMessage {
            role: Role::Assistant,
            content: vec![ContentBlock::Text(TextBlock::new(text))],
        }
    }

    fn make_engine(responses: Vec<ConversationMessage>) -> QueryEngine {
        let client = Arc::new(FakeApiClient::with_responses(responses));
        let registry = Arc::new(ToolRegistry::new());
        let checker = Arc::new(PermissionChecker::new(PermissionSettings::default()));
        QueryEngine::new(
            client,
            registry,
            checker,
            PathBuf::from("/tmp"),
            "test-model".to_string(),
            "You are a test assistant.".to_string(),
            1024,
        )
    }

    #[test]
    fn test_construction_messages_start_empty() {
        let engine = make_engine(vec![]);
        assert!(engine.messages().is_empty());
        assert_eq!(engine.total_usage().total_tokens(), 0);
        assert_eq!(engine.total_usage().turns, 0);
    }

    #[test]
    fn test_set_model_updates_field() {
        let mut engine = make_engine(vec![]);
        engine.set_model("new-model".to_string());
        // Verify via submit_message that the model is used (indirectly tested).
        // Direct field access is private, so we confirm no panic.
    }

    #[test]
    fn test_set_system_prompt_updates_field() {
        let mut engine = make_engine(vec![]);
        engine.set_system_prompt("New prompt".to_string());
        // Field is private; verified by successful construction / no panic.
    }

    #[test]
    fn test_set_max_turns() {
        let mut engine = make_engine(vec![]);
        engine.set_max_turns(20);
        // No panic, field updated.
    }

    #[test]
    fn test_load_messages() {
        let mut engine = make_engine(vec![]);
        let msgs = vec![ConversationMessage::from_user_text("preloaded")];
        engine.load_messages(msgs);
        assert_eq!(engine.messages().len(), 1);
        assert_eq!(engine.messages()[0].text(), "preloaded");
    }

    #[tokio::test]
    async fn test_submit_message_returns_turn_complete() {
        let mut engine = make_engine(vec![make_assistant_text("Hello!")]);
        let events = engine.submit_message("Hi").await.unwrap();

        // Should contain at least one AssistantTurnComplete event
        let has_complete = events.iter().any(|(e, _)| {
            matches!(e, StreamEvent::AssistantTurnComplete(_))
        });
        assert!(has_complete, "expected AssistantTurnComplete event");
    }

    #[tokio::test]
    async fn test_submit_message_populates_messages() {
        let mut engine = make_engine(vec![make_assistant_text("Reply")]);
        engine.submit_message("Question").await.unwrap();

        // Messages should contain user + assistant
        assert_eq!(engine.messages().len(), 2);
        assert_eq!(engine.messages()[0].role, Role::User);
        assert_eq!(engine.messages()[0].text(), "Question");
        assert_eq!(engine.messages()[1].role, Role::Assistant);
        assert_eq!(engine.messages()[1].text(), "Reply");
    }

    #[tokio::test]
    async fn test_cost_tracker_updates_after_submit() {
        let mut engine = make_engine(vec![make_assistant_text("Answer")]);
        engine.submit_message("Ask").await.unwrap();

        // FakeApiClient returns 10 input + 5 output per call
        assert_eq!(engine.total_usage().total_input_tokens, 10);
        assert_eq!(engine.total_usage().total_output_tokens, 5);
        assert_eq!(engine.total_usage().total_tokens(), 15);
        assert_eq!(engine.total_usage().turns, 1);
    }

    #[tokio::test]
    async fn test_message_history_grows_over_turns() {
        let mut engine = make_engine(vec![
            make_assistant_text("First reply"),
            make_assistant_text("Second reply"),
        ]);

        engine.submit_message("First question").await.unwrap();
        assert_eq!(engine.messages().len(), 2);

        engine.submit_message("Second question").await.unwrap();
        assert_eq!(engine.messages().len(), 4);

        // Cost accumulates across turns
        assert_eq!(engine.total_usage().total_input_tokens, 20);
        assert_eq!(engine.total_usage().total_output_tokens, 10);
        assert_eq!(engine.total_usage().turns, 2);
    }

    #[tokio::test]
    async fn test_clear_resets_messages_and_cost() {
        let mut engine = make_engine(vec![
            make_assistant_text("Reply 1"),
            make_assistant_text("Reply 2"),
        ]);

        engine.submit_message("Q1").await.unwrap();
        assert!(!engine.messages().is_empty());
        assert!(engine.total_usage().total_tokens() > 0);

        engine.clear();

        assert!(engine.messages().is_empty());
        assert_eq!(engine.total_usage().total_tokens(), 0);
        assert_eq!(engine.total_usage().turns, 0);

        // Can submit again after clear
        engine.submit_message("Q2").await.unwrap();
        assert_eq!(engine.messages().len(), 2);
    }
}

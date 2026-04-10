//! Main TUI application — event loop, state, and coordination.

use std::io::{self, Stdout};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::ExecutableCommand;
use ratatui::prelude::*;
use ratatui::Terminal;

use oh_api::StreamingApiClient;
use oh_engine::QueryEngine;
use oh_hooks::{HookEvent, HookExecutorTrait};
use oh_types::stream_events::StreamEvent;

use super::input::handle_key;
use super::render;
use super::transcript::{TranscriptEntry, TranscriptRole};

/// Modal state.
#[derive(Debug, Clone)]
pub enum Modal {
    Permission {
        tool_name: String,
        reason: Option<String>,
    },
    Question {
        question: String,
        input: String,
    },
}

/// Action returned by input handling.
#[derive(Debug)]
pub enum InputAction {
    None,
    Redraw,
    Submit(String),
    Quit,
    PermissionResponse(bool),
    QuestionResponse(String),
}

/// Full TUI application state.
pub struct AppState {
    pub input: String,
    pub cursor_pos: usize,
    pub history: Vec<String>,
    pub history_index: Option<usize>,
    pub transcript: Vec<TranscriptEntry>,
    pub assistant_buffer: String,
    pub busy: bool,
    pub busy_since: Instant,
    pub current_tool: Option<String>,
    pub modal: Option<Modal>,
    pub scroll_offset: Option<u16>,
    pub model: String,
    pub permission_mode: String,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub total_cache_read_tokens: u64,
    pub total_cache_creation_tokens: u64,
    /// Last turn's fresh input tokens (non-cached).
    pub last_input_tokens: u64,
    /// Last turn's output tokens.
    pub last_output_tokens: u64,
    /// Last turn's cache-read tokens.
    pub last_cache_read_tokens: u64,
    /// Context window size (tokens used in the last API call).
    pub last_context_tokens: u64,
}

impl AppState {
    pub fn new(model: String, permission_mode: String) -> Self {
        Self {
            input: String::new(),
            cursor_pos: 0,
            history: Vec::new(),
            history_index: None,
            transcript: Vec::new(),
            assistant_buffer: String::new(),
            busy: false,
            busy_since: Instant::now(),
            current_tool: None,
            modal: None,
            scroll_offset: None,
            model,
            permission_mode,
            total_input_tokens: 0,
            total_output_tokens: 0,
            total_cache_read_tokens: 0,
            total_cache_creation_tokens: 0,
            last_input_tokens: 0,
            last_output_tokens: 0,
            last_cache_read_tokens: 0,
            last_context_tokens: 0,
        }
    }
}

/// Run the interactive TUI.
pub async fn run_tui(
    mut engine: QueryEngine,
    hook_executor: Arc<dyn HookExecutorTrait>,
    model: String,
    permission_mode: String,
) -> Result<(), Box<dyn std::error::Error>> {
    // Set up terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    stdout.execute(EnterAlternateScreen)?;
    stdout.execute(crossterm::event::EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut state = AppState::new(model, permission_mode);

    // Fire SessionStart
    hook_executor
        .execute(HookEvent::SessionStart, serde_json::json!({}))
        .await;

    // Initial render
    terminal.draw(|f| render::render(f, &state))?;

    // Event loop
    let result = event_loop(&mut terminal, &mut state, &mut engine, &hook_executor).await;

    // Fire SessionEnd
    hook_executor
        .execute(
            HookEvent::SessionEnd,
            serde_json::json!({"reason": "tui_exit"}),
        )
        .await;

    // Restore terminal
    disable_raw_mode()?;
    terminal
        .backend_mut()
        .execute(crossterm::event::DisableMouseCapture)?;
    terminal
        .backend_mut()
        .execute(LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    result
}

async fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    state: &mut AppState,
    engine: &mut QueryEngine,
    hook_executor: &Arc<dyn HookExecutorTrait>,
) -> Result<(), Box<dyn std::error::Error>> {
    loop {
        // Poll for events with a short timeout for spinner animation
        let timeout = if state.busy {
            Duration::from_millis(80) // spinner frame rate
        } else {
            Duration::from_millis(250) // idle refresh
        };

        if event::poll(timeout)? {
            let ev = event::read()?;

            // Handle mouse scroll
            if let Event::Mouse(mouse) = &ev {
                match mouse.kind {
                    crossterm::event::MouseEventKind::ScrollUp => {
                        // Scroll up = look at older messages
                        let back = state.scroll_offset.unwrap_or(0);
                        state.scroll_offset = Some(back + 3);
                        terminal.draw(|f| render::render(f, state))?;
                        continue;
                    }
                    crossterm::event::MouseEventKind::ScrollDown => {
                        // Scroll down = toward newer messages / bottom
                        match state.scroll_offset {
                            None => {} // already at bottom
                            Some(back) if back <= 3 => {
                                state.scroll_offset = None; // snap to bottom
                            }
                            Some(back) => {
                                state.scroll_offset = Some(back - 3);
                            }
                        }
                        terminal.draw(|f| render::render(f, state))?;
                        continue;
                    }
                    _ => continue,
                }
            }

            if let Event::Key(key) = ev {
                let action = handle_key(state, key);

                match action {
                    InputAction::None => {}
                    InputAction::Redraw => {
                        terminal.draw(|f| render::render(f, state))?;
                    }
                    InputAction::Quit => {
                        return Ok(());
                    }
                    InputAction::Submit(text) => {
                        // Handle slash commands
                        if text == "/exit" || text == "/quit" {
                            return Ok(());
                        }
                        if text == "/clear" {
                            state.transcript.clear();
                            state.assistant_buffer.clear();
                            engine.clear();
                            terminal.draw(|f| render::render(f, state))?;
                            continue;
                        }

                        // Add user message to transcript
                        state.transcript.push(TranscriptEntry::user(&text));
                        state.busy = true;
                        state.busy_since = Instant::now();
                        state.scroll_offset = None;

                        terminal.draw(|f| render::render(f, state))?;

                        // Submit to engine
                        match engine.submit_message(&text).await {
                            Ok(events) => {
                                process_events(state, &events);
                            }
                            Err(e) => {
                                state.transcript.push(TranscriptEntry::system(format!(
                                    "Error: {e}"
                                )));
                            }
                        }

                        state.busy = false;
                        state.current_tool = None;
                        state.scroll_offset = None;
                        terminal.draw(|f| render::render(f, state))?;
                    }
                    InputAction::PermissionResponse(allowed) => {
                        state.modal = None;
                        // TODO: wire permission response back to engine
                        if !allowed {
                            state
                                .transcript
                                .push(TranscriptEntry::system("Permission denied by user"));
                        }
                        terminal.draw(|f| render::render(f, state))?;
                    }
                    InputAction::QuestionResponse(answer) => {
                        state.modal = None;
                        state
                            .transcript
                            .push(TranscriptEntry::user(format!("[answer] {answer}")));
                        terminal.draw(|f| render::render(f, state))?;
                    }
                }
            }
        } else {
            // Timeout — redraw for spinner animation
            if state.busy {
                terminal.draw(|f| render::render(f, state))?;
            }
        }
    }
}

/// Process stream events from the engine into transcript entries.
fn process_events(
    state: &mut AppState,
    events: &[(StreamEvent, Option<oh_types::api::UsageSnapshot>)],
) {
    for (event, usage) in events {
        match event {
            StreamEvent::AssistantTextDelta(delta) => {
                state.assistant_buffer.push_str(&delta.text);
            }
            StreamEvent::AssistantTurnComplete(turn) => {
                // Flush buffer or use message text
                let text = if state.assistant_buffer.is_empty() {
                    turn.message.text()
                } else {
                    std::mem::take(&mut state.assistant_buffer)
                };
                if !text.is_empty() {
                    state.transcript.push(TranscriptEntry::assistant(text));
                }
            }
            StreamEvent::ToolExecutionStarted(started) => {
                state.current_tool = Some(started.tool_name.clone());
                state
                    .transcript
                    .push(TranscriptEntry::tool_start(
                        &started.tool_name,
                        started.tool_input.clone(),
                    ));
            }
            StreamEvent::ToolExecutionCompleted(completed) => {
                state.current_tool = None;
                state.transcript.push(TranscriptEntry::tool_result(
                    &completed.tool_name,
                    &completed.output,
                    completed.is_error,
                ));
            }
        }

        // Update token counts
        if let Some(u) = usage {
            state.total_input_tokens += u.input_tokens;
            state.total_output_tokens += u.output_tokens;
            state.total_cache_read_tokens += u.cache_read_input_tokens;
            state.total_cache_creation_tokens += u.cache_creation_input_tokens;
            // Last turn values (overwrite each turn)
            state.last_input_tokens = u.input_tokens;
            state.last_output_tokens = u.output_tokens;
            state.last_cache_read_tokens = u.cache_read_input_tokens;
            state.last_context_tokens = u.context_tokens();
        }
    }
}

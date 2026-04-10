//! Rendering logic — converts app state to ratatui widgets.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;

use super::app::{AppState, Modal};
use super::transcript::{TranscriptEntry, TranscriptRole};

const SPINNER_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const VERBS: &[&str] = &[
    "Thinking",
    "Processing",
    "Analyzing",
    "Reasoning",
    "Working",
    "Computing",
    "Evaluating",
    "Considering",
];

/// Render the full TUI frame.
pub fn render(f: &mut Frame, state: &AppState) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(5),     // conversation
            Constraint::Length(1),  // separator + status
            Constraint::Length(1),  // input / spinner
            Constraint::Length(1),  // hints
        ])
        .split(f.area());

    render_conversation(f, state, chunks[0]);
    render_status_bar(f, state, chunks[1]);
    render_input(f, state, chunks[2]);
    render_hints(f, state, chunks[3]);

    // Modal overlay (if active)
    if let Some(ref modal) = state.modal {
        render_modal(f, modal, f.area());
    }
}

/// Render the conversation transcript.
fn render_conversation(f: &mut Frame, state: &AppState, area: Rect) {
    let mut lines: Vec<Line> = Vec::new();

    if state.transcript.is_empty() && !state.busy {
        // Welcome banner
        lines.push(Line::from(""));
        lines.push(Line::from(vec![
            Span::styled(
                "  OpenHarness",
                Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" v0.1.0", Style::default().fg(Color::DarkGray)),
        ]));
        lines.push(Line::from(vec![Span::styled(
            "  An AI-powered coding assistant",
            Style::default().fg(Color::DarkGray),
        )]));
        lines.push(Line::from(""));
        lines.push(Line::from(vec![
            Span::styled("  /help", Style::default().fg(Color::Cyan)),
            Span::styled("  commands", Style::default().fg(Color::DarkGray)),
            Span::styled("  │  ", Style::default().fg(Color::DarkGray)),
            Span::styled("Ctrl+C", Style::default().fg(Color::Cyan)),
            Span::styled("  exit", Style::default().fg(Color::DarkGray)),
        ]));
        lines.push(Line::from(""));
    }

    for entry in &state.transcript {
        render_transcript_entry(&mut lines, entry);
    }

    // Streaming assistant buffer
    if !state.assistant_buffer.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(vec![
            Span::styled(
                "⏺ ",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(&state.assistant_buffer),
        ]));
    }

    // Add padding so the last message can be scrolled above the fold
    let padding = area.height.saturating_sub(2) as usize;
    for _ in 0..padding {
        lines.push(Line::from(""));
    }

    // Scroll: None = pinned to bottom, Some(n) = n lines scrolled back from bottom
    let content_height = lines.len() as u16;
    let view_height = area.height;
    let max_scroll = content_height.saturating_sub(view_height);

    let scroll = match state.scroll_offset {
        None => max_scroll,
        Some(back) => {
            let clamped = back.min(max_scroll);
            max_scroll.saturating_sub(clamped)
        }
    };

    let paragraph = Paragraph::new(lines)
        .scroll((scroll, 0))
        .wrap(Wrap { trim: false });

    f.render_widget(paragraph, area);
}

/// Render a single transcript entry into lines.
fn render_transcript_entry<'a>(lines: &mut Vec<Line<'a>>, entry: &'a TranscriptEntry) {
    match entry.role {
        TranscriptRole::User => {
            lines.push(Line::from(""));
            lines.push(Line::from(vec![
                Span::styled(
                    "> ",
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(&entry.text),
            ]));
        }
        TranscriptRole::Assistant => {
            lines.push(Line::from(""));
            // Split into paragraphs for readability
            for line in entry.text.lines() {
                lines.push(Line::from(vec![
                    Span::styled("  ", Style::default()),
                    Span::raw(line),
                ]));
            }
        }
        TranscriptRole::Tool => {
            lines.push(Line::from(vec![
                Span::styled(
                    "  ⏵ ",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    entry.tool_name.as_deref().unwrap_or("tool"),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!(" {}", entry.text),
                    Style::default().fg(Color::DarkGray),
                ),
            ]));
        }
        TranscriptRole::ToolResult => {
            let color = if entry.is_error {
                Color::Red
            } else {
                Color::DarkGray
            };
            let result_lines: Vec<&str> = entry.text.lines().collect();
            let max_lines = 12;
            for line in result_lines.iter().take(max_lines) {
                lines.push(Line::from(vec![Span::styled(
                    format!("    {line}"),
                    Style::default().fg(color),
                )]));
            }
            if result_lines.len() > max_lines {
                lines.push(Line::from(vec![Span::styled(
                    format!("    ... ({} more lines)", result_lines.len() - max_lines),
                    Style::default().fg(Color::DarkGray),
                )]));
            }
        }
        TranscriptRole::System => {
            lines.push(Line::from(vec![
                Span::styled(
                    "  ℹ ",
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::ITALIC),
                ),
                Span::styled(&entry.text, Style::default().fg(Color::Yellow)),
            ]));
        }
        TranscriptRole::Log => {
            lines.push(Line::from(vec![Span::styled(
                format!("  {}", entry.text),
                Style::default().fg(Color::DarkGray),
            )]));
        }
    }
}

/// Render the status bar.
fn render_status_bar(f: &mut Frame, state: &AppState, area: Rect) {
    let mode_style = match state.permission_mode.as_str() {
        "full_auto" | "Auto" => Style::default().fg(Color::Green),
        "plan" | "Plan Mode" => Style::default().fg(Color::Yellow),
        _ => Style::default().fg(Color::White),
    };

    // Format token counts with K suffix for readability
    let fmt_tokens = |n: u64| -> String {
        if n >= 1000 {
            format!("{:.1}k", n as f64 / 1000.0)
        } else {
            n.to_string()
        }
    };

    let mut parts: Vec<Span> = vec![
        Span::styled(
            format!(" {} ", state.model),
            Style::default().fg(Color::Cyan),
        ),
        Span::styled("│ ", Style::default().fg(Color::DarkGray)),
    ];

    // Context = last turn's full context (fresh + cached)
    if state.last_context_tokens > 0 {
        parts.push(Span::styled(
            format!("ctx {} ", fmt_tokens(state.last_context_tokens)),
            Style::default().fg(Color::DarkGray),
        ));
        parts.push(Span::styled("│ ", Style::default().fg(Color::DarkGray)));
    }

    // Show last turn's token breakdown (not cumulative — more useful)
    let fresh = fmt_tokens(state.last_input_tokens);
    let out = fmt_tokens(state.last_output_tokens);
    let cached = state.last_cache_read_tokens;

    if cached > 0 {
        // Show: "500↑ 228↓ +4.1k cached"
        parts.push(Span::styled(
            format!("{fresh}↑ {out}↓ "),
            Style::default().fg(Color::DarkGray),
        ));
        parts.push(Span::styled(
            format!("+{}⚡", fmt_tokens(cached)),
            Style::default().fg(Color::Green),
        ));
    } else {
        parts.push(Span::styled(
            format!("{fresh}↑ {out}↓"),
            Style::default().fg(Color::DarkGray),
        ));
    }

    parts.push(Span::styled(" │ ", Style::default().fg(Color::DarkGray)));
    parts.push(Span::styled(format!("{} ", state.permission_mode), mode_style));

    let status = Paragraph::new(Line::from(parts)).style(Style::default().bg(Color::Black));
    f.render_widget(status, area);
}

/// Render the input line or spinner.
fn render_input(f: &mut Frame, state: &AppState, area: Rect) {
    if state.busy {
        let elapsed_ms = state.busy_since.elapsed().as_millis() as usize;
        let frame_idx = (elapsed_ms / 80) % SPINNER_FRAMES.len();
        let verb_idx = (elapsed_ms / 3000) % VERBS.len();
        let tool_label = state
            .current_tool
            .as_deref()
            .map(|t| format!("Running {t}..."))
            .unwrap_or_else(|| format!("{}...", VERBS[verb_idx]));

        let spinner = Paragraph::new(Line::from(vec![
            Span::styled(
                format!(" {} ", SPINNER_FRAMES[frame_idx]),
                Style::default().fg(Color::Cyan),
            ),
            Span::styled(tool_label, Style::default().fg(Color::DarkGray)),
        ]));
        f.render_widget(spinner, area);
    } else {
        let cursor_pos = state.cursor_pos;
        let input = Paragraph::new(Line::from(vec![
            Span::styled(
                " > ",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(&state.input),
        ]));
        f.render_widget(input, area);

        // Place cursor
        f.set_cursor_position((area.x + 3 + cursor_pos as u16, area.y));
    }
}

/// Render keyboard hints footer.
fn render_hints(f: &mut Frame, state: &AppState, area: Rect) {
    let hints = if state.modal.is_some() {
        vec![
            Span::styled(" [y]", Style::default().fg(Color::Green)),
            Span::styled(" allow  ", Style::default().fg(Color::DarkGray)),
            Span::styled("[n]", Style::default().fg(Color::Red)),
            Span::styled(" deny", Style::default().fg(Color::DarkGray)),
        ]
    } else {
        vec![
            Span::styled(" enter", Style::default().fg(Color::Cyan)),
            Span::styled(" send  ", Style::default().fg(Color::DarkGray)),
            Span::styled("↑↓", Style::default().fg(Color::Cyan)),
            Span::styled(" history  ", Style::default().fg(Color::DarkGray)),
            Span::styled("ctrl+c", Style::default().fg(Color::Cyan)),
            Span::styled(" exit", Style::default().fg(Color::DarkGray)),
        ]
    };

    let footer = Paragraph::new(Line::from(hints));
    f.render_widget(footer, area);
}

/// Render a modal overlay.
fn render_modal(f: &mut Frame, modal: &Modal, area: Rect) {
    let modal_area = centered_rect(60, 5, area);

    f.render_widget(Clear, modal_area);

    match modal {
        Modal::Permission {
            tool_name, reason, ..
        } => {
            let block = Block::default()
                .title(format!(" Allow {tool_name}? "))
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Yellow));

            let reason_text = reason.as_deref().unwrap_or("This tool requires permission");
            let content = Paragraph::new(vec![
                Line::from(Span::raw(reason_text)),
                Line::from(""),
                Line::from(vec![
                    Span::styled("[y] Allow  ", Style::default().fg(Color::Green)),
                    Span::styled("[n] Deny", Style::default().fg(Color::Red)),
                ]),
            ])
            .block(block);

            f.render_widget(content, modal_area);
        }
        Modal::Question {
            question, input, ..
        } => {
            let block = Block::default()
                .title(" Question ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan));

            let content = Paragraph::new(vec![
                Line::from(Span::raw(question.as_str())),
                Line::from(format!("> {input}")),
            ])
            .block(block);

            f.render_widget(content, modal_area);
        }
    }
}

/// Create a centered rect.
fn centered_rect(percent_x: u16, height: u16, area: Rect) -> Rect {
    let width = (area.width * percent_x / 100).min(area.width);
    let x = (area.width - width) / 2;
    let y = (area.height.saturating_sub(height)) / 2;
    Rect::new(area.x + x, area.y + y, width, height)
}

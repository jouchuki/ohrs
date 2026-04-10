//! Input handling — keyboard events to state mutations.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::app::{AppState, InputAction, Modal};

/// Process a key event and return the action to take.
pub fn handle_key(state: &mut AppState, key: KeyEvent) -> InputAction {
    // Modal takes priority
    if let Some(ref mut modal) = state.modal {
        return handle_modal_key(modal, key);
    }

    // Busy state — only allow Ctrl+C
    if state.busy {
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return InputAction::Quit;
        }
        return InputAction::None;
    }

    match key.code {
        // Ctrl+C / Ctrl+D — exit
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => InputAction::Quit,
        KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            if state.input.is_empty() {
                InputAction::Quit
            } else {
                InputAction::None
            }
        }

        // Enter — submit
        KeyCode::Enter => {
            if state.input.is_empty() {
                return InputAction::None;
            }
            let text = state.input.clone();
            state.history.push(text.clone());
            state.history_index = None;
            state.input.clear();
            state.cursor_pos = 0;
            InputAction::Submit(text)
        }

        // Backspace
        KeyCode::Backspace => {
            if state.cursor_pos > 0 {
                state.cursor_pos -= 1;
                state.input.remove(state.cursor_pos);
            }
            InputAction::Redraw
        }

        // Delete
        KeyCode::Delete => {
            if state.cursor_pos < state.input.len() {
                state.input.remove(state.cursor_pos);
            }
            InputAction::Redraw
        }

        // Arrow keys — cursor movement
        KeyCode::Left => {
            if state.cursor_pos > 0 {
                state.cursor_pos -= 1;
            }
            InputAction::Redraw
        }
        KeyCode::Right => {
            if state.cursor_pos < state.input.len() {
                state.cursor_pos += 1;
            }
            InputAction::Redraw
        }

        // Home / End
        KeyCode::Home => {
            state.cursor_pos = 0;
            InputAction::Redraw
        }
        KeyCode::End => {
            state.cursor_pos = state.input.len();
            InputAction::Redraw
        }

        // Ctrl+A / Ctrl+E — home/end (emacs style)
        KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            state.cursor_pos = 0;
            InputAction::Redraw
        }
        KeyCode::Char('e') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            state.cursor_pos = state.input.len();
            InputAction::Redraw
        }

        // Ctrl+U — clear line
        KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            state.input.clear();
            state.cursor_pos = 0;
            InputAction::Redraw
        }

        // Ctrl+W — delete word backward
        KeyCode::Char('w') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            if state.cursor_pos > 0 {
                let before = &state.input[..state.cursor_pos];
                let new_pos = before
                    .rfind(|c: char| c.is_whitespace())
                    .map(|i| i + 1)
                    .unwrap_or(0);
                state.input.drain(new_pos..state.cursor_pos);
                state.cursor_pos = new_pos;
            }
            InputAction::Redraw
        }

        // Up arrow — history previous
        KeyCode::Up => {
            if state.history.is_empty() {
                return InputAction::None;
            }
            let idx = match state.history_index {
                None => state.history.len() - 1,
                Some(0) => return InputAction::None,
                Some(i) => i - 1,
            };
            state.history_index = Some(idx);
            state.input = state.history[idx].clone();
            state.cursor_pos = state.input.len();
            InputAction::Redraw
        }

        // Down arrow — history next
        KeyCode::Down => {
            match state.history_index {
                None => {}
                Some(i) if i + 1 >= state.history.len() => {
                    state.history_index = None;
                    state.input.clear();
                    state.cursor_pos = 0;
                }
                Some(i) => {
                    state.history_index = Some(i + 1);
                    state.input = state.history[i + 1].clone();
                    state.cursor_pos = state.input.len();
                }
            }
            InputAction::Redraw
        }

        // Page Up/Down — scroll transcript
        KeyCode::PageUp => {
            let back = state.scroll_offset.unwrap_or(0);
            state.scroll_offset = Some(back + 10);
            InputAction::Redraw
        }
        KeyCode::PageDown => {
            state.scroll_offset = None; // snap to bottom
            InputAction::Redraw
        }

        // Regular character input
        KeyCode::Char(c) => {
            state.input.insert(state.cursor_pos, c);
            state.cursor_pos += 1;
            InputAction::Redraw
        }

        // Tab — no-op for now
        KeyCode::Tab => InputAction::None,

        _ => InputAction::None,
    }
}

/// Handle keyboard input when a modal is active.
fn handle_modal_key(modal: &mut Modal, key: KeyEvent) -> InputAction {
    match modal {
        Modal::Permission { .. } => match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => InputAction::PermissionResponse(true),
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                InputAction::PermissionResponse(false)
            }
            _ => InputAction::None,
        },
        Modal::Question { ref mut input, .. } => match key.code {
            KeyCode::Enter => {
                let answer = input.clone();
                InputAction::QuestionResponse(answer)
            }
            KeyCode::Char(c) => {
                input.push(c);
                InputAction::Redraw
            }
            KeyCode::Backspace => {
                input.pop();
                InputAction::Redraw
            }
            KeyCode::Esc => InputAction::QuestionResponse(String::new()),
            _ => InputAction::None,
        },
    }
}

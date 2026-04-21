//! Keybindings — map string sequences to named actions.
//!
//! YAML format:
//! ```yaml
//! bindings:
//!   "ctrl+l": clear_transcript
//!   "ctrl+k": toggle_vim
//!   "f1":     show_help
//! ```

use std::collections::HashMap;
use std::path::Path;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde::Deserialize;
use tracing::warn;

/// A named action that a key binding can trigger.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyAction {
    ClearTranscript,
    ToggleVim,
    ShowHelp,
    Quit,
    Submit,
    ScrollUp,
    ScrollDown,
    HistoryPrev,
    HistoryNext,
    /// Catch-all for unknown / future actions supplied in YAML.
    Custom(String),
}

impl std::fmt::Display for KeyAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            KeyAction::Custom(s) => write!(f, "{s}"),
            other => write!(f, "{other:?}"),
        }
    }
}

/// A normalised key combo (modifier flags + code).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct KeyCombo {
    pub modifiers: KeyModifiers,
    pub code: KeyCode,
}

impl KeyCombo {
    /// Parse a human-readable key description such as `"ctrl+l"` or `"f1"`.
    pub fn from_str(s: &str) -> Result<Self, String> {
        let lower = s.to_lowercase();
        let parts: Vec<&str> = lower.split('+').collect();

        let mut modifiers = KeyModifiers::empty();
        let key_part = parts
            .last()
            .copied()
            .ok_or_else(|| format!("empty key spec: {s}"))?;

        for mod_part in &parts[..parts.len().saturating_sub(1)] {
            match *mod_part {
                "ctrl" => modifiers |= KeyModifiers::CONTROL,
                "alt" => modifiers |= KeyModifiers::ALT,
                "shift" => modifiers |= KeyModifiers::SHIFT,
                other => return Err(format!("unknown modifier '{other}' in '{s}'")),
            }
        }

        let code = match key_part {
            "enter" => KeyCode::Enter,
            "esc" | "escape" => KeyCode::Esc,
            "backspace" => KeyCode::Backspace,
            "delete" | "del" => KeyCode::Delete,
            "tab" => KeyCode::Tab,
            "up" => KeyCode::Up,
            "down" => KeyCode::Down,
            "left" => KeyCode::Left,
            "right" => KeyCode::Right,
            "home" => KeyCode::Home,
            "end" => KeyCode::End,
            "pageup" => KeyCode::PageUp,
            "pagedown" => KeyCode::PageDown,
            "f1" => KeyCode::F(1),
            "f2" => KeyCode::F(2),
            "f3" => KeyCode::F(3),
            "f4" => KeyCode::F(4),
            "f5" => KeyCode::F(5),
            "f6" => KeyCode::F(6),
            "f7" => KeyCode::F(7),
            "f8" => KeyCode::F(8),
            "f9" => KeyCode::F(9),
            "f10" => KeyCode::F(10),
            "f11" => KeyCode::F(11),
            "f12" => KeyCode::F(12),
            c if c.chars().count() == 1 => {
                KeyCode::Char(c.chars().next().unwrap())
            }
            other => return Err(format!("unrecognised key '{other}' in '{s}'")),
        };

        Ok(KeyCombo { modifiers, code })
    }
}

/// Deserialised shape of the YAML file.
#[derive(Deserialize)]
struct KeyMapFile {
    bindings: HashMap<String, String>,
}

fn action_from_str(s: &str) -> KeyAction {
    match s {
        "clear_transcript" => KeyAction::ClearTranscript,
        "toggle_vim" => KeyAction::ToggleVim,
        "show_help" => KeyAction::ShowHelp,
        "quit" => KeyAction::Quit,
        "submit" => KeyAction::Submit,
        "scroll_up" => KeyAction::ScrollUp,
        "scroll_down" => KeyAction::ScrollDown,
        "history_prev" => KeyAction::HistoryPrev,
        "history_next" => KeyAction::HistoryNext,
        other => KeyAction::Custom(other.to_string()),
    }
}

/// A map from `KeyCombo` to `KeyAction`.
#[derive(Debug, Clone, Default)]
pub struct KeyMap {
    bindings: HashMap<KeyCombo, KeyAction>,
}

impl KeyMap {
    /// Create an empty key map.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Parse from a YAML string.
    ///
    /// Invalid individual entries are skipped with a warning; the rest of the
    /// config is kept.
    pub fn from_yaml_str(s: &str) -> Result<Self, String> {
        let file: KeyMapFile =
            serde_yaml::from_str(s).map_err(|e| format!("YAML parse error: {e}"))?;

        let mut bindings = HashMap::new();
        for (key_str, action_str) in file.bindings {
            match KeyCombo::from_str(&key_str) {
                Ok(combo) => {
                    bindings.insert(combo, action_from_str(&action_str));
                }
                Err(e) => {
                    warn!("keybindings: skipping invalid entry '{}': {}", key_str, e);
                }
            }
        }

        Ok(Self { bindings })
    }

    /// Parse from a YAML file.
    ///
    /// Invalid individual entries are skipped with a warning; the rest of the
    /// config is kept.
    pub fn from_yaml_file(path: &Path) -> Result<Self, String> {
        let s = std::fs::read_to_string(path)
            .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
        Self::from_yaml_str(&s)
    }

    /// Resolve a crossterm `KeyEvent` to a `KeyAction`, or `None` if unbound.
    ///
    /// Normalises shifted letters: `Char('A')` with no Shift modifier is
    /// treated identically to `Char('a')` with `SHIFT`.
    pub fn resolve(&self, event: KeyEvent) -> Option<&KeyAction> {
        let (code, modifiers) = normalise_shift(event.code, event.modifiers);
        self.bindings.get(&KeyCombo { modifiers, code })
    }
}

/// Normalise shifted-letter variants so both platforms produce the same lookup
/// key.
///
/// - `Char('A')` with no SHIFT  →  `Char('a')` with SHIFT
/// - `Char('A')` with SHIFT     →  unchanged (already normalised)
/// - `Char('a')` with SHIFT     →  unchanged
/// - Anything else              →  unchanged
fn normalise_shift(code: KeyCode, modifiers: KeyModifiers) -> (KeyCode, KeyModifiers) {
    if let KeyCode::Char(c) = code {
        if c.is_ascii_uppercase() && !modifiers.contains(KeyModifiers::SHIFT) {
            // Terminal sent an uppercase char without the SHIFT modifier set.
            // Treat it as lowercase + SHIFT for consistent lookup.
            return (
                KeyCode::Char(c.to_ascii_lowercase()),
                modifiers | KeyModifiers::SHIFT,
            );
        }
    }
    (code, modifiers)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

    fn make_key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press,
            state: KeyEventState::empty(),
        }
    }

    // ── YAML loader tests ──────────────────────────────────────────────────

    #[test]
    fn yaml_loader_valid_entries() {
        let yaml = r#"
bindings:
  "ctrl+l": clear_transcript
  "ctrl+k": toggle_vim
  "f1": show_help
"#;
        let km = KeyMap::from_yaml_str(yaml).unwrap();

        let ctrl_l = make_key(KeyCode::Char('l'), KeyModifiers::CONTROL);
        assert_eq!(km.resolve(ctrl_l), Some(&KeyAction::ClearTranscript));

        let ctrl_k = make_key(KeyCode::Char('k'), KeyModifiers::CONTROL);
        assert_eq!(km.resolve(ctrl_k), Some(&KeyAction::ToggleVim));

        let f1 = make_key(KeyCode::F(1), KeyModifiers::empty());
        assert_eq!(km.resolve(f1), Some(&KeyAction::ShowHelp));
    }

    #[test]
    fn yaml_loader_bad_entry_skipped_rest_kept() {
        // "ctrl+badkey" is invalid; "f1" should still load.
        let yaml = r#"
bindings:
  "ctrl+badkey": show_help
  "f1": show_help
"#;
        let km = KeyMap::from_yaml_str(yaml).unwrap();

        // The bad entry was skipped; f1 is present.
        let f1 = make_key(KeyCode::F(1), KeyModifiers::empty());
        assert_eq!(km.resolve(f1), Some(&KeyAction::ShowHelp));

        // Exactly one binding survived.
        assert_eq!(km.bindings.len(), 1);
    }

    #[test]
    fn yaml_loader_all_bad_entries_returns_empty_map() {
        let yaml = r#"
bindings:
  "ctrl+badkey1": show_help
  "ctrl+badkey2": clear_transcript
"#;
        let km = KeyMap::from_yaml_str(yaml).unwrap();
        assert_eq!(km.bindings.len(), 0);
    }

    #[test]
    fn yaml_loader_custom_action() {
        let yaml = r#"
bindings:
  "f2": my_custom_action
"#;
        let km = KeyMap::from_yaml_str(yaml).unwrap();
        let f2 = make_key(KeyCode::F(2), KeyModifiers::empty());
        assert_eq!(
            km.resolve(f2),
            Some(&KeyAction::Custom("my_custom_action".to_string()))
        );
    }

    // ── Shifted-char resolution tests ─────────────────────────────────────

    #[test]
    fn shifted_char_uppercase_no_shift_mod_normalises() {
        // Platform variant A: terminal sends Char('A') without SHIFT modifier.
        let yaml = r#"
bindings:
  "shift+a": show_help
"#;
        let km = KeyMap::from_yaml_str(yaml).unwrap();

        // Form 1: Char('A') with no SHIFT modifier
        let ev_no_mod = make_key(KeyCode::Char('A'), KeyModifiers::empty());
        assert_eq!(
            km.resolve(ev_no_mod),
            Some(&KeyAction::ShowHelp),
            "Char('A') without SHIFT should resolve to the shift+a binding"
        );
    }

    #[test]
    fn shifted_char_lowercase_with_shift_mod_resolves() {
        // Platform variant B: terminal sends Char('a') with SHIFT modifier.
        let yaml = r#"
bindings:
  "shift+a": show_help
"#;
        let km = KeyMap::from_yaml_str(yaml).unwrap();

        // Form 2: Char('a') with SHIFT modifier (crossterm canonical form)
        let ev_with_shift = make_key(KeyCode::Char('a'), KeyModifiers::SHIFT);
        assert_eq!(
            km.resolve(ev_with_shift),
            Some(&KeyAction::ShowHelp),
            "Char('a') with SHIFT should resolve to the shift+a binding"
        );
    }

    #[test]
    fn uppercase_with_shift_mod_not_double_shifted() {
        // Char('A') + SHIFT already set — should not double-apply SHIFT.
        // It stays as Char('A') | SHIFT (no forced lowercase transformation).
        let (code, mods) = normalise_shift(KeyCode::Char('A'), KeyModifiers::SHIFT);
        assert_eq!(code, KeyCode::Char('A'));
        assert!(mods.contains(KeyModifiers::SHIFT));
    }

    #[test]
    fn lowercase_no_shift_unchanged() {
        let (code, mods) = normalise_shift(KeyCode::Char('a'), KeyModifiers::empty());
        assert_eq!(code, KeyCode::Char('a'));
        assert!(!mods.contains(KeyModifiers::SHIFT));
    }
}

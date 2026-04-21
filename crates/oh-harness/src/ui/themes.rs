//! Theme system — named colour palettes for the TUI.

use std::path::Path;
use ratatui::style::{Color, Style, Modifier};
use serde::Deserialize;

/// A complete colour palette for the TUI.
#[derive(Debug, Clone, Deserialize)]
pub struct ThemeColors {
    pub background: Option<Color>,
    pub foreground: Option<Color>,
    pub primary: Option<Color>,
    pub secondary: Option<Color>,
    pub accent: Option<Color>,
    pub warning: Option<Color>,
    pub error: Option<Color>,
    pub info: Option<Color>,
    pub border: Option<Color>,
    pub status_bg: Option<Color>,
    pub status_fg: Option<Color>,
    pub input_fg: Option<Color>,
}

impl Default for ThemeColors {
    fn default() -> Self {
        // Dark theme defaults
        Self {
            background: Some(Color::Black),
            foreground: Some(Color::White),
            primary: Some(Color::Green),
            secondary: Some(Color::Cyan),
            accent: Some(Color::Magenta),
            warning: Some(Color::Yellow),
            error: Some(Color::Red),
            info: Some(Color::Blue),
            border: Some(Color::DarkGray),
            status_bg: Some(Color::Black),
            status_fg: Some(Color::White),
            input_fg: Some(Color::Green),
        }
    }
}

/// Named theme with a colour palette.
#[derive(Debug, Clone, Deserialize)]
pub struct Theme {
    pub name: String,
    pub colors: ThemeColors,
}

/// Errors that can occur during theme loading.
#[derive(Debug, thiserror::Error)]
pub enum ThemeError {
    #[error("theme not found: {0}")]
    NotFound(String),
    #[error("parse error: {0}")]
    ParseError(String),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

impl Theme {
    /// Built-in dark theme.
    pub fn default_dark() -> Self {
        Self {
            name: "dark".to_string(),
            colors: ThemeColors::default(),
        }
    }

    /// Built-in light theme.
    pub fn default_light() -> Self {
        Self {
            name: "light".to_string(),
            colors: ThemeColors {
                background: Some(Color::White),
                foreground: Some(Color::Black),
                primary: Some(Color::Green),
                secondary: Some(Color::Blue),
                accent: Some(Color::Magenta),
                warning: Some(Color::Yellow),
                error: Some(Color::Red),
                info: Some(Color::Cyan),
                border: Some(Color::Gray),
                status_bg: Some(Color::Gray),
                status_fg: Some(Color::Black),
                input_fg: Some(Color::Green),
            },
        }
    }

    /// Load a built-in theme by name, returning `ThemeError::NotFound` for unknowns.
    pub fn load_by_name(name: &str) -> Result<Theme, ThemeError> {
        match name {
            "dark" | "default" | "default_dark" => Ok(Self::default_dark()),
            "light" | "default_light" => Ok(Self::default_light()),
            other => Err(ThemeError::NotFound(other.to_string())),
        }
    }

    /// Load a theme from a YAML string.
    pub fn from_yaml_str(s: &str) -> Result<Theme, ThemeError> {
        serde_yaml::from_str(s).map_err(|e| ThemeError::ParseError(e.to_string()))
    }

    /// Load a theme from a YAML file.
    pub fn from_yaml_file(path: &Path) -> Result<Theme, ThemeError> {
        let s = std::fs::read_to_string(path)?;
        Self::from_yaml_str(&s)
    }

    // ---- Style helpers ----

    fn fg(&self) -> Color {
        self.colors.foreground.unwrap_or(Color::White)
    }
    fn bg(&self) -> Color {
        self.colors.background.unwrap_or(Color::Black)
    }
    fn primary(&self) -> Color {
        self.colors.primary.unwrap_or(Color::Green)
    }
    fn secondary(&self) -> Color {
        self.colors.secondary.unwrap_or(Color::Cyan)
    }
    fn warning(&self) -> Color {
        self.colors.warning.unwrap_or(Color::Yellow)
    }
    fn error(&self) -> Color {
        self.colors.error.unwrap_or(Color::Red)
    }
    fn info_color(&self) -> Color {
        self.colors.info.unwrap_or(Color::Blue)
    }
    fn border(&self) -> Color {
        self.colors.border.unwrap_or(Color::DarkGray)
    }

    /// Style for the overall background area.
    pub fn base_style(&self) -> Style {
        Style::default().fg(self.fg()).bg(self.bg())
    }

    /// Style for primary headings / labels.
    pub fn primary_style(&self) -> Style {
        Style::default()
            .fg(self.primary())
            .bg(self.bg())
            .add_modifier(Modifier::BOLD)
    }

    /// Style for secondary elements.
    pub fn secondary_style(&self) -> Style {
        Style::default().fg(self.secondary()).bg(self.bg())
    }

    /// Style for informational messages.
    pub fn info_style(&self) -> Style {
        Style::default().fg(self.info_color()).bg(self.bg())
    }

    /// Style for warning messages.
    pub fn warning_style(&self) -> Style {
        Style::default().fg(self.warning()).bg(self.bg())
    }

    /// Style for error messages.
    pub fn error_style(&self) -> Style {
        Style::default().fg(self.error()).bg(self.bg())
    }

    /// Style for borders.
    pub fn border_style(&self) -> Style {
        Style::default().fg(self.border()).bg(self.bg())
    }

    /// Style for the status bar.
    pub fn status_bar_style(&self) -> Style {
        let status_bg = self.colors.status_bg.unwrap_or(self.bg());
        let status_fg = self.colors.status_fg.unwrap_or(self.fg());
        Style::default().fg(status_fg).bg(status_bg)
    }

    /// Style for the text input area.
    pub fn input_style(&self) -> Style {
        let input_fg = self.colors.input_fg.unwrap_or(self.primary());
        Style::default().fg(input_fg).bg(self.bg())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_by_name_dark_ok() {
        let t = Theme::load_by_name("dark").unwrap();
        assert_eq!(t.name, "dark");
    }

    #[test]
    fn load_by_name_light_ok() {
        let t = Theme::load_by_name("light").unwrap();
        assert_eq!(t.name, "light");
    }

    #[test]
    fn load_by_name_unknown_returns_not_found() {
        let err = Theme::load_by_name("neon_banana").unwrap_err();
        assert!(matches!(err, ThemeError::NotFound(_)));
        assert!(err.to_string().contains("neon_banana"));
    }

    #[test]
    fn foreground_applied_in_base_style() {
        let t = Theme::default_dark();
        let s = t.base_style();
        assert_eq!(s.fg, Some(t.fg()));
    }

    #[test]
    fn foreground_applied_in_status_bar_style() {
        let t = Theme::default_dark();
        let s = t.status_bar_style();
        // fg must be set (not None)
        assert!(s.fg.is_some());
    }

    #[test]
    fn info_style_uses_theme_bg() {
        let t = Theme::default_dark();
        let s = t.info_style();
        assert_eq!(s.bg, Some(t.bg()));
    }

    #[test]
    fn warning_style_uses_theme_bg() {
        let t = Theme::default_dark();
        let s = t.warning_style();
        assert_eq!(s.bg, Some(t.bg()));
    }
}

//! Provider enum and helpers.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

/// All supported AI/API providers.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    Anthropic,
    OpenAi,
    Codex,
    GitHub,
    Moonshot,
    DashScope,
    MiniMax,
    Gemini,
    Bedrock,
    Vertex,
    Custom(String),
}

impl Provider {
    /// Canonical string key used for storage lookups.
    pub fn as_key(&self) -> String {
        match self {
            Provider::Anthropic => "anthropic".into(),
            Provider::OpenAi => "openai".into(),
            Provider::Codex => "codex".into(),
            Provider::GitHub => "github".into(),
            Provider::Moonshot => "moonshot".into(),
            Provider::DashScope => "dashscope".into(),
            Provider::MiniMax => "minimax".into(),
            Provider::Gemini => "gemini".into(),
            Provider::Bedrock => "bedrock".into(),
            Provider::Vertex => "vertex".into(),
            Provider::Custom(s) => s.clone(),
        }
    }
}

impl fmt::Display for Provider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_key())
    }
}

impl FromStr for Provider {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s.to_lowercase().as_str() {
            "anthropic" | "claude" => Provider::Anthropic,
            "openai" | "open_ai" => Provider::OpenAi,
            "codex" | "openai_codex" => Provider::Codex,
            "github" | "copilot" => Provider::GitHub,
            "moonshot" => Provider::Moonshot,
            "dashscope" | "dash_scope" => Provider::DashScope,
            "minimax" | "mini_max" => Provider::MiniMax,
            "gemini" | "google" => Provider::Gemini,
            "bedrock" | "aws" => Provider::Bedrock,
            "vertex" | "gcp" => Provider::Vertex,
            other => Provider::Custom(other.to_string()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_openai() {
        let p: Provider = "openai".parse().unwrap();
        assert_eq!(p, Provider::OpenAi);
    }

    #[test]
    fn parse_anthropic_alias() {
        let p: Provider = "claude".parse().unwrap();
        assert_eq!(p, Provider::Anthropic);
    }

    #[test]
    fn parse_custom() {
        let p: Provider = "mycompany".parse().unwrap();
        assert_eq!(p, Provider::Custom("mycompany".into()));
    }

    #[test]
    fn as_key_roundtrip() {
        for p in [
            Provider::Anthropic,
            Provider::OpenAi,
            Provider::Codex,
            Provider::GitHub,
            Provider::Moonshot,
            Provider::DashScope,
            Provider::MiniMax,
            Provider::Gemini,
            Provider::Bedrock,
            Provider::Vertex,
        ] {
            let key = p.as_key();
            let parsed: Provider = key.parse().unwrap();
            assert_eq!(p, parsed, "roundtrip failed for {key}");
        }
    }

    #[test]
    fn display_matches_key() {
        assert_eq!(Provider::OpenAi.to_string(), "openai");
    }
}

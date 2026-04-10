//! Provider detection (Anthropic, Bedrock, Vertex, Kimi).

/// Detected API provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    Anthropic,
    OpenAi,
    Bedrock,
    Vertex,
    Kimi,
    Unknown,
}

impl std::fmt::Display for Provider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Anthropic => write!(f, "anthropic"),
            Self::OpenAi => write!(f, "openai"),
            Self::Bedrock => write!(f, "bedrock"),
            Self::Vertex => write!(f, "vertex"),
            Self::Kimi => write!(f, "kimi"),
            Self::Unknown => write!(f, "unknown"),
        }
    }
}

/// Detect the provider from the base URL.
pub fn detect_provider(base_url: Option<&str>) -> Provider {
    match base_url {
        None => Provider::Anthropic,
        Some(url) => {
            if url.contains("openai") {
                Provider::OpenAi
            } else if url.contains("bedrock") {
                Provider::Bedrock
            } else if url.contains("vertex") || url.contains("googleapis") {
                Provider::Vertex
            } else if url.contains("kimi") || url.contains("moonshot") {
                Provider::Kimi
            } else if url.contains("anthropic") {
                Provider::Anthropic
            } else {
                Provider::Unknown
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_provider_none_defaults_to_anthropic() {
        assert_eq!(detect_provider(None), Provider::Anthropic);
    }

    #[test]
    fn test_detect_provider_anthropic_url() {
        assert_eq!(
            detect_provider(Some("https://api.anthropic.com")),
            Provider::Anthropic,
        );
    }

    #[test]
    fn test_detect_provider_bedrock_url() {
        assert_eq!(
            detect_provider(Some("https://bedrock.amazonaws.com/model/invoke")),
            Provider::Bedrock,
        );
    }

    #[test]
    fn test_detect_provider_vertex_googleapis_url() {
        assert_eq!(
            detect_provider(Some("https://us-central1-aiplatform.googleapis.com")),
            Provider::Vertex,
        );
    }

    #[test]
    fn test_detect_provider_kimi_moonshot_url() {
        assert_eq!(
            detect_provider(Some("https://api.moonshot.cn")),
            Provider::Kimi,
        );
    }

    #[test]
    fn test_detect_provider_openai_url() {
        assert_eq!(
            detect_provider(Some("https://api.openai.com")),
            Provider::OpenAi,
        );
    }

    #[test]
    fn test_provider_display_openai() {
        assert_eq!(Provider::OpenAi.to_string(), "openai");
    }

    #[test]
    fn test_detect_provider_unknown_url() {
        assert_eq!(
            detect_provider(Some("https://example.com")),
            Provider::Unknown,
        );
    }

    #[test]
    fn test_provider_display_anthropic() {
        assert_eq!(Provider::Anthropic.to_string(), "anthropic");
    }

    #[test]
    fn test_provider_display_bedrock() {
        assert_eq!(Provider::Bedrock.to_string(), "bedrock");
    }

    #[test]
    fn test_provider_display_vertex() {
        assert_eq!(Provider::Vertex.to_string(), "vertex");
    }

    #[test]
    fn test_provider_display_kimi() {
        assert_eq!(Provider::Kimi.to_string(), "kimi");
    }

    #[test]
    fn test_provider_display_unknown() {
        assert_eq!(Provider::Unknown.to_string(), "unknown");
    }
}

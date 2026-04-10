//! Fetch web pages tool.

use async_trait::async_trait;
use oh_types::tools::{ToolExecutionContext, ToolResult};
use reqwest::Client;
use std::time::Duration;

pub struct WebFetchTool;

const DEFAULT_MAX_CHARS: u64 = 12_000;
const MAX_MAX_CHARS: u64 = 50_000;

#[async_trait]
impl crate::traits::Tool for WebFetchTool {
    fn name(&self) -> &str {
        "WebFetch"
    }

    fn description(&self) -> &str {
        "Fetch one web page and return compact readable text."
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "HTTP or HTTPS URL to fetch"
                },
                "max_chars": {
                    "type": "integer",
                    "description": "Maximum characters to return (default 12000, max 50000)",
                    "default": 12000
                }
            },
            "required": ["url"]
        })
    }

    fn is_read_only(&self, _arguments: &serde_json::Value) -> bool {
        true
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        _context: &ToolExecutionContext,
    ) -> ToolResult {
        let url = match arguments.get("url").and_then(|v| v.as_str()) {
            Some(u) => u.to_string(),
            None => return ToolResult::error("Missing required parameter: url"),
        };

        let max_chars = arguments
            .get("max_chars")
            .and_then(|v| v.as_u64())
            .unwrap_or(DEFAULT_MAX_CHARS)
            .min(MAX_MAX_CHARS) as usize;

        let client = match Client::builder()
            .timeout(Duration::from_secs(20))
            .user_agent("OpenHarness/0.1")
            .redirect(reqwest::redirect::Policy::limited(10))
            .build()
        {
            Ok(c) => c,
            Err(e) => return ToolResult::error(format!("Failed to create HTTP client: {e}")),
        };

        let response = match client.get(&url).send().await {
            Ok(r) => r,
            Err(e) => return ToolResult::error(format!("web_fetch failed: {e}")),
        };

        let status = response.status().as_u16();
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let final_url = response.url().to_string();

        if !response.status().is_success() {
            return ToolResult::error(format!(
                "HTTP error: status {status} for URL {url}"
            ));
        }

        let body_text = match response.text().await {
            Ok(t) => t,
            Err(e) => return ToolResult::error(format!("Failed to read response body: {e}")),
        };

        let mut body = if content_type.contains("html") {
            html_to_text(&body_text)
        } else {
            body_text
        };

        body = body.trim().to_string();

        if body.len() > max_chars {
            // Find the nearest valid UTF-8 char boundary at or before max_chars
            let mut end = max_chars;
            while end > 0 && !body.is_char_boundary(end) {
                end -= 1;
            }
            body.truncate(end);
            body = body.trim_end().to_string();
            body.push_str("\n...[truncated]");
        }

        let ct_display = if content_type.is_empty() {
            "(unknown)".to_string()
        } else {
            content_type
        };

        ToolResult::success(format!(
            "URL: {final_url}\nStatus: {status}\nContent-Type: {ct_display}\n\n{body}"
        ))
    }
}

/// Strip HTML tags and decode common entities to produce plain text.
pub fn html_to_text(html: &str) -> String {
    use std::sync::LazyLock;

    static RE_SCRIPT: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"(?is)<script[^>]*>.*?</script>").unwrap());
    static RE_STYLE: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"(?is)<style[^>]*>.*?</style>").unwrap());
    static RE_TAGS: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"(?s)<[^>]+>").unwrap());
    static RE_SPACES: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"[ \t\r\f\v]+").unwrap());

    let text = RE_SCRIPT.replace_all(html, " ");
    let text = RE_STYLE.replace_all(&text, " ");
    let text = RE_TAGS.replace_all(&text, " ");

    let text = text
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'");

    let text = RE_SPACES.replace_all(&text, " ");
    text.replace(" \n", "\n").trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::traits::Tool;
    use oh_types::tools::ToolExecutionContext;
    use std::path::PathBuf;

    fn test_ctx() -> ToolExecutionContext {
        ToolExecutionContext::new(PathBuf::from("/tmp"))
    }

    #[test]
    fn test_html_to_text_strips_tags() {
        let html = "<html><body><h1>Hello</h1><p>World</p></body></html>";
        let result = html_to_text(html);
        assert!(result.contains("Hello"));
        assert!(result.contains("World"));
        assert!(!result.contains("<h1>"));
        assert!(!result.contains("<p>"));
    }

    #[test]
    fn test_html_to_text_removes_script_and_style() {
        let html = r#"<html><head><style>body { color: red; }</style></head>
            <body><script>alert('hi')</script><p>Content</p></body></html>"#;
        let result = html_to_text(html);
        assert!(result.contains("Content"));
        assert!(!result.contains("alert"));
        assert!(!result.contains("color: red"));
    }

    #[test]
    fn test_html_to_text_decodes_entities() {
        let html = "<p>A &amp; B &lt; C &gt; D &quot;E&quot; &#39;F&#39;</p>";
        let result = html_to_text(html);
        assert!(result.contains("A & B < C > D \"E\" 'F'"));
    }

    #[test]
    fn test_html_to_text_collapses_whitespace() {
        let html = "<p>Hello    World</p>";
        let result = html_to_text(html);
        // Should not have multiple spaces in a row
        assert!(!result.contains("    "));
    }

    #[tokio::test]
    async fn test_missing_url() {
        let tool = WebFetchTool;
        let args = serde_json::json!({});
        let ctx = test_ctx();
        let result = tool.execute(args, &ctx).await;
        assert!(result.is_error);
        assert!(result.output.contains("Missing required parameter"));
    }

    #[tokio::test]
    async fn test_invalid_url() {
        let tool = WebFetchTool;
        let args = serde_json::json!({"url": "not-a-valid-url"});
        let ctx = test_ctx();
        let result = tool.execute(args, &ctx).await;
        assert!(result.is_error);
    }

    #[test]
    fn test_is_read_only() {
        let tool = WebFetchTool;
        assert!(crate::traits::Tool::is_read_only(&tool, &serde_json::json!({})));
    }
}

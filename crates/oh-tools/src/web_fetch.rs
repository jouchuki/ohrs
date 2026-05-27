//! Fetch web pages tool.

use async_trait::async_trait;
use futures_util::StreamExt;
use oh_types::tools::{ToolExecutionContext, ToolResult};
use reqwest::Client;
use std::net::IpAddr;
use std::time::Duration;
use tracing::warn;
use url::Url;

pub struct WebFetchTool;

const DEFAULT_MAX_CHARS: u64 = 12_000;
const MAX_MAX_CHARS: u64 = 50_000;
/// Hard cap on bytes streamed from the response body (TOOL-8). Generous enough
/// to satisfy `MAX_MAX_CHARS` of multi-byte text plus HTML overhead.
const MAX_BODY_BYTES: usize = 4 * 1024 * 1024;
/// Maximum redirects we follow (we resolve+validate the host on each hop).
const MAX_REDIRECTS: usize = 10;

/// Outcome of validating a single URL hop against the SSRF policy.
#[derive(Debug, PartialEq, Eq)]
enum UrlPolicy {
    Allowed,
    Rejected(String),
}

/// Returns true for IPs that must never be reached from a fetch (loopback,
/// private RFC1918/ULA, link-local incl. the cloud metadata 169.254.169.254,
/// unspecified, and broadcast/multicast) (TOOL-8).
fn is_blocked_ip(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_broadcast()
                || v4.is_multicast()
                || v4.is_unspecified()
                // Carrier-grade NAT 100.64.0.0/10.
                || (v4.octets()[0] == 100 && (v4.octets()[1] & 0xc0) == 64)
        }
        IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6.is_multicast()
                || v6.is_unspecified()
                // Unique-local fc00::/7.
                || (v6.segments()[0] & 0xfe00) == 0xfc00
                // Link-local fe80::/10.
                || (v6.segments()[0] & 0xffc0) == 0xfe80
                // IPv4-mapped: re-check the embedded v4 address.
                || v6.to_ipv4_mapped().map(|m| is_blocked_ip(&IpAddr::V4(m))).unwrap_or(false)
        }
    }
}

/// Validate a URL's scheme and resolved host IPs against the SSRF policy.
/// Resolves the host (DNS) and rejects if ANY resolved address is blocked.
async fn validate_url(raw: &str) -> UrlPolicy {
    let parsed = match Url::parse(raw) {
        Ok(u) => u,
        Err(e) => return UrlPolicy::Rejected(format!("invalid URL: {e}")),
    };

    let scheme = parsed.scheme();
    if scheme != "http" && scheme != "https" {
        return UrlPolicy::Rejected(format!("scheme '{scheme}' not allowed (http/https only)"));
    }

    let host = match parsed.host_str() {
        Some(h) => h,
        None => return UrlPolicy::Rejected("URL has no host".to_string()),
    };

    // If the host is a literal IP, check it directly.
    if let Ok(ip) = host.parse::<IpAddr>() {
        return if is_blocked_ip(&ip) {
            UrlPolicy::Rejected(format!("host IP {ip} is private/loopback/link-local"))
        } else {
            UrlPolicy::Allowed
        };
    }

    // Otherwise resolve via DNS and reject if any address is blocked.
    let port = parsed.port_or_known_default().unwrap_or(80);
    let addrs = match tokio::net::lookup_host((host, port)).await {
        Ok(it) => it.collect::<Vec<_>>(),
        Err(e) => return UrlPolicy::Rejected(format!("DNS resolution failed for {host}: {e}")),
    };

    if addrs.is_empty() {
        return UrlPolicy::Rejected(format!("no addresses resolved for {host}"));
    }

    for addr in addrs {
        if is_blocked_ip(&addr.ip()) {
            return UrlPolicy::Rejected(format!(
                "host {host} resolves to blocked address {}",
                addr.ip()
            ));
        }
    }

    UrlPolicy::Allowed
}

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

        // Disable reqwest's automatic redirects; we follow them manually so we
        // can re-validate the host on EVERY hop (TOOL-8).
        let client = match Client::builder()
            .timeout(Duration::from_secs(20))
            .user_agent("OpenHarness/0.1")
            .redirect(reqwest::redirect::Policy::none())
            .build()
        {
            Ok(c) => c,
            Err(e) => return ToolResult::error(format!("Failed to create HTTP client: {e}")),
        };

        let mut current_url = url.clone();
        let mut redirects = 0usize;

        let (response, final_url) = loop {
            match validate_url(&current_url).await {
                UrlPolicy::Allowed => {}
                UrlPolicy::Rejected(reason) => {
                    return ToolResult::error(format!("web_fetch blocked: {reason}"));
                }
            }

            let resp = match client.get(&current_url).send().await {
                Ok(r) => r,
                Err(e) => return ToolResult::error(format!("web_fetch failed: {e}")),
            };

            if resp.status().is_redirection() {
                let location = resp
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .and_then(|v| v.to_str().ok())
                    .map(|s| s.to_string());
                let next = match location {
                    Some(loc) => match Url::parse(&current_url)
                        .ok()
                        .and_then(|base| base.join(&loc).ok())
                    {
                        Some(u) => u.to_string(),
                        None => loc,
                    },
                    None => {
                        return ToolResult::error(
                            "web_fetch failed: redirect without Location header".to_string(),
                        )
                    }
                };
                redirects += 1;
                if redirects > MAX_REDIRECTS {
                    return ToolResult::error(format!(
                        "web_fetch failed: too many redirects (>{MAX_REDIRECTS})"
                    ));
                }
                current_url = next;
                continue;
            }

            let final_url = resp.url().to_string();
            break (resp, final_url);
        };

        let status = response.status().as_u16();
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();

        if !response.status().is_success() {
            return ToolResult::error(format!("HTTP error: status {status} for URL {url}"));
        }

        // Stream the body with a hard byte cap instead of buffering text() (TOOL-8).
        let body_text = match read_body_capped(response).await {
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

/// Stream a response body, stopping once [`MAX_BODY_BYTES`] have been read
/// (TOOL-8). Decodes the accumulated bytes lossily as UTF-8.
async fn read_body_capped(response: reqwest::Response) -> Result<String, String> {
    let mut stream = response.bytes_stream();
    let mut buf: Vec<u8> = Vec::new();

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| e.to_string())?;
        let remaining = MAX_BODY_BYTES.saturating_sub(buf.len());
        if chunk.len() >= remaining {
            buf.extend_from_slice(&chunk[..remaining]);
            warn!(
                cap = MAX_BODY_BYTES,
                "web_fetch body reached byte cap; truncating"
            );
            break;
        }
        buf.extend_from_slice(&chunk);
    }

    Ok(String::from_utf8_lossy(&buf).to_string())
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
        assert!(crate::traits::Tool::is_read_only(
            &tool,
            &serde_json::json!({})
        ));
    }

    #[test]
    fn test_blocked_ip_loopback_v4() {
        assert!(is_blocked_ip(&"127.0.0.1".parse().unwrap()));
    }

    #[test]
    fn test_blocked_ip_private_rfc1918() {
        assert!(is_blocked_ip(&"10.0.0.5".parse().unwrap()));
        assert!(is_blocked_ip(&"192.168.1.1".parse().unwrap()));
        assert!(is_blocked_ip(&"172.16.0.1".parse().unwrap()));
    }

    #[test]
    fn test_blocked_ip_metadata_link_local() {
        assert!(is_blocked_ip(&"169.254.169.254".parse().unwrap()));
    }

    #[test]
    fn test_blocked_ip_ipv6_loopback_and_ula() {
        assert!(is_blocked_ip(&"::1".parse().unwrap()));
        assert!(is_blocked_ip(&"fd00::1".parse().unwrap()));
        assert!(is_blocked_ip(&"fe80::1".parse().unwrap()));
    }

    #[test]
    fn test_public_ip_allowed() {
        assert!(!is_blocked_ip(&"8.8.8.8".parse().unwrap()));
        assert!(!is_blocked_ip(&"1.1.1.1".parse().unwrap()));
    }

    #[tokio::test]
    async fn test_validate_rejects_non_http_scheme() {
        let policy = validate_url("file:///etc/passwd").await;
        assert!(matches!(policy, UrlPolicy::Rejected(_)));
    }

    #[tokio::test]
    async fn test_validate_rejects_loopback_literal() {
        let policy = validate_url("http://127.0.0.1/").await;
        assert!(matches!(policy, UrlPolicy::Rejected(_)));
    }

    #[tokio::test]
    async fn test_validate_rejects_metadata_literal() {
        let policy = validate_url("http://169.254.169.254/latest/meta-data/").await;
        assert!(matches!(policy, UrlPolicy::Rejected(_)));
    }

    #[tokio::test]
    async fn test_fetch_rejects_loopback_url() {
        let tool = WebFetchTool;
        let result = tool
            .execute(serde_json::json!({"url": "http://127.0.0.1:1/"}), &test_ctx())
            .await;
        assert!(result.is_error);
        assert!(result.output.contains("blocked"));
    }
}

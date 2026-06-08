//! Search the web tool.

use async_trait::async_trait;
use oh_types::tools::{ToolExecutionContext, ToolResult};
use regex::Regex;
use reqwest::Client;
use std::time::Duration;

pub struct WebSearchTool;

const DEFAULT_MAX_RESULTS: u64 = 5;
const MAX_MAX_RESULTS: u64 = 10;

/// A realistic browser UA. The DuckDuckGo HTML endpoint serves an anti-bot
/// challenge page to non-browser UAs (and, increasingly, to datacenter IPs
/// regardless of UA — which is why the Brave API is preferred when configured).
const BROWSER_UA: &str =
    "Mozilla/5.0 (X11; Linux x86_64; rv:124.0) Gecko/20100101 Firefox/124.0";

#[async_trait]
impl crate::traits::Tool for WebSearchTool {
    fn name(&self) -> &str {
        "WebSearch"
    }

    fn description(&self) -> &str {
        "Search the web and return compact top results with titles, URLs, and snippets."
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Search query"
                },
                "max_results": {
                    "type": "integer",
                    "description": "Maximum number of results (default 5, max 10)",
                    "default": 5
                }
            },
            "required": ["query"]
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
        let query = match arguments.get("query").and_then(|v| v.as_str()) {
            Some(q) => q.to_string(),
            None => return ToolResult::error("Missing required parameter: query"),
        };

        let max_results = arguments
            .get("max_results")
            .and_then(|v| v.as_u64())
            .unwrap_or(DEFAULT_MAX_RESULTS)
            .min(MAX_MAX_RESULTS) as usize;

        let client = match Client::builder()
            .timeout(Duration::from_secs(20))
            .user_agent(BROWSER_UA)
            .redirect(reqwest::redirect::Policy::limited(10))
            .build()
        {
            Ok(c) => c,
            Err(e) => return ToolResult::error(format!("Failed to create HTTP client: {e}")),
        };

        // Prefer the Brave Search API when a key is configured. Datacenter IPs
        // get anti-bot challenged by the DuckDuckGo HTML endpoint regardless of
        // UA, so the DDG scrape is only a best-effort fallback (local/dev).
        match std::env::var("BRAVE_API_KEY") {
            Ok(key) if !key.trim().is_empty() => {
                brave_search(&client, &query, max_results, key.trim()).await
            }
            _ => ddg_search(&client, &query, max_results).await,
        }
    }
}

/// Render results in the compact "Search results for:" text format.
fn format_results(query: &str, results: &[SearchResult]) -> ToolResult {
    if results.is_empty() {
        return ToolResult::error("No search results found.");
    }
    let mut lines = vec![format!("Search results for: {query}")];
    for (i, result) in results.iter().enumerate() {
        lines.push(format!("{}. {}", i + 1, result.title));
        lines.push(format!("   URL: {}", result.url));
        if !result.snippet.is_empty() {
            lines.push(format!("   {}", result.snippet));
        }
    }
    ToolResult::success(lines.join("\n"))
}

/// Query the Brave Search API (`X-Subscription-Token` auth). Reliable from
/// datacenter IPs, unlike the DDG HTML scrape.
async fn brave_search(
    client: &Client,
    query: &str,
    max_results: usize,
    key: &str,
) -> ToolResult {
    let response = match client
        .get("https://api.search.brave.com/res/v1/web/search")
        .query(&[("q", query), ("count", &max_results.to_string())])
        .header("Accept", "application/json")
        .header("X-Subscription-Token", key)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return ToolResult::error(format!("Brave search request failed: {e}")),
    };
    if !response.status().is_success() {
        return ToolResult::error(format!(
            "Brave search HTTP error: status {}",
            response.status().as_u16()
        ));
    }
    let json: serde_json::Value = match response.json().await {
        Ok(j) => j,
        Err(e) => return ToolResult::error(format!("Failed to parse Brave response: {e}")),
    };
    format_results(query, &parse_brave_results(&json, max_results))
}

/// Scrape the DuckDuckGo HTML endpoint. Best-effort fallback only.
async fn ddg_search(client: &Client, query: &str, max_results: usize) -> ToolResult {
    let response = match client
        .post("https://html.duckduckgo.com/html/")
        .form(&[("q", query)])
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return ToolResult::error(format!("web_search failed: {e}")),
    };
    if !response.status().is_success() {
        return ToolResult::error(format!(
            "HTTP error: status {} for search query",
            response.status().as_u16()
        ));
    }
    let body = match response.text().await {
        Ok(t) => t,
        Err(e) => return ToolResult::error(format!("Failed to read response body: {e}")),
    };
    format_results(query, &parse_search_results(&body, max_results))
}

/// Parse the Brave Search API JSON response (`web.results[]`).
pub fn parse_brave_results(json: &serde_json::Value, limit: usize) -> Vec<SearchResult> {
    let mut results = Vec::new();
    let items = json
        .get("web")
        .and_then(|w| w.get("results"))
        .and_then(|r| r.as_array());
    if let Some(items) = items {
        for item in items {
            let title = clean_html(item.get("title").and_then(|v| v.as_str()).unwrap_or(""));
            let url = item
                .get("url")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let snippet =
                clean_html(item.get("description").and_then(|v| v.as_str()).unwrap_or(""));
            if !title.is_empty() && !url.is_empty() {
                results.push(SearchResult { title, url, snippet });
            }
            if results.len() >= limit {
                break;
            }
        }
    }
    results
}

#[derive(Debug, Clone, PartialEq)]
pub struct SearchResult {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

/// Parse DuckDuckGo HTML search results page.
pub fn parse_search_results(body: &str, limit: usize) -> Vec<SearchResult> {
    use std::sync::LazyLock;

    static SNIPPET_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
        r#"(?is)<(?:a|div|span)[^>]+class="[^"]*(?:result__snippet|result-snippet)[^"]*"[^>]*>(?P<snippet>.*?)</(?:a|div|span)>"#,
    ).unwrap()
    });
    static ANCHOR_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#"(?is)<a(?P<attrs>[^>]+)>(?P<title>.*?)</a>"#).unwrap());
    static CLASS_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#"(?i)class="(?P<class>[^"]+)""#).unwrap());
    static HREF_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#"(?i)href="(?P<href>[^"]+)""#).unwrap());

    let snippets: Vec<String> = SNIPPET_RE
        .captures_iter(body)
        .map(|cap| clean_html(&cap["snippet"]))
        .collect();

    let mut results = Vec::new();
    let mut snippet_idx = 0;

    for cap in ANCHOR_RE.captures_iter(body) {
        let attrs = &cap["attrs"];

        let class_names = match CLASS_RE.captures(attrs) {
            Some(c) => c["class"].to_string(),
            None => continue,
        };

        if !class_names.contains("result__a") && !class_names.contains("result-link") {
            continue;
        }

        let href = match HREF_RE.captures(attrs) {
            Some(h) => h["href"].to_string(),
            None => continue,
        };

        let title = clean_html(&cap["title"]);
        let url = normalize_result_url(&href);
        let snippet = if snippet_idx < snippets.len() {
            snippets[snippet_idx].clone()
        } else {
            String::new()
        };
        snippet_idx += 1;

        if !title.is_empty() && !url.is_empty() {
            results.push(SearchResult {
                title,
                url,
                snippet,
            });
        }

        if results.len() >= limit {
            break;
        }
    }

    results
}

/// Normalize DuckDuckGo redirect URLs to their target.
fn normalize_result_url(raw_url: &str) -> String {
    if let Ok(parsed) = reqwest::Url::parse(raw_url) {
        if parsed
            .host_str()
            .is_some_and(|h| h.ends_with("duckduckgo.com"))
            && parsed.path().starts_with("/l/")
        {
            for (key, value) in parsed.query_pairs() {
                if key == "uddg" {
                    return value.to_string();
                }
            }
        }
    }
    raw_url.to_string()
}

/// Strip HTML tags and decode entities from an HTML fragment.
pub fn clean_html(fragment: &str) -> String {
    use std::sync::LazyLock;

    static RE_TAGS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)<[^>]+>").unwrap());
    static RE_SPACES: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").unwrap());

    let text = RE_TAGS.replace_all(fragment, " ");
    let text = text
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'");
    RE_SPACES.replace_all(&text, " ").trim().to_string()
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
    fn test_clean_html() {
        let html = r#"<b>Hello</b> &amp; <i>World</i>"#;
        let result = clean_html(html);
        assert_eq!(result, "Hello & World");
    }

    #[test]
    fn test_normalize_result_url_passthrough() {
        let url = "https://example.com/page";
        assert_eq!(normalize_result_url(url), url);
    }

    #[test]
    fn test_normalize_result_url_duckduckgo_redirect() {
        let url = "https://duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2Fpage&rut=abc";
        let result = normalize_result_url(url);
        assert_eq!(result, "https://example.com/page");
    }

    #[test]
    fn test_parse_search_results_sample_html() {
        let html = r#"
        <div class="results">
            <div class="result">
                <a class="result__a" href="https://example.com/1">
                    <b>Result One</b>
                </a>
                <span class="result__snippet">This is the first snippet.</span>
            </div>
            <div class="result">
                <a class="result__a" href="https://example.com/2">
                    Result Two
                </a>
                <span class="result__snippet">This is the second snippet.</span>
            </div>
            <div class="result">
                <a class="result__a" href="https://example.com/3">
                    Result Three
                </a>
                <span class="result__snippet">This is the third snippet.</span>
            </div>
        </div>
        "#;
        let results = parse_search_results(html, 5);
        assert_eq!(results.len(), 3);
        assert_eq!(results[0].title, "Result One");
        assert_eq!(results[0].url, "https://example.com/1");
        assert_eq!(results[0].snippet, "This is the first snippet.");
        assert_eq!(results[1].title, "Result Two");
        assert_eq!(results[1].url, "https://example.com/2");
    }

    #[test]
    fn test_parse_search_results_respects_limit() {
        let html = r#"
            <a class="result__a" href="https://example.com/1">One</a>
            <span class="result__snippet">S1</span>
            <a class="result__a" href="https://example.com/2">Two</a>
            <span class="result__snippet">S2</span>
            <a class="result__a" href="https://example.com/3">Three</a>
            <span class="result__snippet">S3</span>
        "#;
        let results = parse_search_results(html, 2);
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn test_parse_search_results_no_results() {
        let html = "<html><body>No results</body></html>";
        let results = parse_search_results(html, 5);
        assert!(results.is_empty());
    }

    #[test]
    fn test_parse_brave_results() {
        let json = serde_json::json!({
            "web": {"results": [
                {"title": "Jeugdzorg <strong>tekort</strong>", "url": "https://vng.nl/a",
                 "description": "De <strong>tekorten</strong> liepen op."},
                {"title": "Tweede bron", "url": "https://example.nl/b", "description": ""}
            ]}
        });
        let results = parse_brave_results(&json, 5);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].title, "Jeugdzorg tekort");   // HTML stripped
        assert_eq!(results[0].url, "https://vng.nl/a");
        assert_eq!(results[0].snippet, "De tekorten liepen op.");
        assert_eq!(results[1].snippet, "");
    }

    #[test]
    fn test_parse_brave_results_respects_limit_and_skips_incomplete() {
        let json = serde_json::json!({
            "web": {"results": [
                {"title": "A", "url": "https://a"},
                {"title": "", "url": "https://b"},      // no title -> skipped
                {"title": "C", "url": "https://c"},
            ]}
        });
        assert_eq!(parse_brave_results(&json, 1).len(), 1);
        // empty-title row skipped, so within limit=5 we get A and C
        assert_eq!(parse_brave_results(&json, 5).len(), 2);
    }

    #[test]
    fn test_parse_brave_results_empty_on_missing_web() {
        assert!(parse_brave_results(&serde_json::json!({}), 5).is_empty());
    }

    #[tokio::test]
    async fn test_missing_query() {
        let tool = WebSearchTool;
        let args = serde_json::json!({});
        let ctx = test_ctx();
        let result = tool.execute(args, &ctx).await;
        assert!(result.is_error);
        assert!(result.output.contains("Missing required parameter"));
    }

    #[test]
    fn test_is_read_only() {
        let tool = WebSearchTool;
        assert!(crate::traits::Tool::is_read_only(
            &tool,
            &serde_json::json!({})
        ));
    }
}

use async_trait::async_trait;
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, ACCEPT_LANGUAGE, USER_AGENT};
use reqwest::{Client, Response, StatusCode, Url};
use scraper::{Html, Selector};
use serde_json::json;
use tokio::time::{sleep, Duration};

use super::{Tool, ToolPreview};

pub struct WebSearchTool;

const HTTP_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_REQUEST_ATTEMPTS: usize = 3;

fn http_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        USER_AGENT,
        HeaderValue::from_static("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36"),
    );
    headers.insert(ACCEPT_LANGUAGE, HeaderValue::from_static("en-US,en;q=0.9"));
    headers.insert(
        ACCEPT,
        HeaderValue::from_static("text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8"),
    );
    headers
}

fn build_http_client() -> Result<Client, String> {
    Client::builder()
        .default_headers(http_headers())
        .timeout(HTTP_TIMEOUT)
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()
        .map_err(|error| format!("Failed to build HTTP client: {}", error))
}

fn validate_http_url(raw_url: &str) -> Result<Url, String> {
    let url =
        Url::parse(raw_url).map_err(|error| format!("Invalid URL '{}': {}", raw_url, error))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(format!(
            "Unsupported URL scheme '{}'; only http and https are allowed",
            url.scheme()
        ));
    }
    Ok(url)
}

fn is_retryable_error(error: &reqwest::Error) -> bool {
    error.is_timeout() || error.is_connect() || error.is_request()
}

fn is_retryable_status(status: StatusCode) -> bool {
    matches!(
        status,
        StatusCode::REQUEST_TIMEOUT
            | StatusCode::TOO_EARLY
            | StatusCode::TOO_MANY_REQUESTS
            | StatusCode::INTERNAL_SERVER_ERROR
            | StatusCode::BAD_GATEWAY
            | StatusCode::SERVICE_UNAVAILABLE
            | StatusCode::GATEWAY_TIMEOUT
    )
}

async fn send_with_retries(
    client: &Client,
    url: &Url,
    operation: &str,
) -> Result<Response, String> {
    let mut last_error = None;
    for attempt in 1..=MAX_REQUEST_ATTEMPTS {
        match client.get(url.clone()).send().await {
            Ok(response)
                if is_retryable_status(response.status()) && attempt < MAX_REQUEST_ATTEMPTS =>
            {
                sleep(Duration::from_millis(250 * attempt as u64)).await;
            }
            Ok(response) => return Ok(response),
            Err(error) if is_retryable_error(&error) && attempt < MAX_REQUEST_ATTEMPTS => {
                last_error = Some(error.to_string());
                sleep(Duration::from_millis(250 * attempt as u64)).await;
            }
            Err(error) => {
                return Err(format_request_error(operation, url, &error.to_string()));
            }
        }
    }

    Err(format_request_error(
        operation,
        url,
        last_error
            .as_deref()
            .unwrap_or("request failed after retries"),
    ))
}

fn format_request_error(operation: &str, url: &Url, detail: &str) -> String {
    let kind = if detail.to_ascii_lowercase().contains("timed out") {
        "timeout"
    } else if detail.to_ascii_lowercase().contains("dns") {
        "DNS resolution"
    } else if detail.to_ascii_lowercase().contains("certificate") {
        "TLS certificate"
    } else {
        "network"
    };
    format!(
        "{} failed for {} ({} error): {}",
        operation, url, kind, detail
    )
}

fn looks_like_error_page(body: &str) -> bool {
    let sample = body
        .chars()
        .take(12_000)
        .collect::<String>()
        .to_ascii_lowercase();
    [
        "oops, something went wrong",
        "access denied",
        "captcha",
        "cloudflare ray id",
        "enable javascript to continue",
    ]
    .iter()
    .any(|marker| sample.contains(marker))
}

#[async_trait]
impl Tool for WebSearchTool {
    fn name(&self) -> &'static str {
        "web_search"
    }

    fn description(&self) -> &'static str {
        "Searches the web using DuckDuckGo HTML Lite and returns top search result titles, URLs, and text snippets. No API key required."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "The search query string"
                }
            },
            "required": ["query"]
        })
    }

    fn generate_preview(&self, args: &serde_json::Value) -> ToolPreview {
        let query = args
            .get("query")
            .and_then(|v| v.as_str())
            .unwrap_or("<missing query>");
        ToolPreview {
            title: "Web Search".to_string(),
            details: vec![
                format!("Query: \"{}\"", query),
                "Source: DuckDuckGo HTML Lite (live zero-API-key scraper)".to_string(),
            ],
            reason: None,
            expected_effect: None,
            command: None,
            diff_hunks: Vec::new(),
            is_mutation: false,
        }
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let query = args
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "Missing required parameter 'query'".to_string())?;

        let client = build_http_client()?;

        let url = format!("https://html.duckduckgo.com/html/?q={}", urlencoding(query));
        let parsed_url = validate_http_url(&url)?;
        let resp = send_with_retries(&client, &parsed_url, "Web search request").await?;

        if !resp.status().is_success() {
            return Err(format!(
                "DuckDuckGo returned HTTP {} for {}{}",
                resp.status(),
                parsed_url,
                if is_retryable_status(resp.status()) {
                    " after retrying"
                } else {
                    ""
                }
            ));
        }

        let body = resp
            .text()
            .await
            .map_err(|e| format!("Failed to read search response body: {}", e))?;

        if looks_like_error_page(&body) {
            return Err(
                "DuckDuckGo returned an anti-bot or error page instead of search results. Try again or use a different web source.".to_string(),
            );
        }

        let document = Html::parse_document(&body);
        let result_selector =
            Selector::parse(".result").map_err(|e| format!("Selector error: {:?}", e))?;
        let title_selector =
            Selector::parse(".result__title a").map_err(|e| format!("Selector error: {:?}", e))?;
        let snippet_selector =
            Selector::parse(".result__snippet").map_err(|e| format!("Selector error: {:?}", e))?;

        let mut results = Vec::new();
        for el in document.select(&result_selector).take(15) {
            let title_el = el.select(&title_selector).next();
            let snippet_el = el.select(&snippet_selector).next();

            if let Some(t) = title_el {
                let title = t.text().collect::<Vec<_>>().join(" ").trim().to_string();
                let raw_url = t.value().attr("href").unwrap_or("").trim().to_string();
                let snippet = snippet_el
                    .map(|s| s.text().collect::<Vec<_>>().join(" ").trim().to_string())
                    .unwrap_or_default();

                // Clean duckduckgo uddg parameter if present
                let clean_url = extract_ddg_url(&raw_url);

                if !title.is_empty() && !clean_url.is_empty() {
                    results.push(format!("- **[{}]({})**\n  {}", title, clean_url, snippet));
                }
            }
        }

        if results.is_empty() {
            Ok(format!(
                "No results found on DuckDuckGo for query: \"{}\"",
                query
            ))
        } else {
            Ok(format!(
                "Search results for \"{}\":\n\n{}",
                query,
                results.join("\n\n")
            ))
        }
    }
}

pub struct WebFetchTool;

#[async_trait]
impl Tool for WebFetchTool {
    fn name(&self) -> &'static str {
        "web_fetch"
    }

    fn description(&self) -> &'static str {
        "Fetches the content of a target web page and extracts readable text paragraphs (stripping ads, scripts, styles)."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "The URL of the webpage to fetch"
                }
            },
            "required": ["url"]
        })
    }

    fn generate_preview(&self, args: &serde_json::Value) -> ToolPreview {
        let url = args
            .get("url")
            .and_then(|v| v.as_str())
            .unwrap_or("<missing url>");
        ToolPreview {
            title: "Web Fetch".to_string(),
            details: vec![
                format!("URL: {}", url),
                "Action: In-memory readability extraction (stripped scripts/styles)".to_string(),
            ],
            reason: None,
            expected_effect: None,
            command: None,
            diff_hunks: Vec::new(),
            is_mutation: false,
        }
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let url = args
            .get("url")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "Missing required parameter 'url'".to_string())?;

        let parsed_url = validate_http_url(url)?;
        let client = build_http_client()?;
        let resp = send_with_retries(&client, &parsed_url, "Web fetch request").await?;

        if !resp.status().is_success() {
            return Err(format!(
                "Web server returned HTTP {} for {}{}",
                resp.status(),
                parsed_url,
                if is_retryable_status(resp.status()) {
                    " after retrying"
                } else {
                    ""
                }
            ));
        }

        let body = resp
            .text()
            .await
            .map_err(|e| format!("Failed to read webpage content: {}", e))?;

        if looks_like_error_page(&body) {
            return Err(format!(
                "{} returned an error, anti-bot, or JavaScript challenge page; no reliable page content was available",
                parsed_url
            ));
        }

        let document = Html::parse_document(&body);
        let paragraph_selector = Selector::parse("p, h1, h2, h3, h4, article, li, pre, code")
            .map_err(|e| format!("Selector error: {:?}", e))?;

        let mut extracted = String::new();
        for el in document.select(&paragraph_selector) {
            let tag = el.value().name();
            let text = el.text().collect::<Vec<_>>().join(" ").trim().to_string();
            if text.len() > 10 {
                if tag.starts_with('h') {
                    extracted.push_str(&format!("\n### {}\n", text));
                } else if tag == "li" {
                    extracted.push_str(&format!("* {}\n", text));
                } else {
                    extracted.push_str(&format!("{}\n\n", text));
                }
            }

            if extracted.len() > 8000 {
                extracted
                    .push_str("\n\n[Content truncated at 8,000 characters to conserve context]");
                break;
            }
        }

        if extracted.trim().is_empty() {
            // Fallback plain body text
            let raw_text = document.root_element().text().collect::<Vec<_>>().join(" ");
            let trimmed = raw_text.chars().take(8000).collect::<String>();
            Ok(format!(
                "Extracted content from {}:\n\n{}",
                parsed_url, trimmed
            ))
        } else {
            Ok(format!(
                "Extracted content from {}:\n\n{}",
                parsed_url, extracted
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{is_retryable_status, looks_like_error_page, validate_http_url};
    use reqwest::StatusCode;

    #[test]
    fn only_http_urls_are_accepted() {
        assert!(validate_http_url("https://example.com/path").is_ok());
        assert!(validate_http_url("file:///tmp/example").is_err());
        assert!(validate_http_url("not a url").is_err());
    }

    #[test]
    fn transient_statuses_are_retryable() {
        assert!(is_retryable_status(StatusCode::SERVICE_UNAVAILABLE));
        assert!(is_retryable_status(StatusCode::TOO_MANY_REQUESTS));
        assert!(!is_retryable_status(StatusCode::NOT_FOUND));
    }

    #[test]
    fn challenge_pages_are_not_presented_as_content() {
        assert!(looks_like_error_page(
            "<title>Oops, something went wrong</title>"
        ));
        assert!(looks_like_error_page("Cloudflare Ray ID: abc"));
        assert!(!looks_like_error_page(
            "<article><p>Markets opened higher today.</p></article>"
        ));
    }
}

fn extract_ddg_url(raw: &str) -> String {
    if let Some(pos) = raw.find("uddg=") {
        let rest = &raw[pos + 5..];
        let end = rest.find('&').unwrap_or(rest.len());
        let encoded = &rest[..end];
        url_decode(encoded)
    } else if raw.starts_with("//duckduckgo.com/l/?uddg=") {
        let rest = &raw[25..];
        let end = rest.find('&').unwrap_or(rest.len());
        url_decode(&rest[..end])
    } else if raw.starts_with("http://") || raw.starts_with("https://") {
        raw.to_string()
    } else {
        format!("https://duckduckgo.com{}", raw)
    }
}

fn urlencoding(input: &str) -> String {
    let mut encoded = String::new();
    for byte in input.bytes() {
        match byte {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(byte as char);
            }
            b' ' => encoded.push('+'),
            _ => encoded.push_str(&format!("%{:02X}", byte)),
        }
    }
    encoded
}

fn url_decode(input: &str) -> String {
    let mut result = Vec::new();
    let bytes = input.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(hex_val) =
                u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..=i + 2]).unwrap_or(""), 16)
            {
                result.push(hex_val);
                i += 3;
                continue;
            }
        } else if bytes[i] == b'+' {
            result.push(b' ');
            i += 1;
            continue;
        }
        result.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&result).to_string()
}

use async_trait::async_trait;
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT_LANGUAGE, USER_AGENT};
use scraper::{Html, Selector};
use serde_json::json;

use super::{Tool, ToolPreview};

pub struct WebSearchTool;

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
            diff_hunks: Vec::new(),
            is_mutation: false,
        }
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let query = args
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "Missing required parameter 'query'".to_string())?;

        let mut headers = HeaderMap::new();
        headers.insert(
            USER_AGENT,
            HeaderValue::from_static("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36"),
        );
        headers.insert(ACCEPT_LANGUAGE, HeaderValue::from_static("en-US,en;q=0.9"));

        let client = reqwest::Client::builder()
            .default_headers(headers)
            .build()
            .map_err(|e| format!("Failed to build HTTP client: {}", e))?;

        let url = format!("https://html.duckduckgo.com/html/?q={}", urlencoding(query));
        let resp = client
            .get(&url)
            .send()
            .await
            .map_err(|e| format!("Search request failed: {}", e))?;

        if !resp.status().is_success() {
            return Err(format!("DuckDuckGo returned HTTP status {}", resp.status()));
        }

        let body = resp
            .text()
            .await
            .map_err(|e| format!("Failed to read search response body: {}", e))?;

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
            diff_hunks: Vec::new(),
            is_mutation: false,
        }
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let url = args
            .get("url")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "Missing required parameter 'url'".to_string())?;

        let mut headers = HeaderMap::new();
        headers.insert(
            USER_AGENT,
            HeaderValue::from_static("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36"),
        );

        let client = reqwest::Client::builder()
            .default_headers(headers)
            .timeout(std::time::Duration::from_secs(12))
            .build()
            .map_err(|e| format!("Failed to build HTTP client: {}", e))?;

        let resp = client
            .get(url)
            .send()
            .await
            .map_err(|e| format!("Fetch request failed: {}", e))?;

        if !resp.status().is_success() {
            return Err(format!("Web server returned HTTP {}", resp.status()));
        }

        let body = resp
            .text()
            .await
            .map_err(|e| format!("Failed to read webpage content: {}", e))?;

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
            Ok(format!("Extracted content from {}:\n\n{}", url, trimmed))
        } else {
            Ok(format!("Extracted content from {}:\n\n{}", url, extracted))
        }
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

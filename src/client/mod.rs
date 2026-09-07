mod openai;
pub mod sse;
pub mod types;

use crate::config::AppConfig;
use crate::events::StreamSignal;
use reqwest::{Client, RequestBuilder};
use std::collections::BTreeMap;
use tokio::sync::mpsc::UnboundedSender;
use tokio::time::{sleep, Duration};
use types::{GenerateContentRequest, ListModelsResponse, ModelInfo};

#[derive(Clone)]
pub struct AiClient {
    client: Client,
    api_key: String,
    base_url: String,
    provider: ProviderKind,
    headers: BTreeMap<String, String>,
    include_stream_usage: bool,
    protocol: ProviderProtocol,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderKind {
    Gemini,
    OpenAiCompatible,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderProtocol {
    Auto,
    ChatCompletions,
    Responses,
}

impl ProviderProtocol {
    pub fn parse(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().replace('_', "-").as_str() {
            "responses" | "response" | "openai-responses" => Self::Responses,
            "chat-completions" | "chat" | "completions" | "openai-compatible" => {
                Self::ChatCompletions
            }
            _ => Self::Auto,
        }
    }
}

impl ProviderKind {
    pub fn parse(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "gemini" | "google" | "google-gemini" => Self::Gemini,
            _ => Self::OpenAiCompatible,
        }
    }
}

impl AiClient {
    fn authorize(&self, mut request: RequestBuilder) -> RequestBuilder {
        for (name, value) in &self.headers {
            request = request.header(name, value);
        }
        if self.provider == ProviderKind::OpenAiCompatible && !self.api_key.is_empty() {
            request.bearer_auth(&self.api_key)
        } else {
            request
        }
    }

    pub fn from_config(api_key: String, config: &AppConfig) -> Self {
        let provider = config.active_provider_config();
        Self::with_provider(
            api_key,
            ProviderKind::parse(&provider.kind),
            provider.base_url.or_else(|| config.base_url.clone()),
            provider.headers,
            provider.stream_usage,
            ProviderProtocol::parse(&provider.protocol),
        )
    }

    pub fn with_provider(
        api_key: String,
        provider: ProviderKind,
        base_url: Option<String>,
        headers: BTreeMap<String, String>,
        include_stream_usage: bool,
        protocol: ProviderProtocol,
    ) -> Self {
        Self {
            client: Client::builder()
                .tcp_nodelay(true)
                .timeout(Duration::from_secs(120))
                .build()
                .unwrap_or_default(),
            base_url: base_url.unwrap_or_else(|| match provider {
                ProviderKind::Gemini => {
                    "https://generativelanguage.googleapis.com/v1beta".to_string()
                }
                ProviderKind::OpenAiCompatible => "https://api.openai.com/v1".to_string(),
            }),
            api_key,
            provider,
            headers,
            include_stream_usage,
            protocol,
        }
    }

    pub fn update_provider(
        &mut self,
        provider: ProviderKind,
        base_url: Option<String>,
        headers: BTreeMap<String, String>,
        include_stream_usage: bool,
        protocol: ProviderProtocol,
    ) {
        self.provider = provider;
        self.headers = headers;
        self.include_stream_usage = include_stream_usage;
        self.protocol = protocol;
        self.base_url = base_url.unwrap_or_else(|| match provider {
            ProviderKind::Gemini => "https://generativelanguage.googleapis.com/v1beta".to_string(),
            ProviderKind::OpenAiCompatible => "https://api.openai.com/v1".to_string(),
        });
    }

    pub fn update_api_key(&mut self, new_key: String) {
        self.api_key = new_key;
    }

    pub async fn stream_generate_content(
        &self,
        model: &str,
        fallback_models: &[String],
        max_retries: u32,
        request: &GenerateContentRequest,
        tx: UnboundedSender<StreamSignal>,
    ) {
        if self.provider == ProviderKind::OpenAiCompatible {
            self.stream_openai(model, fallback_models, max_retries, request, tx)
                .await;
            return;
        }
        let models = model_candidates(model, fallback_models);
        let mut last_error = String::new();
        'models: for (model_index, candidate) in models.iter().enumerate() {
            for attempt in 0..=max_retries {
                let url = format!(
                    "{}/models/{}:streamGenerateContent?alt=sse&key={}",
                    self.base_url,
                    clean_model(candidate),
                    self.api_key
                );
                match self
                    .authorize(
                        self.client
                            .post(&url)
                            .header("Content-Type", "application/json"),
                    )
                    .json(request)
                    .send()
                    .await
                {
                    Ok(response) if response.status().is_success() => {
                        if model_index > 0 {
                            let _ = tx.send(StreamSignal::Notice(format!(
                                "Using fallback model {}",
                                candidate
                            )));
                        }
                        sse::stream_sse_response(response, tx).await;
                        return;
                    }
                    Ok(response) => {
                        let status = response.status().as_u16();
                        let body = response.text().await.unwrap_or_default();
                        last_error = format_api_error(Some(status), &body);
                        // A retired/unavailable model can fall through immediately.
                        if status == 404 && model_index + 1 < models.len() {
                            let _ = tx.send(StreamSignal::Notice(format!(
                                "{} not found; trying {}",
                                candidate,
                                models[model_index + 1]
                            )));
                            continue 'models;
                        }
                        if !is_transient(status) {
                            let _ = tx.send(StreamSignal::Error(last_error));
                            return;
                        }
                    }
                    Err(e) => last_error = format!("Request failed: {}", e),
                }
                if attempt < max_retries {
                    let delay = backoff(attempt);
                    let _ = tx.send(StreamSignal::Notice(format!(
                        "Transient failure on {}. Retrying in {}s ({}/{})",
                        candidate,
                        delay.as_secs(),
                        attempt + 1,
                        max_retries
                    )));
                    sleep(delay).await;
                }
            }
            if model_index + 1 < models.len() {
                let _ = tx.send(StreamSignal::Notice(format!(
                    "{} unavailable; trying {}",
                    candidate,
                    models[model_index + 1]
                )));
            }
        }
        let _ = tx.send(StreamSignal::Error(last_error));
    }

    pub async fn generate_content(
        &self,
        model: &str,
        request: &GenerateContentRequest,
    ) -> Result<String, String> {
        if self.provider == ProviderKind::OpenAiCompatible {
            return self.generate_openai(model, request).await;
        }
        let clean_model = if model.starts_with("models/") {
            model.strip_prefix("models/").unwrap_or(model)
        } else {
            model
        };

        let url = format!(
            "{}/models/{}:generateContent?key={}",
            self.base_url, clean_model, self.api_key
        );

        let response = self
            .authorize(
                self.client
                    .post(&url)
                    .header("Content-Type", "application/json"),
            )
            .json(request)
            .send()
            .await
            .map_err(|e| format!("Request failed: {}", e))?;

        if !response.status().is_success() {
            let status = response.status().as_u16();
            let err_text = response.text().await.unwrap_or_default();
            return Err(format_api_error(Some(status), &err_text));
        }

        let resp: types::GenerateContentResponse = response
            .json()
            .await
            .map_err(|e| format!("Failed to parse response JSON: {}", e))?;

        if let Some(candidates) = resp.candidates {
            for cand in candidates {
                if let Some(content) = cand.content {
                    if let Some(parts) = content.parts {
                        let mut text_out = String::new();
                        for p in parts {
                            if let Some(t) = p.text {
                                text_out.push_str(&t);
                            }
                        }
                        if !text_out.is_empty() {
                            return Ok(text_out);
                        }
                    }
                }
            }
        }

        Err("No text output produced by model".to_string())
    }

    pub async fn generate_content_with_fallback(
        &self,
        model: &str,
        fallback_models: &[String],
        max_retries: u32,
        request: &GenerateContentRequest,
    ) -> Result<String, String> {
        let mut models = model_candidates(model, fallback_models);
        if self.provider == ProviderKind::OpenAiCompatible {
            models.retain(|candidate| !candidate.to_ascii_lowercase().starts_with("gemini"));
            if models.is_empty() {
                models.push(clean_model(model).to_string());
            }
        }
        let mut last_error = String::new();
        for candidate in models {
            for attempt in 0..=max_retries {
                match self.generate_content(&candidate, request).await {
                    Ok(text) => return Ok(text),
                    Err(error) => {
                        last_error = error;
                        let transient = last_error.contains("429")
                            || last_error.contains("408")
                            || last_error.contains("500")
                            || last_error.contains("502")
                            || last_error.contains("503")
                            || last_error.contains("504")
                            || last_error.contains("Request failed");
                        if !transient {
                            break;
                        }
                    }
                }
                if attempt < max_retries {
                    sleep(backoff(attempt)).await;
                }
            }
        }
        Err(last_error)
    }

    pub async fn list_models(&self) -> Result<Vec<ModelInfo>, String> {
        if self.provider == ProviderKind::OpenAiCompatible {
            return self.list_openai_models().await;
        }
        let url = format!("{}/models?key={}", self.base_url, self.api_key);
        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| format!("Failed to list models: {}", e))?;

        if !resp.status().is_success() {
            let err_text = resp.text().await.unwrap_or_default();
            return Err(format!("List models API error: {}", err_text));
        }

        let list_resp: ListModelsResponse = resp
            .json()
            .await
            .map_err(|e| format!("Failed to deserialize models list: {}", e))?;

        let mut models = Vec::new();
        if let Some(entries) = list_resp.models {
            for entry in entries {
                let is_generate = entry
                    .supported_generation_methods
                    .as_ref()
                    .map(|methods| methods.iter().any(|m| m == "generateContent"))
                    .unwrap_or(false);

                if !is_generate {
                    continue;
                }

                let id = entry.name.trim_start_matches("models/").to_string();
                let display_name = entry.display_name.unwrap_or_else(|| id.clone());
                let description = entry.description.unwrap_or_default();

                // Dynamic pricing estimates based on model family
                let (input_price, output_price) = get_model_pricing(&id);

                models.push(ModelInfo {
                    id,
                    display_name,
                    description,
                    input_price_per_m: input_price,
                    output_price_per_m: output_price,
                    input_token_limit: entry.input_token_limit,
                });
            }
        }

        Ok(models)
    }
}

impl AiClient {
    async fn stream_openai(
        &self,
        model: &str,
        fallback_models: &[String],
        max_retries: u32,
        request: &GenerateContentRequest,
        tx: UnboundedSender<StreamSignal>,
    ) {
        let mut models = model_candidates(model, fallback_models);
        models.retain(|candidate| !candidate.to_ascii_lowercase().starts_with("gemini"));
        if models.is_empty() {
            models.push(clean_model(model).to_string());
        }
        let mut last_error = String::new();
        'models: for (model_index, candidate) in models.iter().enumerate() {
            if self.is_zen() && is_zen_unsupported_model_id(candidate) {
                let error = format!("Zen model {} uses an unsupported protocol for this harness; choose a Chat Completions or Responses model.", candidate);
                let _ = tx.send(StreamSignal::Error(error));
                return;
            }
            for attempt in 0..=max_retries {
                let use_responses = self.uses_responses(candidate);
                let endpoint = if use_responses {
                    "responses"
                } else {
                    "chat/completions"
                };
                let url = format!("{}/{}", self.base_url.trim_end_matches('/'), endpoint);
                let payload = if use_responses {
                    openai::responses_request_payload(candidate, request, true)
                } else {
                    openai::request_payload(candidate, request, true, self.include_stream_usage)
                };
                let response = self
                    .authorize(self.client.post(&url).json(&payload))
                    .send()
                    .await;
                match response {
                    Ok(response) if response.status().is_success() => {
                        if model_index > 0 {
                            let _ = tx.send(StreamSignal::Notice(format!(
                                "Using fallback model {}",
                                candidate
                            )));
                        }
                        if use_responses {
                            openai::stream_responses_response(response, tx).await;
                        } else {
                            openai::stream_response(response, tx).await;
                        }
                        return;
                    }
                    Ok(response) => {
                        let status = response.status().as_u16();
                        let body = response.text().await.unwrap_or_default();
                        last_error = format_api_error(Some(status), &body);
                        if status == 404 && model_index + 1 < models.len() {
                            let _ = tx.send(StreamSignal::Notice(format!(
                                "{} not found; trying {}",
                                candidate,
                                models[model_index + 1]
                            )));
                            continue 'models;
                        }
                        if !is_transient(status) {
                            let _ = tx.send(StreamSignal::Error(last_error));
                            return;
                        }
                    }
                    Err(e) => last_error = format!("Request failed: {}", e),
                }
                if attempt < max_retries {
                    let delay = backoff(attempt);
                    let _ = tx.send(StreamSignal::Notice(format!(
                        "Transient failure on {}. Retrying in {}s ({}/{})",
                        candidate,
                        delay.as_secs(),
                        attempt + 1,
                        max_retries
                    )));
                    sleep(delay).await;
                }
            }
            if model_index + 1 < models.len() {
                let _ = tx.send(StreamSignal::Notice(format!(
                    "{} unavailable; trying {}",
                    candidate,
                    models[model_index + 1]
                )));
            }
        }
        let _ = tx.send(StreamSignal::Error(last_error));
    }

    async fn generate_openai(
        &self,
        model: &str,
        request: &GenerateContentRequest,
    ) -> Result<String, String> {
        if self.uses_responses(model) {
            let url = format!("{}/responses", self.base_url.trim_end_matches('/'));
            let response = self
                .authorize(
                    self.client
                        .post(&url)
                        .json(&openai::responses_request_payload(model, request, false)),
                )
                .send()
                .await
                .map_err(|e| format!("Request failed: {}", e))?;
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            if !status.is_success() {
                return Err(format_api_error(Some(status.as_u16()), &body));
            }
            let json: serde_json::Value = serde_json::from_str(&body)
                .map_err(|e| format!("Failed to parse response JSON: {}", e))?;
            return openai::responses_text(&json)
                .ok_or_else(|| "No text output produced by Responses API".to_string());
        }
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let response = self
            .authorize(
                self.client
                    .post(&url)
                    .json(&openai::request_payload(model, request, false, false)),
            )
            .send()
            .await
            .map_err(|e| format!("Request failed: {}", e))?;
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(format_api_error(Some(status.as_u16()), &body));
        }
        let json: serde_json::Value = serde_json::from_str(&body)
            .map_err(|e| format!("Failed to parse response JSON: {}", e))?;
        json.pointer("/choices/0/message/content")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| "No text output produced by model".to_string())
    }

    async fn list_openai_models(&self) -> Result<Vec<ModelInfo>, String> {
        let url = format!("{}/models", self.base_url.trim_end_matches('/'));
        let response = self
            .authorize(self.client.get(&url))
            .send()
            .await
            .map_err(|e| format!("Failed to list models: {}", e))?;
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(format_api_error(Some(status.as_u16()), &body));
        }
        let data: serde_json::Value = serde_json::from_str(&body)
            .map_err(|e| format!("Failed to deserialize models list: {}", e))?;
        Ok(data
            .get("data")
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
            .filter_map(|entry| {
                let id = entry.get("id").and_then(|v| v.as_str())?;
                if self.is_zen() && is_zen_unsupported_model_id(id) {
                    return None;
                }
                let description = entry
                    .get("description")
                    .and_then(|v| v.as_str())
                    .unwrap_or("OpenAI-compatible model")
                    .to_string();
                let input_price_per_m = openrouter_price_per_m(entry, "prompt");
                let output_price_per_m = openrouter_price_per_m(entry, "completion");
                let input_token_limit = entry.get("context_length").and_then(|v| v.as_u64());
                Some(ModelInfo {
                    id: id.to_string(),
                    display_name: entry
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or(id)
                        .to_string(),
                    description,
                    input_price_per_m,
                    output_price_per_m,
                    input_token_limit,
                })
            })
            .collect())
    }

    fn is_zen(&self) -> bool {
        self.base_url
            .to_ascii_lowercase()
            .contains("opencode.ai/zen/")
    }

    fn uses_responses(&self, model: &str) -> bool {
        match self.protocol {
            ProviderProtocol::Responses => true,
            ProviderProtocol::ChatCompletions => false,
            ProviderProtocol::Auto => self.is_zen() && is_zen_responses_model_id(model),
        }
    }
}

/// Zen publishes one model catalog for several wire protocols. These model
/// families are documented as Responses rather than Chat Completions.
pub fn is_zen_responses_model_id(model: &str) -> bool {
    let model = clean_model(model).to_ascii_lowercase();
    model.starts_with("gpt-") || model.starts_with("grok-") || model.starts_with("muse-spark-")
}

/// These Zen families use Anthropic Messages or Gemini-native endpoints. They
/// are hidden from the OpenAI-compatible picker until those adapters exist.
pub fn is_zen_unsupported_model_id(model: &str) -> bool {
    let model = clean_model(model).to_ascii_lowercase();
    model.starts_with("claude-") || model.starts_with("gemini-") || model.starts_with("qwen3.")
}

fn openrouter_price_per_m(entry: &serde_json::Value, key: &str) -> Option<f64> {
    entry
        .get("pricing")
        .and_then(|pricing| pricing.get(key))
        .and_then(|price| {
            price
                .as_str()
                .and_then(|value| value.parse::<f64>().ok())
                .or_else(|| price.as_f64())
        })
        .map(|price_per_token| price_per_token * 1_000_000.0)
}

fn clean_model(model: &str) -> &str {
    model.strip_prefix("models/").unwrap_or(model)
}

fn model_candidates(primary: &str, fallbacks: &[String]) -> Vec<String> {
    let mut result = vec![clean_model(primary).to_string()];
    for model in fallbacks {
        let model = clean_model(model).to_string();
        if !result.contains(&model) {
            result.push(model);
        }
    }
    result
}

fn is_transient(status: u16) -> bool {
    status == 408 || status == 429 || status >= 500
}

fn backoff(attempt: u32) -> Duration {
    // Capped exponential delay with small time-derived jitter; 1-60 seconds.
    let base_ms = 1_000u64.saturating_mul(1u64 << attempt.min(5));
    let jitter = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_millis() as u64 % 251)
        .unwrap_or(0);
    Duration::from_millis((base_ms + jitter).min(60_000))
}

#[cfg(test)]
mod tests {
    use super::{
        is_zen_responses_model_id, is_zen_unsupported_model_id, openrouter_price_per_m,
        ProviderKind, ProviderProtocol,
    };
    use serde_json::json;

    #[test]
    fn parses_provider_aliases() {
        assert_eq!(ProviderKind::parse("gemini"), ProviderKind::Gemini);
        assert_eq!(
            ProviderKind::parse("openai"),
            ProviderKind::OpenAiCompatible
        );
        assert_eq!(
            ProviderKind::parse("openai-compatible"),
            ProviderKind::OpenAiCompatible
        );
        assert_eq!(ProviderKind::parse("local"), ProviderKind::OpenAiCompatible);
        assert_eq!(
            ProviderProtocol::parse("responses"),
            ProviderProtocol::Responses
        );
        assert_eq!(
            ProviderProtocol::parse("chat-completions"),
            ProviderProtocol::ChatCompletions
        );
    }

    #[test]
    fn parses_openrouter_prices_per_million_tokens() {
        let entry = json!({"pricing": {"prompt": "0.00000015", "completion": "0.0000006"}});
        assert_eq!(openrouter_price_per_m(&entry, "prompt"), Some(0.15));
        assert_eq!(openrouter_price_per_m(&entry, "completion"), Some(0.6));
    }

    #[test]
    fn recognizes_zen_models_that_need_another_protocol() {
        assert!(is_zen_responses_model_id("muse-spark-1.3-contributor-free"));
        assert!(is_zen_responses_model_id("gpt-5.5"));
        assert!(is_zen_unsupported_model_id("claude-opus-5"));
        assert!(is_zen_unsupported_model_id("gemini-3.8-flash"));
        assert!(!is_zen_responses_model_id("mimo-v2.5-free"));
    }

    #[test]
    fn uses_standard_json_pointer_for_chat_completion_text() {
        let response = json!({
            "choices": [{"message": {"content": "hello"}}]
        });
        assert_eq!(
            response
                .pointer("/choices/0/message/content")
                .and_then(|value| value.as_str()),
            Some("hello")
        );
        assert!(response.pointer("choices.0.message.content").is_none());
    }
}

pub fn get_model_pricing(model_id: &str) -> (Option<f64>, Option<f64>) {
    let lower = model_id.to_lowercase();
    if lower.contains("3.8-flash") || lower.contains("flash-lite") {
        // e.g., $0.075 / 1M input, $0.30 / 1M output
        (Some(0.075), Some(0.30))
    } else if lower.contains("2.5-flash") || lower.contains("2.0-flash") || lower.contains("flash")
    {
        // Flash standard: $0.10 / 1M input, $0.40 / 1M output
        (Some(0.10), Some(0.40))
    } else if lower.contains("2.5-pro") || lower.contains("pro") {
        // Pro models: $1.25 / 1M input, $5.00 / 1M output
        (Some(1.25), Some(5.00))
    } else {
        (None, None)
    }
}

pub fn format_api_error(status_code: Option<u16>, raw_body: &str) -> String {
    if let Ok(val) = serde_json::from_str::<serde_json::Value>(raw_body) {
        if let Some(err) = val.get("error") {
            let code = err
                .get("code")
                .and_then(|c| c.as_i64())
                .unwrap_or(status_code.unwrap_or(0) as i64);
            let message = err
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("Unknown error");
            let status = err.get("status").and_then(|s| s.as_str()).unwrap_or("");

            if status == "UNAVAILABLE" || code == 503 {
                return "Provider service temporarily unavailable (503 high demand). Please retry in a moment.".to_string();
            } else if status == "RESOURCE_EXHAUSTED" || code == 429 {
                return "Provider rate limit / quota exceeded (429). Please wait before retrying or use a fallback model.".to_string();
            } else if status == "PERMISSION_DENIED" || code == 403 {
                return "Provider API key invalid or permission denied (403). Check the active provider key.".to_string();
            } else if status == "INVALID_ARGUMENT" || code == 400 {
                return format!("Invalid request payload (400): {}", message);
            } else {
                return format!("API Error {} ({}): {}", code, status, message);
            }
        }
    }

    if let Some(code) = status_code {
        format!("HTTP Error {}: {}", code, raw_body.trim())
    } else {
        raw_body.trim().to_string()
    }
}

mod openai;
mod providers;
pub mod sse;
pub mod types;

use crate::config::AppConfig;
use crate::events::StreamSignal;
use providers::{adapter_for, ModelCatalogShape, ProviderAdapter};
use reqwest::{Client, RequestBuilder};
use std::collections::BTreeMap;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};
use tokio::sync::mpsc::UnboundedSender;
use tokio::time::{sleep, Duration};
use types::{GenerateContentRequest, ListModelsResponse, ModelInfo};

#[derive(Clone)]
pub struct AiClient {
    client: Client,
    api_key: String,
    base_url: String,
    headers: BTreeMap<String, String>,
    include_stream_usage: bool,
    protocol: ProviderProtocol,
    codex_account_id: String,
    adapter: Arc<dyn ProviderAdapter>,
    zen_session_id: Arc<String>,
    zen_request_counter: Arc<AtomicU64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderKind {
    Gemini,
    OpenAiCompatible,
    Codex,
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
            "codex" | "chatgpt" | "openai-codex" => Self::Codex,
            _ => Self::OpenAiCompatible,
        }
    }
}

impl AiClient {
    fn authorize(&self, mut request: RequestBuilder) -> RequestBuilder {
        for (name, value) in &self.headers {
            request = request.header(name, value);
        }
        let request_id = self.zen_request_counter.fetch_add(1, Ordering::Relaxed);
        self.adapter.authorize(
            request,
            &self.api_key,
            &self.codex_account_id,
            self.zen_session_id.as_str(),
            request_id,
            &self.base_url,
        )
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
            AppConfig::get_codex_auth()
                .map(|auth| auth.account_id)
                .unwrap_or_default(),
        )
    }

    pub fn with_provider(
        api_key: String,
        provider: ProviderKind,
        base_url: Option<String>,
        headers: BTreeMap<String, String>,
        include_stream_usage: bool,
        protocol: ProviderProtocol,
        codex_account_id: String,
    ) -> Self {
        Self {
            client: Client::builder()
                .tcp_nodelay(true)
                .connect_timeout(Duration::from_secs(30))
                .read_timeout(Duration::from_secs(120))
                .build()
                .unwrap_or_default(),
            base_url: base_url
                .unwrap_or_else(|| adapter_for(provider).default_base_url().to_string()),
            api_key,
            headers,
            include_stream_usage,
            protocol,
            codex_account_id,
            adapter: adapter_for(provider),
            zen_session_id: Arc::new(format!(
                "holiday-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos()
            )),
            zen_request_counter: Arc::new(AtomicU64::new(0)),
        }
    }

    pub fn update_provider(
        &mut self,
        provider: ProviderKind,
        base_url: Option<String>,
        headers: BTreeMap<String, String>,
        include_stream_usage: bool,
        protocol: ProviderProtocol,
        codex_account_id: String,
    ) {
        self.headers = headers;
        self.include_stream_usage = include_stream_usage;
        self.protocol = protocol;
        self.adapter = adapter_for(provider);
        self.base_url = base_url.unwrap_or_else(|| self.adapter.default_base_url().to_string());
        self.codex_account_id = codex_account_id;
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
        if self.is_openai_protocol_provider() {
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
                        let _ = tx.send(StreamSignal::ModelSelected {
                            model: candidate.clone(),
                            protocol: ("gemini").into(),
                        });
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
        if self.is_openai_protocol_provider() {
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

    /// Auxiliary requests never retry or fall back: a gateway timeout may still
    /// represent a billed/in-flight request. Bound the entire operation, not reads.
    pub async fn generate_bounded(
        &self,
        model: &str,
        request: &GenerateContentRequest,
        seconds: u64,
    ) -> Result<String, String> {
        tokio::time::timeout(
            Duration::from_secs(seconds),
            self.generate_content(model, request),
        )
        .await
        .map_err(|_| {
            format!("Total deadline exceeded ({seconds}s); provider processing may still continue")
        })?
    }

    /// Review-only structured response request. No protocol switching, retries,
    /// or downgrade if a compatible provider rejects the schema.
    pub async fn generate_bounded_structured(
        &self,
        model: &str,
        request: &GenerateContentRequest,
        seconds: u64,
        schema: serde_json::Value,
    ) -> Result<String, String> {
        if !self.is_openai_protocol_provider() {
            return Err("Structured auto-review requires an OpenAI-compatible provider".into());
        }
        let mut request = request.clone();
        let config = request
            .generation_config
            .as_mut()
            .ok_or("Review generation config missing")?;
        config.extra = Some(structured_output_options(
            self.uses_responses(model),
            schema,
        ));
        self.generate_bounded(model, &request, seconds).await
    }

    pub async fn generate_content_with_fallback(
        &self,
        model: &str,
        fallback_models: &[String],
        max_retries: u32,
        request: &GenerateContentRequest,
    ) -> Result<String, String> {
        let mut models = model_candidates(model, fallback_models);
        if self.is_openai_protocol_provider() {
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
        if self.is_openai_protocol_provider() {
            return self.list_openai_models().await;
        }
        let url = format!("{}/models?key={}", self.base_url, self.api_key);
        let resp = self
            .client
            .get(&url)
            .timeout(Duration::from_secs(30))
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
                    context_window: None,
                    output_token_limit: entry.output_token_limit,
                    reasoning_levels: Vec::new(),
                    metadata_source: "provider catalog".into(),
                });
            }
        }

        Ok(models)
    }

    pub async fn usage_summary(&self) -> Result<String, String> {
        let Some(path) = self.adapter.usage_path() else {
            return Err("This provider does not expose account usage".to_string());
        };
        let url = format!("{}/{}", self.base_url.trim_end_matches('/'), path);
        let response = self
            .authorize(self.client.get(&url))
            .timeout(Duration::from_secs(30))
            .send()
            .await
            .map_err(|error| format!("Failed to fetch provider usage: {error}"))?;
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(format_api_error(Some(status.as_u16()), &body));
        }
        let data: serde_json::Value = serde_json::from_str(&body)
            .map_err(|error| format!("Failed to parse provider usage: {error}"))?;
        self.adapter.format_usage(&data)
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
                let error = format!("Zen model {} uses an unsupported protocol for Holiday; choose a Chat Completions or Responses model.", candidate);
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
                    let mut payload = openai::responses_request_payload(candidate, request, true);
                    self.adapter.customize_responses_payload(&mut payload);
                    payload
                } else {
                    openai::request_payload(candidate, request, true, self.include_stream_usage)
                };
                let response = self
                    .authorize(self.client.post(&url).json(&payload))
                    .send()
                    .await;
                match response {
                    Ok(response) if response.status().is_success() => {
                        let _ = tx.send(StreamSignal::ModelSelected {
                            model: candidate.clone(),
                            protocol: (if use_responses {
                                "responses"
                            } else {
                                "chat-completions"
                            })
                            .into(),
                        });
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
            let mut payload = openai::responses_request_payload(model, request, false);
            self.adapter.customize_responses_payload(&mut payload);
            let response = self
                .authorize(self.client.post(&url).json(&payload))
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
        let url = self.adapter.models_url(&self.base_url);
        let response = self
            .authorize(self.client.get(&url))
            .timeout(Duration::from_secs(30))
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
        parse_model_catalog(&data, self.adapter.model_catalog_shape(), self.is_zen())
    }

    fn is_zen(&self) -> bool {
        self.base_url
            .to_ascii_lowercase()
            .contains("opencode.ai/zen/")
    }

    fn is_openai_protocol_provider(&self) -> bool {
        self.adapter.is_openai_protocol()
    }

    fn uses_responses(&self, model: &str) -> bool {
        self.adapter
            .uses_responses(self.protocol, model, &self.base_url)
    }
}

fn structured_output_options(responses: bool, schema: serde_json::Value) -> serde_json::Value {
    if responses {
        serde_json::json!({"text":{"format":{"type":"json_schema","name":"approval_review","strict":true,"schema":schema}}})
    } else {
        serde_json::json!({"response_format":{"type":"json_schema","json_schema":{"name":"approval_review","strict":true,"schema":schema}}})
    }
}

fn parse_model_catalog(
    data: &serde_json::Value,
    shape: ModelCatalogShape,
    zen: bool,
) -> Result<Vec<ModelInfo>, String> {
    let entries = data
        .get(match shape {
            ModelCatalogShape::Codex => "models",
            ModelCatalogShape::OpenAi => "data",
            ModelCatalogShape::Gemini => "models",
        })
        .and_then(|value| value.as_array())
        .ok_or_else(|| "Model catalog is missing its expected model array".to_string())?;
    Ok(entries
        .iter()
        .filter_map(|entry| {
            let id = entry
                .get(match shape {
                    ModelCatalogShape::Codex => "slug",
                    ModelCatalogShape::OpenAi => "id",
                    ModelCatalogShape::Gemini => "name",
                })
                .or_else(|| entry.get("id"))
                .and_then(|value| value.as_str())?;
            if shape == ModelCatalogShape::Codex
                && entry
                    .get("visibility")
                    .and_then(|value| value.as_str())
                    .is_some_and(|visibility| visibility != "list")
            {
                return None;
            }
            if shape == ModelCatalogShape::Codex
                && entry
                    .get("supported_in_api")
                    .and_then(|value| value.as_bool())
                    == Some(false)
            {
                return None;
            }
            if zen && is_zen_unsupported_model_id(id) {
                return None;
            }
            let description = entry
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or(if shape == ModelCatalogShape::Codex {
                    "Codex model"
                } else {
                    "OpenAI-compatible model"
                })
                .to_string();
            let input_price_per_m = openrouter_price_per_m(entry, "prompt");
            let output_price_per_m = openrouter_price_per_m(entry, "completion");
            let context_window = model_context_limit(entry);
            let input_token_limit = positive_limit(
                entry,
                &["input_token_limit", "max_input_tokens", "inputTokenLimit"],
            );
            let output_token_limit = positive_limit(
                entry,
                &[
                    "output_token_limit",
                    "max_output_tokens",
                    "outputTokenLimit",
                ],
            );
            let reasoning_levels = entry
                .get("supported_reasoning_levels")
                .and_then(|value| value.as_array())
                .map(|levels| {
                    levels
                        .iter()
                        .filter_map(|level| {
                            level
                                .as_str()
                                .or_else(|| {
                                    level
                                        .get("effort")
                                        .or_else(|| level.get("level"))
                                        .and_then(|value| value.as_str())
                                })
                                .map(str::to_string)
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            Some(ModelInfo {
                id: id.to_string(),
                display_name: entry
                    .get("display_name")
                    .or_else(|| entry.get("name"))
                    .and_then(|v| v.as_str())
                    .unwrap_or(id)
                    .to_string(),
                description,
                input_price_per_m,
                output_price_per_m,
                input_token_limit,
                context_window,
                output_token_limit,
                reasoning_levels,
                metadata_source: "provider catalog".into(),
            })
        })
        .collect())
}

fn positive_limit(entry: &serde_json::Value, keys: &[&str]) -> Option<u64> {
    keys.iter().find_map(|key| {
        entry
            .get(*key)
            .and_then(|value| {
                value
                    .as_u64()
                    .or_else(|| value.as_str().and_then(|s| s.trim().parse().ok()))
            })
            .filter(|limit| *limit > 0)
    })
}

fn model_context_limit(entry: &serde_json::Value) -> Option<u64> {
    let keys = [
        "context_window",
        "max_context_window",
        "context_length",
        "contextWindow",
        "maxContextWindow",
        "contextLength",
    ];
    positive_limit(entry, &keys).or_else(|| {
        entry
            .get("limits")
            .and_then(|limits| positive_limit(limits, &keys))
    })
}

/// Zen publishes one model catalog for several wire protocols. These model
/// families are documented as Responses rather than Chat Completions.
#[allow(dead_code)]
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

#[cfg(test)]
mod tests {
    use super::{
        is_zen_responses_model_id, is_zen_unsupported_model_id, model_context_limit,
        openrouter_price_per_m, ProviderKind, ProviderProtocol,
    };
    use serde_json::json;

    #[tokio::test]
    async fn bounded_request_does_not_retry_gateway_timeout() {
        use tokio::{
            io::{AsyncReadExt, AsyncWriteExt},
            net::TcpListener,
        };
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buffer = vec![0; 8192];
            socket.read(&mut buffer).await.unwrap();
            socket.write_all(b"HTTP/1.1 504 Gateway Timeout\r\nContent-Length: 7\r\nConnection: close\r\n\r\ntimeout").await.unwrap();
            assert!(
                tokio::time::timeout(std::time::Duration::from_millis(150), listener.accept())
                    .await
                    .is_err()
            );
        });
        let client = super::AiClient::with_provider(
            "test".into(),
            super::ProviderKind::OpenAiCompatible,
            Some(format!("http://{address}")),
            Default::default(),
            false,
            super::ProviderProtocol::ChatCompletions,
            String::new(),
        );
        let request = super::types::GenerateContentRequest {
            contents: vec![],
            system_instruction: None,
            generation_config: None,
            safety_settings: None,
            tools: None,
        };
        let error = client
            .generate_bounded("test", &request, 2)
            .await
            .unwrap_err();
        assert!(error.contains("504"));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn bounded_request_enforces_total_deadline() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let client = super::AiClient::with_provider(
            "test".into(),
            super::ProviderKind::OpenAiCompatible,
            Some(format!("http://{address}")),
            Default::default(),
            false,
            super::ProviderProtocol::ChatCompletions,
            String::new(),
        );
        let request = super::types::GenerateContentRequest {
            contents: vec![],
            system_instruction: None,
            generation_config: None,
            safety_settings: None,
            tools: None,
        };
        let error = client
            .generate_bounded("test", &request, 1)
            .await
            .unwrap_err();
        assert!(error.contains("Total deadline exceeded"));
    }

    #[test]
    fn structured_review_format_uses_protocol_specific_fields() {
        let schema =
            serde_json::json!({"type":"object","properties":{},"additionalProperties":false});
        for responses in [true, false] {
            let options = super::structured_output_options(responses, schema.clone());
            let request = super::types::GenerateContentRequest {
                contents: vec![],
                system_instruction: None,
                generation_config: Some(super::types::GenerationConfig {
                    temperature: None,
                    max_output_tokens: Some(1200),
                    thinking_config: None,
                    reasoning_effort: None,
                    extra: Some(options),
                }),
                safety_settings: None,
                tools: None,
            };
            let payload = if responses {
                super::openai::responses_request_payload("codex-auto-review", &request, false)
            } else {
                super::openai::request_payload("codex-auto-review", &request, false, false)
            };
            let format = if responses {
                &payload["text"]["format"]
            } else {
                &payload["response_format"]["json_schema"]
            };
            assert_eq!(format["schema"], schema);
            assert_eq!(format["strict"], true);
            assert_eq!(format["name"], "approval_review");
            if responses {
                assert!(payload.get("response_format").is_none());
            } else {
                assert!(payload.get("text").is_none());
            }
        }
    }

    #[test]
    fn app_refresh_does_not_reuse_previous_provider_credentials() {
        let config = crate::config::AppConfig {
            provider: "holiday-test-without-credentials".into(),
            ..Default::default()
        };
        let mut app = crate::app::App::new(config, "previous-provider-secret".into());
        app.refresh_client_from_config();
        let request = app
            .client
            .authorize(app.client.client.get("https://example.invalid/v1/models"))
            .build()
            .unwrap();
        assert!(!request.headers().contains_key("authorization"));
    }

    #[test]
    fn catalog_preserves_distinct_limits_and_reasoning_shapes() {
        let models = super::parse_model_catalog(&json!({"data": [{
            "id": "test", "context_window": "128000", "input_token_limit": 120000,
            "output_token_limit": 8000, "supported_reasoning_levels": ["low", {"effort": "high"}]
        }]}), super::ModelCatalogShape::OpenAi, false).unwrap();
        assert_eq!(models[0].context_window, Some(128_000));
        assert_eq!(models[0].input_token_limit, Some(120_000));
        assert_eq!(models[0].output_token_limit, Some(8_000));
        assert_eq!(models[0].reasoning_levels, vec!["low", "high"]);
        let minimal = super::parse_model_catalog(
            &json!({"data": [{"id": "unknown"}]}),
            super::ModelCatalogShape::OpenAi,
            false,
        )
        .unwrap();
        assert_eq!(minimal[0].context_window, None);
        assert!(super::parse_model_catalog(
            &json!({"error": "no catalog"}),
            super::ModelCatalogShape::OpenAi,
            false
        )
        .is_err());
    }

    #[test]
    fn context_limits_reject_invalid_values() {
        for value in [json!(0), json!(-1), json!("invalid"), json!(null)] {
            assert_eq!(model_context_limit(&json!({"context_window": value})), None);
        }
        assert_eq!(
            model_context_limit(&json!({"limits": {"contextWindow": "64000"}})),
            Some(64_000)
        );
        assert_eq!(model_context_limit(&json!({"id": "bearlab-model"})), None);
    }

    #[test]
    fn clearing_credentials_removes_authorization_header() {
        let mut client = super::AiClient::with_provider(
            "old-secret".into(),
            super::ProviderKind::OpenAiCompatible,
            Some("https://example.invalid/v1".into()),
            Default::default(),
            true,
            super::ProviderProtocol::Auto,
            String::new(),
        );
        client.update_api_key(String::new());
        let request = client
            .authorize(client.client.get("https://example.invalid/v1/models"))
            .build()
            .unwrap();
        assert!(!request.headers().contains_key("authorization"));
    }

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
        assert_eq!(ProviderKind::parse("codex"), ProviderKind::Codex);
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
    fn parses_context_window_metadata_from_codex_catalog_shapes() {
        assert_eq!(
            model_context_limit(&json!({"context_window": 272000})),
            Some(272000)
        );
        assert_eq!(
            model_context_limit(&json!({"max_context_window": "256000"})),
            Some(256000)
        );
        assert_eq!(
            model_context_limit(&json!({"limits": {"context_window": 128000}})),
            Some(128000)
        );
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

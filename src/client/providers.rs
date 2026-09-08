use super::{ProviderKind, ProviderProtocol};
use reqwest::RequestBuilder;
use serde_json::Value;

// The Codex backend uses this independently from Holiday's package version
// when deciding which model presets to return from `/models`.
const CODEX_CLIENT_VERSION: &str = "0.153.4";
const CODEX_USER_AGENT: &str = "codex_cli_rs/0.153.4";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ModelCatalogShape {
    Gemini,
    OpenAi,
    Codex,
}

pub(super) trait ProviderAdapter: Send + Sync {
    fn default_base_url(&self) -> &'static str;

    fn authorize(
        &self,
        request: RequestBuilder,
        api_key: &str,
        account_id: &str,
        session_id: &str,
        request_id: u64,
        base_url: &str,
    ) -> RequestBuilder;

    fn is_openai_protocol(&self) -> bool;

    fn uses_responses(&self, protocol: ProviderProtocol, model: &str, base_url: &str) -> bool;

    fn customize_responses_payload(&self, _payload: &mut Value) {}

    fn models_url(&self, base_url: &str) -> String;

    fn model_catalog_shape(&self) -> ModelCatalogShape;

    fn usage_path(&self) -> Option<&'static str> {
        None
    }

    fn format_usage(&self, _data: &Value) -> Result<String, String> {
        Err("This provider does not expose account usage".to_string())
    }
}

pub(super) fn adapter_for(kind: ProviderKind) -> std::sync::Arc<dyn ProviderAdapter> {
    match kind {
        ProviderKind::Gemini => std::sync::Arc::new(GeminiAdapter),
        ProviderKind::OpenAiCompatible => std::sync::Arc::new(OpenAiAdapter),
        ProviderKind::Codex => std::sync::Arc::new(CodexAdapter),
    }
}

struct GeminiAdapter;

impl ProviderAdapter for GeminiAdapter {
    fn default_base_url(&self) -> &'static str {
        "https://generativelanguage.googleapis.com/v1beta"
    }

    fn authorize(
        &self,
        request: RequestBuilder,
        _api_key: &str,
        _account_id: &str,
        _session_id: &str,
        _request_id: u64,
        _base_url: &str,
    ) -> RequestBuilder {
        request
    }

    fn is_openai_protocol(&self) -> bool {
        false
    }

    fn uses_responses(&self, _protocol: ProviderProtocol, _model: &str, _base_url: &str) -> bool {
        false
    }

    fn models_url(&self, base_url: &str) -> String {
        format!("{}/models", base_url.trim_end_matches('/'))
    }

    fn model_catalog_shape(&self) -> ModelCatalogShape {
        ModelCatalogShape::Gemini
    }
}

struct OpenAiAdapter;

impl ProviderAdapter for OpenAiAdapter {
    fn default_base_url(&self) -> &'static str {
        "https://api.openai.com/v1"
    }

    fn authorize(
        &self,
        mut request: RequestBuilder,
        api_key: &str,
        _account_id: &str,
        session_id: &str,
        request_id: u64,
        base_url: &str,
    ) -> RequestBuilder {
        if base_url.to_ascii_lowercase().contains("opencode.ai/zen/") {
            request = request
                .header("x-opencode-session", session_id)
                .header("x-opencode-request", format!("holiday-{request_id}"))
                .header("x-opencode-client", "cli")
                .header(
                    "User-Agent",
                    concat!("opencode/holiday/", env!("CARGO_PKG_VERSION")),
                );
            return if api_key.is_empty() {
                request.bearer_auth("public")
            } else {
                request.bearer_auth(api_key)
            };
        }
        if !api_key.is_empty() {
            request = request.bearer_auth(api_key);
        }
        request
    }

    fn is_openai_protocol(&self) -> bool {
        true
    }

    fn uses_responses(&self, protocol: ProviderProtocol, model: &str, base_url: &str) -> bool {
        match protocol {
            ProviderProtocol::Responses => true,
            ProviderProtocol::ChatCompletions => false,
            ProviderProtocol::Auto => {
                let model = model.trim_start_matches("models/").to_ascii_lowercase();
                base_url.to_ascii_lowercase().contains("opencode.ai/zen/")
                    && (model.starts_with("gpt-")
                        || model.starts_with("grok-")
                        || model.starts_with("muse-spark-"))
            }
        }
    }

    fn models_url(&self, base_url: &str) -> String {
        format!("{}/models", base_url.trim_end_matches('/'))
    }

    fn model_catalog_shape(&self) -> ModelCatalogShape {
        ModelCatalogShape::OpenAi
    }
}

struct CodexAdapter;

impl ProviderAdapter for CodexAdapter {
    fn default_base_url(&self) -> &'static str {
        "https://chatgpt.com/backend-api/codex"
    }

    fn authorize(
        &self,
        mut request: RequestBuilder,
        api_key: &str,
        account_id: &str,
        session_id: &str,
        _request_id: u64,
        _base_url: &str,
    ) -> RequestBuilder {
        if !api_key.is_empty() {
            request = request.bearer_auth(api_key);
        }
        if !account_id.is_empty() {
            request = request.header("ChatGPT-Account-Id", account_id);
        }
        request
            .header("originator", "codex_cli_rs")
            .header("User-Agent", CODEX_USER_AGENT)
            .header("session-id", session_id)
    }

    fn is_openai_protocol(&self) -> bool {
        true
    }

    fn uses_responses(&self, _protocol: ProviderProtocol, _model: &str, _base_url: &str) -> bool {
        true
    }

    fn customize_responses_payload(&self, payload: &mut Value) {
        payload["store"] = Value::Bool(false);
        payload["tool_choice"] = Value::String("auto".to_string());
        payload["parallel_tool_calls"] = Value::Bool(true);
        payload["include"] = serde_json::json!(["reasoning.encrypted_content"]);
        if let Some(object) = payload.as_object_mut() {
            object.remove("max_output_tokens");
            object.remove("temperature");
            object.remove("top_p");
            object.remove("top_logprobs");
        }
    }

    fn models_url(&self, base_url: &str) -> String {
        format!(
            "{}/models?client_version={}",
            base_url.trim_end_matches('/'),
            CODEX_CLIENT_VERSION
        )
    }

    fn model_catalog_shape(&self) -> ModelCatalogShape {
        ModelCatalogShape::Codex
    }

    fn usage_path(&self) -> Option<&'static str> {
        Some("usage")
    }

    fn format_usage(&self, data: &Value) -> Result<String, String> {
        let plan = data
            .get("plan_type")
            .or_else(|| data.get("planType"))
            .and_then(|value| value.as_str())
            .unwrap_or("unknown");
        let mut lines = vec![format!("Codex usage ({plan})")];
        if let Some(window) = data
            .pointer("/rate_limit/primary_window")
            .or_else(|| data.pointer("/rate_limit/primary"))
        {
            lines.push(format_usage_window("Primary", window));
        }
        if let Some(window) = data
            .pointer("/rate_limit/secondary_window")
            .or_else(|| data.pointer("/rate_limit/secondary"))
        {
            lines.push(format_usage_window("Secondary", window));
        }
        if let Some(additional) = data
            .get("additional_rate_limits")
            .and_then(|value| value.as_array())
        {
            for entry in additional {
                let name = entry
                    .get("limit_name")
                    .or_else(|| entry.get("limitName"))
                    .and_then(|value| value.as_str())
                    .unwrap_or("Additional");
                if let Some(window) = entry
                    .pointer("/rate_limit/primary_window")
                    .or_else(|| entry.pointer("/rate_limit/primary"))
                {
                    lines.push(format_usage_window(name, window));
                }
            }
        }
        if lines.len() == 1 {
            lines.push("No rate-limit windows were returned by the account.".to_string());
        }
        Ok(lines.join("\n"))
    }
}

fn format_usage_window(label: &str, window: &Value) -> String {
    let used = window
        .get("used_percent")
        .or_else(|| window.get("usedPercent"))
        .and_then(|value| value.as_f64())
        .unwrap_or(0.0);
    let left = (100.0 - used).clamp(0.0, 100.0);
    let duration = window
        .get("limit_window_seconds")
        .or_else(|| window.get("window_duration_seconds"))
        .and_then(|value| value.as_u64())
        .map(|seconds| match seconds / 3600 {
            0 => format!("{}m", seconds / 60),
            hours if hours % 24 == 0 => format!("{}d", hours / 24),
            hours => format!("{}h", hours),
        })
        .unwrap_or_else(|| "window".to_string());
    let reset = window
        .get("reset_after_seconds")
        .or_else(|| window.get("resetAfterSeconds"))
        .and_then(|value| value.as_u64())
        .map(format_duration)
        .unwrap_or_else(|| "unknown".to_string());
    format!("{label}: {:.0}% left ({duration}), resets in {reset}", left)
}

fn format_duration(seconds: u64) -> String {
    if seconds >= 86_400 {
        format!("{}d {}h", seconds / 86_400, (seconds % 86_400) / 3_600)
    } else if seconds >= 3_600 {
        format!("{}h {}m", seconds / 3_600, (seconds % 3_600) / 60)
    } else {
        format!("{}m", seconds / 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_responses_payload_uses_codex_compatibility_fields() {
        let mut payload = serde_json::json!({
            "max_output_tokens": 1024,
            "temperature": 0.7,
            "top_p": 0.9,
            "top_logprobs": 2
        });
        CodexAdapter.customize_responses_payload(&mut payload);

        assert_eq!(payload["store"], false);
        assert_eq!(payload["tool_choice"], "auto");
        assert_eq!(payload["parallel_tool_calls"], true);
        assert_eq!(payload["include"][0], "reasoning.encrypted_content");
        assert!(payload.get("max_output_tokens").is_none());
        assert!(payload.get("temperature").is_none());
        assert!(payload.get("top_p").is_none());
        assert!(payload.get("top_logprobs").is_none());
    }
}

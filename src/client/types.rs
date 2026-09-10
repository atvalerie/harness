use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Content {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    pub parts: Vec<Part>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Part {
    FunctionResponse {
        #[serde(rename = "functionResponse")]
        function_response: FunctionResponsePayload,
    },
    FunctionCall {
        #[serde(rename = "functionCall")]
        function_call: FunctionCallPayload,
        #[serde(rename = "thoughtSignature", skip_serializing_if = "Option::is_none")]
        thought_signature: Option<String>,
    },
    Text {
        text: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        thought: Option<bool>,
    },
    InlineData {
        #[serde(rename = "inlineData")]
        inline_data: InlineDataPayload,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InlineDataPayload {
    #[serde(rename = "mimeType")]
    pub mime_type: String,
    pub data: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionCallPayload {
    pub name: String,
    pub args: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionResponsePayload {
    pub name: String,
    pub response: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThinkingConfig {
    #[serde(rename = "thinkingLevel", skip_serializing_if = "Option::is_none")]
    pub thinking_level: Option<String>,
    #[serde(rename = "thinkingBudget")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_budget: Option<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenerationConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(rename = "maxOutputTokens", skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
    #[serde(rename = "thinkingConfig", skip_serializing_if = "Option::is_none")]
    pub thinking_config: Option<ThinkingConfig>,
    #[serde(skip)]
    pub reasoning_effort: Option<String>,
    #[serde(skip)]
    pub extra: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SafetySetting {
    pub category: String,
    pub threshold: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeminiToolDeclaration {
    #[serde(rename = "functionDeclarations")]
    pub function_declarations: Vec<FunctionDeclaration>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionDeclaration {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenerateContentRequest {
    pub contents: Vec<Content>,
    #[serde(rename = "systemInstruction", skip_serializing_if = "Option::is_none")]
    pub system_instruction: Option<Content>,
    #[serde(rename = "generationConfig", skip_serializing_if = "Option::is_none")]
    pub generation_config: Option<GenerationConfig>,
    #[serde(rename = "safetySettings", skip_serializing_if = "Option::is_none")]
    pub safety_settings: Option<Vec<SafetySetting>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<GeminiToolDeclaration>>,
}

// SSE Incoming Candidate types
#[derive(Debug, Clone, Deserialize)]
pub struct GenerateContentResponse {
    pub candidates: Option<Vec<Candidate>>,
    #[serde(rename = "usageMetadata")]
    pub usage_metadata: Option<UsageMetadata>,
    pub error: Option<ApiError>,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct ApiError {
    pub code: Option<i64>,
    pub message: Option<String>,
    pub status: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Candidate {
    pub content: Option<ContentPayload>,
    #[serde(rename = "finishReason")]
    pub finish_reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct ContentPayload {
    pub role: Option<String>,
    pub parts: Option<Vec<ResponsePart>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ResponsePart {
    pub text: Option<String>,
    pub thought: Option<bool>,
    #[serde(rename = "functionCall")]
    pub function_call: Option<FunctionCallPayload>,
    #[serde(rename = "thoughtSignature")]
    pub thought_signature: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UsageMetadata {
    #[serde(rename = "promptTokenCount")]
    pub prompt_token_count: Option<u64>,
    #[serde(rename = "candidatesTokenCount")]
    pub candidates_token_count: Option<u64>,
    #[serde(rename = "totalTokenCount")]
    pub total_token_count: Option<u64>,
    #[serde(rename = "cachedContentTokenCount")]
    pub cached_content_token_count: Option<u64>,
    #[serde(rename = "thoughtsTokenCount")]
    pub thoughts_token_count: Option<u64>,
}

impl UsageMetadata {
    pub fn normalized(&self) -> crate::usage::TokenUsage {
        crate::usage::TokenUsage {
            input_tokens: self.prompt_token_count,
            output_tokens: self
                .candidates_token_count
                .map(|n| n.saturating_add(self.thoughts_token_count.unwrap_or(0))),
            total_tokens: self.total_token_count,
            cache_read_tokens: self.cached_content_token_count,
            reasoning_tokens: self.thoughts_token_count,
            ..Default::default()
        }
    }
}

// Models API types
#[derive(Debug, Clone, Deserialize)]
pub struct ListModelsResponse {
    pub models: Option<Vec<ModelApiEntry>>,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct ModelApiEntry {
    pub name: String,
    #[serde(rename = "displayName")]
    pub display_name: Option<String>,
    pub description: Option<String>,
    #[serde(rename = "inputTokenLimit")]
    pub input_token_limit: Option<u64>,
    #[serde(rename = "outputTokenLimit")]
    pub output_token_limit: Option<u64>,
    #[serde(rename = "supportedGenerationMethods")]
    pub supported_generation_methods: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    pub display_name: String,
    pub description: String,
    pub input_price_per_m: Option<f64>,
    pub output_price_per_m: Option<f64>,
    pub input_token_limit: Option<u64>,
    #[serde(default)]
    pub context_window: Option<u64>,
    #[serde(default)]
    pub output_token_limit: Option<u64>,
    #[serde(default)]
    pub reasoning_levels: Vec<String>,
    #[serde(default)]
    pub metadata_source: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thought_signature_is_preserved_as_a_function_call_sibling() {
        let part = Part::FunctionCall {
            function_call: FunctionCallPayload {
                name: "run_command".into(),
                args: serde_json::json!({"command":"git status"}),
                id: Some("call-1".into()),
            },
            thought_signature: Some("sig".into()),
        };
        let value = serde_json::to_value(part).unwrap();
        assert_eq!(value["thoughtSignature"], "sig");
        assert_eq!(value["functionCall"]["name"], "run_command");
    }
}

use super::types::{Content, GenerateContentRequest, Part};
use crate::events::StreamSignal;
use futures_util::StreamExt;
use reqwest::Response;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use tokio::sync::mpsc::UnboundedSender;

pub fn request_payload(model: &str, request: &GenerateContentRequest, stream: bool) -> Value {
    let mut messages = Vec::new();
    if let Some(system) = &request.system_instruction {
        messages.extend(content_messages(system));
    }
    for content in &request.contents {
        messages.extend(content_messages(content));
    }

    let mut payload = json!({ "model": model, "messages": messages, "stream": stream });
    if let Some(config) = &request.generation_config {
        if let Some(temp) = config.temperature { payload["temperature"] = json!(temp); }
        if let Some(max) = config.max_output_tokens { payload["max_tokens"] = json!(max); }
        if let Some(reasoning_effort) = &config.reasoning_effort { payload["reasoning_effort"] = json!(reasoning_effort); }
        if let Some(extra) = &config.extra {
            if let Some(extra_object) = extra.as_object() {
                for (key, value) in extra_object { payload[key] = value.clone(); }
            }
        }
    }
    if stream { payload["stream_options"] = json!({ "include_usage": true }); }
    if let Some(tools) = &request.tools {
        let declarations = tools.iter().flat_map(|tool| tool.function_declarations.iter()).map(|f| json!({
            "type": "function",
            "function": { "name": f.name, "description": f.description, "parameters": f.parameters }
        })).collect::<Vec<_>>();
        if !declarations.is_empty() { payload["tools"] = json!(declarations); }
    }
    payload
}

fn content_messages(content: &Content) -> Vec<Value> {
    let role = match content.role.as_deref() { Some("model") => "assistant", Some("system") => "system", _ => "user" };
    let mut messages = Vec::new();
    let mut text = String::new();
    let mut tool_calls = Vec::new();
    for part in &content.parts {
        match part {
            Part::Text { text: value, .. } => text.push_str(value),
            Part::FunctionCall { function_call, .. } => tool_calls.push(json!({
                "id": function_call.id.clone().unwrap_or_else(|| format!("call_{}", function_call.name)),
                "type": "function",
                "function": { "name": function_call.name, "arguments": function_call.args.to_string() }
            })),
            Part::FunctionResponse { function_response } => {
                messages.push(json!({
                    "role": "tool",
                    "tool_call_id": function_response.id.clone().unwrap_or_default(),
                    "name": function_response.name,
                    "content": function_response.response.to_string()
                }));
            }
        }
    }
    if !text.is_empty() || tool_calls.is_empty() && messages.is_empty() {
        let mut message = json!({ "role": role, "content": text });
        if !tool_calls.is_empty() { message["tool_calls"] = json!(tool_calls); }
        messages.insert(0, message);
    } else if !tool_calls.is_empty() {
        messages.insert(0, json!({ "role": role, "tool_calls": tool_calls }));
    }
    messages
}

pub async fn stream_response(response: Response, tx: UnboundedSender<StreamSignal>) {
    let mut stream = response.bytes_stream();
    let mut buffer = Vec::new();
    let mut tools: BTreeMap<usize, (Option<String>, String, String)> = BTreeMap::new();
    let mut finished = false;
    while let Some(chunk) = stream.next().await {
        match chunk {
            Ok(bytes) => {
                buffer.extend_from_slice(&bytes);
                while let Some(pos) = buffer.iter().position(|b| *b == b'\n') {
                    let mut line = buffer.drain(..=pos).collect::<Vec<_>>();
                    if line.last() == Some(&b'\n') { line.pop(); }
                    if line.last() == Some(&b'\r') { line.pop(); }
                    let line = String::from_utf8_lossy(&line);
                    let Some(data) = line.strip_prefix("data:").map(str::trim) else { continue };
                    if data == "[DONE]" { emit_tools(&mut tools, &tx); let _ = tx.send(StreamSignal::Finished { finish_reason: Some("STOP".to_string()) }); finished = true; continue; }
                    let Ok(value) = serde_json::from_str::<Value>(data) else { continue };
                    if let Some(usage) = value.get("usage") { emit_usage(usage, &tx); }
                    for choice in value.get("choices").and_then(Value::as_array).into_iter().flatten() {
                        let delta = choice.get("delta").unwrap_or(&Value::Null);
                        if let Some(text) = delta.get("content").and_then(Value::as_str) { let _ = tx.send(StreamSignal::TextDelta(text.to_string())); }
                        if let Some(thought) = delta.get("reasoning_content").and_then(Value::as_str) { let _ = tx.send(StreamSignal::ThoughtDelta(thought.to_string())); }
                        for call in delta.get("tool_calls").and_then(Value::as_array).into_iter().flatten() {
                            let index = call.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                            let entry = tools.entry(index).or_insert((None, String::new(), String::new()));
                            if let Some(id) = call.get("id").and_then(Value::as_str) { entry.0 = Some(id.to_string()); }
                            if let Some(name) = call.pointer("function.name").and_then(Value::as_str) { entry.1.push_str(name); }
                            if let Some(args) = call.pointer("function.arguments").and_then(Value::as_str) { entry.2.push_str(args); }
                        }
                    }
                }
            }
            Err(error) => { let _ = tx.send(StreamSignal::Error(format!("Network stream error: {}", error))); return; }
        }
    }
    emit_tools(&mut tools, &tx);
    if !finished { let _ = tx.send(StreamSignal::Finished { finish_reason: Some("STOP".to_string()) }); }
}

fn emit_tools(tools: &mut BTreeMap<usize, (Option<String>, String, String)>, tx: &UnboundedSender<StreamSignal>) {
    for (_, (id, name, arguments)) in std::mem::take(tools) {
        let args = serde_json::from_str(&arguments).unwrap_or_else(|_| json!({ "raw_arguments": arguments }));
        let _ = tx.send(StreamSignal::ToolCall { id, name, args, thought_signature: None });
    }
}

fn emit_usage(usage: &Value, tx: &UnboundedSender<StreamSignal>) {
    let prompt = usage.get("prompt_tokens").and_then(Value::as_u64).unwrap_or(0) as u32;
    let completion = usage.get("completion_tokens").and_then(Value::as_u64).unwrap_or(0) as u32;
    let total = usage.get("total_tokens").and_then(Value::as_u64).unwrap_or(prompt as u64 + completion as u64) as u32;
    let _ = tx.send(StreamSignal::Usage { prompt_tokens: prompt, candidates_tokens: completion, total_tokens: total });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::types::{Content, GenerateContentRequest, Part};

    #[test]
    fn converts_gemini_request_to_openai_chat_payload() {
        let request = GenerateContentRequest {
            contents: vec![Content { role: Some("user".into()), parts: vec![Part::Text { text: "hello".into(), thought: None }] }],
            system_instruction: None,
            generation_config: None,
            safety_settings: None,
            tools: None,
        };
        let payload = request_payload("test-model", &request, true);
        assert_eq!(payload["model"], "test-model");
        assert_eq!(payload["messages"][0]["role"], "user");
        assert_eq!(payload["messages"][0]["content"], "hello");
        assert_eq!(payload["stream_options"]["include_usage"], true);
    }
}

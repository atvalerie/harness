use super::types::{Content, GenerateContentRequest, Part};
use crate::events::StreamSignal;
use futures_util::StreamExt;
use reqwest::Response;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use tokio::sync::mpsc::UnboundedSender;

pub fn request_payload(model: &str, request: &GenerateContentRequest, stream: bool, include_usage: bool) -> Value {
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
    if stream && include_usage { payload["stream_options"] = json!({ "include_usage": true }); }
    if let Some(tools) = &request.tools {
        let declarations = tools.iter().flat_map(|tool| tool.function_declarations.iter()).map(|f| json!({
            "type": "function",
            "function": { "name": f.name, "description": f.description, "parameters": f.parameters }
        })).collect::<Vec<_>>();
        if !declarations.is_empty() { payload["tools"] = json!(declarations); }
    }
    payload
}

/// Build an OpenAI Responses API request. Zen uses this endpoint for several
/// model families (including Muse Spark), while other models use Chat
/// Completions. Keeping the conversion beside the Chat adapter makes the
/// provider choice explicit instead of silently forcing one wire format.
pub fn responses_request_payload(model: &str, request: &GenerateContentRequest, stream: bool) -> Value {
    let mut input = Vec::new();
    for content in &request.contents {
        let role = match content.role.as_deref() {
            Some("model") => "assistant",
            Some("system") => "system",
            _ => "user",
        };
        let mut text = String::new();
        for part in &content.parts {
            match part {
                Part::Text { text: value, .. } => text.push_str(value),
                Part::FunctionCall { function_call, .. } => {
                    input.push(json!({
                        "type": "function_call",
                        "call_id": function_call.id.clone().unwrap_or_else(|| format!("call_{}", function_call.name)),
                        "name": function_call.name,
                        "arguments": function_call.args.to_string(),
                    }));
                }
                Part::FunctionResponse { function_response } => {
                    input.push(json!({
                        "type": "function_call_output",
                        "call_id": function_response.id.clone().unwrap_or_default(),
                        "output": function_response.response.to_string(),
                    }));
                }
            }
        }
        if !text.is_empty() {
            let text_type = if role == "assistant" { "output_text" } else { "input_text" };
            input.push(json!({
                "role": role,
                "content": [{ "type": text_type, "text": text }],
            }));
        }
    }

    let mut payload = json!({
        "model": model,
        "input": input,
        "stream": stream,
    });
    if let Some(system) = &request.system_instruction {
        let instructions = system.parts.iter().filter_map(|part| match part {
            Part::Text { text, .. } => Some(text.as_str()),
            _ => None,
        }).collect::<String>();
        if !instructions.is_empty() { payload["instructions"] = json!(instructions); }
    }
    if let Some(config) = &request.generation_config {
        if let Some(temp) = config.temperature { payload["temperature"] = json!(temp); }
        if let Some(max) = config.max_output_tokens { payload["max_output_tokens"] = json!(max); }
        if let Some(reasoning_effort) = &config.reasoning_effort {
            payload["reasoning"] = json!({ "effort": reasoning_effort });
        }
        if let Some(extra) = &config.extra {
            if let Some(extra_object) = extra.as_object() {
                for (key, value) in extra_object { payload[key] = value.clone(); }
            }
        }
    }
    if let Some(tools) = &request.tools {
        let declarations = tools.iter().flat_map(|tool| tool.function_declarations.iter()).map(|f| json!({
            "type": "function",
            "name": f.name,
            "description": f.description,
            "parameters": f.parameters,
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

pub async fn stream_responses_response(response: Response, tx: UnboundedSender<StreamSignal>) {
    let mut stream = response.bytes_stream();
    let mut buffer = Vec::new();
    let mut tools: BTreeMap<String, (Option<String>, String, String)> = BTreeMap::new();
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
                    if data == "[DONE]" { emit_responses_tools(&mut tools, &tx); let _ = tx.send(StreamSignal::Finished { finish_reason: Some("STOP".to_string()) }); finished = true; continue; }
                    let Ok(value) = serde_json::from_str::<Value>(data) else { continue };
                    let event_type = value.get("type").and_then(Value::as_str).unwrap_or_default();
                    match event_type {
                        "response.output_text.delta" => {
                            if let Some(delta) = value.get("delta").and_then(Value::as_str) { let _ = tx.send(StreamSignal::TextDelta(delta.to_string())); }
                        }
                        "response.reasoning_summary_text.delta" | "response.reasoning_content.delta" => {
                            if let Some(delta) = value.get("delta").and_then(Value::as_str) { let _ = tx.send(StreamSignal::ThoughtDelta(delta.to_string())); }
                        }
                        "response.output_item.added" | "response.output_item.done" => {
                            if let Some(item) = value.get("item") {
                                if item.get("type").and_then(Value::as_str) == Some("function_call") {
                                    let key = item.get("id").and_then(Value::as_str).or_else(|| item.get("call_id").and_then(Value::as_str)).unwrap_or("function_call").to_string();
                                    let entry = tools.entry(key).or_insert((None, String::new(), String::new()));
                                    if let Some(id) = item.get("call_id").and_then(Value::as_str) { entry.0 = Some(id.to_string()); }
                                    if let Some(name) = item.get("name").and_then(Value::as_str) { entry.1 = name.to_string(); }
                                    if let Some(arguments) = item.get("arguments").and_then(Value::as_str) { entry.2 = arguments.to_string(); }
                                }
                            }
                        }
                        "response.function_call_arguments.delta" => {
                            let key = value.get("item_id").and_then(Value::as_str).unwrap_or("function_call").to_string();
                            let entry = tools.entry(key).or_insert((None, String::new(), String::new()));
                            if let Some(delta) = value.get("delta").and_then(Value::as_str) { entry.2.push_str(delta); }
                        }
                        "response.function_call_arguments.done" => {
                            let key = value.get("item_id").and_then(Value::as_str).unwrap_or("function_call").to_string();
                            let entry = tools.entry(key).or_insert((None, String::new(), String::new()));
                            if let Some(arguments) = value.get("arguments").and_then(Value::as_str) { entry.2 = arguments.to_string(); }
                        }
                        "response.completed" => {
                            if let Some(usage) = value.pointer("response.usage") { emit_responses_usage(usage, &tx); }
                            emit_responses_tools(&mut tools, &tx);
                            let _ = tx.send(StreamSignal::Finished { finish_reason: Some("STOP".to_string()) });
                            finished = true;
                        }
                        "response.failed" | "error" => {
                            let message = value.pointer("response.error.message").and_then(Value::as_str)
                                .or_else(|| value.get("message").and_then(Value::as_str))
                                .unwrap_or("Responses API request failed");
                            let _ = tx.send(StreamSignal::Error(message.to_string()));
                            return;
                        }
                        _ => {
                            if let Some(usage) = value.get("usage") { emit_responses_usage(usage, &tx); }
                        }
                    }
                }
            }
            Err(error) => { let _ = tx.send(StreamSignal::Error(format!("Network stream error: {}", error))); return; }
        }
    }
    if !finished {
        emit_responses_tools(&mut tools, &tx);
        let _ = tx.send(StreamSignal::Finished { finish_reason: Some("STOP".to_string()) });
    }
}

pub fn responses_text(value: &Value) -> Option<String> {
    if let Some(text) = value.get("output_text").and_then(Value::as_str) {
        if !text.is_empty() { return Some(text.to_string()); }
    }
    let mut text = String::new();
    for item in value.get("output").and_then(Value::as_array).into_iter().flatten() {
        for content in item.get("content").and_then(Value::as_array).into_iter().flatten() {
            if let Some(value) = content.get("text").and_then(Value::as_str) { text.push_str(value); }
        }
    }
    (!text.is_empty()).then_some(text)
}

fn emit_responses_tools(tools: &mut BTreeMap<String, (Option<String>, String, String)>, tx: &UnboundedSender<StreamSignal>) {
    for (_, (id, name, arguments)) in std::mem::take(tools) {
        if name.is_empty() { continue; }
        let args = serde_json::from_str(&arguments).unwrap_or_else(|_| json!({ "raw_arguments": arguments }));
        let _ = tx.send(StreamSignal::ToolCall { id, name, args, thought_signature: None });
    }
}

fn emit_responses_usage(usage: &Value, tx: &UnboundedSender<StreamSignal>) {
    let prompt = usage.get("input_tokens").and_then(Value::as_u64).unwrap_or(0) as u32;
    let completion = usage.get("output_tokens").and_then(Value::as_u64).unwrap_or(0) as u32;
    let total = usage.get("total_tokens").and_then(Value::as_u64).unwrap_or(prompt as u64 + completion as u64) as u32;
    let _ = tx.send(StreamSignal::Usage { prompt_tokens: prompt, candidates_tokens: completion, total_tokens: total });
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
        let payload = request_payload("test-model", &request, true, true);
        assert_eq!(payload["model"], "test-model");
        assert_eq!(payload["messages"][0]["role"], "user");
        assert_eq!(payload["messages"][0]["content"], "hello");
        assert_eq!(payload["stream_options"]["include_usage"], true);

        let payload_without_usage = request_payload("test-model", &request, true, false);
        assert!(payload_without_usage.get("stream_options").is_none());
    }

    #[test]
    fn converts_gemini_request_to_responses_payload() {
        let request = GenerateContentRequest {
            contents: vec![Content { role: Some("user".into()), parts: vec![Part::Text { text: "hello".into(), thought: None }] }],
            system_instruction: Some(Content { role: Some("system".into()), parts: vec![Part::Text { text: "be concise".into(), thought: None }] }),
            generation_config: None,
            safety_settings: None,
            tools: None,
        };
        let payload = responses_request_payload("muse-spark-1.3-contributor-free", &request, true);
        assert_eq!(payload["model"], "muse-spark-1.3-contributor-free");
        assert_eq!(payload["instructions"], "be concise");
        assert_eq!(payload["input"][0]["content"][0]["type"], "input_text");
        assert_eq!(payload["input"][0]["content"][0]["text"], "hello");
    }
}

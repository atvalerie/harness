use super::types::{Content, GenerateContentRequest, Part};
use crate::events::StreamSignal;
use futures_util::StreamExt;
use reqwest::Response;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use tokio::sync::mpsc::UnboundedSender;

pub fn request_payload(
    model: &str,
    request: &GenerateContentRequest,
    stream: bool,
    include_usage: bool,
) -> Value {
    let mut messages = Vec::new();
    if let Some(system) = &request.system_instruction {
        messages.extend(content_messages(system));
    }
    for content in &request.contents {
        messages.extend(content_messages(content));
    }

    let mut payload = json!({ "model": model, "messages": messages, "stream": stream });
    if let Some(config) = &request.generation_config {
        if let Some(temp) = config.temperature {
            payload["temperature"] = json!(temp);
        }
        if let Some(max) = config.max_output_tokens {
            payload["max_tokens"] = json!(max);
        }
        if let Some(reasoning_effort) = &config.reasoning_effort {
            payload["reasoning_effort"] = json!(reasoning_effort);
        }
        if let Some(extra) = &config.extra {
            if let Some(extra_object) = extra.as_object() {
                for (key, value) in extra_object {
                    payload[key] = value.clone();
                }
            }
        }
    }
    if stream && include_usage {
        payload["stream_options"] = json!({ "include_usage": true });
    }
    if let Some(tools) = &request.tools {
        let declarations = tools.iter().flat_map(|tool| tool.function_declarations.iter()).map(|f| json!({
            "type": "function",
            "function": { "name": f.name, "description": f.description, "parameters": f.parameters }
        })).collect::<Vec<_>>();
        if !declarations.is_empty() {
            payload["tools"] = json!(declarations);
        }
    }
    payload
}

/// Build an OpenAI Responses API request. Zen uses this endpoint for several
/// model families (including Muse Spark), while other models use Chat
/// Completions. Keeping the conversion beside the Chat adapter makes the
/// provider choice explicit instead of silently forcing one wire format.
pub fn responses_request_payload(
    model: &str,
    request: &GenerateContentRequest,
    stream: bool,
) -> Value {
    let mut input = Vec::new();
    for content in &request.contents {
        let role = match content.role.as_deref() {
            Some("model") => "assistant",
            Some("system") => "system",
            _ => "user",
        };
        let text_type = if role == "assistant" {
            "output_text"
        } else {
            "input_text"
        };
        let mut content_items = Vec::new();
        for part in &content.parts {
            match part {
                Part::Text { text: value, .. } => content_items.push(json!({
                    "type": text_type,
                    "text": value,
                })),
                Part::InlineData { inline_data } => content_items.push(json!({
                    "type": "input_image",
                    "image_url": format!("data:{};base64,{}", inline_data.mime_type, inline_data.data),
                })),
                Part::FunctionCall { function_call, .. } => {
                    // A Responses function call without its original call_id
                    // cannot be paired with a later function_call_output.
                    // Drop malformed legacy history instead of inventing an
                    // id that the provider never issued.
                    if let Some(call_id) = &function_call.id {
                        input.push(json!({
                            "type": "function_call",
                            "call_id": call_id,
                            "name": function_call.name,
                            "arguments": function_call.args.to_string(),
                        }));
                    }
                }
                Part::FunctionResponse { function_response } => {
                    if let Some(call_id) = &function_response.id {
                        input.push(json!({
                            "type": "function_call_output",
                            "call_id": call_id,
                            "output": openai_tool_output(&function_response.response),
                        }));
                    }
                }
            }
        }
        if !content_items.is_empty() {
            input.push(json!({ "role": role, "content": content_items }));
        }
    }

    let mut payload = json!({
        "model": model,
        "input": input,
        "stream": stream,
    });
    if let Some(system) = &request.system_instruction {
        let instructions = system
            .parts
            .iter()
            .filter_map(|part| match part {
                Part::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect::<String>();
        if !instructions.is_empty() {
            payload["instructions"] = json!(instructions);
        }
    }
    if let Some(config) = &request.generation_config {
        if let Some(temp) = config.temperature {
            payload["temperature"] = json!(temp);
        }
        if let Some(max) = config.max_output_tokens {
            payload["max_output_tokens"] = json!(max);
        }
        if let Some(reasoning_effort) = &config.reasoning_effort {
            payload["reasoning"] = json!({ "effort": reasoning_effort });
        }
        if let Some(extra) = &config.extra {
            if let Some(extra_object) = extra.as_object() {
                for (key, value) in extra_object {
                    payload[key] = value.clone();
                }
            }
        }
    }
    if let Some(tools) = &request.tools {
        let declarations = tools
            .iter()
            .flat_map(|tool| tool.function_declarations.iter())
            .map(|f| {
                json!({
                    "type": "function",
                    "name": f.name,
                    "description": f.description,
                    "parameters": f.parameters,
                })
            })
            .collect::<Vec<_>>();
        if !declarations.is_empty() {
            payload["tools"] = json!(declarations);
        }
    }
    payload
}

fn content_messages(content: &Content) -> Vec<Value> {
    let role = match content.role.as_deref() {
        Some("model") => "assistant",
        Some("system") => "system",
        _ => "user",
    };
    let mut messages = Vec::new();
    let mut text = String::new();
    let mut content_items = Vec::new();
    let mut has_image = false;
    let mut tool_calls = Vec::new();
    for part in &content.parts {
        match part {
            Part::Text { text: value, .. } => {
                text.push_str(value);
                content_items.push(json!({ "type": "text", "text": value }));
            }
            Part::InlineData { inline_data } => {
                has_image = true;
                content_items.push(json!({
                "type": "image_url",
                "image_url": { "url": format!("data:{};base64,{}", inline_data.mime_type, inline_data.data) },
                }));
            }
            Part::FunctionCall { function_call, .. } => {
                if let Some(id) = &function_call.id {
                    tool_calls.push(json!({
                        "id": id,
                        "type": "function",
                        "function": { "name": function_call.name, "arguments": function_call.args.to_string() }
                    }));
                }
            }
            Part::FunctionResponse { function_response } => {
                if let Some(id) = &function_response.id {
                    messages.push(json!({
                        "role": "tool",
                        "tool_call_id": id,
                        "name": function_response.name,
                        "content": openai_tool_output(&function_response.response)
                    }));
                }
            }
        }
    }
    if !text.is_empty() || has_image || tool_calls.is_empty() && messages.is_empty() {
        let content = if !has_image {
            json!(text)
        } else {
            json!(content_items)
        };
        let mut message = json!({ "role": role, "content": content });
        if !tool_calls.is_empty() {
            message["tool_calls"] = json!(tool_calls);
        }
        messages.insert(0, message);
    } else if !tool_calls.is_empty() {
        messages.insert(0, json!({ "role": role, "tool_calls": tool_calls }));
    }
    messages
}

/// Holiday stores provider-neutral function responses as an object so Gemini
/// can receive its normal functionResponse shape. OpenAI tool outputs are
/// plain text (or structured content), so unwrap our envelope instead of
/// sending a JSON object encoded inside a string.
fn openai_tool_output(response: &Value) -> String {
    response
        .get("output")
        .and_then(Value::as_str)
        .or_else(|| response.get("error").and_then(Value::as_str))
        .map(str::to_string)
        .unwrap_or_else(|| response.to_string())
}

pub async fn stream_response(response: Response, tx: UnboundedSender<StreamSignal>) {
    let mut stream = response.bytes_stream();
    let mut buffer = Vec::new();
    let mut tools: BTreeMap<usize, (Option<String>, String, String)> = BTreeMap::new();
    while let Some(chunk) = stream.next().await {
        match chunk {
            Ok(bytes) => {
                buffer.extend_from_slice(&bytes);
                while let Some(pos) = buffer.iter().position(|b| *b == b'\n') {
                    let mut line = buffer.drain(..=pos).collect::<Vec<_>>();
                    if line.last() == Some(&b'\n') {
                        line.pop();
                    }
                    if line.last() == Some(&b'\r') {
                        line.pop();
                    }
                    let line = String::from_utf8_lossy(&line);
                    let Some(data) = line.strip_prefix("data:").map(str::trim) else {
                        continue;
                    };
                    if data == "[DONE]" {
                        if !emit_tools(&mut tools, &tx) {
                            return;
                        }
                        let _ = tx.send(StreamSignal::Finished {
                            finish_reason: Some("STOP".to_string()),
                        });
                        return;
                    }
                    if data.is_empty() {
                        continue;
                    }
                    let Ok(value) = serde_json::from_str::<Value>(data) else {
                        let _ = tx.send(StreamSignal::Error(
                            "Malformed JSON in provider stream".to_string(),
                        ));
                        return;
                    };
                    if let Some(error) = value.get("error") {
                        let _ = tx.send(StreamSignal::Error(format!(
                            "Provider stream error: {error}"
                        )));
                        return;
                    }
                    if let Some(usage) = value.get("usage") {
                        emit_usage(usage, &tx);
                    }
                    for choice in value
                        .get("choices")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                    {
                        let delta = choice.get("delta").unwrap_or(&Value::Null);
                        if let Some(text) = delta.get("content").and_then(Value::as_str) {
                            let _ = tx.send(StreamSignal::TextDelta(text.to_string()));
                        }
                        if let Some(thought) =
                            delta.get("reasoning_content").and_then(Value::as_str)
                        {
                            let _ = tx.send(StreamSignal::ThoughtDelta(thought.to_string()));
                        }
                        for call in delta
                            .get("tool_calls")
                            .and_then(Value::as_array)
                            .into_iter()
                            .flatten()
                        {
                            let index =
                                call.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                            let entry =
                                tools
                                    .entry(index)
                                    .or_insert((None, String::new(), String::new()));
                            if let Some(id) = call.get("id").and_then(Value::as_str) {
                                entry.0 = Some(id.to_string());
                            }
                            if let Some(name) =
                                call.pointer("/function/name").and_then(Value::as_str)
                            {
                                entry.1.push_str(name);
                            }
                            if let Some(args) =
                                call.pointer("/function/arguments").and_then(Value::as_str)
                            {
                                entry.2.push_str(args);
                            }
                        }
                        if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
                            if matches!(reason, "length" | "content_filter") {
                                let _ = tx.send(StreamSignal::Error(format!(
                                    "Response incomplete: {reason}"
                                )));
                                return;
                            }
                        }
                    }
                }
            }
            Err(error) => {
                let _ = tx.send(StreamSignal::Error(format!(
                    "Network stream error: {}",
                    error
                )));
                return;
            }
        }
    }
    let _ = tx.send(StreamSignal::Error(
        "Stream ended without a completion event; response is incomplete".to_string(),
    ));
}

pub async fn stream_responses_response(response: Response, tx: UnboundedSender<StreamSignal>) {
    let mut stream = response.bytes_stream();
    let mut buffer = Vec::new();
    let mut tools: BTreeMap<String, (Option<String>, String, String)> = BTreeMap::new();

    while let Some(chunk) = stream.next().await {
        match chunk {
            Ok(bytes) => {
                buffer.extend_from_slice(&bytes);
                while let Some(pos) = buffer.iter().position(|b| *b == b'\n') {
                    let mut line = buffer.drain(..=pos).collect::<Vec<_>>();
                    if line.last() == Some(&b'\n') {
                        line.pop();
                    }
                    if line.last() == Some(&b'\r') {
                        line.pop();
                    }
                    let line = String::from_utf8_lossy(&line);
                    let Some(data) = line.strip_prefix("data:").map(str::trim) else {
                        continue;
                    };
                    if data == "[DONE]" {
                        if !emit_responses_tools(&mut tools, &tx) {
                            return;
                        }
                        let _ = tx.send(StreamSignal::Finished {
                            finish_reason: Some("STOP".to_string()),
                        });
                        return;
                    }
                    if data.is_empty() {
                        continue;
                    }
                    let Ok(value) = serde_json::from_str::<Value>(data) else {
                        let _ = tx.send(StreamSignal::Error(
                            "Malformed JSON in provider stream".to_string(),
                        ));
                        return;
                    };
                    if let Some(error) = value.get("error") {
                        let _ = tx.send(StreamSignal::Error(format!(
                            "Provider stream error: {error}"
                        )));
                        return;
                    }
                    let event_type = value
                        .get("type")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    match event_type {
                        "response.output_text.delta" => {
                            if let Some(delta) = value.get("delta").and_then(Value::as_str) {
                                let _ = tx.send(StreamSignal::TextDelta(delta.to_string()));
                            }
                        }
                        "response.reasoning_summary_text.delta"
                        | "response.reasoning_content.delta" => {
                            if let Some(delta) = value.get("delta").and_then(Value::as_str) {
                                let _ = tx.send(StreamSignal::ThoughtDelta(delta.to_string()));
                            }
                        }
                        "response.output_item.added" | "response.output_item.done" => {
                            if let Some(item) = value.get("item") {
                                if item.get("type").and_then(Value::as_str) == Some("function_call")
                                {
                                    let key = item
                                        .get("id")
                                        .and_then(Value::as_str)
                                        .or_else(|| item.get("call_id").and_then(Value::as_str))
                                        .unwrap_or("function_call")
                                        .to_string();
                                    let entry = tools.entry(key).or_insert((
                                        None,
                                        String::new(),
                                        String::new(),
                                    ));
                                    if let Some(id) = item.get("call_id").and_then(Value::as_str) {
                                        entry.0 = Some(id.to_string());
                                    }
                                    if let Some(name) = item.get("name").and_then(Value::as_str) {
                                        entry.1 = name.to_string();
                                    }
                                    if let Some(arguments) =
                                        item.get("arguments").and_then(Value::as_str)
                                    {
                                        entry.2 = arguments.to_string();
                                    }
                                }
                            }
                        }
                        "response.function_call_arguments.delta" => {
                            let key = value
                                .get("item_id")
                                .and_then(Value::as_str)
                                .unwrap_or("function_call")
                                .to_string();
                            let entry =
                                tools
                                    .entry(key)
                                    .or_insert((None, String::new(), String::new()));
                            if let Some(call_id) = value.get("call_id").and_then(Value::as_str) {
                                entry.0 = Some(call_id.to_string());
                            }
                            if let Some(delta) = value.get("delta").and_then(Value::as_str) {
                                entry.2.push_str(delta);
                            }
                        }
                        "response.function_call_arguments.done" => {
                            let key = value
                                .get("item_id")
                                .and_then(Value::as_str)
                                .unwrap_or("function_call")
                                .to_string();
                            let entry =
                                tools
                                    .entry(key)
                                    .or_insert((None, String::new(), String::new()));
                            if let Some(call_id) = value.get("call_id").and_then(Value::as_str) {
                                entry.0 = Some(call_id.to_string());
                            }
                            if let Some(name) = value.get("name").and_then(Value::as_str) {
                                entry.1 = name.to_string();
                            }
                            if let Some(arguments) = value.get("arguments").and_then(Value::as_str)
                            {
                                entry.2 = arguments.to_string();
                            }
                        }
                        "response.completed" => {
                            if let Some(usage) = value.pointer("/response/usage") {
                                emit_responses_usage(usage, &tx);
                            }
                            if !emit_responses_tools(&mut tools, &tx) {
                                return;
                            }
                            let _ = tx.send(StreamSignal::Finished {
                                finish_reason: Some("STOP".to_string()),
                            });
                            return;
                        }
                        "response.incomplete" => {
                            if let Some(usage) = value.pointer("/response/usage") {
                                emit_responses_usage(usage, &tx);
                            }
                            let reason = value
                                .pointer("/response/incomplete_details/reason")
                                .and_then(Value::as_str)
                                .unwrap_or("unknown reason");
                            let _ = tx.send(StreamSignal::Error(format!(
                                "Response incomplete: {reason}"
                            )));
                            return;
                        }
                        "response.failed" | "error" => {
                            let message = value
                                .pointer("/response/error/message")
                                .and_then(Value::as_str)
                                .or_else(|| value.get("message").and_then(Value::as_str))
                                .unwrap_or("Responses API request failed");
                            let _ = tx.send(StreamSignal::Error(message.to_string()));
                            return;
                        }
                        _ => {
                            if let Some(usage) = value.get("usage") {
                                emit_responses_usage(usage, &tx);
                            }
                        }
                    }
                }
            }
            Err(error) => {
                let _ = tx.send(StreamSignal::Error(format!(
                    "Network stream error: {}",
                    error
                )));
                return;
            }
        }
    }
    let _ = tx.send(StreamSignal::Error(
        "Stream ended without a completion event; response is incomplete".to_string(),
    ));
}

pub fn responses_text(value: &Value) -> Option<String> {
    if let Some(text) = value.get("output_text").and_then(Value::as_str) {
        if !text.is_empty() {
            return Some(text.to_string());
        }
    }
    let mut text = String::new();
    for item in value
        .get("output")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        for content in item
            .get("content")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if let Some(value) = content.get("text").and_then(Value::as_str) {
                text.push_str(value);
            }
        }
    }
    (!text.is_empty()).then_some(text)
}

fn emit_responses_tools(
    tools: &mut BTreeMap<String, (Option<String>, String, String)>,
    tx: &UnboundedSender<StreamSignal>,
) -> bool {
    emit_validated_tools(std::mem::take(tools).into_values(), tx)
}

fn emit_validated_tools(
    tools: impl Iterator<Item = (Option<String>, String, String)>,
    tx: &UnboundedSender<StreamSignal>,
) -> bool {
    let mut validated = Vec::new();
    for (id, name, arguments) in tools {
        let args = serde_json::from_str::<Value>(&arguments);
        if name.is_empty()
            || id.as_deref().is_none_or(str::is_empty)
            || !matches!(&args, Ok(Value::Object(_)))
        {
            let _ = tx.send(StreamSignal::Error(
                "Provider returned an incomplete or malformed tool call".to_string(),
            ));
            return false;
        }
        validated.push(StreamSignal::ToolCall {
            id,
            name,
            args: args.unwrap(),
            thought_signature: None,
        });
    }
    for call in validated {
        let _ = tx.send(call);
    }
    true
}

fn emit_responses_usage(usage: &Value, tx: &UnboundedSender<StreamSignal>) {
    if let Some(usage) = crate::usage::TokenUsage::openai(usage, true) {
        let _ = tx.send(usage.signal());
    }
}

fn emit_tools(
    tools: &mut BTreeMap<usize, (Option<String>, String, String)>,
    tx: &UnboundedSender<StreamSignal>,
) -> bool {
    emit_validated_tools(std::mem::take(tools).into_values(), tx)
}

fn emit_usage(usage: &Value, tx: &UnboundedSender<StreamSignal>) {
    if let Some(usage) = crate::usage::TokenUsage::openai(usage, false) {
        let _ = tx.send(usage.signal());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::types::{Content, GenerateContentRequest, Part};

    #[tokio::test]
    async fn cache_usage_survives_stream_and_null_is_ignored() {
        let signals = fixture(concat!(
            "data: {\"usage\":null,\"choices\":[]}\n\n",
            "data: {\"usage\":{\"prompt_tokens\":100,\"completion_tokens\":20,\"prompt_tokens_details\":{\"cached_tokens\":80},\"completion_tokens_details\":{\"reasoning_tokens\":5}},\"choices\":[]}\n\n",
            "data: [DONE]\n\n"
        ), false).await;
        let usage = signals
            .iter()
            .filter_map(|s| match s {
                StreamSignal::Usage { details, .. } => Some(details),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(usage.len(), 1);
        assert_eq!(usage[0].cache_read_tokens, Some(80));
        assert_eq!(usage[0].reasoning_tokens, Some(5));
        assert_eq!(usage[0].total(), Some(120));
    }

    async fn fixture(body: &'static str, responses: bool) -> Vec<StreamSignal> {
        let response = reqwest::Response::from(http::Response::new(body));
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        if responses {
            stream_responses_response(response, tx).await;
        } else {
            stream_response(response, tx).await;
        }
        let mut signals = Vec::new();
        while let Ok(signal) = rx.try_recv() {
            signals.push(signal);
        }
        signals
    }

    #[tokio::test]
    async fn eof_without_terminal_event_is_not_success() {
        for responses in [false, true] {
            let signals = fixture("data: {}\n\n", responses).await;
            assert!(signals.iter().any(|s| matches!(s, StreamSignal::Error(_))));
            assert!(!signals
                .iter()
                .any(|s| matches!(s, StreamSignal::Finished { .. })));
        }
    }

    #[tokio::test]
    async fn incomplete_responses_preserve_usage_but_do_not_dispatch_tools() {
        let signals = fixture(concat!(
            "data: {\"type\":\"response.output_item.added\",\"item\":{\"type\":\"function_call\",\"id\":\"item1\",\"call_id\":\"call1\",\"name\":\"run_command\",\"arguments\":\"{}\"}}\n\n",
            "data: {\"type\":\"response.incomplete\",\"response\":{\"usage\":{\"input_tokens\":10,\"output_tokens\":5},\"incomplete_details\":{\"reason\":\"max_output_tokens\"}}}\n\n"
        ), true).await;
        assert!(signals.iter().any(|s| matches!(
            s,
            StreamSignal::Usage {
                prompt_tokens: 10,
                ..
            }
        )));
        assert!(signals
            .iter()
            .any(|s| matches!(s, StreamSignal::Error(e) if e.contains("max_output_tokens"))));
        assert!(!signals.iter().any(|s| matches!(
            s,
            StreamSignal::Finished { .. } | StreamSignal::ToolCall { .. }
        )));
    }

    #[tokio::test]
    async fn responses_finish_exactly_once() {
        let signals = fixture(
            "data: {\"type\":\"response.completed\"}\n\ndata: [DONE]\n\n",
            true,
        )
        .await;
        assert_eq!(
            signals
                .iter()
                .filter(|s| matches!(s, StreamSignal::Finished { .. }))
                .count(),
            1
        );
        assert!(!signals.iter().any(|s| matches!(s, StreamSignal::Error(_))));
    }

    #[tokio::test]
    async fn malformed_stream_json_fails_closed() {
        for responses in [false, true] {
            let signals = fixture("data: {bad json}\n\ndata: [DONE]\n\n", responses).await;
            assert!(matches!(signals.as_slice(), [StreamSignal::Error(_)]));
        }
    }

    #[tokio::test]
    async fn chat_length_limit_is_incomplete_even_with_done_marker() {
        let signals = fixture("data: {\"choices\":[{\"delta\":{\"content\":\"partial\"},\"finish_reason\":\"length\"}]}\n\ndata: [DONE]\n\n", false).await;
        assert!(matches!(
            signals.as_slice(),
            [StreamSignal::TextDelta(_), StreamSignal::Error(_)]
        ));
    }

    #[test]
    fn review_json_contract_is_present_in_both_protocols() {
        let prompt = include_str!("../prompts/auto_review.md");
        let request = super::GenerateContentRequest {
            contents: vec![],
            system_instruction: Some(super::Content {
                role: Some("system".into()),
                parts: vec![super::Part::Text {
                    text: prompt.into(),
                    thought: None,
                }],
            }),
            generation_config: None,
            safety_settings: None,
            tools: None,
        };
        let chat = super::request_payload("codex-auto-review", &request, false, false);
        assert_eq!(chat["messages"][0]["role"], "system");
        assert_eq!(chat["messages"][0]["content"], prompt);
        let responses = super::responses_request_payload("codex-auto-review", &request, false);
        assert_eq!(responses["instructions"], prompt);
        assert!(prompt.contains("exactly one JSON object"));
    }

    #[test]
    fn malformed_tool_batch_dispatches_nothing() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let tools = vec![
            (Some("a".into()), "read_file".into(), "{}".into()),
            (
                Some("b".into()),
                "run_command".into(),
                "{\"command\":".into(),
            ),
        ];
        assert!(!emit_validated_tools(tools.into_iter(), &tx));
        assert!(matches!(rx.try_recv().unwrap(), StreamSignal::Error(_)));
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn complete_chat_stream_succeeds() {
        let signals = fixture(
            "data: {\"choices\":[{\"delta\":{\"content\":\"hello\"}}]}\n\ndata: [DONE]\n\n",
            false,
        )
        .await;
        assert!(
            matches!(signals.as_slice(), [StreamSignal::TextDelta(t), StreamSignal::Finished { .. }] if t == "hello")
        );
    }

    #[test]
    fn converts_gemini_request_to_openai_chat_payload() {
        let request = GenerateContentRequest {
            contents: vec![Content {
                role: Some("user".into()),
                parts: vec![Part::Text {
                    text: "hello".into(),
                    thought: None,
                }],
            }],
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
            contents: vec![Content {
                role: Some("user".into()),
                parts: vec![Part::Text {
                    text: "hello".into(),
                    thought: None,
                }],
            }],
            system_instruction: Some(Content {
                role: Some("system".into()),
                parts: vec![Part::Text {
                    text: "be concise".into(),
                    thought: None,
                }],
            }),
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

    #[test]
    fn preserves_responses_function_call_ids_for_tool_outputs() {
        let request = GenerateContentRequest {
            contents: vec![
                Content {
                    role: Some("model".into()),
                    parts: vec![Part::FunctionCall {
                        function_call: crate::client::types::FunctionCallPayload {
                            name: "run_command".into(),
                            args: json!({"command": "dir"}),
                            id: Some("call_123".into()),
                        },
                        thought_signature: None,
                    }],
                },
                Content {
                    role: Some("user".into()),
                    parts: vec![Part::FunctionResponse {
                        function_response: crate::client::types::FunctionResponsePayload {
                            name: "run_command".into(),
                            response: json!({"output": "ok"}),
                            id: Some("call_123".into()),
                        },
                    }],
                },
            ],
            system_instruction: None,
            generation_config: None,
            safety_settings: None,
            tools: None,
        };
        let payload = responses_request_payload("muse-spark-1.3-contributor-free", &request, false);
        assert_eq!(payload["input"][0]["type"], "function_call");
        assert_eq!(payload["input"][0]["call_id"], "call_123");
        assert_eq!(payload["input"][1]["type"], "function_call_output");
        assert_eq!(payload["input"][1]["call_id"], "call_123");
        assert_eq!(payload["input"][1]["output"], "ok");
    }

    #[test]
    fn does_not_invent_or_emit_empty_openai_tool_call_ids() {
        let request = GenerateContentRequest {
            contents: vec![Content {
                role: Some("model".into()),
                parts: vec![Part::FunctionCall {
                    function_call: crate::client::types::FunctionCallPayload {
                        name: "run_command".into(),
                        args: json!({"command": "dir"}),
                        id: None,
                    },
                    thought_signature: None,
                }],
            }],
            system_instruction: None,
            generation_config: None,
            safety_settings: None,
            tools: None,
        };
        let payload = responses_request_payload("gpt-test", &request, false);
        assert!(payload["input"].as_array().unwrap().is_empty());
    }
}

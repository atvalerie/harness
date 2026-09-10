use super::types::GenerateContentResponse;
use crate::events::StreamSignal;
use futures_util::StreamExt;
use reqwest::Response;
use tokio::sync::mpsc::UnboundedSender;

pub struct SseParser {
    buffer: Vec<u8>,
    finished: bool,
}

impl SseParser {
    pub fn new() -> Self {
        Self {
            buffer: Vec::new(),
            finished: false,
        }
    }

    pub fn is_finished(&self) -> bool {
        self.finished
    }

    /// Process an arbitrary incoming chunk of bytes from the HTTP stream.
    /// Buffers bytes until full lines separated by '\n' are formed to prevent splitting multi-byte UTF-8 sequences.
    pub fn process_chunk(&mut self, chunk: &[u8], tx: &UnboundedSender<StreamSignal>) {
        self.buffer.extend_from_slice(chunk);

        while let Some(pos) = self.buffer.iter().position(|&b| b == b'\n') {
            let mut line_bytes = self.buffer[..pos].to_vec();
            self.buffer.drain(..=pos);

            // Strip trailing \r if present
            if line_bytes.ends_with(b"\r") {
                line_bytes.pop();
            }

            let line = match std::str::from_utf8(&line_bytes) {
                Ok(s) => s.trim(),
                Err(_) => continue,
            };

            if line.is_empty() {
                continue;
            }

            if let Some(data_str) = line.strip_prefix("data:") {
                let json_str = data_str.trim();
                if json_str.is_empty() {
                    continue;
                }

                if json_str == "[DONE]" {
                    self.finished = true;
                    let _ = tx.send(StreamSignal::Finished {
                        finish_reason: Some("STOP".to_string()),
                    });
                    continue;
                }

                match serde_json::from_str::<GenerateContentResponse>(json_str) {
                    Ok(resp) => {
                        if let Some(err) = resp.error {
                            let formatted =
                                super::format_api_error(err.code.map(|c| c as u16), json_str);
                            let _ = tx.send(StreamSignal::Error(formatted));
                            continue;
                        }

                        if let Some(usage) = resp.usage_metadata {
                            let _ = tx.send(StreamSignal::Usage {
                                prompt_tokens: usage.prompt_token_count.unwrap_or(0),
                                candidates_tokens: usage.candidates_token_count.unwrap_or(0),
                                total_tokens: usage.total_token_count.unwrap_or(0),
                            });
                        }

                        if let Some(candidates) = resp.candidates {
                            for cand in candidates {
                                if let Some(content) = cand.content {
                                    if let Some(parts) = content.parts {
                                        for part in parts {
                                            if let Some(fc) = part.function_call {
                                                let _ = tx.send(StreamSignal::ToolCall {
                                                    id: fc.id,
                                                    name: fc.name,
                                                    args: fc.args,
                                                    thought_signature: part.thought_signature,
                                                });
                                            } else if let Some(text) = part.text {
                                                if part.thought.unwrap_or(false) {
                                                    let _ =
                                                        tx.send(StreamSignal::ThoughtDelta(text));
                                                } else {
                                                    let _ = tx.send(StreamSignal::TextDelta(text));
                                                }
                                            }
                                        }
                                    }
                                }

                                if let Some(finish_reason) = cand.finish_reason {
                                    self.finished = true;
                                    let _ = tx.send(StreamSignal::Finished {
                                        finish_reason: Some(finish_reason),
                                    });
                                }
                            }
                        }
                    }
                    Err(e) => {
                        let _ = tx.send(StreamSignal::Error(format!(
                            "JSON Parse Error: {} on payload: {}",
                            e, json_str
                        )));
                    }
                }
            }
        }
    }
}

pub async fn stream_sse_response(response: Response, tx: UnboundedSender<StreamSignal>) {
    let mut parser = SseParser::new();
    let mut stream = response.bytes_stream();
    let mut failed = false;

    while let Some(chunk_res) = stream.next().await {
        match chunk_res {
            Ok(chunk) => {
                parser.process_chunk(&chunk, &tx);
            }
            Err(e) => {
                failed = true;
                let _ = tx.send(StreamSignal::Error(format!("Network stream error: {}", e)));
                break;
            }
        }
    }
    if !failed {
        // Some gateways close the connection without a trailing newline or a
        // finishReason. Flush the final line and guarantee a terminal event.
        parser.process_chunk(b"\n", &tx);
        if !parser.is_finished() {
            let _ = tx.send(StreamSignal::Error(
                "Stream ended without a completion event; response is incomplete".to_string(),
            ));
        }
    }
}

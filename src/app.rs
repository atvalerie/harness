use crate::client::types::{Content, GenerationConfig, Part, SafetySetting, ThinkingConfig};
use crate::client::{AiClient, ProviderKind};
use crate::config::AppConfig;
use crate::events::{AppEvent, StreamSignal};
use crate::tools::{ToolPreview, ToolRegistry};
use crate::session::{self, SessionSnapshot};
use serde_json::json;
use std::collections::HashSet;
use tokio::sync::mpsc::UnboundedSender;
use tokio::task::JoinHandle;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineState {
    Idle,
    Streaming,
    Compacting,
    AwaitingHitlApproval,
    ExecutingTool,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ChatMessage {
    pub role: String, // "user", "model", "thought", "tool", "system"
    pub content: String,
    pub timestamp: String,
}

#[derive(Debug, Clone)]
pub struct PendingToolCall {
    pub call_id: Option<String>,
    pub tool_name: String,
    pub args: serde_json::Value,
    pub preview: ToolPreview,
}

pub struct App {
    pub config: AppConfig,
    pub client: AiClient,
    pub tool_registry: ToolRegistry,
    pub state: EngineState,
    pub messages: Vec<ChatMessage>,
    pub chat_scroll: usize,
    pub input_buffer: String,
    pub input_cursor: usize,
    pub input_history: Vec<String>,
    pub input_history_idx: Option<usize>,

    // Active token metrics
    pub prompt_tokens: u32,
    pub candidates_tokens: u32,
    pub total_tokens: u32,

    // Streaming state
    pub active_stream_task: Option<JoinHandle<()>>,
    pub current_thought_buffer: String,
    pub current_response_buffer: String,

    // HITL Modal State
    pub pending_tool_call: Option<PendingToolCall>,
    pub session_allowed_tools: HashSet<String>,
    pub modal_scroll: usize,

    // Models list cached
    pub available_models: Vec<crate::client::types::ModelInfo>,
    pub show_models_modal: bool,
    pub models_scroll: usize,
    pub models_selected: usize,
    pub show_sessions_modal: bool,
    pub available_sessions: Vec<session::SessionInfo>,
    pub sessions_selected: usize,

    // Metrics & Performance
    pub stream_epoch: u64,
    pub stream_start_time: Option<std::time::Instant>,
    pub candidate_chunks_count: u32,
    pub current_tps: f64,

    // Status bar notification
    pub status_message: Option<String>,
    pub should_quit: bool,
    pub pending_generation_after_compaction: bool,
    pub pending_prompt_after_compaction: Option<String>,
    pub session_path: Option<std::path::PathBuf>,
    pub session_messages_at_save: usize,
}

impl App {
    pub fn new(config: AppConfig, api_key: String) -> Self {
        let client = AiClient::from_config(api_key.clone(), &config);
        let tool_registry = ToolRegistry::new();
        let session_path = session::new_session_path(&config.session_name);

        Self {
            config,
            client,
            tool_registry,
            state: EngineState::Idle,
            messages: Vec::new(),
            chat_scroll: 0,
            input_buffer: String::new(),
            input_cursor: 0,
            input_history: Vec::new(),
            input_history_idx: None,
            prompt_tokens: 0,
            candidates_tokens: 0,
            total_tokens: 0,
            active_stream_task: None,
            current_thought_buffer: String::new(),
            current_response_buffer: String::new(),
            pending_tool_call: None,
            session_allowed_tools: HashSet::new(),
            modal_scroll: 0,
            available_models: Vec::new(),
            show_models_modal: false,
            models_scroll: 0,
            models_selected: 0,
            show_sessions_modal: false,
            available_sessions: Vec::new(),
            sessions_selected: 0,
            stream_epoch: 0,
            stream_start_time: None,
            candidate_chunks_count: 0,
            current_tps: 0.0,
            status_message: Some("Ready".to_string()),
            should_quit: false,
            pending_generation_after_compaction: false,
            pending_prompt_after_compaction: None,
            session_path,
            session_messages_at_save: 0,
        }
    }

    pub fn set_status(&mut self, msg: impl Into<String>) {
        self.status_message = Some(msg.into());
    }

    pub fn selected_model_id(&self) -> Option<String> {
        self.available_models.get(self.models_selected).map(|model| model.id.clone())
    }

    pub fn refresh_sessions(&mut self) {
        self.available_sessions = session::list();
        self.sessions_selected = 0;
    }

    pub fn resume_selected_session(&mut self) {
        let Some(info) = self.available_sessions.get(self.sessions_selected).cloned() else { return };
        if let Some(snapshot) = session::load(&info.path) {
            self.messages = snapshot.messages;
            self.config.provider = snapshot.provider;
            self.config.model = snapshot.model;
            self.client.update_provider(ProviderKind::parse(&self.config.provider), self.config.base_url.clone());
            self.session_path = Some(info.path.clone());
            self.chat_scroll = 0;
            self.session_messages_at_save = self.messages.len();
            self.show_sessions_modal = false;
            self.add_message("system", format!("Resumed session '{}'.", info.name));
        }
    }

    pub fn delete_selected_session(&mut self) {
        let Some(info) = self.available_sessions.get(self.sessions_selected).cloned() else { return };
        if std::fs::remove_file(&info.path).is_ok() {
            self.refresh_sessions();
            self.set_status(format!("Deleted session '{}'", info.name));
        }
    }

    pub fn export_selected_session(&mut self) {
        let Some(info) = self.available_sessions.get(self.sessions_selected).cloned() else { return };
        let export_path = info.path.with_file_name(format!("{}.export.json", info.name));
        if let Ok(snapshot) = std::fs::read_to_string(&info.path) {
            if std::fs::write(&export_path, snapshot).is_ok() { self.set_status(format!("Exported session to {}", export_path.display())); }
        }
    }

    pub fn select_model_from_catalog(&mut self) {
        if let Some(model) = self.selected_model_id() {
            self.config.model = model.clone();
            self.set_status(format!("Active model: {}", model));
        }
    }

    pub fn toggle_selected_reasoning(&mut self) {
        let Some(model) = self.selected_model_id() else { return };
        let key = format!("{}:{}", self.config.provider, model);
        let profile = self.config.model_profiles.entry(key).or_default();
        let enabled = !profile.reasoning_enabled.unwrap_or(self.config.thinking_budget > 0);
        profile.reasoning_enabled = Some(enabled);
        self.add_message("system", format!("{} reasoning {}", model, if enabled { "enabled" } else { "disabled" }));
    }

    pub fn adjust_selected_thinking(&mut self, delta: i32) {
        let Some(model) = self.selected_model_id() else { return };
        let key = format!("{}:{}", self.config.provider, model);
        let profile = self.config.model_profiles.entry(key).or_default();
        let current = profile.thinking_budget.unwrap_or(self.config.thinking_budget);
        profile.thinking_budget = Some((current + delta).max(0));
        profile.reasoning_enabled = Some(profile.thinking_budget != Some(0));
    }

    pub fn adjust_selected_temperature(&mut self, delta: f32) {
        let Some(model) = self.selected_model_id() else { return };
        let key = format!("{}:{}", self.config.provider, model);
        let profile = self.config.model_profiles.entry(key).or_default();
        let current = profile.temperature.unwrap_or(self.config.temperature);
        profile.temperature = Some((current + delta).clamp(0.0, 2.0));
    }

    pub fn adjust_selected_max_tokens(&mut self, delta: i32) {
        let Some(model) = self.selected_model_id() else { return };
        let key = format!("{}:{}", self.config.provider, model);
        let profile = self.config.model_profiles.entry(key).or_default();
        let current = profile.max_output_tokens.unwrap_or(8192) as i32;
        profile.max_output_tokens = Some((current + delta).max(256) as u32);
    }

    pub fn toggle_selected_fallback(&mut self) {
        let Some(model) = self.selected_model_id() else { return };
        let provider_name = self.config.provider.clone();
        let provider = self.config.providers.entry(provider_name.clone()).or_insert_with(|| crate::config::ProviderConfig {
            kind: provider_name.clone(),
            base_url: self.config.base_url.clone(),
            fallback_models: Vec::new(),
            api_key_env: None,
        });
        if let Some(position) = provider.fallback_models.iter().position(|candidate| candidate == &model) {
            provider.fallback_models.remove(position);
        } else {
            provider.fallback_models.push(model);
        }
    }

    pub fn add_message(&mut self, role: &str, content: impl Into<String>) {
        let now = chrono::Local::now().format("%H:%M:%S").to_string();
        self.messages.push(ChatMessage {
            role: role.to_string(),
            content: content.into(),
            timestamp: now,
        });
        self.chat_scroll = 0; // Stick to bottom
    }

    pub fn flush_session(&mut self) -> Result<(), String> {
        let Some(path) = &self.session_path else { return Ok(()) };
        let snapshot = SessionSnapshot { provider: self.config.provider.clone(), model: self.config.model.clone(), messages: self.messages.clone() };
        session::save(path, &snapshot)?;
        self.session_messages_at_save = self.messages.len();
        Ok(())
    }

    pub fn handle_enter(&mut self, tx: UnboundedSender<AppEvent>) {
        if self.state != EngineState::Idle {
            self.set_status("Engine busy. Press Esc to cancel active stream.");
            return;
        }

        let text = self.input_buffer.trim().to_string();
        if text.is_empty() {
            return;
        }

        self.input_history.push(text.clone());
        self.input_history_idx = None;
        self.input_buffer.clear();
        self.input_cursor = 0;

        if text.starts_with('/') {
            self.handle_slash_command(&text, tx);
            return;
        }

        self.add_message("user", text);
        let _ = self.flush_session();
        self.trigger_generation(tx);
    }

    pub fn handle_slash_command(&mut self, command_line: &str, tx: UnboundedSender<AppEvent>) {
        let mut parts = command_line.splitn(2, ' ');
        let cmd = parts.next().unwrap_or("").to_lowercase();
        let arg = parts.next().map(|s| s.trim()).unwrap_or("");

        match cmd.as_str() {
            "/help" => {
                self.add_message("system", 
                    "Available Commands:\n\
                    - /compact : Summarize conversation history to reclaim context window\n\
                    - /models : Fetch live models & pricing from the active provider\n\
                    - /provider <gemini|openai> : Select API protocol/provider\n\
                    - /baseurl <url|default> : Set a custom OpenAI-compatible API base URL\n\
                    - /model <name> : Switch active model (e.g. /model gemini-3.6-flash)\n\
                    - /thinking <budget> : Set thinking token budget (0 to disable, 1024, 2048, 4096)\n\
                    - /reasoning <on|off|budget> : Toggle or set reasoning for the active model\n\
                    - /autocompact <on|off|tokens> : Configure automatic context compaction\n\
                    - /session <save|clear|path> : Manage the low-write resumable session\n\
                    - /sessions : Browse, resume, export, or delete sessions\n\
                    - /temp <float> : Adjust temperature (0.0 to 2.0)\n\
                    - /sys <instruction> : Update system prompt\n\
                    - /key <api_key> : Save the active provider API key\n\
                    - /copy : Copy last assistant response to system clipboard (or Ctrl+Y)\n\
                    - /clear : Clear conversation history\n\
                    - /save : Save config to disk\n\
                    - /quit : Exit application"
                );
            }
            "/compact" => {
                self.compact_history(tx);
            }
            "/models" => {
                self.add_message("system", format!("Fetching models from {}...", self.config.provider));
                let client = self.client.clone();
                tokio::spawn(async move {
                    let res = client.list_models().await;
                    let _ = tx.send(AppEvent::ModelsFetched(res));
                });
            }
            "/provider" => {
                if arg.is_empty() {
                    self.add_message("system", format!("Current provider: {}", self.config.provider));
                } else {
                    let provider = ProviderKind::parse(arg);
                    self.config.provider = provider.as_str().to_string();
                    self.client.update_provider(provider, self.config.base_url.clone());
                    self.set_status(format!("Provider set to {}", self.config.provider));
                    self.add_message("system", format!("Provider set to {}", self.config.provider));
                }
            }
            "/baseurl" => {
                if arg.is_empty() {
                    self.add_message("system", format!("Current base URL: {}", self.config.base_url.as_deref().unwrap_or("provider default")));
                } else {
                    self.config.base_url = if arg.eq_ignore_ascii_case("default") { None } else { Some(arg.to_string()) };
                    self.client.update_provider(ProviderKind::parse(&self.config.provider), self.config.base_url.clone());
                    self.set_status("Provider base URL updated");
                    self.add_message("system", "Provider base URL updated. Use /save to persist it.");
                }
            }
            "/model" => {
                if arg.is_empty() {
                    self.add_message("system", format!("Current model: {}", self.config.model));
                } else {
                    self.config.model = arg.to_string();
                    self.set_status(format!("Model set to: {}", self.config.model));
                    self.add_message("system", format!("Active model switched to: {}", self.config.model));
                }
            }
            "/thinking" => {
                if let Ok(b) = arg.parse::<i32>() {
                    self.config.thinking_budget = b;
                    self.set_status(format!("Thinking budget set to: {}", b));
                    self.add_message("system", format!("Thinking budget set to: {} tokens", b));
                } else {
                    self.add_message("system", format!("Current thinking budget: {} tokens. Use /thinking <int>", self.config.thinking_budget));
                }
            }
            "/reasoning" => {
                let key = self.config.model_profile_key();
                let profile = self.config.model_profiles.entry(key).or_default();
                if arg.is_empty() {
                    let enabled = profile.reasoning_enabled.unwrap_or(self.config.thinking_budget > 0);
                    self.add_message("system", format!("Reasoning for {}: {}", self.config.model, if enabled { "on" } else { "off" }));
                } else if arg.eq_ignore_ascii_case("on") || arg.eq_ignore_ascii_case("off") {
                    let enabled = arg.eq_ignore_ascii_case("on");
                    profile.reasoning_enabled = Some(enabled);
                    let model = self.config.model.clone();
                    let status = if enabled { "enabled" } else { "disabled" };
                    let _ = profile;
                    self.set_status(format!("Reasoning {} for {}", status, model));
                } else if let Ok(budget) = arg.parse::<i32>() {
                    profile.thinking_budget = Some(budget.max(0));
                    profile.reasoning_enabled = Some(budget > 0);
                    self.set_status(format!("Reasoning budget set to {} for {}", budget.max(0), self.config.model));
                } else {
                    self.add_message("system", "Usage: /reasoning <on|off|token-budget>");
                }
            }
            "/autocompact" => {
                if arg.is_empty() {
                    self.add_message("system", format!("Automatic compaction: {} at ~{} tokens", if self.config.auto_compact { "on" } else { "off" }, self.config.auto_compact_threshold_tokens));
                } else if arg.eq_ignore_ascii_case("on") || arg.eq_ignore_ascii_case("off") {
                    self.config.auto_compact = arg.eq_ignore_ascii_case("on");
                    self.set_status(format!("Automatic compaction {}", if self.config.auto_compact { "enabled" } else { "disabled" }));
                } else if let Ok(tokens) = arg.parse::<u64>() {
                    self.config.auto_compact_threshold_tokens = tokens.max(1_000);
                    self.config.auto_compact = true;
                    self.set_status(format!("Automatic compaction threshold set to {} tokens", self.config.auto_compact_threshold_tokens));
                } else {
                    self.add_message("system", "Usage: /autocompact <on|off|token-threshold>");
                }
            }
            "/session" => {
                match arg.to_ascii_lowercase().as_str() {
                    "save" => match self.flush_session() {
                        Ok(()) => self.add_message("system", "Session saved."),
                        Err(error) => self.add_message("system", format!("Session save failed: {}", error)),
                    },
                    "clear" => {
                        self.messages.clear();
                        self.session_messages_at_save = 0;
                        let _ = self.flush_session();
                        self.add_message("system", "Session cleared and saved.");
                    }
                    "path" => self.add_message("system", format!("Session path: {}", self.session_path.as_ref().map(|path| path.display().to_string()).unwrap_or_else(|| "disabled".to_string()))),
                    _ => self.add_message("system", "Usage: /session <save|clear|path>"),
                }
            }
            "/sessions" => {
                self.refresh_sessions();
                self.show_sessions_modal = true;
            }
            "/temp" => {
                if let Ok(t) = arg.parse::<f32>() {
                    self.config.temperature = t.clamp(0.0, 2.0);
                    self.set_status(format!("Temperature set to: {:.2}", self.config.temperature));
                    self.add_message("system", format!("Temperature set to: {:.2}", self.config.temperature));
                } else {
                    self.add_message("system", format!("Current temperature: {:.2}. Use /temp <float>", self.config.temperature));
                }
            }
            "/sys" => {
                if arg.is_empty() {
                    self.add_message("system", format!("Current system prompt:\n{}", self.config.system_instruction));
                } else {
                    self.config.system_instruction = arg.to_string();
                    self.set_status("System instruction updated");
                    self.add_message("system", "System instruction updated.");
                }
            }
            "/key" => {
                if arg.is_empty() {
                    self.add_message("system", "Usage: /key <your_provider_api_key>");
                } else {
                    match AppConfig::set_api_key(arg) {
                        Ok(()) => {
                            self.client.update_api_key(arg.to_string());
                            self.set_status("API key updated successfully");
                            self.add_message("system", "Provider API key updated and stored securely.");
                        }
                        Err(e) => {
                            self.add_message("system", format!("Failed to store key: {}", e));
                        }
                    }
                }
            }
            "/clear" => {
                self.messages.clear();
                self.chat_scroll = 0;
                self.prompt_tokens = 0;
                self.candidates_tokens = 0;
                self.total_tokens = 0;
                self.set_status("Session cleared");
                self.add_message("system", "Conversation history cleared.");
            }
            "/save" => {
                match self.config.save() {
                    Ok(()) => {
                        self.set_status("Configuration saved");
                        self.add_message("system", "Configuration saved to disk.");
                    }
                    Err(e) => {
                        self.add_message("system", format!("Error saving configuration: {}", e));
                    }
                }
            }
            "/copy" => {
                self.copy_last_response();
            }
            "/quit" => {
                self.should_quit = true;
            }
            _ => {
                self.add_message("system", format!("Unknown command: '{}'. Type /help for assistance.", cmd));
            }
        }
    }

    pub fn copy_last_response(&mut self) {
        // Find last assistant message
        let last_model_msg = self
            .messages
            .iter()
            .rev()
            .find(|m| m.role == "model")
            .map(|m| m.content.clone());

        match last_model_msg {
            Some(text) => {
                let char_len = text.len();
                tokio::spawn(async move {
                    #[cfg(windows)]
                    {
                        use tokio::io::AsyncWriteExt;
                        use tokio::process::Command;
                        use std::process::Stdio;

                        if let Ok(mut child) = Command::new("clip")
                            .stdin(Stdio::piped())
                            .spawn()
                        {
                            if let Some(mut stdin) = child.stdin.take() {
                                let _ = stdin.write_all(text.as_bytes()).await;
                            }
                            let _ = child.wait().await;
                        }
                    }

                    #[cfg(not(windows))]
                    {
                        use tokio::io::AsyncWriteExt;
                        use tokio::process::Command;
                        use std::process::Stdio;

                        if let Ok(mut child) = Command::new("xclip")
                            .arg("-selection")
                            .arg("clipboard")
                            .stdin(Stdio::piped())
                            .spawn()
                        {
                            if let Some(mut stdin) = child.stdin.take() {
                                let _ = stdin.write_all(text.as_bytes()).await;
                            }
                            let _ = child.wait().await;
                        }
                    }
                });

                self.set_status("Copied response to clipboard");
                self.add_message("system", format!("Copied last assistant response ({} chars) to system clipboard.", char_len));
            }
            None => {
                self.add_message("system", "No assistant response found to copy.");
            }
        }
    }

    pub fn trigger_generation(&mut self, tx: UnboundedSender<AppEvent>) {
        if self.state != EngineState::Idle {
            return;
        }

        if self.config.auto_compact
            && self.estimated_context_tokens() >= self.config.auto_compact_threshold_tokens
        {
            self.pending_generation_after_compaction = true;
            self.pending_prompt_after_compaction = self.messages.iter().rev().find(|message| message.role == "user").map(|message| message.content.clone());
            self.state = EngineState::Compacting;
            self.compact_history(tx);
            return;
        }

        self.stream_epoch += 1;
        let epoch = self.stream_epoch;

        if let Some(handle) = self.active_stream_task.take() {
            handle.abort();
        }

        self.state = EngineState::Streaming;
        self.set_status(format!("Streaming ({})", self.config.model));
        self.current_thought_buffer.clear();
        self.current_response_buffer.clear();
        self.stream_start_time = Some(std::time::Instant::now());
        self.candidate_chunks_count = 0;
        self.current_tps = 0.0;

        let client = self.client.clone();
        let model = self.config.model.clone();
        let fallback_models = self.config.effective_fallback_models();
        let max_retries = self.config.max_retries;
        let request = self.build_request();

        let (stream_tx, mut stream_rx) = tokio::sync::mpsc::unbounded_channel::<StreamSignal>();

        let stream_handle = tokio::spawn(async move {
            client.stream_generate_content(&model, &fallback_models, max_retries, &request, stream_tx).await;
        });

        self.active_stream_task = Some(stream_handle);

        let app_tx = tx.clone();
        tokio::spawn(async move {
            while let Some(sig) = stream_rx.recv().await {
                let _ = app_tx.send(AppEvent::Stream { epoch, signal: sig });
            }
        });
    }

    fn estimated_context_tokens(&self) -> u64 {
        let message_chars: usize = self.messages.iter().map(|message| message.content.len()).sum();
        ((message_chars + self.config.system_instruction.len()) as u64 / 4).max(self.total_tokens as u64)
    }

    pub fn cancel_generation(&mut self) {
        if let Some(handle) = self.active_stream_task.take() {
            handle.abort();
        }

        if !self.current_thought_buffer.is_empty() {
            let thought = std::mem::take(&mut self.current_thought_buffer);
            self.add_message("thought", thought);
        }

        if !self.current_response_buffer.is_empty() {
            let resp = std::mem::take(&mut self.current_response_buffer);
            self.add_message("model", resp);
        }

        self.state = EngineState::Idle;
        self.stream_start_time = None;
        self.set_status("Generation stopped");
    }

    pub fn handle_stream_signal(&mut self, epoch: u64, sig: StreamSignal, tx: UnboundedSender<AppEvent>) {
        if epoch != self.stream_epoch {
            return;
        }

        match sig {
            StreamSignal::ThoughtDelta(chunk) => {
                self.current_thought_buffer.push_str(&chunk);
            }
            StreamSignal::TextDelta(chunk) => {
                self.current_response_buffer.push_str(&chunk);
                self.candidate_chunks_count += 1;
                if let Some(start) = self.stream_start_time {
                    let elapsed = start.elapsed().as_secs_f64();
                    if elapsed > 0.1 && self.candidates_tokens > 0 {
                        self.current_tps = self.candidates_tokens as f64 / elapsed;
                    }
                }
            }
            StreamSignal::ToolCall { id, name, args, thought_signature } => {
                // If model produced any thought prior to tool call, record it
                if !self.current_thought_buffer.is_empty() {
                    let thought = std::mem::take(&mut self.current_thought_buffer);
                    self.add_message("thought", thought);
                }

                // If model produced any response text prior to tool call, flush it into messages
                if !self.current_response_buffer.is_empty() {
                    let resp = std::mem::take(&mut self.current_response_buffer);
                    self.add_message("model", resp);
                }

                // Record model's functionCall part in conversation history for turn alternation
                let fc_part = Part::FunctionCall {
                    function_call: crate::client::types::FunctionCallPayload {
                        name: name.clone(),
                        args: args.clone(),
                        id: id.clone(),
                    },
                    thought_signature: thought_signature.clone(),
                };
                self.messages.push(ChatMessage {
                    role: "model_tool_call".to_string(),
                    content: serde_json::to_string(&fc_part).unwrap_or_default(),
                    timestamp: chrono::Local::now().format("%H:%M:%S").to_string(),
                });

                if let Some(tool) = self.tool_registry.get(&name) {
                    let preview = tool.generate_preview(&args);

                    if self.session_allowed_tools.contains(&name) {
                        self.execute_tool(name, id, args, tx);
                    } else {
                        // Pause and trigger HITL modal
                        self.state = EngineState::AwaitingHitlApproval;
                        self.set_status(format!("HITL Gate: Approval needed for '{}'", name));
                        self.modal_scroll = 0;
                        self.pending_tool_call = Some(PendingToolCall {
                            call_id: id,
                            tool_name: name,
                            args,
                            preview,
                        });
                    }
                } else {
                    self.add_message("system", format!("Warning: Model attempted to call unknown tool '{}'", name));
                }
            }
            StreamSignal::Usage { prompt_tokens, candidates_tokens, total_tokens } => {
                self.prompt_tokens = prompt_tokens;
                self.candidates_tokens = candidates_tokens;
                self.total_tokens = total_tokens;

                if let Some(start) = self.stream_start_time {
                    let elapsed = start.elapsed().as_secs_f64();
                    if elapsed > 0.05 {
                        self.current_tps = self.candidates_tokens as f64 / elapsed;
                    }
                }
            }
            StreamSignal::Finished { finish_reason } => {
                if !self.current_thought_buffer.is_empty() {
                    let thought = std::mem::take(&mut self.current_thought_buffer);
                    self.add_message("thought", thought);
                }

                if !self.current_response_buffer.is_empty() {
                    let resp = std::mem::take(&mut self.current_response_buffer);
                    self.add_message("model", resp);
                }

                if self.state == EngineState::Streaming {
                    self.state = EngineState::Idle;
                    let reason = finish_reason.unwrap_or_else(|| "STOP".to_string());
                    self.set_status(format!("Done ({})", reason));
                }
                self.stream_start_time = None;
                let _ = self.flush_session();
            }
            StreamSignal::Notice(message) => {
                self.set_status(&message);
                self.add_message("system", message);
            }
            StreamSignal::Error(err) => {
                self.add_message("system", format!("Error: {}", err));
                self.state = EngineState::Idle;
                self.set_status("Stream ended with error.");
            }
        }
    }

    pub fn approve_pending_tool(&mut self, whitelist_for_session: bool, tx: UnboundedSender<AppEvent>) {
        if let Some(pending) = self.pending_tool_call.take() {
            if whitelist_for_session {
                self.session_allowed_tools.insert(pending.tool_name.clone());
            }
            self.execute_tool(pending.tool_name, pending.call_id, pending.args, tx);
        }
    }

    pub fn deny_pending_tool(&mut self, tx: UnboundedSender<AppEvent>) {
        if let Some(pending) = self.pending_tool_call.take() {
            let rejection_result = "Execution rejected by user.".to_string();
            self.add_message("system", format!("Denied execution of tool '{}'", pending.tool_name));
            
            // Send denial back into tool result pipeline so Gemini can adjust
            let app_tx = tx.clone();
            tokio::spawn(async move {
                let _ = app_tx.send(AppEvent::ToolExecutionResult {
                    tool_name: pending.tool_name,
                    call_id: pending.call_id,
                    result: Err(rejection_result),
                });
            });
            self.state = EngineState::ExecutingTool;
        }
    }

    pub fn execute_tool(
        &mut self,
        tool_name: String,
        call_id: Option<String>,
        args: serde_json::Value,
        tx: UnboundedSender<AppEvent>,
    ) {
        self.state = EngineState::ExecutingTool;
        self.set_status(format!("Executing tool '{}'...", tool_name));

        if let Some(tool) = self.tool_registry.get(&tool_name) {
            let app_tx = tx.clone();
            let t_name = tool_name.clone();
            let c_id = call_id.clone();

            tokio::spawn(async move {
                let res = tool.execute(args).await;
                let _ = app_tx.send(AppEvent::ToolExecutionResult {
                    tool_name: t_name,
                    call_id: c_id,
                    result: res,
                });
            });
        }
    }

    pub fn handle_tool_result(
        &mut self,
        tool_name: String,
        call_id: Option<String>,
        result: Result<String, String>,
        tx: UnboundedSender<AppEvent>,
    ) {
        let (output_str, is_err) = match result {
            Ok(output) => (output, false),
            Err(e) => (format!("Error: {}", e), true),
        };

        // Add tool interaction to chat view
        let preview_snippet = if output_str.len() > 200 {
            format!("{}...\n(Total {} chars)", &output_str[..200], output_str.len())
        } else {
            output_str.clone()
        };

        self.add_message(
            "tool",
            format!("[Tool Output: {}]\n{}", tool_name, preview_snippet),
        );

        // Append assistant tool_call & user tool_response to internal conversation history
        let response_payload = if is_err {
            json!({ "error": output_str })
        } else {
            json!({ "output": output_str })
        };

        let response_part = Part::FunctionResponse {
            function_response: crate::client::types::FunctionResponsePayload {
                name: tool_name.clone(),
                response: response_payload,
                id: call_id,
            },
        };

        // Save into message stream as a synthesized user/function turn
        self.messages.push(ChatMessage {
            role: "function".to_string(),
            content: serde_json::to_string(&response_part).unwrap_or_default(),
            timestamp: chrono::Local::now().format("%H:%M:%S").to_string(),
        });
        let _ = self.flush_session();

        // Resume generation so model can synthesize answer from tool result
        self.state = EngineState::Idle;
        self.trigger_generation(tx);
    }

    pub fn build_request(&self) -> crate::client::types::GenerateContentRequest {
        let mut contents: Vec<crate::client::types::Content> = Vec::new();

        for m in &self.messages {
            if m.role == "thought" || m.role == "system" || m.role == "tool" {
                continue;
            }

            if m.role == "model_tool_call" {
                if let Ok(part) = serde_json::from_str::<Part>(&m.content) {
                    if let Some(last) = contents.last_mut() {
                        if last.role.as_deref() == Some("model") {
                            last.parts.push(part);
                            continue;
                        }
                    }
                    contents.push(Content {
                        role: Some("model".to_string()),
                        parts: vec![part],
                    });
                }
            } else if m.role == "function" {
                if let Ok(part) = serde_json::from_str::<Part>(&m.content) {
                    if let Some(last) = contents.last_mut() {
                        if last.role.as_deref() == Some("user") && last.parts.iter().any(|p| matches!(p, Part::FunctionResponse { .. })) {
                            last.parts.push(part);
                            continue;
                        }
                    }
                    contents.push(Content {
                        role: Some("user".to_string()),
                        parts: vec![part],
                    });
                }
            } else if m.role == "user" {
                contents.push(Content {
                    role: Some("user".to_string()),
                    parts: vec![Part::Text {
                        text: m.content.clone(),
                        thought: None,
                    }],
                });
            } else if m.role == "model" {
                contents.push(Content {
                    role: Some("model".to_string()),
                    parts: vec![Part::Text {
                        text: m.content.clone(),
                        thought: None,
                    }],
                });
            }
        }

        let model_profile = self.config.active_model_profile();
        let thinking_budget = model_profile.thinking_budget.unwrap_or(self.config.thinking_budget);
        let reasoning_enabled = model_profile.reasoning_enabled.unwrap_or(thinking_budget > 0);
        let thinking_config = if reasoning_enabled && thinking_budget > 0 {
            Some(ThinkingConfig {
                thinking_budget,
            })
        } else {
            None
        };

        // Safety Settings: BLOCK_NONE
        let safety_settings = vec![
            SafetySetting {
                category: "HARM_CATEGORY_HARASSMENT".to_string(),
                threshold: "BLOCK_NONE".to_string(),
            },
            SafetySetting {
                category: "HARM_CATEGORY_HATE_SPEECH".to_string(),
                threshold: "BLOCK_NONE".to_string(),
            },
            SafetySetting {
                category: "HARM_CATEGORY_SEXUALLY_EXPLICIT".to_string(),
                threshold: "BLOCK_NONE".to_string(),
            },
            SafetySetting {
                category: "HARM_CATEGORY_DANGEROUS_CONTENT".to_string(),
                threshold: "BLOCK_NONE".to_string(),
            },
        ];

        let tools = self.tool_registry.to_gemini_declarations();

        crate::client::types::GenerateContentRequest {
            contents,
            system_instruction: Some(Content {
                role: Some("system".to_string()),
                parts: vec![Part::Text {
                    text: self.config.system_instruction.clone(),
                    thought: None,
                }],
            }),
            generation_config: Some(GenerationConfig {
                temperature: Some(model_profile.temperature.unwrap_or(self.config.temperature)),
                max_output_tokens: Some(model_profile.max_output_tokens.unwrap_or(8192)),
                thinking_config,
                reasoning_effort: if reasoning_enabled { model_profile.reasoning_effort.clone() } else { None },
                extra: if model_profile.extra.is_empty() { None } else { Some(serde_json::Value::Object(model_profile.extra.clone().into_iter().collect())) },
            }),
            safety_settings: Some(safety_settings),
            tools: Some(tools),
        }
    }

    pub fn compact_history(&mut self, tx: UnboundedSender<AppEvent>) {
        let count = self.messages.len();
        if count <= 2 {
            self.add_message("system", "History is already minimal; compaction unnecessary.");
            return;
        }

        self.set_status("Compacting conversation history...");
        self.state = EngineState::Compacting;
        self.add_message("system", format!("Compacting {} turns into a concise context summary...", count));

        let mut transcript = String::new();
        for m in &self.messages {
            if m.role == "thought" {
                continue;
            }
            transcript.push_str(&format!("{}: {}\n\n", m.role, m.content));
        }

        let prompt = format!(
            "Analyze and summarize the following multi-turn coding and development conversation into a concise, high-density structured summary.\n\
            Preserve all key decisions, file paths modified or discussed, tool outputs, technical facts, and remaining user requests or tasks.\n\n\
            CONVERSATION TRANSCRIPT:\n{}",
            transcript
        );

        let request = crate::client::types::GenerateContentRequest {
            contents: vec![Content {
                role: Some("user".to_string()),
                parts: vec![Part::Text {
                    text: prompt,
                    thought: None,
                }],
            }],
            system_instruction: Some(Content {
                role: Some("system".to_string()),
                parts: vec![Part::Text {
                    text: "You are a precise technical summarizer for developer conversations. Retain essential context and code artifacts.".to_string(),
                    thought: None,
                }],
            }),
            generation_config: Some(GenerationConfig {
                temperature: Some(0.2),
                max_output_tokens: Some(2048),
                thinking_config: None,
                reasoning_effort: None,
                extra: None,
            }),
            safety_settings: None,
            tools: None,
        };

        let client = self.client.clone();
        let model = self.config.model.clone();
        let fallback_models = self.config.effective_fallback_models();
        let max_retries = self.config.max_retries;

        tokio::spawn(async move {
            let res = client.generate_content_with_fallback(&model, &fallback_models, max_retries, &request).await;
            let _ = tx.send(AppEvent::CompactionFinished(res));
        });
    }

    pub fn handle_compaction_result(&mut self, result: Result<String, String>, tx: UnboundedSender<AppEvent>) {
        match result {
            Ok(summary) => {
                let original_count = self.messages.len();
                self.messages.clear();
                self.chat_scroll = 0;

                // Insert compacted summary as initial system/user grounding
                let now = chrono::Local::now().format("%H:%M:%S").to_string();
                self.messages.push(ChatMessage {
                    role: "system".to_string(),
                    content: format!("Compact History Summary:\n{}", summary),
                    timestamp: now,
                });

                self.set_status("Context compacted");
                self.add_message(
                    "system",
                    format!("Successfully compacted {} messages. Context window reclaimed.", original_count),
                );
                let continue_generation = self.pending_generation_after_compaction;
                self.pending_generation_after_compaction = false;
                if let Some(prompt) = self.pending_prompt_after_compaction.take() {
                    self.add_message("user", prompt);
                }
                self.state = EngineState::Idle;
                let _ = self.flush_session();
                if continue_generation { self.trigger_generation(tx); }
            }
            Err(e) => {
                self.set_status("Compaction failed");
                self.add_message("system", format!("Compaction failed: {}", e));
                self.pending_generation_after_compaction = false;
                self.pending_prompt_after_compaction = None;
                self.state = EngineState::Idle;
            }
        }
    }
}

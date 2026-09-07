use crate::client::types::{
    Content, GenerationConfig, InlineDataPayload, Part, SafetySetting, ThinkingConfig,
};
use crate::client::{AiClient, ProviderKind};
use crate::config::AppConfig;
use crate::events::{AppEvent, StreamSignal};
use crate::session::{self, SessionSnapshot, UsageRecord};
use crate::tools::{ToolPreview, ToolRegistry};
use serde_json::json;
use std::collections::{HashSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
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
    #[serde(default)]
    pub attachments: Vec<Attachment>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Attachment {
    pub kind: String,
    pub name: String,
    #[serde(default)]
    pub mime_type: Option<String>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub data: Option<String>,
}

#[derive(Debug, Clone)]
pub enum DraftAttachment {
    Text {
        name: String,
        text: String,
    },
    Image {
        name: String,
        mime_type: String,
        data: String,
    },
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
    pub draft_attachments: Vec<DraftAttachment>,
    pub plan_mode: bool,

    // Active token metrics
    pub prompt_tokens: u64,
    pub candidates_tokens: u64,
    pub total_tokens: u64,
    /// Tokens in the context that will be sent on the next request. This is
    /// separate from `total_tokens`, which is the last request's total usage.
    pub context_tokens: u64,
    pub context_tokens_estimated: bool,

    // Streaming state
    pub active_stream_task: Option<JoinHandle<()>>,
    pub current_thought_buffer: String,
    pub current_response_buffer: String,
    pub usage_records: Vec<UsageRecord>,
    request_usage_received: bool,
    request_started_at: Option<std::time::Instant>,
    request_context_tokens: u64,

    // HITL Modal State
    pub pending_tool_call: Option<PendingToolCall>,
    pub queued_tool_calls: VecDeque<PendingToolCall>,
    /// Number of tool executions belonging to the current assistant turn.
    /// The next model request must wait until the entire batch is complete.
    pub pending_tool_executions: usize,
    pub session_allowed_tools: HashSet<String>,
    pub modal_scroll: usize,

    // Models list cached
    pub available_models: Vec<crate::client::types::ModelInfo>,
    pub show_models_modal: bool,
    pub models_scroll: usize,
    pub models_selected: usize,
    pub models_filter: String,
    pub models_searching: bool,
    pub show_thinking_modal: bool,
    pub thinking_selected: usize,
    pub thinking_target_model: Option<String>,
    pub show_plan_modal: bool,
    pub plan_modal_selected: usize,
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
    pub pending_todo_notice: Option<String>,
    pub session_path: Option<std::path::PathBuf>,
    pub session_messages_at_save: usize,
    pub project_root: Option<PathBuf>,
    pub project_instructions: String,
    pub ui_started_at: std::time::Instant,
}

fn open_config_file(path: &Path) -> Result<(), String> {
    #[cfg(windows)]
    let mut command = {
        let mut command = Command::new("notepad.exe");
        command.arg(path);
        command
    };
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut command = Command::new("open");
        command.arg(path);
        command
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut command = {
        let mut command = Command::new("xdg-open");
        command.arg(path);
        command
    };

    command
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("Could not open config: {}", error))
}

fn discover_project_context(start: &Path) -> (Option<PathBuf>, String) {
    let start = start.canonicalize().unwrap_or_else(|_| start.to_path_buf());
    let mut ancestors = Vec::new();
    let mut current = start.as_path();
    loop {
        ancestors.push(current.to_path_buf());
        let Some(parent) = current.parent() else {
            break;
        };
        current = parent;
    }
    ancestors.reverse();

    let mut root = None;
    let mut instructions = Vec::new();
    for directory in ancestors {
        for file_name in ["AGENTS.md", "CLAUDE.md"] {
            let path = directory.join(file_name);
            let Ok(content) = fs::read_to_string(&path) else {
                continue;
            };
            if content.trim().is_empty() {
                continue;
            }
            root.get_or_insert_with(|| directory.clone());
            let bounded = content.chars().take(32_000).collect::<String>();
            instructions.push(format!("# {}\n{}", path.display(), bounded));
            if instructions
                .iter()
                .map(|item| item.chars().count())
                .sum::<usize>()
                >= 64_000
            {
                return (root, instructions.join("\n\n"));
            }
        }
    }
    (root, instructions.join("\n\n"))
}

#[cfg(test)]
mod tests {
    use super::{truncate_tool_output, App, Attachment, ChatMessage, MAX_TOOL_RESULT_CHARS};
    use crate::client::types::{FunctionCallPayload, FunctionResponsePayload, Part};
    use crate::config::AppConfig;
    use serde_json::json;

    fn message(role: &str, part: Part) -> ChatMessage {
        ChatMessage {
            role: role.to_string(),
            content: serde_json::to_string(&part).unwrap(),
            timestamp: String::new(),
            attachments: Vec::new(),
        }
    }

    #[test]
    fn drops_orphaned_openai_tool_calls_but_keeps_completed_calls() {
        let mut config = AppConfig::default();
        config.providers.get_mut("gemini").unwrap().kind = "openai-compatible".to_string();
        let mut app = App::new(config, "test-key".to_string());
        app.messages = vec![
            message(
                "model_tool_call",
                Part::FunctionCall {
                    function_call: FunctionCallPayload {
                        name: "read_file".to_string(),
                        args: json!({"path": "orphan.rs"}),
                        id: Some("call-orphan".to_string()),
                    },
                    thought_signature: None,
                },
            ),
            ChatMessage {
                role: "user".to_string(),
                content: "continue".to_string(),
                timestamp: String::new(),
                attachments: Vec::new(),
            },
        ];
        let request = app.build_request();
        assert!(request.contents.iter().all(|content| {
            content
                .parts
                .iter()
                .all(|part| !matches!(part, Part::FunctionCall { .. }))
        }));

        app.messages.push(message(
            "model_tool_call",
            Part::FunctionCall {
                function_call: FunctionCallPayload {
                    name: "read_file".to_string(),
                    args: json!({"path": "done.rs"}),
                    id: Some("call-done".to_string()),
                },
                thought_signature: None,
            },
        ));
        app.messages.push(message(
            "function",
            Part::FunctionResponse {
                function_response: FunctionResponsePayload {
                    name: "read_file".to_string(),
                    response: json!({"output": "ok"}),
                    id: Some("call-done".to_string()),
                },
            },
        ));
        let request = app.build_request();
        assert!(request.contents.iter().any(|content| {
            content.parts.iter().any(|part| {
                matches!(part, Part::FunctionCall { function_call, .. } if function_call.id.as_deref() == Some("call-done"))
            })
        }));
    }

    #[test]
    fn context_metric_is_not_last_request_total() {
        let mut app = App::new(AppConfig::default(), "test-key".to_string());
        app.add_message("user", "A short prompt");
        app.total_tokens = 900_000;
        assert!(app.context_tokens < 900_000);
        assert!(app.context_tokens_estimated);
    }

    #[test]
    fn compact_summary_is_replayed_as_model_input() {
        let mut app = App::new(AppConfig::default(), "test-key".to_string());
        app.messages.push(ChatMessage {
            role: "summary".to_string(),
            content: "Compact History Summary:\nThe user is fixing session restore.".to_string(),
            timestamp: String::new(),
            attachments: Vec::new(),
        });
        app.messages.push(ChatMessage {
            role: "system".to_string(),
            content: "UI-only notice".to_string(),
            timestamp: String::new(),
            attachments: Vec::new(),
        });
        let request = app.build_request();
        assert!(request.contents.iter().any(|content| {
            content.parts.iter().any(
                |part| matches!(part, Part::Text { text, .. } if text.contains("session restore")),
            )
        }));
        assert!(request.contents.iter().all(|content| {
            content
                .parts
                .iter()
                .all(|part| !matches!(part, Part::Text { text, .. } if text == "UI-only notice"))
        }));
    }

    #[test]
    fn tool_output_truncation_is_unicode_safe() {
        let output = "界".repeat(MAX_TOOL_RESULT_CHARS + 256);
        let truncated = truncate_tool_output(&output);
        assert!(truncated.contains("tool output truncated"));
        assert!(truncated.starts_with("界"));
        assert!(truncated.ends_with("界"));
    }

    #[test]
    fn structured_plan_requires_a_heading_and_step() {
        let mut app = App::new(AppConfig::default(), "test-key".to_string());
        app.add_message(
            "model",
            "## Plan\n1. Inspect the project\n2. Propose the fix",
        );
        assert!(app.latest_model_has_plan());
        app.messages.push(ChatMessage {
            role: "model".to_string(),
            content: "I have some ideas, but no executable outline yet.".to_string(),
            timestamp: String::new(),
            attachments: Vec::new(),
        });
        assert!(!app.latest_model_has_plan());
    }

    #[test]
    fn attachments_are_sent_after_prompt_with_text_last() {
        let mut config = AppConfig::default();
        config.providers.get_mut("gemini").unwrap().kind = "gemini".to_string();
        let mut app = App::new(config, "test-key".to_string());
        app.add_message_with_attachments(
            "user",
            "question",
            vec![
                Attachment {
                    kind: "image".to_string(),
                    name: "screen.png".to_string(),
                    mime_type: Some("image/png".to_string()),
                    text: None,
                    data: Some("base64-image".to_string()),
                },
                Attachment {
                    kind: "text".to_string(),
                    name: "paste-1.txt".to_string(),
                    mime_type: Some("text/plain".to_string()),
                    text: Some("actual pasted content".to_string()),
                    data: None,
                },
            ],
        );

        let request = app.build_request();
        let user = request
            .contents
            .iter()
            .find(|content| content.role.as_deref() == Some("user"))
            .unwrap();
        assert!(matches!(&user.parts[0], Part::InlineData { .. }));
        assert!(
            matches!(&user.parts[1], Part::Text { text, .. } if text.contains("actual pasted content"))
        );
        assert!(matches!(&user.parts[2], Part::Text { text, .. } if text == "question"));
    }

    #[test]
    fn attachment_commands_operate_on_draft_instead_of_clearing_it() {
        let mut app = App::new(AppConfig::default(), "test-key".to_string());
        app.draft_attachments.push(super::DraftAttachment::Text {
            name: "paste-1.txt".to_string(),
            text: "editable paste".to_string(),
        });
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();

        app.input_buffer = "/attachments".to_string();
        app.input_cursor = app.input_buffer.chars().count();
        app.handle_enter(tx.clone());
        assert_eq!(app.draft_attachments.len(), 1);

        app.input_buffer = "/edit 1".to_string();
        app.input_cursor = app.input_buffer.chars().count();
        app.handle_enter(tx);
        assert!(app.draft_attachments.is_empty());
        assert_eq!(app.input_buffer, "editable paste");
    }

    #[test]
    fn ctrl_c_clears_draft_before_quitting() {
        let mut app = App::new(AppConfig::default(), "test-key".to_string());
        app.input_buffer = "unsent text".to_string();
        app.input_cursor = app.input_buffer.chars().count();

        app.handle_ctrl_c();
        assert!(app.input_buffer.is_empty());
        assert!(!app.should_quit);
        assert!(app
            .status_message
            .as_deref()
            .is_some_and(|message| message.contains("Press Ctrl+C again")));

        app.handle_ctrl_c();
        assert!(app.should_quit);
    }
}

impl App {
    pub fn new(config: AppConfig, api_key: String) -> Self {
        let client = AiClient::from_config(api_key.clone(), &config);
        let mut tool_registry = ToolRegistry::new();
        tool_registry.install_agent_tools(
            client.clone(),
            config.model.clone(),
            config.effective_fallback_models(),
            config.max_retries,
        );
        let session_path = session::new_session_path(&config.session_name);
        let working_dir = tool_registry.working_dir();
        let (project_root, project_instructions) = discover_project_context(&working_dir);

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
            draft_attachments: Vec::new(),
            plan_mode: false,
            prompt_tokens: 0,
            candidates_tokens: 0,
            total_tokens: 0,
            context_tokens: 0,
            context_tokens_estimated: true,
            active_stream_task: None,
            current_thought_buffer: String::new(),
            current_response_buffer: String::new(),
            usage_records: Vec::new(),
            request_usage_received: false,
            request_started_at: None,
            request_context_tokens: 0,
            pending_tool_call: None,
            queued_tool_calls: VecDeque::new(),
            pending_tool_executions: 0,
            session_allowed_tools: HashSet::new(),
            modal_scroll: 0,
            available_models: Vec::new(),
            show_models_modal: false,
            models_scroll: 0,
            models_selected: 0,
            models_filter: String::new(),
            models_searching: false,
            show_thinking_modal: false,
            thinking_selected: 0,
            thinking_target_model: None,
            show_plan_modal: false,
            plan_modal_selected: 0,
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
            pending_todo_notice: None,
            session_path,
            session_messages_at_save: 0,
            project_root,
            project_instructions,
            ui_started_at: std::time::Instant::now(),
        }
    }

    pub fn set_status(&mut self, msg: impl Into<String>) {
        self.status_message = Some(msg.into());
    }

    /// Implements the two-stage terminal interrupt used by the TUI. The
    /// first Ctrl+C clears the current composer draft; only an empty composer
    /// allows the next Ctrl+C to exit.
    pub fn handle_ctrl_c(&mut self) {
        if !self.input_buffer.is_empty() || !self.draft_attachments.is_empty() {
            self.input_buffer.clear();
            self.input_cursor = 0;
            self.input_history_idx = None;
            self.draft_attachments.clear();
            self.set_status("Draft cleared. Press Ctrl+C again to exit.");
        } else {
            self.should_quit = true;
        }
    }

    pub fn context_limit(&self) -> u64 {
        self.available_models
            .iter()
            .find(|model| model.id == self.config.model)
            .and_then(|model| model.input_token_limit)
            .unwrap_or(1_048_576)
    }

    pub fn refresh_client_from_config(&mut self) {
        let provider_config = self.config.active_provider_config();
        self.client.update_provider(
            ProviderKind::parse(&provider_config.kind),
            provider_config
                .base_url
                .or_else(|| self.config.base_url.clone()),
            provider_config.headers,
            provider_config.stream_usage,
            crate::client::ProviderProtocol::parse(&provider_config.protocol),
        );
        if let Some(api_key) = self.config.get_api_key_for_active_provider() {
            self.client.update_api_key(api_key);
        }
    }

    fn refresh_context_estimate(&mut self) {
        self.context_tokens = self.estimated_context_tokens();
        self.context_tokens_estimated = true;
    }

    fn effective_system_instruction(&self) -> String {
        let mut instruction = self.config.system_instruction.clone();
        if !self.project_instructions.is_empty() {
            instruction.push_str(&format!(
                "\n\nProject instructions loaded from {}:\n{}",
                self.project_root
                    .as_ref()
                    .map(|path| path.display().to_string())
                    .unwrap_or_else(|| "the current project".to_string()),
                self.project_instructions
            ));
        }
        instruction.push_str(
            "\n\nTool discovery: core tools are available directly. Specialized and MCP tools may be hidden to save context; call search_tools with a short purpose query when the core tools are insufficient. Matching tools become available on the next turn.",
        );
        if self.plan_mode {
            instruction.push_str(
                "\n\nPLAN MODE: inspect and reason about the task, maintain the todo list, and propose changes. Do not mutate files, execute commands, or make external changes. When the plan is ready, format it with a `## Plan` heading followed by numbered steps. The user can approve it with /plan off, which will begin execution; otherwise continue planning when asked.",
           );
        }
        instruction
    }
    pub fn selected_model_id(&self) -> Option<String> {
        self.filtered_model_indices()
            .get(self.models_selected)
            .and_then(|index| self.available_models.get(*index))
            .map(|model| model.id.clone())
    }

    pub fn filtered_model_indices(&self) -> Vec<usize> {
        let filter = self.models_filter.to_ascii_lowercase();
        self.available_models
            .iter()
            .enumerate()
            .filter_map(|(index, model)| {
                if filter.is_empty()
                    || model.id.to_ascii_lowercase().contains(&filter)
                    || model.display_name.to_ascii_lowercase().contains(&filter)
                    || model.description.to_ascii_lowercase().contains(&filter)
                {
                    Some(index)
                } else {
                    None
                }
            })
            .collect()
    }

    pub fn clamp_model_selection(&mut self) {
        self.models_selected = self
            .models_selected
            .min(self.filtered_model_indices().len().saturating_sub(1));
    }

    pub fn refresh_sessions(&mut self) {
        self.available_sessions = session::list();
        self.sessions_selected = 0;
    }

    pub fn resume_selected_session(&mut self) {
        let Some(info) = self.available_sessions.get(self.sessions_selected).cloned() else {
            return;
        };
        if self.restore_session(&info.path).is_ok() {
            self.show_sessions_modal = false;
            self.add_message("system", format!("Resumed session '{}'.", info.name));
        }
    }

    pub fn choose_plan_decision(&mut self, execute: bool, tx: UnboundedSender<AppEvent>) {
        if !self.show_plan_modal {
            return;
        }
        self.show_plan_modal = false;
        if execute {
            self.plan_mode = false;
            self.add_message("system", "Plan approved. Starting execution.");
            self.add_message(
                "user",
                "The plan above is approved. Execute it now, using the todo list and reporting progress.",
            );
            let _ = self.flush_session();
            self.trigger_generation(tx);
        } else {
            self.plan_mode = true;
            self.add_message(
                "user",
                "Keep planning. Refine the plan, resolve open questions, and do not execute changes.",
            );
            let _ = self.flush_session();
            self.trigger_generation(tx);
        }
    }

    pub fn restore_session(&mut self, path: &Path) -> Result<(), String> {
        let snapshot = session::load(path)
            .ok_or_else(|| format!("Could not read session snapshot: {}", path.display()))?;
        self.messages = snapshot.messages;
        self.usage_records = snapshot.usage;
        let restored_working_dir = snapshot
            .working_dir
            .clone()
            .map(|working_dir| self.tool_registry.set_working_dir(working_dir).is_ok())
            .unwrap_or(false);
        let discovered = discover_project_context(&self.tool_registry.working_dir());
        self.project_root = if restored_working_dir {
            snapshot.project_root.or(discovered.0)
        } else {
            discovered.0
        };
        self.project_instructions = if restored_working_dir {
            snapshot.project_instructions.unwrap_or(discovered.1)
        } else {
            discovered.1
        };
        self.tool_registry.set_todo_items(snapshot.todos);
        self.plan_mode = snapshot.plan_mode;
        self.config.select_provider(&snapshot.provider);
        self.config.model = snapshot.model;
        self.config.remember_model();
        let _ = self.config.save();
        self.refresh_client_from_config();
        self.state = EngineState::Idle;
        self.pending_tool_call = None;
        self.queued_tool_calls.clear();
        self.pending_tool_executions = 0;
        self.current_thought_buffer.clear();
        self.current_response_buffer.clear();
        self.pending_todo_notice = None;
        self.show_plan_modal = false;
        self.plan_modal_selected = 0;
        self.request_started_at = None;
        self.request_usage_received = false;
        self.active_stream_task = None;
        self.session_path = Some(path.to_path_buf());
        self.chat_scroll = 0;
        self.session_messages_at_save = self.messages.len();
        self.prompt_tokens = 0;
        self.candidates_tokens = 0;
        self.total_tokens = 0;
        self.refresh_context_estimate();
        Ok(())
    }

    pub fn delete_selected_session(&mut self) {
        let Some(info) = self.available_sessions.get(self.sessions_selected).cloned() else {
            return;
        };
        if std::fs::remove_file(&info.path).is_ok() {
            self.refresh_sessions();
            self.set_status(format!("Deleted session '{}'", info.name));
        }
    }

    pub fn export_selected_session(&mut self) {
        let Some(info) = self.available_sessions.get(self.sessions_selected).cloned() else {
            return;
        };
        let export_path = info
            .path
            .with_file_name(format!("{}.export.json", info.name));
        if let Ok(snapshot) = std::fs::read_to_string(&info.path) {
            if std::fs::write(&export_path, snapshot).is_ok() {
                self.set_status(format!("Exported session to {}", export_path.display()));
            }
        }
    }

    pub fn select_model_from_catalog(&mut self) {
        if let Some(model) = self.selected_model_id() {
            self.config.model = model.clone();
            self.config.remember_model();
            let _ = self.config.save();
            self.set_status(format!("Active model: {}", model));
        }
    }

    pub fn thinking_mode(&self, model: &str) -> String {
        let profile = self
            .config
            .model_profiles
            .get(&format!("{}:{}", self.config.provider, model));
        let enabled = profile
            .and_then(|profile| profile.reasoning_enabled)
            .unwrap_or(self.config.thinking_budget > 0);
        if !enabled {
            return "off".to_string();
        }
        if let Some(effort) = profile.and_then(|profile| profile.reasoning_effort.as_deref()) {
            if effort == "none" {
                return "off".to_string();
            }
            if matches!(effort, "minimal" | "low" | "medium" | "high") {
                return effort.to_string();
            }
        }
        match profile
            .and_then(|profile| profile.thinking_budget)
            .unwrap_or(self.config.thinking_budget)
        {
            0 => "off".to_string(),
            1..=2048 => "low".to_string(),
            2049..=6144 => "medium".to_string(),
            _ => "high".to_string(),
        }
    }

    pub fn thinking_choices(&self, model: &str) -> Vec<&'static str> {
        let provider_kind = self.config.active_provider_config().kind;
        if provider_kind.eq_ignore_ascii_case("gemini") {
            let model = model.to_ascii_lowercase();
            if model.contains("gemini-3.1-flash-lite-image") {
                return vec!["minimal", "high"];
            }
            if model.contains("gemini-3-pro") {
                return vec!["low", "high"];
            }
            if model.contains("gemini-3.5")
                || model.contains("gemini-3.6")
                || model.contains("gemini-3-flash")
                || model.contains("gemini-3.1-flash-lite")
            {
                return vec!["minimal", "low", "medium", "high"];
            }
            if model.contains("gemini-3") {
                return vec!["low", "medium", "high"];
            }
            return vec!["off", "low", "medium", "high"];
        }
        vec!["off", "low", "medium", "high"]
    }

    pub fn set_thinking_mode_for_model(&mut self, model: &str, mode: &str) {
        let mode = mode.to_ascii_lowercase();
        let (enabled, budget, effort) = match mode.as_str() {
            "off" | "none" => (false, 0, None),
            "minimal" => (true, 1024, Some("minimal")),
            "low" => (true, 1024, Some("low")),
            "medium" | "med" => (true, 8192, Some("medium")),
            "high" => (true, 24576, Some("high")),
            _ => return,
        };
        let key = format!("{}:{}", self.config.provider, model);
        let is_gemini = self
            .config
            .active_provider_config()
            .kind
            .eq_ignore_ascii_case("gemini");
        {
            let profile = self.config.model_profiles.entry(key).or_default();
            profile.reasoning_enabled = Some(enabled);
            profile.thinking_budget = Some(budget);
            profile.reasoning_effort = if enabled {
                effort.map(str::to_string)
            } else if !is_gemini {
                Some("none".to_string())
            } else {
                None
            };
        }
        let _ = self.config.save();
        self.set_status(format!("Thinking {} for {}", mode, model));
    }

    pub fn set_active_thinking_mode(&mut self, mode: &str) {
        let model = self.config.model.clone();
        self.set_thinking_mode_for_model(&model, mode);
    }

    pub fn open_thinking_modal(&mut self) {
        self.open_thinking_modal_for(self.config.model.clone());
    }

    pub fn open_thinking_modal_for_selected_model(&mut self) {
        let model = self
            .selected_model_id()
            .unwrap_or_else(|| self.config.model.clone());
        self.open_thinking_modal_for(model);
    }

    fn open_thinking_modal_for(&mut self, model: String) {
        self.thinking_target_model = Some(model.clone());
        let current = self.thinking_mode(&model);
        let choices = self.thinking_choices(&model);
        self.thinking_selected = choices
            .iter()
            .position(|choice| *choice == current)
            .unwrap_or(0);
        self.show_thinking_modal = true;
    }

    pub fn apply_thinking_modal_selection(&mut self) {
        let model = self
            .thinking_target_model
            .take()
            .unwrap_or_else(|| self.config.model.clone());
        let choices = self.thinking_choices(&model);
        let mode = choices
            .get(self.thinking_selected.min(choices.len().saturating_sub(1)))
            .copied()
            .unwrap_or("off");
        self.set_thinking_mode_for_model(&model, mode);
        self.show_thinking_modal = false;
    }

    pub fn toggle_selected_reasoning(&mut self) {
        let Some(model) = self.selected_model_id() else {
            return;
        };
        let key = format!("{}:{}", self.config.provider, model);
        let profile = self.config.model_profiles.entry(key).or_default();
        let enabled = !profile
            .reasoning_enabled
            .unwrap_or(self.config.thinking_budget > 0);
        profile.reasoning_enabled = Some(enabled);
        let _ = self.config.save();
        self.add_message(
            "system",
            format!(
                "{} reasoning {}",
                model,
                if enabled { "enabled" } else { "disabled" }
            ),
        );
    }

    pub fn adjust_selected_temperature(&mut self, delta: f32) {
        let Some(model) = self.selected_model_id() else {
            return;
        };
        let key = format!("{}:{}", self.config.provider, model);
        let profile = self.config.model_profiles.entry(key).or_default();
        let current = profile.temperature.unwrap_or(self.config.temperature);
        profile.temperature = Some((current + delta).clamp(0.0, 2.0));
    }

    pub fn adjust_selected_max_tokens(&mut self, delta: i32) {
        let Some(model) = self.selected_model_id() else {
            return;
        };
        let key = format!("{}:{}", self.config.provider, model);
        let profile = self.config.model_profiles.entry(key).or_default();
        let current = profile.max_output_tokens.unwrap_or(8192) as i32;
        profile.max_output_tokens = Some((current + delta).max(256) as u32);
    }

    pub fn toggle_selected_fallback(&mut self) {
        let Some(model) = self.selected_model_id() else {
            return;
        };
        let provider_name = self.config.provider.clone();
        let provider = self
            .config
            .providers
            .entry(provider_name.clone())
            .or_insert_with(|| crate::config::ProviderConfig {
                kind: "openai-compatible".to_string(),
                protocol: "auto".to_string(),
                base_url: self.config.base_url.clone(),
                model: None,
                models: Vec::new(),
                fallback_models: Vec::new(),
                api_key_env: None,
                headers: std::collections::BTreeMap::new(),
                stream_usage: true,
            });
        if let Some(position) = provider
            .fallback_models
            .iter()
            .position(|candidate| candidate == &model)
        {
            provider.fallback_models.remove(position);
        } else {
            provider.fallback_models.push(model);
        }
    }

    pub fn add_message(&mut self, role: &str, content: impl Into<String>) {
        self.add_message_with_attachments(role, content, Vec::new());
    }

    fn format_todos(&self) -> String {
        let todos = self.tool_registry.todo_items();
        if todos.is_empty() {
            return "Todo list is empty.".to_string();
        }
        let body = todos
            .iter()
            .enumerate()
            .map(|(index, item)| {
                format!(
                    "{} [{}] {}",
                    index + 1,
                    if item.done { "x" } else { " " },
                    item.text
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        format!("Todo list:\n{}", body)
    }

    fn latest_model_has_plan(&self) -> bool {
        let Some(content) = self
            .messages
            .iter()
            .rev()
            .find(|message| message.role == "model")
            .map(|message| message.content.as_str())
        else {
            return false;
        };

        let mut in_plan = false;
        let mut steps = 0usize;
        for line in content.lines() {
            let trimmed = line.trim();
            let lower = trimmed.to_ascii_lowercase();
            if lower == "## plan"
                || lower.starts_with("## plan:")
                || lower.starts_with("### plan")
                || lower.starts_with("implementation plan")
            {
                in_plan = true;
                continue;
            }
            if in_plan && trimmed.starts_with("## ") {
                break;
            }
            if in_plan
                && (trimmed.starts_with("- [ ]")
                    || trimmed.starts_with("- [x]")
                    || trimmed
                        .chars()
                        .take_while(|character| character.is_ascii_digit())
                        .count()
                        > 0)
            {
                steps += 1;
            }
        }
        in_plan && steps > 0
    }

    fn note_todo_change(&mut self, description: String, tx: UnboundedSender<AppEvent>) {
        let notice = format!(
            "TODO UPDATE: The user changed the task list ({}).\n{}",
            description,
            self.format_todos()
        );
        if self.config.todo_change_mode.eq_ignore_ascii_case("force")
            && self.state == EngineState::Streaming
        {
            self.cancel_generation();
            self.add_message("system", notice);
            self.trigger_generation(tx);
        } else {
            self.pending_todo_notice = Some(notice);
            self.set_status("Todo change will be included on the next turn");
        }
    }

    pub fn add_message_with_attachments(
        &mut self,
        role: &str,
        content: impl Into<String>,
        attachments: Vec<Attachment>,
    ) {
        let now = chrono::Local::now().format("%H:%M:%S").to_string();
        self.messages.push(ChatMessage {
            role: role.to_string(),
            content: content.into(),
            timestamp: now,
            attachments,
        });
        self.chat_scroll = 0; // Stick to bottom
        self.refresh_context_estimate();
    }

    pub fn flush_session(&mut self) -> Result<(), String> {
        let Some(path) = &self.session_path else {
            return Ok(());
        };
        let snapshot = SessionSnapshot {
            schema_version: 2,
            provider: self.config.provider.clone(),
            model: self.config.model.clone(),
            messages: self.messages.clone(),
            usage: self.usage_records.clone(),
            working_dir: Some(self.tool_registry.working_dir()),
            project_root: self.project_root.clone(),
            project_instructions: (!self.project_instructions.is_empty())
                .then(|| self.project_instructions.clone()),
            todos: self.tool_registry.todo_items(),
            plan_mode: self.plan_mode,
        };
        session::save(path, &snapshot)?;
        self.session_messages_at_save = self.messages.len();
        Ok(())
    }

    fn record_request_usage(&mut self, status: &str) {
        let Some(started_at) = self.request_started_at.take() else {
            return;
        };
        let estimated_output =
            ((self.current_thought_buffer.len() + self.current_response_buffer.len()) as u64 / 4)
                .max(1);
        let prompt_tokens = if self.request_usage_received {
            self.prompt_tokens
        } else {
            self.request_context_tokens
        };
        let candidates_tokens = if self.request_usage_received {
            self.candidates_tokens
        } else {
            estimated_output
        };
        let total_tokens = if self.request_usage_received && self.total_tokens > 0 {
            self.total_tokens
        } else {
            prompt_tokens.saturating_add(candidates_tokens)
        };
        self.usage_records.push(UsageRecord {
            timestamp: chrono::Local::now().to_rfc3339(),
            provider: self.config.provider.clone(),
            model: self.config.model.clone(),
            prompt_tokens,
            candidates_tokens,
            total_tokens,
            estimated: !self.request_usage_received,
            duration_ms: started_at.elapsed().as_millis() as u64,
            status: status.to_string(),
        });
        self.request_usage_received = false;
    }

    pub fn handle_enter(&mut self, tx: UnboundedSender<AppEvent>) {
        let text = self.input_buffer.trim().to_string();
        if text.is_empty() && self.draft_attachments.is_empty() {
            return;
        }

        if self.state != EngineState::Idle
            && !(text.starts_with("/todo") || text.eq_ignore_ascii_case("/todos"))
        {
            self.set_status("Engine busy. Press Esc to cancel active stream.");
            return;
        }

        self.input_history.push(text.clone());
        self.input_history_idx = None;
        self.input_buffer.clear();
        self.input_cursor = 0;

        if self.state != EngineState::Idle {
            self.handle_slash_command(&text, tx);
            return;
        }

        // Commands operate on draft blocks. Do not drain or clear them before
        // dispatching /edit, /remove, /attachments, or /attach.
        if text.starts_with('/') {
            self.handle_slash_command(&text, tx);
            return;
        }

        let attachments = self
            .draft_attachments
            .drain(..)
            .map(|attachment| match attachment {
                DraftAttachment::Text { name, text } => Attachment {
                    kind: "text".to_string(),
                    name,
                    mime_type: Some("text/plain".to_string()),
                    text: Some(text),
                    data: None,
                },
                DraftAttachment::Image {
                    name,
                    mime_type,
                    data,
                } => Attachment {
                    kind: "image".to_string(),
                    name,
                    mime_type: Some(mime_type),
                    text: None,
                    data: Some(data),
                },
            })
            .collect::<Vec<_>>();

        self.add_message_with_attachments("user", text, attachments);
        let _ = self.flush_session();
        self.trigger_generation(tx);
    }

    pub fn append_paste(&mut self, pasted: String) {
        if pasted.is_empty() {
            return;
        }
        let char_count = pasted.chars().count();
        if pasted.contains('\n') || pasted.contains('\r') || char_count > 512 {
            let number = self.draft_attachments.len() + 1;
            self.draft_attachments.push(DraftAttachment::Text {
                name: format!("paste-{}.txt", number),
                text: pasted,
            });
            self.set_status(format!("Attached pasted text as block #{}", number));
        } else {
            self.insert_input_text(&pasted);
        }
    }

    pub fn insert_input_text(&mut self, text: &str) {
        let byte_index = self
            .input_buffer
            .char_indices()
            .nth(self.input_cursor)
            .map(|(index, _)| index)
            .unwrap_or(self.input_buffer.len());
        self.input_buffer.insert_str(byte_index, text);
        self.input_cursor += text.chars().count();
        self.input_history_idx = None;
    }

    pub fn remove_last_attachment(&mut self) {
        if let Some(attachment) = self.draft_attachments.pop() {
            self.set_status(format!(
                "Removed attachment '{}'",
                attachment_name(&attachment)
            ));
        }
    }

    pub fn remove_attachment(&mut self, index: usize) {
        if index < self.draft_attachments.len() {
            let attachment = self.draft_attachments.remove(index);
            self.set_status(format!(
                "Removed attachment '{}'",
                attachment_name(&attachment)
            ));
        }
    }

    pub fn edit_attachment(&mut self, index: usize) {
        if index >= self.draft_attachments.len() {
            return;
        }
        if let DraftAttachment::Text { text, .. } = self.draft_attachments.remove(index) {
            self.input_buffer = text;
            self.input_cursor = self.input_buffer.chars().count();
            self.set_status(
                "Editing pasted text block; send it inline or paste it again to reattach.",
            );
        } else {
            self.set_status(
                "Image blocks are removable; edit the source file and attach it again.",
            );
        }
    }

    pub fn clipboard_paste(&mut self) {
        #[cfg(windows)]
        {
            let script = r#"$ErrorActionPreference='Stop'; Add-Type -AssemblyName System.Windows.Forms; Add-Type -AssemblyName System.Drawing; if ([Windows.Forms.Clipboard]::ContainsImage()) { $image=[Windows.Forms.Clipboard]::GetImage(); $stream=New-Object IO.MemoryStream; $image.Save($stream,[Drawing.Imaging.ImageFormat]::Png); [Console]::Write('IMG:'+[Convert]::ToBase64String($stream.ToArray())) } elseif ([Windows.Forms.Clipboard]::ContainsText()) { [Console]::Write('TXT:'+[Windows.Forms.Clipboard]::GetText()) } else { exit 2 }"#;
            match Command::new("powershell")
                .args(["-NoProfile", "-STA", "-Command", script])
                .output()
            {
                Ok(output) if output.status.success() => {
                    let clipboard = String::from_utf8_lossy(&output.stdout);
                    if let Some(data) = clipboard.strip_prefix("IMG:") {
                        let number = self.draft_attachments.len() + 1;
                        self.draft_attachments.push(DraftAttachment::Image {
                            name: format!("clipboard-{}.png", number),
                            mime_type: "image/png".to_string(),
                            data: data.to_string(),
                        });
                        self.set_status(format!("Attached clipboard image as block #{}", number));
                    } else if let Some(text) = clipboard.strip_prefix("TXT:") {
                        self.append_paste(text.to_string());
                    }
                }
                _ => self.set_status("Clipboard unavailable; use bracketed paste for text."),
            }
        }
        #[cfg(not(windows))]
        self.set_status("Clipboard image paste is currently supported on Windows; use bracketed paste for text.");
    }

    pub fn attach_path(&mut self, path: &str) {
        let path = PathBuf::from(path.trim_matches('"'));
        if !path.is_file() {
            self.set_status(format!("Attachment not found: {}", path.display()));
            return;
        }
        let extension = path
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if matches!(
            extension.as_str(),
            "png" | "jpg" | "jpeg" | "webp" | "gif" | "bmp"
        ) {
            match fs::read(&path) {
                Ok(bytes) => {
                    let mime_type = match extension.as_str() {
                        "jpg" | "jpeg" => "image/jpeg",
                        "webp" => "image/webp",
                        "gif" => "image/gif",
                        "bmp" => "image/bmp",
                        _ => "image/png",
                    };
                    self.draft_attachments.push(DraftAttachment::Image {
                        name: path
                            .file_name()
                            .map(|name| name.to_string_lossy().to_string())
                            .unwrap_or_else(|| "image".to_string()),
                        mime_type: mime_type.to_string(),
                        data: base64::Engine::encode(
                            &base64::engine::general_purpose::STANDARD,
                            bytes,
                        ),
                    });
                    self.set_status(format!("Attached image {}", path.display()));
                }
                Err(error) => self.set_status(format!("Could not read image: {}", error)),
            }
        } else {
            match fs::read_to_string(&path) {
                Ok(text) => {
                    self.draft_attachments.push(DraftAttachment::Text {
                        name: path
                            .file_name()
                            .map(|name| name.to_string_lossy().to_string())
                            .unwrap_or_else(|| "attachment.txt".to_string()),
                        text,
                    });
                    self.set_status(format!("Attached text file {}", path.display()));
                }
                Err(error) => self.set_status(format!("Could not read attachment: {}", error)),
            }
        }
    }

    pub fn handle_slash_command(&mut self, command_line: &str, tx: UnboundedSender<AppEvent>) {
        let mut parts = command_line.splitn(2, |character: char| character.is_whitespace());
        let cmd = parts.next().unwrap_or("").to_lowercase();
        let arg = parts.next().map(|s| s.trim()).unwrap_or("");

        match cmd.as_str() {
            "/help" => {
                self.add_message("system", 
                    "Available Commands:\n\
                    - /compact : Summarize conversation history to reclaim context window\n\
                    - /models : Fetch live models & pricing from the active provider\n\
                    - /providers : List configured and built-in providers\n\
                    - /provider <name> : Select a configured provider\n\
                    - /baseurl <url|default> : Set the active provider base URL\n\
                    - /config <path|open|dir> : Inspect or open the active config file\n\
                    - /model <name> : Switch active model (e.g. /model gemini-3.5-flash-lite)\n\
                    - /thinking [off|minimal|low|medium|high] : Open the model-aware thinking picker\n\
                    - /reasoning [on|off|low|medium|high] : Set reasoning effort for the active model\n\
                    - /autocompact <on|off|tokens> : Configure automatic context compaction\n\
                    - /usage : Show persisted token usage for this session\n\
                    - /context : Show next-request context and model limit\n\
                    - /status : Show engine/provider/session state\n\
                    - /pwd : Show the restored project working directory\n\
                    - /tools : List registered tools\n\
                    - /plan [on|off|continue] : Plan, approve a structured plan, or keep planning\n\
                    - /todos : Show the current task list\n\
                    - /todo <add|done|remove|clear|mode> ... : Update tasks or choose next/force updates\n\
                    - /agents : Show in-process subagent status\n\
                    - /attachments : List draft attachment blocks\n\
                    - /attach <path> : Attach a text file or image\n\
                    - /remove <n> : Remove a draft attachment block\n\
                    - /edit <n> : Edit a draft text attachment\n\
                    - /session <save|clear|path> : Manage the low-write resumable session\n\
                    - /sessions : Browse, resume, export, or delete sessions\n\
                    - /resume <name|path> : Resume a saved session directly\n\
                    - /retry : Retry the last user request\n\
                    - /fork : Save the current conversation as a new session\n\
                    - /temp <float> : Adjust temperature (0.0 to 2.0)\n\
                    - /sys <instruction> : Update system prompt\n\
                    - /key <api_key> : Save the active provider API key\n\
                    - /copy : Copy last assistant response to system clipboard (or Ctrl+Y)\n\
                    - /clear or /new : Clear conversation history and start fresh\n\
                    - /save : Save config to disk\n\
                    - /quit or /exit : Exit application"
                );
            }
            "/compact" => {
                self.compact_history(tx);
            }
            "/plan" => match arg.to_ascii_lowercase().as_str() {
                "on" | "" => {
                    self.plan_mode = true;
                    self.set_status("Plan mode enabled");
                    self.add_message("system", "Plan mode enabled (read-only). Use /plan continue to ask for more planning.");
                    let _ = self.flush_session();
                }
                "continue" | "keep" => {
                    self.plan_mode = true;
                    self.add_message(
                        "user",
                        "Continue planning. Refine the proposed plan and do not execute changes.",
                    );
                    let _ = self.flush_session();
                    if self.state == EngineState::Idle {
                        self.trigger_generation(tx);
                    }
                }
                "off" | "approve" | "execute" => {
                    let structured = self.latest_model_has_plan();
                    self.plan_mode = false;
                    if structured {
                        self.add_message("system", "Structured plan approved. Starting execution.");
                        self.add_message("user", "The plan above is approved. Execute it now, using the todo list and reporting progress.");
                        let _ = self.flush_session();
                        if self.state == EngineState::Idle {
                            self.trigger_generation(tx);
                        }
                    } else {
                        self.set_status("Plan mode disabled");
                        self.add_message("system", "Plan mode disabled. No structured plan was detected, so execution was not started.");
                        let _ = self.flush_session();
                    }
                }
                _ => self.add_message("system", "Usage: /plan [on|off|continue]"),
            },
            "/todos" => self.add_message("system", self.format_todos()),
            "/agents" => self.add_message("system", self.tool_registry.agent_manager().status()),
            "/todo" => {
                let mut todo_parts = arg.splitn(2, |character: char| character.is_whitespace());
                let action = todo_parts.next().unwrap_or("list").to_ascii_lowercase();
                let value = todo_parts.next().map(str::trim).unwrap_or("");
                if action == "mode" {
                    let mode = match value.to_ascii_lowercase().as_str() {
                        "next" | "next_turn" => "next_turn",
                        "force" => "force",
                        _ => {
                            self.add_message("system", "Usage: /todo mode <next|force>");
                            return;
                        }
                    };
                    self.config.todo_change_mode = mode.to_string();
                    let _ = self.config.save();
                    self.add_message("system", format!("Todo change policy set to '{}'.", mode));
                    return;
                }
                let todo_changed = matches!(action.as_str(), "add" | "done" | "remove" | "clear");
                let result = match action.as_str() {
                    "list" => Ok(self.format_todos()),
                    "add" => self.tool_registry.todo_add(value),
                    "done" => value
                        .parse::<usize>()
                        .map_err(|_| "Usage: /todo done <id>".to_string())
                        .and_then(|id| self.tool_registry.todo_done(id)),
                    "remove" => value
                        .parse::<usize>()
                        .map_err(|_| "Usage: /todo remove <id>".to_string())
                        .and_then(|id| self.tool_registry.todo_remove(id)),
                    "clear" => self.tool_registry.todo_clear(),
                    _ => Err("Usage: /todo <list|add text|done id|remove id|clear>".to_string()),
                };
                match result {
                    Ok(message) => {
                        self.add_message("system", message);
                        if todo_changed {
                            self.note_todo_change(
                                format!("{} {}", action, value).trim().to_string(),
                                tx.clone(),
                            );
                        }
                        let _ = self.flush_session();
                    }
                    Err(error) => self.add_message("system", error),
                }
            }
            "/models" => {
                self.add_message(
                    "system",
                    format!("Fetching models from {}...", self.config.provider),
                );
                let client = self.client.clone();
                let configured_models = self.config.configured_models();
                let bootstrap_model = self.config.model.clone();
                let free_only = self.config.provider.eq_ignore_ascii_case("opencode-zen")
                    && self.config.get_api_key_for_active_provider().is_none();
                if free_only {
                    self.add_message(
                        "system",
                        "No Zen API key detected; showing -free models only.",
                    );
                }
                tokio::spawn(async move {
                    let res = match client.list_models().await {
                        Ok(mut models) => {
                            if free_only {
                                models.retain(|model| is_free_model_id(&model.id));
                            }
                            for id in configured_models {
                                if free_only
                                    && (!is_free_model_id(&id)
                                        || crate::client::is_zen_unsupported_model_id(&id))
                                {
                                    continue;
                                }
                                if !models.iter().any(|model| model.id == id) {
                                    models.push(crate::client::types::ModelInfo {
                                        id: id.clone(),
                                        display_name: id,
                                        description: "Configured provider model".to_string(),
                                        input_price_per_m: None,
                                        output_price_per_m: None,
                                        input_token_limit: None,
                                    });
                                }
                            }
                            Ok(models)
                        }
                        Err(_error) if free_only && configured_models.is_empty() => {
                            Ok(vec![crate::client::types::ModelInfo {
                                display_name: bootstrap_model.clone(),
                                id: bootstrap_model,
                                description: "Bootstrap free model (live catalog unavailable)"
                                    .to_string(),
                                input_price_per_m: None,
                                output_price_per_m: None,
                                input_token_limit: None,
                            }])
                        }
                        Err(_error) if !configured_models.is_empty() => Ok(configured_models
                            .into_iter()
                            .filter(|id| {
                                !free_only
                                    || (is_free_model_id(id)
                                        && !crate::client::is_zen_unsupported_model_id(id))
                            })
                            .map(|id| crate::client::types::ModelInfo {
                                display_name: id.clone(),
                                id,
                                description:
                                    "Configured provider model (API model listing unavailable)"
                                        .to_string(),
                                input_price_per_m: None,
                                output_price_per_m: None,
                                input_token_limit: None,
                            })
                            .collect()),
                        Err(error) => Err(error),
                    };
                    let _ = tx.send(AppEvent::ModelsFetched(res));
                });
            }
            "/providers" => {
                let mut lines = vec!["Available providers:".to_string()];
                for (name, provider) in &self.config.providers {
                    let marker = if name == &self.config.provider {
                        "*"
                    } else {
                        " "
                    };
                    let model = provider.model.as_deref().unwrap_or("(choose a model)");
                    lines.push(format!(
                        "{} {:<18} kind={} protocol={}  model={}",
                        marker, name, provider.kind, provider.protocol, model
                    ));
                }
                if !self.config.providers.contains_key(&self.config.provider) {
                    lines.push(format!(
                        "* {:<18} kind={}  model={}",
                        self.config.provider, "openai-compatible", self.config.model
                    ));
                }
                lines.push("Supported kinds: gemini, openai-compatible (custom endpoints, OpenRouter, Zen, local servers)".to_string());
                lines.push("Use /provider <name> to switch.".to_string());
                self.add_message("system", lines.join("\n"));
            }
            "/provider" => {
                if arg.is_empty() {
                    self.add_message(
                        "system",
                        format!("Current provider: {}", self.config.provider),
                    );
                } else {
                    self.config.select_provider(arg);
                    let _ = self.config.save();
                    let provider_config = self.config.active_provider_config();
                    let provider = ProviderKind::parse(&provider_config.kind);
                    self.client.update_provider(
                        provider,
                        provider_config
                            .base_url
                            .or_else(|| self.config.base_url.clone()),
                        provider_config.headers,
                        provider_config.stream_usage,
                        crate::client::ProviderProtocol::parse(&provider_config.protocol),
                    );
                    if let Some(api_key) = self.config.get_api_key_for_active_provider() {
                        self.client.update_api_key(api_key);
                    }
                    self.set_status(format!("Provider set to {}", self.config.provider));
                    self.add_message(
                        "system",
                        format!("Provider set to {}", self.config.provider),
                    );
                }
            }
            "/baseurl" => {
                if arg.is_empty() {
                    let base_url = self
                        .config
                        .active_provider_config()
                        .base_url
                        .or_else(|| self.config.base_url.clone());
                    self.add_message(
                        "system",
                        format!(
                            "Current base URL: {}",
                            base_url.as_deref().unwrap_or("provider default")
                        ),
                    );
                } else {
                    let base_url = if arg.eq_ignore_ascii_case("default") {
                        None
                    } else {
                        Some(arg.to_string())
                    };
                    if let Some(provider) = self.config.providers.get_mut(&self.config.provider) {
                        provider.base_url = base_url.clone();
                    } else {
                        self.config.base_url = base_url.clone();
                    }
                    let provider_config = self.config.active_provider_config();
                    self.client.update_provider(
                        ProviderKind::parse(&provider_config.kind),
                        provider_config
                            .base_url
                            .or_else(|| self.config.base_url.clone()),
                        provider_config.headers,
                        provider_config.stream_usage,
                        crate::client::ProviderProtocol::parse(&provider_config.protocol),
                    );
                    self.set_status("Provider base URL updated");
                    self.add_message(
                        "system",
                        "Provider base URL updated. Use /save to persist it.",
                    );
                }
            }
            "/config" => {
                let path = AppConfig::config_path();
                match arg.to_ascii_lowercase().as_str() {
                    "" | "path" => self.add_message(
                        "system",
                        format!(
                            "Config path: {}",
                            path.as_ref()
                                .map(|path| path.display().to_string())
                                .unwrap_or_else(|| "unavailable".to_string())
                        ),
                    ),
                    "dir" => self.add_message(
                        "system",
                        format!(
                            "Config directory: {}",
                            path.as_ref()
                                .and_then(|path| path.parent())
                                .map(|path| path.display().to_string())
                                .unwrap_or_else(|| "unavailable".to_string())
                        ),
                    ),
                    "open" => match path {
                        Some(path) => {
                            if !path.exists() {
                                let _ = self.config.save();
                            }
                            match open_config_file(&path) {
                                Ok(()) => self.add_message(
                                    "system",
                                    format!("Opened config: {}", path.display()),
                                ),
                                Err(error) => self.add_message("system", error),
                            }
                        }
                        None => self.add_message("system", "Config path is unavailable."),
                    },
                    _ => self.add_message("system", "Usage: /config <path|open|dir>"),
                }
            }
            "/model" => {
                if arg.is_empty() {
                    self.add_message("system", format!("Current model: {}", self.config.model));
                } else {
                    self.config.model = arg.to_string();
                    self.config.remember_model();
                    let _ = self.config.save();
                    self.set_status(format!("Model set to: {}", self.config.model));
                    self.add_message(
                        "system",
                        format!("Active model switched to: {}", self.config.model),
                    );
                }
            }
            "/thinking" => {
                if matches!(
                    arg.to_ascii_lowercase().as_str(),
                    "off" | "minimal" | "low" | "medium" | "med" | "high"
                ) {
                    self.set_active_thinking_mode(arg);
                } else if let Ok(b) = arg.parse::<i32>() {
                    let budget = b.max(0);
                    let key = self.config.model_profile_key();
                    let profile = self.config.model_profiles.entry(key).or_default();
                    profile.thinking_budget = Some(budget);
                    profile.reasoning_enabled = Some(budget > 0);
                    profile.reasoning_effort = None;
                    let _ = self.config.save();
                    self.set_status(format!(
                        "Thinking budget set to {} for {}",
                        budget, self.config.model
                    ));
                    self.add_message(
                        "system",
                        format!(
                            "Thinking budget set to {} tokens for {}",
                            budget, self.config.model
                        ),
                    );
                } else {
                    self.open_thinking_modal();
                    self.add_message(
                        "system",
                        "Thinking picker opened: choose a supported model effort level.",
                    );
                }
            }
            "/reasoning" => {
                if arg.is_empty() {
                    self.open_thinking_modal();
                    self.add_message(
                        "system",
                        "Thinking picker opened: choose a supported model effort level.",
                    );
                } else if arg.eq_ignore_ascii_case("on") || arg.eq_ignore_ascii_case("off") {
                    let current_mode = self.thinking_mode(&self.config.model);
                    let mode = if arg.eq_ignore_ascii_case("off") {
                        "off".to_string()
                    } else if current_mode == "off" {
                        "medium".to_string()
                    } else {
                        current_mode
                    };
                    self.set_active_thinking_mode(&mode);
                } else if matches!(
                    arg.to_ascii_lowercase().as_str(),
                    "minimal" | "low" | "medium" | "med" | "high"
                ) {
                    self.set_active_thinking_mode(arg);
                } else if let Ok(budget) = arg.parse::<i32>() {
                    let key = self.config.model_profile_key();
                    let profile = self.config.model_profiles.entry(key).or_default();
                    profile.thinking_budget = Some(budget.max(0));
                    profile.reasoning_enabled = Some(budget > 0);
                    profile.reasoning_effort = None;
                    let _ = self.config.save();
                    self.set_status(format!(
                        "Reasoning budget set to {} for {}",
                        budget.max(0),
                        self.config.model
                    ));
                } else {
                    self.open_thinking_modal();
                    self.add_message(
                        "system",
                        "Thinking picker opened: choose off, low, medium, or high.",
                    );
                }
            }
            "/autocompact" => {
                if arg.is_empty() {
                    self.add_message(
                        "system",
                        format!(
                            "Automatic compaction: {} at ~{} tokens",
                            if self.config.auto_compact {
                                "on"
                            } else {
                                "off"
                            },
                            self.config.auto_compact_threshold_tokens
                        ),
                    );
                } else if arg.eq_ignore_ascii_case("on") || arg.eq_ignore_ascii_case("off") {
                    self.config.auto_compact = arg.eq_ignore_ascii_case("on");
                    self.set_status(format!(
                        "Automatic compaction {}",
                        if self.config.auto_compact {
                            "enabled"
                        } else {
                            "disabled"
                        }
                    ));
                } else if let Ok(tokens) = arg.parse::<u64>() {
                    self.config.auto_compact_threshold_tokens = tokens.max(1_000);
                    self.config.auto_compact = true;
                    self.set_status(format!(
                        "Automatic compaction threshold set to {} tokens",
                        self.config.auto_compact_threshold_tokens
                    ));
                } else {
                    self.add_message("system", "Usage: /autocompact <on|off|token-threshold>");
                }
            }
            "/session" => match arg.to_ascii_lowercase().as_str() {
                "save" => match self.flush_session() {
                    Ok(()) => self.add_message("system", "Session saved."),
                    Err(error) => {
                        self.add_message("system", format!("Session save failed: {}", error))
                    }
                },
                "clear" => {
                    self.messages.clear();
                    self.usage_records.clear();
                    self.prompt_tokens = 0;
                    self.candidates_tokens = 0;
                    self.total_tokens = 0;
                    self.context_tokens = 0;
                    self.context_tokens_estimated = true;
                    self.session_messages_at_save = 0;
                    self.tool_registry.todo_clear().ok();
                    self.tool_registry.clear_discovered_tools();
                    self.pending_todo_notice = None;
                    let _ = self.flush_session();
                    self.add_message("system", "Session cleared and saved.");
                }
                "path" => self.add_message(
                    "system",
                    format!(
                        "Session path: {}",
                        self.session_path
                            .as_ref()
                            .map(|path| path.display().to_string())
                            .unwrap_or_else(|| "disabled".to_string())
                    ),
                ),
                _ => self.add_message("system", "Usage: /session <save|clear|path>"),
            },
            "/sessions" => {
                self.refresh_sessions();
                self.show_sessions_modal = true;
            }
            "/resume" => {
                let path = PathBuf::from(arg);
                let path = if path.is_file() {
                    Some(path)
                } else {
                    session::list()
                        .into_iter()
                        .find(|info| info.name == arg || info.name.eq_ignore_ascii_case(arg))
                        .map(|info| info.path)
                };
                match path {
                    Some(path) => match self.restore_session(&path) {
                        Ok(()) => self.add_message(
                            "system",
                            format!("Resumed session '{}'.", path.display()),
                        ),
                        Err(error) => self.add_message("system", error),
                    },
                    None => {
                        self.add_message("system", "Usage: /resume <saved-session-name-or-path>")
                    }
                }
            }
            "/temp" => {
                if let Ok(t) = arg.parse::<f32>() {
                    self.config.temperature = t.clamp(0.0, 2.0);
                    self.set_status(format!(
                        "Temperature set to: {:.2}",
                        self.config.temperature
                    ));
                    self.add_message(
                        "system",
                        format!("Temperature set to: {:.2}", self.config.temperature),
                    );
                } else {
                    self.add_message(
                        "system",
                        format!(
                            "Current temperature: {:.2}. Use /temp <float>",
                            self.config.temperature
                        ),
                    );
                }
            }
            "/sys" => {
                if arg.is_empty() {
                    self.add_message(
                        "system",
                        format!("Current system prompt:\n{}", self.config.system_instruction),
                    );
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
                            self.add_message(
                                "system",
                                "Provider API key updated and stored securely.",
                            );
                        }
                        Err(e) => {
                            self.add_message("system", format!("Failed to store key: {}", e));
                        }
                    }
                }
            }
            "/clear" => {
                self.messages.clear();
                self.usage_records.clear();
                self.chat_scroll = 0;
                self.prompt_tokens = 0;
                self.candidates_tokens = 0;
                self.total_tokens = 0;
                self.context_tokens = 0;
                self.context_tokens_estimated = true;
                self.tool_registry.todo_clear().ok();
                self.tool_registry.clear_discovered_tools();
                self.pending_todo_notice = None;
                self.set_status("Session cleared");
                self.add_message("system", "Conversation history cleared.");
                let _ = self.flush_session();
            }
            "/retry" => {
                if let Some(last_user) = self
                    .messages
                    .iter()
                    .rposition(|message| message.role == "user")
                {
                    self.messages.truncate(last_user + 1);
                    self.current_thought_buffer.clear();
                    self.current_response_buffer.clear();
                    self.pending_tool_call = None;
                    self.queued_tool_calls.clear();
                    self.pending_tool_executions = 0;
                    self.state = EngineState::Idle;
                    let _ = self.flush_session();
                    self.set_status("Retrying last request");
                    self.trigger_generation(tx);
                } else {
                    self.add_message("system", "No user request is available to retry.");
                }
            }
            "/fork" => {
                self.session_path = session::new_session_path(&self.config.session_name);
                self.session_messages_at_save = 0;
                self.add_message("system", "Forked the conversation into a new session.");
                match self.flush_session() {
                    Ok(()) => self.set_status("Conversation forked into a new session"),
                    Err(error) => self.add_message("system", error),
                }
            }
            "/new" => {
                self.messages.clear();
                self.usage_records.clear();
                self.chat_scroll = 0;
                self.prompt_tokens = 0;
                self.candidates_tokens = 0;
                self.total_tokens = 0;
                self.context_tokens = 0;
                self.context_tokens_estimated = true;
                self.tool_registry.todo_clear().ok();
                self.tool_registry.clear_discovered_tools();
                self.pending_todo_notice = None;
                self.session_path = session::new_session_path(&self.config.session_name);
                self.session_messages_at_save = 0;
                self.set_status("New session started");
                self.add_message("system", "Started a new session.");
                let _ = self.flush_session();
            }
            "/usage" => {
                let prompt: u64 = self.usage_records.iter().map(|r| r.prompt_tokens).sum();
                let candidates: u64 = self.usage_records.iter().map(|r| r.candidates_tokens).sum();
                let total: u64 = self.usage_records.iter().map(|r| r.total_tokens).sum();
                self.add_message(
                    "system",
                    format!(
                        "Session usage: {} requests, {} input + {} output = {} tokens{}",
                        self.usage_records.len(),
                        prompt,
                        candidates,
                        total,
                        if self.usage_records.iter().any(|record| record.estimated) {
                            " (some estimated)"
                        } else {
                            ""
                        }
                    ),
                );
            }
            "/context" => {
                let limit = self.context_limit();
                self.add_message(
                    "system",
                    format!(
                        "Next-request context: {} / {} tokens ({}; {:.1}%). Last request: {} input + {} output = {} total.",
                        self.context_tokens,
                        limit,
                        if self.context_tokens_estimated { "estimated" } else { "provider reported" },
                        (self.context_tokens as f64 / limit.max(1) as f64) * 100.0,
                        self.prompt_tokens,
                        self.candidates_tokens,
                        self.total_tokens
                    ),
                );
            }
            "/status" => {
                self.add_message(
                    "system",
                    format!(
                        "State: {:?}\nProvider: {}\nModel: {}\nContext: {} / {}{}\nSession: {}",
                        self.state,
                        self.config.provider,
                        self.config.model,
                        self.context_tokens,
                        self.context_limit(),
                        if self.context_tokens_estimated {
                            " (estimated)"
                        } else {
                            ""
                        },
                        self.session_path
                            .as_ref()
                            .map(|path| path.display().to_string())
                            .unwrap_or_else(|| "disabled".to_string())
                    ),
                );
            }
            "/pwd" => {
                self.add_message(
                    "system",
                    format!(
                        "Working directory: {}\nProject root: {}",
                        self.tool_registry.working_dir().display(),
                        self.project_root
                            .as_ref()
                            .map(|path| path.display().to_string())
                            .unwrap_or_else(|| "not detected".to_string())
                    ),
                );
            }
            "/tools" => {
                let all_tools = self.tool_registry.names();
                let discovered = self.tool_registry.discovered_tools();
                self.add_message(
                    "system",
                    format!(
                        "Registered tools ({}):\n- {}\n\nDiscovered hidden tools for this session ({}):\n- {}",
                        all_tools.len(),
                        all_tools.join("\n- "),
                        discovered.len(),
                        if discovered.is_empty() {
                            "none".to_string()
                        } else {
                            discovered.join("\n- ")
                        }
                    ),
                );
            }
            "/attachments" => {
                if self.draft_attachments.is_empty() {
                    self.add_message("system", "No draft attachments.");
                } else {
                    let lines = self
                        .draft_attachments
                        .iter()
                        .enumerate()
                        .map(|(index, attachment)| {
                            format!(
                                "{}: {} ({})",
                                index + 1,
                                attachment_name(attachment),
                                match attachment {
                                    DraftAttachment::Text { text, .. } => {
                                        format!("text, {} chars", text.chars().count())
                                    }
                                    DraftAttachment::Image { .. } => "image".to_string(),
                                }
                            )
                        })
                        .collect::<Vec<_>>();
                    self.add_message("system", lines.join("\n"));
                }
            }
            "/attach" => {
                if arg.is_empty() {
                    self.add_message("system", "Usage: /attach <path>");
                } else {
                    self.attach_path(arg);
                }
            }
            "/remove" => match arg.parse::<usize>() {
                Ok(index) if index > 0 => self.remove_attachment(index - 1),
                _ => self.add_message("system", "Usage: /remove <attachment-number>"),
            },
            "/edit" => match arg.parse::<usize>() {
                Ok(index) if index > 0 => self.edit_attachment(index - 1),
                _ => self.add_message("system", "Usage: /edit <text-attachment-number>"),
            },
            "/save" => match self.config.save() {
                Ok(()) => {
                    self.set_status("Configuration saved");
                    self.add_message("system", "Configuration saved to disk.");
                }
                Err(e) => {
                    self.add_message("system", format!("Error saving configuration: {}", e));
                }
            },
            "/copy" => {
                self.copy_last_response();
            }
            "/quit" | "/exit" => {
                self.should_quit = true;
            }
            _ => {
                self.add_message(
                    "system",
                    format!("Unknown command: '{}'. Type /help for assistance.", cmd),
                );
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
                        use std::process::Stdio;
                        use tokio::io::AsyncWriteExt;
                        use tokio::process::Command;

                        if let Ok(mut child) = Command::new("clip").stdin(Stdio::piped()).spawn() {
                            if let Some(mut stdin) = child.stdin.take() {
                                let _ = stdin.write_all(text.as_bytes()).await;
                            }
                            let _ = child.wait().await;
                        }
                    }

                    #[cfg(not(windows))]
                    {
                        use std::process::Stdio;
                        use tokio::io::AsyncWriteExt;
                        use tokio::process::Command;

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
                self.add_message(
                    "system",
                    format!(
                        "Copied last assistant response ({} chars) to system clipboard.",
                        char_len
                    ),
                );
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

        if let Some(notice) = self.pending_todo_notice.take() {
            self.add_message("system", notice);
        }

        let context_estimate = self.estimated_context_tokens();
        self.context_tokens = context_estimate;
        self.context_tokens_estimated = true;
        self.request_context_tokens = context_estimate;

        let output_budget = self
            .config
            .active_model_profile()
            .max_output_tokens
            .unwrap_or(8192) as u64;
        let context_limit = self.context_limit();
        let output_would_overflow = context_estimate.saturating_add(output_budget) >= context_limit;
        if self.config.auto_compact
            && self.messages.len() > 2
            && (context_estimate >= self.config.auto_compact_threshold_tokens
                || output_would_overflow)
        {
            self.pending_generation_after_compaction = true;
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
        self.prompt_tokens = 0;
        self.candidates_tokens = 0;
        self.total_tokens = 0;
        self.request_usage_received = false;
        self.request_started_at = Some(std::time::Instant::now());
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
            client
                .stream_generate_content(&model, &fallback_models, max_retries, &request, stream_tx)
                .await;
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
        let request = self.build_request();
        let serialized_bytes = serde_json::to_vec(&request)
            .map(|bytes| bytes.len())
            .unwrap_or_default();
        ((serialized_bytes as u64) / 4).max(1)
    }

    pub fn cancel_generation(&mut self) {
        if let Some(handle) = self.active_stream_task.take() {
            handle.abort();
        }

        self.record_request_usage("cancelled");

        if !self.current_thought_buffer.is_empty() {
            let thought = std::mem::take(&mut self.current_thought_buffer);
            self.add_message("thought", thought);
        }

        if !self.current_response_buffer.is_empty() {
            let resp = std::mem::take(&mut self.current_response_buffer);
            self.add_message("model", resp);
        }

        let _ = self.flush_session();

        self.state = EngineState::Idle;
        self.pending_tool_executions = 0;
        self.pending_tool_call = None;
        self.queued_tool_calls.clear();
        self.stream_start_time = None;
        self.set_status("Generation stopped");
    }

    pub fn handle_stream_signal(
        &mut self,
        epoch: u64,
        sig: StreamSignal,
        tx: UnboundedSender<AppEvent>,
    ) {
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
            StreamSignal::ToolCall {
                id,
                name,
                args,
                thought_signature,
            } => {
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
                    attachments: Vec::new(),
                });

                if let Some(tool) = self.tool_registry.get(&name) {
                    let preview = tool.generate_preview(&args);

                    if self.plan_mode && preview.is_mutation {
                        self.add_message(
                            "system",
                            format!("Plan mode blocked mutation tool '{}'. Use /plan off when ready to execute.", name),
                        );
                        self.state = EngineState::ExecutingTool;
                        let _ = tx.send(AppEvent::ToolExecutionResult {
                            tool_name: name,
                            call_id: id,
                            result: Err("Mutation blocked while plan mode is enabled.".to_string()),
                        });
                    } else if self.session_allowed_tools.contains(&name) {
                        self.execute_tool(name, id, args, tx);
                    } else {
                        let pending = PendingToolCall {
                            call_id: id,
                            tool_name: name,
                            args,
                            preview,
                        };
                        if self.pending_tool_call.is_none() {
                            // Pause and trigger HITL modal.
                            self.state = EngineState::AwaitingHitlApproval;
                            self.set_status(format!(
                                "HITL Gate: Approval needed for '{}'",
                                pending.tool_name
                            ));
                            self.modal_scroll = 0;
                            self.pending_tool_call = Some(pending);
                        } else {
                            self.queued_tool_calls.push_back(pending);
                        }
                    }
                } else {
                    self.messages.pop();
                    self.add_message(
                        "system",
                        format!("Warning: Model attempted to call unknown tool '{}'", name),
                    );
                }
            }
            StreamSignal::Usage {
                prompt_tokens,
                candidates_tokens,
                total_tokens,
            } => {
                self.prompt_tokens = prompt_tokens;
                self.candidates_tokens = candidates_tokens;
                self.total_tokens = total_tokens;
                if prompt_tokens > 0 {
                    self.context_tokens = prompt_tokens;
                    self.context_tokens_estimated = false;
                }
                self.request_usage_received = true;

                if let Some(start) = self.stream_start_time {
                    let elapsed = start.elapsed().as_secs_f64();
                    if elapsed > 0.05 {
                        self.current_tps = self.candidates_tokens as f64 / elapsed;
                    }
                }
            }
            StreamSignal::Finished { finish_reason } => {
                self.record_request_usage("ok");

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
                    if self.plan_mode && self.latest_model_has_plan() {
                        self.show_plan_modal = true;
                        self.plan_modal_selected = 0;
                        self.set_status("Plan ready for review");
                    } else {
                        self.set_status(format!("Done ({})", reason));
                    }
                }
                self.stream_start_time = None;
                let _ = self.flush_session();
            }
            StreamSignal::Notice(message) => {
                self.set_status(&message);
                self.add_message("system", message);
            }
            StreamSignal::Error(err) => {
                self.record_request_usage("error");

                if !self.current_thought_buffer.is_empty() {
                    let thought = std::mem::take(&mut self.current_thought_buffer);
                    self.add_message("thought", thought);
                }
                if !self.current_response_buffer.is_empty() {
                    let response = std::mem::take(&mut self.current_response_buffer);
                    self.add_message("model", response);
                }
                let _ = self.flush_session();
                self.add_message("system", format!("Error: {}", err));
                // A failed stream cannot resume a tool approval that was
                // emitted by that stream. Drop it so the UI cannot leave a
                // ghost HITL modal over the normal input box.
                self.pending_tool_call = None;
                self.queued_tool_calls.clear();
                self.state = EngineState::Idle;
                self.set_status("Stream ended with error.");
            }
        }
    }

    pub fn approve_pending_tool(
        &mut self,
        whitelist_for_session: bool,
        tx: UnboundedSender<AppEvent>,
    ) {
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
            self.add_message(
                "system",
                format!("Denied execution of tool '{}'", pending.tool_name),
            );

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
        if tool_name == "search_tools" {
            self.tool_registry.discover_from_query(&args);
        }
        self.state = EngineState::ExecutingTool;
        self.pending_tool_executions = self.pending_tool_executions.saturating_add(1);
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
        } else {
            // Keep the turn's accounting balanced even if an MCP tool
            // disappears between declaration and execution.
            self.pending_tool_executions = self.pending_tool_executions.saturating_sub(1);
            let _ = tx.send(AppEvent::ToolExecutionResult {
                tool_name,
                call_id,
                result: Err("Tool is no longer available.".to_string()),
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

        // Keep tool activity compact in the transcript. The complete result is
        // still sent to the provider (within the normal safety cap) and kept
        // in the persisted function-response message; the UI should not turn
        // a 10,000-line grep into 10,000 visible lines.
        let model_output = truncate_tool_output(&output_str);
        self.add_message(
            "tool",
            format!(
                "{} {} · {} chars{}",
                if is_err { "✗" } else { "✓" },
                tool_name,
                output_str.chars().count(),
                output_str
                    .lines()
                    .next()
                    .filter(|line| !line.trim().is_empty())
                    .map(|line| format!(" · {}", one_line_preview(line, 96)))
                    .unwrap_or_default()
            ),
        );

        // Append assistant tool_call & user tool_response to internal conversation history
        let response_payload = if is_err {
            json!({ "error": model_output })
        } else {
            json!({ "output": model_output })
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
            attachments: Vec::new(),
        });
        let _ = self.flush_session();

        // Resume generation only after every tool from this assistant turn has
        // returned. Starting immediately for the first result races the other
        // tool tasks and can abort/restart an in-flight model request.
        self.pending_tool_executions = self.pending_tool_executions.saturating_sub(1);
        if self.pending_tool_executions > 0 {
            self.state = EngineState::ExecutingTool;
            return;
        }

        if let Some(next) = self.queued_tool_calls.pop_front() {
            self.pending_tool_call = Some(next);
            self.state = EngineState::AwaitingHitlApproval;
            self.set_status("HITL Gate: Approval needed for the next tool");
            self.modal_scroll = 0;
            return;
        }

        // Resume generation so model can synthesize an answer from the full
        // batch of tool results.
        self.state = EngineState::Idle;
        self.trigger_generation(tx);
    }

    pub fn build_request(&self) -> crate::client::types::GenerateContentRequest {
        let mut contents: Vec<crate::client::types::Content> = Vec::new();
        // A process/API failure can leave a persisted assistant tool call
        // without its matching output. OpenAI Responses rejects the entire
        // next request in that situation ("No tool output found ..."). Only
        // replay calls that have a recorded response; this lets a resumed
        // session recover instead of replaying the same orphan forever.
        let completed_tool_call_ids: HashSet<String> = self
            .messages
            .iter()
            .filter(|message| message.role == "function")
            .filter_map(|message| serde_json::from_str::<Part>(&message.content).ok())
            .filter_map(|part| match part {
                Part::FunctionResponse { function_response } => function_response.id,
                _ => None,
            })
            .collect();
        let filter_orphaned_calls = self
            .config
            .active_provider_config()
            .kind
            .eq_ignore_ascii_case("openai-compatible");

        for m in &self.messages {
            if m.role == "thought" || m.role == "tool" {
                continue;
            }

            // UI/system notices are intentionally not sent to the provider,
            // but a compaction summary is durable conversation state and must
            // be replayed after a restart or restore.
            if m.role == "system" && !m.content.starts_with("Compact History Summary:") {
                continue;
            }
            if m.role == "summary" || m.role == "system" {
                contents.push(Content {
                    role: Some("user".to_string()),
                    parts: vec![Part::Text {
                        text: m.content.clone(),
                        thought: None,
                    }],
                });
                continue;
            }

            if m.role == "model_tool_call" {
                if let Ok(part) = serde_json::from_str::<Part>(&m.content) {
                    if filter_orphaned_calls {
                        if let Part::FunctionCall { function_call, .. } = &part {
                            if function_call
                                .id
                                .as_ref()
                                .is_some_and(|id| !completed_tool_call_ids.contains(id))
                            {
                                continue;
                            }
                        }
                    }
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
                        if last.role.as_deref() == Some("user")
                            && last
                                .parts
                                .iter()
                                .any(|p| matches!(p, Part::FunctionResponse { .. }))
                        {
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
                let mut attached_text = String::new();
                let mut images = Vec::new();
                for attachment in &m.attachments {
                    match attachment.kind.as_str() {
                        "text" => {
                            if let Some(text) = &attachment.text {
                                attached_text.push_str(&format!(
                                    "\n\n[Attached text: {}]\n{}",
                                    attachment.name, text
                                ));
                            }
                        }
                        "image" => {
                            if let (Some(mime_type), Some(data)) =
                                (&attachment.mime_type, &attachment.data)
                            {
                                images.push(Part::InlineData {
                                    inline_data: InlineDataPayload {
                                        mime_type: mime_type.clone(),
                                        data: data.clone(),
                                    },
                                });
                            }
                        }
                        _ => {}
                    }
                }
                // Put attachments first and the actual user message last so
                // the model cannot lose the user's instruction after parsing
                // a large pasted block.
                let mut parts = Vec::new();
                parts.extend(images);
                if !attached_text.is_empty() {
                    parts.push(Part::Text {
                        text: attached_text,
                        thought: None,
                    });
                }
                parts.push(Part::Text {
                    text: m.content.clone(),
                    thought: None,
                });
                contents.push(Content {
                    role: Some("user".to_string()),
                    parts,
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
        let explicit_reasoning =
            model_profile.reasoning_effort.is_some() || model_profile.reasoning_enabled.is_some();
        let thinking_mode = if model_profile.reasoning_enabled == Some(false) {
            "off".to_string()
        } else {
            model_profile
                .reasoning_effort
                .clone()
                .unwrap_or_else(|| self.thinking_mode(&self.config.model))
        };
        let reasoning_enabled = !matches!(thinking_mode.as_str(), "off" | "none");
        let is_gemini = self
            .config
            .active_provider_config()
            .kind
            .eq_ignore_ascii_case("gemini");
        let uses_thinking_level =
            is_gemini && self.config.model.to_ascii_lowercase().contains("gemini-3");
        let thinking_config = if is_gemini && reasoning_enabled {
            if uses_thinking_level {
                Some(ThinkingConfig {
                    thinking_level: Some(thinking_mode.clone()),
                    thinking_budget: None,
                })
            } else {
                Some(ThinkingConfig {
                    thinking_level: None,
                    thinking_budget: Some(gemini_budget_for_mode(&thinking_mode)),
                })
            }
        } else if is_gemini && !reasoning_enabled {
            if uses_thinking_level {
                Some(ThinkingConfig {
                    thinking_level: Some(
                        self.thinking_choices(&self.config.model)
                            .first()
                            .copied()
                            .unwrap_or("low")
                            .to_string(),
                    ),
                    thinking_budget: None,
                })
            } else {
                Some(ThinkingConfig {
                    thinking_level: None,
                    thinking_budget: Some(0),
                })
            }
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
                    text: self.effective_system_instruction(),
                    thought: None,
                }],
            }),
            generation_config: Some(GenerationConfig {
                temperature: Some(model_profile.temperature.unwrap_or(self.config.temperature)),
                max_output_tokens: Some(model_profile.max_output_tokens.unwrap_or(8192)),
                thinking_config,
                reasoning_effort: if !is_gemini && explicit_reasoning {
                    Some(if reasoning_enabled {
                        thinking_mode
                    } else {
                        "none".to_string()
                    })
                } else {
                    None
                },
                extra: if model_profile.extra.is_empty() {
                    None
                } else {
                    Some(serde_json::Value::Object(
                        model_profile.extra.clone().into_iter().collect(),
                    ))
                },
            }),
            safety_settings: Some(safety_settings),
            tools: Some(tools),
        }
    }

    pub fn compact_history(&mut self, tx: UnboundedSender<AppEvent>) {
        let count = self.messages.len();
        if count <= 2 {
            self.pending_generation_after_compaction = false;
            self.state = EngineState::Idle;
            self.add_message(
                "system",
                "History is already minimal; compaction unnecessary.",
            );
            return;
        }

        self.set_status("Compacting conversation history...");
        self.state = EngineState::Compacting;
        self.add_message(
            "system",
            format!(
                "Compacting {} turns into a concise context summary...",
                count
            ),
        );

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
            let res = client
                .generate_content_with_fallback(&model, &fallback_models, max_retries, &request)
                .await;
            let _ = tx.send(AppEvent::CompactionFinished(res));
        });
    }

    pub fn handle_compaction_result(
        &mut self,
        result: Result<String, String>,
        tx: UnboundedSender<AppEvent>,
    ) {
        match result {
            Ok(summary) => {
                let original_count = self.messages.len();
                self.messages.clear();
                self.chat_scroll = 0;

                // Summary is a real conversation item. `build_request` maps it
                // to a user grounding turn so it survives session restore.
                let now = chrono::Local::now().format("%H:%M:%S").to_string();
                self.messages.push(ChatMessage {
                    role: "summary".to_string(),
                    content: format!("Compact History Summary:\n{}", summary),
                    timestamp: now,
                    attachments: Vec::new(),
                });

                self.set_status("Context compacted");
                self.add_message(
                    "system",
                    format!(
                        "Successfully compacted {} messages. Context window reclaimed.",
                        original_count
                    ),
                );
                let continue_generation = self.pending_generation_after_compaction;
                self.pending_generation_after_compaction = false;
                self.state = EngineState::Idle;
                let _ = self.flush_session();
                if continue_generation {
                    self.trigger_generation(tx);
                }
            }
            Err(e) => {
                self.set_status("Compaction failed");
                self.add_message("system", format!("Compaction failed: {}", e));
                self.pending_generation_after_compaction = false;
                self.state = EngineState::Idle;
            }
        }
    }
}

fn is_free_model_id(id: &str) -> bool {
    id.trim().to_ascii_lowercase().ends_with("-free")
}

const MAX_TOOL_RESULT_CHARS: usize = 32_000;

fn truncate_tool_output(output: &str) -> String {
    let char_count = output.chars().count();
    if char_count <= MAX_TOOL_RESULT_CHARS {
        return output.to_string();
    }
    let head_len = MAX_TOOL_RESULT_CHARS * 3 / 4;
    let tail_len = MAX_TOOL_RESULT_CHARS - head_len;
    let head = output.chars().take(head_len).collect::<String>();
    let tail = output
        .chars()
        .skip(char_count.saturating_sub(tail_len))
        .collect::<String>();
    format!(
        "{}\n...[tool output truncated; {} characters omitted]...\n{}",
        head,
        char_count.saturating_sub(head_len + tail_len),
        tail
    )
}

fn one_line_preview(value: &str, max_chars: usize) -> String {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut result = normalized.chars().take(max_chars).collect::<String>();
    if normalized.chars().count() > max_chars {
        result.push('…');
    }
    result
}

fn gemini_budget_for_mode(mode: &str) -> i32 {
    match mode {
        "minimal" | "low" => 1_024,
        "medium" => 8_192,
        "high" => 24_576,
        _ => 0,
    }
}

fn attachment_name(attachment: &DraftAttachment) -> &str {
    match attachment {
        DraftAttachment::Text { name, .. } | DraftAttachment::Image { name, .. } => name,
    }
}

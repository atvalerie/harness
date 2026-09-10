use crate::client::types::{
    Content, FunctionDeclaration, FunctionResponsePayload, GenerationConfig, InlineDataPayload,
    Part, SafetySetting, ThinkingConfig,
};
use crate::client::{AiClient, ProviderKind};
use crate::config::{AppConfig, PermissionMode, TOOL_PERMISSION_GROUPS};
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
    pub epoch: u64,
    pub call_id: Option<String>,
    pub tool_name: String,
    pub args: serde_json::Value,
    pub preview: ToolPreview,
}

pub struct App {
    pub config: AppConfig,
    pub interaction: crate::interaction::Interaction,
    pub review: crate::review::ReviewStore,
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
    /// Runtime-only override; never persisted into permission settings.
    pub auto_mode: bool,

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
    /// Number of unresolved tool calls belonging to the current assistant
    /// turn, including calls waiting for HITL approval.
    /// The next model request must wait until the entire batch is complete.
    pub pending_tool_executions: usize,
    /// The model stream must finish before a tool result can resume the turn.
    /// Responses providers may emit more function calls after the first one
    /// has reached the approval UI.
    tool_turn_stream_finished: bool,
    tool_turn_failed: bool,
    pub session_allowed_tools: HashSet<String>,
    pub modal_scroll: usize,

    // Models list cached
    pub available_models: Vec<crate::client::types::ModelInfo>,
    pub models_epoch: u64,
    pub models_scroll: usize,
    pub models_selected: usize,
    pub models_filter: String,
    pub models_searching: bool,
    pub thinking_selected: usize,
    pub thinking_target_model: Option<String>,
    pub plan_modal_selected: usize,
    pub permissions_selected: usize,
    pub available_sessions: Vec<session::SessionInfo>,
    pub sessions_selected: usize,

    // Metrics & Performance
    pub stream_epoch: u64,
    pub stream_start_time: Option<std::time::Instant>,
    pub candidate_chunks_count: u32,
    pub current_tps: f64,

    // Provider account limits shown in the status bar
    pub provider_limits: Option<String>,

    // Status bar notification
    pub status_message: Option<String>,
    pub should_quit: bool,
    pub pending_generation_after_compaction: bool,
    pub pending_todo_notice: Option<String>,
    pub session_path: Option<std::path::PathBuf>,
    pub session_messages_at_save: usize,
    pub project_root: Option<PathBuf>,
    pub project_instructions: String,
    /// Runtime-only instructions supplied by the active frontend session.
    /// These are never written to global configuration or session history.
    pub session_instruction: Option<String>,
    /// Runtime-only capability declarations supplied by the active frontend.
    pub session_capabilities: Vec<FunctionDeclaration>,
    /// Runtime-only tool profile selected by the active frontend.
    pub session_tool_profile: Option<String>,
    /// Runtime-only tools hidden from the active frontend session. This lets
    /// a specialized client remove generic or expensive capabilities without
    /// changing Holiday's global registry.
    pub session_disabled_tools: HashSet<String>,
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

    let project_root = ancestors
        .iter()
        .rev()
        .find(|directory| {
            directory.join("Cargo.toml").is_file()
                || directory.join(".git").exists()
                || directory.join("package.json").is_file()
        })
        .cloned();
    let mut root = project_root.clone();
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
        let review = crate::review::ReviewStore::default();
        if let Some(path) = &session_path {
            let _ = review.bind(path, false);
        }
        let mut interaction = crate::interaction::Interaction::default();
        interaction.credentials_available = !api_key.is_empty();
        interaction.dirty = true;
        interaction.set_overlay(
            crate::interaction::Overlay::Permissions,
            !config.permissions_configured,
        );
        let initial_working_dir = tool_registry.working_dir();
        let (project_root, project_instructions) = discover_project_context(&initial_working_dir);
        if let Some(root) = &project_root {
            let _ = tool_registry.set_working_dir(root.clone());
        }

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
            tool_turn_stream_finished: true,
            tool_turn_failed: false,
            session_allowed_tools: HashSet::new(),
            modal_scroll: 0,
            interaction,
            review,
            available_models: Vec::new(),
            models_epoch: 0,
            models_scroll: 0,
            models_selected: 0,
            models_filter: String::new(),
            models_searching: false,
            thinking_selected: 0,
            thinking_target_model: None,
            plan_modal_selected: 0,
            permissions_selected: 0,
            available_sessions: Vec::new(),
            sessions_selected: 0,
            stream_epoch: 0,
            stream_start_time: None,
            candidate_chunks_count: 0,
            current_tps: 0.0,
            provider_limits: None,
            status_message: Some("Ready".to_string()),
            should_quit: false,
            auto_mode: false,
            pending_generation_after_compaction: false,
            pending_todo_notice: None,
            session_path,
            session_messages_at_save: 0,
            project_root,
            project_instructions,
            session_instruction: None,
            session_capabilities: Vec::new(),
            session_tool_profile: None,
            session_disabled_tools: HashSet::new(),
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

    pub fn context_limit(&self) -> Option<u64> {
        self.config
            .active_model_profile()
            .context_window
            .filter(|limit| *limit > 0)
            .or(self
                .config
                .active_model_profile()
                .input_token_limit
                .filter(|limit| *limit > 0))
            .or_else(|| {
                self.available_models
                    .iter()
                    .find(|model| same_model_id(&model.id, &self.config.model))
                    .and_then(|model| model.context_window.or(model.input_token_limit))
            })
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
            AppConfig::get_codex_auth()
                .map(|auth| auth.account_id)
                .unwrap_or_default(),
        );
        self.client.update_api_key(
            self.config
                .get_api_key_for_active_provider()
                .unwrap_or_default(),
        );
        self.interaction.credentials_available =
            self.config.get_api_key_for_active_provider().is_some();
        self.models_epoch += 1;
        self.available_models.clear();
        self.provider_limits = None;
    }

    /// Refresh the active provider's model catalog without blocking the UI.
    /// The catalog is also the source of truth for model context limits.
    pub fn prefetch_models(&mut self, tx: UnboundedSender<AppEvent>) {
        self.spawn_models_fetch(tx, false);
    }

    /// Headless frontends resolve the same metadata before constructing a request.
    /// A failed catalog fetch leaves configured overrides usable.
    pub async fn ensure_models_loaded(&mut self) {
        if !self.available_models.is_empty() {
            return;
        }
        let free_only = self.config.provider.eq_ignore_ascii_case("opencode-zen")
            && self.config.get_api_key_for_active_provider().is_none();
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            fetch_models(
                self.client.clone(),
                self.config.configured_models(),
                self.config.model.clone(),
                free_only,
            ),
        )
        .await;
        if let Ok(Ok(mut models)) = result {
            crate::model_metadata::enrich(&self.config, &mut models).await;
            self.available_models = models;
        }
    }

    pub fn install_models(
        &mut self,
        epoch: u64,
        models: Vec<crate::client::types::ModelInfo>,
    ) -> bool {
        if epoch != self.models_epoch {
            return false;
        }
        self.available_models = models;
        true
    }

    pub fn output_budget(&self) -> u32 {
        let requested = self
            .config
            .active_model_profile()
            .max_output_tokens
            .unwrap_or(8192)
            .max(1);
        let limit = self
            .available_models
            .iter()
            .find(|model| same_model_id(&model.id, &self.config.model))
            .and_then(|model| model.output_token_limit)
            .filter(|limit| *limit > 0);
        limit
            .map(|limit| (requested as u64).min(limit) as u32)
            .unwrap_or(requested)
    }

    fn context_would_overflow(&self, context_estimate: u64) -> bool {
        let output_budget = self.output_budget() as u64;
        let profile = self.config.active_model_profile();
        let metadata = self
            .available_models
            .iter()
            .find(|model| same_model_id(&model.id, &self.config.model));
        let window = profile
            .context_window
            .filter(|limit| *limit > 0)
            .or_else(|| metadata.and_then(|model| model.context_window));
        let input_limit = profile
            .input_token_limit
            .filter(|limit| *limit > 0)
            .or_else(|| metadata.and_then(|model| model.input_token_limit));
        window.is_some_and(|limit| context_estimate.saturating_add(output_budget) >= limit)
            || input_limit.is_some_and(|limit| context_estimate >= limit)
    }

    fn spawn_models_fetch(&mut self, tx: UnboundedSender<AppEvent>, interactive: bool) {
        self.models_epoch += 1;
        let epoch = self.models_epoch;
        let client = self.client.clone();
        let configured_models = self.config.configured_models();
        let bootstrap_model = self.config.model.clone();
        let free_only = self.config.provider.eq_ignore_ascii_case("opencode-zen")
            && self.config.get_api_key_for_active_provider().is_none();

        let config = self.config.clone();
        tokio::spawn(async move {
            let mut result =
                fetch_models(client, configured_models, bootstrap_model, free_only).await;
            if let Ok(models) = &mut result {
                crate::model_metadata::enrich(&config, models).await;
            }
            let _ = tx.send(AppEvent::ModelsFetched {
                epoch,
                result,
                interactive,
            });
        });
    }

    fn refresh_context_estimate(&mut self) {
        self.context_tokens = self.estimated_context_tokens();
        self.context_tokens_estimated = true;
    }

    fn effective_system_instruction(&self) -> String {
        let mut instruction = crate::prompts::compose(
            &self.config.system_instruction,
            &self.project_instructions,
            self.plan_mode,
        );
        if let Some(session_instruction) = self
            .session_instruction
            .as_deref()
            .filter(|instruction| !instruction.trim().is_empty())
        {
            instruction.push_str("\n\n--- BEGIN Session instructions ---\n");
            instruction.push_str(session_instruction.trim());
            instruction.push_str("\n--- END Session instructions ---");
        }
        instruction
    }

    /// Sets instructions for the active frontend session only. The value is
    /// intentionally kept outside the persisted configuration and transcript.
    pub fn set_session_instruction(&mut self, instruction: Option<String>) {
        self.session_instruction = instruction.filter(|value| !value.trim().is_empty());
    }

    /// Selects a runtime-only tool profile for the active frontend session.
    /// Profile semantics remain frontend-owned; Holiday only preserves the
    /// value for diagnostics and future generic policy layers.
    pub fn set_session_tool_profile(&mut self, profile: Option<String>) {
        self.session_tool_profile = profile.filter(|value| !value.trim().is_empty());
    }

    /// Hides selected tools from requests built for the active frontend
    /// session. The registry remains available to other sessions and to
    /// Holiday's normal interactive mode.
    pub fn set_session_disabled_tools<I, S>(&mut self, names: I)
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        self.session_disabled_tools = names
            .into_iter()
            .map(|name| name.as_ref().trim().to_string())
            .filter(|name| !name.is_empty())
            .collect();
    }

    /// Installs function declarations for this frontend session only. String
    /// entries remain accepted for protocol compatibility.
    pub fn set_session_capabilities(
        &mut self,
        capabilities: &serde_json::Value,
    ) -> Result<(), String> {
        let Some(entries) = capabilities.as_array() else {
            return Err("session_config capabilities must be an array".to_string());
        };
        let mut parsed = Vec::with_capacity(entries.len());
        for entry in entries {
            if let Some(name) = entry.as_str() {
                parsed.push(FunctionDeclaration {
                    name: name.to_string(),
                    description: "A capability supplied by the active frontend session."
                        .to_string(),
                    parameters: serde_json::json!({"type":"object"}),
                });
                continue;
            }
            let Some(object) = entry.as_object() else {
                return Err("each session capability must be a string or object".to_string());
            };
            let name = object
                .get("name")
                .and_then(serde_json::Value::as_str)
                .filter(|name| !name.trim().is_empty())
                .ok_or_else(|| "session capability is missing a name".to_string())?;
            let description = object
                .get("description")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("A capability supplied by the active frontend session.");
            parsed.push(FunctionDeclaration {
                name: name.to_string(),
                description: description.to_string(),
                parameters: object
                    .get("parameters")
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!({"type":"object"})),
            });
        }
        self.session_capabilities = parsed;
        Ok(())
    }

    /// Activates optional tools for this frontend session only. The global
    /// tool registry and other sessions are unaffected.
    pub fn preload_session_tools(&self, names: &[String]) {
        self.tool_registry.preload_tools(names.iter());
    }

    pub fn is_session_capability(&self, name: &str) -> bool {
        self.session_capabilities
            .iter()
            .any(|capability| capability.name == name)
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
            self.interaction
                .set_overlay(crate::interaction::Overlay::Sessions, false);
            self.add_message("system", format!("Resumed session '{}'.", info.name));
        }
    }

    pub fn choose_plan_decision(&mut self, execute: bool, tx: UnboundedSender<AppEvent>) {
        if !self
            .interaction
            .is_overlay(crate::interaction::Overlay::Plan)
        {
            return;
        }
        self.interaction
            .set_overlay(crate::interaction::Overlay::Plan, false);
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

    pub fn cycle_permission_mode(&mut self, reverse: bool) {
        let Some((group, _)) = TOOL_PERMISSION_GROUPS.get(self.permissions_selected) else {
            return;
        };
        let current = self.config.permission_mode_for(group);
        self.config
            .set_permission_mode(group, current.cycle(reverse));
    }

    pub fn finish_permission_setup(&mut self) {
        self.config.permissions_configured = true;
        self.interaction
            .set_overlay(crate::interaction::Overlay::Permissions, false);
        match self.config.save() {
            Ok(()) => self.set_status("Tool permissions saved."),
            Err(error) => self.set_status(format!("Could not save tool permissions: {}", error)),
        }
    }

    pub fn restore_session(&mut self, path: &Path) -> Result<(), String> {
        let old = self
            .session_path
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        self.interaction.drafts.insert(
            old,
            (self.input_buffer.clone(), self.draft_attachments.clone()),
        );
        let snapshot = session::load(path)
            .ok_or_else(|| format!("Could not read session snapshot: {}", path.display()))?;
        self.interaction.reset_transcript();
        self.review = crate::review::ReviewStore::default();
        self.review.restore(snapshot.checkpoints);
        self.review.bind(path, true)?;
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
        self.interaction
            .set_overlay(crate::interaction::Overlay::Plan, false);
        self.plan_modal_selected = 0;
        self.request_started_at = None;
        self.request_usage_received = false;
        self.active_stream_task = None;
        self.session_path = Some(path.to_path_buf());
        let (text, blocks) = self
            .interaction
            .drafts
            .remove(&path.display().to_string())
            .unwrap_or_default();
        self.input_buffer = text;
        self.input_cursor = self.input_buffer.chars().count();
        self.draft_attachments = blocks;
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
        if session::delete(&info.path).is_ok() {
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
        if let Some(snapshot) = session::load(&info.path) {
            if serde_json::to_vec_pretty(&snapshot)
                .ok()
                .is_some_and(|bytes| std::fs::write(&export_path, bytes).is_ok())
            {
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
            if matches!(
                effort,
                "minimal" | "low" | "medium" | "high" | "xhigh" | "max"
            ) {
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
        {
            if let Some(levels) = self
                .available_models
                .iter()
                .find(|entry| entry.id == model)
                .map(|entry| entry.reasoning_levels.as_slice())
                .filter(|levels| !levels.is_empty())
            {
                let mut choices = Vec::new();
                for level in levels {
                    match level.as_str() {
                        "none" | "off" if !choices.contains(&"off") => choices.push("off"),
                        "minimal" if !choices.contains(&"minimal") => choices.push("minimal"),
                        "low" if !choices.contains(&"low") => choices.push("low"),
                        "medium" if !choices.contains(&"medium") => choices.push("medium"),
                        "high" if !choices.contains(&"high") => choices.push("high"),
                        "xhigh" if !choices.contains(&"xhigh") => choices.push("xhigh"),
                        "max" if !choices.contains(&"max") => choices.push("max"),
                        _ => {}
                    }
                }
                if !choices.is_empty() {
                    return choices;
                }
            }
        }
        if provider_kind.eq_ignore_ascii_case("codex") {
            return vec!["low", "medium", "high"];
        }
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
            "xhigh" => (true, 65536, Some("xhigh")),
            "max" => (true, 131072, Some("max")),
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
        self.interaction
            .set_overlay(crate::interaction::Overlay::Thinking, true);
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
        self.interaction
            .set_overlay(crate::interaction::Overlay::Thinking, false);
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
        self.interaction.dirty = true; // Preserve the reader viewport.
        self.refresh_context_estimate();
    }

    pub fn flush_session(&mut self) -> Result<(), String> {
        let Some(path) = &self.session_path else {
            return Ok(());
        };
        let snapshot = SessionSnapshot {
            schema_version: 3,
            checkpoints: self.review.snapshot(),
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
            model: self
                .interaction
                .actual_model
                .clone()
                .unwrap_or_else(|| self.config.model.clone()),
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

        if self.state != EngineState::Idle && !text.starts_with('/') {
            self.interaction
                .queue
                .push_back((text, std::mem::take(&mut self.draft_attachments)));
            self.input_buffer.clear();
            self.input_cursor = 0;
            self.set_status("Prompt queued. /interrupt <prompt> cancels and sends immediately.");
            return;
        }

        if !crate::commands::parse(&text)
            .is_ok_and(|(spec, _)| spec.id == crate::commands::CommandId::Key)
        {
            self.input_history.push(text.clone());
        }
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
        self.interaction
            .remember_edit(&self.input_buffer, self.input_cursor);
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
        let (spec, argument) = match crate::commands::parse(command_line) {
            Ok(parsed) => parsed,
            Err(error) => {
                self.add_message("system", error);
                return;
            }
        };
        if spec.idle_only && self.state != EngineState::Idle {
            self.set_status(
                "This command requires an idle engine. Cancel or wait for the current turn.",
            );
            return;
        }
        let arg = argument.as_str();
        match spec.id {
            crate::commands::CommandId::Auto => {
                match arg.trim().to_ascii_lowercase().as_str() {
                    "on" => self.auto_mode = true,
                    "off" => self.auto_mode = false,
                    "" | "status" => {}
                    _ => {
                        self.add_message("system", "Usage: /auto <on|off|status>");
                        return;
                    }
                }
                self.add_message("system", if self.auto_mode {
                    "AUTO REVIEW ON: codex-auto-review reviews ASK actions. ALLOW runs directly; DENY stays blocked. High-risk or unclear actions still ask you. Saved permissions are unchanged."
                } else {
                    "AUTO REVIEW OFF: normal per-group permissions restored (ASK remains manual; ALLOW and REVIEW retain their configured behavior)."
                });
            }
            crate::commands::CommandId::Help => self.add_message("system", crate::commands::help()),
            crate::commands::CommandId::Compact => {
                self.compact_history(tx);
            }
            crate::commands::CommandId::Plan => match arg.to_ascii_lowercase().as_str() {
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
            crate::commands::CommandId::Todos => self.add_message("system", self.format_todos()),
            crate::commands::CommandId::Agents => {
                self.add_message("system", self.tool_registry.agent_manager().status())
            }
            crate::commands::CommandId::Todo => {
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
            crate::commands::CommandId::Models => {
                self.add_message(
                    "system",
                    format!("Fetching models from {}...", self.config.provider),
                );
                let free_only = self.config.provider.eq_ignore_ascii_case("opencode-zen")
                    && self.config.get_api_key_for_active_provider().is_none();
                if free_only {
                    self.add_message(
                        "system",
                        "No Zen API key detected; showing -free models only.",
                    );
                }
                self.spawn_models_fetch(tx, true);
            }
            crate::commands::CommandId::Providers => {
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
                lines.push("Supported kinds: gemini, codex (ChatGPT OAuth/device login), openai-compatible (custom endpoints, OpenRouter, Zen, local servers)".to_string());
                lines.push("Use /provider <name> to switch.".to_string());
                self.add_message("system", lines.join("\n"));
            }
            crate::commands::CommandId::Provider => {
                if arg.is_empty() {
                    self.add_message(
                        "system",
                        format!("Current provider: {}", self.config.provider),
                    );
                } else {
                    self.config.select_provider(arg);
                    let _ = self.config.save();
                    self.refresh_client_from_config();
                    self.prefetch_models(tx.clone());
                    self.set_status(format!("Provider set to {}", self.config.provider));
                    self.add_message(
                        "system",
                        format!("Provider set to {}", self.config.provider),
                    );
                }
            }
            crate::commands::CommandId::Baseurl => {
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
                    self.refresh_client_from_config();
                    self.prefetch_models(tx.clone());
                    self.set_status("Provider base URL updated");
                    self.add_message(
                        "system",
                        "Provider base URL updated. Use /save to persist it.",
                    );
                }
            }
            crate::commands::CommandId::Config => {
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
            crate::commands::CommandId::Model => {
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
            crate::commands::CommandId::Thinking => {
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
            crate::commands::CommandId::Reasoning => {
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
            crate::commands::CommandId::Autocompact => {
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
            crate::commands::CommandId::Session => match arg.to_ascii_lowercase().as_str() {
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
            crate::commands::CommandId::Sessions => {
                self.refresh_sessions();
                self.interaction
                    .set_overlay(crate::interaction::Overlay::Sessions, true);
            }
            crate::commands::CommandId::Resume => {
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
            crate::commands::CommandId::Temp => {
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
            crate::commands::CommandId::Sys => {
                if arg.is_empty() {
                    self.add_message(
                        "system",
                        format!(
                            "Effective system prompt:\n{}",
                            self.effective_system_instruction()
                        ),
                    );
                } else {
                    self.config.system_instruction = if arg == "--clear" {
                        String::new()
                    } else {
                        arg.to_string()
                    };
                    self.set_status("System customization updated");
                    self.add_message("system", "System customization updated. Built-in guidance remains active. Use /save to persist.");
                }
            }
            crate::commands::CommandId::Key => {
                if arg.is_empty() {
                    self.add_message("system", "Usage: /key <your_provider_api_key>");
                } else {
                    match self.config.set_provider_api_key(arg) {
                        Ok(()) => {
                            self.client.update_api_key(arg.trim().to_string());
                            self.available_models.clear();
                            self.prefetch_models(tx.clone());
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
            crate::commands::CommandId::Clear => {
                self.interaction.reset_transcript();
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
            crate::commands::CommandId::Retry => {
                if self.messages.iter().any(|message| message.role == "user") {
                    // Retry the continuation without destroying completed tool work or history.
                    self.interaction.reset_transcript();
                    self.chat_scroll = 0;
                    self.current_thought_buffer.clear();
                    self.current_response_buffer.clear();
                    self.pending_tool_call = None;
                    self.queued_tool_calls.clear();
                    self.pending_tool_executions = 0;
                    self.state = EngineState::Idle;
                    let _ = self.flush_session();
                    self.set_status(
                        "Retrying with conversation and completed tool results preserved",
                    );
                    self.trigger_generation(tx);
                } else {
                    self.add_message("system", "No user request is available to retry.");
                }
            }
            crate::commands::CommandId::Fork => {
                self.session_path = session::new_session_path(&self.config.session_name);
                self.session_messages_at_save = 0;
                self.add_message("system", "Forked the conversation into a new session.");
                match self.flush_session() {
                    Ok(()) => self.set_status("Conversation forked into a new session"),
                    Err(error) => self.add_message("system", error),
                }
            }
            crate::commands::CommandId::New => {
                self.start_new_session();
            }
            crate::commands::CommandId::Usage => {
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
            crate::commands::CommandId::Limits => {
                let client = self.client.clone();
                tokio::spawn(async move {
                    let message = client
                        .usage_summary()
                        .await
                        .unwrap_or_else(|error| format!("Unable to fetch Codex usage: {error}"));
                    let _ = tx.send(AppEvent::SystemNotification(message));
                });
                self.add_message("system", "Fetching provider usage limits...");
            }
            crate::commands::CommandId::Context => {
                let limit = self.context_limit();
                let limit_text = limit
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "unknown".to_string());
                let percentage = limit
                    .map(|value| (self.context_tokens as f64 / value.max(1) as f64) * 100.0)
                    .map(|value| format!("{value:.1}%"))
                    .unwrap_or_else(|| "unknown".to_string());
                self.add_message(
                    "system",
                    format!(
                        "Next-request context: {} / {} tokens ({}; {}). Last request: {} input + {} output = {} total.",
                        self.context_tokens,
                        limit_text,
                        if self.context_tokens_estimated { "estimated" } else { "provider reported" },
                        percentage,
                        self.prompt_tokens,
                        self.candidates_tokens,
                        self.total_tokens
                    ),
                );
                self.add_message("system", self.context_breakdown());
            }
            crate::commands::CommandId::Status => {
                let context_limit = self
                    .context_limit()
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "unknown".to_string());
                self.add_message(
                    "system",
                    format!(
                        "State: {:?}\nProvider: {}\nModel: {}\nContext: {} / {}{}\nSession: {}",
                        self.state,
                        self.config.provider,
                        self.config.model,
                        self.context_tokens,
                        context_limit,
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
            crate::commands::CommandId::Pwd => {
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
            crate::commands::CommandId::Tools => {
                let all_tools = self.tool_registry.names();
                let discovered = self.tool_registry.discovered_tools();
                let metrics = self.tool_registry.context_metrics();
                self.add_message(
                    "system",
                    format!(
                        "Tool schema context: {} active / {} registered, ~{} chars (~{} tokens), {} discovered.\n\nRegistered tools ({}):\n- {}\n\nDiscovered optional tools for this session ({}):\n- {}",
                        metrics.active_tools,
                        metrics.registered_tools,
                        metrics.estimated_schema_chars,
                        metrics.estimated_schema_tokens,
                        metrics.discovered_tools,
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
            crate::commands::CommandId::Permissions => {
                self.interaction
                    .set_overlay(crate::interaction::Overlay::Permissions, true);
                self.permissions_selected = 0;
                self.modal_scroll = 0;
            }
            crate::commands::CommandId::Attachments => {
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
            crate::commands::CommandId::Attach => {
                if arg.is_empty() {
                    self.add_message("system", "Usage: /attach <path>");
                } else {
                    self.attach_path(arg);
                }
            }
            crate::commands::CommandId::Remove => match arg.parse::<usize>() {
                Ok(index) if index > 0 => self.remove_attachment(index - 1),
                _ => self.add_message("system", "Usage: /remove <attachment-number>"),
            },
            crate::commands::CommandId::Edit => match arg.parse::<usize>() {
                Ok(index) if index > 0 => self.edit_attachment(index - 1),
                _ => self.add_message("system", "Usage: /edit <text-attachment-number>"),
            },
            crate::commands::CommandId::Save => match self.config.save() {
                Ok(()) => {
                    self.set_status("Configuration saved");
                    self.add_message("system", "Configuration saved to disk.");
                }
                Err(e) => {
                    self.add_message("system", format!("Error saving configuration: {}", e));
                }
            },
            crate::commands::CommandId::Copy => {
                self.copy_selection(arg, tx);
            }
            crate::commands::CommandId::Quit => {
                self.should_quit = true;
            }
            other => self.handle_workspace_command(other, arg, tx),
        }
    }

    pub fn start_new_session(&mut self) {
        self.auto_mode = false;
        self.interaction.reset_transcript();
        let old = self
            .session_path
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        self.interaction.drafts.insert(
            old,
            (self.input_buffer.clone(), self.draft_attachments.clone()),
        );
        self.messages.clear();
        self.session_instruction = None;
        self.session_capabilities.clear();
        self.session_tool_profile = None;
        self.session_disabled_tools.clear();
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
        self.review = crate::review::ReviewStore::default();
        if let Some(path) = &self.session_path {
            let _ = self.review.bind(path, false);
        }
        self.session_messages_at_save = 0;
        self.set_status("New session started");
        self.add_message("system", "Started a new session.");
        let _ = self.flush_session();
    }

    pub fn copy_selection(&mut self, arg: &str, tx: UnboundedSender<AppEvent>) {
        let mut parts = arg.split_whitespace();
        let selection = parts.next().and_then(|s| s.parse::<usize>().ok());
        let message = if let Some(index) = selection {
            index.checked_sub(1).and_then(|i| self.messages.get(i))
        } else {
            self.messages.iter().rev().find(|m| m.role == "model")
        };
        let Some(message) = message else {
            self.set_status("No matching message to copy");
            return;
        };
        let mut text = message.content.clone();
        if parts.next() == Some("code") {
            let index = parts
                .next()
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or(1);
            let blocks = text
                .split("```")
                .enumerate()
                .filter(|(i, _)| i % 2 == 1)
                .map(|(_, b)| b.split_once('\n').map(|(_, code)| code).unwrap_or(b))
                .collect::<Vec<_>>();
            let Some(code) = index.checked_sub(1).and_then(|i| blocks.get(i)) else {
                self.set_status("Code block not found");
                return;
            };
            text = (*code).into();
        }
        tokio::spawn(async move {
            use tokio::io::AsyncWriteExt;
            let mut cmd = if cfg!(windows) {
                tokio::process::Command::new("clip")
            } else if cfg!(target_os = "macos") {
                tokio::process::Command::new("pbcopy")
            } else if std::env::var_os("WAYLAND_DISPLAY").is_some() {
                tokio::process::Command::new("wl-copy")
            } else {
                let mut c = tokio::process::Command::new("xclip");
                c.args(["-selection", "clipboard"]);
                c
            };
            #[cfg(windows)]
            {
                cmd.creation_flags(0x08000000);
            }
            let result = async {
                let mut child = cmd
                    .stdin(std::process::Stdio::piped())
                    .kill_on_drop(true)
                    .spawn()
                    .map_err(|e| e.to_string())?;
                if let Some(mut input) = child.stdin.take() {
                    input
                        .write_all(text.as_bytes())
                        .await
                        .map_err(|e| e.to_string())?;
                }
                if !child.wait().await.map_err(|e| e.to_string())?.success() {
                    return Err("Clipboard utility failed".to_string());
                }
                Ok::<_, String>(())
            };
            let message =
                match tokio::time::timeout(std::time::Duration::from_secs(5), result).await {
                    Ok(Ok(())) => "Copied to clipboard".into(),
                    Ok(Err(e)) => format!("Clipboard failed: {e}"),
                    Err(_) => "Clipboard utility timed out".into(),
                };
            let _ = tx.send(AppEvent::SystemNotification(message));
        });
    }

    pub fn trigger_generation(&mut self, tx: UnboundedSender<AppEvent>) {
        if self.state != EngineState::Idle {
            return;
        }

        if let Some(notice) = self.pending_todo_notice.take() {
            self.add_message("system", notice);
        }

        if self.config.model.trim().is_empty() {
            self.stream_epoch += 1;
            let _ = tx.send(AppEvent::Stream {
                epoch: self.stream_epoch,
                signal: StreamSignal::Error(
                    "No model selected for this provider. Use /model <id> or /models.".to_string(),
                ),
            });
            return;
        }
        let context_estimate = self.estimated_context_tokens();
        self.context_tokens = context_estimate;
        self.context_tokens_estimated = true;
        self.request_context_tokens = context_estimate;

        let output_would_overflow = self.context_would_overflow(context_estimate);
        if self.config.auto_compact
            && self.messages.len() > 2
            && (context_estimate >= self.config.auto_compact_threshold_tokens
                || output_would_overflow)
        {
            self.pending_generation_after_compaction = true;
            self.compact_history(tx);
            return;
        }

        self.stream_epoch += 1;
        let epoch = self.stream_epoch;

        if let Some(handle) = self.active_stream_task.take() {
            handle.abort();
        }

        self.state = EngineState::Streaming;
        self.tool_turn_stream_finished = false;
        self.tool_turn_failed = false;
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

        self.interaction.actual_model = None;
        self.interaction.actual_protocol = None;
        let tasks = self.tool_registry.tasks.clone();
        let (request_task, mut cancel) = tasks.start(&format!("generation {model}"), epoch);
        self.interaction.request_task = Some(request_task.clone());
        let error_tx = stream_tx.clone();
        let stream_handle = tokio::spawn(async move {
            tokio::select! {
                _=client.stream_generate_content(&model,&fallback_models,max_retries,&request,stream_tx)=>{},
                _=cancel.changed()=>{let _=error_tx.send(StreamSignal::Error("Generation cancelled".into()));},
            }
        });

        self.active_stream_task = Some(stream_handle);

        let app_tx = tx.clone();
        tokio::spawn(async move {
            while let Some(sig) = stream_rx.recv().await {
                match &sig {
                    StreamSignal::TextDelta(text) => tasks.log(&request_task, text),
                    StreamSignal::Finished { .. } => tasks.finish(&request_task, "completed"),
                    StreamSignal::Error(e) => {
                        tasks.log(&request_task, e);
                        tasks.finish(&request_task, "failed");
                    }
                    _ => {}
                }
                let _ = app_tx.send(AppEvent::Stream { epoch, signal: sig });
            }
        });
    }

    pub fn context_breakdown(&self) -> String {
        let request = self.build_request();
        let estimate = |n: usize| n.div_ceil(4);
        let instructions = request
            .system_instruction
            .as_ref()
            .and_then(|v| serde_json::to_vec(v).ok())
            .map(|v| estimate(v.len()))
            .unwrap_or(0);
        let schemas = request
            .tools
            .as_ref()
            .and_then(|v| serde_json::to_vec(v).ok())
            .map(|v| estimate(v.len()))
            .unwrap_or(0);
        let mut conversation = 0;
        let mut tools = 0;
        let mut attachments = 0;
        let mut images = 0;
        for message in &self.messages {
            if matches!(message.role.as_str(), "function" | "model_tool_call") {
                tools += estimate(message.content.len());
            } else if matches!(message.role.as_str(), "model" | "user" | "summary") {
                conversation += estimate(message.content.len());
            }
            for attachment in &message.attachments {
                if let Some(text) = &attachment.text {
                    attachments += estimate(text.len());
                }
                if attachment.kind == "image" {
                    images += 1;
                }
            }
        }
        let profile = self.config.active_model_profile();
        let source = if profile.context_window.is_some() || profile.input_token_limit.is_some() {
            "explicit model profile override"
        } else {
            self.available_models
                .iter()
                .find(|m| same_model_id(&m.id, &self.config.model))
                .map(|m| m.metadata_source.as_str())
                .unwrap_or("unknown")
        };
        let source = format!(
            "{}; actual route: {} / {}",
            source,
            self.interaction
                .actual_model
                .as_deref()
                .unwrap_or("not requested yet"),
            self.interaction
                .actual_protocol
                .as_deref()
                .unwrap_or("not requested yet")
        );
        format!("Context inspector (approximate, not tokenizer counts):\nInstructions ~{instructions}\nConversation ~{conversation}\nTool history ~{tools}\nTool schemas ~{schemas}\nText attachments ~{attachments}; images {images} (not text-tokenized)\nOutput reserve {}\nLimit source: {source}\nProvider: {} | requested model: {} | protocol: {}\nReasoning: {}",self.output_budget(),self.config.provider,self.config.model,self.config.active_provider_config().protocol,self.thinking_mode(&self.config.model))
    }

    fn estimated_context_tokens(&self) -> u64 {
        let request = self.build_request();
        let serialized_bytes = serde_json::to_vec(&request)
            .map(|bytes| bytes.len())
            .unwrap_or_default();
        ((serialized_bytes as u64) / 4).max(1)
    }

    pub fn cancel_generation(&mut self) {
        if let Some(id) = self.interaction.request_task.take() {
            self.tool_registry.tasks.finish(&id, "cancelled");
        }
        self.tool_registry.tasks.cancel_parent(self.stream_epoch);
        self.stream_epoch += 1;
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
        self.pending_generation_after_compaction = false;
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
            StreamSignal::ToolReviewed {
                mut pending,
                result,
            } => {
                if self.tool_turn_failed || pending.epoch != epoch {
                    return;
                }
                let explanation = match &result {
                    Ok(d) => d.explanation(),
                    Err(e) => format!("Automatic review unavailable: {e}. Confirm this exact action in writing or reject it."),
                };
                self.add_message(
                    "system",
                    format!("Auto-review {}: {}", pending.tool_name, explanation),
                );
                let policy = self.effective_permission_mode(
                    self.tool_registry.permission_group(&pending.tool_name),
                );
                if policy == PermissionMode::Deny || (self.plan_mode && pending.preview.is_mutation)
                {
                    self.reject_tool_call(
                        epoch,
                        pending.tool_name,
                        pending.call_id,
                        "Current policy blocks this action".into(),
                        tx,
                    );
                } else if policy == PermissionMode::Allow
                    || (policy == PermissionMode::Review
                        && result.as_ref().is_ok_and(|d| d.may_approve()))
                {
                    self.execute_tool(epoch, pending.tool_name, pending.call_id, pending.args, tx);
                } else if result.as_ref().is_ok_and(|d| d.is_denied()) {
                    self.reject_tool_call(
                        epoch,
                        pending.tool_name,
                        pending.call_id,
                        explanation,
                        tx,
                    );
                } else {
                    pending.preview.reason = Some(explanation);
                    if self.pending_tool_call.is_none() {
                        self.pending_tool_call = Some(pending);
                        self.state = EngineState::AwaitingHitlApproval;
                        self.modal_scroll = 0;
                        self.set_status("Auto-review requires your confirmation");
                    } else {
                        self.queued_tool_calls.push_back(pending);
                    }
                }
            }
            StreamSignal::ModelSelected { model, protocol } => {
                self.interaction.actual_model = Some(model);
                self.interaction.actual_protocol = Some(protocol);
            }
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
                // The native provider may omit a tool-call id, but the OpenAI Responses and
                // Chat Completions protocols require one. Normalize at the
                // boundary so every provider gets a stable correlation key.
                let id = Some(id.unwrap_or_else(|| {
                    format!("holiday-call-{}-{}", self.stream_epoch, self.messages.len())
                }));

                // Count every emitted call, including calls waiting for HITL
                // approval. A rejected call still produces a terminal output.
                self.pending_tool_executions = self.pending_tool_executions.saturating_add(1);

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

                if self.is_session_capability(&name) {
                    self.state = EngineState::ExecutingTool;
                    self.set_status(format!("Waiting for capability '{}'...", name));
                    let _ = tx.send(AppEvent::CapabilityRequest {
                        epoch: self.stream_epoch,
                        capability_name: name,
                        call_id: id,
                        args,
                    });
                    return;
                }

                if let Some(tool) = self.tool_registry.get(&name) {
                    let mut preview = tool.generate_preview(&args);
                    if preview.reason.is_none() {
                        // Use public narration only, never private reasoning or invented intent.
                        preview.reason = self
                            .messages
                            .iter()
                            .rev()
                            .take_while(|m| m.role != "user")
                            .find(|m| m.role == "model" && !m.content.trim().is_empty())
                            .map(|m| m.content.clone());
                    }
                    let permission_group = self.tool_registry.permission_group(&name);
                    let permission_mode = self.effective_permission_mode(permission_group);

                    if self.plan_mode && preview.is_mutation {
                        self.add_message(
                            "system",
                            format!("Plan mode blocked mutation tool '{}'. Use /plan off when ready to execute.", name),
                        );
                        self.state = EngineState::ExecutingTool;
                        let _ = tx.send(AppEvent::ToolExecutionResult {
                            epoch: self.stream_epoch,
                            tool_name: name,
                            call_id: id,
                            result: Err("Mutation blocked while plan mode is enabled.".to_string()),
                        });
                    } else if permission_mode == PermissionMode::Deny {
                        self.reject_tool_call(
                            self.stream_epoch,
                            name,
                            id,
                            format!(
                                "Tool denied by the '{}' permission policy.",
                                permission_group
                            ),
                            tx,
                        );
                    } else if permission_mode == PermissionMode::Allow
                        || (permission_mode != PermissionMode::Review
                            && self.session_allowed_tools.contains(&name))
                    {
                        self.execute_tool(self.stream_epoch, name, id, args, tx);
                    } else if permission_mode == PermissionMode::Review {
                        let pending = PendingToolCall {
                            epoch: self.stream_epoch,
                            call_id: id,
                            tool_name: name,
                            args,
                            preview,
                        };
                        self.start_auto_review(pending, tx);
                    } else {
                        let pending = PendingToolCall {
                            epoch: self.stream_epoch,
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
                    let _ = tx.send(AppEvent::ToolExecutionResult {
                        epoch: self.stream_epoch,
                        tool_name: name,
                        call_id: id,
                        result: Err("Tool is no longer available.".to_string()),
                    });
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
                self.tool_turn_stream_finished = true;
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
                        self.interaction
                            .set_overlay(crate::interaction::Overlay::Plan, true);
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
                self.tool_turn_stream_finished = true;
                self.tool_turn_failed = true;
                self.record_request_usage("error");

                if !self.current_thought_buffer.is_empty() {
                    let thought = std::mem::take(&mut self.current_thought_buffer);
                    self.add_message("thought", thought);
                }
                if !self.current_response_buffer.is_empty() {
                    let response = std::mem::take(&mut self.current_response_buffer);
                    self.add_message("model", response);
                }
                self.add_message("system", format!("Error: {}", err));
                let _ = self.flush_session();
                // A failed stream cannot resume a tool approval that was
                // emitted by that stream. Drop it so the UI cannot leave a
                // ghost HITL modal over the normal input box.
                self.pending_tool_call = None;
                self.queued_tool_calls.clear();
                self.pending_tool_executions = 0;
                self.state = EngineState::Idle;
                self.set_status("Stream ended with error.");
            }
        }
    }

    fn start_auto_review(&mut self, pending: PendingToolCall, tx: UnboundedSender<AppEvent>) {
        let client = self.client.clone();
        let context = crate::auto_review::review_context(&self.messages);
        let root = self.tool_registry.working_dir();
        self.state = if self.pending_tool_call.is_some() {
            EngineState::AwaitingHitlApproval
        } else {
            EngineState::ExecutingTool
        };
        self.set_status("Reviewing exact action with codex-auto-review (30s deadline)...");
        tokio::spawn(async move {
            let result = match context {
                Ok(context) => crate::auto_review::review(&client, &context, &root, &pending).await,
                Err(error) => Err(error),
            };
            let _ = tx.send(AppEvent::Stream {
                epoch: pending.epoch,
                signal: StreamSignal::ToolReviewed { pending, result },
            });
        });
    }

    pub fn effective_permission_mode(&self, group: &str) -> PermissionMode {
        let saved = self.config.permission_mode_for(group);
        if self.auto_mode && saved == PermissionMode::Ask {
            PermissionMode::Review
        } else {
            saved
        }
    }

    pub fn pending_requires_review(&self) -> bool {
        self.pending_tool_call.as_ref().is_some_and(|pending| {
            self.effective_permission_mode(self.tool_registry.permission_group(&pending.tool_name))
                == PermissionMode::Review
        })
    }

    pub fn approve_pending_tool(
        &mut self,
        whitelist_for_session: bool,
        tx: UnboundedSender<AppEvent>,
    ) {
        if let Some(pending) = self.pending_tool_call.take() {
            if whitelist_for_session
                && self.effective_permission_mode(
                    self.tool_registry.permission_group(&pending.tool_name),
                ) != PermissionMode::Review
            {
                self.session_allowed_tools.insert(pending.tool_name.clone());
            }
            self.execute_tool(
                pending.epoch,
                pending.tool_name,
                pending.call_id,
                pending.args,
                tx,
            );
        }
    }

    fn reject_tool_call(
        &mut self,
        epoch: u64,
        tool_name: String,
        call_id: Option<String>,
        reason: String,
        tx: UnboundedSender<AppEvent>,
    ) {
        self.add_message(
            "system",
            format!("Denied execution of tool '{}': {}", tool_name, reason),
        );
        let app_tx = tx.clone();
        tokio::spawn(async move {
            let _ = app_tx.send(AppEvent::ToolExecutionResult {
                epoch,
                tool_name,
                call_id,
                result: Err(reason),
            });
        });
        self.state = if self.pending_tool_call.is_some() {
            EngineState::AwaitingHitlApproval
        } else {
            EngineState::ExecutingTool
        };
    }

    pub fn deny_pending_tool(&mut self, tx: UnboundedSender<AppEvent>) {
        if let Some(pending) = self.pending_tool_call.take() {
            self.reject_tool_call(
                pending.epoch,
                pending.tool_name,
                pending.call_id,
                "Execution rejected by user.".to_string(),
                tx,
            );
        }
    }

    pub fn execute_tool(
        &mut self,
        epoch: u64,
        tool_name: String,
        call_id: Option<String>,
        args: serde_json::Value,
        tx: UnboundedSender<AppEvent>,
    ) {
        if tool_name == "spawn_agent" {
            self.tool_registry.install_agent_tools(
                self.client.clone(),
                self.config.model.clone(),
                self.config.effective_fallback_models(),
                self.config.max_retries,
            );
            let mut runtime = self
                .tool_registry
                .worker_runtime(&self.config, self.review.clone());
            runtime.tools.retain(|name, _| {
                !self.session_disabled_tools.contains(name)
                    && !(self.plan_mode && matches!(name.as_str(), "write_file" | "edit_file"))
            });
            self.tool_registry.agent_manager().configure(runtime);
        }
        if tool_name == "search_tools" {
            self.tool_registry.discover_from_query(&args);
        }
        self.state = if self.pending_tool_call.is_some() {
            EngineState::AwaitingHitlApproval
        } else {
            EngineState::ExecutingTool
        };
        self.set_status(format!("Executing tool '{}'...", tool_name));

        if let Some(tool) = self.tool_registry.get(&tool_name) {
            let app_tx = tx.clone();
            let t_name = tool_name.clone();
            let c_id = call_id.clone();

            let tasks = self.tool_registry.tasks.clone();
            let review = self.review.clone();
            let root = self.tool_registry.working_dir();
            let (task_id, mut cancel) = tasks.start(&t_name, epoch);
            tokio::spawn(async move {
                let pending = if matches!(t_name.as_str(), "write_file" | "edit_file") {
                    args.get("path")
                        .and_then(|v| v.as_str())
                        .map(|path| review.before(&root, path, epoch))
                } else {
                    None
                };
                let res = match pending {
                    Some(Err(error)) => {
                        Err(format!("Checkpoint failed; tool was not run: {error}"))
                    }
                    pending => {
                        let mut args = args;
                        if t_name == "run_command" {
                            args["_holiday_epoch"] = json!(epoch);
                        }
                        let result = if *cancel.borrow() {
                            Err("Task cancelled".to_string())
                        } else {
                            tokio::select! { biased; _=cancel.changed()=>Err("Task cancelled".to_string()), result=tool.execute(args)=>result }
                        };
                        if let Some(Ok(pending)) = pending {
                            if let Err(error) = review.after(pending) {
                                tasks.log(&task_id, &format!("Checkpoint warning: {error}"));
                            }
                        }
                        result
                    }
                };
                tasks.log(
                    &task_id,
                    res.as_ref()
                        .map(|s| s.as_str())
                        .unwrap_or_else(|s| s.as_str()),
                );
                tasks.finish(&task_id, if res.is_ok() { "completed" } else { "failed" });
                let _ = app_tx.send(AppEvent::ToolExecutionResult {
                    epoch,
                    tool_name: t_name,
                    call_id: c_id,
                    result: res,
                });
            });
        } else {
            // The call was counted when it was emitted. Return a terminal
            // result so the provider never sees an unpaired function call.
            let _ = tx.send(AppEvent::ToolExecutionResult {
                epoch,
                tool_name,
                call_id,
                result: Err("Tool is no longer available.".to_string()),
            });
        }
    }

    pub fn handle_tool_result(
        &mut self,
        epoch: u64,
        tool_name: String,
        call_id: Option<String>,
        result: Result<String, String>,
        tx: UnboundedSender<AppEvent>,
    ) {
        if epoch != self.stream_epoch {
            return;
        }

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
        if self.tool_turn_failed {
            return;
        }
        // The outstanding count includes calls waiting in the approval queue,
        // so advance that queue before testing whether the count reached zero.
        if self.pending_tool_call.is_none() {
            if let Some(next) = self.queued_tool_calls.pop_front() {
                self.pending_tool_call = Some(next);
                self.state = EngineState::AwaitingHitlApproval;
                self.set_status("HITL Gate: Approval needed for the next tool");
                self.modal_scroll = 0;
                return;
            }
        }

        if self.pending_tool_call.is_some() {
            self.state = EngineState::AwaitingHitlApproval;
            return;
        }

        if self.pending_tool_executions > 0 || !self.tool_turn_stream_finished {
            self.state = EngineState::ExecutingTool;
            return;
        }

        // Resume generation so model can synthesize an answer from the full
        // batch of tool results.
        self.state = EngineState::Idle;
        self.trigger_generation(tx);
    }

    pub fn handle_capability_result(
        &mut self,
        epoch: u64,
        capability_name: String,
        call_id: Option<String>,
        result: serde_json::Value,
        tx: UnboundedSender<AppEvent>,
    ) {
        if epoch != self.stream_epoch || !self.is_session_capability(&capability_name) {
            return;
        }
        let response_part = Part::FunctionResponse {
            function_response: FunctionResponsePayload {
                name: capability_name,
                response: result,
                id: call_id,
            },
        };
        self.messages.push(ChatMessage {
            role: "function".to_string(),
            content: serde_json::to_string(&response_part).unwrap_or_default(),
            timestamp: chrono::Local::now().format("%H:%M:%S").to_string(),
            attachments: Vec::new(),
        });
        let _ = self.flush_session();
        self.pending_tool_executions = self.pending_tool_executions.saturating_sub(1);
        if self.tool_turn_failed
            || self.pending_tool_executions > 0
            || !self.tool_turn_stream_finished
        {
            self.state = EngineState::ExecutingTool;
            return;
        }
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
        let recorded_tool_call_ids: HashSet<String> = self
            .messages
            .iter()
            .filter(|message| message.role == "model_tool_call")
            .filter_map(|message| serde_json::from_str::<Part>(&message.content).ok())
            .filter_map(|part| match part {
                Part::FunctionCall { function_call, .. } => function_call.id,
                _ => None,
            })
            .collect();
        // Both OpenAI-compatible and Codex providers use OpenAI's tool-call
        // correlation rules. Do not key this recovery behavior only on the
        // provider label: Codex is also a Responses provider.
        let provider = self.config.active_provider_config();
        let filter_orphaned_calls = provider.kind.eq_ignore_ascii_case("openai-compatible")
            || provider.kind.eq_ignore_ascii_case("codex")
            || provider.protocol.eq_ignore_ascii_case("responses");

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
                    if filter_orphaned_calls {
                        if let Part::FunctionResponse { function_response } = &part {
                            if function_response.id.as_ref().is_none_or(|id| {
                                !completed_tool_call_ids.contains(id)
                                    || !recorded_tool_call_ids.contains(id)
                            }) {
                                continue;
                            }
                        }
                    }
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

        let mut tools = self.tool_registry.to_gemini_declarations();
        for declaration in &mut tools {
            declaration
                .function_declarations
                .retain(|tool| !self.session_disabled_tools.contains(&tool.name));
        }
        tools.retain(|declaration| !declaration.function_declarations.is_empty());
        if !self.session_capabilities.is_empty() {
            if let Some(declaration) = tools.first_mut() {
                declaration
                    .function_declarations
                    .extend(self.session_capabilities.iter().cloned());
            } else {
                tools.push(crate::client::types::GeminiToolDeclaration {
                    function_declarations: self.session_capabilities.clone(),
                });
            }
        }

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
                max_output_tokens: Some(self.output_budget()),
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
        if self.state == EngineState::Compacting {
            return;
        }
        if self.state != EngineState::Idle {
            self.set_status("Stop the active turn before compacting.");
            self.pending_generation_after_compaction = false;
            return;
        }
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

        self.stream_epoch += 1;
        let epoch = self.stream_epoch;
        self.set_status("Compacting: one request, 60-second deadline (Esc cancels)...");
        self.state = EngineState::Compacting;
        self.add_message(
            "system",
            format!(
                "Compacting {} turns into a concise context summary...",
                count
            ),
        );

        let transcript = compaction_transcript(&self.messages);

        let prompt = format!("Summarize this conversation for continuation.\n\nBEGIN CONVERSATION TRANSCRIPT\n{}\nEND CONVERSATION TRANSCRIPT", transcript);

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
                    text: crate::prompts::COMPACTION.to_string(),
                    thought: None,
                }],
            }),
            generation_config: Some(GenerationConfig {
                temperature: Some(0.2),
                max_output_tokens: Some(2048),
                thinking_config: None,
                reasoning_effort: Some("low".into()),
                extra: None,
            }),
            safety_settings: None,
            tools: None,
        };

        let client = self.client.clone();
        let model = self.config.model.clone();
        self.active_stream_task = Some(tokio::spawn(async move {
            let res = client.generate_bounded(&model, &request, 60).await;
            let _ = tx.send(AppEvent::CompactionFinished(epoch, res));
        }));
    }

    pub fn handle_compaction_result(
        &mut self,
        epoch: u64,
        result: Result<String, String>,
        tx: UnboundedSender<AppEvent>,
    ) {
        if epoch != self.stream_epoch {
            return;
        }
        self.active_stream_task = None;
        let result = result.and_then(|summary| {
            if summary.trim().is_empty() {
                Err("The summarizer returned an empty handoff; history was retained.".to_string())
            } else if !summary.trim_end().ends_with("Checkpoint complete.") {
                Err("Incomplete checkpoint; history was retained".into())
            } else {
                Ok(summary)
            }
        });
        match result {
            Ok(summary) => {
                self.interaction.reset_transcript();
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
                    // One compaction per continuation, even with a very low threshold.
                    let auto_compact = self.config.auto_compact;
                    self.config.auto_compact = false;
                    self.trigger_generation(tx);
                    self.config.auto_compact = auto_compact;
                }
            }
            Err(e) => {
                self.set_status("Compaction failed");
                self.add_message("system", format!("Compaction failed: {}. History retained; no automatic retry. Use /compact to retry explicitly.", e));
                self.pending_generation_after_compaction = false;
                self.state = EngineState::Idle;
            }
        }
    }
}

async fn fetch_models(
    client: AiClient,
    configured_models: Vec<String>,
    bootstrap_model: String,
    free_only: bool,
) -> Result<Vec<crate::client::types::ModelInfo>, String> {
    match client.list_models().await {
        Ok(mut models) => {
            if free_only {
                models.retain(|model| is_free_model_id(&model.id));
            }
            for id in configured_models {
                if free_only
                    && (!is_free_model_id(&id) || crate::client::is_zen_unsupported_model_id(&id))
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
                        context_window: None,
                        output_token_limit: None,
                        reasoning_levels: Vec::new(),
                        metadata_source: "provider catalog".into(),
                    });
                }
            }
            Ok(models)
        }
        Err(_error) if free_only && configured_models.is_empty() => {
            Ok(vec![crate::client::types::ModelInfo {
                display_name: bootstrap_model.clone(),
                id: bootstrap_model,
                description: "Bootstrap free model (live catalog unavailable)".to_string(),
                input_price_per_m: None,
                output_price_per_m: None,
                input_token_limit: None,
                context_window: None,
                output_token_limit: None,
                reasoning_levels: Vec::new(),
                metadata_source: "provider catalog".into(),
            }])
        }
        Err(_error) if !configured_models.is_empty() => Ok(configured_models
            .into_iter()
            .filter(|id| {
                !free_only
                    || (is_free_model_id(id) && !crate::client::is_zen_unsupported_model_id(id))
            })
            .map(|id| crate::client::types::ModelInfo {
                display_name: id.clone(),
                id,
                description: "Configured provider model (API model listing unavailable)"
                    .to_string(),
                input_price_per_m: None,
                output_price_per_m: None,
                input_token_limit: None,
                context_window: None,
                output_token_limit: None,
                reasoning_levels: Vec::new(),
                metadata_source: "provider catalog".into(),
            })
            .collect()),
        Err(error) => Err(error),
    }
}

/// Preserve source roles and attached text, without copying image payloads or
/// private reasoning into the summarizer's input.
fn compaction_transcript(messages: &[ChatMessage]) -> String {
    let mut transcript = String::new();
    for message in messages {
        if message.role == "thought" || message.role == "tool" {
            continue;
        }
        if message.role == "system" && !message.content.starts_with("Compact History Summary:") {
            continue;
        }
        let attachments = message.attachments.iter().map(|attachment| {
            json!({
                "kind": attachment.kind,
                "name": attachment.name,
                "text": attachment.text,
                "note": if attachment.kind == "image" {
                    Some("Image payload omitted; preserve any established findings and note if reinspection is needed.")
                } else { None },
            })
        }).collect::<Vec<_>>();
        // JSON escaping prevents pasted text from masquerading as another
        // transcript record. The summarizer still treats all records as data.
        transcript.push_str(
            &json!({
                "role": message.role,
                "content": message.content,
                "attachments": attachments,
            })
            .to_string(),
        );
        transcript.push('\n');
    }
    transcript
}

fn is_free_model_id(id: &str) -> bool {
    id.trim().to_ascii_lowercase().ends_with("-free")
}

fn same_model_id(left: &str, right: &str) -> bool {
    let normalize = |id: &str| id.trim().trim_start_matches("models/").to_ascii_lowercase();
    normalize(left) == normalize(right)
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

    fn catalog_model(id: &str) -> crate::client::types::ModelInfo {
        crate::client::types::ModelInfo {
            id: id.into(),
            display_name: id.into(),
            description: String::new(),
            input_price_per_m: None,
            output_price_per_m: None,
            input_token_limit: None,
            context_window: Some(128_000),
            output_token_limit: Some(1024),
            reasoning_levels: vec!["minimal".into(), "high".into()],
            metadata_source: "test catalog".into(),
        }
    }

    fn review_pending(epoch: u64) -> super::PendingToolCall {
        super::PendingToolCall {
            epoch,
            call_id: Some("review-call".into()),
            tool_name: "run_command".into(),
            args: serde_json::json!({"command":"echo test"}),
            preview: crate::tools::ToolPreview {
                title: "test".into(),
                details: vec![],
                reason: None,
                expected_effect: None,
                command: None,
                diff_hunks: vec![],
                is_mutation: true,
            },
        }
    }

    #[tokio::test]
    async fn auto_mode_allowed_directory_listing_never_calls_reviewer() {
        let mut app = App::new(AppConfig::default(), "invalid-test-key".into());
        app.auto_mode = true;
        app.config
            .set_permission_mode("read_only", crate::config::PermissionMode::Allow);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let epoch = app.stream_epoch;
        app.handle_stream_signal(
            epoch,
            crate::events::StreamSignal::ToolCall {
                id: Some("list-allowed".into()),
                name: "list_directory".into(),
                args: serde_json::json!({"path":".", "max_entries":1}),
                thought_signature: None,
            },
            tx,
        );
        assert!(app.pending_tool_call.is_none());
        let event = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
            .await
            .unwrap()
            .unwrap();
        match event {
            crate::events::AppEvent::ToolExecutionResult {
                tool_name, result, ..
            } => {
                assert_eq!(tool_name, "list_directory");
                assert!(result.is_ok(), "{result:?}");
            }
            other => panic!("Allowed listing unexpectedly entered review: {other:?}"),
        }
    }

    #[test]
    fn global_auto_override_is_temporary_and_preserves_denials() {
        let mut app = App::new(AppConfig::default(), "test".into());
        app.config
            .set_permission_mode("host_execution", crate::config::PermissionMode::Ask);
        app.config
            .set_permission_mode("read_only", crate::config::PermissionMode::Allow);
        app.config
            .set_permission_mode("destructive", crate::config::PermissionMode::Deny);
        assert!(!app.auto_mode);
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        app.handle_slash_command("/auto on", tx.clone());
        assert!(app.auto_mode);
        assert_eq!(
            app.effective_permission_mode("host_execution"),
            crate::config::PermissionMode::Review
        );
        assert_eq!(
            app.effective_permission_mode("read_only"),
            crate::config::PermissionMode::Allow
        );
        assert_eq!(
            app.effective_permission_mode("destructive"),
            crate::config::PermissionMode::Deny
        );
        assert_eq!(
            app.config.permission_mode_for("host_execution"),
            crate::config::PermissionMode::Ask
        );
        app.handle_slash_command("/auto off", tx.clone());
        assert_eq!(
            app.effective_permission_mode("host_execution"),
            crate::config::PermissionMode::Ask
        );
        assert_eq!(
            app.effective_permission_mode("read_only"),
            crate::config::PermissionMode::Allow
        );
        app.handle_slash_command("/auto nonsense", tx);
        assert!(!app.auto_mode);
    }

    #[test]
    fn auto_toggle_requires_idle_and_new_session_clears_it() {
        let mut app = App::new(AppConfig::default(), "test".into());
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        app.state = super::EngineState::ExecutingTool;
        app.handle_slash_command("/auto on", tx.clone());
        assert!(!app.auto_mode);
        app.state = super::EngineState::Idle;
        app.handle_slash_command("/auto on", tx);
        assert!(app.auto_mode);
        app.start_new_session();
        assert!(!app.auto_mode);
    }

    #[test]
    fn review_error_asks_and_stale_reviews_are_ignored() {
        let mut app = App::new(AppConfig::default(), "test".into());
        app.config
            .set_permission_mode("host_execution", crate::config::PermissionMode::Review);
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let epoch = app.stream_epoch;
        app.handle_stream_signal(
            epoch + 1,
            crate::events::StreamSignal::ToolReviewed {
                pending: review_pending(epoch + 1),
                result: Err("timeout".into()),
            },
            tx.clone(),
        );
        assert!(app.pending_tool_call.is_none());
        app.handle_stream_signal(
            epoch,
            crate::events::StreamSignal::ToolReviewed {
                pending: review_pending(epoch),
                result: Err("timeout".into()),
            },
            tx.clone(),
        );
        assert!(app.pending_requires_review());
        assert_eq!(app.state, super::EngineState::AwaitingHitlApproval);
        assert!(app
            .pending_tool_call
            .as_ref()
            .unwrap()
            .preview
            .reason
            .as_ref()
            .unwrap()
            .contains("timeout"));
        app.cancel_generation();
        app.handle_stream_signal(
            epoch,
            crate::events::StreamSignal::ToolReviewed {
                pending: review_pending(epoch),
                result: Err("late".into()),
            },
            tx,
        );
        assert!(app.pending_tool_call.is_none());
    }

    #[test]
    fn failed_turn_cannot_accept_review_result() {
        let mut app = App::new(AppConfig::default(), "test".into());
        app.tool_turn_failed = true;
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let epoch = app.stream_epoch;
        app.handle_stream_signal(
            epoch,
            crate::events::StreamSignal::ToolReviewed {
                pending: review_pending(epoch),
                result: Err("late".into()),
            },
            tx,
        );
        assert!(app.pending_tool_call.is_none());
    }

    #[test]
    fn stale_and_incomplete_compaction_cannot_replace_history() {
        let mut app = App::new(AppConfig::default(), "test".into());
        app.add_message("user", "Keep this objective");
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        app.handle_compaction_result(
            app.stream_epoch + 1,
            Ok("stale Checkpoint complete.".into()),
            tx.clone(),
        );
        assert_eq!(app.messages.len(), 1);
        app.handle_compaction_result(app.stream_epoch, Ok("truncated checkpoint".into()), tx);
        assert_eq!(app.messages[0].content, "Keep this objective");
        assert!(!app.messages.iter().any(|m| m.role == "summary"));
    }

    #[tokio::test]
    async fn compaction_is_single_flight_and_cancellable() {
        let mut app = App::new(AppConfig::default(), "test".into());
        for _ in 0..3 {
            app.add_message("user", "Keep this objective");
        }
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        app.compact_history(tx.clone());
        let epoch = app.stream_epoch;
        app.compact_history(tx);
        assert_eq!(app.stream_epoch, epoch);
        assert!(app.active_stream_task.is_some());
        app.cancel_generation();
        assert!(app.active_stream_task.is_none());
        assert_eq!(app.state, super::EngineState::Idle);
        assert!(app.stream_epoch > epoch);
        assert!(!app.pending_generation_after_compaction);
    }

    #[test]
    fn successful_checkpoint_replaces_history() {
        let mut app = App::new(AppConfig::default(), "test".into());
        app.add_message("user", "Keep this objective");
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        app.handle_compaction_result(
            app.stream_epoch,
            Ok("Objective retained. Checkpoint complete.".into()),
            tx,
        );
        assert_eq!(app.messages[0].role, "summary");
        assert!(app.messages[0].content.contains("Objective retained"));
    }

    #[test]
    fn input_only_limits_do_not_reserve_output_tokens() {
        let mut app = App::new(AppConfig::default(), "test-key".into());
        let mut model = catalog_model(&app.config.model);
        model.context_window = None;
        model.input_token_limit = Some(1000);
        app.available_models = vec![model];
        assert!(!app.context_would_overflow(999));
        assert!(app.context_would_overflow(1000));
        app.available_models[0].context_window = Some(1500);
        assert!(app.context_would_overflow(999));
    }

    #[test]
    fn refresh_invalidates_catalog_and_rejects_stale_fetches() {
        let mut app = App::new(AppConfig::default(), "test-key".into());
        let epoch = app.models_epoch;
        assert!(app.install_models(epoch, vec![catalog_model(&app.config.model)]));
        app.refresh_client_from_config();
        assert!(app.available_models.is_empty());
        assert!(!app.install_models(epoch, vec![catalog_model("stale")]));
        assert!(app.available_models.is_empty());
        assert!(app.install_models(app.models_epoch, vec![catalog_model("current")]));
    }

    #[test]
    fn configured_context_override_works_without_catalog() {
        let mut app = App::new(AppConfig::default(), "test-key".into());
        let key = app.config.model_profile_key();
        app.config
            .model_profiles
            .entry(key)
            .or_default()
            .context_window = Some(64_000);
        assert_eq!(app.context_limit(), Some(64_000));
        app.available_models = vec![catalog_model(&app.config.model)];
        assert_eq!(app.context_limit(), Some(64_000));
    }

    #[test]
    fn output_budget_is_clamped_and_used_in_payload() {
        let mut app = App::new(AppConfig::default(), "test-key".into());
        app.available_models = vec![catalog_model(&app.config.model)];
        assert_eq!(app.output_budget(), 1024);
        assert_eq!(
            app.build_request()
                .generation_config
                .unwrap()
                .max_output_tokens,
            Some(1024)
        );
    }

    #[test]
    fn compatible_provider_uses_catalog_reasoning_capabilities() {
        let mut app = App::new(AppConfig::default(), "test-key".into());
        app.config.provider = "bearlab".into();
        app.available_models = vec![catalog_model(&app.config.model)];
        assert_eq!(
            app.thinking_choices(&app.config.model),
            vec!["minimal", "high"]
        );
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
    fn drops_orphaned_codex_tool_calls_and_outputs() {
        let config = AppConfig {
            provider: "codex".into(),
            model: "gpt-test".into(),
            ..Default::default()
        };
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
            message(
                "function",
                Part::FunctionResponse {
                    function_response: FunctionResponsePayload {
                        name: "read_file".to_string(),
                        response: json!({"output": "orphan output"}),
                        id: Some("call-unrelated".to_string()),
                    },
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
            content.parts.iter().all(|part| {
                !matches!(
                    part,
                    Part::FunctionCall { .. } | Part::FunctionResponse { .. }
                )
            })
        }));
    }

    #[test]
    fn request_uses_maintained_prompt_and_sys_only_changes_customization() {
        let mut app = App::new(AppConfig::default(), "test-key".to_string());
        app.project_instructions = "Project convention: use tabs.".to_string();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        app.handle_slash_command("/sys Respond in Polish.", tx.clone());
        app.plan_mode = true;
        let request = app.build_request();
        let system = request.system_instruction.unwrap();
        let Part::Text { text, .. } = &system.parts[0] else {
            panic!("missing system text")
        };
        assert!(text.starts_with(crate::prompts::MAIN.trim()));
        assert!(text.contains("Respond in Polish."));
        assert!(text.contains("Project convention: use tabs."));
        assert!(text.ends_with(crate::prompts::PLAN.trim()));
        app.handle_slash_command("/sys --clear", tx);
        assert!(app.config.system_instruction.is_empty());
        assert!(app
            .effective_system_instruction()
            .starts_with(crate::prompts::MAIN.trim()));
    }

    #[test]
    fn runtime_session_instruction_is_request_scoped() {
        let mut app = App::new(AppConfig::default(), "test-key".to_string());
        app.set_session_instruction(Some("Speak for this frontend session.".to_string()));
        assert!(app
            .effective_system_instruction()
            .contains("--- BEGIN Session instructions ---\nSpeak for this frontend session."));
        app.start_new_session();
        assert!(!app
            .effective_system_instruction()
            .contains("Speak for this frontend session."));
    }

    #[test]
    fn compaction_preserves_attachment_evidence_and_rejects_empty_summary() {
        let mut app = App::new(AppConfig::default(), "test-key".to_string());
        app.add_message_with_attachments(
            "user",
            "Review this",
            vec![
                Attachment {
                    kind: "text".into(),
                    name: "source.rs".into(),
                    mime_type: None,
                    text: Some("fn supplied_code() {}".into()),
                    data: None,
                },
                Attachment {
                    kind: "image".into(),
                    name: "screen.png".into(),
                    mime_type: Some("image/png".into()),
                    text: None,
                    data: Some("IMAGE_PAYLOAD_SHOULD_NOT_BE_COPIED".into()),
                },
            ],
        );
        app.add_message("thought", "PRIVATE_REASONING");
        let transcript = super::compaction_transcript(&app.messages);
        assert!(transcript.contains("fn supplied_code() {}"));
        assert!(transcript.contains("screen.png"));
        assert!(!transcript.contains("IMAGE_PAYLOAD_SHOULD_NOT_BE_COPIED"));
        assert!(!transcript.contains("PRIVATE_REASONING"));
        for line in transcript.lines() {
            assert!(serde_json::from_str::<serde_json::Value>(line).is_ok());
        }
        let original = app.messages[0].content.clone();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        app.handle_compaction_result(app.stream_epoch, Ok("  ".into()), tx);
        assert_eq!(app.messages[0].content, original);
        assert_eq!(app.messages[0].attachments.len(), 2);
        assert!(!app.messages.iter().any(|message| message.role == "summary"));
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

    #[test]
    fn retry_preserves_all_history_even_after_one_long_user_turn() {
        let mut app = App::new(AppConfig::default(), "".into());
        app.session_path = None;
        app.config.model.clear(); // No network or runtime needed for this regression.
        app.add_message("user", "implement this");
        app.add_message("model", "Checking files");
        app.add_message("tool", "completed edit");
        app.add_message("function", "saved result");
        app.add_message("system", "Error: connection lost");
        let before = serde_json::to_string(&app.messages).unwrap();
        let (tx, _) = tokio::sync::mpsc::unbounded_channel();
        app.handle_slash_command("/retry", tx);
        assert_eq!(serde_json::to_string(&app.messages).unwrap(), before);
        assert_eq!(app.chat_scroll, 0);
    }

    #[test]
    fn permission_preview_uses_public_narration_not_thoughts() {
        let mut app = App::new(AppConfig::default(), "".into());
        app.session_path = None;
        app.add_message("user", "check files");
        app.current_response_buffer = "I will list files to locate the configuration.".into();
        app.current_thought_buffer = "Private reasoning must not be shown as justification".into();
        let (tx, _) = tokio::sync::mpsc::unbounded_channel();
        app.handle_stream_signal(
            app.stream_epoch,
            crate::events::StreamSignal::ToolCall {
                id: Some("call-1".into()),
                name: "run_command".into(),
                args: json!({"command":"dir"}),
                thought_signature: None,
            },
            tx,
        );
        let pending = app.pending_tool_call.expect("approval requested");
        assert_eq!(
            pending.preview.reason.as_deref(),
            Some("I will list files to locate the configuration.")
        );
    }
}

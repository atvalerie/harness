use directories::ProjectDirs;
use keyring::Entry;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::PathBuf;

const KEYRING_SERVICE: &str = "holiday";
const CODEX_ACCESS_USER: &str = "codex_access_token";
const CODEX_REFRESH_USER: &str = "codex_refresh_token";
const CODEX_ACCOUNT_USER: &str = "codex_account_id";
const DEFAULT_GEMINI_MODEL: &str = "gemini-3.5-flash-lite";

pub const TOOL_PERMISSION_GROUPS: &[(&str, &str)] = &[
    ("read_only", "Read-only inspection"),
    ("workspace_write", "Workspace writes and moves"),
    ("destructive", "Permanent deletion"),
    ("host_execution", "Shell and host execution"),
    ("external_side_effect", "MCP and external services"),
    ("agent_control", "Subagent lifecycle and messaging"),
    ("session_state", "In-session state"),
];

pub fn tool_permission_description(group: &str) -> &'static str {
    match group {
        "read_only" => {
            "Inspects files, directories, metadata, repository matches, and web pages without changing the workspace."
        }
        "workspace_write" => {
            "Creates, edits, writes, moves, or renames workspace files and directories."
        }
        "destructive" => {
            "Permanently deletes files or directories. These operations have no built-in undo."
        }
        "host_execution" => {
            "Runs shell commands on the host. Commands may modify files, start processes, or affect systems outside the project."
        }
        "external_side_effect" => {
            "Calls MCP and other integration tools. Effects depend on the connected server or service."
        }
        "agent_control" => {
            "Starts, stops, inspects, and sends messages to subagents."
        }
        "session_state" => {
            "Updates Holiday session state such as todos; it does not directly modify host files."
        }
        _ => "Tools in this group require an explicit policy before execution.",
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionMode {
    Ask,
    Review,
    Allow,
    Deny,
}

impl PermissionMode {
    pub fn parse(value: &str) -> Self {
        match value {
            "review" => Self::Review,
            "allow" => Self::Allow,
            "deny" => Self::Deny,
            _ => Self::Ask,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ask => "ask",
            Self::Review => "review",
            Self::Allow => "allow",
            Self::Deny => "deny",
        }
    }

    pub fn cycle(self, reverse: bool) -> Self {
        match (self, reverse) {
            (Self::Ask, false) => Self::Allow,
            (Self::Allow, false) => Self::Review,
            (Self::Review, false) => Self::Deny,
            (Self::Deny, false) => Self::Ask,
            (Self::Ask, true) => Self::Deny,
            (Self::Deny, true) => Self::Review,
            (Self::Review, true) => Self::Allow,
            (Self::Allow, true) => Self::Ask,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct CodexAuth {
    pub access_token: String,
    pub refresh_token: String,
    pub account_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    #[serde(default = "default_provider_kind")]
    pub kind: String,
    /// Wire protocol: auto, chat-completions, or responses.
    #[serde(default = "default_provider_protocol")]
    pub protocol: String,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub models: Vec<String>,
    #[serde(default)]
    pub fallback_models: Vec<String>,
    #[serde(default)]
    pub api_key_env: Option<String>,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    /// Whether streaming requests should ask for a final usage event.
    /// Some OpenAI-compatible gateways reject this optional field.
    #[serde(default = "default_true")]
    pub stream_usage: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    #[serde(default = "default_stdio_transport")]
    pub transport: String,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_stdio_transport() -> String {
    "stdio".to_string()
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelProfile {
    /// Explicit mapping to a Codex catalog model for metadata fallback.
    pub codex_model: Option<String>,
    /// Explicit combined context window override for catalogs that omit limits.
    pub context_window: Option<u64>,
    /// Separate input-only limit; output tokens are not subtracted from this.
    pub input_token_limit: Option<u64>,
    pub temperature: Option<f32>,
    pub thinking_budget: Option<i32>,
    pub reasoning_enabled: Option<bool>,
    pub reasoning_effort: Option<String>,
    pub max_output_tokens: Option<u32>,
    #[serde(default)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    /// Explicit USD rates in microdollars per million tokens, keyed by provider:model.
    #[serde(default)]
    pub usage_pricing: std::collections::BTreeMap<String, crate::usage::Pricing>,
    #[serde(default = "default_provider")]
    pub provider: String,
    #[serde(default)]
    pub base_url: Option<String>,
    pub model: String,
    #[serde(default = "default_fallback_models")]
    pub fallback_models: Vec<String>,
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,
    pub thinking_budget: i32,
    pub temperature: f32,
    /// Optional user customization appended to maintained harness instructions.
    #[serde(default)]
    pub system_instruction: String,
    #[serde(default)]
    pub prompt_config_version: u32,
    #[serde(default = "default_provider_configs")]
    pub providers: BTreeMap<String, ProviderConfig>,
    #[serde(default)]
    pub model_profiles: BTreeMap<String, ModelProfile>,
    /// Last selected model for each provider, so switching providers and
    /// restarting Holiday returns to the user's actual choices.
    #[serde(default)]
    pub last_models: BTreeMap<String, String>,
    #[serde(default = "default_auto_compact")]
    pub auto_compact: bool,
    #[serde(default = "default_auto_compact_threshold")]
    pub auto_compact_threshold_tokens: u64,
    #[serde(default = "default_todo_change_mode")]
    pub todo_change_mode: String,
    #[serde(default = "default_session_name")]
    pub session_name: String,
    #[serde(default)]
    pub mcp_servers: BTreeMap<String, McpServerConfig>,
    #[serde(default = "default_tool_permissions")]
    pub tool_permissions: BTreeMap<String, String>,
    #[serde(default)]
    pub permissions_configured: bool,
    #[serde(default = "default_status_bar_position")]
    pub status_bar_position: String,
}

// Exact historical default: only known generated text is removed during migration.
const LEGACY_SYSTEM_INSTRUCTION: &str = "You are an expert autonomous developer and coding assistant running in the Gemini TUI Holiday.\n\n\
You have direct access to native tools for filesystem inspection, safe command execution, and live web grounding:\n\
- `web_search(query)`: Query the live internet using DuckDuckGo HTML Lite to look up current documentation, breaking news, libraries, or release notes.\n\
- `web_fetch(url)`: Fetch and extract clean article text and code from web pages.\n\
- `search_files(query, path, glob)`: Search the repository with ripgrep and return bounded file/line matches.\n\
- `read_file(path)`: Inspect an existing text file with bounded line ranges.\n\
- `list_directory(path, depth, max_entries)`: Inspect a bounded directory tree without shell commands.\n\
- `stat_path(path)`: Inspect file or directory metadata without shell commands.\n\
- `write_file(path, content)`: Propose file writes and edits. Holiday automatically generates unified diffs for the user to review in an interactive HITL modal.\n\
- `run_command(command)`: Run host shell commands. Always preview the exact command before requesting execution.\n\n\
Operational Guidelines:\n\
1. Proactively use `web_search` and `web_fetch` whenever you need up-to-date documentation or external facts.\n\
2. Prioritize reading relevant files before modifying them.\n\
3. Use `read_file` for source and text inspection, using its optional `start_line` and `max_lines` for bounded chunks. Do not emulate file reads with `run_command`, `type`, `Get-Content`, `findstr`, or line-count probes.\n\
4. Use `run_command` for actual execution such as builds, tests, git, and scripts. Its working directory persists after `cd` or `Set-Location`; use its optional `shell` field when a specific shell is required.\n\
5. Explain your thinking concisely. Be accurate, pragmatic, and write clean, production-ready code.";
const PREVIOUS_SYSTEM_INSTRUCTION: &str = "You are an expert autonomous developer and coding assistant running in the Gemini TUI Holiday.\n\n\
You have direct access to native tools for filesystem inspection, safe command execution, and live web grounding:\n\
- `web_search(query)`: Query the live internet using DuckDuckGo HTML Lite to look up current documentation, breaking news, libraries, or release notes.\n\
- `web_fetch(url)`: Fetch and extract clean article text and code from web pages.\n\
- `search_files(query, path, glob)`: Search the repository with ripgrep and return bounded file/line matches.\n\
- `read_file(path)`: Inspect existing source files and directory contents.\n\
- `write_file(path, content)`: Propose file writes and edits. Holiday automatically generates unified diffs for the user to review in an interactive HITL modal.\n\
- `run_command(command)`: Run host shell commands. Always preview the exact command before requesting execution.\n\n\
Operational Guidelines:\n\
1. Proactively use `web_search` and `web_fetch` whenever you need up-to-date documentation or external facts.\n\
2. Prioritize reading relevant files before modifying them.\n\
3. Use `read_file` for source and text inspection, using its optional `start_line` and `max_lines` for bounded chunks. Do not emulate file reads with `run_command`, `type`, `Get-Content`, `findstr`, or line-count probes.\n\
4. Use `run_command` for actual execution such as builds, tests, git, and scripts. Its working directory persists after `cd` or `Set-Location`; use its optional `shell` field when a specific shell is required.\n\
5. Explain your thinking concisely. Be accurate, pragmatic, and write clean, production-ready code.";
const LEGACY_DIRECTORY_GUIDANCE: &str = "\n- Use `list_directory(path, depth, max_entries)` for bounded directory exploration and `stat_path(path)` for metadata; paths are resolved from the project working directory.";
const PROMPT_CONFIG_VERSION: u32 = 1;

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            provider: default_provider(),
            usage_pricing: Default::default(),
            base_url: None,
            model: DEFAULT_GEMINI_MODEL.to_string(),
            fallback_models: default_fallback_models(),
            max_retries: default_max_retries(),
            thinking_budget: 1024,
            temperature: 0.7,
            system_instruction: String::new(),
            prompt_config_version: PROMPT_CONFIG_VERSION,
            providers: default_provider_configs(),
            model_profiles: BTreeMap::new(),
            last_models: BTreeMap::new(),
            auto_compact: default_auto_compact(),
            auto_compact_threshold_tokens: default_auto_compact_threshold(),
            todo_change_mode: default_todo_change_mode(),
            session_name: "default".to_string(),
            mcp_servers: BTreeMap::new(),
            tool_permissions: default_tool_permissions(),
            permissions_configured: false,
            status_bar_position: default_status_bar_position(),
        }
    }
}

fn default_provider() -> String {
    "gemini".to_string()
}
fn default_provider_kind() -> String {
    "openai-compatible".to_string()
}
fn default_provider_protocol() -> String {
    "auto".to_string()
}

fn default_fallback_models() -> Vec<String> {
    vec![
        "gemini-3.1-flash-lite".to_string(),
        "gemini-3.6-flash".to_string(),
        "gemini-3.5-flash".to_string(),
    ]
}

fn default_max_retries() -> u32 {
    3
}
fn default_auto_compact() -> bool {
    true
}
fn default_auto_compact_threshold() -> u64 {
    100_000
}
fn default_todo_change_mode() -> String {
    "next_turn".to_string()
}
fn default_true() -> bool {
    true
}
fn default_session_name() -> String {
    "default".to_string()
}

fn default_status_bar_position() -> String {
    "top".to_string()
}

fn default_tool_permissions() -> BTreeMap<String, String> {
    TOOL_PERMISSION_GROUPS
        .iter()
        .map(|(key, _)| {
            (
                (*key).to_string(),
                if *key == "read_only" || *key == "session_state" {
                    "allow".to_string()
                } else {
                    "ask".to_string()
                },
            )
        })
        .collect()
}

fn default_provider_configs() -> BTreeMap<String, ProviderConfig> {
    let mut providers = BTreeMap::new();
    providers.insert(
        "gemini".to_string(),
        ProviderConfig {
            kind: "gemini".to_string(),
            protocol: "auto".to_string(),
            base_url: None,
            model: Some(DEFAULT_GEMINI_MODEL.to_string()),
            models: Vec::new(),
            fallback_models: default_fallback_models(),
            api_key_env: Some("GEMINI_API_KEY".to_string()),
            headers: BTreeMap::new(),
            stream_usage: true,
        },
    );
    providers.insert(
        "openai".to_string(),
        ProviderConfig {
            kind: "openai-compatible".to_string(),
            protocol: "auto".to_string(),
            base_url: None,
            model: None,
            models: Vec::new(),
            fallback_models: Vec::new(),
            api_key_env: Some("OPENAI_API_KEY".to_string()),
            headers: BTreeMap::new(),
            stream_usage: true,
        },
    );
    providers.insert(
        "codex".to_string(),
        ProviderConfig {
            kind: "codex".to_string(),
            protocol: "responses".to_string(),
            base_url: Some("https://chatgpt.com/backend-api/codex".to_string()),
            model: Some("gpt-5.6-sol".to_string()),
            models: Vec::new(),
            fallback_models: Vec::new(),
            api_key_env: Some("OPENAI_API_KEY".to_string()),
            headers: BTreeMap::new(),
            stream_usage: false,
        },
    );
    providers.insert(
        "opencode-zen".to_string(),
        ProviderConfig {
            kind: "openai-compatible".to_string(),
            protocol: "auto".to_string(),
            base_url: Some("https://opencode.ai/zen/v1".to_string()),
            model: Some("ling-3.0-flash-fin-free".to_string()),
            // The live /models catalog is the source of truth. This single model
            // is only a bootstrap choice for the first request before discovery.
            models: Vec::new(),
            fallback_models: Vec::new(),
            api_key_env: Some("OPENCODE_API_KEY".to_string()),
            headers: BTreeMap::new(),
            // Zen documents a few models that emit non-standard SSE when this is
            // requested, so keep the provider-compatible default disabled.
            stream_usage: false,
        },
    );
    providers
}

impl AppConfig {
    pub fn status_bar_at_bottom(&self) -> bool {
        self.status_bar_position.eq_ignore_ascii_case("bottom")
    }

    pub fn permission_mode_for(&self, group: &str) -> PermissionMode {
        self.tool_permissions
            .get(group)
            .map(|mode| PermissionMode::parse(mode))
            .unwrap_or(PermissionMode::Ask)
    }

    pub fn set_permission_mode(&mut self, group: &str, mode: PermissionMode) {
        self.tool_permissions
            .insert(group.to_string(), mode.as_str().to_string());
    }

    pub fn active_provider_config(&self) -> ProviderConfig {
        self.providers
            .get(&self.provider)
            .cloned()
            .unwrap_or_else(|| ProviderConfig {
                kind: default_provider_kind(),
                protocol: default_provider_protocol(),
                base_url: self.base_url.clone(),
                model: None,
                models: Vec::new(),
                fallback_models: self.fallback_models.clone(),
                api_key_env: None,
                headers: BTreeMap::new(),
                stream_usage: true,
            })
    }

    pub fn select_provider(&mut self, name: &str) {
        self.last_models
            .insert(self.provider.clone(), self.model.clone());
        self.provider = name.trim().to_string();
        if let Some(model) = self.last_models.get(&self.provider).cloned().or_else(|| {
            self.providers
                .get(&self.provider)
                .and_then(|provider| provider.model.clone())
        }) {
            self.model = model;
        } else {
            self.model.clear();
        }
    }

    pub fn remember_model(&mut self) {
        self.last_models
            .insert(self.provider.clone(), self.model.clone());
    }

    pub fn effective_fallback_models(&self) -> Vec<String> {
        self.providers
            .get(&self.provider)
            .map(|profile| profile.fallback_models.clone())
            .unwrap_or_else(|| self.fallback_models.clone())
    }

    pub fn configured_models(&self) -> Vec<String> {
        self.active_provider_config().models
    }

    pub fn model_profile_key(&self) -> String {
        format!("{}:{}", self.provider, self.model)
    }

    pub fn active_model_profile(&self) -> ModelProfile {
        self.model_profiles
            .get(&self.model_profile_key())
            .cloned()
            .or_else(|| self.model_profiles.get(&self.model).cloned())
            .unwrap_or_default()
    }

    pub fn config_dir() -> Option<PathBuf> {
        ProjectDirs::from("", "", "holiday").map(|dirs| dirs.config_dir().to_path_buf())
    }

    pub fn config_path() -> Option<PathBuf> {
        if let Ok(path) = env::var("HOLIDAY_CONFIG") {
            let path = PathBuf::from(path);
            if !path.as_os_str().is_empty() {
                return Some(path);
            }
        }
        Self::config_dir().map(|dir| dir.join("config.json"))
    }

    pub fn load() -> Self {
        if let Some(path) = Self::config_path() {
            if path.exists() {
                if let Ok(content) = fs::read_to_string(&path) {
                    if let Ok(cfg) = serde_json::from_str::<AppConfig>(&content) {
                        let cfg = Self::migrate_legacy_gemini_defaults(cfg);
                        let cfg = Self::with_builtin_providers(cfg);
                        return Self::migrate_prompt_config(cfg);
                    }
                }
            }
        }
        Self::default()
    }

    fn migrate_prompt_config(mut config: Self) -> Self {
        if config.prompt_config_version < PROMPT_CONFIG_VERSION {
            // Preserve customized legacy text verbatim rather than guessing which
            // paragraphs belong to the user. Never strip based on a substring.
            let previous_with_directory_guidance = format!(
                "{}{}",
                PREVIOUS_SYSTEM_INSTRUCTION, LEGACY_DIRECTORY_GUIDANCE
            );
            if config.system_instruction == LEGACY_SYSTEM_INSTRUCTION
                || config.system_instruction == PREVIOUS_SYSTEM_INSTRUCTION
                || config.system_instruction == previous_with_directory_guidance
            {
                config.system_instruction.clear();
            }
            config.prompt_config_version = PROMPT_CONFIG_VERSION;
        }
        config
    }

    fn with_builtin_providers(mut config: Self) -> Self {
        for (name, provider) in default_provider_configs() {
            config.providers.entry(name).or_insert(provider);
        }
        config
    }

    fn migrate_legacy_gemini_defaults(mut config: Self) -> Self {
        if !config.provider.eq_ignore_ascii_case("gemini") {
            return config;
        }

        // Older releases shipped these as their built-in/default choices.
        // Keep explicitly selected models intact, but repair configurations
        // that still contain the old generated fallback chain.
        let old_fallbacks = vec![
            "gemini-2.5-flash-lite".to_string(),
            "gemini-2.5-flash".to_string(),
        ];
        let previous_free_fallbacks = vec![
            "gemini-3.5-flash".to_string(),
            "gemini-3.1-flash-lite".to_string(),
            "gemini-2.5-flash-lite".to_string(),
        ];
        if config.fallback_models == old_fallbacks
            || config.fallback_models == previous_free_fallbacks
        {
            config.fallback_models = default_fallback_models();
        }

        if let Some(provider) = config.providers.get_mut("gemini") {
            if provider.fallback_models == old_fallbacks
                || provider.fallback_models == previous_free_fallbacks
            {
                provider.fallback_models = default_fallback_models();
            }
        }

        // The old generated configs used one of these as the primary model.
        // Move those configs to the current free-tier-safe default.
        if config.model == "gemini-3.8-flash" {
            config.model = DEFAULT_GEMINI_MODEL.to_string();
        }

        config
    }

    pub fn save(&self) -> Result<(), String> {
        let path =
            Self::config_path().ok_or_else(|| "Failed to resolve config directory".to_string())?;

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create config dir: {}", e))?;
        }

        let json = serde_json::to_string_pretty(self)
            .map_err(|e| format!("Serialization error: {}", e))?;

        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("config.json");
        let temp = path.with_file_name(format!(".{}.{}.tmp", file_name, std::process::id()));
        fs::write(&temp, json).map_err(|e| format!("Failed to write config file: {}", e))?;
        match fs::rename(&temp, &path) {
            Ok(()) => Ok(()),
            Err(_error) if cfg!(windows) && path.exists() => {
                fs::remove_file(&path)
                    .map_err(|e| format!("Failed to replace config file: {}", e))?;
                fs::rename(&temp, &path).map_err(|e| format!("Failed to commit config file: {}", e))
            }
            Err(error) => Err(format!("Failed to commit config file: {}", error)),
        }
    }

    pub fn get_api_key_for(provider: &str) -> Option<String> {
        // 1. Check environment variable first
        let variables: Vec<&str> = if provider.eq_ignore_ascii_case("codex")
            || provider.eq_ignore_ascii_case("openai")
            || provider.eq_ignore_ascii_case("openai-compatible")
        {
            vec!["OPENAI_API_KEY"]
        } else if provider.eq_ignore_ascii_case("gemini") {
            vec!["GEMINI_API_KEY"]
        } else {
            Vec::new()
        };
        for variable in variables {
            if let Ok(key) = std::env::var(variable) {
                let trimmed = key.trim();
                if !trimmed.is_empty() {
                    return Some(trimmed.to_string());
                }
            }
        }

        // Shared legacy keys have no provider identity: never guess their owner.
        Entry::new(KEYRING_SERVICE, &format!("api_key:{}", provider.trim()))
            .ok()
            .and_then(|entry| entry.get_password().ok())
            .map(|key| key.trim().to_string())
            .filter(|key| !key.is_empty())
    }

    pub fn get_api_key_for_active_provider(&self) -> Option<String> {
        if self.provider.eq_ignore_ascii_case("codex") {
            if let Some(auth) = Self::get_codex_auth() {
                if !auth.access_token.is_empty() {
                    return Some(auth.access_token);
                }
            }
        }
        if let Some(variable) = self.active_provider_config().api_key_env {
            if let Ok(key) = std::env::var(variable) {
                let trimmed = key.trim();
                if !trimmed.is_empty() {
                    return Some(trimmed.to_string());
                }
            }
        }
        Self::get_api_key_for(&self.provider)
    }

    fn codex_secret_path(user: &str) -> Option<PathBuf> {
        Self::config_dir().map(|dir| dir.join(format!(".{user}")))
    }

    fn get_secret(user: &str) -> Option<String> {
        if let Ok(entry) = Entry::new(KEYRING_SERVICE, user) {
            if let Ok(secret) = entry.get_password() {
                let secret = secret.trim().to_string();
                if !secret.is_empty() {
                    return Some(secret);
                }
            }
        }
        Self::codex_secret_path(user).and_then(|path| {
            fs::read_to_string(path)
                .ok()
                .map(|secret| secret.trim().to_string())
                .filter(|secret| !secret.is_empty())
        })
    }

    fn set_secret(user: &str, secret: &str) -> Result<(), String> {
        let entry = Entry::new(KEYRING_SERVICE, user)
            .map_err(|error| format!("Could not open secure credential store: {error}"))?;
        entry.set_password(secret)
            .map_err(|error| format!("Could not save credential securely: {error}. Configure an environment variable instead."))
    }

    pub fn get_codex_auth() -> Option<CodexAuth> {
        let access_token = Self::get_secret(CODEX_ACCESS_USER)?;
        Some(CodexAuth {
            access_token,
            refresh_token: Self::get_secret(CODEX_REFRESH_USER).unwrap_or_default(),
            account_id: Self::get_secret(CODEX_ACCOUNT_USER).unwrap_or_default(),
        })
    }

    pub fn set_codex_auth(auth: &CodexAuth) -> Result<(), String> {
        if auth.access_token.trim().is_empty() {
            return Err("Codex access token cannot be empty".to_string());
        }
        Self::set_secret(CODEX_ACCESS_USER, auth.access_token.trim())?;
        if !auth.refresh_token.trim().is_empty() {
            Self::set_secret(CODEX_REFRESH_USER, auth.refresh_token.trim())?;
        }
        if !auth.account_id.trim().is_empty() {
            Self::set_secret(CODEX_ACCOUNT_USER, auth.account_id.trim())?;
        }
        Ok(())
    }

    pub fn set_provider_api_key(&self, key: &str) -> Result<(), String> {
        let trimmed = key.trim();
        if trimmed.is_empty() {
            return Err("API key cannot be empty".to_string());
        }
        Self::set_secret(&format!("api_key:{}", self.provider.trim()), trimmed)
    }
}

#[cfg(test)]
mod tests {
    use super::{default_fallback_models, AppConfig, PermissionMode, ProviderConfig};
    use super::{LEGACY_SYSTEM_INSTRUCTION, PROMPT_CONFIG_VERSION};
    use std::collections::BTreeMap;

    #[test]
    fn provider_without_default_does_not_inherit_previous_model() {
        let mut config = AppConfig::default();
        config.select_provider("openai");
        assert!(config.model.is_empty());
        config.select_provider("gemini");
        assert_eq!(config.model, super::DEFAULT_GEMINI_MODEL);
    }

    #[test]
    fn empty_provider_fallbacks_disable_global_chain() {
        let mut config = AppConfig {
            fallback_models: vec!["wrong-provider-model".into()],
            ..Default::default()
        };
        config.select_provider("codex");
        assert!(config.effective_fallback_models().is_empty());
    }

    #[test]
    fn prompt_migration_preserves_customization_and_is_idempotent() {
        let mut legacy = AppConfig {
            prompt_config_version: 0,
            system_instruction: LEGACY_SYSTEM_INSTRUCTION.into(),
            ..Default::default()
        };
        let migrated = AppConfig::migrate_prompt_config(legacy.clone());
        assert!(migrated.system_instruction.is_empty());
        assert_eq!(migrated.prompt_config_version, PROMPT_CONFIG_VERSION);
        let again = AppConfig::migrate_prompt_config(migrated);
        assert!(again.system_instruction.is_empty());

        for known in [
            super::PREVIOUS_SYSTEM_INSTRUCTION.to_string(),
            format!(
                "{}{}",
                super::PREVIOUS_SYSTEM_INSTRUCTION,
                super::LEGACY_DIRECTORY_GUIDANCE
            ),
        ] {
            let mut previous = legacy.clone();
            previous.system_instruction = known;
            assert!(AppConfig::migrate_prompt_config(previous)
                .system_instruction
                .is_empty());
        }

        legacy.system_instruction.push_str("\nCustom instruction.");
        let custom = AppConfig::migrate_prompt_config(legacy.clone());
        assert_eq!(custom.system_instruction, legacy.system_instruction);
        let serialized = serde_json::to_string(&custom).unwrap();
        let restored: AppConfig = serde_json::from_str(&serialized).unwrap();
        assert_eq!(
            AppConfig::migrate_prompt_config(restored).system_instruction,
            legacy.system_instruction
        );

        let mut old_json = serde_json::to_value(AppConfig::default()).unwrap();
        old_json
            .as_object_mut()
            .unwrap()
            .remove("prompt_config_version");
        old_json["system_instruction"] = serde_json::json!(LEGACY_SYSTEM_INSTRUCTION);
        let old_config: AppConfig = serde_json::from_value(old_json).unwrap();
        assert!(AppConfig::migrate_prompt_config(old_config)
            .system_instruction
            .is_empty());
    }

    #[test]
    fn defaults_target_free_tier_models() {
        let config = AppConfig::default();
        assert_eq!(config.model, "gemini-3.5-flash-lite");
        assert_eq!(config.fallback_models, default_fallback_models());
        assert_eq!(config.status_bar_position, "top");
        assert!(!config.status_bar_at_bottom());

        let mut bottom = config;
        bottom.status_bar_position = "bottom".to_string();
        assert!(bottom.status_bar_at_bottom());
    }

    #[test]
    fn zen_uses_live_model_catalog() {
        let config = AppConfig::default();
        let zen = &config.providers["opencode-zen"];
        assert_eq!(zen.base_url.as_deref(), Some("https://opencode.ai/zen/v1"));
        assert!(zen.models.is_empty());
        assert!(zen.fallback_models.is_empty());
        assert!(!zen.stream_usage);
    }

    #[test]
    fn migrates_old_generated_gemini_defaults() {
        let mut config = AppConfig {
            model: "gemini-3.8-flash".into(),
            fallback_models: vec!["gemini-2.5-flash-lite".into(), "gemini-2.5-flash".into()],
            ..Default::default()
        };
        config.providers.get_mut("gemini").unwrap().fallback_models =
            config.fallback_models.clone();

        let migrated = AppConfig::migrate_legacy_gemini_defaults(config);
        assert_eq!(migrated.model, "gemini-3.5-flash-lite");
        assert_eq!(migrated.fallback_models, default_fallback_models());
        assert_eq!(
            migrated.providers["gemini"].fallback_models,
            default_fallback_models()
        );
    }

    #[test]
    fn custom_provider_selects_its_model() {
        let mut config = AppConfig::default();
        config.providers.insert(
            "local".to_string(),
            ProviderConfig {
                kind: "openai-compatible".to_string(),
                protocol: "auto".to_string(),
                base_url: Some("http://localhost:11434/v1".to_string()),
                model: Some("qwen3:8b".to_string()),
                models: vec!["qwen3:8b".to_string()],
                fallback_models: vec!["llama3.2:3b".to_string()],
                api_key_env: Some("OLLAMA_API_KEY".to_string()),
                headers: BTreeMap::new(),
                stream_usage: true,
            },
        );

        config.select_provider("local");
        assert_eq!(config.provider, "local");
        assert_eq!(config.model, "qwen3:8b");
        assert_eq!(
            config.active_provider_config().base_url.as_deref(),
            Some("http://localhost:11434/v1")
        );
        assert_eq!(config.effective_fallback_models(), vec!["llama3.2:3b"]);
    }

    #[test]
    fn permission_defaults_are_conservative() {
        let config = AppConfig::default();
        assert!(!config.permissions_configured);
        assert_eq!(
            config.permission_mode_for("read_only"),
            PermissionMode::Allow
        );
        assert_eq!(
            config.permission_mode_for("host_execution"),
            PermissionMode::Ask
        );
        assert_eq!(
            config.permission_mode_for("destructive"),
            PermissionMode::Ask
        );
        assert_eq!(config.permission_mode_for("unknown"), PermissionMode::Ask);
    }

    #[test]
    fn permission_modes_round_trip() {
        let mut config = AppConfig::default();
        config.set_permission_mode("host_execution", PermissionMode::Deny);
        assert_eq!(
            config.permission_mode_for("host_execution"),
            PermissionMode::Deny
        );
        assert_eq!(PermissionMode::Ask.cycle(false), PermissionMode::Allow);
        assert_eq!(PermissionMode::Allow.cycle(false), PermissionMode::Review);
        assert_eq!(PermissionMode::Deny.cycle(true), PermissionMode::Review);
    }
}

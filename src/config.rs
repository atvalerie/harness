use directories::ProjectDirs;
use keyring::Entry;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::PathBuf;

const KEYRING_SERVICE: &str = "gemini-harness";
const KEYRING_USER: &str = "api_key";
const DEFAULT_GEMINI_MODEL: &str = "gemini-3.5-flash-lite";

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
    pub system_instruction: String,
    #[serde(default = "default_provider_configs")]
    pub providers: BTreeMap<String, ProviderConfig>,
    #[serde(default)]
    pub model_profiles: BTreeMap<String, ModelProfile>,
    #[serde(default = "default_auto_compact")]
    pub auto_compact: bool,
    #[serde(default = "default_auto_compact_threshold")]
    pub auto_compact_threshold_tokens: u64,
    #[serde(default = "default_session_name")]
    pub session_name: String,
    #[serde(default)]
    pub mcp_servers: BTreeMap<String, McpServerConfig>,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            provider: default_provider(),
            base_url: None,
            model: DEFAULT_GEMINI_MODEL.to_string(),
            fallback_models: default_fallback_models(),
            max_retries: default_max_retries(),
            thinking_budget: 1024,
            temperature: 0.7,
            system_instruction: "You are an expert autonomous developer and coding assistant running in high-performance Gemini TUI Harness.\n\n\
You have direct access to native tools for filesystem inspection, safe command execution, and live web grounding:\n\
- `web_search(query)`: Query the live internet using DuckDuckGo HTML Lite to look up current documentation, breaking news, libraries, or release notes.\n\
- `web_fetch(url)`: Fetch and extract clean article text and code from web pages.\n\
- `read_file(path)`: Inspect existing source files and directory contents.\n\
- `write_file(path, content)`: Propose file writes and edits. The harness automatically generates unified diffs for the user to review in an interactive HITL modal.\n\
- `run_command(command)`: Run host shell commands. Always preview the exact command before requesting execution.\n\n\
Operational Guidelines:\n\
1. Proactively use `web_search` and `web_fetch` whenever you need up-to-date documentation or external facts.\n\
2. Prioritize reading relevant files before modifying them.\n\
3. Use `read_file` for source and text inspection, using its optional `start_line` and `max_lines` for bounded chunks. Do not emulate file reads with `run_command`, `type`, `Get-Content`, `findstr`, or line-count probes.\n\
4. Use `run_command` for actual execution such as builds, tests, git, and scripts. Its working directory persists after `cd` or `Set-Location`; use its optional `shell` field when a specific shell is required.\n\
5. Explain your thinking concisely. Be accurate, pragmatic, and write clean, production-ready code.".to_string(),
            providers: default_provider_configs(),
            model_profiles: BTreeMap::new(),
            auto_compact: default_auto_compact(),
            auto_compact_threshold_tokens: default_auto_compact_threshold(),
            session_name: "default".to_string(),
            mcp_servers: BTreeMap::new(),
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
fn default_true() -> bool {
    true
}
fn default_session_name() -> String {
    "default".to_string()
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
        self.provider = name.trim().to_string();
        if let Some(model) = self
            .providers
            .get(&self.provider)
            .and_then(|provider| provider.model.clone())
        {
            self.model = model;
        }
    }

    pub fn effective_fallback_models(&self) -> Vec<String> {
        let profile = self.active_provider_config();
        if profile.fallback_models.is_empty() {
            self.fallback_models.clone()
        } else {
            profile.fallback_models
        }
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
        ProjectDirs::from("com", "gemini", "gemini-harness")
            .map(|dirs| dirs.config_dir().to_path_buf())
    }

    pub fn config_path() -> Option<PathBuf> {
        if let Ok(path) = env::var("GEMINI_HARNESS_CONFIG") {
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
                        return Self::with_tool_guidance(cfg);
                    }
                }
            }
        }
        Self::default()
    }

    fn with_tool_guidance(mut config: Self) -> Self {
        const MARKER: &str = "Use `read_file` for source and text inspection";
        if !config.system_instruction.contains(MARKER) {
            config.system_instruction.push_str(
                "\n\nTool selection rules:\n\
- Use `read_file` for source and text inspection. Request `start_line` and `max_lines` when you need a bounded chunk; do not use shell commands to print files or count lines.\n\
- Use `run_command` for builds, tests, git, and other commands that must execute. Its working directory is persistent across tool calls, so `cd` and `Set-Location` affect subsequent tools.\n\
- `run_command` accepts an optional `shell` of `auto`, `powershell`, or `cmd` on Windows; leave it as `auto` unless syntax requires a specific shell.",
            );
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

        fs::write(&path, json).map_err(|e| format!("Failed to write config file: {}", e))?;

        Ok(())
    }

    pub fn get_api_key_for(provider: &str) -> Option<String> {
        // 1. Check environment variable first
        let variables: Vec<&str> = if provider.eq_ignore_ascii_case("openai")
            || provider.eq_ignore_ascii_case("openai-compatible")
        {
            vec!["OPENAI_API_KEY", "GEMINI_API_KEY"]
        } else if provider.eq_ignore_ascii_case("gemini") {
            vec!["GEMINI_API_KEY", "OPENAI_API_KEY"]
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

        // 2. Check native OS keyring
        if let Ok(entry) = Entry::new(KEYRING_SERVICE, KEYRING_USER) {
            if let Ok(secret) = entry.get_password() {
                let trimmed = secret.trim();
                if !trimmed.is_empty() {
                    return Some(trimmed.to_string());
                }
            }
        }

        // 3. Check fallback local .env or config directory file
        if let Some(cfg_dir) = Self::config_dir() {
            let key_file = cfg_dir.join(".key");
            if key_file.exists() {
                if let Ok(key) = fs::read_to_string(key_file) {
                    let trimmed = key.trim();
                    if !trimmed.is_empty() {
                        return Some(trimmed.to_string());
                    }
                }
            }
        }

        None
    }

    pub fn get_api_key_for_active_provider(&self) -> Option<String> {
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

    pub fn set_api_key(key: &str) -> Result<(), String> {
        let trimmed = key.trim();
        if trimmed.is_empty() {
            return Err("API key cannot be empty".to_string());
        }

        // Try OS keyring first
        let mut keyring_saved = false;
        if let Ok(entry) = Entry::new(KEYRING_SERVICE, KEYRING_USER) {
            if entry.set_password(trimmed).is_ok() {
                keyring_saved = true;
            }
        }

        // Also save to config dir .key file as reliable fallback
        if let Some(cfg_dir) = Self::config_dir() {
            let _ = fs::create_dir_all(&cfg_dir);
            let key_file = cfg_dir.join(".key");
            let _ = fs::write(key_file, trimmed);
        }

        if keyring_saved {
            Ok(())
        } else {
            // If fallback file was written, consider success
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{default_fallback_models, AppConfig, ProviderConfig};
    use std::collections::BTreeMap;

    #[test]
    fn defaults_target_free_tier_models() {
        let config = AppConfig::default();
        assert_eq!(config.model, "gemini-3.5-flash-lite");
        assert_eq!(config.fallback_models, default_fallback_models());
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
        let mut config = AppConfig::default();
        config.model = "gemini-3.8-flash".to_string();
        config.fallback_models = vec![
            "gemini-2.5-flash-lite".to_string(),
            "gemini-2.5-flash".to_string(),
        ];
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
}

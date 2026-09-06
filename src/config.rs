use directories::ProjectDirs;
use keyring::Entry;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::collections::BTreeMap;

const KEYRING_SERVICE: &str = "gemini-harness";
const KEYRING_USER: &str = "api_key";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    #[serde(default = "default_provider")]
    pub kind: String,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub fallback_models: Vec<String>,
    #[serde(default)]
    pub api_key_env: Option<String>,
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
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            provider: default_provider(),
            base_url: None,
            model: "gemini-3.8-flash".to_string(),
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
3. Explain your thinking concisely. Be accurate, pragmatic, and write clean, production-ready code.".to_string(),
            providers: default_provider_configs(),
            model_profiles: BTreeMap::new(),
            auto_compact: default_auto_compact(),
            auto_compact_threshold_tokens: default_auto_compact_threshold(),
        }
    }
}

fn default_provider() -> String { "gemini".to_string() }

fn default_fallback_models() -> Vec<String> {
    vec!["gemini-2.5-flash-lite".to_string(), "gemini-2.5-flash".to_string()]
}

fn default_max_retries() -> u32 { 3 }
fn default_auto_compact() -> bool { true }
fn default_auto_compact_threshold() -> u64 { 100_000 }

fn default_provider_configs() -> BTreeMap<String, ProviderConfig> {
    let mut providers = BTreeMap::new();
    providers.insert("gemini".to_string(), ProviderConfig {
        kind: "gemini".to_string(),
        base_url: None,
        fallback_models: default_fallback_models(),
        api_key_env: Some("GEMINI_API_KEY".to_string()),
    });
    providers.insert("openai".to_string(), ProviderConfig {
        kind: "openai-compatible".to_string(),
        base_url: None,
        fallback_models: Vec::new(),
        api_key_env: Some("OPENAI_API_KEY".to_string()),
    });
    providers
}

impl AppConfig {
    pub fn active_provider_config(&self) -> ProviderConfig {
        self.providers.get(&self.provider).cloned().unwrap_or_else(|| ProviderConfig {
            kind: self.provider.clone(),
            base_url: self.base_url.clone(),
            fallback_models: self.fallback_models.clone(),
            api_key_env: None,
        })
    }

    pub fn effective_fallback_models(&self) -> Vec<String> {
        let profile = self.active_provider_config();
        if profile.fallback_models.is_empty() { self.fallback_models.clone() } else { profile.fallback_models }
    }

    pub fn model_profile_key(&self) -> String { format!("{}:{}", self.provider, self.model) }

    pub fn active_model_profile(&self) -> ModelProfile {
        self.model_profiles.get(&self.model_profile_key()).cloned().or_else(|| self.model_profiles.get(&self.model).cloned()).unwrap_or_default()
    }

    pub fn config_dir() -> Option<PathBuf> {
        ProjectDirs::from("com", "gemini", "gemini-harness")
            .map(|dirs| dirs.config_dir().to_path_buf())
    }

    pub fn config_path() -> Option<PathBuf> {
        Self::config_dir().map(|dir| dir.join("config.json"))
    }

    pub fn load() -> Self {
        if let Some(path) = Self::config_path() {
            if path.exists() {
                if let Ok(content) = fs::read_to_string(&path) {
                    if let Ok(cfg) = serde_json::from_str::<AppConfig>(&content) {
                        return cfg;
                    }
                }
            }
        }
        Self::default()
    }

    pub fn save(&self) -> Result<(), String> {
        let path = Self::config_path()
            .ok_or_else(|| "Failed to resolve config directory".to_string())?;

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create config dir: {}", e))?;
        }

        let json = serde_json::to_string_pretty(self)
            .map_err(|e| format!("Serialization error: {}", e))?;

        fs::write(&path, json)
            .map_err(|e| format!("Failed to write config file: {}", e))?;

        Ok(())
    }

    pub fn get_api_key_for(provider: &str) -> Option<String> {
        // 1. Check environment variable first
        let variables = if provider.eq_ignore_ascii_case("openai") || provider.eq_ignore_ascii_case("openai-compatible") {
            ["OPENAI_API_KEY", "GEMINI_API_KEY"]
        } else {
            ["GEMINI_API_KEY", "OPENAI_API_KEY"]
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

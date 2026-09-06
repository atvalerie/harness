use directories::ProjectDirs;
use keyring::Entry;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

const KEYRING_SERVICE: &str = "gemini-harness";
const KEYRING_USER: &str = "api_key";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    pub model: String,
    #[serde(default = "default_fallback_models")]
    pub fallback_models: Vec<String>,
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,
    pub thinking_budget: i32,
    pub temperature: f32,
    pub system_instruction: String,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
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
        }
    }
}

fn default_fallback_models() -> Vec<String> {
    vec!["gemini-2.5-flash-lite".to_string(), "gemini-2.5-flash".to_string()]
}

fn default_max_retries() -> u32 { 3 }

impl AppConfig {
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

    pub fn get_api_key() -> Option<String> {
        // 1. Check environment variable first
        if let Ok(key) = std::env::var("GEMINI_API_KEY") {
            let trimmed = key.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
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

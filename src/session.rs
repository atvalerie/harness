use crate::app::ChatMessage;
use crate::config::AppConfig;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSnapshot {
    pub provider: String,
    pub model: String,
    pub messages: Vec<ChatMessage>,
}

pub fn default_session_path(name: &str) -> Option<PathBuf> {
    AppConfig::config_dir().map(|dir| dir.join("sessions").join(format!("{}.json", name)))
}

pub fn load(path: &Path) -> Option<SessionSnapshot> {
    fs::read_to_string(path).ok().and_then(|content| serde_json::from_str(&content).ok())
}

pub fn save(path: &Path, snapshot: &SessionSnapshot) -> Result<(), String> {
    let parent = path.parent().ok_or_else(|| "Session path has no parent".to_string())?;
    fs::create_dir_all(parent).map_err(|e| format!("Failed to create session directory: {}", e))?;
    let json = serde_json::to_string_pretty(snapshot).map_err(|e| format!("Failed to serialize session: {}", e))?;
    let temp = path.with_extension("json.tmp");
    fs::write(&temp, json).map_err(|e| format!("Failed to write session: {}", e))?;
    fs::rename(&temp, path).map_err(|e| format!("Failed to commit session: {}", e))
}


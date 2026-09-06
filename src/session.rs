use crate::app::ChatMessage;
use crate::config::AppConfig;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSnapshot {
    pub provider: String,
    pub model: String,
    pub messages: Vec<ChatMessage>,
}

#[derive(Debug, Clone)]
pub struct SessionInfo {
    pub name: String,
    pub path: PathBuf,
    pub provider: String,
    pub model: String,
    pub messages: usize,
    pub modified: SystemTime,
}

pub fn default_session_path(name: &str) -> Option<PathBuf> {
    AppConfig::config_dir().map(|dir| dir.join("sessions").join(format!("{}.json", name)))
}

pub fn new_session_path(prefix: &str) -> Option<PathBuf> {
    let name = format!(
        "{}-{}",
        prefix,
        chrono::Local::now().format("%Y%m%d-%H%M%S-%3f")
    );
    default_session_path(&name)
}

pub fn load(path: &Path) -> Option<SessionSnapshot> {
    fs::read_to_string(path)
        .ok()
        .and_then(|content| serde_json::from_str(&content).ok())
}

pub fn save(path: &Path, snapshot: &SessionSnapshot) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "Session path has no parent".to_string())?;
    fs::create_dir_all(parent).map_err(|e| format!("Failed to create session directory: {}", e))?;
    let json = serde_json::to_string_pretty(snapshot)
        .map_err(|e| format!("Failed to serialize session: {}", e))?;
    let temp = path.with_extension("json.tmp");
    fs::write(&temp, json).map_err(|e| format!("Failed to write session: {}", e))?;
    fs::rename(&temp, path).map_err(|e| format!("Failed to commit session: {}", e))
}

pub fn list() -> Vec<SessionInfo> {
    let Some(dir) = AppConfig::config_dir().map(|path| path.join("sessions")) else {
        return Vec::new();
    };
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut sessions = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                return None;
            }
            if path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .map(|stem| stem.ends_with(".export"))
                .unwrap_or(false)
            {
                return None;
            }
            let snapshot = load(&path)?;
            let name = path.file_stem()?.to_string_lossy().to_string();
            let modified = entry
                .metadata()
                .and_then(|meta| meta.modified())
                .unwrap_or(SystemTime::UNIX_EPOCH);
            Some(SessionInfo {
                name,
                path,
                provider: snapshot.provider,
                model: snapshot.model,
                messages: snapshot.messages.len(),
                modified,
            })
        })
        .collect::<Vec<_>>();
    sessions.sort_by(|a, b| b.modified.cmp(&a.modified));
    sessions
}

use crate::app::ChatMessage;
use crate::config::AppConfig;
use crate::tools::TodoItem;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSnapshot {
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    pub provider: String,
    pub model: String,
    pub messages: Vec<ChatMessage>,
    #[serde(default)]
    pub usage: Vec<UsageRecord>,
    /// The project context is optional so snapshots from older releases stay
    /// readable and portable.
    #[serde(default)]
    pub working_dir: Option<PathBuf>,
    #[serde(default)]
    pub project_root: Option<PathBuf>,
    #[serde(default)]
    pub project_instructions: Option<String>,
    #[serde(default)]
    pub todos: Vec<TodoItem>,
    #[serde(default)]
    pub plan_mode: bool,
}

fn default_schema_version() -> u32 {
    1
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageRecord {
    pub timestamp: String,
    pub provider: String,
    pub model: String,
    pub prompt_tokens: u64,
    pub candidates_tokens: u64,
    pub total_tokens: u64,
    pub estimated: bool,
    pub duration_ms: u64,
    pub status: String,
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
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("session.json");
    let temp = path.with_file_name(format!(".{}.{}.tmp", file_name, std::process::id()));
    fs::write(&temp, json).map_err(|e| format!("Failed to write session: {}", e))?;
    match fs::rename(&temp, path) {
        Ok(()) => Ok(()),
        Err(_error) if cfg!(windows) && path.exists() => {
            // Windows does not replace an existing destination with rename.
            // The destination is exact and the new snapshot is already fully
            // written, so replace it as a fallback.
            fs::remove_file(path)
                .map_err(|remove_error| format!("Failed to replace session: {}", remove_error))?;
            fs::rename(&temp, path)
                .map_err(|rename_error| format!("Failed to commit session: {}", rename_error))
        }
        Err(error) => Err(format!("Failed to commit session: {}", error)),
    }
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

#[cfg(test)]
mod tests {
    use super::SessionSnapshot;

    #[test]
    fn loads_legacy_snapshot_with_usage_defaults() {
        let snapshot: SessionSnapshot =
            serde_json::from_str(r#"{"provider":"gemini","model":"test","messages":[]}"#)
                .expect("legacy session should remain readable");
        assert_eq!(snapshot.schema_version, 1);
        assert!(snapshot.usage.is_empty());
        assert!(snapshot.working_dir.is_none());
    }
}

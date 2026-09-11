use crate::app::ChatMessage;
use crate::config::AppConfig;
use crate::tools::TodoItem;
use serde::{Deserialize, Serialize};
use std::fs;
use fs2::FileExt;
use std::fs::File;
use std::io::{Seek, SeekFrom, Write};
use std::sync::atomic::{AtomicU64, Ordering};
static NEW_SESSION_COUNTER: AtomicU64 = AtomicU64::new(0);
use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSnapshot {
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    #[serde(default)]
    pub checkpoints: Vec<crate::review::Checkpoint>,
    pub provider: String,
    pub model: String,
    pub messages: Vec<ChatMessage>,
    #[serde(default)]
    pub transcript_history: Vec<ChatMessage>,
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
    #[serde(default)]
    pub details: crate::usage::TokenUsage,
    #[serde(default)]
    pub cost_nano_usd: Option<u64>,
    #[serde(default)]
    pub pricing: Option<crate::usage::Pricing>,
}

/// An OS-level exclusive lock for one saved session.
pub struct SessionLock {
    file: File,
}

impl SessionLock {
    pub fn acquire(session: &Path) -> Result<Self, String> {
        let parent = session
            .parent()
            .ok_or_else(|| "Session path has no parent".to_string())?;
        fs::create_dir_all(parent)
            .map_err(|error| format!("Failed to create session directory: {error}"))?;
        let path = session.with_extension("lock");
        let mut file = fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .read(true)
            .write(true)
            .open(&path)
            .map_err(|error| format!("Cannot open session lock {}: {error}", path.display()))?;
        if let Err(error) = file.try_lock_exclusive() {
            return Err(format!(
                "Session is already in use by another Harness process: {} ({error})",
                session.display()
            ));
        }
        let write_result = (|| {
            file.set_len(0)?;
            file.seek(SeekFrom::Start(0))?;
            writeln!(
                &file,
                "pid={} acquired_at={}",
                std::process::id(),
                chrono::Utc::now().to_rfc3339()
            )?;
            file.sync_data()
        })();
        if let Err(error) = write_result {
            let _ = file.unlock();
            return Err(format!(
                "Cannot record session lock {}: {error}",
                path.display()
            ));
        }
        Ok(Self { file })
    }
}

impl Drop for SessionLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
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
        "{}-{}-{}-{}",
        prefix,
        chrono::Local::now().format("%Y%m%d-%H%M%S-%3f"),
        std::process::id(),
        NEW_SESSION_COUNTER.fetch_add(1, Ordering::Relaxed)
    );
    default_session_path(&name)
}

pub fn load(path: &Path) -> Option<SessionSnapshot> {
    // A partial final journal record is ignored; earlier records remain usable.
    let journal = journal_path(path);
    if let Ok(file) = std::fs::File::open(journal) {
        use std::io::BufRead;
        let mut latest: Option<SessionSnapshot> = None;
        for line in std::io::BufReader::new(file).lines().map_while(Result::ok) {
            if let Ok(event) = serde_json::from_str::<JournalRecord>(&line) {
                if event.version == 1 {
                    if event.base_messages == 0
                        && event.base_usage == 0
                        && event.base_checkpoints == 0
                    {
                        latest = Some(event.snapshot);
                    } else if let Some(previous) = latest.take() {
                        if event.base_messages <= previous.messages.len()
                            && event.base_usage <= previous.usage.len()
                            && event.base_checkpoints <= previous.checkpoints.len()
                        {
                            let mut next = event.snapshot;
                            let mut messages = previous
                                .messages
                                .into_iter()
                                .take(event.base_messages)
                                .collect::<Vec<_>>();
                            messages.append(&mut next.messages);
                            next.messages = messages;
                            let mut usage = previous
                                .usage
                                .into_iter()
                                .take(event.base_usage)
                                .collect::<Vec<_>>();
                            usage.append(&mut next.usage);
                            next.usage = usage;
                            let mut checkpoints = previous
                                .checkpoints
                                .into_iter()
                                .take(event.base_checkpoints)
                                .collect::<Vec<_>>();
                            checkpoints.append(&mut next.checkpoints);
                            next.checkpoints = checkpoints;
                            latest = Some(next);
                        } else {
                            latest = Some(previous);
                        }
                    }
                }
            }
        }
        if latest.is_some() {
            return latest;
        }
    }
    fs::read_to_string(path)
        .ok()
        .and_then(|content| serde_json::from_str(&content).ok())
}
#[derive(Serialize, Deserialize)]
struct JournalRecord {
    version: u32,
    timestamp: String,
    kind: String,
    snapshot: SessionSnapshot,
    #[serde(default)]
    base_messages: usize,
    #[serde(default)]
    base_usage: usize,
    #[serde(default)]
    base_checkpoints: usize,
}
fn journal_path(path: &Path) -> PathBuf {
    path.with_extension("events.jsonl")
}

pub fn meta_path(path: &Path) -> PathBuf {
    path.with_extension("meta.json")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMetadata {
    pub version: u32,
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub project_root: Option<PathBuf>,
    pub provider: String,
    pub model: String,
    pub created_at: String,
    pub updated_at: String,
    pub message_count: usize,
    pub prompt_tokens: u64,
    pub candidates_tokens: u64,
    pub total_tokens: u64,
    #[serde(default)]
    pub keywords: Vec<String>,
}

fn derive_session_title(snapshot: &SessionSnapshot, fallback: &str) -> String {
    // 1. Look for the first meaningful user message to extract intent
    for msg in &snapshot.messages {
        if msg.role == "user" {
            let line = msg.content.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
            let clean = line.trim();
            if !clean.is_empty() {
                let truncated = clean.chars().take(60).collect::<String>();
                return if clean.chars().count() > 60 {
                    format!("{truncated}...")
                } else {
                    truncated
                };
            }
        }
    }
    // 2. Look for summary checkpoint
    for msg in &snapshot.messages {
        if msg.role == "summary" {
            if let Some(pos) = msg.content.find("## Objective and authorization") {
                let after = &msg.content[pos..];
                if let Some(line) = after.lines().nth(1) {
                    let clean = line.trim().trim_start_matches("- ").trim();
                    if !clean.is_empty() {
                        return clean.chars().take(60).collect::<String>();
                    }
                }
            }
        }
    }
    fallback.to_string()
}

fn extract_keywords(snapshot: &SessionSnapshot) -> Vec<String> {
    let mut words = std::collections::BTreeSet::new();
    for msg in &snapshot.messages {
        if msg.role == "user" || msg.role == "tool" {
            for word in msg.content.split(|c: char| !c.is_alphanumeric() && c != '_' && c != '-') {
                let w = word.trim().to_ascii_lowercase();
                if w.len() >= 4 && w.len() <= 20 {
                    words.insert(w);
                }
            }
        }
    }
    words.into_iter().take(25).collect()
}

pub fn save_metadata(path: &Path, snapshot: &SessionSnapshot) {
    let id = path.file_stem().and_then(|s| s.to_str()).unwrap_or("unknown");
    let title = derive_session_title(snapshot, id);
    let (mut prompt_tokens, mut candidates_tokens, mut total_tokens): (u64, u64, u64) = (0, 0, 0);
    for u in &snapshot.usage {
        prompt_tokens = prompt_tokens.saturating_add(u.prompt_tokens);
        candidates_tokens = candidates_tokens.saturating_add(u.candidates_tokens);
        total_tokens = total_tokens.saturating_add(u.total_tokens);
    }
    let now = chrono::Utc::now().to_rfc3339();
    let meta = SessionMetadata {
        version: 1,
        id: id.to_string(),
        title,
        project_root: snapshot.project_root.clone().or_else(|| snapshot.working_dir.clone()),
        provider: snapshot.provider.clone(),
        model: snapshot.model.clone(),
        created_at: snapshot.usage.first().map(|u| u.timestamp.clone()).unwrap_or_else(|| now.clone()),
        updated_at: now,
        message_count: snapshot.messages.len(),
        prompt_tokens,
        candidates_tokens,
        total_tokens,
        keywords: extract_keywords(snapshot),
    };
    if let Ok(json) = serde_json::to_string_pretty(&meta) {
        let meta_target = meta_path(path);
        let temp = meta_target.with_extension(format!("tmp.{}", std::process::id()));
        if let Ok(()) = fs::write(&temp, json.as_bytes()) {
            let _ = fs::rename(&temp, &meta_target);
        }
    }
}

fn saved_states(
) -> &'static std::sync::Mutex<std::collections::HashMap<PathBuf, (SessionSnapshot, u64)>> {
    static STATES: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<PathBuf, (SessionSnapshot, u64)>>,
    > = std::sync::OnceLock::new();
    STATES.get_or_init(Default::default)
}
fn prefix<T: Serialize>(a: &[T], b: &[T]) -> usize {
    a.iter()
        .zip(b)
        .take_while(|(a, b)| serde_json::to_vec(a).ok() == serde_json::to_vec(b).ok())
        .count()
}

pub fn save(path: &Path, snapshot: &SessionSnapshot) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "Session path has no parent".to_string())?;
    fs::create_dir_all(parent).map_err(|e| format!("Failed to create session directory: {}", e))?;
    // Write-ahead checkpoint first. Never remove the old snapshot to replace it.
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut journal = options
        .open(journal_path(path))
        .map_err(|e| format!("Cannot open session journal: {e}"))?;
    let mut saved = saved_states()
        .lock()
        .map_err(|_| "Session state cache unavailable")?;
    let previous = saved.get(path);
    let revision = previous.map(|(_, r)| r + 1).unwrap_or(0);
    let mut delta = snapshot.clone();
    let (mut base_messages, mut base_usage, mut base_checkpoints) = (0, 0, 0);
    if let Some((previous, _)) = previous.filter(|_| revision % 32 != 0) {
        base_messages = prefix(&previous.messages, &snapshot.messages);
        base_usage = prefix(&previous.usage, &snapshot.usage);
        base_checkpoints = prefix(&previous.checkpoints, &snapshot.checkpoints);
        delta.messages.drain(..base_messages);
        delta.usage.drain(..base_usage);
        delta.checkpoints.drain(..base_checkpoints);
    }
    let mut record = serde_json::to_vec(&JournalRecord {
        version: 1,
        timestamp: chrono::Utc::now().to_rfc3339(),
        kind: if revision % 32 == 0 {
            "checkpoint"
        } else {
            "append"
        }
        .into(),
        snapshot: delta,
        base_messages,
        base_usage,
        base_checkpoints,
    })
    .map_err(|e| e.to_string())?;
    // A leading newline isolates a previously torn final record from this commit.
    record.insert(0, b'\n');
    record.push(b'\n');
    journal
        .write_all(&record)
        .and_then(|_| journal.sync_data())
        .map_err(|e| format!("Cannot commit session journal: {e}"))?;
    if saved.len() > 16 {
        saved.clear();
    }
    saved.insert(path.to_path_buf(), (snapshot.clone(), revision));
    drop(saved);
    if revision % 32 != 0 && path.exists() {
        return Ok(());
    }
    let json = serde_json::to_string_pretty(snapshot)
        .map_err(|e| format!("Failed to serialize session: {}", e))?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("session.json");
    let temp = path.with_file_name(format!(".{}.{}.tmp", file_name, std::process::id()));
    let mut options = std::fs::OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
        .open(&temp)
        .and_then(|mut file| {
            file.write_all(json.as_bytes())
                .and_then(|_| file.sync_data())
        })
        .map_err(|e| format!("Failed to write snapshot: {e}"))?;
    match fs::rename(&temp, path) {
        Ok(()) => Ok(()),
        Err(_error) if cfg!(windows) && path.exists() => {
            // Recovery uses the durably appended journal. Keep the previous snapshot
            // intact rather than introducing a delete/rename data-loss window.
            let _ = fs::remove_file(&temp);
            Ok(())
        }
        Err(error) => Err(format!("Failed to commit session: {}", error)),
    }?;
    save_metadata(path, snapshot);
    Ok(())
}

pub fn delete(path: &Path) -> Result<(), String> {
    // Exact sidecar paths only; never recursive deletion.
    for file in [
        journal_path(path),
        meta_path(path),
        path.with_extension("changes.jsonl"),
        path.to_path_buf(),
    ] {
        match fs::remove_file(file) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.to_string()),
        }
    }
    if let Ok(mut saved) = saved_states().lock() {
        saved.remove(path);
    }
    Ok(())
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
                .map(|stem| stem.ends_with(".export") || stem.ends_with(".meta"))
                .unwrap_or(false)
            {
                return None;
            }
            let name = path.file_stem()?.to_string_lossy().to_string();
            let modified = fs::metadata(journal_path(&path))
                .or_else(|_| entry.metadata())
                .and_then(|meta| meta.modified())
                .unwrap_or(SystemTime::UNIX_EPOCH);

            // Fast path: try loading lightweight sidecar meta.json
            let meta_file = meta_path(&path);
            if let Ok(meta_content) = fs::read_to_string(&meta_file) {
                if let Ok(meta) = serde_json::from_str::<SessionMetadata>(&meta_content) {
                    return Some(SessionInfo {
                        name,
                        path,
                        provider: meta.provider,
                        model: meta.model,
                        messages: meta.message_count,
                        modified,
                    });
                }
            }

            // Fallback / self-healing: read full snapshot and generate sidecar
            let snapshot = load(&path)?;
            save_metadata(&path, &snapshot);
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
    sessions.sort_by_key(|session| std::cmp::Reverse(session.modified));
    sessions
}

#[cfg(test)]
mod tests {
    use super::SessionSnapshot;

    #[test]
    fn journal_replays_deltas_and_survives_torn_record() {
        use std::io::Write;
        let root = std::env::temp_dir().join(format!(
            "holiday-journal-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("session.json");
        let mut snapshot: super::SessionSnapshot = serde_json::from_value(
            serde_json::json!({"provider":"test","model":"test","messages":[]}),
        )
        .unwrap();
        super::save(&path, &snapshot).unwrap();
        snapshot.messages.push(crate::app::ChatMessage {
            role: "user".into(),
            content: "first".into(),
            timestamp: "now".into(),
            attachments: Vec::new(),
        });
        super::save(&path, &snapshot).unwrap();
        snapshot.messages.push(crate::app::ChatMessage {
            role: "model".into(),
            content: "second".into(),
            timestamp: "now".into(),
            attachments: Vec::new(),
        });
        super::save(&path, &snapshot).unwrap();
        let journal = super::journal_path(&path);
        std::fs::OpenOptions::new()
            .append(true)
            .open(&journal)
            .unwrap()
            .write_all(b"{broken")
            .unwrap();
        assert_eq!(super::load(&path).unwrap().messages.len(), 2);
        snapshot.messages.truncate(1);
        super::save(&path, &snapshot).unwrap();
        assert_eq!(super::load(&path).unwrap().messages.len(), 1);
        std::fs::remove_file(&path).unwrap();
        assert_eq!(super::load(&path).unwrap().messages[0].content, "first");
        super::delete(&path).unwrap();
        assert!(super::load(&path).is_none());
        std::fs::remove_dir(root).unwrap();
    }

    #[test]
    fn metadata_sidecar_round_trips_and_derives_title() {
        let root = std::env::temp_dir().join(format!(
            "holiday-meta-test-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("test-session.json");
        let mut snapshot: super::SessionSnapshot = serde_json::from_value(
            serde_json::json!({"provider":"openai","model":"gpt-4o","messages":[]}),
        )
        .unwrap();
        snapshot.messages.push(crate::app::ChatMessage {
            role: "user".into(),
            content: "Fix TUI autocomplete acceptance and keybindings".into(),
            timestamp: "12:00:00".into(),
            attachments: Vec::new(),
        });
        super::save(&path, &snapshot).unwrap();

        let meta_file = super::meta_path(&path);
        assert!(meta_file.exists());
        let meta_content = std::fs::read_to_string(&meta_file).unwrap();
        let meta: super::SessionMetadata = serde_json::from_str(&meta_content).unwrap();
        assert_eq!(meta.provider, "openai");
        assert_eq!(meta.model, "gpt-4o");
        assert_eq!(meta.title, "Fix TUI autocomplete acceptance and keybindings");
        assert!(meta.keywords.contains(&"autocomplete".to_string()));
        super::delete(&path).unwrap();
        assert!(!meta_file.exists());
        let _ = std::fs::remove_dir(root);
    }

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

use async_trait::async_trait;
use serde_json::json;
use std::process::Stdio;
use tokio::process::Command;

use super::{resolve_path, working_dir_path, SharedWorkingDir, Tool, ToolPreview};

pub struct SearchFilesTool {
    cwd: SharedWorkingDir,
}

impl SearchFilesTool {
    pub fn new(cwd: SharedWorkingDir) -> Self {
        Self { cwd }
    }
}

#[async_trait]
impl Tool for SearchFilesTool {
    fn name(&self) -> &'static str {
        "search_files"
    }

    fn description(&self) -> &'static str {
        "Searches repository files with ripgrep. Returns file, line, column, and matching text without invoking a shell."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "query": {"type": "string", "description": "Literal or regex pattern to search for"},
                "path": {"type": "string", "description": "File or directory relative to the current working directory (default: .)"},
                "glob": {"type": "string", "description": "Optional file glob such as *.rs"},
                "max_results": {"type": "integer", "minimum": 1, "maximum": 1000, "description": "Maximum matching lines to return (default: 100)"},
                "context_lines": {"type": "integer", "minimum": 0, "maximum": 10, "description": "Matching context lines (default: 0)"}
            },
            "required": ["query"]
        })
    }

    fn generate_preview(&self, args: &serde_json::Value) -> ToolPreview {
        let query = args
            .get("query")
            .and_then(|value| value.as_str())
            .unwrap_or("<missing query>");
        let path = args
            .get("path")
            .and_then(|value| value.as_str())
            .unwrap_or(".");
        let resolved_path = resolve_path(path, &working_dir_path(&self.cwd));
        ToolPreview {
            title: "Search Files (ripgrep)".to_string(),
            details: vec![
                format!("Query: {}", query),
                format!("Path: {}", resolved_path.display()),
            ],
            reason: None,
            expected_effect: None,
            command: None,
            diff_hunks: Vec::new(),
            is_mutation: false,
        }
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let query = args
            .get("query")
            .and_then(|value| value.as_str())
            .ok_or_else(|| "Missing required parameter 'query'".to_string())?;
        if query.is_empty() {
            return Err("Search query cannot be empty".to_string());
        }

        let path = args
            .get("path")
            .and_then(|value| value.as_str())
            .unwrap_or(".");
        let max_results = args
            .get("max_results")
            .and_then(|value| value.as_u64())
            .unwrap_or(100)
            .clamp(1, 1000);
        let context_lines = args
            .get("context_lines")
            .and_then(|value| value.as_u64())
            .unwrap_or(0)
            .min(10);

        let mut command = Command::new("rg");
        command
            .arg("--line-number")
            .arg("--column")
            .arg("--with-filename")
            .arg("--color")
            .arg("never")
            .arg("--max-count")
            .arg(max_results.to_string());
        if context_lines > 0 {
            command.arg("--context").arg(context_lines.to_string());
        }
        if let Some(glob) = args.get("glob").and_then(|value| value.as_str()) {
            if !glob.is_empty() {
                command.arg("--glob").arg(glob);
            }
        }
        let resolved_path = resolve_path(path, &working_dir_path(&self.cwd));
        command
            .arg("--")
            .arg(query)
            .arg(resolved_path)
            .current_dir(working_dir_path(&self.cwd))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let output = command.output().await.map_err(|error| {
            format!(
                "Could not run ripgrep: {}. Use bounded read_file/list_directory inspection, or run_command as a fallback if the active mode and permissions permit it.",
                error
            )
        })?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        match output.status.code() {
            Some(0) => {
                let truncated = stdout.chars().take(12000).collect::<String>();
                if stdout.chars().count() > 12000 {
                    Ok(format!(
                        "{}\n[Search output limited to 12,000 characters]",
                        truncated
                    ))
                } else {
                    Ok(truncated)
                }
            }
            Some(1) => Ok(format!("No matches for {:?}.", query)),
            _ => Err(format!("ripgrep failed: {}", stderr.trim())),
        }
    }
}

pub struct FindSymbolsTool {
    cwd: SharedWorkingDir,
}

impl FindSymbolsTool {
    pub fn new(cwd: SharedWorkingDir) -> Self {
        Self { cwd }
    }
}

#[async_trait]
impl Tool for FindSymbolsTool {
    fn name(&self) -> &'static str {
        "find_symbols"
    }

    fn description(&self) -> &'static str {
        "Finds symbol definitions (functions, structs, classes, enums, traits, methods, constants) across codebase without loading indices into RAM."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "query": {"type": "string", "description": "Symbol name or substring to search for"},
                "path": {"type": "string", "description": "Optional subdirectory or file to scope search to"},
                "kind": {"type": "string", "description": "Optional symbol kind: fn, struct, class, enum, trait, const, type, interface"},
                "max_results": {"type": "integer", "minimum": 1, "maximum": 500, "description": "Maximum matches to return (default: 50)"}
            },
            "required": ["query"]
        })
    }

    fn generate_preview(&self, args: &serde_json::Value) -> ToolPreview {
        let query = args
            .get("query")
            .and_then(|value| value.as_str())
            .unwrap_or("<missing query>");
        let path = args
            .get("path")
            .and_then(|value| value.as_str())
            .unwrap_or(".");
        let resolved_path = resolve_path(path, &working_dir_path(&self.cwd));
        ToolPreview {
            title: "Find Symbols (on-demand streaming)".to_string(),
            details: vec![
                format!("Query: {}", query),
                format!("Path: {}", resolved_path.display()),
            ],
            reason: None,
            expected_effect: None,
            command: None,
            diff_hunks: Vec::new(),
            is_mutation: false,
        }
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let query = args
            .get("query")
            .and_then(|value| value.as_str())
            .ok_or_else(|| "Missing required parameter 'query'".to_string())?;
        if query.is_empty() {
            return Err("Symbol query cannot be empty".to_string());
        }

        let path = args
            .get("path")
            .and_then(|value| value.as_str())
            .unwrap_or(".");
        let max_results = args
            .get("max_results")
            .and_then(|value| value.as_u64())
            .unwrap_or(50)
            .clamp(1, 500);

        let kind = args.get("kind").and_then(|v| v.as_str()).unwrap_or("");

        // Construct targeted regex for definition keywords
        let pattern = if !kind.is_empty() {
            format!(r"\b{}\s+.*{}\b", regex_escape(kind), regex_escape(query))
        } else {
            format!(r"\b(fn|function|def|struct|class|enum|interface|trait|type|const)\s+.*{}\b", regex_escape(query))
        };

        let mut command = Command::new("rg");
        command
            .arg("--line-number")
            .arg("--column")
            .arg("--with-filename")
            .arg("--color")
            .arg("never")
            .arg("--max-count")
            .arg(max_results.to_string())
            .arg("-e")
            .arg(pattern);

        let resolved_path = resolve_path(path, &working_dir_path(&self.cwd));
        command
            .arg(resolved_path)
            .current_dir(working_dir_path(&self.cwd))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let output = command.output().await.map_err(|error| {
            format!("Could not run ripgrep for symbol search: {}", error)
        })?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        match output.status.code() {
            Some(0) => {
                let lines: Vec<&str> = stdout.lines().take(max_results as usize).collect();
                if lines.is_empty() {
                    Ok(format!("No symbols found matching {:?}.", query))
                } else {
                    Ok(lines.join("\n"))
                }
            }
            Some(1) => Ok(format!("No symbols found matching {:?}.", query)),
            _ => Err(format!("ripgrep failed: {}", stderr.trim())),
        }
    }
}

fn regex_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, '\\' | '.' | '+' | '*' | '?' | '(' | ')' | '|' | '[' | ']' | '{' | '}' | '^' | '$') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

pub struct SearchHistoryTool {
    history: super::HistoryStore,
}

impl SearchHistoryTool {
    pub fn new(history: super::HistoryStore) -> Self {
        Self { history }
    }
}

#[async_trait]
impl Tool for SearchHistoryTool {
    fn name(&self) -> &'static str {
        "search_history"
    }

    fn description(&self) -> &'static str {
        "Searches the raw session transcript (including pre-compaction turns) for a query string or keyword. Returns matching messages with message IDs and timestamps."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Literal text to search for across past user, assistant, and tool turns"
                },
                "max_results": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 50,
                    "description": "Maximum number of matching messages to return (default: 10)"
                }
            },
            "required": ["query"]
        })
    }

    fn generate_preview(&self, args: &serde_json::Value) -> ToolPreview {
        let query = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
        ToolPreview {
            title: "Search Session History".to_string(),
            details: vec![format!("Query: {}", query)],
            reason: None,
            expected_effect: None,
            command: None,
            diff_hunks: Vec::new(),
            is_mutation: false,
        }
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let query = args
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "Missing required parameter 'query'".to_string())?
            .trim()
            .to_ascii_lowercase();
        if query.is_empty() {
            return Err("Search query cannot be empty".to_string());
        }
        let max_results = args
            .get("max_results")
            .and_then(|v| v.as_u64())
            .unwrap_or(10) as usize;

        let history = self
            .history
            .lock()
            .map_err(|_| "History lock poisoned".to_string())?;

        let mut matches = Vec::new();
        for (idx, msg) in history.iter().enumerate() {
            if msg.role == "thought" {
                continue;
            }
            if msg.content.to_ascii_lowercase().contains(&query) {
                let preview = msg
                    .content
                    .lines()
                    .find(|l| l.to_ascii_lowercase().contains(&query))
                    .unwrap_or_else(|| msg.content.lines().next().unwrap_or(""));
                let snippet = preview.trim().chars().take(120).collect::<String>();
                matches.push(format!(
                    "[Msg #{idx} | {} | role: {}]\n  {snippet}",
                    msg.timestamp, msg.role
                ));
                if matches.len() >= max_results {
                    break;
                }
            }
        }

        if matches.is_empty() {
            Ok(format!("No history messages matched query: {query:?}"))
        } else {
            Ok(format!(
                "Found {} matching message(s) in session history:\n\n{}",
                matches.len(),
                matches.join("\n\n")
            ))
        }
    }
}

pub struct ReadHistoryTool {
    history: super::HistoryStore,
}

impl ReadHistoryTool {
    pub fn new(history: super::HistoryStore) -> Self {
        Self { history }
    }
}

#[async_trait]
impl Tool for ReadHistoryTool {
    fn name(&self) -> &'static str {
        "read_history"
    }

    fn description(&self) -> &'static str {
        "Inspects full raw messages from session history (including pre-compaction turns) around a specific message ID."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "start_id": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "1-based or 0-based message ID to read from"
                },
                "count": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 20,
                    "description": "Number of consecutive messages to inspect (default: 3)"
                }
            },
            "required": ["start_id"]
        })
    }

    fn generate_preview(&self, args: &serde_json::Value) -> ToolPreview {
        let start_id = args.get("start_id").and_then(|v| v.as_u64()).unwrap_or(0);
        let count = args.get("count").and_then(|v| v.as_u64()).unwrap_or(3);
        ToolPreview {
            title: "Read Session History".to_string(),
            details: vec![format!("Message ID: {}, Count: {}", start_id, count)],
            reason: None,
            expected_effect: None,
            command: None,
            diff_hunks: Vec::new(),
            is_mutation: false,
        }
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let start_id = args
            .get("start_id")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| "Missing required parameter 'start_id'".to_string())? as usize;
        let count = args
            .get("count")
            .and_then(|v| v.as_u64())
            .unwrap_or(3) as usize;

        let history = self
            .history
            .lock()
            .map_err(|_| "History lock poisoned".to_string())?;

        if history.is_empty() {
            return Ok("Session history is empty.".to_string());
        }

        let slice_start = start_id.min(history.len().saturating_sub(1));
        let slice_end = (slice_start + count).min(history.len());

        let mut output = Vec::new();
        for idx in slice_start..slice_end {
            let msg = &history[idx];
            if msg.role == "thought" {
                continue;
            }
            output.push(format!(
                "--- Message #{idx} [{} | role: {}] ---\n{}",
                msg.timestamp, msg.role, msg.content
            ));
        }

        if output.is_empty() {
            Ok(format!("No readable messages in range #{slice_start}..#{slice_end}"))
        } else {
            Ok(output.join("\n\n"))
        }
    }
}

pub struct ListSessionsTool;

#[async_trait]
impl Tool for ListSessionsTool {
    fn name(&self) -> &'static str {
        "list_sessions"
    }

    fn description(&self) -> &'static str {
        "Lists saved sessions with lightweight metadata (session ID, title, model, modified time, message count). Fast and reads from sidecar index without parsing full transcripts."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "limit": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 50,
                    "description": "Maximum number of recent sessions to return (default: 10)"
                }
            }
        })
    }

    fn generate_preview(&self, args: &serde_json::Value) -> ToolPreview {
        let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(10);
        ToolPreview {
            title: "List Sessions".to_string(),
            details: vec![format!("Limit: {}", limit)],
            reason: None,
            expected_effect: None,
            command: None,
            diff_hunks: Vec::new(),
            is_mutation: false,
        }
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let limit = args
            .get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(10) as usize;

        let sessions = crate::session::list();
        if sessions.is_empty() {
            return Ok("No saved sessions found.".to_string());
        }

        let mut lines = Vec::new();
        lines.push(format!("Found {} session(s) (showing up to {}):", sessions.len(), limit));
        for s in sessions.iter().take(limit) {
            let meta = crate::session::meta_path(&s.path);
            let title = if let Ok(content) = std::fs::read_to_string(meta) {
                serde_json::from_str::<crate::session::SessionMetadata>(&content)
                    .map(|m| m.title)
                    .unwrap_or_else(|_| s.name.clone())
            } else {
                s.name.clone()
            };
            lines.push(format!(
                "- ID: {} | Title: {:?} | Model: {} | Messages: {}",
                s.name, title, s.model, s.messages
            ));
        }
        Ok(lines.join("\n"))
    }
}

pub struct SearchSessionsTool;

#[async_trait]
impl Tool for SearchSessionsTool {
    fn name(&self) -> &'static str {
        "search_sessions"
    }

    fn description(&self) -> &'static str {
        "Searches previous sessions by keyword, title, or project. Fast: inspects metadata sidecars."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Keyword or topic to search for across previous session titles and metadata"
                },
                "max_results": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 30,
                    "description": "Maximum sessions to return (default: 5)"
                }
            },
            "required": ["query"]
        })
    }

    fn generate_preview(&self, args: &serde_json::Value) -> ToolPreview {
        let query = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
        ToolPreview {
            title: "Search Sessions".to_string(),
            details: vec![format!("Query: {}", query)],
            reason: None,
            expected_effect: None,
            command: None,
            diff_hunks: Vec::new(),
            is_mutation: false,
        }
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let query = args
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "Missing required parameter 'query'".to_string())?
            .trim()
            .to_ascii_lowercase();
        if query.is_empty() {
            return Err("Search query cannot be empty".to_string());
        }
        let max_results = args
            .get("max_results")
            .and_then(|v| v.as_u64())
            .unwrap_or(5) as usize;

        let sessions = crate::session::list();
        let mut matches = Vec::new();

        for s in sessions {
            let meta_file = crate::session::meta_path(&s.path);
            let (title, keywords, root) = if let Ok(content) = std::fs::read_to_string(&meta_file) {
                if let Ok(m) = serde_json::from_str::<crate::session::SessionMetadata>(&content) {
                    (m.title, m.keywords, m.project_root.map(|p| p.to_string_lossy().to_string()).unwrap_or_default())
                } else {
                    (s.name.clone(), Vec::new(), String::new())
                }
            } else {
                (s.name.clone(), Vec::new(), String::new())
            };

            let title_match = title.to_ascii_lowercase().contains(&query);
            let name_match = s.name.to_ascii_lowercase().contains(&query);
            let root_match = root.to_ascii_lowercase().contains(&query);
            let kw_match = keywords.iter().any(|k| k.to_ascii_lowercase().contains(&query));

            if title_match || name_match || root_match || kw_match {
                matches.push(format!(
                    "- ID: {} | Title: {:?} | Model: {} | Messages: {}\n  Keywords: {}",
                    s.name, title, s.model, s.messages, keywords.join(", ")
                ));
                if matches.len() >= max_results {
                    break;
                }
            }
        }

        if matches.is_empty() {
            Ok(format!("No previous sessions matched query: {query:?}"))
        } else {
            Ok(format!(
                "Found {} matching session(s):\n\n{}",
                matches.len(),
                matches.join("\n\n")
            ))
        }
    }
}

pub struct ReadSessionTool;

#[async_trait]
impl Tool for ReadSessionTool {
    fn name(&self) -> &'static str {
        "read_session"
    }

    fn description(&self) -> &'static str {
        "Inspects messages from a past session by session ID without resuming or overwriting current session state."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "session_id": {
                    "type": "string",
                    "description": "Session name or ID to read (e.g. from list_sessions or search_sessions)"
                },
                "start_message": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "0-based message index to start reading from (default: 0)"
                },
                "count": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 20,
                    "description": "Number of messages to inspect (default: 5)"
                }
            },
            "required": ["session_id"]
        })
    }

    fn generate_preview(&self, args: &serde_json::Value) -> ToolPreview {
        let id = args.get("session_id").and_then(|v| v.as_str()).unwrap_or("");
        ToolPreview {
            title: "Read Past Session".to_string(),
            details: vec![format!("Session ID: {}", id)],
            reason: None,
            expected_effect: None,
            command: None,
            diff_hunks: Vec::new(),
            is_mutation: false,
        }
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let session_id = args
            .get("session_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "Missing required parameter 'session_id'".to_string())?
            .trim();
        let start_msg = args
            .get("start_message")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as usize;
        let count = args
            .get("count")
            .and_then(|v| v.as_u64())
            .unwrap_or(5) as usize;

        let path = crate::session::default_session_path(session_id)
            .ok_or_else(|| format!("Could not resolve session path for '{session_id}'"))?;

        let snapshot = crate::session::load(&path)
            .ok_or_else(|| format!("Could not load session '{session_id}'"))?;

        let msgs = &snapshot.messages;
        if msgs.is_empty() {
            return Ok(format!("Session '{session_id}' contains no messages."));
        }

        let slice_start = start_msg.min(msgs.len().saturating_sub(1));
        let slice_end = (slice_start + count).min(msgs.len());

        let mut output = Vec::new();
        output.push(format!(
            "Session '{}' (total {} messages, showing #{}..#{}):",
            session_id, msgs.len(), slice_start, slice_end.saturating_sub(1)
        ));

        for (idx, msg) in msgs[slice_start..slice_end].iter().enumerate() {
            let actual_idx = slice_start + idx;
            if msg.role == "thought" {
                continue;
            }
            output.push(format!(
                "--- Message #{} [{} | role: {}] ---\n{}",
                actual_idx, msg.timestamp, msg.role, msg.content
            ));
        }

        Ok(output.join("\n\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[tokio::test]
    async fn search_and_read_history_tools_inspect_pre_compaction_turns() {
        let history = Arc::new(Mutex::new(vec![
            crate::app::ChatMessage {
                role: "user".into(),
                content: "Investigate Windows conhost font issues".into(),
                timestamp: "10:00:00".into(),
                attachments: Vec::new(),
            },
            crate::app::ChatMessage {
                role: "thought".into(),
                content: "Secret reasoning that should be ignored by retrieval".into(),
                timestamp: "10:00:01".into(),
                attachments: Vec::new(),
            },
            crate::app::ChatMessage {
                role: "model".into(),
                content: "Replacing extended unicode with ASCII glyphs".into(),
                timestamp: "10:00:05".into(),
                attachments: Vec::new(),
            },
        ]));

        let search_tool = SearchHistoryTool::new(history.clone());
        let res = search_tool
            .execute(serde_json::json!({"query": "conhost"}))
            .await
            .unwrap();
        assert!(res.contains("Msg #0"));
        assert!(res.contains("Windows conhost"));

        // Thought message is ignored
        let thought_res = search_tool
            .execute(serde_json::json!({"query": "Secret reasoning"}))
            .await
            .unwrap();
        assert!(thought_res.contains("No history messages matched query"));

        let read_tool = ReadHistoryTool::new(history);
        let read_res = read_tool
            .execute(serde_json::json!({"start_id": 0, "count": 3}))
            .await
            .unwrap();
        assert!(read_res.contains("Message #0"));
        assert!(read_res.contains("Message #2"));
        assert!(!read_res.contains("Secret reasoning"));
    }
}

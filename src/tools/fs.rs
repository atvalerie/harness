use async_trait::async_trait;
use serde_json::json;
use similar::{ChangeTag, TextDiff};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use super::{resolve_path, working_dir_path, DiffHunk, SharedWorkingDir, Tool, ToolPreview};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct ReadCacheKey {
    path: PathBuf,
    start_line: Option<usize>,
    max_lines: Option<usize>,
}

#[derive(Clone)]
struct ReadCacheEntry {
    mtime: Option<SystemTime>,
    len: u64,
    content: String,
}

#[derive(Clone, Default)]
pub struct ReadFileCache {
    entries: Arc<Mutex<HashMap<ReadCacheKey, ReadCacheEntry>>>,
}

impl ReadFileCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn invalidate(&self, path: &Path) {
        if let Ok(mut cache) = self.entries.lock() {
            cache.retain(|k, _| k.path != path);
        }
    }

    fn get(
        &self,
        path: &Path,
        start_line: Option<usize>,
        max_lines: Option<usize>,
        mtime: Option<SystemTime>,
        len: u64,
    ) -> Option<String> {
        let cache = self.entries.lock().ok()?;
        let key = ReadCacheKey {
            path: path.to_path_buf(),
            start_line,
            max_lines,
        };
        let entry = cache.get(&key)?;
        if entry.mtime == mtime && entry.len == len {
            Some(entry.content.clone())
        } else {
            None
        }
    }

    fn insert(
        &self,
        path: &Path,
        start_line: Option<usize>,
        max_lines: Option<usize>,
        mtime: Option<SystemTime>,
        len: u64,
        content: String,
    ) {
        if let Ok(mut cache) = self.entries.lock() {
            if cache.len() >= 256 {
                cache.clear();
            }
            let key = ReadCacheKey {
                path: path.to_path_buf(),
                start_line,
                max_lines,
            };
            cache.insert(
                key,
                ReadCacheEntry {
                    mtime,
                    len,
                    content,
                },
            );
        }
    }
}

pub struct ReadFileTool {
    cwd: SharedWorkingDir,
    cache: ReadFileCache,
}

impl ReadFileTool {
    pub fn new(cwd: SharedWorkingDir) -> Self {
        Self {
            cwd,
            cache: ReadFileCache::new(),
        }
    }

    pub fn cache(&self) -> ReadFileCache {
        self.cache.clone()
    }
}

#[async_trait]
impl Tool for ReadFileTool {
    fn name(&self) -> &'static str {
        "read_file"
    }

    fn description(&self) -> &'static str {
        "Reads a text file from the filesystem. Use start_line and max_lines for bounded source-code chunks instead of shell commands."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "The path of the file to read"
                },
                "start_line": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "Optional 1-based first line to return"
                },
                "max_lines": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "Optional maximum number of lines to return (defaults to 300 when start_line is used)"
                }
            },
            "required": ["path"]
        })
    }

    fn generate_preview(&self, args: &serde_json::Value) -> ToolPreview {
        let path = args
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or("<unknown>");
        let resolved = if path == "<unknown>" {
            path.to_string()
        } else {
            resolve_path(path, &working_dir_path(&self.cwd))
                .display()
                .to_string()
        };
        ToolPreview {
            title: "Read File".to_string(),
            details: vec![format!("Path: {}", resolved)],
            reason: None,
            expected_effect: None,
            command: None,
            diff_hunks: Vec::new(),
            is_mutation: false,
        }
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let path_str = args
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "Missing required parameter 'path'".to_string())?;

        let path = resolve_path(path_str, &working_dir_path(&self.cwd));
        if !path.exists() {
            return Err(format!("File does not exist: {}", path_str));
        }

        let metadata = fs::metadata(&path)
            .map_err(|e| format!("Failed to read file metadata '{}': {}", path.display(), e))?;
        let mtime = metadata.modified().ok();
        let len = metadata.len();

        let has_range = args.get("start_line").is_some() || args.get("max_lines").is_some();
        let (start_line_opt, max_lines_opt) = if has_range {
            let start = args
                .get("start_line")
                .and_then(|value| value.as_u64())
                .unwrap_or(1) as usize;
            if start == 0 {
                return Err("'start_line' must be at least 1".to_string());
            }
            let max = args
                .get("max_lines")
                .and_then(|value| value.as_u64())
                .unwrap_or(300) as usize;
            if max == 0 {
                return Err("'max_lines' must be at least 1".to_string());
            }
            (Some(start), Some(max))
        } else {
            (None, None)
        };

        if let Some(cached) = self
            .cache
            .get(&path, start_line_opt, max_lines_opt, mtime, len)
        {
            return Ok(cached);
        }

        let content = fs::read_to_string(&path)
            .map_err(|e| format!("Failed to read file '{}': {}", path.display(), e))?;

        let result = if let (Some(start_line), Some(max_lines)) = (start_line_opt, max_lines_opt) {
            let lines: Vec<&str> = content.lines().collect();
            let total = lines.len();
            let first = start_line.saturating_sub(1).min(total);
            let last = first.saturating_add(max_lines).min(total);
            let selected = lines[first..last].join("\n");
            let displayed_start = if total == 0 { 0 } else { first + 1 };
            let displayed_end = if last == 0 { 0 } else { last };
            format!(
                "[Lines {}-{} of {} in {}]\n{}",
                displayed_start,
                displayed_end,
                total,
                path.display(),
                selected
            )
        } else if content.len() > 16000 {
            let truncated = content.chars().take(16000).collect::<String>();
            format!(
                "File content (truncated to 16,000 chars):\n{}",
                truncated
            )
        } else {
            content
        };

        self.cache
            .insert(&path, start_line_opt, max_lines_opt, mtime, len, result.clone());

        Ok(result)
    }
}

pub struct ListDirectoryTool {
    cwd: SharedWorkingDir,
}

impl ListDirectoryTool {
    pub fn new(cwd: SharedWorkingDir) -> Self {
        Self { cwd }
    }
}

#[async_trait]
impl Tool for ListDirectoryTool {
    fn name(&self) -> &'static str {
        "list_directory"
    }

    fn description(&self) -> &'static str {
        "Lists files and directories without invoking a shell. Use depth and max_entries to keep repository exploration bounded."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Directory to list, relative to the current working directory (default: .)"
                },
                "depth": {
                    "type": "integer",
                    "minimum": 0,
                    "maximum": 3,
                    "description": "Additional directory levels to include (default: 0)"
                },
                "max_entries": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 500,
                    "description": "Maximum entries to return (default: 200)"
                },
                "include_hidden": {
                    "type": "boolean",
                    "description": "Include dot-prefixed entries (default: false)"
                }
            }
        })
    }

    fn generate_preview(&self, args: &serde_json::Value) -> ToolPreview {
        let path = args
            .get("path")
            .and_then(|value| value.as_str())
            .unwrap_or(".");
        ToolPreview {
            title: "List Directory".to_string(),
            details: vec![format!(
                "Path: {}",
                resolve_path(path, &working_dir_path(&self.cwd)).display()
            )],
            reason: None,
            expected_effect: None,
            command: None,
            diff_hunks: Vec::new(),
            is_mutation: false,
        }
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let path_str = args
            .get("path")
            .and_then(|value| value.as_str())
            .unwrap_or(".");
        let path = resolve_path(path_str, &working_dir_path(&self.cwd));
        if !path.is_dir() {
            return Err(format!("Directory does not exist: {}", path.display()));
        }
        let depth = args
            .get("depth")
            .and_then(|value| value.as_u64())
            .unwrap_or(0)
            .min(3) as usize;
        let max_entries = args
            .get("max_entries")
            .and_then(|value| value.as_u64())
            .unwrap_or(200)
            .clamp(1, 500) as usize;
        let include_hidden = args
            .get("include_hidden")
            .and_then(|value| value.as_bool())
            .unwrap_or(false);

        let mut entries = Vec::new();
        collect_directory_entries(
            &path,
            &path,
            depth,
            max_entries,
            include_hidden,
            &mut entries,
        )
        .map_err(|error| format!("Failed to list '{}': {}", path.display(), error))?;
        let mut output = format!(
            "Directory listing for {} ({} entr{}):\n",
            path.display(),
            entries.len(),
            if entries.len() == 1 { "y" } else { "ies" }
        );
        output.push_str(&entries.join("\n"));
        if entries.len() == max_entries {
            output.push_str("\n[Directory output limited by max_entries]");
        }
        Ok(output)
    }
}

fn collect_directory_entries(
    root: &Path,
    current: &Path,
    depth: usize,
    max_entries: usize,
    include_hidden: bool,
    output: &mut Vec<String>,
) -> std::io::Result<()> {
    let mut entries = fs::read_dir(current)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(|entry| entry.file_name().to_ascii_lowercase());
    for entry in entries {
        if output.len() >= max_entries {
            break;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !include_hidden && name.starts_with('.') {
            continue;
        }
        let file_type = entry.file_type()?;
        let relative = entry
            .path()
            .strip_prefix(root)
            .unwrap_or(entry.path().as_path())
            .display()
            .to_string();
        let (kind, suffix) = if file_type.is_dir() {
            ("dir", "/")
        } else if file_type.is_symlink() {
            ("link", "")
        } else {
            ("file", "")
        };
        output.push(format!("[{}] {}{}", kind, relative, suffix));
        if file_type.is_dir() && depth > 0 {
            collect_directory_entries(
                root,
                &entry.path(),
                depth - 1,
                max_entries,
                include_hidden,
                output,
            )?;
        }
    }
    Ok(())
}

pub struct StatPathTool {
    cwd: SharedWorkingDir,
}

impl StatPathTool {
    pub fn new(cwd: SharedWorkingDir) -> Self {
        Self { cwd }
    }
}

#[async_trait]
impl Tool for StatPathTool {
    fn name(&self) -> &'static str {
        "stat_path"
    }

    fn description(&self) -> &'static str {
        "Reports bounded metadata for a file, directory, or symlink without invoking a shell."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "File or directory path relative to the current working directory"
                }
            },
            "required": ["path"]
        })
    }

    fn generate_preview(&self, args: &serde_json::Value) -> ToolPreview {
        let path = args
            .get("path")
            .and_then(|value| value.as_str())
            .unwrap_or("<unknown>");
        let resolved = if path == "<unknown>" {
            path.to_string()
        } else {
            resolve_path(path, &working_dir_path(&self.cwd))
                .display()
                .to_string()
        };
        ToolPreview {
            title: "Stat Path".to_string(),
            details: vec![format!("Path: {}", resolved)],
            reason: None,
            expected_effect: None,
            command: None,
            diff_hunks: Vec::new(),
            is_mutation: false,
        }
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let path_str = args
            .get("path")
            .and_then(|value| value.as_str())
            .ok_or_else(|| "Missing required parameter 'path'".to_string())?;
        let path = resolve_path(path_str, &working_dir_path(&self.cwd));
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| format!("Failed to inspect '{}': {}", path.display(), error))?;
        let kind = if metadata.file_type().is_symlink() {
            "symlink"
        } else if metadata.is_dir() {
            "directory"
        } else if metadata.is_file() {
            "file"
        } else {
            "other"
        };
        Ok(format!(
            "Path: {}\nType: {}\nSize: {} bytes\nReadonly: {}",
            path.display(),
            kind,
            metadata.len(),
            metadata.permissions().readonly()
        ))
    }
}

pub struct CreateDirectoryTool {
    cwd: SharedWorkingDir,
}

impl CreateDirectoryTool {
    pub fn new(cwd: SharedWorkingDir) -> Self {
        Self { cwd }
    }
}

#[async_trait]
impl Tool for CreateDirectoryTool {
    fn name(&self) -> &'static str {
        "create_directory"
    }

    fn description(&self) -> &'static str {
        "Creates a directory and its missing parents after approval."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Directory path to create relative to the current working directory"
                }
            },
            "required": ["path"]
        })
    }

    fn generate_preview(&self, args: &serde_json::Value) -> ToolPreview {
        let path = args
            .get("path")
            .and_then(|value| value.as_str())
            .unwrap_or("<unknown>");
        let resolved = resolve_path(path, &working_dir_path(&self.cwd));
        ToolPreview {
            title: "Create Directory".to_string(),
            details: vec![
                format!("Target: {}", resolved.display()),
                "Action: Create directory and missing parents".to_string(),
            ],
            reason: None,
            expected_effect: None,
            command: None,
            diff_hunks: Vec::new(),
            is_mutation: true,
        }
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let path_str = args
            .get("path")
            .and_then(|value| value.as_str())
            .ok_or_else(|| "Missing required parameter 'path'".to_string())?;
        let path = resolve_path(path_str, &working_dir_path(&self.cwd));
        fs::create_dir_all(&path)
            .map_err(|error| format!("Failed to create '{}': {}", path.display(), error))?;
        Ok(format!("Created directory {}", path.display()))
    }
}

pub struct MovePathTool {
    cwd: SharedWorkingDir,
}

impl MovePathTool {
    pub fn new(cwd: SharedWorkingDir) -> Self {
        Self { cwd }
    }
}

#[async_trait]
impl Tool for MovePathTool {
    fn name(&self) -> &'static str {
        "move_path"
    }

    fn description(&self) -> &'static str {
        "Moves or renames a file or directory after approval. It never overwrites an existing destination."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "source": {"type": "string", "description": "Existing file or directory to move"},
                "destination": {"type": "string", "description": "New path; existing destinations are rejected"}
            },
            "required": ["source", "destination"]
        })
    }

    fn generate_preview(&self, args: &serde_json::Value) -> ToolPreview {
        let source = args
            .get("source")
            .and_then(|value| value.as_str())
            .unwrap_or("<unknown>");
        let destination = args
            .get("destination")
            .and_then(|value| value.as_str())
            .unwrap_or("<unknown>");
        let cwd = working_dir_path(&self.cwd);
        ToolPreview {
            title: "Move Path".to_string(),
            details: vec![
                format!("Source: {}", resolve_path(source, &cwd).display()),
                format!("Destination: {}", resolve_path(destination, &cwd).display()),
                "Action: Move or rename without overwriting".to_string(),
            ],
            reason: None,
            expected_effect: None,
            command: None,
            diff_hunks: Vec::new(),
            is_mutation: true,
        }
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let source_str = args
            .get("source")
            .and_then(|value| value.as_str())
            .ok_or_else(|| "Missing required parameter 'source'".to_string())?;
        let destination_str = args
            .get("destination")
            .and_then(|value| value.as_str())
            .ok_or_else(|| "Missing required parameter 'destination'".to_string())?;
        let cwd = working_dir_path(&self.cwd);
        let source = resolve_path(source_str, &cwd);
        let destination = resolve_path(destination_str, &cwd);
        if !source.exists() {
            return Err(format!("Source does not exist: {}", source.display()));
        }
        if destination.exists() {
            return Err(format!(
                "Destination already exists: {}",
                destination.display()
            ));
        }
        fs::rename(&source, &destination).map_err(|error| {
            format!(
                "Failed to move '{}' to '{}': {}",
                source.display(),
                destination.display(),
                error
            )
        })?;
        Ok(format!(
            "Moved {} to {}",
            source.display(),
            destination.display()
        ))
    }
}

pub struct DeletePathTool {
    cwd: SharedWorkingDir,
}

impl DeletePathTool {
    pub fn new(cwd: SharedWorkingDir) -> Self {
        Self { cwd }
    }
}

#[async_trait]
impl Tool for DeletePathTool {
    fn name(&self) -> &'static str {
        "delete_path"
    }

    fn description(&self) -> &'static str {
        "Deletes a file or an explicitly approved directory. Directory deletion requires recursive=true and cannot be undone."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "File or directory to delete"},
                "recursive": {"type": "boolean", "description": "Required for directory deletion; defaults to false"}
            },
            "required": ["path"]
        })
    }

    fn generate_preview(&self, args: &serde_json::Value) -> ToolPreview {
        let path = args
            .get("path")
            .and_then(|value| value.as_str())
            .unwrap_or("<unknown>");
        let recursive = args
            .get("recursive")
            .and_then(|value| value.as_bool())
            .unwrap_or(false);
        let resolved = resolve_path(path, &working_dir_path(&self.cwd));
        ToolPreview {
            title: "Delete Path".to_string(),
            details: vec![
                format!("Target: {}", resolved.display()),
                format!("Recursive: {}", recursive),
                "Risk: Permanent deletion; there is no undo".to_string(),
            ],
            reason: None,
            expected_effect: None,
            command: None,
            diff_hunks: Vec::new(),
            is_mutation: true,
        }
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let path_str = args
            .get("path")
            .and_then(|value| value.as_str())
            .ok_or_else(|| "Missing required parameter 'path'".to_string())?;
        let recursive = args
            .get("recursive")
            .and_then(|value| value.as_bool())
            .unwrap_or(false);
        let path = resolve_path(path_str, &working_dir_path(&self.cwd));
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| format!("Failed to inspect '{}': {}", path.display(), error))?;
        let cwd = resolve_path(".", &working_dir_path(&self.cwd));
        if path == cwd {
            return Err("Refusing to delete the active working directory".to_string());
        }
        if metadata.is_dir() {
            if !recursive {
                return Err("Directory deletion requires recursive=true".to_string());
            }
            fs::remove_dir_all(&path)
                .map_err(|error| format!("Failed to delete '{}': {}", path.display(), error))?;
        } else {
            fs::remove_file(&path)
                .map_err(|error| format!("Failed to delete '{}': {}", path.display(), error))?;
        }
        Ok(format!("Deleted {}", path.display()))
    }
}

pub struct WriteFileTool {
    cwd: SharedWorkingDir,
    read_cache: Option<ReadFileCache>,
}

impl WriteFileTool {
    pub fn new(cwd: SharedWorkingDir, read_cache: ReadFileCache) -> Self {
        Self {
            cwd,
            read_cache: Some(read_cache),
        }
    }
}

#[async_trait]
impl Tool for WriteFileTool {
    fn name(&self) -> &'static str {
        "write_file"
    }

    fn description(&self) -> &'static str {
        "Creates or overwrites a file with full content. Prefer edit_file for targeted changes to existing files. Generates a diff preview; execution follows the configured permission policy."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the target file to write"
                },
                "content": {
                    "type": "string",
                    "description": "The full text content to write into the file"
                }
            },
            "required": ["path", "content"]
        })
    }

    fn generate_preview(&self, args: &serde_json::Value) -> ToolPreview {
        let path_str = args
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or("<unknown>");
        let new_content = args.get("content").and_then(|v| v.as_str()).unwrap_or("");

        let path = resolve_path(path_str, &working_dir_path(&self.cwd));
        let old_content = if path.exists() {
            fs::read_to_string(&path).unwrap_or_default()
        } else {
            String::new()
        };

        let is_new_file = !path.exists();
        let diff = TextDiff::from_lines(old_content.as_str(), new_content);
        let mut diff_hunks = Vec::new();

        for group in diff.grouped_ops(3) {
            for op in group {
                for change in diff.iter_changes(&op) {
                    let (tag, line) = match change.tag() {
                        ChangeTag::Equal => (" ", change.to_string()),
                        ChangeTag::Delete => ("-", change.to_string()),
                        ChangeTag::Insert => ("+", change.to_string()),
                    };

                    diff_hunks.push(DiffHunk {
                        tag: tag.to_string(),
                        line: line.trim_end_matches(&['\r', '\n'][..]).to_string(),
                        old_line_no: change.old_index(),
                        new_line_no: change.new_index(),
                    });
                }
            }
        }

        let summary = if is_new_file {
            format!(
                "Action: Create new file with {} lines",
                new_content.lines().count()
            )
        } else {
            format!(
                "Action: Modify existing file ({} -> {} lines)",
                old_content.lines().count(),
                new_content.lines().count()
            )
        };

        ToolPreview {
            title: "Write File (Diff Review)".to_string(),
            details: vec![format!("Target: {}", path.display()), summary],
            reason: None,
            expected_effect: None,
            command: None,
            diff_hunks,
            is_mutation: true,
        }
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let path_str = args
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "Missing required parameter 'path'".to_string())?;

        let content = args
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "Missing required parameter 'content'".to_string())?;

        let path = resolve_path(path_str, &working_dir_path(&self.cwd));
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)
                    .map_err(|e| format!("Failed to create parent directory: {}", e))?;
            }
        }

        fs::write(&path, content)
            .map_err(|e| format!("Failed to write file '{}': {}", path.display(), e))?;

        if let Some(cache) = &self.read_cache {
            cache.invalidate(&path);
        }

        Ok(format!(
            "Successfully wrote {} bytes to {}",
            content.len(),
            path.display()
        ))
    }
}

pub struct EditFileTool {
    cwd: SharedWorkingDir,
    read_cache: Option<ReadFileCache>,
}

impl EditFileTool {
    pub fn new(cwd: SharedWorkingDir, read_cache: ReadFileCache) -> Self {
        Self {
            cwd,
            read_cache: Some(read_cache),
        }
    }
}

#[async_trait]
impl Tool for EditFileTool {
    fn name(&self) -> &'static str {
        "edit_file"
    }

    fn description(&self) -> &'static str {
        "Applies a precise patch to an existing text file by replacing an exact old_string with new_string. Prefer this over rewriting a whole file. Generates a diff preview; execution follows the configured permission policy."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "path": {"type":"string", "description":"Path to the existing file"},
                "old_string": {"type":"string", "description":"Exact text to replace"},
                "new_string": {"type":"string", "description":"Replacement text"},
                "replace_all": {"type":"boolean", "description":"Replace every occurrence; defaults to false"}
            },
            "required":["path","old_string","new_string"]
        })
    }

    fn generate_preview(&self, args: &serde_json::Value) -> ToolPreview {
        let path_str = args
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or("<unknown>");
        let old_string = args
            .get("old_string")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let new_string = args
            .get("new_string")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let replace_all = args
            .get("replace_all")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let path = resolve_path(path_str, &working_dir_path(&self.cwd));
        let source = fs::read_to_string(&path).unwrap_or_default();
        let (updated, occurrences) = apply_exact_edit(&source, old_string, new_string, replace_all)
            .unwrap_or_else(|_| (source.clone(), 0));
        let diff = TextDiff::from_lines(&source, &updated);
        let mut diff_hunks = Vec::new();
        for group in diff.grouped_ops(3) {
            for op in group {
                for change in diff.iter_changes(&op) {
                    let (tag, line) = match change.tag() {
                        ChangeTag::Equal => (" ", change.to_string()),
                        ChangeTag::Delete => ("-", change.to_string()),
                        ChangeTag::Insert => ("+", change.to_string()),
                    };
                    diff_hunks.push(DiffHunk {
                        tag: tag.to_string(),
                        line: line.trim_end_matches(&['\r', '\n'][..]).to_string(),
                        old_line_no: change.old_index(),
                        new_line_no: change.new_index(),
                    });
                }
            }
        }
        ToolPreview {
            title: "Edit File (Patch Review)".to_string(),
            details: vec![
                format!("Target: {}", path.display()),
                format!("Exact replacements: {}", occurrences),
                format!("Replace all: {}", replace_all),
            ],
            reason: None,
            expected_effect: None,
            command: None,
            diff_hunks,
            is_mutation: true,
        }
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let path_str = args
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "Missing required parameter 'path'".to_string())?;
        let old_string = args
            .get("old_string")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "Missing required parameter 'old_string'".to_string())?;
        let new_string = args
            .get("new_string")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "Missing required parameter 'new_string'".to_string())?;
        if old_string.is_empty() {
            return Err(
                "'old_string' cannot be empty; use write_file to create or replace a whole file"
                    .to_string(),
            );
        }
        let replace_all = args
            .get("replace_all")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let path = resolve_path(path_str, &working_dir_path(&self.cwd));
        let source = fs::read_to_string(&path)
            .map_err(|e| format!("Failed to read file '{}': {}", path.display(), e))?;
        let (updated, occurrences) =
            apply_exact_edit(&source, old_string, new_string, replace_all)?;
        fs::write(&path, updated)
            .map_err(|e| format!("Failed to write file '{}': {}", path.display(), e))?;
        if let Some(cache) = &self.read_cache {
            cache.invalidate(&path);
        }
        Ok(format!(
            "Applied {} exact replacement{} to {}",
            occurrences,
            if occurrences == 1 { "" } else { "s" },
            path.display()
        ))
    }
}

fn apply_exact_edit(
    source: &str,
    old_string: &str,
    new_string: &str,
    replace_all: bool,
) -> Result<(String, usize), String> {
    if old_string.is_empty() {
        return Err("'old_string' cannot be empty".to_string());
    }

    // Direct match first.
    let occurrences = source.matches(old_string).count();
    if occurrences > 0 {
        if occurrences > 1 && !replace_all {
            return Err(format!(
                "old_string matched {} places; provide more context or set replace_all=true",
                occurrences
            ));
        }
        let updated = if replace_all {
            source.replace(old_string, new_string)
        } else {
            source.replacen(old_string, new_string, 1)
        };
        return Ok((updated, if replace_all { occurrences } else { 1 }));
    }

    // Fallback: Line-ending normalization (CRLF <-> LF) for cross-platform robustness.
    let source_has_crlf = source.contains("\r\n");
    let norm_old = if source_has_crlf {
        old_string.replace("\r\n", "\n").replace('\n', "\r\n")
    } else {
        old_string.replace("\r\n", "\n")
    };
    let norm_new = if source_has_crlf {
        new_string.replace("\r\n", "\n").replace('\n', "\r\n")
    } else {
        new_string.replace("\r\n", "\n")
    };

    let occurrences = source.matches(&norm_old).count();
    if occurrences == 0 {
        return Err("old_string was not found exactly in the target file".to_string());
    }
    if occurrences > 1 && !replace_all {
        return Err(format!(
            "old_string matched {} places; provide more context or set replace_all=true",
            occurrences
        ));
    }
    let updated = if replace_all {
        source.replace(&norm_old, &norm_new)
    } else {
        source.replacen(&norm_old, &norm_new, 1)
    };
    Ok((updated, if replace_all { occurrences } else { 1 }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};

    #[tokio::test]
    async fn read_file_caches_exact_reads_and_invalidates_on_modification() {
        let temp_dir = std::env::temp_dir().join(format!("holiday_read_cache_test_{}", rand::random::<u64>()));
        std::fs::create_dir_all(&temp_dir).unwrap();
        let test_file = temp_dir.join("sample.txt");
        std::fs::write(&test_file, "line1\nline2\nline3\nline4\n").unwrap();

        let cwd = Arc::new(Mutex::new(temp_dir.clone()));
        let tool = ReadFileTool::new(cwd);

        let args = serde_json::json!({
            "path": "sample.txt",
            "start_line": 1,
            "max_lines": 2
        });

        let res1 = tool.execute(args.clone()).await.unwrap();
        assert!(res1.contains("line1\nline2"));

        // Second call hits cache
        let res2 = tool.execute(args.clone()).await.unwrap();
        assert_eq!(res1, res2);

        // Modify file
        // Sleep slightly to guarantee different mtime if filesystem resolution requires it
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&test_file, "modified1\nmodified2\nmodified3\n").unwrap();

        let res3 = tool.execute(args).await.unwrap();
        assert!(res3.contains("modified1\nmodified2"));

        let _ = std::fs::remove_dir_all(temp_dir);
    }

    #[tokio::test]
    async fn edit_and_write_file_invalidate_read_cache() {
        let temp_dir = std::env::temp_dir().join(format!("holiday_edit_inval_test_{}", rand::random::<u64>()));
        std::fs::create_dir_all(&temp_dir).unwrap();
        let test_file = temp_dir.join("sample.txt");
        std::fs::write(&test_file, "original text\n").unwrap();

        let cwd = Arc::new(Mutex::new(temp_dir.clone()));
        let read_tool = ReadFileTool::new(cwd.clone());
        let cache = read_tool.cache();
        let edit_tool = EditFileTool::new(cwd.clone(), cache.clone());
        let write_tool = WriteFileTool::new(cwd.clone(), cache.clone());

        let read_args = serde_json::json!({"path": "sample.txt"});
        let res1 = read_tool.execute(read_args.clone()).await.unwrap();
        assert_eq!(res1, "original text\n");

        // Edit the file through EditFileTool
        edit_tool
            .execute(serde_json::json!({
                "path": "sample.txt",
                "old_string": "original",
                "new_string": "modified"
            }))
            .await
            .unwrap();

        // Read tool should immediately see the modification without stale cache
        let res2 = read_tool.execute(read_args.clone()).await.unwrap();
        assert_eq!(res2, "modified text\n");

        // Write to file through WriteFileTool
        write_tool
            .execute(serde_json::json!({
                "path": "sample.txt",
                "content": "overwritten\n"
            }))
            .await
            .unwrap();

        let res3 = read_tool.execute(read_args).await.unwrap();
        assert_eq!(res3, "overwritten\n");

        let _ = std::fs::remove_dir_all(temp_dir);
    }

    #[test]
    fn precise_edit_rejects_ambiguous_match() {
        assert!(apply_exact_edit("a\na\n", "a", "b", false).is_err());
        let (updated, count) = apply_exact_edit("a\na\n", "a", "b", true).unwrap();
        assert_eq!(updated, "b\nb\n");
        assert_eq!(count, 2);
    }

    #[test]
    fn paths_are_anchored_and_lexically_normalized() {
        let cwd = PathBuf::from("projects")
            .join("harness")
            .join("target")
            .join("debug");
        assert_eq!(
            resolve_path("../../src/main.rs", &cwd),
            PathBuf::from("projects")
                .join("harness")
                .join("src")
                .join("main.rs")
        );
        assert_eq!(
            resolve_path("./src/../Cargo.toml", Path::new("projects/harness")),
            PathBuf::from("projects").join("harness").join("Cargo.toml")
        );
    }

    #[test]
    fn unresolved_paths_do_not_search_parent_directories() {
        let cwd = PathBuf::from("projects")
            .join("harness")
            .join("target")
            .join("debug");
        assert_eq!(resolve_path("missing.txt", &cwd), cwd.join("missing.txt"));
    }
}

use async_trait::async_trait;
use serde_json::json;
use similar::{ChangeTag, TextDiff};
use std::fs;
use std::path::PathBuf;

use super::{DiffHunk, Tool, ToolPreview};

fn resolve_path(path_str: &str) -> PathBuf {
    let initial = PathBuf::from(path_str);
    if initial.exists() || !initial.is_relative() {
        return initial;
    }

    if let Ok(cwd) = std::env::current_dir() {
        let mut curr = cwd;
        while let Some(parent) = curr.parent() {
            let candidate = parent.join(path_str);
            if candidate.exists() {
                return candidate;
            }
            curr = parent.to_path_buf();
        }
    }
    initial
}

pub struct ReadFileTool;

#[async_trait]
impl Tool for ReadFileTool {
    fn name(&self) -> &'static str {
        "read_file"
    }

    fn description(&self) -> &'static str {
        "Reads the contents of a file from the filesystem."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "The path of the file to read"
                }
            },
            "required": ["path"]
        })
    }

    fn generate_preview(&self, args: &serde_json::Value) -> ToolPreview {
        let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("<unknown>");
        ToolPreview {
            title: "Read File".to_string(),
            details: vec![format!("Path: {}", path)],
            diff_hunks: Vec::new(),
            is_mutation: false,
        }
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let path_str = args
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "Missing required parameter 'path'".to_string())?;

        let path = resolve_path(path_str);
        if !path.exists() {
            return Err(format!("File does not exist: {}", path_str));
        }

        let content = fs::read_to_string(&path)
            .map_err(|e| format!("Failed to read file '{}': {}", path.display(), e))?;

        if content.len() > 16000 {
            let truncated = content.chars().take(16000).collect::<String>();
            Ok(format!("File content (truncated to 16,000 chars):\n{}", truncated))
        } else {
            Ok(content)
        }
    }
}

pub struct WriteFileTool;

#[async_trait]
impl Tool for WriteFileTool {
    fn name(&self) -> &'static str {
        "write_file"
    }

    fn description(&self) -> &'static str {
        "Writes or overwrites content to a specified file. Generates unified diff for review before execution."
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
        let path_str = args.get("path").and_then(|v| v.as_str()).unwrap_or("<unknown>");
        let new_content = args.get("content").and_then(|v| v.as_str()).unwrap_or("");

        let path = resolve_path(path_str);
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
            format!("Action: Create new file with {} lines", new_content.lines().count())
        } else {
            format!("Action: Modify existing file ({} -> {} lines)", old_content.lines().count(), new_content.lines().count())
        };

        ToolPreview {
            title: "Write File (Diff Review)".to_string(),
            details: vec![
                format!("Target: {}", path.display()),
                summary,
            ],
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

        let path = resolve_path(path_str);
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)
                    .map_err(|e| format!("Failed to create parent directory: {}", e))?;
            }
        }

        fs::write(&path, content)
            .map_err(|e| format!("Failed to write file '{}': {}", path.display(), e))?;

        Ok(format!("Successfully wrote {} bytes to {}", content.len(), path.display()))
    }
}

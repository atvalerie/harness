use async_trait::async_trait;
use serde_json::json;
use similar::{ChangeTag, TextDiff};
use std::fs;
use std::path::PathBuf;

use super::{working_dir_path, DiffHunk, SharedWorkingDir, Tool, ToolPreview};

fn resolve_path(path_str: &str, cwd: &std::path::Path) -> PathBuf {
    let initial = PathBuf::from(path_str);
    if !initial.is_relative() {
        return initial;
    }

    let direct = cwd.join(path_str);
    if direct.exists() {
        return direct;
    }
    let mut curr = cwd.to_path_buf();
    while let Some(parent) = curr.parent() {
        let candidate = parent.join(path_str);
        if candidate.exists() {
            return candidate;
        }
        curr = parent.to_path_buf();
    }
    // Keep relative paths anchored to the session's working directory even
    // when the target does not exist yet (important for write_file).
    direct
}

pub struct ReadFileTool {
    cwd: SharedWorkingDir,
}

impl ReadFileTool {
    pub fn new(cwd: SharedWorkingDir) -> Self {
        Self { cwd }
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

        let path = resolve_path(path_str, &working_dir_path(&self.cwd));
        if !path.exists() {
            return Err(format!("File does not exist: {}", path_str));
        }

        let content = fs::read_to_string(&path)
            .map_err(|e| format!("Failed to read file '{}': {}", path.display(), e))?;

        if args.get("start_line").is_some() || args.get("max_lines").is_some() {
            let start_line = args
                .get("start_line")
                .and_then(|value| value.as_u64())
                .unwrap_or(1) as usize;
            if start_line == 0 {
                return Err("'start_line' must be at least 1".to_string());
            }
            let max_lines = args
                .get("max_lines")
                .and_then(|value| value.as_u64())
                .unwrap_or(300) as usize;
            if max_lines == 0 {
                return Err("'max_lines' must be at least 1".to_string());
            }
            let lines: Vec<&str> = content.lines().collect();
            let total = lines.len();
            let first = start_line.saturating_sub(1).min(total);
            let last = first.saturating_add(max_lines).min(total);
            let selected = lines[first..last].join("\n");
            let displayed_start = if total == 0 { 0 } else { first + 1 };
            let displayed_end = if last == 0 { 0 } else { last };
            return Ok(format!(
                "[Lines {}-{} of {} in {}]\n{}",
                displayed_start,
                displayed_end,
                total,
                path.display(),
                selected
            ));
        }

        if content.len() > 16000 {
            let truncated = content.chars().take(16000).collect::<String>();
            Ok(format!(
                "File content (truncated to 16,000 chars):\n{}",
                truncated
            ))
        } else {
            Ok(content)
        }
    }
}

pub struct WriteFileTool {
    cwd: SharedWorkingDir,
}

impl WriteFileTool {
    pub fn new(cwd: SharedWorkingDir) -> Self {
        Self { cwd }
    }
}

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

        Ok(format!(
            "Successfully wrote {} bytes to {}",
            content.len(),
            path.display()
        ))
    }
}

pub struct EditFileTool {
    cwd: SharedWorkingDir,
}

impl EditFileTool {
    pub fn new(cwd: SharedWorkingDir) -> Self {
        Self { cwd }
    }
}

#[async_trait]
impl Tool for EditFileTool {
    fn name(&self) -> &'static str {
        "edit_file"
    }

    fn description(&self) -> &'static str {
        "Applies a precise patch to an existing text file by replacing an exact old_string with new_string. Prefer this over rewriting a whole file. Generates a diff for review."
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
    let occurrences = source.matches(old_string).count();
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
        source.replace(old_string, new_string)
    } else {
        source.replacen(old_string, new_string, 1)
    };
    Ok((updated, if replace_all { occurrences } else { 1 }))
}

#[cfg(test)]
mod edit_tests {
    use super::apply_exact_edit;

    #[test]
    fn precise_edit_rejects_ambiguous_match() {
        assert!(apply_exact_edit("a\na\n", "a", "b", false).is_err());
        let (updated, count) = apply_exact_edit("a\na\n", "a", "b", true).unwrap();
        assert_eq!(updated, "b\nb\n");
        assert_eq!(count, 2);
    }
}

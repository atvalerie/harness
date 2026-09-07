use async_trait::async_trait;
use serde_json::json;
use std::process::Stdio;
use tokio::process::Command;

use super::{working_dir_path, SharedWorkingDir, Tool, ToolPreview};

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
        ToolPreview {
            title: "Search Files (ripgrep)".to_string(),
            details: vec![format!("Query: {}", query), format!("Path: {}", path)],
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
        command
            .arg("--")
            .arg(query)
            .arg(path)
            .current_dir(working_dir_path(&self.cwd))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let output = command.output().await.map_err(|error| {
            format!(
                "Could not run ripgrep: {}. Install 'rg' or use run_command as a fallback.",
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

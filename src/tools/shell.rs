use async_trait::async_trait;
use serde_json::json;
use std::process::Stdio;
use tokio::process::Command;

use super::{Tool, ToolPreview};

pub struct RunCommandTool;

#[async_trait]
impl Tool for RunCommandTool {
    fn name(&self) -> &'static str {
        "run_command"
    }

    fn description(&self) -> &'static str {
        "Executes a host shell command in the current working directory."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The shell command to execute"
                }
            },
            "required": ["command"]
        })
    }

    fn generate_preview(&self, args: &serde_json::Value) -> ToolPreview {
        let command = args.get("command").and_then(|v| v.as_str()).unwrap_or("<missing command>");
        let cwd = std::env::current_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| ".".to_string());

        ToolPreview {
            title: "Run Shell Command".to_string(),
            details: vec![
                format!("Command: {}", command),
                format!("Working Dir: {}", cwd),
                "Risk: Executes arbitrary code on host system".to_string(),
            ],
            diff_hunks: Vec::new(),
            is_mutation: true,
        }
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let command_str = args
            .get("command")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "Missing required parameter 'command'".to_string())?;

        #[cfg(target_os = "windows")]
        let mut cmd = {
            let mut c = Command::new("cmd");
            c.args(["/C", command_str]);
            c
        };

        #[cfg(not(target_os = "windows"))]
        let mut cmd = {
            let mut c = Command::new("sh");
            c.args(["-c", command_str]);
            c
        };

        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

        let output = cmd
            .output()
            .await
            .map_err(|e| format!("Failed to spawn command '{}': {}", command_str, e))?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let code = output.status.code().unwrap_or(-1);

        let mut out = format!("Exit Code: {}\n", code);
        if !stdout.is_empty() {
            out.push_str("=== STDOUT ===\n");
            out.push_str(&stdout);
            if !stdout.ends_with('\n') {
                out.push('\n');
            }
        }
        if !stderr.is_empty() {
            out.push_str("=== STDERR ===\n");
            out.push_str(&stderr);
            if !stderr.ends_with('\n') {
                out.push('\n');
            }
        }

        if out.len() > 8000 {
            let truncated = out.chars().take(8000).collect::<String>();
            Ok(format!("{}\n[Output truncated at 8,000 characters]", truncated))
        } else {
            Ok(out)
        }
    }
}

use async_trait::async_trait;
use serde_json::json;
use std::path::PathBuf;
use std::process::Stdio;
use tokio::process::Command;

use super::{working_dir_path, SharedWorkingDir, Tool, ToolPreview};

const CWD_MARKER: &str = "__GEMINI_HARNESS_CWD__";

pub struct RunCommandTool {
    cwd: SharedWorkingDir,
}

impl RunCommandTool {
    pub fn new(cwd: SharedWorkingDir) -> Self {
        Self { cwd }
    }
}

#[async_trait]
impl Tool for RunCommandTool {
    fn name(&self) -> &'static str {
        "run_command"
    }

    fn description(&self) -> &'static str {
        "Executes a host shell command in the current working directory. Directory changes persist for later tools in this session. On Windows, shell may be auto, powershell, or cmd; auto prefers PowerShell."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The shell command to execute"
                },
                "shell": {
                    "type": "string",
                    "enum": ["auto", "powershell", "cmd", "sh"],
                    "description": "Optional shell backend. Use auto unless the command requires a specific shell."
                }
            },
            "required": ["command"]
        })
    }

    fn generate_preview(&self, args: &serde_json::Value) -> ToolPreview {
        let command = args
            .get("command")
            .and_then(|v| v.as_str())
            .unwrap_or("<missing command>");
        let shell = args.get("shell").and_then(|v| v.as_str()).unwrap_or("auto");
        let cwd = working_dir_path(&self.cwd).display().to_string();

        ToolPreview {
            title: "Run Shell Command".to_string(),
            details: vec![
                format!("Command: {}", command),
                format!("Shell: {}", shell),
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

        let starting_dir = working_dir_path(&self.cwd);
        let requested_shell = args.get("shell").and_then(|v| v.as_str()).unwrap_or("auto");

        #[cfg(target_os = "windows")]
        let mut cmd = {
            let selected_shell = select_windows_shell(requested_shell, command_str);
            let mut c = if selected_shell == "cmd" {
                Command::new("cmd")
            } else {
                Command::new("powershell.exe")
            };
            if selected_shell == "cmd" {
                let wrapped = format!("{} & echo. & echo {}!CD!", command_str, CWD_MARKER);
                c.args(["/V:ON", "/C", &wrapped]);
            } else {
                let wrapped = format!(
                    "{}; Write-Output (\"{}\" + (Get-Location).Path)",
                    command_str, CWD_MARKER
                );
                c.args([
                    "-NoLogo",
                    "-NoProfile",
                    "-NonInteractive",
                    "-ExecutionPolicy",
                    "Bypass",
                    "-Command",
                    &wrapped,
                ]);
            }
            c
        };

        #[cfg(not(target_os = "windows"))]
        let mut cmd = {
            let mut c = Command::new("sh");
            let wrapped = format!("{}; printf '\\n{}%s\\n' \"$PWD\"", command_str, CWD_MARKER);
            c.args(["-c", &wrapped]);
            c
        };

        cmd.current_dir(&starting_dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let output = cmd
            .output()
            .await
            .map_err(|e| format!("Failed to spawn command '{}': {}", command_str, e))?;

        let raw_stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let (stdout, discovered_cwd) = split_cwd_marker(&raw_stdout);
        if let Some(next_dir) = discovered_cwd {
            if next_dir.is_dir() {
                if let Ok(mut cwd) = self.cwd.lock() {
                    *cwd = next_dir;
                }
            }
        }
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
            Ok(format!(
                "{}\n[Output truncated at 8,000 characters]",
                truncated
            ))
        } else {
            Ok(out)
        }
    }
}

#[cfg(target_os = "windows")]
fn select_windows_shell(requested: &str, command: &str) -> &'static str {
    match requested.to_ascii_lowercase().as_str() {
        "cmd" => "cmd",
        "powershell" | "pwsh" => "powershell",
        "sh" => "powershell",
        _ if command.contains("&&")
            || command.contains("||")
            || command.contains("%CD%")
            || command.contains("!CD!") =>
        {
            "cmd"
        }
        _ => "powershell",
    }
}

#[cfg(test)]
mod shell_selection_tests {
    #[cfg(target_os = "windows")]
    use super::select_windows_shell;

    #[cfg(target_os = "windows")]
    #[test]
    fn auto_prefers_powershell_for_powershell_syntax() {
        assert_eq!(
            select_windows_shell("auto", "Get-ChildItem | Select-Object Name"),
            "powershell"
        );
        assert_eq!(select_windows_shell("auto", "echo one && echo two"), "cmd");
        assert_eq!(select_windows_shell("cmd", "Get-ChildItem"), "cmd");
    }
}

fn split_cwd_marker(stdout: &str) -> (String, Option<PathBuf>) {
    let Some(marker_pos) = stdout.rfind(CWD_MARKER) else {
        return (stdout.to_string(), None);
    };
    let command_output = stdout[..marker_pos]
        .trim_end_matches(&['\r', '\n'][..])
        .to_string();
    let cwd = stdout[marker_pos + CWD_MARKER.len()..]
        .lines()
        .next()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    (command_output, cwd)
}

#[cfg(test)]
mod tests {
    use super::split_cwd_marker;

    #[test]
    fn extracts_persistent_directory_without_leaking_marker() {
        let (output, cwd) = split_cwd_marker("hello\r\n\r\n__GEMINI_HARNESS_CWD__C:\\work\r\n");
        assert_eq!(output, "hello");
        assert_eq!(cwd.unwrap().to_string_lossy(), "C:\\work");
    }
}

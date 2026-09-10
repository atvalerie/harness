use async_trait::async_trait;
use serde_json::json;
use std::path::PathBuf;
use std::process::Stdio;
use tokio::process::Command;

use super::{working_dir_path, SharedWorkingDir, Tool, ToolPreview};

const CWD_MARKER: &str = "__HOLIDAY_CWD__";

pub struct RunCommandTool {
    tasks: crate::tasks::TaskManager,
    cwd: SharedWorkingDir,
}

impl RunCommandTool {
    pub fn with_tasks(cwd: SharedWorkingDir, tasks: crate::tasks::TaskManager) -> Self {
        Self { cwd, tasks }
    }
    #[cfg(test)]
    pub fn new(cwd: SharedWorkingDir) -> Self {
        Self {
            cwd,
            tasks: crate::tasks::TaskManager::default(),
        }
    }
}

#[async_trait]
impl Tool for RunCommandTool {
    fn name(&self) -> &'static str {
        "run_command"
    }

    fn description(&self) -> &'static str {
        "Executes a host shell command in the current working directory for builds, tests, git, scripts, or necessary host execution. Use edit_file/write_file for workspace changes instead of shell editing. Directory changes persist for later tools. On Windows, shell may be auto, powershell, or cmd; auto prefers PowerShell."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The exact shell command to execute"
                },
                "reason": {
                    "type": "string",
                    "description": "Briefly explain why this command is needed"
                },
                "expected_effect": {
                    "type": "string",
                    "description": "Describe what the command is expected to do"
                },
                "shell": {
                    "type": "string",
                    "enum": ["auto", "powershell", "cmd", "sh"],
                    "description": "Optional shell backend. Use auto unless the command requires a specific shell."
                },
                "timeout_seconds": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 900,
                    "description": "Optional execution timeout; defaults to 120 seconds"
                },
                "max_output_chars": {
                    "type": "integer",
                    "minimum": 1000,
                    "maximum": 100000,
                    "description": "Optional output cap; keeps both the beginning and end when exceeded"
                },
                "background": {
                    "type": "boolean",
                    "description": "Start and detach the command instead of waiting for completion"
                }
            },
            "required": ["command", "reason", "expected_effect"]
        })
    }

    fn generate_preview(&self, args: &serde_json::Value) -> ToolPreview {
        let command = args
            .get("command")
            .and_then(|v| v.as_str())
            .unwrap_or("<missing command>");
        let shell = args.get("shell").and_then(|v| v.as_str()).unwrap_or("auto");
        let cwd = working_dir_path(&self.cwd).display().to_string();
        let reason = args
            .get("reason")
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(String::from);
        let expected_effect = args
            .get("expected_effect")
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(String::from);

        ToolPreview {
            title: "Run Shell Command".to_string(),
            details: vec![
                format!("Shell: {}", shell),
                format!("Working Dir: {}", cwd),
                "Risk: Executes arbitrary code on host system".to_string(),
            ],
            reason,
            expected_effect,
            command: Some(command.to_string()),
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
        #[cfg(not(windows))]
        if !matches!(requested_shell, "auto" | "sh") {
            return Err("This platform supports auto/sh for managed shell commands".into());
        }
        let timeout_seconds = args
            .get("timeout_seconds")
            .and_then(|v| v.as_u64())
            .unwrap_or(120)
            .clamp(1, 900);
        let max_output_chars = args
            .get("max_output_chars")
            .and_then(|v| v.as_u64())
            .unwrap_or(8_000)
            .clamp(1_000, 100_000) as usize;
        let background = args
            .get("background")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        #[cfg(target_os = "windows")]
        let mut cmd = {
            let selected_shell = select_windows_shell(requested_shell, command_str);
            let mut c = if selected_shell == "cmd" {
                Command::new("cmd")
            } else {
                Command::new("powershell.exe")
            };
            if selected_shell == "cmd" {
                let wrapped = format!("{} & set holiday_exit=!ERRORLEVEL! & echo. & echo {}!CD! & exit /b !holiday_exit!", command_str, CWD_MARKER);
                c.args(["/V:ON", "/C", &wrapped]);
            } else {
                let wrapped = format!(
                    "{}; $holidaySuccess=$?; $holidayExit=$LASTEXITCODE; if ($null -eq $holidayExit) {{ $holidayExit=0 }}; if (-not $holidaySuccess -and $holidayExit -eq 0) {{ $holidayExit=1 }}; Write-Output (\"{}\" + (Get-Location).Path); exit $holidayExit",
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
            let wrapped = format!(
                "{}; holiday_exit=$?; printf '\\n{}%s\\n' \"$PWD\"; exit \"$holiday_exit\"",
                command_str, CWD_MARKER
            );
            c.args(["-c", &wrapped]);
            c
        };

        cmd.current_dir(&starting_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let output = self
            .tasks
            .process(
                cmd,
                command_str,
                timeout_seconds,
                background,
                args.get("_holiday_epoch")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0),
            )
            .await?;
        let raw_stdout = output.stdout;
        let (stdout, discovered_cwd) = split_cwd_marker(&raw_stdout);
        if let Some(next_dir) = discovered_cwd {
            if next_dir.is_dir() {
                let next_dir = next_dir.canonicalize().unwrap_or(next_dir);
                if let Ok(mut cwd) = self.cwd.lock() {
                    *cwd = next_dir;
                }
            }
        }
        let stderr = output.stderr;
        let code = output.code;

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

        if out.chars().count() > max_output_chars {
            Ok(format_bounded_output(&out, max_output_chars))
        } else {
            Ok(out)
        }
    }
}

fn format_bounded_output(output: &str, limit: usize) -> String {
    let head_len = limit * 2 / 3;
    let tail_len = limit.saturating_sub(head_len);
    let head = output.chars().take(head_len).collect::<String>();
    let tail = output
        .chars()
        .rev()
        .take(tail_len)
        .collect::<String>()
        .chars()
        .rev()
        .collect::<String>();
    format!(
        "{}\n[Output truncated to {} characters; tail preserved]\n{}",
        head, limit, tail
    )
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
    use super::{format_bounded_output, split_cwd_marker, RunCommandTool};
    use crate::tools::{new_working_dir, Tool};
    use serde_json::json;

    #[test]
    fn extracts_persistent_directory_without_leaking_marker() {
        let (output, cwd) = split_cwd_marker("hello\r\n\r\n__HOLIDAY_CWD__C:\\work\r\n");
        assert_eq!(output, "hello");
        assert_eq!(cwd.unwrap().to_string_lossy(), "C:\\work");
    }

    #[test]
    fn output_cap_keeps_head_and_tail() {
        let output = "0123456789";
        let bounded = format_bounded_output(output, 6);
        assert!(bounded.contains("0123"));
        assert!(bounded.contains("89"));
        assert!(bounded.contains("tail preserved"));
    }

    #[test]
    fn preview_preserves_multiline_command_and_explanations() {
        let tool = RunCommandTool::new(new_working_dir());
        let preview = tool.generate_preview(&json!({
            "command": "echo one\necho two",
            "reason": "Check both outputs",
            "expected_effect": "Prints two lines"
        }));

        assert_eq!(preview.command.as_deref(), Some("echo one\necho two"));
        assert_eq!(preview.reason.as_deref(), Some("Check both outputs"));
        assert_eq!(preview.expected_effect.as_deref(), Some("Prints two lines"));
        assert!(!preview
            .details
            .iter()
            .any(|detail| detail.starts_with("Command:")));
    }

    #[test]
    fn preview_keeps_missing_explanations_compatible() {
        let tool = RunCommandTool::new(new_working_dir());
        let preview = tool.generate_preview(&json!({"command": "echo hello"}));

        assert!(preview.reason.is_none());
        assert!(preview.expected_effect.is_none());
        assert_eq!(preview.command.as_deref(), Some("echo hello"));
    }

    #[test]
    fn preview_trims_explanations_and_ignores_whitespace_only_values() {
        let tool = RunCommandTool::new(new_working_dir());
        let preview = tool.generate_preview(&json!({
            "command": "echo hello",
            "reason": "  Explain this  ",
            "expected_effect": "   "
        }));

        assert_eq!(preview.reason.as_deref(), Some("Explain this"));
        assert!(preview.expected_effect.is_none());
    }
}

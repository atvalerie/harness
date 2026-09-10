//! Bounded, inspectable runtime task records with cancellation channels.
use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::Instant,
};
use tokio::sync::watch;
#[derive(Clone, Default)]
pub struct TaskManager {
    records: Arc<Mutex<BTreeMap<String, Task>>>,
    next: Arc<AtomicU64>,
}
struct Task {
    label: String,
    parent: u64,
    status: String,
    output: String,
    started: Instant,
    elapsed: Option<std::time::Duration>,
    cancel: watch::Sender<bool>,
}
impl TaskManager {
    pub fn start(&self, label: &str, parent: u64) -> (String, watch::Receiver<bool>) {
        let id = format!("task-{}", self.next.fetch_add(1, Ordering::Relaxed) + 1);
        let (tx, rx) = watch::channel(false);
        let mut records = self.records.lock().unwrap_or_else(|e| e.into_inner());
        if records.len() >= 200 {
            if let Some(old) = records
                .iter()
                .find(|(_, t)| t.elapsed.is_some())
                .map(|(id, _)| id.clone())
            {
                records.remove(&old);
            }
        }
        records.insert(
            id.clone(),
            Task {
                label: label.into(),
                parent,
                status: "running".into(),
                output: String::new(),
                started: Instant::now(),
                elapsed: None,
                cancel: tx,
            },
        );
        (id, rx)
    }
    pub fn log(&self, id: &str, text: &str) {
        if let Ok(mut all) = self.records.lock() {
            if let Some(t) = all.get_mut(id) {
                t.output.push_str(text);
                if t.output.len() > 128_000 {
                    let mut offset = t.output.len() - 96_000;
                    while !t.output.is_char_boundary(offset) {
                        offset += 1;
                    }
                    t.output.drain(..offset);
                }
            }
        }
    }
    pub fn finish(&self, id: &str, status: &str) {
        if let Ok(mut all) = self.records.lock() {
            if let Some(t) = all.get_mut(id) {
                t.status = status.into();
                t.elapsed = Some(t.started.elapsed());
            }
        }
    }
    pub fn cancel(&self, id: &str) -> Result<String, String> {
        let all = self.records.lock().map_err(|_| "Task store unavailable")?;
        let t = all.get(id).ok_or("Unknown task")?;
        if t.elapsed.is_some() {
            return Err("Task already finished".into());
        }
        t.cancel
            .send(true)
            .map_err(|_| "Task no longer accepts cancellation")?;
        Ok(format!("Cancellation requested for {id}"))
    }
    pub async fn shutdown(&self) {
        if let Ok(all) = self.records.lock() {
            for task in all.values().filter(|t| t.elapsed.is_none()) {
                let _ = task.cancel.send(true);
            }
        }
        let _ = tokio::time::timeout(std::time::Duration::from_secs(4), async {
            while self.running() {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await;
    }
    pub fn cancel_parent(&self, parent: u64) {
        if let Ok(all) = self.records.lock() {
            for t in all
                .values()
                .filter(|t| t.parent == parent && t.elapsed.is_none())
            {
                let _ = t.cancel.send(true);
            }
        }
    }
    pub fn summary(&self) -> String {
        if self.records.lock().is_ok_and(|all| all.is_empty()) {
            return "No managed tasks".into();
        }
        self.records
            .lock()
            .map(|all| {
                all.iter()
                    .map(|(id, t)| {
                        format!(
                            "{id} [{}] {:.1}s - {}",
                            t.status,
                            t.elapsed
                                .unwrap_or_else(|| t.started.elapsed())
                                .as_secs_f64(),
                            t.label
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default()
    }
    pub fn inspect(&self, id: &str) -> String {
        self.records
            .lock()
            .ok()
            .and_then(|all| {
                all.get(id)
                    .map(|t| format!("{id} [{}] {}\n\n{}", t.status, t.label, t.output))
            })
            .unwrap_or_else(|| "Unknown task".into())
    }
    pub fn running(&self) -> bool {
        self.records
            .lock()
            .is_ok_and(|all| all.values().any(|t| t.elapsed.is_none()))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cancellation_and_bounded_output() {
        let m = TaskManager::default();
        let (id, rx) = m.start("test", 1);
        m.log(&id, &"?".repeat(100_000));
        assert!(m.inspect(&id).len() < 130_000);
        m.cancel(&id).unwrap();
        assert!(*rx.borrow());
        m.finish(&id, "cancelled");
        assert!(!m.running());
        assert!(m.cancel(&id).is_err());
    }
}

pub struct ProcessOutput {
    pub stdout: String,
    pub stderr: String,
    pub code: i32,
}
impl TaskManager {
    pub async fn process(
        &self,
        mut cmd: tokio::process::Command,
        label: &str,
        seconds: u64,
        background: bool,
        parent: u64,
    ) -> Result<ProcessOutput, String> {
        use tokio::io::AsyncReadExt;
        cmd.kill_on_drop(true);
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            cmd.as_std_mut().process_group(0);
        }
        #[cfg(windows)]
        {
            cmd.creation_flags(0x08000000);
        }
        let mut child = cmd.spawn().map_err(|e| e.to_string())?;
        let (id, mut cancel) = self.start(label, parent);
        let pid = child.id();
        let manager = self.clone();
        let worker_id = id.clone();
        let mut stdout = child.stdout.take().ok_or("Missing stdout")?;
        let mut stderr = child.stderr.take().ok_or("Missing stderr")?;
        let out_manager = self.clone();
        let out_id = id.clone();
        let mut out = tokio::spawn(async move {
            let mut bytes = Vec::new();
            let mut chunk = [0; 4096];
            loop {
                match stdout.read(&mut chunk).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        out_manager.log(&out_id, &String::from_utf8_lossy(&chunk[..n]));
                        bytes.extend_from_slice(&chunk[..n]);
                        if bytes.len() > 128_000 {
                            bytes.drain(..bytes.len() - 96_000);
                        }
                    }
                }
            }
            String::from_utf8_lossy(&bytes).to_string()
        });
        let err_manager = self.clone();
        let err_id = id.clone();
        let mut err = tokio::spawn(async move {
            let mut bytes = Vec::new();
            let mut chunk = [0; 4096];
            loop {
                match stderr.read(&mut chunk).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        err_manager.log(&err_id, &String::from_utf8_lossy(&chunk[..n]));
                        bytes.extend_from_slice(&chunk[..n]);
                        if bytes.len() > 128_000 {
                            bytes.drain(..bytes.len() - 96_000);
                        }
                    }
                }
            }
            String::from_utf8_lossy(&bytes).to_string()
        });
        let (mut done, receive) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let result = tokio::select! {
              status=child.wait()=>status.map(|s|s.code().unwrap_or(-1)).map_err(|e|e.to_string()),
              _=cancel.changed()=>Err("Task cancelled".to_string()),
              _=done.closed(), if !background=>Err("Task cancelled by parent".to_string()),
              _=tokio::time::sleep(std::time::Duration::from_secs(seconds))=>Err(format!("Task timed out after {seconds}s")),
            };
            if result.is_err() {
                if let Some(pid) = pid {
                    #[cfg(windows)]
                    {
                        let mut kill = tokio::process::Command::new("taskkill.exe");
                        kill.creation_flags(0x08000000);
                        let _ = kill
                            .args(["/PID", &pid.to_string(), "/T", "/F"])
                            .output()
                            .await;
                    }
                    #[cfg(unix)]
                    {
                        let _ = tokio::process::Command::new("kill")
                            .args(["-KILL", "--", &format!("-{pid}")])
                            .output()
                            .await;
                    }
                }
                let _ = child.kill().await;
                out.abort();
                err.abort();
            }
            let stdout =
                match tokio::time::timeout(std::time::Duration::from_secs(1), &mut out).await {
                    Ok(result) => result.unwrap_or_default(),
                    Err(_) => {
                        out.abort();
                        "[Output pipe held open by descendant; see task log]".into()
                    }
                };
            let stderr =
                match tokio::time::timeout(std::time::Duration::from_secs(1), &mut err).await {
                    Ok(result) => result.unwrap_or_default(),
                    Err(_) => {
                        err.abort();
                        "[Error pipe held open by descendant; see task log]".into()
                    }
                };
            let output = ProcessOutput {
                stdout,
                stderr,
                code: *result.as_ref().unwrap_or(&-1),
            };
            manager.finish(
                &worker_id,
                match &result {
                    Ok(0) => "completed",
                    Ok(_) => "failed",
                    Err(e) if e.contains("cancelled") => "cancelled",
                    Err(_) => "failed",
                },
            );
            if let Err(e) = &result {
                manager.log(&worker_id, e);
            }
            let _ = done.send(result.map(|_| output));
        });
        if background {
            Ok(ProcessOutput {
                stdout: format!("Started managed task {id}; use /tasks {id} or /tasks cancel {id}"),
                stderr: String::new(),
                code: 0,
            })
        } else {
            receive
                .await
                .map_err(|_| "Process supervisor stopped".to_string())?
        }
    }
}

#[cfg(test)]
mod process_tests {
    use super::*;
    fn shell(script: &str) -> tokio::process::Command {
        #[cfg(windows)]
        let mut command = {
            let mut c = tokio::process::Command::new("powershell.exe");
            c.args(["-NoProfile", "-NonInteractive", "-Command", script]);
            c
        };
        #[cfg(not(windows))]
        let mut command = {
            let mut c = tokio::process::Command::new("sh");
            c.args(["-c", script]);
            c
        };
        command
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        command
    }
    #[tokio::test]
    async fn captures_exit_status_and_output() {
        let tasks = TaskManager::default();
        let result = tasks
            .process(shell("echo hello; exit 7"), "test", 5, false, 1)
            .await
            .unwrap();
        assert_eq!(result.code, 7);
        assert!(result.stdout.contains("hello"));
        assert!(tasks.summary().contains("failed"));
    }
    #[tokio::test]
    async fn background_process_can_be_cancelled() {
        let tasks = TaskManager::default();
        let script = if cfg!(windows) {
            "Start-Sleep -Seconds 30"
        } else {
            "sleep 30"
        };
        tasks
            .process(shell(script), "background", 40, true, 4)
            .await
            .unwrap();
        tasks.cancel("task-1").unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(6), async {
            while tasks.running() {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        assert!(tasks.summary().contains("cancelled"));
    }
}

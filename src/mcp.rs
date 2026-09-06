use crate::config::McpServerConfig;
use crate::tools::{DiffHunk, Tool, ToolPreview};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::{Arc, atomic::{AtomicU64, Ordering}};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;
use tokio::time::{timeout, Duration};

struct McpConnection {
    _child: Mutex<Child>,
    stdin: Mutex<ChildStdin>,
    stdout: Mutex<BufReader<ChildStdout>>,
    next_id: AtomicU64,
}

impl McpConnection {
    async fn request(&self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let request = json!({"jsonrpc":"2.0","id":id,"method":method,"params":params});
        {
            let mut stdin = self.stdin.lock().await;
            stdin.write_all(format!("{}\n", request).as_bytes()).await.map_err(|e| format!("MCP write failed: {}", e))?;
            stdin.flush().await.map_err(|e| format!("MCP flush failed: {}", e))?;
        }
        let mut stdout = self.stdout.lock().await;
        let mut line = String::new();
        loop {
            line.clear();
            let read = stdout.read_line(&mut line).await.map_err(|e| format!("MCP read failed: {}", e))?;
            if read == 0 { return Err("MCP server exited unexpectedly".to_string()); }
            let Ok(value) = serde_json::from_str::<Value>(line.trim()) else { continue };
            if value.get("id").and_then(Value::as_u64) != Some(id) { continue; }
            if let Some(error) = value.get("error") { return Err(format!("MCP error: {}", error)); }
            return Ok(value.get("result").cloned().unwrap_or(Value::Null));
        }
    }

    async fn notify(&self, method: &str, params: Value) -> Result<(), String> {
        let request = json!({"jsonrpc":"2.0","method":method,"params":params});
        let mut stdin = self.stdin.lock().await;
        stdin.write_all(format!("{}\n", request).as_bytes()).await.map_err(|e| format!("MCP write failed: {}", e))?;
        stdin.flush().await.map_err(|e| format!("MCP flush failed: {}", e))
    }
}

struct McpTool {
    public_name: &'static str,
    description: &'static str,
    original_name: String,
    schema: Value,
    connection: Arc<McpConnection>,
}

#[async_trait]
impl Tool for McpTool {
    fn name(&self) -> &'static str { self.public_name }
    fn description(&self) -> &'static str { self.description }
    fn parameters_schema(&self) -> Value { self.schema.clone() }
    fn generate_preview(&self, args: &Value) -> ToolPreview {
        ToolPreview { title: format!("MCP: {}", self.public_name), details: vec![format!("Arguments: {}", args)], diff_hunks: Vec::<DiffHunk>::new(), is_mutation: true }
    }
    async fn execute(&self, args: Value) -> Result<String, String> {
        let result = timeout(Duration::from_secs(120), self.connection.request("tools/call", json!({"name":self.original_name,"arguments":args}))).await.map_err(|_| "MCP tool timed out".to_string())??;
        let Some(content) = result.get("content").and_then(Value::as_array) else { return Ok(result.to_string()); };
        let mut output = String::new();
        for item in content {
            if let Some(text) = item.get("text").and_then(Value::as_str) { output.push_str(text); output.push('\n'); }
            else { output.push_str(&item.to_string()); output.push('\n'); }
        }
        if result.get("isError").and_then(Value::as_bool).unwrap_or(false) { Err(output) } else { Ok(output.trim_end().to_string()) }
    }
}

pub async fn connect_all(configs: &BTreeMap<String, McpServerConfig>) -> Vec<Arc<dyn Tool>> {
    let mut tools: Vec<Arc<dyn Tool>> = Vec::new();
    for (server_name, config) in configs {
        if !config.enabled { continue; }
        match connect_server(server_name, config).await {
            Ok(server_tools) => tools.extend(server_tools),
            Err(error) => eprintln!("MCP server '{}': {}", server_name, error),
        }
    }
    tools
}

async fn connect_server(server_name: &str, config: &McpServerConfig) -> Result<Vec<Arc<dyn Tool>>, String> {
    let mut command = Command::new(&config.command);
    command.args(&config.args).envs(&config.env).stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::null());
    let mut child = command.spawn().map_err(|e| format!("failed to start '{}': {}", config.command, e))?;
    let stdin = child.stdin.take().ok_or_else(|| "MCP stdin unavailable".to_string())?;
    let stdout = child.stdout.take().ok_or_else(|| "MCP stdout unavailable".to_string())?;
    let connection = Arc::new(McpConnection { _child: Mutex::new(child), stdin: Mutex::new(stdin), stdout: Mutex::new(BufReader::new(stdout)), next_id: AtomicU64::new(1) });
    let result_init = timeout(Duration::from_secs(10), connection.request("initialize", json!({"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"gemini-harness","version":env!("CARGO_PKG_VERSION")}}))).await.map_err(|_| "initialize timed out".to_string())??;
    let _ = result_init;
    timeout(Duration::from_secs(10), connection.notify("notifications/initialized", json!({}))).await.map_err(|_| "initialized notification timed out".to_string())??;
    let result = timeout(Duration::from_secs(10), connection.request("tools/list", json!({}))).await.map_err(|_| "tools/list timed out".to_string())??;
    let mut tools: Vec<Arc<dyn Tool>> = Vec::new();
    for tool in result.get("tools").and_then(Value::as_array).into_iter().flatten() {
        let original_name = tool.get("name").and_then(Value::as_str).unwrap_or("unknown").to_string();
        let public_name = Box::leak(format!("mcp__{}__{}", server_name, original_name).into_boxed_str());
        let description = Box::leak(tool.get("description").and_then(Value::as_str).unwrap_or("MCP server tool").to_string().into_boxed_str());
        let schema = tool.get("inputSchema").cloned().unwrap_or_else(|| json!({"type":"object"}));
        tools.push(Arc::new(McpTool { public_name, description, original_name, schema, connection: connection.clone() }));
    }
    Ok(tools)
}

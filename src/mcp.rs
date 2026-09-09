use crate::config::McpServerConfig;
use crate::tools::{DiffHunk, Tool, ToolPreview, ToolRisk};
use async_trait::async_trait;
use reqwest::Client;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashSet};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;
use tokio::time::{timeout, Duration};

const MCP_PROTOCOL_VERSION: &str = "2025-11-25";

fn provider_safe_tool_name(raw: &str) -> String {
    let normalized = raw
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>();
    let normalized = if normalized.is_empty() {
        "mcp_tool".to_string()
    } else {
        normalized
    };

    if normalized.len() <= 64 {
        return normalized;
    }

    let digest = Sha256::digest(raw.as_bytes());
    let suffix = format!(
        "_{:02x}{:02x}{:02x}{:02x}",
        digest[0], digest[1], digest[2], digest[3]
    );
    let prefix_len = 64 - suffix.len();
    format!(
        "{}{}",
        normalized.chars().take(prefix_len).collect::<String>(),
        suffix
    )
}

fn unique_provider_safe_tool_name(raw: &str, used_names: &mut HashSet<String>) -> String {
    let base = provider_safe_tool_name(raw);
    if used_names.insert(base.clone()) {
        return base;
    }

    let digest = Sha256::digest(raw.as_bytes());
    let mut attempt = 0u32;
    loop {
        let candidate = provider_safe_tool_name(&format!(
            "{}__collision_{:02x}{:02x}{:02x}{:02x}_{}",
            raw, digest[0], digest[1], digest[2], digest[3], attempt
        ));
        if used_names.insert(candidate.clone()) {
            return candidate;
        }
        attempt = attempt.saturating_add(1);
    }
}

/// Classifies the known Lightpanda mutation tools while keeping unknown MCP
/// tools conservative. Read-only browser inspection should not require a
/// host-side approval; page mutations and local artifact writes should.
pub fn tool_risk(name: &str) -> ToolRisk {
    match name.rsplit("__").next().unwrap_or(name) {
        "click" | "fill" | "evaluate" | "press" | "selectOption" | "setChecked" | "save"
        | "screenshot" | "session_close" => ToolRisk::ExternalSideEffect,
        "goto"
        | "search"
        | "markdown"
        | "html"
        | "links"
        | "tree"
        | "nodeDetails"
        | "interactiveElements"
        | "structuredData"
        | "detectForms"
        | "scroll"
        | "hover"
        | "waitForSelector"
        | "waitForScript"
        | "waitForState"
        | "getUrl"
        | "getCookies"
        | "getEnv"
        | "consoleLogs"
        | "session_list"
        | "session_new"
        | "extract"
            if name.starts_with("mcp__") =>
        {
            ToolRisk::ReadOnly
        }
        _ => ToolRisk::ExternalSideEffect,
    }
}

enum Transport {
    Stdio {
        _child: Child,
        stdin: ChildStdin,
        stdout: BufReader<ChildStdout>,
    },
    Http {
        client: Client,
        url: String,
        headers: BTreeMap<String, String>,
        session_id: Option<String>,
    },
}

struct McpConnection {
    transport: Mutex<Transport>,
    next_id: AtomicU64,
}

impl McpConnection {
    async fn request(&self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let request = json!({"jsonrpc":"2.0","id":id,"method":method,"params":params});
        let mut transport = self.transport.lock().await;
        match &mut *transport {
            Transport::Stdio { stdin, stdout, .. } => {
                stdin
                    .write_all(format!("{}\n", request).as_bytes())
                    .await
                    .map_err(|e| format!("MCP write failed: {}", e))?;
                stdin
                    .flush()
                    .await
                    .map_err(|e| format!("MCP flush failed: {}", e))?;
                let mut line = String::new();
                loop {
                    line.clear();
                    if stdout
                        .read_line(&mut line)
                        .await
                        .map_err(|e| format!("MCP read failed: {}", e))?
                        == 0
                    {
                        return Err("MCP server exited unexpectedly".to_string());
                    }
                    if let Some(value) = parse_matching_json(&line, id) {
                        return value;
                    }
                }
            }
            Transport::Http {
                client,
                url,
                headers,
                session_id,
            } => {
                let mut req = client
                    .post(&*url)
                    .header("Accept", "application/json, text/event-stream")
                    .header("Content-Type", "application/json")
                    .header("MCP-Protocol-Version", MCP_PROTOCOL_VERSION)
                    .json(&request);
                for (key, value) in headers.iter() {
                    req = req.header(key, value);
                }
                if let Some(session) = session_id.as_deref() {
                    req = req.header("Mcp-Session-Id", session);
                }
                let response = req
                    .send()
                    .await
                    .map_err(|e| format!("MCP HTTP request failed: {}", e))?;
                if let Some(value) = response
                    .headers()
                    .get("Mcp-Session-Id")
                    .and_then(|value| value.to_str().ok())
                {
                    *session_id = Some(value.to_string());
                }
                if !response.status().is_success() {
                    return Err(format!(
                        "MCP HTTP error {}: {}",
                        response.status(),
                        response.text().await.unwrap_or_default()
                    ));
                }
                let body = response
                    .text()
                    .await
                    .map_err(|e| format!("MCP HTTP response failed: {}", e))?;
                for line in body.lines() {
                    if let Some(value) = parse_matching_json(line, id) {
                        return value;
                    }
                }
                Err("MCP HTTP response contained no matching JSON-RPC result".to_string())
            }
        }
    }

    async fn notify(&self, method: &str, params: Value) -> Result<(), String> {
        let request = json!({"jsonrpc":"2.0","method":method,"params":params});
        let mut transport = self.transport.lock().await;
        match &mut *transport {
            Transport::Stdio { stdin, .. } => {
                stdin
                    .write_all(format!("{}\n", request).as_bytes())
                    .await
                    .map_err(|e| format!("MCP write failed: {}", e))?;
                stdin
                    .flush()
                    .await
                    .map_err(|e| format!("MCP flush failed: {}", e))
            }
            Transport::Http {
                client,
                url,
                headers,
                session_id,
            } => {
                let mut req = client
                    .post(&*url)
                    .header("Accept", "application/json, text/event-stream")
                    .header("Content-Type", "application/json")
                    .header("MCP-Protocol-Version", MCP_PROTOCOL_VERSION)
                    .json(&request);
                for (key, value) in headers.iter() {
                    req = req.header(key, value);
                }
                if let Some(session) = session_id.as_deref() {
                    req = req.header("Mcp-Session-Id", session);
                }
                let response = req
                    .send()
                    .await
                    .map_err(|e| format!("MCP HTTP notification failed: {}", e))?;
                if !response.status().is_success() {
                    return Err(format!("MCP HTTP notification error {}", response.status()));
                }
                Ok(())
            }
        }
    }
}

fn parse_matching_json(line: &str, id: u64) -> Option<Result<Value, String>> {
    let payload = line
        .trim()
        .strip_prefix("data:")
        .map(str::trim)
        .unwrap_or(line.trim());
    let value = serde_json::from_str::<Value>(payload).ok()?;
    if value.get("id").and_then(Value::as_u64) != Some(id) {
        return None;
    }
    if let Some(error) = value.get("error") {
        return Some(Err(format!("MCP error: {}", error)));
    }
    Some(Ok(value.get("result").cloned().unwrap_or(Value::Null)))
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
    fn name(&self) -> &'static str {
        self.public_name
    }
    fn description(&self) -> &'static str {
        self.description
    }
    fn parameters_schema(&self) -> Value {
        self.schema.clone()
    }
    fn generate_preview(&self, args: &Value) -> ToolPreview {
        ToolPreview {
            title: format!("MCP: {}", self.public_name),
            details: vec![format!("Arguments: {}", args)],
            reason: None,
            expected_effect: None,
            command: None,
            diff_hunks: Vec::<DiffHunk>::new(),
            is_mutation: true,
        }
    }
    async fn execute(&self, args: Value) -> Result<String, String> {
        let result = timeout(
            Duration::from_secs(120),
            self.connection.request(
                "tools/call",
                json!({"name":self.original_name,"arguments":args}),
            ),
        )
        .await
        .map_err(|_| "MCP tool timed out".to_string())??;
        let Some(content) = result.get("content").and_then(Value::as_array) else {
            return Ok(result.to_string());
        };
        let mut output = String::new();
        for item in content {
            if let Some(text) = item.get("text").and_then(Value::as_str) {
                output.push_str(text);
                output.push('\n');
            } else {
                output.push_str(&item.to_string());
                output.push('\n');
            }
        }
        if result
            .get("isError")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            Err(output)
        } else {
            Ok(output.trim_end().to_string())
        }
    }
}

pub async fn connect_all(configs: &BTreeMap<String, McpServerConfig>) -> Vec<Arc<dyn Tool>> {
    let mut tools: Vec<Arc<dyn Tool>> = Vec::new();
    let mut used_names = HashSet::new();
    for (server_name, config) in configs {
        if !config.enabled {
            continue;
        }
        match connect_server(server_name, config, &mut used_names).await {
            Ok(server_tools) => tools.extend(server_tools),
            Err(error) => eprintln!("MCP server '{}': {}", server_name, error),
        }
    }
    tools
}

async fn connect_server(
    server_name: &str,
    config: &McpServerConfig,
    used_names: &mut HashSet<String>,
) -> Result<Vec<Arc<dyn Tool>>, String> {
    let transport = if config.transport.eq_ignore_ascii_case("http")
        || config.transport.eq_ignore_ascii_case("streamable-http")
        || config.transport.eq_ignore_ascii_case("sse")
    {
        let url = config
            .url
            .clone()
            .ok_or_else(|| "HTTP MCP server requires url".to_string())?;
        Transport::Http {
            client: Client::new(),
            url,
            headers: config.headers.clone(),
            session_id: None,
        }
    } else {
        let mut command = Command::new(&config.command);
        command
            .args(&config.args)
            .envs(&config.env)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null());
        let mut child = command
            .spawn()
            .map_err(|e| format!("failed to start '{}': {}", config.command, e))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| "MCP stdin unavailable".to_string())?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "MCP stdout unavailable".to_string())?;
        Transport::Stdio {
            _child: child,
            stdin,
            stdout: BufReader::new(stdout),
        }
    };
    let connection = Arc::new(McpConnection {
        transport: Mutex::new(transport),
        next_id: AtomicU64::new(1),
    });
    timeout(Duration::from_secs(10), connection.request("initialize", json!({"protocolVersion":MCP_PROTOCOL_VERSION,"capabilities":{},"clientInfo":{"name":"holiday","version":env!("CARGO_PKG_VERSION")}}))).await.map_err(|_| "initialize timed out".to_string())??;
    timeout(
        Duration::from_secs(10),
        connection.notify("notifications/initialized", json!({})),
    )
    .await
    .map_err(|_| "initialized notification timed out".to_string())??;
    let result = timeout(
        Duration::from_secs(10),
        connection.request("tools/list", json!({})),
    )
    .await
    .map_err(|_| "tools/list timed out".to_string())??;
    let mut tools: Vec<Arc<dyn Tool>> = Vec::new();
    for tool in result
        .get("tools")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let original_name = tool
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string();
        let public_name = Box::leak(
            unique_provider_safe_tool_name(
                &format!("mcp__{}__{}", server_name, original_name),
                used_names,
            )
            .into_boxed_str(),
        );
        let description = Box::leak(
            tool.get("description")
                .and_then(Value::as_str)
                .unwrap_or("MCP server tool")
                .to_string()
                .into_boxed_str(),
        );
        let schema = tool
            .get("inputSchema")
            .cloned()
            .unwrap_or_else(|| json!({"type":"object"}));
        tools.push(Arc::new(McpTool {
            public_name,
            description,
            original_name,
            schema,
            connection: connection.clone(),
        }));
    }
    Ok(tools)
}

#[cfg(test)]
mod tests {
    use super::{
        parse_matching_json, provider_safe_tool_name, tool_risk, unique_provider_safe_tool_name,
    };
    use crate::tools::ToolRisk;
    use std::collections::HashSet;

    #[test]
    fn parses_streamable_http_sse_json_rpc_data() {
        let result = parse_matching_json(
            "data: {\"jsonrpc\":\"2.0\",\"id\":7,\"result\":{\"ok\":true}}",
            7,
        )
        .unwrap()
        .unwrap();
        assert_eq!(result["ok"], true);
        assert!(parse_matching_json("data: {\"id\":8,\"result\":{}}", 7).is_none());
    }

    #[test]
    fn normalizes_mcp_names_for_provider_compatibility() {
        let name = provider_safe_tool_name("mcp__server.name__tool/read");
        assert_eq!(name, "mcp__server_name__tool_read");
        assert!(name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-'));
    }

    #[test]
    fn bounds_long_mcp_names_without_losing_stable_identity() {
        let raw = format!("mcp__{}__{}", "server".repeat(40), "tool".repeat(40));
        let first = provider_safe_tool_name(&raw);
        let second = provider_safe_tool_name(&raw);
        assert_eq!(first, second);
        assert_eq!(first.len(), 64);
    }

    #[test]
    fn disambiguates_mcp_names_that_normalize_to_the_same_value() {
        let mut used = HashSet::new();
        let first = unique_provider_safe_tool_name("mcp__server.name__tool/read", &mut used);
        let second = unique_provider_safe_tool_name("mcp__server/name__tool.read", &mut used);
        assert_ne!(first, second);
        assert_eq!(used.len(), 2);
    }

    #[test]
    fn classifies_lightpanda_reads_and_mutations() {
        assert_eq!(tool_risk("mcp__lightpanda__markdown"), ToolRisk::ReadOnly);
        assert_eq!(
            tool_risk("mcp__lightpanda__fill"),
            ToolRisk::ExternalSideEffect
        );
    }
}

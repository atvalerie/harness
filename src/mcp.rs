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
            if name.starts_with("mcp__lightpanda__") =>
        {
            ToolRisk::ReadOnly
        }
        _ => ToolRisk::ExternalSideEffect,
    }
}

fn normalize_lightpanda_args(tool_name: &str, mut args: Value) -> Value {
    if !tool_name.starts_with("mcp__lightpanda__") {
        return args;
    }
    let Some(object) = args.as_object_mut() else {
        return args;
    };

    // Some providers materialize optional JSON-schema fields as empty
    // strings or zeroes. Lightpanda treats those as real arguments, which can
    // turn an otherwise valid markdown/tree read into InvalidParams.
    if object
        .get("selector")
        .and_then(Value::as_str)
        .is_some_and(str::is_empty)
    {
        object.remove("selector");
    }
    if object
        .get("url")
        .and_then(Value::as_str)
        .is_some_and(str::is_empty)
        && tool_name != "mcp__lightpanda__goto"
    {
        object.remove("url");
    }
    if object
        .get("backendNodeId")
        .and_then(Value::as_i64)
        .is_some_and(|value| value == 0)
    {
        object.remove("backendNodeId");
    }
    args
}

enum Transport {
    Stdio {
        _child: Box<Child>,
        stdin: ChildStdin,
        stdout: BufReader<ChildStdout>,
    },
    Http {
        client: Client,
        url: String,
        headers: BTreeMap<String, String>,
        session_id: Option<String>,
        protocol_version: String,
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
                    // With no sampling/roots capabilities advertised, reject unsupported
                    // server requests instead of silently deadlocking the server.
                    if let Ok(incoming) = serde_json::from_str::<Value>(&line) {
                        if let (Some(request_id), Some(method)) = (
                            incoming.get("id"),
                            incoming.get("method").and_then(Value::as_str),
                        ) {
                            let reply = if method == "ping" {
                                json!({"jsonrpc":"2.0","id":request_id,"result":{}})
                            } else {
                                json!({"jsonrpc":"2.0","id":request_id,"error":{"code":-32601,"message":"Client does not support this method"}})
                            };
                            stdin
                                .write_all(format!("{reply}\n").as_bytes())
                                .await
                                .map_err(|e| format!("MCP reply failed: {e}"))?;
                            stdin
                                .flush()
                                .await
                                .map_err(|e| format!("MCP flush failed: {e}"))?;
                            continue;
                        }
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
                protocol_version,
            } => {
                let mut req = client
                    .post(&*url)
                    .header("Accept", "application/json, text/event-stream")
                    .header("Content-Type", "application/json")
                    .header("MCP-Protocol-Version", protocol_version.as_str())
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
                read_http_result(response, id).await
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
                protocol_version,
            } => {
                let mut req = client
                    .post(&*url)
                    .header("Accept", "application/json, text/event-stream")
                    .header("Content-Type", "application/json")
                    .header("MCP-Protocol-Version", protocol_version.as_str())
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

// Do not wait for EOF on SSE: servers may keep the connection open after
// the matching response. Buffer bytes so split UTF-8 and multiline data work.
async fn read_http_result(mut response: reqwest::Response, id: u64) -> Result<Value, String> {
    let sse = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.split(';').next().unwrap_or("").trim() == "text/event-stream");
    let mut buffer = Vec::new();
    let mut data = String::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| format!("MCP response read failed: {e}"))?
    {
        buffer.extend_from_slice(&chunk);
        if buffer.len() + data.len() > 8 * 1024 * 1024 {
            return Err("MCP response exceeded 8 MiB".into());
        }
        if !sse {
            continue;
        }
        while let Some(end) = buffer.iter().position(|b| *b == b'\n') {
            let bytes: Vec<_> = buffer.drain(..=end).collect();
            let line = std::str::from_utf8(&bytes)
                .map_err(|_| "MCP SSE contains invalid UTF-8")?
                .trim_end_matches(['\r', '\n']);
            if line.is_empty() {
                if let Some(result) = parse_matching_json(&data, id) {
                    return result;
                }
                data.clear();
            } else if let Some(value) = line.strip_prefix("data:") {
                if !data.is_empty() {
                    data.push('\n');
                }
                data.push_str(value.strip_prefix(' ').unwrap_or(value));
            }
        }
    }
    if !sse {
        let body = std::str::from_utf8(&buffer).map_err(|_| "MCP JSON contains invalid UTF-8")?;
        if let Some(result) = parse_matching_json(body, id) {
            return result;
        }
    }
    Err("MCP HTTP response ended without a matching JSON-RPC result".into())
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
        let args = normalize_lightpanda_args(self.public_name, args);
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

pub async fn connect_all(
    configs: &BTreeMap<String, McpServerConfig>,
) -> (Vec<Arc<dyn Tool>>, Vec<String>) {
    let mut errors = Vec::new();
    let mut tools: Vec<Arc<dyn Tool>> = Vec::new();
    let mut used_names = HashSet::new();
    for (server_name, config) in configs {
        if !config.enabled {
            continue;
        }
        match connect_server(server_name, config, &mut used_names).await {
            Ok(server_tools) => tools.extend(server_tools),
            Err(error) => errors.push(format!("{server_name}: {error}")),
        }
    }
    (tools, errors)
}

async fn connect_server(
    server_name: &str,
    config: &McpServerConfig,
    used_names: &mut HashSet<String>,
) -> Result<Vec<Arc<dyn Tool>>, String> {
    if config.transport.eq_ignore_ascii_case("sse") {
        return Err("Legacy HTTP+SSE transport is not supported; configure a streamable-http endpoint instead".into());
    }
    let transport = if config.transport.eq_ignore_ascii_case("http")
        || config.transport.eq_ignore_ascii_case("streamable-http")
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
            protocol_version: MCP_PROTOCOL_VERSION.to_string(),
        }
    } else {
        let mut command = Command::new(&config.command);
        command
            .kill_on_drop(true)
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
            _child: Box::new(child),
            stdin,
            stdout: BufReader::new(stdout),
        }
    };
    let connection = Arc::new(McpConnection {
        transport: Mutex::new(transport),
        next_id: AtomicU64::new(1),
    });
    let initialized = timeout(Duration::from_secs(10), connection.request("initialize", json!({"protocolVersion":MCP_PROTOCOL_VERSION,"capabilities":{},"clientInfo":{"name":"holiday","version":env!("CARGO_PKG_VERSION")}}))).await.map_err(|_| "initialize timed out".to_string())??;
    let negotiated = initialized
        .get("protocolVersion")
        .and_then(Value::as_str)
        .ok_or_else(|| "MCP initialize omitted protocolVersion".to_string())?;
    if ![
        "2024-11-05",
        "2025-03-26",
        "2025-06-18",
        MCP_PROTOCOL_VERSION,
    ]
    .contains(&negotiated)
    {
        return Err(format!("Unsupported MCP protocol version: {negotiated}"));
    }
    if let Transport::Http {
        protocol_version, ..
    } = &mut *connection.transport.lock().await
    {
        *protocol_version = negotiated.to_string();
    }
    timeout(
        Duration::from_secs(10),
        connection.notify("notifications/initialized", json!({})),
    )
    .await
    .map_err(|_| "initialized notification timed out".to_string())??;
    let mut catalog = Vec::new();
    let mut cursor: Option<String> = None;
    let mut seen_cursors = HashSet::new();
    loop {
        let params = cursor
            .as_ref()
            .map(|c| json!({"cursor":c}))
            .unwrap_or_else(|| json!({}));
        let result = timeout(
            Duration::from_secs(10),
            connection.request("tools/list", params),
        )
        .await
        .map_err(|_| "tools/list timed out".to_string())??;
        let page = result
            .get("tools")
            .and_then(Value::as_array)
            .ok_or_else(|| "MCP tools/list omitted tools array".to_string())?;
        catalog.extend(page.iter().cloned());
        cursor = result
            .get("nextCursor")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let Some(next) = &cursor else {
            break;
        };
        if !seen_cursors.insert(next.clone()) || seen_cursors.len() > 100 {
            return Err("MCP tools/list pagination repeated a cursor or exceeded 100 pages".into());
        }
    }
    let mut tools: Vec<Arc<dyn Tool>> = Vec::new();
    for tool in &catalog {
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
        normalize_lightpanda_args, parse_matching_json, provider_safe_tool_name, tool_risk,
        unique_provider_safe_tool_name,
    };
    use crate::tools::ToolRisk;
    use serde_json::json;
    use std::collections::HashSet;

    #[test]
    fn unknown_servers_do_not_inherit_lightpanda_read_permissions() {
        for tool in ["search", "extract", "getEnv", "session_new"] {
            assert_eq!(
                tool_risk(&format!("mcp__untrusted__{tool}")),
                ToolRisk::ExternalSideEffect
            );
        }
        assert_eq!(tool_risk("mcp__lightpanda__markdown"), ToolRisk::ReadOnly);
    }

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

    #[test]
    fn removes_provider_generated_lightpanda_defaults() {
        let normalized = normalize_lightpanda_args(
            "mcp__lightpanda__markdown",
            json!({"url":"", "selector":"", "backendNodeId":0, "maxBytes":2000}),
        );
        assert_eq!(normalized, json!({"maxBytes":2000}));

        let goto = normalize_lightpanda_args(
            "mcp__lightpanda__goto",
            json!({"url":"", "waitUntil":"load"}),
        );
        assert_eq!(goto, json!({"url":"", "waitUntil":"load"}));
    }

    #[tokio::test]
    async fn pretty_json_and_rpc_errors_are_parsed() {
        let response = reqwest::Response::from(http::Response::new(
            "{\n\"id\":1,\n\"result\":{\"ok\":true}\n}",
        ));
        assert_eq!(
            super::read_http_result(response, 1).await.unwrap()["ok"],
            true
        );
        let response = reqwest::Response::from(http::Response::new(
            r#"{"id":1,"error":{"code":-1,"message":"broken"}}"#,
        ));
        assert!(super::read_http_result(response, 1)
            .await
            .unwrap_err()
            .contains("broken"));
    }

    #[tokio::test]
    async fn sse_returns_before_eof_with_fragmented_utf8_and_multiline_data() {
        use futures_util::StreamExt;
        let bytes = "data: {\"id\":99,\"result\":{}}\r\n\r\n: keepalive\r\ndata: {\"id\":1,\r\ndata: \"result\":{\"text\":\"漢字\"}}\r\n\r\n".as_bytes();
        let chunks: Vec<Result<Vec<u8>, std::io::Error>> =
            bytes.iter().map(|b| Ok(vec![*b])).collect();
        let body = reqwest::Body::wrap_stream(
            futures_util::stream::iter(chunks).chain(futures_util::stream::pending()),
        );
        let response = reqwest::Response::from(
            http::Response::builder()
                .header("content-type", "text/event-stream; charset=utf-8")
                .body(body)
                .unwrap(),
        );
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            super::read_http_result(response, 1),
        )
        .await
        .expect("must not wait for EOF")
        .unwrap();
        assert_eq!(result["text"], "漢字");
    }

    #[tokio::test]
    async fn truncated_sse_is_not_a_success() {
        let response = reqwest::Response::from(
            http::Response::builder()
                .header("content-type", "text/event-stream")
                .body("data: {\"id\":1,\"result\":{}")
                .unwrap(),
        );
        assert!(super::read_http_result(response, 1).await.is_err());
    }
    #[tokio::test]
    async fn http_negotiates_session_version_and_paginates_catalog() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            for step in 0..4 {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut data = Vec::new();
                let header_end = loop {
                    let mut byte = [0u8];
                    socket.read_exact(&mut byte).await.unwrap();
                    data.push(byte[0]);
                    if data.ends_with(b"\r\n\r\n") {
                        break data.len();
                    }
                };
                let headers = String::from_utf8(data.clone()).unwrap().to_lowercase();
                let length: usize = headers
                    .lines()
                    .find_map(|line| line.strip_prefix("content-length:"))
                    .unwrap()
                    .trim()
                    .parse()
                    .unwrap();
                data.resize(header_end + length, 0);
                socket.read_exact(&mut data[header_end..]).await.unwrap();
                let request: serde_json::Value =
                    serde_json::from_slice(&data[header_end..]).unwrap();
                if step > 0 {
                    assert!(headers.contains("mcp-protocol-version: 2025-03-26"));
                    assert!(headers.contains("mcp-session-id: session-test"));
                }
                let result = match step {
                    0 => {
                        assert_eq!(request["method"], "initialize");
                        json!({"protocolVersion":"2025-03-26","capabilities":{"tools":{}},"serverInfo":{"name":"fixture","version":"1"}})
                    }
                    1 => {
                        assert_eq!(request["method"], "notifications/initialized");
                        json!({})
                    }
                    2 => {
                        assert!(request["params"].get("cursor").is_none());
                        json!({"tools":[{"name":"first","inputSchema":{"type":"object"}}],"nextCursor":"page-2"})
                    }
                    _ => {
                        assert_eq!(request["params"]["cursor"], "page-2");
                        json!({"tools":[{"name":"second","inputSchema":{"type":"object"}}]})
                    }
                };
                let body = if step == 1 {
                    String::new()
                } else {
                    json!({"jsonrpc":"2.0","id":request["id"],"result":result}).to_string()
                };
                let status = if step == 1 { "202 Accepted" } else { "200 OK" };
                socket.write_all(format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nMcp-Session-Id: session-test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
            }
        });
        let config = serde_json::from_value(
            json!({"transport":"streamable-http","url":format!("http://{addr}/mcp")}),
        )
        .unwrap();
        let tools = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            super::connect_server("fixture", &config, &mut HashSet::new()),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[1].name(), "mcp__fixture__second");
        server.await.unwrap();
    }
}

mod agents;
mod app;
mod auto_review;
mod client;
mod codex_auth;
mod commands;
mod config;
mod events;
mod interaction;
mod mcp;
mod model_metadata;
mod prompts;
mod review;
mod session;
mod tasks;
mod tools;
mod ui;
mod usage;
mod worker;

use app::{App, EngineState};
use client::{is_zen_unsupported_model_id, ProviderKind};
use config::AppConfig;
use crossterm::{
    event::{
        self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        Event, KeyCode, KeyModifiers,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use events::{AppEvent, StreamSignal};
use ratatui::{backend::CrosstermBackend, Terminal};
use serde_json::{json, Value};
use std::io::{self, stdout, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::io::AsyncBufReadExt;
use tokio::sync::mpsc;

struct CliArgs {
    prompt: Option<String>,
    chat: bool,
    help: bool,
    resume: Option<String>,
    continue_latest: bool,
    provider: Option<String>,
    model: Option<String>,
    base_url: Option<String>,
    config_path: Option<PathBuf>,
    output_format: OutputFormat,
    tool_policy: ToolPolicy,
    login: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum OutputFormat {
    Text,
    Jsonl,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ToolPolicy {
    Ask,
    Auto,
    Review,
    Deny,
}

impl ToolPolicy {
    fn parse(value: &str) -> Result<Self, String> {
        match value.to_ascii_lowercase().as_str() {
            "ask" => Ok(Self::Ask),
            "auto" => Ok(Self::Auto),
            "review" => Ok(Self::Review),
            "deny" => Ok(Self::Deny),
            _ => Err(format!(
                "Unknown tool policy '{}'. Use ask, review, auto, or deny.",
                value
            )),
        }
    }
}

fn resolve_api_key(config: &AppConfig) -> io::Result<String> {
    if let Some(key) = config.get_api_key_for_active_provider() {
        return Ok(key);
    }
    if matches!(
        ProviderKind::parse(&config.active_provider_config().kind),
        ProviderKind::OpenAiCompatible | ProviderKind::Codex
    ) {
        return Ok(String::new());
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "No provider API key found in the environment or keyring",
    ))
}

async fn refresh_codex_auth_if_available(config: &AppConfig) {
    if !config.provider.eq_ignore_ascii_case("codex") {
        return;
    }
    let Some(auth) = AppConfig::get_codex_auth() else {
        return;
    };
    if auth.refresh_token.is_empty() {
        return;
    }
    let _ = codex_auth::refresh(&auth).await;
}

fn resolve_session_selector(
    selector: Option<&str>,
    continue_latest: bool,
) -> Result<Option<PathBuf>, String> {
    if let Some(selector) = selector {
        let path = PathBuf::from(selector);
        if path.is_file() {
            return Ok(Some(path));
        }
        return session::list()
            .into_iter()
            .find(|info| info.name == selector || info.name.eq_ignore_ascii_case(selector))
            .map(|info| Some(info.path))
            .ok_or_else(|| format!("Saved session '{}' was not found", selector));
    }
    if continue_latest {
        return session::list()
            .into_iter()
            .next()
            .map(|info| Some(info.path))
            .ok_or_else(|| "No saved sessions were found".to_string());
    }
    Ok(None)
}

fn seed_config_from_session(config: &mut AppConfig, path: Option<&PathBuf>) {
    if let Some(path) = path {
        if let Some(snapshot) = session::load(path) {
            config.select_provider(&snapshot.provider);
            config.model = snapshot.model;
        }
    }
}

fn parse_cli_args() -> Result<CliArgs, String> {
    let mut args = std::env::args().skip(1);
    let mut cli = CliArgs {
        prompt: None,
        chat: false,
        help: false,
        resume: None,
        continue_latest: false,
        provider: None,
        model: None,
        base_url: None,
        config_path: None,
        output_format: OutputFormat::Text,
        tool_policy: ToolPolicy::Ask,
        login: None,
    };
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-p" | "--prompt" => {
                cli.prompt = Some(
                    args.next()
                        .ok_or_else(|| "-p/--prompt requires a value".to_string())?,
                )
            }
            "--chat" => cli.chat = true,
            "--resume" => {
                cli.resume = Some(
                    args.next()
                        .ok_or_else(|| "--resume requires a session name or path".to_string())?,
                )
            }
            "--continue" => cli.continue_latest = true,
            "--provider" => {
                cli.provider = Some(
                    args.next()
                        .ok_or_else(|| "--provider requires a value".to_string())?,
                )
            }
            "--model" => {
                cli.model = Some(
                    args.next()
                        .ok_or_else(|| "--model requires a value".to_string())?,
                )
            }
            "--base-url" => {
                cli.base_url = Some(
                    args.next()
                        .ok_or_else(|| "--base-url requires a value".to_string())?,
                )
            }
            "--config" => {
                cli.config_path = Some(PathBuf::from(
                    args.next()
                        .ok_or_else(|| "--config requires a path".to_string())?,
                ))
            }
            "--format" => {
                cli.output_format = match args
                    .next()
                    .ok_or_else(|| "--format requires text or jsonl".to_string())?
                    .as_str()
                {
                    "text" => OutputFormat::Text,
                    "jsonl" => OutputFormat::Jsonl,
                    value => {
                        return Err(format!(
                            "Unknown output format '{}'. Use text or jsonl.",
                            value
                        ))
                    }
                }
            }
            "--tools" => {
                cli.tool_policy =
                    ToolPolicy::parse(&args.next().ok_or_else(|| {
                        "--tools requires ask, review, auto, or deny".to_string()
                    })?)?;
            }
            "--login" => {
                cli.login = Some(
                    args.next()
                        .ok_or_else(|| "--login requires browser or device".to_string())?,
                );
            }
            "-h" | "--help" => {
                cli.help = true;
                break;
            }
            unknown => return Err(format!("Unknown argument '{}'. Use --help.", unknown)),
        }
    }
    Ok(cli)
}

async fn attach_mcp_tools(app: &mut App) {
    let (mcp_tools, errors) = mcp::connect_all(&app.config.mcp_servers).await;
    for error in errors {
        app.add_message("system", format!("MCP connection failed: {error}"));
    }
    for tool in mcp_tools {
        let risk = mcp::tool_risk(tool.name());
        app.tool_registry.register_with_descriptor(
            tool,
            tools::ToolDescriptor::lazy(tools::ToolCategory::Integrations, risk),
        );
    }
}

async fn run_headless(cli: CliArgs) -> Result<(), Box<dyn std::error::Error>> {
    let Some(prompt) = cli.prompt else {
        return Err(
            "Missing prompt. Use -p \"prompt\" or launch without arguments for the TUI.".into(),
        );
    };
    let resume_path = resolve_session_selector(cli.resume.as_deref(), cli.continue_latest)
        .map_err(|error| io::Error::new(io::ErrorKind::NotFound, error))?;
    let mut config = AppConfig::load();
    seed_config_from_session(&mut config, resume_path.as_ref());
    if let Some(provider) = cli.provider.as_deref() {
        config.select_provider(provider);
    }
    if let Some(model) = cli.model.as_deref() {
        config.model = model.to_string();
    }
    if let Some(base_url) = cli.base_url.as_deref() {
        let value = if base_url.eq_ignore_ascii_case("default") {
            None
        } else {
            Some(base_url.to_string())
        };
        if let Some(provider) = config.providers.get_mut(&config.provider) {
            provider.base_url = value;
        } else {
            config.base_url = value;
        }
    }
    refresh_codex_auth_if_available(&config).await;
    let api_key = resolve_api_key(&config)?;
    let mut app = App::new(config, api_key);
    app.interaction.headless = true;
    attach_mcp_tools(&mut app).await;
    if let Some(path) = resume_path {
        app.restore_session(&path)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        // Explicit CLI provider/model/base-url choices take precedence over
        // the restored snapshot, matching normal command-line expectations.
        if let Some(provider) = cli.provider.as_deref() {
            app.config.select_provider(provider);
        }
        if let Some(model) = cli.model.as_deref() {
            app.config.model = model.to_string();
        }
        if let Some(base_url) = cli.base_url.as_deref() {
            let value = (!base_url.eq_ignore_ascii_case("default")).then(|| base_url.to_string());
            if let Some(provider) = app.config.providers.get_mut(&app.config.provider) {
                provider.base_url = value;
            } else {
                app.config.base_url = value;
            }
        }
        app.refresh_client_from_config();
    }
    app.add_message("user", prompt);
    if cli.output_format == OutputFormat::Jsonl {
        return run_headless_jsonl(&mut app, cli.tool_policy, None, None).await;
    }
    let request = app.build_request();
    let result = app
        .client
        .generate_content_with_fallback(
            &app.config.model,
            &app.config.effective_fallback_models(),
            app.config.max_retries,
            &request,
        )
        .await?;
    println!("{}", result);
    app.add_message("model", result);
    let _ = app.flush_session();
    Ok(())
}

async fn run_login(mode: &str) -> Result<(), Box<dyn std::error::Error>> {
    let mode = codex_auth::parse_login_mode(Some(mode))?;
    let auth = codex_auth::login(mode)
        .await
        .map_err(codex_auth::login_error)?;
    let mut config = AppConfig::load();
    config.select_provider("codex");
    config.save().map_err(io::Error::other)?;
    println!(
        "Signed in to Codex{}.",
        if auth.account_id.is_empty() {
            String::new()
        } else {
            format!(" ({})", auth.account_id)
        }
    );
    Ok(())
}

fn emit_jsonl(run_id: &str, sequence: &mut u64, event: &str, data: Value) -> io::Result<()> {
    let record = json!({
        "version": 1,
        "sequence": *sequence,
        "run_id": run_id,
        "timestamp": chrono::Utc::now().to_rfc3339(),
        "event": event,
        "data": data,
    });
    *sequence += 1;
    println!("{}", record);
    io::stdout().flush()
}

fn new_run_id() -> String {
    static RUN_COUNTER: AtomicU64 = AtomicU64::new(0);
    let counter = RUN_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!(
        "{}-{}-{}",
        chrono::Utc::now().format("%Y%m%dT%H%M%S%.3fZ"),
        std::process::id(),
        counter
    )
}

fn approval_payload(app: &App, approval_id: &str, policy: &str) -> Value {
    let Some(pending) = app.pending_tool_call.as_ref() else {
        return json!({
            "approval_id": approval_id,
            "policy": policy,
            "risk": "high",
            "confirmation": "text_or_keybind_or_ui"
        });
    };
    let risk = if pending.preview.is_mutation {
        "high"
    } else {
        "low"
    };
    json!({
        "approval_id": approval_id,
        "policy": policy,
        "action": "confirm",
        "risk": risk,
        "confirmation": if risk == "high" || app.pending_requires_review() { "text_or_keybind_or_ui" } else { "voice_or_keybind" },
        "tool": pending.tool_name,
        "call_id": pending.call_id,
        "args": pending.args,
        "title": pending.preview.title,
        "details": pending.preview.details,
        "reason": pending.preview.reason,
        "expected_effect": pending.preview.expected_effect,
        "command": pending.preview.command
    })
}

async fn run_headless_jsonl(
    app: &mut App,
    tool_policy: ToolPolicy,
    input_metadata: Option<Value>,
    input_rx: Option<&mut mpsc::UnboundedReceiver<String>>,
) -> Result<(), Box<dyn std::error::Error>> {
    if tool_policy == ToolPolicy::Review {
        app.auto_mode = true;
    }
    let run_id = new_run_id();
    let mut sequence = 0;
    emit_jsonl(
        &run_id,
        &mut sequence,
        "run_started",
        json!({
            "provider": app.config.provider,
            "model": app.config.model,
            "input": input_metadata.clone()
        }),
    )?;
    emit_jsonl(
        &run_id,
        &mut sequence,
        "user_message",
        json!({
            "content": app.messages.last().map(|message| message.content.clone()).unwrap_or_default(),
            "input": input_metadata
        }),
    )?;

    let (event_tx, mut event_rx) = mpsc::unbounded_channel::<AppEvent>();
    let mut final_status = "ok";
    let mut generation_finished = false;
    let mut response = String::new();
    let mut pending_approval_id: Option<String> = None;
    let mut pending_capability_id: Option<String> = None;
    let mut input_rx = input_rx;
    app.ensure_models_loaded().await;
    app.trigger_generation(event_tx.clone());

    enum HeadlessEvent {
        App(Box<Option<AppEvent>>),
        Input(Option<String>),
    }

    'run: loop {
        let event = if pending_approval_id.is_some() || pending_capability_id.is_some() {
            let Some(input_rx) = input_rx.as_deref_mut() else {
                app.deny_pending_tool(event_tx.clone());
                final_status = "error";
                break 'run;
            };
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {
                    app.cancel_generation();
                    final_status = "cancelled";
                    break 'run;
                }
                event = event_rx.recv() => HeadlessEvent::App(Box::new(event)),
                line = input_rx.recv() => HeadlessEvent::Input(line),
            }
        } else {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {
                    app.cancel_generation();
                    final_status = "cancelled";
                    break 'run;
                }
                event = event_rx.recv() => HeadlessEvent::App(Box::new(event)),
            }
        };

        if let HeadlessEvent::Input(line) = event {
            let Some(line) = line else {
                final_status = "cancelled";
                app.deny_pending_tool(event_tx.clone());
                break 'run;
            };
            let input = match parse_jsonl_input(&line) {
                Ok(input) => input,
                Err(error) => {
                    emit_jsonl(
                        &run_id,
                        &mut sequence,
                        "error",
                        json!({"message": error, "kind": "invalid_approval_input"}),
                    )?;
                    continue;
                }
            };
            if matches!(input.event.as_str(), "shutdown" | "exit") {
                final_status = "cancelled";
                if pending_approval_id.is_some() {
                    app.deny_pending_tool(event_tx.clone());
                }
                break 'run;
            }
            if pending_capability_id.is_some() {
                if input.event != "capability_response" {
                    emit_jsonl(
                        &run_id,
                        &mut sequence,
                        "input_ignored",
                        json!({"reason": "capability_pending", "event": input.event}),
                    )?;
                    continue;
                }
                let request_id = input
                    .metadata
                    .get("request_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if Some(request_id) != pending_capability_id.as_deref() {
                    emit_jsonl(
                        &run_id,
                        &mut sequence,
                        "capability_rejected",
                        json!({"reason": "request_id_mismatch", "request_id": request_id}),
                    )?;
                    continue;
                }
                let capability_name = input
                    .metadata
                    .get("capability")
                    .and_then(Value::as_str)
                    .unwrap_or("session capability")
                    .to_string();
                let result = input.metadata.get("result").cloned().unwrap_or_else(
                    || json!({"status":"error","message":"missing capability result"}),
                );
                let request_id = pending_capability_id.take().unwrap_or_default();
                emit_jsonl(
                    &run_id,
                    &mut sequence,
                    "capability_response",
                    json!({"request_id": request_id, "capability": capability_name, "result": result}),
                )?;
                app.handle_capability_result(
                    app.stream_epoch,
                    capability_name,
                    Some(request_id),
                    result,
                    event_tx.clone(),
                );
                continue;
            }
            if input.event != "approval_response" {
                emit_jsonl(
                    &run_id,
                    &mut sequence,
                    "input_ignored",
                    json!({"reason": "approval_pending", "event": input.event}),
                )?;
                continue;
            }

            let approval_id = input
                .metadata
                .get("approval_id")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if Some(approval_id) != pending_approval_id.as_deref() {
                emit_jsonl(
                    &run_id,
                    &mut sequence,
                    "approval_rejected",
                    json!({"reason": "approval_id_mismatch", "approval_id": approval_id}),
                )?;
                continue;
            }
            let approved = input
                .metadata
                .get("approved")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let method = input
                .metadata
                .get("method")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let high_risk = app
                .pending_tool_call
                .as_ref()
                .is_some_and(|pending| pending.preview.is_mutation);
            if approved
                && ((app.pending_requires_review() && !matches!(method, "text" | "keybind" | "ui"))
                    || (high_risk && method == "voice"))
            {
                emit_jsonl(
                    &run_id,
                    &mut sequence,
                    "approval_rejected",
                    json!({
                        "reason": "keybind_or_ui_required",
                        "approval_id": approval_id,
                        "risk": "high"
                    }),
                )?;
                app.deny_pending_tool(event_tx.clone());
            } else if approved {
                app.approve_pending_tool(false, event_tx.clone());
            } else {
                app.deny_pending_tool(event_tx.clone());
            }
            pending_approval_id = None;
            continue;
        }

        let HeadlessEvent::App(boxed_event) = event else {
            unreachable!("input handled above");
        };
        let Some(event) = *boxed_event else {
            break 'run;
        };

        match event {
            AppEvent::Stream { epoch, signal } => {
                let tool_call = matches!(
                    &signal,
                    StreamSignal::ToolCall { .. } | StreamSignal::ToolReviewed { .. }
                );
                match &signal {
                    StreamSignal::ModelSelected { model, protocol } => emit_jsonl(
                        &run_id,
                        &mut sequence,
                        "model_selected",
                        json!({"model":model,"protocol":protocol}),
                    )?,
                    StreamSignal::ThoughtDelta(delta) => emit_jsonl(
                        &run_id,
                        &mut sequence,
                        "reasoning_delta",
                        json!({"delta": delta, "visibility": "provider_summary"}),
                    )?,
                    StreamSignal::TextDelta(delta) => {
                        response.push_str(delta);
                        emit_jsonl(
                            &run_id,
                            &mut sequence,
                            "text_delta",
                            json!({"delta": delta}),
                        )?;
                    }
                    StreamSignal::ToolCall {
                        id,
                        name,
                        args,
                        thought_signature,
                    } => emit_jsonl(
                        &run_id,
                        &mut sequence,
                        "tool_call",
                        json!({"id": id, "name": name, "args": args, "thought_signature": thought_signature}),
                    )?,
                    StreamSignal::Usage {
                        prompt_tokens,
                        candidates_tokens,
                        total_tokens,
                        details,
                    } => emit_jsonl(
                        &run_id,
                        &mut sequence,
                        "usage",
                        json!({"prompt_tokens": prompt_tokens, "candidates_tokens": candidates_tokens, "total_tokens": total_tokens, "details": details}),
                    )?,
                    StreamSignal::Finished { .. } => {
                        generation_finished = true;
                    }
                    StreamSignal::ToolReviewed { pending, result } => {
                        emit_jsonl(
                            &run_id,
                            &mut sequence,
                            "auto_review",
                            json!({
                                "tool": pending.tool_name, "call_id": pending.call_id,
                                "result": result, "auto_approved": result.as_ref().is_ok_and(|d| d.may_approve())
                            }),
                        )?;
                    }
                    StreamSignal::Notice(message) => emit_jsonl(
                        &run_id,
                        &mut sequence,
                        "notice",
                        json!({"message": message}),
                    )?,
                    StreamSignal::Error(error) => {
                        final_status = "error";
                        emit_jsonl(&run_id, &mut sequence, "error", json!({"message": error}))?;
                    }
                }
                app.handle_stream_signal(epoch, signal, event_tx.clone());

                if tool_call && app.state == EngineState::AwaitingHitlApproval {
                    let policy_name = match tool_policy {
                        ToolPolicy::Ask => "ask",
                        ToolPolicy::Auto => "auto",
                        ToolPolicy::Review => "review",
                        ToolPolicy::Deny => "deny",
                    };
                    if tool_policy == ToolPolicy::Auto && !app.pending_requires_review() {
                        app.approve_pending_tool(true, event_tx.clone());
                    } else if tool_policy == ToolPolicy::Deny {
                        app.deny_pending_tool(event_tx.clone());
                    } else {
                        let approval_id = app
                            .pending_tool_call
                            .as_ref()
                            .and_then(|pending| pending.call_id.clone())
                            .unwrap_or_else(|| format!("{run_id}:approval:{sequence}"));
                        emit_jsonl(
                            &run_id,
                            &mut sequence,
                            "approval_required",
                            approval_payload(app, &approval_id, policy_name),
                        )?;
                        pending_approval_id = Some(approval_id);
                    }
                }
                if generation_finished
                    && app.state == EngineState::Idle
                    && app.pending_tool_call.is_none()
                    && app.pending_tool_executions == 0
                {
                    break;
                }
            }
            AppEvent::ToolExecutionResult {
                epoch,
                tool_name,
                call_id,
                result,
            } => {
                let result_data = match &result {
                    Ok(output) => json!({"tool": tool_name, "output": output}),
                    Err(error) => json!({"tool": tool_name, "error": error}),
                };
                emit_jsonl(&run_id, &mut sequence, "tool_result", result_data)?;
                app.handle_tool_result(epoch, tool_name.clone(), call_id, result, event_tx.clone());
                if app.state == EngineState::AwaitingHitlApproval {
                    if tool_policy == ToolPolicy::Auto && !app.pending_requires_review() {
                        app.approve_pending_tool(true, event_tx.clone());
                    } else if tool_policy == ToolPolicy::Deny {
                        app.deny_pending_tool(event_tx.clone());
                    } else {
                        let approval_id = app
                            .pending_tool_call
                            .as_ref()
                            .and_then(|pending| pending.call_id.clone())
                            .unwrap_or_else(|| format!("{run_id}:approval:{sequence}"));
                        emit_jsonl(
                            &run_id,
                            &mut sequence,
                            "approval_required",
                            approval_payload(app, &approval_id, "ask"),
                        )?;
                        pending_approval_id = Some(approval_id);
                    }
                }
                if app.state == EngineState::Streaming {
                    generation_finished = false;
                }
            }
            AppEvent::CapabilityRequest {
                capability_name,
                call_id,
                args,
                ..
            } => {
                let request_id =
                    call_id.unwrap_or_else(|| format!("{run_id}:capability:{sequence}"));
                emit_jsonl(
                    &run_id,
                    &mut sequence,
                    "capability_request",
                    json!({"request_id": request_id, "capability": capability_name, "args": args}),
                )?;
                pending_capability_id = Some(request_id);
            }
            AppEvent::CompactionFinished(epoch, result) => {
                emit_jsonl(
                    &run_id,
                    &mut sequence,
                    "compaction",
                    json!({"status": if result.is_ok() { "ok" } else { "error" }}),
                )?;
                if epoch != app.stream_epoch {
                    continue;
                }
                app.handle_compaction_result(epoch, result, event_tx.clone());
                if app.state == EngineState::Idle {
                    final_status = "error";
                    break;
                }
                generation_finished = false;
            }
            AppEvent::SystemNotification(message) => emit_jsonl(
                &run_id,
                &mut sequence,
                "notice",
                json!({"message": message}),
            )?,
            AppEvent::ModelsFetched { .. }
            | AppEvent::Paste(_)
            | AppEvent::Key(_)
            | AppEvent::Mouse(_)
            | AppEvent::Resize(_, _)
            | AppEvent::ProviderUsageFetched(_) => {}
        }
        if final_status == "error" {
            break;
        }
    }
    emit_jsonl(
        &run_id,
        &mut sequence,
        "response_final",
        json!({"content": response, "complete": final_status == "ok"}),
    )?;
    emit_jsonl(
        &run_id,
        &mut sequence,
        "run_finished",
        json!({"status": final_status}),
    )?;
    Ok(())
}

async fn run_text_generation(
    app: &mut App,
    tx: &mpsc::UnboundedSender<AppEvent>,
    rx: &mut mpsc::UnboundedReceiver<AppEvent>,
) {
    app.ensure_models_loaded().await;
    app.trigger_generation(tx.clone());
    let mut saw_finished = false;

    loop {
        let event = tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                if app.state == EngineState::Streaming {
                    app.cancel_generation();
                }
                break;
            }
            event = rx.recv() => event,
        };
        let Some(event) = event else {
            break;
        };
        match event {
            AppEvent::Stream { epoch, signal } => {
                let approval_needed = matches!(
                    &signal,
                    StreamSignal::ToolCall { .. } | StreamSignal::ToolReviewed { .. }
                );
                match &signal {
                    StreamSignal::TextDelta(delta) => {
                        print!("{}", delta);
                        let _ = io::stdout().flush();
                    }
                    StreamSignal::ModelSelected { .. } | StreamSignal::ThoughtDelta(_) => {}
                    StreamSignal::ToolCall { name, args, .. } => {
                        println!("\n[tool request: {} {}]", name, args);
                    }
                    StreamSignal::Usage { .. } => {}
                    StreamSignal::Finished { .. } => {
                        saw_finished = true;
                        println!();
                    }
                    StreamSignal::ToolReviewed { result, .. } => {
                        eprintln!("auto-review: {:?}", result);
                    }
                    StreamSignal::Notice(message) => {
                        eprintln!("notice: {}", message);
                    }
                    StreamSignal::Error(error) => {
                        saw_finished = true;
                        eprintln!("error: {}", error);
                    }
                }
                app.handle_stream_signal(epoch, signal, tx.clone());

                if approval_needed && app.state == EngineState::AwaitingHitlApproval {
                    if let Some(pending) = &app.pending_tool_call {
                        println!("Tool: {}", pending.tool_name);
                        for detail in &pending.preview.details {
                            println!("  {}", detail);
                        }
                    }
                    print!("Approve? [y]es/[a]lways/[n]o: ");
                    let _ = io::stdout().flush();
                    let mut answer = String::new();
                    if io::stdin().read_line(&mut answer).is_ok()
                        && answer.trim().eq_ignore_ascii_case("a")
                    {
                        app.approve_pending_tool(true, tx.clone());
                    } else if answer.trim().eq_ignore_ascii_case("y") {
                        app.approve_pending_tool(false, tx.clone());
                    } else {
                        app.deny_pending_tool(tx.clone());
                    }
                }
            }
            AppEvent::ToolExecutionResult {
                epoch,
                tool_name,
                call_id,
                result,
            } => {
                println!("\n[tool finished: {}]", tool_name);
                app.handle_tool_result(epoch, tool_name, call_id, result, tx.clone());
                if app.state == EngineState::Streaming {
                    saw_finished = false;
                }
            }
            AppEvent::CapabilityRequest { .. } => {
                eprintln!("capability requests require the JSONL frontend");
            }
            AppEvent::CompactionFinished(epoch, result) => {
                app.handle_compaction_result(epoch, result, tx.clone());
                saw_finished = app.state == EngineState::Idle;
            }
            AppEvent::SystemNotification(message) => eprintln!("{}", message),
            AppEvent::ModelsFetched { .. }
            | AppEvent::Paste(_)
            | AppEvent::Key(_)
            | AppEvent::Mouse(_)
            | AppEvent::Resize(_, _)
            | AppEvent::ProviderUsageFetched(_) => {}
        }

        if saw_finished
            && app.state == EngineState::Idle
            && app.pending_tool_call.is_none()
            && app.pending_tool_executions == 0
        {
            break;
        }
    }
}

#[derive(Debug)]
struct JsonlInput {
    event: String,
    text: Option<String>,
    metadata: Value,
}

fn parse_jsonl_input(line: &str) -> Result<JsonlInput, String> {
    let value: Value = serde_json::from_str(line).map_err(|error| error.to_string())?;
    let object = value
        .as_object()
        .ok_or_else(|| "input must be a JSON object".to_string())?;
    if let Some(version) = object.get("version") {
        if version.as_u64() != Some(1) {
            return Err("unsupported JSONL input version; expected 1".to_string());
        }
    }
    let event = object
        .get("event")
        .and_then(Value::as_str)
        .ok_or_else(|| "input is missing string field 'event'".to_string())?
        .to_ascii_lowercase();
    let data = object.get("data").and_then(Value::as_object);
    let text = data
        .and_then(|data| data.get("text"))
        .or_else(|| object.get("text"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let metadata = data
        .map(|data| Value::Object(data.clone()))
        .unwrap_or_else(|| {
            let mut metadata = serde_json::Map::new();
            for (key, value) in object {
                if key != "version" && key != "event" && key != "text" {
                    metadata.insert(key.clone(), value.clone());
                }
            }
            Value::Object(metadata)
        });
    Ok(JsonlInput {
        event,
        text,
        metadata,
    })
}

async fn run_jsonl_chat(cli: CliArgs) -> Result<(), Box<dyn std::error::Error>> {
    let _ = crossterm::terminal::disable_raw_mode();
    let resume_path = resolve_session_selector(cli.resume.as_deref(), cli.continue_latest)
        .map_err(|error| io::Error::new(io::ErrorKind::NotFound, error))?;
    let mut config = AppConfig::load();
    seed_config_from_session(&mut config, resume_path.as_ref());
    if let Some(provider) = cli.provider.as_deref() {
        config.select_provider(provider);
    }
    if let Some(model) = cli.model.as_deref() {
        config.model = model.to_string();
    }
    if let Some(base_url) = cli.base_url.as_deref() {
        let value = (!base_url.eq_ignore_ascii_case("default")).then(|| base_url.to_string());
        if let Some(provider) = config.providers.get_mut(&config.provider) {
            provider.base_url = value;
        } else {
            config.base_url = value;
        }
    }
    refresh_codex_auth_if_available(&config).await;
    let api_key = resolve_api_key(&config)?;
    let mut app = App::new(config, api_key);
    app.interaction.headless = true;
    attach_mcp_tools(&mut app).await;
    if let Some(path) = resume_path {
        app.restore_session(&path)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        if let Some(provider) = cli.provider.as_deref() {
            app.config.select_provider(provider);
        }
        if let Some(model) = cli.model.as_deref() {
            app.config.model = model.to_string();
        }
        app.refresh_client_from_config();
    }

    let (input_tx, mut input_rx) = mpsc::unbounded_channel::<String>();
    tokio::spawn(async move {
        let mut stdin = tokio::io::BufReader::new(tokio::io::stdin());
        let mut line = String::new();
        while stdin.read_line(&mut line).await.unwrap_or(0) != 0 {
            if input_tx.send(std::mem::take(&mut line)).is_err() {
                break;
            }
        }
    });

    while let Some(line_text) = input_rx.recv().await {
        if line_text.trim().is_empty() {
            continue;
        }
        let input = match parse_jsonl_input(&line_text) {
            Ok(input) => input,
            Err(error) => {
                let input_run_id = new_run_id();
                let mut sequence = 0;
                emit_jsonl(
                    &input_run_id,
                    &mut sequence,
                    "error",
                    json!({"message": error, "kind": "invalid_input"}),
                )?;
                continue;
            }
        };

        match input.event.as_str() {
            "session_config" => {
                let session_id = input
                    .metadata
                    .get("session_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let system_instruction = input
                    .metadata
                    .get("system_instruction")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                let capabilities = input
                    .metadata
                    .get("capabilities")
                    .cloned()
                    .unwrap_or_else(|| Value::Array(Vec::new()));
                let preload_tools = input
                    .metadata
                    .get("preload_tools")
                    .and_then(Value::as_array)
                    .map(|tools| {
                        tools
                            .iter()
                            .filter_map(Value::as_str)
                            .map(str::to_owned)
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                let tool_profile = input
                    .metadata
                    .get("tool_profile")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                let disabled_tools = input
                    .metadata
                    .get("disabled_tools")
                    .and_then(Value::as_array)
                    .map(|tools| {
                        tools
                            .iter()
                            .filter_map(Value::as_str)
                            .map(str::to_owned)
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                let input_run_id = new_run_id();
                let mut sequence = 0;
                if session_id.is_empty() {
                    emit_jsonl(
                        &input_run_id,
                        &mut sequence,
                        "error",
                        json!({
                            "message": "session_config requires data.session_id",
                            "kind": "invalid_session_config"
                        }),
                    )?;
                } else {
                    app.set_session_instruction(system_instruction);
                    app.set_session_tool_profile(tool_profile.clone());
                    if let Err(error) = app.set_session_capabilities(&capabilities) {
                        emit_jsonl(
                            &input_run_id,
                            &mut sequence,
                            "error",
                            json!({"message": error, "kind": "invalid_session_config"}),
                        )?;
                        continue;
                    }
                    app.preload_session_tools(&preload_tools);
                    app.set_session_disabled_tools(&disabled_tools);
                    emit_jsonl(
                        &input_run_id,
                        &mut sequence,
                        "session_configured",
                        json!({
                            "status": "ok",
                            "session_id": session_id,
                            "capabilities": capabilities,
                            "preload_tools": preload_tools,
                            "tool_profile": tool_profile,
                            "disabled_tools": disabled_tools
                        }),
                    )?;
                }
            }
            "speech" | "message" | "user_message" => {
                let Some(text) = input.text.map(|text| text.trim().to_string()) else {
                    let input_run_id = new_run_id();
                    let mut sequence = 0;
                    emit_jsonl(
                        &input_run_id,
                        &mut sequence,
                        "input_ignored",
                        json!({"reason": "missing_text", "input": input.metadata}),
                    )?;
                    continue;
                };
                if text.is_empty() {
                    let input_run_id = new_run_id();
                    let mut sequence = 0;
                    emit_jsonl(
                        &input_run_id,
                        &mut sequence,
                        "input_ignored",
                        json!({"reason": "empty_text", "input": input.metadata}),
                    )?;
                    continue;
                }
                app.add_message("user", text);
                let _ = app.flush_session();
                run_headless_jsonl(
                    &mut app,
                    cli.tool_policy,
                    Some(input.metadata),
                    Some(&mut input_rx),
                )
                .await?;
                let _ = app.flush_session();
            }
            "ping" => {
                let input_run_id = new_run_id();
                let mut sequence = 0;
                emit_jsonl(
                    &input_run_id,
                    &mut sequence,
                    "pong",
                    json!({"input": input.metadata}),
                )?;
            }
            "new_session" | "reset_context" => {
                app.start_new_session();
                let input_run_id = new_run_id();
                let mut sequence = 0;
                emit_jsonl(
                    &input_run_id,
                    &mut sequence,
                    "session_reset",
                    json!({"status": "ok", "reason": input.metadata}),
                )?;
            }
            "partial" | "listening" | "level" => {
                let input_run_id = new_run_id();
                let mut sequence = 0;
                emit_jsonl(
                    &input_run_id,
                    &mut sequence,
                    "input_ignored",
                    json!({"reason": "event_not_submitted", "event": input.event, "input": input.metadata}),
                )?;
            }
            "shutdown" | "exit" => {
                let input_run_id = new_run_id();
                let mut sequence = 0;
                emit_jsonl(
                    &input_run_id,
                    &mut sequence,
                    "shutdown",
                    json!({"status": "ok"}),
                )?;
                break;
            }
            event => {
                let input_run_id = new_run_id();
                let mut sequence = 0;
                emit_jsonl(
                    &input_run_id,
                    &mut sequence,
                    "error",
                    json!({"message": format!("unsupported input event '{}'; expected speech or shutdown", event), "kind": "unsupported_event"}),
                )?;
            }
        }
    }
    let _ = app.flush_session();
    Ok(())
}

async fn run_chat(cli: CliArgs) -> Result<(), Box<dyn std::error::Error>> {
    // `--chat` may be launched after the TUI was force-terminated. In that
    // case Windows can leave the console in raw/no-echo mode, so restore the
    // normal line-input and echo flags before reading stdin.
    let _ = crossterm::terminal::disable_raw_mode();
    let resume_path = resolve_session_selector(cli.resume.as_deref(), cli.continue_latest)
        .map_err(|error| io::Error::new(io::ErrorKind::NotFound, error))?;
    let mut config = AppConfig::load();
    seed_config_from_session(&mut config, resume_path.as_ref());
    if let Some(provider) = cli.provider.as_deref() {
        config.select_provider(provider);
    }
    if let Some(model) = cli.model.as_deref() {
        config.model = model.to_string();
    }
    if let Some(base_url) = cli.base_url.as_deref() {
        let value = if base_url.eq_ignore_ascii_case("default") {
            None
        } else {
            Some(base_url.to_string())
        };
        if let Some(provider) = config.providers.get_mut(&config.provider) {
            provider.base_url = value;
        } else {
            config.base_url = value;
        }
    }
    refresh_codex_auth_if_available(&config).await;
    let api_key = resolve_api_key(&config)?;
    let mut app = App::new(config, api_key);
    app.interaction.headless = true;
    attach_mcp_tools(&mut app).await;
    if let Some(path) = resume_path {
        app.restore_session(&path)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        if let Some(provider) = cli.provider.as_deref() {
            app.config.select_provider(provider);
        }
        if let Some(model) = cli.model.as_deref() {
            app.config.model = model.to_string();
        }
        app.refresh_client_from_config();
    }
    let stdin = io::stdin();
    let mut line = String::new();
    let (command_tx, mut command_rx) = mpsc::unbounded_channel::<AppEvent>();
    loop {
        print!("> ");
        io::stdout().flush()?;
        line.clear();
        if stdin.read_line(&mut line)? == 0 {
            break;
        }
        let prompt = line.trim();
        if prompt.is_empty() {
            continue;
        }
        if prompt.eq_ignore_ascii_case("/quit") || prompt.eq_ignore_ascii_case("/exit") {
            break;
        }
        if prompt.starts_with('/') {
            let before = app.messages.len();
            app.handle_slash_command(prompt, command_tx.clone());
            while let Ok(event) = command_rx.try_recv() {
                match event {
                    AppEvent::CompactionFinished(epoch, result) => {
                        app.handle_compaction_result(epoch, result, command_tx.clone());
                    }
                    AppEvent::ModelsFetched {
                        result: Ok(models),
                        epoch,
                        ..
                    } if epoch == app.models_epoch => {
                        app.install_models(epoch, models);
                    }
                    AppEvent::ModelsFetched {
                        result: Err(error), ..
                    } => {
                        eprintln!("error: failed to fetch models: {}", error);
                    }
                    _ => {}
                }
            }
            if app.state == EngineState::Compacting {
                if let Some(AppEvent::CompactionFinished(epoch, result)) = command_rx.recv().await {
                    app.handle_compaction_result(epoch, result, command_tx.clone());
                }
            }
            if app.state == EngineState::Streaming {
                run_text_generation(&mut app, &command_tx, &mut command_rx).await;
            }
            for message in app.messages.iter().skip(before) {
                if message.role == "system" {
                    println!("{}", message.content);
                }
            }
            if app.should_quit {
                break;
            }
            continue;
        }
        app.add_message("user", prompt.to_string());
        let _ = app.flush_session();
        run_text_generation(&mut app, &command_tx, &mut command_rx).await;
    }
    let _ = app.flush_session();
    Ok(())
}

fn reset_terminal() {
    let _ = disable_raw_mode();
    let _ = execute!(
        stdout(),
        DisableBracketedPaste,
        LeaveAlternateScreen,
        DisableMouseCapture
    );
    let _ = execute!(stdout(), crossterm::cursor::Show);
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = parse_cli_args().map_err(|error| {
        if error.starts_with("Usage:") {
            eprintln!("{}", error);
        }
        io::Error::new(io::ErrorKind::InvalidInput, error)
    })?;
    if cli.help {
        println!("Usage: holiday.exe [--login browser|device] | -p \"prompt\" [--format text|jsonl] [--tools ask|review|auto|deny] [--resume NAME|PATH | --continue] | --chat [--format text|jsonl] [--resume NAME|PATH | --continue] [--config PATH] [--provider NAME] [--model MODEL] [--base-url URL]");
        return Ok(());
    }
    if let Some(path) = &cli.config_path {
        std::env::set_var("HOLIDAY_CONFIG", path);
    }
    if let Some(mode) = cli.login.as_deref() {
        return run_login(mode).await;
    }
    if cli.prompt.is_some() {
        return run_headless(cli).await;
    }
    if cli.chat {
        if cli.output_format == OutputFormat::Jsonl {
            return run_jsonl_chat(cli).await;
        }
        return run_chat(cli).await;
    }
    // 1. Install panic hook to ensure terminal is restored cleanly on panic
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        reset_terminal();
        original_hook(panic_info);
    }));

    // 2. Load configuration. The TUI is usable before a provider key is set;
    // requests will report the provider's authentication error when submitted.
    let config = AppConfig::load();
    refresh_codex_auth_if_available(&config).await;
    let api_key = config.get_api_key_for_active_provider().unwrap_or_default();

    // 3. Initialize Terminal in Raw Mode & Alternate Screen
    enable_raw_mode()?;
    let mut stdout_handle = stdout();
    execute!(
        stdout_handle,
        EnterAlternateScreen,
        EnableMouseCapture,
        EnableBracketedPaste
    )?;
    let backend = CrosstermBackend::new(stdout_handle);
    let mut terminal = Terminal::new(backend)?;

    // 5. Initialize App state and channels
    let mut app = App::new(config, api_key);
    attach_mcp_tools(&mut app).await;
    let (tx, mut rx) = mpsc::unbounded_channel::<AppEvent>();
    app.prefetch_models(tx.clone());

    // 6. Spawn input event listener thread
    let event_tx = tx.clone();
    std::thread::spawn(move || loop {
        if event_tx.is_closed() {
            break;
        }
        if event::poll(Duration::from_millis(16)).unwrap_or(false) {
            match event::read() {
                Ok(Event::Key(key)) => {
                    let _ = event_tx.send(AppEvent::Key(key));
                }
                Ok(Event::Paste(data)) => {
                    let _ = event_tx.send(AppEvent::Paste(data));
                }
                Ok(Event::Mouse(mouse)) => {
                    let _ = event_tx.send(AppEvent::Mouse(mouse));
                }
                Ok(Event::Resize(w, h)) => {
                    let _ = event_tx.send(AppEvent::Resize(w, h));
                }
                _ => {}
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    });

    // 7. Dirty-state rendering with a bounded animation tick.
    let mut render_interval = tokio::time::interval(Duration::from_millis(80)); // animations at most 12.5fps
    let mut usage_interval = tokio::time::interval(Duration::from_secs(60));

    loop {
        tokio::select! {
            _ = render_interval.tick() => {
                if app.interaction.external_editor_requested { edit_external_draft(&mut app); }
                if app.interaction.dirty || app.state != EngineState::Idle || app.tool_registry.tasks.running() {
                    terminal.draw(|f| ui::render(&mut app, f))?;
                    app.interaction.dirty=false;
                }
                app.send_queued_prompt(tx.clone());
            }
            _ = usage_interval.tick() => {
                // Clone the current client at refresh time so provider switches
                // do not leave the status bar querying the previous provider.
                let usage_client = app.client.clone();
                let usage_tx = tx.clone();
                tokio::spawn(async move {
                    let _ = usage_tx.send(AppEvent::ProviderUsageFetched(usage_client.usage_summary().await));
                });
            }
            Some(app_event) = rx.recv() => {
                app.interaction.dirty=true;
                let mut current_event = app_event;
                loop {
                    match current_event {
                        AppEvent::Key(key) => {
                            if key.kind == crossterm::event::KeyEventKind::Press {
                                handle_key_event(&mut app, key, tx.clone());
                            }
                        }
                        AppEvent::Paste(data) => {
                            app.append_paste(data);
                        }
                        AppEvent::Mouse(mouse) => {
                            match mouse.kind {
                                crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left)
                                | crossterm::event::MouseEventKind::Drag(crossterm::event::MouseButton::Left) => {
                                    if let Some(chat_area) = app.chat_area {
                                        let col = mouse.column;
                                        let row = mouse.row;
                                        // Scrollbar column on right edge of chat box
                                        if col == chat_area.right().saturating_sub(1)
                                            && row >= chat_area.top().saturating_add(1)
                                            && row < chat_area.bottom().saturating_sub(1)
                                        {
                                            let track_height = chat_area.height.saturating_sub(2).max(1) as f64;
                                            let click_offset = (row.saturating_sub(chat_area.top().saturating_add(1))) as f64;
                                            let fraction = (click_offset / track_height).clamp(0.0, 1.0);
                                            // Top of track = oldest history (chat_max_scroll), bottom = latest (0)
                                            let target_scroll = ((1.0 - fraction) * app.chat_max_scroll as f64).round() as usize;
                                            app.chat_scroll = target_scroll.min(app.chat_max_scroll);
                                        }
                                    }
                                }
                                crossterm::event::MouseEventKind::ScrollUp => {
                                    if app.state == EngineState::AwaitingHitlApproval {
                                        app.modal_scroll = app.modal_scroll.saturating_sub(5);
                                    } else if app.interaction.is_overlay(crate::interaction::Overlay::Models) {
                                        app.models_scroll = app.models_scroll.saturating_sub(5);
                                    } else if app.interaction.is_overlay(crate::interaction::Overlay::Sessions) {
                                        app.sessions_selected = app.sessions_selected.saturating_sub(1);
                                    } else {
                                        app.chat_scroll = app.chat_scroll.saturating_add(5);
                                    }
                                }
                                crossterm::event::MouseEventKind::ScrollDown => {
                                    if app.state == EngineState::AwaitingHitlApproval {
                                        app.modal_scroll = app.modal_scroll.saturating_add(5);
                                    } else if app.interaction.is_overlay(crate::interaction::Overlay::Models) {
                                        app.models_scroll = app.models_scroll.saturating_add(5);
                                    } else if app.interaction.is_overlay(crate::interaction::Overlay::Sessions) {
                                        if !app.available_sessions.is_empty() { app.sessions_selected = (app.sessions_selected + 1).min(app.available_sessions.len() - 1); }
                                    } else {
                                        app.chat_scroll = app.chat_scroll.saturating_sub(5);
                                    }
                                }
                                _ => {}
                            }
                        }
                    AppEvent::Resize(_, _) => {}
                    AppEvent::Stream { epoch, signal } => {
                        app.handle_stream_signal(epoch, signal, tx.clone());
                    }
                    AppEvent::ToolExecutionResult { epoch, tool_name, call_id, result } => {
                        app.handle_tool_result(epoch, tool_name, call_id, result, tx.clone());
                    }
                    AppEvent::CapabilityRequest { .. } => {}
                    AppEvent::SystemNotification(msg) => {
                        app.add_message("system", msg);
                    }
                    AppEvent::ProviderUsageFetched(result) => {
                        app.provider_limits = result.ok();
                    }
                    AppEvent::ModelsFetched { epoch, result, interactive } if epoch == app.models_epoch => {
                        match result {
                            Ok(models) => {
                                app.install_models(epoch, models);
                                if app.config.provider.eq_ignore_ascii_case("opencode-zen")
                                    && app.config.get_api_key_for_active_provider().is_none()
                                {
                                    let current_model = app.config.model.clone();
                                    let free_fallbacks = app.available_models.iter()
                                        .filter(|model| model.id.to_ascii_lowercase().ends_with("-free"))
                                        .filter(|model| !is_zen_unsupported_model_id(&model.id))
                                        .map(|model| model.id.clone())
                                        .filter(|model| model != &current_model)
                                        .take(5)
                                        .collect::<Vec<_>>();
                                    if !free_fallbacks.is_empty() {
                                        if let Some(provider) = app.config.providers.get_mut(&app.config.provider) {
                                            if provider.fallback_models.is_empty() {
                                                provider.fallback_models = free_fallbacks;
                                            }
                                        }
                                    }
                                }
                                app.models_selected = app.available_models.iter().position(|model| model.id == app.config.model).unwrap_or(0);
                                app.models_filter.clear();
                                app.models_searching = false;
                                app.interaction.set_overlay(crate::interaction::Overlay::Models, interactive);
                                app.models_scroll = 0;
                                app.set_status(if interactive {
                                    "Fetched live models list."
                                } else {
                                    "Loaded provider model catalog."
                                });
                            }
                            Err(e) => {
                                if interactive {
                                    app.add_message("system", format!("Failed to fetch models: {}", e));
                                } else {
                                    app.set_status("Provider model catalog unavailable.");
                                }
                            }
                        }
                    }
                    AppEvent::ModelsFetched { .. } => {}
                    AppEvent::CompactionFinished(epoch, res) => {
                        app.handle_compaction_result(epoch, res, tx.clone());
                    }
                }

                match rx.try_recv() {
                    Ok(next) => current_event = next,
                    Err(_) => break,
                }
            }
        }
        }

        if app.should_quit {
            break;
        }
    }

    // 8. Stop managed work before dropping process supervisors and restore the terminal.
    app.cancel_generation();
    app.tool_registry.tasks.shutdown().await;
    let _ = app.flush_session();
    reset_terminal();
    Ok(())
}

fn handle_key_event(
    app: &mut App,
    key: crossterm::event::KeyEvent,
    tx: mpsc::UnboundedSender<AppEvent>,
) {
    use crate::interaction::Overlay;
    app.interaction.dirty = true;
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        match key.code {
            KeyCode::Char('k') => {
                app.open_palette();
                return;
            }
            KeyCode::Char('z') => {
                app.undo_draft(false);
                return;
            }
            KeyCode::Char('y') if app.interaction.top().is_none() => {
                app.undo_draft(true);
                return;
            }
            KeyCode::Char('r') => {
                app.handle_workspace_command(commands::CommandId::History, "", tx);
                return;
            }
            KeyCode::Char('o') if app.interaction.top().is_none() => {
                let index = app.interaction.focus_message.or_else(|| {
                    app.messages
                        .iter()
                        .rposition(|m| m.role == "tool" || m.role == "thought" || m.role == "model")
                });
                if let Some(index) = index {
                    app.handle_workspace_command(
                        commands::CommandId::Inspect,
                        &(index + 1).to_string(),
                        tx,
                    );
                }
                return;
            }
            KeyCode::Char('g') => {
                app.cancel_generation();
                return;
            }
            KeyCode::Left => {
                app.move_word(false);
                return;
            }
            KeyCode::Right => {
                app.move_word(true);
                return;
            }
            _ => {}
        }
    }
    if matches!(
        app.interaction.top(),
        Some(Overlay::Palette | Overlay::History | Overlay::Tasks | Overlay::Review)
    ) {
        let overlay = app.interaction.top().unwrap();
        match key.code {
            KeyCode::Esc => app.interaction.back(),
            KeyCode::Up => app.interaction.selected = app.interaction.selected.saturating_sub(1),
            KeyCode::Down => app.interaction.selected = app.interaction.selected.saturating_add(1),
            KeyCode::Backspace => {
                app.interaction.query.pop();
                app.interaction.selected = 0;
            }
            KeyCode::Char(c) => {
                app.interaction.query.push(c);
                app.interaction.selected = 0;
            }
            KeyCode::Enter => {
                if overlay == Overlay::Palette {
                    let choices = commands::matching(&app.interaction.query);
                    if let Some(spec) = choices.get(
                        app.interaction
                            .selected
                            .min(choices.len().saturating_sub(1)),
                    ) {
                        app.input_buffer = format!("{} ", spec.name);
                        app.input_cursor = app.input_buffer.chars().count();
                        app.interaction.back();
                    }
                } else if overlay == Overlay::History {
                    let choices = app
                        .input_history
                        .iter()
                        .rev()
                        .filter(|s| s.contains(&app.interaction.query))
                        .collect::<Vec<_>>();
                    if let Some(text) = choices.get(
                        app.interaction
                            .selected
                            .min(choices.len().saturating_sub(1)),
                    ) {
                        app.input_buffer = (*text).clone();
                        app.input_cursor = app.input_buffer.chars().count();
                        app.interaction.back();
                    }
                } else if overlay == Overlay::Tasks {
                    if let Some(id) = app.interaction.query.strip_prefix("cancel ") {
                        app.set_status(
                            app.tool_registry
                                .tasks
                                .cancel(id.trim())
                                .unwrap_or_else(|e| e),
                        );
                    }
                }
            }
            _ => {}
        }
        return;
    }
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Backspace {
        app.interaction
            .remember_edit(&app.input_buffer, app.input_cursor);
        let end = input_byte_index(&app.input_buffer, app.input_cursor);
        app.move_word(false);
        let start = input_byte_index(&app.input_buffer, app.input_cursor);
        app.input_buffer.replace_range(start..end, "");
        return;
    }
    if key.modifiers.contains(KeyModifiers::ALT) && matches!(key.code, KeyCode::Up | KeyCode::Down)
    {
        app.handle_workspace_command(
            commands::CommandId::Jump,
            if key.code == KeyCode::Up {
                "prev"
            } else {
                "next"
            },
            tx,
        );
        return;
    }
    // Ctrl+C clears the composer once, then exits only when pressed again.
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        app.handle_ctrl_c();
        return;
    }

    // Ctrl+V is intercepted here so clipboard images can become attachment
    // blocks; bracketed paste events below handle normal text paste.
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('v') {
        app.clipboard_paste();
        return;
    }

    if app
        .interaction
        .is_overlay(crate::interaction::Overlay::Permissions)
    {
        match key.code {
            KeyCode::Esc => {
                app.interaction
                    .set_overlay(crate::interaction::Overlay::Permissions, false);
                app.set_status("Using current tool permission defaults.");
            }
            KeyCode::Up => {
                app.permissions_selected = app.permissions_selected.saturating_sub(1);
                app.modal_scroll = 0;
            }
            KeyCode::Down => {
                app.permissions_selected = (app.permissions_selected + 1)
                    .min(config::TOOL_PERMISSION_GROUPS.len().saturating_sub(1));
                app.modal_scroll = 0;
            }
            KeyCode::Left => app.cycle_permission_mode(true),
            KeyCode::Right | KeyCode::Char(' ') => app.cycle_permission_mode(false),
            KeyCode::PageUp => app.modal_scroll = app.modal_scroll.saturating_sub(6),
            KeyCode::PageDown => app.modal_scroll = app.modal_scroll.saturating_add(6),
            KeyCode::Enter => app.finish_permission_setup(),
            _ => {}
        }
        return;
    }

    if app
        .interaction
        .is_overlay(crate::interaction::Overlay::Plan)
    {
        match key.code {
            KeyCode::Esc => {
                app.interaction
                    .set_overlay(crate::interaction::Overlay::Plan, false);
                app.set_status("Plan left for review");
            }
            KeyCode::Up | KeyCode::Char('1') => app.plan_modal_selected = 0,
            KeyCode::Down | KeyCode::Char('2') => app.plan_modal_selected = 1,
            KeyCode::Enter => {
                app.choose_plan_decision(app.plan_modal_selected == 0, tx);
            }
            _ => {}
        }
        return;
    }

    // 1. When Session Browser is active
    if app
        .interaction
        .is_overlay(crate::interaction::Overlay::Sessions)
    {
        match key.code {
            KeyCode::Esc => app
                .interaction
                .set_overlay(crate::interaction::Overlay::Sessions, false),
            KeyCode::Up => app.sessions_selected = app.sessions_selected.saturating_sub(1),
            KeyCode::Down => {
                if !app.available_sessions.is_empty() {
                    app.sessions_selected =
                        (app.sessions_selected + 1).min(app.available_sessions.len() - 1);
                }
            }
            KeyCode::Enter => app.resume_selected_session(),
            KeyCode::Char('d') | KeyCode::Char('D') => app.delete_selected_session(),
            KeyCode::Char('e') | KeyCode::Char('E') => app.export_selected_session(),
            _ => {}
        }
        return;
    }

    // Thinking picker sits above the model browser and edits the selected
    // provider/model profile without changing the active model itself.
    if app
        .interaction
        .is_overlay(crate::interaction::Overlay::Thinking)
    {
        let target_model = app
            .thinking_target_model
            .clone()
            .unwrap_or_else(|| app.config.model.clone());
        let max_choice = app.thinking_choices(&target_model).len().saturating_sub(1);
        match key.code {
            KeyCode::Esc => {
                app.interaction
                    .set_overlay(crate::interaction::Overlay::Thinking, false);
                app.thinking_target_model = None;
            }
            KeyCode::Up => app.thinking_selected = app.thinking_selected.saturating_sub(1),
            KeyCode::Down => app.thinking_selected = (app.thinking_selected + 1).min(max_choice),
            KeyCode::Char('0') => app.thinking_selected = 0,
            KeyCode::Char('1') => app.thinking_selected = 1,
            KeyCode::Char('2') => app.thinking_selected = 2,
            KeyCode::Char('3') => app.thinking_selected = 3,
            KeyCode::Enter => app.apply_thinking_modal_selection(),
            _ => {}
        }
        return;
    }

    // 2. When Live Models Modal is active
    if app
        .interaction
        .is_overlay(crate::interaction::Overlay::Models)
    {
        if app.models_searching {
            match key.code {
                KeyCode::Esc => {
                    app.models_searching = false;
                    app.models_filter.clear();
                    app.models_selected = 0;
                }
                KeyCode::Backspace => {
                    app.models_filter.pop();
                    app.models_selected = 0;
                }
                KeyCode::Enter => {
                    app.models_searching = false;
                    app.select_model_from_catalog();
                }
                KeyCode::Char(c) => {
                    app.models_filter.push(c);
                    app.models_selected = 0;
                }
                _ => {}
            }
            app.clamp_model_selection();
            return;
        }
        match key.code {
            KeyCode::Esc => {
                app.interaction
                    .set_overlay(crate::interaction::Overlay::Models, false);
            }
            KeyCode::Char('/') => {
                app.models_searching = true;
                app.models_filter.clear();
                app.models_selected = 0;
            }
            KeyCode::Enter => app.select_model_from_catalog(),
            KeyCode::Char('k') | KeyCode::Char('K') => app.open_thinking_modal_for_selected_model(),
            KeyCode::Up => {
                app.models_selected = app.models_selected.saturating_sub(1);
            }
            KeyCode::Down => {
                let count = app.filtered_model_indices().len();
                if count > 0 {
                    app.models_selected = (app.models_selected + 1).min(count - 1);
                }
            }
            KeyCode::PageUp => {
                app.models_selected = app.models_selected.saturating_sub(10);
            }
            KeyCode::PageDown => {
                let count = app.filtered_model_indices().len();
                if count > 0 {
                    app.models_selected = (app.models_selected + 10).min(count - 1);
                }
            }
            KeyCode::Char('r') | KeyCode::Char('R') => app.toggle_selected_reasoning(),
            KeyCode::Char('t') | KeyCode::Char('T') => app.adjust_selected_temperature(0.1),
            KeyCode::Char('-') => app.adjust_selected_max_tokens(-512),
            KeyCode::Char('+') | KeyCode::Char('=') => app.adjust_selected_max_tokens(512),
            KeyCode::Char('f') | KeyCode::Char('F') => app.toggle_selected_fallback(),
            KeyCode::Char('s') | KeyCode::Char('S') => {
                let _ = app.config.save();
                app.set_status("Model profile saved");
            }
            _ => {}
        }
        return;
    }

    // 3. When HITL Modal is active: gatekeeper authorization mode
    if app.state == EngineState::AwaitingHitlApproval {
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                app.approve_pending_tool(false, tx);
            }
            KeyCode::Char('a') | KeyCode::Char('A') => {
                app.approve_pending_tool(true, tx);
            }
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                app.deny_pending_tool(tx);
            }
            KeyCode::Up => {
                app.modal_scroll = app.modal_scroll.saturating_sub(1);
            }
            KeyCode::Down => {
                app.modal_scroll = app.modal_scroll.saturating_add(1);
            }
            KeyCode::PageUp => {
                app.modal_scroll = app.modal_scroll.saturating_sub(10);
            }
            KeyCode::PageDown => {
                app.modal_scroll = app.modal_scroll.saturating_add(10);
            }
            _ => {}
        }
        return;
    }

    // 4. When streaming: Esc cancels generation on the fly
    if matches!(
        app.state,
        EngineState::Streaming | EngineState::Compacting | EngineState::ExecutingTool
    ) && key.code == KeyCode::Esc
    {
        app.cancel_generation();
        return;
    }

    // Modifiers-based chat scroll
    if key.modifiers.contains(KeyModifiers::SHIFT) || key.modifiers.contains(KeyModifiers::ALT) {
        match key.code {
            KeyCode::Up => {
                app.chat_scroll = app.chat_scroll.saturating_add(3);
                return;
            }
            KeyCode::Down => {
                app.chat_scroll = app.chat_scroll.saturating_sub(3);
                return;
            }
            _ => {}
        }
    }

    // Terminals disagree about Shift+Enter: some send Enter with SHIFT,
    // others send a literal newline/carriage-return character. Accept all
    // forms before the Ctrl-only dispatch below.
    if (key.modifiers.contains(KeyModifiers::SHIFT) || key.modifiers.contains(KeyModifiers::ALT))
        && matches!(
            key.code,
            KeyCode::Enter | KeyCode::Char('\n') | KeyCode::Char('\r')
        )
    {
        app.insert_input_text("\n");
        return;
    }

    if key.modifiers.contains(KeyModifiers::CONTROL) {
        match key.code {
            // Terminals may report Ctrl+J as lowercase/uppercase J, a
            // newline, or Enter. Accept all forms, plus Ctrl+Enter.
            KeyCode::Char('j')
            | KeyCode::Char('J')
            | KeyCode::Char('\n')
            | KeyCode::Char('\r')
            | KeyCode::Enter => {
                app.insert_input_text("\n");
                return;
            }
            KeyCode::Char('x') => {
                if app.draft_attachments.is_empty() {
                    app.set_status("No draft attachment block to remove");
                } else {
                    app.remove_last_attachment();
                }
                return;
            }
            KeyCode::Up => {
                app.chat_scroll = app.chat_scroll.saturating_add(6);
                return;
            }
            KeyCode::Down => {
                app.chat_scroll = app.chat_scroll.saturating_sub(6);
                return;
            }
            KeyCode::Char('u') => {
                app.chat_scroll = app.chat_scroll.saturating_add(12);
                return;
            }
            KeyCode::Char('d') => {
                app.chat_scroll = app.chat_scroll.saturating_sub(12);
                return;
            }
            KeyCode::PageUp => {
                app.chat_scroll = app.chat_scroll.saturating_add(20);
                return;
            }
            KeyCode::PageDown => {
                app.chat_scroll = app.chat_scroll.saturating_sub(20);
                return;
            }
            KeyCode::Home => {
                app.chat_scroll = usize::MAX;
                return;
            }
            KeyCode::End => {
                app.chat_scroll = 0;
                return;
            }
            KeyCode::Char('y') => {
                app.copy_selection("", tx);
                return;
            }
            // Command history navigation via standard Ctrl+P / Ctrl+N
            KeyCode::Char('p') => {
                if !app.input_history.is_empty() {
                    let next_idx = match app.input_history_idx {
                        None => app.input_history.len().saturating_sub(1),
                        Some(i) => i.saturating_sub(1),
                    };
                    app.input_history_idx = Some(next_idx);
                    if let Some(cmd) = app.input_history.get(next_idx) {
                        app.input_buffer = cmd.clone();
                        app.input_cursor = app.input_buffer.chars().count();
                    }
                }
                return;
            }
            KeyCode::Char('n') => {
                if let Some(i) = app.input_history_idx {
                    if i + 1 < app.input_history.len() {
                        let next_idx = i + 1;
                        app.input_history_idx = Some(next_idx);
                        if let Some(cmd) = app.input_history.get(next_idx) {
                            app.input_buffer = cmd.clone();
                            app.input_cursor = app.input_buffer.chars().count();
                        }
                    } else {
                        app.input_history_idx = None;
                        app.input_buffer.clear();
                        app.input_cursor = 0;
                    }
                }
                return;
            }
            _ => {}
        }
    }

    let suggestions = app.slash_suggestions();
    if !suggestions.is_empty() && key.modifiers.is_empty() {
        let selected = app
            .interaction
            .suggestion_selected
            .min(suggestions.len() - 1);
        match key.code {
            KeyCode::Up => {
                app.interaction.suggestion_selected = selected.saturating_sub(1);
                return;
            }
            KeyCode::Down => {
                app.interaction.suggestion_selected = (selected + 1).min(suggestions.len() - 1);
                return;
            }
            KeyCode::Tab => {
                app.input_buffer = format!("{} ", suggestions[selected].name);
                app.input_cursor = app.input_buffer.chars().count();
                app.interaction.suggestion_selected = 0;
                return;
            }
            KeyCode::Enter => {
                let chosen = suggestions[selected];
                if chosen.arguments == crate::commands::Arguments::Text
                    || chosen.arguments == crate::commands::Arguments::Path
                {
                    app.input_buffer = format!("{} ", chosen.name);
                    app.input_cursor = app.input_buffer.chars().count();
                    app.interaction.suggestion_selected = 0;
                } else {
                    app.input_buffer = chosen.name.to_string();
                    app.input_cursor = app.input_buffer.chars().count();
                    app.interaction.suggestion_selected = 0;
                    app.handle_enter(tx);
                }
                return;
            }
            KeyCode::Esc => {
                app.interaction.suggestions_dismissed = true;
                return;
            }
            _ => {}
        }
    }
    if matches!(
        key.code,
        KeyCode::Char(_) | KeyCode::Backspace | KeyCode::Delete
    ) {
        app.interaction.suggestion_selected = 0;
        app.interaction.suggestions_dismissed = false;
    }

    // 4. Normal Prompt and Viewport navigation
    match key.code {
        KeyCode::Enter
            if key.modifiers.contains(KeyModifiers::SHIFT)
                || key.modifiers.contains(KeyModifiers::ALT) =>
        {
            app.insert_input_text("\n");
        }
        KeyCode::Enter => {
            app.handle_enter(tx);
        }
        KeyCode::Esc => {
            if app.chat_scroll > 0 {
                app.chat_scroll = 0; // Jump to bottom
            } else if !app.input_buffer.is_empty() {
                app.input_buffer.clear();
                app.input_cursor = 0;
                app.input_history_idx = None;
            } else if !app.draft_attachments.is_empty() {
                app.draft_attachments.clear();
                app.set_status("Draft attachment blocks cleared");
            }
        }
        KeyCode::Tab => {
            let choices = app.completion_candidates();
            if choices.len() == 1 {
                app.input_buffer = choices[0].clone();
                app.input_cursor = app.input_buffer.chars().count();
            } else if app.input_buffer.starts_with('/') && !app.input_buffer.contains(' ') {
                app.open_palette();
            } else if !choices.is_empty() {
                app.set_status(
                    choices
                        .iter()
                        .take(8)
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(" | "),
                );
            }
        }
        KeyCode::Char(c) => {
            app.insert_input_text(&c.to_string());
        }
        KeyCode::Backspace => {
            app.interaction
                .remember_edit(&app.input_buffer, app.input_cursor);
            if app.input_cursor > 0 {
                let end = input_byte_index(&app.input_buffer, app.input_cursor);
                let start = input_byte_index(
                    &app.input_buffer,
                    crate::interaction::previous_grapheme(&app.input_buffer, app.input_cursor),
                );
                app.input_buffer.replace_range(start..end, "");
                app.input_cursor = app.input_buffer[..start].chars().count();
            }
        }
        KeyCode::Delete => {
            app.interaction
                .remember_edit(&app.input_buffer, app.input_cursor);
            if app.input_cursor < app.input_buffer.chars().count() {
                let start = input_byte_index(&app.input_buffer, app.input_cursor);
                let end = input_byte_index(
                    &app.input_buffer,
                    crate::interaction::next_grapheme(&app.input_buffer, app.input_cursor),
                );
                app.input_buffer.replace_range(start..end, "");
            }
        }
        KeyCode::Left => {
            if app.input_cursor > 0 {
                app.input_cursor =
                    crate::interaction::previous_grapheme(&app.input_buffer, app.input_cursor);
            }
        }
        KeyCode::Right => {
            if app.input_cursor < app.input_buffer.chars().count() {
                app.input_cursor =
                    crate::interaction::next_grapheme(&app.input_buffer, app.input_cursor);
            }
        }
        KeyCode::Home => {
            app.input_cursor = 0;
        }
        KeyCode::End => {
            app.input_cursor = app.input_buffer.chars().count();
        }
        // Dedicated Viewport scroll keys
        KeyCode::PageUp => {
            app.chat_scroll = app.chat_scroll.saturating_add(8);
        }
        KeyCode::PageDown => {
            app.chat_scroll = app.chat_scroll.saturating_sub(8);
        }
        // Up arrow:
        // - If viewing earlier messages in chat (chat_scroll > 0): scroll further up
        // - If at bottom with empty input buffer (or mouse wheel translated by terminal): scroll chat up!
        // - If at bottom with existing input text or history index active: navigate command history
        KeyCode::Up => {
            if move_input_vertical(app, true) {
                return;
            }
            if app.chat_scroll > 0
                || (app.input_buffer.is_empty() && app.input_history_idx.is_none())
            {
                app.chat_scroll = app.chat_scroll.saturating_add(3);
            } else if !app.input_history.is_empty() {
                let next_idx = match app.input_history_idx {
                    None => app.input_history.len().saturating_sub(1),
                    Some(i) => i.saturating_sub(1),
                };
                app.input_history_idx = Some(next_idx);
                if let Some(cmd) = app.input_history.get(next_idx) {
                    app.input_buffer = cmd.clone();
                    app.input_cursor = app.input_buffer.chars().count();
                }
            }
        }
        // Down arrow:
        // - If viewing earlier messages in chat (chat_scroll > 0): scroll back towards bottom
        // - If at bottom with history index active: navigate history forward
        KeyCode::Down => {
            if move_input_vertical(app, false) {
                return;
            }
            if app.chat_scroll > 0 {
                app.chat_scroll = app.chat_scroll.saturating_sub(3);
            } else if let Some(i) = app.input_history_idx {
                if i + 1 < app.input_history.len() {
                    let next_idx = i + 1;
                    app.input_history_idx = Some(next_idx);
                    if let Some(cmd) = app.input_history.get(next_idx) {
                        app.input_buffer = cmd.clone();
                        app.input_cursor = app.input_buffer.chars().count();
                    }
                } else {
                    app.input_history_idx = None;
                    app.input_buffer.clear();
                    app.input_cursor = 0;
                }
            }
        }
        _ => {}
    }
}

fn input_byte_index(input: &str, char_index: usize) -> usize {
    input
        .char_indices()
        .nth(char_index)
        .map(|(index, _)| index)
        .unwrap_or(input.len())
}

fn move_input_vertical(app: &mut App, up: bool) -> bool {
    if !app.input_buffer.contains('\n') {
        return false;
    }
    let chars = app.input_buffer.chars().collect::<Vec<_>>();
    let cursor = app.input_cursor.min(chars.len());
    let line_start = chars[..cursor]
        .iter()
        .rposition(|character| *character == '\n')
        .map(|index| index + 1)
        .unwrap_or(0);
    let column = cursor - line_start;
    if up {
        if line_start == 0 {
            return false;
        }
        let previous_end = line_start - 1;
        let previous_start = chars[..previous_end]
            .iter()
            .rposition(|character| *character == '\n')
            .map(|index| index + 1)
            .unwrap_or(0);
        app.input_cursor = previous_start + column.min(previous_end - previous_start);
    } else {
        let Some(next_break) = chars[cursor..]
            .iter()
            .position(|character| *character == '\n')
            .map(|index| cursor + index)
        else {
            return false;
        };
        let next_start = next_break + 1;
        let next_end = chars[next_start..]
            .iter()
            .position(|character| *character == '\n')
            .map(|index| next_start + index)
            .unwrap_or(chars.len());
        app.input_cursor = next_start + column.min(next_end - next_start);
    }
    true
}

fn edit_external_draft(app: &mut App) {
    app.interaction.external_editor_requested = false;
    app.interaction
        .input_paused
        .store(true, std::sync::atomic::Ordering::Relaxed);
    std::thread::sleep(std::time::Duration::from_millis(30));
    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| {
            if cfg!(windows) {
                "notepad.exe".into()
            } else {
                "vi".into()
            }
        });
    let path = std::env::temp_dir().join(format!(
        "holiday-draft-{}-{}.txt",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
    ));
    let result = (|| -> Result<String, String> {
        use std::io::Write;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        options
            .open(&path)
            .and_then(|mut f| f.write_all(app.input_buffer.as_bytes()))
            .map_err(|e| e.to_string())?;
        let _ = crossterm::terminal::disable_raw_mode();
        let _ = crossterm::execute!(std::io::stdout(), crossterm::terminal::LeaveAlternateScreen);
        // Treat EDITOR as an executable path, never interpolate it into a shell.
        let result=std::process::Command::new(&editor).arg(&path).status().map_err(|e|format!("Could not run editor '{editor}': {e}. EDITOR must name an executable, without shell arguments."));
        let _ = crossterm::execute!(std::io::stdout(), crossterm::terminal::EnterAlternateScreen);
        let _ = crossterm::terminal::enable_raw_mode();
        if !result?.success() {
            return Err("Editor exited unsuccessfully; draft retained".into());
        }
        std::fs::read_to_string(&path).map_err(|e| e.to_string())
    })();
    let _ = std::fs::remove_file(&path);
    match result {
        Ok(text) => {
            app.interaction
                .remember_edit(&app.input_buffer, app.input_cursor);
            app.input_buffer = text;
            app.input_cursor = app.input_buffer.chars().count();
        }
        Err(e) => app.set_status(e),
    }
    app.interaction
        .input_paused
        .store(false, std::sync::atomic::Ordering::Relaxed);
    app.interaction.dirty = true;
}

#[cfg(test)]
mod tests {
    use super::parse_jsonl_input;
    use serde_json::json;

    #[test]
    fn parses_versioned_nested_speech_input() {
        let input = parse_jsonl_input(
            r#"{"version":1,"event":"speech","data":{"text":" hello ","sequence":42}}"#,
        )
        .expect("valid input");

        assert_eq!(input.event, "speech");
        assert_eq!(input.text.as_deref(), Some(" hello "));
        assert_eq!(input.metadata, json!({"text":" hello ","sequence":42}));
    }

    #[test]
    fn parses_flat_input_and_preserves_metadata() {
        let input = parse_jsonl_input(
            r#"{"version":1,"event":"speech","text":"hello","sequence":7,"input_kind":"command"}"#,
        )
        .expect("valid input");

        assert_eq!(input.text.as_deref(), Some("hello"));
        assert_eq!(input.metadata, json!({"sequence":7,"input_kind":"command"}));
    }

    #[test]
    fn rejects_unknown_versions_and_non_objects() {
        assert!(parse_jsonl_input(r#"{"version":2,"event":"ping"}"#).is_err());
        assert!(parse_jsonl_input(r#"[1,2,3]"#).is_err());
        assert!(parse_jsonl_input(r#"{"version":1}"#).is_err());
    }

    #[test]
    fn slash_suggestion_navigation_and_insertion() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut app = crate::app::App::new(crate::config::AppConfig::default(), "".into());
        app.interaction.back();
        app.input_buffer = "/".into();
        let expected = app.slash_suggestions()[1].name.to_string();
        let (tx, _) = tokio::sync::mpsc::unbounded_channel();
        super::handle_key_event(
            &mut app,
            KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
            tx.clone(),
        );
        super::handle_key_event(
            &mut app,
            KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE),
            tx,
        );
        assert_eq!(app.input_buffer, format!("{expected} "));
        assert!(app.interaction.top().is_none());
    }

    #[test]
    fn slash_suggestion_enter_selects_and_executes_or_inserts() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut app = crate::app::App::new(crate::config::AppConfig::default(), "".into());
        app.interaction.back();
        // Type partial command "/cle" (should match "/clear")
        app.input_buffer = "/cle".into();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        super::handle_key_event(
            &mut app,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            tx.clone(),
        );
        // /clear has Optional arguments, so hitting Enter executes it and clears prompt
        assert_eq!(app.input_buffer, "");

        // Type partial command "/att" (should match "/attach", which requires Path)
        app.input_buffer = "/att".into();
        super::handle_key_event(
            &mut app,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            tx,
        );
        // /attach requires Path, so Enter inserts "/attach " for arguments
        assert_eq!(app.input_buffer, "/attach ");
    }
}

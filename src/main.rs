mod agents;
mod app;
mod client;
mod codex_auth;
mod config;
mod events;
mod mcp;
mod session;
mod tools;
mod ui;

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
use std::time::Duration;
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
    Deny,
}

impl ToolPolicy {
    fn parse(value: &str) -> Result<Self, String> {
        match value.to_ascii_lowercase().as_str() {
            "ask" => Ok(Self::Ask),
            "auto" => Ok(Self::Auto),
            "deny" => Ok(Self::Deny),
            _ => Err(format!(
                "Unknown tool policy '{}'. Use ask, auto, or deny.",
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
                cli.tool_policy = ToolPolicy::parse(
                    &args
                        .next()
                        .ok_or_else(|| "--tools requires ask, auto, or deny".to_string())?,
                )?;
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
        return run_headless_jsonl(&mut app, cli.tool_policy).await;
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

async fn run_headless_jsonl(
    app: &mut App,
    tool_policy: ToolPolicy,
) -> Result<(), Box<dyn std::error::Error>> {
    let run_id = format!(
        "{}-{}",
        chrono::Utc::now().format("%Y%m%dT%H%M%S%.3fZ"),
        std::process::id()
    );
    let mut sequence = 0;
    emit_jsonl(
        &run_id,
        &mut sequence,
        "run_started",
        json!({"provider": app.config.provider, "model": app.config.model}),
    )?;
    emit_jsonl(
        &run_id,
        &mut sequence,
        "user_message",
        json!({"content": app.messages.last().map(|message| message.content.clone()).unwrap_or_default()}),
    )?;

    let (event_tx, mut event_rx) = mpsc::unbounded_channel::<AppEvent>();
    let mut final_status = "ok";
    let mut generation_finished = false;
    app.trigger_generation(event_tx.clone());

    loop {
        let event = tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                if app.state == EngineState::Streaming {
                    app.cancel_generation();
                }
                final_status = "cancelled";
                break;
            }
            event = event_rx.recv() => event,
        };
        let Some(event) = event else {
            break;
        };

        match event {
            AppEvent::Stream { epoch, signal } => {
                let tool_call = matches!(&signal, StreamSignal::ToolCall { .. });
                match &signal {
                    StreamSignal::ThoughtDelta(delta) => emit_jsonl(
                        &run_id,
                        &mut sequence,
                        "reasoning_delta",
                        json!({"delta": delta, "visibility": "provider_summary"}),
                    )?,
                    StreamSignal::TextDelta(delta) => {
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
                    } => emit_jsonl(
                        &run_id,
                        &mut sequence,
                        "usage",
                        json!({"prompt_tokens": prompt_tokens, "candidates_tokens": candidates_tokens, "total_tokens": total_tokens}),
                    )?,
                    StreamSignal::Finished { .. } => {
                        generation_finished = true;
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
                        ToolPolicy::Deny => "deny",
                    };
                    emit_jsonl(
                        &run_id,
                        &mut sequence,
                        "approval_required",
                        json!({"policy": policy_name, "action": if tool_policy == ToolPolicy::Auto { "execute" } else { "deny" }}),
                    )?;
                    if tool_policy == ToolPolicy::Auto {
                        app.approve_pending_tool(true, event_tx.clone());
                    } else {
                        app.deny_pending_tool(event_tx.clone());
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
                tool_name,
                call_id,
                result,
            } => {
                let result_data = match &result {
                    Ok(output) => json!({"tool": tool_name, "output": output}),
                    Err(error) => json!({"tool": tool_name, "error": error}),
                };
                emit_jsonl(&run_id, &mut sequence, "tool_result", result_data)?;
                app.handle_tool_result(tool_name, call_id, result, event_tx.clone());
                if app.state == EngineState::AwaitingHitlApproval {
                    emit_jsonl(
                        &run_id,
                        &mut sequence,
                        "approval_required",
                        json!({"policy": if tool_policy == ToolPolicy::Auto { "auto" } else { "deny" }, "action": if tool_policy == ToolPolicy::Auto { "execute" } else { "deny" }}),
                    )?;
                    if tool_policy == ToolPolicy::Auto {
                        app.approve_pending_tool(true, event_tx.clone());
                    } else {
                        app.deny_pending_tool(event_tx.clone());
                    }
                }
                if app.state == EngineState::Streaming {
                    generation_finished = false;
                }
            }
            AppEvent::CompactionFinished(result) => {
                emit_jsonl(
                    &run_id,
                    &mut sequence,
                    "compaction",
                    json!({"status": if result.is_ok() { "ok" } else { "error" }}),
                )?;
                app.handle_compaction_result(result, event_tx.clone());
                if app.state == EngineState::Streaming {
                    generation_finished = false;
                }
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
            | AppEvent::Resize(_, _) => {}
        }
        if final_status == "error" {
            break;
        }
    }
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
                let approval_needed = matches!(&signal, StreamSignal::ToolCall { .. });
                match &signal {
                    StreamSignal::TextDelta(delta) => {
                        print!("{}", delta);
                        let _ = io::stdout().flush();
                    }
                    StreamSignal::ThoughtDelta(_) => {}
                    StreamSignal::ToolCall { name, args, .. } => {
                        println!("\n[tool request: {} {}]", name, args);
                    }
                    StreamSignal::Usage { .. } => {}
                    StreamSignal::Finished { .. } => {
                        saw_finished = true;
                        println!();
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
                tool_name,
                call_id,
                result,
            } => {
                println!("\n[tool finished: {}]", tool_name);
                app.handle_tool_result(tool_name, call_id, result, tx.clone());
                if app.state == EngineState::Streaming {
                    saw_finished = false;
                }
            }
            AppEvent::CompactionFinished(result) => {
                app.handle_compaction_result(result, tx.clone());
                if app.state == EngineState::Streaming {
                    saw_finished = false;
                }
            }
            AppEvent::SystemNotification(message) => eprintln!("{}", message),
            AppEvent::ModelsFetched { .. }
            | AppEvent::Paste(_)
            | AppEvent::Key(_)
            | AppEvent::Mouse(_)
            | AppEvent::Resize(_, _) => {}
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
                    AppEvent::CompactionFinished(result) => {
                        app.handle_compaction_result(result, command_tx.clone());
                    }
                    AppEvent::ModelsFetched {
                        result: Ok(models), ..
                    } => {
                        app.available_models = models;
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
                if let Some(AppEvent::CompactionFinished(result)) = command_rx.recv().await {
                    app.handle_compaction_result(result, command_tx.clone());
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
        println!("Usage: holiday.exe [--login browser|device] | -p \"prompt\" [--format text|jsonl] [--tools ask|auto|deny] [--resume NAME|PATH | --continue] | --chat [--resume NAME|PATH | --continue] [--config PATH] [--provider NAME] [--model MODEL] [--base-url URL]");
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
    let mcp_tools = mcp::connect_all(&app.config.mcp_servers).await;
    for tool in mcp_tools {
        app.tool_registry.register(tool);
    }
    let (tx, mut rx) = mpsc::unbounded_channel::<AppEvent>();
    app.prefetch_models(tx.clone());

    // 6. Spawn input event listener thread
    let event_tx = tx.clone();
    tokio::spawn(async move {
        loop {
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
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    });

    // 7. Main TUI Event Loop (60 FPS redraw decoupled from background I/O)
    let mut render_interval = tokio::time::interval(Duration::from_millis(16)); // ~60fps

    loop {
        tokio::select! {
            _ = render_interval.tick() => {
                terminal.draw(|f| ui::render(&app, f))?;
            }
            Some(app_event) = rx.recv() => {
                let mut current_event = app_event;
                loop {
                    match current_event {
                        AppEvent::Key(key) => {
                            if key.kind == crossterm::event::KeyEventKind::Press {
                                handle_key_event(&mut app, key, tx.clone());
                            }
                        }
                        AppEvent::Paste(data) => {
                            if app.state == EngineState::Idle {
                                app.append_paste(data);
                            }
                        }
                        AppEvent::Mouse(mouse) => {
                            match mouse.kind {
                                crossterm::event::MouseEventKind::ScrollUp => {
                                    if app.state == EngineState::AwaitingHitlApproval {
                                        app.modal_scroll = app.modal_scroll.saturating_sub(5);
                                    } else if app.show_models_modal {
                                        app.models_scroll = app.models_scroll.saturating_sub(5);
                                    } else if app.show_sessions_modal {
                                        app.sessions_selected = app.sessions_selected.saturating_sub(1);
                                    } else {
                                        app.chat_scroll = app.chat_scroll.saturating_add(5);
                                    }
                                }
                                crossterm::event::MouseEventKind::ScrollDown => {
                                    if app.state == EngineState::AwaitingHitlApproval {
                                        app.modal_scroll = app.modal_scroll.saturating_add(5);
                                    } else if app.show_models_modal {
                                        app.models_scroll = app.models_scroll.saturating_add(5);
                                    } else if app.show_sessions_modal {
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
                    AppEvent::ToolExecutionResult { tool_name, call_id, result } => {
                        app.handle_tool_result(tool_name, call_id, result, tx.clone());
                    }
                    AppEvent::SystemNotification(msg) => {
                        app.add_message("system", msg);
                    }
                    AppEvent::ModelsFetched { result, interactive } => {
                        match result {
                            Ok(models) => {
                                app.available_models = models;
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
                                app.show_models_modal = interactive;
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
                    AppEvent::CompactionFinished(res) => {
                        app.handle_compaction_result(res, tx.clone());
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

    // 8. Clean terminal restoration
    let _ = app.flush_session();
    reset_terminal();
    Ok(())
}

fn handle_key_event(
    app: &mut App,
    key: crossterm::event::KeyEvent,
    tx: mpsc::UnboundedSender<AppEvent>,
) {
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

    if app.show_plan_modal {
        match key.code {
            KeyCode::Esc => {
                app.show_plan_modal = false;
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
    if app.show_sessions_modal {
        match key.code {
            KeyCode::Esc => app.show_sessions_modal = false,
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
    if app.show_thinking_modal {
        let target_model = app
            .thinking_target_model
            .clone()
            .unwrap_or_else(|| app.config.model.clone());
        let max_choice = app.thinking_choices(&target_model).len().saturating_sub(1);
        match key.code {
            KeyCode::Esc => {
                app.show_thinking_modal = false;
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
    if app.show_models_modal {
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
                app.show_models_modal = false;
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
    if app.state == EngineState::Streaming {
        if key.code == KeyCode::Esc {
            app.cancel_generation();
            return;
        }
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
                app.copy_last_response();
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
            if app.input_buffer.starts_with('/') && !app.input_buffer.contains(' ') {
                let commands = [
                    "/help",
                    "/models",
                    "/model",
                    "/compact",
                    "/thinking",
                    "/reasoning",
                    "/autocompact",
                    "/context",
                    "/status",
                    "/pwd",
                    "/tools",
                    "/plan",
                    "/todos",
                    "/todo",
                    "/agents",
                    "/attachments",
                    "/attach",
                    "/remove",
                    "/edit",
                    "/session",
                    "/sessions",
                    "/resume",
                    "/retry",
                    "/fork",
                    "/temp",
                    "/sys",
                    "/key",
                    "/provider",
                    "/baseurl",
                    "/copy",
                    "/clear",
                    "/new",
                    "/save",
                    "/quit",
                    "/exit",
                ];
                let prefix = app.input_buffer.to_lowercase();
                if let Some(matched) = commands.iter().find(|cmd| cmd.starts_with(&prefix)) {
                    app.input_buffer = format!("{} ", matched);
                    app.input_cursor = app.input_buffer.chars().count();
                }
            }
        }
        KeyCode::Char(c) => {
            app.insert_input_text(&c.to_string());
        }
        KeyCode::Backspace => {
            if app.input_cursor > 0 {
                let end = input_byte_index(&app.input_buffer, app.input_cursor);
                let start = input_byte_index(&app.input_buffer, app.input_cursor - 1);
                app.input_buffer.replace_range(start..end, "");
                app.input_cursor -= 1;
            }
        }
        KeyCode::Delete => {
            if app.input_cursor < app.input_buffer.chars().count() {
                let start = input_byte_index(&app.input_buffer, app.input_cursor);
                let end = input_byte_index(&app.input_buffer, app.input_cursor + 1);
                app.input_buffer.replace_range(start..end, "");
            }
        }
        KeyCode::Left => {
            if app.input_cursor > 0 {
                app.input_cursor -= 1;
            }
        }
        KeyCode::Right => {
            if app.input_cursor < app.input_buffer.chars().count() {
                app.input_cursor += 1;
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

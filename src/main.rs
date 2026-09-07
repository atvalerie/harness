mod app;
mod client;
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
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyModifiers},
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
    provider: Option<String>,
    model: Option<String>,
    base_url: Option<String>,
    config_path: Option<PathBuf>,
    output_format: OutputFormat,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum OutputFormat {
    Text,
    Jsonl,
}

fn resolve_api_key(config: &AppConfig) -> io::Result<String> {
    if let Some(key) = config.get_api_key_for_active_provider() {
        return Ok(key);
    }
    if ProviderKind::parse(&config.active_provider_config().kind) == ProviderKind::OpenAiCompatible
    {
        return Ok(String::new());
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "No provider API key found in the environment or keyring",
    ))
}

fn parse_cli_args() -> Result<CliArgs, String> {
    let mut args = std::env::args().skip(1);
    let mut cli = CliArgs {
        prompt: None,
        chat: false,
        provider: None,
        model: None,
        base_url: None,
        config_path: None,
        output_format: OutputFormat::Text,
    };
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-p" | "--prompt" => cli.prompt = Some(args.next().ok_or_else(|| "-p/--prompt requires a value".to_string())?),
            "--chat" => cli.chat = true,
            "--provider" => cli.provider = Some(args.next().ok_or_else(|| "--provider requires a value".to_string())?),
            "--model" => cli.model = Some(args.next().ok_or_else(|| "--model requires a value".to_string())?),
            "--base-url" => cli.base_url = Some(args.next().ok_or_else(|| "--base-url requires a value".to_string())?),
            "--config" => cli.config_path = Some(PathBuf::from(args.next().ok_or_else(|| "--config requires a path".to_string())?)),
            "--format" => {
                cli.output_format = match args.next().ok_or_else(|| "--format requires text or jsonl".to_string())?.as_str() {
                    "text" => OutputFormat::Text,
                    "jsonl" => OutputFormat::Jsonl,
                    value => return Err(format!("Unknown output format '{}'. Use text or jsonl.", value)),
                }
            }
            "-h" | "--help" => return Err("Usage: gemini-harness.exe -p \"prompt\" [--format text|jsonl] | --chat [--config PATH] [--provider NAME] [--model MODEL] [--base-url URL]".to_string()),
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
    let mut config = AppConfig::load();
    if let Some(provider) = cli.provider {
        config.select_provider(&provider);
    }
    if let Some(model) = cli.model {
        config.model = model;
    }
    if let Some(base_url) = cli.base_url {
        let value = if base_url.eq_ignore_ascii_case("default") {
            None
        } else {
            Some(base_url)
        };
        if let Some(provider) = config.providers.get_mut(&config.provider) {
            provider.base_url = value;
        } else {
            config.base_url = value;
        }
    }
    let api_key = resolve_api_key(&config)?;
    let mut app = App::new(config, api_key);
    app.add_message("user", prompt);
    if cli.output_format == OutputFormat::Jsonl {
        return run_headless_jsonl(&app).await;
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

async fn run_headless_jsonl(app: &App) -> Result<(), Box<dyn std::error::Error>> {
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

    let request = app.build_request();
    let model = app.config.model.clone();
    let fallback_models = app.config.effective_fallback_models();
    let max_retries = app.config.max_retries;
    let (stream_tx, mut stream_rx) = mpsc::unbounded_channel();
    let client = app.client.clone();
    tokio::spawn(async move {
        client
            .stream_generate_content(&model, &fallback_models, max_retries, &request, stream_tx)
            .await;
    });

    let mut final_status = "ok";
    while let Some(signal) = stream_rx.recv().await {
        let (event, data) = match signal {
            StreamSignal::ThoughtDelta(delta) => (
                "reasoning_delta",
                json!({"delta": delta, "visibility": "provider_summary"}),
            ),
            StreamSignal::TextDelta(delta) => ("text_delta", json!({"delta": delta})),
            StreamSignal::ToolCall {
                id,
                name,
                args,
                thought_signature,
            } => (
                "tool_call",
                json!({"id": id, "name": name, "args": args, "thought_signature": thought_signature}),
            ),
            StreamSignal::Usage {
                prompt_tokens,
                candidates_tokens,
                total_tokens,
            } => (
                "usage",
                json!({"prompt_tokens": prompt_tokens, "candidates_tokens": candidates_tokens, "total_tokens": total_tokens}),
            ),
            StreamSignal::Finished { finish_reason } => (
                "run_finished",
                json!({"status": "ok", "finish_reason": finish_reason}),
            ),
            StreamSignal::Notice(message) => ("notice", json!({"message": message})),
            StreamSignal::Error(error) => {
                final_status = "error";
                ("error", json!({"message": error}))
            }
        };
        emit_jsonl(&run_id, &mut sequence, event, data)?;
    }
    if final_status != "ok" {
        emit_jsonl(
            &run_id,
            &mut sequence,
            "run_finished",
            json!({"status": final_status}),
        )?;
    }
    Ok(())
}

async fn run_chat(cli: CliArgs) -> Result<(), Box<dyn std::error::Error>> {
    // `--chat` may be launched after the TUI was force-terminated. In that
    // case Windows can leave the console in raw/no-echo mode, so restore the
    // normal line-input and echo flags before reading stdin.
    let _ = crossterm::terminal::disable_raw_mode();
    let mut config = AppConfig::load();
    if let Some(provider) = cli.provider {
        config.select_provider(&provider);
    }
    if let Some(model) = cli.model {
        config.model = model;
    }
    if let Some(base_url) = cli.base_url {
        let value = if base_url.eq_ignore_ascii_case("default") {
            None
        } else {
            Some(base_url)
        };
        if let Some(provider) = config.providers.get_mut(&config.provider) {
            provider.base_url = value;
        } else {
            config.base_url = value;
        }
    }
    let api_key = resolve_api_key(&config)?;
    let mut app = App::new(config, api_key);
    let stdin = io::stdin();
    let mut line = String::new();
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
        app.add_message("user", prompt.to_string());
        let request = app.build_request();
        let model = app.config.model.clone();
        let fallback_models = app.config.effective_fallback_models();
        let max_retries = app.config.max_retries;
        let result = tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            result = app.client.generate_content_with_fallback(&model, &fallback_models, max_retries, &request) => result,
        };
        match result {
            Ok(response) => {
                println!("{}", response);
                app.add_message("model", response);
                let _ = app.flush_session();
            }
            Err(error) => eprintln!("error: {}", error),
        }
    }
    let _ = app.flush_session();
    Ok(())
}

fn reset_terminal() {
    let _ = disable_raw_mode();
    let _ = execute!(stdout(), LeaveAlternateScreen, DisableMouseCapture);
    let _ = execute!(stdout(), crossterm::cursor::Show);
}

fn prompt_for_api_key_if_missing(config: &AppConfig) -> io::Result<String> {
    if let Some(key) = config.get_api_key_for_active_provider() {
        return Ok(key);
    }
    if ProviderKind::parse(&config.active_provider_config().kind) == ProviderKind::OpenAiCompatible
    {
        return Ok(String::new());
    }

    println!();
    println!("===========================================================");
    println!("       Multi-Provider High-Performance Native TUI Harness  ");
    println!("===========================================================");
    println!("No API key detected in environment or native OS vault.");
    print!("Please enter your provider API key: ");
    io::stdout().flush()?;

    let mut input_key = String::new();
    io::stdin().read_line(&mut input_key)?;
    let trimmed = input_key.trim();

    if trimmed.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "API key cannot be empty. Exiting.",
        ));
    }

    if let Err(e) = AppConfig::set_api_key(trimmed) {
        eprintln!("Warning: could not persist key to keyring: {}", e);
    } else {
        println!("API key securely saved to OS vault / config directory.");
    }

    println!("Starting TUI...\n");
    std::thread::sleep(Duration::from_millis(500));
    Ok(trimmed.to_string())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = parse_cli_args().map_err(|error| {
        if error.starts_with("Usage:") {
            eprintln!("{}", error);
        }
        io::Error::new(io::ErrorKind::InvalidInput, error)
    })?;
    if let Some(path) = &cli.config_path {
        std::env::set_var("GEMINI_HARNESS_CONFIG", path);
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

    // 2. Check or prompt for API Key
    let config = AppConfig::load();
    let api_key = match prompt_for_api_key_if_missing(&config) {
        Ok(k) => k,
        Err(e) => {
            eprintln!("Error: {}", e);
            std::process::exit(1);
        }
    };

    // 3. Initialize Terminal in Raw Mode & Alternate Screen
    enable_raw_mode()?;
    let mut stdout_handle = stdout();
    execute!(stdout_handle, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout_handle);
    let mut terminal = Terminal::new(backend)?;

    // 5. Initialize App state and channels
    let mut app = App::new(config, api_key);
    let mcp_tools = mcp::connect_all(&app.config.mcp_servers).await;
    for tool in mcp_tools {
        app.tool_registry.register(tool);
    }
    let (tx, mut rx) = mpsc::unbounded_channel::<AppEvent>();

    // 6. Spawn input event listener thread
    let event_tx = tx.clone();
    tokio::spawn(async move {
        loop {
            if event::poll(Duration::from_millis(16)).unwrap_or(false) {
                match event::read() {
                    Ok(Event::Key(key)) => {
                        let _ = event_tx.send(AppEvent::Key(key));
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
                    AppEvent::ModelsFetched(res) => {
                        match res {
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
                                app.show_models_modal = true;
                                app.models_scroll = 0;
                                app.set_status("Fetched live models list.");
                            }
                            Err(e) => {
                                app.add_message("system", format!("Failed to fetch models: {}", e));
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
    // Global interrupt: Ctrl+C always exits cleanly
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        app.should_quit = true;
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
            KeyCode::Char('[') => app.adjust_selected_thinking(-256),
            KeyCode::Char(']') => app.adjust_selected_thinking(256),
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

    if key.modifiers.contains(KeyModifiers::CONTROL) {
        match key.code {
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
                        app.input_cursor = app.input_buffer.len();
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
                            app.input_cursor = app.input_buffer.len();
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
                    "/session",
                    "/sessions",
                    "/temp",
                    "/sys",
                    "/key",
                    "/provider",
                    "/baseurl",
                    "/copy",
                    "/clear",
                    "/save",
                    "/quit",
                ];
                let prefix = app.input_buffer.to_lowercase();
                if let Some(matched) = commands.iter().find(|cmd| cmd.starts_with(&prefix)) {
                    app.input_buffer = format!("{} ", matched);
                    app.input_cursor = app.input_buffer.len();
                }
            }
        }
        KeyCode::Char(c) => {
            app.input_buffer.insert(app.input_cursor, c);
            app.input_cursor += 1;
        }
        KeyCode::Backspace => {
            if app.input_cursor > 0 {
                app.input_cursor -= 1;
                app.input_buffer.remove(app.input_cursor);
            }
        }
        KeyCode::Delete => {
            if app.input_cursor < app.input_buffer.len() {
                app.input_buffer.remove(app.input_cursor);
            }
        }
        KeyCode::Left => {
            if app.input_cursor > 0 {
                app.input_cursor -= 1;
            }
        }
        KeyCode::Right => {
            if app.input_cursor < app.input_buffer.len() {
                app.input_cursor += 1;
            }
        }
        KeyCode::Home => {
            app.input_cursor = 0;
        }
        KeyCode::End => {
            app.input_cursor = app.input_buffer.len();
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
                    app.input_cursor = app.input_buffer.len();
                }
            }
        }
        // Down arrow:
        // - If viewing earlier messages in chat (chat_scroll > 0): scroll back towards bottom
        // - If at bottom with history index active: navigate history forward
        KeyCode::Down => {
            if app.chat_scroll > 0 {
                app.chat_scroll = app.chat_scroll.saturating_sub(3);
            } else if let Some(i) = app.input_history_idx {
                if i + 1 < app.input_history.len() {
                    let next_idx = i + 1;
                    app.input_history_idx = Some(next_idx);
                    if let Some(cmd) = app.input_history.get(next_idx) {
                        app.input_buffer = cmd.clone();
                        app.input_cursor = app.input_buffer.len();
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

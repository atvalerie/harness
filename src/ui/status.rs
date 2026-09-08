use crate::app::{App, EngineState};
use crate::client::get_model_pricing;
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

pub fn render_status(app: &App, frame: &mut Frame, area: Rect) {
    let (in_p, out_p) = get_model_pricing(&app.config.model);
    let pricing_str = match (in_p, out_p) {
        (Some(i), Some(o)) => format!(" (${:.2}/${:.2} M)", i, o),
        _ => String::new(),
    };

    let (state_badge, state_fg, state_bg) = match app.state {
        EngineState::Idle => ("IDLE", Color::Black, Color::DarkGray),
        EngineState::Streaming => ("STREAMING", Color::Black, Color::Green),
        EngineState::Compacting => ("COMPACTING", Color::Black, Color::Yellow),
        EngineState::AwaitingHitlApproval => ("GATE-HOLD", Color::Black, Color::Yellow),
        EngineState::ExecutingTool => ("TOOL-EXEC", Color::Black, Color::LightCyan),
    };

    let thinking_str = app.thinking_mode(&app.config.model);
    let temperature_text = if app
        .config
        .active_provider_config()
        .kind
        .eq_ignore_ascii_case("codex")
    {
        "n/a".to_string()
    } else {
        format!("{:.1}", app.config.temperature)
    };

    // Context tracking
    let context_limit = app.context_limit();

    let context_pct = context_limit
        .map(|limit| (app.context_tokens as f64 / limit.max(1) as f64) * 100.0)
        .unwrap_or(0.0);
    let context_limit_text = context_limit
        .map(format_compact_number)
        .unwrap_or_else(|| "?".to_string());

    let tps_str = if app.state == EngineState::Streaming || app.current_tps > 0.0 {
        format!(" │ {:.1} tps", app.current_tps)
    } else {
        String::new()
    };

    let activity = match app.state {
        EngineState::Streaming | EngineState::Compacting | EngineState::ExecutingTool => {
            format!("{} ", spinner(app))
        }
        EngineState::AwaitingHitlApproval => "! ".to_string(),
        EngineState::Idle => String::new(),
    };

    let line = Line::from(vec![
        Span::styled(
            " HOLIDAY ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(" {} ", state_badge),
            Style::default()
                .fg(state_fg)
                .bg(state_bg)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled(activity, Style::default().fg(Color::LightCyan)),
        Span::styled(
            &app.config.model,
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(pricing_str, Style::default().fg(Color::DarkGray)),
        Span::raw(" │ "),
        Span::styled("think: ", Style::default().fg(Color::DarkGray)),
        Span::styled(thinking_str, Style::default().fg(Color::Magenta)),
        Span::raw(" │ "),
        Span::styled("temp: ", Style::default().fg(Color::DarkGray)),
        Span::styled(temperature_text, Style::default().fg(Color::Yellow)),
        Span::raw(" │ "),
        Span::styled("ctx: ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            format!(
                "{}{}/{} ({:.1}%)",
                if app.context_tokens_estimated {
                    "~"
                } else {
                    ""
                },
                app.context_tokens,
                context_limit_text,
                context_pct
            ),
            Style::default().fg(if context_pct > 80.0 {
                Color::Red
            } else {
                Color::Green
            }),
        ),
        Span::styled(tps_str, Style::default().fg(Color::LightCyan)),
        Span::raw(" │ "),
    ]);

    frame.render_widget(Paragraph::new(line), area);
}

pub fn spinner(app: &App) -> char {
    const FRAMES: [char; 8] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧'];
    let index = ((app.ui_started_at.elapsed().as_millis() / 100) as usize) % FRAMES.len();
    FRAMES[index]
}

fn format_compact_number(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{}M", n / 1_000_000)
    } else if n >= 1_000 {
        format!("{}k", n / 1_000)
    } else {
        n.to_string()
    }
}

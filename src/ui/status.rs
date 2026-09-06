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

    let thinking_str = if app.config.thinking_budget > 0 {
        format!("{} tok", app.config.thinking_budget)
    } else {
        "off".to_string()
    };

    // Context tracking
    let context_limit = app
        .available_models
        .iter()
        .find(|m| m.id == app.config.model)
        .and_then(|m| m.input_token_limit)
        .unwrap_or(1_048_576);

    let context_pct = if context_limit > 0 {
        (app.total_tokens as f64 / context_limit as f64) * 100.0
    } else {
        0.0
    };

    let tps_str = if app.state == EngineState::Streaming || app.current_tps > 0.0 {
        format!(" │ {:.1} tps", app.current_tps)
    } else {
        String::new()
    };

    let status_info = app.status_message.as_deref().unwrap_or("ready");

    let line = Line::from(vec![
        Span::styled(" HARNESS ", Style::default().fg(Color::Black).bg(Color::Cyan).add_modifier(Modifier::BOLD)),
        Span::styled(format!(" {} ", state_badge), Style::default().fg(state_fg).bg(state_bg).add_modifier(Modifier::BOLD)),
        Span::raw(" "),
        Span::styled(&app.config.model, Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
        Span::styled(pricing_str, Style::default().fg(Color::DarkGray)),
        Span::raw(" │ "),
        Span::styled("think: ", Style::default().fg(Color::DarkGray)),
        Span::styled(thinking_str, Style::default().fg(Color::Magenta)),
        Span::raw(" │ "),
        Span::styled("temp: ", Style::default().fg(Color::DarkGray)),
        Span::styled(format!("{:.1}", app.config.temperature), Style::default().fg(Color::Yellow)),
        Span::raw(" │ "),
        Span::styled("ctx: ", Style::default().fg(Color::DarkGray)),
        Span::styled(format!("{}/{} ({:.1}%)", app.total_tokens, format_compact_number(context_limit), context_pct), Style::default().fg(if context_pct > 80.0 { Color::Red } else { Color::Green })),
        Span::styled(tps_str, Style::default().fg(Color::LightCyan)),
        Span::raw(" │ "),
        Span::styled(status_info, Style::default().fg(Color::DarkGray)),
    ]);

    frame.render_widget(Paragraph::new(line), area);
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

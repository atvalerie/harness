use crate::app::App;
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
    Frame,
};

pub fn render_hitl_modal(app: &App, frame: &mut Frame, area: Rect) {
    let pending = match &app.pending_tool_call {
        Some(p) => p,
        None => return,
    };

    let mut lines: Vec<Line> = Vec::new();

    // Line 1: Header + Tool Identification
    lines.push(Line::from(vec![
        Span::styled(" ⚠ HITL PERMISSION GATE ", Style::default().fg(Color::Black).bg(Color::Yellow).add_modifier(Modifier::BOLD)),
        Span::raw(" "),
        Span::styled(format!("Tool: {} ", pending.tool_name), Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
        Span::styled(format!("({})", pending.preview.title), Style::default().fg(Color::DarkGray)),
    ]));

    // Line 2: Details / Target explanations
    for d in &pending.preview.details {
        lines.push(Line::from(vec![
            Span::styled(" • ", Style::default().fg(Color::Yellow)),
            Span::styled(d, Style::default().fg(Color::White)),
        ]));
    }

    // Line 3+: Unified Diffs or Arguments
    if !pending.preview.diff_hunks.is_empty() {
        lines.push(Line::from(vec![
            Span::styled("Unified Diff Preview: ", Style::default().fg(Color::Yellow)),
            Span::styled("(Use Up/Down or PgUp/PgDn to scroll diff)", Style::default().fg(Color::DarkGray)),
        ]));

        for hunk in &pending.preview.diff_hunks {
            let (prefix, style) = match hunk.tag.as_str() {
                "+" => ("+ ", Style::default().fg(Color::Green)),
                "-" => ("- ", Style::default().fg(Color::Red)),
                _ => ("  ", Style::default().fg(Color::DarkGray)),
            };

            let line_no_str = match (hunk.old_line_no, hunk.new_line_no) {
                (Some(o), Some(n)) => format!("{:3}:{:3} ", o, n),
                (Some(o), None) => format!("{:3}:    ", o),
                (None, Some(n)) => format!("   :{:3} ", n),
                _ => "       ".to_string(),
            };

            lines.push(Line::from(vec![
                Span::styled(line_no_str, Style::default().fg(Color::DarkGray)),
                Span::styled(format!("{}{}", prefix, hunk.line), style),
            ]));
        }
    } else {
        let formatted_args = serde_json::to_string_pretty(&pending.args).unwrap_or_default();
        for l in formatted_args.lines() {
            lines.push(Line::from(vec![
                Span::styled(format!("  {}", l), Style::default().fg(Color::LightCyan)),
            ]));
        }
    }

    // Trailing explanation line
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled("Authorize? ", Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
        Span::styled("[Y] Approve Once ", Style::default().fg(Color::Black).bg(Color::Green).add_modifier(Modifier::BOLD)),
        Span::raw("  "),
        Span::styled("[A] Always Allow (Session) ", Style::default().fg(Color::Black).bg(Color::LightCyan).add_modifier(Modifier::BOLD)),
        Span::raw("  "),
        Span::styled("[N / Esc] Reject ", Style::default().fg(Color::Black).bg(Color::Red).add_modifier(Modifier::BOLD)),
        Span::styled("  (Rejection returns feedback to Gemini)", Style::default().fg(Color::DarkGray)),
    ]));

    let paragraph = Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(Span::styled(" Human-In-The-Loop Security Review ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)))
                .border_style(Style::default().fg(Color::Yellow)),
        )
        .wrap(Wrap { trim: false })
        .scroll((app.modal_scroll as u16, 0));

    frame.render_widget(paragraph, area);
}

pub fn render_models_modal(app: &App, frame: &mut Frame, area: Rect) {
    if !app.show_models_modal {
        return;
    }

    let popup_area = centered_rect(80, 75, area);
    frame.render_widget(Clear, popup_area);

    let mut lines = Vec::new();
    lines.push(Line::from(vec![
        Span::styled(" Models & Profiles ", Style::default().fg(Color::Black).bg(Color::Cyan).add_modifier(Modifier::BOLD)),
    ]));
    lines.push(Line::from(""));
    lines.push(Line::from("[Enter] use  [R] reasoning  [[]/[]] thinking  [T] temperature  [-/+] max tokens  [F] fallback  [Esc] close"));

    if app.available_models.is_empty() {
        lines.push(Line::from("No models loaded or query still in progress..."));
    } else {
        for (index, m) in app.available_models.iter().enumerate() {
            let is_current = m.id == app.config.model;
            let is_selected = index == app.models_selected;
            let marker = if is_current { "▶ " } else { "  " };

            let profile = app.config.model_profiles.get(&format!("{}:{}", app.config.provider, m.id));
            let reasoning = profile.and_then(|p| p.reasoning_enabled).unwrap_or(app.config.thinking_budget > 0);
            let budget = profile.and_then(|p| p.thinking_budget).unwrap_or(app.config.thinking_budget);
            let temperature = profile.and_then(|p| p.temperature).unwrap_or(app.config.temperature);
            let max_tokens = profile.and_then(|p| p.max_output_tokens).unwrap_or(8192);
            let fallback = app.config.active_provider_config().fallback_models.iter().any(|candidate| candidate == &m.id);

            let pricing = match (m.input_price_per_m, m.output_price_per_m) {
                (Some(in_p), Some(out_p)) => format!(" [${:.2} in / ${:.2} out per 1M tokens]", in_p, out_p),
                _ => " [Free tier / Standard API]".to_string(),
            };

            lines.push(Line::from(vec![
                Span::styled(if is_selected { "◆ " } else { marker }, Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
                Span::styled(format!("{:<28}", m.id), Style::default().fg(if is_selected { Color::Yellow } else if is_current { Color::Green } else { Color::Cyan }).add_modifier(Modifier::BOLD)),
                Span::styled(pricing, Style::default().fg(Color::Green)),
            ]));
            if is_selected {
                lines.push(Line::from(format!("    profile: reasoning={} budget={} temp={:.1} max_tokens={} fallback={}", if reasoning { "on" } else { "off" }, budget, temperature, max_tokens, if fallback { "yes" } else { "no" })));
            }
            if !m.description.is_empty() {
                lines.push(Line::from(vec![
                    Span::styled(format!("    {}", m.description), Style::default().fg(Color::DarkGray)),
                ]));
            }
            lines.push(Line::from(""));
        }
    }

    lines.push(Line::from(vec![
        Span::styled("[S] save config  [Esc] close", Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
    ]));

    let selected_offset = 3usize.saturating_add(
        app.available_models
            .iter()
            .take(app.models_selected)
            .map(|model| 2 + usize::from(!model.description.is_empty()))
            .sum::<usize>(),
    );
    let paragraph = Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Live Models List ")
                .border_style(Style::default().fg(Color::Cyan)),
        )
        .wrap(Wrap { trim: false })
        .scroll((selected_offset as u16, 0));

    frame.render_widget(paragraph, popup_area);
}

pub fn render_sessions_modal(app: &App, frame: &mut Frame, area: Rect) {
    if !app.show_sessions_modal { return; }
    let popup_area = centered_rect(75, 65, area);
    frame.render_widget(Clear, popup_area);
    let mut lines = vec![
        Line::from(Span::styled(" Session Browser ", Style::default().fg(Color::Black).bg(Color::Magenta).add_modifier(Modifier::BOLD))),
        Line::from("[Enter] resume  [D] delete  [E] export  [Esc] close"),
        Line::from(""),
    ];
    if app.available_sessions.is_empty() {
        lines.push(Line::from("No saved sessions."));
    } else {
        for (index, session) in app.available_sessions.iter().enumerate() {
            let selected = index == app.sessions_selected;
            let marker = if selected { "◆ " } else { "  " };
            let modified = session.modified.duration_since(std::time::SystemTime::UNIX_EPOCH).map(|duration| duration.as_secs()).unwrap_or(0);
            lines.push(Line::from(vec![
                Span::styled(marker, Style::default().fg(Color::Yellow)),
                Span::styled(format!("{:<20}", session.name), Style::default().fg(if selected { Color::Yellow } else { Color::Cyan }).add_modifier(Modifier::BOLD)),
                Span::raw(format!(" {} / {}  {} msgs  modified:{}", session.provider, session.model, session.messages, modified)),
            ]));
        }
    }
    let paragraph = Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(" Saved Sessions ").border_style(Style::default().fg(Color::Magenta))).wrap(Wrap { trim: false });
    frame.render_widget(paragraph, popup_area);
}

fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}

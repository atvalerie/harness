use crate::app::App;
use crate::tools::DiffHunk;
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

    let block = Block::default().borders(Borders::ALL).title(" Approval Required ").border_style(Style::default().fg(Color::Yellow));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let header_area = Rect { x: inner.x, y: inner.y, width: inner.width, height: 1.min(inner.height) };
    let footer_height = 1.min(inner.height.saturating_sub(header_area.height));
    let footer_area = Rect { x: inner.x, y: inner.y + inner.height.saturating_sub(footer_height), width: inner.width, height: footer_height };
    let body_area = Rect { x: inner.x, y: inner.y + header_area.height, width: inner.width, height: inner.height.saturating_sub(header_area.height + footer_height) };
    frame.render_widget(Paragraph::new(Line::from(vec![
        Span::styled("ACTION ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
        Span::styled(&pending.tool_name, Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
        Span::raw(" — "),
        Span::styled(&pending.preview.title, Style::default().fg(Color::White)),
    ])), header_area);

    let mut body_lines = pending
        .preview
        .details
        .iter()
        .cloned()
        .map(Line::from)
        .collect::<Vec<_>>();
    if !pending.preview.diff_hunks.is_empty() {
        body_lines.push(Line::from(Span::styled(
            "Diff:",
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        )));
        body_lines.extend(pending.preview.diff_hunks.iter().map(render_diff_line));
    }

    frame.render_widget(
        Paragraph::new(body_lines)
            .wrap(Wrap { trim: false })
            .scroll((app.modal_scroll as u16, 0)),
        body_area,
    );
    let footer = if pending.preview.diff_hunks.is_empty() {
        "[Y] approve once   [A] allow this session   [N/Esc] reject"
    } else {
        "[↑/↓] scroll diff   [Y] approve once   [A] allow   [N/Esc] reject"
    };
    frame.render_widget(Paragraph::new(footer), footer_area);
}

fn render_diff_line(hunk: &DiffHunk) -> Line<'static> {
    let line_number = hunk
        .new_line_no
        .or(hunk.old_line_no)
        .map(|line| format!("{:>4} ", line + 1))
        .unwrap_or_else(|| "     ".to_string());
    let (color, modifier) = match hunk.tag.as_str() {
        "+" => (Color::Green, Modifier::BOLD),
        "-" => (Color::Red, Modifier::BOLD),
        _ => (Color::DarkGray, Modifier::empty()),
    };
    Line::from(vec![
        Span::styled(line_number, Style::default().fg(Color::DarkGray)),
        Span::styled(
            format!("{}{}", hunk.tag, hunk.line),
            Style::default().fg(color).add_modifier(modifier),
        ),
    ])
}

pub fn render_models_modal(app: &App, frame: &mut Frame, area: Rect) {
    if !app.show_models_modal {
        return;
    }

    let popup_area = centered_rect(80, 75, area);
    frame.render_widget(Clear, popup_area);

    let block = Block::default().borders(Borders::ALL).title(" Models & Profiles ").border_style(Style::default().fg(Color::Cyan));
    let inner = block.inner(popup_area);
    frame.render_widget(block, popup_area);
    let header_area = Rect { x: inner.x, y: inner.y, width: inner.width, height: 1.min(inner.height) };
    let footer_height = 1.min(inner.height.saturating_sub(header_area.height));
    let footer_area = Rect { x: inner.x, y: inner.y + inner.height.saturating_sub(footer_height), width: inner.width, height: footer_height };
    let list_area = Rect { x: inner.x, y: inner.y + header_area.height, width: inner.width, height: inner.height.saturating_sub(header_area.height + footer_height) };
    frame.render_widget(Paragraph::new("[Enter] use  [R] reasoning  [[]/[]] thinking  [T] temperature  [-/+] max tokens  [F] fallback"), header_area);

    let mut lines = Vec::new();
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

    let selected_offset = 3usize.saturating_add(
        app.available_models
            .iter()
            .take(app.models_selected)
            .map(|model| 2 + usize::from(!model.description.is_empty()))
            .sum::<usize>(),
    );
    let paragraph = Paragraph::new(lines).wrap(Wrap { trim: false }).scroll((selected_offset.saturating_sub(3) as u16, 0));
    frame.render_widget(paragraph, list_area);
    frame.render_widget(Paragraph::new("[S] save config  [Esc] close"), footer_area);
}

pub fn render_sessions_modal(app: &App, frame: &mut Frame, area: Rect) {
    if !app.show_sessions_modal { return; }
    let popup_area = centered_rect(75, 65, area);
    frame.render_widget(Clear, popup_area);
    let block = Block::default().borders(Borders::ALL).title(" Session Browser ").border_style(Style::default().fg(Color::Magenta));
    let inner = block.inner(popup_area);
    frame.render_widget(block, popup_area);
    let header_area = Rect { x: inner.x, y: inner.y, width: inner.width, height: 1.min(inner.height) };
    let footer_height = 1.min(inner.height.saturating_sub(header_area.height));
    let footer_area = Rect { x: inner.x, y: inner.y + inner.height.saturating_sub(footer_height), width: inner.width, height: footer_height };
    let list_area = Rect { x: inner.x, y: inner.y + header_area.height, width: inner.width, height: inner.height.saturating_sub(header_area.height + footer_height) };
    frame.render_widget(Paragraph::new("[Enter] resume  [D] delete  [E] export"), header_area);
    let mut lines = Vec::new();
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
    let paragraph = Paragraph::new(lines).wrap(Wrap { trim: false }).scroll((app.sessions_selected as u16, 0));
    frame.render_widget(paragraph, list_area);
    frame.render_widget(Paragraph::new("[Esc] close"), footer_area);
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

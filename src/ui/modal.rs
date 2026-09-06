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
        Span::styled(" Gemini Available Models & Real-time Pricing ", Style::default().fg(Color::Black).bg(Color::Cyan).add_modifier(Modifier::BOLD)),
    ]));
    lines.push(Line::from(""));

    if app.available_models.is_empty() {
        lines.push(Line::from("No models loaded or query still in progress..."));
    } else {
        for m in &app.available_models {
            let is_current = m.id == app.config.model;
            let marker = if is_current { "▶ " } else { "  " };

            let pricing = match (m.input_price_per_m, m.output_price_per_m) {
                (Some(in_p), Some(out_p)) => format!(" [${:.2} in / ${:.2} out per 1M tokens]", in_p, out_p),
                _ => " [Free tier / Standard API]".to_string(),
            };

            lines.push(Line::from(vec![
                Span::styled(marker, Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
                Span::styled(format!("{:<28}", m.id), Style::default().fg(if is_current { Color::Yellow } else { Color::Cyan }).add_modifier(Modifier::BOLD)),
                Span::styled(pricing, Style::default().fg(Color::Green)),
            ]));
            if !m.description.is_empty() {
                lines.push(Line::from(vec![
                    Span::styled(format!("    {}", m.description), Style::default().fg(Color::DarkGray)),
                ]));
            }
            lines.push(Line::from(""));
        }
    }

    lines.push(Line::from(vec![
        Span::styled("Press [Esc] or [Enter] to close. Switch model anytime with /model <name>", Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
    ]));

    let paragraph = Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Live Models List ")
                .border_style(Style::default().fg(Color::Cyan)),
        )
        .wrap(Wrap { trim: false })
        .scroll((app.models_scroll as u16, 0));

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

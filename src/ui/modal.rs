use crate::app::App;
use crate::config::{tool_permission_description, PermissionMode, TOOL_PERMISSION_GROUPS};
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

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Approval Required ")
        .border_style(Style::default().fg(Color::Yellow));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let header_area = Rect {
        x: inner.x,
        y: inner.y,
        width: inner.width,
        height: 1.min(inner.height),
    };
    let footer_height = 1.min(inner.height.saturating_sub(header_area.height));
    let footer_area = Rect {
        x: inner.x,
        y: inner.y + inner.height.saturating_sub(footer_height),
        width: inner.width,
        height: footer_height,
    };
    let body_area = Rect {
        x: inner.x,
        y: inner.y + header_area.height,
        width: inner.width,
        height: inner
            .height
            .saturating_sub(header_area.height + footer_height),
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                "ACTION ",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                &pending.tool_name,
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" — "),
            Span::styled(&pending.preview.title, Style::default().fg(Color::White)),
        ])),
        header_area,
    );

    let section_style = Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD);
    let mut body_lines = Vec::new();
    {
        body_lines.push(Line::from(Span::styled("Why:", section_style)));
        body_lines.push(Line::from(format!(
            "  {}",
            pending
                .preview
                .reason
                .as_deref()
                .unwrap_or("Not provided by model.")
        )));
        body_lines.push(Line::from(""));
        body_lines.push(Line::from(Span::styled("Expected effect:", section_style)));
        body_lines.push(Line::from(format!(
            "  {}",
            pending
                .preview
                .expected_effect
                .as_deref()
                .unwrap_or("Not provided by model.")
        )));
        body_lines.push(Line::from(""));
    }
    if let Some(command) = &pending.preview.command {
        body_lines.push(Line::from(Span::styled("Command:", section_style)));
        for (index, line) in command.split('\n').enumerate() {
            let line = line.strip_suffix('\r').unwrap_or(line);
            body_lines.push(Line::from(format!("{:>3} | {}", index + 1, line)));
        }
        body_lines.push(Line::from(""));
    }
    body_lines.extend(pending.preview.details.iter().cloned().map(Line::from));
    if !pending.preview.diff_hunks.is_empty() {
        body_lines.push(Line::from(Span::styled("Diff:", section_style)));
        body_lines.extend(pending.preview.diff_hunks.iter().map(render_diff_line));
    }

    frame.render_widget(
        Paragraph::new(body_lines)
            .wrap(Wrap { trim: false })
            .scroll((app.modal_scroll as u16, 0)),
        body_area,
    );
    let footer = if pending.preview.diff_hunks.is_empty() {
        "[Up/Down] scroll   [Y] once   [A] allow session   [N/Esc] reject"
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

pub fn render_permissions_modal(app: &App, frame: &mut Frame, area: Rect) {
    if !app
        .interaction
        .is_overlay(crate::interaction::Overlay::Permissions)
    {
        return;
    }
    let popup_area = centered_rect(86, 82, area);
    frame.render_widget(Clear, popup_area);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Tool Permissions ")
        .border_style(Style::default().fg(Color::Yellow));
    let inner = block.inner(popup_area);
    frame.render_widget(block, popup_area);

    let mut lines = vec![
        Line::from(Span::styled(
            "Choose the default handling for each tool risk group.",
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(
            "Allow bypasses review; Review uses codex-auto-review; Ask asks you; Deny rejects.",
        ),
        Line::from(""),
    ];
    for (index, (key, label)) in TOOL_PERMISSION_GROUPS.iter().enumerate() {
        let selected = index == app.permissions_selected;
        let mode = app.config.permission_mode_for(key);
        let mode_label = match mode {
            PermissionMode::Review => "REVIEW",
            PermissionMode::Allow => "ALLOW",
            PermissionMode::Ask => "ASK",
            PermissionMode::Deny => "DENY",
        };
        let mode_color = match mode {
            PermissionMode::Review => Color::Cyan,
            PermissionMode::Allow => Color::Green,
            PermissionMode::Ask => Color::Yellow,
            PermissionMode::Deny => Color::Red,
        };
        lines.push(Line::from(vec![
            Span::styled(
                if selected { "> " } else { "  " },
                Style::default().fg(Color::Yellow),
            ),
            Span::styled(
                format!("{:<34}", label),
                Style::default()
                    .fg(if selected { Color::Cyan } else { Color::White })
                    .add_modifier(if selected {
                        Modifier::BOLD
                    } else {
                        Modifier::empty()
                    }),
            ),
            Span::styled(
                format!("[{}]", mode_label),
                Style::default().fg(mode_color).add_modifier(Modifier::BOLD),
            ),
        ]));
    }
    lines.push(Line::from(""));
    if let Some((key, label)) = TOOL_PERMISSION_GROUPS.get(app.permissions_selected) {
        lines.push(Line::from(Span::styled(
            format!("{} covers:", label),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(format!(
            "  {}",
            tool_permission_description(key)
        )));
        let tools = app.tool_registry.tools_for_permission_group(key);
        if tools.is_empty() {
            lines.push(Line::from("  No registered tools."));
        } else {
            lines.push(Line::from(format!("  Registered tools ({}):", tools.len())));
            for (name, description) in tools {
                lines.push(Line::from(format!("    • {} — {}", name, description)));
            }
        }
    }
    lines.push(Line::from(""));
    lines.push(Line::from(
        "These policies are independent from which schemas are visible to the model.",
    ));
    lines.push(Line::from("Reopen this screen with /permissions."));
    let footer_height = 1.min(inner.height);
    let footer = Rect {
        x: inner.x,
        y: inner.y + inner.height.saturating_sub(footer_height),
        width: inner.width,
        height: footer_height,
    };
    let body = Rect {
        x: inner.x,
        y: inner.y,
        width: inner.width,
        height: inner.height.saturating_sub(footer_height),
    };
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((app.modal_scroll as u16, 0)),
        body,
    );
    frame.render_widget(
        Paragraph::new(
            "[Up/Down] choose  [Left/Right/Space] change  [PgUp/PgDn] scroll  [Enter] save  [Esc] close",
        ),
        footer,
    );
}

pub fn render_models_modal(app: &App, frame: &mut Frame, area: Rect) {
    if !app
        .interaction
        .is_overlay(crate::interaction::Overlay::Models)
    {
        return;
    }

    let popup_area = centered_rect(80, 75, area);
    frame.render_widget(Clear, popup_area);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Models & Profiles ")
        .border_style(Style::default().fg(Color::Cyan));
    let inner = block.inner(popup_area);
    frame.render_widget(block, popup_area);
    let header_area = Rect {
        x: inner.x,
        y: inner.y,
        width: inner.width,
        height: 1.min(inner.height),
    };
    let footer_height = 1.min(inner.height.saturating_sub(header_area.height));
    let footer_area = Rect {
        x: inner.x,
        y: inner.y + inner.height.saturating_sub(footer_height),
        width: inner.width,
        height: footer_height,
    };
    let list_area = Rect {
        x: inner.x,
        y: inner.y + header_area.height,
        width: inner.width,
        height: inner
            .height
            .saturating_sub(header_area.height + footer_height),
    };
    let header = if app.models_searching {
        format!("Search: /{}", app.models_filter)
    } else {
        "[Enter] use  [K] thinking picker  [R] toggle  [T] temp  [-/+] max tokens  [F] fallback"
            .to_string()
    };
    frame.render_widget(Paragraph::new(header), header_area);

    let mut lines = Vec::new();
    let model_indices = app.filtered_model_indices();
    if app.available_models.is_empty() {
        lines.push(Line::from("No models loaded or query still in progress..."));
    } else if model_indices.is_empty() {
        lines.push(Line::from(format!(
            "No models match '{}'. Press Esc to clear search.",
            app.models_filter
        )));
    } else {
        for (filtered_index, source_index) in model_indices.iter().enumerate() {
            let m = &app.available_models[*source_index];
            let is_current = m.id == app.config.model;
            let is_selected = filtered_index == app.models_selected;
            let marker = if is_current { "▶ " } else { "  " };

            let profile = app
                .config
                .model_profiles
                .get(&format!("{}:{}", app.config.provider, m.id));
            let reasoning = profile
                .and_then(|p| p.reasoning_enabled)
                .unwrap_or(app.config.thinking_budget > 0);
            let thinking_mode = app.thinking_mode(&m.id);
            let temperature = profile
                .and_then(|p| p.temperature)
                .unwrap_or(app.config.temperature);
            let max_tokens = profile.and_then(|p| p.max_output_tokens).unwrap_or(8192);
            let fallback = app
                .config
                .active_provider_config()
                .fallback_models
                .iter()
                .any(|candidate| candidate == &m.id);

            let pricing = if m.id.to_ascii_lowercase().ends_with("-free") {
                " [Free]".to_string()
            } else {
                match (m.input_price_per_m, m.output_price_per_m) {
                    (Some(in_p), Some(out_p)) => {
                        format!(" [${:.2} in / ${:.2} out per 1M tokens]", in_p, out_p)
                    }
                    _ => " [Pricing unavailable]".to_string(),
                }
            };

            lines.push(Line::from(vec![
                Span::styled(
                    if is_selected { "◆ " } else { marker },
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("{:<28}", m.id),
                    Style::default()
                        .fg(if is_selected {
                            Color::Yellow
                        } else if is_current {
                            Color::Green
                        } else {
                            Color::Cyan
                        })
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(pricing, Style::default().fg(Color::Green)),
            ]));
            if is_selected {
                lines.push(Line::from(format!(
                    "    profile: thinking={} reasoning={} temp={:.1} max_tokens={} fallback={}",
                    thinking_mode,
                    if reasoning { "on" } else { "off" },
                    temperature,
                    max_tokens,
                    if fallback { "yes" } else { "no" }
                )));
            }
            if !m.description.is_empty() {
                lines.push(Line::from(vec![Span::styled(
                    format!("    {}", m.description),
                    Style::default().fg(Color::DarkGray),
                )]));
            }
            lines.push(Line::from(""));
        }
    }

    let selected_offset = 3usize.saturating_add(
        model_indices
            .iter()
            .take(app.models_selected)
            .map(|index| &app.available_models[*index])
            .map(|model| 2 + usize::from(!model.description.is_empty()))
            .sum::<usize>(),
    );
    let paragraph = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .scroll((selected_offset.saturating_sub(3) as u16, 0));
    frame.render_widget(paragraph, list_area);
    let footer = if app.models_searching {
        "[Enter] use filtered model  [Esc] clear search  [Backspace] edit"
    } else {
        "[S] save config  [/] search  [Esc] close"
    };
    frame.render_widget(Paragraph::new(footer), footer_area);
}

pub fn render_thinking_modal(app: &App, frame: &mut Frame, area: Rect) {
    if !app
        .interaction
        .is_overlay(crate::interaction::Overlay::Thinking)
    {
        return;
    }
    let popup_area = centered_rect(52, 45, area);
    frame.render_widget(Clear, popup_area);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Thinking / Reasoning ")
        .border_style(Style::default().fg(Color::Magenta));
    let inner = block.inner(popup_area);
    frame.render_widget(block, popup_area);

    let model = app
        .thinking_target_model
        .clone()
        .unwrap_or_else(|| app.config.model.clone());
    let choices = app.thinking_choices(&model);
    let mut lines = vec![Line::from(format!(
        "Model: {}  (persisted per provider/model)",
        model
    ))];
    for (index, name) in choices.iter().enumerate() {
        let selected = index == app.thinking_selected;
        let description = match *name {
            "off" => "No extended reasoning",
            "minimal" => "Minimal reasoning effort",
            "low" => "Low reasoning effort",
            "medium" => "Balanced reasoning effort",
            "high" => "High reasoning effort",
            "xhigh" => "Extra-high reasoning effort",
            _ => "Provider-defined reasoning effort",
        };
        lines.push(Line::from(vec![
            Span::styled(
                if selected { "◆ " } else { "  " },
                Style::default().fg(Color::Yellow),
            ),
            Span::styled(
                format!("{:8}", name),
                Style::default()
                    .fg(if selected { Color::Yellow } else { Color::Cyan })
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(description, Style::default().fg(Color::DarkGray)),
        ]));
    }
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
    let footer = Rect {
        x: inner.x,
        y: inner.y + inner.height.saturating_sub(1),
        width: inner.width,
        height: 1.min(inner.height),
    };
    frame.render_widget(
        Paragraph::new("[↑/↓] choose  [Enter] apply  [Esc] cancel"),
        footer,
    );
}

pub fn render_plan_modal(app: &App, frame: &mut Frame, area: Rect) {
    if !app
        .interaction
        .is_overlay(crate::interaction::Overlay::Plan)
    {
        return;
    }
    let popup_area = centered_rect(62, 38, area);
    frame.render_widget(Clear, popup_area);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Plan Ready ")
        .border_style(Style::default().fg(Color::Yellow));
    let inner = block.inner(popup_area);
    frame.render_widget(block, popup_area);

    let options = [
        (
            "Execute plan",
            "Approve the structured plan and allow mutation tools.",
        ),
        (
            "Keep planning",
            "Ask the model to refine the plan or resolve questions.",
        ),
    ];
    let mut lines = vec![
        Line::from(Span::styled(
            "The model produced a structured plan.",
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
    ];
    for (index, (title, description)) in options.iter().enumerate() {
        let selected = index == app.plan_modal_selected;
        lines.push(Line::from(vec![
            Span::styled(
                if selected { "> " } else { "  " },
                Style::default().fg(Color::Yellow),
            ),
            Span::styled(
                format!("{}: {}", index + 1, title),
                Style::default()
                    .fg(if selected { Color::Yellow } else { Color::Cyan })
                    .add_modifier(Modifier::BOLD),
            ),
        ]));
        lines.push(Line::from(format!("     {}", description)));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(
        "Esc keeps plan mode active without sending another turn.",
    ));
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
    let footer = Rect {
        x: inner.x,
        y: inner.y + inner.height.saturating_sub(1),
        width: inner.width,
        height: 1.min(inner.height),
    };
    frame.render_widget(
        Paragraph::new("[Up/Down] choose  [Enter] select  [1/2] quick select  [Esc] close"),
        footer,
    );
}

pub fn render_sessions_modal(app: &App, frame: &mut Frame, area: Rect) {
    if !app
        .interaction
        .is_overlay(crate::interaction::Overlay::Sessions)
    {
        return;
    }
    let popup_area = centered_rect(75, 65, area);
    frame.render_widget(Clear, popup_area);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Session Browser ")
        .border_style(Style::default().fg(Color::Magenta));
    let inner = block.inner(popup_area);
    frame.render_widget(block, popup_area);
    let header_area = Rect {
        x: inner.x,
        y: inner.y,
        width: inner.width,
        height: 1.min(inner.height),
    };
    let footer_height = 1.min(inner.height.saturating_sub(header_area.height));
    let footer_area = Rect {
        x: inner.x,
        y: inner.y + inner.height.saturating_sub(footer_height),
        width: inner.width,
        height: footer_height,
    };
    let list_area = Rect {
        x: inner.x,
        y: inner.y + header_area.height,
        width: inner.width,
        height: inner
            .height
            .saturating_sub(header_area.height + footer_height),
    };
    frame.render_widget(
        Paragraph::new("[Enter] resume  [D] delete  [E] export"),
        header_area,
    );
    let mut lines = Vec::new();
    if app.available_sessions.is_empty() {
        lines.push(Line::from("No saved sessions."));
    } else {
        for (index, session) in app.available_sessions.iter().enumerate() {
            let selected = index == app.sessions_selected;
            let marker = if selected { "◆ " } else { "  " };
            let modified = session
                .modified
                .duration_since(std::time::SystemTime::UNIX_EPOCH)
                .map(|duration| duration.as_secs())
                .unwrap_or(0);
            lines.push(Line::from(vec![
                Span::styled(marker, Style::default().fg(Color::Yellow)),
                Span::styled(
                    format!("{:<20}", session.name),
                    Style::default()
                        .fg(if selected { Color::Yellow } else { Color::Cyan })
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(format!(
                    " {} / {}  {} msgs  modified:{}",
                    session.provider, session.model, session.messages, modified
                )),
            ]));
        }
    }
    let paragraph = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .scroll((app.sessions_selected as u16, 0));
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

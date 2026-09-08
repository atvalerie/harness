use crate::app::App;
use crate::ui::markdown::parse_markdown;
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

pub fn render_chat(app: &App, frame: &mut Frame, area: Rect) {
    let mut lines: Vec<Line> = Vec::new();

    if app.messages.is_empty()
        && app.current_thought_buffer.is_empty()
        && app.current_response_buffer.is_empty()
    {
        lines.push(Line::from(vec![
            Span::styled(
                "Welcome to Holiday",
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(" [{}]", app.config.model),
                Style::default().fg(Color::Cyan),
            ),
        ]));
        if app.config.get_api_key_for_active_provider().is_some() {
            lines.push(Line::from(vec![Span::styled(
                "Ready. Type /help for commands or enter a prompt.",
                Style::default().fg(Color::DarkGray),
            )]));
        } else {
            lines.push(Line::from(vec![Span::styled(
                "You can explore without an API key.",
                Style::default().fg(Color::DarkGray),
            )]));
            lines.push(Line::from(vec![Span::styled(
                "For ChatGPT/Codex, run `holiday --login browser` or `holiday --login device`, then choose the codex provider.",
                Style::default().fg(Color::DarkGray),
            )]));
            lines.push(Line::from(vec![Span::styled(
                "For API keys, use /key <provider_api_key> or set the provider's environment variable.",
                Style::default().fg(Color::DarkGray),
            )]));
            lines.push(Line::from(vec![Span::styled(
                "Type /help for commands or enter a prompt.",
                Style::default().fg(Color::DarkGray),
            )]));
        }
    }

    for msg in &app.messages {
        match msg.role.as_str() {
            "user" => {
                lines.push(Line::from(""));
                lines.push(Line::from(vec![
                    Span::styled(
                        "❯ user",
                        Style::default()
                            .fg(Color::Green)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        format!("  {}", msg.timestamp),
                        Style::default().fg(Color::DarkGray),
                    ),
                ]));
                for l in msg.content.lines() {
                    lines.push(Line::from(vec![Span::styled(
                        format!("  {}", l),
                        Style::default().fg(Color::White),
                    )]));
                }
                for (index, attachment) in msg.attachments.iter().enumerate() {
                    let (kind, details) = match attachment.kind.as_str() {
                        "text" => {
                            let text = attachment.text.as_deref().unwrap_or("");
                            ("text", format!("{} chars", text.chars().count()))
                        }
                        "image" => (
                            "image",
                            attachment
                                .mime_type
                                .as_deref()
                                .unwrap_or("image")
                                .to_string(),
                        ),
                        other => (other, String::new()),
                    };
                    let label = format!(
                        "  [attachment {}: {} | {} | {}]",
                        index + 1,
                        kind,
                        attachment.name,
                        details
                    );
                    lines.push(Line::from(vec![Span::styled(
                        label,
                        Style::default().fg(Color::Yellow),
                    )]));
                }
            }
            "model" => {
                lines.push(Line::from(""));
                lines.push(Line::from(vec![
                    Span::styled(
                        "◆ assistant",
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        format!("  {}", msg.timestamp),
                        Style::default().fg(Color::DarkGray),
                    ),
                ]));
                // Markdown rendering for assistant messages
                let md_lines = parse_markdown(&msg.content, "  ");
                lines.extend(md_lines);
            }
            "thought" => {
                lines.push(Line::from(""));
                lines.push(Line::from(vec![
                    Span::styled("· thought", Style::default().fg(Color::Magenta)),
                    Span::styled(
                        format!("  {}", msg.timestamp),
                        Style::default().fg(Color::DarkGray),
                    ),
                ]));
                for l in msg.content.lines() {
                    lines.push(Line::from(vec![Span::styled(
                        format!("  │ {}", l),
                        Style::default().fg(Color::DarkGray),
                    )]));
                }
            }
            "tool" => {
                lines.push(Line::from(""));
                lines.push(Line::from(vec![
                    Span::styled("⚡ tool", Style::default().fg(Color::Yellow)),
                    Span::styled(
                        format!("  {}", msg.timestamp),
                        Style::default().fg(Color::DarkGray),
                    ),
                ]));
                let max_chars = area.width.saturating_sub(6).max(12) as usize;
                let compact = msg.content.split_whitespace().collect::<Vec<_>>().join(" ");
                let mut visible = compact.chars().take(max_chars).collect::<String>();
                if compact.chars().count() > max_chars {
                    visible.push('…');
                }
                lines.push(Line::from(vec![Span::styled(
                    format!("  {}", visible),
                    Style::default().fg(Color::Yellow),
                )]));
            }
            "system" => {
                lines.push(Line::from(""));
                lines.push(Line::from(vec![
                    Span::styled("— system", Style::default().fg(Color::Blue)),
                    Span::styled(
                        format!("  {}", msg.timestamp),
                        Style::default().fg(Color::DarkGray),
                    ),
                ]));
                for l in msg.content.lines() {
                    lines.push(Line::from(vec![Span::styled(
                        format!("  {}", l),
                        Style::default().fg(Color::DarkGray),
                    )]));
                }
            }
            _ => {}
        }
    }

    // Render active streaming thought buffer
    if !app.current_thought_buffer.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(vec![Span::styled(
            "Thinking... ",
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD | Modifier::ITALIC),
        )]));
        for l in app.current_thought_buffer.lines() {
            lines.push(Line::from(vec![Span::styled(
                format!("  │ {}", l),
                Style::default().fg(Color::DarkGray),
            )]));
        }
    }

    // Render active streaming response buffer with live markdown parsing
    if !app.current_response_buffer.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(vec![Span::styled(
            "Gemini (Streaming)... ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )]));
        let md_stream_lines = parse_markdown(&app.current_response_buffer, "  ");
        lines.extend(md_stream_lines);
    }

    if app.state != crate::app::EngineState::Idle
        && app.state != crate::app::EngineState::AwaitingHitlApproval
        && app.current_thought_buffer.is_empty()
        && app.current_response_buffer.is_empty()
    {
        let label = match app.state {
            crate::app::EngineState::Compacting => "compacting context",
            crate::app::EngineState::ExecutingTool => "running tool",
            _ => "waiting for response",
        };
        lines.push(Line::from(vec![Span::styled(
            format!("{} {}…", crate::ui::status::spinner(app), label),
            Style::default()
                .fg(Color::LightCyan)
                .add_modifier(Modifier::BOLD),
        )]));
    }

    // Bottom breathing room
    lines.push(Line::from(""));

    // Pre-wrap lines to exact inner width so line count matches visual layout 1:1
    let inner_width = area.width.saturating_sub(2).max(10) as usize;
    let mut visual_lines: Vec<Line<'static>> = Vec::new();
    for line in lines {
        visual_lines.extend(wrap_line(line, inner_width));
    }

    let viewport_height = area.height.saturating_sub(2) as usize;
    let total_visual_lines = visual_lines.len();

    let max_scroll = if total_visual_lines > viewport_height {
        total_visual_lines.saturating_sub(viewport_height)
    } else {
        0
    };

    // app.chat_scroll represents how many lines UP from the bottom we have scrolled.
    // 0 = bottom (most recent).
    let effective_scroll = app.chat_scroll.min(max_scroll);
    let scroll_y = max_scroll.saturating_sub(effective_scroll);

    let scroll_indicator = if max_scroll > 0 {
        if effective_scroll == 0 {
            " [Bottom] ".to_string()
        } else {
            format!(" [Scroll: -{} lines | Esc to bottom] ", effective_scroll)
        }
    } else {
        String::new()
    };

    let title = if scroll_indicator.is_empty() {
        " Conversation ".to_string()
    } else {
        format!(" Conversation{} ", scroll_indicator)
    };

    let paragraph = Paragraph::new(visual_lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(Span::styled(title, Style::default().fg(Color::DarkGray)))
                .border_style(Style::default().fg(Color::DarkGray)),
        )
        .scroll((scroll_y as u16, 0));

    frame.render_widget(paragraph, area);
}

fn wrap_line(line: Line<'static>, max_width: usize) -> Vec<Line<'static>> {
    if line.width() <= max_width || max_width == 0 {
        return vec![line];
    }

    let mut result = Vec::new();
    let mut current_spans: Vec<Span<'static>> = Vec::new();
    let mut current_line_width = 0;

    for span in line.spans {
        let style = span.style;
        let text = span.content;

        let chars: Vec<(usize, char)> = text.char_indices().collect();
        let len = chars.len();

        let mut i = 0;
        while i < len {
            let is_space = chars[i].1.is_whitespace();
            let start_byte = chars[i].0;
            while i < len && chars[i].1.is_whitespace() == is_space {
                i += 1;
            }
            let end_byte = if i < len { chars[i].0 } else { text.len() };
            let token = &text[start_byte..end_byte];
            let token_width = token.chars().count();

            if current_line_width + token_width <= max_width {
                current_spans.push(Span::styled(token.to_string(), style));
                current_line_width += token_width;
            } else if is_space {
                if !current_spans.is_empty() {
                    result.push(Line::from(std::mem::take(&mut current_spans)));
                    current_line_width = 0;
                }
            } else {
                if !current_spans.is_empty() {
                    result.push(Line::from(std::mem::take(&mut current_spans)));
                    current_line_width = 0;
                }

                if token_width > max_width {
                    let mut chunk = String::new();
                    let mut chunk_w = 0;
                    for c in token.chars() {
                        if chunk_w + 1 > max_width {
                            result.push(Line::from(vec![Span::styled(chunk, style)]));
                            chunk = String::new();
                            chunk_w = 0;
                        }
                        chunk.push(c);
                        chunk_w += 1;
                    }
                    if !chunk.is_empty() {
                        current_spans.push(Span::styled(chunk, style));
                        current_line_width = chunk_w;
                    }
                } else {
                    current_spans.push(Span::styled(token.to_string(), style));
                    current_line_width = token_width;
                }
            }
        }
    }

    if !current_spans.is_empty() {
        result.push(Line::from(current_spans));
    }

    if result.is_empty() {
        vec![Line::from("")]
    } else {
        result
    }
}

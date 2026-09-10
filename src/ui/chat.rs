use crate::app::App;
use crate::ui::markdown::parse_markdown;
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

#[derive(Default)]
pub struct TranscriptCache {
    entries: Vec<(u64, Vec<Line<'static>>)>,
    last_scroll: usize,
    anchor: Option<usize>,
}

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
        if app.interaction.credentials_available {
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

    let mut cache = app.interaction.cache.borrow_mut();
    for (index, msg) in app.messages.iter().enumerate() {
        if app
            .interaction
            .focus_message
            .is_some_and(|first| index < first)
        {
            continue;
        }
        use std::hash::{Hash, Hasher};
        if !app.interaction.transcript_query.is_empty()
            && !msg
                .content
                .to_lowercase()
                .contains(&app.interaction.transcript_query.to_lowercase())
        {
            continue;
        }
        let expanded = app.interaction.expanded.contains(&index);
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        msg.content.hash(&mut hasher);
        msg.role.hash(&mut hasher);
        msg.timestamp.hash(&mut hasher);
        serde_json::to_string(&msg.attachments)
            .unwrap_or_default()
            .hash(&mut hasher);
        if expanded && msg.role == "tool" {
            app.messages
                .get(index + 1)
                .map(|m| &m.content)
                .hash(&mut hasher);
        }
        expanded.hash(&mut hasher);
        area.width.hash(&mut hasher);
        let key = hasher.finish();
        if let Some((old, rendered)) = cache.entries.get(index) {
            if *old == key {
                lines.extend(rendered.iter().cloned());
                continue;
            }
        }
        let mut item_lines: Vec<Line<'static>> = Vec::new();

        match msg.role.as_str() {
            "user" => {
                item_lines.push(Line::from(""));
                item_lines.push(Line::from(vec![
                    Span::styled(
                        "❯ user",
                        Style::default()
                            .fg(Color::Green)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        format!("  {}  #{}", msg.timestamp, index + 1),
                        Style::default().fg(Color::DarkGray),
                    ),
                ]));
                for l in msg.content.lines() {
                    item_lines.push(Line::from(vec![Span::styled(
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
                    item_lines.push(Line::from(vec![Span::styled(
                        label,
                        Style::default().fg(Color::Yellow),
                    )]));
                }
            }
            "model" => {
                item_lines.push(Line::from(""));
                item_lines.push(Line::from(vec![
                    Span::styled(
                        "◆ assistant",
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        format!("  {}  #{}", msg.timestamp, index + 1),
                        Style::default().fg(Color::DarkGray),
                    ),
                ]));
                // Markdown rendering for assistant messages
                let md_lines = parse_markdown(&msg.content, "  ");
                item_lines.extend(md_lines);
            }
            "thought" => {
                item_lines.push(Line::from(""));
                item_lines.push(Line::from(vec![
                    Span::styled("· thought", Style::default().fg(Color::Magenta)),
                    Span::styled(
                        format!("  {}  #{}", msg.timestamp, index + 1),
                        Style::default().fg(Color::DarkGray),
                    ),
                ]));
                if !expanded {
                    item_lines.push(Line::from(format!(
                        "  {} ...",
                        msg.content.chars().take(100).collect::<String>()
                    )));
                }
            }
            "tool" => {
                item_lines.push(Line::from(""));
                item_lines.push(Line::from(vec![
                    Span::styled("⚡ tool", Style::default().fg(Color::Yellow)),
                    Span::styled(
                        format!("  {}  #{}", msg.timestamp, index + 1),
                        Style::default().fg(Color::DarkGray),
                    ),
                ]));
                let max_chars = area.width.saturating_sub(6).max(12) as usize;
                let compact = msg.content.split_whitespace().collect::<Vec<_>>().join(" ");
                let mut visible = compact.chars().take(max_chars).collect::<String>();
                if compact.chars().count() > max_chars {
                    visible.push('…');
                }
                item_lines.push(Line::from(vec![Span::styled(
                    format!("  {}", visible),
                    Style::default().fg(Color::Yellow),
                )]));
            }
            "system" => {
                if msg.content.starts_with("Error:") {
                    item_lines.push(Line::from("  Recovery: /retry | /fork | /review"));
                }
                item_lines.push(Line::from(""));
                item_lines.push(Line::from(vec![
                    Span::styled("— system", Style::default().fg(Color::Blue)),
                    Span::styled(
                        format!("  {}  #{}", msg.timestamp, index + 1),
                        Style::default().fg(Color::DarkGray),
                    ),
                ]));
                for l in msg.content.lines() {
                    item_lines.push(Line::from(vec![Span::styled(
                        format!("  {}", l),
                        Style::default().fg(Color::DarkGray),
                    )]));
                }
            }
            _ => {}
        }
        if expanded && msg.role == "tool" {
            if let Some(output) = app.messages.get(index + 1).filter(|m| m.role == "function") {
                if let Ok(crate::client::types::Part::FunctionResponse { function_response }) =
                    serde_json::from_str(&output.content)
                {
                    if let Some(id) = &function_response.id {
                        for call in app.messages[..index].iter().rev() {
                            if let Ok(crate::client::types::Part::FunctionCall {
                                function_call,
                                ..
                            }) = serde_json::from_str(&call.content)
                            {
                                if function_call.id.as_ref() == Some(id) {
                                    item_lines.push(Line::from("  Arguments:"));
                                    item_lines.extend(
                                        serde_json::to_string_pretty(&function_call.args)
                                            .unwrap_or_default()
                                            .lines()
                                            .map(|s| Line::from(format!("  {s}"))),
                                    );
                                    break;
                                }
                            }
                        }
                    }
                    item_lines.push(Line::from("  Result:"));
                    let result = function_response
                        .response
                        .get("output")
                        .or_else(|| function_response.response.get("error"))
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string)
                        .unwrap_or_else(|| {
                            serde_json::to_string_pretty(&function_response.response)
                                .unwrap_or_default()
                        });
                    item_lines.extend(result.lines().map(|s| Line::from(format!("  {s}"))));
                } else {
                    item_lines.extend(output.content.lines().map(|s| Line::from(format!("  {s}"))));
                }
            }
        }
        if expanded
            && matches!(
                msg.role.as_str(),
                "tool" | "function" | "thought" | "model_tool_call"
            )
        {
            item_lines.extend(
                msg.content
                    .lines()
                    .map(|line| Line::from(format!("  {line}"))),
            );
        }
        while cache.entries.len() <= index {
            cache.entries.push((0, Vec::new()));
        }
        cache.entries[index] = (key, item_lines.clone());
        lines.extend(item_lines);
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
            format!("{} (Streaming)... ", app.config.model),
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
    let inner_width = area.width.saturating_sub(2).max(1) as usize;
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
    let scroll_y = if app.chat_scroll == 0 {
        cache.anchor = None;
        max_scroll
    } else if cache.last_scroll == app.chat_scroll {
        cache
            .anchor
            .unwrap_or(max_scroll.saturating_sub(effective_scroll))
            .min(max_scroll)
    } else {
        max_scroll.saturating_sub(effective_scroll)
    };
    if app.chat_scroll > 0 {
        cache.anchor = Some(scroll_y);
    }
    cache.last_scroll = app.chat_scroll;

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

    let paragraph = Paragraph::new(
        visual_lines
            .into_iter()
            .skip(scroll_y)
            .take(viewport_height)
            .collect::<Vec<_>>(),
    )
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title(Span::styled(title, Style::default().fg(Color::DarkGray)))
            .border_style(Style::default().fg(Color::DarkGray)),
    );

    frame.render_widget(paragraph, area);
}

fn wrap_line(line: Line<'static>, max_width: usize) -> Vec<Line<'static>> {
    use unicode_segmentation::UnicodeSegmentation;
    use unicode_width::UnicodeWidthStr;
    let max_width = max_width.max(1);
    let mut result = Vec::new();
    let mut spans = Vec::new();
    let mut width = 0;
    for span in line.spans {
        for glyph in span.content.graphemes(true) {
            let cells = UnicodeWidthStr::width(glyph);
            if width + cells > max_width && !spans.is_empty() {
                result.push(Line::from(std::mem::take(&mut spans)));
                width = 0;
            }
            if cells > max_width {
                continue;
            }
            spans.push(Span::styled(glyph.to_owned(), span.style));
            width += cells;
        }
    }
    if !spans.is_empty() || result.is_empty() {
        result.push(Line::from(spans));
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn wrapping_respects_terminal_cells() {
        for width in 1..12 {
            for line in wrap_line(Line::from("abc 漢字 👩‍💻 é longtext"), width) {
                assert!(line.width() <= width);
            }
        }
    }
    #[test]
    fn expanded_tool_shows_arguments_and_readable_result_next_to_numbered_timestamp() {
        let mut app = App::new(crate::config::AppConfig::default(), "".into());
        app.add_message(
            "model_tool_call",
            r#"{"functionCall":{"name":"read_file","args":{"path":"fixture.rs"},"id":"call-1"}}"#,
        );
        app.add_message("tool", "read_file completed");
        app.add_message("function", r#"{"functionResponse":{"name":"read_file","response":{"output":"line one\nline two"},"id":"call-1"}}"#);
        app.interaction.expanded.insert(1);
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| render_chat(&app, f, f.area())).unwrap();
        let text = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect::<String>();
        assert!(text.contains("#2"));
        assert!(!text.contains("/inspect"));
        assert!(text.contains("Arguments:"));
        assert!(text.contains("fixture.rs"));
        assert!(text.contains("line one"));
        assert!(text.contains("line two"));
    }
}

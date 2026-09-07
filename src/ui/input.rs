use crate::app::{App, DraftAttachment};
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

pub fn input_height(app: &App, width: u16) -> u16 {
    let content_width = width.saturating_sub(4).max(1) as usize;
    let text_rows = input_text_rows(&app.input_buffer, content_width);
    let attachment_rows = usize::from(!app.draft_attachments.is_empty());
    (text_rows + attachment_rows + 2).clamp(3, 10) as u16
}

pub fn render_input(app: &App, frame: &mut Frame, area: Rect) {
    let content_width = area.width.saturating_sub(4).max(1) as usize;
    let cursor = app.input_cursor.min(app.input_buffer.chars().count());
    let mut lines = input_text_lines(&app.input_buffer, content_width);

    if !app.draft_attachments.is_empty() {
        let mut chips = vec![Span::styled(
            "  blocks: ",
            Style::default().fg(Color::DarkGray),
        )];
        for (index, attachment) in app.draft_attachments.iter().enumerate() {
            let (label, color) = match attachment {
                DraftAttachment::Text { text, .. } => (
                    format!("[{} text:{}c]", index + 1, text.chars().count()),
                    Color::Yellow,
                ),
                DraftAttachment::Image { .. } => (format!("[{} image]", index + 1), Color::Magenta),
            };
            chips.push(Span::styled(
                format!("{} ", label),
                Style::default().fg(color),
            ));
        }
        lines.push(Line::from(chips));
    }

    let (cursor_row, cursor_col) = cursor_position(&app.input_buffer, cursor, content_width);
    let viewport_height = area.height.saturating_sub(2) as usize;
    let scroll = cursor_row.saturating_sub(viewport_height.saturating_sub(1));
    let status = app.status_message.as_deref().unwrap_or("");
    let status = status.chars().take(80).collect::<String>();
    let hint = if app.draft_attachments.is_empty() {
        " Ctrl+Enter/Ctrl+J: newline "
    } else {
        " Ctrl+Enter/Ctrl+J: newline | Ctrl+X: remove last attachment "
    };
    let paragraph = Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title_top(Line::from(Span::styled(
                    " Prompt ",
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                )))
                .title_top(
                    Line::from(Span::styled(
                        format!(" {} ", status),
                        Style::default().fg(Color::Yellow),
                    ))
                    .right_aligned(),
                )
                .title_bottom(
                    Line::from(Span::styled(hint, Style::default().fg(Color::DarkGray)))
                        .right_aligned(),
                )
                .border_style(Style::default().fg(Color::DarkGray)),
        )
        .scroll((scroll as u16, 0));
    frame.render_widget(paragraph, area);

    let cursor_x = area.x + 1 + 2 + cursor_col as u16;
    let cursor_y = area.y + 1 + cursor_row.saturating_sub(scroll) as u16;
    if cursor_x < area.x + area.width.saturating_sub(1)
        && cursor_y < area.y + area.height.saturating_sub(1)
    {
        frame.set_cursor_position((cursor_x, cursor_y));
    }
}

fn input_text_rows(input: &str, content_width: usize) -> usize {
    let width = content_width.max(1);
    input
        .split('\n')
        .map(|line| ((line.chars().count() + width - 1) / width).max(1))
        .sum()
}

fn input_text_lines(input: &str, content_width: usize) -> Vec<Line<'static>> {
    let width = content_width.max(1);
    let mut lines = Vec::new();
    for (logical_index, logical_line) in input.split('\n').enumerate() {
        let prefix = if logical_index == 0 { "> " } else { "  " };
        let prefix_style = if logical_index == 0 {
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        let chars = logical_line.chars().collect::<Vec<_>>();
        if chars.is_empty() {
            lines.push(Line::from(Span::styled(prefix, prefix_style)));
            continue;
        }
        for (chunk_index, chunk) in chars.chunks(width).enumerate() {
            let continuation = if chunk_index == 0 { prefix } else { "  " };
            let continuation_style = if chunk_index == 0 {
                prefix_style
            } else {
                Style::default().fg(Color::DarkGray)
            };
            lines.push(Line::from(vec![
                Span::styled(continuation, continuation_style),
                Span::styled(
                    chunk.iter().collect::<String>(),
                    Style::default().fg(Color::White),
                ),
            ]));
        }
    }
    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            "> ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )));
    }
    lines
}

fn cursor_position(input: &str, cursor: usize, width: usize) -> (usize, usize) {
    let width = width.max(1);
    let mut row = 0;
    let mut col = 0;
    let mut seen = 0;
    for character in input.chars() {
        if seen >= cursor {
            break;
        }
        seen += 1;
        if character == '\n' {
            row += 1;
            col = 0;
        } else {
            col += 1;
            if col >= width {
                row += col / width;
                col %= width;
            }
        }
    }
    (row, col)
}

#[cfg(test)]
mod tests {
    use super::{cursor_position, input_text_lines, input_text_rows};

    #[test]
    fn explicit_newline_keeps_following_text_on_a_visible_row() {
        assert_eq!(input_text_rows("first\nsecond", 20), 2);
        assert_eq!(input_text_lines("first\nsecond", 20).len(), 2);
        assert_eq!(cursor_position("first\nsecond", 8, 20), (1, 2));
    }

    #[test]
    fn long_lines_use_the_same_manual_wrap_as_the_cursor() {
        assert_eq!(input_text_rows("abcdef", 3), 2);
        assert_eq!(input_text_lines("abcdef", 3).len(), 2);
        assert_eq!(cursor_position("abcdef", 4, 3), (1, 1));
    }
}

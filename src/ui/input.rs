use crate::app::{App, DraftAttachment};
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Paragraph},
    Frame,
};

pub fn input_height(app: &App, width: u16) -> u16 {
    let content_width = width.saturating_sub(4).max(1) as usize;
    let text_rows = input_text_rows(&app.input_buffer, content_width)
        .max(cursor_position(&app.input_buffer, app.input_cursor, content_width).0 + 1);
    let attachment_rows = usize::from(!app.draft_attachments.is_empty());
    (text_rows + attachment_rows + 2).clamp(3, 10) as u16
}

pub fn render_input(app: &App, frame: &mut Frame, area: Rect) {
    let content_width = area.width.saturating_sub(4).max(1) as usize;
    let cursor = app.input_cursor.min(app.input_buffer.chars().count());
    let mut lines = input_text_lines(&app.input_buffer, content_width);
    let needed = cursor_position(&app.input_buffer, cursor, content_width).0 + 1;
    while lines.len() < needed {
        lines.push(Line::from("  "));
    }

    if !app.draft_attachments.is_empty() {
        let mut chips = vec![Span::styled(
            "  [+] ",
            Style::default().fg(Color::DarkGray),
        )];
        for (index, attachment) in app.draft_attachments.iter().enumerate() {
            let (label, color) = match attachment {
                DraftAttachment::Text { text, .. } => (
                    format!("[#{} text:{}c]", index + 1, text.chars().count()),
                    Color::Yellow,
                ),
                DraftAttachment::Image { .. } => (format!("[#{} image]", index + 1), Color::Magenta),
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
    let mut status_width = 0;
    let status = status
        .chars()
        .take_while(|ch| {
            status_width += unicode_width::UnicodeWidthChar::width(*ch).unwrap_or(0);
            status_width <= area.width.saturating_sub(14) as usize
        })
        .collect::<String>();

    let mut top_titles = vec![
        Span::styled(
            " prompt ",
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
    ];
    if app.auto_mode {
        top_titles.push(Span::styled(
            "[auto] ",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ));
    }

    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::DarkGray))
        .title_top(Line::from(top_titles));

    if !status.is_empty() {
        block = block.title_top(
            Line::from(vec![
                Span::styled("* ", Style::default().fg(Color::Yellow)),
                Span::styled(format!("{} ", status), Style::default().fg(Color::Yellow)),
            ])
            .right_aligned(),
        );
    }

    let hint_line = render_keybind_hints(app, area.width as usize);
    block = block.title_bottom(hint_line);

    let paragraph = Paragraph::new(lines)
        .block(block)
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

fn render_keybind_hints(app: &App, width: usize) -> Line<'static> {
    let mut spans = Vec::new();
    let is_compact = width < 75;
    let is_narrow = width < 95;

    let items: Vec<(&str, &str)> = if !app.draft_attachments.is_empty() {
        if is_compact {
            vec![("↵", "send"), ("^X", "drop"), ("⇧↵", "newline")]
        } else {
            vec![
                ("Enter", "send"),
                ("Ctrl+X", "remove block"),
                ("Shift+Enter", "newline"),
                ("Ctrl+K", "commands"),
            ]
        }
    } else if app.chat_scroll > 0 {
        if is_compact {
            vec![("Esc", "bottom"), ("PgUp/Dn", "scroll"), ("↵", "send")]
        } else {
            vec![
                ("Esc", "scroll to bottom"),
                ("PgUp/PgDn", "scroll"),
                ("Enter", "send"),
            ]
        }
    } else if is_compact {
        vec![("↵", "send"), ("/", "commands"), ("^R", "history")]
    } else if is_narrow {
        vec![
            ("Enter", "send"),
            ("Shift+Enter", "newline"),
            ("/", "commands"),
            ("Ctrl+R", "history"),
        ]
    } else {
        vec![
            ("Enter", "send"),
            ("Shift+Enter", "newline"),
            ("/", "commands"),
            ("Ctrl+R", "history"),
            ("Ctrl+K", "palette"),
        ]
    };

    for (i, (key, desc)) in items.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(" · ", Style::default().fg(Color::DarkGray)));
        }
        spans.push(Span::styled(
            *key,
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(
            format!(" {}", desc),
            Style::default().fg(Color::DarkGray),
        ));
    }
    spans.push(Span::raw(" "));
    Line::from(spans).right_aligned()
}

fn wrapped_rows(input: &str, width: usize) -> Vec<String> {
    use unicode_segmentation::UnicodeSegmentation;
    use unicode_width::UnicodeWidthStr;
    let width = width.max(1);
    let mut rows = Vec::new();
    for logical in input.split('\n') {
        let mut row = String::new();
        let mut cells = 0;
        for glyph in logical.graphemes(true) {
            let n = UnicodeWidthStr::width(glyph);
            if cells + n > width && !row.is_empty() {
                rows.push(std::mem::take(&mut row));
                cells = 0;
            }
            row.push_str(glyph);
            cells += n;
        }
        rows.push(row);
    }
    rows
}
fn input_text_rows(input: &str, width: usize) -> usize {
    wrapped_rows(input, width).len()
}
fn input_text_lines(input: &str, width: usize) -> Vec<Line<'static>> {
    wrapped_rows(input, width)
        .into_iter()
        .enumerate()
        .map(|(i, row)| {
            Line::from(vec![
                Span::styled(
                    if i == 0 { "> " } else { "  " },
                    Style::default().fg(Color::Cyan),
                ),
                Span::raw(row),
            ])
        })
        .collect()
}
fn cursor_position(input: &str, cursor: usize, width: usize) -> (usize, usize) {
    use unicode_segmentation::UnicodeSegmentation;
    use unicode_width::UnicodeWidthStr;
    let width = width.max(1);
    let mut row = 0;
    let mut col = 0;
    let mut seen = 0;
    for glyph in input.graphemes(true) {
        if seen >= cursor {
            break;
        }
        seen += glyph.chars().count();
        if glyph == "\n" {
            row += 1;
            col = 0;
        } else {
            let n = UnicodeWidthStr::width(glyph);
            if col + n > width {
                row += 1;
                col = 0;
            }
            col += n;
        }
    }
    if col == width {
        (row + 1, 0)
    } else {
        (row, col)
    }
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

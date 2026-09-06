use crate::app::App;
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

pub fn render_input(app: &App, frame: &mut Frame, area: Rect) {
    let prompt_prefix = Span::styled("❯ ", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD));

    // Calculate inner width available for text (area.width - borders(2) - prompt prefix(2) = area.width - 4)
    let inner_width = area.width.saturating_sub(4) as usize;

    // Handle horizontal scrolling / cursor tracking so long prompts stay visible around the cursor
    let chars: Vec<char> = app.input_buffer.chars().collect();
    let cursor = app.input_cursor.min(chars.len());

    let mut start = 0;
    if cursor >= inner_width {
        start = cursor.saturating_sub(inner_width.saturating_sub(1));
    }
    let end = (start + inner_width).min(chars.len());
    if end - start < inner_width && start > 0 {
        start = end.saturating_sub(inner_width);
    }

    let visible_chars: String = chars.iter().skip(start).take(end - start).collect();
    let input_text = Span::styled(visible_chars, Style::default().fg(Color::White));

    // Command autocompletion ghost hint when typing a slash command (only when cursor is at the end)
    let ghost_hint = if cursor == chars.len() && app.input_buffer.starts_with('/') && !app.input_buffer.contains(' ') {
        let commands = [
            "/help - show available commands",
            "/models - query live models & pricing",
            "/provider <gemini|openai> - select API provider",
            "/baseurl <url|default> - set provider base URL",
            "/reasoning <on|off|budget> - configure active model reasoning",
            "/autocompact <on|off|tokens> - configure automatic compaction",
            "/session <save|clear|path> - manage resumable session",
            "/sessions - browse saved sessions",
            "/model <name> - switch active model",
            "/compact - summarize history to reclaim context",
            "/thinking <tokens> - set reasoning budget",
            "/temp <0.0-2.0> - adjust temperature",
            "/sys <instruction> - update system prompt",
            "/key <api_key> - store API key in vault",
            "/copy - copy last assistant response to clipboard",
            "/clear - reset session messages",
            "/save - save configuration to disk",
            "/quit - exit harness",
        ];

        let prefix = app.input_buffer.to_lowercase();
        commands
            .iter()
            .find(|cmd| cmd.starts_with(&prefix))
            .map(|match_cmd| {
                let rest = &match_cmd[prefix.len()..];
                Span::styled(rest, Style::default().fg(Color::DarkGray))
            })
    } else {
        None
    };

    let mut spans = vec![prompt_prefix, input_text];
    if let Some(hint) = ghost_hint {
        spans.push(hint);
    }

    let line = Line::from(spans);
    let paragraph = Paragraph::new(line)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::DarkGray)),
        );

    frame.render_widget(paragraph, area);

    // Set cursor position inside input block (accounting for border and prefix "❯ ", plus horizontal scrolling offset)
    let cursor_rel = cursor.saturating_sub(start);
    let cursor_x = area.x + 1 + 2 + cursor_rel as u16;
    let cursor_y = area.y + 1;
    if cursor_x < area.x + area.width - 1 {
        frame.set_cursor_position((cursor_x, cursor_y));
    }
}

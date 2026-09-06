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
    let input_text = Span::styled(&app.input_buffer, Style::default().fg(Color::White));

    // Command autocompletion ghost hint when typing a slash command
    let ghost_hint = if app.input_buffer.starts_with('/') && !app.input_buffer.contains(' ') {
        let commands = [
            "/help - show available commands",
            "/models - query live models & pricing",
            "/provider <gemini|openai> - select API provider",
            "/baseurl <url|default> - set provider base URL",
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

    // Set cursor position inside input block (accounting for border and prefix "❯ ")
    let cursor_x = area.x + 1 + 2 + app.input_cursor as u16;
    let cursor_y = area.y + 1;
    if cursor_x < area.x + area.width - 1 {
        frame.set_cursor_position((cursor_x, cursor_y));
    }
}

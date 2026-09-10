pub mod chat;
pub mod input;
pub mod markdown;
pub mod modal;
pub mod status;

use crate::app::App;
use ratatui::{
    layout::{Constraint, Direction, Layout},
    Frame,
};

pub fn render(app: &App, frame: &mut Frame) {
    // An overlay owns the frame, including the cursor. Never leave transcript
    // cells or the underlying prompt cursor behind its content.
    if app.interaction.top().is_some() {
        frame.render_widget(ratatui::widgets::Clear, frame.area());
        modal::render_models_modal(app, frame, frame.area());
        modal::render_thinking_modal(app, frame, frame.area());
        modal::render_plan_modal(app, frame, frame.area());
        modal::render_sessions_modal(app, frame, frame.area());
        modal::render_permissions_modal(app, frame, frame.area());
        render_workspace_overlay(app, frame);
        return;
    }
    let outer = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(frame.area());
    status::render_usage(app, frame, outer[1]);
    let status_bar_at_bottom = app.config.status_bar_at_bottom();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            if status_bar_at_bottom {
                Constraint::Min(5)
            } else {
                Constraint::Length(1)
            },
            if status_bar_at_bottom {
                Constraint::Length(input::input_height(app, frame.area().width))
            } else {
                Constraint::Min(5)
            },
            if status_bar_at_bottom {
                Constraint::Length(1)
            } else {
                Constraint::Length(input::input_height(app, frame.area().width))
            },
        ])
        .split(outer[0]);

    let (status_area, chat_area, input_area) = if status_bar_at_bottom {
        (chunks[2], chunks[0], chunks[1])
    } else {
        (chunks[0], chunks[1], chunks[2])
    };

    status::render_status(app, frame, status_area);

    if let Some(pending) = app
        .pending_tool_call
        .as_ref()
        .filter(|_| app.state == crate::app::EngineState::AwaitingHitlApproval)
    {
        // Reserve enough space for the approval explanation, exact command,
        // metadata, and footer. A five-line panel clips the command before the
        // user can review it, especially when the model omits optional text.
        let panel_area = chat_area.union(input_area);
        let preview = &pending.preview;
        let command_lines = preview
            .command
            .as_deref()
            .map(|command| command.split('\n').count())
            .unwrap_or(0);
        let structured_lines = {
            // Why + explanation + expected effect + explanation + command
            // heading + command lines + spacing between sections.
            8usize.saturating_add(command_lines)
        };
        let content_lines = structured_lines
            .saturating_add(preview.details.len())
            .saturating_add(preview.diff_hunks.len());
        let desired_height = content_lines.saturating_add(3).clamp(12, 24) as u16;
        let hitl_height = desired_height.min(panel_area.height.saturating_sub(3).max(1));

        let lower_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(3), // Main chat log viewport
                Constraint::Length(hitl_height),
            ])
            .split(panel_area);

        chat::render_chat(app, frame, lower_chunks[0]);
        modal::render_hitl_modal(app, frame, lower_chunks[1]);
    } else {
        // Normal mode
        let suggestions = app.slash_suggestions();
        let areas = Layout::vertical([
            Constraint::Min(1),
            Constraint::Length(if suggestions.is_empty() {
                0
            } else {
                (suggestions.len().min(6) as u16 + 2).min(chat_area.height.saturating_sub(1))
            }),
        ])
        .split(chat_area);
        chat::render_chat(app, frame, areas[0]);
        if !suggestions.is_empty() {
            let selected = app
                .interaction
                .suggestion_selected
                .min(suggestions.len() - 1);
            let count = areas[1].height.saturating_sub(2) as usize;
            let start = selected.saturating_sub(count.saturating_sub(1));
            let lines = suggestions
                .iter()
                .enumerate()
                .skip(start)
                .take(count)
                .map(|(i, spec)| {
                    ratatui::text::Line::styled(
                        format!(
                            "{} {}  {}",
                            if i == selected { ">" } else { " " },
                            spec.name,
                            spec.description
                        ),
                        ratatui::style::Style::default().fg(if i == selected {
                            ratatui::style::Color::Cyan
                        } else {
                            ratatui::style::Color::Gray
                        }),
                    )
                })
                .collect::<Vec<_>>();
            frame.render_widget(
                ratatui::widgets::Paragraph::new(lines).block(
                    ratatui::widgets::Block::bordered()
                        .title(" Commands | Up/Down select | Tab insert | Esc hide "),
                ),
                areas[1],
            );
        }
        input::render_input(app, frame, input_area);
    }
}

pub fn render_workspace_overlay(app: &App, frame: &mut Frame) {
    use crate::interaction::Overlay;
    use ratatui::{
        layout::Rect,
        style::{Color, Style},
        text::{Line, Span},
        widgets::{Block, Borders, Clear, Paragraph},
    };
    let Some(overlay) = app.interaction.top() else {
        return;
    };
    let (title, rows) = match overlay {
        Overlay::Palette => (
            "Commands - Enter inserts, Esc closes",
            crate::commands::matching(&app.interaction.query)
                .iter()
                .map(|s| {
                    format!(
                        "{}  {}{}",
                        s.name,
                        s.description,
                        if s.idle_only { " [idle]" } else { "" }
                    )
                })
                .collect::<Vec<_>>(),
        ),
        Overlay::History => (
            "Prompt history - Enter restores",
            app.input_history
                .iter()
                .rev()
                .filter(|s| s.contains(&app.interaction.query))
                .cloned()
                .collect(),
        ),
        Overlay::Tasks => (
            "Tasks - type task-id to inspect; cancel task-id + Enter",
            if app.interaction.query.starts_with("task-") {
                app.tool_registry.tasks.inspect(&app.interaction.query)
            } else {
                app.tool_registry.tasks.summary()
            }
            .lines()
            .map(str::to_string)
            .collect(),
        ),
        Overlay::Review => (
            "Changes - type checkpoint number for diff; /rollback ID to restore",
            if let Ok(id) = app.interaction.query.parse() {
                app.review.diff(id)
            } else {
                app.review.summary()
            }
            .lines()
            .map(str::to_string)
            .collect(),
        ),
        _ => return,
    };
    let bounds = frame.area();
    let area = Rect::new(
        bounds.x + bounds.width.min(4) / 2,
        bounds.y + bounds.height.min(4) / 2,
        bounds.width.saturating_sub(4),
        bounds.height.saturating_sub(4),
    );
    frame.render_widget(Clear, area);
    let selected = app.interaction.selected.min(rows.len().saturating_sub(1));
    let height = area.height.saturating_sub(4) as usize;
    let start = selected.saturating_sub(height.saturating_sub(1));
    let mut lines = vec![
        Line::from(format!("> {}", app.interaction.query)),
        Line::from(""),
    ];
    lines.extend(
        rows.iter()
            .enumerate()
            .skip(start)
            .take(height)
            .map(|(i, s)| {
                Line::from(Span::styled(
                    s.clone(),
                    if i == selected {
                        Style::default().fg(Color::Cyan)
                    } else {
                        Style::default()
                    },
                ))
            }),
    );
    frame.render_widget(
        Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(title)),
        area,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn overlays_render_on_narrow_and_normal_terminals() {
        for (width, height) in [(1, 1), (20, 6), (80, 24), (120, 40)] {
            let mut app = App::new(crate::config::AppConfig::default(), "test-key".into());
            app.add_message("user", "hello");
            app.add_message("model", "# Response\n```rust\nlet x = 1;\n```");
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
            for overlay in [
                crate::interaction::Overlay::Palette,
                crate::interaction::Overlay::Tasks,
                crate::interaction::Overlay::Review,
                crate::interaction::Overlay::Models,
            ] {
                app.interaction.set_overlay(overlay, true);
                terminal.draw(|frame| render(&app, frame)).unwrap();
            }
        }
    }
    #[test]
    fn palette_contains_generated_commands() {
        let mut app = App::new(crate::config::AppConfig::default(), "test".into());
        app.open_palette();
        app.interaction.query = "rollback".into();
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 25)).unwrap();
        terminal.draw(|f| render(&app, f)).unwrap();
        let text = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect::<String>();
        assert!(text.contains("/rollback"));
    }

    #[test]
    fn overlays_hide_transcript_and_underlying_cursor() {
        let mut app = App::new(crate::config::AppConfig::default(), "".into());
        app.add_message("model", "TRANSCRIPT_SENTINEL");
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        for overlay in [
            crate::interaction::Overlay::History,
            crate::interaction::Overlay::Palette,
        ] {
            app.interaction.set_overlay(overlay, true);
            terminal.draw(|f| render(&app, f)).unwrap();
            let text = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|c| c.symbol())
                .collect::<String>();
            assert!(!text.contains("TRANSCRIPT_SENTINEL"));
        }
    }

    #[test]
    fn slash_suggestions_appear_without_tab() {
        let mut app = App::new(crate::config::AppConfig::default(), "".into());
        app.interaction.back();
        app.input_buffer = "/ret".into();
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| render(&app, f)).unwrap();
        let text = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect::<String>();
        assert!(text.contains("/retry"));
    }
}

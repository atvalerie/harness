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
        .split(frame.area());

    let (status_area, chat_area, input_area) = if status_bar_at_bottom {
        (chunks[2], chunks[0], chunks[1])
    } else {
        (chunks[0], chunks[1], chunks[2])
    };

    status::render_status(app, frame, status_area);

    if app.state == crate::app::EngineState::AwaitingHitlApproval && app.pending_tool_call.is_some()
    {
        // Reserve enough space for the approval explanation, exact command,
        // metadata, and footer. A five-line panel clips the command before the
        // user can review it, especially when the model omits optional text.
        let panel_area = chat_area.union(input_area);
        let preview = &app.pending_tool_call.as_ref().unwrap().preview;
        let command_lines = preview
            .command
            .as_deref()
            .map(|command| command.split('\n').count())
            .unwrap_or(0);
        let structured_lines = if preview.command.is_some() {
            // Why + explanation + expected effect + explanation + command
            // heading + command lines + spacing between sections.
            8usize.saturating_add(command_lines)
        } else {
            0
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
        chat::render_chat(app, frame, chat_area);
        input::render_input(app, frame, input_area);
    }

    // Modal Overlays
    modal::render_models_modal(app, frame, frame.area());
    modal::render_thinking_modal(app, frame, frame.area());
    modal::render_plan_modal(app, frame, frame.area());
    modal::render_sessions_modal(app, frame, frame.area());
    modal::render_permissions_modal(app, frame, frame.area());
}

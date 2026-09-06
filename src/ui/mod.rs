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
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // Clean borderless status bar
            Constraint::Min(5),    // Main chat log viewport
            Constraint::Length(3), // Input prompt box
        ])
        .split(frame.area());

    // 1. Render Status Bar
    status::render_status(app, frame, chunks[0]);

    if app.pending_tool_call.is_some() {
        // Keep simple command approvals compact, but make file mutations large
        // enough to inspect their diff without trusting the model blindly.
        let panel_area = chunks[1].union(chunks[2]);
        let diff_lines = app
            .pending_tool_call
            .as_ref()
            .map(|pending| pending.preview.diff_hunks.len())
            .unwrap_or(0);
        let desired_height = if diff_lines == 0 {
            5
        } else {
            (diff_lines.saturating_add(4).min(24).max(10)) as u16
        };
        let hitl_height = desired_height.min(panel_area.height.saturating_sub(5).max(1));

        let lower_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(5),    // Main chat log viewport
                Constraint::Length(hitl_height),
            ])
            .split(panel_area);

        chat::render_chat(app, frame, lower_chunks[0]);
        modal::render_hitl_modal(app, frame, lower_chunks[1]);
    } else {
        // Normal mode
        chat::render_chat(app, frame, chunks[1]);
        input::render_input(app, frame, chunks[2]);
    }

    // Modal Overlays
    modal::render_models_modal(app, frame, frame.area());
    modal::render_sessions_modal(app, frame, frame.area());
}

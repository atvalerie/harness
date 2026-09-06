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
        // Dock HITL security panel at the bottom replacing normal prompt
        let lower_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(5),    // Main chat log viewport
                Constraint::Length(10), // Bottom-docked HITL security panel
            ])
            .split(chunks[1].union(chunks[2]));

        chat::render_chat(app, frame, lower_chunks[0]);
        modal::render_hitl_modal(app, frame, lower_chunks[1]);
    } else {
        // Normal mode
        chat::render_chat(app, frame, chunks[1]);
        input::render_input(app, frame, chunks[2]);
    }

    // Modal Overlays
    modal::render_models_modal(app, frame, frame.area());
}

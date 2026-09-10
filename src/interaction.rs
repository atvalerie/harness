//! Frontend state deliberately separate from model conversation state.
use crate::{
    app::{App, DraftAttachment, EngineState},
    commands::CommandId,
    events::AppEvent,
};
use std::collections::{HashMap, HashSet, VecDeque};
use tokio::sync::mpsc::UnboundedSender;
use unicode_segmentation::UnicodeSegmentation;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Overlay {
    Models,
    Thinking,
    Plan,
    Sessions,
    Permissions,
    Palette,
    Tasks,
    Review,
    History,
}
#[derive(Default)]
pub struct Interaction {
    overlays: Vec<Overlay>,
    pub query: String,
    pub selected: usize,
    pub suggestion_selected: usize,
    pub suggestions_dismissed: bool,
    pub expanded: HashSet<usize>,
    pub transcript_query: String,
    pub focus_message: Option<usize>,
    pub queue: VecDeque<(String, Vec<DraftAttachment>)>,
    pub undo: VecDeque<(String, usize)>,
    pub redo: Vec<(String, usize)>,
    pub drafts: HashMap<String, (String, Vec<DraftAttachment>)>,
    pub dirty: bool,
    pub headless: bool,
    pub actual_model: Option<String>,
    pub actual_protocol: Option<String>,
    pub request_task: Option<String>,
    pub credentials_available: bool,
    pub input_paused: std::sync::Arc<std::sync::atomic::AtomicBool>,
    pub external_editor_requested: bool,
    pub cache: std::cell::RefCell<crate::ui::chat::TranscriptCache>,
}
impl Interaction {
    pub fn reset_transcript(&mut self) {
        self.expanded.clear();
        self.transcript_query.clear();
        self.focus_message = None;
        *self.cache.borrow_mut() = Default::default();
        self.undo.clear();
        self.redo.clear();
        self.dirty = true;
    }
    pub fn is_overlay(&self, overlay: Overlay) -> bool {
        self.overlays.last() == Some(&overlay)
    }
    pub fn top(&self) -> Option<Overlay> {
        self.overlays.last().copied()
    }
    pub fn back(&mut self) {
        self.overlays.pop();
        self.dirty = true;
    }
    pub fn set_overlay(&mut self, overlay: Overlay, show: bool) {
        if show {
            if self.is_overlay(overlay) {
                return;
            }
            if !(overlay == Overlay::Thinking && self.is_overlay(Overlay::Models)) {
                self.overlays.clear();
            }
            self.overlays.push(overlay);
        } else {
            self.overlays.retain(|entry| *entry != overlay);
        }
        self.dirty = true;
    }
    pub fn remember_edit(&mut self, text: &str, cursor: usize) {
        if self
            .undo
            .back()
            .is_some_and(|(old, pos)| old == text && *pos == cursor)
        {
            return;
        }
        self.undo.push_back((text.to_string(), cursor));
        while self.undo.len() > 100 {
            self.undo.pop_front();
        }
        self.redo.clear();
    }
}
pub fn previous_grapheme(text: &str, cursor: usize) -> usize {
    let byte = text
        .char_indices()
        .nth(cursor)
        .map(|(i, _)| i)
        .unwrap_or(text.len());
    text.grapheme_indices(true)
        .take_while(|(i, _)| *i < byte)
        .last()
        .map(|(i, _)| text[..i].chars().count())
        .unwrap_or(0)
}
pub fn next_grapheme(text: &str, cursor: usize) -> usize {
    let byte = text
        .char_indices()
        .nth(cursor)
        .map(|(i, _)| i)
        .unwrap_or(text.len());
    text.grapheme_indices(true)
        .find(|(i, _)| *i > byte)
        .map(|(i, _)| text[..i].chars().count())
        .unwrap_or(text.chars().count())
}
impl App {
    pub fn open_palette(&mut self) {
        self.interaction.query = self.input_buffer.trim_start_matches('/').to_string();
        self.interaction.selected = 0;
        self.interaction.set_overlay(Overlay::Palette, true);
    }
    pub fn undo_draft(&mut self, redo: bool) {
        if redo {
            if let Some((text, cursor)) = self.interaction.redo.pop() {
                self.interaction
                    .undo
                    .push_back((self.input_buffer.clone(), self.input_cursor));
                self.input_buffer = text;
                self.input_cursor = cursor;
            }
        } else if let Some((text, cursor)) = self.interaction.undo.pop_back() {
            self.interaction
                .redo
                .push((self.input_buffer.clone(), self.input_cursor));
            self.input_buffer = text;
            self.input_cursor = cursor;
        }
    }
    pub fn move_word(&mut self, forward: bool) {
        let chars = self.input_buffer.chars().collect::<Vec<_>>();
        let mut i = self.input_cursor.min(chars.len());
        if forward {
            while i < chars.len() && !chars[i].is_whitespace() {
                i += 1;
            }
            while i < chars.len() && chars[i].is_whitespace() {
                i += 1;
            }
        } else {
            while i > 0 && chars[i - 1].is_whitespace() {
                i -= 1;
            }
            while i > 0 && !chars[i - 1].is_whitespace() {
                i -= 1;
            }
        }
        self.input_cursor = i;
    }
    pub fn send_queued_prompt(&mut self, tx: UnboundedSender<AppEvent>) {
        if self.state != EngineState::Idle || self.interaction.queue.is_empty() {
            return;
        }
        let draft = std::mem::take(&mut self.input_buffer);
        let cursor = self.input_cursor;
        let attachments = std::mem::take(&mut self.draft_attachments);
        if let Some((text, blocks)) = self.interaction.queue.pop_front() {
            self.input_buffer = text;
            self.draft_attachments = blocks;
            self.handle_enter(tx);
        }
        self.input_buffer = draft;
        self.input_cursor = cursor;
        self.draft_attachments = attachments;
    }
    pub fn handle_workspace_command(
        &mut self,
        id: CommandId,
        arg: &str,
        tx: UnboundedSender<AppEvent>,
    ) {
        match id {
            CommandId::Palette => {
                if self.interaction.headless {
                    self.add_message("system", crate::commands::help());
                } else {
                    self.open_palette();
                }
            }
            CommandId::Tasks => {
                if let Some(id) = arg.strip_prefix("cancel ") {
                    self.set_status(
                        self.tool_registry
                            .tasks
                            .cancel(id.trim())
                            .unwrap_or_else(|e| e),
                    );
                } else if !arg.is_empty() {
                    self.add_message("system", self.tool_registry.tasks.inspect(arg));
                } else {
                    if self.interaction.headless {
                        self.add_message("system", self.tool_registry.tasks.summary());
                    } else {
                        self.interaction.set_overlay(Overlay::Tasks, true);
                    }
                }
            }
            CommandId::Review => {
                if self.interaction.headless {
                    self.add_message(
                        "system",
                        if let Ok(id) = arg.parse() {
                            self.review.diff(id)
                        } else {
                            self.review.summary()
                        },
                    );
                } else {
                    self.interaction.set_overlay(Overlay::Review, true);
                    self.interaction.query = arg.into();
                }
            }
            CommandId::Rollback => {
                let result = arg
                    .parse::<usize>()
                    .map_err(|_| "Usage: /rollback <checkpoint-id>".to_string())
                    .and_then(|id| self.review.rollback(&self.tool_registry.working_dir(), id));
                self.add_message("system", result.unwrap_or_else(|e| e));
            }
            CommandId::Search => {
                self.interaction.focus_message = None;
                self.interaction.transcript_query = arg.into();
                self.chat_scroll = 0;
            }
            CommandId::Jump => {
                let current = self
                    .interaction
                    .focus_message
                    .unwrap_or(self.messages.len());
                let target = match arg {
                    "prev" => self
                        .messages
                        .iter()
                        .enumerate()
                        .rev()
                        .find(|(i, m)| *i < current && m.role == "user")
                        .map(|(i, _)| i),
                    "next" => self
                        .messages
                        .iter()
                        .enumerate()
                        .find(|(i, m)| *i > current && m.role == "user")
                        .map(|(i, _)| i),
                    "error" => self
                        .messages
                        .iter()
                        .enumerate()
                        .rev()
                        .find(|(_, m)| m.content.starts_with("Error:"))
                        .map(|(i, _)| i),
                    _ => arg
                        .parse::<usize>()
                        .ok()
                        .and_then(|i| i.checked_sub(1))
                        .filter(|i| *i < self.messages.len()),
                };
                if let Some(index) = target {
                    self.interaction.transcript_query.clear();
                    self.interaction.focus_message = Some(index);
                    self.chat_scroll = usize::MAX;
                    self.set_status("Focused transcript; /search clears focus");
                } else {
                    self.set_status("Usage: /jump <message|prev|next|error>");
                }
            }
            CommandId::Inspect => {
                let index = arg
                    .trim_start_matches('#')
                    .parse::<usize>()
                    .ok()
                    .and_then(|n| n.checked_sub(1))
                    .filter(|i| *i < self.messages.len());
                if let Some(index) = index {
                    if !self.interaction.expanded.remove(&index) {
                        self.interaction.expanded.insert(index);
                    }
                    self.interaction.focus_message = Some(index);
                    self.chat_scroll = usize::MAX;
                    self.interaction.transcript_query.clear();
                    *self.interaction.cache.borrow_mut() = Default::default();
                    self.set_status(
                        "Expanded/collapsed item. Ctrl+O toggles; /search clears focused view.",
                    );
                } else {
                    self.set_status(
                        "Usage: /inspect <existing message number> (toggles expansion)",
                    );
                }
            }
            CommandId::Queue => {
                if !arg.is_empty() {
                    self.interaction.queue.push_back((arg.into(), Vec::new()));
                    self.set_status("Prompt queued; /interrupt sends immediately.");
                }
            }
            CommandId::Interrupt => {
                if !arg.is_empty() {
                    self.cancel_generation();
                    self.input_buffer = arg.into();
                    self.handle_enter(tx);
                }
            }
            CommandId::History => {
                if self.interaction.headless {
                    self.add_message(
                        "system",
                        self.input_history
                            .iter()
                            .rev()
                            .filter(|s| s.contains(arg))
                            .cloned()
                            .collect::<Vec<_>>()
                            .join("\n"),
                    );
                    return;
                }
                self.interaction.query = arg.into();
                self.interaction.selected = 0;
                self.interaction.set_overlay(Overlay::History, true);
            }
            CommandId::Editor => {
                if self.interaction.headless {
                    self.add_message(
                        "system",
                        "External editor requires the interactive terminal",
                    );
                } else {
                    self.interaction.external_editor_requested = true;
                }
            }
            _ => self.set_status("Command unavailable in this frontend"),
        }
    }
    pub fn slash_suggestions(&self) -> Vec<&'static crate::commands::CommandSpec> {
        if self.interaction.top().is_some()
            || self.interaction.suggestions_dismissed
            || !self.input_buffer.starts_with('/')
            || self.input_buffer.contains(char::is_whitespace)
        {
            return Vec::new();
        }
        crate::commands::matching(&self.input_buffer)
    }
    pub fn completion_candidates(&self) -> Vec<String> {
        let Some((command, prefix)) = self.input_buffer.split_once(' ') else {
            return crate::commands::matching(&self.input_buffer)
                .iter()
                .map(|s| format!("{} ", s.name))
                .collect();
        };
        let values: Vec<String> = match command {
            "/model" => self.available_models.iter().map(|m| m.id.clone()).collect(),
            "/provider" => self.config.providers.keys().cloned().collect(),
            "/attach" | "/resume" => {
                let prefix = prefix.trim_matches('"');
                let path = std::path::Path::new(prefix);
                let (parent, stem) = if prefix.ends_with('/') || prefix.ends_with('\\') {
                    (path, "")
                } else {
                    (
                        path.parent().unwrap_or(std::path::Path::new("")),
                        path.file_name().and_then(|s| s.to_str()).unwrap_or(""),
                    )
                };
                let root = self.tool_registry.working_dir().join(parent);
                std::fs::read_dir(root)
                    .into_iter()
                    .flatten()
                    .flatten()
                    .filter_map(|entry| {
                        let name = entry.file_name().to_string_lossy().to_string();
                        if !name.starts_with(stem) {
                            return None;
                        }
                        let mut value = parent.join(name).to_string_lossy().to_string();
                        if entry.path().is_dir() {
                            value.push(std::path::MAIN_SEPARATOR);
                        }
                        Some(value)
                    })
                    .take(100)
                    .collect()
            }
            _ => Vec::new(),
        };
        values
            .into_iter()
            .filter(|v| v.starts_with(prefix.trim_matches('"')))
            .map(|v| {
                format!(
                    "{command} {}",
                    if v.contains(' ') {
                        format!("\"{v}\"")
                    } else {
                        v
                    }
                )
            })
            .collect()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn undo_and_redo_preserve_draft() {
        let mut app = App::new(crate::config::AppConfig::default(), "test".into());
        app.insert_input_text("hello");
        app.insert_input_text(" world");
        app.undo_draft(false);
        assert_eq!(app.input_buffer, "hello");
        app.undo_draft(true);
        assert_eq!(app.input_buffer, "hello world");
    }
    #[test]
    fn busy_prompt_is_queued_without_changing_conversation() {
        let mut app = App::new(crate::config::AppConfig::default(), "test".into());
        app.state = EngineState::Streaming;
        app.input_buffer = "next prompt".into();
        let (tx, _) = tokio::sync::mpsc::unbounded_channel();
        app.handle_enter(tx);
        assert_eq!(app.interaction.queue.len(), 1);
        assert!(app.input_buffer.is_empty());
        assert!(app.messages.is_empty());
    }
    #[test]
    fn idle_only_commands_do_not_switch_busy_provider() {
        let mut app = App::new(crate::config::AppConfig::default(), "test".into());
        let old = app.config.provider.clone();
        app.state = EngineState::Streaming;
        let (tx, _) = tokio::sync::mpsc::unbounded_channel();
        app.handle_slash_command("/provider codex", tx);
        assert_eq!(app.config.provider, old);
    }
    #[test]
    fn model_argument_completion_is_provider_scoped() {
        let mut app = App::new(crate::config::AppConfig::default(), "test".into());
        app.input_buffer = "/provider co".into();
        assert_eq!(app.completion_candidates(), vec!["/provider codex"]);
    }

    #[test]
    fn overlay_navigation() {
        let mut ui = Interaction::default();
        ui.set_overlay(Overlay::Models, true);
        ui.set_overlay(Overlay::Thinking, true);
        ui.back();
        assert!(ui.is_overlay(Overlay::Models));
        ui.set_overlay(Overlay::Tasks, true);
        assert!(!ui.is_overlay(Overlay::Models));
    }
    #[test]
    fn graphemes_are_not_split() {
        let s = "a\u{1f469}\u{200d}\u{1f4bb}e\u{301}";
        assert_eq!(next_grapheme(s, 1), 4);
        assert_eq!(previous_grapheme(s, 6), 4);
    }
    #[test]
    fn inspect_focuses_and_toggles_without_losing_history() {
        let mut app = App::new(crate::config::AppConfig::default(), "".into());
        app.add_message("user", "hello");
        app.add_message("tool", "result");
        let (tx, _) = tokio::sync::mpsc::unbounded_channel();
        app.handle_workspace_command(CommandId::Inspect, "#2", tx.clone());
        assert_eq!(app.interaction.focus_message, Some(1));
        assert!(app.interaction.expanded.contains(&1));
        assert_eq!(app.chat_scroll, usize::MAX);
        app.handle_workspace_command(CommandId::Inspect, "2", tx.clone());
        assert!(!app.interaction.expanded.contains(&1));
        app.handle_workspace_command(CommandId::Inspect, "999", tx);
        assert_eq!(app.interaction.focus_message, Some(1));
        assert_eq!(app.messages.len(), 2);
    }
}

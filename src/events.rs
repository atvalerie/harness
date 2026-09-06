#[derive(Debug, Clone)]
pub enum StreamSignal {
    ThoughtDelta(String),
    TextDelta(String),
    ToolCall {
        id: Option<String>,
        name: String,
        args: serde_json::Value,
        thought_signature: Option<String>,
    },
    Usage {
        prompt_tokens: u32,
        candidates_tokens: u32,
        total_tokens: u32,
    },
    Finished {
        finish_reason: Option<String>,
    },
    Notice(String),
    Error(String),
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum AppEvent {
    Key(crossterm::event::KeyEvent),
    Mouse(crossterm::event::MouseEvent),
    Resize(u16, u16),
    Stream {
        epoch: u64,
        signal: StreamSignal,
    },
    ToolExecutionResult {
        tool_name: String,
        call_id: Option<String>,
        result: Result<String, String>,
    },
    SystemNotification(String),
    ModelsFetched(Result<Vec<crate::client::types::ModelInfo>, String>),
    CompactionFinished(Result<String, String>),
}

#[derive(Debug, Clone)]
pub enum StreamSignal {
    ToolReviewed {
        pending: crate::app::PendingToolCall,
        result: Result<crate::auto_review::Decision, String>,
    },
    ModelSelected {
        model: String,
        protocol: String,
    },
    ThoughtDelta(String),
    TextDelta(String),
    ToolCall {
        id: Option<String>,
        name: String,
        args: serde_json::Value,
        thought_signature: Option<String>,
    },
    Usage {
        prompt_tokens: u64,
        candidates_tokens: u64,
        total_tokens: u64,
        details: crate::usage::TokenUsage,
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
    Paste(String),
    Mouse(crossterm::event::MouseEvent),
    Resize(u16, u16),
    Stream {
        epoch: u64,
        signal: StreamSignal,
    },
    ToolExecutionResult {
        epoch: u64,
        tool_name: String,
        call_id: Option<String>,
        result: Result<String, String>,
    },
    CapabilityRequest {
        epoch: u64,
        capability_name: String,
        call_id: Option<String>,
        args: serde_json::Value,
    },
    SystemNotification(String),
    ModelsFetched {
        epoch: u64,
        result: Result<Vec<crate::client::types::ModelInfo>, String>,
        interactive: bool,
    },
    CompactionFinished(u64, Result<String, String>),
    ProviderUsageFetched(Result<String, String>),
}

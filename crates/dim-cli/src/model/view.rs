//! The view model: everything the terminal UI knows about a session.
//!
//! These types are owned by the UI. The renderer, the prompts and the panels
//! read them and nothing from the backend crates (`dim-client`,
//! `dim-agent-types`, `dim-app-protocol`); `view_adapter` is the one place a
//! backend projection is turned into them, so a change below the wire lands
//! there and not in a widget. `xtask check` enforces the boundary.

use std::collections::BTreeMap;

/// The kind of one transcript row, as far as drawing is concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowKind {
    UserMessage,
    /// A shell-mode run the user sent to the model (`!command`).
    Shell,
    /// Assistant text that is still arriving.
    StreamingAssistant,
    AssistantMessage,
    Thinking,
    ToolCall,
    Error,
    Status,
    /// A row kind the UI does not draw specially; it falls back to a status
    /// line.
    Other,
}

impl RowKind {
    /// Rows counted as messages by the history window.
    pub fn is_message(self) -> bool {
        matches!(
            self,
            Self::UserMessage | Self::Shell | Self::AssistantMessage | Self::ToolCall
        )
    }
}

/// How a status row is coloured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowTone {
    Danger,
    Warning,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ViewRow {
    pub row_id: String,
    pub kind: RowKind,
    pub text: String,
    pub streaming: bool,
    /// A settled thought: how long it ran, in milliseconds.
    pub elapsed_ms: Option<u64>,
    pub tone: Option<RowTone>,
    pub tool_name: Option<String>,
    pub tool_input: Option<serde_json::Value>,
    /// The model request a tool call came from: the calls of one request are
    /// drawn as one group.
    pub batch: Option<String>,
    /// A tool call that has not produced its result yet.
    pub pending: bool,
    /// A call that waits on the user (an approval names it, or it is the
    /// question tool itself): it is part of the transcript at once, and the
    /// prompt owns the waiting.
    pub parked: bool,
    pub result_text: Option<String>,
    pub result_is_error: bool,
    pub diff: Option<String>,
}

impl ViewRow {
    /// A running tool the live area should animate.
    pub fn is_live_tool(&self) -> bool {
        self.pending && !self.parked
    }
}

/// A tool call that asks the user for permission.
#[derive(Debug, Clone, PartialEq)]
pub struct ApprovalView {
    pub request_id: String,
    pub branch_id: String,
    pub reason: String,
    pub tool_name: String,
    pub tool_input: serde_json::Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuestionKind {
    SingleSelect,
    MultiSelect,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuestionOption {
    pub label: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Question {
    pub id: String,
    pub header: String,
    pub question: String,
    pub kind: QuestionKind,
    pub options: Vec<QuestionOption>,
}

/// An open request for the user to answer questions.
#[derive(Debug, Clone, PartialEq)]
pub struct QuestionRequestView {
    pub request_id: String,
    pub branch_id: String,
    pub questions: Vec<Question>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    Single(String),
    Multiple(Vec<String>),
}

/// Answers keyed by question id.
pub type Answers = BTreeMap<String, Answer>;

/// A message submitted while a turn runs, not yet consumed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueuedInput {
    pub id: String,
    pub text: String,
}

/// The session as the screen draws it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ViewSnapshot {
    /// Shared with the worker's projection: publishing copies pointers.
    pub rows: Vec<std::sync::Arc<ViewRow>>,
    pub running: bool,
    pub approvals: Vec<ApprovalView>,
    pub questions: Vec<QuestionRequestView>,
    /// Oldest first.
    pub queued: Vec<QueuedInput>,
}

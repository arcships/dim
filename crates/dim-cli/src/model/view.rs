//! The view model: everything the terminal UI knows about a session.
//!
//! These types are owned by the UI. The renderer, the prompts and the panels
//! read them without backend handles. Questions reuse the shared client
//! contract in `question`; the session worker and wire decoder own backend I/O.

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
    #[allow(dead_code)]
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
    #[allow(dead_code)]
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
    /// A finished command's exit code (the exec tool reports it).
    pub exit_code: Option<i64>,
    /// An `agent` request call: the delegation it started, drawn as a card.
    pub member: Option<Box<MemberCard>>,
}

/// A delegation in its leader's transcript, with what this client observed
/// of its member. The outcome comes only from the leader's facts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberCard {
    pub task: TaskView,
    /// The temporary profile's name, or a native Agent's name once
    /// `agent.list` named it; its ID until then.
    pub agent: String,
    /// The native Agent `agent.list` can name; none for a temporary profile
    /// or a provider member.
    pub agent_id: Option<String>,
    pub prompt: String,
    /// Member activity, not a claim about the model that ran this task: an
    /// ended card keeps the last observation made while it ran.
    pub model: Option<String>,
    pub activity: Option<String>,
    /// None until this client observes the member.
    pub observation: Option<MemberConversationState>,
}

/// A tool call that asks the user for permission.
#[derive(Debug, Clone, PartialEq)]
pub struct ApprovalView {
    pub request_id: String,
    pub branch_id: String,
    pub reason: String,
    pub tool_name: String,
    pub tool_input: serde_json::Value,
    /// The simple command an "always" answer would save an allow rule for,
    /// when the daemon keeps command rules.
    pub always: Option<String>,
}

/// A message submitted while a turn runs, not yet consumed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueuedInput {
    pub id: String,
    pub text: String,
}

/// Work of the session that runs apart from the turn: a command the `exec`
/// tool moved to the background, a member agent on a delegated task, or a
/// Workflow run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskView {
    /// The Timeline event that started the command or delegation, or the
    /// Workflow run ID.
    pub id: String,
    pub branch_id: String,
    pub kind: TaskKind,
    pub label: String,
    pub started_at: u64,
    pub finished: Option<TaskFinish>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskFinish {
    pub at: u64,
    pub status: &'static str,
    pub failed: bool,
    pub text: String,
    pub exit_code: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TaskTarget {
    pub branch_id: String,
    pub task_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskOutputState {
    Loading,
    Live,
    Unavailable,
    Unsupported,
    ReadError(String),
    Finished { status: &'static str, failed: bool },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemberConversationState {
    Loading,
    Live,
    ReadError(String),
    Closed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskKind {
    /// Read and stopped on the originating branch.
    Command(TaskTarget),
    Member(MemberTarget),
    /// A run of the optional Workflow package, by its run ID.
    Workflow(Box<WorkflowView>),
}

/// Who carries out a delegation, as its delivery fact says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemberTarget {
    /// Requested; the actual member is known only after successful delivery.
    Starting,
    /// A native member Session: what attach, history and stop address.
    Session(String),
    /// A provider member (its Agent ID holds `:`): its member ID is opaque,
    /// never a Session to observe or stop.
    Provider,
}

/// The last stored run the owner returned for this Session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowView {
    pub run: crate::wire::WorkflowRun,
    /// The owner no longer returns the run; the record is the last observed.
    pub unavailable: bool,
    /// A stop request awaits its reply.
    pub stop_pending: bool,
}

/// How far the Session's Workflow runs are known.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum WorkflowInventory {
    /// The connection offers no `workflow.status`.
    #[default]
    Absent,
    /// An explicit discovery has not replied yet.
    Loading,
    Ready,
    /// Reads stopped after an error; `observed` when some read succeeded.
    Failed {
        error: String,
        observed: bool,
    },
}

/// How a turn ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnEnd {
    Completed,
    Failed,
    Cancelled,
}

/// The session as the screen draws it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ViewSnapshot {
    pub session_id: String,
    /// Private attachment identity; reopening a panel keeps it.
    pub projection_id: u64,
    pub main_history_complete: bool,
    /// Shared with the worker's projection: publishing copies pointers.
    pub rows: Vec<std::sync::Arc<ViewRow>>,
    pub running: bool,
    pub approvals: Vec<ApprovalView>,
    pub questions: Vec<crate::question::Call>,
    /// Oldest first.
    pub queued: Vec<QueuedInput>,
    /// Unfinished tasks followed by the latest 50 terminal facts.
    pub tasks: Vec<TaskView>,
    pub task_history_loading: usize,
    pub task_history_errors: Vec<String>,
    pub workflow: WorkflowInventory,
    /// The connection offers `workflow.stop`.
    pub workflow_stop: bool,
    /// The newest turn that ended on main: its sequence and how it ended.
    pub ended: Option<(u64, TurnEnd)>,
}

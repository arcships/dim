//! The view model: everything the terminal UI knows about a session.
//!
//! These types are owned by the UI. The renderer, the prompts and the panels
//! read them without backend handles. Questions reuse the shared client
//! contract in `question`; the session worker and wire decoder own backend I/O.

/// The cancellation reason used when the terminal user interrupts a turn.
pub const INTERRUPT_REASON: &str = "User interrupted the run.";

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
    /// A `context/compaction-start` and the `context/compaction` that ends it.
    Compaction,
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
    Warning,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ViewRow {
    pub row_id: String,
    pub kind: RowKind,
    /// Presentation of an image message; `text` remains editable text only.
    pub display: Option<String>,
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
    /// Work this row stands for that has not settled yet: a tool call
    /// without its result, or a user message the model has not consumed.
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
    /// A user message: the line's sequence just before it, where a line or
    /// a fork opened from this message cuts (timeline UI §4.1).
    pub cut: Option<u64>,
    /// A line-opened marker: the line it opened, which a pick enters
    /// (timeline UI §4.4).
    pub line: Option<String>,
    /// A compaction row's state and detail.
    pub compaction: Option<CompactionView>,
}

/// A context compaction as its transcript row shows it (compaction
/// requirements §4).
#[derive(Debug, Clone, PartialEq)]
pub struct CompactionView {
    pub state: CompactionState,
    /// A completed compaction's summary text — the strategy's part of what
    /// replaced the cut history — or the reason or error of one that did not
    /// complete.
    pub text: String,
    /// The parts that keep a completed compaction above its bound, as the
    /// record names them; empty when the record carries no such mark.
    pub insufficient: Vec<CompactionPartSize>,
}

/// One part the record names in a compaction's insufficient-space mark.
#[derive(Debug, Clone, PartialEq)]
pub struct CompactionPartSize {
    pub part: CompactionPart,
    pub tokens: f64,
}

/// A part of the context a compaction could not shrink, as the record names
/// it (`insufficientSpace.parts[].part`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactionPart {
    SystemPrompt,
    Tools,
    AppendedMessages,
    KeyedBaseline,
    PinnedInputs,
    Digest,
    Summary,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactionState {
    /// The strategy was called and its result has not arrived.
    Running,
    Completed,
    Skipped,
    Failed,
    Cancelled,
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

/// One line of the Session, as the lines panel lists it (design §14).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineView {
    pub id: String,
    /// The line it was opened from; `None` for `main`.
    pub parent: Option<String>,
    /// The first line of the purpose it was opened with.
    pub purpose: String,
    pub state: LineState,
    /// Where it was cut from its parent; `None` for `main`.
    pub cut: Option<LineCut>,
}

/// Where a line was cut from its parent (timeline UI §6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LineCut {
    /// The parent's sequence the line inherits through.
    pub sequence: u64,
    /// User messages in the parent's history by then, its inherited ones
    /// included: the cut reads "after message N".
    pub messages: usize,
}

/// How a line stands (design §14).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LineState {
    /// `main`: the interaction surface, never ended.
    Main,
    /// A turn runs on the line.
    Working,
    /// Open, with no turn running.
    Waiting,
    /// It ended itself through the branch tool.
    EndedItself,
    /// Another issuer ended it: a host operation or the user.
    EndedByOperation,
    /// An ancestor's end ended it; the ancestor's id.
    EndedWith(String),
    /// The tree could not build it.
    CreationFailed,
}

/// A tool call that asks the user for permission.
#[derive(Debug, Clone, PartialEq)]
pub struct ApprovalView {
    pub member: Option<
        Result<dim_agent_types::MemberApprovalDescriptor, crate::member_approval::DetailError>,
    >,
    pub request_id: String,
    pub branch_id: String,
    pub reason: String,
    pub tool_name: String,
    pub tool_input: serde_json::Value,
    /// The simple command an "always" answer would save an allow rule for,
    /// when the daemon keeps command rules.
    pub always: Option<String>,
}

/// A message not yet consumed by the model, held or already applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueuedInput {
    pub id: String,
    pub text: String,
    pub held: bool,
    pub recallable: bool,
    /// Presentation of an image message; never restored into a draft.
    pub display: Option<String>,
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
    /// The shown line's history has loaded: its transcript can be laid out.
    pub history_complete: bool,
    /// Shared with the worker's projection: publishing copies pointers.
    pub rows: Vec<std::sync::Arc<ViewRow>>,
    pub running: bool,
    pub main_running: bool,
    /// The first message the user sent on `main`: the window title's topic
    /// until the Session has a stored title (`ui::title`), whichever line is
    /// shown.
    pub main_topic: Option<String>,
    /// The Session's stored title, when it has a real one: the window
    /// title's topic in place of `main_topic` (`ui::title`).
    pub session_title: Option<String>,
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
    /// `main` followed by this Session's lines, as the lines panel lists them.
    pub lines: Vec<LineView>,
    pub focus: String,
    /// The line opened beside the shown one, while its panel is open.
    pub side: Option<SideLine>,
    /// The goal in force on the shown line, folded from its events (Goal
    /// §A.3); none when the line has no goal.
    pub goal: Option<crate::goal::Goal>,
}

/// A line the side panel follows (timeline UI §4.7).
#[derive(Debug, Clone, PartialEq)]
pub struct SideLine {
    pub id: String,
    pub purpose: String,
    pub state: LineState,
    /// Its transcript, shared with the worker's projection.
    pub rows: Vec<std::sync::Arc<ViewRow>>,
}

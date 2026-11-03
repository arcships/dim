//! The contract between the terminal UI and the session worker.
//!
//! The UI sends [`Command`]s and drains [`Update`]s through [`SessionPort`];
//! the App Server behind it (`session_host`) is the only implementation.
//!
//! Commands carry user intent; the worker owns backend calls and publishes
//! read-only views. Optional methods missing from a daemon produce an
//! unavailable diagnostic.

use crate::model::shell::ShellResult;
use crate::model::view::{MemberConversationState, TaskOutputState, TaskTarget, ViewSnapshot};
use crate::wire::Configuration;

/// The Dim account provider, signed in with `/login`.
pub const DIM_OAUTH_ID: &str = "dimcode-api-oauth";

/// The namespace of the Dim account's calls: they target a provider id and
/// name the account directory. A native OAuth namespace has one account and
/// takes no target.
pub const ACCOUNT_NAMESPACE: &str = "account";

/// One piece of a draft sent with [`Command::SubmitDraft`], in order.
#[derive(Debug, Clone, PartialEq)]
pub enum DraftPart {
    Text(String),
    /// Pasted text; a long one is stored as a session attachment.
    Paste(String),
    /// An image or other content block inserted from a file or the
    /// clipboard.
    Block(dim_agent_types::ContentBlock),
}

#[allow(dead_code)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GrantRequest {
    pub path: String,
    pub kind: String,
    pub mode: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecutionSnapshot {
    pub revision: String,
    pub sandbox: bool,
    pub network: bool,
}

/// The `prompt-suggestion` source as `settings.read` and `settings.update`
/// answer it: the revision the next write expects, and the capability's own
/// document, so the client keeps no second copy of the members.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SuggestionSetting {
    pub revision: String,
    pub document: dim_agent_capabilities::prompt_suggestion::PromptSuggestionSettings,
}

/// The host's answer to `host.execution.backend`; the Execution panel shows
/// the reason when the platform has no sandbox backend.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SandboxBackend {
    Available,
    Unavailable { reason: String },
}

impl<'de> serde::Deserialize<'de> for SandboxBackend {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(serde::Deserialize)]
        struct Reply {
            available: bool,
            #[serde(default)]
            reason: Option<String>,
        }
        let reply = Reply::deserialize(deserializer)?;
        if reply.available {
            Ok(Self::Available)
        } else {
            Ok(Self::Unavailable {
                reason: reply
                    .reason
                    .ok_or_else(|| serde::de::Error::missing_field("reason"))?,
            })
        }
    }
}

/// Whether a submitted message starts after the current run (`Queue`) or
/// joins the run at its next turn boundary (`Steer`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubmitMode {
    Queue,
    Steer,
}

impl SubmitMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queue => "queue",
            Self::Steer => "steer",
        }
    }
}

/// What `/resume` lists: the launch workspace, the same directory in its
/// repository's worktrees, or every workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionsScope {
    Workspace,
    Repository(Vec<String>),
    All,
}

pub enum Command {
    Submit(String, SubmitMode),
    /// A draft with pastes to store or content blocks to send.
    SubmitDraft(Vec<DraftPart>, SubmitMode),
    Stop,
    /// Answer an approval request.
    Approval {
        request_id: String,
        branch_id: String,
        allow: bool,
        remember: bool,
        grant: Option<GrantRequest>,
    },
    RetryApproval {
        request_id: String,
        branch_id: String,
    },
    AskAnswers {
        session_id: String,
        call: crate::question::Call,
        answers: Vec<crate::question::Answer>,
    },
    AskCancel {
        session_id: String,
        call: crate::question::Call,
    },
    /// Interrupt the branch whose question is open and the current turn.
    StopQuestion {
        session_id: String,
        branch_id: String,
        stop_turn: bool,
    },
    /// Steer or withdraw a queued input.
    InputControl {
        input_id: String,
        action: &'static str,
    },
    /// Stop a background command on its originating branch (`/tasks`).
    StopTask {
        session_id: String,
        projection_id: u64,
        target: TaskTarget,
    },
    /// Select a live output detail, or stop refreshing when it closes.
    WatchTaskOutput {
        projection_id: u64,
        target: Option<TaskTarget>,
    },
    FocusLine(String),
    /// Stop following the side panel's line; the line keeps running.
    CloseSide,
    /// End a line of the displayed attachment, as the user asked.
    EndLine {
        session_id: String,
        projection_id: u64,
        branch_id: String,
    },
    /// Open a line of the displayed attachment at `at` of `parent`, with
    /// its opening input, and follow it in the side panel (timeline UI §4.7).
    OpenLine {
        session_id: String,
        projection_id: u64,
        parent: String,
        at: u64,
        prompt: String,
    },
    /// Fork the displayed Session's `main` through `at` and attach to the
    /// fork (timeline UI §4.3).
    Fork {
        session_id: String,
        projection_id: u64,
        at: u64,
    },
    /// Observe one delivered member without selecting its Session for editing.
    WatchMember {
        projection_id: u64,
        member_id: Option<String>,
    },
    /// Reopen the same inventory and retry only known failed history pages.
    TaskHistory {
        projection_id: u64,
    },
    /// Capture addressed stops from the displayed attachment.
    StopTasks {
        session_id: String,
        projection_id: u64,
    },
    /// Interrupt the delivered member Session's current turn.
    StopMember {
        session_id: String,
        projection_id: u64,
        member_id: String,
    },
    /// Ask Workflow to stop one run of the displayed Session.
    StopWorkflow {
        session_id: String,
        projection_id: u64,
        run_id: String,
    },
    /// Start a fresh session in the same directory.
    NewSession,
    /// Switch to a stored session.
    Resume(String),
    /// Find the launch workspace's repository worktrees, once per picker open.
    FindRepository,
    /// List stored sessions (newest first), in `scope`.
    ListSessions {
        id: String,
        query: String,
        cursor: Option<String>,
        /// The "Show ACP sessions" preference the picker was opened with.
        show_acp: bool,
        scope: SessionsScope,
    },
    CloseSessions,
    /// List the models that can be chosen.
    ListModels,
    ReadIsolation,
    SaveIsolation {
        expected_revision: String,
        sandbox: bool,
        network: bool,
    },
    ResetIsolation(String),
    /// Ask the Session for the message the user would send next, after the
    /// turn completed in this interface.
    Suggest {
        turn_id: String,
    },
    /// Report what became of the offer for `turn_id`; accepting inserts the
    /// text locally and sends nothing.
    SuggestionRespond {
        turn_id: String,
        accept: bool,
    },
    /// Read the `prompt-suggestion` switch.
    ReadSuggestionSetting,
    /// Write the `prompt-suggestion` switch.
    SaveSuggestionSetting {
        expected_revision: String,
        enabled: bool,
        max_chars: u32,
    },
    ClearModelPreferences {
        provider_id: String,
        model_id: String,
        expected_version: u32,
    },
    /// Produce the `/usage` report.
    Usage,
    /// `/compact`: ask the shown line to compact its context.
    Compact,
    /// `/rename`: set the shown Session's manual title, or clear it when
    /// `title` is `None`; an empty text is refused, and only `--clear`
    /// clears. Nothing here reaches the model.
    Rename(Option<String>),
    /// `/goal`: show, set, edit, pause, resume or clear the shown line's goal.
    Goal(crate::goal::Request),
    SetPermission(dim_agent_types::PermissionPreset),
    /// List the providers of the `/connect` panel.
    ListProviders,
    /// Change one provider.
    Provider(ProviderAction),
    /// Begin a browser or device sign-in; `attempt` tags its updates.
    OauthStart(OauthStart),
    /// Abandon the sign-in in progress.
    OauthCancel,
    /// A callback URL the user pasted.
    OauthPaste(String),
    /// The `/login` panel opened: report the provider's account.
    LoginStatus(String),
    /// The `/login` panel shows a signed-in account: report its usage.
    LoginUsage(String),
    /// `/web`: flip the daemon-owned Web entry switch.
    ToggleWeb,
    /// Sign the described account out.
    Logout(AccountTarget),
    /// Run a shell-mode command through the App Server; `private` (`!!`)
    /// keeps its result out of the session.
    ShellRun {
        run_id: String,
        command: String,
        private: bool,
    },
    /// Ask the run `run_id` to end.
    ShellStop {
        run_id: String,
    },
    /// `/mcp ...`: the words after the command.
    Mcp(String),
    /// `/settings recording ...`: the words after the command.
    Recording(String),
    /// List the plugins of the `/plugins` panel (`plugin.list`).
    ListPlugins,
    /// One request to a management area's owner, through its adapter.
    Management(ManagementRequest),
    /// The interface language's code (`en`, `zh-CN`, ...), which the worker
    /// composes management text in from the next command on.
    SetLocale(&'static str),
    /// Change one plugin; the worker lists the plugins again after it.
    Plugin(PluginAction),
    /// List the runtimes of the `/runtime` panel (`runtime.list`) and the
    /// plugins that wait on them (`plugin.list`).
    ListRuntimes,
    /// Change one runtime; the worker lists the runtimes and the plugins
    /// again after it.
    Runtime(RuntimeAction),
    /// A configuration command line (`/effort high`, `/agent`, ...).
    Configure(String),
    /// Make `(provider, model)` the default and the active session's model.
    /// `reasoning_effort` is set when the picker cycled that model's effort.
    SelectModel {
        provider_id: String,
        model_id: String,
        reasoning_effort: Option<String>,
    },
    /// Read the subagent model policy and its revision.
    ReadModelPolicy,
    /// Replace the subagent model policy if it is still at `expected_revision`.
    SetModelPolicy {
        expected_revision: String,
        policy: SubagentModelPolicy,
    },
    /// Install the offered version (the update dialog's "Update now").
    UpgradeInstall(crate::update::Install),
}

/// One management area: an owner's management methods, drawn by the native
/// client through the area's adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Area {
    Skills,
    Hooks,
    Mcp,
    Scheduled,
    /// One Provider's models: the Provider is the area's scope.
    Models {
        provider_id: String,
    },
    Workflow,
    Permissions,
}

impl Area {
    /// The slash command and the subcommand stem of the area.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Skills => "skills",
            Self::Hooks => "hooks",
            Self::Mcp => "mcp",
            Self::Scheduled => "scheduled",
            Self::Models { .. } => "models",
            Self::Workflow => "workflow",
            Self::Permissions => "permissions",
        }
    }

    /// Whether the area's documents are per entry, so `edit` and `set` name
    /// one, rather than one document for the area.
    pub fn per_entry(&self) -> bool {
        matches!(
            self,
            Self::Scheduled | Self::Mcp | Self::Models { .. } | Self::Workflow
        )
    }

    /// Whether a request for this area names the workspace: the areas that
    /// read or write this project's own files.
    pub fn names_workspace(&self) -> bool {
        matches!(
            self,
            Self::Skills | Self::Hooks | Self::Mcp | Self::Workflow
        )
    }
}

/// What a management request asks of its area's owner.
#[derive(Debug, Clone, PartialEq)]
pub enum ManagementAction {
    /// Read the area.
    List,
    /// Enable or disable one entry, and report the area again.
    Toggle { id: String, enabled: bool },
    /// Re-read the area, or ask its owner to rescan it.
    Reload,
    /// Replace the area's document, or one entry's, if it is still at
    /// `expected_revision`.
    Save {
        /// The entry whose document this is, for an area whose documents are
        /// per entry; `None` writes the area's own document.
        id: Option<String>,
        expected_revision: String,
        document: serde_json::Value,
    },
    /// Create an entry from `document`.
    Add { document: serde_json::Value },
    /// Remove one entry.
    Remove { id: String },
    /// Perform the area's named operation on one entry, with the input
    /// document the editor left when the entry carries one.
    Run {
        id: String,
        /// The parameters the operation takes; `None` sends it at once.
        document: Option<serde_json::Value>,
        /// The input document the editor was opened with, `None` when the
        /// document came from elsewhere. The shared layer carries it and
        /// never reads it: an adapter that knows what it means applies its
        /// own rule.
        seed: Option<serde_json::Value>,
    },
    /// Read one entry's detail, in place of the list.
    Inspect { id: String },
}

/// One request to a management area, carrying the area and what it asks.
#[derive(Debug, Clone, PartialEq)]
pub struct ManagementRequest {
    pub area: Area,
    pub action: ManagementAction,
}

/// An area as its owner reports it, at the adapter's boundary.
///
/// The client infers nothing here: every fact comes from the owner's reply.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ManagementView {
    /// The title a scoped area's panel shows in place of the area's name;
    /// `None` draws the area's name.
    pub title: Option<String>,
    /// The revision the next write takes.
    pub revision: Option<String>,
    /// The area's document, when its owner reports one the client can edit.
    pub document: Option<serde_json::Value>,
    /// The document a new entry starts from, when the area can create one:
    /// the owner's create shape, filled with example values.
    pub create: Option<serde_json::Value>,
    /// The kinds that act on the area itself rather than on one entry, in the
    /// order a panel offers them: reload and add, neither of which needs a
    /// selection, so both work on an empty area. Add still needs this view's
    /// create document; without a view there is none, and the hint offers
    /// only reload.
    pub actions: Vec<ActionKind>,
    /// Entries under the source headings their owner reports.
    pub groups: Vec<ManagementGroup>,
    /// What the owner could not use, after the entries.
    pub issues: Vec<String>,
}

/// The entries one source holds.
#[derive(Debug, Clone, PartialEq)]
pub struct ManagementGroup {
    pub heading: String,
    pub entries: Vec<ManagementEntry>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ManagementEntry {
    /// The id the owner's actions take.
    pub id: String,
    pub label: String,
    /// What the entry says about itself, a line each, shown for the entry the
    /// panel has selected.
    pub detail: Vec<String>,
    /// Whether the entry is on; a toggle asks for the other state. `None`
    /// for an entry that has no on/off state, such as a command rule.
    pub enabled: Option<bool>,
    /// What the row names besides on and off.
    pub states: Vec<EntryState>,
    pub actions: Vec<ActionKind>,
    /// The verb the remove confirmation and outcome use.
    pub remove_verb: RemoveVerb,
    /// The entry's own document, for an area whose documents are per entry
    /// and whose entries offer edit.
    pub document: Option<EntryDocument>,
    /// The operation's input document when the run kind takes one: the panel
    /// opens the editor on it before the run. `None` sends the run at once.
    pub run: Option<serde_json::Value>,
}

/// What a remove does, for the confirmation and the outcome wording.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum RemoveVerb {
    /// Take the entry out of the area.
    #[default]
    Remove,
    /// Return the entry to its owner's default.
    Reset,
}

/// One entry's document and the revision a write of it takes.
#[derive(Debug, Clone, PartialEq)]
pub struct EntryDocument {
    pub revision: String,
    pub document: serde_json::Value,
}

/// A property of an entry that its row names besides on and off.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntryState {
    /// The owner could not use what it found there; the entry's detail says
    /// what the owner reported.
    Invalid,
    /// Another entry of the same name takes precedence.
    Shadowed { by: String },
    /// The owner cannot use it as it is; the entry's detail says why.
    Unavailable,
    /// The owner does not let the client write the entry; the entry's detail
    /// says why.
    ReadOnly,
}

/// An action kind the client offers. Every kind has one key in every panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionKind {
    Toggle,
    Edit,
    Reload,
    Add,
    Remove,
    Run,
    Inspect,
}

/// A management request and what it came to.
#[derive(Debug, Clone)]
pub struct ManagementReply {
    pub request: ManagementRequest,
    pub result: ManagementResult,
}

/// What a management request came to.
#[derive(Debug, Clone, PartialEq)]
pub enum ManagementResult {
    /// The area as its owner reports it after the request, and what the
    /// owner answered beyond the view: a run's state, or the entry an add
    /// created. A read has no note.
    View {
        view: Box<ManagementView>,
        note: Option<String>,
    },
    /// The action was applied and the read that reports the area failed:
    /// the change was made, and `read_error` says why the area is not shown.
    /// `note` is the owner's answer when that is more than the view, such as
    /// a run's state; without one, the surface composes the action's line
    /// with the entry's name from the read it holds.
    Applied {
        note: Option<String>,
        read_error: String,
    },
    /// What an inspect found, in place of the list.
    Detail(ManagementDetail),
    /// Nothing was applied, and this is why.
    Refused(ManagementRefusal),
}

/// One entry's read-only detail, in place of the list.
#[derive(Debug, Clone, PartialEq)]
pub struct ManagementDetail {
    /// What the detail is about, as the panel's title.
    pub title: String,
    /// Its lines, in the order the owner reports them.
    pub lines: Vec<String>,
}

/// Why a management request did not produce a view.
#[derive(Debug, Clone, PartialEq)]
pub enum ManagementRefusal {
    /// The owner kept the stored document because it changed since the client
    /// read it. Nothing was written; the area was read again, and this is
    /// what it now is, or why that read failed.
    Conflict {
        message: String,
        view: Result<Box<ManagementView>, String>,
    },
    /// The daemon does not advertise the methods, or the owner refused the
    /// text: nothing was written.
    Refused(String),
}

impl std::fmt::Display for ManagementRefusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Conflict { message, view } => match view {
                Ok(_) => formatter.write_str(message),
                Err(error) => write!(formatter, "{message} {error}"),
            },
            Self::Refused(message) => formatter.write_str(message),
        }
    }
}

/// One change the `/plugins` panel asks PluginManager for.
#[derive(Debug, Clone, PartialEq)]
pub enum PluginAction {
    Enable(String),
    Disable(String),
    Reload(String),
    /// Install from a local directory and turn it on.
    Install(std::path::PathBuf),
}

/// One name of `plugin.list` (PluginManager A.1).
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginItem {
    pub name: String,
    /// The candidate in use: an installed id, `builtin` or `default`.
    #[serde(rename = "use")]
    pub in_use: Option<String>,
    pub off: bool,
    pub locked: bool,
    pub candidates: Vec<PluginCandidate>,
    /// `running`, `pending`, `failed`, `stopped` or `off`; none when the
    /// plugin in use has no code.
    pub status: Option<String>,
    pub error: Option<String>,
    #[serde(default)]
    pub waiting_for: Vec<String>,
    #[serde(default)]
    pub refused: Vec<PluginRefusal>,
}

#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
pub struct PluginCandidate {
    pub id: String,
}

/// Content an owner refused: its kind and the owner's error.
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
pub struct PluginRefusal {
    pub kind: String,
    pub error: String,
}

/// What a `plugin.*` change replied (A.1) that `plugin.list` does not show;
/// `install` also names the plugin.
#[derive(Debug, Clone, Default, PartialEq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginReply {
    pub id: Option<String>,
    pub name: Option<String>,
    pub enabled: Option<bool>,
    #[serde(default)]
    pub skipped: Vec<String>,
    #[serde(default)]
    pub restart_required: bool,
}

/// Where a runtime's interpreter comes from (RuntimeManager §2.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RuntimeSource {
    Download,
    System,
}

impl RuntimeSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Download => "download",
            Self::System => "system",
        }
    }

    /// The other source: what `s` switches to.
    pub fn flipped(self) -> Self {
        match self {
            Self::Download => Self::System,
            Self::System => Self::Download,
        }
    }

    /// A typed `download` or `system`.
    pub fn parse(word: &str) -> Option<Self> {
        [Self::Download, Self::System]
            .into_iter()
            .find(|source| source.as_str() == word)
    }
}

/// One change the `/runtime` panel or a typed `/runtime` form asks for.
#[derive(Debug, Clone, PartialEq)]
pub enum RuntimeAction {
    /// Apply the configuration as it stands: acquire, provide again or
    /// re-acquire under a changed source (RuntimeManager §5).
    Install(String),
    /// Write one runtime's configuration, then apply it.
    Configure {
        name: String,
        source: Option<RuntimeSource>,
        auto: Option<bool>,
    },
}

/// Why `/runtime` could not read the daemon's runtimes: a daemon that does not
/// serve them, which the panel names with its version, or what the call said.
#[derive(Debug, Clone, PartialEq)]
pub enum RuntimesError {
    Unsupported { version: String },
    Failed(String),
}

/// One runtime of `runtime.list` (RuntimeManager §5).
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
pub struct RuntimeItem {
    pub name: String,
    pub source: RuntimeSource,
    /// The configuration in effect: whether it is turned on at startup.
    /// Absent from a daemon that does not report it.
    #[serde(default)]
    pub auto: Option<bool>,
    /// The fixed version for `download`; for `system`, the version of the
    /// checked interpreter, absent until it is checked.
    pub version: Option<String>,
    /// `off`, `acquiring`, `failed`, `starting`, `ready` or `down`.
    pub status: String,
    /// Why it is `failed` or `down`.
    pub reason: Option<String>,
}

/// What a sign-in needs to begin.
#[derive(Debug, Clone, PartialEq)]
pub struct OauthStart {
    pub attempt: u64,
    pub provider_id: String,
    /// The account namespace the provider's sign-in methods belong to.
    pub namespace: String,
    pub device: bool,
    pub email: Option<String>,
}

/// A described account operation: the provider and its account namespace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountTarget {
    pub provider_id: String,
    pub namespace: String,
}

/// A Provider entry's `account` descriptor (`provider.management`): the
/// namespace its account methods live in and the sign-in methods it offers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountDescriptor {
    pub namespace: String,
    pub login_methods: Vec<String>,
}

/// How a sign-in is going.
#[derive(Debug, Clone, PartialEq)]
pub enum OauthEvent {
    /// Browser mode: open `url`; `opened` says whether the browser launched.
    Url {
        url: String,
        opened: bool,
    },
    Device {
        verification_uri: String,
        verification_uri_complete: Option<String>,
        user_code: String,
    },
    Exchanging,
    #[allow(dead_code)]
    SlowDown(u64),
    /// Signed in, or why not (`None` error text = cancelled).
    Done(Result<String, Option<String>>),
}

/// One change to a provider.
#[derive(Debug, Clone, PartialEq)]
pub enum ProviderAction {
    Configure {
        provider_id: String,
        expected_version: u32,
        api_key: Option<String>,
        base_url: Option<String>,
        enabled: Option<bool>,
    },
    Disconnect {
        provider_id: String,
        expected_version: u32,
    },
    /// `Add custom provider`: an OpenAI-compatible endpoint with a bearer
    /// key, probing its models before creating the record.
    Create {
        provider_id: String,
        name: String,
        base_url: String,
        api_key: String,
    },
}

/// The signed-in Dim account, as `account.status` reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    pub issuer: String,
    pub subject: String,
    pub email: Option<String>,
    pub name: Option<String>,
}

/// The account's plan and consumption (`account.usage`). Every part is
/// optional; an unlimited bucket reports its totals as 0.
#[derive(Debug, Clone, Default, PartialEq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    pub plan: Option<UsagePlan>,
    pub credits: Option<UsageCredits>,
    #[serde(default)]
    pub windows: Vec<UsageWindow>,
    #[serde(default)]
    pub features: Vec<UsageFeature>,
}
#[derive(Debug, Clone, Default, PartialEq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsagePlan {
    pub name: Option<String>,
    pub status: Option<String>,
    /// In minor units (fen, cents); divide by 100 before showing it.
    pub price_amount: Option<f64>,
    pub price_currency: Option<String>,
    pub price_interval: Option<String>,
    pub expires_at: Option<String>,
}
#[derive(Debug, Clone, Default, PartialEq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageCredits {
    pub used_units: Option<u64>,
    pub total_units: Option<u64>,
    pub remaining_units: Option<u64>,
    pub unlimited: Option<bool>,
    pub resets_at: Option<String>,
}
#[derive(Debug, Clone, Default, PartialEq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageWindow {
    pub duration_hours: u64,
    pub token_used: u64,
    pub token_cap: u64,
    pub expires_at: Option<String>,
}
#[derive(Debug, Clone, Default, PartialEq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageFeature {
    pub key: String,
    pub used: Option<u64>,
    pub allowance: Option<u64>,
    pub remaining: Option<u64>,
    pub unit: Option<String>,
    pub unlimited: Option<bool>,
    pub period_end: Option<String>,
}

/// One provider of the `/connect` list.
#[derive(Debug, Clone)]
pub struct ProviderEntry {
    pub id: String,
    pub version: u32,
    pub name: String,
    pub driver: String,
    pub status: String,
    pub is_current: bool,
    pub enabled: bool,
    #[allow(dead_code)]
    pub custom: bool,
    pub base_url: Option<String>,
    pub api_key_preview: Option<String>,
    /// Present when the daemon describes an account this provider signs in
    /// through.
    pub account: Option<AccountDescriptor>,
}

/// One model the picker offers.
#[derive(Debug, Clone)]
pub struct ModelItem {
    pub provider_id: String,
    pub provider_name: String,
    pub provider_version: u32,
    pub model_id: String,
    pub display_name: String,
    pub zdr: bool,
    /// Advertised reasoning efforts, in cycle order. Empty when the model
    /// offers no choice.
    pub efforts: Vec<String>,
    /// Catalog default, when it is one of `efforts`.
    pub default_effort: Option<String>,
    /// Saved `reasoningEffort`, when it is one of `efforts`.
    pub saved_effort: Option<String>,
}

/// Why a usable provider contributes no selectable model.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum EmptyReason {
    /// It lists no models: none discovered yet, or none at all. `listable`
    /// is whether `provider.refresh` can ask it for a listing.
    NoModels { listable: bool },
    /// Some models are unavailable and some are disabled.
    Mixed { unavailable: usize, disabled: usize },
    /// Every model it lists is unavailable.
    Unavailable { count: usize },
    /// Every model it lists is disabled.
    Disabled { count: usize },
    /// Its catalog could not be read, so nothing is known about its models.
    Unreadable,
}

/// A usable provider with no selectable model.
#[derive(Debug, Clone)]
pub struct EmptyProvider {
    pub provider_id: String,
    pub provider_name: String,
    pub reason: EmptyReason,
}

/// A usable provider whose catalog holds models no source declares a
/// context window or maximum output for.
#[derive(Debug, Clone)]
pub struct NeedsLimitsProvider {
    pub provider_id: String,
    pub provider_name: String,
    pub count: usize,
}

/// Which model new Collaboration members use (`collaboration.model_policy`).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "mode", rename_all = "lowercase")]
pub enum SubagentModelPolicy {
    Agent,
    Inherit,
    Fixed { model: dim_agent_types::ModelId },
}

/// What became of the subagent model policy.
#[derive(Debug, Clone)]
pub enum ModelPolicyUpdate {
    /// The source as read; `saved` after this client's own write.
    Read {
        revision: String,
        policy: SubagentModelPolicy,
        saved: bool,
    },
    /// Another write won; reread before saving again.
    Conflict,
    /// A read failed or a write's outcome is unknown; reread before saving.
    Failed(String),
}

/// The models and which one is in effect.
#[derive(Debug, Clone, Default)]
pub struct ModelList {
    pub models: Vec<ModelItem>,
    /// Whether the daemon manages Providers through `provider.management`:
    /// the picker offers a Provider's models for editing only when it does.
    pub manage: bool,
    pub current: Option<(String, String)>,
    /// The usable providers that offer no selectable model, with why, so the
    /// picker can name them.
    pub empty: Vec<EmptyProvider>,
    /// The usable providers whose models include ones with undeclared
    /// limits, with how many.
    pub limits: Vec<NeedsLimitsProvider>,
}

/// One stored session as the picker shows it.
#[derive(Debug, Clone)]
pub struct SessionItem {
    pub id: String,
    pub title: Option<String>,
    pub preview: Option<String>,
    pub cwd: String,
    pub updated_at: u64,
    pub model: String,
    /// The creation origin the daemon reports (`client`, `acp`, `internal`,
    /// `unknown`); absent from a daemon that does not report one.
    pub origin: Option<String>,
}

/// The Session's stored title, unless it has none: empty, or the
/// `New session` placeholder an untitled Session carries. The one rule the
/// `/resume` row and the window title share (`ui::title`).
pub fn stored_title(title: Option<&str>) -> Option<&str> {
    let title = title?.trim();
    (!title.is_empty() && !title.eq_ignore_ascii_case("new session")).then_some(title)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwitchKind {
    New,
    Resumed,
    /// A fork of the Session shown before.
    Forked,
}

/// The main branch's context use, in tokens: what the next request is
/// estimated to send, the window it must fit, and the compaction threshold
/// the server returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextReading {
    pub input: u64,
    pub window: u64,
    pub threshold: u64,
}

/// Failures `/compact` reports independently of committed outcomes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompactReply {
    /// Failure of the call itself, with its message.
    Failed(String),
    /// The daemon offers no `session.context.compact`.
    Unsupported,
}

/// A `session.rename` reply as `/rename` reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenameReply {
    /// The manual title the server stored.
    Titled(String),
    /// The Session has no title again.
    Untitled,
    /// The daemon serves no `session.rename` for this Session.
    Unsupported,
    /// The call failed, with its message.
    Failed(String),
}

#[allow(dead_code)]
pub enum Update {
    /// A queued input the TUI withdrew (`Command::InputControl` with action
    /// `withdraw`) is deleted: its text goes back into the draft.
    Withdrawn {
        input_id: String,
    },
    /// The session's adopted configuration: `provider / model`, and the
    /// reasoning effort it sends when it carries one, label the status line.
    Ready(Configuration),
    /// What the session's model offers, once its catalog entry is known:
    /// image input, and the priority/default service tiers of `/fast`.
    ModelFeatures {
        images: bool,
        service_tiers: bool,
    },
    Snapshot(Box<ViewSnapshot>),
    ApprovalPending {
        branch_id: String,
        request_id: String,
    },
    QuestionReply {
        session_id: String,
        call: crate::question::Call,
        reply: crate::question::Reply,
    },
    TaskOutput {
        projection_id: u64,
        target: TaskTarget,
        /// None retains the last preview when a read fails.
        text: Option<String>,
        state: TaskOutputState,
    },
    MemberConversation {
        projection_id: u64,
        session_id: String,
        snapshot: Box<ViewSnapshot>,
        state: MemberConversationState,
        /// The member's latest adopted model, as observed.
        model: Option<String>,
    },
    /// How many files the run that just ended changed.
    FilesChanged(usize),
    /// The active session changed; snapshots that follow belong to it.
    Switched {
        session_id: String,
        kind: SwitchKind,
    },
    /// The stored sessions, or why they could not be listed.
    Sessions {
        id: String,
        paged: bool,
        result: Result<(Vec<SessionItem>, Option<String>), String>,
    },
    /// The workspaces the picker may offer as its repository scope: more than
    /// one, found once per picker open.
    RepositoryWorkspaces(Vec<String>),
    /// A command failed without ending the session.
    Notice(String),
    /// A newer version the user has not skipped, once per start.
    Upgrade(crate::update::Offer),
    /// What installing the offered version came to.
    UpgradeInstalled {
        version: String,
        result: Result<(), String>,
    },
    Models(Result<ModelList, String>),
    EffortChoices(Result<crate::configure::EffortChoices, String>),
    /// A catalog `models()` asked `provider.refresh` to fill holds models
    /// now: a picker that is still open reads the list again.
    ModelsRefilled,
    /// Sent only by a daemon that advertises the policy methods.
    ModelPolicy(ModelPolicyUpdate),
    Isolation(Result<ExecutionSnapshot, String>),
    /// Read once with the switches; a failed read adds no line and leaves the
    /// switches usable.
    IsolationBackend(Result<SandboxBackend, String>),
    /// Whether the Session's method table lists `session.suggestion.request`:
    /// without it the interface neither asks for an offer nor shows the row.
    SuggestionMethods(bool),
    /// The `prompt-suggestion` switch, as last read or written.
    SuggestionSetting(Result<SuggestionSetting, String>),
    /// A turn completed on `main`: the interface may ask for the next
    /// message when nothing else has its attention.
    TurnCompleted {
        turn_id: String,
    },
    /// The Session offered the message the user would send next.
    SuggestionOffer {
        turn_id: String,
        text: String,
    },
    /// The offer for `turn_id` is withdrawn.
    SuggestionCleared {
        turn_id: String,
    },
    /// The Session paused suggestions for the rest of it after the unused
    /// count reached its limit: said once per Session.
    SuggestionThrottled,
    /// A transient status-line message.
    Toast(String),
    Permissions(Result<dim_agent_types::PermissionPolicy, String>),
    PermissionSelected(dim_agent_types::PermissionPreset),
    Providers(Result<Vec<ProviderEntry>, String>),
    /// What a provider change came to.
    ProviderResult {
        result: Result<String, String>,
        providers: Result<Vec<ProviderEntry>, String>,
    },
    Plugins(Result<Vec<PluginItem>, String>),
    /// What a management request came to.
    Management(ManagementReply),
    /// What a plugin change came to, and the plugins listed after it.
    PluginResult {
        action: PluginAction,
        result: Result<PluginReply, String>,
        plugins: Result<Vec<PluginItem>, String>,
    },
    /// The runtimes of `/runtime` and the plugins that wait on them; `result`
    /// is present when this answers a change.
    Runtimes {
        result: Option<Result<(), String>>,
        runtimes: Result<Vec<RuntimeItem>, RuntimesError>,
        plugins: Result<Vec<PluginItem>, String>,
    },
    /// The account of the `/login` panel, or none when signed out.
    Login(Result<Option<Account>, String>),
    /// The usage of the `/login` panel's account.
    Usage(Result<Usage, String>),
    /// The Dim account usage requested by `/usage`.
    UsageReport(Result<Usage, String>),
    /// The main branch's context use; none while the session's usage is
    /// unknown.
    Context(Option<ContextReading>),
    /// What a `/compact` came to, localized by the interface.
    Compacted(CompactReply),
    /// What a `/rename` came to, localized by the interface.
    Renamed(RenameReply),
    /// Progress of the sign-in tagged `attempt`.
    Oauth {
        attempt: u64,
        event: OauthEvent,
    },
    /// Output the run `run_id` has produced since the last; `skipped` bytes
    /// were left out before it.
    ShellProgress {
        run_id: String,
        text: String,
        skipped: u64,
    },
    /// The run `run_id` ended, or why its outcome is unknown.
    ShellDone {
        run_id: String,
        result: Result<ShellResult, String>,
    },
    /// A report to commit as a status block; `warning` colours it.
    Report {
        text: String,
        warning: bool,
    },
    /// The Web entry switch after `/web`; `url` is present while the entry
    /// is open and carries its sign-in fragment.
    Web {
        enabled: bool,
        open: bool,
        url: Option<String>,
    },
    /// The session cannot continue.
    Failed(String),
    /// The App Server asked this interface to leave (`ui.shutdown`); the
    /// Session goes on without it.
    Shutdown {
        reason: String,
    },
}

/// The seam between the interactive UI and the session worker.
///
/// The UI sends [`Command`]s and drains [`Update`]s through this and nothing
/// else, so the worker behind it can be replaced or faked without touching a
/// widget.
pub trait SessionPort {
    /// Queues a command for the worker.
    fn send(&self, command: Command);
    /// Everything the worker published since the last call.
    fn drain(&self) -> Vec<Update>;
    /// Closes the connection and gives the worker a moment to wind down.
    fn shutdown(self: Box<Self>);
    fn submit(&self, text: String, mode: SubmitMode) {
        self.send(Command::Submit(text, mode));
    }
    fn stop(&self) {
        self.send(Command::Stop);
    }
}

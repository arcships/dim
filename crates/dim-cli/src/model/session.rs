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

pub enum Command {
    Submit(String),
    /// A draft with pastes to store or content blocks to send.
    SubmitDraft(Vec<DraftPart>),
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
    /// List stored sessions (newest first).
    ListSessions {
        id: String,
        query: String,
        cursor: Option<String>,
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
    ClearModelPreferences {
        provider_id: String,
        model_id: String,
        expected_version: u32,
    },
    /// Produce the `/usage` report.
    Usage,
    /// `/compact`: ask the shown line to compact its context.
    Compact,
    SetPermission(crate::permission::Change),
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
    /// Change one plugin; the worker lists the plugins again after it.
    Plugin(PluginAction),
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

/// A `session.context.compact` reply as `/compact` reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompactReply {
    Completed,
    Skipped(String),
    /// `compaction_failed` or another failure of the call, with its message.
    Failed(String),
    Cancelled(String),
    /// The daemon offers no `session.context.compact`.
    Unsupported,
}

#[allow(dead_code)]
pub enum Update {
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
    /// A catalog `models()` asked `provider.refresh` to fill holds models
    /// now: a picker that is still open reads the list again.
    ModelsRefilled,
    /// Sent only by a daemon that advertises the policy methods.
    ModelPolicy(ModelPolicyUpdate),
    Isolation(Result<ExecutionSnapshot, String>),
    /// Read once with the switches; a failed read adds no line and leaves the
    /// switches usable.
    IsolationBackend(Result<SandboxBackend, String>),
    /// A transient status-line message.
    Toast(String),
    Permissions(Result<dim_agent_types::PermissionPolicy, String>),
    PermissionSelected(crate::permission::Change),
    Providers(Result<Vec<ProviderEntry>, String>),
    /// What a provider change came to.
    ProviderResult {
        result: Result<String, String>,
        providers: Result<Vec<ProviderEntry>, String>,
    },
    Plugins(Result<Vec<PluginItem>, String>),
    /// What a plugin change came to, and the plugins listed after it.
    PluginResult {
        action: PluginAction,
        result: Result<PluginReply, String>,
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
    fn submit(&self, text: String) {
        self.send(Command::Submit(text));
    }
    fn stop(&self) {
        self.send(Command::Stop);
    }
}

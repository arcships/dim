//! Runs the App Server session for the interactive interface on a worker thread.
//!
//! The terminal loop is synchronous; the client is async. The worker owns the
//! connection and the projection, receives commands, and publishes view
//! snapshots: one a frame interval after the first change, however fast the
//! stream runs, so text arrives steadily and a burst of events costs one
//! snapshot. Snapshot rows are shared with the projection, not copied.
//!
//! Features the protocol v3 App Server does not offer yet (provider
//! management, usage, approval preferences) answer with an explicit "not
//! available" update instead of failing silently.

use std::sync::mpsc;
use std::time::Duration;

use dim_agent_types::{
    ApprovalCallData, ApprovalDecision, DEFAULT_BRANCH_ID, ModelId, ModelPreferences,
    ROOT_BRANCH_ID,
};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

use crate::args::Options;
use crate::client::{self, Client, Message};
use crate::model::session::{
    ACCOUNT_NAMESPACE, Account, AccountDescriptor, Command, CompactReply, ContextReading,
    DIM_OAUTH_ID, DeleteOutcome, DeleteReply, DraftPart, EmptyProvider, EmptyReason,
    ExecutionSnapshot, ManagementReply, ModelItem, ModelList, ModelOutcome, ModelPolicyUpdate,
    NeedsLimitsProvider, OauthEvent, OauthStart, Origin, PluginItem, PluginReply, PromptField,
    PromptSetting, ProviderAction, ProviderEntry, RuntimeAction, RuntimeItem, RuntimesError,
    SandboxBackend, SessionItem, SessionPort, SessionsScope, SubagentModelPolicy,
    SuggestionSetting, SwitchKind, Update, Usage, stored_title,
};
use crate::model::view::{
    Earlier, LineState, LineView, SideLine, TaskOutputState, TaskTarget, WorkflowInventory,
};
use crate::projection::Projection;
use crate::wire::{
    Attached, CatalogModel, Configuration, Created, DisplayNotice, Event, EventContent,
    EventNotice, Progress, ProviderCatalogSnapshot,
};
use crate::{
    configure, mcp, permission, session_create_from_options, session_create_params, update,
};

mod compaction;
mod inventory;
mod member;
mod report;
mod tasks;
mod workflow;
use member::Members;
use tasks::{BranchHistory, HistoryRequest, StopBatch};
use workflow::WorkflowReads;

const PUBLISH_INTERVAL: Duration = Duration::from_millis(16);
const OUTPUT_INTERVAL: Duration = Duration::from_secs(1);
/// The complete Provider editing interface, without legacy write fallbacks.
const PROVIDER_METHODS: [&str; 5] = [
    "provider.management",
    "provider.management.list",
    "provider.create",
    "provider.update",
    "provider.probe",
];

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ManagementInfo {
    provider_id: String,
    source: String,
    display_name: String,
    enabled: Option<bool>,
    version: Option<u32>,
    connection: Option<ManagementConnection>,
    account: Option<ManagementAccount>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ManagementEntry {
    #[serde(flatten)]
    info: ManagementInfo,
    preferences: std::collections::BTreeMap<String, serde_json::Value>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ManagementUpdateSummary {
    #[serde(flatten)]
    info: ManagementInfo,
    models_listed: Option<ModelsListed>,
}
/// `provider.management`'s safe account descriptor.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ManagementAccount {
    namespace: String,
    login_methods: Vec<String>,
    state: Option<ManagementAccountState>,
}

#[derive(Deserialize)]
struct ManagementAccountState {
    account: Option<crate::model::session::Account>,
}
/// `provider.update`'s answer to what it did with the record's models when
/// the patch changed its connection.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ModelsListed {
    Listed,
    Bundled,
    Unsupported,
    Disabled,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ManagementConnection {
    driver_kind: String,
    auth: serde_json::Value,
    base_url: String,
    credential_configured: bool,
    #[serde(default)]
    glued_prefix: Option<String>,
}
#[derive(Deserialize)]
struct ProviderManagement {
    providers: Vec<ManagementEntry>,
}
#[derive(Deserialize)]
struct ProviderManagementPage {
    providers: Vec<ManagementInfo>,
    errors: Vec<ManagementError>,
    next: Option<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ManagementError {
    provider_id: String,
    message: String,
}
/// The subagent model policy's management methods (`/models`).
const MODEL_POLICY_METHODS: [&str; 2] = [
    "collaboration.model_policy",
    "collaboration.set_model_policy",
];
const LOGIN_POLL: Duration = Duration::from_secs(1);
const NO_SESSION: &str = "Sign in with /login or choose a model with /models to start a session.";
const PLUGIN_MISSING: &str = "The DimAgent account plugin is not installed in this build";
const USAGE_MISSING: &str = "This account plugin does not report usage";
const QUEUE_LEFT: &str = "That message has already left the queue.";

pub struct SessionHost {
    commands: UnboundedSender<Command>,
    updates: mpsc::Receiver<Update>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl SessionPort for SessionHost {
    fn send(&self, command: Command) {
        let _ = self.commands.send(command);
    }
    fn drain(&self) -> Vec<Update> {
        self.updates.try_iter().collect()
    }
    fn shutdown(self: Box<Self>) {
        SessionHost::shutdown(*self);
    }
}

impl SessionHost {
    /// Runs the session on `client`, the connection the composition root
    /// took from the client tree, composing management text in `language`,
    /// the interface's language code. `notice` is shown once before the
    /// Session opens; it says a daemon of another build was kept in use.
    pub fn start(
        options: Options,
        client: Client,
        language: &'static str,
        notice: Option<String>,
        ctx: rutis::Ctx,
        leaf_end: tokio::sync::oneshot::Sender<()>,
    ) -> Self {
        let (commands, command_rx) = unbounded_channel();
        let (update_tx, updates) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("dim-session".into())
            .spawn(move || {
                crate::management::use_language(language);
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        let _ = update_tx.send(Update::Failed(format!("runtime: {error}")));
                        return;
                    }
                };
                runtime.block_on(run(
                    options, client, notice, command_rx, update_tx, ctx, leaf_end,
                ));
            })
            .expect("spawn session thread");
        Self {
            commands,
            updates,
            thread: Some(thread),
        }
    }

    /// Closes the command channel. The worker is not joined: it may still be
    /// inside the App Server start (seconds), and leaving the interface must
    /// not wait for that. The process exit reaps the thread.
    pub fn shutdown(mut self) {
        drop(std::mem::replace(&mut self.commands, unbounded_channel().0));
        if let Some(thread) = self.thread.take() {
            let deadline = std::time::Instant::now() + Duration::from_millis(150);
            while !thread.is_finished() && std::time::Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            if thread.is_finished() {
                let _ = thread.join();
            }
        }
    }
}

struct LineProjection {
    projection: Projection,
    cursor: u64,
}

impl LineProjection {
    fn new(branch: &str) -> Self {
        Self {
            projection: Projection::for_branch(branch),
            cursor: 0,
        }
    }
}

fn transcript_mut<'a>(
    main: &'a mut Projection,
    lines: &'a mut std::collections::BTreeMap<String, LineProjection>,
    branch: &str,
) -> Option<&'a mut Projection> {
    if let Some(line) = lines.get_mut(branch) {
        return Some(&mut line.projection);
    }
    (branch == ROOT_BRANCH_ID).then_some(main)
}

/// The session the worker is attached to and what the screen shows of it.
struct Worker {
    options: Options,
    client: Client,
    updates: mpsc::Sender<Update>,
    ctx: rutis::Ctx,
    leaf_end: Option<tokio::sync::oneshot::Sender<()>>,
    ui_identity: Option<crate::leaf::Opened>,
    session_id: String,
    /// The Session's stored title, when it has a real one, and the newest
    /// `titleRevision` this client has: the attach record's or a later
    /// SessionStore notice's.
    session_title: Option<String>,
    title_revision: u64,
    /// The attached Session's workspace, which the management panels work on.
    /// A server older than the record leaves it unset, and the directory the
    /// client started in stands.
    session_cwd: Option<String>,
    /// The Session `/new` left: still attached until the next one is entered.
    parked: String,
    projection: Projection,
    retired_inputs: std::collections::HashMap<String, bool>,
    question_responses: Vec<(u64, String, crate::question::Call)>,
    approval_details: std::collections::BTreeMap<u64, MemberDetailRead>,
    /// Highest event sequence applied; replayed or duplicate events are skipped.
    cursors: std::collections::HashMap<String, u64>,
    tool_origins: std::collections::VecDeque<(String, String, String)>,
    projection_id: u64,
    /// The line the Session's conversation is on: the target a request that
    /// names no line reaches, and the line the top bar folds (design §4, §14).
    default_branch: String,
    focus: String,
    /// The line the side panel follows.
    side: Option<String>,
    /// One transcript per line the user opened, folded from its events.
    lines: std::collections::BTreeMap<String, LineProjection>,
    task_history: std::collections::HashMap<String, BranchHistory>,
    task_history_queue: std::collections::VecDeque<String>,
    /// A stale request still occupies this slot until its reply arrives.
    task_history_request: Option<HistoryRequest>,
    methods: std::collections::HashSet<String>,
    report_generations: std::collections::HashMap<u64, u64>,
    model_through: u64,
    task_stops: Option<StopBatch>,
    workflow: WorkflowReads,
    members: Members,
    compaction: compaction::CompactionReads,
    output: Option<OutputWatch>,
    /// The admitted read can outlive its detail; false discards that old reply.
    output_request: Option<(u64, bool)>,
    catalog_request: Option<(u64, String, bool, SessionsScope, bool)>,
    /// When the next snapshot goes out; set by the first unpublished change.
    publish_at: Option<tokio::time::Instant>,
    /// The `shell.run` in flight: its request id and run id.
    shell: Option<(u64, String)>,
    /// This interface's id in the daemon's UI registry (`ui.attach`), when
    /// the daemon keeps one.
    ui_id: Option<String>,
    /// `shell.stop` requests awaiting their replies.
    shell_stops: Vec<u64>,
    /// The model the session adopted, for the preference commands.
    model: Option<Configuration>,
    /// The configuration each line folded from its own events, with the
    /// sequence of the fact: the bar shows the default branch's own when the
    /// conversation moves (design §14).
    line_configurations: std::collections::HashMap<String, LineConfiguration>,
    /// The line this entry already reported a missing configuration for: the
    /// window that follows a move stays quiet, and a later entry reports again
    /// (design §14).
    unreadable_line: Option<String>,
    /// The model Home previews, which the first message's Session adopts.
    preview: Option<Configuration>,
    /// The preference Home chose for that model: the members it named, which
    /// the server applies over the model's saved values. Dropped once Home
    /// shows another model or the Session starts.
    pending: Option<(ModelId, ModelPreferences)>,
    model_session: Option<String>,
    /// The MCP sign-in waiting for its callback.
    mcp_attempt: Option<McpAttempt>,
    /// Callbacks the local listener captured.
    mcp_callbacks: (UnboundedSender<McpCallback>, UnboundedReceiver<McpCallback>),
    /// Servers already reported as needing sign-in.
    mcp_required: std::collections::HashSet<String>,
    /// The managed-provider sign-in of the `/login` panel.
    login: Option<Login>,
    /// Browser callbacks its local listener captured.
    login_callbacks: (UnboundedSender<McpCallback>, UnboundedReceiver<McpCallback>),
    opening_reads: Vec<(u64, OpeningRead)>,
    /// The centre handoffs since this interface started: an opening read that
    /// left before the last one describes a line that is no longer the
    /// conversation's (design §14).
    moves: u64,
    /// The composer's context reading is out of date: a model request on main
    /// finished, a run on main ended, a compaction result arrived on main, or
    /// the session or its model changed.
    context_stale: bool,
    context_request: Option<(u64, bool)>,
    /// The last `model/usage` on main, folded from the events so `/usage`
    /// can report its cache read share.
    last_usage: Option<LastUsage>,
    /// The outstanding `session.context.compact` requests (`/compact`).
    compact_requests: Vec<u64>,
    /// Whether this Session's method table lists `session.suggestion.request`:
    /// `None` until the first completed turn reads it, and again after a
    /// Session switch.
    suggestion_methods: Option<bool>,
    /// Whether this Session already said that the unused count paused its
    /// suggestions, so later refusals stay quiet; a switch starts over.
    suggestion_throttle_noted: bool,
}

struct MemberDetailRead {
    projection_id: u64,
    branch_id: String,
    request_id: String,
    allow: bool,
}

struct OutputWatch {
    target: TaskTarget,
    poll_at: Option<tokio::time::Instant>,
}

enum OpeningRead {
    Catalog(ModelId),
    /// The policy of `line` as of the move `generation`: a reply that arrives
    /// after the conversation moved on is stale (design §14).
    Permissions {
        line: String,
        generation: u64,
    },
    Skills,
}

fn catalog_features(entry: Option<CatalogModel>) -> Update {
    let images = entry.as_ref().is_none_or(|entry| {
        entry.capabilities.vision || entry.capabilities.modalities.iter().any(|m| m == "image")
    });
    let service_tiers = entry
        .and_then(|entry| entry.metadata?.service_tier)
        .is_some_and(|tier| {
            tier.supported
                && ["priority", "default"]
                    .iter()
                    .all(|value| tier.options.iter().any(|option| option == value))
        });
    Update::ModelFeatures {
        images,
        service_tiers,
    }
}

struct Login {
    /// The panel's attempt tag for its updates.
    tag: u64,
    migrate_legacy: bool,
    provider_id: String,
    /// The account namespace the attempt's methods belong to.
    namespace: String,
    attempt_id: String,
    /// Device login: when its status is asked next.
    poll_at: Option<tokio::time::Instant>,
    /// Browser login: the local listener on the redirect address, when the
    /// port was free. A pasted callback works either way.
    listener: Option<tokio::task::JoinHandle<()>>,
}

impl Drop for Login {
    fn drop(&mut self) {
        if let Some(listener) = self.listener.take() {
            listener.abort();
        }
    }
}

struct McpAttempt {
    server_id: String,
    attempt_id: String,
    deadline: tokio::time::Instant,
    listener: Option<tokio::task::JoinHandle<()>>,
}

/// What the local callback listener heard for `attempt_id`.
struct McpCallback {
    attempt_id: String,
    callback: crate::Result<(String, tokio::net::TcpStream)>,
}

async fn run(
    options: Options,
    client: Client,
    notice: Option<String>,
    mut commands: UnboundedReceiver<Command>,
    updates: mpsc::Sender<Update>,
    ctx: rutis::Ctx,
    leaf_end: tokio::sync::oneshot::Sender<()>,
) {
    if let Some(notice) = notice {
        let _ = updates.send(Update::Notice(notice));
    }
    if update::enabled() {
        let offers = updates.clone();
        let home = options.home.clone();
        let managed = options.server.is_none() && client.offers("daemon.shutdown.ifIdle");
        tokio::spawn(async move {
            if let Some(mut offer) = update::available(&home, env!("CARGO_PKG_VERSION")).await {
                offer.direct &= managed;
                let _ = offers.send(Update::Upgrade(offer));
            }
        });
    }
    let mut worker = Worker::new(options, client, updates.clone(), ctx, leaf_end);
    worker.send(Update::Permissions(Ok(dim_agent_types::PermissionPolicy {
        preset: worker
            .options
            .permission
            .unwrap_or(dim_agent_types::PermissionPreset::WorkspaceWrite),
        ..Default::default()
    })));
    if let Err(error) = worker.open().await {
        let _ = updates.send(Update::StartupFailed {
            message: error.to_string(),
            resumable: client::resume_can_succeed(&error),
        });
        return;
    }
    loop {
        if let Err(error) = worker.poll_member().await {
            worker.send(Update::Failed(error.to_string()));
            break;
        }
        if let Err(error) = worker.poll_task_history().await {
            worker.send(Update::Failed(error.to_string()));
            break;
        }
        worker.poll_task_stops().await;
        worker.poll_workflow().await;
        worker.poll_context().await;
        worker.poll_compaction().await;
        let workflow_poll = worker.workflow.next_at.filter(|_| worker.workflow_ready());
        let login_poll = worker.login.as_ref().and_then(|login| login.poll_at);
        let output_poll = worker
            .output
            .as_ref()
            .and_then(|output| output.poll_at)
            .filter(|_| worker.output_request.is_none());
        tokio::select! {
            command = commands.recv() => {
                let Some(command) = command else { break };
                // The command is judged against the centre it was typed on
                // before Home can start a Session: a send left over from a
                // Session a handoff replaced starts nothing.
                let Some(command) = worker.typed_command(command) else { continue };
                if worker.session_id.is_empty()
                    && matches!(
                        command,
                        Command::Submit { .. }
                            | Command::SubmitDraft { .. }
                            | Command::ShellRun { .. }
                    )
                    && let Err(error) = worker.start_session().await
                {
                    if !worker.session_id.is_empty() {
                        worker.send(Update::StartupFailed {
                            message: error.to_string(),
                            resumable: client::resume_can_succeed(&error),
                        });
                        break;
                    }
                    let error =
                        if worker.options.startup.is_some() || worker.model_session.is_some() {
                            error.to_string()
                        } else {
                            no_session(&error)
                        };
                    worker.send(match command {
                        Command::ShellRun { run_id, .. } => Update::ShellDone {
                            run_id,
                            result: Err(error),
                        },
                        _ => Update::SubmitNotSent(error),
                    });
                    continue;
                }
                let submitted_line = match &command {
                    Command::Submit { line, .. } | Command::SubmitDraft { line, .. } => {
                        Some(line.as_str())
                    }
                    _ => None,
                };
                if let Some(line) = submitted_line
                    && line == worker.focus
                    && worker
                        .task_history
                        .get(&worker.focus)
                        .is_some_and(|history| history.error.is_some())
                {
                    worker.send(Update::SubmitNotSent(
                        "Conversation state could not be read; retry the read before submitting"
                            .into(),
                    ));
                    continue;
                }
                if let Err(error) = worker.command(command).await {
                    let _ = updates.send(Update::Failed(error.to_string()));
                    break;
                }
            }
            message = worker.client.next() => {
                let message = match message {
                    Ok(message) => message,
                    Err(error) => {
                        let _ = updates.send(Update::Failed(error.to_string()));
                        break;
                    }
                };
                if let Err(error) = worker.message(message).await {
                    let _ = updates.send(Update::Failed(error.to_string()));
                    break;
                }
            }
            Some(callback) = worker.mcp_callbacks.1.recv() => worker.mcp_callback(callback).await,
            Some(callback) = worker.login_callbacks.1.recv() => {
                worker.login_callback(callback).await
            },
            _ = tokio::time::sleep_until(output_poll.unwrap_or_else(tokio::time::Instant::now)),
                if output_poll.is_some() => {
                    if let Err(error) = worker.poll_task_output().await {
                        worker.send(Update::Failed(error.to_string()));
                        break;
                    }
                },
            _ = tokio::time::sleep_until(login_poll.unwrap_or_else(tokio::time::Instant::now)),
                if login_poll.is_some() => worker.poll_login().await,
            _ = tokio::time::sleep_until(workflow_poll.unwrap_or_else(tokio::time::Instant::now)),
                if workflow_poll.is_some() => {},
            _ = tokio::time::sleep_until(
                worker.publish_at.unwrap_or_else(tokio::time::Instant::now),
            ),
                if worker.publish_at.is_some() => worker.publish(),
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WebToggled {
    enabled: bool,
    open: bool,
    #[serde(default)]
    url: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct StateNotice {
    session_id: String,
    branch_id: String,
    running: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ContextUsage {
    context_window: f64,
    output_tokens: f64,
    threshold_tokens: f64,
    estimated_input_tokens: f64,
    baseline_source: String,
}

impl ContextUsage {
    fn input(&self) -> u64 {
        tokens(self.estimated_input_tokens)
    }

    fn window(&self) -> u64 {
        tokens(self.context_window)
    }

    /// `/usage`'s one line.
    fn describe(&self) -> String {
        let (input, window) = (self.input(), self.window());
        let percent = (input * 100).checked_div(window).unwrap_or(0);
        let basis = match self.baseline_source.as_str() {
            "provider_baseline" => "counted by the provider",
            _ => "estimated",
        };
        format!(
            "Context: {} / {} tokens ({percent}%), {basis} · output up to {} · compacts at {}",
            group(input),
            group(window),
            group(tokens(self.output_tokens)),
            group(tokens(self.threshold_tokens)),
        )
    }
}

fn tokens(value: f64) -> u64 {
    value.max(0.0).round() as u64
}

/// One line's adopted configuration and the sequence of the fact that carried
/// it: what the top bar shows while the conversation is on that line.
struct LineConfiguration {
    sequence: u64,
    configuration: Configuration,
}

/// The last `model/usage` fact on `main`: the counters `/usage` reports the
/// cache read share of.
#[derive(Clone, Copy)]
struct LastUsage {
    prompt_tokens: u64,
    cache_read_tokens: Option<u64>,
}

impl LastUsage {
    /// The share `/usage` prints, or `None` when the counters cannot
    /// produce one.
    fn percent(&self) -> Option<String> {
        cache_hit_rate(self.prompt_tokens, self.cache_read_tokens)
    }
}

/// The cache read share of one request, floored at every step so the figure
/// never flatters: `100%` only when every prompt token was a cache read, one
/// decimal rounded down between 99% and 100%, an integer rounded down below
/// that, and `<1%` for a positive share too small for an integer. `None` when
/// the request reported no cache read count, its cache reads exceed its
/// prompt, or its prompt is zero, so a reported zero stays `0%`.
fn cache_hit_rate(prompt_tokens: u64, cache_read_tokens: Option<u64>) -> Option<String> {
    let cache_read_tokens = cache_read_tokens?;
    if prompt_tokens == 0 || cache_read_tokens > prompt_tokens {
        return None;
    }
    if cache_read_tokens == prompt_tokens {
        return Some("100%".to_owned());
    }
    if cache_read_tokens == 0 {
        return Some("0%".to_owned());
    }
    let tenths = u128::from(cache_read_tokens) * 1000 / u128::from(prompt_tokens);
    Some(if tenths >= 990 {
        format!("{}.{}%", tenths / 10, tenths % 10)
    } else if tenths >= 10 {
        format!("{}%", tenths / 10)
    } else {
        "<1%".to_owned()
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClosedNotice {
    session_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApprovalNotice {
    session_id: String,
    branch_id: String,
    request_id: String,
    call: ApprovalCallData,
    reason: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CancelNotice {
    session_id: String,
    request_id: String,
    branch_id: String,
}

impl Worker {
    fn new(
        options: Options,
        client: Client,
        updates: mpsc::Sender<Update>,
        ctx: rutis::Ctx,
        leaf_end: tokio::sync::oneshot::Sender<()>,
    ) -> Self {
        Self {
            options,
            client,
            updates,
            ctx,
            leaf_end: Some(leaf_end),
            ui_identity: None,
            session_id: String::new(),
            session_title: None,
            title_revision: 0,
            session_cwd: None,
            parked: String::new(),
            model_session: None,
            projection: Projection::default(),
            retired_inputs: Default::default(),
            question_responses: Vec::new(),
            approval_details: Default::default(),
            cursors: Default::default(),
            tool_origins: Default::default(),
            projection_id: 0,
            default_branch: ROOT_BRANCH_ID.into(),
            focus: DEFAULT_BRANCH_ID.into(),
            side: None,
            lines: Default::default(),
            task_history: Default::default(),
            task_history_queue: Default::default(),
            task_history_request: None,
            methods: Default::default(),
            report_generations: Default::default(),
            model_through: 0,
            task_stops: None,
            workflow: WorkflowReads::default(),
            members: Members::default(),
            compaction: Default::default(),
            output: None,
            output_request: None,
            catalog_request: None,
            publish_at: None,
            shell: None,
            ui_id: None,
            shell_stops: Vec::new(),
            model: None,
            line_configurations: Default::default(),
            unreadable_line: None,
            preview: None,
            pending: None,
            mcp_attempt: None,
            mcp_callbacks: unbounded_channel(),
            mcp_required: Default::default(),
            login: None,
            login_callbacks: unbounded_channel(),
            opening_reads: Vec::new(),
            moves: 0,
            context_stale: false,
            context_request: None,
            last_usage: None,
            compact_requests: Vec::new(),
            suggestion_methods: None,
            suggestion_throttle_noted: false,
        }
    }

    async fn cancel_catalog(&mut self) -> crate::Result<()> {
        if let Some((request, ..)) = self.catalog_request.take()
            && self.client.offers("session.catalog.cancel")
        {
            self.client
                .send("session.catalog.cancel", json!({"requestId":request}))
                .await?;
        }
        Ok(())
    }

    /// The catalog `cwd` for `scope`: the launch workspace, the git set (which
    /// may hold one entry), or none.
    fn scope_cwd(&self, scope: &SessionsScope) -> Option<serde_json::Value> {
        scope_cwd(self.options.workspace().as_str(), scope)
    }

    /// Resumes the requested Session; an ordinary startup stays at Home.
    async fn open(&mut self) -> crate::Result<()> {
        if let Some(id) = self.options.resume.clone() {
            if self.options.permission.is_some()
                && !self.client.supports(&permission::PRESET_METHODS)
            {
                return Err(
                    "This daemon cannot change a Session's permission preset; omit --permission"
                        .into(),
                );
            }
            // A launch preset or model is applied to the line the attach reply
            // names, after the attachment. Registering this interface first
            // would make the resumed leaf observable, and addressable for
            // approvals, while the old policy or model is still in force; it
            // therefore registers once they are applied (design §14).
            let deferred = self.options.permission.is_some()
                || self.options.startup.is_some()
                || self.options.effort.is_some()
                || self.options.model.is_some();
            self.attach(id.clone(), SwitchKind::Resumed, !deferred)
                .await?;
            // The preset belongs to the line the attach reply named
            // (design §14); a directory read would be one round trip older.
            if let Some(preset) = self.options.permission {
                let branch = self.default_branch.clone();
                permission::select(&mut self.client, &id, &branch, preset)
                    .await
                    .map_err(|error| {
                        client::resume_failure("Cannot set the permission preset", error)
                    })?;
            }
            adopt_startup(&mut self.client, &self.options, &id, &self.default_branch).await?;
            if deferred {
                self.register_ui().await?;
            }
            return Ok(());
        }
        if self.client.offers("session.model.preview")
            && let Err(error) = self.preview_model().await
        {
            self.send(Update::Notice(format!(
                "Cannot preview startup model: {error}"
            )));
        }
        Ok(())
    }

    async fn preview_model(&mut self) -> crate::Result<()> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Preview {
            model: ModelId,
            model_preferences: Option<dim_agent_types::ModelPreferences>,
        }
        let preview: Preview = tokio::time::timeout(Duration::from_secs(5), async {
            let mut params = session_create_from_options(&mut self.client, &self.options).await?;
            let object = params.as_object_mut().expect("creation parameters");
            object.remove("permissionPolicy");
            // Creation-only member: `session.model.preview` denies it.
            object.remove("worktree");
            self.client.typed("session.model.preview", params).await
        })
        .await??;
        let configuration = Configuration {
            catalog_provider_id: None,
            provider_id: preview.model.provider_id,
            model_id: preview.model.model_id,
            capability_snapshot: None,
            model_preferences: preview.model_preferences,
        };
        self.preview = Some(configuration.clone());
        self.send(Update::Ready(configuration));
        Ok(())
    }

    /// Creates Home's first Session using the launch settings or the model
    /// selected or inherited there.
    async fn start_session(&mut self) -> crate::Result<()> {
        let attached_session = self.invalidate_projection();
        let initial = self.model.is_none() && self.model_session.is_none();
        let mut params = if initial {
            session_create_from_options(&mut self.client, &self.options).await?
        } else {
            let mut params = session_create_params(&self.options);
            // `session_create_params` contacts no daemon, so the member
            // `--worktree` asks for is added here; the initial path adds it
            // in `session_create_from_options`.
            crate::add_creation_worktree(&self.client, &self.options, &mut params)?;
            params
        };
        // A choice made at Home came after the launch flag, so it replaces
        // that flag's member and leaves the rest of the creation's
        // preferences alone. `modelFromSession` denies the member entirely.
        let from_session =
            self.model_session.is_some() && self.client.offers("session.create.modelFromSession");
        if let (false, Some((_, pending))) = (from_session, &self.pending) {
            let mut preferences = params["modelPreferences"]
                .as_object()
                .cloned()
                .unwrap_or_default();
            if let Some(effort) = &pending.reasoning_effort {
                preferences.insert("reasoningEffort".into(), json!(effort));
            }
            if let Some(tier) = &pending.service_tier {
                preferences.insert("serviceTier".into(), json!(tier));
            }
            params["modelPreferences"] = serde_json::Value::Object(preferences);
        }
        let mut target = if initial {
            self.options
                .provider
                .clone()
                .zip(self.options.model.clone())
        } else {
            None
        };
        let result = async {
            if let Some(source) = &self.model_session
                && self.client.offers("session.create.modelFromSession")
            {
                params["modelFromSession"] = json!(source);
            } else if let Some(model) = &self.model {
                let (provider, model) = (model.provider_id.clone(), model.model_id.clone());
                params["model"] = json!({"provider": provider, "modelId": model});
                target = Some((provider, model));
            }
            match self.client.typed::<Created>("session.create", params).await {
                Ok(created) => Ok(created),
                Err(error) => Err(crate::model_adoption_error(
                    &mut self.client,
                    target.as_ref().map(|(p, m)| (p.as_str(), m.as_str())),
                    error,
                )
                .await),
            }
        }
        .await;
        match result {
            Ok(created) => {
                let worktree = created.worktree.map(|worktree| {
                    format!(
                        "worktree: {}\t{}",
                        crate::sanitize::safe(&worktree.task_id),
                        crate::sanitize::safe(&worktree.root)
                    )
                });
                self.enter(
                    created.session_id,
                    Some(created.model),
                    SwitchKind::New,
                    attached_session,
                    true,
                )
                .await?;
                if let Some(worktree) = worktree {
                    self.send(Update::Notice(worktree));
                }
                Ok(())
            }
            Err(error) => {
                self.parked = attached_session;
                Err(error)
            }
        }
    }

    async fn enter(
        &mut self,
        session_id: String,
        model: Option<ModelId>,
        kind: SwitchKind,
        attached_session: String,
        register: bool,
    ) -> crate::Result<()> {
        if !attached_session.is_empty() && attached_session != session_id {
            self.client
                .rpc("session.detach", json!({"sessionId": attached_session}))
                .await?;
            self.ui_id = None;
        }
        let mut params = json!({"sessionId": session_id});
        if self
            .client
            .supports(&["session.history.display", "session.history.detail"])
        {
            params["display"] = json!(true);
        }
        let attached: Attached = match self.client.typed("session.attach", params).await {
            Ok(attached) => attached,
            Err(error)
                if kind != SwitchKind::New
                    && self.client.offers("report.prepare")
                    && crate::report::session_admission_failed(&error) =>
            {
                self.send(Update::Switched {
                    session_id: String::new(),
                    kind,
                    cwd: None,
                });
                self.reportable(
                    crate::report::Captured::observed(
                        crate::report::Operation::SessionRead,
                        dim_client::app_error(&error),
                        error.to_string(),
                    ),
                    true,
                );
                return Ok(());
            }
            Err(error) => return Err(error),
        };
        // Without a record, find the model in the catalog: this is the only
        // caller of `list()` besides `/resume`, and only for an older server.
        let model_preferences = attached.model_preferences;
        let model = match attached.model.or(model) {
            Some(model) => model,
            None => self.model_of(&session_id).await?,
        };
        let configuration = Configuration {
            catalog_provider_id: attached.catalog_provider_id,
            provider_id: model.provider_id.clone(),
            model_id: model.model_id.clone(),
            capability_snapshot: attached.capability_snapshot,
            model_preferences,
        };
        self.session_title = stored_title(attached.title.as_deref()).map(str::to_owned);
        self.title_revision = attached.title_revision;
        self.session_id = session_id.clone();
        self.model_session = Some(session_id.clone());
        self.unreadable_line = None;
        // The method table is read once per Session, before anything can ask
        // for an offer or probe the settings source: a Session that does not
        // list the methods is never asked.
        let offered = self.suggestion_offered().await;
        self.suggestion_methods = Some(offered);
        self.send(Update::SuggestionMethods(offered));
        self.suggestion_throttle_noted = false;
        self.session_cwd = attached.cwd;
        if register {
            self.register_ui().await?;
        }
        self.mcp_required.clear();
        // The reply's record is the root's adopted configuration, the model the
        // conversation keeps when it moves back to the root (design §14).
        self.line_configurations.insert(
            ROOT_BRANCH_ID.into(),
            LineConfiguration {
                sequence: 0,
                configuration: configuration.clone(),
            },
        );
        self.preview = None;
        self.pending = None;
        // The Session's conversation starts where its default branch is
        // (design §4): the attach reply names that line beside the root's
        // `sequence`, and the directory answers for a server that predates the
        // field (§14).
        self.default_branch = attached
            .default_branch
            .as_ref()
            .map(|cursor| cursor.branch_id.clone())
            .unwrap_or_else(|| ROOT_BRANCH_ID.into());
        if self.client.supports(&["session.branches"]) {
            let directory: dim_client::wire::BranchDirectory = self
                .client
                .typed("session.branches", json!({"sessionId":self.session_id}))
                .await?;
            if attached.default_branch.is_none() {
                self.default_branch = directory
                    .default_branch_id
                    .unwrap_or_else(|| ROOT_BRANCH_ID.into());
            }
            for entry in directory.branches {
                self.projection.directory(&entry);
                if entry.state == "ended" {
                    let history = self.task_history.entry(entry.branch_id).or_default();
                    history.ended = true;
                    history.done = true;
                } else {
                    self.discover_branch(&entry.branch_id);
                }
            }
        }
        // This attachment's centre is the first move of this interface; the
        // reads it starts belong to this generation (design §14).
        self.moves += 1;
        self.focus = self.default_branch.clone();
        let conversation_line = self.focus.clone();
        self.discover_branch(&conversation_line);
        let conversation_cursor = dim_client::wire::resume_cursor(
            attached.default_branch.as_ref(),
            attached.sequence,
            &conversation_line,
        );
        if self.client.offers("session.history.display.transcript")
            && let Some(cursor) = conversation_cursor
        {
            self.task_history
                .get_mut(&conversation_line)
                .expect("the conversation is discovered")
                .window
                .through = Some(cursor);
        }
        // A conversation on the root takes the reply's configuration as its
        // own; one on a line reads that line's adopted configuration at its
        // cursor (a display transcript omits the fact, and a bounded window can
        // start after it). A reply that names no line comes from a server that
        // cannot move the default branch, so its model is the Session's
        // (design §14).
        let seeded = attached.default_branch.is_none() || conversation_line == ROOT_BRANCH_ID;
        self.model_through = if seeded {
            conversation_cursor.unwrap_or(0)
        } else {
            0
        };
        self.poll_task_history().await?;
        if seeded {
            self.model = Some(configuration.clone());
            self.send(Update::Ready(configuration));
        } else {
            self.model = None;
            self.send(Update::NoModel);
        }
        self.send(catalog_features(None));
        self.send(Update::Skills(Vec::new()));
        self.send(Update::Switched {
            session_id: session_id.clone(),
            kind,
            cwd: self.session_cwd.clone(),
        });
        // The policy shown belonged to the Session this interface left; this
        // Session's own read publishes its own, and a refused or failed read
        // leaves it unknown rather than showing the outgoing preset (design
        // §14). A daemon that serves no preset read cannot report one, and the
        // launch preset stays the only policy this client knows.
        let mut reads = Vec::new();
        if self.client.offers("provider.catalog") {
            reads.push((
                "provider.catalog",
                json!({"providerId":model.provider_id,"modelId":model.model_id}),
                OpeningRead::Catalog(model),
            ));
        }
        if self.client.supports(&permission::PRESET_METHODS) {
            self.send(Update::PermissionsUnknown);
            reads.push((
                "session.permission.get",
                json!({"sessionId":session_id,"branchId":self.default_branch}),
                OpeningRead::Permissions {
                    line: self.default_branch.clone(),
                    generation: self.moves,
                },
            ));
        }
        if self.client.offers("skills.catalog") {
            match &self.session_cwd {
                Some(cwd) => {
                    reads.push(("skills.catalog", json!({"cwd":cwd}), OpeningRead::Skills))
                }
                None => self.send(Update::Notice(
                    "Skills unavailable: the daemon did not report the Session workspace".into(),
                )),
            }
        }
        for (method, params, kind) in reads {
            match self.client.send(method, params).await {
                Ok(id) => self.opening_reads.push((id, kind)),
                Err(error) => self.send(Update::Notice(format!("{method}: {error}"))),
            }
        }
        self.context_stale = true;
        self.publish();
        Ok(())
    }

    fn opening_reply(&mut self, kind: OpeningRead, result: Result<Value, client::RpcError>) {
        match kind {
            OpeningRead::Catalog(model) => {
                if !self.model.as_ref().is_some_and(|current| {
                    current.provider_id == model.provider_id && current.model_id == model.model_id
                }) {
                    return;
                }
                let entry = result
                    .ok()
                    .and_then(|value| serde_json::from_value::<ProviderCatalogSnapshot>(value).ok())
                    .and_then(|snapshot| snapshot.catalog)
                    .and_then(|catalog| {
                        catalog
                            .models
                            .into_iter()
                            .find(|entry| entry.id == model.model_id)
                    });
                self.send(catalog_features(entry));
            }
            OpeningRead::Permissions { line, generation } => {
                if generation != self.moves || line != self.default_branch {
                    return;
                }
                let policy = result
                    .map_err(|error| error.to_string())
                    .and_then(|value| {
                        serde_json::from_value::<permission::State>(value)
                            .map_err(|error| error.to_string())
                    })
                    .map(|state| state.policy)
                    .map_err(|error| format!("permission: unavailable ({error})"));
                self.send(Update::Permissions(policy));
            }
            OpeningRead::Skills => {
                if result
                    .as_ref()
                    .err()
                    .is_some_and(|error| error.code == -32601)
                {
                    self.send(Update::Skills(Vec::new()));
                    return;
                }
                match result.map_err(|error| error.to_string()).and_then(|value| {
                    serde_json::from_value::<crate::management::skills::Catalog>(value)
                        .map_err(|error| error.to_string())
                }) {
                    Ok(catalog) => self.send(Update::Skills(catalog.entries)),
                    Err(error) => self.send(Update::Notice(format!("Skills unavailable: {error}"))),
                }
            }
        }
    }

    async fn model_features(&mut self, model: &ModelId) -> Update {
        catalog_features(self.catalog_entry(model).await)
    }

    async fn catalog_entry(&mut self, model: &ModelId) -> Option<CatalogModel> {
        if !self.client.offers("provider.catalog") {
            return None;
        }
        let request = self.client.typed::<ProviderCatalogSnapshot>(
            "provider.catalog",
            json!({"providerId": model.provider_id,"modelId":model.model_id}),
        );
        let snapshot = tokio::time::timeout(Duration::from_secs(5), request)
            .await
            .ok()?
            .ok()?;
        snapshot
            .catalog?
            .models
            .into_iter()
            .find(|entry| entry.id == model.model_id)
    }

    async fn attach(
        &mut self,
        session_id: String,
        kind: SwitchKind,
        register: bool,
    ) -> crate::Result<()> {
        let attached_session = self.invalidate_projection();
        self.enter(session_id, None, kind, attached_session, register)
            .await
    }

    async fn suggestion_offered(&mut self) -> bool {
        if !self.client.offers("session.methods") {
            return false;
        }
        match self
            .client
            .typed::<dim_client::wire::MethodTable>(
                "session.methods",
                json!({"sessionId": self.session_id}),
            )
            .await
        {
            Ok(table) => {
                self.methods = table.methods.into_iter().map(|decl| decl.name).collect();
                self.methods.contains("session.suggestion.request")
            }
            Err(_) => false,
        }
    }

    async fn list(&mut self) -> crate::Result<Vec<SessionSummary>> {
        self.client.typed("session.summaries", json!({})).await
    }

    /// The model of one Session, found in the catalog. Only a server whose
    /// `session.attach` reply carries no record needs this.
    async fn model_of(&mut self, session_id: &str) -> crate::Result<ModelId> {
        let sessions = self.list().await?;
        sessions
            .into_iter()
            .find(|session| session.session_id == session_id)
            .map(|session| session.model)
            .ok_or_else(|| format!("Session {session_id} is absent from the catalog").into())
    }

    async fn submit(&mut self, text: String, mode: &str) {
        let params = json!({
            "sessionId": self.session_id,
            "branchId": self.focus,
            "submissionId": uuid::Uuid::new_v4().to_string(),
            "text": text,
            "mode": mode,
        });
        let result = self.client.rpc("session.submit", params).await;
        self.report_value_warnings(&result);
        self.report_failure(result);
    }

    /// Shows the submit checks' warnings, which a successful reply carries
    /// when a check did not do its own work (Hooks A.6.2): one warning row
    /// per element, for a submit, a steer and a new line alike.
    fn report_warnings(&mut self, warnings: &[dim_agent_types::SubmitWarning]) {
        for warning in warnings {
            self.report(warning.message.clone(), true);
        }
    }

    /// The same for a reply the worker decoded as a bare value.
    fn report_value_warnings(&mut self, result: &crate::Result<serde_json::Value>) {
        if let Ok(reply) = result {
            self.report_warnings(&crate::wire::warnings(reply));
        }
    }

    /// Stores the draft's pastes as attachments, then submits its text
    /// and blocks in order.
    async fn submit_draft(&mut self, mut parts: Vec<DraftPart>, mode: &str) {
        use dim_agent_types::ContentBlock;
        let mut content = Vec::new();
        let mut text = String::new();
        let mut pastes = 0;
        for part in parts.clone() {
            match part {
                DraftPart::Text(value) => text.push_str(&value),
                DraftPart::Paste(value) => {
                    pastes += 1;
                    #[derive(Deserialize)]
                    struct Stored {
                        path: String,
                    }
                    let lines = value.lines().count();
                    let stored: crate::Result<Stored> = self
                        .client
                        .typed(
                            "attachment.put",
                            json!({"sessionId": self.session_id, "text": value}),
                        )
                        .await;
                    match stored {
                        Ok(stored) => text
                            .push_str(&format!("[Pasted text: {} ({lines} lines)]", stored.path)),
                        Err(error) => {
                            self.send(Update::DraftNotSent {
                                parts,
                                message: format!("Paste {pastes}: {error}; not sent"),
                            });
                            return;
                        }
                    }
                }
                DraftPart::Block(block) => {
                    if !text.is_empty() {
                        content.push(ContentBlock::Text {
                            text: std::mem::take(&mut text),
                        });
                    }
                    content.push(block);
                }
            }
        }
        if !text.is_empty() {
            content.push(ContentBlock::Text { text });
        }
        if let Err(error) =
            crate::images::import(&mut self.client, &self.session_id, &mut content).await
        {
            let mut images = content
                .iter()
                .filter(|block| matches!(block, ContentBlock::Image { .. }));
            for part in &mut parts {
                if let DraftPart::Block(block @ ContentBlock::Image { .. }) = part {
                    *block = images.next().expect("draft image order").clone();
                }
            }
            self.send(Update::DraftNotSent {
                parts,
                message: format!("Image import failed: {error}; not sent"),
            });
            return;
        }
        let mut params = json!({
            "sessionId": self.session_id,
            "branchId": self.focus,
            "submissionId": uuid::Uuid::new_v4().to_string(),
            "mode": mode,
        });
        match content.as_slice() {
            [ContentBlock::Text { text }] => params["text"] = json!(text),
            _ => params["user"] = json!(dim_agent_types::UserInput { content }),
        }
        let result = self.client.rpc("session.submit", params).await;
        self.report_value_warnings(&result);
        self.report_failure(result);
    }

    /// A command typed for a Session and line only runs while it still names
    /// that pair: the shown line for a submit, a draft, a goal, a loop, a
    /// configuration, a compaction, an earlier read or a shell command, and
    /// the Session's default branch for `/usage` and a permission change,
    /// which act on it. A move or a Session handoff the key could not see
    /// cancels the command and returns what it carried to the draft, instead
    /// of writing it into another line or Session. Home is its own origin: a
    /// command typed there keeps the startup path it belongs to and never
    /// reaches a Session that an attachment brought in. The stop gestures name
    /// the Session and line they were pressed on: a handoff cancels them, while
    /// a move within the same Session still stops the line the key saw (design
    /// §14).
    fn typed_command(&mut self, command: Command) -> Option<Command> {
        const KEPT: &str = "The centre moved before the command ran; the draft was kept";
        let (origin, line, draft, cancelled) = match &command {
            Command::Submit {
                text,
                line,
                session,
                ..
            } => (
                session,
                Some(line),
                Some(vec![DraftPart::Text(text.clone())]),
                KEPT,
            ),
            Command::SubmitDraft {
                parts,
                line,
                session,
                ..
            } => (session, Some(line), Some(parts.clone()), KEPT),
            Command::Goal {
                line,
                session,
                draft,
                ..
            } => (
                session,
                Some(line),
                Some(vec![DraftPart::Text(draft.clone())]),
                KEPT,
            ),
            Command::Loop {
                line,
                session,
                draft,
                ..
            } => (
                session,
                Some(line),
                Some(vec![DraftPart::Text(draft.clone())]),
                KEPT,
            ),
            Command::Configure {
                text,
                line,
                session,
            } => (
                session,
                Some(line),
                Some(vec![DraftPart::Text(text.clone())]),
                KEPT,
            ),
            Command::Compact { line, session } => (
                session,
                Some(line),
                None,
                "The centre moved before /compact ran; nothing was compacted",
            ),
            Command::Usage { line, session } => (
                session,
                Some(line),
                None,
                "The centre moved before /usage ran; no report was printed",
            ),
            Command::Earlier { line, session } => (
                session,
                Some(line),
                None,
                "The centre moved before /earlier ran; nothing was loaded",
            ),
            Command::SetPermission { session, line, .. } => (
                session,
                Some(line),
                None,
                "The session or line moved before the permission change ran; nothing was changed",
            ),
            Command::Stop { session, .. } => (
                session,
                None,
                None,
                "The session moved before the stop ran; nothing was stopped",
            ),
            Command::SubmitNow { session, .. } => {
                (session, None, None, "The session moved before the send ran")
            }
            Command::ShellRun { line, session, .. } => (
                session,
                Some(line),
                None,
                "The centre moved before the shell command ran; nothing was run",
            ),
            _ => return Some(command),
        };
        let now = match &command {
            Command::Usage { .. } | Command::SetPermission { .. } => &self.default_branch,
            _ => &self.focus,
        };
        if self.typed_at(origin) && line.is_none_or(|line| line.is_empty() || line == now) {
            return Some(command);
        }
        match &command {
            Command::SubmitNow { .. } => self.send(Update::Stopped {
                error: Some(cancelled.into()),
            }),
            Command::ShellRun { run_id, .. } => self.send(Update::ShellDone {
                run_id: run_id.clone(),
                result: Err(cancelled.into()),
            }),
            _ => match draft {
                Some(parts) => self.send(Update::DraftNotSent {
                    parts,
                    message: cancelled.into(),
                }),
                None => self.send(Update::Notice(cancelled.into())),
            },
        };
        None
    }

    /// Whether `origin` is the centre this worker shows: Home while it has no
    /// Session, else that Session. A command read at another origin has been
    /// overtaken by a handoff and is cancelled.
    fn typed_at(&self, origin: &Origin) -> bool {
        match origin {
            Origin::Home => self.session_id.is_empty(),
            Origin::Session(session) => session == &self.session_id,
        }
    }

    async fn command(&mut self, command: Command) -> crate::Result<()> {
        match command {
            Command::Submit { text, mode, .. } => self.submit(text, mode.as_str()).await,
            Command::SubmitDraft { parts, mode, .. } => {
                self.submit_draft(parts, mode.as_str()).await
            }
            Command::FocusLine(line) => self.focus_line(line).await,
            Command::CloseSide => {
                self.side = None;
                self.publish();
            }
            Command::SetDefaultLine {
                session_id,
                projection_id,
                branch_id,
            } => {
                self.set_default_line(session_id, projection_id, branch_id)
                    .await;
            }
            Command::EndLine {
                session_id,
                projection_id,
                branch_id,
            } => {
                if branch_id == ROOT_BRANCH_ID {
                    self.send(Update::Notice(
                        "main is the session itself; a line ends, main does not".into(),
                    ));
                } else if !self.attached(&session_id, projection_id) {
                    self.send(Update::Notice(
                        "Session attachment changed; reopen the lines before ending one".into(),
                    ));
                } else {
                    let result = self
                        .client
                        .rpc(
                            "session.branch_end",
                            json!({"sessionId": self.session_id, "branchId": branch_id}),
                        )
                        .await;
                    self.report_failure(result);
                }
            }
            Command::OpenLine {
                session_id,
                projection_id,
                parent,
                at,
                prompt,
            } => {
                #[derive(Deserialize)]
                struct Opened {
                    branch: OpenedBranch,
                    #[serde(default)]
                    warnings: Vec<dim_agent_types::SubmitWarning>,
                }
                #[derive(Deserialize)]
                #[serde(rename_all = "camelCase")]
                struct OpenedBranch {
                    branch_id: String,
                }
                if !self.attached(&session_id, projection_id) {
                    self.send(Update::Notice(
                        "Session attachment changed; pick the message again".into(),
                    ));
                    return Ok(());
                }
                let opened = self
                    .client
                    .typed::<Opened>(
                        "session.branch_create",
                        json!({
                            "sessionId": self.session_id, "parentBranchId": parent,
                            "at": at, "prompt": prompt,
                        }),
                    )
                    .await;
                if let Ok(opened) = &opened {
                    self.report_warnings(&opened.warnings);
                    self.discover_branch(&opened.branch.branch_id);
                    self.line_projection(&opened.branch.branch_id);
                    self.side = Some(opened.branch.branch_id.clone());
                    self.publish();
                }
                if let Err(error) = opened
                    && !self.reportable(
                        crate::report::Captured::observed(
                            crate::report::Operation::BranchCreate,
                            dim_client::app_error(&error),
                            error.to_string(),
                        ),
                        true,
                    )
                {
                    self.send(Update::Notice(error.to_string()));
                }
            }
            Command::Fork {
                session_id,
                projection_id,
                at,
            } => {
                #[derive(Deserialize)]
                #[serde(rename_all = "camelCase")]
                struct Forked {
                    session_id: String,
                }
                if !self.attached(&session_id, projection_id) {
                    self.send(Update::Notice(
                        "Session attachment changed; pick the message again".into(),
                    ));
                    self.send(Update::ForkFailed);
                    return Ok(());
                }
                let forked = self
                    .client
                    .typed::<Forked>(
                        "session.fork",
                        json!({"sessionId": self.session_id, "at": at}),
                    )
                    .await;
                match forked {
                    Ok(forked) => {
                        self.attach(forked.session_id, SwitchKind::Forked, true)
                            .await?
                    }
                    Err(error) => {
                        if !self.reportable(
                            crate::report::Captured::observed(
                                crate::report::Operation::SessionFork,
                                dim_client::app_error(&error),
                                error.to_string(),
                            ),
                            true,
                        ) {
                            self.send(Update::Notice(error.to_string()));
                        }
                        self.send(Update::ForkFailed);
                    }
                }
            }
            Command::Stop { line, .. } => {
                let params = json!({
                    "sessionId": self.session_id,
                    "branchId": line,
                    "reason": crate::model::view::INTERRUPT_REASON,
                });
                let result = self.client.rpc("session.stop", params).await;
                self.report_failure(result);
            }
            Command::SubmitNow { line, .. } => {
                let params = json!({
                    "sessionId": self.session_id,
                    "branchId": line,
                    "reason": crate::model::view::INTERRUPT_REASON,
                });
                // `session.stop` replies once the turn has settled, so its
                // reply is the point the held draft may start a run.
                let error = self
                    .client
                    .rpc("session.stop", params)
                    .await
                    .err()
                    .map(|error| error.to_string());
                self.send(Update::Stopped { error });
            }
            Command::RetryApproval {
                request_id,
                branch_id,
            } => {
                self.read_member_approval(&branch_id, &request_id, false)
                    .await?;
            }
            Command::Approval {
                request_id,
                branch_id,
                allow,
                remember,
                grant,
            } => {
                let is_member = self.projection.approvals().iter().any(|approval| {
                    approval.request_id == request_id
                        && approval.branch_id == branch_id
                        && approval.tool_name == dim_agent_types::MEMBER_APPROVAL_ALLOW
                });
                if allow && is_member {
                    self.read_member_approval(&branch_id, &request_id, true)
                        .await?;
                    return Ok(());
                }
                if allow && let Some(grant) = grant {
                    let result = self
                        .client
                        .rpc(
                            "session.grants.add",
                            json!({
                                "sessionId": self.session_id,
                                "path": grant.path,
                                "kind": grant.kind,
                                "mode": grant.mode,
                            }),
                        )
                        .await;
                    if let Err(error) = result {
                        self.send(Update::Toast(format!("Grant not saved: {error}")));
                    }
                }
                // "Always" saves the exact allow rule first (F2 §5.4); the
                // call is allowed once either way, as the user asked.
                if allow && remember {
                    let rule = self
                        .projection
                        .approval_always(&branch_id, &request_id)
                        .and_then(|command| permission::proposed(&command, ""));
                    let notice = match rule {
                        Some(rule) => {
                            match permission::add_rule(&mut self.client, rule.clone()).await {
                                Ok(permission::Added::Added(_)) => {
                                    format!("Rule saved: {}", permission::describe(&rule))
                                }
                                Ok(permission::Added::Present) => {
                                    format!("Rule already present: {}", permission::describe(&rule))
                                }
                                Err(error) => format!("Rule not saved ({error}); allowed once"),
                            }
                        }
                        None => "No rule for this call; allowed once".to_owned(),
                    };
                    self.send(Update::Notice(notice));
                }
                let decision = if allow {
                    ApprovalDecision::Allow
                } else {
                    ApprovalDecision::Deny
                };
                let params = json!({
                    "sessionId": self.session_id,
                    "branchId": branch_id,
                    "requestId": request_id,
                    "decision": decision,
                });
                let result = self.client.rpc("approval.respond", params).await;
                if self.report_failure(result) {
                    self.projection.remove_approval(&branch_id, &request_id);
                    self.changed();
                }
            }
            // Home, not a Session: the first message creates one. Every
            // `/new` ends here, including the one that was already Home.
            Command::NewSession => {
                self.home();
                self.send(Update::NewSessionDone);
            }
            Command::DeleteSession { session_id, name } => {
                let outcome = self.delete_session(&session_id).await;
                let shown = session_id == self.session_id;
                // The shown Session leaves with the close: the notices queued
                // behind the reply then match no Session and are ignored, so
                // they raise no disconnect report.
                if shown && outcome.ends_session() {
                    self.home();
                }
                self.send(Update::Deleted(DeleteReply {
                    session_id,
                    name,
                    outcome,
                }));
            }
            Command::Resume(session_id) => {
                self.attach(session_id.clone(), SwitchKind::Resumed, true)
                    .await?;
                if let Some(preset) = self.options.permission
                    && self.client.supports(&permission::PRESET_METHODS)
                {
                    // The preset lands on the line the attach reply named,
                    // which this Home was not (design §14).
                    let branch = self.default_branch.clone();
                    if let Err(error) =
                        permission::select(&mut self.client, &session_id, &branch, preset).await
                    {
                        self.send(Update::Notice(format!(
                            "The permission preset was not applied: {error}"
                        )));
                    }
                }
            }
            Command::ListSessions {
                id,
                query,
                cursor,
                show_acp,
                scope,
            } => {
                self.cancel_catalog().await?;
                let paged = self.client.offers("session.catalog");
                let (method, params) = if paged {
                    let mut params = json!({"limit":50,"query":query,"withPreview":true});
                    if show_acp && self.client.offers("session.catalog.origins") {
                        params["withAcpSessions"] = json!(true);
                        if self.client.offers("session.catalog.acpHistory") {
                            params["withAcpHistory"] = json!(true);
                        }
                    }
                    if let Some(cwd) = self.scope_cwd(&scope) {
                        params["cwd"] = cwd;
                    }
                    if let Some(cursor) = cursor {
                        params["cursor"] = cursor.into();
                    }
                    ("session.catalog", params)
                } else {
                    ("session.summaries", json!({}))
                };
                match self.client.send(method, params).await {
                    Ok(request) => {
                        self.catalog_request = Some((request, id, paged, scope, show_acp))
                    }
                    Err(error) => self.send(Update::Sessions {
                        id,
                        paged,
                        result: Err(error.to_string()),
                    }),
                }
            }
            Command::FindRepository => {
                let usable = self.client.offers("session.catalog.workspaces")
                    || !self.client.offers("session.catalog");
                if usable {
                    let updates = self.updates.clone();
                    let workspace = self.options.workspace().as_str().to_owned();
                    tokio::spawn(async move {
                        let workspaces = repository_workspaces(&workspace).await;
                        // Any git success answers with the launch workspace at
                        // least, one worktree or several; a failure answers
                        // nothing, and the local scope keeps the exact launch
                        // workspace.
                        if !workspaces.is_empty() {
                            let _ = updates.send(Update::RepositoryWorkspaces(workspaces));
                        }
                    });
                }
            }
            Command::CloseSessions => self.cancel_catalog().await?,
            Command::Usage { line, .. } => {
                self.request_context(true, &line).await;
                if self
                    .model
                    .as_ref()
                    .is_some_and(|model| model.provider_id == DIM_OAUTH_ID)
                {
                    let usage = self.account_usage(DIM_OAUTH_ID).await;
                    self.send(Update::UsageReport(usage));
                }
            }
            Command::Compact { line, .. } => self.compact(&line).await,
            Command::Report(request) => self.report_request(request).await,
            Command::Export(format) => {
                let extension = match format {
                    crate::export::Format::Markdown => "md",
                    crate::export::Format::Json => "json",
                };
                let directory = match std::env::current_dir() {
                    Ok(directory) => directory,
                    Err(error) => {
                        self.report(error.to_string(), true);
                        return Ok(());
                    }
                };
                let path = directory.join(format!("session-{}.{extension}", uuid::Uuid::new_v4()));
                match crate::export::write(&mut self.client, &self.session_id, format, Some(&path))
                    .await
                {
                    Ok(()) => self.send(Update::Exported { format, path }),
                    Err(error) => self.report(error.to_string(), true),
                }
            }
            Command::Rename(title) => self.rename(title).await,
            Command::Earlier { line, .. } => self.earlier(&line),
            Command::Goal { request, .. } => self.goal(request).await,
            Command::Loop { request, .. } => self.loop_command(request).await,
            Command::CheckUpdate => {
                let updates = self.updates.clone();
                let managed =
                    self.options.server.is_none() && self.client.offers("daemon.shutdown.ifIdle");
                tokio::spawn(async move {
                    let mut checked = update::check(env!("CARGO_PKG_VERSION")).await;
                    if let Ok(Some(offer)) = &mut checked {
                        offer.direct &= managed;
                    }
                    let _ = updates.send(Update::UpgradeChecked(checked));
                });
            }
            Command::ToggleWeb => {
                let result: crate::Result<WebToggled> =
                    self.client.typed("web.toggle", json!({})).await;
                match result {
                    Ok(mut state) => {
                        let url = state.url.take().map(|url| self.web_url(&url));
                        let browser = state.enabled
                            && state.open
                            && url.as_deref().is_some_and(crate::browser::open);
                        state.url = url.map(|mut url| {
                            if browser && let Some(fragment) = url.find('#') {
                                url.truncate(fragment);
                            }
                            url
                        });
                        self.send(Update::Web {
                            enabled: state.enabled,
                            open: state.open,
                            url: state.url,
                        });
                    }
                    Err(error) => {
                        let executable = std::env::current_exe().unwrap_or_default();
                        self.send(Update::Notice(
                            match client::web_hint(&error, &executable, &self.options.home) {
                                Some((problem, fix)) => format!("{problem}. {fix}."),
                                None => format!("Web: {error}"),
                            },
                        ));
                    }
                }
            }
            Command::ListModels => {
                self.send_models(String::new(), None, 1).await;
                self.read_model_policy(false).await;
            }
            Command::QueryModels { query, after } => {
                self.send_models(query, after, 1).await;
            }
            Command::ReadIsolation => {
                if !self.client.supports(&["settings.read", "settings.update"]) {
                    self.send(Update::Isolation(Err(
                        "This daemon does not support execution settings".into(),
                    )));
                } else {
                    let result = self
                        .client
                        .typed(
                            "settings.read",
                            json!({"source": "execution", "scope": "user"}),
                        )
                        .await
                        .map_err(|error| error.to_string())
                        .and_then(execution_snapshot);
                    self.send(Update::Isolation(result));
                    if self.client.supports(&["host.execution.backend"]) {
                        let backend = self
                            .client
                            .typed::<SandboxBackend>("host.execution.backend", json!({}))
                            .await
                            .map_err(|error| error.to_string());
                        self.send(Update::IsolationBackend(backend));
                    }
                }
            }
            Command::SaveIsolation {
                expected_revision,
                sandbox,
                network,
            } => {
                let result = self
                    .client
                    .typed(
                        "settings.update",
                        json!({
                            "source": "execution",
                            "scope": "user",
                            "expectedRevision": expected_revision,
                            "document": {"sandbox": sandbox, "network": network},
                        }),
                    )
                    .await
                    .map_err(|error| error.to_string())
                    .and_then(execution_snapshot);
                self.send(Update::Isolation(result));
            }
            Command::ResetIsolation(expected_revision) => {
                let result = self
                    .client
                    .typed(
                        "settings.update",
                        json!({
                            "source": "execution",
                            "scope": "user",
                            "expectedRevision": expected_revision,
                            "document": {"sandbox": false, "network": false},
                        }),
                    )
                    .await
                    .map_err(|error| error.to_string())
                    .and_then(execution_snapshot);
                self.send(Update::Isolation(result));
            }
            Command::Suggest { turn_id } => {
                let offered = match self.suggestion_methods {
                    Some(offered) => offered,
                    None => {
                        let offered = self.suggestion_offered().await;
                        self.suggestion_methods = Some(offered);
                        offered
                    }
                };
                if offered {
                    #[derive(Deserialize)]
                    struct Reply {
                        #[serde(default)]
                        reason: Option<String>,
                    }
                    let reply = self
                        .client
                        .typed::<Reply>(
                            "session.suggestion.request",
                            json!({"sessionId": self.session_id, "turnId": turn_id}),
                        )
                        .await;
                    if let Ok(reply) = reply
                        && reply.reason.as_deref() == Some("throttled")
                        && !self.suggestion_throttle_noted
                    {
                        self.suggestion_throttle_noted = true;
                        self.send(Update::SuggestionThrottled);
                    }
                }
            }
            Command::SuggestionRespond { turn_id, accept } => {
                let action = if accept { "accept" } else { "dismiss" };
                let _ = self
                    .client
                    .rpc(
                        "session.suggestion.respond",
                        json!({
                            "sessionId": self.session_id,
                            "turnId": turn_id,
                            "action": action,
                        }),
                    )
                    .await;
            }
            Command::ReadSuggestionSetting => {
                if !self.client.supports(&["settings.read", "settings.update"]) {
                    self.send(Update::SuggestionSetting(Err(
                        "This daemon does not support prompt suggestions".into(),
                    )));
                } else {
                    let result = self
                        .client
                        .typed(
                            "settings.read",
                            json!({"source": "prompt-suggestion", "scope": "user"}),
                        )
                        .await
                        .map_err(|error| error.to_string())
                        .and_then(suggestion_setting);
                    self.send(Update::SuggestionSetting(result));
                }
            }
            Command::SaveSuggestionSetting {
                expected_revision,
                enabled,
                max_chars,
            } => {
                let result = self
                    .client
                    .typed(
                        "settings.update",
                        json!({
                            "source": "prompt-suggestion",
                            "scope": "user",
                            "expectedRevision": expected_revision,
                            "document": {"enabled": enabled, "maxChars": max_chars},
                        }),
                    )
                    .await
                    .map_err(|error| error.to_string())
                    .and_then(suggestion_setting);
                self.send(Update::SuggestionSetting(result));
            }
            Command::ReadPromptSettings => {
                let result = if self.client.supports(&["settings.read", "settings.update"]) {
                    self.client
                        .typed(
                            "settings.read",
                            json!({"source": "prompts", "scope": "user"}),
                        )
                        .await
                        .map_err(|error| error.to_string())
                } else {
                    Err("This daemon does not provide prompt overrides".into())
                };
                self.send(Update::PromptSettings {
                    result,
                    field: None,
                    request: None,
                });
            }
            Command::SavePromptSetting {
                field,
                text,
                revision,
                request,
            } => {
                let result = self.write_prompt(field, Some(&text), &revision).await;
                self.send(Update::PromptSettings {
                    result,
                    field: Some(field),
                    request: Some(request),
                });
            }
            Command::ResetPromptSetting {
                field,
                revision,
                request,
            } => {
                let result = self.write_prompt(field, None, &revision).await;
                self.send(Update::PromptSettings {
                    result,
                    field: Some(field),
                    request: Some(request),
                });
            }
            Command::ReadModelPolicy => self.read_model_policy(false).await,
            Command::SetModelPolicy {
                expected_revision,
                policy,
            } => {
                let result = self
                    .client
                    .rpc(
                        "collaboration.set_model_policy",
                        json!({"expectedRevision": expected_revision, "policy": policy}),
                    )
                    .await;
                match result {
                    Ok(_) => self.read_model_policy(true).await,
                    Err(error)
                        if client::app_error(&error)
                            == Some("collaboration_model_policy_conflict") =>
                    {
                        self.send(Update::ModelPolicy(ModelPolicyUpdate::Conflict));
                    }
                    Err(error) => self.send(Update::ModelPolicy(ModelPolicyUpdate::Failed(
                        error.to_string(),
                    ))),
                }
            }
            Command::ClearModelPreferences {
                query,
                batches,
                provider_id,
                model_id,
                expected_version,
            } => {
                let result = clear_model_preferences(
                    &mut self.client,
                    &provider_id,
                    &model_id,
                    expected_version,
                )
                .await;
                self.send_models(query, None, batches).await;
                self.send(Update::Notice(match result {
                    Ok(()) => {
                        "Saved model preferences cleared; select the model to adopt its defaults"
                            .into()
                    }
                    Err(error) => error.to_string(),
                }));
            }
            Command::SetPermission { change, .. } => {
                let result = if self.session_id.is_empty() {
                    Ok(dim_agent_types::PermissionPolicy {
                        preset: change.resolve(
                            self.options
                                .permission
                                .unwrap_or(dim_agent_types::PermissionPreset::WorkspaceWrite),
                        ),
                        ..Default::default()
                    })
                } else {
                    let session_id = self.session_id.clone();
                    permission::select_preset(
                        &mut self.client,
                        &session_id,
                        &self.default_branch,
                        change,
                    )
                    .await
                    .map_err(|error| error.to_string())
                };
                if let Ok(policy) = &result {
                    self.options.permission = Some(policy.preset);
                    self.send(Update::PermissionSelected(policy.preset));
                }
                self.send(Update::Permissions(result));
            }
            Command::ListProviders => {
                let providers = self.providers().await;
                self.send(Update::Providers(providers));
            }
            Command::Provider(action) => {
                let result = self.provider_action(action).await;
                let providers = self.providers().await;
                self.send(Update::ProviderResult { result, providers });
            }
            Command::OauthStart(start) => self.login_start(start).await,
            Command::OauthCancel => self.login_cancel().await,
            Command::OauthPaste(url) => self.login_complete(url, None).await,
            Command::RefreshAccountModels {
                provider_id,
                attempt,
            } => {
                self.refresh_account_models(&provider_id, attempt, false)
                    .await;
            }
            Command::LoginStatus(provider_id) => {
                let account = self
                    .account(&provider_id)
                    .await
                    .map_err(|error| error.to_string());
                self.send(Update::Login(account));
            }
            Command::ChatGptProfiles => self.chatgpt_profiles().await,
            Command::SelectChatGptProfile {
                profile_id,
                attempt,
            } => {
                let result = self
                    .client
                    .rpc("account.chatgpt.select", json!({"profileId": profile_id}))
                    .await;
                match result {
                    Ok(_) => {
                        self.chatgpt_profiles().await;
                        self.refresh_account_models(
                            crate::login::CHATGPT_PROVIDER_ID,
                            attempt,
                            true,
                        )
                        .await;
                    }
                    Err(error) => {
                        self.login_event(attempt, OauthEvent::Done(Err(Some(error.to_string()))))
                    }
                }
            }
            Command::LoginUsage(provider_id) => {
                let usage = self.account_usage(&provider_id).await;
                self.send(Update::Usage(usage));
            }
            Command::Logout(target) => {
                let method = format!("{}.logout", target.namespace);
                let result = self
                    .client
                    .rpc(
                        &method,
                        crate::login::target(&target.namespace, &target.provider_id),
                    )
                    .await;
                self.send(Update::Notice(match result {
                    Ok(reply)
                        if target.namespace == crate::login::CHATGPT_NAMESPACE
                            && reply.get("revocationConfirmed").and_then(Value::as_bool)
                                == Some(false) =>
                    {
                        format!(
                            "Signed out. OpenAI did not confirm revocation. Disconnect Dim at {}.",
                            crate::login::CHATGPT_USAGE
                        )
                    }
                    Ok(_) => "Signed out.".into(),
                    Err(error) => format!("Sign-out failed: {error}"),
                }));
                if target.namespace == ACCOUNT_NAMESPACE {
                    let account = self
                        .account(&target.provider_id)
                        .await
                        .map_err(|error| error.to_string());
                    self.send(Update::Login(account));
                } else {
                    let providers = self.providers().await;
                    self.send(Update::Providers(providers));
                }
            }
            Command::ShellRun {
                run_id,
                command,
                private,
                ..
            } => {
                if self.shell.is_some() {
                    self.send(Update::ShellDone {
                        run_id,
                        result: Err("A shell command is already running".into()),
                    });
                    return Ok(());
                }
                let params = json!({
                    "sessionId": self.session_id,
                    "branchId": self.focus,
                    "runId": run_id,
                    "command": command,
                    "mode": if private { "private" } else { "record" },
                });
                match self.client.send("shell.run", params).await {
                    Ok(id) => self.shell = Some((id, run_id)),
                    Err(error) => self.send(Update::ShellDone {
                        run_id,
                        result: Err(error.to_string()),
                    }),
                }
            }
            Command::ShellStop { run_id } => {
                let params = json!({"sessionId": self.session_id, "runId": run_id});
                match self.client.send("shell.stop", params).await {
                    Ok(id) => self.shell_stops.push(id),
                    Err(error) => self.send(Update::Notice(error.to_string())),
                }
            }
            Command::Mcp(args) => {
                let result = self.mcp(&args).await;
                if let Err(error) = result {
                    self.report(mcp::error(error).to_string(), true);
                }
            }
            Command::Recording(args) => {
                if let Err(error) = self.recording(&args).await {
                    self.report(error.to_string(), true);
                }
            }
            Command::ListPlugins => {
                let plugins = self.plugins().await;
                self.send(Update::Plugins(plugins));
            }
            Command::Plugin(action) => {
                let (method, params) = crate::plugin_command::call(&action);
                let result = self
                    .client
                    .typed::<PluginReply>(method, params)
                    .await
                    .map_err(|error| error.to_string());
                let plugins = self.plugins().await;
                self.send(Update::PluginResult {
                    action,
                    result,
                    plugins,
                });
            }
            Command::ListRuntimes => {
                let runtimes = self.runtimes().await;
                let plugins = self.plugins().await;
                self.send(Update::Runtimes {
                    result: None,
                    runtimes,
                    plugins,
                });
            }
            Command::Runtime(action) => {
                let result = self.runtime_change(&action).await;
                let runtimes = self.runtimes().await;
                let plugins = self.plugins().await;
                self.send(Update::Runtimes {
                    result: Some(result),
                    runtimes,
                    plugins,
                });
            }
            Command::Management(request) => {
                // The Session's workspace, not the directory the client
                // started in: a resumed Session may live elsewhere.
                let cwd = self
                    .session_cwd
                    .as_deref()
                    .unwrap_or(self.options.workspace().as_str());
                // A run a panel starts reports to the line the transcript
                // shows, the line a submit goes to. Home has no Session yet,
                // so it has no line to report to.
                let line = (!self.session_id.is_empty()).then(|| crate::management::SessionLine {
                    session_id: self.session_id.clone(),
                    branch_id: self.focus.clone(),
                });
                let applied =
                    crate::management::apply(&mut self.client, &request, Some(cwd), line.as_ref())
                        .await;
                self.send(Update::Management(ManagementReply {
                    request,
                    result: applied.result,
                }));
                // The palette lists the same entries the panel's view was
                // built from.
                if let Some(entries) = applied.skills {
                    self.send(Update::Skills(entries));
                }
            }
            Command::SetLocale(language) => crate::management::use_language(language),
            Command::Configure { text, .. } => {
                if self.session_id.is_empty() {
                    if configure::needs_session(&text) {
                        self.report(NO_SESSION.to_owned(), true);
                    } else {
                        self.configure_home(&text).await;
                    }
                    return Ok(());
                }
                let model = self.model.clone();
                let result = configure::run(
                    &mut self.client,
                    &self.session_id,
                    &self.focus,
                    model.as_ref(),
                    &text,
                )
                .await;
                if matches!(text.as_str(), "/effort" | "/reasoning") {
                    self.send(Update::EffortChoices(
                        result
                            .map(|report| report.effort.expect("bare effort query has choices"))
                            .map_err(|error| error.to_string()),
                    ));
                } else {
                    match result {
                        Ok(report) => self.report(report.lines.join("\n"), false),
                        Err(error) => self.report(error.to_string(), true),
                    }
                }
            }
            Command::SelectModel {
                provider_id,
                model_id,
                reasoning_effort,
                label,
            } => {
                if let Err(error) = self
                    .select_model(provider_id, model_id, reasoning_effort, label)
                    .await
                {
                    self.send(Update::Notice(format!("Model switch failed: {error}")));
                }
            }
            Command::AskAnswers {
                session_id,
                call,
                answers,
            } => {
                self.respond_question(session_id, call, Some(answers)).await;
            }
            Command::AskCancel { session_id, call } => {
                self.respond_question(session_id, call, None).await;
            }
            Command::StopQuestion {
                session_id,
                branch_id,
                stop_turn,
            } => {
                let session_line = self.default_branch.as_str();
                let also_session_line = stop_turn && branch_id != session_line;
                for branch in std::iter::once(branch_id.as_str())
                    .chain(also_session_line.then_some(session_line))
                {
                    let result = self.client.rpc("session.stop", json!({"sessionId":session_id,"branchId":branch,"reason":crate::model::view::INTERRUPT_REASON})).await;
                    self.report_failure(result);
                }
            }
            Command::StopTask {
                session_id,
                projection_id,
                target,
            } => {
                if session_id != self.session_id || projection_id != self.projection_id {
                    self.send(Update::Notice(
                        "Task attachment changed; reopen the panel before stopping work".into(),
                    ));
                    return Ok(());
                }
                let params = json!({
                    "sessionId": session_id,
                    "branchId": target.branch_id,
                    "taskId": target.task_id,
                });
                let result = self.client.rpc("session.exec.stop", params).await;
                self.report_failure(result);
            }
            Command::WatchTaskOutput {
                projection_id,
                target,
            } => {
                if projection_id == self.projection_id {
                    self.watch_task_output(target);
                }
            }
            Command::ToolOrigin {
                session_id,
                projection_id,
                branch_id,
                request_id,
                call_id,
            } => {
                if session_id == self.session_id
                    && projection_id == self.projection_id
                    && self.methods.contains("session.tool.get")
                {
                    let key = (branch_id.clone(), request_id.clone(), call_id.clone());
                    let queued = self.tool_origins.contains(&key)
                        || self.task_history_request.as_ref().is_some_and(|read| {
                            read.is_tool_origin(&branch_id, &request_id, &call_id)
                        });
                    let needed = transcript_mut(&mut self.projection, &mut self.lines, &branch_id)
                        .is_some_and(|projection| {
                            projection.rows().iter().any(|row| {
                                row.batch.as_deref() == Some(&request_id)
                                    && row.call_id.as_deref() == Some(&call_id)
                                    && !row.tool_lookup_done
                            })
                        });
                    if needed && !queued {
                        self.tool_origins.push_back(key);
                    }
                }
            }
            Command::CompactionDetails {
                projection_id,
                targets,
            } => {
                if projection_id == self.projection_id {
                    self.compaction_targets(targets);
                }
            }
            Command::WatchMember {
                projection_id,
                member_id,
            } => {
                if projection_id == self.projection_id {
                    self.watch_member(member_id);
                }
            }
            Command::TaskHistory { projection_id } => {
                if projection_id == self.projection_id {
                    self.retry_task_history();
                    self.discover_workflows();
                }
            }
            Command::StopWorkflow {
                session_id,
                projection_id,
                run_id,
            } => self.stop_workflow(session_id, projection_id, run_id).await,
            Command::StopTasks {
                session_id,
                projection_id,
            } => {
                self.stop_tasks(session_id, projection_id);
            }
            Command::StopMember {
                session_id,
                projection_id,
                member_id,
            } => {
                if session_id != self.session_id || projection_id != self.projection_id {
                    self.send(Update::Notice(
                        "Task attachment changed; reopen the panel before stopping work".into(),
                    ));
                    return Ok(());
                }
                let params = json!({
                    "sessionId": member_id,
                    "reason": "stopped by the user",
                });
                let result = self.client.rpc("session.stop", params).await;
                self.report_failure(result);
            }
            Command::InputControl { input_id, action } => {
                if let Some(notified) = self.retired_inputs.get_mut(&input_id) {
                    if !std::mem::replace(notified, true) {
                        self.send(Update::Notice(QUEUE_LEFT.into()));
                    }
                    return Ok(());
                }
                let Some(user) = self.shown().held_input(&input_id).cloned() else {
                    return Ok(());
                };
                let mut params = json!({
                    "sessionId": self.session_id,
                    "branchId": self.focus,
                    "inputId": input_id,
                });
                if action == "steer" {
                    params["steer"] = json!(user);
                }
                #[derive(Deserialize)]
                struct Withdrawn {
                    status: String,
                    #[serde(default)]
                    warnings: Vec<dim_agent_types::SubmitWarning>,
                }
                let result = self
                    .client
                    .typed::<Withdrawn>("session.input.withdraw", params)
                    .await;
                if let Ok(withdrawn) = &result {
                    self.report_warnings(&withdrawn.warnings);
                    self.retired_inputs
                        .insert(input_id, withdrawn.status == "notHeld");
                }
                match result {
                    Ok(withdrawn) if withdrawn.status == "notHeld" => {
                        self.send(Update::Notice(QUEUE_LEFT.into()))
                    }
                    Ok(withdrawn) if withdrawn.status == "deleted" && action == "withdraw" => {
                        self.send(Update::Withdrawn(user));
                    }
                    Ok(_) => {}
                    Err(error) => self.send(Update::Notice(error.to_string())),
                }
            }
        }
        Ok(())
    }

    async fn account_usage(&mut self, provider_id: &str) -> Result<Usage, String> {
        self.client
            .typed("account.usage", json!({"providerId": provider_id}))
            .await
            .map_err(
                |error| match error.downcast_ref::<client::RpcError>().and_then(app_error) {
                    Some("unknown_method") => USAGE_MISSING.to_owned(),
                    _ => error.to_string(),
                },
            )
    }

    /// The context reading of `line`, the Session's default branch; `report`
    /// also prints it as `/usage`'s report.
    async fn request_context(&mut self, report: bool, line: &str) {
        if let Some((_, requested_report)) = &mut self.context_request {
            *requested_report |= report;
            return;
        }
        self.context_stale = false;
        match self
            .client
            .send(
                "session.context.usage",
                json!({"sessionId": self.session_id, "branchId": line}),
            )
            .await
        {
            Ok(request) => self.context_request = Some((request, report)),
            Err(error) => {
                self.send(Update::Context(None));
                if report {
                    self.report(error.to_string(), true);
                }
            }
        }
    }

    /// Refreshes the composer's context reading once it is stale; a session
    /// whose usage is unknown shows none.
    async fn poll_context(&mut self) {
        if self.context_stale && !self.session_id.is_empty() && self.context_request.is_none() {
            let line = self.default_branch.clone();
            self.request_context(false, &line).await;
        }
    }

    /// Asks `line` to compact; committed outcomes reach its transcript.
    async fn compact(&mut self, line: &str) {
        let params = json!({"sessionId": self.session_id, "branchId": line});
        match self.client.send("session.context.compact", params).await {
            Ok(request) => self.compact_requests.push(request),
            Err(error) => self.send(Update::Compacted(CompactReply::Failed(error.to_string()))),
        }
    }

    /// A SessionStore title commit, generated or a rename from any client.
    /// The newest revision wins, and a title that is only the `New session`
    /// placeholder is no title. A notice for another Session never reaches
    /// here: the caller checks the shown Session's id.
    fn title_notice(&mut self, data: &serde_json::Value) {
        let Some(revision) = data["titleRevision"].as_u64() else {
            return;
        };
        if revision <= self.title_revision {
            return;
        }
        self.title_revision = revision;
        self.session_title = stored_title(data["title"].as_str()).map(str::to_owned);
        self.publish();
    }

    /// `/rename`: sets the shown Session's manual title, or clears it when
    /// `title` is `None`. The text is a command, never an input.
    async fn rename(&mut self, title: Option<String>) {
        let reply =
            crate::session_command::set_title(&mut self.client, &self.session_id, title.as_deref())
                .await;
        self.send(Update::Renamed(reply));
    }

    /// Home, not a Session: the first message creates one. `/new`, and a
    /// delete of the shown Session, leave to it the same way, so the composer
    /// and the `─ new session ─` boundary are what `/new` leaves.
    fn home(&mut self) {
        if self.session_id.is_empty() {
            return;
        }
        self.parked = self.invalidate_projection();
        self.send(Update::Permissions(Ok(dim_agent_types::PermissionPolicy {
            preset: self
                .options
                .permission
                .unwrap_or(dim_agent_types::PermissionPreset::WorkspaceWrite),
            ..Default::default()
        })));
        self.send(Update::Context(None));
        self.send(Update::Switched {
            session_id: String::new(),
            kind: SwitchKind::New,
            cwd: None,
        });
    }

    /// `session.delete {sessionId}` for one row of `/resume`, without
    /// `requireInactive`: the issue asks the picker to delete a Session a
    /// turn is running in, which the close in the delete settles.
    ///
    /// A `-32603` does not confirm deletion or whether the Session closed:
    /// storage operations can fail before or after close. The picker keeps
    /// the row for reconciliation and leaves an affected current Session
    /// (session-deletion-and-fork §3.5).
    async fn delete_session(&mut self, session_id: &str) -> DeleteOutcome {
        if !self.client.supports(&["session.delete"]) {
            return DeleteOutcome::Unsupported;
        }
        match self
            .client
            .typed::<crate::session_command::Deleted>(
                "session.delete",
                json!({"sessionId": session_id}),
            )
            .await
        {
            Ok(reply) if reply.deleted == Some(false) => DeleteOutcome::NothingRemoved,
            Ok(_) => DeleteOutcome::Deleted,
            Err(error) => {
                let removal_failed = error
                    .downcast_ref::<client::RpcError>()
                    .is_some_and(|rpc| rpc.code == -32603);
                let reason = crate::session_command::reason(&error);
                if removal_failed {
                    DeleteOutcome::ClosedNotRemoved(reason)
                } else {
                    DeleteOutcome::Failed(reason)
                }
            }
        }
    }

    async fn goal(&mut self, request: crate::goal::Request) {
        if self.session_id.is_empty() {
            self.report(format!("Goal: {NO_SESSION}"), true);
            return;
        }
        if !crate::goal::available(&mut self.client, &self.session_id).await {
            self.report(crate::goal::NOT_AVAILABLE.into(), true);
            return;
        }
        let branch = self.focus.clone();
        let result = async {
            let params = json!({"sessionId": self.session_id, "branchId": branch});
            let mut state: dim_agent_types::GoalRead = self
                .client
                .typed("session.goal.get", params.clone())
                .await?;
            let call = crate::goal::call(&request, state.goal.as_ref());
            if let Some(call) = call {
                let mut write = params.clone();
                if let Some(objective) = crate::goal::objective(&request) {
                    write["objective"] = json!(objective);
                }
                self.client.rpc(call.method(), write).await?;
                state = self.client.typed("session.goal.get", params).await?;
            }
            let report = crate::goal::report(&branch, call, state.goal.as_ref()).join("\n");
            if let Some(projection) = transcript_mut(&mut self.projection, &mut self.lines, &branch)
            {
                projection.goal = state.goal;
            }
            self.changed();
            Ok::<_, crate::Error>(report)
        }
        .await;
        match result {
            Ok(report) => self.report(report, false),
            Err(error) => self.report(format!("Goal: {error}"), true),
        }
    }

    /// `/loop`: show the shown line's loops, start one, or stop one, and print
    /// the owner's answer into the transcript. The pattern is `goal` above:
    /// the command calls a session method and the report is one transcript
    /// line.
    async fn loop_command(&mut self, request: crate::loop_view::Request) {
        if self.session_id.is_empty() {
            self.report(format!("Loop: {NO_SESSION}"), true);
            return;
        }
        if !crate::loop_command::available(&mut self.client, &self.session_id).await {
            self.report(crate::loop_view::NOT_AVAILABLE.into(), true);
            return;
        }
        let branch = self.focus.clone();
        let result = async {
            match request {
                crate::loop_view::Request::Show => {
                    let loops =
                        crate::loop_command::list(&mut self.client, &self.session_id, &branch)
                            .await?;
                    Ok::<_, crate::Error>(crate::loop_view::report(&branch, &loops).join("\n"))
                }
                crate::loop_view::Request::Start {
                    prompt,
                    every_seconds,
                } => {
                    let created = self
                        .client
                        .rpc(
                            "session.loop.start",
                            crate::loop_command::start_request(
                                &self.session_id,
                                &branch,
                                &prompt,
                                every_seconds,
                            ),
                        )
                        .await?;
                    let id = created["loop"]["loopId"].as_str().unwrap_or_default();
                    Ok(format!(
                        "Loop {id} started on {branch} ({}).",
                        crate::loop_view::cadence_text(every_seconds)
                    ))
                }
                crate::loop_view::Request::Stop(loop_id) => {
                    let loop_id = match loop_id {
                        Some(id) => id,
                        None => match crate::loop_command::only_live(
                            &mut self.client,
                            &self.session_id,
                            &branch,
                        )
                        .await?
                        {
                            Some(id) => id,
                            None => {
                                return Ok(format!(
                                    "No single live loop on {branch}; name one with /loop stop ID."
                                ));
                            }
                        },
                    };
                    self.client
                        .rpc(
                            "session.loop.stop",
                            json!({"sessionId": self.session_id, "loopId": loop_id}),
                        )
                        .await?;
                    Ok(format!("Loop {loop_id} stopped."))
                }
            }
        }
        .await;
        match result {
            Ok(report) => self.report(report, false),
            Err(error) => self.report(format!("Loop: {error}"), true),
        }
    }

    /// Whether a request made from a snapshot still addresses this
    /// attachment.
    fn attached(&self, session_id: &str, projection_id: u64) -> bool {
        !self.session_id.is_empty()
            && session_id == self.session_id
            && projection_id == self.projection_id
    }

    /// Reports an error as a notice and returns true only on success.
    fn report_failure<T>(&self, result: crate::Result<T>) -> bool {
        match result {
            Ok(_) => true,
            Err(error) => {
                self.send(Update::Notice(error.to_string()));
                false
            }
        }
    }

    /// Opens this process's leaf on its first Session, then registers its
    /// interface. Older daemons use the connection registration.
    async fn register_ui(&mut self) -> crate::Result<()> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Attached {
            ui_id: String,
        }
        if self.ui_identity.is_none() && self.client.offers("ui.leaf.2") {
            let leaf = crate::leaf::Leaf::bind(&self.options)?;
            let params = json!({
                "sessionId": self.session_id,
                "kind": "tui",
                "peer": leaf.peer,
                "address": leaf.address,
            });
            let end = self
                .leaf_end
                .take()
                .ok_or("UI leaf has already been started")?;
            let node = self.ctx.plugin(leaf.node(end));
            (&node).await.map_err(crate::plugin_error)?;
            match self.client.typed("ui.open", params).await {
                Ok(opened) => self.ui_identity = Some(opened),
                Err(error) => {
                    node.dispose().await?;
                    return Err(error);
                }
            }
        }
        if !self.client.offers("ui.attach") {
            return Ok(());
        }
        let mut params = json!({"sessionId": self.session_id, "kind": "tui"});
        if let Some(identity) = &self.ui_identity {
            params["uiId"] = json!(identity.ui_id);
            params["token"] = json!(identity.token);
        }
        match self.client.typed::<Attached>("ui.attach", params).await {
            Ok(attached) => self.ui_id = Some(attached.ui_id),
            Err(error) => self.send(Update::Notice(format!(
                "Interface registration failed: {error}"
            ))),
        }
        Ok(())
    }

    /// Put the browser on this Session's chat while keeping the bootstrap
    /// fragment (the sign-in token) last.
    fn web_url(&self, url: &str) -> String {
        if self.session_id.is_empty() {
            return url.to_owned();
        }
        let (base, fragment) = url.split_once('#').unwrap_or((url, ""));
        let mut scoped = format!("{}/chat/{}", base.trim_end_matches('/'), self.session_id);
        if !fragment.is_empty() {
            scoped.push('#');
            scoped.push_str(fragment);
        }
        scoped
    }

    async fn message(&mut self, message: Message) -> crate::Result<()> {
        let (method, params) = match message {
            Message::Notice { method, params } => (method, params),
            Message::Response { id, result } => {
                if let Some(panel) = self.report_generations.remove(&id) {
                    let reply = result
                        .map_err(Into::into)
                        .and_then(|value| serde_json::from_value(value).map_err(Into::into))
                        .map(crate::report::Reply::Generated);
                    self.report_reply(panel, reply);
                    return Ok(());
                }
                if let Some(read) = self.approval_details.remove(&id) {
                    self.member_approval_reply(read, result).await?;
                    return Ok(());
                }
                self.reply(id, result).await?;
                return Ok(());
            }
        };
        match method.as_str() {
            "management.progress"
                if params["method"] == "shell.run"
                    && self
                        .shell
                        .as_ref()
                        .is_some_and(|(request, _)| params["id"].as_u64() == Some(*request)) =>
            {
                let run_id = self.shell.as_ref().expect("shell run").1.clone();
                let data = &params["data"];
                if let Some(text) = data["text"].as_str() {
                    self.send(Update::ShellProgress {
                        run_id,
                        text: text.to_owned(),
                        skipped: data["skipped"].as_u64().unwrap_or(0),
                    });
                }
            }
            "session.event" | "session.display" => {
                let notice: EventNotice = if method == "session.display" {
                    serde_json::from_value::<DisplayNotice>(params)?.into()
                } else {
                    serde_json::from_value(params)?
                };
                if notice.session_id == self.session_id {
                    // Live only: history pages arrive as `session.history`
                    // replies applied by `apply`, so they spend no read.
                    if notice.branch_id == self.default_branch
                        && matches!(notice.event.content, EventContent::ModelEnd(_))
                    {
                        self.context_stale = true;
                    }
                    let completed = match &notice.event.content {
                        EventContent::TurnEnd(data)
                            if notice.branch_id == self.default_branch
                                && matches!(
                                    data.outcome,
                                    dim_agent_types::TurnOutcome::Completed { .. }
                                ) =>
                        {
                            Some(data.turn_id.clone())
                        }
                        _ => None,
                    };
                    self.task_event(&notice.branch_id, notice.event);
                    if let Some(turn_id) = completed {
                        self.publish();
                        self.send(Update::TurnCompleted { turn_id });
                    }
                } else if notice.branch_id == DEFAULT_BRANCH_ID
                    && self.observes_member(&notice.session_id)
                {
                    self.member_notice(&notice.session_id, member::Notice::Event(notice.event));
                }
            }
            "session.model.progress" => {
                let notice: Progress = serde_json::from_value(params)?;
                if notice.session_id == self.session_id {
                    if let Some(transcript) =
                        transcript_mut(&mut self.projection, &mut self.lines, &notice.branch_id)
                    {
                        transcript.progress(&notice);
                    }
                    self.changed();
                } else if notice.branch_id == DEFAULT_BRANCH_ID
                    && self.observes_member(&notice.session_id)
                {
                    let session_id = notice.session_id.clone();
                    self.member_notice(&session_id, member::Notice::Progress(notice));
                }
            }
            "session.state" => {
                let notice: StateNotice = serde_json::from_value(params)?;
                if notice.session_id == self.session_id {
                    self.discover_branch(&notice.branch_id);
                    self.task_history
                        .get_mut(&notice.branch_id)
                        .expect("discovered branch")
                        .running = Some(notice.running);
                    if let Some(transcript) =
                        transcript_mut(&mut self.projection, &mut self.lines, &notice.branch_id)
                    {
                        transcript.set_running(notice.running);
                    }
                    self.context_stale |=
                        notice.branch_id == self.default_branch && !notice.running;
                    self.changed();
                } else if notice.branch_id == DEFAULT_BRANCH_ID
                    && self.observes_member(&notice.session_id)
                {
                    self.member_notice(&notice.session_id, member::Notice::State(notice.running));
                }
            }
            "approval.request" => {
                let notice: ApprovalNotice = serde_json::from_value(params)?;
                if notice.session_id == self.session_id {
                    let always = self
                        .client
                        .supports(&permission::RULE_METHODS)
                        .then(|| permission::always_command(&notice.call).map(str::to_owned))
                        .flatten();
                    let member = notice.call.name == dim_agent_types::MEMBER_APPROVAL_ALLOW;
                    let branch_id = notice.branch_id.clone();
                    let request_id = notice.request_id.clone();
                    self.projection.add_approval(
                        notice.branch_id,
                        notice.request_id,
                        notice.call.name,
                        notice.call.input,
                        notice.reason,
                        always,
                    );
                    self.changed();
                    if member {
                        self.publish();
                        self.read_member_approval(&branch_id, &request_id, false)
                            .await?;
                    }
                }
            }
            "approval.resolved" | "approval.cancelled" => {
                let notice: CancelNotice = serde_json::from_value(params)?;
                if notice.session_id == self.session_id
                    && self.projection.approvals().iter().any(|approval| {
                        approval.request_id == notice.request_id
                            && approval.branch_id == notice.branch_id
                    })
                {
                    self.projection
                        .remove_approval(&notice.branch_id, &notice.request_id);
                    self.changed();
                }
            }
            "session.notice"
                if params["sessionId"] == self.session_id
                    && params["source"] == "SessionStore"
                    && params["kind"] == "title" =>
            {
                self.title_notice(&params["data"]);
            }
            "session.notice"
                if params["sessionId"] == self.session_id
                    && params["source"]
                        == dim_agent_capabilities::prompt_suggestion::NOTICE_SOURCE =>
            {
                #[derive(Deserialize)]
                #[serde(rename_all = "camelCase")]
                struct Suggestion {
                    turn_id: String,
                    #[serde(default)]
                    text: Option<String>,
                }
                let notice: Suggestion = serde_json::from_value(params["data"].clone())?;
                match notice.text {
                    Some(text) if params["kind"] == "offer" => {
                        self.send(Update::SuggestionOffer {
                            turn_id: notice.turn_id,
                            text,
                        });
                    }
                    _ => self.send(Update::SuggestionCleared {
                        turn_id: notice.turn_id,
                    }),
                }
            }
            "session.notice"
                if params["sessionId"] == self.session_id
                    && params["source"] == "Collaboration"
                    && params["kind"] == "memberApprovalEnded" =>
            {
                let ended: dim_agent_types::MemberApprovalEnded =
                    serde_json::from_value(params["data"].clone())?;
                let requests: Vec<_> = self
                    .projection
                    .approvals()
                    .into_iter()
                    .filter(|approval| {
                        approval.branch_id == ended.branch_id
                            && approval.tool_name == dim_agent_types::MEMBER_APPROVAL_ALLOW
                            && approval.tool_input["approvalId"] == ended.approval_id
                    })
                    .collect();
                for approval in requests {
                    self.projection
                        .remove_approval(&approval.branch_id, &approval.request_id);
                    let _ = self.client.rpc("approval.respond", json!({"sessionId":self.session_id,"branchId":ended.branch_id,"requestId":approval.request_id,"decision":"deny"})).await;
                }
                self.changed();
            }
            "session.mcp.state" => {
                let state: mcp::State = serde_json::from_value(params)?;
                if state.session_id == self.session_id {
                    let required: std::collections::HashSet<_> = state
                        .servers
                        .into_iter()
                        .filter(|server| server.state == "auth_required")
                        .map(|server| server.id)
                        .collect();
                    for id in required.difference(&self.mcp_required) {
                        self.send(Update::Notice(format!(
                            "MCP {id:?}: sign-in required. Run /mcp login {id:?}."
                        )));
                    }
                    self.mcp_required = required;
                }
            }
            "session.closed" => {
                let notice: ClosedNotice = serde_json::from_value(params)?;
                if self.observes_member(&notice.session_id) {
                    self.member_closed(&notice.session_id);
                } else if notice.session_id == self.session_id {
                    return Err("Session closed while attached".into());
                }
            }
            "ui.shutdown" => {
                #[derive(Deserialize)]
                #[serde(rename_all = "camelCase")]
                struct Shutdown {
                    ui_id: String,
                    reason: String,
                }
                let notice: Shutdown = serde_json::from_value(params)?;
                if self.ui_id.as_deref() == Some(notice.ui_id.as_str()) {
                    self.send(Update::Shutdown {
                        reason: notice.reason,
                    });
                }
            }
            _ => {}
        }
        Ok(())
    }

    async fn read_member_approval(
        &mut self,
        branch_id: &str,
        request_id: &str,
        allow: bool,
    ) -> crate::Result<()> {
        if let Some(read) = self.approval_details.values_mut().find(|read| {
            read.projection_id == self.projection_id
                && read.branch_id == branch_id
                && read.request_id == request_id
        }) {
            read.allow |= allow;
            return Ok(());
        }
        let id = self
            .client
            .send(
                dim_agent_types::MEMBER_APPROVAL_GET,
                json!({"sessionId":self.session_id,"branchId":branch_id,"requestId":request_id}),
            )
            .await?;
        self.approval_details.insert(
            id,
            MemberDetailRead {
                projection_id: self.projection_id,
                branch_id: branch_id.into(),
                request_id: request_id.into(),
                allow,
            },
        );
        self.projection.member_approval(branch_id, request_id, None);
        self.changed();
        Ok(())
    }

    async fn member_approval_reply(
        &mut self,
        read: MemberDetailRead,
        result: std::result::Result<serde_json::Value, client::RpcError>,
    ) -> crate::Result<()> {
        if read.projection_id != self.projection_id
            || !self.projection.approvals().iter().any(|approval| {
                approval.branch_id == read.branch_id
                    && approval.request_id == read.request_id
                    && approval.tool_name == dim_agent_types::MEMBER_APPROVAL_ALLOW
            })
        {
            return Ok(());
        }
        let detail = result
            .map_err(crate::member_approval::DetailError::from)
            .and_then(|value| {
                serde_json::from_value::<dim_agent_types::MemberApprovalDetail>(value).map_err(
                    |error| {
                        crate::member_approval::DetailError::Failed(format!(
                            "Invalid member approval detail: {error}"
                        ))
                    },
                )
            });
        match detail {
            Ok(dim_agent_types::MemberApprovalDetail::Pending { descriptor, .. }) => {
                self.projection.member_approval(
                    &read.branch_id,
                    &read.request_id,
                    Some(Ok(*descriptor)),
                );
                if read.allow {
                    let result = self.client.rpc("approval.respond", json!({"sessionId":self.session_id,"branchId":read.branch_id,"requestId":read.request_id,"decision":"allow"})).await;
                    if self.report_failure(result) {
                        self.projection
                            .remove_approval(&read.branch_id, &read.request_id);
                    } else {
                        self.send(Update::ApprovalPending {
                            branch_id: read.branch_id,
                            request_id: read.request_id,
                        });
                    }
                }
            }
            Ok(dim_agent_types::MemberApprovalDetail::Ended) => {
                self.projection
                    .remove_approval(&read.branch_id, &read.request_id);
                let _ = self.client.rpc("approval.respond", json!({"sessionId":self.session_id,"branchId":read.branch_id,"requestId":read.request_id,"decision":"deny"})).await;
                self.send(Update::Notice("Member request ended".into()));
            }
            Err(error) => {
                self.projection.member_approval(
                    &read.branch_id,
                    &read.request_id,
                    Some(Err(error)),
                );
                self.send(Update::ApprovalPending {
                    branch_id: read.branch_id,
                    request_id: read.request_id,
                });
            }
        }
        self.changed();
        Ok(())
    }

    fn report(&self, text: String, warning: bool) {
        self.send(Update::Report { text, warning });
    }

    /// `/mcp [list]`, `/mcp login [--no-browser] ID`, `/mcp callback URL`,
    /// `/mcp logout ID`, `/mcp disconnect ID`, `/mcp enable ID`,
    /// `/mcp disable ID`; every command that names one entry takes
    /// `--source SOURCE`.
    async fn mcp(&mut self, args: &str) -> crate::Result<()> {
        const USAGE: &str = "Usage: /mcp [list|login [--no-browser] ID|callback URL|logout ID|disconnect ID|enable ID|disable ID] [--source SOURCE]";
        let cwd = self
            .session_cwd
            .as_deref()
            .unwrap_or(self.options.workspace().as_str())
            .to_owned();
        let words: Vec<&str> = args.split_whitespace().collect();
        match words.as_slice() {
            [] | ["list"] => {
                let (rows, notes) = mcp::catalog_report(&mut self.client, &cwd).await?;
                let mut lines = rows;
                lines.extend(notes);
                // Without a Session there are no connections to report; the
                // catalog is the whole report, as it is for `mcp list`.
                if !self.session_id.is_empty() {
                    lines.extend(mcp::session_report(&mut self.client, &self.session_id).await?);
                }
                self.report(lines.join("\n"), false);
            }
            ["login", rest @ ..] => {
                let no_browser = rest.contains(&"--no-browser");
                if rest.contains(&"--client-secret") {
                    return Err("A client secret needs hidden input; run dimcode mcp login ID --client-secret in a terminal".into());
                }
                let (source, rest) = crate::mcp::take_source(rest)?;
                let ids: Vec<_> = rest.iter().filter(|word| !word.starts_with("--")).collect();
                let [id] = ids.as_slice() else {
                    return Err(USAGE.into());
                };
                self.mcp_login(id, source, no_browser).await?;
            }
            ["callback", url] => {
                let Some(attempt) = &self.mcp_attempt else {
                    return Err("No MCP sign-in is waiting; run /mcp login ID".into());
                };
                let attempt_id = attempt.attempt_id.clone();
                self.mcp_complete(attempt_id, url.to_string(), None).await;
            }
            ["logout", rest @ ..] => {
                #[derive(Deserialize)]
                struct LoggedOut {
                    revoked: bool,
                }
                let (source, rest) = crate::mcp::take_source(rest)?;
                let [id] = rest.as_slice() else {
                    return Err(USAGE.into());
                };
                let params =
                    crate::management::mcp::target_params(&mut self.client, &cwd, id, source)
                        .await?;
                let result: LoggedOut = self.client.typed("mcp.auth.logout", params).await?;
                self.report(
                    format!("Signed out of {id:?}. Revoked: {}.", result.revoked),
                    false,
                );
            }
            // The ID is the rest of the line, spaces included.
            ["disconnect", rest @ ..] => {
                let (source, rest) = crate::mcp::take_source(rest)?;
                let id = rest.join(" ");
                if id.is_empty() {
                    return Err(USAGE.into());
                }
                let params =
                    crate::management::mcp::target_params(&mut self.client, &cwd, &id, source)
                        .await?;
                self.client.rpc("mcp.disconnect", params).await?;
                self.report(
                    format!("Disconnected {}.", crate::sanitize::safe(&id)),
                    false,
                );
            }
            // The ID is the rest of the line, spaces included.
            ["enable", rest @ ..] | ["disable", rest @ ..] => {
                let (source, rest) = crate::mcp::take_source(rest)?;
                let id = rest.join(" ");
                if id.is_empty() {
                    return Err(USAGE.into());
                }
                let state = crate::management::mcp::set_enabled(
                    &mut self.client,
                    &cwd,
                    &id,
                    source,
                    words[0] == "enable",
                )
                .await?;
                let mut lines = vec![state.sentence()];
                lines.extend(state.notes);
                self.report(lines.join("\n"), false);
            }
            _ => return Err(USAGE.into()),
        }
        Ok(())
    }

    async fn recording(&mut self, args: &str) -> crate::Result<String> {
        let enabled = match args.split_whitespace().next() {
            None => None,
            Some("on") => Some(true),
            Some("off") => Some(false),
            Some(_) => return Err("Usage: /settings recording [on|off]".into()),
        };
        let state = match enabled {
            Some(enabled) => crate::recording::set(&mut self.client, enabled).await?,
            None => crate::recording::read(&mut self.client).await?,
        };
        Ok(crate::recording::report(&state, &self.options))
    }

    async fn mcp_login(
        &mut self,
        id: &str,
        source: Option<&str>,
        no_browser: bool,
    ) -> crate::Result<()> {
        self.end_mcp_attempt();
        let cwd = self
            .session_cwd
            .as_deref()
            .unwrap_or(self.options.workspace().as_str());
        let params =
            crate::management::mcp::target_params(&mut self.client, cwd, id, source).await?;
        let started: mcp::Started = self.client.typed("mcp.auth.start", params).await?;
        let issuer = url::Url::parse(&started.issuer)?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_millis() as u64;
        let deadline = tokio::time::Instant::now()
            + Duration::from_millis(started.expires_at.saturating_sub(now));
        let local_browser = !no_browser
            && std::env::var_os("SSH_CONNECTION").is_none()
            && std::env::var_os("SSH_TTY").is_none();
        let capture = if local_browser {
            mcp::Capture::bind(&started.redirect_uri).await
        } else {
            None
        };
        let mut lines = vec![
            format!("MCP server: {id:?}"),
            format!("Resource: {}", crate::sanitize::safe(&started.resource)),
            format!(
                "Authorization server: {}",
                crate::sanitize::safe(
                    issuer
                        .host_str()
                        .ok_or("Authorization server has no host")?
                )
            ),
            format!(
                "Open: {}",
                crate::sanitize::safe(&started.authorization_url)
            ),
        ];
        let listener = capture.map(|capture| {
            let callbacks = self.mcp_callbacks.0.clone();
            let attempt_id = started.attempt_id.clone();
            tokio::spawn(async move {
                let callback = tokio::select! {
                    callback = capture.receive() => callback,
                    _ = tokio::time::sleep_until(deadline) => Err("Sign-in expired".into()),
                };
                let _ = callbacks.send(McpCallback {
                    attempt_id,
                    callback,
                });
            })
        });
        lines.push(if listener.is_some() {
            "Waiting for the browser callback; you can also paste it with /mcp callback URL.".into()
        } else {
            "Copy the full URL from the browser address bar and paste it with /mcp callback URL."
                .into()
        });
        if local_browser {
            crate::browser::open(&started.authorization_url);
        }
        self.report(lines.join("\n"), false);
        self.mcp_attempt = Some(McpAttempt {
            server_id: id.to_owned(),
            attempt_id: started.attempt_id,
            deadline,
            listener,
        });
        Ok(())
    }

    /// Forgets the sign-in, closing its listener.
    fn end_mcp_attempt(&mut self) {
        if let Some(listener) = self.mcp_attempt.take().and_then(|attempt| attempt.listener) {
            listener.abort();
        }
    }

    async fn mcp_callback(&mut self, callback: McpCallback) {
        if self
            .mcp_attempt
            .as_ref()
            .is_none_or(|attempt| attempt.attempt_id != callback.attempt_id)
        {
            return;
        }
        match callback.callback {
            Ok((url, stream)) => {
                self.mcp_complete(callback.attempt_id, url, Some(stream))
                    .await
            }
            Err(error) => {
                if let Some(attempt) = self.mcp_attempt.as_mut() {
                    attempt.listener = None;
                }
                self.report(
                    format!("{error}; paste the callback with /mcp callback URL, or run /mcp login ID again."),
                    true,
                );
            }
        }
    }

    async fn mcp_complete(
        &mut self,
        attempt_id: String,
        url: String,
        browser: Option<tokio::net::TcpStream>,
    ) {
        let attempt = self.mcp_attempt.as_ref().expect("MCP attempt");
        if tokio::time::Instant::now() >= attempt.deadline {
            let id = attempt.server_id.clone();
            self.end_mcp_attempt();
            self.report(
                format!("Sign-in expired. Run /mcp login {id:?} to start again."),
                true,
            );
            return;
        }
        let result = self
            .client
            .rpc(
                "mcp.auth.complete",
                json!({"attemptId": attempt_id, "callbackUrl": url}),
            )
            .await;
        if let Some(stream) = browser {
            answer_browser(stream, result.is_ok()).await;
        }
        let id = self
            .mcp_attempt
            .as_ref()
            .map(|attempt| attempt.server_id.clone())
            .unwrap_or_default();
        match result {
            Ok(_) => {
                self.end_mcp_attempt();
                self.report(format!("Signed in to {id:?}."), false);
            }
            Err(error)
                if error.downcast_ref::<client::RpcError>().and_then(app_error)
                    == Some("mcp_auth_callback_invalid") =>
            {
                self.report(
                    "Callback did not match this attempt; paste it again with /mcp callback URL."
                        .into(),
                    true,
                );
            }
            Err(error) => {
                self.end_mcp_attempt();
                self.report(mcp::error(error).to_string(), true);
            }
        }
    }

    /// The `/plugins` list: every name `plugin.list` returns.
    async fn plugins(&mut self) -> Result<Vec<PluginItem>, String> {
        if !self.client.supports(&["plugin.list"]) {
            return Err("This daemon cannot manage plugins".into());
        }
        self.client
            .typed("plugin.list", json!({}))
            .await
            .map_err(|error| error.to_string())
    }

    /// The `/runtime` rows: what `runtime.list` reports.
    async fn runtimes(&mut self) -> Result<Vec<RuntimeItem>, RuntimesError> {
        if !self.client.supports(&["runtime.list"]) {
            return Err(RuntimesError::Unsupported {
                version: self.client.server_version().to_owned(),
            });
        }
        self.client
            .typed("runtime.list", json!({}))
            .await
            .map_err(|error| RuntimesError::Failed(error.to_string()))
    }

    /// A `/runtime` change: the calls it makes, in order, are the change.
    async fn runtime_change(&mut self, action: &RuntimeAction) -> Result<(), String> {
        for (method, params) in runtime_calls(action) {
            self.client
                .rpc(method, params)
                .await
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    }

    async fn providers(&mut self) -> Result<Vec<ProviderEntry>, String> {
        let current = self.model.as_ref().map(|model| model.provider_id.clone());
        let entries = self
            .provider_summaries()
            .await
            .map_err(|error| error.to_string())?;
        entries
            .into_iter()
            .filter_map(|entry| {
                let chatgpt = entry
                    .account
                    .as_ref()
                    .is_some_and(|account| account.namespace == crate::login::CHATGPT_NAMESPACE);
                let ready = entry
                    .account
                    .as_ref()
                    .and_then(|account| account.state.as_ref())
                    .is_some_and(|state| state.account.is_some());
                let account = entry.account.map(|account| AccountDescriptor {
                    namespace: account.namespace,
                    login_methods: account.login_methods,
                });
                let result = match entry.connection {
                    Some(connection) => {
                        let Some(enabled) = entry.enabled else {
                            return Some(Err("Static Provider enable flag is missing".into()));
                        };
                        if entry.version.is_none() {
                            return Some(Err("Static Provider record version is missing".into()));
                        }
                        let status = if !enabled {
                            "disabled"
                        } else if connection.credential_configured
                            || connection.auth["type"] == "anonymous"
                        {
                            "connected"
                        } else {
                            "disconnected"
                        };
                        (
                            status,
                            enabled,
                            connection.driver_kind,
                            Some(connection.base_url).filter(|url| !url.is_empty()),
                            connection.credential_configured.then(|| "••••••••".into()),
                            connection.glued_prefix,
                        )
                    }
                    None if chatgpt => (
                        if ready { "connected" } else { "disconnected" },
                        true,
                        entry.provider_id.clone(),
                        None,
                        None,
                        None,
                    ),
                    None => return None,
                };
                let (status, enabled, driver, base_url, api_key_preview, glued_prefix) = result;
                Some(Ok(ProviderEntry {
                    is_current: current.as_deref() == Some(entry.provider_id.as_str()),
                    status: status.into(),
                    enabled,
                    custom: entry.source == "custom",
                    base_url,
                    api_key_preview,
                    glued_prefix,
                    id: entry.provider_id,
                    version: entry.version,
                    name: entry.display_name,
                    driver,
                    account,
                }))
            })
            .collect()
    }

    async fn provider_summaries(&mut self) -> crate::Result<Vec<ManagementInfo>> {
        if !self.client.supports(&PROVIDER_METHODS) {
            return Err("This daemon cannot manage providers".into());
        }
        let mut providers = Vec::new();
        let mut after: Option<String> = None;
        loop {
            let mut params = json!({});
            if let Some(id) = after {
                params["after"] = json!(id);
            }
            let page: ProviderManagementPage = self
                .client
                .typed("provider.management.list", params)
                .await?;
            for error in page.errors {
                self.send(Update::Notice(format!(
                    "{}: {}",
                    error.provider_id, error.message
                )));
            }
            providers.extend(page.providers);
            after = page.next;
            if after.is_none() {
                break;
            }
        }
        Ok(providers)
    }

    /// Probe a new connection, then create once using the form's retained UUID.
    async fn create_provider(
        &mut self,
        provider_id: String,
        name: String,
        base_url: String,
        api_key: String,
    ) -> Result<String, String> {
        #[derive(Deserialize)]
        struct Probed {
            models: Vec<serde_json::Value>,
        }
        let result: crate::Result<String> = async {
            if let Some(existing) = self
                .provider_summaries()
                .await?
                .into_iter()
                .find(|provider| provider.provider_id == provider_id)
            {
                return Err(format!(
                    "Provider {} already exists; review the retained draft and press Apply again",
                    existing.display_name
                )
                .into());
            }
            let connection = json!({
                "driverKind": "openai-compatible",
                "auth": {"type": "bearer"},
                "baseUrl": base_url,
                "credential": {"action": "set", "value": api_key},
            });
            let probed: Probed = self
                .client
                .typed(
                    "provider.probe",
                    json!({"providerId": provider_id, "connection": connection}),
                )
                .await?;
            self.client
                .rpc(
                    "provider.create",
                    json!({
                        "providerId": provider_id,
                        "displayName": name,
                        "connection": connection,
                        "models": probed.models,
                        "enabled": true,
                    }),
                )
                .await?;
            Ok(format!("Added {name}"))
        }
        .await;
        result.map_err(|error| error.to_string())
    }

    /// The form's original version guards only the fields the user changed.
    async fn provider_action(&mut self, action: ProviderAction) -> Result<String, String> {
        if let ProviderAction::Create {
            provider_id,
            name,
            base_url,
            api_key,
        } = action
        {
            return self
                .create_provider(provider_id, name, base_url, api_key)
                .await;
        }
        let (provider_id, expected_version, patch, probe, notice) = match action {
            ProviderAction::Configure {
                provider_id,
                expected_version,
                api_key,
                base_url,
                enabled,
                glued_prefix,
            } => {
                let probe = api_key.is_some() || base_url.is_some();
                let mut patch = json!({});
                if let Some(key) = api_key {
                    patch["credential"] = json!({"action":"set","value":key});
                }
                if let Some(url) = base_url {
                    patch["baseUrl"] = json!(url);
                }
                if let Some(enabled) = enabled {
                    patch["enabled"] = json!(enabled);
                }
                if let Some(prefix) = glued_prefix {
                    // The patch's own "clear it" is a null, and its "leave it
                    // alone" is the omitted field.
                    patch["gluedPrefix"] = match prefix {
                        Some(prefix) => json!(prefix),
                        None => json!(null),
                    };
                }
                let notice = if probe {
                    "Connected"
                } else if enabled == Some(false) {
                    "Disabled"
                } else {
                    "Enabled"
                };
                (provider_id, expected_version, patch, probe, notice)
            }
            ProviderAction::Disconnect {
                provider_id,
                expected_version,
            } => (
                provider_id,
                expected_version,
                json!({"credential":{"action":"clear"},"enabled":false}),
                false,
                "Disconnected",
            ),
            ProviderAction::Create { .. } => unreachable!("handled above"),
        };
        let result: crate::Result<String> = async {
            let mut listing_unsupported = false;
            if probe && let Err(error) = self.client.rpc("provider.probe", json!({"providerId":provider_id,"connection":patch})).await {
                if client::app_error(&error) != Some("provider_unsupported") {
                    return Err(error);
                }
                listing_unsupported = true;
            }
            let updated: ManagementUpdateSummary = self.client.typed("provider.update", json!({"providerId":provider_id,"expectedVersion":expected_version,"patch":patch,"reply":"summary"})).await?;
            let mut message = format!("{notice} {}", updated.info.display_name);
            match updated.models_listed {
                Some(ModelsListed::Listed) => message.push_str("; listed its models"),
                Some(ModelsListed::Unsupported) => listing_unsupported = true,
                Some(ModelsListed::Bundled | ModelsListed::Disabled) | None => {}
            }
            if listing_unsupported {
                message.push_str("; saved without testing the key because model listing is unsupported");
            }
            Ok(message)
        }.await;
        result.map_err(|error| error.to_string())
    }

    async fn send_models(
        &mut self,
        query: String,
        after: Option<dim_agent_types::ModelId>,
        batches: usize,
    ) {
        let result = async {
            let mut pages = Vec::new();
            let mut cursor = after.clone();
            for _ in 0..batches {
                let page = self.models(query.clone(), cursor).await?;
                cursor = page.next.clone();
                pages.push(page);
                if cursor.is_none() {
                    break;
                }
            }
            Ok::<_, crate::Error>(pages)
        }
        .await
        .map_err(|error| error.to_string());
        self.send(Update::Models {
            query,
            after,
            result,
        });
    }

    async fn models(
        &mut self,
        query: String,
        after: Option<dim_agent_types::ModelId>,
    ) -> crate::Result<ModelList> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Row {
            provider_id: String,
            provider_name: String,
            model: crate::wire::CatalogModel,
            version: u32,
            disabled: bool,
            preferences: serde_json::Value,
        }
        #[derive(Deserialize)]
        struct Page {
            models: Vec<Row>,
            next: Option<dim_agent_types::ModelId>,
            empty: Vec<EmptyProvider>,
        }
        let page: Page = self
            .client
            .typed(
                "provider.models.query",
                json!({"query":query,"after":after}),
            )
            .await?;
        let mut empty = page.empty;
        let mut limits = std::collections::BTreeMap::<String, NeedsLimitsProvider>::new();
        let mut unavailable = std::collections::BTreeMap::<String, (String, usize, usize)>::new();
        let models = page
            .models
            .into_iter()
            .filter_map(|row| {
                if row.disabled || row.model.unavailable_reason.is_some() {
                    if crate::limits_undeclared(
                        &row.model.missing_limits,
                        row.model.unavailable_reason.as_deref(),
                        row.disabled,
                    ) {
                        let candidates =
                            match model_outcome(row.model.metadata.as_ref(), &row.provider_id) {
                                ModelOutcome::Ambiguous { candidates } => candidates,
                                _ => Vec::new(),
                            };
                        let provider =
                            limits
                                .entry(row.provider_id.clone())
                                .or_insert(NeedsLimitsProvider {
                                    provider_id: row.provider_id,
                                    provider_name: row.provider_name,
                                    count: 0,
                                    candidates: Vec::new(),
                                });
                        provider.count += 1;
                        for candidate in candidates {
                            if !provider.candidates.contains(&candidate) {
                                provider.candidates.push(candidate);
                            }
                        }
                    } else {
                        let counts = unavailable.entry(row.provider_id.clone()).or_insert((
                            row.provider_name.clone(),
                            0,
                            0,
                        ));
                        if row.model.unavailable_reason.is_some() {
                            counts.1 += 1;
                        } else {
                            counts.2 += 1;
                        }
                    }
                    return None;
                }
                let model = row.model;
                let (efforts, default_effort) = effort_choices(
                    &model.capabilities,
                    model.metadata.as_ref().and_then(|m| m.reasoning.as_ref()),
                );
                let saved_effort = row
                    .preferences
                    .get("reasoningEffort")
                    .and_then(Value::as_str)
                    .filter(|effort| efforts.iter().any(|option| option == *effort))
                    .map(str::to_owned);
                let outcome = model_outcome(model.metadata.as_ref(), &row.provider_id);
                Some(ModelItem {
                    provider_id: row.provider_id,
                    provider_name: row.provider_name,
                    provider_version: row.version,
                    model_id: model.id,
                    display_name: model.name,
                    zdr: model.zdr,
                    efforts,
                    default_effort,
                    saved_effort,
                    outcome,
                })
            })
            .collect();
        for (provider_id, (provider_name, unavailable, disabled)) in unavailable {
            if unavailable + disabled == 0 {
                continue;
            }
            let reason = match (unavailable, disabled) {
                (0, count) => EmptyReason::Disabled { count },
                (count, 0) => EmptyReason::Unavailable { count },
                _ => EmptyReason::Mixed {
                    unavailable,
                    disabled,
                },
            };
            empty.push(EmptyProvider {
                provider_id,
                provider_name,
                reason,
            });
        }
        Ok(ModelList {
            query,
            models,
            next: page.next,
            manage: self.client.supports(&["provider.management"]),
            current: self
                .model
                .as_ref()
                .map(|m| (m.provider_id.clone(), m.model_id.clone())),
            empty,
            limits: limits.into_values().collect(),
        })
    }

    /// Reads the `prompts` source, replaces or removes one field, and writes
    /// the whole document with the revision it read.
    async fn write_prompt(
        &mut self,
        field: PromptField,
        value: Option<&str>,
        revision: &str,
    ) -> Result<PromptSetting, String> {
        if !self.client.supports(&["settings.read", "settings.update"]) {
            return Err("This daemon does not provide prompt overrides".into());
        }
        let snapshot: PromptSetting = self
            .client
            .typed(
                "settings.read",
                json!({"source": "prompts", "scope": "user"}),
            )
            .await
            .map_err(|error| error.to_string())?;
        let mut document = snapshot.document.as_object().cloned().unwrap_or_default();
        match value {
            Some(text) => {
                document.insert(
                    field.key().into(),
                    serde_json::Value::String(text.to_owned()),
                );
            }
            None => {
                document.remove(field.key());
            }
        }
        self.client
            .typed(
                "settings.update",
                json!({
                    "source": "prompts",
                    "scope": "user",
                    "expectedRevision": revision,
                    "document": document,
                }),
            )
            .await
            .map_err(|error| error.to_string())
    }

    /// Reports the subagent model policy, only to a daemon that manages it.
    async fn read_model_policy(&mut self, saved: bool) {
        #[derive(Deserialize)]
        struct Read {
            revision: String,
            policy: SubagentModelPolicy,
        }
        if !self.client.supports(&MODEL_POLICY_METHODS) {
            return;
        }
        let update = match self
            .client
            .typed::<Read>("collaboration.model_policy", json!({}))
            .await
        {
            Ok(read) => ModelPolicyUpdate::Read {
                revision: read.revision,
                policy: read.policy,
                saved,
            },
            Err(error) => ModelPolicyUpdate::Failed(error.to_string()),
        };
        self.send(Update::ModelPolicy(update));
    }

    /// Saves `effort` when it differs from the stored reasoning effort.
    /// A daemon without management keeps the choice on this adoption only.
    async fn save_reasoning_effort(
        &mut self,
        provider_id: &str,
        model_id: &str,
        effort: &str,
    ) -> crate::Result<()> {
        if !self.client.offers("provider.management") {
            return Ok(());
        }
        let snapshot: ProviderManagement = self
            .client
            .typed(
                "provider.management",
                json!({"providerId": provider_id, "modelId": model_id}),
            )
            .await?;
        let provider = snapshot
            .providers
            .into_iter()
            .next()
            .ok_or("Provider management entry is missing")?;
        let saved = provider
            .preferences
            .get(model_id)
            .and_then(|value| value.get("reasoningEffort"))
            .and_then(|value| value.as_str());
        if saved == Some(effort) {
            return Ok(());
        }
        let version = provider
            .info
            .version
            .ok_or("Provider catalog must be refreshed before editing preferences")?;
        self.client
            .rpc(
                "provider.preferences.update",
                json!({
                    "providerId": provider_id,
                    "modelId": model_id,
                    "expectedVersion": version,
                    "patch": {"reasoningEffort": {"action": "set", "value": effort}}
                }),
            )
            .await?;
        Ok(())
    }

    /// `/effort`, `/reasoning` and `/fast` at Home: the configuration
    /// commands act on the model the footer shows, which is the one the first
    /// message's Session adopts. A value is held as that model's pending
    /// `modelPreferences` member for `start_session`; it sends no RPC and
    /// writes no default. Bare `/agent` lists profiles, which names no
    /// Session, so it keeps the ordinary path.
    async fn configure_home(&mut self, text: &str) {
        let command = text.split_whitespace().next().unwrap_or_default();
        let bare_effort = matches!(text, "/effort" | "/reasoning");
        if command == "/agent" {
            match configure::run(&mut self.client, "", ROOT_BRANCH_ID, None, text).await {
                Ok(report) => self.report(report.lines.join("\n"), false),
                Err(error) => self.report(error.to_string(), true),
            }
            return;
        }
        let shown = self.model.clone().or_else(|| self.preview.clone());
        let Some(shown) = shown else {
            // Home shows no model, so the dialog says what to do instead of
            // waiting for choices that cannot come.
            if bare_effort {
                self.send(Update::EffortChoices(Err(configure::NO_MODEL.into())));
            } else {
                self.report(configure::NO_MODEL.to_owned(), true);
            }
            return;
        };
        let result = configure::preference(&mut self.client, &shown, text).await;
        if bare_effort {
            self.send(Update::EffortChoices(
                result
                    .map(|preference| match preference {
                        configure::Preference::List {
                            effort: Some(effort),
                            ..
                        } => effort,
                        configure::Preference::List { effort: None, .. }
                        | configure::Preference::Apply { .. } => {
                            unreachable!("bare /effort lists choices")
                        }
                    })
                    .map_err(|error| error.to_string()),
            ));
            return;
        }
        let preferences = match result {
            Ok(configure::Preference::List { line, .. }) => return self.report(line, false),
            Ok(configure::Preference::Apply { preferences, value }) => {
                // `/new` after a Session inherits that Session's model
                // through `modelFromSession`, which a creation's preferences
                // cannot accompany: the choice would silently never arrive.
                if self.model_session.is_some()
                    && self.client.offers("session.create.modelFromSession")
                {
                    self.report(
                        "The next Session inherits its model; run the command there.".into(),
                        true,
                    );
                    return;
                }
                self.report(
                    format!(
                        "{} for the first Session: {value}",
                        if command == "/fast" {
                            "Service tier"
                        } else {
                            "Reasoning effort"
                        }
                    ),
                    false,
                );
                preferences
            }
            Err(error) => return self.report(error.to_string(), true),
        };
        // The other member keeps what the shown model already carries: the
        // saved value the preview reported, else the choice Home made before.
        let held = self
            .pending
            .as_ref()
            .filter(|(pending, _)| {
                pending.provider_id == shown.provider_id && pending.model_id == shown.model_id
            })
            .map(|(_, held)| held);
        let saved = shown.model_preferences.as_ref();
        let preferences = ModelPreferences {
            reasoning_effort: preferences.reasoning_effort.or_else(|| {
                held.and_then(|held| held.reasoning_effort.clone())
                    .or_else(|| saved.and_then(|saved| saved.reasoning_effort.clone()))
            }),
            service_tier: preferences.service_tier.or_else(|| {
                held.and_then(|held| held.service_tier.clone())
                    .or_else(|| saved.and_then(|saved| saved.service_tier.clone()))
            }),
        };
        self.pending = Some((
            ModelId {
                provider_id: shown.provider_id.clone(),
                model_id: shown.model_id.clone(),
            },
            preferences.clone(),
        ));
        let configuration = Configuration {
            model_preferences: Some(preferences),
            ..shown
        };
        self.preview = Some(configuration.clone());
        // Home's shown model may be the one `/models` picked, which the first
        // creation reads; its label draws the pending choice too.
        if self.model.is_some() {
            self.model = Some(configuration.clone());
        }
        self.send(Update::Ready(configuration));
    }

    /// Switches the session's model, or at home the one the first message
    /// starts a session on, then makes it the default for new ones.
    /// A cycled `reasoning_effort` is saved first, so the server reads it when
    /// it adopts the model. `label` is the picker row's text, which the
    /// default's confirmation names back.
    async fn select_model(
        &mut self,
        provider_id: String,
        model_id: String,
        reasoning_effort: Option<String>,
        label: String,
    ) -> crate::Result<()> {
        #[derive(Deserialize)]
        struct Updated {
            model: ModelId,
        }
        if let Some(effort) = &reasoning_effort {
            self.save_reasoning_effort(&provider_id, &model_id, effort)
                .await?;
        }
        let model = if self.session_id.is_empty() {
            self.model_session = None;
            // `/models` at Home may have saved an effort for the model it
            // picked; that value is the new model's saved one, and the choice
            // Home held belonged to the model it replaced.
            self.pending = None;
            let model = ModelId {
                provider_id,
                model_id,
            };
            let features = self.model_features(&model).await;
            let configuration = Configuration {
                catalog_provider_id: None,
                provider_id: model.provider_id.clone(),
                model_id: model.model_id.clone(),
                capability_snapshot: None,
                model_preferences: None,
            };
            self.model = Some(configuration.clone());
            self.preview = Some(configuration.clone());
            self.send(Update::Ready(configuration));
            self.send(features);
            model
        } else {
            let target = (provider_id.clone(), model_id.clone());
            let params = json!({"sessionId":self.session_id,"branchId":self.focus,"providerId":provider_id,"modelId":model_id});
            let updated: Updated = match self.client.typed("session.model.update", params).await {
                Ok(updated) => updated,
                Err(error) => {
                    return Err(crate::model_adoption_error(
                        &mut self.client,
                        Some((target.0.as_str(), target.1.as_str())),
                        error,
                    )
                    .await);
                }
            };
            let features = self.model_features(&updated.model).await;
            // Re-picking the model the label already shows keeps its
            // preferences: an `unchanged` outcome publishes no fact, and when
            // the model changed the adopted fact draws the new ones.
            let model_preferences = self
                .model
                .as_ref()
                .filter(|shown| {
                    shown.provider_id == updated.model.provider_id
                        && shown.model_id == updated.model.model_id
                })
                .and_then(|shown| shown.model_preferences.clone());
            let configuration = Configuration {
                catalog_provider_id: None,
                provider_id: updated.model.provider_id.clone(),
                model_id: updated.model.model_id.clone(),
                capability_snapshot: self
                    .model
                    .as_ref()
                    .filter(|shown| {
                        shown.provider_id == updated.model.provider_id
                            && shown.model_id == updated.model.model_id
                    })
                    .and_then(|shown| shown.capability_snapshot.clone()),
                model_preferences,
            };
            self.model = Some(configuration.clone());
            self.send(Update::Ready(configuration));
            self.send(features);
            self.context_stale = true;
            updated.model
        };
        if let Err(error) = self.make_default(&model, &label).await {
            self.send(Update::Notice(format!(
                "The model was not saved as the default: {error}"
            )));
        }
        Ok(())
    }

    /// Writes `model` as the default the next Session in the launch workspace
    /// reads: the workspace's own row when the folder has one, else `global`.
    /// Each scope is read for the version `provider.select` replaces, and the
    /// interface names the scope it wrote.
    async fn make_default(&mut self, model: &ModelId, label: &str) -> crate::Result<()> {
        let workspace = self.options.workspace().as_str().to_owned();
        let folder_request = self
            .client
            .send("provider.selection", json!({"cwd": workspace}))
            .await?;
        let global_request = self.client.send("provider.selection", json!({})).await?;
        let folder = self.client.response(folder_request).await;
        let global = self.client.response(global_request).await;
        let folder = selection_version(folder?)?;
        let global = selection_version(global?)?;
        let (scope, expected) = match folder {
            Some(version) => (Some(workspace), Some(version)),
            None => (None, global),
        };
        let mut params = json!({"providerId": model.provider_id, "modelId": model.model_id});
        if let Some(cwd) = &scope {
            params["cwd"] = json!(cwd);
        }
        if let Some(expected) = expected {
            params["expectedVersion"] = json!(expected);
        }
        self.client.rpc("provider.select", params).await?;
        self.send(Update::ModelDefault {
            folder: scope,
            model: label.to_owned(),
        });
        Ok(())
    }

    /// The signed-in account of a managed provider, or none when signed out.
    async fn account(&mut self, provider_id: &str) -> crate::Result<Option<Account>> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Provider {
            provider_id: String,
        }
        #[derive(Deserialize)]
        struct Snapshot {
            account: Option<Identity>,
        }
        #[derive(Deserialize)]
        struct Identity {
            issuer: String,
            subject: String,
            email: Option<String>,
            name: Option<String>,
        }
        let providers: Vec<Provider> = match self.client.typed("account.providers", json!({})).await
        {
            Err(error)
                if error.downcast_ref::<client::RpcError>().and_then(app_error)
                    == Some("unknown_method") =>
            {
                return Err(PLUGIN_MISSING.into());
            }
            result => result?,
        };
        if !providers
            .iter()
            .any(|provider| provider.provider_id == provider_id)
        {
            return Err(PLUGIN_MISSING.into());
        }
        let snapshot: Snapshot = self
            .client
            .typed("account.status", json!({"providerId": provider_id}))
            .await?;
        Ok(snapshot.account.map(|account| Account {
            issuer: account.issuer,
            subject: crate::sanitize::safe(&account.subject),
            email: account.email.as_deref().map(crate::sanitize::safe),
            name: account.name.as_deref().map(crate::sanitize::safe),
        }))
    }

    async fn chatgpt_profiles(&mut self) {
        let profiles = self
            .client
            .typed("account.chatgpt.profiles", json!({}))
            .await
            .map_err(|error| error.to_string());
        self.send(Update::ChatGptProfiles(profiles));
    }

    fn login_event(&self, tag: u64, event: OauthEvent) {
        self.send(Update::Oauth {
            attempt: tag,
            event,
        });
    }

    /// Begins a browser or device sign-in, replacing one still pending.
    async fn login_start(&mut self, start: OauthStart) {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Browser {
            attempt_id: String,
            authorization_url: String,
            expires_at: u64,
        }
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum BrowserStep {
            Browser(Browser),
            Migrated { migrated: bool },
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Device {
            attempt_id: String,
            verification_uri: String,
            verification_uri_complete: Option<String>,
            user_code: String,
        }
        self.login_cancel().await;
        let method = if start.device {
            crate::login::device_begin(&start.namespace)
        } else {
            crate::login::browser_begin(&start.namespace)
        };
        if !self.client.supports(&[&method]) {
            self.login_event(
                start.attempt,
                OauthEvent::Done(Err(Some(format!("This daemon does not offer {method}")))),
            );
            return;
        }
        let managed = start.namespace == ACCOUNT_NAMESPACE;
        let mut params = crate::login::target(&start.namespace, &start.provider_id);
        if managed {
            params["directory"] = json!(crate::login::account_directory());
            if let Some(email) = &start.email {
                params["email"] = json!(email);
            }
        }
        let mut listener = None;
        let attempt_id = if start.device {
            match self.client.typed::<Device>(&method, params).await {
                Ok(device) => {
                    self.login_event(
                        start.attempt,
                        OauthEvent::Device {
                            verification_uri: crate::sanitize::safe(&device.verification_uri),
                            verification_uri_complete: device
                                .verification_uri_complete
                                .map(|uri| crate::sanitize::safe(&uri)),
                            user_code: crate::sanitize::safe(&device.user_code),
                        },
                    );
                    Ok(device.attempt_id)
                }
                Err(error) => Err(error),
            }
        } else {
            let chatgpt = start.namespace == crate::login::CHATGPT_NAMESPACE;
            let local = std::env::var_os("SSH_CONNECTION").is_none()
                && std::env::var_os("SSH_TTY").is_none();
            let capture = if chatgpt && local {
                crate::mcp::Capture::bind_ipv4("/auth/callback").await.ok()
            } else {
                None
            };
            if chatgpt {
                let Some(capture) = &capture else {
                    self.login_event(
                        start.attempt,
                        OauthEvent::Done(Err(Some(
                            "Continue with ChatGPT from a terminal on this computer.".into(),
                        ))),
                    );
                    return;
                };
                params["redirectUri"] = json!(capture.redirect_uri());
                if start.migrate_legacy {
                    params["migrateLegacy"] = json!(true);
                }
                if let Some(profile) = &start.profile_id {
                    params["profileId"] = json!(profile);
                }
            } else if managed {
                params["redirectUri"] = json!(self.options.redirect_uri);
            }
            match self.client.typed::<BrowserStep>(&method, params).await {
                Ok(BrowserStep::Migrated { migrated: true }) if start.migrate_legacy => {
                    self.login_event(start.attempt, OauthEvent::Exchanging);
                    let providers = self.providers().await;
                    self.send(Update::Providers(providers));
                    self.refresh_account_models(&start.provider_id, start.attempt, true)
                        .await;
                    return;
                }
                Ok(BrowserStep::Migrated { .. }) => {
                    self.login_event(
                        start.attempt,
                        OauthEvent::Done(Err(Some("Unexpected ChatGPT switch reply".into()))),
                    );
                    return;
                }
                Ok(BrowserStep::Browser(browser)) => {
                    let capture = if local && managed {
                        mcp::Capture::bind(&self.options.redirect_uri).await
                    } else {
                        capture
                    };
                    listener = capture.map(|capture| {
                        let now = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map_or(0, |elapsed| elapsed.as_millis() as u64);
                        let deadline = tokio::time::Instant::now()
                            + Duration::from_millis(browser.expires_at.saturating_sub(now));
                        let callbacks = self.login_callbacks.0.clone();
                        let attempt_id = browser.attempt_id.clone();
                        tokio::spawn(async move {
                            let callback = tokio::select! {
                                callback = capture.receive() => callback,
                                _ = tokio::time::sleep_until(deadline) => Err("Sign-in expired".into()),
                            };
                            let _ = callbacks.send(McpCallback {
                                attempt_id,
                                callback,
                            });
                        })
                    });
                    let opened = local && crate::browser::open(&browser.authorization_url);
                    self.login_event(
                        start.attempt,
                        OauthEvent::Url {
                            url: crate::sanitize::safe(&browser.authorization_url),
                            opened,
                        },
                    );
                    Ok(browser.attempt_id)
                }
                Err(error) => Err(error),
            }
        };
        match attempt_id {
            Ok(attempt_id) => {
                self.login = Some(Login {
                    tag: start.attempt,
                    migrate_legacy: start.migrate_legacy,
                    provider_id: start.provider_id,
                    namespace: start.namespace,
                    attempt_id,
                    poll_at: start
                        .device
                        .then(|| tokio::time::Instant::now() + LOGIN_POLL),
                    listener,
                });
            }
            Err(error) => self.login_event(
                start.attempt,
                OauthEvent::Done(Err(Some(error.to_string()))),
            ),
        }
    }

    /// Abandons the pending sign-in. An exchange the plugin already accepted
    /// stays its own.
    async fn login_cancel(&mut self) {
        if let Some(login) = self.login.take() {
            let _ = self
                .client
                .rpc(
                    &format!("{}.login.cancel", login.namespace),
                    crate::login::attempt(&login.namespace, &login.provider_id, &login.attempt_id),
                )
                .await;
        }
    }

    /// The browser reached the local listener: complete with its callback.
    /// A failed listener leaves the pasted callback to finish sign-in.
    async fn login_callback(&mut self, callback: McpCallback) {
        let Some(login) = self.login.as_mut() else {
            return;
        };
        if login.attempt_id != callback.attempt_id {
            return;
        }
        match callback.callback {
            Ok((url, stream)) => self.login_complete(url, Some(stream)).await,
            Err(_) => login.listener = None,
        }
    }

    /// Completes a browser sign-in with its callback URL, pasted or caught
    /// by the listener (whose browser tab is then answered).
    async fn login_complete(&mut self, callback: String, browser: Option<tokio::net::TcpStream>) {
        let Some(login) = self.login.take() else {
            return;
        };
        self.login_event(login.tag, OauthEvent::Exchanging);
        let mut params =
            crate::login::attempt(&login.namespace, &login.provider_id, &login.attempt_id);
        params["callbackUrl"] = json!(callback);
        let result = self
            .client
            .rpc(
                &format!("{}.login.browser.complete", login.namespace),
                params,
            )
            .await;
        if let Some(stream) = browser {
            answer_browser(stream, result.is_ok()).await;
        }
        match result {
            Ok(_) => self.login_done(login).await,
            Err(error) => {
                self.login_event(login.tag, OauthEvent::Done(Err(Some(error.to_string()))))
            }
        }
    }

    /// Asks how the device sign-in is going.
    async fn poll_login(&mut self) {
        #[derive(Deserialize)]
        #[serde(tag = "status", rename_all = "lowercase")]
        enum State {
            Pending,
            Completed,
            Failed { error: String },
            Cancelled,
            Expired,
        }
        let Some(login) = self.login.take() else {
            return;
        };
        let status = self
            .client
            .typed::<State>(
                &format!("{}.login.status", login.namespace),
                crate::login::attempt(&login.namespace, &login.provider_id, &login.attempt_id),
            )
            .await;
        let failed = |error: String| OauthEvent::Done(Err(Some(error)));
        match status {
            Ok(State::Pending) => {
                let mut login = login;
                login.poll_at = Some(tokio::time::Instant::now() + LOGIN_POLL);
                self.login = Some(login);
            }
            Ok(State::Completed) => self.login_done(login).await,
            Ok(State::Failed { error }) => self.login_event(
                login.tag,
                failed(format!("Login failed: {}", crate::sanitize::safe(&error))),
            ),
            Ok(State::Cancelled) => self.login_event(login.tag, OauthEvent::Done(Err(None))),
            Ok(State::Expired) => {
                self.login_event(login.tag, failed("Login expired; start login again".into()))
            }
            Err(error) => self.login_event(login.tag, failed(error.to_string())),
        }
    }

    async fn login_done(&mut self, login: Login) {
        if login.migrate_legacy {
            let providers = self.providers().await;
            self.send(Update::Providers(providers));
        }
        if matches!(
            login.namespace.as_str(),
            ACCOUNT_NAMESPACE | crate::login::CHATGPT_NAMESPACE
        ) {
            if login.poll_at.is_some() {
                self.login_event(login.tag, OauthEvent::Exchanging);
            }
            self.refresh_account_models(&login.provider_id, login.tag, true)
                .await;
        } else {
            self.login_event(login.tag, OauthEvent::Done(Ok(())));
        }
    }

    async fn refresh_account_models(&mut self, provider_id: &str, attempt: u64, after_login: bool) {
        let result = crate::login::refresh_models(&mut self.client, provider_id).await;
        self.login_event(
            attempt,
            OauthEvent::ModelsRefreshed {
                after_login,
                result,
            },
        );
    }

    async fn respond_question(
        &mut self,
        session_id: String,
        call: crate::question::Call,
        answers: Option<Vec<crate::question::Answer>>,
    ) {
        let params = crate::question::respond(&session_id, &call, answers.as_deref());
        match self.client.send(crate::question::RESPOND, params).await {
            Ok(id) => self.question_responses.push((id, session_id, call)),
            Err(error) => self.send(Update::QuestionReply {
                session_id,
                call,
                reply: crate::question::Reply::Failed(error.to_string()),
            }),
        }
    }

    /// A reply to a request sent without waiting for it.
    async fn reply(
        &mut self,
        id: u64,
        result: Result<serde_json::Value, client::RpcError>,
    ) -> crate::Result<()> {
        if let Some(index) = self
            .opening_reads
            .iter()
            .position(|(request, _)| *request == id)
        {
            let (_, kind) = self.opening_reads.remove(index);
            self.opening_reply(kind, result);
            return Ok(());
        }
        if self.owns_compaction(id) {
            self.compaction_reply(result);
            return Ok(());
        }
        if self
            .context_request
            .as_ref()
            .is_some_and(|(request, _)| *request == id)
        {
            let (_, report) = self
                .context_request
                .take()
                .expect("matched context request");
            let reading = result
                .map_err(|error| match app_error(&error) {
                    Some("context_not_configured") => {
                        "Context usage is known once a model has answered in this session"
                            .to_owned()
                    }
                    Some("context_usage_unavailable") => {
                        "Context usage will be available after the next model request".into()
                    }
                    Some("context_model_failed") => {
                        "Context usage needs the model's capabilities; select the model again"
                            .to_owned()
                    }
                    Some("unknown_method") => "This daemon does not track context usage".to_owned(),
                    _ => error.to_string(),
                })
                .and_then(|value| {
                    serde_json::from_value::<ContextUsage>(value)
                        .map_err(|error| format!("Invalid context usage reply: {error}"))
                });
            self.send(Update::Context(reading.as_ref().ok().map(|usage| {
                ContextReading {
                    input: usage.input(),
                    window: usage.window(),
                    threshold: tokens(usage.threshold_tokens),
                }
            })));
            if report {
                match reading {
                    Ok(usage) => {
                        let mut line = usage.describe();
                        let loaded = self
                            .task_history
                            .get(&self.default_branch)
                            .is_some_and(|history| history.done);
                        if let Some(cached) = self
                            .last_usage
                            .filter(|_| loaded)
                            .and_then(|usage| usage.percent())
                        {
                            line.push_str(" · last request cached ");
                            line.push_str(&cached);
                        }
                        self.report(line, false);
                    }
                    Err(error) => self.report(error, true),
                }
            }
            return Ok(());
        }
        if self
            .catalog_request
            .as_ref()
            .is_some_and(|(request, ..)| *request == id)
        {
            let (_, id, paged, scope, show_acp) = self
                .catalog_request
                .take()
                .expect("matched catalog request");
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Page {
                sessions: Vec<SessionSummary>,
                next_cursor: Option<String>,
            }
            let result = result.map_err(|error| error.to_string()).and_then(|value| {
                let (mut sessions, next_cursor) = if paged {
                    let page: Page = serde_json::from_value(value)
                        .map_err(|error| format!("Invalid catalog reply: {error}"))?;
                    (page.sessions, page.next_cursor)
                } else {
                    let sessions: Vec<SessionSummary> = serde_json::from_value(value)
                        .map_err(|error| format!("Invalid summaries reply: {error}"))?;
                    (sessions, None)
                };
                if !paged {
                    sessions.sort_by_key(|session| std::cmp::Reverse(session.updated_at));
                    let workspace = self.options.workspace().as_str();
                    sessions.retain(|session| match &scope {
                        SessionsScope::Local(None) => session.cwd == workspace,
                        SessionsScope::Local(Some(workspaces)) => workspaces.contains(&session.cwd),
                        SessionsScope::All => true,
                    });
                    sessions.retain(|session| {
                        if session.origin.as_deref() == Some("acp") {
                            show_acp
                        } else {
                            session.preview.is_some()
                        }
                    });
                }
                Ok((
                    sessions
                        .into_iter()
                        .map(|session| SessionItem {
                            id: session.session_id,
                            title: session.title,
                            preview: session.preview,
                            cwd: session.cwd,
                            updated_at: session.updated_at,
                            model: session.model.model_id,
                            origin: session.origin,
                            closed: session.closed,
                        })
                        .collect(),
                    next_cursor,
                ))
            });
            self.send(Update::Sessions { id, paged, result });
            return Ok(());
        }
        if let Some(index) = self
            .question_responses
            .iter()
            .position(|(request, _, _)| *request == id)
        {
            let (_, session_id, call) = self.question_responses.remove(index);
            let reply = crate::question::reply(&result);
            if session_id == self.session_id && reply == crate::question::Reply::Accepted {
                self.projection.questions.remove(&call);
                self.changed();
            }
            self.send(Update::QuestionReply {
                session_id,
                call,
                reply,
            });
            return Ok(());
        }
        if self.members.owns(id) {
            self.member_reply(id, result).await;
            return Ok(());
        }
        if self
            .task_history_request
            .as_ref()
            .is_some_and(|request| request.id == id)
        {
            return self.task_history_reply(result).await;
        }
        if self.task_stops.as_ref().is_some_and(|batch| {
            batch
                .request
                .as_ref()
                .is_some_and(|(request, _, _)| *request == id)
        }) {
            self.task_stop_reply(result);
            return Ok(());
        }
        if self
            .workflow
            .request
            .as_ref()
            .is_some_and(|request| request.id == id)
        {
            self.workflow_reply(result);
            return Ok(());
        }
        if let Some(index) = self.workflow.stops.iter().position(|stop| stop.id == id) {
            self.workflow_stop_reply(index, result);
            return Ok(());
        }
        if self
            .output_request
            .is_some_and(|(request, _)| request == id)
        {
            let (_, current) = self.output_request.take().expect("matched output request");
            if current && let Some(output) = &mut self.output {
                #[derive(Deserialize)]
                struct Reply {
                    text: String,
                }
                let (text, state) = match result {
                    Ok(value) => match serde_json::from_value::<Reply>(value) {
                        Ok(reply) => (Some(reply.text), TaskOutputState::Live),
                        Err(_) => (
                            None,
                            TaskOutputState::ReadError(
                                "Invalid session.exec.output response".into(),
                            ),
                        ),
                    },
                    Err(error) => (
                        None,
                        match app_error(&error) {
                            Some("exec_task_unknown") => TaskOutputState::Unavailable,
                            Some("unknown_method") => TaskOutputState::Unsupported,
                            _ => TaskOutputState::ReadError(error.to_string()),
                        },
                    ),
                };
                output.poll_at = matches!(&state, TaskOutputState::Live)
                    .then(|| tokio::time::Instant::now() + OUTPUT_INTERVAL);
                let target = output.target.clone();
                self.send(Update::TaskOutput {
                    projection_id: self.projection_id,
                    target,
                    text,
                    state,
                });
            }
            return Ok(());
        }
        if let Some(index) = self
            .compact_requests
            .iter()
            .position(|request| *request == id)
        {
            self.compact_requests.remove(index);
            if let Some(reply) = compact_reply(result) {
                self.send(Update::Compacted(reply));
            }
            return Ok(());
        }
        if let Some(index) = self.shell_stops.iter().position(|stop| *stop == id) {
            self.shell_stops.remove(index);
            if let Err(error) = result {
                self.send(Update::Notice(format!("Shell stop: {error}")));
            }
            return Ok(());
        }
        if self
            .shell
            .as_ref()
            .is_some_and(|(request, _)| *request == id)
        {
            let (_, run_id) = self.shell.take().expect("shell run");
            let result = result
                .map_err(|error| match app_error(&error) {
                    Some("operation_interrupted") => {
                        format!("{error}; the command may still be running")
                    }
                    _ => error.to_string(),
                })
                .and_then(|value| {
                    serde_json::from_value(value).map_err(|_| "Invalid shell.run reply".to_owned())
                });
            self.send(Update::ShellDone { run_id, result });
        }
        Ok(())
    }

    /// Applies an event the live stream carried: a task it ends is news.
    fn apply(&mut self, branch_id: &str, event: Event) {
        self.fold(branch_id, event, false);
    }

    /// Applies an event a read of recorded history replayed: a task it ends
    /// had already ended, so it is history (`Projection::replay`).
    fn replay(&mut self, branch_id: &str, event: Event) {
        self.fold(branch_id, event, true);
    }

    fn fold(&mut self, branch_id: &str, event: Event, replayed: bool) {
        if let EventContent::Configuration(configuration) = &event.content {
            if self
                .line_configurations
                .get(branch_id)
                .is_none_or(|known| known.sequence < event.sequence)
            {
                self.line_configurations.insert(
                    branch_id.to_owned(),
                    LineConfiguration {
                        sequence: event.sequence,
                        configuration: configuration.clone(),
                    },
                );
            }
            if branch_id == self.default_branch && event.sequence > self.model_through {
                self.model_through = event.sequence;
                self.model = Some(configuration.clone());
                self.send(Update::Ready(configuration.clone()));
            }
        }
        self.discover_task_branches(&event);
        let captured = if self.report_available()
            && (branch_id == self.focus || self.side.as_deref() == Some(branch_id))
        {
            crate::report::Captured::event(&event)
        } else {
            None
        };
        let inline_report = captured.as_ref().is_some_and(|error| {
            matches!(error.operation, crate::report::Operation::ModelGeneration)
        });
        let mut inline_shown = false;
        if let Some(line) = self.lines.get_mut(branch_id)
            && event.sequence > line.cursor
        {
            line.cursor = event.sequence;
            let previous_rows = line.projection.rows().len();
            match replayed {
                true => line.projection.replay(branch_id, &event),
                false => line.projection.apply(branch_id, &event),
            }
            inline_shown = inline_report && line.projection.mark_reportable_error(previous_rows);
        }
        let cursor = self.cursors.entry(branch_id.to_owned()).or_default();
        if event.sequence <= *cursor {
            if replayed {
                // A cut read moves the cursor to a line's head without
                // replaying its events; the line list still counts them.
                self.projection.line_event(branch_id, &event);
            }
            return;
        }
        *cursor = event.sequence;
        self.discover_workflow(branch_id, &event);
        if branch_id == self.default_branch
            && let EventContent::PermissionChanged(data) = &event.content
        {
            self.opening_reads
                .retain(|(_, kind)| !matches!(kind, OpeningRead::Permissions { .. }));
            self.send(Update::Permissions(Ok(data.policy.clone())));
        }
        if branch_id == self.default_branch
            && matches!(
                event.content,
                EventContent::Compaction(_) | EventContent::CompactionSummary(_)
            )
        {
            self.context_stale = true;
        }
        if branch_id == self.default_branch
            && let EventContent::ModelUsage(data) = &event.content
        {
            self.last_usage = Some(LastUsage {
                prompt_tokens: tokens(data.usage.prompt_tokens),
                cache_read_tokens: data.usage.cache_read_tokens.map(tokens),
            });
        }
        let previous_rows = self.projection.rows().len();
        match replayed {
            true => self.projection.replay(branch_id, &event),
            false => self.projection.apply(branch_id, &event),
        }
        if let Some(error) = captured
            && (!inline_report
                || self.projection.mark_reportable_error(previous_rows)
                || inline_shown)
        {
            self.reportable(error, false);
        }
        if let EventContent::ExecEnded(data) = &event.content
            && self.output.as_ref().is_some_and(|output| {
                output.target.branch_id == branch_id && output.target.task_id == data.task_id
            })
        {
            self.watch_task_output(None);
            self.publish();
            return;
        }
        self.changed();
    }

    fn watch_task_output(&mut self, target: Option<TaskTarget>) {
        if let Some((_, current)) = &mut self.output_request {
            *current = false;
        }
        self.output = target.map(|target| OutputWatch {
            target,
            poll_at: Some(tokio::time::Instant::now()),
        });
    }

    async fn poll_task_output(&mut self) -> crate::Result<()> {
        let output = self
            .output
            .as_mut()
            .expect("a visible output detail schedules the read");
        let id = self
            .client
            .send(
                "session.exec.output",
                json!({
                    "sessionId": self.session_id, "branchId": output.target.branch_id,
                    "taskId": output.target.task_id,
                }),
            )
            .await?;
        output.poll_at = None;
        self.output_request = Some((id, true));
        Ok(())
    }

    fn line_views(&self) -> Vec<LineView> {
        let running = |id: &str| self.task_history.get(id).and_then(|branch| branch.running);
        let mut lines = vec![LineView {
            id: ROOT_BRANCH_ID.into(),
            parent: None,
            purpose: String::new(),
            state: LineState::Main,
            running: running(ROOT_BRANCH_ID),
            branch_point: None,
            cut: None,
        }];
        lines.extend(
            self.projection
                .line_views()
                .iter()
                .cloned()
                .map(|mut line| {
                    line.running = running(&line.id);
                    if line.state == LineState::Waiting && line.running == Some(true) {
                        line.state = LineState::Working;
                    }
                    line
                }),
        );
        lines
    }

    /// `session.branch_set_default`, then shows the line the Session's
    /// conversation is on now; a refusal goes back to the panel that asked
    /// (design §14).
    async fn set_default_line(
        &mut self,
        session_id: String,
        projection_id: u64,
        branch_id: String,
    ) {
        if !self.attached(&session_id, projection_id) {
            self.send(Update::Notice(
                "Session attachment changed; reopen the lines before choosing one".into(),
            ));
            return;
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Reply {
            default_branch_id: String,
        }
        match self
            .client
            .typed::<Reply>(
                "session.branch_set_default",
                json!({"sessionId": self.session_id, "branchId": branch_id.clone()}),
            )
            .await
        {
            Ok(reply) => {
                self.default_branch = reply.default_branch_id;
                self.moves += 1;
                self.continue_on_line().await;
            }
            Err(error) => self.send(Update::LineRefusal {
                projection_id,
                branch_id,
                text: error.to_string(),
            }),
        }
    }

    /// Shows the line the Session's conversation is on now: its transcript,
    /// and its own model, permission and context in the top bar (design §14).
    /// A line numbers its own events, so the model watermark starts from that
    /// line's own last fact, whatever its sequence (timeline UI §5).
    async fn continue_on_line(&mut self) {
        self.side = None;
        self.last_usage = None;
        self.context_request = None;
        self.model_through = 0;
        self.model = None;
        self.unreadable_line = None;
        let line = self.default_branch.clone();
        let mut adopted = self
            .line_configurations
            .get(&line)
            .map(|adopted| (adopted.sequence, Some(adopted.configuration.clone())));
        if adopted.is_none() && line != ROOT_BRANCH_ID {
            let through = self
                .task_history
                .get(&line)
                .and_then(|history| history.window.through);
            adopted = self.line_configuration(&line, through).await;
        }
        if let Some((cutoff, configuration)) = adopted {
            self.model_through = cutoff;
            if let Some(configuration) = configuration {
                self.model = Some(configuration.clone());
                self.line_configurations.insert(
                    line.clone(),
                    LineConfiguration {
                        sequence: cutoff,
                        configuration: configuration.clone(),
                    },
                );
                self.send(Update::Ready(configuration));
            }
        }
        if self.model.is_none() {
            self.send(Update::NoModel);
        }
        // The policy shown belonged to the line that is no longer the centre;
        // this line's own read publishes its own, and a refused or failed read
        // leaves it unknown rather than showing the outgoing preset (design
        // §14).
        self.send(Update::PermissionsUnknown);
        self.focus_line(line).await;
        if self.client.supports(&permission::PRESET_METHODS) {
            let params = json!({"sessionId":self.session_id,"branchId":self.default_branch});
            match self.client.send("session.permission.get", params).await {
                Ok(id) => self.opening_reads.push((
                    id,
                    OpeningRead::Permissions {
                        line: self.default_branch.clone(),
                        generation: self.moves,
                    },
                )),
                Err(error) => self.send(Update::Notice(format!("permission: {error}"))),
            }
        }
        let line = self.default_branch.clone();
        self.request_context(false, &line).await;
    }

    /// One line's adopted configuration through `session.model.get`, at
    /// `through` or at the line's head, with the line's own cutoff. A server
    /// without that read cannot name this line's configuration, and the attach
    /// reply's is the root's, so the line reports that and carries no model
    /// (design §14).
    async fn line_configuration(
        &mut self,
        line: &str,
        through: Option<u64>,
    ) -> Option<(u64, Option<Configuration>)> {
        // The window that follows an entry asks again; the user hears the
        // limitation once per entry, not once per page (design §14).
        let reported = self.unreadable_line.as_deref() == Some(line);
        if !self.client.offers("session.model.get") {
            if !reported {
                self.unreadable_line = Some(line.to_owned());
                self.send(Update::Notice(format!(
                    "Configuration: this server cannot read line {line}"
                )));
            }
            return None;
        }
        match dim_client::configuration::adopted(&mut self.client, &self.session_id, line, through)
            .await
        {
            Ok(adopted) => Some(adopted),
            Err(error) => {
                if !reported {
                    self.unreadable_line = Some(line.to_owned());
                    self.send(Update::Notice(format!("Configuration: {error}")));
                }
                None
            }
        }
    }

    async fn focus_line(&mut self, line: String) {
        if line != ROOT_BRANCH_ID && !self.task_history.contains_key(&line) {
            self.send(Update::Notice(format!("No line {line} in this session")));
            return;
        }
        if line != ROOT_BRANCH_ID && self.projection.creation_failed(&line) {
            self.send(Update::Notice(format!("Line creation failed: {line}")));
            return;
        }
        if line != ROOT_BRANCH_ID {
            self.line_projection(&line);
        }
        self.focus = line;
        self.side = None;
        self.publish();
        if self.focus != ROOT_BRANCH_ID && self.client.supports(&["session.branch.get"]) {
            match self
                .client
                .typed::<dim_client::wire::BranchEntry>(
                    "session.branch.get",
                    json!({"sessionId":self.session_id,"branchId":self.focus}),
                )
                .await
            {
                Ok(entry) => {
                    self.projection.directory(&entry);
                    self.publish();
                }
                Err(error) => self.send(Update::Notice(format!("Branch details: {error}"))),
            }
        }
    }

    /// The transcript the screen shows: the focused line's, or `main`'s.
    fn shown(&self) -> &Projection {
        self.projection_on(&self.focus)
    }

    /// The projection that draws `branch`, when the interface opened it.
    fn projection_on(&self, branch: &str) -> &Projection {
        self.lines
            .get(branch)
            .filter(|_| branch != ROOT_BRANCH_ID)
            .map_or(&self.projection, |line| &line.projection)
    }

    /// Schedules a snapshot a frame interval after the first change.
    fn changed(&mut self) {
        self.publish_at
            .get_or_insert_with(|| tokio::time::Instant::now() + PUBLISH_INTERVAL);
    }

    fn publish(&mut self) {
        self.publish_at = None;
        self.retired_inputs.retain(|id, _| {
            self.projection.held_input(id).is_some()
                || self
                    .lines
                    .values()
                    .any(|line| line.projection.held_input(id).is_some())
        });
        let mut snapshot = match self
            .lines
            .get(&self.focus)
            .filter(|_| self.focus != ROOT_BRANCH_ID)
        {
            Some(line) => line.projection.snapshot(),
            None => self.projection.snapshot(),
        };
        snapshot.lines = self.line_views();
        snapshot.unread_lines = snapshot
            .lines
            .iter()
            .filter(|line| {
                self.task_history
                    .get(&line.id)
                    .is_some_and(|history| history.latest_reply > history.seen_reply)
            })
            .map(|line| line.id.clone())
            .collect();
        snapshot.side = self.side.as_ref().and_then(|id| {
            let line = snapshot.lines.iter().find(|line| &line.id == id)?;
            Some(SideLine {
                id: id.clone(),
                purpose: line.purpose.clone(),
                state: line.state.clone(),
                rows: self
                    .lines
                    .get(id)
                    .map_or_else(Vec::new, |line| line.projection.rows().to_vec()),
            })
        });
        snapshot.default_branch_id = self.default_branch.clone();
        snapshot.focus = self.focus.clone();
        snapshot.main_topic = self.projection.first_message();
        snapshot.session_title = self.session_title.clone();
        if self.focus != ROOT_BRANCH_ID {
            snapshot.approvals = self.projection.approvals();
            snapshot.questions = self.projection.questions();
            snapshot.tasks = self.projection.tasks().to_vec();
        }
        snapshot.session_id = self.session_id.clone();
        snapshot.projection_id = self.projection_id;
        let shown = self.task_history.get(&self.focus);
        snapshot.history_complete = shown.is_some_and(|branch| branch.window_complete);
        snapshot.window_error = shown.and_then(|branch| branch.window_error.clone());
        snapshot.earlier = self.earlier_view();
        snapshot.task_history_loading = self
            .task_history
            .values()
            .filter(|branch| !branch.done && branch.error.is_none())
            .count();
        snapshot.task_history_errors = self
            .task_history
            .iter()
            .filter_map(|(id, branch)| branch.error.as_ref().map(|error| format!("{id}: {error}")))
            .collect();
        snapshot.task_history_errors.sort();
        snapshot.workflow = self
            .workflow
            .inventory(self.client.supports(&["workflow.status"]));
        snapshot.workflow_stop = self.client.supports(&["workflow.stop"]);
        self.send(Update::Snapshot(Box::new(snapshot)));
        self.publish_member();
    }

    fn send(&self, update: Update) {
        let _ = self.updates.send(update);
    }
}

/// Answers the browser tab the local listener caught a callback from.
pub(crate) async fn answer_browser(mut stream: tokio::net::TcpStream, completed: bool) {
    use tokio::io::AsyncWriteExt;
    let page = if completed {
        "Sign-in completed. You can close this window."
    } else {
        "Return to the terminal to continue sign-in."
    };
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/plain; charset=utf-8\r\nCache-Control: no-store\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{page}",
        page.len()
    );
    let _ = stream.write_all(response.as_bytes()).await;
}

/// `12345` reads `12,345`.
fn group(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// What identification made of one entry's id, in the words its row shows.
///
/// A merge is the one rule whose numbers come from several rows rather than
/// from one declaration, so it reads as inferred from them; every other rule
/// named one row, and the entry's `matchedProviderId` and `matchedModelId` say
/// which.
fn model_outcome(
    metadata: Option<&crate::wire::CatalogPreferences>,
    listed_by: &str,
) -> ModelOutcome {
    let Some(metadata) = metadata else {
        return ModelOutcome::Unidentified;
    };
    let Some(identification) = metadata.identification.as_ref() else {
        return ModelOutcome::Unidentified;
    };
    if identification.rule == "ambiguous" {
        return ModelOutcome::Ambiguous {
            candidates: identification
                .candidates
                .iter()
                .map(|candidate| format!("{}/{}", candidate.provider_id, candidate.model_id))
                .collect(),
        };
    }
    ModelOutcome::Identified {
        provider_id: metadata
            .matched_provider_id
            .as_deref()
            .unwrap_or(listed_by)
            .to_owned(),
        model_id: metadata.matched_model_id.clone().unwrap_or_default(),
        inferred: identification.rule == "merge",
    }
}

fn effort_choices(
    capabilities: &dim_agent_types::CatalogCapabilities,
    reasoning: Option<&dim_agent_types::ModelReasoning>,
) -> (Vec<String>, Option<String>) {
    let Some(reasoning) = reasoning else {
        return (Vec::new(), None);
    };
    if !reasoning.supported || !capabilities.reasoning {
        return (Vec::new(), None);
    }
    let efforts = reasoning.offered_efforts();
    let default = reasoning
        .effort
        .clone()
        .filter(|effort| efforts.iter().any(|option| option == effort));
    (efforts, default)
}

pub(crate) async fn adopt_startup(
    client: &mut Client,
    options: &Options,
    session_id: &str,
    branch: &str,
) -> crate::Result<()> {
    if options.startup.is_none() && options.effort.is_none() && options.model.is_none() {
        return Ok(());
    }
    // The update names the line the attach reply named: a move between attach
    // and this write must not apply the startup configuration to another line
    // (design §9, §14).
    let mut params = json!({"sessionId":session_id,"branchId":branch});
    if let Some(startup) = &options.startup {
        params["startup"] = startup.clone();
    }
    if let Some(model) = &options.model {
        params["modelId"] = json!(model);
    }
    if let Some(provider) = &options.provider {
        params["providerId"] = json!(provider);
    }
    if let Some(preferences) = options.creation_preferences() {
        params["modelPreferences"] = preferences;
    }
    client
        .rpc("session.model.update", params)
        .await
        .map_err(|error| {
            client::resume_failure(
                "Cannot adopt startup configuration for resumed Session",
                error,
            )
        })?;
    Ok(())
}

/// The version of a `provider.selection` reply's row, `None` when the scope
/// holds none: the value `provider.select` replaces for that same scope.
fn selection_version(reply: serde_json::Value) -> crate::Result<Option<u32>> {
    #[derive(Deserialize)]
    struct Selection {
        version: u32,
    }
    #[derive(Deserialize)]
    struct Current {
        selection: Option<Selection>,
    }
    let current: Current =
        serde_json::from_value(reply).map_err(|_| "Invalid provider.selection response")?;
    Ok(current.selection.map(|selection| selection.version))
}

pub(crate) async fn clear_model_preferences(
    client: &mut Client,
    provider_id: &str,
    model_id: &str,
    expected_version: u32,
) -> crate::Result<()> {
    client.rpc("provider.preferences.update", json!({
        "providerId": provider_id, "modelId": model_id, "expectedVersion": expected_version,
        "patch": {"reasoningEffort": {"action": "clear"}, "serviceTier": {"action": "clear"}}
    })).await?;
    Ok(())
}

pub(crate) async fn clear_saved_model_preferences(
    client: &mut Client,
    provider_id: &str,
    model_id: &str,
) -> crate::Result<()> {
    let snapshot: ProviderManagement = client
        .typed(
            "provider.management",
            json!({"providerId": provider_id, "modelId": model_id}),
        )
        .await?;
    let version = snapshot
        .providers
        .into_iter()
        .next()
        .ok_or("Provider management entry is missing")?
        .info
        .version
        .ok_or("Provider catalog must be refreshed before editing preferences")?;
    clear_model_preferences(client, provider_id, model_id, version).await
}

/// The models `dimcode models PROVIDER --set-limits` declares limits for.
pub(crate) enum LimitTarget {
    /// The one model the command names.
    Model(String),
    /// Every model `dimcode models` marks `undeclared`: an enabled model whose
    /// missing limits are its only blocker.
    Missing,
}

/// Declares a context window and maximum output for the target models
/// through `provider.models.edit`, sending each model only the limits its
/// catalog entry reports missing, so a declared value is never overwritten.
/// The catalog reply carries the record version, and each write uses the
/// version the previous reply returned. A conflict stops the run and keeps
/// the server's reread requirement.
pub(crate) async fn declare_model_limits(
    client: &mut Client,
    provider_id: &str,
    target: LimitTarget,
    context_window: u64,
    max_output_tokens: u64,
) -> crate::Result<()> {
    let mut params = json!({"providerId": provider_id});
    if let LimitTarget::Model(model_id) = &target {
        params["modelId"] = json!(model_id);
    }
    let snapshot: ProviderCatalogSnapshot = client.typed("provider.catalog", params).await?;
    let catalog = snapshot
        .catalog
        .ok_or("Provider catalog must be refreshed before declaring model limits")?;
    let mut version = catalog.version;
    let targets: Vec<(String, Vec<String>)> = match target {
        LimitTarget::Model(model) => vec![(
            model,
            vec!["contextWindow".into(), "maxOutputTokens".into()],
        )],
        LimitTarget::Missing => {
            let disabled = catalog.disabled_model_ids;
            catalog
                .models
                .into_iter()
                .filter(|model| {
                    crate::limits_undeclared(
                        &model.missing_limits,
                        model.unavailable_reason.as_deref(),
                        disabled.contains(&model.id),
                    )
                })
                .map(|model| (model.id, model.missing_limits))
                .collect()
        }
    };
    for (model_id, missing) in targets {
        #[derive(Deserialize)]
        struct Edited {
            version: u32,
        }
        let mut edit = serde_json::Map::new();
        for field in missing {
            match field.as_str() {
                "contextWindow" => {
                    edit.insert("contextWindow".into(), json!(context_window));
                }
                "maxOutputTokens" => {
                    edit.insert("maxOutputTokens".into(), json!(max_output_tokens));
                }
                _ => {}
            }
        }
        let edited: Edited = client
            .typed(
                "provider.models.edit",
                json!({
                    "providerId": provider_id,
                    "expectedVersion": version,
                    "modelId": model_id,
                    "edit": edit,
                    "reply": "summary",
                }),
            )
            .await
            .map_err(|error| match client::app_error(&error) {
                Some("provider_conflict") if !error.to_string().contains("reread") => {
                    format!("{error}; reread before retrying").into()
                }
                _ => error,
            })?;
        version = edited.version;
    }
    Ok(())
}

/// The notice when no Session could be started.
fn no_session(error: &crate::Error) -> String {
    match error.downcast_ref::<client::RpcError>().and_then(app_error) {
        Some("provider_failed") => NO_SESSION.to_owned(),
        _ => format!("{error}. {NO_SESSION}"),
    }
}

fn app_error(error: &client::RpcError) -> Option<&str> {
    error
        .data
        .as_ref()
        .and_then(|data| data.get("appError"))
        .and_then(serde_json::Value::as_str)
}

/// What a `session.context.compact` reply came to.
fn compact_reply(result: Result<serde_json::Value, client::RpcError>) -> Option<CompactReply> {
    #[derive(Deserialize)]
    #[serde(tag = "status", rename_all = "snake_case")]
    enum Reply {
        Completed {},
        Skipped {
            #[serde(rename = "reason")]
            _reason: String,
        },
    }
    match result {
        Ok(value) => match serde_json::from_value::<Reply>(value) {
            Ok(Reply::Completed { .. } | Reply::Skipped { .. }) => None,
            Err(_) => Some(CompactReply::Failed(
                "Invalid session.context.compact response".into(),
            )),
        },
        Err(error) => match app_error(&error) {
            Some("unknown_method") => Some(CompactReply::Unsupported),
            Some("compaction_failed" | "compaction_cancelled") => None,
            _ => Some(CompactReply::Failed(error.message)),
        },
    }
}

/// The catalog `cwd` for `scope`: the launch workspace, the git set (which
/// may hold one entry), or none.
fn scope_cwd(workspace: &str, scope: &SessionsScope) -> Option<serde_json::Value> {
    match scope {
        SessionsScope::Local(None) => Some(json!(workspace)),
        SessionsScope::Local(Some(workspaces)) => Some(json!(workspaces)),
        SessionsScope::All => None,
    }
}

/// The workspaces `/resume` offers as one repository: the launch workspace
/// and the same relative directory in every worktree of its git repository,
/// one worktree or several. Both reads share one two-second bound; a git that
/// fails or does not answer leaves the set empty, and the local scope keeps
/// the exact launch workspace.
async fn repository_workspaces(workspace: &str) -> Vec<String> {
    let read = async {
        let path = std::path::Path::new(workspace);
        let root = git_output(path, &["rev-parse", "--show-toplevel"]).await?;
        let root = String::from_utf8(root).ok()?;
        let root = std::path::PathBuf::from(root.trim());
        let root = std::fs::canonicalize(&root).unwrap_or(root);
        let relative = path.strip_prefix(&root).ok()?;
        let list = git_output(path, &["worktree", "list", "--porcelain", "-z"]).await?;
        Some(worktree_workspaces(&list, relative, workspace))
    };
    tokio::time::timeout(std::time::Duration::from_secs(2), read)
        .await
        .ok()
        .flatten()
        .unwrap_or_default()
}

/// One git read from the workspace; a failed read is no answer.
async fn git_output(workspace: &std::path::Path, args: &[&str]) -> Option<Vec<u8>> {
    let output = tokio::process::Command::new("git")
        .current_dir(workspace)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .ok()?;
    output.status.success().then_some(output.stdout)
}

/// The listed worktrees, each joined with the workspace's relative directory.
fn worktree_workspaces(list: &[u8], relative: &std::path::Path, workspace: &str) -> Vec<String> {
    let mut workspaces = vec![workspace.to_owned()];
    let mut entry: Option<&[u8]> = None;
    let mut bare = false;
    for field in list.split(|byte| *byte == 0) {
        if field.is_empty() {
            if let Some(path) = entry.take().and_then(|path| std::str::from_utf8(path).ok())
                && !bare
            {
                let path = std::path::Path::new(path);
                let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
                // A launch at the repository root keeps the worktree path
                // as it is: `join` with an empty relative path would add a
                // trailing separator and list the launch workspace twice.
                let joined = if relative.as_os_str().is_empty() {
                    path
                } else {
                    path.join(relative)
                };
                if let Some(joined) = joined.to_str()
                    && !workspaces.iter().any(|workspace| workspace == joined)
                {
                    workspaces.push(joined.to_owned());
                }
            }
            bare = false;
        } else if let Some(path) = field.strip_prefix(b"worktree ") {
            entry = Some(path);
        } else if field == b"bare" {
            bare = true;
        }
    }
    workspaces
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionSummary {
    session_id: String,
    cwd: String,
    model: ModelId,
    updated_at: u64,
    #[serde(default)]
    preview: Option<String>,
    title: Option<String>,
    /// The creation origin, from a daemon that reports one (#557).
    #[serde(default)]
    origin: Option<String>,
    #[serde(default)]
    closed: bool,
}

fn execution_snapshot(value: serde_json::Value) -> Result<ExecutionSnapshot, String> {
    let revision = value
        .get("revision")
        .and_then(|value| value.as_str())
        .ok_or_else(|| "execution settings response has no revision".to_string())?
        .to_owned();
    let document = value.get("document").cloned().unwrap_or_else(|| json!({}));
    Ok(ExecutionSnapshot {
        revision,
        sandbox: document
            .get("sandbox")
            .and_then(|value| value.as_bool())
            .unwrap_or(false),
        network: document
            .get("network")
            .and_then(|value| value.as_bool())
            .unwrap_or(false),
    })
}

/// The `prompt-suggestion` source as `settings.read` and `settings.update`
/// answer it, in the capability's own document type: a missing or malformed
/// document, or a missing member, is an error rather than an invented value.
fn suggestion_setting(value: serde_json::Value) -> Result<SuggestionSetting, String> {
    let revision = value
        .get("revision")
        .and_then(|value| value.as_str())
        .ok_or_else(|| "prompt suggestion settings response has no revision".to_string())?
        .to_owned();
    let document = value
        .get("document")
        .cloned()
        .ok_or_else(|| "prompt suggestion settings response has no document".to_string())?;
    let document = serde_json::from_value(document)
        .map_err(|error| format!("prompt suggestion settings: {error}"))?;
    Ok(SuggestionSetting { revision, document })
}

/// The `runtime.*` calls a change makes, in order: the configuration write
/// first when it has one, then the apply (RuntimeManager §5).
fn runtime_calls(action: &RuntimeAction) -> Vec<(&'static str, serde_json::Value)> {
    match action {
        RuntimeAction::Install(name) => vec![("runtime.install", json!({"name": name}))],
        RuntimeAction::Configure { name, source, auto } => {
            let mut configure = json!({"name": name});
            if let Some(source) = source {
                configure["source"] = json!(source.as_str());
            }
            if let Some(auto) = auto {
                configure["auto"] = json!(auto);
            }
            // `runtime.configure` applies what it wrote; installing again
            // would only re-read the same file.
            vec![("runtime.configure", configure)]
        }
    }
}

/// The methods every test worker's daemon advertises: enough for `hello`, and
/// a bounded window read.
#[cfg(all(test, unix))]
const TEST_FEATURES: [&str; 4] = [
    "session.create",
    "session.history",
    "session.history.latestTurn",
    "session.input.pending.blockPresence",
];

/// A worker on a daemon that advertises `features` and answers only
/// `session.attach` and `session.detach`: the test supplies every other reply
/// itself, through `Worker::message`. Every request the worker sends is
/// recorded, in order, for the tests that assert on one.
#[cfg(all(test, unix))]
async fn test_worker(
    features: &[&str],
) -> (
    Worker,
    mpsc::Receiver<Update>,
    std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
) {
    test_worker_answering(features, false, None).await
}

/// [`test_worker`] with a daemon that refuses `session.fork` with an RPC error
/// when `fail_fork` is set, and refuses `session.attach` for `refuse_attach`.
#[cfg(all(test, unix))]
async fn test_worker_answering(
    features: &[&str],
    fail_fork: bool,
    refuse_attach: Option<&str>,
) -> (
    Worker,
    mpsc::Receiver<Update>,
    std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
) {
    use rutis::host::CtxHost;
    use std::os::unix::fs::PermissionsExt;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let home = tempfile::tempdir().unwrap();
    let endpoint = home.path().join("daemon.sock");
    let listener = tokio::net::UnixListener::bind(&endpoint).unwrap();
    std::fs::set_permissions(&endpoint, std::fs::Permissions::from_mode(0o600)).unwrap();
    let options =
        Options::parse(["--home".into(), home.path().to_str().unwrap().to_owned()].into_iter())
            .unwrap();
    let advertised: Vec<String> = features
        .iter()
        .map(|feature| (*feature).to_owned())
        .collect();
    let sent = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let recorded = std::sync::Arc::clone(&sent);
    let refuse_attach = refuse_attach.map(str::to_owned);
    tokio::spawn(async move {
        let _home = home;
        let Ok((stream, _)) = listener.accept().await else {
            return;
        };
        let (reader, mut writer) = stream.into_split();
        let mut lines = BufReader::new(reader).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let Ok(request) = serde_json::from_str::<serde_json::Value>(&line) else {
                return;
            };
            if request["method"] != "hello" {
                recorded.lock().unwrap().push(request.clone());
                if request["method"] == "session.fork" && fail_fork {
                    let reply = json!({"jsonrpc":"2.0","id":request["id"],"error":{
                        "code":-32603,"message":"the test daemon refuses the fork"
                    }});
                    if writer
                        .write_all(format!("{reply}\n").as_bytes())
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
                if request["method"] == "session.detach" {
                    let reply = json!({"jsonrpc":"2.0","id":request["id"],"result":{}});
                    if writer
                        .write_all(format!("{reply}\n").as_bytes())
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
                if request["method"] == "session.attach" {
                    let refused = refuse_attach
                        .as_deref()
                        .is_some_and(|id| request["params"]["sessionId"] == id);
                    let reply = if refused {
                        json!({"jsonrpc":"2.0","id":request["id"],"error":{
                            "code":-32603,"message":"the test daemon refuses the attach"
                        }})
                    } else {
                        json!({"jsonrpc":"2.0","id":request["id"],"result":{
                            "sequence":1,"model":{"providerId":"test","modelId":"test"}
                        }})
                    };
                    if writer
                        .write_all(format!("{reply}\n").as_bytes())
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
                continue;
            }
            let reply = json!({"jsonrpc":"2.0","id":request["id"],"result":{
                "instanceId":"earlier-test","serverVersion":"test","protocolVersion":4,
                "features":advertised,"daemonProfile":{"kind":"product"}
            }});
            if writer
                .write_all(format!("{reply}\n").as_bytes())
                .await
                .is_err()
            {
                return;
            }
        }
    });
    let client = Client::connect(&dim_client::ConnectionConfig {
        endpoint,
        server: dim_client::Server {
            program: std::env::current_exe().unwrap(),
            embedded: false,
        },
        profile: None,
        client_kind: "cli",
        client_name: "dim-cli-test",
        protocol_version: 4,
    })
    .await
    .unwrap();
    let (updates, receiver) = mpsc::channel();
    let worker = Worker::new(
        options,
        client,
        updates,
        rutis::Ctx::root().unwrap(),
        tokio::sync::oneshot::channel().0,
    );
    (worker, receiver, sent)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The `prompt-suggestion` document is the capability's own type: a
    /// missing or malformed document, or a missing member, is an error rather
    /// than an invented value.
    #[test]
    fn a_malformed_suggestion_setting_is_an_error_not_a_default() {
        for value in [
            json!({"revision": "r1", "document": {"enabled": true}}),
            json!({"revision": "r1", "document": {"maxChars": 80}}),
            json!({"revision": "r1", "document": {"enabled": true, "maxChars": 80, "extra": 1}}),
            json!({"revision": "r1", "document": "not an object"}),
            json!({"revision": "r1"}),
            json!({"document": {"enabled": true, "maxChars": 80}}),
        ] {
            assert!(
                suggestion_setting(value.clone()).is_err(),
                "{value} was accepted"
            );
        }
        let setting =
            suggestion_setting(json!({"revision":"r1","document":{"enabled":true,"maxChars":120}}))
                .unwrap();
        assert_eq!(setting.revision, "r1");
        assert!(setting.document.enabled);
        assert_eq!(setting.document.max_chars, 120);
    }

    #[test]
    fn a_line_notice_reaches_its_own_transcript() {
        let mut main = Projection::default();
        let mut lines =
            std::collections::BTreeMap::from([("b-7c".to_owned(), LineProjection::new("b-7c"))]);
        transcript_mut(&mut main, &mut lines, "b-7c")
            .expect("the line")
            .set_running(true);
        assert!(lines["b-7c"].projection.snapshot().running);
        assert!(!main.snapshot().running, "main is not the line");
        transcript_mut(&mut main, &mut lines, "main")
            .expect("main")
            .set_running(true);
        assert!(main.snapshot().running);
        assert!(
            transcript_mut(&mut main, &mut lines, "unknown").is_none(),
            "an unknown branch has no transcript"
        );
    }

    /// A switch publishes the empty projection at once and the daemon's
    /// state report only later: the empty snapshot carries no state, so the
    /// interface must not read it as an idle Session.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_switch_publishes_the_placeholder_before_the_sessions_state() {
        use crate::model::view::ViewSnapshot;

        let (mut worker, mut receiver, _sent) = test_worker(&TEST_FEATURES).await;
        worker
            .attach("s2".into(), SwitchKind::Resumed, true)
            .await
            .expect("the switch attaches");
        let published = |receiver: &mut mpsc::Receiver<Update>| -> Vec<ViewSnapshot> {
            receiver
                .try_iter()
                .filter_map(|update| match update {
                    Update::Snapshot(view) => Some(*view),
                    _ => None,
                })
                .collect()
        };
        let snapshots = published(&mut receiver);
        let placeholder = snapshots.last().expect("the switch published a snapshot");
        assert_eq!(placeholder.session_id, "s2");
        assert!(placeholder.rows.is_empty(), "the projection is still empty");
        assert!(
            snapshots
                .iter()
                .all(|view| view.lines.iter().all(|line| line.running.is_none())),
            "the switch has no Session state to report yet"
        );

        worker
            .message(Message::Notice {
                method: "session.state".into(),
                params: json!({"sessionId":"s2","branchId":"main","running":true}),
            })
            .await
            .unwrap();
        worker.publish();
        let reported = published(&mut receiver);
        let reported = reported
            .last()
            .expect("the state report published a snapshot");
        assert!(
            reported.lines.iter().all(|line| line.running.is_some()),
            "the report is the Session's state"
        );
        assert!(reported.running, "the report carries the running turn");
    }

    /// A fork sent with an attachment that has moved on opens no Session, so
    /// the notice and `ForkFailed` are its only ending.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_stale_fork_ends_with_a_notice_and_fork_failed() {
        let (mut worker, receiver, _sent) = test_worker(&TEST_FEATURES).await;
        worker
            .attach("s2".into(), SwitchKind::Resumed, true)
            .await
            .expect("the switch attaches");
        receiver.try_iter().for_each(drop);
        worker
            .command(Command::Fork {
                session_id: "s1".into(),
                projection_id: 0,
                at: 1,
            })
            .await
            .expect("the stale fork is handled");
        let mut notices = Vec::new();
        let mut failed = false;
        for update in receiver.try_iter() {
            match update {
                Update::Notice(message) => notices.push(message),
                Update::ForkFailed => failed = true,
                _ => {}
            }
        }
        assert!(
            notices
                .iter()
                .any(|message| message == "Session attachment changed; pick the message again"),
            "the stale fork says why: {notices:?}"
        );
        assert!(failed, "the stale fork ends the wait");
    }

    /// A fork the daemon refuses with an RPC error ends with `ForkFailed`, so
    /// the interface stops waiting without a `Switched` for a Session that
    /// was never opened.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_refused_fork_ends_with_fork_failed() {
        let (mut worker, receiver, _sent) = test_worker_answering(&TEST_FEATURES, true, None).await;
        worker
            .attach("s2".into(), SwitchKind::Resumed, true)
            .await
            .expect("the switch attaches");
        receiver.try_iter().for_each(drop);
        let session_id = worker.session_id.clone();
        let projection_id = worker.projection_id;
        worker
            .command(Command::Fork {
                session_id,
                projection_id,
                at: 1,
            })
            .await
            .expect("the refused fork is handled");
        let mut notices = Vec::new();
        let mut failed = false;
        for update in receiver.try_iter() {
            match update {
                Update::Notice(message) => notices.push(message),
                Update::ForkFailed => failed = true,
                _ => {}
            }
        }
        assert!(failed, "the refused fork ends the wait");
        assert!(!notices.is_empty(), "the refused fork says why");
    }

    /// A `/new` is one command: the host ends it once, whether it leaves a
    /// Session or finds itself already Home. A refused resume reports Home
    /// with its own kind, so it cannot end the `/new` queued behind it.
    #[cfg(unix)]
    #[tokio::test]
    async fn every_new_session_command_ends_with_its_own_completion() {
        let mut features = TEST_FEATURES.to_vec();
        features.push("report.prepare");
        let (mut worker, receiver, _sent) =
            test_worker_answering(&features, false, Some("refused")).await;
        worker
            .attach("s1".into(), SwitchKind::Resumed, true)
            .await
            .expect("the switch attaches");
        receiver.try_iter().for_each(drop);

        worker
            .command(Command::Resume("refused".into()))
            .await
            .expect("the refused resume is handled");
        let mut resumed_home = false;
        for update in receiver.try_iter() {
            match update {
                Update::Switched {
                    session_id,
                    kind: SwitchKind::Resumed,
                    ..
                } => {
                    assert!(session_id.is_empty(), "the refusal reports Home");
                    resumed_home = true;
                }
                Update::NewSessionDone => panic!("the refused resume ended a /new"),
                _ => {}
            }
        }
        assert!(resumed_home, "the refused resume reports Home");

        worker
            .command(Command::NewSession)
            .await
            .expect("the already-Home /new is handled");
        assert_eq!(
            receiver
                .try_iter()
                .filter(|update| matches!(update, Update::NewSessionDone))
                .count(),
            1,
            "the already-Home /new has no completion"
        );

        worker
            .attach("s1".into(), SwitchKind::Resumed, true)
            .await
            .expect("the switch attaches");
        receiver.try_iter().for_each(drop);
        worker
            .command(Command::NewSession)
            .await
            .expect("the /new from a Session is handled");
        let mut switched = 0;
        let mut completions = 0;
        for update in receiver.try_iter() {
            match update {
                Update::Switched {
                    session_id,
                    kind: SwitchKind::New,
                    ..
                } => {
                    assert!(session_id.is_empty(), "the /new switch reports Home");
                    switched += 1;
                }
                Update::NewSessionDone => completions += 1,
                _ => {}
            }
        }
        assert_eq!(switched, 1, "the /new from a Session reports Home once");
        assert_eq!(completions, 1, "the /new from a Session has one completion");
    }

    /// A turn read answers what the recorded turn looks like, and a turn that
    /// already ended clears the transcript's running flag. The line's live
    /// report is a separate observation: it stays what `session.state` said,
    /// so a history reply cannot make a busy line read as idle.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_turn_read_rewrites_the_transcript_not_the_lines_live_report() {
        use crate::model::view::ViewSnapshot;

        let (mut worker, mut receiver, _sent) = test_worker(&TEST_FEATURES).await;
        worker
            .attach("s2".into(), SwitchKind::Resumed, true)
            .await
            .expect("the switch attaches");
        let published = |receiver: &mut mpsc::Receiver<Update>| -> ViewSnapshot {
            receiver
                .try_iter()
                .filter_map(|update| match update {
                    Update::Snapshot(view) => Some(*view),
                    _ => None,
                })
                .last()
                .expect("the worker published a snapshot")
        };
        worker
            .message(Message::Notice {
                method: "session.state".into(),
                params: json!({"sessionId":"s2","branchId":"main","running":true}),
            })
            .await
            .unwrap();
        worker.publish();
        let reported = published(&mut receiver);
        assert_eq!(reported.lines[0].running, Some(true));
        assert!(reported.running, "the report is main's running turn");

        worker
            .apply_inventory(
                DEFAULT_BRANCH_ID,
                1,
                "session.turn.get",
                json!({
                    "branchId":"main",
                    "through":1,
                    "turn":{"turnId":"t-1","sequence":1,"ended":true}
                }),
            )
            .expect("the turn read applies");
        worker.publish();
        let replayed = published(&mut receiver);
        assert!(!replayed.running, "the ended turn rewrites the transcript");
        assert_eq!(
            replayed.lines[0].running,
            Some(true),
            "the line keeps the daemon's live report"
        );
    }

    /// Every live line reports on its own: an idle `main` leaves a known
    /// child unreported until its own `session.state` arrives, and only then
    /// does that child's running flag show as work.
    #[cfg(unix)]
    #[tokio::test]
    async fn every_live_line_carries_its_own_state_report() {
        let (mut worker, mut receiver, _sent) = test_worker(&TEST_FEATURES).await;
        worker
            .attach("s2".into(), SwitchKind::Resumed, true)
            .await
            .expect("the switch attaches");
        let lines = |receiver: &mut mpsc::Receiver<Update>| {
            receiver
                .try_iter()
                .filter_map(|update| match update {
                    Update::Snapshot(view) => Some(*view),
                    _ => None,
                })
                .last()
                .expect("the worker published a snapshot")
                .lines
        };
        let create = json!({
            "id": "create", "sequence": 1, "create_at": 1000, "type": "branch", "name": "create",
            "data": {
                "at": {"kind": "sequence", "sequence": 0}, "branch": "b-1", "prompt": "Check the parser",
                "source": {"kind": "host", "operation_id": "op"}
            }
        });
        let create: Event = serde_json::from_value(create).unwrap();
        worker.discover_task_branches(&create);
        worker.apply("main", create);
        worker
            .message(Message::Notice {
                method: "session.state".into(),
                params: json!({"sessionId":"s2","branchId":"main","running":false}),
            })
            .await
            .unwrap();
        worker.publish();
        let shown = lines(&mut receiver);
        assert_eq!(
            shown[0].running,
            Some(false),
            "main reported its idle state"
        );
        assert_eq!(shown[1].state, LineState::Waiting);
        assert_eq!(
            shown[1].running, None,
            "the child line has not reported yet"
        );

        worker
            .message(Message::Notice {
                method: "session.state".into(),
                params: json!({"sessionId":"s2","branchId":"b-1","running":true}),
            })
            .await
            .unwrap();
        worker.publish();
        let shown = lines(&mut receiver);
        assert_eq!(
            shown[1].running,
            Some(true),
            "the child line reported its running state"
        );
        assert_eq!(shown[1].state, LineState::Working);
    }

    /// A line that ended before this client attached never sends a
    /// `session.state`. The host still lists it, ended and unreported, so the
    /// interface can settle it without waiting.
    #[cfg(unix)]
    #[tokio::test]
    async fn an_ended_line_is_listed_without_a_state_report() {
        let (mut worker, receiver, _sent) = test_worker(&TEST_FEATURES).await;
        worker
            .attach("s2".into(), SwitchKind::Resumed, true)
            .await
            .expect("the switch attaches");
        let create = json!({
            "id": "create", "sequence": 1, "create_at": 1000, "type": "branch", "name": "create",
            "data": {
                "at": {"kind": "sequence", "sequence": 0}, "branch": "b-gone", "prompt": "Old line",
                "source": {"kind": "host", "operation_id": "op"}
            }
        });
        let ended = json!({
            "id": "ended", "sequence": 2, "create_at": 2000, "type": "branch", "name": "ended",
            "data": {"branch": "b-gone", "endEvent": 2, "source": {"kind": "host", "operation_id": "op"}}
        });
        let create: Event = serde_json::from_value(create).unwrap();
        worker.discover_task_branches(&create);
        worker.apply("main", create);
        let ended: Event = serde_json::from_value(ended).unwrap();
        worker.discover_task_branches(&ended);
        worker.apply("main", ended);
        worker
            .command(Command::FocusLine("b-gone".into()))
            .await
            .unwrap();
        worker
            .message(Message::Notice {
                method: "session.state".into(),
                params: json!({"sessionId":"s2","branchId":"main","running":false}),
            })
            .await
            .unwrap();
        worker.publish();
        let shown = receiver
            .try_iter()
            .filter_map(|update| match update {
                Update::Snapshot(view) => Some(*view),
                _ => None,
            })
            .last()
            .expect("the worker published a snapshot");
        assert_eq!(shown.focus, "b-gone");
        let gone = shown
            .lines
            .iter()
            .find(|line| line.id == "b-gone")
            .expect("the ended line is listed");
        assert_eq!(gone.state, LineState::EndedByOperation);
        assert_eq!(
            gone.running, None,
            "the ended line never sends a state report"
        );
        assert!(
            shown
                .lines
                .iter()
                .filter(|line| line.id == "main")
                .all(|line| line.running.is_some()),
            "main did report"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn live_unread_replies_follow_focus_and_attachment() {
        use rutis::host::CtxHost;
        use std::os::unix::fs::PermissionsExt;

        let home = tempfile::tempdir().unwrap();
        let endpoint = home.path().join("daemon.sock");
        let _listener = tokio::net::UnixListener::bind(&endpoint).unwrap();
        std::fs::set_permissions(&endpoint, std::fs::Permissions::from_mode(0o600)).unwrap();
        let client = Client::open(
            &dim_client::ConnectionConfig {
                endpoint,
                server: dim_client::Server {
                    program: std::env::current_exe().unwrap(),
                    embedded: false,
                },
                profile: None,
                client_kind: "cli",
                client_name: "dim-cli-test",
                protocol_version: 4,
            },
            false,
        )
        .await
        .unwrap();
        let options =
            Options::parse(["--home".into(), home.path().to_str().unwrap().to_owned()].into_iter())
                .unwrap();
        let (updates, receiver) = mpsc::channel();
        let mut worker = Worker::new(
            options,
            client,
            updates,
            rutis::Ctx::root().unwrap(),
            tokio::sync::oneshot::channel().0,
        );
        let snapshot = || {
            receiver
                .try_iter()
                .filter_map(|update| match update {
                    Update::Snapshot(view) => Some(view),
                    _ => None,
                })
                .last()
                .expect("the worker published a snapshot")
        };
        let create = json!({
            "id": "create", "sequence": 1, "create_at": 1000, "type": "branch", "name": "create",
            "data": {
                "at": {"kind": "sequence", "sequence": 0}, "branch": "b-7c", "prompt": "Check the parser",
                "source": {"kind": "tool", "turn_id": "t1", "request_id": "r1", "call_id": "c1"}
            }
        });
        let text_event = |sequence, delta| {
            json!({
                "id": format!("text-{sequence}"), "sequence": sequence, "create_at": 2000,
                "type": "step", "name": "text", "data": {"turn_id": "t2", "request_id": "r2", "delta": delta}
            })
        };
        let reply = |sequence, delta| Message::Notice {
            method: "session.event".into(),
            params: json!({"sessionId": "s1", "branchId": "b-7c", "event": text_event(sequence, delta)}),
        };

        worker.session_id = "s1".into();
        worker.apply("main", serde_json::from_value(create.clone()).unwrap());
        worker.message(reply(1, " ")).await.unwrap();
        worker.publish();
        assert!(snapshot().unread_lines.is_empty());

        worker
            .message(reply(2, "Parser issue found"))
            .await
            .unwrap();
        worker.publish();
        assert_eq!(snapshot().unread_lines, ["b-7c".into()].into());
        worker
            .command(Command::FocusLine("b-7c".into()))
            .await
            .unwrap();
        assert!(snapshot().unread_lines.is_empty());

        worker
            .command(Command::FocusLine("main".into()))
            .await
            .unwrap();
        worker
            .message(reply(2, "Parser issue found"))
            .await
            .unwrap();
        worker.publish();
        assert!(snapshot().unread_lines.is_empty());
        worker.message(reply(3, "More evidence")).await.unwrap();
        worker.publish();
        assert_eq!(snapshot().unread_lines, ["b-7c".into()].into());

        assert_eq!(worker.invalidate_projection(), "s1");
        worker.session_id = "s1".into();
        worker.apply("main", serde_json::from_value(create).unwrap());
        worker.apply(
            "b-7c",
            serde_json::from_value(text_event(3, "More evidence")).unwrap(),
        );
        worker.publish();
        assert!(snapshot().unread_lines.is_empty());
    }

    /// Hooks A.6.2: a successful submit reply that carries a check's warning
    /// becomes a warning report, which the interface commits as a status row
    /// in the warning colour.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_submit_replys_warning_becomes_a_warning_report() {
        use rutis::host::CtxHost;
        use std::os::unix::fs::PermissionsExt;

        let home = tempfile::tempdir().unwrap();
        let endpoint = home.path().join("daemon.sock");
        let _listener = tokio::net::UnixListener::bind(&endpoint).unwrap();
        std::fs::set_permissions(&endpoint, std::fs::Permissions::from_mode(0o600)).unwrap();
        let client = Client::open(
            &dim_client::ConnectionConfig {
                endpoint,
                server: dim_client::Server {
                    program: std::env::current_exe().unwrap(),
                    embedded: false,
                },
                profile: None,
                client_kind: "cli",
                client_name: "dim-cli-test",
                protocol_version: 4,
            },
            false,
        )
        .await
        .unwrap();
        let options =
            Options::parse(["--home".into(), home.path().to_str().unwrap().to_owned()].into_iter())
                .unwrap();
        let (updates, receiver) = mpsc::channel();
        let mut worker = Worker::new(
            options,
            client,
            updates,
            rutis::Ctx::root().unwrap(),
            tokio::sync::oneshot::channel().0,
        );
        let reply = json!({
            "event": {"id": "in-1", "sequence": 1, "create_at": 1000, "type": "input",
                "name": "message", "data": {}},
            "warnings": [{
                "check": "hooks",
                "message": "Hook user did not finish: exited with exit status: 3",
            }],
        });
        worker.report_value_warnings(&Ok(reply));
        let reported = receiver
            .try_iter()
            .find_map(|update| match update {
                Update::Report { text, warning } => Some((text, warning)),
                _ => None,
            })
            .expect("the warning became a report");
        assert_eq!(
            reported,
            (
                "Hook user did not finish: exited with exit status: 3".to_owned(),
                true
            )
        );
    }

    /// A `/runtime` change writes the configuration it carries and then
    /// applies it; an apply alone calls only `runtime.install`.
    #[test]
    fn a_runtime_change_asks_the_owner_once() {
        use crate::model::session::RuntimeSource;
        let cases = [
            (
                RuntimeAction::Install("node".into()),
                vec![("runtime.install", json!({"name": "node"}))],
            ),
            (
                RuntimeAction::Configure {
                    name: "node".into(),
                    source: Some(RuntimeSource::System),
                    auto: None,
                },
                vec![(
                    "runtime.configure",
                    json!({"name": "node", "source": "system"}),
                )],
            ),
            (
                RuntimeAction::Configure {
                    name: "python".into(),
                    source: None,
                    auto: Some(false),
                },
                vec![(
                    "runtime.configure",
                    json!({"name": "python", "auto": false}),
                )],
            ),
        ];
        for (action, calls) in cases {
            assert_eq!(runtime_calls(&action), calls, "{action:?}");
        }
    }

    /// The TUI creates a Session in the resolved workspace: the request
    /// carries exactly the directory the client resolved, never a rendering
    /// of it whose invalid bytes another path replaced.
    #[test]
    fn a_session_is_created_in_the_resolved_workspace() {
        let root = tempfile::tempdir().unwrap();
        let directory = std::fs::canonicalize(root.path()).unwrap();
        let mut options = crate::args::Options::parse(
            ["tui", "--cwd", directory.to_str().unwrap()]
                .into_iter()
                .map(str::to_owned),
        )
        .unwrap();
        options.resolve_workspace().unwrap();
        assert_eq!(
            session_create_params(&options)["cwd"],
            directory.to_str().unwrap()
        );
    }

    /// The three cases the catalog `cwd` maps: the exact launch workspace,
    /// the git set — even one entry, which the catalog canonicalizes to the
    /// same filter — and no filter for every workspace.
    #[test]
    fn the_catalog_asks_for_the_launch_workspace_the_git_set_or_everything() {
        assert_eq!(
            scope_cwd("/repo/app", &SessionsScope::Local(None)),
            Some(json!("/repo/app"))
        );
        let set = vec!["/repo/app".to_owned(), "/repo-fix/app".to_owned()];
        assert_eq!(
            scope_cwd("/repo/app", &SessionsScope::Local(Some(set.clone()))),
            Some(json!(set))
        );
        assert_eq!(
            scope_cwd(
                "/repo/app",
                &SessionsScope::Local(Some(vec!["/repo/app".to_owned()]))
            ),
            Some(json!(["/repo/app"]))
        );
        assert_eq!(scope_cwd("/repo/app", &SessionsScope::All), None);
    }

    /// One worktree, several, a bare entry, and a launch in a subdirectory:
    /// each worktree contributes the same relative directory, and the launch
    /// workspace appears once.
    #[test]
    fn the_worktree_set_holds_the_launch_workspace_and_each_sibling() {
        let one = b"worktree /repo\0HEAD 111\0branch refs/heads/main\0\0";
        assert_eq!(
            worktree_workspaces(one, std::path::Path::new(""), "/repo"),
            ["/repo"]
        );
        let several = b"worktree /repo\0HEAD 111\0branch refs/heads/main\0\0\
                        worktree /repo-fix\0HEAD 222\0branch refs/heads/fix\0\0";
        assert_eq!(
            worktree_workspaces(several, std::path::Path::new(""), "/repo"),
            ["/repo", "/repo-fix"]
        );
        let bare = b"worktree /repo\0bare\0\0";
        assert_eq!(
            worktree_workspaces(bare, std::path::Path::new(""), "/repo"),
            ["/repo"]
        );
        // The launch workspace is the first worktree joined with the relative
        // directory, so it is built the same way: `Path::join` gives the
        // platform's separator, and the launch workspace is listed once.
        let launch = std::path::Path::new("/repo").join("app");
        let sibling = std::path::Path::new("/repo-fix").join("app");
        assert_eq!(
            worktree_workspaces(
                several,
                std::path::Path::new("app"),
                launch.to_str().unwrap()
            ),
            [launch.to_str().unwrap(), sibling.to_str().unwrap()]
        );
    }

    /// The git read itself: one worktree answers one entry, a sibling
    /// worktree joins, and a directory outside git answers nothing.
    #[tokio::test]
    async fn the_git_read_answers_one_worktree_several_and_nothing_outside_git() {
        let root = tempfile::tempdir().unwrap();
        let repo = std::fs::canonicalize(root.path()).unwrap();
        assert!(
            repository_workspaces(repo.to_str().unwrap())
                .await
                .is_empty()
        );
        git(&repo, &["init", "-q"]);
        git(&repo, &["config", "user.email", "test@example.com"]);
        git(&repo, &["config", "user.name", "test"]);
        std::fs::write(repo.join("file"), "one\n").unwrap();
        git(&repo, &["add", "file"]);
        git(&repo, &["commit", "-qm", "one"]);
        assert_eq!(
            repository_workspaces(repo.to_str().unwrap()).await,
            [repo.to_str().unwrap().to_owned()]
        );
        let sibling_root = tempfile::tempdir().unwrap();
        let sibling = sibling_root.path().join("worktree");
        git(&repo, &["worktree", "add", sibling.to_str().unwrap()]);
        let sibling = std::fs::canonicalize(sibling).unwrap();
        assert_eq!(
            repository_workspaces(repo.to_str().unwrap()).await,
            [
                repo.to_str().unwrap().to_owned(),
                sibling.to_str().unwrap().to_owned()
            ]
        );
    }

    fn git(directory: &std::path::Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .current_dir(directory)
            .args(args)
            .stdin(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} failed");
    }
}

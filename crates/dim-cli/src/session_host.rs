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

use dim_agent_types::{ApprovalCallData, ApprovalDecision, ModelId};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

use crate::args::Options;
use crate::client::{self, Client, Message};
use crate::model::session::{
    ACCOUNT_NAMESPACE, Account, AccountDescriptor, Command, CompactReply, ContextReading,
    DIM_OAUTH_ID, DraftPart, EmptyProvider, EmptyReason, ExecutionSnapshot, ManagementReply,
    ModelItem, ModelList, ModelPolicyUpdate, NeedsLimitsProvider, OauthEvent, OauthStart,
    PluginItem, PluginReply, ProviderAction, ProviderEntry, SandboxBackend, SessionItem,
    SessionPort, SubagentModelPolicy, SwitchKind, Update, Usage,
};
use crate::model::view::{
    LineState, LineView, SideLine, TaskOutputState, TaskTarget, WorkflowInventory,
};
use crate::projection::Projection;
use crate::wire::{Configuration, Event, EventContent, Progress};
use crate::{configure, mcp, permission, update};

mod member;
mod tasks;
mod workflow;
use member::Members;
use tasks::{BranchHistory, HistoryRequest, StopBatch};
use workflow::WorkflowReads;

const PUBLISH_INTERVAL: Duration = Duration::from_millis(16);
const HISTORY_PAGE: u64 = 256;
const OUTPUT_INTERVAL: Duration = Duration::from_secs(1);
/// The complete Provider editing interface, without legacy write fallbacks.
const PROVIDER_METHODS: [&str; 4] = [
    "provider.management",
    "provider.create",
    "provider.update",
    "provider.probe",
];

struct ProviderSummary {
    provider_id: String,
    source: String,
    display_name: String,
    driver_kind: String,
    auth: serde_json::Value,
    base_url: String,
    credential_configured: bool,
    enabled: bool,
    version: u32,
    account: Option<AccountDescriptor>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ManagementEntry {
    provider_id: String,
    source: String,
    display_name: String,
    enabled: Option<bool>,
    version: Option<u32>,
    connection: Option<ManagementConnection>,
    account: Option<ManagementAccount>,
    preferences: std::collections::BTreeMap<String, serde_json::Value>,
    /// What the update did with the record's models; absent from the reads.
    #[serde(default)]
    models_listed: Option<ModelsListed>,
}
/// `provider.management`'s safe account descriptor.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ManagementAccount {
    namespace: String,
    login_methods: Vec<String>,
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
}
#[derive(Deserialize)]
struct ProviderManagement {
    providers: Vec<ManagementEntry>,
    errors: Vec<ManagementError>,
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
    /// took from the client tree.
    pub fn start(options: Options, client: Client) -> Self {
        let (commands, command_rx) = unbounded_channel();
        let (update_tx, updates) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("dim-session".into())
            .spawn(move || {
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
                runtime.block_on(run(options, client, command_rx, update_tx));
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
    (branch == "main").then_some(main)
}

/// The session the worker is attached to and what the screen shows of it.
struct Worker {
    options: Options,
    client: Client,
    updates: mpsc::Sender<Update>,
    session_id: String,
    /// The attached Session's workspace, which the management panels work on.
    /// A server older than the record leaves it unset, and the directory the
    /// client started in stands.
    session_cwd: Option<String>,
    /// The Session `/new` left: still attached until the next one is entered.
    parked: String,
    projection: Projection,
    question_responses: Vec<(u64, String, crate::question::Call)>,
    approval_details: std::collections::BTreeMap<u64, MemberDetailRead>,
    /// Highest event sequence applied; replayed or duplicate events are skipped.
    cursors: std::collections::HashMap<String, u64>,
    projection_id: u64,
    focus: String,
    /// The line the side panel follows.
    side: Option<String>,
    /// One transcript per line the user opened, folded from its events.
    lines: std::collections::BTreeMap<String, LineProjection>,
    task_history: std::collections::HashMap<String, BranchHistory>,
    task_history_queue: std::collections::VecDeque<String>,
    /// A stale request still occupies this slot until its reply arrives.
    task_history_request: Option<HistoryRequest>,
    task_stops: Option<StopBatch>,
    workflow: WorkflowReads,
    members: Members,
    output: Option<OutputWatch>,
    /// The admitted read can outlive its detail; false discards that old reply.
    output_request: Option<(u64, bool)>,
    catalog_request: Option<(u64, String, bool)>,
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
    /// The composer's context reading is out of date: a model request on main
    /// finished, a run on main ended, a compaction result arrived on main, or
    /// the session or its model changed.
    context_stale: bool,
    /// The last `model/usage` on main, folded from the events so `/usage`
    /// can report its cache read share.
    last_usage: Option<LastUsage>,
    /// The outstanding `session.context.compact` requests (`/compact`).
    compact_requests: Vec<u64>,
    /// The catalog version each provider was last asked to refill at in this
    /// connection, moved to the version the refresh stored: an endpoint that
    /// lists nothing costs one request per unchanged record, not one per open,
    /// and a write to the record (a re-point, an enable) asks once more.
    refilled_catalogs: std::collections::HashMap<String, u32>,
    /// The outstanding `provider.refresh` requests `models()` sent for those
    /// catalogs, with the provider each was sent for.
    refill_requests: Vec<(u64, String)>,
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

struct Login {
    /// The panel's attempt tag for its updates.
    tag: u64,
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
    mut commands: UnboundedReceiver<Command>,
    updates: mpsc::Sender<Update>,
) {
    if update::enabled() {
        let offers = updates.clone();
        let home = options.home.clone();
        tokio::spawn(async move {
            if let Some(offer) = update::available(&home, env!("CARGO_PKG_VERSION")).await {
                let _ = offers.send(Update::Upgrade(offer));
            }
        });
    }
    let mut worker = Worker {
        options,
        client,
        updates: updates.clone(),
        session_id: String::new(),
        session_cwd: None,
        parked: String::new(),
        projection: Projection::default(),
        question_responses: Vec::new(),
        approval_details: Default::default(),
        cursors: Default::default(),
        projection_id: 0,
        focus: "main".into(),
        side: None,
        lines: Default::default(),
        task_history: Default::default(),
        task_history_queue: Default::default(),
        task_history_request: None,
        task_stops: None,
        workflow: WorkflowReads::default(),
        members: Members::default(),
        output: None,
        output_request: None,
        catalog_request: None,
        publish_at: None,
        shell: None,
        ui_id: None,
        shell_stops: Vec::new(),
        model: None,
        mcp_attempt: None,
        mcp_callbacks: unbounded_channel(),
        mcp_required: Default::default(),
        login: None,
        login_callbacks: unbounded_channel(),
        context_stale: false,
        last_usage: None,
        compact_requests: Vec::new(),
        refilled_catalogs: Default::default(),
        refill_requests: Vec::new(),
    };
    worker.send(Update::Permissions(Ok(dim_agent_types::PermissionPolicy {
        preset: worker
            .options
            .permission
            .unwrap_or(dim_agent_types::PermissionPreset::WorkspaceWrite),
        file_access: worker.options.file_access,
        tool_approval: worker.options.tool_approval,
        ..Default::default()
    })));
    if let Err(error) = worker.open().await {
        if worker.options.resume.is_some() {
            let _ = updates.send(Update::Failed(error.to_string()));
            return;
        }
        let _ = updates.send(Update::Notice(no_session(&error)));
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
        let workflow_poll = worker
            .workflow
            .next_at
            .filter(|_| worker.workflow.request.is_none());
        let login_poll = worker.login.as_ref().and_then(|login| login.poll_at);
        let output_poll = worker
            .output
            .as_ref()
            .and_then(|output| output.poll_at)
            .filter(|_| worker.output_request.is_none());
        tokio::select! {
            command = commands.recv() => {
                let Some(command) = command else { break };
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
            Some(callback) = worker.login_callbacks.1.recv() => worker.login_callback(callback).await,
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
            _ = tokio::time::sleep_until(worker.publish_at.unwrap_or_else(tokio::time::Instant::now)),
                if worker.publish_at.is_some() => worker.publish(),
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Created {
    session_id: String,
    model: ModelId,
}

/// The record `session.attach` replies for the Session it opened. A server
/// from before the record existed replies `{}`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Attached {
    #[serde(default)]
    model: Option<ModelId>,
    /// The adopted configuration's preferences; absent when it carries none.
    #[serde(default)]
    model_preferences: Option<dim_agent_types::ModelPreferences>,
    /// The Session's workspace; absent from servers older than the record.
    #[serde(default)]
    cwd: Option<String>,
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
struct History {
    events: Vec<Event>,
    through: u64,
    next_after: u64,
    done: bool,
}

impl History {
    fn decode(value: serde_json::Value, after: u64, through: Option<u64>) -> Result<Self, String> {
        let page: Self = serde_json::from_value(value)
            .map_err(|error| format!("Invalid history reply: {error}"))?;
        if through.is_some_and(|through| through != page.through)
            || page.through > dim_agent_types::JS_MAX_SAFE_INTEGER
            || page.next_after < after
            || page.next_after > page.through
            || (!page.done && page.next_after == after)
            || (page.done && page.next_after != page.through)
        {
            return Err("Invalid history page cursor".into());
        }
        let mut sequence = after;
        for event in &page.events {
            if event.sequence <= sequence || event.sequence > page.next_after {
                return Err("History event is outside its page or unordered".into());
            }
            sequence = event.sequence;
        }
        Ok(page)
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct EventNotice {
    session_id: String,
    branch_id: String,
    event: Event,
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
    async fn cancel_catalog(&mut self) -> crate::Result<()> {
        if let Some((request, _, _)) = self.catalog_request.take()
            && self.client.offers("session.catalog.cancel")
        {
            self.client
                .send("session.catalog.cancel", json!({"requestId":request}))
                .await?;
        }
        Ok(())
    }

    /// Creates or resumes the session the process was started for.
    async fn open(&mut self) -> crate::Result<()> {
        if let Some(id) = self.options.resume.clone() {
            let kind = if self.options.created_session {
                SwitchKind::New
            } else {
                SwitchKind::Resumed
            };
            return self.attach(id, kind).await;
        }
        let attached_session = self.invalidate_projection();
        let mut params = self.create_params();
        let mut preferences = None;
        let mut target = None;
        if let (Some(provider), Some(model)) =
            (self.options.provider.clone(), self.options.model.clone())
        {
            params["model"] = json!({"provider": provider, "modelId": model});
            preferences = saved_model_preferences(&mut self.client, &provider, &model).await?;
            if let Some(preferences) = &preferences {
                params["modelPreferences"] = preferences.clone();
            }
            target = Some((provider, model));
        }
        let created: Created = match self.client.typed("session.create", params).await {
            Ok(created) => created,
            Err(error) => {
                return Err(crate::model_adoption_error(
                    &mut self.client,
                    target.as_ref().map(|(p, m)| (p.as_str(), m.as_str())),
                    error,
                    preferences.as_ref(),
                )
                .await);
            }
        };
        self.enter(
            created.session_id,
            Some(created.model),
            SwitchKind::New,
            attached_session,
        )
        .await
    }

    /// Creates the Session the first message from home starts, on the model
    /// the status line shows.
    async fn start_session(&mut self) -> crate::Result<()> {
        let attached_session = self.invalidate_projection();
        let mut params = self.create_params();
        let mut preferences = None;
        let mut target = None;
        let result = async {
            if let Some(model) = &self.model {
                params["model"] = json!({"provider": model.provider_id, "modelId": model.model_id});
                let (provider, model) = (model.provider_id.clone(), model.model_id.clone());
                preferences = saved_model_preferences(&mut self.client, &provider, &model).await?;
                if let Some(preferences) = &preferences {
                    params["modelPreferences"] = preferences.clone();
                }
                target = Some((provider, model));
            }
            match self.client.typed::<Created>("session.create", params).await {
                Ok(created) => Ok(created),
                Err(error) => Err(crate::model_adoption_error(
                    &mut self.client,
                    target.as_ref().map(|(p, m)| (p.as_str(), m.as_str())),
                    error,
                    preferences.as_ref(),
                )
                .await),
            }
        }
        .await;
        match result {
            Ok(created) => {
                self.enter(
                    created.session_id,
                    Some(created.model),
                    SwitchKind::New,
                    attached_session,
                )
                .await
            }
            Err(error) => {
                self.parked = attached_session;
                Err(error)
            }
        }
    }

    /// `session.create` parameters: the workspace and, when one is chosen,
    /// the permission preset, which the Session's branches and members inherit.
    fn create_params(&self) -> serde_json::Value {
        let mut params = json!({"cwd": self.options.cwd});
        if let Some(policy) = permission::creation_policy(&self.options) {
            params["permissionPolicy"] = policy;
        }
        params
    }

    async fn enter(
        &mut self,
        session_id: String,
        model: Option<ModelId>,
        kind: SwitchKind,
        attached_session: String,
    ) -> crate::Result<()> {
        if let (SwitchKind::Resumed, Some(preset)) = (kind, self.options.permission)
            && self.client.supports(&permission::PRESET_METHODS)
            && let Err(error) = permission::select(&mut self.client, &session_id, preset).await
        {
            self.send(Update::Notice(format!(
                "The permission preset was not applied: {error}"
            )));
        }
        if !attached_session.is_empty() && attached_session != session_id {
            self.client
                .rpc("session.detach", json!({"sessionId": attached_session}))
                .await?;
            self.ui_id = None;
        }
        let attached: Attached = self
            .client
            .typed("session.attach", json!({"sessionId": session_id}))
            .await?;
        // Without a record, find the model in the catalog: this is the only
        // caller of `list()` besides `/resume`, and only for an older server.
        let model_preferences = attached.model_preferences;
        let model = match model.or(attached.model) {
            Some(model) => model,
            None => self.model_of(&session_id).await?,
        };
        let configuration = Configuration {
            provider_id: model.provider_id.clone(),
            model_id: model.model_id.clone(),
            model_preferences,
        };
        self.session_id = session_id.clone();
        self.session_cwd = attached.cwd;
        self.register_ui().await;
        self.mcp_required.clear();
        self.model = Some(configuration.clone());
        self.discover_branch("main");
        self.poll_task_history().await?;
        let features = self.model_features(&model).await;
        self.send(Update::Ready(configuration));
        self.send(features);
        if self.client.supports(&permission::PRESET_METHODS) {
            let policy = permission::read(&mut self.client, &session_id)
                .await
                .map(|state| state.policy)
                .map_err(|error| format!("permission: unavailable ({error})"));
            self.send(Update::Permissions(policy));
        }
        self.send(Update::Switched { session_id, kind });
        self.context_stale = true;
        self.publish();
        Ok(())
    }

    /// Whether the model takes image input, from its catalog entry:
    /// `vision`, or `image` among its modalities. `None` when the server
    /// has no catalog or it does not answer in time.
    /// The model's catalog entry as [`Update::ModelFeatures`]. Unknown
    /// capabilities leave images as they are, sent as images, and offer
    /// no service tiers.
    async fn model_features(&mut self, model: &ModelId) -> Update {
        let entry = self.catalog_entry(model).await;
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

    async fn catalog_entry(&mut self, model: &ModelId) -> Option<CatalogModel> {
        if !self.client.offers("provider.catalog") {
            return None;
        }
        #[derive(Deserialize)]
        struct Snapshot {
            catalog: Option<Catalog>,
        }
        #[derive(Deserialize)]
        struct Catalog {
            models: Vec<CatalogModel>,
        }
        let request = self
            .client
            .typed::<Snapshot>("provider.catalog", json!({"providerId": model.provider_id}));
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

    async fn attach(&mut self, session_id: String, kind: SwitchKind) -> crate::Result<()> {
        let attached_session = self.invalidate_projection();
        self.enter(session_id, None, kind, attached_session).await
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
        self.report_failure(result);
    }

    /// Stores the draft's pastes as attachments, then submits its text
    /// and blocks in order.
    async fn submit_draft(&mut self, parts: Vec<DraftPart>, mode: &str) {
        use dim_agent_types::ContentBlock;
        let mut content = Vec::new();
        let mut text = String::new();
        let mut pastes = 0;
        for part in parts {
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
                            self.send(Update::Notice(format!("Paste {pastes}: {error}; not sent")));
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
        self.report_failure(result);
    }

    async fn command(&mut self, command: Command) -> crate::Result<()> {
        if self.session_id.is_empty()
            && matches!(command, Command::Submit(..) | Command::SubmitDraft(..))
            && let Err(error) = self.start_session().await
        {
            self.send(Update::Notice(no_session(&error)));
            return Ok(());
        }
        match command {
            Command::Submit(text, mode) => self.submit(text, mode.as_str()).await,
            Command::SubmitDraft(parts, mode) => self.submit_draft(parts, mode.as_str()).await,
            Command::FocusLine(line) => self.focus_line(line).await,
            Command::CloseSide => {
                self.side = None;
                self.publish();
            }
            Command::EndLine {
                session_id,
                projection_id,
                branch_id,
            } => {
                if branch_id == "main" {
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
                    self.discover_branch(&opened.branch.branch_id);
                    self.side = Some(opened.branch.branch_id.clone());
                    self.publish();
                }
                self.report_failure(opened);
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
                    Ok(forked) => self.attach(forked.session_id, SwitchKind::Forked).await?,
                    Err(error) => self.send(Update::Notice(error.to_string())),
                }
            }
            Command::Stop => {
                let params = json!({
                    "sessionId": self.session_id,
                    "branchId": self.focus,
                    "reason": crate::model::view::INTERRUPT_REASON,
                });
                let result = self.client.rpc("session.stop", params).await;
                self.report_failure(result);
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
            // Home, not a Session: the first message creates one.
            Command::NewSession => {
                if !self.session_id.is_empty() {
                    self.parked = self.invalidate_projection();
                    self.send(Update::Permissions(Ok(dim_agent_types::PermissionPolicy {
                        preset: self
                            .options
                            .permission
                            .unwrap_or(dim_agent_types::PermissionPreset::WorkspaceWrite),
                        file_access: self.options.file_access,
                        tool_approval: self.options.tool_approval,
                        ..Default::default()
                    })));
                    self.send(Update::Context(None));
                    self.send(Update::Switched {
                        session_id: String::new(),
                        kind: SwitchKind::New,
                    });
                }
            }
            Command::Resume(session_id) => {
                self.attach(session_id, SwitchKind::Resumed).await?;
            }
            Command::ListSessions {
                id,
                query,
                cursor,
                show_acp,
            } => {
                self.cancel_catalog().await?;
                let paged = self.client.offers("session.catalog");
                let (method, params) = if paged {
                    let mut params = json!({"limit":50,"query":query,"withPreview":true});
                    if show_acp && self.client.offers("session.catalog.origins") {
                        params["withAcpSessions"] = json!(true);
                    }
                    if let Some(cursor) = cursor {
                        params["cursor"] = cursor.into();
                    }
                    ("session.catalog", params)
                } else {
                    ("session.summaries", json!({}))
                };
                match self.client.send(method, params).await {
                    Ok(request) => self.catalog_request = Some((request, id, paged)),
                    Err(error) => self.send(Update::Sessions {
                        id,
                        paged,
                        result: Err(error.to_string()),
                    }),
                }
            }
            Command::CloseSessions => self.cancel_catalog().await?,
            Command::Usage => {
                let reading = self.context_usage().await;
                // Pages land oldest first: until `main`'s history has
                // finished loading, an earlier page's older request is not
                // the newest, so the segment waits for `done`.
                let loaded = self
                    .task_history
                    .get("main")
                    .is_some_and(|history| history.done);
                let cached = self
                    .last_usage
                    .filter(|_| loaded)
                    .and_then(|usage| usage.percent());
                match reading {
                    Ok(usage) => {
                        let mut line = usage.describe();
                        if let Some(cached) = cached {
                            line.push_str(" · last request cached ");
                            line.push_str(&cached);
                        }
                        self.report(line, false)
                    }
                    Err(error) => self.report(error.to_string(), true),
                }
                if self
                    .model
                    .as_ref()
                    .is_some_and(|model| model.provider_id == DIM_OAUTH_ID)
                {
                    let usage = self.account_usage(DIM_OAUTH_ID).await;
                    self.send(Update::UsageReport(usage));
                }
            }
            Command::Compact => self.compact().await,
            Command::Goal(request) => self.goal(request).await,
            Command::ToggleWeb => {
                let result: crate::Result<WebToggled> =
                    self.client.typed("web.toggle", json!({})).await;
                match result {
                    Ok(mut state) => {
                        let url = state.url.take().map(|url| self.web_url(&url));
                        let browser = state.enabled
                            && state.open
                            && url.as_deref().is_some_and(crate::browser::open);
                        state.url = (state.enabled && state.open && !browser)
                            .then_some(url)
                            .flatten();
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
                let models = self.models().await.map_err(|error| error.to_string());
                self.send(Update::Models(models));
                self.read_model_policy(false).await;
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
                // The reread lands before the notice: a conflict tells the
                // user to reread and retry, so the row a retry presses must
                // already carry the record's current version.
                let models = self.models().await.map_err(|error| error.to_string());
                self.send(Update::Models(models));
                self.send(Update::Notice(match result {
                    Ok(()) => {
                        "Saved model preferences cleared; select the model to adopt its defaults"
                            .into()
                    }
                    Err(error) => error.to_string(),
                }));
            }
            Command::SetPermission(change) => {
                let result = if self.session_id.is_empty() {
                    let mut policy = dim_agent_types::PermissionPolicy {
                        preset: self
                            .options
                            .permission
                            .unwrap_or(dim_agent_types::PermissionPreset::WorkspaceWrite),
                        file_access: self.options.file_access,
                        tool_approval: self.options.tool_approval,
                        ..Default::default()
                    };
                    change.apply(&mut policy);
                    Ok(policy)
                } else {
                    let session_id = self.session_id.clone();
                    permission::select_control(&mut self.client, &session_id, change)
                        .await
                        .map_err(|error| error.to_string())
                };
                if result.is_ok() {
                    match change {
                        permission::Change::FileAccess(value) => {
                            self.options.file_access = Some(value)
                        }
                        permission::Change::ToolApproval(value) => {
                            self.options.tool_approval = Some(value)
                        }
                    }
                    self.send(Update::PermissionSelected(change));
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
            Command::UpgradeInstall(install) => {
                let updates = self.updates.clone();
                tokio::spawn(async move {
                    let version = install.version.clone();
                    let result = update::install(install).await;
                    let _ = updates.send(Update::UpgradeInstalled { version, result });
                });
            }
            Command::OauthPaste(url) => self.login_complete(url, None).await,
            Command::LoginStatus(provider_id) => {
                let account = self
                    .account(&provider_id)
                    .await
                    .map_err(|error| error.to_string());
                self.send(Update::Login(account));
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
            } => {
                if self.session_id.is_empty() {
                    self.send(Update::ShellDone {
                        run_id,
                        result: Err(NO_SESSION.to_owned()),
                    });
                    return Ok(());
                }
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
            Command::Management(request) => {
                // The Session's workspace, not the directory the client
                // started in: a resumed Session may live elsewhere.
                let cwd = self
                    .session_cwd
                    .as_deref()
                    .map(std::path::PathBuf::from)
                    .unwrap_or_else(|| self.options.cwd.clone());
                let result = crate::management::apply(&mut self.client, &request, &cwd).await;
                self.send(Update::Management(ManagementReply { request, result }));
            }
            Command::Configure(text) => {
                if self.session_id.is_empty() && configure::needs_session(&text) {
                    self.report(NO_SESSION.to_owned(), true);
                    return Ok(());
                }
                let model = self.model.clone();
                match configure::run(&mut self.client, &self.session_id, model.as_ref(), &text)
                    .await
                {
                    Ok(report) => self.report(report.lines.join("\n"), false),
                    Err(error) => self.report(error.to_string(), true),
                }
            }
            Command::SelectModel {
                provider_id,
                model_id,
                reasoning_effort,
            } => {
                if let Err(error) = self
                    .select_model(provider_id, model_id, reasoning_effort)
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
                let main = stop_turn && branch_id != "main";
                for branch in std::iter::once(branch_id.as_str()).chain(main.then_some("main")) {
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
                    "branchId": "main",
                    "reason": "stopped by the user",
                });
                let result = self.client.rpc("session.stop", params).await;
                self.report_failure(result);
            }
            Command::InputControl { input_id, action } => {
                let mut params = json!({
                    "sessionId": self.session_id,
                    "branchId": self.focus,
                    "inputId": input_id,
                });
                if action == "steer" {
                    let user = self.shown().held_input(&input_id).cloned();
                    let Some(user) = user else {
                        self.send(Update::Notice(QUEUE_LEFT.into()));
                        return Ok(());
                    };
                    params["steer"] = json!(user);
                }
                #[derive(Deserialize)]
                struct Withdrawn {
                    status: String,
                }
                match self
                    .client
                    .typed::<Withdrawn>("session.input.withdraw", params)
                    .await
                {
                    Ok(withdrawn) if withdrawn.status == "notHeld" => {
                        self.send(Update::Notice(QUEUE_LEFT.into()))
                    }
                    Ok(withdrawn) if withdrawn.status == "deleted" && action == "withdraw" => self
                        .send(Update::Withdrawn {
                            input_id: input_id.clone(),
                        }),
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

    /// The main branch's context window as `session.context.usage`
    /// estimates it.
    async fn context_usage(&mut self) -> crate::Result<ContextUsage> {
        self.client
            .typed(
                "session.context.usage",
                json!({"sessionId": self.session_id, "branchId": "main"}),
            )
            .await
            .map_err(
                |error| match error.downcast_ref::<client::RpcError>().and_then(app_error) {
                    Some("context_not_configured") => {
                        "Context usage is known once a model has answered in this session".into()
                    }
                    Some("context_model_failed") => {
                        "Context usage needs the model's capabilities; select the model again"
                            .into()
                    }
                    Some("unknown_method") => "This daemon does not track context usage".into(),
                    _ => error,
                },
            )
    }

    /// Refreshes the composer's context reading once it is stale; a session
    /// whose usage is unknown shows none.
    async fn poll_context(&mut self) {
        if !std::mem::take(&mut self.context_stale) || self.session_id.is_empty() {
            return;
        }
        let reading = self.context_usage().await.ok().map(|usage| ContextReading {
            input: usage.input(),
            window: usage.window(),
            threshold: tokens(usage.threshold_tokens),
        });
        self.send(Update::Context(reading));
    }

    /// `/compact`: asks the shown line's context to compact. The reply is
    /// reported; the transcript row comes from the events (compaction
    /// requirements §4).
    async fn compact(&mut self) {
        let params = json!({"sessionId": self.session_id, "branchId": self.focus});
        match self.client.send("session.context.compact", params).await {
            Ok(request) => self.compact_requests.push(request),
            Err(error) => self.send(Update::Compacted(CompactReply::Failed(error.to_string()))),
        }
    }

    /// `/goal`: the shown line's goal, written by the method the request
    /// names. The line is read before the call is chosen, so a fold the
    /// interface has not caught up with cannot pick the wrong method; a
    /// write is followed by the events it produced, so the report shows the
    /// state after it.
    async fn goal(&mut self, request: crate::goal::Request) {
        let branch = self.focus.clone();
        if !crate::goal::available(&mut self.client, &self.session_id).await {
            self.report(crate::goal::NOT_AVAILABLE.into(), true);
            return;
        }
        if let Err(error) = self.read_branch(&branch).await {
            self.report(format!("Goal: {error}"), true);
            return;
        }
        let call = crate::goal::call(&request, self.shown().goal().as_ref());
        if let Some(call) = call {
            let mut params = json!({"sessionId": self.session_id, "branchId": branch});
            if let Some(objective) = crate::goal::objective(&request) {
                params["objective"] = json!(objective);
            }
            if let Err(error) = self.client.rpc(call.method(), params).await {
                let text = match crate::client::app_error(&error) {
                    Some("unknown_method") => crate::goal::NOT_AVAILABLE.to_owned(),
                    _ => format!("Goal: {error}"),
                };
                self.report(text, true);
                return;
            }
            if let Err(error) = self.read_branch(&branch).await {
                self.report(format!("Goal: {error}"), true);
                return;
            }
        }
        let text = crate::goal::report(&branch, call, self.shown().goal().as_ref()).join("\n");
        self.report(text, false);
    }

    /// Applies the portion of `branch` the interface has not folded yet, from
    /// its applied sequence to the published head.
    async fn read_branch(&mut self, branch: &str) -> crate::Result<()> {
        let mut after = self.cursors.get(branch).copied().unwrap_or(0);
        let mut through = None;
        loop {
            let mut params = json!({
                "sessionId": self.session_id, "branchId": branch,
                "after": after, "limit": HISTORY_PAGE,
            });
            if let Some(through) = through {
                params["through"] = json!(through);
            }
            let page: crate::wire::History = self.client.typed("session.history", params).await?;
            through = Some(page.through);
            let next = page.next(after, through)?;
            for value in page.events {
                self.apply(branch, serde_json::from_value(value)?);
            }
            match next {
                Some(next) => after = next,
                None => return Ok(()),
            }
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

    /// Registers this interface on the Session with the daemon's UI registry
    /// (Tui design §3.6), when the daemon keeps one; a failure to register
    /// is a notice, since the Session works without it.
    async fn register_ui(&mut self) {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Attached {
            ui_id: String,
        }
        if !self.client.offers("ui.attach") {
            return;
        }
        let mut params = json!({"sessionId": self.session_id, "kind": "tui"});
        if let Some(identity) = &self.options.ui_identity {
            params["uiId"] = json!(identity.ui_id);
            params["token"] = json!(identity.token);
        }
        match self.client.typed::<Attached>("ui.attach", params).await {
            Ok(attached) => self.ui_id = Some(attached.ui_id),
            Err(error) => self.send(Update::Notice(format!(
                "Interface registration failed: {error}"
            ))),
        }
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
                if let Some(read) = self.approval_details.remove(&id) {
                    self.member_approval_reply(read, result).await?;
                    return Ok(());
                }
                self.reply(id, result)?;
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
            "session.event" => {
                let notice: EventNotice = serde_json::from_value(params)?;
                if notice.session_id == self.session_id {
                    // Live only: history pages arrive as `session.history`
                    // replies applied by `apply`, so they spend no read.
                    if notice.branch_id == "main"
                        && matches!(notice.event.content, EventContent::ModelEnd(_))
                    {
                        self.context_stale = true;
                    }
                    self.task_event(&notice.branch_id, notice.event);
                } else if notice.branch_id == "main" && self.observes_member(&notice.session_id) {
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
                } else if notice.branch_id == "main" && self.observes_member(&notice.session_id) {
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
                    self.context_stale |= notice.branch_id == "main" && !notice.running;
                    self.changed();
                } else if notice.branch_id == "main" && self.observes_member(&notice.session_id) {
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
    /// `/mcp logout ID`, `/mcp enable ID`, `/mcp disable ID`.
    async fn mcp(&mut self, args: &str) -> crate::Result<()> {
        const USAGE: &str = "Usage: /mcp [list|login [--no-browser] ID|callback URL|logout ID|enable ID|disable ID]";
        let words: Vec<&str> = args.split_whitespace().collect();
        match words.as_slice() {
            [] | ["list"] => {
                let (rows, notes) = mcp::catalog_report(&mut self.client, &self.options).await?;
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
                let ids: Vec<_> = rest.iter().filter(|word| !word.starts_with("--")).collect();
                let [id] = ids.as_slice() else {
                    return Err(USAGE.into());
                };
                self.mcp_login(id, no_browser).await?;
            }
            ["callback", url] => {
                let Some(attempt) = &self.mcp_attempt else {
                    return Err("No MCP sign-in is waiting; run /mcp login ID".into());
                };
                let attempt_id = attempt.attempt_id.clone();
                self.mcp_complete(attempt_id, url.to_string(), None).await;
            }
            ["logout", id] => {
                #[derive(Deserialize)]
                struct LoggedOut {
                    revoked: bool,
                }
                let params = mcp::target(&mut self.client, &self.options, id).await?;
                let result: LoggedOut = self.client.typed("mcp.auth.logout", params).await?;
                self.report(
                    format!("Signed out of {id:?}. Revoked: {}.", result.revoked),
                    false,
                );
            }
            // The ID is the rest of the line, spaces included.
            ["enable", ..] | ["disable", ..] => {
                let (command, id) = args
                    .trim()
                    .split_once(char::is_whitespace)
                    .unwrap_or((args.trim(), ""));
                let id = id.trim();
                if id.is_empty() {
                    return Err(USAGE.into());
                }
                let (rows, notes) = crate::mcp_command::set_enabled(
                    &mut self.client,
                    &self.options,
                    id,
                    command == "enable",
                )
                .await?;
                let mut lines = rows;
                lines.extend(notes);
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

    async fn mcp_login(&mut self, id: &str, no_browser: bool) -> crate::Result<()> {
        self.end_mcp_attempt();
        let params = mcp::target(&mut self.client, &self.options, id).await?;
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

    /// The `/connect` list: every static provider the App Server knows (the
    /// bundled catalog's and custom ones), as the panel shows them.
    async fn providers(&mut self) -> Result<Vec<ProviderEntry>, String> {
        let current = self.model.as_ref().map(|model| model.provider_id.clone());
        let listed = self.provider_summaries().await.map_err(|e| e.to_string())?;
        Ok(listed
            .into_iter()
            .map(|summary| ProviderEntry {
                status: if !summary.enabled {
                    "disabled"
                } else if summary.credential_configured || summary.auth["type"] == "anonymous" {
                    "connected"
                } else {
                    "disconnected"
                }
                .into(),
                is_current: current.as_deref() == Some(summary.provider_id.as_str()),
                enabled: summary.enabled,
                custom: summary.source == "custom",
                base_url: Some(summary.base_url).filter(|url| !url.is_empty()),
                api_key_preview: summary.credential_configured.then(|| "••••••••".into()),
                id: summary.provider_id,
                version: summary.version,
                name: summary.display_name,
                driver: summary.driver_kind,
                account: summary.account,
            })
            .collect())
    }

    async fn provider_summaries(&mut self) -> crate::Result<Vec<ProviderSummary>> {
        if !self.client.supports(&PROVIDER_METHODS) {
            return Err("This daemon cannot manage providers".into());
        }
        let listed: ProviderManagement =
            self.client.typed("provider.management", json!({})).await?;
        for error in listed.errors {
            self.send(Update::Notice(format!(
                "{}: {}",
                error.provider_id, error.message
            )));
        }
        listed
            .providers
            .into_iter()
            .filter_map(|entry| {
                entry.connection.map(|connection| {
                    Ok(ProviderSummary {
                        provider_id: entry.provider_id,
                        source: entry.source,
                        display_name: entry.display_name,
                        driver_kind: connection.driver_kind,
                        auth: connection.auth,
                        base_url: connection.base_url,
                        credential_configured: connection.credential_configured,
                        enabled: entry
                            .enabled
                            .ok_or("Static Provider enable flag is missing")?,
                        version: entry
                            .version
                            .ok_or("Static Provider record version is missing")?,
                        account: entry.account.map(|account| AccountDescriptor {
                            namespace: account.namespace,
                            login_methods: account.login_methods,
                        }),
                    })
                })
            })
            .collect()
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
            if let Some(existing) = self.provider_summaries().await?.into_iter().find(|provider| provider.provider_id == provider_id) {
                return Err(format!("Provider {} already exists; review the retained draft and press Apply again", existing.display_name).into());
            }
            let connection = json!({"driverKind":"openai-compatible", "auth":{"type":"bearer"}, "baseUrl":base_url, "credential":{"action":"set", "value":api_key}});
            let probed: Probed = self.client.typed("provider.probe", json!({"providerId":provider_id,"connection":connection})).await?;
            self.client.rpc("provider.create", json!({"providerId":provider_id,"displayName":name,"connection":connection,"models":probed.models,"enabled":true})).await?;
            Ok(format!("Added {name}"))
        }.await;
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
            let updated: ManagementEntry = self.client.typed("provider.update", json!({"providerId":provider_id,"expectedVersion":expected_version,"patch":patch})).await?;
            let mut message = format!("{notice} {}", updated.display_name);
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

    /// The selectable models: every enabled static provider with
    /// credentials and every signed-in managed provider (`provider.list`
    /// lists static ones only), less unavailable and disabled models.
    ///
    /// A static provider whose stored catalog holds no models is asked to
    /// refill it through `provider.refresh` when `provider.list` says its
    /// models can be listed, once per connection. The request is sent, not
    /// awaited: the picker opens with what the catalogs hold, a provider
    /// still listing none shows its `no models` row meanwhile, and the reply
    /// asks the open picker to read the list again. Every usable provider
    /// that offers no selectable model is reported for its picker row, with
    /// the reason and whether `provider.refresh` can list it.
    async fn models(&mut self) -> crate::Result<ModelList> {
        #[derive(Deserialize)]
        struct Providers {
            providers: Vec<Provider>,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Provider {
            provider_id: String,
            display_name: String,
            /// Absent from a daemon that cannot say, and such a daemon cannot
            /// be asked to list a provider's models either.
            #[serde(default)]
            remote_model_list: bool,
            enabled: bool,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Managed {
            provider_id: String,
            display_name: String,
        }
        #[derive(Deserialize)]
        struct Account {
            account: Option<serde_json::Value>,
        }
        #[derive(Deserialize)]
        struct Snapshot {
            catalog: Option<Catalog>,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Catalog {
            version: u32,
            models: Vec<CatalogModel>,
            disabled_model_ids: Vec<String>,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct CatalogModel {
            id: String,
            name: String,
            unavailable_reason: Option<String>,
            #[serde(default)]
            missing_limits: Vec<String>,
            #[serde(default)]
            zdr: bool,
            #[serde(default)]
            capabilities: dim_agent_types::CatalogCapabilities,
            #[serde(default)]
            metadata: Option<CatalogReasoning>,
        }
        #[derive(Deserialize, Default)]
        #[serde(rename_all = "camelCase")]
        struct CatalogReasoning {
            reasoning: Option<dim_agent_types::ModelReasoning>,
        }
        let listed: Providers = self.client.typed("provider.list", json!({})).await?;
        let mut usable: Vec<(String, String, bool)> = Vec::new();
        // `provider.refresh` lists a static provider only; an account
        // catalog is filled by its account owner.
        let mut refillable = std::collections::HashSet::new();
        for provider in listed
            .providers
            .into_iter()
            .filter(|provider| provider.enabled)
        {
            if provider.remote_model_list {
                refillable.insert(provider.provider_id.clone());
            }
            usable.push((
                provider.provider_id,
                provider.display_name,
                provider.remote_model_list,
            ));
        }
        let managed: Vec<Managed> = self
            .client
            .typed("account.providers", json!({}))
            .await
            .unwrap_or_default();
        for provider in managed {
            let status = self
                .client
                .typed::<Account>(
                    "account.status",
                    json!({"providerId": provider.provider_id}),
                )
                .await;
            if status.is_ok_and(|status| status.account.is_some()) {
                // A managed catalog is refreshed from its account owner.
                usable.push((provider.provider_id, provider.display_name, true));
            }
        }
        let mut models = Vec::new();
        let mut empty = Vec::new();
        let mut limits = Vec::new();
        for (provider_id, provider_name, listable) in usable {
            let snapshot = self
                .client
                .typed::<Snapshot>("provider.catalog", json!({"providerId": provider_id}))
                .await;
            let catalog = match snapshot {
                Ok(Snapshot { catalog }) => catalog,
                Err(_) => {
                    empty.push(EmptyProvider {
                        provider_id,
                        provider_name,
                        reason: EmptyReason::Unreadable,
                    });
                    continue;
                }
            };
            let version = catalog.as_ref().map_or(0, |catalog| catalog.version);
            if catalog
                .as_ref()
                .is_none_or(|catalog| catalog.models.is_empty())
                && refillable.contains(&provider_id)
                && self.refilled_catalogs.insert(provider_id.clone(), version) != Some(version)
                && let Ok(request) = self
                    .client
                    .send("provider.refresh", json!({"providerId": provider_id}))
                    .await
            {
                self.refill_requests.push((request, provider_id.clone()));
            }
            let Some(catalog) = catalog else {
                empty.push(EmptyProvider {
                    provider_id,
                    provider_name,
                    reason: EmptyReason::NoModels { listable },
                });
                continue;
            };
            let preferences = self.saved_preferences(&provider_id).await;
            let total = catalog.models.len();
            let undeclared = catalog
                .models
                .iter()
                .filter(|model| {
                    crate::limits_undeclared(
                        &model.missing_limits,
                        model.unavailable_reason.as_deref(),
                        catalog.disabled_model_ids.contains(&model.id),
                    )
                })
                .count();
            let unavailable = catalog
                .models
                .iter()
                .filter(|model| model.unavailable_reason.is_some())
                .count();
            let disabled = catalog
                .models
                .iter()
                .filter(|model| {
                    model.unavailable_reason.is_none()
                        && catalog.disabled_model_ids.contains(&model.id)
                })
                .count();
            let before = models.len();
            models.extend(
                catalog
                    .models
                    .into_iter()
                    .filter(|model| {
                        model.unavailable_reason.is_none()
                            && !catalog.disabled_model_ids.contains(&model.id)
                    })
                    .map(|model| {
                        let (efforts, default_effort) = effort_choices(
                            &model.capabilities,
                            model
                                .metadata
                                .as_ref()
                                .and_then(|meta| meta.reasoning.as_ref()),
                        );
                        let saved_effort = preferences.get(&model.id).and_then(|value| {
                            value
                                .get("reasoningEffort")
                                .and_then(|effort| effort.as_str())
                                .filter(|effort| efforts.iter().any(|option| option == *effort))
                                .map(str::to_owned)
                        });
                        ModelItem {
                            provider_id: provider_id.clone(),
                            provider_name: provider_name.clone(),
                            provider_version: catalog.version,
                            model_id: model.id,
                            display_name: model.name,
                            zdr: model.zdr,
                            efforts,
                            default_effort,
                            saved_effort,
                        }
                    }),
            );
            if undeclared > 0 {
                limits.push(NeedsLimitsProvider {
                    provider_id: provider_id.clone(),
                    provider_name: provider_name.clone(),
                    count: undeclared,
                });
            }
            if models.len() == before {
                let reason = if total == 0 {
                    EmptyReason::NoModels { listable }
                } else if unavailable > 0 && disabled > 0 {
                    EmptyReason::Mixed {
                        unavailable,
                        disabled,
                    }
                } else if unavailable > 0 {
                    EmptyReason::Unavailable { count: unavailable }
                } else {
                    EmptyReason::Disabled { count: disabled }
                };
                empty.push(EmptyProvider {
                    provider_id,
                    provider_name,
                    reason,
                });
            }
        }
        let current = self
            .model
            .as_ref()
            .map(|model| (model.provider_id.clone(), model.model_id.clone()));
        Ok(ModelList {
            models,
            current,
            empty,
            limits,
        })
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

    /// Saved preferences for the picker. Empty when management is absent or
    /// the read fails, so the catalog list still shows.
    async fn saved_preferences(
        &mut self,
        provider_id: &str,
    ) -> std::collections::BTreeMap<String, serde_json::Value> {
        if !self.client.offers("provider.management") {
            return std::collections::BTreeMap::new();
        }
        self.client
            .typed::<ProviderManagement>("provider.management", json!({"providerId": provider_id}))
            .await
            .ok()
            .and_then(|snapshot| snapshot.providers.into_iter().next())
            .map(|provider| provider.preferences)
            .unwrap_or_default()
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
            .typed("provider.management", json!({"providerId": provider_id}))
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

    /// Switches the session's model, or at home the one the first message
    /// starts a session on, then makes it the default for new ones.
    /// A cycled `reasoning_effort` is saved first, then sent with the model's
    /// other saved preferences.
    async fn select_model(
        &mut self,
        provider_id: String,
        model_id: String,
        reasoning_effort: Option<String>,
    ) -> crate::Result<()> {
        #[derive(Deserialize)]
        struct Updated {
            model: ModelId,
        }
        if let Some(effort) = &reasoning_effort {
            self.save_reasoning_effort(&provider_id, &model_id, effort)
                .await?;
        }
        let mut preferences =
            saved_model_preferences(&mut self.client, &provider_id, &model_id).await?;
        if let Some(effort) = &reasoning_effort {
            let value = preferences.get_or_insert_with(|| json!({}));
            if let Some(object) = value.as_object_mut() {
                object.insert("reasoningEffort".to_owned(), json!(effort));
            }
        }
        let model = if self.session_id.is_empty() {
            let model = ModelId {
                provider_id,
                model_id,
            };
            let features = self.model_features(&model).await;
            let configuration = Configuration {
                provider_id: model.provider_id.clone(),
                model_id: model.model_id.clone(),
                model_preferences: None,
            };
            self.model = Some(configuration.clone());
            self.send(Update::Ready(configuration));
            self.send(features);
            model
        } else {
            let target = (provider_id.clone(), model_id.clone());
            let mut params = json!({"sessionId":self.session_id,"branchId":"main","providerId":provider_id,"modelId":model_id});
            if let Some(preferences) = &preferences {
                params["modelPreferences"] = preferences.clone();
            }
            let updated: Updated = match self.client.typed("session.model.update", params).await {
                Ok(updated) => updated,
                Err(error) => {
                    return Err(crate::model_adoption_error(
                        &mut self.client,
                        Some((target.0.as_str(), target.1.as_str())),
                        error,
                        preferences.as_ref(),
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
                provider_id: updated.model.provider_id.clone(),
                model_id: updated.model.model_id.clone(),
                model_preferences,
            };
            self.model = Some(configuration.clone());
            self.send(Update::Ready(configuration));
            self.send(features);
            self.context_stale = true;
            updated.model
        };
        if let Err(error) = self.make_default(&model).await {
            self.send(Update::Notice(format!(
                "The model was not saved as the default: {error}"
            )));
        }
        Ok(())
    }

    /// Saves the global default model over the selection read just before,
    /// as `provider.select` requires its current version.
    async fn make_default(&mut self, model: &ModelId) -> crate::Result<()> {
        #[derive(Deserialize)]
        struct Current {
            selection: Option<Selection>,
        }
        #[derive(Deserialize)]
        struct Selection {
            version: u32,
        }
        let current: Current = self.client.typed("provider.selection", json!({})).await?;
        let mut params = json!({"providerId": model.provider_id, "modelId": model.model_id});
        if let Some(selection) = current.selection {
            params["expectedVersion"] = json!(selection.version);
        }
        self.client.rpc("provider.select", params).await?;
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
            if managed {
                params["redirectUri"] = json!(self.options.redirect_uri);
            }
            match self.client.typed::<Browser>(&method, params).await {
                Ok(browser) => {
                    let local = std::env::var_os("SSH_CONNECTION").is_none()
                        && std::env::var_os("SSH_TTY").is_none();
                    let capture = if local && managed {
                        mcp::Capture::bind(&self.options.redirect_uri).await
                    } else {
                        None
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

    /// Refreshes the provider's catalog, so `/models` lists its models at
    /// once. The account namespace owns a cached catalog; a native OAuth
    /// namespace's provider keeps the bundled one.
    async fn login_done(&mut self, login: Login) {
        if login.namespace == ACCOUNT_NAMESPACE
            && let Err(error) = self
                .client
                .rpc("provider.refresh", json!({"providerId": login.provider_id}))
                .await
        {
            self.send(Update::Notice(format!(
                "Model catalog refresh failed: {error}"
            )));
        }
        self.login_event(login.tag, OauthEvent::Done(Ok(login.provider_id.clone())));
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
    fn reply(
        &mut self,
        id: u64,
        result: Result<serde_json::Value, client::RpcError>,
    ) -> crate::Result<()> {
        if self
            .catalog_request
            .as_ref()
            .is_some_and(|(request, _, _)| *request == id)
        {
            let (_, id, paged) = self
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
                }
                Ok((
                    sessions
                        .into_iter()
                        .filter(|session| session.preview.is_some())
                        .map(|session| SessionItem {
                            id: session.session_id,
                            title: session.title,
                            preview: session.preview,
                            cwd: session.cwd,
                            updated_at: session.updated_at,
                            model: session.model.model_id,
                            origin: session.origin,
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
            self.member_reply(id, result);
            return Ok(());
        }
        if self
            .task_history_request
            .as_ref()
            .is_some_and(|request| request.id == id)
        {
            return self.task_history_reply(result);
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
            self.send(Update::Compacted(compact_reply(result)));
            return Ok(());
        }
        if let Some(index) = self
            .refill_requests
            .iter()
            .position(|(request, _)| *request == id)
        {
            let (_, provider_id) = self.refill_requests.remove(index);
            if let Ok(value) = result {
                // The refresh stored the catalog under a new version; record
                // it, so a listing that came back empty is not asked for
                // again. A picker that is still open reads the list again to
                // show the fill; with the picker closed this only records it.
                #[derive(Deserialize)]
                struct Stored {
                    version: u32,
                }
                if let Ok(Stored { version }) = serde_json::from_value(value) {
                    self.refilled_catalogs.insert(provider_id, version);
                }
                self.send(Update::ModelsRefilled);
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

    fn apply(&mut self, branch_id: &str, event: Event) {
        self.discover_task_branches(&event);
        if let Some(line) = self.lines.get_mut(branch_id)
            && event.sequence > line.cursor
        {
            line.cursor = event.sequence;
            line.projection.apply(branch_id, &event);
        }
        let cursor = self.cursors.entry(branch_id.to_owned()).or_default();
        if event.sequence <= *cursor {
            return;
        }
        *cursor = event.sequence;
        self.discover_workflow(&event);
        if branch_id == "main"
            && let EventContent::Configuration(configuration) = &event.content
        {
            self.model = Some(configuration.clone());
            self.send(Update::Ready(configuration.clone()));
        }
        if branch_id == "main"
            && let EventContent::PermissionChanged(data) = &event.content
        {
            self.send(Update::Permissions(Ok(data.policy.clone())));
        }
        if branch_id == "main" && matches!(event.content, EventContent::Compaction(_)) {
            self.context_stale = true;
        }
        if branch_id == "main"
            && let EventContent::ModelUsage(data) = &event.content
        {
            self.last_usage = Some(LastUsage {
                prompt_tokens: tokens(data.usage.prompt_tokens),
                cache_read_tokens: data.usage.cache_read_tokens.map(tokens),
            });
        }
        self.projection.apply(branch_id, &event);
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
        let mut lines = vec![LineView {
            id: "main".into(),
            parent: None,
            purpose: String::new(),
            state: LineState::Main,
            cut: None,
        }];
        lines.extend(
            self.projection
                .line_views()
                .iter()
                .cloned()
                .map(|mut line| {
                    if line.state == LineState::Waiting
                        && self
                            .task_history
                            .get(&line.id)
                            .and_then(|history| history.running)
                            == Some(true)
                    {
                        line.state = LineState::Working;
                    }
                    line
                }),
        );
        lines
    }

    async fn focus_line(&mut self, line: String) {
        if line != "main" && !self.task_history.contains_key(&line) {
            self.send(Update::Notice(format!("No line {line} in this session")));
            return;
        }
        if line != "main" {
            self.line_projection(&line);
        }
        self.focus = line;
        self.side = None;
        self.publish();
    }

    /// The transcript the screen shows: the focused line's, or `main`'s.
    fn shown(&self) -> &Projection {
        self.lines
            .get(&self.focus)
            .filter(|_| self.focus != "main")
            .map_or(&self.projection, |line| &line.projection)
    }

    /// Schedules a snapshot a frame interval after the first change.
    fn changed(&mut self) {
        self.publish_at
            .get_or_insert_with(|| tokio::time::Instant::now() + PUBLISH_INTERVAL);
    }

    fn publish(&mut self) {
        self.publish_at = None;
        let mut snapshot = match self.lines.get(&self.focus).filter(|_| self.focus != "main") {
            Some(line) => line.projection.snapshot(),
            None => self.projection.snapshot(),
        };
        snapshot.lines = self.line_views();
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
        snapshot.focus = self.focus.clone();
        snapshot.main_running = self.projection.running();
        snapshot.main_topic = self.projection.first_message();
        if self.focus != "main" {
            snapshot.approvals = self.projection.approvals();
            snapshot.questions = self.projection.questions();
            snapshot.tasks = self.projection.tasks().to_vec();
        }
        snapshot.session_id = self.session_id.clone();
        snapshot.projection_id = self.projection_id;
        snapshot.history_complete = self
            .task_history
            .get(&self.focus)
            .is_some_and(|branch| branch.done);
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
async fn answer_browser(mut stream: tokio::net::TcpStream, completed: bool) {
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

/// One model of `provider.catalog`, as far as the interface reads it.
#[derive(Deserialize)]
struct CatalogModel {
    id: String,
    capabilities: dim_agent_types::CatalogCapabilities,
    metadata: Option<CatalogPreferences>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CatalogPreferences {
    service_tier: Option<dim_agent_types::ModelServiceTier>,
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
    let mut efforts = reasoning.effort_options.clone().unwrap_or_default();
    if reasoning.off.is_some() && !efforts.iter().any(|effort| effort == "none") {
        efforts.push("none".into());
    }
    let default = reasoning
        .effort
        .clone()
        .filter(|effort| efforts.iter().any(|option| option == effort));
    (efforts, default)
}

pub(crate) async fn saved_model_preferences(
    client: &mut Client,
    provider_id: &str,
    model_id: &str,
) -> crate::Result<Option<serde_json::Value>> {
    if !client.offers("provider.management") {
        return Ok(None);
    }
    let snapshot: ProviderManagement = client
        .typed("provider.management", json!({"providerId":provider_id}))
        .await?;
    let provider = snapshot
        .providers
        .into_iter()
        .next()
        .ok_or("Provider management entry is missing")?;
    Ok(provider.preferences.get(model_id).cloned())
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
        .typed("provider.management", json!({"providerId": provider_id}))
        .await?;
    let version = snapshot
        .providers
        .into_iter()
        .next()
        .ok_or("Provider management entry is missing")?
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
    #[derive(Deserialize)]
    struct Snapshot {
        catalog: Option<Catalog>,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Catalog {
        version: u32,
        #[serde(default)]
        disabled_model_ids: Vec<String>,
        models: Vec<CatalogModel>,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct CatalogModel {
        id: String,
        #[serde(default)]
        missing_limits: Vec<String>,
        #[serde(default)]
        unavailable_reason: Option<String>,
    }
    let snapshot: Snapshot = client
        .typed("provider.catalog", json!({"providerId": provider_id}))
        .await?;
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
fn compact_reply(result: Result<serde_json::Value, client::RpcError>) -> CompactReply {
    #[derive(Deserialize)]
    #[serde(tag = "status", rename_all = "snake_case")]
    enum Reply {
        Completed {},
        Skipped { reason: String },
    }
    match result {
        Ok(value) => match serde_json::from_value::<Reply>(value) {
            Ok(Reply::Completed { .. }) => CompactReply::Completed,
            Ok(Reply::Skipped { reason }) => CompactReply::Skipped(reason),
            Err(_) => CompactReply::Failed("Invalid session.context.compact response".into()),
        },
        Err(error) => match app_error(&error) {
            Some("unknown_method") => CompactReply::Unsupported,
            Some("compaction_cancelled") => CompactReply::Cancelled(error.message),
            _ => CompactReply::Failed(error.message),
        },
    }
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

#[cfg(test)]
mod tests {
    use super::*;

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
}

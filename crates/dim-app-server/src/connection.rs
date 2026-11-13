use crate::{
    ServerConfig,
    agents::{AgentIndex, DeleteOutcome, SessionUse},
    control::ServerControl,
    outbound::{Admission, Frame, Sender},
    protocol::*,
    skills_management::skills_error,
};
use dim_agent_capabilities::{
    methods::{Accepted, CallSite, CallerConnection, Callers, NotifyError, RpcError},
    permission::{Permission, PermissionState, PermissionUpdateError},
    skills::SkillsBranch,
    workspace_tools::background::{BackgroundExec, TaskError},
};
use dim_agent_runtime::{AgentError, Names, Session, TimelineQueryDirection, TimelineQueryScope};
use dim_agent_types::{ContentBlock, InputMessageData, UserInput};
mod mcp;
mod subscription;
mod tree;
mod ui_extensions;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, Key, TypeKey};
use rutis::{Ctx, Error};
use rutis_bridge::session::{
    Error as BridgeError, HostDispatch, Reply as BridgeReply, Value as BridgeValue, caller_row,
};
use serde::de::DeserializeOwned;
use serde_json::Map;
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock, RwLock, Weak,
        atomic::{AtomicU64, Ordering},
    },
};
use subscription::Attachment;
use tokio_util::{sync::CancellationToken, task::TaskTracker};

const MAX_REQUESTS: usize = 64;
const CONTROL_RESERVE: usize = 8;

/// The channel a connection declared at `hello`, for presence only.
#[derive(Clone, Debug)]
pub(crate) struct ClientIdentity {
    pub channel: dim_agent_capabilities::environment_clients::ClientChannel,
    pub label: Option<String>,
}

/// The entry a connection was accepted on; it decides which methods Rpc serves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Entry {
    /// Owner-only Unix socket or named pipe.
    Local,
    /// The unauthenticated `/ws` of `--ws`.
    Bare,
    /// The authenticated local web entry: Session methods and allowlisted
    /// management methods.
    Web,
}

pub(crate) struct Connection {
    pub id: String,
    pub version: OnceLock<u32>,
    /// The protocol instance this connection completed hello with.
    pub instance: OnceLock<String>,
    /// The channel declared by `hello`; informational, never durable.
    pub client: OnceLock<ClientIdentity>,
    pub entry: Entry,
    hub: Arc<ConnectionHub>,
    pub closed: CancellationToken,
    pub sender: Sender,
    pub shutdown_reply: Mutex<Option<RequestId>>,
    ids: Mutex<HashMap<RequestId, Option<CancellationToken>>>,
    attachments: Mutex<HashMap<String, Attachment>>,
    /// Serializes attach, detach and finish so one retirement per connection
    /// is in flight and the window an attachment lives in is private.
    transitions: tokio::sync::Mutex<()>,
    next_attachment: AtomicU64,
}
struct Inflight {
    connection: Arc<Connection>,
    id: RequestId,
}
impl Drop for Inflight {
    fn drop(&mut self) {
        self.connection
            .ids
            .lock()
            .expect("request identities")
            .remove(&self.id);
    }
}

/// The plain writer and close signal of one client connection (methods
/// contract A.2): what a foreign notification whose call has finished uses.
/// It holds the connection weakly, so a kept handle does not keep it alive.
pub(crate) struct DirectConnection {
    id: String,
    connection: Weak<Connection>,
    closed: CancellationToken,
    unloading: CancellationToken,
}
impl DirectConnection {
    fn new(connection: &Arc<Connection>, unloading: &CancellationToken) -> Arc<Self> {
        Arc::new(Self {
            id: connection.id.clone(),
            connection: Arc::downgrade(connection),
            closed: connection.closed.clone(),
            unloading: unloading.clone(),
        })
    }
    /// The encoded frame, after the connection check and encoded once through
    /// the sender's admission, without queueing it: a call's sink holds it
    /// until its reply.
    fn frame(
        &self,
        method: &str,
        params: serde_json::Map<String, Value>,
    ) -> Result<Frame, NotifyError> {
        let Some(connection) = self.connection.upgrade() else {
            return Err(NotifyError::Gone);
        };
        if self.closed.is_cancelled() || self.unloading.is_cancelled() {
            return Err(NotifyError::Gone);
        }
        connection
            .sender
            .hold(Outbound::message(method.to_owned(), params))
            .map_err(|admission| match admission {
                Admission::TooLarge => NotifyError::TooLarge,
                Admission::Full => NotifyError::Gone,
            })
    }
    fn write(&self, frame: Frame) -> Result<(), NotifyError> {
        let Some(connection) = self.connection.upgrade() else {
            return Err(NotifyError::Gone);
        };
        if connection.sender.push(frame) {
            Ok(())
        } else {
            Err(NotifyError::Gone)
        }
    }
}
impl CallerConnection for DirectConnection {
    fn id(&self) -> &str {
        &self.id
    }
    fn notify(
        &self,
        method: &str,
        params: serde_json::Map<String, Value>,
    ) -> Result<(), NotifyError> {
        self.write(self.frame(method, params)?)
    }
    fn closed(&self) -> BoxFuture<'static, ()> {
        let (closed, unloading) = (self.closed.clone(), self.unloading.clone());
        Box::pin(async move {
            tokio::select! {
                _ = closed.cancelled() => {}
                _ = unloading.cancelled() => {}
            }
        })
    }
}

/// The calling connection of one admitted call (methods contract A.2):
/// `callChannel.notify` and `closed` for the provider that serves it.
///
/// Every notification is written after the reply of the call that produced
/// its `Caller`: one sent while the call runs is encoded once and admitted by
/// the connection's sender here, then goes out right after the reply, in
/// order. It is a reliable frame: a call holding more than the connection's
/// reliable allowance is a slow consumer and the connection closes, while a
/// notification over the frame limit is `TooLarge` and the connection stays
/// open.
pub(crate) struct CallSink {
    direct: Arc<DirectConnection>,
    state: Mutex<CallState>,
}
enum CallState {
    /// Frames sent before the reply was written, in order: encoded and
    /// holding their room already.
    Held(Vec<Frame>),
    /// The reply is written; later frames go straight to the connection.
    Released,
}
impl CallSink {
    fn new(connection: &Arc<Connection>, unloading: &CancellationToken) -> Arc<Self> {
        Arc::new(Self {
            direct: DirectConnection::new(connection, unloading),
            state: Mutex::new(CallState::Held(Vec::new())),
        })
    }
    fn direct(&self) -> Arc<DirectConnection> {
        self.direct.clone()
    }
    /// The reply is written: everything held follows it, in order. A
    /// notification sent from here on is written directly.
    fn release(&self) {
        let mut state = self.state.lock().expect("held notifications");
        if let CallState::Held(frames) = &mut *state {
            for frame in frames.drain(..) {
                let _ = self.direct.write(frame);
            }
        }
        *state = CallState::Released;
    }
}
impl CallerConnection for CallSink {
    fn id(&self) -> &str {
        self.direct.id()
    }
    fn notify(
        &self,
        method: &str,
        params: serde_json::Map<String, Value>,
    ) -> Result<(), NotifyError> {
        let frame = self.direct.frame(method, params)?;
        let mut state = self.state.lock().expect("held notifications");
        match &mut *state {
            CallState::Held(frames) => frames.push(frame),
            CallState::Released => {
                drop(state);
                self.direct.write(frame)?;
            }
        }
        Ok(())
    }
    fn closed(&self) -> BoxFuture<'static, ()> {
        self.direct.closed()
    }
}
impl Connection {
    pub fn new(
        hub: Arc<ConnectionHub>,
        entry: Entry,
    ) -> (Arc<Self>, tokio::sync::mpsc::UnboundedReceiver<Frame>) {
        let closed = hub.closing.child_token();
        let (sender, receiver) = Sender::new(closed.clone());
        let connection = Arc::new(Self {
            id: uuid::Uuid::new_v4().to_string(),
            version: OnceLock::new(),
            instance: OnceLock::new(),
            client: OnceLock::new(),
            entry,
            closed,
            sender,
            shutdown_reply: Mutex::new(None),
            hub,
            ids: Mutex::new(HashMap::new()),
            attachments: Mutex::new(HashMap::new()),
            transitions: tokio::sync::Mutex::new(()),
            next_attachment: AtomicU64::new(1),
        });
        connection
            .hub
            .connections
            .lock()
            .expect("connection index")
            .insert(connection.id.clone(), Arc::downgrade(&connection));
        (connection, receiver)
    }
    pub fn is_attached(&self, id: &str) -> bool {
        self.attachments
            .lock()
            .expect("attachments")
            .contains_key(id)
    }
    /// The channel `hello` declared; `unknown` before hello or for an
    /// unrecognized `other` client.
    pub fn channel(&self) -> dim_agent_capabilities::environment_clients::ClientChannel {
        self.client.get().map_or(
            dim_agent_capabilities::environment_clients::ClientChannel::Unknown,
            |identity| identity.channel,
        )
    }
    pub fn label(&self) -> Option<String> {
        self.client
            .get()
            .and_then(|identity| identity.label.clone())
    }
    pub async fn finish(&self) {
        self.closed.cancel();
        // The connection closed: no call of any row can name it again.
        let _ = self
            .hub
            .with_rpc(|rpc| rpc.callers.close_connection(&self.id));
        if self.entry == Entry::Local {
            self.hub.web.release(&self.hub, &self.id).await;
        }
        self.hub.ui.release(&self.id);
        self.hub.ui.finished(&self.id);
        // Every attachment settles before the connection ends: a detached
        // connection leaves no listener, task or usage behind for the
        // Session's next attach.
        let _transition = self.transitions.lock().await;
        let (sessions, attachments): (Vec<String>, Vec<Attachment>) = {
            let mut attachments = self.attachments.lock().expect("attachments");
            let sessions = attachments.keys().cloned().collect();
            (
                sessions,
                attachments
                    .drain()
                    .map(|(_, attachment)| attachment)
                    .collect(),
            )
        };
        for session in sessions {
            self.hub.notify_clients(&session);
        }
        for attachment in attachments {
            attachment.retire().await;
        }
        drop(_transition);
        self.hub
            .connections
            .lock()
            .expect("connection index")
            .remove(&self.id);
    }
    /// Admits a request for the registered RPC service; none is registered
    /// while that service reloads, which the client may retry. The command is
    /// tracked before the handler lock is released, so an unloading service
    /// drains every command it admitted.
    pub fn dispatch(self: &Arc<Self>, request: Request) {
        let id = request.id.clone();
        let admitted = self.hub.with_rpc(|rpc| {
            if self.closed.is_cancelled()
                || self.instance.get().map(String::as_str) != Some(rpc.index.server_instance_id())
            {
                return Err(RpcError::reloading());
            }
            let guard = self.admit(&request)?;
            self.admit_entry(rpc, &request.call)?;
            let activity = if matches!(request.call, Call::Shutdown(_)) {
                None
            } else {
                Some(
                    rpc.control
                        .activity
                        .take_hold()
                        .ok_or_else(RpcError::reloading)?,
                )
            };
            let this = self.clone();
            let Request { id, call } = request;
            if let Call::Contributed { method, params } = call {
                if self.protocol_version() >= 3 && session_candidate(rpc, &method, &params) {
                    if rpc.stopping.is_cancelled() {
                        return Err(RpcError::reloading());
                    }
                    let rpc = rpc.clone();
                    let progress = this.progress(&id, &method);
                    let sink = CallSink::new(&this, &rpc.unloading);
                    let site = CallSite {
                        connection: sink.clone(),
                        direct: sink.direct(),
                        callers: rpc.callers.clone(),
                        closed: this.closed.clone(),
                    };
                    rpc.commands.clone().spawn(async move {
                        let _activity = activity;
                        match this
                            .session_method(&rpc, method, params, site, progress)
                            .await
                        {
                            Ok((accepted, usage)) => {
                                this.answer(id, guard, accepted, sink).await;
                                drop(usage);
                            }
                            Err(error) => {
                                let _ = this.sender.send(Outbound::result(id, Err(error)), false);
                            }
                        }
                    });
                    return Ok(());
                }
                if self.protocol_version() < 3 || self.entry == Entry::Bare {
                    return Err(RpcError::app(
                        -32601,
                        "unknown_method",
                        "Management method is unavailable",
                    ));
                }
                if rpc.stopping.is_cancelled() {
                    return Err(RpcError::reloading());
                }
                let progress = this.progress(&id, &method);
                let sink = CallSink::new(&this, &rpc.unloading);
                let site = CallSite {
                    connection: sink.clone(),
                    direct: sink.direct(),
                    callers: rpc.callers.clone(),
                    closed: this.closed.clone(),
                };
                let accepted = crate::methods::dispatch(
                    &rpc.management,
                    &rpc.loader,
                    method,
                    params,
                    Some(site),
                    rpc.unloading.clone(),
                    progress,
                )?;
                rpc.commands.spawn(async move {
                    let _activity = activity;
                    this.answer(id, guard, accepted, sink).await;
                });
                return Ok(());
            }
            let rpc = rpc.clone();
            let shutdown =
                matches!(call, Call::Shutdown(_)).then(|| self.hub.shutdown_requests.token());
            rpc.commands.clone().spawn(async move {
                let _activity = activity;
                let _guard = guard;
                let _shutdown = shutdown;
                let result = this.handle(&rpc, id.clone(), call).await;
                if let Some(result) = result {
                    let _ = this.sender.send(Outbound::result(id, result), false);
                }
            });
            Ok(())
        });
        if let Err(error) = admitted.and_then(|admitted| admitted) {
            let _ = self.sender.send(Outbound::result(id, Err(error)), false);
        }
    }
    /// Sends an admitted contributed call's reply on this connection, then
    /// releases the call's held notifications behind it. The id is the
    /// client's again once answered. An interrupted operation may still
    /// commit: the calling load's unload waits for it, so the next load reads
    /// its outcome.
    async fn answer(
        self: Arc<Self>,
        id: RequestId,
        guard: Inflight,
        accepted: Accepted,
        sink: Arc<CallSink>,
    ) {
        let Accepted {
            reply,
            settled,
            registration,
        } = accepted;
        let result = reply
            .await
            .unwrap_or_else(|_| Err(RpcError::wire(-32603, "Method operation failed")))
            .map(Reply::Contributed);
        let _ = self.sender.send(Outbound::result(id, result), false);
        sink.release();
        // The reply is written: the foreign call no longer holds
        // notifications, so its index entry ends here, not at handler return.
        if let Some(registration) = registration {
            registration.finish();
        }
        drop(guard);
        let _ = settled.await;
    }
    /// The progress sink of contributed call `id` (methods contract A.2):
    /// `management.progress {id, method, data}` on this connection only, best
    /// effort. Progress the connection cannot take, also after a disconnect,
    /// is dropped; it never closes the connection or holds back the reply.
    fn progress(
        self: &Arc<Self>,
        id: &RequestId,
        method: &str,
    ) -> impl Fn(Value) + Send + Sync + 'static {
        let (connection, id, method) = (self.clone(), id.clone(), method.to_owned());
        move |data| {
            connection
                .sender
                .offer(Outbound::notice(Notification::Progress(ProgressNotice {
                    id: id.clone(),
                    method: method.clone(),
                    data,
                })));
        }
    }
    /// Admits a Session method call (methods contract A.1): reads `sessionId`
    /// from the raw params, authorizes it like a built-in, acquires the
    /// Session (restoring an unloaded one) and hands the call to the provider
    /// the Session's method table holds for it. The returned use holds the
    /// Session for the duration of the call; `sessionId` stays in the params.
    async fn session_method(
        &self,
        rpc: &RpcService,
        method: String,
        params: Value,
        site: CallSite,
        progress: impl Fn(Value) + Send + Sync + 'static,
    ) -> Result<(Accepted, SessionUse), RpcError> {
        let session_id = params
            .get("sessionId")
            .and_then(Value::as_str)
            .ok_or_else(|| RpcError::invalid("sessionId is required"))?
            .to_owned();
        if !rpc.config.session_execution {
            return Err(integration_pending());
        }
        self.authorize(rpc, &session_id)?;
        let usage = rpc.index.acquire(&session_id).await?;
        let provider = crate::methods::session_provider(&rpc.loader, &usage.session, &method)?;
        if rpc.stopping.is_cancelled() {
            return Err(RpcError::reloading());
        }
        let accepted =
            provider.accept(method, params, Some(site), rpc.unloading.clone(), progress)?;
        Ok((accepted, usage))
    }
    /// The connection's entry decides what it may call before any method is
    /// routed, for built-in and contributed methods alike: protected profiles
    /// serve only `hello` and `daemon.ping` to the bare WebSocket, and the web
    /// entry serves Session methods plus explicitly permitted management.
    fn admit_entry(&self, rpc: &RpcService, call: &Call) -> Result<(), RpcError> {
        match self.entry {
            Entry::Local => Ok(()),
            Entry::Web => {
                let allowed = match call {
                    Call::Shutdown(_) => false,
                    Call::Contributed { method, params } => {
                        if matches!(method.as_str(), "settings.read" | "settings.update") {
                            params
                                .get("source")
                                .and_then(Value::as_str)
                                .is_some_and(|source| WEB_SETTINGS_SOURCES.contains(&source))
                        } else {
                            !rpc.management.holds(method)
                                || WEB_MANAGEMENT_METHODS.contains(&method.as_str())
                        }
                    }
                    call => !call.is_web_entry() && !call.is_ui_host(),
                };
                if allowed {
                    Ok(())
                } else {
                    Err(RpcError::app(
                        -32601,
                        "unknown_method",
                        "Method is unavailable on the web entry",
                    ))
                }
            }
            Entry::Bare
                if rpc.config.protected_profile()
                    && !matches!(call, Call::Hello(_) | Call::Ping) =>
            {
                Err(RpcError::app(
                    -32000,
                    "local_transport_required",
                    "Execution and management require a protected local connection",
                ))
            }
            Entry::Bare => Ok(()),
        }
    }
    fn admit(self: &Arc<Self>, request: &Request) -> Result<Inflight, RpcError> {
        let mut ids = self.ids.lock().expect("request identities");
        if ids.contains_key(&request.id) {
            return Err(RpcError::invalid("Request id is already in flight"));
        }
        let control = matches!(
            request.call,
            Call::Stop(_)
                | Call::Close(_)
                | Call::Delete(_)
                | Call::AgentClose(_)
                | Call::ExecStop(_)
                | Call::Detach(_)
                | Call::ApprovalRespond(_)
                | Call::Shutdown(_)
                | Call::CatalogCancel(_)
        );
        if ids.len() >= MAX_REQUESTS + if control { CONTROL_RESERVE } else { 0 } {
            return Err(RpcError::app(
                -32000,
                "connection_busy",
                "Connection request limit reached",
            ));
        }
        let cancel = matches!(request.call, Call::Catalog(_) | Call::Summaries(_))
            .then(|| self.closed.child_token());
        ids.insert(request.id.clone(), cancel);
        Ok(Inflight {
            connection: self.clone(),
            id: request.id.clone(),
        })
    }
    fn session(&self, rpc: &RpcService, id: &str) -> Result<Session, RpcError> {
        rpc.index
            .session(id)
            .map(|(_, session)| session)
            .ok_or_else(RpcError::closed)
    }
    fn protocol_version(&self) -> u32 {
        *self.version.get().expect("handshake precedes requests")
    }
    fn validate_call(&self, rpc: &RpcService, call: &Call) -> Result<(), RpcError> {
        if call.is_web_entry() || call.is_ui_host() {
            if self.protocol_version() < 3 {
                return Err(RpcError::app(
                    -32601,
                    "unknown_method",
                    "Method requires protocol v3",
                ));
            }
            if self.entry != Entry::Local {
                return Err(RpcError::app(
                    -32000,
                    "local_transport_required",
                    "The web entry is managed from a protected local connection",
                ));
            }
            return Ok(());
        }
        if call.is_execution_session() {
            let available = rpc.config.local_execution();
            if self.protocol_version() < 3 || !available {
                return Err(RpcError::app(
                    -32601,
                    "unknown_method",
                    "Management method is unavailable",
                ));
            }
            if self.entry == Entry::Bare {
                return Err(RpcError::app(
                    -32000,
                    "local_transport_required",
                    "Product management requires a protected local connection",
                ));
            }
        }
        if !rpc.config.session_execution
            && !matches!(
                call,
                Call::Hello(_)
                    | Call::Ping
                    | Call::Shutdown(_)
                    | Call::Summaries(_)
                    | Call::Catalog(_)
                    | Call::CatalogCancel(_)
                    | Call::AgentList
            )
        {
            return Err(integration_pending());
        }
        if self.protocol_version() < 3
            && matches!(
                call,
                Call::InputLookup(_)
                    | Call::History(_)
                    | Call::HistoryDisplay(..)
                    | Call::HistoryDetail(_)
                    | Call::Branches(_)
                    | Call::BranchGet(_)
                    | Call::Keys(_)
                    | Call::Summaries(_)
                    | Call::Catalog(_)
                    | Call::CatalogCancel(_)
                    | Call::SessionMethods(_)
                    | Call::ModelPreview(_)
                    | Call::Delete(_)
                    | Call::Fork(_)
                    | Call::BranchEnd(_)
            )
        {
            return Err(RpcError::app(
                -32601,
                "unknown_method",
                "Method requires protocol v3",
            ));
        }
        if matches!(
            call,
            Call::SessionMcp(_) | Call::McpRefresh(_) | Call::McpAppWidget(_) | Call::McpAppCall(_)
        ) && rpc.mcp.is_none()
        {
            return Err(RpcError::app(
                -32601,
                "unknown_method",
                "MCP integration is unavailable",
            ));
        }
        let (session_id, branch) = match call {
            Call::Submit(p) => {
                if p.submission_id.is_some() && self.protocol_version() < 3 {
                    return Err(RpcError::invalid("submissionId requires protocol v3"));
                }
                if self.protocol_version() == 1 && (p.text.is_none() || p.user.is_some()) {
                    return Err(RpcError::invalid(
                        "protocol v1 submit requires text and does not accept user",
                    ));
                }
                (Some(&p.session_id), Some(&p.branch_id))
            }
            Call::InputLookup(p) => (Some(&p.session_id), Some(&p.branch_id)),
            Call::History(p) | Call::HistoryDisplay(p, ..) => {
                (Some(&p.session_id), Some(&p.branch_id))
            }
            Call::HistoryDetail(p) => (Some(&p.session_id), Some(&p.branch_id)),
            Call::Keys(p) => (Some(&p.session_id), Some(&p.branch_id)),
            Call::Stop(p) => (Some(&p.session_id), Some(&p.branch_id)),
            Call::Idle(p) => (Some(&p.session_id), Some(&p.branch_id)),
            Call::ApprovalRespond(p) => (Some(&p.session_id), Some(&p.branch_id)),
            Call::PermissionGet(p) | Call::SessionSkills(p) | Call::PromptRefresh(p) => {
                (Some(&p.session_id), None)
            }
            Call::PermissionUpdate(p) => (Some(&p.session_id), None),
            Call::ExecStop(p) => (Some(&p.session_id), None),
            Call::ModelUpdate(p) => (Some(&p.session_id), None),
            Call::Attach(p) => (Some(&p.session_id), None),
            Call::Detach(p)
            | Call::Close(p)
            | Call::SessionMcp(p)
            | Call::McpRefresh(p)
            | Call::SessionMethods(p) => (Some(&p.session_id), None),
            Call::McpAppWidget(p) => (Some(&p.session_id), None),
            Call::McpAppCall(p) => (Some(&p.session_id), None),
            Call::Create(p) => {
                if p.agent_id.is_some() && self.protocol_version() < 3 {
                    return Err(RpcError::invalid("agentId requires protocol v3"));
                }
                (None, None)
            }
            Call::ModelPreview(_) => {
                if self.entry != Entry::Local {
                    return Err(RpcError::app(
                        -32000,
                        "local_transport_required",
                        "Model preview requires a protected local connection",
                    ));
                }
                (None, None)
            }
            Call::BranchEnd(p) => {
                if p.branch_id.is_empty() {
                    return Err(RpcError::invalid("branchId must not be empty"));
                }
                (Some(&p.session_id), None)
            }
            Call::Branches(p) => (Some(&p.session_id), None),
            Call::BranchGet(p) => {
                if p.branch_id.is_empty() {
                    return Err(RpcError::invalid("branchId must not be empty"));
                }
                (Some(&p.session_id), None)
            }
            Call::BranchCreate(p) => {
                if self.protocol_version() < 2 {
                    return Err(RpcError::app(
                        -32601,
                        "unknown_method",
                        "Method requires protocol v2",
                    ));
                }
                (Some(&p.session_id), None)
            }
            // A delete names rows the storage format defines and removes them
            // by session id alone (`storage/timeline.rs` `drop_session`), and a
            // daemon opens only a store format it knows: an unknown one is
            // refused at open (`storage/sqlite.rs`). So every row this daemon
            // can see, it understands and can remove, while the recorded
            // protocol version only shapes projections. That is why it does not
            // gate the delete. An empty id is still invalid input, as for every
            // Session call.
            Call::Delete(p) => {
                if p.session_id.is_empty() {
                    return Err(RpcError::invalid("sessionId must not be empty"));
                }
                (None, None)
            }
            Call::Fork(p) => {
                if rpc.config.database_path.is_none() {
                    return Err(RpcError::app(
                        -32601,
                        "unknown_method",
                        "Method requires native storage",
                    ));
                }
                (Some(&p.session_id), None)
            }
            Call::AgentCreate(_) | Call::AgentList | Call::AgentClose(_) => {
                if self.protocol_version() < 3 {
                    return Err(RpcError::app(
                        -32601,
                        "unknown_method",
                        "Method requires protocol v3",
                    ));
                }
                (None, None)
            }
            _ => (None, None),
        };
        if let Some(branch) = branch {
            match (self.protocol_version(), branch) {
                (1, None) => {}
                (2..=4, Some(id)) if !id.is_empty() => {}
                _ => {
                    return Err(RpcError::invalid(
                        "branchId is required in v2-v4 and absent in v1",
                    ));
                }
            }
        }
        let product_branch = match call {
            Call::PermissionGet(p) | Call::SessionSkills(p) | Call::PromptRefresh(p) => {
                Some(p.branch_id.as_str())
            }
            Call::PermissionUpdate(p) => {
                if p.policy
                    .allowed_tools
                    .iter()
                    .chain(&p.policy.disallowed_tools)
                    .any(|name| name.trim().is_empty())
                {
                    return Err(RpcError::invalid("Tool names must not be empty"));
                }
                Some(p.branch_id.as_str())
            }
            Call::ExecStop(p) => Some(p.branch_id.as_str()),
            Call::ModelUpdate(p) => Some(p.branch_id.as_str()),
            _ => None,
        };
        if product_branch.is_some_and(str::is_empty) {
            return Err(RpcError::invalid("branchId must not be empty"));
        }
        if let Some(id) = session_id {
            self.authorize(rpc, id)?;
        }
        Ok(())
    }
    /// A Session belongs to the protocol version that created it, and one
    /// whose storage is damaged is unavailable.
    fn authorize(&self, rpc: &RpcService, id: &str) -> Result<(), RpcError> {
        self.authorize_version(rpc, id)?;
        if let Some(reason) = rpc.index.unavailable_session(id) {
            return Err(RpcError::from(AgentError::Storage(reason)));
        }
        Ok(())
    }
    /// A Session belongs to the protocol version that created it, and a
    /// connection may use it from that version on: the recorded version is a
    /// lower bound, not an equality. Only a Session recorded later than this
    /// connection is refused, and the refusal names both versions so the
    /// operator can tell the two builds apart. A Session without a readable
    /// owner row has no recorded version and is refused without one. An empty
    /// id names no Session at all: it is invalid input, not a closed Session.
    fn authorize_version(&self, rpc: &RpcService, id: &str) -> Result<(), RpcError> {
        if id.is_empty() {
            return Err(RpcError::invalid("sessionId must not be empty"));
        }
        let message = match rpc.index.version_of(id) {
            Some(recorded) if recorded > self.protocol_version() => format!(
                "Session was created by protocol version {recorded}; this connection speaks {}",
                self.protocol_version()
            ),
            None if rpc.index.session(id).is_some() => {
                "Session belongs to another protocol version".to_owned()
            }
            _ => return Ok(()),
        };
        Err(RpcError::app(-32000, "session_protocol_version", message))
    }
    /// Execution owns the cutoff; this connection owns delivery to its subscriber.
    async fn barrier(
        &self,
        sequence: u64,
        delivery: Option<Arc<subscription::Delivery>>,
        unloading: &CancellationToken,
    ) -> Result<Reply, RpcError> {
        if self.protocol_version() >= 2 {
            if let Some(delivery) = delivery {
                tokio::select! {
                    biased;
                    _ = unloading.cancelled() => return Err(RpcError::reloading()),
                    result = delivery.wait() => result?,
                }
            }
            Ok(Reply::Barrier(crate::tree_protocol::BarrierResult {
                sequence,
            }))
        } else {
            Ok(Reply::empty())
        }
    }
    async fn handle(
        self: &Arc<Self>,
        rpc: &Arc<RpcService>,
        id: RequestId,
        call: Call,
    ) -> Option<Result<Reply, RpcError>> {
        if let Err(error) = self.validate_call(rpc, &call) {
            return Some(Err(error));
        }
        if call.is_web_entry() {
            return Some(self.web(call).await);
        }
        if call.is_ui_host() {
            return Some(self.ui(call).await);
        }
        let session_id = match &call {
            Call::Submit(p) => Some(&p.session_id),
            Call::InputLookup(p) => Some(&p.session_id),
            Call::History(p) | Call::HistoryDisplay(p, ..) => Some(&p.session_id),
            Call::HistoryDetail(p) => Some(&p.session_id),
            Call::Branches(p) => Some(&p.session_id),
            Call::BranchGet(p) => Some(&p.session_id),
            Call::Keys(p) => Some(&p.session_id),
            Call::Stop(p) => Some(&p.session_id),
            Call::Idle(p) => Some(&p.session_id),
            Call::ApprovalRespond(p) => Some(&p.session_id),
            Call::PermissionGet(p) | Call::SessionSkills(p) | Call::PromptRefresh(p) => {
                Some(&p.session_id)
            }
            Call::PermissionUpdate(p) => Some(&p.session_id),
            Call::ExecStop(p) => Some(&p.session_id),
            Call::ModelUpdate(p) => Some(&p.session_id),
            Call::SessionMcp(p) | Call::McpRefresh(p) => Some(&p.session_id),
            Call::McpAppWidget(p) => Some(&p.session_id),
            Call::McpAppCall(p) => Some(&p.session_id),
            Call::Attach(p) => Some(&p.session_id),
            Call::BranchCreate(p) => Some(&p.session_id),
            Call::BranchEnd(p) => Some(&p.session_id),
            Call::SessionMethods(p) => Some(&p.session_id),
            Call::Close(p) if rpc.index.session_available(&p.session_id) => Some(&p.session_id),
            _ => None,
        };
        let expected_cwd = match &call {
            Call::Attach(p) => match p.expected_cwd.as_deref() {
                Some(cwd) => match crate::protocol::canonical_cwd(cwd).await {
                    Ok(cwd) => Some(cwd),
                    Err(error) => return Some(Err(error)),
                },
                None => None,
            },
            _ => None,
        };
        let usage = if let Some(session_id) = session_id {
            match rpc
                .index
                .acquire_checked(session_id, expected_cwd.as_deref())
                .await
            {
                Ok(usage) => Some(usage),
                Err(error) => return Some(Err(RpcError::from(error))),
            }
        } else {
            None
        };
        if call.is_execution_session() {
            return Some(tokio::select! {
                biased;
                _ = rpc.unloading.cancelled() => Err(RpcError::interrupted()),
                result = self.execution_session(rpc, &usage.as_ref().expect("execution operation pins its Session").session, call) => result,
            });
        }
        let delivery = match &call {
            Call::Stop(p) => Some(&p.session_id),
            Call::Idle(p) => Some(&p.session_id),
            _ => None,
        }
        .and_then(|id| {
            self.attachments
                .lock()
                .expect("attachments")
                .get(id)
                .map(|a| a.delivery.clone())
        });
        Some(match call {
            Call::Hello(_) => Err(RpcError::invalid("hello has already completed")),
            Call::Ping => Ok(Reply::Ping(PingResult {
                sessions: rpc.index.session_count(),
                connections: self.hub.count(),
                server_version: SERVER_VERSION,
            })),
            Call::Shutdown(params) => {
                if params.if_idle && !params.force {
                    // The decision is atomic with hello and attach admission:
                    // an admission either ran before it and is counted or is
                    // refused as shutting down, so no client joins a daemon
                    // this call is closing.
                    let busy = {
                        let mut admission = self.hub.idle_decision();
                        if admission.closing_for_idle {
                            return Some(Err(RpcError::invalid("Shutdown already requested")));
                        }
                        let connections = self.hub.count_except(&self.id);
                        let attached_sessions = self.hub.attached_sessions();
                        let running = rpc.index.running_sessions();
                        if connections > 0 || attached_sessions > 0 || running > 0 {
                            Some((connections, attached_sessions, running))
                        } else {
                            let Some(pause) = rpc.control.activity.pause() else {
                                return Some(Err(RpcError::service(
                                    "daemon_busy",
                                    "The daemon has accepted process work; retry after it settles",
                                )));
                            };
                            let Some(sessions) = rpc.index.pause_sessions() else {
                                return Some(Err(RpcError::service(
                                    "daemon_busy",
                                    "A Session is not quiescent; retry after its work settles",
                                )));
                            };
                            admission.closing_for_idle = true;
                            rpc.control.keep_paused(pause, sessions);
                            None
                        }
                    };
                    if let Some((connections, attached_sessions, running)) = busy {
                        return Some(Err(RpcError::daemon_busy(
                            connections,
                            attached_sessions,
                            running,
                        )));
                    }
                }
                if !params.force {
                    let mut reply = self.shutdown_reply.lock().expect("shutdown reply");
                    if reply.is_some() {
                        return Some(Err(RpcError::invalid("Shutdown already requested")));
                    }
                    *reply = Some(id);
                    self.closed.cancel();
                    rpc.control.shutdown().cancel();
                    return None;
                }
                rpc.control.force();
                let ack = self
                    .sender
                    .send(Outbound::result(id, Ok(Reply::empty())), true);
                if let Ok(Some(ack)) = ack {
                    let _ = tokio::time::timeout(rpc.config.write_timeout, ack).await;
                }
                rpc.control.shutdown().cancel();
                return None;
            }
            Call::Create(params) => self.create(rpc, *params).await,
            Call::ModelPreview(params) => rpc.preview_model(params).await,
            Call::CatalogCancel(params) => {
                if let Some(Some(cancel)) = self
                    .ids
                    .lock()
                    .expect("request identities")
                    .get(&params.request_id)
                {
                    cancel.cancel();
                }
                Ok(Reply::empty())
            }
            Call::Catalog(mut params) => {
                let limit = params.limit.unwrap_or(50);
                if !(1..=200).contains(&limit) {
                    return Some(Err(RpcError::invalid(
                        "catalog limit must be between 1 and 200",
                    )));
                }
                params.limit = Some(limit);
                let cancel = self.ids.lock().expect("request identities")[&id]
                    .clone()
                    .expect("catalog reads have cancellation");
                match rpc
                    .index
                    .session_catalog(params, None, self.protocol_version(), cancel)
                    .await
                {
                    Ok(mut page) => {
                        for summary in &mut page.sessions {
                            summary.session.attached_clients =
                                self.hub.attached_clients(&summary.session.session_id);
                        }
                        Ok(Reply::Catalog(page))
                    }
                    Err(AgentError::InvalidInput(reason)) => Err(RpcError::invalid(reason)),
                    Err(AgentError::Cancelled(_)) => Err(RpcError::app(
                        -32000,
                        "cancelled",
                        "Session catalog read cancelled",
                    )),
                    Err(error) => Err(RpcError::from(error)),
                }
            }
            Call::Summaries(Summaries {
                agent_id,
                include_closed,
            }) => {
                if let Some(agent_id) = agent_id.as_deref()
                    && let Some(reason) = rpc.index.unavailable_agent(agent_id)
                {
                    return Some(Err(RpcError::from(AgentError::Storage(reason))));
                }
                if rpc.config.database_path.is_some() {
                    let cancel = self.ids.lock().expect("request identities")[&id]
                        .clone()
                        .expect("summary reads have cancellation");
                    return Some(
                        match rpc
                            .index
                            .session_catalog(
                                Catalog {
                                    with_internal: true,
                                    with_acp_sessions: true,
                                    with_automation_sessions: true,
                                    include_closed,
                                    ..Catalog::default()
                                },
                                agent_id,
                                self.protocol_version(),
                                cancel,
                            )
                            .await
                        {
                            Ok(mut page) => {
                                for summary in &mut page.sessions {
                                    summary.session.attached_clients =
                                        self.hub.attached_clients(&summary.session.session_id);
                                }
                                Ok(Reply::Summaries(page.sessions))
                            }
                            Err(AgentError::Cancelled(_)) => Err(RpcError::app(
                                -32000,
                                "cancelled",
                                "Session catalog read cancelled",
                            )),
                            Err(error) => Err(RpcError::from(error)),
                        },
                    );
                }
                let records = rpc
                    .index
                    .hosts()
                    .into_iter()
                    .filter(|host| {
                        agent_id
                            .as_ref()
                            .is_none_or(|agent| agent == &host.agent_id)
                    })
                    .flat_map(|host| {
                        let sessions = host.agent.sessions();
                        sessions.into_iter().map(move |s| (host.clone(), s))
                    })
                    .filter(|(_, session)| {
                        rpc.index
                            .version_of(session.id())
                            .is_some_and(|recorded| recorded <= self.protocol_version())
                    })
                    .map(|(host, s)| {
                        let status = s.state("main").map_err(RpcError::from)?;
                        Ok(SessionInfo {
                            session_id: s.id().to_owned(),
                            cwd: s.config().cwd.clone(),
                            model: dim_agent_types::ModelId {
                                provider_id: s.config().model.provider_id.clone(),
                                model_id: s.config().model.model_id.clone(),
                            },
                            created_at: s.created_at(),
                            running: status.running,
                            sequence: s.sequence("main").map_err(RpcError::from)?,
                            attached_clients: 0,
                            agent_id: Some(host.agent_id.clone()),
                            origin: None,
                            parent_session_id: None,
                            title: Some(crate::storage::TitleFields::default()),
                        })
                    })
                    .collect::<Result<Vec<_>, RpcError>>();
                let mut records = match records {
                    Ok(records) => records,
                    Err(error) => return Some(Err(error)),
                };
                for record in &mut records {
                    record.attached_clients = self.hub.attached_clients(&record.session_id);
                }
                let mut summaries = Vec::with_capacity(records.len());
                for session in records {
                    let summary = rpc
                        .index
                        .session_summary(&session.session_id, session.sequence, session.created_at)
                        .await;
                    let (preview, updated_at) = match summary {
                        Ok(summary) => summary,
                        Err(AgentError::Closed(_)) => continue,
                        Err(error) => return Some(Err(RpcError::from(error))),
                    };
                    summaries.push(SessionSummary {
                        closed: rpc.index.closed(&session.session_id),
                        session,
                        preview,
                        updated_at,
                    });
                }
                Ok(Reply::Summaries(summaries))
            }
            Call::Attach(params) => {
                return self
                    .attach(
                        rpc,
                        id.clone(),
                        params,
                        usage.expect("attach pins its Session"),
                    )
                    .await
                    .err()
                    .map(Err);
            }
            Call::Detach(params) => {
                // The attachment settles before the reply: its listener,
                // tasks and usage are gone when the client learns the detach
                // happened, so a count read now is the count after the work.
                let _transition = self.transitions.lock().await;
                let retired = self
                    .attachments
                    .lock()
                    .expect("attachments")
                    .remove(&params.session_id);
                self.hub.ui.detach(&self.id, &params.session_id);
                self.hub.notify_clients(&params.session_id);
                if let Some(retired) = retired {
                    retired.retire().await;
                }
                Ok(Reply::empty())
            }
            Call::Submit(params) => self.submit(rpc, params).await,
            Call::InputLookup(params) => self.input_lookup(rpc, params).await,
            Call::History(params) => self.history(rpc, params, false, false, false).await,
            Call::HistoryDisplay(params, tail, transcript) => {
                self.history(rpc, params, true, tail, transcript).await
            }
            Call::HistoryDetail(params) => tokio::select! {
                biased;
                _ = self.closed.cancelled() => return None,
                _ = rpc.unloading.cancelled() => Err(RpcError::reloading()),
                result = self.history_detail(rpc, params) => result,
            },
            Call::Branches(params) => self.branches(rpc, params).await,
            Call::BranchGet(params) => self.branch_get(rpc, params).await,
            Call::Keys(params) => self.keys(rpc, params).await,
            Call::Stop(params) => match self.session(rpc, &params.session_id) {
                Ok(session) => {
                    let branch = params.branch_id.as_deref().unwrap_or("main");
                    let result = session
                        .stop(
                            branch,
                            params
                                .reason
                                .unwrap_or_else(|| "User requested stop".into()),
                        )
                        .await
                        .map_err(|error| {
                            tracing::error!(%error, session_id=%params.session_id, "Session stop failed");
                            RpcError::from(error)
                        });
                    match result {
                        Ok(sequence) => self.barrier(sequence, delivery, &rpc.unloading).await,
                        Err(error) => Err(error),
                    }
                }
                Err(e) => Err(e),
            },
            Call::Idle(params) => match self.session(rpc, &params.session_id) {
                Ok(session) => {
                    let branch = params.branch_id.as_deref().unwrap_or("main");
                    let result = tokio::select! {biased;_=self.closed.cancelled()=>return None,_=rpc.unloading.cancelled()=>Err(RpcError::reloading()),result=session.idle(branch)=>result.map_err(RpcError::from)};
                    match result {
                        Ok(sequence) => self.barrier(sequence, delivery, &rpc.unloading).await,
                        Err(error) => Err(error),
                    }
                }
                Err(e) => Err(e),
            },
            Call::Close(params) => {
                let session = rpc.index.session(&params.session_id).map(|(_, s)| s);
                let result = match session {
                    Some(session) => session
                        .close()
                        .await
                        .map_err(|error| {
                            tracing::error!(%error, session_id=%params.session_id, "Session cleanup failed");
                            RpcError::wire(-32000, "Session cleanup failed")
                        }),
                    None => {
                        if let Some(host) = rpc.index.host_by_session(&params.session_id)
                            && let Some(session) = host.agent.get_session(&params.session_id)
                        {
                            session.closed().await.map_err(|error| {
                                tracing::error!(
                                    %error,
                                    session_id=%params.session_id,
                                    "Session cleanup failed"
                                );
                                RpcError::wire(-32000, "Session cleanup failed")
                            })
                        } else {
                            Ok(())
                        }
                    }
                };
                rpc.index.remove_version(&params.session_id);
                result.map(|_| Reply::empty())
            }
            Call::Delete(params) if params.require_inactive => {
                match rpc.index.delete_if_inactive(&params.session_id).await {
                    Ok(DeleteOutcome::Removed(deleted)) => {
                        Ok(Reply::Deleted(DeleteResult { deleted }))
                    }
                    Ok(DeleteOutcome::InUse(reason)) => {
                        Err(RpcError::app(-32000, "session_active", reason))
                    }
                    Err(error) => {
                        tracing::error!(%error, session_id=%params.session_id, "Session delete failed");
                        Err(RpcError::from(error))
                    }
                }
            }
            Call::Delete(params) => rpc
                .index
                .delete(&params.session_id)
                .await
                .map(|deleted| Reply::Deleted(DeleteResult { deleted }))
                .map_err(|error| {
                    tracing::error!(%error, session_id=%params.session_id, "Session delete failed");
                    RpcError::from(error)
                }),
            Call::Fork(params) => match rpc.index.fork(&params.session_id, params.at).await {
                Ok(fork) => fork
                    .sequence("main")
                    .map(|sequence| {
                        Reply::Fork(ForkResult {
                            session_id: fork.id().to_owned(),
                            sequence,
                        })
                    })
                    .map_err(RpcError::from),
                Err(error) => Err(RpcError::from(error)),
            },
            Call::ApprovalRespond(params) => {
                let branch = params.branch_id.clone().unwrap_or_else(|| "main".into());
                let result = match rpc.index.session(&params.session_id) {
                    Some((_, session)) => crate::approval::respond(
                        &session,
                        &branch,
                        &params.request_id,
                        params.decision,
                    )
                    .await
                    .map(|_| Reply::empty()),
                    None => Err(RpcError::closed()),
                };
                result.map_err(|error| {
                    let live = rpc
                        .index
                        .session(&params.session_id)
                        .and_then(|(_, session)| session.state(&branch).ok())
                        .is_some_and(|state| !state.closed);
                    if error.is_closed() && live {
                        RpcError::reloading()
                    } else {
                        error
                    }
                })
            }
            Call::BranchCreate(params) => self.create_branch(rpc, params).await,
            Call::BranchEnd(params) => self.end_branch(rpc, params).await,
            Call::SessionMethods(_) => crate::methods::session_listed(
                &rpc.management,
                &usage.expect("session.methods pins its Session").session,
            )
            .map(|methods| Reply::SessionMethods(SessionMethodsResult { methods })),
            Call::AgentCreate(params) => rpc
                .index
                .create(uuid::Uuid::new_v4(), params.profile, None)
                .await
                .map(|host| {
                    Reply::AgentCreated(AgentCreated {
                        agent_id: host.agent_id.clone(),
                    })
                })
                .map_err(RpcError::from),
            Call::AgentList => Ok(Reply::Agents(rpc.index.list())),
            Call::AgentClose(params) => {
                if let Some(reason) = rpc.index.unavailable_agent(&params.agent_id) {
                    return Some(Err(RpcError::from(AgentError::Storage(reason))));
                }
                if params.agent_id == rpc.index.default_agent_id() {
                    return Some(Err(RpcError::app(
                        -32000,
                        "agent_default",
                        "The default Agent cannot be closed",
                    )));
                }
                if rpc
                    .index
                    .owner(&params.agent_id)
                    .is_some_and(|owner| owner.is_some())
                {
                    return Some(Err(crate::protocol::agent_unknown()));
                }
                match rpc.index.close(&params.agent_id).await {
                    Ok(()) => Ok(Reply::empty()),
                    Err(
                        error @ (crate::agents::AgentOpError::Unload(_)
                        | crate::agents::AgentOpError::Settle(_)),
                    ) => {
                        tracing::error!(%error, agent_id=%params.agent_id, "Agent close failed");
                        Err(RpcError::from(error))
                    }
                    Err(error) => Err(RpcError::from(error)),
                }
            }
            _ => unreachable!("contributed methods are dispatched before session operations"),
        })
    }
    async fn execution_session(
        &self,
        rpc: &RpcService,
        session: &Session,
        call: Call,
    ) -> Result<Reply, RpcError> {
        Ok(match call {
            Call::SessionMcp(_) => Reply::McpState(mcp::session_state(session)?),
            Call::McpRefresh(_) => {
                session
                    .session_service::<crate::mcp::McpSession>()?
                    .refresh()
                    .map_err(mcp::error)?;
                Reply::empty()
            }
            Call::McpAppWidget(p) => {
                if p.tool.is_empty() {
                    return Err(RpcError::invalid("tool must not be empty"));
                }
                let widget = session
                    .session_service::<crate::mcp::McpSession>()?
                    .widget(&p.tool)
                    .await
                    .map_err(mcp::app_error)?;
                Reply::McpWidget(widget)
            }
            Call::McpAppCall(p) => {
                if p.server.is_empty() {
                    return Err(RpcError::invalid("server must not be empty"));
                }
                if p.tool.is_empty() {
                    return Err(RpcError::invalid("tool must not be empty"));
                }
                let started = std::time::Instant::now();
                let outcome = session
                    .session_service::<crate::mcp::McpSession>()?
                    .call(&p.server, &p.tool, p.arguments)
                    .await;
                tracing::info!(
                    target: "mcp.app",
                    session = %session.id(),
                    server = %p.server,
                    tool = %p.tool,
                    operation = "call",
                    outcome = %match &outcome {
                        Ok(_) => "ok".to_owned(),
                        Err(error) => error.to_string(),
                    },
                    duration_ms = started.elapsed().as_millis() as u64,
                );
                Reply::McpAppCall(outcome.map_err(mcp::app_error)?)
            }
            Call::PermissionGet(p) => {
                let state = session
                    .get::<PermissionState>(&p.branch_id)?
                    .ok_or_else(RpcError::reloading)?;
                Reply::Permission(PermissionResult {
                    version: state.version,
                    policy: state.policy,
                })
            }
            Call::PermissionUpdate(p) => {
                let version = session
                    .service::<Permission>(&p.branch_id)?
                    .update(p.expected_version, p.policy.clone())
                    .await
                    .map_err(|error| match error {
                        PermissionUpdateError::Conflict { .. } => {
                            RpcError::app(-32000, "permission_conflict", error.to_string())
                        }
                        PermissionUpdateError::Closed => RpcError::reloading(),
                        PermissionUpdateError::Failed(_) => {
                            RpcError::app(-32000, "permission_failed", error.to_string())
                        }
                    })?;
                Reply::Permission(PermissionResult {
                    version,
                    policy: p.policy,
                })
            }
            Call::ExecStop(p) => {
                if p.task_id.is_empty() {
                    return Err(RpcError::invalid("taskId must not be empty"));
                }
                let result = session
                    .service::<BackgroundExec>(&p.branch_id)?
                    .stop(&p.task_id)
                    .await
                    .map_err(|error| match error {
                        TaskError::Unknown => RpcError::app(
                            -32000,
                            "exec_task_unknown",
                            "Command is not a task of this branch",
                        ),
                        TaskError::Closed => RpcError::reloading(),
                        TaskError::Read(error) => error.into(),
                    })?;
                Reply::ExecEnded(result)
            }
            Call::SessionSkills(p) => {
                let skills = session
                    .service::<SkillsBranch>(&p.branch_id)?
                    .session()
                    .list()
                    .map_err(skills_error)?;
                Reply::SessionSkills(SessionSkillsResult { skills })
            }
            Call::ModelUpdate(p) => {
                if p.branch_id != "main" {
                    return Err(RpcError::invalid("Model updates only support main"));
                }
                let mut startup_admission = None;
                let mut model = if let Some(startup) = p.startup {
                    if self.entry != Entry::Local || rpc.config.product_home.is_none() {
                        return Err(RpcError::invalid(
                            "Model startup requires the protected-local product entry",
                        ));
                    }
                    if p.model_preferences.clears_reasoning()
                        || p.model_preferences.clears_service_tier()
                    {
                        return Err(RpcError::invalid(
                            "Startup preferences accept set values only",
                        ));
                    }
                    let state = session
                        .get::<dim_agent_capabilities::prompt_state::PromptSnapshot>(&p.branch_id)?
                        .ok_or_else(RpcError::reloading)?;
                    let current = state
                        .configuration
                        .ok_or_else(|| RpcError::invalid("No model configuration is adopted"))?;
                    let provider = rpc
                        .ctx
                        .get::<crate::provider::ProviderService>()
                        .ok_or_else(RpcError::reloading)?;
                    let admission = provider
                        .startup_configuration(
                            crate::protocol::ModelSelection {
                                provider: p.provider_id,
                                model_id: p.model_id,
                            },
                            Path::new(&session.config().cwd),
                            p.model_preferences.values(),
                            startup,
                            Some(&current),
                            &rpc.unloading,
                        )
                        .await
                        .map_err(provider_error)?;
                    let model = admission.model.clone();
                    startup_admission = Some(admission);
                    model
                } else {
                    match (p.provider_id, p.model_id) {
                        (Some(provider_id), Some(model_id)) => {
                            if provider_id.trim().is_empty() || model_id.trim().is_empty() {
                                return Err(RpcError::invalid("Model target must not be empty"));
                            }
                            rpc.model_selection(
                                Some(dim_agent_types::ModelId {
                                    provider_id,
                                    model_id,
                                }),
                                Some(PathBuf::from(&session.config().cwd)),
                                p.model_preferences.values(),
                            )
                            .await?
                        }
                        (None, None) => {
                            if p.model_preferences.is_empty() {
                                return Err(RpcError::invalid(
                                    "Supply at least one model preference",
                                ));
                            }
                            if rpc.config.product_home.is_none() {
                                return Err(provider_error(
                                    dim_agent_capabilities::host::HostError::failed(
                                        "Model preferences require a catalog with capability declarations; configure the API-key model's generation settings in --model-config",
                                    ),
                                ));
                            }
                            let state = session
                                .get::<dim_agent_capabilities::prompt_state::PromptSnapshot>(
                                    &p.branch_id,
                                )?
                                .ok_or_else(RpcError::reloading)?;
                            let current = state.configuration.ok_or_else(|| {
                            RpcError::invalid(
                                "No model configuration is adopted; supply providerId and modelId",
                            )
                        })?;
                            let provider = rpc
                                .ctx
                                .get::<crate::provider::ProviderService>()
                                .ok_or_else(RpcError::reloading)?;
                            provider
                                .apply_preferences(&current, &p.model_preferences, &rpc.unloading)
                                .await
                                .map_err(provider_error)?
                        }
                        _ => {
                            return Err(RpcError::invalid(
                                "providerId and modelId must appear together",
                            ));
                        }
                    }
                };
                // A model selection replaces the model, not the profile the
                // branch adopted: the record names the profile whose prompt
                // the branch runs, so it carries over until another adoption.
                if let Some(agent_id) = session
                    .get::<dim_agent_capabilities::prompt_state::PromptSnapshot>(&p.branch_id)?
                    .and_then(|state| state.configuration)
                    .and_then(|configuration| configuration.metadata)
                    .and_then(|metadata| metadata.agent_id)
                {
                    let mut metadata = model.metadata.take().unwrap_or_default();
                    metadata.agent_id = Some(agent_id);
                    model.metadata = Some(metadata);
                }
                let id = dim_agent_types::ModelId {
                    provider_id: model.provider_id.clone(),
                    model_id: model.model_id.clone(),
                };
                let outcome = session
                    .configure(
                        dim_agent_runtime::ConfigureUpdate {
                            model: Some(model.clone()),
                            ..Default::default()
                        },
                        &p.branch_id,
                    )
                    .await?;
                let outcome = configuration_outcome(outcome)?;
                if startup_admission.is_some() {
                    rpc.ctx
                        .get::<crate::provider::ProviderService>()
                        .ok_or_else(RpcError::reloading)?
                        .retain_startup(session, &model)
                        .map_err(provider_error)?;
                }
                Reply::ModelUpdate(ModelUpdateResult { model: id, outcome })
            }
            Call::PromptRefresh(p) => {
                if p.branch_id != "main" {
                    return Err(RpcError::invalid("Prompt refresh only supports main"));
                }
                let outcome = session
                    .configure(
                        dim_agent_capabilities::prompt_sources::refresh_update(),
                        &p.branch_id,
                    )
                    .await?;
                Reply::Configuration(ConfigurationResult {
                    outcome: configuration_outcome(outcome)?,
                })
            }
            _ => unreachable!("only execution Session operations reach this boundary"),
        })
    }
    async fn create(&self, rpc: &RpcService, params: Create) -> Result<Reply, RpcError> {
        if (params.startup.is_some() || params.model_from_session.is_some())
            && (self.entry != Entry::Local || rpc.config.product_home.is_none())
        {
            return Err(RpcError::invalid(
                "Model startup requires the protected-local product entry",
            ));
        }
        if params.model_from_session.is_some()
            && (params.model.is_some()
                || params.startup.is_some()
                || params.model_preferences.is_some())
        {
            return Err(RpcError::invalid(
                "modelFromSession cannot be combined with model, startup or modelPreferences",
            ));
        }
        let source = if let Some(id) = &params.model_from_session {
            self.authorize(rpc, id)?;
            Some(rpc.index.acquire(id).await?)
        } else {
            None
        };
        let cwd = session_cwd(&params.cwd).await?;
        if params
            .tools
            .as_ref()
            .is_some_and(|tools| tools.iter().any(|name| name.trim().is_empty()))
            || params.permission_policy.as_ref().is_some_and(|policy| {
                policy
                    .allowed_tools
                    .iter()
                    .chain(&policy.disallowed_tools)
                    .any(|name| name.trim().is_empty())
            })
        {
            return Err(RpcError::invalid("Tool names must not be empty"));
        }
        let host = rpc.index.host(params.agent_id.as_deref()).ok_or_else(|| {
            let agent_id = params
                .agent_id
                .as_deref()
                .unwrap_or(rpc.index.default_agent_id());
            match rpc.index.unavailable_agent(agent_id) {
                Some(reason) => RpcError::from(AgentError::Storage(reason)),
                None => agent_unknown(),
            }
        })?;
        let mut config = crate::agents::session_config(&host.profile, &rpc.config.session_defaults);
        config.cwd = cwd.clone();
        let mut startup_admission = None;
        if let Some(source) = &source {
            let adopted = source
                .session
                .get::<dim_agent_capabilities::prompt_state::PromptSnapshot>("main")?
                .and_then(|state| state.configuration.clone())
                .ok_or_else(|| RpcError::invalid("Source Session has no adopted model"))?;
            config.model = dim_agent_types::ModelSelection {
                provider_id: adopted.provider_id,
                model_id: adopted.model_id,
                capability_snapshot: adopted.capability_snapshot,
                config: adopted.config,
                metadata: adopted
                    .metadata
                    .map(|metadata| dim_agent_types::RequestMetadata {
                        startup: metadata.startup,
                        extensions: {
                            let mut extensions = metadata.extensions;
                            extensions.remove("requestId");
                            extensions
                        },
                        ..Default::default()
                    }),
            };
            if let Some(connection) = config
                .model
                .metadata
                .as_ref()
                .and_then(|metadata| metadata.startup.as_ref())
                .and_then(|startup| startup.connection.as_ref())
            {
                rpc.ctx
                    .get::<crate::provider::ProviderService>()
                    .ok_or_else(RpcError::reloading)?
                    .startup_connection(connection)
                    .map_err(provider_error)?;
            }
        } else {
            startup_admission = rpc
                .session_model(
                    &mut config,
                    &host.profile,
                    params.model,
                    params.model_preferences.unwrap_or_default(),
                    params.startup,
                )
                .await?;
        }
        if let Some(prompt) = params.system_prompt {
            config.system_prompt = prompt;
        }
        if let Some(prompt) = params.append_system_prompt {
            if !config.system_prompt.is_empty() {
                config.system_prompt.push_str("\n\n");
            }
            config.system_prompt.push_str(&prompt);
        }
        if let Some(tools) = params.tools {
            config.tools = Some(tools);
        }
        if self.protocol_version() < 4
            && !config.excluded_tools.iter().any(|name| name == "codemode")
        {
            config.excluded_tools.push("codemode".into());
        }
        if let Some(policy) = params.permission_policy {
            config.permission_policy = policy;
        }
        if let Some(interactive) = params.interactive {
            config.interactive = interactive;
        }
        if let Some(language) = params.language {
            config.language = Some(language);
        }
        let origin = match params.origin {
            None | Some(SessionOrigin::Client) => SessionOrigin::Client,
            Some(SessionOrigin::Acp) => SessionOrigin::Acp,
            Some(_) => {
                return Err(RpcError::invalid(
                    "a client may declare client or acp as a Session origin",
                ));
            }
        };
        let managed = rpc.ctx.get::<crate::worktrees::ManagedWorktrees>();
        let worktree = match (params.worktree, &managed) {
            (Some(request), Some(managed)) => Some(
                managed
                    .create(
                        std::path::Path::new(&cwd),
                        request.base.as_deref(),
                        request.branch.as_deref(),
                        &self.closed,
                    )
                    .await
                    .map_err(|error| error.rpc())?,
            ),
            (Some(_), None) => {
                return Err(RpcError::app(
                    -32000,
                    "worktree_unavailable",
                    "This daemon has no managed worktree root",
                ));
            }
            (None, _) => None,
        };
        if let Some(created) = &worktree {
            config.cwd = created.prepared.subtree.to_string_lossy().into_owned();
        }
        let session = match rpc
            .index
            .create_session(
                &host,
                config,
                self.protocol_version(),
                None,
                origin,
                worktree.as_ref().map(|created| created.prepared.clone()),
            )
            .await
        {
            Ok(session) => session,
            Err(error) => {
                if let (Some(created), Some(managed)) = (&worktree, &managed) {
                    managed.discard(created, &self.closed).await;
                }
                clear_worktree_record(rpc, worktree.as_ref()).await;
                return Err(RpcError::from(error));
            }
        };
        if startup_admission.is_some() || source.is_some() {
            let retained = rpc
                .ctx
                .get::<crate::provider::ProviderService>()
                .ok_or_else(RpcError::reloading)
                .and_then(|provider| {
                    provider
                        .retain_startup(&session, &session.config().model)
                        .map_err(provider_error)
                });
            if let Err(error) = retained {
                rpc.index
                    .delete(session.id())
                    .await
                    .map_err(RpcError::from)?;
                if let (Some(created), Some(managed)) = (&worktree, &managed) {
                    managed.discard(created, &self.closed).await;
                }
                clear_worktree_record(rpc, worktree.as_ref()).await;
                return Err(error);
            }
        }
        rpc.index.retain_versions();
        Ok(Reply::Create(CreateResult {
            session_id: session.id().to_owned(),
            model: dim_agent_types::ModelId {
                provider_id: session.config().model.provider_id.clone(),
                model_id: session.config().model.model_id.clone(),
            },
            model_preferences: crate::provider::adopted_selection_preferences(
                session.config().model.config.as_ref(),
                session.config().model.metadata.as_ref(),
            ),
            sequence: session.sequence("main").map_err(RpcError::from)?,
            worktree: worktree.as_ref().map(|created| created.reply()),
        }))
    }

    async fn web(&self, call: Call) -> Result<Reply, RpcError> {
        let web = &self.hub.web;
        Ok(match call {
            Call::WebOpen(params) => {
                Reply::WebOpened(web.open(&self.hub, &self.id, params.port).await?)
            }
            Call::WebToggle(params) => Reply::WebToggled(web.toggle(&self.hub, params.port).await?),
            Call::WebDisable => Reply::WebStatus(web.disable(&self.hub).await),
            Call::WebStatus => Reply::WebStatus(web.status(&self.hub).await),
            _ => unreachable!("validated web entry method"),
        })
    }

    /// The UI registry (Tui design §3.6): this connection registers itself,
    /// lists interfaces, or asks one to leave.
    async fn ui(&self, call: Call) -> Result<Reply, RpcError> {
        let ui = &self.hub.ui;
        Ok(match call {
            Call::UiOpen(params) => Reply::UiOpened(ui.open(self, params).await?),
            Call::UiClose(params) => {
                ui.close(&self.hub, &params.ui_id, params.reason).await?;
                Reply::empty()
            }
            Call::UiAttach(params) => Reply::UiAttached(ui.attach(&self.hub, self, &params).await?),
            Call::UiStatus => Reply::UiStatus(ui.status(&self.hub)),
            Call::UiShutdown(params) => {
                ui.shutdown(&self.hub, &params.ui_id, params.reason)?;
                Reply::empty()
            }
            Call::UiReplace(params) => {
                Reply::UiAttached(ui.replace(&self.hub, self, &params).await?)
            }
            _ => unreachable!("validated ui method"),
        })
    }
}

pub(crate) fn configuration_outcome(
    outcome: dim_agent_runtime::ConfigureOutcome,
) -> Result<&'static str, RpcError> {
    match outcome {
        dim_agent_runtime::ConfigureOutcome::Adopted => Ok("adopted"),
        dim_agent_runtime::ConfigureOutcome::Unchanged => Ok("unchanged"),
        dim_agent_runtime::ConfigureOutcome::Superseded => Err(RpcError::app(
            -32000,
            "configuration_superseded",
            "Configuration was superseded by another update",
        )),
    }
}

/// A failed creation after the Session transaction removed its rows: the
/// directory record it wrote stays a candidate, so drop it when nothing
/// associates it. A cleanup failure is visible in the next `worktree list`.
async fn clear_worktree_record(
    rpc: &RpcService,
    worktree: Option<&crate::worktrees::CreatedWorktree>,
) {
    let (Some(created), Some(store)) = (worktree, rpc.ctx.get::<crate::storage::SessionStore>())
    else {
        return;
    };
    let _ = store
        .remove_worktree_directory(created.prepared.directory.clone())
        .await;
}

/// Session operations wait for the product execution profile.
fn integration_pending() -> RpcError {
    RpcError::app(
        -32000,
        "product_integration_pending",
        "Programming integration is pending",
    )
}

pub(crate) fn provider_error(error: dim_agent_capabilities::host::HostError) -> RpcError {
    let kind = match &error {
        dim_agent_capabilities::host::HostError::Cancelled(_) => "provider_cancelled",
        _ => "provider_failed",
    };
    RpcError::app(-32000, kind, error.to_string())
}

/// Connection index, admission permits and connection tasks of the process.
pub(crate) struct ConnectionHub {
    config: Arc<ServerConfig>,
    pub control: Arc<ServerControl>,
    connections: Mutex<HashMap<String, Weak<Connection>>>,
    /// Serializes an idle-shutdown decision with hello and attach admission:
    /// an admission runs before the decision and is counted, or sees it and
    /// is refused as shutting down.
    admission: Mutex<IdleAdmission>,
    /// Emits a client-change fact for a Session whose presence a branch observes.
    client_emitter: Mutex<Option<rutis::Ctx>>,
    pub tasks: TaskTracker,
    pub shutdown_requests: TaskTracker,
    closing: CancellationToken,
    rpc: RwLock<Option<Arc<RpcService>>>,
    pub web: crate::web_entry::WebEntry,
    pub ui: Arc<crate::ui_host::UiHost>,
    /// Feature `testing`: holds the next subscription's `follow` task.
    #[cfg(feature = "testing")]
    follow_hold: Mutex<Option<Arc<crate::FollowHold>>>,
    /// Feature `testing`: fails the next attach before it registers its
    /// listener.
    #[cfg(feature = "testing")]
    fail_attach_before_registration: std::sync::atomic::AtomicBool,
}
/// What [`ConnectionHub::admission`] serializes: once an idle shutdown has
/// found the daemon idle, no hello or attach may join it.
#[derive(Default)]
pub(crate) struct IdleAdmission {
    closing_for_idle: bool,
}
impl ConnectionHub {
    fn new(
        config: Arc<ServerConfig>,
        control: Arc<ServerControl>,
        ui: Arc<crate::ui_host::UiHost>,
    ) -> Self {
        Self {
            web: crate::web_entry::WebEntry::new(config.web_assets.clone()),
            ui,
            connections: Mutex::new(HashMap::new()),
            admission: Mutex::new(IdleAdmission::default()),
            client_emitter: Mutex::new(None),
            tasks: TaskTracker::new(),
            shutdown_requests: TaskTracker::new(),
            closing: CancellationToken::new(),
            rpc: RwLock::new(None),
            control,
            config,
            #[cfg(feature = "testing")]
            follow_hold: Mutex::new(None),
            #[cfg(feature = "testing")]
            fail_attach_before_registration: std::sync::atomic::AtomicBool::new(false),
        }
    }
    pub fn config(&self) -> &ServerConfig {
        &self.config
    }
    /// Runs `f` with the inbound handler held registered, or returns a
    /// retryable rejection while it reloads.
    pub fn with_rpc<R>(&self, f: impl FnOnce(&Arc<RpcService>) -> R) -> Result<R, RpcError> {
        let handler = self.rpc.read().expect("inbound handler");
        let rpc = handler.as_ref().ok_or_else(RpcError::reloading)?;
        Ok(f(rpc))
    }
    /// Registers a handler generation. Connections that completed hello with
    /// another protocol instance close, so clients handshake with the new one.
    fn register(&self, rpc: Arc<RpcService>) {
        let mut handler = self.rpc.write().expect("inbound handler");
        for connection in self
            .connections
            .lock()
            .expect("connection index")
            .values()
            .filter_map(Weak::upgrade)
        {
            if connection
                .instance
                .get()
                .is_some_and(|instance| *instance != rpc.index.server_instance_id())
            {
                connection.closed.cancel();
            }
        }
        *handler = Some(rpc);
    }
    /// Removes the handler; the caller drains its commands.
    pub fn take_rpc(&self) -> Option<Arc<RpcService>> {
        self.rpc.write().expect("inbound handler").take()
    }
    fn entry_connections(&self, entry: Entry) -> Vec<Arc<Connection>> {
        self.connections
            .lock()
            .expect("connection index")
            .values()
            .filter_map(Weak::upgrade)
            .filter(|connection| connection.entry == entry)
            .collect()
    }
    pub fn entry_count(&self, entry: Entry) -> usize {
        self.entry_connections(entry).len()
    }
    /// A live connection by id.
    pub fn connection(&self, id: &str) -> Option<Arc<Connection>> {
        self.connections
            .lock()
            .expect("connection index")
            .get(id)
            .and_then(Weak::upgrade)
    }
    /// Asks every connection accepted on `entry` to close.
    pub fn close_entry(&self, entry: Entry) {
        for connection in self.entry_connections(entry) {
            connection.closed.cancel();
        }
    }
    /// A token cancelled when the hub closes; listeners it owns stop with it.
    pub fn closing(&self) -> CancellationToken {
        self.closing.clone()
    }
    fn count(&self) -> usize {
        self.connections.lock().expect("connection index").len()
    }
    /// Live connections other than `requester`.
    pub(crate) fn count_except(&self, requester: &str) -> usize {
        self.connections
            .lock()
            .expect("connection index")
            .keys()
            .filter(|id| id.as_str() != requester)
            .count()
    }
    /// Sessions with a client attached, on any connection: the requester's
    /// own attachment keeps the daemon too, because a concurrent attach is
    /// either counted here or refused as shutting down.
    pub(crate) fn attached_sessions(&self) -> usize {
        let connections = self.connections.lock().expect("connection index");
        let mut sessions = std::collections::HashSet::new();
        for connection in connections.values() {
            let Some(connection) = connection.upgrade() else {
                continue;
            };
            sessions.extend(
                connection
                    .attachments
                    .lock()
                    .expect("attachments")
                    .keys()
                    .cloned(),
            );
        }
        sessions.len()
    }
    /// Admits a hello or attach: refused once an idle shutdown has decided to
    /// close the daemon. Holding the guard makes the admission and that
    /// decision see each other.
    pub(crate) fn admit(&self) -> Result<std::sync::MutexGuard<'_, IdleAdmission>, RpcError> {
        let admission = self.admission.lock().expect("admission");
        if admission.closing_for_idle {
            return Err(RpcError::wire(-32603, "Server is shutting down"));
        }
        Ok(admission)
    }
    /// The idle-shutdown decision, with hello and attach admission held out
    /// while it counts and commits.
    pub(crate) fn idle_decision(&self) -> std::sync::MutexGuard<'_, IdleAdmission> {
        self.admission.lock().expect("admission")
    }
    fn attached_clients(&self, session: &str) -> usize {
        self.connections
            .lock()
            .expect("connection index")
            .values()
            .filter_map(Weak::upgrade)
            .filter(|conn| conn.is_attached(session))
            .count()
    }
    /// Holds the next subscription's `follow` task until `release`, so a test
    /// can keep a retirement in flight. Feature `testing` only.
    #[cfg(feature = "testing")]
    pub(crate) fn hold_next_follow(&self) -> Arc<crate::FollowHold> {
        let hold = Arc::new(crate::FollowHold::default());
        *self.follow_hold.lock().expect("follow hold") = Some(hold.clone());
        hold
    }
    #[cfg(feature = "testing")]
    pub(crate) fn take_follow_hold(&self) -> Option<Arc<crate::FollowHold>> {
        self.follow_hold.lock().expect("follow hold").take()
    }
    /// Feature `testing`: the next attach fails before it registers its
    /// listener.
    #[cfg(feature = "testing")]
    pub(crate) fn fail_next_attach_before_registration(&self) {
        self.fail_attach_before_registration
            .store(true, Ordering::SeqCst);
    }
    #[cfg(feature = "testing")]
    pub(crate) fn take_attach_failure_before_registration(&self) -> bool {
        self.fail_attach_before_registration
            .swap(false, Ordering::SeqCst)
    }
    fn set_client_emitter(&self, ctx: rutis::Ctx) {
        *self.client_emitter.lock().expect("client emitter") = Some(ctx);
    }
    /// Announces that `session`'s client counts may have changed.
    pub(crate) fn notify_clients(&self, session: &str) {
        let emitter = self.client_emitter.lock().expect("client emitter").clone();
        let Some(ctx) = emitter else {
            return;
        };
        let _ = ctx.emit(
            dim_agent_capabilities::environment_clients::ClientsChanged {
                name: dim_agent_capabilities::environment_clients::clients_changes(session),
                session_id: session.to_owned(),
            },
        );
    }
    /// The attached connections of one Session. A UI leaf's own connection
    /// is `tui`; it is not counted again under its declared kind.
    pub(crate) fn client_presences(
        &self,
        session: &str,
    ) -> Vec<dim_agent_capabilities::environment_clients::ClientPresence> {
        use dim_agent_capabilities::environment_clients::{ClientChannel, ClientPresence};
        let connections: Vec<_> = self
            .connections
            .lock()
            .expect("connection index")
            .values()
            .filter_map(Weak::upgrade)
            .filter(|conn| conn.is_attached(session))
            .filter(|conn| !self.ui.is_disposing(&conn.id))
            .collect();
        connections
            .into_iter()
            .map(|connection| {
                let channel = if self.ui.is_leaf_connection(&connection.id) {
                    ClientChannel::Tui
                } else {
                    connection.channel()
                };
                ClientPresence {
                    channel,
                    label: connection.label(),
                }
            })
            .collect()
    }
    /// Asks every connection to close and waits for its reader and writer.
    /// Connections created after this call start already closed.
    pub async fn close(&self) {
        self.closing.cancel();
        self.tasks.close();
        self.tasks.wait().await;
    }
}

pub(crate) struct ConnectionPlugin {
    config: Arc<ServerConfig>,
    control: Key<ServerControl>,
    ui: Key<crate::ui_host::UiHost>,
    clients: Key<dim_agent_capabilities::environment_clients::SessionClientsHost>,
    parts: Key<dim_agent_runtime::PartRegistry>,
}
impl ConnectionPlugin {
    pub fn new(names: &Names<'_>, config: Arc<ServerConfig>) -> rutis::Result<Self> {
        Ok(Self {
            config,
            control: names.key()?,
            ui: names.key()?,
            clients: names.key()?,
            parts: names.key()?,
        })
    }
}
/// The presence host over the connection and UI registries.
fn hub_clients(
    hub: Arc<ConnectionHub>,
) -> dim_agent_capabilities::environment_clients::SessionClientsHost {
    use dim_agent_capabilities::environment_clients::SessionClientsHost;
    SessionClientsHost::new(Arc::new(move |session_id| hub.client_presences(session_id)))
}
impl DynPlugin for ConnectionPlugin {
    fn name(&self) -> &str {
        "Connection"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.control.erase(), self.ui.erase(), self.parts.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let control = ctx.require_at(&self.control)?;
            let ui = ctx.require_at(&self.ui)?;
            let parts = ctx.require_at(&self.parts)?;
            let hub = Arc::new(ConnectionHub::new(self.config.clone(), control, ui));
            hub.set_client_emitter(ctx.clone());
            let owner = hub.clone();
            ctx.effect(move || async move {
                owner.close().await;
                Ok(())
            })?;
            ctx.provide_as::<ConnectionHub>(hub.clone())?;
            dim_agent_capabilities::environment_clients::register_parts(ctx, &parts)?;
            ctx.provide_at(self.clients.clone(), Arc::new(hub_clients(hub)))?;
            Ok(Effect::Done)
        })
    }
}

/// Typed services behind protocol handlers; accepted commands belong to it.
pub(crate) struct RpcService {
    pub index: Arc<AgentIndex>,
    /// The process root, for per-call lookups of services Rpc does not depend
    /// on: management method providers and, in the product profile, Provider.
    /// This load's own context sees no provider once it starts unloading,
    /// before `stopping` is cancelled.
    pub ctx: Ctx,
    /// The process's management method table.
    pub management: Arc<crate::methods::ManagementMethods>,
    /// Reads the status of the rows that registered a namespace; the loader
    /// never reloads.
    pub loader: rutis_loader::Loader,
    /// Cancelled when this load starts unloading, before its handler is
    /// removed; it then admits no management call or `hello`.
    pub stopping: CancellationToken,
    pub control: Arc<ServerControl>,
    pub config: Arc<ServerConfig>,
    /// The callers the providers of this process recorded, by row instance
    /// and connection id: what `callChannel` serves to other languages.
    pub callers: Arc<Callers>,
    /// Session MCP follow/refresh availability, not a host management service.
    /// The actual owners are acquired from the Session and followed across reload.
    pub mcp: Option<()>,
    pub commands: TaskTracker,
    /// Cancelled when this generation unloads: commands waiting on client
    /// input end so that draining cannot wait for requests it rejects.
    pub unloading: CancellationToken,
}

impl RpcService {
    async fn preview_model(&self, params: ModelPreview) -> Result<Reply, RpcError> {
        let cwd = session_cwd(&params.cwd).await?;
        let host = self
            .index
            .host(params.agent_id.as_deref())
            .ok_or_else(agent_unknown)?;
        let mut config =
            crate::agents::session_config(&host.profile, &self.config.session_defaults);
        config.cwd = cwd;
        let _admission = self
            .session_model(
                &mut config,
                &host.profile,
                params.model,
                params.model_preferences.unwrap_or_default(),
                params.startup,
            )
            .await?;
        Ok(Reply::ModelPreview(ModelPreviewResult {
            model_preferences: crate::provider::adopted_selection_preferences(
                config.model.config.as_ref(),
                config.model.metadata.as_ref(),
            ),
            model: dim_agent_types::ModelId {
                provider_id: config.model.provider_id,
                model_id: config.model.model_id,
            },
        }))
    }

    async fn session_model(
        &self,
        config: &mut dim_agent_runtime::SessionConfig,
        profile: &crate::agents::AgentProfile,
        model: Option<ModelSelection>,
        preferences: crate::provider::ModelPreferences,
        startup: Option<crate::provider::StartupInput>,
    ) -> Result<Option<crate::provider::StartupSelection>, RpcError> {
        if !self.index.selects_models() && preferences != Default::default() {
            return Err(RpcError::invalid(
                "Model preferences require the product provider",
            ));
        }
        if let Some(startup) = startup {
            if self.config.product_home.is_none() {
                return Err(RpcError::invalid(
                    "Model startup requires the protected-local product entry",
                ));
            }
            let provider = self
                .ctx
                .get::<crate::provider::ProviderService>()
                .ok_or_else(RpcError::reloading)?;
            let target = model.unwrap_or_else(|| {
                profile
                    .model
                    .clone()
                    .map(|model| ModelSelection {
                        provider: Some(model.provider_id),
                        model_id: Some(model.model_id),
                    })
                    .unwrap_or_default()
            });
            let admission = provider
                .startup_configuration(
                    target,
                    Path::new(&config.cwd),
                    preferences,
                    startup,
                    None,
                    &self.unloading,
                )
                .await
                .map_err(provider_error)?;
            config.model = admission.model.clone();
            return Ok(Some(admission));
        }
        if self.index.selects_models() {
            let explicit = match model {
                Some(ModelSelection {
                    provider: Some(provider_id),
                    model_id: Some(model_id),
                }) => Some(dim_agent_types::ModelId {
                    provider_id,
                    model_id,
                }),
                None
                | Some(ModelSelection {
                    provider: None,
                    model_id: None,
                }) => None,
                _ => {
                    return Err(RpcError::invalid(
                        "Explicit model requires provider and modelId",
                    ));
                }
            };
            config.model = self
                .model_selection(
                    explicit.or_else(|| profile.model.clone()),
                    Some(PathBuf::from(&config.cwd)),
                    preferences,
                )
                .await?;
        } else if let Some(model) = model {
            config.model = dim_agent_types::ModelSelection {
                provider_id: model
                    .provider
                    .unwrap_or_else(|| config.model.provider_id.clone()),
                model_id: model
                    .model_id
                    .unwrap_or_else(|| config.model.model_id.clone()),
                capability_snapshot: None,
                config: None,
                metadata: None,
            };
        }
        Ok(None)
    }

    async fn model_selection(
        &self,
        explicit: Option<dim_agent_types::ModelId>,
        cwd: Option<PathBuf>,
        preferences: crate::provider::ModelPreferences,
    ) -> Result<dim_agent_types::ModelSelection, RpcError> {
        if self.config.product_home.is_some() {
            let provider = self
                .ctx
                .get::<crate::provider::ProviderService>()
                .ok_or_else(RpcError::reloading)?;
            return provider
                .configuration(explicit, cwd.as_deref(), preferences, &self.unloading)
                .await
                .map_err(provider_error);
        }
        if preferences != Default::default() {
            return Err(provider_error(
                dim_agent_capabilities::host::HostError::failed(
                    "Model preferences require a catalog with capability declarations; configure the API-key model's generation settings in --model-config",
                ),
            ));
        }
        self.index
            .model_selection()
            .ok_or_else(RpcError::reloading)?
            .0
            .resolve(explicit, cwd, self.unloading.clone())
            .await
            .map_err(provider_error)
    }
}

pub(crate) struct RpcPlugin {
    config: Arc<ServerConfig>,
    root: Ctx,
    index: Key<AgentIndex>,
    collaboration: Key<crate::collaboration::Collaboration>,
    hub: Key<ConnectionHub>,
    control: Key<ServerControl>,
    loader: Key<rutis_loader::Loader>,
    management: Key<crate::methods::ManagementMethods>,
    callers: Key<Callers>,
}
impl RpcPlugin {
    pub fn new(names: &Names<'_>, config: Arc<ServerConfig>, root: Ctx) -> rutis::Result<Self> {
        Ok(Self {
            config,
            root,
            index: names.key()?,
            collaboration: names.key()?,
            hub: names.key()?,
            control: names.key()?,
            loader: names.key()?,
            management: names.key()?,
            callers: names.key()?,
        })
    }
}
impl DynPlugin for RpcPlugin {
    fn name(&self) -> &str {
        "Rpc"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![
            self.index.erase(),
            self.collaboration.erase(),
            self.hub.erase(),
            self.control.erase(),
            self.loader.erase(),
            self.management.erase(),
            self.callers.erase(),
        ]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let index = ctx.require_at(&self.index)?;
            let hub = ctx.require_at(&self.hub)?;
            let control = ctx.require_at(&self.control)?;
            let loader = ctx.require_at(&self.loader)?;
            let management = ctx.require_at(&self.management)?;
            let callers = ctx.require_at(&self.callers)?;
            let service = Arc::new(RpcService {
                index,
                ctx: self.root.clone(),
                management,
                loader: (*loader).clone(),
                stopping: ctx.cancellation_token(),
                control,
                config: self.config.clone(),
                callers,
                mcp: self.config.local_execution().then_some(()),
                commands: TaskTracker::new(),
                unloading: CancellationToken::new(),
            });
            let registered = service.clone();
            let handler = hub.clone();
            ctx.effect(move || async move {
                {
                    let mut handler = hub.rpc.write().expect("inbound handler");
                    if handler
                        .as_ref()
                        .is_some_and(|current| Arc::ptr_eq(current, &service))
                    {
                        *handler = None;
                    }
                }
                service.unloading.cancel();
                service.commands.close();
                service.commands.wait().await;
                // This load admitted every call the index records; a new
                // generation starts empty and records its own.
                service.callers.clear();
                Ok(())
            })?;
            handler.register(registered);
            Ok(Effect::Done)
        })
    }
}

/// Whether `method` is a Session method candidate (methods contract A.1):
/// no management namespace holds it, it follows the declaration grammar, and
/// its params name a Session.
fn session_candidate(rpc: &RpcService, method: &str, params: &Value) -> bool {
    dim_agent_capabilities::methods::valid_name(method)
        && !rpc.management.holds(method)
        && params.get("sessionId").is_some()
}

/// The process row providing `callChannel` for parts of other languages
/// (methods contract A.2) and the caller index Rpc records into. It depends
/// on no other row, so a row that names `callChannel` does not reload when
/// Rpc or the Agent index reloads.
pub(crate) struct CallChannelPlugin {
    callers: Key<Callers>,
}
impl CallChannelPlugin {
    pub fn new(names: &Names<'_>) -> rutis::Result<Self> {
        Ok(Self {
            callers: names.key()?,
        })
    }
}
impl DynPlugin for CallChannelPlugin {
    fn name(&self) -> &str {
        "CallChannel"
    }
    fn injects(&self) -> Vec<TypeKey> {
        Vec::new()
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let callers = Arc::new(Callers::default());
            ctx.provide_at(
                rutis_bridge::session::host_key("callChannel"),
                Arc::new(ForeignCallChannel {
                    callers: callers.clone(),
                }),
            )?;
            ctx.provide_at(self.callers.clone(), callers)?;
            Ok(Effect::Done)
        })
    }
}

/// The process service `callChannel` (methods contract A.2): the
/// notifications a row sends to a connection that called one of its
/// namespaces, and the wait for that connection to close. Its calls end in
/// the same `Caller` a Rust provider sends through.
pub(crate) struct ForeignCallChannel {
    pub callers: Arc<Callers>,
}
impl HostDispatch for ForeignCallChannel {
    fn methods(&self) -> Option<Value> {
        Some(serde_json::json!({ "notify": "async", "closed": "async" }))
    }
    fn invoke(&self, method: &str, args: BridgeValue) -> BridgeReply {
        // Read the caller inside `invoke`: a future returned here runs later
        // and no longer sees it.
        let row = caller_row().ok_or_else(|| {
            BridgeError::Value(
                "callChannel needs the calling row's context: only a row of a language runtime can call it"
                    .into(),
            )
        })?;
        let instance = row.instance();
        match method {
            "notify" => {
                let NotifyRequest {
                    connection_id,
                    call_id,
                    method,
                    params,
                } = request(args)?;
                let callers = self.callers.clone();
                Ok(BridgeValue::future(async move {
                    callers
                        .notify(instance, &connection_id, &call_id, &method, params)
                        .map(|()| serde_json::json!({}).into())
                        .map_err(|error| failure(&method, &connection_id, error))
                }))
            }
            "closed" => {
                let ClosedRequest { connection_id } = request(args)?;
                let closed = self
                    .callers
                    .closed(instance, &connection_id)
                    .map_err(|error| failure("closed", &connection_id, error))?;
                Ok(BridgeValue::future(async move {
                    closed.await;
                    Ok(BridgeValue::Undefined)
                }))
            }
            _ => Err(BridgeError::no_method(
                "callChannel",
                method,
                vec!["notify", "closed"],
            )),
        }
    }
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct NotifyRequest {
    connection_id: String,
    call_id: String,
    method: String,
    #[serde(default)]
    params: Map<String, Value>,
}
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ClosedRequest {
    connection_id: String,
}

/// One request object; anything else is the caller's programming error.
fn request<T: DeserializeOwned>(args: BridgeValue) -> Result<T, BridgeError> {
    let invalid = |detail: String| {
        BridgeError::Value(format!("expected one callChannel request object: {detail}"))
    };
    let Value::Array(args) = args.json()? else {
        return Err(invalid("not an argument list".into()));
    };
    let [request] = <[Value; 1]>::try_from(args)
        .map_err(|args| invalid(format!("{} arguments", args.len())))?;
    serde_json::from_value(request).map_err(|error| invalid(error.to_string()))
}

/// The service error a caller reads, carrying the contract's code.
fn failure(method: &str, connection: &str, error: NotifyError) -> BridgeError {
    BridgeError::binding(
        error.code(),
        match error {
            NotifyError::Undeclared => format!(
                "notification {method} is not declared by a namespace the calling row registered"
            ),
            // The index cannot tell a closed connection from one the row
            // never received a call from: both leave no entry.
            NotifyError::Gone => {
                format!("connection {connection} is closed or was not served by this row")
            }
            NotifyError::TooLarge => format!("notification {method} exceeds the frame limit"),
        },
        None,
    )
}

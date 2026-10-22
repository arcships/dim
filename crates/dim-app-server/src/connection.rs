use crate::{
    ServerConfig,
    agents::{AgentIndex, SessionUse},
    control::ServerControl,
    outbound::{Frame, Sender},
    protocol::*,
};
use dim_agent_capabilities::{
    methods::{Accepted, RpcError},
    permission::{Permission, PermissionState, PermissionUpdateError},
    skills::{SkillsBranch, SkillsError},
    workspace_tools::background::{BackgroundExec, TaskError},
};
use dim_agent_runtime::{AgentError, Capability, KeysOf, Session, scoped};
use dim_agent_types::{ContentBlock, InputMessageData, UserInput};
mod mcp;
mod subscription;
mod tree;
use rutis::{BoxFuture, CordisError, Ctx, Effect, TypeKey, TypedPlugin};
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::{
        Arc, Mutex, OnceLock, RwLock, Weak,
        atomic::{AtomicU64, Ordering},
    },
};
use subscription::Attachment;
use tokio_util::{sync::CancellationToken, task::TaskTracker};

const MAX_REQUESTS: usize = 64;
const CONTROL_RESERVE: usize = 8;

pub(crate) struct Connection {
    pub id: String,
    pub version: OnceLock<u32>,
    /// The protocol instance this connection completed hello with.
    pub instance: OnceLock<String>,
    pub local: bool,
    hub: Arc<ConnectionHub>,
    pub closed: CancellationToken,
    pub sender: Sender,
    pub shutdown_reply: Mutex<Option<RequestId>>,
    ids: Mutex<HashSet<RequestId>>,
    attachments: Mutex<HashMap<String, Attachment>>,
    next_attachment: AtomicU64,
    subscriptions: TaskTracker,
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
impl Connection {
    pub fn new(
        hub: Arc<ConnectionHub>,
        local: bool,
    ) -> (Arc<Self>, tokio::sync::mpsc::UnboundedReceiver<Frame>) {
        let closed = hub.closing.child_token();
        let (sender, receiver) = Sender::new(closed.clone());
        let connection = Arc::new(Self {
            id: uuid::Uuid::new_v4().to_string(),
            version: OnceLock::new(),
            instance: OnceLock::new(),
            local,
            closed,
            sender,
            shutdown_reply: Mutex::new(None),
            hub,
            ids: Mutex::new(HashSet::new()),
            attachments: Mutex::new(HashMap::new()),
            next_attachment: AtomicU64::new(1),
            subscriptions: TaskTracker::new(),
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
    pub async fn finish(&self) {
        self.closed.cancel();
        self.attachments.lock().expect("attachments").clear();
        self.subscriptions.close();
        self.subscriptions.wait().await;
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
            // A connection of a previous instance is closing; its requests never
            // reach the current generation.
            if self.closed.is_cancelled()
                || self.instance.get().map(String::as_str) != Some(rpc.index.server_instance_id())
            {
                return Err(RpcError::reloading());
            }
            let guard = self.admit(&request)?;
            self.admit_entry(rpc, &request.call)?;
            let this = self.clone();
            let Request { id, call } = request;
            if let Call::Contributed { method, params } = call {
                // Session namespaces never overlap management namespaces
                // (assembly fails otherwise), so routing a candidate first
                // keeps the contract's order: built-in, management, Session.
                if self.protocol_version() >= 3
                    && let Some(namespace) = rpc.sessions.owner(&method)
                {
                    // An unloading load admits no new call; the next load serves it.
                    if rpc.stopping.is_cancelled() {
                        return Err(RpcError::reloading());
                    }
                    let (rpc, namespace) = (rpc.clone(), namespace.to_owned());
                    let progress = this.progress(&id, &method);
                    rpc.commands.clone().spawn(async move {
                        match this
                            .session_method(&rpc, &namespace, method, params, progress)
                            .await
                        {
                            Ok((accepted, usage)) => {
                                this.answer(id, guard, accepted).await;
                                drop(usage);
                            }
                            Err(error) => {
                                let _ = this.sender.send(Outbound::result(id, Err(error)), false);
                            }
                        }
                    });
                    return Ok(());
                }
                if self.protocol_version() < 3 || !self.local {
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
                let accepted = rpc.owners.dispatch(
                    &rpc.ctx,
                    method,
                    params,
                    rpc.unloading.clone(),
                    progress,
                )?;
                rpc.commands.spawn(this.answer(id, guard, accepted));
                return Ok(());
            }
            let rpc = rpc.clone();
            // Shutdown must join its control requests without waiting for
            // business commands that a forced exit deliberately abandons.
            let shutdown =
                matches!(call, Call::Shutdown(_)).then(|| self.hub.shutdown_requests.token());
            rpc.commands.clone().spawn(async move {
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
    /// Sends an admitted contributed call's reply on this connection. The id
    /// is the client's again once answered. An interrupted operation may still
    /// commit: the calling load's unload waits for it, so the next load reads
    /// its outcome.
    async fn answer(self: Arc<Self>, id: RequestId, guard: Inflight, accepted: Accepted) {
        let result = accepted
            .reply
            .await
            .unwrap_or_else(|_| Err(RpcError::wire(-32603, "Method operation failed")))
            .map(Reply::Contributed);
        let _ = self.sender.send(Outbound::result(id, result), false);
        drop(guard);
        let _ = accepted.settled.await;
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
    /// the Session serves under `namespace`. The returned use holds the
    /// Session for the duration of the call; `sessionId` stays in the params.
    async fn session_method(
        &self,
        rpc: &RpcService,
        namespace: &str,
        method: String,
        params: Value,
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
        // A recorded namespace whose provider is not active answers from the
        // row that provides it in this Session: `service_reloading` while it
        // starts or reloads, `service_unavailable` once it cannot run.
        let provider = rpc.sessions.provider(&usage.session, namespace)?;
        if rpc.stopping.is_cancelled() {
            return Err(RpcError::reloading());
        }
        let accepted = provider.accept(method, params, rpc.unloading.clone(), progress)?;
        Ok((accepted, usage))
    }
    /// The connection's entry decides what it may call before any method is
    /// routed, for built-in and contributed methods alike: protected profiles
    /// serve only `hello` and `daemon.ping` to non-local transports.
    fn admit_entry(&self, rpc: &RpcService, call: &Call) -> Result<(), RpcError> {
        if rpc.config.protected_profile()
            && !self.local
            && !matches!(call, Call::Hello(_) | Call::Ping)
        {
            return Err(RpcError::app(
                -32000,
                "local_transport_required",
                "Execution and management require a protected local connection",
            ));
        }
        Ok(())
    }
    fn admit(self: &Arc<Self>, request: &Request) -> Result<Inflight, RpcError> {
        let mut ids = self.ids.lock().expect("request identities");
        if ids.contains(&request.id) {
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
        );
        if ids.len() >= MAX_REQUESTS + if control { CONTROL_RESERVE } else { 0 } {
            return Err(RpcError::app(
                -32000,
                "connection_busy",
                "Connection request limit reached",
            ));
        }
        ids.insert(request.id.clone());
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
        if call.is_execution_session() {
            let available = rpc.config.local_execution();
            if self.protocol_version() < 3 || !available {
                return Err(RpcError::app(
                    -32601,
                    "unknown_method",
                    "Management method is unavailable",
                ));
            }
            if !self.local {
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
                    | Call::List(_)
                    | Call::Summaries(_)
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
                    | Call::Summaries(_)
                    | Call::SessionMethods(_)
                    | Call::Delete(_)
                    | Call::Fork(_)
            )
        {
            return Err(RpcError::app(
                -32601,
                "unknown_method",
                "Method requires protocol v3",
            ));
        }
        if matches!(call, Call::SessionMcp(_) | Call::McpRefresh(_)) && rpc.mcp.is_none() {
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
            Call::History(p) => (Some(&p.session_id), Some(&p.branch_id)),
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
            Call::Create(p) => {
                if p.agent_id.is_some() && self.protocol_version() < 3 {
                    return Err(RpcError::invalid("agentId requires protocol v3"));
                }
                (None, None)
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
            // A delete reads none of the Session's records, so a damaged
            // Session is deleted too: only its protocol version is checked.
            Call::Delete(p) => {
                self.authorize_version(rpc, &p.session_id)?;
                (None, None)
            }
            // A fork copies the native store's rows.
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
                (2 | 3, Some(id)) if !id.is_empty() => {}
                _ => {
                    return Err(RpcError::invalid(
                        "branchId is required in v2/v3 and absent in v1",
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
    /// A Session belongs to the protocol version that created it. A Session
    /// without a readable owner row has no recorded version.
    fn authorize_version(&self, rpc: &RpcService, id: &str) -> Result<(), RpcError> {
        let known = rpc.index.version_of(id);
        if known.is_some_and(|version| version != self.protocol_version())
            || (known.is_none() && rpc.index.session(id).is_some())
        {
            return Err(RpcError::app(
                -32000,
                "session_protocol_version",
                "Session belongs to another protocol version",
            ));
        }
        Ok(())
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
        let session_id = match &call {
            Call::Submit(p) => Some(&p.session_id),
            Call::InputLookup(p) => Some(&p.session_id),
            Call::History(p) => Some(&p.session_id),
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
            Call::Attach(p) => Some(&p.session_id),
            Call::BranchCreate(p) => Some(&p.session_id),
            Call::SessionMethods(p) => Some(&p.session_id),
            Call::Close(p) if rpc.index.session_available(&p.session_id) => Some(&p.session_id),
            _ => None,
        };
        let usage = if let Some(session_id) = session_id {
            match rpc.index.acquire(session_id).await {
                Ok(usage) => Some(usage),
                Err(error) => return Some(Err(RpcError::from(error))),
            }
        } else {
            None
        };
        if call.is_execution_session() {
            // Capability owners retain admitted work when the RPC generation ends.
            return Some(tokio::select! {
                biased;
                _ = rpc.unloading.cancelled() => Err(RpcError::interrupted()),
                result = self.execution_session(rpc, &usage.as_ref().expect("execution operation pins its Session").session, call) => result,
            });
        }
        // Capture the subscription before waiting for execution. A replacement
        // cannot silently inherit this request's delivery guarantee.
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
        let include_summary = matches!(call, Call::Summaries(_));
        Some(match call {
            Call::Hello(_) => Err(RpcError::invalid("hello has already completed")),
            Call::Ping => Ok(Reply::Ping(PingResult {
                sessions: rpc.index.session_count(),
                connections: self.hub.count(),
                server_version: SERVER_VERSION,
            })),
            Call::Shutdown(params) => {
                if !params.force {
                    // The connection transfers only its final-reply writer to the
                    // process coordinator. This RPC must leave the graph before
                    // that coordinator can join cleanup and publish completion.
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
            Call::Create(params) => self.create(rpc, params).await,
            Call::List(params) | Call::Summaries(params) => {
                // v3 lists every Agent unless filtered; v1/v2 keep the
                // single-Agent view of the default Agent.
                let filter: Option<String> = if self.protocol_version() >= 3 {
                    params.agent_id
                } else {
                    Some(rpc.index.default_agent_id().to_owned())
                };
                if let Some(agent_id) = filter.as_deref()
                    && let Some(reason) = rpc.index.unavailable_agent(agent_id)
                {
                    return Some(Err(RpcError::from(AgentError::Storage(reason))));
                }
                let mut records = match rpc
                    .index
                    .session_directory(filter.clone(), self.protocol_version())
                    .await
                {
                    Ok(Some(records)) => records,
                    Err(error) => return Some(Err(RpcError::from(error))),
                    Ok(None) => match rpc
                        .index
                        .hosts()
                        .into_iter()
                        .filter(|host| filter.as_ref().is_none_or(|agent| agent == &host.agent_id))
                        .flat_map(|host| {
                            let sessions = host.agent.sessions();
                            sessions.into_iter().map(move |s| (host.clone(), s))
                        })
                        .filter(|(_, session)| {
                            rpc.index.version_of(session.id()) == Some(self.protocol_version())
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
                                attached_clients: self.hub.attached_clients(s.id()),
                                agent_id: (self.protocol_version() >= 3)
                                    .then(|| host.agent_id.clone()),
                                // No native store, so no titles.
                                title: (self.protocol_version() >= 3)
                                    .then(crate::storage::TitleFields::default),
                            })
                        })
                        .collect::<Result<Vec<_>, RpcError>>()
                    {
                        Ok(records) => records,
                        Err(error) => return Some(Err(error)),
                    },
                };
                for record in &mut records {
                    record.attached_clients = self.hub.attached_clients(&record.session_id);
                }
                if !include_summary {
                    return Some(Ok(Reply::List(records)));
                }
                let mut summaries = Vec::with_capacity(records.len());
                for session in records {
                    let summary = rpc
                        .index
                        .session_summary(&session.session_id, session.sequence, session.created_at)
                        .await;
                    let (preview, updated_at) = match summary {
                        Ok(summary) => summary,
                        // An in-memory session can close after directory capture.
                        Err(AgentError::Closed(_)) => continue,
                        Err(error) => return Some(Err(RpcError::from(error))),
                    };
                    summaries.push(SessionSummary {
                        session,
                        preview,
                        updated_at,
                    });
                }
                Ok(Reply::Summaries(summaries))
            }
            Call::Attach(params) => {
                return match self.attach(
                    rpc,
                    id.clone(),
                    params,
                    usage.expect("attach pins its Session"),
                ) {
                    Ok(()) => None,
                    Err(error) => Some(Err(error)),
                };
            }
            Call::Detach(params) => {
                self.attachments
                    .lock()
                    .expect("attachments")
                    .remove(&params.session_id);
                Ok(Reply::empty())
            }
            Call::Submit(params) => self.submit(rpc, params).await,
            Call::InputLookup(params) => self.input_lookup(rpc, params).await,
            Call::History(params) => self.history(rpc, params).await,
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
                    // Idle only observes work that may wait for client input; an
                    // unloading service ends the wait instead of draining it.
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
                        // The owning Agent may be closing: wait for its existing
                        // settlement, and report a cleanup failure exactly like
                        // the direct close path does.
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
            Call::Delete(params) => rpc
                .index
                .delete(&params.session_id)
                .await
                .map(|()| Reply::empty())
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
                // Accepted commands belong to the Session and continue after disconnect.
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
                // The approval capability of a live branch is reloading, not closed.
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
            Call::SessionMethods(_) => rpc
                .sessions
                .listed(&usage.expect("session.methods pins its Session").session)
                .map(|methods| Reply::SessionMethods(SessionMethodsResult { methods })),
            Call::AgentCreate(params) => rpc
                .index
                .create(uuid::Uuid::new_v4(), params.profile)
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
                    // The default Agent is the v1/v2 compatibility target;
                    // those clients cannot create or recover Agents.
                    return Some(Err(RpcError::app(
                        -32000,
                        "agent_default",
                        "The default Agent cannot be closed",
                    )));
                }
                match rpc.index.close(&params.agent_id).await {
                    Ok(()) => Ok(Reply::empty()),
                    Err(
                        error @ (crate::agents::AgentOpError::Unload(_)
                        | crate::agents::AgentOpError::Settle(_)),
                    ) => {
                        // Cleanup details stay in the server log; the client
                        // gets a fixed outbound message.
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
                let model = match (p.provider_id, p.model_id) {
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
                            p.model_preferences,
                        )
                        .await?
                    }
                    (None, None) => {
                        if p.model_preferences == crate::provider::ModelPreferences::default() {
                            return Err(RpcError::invalid("Supply at least one model preference"));
                        }
                        if rpc.config.product_home.is_none() {
                            return Err(provider_error(
                                dim_agent_capabilities::host::HostError::failed(
                                    "Model preferences require a catalog with capability declarations; configure the API-key model's generation settings in --model-config",
                                ),
                            ));
                        }
                        // shortcut: concurrent clients may replace an older base; revisit with configuration patch semantics.
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
                            .apply_preferences(&current, p.model_preferences, &rpc.unloading)
                            .await
                            .map_err(provider_error)?
                    }
                    _ => {
                        return Err(RpcError::invalid(
                            "providerId and modelId must appear together",
                        ));
                    }
                };
                let id = dim_agent_types::ModelId {
                    provider_id: model.provider_id.clone(),
                    model_id: model.model_id.clone(),
                };
                let outcome = session
                    .configure(
                        dim_agent_runtime::ConfigureUpdate {
                            model: Some(model),
                            ..Default::default()
                        },
                        &p.branch_id,
                    )
                    .await?;
                Reply::ModelUpdate(ModelUpdateResult {
                    model: id,
                    outcome: configuration_outcome(outcome)?,
                })
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
        if !rpc.index.selects_models() && params.model_preferences != Default::default() {
            return Err(RpcError::invalid(
                "Model preferences require the product provider",
            ));
        }
        if rpc.index.selects_models() {
            if params
                .model
                .as_ref()
                .is_some_and(|model| model.provider.is_some() != model.model_id.is_some())
            {
                return Err(RpcError::invalid(
                    "Explicit model requires provider and modelId",
                ));
            }
            config.model = rpc
                .model_selection(
                    params
                        .model
                        .as_ref()
                        .and_then(|model| {
                            model
                                .model_id
                                .as_ref()
                                .map(|model_id| dim_agent_types::ModelId {
                                    provider_id: model
                                        .provider
                                        .clone()
                                        .expect("explicit provider was validated"),
                                    model_id: model_id.clone(),
                                })
                        })
                        .or_else(|| host.profile.model.clone()),
                    Some(cwd.into()),
                    params.model_preferences,
                )
                .await?;
        } else if let Some(model) = params.model {
            config.model = dim_agent_types::ModelSelection {
                provider_id: model.provider.unwrap_or(config.model.provider_id),
                model_id: model.model_id.unwrap_or(config.model.model_id),
                capability_snapshot: None,
                config: None,
                metadata: None,
            };
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
            config.tools = tools;
        }
        if let Some(policy) = params.permission_policy {
            config.permission_policy = policy;
        }
        if let Some(interactive) = params.interactive {
            config.interactive = interactive;
        }
        let session = rpc
            .index
            .create_session(&host, config, self.protocol_version())
            .await
            .map_err(RpcError::from)?;
        rpc.index.retain_versions();
        Ok(Reply::Create(CreateResult {
            session_id: session.id().to_owned(),
            model: dim_agent_types::ModelId {
                provider_id: session.config().model.provider_id.clone(),
                model_id: session.config().model.model_id.clone(),
            },
            sequence: session.sequence("main").map_err(RpcError::from)?,
        }))
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

/// Session operations wait for the product execution profile.
fn integration_pending() -> RpcError {
    RpcError::app(
        -32000,
        "product_integration_pending",
        "Programming integration is pending",
    )
}

fn skills_error(error: SkillsError) -> RpcError {
    let app = match &error {
        SkillsError::Closed => return RpcError::reloading(),
        SkillsError::UnknownSkill(_) => "skill_unknown",
        SkillsError::SettingsConflict => "skills_conflict",
        SkillsError::SettingsWrite(_) => "skills_write_failed",
        SkillsError::SettingsSync(_) => "skills_sync_failed",
        SkillsError::SettingsRead(_) | SkillsError::RootUnreadable { .. } => "skills_unavailable",
    };
    RpcError::app(-32000, app, error.to_string())
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
    pub tasks: TaskTracker,
    pub shutdown_requests: TaskTracker,
    closing: CancellationToken,
    rpc: RwLock<Option<Arc<RpcService>>>,
}
impl ConnectionHub {
    fn new(config: Arc<ServerConfig>, control: Arc<ServerControl>) -> Self {
        Self {
            connections: Mutex::new(HashMap::new()),
            tasks: TaskTracker::new(),
            shutdown_requests: TaskTracker::new(),
            closing: CancellationToken::new(),
            rpc: RwLock::new(None),
            control,
            config,
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
    fn count(&self) -> usize {
        self.connections.lock().expect("connection index").len()
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
}
impl ConnectionPlugin {
    pub fn new(config: Arc<ServerConfig>) -> Self {
        Self { config }
    }
}
impl TypedPlugin for ConnectionPlugin {
    type Deps = (Arc<ServerControl>,);
    fn name(&self) -> &str {
        "Connection"
    }
    fn apply<'a>(
        &'a self,
        ctx: &'a Ctx,
        (control,): Self::Deps,
    ) -> BoxFuture<'a, Result<Effect, CordisError>> {
        Box::pin(async move {
            let hub = Arc::new(ConnectionHub::new(self.config.clone(), control));
            let owner = hub.clone();
            ctx.effect(move || {
                Effect::AsyncDisposer(Box::new(move || {
                    Box::pin(async move {
                        owner.close().await;
                        Ok(())
                    })
                }))
            })?;
            ctx.provide_as(TypeKey::of::<ConnectionHub>(), hub)?;
            Ok(Effect::Done)
        })
    }
}
impl Capability for ConnectionPlugin {
    fn keys(&self) -> KeysOf<Self> {
        scoped::<Self::Deps, _>(&())
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
    /// This profile's management namespaces and their owner rows.
    pub owners: crate::methods::Owners,
    /// The `sessionMethods` namespaces every Session lists, and the rows
    /// known to provide them.
    pub sessions: crate::methods::SessionNamespaces,
    /// Cancelled when this load starts unloading, before its handler is
    /// removed; it then admits no management call or `hello`.
    pub stopping: CancellationToken,
    pub control: Arc<ServerControl>,
    pub config: Arc<ServerConfig>,
    /// Session MCP follow/refresh availability, not a host management service.
    /// The actual owners are acquired from the Session and followed across reload.
    pub mcp: Option<()>,
    pub commands: TaskTracker,
    /// Cancelled when this generation unloads: commands waiting on client
    /// input end so that draining cannot wait for requests it rejects.
    pub unloading: CancellationToken,
}

impl RpcService {
    async fn model_selection(
        &self,
        explicit: Option<dim_agent_types::ModelId>,
        cwd: Option<PathBuf>,
        preferences: crate::provider::ModelPreferences,
    ) -> Result<dim_agent_types::ModelSelection, RpcError> {
        // Product models come from Provider, read per call: while it is not
        // active the call is retryable and every other method keeps working.
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
    /// Management namespace and the loader row of its owner.
    methods: Arc<[(String, String)]>,
    /// The `sessionMethods` namespaces assembly recorded for every Session.
    sessions: Arc<[String]>,
    /// Session namespace and the id of the TypeScript Session row providing it.
    session_rows: Arc<[(String, String)]>,
    root: Ctx,
}
impl RpcPlugin {
    pub fn new(
        config: Arc<ServerConfig>,
        methods: Arc<[(String, String)]>,
        sessions: Arc<[String]>,
        session_rows: Arc<[(String, String)]>,
        root: Ctx,
    ) -> Self {
        Self {
            config,
            methods,
            sessions,
            session_rows,
            root,
        }
    }
}
impl TypedPlugin for RpcPlugin {
    type Deps = (
        Arc<AgentIndex>,
        // Restore observers must outlive admitted RPC commands during reload.
        rutis::Gate<crate::collaboration::Collaboration>,
        Arc<ConnectionHub>,
        Arc<ServerControl>,
        // Reads the status of the rows that provide method namespaces; the
        // loader never reloads.
        Arc<rutis_loader::Loader>,
    );
    fn name(&self) -> &str {
        "Rpc"
    }
    fn apply<'a>(
        &'a self,
        ctx: &'a Ctx,
        (index, _, hub, control, loader): Self::Deps,
    ) -> BoxFuture<'a, Result<Effect, CordisError>> {
        Box::pin(async move {
            // Every configured owner names an assembly row; one that names no
            // row would answer a reloading owner as absent.
            if let Some((namespace, row)) = self
                .methods
                .iter()
                .find(|(_, row)| loader.get(row).is_none())
            {
                return Err(CordisError::PluginFailed(
                    format!("management namespace {namespace} names unknown row {row}").into(),
                ));
            }
            let sessions = crate::methods::SessionNamespaces::new(
                (*loader).clone(),
                &self.methods,
                &self.sessions,
                &self.session_rows,
            )?;
            let service = Arc::new(RpcService {
                index,
                ctx: self.root.clone(),
                owners: crate::methods::Owners::new((*loader).clone(), &self.methods),
                sessions,
                stopping: ctx.cancellation_token(),
                control,
                config: self.config.clone(),
                mcp: self.config.local_execution().then_some(()),
                commands: TaskTracker::new(),
                unloading: CancellationToken::new(),
            });
            let registered = service.clone();
            let handler = hub.clone();
            ctx.effect(move || {
                Effect::AsyncDisposer(Box::new(move || {
                    Box::pin(async move {
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
                        Ok(())
                    })
                }))
            })?;
            // Registered only after its cleanup is owned by this generation.
            handler.register(registered);
            Ok(Effect::Done)
        })
    }
}
impl Capability for RpcPlugin {
    fn keys(&self) -> KeysOf<Self> {
        ((), (), (), (), ())
    }
}

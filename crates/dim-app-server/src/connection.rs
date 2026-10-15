use crate::{
    ServerConfig,
    agents::AgentIndex,
    control::ServerControl,
    outbound::{Frame, Sender},
    protocol::*,
};
use dim_agent_capabilities::{
    permission::{Permission, PermissionState, PermissionUpdateError},
    skills::{RefreshTarget, SkillStatus, SkillsBranch, SkillsCatalog, SkillsError},
    workspace_tools::background::{BackgroundExec, TaskError},
};
use dim_agent_runtime::{AgentError, Capability, KeysOf, Session, When, scoped};
use dim_agent_types::{ContentBlock, InputMessageData, UserInput};
mod mcp;
mod subscription;
mod tree;
use rutis::{BoxFuture, CordisError, Ctx, Effect, TypeKey, TypedPlugin};
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

/// The entry a connection was accepted on; it decides which methods Rpc serves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Entry {
    /// Owner-only Unix socket or named pipe.
    Local,
    /// The unauthenticated `/ws` of `--ws`.
    Bare,
    /// The authenticated local web entry: Session methods and web-readable
    /// management reads only.
    Web,
}

pub(crate) struct Connection {
    pub id: String,
    pub version: OnceLock<u32>,
    /// The protocol instance this connection completed hello with.
    pub instance: OnceLock<String>,
    pub entry: Entry,
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
        entry: Entry,
    ) -> (Arc<Self>, tokio::sync::mpsc::Receiver<Frame>) {
        let closed = hub.closing.child_token();
        let (sender, receiver) = Sender::new(closed.clone());
        let connection = Arc::new(Self {
            id: uuid::Uuid::new_v4().to_string(),
            version: OnceLock::new(),
            instance: OnceLock::new(),
            entry,
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
            let this = self.clone();
            let rpc = rpc.clone();
            // Shutdown must join its control requests without waiting for
            // business commands that a forced exit deliberately abandons.
            let shutdown = matches!(request.call, Call::Shutdown(_))
                .then(|| self.hub.shutdown_requests.token());
            rpc.commands.clone().spawn(async move {
                let _guard = guard;
                let _shutdown = shutdown;
                let id = request.id;
                let result = this.handle(&rpc, id.clone(), request.call).await;
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
    fn admit(self: &Arc<Self>, request: &Request) -> Result<Inflight, RpcError> {
        let mut ids = self.ids.lock().expect("request identities");
        if ids.contains(&request.id) {
            return Err(RpcError::invalid("Request id is already in flight"));
        }
        let control = matches!(
            request.call,
            Call::Stop(_)
                | Call::Close(_)
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
        if self.entry == Entry::Web
            && (matches!(call, Call::Shutdown(_))
                || call.is_web_entry()
                || call.is_management() && !call.is_web_readable())
        {
            return Err(RpcError::app(
                -32601,
                "unknown_method",
                "Method is unavailable on the web entry",
            ));
        }
        if call.is_web_entry() {
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
        let local = self.entry != Entry::Bare;
        if rpc.config.protected_profile() && !local && !matches!(call, Call::Hello(_) | Call::Ping)
        {
            return Err(RpcError::app(
                -32000,
                "local_transport_required",
                "Execution and management require a protected local connection",
            ));
        }
        if call.is_management() || call.is_execution_session() {
            let available = if call.is_execution_management() || call.is_execution_session() {
                rpc.config.local_execution()
            } else {
                rpc.config.product_home.is_some()
            };
            if self.protocol_version() < 3 || !available {
                return Err(RpcError::app(
                    -32601,
                    "unknown_method",
                    "Management method is unavailable",
                ));
            }
            if !local {
                return Err(RpcError::app(
                    -32000,
                    "local_transport_required",
                    "Product management requires a protected local connection",
                ));
            }
        }
        if !rpc.config.session_execution
            && !call.is_management()
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
            return Err(RpcError::app(
                -32000,
                "product_integration_pending",
                "Programming integration is pending",
            ));
        }
        if self.protocol_version() < 3
            && matches!(
                call,
                Call::InputLookup(_) | Call::History(_) | Call::Summaries(_)
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
            Call::McpCatalog(_)
                | Call::McpSetEnabled(_)
                | Call::McpSave(_)
                | Call::McpRemove(_)
                | Call::McpDisconnect(_)
                | Call::McpApprove(_)
                | Call::McpRevoke(_)
                | Call::SessionMcp(_)
                | Call::McpRefresh(_)
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
            Call::Detach(p) | Call::Close(p) | Call::SessionMcp(p) | Call::McpRefresh(p) => {
                (Some(&p.session_id), None)
            }
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
            if let Some(reason) = rpc.index.unavailable_session(id) {
                return Err(RpcError::from(AgentError::Storage(reason)));
            }
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
        if call.is_web_entry() {
            return Some(self.web(call).await);
        }
        if call.is_management() {
            if matches!(
                call,
                Call::SkillsCatalog(_)
                    | Call::SkillSetEnabled(_)
                    | Call::SkillsRefresh(_)
                    | Call::McpCatalog(_)
                    | Call::McpSetEnabled(_)
                    | Call::McpSave(_)
                    | Call::McpRemove(_)
                    | Call::McpDisconnect(_)
                    | Call::McpApprove(_)
                    | Call::McpRevoke(_)
            ) {
                return Some(tokio::select! {
                    biased;
                    _ = rpc.unloading.cancelled() => Err(RpcError::app(-32000, "operation_interrupted", "Service reloaded; the operation may already have committed. Read the current state before retrying")),
                    result = self.management(rpc, call) => result,
                });
            }
            return Some(self.management(rpc, call).await);
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
                _ = rpc.unloading.cancelled() => Err(RpcError::app(-32000, "operation_interrupted", "Service reloaded; the operation may already have committed. Read the current state before retrying")),
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
                        // settlement so this reply still implies a complete
                        // archive, and report a cleanup failure exactly like
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
                // The owning Agent may still be finishing this Session's archive.
                if let Some(host) = rpc.index.host_by_session(&params.session_id) {
                    host.traces.wait_session(&params.session_id).await;
                }
                rpc.index.remove_version(&params.session_id);
                result.map(|_| Reply::empty())
            }
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
            _ => unreachable!("management methods are dispatched before session operations"),
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
                if p.model_id.trim().is_empty() {
                    return Err(RpcError::invalid("modelId must not be empty"));
                }
                let model = rpc
                    .model_selection(
                        Some(dim_agent_types::ModelId {
                            provider_id: p.provider_id,
                            model_id: p.model_id,
                        }),
                        Some(PathBuf::from(&session.config().cwd)),
                        p.model_preferences,
                    )
                    .await
                    .map_err(provider_error)?;
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
        if params.cwd.is_empty() || !Path::new(&params.cwd).is_absolute() {
            return Err(RpcError::invalid("cwd must be an absolute directory"));
        }
        let cwd = canonical_cwd(&params.cwd).await?;
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
                None => RpcError::agent_unknown(),
            }
        })?;
        let mut config = crate::agents::session_config(&host.profile, &rpc.config.session_defaults);
        config.cwd = cwd
            .to_str()
            .ok_or_else(|| RpcError::invalid("cwd must be UTF-8"))?
            .into();
        if rpc.index.model_selection.is_none() && params.model_preferences != Default::default() {
            return Err(RpcError::invalid(
                "Model preferences require the product provider",
            ));
        }
        if rpc.index.model_selection.is_some() {
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
                    Some(cwd.clone()),
                    params.model_preferences,
                )
                .await
                .map_err(provider_error)?;
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
        let (session, trace_path) = if let Some(trace) = params.trace {
            let (session, path) = host
                .traces
                .create(
                    &rpc.index,
                    &host,
                    config,
                    self.protocol_version(),
                    trace,
                    &rpc.config.trace_directory,
                )
                .await?;
            (session, Some(path))
        } else {
            (
                rpc.index
                    .create_session(&host, config, self.protocol_version())
                    .await
                    .map_err(RpcError::from)?,
                None,
            )
        };
        rpc.index.retain_versions();
        rpc.index
            .insert_version(session.id(), &host.agent_id, self.protocol_version());
        Ok(Reply::Create(CreateResult {
            session_id: session.id().to_owned(),
            model: dim_agent_types::ModelId {
                provider_id: session.config().model.provider_id.clone(),
                model_id: session.config().model.model_id.clone(),
            },
            sequence: session.sequence("main").map_err(RpcError::from)?,
            trace_path,
        }))
    }

    async fn web(&self, call: Call) -> Result<Reply, RpcError> {
        let web = &self.hub.web;
        Ok(match call {
            Call::WebOpen(params) => Reply::WebOpened(web.open(&self.hub, params.port).await?),
            Call::WebStatus => Reply::WebStatus(web.status(&self.hub).await),
            Call::WebClose => {
                web.close(&self.hub).await;
                Reply::empty()
            }
            _ => unreachable!("validated web entry method"),
        })
    }
    async fn management(&self, rpc: &RpcService, call: Call) -> Result<Reply, RpcError> {
        if matches!(
            call,
            Call::McpCatalog(_)
                | Call::McpSetEnabled(_)
                | Call::McpSave(_)
                | Call::McpRemove(_)
                | Call::McpDisconnect(_)
                | Call::McpApprove(_)
                | Call::McpRevoke(_)
        ) {
            return mcp::management(rpc, call).await;
        }
        if matches!(
            call,
            Call::SkillsCatalog(_) | Call::SkillSetEnabled(_) | Call::SkillsRefresh(_)
        ) {
            return self.skills_management(rpc, call).await;
        }
        let oauth = rpc
            .oauth
            .as_ref()
            .expect("product services are declared dependencies");
        let provider = rpc
            .provider
            .as_ref()
            .expect("product services are declared dependencies");
        let cancel = &rpc.unloading;
        Ok(match call {
            Call::AccountProviders => {
                Reply::AccountProviders(oauth.providers(cancel).await.map_err(oauth_error)?)
            }
            Call::AccountStatus(params) => Reply::Account(
                oauth
                    .status(&params.provider_id, cancel)
                    .await
                    .map_err(oauth_error)?,
            ),
            Call::AccountRecover(params) => Reply::Account(
                oauth
                    .call(
                        "recover",
                        serde_json::to_value(params).expect("typed DTO"),
                        cancel,
                    )
                    .await
                    .map_err(oauth_error)?,
            ),
            Call::LoginBegin(params) => Reply::DeviceLogin(
                oauth
                    .call(
                        "beginDevice",
                        serde_json::to_value(params).expect("typed DTO"),
                        cancel,
                    )
                    .await
                    .map_err(oauth_error)?,
            ),
            Call::BrowserLoginBegin(params) => Reply::BrowserLogin(
                oauth
                    .call(
                        "beginBrowser",
                        serde_json::to_value(params).expect("typed DTO"),
                        cancel,
                    )
                    .await
                    .map_err(oauth_error)?,
            ),
            Call::BrowserLoginComplete(params) => Reply::Account(
                oauth
                    .call(
                        "completeBrowser",
                        serde_json::to_value(params).expect("typed DTO"),
                        cancel,
                    )
                    .await
                    .map_err(oauth_error)?,
            ),
            Call::LoginStatus(params) => {
                let status: LoginState = oauth
                    .call(
                        "attemptStatus",
                        serde_json::to_value(params).expect("typed DTO"),
                        cancel,
                    )
                    .await
                    .map_err(oauth_error)?;
                Reply::LoginState(match status {
                    LoginState::Failed { .. } => LoginState::Failed {
                        error: "Login failed".into(),
                    },
                    status => status,
                })
            }
            Call::LoginCancel(params) => {
                let _: serde_json::Value = oauth
                    .call(
                        "cancelAttempt",
                        serde_json::to_value(params).expect("typed DTO"),
                        cancel,
                    )
                    .await
                    .map_err(oauth_error)?;
                Reply::empty()
            }
            Call::Logout(params) => Reply::Account(
                oauth
                    .call(
                        "logout",
                        serde_json::to_value(params).expect("typed DTO"),
                        cancel,
                    )
                    .await
                    .map_err(oauth_error)?,
            ),
            Call::CodexAccountStatus => Reply::ManagedAuthStatus(
                rpc.codex
                    .as_ref()
                    .expect("declared dependency")
                    .status(cancel)
                    .await
                    .map_err(managed_oauth_error)?,
            ),
            Call::CodexAccountRecover => {
                rpc.codex
                    .as_ref()
                    .expect("declared dependency")
                    .recover(cancel)
                    .await
                    .map_err(managed_oauth_error)?;
                Reply::empty()
            }
            Call::CodexBrowserLoginBegin => {
                let login = rpc
                    .codex
                    .as_ref()
                    .expect("declared dependency")
                    .begin_browser(cancel)
                    .await
                    .map_err(managed_oauth_error)?;
                Reply::BrowserLogin(BrowserLogin {
                    attempt_id: login.attempt_id.to_string(),
                    authorization_url: login.authorization_url.into(),
                    expires_at: login.expires_at,
                })
            }
            Call::CodexBrowserLoginComplete(params) => {
                let id = uuid::Uuid::parse_str(&params.attempt_id)
                    .map_err(|_| RpcError::invalid("attemptId must be a UUID"))?;
                let callback = url::Url::parse(&params.callback_url)
                    .map_err(|_| RpcError::invalid("callbackUrl must be a callback URL"))?;
                rpc.codex
                    .as_ref()
                    .expect("declared dependency")
                    .complete_browser(id, callback, cancel)
                    .await
                    .map_err(managed_oauth_error)?;
                Reply::empty()
            }
            Call::CodexLoginStatus(params) => managed_attempt(
                rpc.codex
                    .as_ref()
                    .expect("declared dependency")
                    .attempt_status(attempt_id(&params)?)
                    .await
                    .map_err(managed_oauth_error)?,
            ),
            Call::CodexLoginCancel(params) => {
                rpc.codex
                    .as_ref()
                    .expect("declared dependency")
                    .cancel_attempt(attempt_id(&params)?)
                    .await
                    .map_err(managed_oauth_error)?;
                Reply::empty()
            }
            Call::CodexLogout => {
                rpc.codex
                    .as_ref()
                    .expect("declared dependency")
                    .logout(cancel)
                    .await
                    .map_err(managed_oauth_error)?;
                Reply::empty()
            }
            Call::GrokAccountStatus => Reply::ManagedAuthStatus(
                rpc.grok
                    .as_ref()
                    .expect("declared dependency")
                    .status(cancel)
                    .await
                    .map_err(managed_oauth_error)?,
            ),
            Call::GrokAccountRecover => {
                rpc.grok
                    .as_ref()
                    .expect("declared dependency")
                    .recover(cancel)
                    .await
                    .map_err(managed_oauth_error)?;
                Reply::empty()
            }
            Call::GrokLoginBegin => {
                let login = rpc
                    .grok
                    .as_ref()
                    .expect("declared dependency")
                    .begin_device(cancel)
                    .await
                    .map_err(managed_oauth_error)?;
                Reply::DeviceLogin(DeviceLogin {
                    attempt_id: login.attempt_id.to_string(),
                    verification_uri: login.verification_uri.into(),
                    verification_uri_complete: login.verification_uri_complete.map(Into::into),
                    user_code: login.user_code,
                    expires_at: login.expires_at,
                })
            }
            Call::GrokLoginStatus(params) => managed_attempt(
                rpc.grok
                    .as_ref()
                    .expect("declared dependency")
                    .attempt_status(attempt_id(&params)?)
                    .await
                    .map_err(managed_oauth_error)?,
            ),
            Call::GrokLoginCancel(params) => {
                rpc.grok
                    .as_ref()
                    .expect("declared dependency")
                    .cancel_attempt(attempt_id(&params)?)
                    .await
                    .map_err(managed_oauth_error)?;
                Reply::empty()
            }
            Call::GrokLogout => {
                rpc.grok
                    .as_ref()
                    .expect("declared dependency")
                    .logout(cancel)
                    .await
                    .map_err(managed_oauth_error)?;
                Reply::empty()
            }
            Call::ProviderCatalog(params) => Reply::Catalog(CatalogResult {
                catalog: provider
                    .catalog_for(params.provider_id, cancel)
                    .await
                    .map_err(provider_error)?,
            }),
            Call::ProviderRefresh(params) => Reply::ProviderCatalog(
                provider
                    .refresh_for(params.provider_id, cancel)
                    .await
                    .map_err(provider_error)?,
            ),
            Call::ProviderModelsSetEnabled(params) => Reply::ProviderCatalog(
                provider
                    .set_enabled_models(
                        params.provider_id,
                        params.expected_version,
                        params.model_ids,
                        cancel,
                    )
                    .await
                    .map_err(provider_error)?,
            ),
            Call::ProviderModelEdit(params) => Reply::ProviderCatalog(
                provider
                    .update_model_config(
                        params.provider_id,
                        params.expected_version,
                        params.model_id,
                        Some(params.edit),
                        cancel,
                    )
                    .await
                    .map_err(provider_error)?,
            ),
            Call::ProviderModelReset(params) => Reply::ProviderCatalog(
                provider
                    .update_model_config(
                        params.provider_id,
                        params.expected_version,
                        params.model_id,
                        None,
                        cancel,
                    )
                    .await
                    .map_err(provider_error)?,
            ),
            Call::ProviderSelection(params) => {
                let cwd = match params.cwd {
                    Some(cwd) => Some(canonical_cwd(&cwd).await?),
                    None => None,
                };
                Reply::Selection(SelectionResult {
                    selection: provider
                        .selection(cwd.as_deref(), cancel)
                        .await
                        .map_err(provider_error)?,
                })
            }
            Call::ProviderSelect(params) => {
                if params.model_id.trim().is_empty() {
                    return Err(RpcError::invalid("modelId must not be empty"));
                }
                let cwd = match params.cwd {
                    Some(cwd) => Some(canonical_cwd(&cwd).await?),
                    None => None,
                };
                Reply::ProviderSelection(
                    provider
                        .select(
                            cwd.as_deref(),
                            dim_agent_types::ModelId {
                                provider_id: params.provider_id,
                                model_id: params.model_id,
                            },
                            params.expected_version,
                            cancel,
                        )
                        .await
                        .map_err(provider_error)?,
                )
            }
            Call::ProviderList => {
                Reply::ProviderList(provider.list(cancel).await.map_err(provider_error)?)
            }
            Call::ProviderSave(params) => Reply::ProviderSummary(
                provider
                    .save_static(params.provider, params.expected_version, cancel)
                    .await
                    .map_err(provider_error)?,
            ),
            Call::ProviderImportLegacy(params) => Reply::ProviderImport(
                provider
                    .import_legacy(params.source_path, cancel)
                    .await
                    .map_err(provider_error)?,
            ),
            Call::ProviderDelete(params) => {
                provider
                    .delete_static(params.provider_id, params.expected_version, cancel)
                    .await
                    .map_err(provider_error)?;
                Reply::empty()
            }
            _ => unreachable!("only provider management calls reach this boundary"),
        })
    }

    async fn skills_management(&self, rpc: &RpcService, call: Call) -> Result<Reply, RpcError> {
        Ok(match call {
            Call::SkillsCatalog(params) => {
                let catalog = rpc.skills.as_ref().ok_or_else(|| {
                    RpcError::app(
                        -32000,
                        "product_integration_pending",
                        "Skills integration is unavailable",
                    )
                })?;
                let cwd = match params.cwd {
                    Some(cwd) => Some(canonical_cwd(&cwd).await?),
                    None => None,
                };
                let view = catalog.query(cwd.as_deref()).await.map_err(skills_error)?;
                Reply::SkillsCatalog(SkillsCatalogResult {
                    version: view.version,
                    entries: view
                        .entries
                        .into_iter()
                        .map(|entry| SkillEntry {
                            id: entry.definition.id.clone(),
                            name: entry.definition.name.clone(),
                            description: entry.definition.description.clone(),
                            file_path: entry.definition.file_path.to_string_lossy().into_owned(),
                            allowed_tools: entry.definition.allowed_tools.clone(),
                            enabled: entry.enabled,
                            shadowed_by: match entry.status {
                                SkillStatus::Active => None,
                                SkillStatus::Shadowed { by } => Some(by),
                            },
                        })
                        .collect(),
                    issues: view.issues.into_iter().map(skill_issue).collect(),
                })
            }
            Call::SkillSetEnabled(params) => {
                if params.id.trim().is_empty() {
                    return Err(RpcError::invalid("id must not be empty"));
                }
                let catalog = rpc.skills.as_ref().ok_or_else(|| {
                    RpcError::app(
                        -32000,
                        "product_integration_pending",
                        "Skills integration is unavailable",
                    )
                })?;
                let cwd = match params.cwd {
                    Some(cwd) => Some(canonical_cwd(&cwd).await?),
                    None => None,
                };
                let (_, version) = catalog
                    .set_enabled(&params.id, params.enabled, cwd.as_deref())
                    .await
                    .map_err(skills_error)?;
                Reply::SkillsVersion(SkillsVersion { version })
            }
            Call::SkillsRefresh(params) => {
                let catalog = rpc.skills.as_ref().ok_or_else(|| {
                    RpcError::app(
                        -32000,
                        "product_integration_pending",
                        "Skills integration is unavailable",
                    )
                })?;
                let project = match params.cwd {
                    Some(cwd) => Some(canonical_cwd(&cwd).await?),
                    None => None,
                };
                let (version, issues) = catalog
                    .refresh(RefreshTarget {
                        user: true,
                        project,
                    })
                    .await
                    .map_err(skills_error)?;
                Reply::SkillsRefresh(SkillsRefreshResult {
                    version,
                    issues: issues.into_iter().map(skill_issue).collect(),
                })
            }
            _ => unreachable!("only management calls reach this boundary"),
        })
    }
}

async fn canonical_cwd(cwd: &str) -> Result<std::path::PathBuf, RpcError> {
    if !Path::new(cwd).is_absolute() {
        return Err(RpcError::invalid("cwd must be an absolute directory"));
    }
    let path = tokio::fs::canonicalize(cwd)
        .await
        .map_err(|_| RpcError::invalid("cwd is not accessible"))?;
    if !tokio::fs::metadata(&path)
        .await
        .map_err(|_| RpcError::invalid("cwd is not accessible"))?
        .is_dir()
    {
        return Err(RpcError::invalid("cwd must be a directory"));
    }
    Ok(path)
}

fn configuration_outcome(
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

fn skill_issue(issue: dim_agent_capabilities::skills::SkillIssue) -> SkillIssue {
    use dim_agent_capabilities::skills::SkillIssue as Source;
    match issue {
        Source::InvalidManifest { path, reason } | Source::Unreadable { path, reason } => {
            SkillIssue {
                path: path.to_string_lossy().into_owned(),
                message: reason,
            }
        }
        Source::RootUnreadable { root, reason } => SkillIssue {
            path: root.to_string_lossy().into_owned(),
            message: reason,
        },
        Source::ScanLimitReached { root } => SkillIssue {
            path: root.to_string_lossy().into_owned(),
            message: "Skill scan limit reached".into(),
        },
    }
}

fn oauth_error(error: crate::managed_provider::ManagedProviderError) -> RpcError {
    RpcError::app(-32000, error.code, error.message)
}
fn attempt_id(params: &LoginAttempt) -> Result<uuid::Uuid, RpcError> {
    uuid::Uuid::parse_str(&params.attempt_id)
        .map_err(|_| RpcError::invalid("attemptId must be a UUID"))
}
fn managed_attempt(status: crate::managed_oauth::AttemptStatus) -> Reply {
    use crate::managed_oauth::AttemptStatus;
    Reply::ManagedLoginState(match status {
        AttemptStatus::Pending => ManagedLoginState::Pending,
        AttemptStatus::Completed => ManagedLoginState::Completed,
        AttemptStatus::Failed(_) => ManagedLoginState::Failed {
            error: "oauth_failed".into(),
        },
        AttemptStatus::Cancelled => ManagedLoginState::Cancelled,
        AttemptStatus::Expired => ManagedLoginState::Expired,
    })
}
fn managed_oauth_error(error: crate::managed_oauth::OAuthError) -> RpcError {
    use crate::managed_oauth::OAuthError;
    let kind = match error {
        OAuthError::AccountChanged => "account_changed",
        OAuthError::AttemptNotFound => "login_attempt_unknown",
        OAuthError::AttemptClosed => "login_attempt_closed",
        OAuthError::Missing | OAuthError::Rejected => "account_required",
        OAuthError::InvalidStore(_) => "account_invalid_store",
        OAuthError::UnsupportedSchema => "account_unsupported_schema",
        OAuthError::RecoveryRequired => "account_recovery_required",
        OAuthError::InvalidCallback(_) | OAuthError::Config(_) => "account_invalid_input",
        OAuthError::TooManyAttempts => "login_attempt_limit",
        OAuthError::Transient(_) => "account_unavailable",
        OAuthError::Closed => "service_reloading",
        _ => "account_failed",
    };
    let message = match error {
        OAuthError::Storage(_) | OAuthError::Worker(_) => "OAuth account operation failed".into(),
        error => error.to_string(),
    };
    RpcError::app(-32000, kind, message)
}
fn provider_error(error: dim_agent_capabilities::host::HostError) -> RpcError {
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
    pub web: crate::web_entry::WebEntry,
}
impl ConnectionHub {
    fn new(config: Arc<ServerConfig>, control: Arc<ServerControl>) -> Self {
        Self {
            web: crate::web_entry::WebEntry::new(config.web_assets.clone()),
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
    pub control: Arc<ServerControl>,
    pub config: Arc<ServerConfig>,
    pub oauth: Option<Arc<crate::managed_provider::ManagedProviders>>,
    pub codex: Option<Arc<crate::codex_oauth::OpenAICodexOAuthService>>,
    pub grok: Option<Arc<crate::grok_oauth::GrokOAuthService>>,
    pub provider: Option<Arc<crate::provider::ProviderService>>,
    pub skills: Option<Arc<SkillsCatalog>>,
    pub mcp: Option<Arc<crate::mcp::McpService>>,
    pub settings: Option<Arc<crate::settings::SettingsService>>,
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
    ) -> Result<dim_agent_types::ModelSelection, dim_agent_capabilities::host::HostError> {
        if let Some(provider) = &self.provider {
            return provider
                .configuration(explicit, cwd.as_deref(), preferences, &self.unloading)
                .await;
        }
        if preferences != Default::default() {
            return Err(dim_agent_capabilities::host::HostError::failed(
                "Model preferences require a catalog with capability declarations; configure the API-key model's generation settings in --model-config",
            ));
        }
        self.index
            .model_selection
            .as_ref()
            .expect("execution profiles declare model selection")
            .0
            .resolve(explicit, cwd, self.unloading.clone())
            .await
    }
}

pub(crate) struct RpcPlugin {
    config: Arc<ServerConfig>,
}
impl RpcPlugin {
    pub fn new(config: Arc<ServerConfig>) -> Self {
        Self { config }
    }
}
/// Account and provider management of the product profile.
type ProductServices = (
    Arc<crate::managed_provider::ManagedProviders>,
    Arc<crate::codex_oauth::OpenAICodexOAuthService>,
    Arc<crate::grok_oauth::GrokOAuthService>,
    Arc<crate::provider::ProviderService>,
);
/// Public capabilities shared by every local execution profile.
type ExecutionServices = (
    Arc<SkillsCatalog>,
    Arc<crate::mcp::McpService>,
    Arc<crate::settings::SettingsService>,
);
impl TypedPlugin for RpcPlugin {
    type Deps = (
        Arc<AgentIndex>,
        // Restore observers must outlive admitted RPC commands during reload.
        rutis::Gate<crate::collaboration::Collaboration>,
        Arc<ConnectionHub>,
        Arc<ServerControl>,
        When<ProductServices>,
        When<ExecutionServices>,
    );
    fn name(&self) -> &str {
        "Rpc"
    }
    fn apply<'a>(
        &'a self,
        ctx: &'a Ctx,
        (index, _, hub, control, When(product), When(execution)): Self::Deps,
    ) -> BoxFuture<'a, Result<Effect, CordisError>> {
        Box::pin(async move {
            let (oauth, codex, grok, provider) = match product {
                Some((oauth, codex, grok, provider)) => {
                    (Some(oauth), Some(codex), Some(grok), Some(provider))
                }
                None => (None, None, None, None),
            };
            let (skills, mcp, settings) = match execution {
                Some((skills, mcp, settings)) => (Some(skills), Some(mcp), Some(settings)),
                None => (None, None, None),
            };
            let service = Arc::new(RpcService {
                index,
                control,
                config: self.config.clone(),
                oauth,
                codex,
                grok,
                provider,
                skills,
                mcp,
                settings,
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
        let product = self.config.product_home.is_some();
        (
            (),
            (),
            (),
            (),
            product.then_some(((), (), (), ())),
            self.config.local_execution().then_some(((), (), ())),
        )
    }
}

//! A connection's subscription to one Session: a listener on the Session
//! broadcast channel, pending approval replay, branch states, owners' notices
//! and the end.
use super::*;
use crate::tree_protocol as wire;
use dim_agent_capabilities::session_broadcast::{
    SessionBroadcast, SessionBroadcastEvent, session_channel,
};
use dim_agent_runtime::{BranchStatus, ModelProgress};
use dim_agent_types::{AgentEvent, EventKind};
use futures_util::{StreamExt, future::BoxFuture as Pending, stream::FuturesUnordered};
use rutis::Disposer;
use rutis::host::FiberStatusChanged;
use std::{collections::HashMap, sync::atomic::AtomicBool};
use tokio::sync::{mpsc, watch};

/// Replacing, detaching or closing ends the subscription and its listener.
pub(super) struct Attachment {
    pub generation: u64,
    pub _usage: crate::agents::SessionUse,
    pub cancel: CancellationToken,
    pub listener: Option<Disposer>,
    /// v3: signals plugin activation, so a reloaded progress cell is reacquired.
    pub availability: Option<Disposer>,
    pub delivery: Arc<Delivery>,
}
impl Drop for Attachment {
    /// Unregistering waits for in-flight deliveries; the connection does not.
    fn drop(&mut self) {
        self.cancel.cancel();
        for listener in [self.listener.take(), self.availability.take()]
            .into_iter()
            .flatten()
        {
            tokio::spawn(listener.dispose());
        }
    }
}

/// Changes within this interval reach a connection as one progress notice.
const PROGRESS_INTERVAL: std::time::Duration = std::time::Duration::from_millis(50);

type ProgressChange = Pending<'static, (String, Option<watch::Receiver<ModelProgress>>)>;
fn progress_change(branch: String, mut receiver: watch::Receiver<ModelProgress>) -> ProgressChange {
    Box::pin(async move {
        if receiver.changed().await.is_err() {
            return (branch, None);
        }
        tokio::time::sleep(PROGRESS_INTERVAL).await;
        (branch, Some(receiver))
    })
}

/// v3 model progress of the watched branches (ADR agent-029). ModelProvider
/// owns the cell and a reload replaces it, so every branch's current cell is
/// looked up again when a plugin becomes active.
#[derive(Default)]
struct Progress {
    changes: FuturesUnordered<ProgressChange>,
    /// Watched branches and the cell each currently follows, if any.
    cells: HashMap<String, Option<watch::Receiver<ModelProgress>>>,
    sent: HashMap<String, ModelProgress>,
}
impl Progress {
    /// Follows the branch's current cell, reporting its snapshot when the
    /// cell is new; false once the subscription is gone.
    fn follow(
        &mut self,
        connection: &Connection,
        session: &Session,
        generation: u64,
        branch: String,
    ) -> bool {
        let current = session.on::<ModelProgress>(&branch).ok().flatten();
        let followed = self.cells.entry(branch.clone()).or_default();
        let Some(mut receiver) = current else {
            *followed = None;
            return true;
        };
        if followed
            .as_ref()
            .is_some_and(|old| old.same_channel(&receiver))
        {
            return true;
        }
        *followed = Some(receiver.clone());
        let snapshot = receiver.borrow_and_update().clone();
        self.changes.push(progress_change(branch.clone(), receiver));
        self.report(connection, session, generation, branch, snapshot)
    }
    /// Reports a change of a followed cell; a replaced cell's change is dropped.
    fn changed(
        &mut self,
        connection: &Connection,
        session: &Session,
        generation: u64,
        branch: String,
        receiver: Option<watch::Receiver<ModelProgress>>,
    ) -> bool {
        let Some(mut receiver) = receiver else {
            return true;
        };
        if !matches!(self.cells.get(&branch), Some(Some(followed)) if followed.same_channel(&receiver))
        {
            return true;
        }
        let snapshot = receiver.borrow_and_update().clone();
        self.changes.push(progress_change(branch.clone(), receiver));
        self.report(connection, session, generation, branch, snapshot)
    }
    /// A plugin became active: a reloaded ModelProvider has a new cell.
    fn reacquire(&mut self, connection: &Connection, session: &Session, generation: u64) -> bool {
        let branches: Vec<_> = self.cells.keys().cloned().collect();
        branches
            .into_iter()
            .all(|branch| self.follow(connection, session, generation, branch))
    }
    fn report(
        &mut self,
        connection: &Connection,
        session: &Session,
        generation: u64,
        branch: String,
        progress: ModelProgress,
    ) -> bool {
        if self.sent.get(&branch) == Some(&progress) {
            return true;
        }
        self.sent.insert(branch.clone(), progress.clone());
        connection.notify(
            session.id(),
            generation,
            Notification::ModelProgress(wire::ModelProgressNotice::new(
                session.id().to_owned(),
                branch,
                progress,
            )),
        )
    }
}

enum Signal {
    /// A v2 branch without a state watch produced an event.
    Branch(String),
    /// The Session channel ended: every broadcast event was delivered.
    Closed,
}

/// One subscription's observation progress, independent of Session execution.
pub(super) struct Delivery {
    ctx: Ctx,
    session_id: String,
    recipient: String,
    issued: Mutex<u64>,
    reached: watch::Sender<u64>,
    cancel: CancellationToken,
}
impl Delivery {
    pub async fn wait(&self) -> Result<(), RpcError> {
        let mut reached = self.reached.subscribe();
        let token = {
            let mut issued = self.issued.lock().expect("delivery fence");
            if *reached.borrow() == u64::MAX {
                return Ok(());
            }
            if self.cancel.is_cancelled() {
                return Err(subscription_changed());
            }
            *issued += 1;
            let _ = self.ctx.emit(SessionBroadcast::new(
                &self.session_id,
                SessionBroadcastEvent::Fence {
                    recipient: self.recipient.clone(),
                    token: *issued,
                },
            ));
            *issued
        };
        loop {
            if *reached.borrow_and_update() >= token {
                return Ok(());
            }
            tokio::select! {
                _ = self.cancel.cancelled() => {
                    return if *reached.borrow() >= token {
                        Ok(())
                    } else {
                        Err(subscription_changed())
                    };
                }
                _ = reached.changed() => {},
            }
        }
    }
}

fn subscription_changed() -> RpcError {
    RpcError::app(
        -32000,
        "subscription_changed",
        "Session subscription changed before delivery completed",
    )
}

/// Projects broadcast records to this connection's notifications; never waits.
struct Forward {
    connection: Weak<Connection>,
    session_id: String,
    generation: u64,
    /// `Closed` was seen: the subscription forwards no later notice, so
    /// `session.closed` stays its last Session notification.
    ended: AtomicBool,
    watched: Arc<Mutex<HashSet<String>>>,
    signals: mpsc::UnboundedSender<Signal>,
    delivery: Arc<Delivery>,
}
impl Forward {
    async fn on_event(&self, event: &SessionBroadcast) -> Result<Option<()>, Error> {
        match &event.record {
            SessionBroadcastEvent::Closed => {
                self.ended.store(true, Ordering::Relaxed);
                self.acknowledge(u64::MAX);
                let _ = self.signals.send(Signal::Closed);
            }
            // v3 only: the v1 and v2 notification sets are frozen (#278).
            SessionBroadcastEvent::Notice(notice) => {
                if !self.ended.load(Ordering::Relaxed)
                    && let Some(connection) = self.connection.upgrade()
                    && connection.protocol_version() >= 3
                {
                    connection.notify(
                        &self.session_id,
                        self.generation,
                        Notification::Notice(wire::ForwardedNotice {
                            session_id: self.session_id.clone(),
                            notice: notice.clone(),
                        }),
                    );
                }
            }
            SessionBroadcastEvent::Fence { recipient, token } => {
                if recipient == &self.delivery.recipient {
                    self.acknowledge(*token);
                }
            }
            SessionBroadcastEvent::Event { branch_id, event } => {
                if let Some(connection) = self.connection.upgrade() {
                    let version = connection.protocol_version();
                    if version >= 2 && !self.watched.lock().expect("watched").contains(branch_id) {
                        let _ = self.signals.send(Signal::Branch(branch_id.clone()));
                    }
                    if let Some(notice) =
                        project(version, &self.session_id, branch_id, event.clone())
                    {
                        connection.notify(&self.session_id, self.generation, notice);
                    }
                }
            }
        }
        Ok(None)
    }
}

impl Forward {
    fn acknowledge(&self, token: u64) {
        if let Some(connection) = self.connection.upgrade() {
            let attachments = connection.attachments.lock().expect("attachments");
            if !connection.closed.is_cancelled()
                && attachments
                    .get(&self.session_id)
                    .is_some_and(|a| a.generation == self.generation)
            {
                self.delivery.reached.send_if_modified(|reached| {
                    if token > *reached {
                        *reached = token;
                        true
                    } else {
                        false
                    }
                });
            }
        }
    }
}

/// v1 carries main business events; approvals of every branch keep their
/// v1 notifications. v2 carries every branch.
fn project(
    version: u32,
    session_id: &str,
    branch_id: &str,
    event: Arc<AgentEvent>,
) -> Option<Notification> {
    match &event.kind {
        EventKind::ApprovalRequest(data) => Some(approval_request(
            version,
            session_id,
            branch_id.to_owned(),
            data.clone(),
        )),
        EventKind::ApprovalCancelled(data) if version == 1 => {
            Some(Notification::ApprovalCancelled(ApprovalCancelled {
                session_id: session_id.to_owned(),
                request_id: data.approval_id.clone(),
                reason: data.reason.clone(),
            }))
        }
        EventKind::ApprovalCancelled(data) => Some(Notification::TreeApprovalCancelled(
            wire::ApprovalCancelled {
                session_id: session_id.to_owned(),
                branch_id: branch_id.to_owned(),
                request_id: data.approval_id.clone(),
                reason: data.reason.clone(),
            },
        )),
        // v3 only: the v1 and v2 notification sets are frozen (#278).
        EventKind::ApprovalRespond(data) if version >= 3 => {
            Some(Notification::TreeApprovalResolved(wire::ApprovalResolved {
                session_id: session_id.to_owned(),
                branch_id: branch_id.to_owned(),
                request_id: data.approval_id.clone(),
                decision: data.decision,
            }))
        }
        _ if version == 1 => (branch_id == "main")
            .then(|| dim_agent_types::v1::SharedEvent::new(event))
            .flatten()
            .map(|event| {
                Notification::Event(EventNotice {
                    session_id: session_id.to_owned(),
                    event,
                })
            }),
        _ => wire::ProductEvent::new(event, version).map(|event| {
            Notification::TreeEvent(wire::EventNotice {
                session_id: session_id.to_owned(),
                branch_id: branch_id.to_owned(),
                event,
            })
        }),
    }
}

fn approval_request(
    version: u32,
    session_id: &str,
    branch_id: String,
    request: dim_agent_types::ApprovalRequestData,
) -> Notification {
    if version == 1 {
        Notification::ApprovalRequest(ApprovalRequest {
            session_id: session_id.to_owned(),
            request_id: request.approval_id,
            call: ApprovalCall {
                name: request.call.name,
                input: request.call.input,
            },
            reason: request.reason,
        })
    } else {
        Notification::TreeApprovalRequest(wire::ApprovalRequest {
            session_id: session_id.to_owned(),
            branch_id,
            request_id: request.approval_id,
            source: request.source,
            call: request.call,
            reason: request.reason,
        })
    }
}

fn state_notice(
    version: u32,
    session_id: &str,
    branch_id: &str,
    status: &BranchStatus,
) -> Notification {
    if version == 1 {
        Notification::State(StateNotice {
            session_id: session_id.to_owned(),
            running: status.running,
            stopping: status.stopping,
        })
    } else {
        Notification::BranchState(wire::StateNotice {
            session_id: session_id.to_owned(),
            state: wire::BranchState::new(branch_id.to_owned(), status),
        })
    }
}

type StateChange = Pending<'static, Option<(String, watch::Receiver<()>)>>;
fn state_change(id: String, mut receiver: watch::Receiver<()>) -> StateChange {
    Box::pin(async move {
        receiver.changed().await.ok()?;
        Some((id, receiver))
    })
}

impl Connection {
    /// Replies `{}`, then forwards the Session broadcast from this point on.
    /// Events before the listener are not replayed; pending approvals are.
    pub(super) fn attach(
        self: &Arc<Self>,
        rpc: &Arc<RpcService>,
        id: RequestId,
        params: Attach,
        usage: crate::agents::SessionUse,
    ) -> Result<(), RpcError> {
        let (host, session) = rpc
            .index
            .session(&params.session_id)
            .ok_or_else(RpcError::closed)?;
        let version = self.protocol_version();
        let session_id = session.id().to_owned();
        let generation = self.next_attachment.fetch_add(1, Ordering::Relaxed);
        let cancel = self.closed.child_token();
        let watched = Arc::new(Mutex::new(HashSet::new()));
        let (signals, receiver) = mpsc::unbounded_channel();
        let delivery = Arc::new(Delivery {
            ctx: host.ctx.clone(),
            session_id: session_id.clone(),
            recipient: uuid::Uuid::new_v4().to_string(),
            issued: Mutex::new(0),
            reached: watch::channel(0).0,
            cancel: cancel.clone(),
        });
        let forward = Arc::new(Forward {
            connection: Arc::downgrade(self),
            session_id: session_id.clone(),
            generation,
            ended: AtomicBool::new(false),
            watched: watched.clone(),
            signals,
            delivery: delivery.clone(),
        });
        let listener = host
            .ctx
            .listen::<SessionBroadcast>()
            .name(session_channel(&session_id))
            .on(move |event, _| {
                let forward = forward.clone();
                async move { forward.on_event(&event).await }
            })
            .map_err(|_| RpcError::closed())?;
        let (signal, active) = watch::channel(());
        let availability = if version >= 3 {
            let availability = Arc::new(super::mcp::Availability(signal));
            match host.ctx.on::<FiberStatusChanged>(move |event, call| {
                let availability = availability.clone();
                async move {
                    let ctx = call.ctx().clone();
                    availability.on_event(&ctx, &event, call).await
                }
            }) {
                Ok(availability) => Some((availability, active)),
                Err(_) => {
                    tokio::spawn(listener.dispose());
                    return Err(RpcError::closed());
                }
            }
        } else {
            None
        };
        let (availability, active) = availability.unzip();
        let ended = session.is_closing();
        {
            let mut attachments = self.attachments.lock().expect("attachments");
            if self
                .sender
                .send(Outbound::result(id, Ok(Reply::empty())), false)
                .is_err()
            {
                drop(attachments);
                for listener in [Some(listener), availability].into_iter().flatten() {
                    tokio::spawn(listener.dispose());
                }
                return Err(RpcError::wire(-32603, "Connection closed"));
            }
            attachments.insert(
                session_id.clone(),
                Attachment {
                    _usage: usage,
                    generation,
                    cancel: cancel.clone(),
                    listener: Some(listener),
                    availability,
                    delivery,
                },
            );
        }
        crate::approval::replay(&session, |branch_id, request| {
            self.notify(
                &session_id,
                generation,
                approval_request(version, &session_id, branch_id.to_owned(), request.clone()),
            );
        });
        if version >= 3 && rpc.mcp.is_some() {
            let this = self.clone();
            let host = host.clone();
            let session = session.clone();
            let cancel = cancel.clone();
            self.subscriptions.spawn(async move {
                this.follow_mcp(host, session, generation, cancel).await;
            });
        }
        let this = self.clone();
        self.subscriptions.spawn(async move {
            this.follow(
                session, generation, cancel, watched, receiver, active, ended,
            )
            .await;
            let mut attachments = this.attachments.lock().expect("attachments");
            if attachments
                .get(&session_id)
                .is_some_and(|a| a.generation == generation)
            {
                attachments.remove(&session_id);
                this.hub.ui.detach(&this.id, &session_id);
            }
        });
        Ok(())
    }

    /// Branch states (v3: and model progress) until the Session channel
    /// ends, then `session.closed`.
    #[allow(clippy::too_many_arguments)]
    async fn follow(
        &self,
        session: Session,
        generation: u64,
        cancel: CancellationToken,
        watched: Arc<Mutex<HashSet<String>>>,
        mut signals: mpsc::UnboundedReceiver<Signal>,
        mut active: Option<watch::Receiver<()>>,
        mut ended: bool,
    ) {
        let version = self.protocol_version();
        let id = session.id().to_owned();
        let mut watches = FuturesUnordered::<StateChange>::new();
        let mut reported = HashMap::<String, BranchStatus>::new();
        let mut progress = active.is_some().then(Progress::default);
        let watch_branch = |branch: String,
                            watches: &mut FuturesUnordered<StateChange>,
                            reported: &mut HashMap<String, BranchStatus>,
                            progress: &mut Option<Progress>| {
            let Ok(mut receiver) = session.watch(&branch) else {
                if session.ended_head(&branch).is_some() {
                    let closed = BranchStatus {
                        closed: true,
                        ..Default::default()
                    };
                    if reported.get(&branch) != Some(&closed) {
                        if !self.notify(
                            &id,
                            generation,
                            state_notice(version, &id, &branch, &closed),
                        ) {
                            return false;
                        }
                        reported.insert(branch.clone(), closed);
                    }
                }
                return true;
            };
            watched.lock().expect("watched").insert(branch.clone());
            receiver.borrow_and_update();
            let sent = match session.state(&branch) {
                Ok(current) => {
                    let sent = self.notify(
                        &id,
                        generation,
                        state_notice(version, &id, &branch, &current),
                    );
                    reported.insert(branch.clone(), current);
                    sent
                }
                Err(dim_agent_runtime::AgentError::Reloading(_)) => true,
                Err(_) => return true,
            };
            watches.push(state_change(branch.clone(), receiver));
            match progress {
                Some(progress) if sent => progress.follow(self, &session, generation, branch),
                _ => sent,
            }
        };
        let branches: Vec<_> = if version == 1 {
            vec!["main".to_owned()]
        } else {
            session.branches().into_iter().map(|info| info.id).collect()
        };
        for branch in branches {
            if !watch_branch(branch, &mut watches, &mut reported, &mut progress) {
                return;
            }
        }
        while !ended {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => return,
                signal = signals.recv() => match signal {
                    Some(Signal::Branch(branch)) => {
                        let known = watched.lock().expect("watched").contains(&branch);
                        if !known
                            && !watch_branch(branch, &mut watches, &mut reported, &mut progress)
                        {
                            return;
                        }
                    }
                    Some(Signal::Closed) | None => ended = true,
                },
                changed = watches.next(), if !watches.is_empty() => {
                    if let Some(Some((branch, mut receiver))) = changed {
                        receiver.borrow_and_update();
                        match session.state(&branch) {
                            Ok(next) => {
                                if reported.get(&branch) != Some(&next) {
                                    if !self.notify(&id, generation, state_notice(version, &id, &branch, &next)) {
                                        return;
                                    }
                                    reported.insert(branch.clone(), next.clone());
                                }
                                if !next.closed {
                                    watches.push(state_change(branch, receiver));
                                }
                            }
                            Err(dim_agent_runtime::AgentError::Reloading(_)) => {
                                watches.push(state_change(branch, receiver));
                            }
                            Err(_) => {
                                if session.ended_head(&branch).is_some() {
                                    let closed = BranchStatus {
                                        closed: true,
                                        ..Default::default()
                                    };
                                    if reported.get(&branch) != Some(&closed)
                                        && !self.notify(&id, generation, state_notice(version, &id, &branch, &closed))
                                    {
                                        return;
                                    }
                                    reported.insert(branch, closed);
                                }
                            }
                        }
                    }
                }
                changed = async {
                    match &mut progress {
                        Some(progress) if !progress.changes.is_empty() => progress.changes.next().await,
                        _ => std::future::pending().await,
                    }
                } => {
                    let progress = progress.as_mut().expect("progress follows v3");
                    if let Some((branch, receiver)) = changed
                        && !progress.changed(self, &session, generation, branch, receiver)
                    {
                        return;
                    }
                }
                result = async {
                    match &mut active {
                        Some(active) => active.changed().await,
                        None => std::future::pending().await,
                    }
                } => {
                    if result.is_err() {
                        active = None;
                        continue;
                    }
                    let progress = progress.as_mut().expect("progress follows v3");
                    if !progress.reacquire(self, &session, generation) {
                        return;
                    }
                }
            }
        }
        let cleanup = tokio::select! {
            biased;
            _ = cancel.cancelled() => return,
            result = session.closed() => result,
        };
        let branches: Vec<_> = if version == 1 {
            vec!["main".to_owned()]
        } else {
            session.branches().into_iter().map(|info| info.id).collect()
        };
        for branch in branches {
            let Ok(status) = session.state(&branch) else {
                return;
            };
            if !self.notify(
                &id,
                generation,
                state_notice(version, &id, &branch, &status),
            ) {
                return;
            }
        }
        self.notify(
            &id,
            generation,
            Notification::Closed(ClosedNotice {
                session_id: id.clone(),
                reason: cleanup
                    .err()
                    .map(|_| "Session cleanup failed".to_owned())
                    .or_else(|| session.failure().map(|_| "Session failed".to_owned())),
            }),
        );
    }

    pub(super) fn notify(&self, id: &str, generation: u64, notice: Notification) -> bool {
        let attachments = self.attachments.lock().expect("attachments");
        if !attachments
            .get(id)
            .is_some_and(|a| a.generation == generation)
        {
            return false;
        }
        self.sender.send(Outbound::notice(notice), false).is_ok()
    }
}

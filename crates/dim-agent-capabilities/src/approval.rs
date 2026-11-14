//! Branch approval: asks the user when Permission decides a call needs
//! approval. Writes `approval/*` events and waits for this branch's dispatched
//! `approval/respond`. Interrupts arrive through the caller's cancellation
//! token; branch close and reload settle what remains.
//!
//! Pending requests are replayed under the lock that removes them, so an end
//! notification follows any replay that included the request. Requests end through
//! a consumed `approval/respond` or an `approval/cancelled` terminal; automatic
//! policy settlement registers removal with its terminal. A removed request is
//! not replayed.
use crate::{
    host::HostError,
    permission::{Decision, Permission, PermissionState},
};
use dim_agent_runtime::{
    AgentError, CAPABILITY_RELOADING, Dispatcher, Names, QueryComparison, QueryFilter,
    QueryOperand, QueryOutput, QueryPage, QuerySelect, QuerySpec, QueryValue, StateCell, Timeline,
    TimelineQuery, TimelineQueryScope,
};
use dim_agent_types::{
    AgentEvent, ApprovalCallData, ApprovalCancelledData, ApprovalDecision, ApprovalRequestData,
    ApprovalRespondData, BranchSource, EventKind,
};
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, Key, TypeKey};
use rutis::{Ctx, Error};
use std::{
    collections::HashMap,
    num::NonZeroU64,
    sync::{Arc, Mutex},
};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

const CANCELLED: &str = "Tool execution cancelled";

pub struct AdmitCall {
    pub source: BranchSource,
    pub call: ApprovalCallData,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ApprovalRespondError {
    #[error("Approval is absent or already resolved")]
    Unknown,
    #[error("Approval was cancelled")]
    Cancelled,
    #[error("branch is closed")]
    Closed,
}

struct Pending {
    request: ApprovalRequestData,
    cancel: CancellationToken,
    /// `None`: a response arrived after the caller's batch was cancelled.
    /// A dropped sender means branch close or reload.
    response: oneshot::Sender<Option<ApprovalDecision>>,
}

#[derive(Default)]
struct Inner {
    closed: bool,
    /// Committed requests not yet ended, in request order.
    pending: Vec<Pending>,
    /// `respond` waiters by event id, released when the event is dispatched.
    responses: HashMap<String, oneshot::Sender<Result<(), ApprovalRespondError>>>,
}

pub struct Approval {
    timeline: Arc<Timeline>,
    dispatcher: Arc<Dispatcher>,
    permission: Arc<Permission>,
    policy: Arc<StateCell<PermissionState>>,
    inner: Mutex<Inner>,
}
#[rutis::service]
impl Approval {
    /// The committed requests no answer or cancellation ended, in request
    /// order: what the `approval` face's `pending` reads.
    pub fn pending(&self) -> Vec<ApprovalRequestData> {
        self.inner
            .lock()
            .unwrap()
            .pending
            .iter()
            .map(|item| item.request.clone())
            .collect()
    }
    /// Answers request `request_id` with `decision`: what the `approval`
    /// face's `decide` calls, and answered once the branch dispatched the
    /// answer.
    pub async fn decide(
        &self,
        request_id: String,
        decision: ApprovalDecision,
    ) -> std::result::Result<(), ApprovalRespondError> {
        self.respond(&request_id, decision).await
    }
    /// Calls `notify` with each pending request, in request order, under the
    /// Approval lock; see the module documentation for the ordering.
    #[service(skip)]
    pub fn replay(&self, mut notify: impl FnMut(&ApprovalRequestData)) {
        for item in &self.inner.lock().unwrap().pending {
            notify(&item.request);
        }
    }
    /// `Ok` means the call may start. `cancel` is the caller's batch token;
    /// the last cancellation check before the start happens here. The
    /// admission decision asks Permission's advisors with the call's source
    /// (Hooks A.6.1); subsequent rechecks are plain decisions without advisors.
    #[service(skip)]
    pub async fn admit(
        &self,
        call: AdmitCall,
        risk: Option<&str>,
        cancel: CancellationToken,
    ) -> Result<(), HostError> {
        let mut policies = self.policy.subscribe();
        let mut dispatched = self.dispatcher.watch();
        let admission = self
            .permission
            .admit_decision(&call.call, &call.source, risk, &cancel)
            .await?;
        let reason = match admission.decision {
            Decision::Allow if cancel.is_cancelled() => {
                return Err(HostError::Cancelled(CANCELLED.into()));
            }
            Decision::Allow => return Ok(()),
            Decision::Deny { reason } => return Err(HostError::failed(reason)),
            Decision::Ask { reason } => reason,
        };
        if cancel.is_cancelled() {
            return Err(HostError::Cancelled(CANCELLED.into()));
        }
        let checked = call.call.clone();
        let (response, mut received) = oneshot::channel();
        let registered = cancel.clone();
        let mut registered_id = None;
        let committed = self.timeline.append_registered(
            EventKind::ApprovalRequest(ApprovalRequestData {
                approval_id: uuid::Uuid::new_v4().to_string(),
                source: call.source,
                call: call.call,
                reason,
            })
            .into(),
            |event| {
                let EventKind::ApprovalRequest(data) = &event.kind else {
                    unreachable!("before transforms keep the event key");
                };
                let mut inner = self.inner.lock().unwrap();
                if inner.closed {
                    return Err(AgentError::Closed("Approval".into()));
                }
                inner.pending.push(Pending {
                    request: data.clone(),
                    cancel: registered,
                    response,
                });
                registered_id = Some(data.approval_id.clone());
                Ok(())
            },
        );
        let event = match committed {
            Ok(event) => event,
            Err(error) => {
                if let Some(id) = registered_id {
                    take(&mut self.inner.lock().unwrap(), &id);
                }
                return Err(match error {
                    AgentError::Closed(_) | AgentError::Cancelled(_) => {
                        HostError::Cancelled(error.to_string())
                    }
                    error => HostError::failed(error.to_string()),
                });
            }
        };
        let EventKind::ApprovalRequest(data) = &event.kind else {
            unreachable!("before transforms keep the event key");
        };
        let approval_id = &data.approval_id;
        let mut pending = PendingDrop {
            approval: self,
            approval_id: approval_id.clone(),
            armed: true,
        };
        let mut recheck = false;
        let decision = loop {
            tokio::select! {
                biased;
                decision = &mut received => break decision,
                _ = cancel.cancelled() => {
                    let taken = take(&mut self.inner.lock().unwrap(), approval_id);
                    if taken.is_some() {
                        self.cancelled(approval_id, CANCELLED);
                    }
                    return Err(HostError::Cancelled(CANCELLED.into()));
                }
                decision = async {
                    if !recheck {
                        policies.changed().await.map_err(|_| {
                            HostError::Cancelled(CAPABILITY_RELOADING.into())
                        })?;
                    }
                    let through = loop {
                        let state = dispatched.borrow_and_update().clone();
                        if let Some(error) = state.failure {
                            return Err(HostError::failed(error.to_string()));
                        }
                        if state.sequence >= self.timeline.committed() {
                            break state.sequence;
                        }
                        dispatched.changed().await.map_err(|_| {
                            HostError::Cancelled(CAPABILITY_RELOADING.into())
                        })?;
                    };
                    self.permission.decide(&checked, risk, &cancel).await.map(|decision| (through, decision))
                } => {
                    recheck = false;
                    let (through, decision) = decision?;
                    let result = match decision {
                        _ if cancel.is_cancelled() => Err(HostError::Cancelled(CANCELLED.into())),
                        Decision::Allow if !admission.advisor_approval => Ok(()),
                        Decision::Allow | Decision::Ask { .. } => continue,
                        Decision::Deny { reason } => Err(HostError::failed(reason)),
                    };
                    let reason = match &result {
                        Ok(()) => "Approval no longer required by the permission policy".into(),
                        Err(error) => error.to_string(),
                    };
                    let mut deferred = false;
                    let committed = self.timeline.append_registered(
                        EventKind::ApprovalCancelled(ApprovalCancelledData {
                            approval_id: approval_id.clone(),
                            reason,
                        }).into(),
                        |event| {
                            if let Some(error) = self.dispatcher.failure() {
                                return Err(error);
                            }
                            if through + 1 != event.sequence
                                || take(&mut self.inner.lock().unwrap(), approval_id).is_none()
                            {
                                deferred = true;
                                return Err(AgentError::Cancelled("Approval settlement deferred".into()));
                            }
                            Ok(())
                        },
                    );
                    if deferred {
                        recheck = true;
                        continue;
                    }
                    let terminal = committed.map_err(|error| HostError::failed(error.to_string()))?;
                    pending.disarm();
                    let mut published = self.timeline.watch();
                    loop {
                        if let Some(error) = self.timeline.failure().or_else(|| self.dispatcher.failure()) {
                            return Err(HostError::failed(error.to_string()));
                        }
                        let (sequence, closed) = *published.borrow_and_update();
                        if sequence >= terminal.sequence {
                            break;
                        }
                        if closed {
                            return Err(HostError::Cancelled(CAPABILITY_RELOADING.into()));
                        }
                        tokio::select! {
                            _ = cancel.cancelled() => return Err(HostError::Cancelled(CANCELLED.into())),
                            changed = published.changed() => {
                                changed.map_err(|_| HostError::Cancelled(CAPABILITY_RELOADING.into()))?;
                            }
                            changed = dispatched.changed() => {
                                changed.map_err(|_| HostError::Cancelled(CAPABILITY_RELOADING.into()))?;
                            }
                        }
                    }
                    if result.is_ok()
                        && let Decision::Deny { reason } | Decision::Ask { reason } =
                            self.permission.decide(&checked, risk, &cancel).await?
                    {
                        return Err(HostError::failed(reason));
                    }
                    if cancel.is_cancelled() {
                        return Err(HostError::Cancelled(CANCELLED.into()));
                    }
                    return result;
                }
            }
        };
        pending.disarm();
        match decision {
            Err(_) => Err(HostError::Cancelled(CAPABILITY_RELOADING.into())),
            Ok(None) => Err(HostError::Cancelled(CANCELLED.into())),
            Ok(Some(ApprovalDecision::Deny)) => {
                Err(HostError::failed("Tool execution denied by user"))
            }
            Ok(Some(ApprovalDecision::Allow)) => {
                match self.permission.decide(&checked, risk, &cancel).await {
                    Ok(Decision::Deny { reason }) => {
                        self.cancelled(approval_id, &reason);
                        Err(HostError::failed(reason))
                    }
                    Ok(_) if cancel.is_cancelled() => {
                        self.cancelled(approval_id, CANCELLED);
                        Err(HostError::Cancelled(CANCELLED.into()))
                    }
                    Ok(_) => Ok(()),
                    Err(error) => {
                        self.cancelled(approval_id, CANCELLED);
                        Err(error)
                    }
                }
            }
        }
    }
    /// `Ok` means the decision was dispatched and accepted, not that the tool
    /// started.
    #[service(skip)]
    pub async fn respond(
        &self,
        approval_id: &str,
        decision: ApprovalDecision,
    ) -> Result<(), ApprovalRespondError> {
        {
            let inner = self.inner.lock().unwrap();
            if inner.closed {
                return Err(ApprovalRespondError::Closed);
            }
            if !inner
                .pending
                .iter()
                .any(|item| item.request.approval_id == approval_id)
            {
                return Err(ApprovalRespondError::Unknown);
            }
        }
        let (waiter, result) = oneshot::channel();
        let mut registered_id = None;
        let committed = self.timeline.append_registered(
            EventKind::ApprovalRespond(ApprovalRespondData {
                approval_id: approval_id.to_owned(),
                decision,
            })
            .into(),
            |event| {
                let mut inner = self.inner.lock().unwrap();
                if inner.closed {
                    return Err(AgentError::Closed("Approval".into()));
                }
                inner.responses.insert(event.id.clone(), waiter);
                registered_id = Some(event.id.clone());
                Ok(())
            },
        );
        if committed.is_err() {
            if let Some(id) = registered_id {
                self.inner.lock().unwrap().responses.remove(&id);
            }
            return Err(ApprovalRespondError::Closed);
        }
        result.await.unwrap_or(Err(ApprovalRespondError::Closed))
    }
    async fn rebuild(&self) -> dim_agent_runtime::Result<u64> {
        let through = self.timeline.committed();
        let mut request = TimelineQuery {
            scope: TimelineQueryScope::Branch,
            from: None,
            through: Some(through),
            include_publishing: true,
            query: QuerySpec {
                filter: Some(QueryFilter::And {
                    conditions: vec![
                        QueryFilter::Compare {
                            left: QueryOperand::Field {
                                path: "kind".into(),
                            },
                            comparison: QueryComparison::Eq,
                            right: QueryOperand::Value {
                                value: "approval/request".into(),
                            },
                        },
                        QueryFilter::Not {
                            condition: Box::new(QueryFilter::Exists {
                                condition: Box::new(QueryFilter::And {
                                    conditions: vec![
                                        QueryFilter::In {
                                            value: QueryOperand::Field {
                                                path: "kind".into(),
                                            },
                                            values: vec![
                                                "approval/respond".into(),
                                                "approval/cancelled".into(),
                                            ],
                                        },
                                        QueryFilter::Compare {
                                            left: QueryOperand::Field {
                                                path: "data.approval_id".into(),
                                            },
                                            comparison: QueryComparison::Eq,
                                            right: QueryOperand::Outer {
                                                path: "data.approval_id".into(),
                                            },
                                        },
                                    ],
                                }),
                            }),
                        },
                    ],
                }),
                select: QuerySelect::Fields {
                    fields: vec!["data.approval_id".into()],
                },
                group_by: Vec::new(),
                pick: None,
                order_by: Vec::new(),
            },
            page: Some(QueryPage {
                limit: NonZeroU64::new(256).expect("a page contains identities"),
                after: None,
            }),
        };
        loop {
            let reply = self.timeline.query(request.clone()).await?;
            let QueryOutput::Rows { rows } = reply.result else {
                unreachable!("approval recovery selects identity fields")
            };
            for row in rows {
                let Some(QueryValue::Value {
                    value: serde_json::Value::String(approval_id),
                }) = row.values.into_iter().next()
                else {
                    return Err(AgentError::fault(
                        "Approval",
                        "an approval request has no approval identity",
                    ));
                };
                self.timeline.append(
                    EventKind::ApprovalCancelled(ApprovalCancelledData {
                        approval_id,
                        reason: "Approval cancelled: execution did not survive recovery".into(),
                    })
                    .into(),
                )?;
            }
            let Some(next) = reply.next else {
                break;
            };
            request
                .page
                .as_mut()
                .expect("identity queries are paged")
                .after = Some(next);
        }
        Ok(through)
    }
    fn consume(&self, event: &AgentEvent) {
        let EventKind::ApprovalRespond(data) = &event.kind else {
            return;
        };
        let mut inner = self.inner.lock().unwrap();
        let mut cancelled = false;
        let result = match take(&mut inner, &data.approval_id) {
            Some(item) if item.cancel.is_cancelled() => {
                let _ = item.response.send(None);
                cancelled = true;
                Err(ApprovalRespondError::Cancelled)
            }
            Some(item) => {
                let _ = item.response.send(Some(data.decision));
                Ok(())
            }
            None => Err(ApprovalRespondError::Unknown),
        };
        if let Some(waiter) = inner.responses.remove(&event.id) {
            let _ = waiter.send(result);
        }
        drop(inner);
        if cancelled {
            self.cancelled(&data.approval_id, CANCELLED);
        }
    }
    /// Ends a request. Call without the lock: Timeline registration callbacks
    /// take it under the commit lock. A closed branch no longer records it.
    fn cancelled(&self, approval_id: &str, reason: &str) {
        let _ = self.timeline.append(
            EventKind::ApprovalCancelled(ApprovalCancelledData {
                approval_id: approval_id.to_owned(),
                reason: reason.to_owned(),
            })
            .into(),
        );
    }
    /// Settles every pending request as the branch closes or the capability
    /// reloads.
    fn close(&self) {
        let pending: Vec<_> = {
            let mut inner = self.inner.lock().unwrap();
            inner.closed = true;
            inner.responses.clear();
            std::mem::take(&mut inner.pending)
        };
        for item in pending {
            self.cancelled(&item.request.approval_id, CAPABILITY_RELOADING);
        }
    }
}

/// Removes a pending request when its admission future is dropped, so a
/// cancelled child does not leave a prompt that can authorize nothing.
struct PendingDrop<'a> {
    approval: &'a Approval,
    approval_id: String,
    armed: bool,
}
impl PendingDrop<'_> {
    fn disarm(&mut self) {
        self.armed = false;
    }
}
impl Drop for PendingDrop<'_> {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let taken = take(&mut self.approval.inner.lock().unwrap(), &self.approval_id);
        if taken.is_some() {
            self.approval.cancelled(&self.approval_id, CANCELLED);
        }
    }
}

fn take(inner: &mut Inner, approval_id: &str) -> Option<Pending> {
    let index = inner
        .pending
        .iter()
        .position(|item| item.request.approval_id == approval_id)?;
    Some(inner.pending.remove(index))
}

pub struct ApprovalPlugin {
    approval: Key<Approval>,
    timeline: Key<Timeline>,
    dispatcher: Key<Dispatcher>,
    permission: Key<Permission>,
    policy: Key<StateCell<PermissionState>>,
}
impl ApprovalPlugin {
    pub fn new(names: &Names<'_>) -> Result<Self, Error> {
        Ok(Self {
            approval: names.key()?,
            timeline: names.key()?,
            dispatcher: names.key()?,
            permission: names.key()?,
            policy: names.state_cell()?,
        })
    }
}
impl DynPlugin for ApprovalPlugin {
    fn name(&self) -> &str {
        "Approval"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![
            self.timeline.erase(),
            self.dispatcher.erase(),
            self.permission.erase(),
            self.policy.erase(),
        ]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let timeline = ctx.require_at(&self.timeline)?;
            let dispatcher = ctx.require_at(&self.dispatcher)?;
            let permission = ctx.require_at(&self.permission)?;
            let policy = ctx.require_at(&self.policy)?;
            let service = Arc::new(Approval {
                timeline,
                dispatcher: dispatcher.clone(),
                permission,
                policy,
                inner: Mutex::new(Inner::default()),
            });
            let closing = service.clone();
            ctx.effect(move || async move {
                closing.close();
                Ok(())
            })?;
            let rebuild = service.clone();
            let live = service.clone();
            dispatcher
                .consume(
                    ctx,
                    "Approval".into(),
                    ["approval/respond"].map(String::from).to_vec(),
                    Arc::new(move || {
                        let rebuild = rebuild.clone();
                        Box::pin(async move { rebuild.rebuild().await })
                    }),
                    Arc::new(move |event| {
                        live.consume(&event);
                        Box::pin(async { Ok(()) })
                    }),
                )
                .await?;
            ctx.provide_at(self.approval.clone(), service)?;
            Ok(Effect::Done)
        })
    }
}

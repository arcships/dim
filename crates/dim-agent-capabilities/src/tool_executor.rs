pub mod read;

use crate::{
    approval::{AdmitCall, Approval},
    host::*,
    tool_binding::ToolBinding,
    tool_registry::{ToolClaim, ToolConcurrency, ToolContribution},
};
use dim_agent_runtime::{
    AgentError, CAPABILITY_RELOADING, Dispatcher, Names, QueryComparison, QueryFilter,
    QueryOperand, QueryOutput, QueryPage, QueryPick, QuerySelect, QuerySpec, QueryValue, Timeline,
    TimelineIdentity, TimelineQuery, TimelineQueryScope,
};
use dim_agent_types::*;
use futures_util::FutureExt;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, Key, TypeKey};
use rutis::{Ctx, Error};
use std::{
    collections::BTreeMap,
    future::Future,
    num::NonZeroU64,
    panic::AssertUnwindSafe,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{
    sync::{oneshot, watch},
    time::timeout,
};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

#[derive(Default)]
struct ToolState {
    current: Option<CancellationToken>,
    /// The context of the current top-level batch. A nested gate opens only
    /// for this context.
    current_context: Option<ToolExecutionContext>,
    /// Set by `stop/interrupt` until the interrupted turn ends or the next
    /// one starts: requests that still arrive belong to the interrupted turn
    /// and are cancelled. Folded from consumed events, so rebuilt on load.
    interrupted: Option<String>,
    closed: bool,
}
pub struct ToolExecutor {
    state: Mutex<ToolState>,
    tasks: TaskTracker,
    errors: Mutex<Vec<AgentError>>,
    binding: Arc<ToolBinding>,
    approval: Arc<Approval>,
    timeline: Arc<Timeline>,
}

/// The child-call gate of one outer tool call. `invoke` writes a child
/// `step/tool-call` batch and its terminal `step/tool-result`; `close` rejects
/// new children and waits for every child task.
pub trait NestedGate: Send + Sync + 'static {
    fn invoke(
        &self,
        call: ToolCall,
    ) -> Pin<Box<dyn Future<Output = Result<ToolResult, HostError>> + Send + '_>>;
    fn close(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>>;
    fn effects(&self) -> Vec<ToolEffect>;
}

type ChildEffects = Arc<Mutex<Vec<(u64, Vec<ToolEffect>)>>>;

struct NestedGateImpl {
    executor: Arc<ToolExecutor>,
    parent_call_id: String,
    context: ToolExecutionContext,
    cancel: CancellationToken,
    tasks: TaskTracker,
    state: Mutex<GateState>,
    next: AtomicU64,
    effects: ChildEffects,
}

#[derive(Default)]
struct GateState {
    closing: bool,
}

impl NestedGate for NestedGateImpl {
    fn invoke(
        &self,
        call: ToolCall,
    ) -> Pin<Box<dyn Future<Output = Result<ToolResult, HostError>> + Send + '_>> {
        Box::pin(async move {
            let (sender, receiver) = oneshot::channel();
            {
                let state = self.state.lock().unwrap();
                if state.closing {
                    return Err(HostError::failed("the Code Mode child gate is closed"));
                }
                let executor = self.executor.clone();
                let parent_call_id = self.parent_call_id.clone();
                let context = self.context.clone();
                let cancel = self.cancel.clone();
                let effects = self.effects.clone();
                let sequence = self.next.fetch_add(1, Ordering::SeqCst);
                self.tasks.spawn(async move {
                    let result = run_child(
                        executor,
                        parent_call_id,
                        context,
                        call,
                        cancel,
                        sequence,
                        effects,
                    )
                    .await;
                    let _ = sender.send(result);
                });
            }
            receiver
                .await
                .unwrap_or_else(|_| Err(HostError::failed("a Code Mode child task ended")))
        })
    }

    fn close(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(async move {
            self.state.lock().unwrap().closing = true;
            self.cancel.cancel();
            self.tasks.close();
            let _ = timeout(Duration::from_secs(5), self.tasks.wait()).await;
        })
    }

    fn effects(&self) -> Vec<ToolEffect> {
        let mut collected = std::mem::take(&mut *self.effects.lock().unwrap());
        collected.sort_by_key(|(sequence, _)| *sequence);
        collected
            .into_iter()
            .flat_map(|(_, effects)| effects)
            .collect()
    }
}

async fn run_child(
    executor: Arc<ToolExecutor>,
    parent_call_id: String,
    context: ToolExecutionContext,
    call: ToolCall,
    cancel: CancellationToken,
    sequence: u64,
    effects: ChildEffects,
) -> Result<ToolResult, HostError> {
    let batch = executor
        .timeline
        .append(
            EventDraft::from(EventKind::ToolCall(ToolCallData {
                turn_id: context.turn_id.clone(),
                request_id: context.request_id.clone(),
                calls: vec![call.clone()],
            }))
            .with_parent_call_id(parent_call_id.clone()),
        )
        .map_err(|error| HostError::failed(error.to_string()))?;
    let child_call_id = call.id.clone();
    let outcome = AssertUnwindSafe(dim_agent_runtime::core::with_parent_call_id(
        child_call_id,
        async {
            execute(
                &executor.binding,
                &executor.approval,
                call.clone(),
                context.clone(),
                cancel,
            )
            .await
        },
    ))
    .catch_unwind()
    .await;
    let result = match outcome {
        Ok(Ok(result)) => result,
        Ok(Err(error)) => ToolResult::error(error.to_string()),
        Err(panic) => ToolResult::error(panic_message(panic)),
    };
    if let Some(child_effects) = result.effects.clone() {
        effects.lock().unwrap().push((sequence, child_effects));
    }
    executor
        .timeline
        .append(
            EventDraft::from(EventKind::ToolResult(ToolResultData {
                turn_id: context.turn_id,
                request_id: context.request_id,
                call_event_id: Some(batch.id.clone()),
                results: vec![ToolCallResult {
                    call_id: call.id,
                    result: result.clone(),
                }],
            }))
            .with_parent_call_id(parent_call_id),
        )
        .map_err(|error| HostError::failed(error.to_string()))?;
    Ok(result)
}
impl ToolExecutor {
    /// The declarations the request pinned, for a program that may only see
    /// the tools its own request declared.
    pub fn definitions(&self, request_id: &str) -> Result<Vec<ToolDefinition>, String> {
        self.binding.definitions(request_id)
    }
    /// Opens the child-call gate for the current top-level batch. A gate
    /// exists only while its context matches the current batch; a closed or
    /// interrupted executor refuses.
    pub fn open(
        self: &Arc<Self>,
        parent_call_id: String,
        context: ToolExecutionContext,
        cancel: CancellationToken,
    ) -> Result<Arc<dyn NestedGate>, HostError> {
        let state = self.state.lock().unwrap();
        if state.closed {
            return Err(HostError::failed("ToolExecutor is closed"));
        }
        if let Some(reason) = &state.interrupted {
            return Err(HostError::failed(format!(
                "ToolExecutor is interrupted: {reason}"
            )));
        }
        if state.current.is_none() || state.current_context.as_ref() != Some(&context) {
            return Err(HostError::failed(
                "no current tool batch accepts a Code Mode child",
            ));
        }
        drop(state);
        Ok(Arc::new(NestedGateImpl {
            executor: self.clone(),
            parent_call_id,
            context,
            cancel,
            tasks: TaskTracker::new(),
            state: Mutex::new(GateState::default()),
            next: AtomicU64::new(0),
            effects: Arc::new(Mutex::new(Vec::new())),
        }))
    }
}
pub struct ToolExecutorPlugin {
    pub emit_progress: bool,
    executor: Key<ToolExecutor>,
    timeline: Key<Timeline>,
    dispatcher: Key<Dispatcher>,
    binding: Key<ToolBinding>,
    approval: Key<Approval>,
    identity: Key<TimelineIdentity>,
}
impl ToolExecutorPlugin {
    pub fn new(names: &Names<'_>) -> Result<Self, Error> {
        Ok(Self {
            emit_progress: false,
            executor: names.key()?,
            timeline: names.key()?,
            dispatcher: names.key()?,
            binding: names.key()?,
            approval: names.key()?,
            identity: names.key()?,
        })
    }
}
impl DynPlugin for ToolExecutorPlugin {
    fn name(&self) -> &str {
        "ToolExecutor"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![
            self.timeline.erase(),
            self.dispatcher.erase(),
            self.binding.erase(),
            self.approval.erase(),
            self.identity.erase(),
        ]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let timeline = ctx.require_at(&self.timeline)?;
            let dispatcher = ctx.require_at(&self.dispatcher)?;
            let binding = ctx.require_at(&self.binding)?;
            let approval = ctx.require_at(&self.approval)?;
            let identity = ctx.require_at(&self.identity)?;
            let emit_progress = self.emit_progress;
            let service = Arc::new(ToolExecutor {
                state: Mutex::new(ToolState::default()),
                tasks: TaskTracker::new(),
                errors: Mutex::new(Vec::new()),
                binding: binding.clone(),
                approval: approval.clone(),
                timeline: timeline.clone(),
            });
            let closing = service.clone();
            ctx.effect(move || async move {
                let cancel = {
                    let mut state = closing.state.lock().unwrap();
                    state.closed = true;
                    state.current.clone()
                };
                if let Some(cancel) = cancel {
                    cancel.cancel();
                }
                closing.tasks.close();
                closing.tasks.wait().await;
                AgentError::aggregate(closing.errors.lock().unwrap().clone()).map_err(Into::into)
            })?;
            let owner = service.clone();
            let faults = dispatcher.clone();
            let lifecycle = ctx.cancellation_token();
            let read = owner.clone();
            let read_timeline = timeline.clone();
            dispatcher.consume(
                ctx,
                "ToolExecutor".into(),
                [
                    "step/tool-call",
                    "stop/interrupt",
                    "turn/start",
                    "turn/end",
                ]
                .map(String::from)
                .to_vec(),
                Arc::new(move || {
                    let read = read.clone();
                    let timeline = read_timeline.clone();
                    Box::pin(async move {
                        rebuild(&read,&timeline).await
                    })
                }),
                Arc::new(move |event| {
                    let owner = owner.clone();
                    let timeline = timeline.clone();
                    let faults = faults.clone();
                    let lifecycle = lifecycle.clone();
                    let binding = binding.clone();
                    let approval = approval.clone();
                    let identity = identity.clone();
                    Box::pin(async move {
                    (|| {
                        let mut state = owner.state.lock().unwrap();
                        if let EventKind::StopInterrupt(data) = &event.kind {
                            state.interrupted = Some(data.reason.clone());
                            let cancel = state.current.clone();
                            drop(state);
                            if let Some(cancel) = cancel {
                                cancel.cancel();
                            }
                            return Ok(());
                        }
                        if matches!(event.kind, EventKind::TurnStart(_) | EventKind::TurnEnd(_)) {
                            state.interrupted = None;
                            return Ok(());
                        }
                        let EventKind::ToolCall(batch) = &event.kind else {
                            unreachable!()
                        };
                        if event.parent_call_id.is_some() {
                            return Ok(());
                        }
                        if state.closed {
                            return Err(AgentError::Closed("ToolExecutor".into()));
                        }
                        if state.current.is_some() {
                            return Err(AgentError::fault(
                                "ToolExecutor",
                                "concurrent tool batches",
                            ));
                        }
                        if let Some(reason) = state.interrupted.clone() {
                            drop(state);
                            timeline.append(
                                EventKind::ToolResult(ToolResultData {
                                    turn_id: batch.turn_id.clone(),
                                    request_id: batch.request_id.clone(),
                                    call_event_id: Some(event.id.clone()),
                                    results: batch.calls.iter().map(|call| ToolCallResult {
                                        call_id: call.id.clone(),
                                        result: ToolResult::error(format!(
                                            "Tool was not executed: {reason}"
                                        )),
                                    }).collect(),
                                }).into(),
                            )?;
                            return Ok(());
                        }
                        let cancel = lifecycle.child_token();
                        let generation = lifecycle.clone();
                        state.current = Some(cancel.clone());
                        state.current_context = Some(ToolExecutionContext {
                            turn_id: batch.turn_id.clone(),
                            request_id: batch.request_id.clone(),
                            branch_id: identity.branch_id.clone(),
                        });
                        let batch = batch.clone();
                        let call_event_id = event.id.clone();
                        let worker = owner.clone();
                        let timeline = timeline.clone();
                        let faults = faults.clone();
                        let binding = binding.clone();
                        let approval = approval.clone();
                        let identity = identity.clone();
                        owner.tasks.spawn(async move {
                            let outcome = AssertUnwindSafe(async {
                                if cancel.is_cancelled() {
                                    let results = batch.calls.iter().map(|call| ToolCallResult {
                                        call_id: call.id.clone(),
                                        result: ToolResult::error("Tool cancelled before execution"),
                                    }).collect::<Vec<_>>();
                                    return Ok((results, Vec::new()));
                                }
                                let count = batch.calls.len();
                                let classes = batch.calls.iter().map(|call| {
                                    binding
                                        .handler(&batch.request_id, &call.name)
                                        .map(|tool| tool.handler.concurrency(call))
                                        .unwrap_or(ToolConcurrency::Exclusive)
                                });
                                let (turn, segment_of, mut waiters) = Turn::new(classes, count);
                                let calls = batch.calls.iter().cloned().enumerate().map(
                                    |(index, call)| {
                                    let binding = binding.clone();
                                    let approval = approval.clone();
                                    let timeline = timeline.clone();
                                    let cancel = cancel.clone();
                                    let event_id = call_event_id.clone();
                                    let session_id = identity.session_id.clone();
                                    let segment = segment_of[index];
                                    let turn = turn.clone();
                                    let mut barrier = std::mem::replace(
                                        &mut waiters[index],
                                        watch::channel(0).1,
                                    );
                                    let context = ToolExecutionContext {
                                        turn_id: batch.turn_id.clone(),
                                        request_id: batch.request_id.clone(),
                                        branch_id: identity.branch_id.clone(),
                                    };
                                    async move {
                                        // Releases this call's step on every exit:
                                        // a result, a refusal, a panic or a dropped
                                        // future.
                                        let _settled = Settled { turn, segment };
                                        if cancel.is_cancelled() {
                                            return (
                                                index,
                                                ToolCallResult {
                                                    call_id: call.id,
                                                    result: ToolResult::error("Tool cancelled before execution"),
                                                },
                                                vec![],
                                            );
                                        }
                                        let mut errors = Vec::new();
                                        if emit_progress && let Err(error) = timeline.append(
                                            EventKind::ToolStart(ToolStartData {
                                                turn_id: context.turn_id.clone(),
                                                request_id: context.request_id.clone(),
                                                call_event_id: event_id.clone(),
                                                call_id: call.id.clone(),
                                            }).into(),
                                        ) {
                                            return (
                                                index,
                                                ToolCallResult {
                                                    call_id: call.id,
                                                    result: ToolResult::error(
                                                        "Tool was not executed: progress admission failed",
                                                    ),
                                                },
                                                vec![error],
                                            );
                                        }
                                        let span = tracing::info_span!("tool.execute", session_id = %session_id, branch_id = %context.branch_id, turn_id = %context.turn_id, request_id = %context.request_id, call_id = %call.id, outcome = tracing::field::Empty);
                                        let interrupt = cancel.clone();
                                        let (result, outcome) = if cancel.is_cancelled() {
                                            (ToolResult::error("Tool cancelled before execution"), "interrupted")
                                        } else {
                                            let admitted = AssertUnwindSafe(admit(
                                                &binding,
                                                &approval,
                                                call.clone(),
                                                &context,
                                                cancel.clone(),
                                            ))
                                            .catch_unwind()
                                            .await;
                                            match admitted {
                                                Ok(Ok(admitted)) => {
                                                    // Two things hold before a call runs:
                                                    // its own admission is done and every
                                                    // earlier step settled. A sibling's
                                                    // admission is not one of them.
                                                    let mut settle = |outcome| {
                                                        settled(
                                                            outcome,
                                                            interrupt.is_cancelled(),
                                                            &mut errors,
                                                        )
                                                    };
                                                    let ahead = |head: &usize| *head >= segment;
                                                    if segment > 0 {
                                                        let waited = barrier.wait_for(ahead);
                                                        let ready = tokio::select! {
                                                            biased;
                                                            _ = cancel.cancelled() => false,
                                                            waited = waited => waited.is_ok(),
                                                        };
                                                        if !ready {
                                                            (ToolResult::error("Tool cancelled before execution"), "interrupted")
                                                        } else {
                                                            let outcome = AssertUnwindSafe(run(
                                                                admitted,
                                                                context.clone(),
                                                                cancel.clone(),
                                                            ))
                                                            .catch_unwind()
                                                            .await;
                                                            settle(outcome)
                                                        }
                                                    } else {
                                                        let outcome = AssertUnwindSafe(run(
                                                            admitted,
                                                            context.clone(),
                                                            cancel.clone(),
                                                        ))
                                                        .catch_unwind()
                                                        .await;
                                                        settle(outcome)
                                                    }
                                                }
                                                Ok(Err(error)) => {
                                                    let outcome = if matches!(
                                                        error,
                                                        HostError::Cancelled(_)
                                                    ) {
                                                        "interrupted"
                                                    } else {
                                                        "failure"
                                                    };
                                                    let reloading = match &error {
                                                        HostError::Cancelled(reason) => {
                                                            reason == CAPABILITY_RELOADING
                                                        }
                                                        _ => false,
                                                    };
                                                    let result = if reloading {
                                                        reload_cancelled()
                                                    } else {
                                                        ToolResult::error(error.to_string())
                                                    };
                                                    (result, outcome)
                                                }
                                                Err(panic) => {
                                                    let message = panic_message(panic);
                                                    errors.push(AgentError::fault("ToolExecutor host", &message));
                                                    (ToolResult::error(message), "defect")
                                                }
                                            }
                                        };
                                        if emit_progress && let Err(error) = timeline.append(
                                            EventKind::ToolCompleted(ToolCompletedData {
                                                turn_id: context.turn_id,
                                                request_id: context.request_id,
                                                call_event_id: event_id,
                                                call_id: call.id.clone(),
                                                result: result.clone(),
                                            }).into(),
                                        ) {
                                            errors.push(error);
                                        }
                                        span.record("outcome", if errors.is_empty() { outcome } else { "defect" });
                                        (index, ToolCallResult { call_id: call.id, result }, errors)
                                    }
                                });
                                let results = futures_util::future::join_all(calls).await;
                                let mut ordered = vec![None; count];
                                let mut errors = Vec::new();
                                for (index, result, failures) in results {
                                    ordered[index] = Some(result);
                                    errors.extend(failures);
                                }
                                Ok::<_, AgentError>((
                                    ordered.into_iter()
                                        .map(|result| result.expect("all calls settled"))
                                        .collect::<Vec<_>>(),
                                    errors,
                                ))
                            }).catch_unwind().await
                                .map_err(|panic| AgentError::fault("ToolExecutor batch", panic_message(panic)))
                                .and_then(|result| result);
                            let unloading = {
                                let mut state = worker.state.lock().unwrap();
                                state.current = None;
                                state.current_context = None;
                                state.closed || generation.is_cancelled()
                            };
                            let (mut results, mut errors) = match outcome {
                                Ok(settled) => settled,
                                Err(error) => {
                                    let results = batch.calls.iter().map(|call| ToolCallResult {
                                        call_id: call.id.clone(),
                                        result: ToolResult::error(format!(
                                            "Tool was not executed: {error}"
                                        )),
                                    }).collect();
                                    (results, vec![error])
                                }
                            };
                            if unloading {
                                let through = timeline.committed();
                                let parents = results.iter()
                                    .filter(|item| item.result.effects.is_none())
                                    .map(|item| item.call_id.clone())
                                    .collect::<Vec<_>>();
                                let mut children = match child_effects(
                                    &timeline,
                                    &parents,
                                    &batch.request_id,
                                    through,
                                )
                                .await
                                {
                                    Ok(children) => children,
                                    Err(error) => {
                                        errors.push(error);
                                        BTreeMap::new()
                                    }
                                };
                                for item in &mut results {
                                    let mut effects = item.result.effects.take();
                                    if effects.is_none()
                                        && let Some((_, child)) = children.remove(&item.call_id)
                                        && !child.is_empty()
                                    {
                                        effects = Some(child);
                                    }
                                    item.result = reload_cancelled();
                                    item.result.effects = effects;
                                }
                            }
                            let published = std::panic::catch_unwind(AssertUnwindSafe(|| {
                                timeline.append(EventKind::ToolResult(ToolResultData {
                                    turn_id: batch.turn_id,
                                    request_id: batch.request_id,
                                    call_event_id: Some(call_event_id),
                                    results,
                                }).into())
                            }))
                            .map_err(|panic| AgentError::fault("ToolExecutor settlement", panic_message(panic)))
                            .and_then(|result| result);
                            if let Err(error) = published {
                                errors.push(error);
                            }
                            if let Err(error) = AgentError::aggregate(errors) {
                                worker.errors.lock().unwrap().push(error.clone());
                                faults.fail(error);
                            }
                            faults.notify();
                        });
                        Ok(())
                    })()
                    })
                }),
            ).await?;
            ctx.provide_at(self.executor.clone(), service)?;
            Ok(Effect::Done)
        })
    }
}

/// What admission produced for one call: the handler it will run and the call
/// Approval admitted.
struct Admitted {
    tool: Arc<ToolContribution>,
    claim: Option<Box<dyn ToolClaim>>,
    call: ToolCall,
}

/// Admits one call through Approval. It starts the moment the batch starts
/// and settles on its own, whatever step is running, so a batch's approval
/// questions appear together and one call never waits for a sibling's.
async fn admit(
    binding: &ToolBinding,
    approval: &Approval,
    call: ToolCall,
    context: &ToolExecutionContext,
    cancel: CancellationToken,
) -> Result<Admitted, HostError> {
    let tool = match binding.handler(&context.request_id, &call.name) {
        Ok(tool) => tool,
        // An unknown tool is denied here, without an approval question.
        Err(message) => return Err(HostError::failed(message)),
    };
    let claim = binding.take_claim(&context.request_id, &call.id);
    let risk = tool.handler.risk(&call).or(tool.definition.risk.as_deref());
    let call = if risk == Some("write_filesystem") {
        tool.handler.resolve_write(call, cancel.clone()).await?
    } else {
        call
    };
    approval
        .admit(
            AdmitCall {
                source: BranchSource::Tool {
                    turn_id: context.turn_id.clone(),
                    request_id: context.request_id.clone(),
                    call_id: call.id.clone(),
                },
                call: ApprovalCallData {
                    name: call.name.clone(),
                    input: call.input.clone(),
                },
            },
            risk,
            cancel.clone(),
        )
        .await?;
    Ok(Admitted { tool, claim, call })
}

/// Runs an admitted call through its handler.
async fn run(
    admitted: Admitted,
    context: ToolExecutionContext,
    cancel: CancellationToken,
) -> Result<ToolResult, HostError> {
    match admitted.claim {
        Some(claim) => claim.dispatch(admitted.call, context, cancel).await,
        None => {
            admitted
                .tool
                .handler
                .execute(admitted.call, context, cancel)
                .await
        }
    }
}

/// Admits one call through Approval, then runs its handler.
async fn execute(
    binding: &ToolBinding,
    approval: &Approval,
    call: ToolCall,
    context: ToolExecutionContext,
    cancel: CancellationToken,
) -> Result<ToolResult, HostError> {
    let admitted = admit(binding, approval, call, &context, cancel.clone()).await?;
    run(admitted, context, cancel).await
}

/// The steps of one batch and the barrier between them.
///
/// A step is a run of consecutive calls that may share one moment: a call
/// whose handler answers `Concurrent` joins the step before it, and an
/// `Exclusive` call starts a step of its own. `segment_of` holds the step of
/// each call, and `head` the number of leading steps that have settled
/// completely, so a step starts only after every earlier step finished.
struct Turn {
    /// Calls of each step that have not settled yet.
    remaining: Vec<AtomicUsize>,
    /// Leading steps that settled completely; starts at zero.
    head: watch::Sender<usize>,
}
impl Turn {
    /// Fixes the steps before any call is polled. It also returns one barrier
    /// receiver per call, taken here so a call subscribes before any of them
    /// can advance.
    fn new(
        classes: impl IntoIterator<Item = ToolConcurrency>,
        count: usize,
    ) -> (Arc<Self>, Vec<usize>, Vec<watch::Receiver<usize>>) {
        let mut segment_of = Vec::with_capacity(count);
        let mut remaining: Vec<AtomicUsize> = Vec::new();
        // The step before this call, and whether it is a step reads may share.
        let mut previous: Option<(usize, bool)> = None;
        for class in classes {
            let segment = match (class, previous) {
                // A read joins a read step before it, and an exclusive call
                // starts a step of its own; nothing joins an exclusive step.
                (ToolConcurrency::Concurrent, Some((segment, true))) => segment,
                _ => {
                    remaining.push(AtomicUsize::new(0));
                    remaining.len() - 1
                }
            };
            remaining[segment].fetch_add(1, Ordering::SeqCst);
            segment_of.push(segment);
            previous = Some((segment, class == ToolConcurrency::Concurrent));
        }
        debug_assert_eq!(segment_of.len(), count);
        let head = watch::Sender::new(0);
        let waiters = (0..count).map(|_| head.subscribe()).collect();
        (Arc::new(Self { remaining, head }), segment_of, waiters)
    }
}

/// Releases one call's place in its step, however the call ends: a settled
/// result, a refused admission, a panic or a dropped future.
struct Settled {
    turn: Arc<Turn>,
    segment: usize,
}
impl Drop for Settled {
    fn drop(&mut self) {
        let left = self.turn.remaining[self.segment].fetch_sub(1, Ordering::SeqCst) - 1;
        if left != 0 {
            return;
        }
        // Only a settled prefix advances the barrier. A later step can reach
        // zero first, when every call in it was refused during admission while
        // an earlier step still runs, and starting the steps after it then
        // would break the call order. `send_if_modified` holds the channel
        // lock, so concurrent drops advance the prefix one at a time.
        self.turn.head.send_if_modified(|head| {
            let start = *head;
            while *head < self.turn.remaining.len()
                && self.turn.remaining[*head].load(Ordering::SeqCst) == 0
            {
                *head += 1;
            }
            *head != start
        });
    }
}

/// The result and outcome label of one run, after its panic boundary. A panic
/// also records a fault, which fails the turn: the run never produced a result.
fn settled(
    outcome: Result<Result<ToolResult, HostError>, Box<dyn std::any::Any + Send>>,
    interrupted: bool,
    errors: &mut Vec<AgentError>,
) -> (ToolResult, &'static str) {
    match outcome {
        Ok(Ok(result)) => {
            let label = if interrupted {
                "interrupted"
            } else if result.is_error == Some(true) {
                "failure"
            } else {
                "returned"
            };
            (result, label)
        }
        Ok(Err(error)) => {
            let label = if matches!(error, HostError::Cancelled(_)) {
                "interrupted"
            } else {
                "failure"
            };
            let reloading =
                matches!(&error, HostError::Cancelled(reason) if reason == CAPABILITY_RELOADING);
            let result = if reloading {
                reload_cancelled()
            } else {
                ToolResult::error(error.to_string())
            };
            (result, label)
        }
        Err(panic) => {
            let message = panic_message(panic);
            errors.push(AgentError::fault("ToolExecutor host", &message));
            (ToolResult::error(message), "defect")
        }
    }
}

struct OpenBatch {
    parent_call_id: Option<String>,
    call_event_id: String,
    turn_id: String,
    request_id: String,
    calls: Vec<String>,
}

async fn child_effects(
    timeline: &Timeline,
    parent_call_ids: &[String],
    request_id: &str,
    through: u64,
) -> dim_agent_runtime::Result<BTreeMap<String, (usize, Vec<ToolEffect>)>> {
    if parent_call_ids.is_empty() {
        return Ok(BTreeMap::new());
    }
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
                            value: "step/tool-result".into(),
                        },
                    },
                    QueryFilter::Compare {
                        left: QueryOperand::Field {
                            path: "data.request_id".into(),
                        },
                        comparison: QueryComparison::Eq,
                        right: QueryOperand::Value {
                            value: request_id.into(),
                        },
                    },
                    QueryFilter::In {
                        value: QueryOperand::Field {
                            path: "parent_call_id".into(),
                        },
                        values: parent_call_ids.iter().cloned().map(Into::into).collect(),
                    },
                ],
            }),
            select: QuerySelect::Fields {
                fields: vec![
                    "parent_call_id".into(),
                    "data.results[].result.effects".into(),
                ],
            },
            group_by: Vec::new(),
            pick: None,
            order_by: Vec::new(),
        },
        page: Some(QueryPage {
            limit: NonZeroU64::new(256).expect("a page contains tool results"),
            after: None,
        }),
    };
    let mut children: BTreeMap<String, (usize, Vec<ToolEffect>)> = BTreeMap::new();
    loop {
        let reply = timeline.query(request.clone()).await?;
        let QueryOutput::Rows { rows } = reply.result else {
            unreachable!("tool effects selects result data")
        };
        for row in rows {
            let [
                QueryValue::Value {
                    value: serde_json::Value::String(parent),
                },
                QueryValue::Elements { values },
            ] = <[_; 2]>::try_from(row.values).unwrap()
            else {
                unreachable!("child effect projections")
            };
            let (seen, effects) = children.entry(parent).or_default();
            *seen += values.len();
            for value in values {
                if let QueryValue::Value { value } = value {
                    let child: Vec<ToolEffect> =
                        serde_json::from_value(value).map_err(|error| {
                            AgentError::fault(
                                "ToolExecutor",
                                format!("invalid child effects: {error}"),
                            )
                        })?;
                    effects.extend(child);
                }
            }
        }
        let Some(next) = reply.next else {
            break;
        };
        request
            .page
            .as_mut()
            .expect("tool result queries are paged")
            .after = Some(next);
    }
    Ok(children)
}

/// A committed batch without results was left by a reload and settles as
/// not executed.
async fn rebuild(owner: &ToolExecutor, timeline: &Timeline) -> dim_agent_runtime::Result<u64> {
    let through = timeline.committed();
    let mut request = TimelineQuery {
        scope: TimelineQueryScope::Branch,
        from: None,
        through: Some(through),
        include_publishing: true,
        query: QuerySpec {
            filter: Some(QueryFilter::In {
                value: QueryOperand::Field {
                    path: "kind".into(),
                },
                values: vec![
                    "stop/interrupt".into(),
                    "turn/start".into(),
                    "turn/end".into(),
                ],
            }),
            select: QuerySelect::Fields {
                fields: vec!["kind".into(), "data.reason".into()],
            },
            group_by: Vec::new(),
            pick: Some(QueryPick::Last),
            order_by: Vec::new(),
        },
        page: Some(QueryPage {
            limit: NonZeroU64::new(256).expect("a page contains operations"),
            after: None,
        }),
    };
    let control = timeline.query(request.clone()).await?;
    let QueryOutput::Rows { rows } = control.result else {
        unreachable!("tool recovery selects control fields")
    };
    let interrupted = if let Some(row) = rows.into_iter().next() {
        let mut values = row.values.into_iter();
        match (values.next(), values.next()) {
            (
                Some(QueryValue::Value {
                    value: serde_json::Value::String(kind),
                }),
                Some(reason),
            ) => {
                if kind == "stop/interrupt" {
                    let QueryValue::Value {
                        value: serde_json::Value::String(reason),
                    } = reason
                    else {
                        return Err(AgentError::fault(
                            "ToolExecutor",
                            "an interrupt has no reason",
                        ));
                    };
                    Some(reason)
                } else {
                    None
                }
            }
            _ => {
                return Err(AgentError::fault(
                    "ToolExecutor",
                    "a control event has no kind",
                ));
            }
        }
    } else {
        None
    };
    owner.state.lock().unwrap().interrupted = interrupted;
    request.query.pick = None;
    request.query.select = QuerySelect::Fields {
        fields: vec![
            "id".into(),
            "data.turn_id".into(),
            "data.request_id".into(),
            "data.calls[].id".into(),
            "parent_call_id".into(),
        ],
    };
    request.query.filter = Some(QueryFilter::And {
        conditions: vec![
            QueryFilter::Compare {
                left: QueryOperand::Field {
                    path: "kind".into(),
                },
                comparison: QueryComparison::Eq,
                right: QueryOperand::Value {
                    value: "step/tool-call".into(),
                },
            },
            QueryFilter::Not {
                condition: Box::new(QueryFilter::Exists {
                    condition: Box::new(QueryFilter::And {
                        conditions: vec![
                            QueryFilter::Compare {
                                left: QueryOperand::Field {
                                    path: "kind".into(),
                                },
                                comparison: QueryComparison::Eq,
                                right: QueryOperand::Value {
                                    value: "step/tool-result".into(),
                                },
                            },
                            QueryFilter::Compare {
                                left: QueryOperand::Field {
                                    path: "data.call_event_id".into(),
                                },
                                comparison: QueryComparison::Eq,
                                right: QueryOperand::Outer { path: "id".into() },
                            },
                        ],
                    }),
                }),
            },
        ],
    });
    let mut batches = Vec::new();
    loop {
        let reply = timeline.query(request.clone()).await?;
        let QueryOutput::Rows { rows } = reply.result else {
            unreachable!("tool recovery selects operation fields")
        };
        for row in rows {
            let Ok(
                [
                    QueryValue::Value {
                        value: serde_json::Value::String(call_event_id),
                    },
                    QueryValue::Value {
                        value: serde_json::Value::String(turn_id),
                    },
                    QueryValue::Value {
                        value: serde_json::Value::String(request_id),
                    },
                    QueryValue::Elements { values: calls },
                    parent,
                ],
            ) = <[_; 5]>::try_from(row.values)
            else {
                return Err(AgentError::fault(
                    "ToolExecutor",
                    "a tool batch has invalid operation identities",
                ));
            };
            let parent_call_id = match parent {
                QueryValue::Missing
                | QueryValue::Value {
                    value: serde_json::Value::Null,
                } => None,
                QueryValue::Value {
                    value: serde_json::Value::String(value),
                } => Some(value),
                _ => {
                    return Err(AgentError::fault(
                        "ToolExecutor",
                        "a tool batch has an invalid parent",
                    ));
                }
            };
            let calls = calls
                .into_iter()
                .map(|call| {
                    let QueryValue::Value {
                        value: serde_json::Value::String(call_id),
                    } = call
                    else {
                        return Err(AgentError::fault(
                            "ToolExecutor",
                            "a tool call has no identity",
                        ));
                    };
                    Ok(call_id)
                })
                .collect::<dim_agent_runtime::Result<Vec<_>>>()?;
            batches.push(OpenBatch {
                parent_call_id,
                call_event_id,
                turn_id,
                request_id,
                calls,
            });
        }
        let Some(next) = reply.next else {
            break;
        };
        request
            .page
            .as_mut()
            .expect("operation queries are paged")
            .after = Some(next);
    }
    batches.sort_by_key(|batch| batch.parent_call_id.is_none());
    for batch in batches {
        let mut children = if batch.parent_call_id.is_none() {
            child_effects(
                timeline,
                &batch.calls,
                &batch.request_id,
                timeline.committed(),
            )
            .await?
        } else {
            BTreeMap::new()
        };
        let results = batch
            .calls
            .into_iter()
            .map(|call_id| {
                let (seen, effects) = children.remove(&call_id).unwrap_or_default();
                let mut result = if seen > 0 {
                    reload_settled(ToolResult::error(
                        "Tool did not finish before Capability reload; an effect may have happened",
                    ))
                } else {
                    reload_settled(ToolResult::error(format!(
                        "Tool was not executed: {CAPABILITY_RELOADING}"
                    )))
                };
                if !effects.is_empty() {
                    result.effects = Some(effects);
                }
                ToolCallResult { call_id, result }
            })
            .collect();
        let event = EventDraft::from(EventKind::ToolResult(ToolResultData {
            turn_id: batch.turn_id,
            request_id: batch.request_id,
            call_event_id: Some(batch.call_event_id),
            results,
        }));
        let event = match batch.parent_call_id {
            Some(parent_call_id) => event.with_parent_call_id(parent_call_id),
            None => event,
        };
        timeline.append(event)?;
    }
    Ok(through)
}

/// The result of a call cancelled by a capability reload.
fn reload_cancelled() -> ToolResult {
    reload_settled(ToolResult::error(format!(
        "Tool execution cancelled: {CAPABILITY_RELOADING}"
    )))
}
/// Marks a result settled by a capability reload. The mark is internal state
/// (`structuredContent`); the turn that receives it ends Failed.
fn reload_settled(mut result: ToolResult) -> ToolResult {
    result
        .structured_content
        .get_or_insert_with(Default::default)
        .insert("settlement".into(), CAPABILITY_RELOADING.into());
    result
}
pub(crate) fn is_reload_settled(result: &ToolResult) -> bool {
    result
        .structured_content
        .as_ref()
        .and_then(|content| content.get("settlement"))
        .is_some_and(|value| value == CAPABILITY_RELOADING)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn waiters(
        classes: &[ToolConcurrency],
    ) -> (Arc<Turn>, Vec<usize>, Vec<watch::Receiver<usize>>) {
        Turn::new(classes.to_vec(), classes.len())
    }

    fn settle(turn: &Arc<Turn>, segment: usize) {
        drop(Settled {
            turn: turn.clone(),
            segment,
        });
    }

    #[test]
    fn the_barrier_advances_over_the_settled_prefix_only() {
        // Three exclusive calls: one step each.
        let (turn, _, mut waiters) = waiters(&[
            ToolConcurrency::Exclusive,
            ToolConcurrency::Exclusive,
            ToolConcurrency::Exclusive,
        ]);
        assert_eq!(turn.head.borrow().clone(), 0);
        // Step 1 settles first because its call was refused during admission
        // while step 0 still runs. The prefix does not move.
        settle(&turn, 1);
        assert_eq!(
            *turn.head.borrow(),
            0,
            "a later step settling first leaves the prefix alone"
        );
        assert!(
            waiters[1]
                .wait_for(|head| *head > 0)
                .now_or_never()
                .is_none(),
            "step 1 still waits for step 0"
        );
        // Step 0 settles: the prefix runs over steps 0 and 1.
        settle(&turn, 0);
        assert_eq!(*turn.head.borrow(), 2);
        assert!(
            waiters[1]
                .wait_for(|head| *head > 0)
                .now_or_never()
                .is_some(),
            "step 1 starts once step 0 settled"
        );
        // Step 2 settles last.
        settle(&turn, 2);
        assert_eq!(*turn.head.borrow(), 3);
    }

    #[test]
    fn a_step_advances_the_prefix_only_when_all_of_its_calls_settled() {
        use ToolConcurrency::{Concurrent as C, Exclusive as X};
        // Steps: call 0 alone, calls 1 and 2 together.
        let (turn, segments, mut waiters) = waiters(&[X, C, C]);
        assert_eq!(segments, [0, 1, 1]);
        settle(&turn, segments[1]);
        assert_eq!(*turn.head.borrow(), 0);
        settle(&turn, segments[0]);
        assert_eq!(
            *turn.head.borrow(),
            1,
            "step 1 still holds one call, so the prefix stops before it"
        );
        assert!(
            waiters[2]
                .wait_for(|head| *head > 1)
                .now_or_never()
                .is_none(),
            "step 2 waits for its own step"
        );
        settle(&turn, segments[2]);
        assert_eq!(*turn.head.borrow(), 2);
    }
}

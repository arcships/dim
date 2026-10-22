use crate::{
    approval::{AdmitCall, Approval},
    host::*,
    tool_binding::ToolBinding,
};
use dim_agent_runtime::{
    AgentError, BranchScope, CAPABILITY_RELOADING, Capability, Dispatcher, InBranch, KeysOf,
    Timeline, TimelineIdentity, scoped,
};
use dim_agent_types::*;
use futures_util::FutureExt;
use rutis::{BoxFuture, CordisError, Ctx, Effect, TypedPlugin};
use std::{
    panic::AssertUnwindSafe,
    sync::{Arc, Mutex},
};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

#[derive(Default)]
struct ToolState {
    current: Option<CancellationToken>,
    /// Set by `stop/interrupt` until the interrupted turn ends or the next
    /// one starts: requests that still arrive belong to the interrupted turn
    /// and are cancelled. Folded from consumed events, so rebuilt on load.
    interrupted: Option<String>,
    closed: bool,
    /// Events up to this sequence were applied by the rebuild at load.
    rebuilt: u64,
}
pub struct ToolExecutor {
    state: Mutex<ToolState>,
    tasks: TaskTracker,
    errors: Mutex<Vec<AgentError>>,
}
pub struct ToolExecutorPlugin {
    pub emit_progress: bool,
    scope: BranchScope,
}
impl ToolExecutorPlugin {
    pub fn new(scope: &BranchScope) -> Self {
        Self {
            emit_progress: false,
            scope: *scope,
        }
    }
}
impl TypedPlugin for ToolExecutorPlugin {
    type Deps = (
        InBranch<Timeline>,
        InBranch<Dispatcher>,
        InBranch<ToolBinding>,
        InBranch<Approval>,
        InBranch<TimelineIdentity>,
    );
    fn name(&self) -> &str {
        "ToolExecutor"
    }
    fn apply<'a>(
        &'a self,
        ctx: &'a Ctx,
        (
            InBranch(timeline),
            InBranch(dispatcher),
            InBranch(binding),
            InBranch(approval),
            InBranch(identity),
        ): Self::Deps,
    ) -> BoxFuture<'a, Result<Effect, CordisError>> {
        Box::pin(async move {
            let emit_progress = self.emit_progress;
            let service = Arc::new(ToolExecutor {
                state: Mutex::new(ToolState::default()),
                tasks: TaskTracker::new(),
                errors: Mutex::new(Vec::new()),
            });
            let closing = service.clone();
            ctx.effect(move || {
                Effect::AsyncDisposer(Box::new(move || {
                    Box::pin(async move {
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
                        AgentError::aggregate(closing.errors.lock().unwrap().clone())
                            .map_err(Into::into)
                    })
                }))
            })?;
            let owner = service.clone();
            let faults = dispatcher.clone();
            let lifecycle = ctx.cancellation_token();
            // Subscribe, then rebuild under the state lock; see the reload protocol.
            let rebuild_timeline = timeline.clone();
            let gate = Arc::new(tokio::sync::Mutex::new(()));
            let rebuilding = gate.lock().await;
            let handler_gate = gate.clone();
            dispatcher.subscribe(
                ctx,
                "ToolExecutor",
                &["step/tool-call", "stop/interrupt", "turn/start", "turn/end"],
                move |event| {
                    let gate = handler_gate.clone();
                    let owner = owner.clone();
                    let timeline = timeline.clone();
                    let faults = faults.clone();
                    let lifecycle = lifecycle.clone();
                    let binding = binding.clone();
                    let approval = approval.clone();
                    let identity = identity.clone();
                    Box::pin(async move {
                    let _ready = gate.lock().await;
                    (|| {
                        let mut state = owner.state.lock().unwrap();
                        if event.sequence <= state.rebuilt {
                            return Ok(());
                        }
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
                            // A late request owes results, but no host work or permit.
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
                        // Cancelled only when this generation unloads, before its cleanup runs.
                        let generation = lifecycle.clone();
                        state.current = Some(cancel.clone());
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
                                let calls = batch.calls.iter().cloned().enumerate().map(|(index, call)| {
                                    let binding = binding.clone();
                                    let approval = approval.clone();
                                    let timeline = timeline.clone();
                                    let cancel = cancel.clone();
                                    let event_id = call_event_id.clone();
                                    let session_id = identity.session_id.clone();
                                    let context = ToolExecutionContext {
                                        turn_id: batch.turn_id.clone(),
                                        request_id: batch.request_id.clone(),
                                        branch_id: identity.branch_id.clone(),
                                    };
                                    async move {
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
                                        let (result, outcome) = if cancel.is_cancelled() {
                                            (ToolResult::error("Tool cancelled before execution"), "interrupted")
                                        } else {
                                            match AssertUnwindSafe(async {
                                                execute(&binding, &approval, call.clone(), context.clone(), cancel).await
                                            }).catch_unwind().await {
                                                Ok(Ok(result)) => {
                                                    let outcome = if result.is_error == Some(true) { "failure" } else { "returned" };
                                                    (result, outcome)
                                                },
                                                Ok(Err(error)) => {
                                                    let outcome = if matches!(error, HostError::Cancelled(_)) { "interrupted" } else { "failure" };
                                                    let reloading = matches!(&error, HostError::Cancelled(reason) if reason == CAPABILITY_RELOADING);
                                                    let result = if reloading { reload_cancelled() } else { ToolResult::error(error.to_string()) };
                                                    (result, outcome)
                                                },
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
                                for item in &mut results {
                                    item.result = reload_cancelled();
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
                },
            )?;
            let through = rebuild_timeline.committed();
            let events = rebuild_timeline.local(..=through).await?;
            rebuild(
                &mut service.state.lock().unwrap(),
                &rebuild_timeline,
                &events,
            )?;
            drop(rebuilding);
            ctx.provide_as(self.scope.key::<ToolExecutor>(), service)?;
            Ok(Effect::Done)
        })
    }
}
impl Capability for ToolExecutorPlugin {
    fn keys(&self) -> KeysOf<Self> {
        scoped::<Self::Deps, _>(&self.scope)
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
    // The request's snapshot, never the registry's current table.
    let tool = match binding.handler(&context.request_id, &call.name) {
        Ok(tool) => tool,
        Err(message) => return Ok(ToolResult::error(message)),
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
            tool.definition.risk.as_deref(),
            cancel.clone(),
        )
        .await?;
    tool.handler.execute(call, context, cancel).await
}

/// A committed batch without results was left by a reload and settles as
/// not executed.
fn rebuild(
    state: &mut ToolState,
    timeline: &Timeline,
    events: &[Arc<AgentEvent>],
) -> dim_agent_runtime::Result<()> {
    state.rebuilt = events.last().map_or(0, |event| event.sequence);
    let mut open = Vec::new();
    for event in events {
        match &event.kind {
            EventKind::StopInterrupt(data) => state.interrupted = Some(data.reason.clone()),
            EventKind::TurnStart(_) | EventKind::TurnEnd(_) => state.interrupted = None,
            EventKind::ToolCall(batch) => open.push((event.id.clone(), batch.clone())),
            EventKind::ToolResult(data) => {
                open.retain(|(id, _)| data.call_event_id.as_ref() != Some(id))
            }
            _ => {}
        }
    }
    for (call_event_id, batch) in open {
        timeline.append(
            EventKind::ToolResult(ToolResultData {
                turn_id: batch.turn_id,
                request_id: batch.request_id,
                call_event_id: Some(call_event_id),
                results: batch
                    .calls
                    .iter()
                    .map(|call| ToolCallResult {
                        call_id: call.id.clone(),
                        result: reload_settled(ToolResult::error(format!(
                            "Tool was not executed: {CAPABILITY_RELOADING}"
                        ))),
                    })
                    .collect(),
            })
            .into(),
        )?;
    }
    Ok(())
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

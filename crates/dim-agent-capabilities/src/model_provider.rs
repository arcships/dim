mod images;
mod recovery;
pub use recovery::{Consume, invoke as invoke_with_recovery};

use crate::{
    context_preparation::ContextPreparation,
    host::*,
    output::{Output, OutputGate, StartedCall, closure, consume_output},
    steer::{REPLACED_BY_STEER, SteerMark},
};
use dim_agent_runtime::{
    AgentError, BranchScope, CAPABILITY_RELOADING, Capability, Dispatcher, InBranch, InSession,
    KeysOf, ModelProgress, StateCell, Timeline, TimelineIdentity, When,
};
use dim_agent_types::*;
use futures_util::FutureExt;
use rutis::{BoxFuture, CordisError, Ctx, Effect, TypedPlugin};
use std::{
    panic::AssertUnwindSafe,
    sync::{Arc, Mutex},
};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

/// The running request.
struct Current {
    request_id: String,
    cancel: CancellationToken,
    /// Decides a steer consumed while it runs against its received output.
    gate: Arc<OutputGate>,
}

#[derive(Default)]
struct ModelState {
    current: Option<Current>,
    /// A call handed over after a steer of its request was applied is
    /// cancelled without running (ADR agent-029 decision 5).
    steer: SteerMark,
    /// Set by `stop/interrupt` until the interrupted turn ends or the next
    /// one starts: requests that still arrive belong to the interrupted turn
    /// and are cancelled. Folded from consumed events, so rebuilt on load.
    interrupted: Option<String>,
    /// Why the current operation was cancelled by an interrupt.
    stop_reason: Option<String>,
    closed: bool,
    /// Events up to this sequence were applied by the rebuild at load.
    rebuilt: u64,
}

pub struct ModelProvider {
    state: Mutex<ModelState>,
    tasks: TaskTracker,
    errors: Mutex<Vec<AgentError>>,
    context: Arc<ContextPreparation>,
    host: Arc<ModelHost>,
    resolver: Option<Arc<ProviderConnectionResolver>>,
    timeline: Arc<Timeline>,
    identity: Arc<TimelineIdentity>,
    progress: Arc<StateCell<ModelProgress>>,
}
impl ModelProvider {
    /// Writes the request's terminal; an output that did not complete closes
    /// its started calls in the same batch, before the terminal.
    fn complete(
        &self,
        call: ModelCallData,
        result: std::result::Result<FinishReason, HostError>,
        started: &[StartedCall],
    ) -> dim_agent_runtime::Result<()> {
        let outcome = match result {
            Ok(finish_reason) => ModelOutputOutcome::Completed { finish_reason },
            Err(HostError::Cancelled(reason)) => ModelOutputOutcome::Cancelled { reason },
            Err(HostError::Failed {
                message,
                budget,
                presentation,
                ..
            }) => ModelOutputOutcome::Failed {
                error: message,
                budget,
                presentation: presentation.map(|value| *value),
            },
        };
        let mut events = Vec::new();
        if !matches!(outcome, ModelOutputOutcome::Completed { .. })
            && let Some(results) = closure(&call.turn_id, &call.request_id, started)
        {
            events.push(EventKind::ToolResult(results).into());
        }
        events.push(
            EventKind::ModelOutputEnd(ModelOutputEndData {
                turn_id: call.turn_id,
                request_id: call.request_id.clone(),
                outcome,
            })
            .into(),
        );
        self.timeline.append_batch_registered(events, |_| Ok(()))?;
        Ok(())
    }
    /// A committed call without an output end
    /// was left by a reload and fails without running.
    fn rebuild(
        &self,
        state: &mut ModelState,
        events: &[Arc<AgentEvent>],
    ) -> dim_agent_runtime::Result<()> {
        state.rebuilt = events.last().map_or(0, |event| event.sequence);
        // Open calls with the calls their output started.
        let mut open: Vec<(ModelCallData, Vec<StartedCall>)> = Vec::new();
        for event in events {
            state.steer.apply(event);
            match &event.kind {
                EventKind::StopInterrupt(data) => state.interrupted = Some(data.reason.clone()),
                EventKind::TurnStart(_) | EventKind::TurnEnd(_) => state.interrupted = None,
                EventKind::ModelCall(call) => open.push((call.clone(), Vec::new())),
                EventKind::ToolUseStart(data) => {
                    if let Some((_, started)) = open
                        .iter_mut()
                        .find(|(call, _)| call.request_id == data.request_id)
                    {
                        started.push(StartedCall {
                            id: data.call_id.clone(),
                            complete: false,
                        });
                    }
                }
                EventKind::ToolUse(data) => {
                    if let Some(started) = open
                        .iter_mut()
                        .filter(|(call, _)| call.request_id == data.request_id)
                        .flat_map(|(_, started)| started.iter_mut())
                        .find(|started| started.id == data.call.id)
                    {
                        started.complete = true;
                    }
                }
                EventKind::ModelOutputEnd(data) => open.retain(|(call, _)| {
                    call.turn_id != data.turn_id || call.request_id != data.request_id
                }),
                _ => {}
            }
        }
        for (call, started) in open {
            self.complete(call, Err(HostError::failed(CAPABILITY_RELOADING)), &started)?;
        }
        Ok(())
    }
    async fn run(
        &self,
        call: ModelCallData,
        cancel: CancellationToken,
        gate: Arc<OutputGate>,
        generation: CancellationToken,
    ) -> dim_agent_runtime::Result<()> {
        let span = tracing::info_span!(
            "model.invoke",
            session_id = %self.identity.session_id,
            branch_id = %self.identity.branch_id,
            turn_id = %call.turn_id,
            request_id = %call.request_id,
            outcome = tracing::field::Empty,
        );
        let mut output = Output::new(
            self.timeline.clone(),
            gate.clone(),
            self.progress.clone(),
            call.clone(),
        );
        let (result, fault) = if cancel.is_cancelled() {
            let reason = self
                .state
                .lock()
                .unwrap()
                .stop_reason
                .clone()
                .unwrap_or_else(|| CAPABILITY_RELOADING.into());
            (Err(HostError::Cancelled(reason)), None)
        } else {
            let result = AssertUnwindSafe(async {
                self.context
                    .validate(call.request.clone(), cancel.clone())
                    .await?;
                crate::context_policy::check_cancel(&cancel)?;
                // The committed request keeps its images; only what is sent changes.
                let request = images::adapt(&call.request);
                let resolved = match &self.resolver {
                    Some(resolver) => resolver.0.resolve(request.clone(), cancel.clone()).await?,
                    None => ResolvedModelConnection {
                        connection: ModelConnection::Local,
                        failure_policy: None,
                        recovery: None,
                    },
                };
                recovery::invoke(
                    &self.host,
                    &request,
                    resolved,
                    &cancel,
                    &mut output,
                    |stream, output, attempt, replay_safe| {
                        Box::pin(consume_output(stream, output, attempt, replay_safe))
                    },
                )
                .await
            })
            .catch_unwind()
            .await;
            match result {
                Ok(result) => (result, None),
                Err(panic) => {
                    let message = panic_message(panic);
                    (
                        Err(HostError::failed(&message)),
                        Some(AgentError::fault("ModelProvider", message)),
                    )
                }
            }
        };
        // The final recheck, the terminal and releasing the request are one
        // step under the state lock the steer handler takes: a steer consumed
        // before it replaces a request without output, one consumed after it
        // finds no running request (time-bus §4).
        let (outcome, submitted) = {
            let mut state = self.state.lock().unwrap();
            let result = match result {
                // This generation's token is cancelled only when it unloads.
                Err(HostError::Cancelled(_)) if generation.is_cancelled() => {
                    Err(HostError::Cancelled(CAPABILITY_RELOADING.into()))
                }
                // Replaced before any output: whatever the host returned
                // after that, the request ends as replaced.
                _ if gate.replaced() => Err(HostError::Cancelled(REPLACED_BY_STEER.into())),
                // A cancelled task reports the interrupt this owner consumed,
                // not the host's or backoff wait's generic cancellation text.
                Err(HostError::Cancelled(reason)) => Err(HostError::Cancelled(
                    state.stop_reason.clone().unwrap_or(reason),
                )),
                result => result,
            };
            let outcome = match (&result, &fault) {
                (_, Some(_)) => "defect",
                (Ok(_), _) => "returned",
                (Err(HostError::Cancelled(_)), _) => "interrupted",
                (Err(_), _) => "failure",
            };
            let submitted = self.complete(call, result, &output.started);
            state.current = None;
            (outcome, submitted)
        };
        match submitted {
            Ok(()) => output.release(&generation).await,
            Err(_) => output.clear_progress(),
        }
        span.record(
            "outcome",
            if submitted.is_err() {
                "defect"
            } else {
                outcome
            },
        );
        AgentError::aggregate(submitted.err().into_iter().chain(fault).collect())
    }
}

pub struct ModelProviderPlugin {
    scope: BranchScope,
    process_model: bool,
}
impl ModelProviderPlugin {
    pub fn new(scope: &BranchScope) -> Self {
        Self {
            scope: *scope,
            process_model: false,
        }
    }
    /// The network profile reads the process request service and resolver.
    pub fn process(scope: &BranchScope) -> Self {
        Self {
            scope: *scope,
            process_model: true,
        }
    }
}
impl TypedPlugin for ModelProviderPlugin {
    type Deps = (
        InBranch<Timeline>,
        InBranch<Dispatcher>,
        InBranch<ContextPreparation>,
        When<InSession<ModelHost>>,
        When<(Arc<ModelHost>, Arc<ProviderConnectionResolver>)>,
        InBranch<TimelineIdentity>,
    );
    fn name(&self) -> &str {
        "ModelProvider"
    }
    fn apply<'a>(
        &'a self,
        ctx: &'a Ctx,
        (
            InBranch(timeline),
            InBranch(dispatcher),
            InBranch(context),
            When(session_model),
            When(process_model),
            InBranch(identity),
        ): Self::Deps,
    ) -> BoxFuture<'a, Result<Effect, CordisError>> {
        Box::pin(async move {
            let (host, resolver) = match (session_model, process_model) {
                (_, Some((host, resolver))) => (host, Some(resolver)),
                (Some(InSession(host)), None) => (host, None),
                (None, None) => unreachable!("the keys enable exactly one model host"),
            };
            let service = Arc::new(ModelProvider {
                state: Mutex::new(ModelState::default()),
                tasks: TaskTracker::new(),
                errors: Mutex::new(Vec::new()),
                context,
                host,
                resolver,
                timeline,
                identity,
                progress: Arc::new(StateCell::new(ModelProgress::default())),
            });
            let owner = service.clone();
            ctx.effect(move || {
                Effect::AsyncDisposer(Box::new(move || {
                    Box::pin(async move {
                        {
                            let mut state = owner.state.lock().unwrap();
                            state.closed = true;
                            if let Some(current) = &state.current {
                                current.cancel.cancel();
                            }
                        }
                        owner.tasks.close();
                        owner.tasks.wait().await;
                        AgentError::aggregate(owner.errors.lock().unwrap().clone())
                            .map_err(Into::into)
                    })
                }))
            })?;
            let owner = service.clone();
            let failures = dispatcher.clone();
            let lifecycle = ctx.cancellation_token();
            // Subscribe, then rebuild under the state lock; see the reload protocol.
            let gate = Arc::new(tokio::sync::Mutex::new(()));
            let rebuilding = gate.lock().await;
            let handler_gate = gate.clone();
            dispatcher.subscribe(
                ctx,
                "ModelProvider",
                &[
                    "model/call",
                    "stop/interrupt",
                    "step/model-start",
                    "input/applied",
                    "turn/start",
                    "turn/end",
                ],
                move |event| {
                    let owner = owner.clone();
                    let gate = handler_gate.clone();
                    let failures = failures.clone();
                    let lifecycle = lifecycle.clone();
                    Box::pin(async move {
                        let _ready = gate.lock().await;
                        if event.sequence <= owner.state.lock().unwrap().rebuilt {
                            return Ok(());
                        }
                        if let EventKind::StopInterrupt(data) = &event.kind {
                            let mut state = owner.state.lock().unwrap();
                            state.interrupted = Some(data.reason.clone());
                            if let Some(cancel) =
                                state.current.as_ref().map(|current| current.cancel.clone())
                            {
                                state.stop_reason = Some(data.reason.clone());
                                cancel.cancel();
                            }
                            return Ok(());
                        }
                        if matches!(
                            event.kind,
                            EventKind::ModelStart(_) | EventKind::InputApplied(_)
                        ) {
                            let mut state = owner.state.lock().unwrap();
                            state.steer.apply(&event);
                            // A steer of the running request replaces it unless
                            // it already received output; an interrupt
                            // already ended it.
                            if let Some(current) = state.current.as_ref().filter(|current| {
                                state.interrupted.is_none()
                                    && state.steer.steered(&current.request_id)
                            }) && current.gate.replace()
                            {
                                current.cancel.cancel();
                            }
                            return Ok(());
                        }
                        if matches!(event.kind, EventKind::TurnStart(_) | EventKind::TurnEnd(_)) {
                            owner.state.lock().unwrap().interrupted = None;
                            return Ok(());
                        }
                        let EventKind::ModelCall(call) = &event.kind else {
                            unreachable!()
                        };
                        let mut state = owner.state.lock().unwrap();
                        if state.closed {
                            return Err(AgentError::Closed("ModelProvider".into()));
                        }
                        if state.current.is_some() {
                            return Err(AgentError::fault(
                                "ModelProvider",
                                "concurrent model calls",
                            ));
                        }
                        let reason = state.interrupted.clone().or_else(|| {
                            state
                                .steer
                                .steered(&call.request_id)
                                .then(|| REPLACED_BY_STEER.to_owned())
                        });
                        if let Some(reason) = reason {
                            drop(state);
                            return owner.complete(
                                call.clone(),
                                Err(HostError::Cancelled(reason)),
                                &[],
                            );
                        }
                        state.stop_reason = None;
                        let cancel = lifecycle.child_token();
                        let generation = lifecycle.clone();
                        let gate = Arc::new(OutputGate::default());
                        state.current = Some(Current {
                            request_id: call.request_id.clone(),
                            cancel: cancel.clone(),
                            gate: gate.clone(),
                        });
                        let call = call.clone();
                        let worker = owner.clone();
                        owner.tasks.spawn(async move {
                            let result =
                                AssertUnwindSafe(worker.run(call, cancel, gate, generation))
                                    .catch_unwind()
                                    .await
                                    .map_err(|panic| {
                                        AgentError::fault("ModelProvider", panic_message(panic))
                                    })
                                    .and_then(|result| result);
                            if let Err(error) = result {
                                worker.errors.lock().unwrap().push(error.clone());
                                failures.fail(error);
                            }
                        });
                        Ok(())
                    })
                },
            )?;
            let through = service.timeline.committed();
            let events = service.timeline.local(..=through).await?;
            service.rebuild(&mut service.state.lock().unwrap(), &events)?;
            drop(rebuilding);
            ctx.provide_as(
                self.scope.key::<StateCell<ModelProgress>>(),
                service.progress.clone(),
            )?;
            ctx.provide_as(self.scope.key::<ModelProvider>(), service)?;
            Ok(Effect::Done)
        })
    }
}
impl Capability for ModelProviderPlugin {
    fn keys(&self) -> KeysOf<Self> {
        let scope = self.scope;
        (
            scope,
            scope,
            scope,
            (!self.process_model).then_some(scope.session),
            self.process_model.then_some(((), ())),
            scope,
        )
    }
}

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
    AgentError, CAPABILITY_RELOADING, Delivery, Dispatcher, ModelProgress, Names, StateCell,
    Timeline, TimelineIdentity,
};
use dim_agent_types::*;
use futures_util::FutureExt;
use rutis::Ctx;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, Key, TypeKey};
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
            }) => {
                tracing::error!(
                    session_id = %self.identity.session_id,
                    branch_id = %self.identity.branch_id,
                    turn_id = %call.turn_id,
                    request_id = %call.request_id,
                    provider_id = %call.request.provider_id,
                    model_id = %call.request.model_id,
                    error = %message,
                    "Model request failed"
                );
                ModelOutputOutcome::Failed {
                    error: message,
                    budget,
                    presentation: presentation.map(|value| *value),
                }
            }
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
                Err(HostError::Cancelled(_)) if generation.is_cancelled() => {
                    Err(HostError::Cancelled(CAPABILITY_RELOADING.into()))
                }
                _ if gate.replaced() => Err(HostError::Cancelled(REPLACED_BY_STEER.into())),
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
    provider: Key<ModelProvider>,
    progress: Key<StateCell<ModelProgress>>,
    timeline: Key<Timeline>,
    dispatcher: Key<Dispatcher>,
    context: Key<ContextPreparation>,
    host: Key<ModelHost>,
    /// Each request's connection comes from the process
    /// `ProviderConnectionResolver`; otherwise it is local.
    resolver: Option<Key<ProviderConnectionResolver>>,
    identity: Key<TimelineIdentity>,
}
impl ModelProviderPlugin {
    /// Requests run on the local connection.
    pub fn new(names: &Names<'_>) -> rutis::Result<Self> {
        Self::with(names, false)
    }
    /// The network profile resolves each request's connection through the
    /// process `ProviderConnectionResolver`.
    pub fn process(names: &Names<'_>) -> rutis::Result<Self> {
        Self::with(names, true)
    }
    fn with(names: &Names<'_>, resolves: bool) -> rutis::Result<Self> {
        Ok(Self {
            provider: names.key()?,
            progress: names.state_cell()?,
            timeline: names.key()?,
            dispatcher: names.key()?,
            context: names.key()?,
            host: names.key()?,
            resolver: resolves.then(|| names.key()).transpose()?,
            identity: names.key()?,
        })
    }
}
impl DynPlugin for ModelProviderPlugin {
    fn name(&self) -> &str {
        "ModelProvider"
    }
    fn injects(&self) -> Vec<TypeKey> {
        let mut keys = vec![
            self.timeline.erase(),
            self.dispatcher.erase(),
            self.context.erase(),
            self.host.erase(),
        ];
        keys.extend(self.resolver.as_ref().map(Key::erase));
        keys.push(self.identity.erase());
        keys
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let timeline = ctx.require_at(&self.timeline)?;
            let dispatcher = ctx.require_at(&self.dispatcher)?;
            let context = ctx.require_at(&self.context)?;
            let host = ctx.require_at(&self.host)?;
            let resolver = match &self.resolver {
                Some(key) => Some(ctx.require_at(key)?),
                None => None,
            };
            let identity = ctx.require_at(&self.identity)?;
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
            ctx.effect(move || async move {
                {
                    let mut state = owner.state.lock().unwrap();
                    state.closed = true;
                    if let Some(current) = &state.current {
                        current.cancel.cancel();
                    }
                }
                owner.tasks.close();
                owner.tasks.wait().await;
                AgentError::aggregate(owner.errors.lock().unwrap().clone()).map_err(Into::into)
            })?;
            let owner = service.clone();
            let failures = dispatcher.clone();
            let lifecycle = ctx.cancellation_token();
            dispatcher
                .follow(
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
                    move |delivery| {
                        let owner = owner.clone();
                        let failures = failures.clone();
                        let lifecycle = lifecycle.clone();
                        Box::pin(async move {
                            let event = match delivery {
                                Delivery::Replay(events) => {
                                    return owner
                                        .rebuild(&mut owner.state.lock().unwrap(), &events);
                                }
                                Delivery::Live(event) => event,
                            };
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
                                if let Some(current) = state.current.as_ref().filter(|current| {
                                    state.interrupted.is_none()
                                        && state.steer.steered(&current.request_id)
                                }) && current.gate.replace()
                                {
                                    current.cancel.cancel();
                                }
                                return Ok(());
                            }
                            if matches!(event.kind, EventKind::TurnStart(_) | EventKind::TurnEnd(_))
                            {
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
                )
                .await?;
            ctx.provide_at(self.progress.clone(), service.progress.clone())?;
            ctx.provide_at(self.provider.clone(), service)?;
            Ok(Effect::Done)
        })
    }
}

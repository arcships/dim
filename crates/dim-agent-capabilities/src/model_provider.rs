mod images;
mod recovery;
pub use recovery::{
    Consume, MAX_ATTEMPTS, RetryObserver, RetryWaiting, WAIT_BUDGET, invoke as invoke_with_recovery,
};

use crate::{
    context_preparation::ContextPreparation,
    host::*,
    output::{Output, OutputGate, StartedCall, closure, consume_output},
    steer::{REPLACED_BY_STEER, SteerMark},
};
use dim_agent_runtime::{
    AgentError, CAPABILITY_RELOADING, Dispatcher, ModelProgress, Names, QueryComparison,
    QueryFilter, QueryOperand, QueryOutput, QueryPage, QueryPick, QuerySelect, QuerySpec,
    QueryValue, StateCell, Timeline, TimelineIdentity, TimelineQuery, TimelineQueryScope,
};
use dim_agent_types::*;
use futures_util::FutureExt;
use rutis::Ctx;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, Key, TypeKey};
use std::{
    collections::HashMap,
    num::NonZeroU64,
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

/// The terminal fact's durable failure: the host facts, including the attempt
/// count the recovery loop recorded.
fn model_failure(facts: ModelFailureFacts) -> ModelFailure {
    ModelFailure {
        kind: facts.kind,
        status: facts.status,
        provider_code: facts.provider_code,
        request_id: facts.request_id,
        reason: facts.reason,
        cause: facts.cause,
        attempts: facts.attempts.max(1),
        waited_ms: u64::try_from(facts.waited.as_millis()).unwrap_or(u64::MAX),
        retry_after_ms: facts
            .retry_after_stop
            .map(|wait| u64::try_from(wait.as_millis()).unwrap_or(u64::MAX)),
        retry_forbidden: facts.retry_forbidden,
    }
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
        turn_id: &str,
        request_id: &str,
        model: (&str, &str),
        failure_chain: Option<&FailureChain>,
        result: std::result::Result<FinishReason, HostError>,
        started: &[StartedCall],
    ) -> dim_agent_runtime::Result<()> {
        let (provider_id, model_id) = model;
        let outcome = match result {
            Ok(finish_reason) => ModelOutputOutcome::Completed { finish_reason },
            Err(HostError::Cancelled(reason)) => ModelOutputOutcome::Cancelled { reason },
            Err(HostError::Failed {
                message,
                budget,
                presentation,
                model,
                ..
            }) => {
                let facts = model.unwrap_or_else(|| {
                    let mut facts = ModelFailureFacts::unknown(Some(message.clone()));
                    if let Some(chain) = failure_chain {
                        facts.attempts = chain.attempts;
                        facts.waited = std::time::Duration::from_millis(chain.waited_ms);
                    }
                    Box::new(facts)
                });
                tracing::error!(
                    session_id = %self.identity.session_id,
                    branch_id = %self.identity.branch_id,
                    turn_id = %turn_id,
                    request_id = %request_id,
                    provider_id = %provider_id,
                    model_id = %model_id,
                    error = %message,
                    failure_kind = ?facts.kind,
                    status = facts.status,
                    provider_code = facts.provider_code.as_deref(),
                    attempts = facts.attempts,
                    waited_ms = facts.waited.as_millis(),
                    "Model request failed"
                );
                ModelOutputOutcome::Failed {
                    error: message,
                    budget,
                    presentation: presentation.map(|value| *value),
                    failure: Some(Box::new(model_failure(*facts))),
                }
            }
        };
        let mut events = Vec::new();
        if !matches!(outcome, ModelOutputOutcome::Completed { .. })
            && let Some(results) = closure(turn_id, request_id, started)
        {
            events.push(EventKind::ToolResult(results).into());
        }
        events.push(
            EventKind::ModelOutputEnd(ModelOutputEndData {
                turn_id: turn_id.to_owned(),
                request_id: request_id.to_owned(),
                outcome,
            })
            .into(),
        );
        self.timeline.append_batch_registered(events, |_| Ok(()))?;
        Ok(())
    }
    async fn rebuild(&self) -> dim_agent_runtime::Result<u64> {
        let through = self.timeline.committed();
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
        let control = self.timeline.query(request.clone()).await?;
        let QueryOutput::Rows { rows } = control.result else {
            unreachable!("model recovery selects control fields")
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
                                "ModelProvider",
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
                        "ModelProvider",
                        "a control event has no kind",
                    ));
                }
            }
        } else {
            None
        };
        let steer = SteerMark::restore(&self.timeline, through).await?;
        {
            let mut state = self.state.lock().unwrap();
            state.interrupted = interrupted;
            state.steer = steer;
        }

        let equal = |path: &str, value: serde_json::Value| QueryFilter::Compare {
            left: QueryOperand::Field { path: path.into() },
            comparison: QueryComparison::Eq,
            right: QueryOperand::Value { value },
        };
        request.query.pick = None;
        request.query.select = QuerySelect::Fields {
            fields: vec![
                "data.turn_id".into(),
                "data.request_id".into(),
                "data.request.providerId".into(),
                "data.request.modelId".into(),
                "position.sequence".into(),
                "position.branch_id".into(),
                "data.failure_chain".into(),
            ],
        };
        request.query.filter = Some(QueryFilter::And {
            conditions: vec![
                equal("kind", "model/call".into()),
                QueryFilter::Not {
                    condition: Box::new(QueryFilter::Exists {
                        condition: Box::new(QueryFilter::And {
                            conditions: vec![
                                equal("kind", "step/model-output-end".into()),
                                QueryFilter::Compare {
                                    left: QueryOperand::Field {
                                        path: "data.turn_id".into(),
                                    },
                                    comparison: QueryComparison::Eq,
                                    right: QueryOperand::Outer {
                                        path: "data.turn_id".into(),
                                    },
                                },
                                QueryFilter::Compare {
                                    left: QueryOperand::Field {
                                        path: "data.request_id".into(),
                                    },
                                    comparison: QueryComparison::Eq,
                                    right: QueryOperand::Outer {
                                        path: "data.request_id".into(),
                                    },
                                },
                            ],
                        }),
                    }),
                },
            ],
        });
        loop {
            let reply = self.timeline.query(request.clone()).await?;
            let QueryOutput::Rows { rows } = reply.result else {
                unreachable!("model recovery selects operation fields")
            };
            for row in rows {
                let Ok(
                    [
                        QueryValue::Value {
                            value: serde_json::Value::String(turn_id),
                        },
                        QueryValue::Value {
                            value: serde_json::Value::String(request_id),
                        },
                        QueryValue::Value {
                            value: serde_json::Value::String(provider_id),
                        },
                        QueryValue::Value {
                            value: serde_json::Value::String(model_id),
                        },
                        QueryValue::Value {
                            value: serde_json::Value::Number(sequence),
                        },
                        QueryValue::Value {
                            value: serde_json::Value::String(branch_id),
                        },
                        failure_chain,
                    ],
                ) = <[_; 7]>::try_from(row.values)
                else {
                    return Err(AgentError::fault(
                        "ModelProvider",
                        "a model call has invalid operation identities",
                    ));
                };
                let failure_chain = match failure_chain {
                    QueryValue::Missing => None,
                    QueryValue::Value { value } if value.is_null() => None,
                    QueryValue::Value { value } => Some(
                        serde_json::from_value::<FailureChain>(value).expect("model failure chain"),
                    ),
                    QueryValue::Elements { .. } => unreachable!("failure chain is an object"),
                };
                let mut tools = TimelineQuery {
                    scope: TimelineQueryScope::Branch,
                    from: Some(TimelinePosition {
                        branch_id,
                        sequence: sequence
                            .as_u64()
                            .expect("event sequences are unsigned integers"),
                    }),
                    through: Some(through),
                    include_publishing: true,
                    query: QuerySpec {
                        filter: Some(QueryFilter::And {
                            conditions: vec![
                                QueryFilter::In {
                                    value: QueryOperand::Field {
                                        path: "kind".into(),
                                    },
                                    values: vec![
                                        "step/tool-use-start".into(),
                                        "step/tool-use".into(),
                                    ],
                                },
                                equal("data.turn_id", turn_id.clone().into()),
                                equal("data.request_id", request_id.clone().into()),
                            ],
                        }),
                        select: QuerySelect::Fields {
                            fields: vec![
                                "kind".into(),
                                "data.call_id".into(),
                                "data.call.id".into(),
                            ],
                        },
                        group_by: Vec::new(),
                        pick: None,
                        order_by: Vec::new(),
                    },
                    page: Some(QueryPage {
                        limit: NonZeroU64::new(256).expect("a page contains call identities"),
                        after: None,
                    }),
                };
                let mut started = Vec::<StartedCall>::new();
                let mut starts = HashMap::new();
                loop {
                    let reply = self.timeline.query(tools.clone()).await?;
                    let QueryOutput::Rows { rows } = reply.result else {
                        unreachable!("tool-use recovery selects identities")
                    };
                    for row in rows {
                        let Ok(
                            [
                                QueryValue::Value {
                                    value: serde_json::Value::String(kind),
                                },
                                start,
                                complete,
                            ],
                        ) = <[_; 3]>::try_from(row.values)
                        else {
                            return Err(AgentError::fault(
                                "ModelProvider",
                                "a tool-use event has no kind",
                            ));
                        };
                        match kind.as_str() {
                            "step/tool-use-start" => {
                                let QueryValue::Value {
                                    value: serde_json::Value::String(id),
                                } = start
                                else {
                                    return Err(AgentError::fault(
                                        "ModelProvider",
                                        "a tool-use start has no call identity",
                                    ));
                                };
                                starts.entry(id.clone()).or_insert(started.len());
                                started.push(StartedCall {
                                    id,
                                    complete: false,
                                });
                            }
                            "step/tool-use" => {
                                let QueryValue::Value {
                                    value: serde_json::Value::String(id),
                                } = complete
                                else {
                                    return Err(AgentError::fault(
                                        "ModelProvider",
                                        "a tool-use has no call identity",
                                    ));
                                };
                                if let Some(index) = starts.get(&id) {
                                    started[*index].complete = true;
                                }
                            }
                            _ => {
                                unreachable!("tool-use recovery selects start and completion facts")
                            }
                        }
                    }
                    let Some(next) = reply.next else {
                        break;
                    };
                    tools
                        .page
                        .as_mut()
                        .expect("call identity queries are paged")
                        .after = Some(next);
                }
                self.complete(
                    &turn_id,
                    &request_id,
                    (&provider_id, &model_id),
                    failure_chain.as_ref(),
                    Err(HostError::failed(CAPABILITY_RELOADING)),
                    &started,
                )?;
            }
            let Some(next) = reply.next else {
                break;
            };
            request
                .page
                .as_mut()
                .expect("model operation queries are paged")
                .after = Some(next);
        }
        Ok(through)
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
                crate::context_policy::check_cancel(&cancel)?;
                let request = images::adapt(&call.request);
                self.context
                    .validate(&call.request_id, request.clone(), cancel.clone())
                    .await?;
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
                    call.failure_chain.as_ref(),
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
            let submitted = self.complete(
                &call.turn_id,
                &call.request_id,
                (&call.request.provider_id, &call.request.model_id),
                call.failure_chain.as_ref(),
                result,
                &output.started,
            );
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
            let read = owner.clone();
            dispatcher
                .consume(
                    ctx,
                    "ModelProvider".into(),
                    [
                        "model/call",
                        "stop/interrupt",
                        "step/model-start",
                        "input/applied",
                        "turn/start",
                        "turn/end",
                    ]
                    .map(String::from)
                    .to_vec(),
                    Arc::new(move || {
                        let read = read.clone();
                        Box::pin(async move { read.rebuild().await })
                    }),
                    Arc::new(move |event| {
                        let owner = owner.clone();
                        let failures = failures.clone();
                        let lifecycle = lifecycle.clone();
                        Box::pin(async move {
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
                                    &call.turn_id,
                                    &call.request_id,
                                    (&call.request.provider_id, &call.request.model_id),
                                    call.failure_chain.as_ref(),
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
                    }),
                )
                .await?;
            ctx.provide_at(self.progress.clone(), service.progress.clone())?;
            ctx.provide_at(self.provider.clone(), service)?;
            Ok(Effect::Done)
        })
    }
}

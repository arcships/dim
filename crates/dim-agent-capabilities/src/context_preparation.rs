use crate::{
    context_policy::{ContextPolicy, configured_request},
    history::{HistoryRenderer, context_configuration, context_messages, with_input_guide},
    host::*,
    prompt_state::{PromptSnapshot, scope_is_visible},
    steer::{REPLACED_BY_STEER, SteerMark},
};
use dim_agent_runtime::{
    AgentError, BranchScope, CAPABILITY_RELOADING, Capability, Dispatcher, InBranch, InSession,
    KeysOf, Timeline, TimelineIdentity, scoped,
};
use dim_agent_types::*;
use futures_util::FutureExt;
use rutis::{BoxFuture, CordisError, Ctx, Effect, TypedPlugin};
use std::{
    panic::AssertUnwindSafe,
    sync::{Arc, Mutex},
};
use tokio_util::{sync::CancellationToken, task::TaskTracker};
use uuid::Uuid;

/// The tool part of a configuration: the complete declaration and the
/// source versions it was built from.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ToolSetAdoption {
    pub tools: Vec<ToolDefinition>,
    pub tool_sources: Vec<ToolSourceVersion>,
}

/// Publishes adopted configuration to the branch Timeline; provided by
/// ContextPreparation for Prompt.
pub struct ConfigurationPublisher {
    timeline: Arc<Timeline>,
    identity: TimelineIdentity,
}

impl ConfigurationPublisher {
    /// Commits `changes` and the configuration as one batch. `inputs` is the
    /// caller's: given the prompt state before and after `changes`, it
    /// returns blocks committed in the same batch, each as its own Append
    /// input, in order.
    pub(crate) async fn publish(
        &self,
        changes: Vec<PromptChange>,
        model: Option<ModelSelection>,
        tools: Option<ToolSetAdoption>,
        inputs: impl FnOnce(&PromptSnapshot, &PromptSnapshot) -> Vec<InputBlock> + Send,
        register: impl FnOnce(&[Arc<AgentEvent>]) -> dim_agent_runtime::Result<()> + Send,
    ) -> dim_agent_runtime::Result<Option<u64>> {
        let context =
            crate::history::read_context(&self.timeline, self.timeline.sequence()).await?;
        let before = PromptSnapshot::from_context(&context);
        for change in &changes {
            let scope = match change {
                PromptChange::Set { entry } => &entry.scope,
                PromptChange::Revoke { scope, .. } => scope,
            };
            if !scope_is_visible(scope, &self.identity.branch_id, &context.ancestry) {
                return Err(AgentError::InvalidInput(
                    "prompt scope is outside branch ancestry".into(),
                ));
            }
            if !before.active(scope) {
                return Err(AgentError::Cancelled(
                    "prompt scope ended before adoption".into(),
                ));
            }
            if matches!(change, PromptChange::Set { entry }
                if entry.delivery == PromptDelivery::System && !matches!(scope, PromptScope::Branch { .. }))
            {
                return Err(AgentError::InvalidInput(
                    "system prompts require branch scope".into(),
                ));
            }
        }
        let previous_configuration = context_configuration(&context);
        // The model part and the tool part update separately; each keeps
        // the other from the last adopted configuration.
        let base = previous_configuration
            .as_ref()
            .map(|data| (*data.settings).clone());
        let configuration = match (model, tools) {
            (None, None) => None,
            (model, tools) => {
                let mut settings = match (model, base) {
                    (Some(model), base) => ModelConfiguration {
                        provider_id: model.provider_id,
                        model_id: model.model_id,
                        capability_snapshot: model.capability_snapshot,
                        tools: base.as_ref().and_then(|base| base.tools.clone()),
                        tool_sources: base.map_or_else(Vec::new, |base| base.tool_sources),
                        config: model.config,
                        metadata: model.metadata,
                    },
                    (None, Some(base)) => base,
                    (None, None) => {
                        return Err(AgentError::InvalidInput(
                            "tool set adoption requires a model configuration".into(),
                        ));
                    }
                };
                if let Some(tools) = tools {
                    settings.tools = Some(tools.tools);
                    settings.tool_sources = tools.tool_sources;
                }
                Some(settings)
            }
        };
        let effective_configuration = configuration.clone().or_else(|| {
            previous_configuration
                .as_ref()
                .map(|data| (*data.settings).clone())
        });
        let mut drafts: Vec<EventDraft> = Vec::new();
        if !changes.is_empty() {
            let revision = Uuid::new_v4().to_string();
            let data = PromptChangedData {
                timeline: self.identity.branch_id.clone(),
                revision: revision.clone(),
                previous: before.revision.clone(),
                reason: if before.revision.is_some() {
                    PromptChangeReason::Refresh
                } else {
                    PromptChangeReason::Initialize
                },
                changes,
            };
            let synthetic = AgentEvent {
                id: "candidate".into(),
                sequence: 0,
                create_at: 0,
                tags: None,
                kind: EventKind::PromptChanged(data.clone()),
            };
            let mut after = before.clone();
            after.apply(&synthetic);
            let blocks = inputs(&before, &after);
            drafts.push(EventKind::PromptChanged(data).into());
            // One input per item, in the caller's order.
            drafts.extend(blocks.into_iter().map(|block| {
                EventKind::InputMessage(InputMessageData {
                    blocks: vec![block],
                    mode: Some(InputMode::Append),
                    ..Default::default()
                })
                .into()
            }));
            if after.system_revision != before.system_revision {
                let settings = effective_configuration.clone().ok_or_else(|| {
                    AgentError::InvalidInput("system prompt update requires configuration".into())
                })?;
                drafts.push(
                    EventKind::ContextConfigured(ContextConfiguredData {
                        timeline: self.identity.branch_id.clone(),
                        revision: Uuid::new_v4().to_string(),
                        previous: previous_configuration.as_ref().map(|data| PromptReference {
                            timeline: data.timeline.clone(),
                            revision: data.revision.clone(),
                        }),
                        prompt: after.system_revision.clone(),
                        settings: Box::new(settings),
                        system: Some(with_input_guide(Some(SystemPrompt::Text(after.system)))),
                    })
                    .into(),
                );
            }
        }
        if let Some(settings) = configuration
            && previous_configuration
                .as_ref()
                .is_none_or(|previous| *previous.settings != settings)
            && !drafts
                .iter()
                .any(|draft| matches!(draft.kind, EventKind::ContextConfigured(_)))
        {
            drafts.push(
                EventKind::ContextConfigured(ContextConfiguredData {
                    timeline: self.identity.branch_id.clone(),
                    revision: Uuid::new_v4().to_string(),
                    previous: previous_configuration.as_ref().map(|data| PromptReference {
                        timeline: data.timeline.clone(),
                        revision: data.revision.clone(),
                    }),
                    prompt: before.system_revision.clone(),
                    settings: Box::new(settings),
                    // Only the model or tool part changed: the adopted
                    // system stays; a first configuration renders it.
                    system: previous_configuration
                        .as_ref()
                        .and_then(|data| data.system.clone())
                        .or_else(|| {
                            Some(with_input_guide(Some(SystemPrompt::Text(
                                before.system.clone(),
                            ))))
                        }),
                })
                .into(),
            );
        }
        if drafts.is_empty() {
            register(&[])?;
            return Ok(None);
        }
        let published = self.timeline.append_batch_registered(drafts, register)?;
        let sequence = published
            .last()
            .expect("non-empty publication batch")
            .sequence;
        let mut publication = self.timeline.watch();
        while publication.borrow().0 < sequence {
            if let Some(error) = self.timeline.failure() {
                return Err(error);
            }
            publication.changed().await.map_err(|_| {
                AgentError::fault("ConfigurationPublisher", "publication channel ended")
            })?;
        }
        Ok(Some(sequence))
    }
}

#[derive(Default)]
struct PreparationState {
    /// The running request and its cancellation.
    current: Option<(String, CancellationToken)>,
    /// A preparation handed over after a steer of its request was applied is
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

/// What a preparation at the committed head would write into
/// `context/budget`, plus the compaction threshold.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextUsage {
    pub through: u64,
    pub context_window: f64,
    pub output_tokens: f64,
    pub threshold_tokens: f64,
    pub estimated_input_tokens: f64,
    pub baseline_source: BaselineSource,
}
#[derive(Debug, Clone, thiserror::Error)]
pub enum ContextOpError {
    /// No `context/configured` in the branch's history.
    #[error("Context is not configured; adopt a model first")]
    NotConfigured,
    /// The window facts could not be resolved, or no policy is bound.
    #[error("{0}")]
    Model(HostError),
    #[error("{0}")]
    Read(AgentError),
}

pub struct ContextPreparation {
    state: Mutex<PreparationState>,
    tasks: TaskTracker,
    errors: Mutex<Vec<AgentError>>,
    prepared: Mutex<Option<(String, u64, Arc<ModelRequest>)>>,
    host: Arc<PrepareContextHost>,
    policy: Option<ContextPolicy>,
    history: Arc<HistoryRenderer>,
    timeline: Arc<Timeline>,
    identity: Arc<TimelineIdentity>,
}
impl ContextPreparation {
    /// Rebuilds the interrupt mark; a committed preparation with neither a
    /// `model/call` nor an output end was left by a reload or restart and
    /// fails without running.
    fn rebuild(
        &self,
        state: &mut PreparationState,
        events: &[Arc<AgentEvent>],
    ) -> dim_agent_runtime::Result<()> {
        state.rebuilt = events.last().map_or(0, |event| event.sequence);
        let mut open: Vec<ContextPrepareData> = Vec::new();
        // An automatic compaction start without its result was in flight.
        let mut started: Vec<(u64, ContextCompactionStartData)> = Vec::new();
        let settled = |open: &mut Vec<ContextPrepareData>, turn: &str, request: &str| {
            open.retain(|source| source.turn_id != turn || source.request_id != request)
        };
        for event in events {
            state.steer.apply(event);
            match &event.kind {
                EventKind::StopInterrupt(data) => state.interrupted = Some(data.reason.clone()),
                EventKind::TurnStart(_) | EventKind::TurnEnd(_) => state.interrupted = None,
                EventKind::ContextPrepare(source) => open.push(source.clone()),
                EventKind::ModelCall(call) => settled(&mut open, &call.turn_id, &call.request_id),
                EventKind::ModelOutputEnd(end) => settled(&mut open, &end.turn_id, &end.request_id),
                EventKind::ContextCompactionStart(data) if data.command.is_none() => {
                    started.push((event.sequence, data.clone()))
                }
                EventKind::ContextCompaction(data) => {
                    started.retain(|(sequence, _)| Some(*sequence) != data.start)
                }
                _ => {}
            }
        }
        for (start, data) in started {
            self.timeline.append(
                EventKind::ContextCompaction(ContextCompactionData {
                    turn_id: data.turn_id,
                    request_id: data.request_id,
                    command: None,
                    start: Some(start),
                    through: data.through,
                    outcome: ContextCompactionResult::Cancelled {
                        reason: CAPABILITY_RELOADING.into(),
                    },
                })
                .into(),
            )?;
        }
        for source in open {
            self.complete(source, Err(HostError::failed(CAPABILITY_RELOADING)))?;
        }
        Ok(())
    }
    fn complete(
        &self,
        source: ContextPrepareData,
        result: std::result::Result<Arc<ModelRequest>, HostError>,
    ) -> dim_agent_runtime::Result<()> {
        let kind = match result {
            Ok(request) => EventKind::ModelCall(ModelCallData {
                turn_id: source.turn_id,
                request_id: source.request_id,
                request,
                configuration: source.configuration,
                prompt: source.prompt,
            }),
            Err(error) => EventKind::ModelOutputEnd(ModelOutputEndData {
                turn_id: source.turn_id,
                request_id: source.request_id,
                outcome: match error {
                    HostError::Cancelled(reason) => ModelOutputOutcome::Cancelled { reason },
                    HostError::Failed {
                        message,
                        budget,
                        presentation,
                        ..
                    } => ModelOutputOutcome::Failed {
                        error: message,
                        budget,
                        presentation: presentation.map(|value| *value),
                    },
                },
            }),
        };
        self.timeline.append(kind.into())?;
        Ok(())
    }
    async fn run(
        &self,
        source: ContextPrepareData,
        cancel: CancellationToken,
        generation: CancellationToken,
    ) -> dim_agent_runtime::Result<()> {
        let span = tracing::info_span!(
            "context.prepare",
            session_id = %self.identity.session_id,
            branch_id = %self.identity.branch_id,
            turn_id = %source.turn_id,
            request_id = %source.request_id,
            outcome = tracing::field::Empty,
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
            match AssertUnwindSafe(self.prepare(source.clone(), cancel, &generation))
                .catch_unwind()
                .await
            {
                Ok(result) => (result, None),
                Err(panic) => {
                    let message = panic_message(panic);
                    (
                        Err(HostError::failed(&message)),
                        Some(AgentError::fault("ContextPreparation", message)),
                    )
                }
            }
        };
        // The final recheck, the terminal and releasing the request are one
        // step under the state lock the steer handler takes: a steer consumed
        // before it replaces the request, one consumed after it finds no
        // running preparation (time-bus §4).
        let (outcome, submitted) = {
            let mut state = self.state.lock().unwrap();
            let replaced = state.interrupted.is_none() && state.steer.steered(&source.request_id);
            let result = match result {
                Err(HostError::Cancelled(_)) if generation.is_cancelled() => {
                    Err(HostError::Cancelled(CAPABILITY_RELOADING.into()))
                }
                // Whatever the host returned after a steer, the request ends as
                // replaced; a completed compaction stays a fact.
                _ if replaced => Err(HostError::Cancelled(REPLACED_BY_STEER.into())),
                result => result,
            };
            let outcome = match (&result, &fault) {
                (_, Some(_)) => "defect",
                (Ok(_), _) => "returned",
                (Err(HostError::Cancelled(_)), _) => "interrupted",
                (Err(_), _) => "failure",
            };
            let submitted = self.complete(source, result);
            state.current = None;
            (outcome, submitted)
        };
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
    pub async fn validate(
        &self,
        request: Arc<ModelRequest>,
        cancel: CancellationToken,
    ) -> std::result::Result<(), HostError> {
        validate_capability_snapshot(&request)?;
        match &self.policy {
            Some(policy) => policy.validate(request, cancel).await,
            None => Ok(()),
        }
    }
    /// The current head's budget; writes nothing.
    pub async fn usage(
        &self,
        cancel: CancellationToken,
    ) -> std::result::Result<ContextUsage, ContextOpError> {
        let policy = self.policy.as_ref().ok_or_else(|| {
            ContextOpError::Model(HostError::failed("Context compaction is not available"))
        })?;
        let through = self.timeline.committed();
        let context = self
            .history
            .context(&self.timeline, through)
            .await
            .map_err(ContextOpError::Read)?;
        let history = (self.history.render)(&context).map_err(|error| {
            ContextOpError::Read(AgentError::fault("ContextPreparation", error))
        })?;
        let request =
            configured_request(&context, &history).ok_or(ContextOpError::NotConfigured)?;
        let view = policy
            .view(request, context, history, Vec::new())
            .map_err(ContextOpError::Model)?;
        let model = policy
            .resolver
            .resolve(view.prepared.clone(), cancel)
            .await
            .map_err(ContextOpError::Model)?;
        let estimate = policy
            .estimate(&view.prepared, model.as_ref(), view.events())
            .map_err(ContextOpError::Model)?;
        Ok(ContextUsage {
            through,
            context_window: model.context_window(),
            output_tokens: model.output_tokens(),
            threshold_tokens: model.threshold_tokens(),
            estimated_input_tokens: estimate.input,
            baseline_source: estimate.source,
        })
    }
    async fn prepare(
        &self,
        source: ContextPrepareData,
        cancel: CancellationToken,
        generation: &CancellationToken,
    ) -> std::result::Result<Arc<ModelRequest>, HostError> {
        crate::context_policy::check_cancel(&cancel)?;
        let cached = self
            .prepared
            .lock()
            .unwrap()
            .as_ref()
            .filter(|(turn, through, _)| turn == &source.turn_id && *through == source.through)
            .map(|(_, _, request)| request.clone());
        let request = match cached {
            Some(request) => request,
            None => {
                self.host
                    .0
                    .prepare(
                        source.request.clone(),
                        ContextSource {
                            turn_id: source.turn_id.clone(),
                            request_id: source.request_id.clone(),
                            through: source.through,
                        },
                        cancel.clone(),
                    )
                    .await?
            }
        };
        validate_capability_snapshot(&request)?;
        *self.prepared.lock().unwrap() =
            Some((source.turn_id.clone(), source.through, request.clone()));
        let context = self
            .history
            .context(&self.timeline, source.through)
            .await
            .map_err(|error| HostError::failed(error.to_string()))?;
        crate::context_policy::check_cancel(&cancel)?;
        let history = (self.history.render)(&context).map_err(HostError::failed)?;
        let prefix = context_messages(&context, &history);
        if request.messages.len() < prefix.len()
            || !prefix
                .iter()
                .zip(&request.messages)
                .all(|(entry, message)| entry == message.as_ref())
        {
            return Err(HostError::failed(
                "Context preparation must preserve the timeline history prefix; additional messages may be appended.",
            ));
        }
        // Why a compaction this preparation started was cancelled.
        let cancelled = || {
            let state = self.state.lock().unwrap();
            if generation.is_cancelled() || state.closed {
                return CAPABILITY_RELOADING.to_owned();
            }
            state
                .stop_reason
                .clone()
                .or_else(|| {
                    state
                        .steer
                        .steered(&source.request_id)
                        .then(|| REPLACED_BY_STEER.to_owned())
                })
                .unwrap_or_else(|| "Operation cancelled".to_owned())
        };
        match &self.policy {
            Some(policy) => {
                policy
                    .prepare(request, &source, context, history, cancel, &cancelled)
                    .await
            }
            None => Ok(request),
        }
    }
}

fn validate_capability_snapshot(request: &ModelRequest) -> Result<(), HostError> {
    request
        .capability_snapshot
        .as_ref()
        .map_or(Ok(()), |snapshot| {
            snapshot
                .validate_for(&request.provider_id, &request.model_id)
                .map_err(|message| HostError::classified(HostErrorKind::InvalidRequest, message))
        })
}
pub struct ContextPreparationPlugin {
    scope: BranchScope,
}
impl ContextPreparationPlugin {
    pub fn new(scope: &BranchScope) -> Self {
        Self { scope: *scope }
    }
}
impl TypedPlugin for ContextPreparationPlugin {
    type Deps = (
        InBranch<Timeline>,
        InBranch<Dispatcher>,
        InSession<PrepareContextHost>,
        InSession<ContextModelHost>,
        InSession<CompactionStrategyHost>,
        InSession<HistoryRenderer>,
        InBranch<TimelineIdentity>,
    );
    fn name(&self) -> &str {
        "ContextPreparation"
    }
    fn apply<'a>(
        &'a self,
        ctx: &'a Ctx,
        (
            InBranch(timeline),
            InBranch(dispatcher),
            InSession(host),
            InSession(context),
            InSession(strategy),
            InSession(history),
            InBranch(identity),
        ): Self::Deps,
    ) -> BoxFuture<'a, std::result::Result<Effect, CordisError>> {
        Box::pin(async move {
            let publisher = Arc::new(ConfigurationPublisher {
                timeline: timeline.clone(),
                identity: (*identity).clone(),
            });
            ctx.provide_as(self.scope.key::<ConfigurationPublisher>(), publisher)?;
            let service = Arc::new(ContextPreparation {
                state: Mutex::new(PreparationState::default()),
                tasks: TaskTracker::new(),
                errors: Mutex::new(Vec::new()),
                prepared: Mutex::new(None),
                host,
                policy: ContextPolicy::bind(&timeline, &context, &strategy, &history)
                    .map_err(|error| CordisError::PluginFailed(error.into()))?,
                timeline,
                history,
                identity,
            });
            let owner = service.clone();
            ctx.effect(move || {
                Effect::AsyncDisposer(Box::new(move || {
                    Box::pin(async move {
                        {
                            let mut state = owner.state.lock().unwrap();
                            state.closed = true;
                            if let Some((_, cancel)) = &state.current {
                                cancel.cancel();
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
            // Subscribe, then rebuild under the gate; see the reload protocol.
            let gate = Arc::new(tokio::sync::Mutex::new(()));
            let rebuilding = gate.lock().await;
            let handler_gate = gate.clone();
            dispatcher.subscribe(
                ctx,
                "ContextPreparation",
                &[
                    "context/prepare",
                    "stop/interrupt",
                    "step/model-start",
                    "input/applied",
                    "turn/start",
                    "turn/end",
                ],
                move |event| {
                    let owner = owner.clone();
                    let failures = failures.clone();
                    let lifecycle = lifecycle.clone();
                    let gate = handler_gate.clone();
                    Box::pin(async move {
                        let _ready = gate.lock().await;
                        if event.sequence <= owner.state.lock().unwrap().rebuilt {
                            return Ok(());
                        }
                        if let EventKind::StopInterrupt(data) = &event.kind {
                            let mut state = owner.state.lock().unwrap();
                            state.interrupted = Some(data.reason.clone());
                            if let Some((_, cancel)) = state.current.clone() {
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
                            // A steer of the running request replaces it; an
                            // interrupt already ended it.
                            if let Some((request, cancel)) = &state.current
                                && state.interrupted.is_none()
                                && state.steer.steered(request)
                            {
                                cancel.cancel();
                            }
                            return Ok(());
                        }
                        if matches!(event.kind, EventKind::TurnStart(_) | EventKind::TurnEnd(_)) {
                            owner.state.lock().unwrap().interrupted = None;
                            return Ok(());
                        }
                        let EventKind::ContextPrepare(source) = &event.kind else {
                            unreachable!()
                        };
                        let mut state = owner.state.lock().unwrap();
                        if state.closed {
                            return Err(AgentError::Closed("ContextPreparation".into()));
                        }
                        if state.current.is_some() {
                            return Err(AgentError::fault(
                                "ContextPreparation",
                                "concurrent preparation",
                            ));
                        }
                        let reason = state.interrupted.clone().or_else(|| {
                            state
                                .steer
                                .steered(&source.request_id)
                                .then(|| REPLACED_BY_STEER.to_owned())
                        });
                        if let Some(reason) = reason {
                            drop(state);
                            return owner
                                .complete(source.clone(), Err(HostError::Cancelled(reason)));
                        }
                        state.stop_reason = None;
                        let cancel = lifecycle.child_token();
                        state.current = Some((source.request_id.clone(), cancel.clone()));
                        let source = source.clone();
                        let worker = owner.clone();
                        owner.tasks.spawn(async move {
                            let result = AssertUnwindSafe(worker.run(source, cancel, lifecycle))
                                .catch_unwind()
                                .await
                                .map_err(|panic| {
                                    AgentError::fault("ContextPreparation", panic_message(panic))
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
            ctx.provide_as(self.scope.key::<ContextPreparation>(), service)?;
            Ok(Effect::Done)
        })
    }
}
impl Capability for ContextPreparationPlugin {
    fn keys(&self) -> KeysOf<Self> {
        scoped::<Self::Deps, _>(&self.scope)
    }
}

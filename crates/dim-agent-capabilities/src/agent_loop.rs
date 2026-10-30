use crate::{
    context_policy::{CompactionStep, ContextPolicy, configured_request},
    history::{HistoryRenderer, context_configuration, context_messages},
    host::{CompactionStrategyHost, ContextModelHost, HostError, PrepareModelHost},
    prompt_state::PromptSnapshot,
    steer::REPLACED_BY_STEER,
    tool_binding::ToolBinding,
};
use dim_agent_runtime::{
    AgentError, CAPABILITY_RELOADING, Delivery, Dispatcher, LoopPhase, LoopSnapshot, LoopState,
    Names, Result, StateCell, Timeline, TimelineIdentity,
};
use dim_agent_types::*;
use futures_util::FutureExt;
use rutis::Ctx;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, ErrorKind, Key, TypeKey};
use std::collections::HashSet;
use std::panic::AssertUnwindSafe;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

#[derive(PartialEq)]
enum Stage {
    Preparing,
    Model,
    Tools,
}
struct Request {
    id: String,
    stage: Stage,
    calls: Vec<ToolCall>,
    call_event_id: Option<String>,
    source: Option<ContextPrepareData>,
    preparation: Option<CancellationToken>,
    /// Calls of this request in the order they started (`step/tool-use-start`).
    started: Vec<String>,
    /// A steer was consumed while this Loop still owned the request, before
    /// its `context/prepare`: it ends Cancelled instead of being handed over.
    replaced: bool,
}
struct Turn {
    id: String,
    /// `turn/start` was dispatched; before that the turn only guards
    /// against a second start and is not published as running.
    open: bool,
    request: Option<Request>,
    ending: bool,
    interrupted: Option<String>,
    /// Set for a turn rebuilt after a reload: it ends Failed once its
    /// outstanding operations have terminals, and starts no new work.
    failed: Option<String>,
}
/// An applied input waiting for this Loop to consume it.
struct Applied {
    id: String,
    /// Sequence of its `input/applied`.
    sequence: u64,
    mode: InputMode,
}
impl Applied {
    fn new(event: &AgentEvent, data: &InputAppliedData) -> Self {
        Self {
            id: data.input_id.clone(),
            sequence: event.sequence,
            mode: data.mode.unwrap_or_default(),
        }
    }
}
/// The one outstanding manual compaction command: written and without a
/// result. Repeated requests join it (`AgentLoop::compact`); deduplication
/// belongs to the interaction boundary, not to a queue of commands.
struct Command {
    sequence: u64,
    /// The Loop has consumed the command; it starts at the next boundary.
    consumed: bool,
    /// The running step's cancellation.
    running: Option<CancellationToken>,
}
#[derive(Default)]
struct CompactionSlot {
    command: Option<Command>,
    /// Why an interrupt cancelled the running step.
    stop_reason: Option<String>,
}
impl CompactionSlot {
    fn running(&self) -> bool {
        self.command
            .as_ref()
            .is_some_and(|command| command.running.is_some())
    }
    /// Consumed and waiting for a boundary.
    fn waiting(&self) -> bool {
        self.command
            .as_ref()
            .is_some_and(|command| command.consumed && command.running.is_none())
    }
}
/// The reason a `context/compact` consumed while another command is
/// outstanding is skipped.
pub const ALREADY_IN_PROGRESS: &str = "a compaction is already in progress";
pub struct AgentLoop {
    turn: Mutex<Option<Turn>>,
    /// Locked after `turn`.
    compaction: Mutex<CompactionSlot>,
    observable: Arc<StateCell<LoopSnapshot>>,
    timeline: Arc<Timeline>,
    /// Sequence of the last `stop/interrupt`. Inputs applied before it that
    /// were not consumed no longer drive a turn by themselves; they join the
    /// next turn that a later input starts.
    interrupted_at: AtomicU64,
    /// Applied inputs not yet consumed, in order. Folded from this Loop's
    /// own events.
    pending: Mutex<Vec<Applied>>,
    /// Inputs this Loop wrote `input/consumed` for, until that event is dispatched.
    claimed: Mutex<HashSet<String>>,
    prepare: Arc<PrepareModelHost>,
    /// Fixes each request's tools; released when its calls settle or its
    /// turn ends.
    binding: Arc<ToolBinding>,
    history: Arc<HistoryRenderer>,
    /// The Session's guide entries, for the host-prepared path.
    guide: Arc<crate::prompt_guide::GuideEntries>,
    /// This branch's timeline id, which selects the entries in force.
    branch: String,
    /// Compaction is bound: budget recovery and manual steps run.
    policy: Option<ContextPolicy>,
    tasks: TaskTracker,
    lifecycle: CancellationToken,
    dispatcher: Arc<Dispatcher>,
}
impl AgentLoop {
    fn publish(&self, slot: &Option<Turn>) {
        let open = slot.as_ref().filter(|turn| turn.open);
        let phase = match slot {
            Some(turn) if turn.interrupted.is_some() => LoopPhase::Stopping,
            Some(turn) if turn.open => LoopPhase::Running,
            _ => LoopPhase::Idle,
        };
        self.observable.set(LoopSnapshot {
            phase,
            turn_id: open.map(|turn| turn.id.clone()),
        });
    }
    /// Whether an unclaimed applied input asks to run: a steer or queue
    /// input applied after the last interrupt. Append inputs never drive;
    /// they join whatever request is built next.
    fn driven(&self) -> bool {
        let interrupted_at = self.interrupted_at.load(Ordering::Acquire);
        let claimed = self.claimed.lock().unwrap();
        self.pending.lock().unwrap().iter().any(|input| {
            input.mode != InputMode::Append
                && input.sequence > interrupted_at
                && !claimed.contains(&input.id)
        })
    }
    /// The only writer of `context/compact`. With a command outstanding
    /// (written and without a result) it writes nothing and returns that
    /// command's sequence, so every caller waits for the same result;
    /// otherwise it appends the command and returns its sequence.
    pub fn compact(&self) -> Result<u64> {
        let mut compaction = self.compaction.lock().unwrap();
        if let Some(command) = &compaction.command {
            return Ok(command.sequence);
        }
        let sequence = self
            .timeline
            .append(EventKind::ContextCompact(ContextCompactData {}).into())?
            .sequence;
        compaction.command = Some(Command {
            sequence,
            consumed: false,
            running: None,
        });
        Ok(sequence)
    }
    /// Starts a turn when idle and an applied input drives one; nothing
    /// starts while a compaction step runs.
    fn try_start(&self, slot: &mut Option<Turn>) -> Result<()> {
        if slot.is_some() || self.compaction.lock().unwrap().running() {
            return Ok(());
        }
        if !self.driven() {
            return Ok(());
        }
        let id = uuid::Uuid::new_v4().to_string();
        *slot = Some(Turn {
            id: id.clone(),
            open: false,
            request: None,
            ending: false,
            interrupted: None,
            failed: None,
        });
        self.timeline
            .append(EventKind::TurnStart(TurnStartData { turn_id: id }).into())?;
        Ok(())
    }
    fn end(&self, turn: &mut Turn, outcome: TurnOutcome) -> Result<()> {
        if turn.ending {
            return Err(AgentError::fault("Loop", "turn ended twice"));
        }
        turn.ending = true;
        let outcome = match &turn.failed {
            Some(error) => TurnOutcome::Failed {
                error: error.clone(),
            },
            None => outcome,
        };
        let mut events = Vec::new();
        // Calls of a completed output that were never dispatched are closed
        // here, in the turn end's batch (ADR agent-029 decision 3).
        if let Some(request) = turn.request.as_ref().filter(|request| {
            request.stage == Stage::Tools
                && request.call_event_id.is_none()
                && !request.calls.is_empty()
        }) {
            let reason = match &outcome {
                TurnOutcome::Completed { reason } | TurnOutcome::Cancelled { reason } => reason,
                TurnOutcome::Failed { error } => error,
            };
            let mut calls: Vec<&ToolCall> = request.calls.iter().collect();
            calls.sort_by_key(|call| request.started.iter().position(|id| *id == call.id));
            events.push(
                EventKind::ToolResult(ToolResultData {
                    turn_id: turn.id.clone(),
                    request_id: request.id.clone(),
                    call_event_id: None,
                    results: calls
                        .into_iter()
                        .map(|call| ToolCallResult {
                            call_id: call.id.clone(),
                            result: ToolResult::error(format!("Tool was not executed: {reason}")),
                        })
                        .collect(),
                })
                .into(),
            );
        }
        events.push(
            EventKind::TurnEnd(TurnEndData {
                turn_id: turn.id.clone(),
                outcome,
            })
            .into(),
        );
        self.timeline.append_batch_registered(events, |_| Ok(()))?;
        Ok(())
    }
    /// A step boundary unless `source` retries a request: a pending
    /// compaction step runs before the next request, which waits for it.
    fn next_model(
        self: &Arc<Self>,
        turn: &mut Turn,
        source: Option<ContextPrepareData>,
        at: u64,
    ) -> Result<()> {
        if let Some(error) = turn.failed.clone() {
            return self.end(turn, TurnOutcome::Failed { error });
        }
        if let Some(reason) = &turn.interrupted {
            return self.end(
                turn,
                TurnOutcome::Cancelled {
                    reason: reason.clone(),
                },
            );
        }
        if source.is_none() {
            let mut compaction = self.compaction.lock().unwrap();
            if compaction.running() {
                turn.request = None;
                return Ok(());
            }
            if compaction.waiting() {
                turn.request = None;
                self.start_step(&mut compaction, Some(turn.id.clone()), at);
                return Ok(());
            }
        }
        self.start_request(turn, source)
    }
    /// Starts the waiting step in a tracked task; its result resumes the Loop.
    fn start_step(
        self: &Arc<Self>,
        compaction: &mut CompactionSlot,
        turn_id: Option<String>,
        through: u64,
    ) {
        if self.lifecycle.is_cancelled() {
            return;
        }
        let cancel = self.lifecycle.child_token();
        let command = compaction.command.as_mut().expect("a waiting step");
        command.running = Some(cancel.clone());
        let command = command.sequence;
        compaction.stop_reason = None;
        let owner = self.clone();
        self.tasks.spawn(async move {
            let result = AssertUnwindSafe(owner.run_step(turn_id, command, through, cancel))
                .catch_unwind()
                .await
                .map_err(|panic| AgentError::fault("Loop", dim_agent_runtime::panic_message(panic)))
                .and_then(|result| result);
            if let Err(error) = result {
                owner.dispatcher.fail(error);
            }
        });
    }
    /// Builds the request as a preparation would and runs the one compaction
    /// path. A request that cannot be built ends the command without a start.
    async fn run_step(
        &self,
        turn_id: Option<String>,
        command: u64,
        through: u64,
        cancel: CancellationToken,
    ) -> Result<()> {
        let unstarted = |outcome| {
            self.timeline.append(
                EventKind::ContextCompaction(ContextCompactionData {
                    turn_id: turn_id.clone(),
                    request_id: None,
                    command: Some(command),
                    start: None,
                    through,
                    outcome,
                })
                .into(),
            )?;
            Ok(())
        };
        let failed = |error: String| {
            unstarted(ContextCompactionResult::Failed {
                error,
                diagnostics: None,
            })
        };
        let cancelled = || {
            if self.lifecycle.is_cancelled() {
                return CAPABILITY_RELOADING.to_owned();
            }
            self.compaction
                .lock()
                .unwrap()
                .stop_reason
                .clone()
                .unwrap_or_else(|| "Operation cancelled".to_owned())
        };
        let Some(policy) = &self.policy else {
            return failed("Context compaction is not available".into());
        };
        let (context, history) = self
            .history
            .rendered_context(&self.timeline, through)
            .await?;
        let Some(request) = configured_request(&context, &history) else {
            return failed("Context is not configured; adopt a model first".into());
        };
        let view = policy
            .view(request, context, history, Vec::new())
            .map_err(|error| AgentError::fault("Loop", error.to_string()))?;
        let model = match policy
            .resolver
            .resolve(view.prepared.clone(), cancel.clone())
            .await
        {
            Ok(_) | Err(_) if cancel.is_cancelled() => {
                return unstarted(ContextCompactionResult::Cancelled {
                    reason: cancelled(),
                });
            }
            Ok(model) => model,
            Err(error) => return failed(error.to_string()),
        };
        let estimate = policy
            .estimate(&view.prepared, model.as_ref(), view.events())
            .map_err(|error| AgentError::fault("Loop", error.to_string()))?;
        policy
            .compact_step(
                CompactionStep {
                    turn_id,
                    request_id: None,
                    command: Some(command),
                    through,
                    trigger: CompactionTrigger::Manual,
                    provider_budget: None,
                },
                &view,
                model,
                &estimate,
                cancel,
                &cancelled,
            )
            .await
            .map_err(|error| match error {
                HostError::Cancelled(reason) => AgentError::Cancelled(reason),
                error => AgentError::fault("Loop", error.to_string()),
            })?;
        Ok(())
    }
    /// Writes the inputs the next request consumes, then its `step/model-start`.
    fn start_request(&self, turn: &mut Turn, source: Option<ContextPrepareData>) -> Result<()> {
        let chosen: Vec<String> = match source {
            Some(_) => Vec::new(),
            None => {
                let claimed = self.claimed.lock().unwrap();
                self.pending
                    .lock()
                    .unwrap()
                    .iter()
                    .map(|input| input.id.clone())
                    .filter(|id| !claimed.contains(id))
                    .collect()
            }
        };
        let id = uuid::Uuid::new_v4().to_string();
        let mut events: Vec<EventDraft> = chosen
            .iter()
            .map(|input_id| {
                EventKind::InputConsumed(InputConsumedData {
                    input_id: input_id.clone(),
                })
                .into()
            })
            .collect();
        events.push(
            EventKind::ModelStart(ModelStartData {
                turn_id: turn.id.clone(),
                request_id: id.clone(),
            })
            .into(),
        );
        self.claimed.lock().unwrap().extend(chosen);
        for event in events {
            self.timeline.append(event)?;
        }
        turn.request = Some(Request {
            id,
            stage: Stage::Preparing,
            calls: Vec::new(),
            call_event_id: None,
            source,
            preparation: None,
            started: Vec::new(),
            replaced: false,
        });
        Ok(())
    }
    /// An input applied while a turn runs joins it as soon as the request
    /// allows. Every case waits for the request's terminal: a request this
    /// Loop still owns (not yet handed over with `context/prepare`) ends
    /// Cancelled now; one handed over is replaced by its current owner unless
    /// it already received model output; a tool batch carries the input with
    /// its results (ADR agent-029 decision 5).
    fn steer(&self, turn: &mut Turn) {
        if !turn.open
            || turn.ending
            || turn.interrupted.is_some()
            || turn.failed.is_some()
            || !self.driven()
        {
            return;
        }
        let Some(request) = turn
            .request
            .as_mut()
            .filter(|request| request.stage != Stage::Tools)
        else {
            return;
        };
        request.replaced = true;
        if let Some(cancel) = &request.preparation {
            cancel.cancel();
        }
    }
    /// Rebuilds the current turn from this branch's committed local events.
    /// An unfinished turn is marked failed; with nothing outstanding it ends now.
    /// A command without a result (its step running, or waiting) is settled
    /// `cancelled` once, with its start if it had one; it is not resumed.
    fn rebuild(&self, slot: &mut Option<Turn>, events: &[Arc<AgentEvent>]) -> Result<()> {
        let mut commands: Vec<u64> = Vec::new();
        let mut starts: Vec<(u64, ContextCompactionStartData)> = Vec::new();
        for event in events {
            match &event.kind {
                EventKind::ContextCompact(_) => commands.push(event.sequence),
                EventKind::ContextCompactionStart(data) if data.command.is_some() => {
                    starts.push((event.sequence, data.clone()))
                }
                EventKind::ContextCompaction(data) => {
                    commands.retain(|command| Some(*command) != data.command);
                    starts.retain(|(sequence, _)| Some(*sequence) != data.start);
                }
                EventKind::InputApplied(data) => {
                    self.pending.lock().unwrap().push(Applied::new(event, data))
                }
                EventKind::InputConsumed(data) => self
                    .pending
                    .lock()
                    .unwrap()
                    .retain(|input| input.id != data.input_id),
                EventKind::StopInterrupt(data) => {
                    self.interrupted_at.store(event.sequence, Ordering::Release);
                    if let Some(turn) = slot.as_mut() {
                        turn.interrupted = Some(data.reason.clone());
                    }
                }
                EventKind::TurnStart(data) => {
                    *slot = Some(Turn {
                        id: data.turn_id.clone(),
                        open: true,
                        request: None,
                        ending: false,
                        interrupted: None,
                        failed: None,
                    });
                }
                EventKind::TurnEnd(_) => *slot = None,
                _ => {
                    let Some(turn) = slot.as_mut() else { continue };
                    match &event.kind {
                        EventKind::ModelStart(data) => {
                            turn.request = Some(Request {
                                id: data.request_id.clone(),
                                stage: Stage::Model,
                                calls: Vec::new(),
                                call_event_id: None,
                                source: None,
                                preparation: None,
                                started: Vec::new(),
                                replaced: false,
                            });
                        }
                        EventKind::ContextPrepare(data) => {
                            if let Some(request) = turn.request.as_mut() {
                                request.source = Some(data.clone());
                            }
                        }
                        EventKind::ToolUseStart(data) => {
                            if let Some(request) = turn.request.as_mut() {
                                request.started.push(data.call_id.clone());
                            }
                        }
                        EventKind::ToolUse(data) => {
                            if let Some(request) = turn.request.as_mut() {
                                request.calls.push(data.call.clone());
                            }
                        }
                        EventKind::ModelOutputEnd(data) => match data.outcome {
                            ModelOutputOutcome::Completed { .. } => {
                                if let Some(request) = turn.request.as_mut() {
                                    request.stage = Stage::Tools;
                                }
                            }
                            _ => turn.request = None,
                        },
                        EventKind::ToolCall(_) => {
                            if let Some(request) = turn.request.as_mut() {
                                request.call_event_id = Some(event.id.clone());
                            }
                        }
                        // Results without a dispatch close calls; they settle
                        // no batch (ADR agent-029).
                        EventKind::ToolResult(data) if data.call_event_id.is_some() => {
                            turn.request = None
                        }
                        _ => {}
                    }
                }
            }
        }
        for (start, data) in starts {
            let command = data.command.expect("a manual start");
            commands.retain(|unsettled| *unsettled != command);
            self.timeline.append(
                EventKind::ContextCompaction(ContextCompactionData {
                    turn_id: data.turn_id,
                    request_id: None,
                    command: Some(command),
                    start: Some(start),
                    through: data.through,
                    outcome: ContextCompactionResult::Cancelled {
                        reason: CAPABILITY_RELOADING.into(),
                    },
                })
                .into(),
            )?;
        }
        for command in commands {
            self.timeline.append(
                EventKind::ContextCompaction(ContextCompactionData {
                    turn_id: None,
                    request_id: None,
                    command: Some(command),
                    start: None,
                    through: command,
                    outcome: ContextCompactionResult::Cancelled {
                        reason: CAPABILITY_RELOADING.into(),
                    },
                })
                .into(),
            )?;
        }
        self.publish(slot);
        let Some(turn) = slot.as_mut() else {
            return self.try_start(slot);
        };
        turn.failed = Some(CAPABILITY_RELOADING.into());
        if let Some(request) = turn
            .request
            .as_ref()
            .filter(|request| request.stage == Stage::Model && request.source.is_none())
        {
            self.timeline.append(
                EventKind::ModelOutputEnd(ModelOutputEndData {
                    turn_id: turn.id.clone(),
                    request_id: request.id.clone(),
                    outcome: ModelOutputOutcome::Failed {
                        error: CAPABILITY_RELOADING.into(),
                        budget: None,
                        presentation: None,
                    },
                })
                .into(),
            )?;
            return Ok(());
        }
        let outstanding = turn.request.as_ref().is_some_and(|request| {
            request.stage == Stage::Model
                || (request.stage == Stage::Tools && request.call_event_id.is_some())
        });
        if !outstanding {
            self.end(
                turn,
                TurnOutcome::Failed {
                    error: CAPABILITY_RELOADING.into(),
                },
            )?;
        }
        Ok(())
    }
    fn start_preparation(
        self: &Arc<Self>,
        through: u64,
        turn_id: String,
        request_id: String,
        recovery: Option<ContextPrepareData>,
    ) -> CancellationToken {
        let cancel = self.lifecycle.child_token();
        let result_cancel = cancel.clone();
        let owner = self.clone();
        self.tasks.spawn(async move {
            let work = AssertUnwindSafe(async {
                if let Some(mut source) = recovery {
                    owner
                        .binding
                        .transfer(&source.request_id, &request_id)
                        .map_err(AgentError::InvalidInput)?;
                    source.turn_id = turn_id.clone();
                    source.request_id = request_id.clone();
                    return Ok(source);
                }
                let (prefix, entries) = owner
                    .history
                    .rendered_context(&owner.timeline, through)
                    .await?;
                let configuration = context_configuration(&prefix);
                let prompt = PromptSnapshot::from_context(&prefix).revision;
                let history = context_messages(&prefix, &entries);
                let settings = match &configuration {
                    Some(data) => crate::host::ModelSettings {
                        provider_id: data.settings.provider_id.clone(),
                        model_id: data.settings.model_id.clone(),
                        capability_snapshot: data.settings.capability_snapshot.clone(),
                        tools: Some(match &data.settings.tools {
                            Some(tools) => {
                                owner
                                    .binding
                                    .bind(&request_id, through, tools, &data.settings.tool_sources)
                                    .map_err(AgentError::InvalidInput)?;
                                tools.clone()
                            }
                            None => owner
                                .binding
                                .bind_current(&request_id, None)
                                .map_err(AgentError::InvalidInput)?,
                        }),
                        system: data.system.clone(),
                        config: data.settings.config.clone(),
                        metadata: data.settings.metadata.clone(),
                    },
                    None => {
                        let mut settings = owner
                            .binding
                            .prepare(&request_id, |tools| {
                                owner
                                    .prepare
                                    .0
                                    .prepare(&turn_id, &request_id, &history, tools)
                                    .map_err(|error| AgentError::fault("Loop", error.to_string()))
                            })?;
                        settings.system =
                            Some(owner.guide.compose_system(&owner.branch, settings.system));
                        settings
                    }
                };
                Ok(ContextPrepareData {
                    turn_id: turn_id.clone(),
                    request_id: request_id.clone(),
                    through,
                    request: Arc::new(settings.request(history)),
                    configuration: configuration.map(|data| PromptReference {
                        timeline: data.timeline,
                        revision: data.revision,
                    }),
                    prompt,
                    recovery: None,
                })
            }).catch_unwind();
            let result = tokio::select! {
                biased;
                _ = cancel.cancelled() => Ok(Err(AgentError::Cancelled("Context read cancelled".into()))),
                result = work => result,
            };
            let result: Result<ContextPrepareData> = match result {
                Ok(result) => result,
                Err(panic) => {
                    owner.dispatcher.fail(AgentError::fault(
                        "Loop",
                        dim_agent_runtime::panic_message(panic),
                    ));
                    return;
                }
            };
            let submitted = (|| {
                let mut slot = owner.turn.lock().unwrap();
                let Some(turn) = slot
                    .as_mut()
                    .filter(|turn| turn.id == turn_id && !turn.ending)
                else {
                    return Ok(());
                };
                let Some(request) = turn
                    .request
                    .as_mut()
                    .filter(|request| request.id == request_id)
                else {
                    return Ok(());
                };
                let reason = turn
                    .interrupted
                    .clone()
                    .or_else(|| {
                        owner
                            .lifecycle
                            .is_cancelled()
                            .then(|| CAPABILITY_RELOADING.to_owned())
                    })
                    .or_else(|| request.replaced.then(|| REPLACED_BY_STEER.to_owned()));
                request.preparation = None;
                let kind = match reason {
                    Some(reason) => EventKind::ModelOutputEnd(ModelOutputEndData {
                        turn_id,
                        request_id,
                        outcome: ModelOutputOutcome::Cancelled { reason },
                    }),
                    None => match result {
                        Ok(source) => {
                            request.source = Some(source.clone());
                            EventKind::ContextPrepare(source)
                        }
                        Err(error) => EventKind::ModelOutputEnd(ModelOutputEndData {
                            turn_id,
                            request_id,
                            outcome: ModelOutputOutcome::Failed {
                                error: error.to_string(),
                                budget: None,
                                presentation: None,
                            },
                        }),
                    },
                };
                owner.timeline.append(kind.into())?;
                Ok(())
            })();
            if let Err(error) = submitted {
                owner.dispatcher.fail(error);
            }
        });
        result_cancel
    }
    fn on_event(self: &Arc<Self>, event: Arc<AgentEvent>) -> Result<()> {
        let mut slot = self.turn.lock().unwrap();
        match &event.kind {
            EventKind::InputApplied(data) => {
                let input = Applied::new(&event, data);
                let drives = input.mode != InputMode::Append;
                self.pending.lock().unwrap().push(input);
                match slot.as_mut() {
                    None => return self.try_start(&mut slot),
                    Some(turn) if drives => self.steer(turn),
                    Some(_) => {}
                }
                return Ok(());
            }
            EventKind::InputConsumed(data) => {
                self.pending
                    .lock()
                    .unwrap()
                    .retain(|input| input.id != data.input_id);
                self.claimed.lock().unwrap().remove(&data.input_id);
                return Ok(());
            }
            EventKind::StopInterrupt(data) => {
                self.interrupted_at.store(event.sequence, Ordering::Release);
                {
                    let mut compaction = self.compaction.lock().unwrap();
                    match compaction.command.as_ref() {
                        Some(Command {
                            running: Some(cancel),
                            ..
                        }) => {
                            cancel.cancel();
                            compaction.stop_reason = Some(data.reason.clone());
                        }
                        Some(Command {
                            sequence,
                            consumed: true,
                            ..
                        }) => {
                            self.timeline.append(
                                EventKind::ContextCompaction(ContextCompactionData {
                                    turn_id: slot.as_ref().map(|turn| turn.id.clone()),
                                    request_id: None,
                                    command: Some(*sequence),
                                    start: None,
                                    through: event.sequence,
                                    outcome: ContextCompactionResult::Cancelled {
                                        reason: data.reason.clone(),
                                    },
                                })
                                .into(),
                            )?;
                            compaction.command = None;
                        }
                        _ => {}
                    }
                }
                if let Some(turn) = slot.as_mut() {
                    turn.interrupted = Some(data.reason.clone());
                    if let Some(cancel) = turn
                        .request
                        .as_ref()
                        .and_then(|request| request.preparation.as_ref())
                    {
                        cancel.cancel();
                    }
                }
                self.publish(&slot);
                return Ok(());
            }
            EventKind::TurnStart(data) => {
                let turn = slot
                    .as_mut()
                    .filter(|turn| {
                        turn.id == data.turn_id && turn.request.is_none() && !turn.ending
                    })
                    .ok_or_else(|| AgentError::fault("Loop", "invalid turn/start"))?;
                turn.open = true;
                let result = self.next_model(turn, None, event.sequence);
                self.publish(&slot);
                return result;
            }
            EventKind::TurnEnd(data) => {
                if !slot
                    .as_ref()
                    .is_some_and(|turn| turn.id == data.turn_id && turn.ending)
                {
                    return Err(AgentError::fault("Loop", "unexpected turn/end"));
                }
                *slot = None;
                self.binding.retain_only(None);
                self.publish(&slot);
                let mut compaction = self.compaction.lock().unwrap();
                if compaction.waiting() {
                    self.start_step(&mut compaction, None, event.sequence);
                    return Ok(());
                }
                drop(compaction);
                return self.try_start(&mut slot);
            }
            EventKind::ContextCompact(_) => {
                let mut compaction = self.compaction.lock().unwrap();
                match compaction.command.as_mut() {
                    Some(command) if command.sequence == event.sequence => command.consumed = true,
                    Some(_) => {
                        self.timeline.append(
                            EventKind::ContextCompaction(ContextCompactionData {
                                turn_id: slot.as_ref().map(|turn| turn.id.clone()),
                                request_id: None,
                                command: Some(event.sequence),
                                start: None,
                                through: event.sequence,
                                outcome: ContextCompactionResult::Skipped {
                                    reason: ALREADY_IN_PROGRESS.into(),
                                    diagnostics: Box::new(CompactionDiagnosticsRecord {
                                        reason: CompactionReason::Manual,
                                        trigger: CompactionTrigger::Manual,
                                        retain_from: None,
                                        retain_limit: None,
                                        estimated_input_tokens: 0.,
                                        estimated_output_tokens: None,
                                        entries_replaced: 0,
                                        model_call_count: 0,
                                        duration_ms: 0,
                                        model_usage: None,
                                        insufficient_space: None,
                                        strategy: CompactionStrategyRecord {
                                            kind: self.policy.as_ref().map_or_else(
                                                || "none".to_owned(),
                                                |policy| policy.strategy.name().to_owned(),
                                            ),
                                            details: JsonObject::new(),
                                        },
                                    }),
                                },
                            })
                            .into(),
                        )?;
                        return Ok(());
                    }
                    None => {
                        compaction.command = Some(Command {
                            sequence: event.sequence,
                            consumed: true,
                            running: None,
                        })
                    }
                }
                if slot
                    .as_ref()
                    .is_none_or(|turn| turn.request.is_none() && !turn.ending)
                {
                    let turn_id = slot.as_ref().map(|turn| turn.id.clone());
                    self.start_step(&mut compaction, turn_id, event.sequence);
                }
                return Ok(());
            }
            EventKind::ContextCompaction(data) => {
                let mut compaction = self.compaction.lock().unwrap();
                let Some(command) = compaction
                    .command
                    .take_if(|command| Some(command.sequence) == data.command)
                else {
                    return Ok(());
                };
                drop(compaction);
                if command.running.is_none() {
                    return Ok(());
                }
                return match slot.as_mut() {
                    Some(turn) if turn.open && !turn.ending => {
                        self.next_model(turn, None, event.sequence)
                    }
                    Some(_) => Ok(()),
                    None => self.try_start(&mut slot),
                };
            }
            // Results closing calls that were never dispatched settle no batch
            // (ADR agent-029); Loop writes some of them with its turn end.
            EventKind::ToolResult(data) if data.call_event_id.is_none() => return Ok(()),
            _ => {}
        }
        let turn = slot
            .as_mut()
            .ok_or_else(|| AgentError::fault("Loop", "step outside a turn"))?;
        let request = turn
            .request
            .as_mut()
            .ok_or_else(|| AgentError::fault("Loop", "step outside a request"))?;
        let (turn_id, request_id) = match &event.kind {
            EventKind::ModelStart(d) => (&d.turn_id, &d.request_id),
            EventKind::ToolUseStart(d) => (&d.turn_id, &d.request_id),
            EventKind::ToolUse(d) => (&d.turn_id, &d.request_id),
            EventKind::ModelOutputEnd(d) => (&d.turn_id, &d.request_id),
            EventKind::ToolStart(d) => (&d.turn_id, &d.request_id),
            EventKind::ToolCompleted(d) => (&d.turn_id, &d.request_id),
            EventKind::ToolResult(d) => (&d.turn_id, &d.request_id),
            _ => return Ok(()),
        };
        if turn.ending || turn.id != *turn_id || request.id != *request_id {
            return Err(AgentError::fault(
                "Loop",
                "event does not belong to the active request",
            ));
        }
        match &event.kind {
            EventKind::ModelStart(_) => {
                if request.stage != Stage::Preparing {
                    return Err(AgentError::fault("Loop", "duplicate model-start"));
                }
                request.stage = Stage::Model;
                let reason = turn
                    .interrupted
                    .clone()
                    .or_else(|| request.replaced.then(|| REPLACED_BY_STEER.to_owned()));
                if let Some(reason) = reason {
                    self.timeline.append(
                        EventKind::ModelOutputEnd(ModelOutputEndData {
                            turn_id: turn.id.clone(),
                            request_id: request.id.clone(),
                            outcome: ModelOutputOutcome::Cancelled { reason },
                        })
                        .into(),
                    )?;
                    return Ok(());
                }
                request.preparation = Some(self.start_preparation(
                    event.sequence,
                    turn.id.clone(),
                    request.id.clone(),
                    request.source.clone(),
                ));
            }
            EventKind::ToolUseStart(data) if request.stage == Stage::Model => {
                request.started.push(data.call_id.clone())
            }
            EventKind::ToolUse(data) if request.stage == Stage::Model => {
                request.calls.push(data.call.clone())
            }
            EventKind::ModelOutputEnd(data) if request.stage == Stage::Model => {
                match &data.outcome {
                    ModelOutputOutcome::Failed { error, budget, .. } => {
                        if self.policy.is_some()
                            && budget.is_some()
                            && request
                                .source
                                .as_ref()
                                .is_some_and(|s| s.recovery.is_none())
                        {
                            let mut source = request.source.clone().expect("checked source");
                            source.recovery = budget.clone();
                            self.next_model(turn, Some(source), event.sequence)?;
                        } else {
                            self.end(
                                turn,
                                TurnOutcome::Failed {
                                    error: error.clone(),
                                },
                            )?;
                        }
                    }
                    ModelOutputOutcome::Cancelled { reason } => {
                        if reason != CAPABILITY_RELOADING
                            && turn.interrupted.is_none()
                            && self.driven()
                        {
                            self.binding.release(&request.id);
                            self.next_model(turn, None, event.sequence)?;
                        } else {
                            if reason == CAPABILITY_RELOADING {
                                turn.failed = Some(CAPABILITY_RELOADING.into());
                            }
                            let reason = turn.interrupted.clone().unwrap_or(reason.clone());
                            self.end(turn, TurnOutcome::Cancelled { reason })?
                        }
                    }
                    ModelOutputOutcome::Completed { finish_reason } => {
                        request.stage = Stage::Tools;
                        if let Some(error) = turn.failed.clone() {
                            self.end(turn, TurnOutcome::Failed { error })?;
                        } else if let Some(reason) = turn.interrupted.clone() {
                            self.end(turn, TurnOutcome::Cancelled { reason })?;
                        } else if !request.calls.is_empty() {
                            let event = self.timeline.append(
                                EventKind::ToolCall(ToolCallData {
                                    turn_id: turn.id.clone(),
                                    request_id: request.id.clone(),
                                    calls: request.calls.clone(),
                                })
                                .into(),
                            )?;
                            request.call_event_id = Some(event.id.clone());
                        } else if self.driven() {
                            self.binding.release(&request.id);
                            self.next_model(turn, None, event.sequence)?;
                        } else {
                            let reason = match finish_reason {
                                FinishReason::EndTurn => "end_turn",
                                FinishReason::ToolUse => "tool_use",
                                FinishReason::StopSequence => "stop_sequence",
                            };
                            self.end(
                                turn,
                                TurnOutcome::Completed {
                                    reason: reason.into(),
                                },
                            )?;
                        }
                    }
                }
            }
            EventKind::ToolStart(data)
                if request.stage == Stage::Tools
                    && request.call_event_id.as_ref() == Some(&data.call_event_id) => {}
            EventKind::ToolCompleted(data)
                if request.stage == Stage::Tools
                    && request.call_event_id.as_ref() == Some(&data.call_event_id) => {}
            EventKind::ToolResult(data)
                if request.stage == Stage::Tools
                    && data.call_event_id.is_some()
                    && request.call_event_id == data.call_event_id =>
            {
                if data
                    .results
                    .iter()
                    .any(|item| crate::tool_executor::is_reload_settled(&item.result))
                {
                    turn.failed = Some(CAPABILITY_RELOADING.into());
                }
                self.binding.release(&request.id);
                self.next_model(turn, None, event.sequence)?;
            }
            _ => {
                return Err(AgentError::fault(
                    "Loop",
                    format!("unexpected {} in request stage", event.kind.key()),
                ));
            }
        }
        Ok(())
    }
}
impl LoopState for AgentLoop {
    fn state(&self) -> LoopSnapshot {
        self.observable.get()
    }
    fn watch(&self) -> tokio::sync::watch::Receiver<LoopSnapshot> {
        self.observable.subscribe()
    }
}

pub struct LoopPlugin {
    state: Key<StateCell<LoopSnapshot>>,
    service: Key<AgentLoop>,
    loop_state: Key<dyn LoopState>,
    timeline: Key<Timeline>,
    dispatcher: Key<Dispatcher>,
    prepare: Key<PrepareModelHost>,
    binding: Key<ToolBinding>,
    history: Key<HistoryRenderer>,
    context: Key<ContextModelHost>,
    strategy: Key<CompactionStrategyHost>,
    guide: Key<crate::prompt_guide::GuideEntries>,
    identity: Key<TimelineIdentity>,
}
impl LoopPlugin {
    pub fn new(names: &Names<'_>) -> rutis::Result<Self> {
        Ok(Self {
            state: names.state_cell()?,
            service: names.key()?,
            loop_state: names.key()?,
            timeline: names.key()?,
            dispatcher: names.key()?,
            prepare: names.key()?,
            binding: names.key()?,
            history: names.key()?,
            context: names.key()?,
            strategy: names.key()?,
            guide: names.key()?,
            identity: names.key()?,
        })
    }
}
impl DynPlugin for LoopPlugin {
    fn name(&self) -> &str {
        "Loop"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![
            self.timeline.erase(),
            self.dispatcher.erase(),
            self.prepare.erase(),
            self.binding.erase(),
            self.history.erase(),
            self.context.erase(),
            self.strategy.erase(),
            self.guide.erase(),
            self.identity.erase(),
        ]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let timeline = ctx.require_at(&self.timeline)?;
            let dispatcher = ctx.require_at(&self.dispatcher)?;
            let prepare = ctx.require_at(&self.prepare)?;
            let binding = ctx.require_at(&self.binding)?;
            let history = ctx.require_at(&self.history)?;
            let context = ctx.require_at(&self.context)?;
            let strategy = ctx.require_at(&self.strategy)?;
            let guide = ctx.require_at(&self.guide)?;
            let identity = ctx.require_at(&self.identity)?;
            let policy = ContextPolicy::bind(&timeline, &context, &strategy, &history)
                .map_err(|error| rutis::Error::from(ErrorKind::PluginFailed(error.into())))?;
            let service = Arc::new(AgentLoop {
                turn: Mutex::new(None),
                compaction: Mutex::new(CompactionSlot::default()),
                observable: Arc::new(StateCell::new(LoopSnapshot::default())),
                timeline,
                interrupted_at: AtomicU64::new(0),
                pending: Mutex::new(Vec::new()),
                claimed: Mutex::new(HashSet::new()),
                prepare,
                binding,
                history,
                guide,
                branch: identity.branch_id.clone(),
                policy,
                tasks: TaskTracker::new(),
                lifecycle: ctx.cancellation_token(),
                dispatcher: dispatcher.clone(),
            });
            let owner = service.clone();
            ctx.effect(move || async move {
                owner.lifecycle.cancel();
                owner.tasks.close();
                owner.tasks.wait().await;
                Ok(())
            })?;
            let owner = service.clone();
            dispatcher
                .follow(
                    ctx,
                    "Loop",
                    &[
                        "input/applied",
                        "input/consumed",
                        "stop/interrupt",
                        "turn/start",
                        "turn/end",
                        "step/model-start",
                        "step/tool-use-start",
                        "step/tool-use",
                        "step/model-output-end",
                        "step/tool-start",
                        "step/tool-completed",
                        "step/tool-result",
                        "context/compact",
                        "context/compaction",
                    ],
                    move |delivery| {
                        let result = match delivery {
                            Delivery::Replay(events) => {
                                let mut slot = owner.turn.lock().unwrap();
                                let rebuilt = owner.rebuild(&mut slot, &events);
                                if rebuilt.is_ok() {
                                    owner.binding.retain_only(
                                        slot.as_ref()
                                            .and_then(|turn| turn.request.as_ref())
                                            .map(|request| request.id.as_str()),
                                    );
                                }
                                rebuilt
                            }
                            Delivery::Live(event) => owner.on_event(event),
                        };
                        Box::pin(async { result })
                    },
                )
                .await?;
            ctx.provide_at(self.state.clone(), service.observable.clone())?;
            ctx.provide_at(self.service.clone(), service.clone())?;
            ctx.provide_at(self.loop_state.clone(), service as Arc<dyn LoopState>)?;
            Ok(Effect::Done)
        })
    }
}

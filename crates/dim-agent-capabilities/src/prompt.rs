use crate::{
    branch_input::append_own,
    context_block,
    context_preparation::{ConfigurationPublisher, ToolSetAdoption},
    host::{HostError, PromptHost},
    prompt_state::PromptSnapshot,
};
use dim_agent_runtime::{Capability, InBranch, InSession, KeysOf, scoped, *};
use dim_agent_types::*;
use futures_util::FutureExt;
use rutis::{BoxFuture, CordisError, Ctx, Effect, TypedPlugin};
use std::{
    collections::{BTreeMap, BTreeSet},
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{Arc, Mutex},
};
use tokio::sync::oneshot;
use tokio_util::{sync::CancellationToken, task::TaskTracker};
use uuid::Uuid;

type Key = (String, PromptScope);
type LoadedPrompts = Vec<(PromptSourceTarget, Option<PromptEntry>)>;

fn validate_entry(target: &PromptSourceTarget, entry: &PromptEntry) -> Result<()> {
    if entry.source != target.source || entry.scope != target.scope {
        return Err(AgentError::InvalidInput(format!(
            "prompt source {} returned a different identity",
            target.source
        )));
    }
    if entry.delivery == PromptDelivery::System
        && !matches!(entry.scope, PromptScope::Branch { .. })
    {
        return Err(AgentError::InvalidInput(
            "system prompts require branch scope".into(),
        ));
    }
    Ok(())
}

pub struct InitialPromptConfiguration(pub Option<ConfigureUpdate>);

/// Rules and state enter the model context as inputs written with the change.
fn context_rules(before: &PromptSnapshot, after: &PromptSnapshot) -> Vec<InputBlock> {
    context_changes(before, after, false)
}

/// Result of [`Prompt::adopt_tools`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ToolAdoptOutcome {
    /// Committed as `context/configured` at this sequence.
    Adopted { sequence: u64 },
    /// The adopted configuration already declares this tool set.
    Unchanged,
    /// Cancelled before it was published.
    Superseded,
}

/// Work of the serial adoption queue.
enum Adoption {
    Event(Arc<AgentEvent>),
    /// `branch/start` or `branch/resume`: deliver the sources' current
    /// content before the branch takes input.
    Open {
        event: Arc<AgentEvent>,
        reply: oneshot::Sender<Result<()>>,
    },
    Tools {
        tools: ToolSetAdoption,
        cancel: CancellationToken,
        reply: oneshot::Sender<Result<ToolAdoptOutcome>>,
    },
}

struct Pending {
    update: ConfigureUpdate,
    cancel: CancellationToken,
    started: bool,
    loaded: Option<Result<LoadedPrompts>>,
    waiting_through: Option<u64>,
    reply: Option<oneshot::Sender<Result<ConfigureOutcome>>>,
}

#[derive(Default)]
struct PromptState {
    closed: bool,
    owners: BTreeMap<Key, String>,
    configuration_owner: Option<String>,
    pending: BTreeMap<String, Pending>,
}

pub struct Prompt {
    host: Arc<PromptHost>,
    /// Sources the Session delivers, and the model a new Session adopts.
    initial: Option<ConfigureUpdate>,
    publisher: Arc<ConfigurationPublisher>,
    timeline: Arc<Timeline>,
    faults: Arc<FaultReporter>,
    identity: Arc<TimelineIdentity>,
    lifecycle: CancellationToken,
    branch_phase: Arc<BranchLifecycle>,
    opening: Arc<BranchOpening>,
    opened: tokio::sync::watch::Sender<Option<Result<()>>>,
    tasks: TaskTracker,
    state: Mutex<PromptState>,
    adoption: tokio::sync::mpsc::UnboundedSender<Adoption>,
}

impl Prompt {
    fn validate_update(update: &ConfigureUpdate) -> Result<()> {
        let mut seen = BTreeSet::new();
        for target in &update.sources {
            if !seen.insert((target.source.clone(), target.scope.clone())) {
                return Err(AgentError::InvalidInput(format!(
                    "duplicate prompt target: {}",
                    target.source
                )));
            }
        }
        Ok(())
    }

    fn supersede(state: &mut PromptState, id: &str) {
        if state
            .pending
            .get(id)
            .is_some_and(|pending| pending.waiting_through.is_some())
        {
            return;
        }
        if let Some(pending) = state.pending.get(id) {
            pending.cancel.cancel();
            state.owners.retain(|_, owner| owner != id);
            if state.configuration_owner.as_deref() == Some(id) {
                state.configuration_owner = None;
            }
        }
    }

    fn register(
        &self,
        id: String,
        update: ConfigureUpdate,
        loaded: Option<LoadedPrompts>,
        reply: Option<oneshot::Sender<Result<ConfigureOutcome>>>,
    ) -> Result<()> {
        Self::validate_update(&update)?;
        let mut state = self.state.lock().unwrap();
        if state.closed {
            return Err(AgentError::Closed("Prompt".into()));
        }
        if state.pending.contains_key(&id) {
            return Err(AgentError::InvalidInput(
                "prompt operation is already registered".into(),
            ));
        }
        state.pending.insert(
            id,
            Pending {
                update,
                cancel: self.lifecycle.child_token(),
                started: false,
                loaded: loaded.map(Ok),
                waiting_through: None,
                reply,
            },
        );
        Ok(())
    }

    fn finish(&self, id: &str, result: Result<ConfigureOutcome>) {
        let mut state = self.state.lock().unwrap();
        if let Some(mut pending) = state.pending.remove(id) {
            state.owners.retain(|_, owner| owner != id);
            if state.configuration_owner.as_deref() == Some(id) {
                state.configuration_owner = None;
            }
            if let Some(reply) = pending.reply.take() {
                let _ = reply.send(result);
            }
        }
    }

    async fn load(
        &self,
        update: &ConfigureUpdate,
        cancel: CancellationToken,
    ) -> Result<LoadedPrompts> {
        let mut loaded = Vec::with_capacity(update.sources.len());
        for target in &update.sources {
            if cancel.is_cancelled() {
                return Ok(loaded);
            }
            let span = tracing::info_span!(
                "prompt.load",
                session_id = %self.identity.session_id,
                branch_id = %self.identity.branch_id,
                prompt_source = %target.source,
                prompt_scope = match target.scope {
                    PromptScope::Branch { .. } => "branch",
                    PromptScope::Turn { .. } => "turn",
                    PromptScope::Task { .. } => "task",
                },
                prompt_version = tracing::field::Empty,
                outcome = tracing::field::Empty,
            );
            let defect = |panic| {
                span.record("outcome", "defect");
                self.report_panic(panic)
            };
            let future = catch_unwind(AssertUnwindSafe(|| {
                self.host.0.load(target.clone(), cancel.clone())
            }))
            .map_err(defect)?;
            // Cancellation only signals the host. Once constructed, its future is always joined.
            let value = AssertUnwindSafe(future)
                .catch_unwind()
                .await
                .map_err(defect)?;
            span.record(
                "outcome",
                match &value {
                    Ok(_) if !cancel.is_cancelled() => "returned",
                    Ok(_) | Err(HostError::Cancelled(_)) => "interrupted",
                    Err(_) => "failure",
                },
            );
            if let Ok(Some(entry)) = &value {
                span.record("prompt_version", entry.version.as_str());
            }
            let value = match value {
                Err(HostError::Cancelled(_)) if cancel.is_cancelled() => return Ok(loaded),
                Err(other) => {
                    return Err(AgentError::InvalidInput(format!(
                        "prompt source {} failed: {other}",
                        target.source
                    )));
                }
                Ok(value) => value,
            };
            if let Some(entry) = &value {
                validate_entry(target, entry)?;
            }
            loaded.push((target.clone(), value));
        }
        Ok(loaded)
    }

    fn report_panic(&self, panic: Box<dyn std::any::Any + Send>) -> AgentError {
        let error = AgentError::fault("Prompt", panic_message(panic));
        self.faults.report(error.clone());
        error
    }

    fn start(self: &Arc<Self>, id: &str) -> Result<()> {
        let (update, cancel, loaded) = {
            let mut state = self.state.lock().unwrap();
            let Some(pending) = state.pending.get(id) else {
                return Ok(());
            };
            let update = pending.update.clone();
            let cancel = pending.cancel.clone();
            let mut replaced = BTreeSet::new();
            for target in &update.sources {
                if let Some(previous) = state
                    .owners
                    .get(&(target.source.clone(), target.scope.clone()))
                {
                    replaced.insert(previous.clone());
                }
            }
            if update.model.is_some()
                && let Some(previous) = &state.configuration_owner
            {
                replaced.insert(previous.clone());
            }
            for previous in replaced {
                Self::supersede(&mut state, &previous);
            }
            for target in &update.sources {
                state
                    .owners
                    .insert((target.source.clone(), target.scope.clone()), id.to_owned());
            }
            if update.model.is_some() {
                state.configuration_owner = Some(id.to_owned());
            }
            let pending = state.pending.get_mut(id).unwrap();
            if std::mem::replace(&mut pending.started, true) {
                return Err(AgentError::fault(
                    "Prompt",
                    "configure request consumed twice",
                ));
            }
            (update, cancel, pending.loaded.take())
        };
        let owner = self.clone();
        let id = id.to_owned();
        self.tasks.spawn(async move {
            // A started load settles through its own cancellation.
            let result = if cancel.is_cancelled() {
                Err(AgentError::Cancelled("prompt load".into()))
            } else if let Some(loaded) = loaded {
                loaded
            } else {
                owner.load(&update, cancel.clone()).await
            };
            {
                let mut state = owner.state.lock().unwrap();
                let Some(pending) = state.pending.get_mut(&id) else {
                    return;
                };
                pending.loaded = Some(result);
            }
            if let Err(error) = owner.timeline.append_registered(
                EventKind::PromptConfigureResult(PromptConfigureControlData {
                    operation_id: id.clone(),
                })
                .into(),
                |event| {
                    let EventKind::PromptConfigureResult(data) = &event.kind else {
                        unreachable!("before preserves event kind")
                    };
                    if data.operation_id != id {
                        let mut state = owner.state.lock().unwrap();
                        if state.pending.contains_key(&data.operation_id) {
                            return Err(AgentError::InvalidInput(
                                "prompt operation is already registered".into(),
                            ));
                        }
                        let pending = state.pending.remove(&id).expect("registered prompt load");
                        state.pending.insert(data.operation_id.clone(), pending);
                        for operation in state.owners.values_mut() {
                            if operation == &id {
                                *operation = data.operation_id.clone();
                            }
                        }
                        if state.configuration_owner.as_ref() == Some(&id) {
                            state.configuration_owner = Some(data.operation_id.clone());
                        }
                    }
                    Ok(())
                },
            ) {
                owner.finish(&id, Err(error));
            }
        });
        Ok(())
    }

    /// Adopts a tool set as the tool part of this branch's configuration, in
    /// the same serial queue as configure and prompt adoptions. Returns once
    /// `context/configured` is committed, without waiting for its dispatch:
    /// ToolBinding adopts while its branch applies, before dispatch starts.
    pub(crate) async fn adopt_tools(
        &self,
        tools: ToolSetAdoption,
        cancel: CancellationToken,
    ) -> Result<ToolAdoptOutcome> {
        let (reply, result) = oneshot::channel();
        self.adoption
            .send(Adoption::Tools {
                tools,
                cancel,
                reply,
            })
            .map_err(|_| AgentError::Closed("Prompt".into()))?;
        result
            .await
            .map_err(|_| AgentError::Closed("Prompt".into()))?
    }

    async fn publish_tools(
        &self,
        tools: ToolSetAdoption,
        cancel: &CancellationToken,
    ) -> Result<ToolAdoptOutcome> {
        if cancel.is_cancelled() {
            return Ok(ToolAdoptOutcome::Superseded);
        }
        let published = self
            .publisher
            .publish(
                Vec::new(),
                None,
                Some(tools),
                |_, _| Vec::new(),
                |_| {
                    if cancel.is_cancelled() {
                        Err(AgentError::Cancelled("tool adoption superseded".into()))
                    } else {
                        Ok(())
                    }
                },
            )
            .await;
        match published {
            Ok(Some(sequence)) => Ok(ToolAdoptOutcome::Adopted { sequence }),
            Ok(None) => Ok(ToolAdoptOutcome::Unchanged),
            Err(AgentError::Cancelled(_)) => Ok(ToolAdoptOutcome::Superseded),
            Err(error) => Err(error),
        }
    }

    fn schedule_adoption(self: &Arc<Self>, event: Arc<AgentEvent>) -> Result<()> {
        match self.adoption.send(Adoption::Event(event)) {
            Ok(()) => Ok(()),
            // Unloading: the adoption task is gone and its operations settle
            // through the disposer, not as a dispatch fault.
            Err(_) if self.lifecycle.is_cancelled() => Ok(()),
            Err(_) => Err(AgentError::Closed("Prompt".into())),
        }
    }

    async fn consume_result(&self, id: &str) -> Result<()> {
        let (update, loaded, valid) = {
            let mut state = self.state.lock().unwrap();
            let Some(pending) = state.pending.get_mut(id) else {
                return Ok(());
            };
            let (update, cancelled, loaded) = {
                let update = pending.update.clone();
                let cancelled = pending.cancel.is_cancelled();
                let loaded = pending.loaded.take().ok_or_else(|| {
                    AgentError::fault(
                        "Prompt",
                        "configure result consumed before loading completed",
                    )
                })?;
                (update, cancelled, loaded)
            };
            let valid = !state.closed
                && !cancelled
                && update.sources.iter().all(|target| {
                    state
                        .owners
                        .get(&(target.source.clone(), target.scope.clone()))
                        .is_some_and(|owner| owner == id)
                })
                && (update.model.is_none() || state.configuration_owner.as_deref() == Some(id));
            (update, loaded, valid)
        };
        if !valid {
            self.finish(id, Ok(ConfigureOutcome::Superseded));
            return Ok(());
        }
        let loaded = match loaded {
            Ok(value) => value,
            Err(error) => {
                self.finish(id, Err(error));
                return Ok(());
            }
        };
        if loaded.len() != update.sources.len() {
            self.finish(id, Ok(ConfigureOutcome::Superseded));
            return Ok(());
        }
        let context =
            match crate::history::read_context(&self.timeline, self.timeline.sequence()).await {
                Ok(context) => context,
                Err(error) => {
                    self.finish(id, Err(error));
                    return Ok(());
                }
            };
        let current = PromptSnapshot::from_context(&context);
        if update
            .sources
            .iter()
            .any(|target| !current.active(&target.scope))
        {
            self.finish(id, Ok(ConfigureOutcome::Superseded));
            return Ok(());
        }
        let mut changes = Vec::new();
        for (target, value) in loaded {
            let key = (target.source.clone(), target.scope.clone());
            match value {
                Some(entry) if current.entries.get(&key) != Some(&entry) => {
                    changes.push(PromptChange::Set { entry });
                }
                None if current.entries.contains_key(&key) => changes.push(PromptChange::Revoke {
                    source: target.source,
                    scope: target.scope,
                }),
                _ => {}
            }
        }
        match self
            .publisher
            .publish(changes, update.model, None, context_rules, |events| {
                let mut state = self.state.lock().unwrap();
                let valid = !state.closed
                    && state
                        .pending
                        .get(id)
                        .is_some_and(|pending| !pending.cancel.is_cancelled());
                if !valid {
                    return Err(AgentError::Cancelled("prompt adoption superseded".into()));
                }
                state
                    .pending
                    .get_mut(id)
                    .expect("valid pending adoption")
                    // Settled once this Prompt has consumed its own
                    // events; the rule input is Loop's to consume.
                    .waiting_through = events
                    .iter()
                    .rev()
                    .find(|event| !matches!(event.kind, EventKind::InputMessage(_)))
                    .map(|event| event.sequence);
                Ok(())
            })
            .await
        {
            Ok(Some(_)) => {}
            Ok(None) => self.finish(id, Ok(ConfigureOutcome::Unchanged)),
            Err(AgentError::Cancelled(_)) => self.finish(id, Ok(ConfigureOutcome::Superseded)),
            Err(error) => self.finish(id, Err(error)),
        }
        Ok(())
    }

    fn settle_consumed(&self, sequence: u64) {
        let ids: Vec<String> = self
            .state
            .lock()
            .unwrap()
            .pending
            .iter()
            .filter(|(_, pending)| {
                pending
                    .waiting_through
                    .is_some_and(|through| sequence >= through)
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in ids {
            self.finish(&id, Ok(ConfigureOutcome::Adopted));
        }
    }

    /// Adopts frozen source values and an optional model through the existing
    /// configure command. Each entry has its target's source and scope.
    pub async fn configure_loaded(
        self: Arc<Self>,
        model: Option<ModelSelection>,
        sources: Vec<(PromptSourceTarget, Option<PromptEntry>)>,
    ) -> Result<ConfigureOutcome> {
        for (target, entry) in &sources {
            if let Some(entry) = entry {
                validate_entry(target, entry)?;
            }
        }
        let update = ConfigureUpdate {
            sources: sources.iter().map(|(target, _)| target.clone()).collect(),
            model,
        };
        self.submit(update, Some(sources)).await
    }

    async fn submit(
        &self,
        update: ConfigureUpdate,
        loaded: Option<LoadedPrompts>,
    ) -> Result<ConfigureOutcome> {
        let id = Uuid::new_v4().to_string();
        let (send, receive) = oneshot::channel();
        self.timeline.append_registered(
            EventKind::PromptConfigureRequest(PromptConfigureControlData { operation_id: id })
                .into(),
            |event| {
                let EventKind::PromptConfigureRequest(data) = &event.kind else {
                    unreachable!("before preserves event kind")
                };
                self.register(data.operation_id.clone(), update, loaded, Some(send))
            },
        )?;
        receive
            .await
            .map_err(|_| AgentError::fault("Prompt", "configure operation ended without result"))?
    }
}

impl ConfigureControl for Prompt {
    fn configure(
        self: Arc<Self>,
        update: ConfigureUpdate,
    ) -> BoxFuture<'static, Result<ConfigureOutcome>> {
        Box::pin(async move { self.submit(update, None).await })
    }
}

pub struct PromptPlugin {
    scope: BranchScope,
}
impl PromptPlugin {
    pub fn new(scope: &BranchScope) -> Self {
        Self { scope: *scope }
    }
}
impl TypedPlugin for PromptPlugin {
    type Deps = (
        InBranch<Timeline>,
        InBranch<Dispatcher>,
        InSession<PromptHost>,
        InSession<InitialPromptConfiguration>,
        InBranch<ConfigurationPublisher>,
        InBranch<TimelineIdentity>,
        InSession<FaultReporter>,
        (InBranch<BranchLifecycle>, InBranch<BranchOpening>),
    );
    fn name(&self) -> &str {
        "Prompt"
    }
    fn apply<'a>(
        &'a self,
        ctx: &'a Ctx,
        (
            InBranch(timeline),
            InBranch(dispatcher),
            InSession(host),
            InSession(initial),
            InBranch(publisher),
            InBranch(identity),
            InSession(faults),
            (InBranch(branch_phase), InBranch(opening)),
        ): Self::Deps,
    ) -> BoxFuture<'a, std::result::Result<Effect, CordisError>> {
        Box::pin(async move {
            if ctx.cancellation_token().is_cancelled() {
                return Ok(Effect::Done);
            }
            let (adoption, mut adoptions) = tokio::sync::mpsc::unbounded_channel();
            let service = Arc::new(Prompt {
                host,
                initial: initial.0.clone(),
                publisher,
                timeline: timeline.clone(),
                faults,
                identity,
                lifecycle: ctx.cancellation_token(),
                branch_phase,
                opening,
                opened: tokio::sync::watch::channel(None).0,
                tasks: TaskTracker::new(),
                state: Mutex::new(PromptState::default()),
                adoption,
            });

            let owner = service.clone();
            ctx.effect(move || {
                Effect::AsyncDisposer(Box::new(move || {
                    Box::pin(async move {
                        {
                            let mut state = owner.state.lock().unwrap();
                            state.closed = true;
                            for pending in state.pending.values() {
                                pending.cancel.cancel();
                            }
                        }
                        owner.tasks.close();
                        owner.tasks.wait().await;
                        let ids = owner
                            .state
                            .lock()
                            .unwrap()
                            .pending
                            .keys()
                            .cloned()
                            .collect::<Vec<_>>();
                        for id in ids {
                            owner.finish(&id, Err(AgentError::Closed("Prompt".into())));
                        }
                        Ok(())
                    })
                }))
            })?;

            let owner = service.clone();
            service.tasks.spawn(async move {
                loop {
                    let item = tokio::select! {
                        biased;
                        _ = owner.lifecycle.cancelled() => break,
                        item = adoptions.recv() => item,
                    };
                    let event = match item {
                        None => break,
                        Some(Adoption::Event(event)) => event,
                        Some(Adoption::Open { event, reply }) => {
                            let result = AssertUnwindSafe(owner.open(&event))
                                .catch_unwind()
                                .await
                                .unwrap_or_else(|panic| {
                                    Err(AgentError::fault("Prompt", panic_message(panic)))
                                });
                            owner.opened.send_replace(Some(result.clone()));
                            let _ = reply.send(result);
                            continue;
                        }
                        Some(Adoption::Tools {
                            tools,
                            cancel,
                            reply,
                        }) => {
                            let result = AssertUnwindSafe(owner.publish_tools(tools, &cancel))
                                .catch_unwind()
                                .await
                                .unwrap_or_else(|panic| {
                                    Err(AgentError::fault("Prompt", panic_message(panic)))
                                });
                            let _ = reply.send(result);
                            continue;
                        }
                    };
                    let result = AssertUnwindSafe(async {
                        match &event.kind {
                            EventKind::PromptConfigureRequest(data) => {
                                owner.start(&data.operation_id)
                            }
                            EventKind::PromptConfigureResult(data) => {
                                owner.consume_result(&data.operation_id).await
                            }
                            _ => unreachable!("only adoption events are queued"),
                        }
                    })
                    .catch_unwind()
                    .await
                    .unwrap_or_else(|panic| Err(AgentError::fault("Prompt", panic_message(panic))));
                    if let Err(error) = &result {
                        // A cold-read failure belongs to this adoption. The
                        // Timeline itself reports failed durable commits.
                        if !matches!(error, AgentError::Storage(_)) {
                            owner.faults.report(error.clone());
                        }
                    }
                }
            });

            // Subscribe, then read the snapshot under the gate: handlers skip
            // what the snapshot covers and miss nothing dispatched meanwhile.
            let state = Arc::new(StateCell::new(PromptSnapshot::default()));
            let gate = Arc::new(tokio::sync::Mutex::new(0u64));
            let mut projecting = gate.lock().await;
            let handler_gate = gate.clone();
            let observable = state.clone();
            let prompt = service.clone();
            dispatcher.subscribe(
                ctx,
                "Prompt",
                &[
                    "prompt/configure-request",
                    "prompt/configure-result",
                    "prompt/changed",
                    "context/configured",
                    "turn/start",
                    "turn/end",
                    "branch/start",
                    "branch/resume",
                    "context/scope",
                    "stop/interrupt",
                ],
                move |event| {
                    let observable = observable.clone();
                    let prompt = prompt.clone();
                    let gate = handler_gate.clone();
                    Box::pin(async move {
                        let projected_through = *gate.lock().await;
                        if event.sequence <= projected_through {
                            return Ok(());
                        }
                        match &event.kind {
                            EventKind::PromptConfigureRequest(_)
                            | EventKind::PromptConfigureResult(_) => {
                                prompt.schedule_adoption(event.clone())?;
                            }
                            EventKind::StopInterrupt(data)
                                if data.scope == InterruptScope::Branch =>
                            {
                                let state = prompt.state.lock().unwrap();
                                for pending in state.pending.values() {
                                    pending.cancel.cancel();
                                }
                            }
                            _ => {}
                        }
                        let before = observable.get();
                        observable.update(|snapshot| snapshot.apply(&event));
                        if matches!(
                            event.kind,
                            EventKind::TurnEnd(_)
                                | EventKind::ContextScope(_)
                                | EventKind::BranchStart(_)
                        ) {
                            let snapshot = observable.get();
                            for block in context_changes(&before, &snapshot, true) {
                                append_own(
                                    &prompt.timeline,
                                    InputMessageData {
                                        blocks: vec![block],
                                        mode: Some(InputMode::Append),
                                        ..Default::default()
                                    },
                                    Vec::new(),
                                )?;
                            }
                            let state = prompt.state.lock().unwrap();
                            for pending in state.pending.values() {
                                if pending
                                    .update
                                    .sources
                                    .iter()
                                    .any(|target| !snapshot.active(&target.scope))
                                {
                                    pending.cancel.cancel();
                                }
                            }
                        }
                        // The branch takes input only after this dispatch:
                        // what the sources hold now precedes every input.
                        if matches!(
                            event.kind,
                            EventKind::BranchStart(_) | EventKind::BranchResume(_)
                        ) {
                            let (reply, opened) = oneshot::channel();
                            prompt.opening.register(Box::pin(async move {
                                opened
                                    .await
                                    .map_err(|_| AgentError::Closed("Prompt".into()))?
                            }))?;
                            prompt
                                .adoption
                                .send(Adoption::Open {
                                    event: event.clone(),
                                    reply,
                                })
                                .map_err(|_| AgentError::Closed("Prompt".into()))?;
                        }
                        prompt.settle_consumed(event.sequence);
                        Ok(())
                    })
                },
            )?;
            let projected_through = timeline.committed();
            let context = crate::history::read_context(&timeline, projected_through).await?;
            state.set(PromptSnapshot::from_context(&context));
            *projecting = projected_through;
            drop(projecting);
            ctx.provide_as(self.scope.key::<StateCell<PromptSnapshot>>(), state)?;
            ctx.provide_as::<Prompt>(self.scope.key::<Prompt>(), service.clone())?;
            ctx.provide_as::<dyn ConfigureControl>(
                self.scope.key::<dyn ConfigureControl>(),
                service,
            )?;
            Ok(Effect::Done)
        })
    }
}
impl Capability for PromptPlugin {
    fn keys(&self) -> KeysOf<Self> {
        scoped::<Self::Deps, _>(&self.scope)
    }
}

impl Prompt {
    pub(crate) async fn initial_completion(&self) -> Result<()> {
        let mut opened = self.opened.subscribe();
        loop {
            if let Some(result) = opened.borrow_and_update().clone() {
                return result;
            }
            tokio::select! {
                _ = self.branch_phase.closing() => return Err(AgentError::Cancelled("initial prompt".into())),
                _ = self.lifecycle.cancelled() => return Err(AgentError::Closed("Prompt".into())),
                changed = opened.changed() => changed.map_err(|_| AgentError::Closed("Prompt".into()))?,
            }
        }
    }

    /// Delivers what the Session's sources hold now, compared with what this
    /// branch already has. A new Session also adopts its model.
    async fn open(&self, event: &AgentEvent) -> Result<()> {
        let Some(initial) = &self.initial else {
            return Ok(());
        };
        // A new Session adopts everything; a child or a resumed branch keeps
        // its system prompt, which changes only by explicit configuration.
        let new_session = matches!(
            &event.kind,
            EventKind::BranchStart(BranchStartData { parent: None, .. })
        );
        let update = if new_session {
            initial.clone()
        } else {
            ConfigureUpdate {
                sources: initial.sources.clone(),
                model: None,
            }
        };
        Self::validate_update(&update)?;
        let cancel = self.lifecycle.child_token();
        let load = self.load(&update, cancel.clone());
        tokio::pin!(load);
        let loaded = tokio::select! {
            result = &mut load => result?,
            _ = self.branch_phase.closing() => {
                cancel.cancel();
                let _ = load.await;
                return Err(AgentError::Cancelled("initial prompt".into()));
            }
        };
        if cancel.is_cancelled()
            || matches!(
                self.branch_phase.phase(),
                BranchPhase::Closing | BranchPhase::Closed
            )
        {
            return Err(AgentError::Cancelled("initial prompt".into()));
        }
        let context = crate::history::read_context(&self.timeline, event.sequence).await?;
        let current = PromptSnapshot::from_context(&context);
        if update
            .sources
            .iter()
            .any(|target| !current.active(&target.scope))
        {
            return Err(AgentError::InvalidInput(
                "initial prompt contains an inactive scope".into(),
            ));
        }
        let system = |entry: Option<&PromptEntry>| {
            entry.is_some_and(|entry| entry.delivery == PromptDelivery::System)
        };
        let changes = loaded
            .into_iter()
            .filter_map(|(target, value)| {
                let key = (target.source, target.scope);
                let old = current.entries.get(&key);
                if !new_session && (system(old) || system(value.as_ref())) {
                    return None;
                }
                match value {
                    Some(entry) if old != Some(&entry) => Some(PromptChange::Set { entry }),
                    None if old.is_some() => Some(PromptChange::Revoke {
                        source: key.0,
                        scope: key.1,
                    }),
                    _ => None,
                }
            })
            .collect();
        self.publisher
            .publish(changes, update.model.clone(), None, context_rules, |_| {
                if cancel.is_cancelled()
                    || matches!(
                        self.branch_phase.phase(),
                        BranchPhase::Closing | BranchPhase::Closed
                    )
                {
                    Err(AgentError::Cancelled("initial prompt".into()))
                } else {
                    Ok(())
                }
            })
            .await?;
        Ok(())
    }
}

/// Input blocks that bring the rule and state changes from `before` to
/// `after` into the model context, ordered by the entries' `order`. Entry
/// bodies are final text: sources escape what comes from outside. A rule
/// that leaves because its scope `ended` expires; otherwise it is revoked.
fn context_changes(
    before: &PromptSnapshot,
    after: &PromptSnapshot,
    ended: bool,
) -> Vec<InputBlock> {
    let delivered = |snapshot: &PromptSnapshot, key: &Key| {
        snapshot
            .entries
            .get(key)
            .filter(|entry| entry.delivery != PromptDelivery::System)
            .cloned()
    };
    let mut changes: Vec<(PromptEntry, Option<PromptEntry>, Option<PromptEntry>)> = before
        .entries
        .keys()
        .chain(after.entries.keys())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .filter_map(|key| {
            let (old, new) = (delivered(before, key), delivered(after, key));
            if old == new {
                return None;
            }
            Some((new.clone().or(old.clone())?, old, new))
        })
        .collect();
    changes.sort_by(|(a, ..), (b, ..)| {
        (a.order, &a.source, &a.scope).cmp(&(b.order, &b.source, &b.scope))
    });
    changes
        .into_iter()
        .map(|(entry, old, new)| {
            let (source, scope) = (&entry.source, &entry.scope);
            let key = format!(
                "{}:{source}:{}",
                if entry.delivery == PromptDelivery::State { "state" } else { "rule" },
                serde_json::to_string(scope).expect("prompt scope serializes")
            );
            match (entry.delivery, new) {
                (PromptDelivery::State, Some(new)) => context_block::raw(
                    format!(
                        "<dim-state source=\"{}\">{}</dim-state>",
                        context_block::attr(source),
                        new.body
                    ),
                    Some(key),
                ),
                (PromptDelivery::State, None) => context_block::retire(
                    format!(
                        "<dim-state source=\"{}\" cleared=\"true\"></dim-state>",
                        context_block::attr(source)
                    ),
                    key,
                ),
                (_, Some(new)) => context_block::raw(
                    rule_text(
                        source,
                        scope,
                        if old.is_some() { "replace" } else { "set" },
                        &new.body,
                    ),
                    Some(key),
                ),
                (_, None) => {
                    let scope_text = context_block::text(&scope_text(scope));
                    let (op, body) = if ended {
                        (
                            "expired",
                            format!("{scope_text} has ended, so this rule no longer applies there. Other scopes are unaffected."),
                        )
                    } else {
                        (
                            "revoke",
                            format!("This rule no longer applies to {scope_text}. Other scopes are unaffected."),
                        )
                    };
                    context_block::retire(rule_text(source, scope, op, &body), key)
                }
            }
        })
        .collect()
}

fn rule_text(source: &str, scope: &PromptScope, op: &str, body: &str) -> String {
    context_block::element(
        "dim-rule",
        &[
            ("source", source),
            ("scope", &scope_text(scope)),
            ("op", op),
        ],
        body,
    )
}

fn scope_text(scope: &PromptScope) -> String {
    match scope {
        PromptScope::Branch { .. } => "branch".into(),
        PromptScope::Turn { id, .. } => format!("turn {id}"),
        PromptScope::Task { id, .. } => format!("task {id}"),
    }
}

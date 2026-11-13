mod recovery;

use crate::{
    context_block,
    context_preparation::{ConfigurationPublisher, ToolSetAdoption},
    host::{HostError, PromptHost},
    prompt_guide::GuideEntries,
    prompt_state::PromptSnapshot,
};
use dim_agent_runtime::*;
use dim_agent_types::*;
use futures_util::FutureExt;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, TypeKey};
use rutis::{Ctx, Error};
use std::{
    collections::{BTreeMap, BTreeSet},
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{Arc, Mutex},
};
use tokio::sync::{oneshot, watch};
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

/// What a branch adopts when it opens: the Session's prompt sources, and the
/// model a new Session adopts. Read when the branch opens, so it holds the
/// sources registered then.
pub struct InitialPromptConfiguration(Box<dyn Fn() -> Option<ConfigureUpdate> + Send + Sync>);
impl InitialPromptConfiguration {
    /// The same configuration for every branch; `None` adopts nothing.
    pub fn fixed(update: Option<ConfigureUpdate>) -> Self {
        Self(Box::new(move || update.clone()))
    }
    /// The configuration `read` returns when a branch opens.
    pub fn read(read: impl Fn() -> Option<ConfigureUpdate> + Send + Sync + 'static) -> Self {
        Self(Box::new(read))
    }
    pub fn get(&self) -> Option<ConfigureUpdate> {
        (self.0)()
    }
}

/// Rules and state enter the model context as inputs written with the change.
fn context_rules(before: &PromptSnapshot, changes: &[PromptChange]) -> Vec<InputBlock> {
    context_changes(before, changes, false)
}

/// Migrates legacy prompt adoptions to effective references on an exclusively held Timeline.
/// Existing references and branches without adopted prompts write nothing.
pub async fn migrate_prompt_references(timeline: Arc<Timeline>) -> Result<bool> {
    recovery::migrate(&timeline).await
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
        snapshot: Box<PromptSnapshot>,
        reply: oneshot::Sender<Result<()>>,
    },
    /// The entries in force changed: adopt the guide if its composition over
    /// them moved.
    Guide,
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
    initial: Arc<InitialPromptConfiguration>,
    publisher: Arc<ConfigurationPublisher>,
    timeline: Arc<Timeline>,
    dispatcher: Arc<Dispatcher>,
    faults: Arc<FaultReporter>,
    identity: Arc<TimelineIdentity>,
    lifecycle: CancellationToken,
    branch_phase: Arc<BranchLifecycle>,
    opening: Arc<BranchOpening>,
    opened: tokio::sync::watch::Sender<Option<Result<()>>>,
    /// The Session's guide entries: the composition the listener adopts.
    guide: Arc<GuideEntries>,
    tasks: TaskTracker,
    state: Mutex<PromptState>,
    snapshot: Arc<StateCell<PromptSnapshot>>,
    projection: Mutex<recovery::Projection>,
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
        self.advance(self.timeline.sequence()).await?;
        let published = self
            .adopt(
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

    /// The registrations watch and the composition it starts from, both
    /// taken before the branch's `Open` adoption composes the System. A
    /// registration made while that adoption runs therefore moves the
    /// composition the listener compares against, rather than entering a
    /// baseline taken afterwards and marked as already seen.
    fn guide_watch(&self) -> (watch::Receiver<u64>, String) {
        (
            self.guide.changes(),
            self.guide.compose(&self.identity.branch_id),
        )
    }

    /// Listens for guide registrations over the watch and composition the
    /// `Open` handler took before its adoption composed, and starts before
    /// that adoption runs: a change during the adoption moves the
    /// composition the listener compares against and is queued behind the
    /// open's own arm, instead of finding a baseline taken afterwards. A
    /// change that composes the same guide for this branch, such as another
    /// branch's entry, costs one composition and no read of the branch's
    /// context. The listener ends when the branch starts closing: an entry
    /// that withdraws only because the branch is shutting down must not
    /// move its System.
    fn listen_guide(self: &Arc<Self>, mut changes: watch::Receiver<u64>, mut composed: String) {
        let branch_id = self.identity.branch_id.clone();
        let branch_phase = self.branch_phase.clone();
        let owner = self.clone();
        self.tasks.spawn(async move {
            loop {
                tokio::select! {
                    biased;
                    _ = branch_phase.closing() => return,
                    _ = owner.lifecycle.cancelled() => return,
                    changed = changes.changed() => if changed.is_err() {
                        return;
                    },
                }
                if owner.branch_phase.phase() >= BranchPhase::Closing {
                    return;
                }
                let guide = owner.guide.compose(&branch_id);
                if guide == composed {
                    continue;
                }
                composed = guide;
                if owner.adoption.send(Adoption::Guide).is_err() {
                    return;
                }
            }
        });
    }

    async fn adopt_guide(&self) -> Result<()> {
        if self.projection.lock().unwrap().configuration.is_none() {
            return Ok(());
        }
        AssertUnwindSafe(async {
            self.advance(self.timeline.sequence()).await?;
            let (snapshot, configuration) = {
                let projection = self.projection.lock().unwrap();
                (
                    projection.snapshot.clone(),
                    projection.configuration.clone(),
                )
            };
            self.publisher
                .adopt_guide(&snapshot, configuration.as_ref())
                .await?;
            self.advance(self.timeline.sequence()).await
        })
        .catch_unwind()
        .await
        .unwrap_or_else(|panic| Err(AgentError::fault("Prompt", panic_message(panic))))
    }

    fn schedule_adoption(self: &Arc<Self>, event: Arc<AgentEvent>) -> Result<()> {
        match self.adoption.send(Adoption::Event(event)) {
            Ok(()) => Ok(()),
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
        if let Err(error) = self.advance(self.timeline.sequence()).await {
            self.finish(id, Err(error.clone()));
            return Err(error);
        }
        let scopes: Vec<_> = update
            .sources
            .iter()
            .map(|target| target.scope.clone())
            .collect();
        self.ensure_scopes(&scopes, self.timeline.sequence())
            .await?;
        let changes = {
            let projection = self.projection.lock().unwrap();
            let current = &projection.snapshot;
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
                    None if current.entries.contains_key(&key) => {
                        changes.push(PromptChange::Revoke {
                            source: target.source,
                            scope: target.scope,
                        })
                    }
                    _ => {}
                }
            }
            changes
        };
        match self
            .adopt(changes, update.model, None, context_rules, |events| {
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
                    .waiting_through = events
                    .iter()
                    .rev()
                    .find(|event| !matches!(event.kind, EventKind::InputMessage(_)))
                    .map(|event| event.sequence);
                Ok(())
            })
            .await
        {
            Ok(Some(_)) => self.finish(id, Ok(ConfigureOutcome::Adopted)),
            Ok(None) => self.finish(id, Ok(ConfigureOutcome::Unchanged)),
            Err(AgentError::Cancelled(_)) => self.finish(id, Ok(ConfigureOutcome::Superseded)),
            Err(error) => {
                self.finish(id, Err(error.clone()));
                return Err(error);
            }
        }
        Ok(())
    }

    async fn adopt(
        &self,
        changes: Vec<PromptChange>,
        model: Option<ModelSelection>,
        tools: Option<ToolSetAdoption>,
        inputs: impl FnOnce(&PromptSnapshot, &[PromptChange]) -> Vec<InputBlock> + Send,
        register: impl FnOnce(&[Arc<AgentEvent>]) -> Result<()> + Send,
    ) -> Result<Option<u64>> {
        let sequence = {
            let projection = self.projection.lock().unwrap();
            self.publisher.publish(
                (&projection.snapshot, projection.configuration.as_ref()),
                changes,
                model,
                tools,
                inputs,
                register,
            )?
        };
        if let Some(sequence) = sequence {
            self.publisher.await_published(sequence).await?;
            self.advance(sequence).await?;
        }
        Ok(sequence)
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
    timeline: rutis::host::Key<Timeline>,
    dispatcher: rutis::host::Key<Dispatcher>,
    host: rutis::host::Key<PromptHost>,
    initial: rutis::host::Key<InitialPromptConfiguration>,
    publisher: rutis::host::Key<ConfigurationPublisher>,
    identity: rutis::host::Key<TimelineIdentity>,
    faults: rutis::host::Key<FaultReporter>,
    branch_phase: rutis::host::Key<BranchLifecycle>,
    opening: rutis::host::Key<BranchOpening>,
    guide: rutis::host::Key<GuideEntries>,
    state: rutis::host::Key<StateCell<PromptSnapshot>>,
    prompt: rutis::host::Key<Prompt>,
    configure: rutis::host::Key<dyn ConfigureControl>,
}
impl PromptPlugin {
    pub fn new(names: &Names<'_>) -> std::result::Result<Self, Error> {
        Ok(Self {
            timeline: names.key()?,
            dispatcher: names.key()?,
            host: names.key()?,
            initial: names.key()?,
            publisher: names.key()?,
            identity: names.key()?,
            faults: names.key()?,
            branch_phase: names.key()?,
            opening: names.key()?,
            guide: names.key()?,
            state: names.state_cell()?,
            prompt: names.key()?,
            configure: names.key()?,
        })
    }
}
impl DynPlugin for PromptPlugin {
    fn name(&self) -> &str {
        "Prompt"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![
            self.timeline.erase(),
            self.dispatcher.erase(),
            self.host.erase(),
            self.initial.erase(),
            self.publisher.erase(),
            self.identity.erase(),
            self.faults.erase(),
            self.branch_phase.erase(),
            self.opening.erase(),
            self.guide.erase(),
        ]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let timeline = ctx.require_at(&self.timeline)?;
            let dispatcher = ctx.require_at(&self.dispatcher)?;
            let host = ctx.require_at(&self.host)?;
            let initial = ctx.require_at(&self.initial)?;
            let publisher = ctx.require_at(&self.publisher)?;
            let identity = ctx.require_at(&self.identity)?;
            let faults = ctx.require_at(&self.faults)?;
            let branch_phase = ctx.require_at(&self.branch_phase)?;
            let opening = ctx.require_at(&self.opening)?;
            let guide = ctx.require_at(&self.guide)?;
            if ctx.cancellation_token().is_cancelled() {
                return Ok(Effect::Done);
            }
            let (adoption, mut adoptions) = tokio::sync::mpsc::unbounded_channel();
            let state = Arc::new(StateCell::new(PromptSnapshot::default()));
            let service = Arc::new(Prompt {
                host,
                initial,
                publisher,
                timeline: timeline.clone(),
                dispatcher: dispatcher.clone(),
                faults,
                identity,
                lifecycle: ctx.cancellation_token(),
                branch_phase,
                opening,
                opened: tokio::sync::watch::channel(None).0,
                guide,
                tasks: TaskTracker::new(),
                state: Mutex::new(PromptState::default()),
                snapshot: state.clone(),
                projection: Mutex::new(recovery::Projection::default()),
                adoption,
            });

            let owner = service.clone();
            ctx.effect(move || async move {
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
            })?;

            let gate = Arc::new(tokio::sync::Mutex::new(0u64));
            let mut projecting = gate.lock().await;
            let handler_gate = gate.clone();
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
                    recovery::KEY,
                ],
                move |event| {
                    let prompt = prompt.clone();
                    let gate = handler_gate.clone();
                    Box::pin(async move {
                        let projected_through = *gate.lock().await;
                        if event.sequence <= projected_through {
                            return Ok(());
                        }
                        {
                            let state = prompt.state.lock().unwrap();
                            for pending in state.pending.values() {
                                let expired = pending.update.sources.iter().any(|target| match &event.kind {
                                    EventKind::TurnEnd(data) => matches!(&target.scope, PromptScope::Turn {id,..} if id == &data.turn_id),
                                    EventKind::BranchStart(_) => matches!(target.scope, PromptScope::Turn {..}),
                                    EventKind::ContextScope(data) if data.status == ContextScopeStatus::Ended => target.scope == data.scope,
                                    _ => false,
                                });
                                if expired || matches!(&event.kind,EventKind::StopInterrupt(data) if data.scope == InterruptScope::Branch) {
                                    pending.cancel.cancel();
                                }
                            }
                        }
                        // The consumption query a gate needs cannot run with
                        // the projection locked, so it is answered first.
                        let delivered = match &event.kind {
                            EventKind::PromptChanged(data) => {
                                recovery::delivered(&prompt.timeline, &prompt.identity.branch_id, data, event.sequence)
                                    .await?
                            }
                            _ => true,
                        };
                        let opening = {
                            let mut projection = prompt.projection.lock().unwrap();
                            if event.kind.key() == recovery::KEY {
                                let EventKind::Extension(data) = &event.kind else { unreachable!("private recovery event") };
                                if data.data().get("schema").and_then(serde_json::Value::as_str) != Some(recovery::SCHEMA) {
                                    return Err(AgentError::fault("Prompt", "unsupported recovery schema"));
                                }
                                projection.publish(&prompt, event.sequence);
                            } else {
                                projection.step(
                                    &prompt,
                                    &event.id,
                                    event.sequence,
                                    &event.kind,
                                    delivered,
                                )?;
                            }
                            if matches!(event.kind,EventKind::BranchStart(_) | EventKind::BranchResume(_)) {
                                let (sequence,snapshot) = projection.opening.take().ok_or_else(||AgentError::fault("Prompt","missing opening snapshot"))?;
                                if sequence != event.sequence {return Err(AgentError::fault("Prompt","opening snapshot has advanced"));}
                                Some(snapshot)
                            } else { None }
                        };
                        if let Some(snapshot) = opening {
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
                                    snapshot: Box::new(snapshot),
                                    reply,
                                })
                                .map_err(|_| AgentError::Closed("Prompt".into()))?;
                        }
                        else if matches!(event.kind, EventKind::PromptConfigureRequest(_) | EventKind::PromptConfigureResult(_)) {
                            prompt.schedule_adoption(event)?;
                        }
                        Ok(())
                    })
                },
            )?;
            let projected_through = timeline.sequence();
            let projection = recovery::Projection::restore(&service, projected_through).await?;
            state.set(projection.snapshot.clone());
            *service.projection.lock().unwrap() = projection;
            service.recover_sources(projected_through).await?;
            *projecting = projected_through;
            drop(projecting);
            let owner = service.clone();
            service.tasks.spawn(async move {
                loop {
                    let item = tokio::select! {
                        biased;
                        _ = owner.lifecycle.cancelled() => break,
                        item = adoptions.recv() => item,
                    };
                    let result = match item {
                        None => break,
                        Some(Adoption::Event(event)) => AssertUnwindSafe(async {
                            match &event.kind {
                                EventKind::PromptConfigureRequest(data) => {
                                    owner.start(&data.operation_id)
                                }
                                EventKind::PromptConfigureResult(data) => {
                                    owner.consume_result(&data.operation_id).await
                                }
                                _ => unreachable!("only configure controls retain an event"),
                            }
                        })
                        .catch_unwind()
                        .await
                        .unwrap_or_else(|panic| {
                            Err(AgentError::fault("Prompt", panic_message(panic)))
                        }),
                        Some(Adoption::Open {
                            event,
                            snapshot,
                            reply,
                        }) => {
                            let (changes, composed) = owner.guide_watch();
                            let mut result =
                                AssertUnwindSafe(async { owner.open(&event, *snapshot).await })
                                    .catch_unwind()
                                    .await
                                    .unwrap_or_else(|panic| {
                                        Err(AgentError::fault("Prompt", panic_message(panic)))
                                    });
                            if result.is_ok() {
                                owner.listen_guide(changes, composed);
                                result = owner.adopt_guide().await;
                            }
                            owner.opened.send_replace(Some(result.clone()));
                            let _ = reply.send(result.clone());
                            result
                        }
                        Some(Adoption::Guide) => owner.adopt_guide().await,
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
                            let _ = reply.send(result.clone());
                            result.map(|_| ())
                        }
                    };
                    if let Err(error) = result
                        && !matches!(
                            error,
                            AgentError::Cancelled(_)
                                | AgentError::Closed(_)
                                | AgentError::InvalidInput(_)
                        )
                    {
                        owner.faults.report(error);
                        break;
                    }
                }
            });

            ctx.provide_at(self.state.clone(), state)?;
            ctx.provide_at::<Prompt>(self.prompt.clone(), service.clone())?;
            ctx.provide_at::<dyn ConfigureControl>(self.configure.clone(), service)?;
            Ok(Effect::Done)
        })
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
    async fn open(&self, event: &AgentEvent, mut current: PromptSnapshot) -> Result<()> {
        let Some(initial) = self.initial.get() else {
            return Ok(());
        };
        let new_session = matches!(
            &event.kind,
            EventKind::BranchStart(BranchStartData { parent: None, .. })
        );
        let update = if new_session {
            initial
        } else {
            ConfigureUpdate {
                sources: initial.sources.clone(),
                model: None,
            }
        };
        Self::validate_update(&update)?;
        let scopes: Vec<_> = update
            .sources
            .iter()
            .map(|target| target.scope.clone())
            .collect();
        self.opening_scopes(&mut current, &scopes, event.sequence)
            .await?;
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
        self.advance(self.timeline.sequence()).await?;
        self.ensure_scopes(&scopes, self.timeline.sequence())
            .await?;
        self.adopt(changes, update.model.clone(), None, context_rules, |_| {
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

fn context_changes(
    before: &PromptSnapshot,
    adopted: &[PromptChange],
    ended: bool,
) -> Vec<InputBlock> {
    let mut targets = BTreeMap::new();
    for change in adopted {
        match change {
            PromptChange::Set { entry } => {
                targets.insert((&entry.source, &entry.scope), Some(entry));
            }
            PromptChange::Revoke { source, scope } => {
                targets.insert((source, scope), None);
            }
        }
    }
    let mut changes = Vec::new();
    for ((source, scope), new) in targets {
        let key = (source.clone(), scope.clone());
        let old = before
            .entries
            .get(&key)
            .filter(|entry| entry.delivery != PromptDelivery::System);
        let new = new.filter(|entry| entry.delivery != PromptDelivery::System);
        if old != new
            && let Some(entry) = new.or(old)
        {
            changes.push((entry, old.is_some(), new));
        }
    }
    changes.sort_by_key(|(entry, _, _)| (entry.order, &entry.source, &entry.scope));
    changes
        .into_iter()
        .map(|(entry, old, new)| context_input(entry, old, new, ended))
        .collect()
}

fn context_input(
    entry: &PromptEntry,
    old: bool,
    new: Option<&PromptEntry>,
    ended: bool,
) -> InputBlock {
    let (source, scope) = (&entry.source, &entry.scope);
    let key = format!(
        "{}:{source}:{}",
        if entry.delivery == PromptDelivery::State {
            "state"
        } else {
            "rule"
        },
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
                if old { "replace" } else { "set" },
                &new.body,
            ),
            Some(key),
        ),
        (_, None) => {
            let scope_text = context_block::text(&scope_text(scope));
            let (op, body) = if ended {
                (
                    "expired",
                    format!(
                        "{scope_text} has ended, so this rule no longer applies there. Other scopes are unaffected."
                    ),
                )
            } else {
                (
                    "revoke",
                    format!(
                        "This rule no longer applies to {scope_text}. Other scopes are unaffected."
                    ),
                )
            };
            context_block::retire(rule_text(source, scope, op, &body), key)
        }
    }
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

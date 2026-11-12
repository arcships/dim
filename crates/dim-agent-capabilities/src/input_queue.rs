mod pending;
mod recovery;

use crate::{
    host::HostFuture,
    methods::{Interruption, MethodCall, MethodDecl, RpcError, SessionMethods, params, reply},
    timeline_tree::TreeCommands,
};
use dim_agent_runtime::{
    AgentError, Dispatcher, InputSnapshot, Names, Registrations, Result, StateCell, Timeline,
};
use dim_agent_types::*;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, Key, TypeKey};
use rutis::{Ctx, Error};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
use tokio_util::sync::CancellationToken;

/// What InputQueue writes for an input it decided on.
#[derive(Debug, Clone)]
enum Decision {
    Apply(String, InputMode),
    Delete(String, Option<InputMessageData>),
}
/// InputQueue's answer to one `input/withdraw`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Withdrawal {
    /// The input was held: `input/deleted` at this sequence, never applied.
    Deleted { sequence: u64 },
    /// Unknown, already applied or already deleted: nothing was written.
    NotHeld,
}

type WithdrawalAnswer = (Withdrawal, Option<Arc<AgentEvent>>);

/// One committed input until Loop consumes it.
struct Entry {
    id: String,
    kind: EntryKind,
}
enum EntryKind {
    /// An input this queue releases by its mode.
    Input { mode: InputMode, released: bool },
    /// Held for the turn it names, never released or counted.
    Handover { turn_id: String },
}
/// What InputQueue derives from the events it consumes.
#[derive(Default)]
struct QueueState {
    entries: Vec<Entry>,
    queued: usize,
    /// The open turn's id, between a dispatched `turn/start` and its
    /// `turn/end`.
    turn: Option<String>,
    /// An input released while no turn ran is about to start one: until
    /// its `turn/start`, queue inputs wait as if the turn ran. A
    /// `stop/interrupt` ends the wait, since inputs applied before it no
    /// longer start a turn.
    starting: bool,
}
impl QueueState {
    fn step(&mut self, event: &AgentEvent) -> Vec<Decision> {
        match &event.kind {
            EventKind::InputMessage(data) => self.input(
                &event.id,
                data.effective_mode(),
                data.handover
                    .as_ref()
                    .map(|handover| handover.turn_id.as_str()),
            ),
            EventKind::TurnStart(data) => {
                self.turn = Some(data.turn_id.clone());
                self.starting = false;
                Vec::new()
            }
            EventKind::StopInterrupt(_) => {
                self.starting = false;
                Vec::new()
            }
            EventKind::TurnEnd(data) => self.end_turn(&data.turn_id),
            EventKind::InputConsumed(data) => {
                if let Some(index) = self
                    .entries
                    .iter()
                    .position(|entry| entry.id == data.input_id)
                {
                    let entry = self.entries.remove(index);
                    self.queued -= usize::from(matches!(entry.kind, EntryKind::Input { .. }));
                }
                Vec::new()
            }
            EventKind::InputWithdraw(data) => self.withdraw(&data.input_id, data.steer.clone()),
            _ => Vec::new(),
        }
    }
    fn input(&mut self, id: &str, mode: InputMode, handover: Option<&str>) -> Vec<Decision> {
        if let Some(turn_id) = handover {
            if self.turn.as_deref() != Some(turn_id) {
                return vec![Decision::Delete(id.to_owned(), None)];
            }
            self.entries.push(Entry {
                id: id.to_owned(),
                kind: EntryKind::Handover {
                    turn_id: turn_id.to_owned(),
                },
            });
            return Vec::new();
        }
        self.queued += 1;
        self.entries.push(Entry {
            id: id.to_owned(),
            kind: EntryKind::Input {
                mode,
                released: false,
            },
        });
        let held_queue = self.entries.iter().any(|entry| {
            matches!(
                entry.kind,
                EntryKind::Input {
                    mode: InputMode::Queue,
                    released: false
                }
            ) && entry.id != id
        });
        match mode {
            InputMode::Queue if self.turn.is_some() || self.starting || held_queue => Vec::new(),
            _ => self.release(id),
        }
    }
    fn end_turn(&mut self, turn: &str) -> Vec<Decision> {
        self.turn = None;
        let mut decisions = Vec::new();
        self.entries.retain(|entry| {
            let ended = matches!(&entry.kind, EntryKind::Handover { turn_id } if turn_id == turn);
            if ended {
                decisions.push(Decision::Delete(entry.id.clone(), None));
            }
            !ended
        });
        if let Some(next) = self.entries.iter().find(|entry| {
            matches!(
                entry.kind,
                EntryKind::Input {
                    mode: InputMode::Queue,
                    released: false
                }
            )
        }) {
            let next = next.id.clone();
            decisions.extend(self.release(&next));
        }
        decisions
    }
    fn withdraw(&mut self, id: &str, steer: Option<InputMessageData>) -> Vec<Decision> {
        match self.entries.iter().position(|entry| {
            entry.id == id && !matches!(entry.kind, EntryKind::Input { released: true, .. })
        }) {
            Some(index) => {
                let entry = self.entries.remove(index);
                self.queued -= usize::from(matches!(entry.kind, EntryKind::Input { .. }));
                vec![Decision::Delete(id.to_owned(), steer)]
            }
            None => Vec::new(),
        }
    }
    /// Releases one input. One that drives a turn while none runs makes a
    /// turn about to start.
    fn release(&mut self, id: &str) -> Vec<Decision> {
        let Some(entry) = self.entries.iter_mut().find(|entry| entry.id == id) else {
            return Vec::new();
        };
        let EntryKind::Input { mode, released } = &mut entry.kind else {
            return Vec::new();
        };
        *released = true;
        let mode = *mode;
        if self.turn.is_none() && mode != InputMode::Append {
            self.starting = true;
        }
        vec![Decision::Apply(entry.id.clone(), mode)]
    }
}
/// Consumes `input/message` and decides by its mode when Loop may consume
/// it, writing `input/applied` with the mode then: steer and append inputs
/// at once; a queue input when no turn runs or is about to start,
/// otherwise after the turn ends, one per turn. A handover input is never
/// released: it waits for the turn it names and is deleted when that turn
/// ends, or on arrival when no such turn is open. Knows of turns only
/// through `turn/start`, `turn/end` and `stop/interrupt`; Loop decides how
/// to consume what is applied. Consumes `input/withdraw` in order and
/// writes `input/deleted` for an input it still holds unapplied.
pub struct InputQueue {
    state: Mutex<QueueState>,
    observable: Arc<StateCell<InputSnapshot>>,
    pending_snapshot: Mutex<Option<(u64, usize)>>,
    timeline: Arc<Timeline>,
    /// Answers to `input/withdraw` commands by their sequence, until the
    /// withdrawal that wrote each one collects it.
    answers: Mutex<HashMap<u64, Option<(u64, WithdrawalAnswer)>>>,
    answered: tokio::sync::Notify,
    /// Ends waits when this generation unloads.
    lifecycle: CancellationToken,
}

struct WithdrawalWaiter<'a> {
    queue: &'a InputQueue,
    sequence: u64,
}

impl Drop for WithdrawalWaiter<'_> {
    fn drop(&mut self) {
        self.queue.answers.lock().unwrap().remove(&self.sequence);
    }
}

impl InputQueue {
    fn publish(&self, through: u64, queued: usize) {
        let mut pending = self.pending_snapshot.lock().unwrap();
        let through = through.max(pending.as_ref().map_or(0, |(at, _)| *at));
        *pending = Some((through, queued));
        self.publish_ready(&mut pending);
    }
    fn publish_ready(&self, pending: &mut Option<(u64, usize)>) {
        if pending
            .as_ref()
            .is_some_and(|(at, _)| self.timeline.sequence() >= *at)
        {
            let (_, queued) = pending.take().expect("a published snapshot");
            if self.observable.get().queued != queued {
                self.observable.set(InputSnapshot { queued });
            }
        }
    }
    fn on_event(&self, event: Arc<AgentEvent>) -> Result<()> {
        if matches!(
            event.kind,
            EventKind::InputApplied(_) | EventKind::InputDeleted(_)
        ) {
            self.publish_ready(&mut self.pending_snapshot.lock().unwrap());
            return Ok(());
        }
        let mut state = self.state.lock().unwrap();
        let decisions = state.step(&event);
        let (published, answer) = self.write(decisions, event.sequence)?;
        self.publish(published, state.queued);
        if matches!(event.kind, EventKind::InputWithdraw(_)) {
            if let Some(waiter) = self.answers.lock().unwrap().get_mut(&event.sequence) {
                *waiter = Some((published, answer));
            }
            self.answered.notify_waiters();
        }
        Ok(())
    }
    /// Withdraws an input by its `input_id` and replies with this queue's
    /// decision for the command. Only an accepted withdrawal adopts its
    /// optional checked Steer candidate, atomically with the deletion.
    /// A Steer result is committed; callers confirm publication before exposing a receipt.
    /// Plain withdrawals wait for publication here.
    pub async fn withdraw(&self, command: InputWithdrawData) -> Result<WithdrawalAnswer> {
        command
            .validate_external()
            .map_err(|reason| AgentError::InvalidInput(reason.into()))?;
        let drafts = vec![EventKind::InputWithdraw(command).into()];
        let mut waiter = None;
        let appended = self.timeline.append_batch_registered(drafts, |events| {
            self.answers
                .lock()
                .unwrap()
                .insert(events[0].sequence, None);
            waiter = Some(WithdrawalWaiter {
                queue: self,
                sequence: events[0].sequence,
            });
            Ok(())
        });
        let _waiter = waiter;
        let events = appended?;
        let sequence = events[0].sequence;
        let unknown = || {
            AgentError::PublicationUnknown(format!(
                "withdrawal {sequence} was accepted, but its outcome was not published; check the input after recovery"
            ))
        };
        let mut publication = self.timeline.watch();
        loop {
            let answered = self.answered.notified();
            {
                let answers = self.answers.lock().unwrap();
                if let Some(Some((published, answer))) = answers.get(&sequence)
                    && (answer.1.is_some() || self.timeline.sequence() >= *published)
                {
                    return Ok(answer.clone());
                }
            }
            if self.timeline.failure().is_some() {
                return Err(unknown());
            }
            tokio::select! {
                _ = answered => {}
                result = publication.changed() => {
                    result.map_err(|_| unknown())?;
                }
                _ = self.lifecycle.cancelled() => {
                    return Err(unknown());
                }
            }
        }
    }
}
pub struct InputQueuePlugin {
    state: Key<StateCell<InputSnapshot>>,
    queue: Key<InputQueue>,
    timeline: Key<Timeline>,
    dispatcher: Key<Dispatcher>,
}
impl InputQueuePlugin {
    pub fn new(names: &Names<'_>) -> std::result::Result<Self, Error> {
        Ok(Self {
            state: names.state_cell()?,
            queue: names.key()?,
            timeline: names.key()?,
            dispatcher: names.key()?,
        })
    }
}
impl DynPlugin for InputQueuePlugin {
    fn name(&self) -> &str {
        "InputQueue"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.timeline.erase(), self.dispatcher.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let timeline = ctx.require_at(&self.timeline)?;
            let dispatcher = ctx.require_at(&self.dispatcher)?;
            let queue = Arc::new(InputQueue {
                state: Mutex::new(QueueState::default()),
                observable: Arc::new(StateCell::new(InputSnapshot::default())),
                pending_snapshot: Mutex::new(None),
                timeline,
                answers: Mutex::new(HashMap::new()),
                answered: tokio::sync::Notify::new(),
                lifecycle: ctx.cancellation_token(),
            });
            let owner = queue.clone();
            let read = queue.clone();
            let context = ctx.clone();
            dispatcher
                .consume(
                    ctx,
                    "InputQueue".into(),
                    [
                        "input/message",
                        "input/consumed",
                        "input/withdraw",
                        "turn/start",
                        "turn/end",
                        "stop/interrupt",
                        "input/applied",
                        "input/deleted",
                    ]
                    .map(String::from)
                    .to_vec(),
                    Arc::new(move || {
                        let read = read.clone();
                        let context = context.clone();
                        Box::pin(async move { read.rebuild(&context).await })
                    }),
                    Arc::new(move |event| {
                        let owner = owner.clone();
                        Box::pin(async move { owner.on_event(Arc::new(event)) })
                    }),
                )
                .await?;
            ctx.provide_at(self.state.clone(), queue.observable.clone())?;
            ctx.provide_at(self.queue.clone(), queue)?;
            Ok(Effect::Done)
        })
    }
}

/// The Session method namespace InputQueue provides through `sessionMethods`.
pub const INPUT_METHODS: &str = "session.input";

/// The operator-input entry that asks the submit checks (Hooks A.6.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubmitEntry {
    /// Rpc `session.submit`.
    Submit,
    /// Rpc `session.branch_create`, whose prompt becomes the branch's first
    /// input.
    Branch,
    /// `session.input.withdraw` with `steer`.
    Steer,
}
impl SubmitEntry {
    pub fn name(self) -> &'static str {
        match self {
            Self::Submit => "submit",
            Self::Branch => "branch",
            Self::Steer => "steer",
        }
    }
}

/// The input an entry is about to write, as its checks see it (Hooks A.6.2).
#[derive(Clone, Debug)]
pub struct SubmitCheckRequest {
    pub entry: SubmitEntry,
    pub branch_id: String,
    /// The operator's own content; `session.branch_create` passes the prompt.
    pub user: Option<UserInput>,
    pub mode: InputMode,
}

/// What one check decided for the input (Hooks A.6.2).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SubmitCheckReply {
    /// The input may be written; these contexts go after the operator's own
    /// content, in name order. `warnings` are for the caller to show, never
    /// for the model: work the check did not do, which is not a block.
    Proceed {
        blocks: Vec<String>,
        warnings: Vec<String>,
    },
    /// The input must not be written; the message is the caller's error.
    Block { message: String },
}

/// What the table decided for the input (Hooks A.6.2): every check that ran,
/// combined in name order. A block ends the table's work and carries neither
/// blocks nor warnings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SubmitDecision {
    /// The input may be written.
    Proceed {
        blocks: Vec<String>,
        warnings: Vec<SubmitWarning>,
    },
    /// The input must not be written; the message is the caller's error.
    Block { message: String },
}

/// A check of operator input, registered with [`SubmitChecks`] by the part
/// that answers it. An error is no opinion and counts as proceed.
pub trait CheckSubmission: Send + Sync + 'static {
    fn check(
        &self,
        request: SubmitCheckRequest,
        cancel: CancellationToken,
    ) -> HostFuture<'_, SubmitCheckReply>;
}

/// The entry's own call was interrupted; the input must not be written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CheckInterrupted;

/// The Session table of submit checks (Hooks A.6.2), provided by InputQueue's
/// Session part. Session parts register checks with it while they run, and
/// the operator-input entries call them before writing anything. No check is
/// registered unless a part registers one, and then the entries write
/// unchecked.
#[derive(Default)]
pub struct SubmitChecks {
    checks: Arc<Registrations<Arc<dyn CheckSubmission>>>,
}
impl SubmitChecks {
    /// Registers `check` under `name` on behalf of the part that owns `ctx`,
    /// until that part unloads. A second check of the same name fails.
    pub fn register(
        &self,
        ctx: &Ctx,
        name: &str,
        check: Arc<dyn CheckSubmission>,
    ) -> std::result::Result<(), Error> {
        self.checks.register(ctx, name, check).map(|_| ())
    }

    /// Calls each registered check in name order. The first block in name
    /// order is the answer; otherwise every check's blocks and warnings are
    /// returned in name order. `Err` means the entry's own call was
    /// interrupted while a check ran, and nothing may be written.
    pub async fn check(
        &self,
        request: &SubmitCheckRequest,
        cancel: &CancellationToken,
    ) -> std::result::Result<SubmitDecision, CheckInterrupted> {
        let mut blocks = Vec::new();
        let mut warnings = Vec::new();
        for registration in self.checks.all() {
            if cancel.is_cancelled() {
                return Err(CheckInterrupted);
            }
            match registration
                .value
                .check(request.clone(), cancel.clone())
                .await
            {
                Ok(SubmitCheckReply::Proceed {
                    blocks: added,
                    warnings: raised,
                }) => {
                    for block in added {
                        // A context lands after the operator's own content, so
                        // index 1 is the first place one may go; anything else
                        // would be read as the operator's words (A.6.2).
                        if !markup::runtime_context(1, &block) {
                            tracing::warn!(
                                check = %registration.name,
                                "submit check context dropped: it is not a runtime marker element"
                            );
                            continue;
                        }
                        blocks.push(block);
                    }
                    warnings.extend(raised.into_iter().map(|message| SubmitWarning {
                        check: registration.name.clone(),
                        message,
                    }));
                }
                Ok(SubmitCheckReply::Block { message }) => {
                    return Ok(SubmitDecision::Block { message });
                }
                Err(_) if cancel.is_cancelled() => return Err(CheckInterrupted),
                Err(_) => {}
            }
        }
        if cancel.is_cancelled() {
            return Err(CheckInterrupted);
        }
        Ok(SubmitDecision::Proceed { blocks, warnings })
    }
}

/// Appends the checks' contexts after the operator's own content (Hooks
/// A.6.2); an input without operator content takes none. A context placed
/// there would be the input's first block, which every reader takes for the
/// operator's words.
///
/// Each block must be one runtime marker element
/// (`dim_agent_types::markup::runtime_context`), which is the producer's
/// precondition: a block that is not one is dropped with a diagnostic instead
/// of appended, because both readers would take it for the operator's words.
pub fn append_blocks(user: &mut Option<UserInput>, blocks: Vec<String>) {
    let Some(user) = user.as_mut().filter(|user| user.has_operator_content()) else {
        if !blocks.is_empty() {
            tracing::warn!("submit check contexts dropped: the input has no operator content");
        }
        return;
    };
    for text in blocks {
        // The submit-check table decides this drop for every in-tree entry;
        // the gate here guards a caller that appends blocks without going
        // through the table.
        if !markup::runtime_context(user.content.len(), &text) {
            tracing::warn!("submit check context dropped: it is not a runtime marker element");
            continue;
        }
        user.content.push(ContentBlock::Text { text });
    }
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WithdrawParams {
    /// Read by Rpc to route the call; the provider already serves this Session.
    #[serde(rename = "sessionId")]
    _session_id: String,
    branch_id: String,
    input_id: String,
    #[serde(default)]
    reason: Option<String>,
    /// Replacement content adopted only when withdrawal is accepted.
    #[serde(default)]
    steer: Option<UserInput>,
}

/// Serves `session.input.withdraw` for every branch of its Session through
/// the branch's InputQueue, so Rpc names no owning service.
pub struct InputMethodsPlugin {
    tree: Key<TreeCommands>,
    methods: Key<SessionMethods>,
    checks: Key<SubmitChecks>,
}
impl InputMethodsPlugin {
    pub fn new(names: &Names<'_>) -> std::result::Result<Self, Error> {
        Ok(Self {
            tree: names.key()?,
            methods: names.key()?,
            checks: names.key()?,
        })
    }
}
impl DynPlugin for InputMethodsPlugin {
    fn name(&self) -> &str {
        "InputMethods"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.tree.erase(), self.methods.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let tree = ctx.require_at(&self.tree)?;
            let methods = ctx.require_at(&self.methods)?;
            let checks = Arc::new(SubmitChecks::default());
            ctx.provide_at(self.checks.clone(), checks.clone())?;
            methods.register(
                ctx,
                INPUT_METHODS,
                vec![
                    MethodDecl::new("session.input.withdraw", true, Interruption::Report),
                    MethodDecl::new("session.input.pending", false, Interruption::Report),
                ],
                Arc::new(move |call, cancel, _| {
                    let MethodCall { method, params: value, .. } = call;
                    let tree = tree.clone();
                    let checks = checks.clone();
                    Box::pin(async move {
                        if method == "session.input.pending" {
                            return pending::read(&tree, value, &cancel).await;
                        }
                        let p: WithdrawParams = params(value)?;
                        let branch = tree.get(&p.branch_id)?;
                        let queue = branch.service::<InputQueue>()?;
                        let mut steer = p.steer.map(|user| InputMessageData {
                            user: Some(user),
                            mode: Some(InputMode::Steer),
                            ..Default::default()
                        });
                        let mut checks_warnings = Vec::new();
                        if let Some(input) = &steer {
                            // The operator's own content is validated before
                            // the checks run: a check's context must never be
                            // what makes an input valid, and a refusal here
                            // writes neither the withdrawal nor a replacement.
                            input
                                .validate()
                                .map_err(|reason| RpcError::from(AgentError::InvalidInput(reason.into())))?;
                            // A blocked steer writes neither the withdrawal nor
                            // the new input (Hooks A.6.2).
                            match checks
                                .check(
                                    &SubmitCheckRequest {
                                        entry: SubmitEntry::Steer,
                                        branch_id: p.branch_id.clone(),
                                        user: input.user.clone(),
                                        mode: InputMode::Steer,
                                    },
                                    &cancel,
                                )
                                .await
                            {
                                Err(CheckInterrupted) => return Err(RpcError::interrupted()),
                                Ok(SubmitDecision::Block { message }) => {
                                    return Err(RpcError::app(-32000, "input_blocked", message));
                                }
                                Ok(SubmitDecision::Proceed {
                                    blocks,
                                    warnings,
                                }) => {
                                    if let Some(steer) = &mut steer {
                                        append_blocks(&mut steer.user, blocks);
                                    }
                                    checks_warnings = warnings;
                                }
                            }
                        }
                        let (answer, replacement) = queue
                            .withdraw(
                                InputWithdrawData {
                                    input_id: p.input_id,
                                    reason: p.reason,
                                    steer,
                                },
                            )
                            .await?;
                        let mut result = match answer {
                            Withdrawal::Deleted { sequence } => {
                                if replacement.is_some() {
                                    let publication = branch.publication();
                                    let mut progress = publication.watch();
                                    tokio::select! {
                                        _ = progress.wait_for(|&(published, closed)| published >= sequence || closed) => {}
                                        _ = cancel.cancelled() => return Err(RpcError::interrupted()),
                                    }
                                    if publication.sequence() < sequence {
                                        return Err(RpcError::app(
                                            -32000,
                                            "publication_unknown",
                                            "committed, but publication was not confirmed; query history after recovery",
                                        ));
                                    }
                                }
                                serde_json::json!({"status": "deleted", "sequence": sequence})
                            }
                            Withdrawal::NotHeld => serde_json::json!({"status": "notHeld"}),
                        };
                        if let Some(event) = replacement {
                            result["steer"] = serde_json::json!({"inputId": event.id, "sequence": event.sequence});
                            // A warning describes the replacement this call
                            // wrote; a withdrawal that wrote none carries
                            // neither.
                            if !checks_warnings.is_empty() {
                                result["warnings"] = serde_json::json!(checks_warnings);
                            }
                        }
                        reply(result)
                    })
                }),
            )?;
            Ok(Effect::Done)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::HostError;
    use rutis::Plugin;
    use rutis::host::{CtxHost, FiberState, FiberView};
    use std::sync::Mutex;
    use tokio::sync::Notify;

    /// How a [`TestCheck`] answers; `Gate` records that it is running, then
    /// waits for the token it was handed and reports that the token fired.
    enum Answer {
        Proceed(Vec<String>),
        ProceedWith {
            blocks: Vec<String>,
            warnings: Vec<String>,
        },
        Block(String),
        Fail,
        Gate {
            entered: Arc<Notify>,
            /// Set once the check saw its own token cancelled.
            cancelled: Arc<Mutex<bool>>,
        },
    }

    /// One check: it records every call in its label, in call order, then
    /// answers as told.
    struct TestCheck {
        label: &'static str,
        log: Arc<Mutex<Vec<&'static str>>>,
        answer: Answer,
    }
    impl CheckSubmission for TestCheck {
        fn check(
            &self,
            _: SubmitCheckRequest,
            cancel: CancellationToken,
        ) -> HostFuture<'_, SubmitCheckReply> {
            self.log.lock().unwrap().push(self.label);
            match &self.answer {
                Answer::Proceed(blocks) => {
                    let blocks = blocks.clone();
                    Box::pin(async move {
                        Ok(SubmitCheckReply::Proceed {
                            blocks,
                            warnings: Vec::new(),
                        })
                    })
                }
                Answer::ProceedWith { blocks, warnings } => {
                    let (blocks, warnings) = (blocks.clone(), warnings.clone());
                    Box::pin(async move { Ok(SubmitCheckReply::Proceed { blocks, warnings }) })
                }
                Answer::Block(message) => {
                    let message = message.clone();
                    Box::pin(async move { Ok(SubmitCheckReply::Block { message }) })
                }
                Answer::Fail => Box::pin(async move { Err(HostError::failed("the check failed")) }),
                Answer::Gate { entered, cancelled } => {
                    let (entered, cancelled) = (entered.clone(), cancelled.clone());
                    Box::pin(async move {
                        entered.notify_one();
                        cancel.cancelled().await;
                        *cancelled.lock().unwrap() = true;
                        Err(HostError::failed("cancelled while running"))
                    })
                }
            }
        }
    }

    fn request() -> SubmitCheckRequest {
        SubmitCheckRequest {
            entry: SubmitEntry::Submit,
            branch_id: "main".into(),
            user: None,
            mode: InputMode::Queue,
        }
    }

    fn check(
        log: &Arc<Mutex<Vec<&'static str>>>,
        label: &'static str,
        answer: Answer,
    ) -> Arc<dyn CheckSubmission> {
        Arc::new(TestCheck {
            label,
            log: log.clone(),
            answer,
        })
    }
    /// A check that proceeds with one runtime marker context per text, as a
    /// producer builds them (A.6.2).
    fn proceed(blocks: &[&str]) -> Answer {
        Answer::Proceed(blocks.iter().map(|block| marker(block)).collect())
    }
    /// The same, with the check's own work that failed reported as warnings.
    fn proceed_with(blocks: &[&str], warnings: &[&str]) -> Answer {
        Answer::ProceedWith {
            blocks: blocks.iter().map(|block| marker(block)).collect(),
            warnings: warnings
                .iter()
                .map(|warning| (*warning).to_owned())
                .collect(),
        }
    }
    /// One context block, as a producer returns it: a single runtime marker
    /// element (A.6.2).
    fn marker(text: &str) -> String {
        format!("<dim-hooks kind=\"prompt-submit\">{text}</dim-hooks>")
    }
    /// The contexts of `blocks`, in the form a producer returns them.
    fn marked(blocks: &[&str]) -> Vec<String> {
        blocks.iter().map(|block| marker(block)).collect()
    }

    /// A part that registers its checks on apply, as an owner does.
    struct ChecksPart {
        checks: Arc<SubmitChecks>,
        entries: Vec<(&'static str, Arc<dyn CheckSubmission>)>,
    }
    impl Plugin for ChecksPart {
        type Deps = ();
        fn name(&self) -> &str {
            "TestSubmitChecks"
        }
        async fn apply(&self, ctx: &Ctx, (): Self::Deps) -> rutis::Result {
            for (name, check) in &self.entries {
                self.checks.register(ctx, name, check.clone())?;
            }
            Ok(())
        }
    }

    async fn mount(
        root: &Ctx,
        checks: &Arc<SubmitChecks>,
        entries: Vec<(&'static str, Arc<dyn CheckSubmission>)>,
    ) -> FiberView {
        let part = root.plugin(ChecksPart {
            checks: checks.clone(),
            entries,
        });
        (&part).await.unwrap();
        assert_eq!(part.state().state, FiberState::Active);
        part
    }

    /// A.6.2: the checks run in name order; the first block answers and the
    /// checks after it are not called.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn checks_run_in_name_order_and_the_first_block_answers() {
        let root = Ctx::root().unwrap();
        let checks = Arc::new(SubmitChecks::default());
        let log = Arc::new(Mutex::new(Vec::new()));
        let part = mount(
            &root,
            &checks,
            vec![
                ("zeta", check(&log, "zeta", proceed(&["z"]))),
                (
                    "alpha",
                    check(&log, "alpha", Answer::Block("no prompts".into())),
                ),
                ("mid", check(&log, "mid", proceed(&["m"]))),
            ],
        )
        .await;
        assert_eq!(
            checks.check(&request(), &CancellationToken::new()).await,
            Ok(SubmitDecision::Block {
                message: "no prompts".into()
            })
        );
        assert_eq!(*log.lock().unwrap(), ["alpha"]);
        part.shutdown().await.unwrap();
        root.shutdown().await.unwrap();
    }

    /// A.6.2: without a block every check's contexts come back, in name
    /// order, and every check is called.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn proceeding_checks_add_their_blocks_in_name_order() {
        let root = Ctx::root().unwrap();
        let checks = Arc::new(SubmitChecks::default());
        let log = Arc::new(Mutex::new(Vec::new()));
        let part = mount(
            &root,
            &checks,
            vec![
                ("zeta", check(&log, "zeta", proceed(&["3"]))),
                ("alpha", check(&log, "alpha", proceed(&["1"]))),
                ("mid", check(&log, "mid", proceed(&["2"]))),
            ],
        )
        .await;
        assert_eq!(
            checks.check(&request(), &CancellationToken::new()).await,
            Ok(SubmitDecision::Proceed {
                blocks: marked(&["1", "2", "3"]),
                warnings: Vec::new(),
            })
        );
        assert_eq!(*log.lock().unwrap(), ["alpha", "mid", "zeta"]);
        part.shutdown().await.unwrap();
        root.shutdown().await.unwrap();
    }

    /// A.6.2: each warning names the check that raised it, and the table keeps
    /// them in name order beside the contexts.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn warnings_name_their_check_in_name_order() {
        let root = Ctx::root().unwrap();
        let checks = Arc::new(SubmitChecks::default());
        let log = Arc::new(Mutex::new(Vec::new()));
        let part = mount(
            &root,
            &checks,
            vec![
                (
                    "zeta",
                    check(&log, "zeta", proceed_with(&[], &["zeta failed"])),
                ),
                (
                    "alpha",
                    check(&log, "alpha", proceed_with(&["1"], &["alpha failed"])),
                ),
            ],
        )
        .await;
        assert_eq!(
            checks.check(&request(), &CancellationToken::new()).await,
            Ok(SubmitDecision::Proceed {
                blocks: marked(&["1"]),
                warnings: vec![
                    SubmitWarning {
                        check: "alpha".into(),
                        message: "alpha failed".into(),
                    },
                    SubmitWarning {
                        check: "zeta".into(),
                        message: "zeta failed".into(),
                    },
                ],
            })
        );
        part.shutdown().await.unwrap();
        root.shutdown().await.unwrap();
    }

    /// A.6.2: a block that is not one runtime marker element is dropped here,
    /// where the registration name is known, and is not a warning: it is a
    /// producer defect, not work the check did not do.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_block_that_is_not_a_marker_is_dropped_and_is_no_warning() {
        let root = Ctx::root().unwrap();
        let checks = Arc::new(SubmitChecks::default());
        let log = Arc::new(Mutex::new(Vec::new()));
        let part = mount(
            &root,
            &checks,
            vec![(
                "alpha",
                check(
                    &log,
                    "alpha",
                    Answer::ProceedWith {
                        blocks: vec!["bare text".into(), marker("kept")],
                        warnings: Vec::new(),
                    },
                ),
            )],
        )
        .await;
        assert_eq!(
            checks.check(&request(), &CancellationToken::new()).await,
            Ok(SubmitDecision::Proceed {
                blocks: marked(&["kept"]),
                warnings: Vec::new(),
            })
        );
        part.shutdown().await.unwrap();
        root.shutdown().await.unwrap();
    }

    /// A.6.2: a check that fails is no opinion; the other checks still run
    /// and the entry proceeds.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_check_that_fails_counts_as_proceed() {
        let root = Ctx::root().unwrap();
        let checks = Arc::new(SubmitChecks::default());
        let log = Arc::new(Mutex::new(Vec::new()));
        let part = mount(
            &root,
            &checks,
            vec![
                ("alpha", check(&log, "alpha", Answer::Fail)),
                ("beta", check(&log, "beta", proceed(&["b"]))),
            ],
        )
        .await;
        assert_eq!(
            checks.check(&request(), &CancellationToken::new()).await,
            Ok(SubmitDecision::Proceed {
                blocks: marked(&["b"]),
                warnings: Vec::new(),
            })
        );
        assert_eq!(*log.lock().unwrap(), ["alpha", "beta"]);
        part.shutdown().await.unwrap();
        root.shutdown().await.unwrap();
    }

    /// A.6.2: a call already interrupted runs no check and reports it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_interrupted_call_runs_no_check() {
        let root = Ctx::root().unwrap();
        let checks = Arc::new(SubmitChecks::default());
        let log = Arc::new(Mutex::new(Vec::new()));
        let part = mount(
            &root,
            &checks,
            vec![("alpha", check(&log, "alpha", proceed(&[])))],
        )
        .await;
        let cancel = CancellationToken::new();
        cancel.cancel();
        assert_eq!(
            checks.check(&request(), &cancel).await,
            Err(CheckInterrupted)
        );
        assert!(log.lock().unwrap().is_empty());
        part.shutdown().await.unwrap();
        root.shutdown().await.unwrap();
    }

    /// A.6.2: interrupting the call while a check runs reaches the check
    /// through its token, stops it and reports the interruption instead of a
    /// proceed.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_interrupted_call_stops_a_running_check() {
        let root = Ctx::root().unwrap();
        let checks = Arc::new(SubmitChecks::default());
        let log = Arc::new(Mutex::new(Vec::new()));
        let entered = Arc::new(Notify::new());
        let cancelled = Arc::new(Mutex::new(false));
        let part = mount(
            &root,
            &checks,
            vec![(
                "alpha",
                check(
                    &log,
                    "alpha",
                    Answer::Gate {
                        entered: entered.clone(),
                        cancelled: cancelled.clone(),
                    },
                ),
            )],
        )
        .await;
        let cancel = CancellationToken::new();
        let running = tokio::spawn({
            let checks = checks.clone();
            let cancel = cancel.clone();
            async move { checks.check(&request(), &cancel).await }
        });
        entered.notified().await;
        cancel.cancel();
        let outcome = tokio::time::timeout(std::time::Duration::from_secs(5), running)
            .await
            .expect("the interrupted call did not return");
        assert_eq!(outcome.unwrap(), Err(CheckInterrupted));
        assert!(
            *cancelled.lock().unwrap(),
            "the check did not see its token cancelled"
        );
        part.shutdown().await.unwrap();
        root.shutdown().await.unwrap();
    }

    /// A.6.2: a name belongs to one check; a second registration of the same
    /// name fails, and the name is free once that part unloads.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_second_check_under_a_name_fails_until_its_part_unloads() {
        struct DuplicatePart {
            checks: Arc<SubmitChecks>,
            second: Arc<Mutex<Option<bool>>>,
        }
        impl Plugin for DuplicatePart {
            type Deps = ();
            fn name(&self) -> &str {
                "DuplicateChecks"
            }
            async fn apply(&self, ctx: &Ctx, (): Self::Deps) -> rutis::Result {
                let check: Arc<dyn CheckSubmission> = Arc::new(TestCheck {
                    label: "dup",
                    log: Arc::new(Mutex::new(Vec::new())),
                    answer: proceed(&[]),
                });
                self.checks.register(ctx, "dup", check.clone())?;
                *self.second.lock().unwrap() =
                    Some(self.checks.register(ctx, "dup", check).is_err());
                Ok(())
            }
        }
        let root = Ctx::root().unwrap();
        let checks = Arc::new(SubmitChecks::default());
        let second = Arc::new(Mutex::new(None));
        let part = root.plugin(DuplicatePart {
            checks: checks.clone(),
            second: second.clone(),
        });
        (&part).await.unwrap();
        assert_eq!(
            *second.lock().unwrap(),
            Some(true),
            "a second duplicate name registered"
        );
        part.shutdown().await.unwrap();

        // The unloaded part's registration is withdrawn, so the name is free.
        let log = Arc::new(Mutex::new(Vec::new()));
        let again = mount(
            &root,
            &checks,
            vec![("dup", check(&log, "dup", proceed(&[])))],
        )
        .await;
        assert_eq!(
            checks.check(&request(), &CancellationToken::new()).await,
            Ok(SubmitDecision::Proceed {
                blocks: Vec::new(),
                warnings: Vec::new(),
            })
        );
        assert_eq!(*log.lock().unwrap(), ["dup"]);
        again.shutdown().await.unwrap();
        root.shutdown().await.unwrap();
    }

    /// A.6.2: unloading the registering part withdraws its checks; the entry
    /// then writes unchecked.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_unloaded_part_withdraws_its_checks() {
        let root = Ctx::root().unwrap();
        let checks = Arc::new(SubmitChecks::default());
        let log = Arc::new(Mutex::new(Vec::new()));
        let part = mount(
            &root,
            &checks,
            vec![(
                "alpha",
                check(&log, "alpha", Answer::Block("no prompts".into())),
            )],
        )
        .await;
        assert_eq!(
            checks.check(&request(), &CancellationToken::new()).await,
            Ok(SubmitDecision::Block {
                message: "no prompts".into()
            })
        );
        part.shutdown().await.unwrap();
        assert_eq!(
            checks.check(&request(), &CancellationToken::new()).await,
            Ok(SubmitDecision::Proceed {
                blocks: Vec::new(),
                warnings: Vec::new(),
            })
        );
        assert_eq!(
            *log.lock().unwrap(),
            ["alpha"],
            "the withdrawn check still ran"
        );
        root.shutdown().await.unwrap();
    }
}

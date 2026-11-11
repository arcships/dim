mod pending;

use crate::{
    host::HostFuture,
    methods::{Interruption, MethodCall, MethodDecl, RpcError, SessionMethods, params, reply},
    timeline_tree::TreeCommands,
};
use dim_agent_runtime::{
    AgentError, Dispatcher, InputSnapshot, Names, QueryFilter, QueryOperand, QueryOutput,
    QueryPage, QuerySelect, QuerySpec, QueryValue, Registrations, Result, StateCell, Timeline,
    TimelineQuery, TimelineQueryScope,
};
use dim_agent_types::*;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, Key, TypeKey};
use rutis::{Ctx, Error};
use std::{
    collections::HashMap,
    num::NonZeroU64,
    sync::{Arc, Mutex},
};
use tokio_util::sync::CancellationToken;

/// What InputQueue writes for an input it decided on.
#[derive(Debug, Clone)]
enum Decision {
    Apply(String, InputMode),
    Delete(String, Option<InputMessageData>),
}
impl Decision {
    fn input_id(&self) -> &str {
        match self {
            Self::Apply(id, _) | Self::Delete(id, _) => id,
        }
    }
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
                self.entries.retain(|entry| entry.id != data.input_id);
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
                self.entries.remove(index);
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
    timeline: Arc<Timeline>,
    /// Answers to `input/withdraw` commands by their sequence, until the
    /// withdrawal that wrote each one collects it.
    answers: Mutex<HashMap<u64, WithdrawalAnswer>>,
    answered: tokio::sync::Notify,
    /// Ends waits when this generation unloads.
    lifecycle: CancellationToken,
}
impl InputQueue {
    fn publish(&self, state: &QueueState) {
        self.observable.set(InputSnapshot {
            queued: state
                .entries
                .iter()
                .filter(|entry| matches!(&entry.kind, EntryKind::Input { .. }))
                .count(),
        });
    }
    fn write(&self, decisions: Vec<Decision>) -> Result<WithdrawalAnswer> {
        let mut answer = (Withdrawal::NotHeld, None);
        for decision in decisions {
            match decision {
                Decision::Apply(input_id, mode) => {
                    self.timeline.append(
                        EventKind::InputApplied(InputAppliedData {
                            input_id,
                            mode: Some(mode),
                        })
                        .into(),
                    )?;
                }
                Decision::Delete(input_id, steer) => {
                    let mut drafts = Vec::new();
                    if let Some(input) = steer {
                        drafts.push(EventKind::InputMessage(input).into());
                    }
                    drafts.push(EventKind::InputDeleted(InputDeletedData { input_id }).into());
                    let mut events = self.timeline.append_batch_registered(drafts, |_| Ok(()))?;
                    let deletion = events
                        .pop()
                        .expect("a withdrawal result ends with deletion");
                    answer = (
                        Withdrawal::Deleted {
                            sequence: deletion.sequence,
                        },
                        events.pop(),
                    );
                }
            }
        }
        Ok(answer)
    }
    async fn rebuild(&self) -> Result<u64> {
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
                    values: [
                        "input/message",
                        "input/consumed",
                        "input/withdraw",
                        "input/applied",
                        "input/deleted",
                        "turn/start",
                        "turn/end",
                        "stop/interrupt",
                    ]
                    .map(serde_json::Value::from)
                    .to_vec(),
                }),
                select: QuerySelect::Fields {
                    fields: [
                        "kind",
                        "id",
                        "data.mode",
                        "data.user.content[].type",
                        "data.input_id",
                        "data.turn_id",
                        "data.handover.turn_id",
                        "data.steer",
                    ]
                    .map(String::from)
                    .to_vec(),
                },
                group_by: Vec::new(),
                pick: None,
                order_by: Vec::new(),
            },
            page: Some(QueryPage {
                limit: NonZeroU64::new(256).expect("a page contains queue transitions"),
                after: None,
            }),
        };
        let mut state = QueueState::default();
        let mut owed = Vec::<Decision>::new();
        loop {
            let reply = self.timeline.query(request.clone()).await?;
            let QueryOutput::Rows { rows } = reply.result else {
                unreachable!("queue recovery selects transition fields")
            };
            for row in rows {
                let Ok(
                    [
                        QueryValue::Value {
                            value: serde_json::Value::String(kind),
                        },
                        QueryValue::Value {
                            value: serde_json::Value::String(id),
                        },
                        mode,
                        user,
                        input,
                        turn,
                        handover,
                        steer,
                    ],
                ) = <[_; 8]>::try_from(row.values)
                else {
                    return Err(AgentError::fault(
                        "InputQueue",
                        "a queue transition has no kind or identity",
                    ));
                };
                match kind.as_str() {
                    "input/message" => {
                        let mode = match mode {
                            QueryValue::Missing if matches!(user, QueryValue::Missing) => {
                                InputMode::Append
                            }
                            QueryValue::Missing => InputMode::Queue,
                            QueryValue::Value { value } => {
                                serde_json::from_value(value).map_err(|error| {
                                    AgentError::fault("InputQueue", error.to_string())
                                })?
                            }
                            QueryValue::Elements { .. } => unreachable!("an input mode is scalar"),
                        };
                        let handover = match &handover {
                            QueryValue::Missing => None,
                            QueryValue::Value { value } if value.is_null() => None,
                            QueryValue::Value { value } => {
                                Some(value.as_str().expect("handover turn identity"))
                            }
                            QueryValue::Elements { .. } => {
                                unreachable!("handover turn identity is scalar")
                            }
                        };
                        owed.extend(state.input(&id, mode, handover));
                    }
                    "turn/start" => {
                        let QueryValue::Value { value: turn } = turn else {
                            unreachable!("turn identity")
                        };
                        state.turn = Some(turn.as_str().expect("turn identity").to_owned());
                        state.starting = false;
                    }
                    "turn/end" => {
                        let QueryValue::Value { value: turn } = turn else {
                            unreachable!("turn identity")
                        };
                        owed.extend(state.end_turn(turn.as_str().expect("turn identity")));
                    }
                    "stop/interrupt" => state.starting = false,
                    "input/consumed" | "input/withdraw" | "input/applied" | "input/deleted" => {
                        let QueryValue::Value {
                            value: serde_json::Value::String(input_id),
                        } = input
                        else {
                            return Err(AgentError::fault(
                                "InputQueue",
                                "a queue transition has no input identity",
                            ));
                        };
                        match kind.as_str() {
                            "input/consumed" => {
                                state.entries.retain(|entry| entry.id != input_id);
                                owed.retain(|decision| !matches!(decision, Decision::Apply(id, _) if *id == input_id));
                            }
                            "input/withdraw" => {
                                let steer = match steer {
                                    QueryValue::Missing => None,
                                    QueryValue::Value { value } => {
                                        Some(serde_json::from_value(value).map_err(|error| {
                                            AgentError::fault("InputQueue", error.to_string())
                                        })?)
                                    }
                                    QueryValue::Elements { .. } => {
                                        unreachable!("a withdrawal candidate is an object")
                                    }
                                };
                                owed.extend(state.withdraw(&input_id, steer));
                            }
                            _ => owed.retain(|decision| decision.input_id() != input_id),
                        }
                    }
                    _ => unreachable!("queue recovery selects only queue transitions"),
                }
            }
            let Some(next) = reply.next else {
                break;
            };
            request
                .page
                .as_mut()
                .expect("queue recovery is paged")
                .after = Some(next);
        }
        owed.retain(|decision| match decision {
            Decision::Apply(id, _) => state.entries.iter().any(|entry| entry.id == *id),
            Decision::Delete(_, _) => true,
        });
        self.publish(&state);
        *self.state.lock().unwrap() = state;
        self.write(owed)?;
        Ok(through)
    }
    fn on_event(&self, event: Arc<AgentEvent>) -> Result<()> {
        let decisions = {
            let mut state = self.state.lock().unwrap();
            let decisions = state.step(&event);
            self.publish(&state);
            decisions
        };
        let answer = self.write(decisions)?;
        if matches!(event.kind, EventKind::InputWithdraw(_)) {
            self.answers.lock().unwrap().insert(event.sequence, answer);
            self.answered.notify_waiters();
        }
        Ok(())
    }
    /// Withdraws an input by its `input_id` and replies with this queue's
    /// decision for the command. Only an accepted withdrawal adopts its
    /// optional checked Steer candidate, atomically with the deletion.
    /// An accepted Steer replies after that result batch is published.
    pub async fn withdraw(&self, command: InputWithdrawData) -> Result<WithdrawalAnswer> {
        command
            .validate_external()
            .map_err(|reason| AgentError::InvalidInput(reason.into()))?;
        let sequence = self
            .timeline
            .append(EventKind::InputWithdraw(command).into())?
            .sequence;
        loop {
            let answered = self.answered.notified();
            let answer = self.answers.lock().unwrap().remove(&sequence);
            if let Some(answer) = answer {
                if let (Withdrawal::Deleted { sequence }, Some(_)) = &answer {
                    let mut publication = self.timeline.watch();
                    tokio::select! {
                        result = publication.wait_for(|&(published, closed)| published >= *sequence || closed) => {
                            if !matches!(result, Ok(head) if head.0 >= *sequence) {
                                return Err(AgentError::Storage("withdrawal result publication was not confirmed; query history after recovery".into()));
                            }
                        }
                        _ = self.lifecycle.cancelled() => {
                            return Err(AgentError::Reloading("InputQueue".into()));
                        }
                    }
                }
                return Ok(answer);
            }
            tokio::select! {
                _ = answered => {}
                _ = self.lifecycle.cancelled() => {
                    return Err(AgentError::Reloading("InputQueue".into()));
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
                timeline,
                answers: Mutex::new(HashMap::new()),
                answered: tokio::sync::Notify::new(),
                lifecycle: ctx.cancellation_token(),
            });
            let owner = queue.clone();
            let read = queue.clone();
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
                    ]
                    .map(String::from)
                    .to_vec(),
                    Arc::new(move || {
                        let read = read.clone();
                        Box::pin(async move { read.rebuild().await })
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
    /// content, in name order.
    Proceed { blocks: Vec<String> },
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
    /// order is the answer; otherwise every check's blocks are returned in
    /// name order. `Err` means the entry's own call was interrupted while a
    /// check ran, and nothing may be written.
    pub async fn check(
        &self,
        request: &SubmitCheckRequest,
        cancel: &CancellationToken,
    ) -> std::result::Result<SubmitCheckReply, CheckInterrupted> {
        let mut blocks = Vec::new();
        for registration in self.checks.all() {
            if cancel.is_cancelled() {
                return Err(CheckInterrupted);
            }
            match registration
                .value
                .check(request.clone(), cancel.clone())
                .await
            {
                Ok(SubmitCheckReply::Proceed { blocks: added }) => blocks.extend(added),
                Ok(SubmitCheckReply::Block { message }) => {
                    return Ok(SubmitCheckReply::Block { message });
                }
                Err(_) if cancel.is_cancelled() => return Err(CheckInterrupted),
                Err(_) => {}
            }
        }
        if cancel.is_cancelled() {
            return Err(CheckInterrupted);
        }
        Ok(SubmitCheckReply::Proceed { blocks })
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
    /// Re-submits the content as Steer in the same batch as the withdrawal.
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
                        let queue = tree.get(&p.branch_id)?.service::<InputQueue>()?;
                        let mut steer = p.steer.map(|user| InputMessageData {
                            user: Some(user),
                            mode: Some(InputMode::Steer),
                            ..Default::default()
                        });
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
                                Ok(SubmitCheckReply::Block { message }) => {
                                    return Err(RpcError::app(-32000, "input_blocked", message));
                                }
                                Ok(SubmitCheckReply::Proceed { blocks }) => {
                                    if let Some(steer) = &mut steer {
                                        append_blocks(&mut steer.user, blocks);
                                    }
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
                                serde_json::json!({"status": "deleted", "sequence": sequence})
                            }
                            Withdrawal::NotHeld => serde_json::json!({"status": "notHeld"}),
                        };
                        if let Some(event) = replacement {
                            result["steer"] = serde_json::json!({"inputId": event.id, "sequence": event.sequence});
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
                    Box::pin(async move { Ok(SubmitCheckReply::Proceed { blocks }) })
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
    fn proceed(blocks: &[&str]) -> Answer {
        Answer::Proceed(blocks.iter().map(|block| (*block).to_owned()).collect())
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
            Ok(SubmitCheckReply::Block {
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
            Ok(SubmitCheckReply::Proceed {
                blocks: vec!["1".into(), "2".into(), "3".into()]
            })
        );
        assert_eq!(*log.lock().unwrap(), ["alpha", "mid", "zeta"]);
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
            Ok(SubmitCheckReply::Proceed {
                blocks: vec!["b".into()]
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
            Ok(SubmitCheckReply::Proceed { blocks: Vec::new() })
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
            Ok(SubmitCheckReply::Block {
                message: "no prompts".into()
            })
        );
        part.shutdown().await.unwrap();
        assert_eq!(
            checks.check(&request(), &CancellationToken::new()).await,
            Ok(SubmitCheckReply::Proceed { blocks: Vec::new() })
        );
        assert_eq!(
            *log.lock().unwrap(),
            ["alpha"],
            "the withdrawn check still ran"
        );
        root.shutdown().await.unwrap();
    }
}

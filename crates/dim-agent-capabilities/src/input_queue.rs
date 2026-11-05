use crate::{
    host::HostFuture,
    methods::{Interruption, MethodDecl, RpcError, SessionMethods, params, reply},
    timeline_tree::TreeCommands,
};
use dim_agent_runtime::{
    AgentError, Dispatcher, InputSnapshot, Names, Registrations, Result, StateCell, Timeline,
    TimelineQuery, TimelineQueryDirection, TimelineQueryOrderBy, TimelineQueryScope,
};
use dim_agent_types::*;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, Key, TypeKey};
use rutis::{Ctx, Error};
use std::{
    collections::{HashMap, HashSet},
    num::NonZeroU64,
    sync::{Arc, Mutex},
};
use tokio_util::sync::CancellationToken;

/// What InputQueue writes for an input it decided on.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Decision {
    Apply(String, InputMode),
    Delete(String),
}
impl Decision {
    fn input_id(&self) -> &str {
        match self {
            Self::Apply(id, _) | Self::Delete(id) => id,
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

/// One committed input until Loop consumes it.
struct Entry {
    id: String,
    mode: InputMode,
    /// This queue decided to write its `input/applied`.
    released: bool,
}
/// What InputQueue derives from the events it consumes.
#[derive(Default)]
struct QueueState {
    entries: Vec<Entry>,
    /// Between a dispatched `turn/start` and its `turn/end`.
    turn_open: bool,
    /// An input released while no turn ran is about to start one: until
    /// its `turn/start`, queue inputs wait as if the turn ran. A
    /// `stop/interrupt` ends the wait, since inputs applied before it no
    /// longer start a turn.
    starting: bool,
}
impl QueueState {
    /// Folds one event; returns the decisions to write now, in order.
    fn step(&mut self, event: &AgentEvent) -> Vec<Decision> {
        match &event.kind {
            EventKind::InputMessage(data) => {
                let mode = data.effective_mode();
                self.entries.push(Entry {
                    id: event.id.clone(),
                    mode,
                    released: false,
                });
                let held_queue = self.entries.iter().any(|entry| {
                    !entry.released && entry.mode == InputMode::Queue && entry.id != event.id
                });
                match mode {
                    InputMode::Queue if self.turn_open || self.starting || held_queue => Vec::new(),
                    _ => self.release(&event.id),
                }
            }
            EventKind::TurnStart(_) => {
                self.turn_open = true;
                self.starting = false;
                Vec::new()
            }
            EventKind::StopInterrupt(_) => {
                self.starting = false;
                Vec::new()
            }
            EventKind::TurnEnd(_) => {
                self.turn_open = false;
                match self
                    .entries
                    .iter()
                    .find(|entry| !entry.released && entry.mode == InputMode::Queue)
                {
                    Some(next) => {
                        let next = next.id.clone();
                        self.release(&next)
                    }
                    None => Vec::new(),
                }
            }
            EventKind::InputConsumed(data) => {
                self.entries.retain(|entry| entry.id != data.input_id);
                Vec::new()
            }
            EventKind::InputWithdraw(data) => {
                match self
                    .entries
                    .iter()
                    .position(|entry| entry.id == data.input_id && !entry.released)
                {
                    Some(index) => {
                        self.entries.remove(index);
                        vec![Decision::Delete(data.input_id.clone())]
                    }
                    None => Vec::new(),
                }
            }
            _ => Vec::new(),
        }
    }
    /// Releases one input. One that drives a turn while none runs makes a
    /// turn about to start.
    fn release(&mut self, id: &str) -> Vec<Decision> {
        let Some(entry) = self.entries.iter_mut().find(|entry| entry.id == id) else {
            return Vec::new();
        };
        entry.released = true;
        if !self.turn_open && entry.mode != InputMode::Append {
            self.starting = true;
        }
        vec![Decision::Apply(entry.id.clone(), entry.mode)]
    }
}
/// Consumes `input/message` and decides by its mode when Loop may consume
/// it, writing `input/applied` with the mode then: steer and append inputs
/// at once; a queue input when no turn runs or is about to start,
/// otherwise after the turn ends, one per turn. Knows of turns only through `turn/start`,
/// `turn/end` and `stop/interrupt`; Loop decides how to consume what is
/// applied. Consumes `input/withdraw` in order and writes `input/deleted`
/// for an input it still holds unapplied.
pub struct InputQueue {
    state: Mutex<QueueState>,
    observable: Arc<StateCell<InputSnapshot>>,
    timeline: Arc<Timeline>,
    /// Answers to `input/withdraw` commands by their sequence, until the
    /// withdrawal that wrote each one collects it.
    answers: Mutex<HashMap<u64, Withdrawal>>,
    answered: tokio::sync::Notify,
    /// Ends waits when this generation unloads.
    lifecycle: CancellationToken,
}
impl InputQueue {
    fn publish(&self, state: &QueueState) {
        self.observable.set(InputSnapshot {
            queued: state.entries.len(),
        });
    }
    /// Writes the decisions; returns the sequence of the last `input/deleted`.
    fn write(&self, decisions: Vec<Decision>) -> Result<Option<u64>> {
        let mut deleted = None;
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
                Decision::Delete(input_id) => {
                    let event = self
                        .timeline
                        .append(EventKind::InputDeleted(InputDeletedData { input_id }).into())?;
                    deleted = Some(event.sequence);
                }
            }
        }
        Ok(deleted)
    }
    /// Replays the same decisions; those the previous generation made but
    /// did not write before it stopped are written now.
    fn rebuild(&self, events: &[Arc<AgentEvent>]) -> Result<()> {
        let owed = {
            let mut state = self.state.lock().unwrap();
            let mut decided = Vec::new();
            let mut written = HashSet::new();
            for event in events {
                match &event.kind {
                    EventKind::InputApplied(data) => {
                        written.insert(data.input_id.clone());
                    }
                    EventKind::InputDeleted(data) => {
                        written.insert(data.input_id.clone());
                    }
                    _ => {}
                }
                decided.extend(state.step(event));
            }
            self.publish(&state);
            decided
                .into_iter()
                .filter(|decision| !written.contains(decision.input_id()))
                .filter(|decision| match decision {
                    Decision::Apply(id, _) => state.entries.iter().any(|entry| entry.id == *id),
                    Decision::Delete(_) => true,
                })
                .collect::<Vec<_>>()
        };
        self.write(owed)?;
        Ok(())
    }
    fn on_event(&self, event: Arc<AgentEvent>) -> Result<()> {
        let decisions = {
            let mut state = self.state.lock().unwrap();
            let decisions = state.step(&event);
            self.publish(&state);
            decisions
        };
        let deleted = self.write(decisions)?;
        if matches!(event.kind, EventKind::InputWithdraw(_)) {
            let answer = match deleted {
                Some(sequence) => Withdrawal::Deleted { sequence },
                None => Withdrawal::NotHeld,
            };
            self.answers.lock().unwrap().insert(event.sequence, answer);
            self.answered.notify_waiters();
        }
        Ok(())
    }
    /// Withdraws an input by its `input_id` and replies with this queue's
    /// decision for the command. With `steer`, the command and the new Steer
    /// input are one batch: the withdrawal takes effect only if the old
    /// input is still held, the new input is written either way.
    pub async fn withdraw(
        &self,
        command: InputWithdrawData,
        steer: Option<InputMessageData>,
    ) -> Result<(Withdrawal, Option<Arc<AgentEvent>>)> {
        command
            .validate_external()
            .map_err(|reason| AgentError::InvalidInput(reason.into()))?;
        let mut drafts = vec![EventKind::InputWithdraw(command).into()];
        if let Some(input) = steer {
            if input.mode != Some(InputMode::Steer) {
                return Err(AgentError::InvalidInput(
                    "a replacement input must be Steer".into(),
                ));
            }
            input
                .validate()
                .map_err(|reason| AgentError::InvalidInput(reason.into()))?;
            drafts.push(EventKind::InputMessage(input).into());
        }
        let mut events = self.timeline.append_batch_registered(drafts, |_| Ok(()))?;
        let replacement = (events.len() > 1).then(|| events.remove(1));
        let sequence = events[0].sequence;
        loop {
            let answered = self.answered.notified();
            if let Some(answer) = self.answers.lock().unwrap().remove(&sequence) {
                return Ok((answer, replacement));
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
            let read_timeline = queue.timeline.clone();
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
                        let timeline = read_timeline.clone();
                        Box::pin(async move {
                            let reply = timeline
                                .query_all(TimelineQuery {
                                    scope: TimelineQueryScope::Branch,
                                    include_publishing: true,
                                    from_event: None,
                                    from_sequence: None,
                                    order_by: TimelineQueryOrderBy::Sequence,
                                    direction: TimelineQueryDirection::Ascending,
                                    kinds: vec![
                                        "input/message".into(),
                                        "input/consumed".into(),
                                        "input/withdraw".into(),
                                        "input/applied".into(),
                                        "input/deleted".into(),
                                        "turn/start".into(),
                                        "turn/end".into(),
                                        "stop/interrupt".into(),
                                    ],
                                    flags: Vec::new(),
                                    event_id: None,
                                    predicate: None,
                                    limit: NonZeroU64::new(1024)
                                        .expect("a page keeps one fact"),
                                    offset: 0,
                                })
                                .await?;
                            read.rebuild(&reply.events)?;
                            Ok(reply.through)
                        })
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
/// A.6.2); an input with no user content takes none.
pub fn append_blocks(user: &mut Option<UserInput>, blocks: Vec<String>) {
    let Some(user) = user else {
        if !blocks.is_empty() {
            tracing::warn!("submit check contexts dropped: the input has no user content");
        }
        return;
    };
    user.content
        .extend(blocks.into_iter().map(|text| ContentBlock::Text { text }));
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
                vec![MethodDecl::new(
                    "session.input.withdraw",
                    true,
                    Interruption::Report,
                )],
                Arc::new(move |_, value, cancel, _| {
                    let tree = tree.clone();
                    let checks = checks.clone();
                    Box::pin(async move {
                        let p: WithdrawParams = params(value)?;
                        let queue = tree.get(&p.branch_id)?.service::<InputQueue>()?;
                        let mut steer = p.steer.map(|user| InputMessageData {
                            user: Some(user),
                            mode: Some(InputMode::Steer),
                            ..Default::default()
                        });
                        if let Some(input) = &steer {
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
                                },
                                steer,
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

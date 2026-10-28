//! The Loop's compaction step and the record contract (ContextCompaction
//! §5.3–§5.8, acceptance 3–6 and 8 at the capability level).
use super::context_reads::TASK_READS;
use super::*;
use dim_agent_capabilities::{
    agent_loop::AgentLoop,
    history::{context_messages, read_context, summary_message},
};
use rutis::host::CtxHost;
use rutis::host::ErrorKind;
use std::sync::atomic::AtomicBool;

#[derive(Clone, Copy, Default, PartialEq)]
enum Mode {
    /// The legal cut within the retention limit, one summary message.
    #[default]
    Cut,
    /// The same cut with a two-message replacement.
    TwoMessages,
    /// A cut after the limit: inside the retained tail.
    InsideRetained,
    /// The first call fails; later calls cut.
    FailFirst,
}

/// A test strategy that can hold its next call until released or cancelled.
#[derive(Default)]
struct Scripted {
    mode: Mode,
    calls: AtomicUsize,
    hold: AtomicBool,
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
}
impl Scripted {
    fn new(mode: Mode) -> Arc<Self> {
        Arc::new(Self {
            mode,
            ..Self::default()
        })
    }
}
impl CompactionStrategy for Scripted {
    fn name(&self) -> &str {
        "scripted"
    }
    fn compact(
        &self,
        input: CompactionInput,
        cancel: CancellationToken,
    ) -> HostFuture<'_, StrategyOutcome> {
        Box::pin(async move {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            if self.hold.swap(false, Ordering::SeqCst) {
                self.entered.notify_one();
                tokio::select! {
                    _ = cancel.cancelled() => {
                        return Err(HostError::Cancelled("strategy cancelled".into()));
                    }
                    _ = self.release.notified() => {}
                }
            }
            if self.mode == Mode::FailFirst && call == 0 {
                return Err(HostError::failed("summarizer unavailable"));
            }
            let cut = match self.mode {
                Mode::InsideRetained => input.cuts.last(),
                _ => input.cut(),
            };
            let Some(cut) = cut else {
                return Ok(StrategyOutcome::Skipped {
                    reason: "nothing_to_compact".into(),
                    diagnostics: test_diagnostics(),
                });
            };
            let replacement = match self.mode {
                Mode::TwoMessages => vec![
                    Message::text(Role::User, "first replacement"),
                    Message::text(Role::User, "second replacement"),
                ],
                _ => vec![summary_message("short")],
            };
            Ok(StrategyOutcome::Completed {
                retain_from: cut.retain_from.clone(),
                replacement,
                diagnostics: test_diagnostics(),
            })
        })
    }
}

/// Emits one `echo` call for an input that asks for it, holding it until
/// released when armed; echoes otherwise.
#[derive(Default)]
struct ToolOnRequest {
    hold: AtomicBool,
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
    text: std::sync::Mutex<String>,
}
impl ModelDriver for ToolOnRequest {
    fn invoke(
        &self,
        request: Arc<ModelRequest>,
        connection: ModelConnection,
        cancel: CancellationToken,
    ) -> HostFuture<'_, ModelStream> {
        let asks = matches!(&request.messages.last().unwrap().content,
            MessageContent::Text(text) if text == "use the tool");
        Box::pin(async move {
            if !asks {
                return EchoModel.invoke(request, connection, cancel).await;
            }
            if self.hold.swap(false, Ordering::SeqCst) {
                self.entered.notify_one();
                self.release.notified().await;
            }
            let text = self.text.lock().unwrap().clone();
            Ok(Box::pin(futures_util::stream::iter(vec![
                Ok(ModelChunk::MessageStart {
                    message_id: "tool".into(),
                }),
                Ok(ModelChunk::ToolStart {
                    id: "echo-call".into(),
                    name: "echo".into(),
                }),
                Ok(ModelChunk::ToolUse(ToolCall {
                    id: "echo-call".into(),
                    name: "echo".into(),
                    input: serde_json::from_value(serde_json::json!({ "text": text })).unwrap(),
                })),
                Ok(ModelChunk::MessageStop {
                    message_id: "tool".into(),
                    reason: ModelStopReason::ToolUse,
                }),
            ])) as ModelStream)
        })
    }
}

/// Provides the strategy from a child plugin; restarting it reloads the
/// Loop and ContextPreparation of every branch.
#[derive(Clone, Default)]
struct StrategyReload(Arc<std::sync::Mutex<Vec<rutis::host::FiberView>>>);
struct StrategyOwner {
    key: rutis::host::Key<CompactionStrategyHost>,
    strategy: Arc<Scripted>,
    views: Arc<std::sync::Mutex<Vec<rutis::host::FiberView>>>,
}
impl Plugin for StrategyOwner {
    type Deps = ();
    fn name(&self) -> &str {
        "test.strategy-owner"
    }
    async fn apply(&self, ctx: &Ctx, (): Self::Deps) -> rutis::Result {
        let view = ctx.plugin(StrategyProvider(self.key.clone(), self.strategy.clone()));
        (&view).await.map_err(|error| {
            rutis::Error::from(ErrorKind::PluginFailed(
                format!("Strategy provider failed: {error}").into(),
            ))
        })?;
        self.views.lock().unwrap().push(view);
        Ok(())
    }
}
struct StrategyProvider(rutis::host::Key<CompactionStrategyHost>, Arc<Scripted>);
impl Plugin for StrategyProvider {
    type Deps = ();
    fn name(&self) -> &str {
        "test.strategy"
    }
    async fn apply(&self, ctx: &Ctx, (): Self::Deps) -> rutis::Result {
        ctx.provide_at(
            self.0.clone(),
            Arc::new(CompactionStrategyHost(Some(self.1.clone()))),
        )?;
        Ok(())
    }
}

/// No automatic compaction (a high threshold); `strategy` serves manual
/// steps and budget recovery. With `reload`, the strategy is provided by a
/// restartable plugin.
fn builder_with_strategy(
    model: Arc<dyn ModelDriver>,
    history: HistoryRenderer,
    strategy: Arc<Scripted>,
    reload: Option<&StrategyReload>,
) -> AgentBuilder {
    let mut builder = AgentBuilder::new(AgentConfig::default());
    builder
        .process_value(Arc::new(ModelHost(model)))
        .session_value(Arc::new(ToolHost(Arc::new(MockTools))))
        .session_value(Arc::new(history))
        .session_value(Arc::new(PrepareContextHost(Arc::new(IdentityContext))))
        .session_value(Arc::new(ContextModelHost(Some(Arc::new(
            Context::default(),
        )))))
        .require_feature(
            Feature::new("PrepareModelHost").session("PrepareModelHost", |names| {
                Ok(Box::new(DefaultPrepareModelPlugin::new(names)?))
            }),
        );
    match reload {
        Some(reload) => {
            let views = reload.0.clone();
            builder.require_feature(Feature::new("test.strategy-owner").session(
                "test.strategy-owner",
                move |names| {
                    Ok(Box::new(StrategyOwner {
                        key: names.key()?,
                        strategy: strategy.clone(),
                        views: views.clone(),
                    }))
                },
            ));
        }
        None => {
            builder.session_value(Arc::new(CompactionStrategyHost(Some(strategy))));
        }
    }
    defaults::install_with_prompt(
        &mut builder,
        Arc::new(EmptyPromptHost),
        Some(ConfigureUpdate {
            sources: Vec::new(),
            model: Some(ModelSelection {
                provider_id: "mock".into(),
                model_id: "echo".into(),
                capability_snapshot: None,
                config: None,
                metadata: None,
            }),
        }),
    );
    builder
}

/// A first turn long enough that replacing it shrinks the request.
fn first() -> String {
    "first ".repeat(100)
}

async fn turns(session: &Session, texts: &[&str]) {
    for text in texts {
        session
            .submit((*text).into(), InputMode::Queue, "main")
            .await
            .unwrap();
        session.idle("main").await.unwrap();
    }
}

/// Waits until the branch history satisfies `done`.
async fn until(
    session: &Session,
    done: impl Fn(&[Arc<AgentEvent>]) -> bool,
) -> Vec<Arc<AgentEvent>> {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let events = session.history("main", None).await.unwrap();
            if done(&events) {
                return events;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the expected facts are committed")
}

/// The result of `command`: the one that carries its command id.
fn result_for(events: &[Arc<AgentEvent>], command: u64) -> Option<&ContextCompactionData> {
    events.iter().find_map(|event| match &event.kind {
        EventKind::ContextCompaction(data) if data.command == Some(command) => Some(data),
        _ => None,
    })
}

/// How many results carry `command`.
fn results_of(events: &[Arc<AgentEvent>], command: u64) -> usize {
    events
        .iter()
        .filter(|event| {
            matches!(&event.kind,
            EventKind::ContextCompaction(data) if data.command == Some(command))
        })
        .count()
}

/// Waits until the Loop has consumed the event at `sequence`.
async fn consumed(session: &Session, sequence: u64) {
    let mut dispatch = session.dispatch_watch("main").unwrap();
    while dispatch.borrow().sequence < sequence {
        dispatch.changed().await.unwrap();
    }
}

fn starts_of(events: &[Arc<AgentEvent>]) -> Vec<Option<u64>> {
    events
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::ContextCompactionStart(data) => Some(data.command),
            _ => None,
        })
        .collect()
}

/// Every start has exactly one result with its `start` and the same ids,
/// and no result names a start that does not exist (acceptance 8).
pub(crate) fn assert_paired(events: &[Arc<AgentEvent>]) {
    for event in events {
        match &event.kind {
            EventKind::ContextCompactionStart(start) => {
                let results: Vec<_> = events
                    .iter()
                    .filter_map(|other| match &other.kind {
                        EventKind::ContextCompaction(data)
                            if data.start == Some(event.sequence) =>
                        {
                            Some(data)
                        }
                        _ => None,
                    })
                    .collect();
                assert_eq!(results.len(), 1, "start {}", event.sequence);
                let result = results[0];
                assert_eq!(
                    (
                        &result.turn_id,
                        &result.request_id,
                        result.command,
                        result.through
                    ),
                    (
                        &start.turn_id,
                        &start.request_id,
                        start.command,
                        start.through
                    )
                );
            }
            EventKind::ContextCompaction(ContextCompactionData {
                start: Some(start), ..
            }) => assert!(events.iter().any(|other| other.sequence == *start
                && matches!(other.kind, EventKind::ContextCompactionStart(_)))),
            _ => {}
        }
    }
}

/// The messages a preparation at the head would start from.
async fn projected(timeline_session: &Session, branch: &str) -> Vec<Message> {
    let through = timeline_session.sequence(branch).unwrap();
    let context = timeline_session.context(branch, through).await.unwrap();
    let history = project_context_history(&context).unwrap();
    context_messages(&context, &history)
}

/// Acceptance 3: with no turn, `context/compact` is served at once and
/// writes only the command, the start and the result.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_manual_compaction_with_no_turn_writes_only_its_facts() {
    let strategy = Scripted::new(Mode::Cut);
    let agent = builder_with_strategy(
        Arc::new(EchoModel),
        HistoryRenderer::default(),
        strategy.clone(),
        None,
    )
    .build();
    let session = agent
        .create_session(SessionConfig::default())
        .await
        .unwrap();
    turns(&session, &[first().as_str(), "second", "third"]).await;
    let command = session
        .service::<AgentLoop>("main")
        .unwrap()
        .compact()
        .unwrap();
    let events = until(&session, |events| result_for(events, command).is_some()).await;
    let after: Vec<_> = events
        .iter()
        .filter(|event| event.sequence >= command)
        .collect();
    assert_eq!(after.len(), 3, "{after:#?}");
    let EventKind::ContextCompactionStart(start) = &after[1].kind else {
        panic!("expected the start, got {:?}", after[1].kind)
    };
    assert_eq!(
        start,
        &ContextCompactionStartData {
            turn_id: None,
            request_id: None,
            command: Some(command),
            through: command,
            trigger: CompactionTrigger::Manual,
            strategy: "scripted".into(),
        }
    );
    let EventKind::ContextCompaction(result) = &after[2].kind else {
        panic!("expected the result")
    };
    assert_eq!(
        (result.command, result.start, result.through),
        (Some(command), Some(after[1].sequence), command)
    );
    let ContextCompactionResult::Completed {
        retain_from,
        strategy: name,
        diagnostics,
        ..
    } = &result.outcome
    else {
        panic!("expected completion, got {:?}", result.outcome)
    };
    let starts: Vec<_> = events
        .iter()
        .filter(|event| matches!(event.kind, EventKind::TurnStart(_)))
        .collect();
    assert_eq!(retain_from, &starts[2].id);
    assert_eq!(name, "scripted");
    assert_eq!(diagnostics.reason, CompactionReason::Manual);
    assert_eq!(diagnostics.retain_limit.as_ref(), Some(&starts[2].id));
    turns(&session, &["fourth"]).await;
    let events = session.history("main", None).await.unwrap();
    let request = events
        .iter()
        .rev()
        .find_map(|event| match &event.kind {
            EventKind::ModelCall(data) => Some(&data.request),
            _ => None,
        })
        .unwrap();
    assert_eq!(*request.messages[0], summary_message("short"));
    let text = serde_json::to_string(&request.messages).unwrap();
    assert!(!text.contains("first") && !text.contains("second") && text.contains("third"));
    assert_paired(&events);
    agent.close().await.unwrap();
}

/// Answers every request with a short text.
struct ShortReply;
impl ModelDriver for ShortReply {
    fn invoke(
        &self,
        _: Arc<ModelRequest>,
        _: ModelConnection,
        _: CancellationToken,
    ) -> HostFuture<'_, ModelStream> {
        Box::pin(async {
            Ok(Box::pin(futures_util::stream::iter(vec![
                Ok(ModelChunk::MessageStart {
                    message_id: "reply".into(),
                }),
                Ok(ModelChunk::Text("ok".into())),
                Ok(ModelChunk::MessageStop {
                    message_id: "reply".into(),
                    reason: ModelStopReason::EndTurn,
                }),
            ])) as ModelStream)
        })
    }
}

/// A manual step on a short session compacts at the latest turn's start,
/// or inside that turn when the token rule keeps less.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_manual_compaction_keeps_at_most_the_latest_turn() {
    for (model, latest) in [
        (
            Arc::new(EchoModel) as Arc<dyn ModelDriver>,
            "second".to_owned(),
        ),
        (Arc::new(ShortReply), "y".repeat(1_500)),
    ] {
        let large = latest.len() > 100;
        let agent = builder_with_strategy(
            model,
            HistoryRenderer::default(),
            Scripted::new(Mode::Cut),
            None,
        )
        .build();
        let session = agent
            .create_session(SessionConfig::default())
            .await
            .unwrap();
        turns(&session, &[first().as_str(), latest.as_str()]).await;
        let command = session
            .service::<AgentLoop>("main")
            .unwrap()
            .compact()
            .unwrap();
        let events = until(&session, |events| result_for(events, command).is_some()).await;
        let ContextCompactionResult::Completed { retain_from, .. } =
            &result_for(&events, command).unwrap().outcome
        else {
            panic!("a manual step on two turns compacts")
        };
        let position = |id: &str| events.iter().position(|event| event.id == id).unwrap();
        let starts: Vec<_> = events
            .iter()
            .filter(|event| matches!(event.kind, EventKind::TurnStart(_)))
            .collect();
        if large {
            assert!(position(retain_from) > position(&starts[1].id));
        } else {
            assert_eq!(retain_from, &starts[1].id);
        }
        let messages = projected(&session, "main").await;
        let text = serde_json::to_string(&messages).unwrap();
        assert!(!text.contains("first"));
        assert!(!text.contains("yyyy"));
        assert_eq!(text.contains("second"), !large);
        assert_paired(&events);
        agent.close().await.unwrap();
    }
}

/// Acceptance 4: inside a turn the step waits for the next step boundary,
/// then the turn goes on from the compacted context.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_manual_compaction_inside_a_turn_runs_at_the_next_step_boundary() {
    let model = Arc::new(ToolOnRequest::default());
    *model.text.lock().unwrap() = "tool output".into();
    let agent = builder_with_strategy(
        model.clone(),
        HistoryRenderer::default(),
        Scripted::new(Mode::Cut),
        None,
    )
    .build();
    let session = agent
        .create_session(SessionConfig::default())
        .await
        .unwrap();
    turns(&session, &[first().as_str(), "second"]).await;
    model.hold.store(true, Ordering::SeqCst);
    session
        .submit("use the tool".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    model.entered.notified().await;
    let command = session
        .service::<AgentLoop>("main")
        .unwrap()
        .compact()
        .unwrap();
    let mut dispatch = session.dispatch_watch("main").unwrap();
    while dispatch.borrow().sequence < command {
        dispatch.changed().await.unwrap();
    }
    assert!(
        result_for(&session.history("main", None).await.unwrap(), command).is_none(),
        "no step runs while a request is in flight"
    );
    model.release.notify_one();
    session.idle("main").await.unwrap();
    let events = until(&session, |events| {
        matches!(
            events.last().map(|event| &event.kind),
            Some(EventKind::TurnEnd(_))
        )
    })
    .await;
    let position = |predicate: &dyn Fn(&EventKind) -> bool| {
        events
            .iter()
            .position(|event| predicate(&event.kind))
            .unwrap()
    };
    let turn = match &events[position(&|kind| matches!(kind, EventKind::ToolCall(_)))].kind {
        EventKind::ToolCall(data) => data.turn_id.clone(),
        _ => unreachable!(),
    };
    let result = position(
        &|kind| matches!(kind, EventKind::ToolResult(data) if data.call_event_id.is_some()),
    );
    let start = position(&|kind| matches!(kind, EventKind::ContextCompactionStart(_)));
    let fact = position(&|kind| matches!(kind, EventKind::ContextCompaction(_)));
    let next = events
        .iter()
        .enumerate()
        .skip(fact)
        .find(|(_, event)| matches!(event.kind, EventKind::ModelStart(_)))
        .map(|(index, _)| index)
        .unwrap();
    assert!(result < start && start < fact && fact < next);
    let EventKind::ContextCompactionStart(data) = &events[start].kind else {
        unreachable!()
    };
    assert_eq!(
        (data.turn_id.as_ref(), data.command, data.through),
        (Some(&turn), Some(command), events[result].sequence)
    );
    let EventKind::ContextCompaction(data) = &events[fact].kind else {
        unreachable!()
    };
    assert_eq!(
        (data.turn_id.as_ref(), data.command, data.start),
        (Some(&turn), Some(command), Some(events[start].sequence))
    );
    assert!(matches!(
        data.outcome,
        ContextCompactionResult::Completed { .. }
    ));
    let request = events[next..]
        .iter()
        .find_map(|event| match &event.kind {
            EventKind::ModelCall(data) => Some(&data.request),
            _ => None,
        })
        .unwrap();
    assert_eq!(*request.messages[0], summary_message("short"));
    let text = serde_json::to_string(&request.messages).unwrap();
    assert!(text.contains("tool output") && !text.contains("first"));
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event.kind, EventKind::TurnStart(_)))
            .count(),
        3
    );
    assert!(matches!(
        &events.last().unwrap().kind,
        EventKind::TurnEnd(TurnEndData {
            outcome: TurnOutcome::Completed { .. },
            ..
        })
    ));
    assert_paired(&events);
    agent.close().await.unwrap();
}

/// Once armed, while `main` opens with its history's read held in the
/// store, reads it the way rebuilding capabilities do: a read that starts
/// after sequence 1 reads alone, and a read of all of it waits for the held
/// read instead of reading again. Then releases the held read.
///
/// While the child opens, reads its ancestry as Permission does, then its
/// context as Prompt does, from `main`'s checkpoint; records the store reads
/// of the context read and where its checkpoint resumes.
struct OpeningReads {
    store: Arc<super::context_reads::CountContextReads>,
    armed: Arc<AtomicBool>,
    reads: Arc<std::sync::Mutex<Option<ContextReads>>>,
    timeline: Key<Timeline>,
    identity: Key<TimelineIdentity>,
}
impl OpeningReads {
    fn new(
        names: &Names<'_>,
        store: Arc<super::context_reads::CountContextReads>,
        armed: Arc<AtomicBool>,
        reads: Arc<std::sync::Mutex<Option<ContextReads>>>,
    ) -> rutis::Result<Self> {
        Ok(Self {
            store,
            armed,
            reads,
            timeline: names.key()?,
            identity: names.key()?,
        })
    }
}
/// The store reads of one read and the branch its checkpoint resumes on.
type ContextReads = (Vec<(String, std::ops::RangeInclusive<u64>)>, String);
impl DynPlugin for OpeningReads {
    fn name(&self) -> &str {
        "test.opening-reads"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.timeline.erase(), self.identity.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let timeline = ctx.require_at(&self.timeline)?;
            let identity = ctx.require_at(&self.identity)?;
            let store = &self.store;
            if !self.armed.load(Ordering::SeqCst) {
                return Ok(Effect::Done);
            }
            if identity.branch_id != "main" {
                timeline.history(Some(0)).await?;
                let (context, reads) = TASK_READS
                    .scope(Default::default(), async {
                        let context = read_context(&timeline, timeline.committed()).await;
                        (context, TASK_READS.with(|reads| reads.take()))
                    })
                    .await;
                let resume = context?
                    .checkpoint
                    .map(|checkpoint| checkpoint.resume.branch_id);
                *self.reads.lock().unwrap() = Some((reads, resume.unwrap_or_default()));
                return Ok(Effect::Done);
            }
            store.entered.notified().await;
            timeline.local(2..).await?;
            let (whole, ()) = tokio::join!(timeline.local(..=timeline.committed()), async {
                store.release.notify_one();
            });
            whole?;
            Ok(Effect::Done)
        })
    }
}

/// Renders the default history with another entry id scheme, keeping each
/// entry's source event.
fn renamed_history(context: &ContextHistory) -> std::result::Result<Vec<HistoryEntry>, String> {
    let mut history = project_context_history(context)?;
    for (index, entry) in history.iter_mut().enumerate() {
        entry.id = format!("other-scheme-{index}");
    }
    Ok(history)
}

/// Acceptance 5: one replay rule for every strategy. A two-message
/// replacement is committed and replayed as is; reload, a fork and another
/// renderer project the same messages; an illegal cut is rejected.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_replaced_strategy_is_replayed_by_the_same_rule_and_checked_by_the_policy() {
    let store = Arc::new(super::context_reads::CountContextReads::default());
    let mut builder = builder_with_strategy(
        Arc::new(EchoModel),
        HistoryRenderer::default(),
        Scripted::new(Mode::TwoMessages),
        None,
    );
    builder.timeline_store(store.clone());
    let agent = builder.build();
    let session = agent
        .create_session(SessionConfig::default())
        .await
        .unwrap();
    turns(&session, &[first().as_str(), "second", "third"]).await;
    let command = session
        .service::<AgentLoop>("main")
        .unwrap()
        .compact()
        .unwrap();
    let events = until(&session, |events| result_for(events, command).is_some()).await;
    let fact = events.last().unwrap().sequence;
    let messages = projected(&session, "main").await;
    assert_eq!(
        messages[..2],
        [
            Message::text(Role::User, "first replacement"),
            Message::text(Role::User, "second replacement"),
        ]
    );
    let child = session
        .branch(
            "main".into(),
            fact,
            BranchOptions {
                prompt: "continue".into(),
                source: BranchSource::Host {
                    operation_id: "fork-after-compaction".into(),
                },
            },
        )
        .await
        .unwrap();
    session.idle(&child).await.unwrap();
    assert!(projected(&session, &child).await.starts_with(&messages));
    for turn in 0..4 {
        session
            .submit(format!("child turn {turn}"), InputMode::Queue, &child)
            .await
            .unwrap();
        session.idle(&child).await.unwrap();
    }
    let context = session.context("main", fact).await.unwrap();
    let renamed = renamed_history(&context).unwrap();
    assert_eq!(context_messages(&context, &renamed), messages);
    let id = session.id().to_owned();
    agent.close().await.unwrap();
    let head = async |branch: &str| {
        let key = TimelineKey {
            session_id: id.clone(),
            branch_id: branch.into(),
        };
        store.head(&key).await.unwrap()
    };

    let reload = StrategyReload::default();
    let mut builder = builder_with_strategy(
        Arc::new(EchoModel),
        HistoryRenderer {
            render: renamed_history,
            ..HistoryRenderer::default()
        },
        Scripted::new(Mode::InsideRetained),
        Some(&reload),
    );
    builder.timeline_store(store.clone());
    let (probe, armed) = (store.clone(), Arc::new(AtomicBool::new(false)));
    let child_reads = Arc::new(std::sync::Mutex::new(None));
    let (arm, child_probe) = (armed.clone(), child_reads.clone());
    builder.require_feature(Feature::new("test.opening-reads").branch(
        "test.opening-reads",
        move |names| {
            Ok(Box::new(OpeningReads::new(
                names,
                probe.clone(),
                arm.clone(),
                child_probe.clone(),
            )?))
        },
    ));
    let agent = builder.build();
    let main_head = head("main").await;
    *store.fail_range.lock().unwrap() = Some(("main".into(), 1..=main_head, 1));
    let failed = agent.restore_session(id.clone()).await;
    assert!(
        matches!(&failed, Err(error) if error.to_string().contains("injected history read failure")),
        "{:?}",
        failed.err()
    );
    assert_eq!(agent.live_openings(), 0);
    let main_head = head("main").await;
    let child_head = head(&child).await;
    store.ranges.lock().unwrap().clear();
    store.pages.lock().unwrap().clear();
    *store.hold_range.lock().unwrap() = Some(("main".into(), 1..=main_head, 1));
    armed.store(true, Ordering::SeqCst);
    let session = tokio::time::timeout(Duration::from_secs(5), agent.restore_session(id))
        .await
        .expect("a read of a part does not wait for the held read")
        .unwrap();
    let reads = std::mem::take(&mut *store.ranges.lock().unwrap());
    let cut = session
        .branches()
        .into_iter()
        .find(|info| info.id == child)
        .unwrap()
        .branch_point
        .unwrap();
    let of = |branch: &str| -> Vec<_> {
        reads
            .iter()
            .filter(|(read, _)| read == branch)
            .map(|(_, range)| range.clone())
            .collect()
    };
    let from_start = |branch: &str| -> Vec<_> {
        let mut ranges: Vec<_> = of(branch)
            .into_iter()
            .filter(|range| *range.start() == 1)
            .collect();
        ranges.sort_by_key(|range| *range.end());
        ranges
    };
    assert_eq!(from_start("main"), [1..=cut, 1..=main_head], "{reads:?}");
    let (context_reads, resume) = child_reads.lock().unwrap().take().unwrap();
    assert_eq!(resume, "main");
    assert!(
        context_reads.iter().all(|(branch, _)| *branch == child),
        "{context_reads:?}"
    );
    assert!(
        matches!(&from_start(&child)[..], [range] if *range == (1..=child_head)),
        "{reads:?}"
    );
    let pages = std::mem::take(&mut *store.pages.lock().unwrap());
    assert!(
        matches!(&pages[..], [(branch, 0, events)] if *branch == child && (*events as u64) < child_head),
        "{pages:?}, {child_head}"
    );
    assert!(
        of(&child)
            .iter()
            .all(|range| *range.start() == 1 || *range.start() > child_head),
        "{reads:?}"
    );
    assert!(
        of("main").iter().all(|range| *range.start() == 1
            || *range.start() > main_head
            || *range == (2..=main_head)
            || *range.end() == cut),
        "{reads:?}"
    );
    assert_eq!(agent.live_openings(), 0);
    let published = session.sequence("main").unwrap();
    session.local("main", ..).await.unwrap();
    assert!(
        store
            .ranges
            .lock()
            .unwrap()
            .contains(&("main".into(), 1..=published))
    );
    let timeline_context = session.context("main", fact).await.unwrap();
    let history = renamed_history(&timeline_context).unwrap();
    assert_eq!(context_messages(&timeline_context, &history), messages);
    let command = session
        .service::<AgentLoop>("main")
        .unwrap()
        .compact()
        .unwrap();
    let events = until(&session, |events| result_for(events, command).is_some()).await;
    assert!(matches!(
        &result_for(&events, command).unwrap().outcome,
        ContextCompactionResult::Failed { error, .. }
            if error == "Compaction cut lies inside the retained tail."
    ));
    assert_eq!(projected(&session, "main").await, messages);
    assert_paired(&events);
    let generation = |branch: &str| {
        session
            .branch_diagnostics(branch)
            .unwrap()
            .plugins
            .into_iter()
            .find(|plugin| plugin.name == "Loop")
            .map(|plugin| (plugin.generation, plugin.state))
    };
    let before = [generation("main"), generation(&child)].map(|loop_| loop_.unwrap().0);
    store.ranges.lock().unwrap().clear();
    let view = reload.0.lock().unwrap().last().cloned().unwrap();
    view.restart().await.unwrap();
    for (branch, before) in ["main", child.as_str()].into_iter().zip(before) {
        while !generation(branch).is_some_and(|(generation, state)| {
            generation > before && state == rutis::host::FiberState::Active
        }) {
            tokio::task::yield_now().await;
        }
    }
    let through = session.sequence("main").unwrap();
    assert!(
        store
            .ranges
            .lock()
            .unwrap()
            .contains(&("main".into(), 1..=through))
    );
    agent.close().await.unwrap();
}

/// Acceptance 6: an interrupt cancels a running step, and a pending one ends
/// without a start.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_interrupt_cancels_a_running_step_and_a_pending_command() {
    let strategy = Scripted::new(Mode::Cut);
    let model = Arc::new(ToolOnRequest::default());
    *model.text.lock().unwrap() = "tool output".into();
    let agent = builder_with_strategy(
        model.clone(),
        HistoryRenderer::default(),
        strategy.clone(),
        None,
    )
    .build();
    let session = agent
        .create_session(SessionConfig::default())
        .await
        .unwrap();
    turns(&session, &[first().as_str(), "second", "third"]).await;
    strategy.hold.store(true, Ordering::SeqCst);
    let loop_service = session.service::<AgentLoop>("main").unwrap();
    let command = loop_service.compact().unwrap();
    strategy.entered.notified().await;
    session.interrupt("main", "user stop".into()).unwrap();
    let events = until(&session, |events| result_for(events, command).is_some()).await;
    let result = result_for(&events, command).unwrap();
    assert!(result.start.is_some());
    assert!(matches!(&result.outcome,
        ContextCompactionResult::Cancelled { reason } if reason == "user stop"));
    assert_eq!(strategy.calls.load(Ordering::SeqCst), 1);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        results_of(&session.history("main", None).await.unwrap(), command),
        1,
        "settled once"
    );
    model.hold.store(true, Ordering::SeqCst);
    session
        .submit("use the tool".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    model.entered.notified().await;
    let command = loop_service.compact().unwrap();
    let mut dispatch = session.dispatch_watch("main").unwrap();
    while dispatch.borrow().sequence < command {
        dispatch.changed().await.unwrap();
    }
    session.interrupt("main", "user stop".into()).unwrap();
    model.release.notify_one();
    session.idle("main").await.unwrap();
    let events = until(&session, |events| result_for(events, command).is_some()).await;
    let result = result_for(&events, command).unwrap();
    assert_eq!(result.start, None);
    assert!(matches!(&result.outcome,
        ContextCompactionResult::Cancelled { reason } if reason == "user stop"));
    assert_eq!(strategy.calls.load(Ordering::SeqCst), 1);
    assert_paired(&events);
    agent.close().await.unwrap();
}

/// Acceptance 6: a plugin reload during a step settles it `cancelled:
/// capability reloading` without a second strategy call; a new command
/// compacts normally.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reload_during_a_step_settles_it_without_another_strategy_call() {
    let strategy = Scripted::new(Mode::Cut);
    let reload = StrategyReload::default();
    let agent = builder_with_strategy(
        Arc::new(EchoModel),
        HistoryRenderer::default(),
        strategy.clone(),
        Some(&reload),
    )
    .build();
    let session = agent
        .create_session(SessionConfig::default())
        .await
        .unwrap();
    turns(&session, &[first().as_str(), "second", "third"]).await;
    let before = projected(&session, "main").await;
    strategy.hold.store(true, Ordering::SeqCst);
    let command = session
        .service::<AgentLoop>("main")
        .unwrap()
        .compact()
        .unwrap();
    strategy.entered.notified().await;
    let view = reload.0.lock().unwrap().last().cloned().unwrap();
    view.restart().await.unwrap();
    let events = until(&session, |events| result_for(events, command).is_some()).await;
    let result = result_for(&events, command).unwrap();
    assert!(result.start.is_some());
    assert!(matches!(&result.outcome,
        ContextCompactionResult::Cancelled { reason } if reason == CAPABILITY_RELOADING));
    assert_eq!(projected(&session, "main").await, before);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        results_of(&session.history("main", None).await.unwrap(), command),
        1,
        "settled once"
    );
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(strategy.calls.load(Ordering::SeqCst), 1);
    let command = session
        .service::<AgentLoop>("main")
        .unwrap()
        .compact()
        .unwrap();
    let events = until(&session, |events| result_for(events, command).is_some()).await;
    assert!(matches!(
        result_for(&events, command).unwrap().outcome,
        ContextCompactionResult::Completed { .. }
    ));
    assert_eq!(strategy.calls.load(Ordering::SeqCst), 2);
    assert_paired(&events);
    agent.close().await.unwrap();
}

/// Acceptance 6: a manual compaction failing inside a turn lets the turn go
/// on uncompacted; when the next request exceeds the window, budget
/// recovery compacts with a fallback cut.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_manual_compaction_lets_the_turn_continue_and_recovery_compacts() {
    let model = Arc::new(ToolOnRequest::default());
    *model.text.lock().unwrap() = "t".repeat(1_500);
    let agent = builder_with_strategy(
        model.clone(),
        HistoryRenderer::default(),
        Scripted::new(Mode::FailFirst),
        None,
    )
    .build();
    let session = agent
        .create_session(SessionConfig::default())
        .await
        .unwrap();
    let first = "f".repeat(1_000);
    turns(&session, &[first.as_str(), "second"]).await;
    model.hold.store(true, Ordering::SeqCst);
    session
        .submit("use the tool".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    model.entered.notified().await;
    let command = session
        .service::<AgentLoop>("main")
        .unwrap()
        .compact()
        .unwrap();
    let mut dispatch = session.dispatch_watch("main").unwrap();
    while dispatch.borrow().sequence < command {
        dispatch.changed().await.unwrap();
    }
    model.release.notify_one();
    session.idle("main").await.unwrap();
    let events = until(&session, |events| {
        matches!(
            events.last().map(|event| &event.kind),
            Some(EventKind::TurnEnd(_))
        )
    })
    .await;
    let manual = result_for(&events, command).unwrap();
    assert!(manual.start.is_some());
    assert!(matches!(&manual.outcome,
        ContextCompactionResult::Failed { error, .. } if error.contains("summarizer unavailable")));
    let recovered = events
        .iter()
        .find_map(|event| match &event.kind {
            EventKind::ContextCompaction(data) if data.command.is_none() => Some(data),
            _ => None,
        })
        .expect("budget recovery compacts");
    let ContextCompactionResult::Completed { diagnostics, .. } = &recovered.outcome else {
        panic!("expected completion, got {:?}", recovered.outcome)
    };
    assert_eq!(
        diagnostics.trigger,
        CompactionTrigger::ProviderOutputBudgetExhausted
    );
    assert!(matches!(
        &events.last().unwrap().kind,
        EventKind::TurnEnd(TurnEndData {
            outcome: TurnOutcome::Completed { .. },
            ..
        })
    ));
    assert_paired(&events);
    agent.close().await.unwrap();
}

/// Captures the branch Timeline so a test can write a `context/compact`
/// that does not come through `AgentLoop::compact`.
struct RawTimeline {
    timeline_cell: Arc<std::sync::OnceLock<Arc<Timeline>>>,
    timeline: Key<Timeline>,
}
impl RawTimeline {
    fn new(
        names: &Names<'_>,
        timeline_cell: Arc<std::sync::OnceLock<Arc<Timeline>>>,
    ) -> rutis::Result<Self> {
        Ok(Self {
            timeline_cell,
            timeline: names.key()?,
        })
    }
}
impl DynPlugin for RawTimeline {
    fn name(&self) -> &str {
        "test.raw-timeline"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.timeline.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let timeline = ctx.require_at(&self.timeline)?;
            let _ = self.timeline_cell.set(timeline);
            Ok(Effect::Done)
        })
    }
}

/// §5.4: repeated and concurrent requests while a command is outstanding
/// join it. The Timeline holds one command and every caller waits for the
/// same result; once that result is consumed, a new request writes a new
/// command.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn repeated_and_concurrent_requests_join_the_outstanding_command() {
    let strategy = Scripted::new(Mode::Cut);
    let agent = builder_with_strategy(
        Arc::new(EchoModel),
        HistoryRenderer::default(),
        strategy.clone(),
        None,
    )
    .build();
    let session = agent
        .create_session(SessionConfig::default())
        .await
        .unwrap();
    turns(&session, &[first().as_str(), "second", "third"]).await;
    let before = session.sequence("main").unwrap();
    let loop_service = session.service::<AgentLoop>("main").unwrap();
    strategy.hold.store(true, Ordering::SeqCst);
    let concurrent: Vec<_> = (0..4)
        .map(|_| {
            let loop_service = loop_service.clone();
            tokio::spawn(async move { loop_service.compact().unwrap() })
        })
        .collect();
    let mut commands = Vec::new();
    for call in concurrent {
        commands.push(call.await.unwrap());
    }
    let command = commands[0];
    assert!(commands.iter().all(|joined| *joined == command));
    strategy.entered.notified().await;
    assert_eq!(loop_service.compact().unwrap(), command);
    strategy.release.notify_one();
    let events = until(&session, |events| result_for(events, command).is_some()).await;
    assert_eq!(
        events
            .iter()
            .filter(|event| event.sequence > before
                && matches!(event.kind, EventKind::ContextCompact(_)))
            .count(),
        1
    );
    assert_eq!(starts_of(&events), [Some(command)]);
    assert_eq!(results_of(&events, command), 1);
    assert_eq!(strategy.calls.load(Ordering::SeqCst), 1);
    let result = events
        .iter()
        .find(|event| {
            matches!(&event.kind,
            EventKind::ContextCompaction(data) if data.command == Some(command))
        })
        .unwrap()
        .sequence;
    consumed(&session, result).await;
    assert!(loop_service.compact().unwrap() > result);
    assert_paired(&session.history("main", None).await.unwrap());
    agent.close().await.unwrap();
}

/// §5.4 defensive rule: a `context/compact` consumed while another command
/// is outstanding (one not written through `AgentLoop::compact`, or old
/// data) is skipped at once with a result that references it; it is not
/// queued.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_second_command_consumed_while_one_is_outstanding_is_skipped() {
    let strategy = Scripted::new(Mode::Cut);
    let timeline = Arc::new(std::sync::OnceLock::new());
    let mut builder = builder_with_strategy(
        Arc::new(EchoModel),
        HistoryRenderer::default(),
        strategy.clone(),
        None,
    );
    let captured = timeline.clone();
    builder.require_feature(
        Feature::new("test.raw-timeline").branch("test.raw-timeline", move |names| {
            Ok(Box::new(RawTimeline::new(names, captured.clone())?))
        }),
    );
    let agent = builder.build();
    let session = agent
        .create_session(SessionConfig::default())
        .await
        .unwrap();
    turns(&session, &[first().as_str(), "second", "third"]).await;
    strategy.hold.store(true, Ordering::SeqCst);
    let command = session
        .service::<AgentLoop>("main")
        .unwrap()
        .compact()
        .unwrap();
    strategy.entered.notified().await;
    let raw = timeline
        .get()
        .unwrap()
        .append(EventKind::ContextCompact(ContextCompactData {}).into())
        .unwrap()
        .sequence;
    let events = until(&session, |events| result_for(events, raw).is_some()).await;
    let skipped = result_for(&events, raw).unwrap();
    assert_eq!((skipped.start, skipped.through), (None, raw));
    assert!(matches!(&skipped.outcome,
        ContextCompactionResult::Skipped { reason, .. }
            if reason == dim_agent_capabilities::agent_loop::ALREADY_IN_PROGRESS));
    strategy.release.notify_one();
    let events = until(&session, |events| result_for(events, command).is_some()).await;
    assert!(matches!(
        result_for(&events, command).unwrap().outcome,
        ContextCompactionResult::Completed { .. }
    ));
    tokio::time::sleep(Duration::from_millis(100)).await;
    let events = session.history("main", None).await.unwrap();
    assert_eq!(starts_of(&events), [Some(command)]);
    assert_eq!(results_of(&events, raw), 1);
    assert_eq!(strategy.calls.load(Ordering::SeqCst), 1);
    assert_paired(&events);
    agent.close().await.unwrap();
}

use dim_agent_capabilities::{
    adapters::{DefaultPrepareModelPlugin, EchoModel, MockTools},
    context_preparation::ContextPreparation,
    defaults,
    history::HistoryRenderer,
    host::*,
    input_queue::InputQueue,
    model_provider::ModelProvider,
    tool_executor::ToolExecutor,
};
use dim_agent_runtime::testing::MinimalHost;
use dim_agent_runtime::testing::chain_events;
use dim_agent_runtime::*;
use dim_agent_types::*;
use rutis::Ctx;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, Key, TypeKey};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::sync::Notify;
use tokio_util::{sync::CancellationToken, task::TaskTracker};

async fn soon<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(3), future)
        .await
        .expect("public flow completed")
}
#[derive(Clone, Copy, PartialEq)]
enum Stage {
    Preparation,
    Model,
    Tools,
}
#[derive(Default)]
struct Barrier {
    started: Notify,
    cleaning: Notify,
    release: Notify,
    cleaned: AtomicBool,
}
impl Barrier {
    async fn cleanup(&self, cancel: CancellationToken) {
        self.started.notify_one();
        cancel.cancelled().await;
        self.cleaning.notify_one();
        self.release.notified().await;
        self.cleaned.store(true, Ordering::SeqCst);
    }
}
struct Prepare {
    stage: Stage,
    barrier: Arc<Barrier>,
    calls: AtomicUsize,
}
impl PrepareContext for Prepare {
    fn prepare(
        &self,
        request: Arc<ModelRequest>,
        _: ContextSource,
        cancel: CancellationToken,
    ) -> HostFuture<'_, Arc<ModelRequest>> {
        Box::pin(async move {
            if self.calls.fetch_add(1, Ordering::SeqCst) == 0 && self.stage == Stage::Preparation {
                self.barrier.cleanup(cancel).await;
            }
            Ok(request)
        })
    }
}
struct Model {
    stage: Stage,
    barrier: Arc<Barrier>,
    calls: Arc<AtomicUsize>,
}
impl ModelDriver for Model {
    fn invoke(
        &self,
        request: Arc<ModelRequest>,
        _: dim_agent_capabilities::host::ModelConnection,
        cancel: CancellationToken,
    ) -> HostFuture<'_, ModelStream> {
        let first = self.calls.fetch_add(1, Ordering::SeqCst) == 0;
        let barrier = self.barrier.clone();
        Box::pin(async move {
            if !first || self.stage != Stage::Model {
                return EchoModel
                    .invoke(
                        request,
                        dim_agent_capabilities::host::ModelConnection::Local,
                        cancel,
                    )
                    .await;
            }
            let stream = futures_util::stream::unfold(
                (0, barrier, cancel),
                |(state, barrier, cancel)| async move {
                    let chunk = match state {
                        0 => ModelChunk::MessageStart {
                            message_id: "blocked".into(),
                        },
                        1 => ModelChunk::ToolStart {
                            id: "pending-tool".into(),
                            name: "echo".into(),
                        },
                        2 => ModelChunk::ToolUse(ToolCall {
                            id: "pending-tool".into(),
                            name: "echo".into(),
                            input: serde_json::from_value(
                                serde_json::json!({"text":"real output"}),
                            )
                            .unwrap(),
                        }),
                        3 => {
                            barrier.cleanup(cancel.clone()).await;
                            ModelChunk::MessageStop {
                                message_id: "blocked".into(),
                                reason: ModelStopReason::ToolUse,
                            }
                        }
                        _ => return None,
                    };
                    Some((Ok(chunk), (state + 1, barrier, cancel)))
                },
            );
            Ok(Box::pin(stream) as ModelStream)
        })
    }
}
struct Tools {
    stage: Stage,
    barrier: Arc<Barrier>,
    calls: Arc<AtomicUsize>,
}
impl ToolDriver for Tools {
    fn definitions(&self) -> Vec<ToolDefinition> {
        MockTools.definitions()
    }
    fn execute(
        &self,
        call: ToolCall,
        context: ToolExecutionContext,
        cancel: CancellationToken,
    ) -> HostFuture<'_, ToolResult> {
        let first = self.calls.fetch_add(1, Ordering::SeqCst) == 0;
        Box::pin(async move {
            if first && self.stage == Stage::Tools {
                self.barrier.cleanup(cancel).await;
                Ok(ToolResult::text("native success"))
            } else {
                MockTools.execute(call, context, cancel).await
            }
        })
    }
}
#[derive(Default)]
struct ExtensionBarrier {
    seen: Notify,
    release: Notify,
    once: AtomicBool,
}
struct Extension {
    barrier: Arc<ExtensionBarrier>,
    dispatcher: Key<Dispatcher>,
    context_preparation: Key<ContextPreparation>,
    model_provider: Key<ModelProvider>,
    tool_executor: Key<ToolExecutor>,
}
impl Extension {
    fn new(names: &Names<'_>, barrier: Arc<ExtensionBarrier>) -> rutis::Result<Self> {
        Ok(Self {
            barrier,
            dispatcher: names.key()?,
            context_preparation: names.key()?,
            model_provider: names.key()?,
            tool_executor: names.key()?,
        })
    }
}
impl DynPlugin for Extension {
    fn name(&self) -> &str {
        "test.interrupt-extension"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![
            self.dispatcher.erase(),
            self.context_preparation.erase(),
            self.model_provider.erase(),
            self.tool_executor.erase(),
        ]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let dispatcher = ctx.require_at(&self.dispatcher)?;
            let tasks = TaskTracker::new();
            let cleanup = tasks.clone();
            ctx.effect(move || async move {
                cleanup.close();
                cleanup.wait().await;
                Ok(())
            })?;
            let signal = self.barrier.clone();
            dispatcher.subscribe(
                ctx,
                "test.interrupt-extension",
                &["stop/interrupt"],
                move |_| {
                    if !signal.once.swap(true, Ordering::SeqCst) {
                        let signal = signal.clone();
                        signal.seen.notify_one();
                        tasks.spawn(async move {
                            signal.release.notified().await;
                        });
                    }
                    Box::pin(async { Ok(()) })
                },
            )?;
            Ok(Effect::Done)
        })
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interrupt_reaches_every_capability_before_native_cleanup_and_stop_waits_for_the_turn() {
    for stage in [Stage::Preparation, Stage::Model, Stage::Tools] {
        let native = Arc::new(Barrier::default());
        let extension = Arc::new(ExtensionBarrier::default());
        let model_calls = Arc::new(AtomicUsize::new(0));
        let tool_calls = Arc::new(AtomicUsize::new(0));
        let mut builder = AgentBuilder::new(AgentConfig::default());
        builder
            .session_value(Arc::new(HistoryRenderer::default()))
            .process_value(Arc::new(ModelHost(Arc::new(Model {
                stage,
                barrier: native.clone(),
                calls: model_calls.clone(),
            }))))
            .session_value(Arc::new(ToolHost(Arc::new(Tools {
                stage,
                barrier: native.clone(),
                calls: tool_calls.clone(),
            }))))
            .session_value(Arc::new(PrepareContextHost(Arc::new(Prepare {
                stage,
                barrier: native.clone(),
                calls: AtomicUsize::new(0),
            }))))
            .session_value(Arc::new(ContextModelHost(None)))
            .session_value(Arc::new(CompactionStrategyHost(None)))
            .require_feature(
                Feature::new("PrepareModelHost").session("PrepareModelHost", |names| {
                    Ok(Box::new(DefaultPrepareModelPlugin::new(names)?))
                }),
            );
        defaults::install(&mut builder);
        let extension_plugin = extension.clone();
        builder.require_feature(
            Feature::new("test.interrupt-extension")
                .branch("test.interrupt-extension", move |names| {
                    Ok(Box::new(Extension::new(names, extension_plugin.clone())?))
                }),
        );
        let agent = MinimalHost::start(&builder).await.unwrap();
        let session = soon(agent.create_session(SessionConfig::default()))
            .await
            .unwrap();
        let diagnostics = session.branch_diagnostics("main").unwrap();
        assert!(!format!("{diagnostics:?}").contains("Execution"));
        soon(session.submit(
            "tool:echo {\"text\":\"hello\"}".into(),
            InputMode::Queue,
            "main",
        ))
        .await
        .unwrap();
        soon(native.started.notified()).await;
        let stopping = session.clone();
        let mut stop =
            tokio::spawn(async move { stopping.stop("main", "stop requested".into()).await });
        soon(native.cleaning.notified()).await;
        soon(extension.seen.notified()).await;
        assert!(!native.cleaned.load(Ordering::SeqCst));
        assert!(
            tokio::time::timeout(Duration::from_millis(10), &mut stop)
                .await
                .is_err()
        );
        native.release.notify_one();
        soon(stop).await.unwrap().unwrap();
        assert!(native.cleaned.load(Ordering::SeqCst));
        extension.release.notify_one();
        let events = chain_events(&session, "main", ..).await.unwrap();
        assert!(events.iter().any(|event| matches!(&event.kind, EventKind::StopInterrupt(data) if data.scope == InterruptScope::Turn)));
        assert!(matches!(
            events.last().unwrap().kind,
            EventKind::TurnEnd(TurnEndData {
                outcome: TurnOutcome::Cancelled { .. },
                ..
            })
        ));
        assert_eq!(
            model_calls.load(Ordering::SeqCst),
            usize::from(stage != Stage::Preparation)
        );
        assert_eq!(
            tool_calls.load(Ordering::SeqCst),
            usize::from(stage == Stage::Tools)
        );
        if stage == Stage::Model {
            let closure = &events[events.len() - 2];
            assert!(matches!(&closure.kind,
                EventKind::ToolResult(data) if data.call_event_id.is_none()
                    && data.results.len() == 1
                    && data.results[0].call_id == "pending-tool"
                    && data.results[0].result.content
                        == ToolContent::Text("Tool was not executed: stop requested".into())));
            assert!(
                !events
                    .iter()
                    .any(|event| matches!(event.kind, EventKind::ToolCall(_)))
            );
        }
        if stage == Stage::Tools {
            assert!(events.iter().any(|event| matches!(&event.kind, EventKind::ToolResult(data) if data.results[0].result.content == ToolContent::Text("native success".into()))));
        }
        soon(session.submit("resume".into(), InputMode::Queue, "main"))
            .await
            .unwrap();
        soon(session.idle("main")).await.unwrap();
        assert!(echoed(
            &chain_events(&session, "main", ..).await.unwrap(),
            "resume"
        ));
        soon(agent.close()).await.unwrap();
    }
}

#[derive(Default)]
struct Gate {
    entered: Notify,
    release: Notify,
    once: AtomicBool,
    timeline: Mutex<Option<Timeline>>,
}
/// Which capabilities an [`InputGate`] loads after, besides the Timeline and
/// Dispatcher it reads.
#[derive(Clone, Copy, PartialEq, Eq)]
enum After {
    Dispatcher,
    InputQueue,
    /// InputQueue and Loop.
    Loop,
}
/// Holds the first dispatch of its event key (`event`) until released.
struct InputGate {
    gate: Arc<Gate>,
    event: &'static str,
    timeline: Key<Timeline>,
    dispatcher: Key<Dispatcher>,
    input_queue: Option<Key<InputQueue>>,
    loop_state: Option<Key<dyn LoopState>>,
}
impl InputGate {
    /// The gate as a branch part loading after `after`.
    fn part(
        names: &Names<'_>,
        gate: Arc<Gate>,
        after: After,
        key: &'static str,
    ) -> rutis::Result<Box<dyn DynPlugin>> {
        Ok(Box::new(InputGate {
            gate,
            event: key,
            timeline: names.key()?,
            dispatcher: names.key()?,
            input_queue: (after != After::Dispatcher)
                .then(|| names.key())
                .transpose()?,
            loop_state: (after == After::Loop).then(|| names.key()).transpose()?,
        }))
    }
}
impl DynPlugin for InputGate {
    fn name(&self) -> &str {
        "test.input-gate"
    }
    fn injects(&self) -> Vec<TypeKey> {
        let mut keys = vec![self.timeline.erase(), self.dispatcher.erase()];
        keys.extend(self.input_queue.as_ref().map(Key::erase));
        keys.extend(self.loop_state.as_ref().map(Key::erase));
        keys
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let timeline = ctx.require_at(&self.timeline)?;
            let dispatcher = ctx.require_at(&self.dispatcher)?;
            *self.gate.timeline.lock().unwrap() = Some((*timeline).clone());
            let gate = self.gate.clone();
            dispatcher.subscribe(ctx, "test.input-gate", &[self.event], move |_| {
                let gate = gate.clone();
                Box::pin(async move {
                    if !gate.once.swap(true, Ordering::SeqCst) {
                        gate.entered.notify_one();
                        gate.release.notified().await;
                    }
                    Ok(())
                })
            })?;
            Ok(Effect::Done)
        })
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn input_written_before_an_interrupt_waits_for_the_next_driving_input() {
    let gate = Arc::new(Gate::default());
    let copy = gate.clone();
    let mut builder = AgentBuilder::new(AgentConfig::default());
    defaults::mock_hosts(&mut builder);
    defaults::install(&mut builder);
    builder.require_feature(
        Feature::new("test.input-gate").branch("test.input-gate", move |names| {
            InputGate::part(names, copy.clone(), After::InputQueue, "input/message")
        }),
    );
    let agent = MinimalHost::start(&builder).await.unwrap();
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    session
        .submit("delayed".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    soon(gate.entered.notified()).await;
    let timeline = gate.timeline.lock().unwrap().clone().unwrap();
    timeline
        .append(
            EventKind::StopInterrupt(StopInterruptData {
                reason: "ordered interrupt".into(),
                scope: InterruptScope::Turn,
            })
            .into(),
        )
        .unwrap();
    gate.release.notify_one();
    soon(session.idle("main")).await.unwrap();
    let events = chain_events(&session, "main", ..).await.unwrap();
    assert!(
        !events
            .iter()
            .any(|event| matches!(event.kind, EventKind::ModelCall(_)))
    );
    assert!(events.iter().all(|event| !matches!(
        event.kind,
        EventKind::TurnEnd(TurnEndData {
            outcome: TurnOutcome::Completed { .. },
            ..
        })
    )));
    assert_eq!(
        session
            .get::<InputSnapshot>("main")
            .unwrap()
            .unwrap()
            .queued,
        1
    );
    let next = session
        .submit("new input".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    soon(session.idle("main")).await.unwrap();
    let events = chain_events(&session, "main", ..).await.unwrap();
    assert!(echoed(&events, "new input"));
    let consumed: Vec<_> = events
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::InputConsumed(data) => Some(data.input_id.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(consumed.len(), 2);
    assert!(consumed.contains(&next.id));
    soon(agent.close()).await.unwrap();
}

async fn gated() -> (MinimalHost, Arc<Gate>) {
    let gate = Arc::new(Gate::default());
    gate.once.store(true, Ordering::SeqCst);
    let copy = gate.clone();
    let mut builder = AgentBuilder::new(AgentConfig::default());
    defaults::mock_hosts(&mut builder);
    defaults::install(&mut builder);
    builder.require_feature(
        Feature::new("test.input-gate").branch("test.input-gate", move |names| {
            InputGate::part(names, copy.clone(), After::InputQueue, "input/message")
        }),
    );
    (MinimalHost::start(&builder).await.unwrap(), gate)
}
fn interrupt() -> EventDraft {
    EventKind::StopInterrupt(StopInterruptData {
        reason: "operator stop".into(),
        scope: InterruptScope::Turn,
    })
    .into()
}
fn echoed(events: &[Arc<AgentEvent>], text: &str) -> bool {
    let expected = format!("Echo: {text}");
    events
        .iter()
        .any(|event| matches!(&event.kind, EventKind::Text(data) if data.delta == expected))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_steer_input_after_a_stop_starts_a_turn() {
    let (agent, _gate) = gated().await;
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    soon(session.stop("main", "operator stop".into()))
        .await
        .unwrap();
    session
        .submit("after stop".into(), InputMode::Steer, "main")
        .await
        .unwrap();
    soon(session.idle("main")).await.unwrap();
    assert!(echoed(
        &chain_events(&session, "main", ..).await.unwrap(),
        "after stop"
    ));
    soon(agent.close()).await.unwrap();
}

/// Model whose first reply proposes one `echo` call; later replies echo.
struct ProposesTool;
impl ModelDriver for ProposesTool {
    fn invoke(
        &self,
        request: Arc<ModelRequest>,
        connection: dim_agent_capabilities::host::ModelConnection,
        cancel: CancellationToken,
    ) -> HostFuture<'_, ModelStream> {
        let first = !request
            .messages
            .iter()
            .any(|message| format!("{message:?}").contains("late-tool"));
        Box::pin(async move {
            if !first {
                return EchoModel.invoke(request, connection, cancel).await;
            }
            let chunks = vec![
                Ok(ModelChunk::MessageStart {
                    message_id: "late".into(),
                }),
                Ok(ModelChunk::ToolStart {
                    id: "late-tool".into(),
                    name: "echo".into(),
                }),
                Ok(ModelChunk::ToolUse(ToolCall {
                    id: "late-tool".into(),
                    name: "echo".into(),
                    input: serde_json::from_value(serde_json::json!({"text":"late"})).unwrap(),
                })),
                Ok(ModelChunk::MessageStop {
                    message_id: "late".into(),
                    reason: ModelStopReason::ToolUse,
                }),
            ];
            Ok(Box::pin(futures_util::stream::iter(chunks)) as ModelStream)
        })
    }
}
struct CountedTools(Arc<AtomicUsize>);
impl ToolDriver for CountedTools {
    fn definitions(&self) -> Vec<ToolDefinition> {
        MockTools.definitions()
    }
    fn execute(
        &self,
        call: ToolCall,
        context: ToolExecutionContext,
        cancel: CancellationToken,
    ) -> HostFuture<'_, ToolResult> {
        self.0.fetch_add(1, Ordering::SeqCst);
        MockTools.execute(call, context, cancel)
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_tool_call_already_committed_when_the_interrupt_arrives_is_not_executed() {
    let gate = Arc::new(Gate::default());
    let executed = Arc::new(AtomicUsize::new(0));
    let mut builder = AgentBuilder::new(AgentConfig::default());
    let copy = gate.clone();
    builder.require_feature(
        Feature::new("test.input-gate").branch("test.input-gate", move |names| {
            InputGate::part(names, copy.clone(), After::Dispatcher, "step/tool-use")
        }),
    );
    builder
        .session_value(Arc::new(HistoryRenderer::default()))
        .process_value(Arc::new(ModelHost(Arc::new(ProposesTool))))
        .session_value(Arc::new(ToolHost(Arc::new(CountedTools(executed.clone())))))
        .session_value(Arc::new(PrepareContextHost(Arc::new(Prepare {
            stage: Stage::Model,
            barrier: Arc::new(Barrier::default()),
            calls: AtomicUsize::new(1),
        }))))
        .session_value(Arc::new(ContextModelHost(None)))
        .session_value(Arc::new(CompactionStrategyHost(None)))
        .require_feature(
            Feature::new("PrepareModelHost").session("PrepareModelHost", |names| {
                Ok(Box::new(DefaultPrepareModelPlugin::new(names)?))
            }),
        );
    defaults::install(&mut builder);
    let agent = MinimalHost::start(&builder).await.unwrap();
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    session
        .submit("call a tool".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    soon(gate.entered.notified()).await;
    until_events(&session, |events| {
        events
            .iter()
            .any(|event| matches!(event.kind, EventKind::ModelOutputEnd(_)))
    })
    .await;
    let timeline = gate.timeline.lock().unwrap().clone().unwrap();
    let interrupt = timeline.append(interrupt()).unwrap();
    gate.release.notify_one();
    soon(session.idle("main")).await.unwrap();
    let events = chain_events(&session, "main", ..).await.unwrap();
    let call = events
        .iter()
        .find(|event| matches!(event.kind, EventKind::ToolCall(_)))
        .expect("Loop proposed the call before handling the interrupt");
    assert!(call.sequence > interrupt.sequence);
    assert_eq!(executed.load(Ordering::SeqCst), 0);
    assert!(events.iter().any(|event| matches!(&event.kind,
        EventKind::ToolResult(data) if matches!(&data.results[0].result.content,
            ToolContent::Text(text) if text.contains("not executed")))));
    assert!(events.iter().any(|event| matches!(
        event.kind,
        EventKind::TurnEnd(TurnEndData {
            outcome: TurnOutcome::Cancelled { .. },
            ..
        })
    )));
    soon(agent.close()).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stop_waits_for_an_interrupted_turn_whose_start_is_not_yet_dispatched() {
    let gate = Arc::new(Gate::default());
    let end_gate = Arc::new(Gate::default());
    let next_model = Arc::new(Barrier::default());
    let copy = gate.clone();
    let mut builder = AgentBuilder::new(AgentConfig::default());
    builder.require_feature(
        Feature::new("test.input-gate").branch("test.input-gate", move |names| {
            InputGate::part(names, copy.clone(), After::InputQueue, "input/message")
        }),
    );
    builder
        .session_value(Arc::new(HistoryRenderer::default()))
        .process_value(Arc::new(ModelHost(Arc::new(Model {
            stage: Stage::Model,
            barrier: next_model.clone(),
            calls: Arc::new(AtomicUsize::new(0)),
        }))))
        .session_value(Arc::new(ToolHost(Arc::new(MockTools))))
        .session_value(Arc::new(PrepareContextHost(Arc::new(IdentityContext))))
        .session_value(Arc::new(ContextModelHost(None)))
        .session_value(Arc::new(CompactionStrategyHost(None)))
        .require_feature(
            Feature::new("PrepareModelHost").session("PrepareModelHost", |names| {
                Ok(Box::new(DefaultPrepareModelPlugin::new(names)?))
            }),
        );
    defaults::install(&mut builder);
    let copy = end_gate.clone();
    builder.require_feature(
        Feature::new("test.turn-end-gate").branch("test.turn-end-gate", move |names| {
            InputGate::part(names, copy.clone(), After::Loop, "turn/end")
        }),
    );
    let agent = MinimalHost::start(&builder).await.unwrap();
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    session
        .submit("runs into a stop".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    soon(gate.entered.notified()).await;
    let stopping = session.clone();
    let mut stop = tokio::spawn(async move { stopping.stop("main", "user stop".into()).await });
    while !chain_events(&session, "main", ..)
        .await
        .unwrap()
        .iter()
        .any(|event| matches!(event.kind, EventKind::StopInterrupt(_)))
    {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    gate.release.notify_one();
    soon(end_gate.entered.notified()).await;
    assert_eq!(
        session.get::<LoopSnapshot>("main").unwrap().unwrap().phase,
        LoopPhase::Idle
    );
    let premature = tokio::time::timeout(Duration::from_millis(25), &mut stop).await;
    session
        .submit("later work".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    end_gate.release.notify_one();
    assert!(
        premature.is_err(),
        "Idle does not acknowledge turn/end dispatch: {premature:?}"
    );
    soon(next_model.started.notified()).await;
    let returned = soon(stop).await.unwrap().unwrap();
    let events = chain_events(&session, "main", ..).await.unwrap();
    let interrupt = events
        .iter()
        .find(|event| matches!(event.kind, EventKind::StopInterrupt(_)))
        .unwrap()
        .sequence;
    let start = events
        .iter()
        .find(|event| matches!(event.kind, EventKind::TurnStart(_)))
        .expect("Loop started the turn before handling the interrupt")
        .sequence;
    let end = events
        .iter()
        .find(|event| matches!(event.kind, EventKind::TurnEnd(_)))
        .expect("the interrupted turn ended before stop returned")
        .sequence;
    assert!(start > interrupt, "turn/start follows the interrupt");
    assert!(
        returned >= end,
        "stop returned at {returned}, turn ended at {end}"
    );
    assert_eq!(
        session.get::<LoopSnapshot>("main").unwrap().unwrap().phase,
        LoopPhase::Running
    );
    next_model.release.notify_one();
    soon(agent.close()).await.unwrap();
}

/// Streams the chunks a test sends to its first call, whatever the
/// cancellation; the stream ends when the test drops the sender. Later calls
/// echo.
struct ChannelModel {
    first: Mutex<Option<tokio::sync::mpsc::UnboundedReceiver<ModelChunk>>>,
    cancel: Mutex<Option<CancellationToken>>,
    ended: Arc<Notify>,
}
fn channel_model() -> (
    Arc<ChannelModel>,
    tokio::sync::mpsc::UnboundedSender<ModelChunk>,
) {
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    let model = Arc::new(ChannelModel {
        first: Mutex::new(Some(receiver)),
        cancel: Mutex::new(None),
        ended: Arc::default(),
    });
    (model, sender)
}
impl ModelDriver for ChannelModel {
    fn invoke(
        &self,
        request: Arc<ModelRequest>,
        connection: dim_agent_capabilities::host::ModelConnection,
        cancel: CancellationToken,
    ) -> HostFuture<'_, ModelStream> {
        let Some(receiver) = self.first.lock().unwrap().take() else {
            return EchoModel.invoke(request, connection, cancel);
        };
        *self.cancel.lock().unwrap() = Some(cancel);
        let ended = self.ended.clone();
        Box::pin(async move {
            let stream = futures_util::stream::unfold(receiver, move |mut receiver| {
                let ended = ended.clone();
                async move {
                    match receiver.recv().await {
                        Some(chunk) => Some((Ok(chunk), receiver)),
                        None => {
                            ended.notify_one();
                            None
                        }
                    }
                }
            });
            Ok(Box::pin(stream) as ModelStream)
        })
    }
}
impl ChannelModel {
    /// The token of the first call, once it was invoked.
    async fn invoked(&self) -> CancellationToken {
        soon(async {
            loop {
                if let Some(cancel) = self.cancel.lock().unwrap().clone() {
                    return cancel;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
    }
}
fn channel_builder(model: Arc<ChannelModel>) -> AgentBuilder {
    let mut builder = AgentBuilder::new(AgentConfig::default());
    builder
        .session_value(Arc::new(HistoryRenderer::default()))
        .process_value(Arc::new(ModelHost(model)))
        .session_value(Arc::new(PrepareContextHost(Arc::new(IdentityContext))))
        .session_value(Arc::new(ContextModelHost(None)))
        .session_value(Arc::new(CompactionStrategyHost(None)))
        .require_feature(
            Feature::new("PrepareModelHost").session("PrepareModelHost", |names| {
                Ok(Box::new(DefaultPrepareModelPlugin::new(names)?))
            }),
        );
    defaults::tool_host(&mut builder, Arc::new(MockTools));
    builder
}
async fn until_events(
    session: &Session,
    check: impl Fn(&[Arc<AgentEvent>]) -> bool,
) -> Vec<Arc<AgentEvent>> {
    soon(async {
        loop {
            let events = chain_events(session, "main", ..).await.unwrap();
            if check(&events) {
                return events;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
}
fn model_starts(events: &[Arc<AgentEvent>]) -> Vec<(u64, String)> {
    events
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::ModelStart(data) => Some((event.sequence, data.request_id.clone())),
            _ => None,
        })
        .collect()
}
fn echo_call(id: &str, text: &str) -> ToolCall {
    ToolCall {
        id: id.into(),
        name: "echo".into(),
        input: serde_json::from_value(serde_json::json!({ "text": text })).unwrap(),
    }
}

/// A stop during output keeps the text received so far, drops the thinking
/// block in progress, and closes every started call, complete or not, with
/// one result batch in start order, committed atomically before the
/// cancelled terminal.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stop_during_output_keeps_its_text_and_closes_every_started_call() {
    let (model, chunks) = channel_model();
    let store = Arc::new(MemoryTimelineStore::default());
    let mut builder = channel_builder(model.clone());
    builder.timeline_store(store.clone());
    defaults::install(&mut builder);
    let agent = MinimalHost::start(&builder).await.unwrap();
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    session
        .submit("go".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    let cancel = model.invoked().await;
    for chunk in [
        ModelChunk::MessageStart {
            message_id: "interleaved".into(),
        },
        ModelChunk::Text("kept ".into()),
        ModelChunk::ToolStart {
            id: "started-only".into(),
            name: "echo".into(),
        },
        ModelChunk::ToolStart {
            id: "complete".into(),
            name: "echo".into(),
        },
        ModelChunk::ToolArguments {
            id: "started-only".into(),
            name: "echo".into(),
            delta: "{\"te".into(),
        },
        ModelChunk::ToolUse(echo_call("complete", "b")),
        ModelChunk::Thinking("half a thought".into()),
        ModelChunk::Text("tail".into()),
    ] {
        chunks.send(chunk).unwrap();
    }
    let mut progress = session.on::<ModelProgress>("main").unwrap().unwrap();
    soon(progress.wait_for(|progress| progress.text == "tail"))
        .await
        .unwrap();
    let shown = session.get::<ModelProgress>("main").unwrap().unwrap();
    assert_eq!(shown.thinking, "half a thought");
    assert_eq!(
        shown.tools,
        vec![ToolProgress {
            call_id: "started-only".into(),
            name: "echo".into(),
            arguments: "{\"te".into(),
        }]
    );
    let stopping = session.clone();
    let stop = tokio::spawn(async move { stopping.stop("main", "operator stop".into()).await });
    soon(cancel.cancelled()).await;
    drop(chunks);
    soon(stop).await.unwrap().unwrap();
    assert_eq!(
        session.get::<ModelProgress>("main").unwrap().unwrap(),
        ModelProgress::default()
    );
    let events = chain_events(&session, "main", ..).await.unwrap();
    let texts: Vec<_> = events
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::Text(data) => Some(data.delta.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(texts, ["kept ", "tail"]);
    assert!(
        !events
            .iter()
            .any(|event| matches!(event.kind, EventKind::Thinking(_)))
    );
    let closure = events
        .iter()
        .position(|event| matches!(&event.kind, EventKind::ToolResult(data) if data.call_event_id.is_none()))
        .expect("the started calls are closed");
    let EventKind::ToolResult(results) = &events[closure].kind else {
        unreachable!()
    };
    let closed: Vec<_> = results
        .results
        .iter()
        .map(|item| (item.call_id.as_str(), item.result.content.clone()))
        .collect();
    assert_eq!(
        closed,
        [
            (
                "started-only",
                ToolContent::Text(
                    "Tool was not executed: the model output ended before its arguments were complete"
                        .into()
                ),
            ),
            (
                "complete",
                ToolContent::Text("Tool was not executed: the model output ended".into()),
            ),
        ]
    );
    assert!(matches!(
        &events[closure + 1].kind,
        EventKind::ModelOutputEnd(ModelOutputEndData {
            outcome: ModelOutputOutcome::Cancelled { .. },
            ..
        })
    ));
    let key = TimelineKey {
        session_id: session.id().to_owned(),
        branch_id: "main".into(),
    };
    let stored: Vec<_> = store
        .read_segments(
            &key.session_id,
            vec![(key.branch_id.clone(), 1..=events.last().unwrap().sequence)],
        )
        .await
        .unwrap()
        .into_iter()
        .flatten()
        .collect();
    let record = stored
        .iter()
        .find(|record| record.event.sequence == events[closure].sequence)
        .unwrap();
    assert_eq!(record.batch_end, events[closure + 1].sequence);
    assert!(
        !events
            .iter()
            .any(|event| matches!(event.kind, EventKind::ToolCall(_)))
    );
    assert!(matches!(
        events.last().unwrap().kind,
        EventKind::TurnEnd(TurnEndData {
            outcome: TurnOutcome::Cancelled { .. },
            ..
        })
    ));
    soon(agent.close()).await.unwrap();
}

fn replaced_terminal(events: &[Arc<AgentEvent>], request_id: &str) -> bool {
    events.iter().any(|event| {
        matches!(&event.kind,
            EventKind::ModelOutputEnd(ModelOutputEndData {
                request_id: id,
                outcome: ModelOutputOutcome::Cancelled { reason },
                ..
            }) if id == request_id && reason == dim_agent_capabilities::steer::REPLACED_BY_STEER)
    })
}

/// A steer consumed before the request received any model output replaces
/// it: ModelProvider cancels the invocation and writes a Cancelled terminal
/// once its host ends, with no output even when the host keeps streaming
/// after the cancellation. The turn then goes on with the steer. A usage
/// chunk is a billing fact and is still committed for the replaced request.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_replaced_request_commits_no_output_its_host_still_streams() {
    let (model, chunks) = channel_model();
    let mut builder = channel_builder(model.clone());
    defaults::install(&mut builder);
    let agent = MinimalHost::start(&builder).await.unwrap();
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    session
        .submit("first".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    model.invoked().await;
    chunks
        .send(ModelChunk::Usage(
            serde_json::from_value(
                serde_json::json!({"promptTokens":4,"completionTokens":2,"totalTokens":6}),
            )
            .unwrap(),
        ))
        .unwrap();
    until_events(&session, |events| {
        events
            .iter()
            .any(|event| matches!(event.kind, EventKind::ModelUsage(_)))
    })
    .await;
    let steer = session
        .submit("STEER-MARKER".into(), InputMode::Steer, "main")
        .await
        .unwrap();
    let applied = until_events(&session, |events| {
        events.iter().any(|event| {
            matches!(&event.kind, EventKind::InputApplied(data) if data.input_id == steer.id)
        })
    })
    .await
    .last()
    .unwrap()
    .sequence;
    let mut dispatch = session.dispatch_watch("main").unwrap();
    soon(dispatch.wait_for(|state| state.sequence >= applied))
        .await
        .unwrap();
    assert!(
        model
            .cancel
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .is_cancelled(),
        "the invocation is cancelled"
    );
    for chunk in [
        ModelChunk::MessageStart {
            message_id: "late".into(),
        },
        ModelChunk::Text("late text".into()),
        ModelChunk::TextEnd,
        ModelChunk::ToolStart {
            id: "late-call".into(),
            name: "echo".into(),
        },
        ModelChunk::ToolUse(echo_call("late-call", "late")),
        ModelChunk::MessageStop {
            message_id: "late".into(),
            reason: ModelStopReason::ToolUse,
        },
    ] {
        chunks.send(chunk).unwrap();
    }
    drop(chunks);
    soon(model.ended.notified()).await;
    soon(session.idle("main")).await.unwrap();
    let events = chain_events(&session, "main", ..).await.unwrap();
    let starts = model_starts(&events);
    assert_eq!(starts.len(), 2);
    let replaced = starts[0].1.clone();
    let request_of = |event: &AgentEvent| match &event.kind {
        EventKind::Text(d) | EventKind::Thinking(d) => Some(d.request_id.clone()),
        EventKind::ToolUseStart(d) => Some(d.request_id.clone()),
        EventKind::ToolUse(d) => Some(d.request_id.clone()),
        EventKind::ModelContent(d) => Some(d.request_id.clone()),
        EventKind::ModelOutputEnd(d) => Some(d.request_id.clone()),
        EventKind::ToolResult(d) => Some(d.request_id.clone()),
        _ => None,
    };
    let written: Vec<_> = events
        .iter()
        .filter(|event| request_of(event).as_ref() == Some(&replaced))
        .collect();
    assert_eq!(
        written.len(),
        1,
        "only the terminal of the replaced request"
    );
    assert!(replaced_terminal(&events, &replaced));
    assert!(
        events.iter().any(|event| matches!(&event.kind,
            EventKind::ModelUsage(data) if data.request_id == replaced && data.usage.total_tokens == 6.)),
        "usage for the replaced request must remain a committed billing fact"
    );
    assert!(written[0].sequence < starts[1].0);
    assert!(echoed(&events, "STEER-MARKER"));
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event.kind, EventKind::TurnStart(_)))
            .count(),
        1
    );
    assert_eq!(
        session.get::<ModelProgress>("main").unwrap().unwrap(),
        ModelProgress::default()
    );
    soon(agent.close()).await.unwrap();
}

/// A steer consumed while ContextPreparation prepares the request replaces
/// it: the preparation is cancelled, the request ends Cancelled without a
/// `model/call`, and the next request of the same turn carries the steer.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_steer_during_preparation_ends_the_request_before_the_model_call() {
    let native = Arc::new(Barrier::default());
    let model_calls = Arc::new(AtomicUsize::new(0));
    let mut builder = AgentBuilder::new(AgentConfig::default());
    builder
        .session_value(Arc::new(HistoryRenderer::default()))
        .process_value(Arc::new(ModelHost(Arc::new(Model {
            stage: Stage::Preparation,
            barrier: native.clone(),
            calls: model_calls.clone(),
        }))))
        .session_value(Arc::new(PrepareContextHost(Arc::new(Prepare {
            stage: Stage::Preparation,
            barrier: native.clone(),
            calls: AtomicUsize::new(0),
        }))))
        .session_value(Arc::new(ContextModelHost(None)))
        .session_value(Arc::new(CompactionStrategyHost(None)))
        .require_feature(
            Feature::new("PrepareModelHost").session("PrepareModelHost", |names| {
                Ok(Box::new(DefaultPrepareModelPlugin::new(names)?))
            }),
        );
    defaults::tool_host(&mut builder, Arc::new(MockTools));
    defaults::install(&mut builder);
    let agent = MinimalHost::start(&builder).await.unwrap();
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    session
        .submit("first".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    soon(native.started.notified()).await;
    session
        .submit("STEER-MARKER".into(), InputMode::Steer, "main")
        .await
        .unwrap();
    soon(native.cleaning.notified()).await;
    native.release.notify_one();
    soon(session.idle("main")).await.unwrap();
    let events = chain_events(&session, "main", ..).await.unwrap();
    let starts = model_starts(&events);
    assert_eq!(starts.len(), 2);
    assert!(replaced_terminal(&events, &starts[0].1));
    let calls: Vec<_> = events
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::ModelCall(data) => Some(data),
            _ => None,
        })
        .collect();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].request_id, starts[1].1);
    assert_eq!(model_calls.load(Ordering::SeqCst), 1);
    assert!(echoed(&events, "STEER-MARKER"));
    assert!(matches!(
        events.last().unwrap().kind,
        EventKind::TurnEnd(TurnEndData {
            outcome: TurnOutcome::Completed { .. },
            ..
        })
    ));
    soon(agent.close()).await.unwrap();
}

/// Holds `input/message` after InputQueue has committed `input/applied`, before
/// the dispatcher can deliver that next event to any of its subscribers.
#[derive(Default)]
struct AppliedGate {
    armed: AtomicBool,
    entered: Notify,
    release: Notify,
}
struct AppliedGatePlugin {
    gate: Arc<AppliedGate>,
    dispatcher: Key<Dispatcher>,
    input_queue: Key<InputQueue>,
}
impl AppliedGatePlugin {
    fn new(names: &Names<'_>, gate: Arc<AppliedGate>) -> rutis::Result<Self> {
        Ok(Self {
            gate,
            dispatcher: names.key()?,
            input_queue: names.key()?,
        })
    }
}
impl DynPlugin for AppliedGatePlugin {
    fn name(&self) -> &str {
        "test.applied-gate"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.dispatcher.erase(), self.input_queue.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let dispatcher = ctx.require_at(&self.dispatcher)?;
            let gate = self.gate.clone();
            dispatcher.subscribe(ctx, "test.applied-gate", &["input/message"], move |_| {
                let gate = gate.clone();
                Box::pin(async move {
                    if gate.armed.swap(false, Ordering::SeqCst) {
                        gate.entered.notify_one();
                        gate.release.notified().await;
                    }
                    Ok(())
                })
            })?;
            Ok(Effect::Done)
        })
    }
}

/// A steer applied before the request's first output but consumed by
/// ModelProvider after that output arrived does not replace the request:
/// ModelProvider decides by the output it received, and the steer joins the
/// same turn once the output ends.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_steer_consumed_after_the_first_output_waits_for_the_output_end() {
    let (model, chunks) = channel_model();
    let gate = Arc::new(AppliedGate::default());
    let mut builder = channel_builder(model.clone());
    let copy = gate.clone();
    builder.require_feature(
        Feature::new("test.applied-gate").branch("test.applied-gate", move |names| {
            Ok(Box::new(AppliedGatePlugin::new(names, copy.clone())?))
        }),
    );
    defaults::install(&mut builder);
    let agent = MinimalHost::start(&builder).await.unwrap();
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    session
        .submit("first".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    model.invoked().await;
    chunks
        .send(ModelChunk::MessageStart {
            message_id: "racing".into(),
        })
        .unwrap();
    gate.armed.store(true, Ordering::SeqCst);
    session
        .submit("STEER-MARKER".into(), InputMode::Steer, "main")
        .await
        .unwrap();
    soon(gate.entered.notified()).await;
    chunks.send(ModelChunk::Text("answer".into())).unwrap();
    chunks.send(ModelChunk::TextEnd).unwrap();
    let committed = until_events(&session, |events| {
        events
            .iter()
            .any(|event| matches!(&event.kind, EventKind::Text(data) if data.delta == "answer"))
    })
    .await;
    let text = committed.last().unwrap().sequence;
    let mut dispatch = session.dispatch_watch("main").unwrap();
    gate.release.notify_one();
    soon(dispatch.wait_for(|state| state.sequence >= text))
        .await
        .unwrap();
    let events = chain_events(&session, "main", ..).await.unwrap();
    assert_eq!(
        model_starts(&events).len(),
        1,
        "the request was not replaced"
    );
    chunks
        .send(ModelChunk::MessageStop {
            message_id: "racing".into(),
            reason: ModelStopReason::EndTurn,
        })
        .unwrap();
    drop(chunks);
    soon(session.idle("main")).await.unwrap();
    let events = chain_events(&session, "main", ..).await.unwrap();
    let starts = model_starts(&events);
    assert_eq!(starts.len(), 2);
    let first_end = events
        .iter()
        .find(|event| {
            matches!(&event.kind,
            EventKind::ModelOutputEnd(data) if data.request_id == starts[0].1
                && matches!(data.outcome, ModelOutputOutcome::Completed { .. }))
        })
        .expect("the first output completed")
        .sequence;
    assert!(first_end < starts[1].0);
    assert!(echoed(&events, "STEER-MARKER"));
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event.kind, EventKind::TurnStart(_)))
            .count(),
        1
    );
    soon(agent.close()).await.unwrap();
}

/// Holds the commit of the next `step/model-output-end` once armed: lets the
/// held `input/applied` of a steer go and waits a while for every handler to
/// consume it, recording whether they did before the terminal commits.
#[derive(Default)]
struct TerminalHold {
    armed: AtomicBool,
    applied: AtomicU64,
    consumed: AtomicBool,
}
struct TerminalHoldPlugin {
    hold: Arc<TerminalHold>,
    gate: Arc<AppliedGate>,
    timeline: Key<Timeline>,
    dispatcher: Key<Dispatcher>,
}
impl TerminalHoldPlugin {
    fn new(
        names: &Names<'_>,
        hold: Arc<TerminalHold>,
        gate: Arc<AppliedGate>,
    ) -> rutis::Result<Self> {
        Ok(Self {
            hold,
            gate,
            timeline: names.key()?,
            dispatcher: names.key()?,
        })
    }
}
impl DynPlugin for TerminalHoldPlugin {
    fn name(&self) -> &str {
        "test.terminal-hold"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.timeline.erase(), self.dispatcher.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let timeline = ctx.require_at(&self.timeline)?;
            let dispatcher = ctx.require_at(&self.dispatcher)?;
            let (hold, gate) = (self.hold.clone(), self.gate.clone());
            timeline.before(
                ctx,
                "test.terminal-hold",
                &["step/model-output-end"],
                move |draft| {
                    if hold.armed.swap(false, Ordering::SeqCst) {
                        let release = gate.clone();
                        std::thread::spawn(move || release.release.notify_one())
                            .join()
                            .unwrap();
                        let applied = hold.applied.load(Ordering::SeqCst);
                        let deadline = std::time::Instant::now() + Duration::from_millis(300);
                        while dispatcher.sequence() < applied
                            && std::time::Instant::now() < deadline
                        {
                            std::thread::sleep(Duration::from_millis(2));
                        }
                        hold.consumed
                            .store(dispatcher.sequence() >= applied, Ordering::SeqCst);
                    }
                    Ok(draft)
                },
            )?;
            Ok(Effect::Done)
        })
    }
}
/// Registers the steer gate after InputQueue and holds the request's terminal.
fn hold_terminal(builder: &mut AgentBuilder) -> (Arc<AppliedGate>, Arc<TerminalHold>) {
    let gate = Arc::new(AppliedGate::default());
    let hold = Arc::new(TerminalHold::default());
    let copy = gate.clone();
    builder.require_feature(
        Feature::new("test.applied-gate").branch("test.applied-gate", move |names| {
            Ok(Box::new(AppliedGatePlugin::new(names, copy.clone())?))
        }),
    );
    let (copy, held) = (gate.clone(), hold.clone());
    builder.require_feature(Feature::new("test.terminal-hold").branch(
        "test.terminal-hold",
        move |names| {
            Ok(Box::new(TerminalHoldPlugin::new(
                names,
                held.clone(),
                copy.clone(),
            )?))
        },
    ));
    (gate, hold)
}
/// Applies a steer whose dispatch the gate holds, and arms the hold of the
/// next terminal with it.
async fn hold_steer(session: &Session, gate: &AppliedGate, hold: &TerminalHold) {
    gate.armed.store(true, Ordering::SeqCst);
    let steer = session
        .submit("STEER-MARKER".into(), InputMode::Steer, "main")
        .await
        .unwrap();
    soon(gate.entered.notified()).await;
    let events = chain_events(session, "main", ..).await.unwrap();
    let applied = events
        .iter()
        .find(|event| matches!(&event.kind, EventKind::InputApplied(data) if data.input_id == steer.id))
        .expect("the steer is applied")
        .sequence;
    hold.applied.store(applied, Ordering::SeqCst);
    hold.armed.store(true, Ordering::SeqCst);
}
/// The steer was not consumed while the request's terminal was committed:
/// the terminal is the failure its owner saw, and the steer still reaches
/// the model in a later request.
fn assert_steer_after_terminal(events: &[Arc<AgentEvent>], hold: &TerminalHold, error: &str) {
    assert!(!hold.armed.load(Ordering::SeqCst), "the terminal was held");
    assert!(
        !hold.consumed.load(Ordering::SeqCst),
        "a steer consumed before the terminal commits must decide it"
    );
    let first = &model_starts(events)[0].1;
    assert!(events.iter().any(|event| matches!(&event.kind,
        EventKind::ModelOutputEnd(ModelOutputEndData {
            request_id,
            outcome: ModelOutputOutcome::Failed { error: failed, .. },
            ..
        }) if request_id == first && failed == error)));
    assert!(echoed(events, "STEER-MARKER"));
}

/// A model call that fails without output and a steer consumed by
/// ModelProvider meanwhile: the final recheck, the terminal and releasing
/// the call are one step under ModelProvider's state, so the steer is
/// consumed either before it (and replaces the request) or after it; never
/// in between, where it would find no running call yet not decide the
/// terminal (time-bus §4).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_steer_is_not_consumed_between_a_failed_call_and_its_terminal() {
    let (model, chunks) = channel_model();
    let mut builder = channel_builder(model.clone());
    let (gate, hold) = hold_terminal(&mut builder);
    defaults::install(&mut builder);
    let agent = MinimalHost::start(&builder).await.unwrap();
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    session
        .submit("first".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    model.invoked().await;
    hold_steer(&session, &gate, &hold).await;
    drop(chunks);
    soon(session.idle("main")).await.unwrap();
    let events = chain_events(&session, "main", ..).await.unwrap();
    assert_steer_after_terminal(&events, &hold, "Provider stream ended without message_stop");
    soon(agent.close()).await.unwrap();
}

/// Fails its first preparation once released; later ones pass through.
#[derive(Default)]
struct FailingPrepare {
    started: Notify,
    fail: Notify,
    calls: AtomicUsize,
}
impl PrepareContext for FailingPrepare {
    fn prepare(
        &self,
        request: Arc<ModelRequest>,
        _: ContextSource,
        _: CancellationToken,
    ) -> HostFuture<'_, Arc<ModelRequest>> {
        Box::pin(async move {
            if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                self.started.notify_one();
                self.fail.notified().await;
                return Err(HostError::failed("preparation failed"));
            }
            Ok(request)
        })
    }
}

/// The same for a preparation that fails: ContextPreparation keeps the
/// request until its terminal is committed, under the state its steer
/// handler takes.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_steer_is_not_consumed_between_a_failed_preparation_and_its_terminal() {
    let prepare = Arc::new(FailingPrepare::default());
    let mut builder = AgentBuilder::new(AgentConfig::default());
    builder
        .session_value(Arc::new(HistoryRenderer::default()))
        .process_value(Arc::new(ModelHost(Arc::new(EchoModel))))
        .session_value(Arc::new(PrepareContextHost(prepare.clone())))
        .session_value(Arc::new(ContextModelHost(None)))
        .session_value(Arc::new(CompactionStrategyHost(None)))
        .require_feature(
            Feature::new("PrepareModelHost").session("PrepareModelHost", |names| {
                Ok(Box::new(DefaultPrepareModelPlugin::new(names)?))
            }),
        );
    defaults::tool_host(&mut builder, Arc::new(MockTools));
    let (gate, hold) = hold_terminal(&mut builder);
    defaults::install(&mut builder);
    let agent = MinimalHost::start(&builder).await.unwrap();
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    session
        .submit("first".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    soon(prepare.started.notified()).await;
    hold_steer(&session, &gate, &hold).await;
    prepare.fail.notify_one();
    soon(session.idle("main")).await.unwrap();
    let events = chain_events(&session, "main", ..).await.unwrap();
    assert_steer_after_terminal(&events, &hold, "preparation failed");
    soon(agent.close()).await.unwrap();
}

/// `stop_turn` stops the open turn it names and writes nothing for another id
/// or for a turn that already ended.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stop_turn_stops_only_the_open_turn_it_names() {
    let (model, chunks) = channel_model();
    let mut builder = channel_builder(model.clone());
    defaults::install(&mut builder);
    let agent = MinimalHost::start(&builder).await.unwrap();
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    session
        .submit("go".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    let cancel = model.invoked().await;
    let events = until_events(&session, |events| {
        events
            .iter()
            .any(|event| matches!(&event.kind, EventKind::TurnStart(_)))
    })
    .await;
    let turn = events
        .iter()
        .find_map(|event| match &event.kind {
            EventKind::TurnStart(data) => Some(data.turn_id.clone()),
            _ => None,
        })
        .expect("the turn started");
    let control = session
        .service::<dim_agent_capabilities::agent_loop::AgentLoop>("main")
        .unwrap();

    // Another id names no open turn: nothing is written.
    assert_eq!(
        control
            .stop_turn("not-the-open-turn", "operator stop".into())
            .unwrap(),
        None
    );
    let events = chain_events(&session, "main", ..).await.unwrap();
    assert!(
        !events
            .iter()
            .any(|event| matches!(&event.kind, EventKind::StopInterrupt(_)))
    );

    // The open turn is stopped, and its model call is cancelled.
    let sequence = control
        .stop_turn(&turn, "operator stop".into())
        .unwrap()
        .expect("the open turn is stopped");
    soon(cancel.cancelled()).await;
    drop(chunks);
    let events = until_events(&session, |events| {
        events
            .iter()
            .any(|event| matches!(&event.kind, EventKind::TurnEnd(_)))
    })
    .await;
    assert!(events.iter().any(|event| {
        event.sequence == sequence
            && matches!(&event.kind, EventKind::StopInterrupt(data)
                if data.reason == "operator stop" && data.scope == InterruptScope::Turn)
    }));
    assert!(matches!(
        events.last().unwrap().kind,
        EventKind::TurnEnd(TurnEndData {
            outcome: TurnOutcome::Cancelled { .. },
            ..
        })
    ));

    // The same turn has ended: naming it again writes nothing.
    let stops = events
        .iter()
        .filter(|event| matches!(&event.kind, EventKind::StopInterrupt(_)))
        .count();
    assert_eq!(
        control
            .stop_turn(&turn, "operator stop again".into())
            .unwrap(),
        None
    );
    let events = chain_events(&session, "main", ..).await.unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(&event.kind, EventKind::StopInterrupt(_)))
            .count(),
        stops
    );
    soon(agent.close()).await.unwrap();
}

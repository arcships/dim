use dim_agent_capabilities::{
    adapters::{DefaultPrepareModelPlugin, EchoModel, MockTools},
    defaults,
    host::*,
    tool_registry::definition,
};
use dim_agent_runtime::testing::MinimalHost;
use dim_agent_runtime::testing::chain_events;
use dim_agent_runtime::*;
use dim_agent_types::*;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, Key, TypeKey};
use rutis::{Ctx, Plugin};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

fn builder(model: Arc<dyn ModelDriver>, tools: Arc<dyn ToolDriver>) -> AgentBuilder {
    let mut builder = AgentBuilder::new(AgentConfig::default());
    builder
        .process_value(Arc::new(ModelHost(model)))
        .session_value(Arc::new(ToolHost(tools)))
        .session_value(Arc::new(
            dim_agent_capabilities::history::HistoryRenderer::default(),
        ))
        .session_value(Arc::new(PrepareContextHost(Arc::new(IdentityContext))))
        .session_value(Arc::new(ContextModelHost(None)))
        .session_value(Arc::new(CompactionStrategyHost(None)))
        .require_feature(
            Feature::new("PrepareModelHost").session("PrepareModelHost", |names| {
                Ok(Box::new(DefaultPrepareModelPlugin::new(names)?))
            }),
        );
    defaults::install(&mut builder);
    builder
}

struct Rewrite {
    timeline: Key<Timeline>,
}
impl Rewrite {
    fn new(names: &Names<'_>) -> rutis::Result<Self> {
        Ok(Self {
            timeline: names.key()?,
        })
    }
}
impl DynPlugin for Rewrite {
    fn name(&self) -> &str {
        "Rewrite"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.timeline.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let timeline = ctx.require_at(&self.timeline)?;
            timeline.before(ctx, "rewrite-model", &["model/call"], |mut draft| {
                if let EventKind::ModelCall(call) = &mut draft.kind {
                    Arc::make_mut(&mut call.request).model_id = "final-model".into();
                }
                Ok(draft)
            })?;
            Ok(Effect::Done)
        })
    }
}
struct RecordingModel(Arc<Mutex<Vec<Arc<ModelRequest>>>>);

/// Hands the test its branch Timeline, so a flow can write a fact only a
/// later capability writes today, such as a continuation's
/// `input/consumed`.
struct TimelineHandle {
    timeline: Key<Timeline>,
    slot: Arc<Mutex<Option<Timeline>>>,
}
impl DynPlugin for TimelineHandle {
    fn name(&self) -> &str {
        "test.timeline-handle"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.timeline.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let timeline = ctx.require_at(&self.timeline)?;
            *self.slot.lock().unwrap() = Some((*timeline).clone());
            Ok(Effect::Done)
        })
    }
}

struct OutputLimitedModel;
impl ModelDriver for OutputLimitedModel {
    fn invoke(
        &self,
        request: Arc<ModelRequest>,
        connection: ModelConnection,
        cancel: CancellationToken,
    ) -> HostFuture<'_, ModelStream> {
        if request.config.as_ref().and_then(|config| config.max_tokens) == Some(16.0) {
            Box::pin(async {
                Ok(Box::pin(futures_util::stream::iter([
                    Ok(ModelChunk::MessageStart {
                        message_id: "limited".into(),
                    }),
                    Ok(ModelChunk::Text("partial output".into())),
                    Ok(ModelChunk::MessageStop {
                        message_id: "limited".into(),
                        reason: ModelStopReason::MaxTokens,
                    }),
                ])) as ModelStream)
            })
        } else {
            EchoModel.invoke(request, connection, cancel)
        }
    }
}

/// How [`TruncatingModel`] cuts the reply it is asked for: the first request
/// is cut, and every later request echoes, so a turn that continued at all
/// shows as a second start.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Cut {
    /// A `write` call is in flight when the provider stops at max_tokens.
    CallAtMaxTokens,
    /// The stream reports unparseable arguments for the call it announced.
    UnparseableCall,
    /// The stream stops normally while the announced arguments are partial.
    IncompleteCallAtStop,
    /// max_tokens with no call and no answer text.
    NothingAtMaxTokens,
    /// max_tokens after partial answer text.
    TextAtMaxTokens,
}

struct TruncatingModel {
    cut: Cut,
    /// How many requests are cut before the model answers normally.
    cuts: usize,
    seen: std::sync::atomic::AtomicUsize,
}

fn malformed_call() -> HostError {
    HostError::Failed {
        message: "The provider sent a response Dim could not use: invalid arguments for tool \
                  'echo'."
            .into(),
        budget: None,
        kind: HostErrorKind::Other,
        model: Some(Box::new(ModelFailureFacts {
            kind: ModelFailureKind::MalformedResponse,
            reason: Some("invalid arguments for tool 'echo'".into()),
            status: None,
            provider_code: None,
            request_id: None,
            cause: Vec::new(),
            retry_after: None,
            attempts: 1,
            waited: std::time::Duration::ZERO,
            retry_after_stop: None,
            retry_forbidden: false,
        })),
        presentation: None,
    }
}

fn cut_chunks(cut: Cut) -> Vec<std::result::Result<ModelChunk, HostError>> {
    let start = ModelChunk::MessageStart {
        message_id: "cut".into(),
    };
    let announce = |chunks: &mut Vec<std::result::Result<ModelChunk, HostError>>| {
        chunks.push(Ok(ModelChunk::ToolStart {
            id: "call-1".into(),
            name: "echo".into(),
        }));
        chunks.push(Ok(ModelChunk::ToolArguments {
            id: "call-1".into(),
            name: "echo".into(),
            delta: "{\"text\":\"hel".into(),
        }));
    };
    let stop = |reason| {
        Some(Ok(ModelChunk::MessageStop {
            message_id: "cut".into(),
            reason,
        }))
    };
    let mut chunks = vec![Ok(start)];
    match cut {
        Cut::CallAtMaxTokens => {
            announce(&mut chunks);
            chunks.extend(stop(ModelStopReason::MaxTokens));
        }
        Cut::UnparseableCall => {
            announce(&mut chunks);
            chunks.push(Err(malformed_call()));
        }
        Cut::IncompleteCallAtStop => {
            announce(&mut chunks);
            chunks.extend(stop(ModelStopReason::ToolUse));
        }
        Cut::NothingAtMaxTokens => chunks.extend(stop(ModelStopReason::MaxTokens)),
        Cut::TextAtMaxTokens => {
            chunks.push(Ok(ModelChunk::Text("half an answer".into())));
            chunks.extend(stop(ModelStopReason::MaxTokens));
        }
    }
    chunks
}

impl ModelDriver for TruncatingModel {
    fn invoke(
        &self,
        request: Arc<ModelRequest>,
        connection: ModelConnection,
        cancel: CancellationToken,
    ) -> HostFuture<'_, ModelStream> {
        let seen = self.seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if seen >= self.cuts {
            return EchoModel.invoke(request, connection, cancel);
        }
        let chunks = cut_chunks(self.cut);
        Box::pin(async move { Ok(Box::pin(futures_util::stream::iter(chunks)) as ModelStream) })
    }
}

fn starts(events: &[std::sync::Arc<AgentEvent>]) -> usize {
    events
        .iter()
        .filter(|event| matches!(event.kind, EventKind::ModelStart(_)))
        .count()
}

fn turn_outcome(events: &[std::sync::Arc<AgentEvent>]) -> Option<TurnOutcome> {
    events.iter().rev().find_map(|event| match &event.kind {
        EventKind::TurnEnd(data) => Some(data.outcome.clone()),
        _ => None,
    })
}

async fn run_cut(cut: Cut) -> (Vec<std::sync::Arc<AgentEvent>>, MinimalHost) {
    run_cuts(cut, 1).await
}

/// One turn against a model that cuts its first `cuts` requests.
async fn run_cuts(cut: Cut, cuts: usize) -> (Vec<std::sync::Arc<AgentEvent>>, MinimalHost) {
    let model = TruncatingModel {
        cut,
        cuts,
        seen: std::sync::atomic::AtomicUsize::new(0),
    };
    let agent = MinimalHost::start(&builder(Arc::new(model), Arc::new(MockTools)))
        .await
        .unwrap();
    let session = agent
        .create_session(SessionConfig::default())
        .await
        .unwrap();
    session
        .submit("cut".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    session.idle("main").await.unwrap();
    let events = chain_events(&session, "main", ..).await.unwrap();
    (events, agent)
}

/// A model whose first reply waits until the test says so, then fails with a
/// truncated call; later requests echo. It signals when the first request has
/// arrived, so an input can be applied while that request is in flight.
struct BlockingModel {
    arrived: Arc<tokio::sync::Notify>,
    gate: Arc<tokio::sync::Notify>,
    seen: std::sync::atomic::AtomicUsize,
}

impl ModelDriver for BlockingModel {
    fn invoke(
        &self,
        request: Arc<ModelRequest>,
        connection: ModelConnection,
        cancel: CancellationToken,
    ) -> HostFuture<'_, ModelStream> {
        let seen = self.seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if seen > 0 {
            return EchoModel.invoke(request, connection, cancel);
        }
        let arrived = self.arrived.clone();
        let gate = self.gate.clone();
        Box::pin(async move {
            arrived.notify_waiters();
            gate.notified().await;
            let chunks = cut_chunks(Cut::UnparseableCall);
            Ok(Box::pin(futures_util::stream::iter(chunks)) as ModelStream)
        })
    }
}

#[tokio::test]
async fn a_steer_applied_before_a_failed_step_takes_over_instead_of_continuing() {
    let arrived = Arc::new(tokio::sync::Notify::new());
    let gate = Arc::new(tokio::sync::Notify::new());
    let model = BlockingModel {
        arrived: arrived.clone(),
        gate: gate.clone(),
        seen: std::sync::atomic::AtomicUsize::new(0),
    };
    let agent = MinimalHost::start(&builder(Arc::new(model), Arc::new(MockTools)))
        .await
        .unwrap();
    let session = agent
        .create_session(SessionConfig::default())
        .await
        .unwrap();
    let waiting = arrived.notified();
    session
        .submit("cut".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    waiting.await;
    // The reply is in flight. A steer applied now must take over the turn
    // rather than be read as consent to continue the failed reply.
    session
        .submit("instead".into(), InputMode::Steer, "main")
        .await
        .unwrap();
    gate.notify_waiters();
    session.idle("main").await.unwrap();
    let events = chain_events(&session, "main", ..).await.unwrap();
    assert_eq!(
        starts(&events),
        2,
        "the cut request and the steer's own request, no continuation"
    );
    assert!(matches!(
        turn_outcome(&events),
        Some(TurnOutcome::Completed { .. })
    ));
    agent.close().await.unwrap();
}

/// Records every request's messages, then cuts the first one like
/// `TruncatingModel`.
struct RecordingCutModel {
    cut: Cut,
    requests: Arc<std::sync::Mutex<Vec<Vec<String>>>>,
    seen: std::sync::atomic::AtomicUsize,
}

impl ModelDriver for RecordingCutModel {
    fn invoke(
        &self,
        request: Arc<ModelRequest>,
        connection: ModelConnection,
        cancel: CancellationToken,
    ) -> HostFuture<'_, ModelStream> {
        let rendered: Vec<String> = request
            .messages
            .iter()
            .flat_map(|message| match &message.content {
                MessageContent::Parts(parts) => parts
                    .iter()
                    .map(|part| format!("{part:?}"))
                    .collect::<Vec<_>>(),
                MessageContent::Text(text) => vec![text.clone()],
            })
            .collect();
        self.requests.lock().unwrap().push(rendered);
        let seen = self.seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if seen > 0 {
            return EchoModel.invoke(request, connection, cancel);
        }
        let chunks = cut_chunks(self.cut);
        Box::pin(async move { Ok(Box::pin(futures_util::stream::iter(chunks)) as ModelStream) })
    }
}

#[tokio::test]
async fn the_continuation_reads_the_closure_result_of_the_announced_call() {
    let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
    let model = RecordingCutModel {
        cut: Cut::CallAtMaxTokens,
        requests: requests.clone(),
        seen: std::sync::atomic::AtomicUsize::new(0),
    };
    let agent = MinimalHost::start(&builder(Arc::new(model), Arc::new(MockTools)))
        .await
        .unwrap();
    let session = agent
        .create_session(SessionConfig::default())
        .await
        .unwrap();
    session
        .submit("cut".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    session.idle("main").await.unwrap();
    let rendered = requests.lock().unwrap().clone();
    assert_eq!(rendered.len(), 2, "the turn continued once");
    let second = &rendered[1];
    assert!(
        second
            .iter()
            .any(|part| part.contains("Tool was not executed")),
        "the continuation reads the per-call error: {second:?}"
    );
    assert!(
        second
            .iter()
            .any(|part| part.contains("call-1") && part.starts_with("ToolResult")),
        "the result is addressed to the announced call: {second:?}"
    );
    agent.close().await.unwrap();
}

#[tokio::test]
async fn an_incomplete_call_reports_a_malformed_response() {
    // A stream that stops normally with partial arguments is the same
    // user-visible defect as one that reports unparseable arguments, so it
    // carries the same failure kind instead of `unknown`.
    let (events, agent) = run_cuts(Cut::IncompleteCallAtStop, 2).await;
    let failure = events
        .iter()
        .find_map(|event| match &event.kind {
            EventKind::ModelOutputEnd(data) => match &data.outcome {
                ModelOutputOutcome::Failed {
                    failure: Some(failure),
                    ..
                } => Some(failure.clone()),
                _ => None,
            },
            _ => None,
        })
        .expect("a failed terminal carrying facts");
    assert_eq!(failure.kind, ModelFailureKind::MalformedResponse);
    assert!(
        failure
            .reason
            .as_deref()
            .is_some_and(|reason| reason.contains("before its tool arguments were complete")),
        "{:?}",
        failure.reason
    );
    agent.close().await.unwrap();
}

#[tokio::test]
async fn a_call_announced_and_never_executed_continues_the_turn_once() {
    for cut in [
        Cut::CallAtMaxTokens,
        Cut::UnparseableCall,
        Cut::IncompleteCallAtStop,
    ] {
        let (events, agent) = run_cut(cut).await;
        assert_eq!(starts(&events), 2, "{cut:?}: the turn continues once");
        assert!(matches!(
            turn_outcome(&events),
            Some(TurnOutcome::Completed { .. })
        ));
        // The batch that closed the announced call is committed before the
        // second request starts, and its error text is what the model reads.
        let closed = events
            .iter()
            .position(|event| {
                matches!(&event.kind, EventKind::ToolResult(data) if data.call_event_id.is_none())
            })
            .expect("the announced call is closed");
        let second = events
            .iter()
            .enumerate()
            .filter(|(_, event)| matches!(event.kind, EventKind::ModelStart(_)))
            .nth(1)
            .map(|(index, _)| index)
            .expect("a second request");
        assert!(
            closed < second,
            "{cut:?}: the result precedes the continuation"
        );
        let ToolResultData { results, .. } = events
            .iter()
            .find_map(|event| match &event.kind {
                EventKind::ToolResult(data) if data.call_event_id.is_none() => Some(data.clone()),
                _ => None,
            })
            .expect("closed calls");
        assert_eq!(results.len(), 1, "{cut:?}");
        assert_eq!(results[0].result.is_error, Some(true), "{cut:?}");
        assert!(
            matches!(&results[0].result.content, ToolContent::Text(text)
                if text.starts_with("Tool was not executed:")),
            "{cut:?}: {:?}",
            results[0].result.content
        );
        agent.close().await.unwrap();
    }
}

/// A model that announces one call and fails before its arguments complete, so
/// nothing reaches a tool. Its provider policy decides whether retrying that
/// failure is forbidden, and every later request echoes.
#[derive(Clone)]
struct AnnouncingFailure {
    failure: ModelFailureKind,
    vetoed: bool,
    seen: Arc<std::sync::atomic::AtomicUsize>,
}
impl ModelFailurePolicy for AnnouncingFailure {
    fn forbids_retry(&self, _: &HostError) -> bool {
        self.vetoed
    }
}
impl ResolveModelConnection for AnnouncingFailure {
    fn resolve(
        &self,
        _: Arc<ModelRequest>,
        _: CancellationToken,
    ) -> HostFuture<'_, ResolvedModelConnection> {
        Box::pin(async move {
            Ok(ResolvedModelConnection {
                connection: ModelConnection::Local,
                failure_policy: Some(Arc::new(self.clone())),
                recovery: None,
            })
        })
    }
}
impl ModelDriver for AnnouncingFailure {
    fn invoke(
        &self,
        request: Arc<ModelRequest>,
        connection: ModelConnection,
        cancel: CancellationToken,
    ) -> HostFuture<'_, ModelStream> {
        if self.seen.fetch_add(1, Ordering::SeqCst) > 0 {
            return EchoModel.invoke(request, connection, cancel);
        }
        let error = HostError::Failed {
            message: "fixture rejection".into(),
            budget: None,
            kind: HostErrorKind::Other,
            model: Some(Box::new(ModelFailureFacts {
                kind: self.failure,
                reason: Some("fixture rejection".into()),
                status: None,
                provider_code: None,
                request_id: None,
                cause: Vec::new(),
                retry_after: None,
                attempts: 1,
                waited: Duration::ZERO,
                retry_after_stop: None,
                retry_forbidden: false,
            })),
            presentation: None,
        };
        Box::pin(async move {
            Ok(Box::pin(futures_util::stream::iter(vec![
                Ok(ModelChunk::MessageStart {
                    message_id: "announced".into(),
                }),
                Ok(ModelChunk::ToolStart {
                    id: "call-1".into(),
                    name: "echo".into(),
                }),
                Ok(ModelChunk::ToolArguments {
                    id: "call-1".into(),
                    name: "echo".into(),
                    delta: "{\"text\":\"hel".into(),
                }),
                Err(error),
            ])) as ModelStream)
        })
    }
}

struct AnnouncingHosts(AnnouncingFailure);
impl Plugin for AnnouncingHosts {
    type Deps = ();
    fn name(&self) -> &str {
        "test.announcing-failure"
    }
    async fn apply(&self, ctx: &Ctx, (): Self::Deps) -> rutis::Result {
        ctx.provide(ModelHost(Arc::new(self.0.clone())))?;
        ctx.provide(ProviderConnectionResolver(Arc::new(self.0.clone())))?;
        Ok(())
    }
}

/// One turn against [`AnnouncingFailure`], with its policy on the invocation
/// path as in production.
async fn run_announcing_failure(
    failure: ModelFailureKind,
    vetoed: bool,
) -> (Vec<std::sync::Arc<AgentEvent>>, MinimalHost) {
    let model = AnnouncingFailure {
        failure,
        vetoed,
        seen: Default::default(),
    };
    let mut builder = AgentBuilder::new(AgentConfig::default());
    builder
        .session_value(Arc::new(ToolHost(Arc::new(MockTools))))
        .session_value(Arc::new(
            dim_agent_capabilities::history::HistoryRenderer::default(),
        ))
        .session_value(Arc::new(PrepareContextHost(Arc::new(IdentityContext))))
        .session_value(Arc::new(ContextModelHost(None)))
        .session_value(Arc::new(CompactionStrategyHost(None)))
        .require_feature(
            Feature::new("PrepareModelHost").session("PrepareModelHost", |names| {
                Ok(Box::new(DefaultPrepareModelPlugin::new(names)?))
            }),
        );
    defaults::install_with_process_model(&mut builder);
    let hosts = model.clone();
    builder.plugin(move |_| Ok(Box::new(AnnouncingHosts(hosts.clone()))));
    let agent = MinimalHost::start(&builder).await.unwrap();
    let session = agent
        .create_session(SessionConfig::default())
        .await
        .unwrap();
    session
        .submit("announced".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    session.idle("main").await.unwrap();
    let events = chain_events(&session, "main", ..).await.unwrap();
    (events, agent)
}

#[tokio::test]
async fn a_vetoed_failure_does_not_continue_the_turn() {
    let (events, agent) = run_announcing_failure(ModelFailureKind::ServerError, true).await;
    assert_eq!(starts(&events), 1, "the provider vetoed the continuation");
    assert!(matches!(
        turn_outcome(&events),
        Some(TurnOutcome::Failed { .. })
    ));
    // The veto does not change how the announced call is closed.
    let closed = events
        .iter()
        .find_map(|event| match &event.kind {
            EventKind::ToolResult(data) if data.call_event_id.is_none() => Some(data.clone()),
            _ => None,
        })
        .expect("the announced call is closed");
    assert_eq!(closed.results.len(), 1);
    assert_eq!(closed.results[0].result.is_error, Some(true));
    agent.close().await.unwrap();
}

#[tokio::test]
async fn a_failure_no_new_request_can_settle_ends_the_turn() {
    for kind in [
        ModelFailureKind::AuthRejected,
        ModelFailureKind::QuotaExhausted,
        ModelFailureKind::NotFound,
        ModelFailureKind::InvalidRequest,
        ModelFailureKind::ContentFiltered,
        ModelFailureKind::ContextTooLong,
    ] {
        let (events, agent) = run_announcing_failure(kind, false).await;
        assert_eq!(starts(&events), 1, "{kind:?}: no continuation");
        assert!(
            matches!(turn_outcome(&events), Some(TurnOutcome::Failed { .. })),
            "{kind:?}: {:?}",
            turn_outcome(&events)
        );
        agent.close().await.unwrap();
    }
}

#[tokio::test]
async fn a_failure_a_new_request_can_settle_continues_the_turn() {
    for kind in [
        ModelFailureKind::Network,
        ModelFailureKind::RateLimited,
        ModelFailureKind::ServerError,
        ModelFailureKind::MalformedResponse,
        ModelFailureKind::Unknown,
    ] {
        let (events, agent) = run_announcing_failure(kind, false).await;
        assert_eq!(starts(&events), 2, "{kind:?}: the turn continues once");
        agent.close().await.unwrap();
    }
}

/// A model whose first request spends three attempts with no output and then
/// announces a call and fails, so that request ends with part of the chain
/// spent. Every later request fails with no output at all, so a continuation
/// carrying the remainder stops sooner than one with a fresh budget.
#[derive(Clone)]
struct QuietThenAnnounced {
    announced: Arc<AtomicBool>,
    calls: Arc<std::sync::atomic::AtomicUsize>,
}
impl QuietThenAnnounced {
    fn failure() -> HostError {
        HostError::Failed {
            message: "fixture rejection".into(),
            budget: None,
            kind: HostErrorKind::Http {
                status: Some(503),
                code: None,
            },
            model: Some(Box::new(ModelFailureFacts {
                kind: ModelFailureKind::ServerError,
                reason: Some("fixture rejection".into()),
                status: Some(503),
                provider_code: None,
                request_id: None,
                cause: Vec::new(),
                retry_after: None,
                attempts: 1,
                waited: Duration::ZERO,
                retry_after_stop: None,
                retry_forbidden: false,
            })),
            presentation: None,
        }
    }
}
impl ModelDriver for QuietThenAnnounced {
    fn invoke(
        &self,
        _: Arc<ModelRequest>,
        _: ModelConnection,
        _: CancellationToken,
    ) -> HostFuture<'_, ModelStream> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let announce = call == 3 && !self.announced.swap(true, Ordering::SeqCst);
        if !announce {
            return Box::pin(async { Err(Self::failure()) });
        }
        let error = Self::failure();
        Box::pin(async move {
            Ok(Box::pin(futures_util::stream::iter(vec![
                Ok(ModelChunk::MessageStart {
                    message_id: "announced".into(),
                }),
                Ok(ModelChunk::ToolStart {
                    id: "call-1".into(),
                    name: "echo".into(),
                }),
                Ok(ModelChunk::ToolArguments {
                    id: "call-1".into(),
                    name: "echo".into(),
                    delta: "{\"text\":\"hel".into(),
                }),
                Err(error),
            ])) as ModelStream)
        })
    }
}
impl ResolveModelConnection for QuietThenAnnounced {
    fn resolve(
        &self,
        _: Arc<ModelRequest>,
        _: CancellationToken,
    ) -> HostFuture<'_, ResolvedModelConnection> {
        Box::pin(async move {
            Ok(ResolvedModelConnection {
                connection: ModelConnection::Local,
                failure_policy: None,
                recovery: None,
            })
        })
    }
}
struct QuietThenAnnouncedHosts(QuietThenAnnounced);
impl Plugin for QuietThenAnnouncedHosts {
    type Deps = ();
    fn name(&self) -> &str {
        "test.quiet-then-announced"
    }
    async fn apply(&self, ctx: &Ctx, (): Self::Deps) -> rutis::Result {
        ctx.provide(ModelHost(Arc::new(self.0.clone())))?;
        ctx.provide(ProviderConnectionResolver(Arc::new(self.0.clone())))?;
        Ok(())
    }
}

#[tokio::test(start_paused = true)]
async fn a_continuation_spends_the_rest_of_the_failed_request_chain() {
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut builder = AgentBuilder::new(AgentConfig::default());
    builder
        .session_value(Arc::new(ToolHost(Arc::new(MockTools))))
        .session_value(Arc::new(
            dim_agent_capabilities::history::HistoryRenderer::default(),
        ))
        .session_value(Arc::new(PrepareContextHost(Arc::new(IdentityContext))))
        .session_value(Arc::new(ContextModelHost(None)))
        .session_value(Arc::new(CompactionStrategyHost(None)))
        .require_feature(
            Feature::new("PrepareModelHost").session("PrepareModelHost", |names| {
                Ok(Box::new(DefaultPrepareModelPlugin::new(names)?))
            }),
        );
    defaults::install_with_process_model(&mut builder);
    let hosts = QuietThenAnnounced {
        announced: Arc::new(AtomicBool::new(false)),
        calls: calls.clone(),
    };
    builder.plugin(move |_| Ok(Box::new(QuietThenAnnouncedHosts(hosts.clone()))));
    let agent = MinimalHost::start(&builder).await.unwrap();
    let session = agent
        .create_session(SessionConfig::default())
        .await
        .unwrap();
    session
        .submit("announced".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    session.idle("main").await.unwrap();
    let events = chain_events(&session, "main", ..).await.unwrap();
    assert_eq!(starts(&events), 2, "the turn continues once");
    // 4 calls ended the failed request: three quiet ones and the announced
    // one. The continuation carries those four attempts, so it has four of
    // the eight left; a fresh chain would have given it eight.
    assert_eq!(calls.load(Ordering::SeqCst), 8, "one shared chain");
    let ends: Vec<_> = events
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::ModelOutputEnd(data) => match &data.outcome {
                ModelOutputOutcome::Failed {
                    failure: Some(failure),
                    ..
                } => Some(failure.clone()),
                _ => None,
            },
            _ => None,
        })
        .collect();
    assert_eq!(ends.len(), 2, "{ends:?}");
    assert_eq!(ends[0].attempts, 4, "{:?}", ends[0]);
    assert_eq!(ends[1].attempts, 8, "{:?}", ends[1]);
    assert!(
        ends[1].waited_ms > ends[0].waited_ms,
        "the continuation reports the chain's total wait: {:?}",
        ends[1]
    );
    agent.close().await.unwrap();
}

#[tokio::test]
async fn a_second_truncated_call_settles_the_turn() {
    for (cut, expected) in [
        (Cut::UnparseableCall, false),
        (Cut::IncompleteCallAtStop, false),
        (Cut::CallAtMaxTokens, true),
    ] {
        let (events, agent) = run_cuts(cut, 2).await;
        assert_eq!(starts(&events), 2, "{cut:?}: the slot is spent once");
        match (turn_outcome(&events), expected) {
            (Some(TurnOutcome::Completed { reason }), true) => assert_eq!(reason, "output_limit"),
            (Some(TurnOutcome::Failed { .. }), false) => {}
            (other, _) => panic!("{cut:?}: {other:?}"),
        }
        agent.close().await.unwrap();
    }
}

#[tokio::test]
async fn an_output_limit_with_no_call_completes_the_turn() {
    for cut in [Cut::NothingAtMaxTokens, Cut::TextAtMaxTokens] {
        let (events, agent) = run_cut(cut).await;
        assert_eq!(starts(&events), 1, "{cut:?}: nothing to continue");
        assert!(
            matches!(&turn_outcome(&events), Some(TurnOutcome::Completed { reason })
                if reason == "output_limit"),
            "{cut:?}: {:?}",
            turn_outcome(&events)
        );
        // The failure fact stays for diagnostics.
        assert!(
            events.iter().any(|event| matches!(&event.kind,
            EventKind::ModelOutputEnd(data) if matches!(
                &data.outcome,
                ModelOutputOutcome::Failed { failure: Some(failure), .. }
                    if failure.kind == ModelFailureKind::OutputLimit
            ))),
            "{cut:?}"
        );
        agent.close().await.unwrap();
    }
}

#[tokio::test]
async fn a_max_tokens_turn_keeps_the_session_usable_with_a_larger_output_limit() {
    let agent = MinimalHost::start(&builder(Arc::new(OutputLimitedModel), Arc::new(MockTools)))
        .await
        .unwrap();
    let mut config = SessionConfig::default();
    config.model.config = Some(ModelConfig {
        max_tokens: Some(16.0),
        ..Default::default()
    });
    let mut model = config.model.clone();
    let session = agent.create_session(config).await.unwrap();
    session
        .submit("first".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    session.idle("main").await.unwrap();
    let history = chain_events(&session, "main", ..).await.unwrap();
    assert!(history.iter().any(|event| matches!(&event.kind,
        EventKind::ModelOutputEnd(ModelOutputEndData { outcome: ModelOutputOutcome::Failed { error, budget: None, presentation: Some(presentation), failure: Some(failure) }, .. })
        if error.contains(&model.provider_id) && error.contains(&model.model_id)
            && presentation.source == model.provider_id && presentation.reason == "max_tokens"
            && presentation.action.as_deref().is_some_and(|action| action.contains("output token limit"))
            && failure.kind == ModelFailureKind::OutputLimit
            && failure.reason.as_deref().is_some_and(|reason| reason.contains("max_tokens")))));
    assert!(matches!(
        &history
            .iter()
            .rev()
            .find(|event| matches!(event.kind, EventKind::TurnEnd(_)))
            .unwrap()
            .kind,
        EventKind::TurnEnd(TurnEndData {
            outcome: TurnOutcome::Completed { reason },
            ..
        }) if reason == "output_limit"
    ));
    model.config.as_mut().unwrap().max_tokens = Some(128.0);
    session
        .configure(
            ConfigureUpdate {
                model: Some(model),
                ..Default::default()
            },
            "main",
        )
        .await
        .unwrap();
    session
        .submit("continue".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    session.idle("main").await.unwrap();
    let history = chain_events(&session, "main", ..).await.unwrap();
    assert!(matches!(
        &history
            .iter()
            .rev()
            .find(|event| matches!(event.kind, EventKind::TurnEnd(_)))
            .unwrap()
            .kind,
        EventKind::TurnEnd(TurnEndData {
            outcome: TurnOutcome::Completed { .. },
            ..
        })
    ));
    agent.close().await.unwrap();
}

struct LongFailure;
impl ModelDriver for LongFailure {
    fn invoke(
        &self,
        _: Arc<ModelRequest>,
        _: ModelConnection,
        _: CancellationToken,
    ) -> HostFuture<'_, ModelStream> {
        Box::pin(async {
            Err(HostError::failed(format!(
                "first line\n{}\nlast line",
                "x".repeat(1_000)
            )))
        })
    }
}

#[tokio::test]
async fn a_failure_without_facts_keeps_its_message_and_bounds_the_reason() {
    let agent = MinimalHost::start(&builder(Arc::new(LongFailure), Arc::new(MockTools)))
        .await
        .unwrap();
    let session = agent
        .create_session(SessionConfig::default())
        .await
        .unwrap();
    session
        .submit("long failure".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    session.idle("main").await.unwrap();
    let events = chain_events(&session, "main", ..).await.unwrap();
    let message = format!("first line\n{}\nlast line", "x".repeat(1_000));
    let ModelOutputOutcome::Failed {
        error,
        failure: Some(failure),
        ..
    } = events
        .iter()
        .find_map(|event| match &event.kind {
            EventKind::ModelOutputEnd(data) => Some(&data.outcome),
            _ => None,
        })
        .expect("a terminal model output")
    else {
        panic!("the terminal is not a failure");
    };
    assert_eq!(
        error, &message,
        "the host's message is passed through unchanged"
    );
    assert_eq!(failure.kind, ModelFailureKind::Unknown);
    let reason = failure.reason.as_deref().expect("a reason");
    assert!(!reason.contains('\n'), "{reason}");
    assert!(reason.chars().count() <= 300, "{reason}");
    assert!(reason.starts_with("first line "), "{reason}");
    agent.close().await.unwrap();
}

impl ModelDriver for RecordingModel {
    fn invoke(
        &self,
        request: Arc<ModelRequest>,
        _: dim_agent_capabilities::host::ModelConnection,
        cancel: CancellationToken,
    ) -> HostFuture<'_, ModelStream> {
        self.0.lock().unwrap().push(request.clone());
        EchoModel.invoke(
            request,
            dim_agent_capabilities::host::ModelConnection::Local,
            cancel,
        )
    }
}

struct BoundaryTools {
    entered: Arc<Notify>,
    release: Arc<Notify>,
}
impl ToolDriver for BoundaryTools {
    fn definitions(&self) -> Vec<ToolDefinition> {
        vec![definition(
            "boundary",
            "Hold the tool-result boundary",
            serde_json::json!({"type":"object"}),
        )]
    }
    fn execute(
        &self,
        _: ToolCall,
        _: ToolExecutionContext,
        cancel: CancellationToken,
    ) -> HostFuture<'_, ToolResult> {
        Box::pin(async move {
            self.entered.notify_one();
            tokio::select! {
                _ = cancel.cancelled() => Err(HostError::Cancelled("boundary cancelled".into())),
                _ = self.release.notified() => Ok(ToolResult::text("boundary completed")),
            }
        })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn steer_and_append_enter_the_current_turn_at_tool_result_while_queue_waits() {
    tokio::time::timeout(Duration::from_secs(5), async {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let agent = MinimalHost::start(&builder(Arc::new(RecordingModel(requests.clone())), Arc::new(BoundaryTools {
            entered: entered.clone(), release: release.clone(),
        }))).await.unwrap();
        let session = agent.create_session(SessionConfig { tools: Some(vec!["boundary".into()]), permission_policy: dim_agent_runtime::PermissionPolicy { preset: dim_agent_runtime::PermissionPreset::FullAccess, ..Default::default() }, ..Default::default() }).await.unwrap();
        session.submit("tool:boundary {}".into(), InputMode::Queue, "main").await.unwrap();
        entered.notified().await;
        let queued = session.submit("next turn".into(), InputMode::Queue, "main").await.unwrap();
        let steered = session.submit("change current request".into(), InputMode::Steer, "main").await.unwrap();
        let appended = session.submit("extra current context".into(), InputMode::Append, "main").await.unwrap();
        let mut dispatch = session.dispatch_watch("main").unwrap();
        loop {
            let events = chain_events(&session, "main", ..).await.unwrap();
            let applied = events.iter().filter(|event| matches!(&event.kind,
                EventKind::InputApplied(data) if [&steered.id, &appended.id].contains(&&data.input_id))).count();
            if applied == 2 && dispatch.borrow().sequence >= events.last().unwrap().sequence {
                break;
            }
            dispatch.changed().await.unwrap();
        }
        assert!(!chain_events(&session, "main", ..).await.unwrap().iter().any(|event| matches!(&event.kind,
            EventKind::InputApplied(data) if data.input_id == queued.id)));
        assert!(!chain_events(&session, "main", ..).await.unwrap().iter().any(|event| matches!(&event.kind,
            EventKind::InputConsumed(data) if [&queued.id, &steered.id, &appended.id].contains(&&data.input_id))));
        release.notify_one();
        session.idle("main").await.unwrap();
        let requests = requests.lock().unwrap().clone();
        assert_eq!(requests.len(), 3);
        assert!(requests[1].messages.contains(&Arc::new(Message::text(Role::User, "change current request"))));
        assert!(requests[1].messages.contains(&Arc::new(Message::text(Role::User, "extra current context"))));
        assert!(!requests[1].messages.contains(&Arc::new(Message::text(Role::User, "next turn"))));
        assert!(requests[2].messages.contains(&Arc::new(Message::text(Role::User, "next turn"))));
        let events = chain_events(&session, "main", ..).await.unwrap();
        let applied = |id: &str| events.iter().find(|event| matches!(&event.kind, EventKind::InputConsumed(data) if data.input_id == id)).unwrap().sequence;
        let tool_result = events.iter().find(|event| matches!(event.kind, EventKind::ToolResult(_))).unwrap().sequence;
        let turn_end = events.iter().find(|event| matches!(event.kind, EventKind::TurnEnd(_))).unwrap().sequence;
        assert!(tool_result < applied(&steered.id));
        assert!(applied(&steered.id) < applied(&appended.id));
        assert!(applied(&appended.id) < turn_end);
        assert!(turn_end < applied(&queued.id));
        assert_eq!(events.iter().filter(|event| matches!(event.kind, EventKind::TurnStart(_))).count(), 2);
        agent.close().await.unwrap();
    }).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn model_receives_the_committed_before_result_and_fixed_history() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let mut builder = builder(
            Arc::new(RecordingModel(requests.clone())),
            Arc::new(MockTools),
        );
        builder.require_feature(
            Feature::new("Rewrite").branch("Rewrite", |names| Ok(Box::new(Rewrite::new(names)?))),
        );
        let agent = MinimalHost::start(&builder).await.unwrap();
        let session = agent
            .create_session(SessionConfig::default())
            .await
            .unwrap();
        session
            .submit("first".into(), InputMode::Queue, "main")
            .await
            .unwrap();
        session.idle("main").await.unwrap();
        let frozen = chain_events(&session, "main", ..).await.unwrap();
        session
            .submit("second".into(), InputMode::Queue, "main")
            .await
            .unwrap();
        session.idle("main").await.unwrap();
        let requests = requests.lock().unwrap().clone();
        assert_eq!(requests[0].model_id, "final-model");
        assert_eq!(
            requests[0].messages,
            vec![Arc::new(Message::text(Role::User, "first"))]
        );
        assert_eq!(requests[1].messages.len(), 3);
        assert_eq!(
            frozen
                .iter()
                .filter(|event| matches!(event.kind, EventKind::InputMessage(_)))
                .count(),
            1
        );
        let call = frozen
            .iter()
            .find(|event| matches!(event.kind, EventKind::ModelCall(_)))
            .unwrap();
        assert!(
            matches!(&call.kind, EventKind::ModelCall(data) if data.request.model_id == "final-model")
        );
        agent.close().await.unwrap();
    })
    .await
    .unwrap();
}

struct DelayedTools {
    started: Arc<Notify>,
    cleaned: Arc<AtomicBool>,
}
impl ToolDriver for DelayedTools {
    fn definitions(&self) -> Vec<ToolDefinition> {
        vec![definition(
            "delayed",
            "Test native cleanup",
            serde_json::json!({"type":"object"}),
        )]
    }
    fn execute(
        &self,
        _: ToolCall,
        _: ToolExecutionContext,
        cancel: CancellationToken,
    ) -> HostFuture<'_, ToolResult> {
        Box::pin(async move {
            self.started.notify_one();
            cancel.cancelled().await;
            tokio::time::sleep(Duration::from_millis(25)).await;
            self.cleaned.store(true, Ordering::SeqCst);
            Ok(ToolResult::text("native operation succeeded"))
        })
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stop_waits_for_native_cleanup_and_keeps_a_real_success() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let started = Arc::new(Notify::new()); let cleaned = Arc::new(AtomicBool::new(false));
        let agent = MinimalHost::start(&builder(Arc::new(EchoModel), Arc::new(DelayedTools { started: started.clone(), cleaned: cleaned.clone() }))).await.unwrap();
        let session = agent.create_session(SessionConfig { tools: Some(vec!["delayed".into()]), permission_policy: dim_agent_runtime::PermissionPolicy { preset: dim_agent_runtime::PermissionPreset::FullAccess, ..Default::default() }, ..SessionConfig::default() }).await.unwrap();
        session.submit("tool:delayed {}".into(), InputMode::Queue, "main").await.unwrap();
        started.notified().await;
        session.stop("main", "stop".into()).await.unwrap();
        assert!(cleaned.load(Ordering::SeqCst));
        let events = chain_events(&session, "main", ..).await.unwrap();
        assert!(events.iter().any(|event| matches!(&event.kind, EventKind::ToolResult(data) if data.results[0].result.content == ToolContent::Text("native operation succeeded".into()) && data.results[0].result.is_error.is_none())));
        assert!(matches!(events.iter().rev().find(|event| matches!(event.kind, EventKind::TurnEnd(_))).unwrap().kind, EventKind::TurnEnd(TurnEndData { outcome: TurnOutcome::Cancelled { .. }, .. })));
        agent.close().await.unwrap();
    }).await.unwrap();
}

struct BrokenStream {
    cleaned: Arc<AtomicBool>,
}
impl ModelDriver for BrokenStream {
    fn invoke(
        &self,
        _: Arc<ModelRequest>,
        _: dim_agent_capabilities::host::ModelConnection,
        cancel: CancellationToken,
    ) -> HostFuture<'_, ModelStream> {
        let cleaned = self.cleaned.clone();
        Box::pin(async move {
            let stream = futures_util::stream::unfold(
                (0, cancel, cleaned),
                |(stage, cancel, cleaned)| async move {
                    match stage {
                        0 | 1 => Some((
                            Ok(ModelChunk::MessageStart {
                                message_id: "duplicate".into(),
                            }),
                            (stage + 1, cancel, cleaned),
                        )),
                        _ => {
                            cancel.cancelled().await;
                            tokio::time::sleep(Duration::from_millis(20)).await;
                            cleaned.store(true, Ordering::SeqCst);
                            None
                        }
                    }
                },
            );
            Ok(Box::pin(stream) as ModelStream)
        })
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invalid_model_stream_is_cancelled_and_drained_before_settlement() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let cleaned = Arc::new(AtomicBool::new(false));
        let agent = MinimalHost::start(&builder(
            Arc::new(BrokenStream {
                cleaned: cleaned.clone(),
            }),
            Arc::new(MockTools),
        ))
        .await
        .unwrap();
        let session = agent
            .create_session(SessionConfig::default())
            .await
            .unwrap();
        session
            .submit("hello".into(), InputMode::Queue, "main")
            .await
            .unwrap();
        session.idle("main").await.unwrap();
        assert!(cleaned.load(Ordering::SeqCst));
        assert!(matches!(
            chain_events(&session, "main", ..)
                .await
                .unwrap()
                .iter()
                .rev()
                .find(|event| matches!(event.kind, EventKind::TurnEnd(_)))
                .unwrap()
                .kind,
            EventKind::TurnEnd(TurnEndData {
                outcome: TurnOutcome::Failed { .. },
                ..
            })
        ));
        agent.close().await.unwrap();
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn mock_hosts_enforce_the_declared_output_byte_limit() {
    use dim_agent_capabilities::adapters::MAX_MOCK_OUTPUT_BYTES;
    use futures_util::StreamExt;
    let request = Arc::new(ModelRequest::new(
        "mock",
        "echo",
        vec![Message::text(
            Role::User,
            "x".repeat(MAX_MOCK_OUTPUT_BYTES - 6),
        )],
    ));
    let chunks = EchoModel
        .invoke(
            request,
            dim_agent_capabilities::host::ModelConnection::Local,
            CancellationToken::new(),
        )
        .await
        .unwrap()
        .collect::<Vec<_>>()
        .await;
    assert!(chunks.iter().any(
        |chunk| matches!(chunk, Ok(ModelChunk::Text(text)) if text.len() == MAX_MOCK_OUTPUT_BYTES)
    ));
    let request = Arc::new(ModelRequest::new(
        "mock",
        "echo",
        vec![Message::text(
            Role::User,
            "x".repeat(MAX_MOCK_OUTPUT_BYTES - 5),
        )],
    ));
    assert!(
        EchoModel
            .invoke(
                request,
                dim_agent_capabilities::host::ModelConnection::Local,
                CancellationToken::new()
            )
            .await
            .is_err()
    );
    for name in ["echo", "wait"] {
        for size in [MAX_MOCK_OUTPUT_BYTES, MAX_MOCK_OUTPUT_BYTES + 1] {
            let mut input = JsonObject::new();
            input.insert("text".into(), serde_json::Value::String("x".repeat(size)));
            if name == "wait" {
                input.insert("millis".into(), serde_json::Value::from(0));
            }
            let result = MockTools
                .execute(
                    ToolCall {
                        id: "limit".into(),
                        name: name.into(),
                        input,
                    },
                    ToolExecutionContext {
                        turn_id: "turn".into(),
                        request_id: "request".into(),
                        branch_id: "main".into(),
                    },
                    CancellationToken::new(),
                )
                .await;
            if size == MAX_MOCK_OUTPUT_BYTES {
                assert!(
                    matches!(result.unwrap().content, ToolContent::Text(text) if text.len() == size)
                );
            } else {
                assert!(
                    matches!(result, Err(HostError::Failed { message, .. }) if message == "Mock tool output exceeds 1 MiB")
                );
            }
        }
    }
}

#[derive(Clone)]
struct RecoveryProbe {
    mode: u8,
    calls: Arc<std::sync::atomic::AtomicUsize>,
    recovered: Arc<std::sync::atomic::AtomicUsize>,
    rejected: Arc<std::sync::atomic::AtomicUsize>,
    drained: Arc<AtomicBool>,
    /// Holds the second attempt at the driver so a test can observe the
    /// progress between attempts.
    resume: Option<Arc<Notify>>,
}
impl RecoveryProbe {
    fn connection(&self) -> ResolvedModelConnection {
        ResolvedModelConnection {
            connection: ModelConnection::Local,
            failure_policy: Some(Arc::new(self.clone())),
            recovery: (self.mode != 13)
                .then(|| Arc::new(self.clone()) as Arc<dyn RecoverModelConnection>),
        }
    }
    fn failure(&self, status: u16, retry_after: Option<Duration>) -> HostError {
        let kind = match status {
            401 | 403 => ModelFailureKind::AuthRejected,
            402 => ModelFailureKind::QuotaExhausted,
            404 => ModelFailureKind::NotFound,
            408 | 500..=599 => ModelFailureKind::ServerError,
            413 => ModelFailureKind::ContextTooLong,
            429 => ModelFailureKind::RateLimited,
            400..=499 => ModelFailureKind::InvalidRequest,
            _ => ModelFailureKind::Unknown,
        };
        self.failure_kind(kind, Some(status), retry_after)
    }
    fn failure_kind(
        &self,
        kind: ModelFailureKind,
        status: Option<u16>,
        retry_after: Option<Duration>,
    ) -> HostError {
        HostError::Failed {
            message: "fixture rejection".into(),
            budget: None,
            kind: match status {
                Some(status) => HostErrorKind::Http {
                    status: Some(status),
                    code: None,
                },
                None => HostErrorKind::Transport,
            },
            model: Some(Box::new(ModelFailureFacts {
                kind,
                reason: Some("fixture rejection".into()),
                status,
                provider_code: None,
                request_id: Some("fixture-request".into()),
                cause: Vec::new(),
                retry_after,
                attempts: 1,
                waited: Duration::ZERO,
                retry_after_stop: None,
                retry_forbidden: false,
            })),
            presentation: None,
        }
    }
}
impl ResolveModelConnection for RecoveryProbe {
    fn resolve(
        &self,
        _: Arc<ModelRequest>,
        _: CancellationToken,
    ) -> HostFuture<'_, ResolvedModelConnection> {
        Box::pin(async move { Ok(self.connection()) })
    }
}
impl RecoverModelConnection for RecoveryProbe {
    fn rejection(&self, error: &HostError, phase: CredentialRecoveryPhase) -> CredentialRejection {
        if self.mode == 20 {
            return CredentialRejection::Recoverable;
        }
        if !matches!(
            error,
            HostError::Failed {
                kind: HostErrorKind::Http {
                    status: Some(401),
                    ..
                },
                ..
            }
        ) {
            return CredentialRejection::Unrelated;
        }
        match phase {
            CredentialRecoveryPhase::Initial => CredentialRejection::Recoverable,
            CredentialRecoveryPhase::Recovered => CredentialRejection::Terminal,
        }
    }
    fn recover(&self, cancel: CancellationToken) -> HostFuture<'_, RecoveredModelConnection> {
        Box::pin(async move {
            self.recovered.fetch_add(1, Ordering::SeqCst);
            if self.mode == 1 {
                assert!(self.drained.load(Ordering::SeqCst));
            }
            if self.mode == 6 {
                cancel.cancel();
            }
            if self.mode == 33 {
                return Err(HostError::failed("fixture credential refresh failed"));
            }
            Ok(RecoveredModelConnection {
                connection: ModelConnection::Local,
                recovery: Arc::new(self.clone()),
            })
        })
    }
    fn finish_rejected(&self, _: CancellationToken) -> HostFuture<'_, ()> {
        Box::pin(async move {
            self.rejected.fetch_add(1, Ordering::SeqCst);
            if self.mode == 34 {
                return Err(HostError::failed("fixture credential settlement failed"));
            }
            Ok(())
        })
    }
}
impl ModelFailurePolicy for RecoveryProbe {
    fn forbids_retry(&self, error: &HostError) -> bool {
        self.mode == 13
            || matches!(error, HostError::Failed {
            kind: HostErrorKind::Http { code: Some(code), .. }, ..
        } if code == "fixture_veto")
    }
    fn presentation(&self, _: &HostError) -> Option<ProviderPresentation> {
        Some(ProviderPresentation {
            source: "fixture_provider".into(),
            reason: "unknown_error".into(),
            action: None,
            request_id: None,
            server_message: None,
        })
    }
}
impl ModelDriver for RecoveryProbe {
    fn invoke(
        &self,
        request: Arc<ModelRequest>,
        connection: ModelConnection,
        cancel: CancellationToken,
    ) -> HostFuture<'_, ModelStream> {
        Box::pin(async move {
            assert!(
                !cancel.is_cancelled(),
                "retry inherited failed attempt cancellation"
            );
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            if call == 1
                && let Some(resume) = &self.resume
            {
                resume.notified().await;
            }
            if call > 0 && !matches!(self.mode, 4 | 7 | 8 | 12 | 15..=20 | 23..=34) {
                return EchoModel.invoke(request, connection, cancel).await;
            }
            let special = match self.mode {
                24 if call < 7 => Some(self.failure_kind(ModelFailureKind::Network, None, None)),
                25 => Some(self.failure_kind(ModelFailureKind::ServerError, Some(503), None)),
                26 => Some(self.failure_kind(ModelFailureKind::MalformedResponse, None, None)),
                27 if call == 0 => Some(self.failure_kind(ModelFailureKind::Network, None, None)),
                27 if call < 3 => {
                    Some(self.failure_kind(ModelFailureKind::MalformedResponse, None, None))
                }
                28 => Some(self.failure_kind(
                    ModelFailureKind::ServerError,
                    Some(503),
                    Some(Duration::from_secs(45)),
                )),
                29 => Some(self.failure_kind(
                    ModelFailureKind::ServerError,
                    Some(503),
                    Some(Duration::from_secs(400)),
                )),
                30 => Some(self.failure_kind(
                    ModelFailureKind::ServerError,
                    Some(503),
                    Some(Duration::from_secs(100)),
                )),
                31 => Some(self.failure_kind(
                    ModelFailureKind::AuthRejected,
                    Some(401),
                    Some(Duration::from_secs(30)),
                )),
                32 if call == 0 => Some(self.failure_kind(
                    ModelFailureKind::ServerError,
                    Some(503),
                    Some(Duration::from_secs(5)),
                )),
                32 if call == 1 => {
                    Some(self.failure_kind(ModelFailureKind::ServerError, Some(503), None))
                }
                33 if call == 0 => {
                    Some(self.failure_kind(ModelFailureKind::ServerError, Some(503), None))
                }
                33 if call == 1 => {
                    Some(self.failure_kind(ModelFailureKind::AuthRejected, Some(401), None))
                }
                _ => None,
            };
            if let Some(error) = special {
                return Err(error);
            }
            if matches!(self.mode, 24 | 27) || (self.mode == 32 && call > 1) {
                return EchoModel.invoke(request, connection, cancel).await;
            }
            let status = match self.mode {
                20 => 403,
                16 if call < 2 => 503,
                19 if call == 0 => 503,
                17 | 23 if call > 0 => 503,
                5 | 7 | 12 | 13 | 22 => 503,
                8 if call > 0 => 429,
                9..=11 | 14 => 529,
                _ => 401,
            };
            let mut error = if self.mode >= 7 {
                self.failure(
                    status,
                    match self.mode {
                        12 | 14 | 17 | 22 => Some(Duration::from_secs(40)),
                        21 => Some(Duration::from_secs(61)),
                        23 if call > 0 => Some(Duration::from_secs(60)),
                        _ => None,
                    },
                )
            } else {
                HostError::classified(
                    HostErrorKind::Http {
                        status: Some(status),
                        code: None,
                    },
                    "fixture rejection",
                )
            };
            if self.mode == 15
                && call > 0
                && let HostError::Failed {
                    kind: HostErrorKind::Http { code, .. },
                    ..
                } = &mut error
            {
                *code = Some("fixture_veto".into());
            }
            if !matches!(self.mode, 1..=3 | 9..=11) && !(self.mode == 18 && call > 0) {
                return Err(error);
            }
            let mut chunks = vec![Ok(ModelChunk::MessageStart {
                message_id: "first".into(),
            })];
            if self.mode == 2 || self.mode == 18 {
                chunks.push(Ok(ModelChunk::Text("already published".into())));
            }
            if self.mode == 3 {
                chunks.push(Ok(ModelChunk::Usage(
                    serde_json::from_value(
                        serde_json::json!({"promptTokens":7,"completionTokens":2,"totalTokens":9}),
                    )
                    .unwrap(),
                )));
            }
            if self.mode == 10 {
                chunks.push(Ok(ModelChunk::Usage(
                    serde_json::from_value(
                        serde_json::json!({"promptTokens":7,"completionTokens":0,"totalTokens":7}),
                    )
                    .unwrap(),
                )));
            }
            if self.mode == 11 {
                chunks.extend([
                    Ok(ModelChunk::ToolStart {
                        id: "partial".into(),
                        name: "echo".into(),
                    }),
                    Ok(ModelChunk::ToolArguments {
                        id: "partial".into(),
                        name: "echo".into(),
                        delta: "{\"text\":".into(),
                    }),
                ]);
            }
            chunks.push(Err(error));
            let drained = self.drained.clone();
            let stream = futures_util::stream::unfold(
                (chunks.into_iter(), cancel, drained),
                |(mut chunks, cancel, drained)| async move {
                    if let Some(chunk) = chunks.next() {
                        return Some((chunk, (chunks, cancel, drained)));
                    }
                    cancel.cancelled().await;
                    drained.store(true, Ordering::SeqCst);
                    None
                },
            );
            Ok(Box::pin(stream) as ModelStream)
        })
    }
}
struct RecoveryHosts(RecoveryProbe);
impl Plugin for RecoveryHosts {
    type Deps = ();
    fn name(&self) -> &str {
        "test.recovery.hosts"
    }
    async fn apply(&self, ctx: &Ctx, (): Self::Deps) -> rutis::Result {
        ctx.provide(ModelHost(Arc::new(self.0.clone())))?;
        ctx.provide(ProviderConnectionResolver(Arc::new(self.0.clone())))?;
        Ok(())
    }
}

/// Attaches `chain` to the committed request of `kind`.
struct ChainHook {
    timeline: Key<Timeline>,
    kind: &'static str,
    chain: FailureChain,
}
impl ChainHook {
    fn new(names: &Names<'_>, kind: &'static str, chain: FailureChain) -> rutis::Result<Self> {
        Ok(Self {
            timeline: names.key()?,
            kind,
            chain,
        })
    }
}
impl DynPlugin for ChainHook {
    fn name(&self) -> &str {
        "test.failure-chain"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.timeline.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let timeline = ctx.require_at(&self.timeline)?;
            let chain = self.chain.clone();
            timeline.before(
                ctx,
                "attach-failure-chain",
                &[self.kind],
                move |mut draft| {
                    match &mut draft.kind {
                        EventKind::ContextPrepare(data) => data.failure_chain = Some(chain.clone()),
                        EventKind::ModelCall(call) => call.failure_chain = Some(chain.clone()),
                        _ => {}
                    }
                    Ok(draft)
                },
            )?;
            Ok(Effect::Done)
        })
    }
}

/// A Session whose only model host is a [`RecoveryProbe`] in `mode`.
async fn recovery_agent(
    mode: u8,
    resume: Option<Arc<Notify>>,
) -> (MinimalHost, dim_agent_runtime::Session, RecoveryProbe) {
    recovery_agent_with(mode, resume, None).await
}

/// A Session whose committed requests of the named kind carry a chain.
async fn recovery_agent_with(
    mode: u8,
    resume: Option<Arc<Notify>>,
    chain: Option<(&'static str, FailureChain)>,
) -> (MinimalHost, dim_agent_runtime::Session, RecoveryProbe) {
    let probe = RecoveryProbe {
        mode,
        calls: Default::default(),
        recovered: Default::default(),
        rejected: Default::default(),
        drained: Default::default(),
        resume,
    };
    let mut builder = AgentBuilder::new(AgentConfig::default());
    builder
        .session_value(Arc::new(ToolHost(Arc::new(MockTools))))
        .session_value(Arc::new(
            dim_agent_capabilities::history::HistoryRenderer::default(),
        ))
        .session_value(Arc::new(PrepareContextHost(Arc::new(IdentityContext))))
        .session_value(Arc::new(ContextModelHost(None)))
        .session_value(Arc::new(CompactionStrategyHost(None)))
        .require_feature(
            Feature::new("PrepareModelHost").session("PrepareModelHost", |names| {
                Ok(Box::new(DefaultPrepareModelPlugin::new(names)?))
            }),
        );
    defaults::install_with_process_model(&mut builder);
    let hosts = probe.clone();
    builder.plugin(move |_| Ok(Box::new(RecoveryHosts(hosts.clone()))));
    if let Some((kind, chain)) = chain {
        builder.require_feature(
            Feature::new("FailureChain").branch("FailureChain", move |names| {
                Ok(Box::new(ChainHook::new(names, kind, chain.clone())?))
            }),
        );
    }
    let agent = MinimalHost::start(&builder).await.unwrap();
    let session = agent
        .create_session(SessionConfig::default())
        .await
        .unwrap();
    (agent, session, probe)
}

/// The facts on the Session's failed terminal model output.
async fn model_failure(session: &dim_agent_runtime::Session) -> ModelFailure {
    let events = chain_events(session, "main", ..).await.unwrap();
    events
        .iter()
        .find_map(|event| match &event.kind {
            EventKind::ModelOutputEnd(ModelOutputEndData {
                outcome:
                    ModelOutputOutcome::Failed {
                        failure: Some(failure),
                        ..
                    },
                ..
            }) => Some(failure.as_ref().clone()),
            _ => None,
        })
        .expect("a failed model output with facts")
}

#[tokio::test(start_paused = true)]
async fn context_preparation_copies_a_failure_chain_into_the_call() {
    let chain = FailureChain {
        from_request_id: "request-before".into(),
        attempts: 2,
        waited_ms: 1_500,
    };
    let (agent, session, _) =
        recovery_agent_with(0, None, Some(("context/prepare", chain.clone()))).await;
    session
        .submit("continue".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    session.idle("main").await.unwrap();
    let events = chain_events(&session, "main", ..).await.unwrap();
    let prepare = events
        .iter()
        .find_map(|event| match &event.kind {
            EventKind::ContextPrepare(data) => Some(data),
            _ => None,
        })
        .expect("a committed preparation");
    assert_eq!(prepare.failure_chain.as_ref(), Some(&chain));
    let call = events
        .iter()
        .find_map(|event| match &event.kind {
            EventKind::ModelCall(data) => Some(data),
            _ => None,
        })
        .expect("a committed call");
    assert_eq!(call.failure_chain.as_ref(), Some(&chain));
    agent.close().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_request_without_a_failure_chain_gets_a_fresh_retry_budget() {
    let (agent, session, probe) = recovery_agent(25, None).await;
    session
        .submit("fresh budget".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    session.idle("main").await.unwrap();
    assert_eq!(probe.calls.load(Ordering::SeqCst), 8);
    let failure = model_failure(&session).await;
    assert_eq!(failure.kind, ModelFailureKind::ServerError);
    assert_eq!(failure.attempts, 8);
    agent.close().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_continuation_continues_its_failure_chain_retry_budget() {
    let chain = FailureChain {
        from_request_id: "request-before".into(),
        attempts: 5,
        waited_ms: 0,
    };
    let (agent, session, probe) = recovery_agent_with(25, None, Some(("model/call", chain))).await;
    session
        .submit("continue".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    session.idle("main").await.unwrap();
    assert_eq!(probe.calls.load(Ordering::SeqCst), 3);
    let failure = model_failure(&session).await;
    assert_eq!(failure.kind, ModelFailureKind::ServerError);
    assert_eq!(failure.attempts, 8);
    assert!((2_400..=3_000).contains(&failure.waited_ms), "{failure:?}");
    agent.close().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_spent_failure_chain_settles_without_a_call() {
    let chain = FailureChain {
        from_request_id: "request-before".into(),
        attempts: 8,
        waited_ms: 70_000,
    };
    let (agent, session, probe) = recovery_agent_with(25, None, Some(("model/call", chain))).await;
    session
        .submit("continue".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    session.idle("main").await.unwrap();
    assert_eq!(probe.calls.load(Ordering::SeqCst), 0);
    let events = chain_events(&session, "main", ..).await.unwrap();
    let (error, failure) = events
        .iter()
        .find_map(|event| match &event.kind {
            EventKind::ModelOutputEnd(ModelOutputEndData {
                outcome:
                    ModelOutputOutcome::Failed {
                        error,
                        failure: Some(failure),
                        ..
                    },
                ..
            }) => Some((error, failure)),
            _ => None,
        })
        .expect("a failed model output with facts");
    assert_eq!(error, "the failure chain's retry budget is spent");
    assert_eq!(failure.kind, ModelFailureKind::Unknown);
    assert_eq!(failure.attempts, 8);
    assert_eq!(failure.waited_ms, 70_000);
    agent.close().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_continuation_waits_no_longer_than_the_chain_budget_left() {
    let chain = FailureChain {
        from_request_id: "request-before".into(),
        attempts: 5,
        waited_ms: 299_000,
    };
    let (agent, session, probe) = recovery_agent_with(25, None, Some(("model/call", chain))).await;
    let started = tokio::time::Instant::now();
    session
        .submit("continue".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    session.idle("main").await.unwrap();
    assert_eq!(probe.calls.load(Ordering::SeqCst), 2);
    let slept = started.elapsed();
    assert!(
        slept >= Duration::from_millis(800) && slept <= Duration::from_secs(1),
        "{slept:?}"
    );
    let failure = model_failure(&session).await;
    assert_eq!(failure.attempts, 7);
    assert!(
        (299_800..=300_000).contains(&failure.waited_ms),
        "{failure:?}"
    );
    agent.close().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_failed_credential_refresh_keeps_the_chain_totals() {
    let chain = FailureChain {
        from_request_id: "request-before".into(),
        attempts: 5,
        waited_ms: 1_500,
    };
    let (agent, session, probe) = recovery_agent_with(33, None, Some(("model/call", chain))).await;
    let started = tokio::time::Instant::now();
    session
        .submit("continue".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    session.idle("main").await.unwrap();
    assert_eq!(probe.calls.load(Ordering::SeqCst), 2);
    assert_eq!(probe.recovered.load(Ordering::SeqCst), 1);
    let slept = started.elapsed();
    assert!(
        slept >= Duration::from_millis(800) && slept <= Duration::from_secs(1),
        "{slept:?}"
    );
    let events = chain_events(&session, "main", ..).await.unwrap();
    let (error, failure) = events
        .iter()
        .find_map(|event| match &event.kind {
            EventKind::ModelOutputEnd(ModelOutputEndData {
                outcome:
                    ModelOutputOutcome::Failed {
                        error,
                        failure: Some(failure),
                        ..
                    },
                ..
            }) => Some((error, failure)),
            _ => None,
        })
        .expect("a failed model output with facts");
    assert_eq!(error, "fixture credential refresh failed");
    assert_eq!(failure.kind, ModelFailureKind::Unknown);
    assert_eq!(failure.attempts, 7);
    assert!((2_300..=2_500).contains(&failure.waited_ms), "{failure:?}");
    agent.close().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_failed_credential_settlement_keeps_the_chain_totals() {
    let chain = FailureChain {
        from_request_id: "request-before".into(),
        attempts: 5,
        waited_ms: 1_500,
    };
    let (agent, session, probe) = recovery_agent_with(34, None, Some(("model/call", chain))).await;
    session
        .submit("continue".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    session.idle("main").await.unwrap();
    assert_eq!(probe.calls.load(Ordering::SeqCst), 2);
    assert_eq!(probe.recovered.load(Ordering::SeqCst), 1);
    assert_eq!(probe.rejected.load(Ordering::SeqCst), 1);
    let events = chain_events(&session, "main", ..).await.unwrap();
    let (error, failure) = events
        .iter()
        .find_map(|event| match &event.kind {
            EventKind::ModelOutputEnd(ModelOutputEndData {
                outcome:
                    ModelOutputOutcome::Failed {
                        error,
                        failure: Some(failure),
                        ..
                    },
                ..
            }) => Some((error, failure)),
            _ => None,
        })
        .expect("a failed model output with facts");
    assert_eq!(error, "fixture credential settlement failed");
    assert_eq!(failure.kind, ModelFailureKind::Unknown);
    assert_eq!(failure.attempts, 7);
    assert_eq!(failure.waited_ms, 1_500);
    agent.close().await.unwrap();
}

#[tokio::test]
async fn process_model_recovery_respects_output_cleanup_and_cancellation() {
    tokio::time::timeout(Duration::from_secs(15), async {
        for mode in 0..=6 {
            let probe = RecoveryProbe {
                mode,
                calls: Default::default(),
                recovered: Default::default(),
                rejected: Default::default(),
                drained: Default::default(),
                resume: None,
            };
            let mut builder = AgentBuilder::new(AgentConfig::default());
            builder
                .session_value(Arc::new(ToolHost(Arc::new(MockTools))))
                .session_value(Arc::new(
                    dim_agent_capabilities::history::HistoryRenderer::default(),
                ))
                .session_value(Arc::new(PrepareContextHost(Arc::new(IdentityContext))))
                .session_value(Arc::new(ContextModelHost(None)))
                .session_value(Arc::new(CompactionStrategyHost(None)))
                .require_feature(
                    Feature::new("PrepareModelHost").session("PrepareModelHost", |names| {
                        Ok(Box::new(DefaultPrepareModelPlugin::new(names)?))
                    }),
                );
            defaults::install_with_process_model(&mut builder);
            let hosts = probe.clone();
            builder.plugin(move |_| Ok(Box::new(RecoveryHosts(hosts.clone()))));
            let agent = MinimalHost::start(&builder).await.unwrap();
            let session = agent
                .create_session(SessionConfig::default())
                .await
                .unwrap();
            session
                .submit("hello".into(), InputMode::Queue, "main")
                .await
                .unwrap();
            session.idle("main").await.unwrap();
            let events = chain_events(&session, "main", ..).await.unwrap();
            let ends: Vec<_> = events
                .iter()
                .filter_map(|event| match &event.kind {
                    EventKind::ModelOutputEnd(end) => Some(end),
                    _ => None,
                })
                .collect();
            assert_eq!(ends.len(), 1, "mode {mode}");
            assert_eq!(
                matches!(ends[0].outcome, ModelOutputOutcome::Completed { .. }),
                mode <= 1,
                "mode {mode}"
            );
            assert_eq!(
                probe.calls.load(Ordering::SeqCst),
                if matches!(mode, 0 | 1 | 4) { 2 } else { 1 },
                "mode {mode}"
            );
            assert_eq!(
                probe.recovered.load(Ordering::SeqCst),
                usize::from(matches!(mode, 0 | 1 | 4 | 6)),
                "mode {mode}"
            );
            assert_eq!(
                probe.rejected.load(Ordering::SeqCst),
                usize::from(mode == 4),
                "mode {mode}"
            );
            if mode == 2 {
                let text: Vec<_> = events
                    .iter()
                    .filter_map(|event| match &event.kind {
                        EventKind::Text(data) => Some(data.delta.as_str()),
                        _ => None,
                    })
                    .collect();
                assert_eq!(text, ["already published"]);
            }
            if matches!(mode, 1..=3) {
                assert!(probe.drained.load(Ordering::SeqCst));
            }
            agent.close().await.unwrap();
        }
    })
    .await
    .unwrap();
}

#[tokio::test(start_paused = true)]
async fn transient_retries_are_bounded_and_stop_after_irreversible_output() {
    tokio::time::timeout(Duration::from_secs(600), async {
        // `continues`: mode 11 announces a call before it fails, so its tool
        // start closes replay, the retry budget stops, and the turn takes its
        // one continuation. `expected_calls` counts the failed request's own
        // calls in every row.
        for (mode, expected_calls, expected_refresh, expected_rejected, completed, continues) in [
            (7, 8, 0, 0, false, false),
            (8, 8, 1, 0, false, false),
            (9, 2, 0, 0, true, false),
            (10, 1, 0, 0, false, false),
            (11, 1, 0, 0, false, true),
            (13, 1, 0, 0, false, false),
            (15, 2, 1, 1, false, false),
            (16, 4, 1, 1, false, false),
            (18, 2, 1, 1, false, false),
            (19, 3, 1, 1, false, false),
            (20, 2, 1, 0, false, false),
        ] {
            let probe = RecoveryProbe {
                mode,
                calls: Default::default(),
                recovered: Default::default(),
                rejected: Default::default(),
                drained: Default::default(),
                resume: None,
            };
            let mut builder = AgentBuilder::new(AgentConfig::default());
            builder
                .session_value(Arc::new(ToolHost(Arc::new(MockTools))))
                .session_value(Arc::new(
                    dim_agent_capabilities::history::HistoryRenderer::default(),
                ))
                .session_value(Arc::new(PrepareContextHost(Arc::new(IdentityContext))))
                .session_value(Arc::new(ContextModelHost(None)))
                .session_value(Arc::new(CompactionStrategyHost(None)))
                .require_feature(
                    Feature::new("PrepareModelHost").session("PrepareModelHost", |names| {
                        Ok(Box::new(DefaultPrepareModelPlugin::new(names)?))
                    }),
                );
            defaults::install_with_process_model(&mut builder);
            let hosts = probe.clone();
            builder.plugin(move |_| Ok(Box::new(RecoveryHosts(hosts.clone()))));
            let agent = MinimalHost::start(&builder).await.unwrap();
            let session = agent
                .create_session(SessionConfig::default())
                .await
                .unwrap();
            session
                .submit("retry contract".into(), InputMode::Queue, "main")
                .await
                .unwrap();
            session.idle("main").await.unwrap();

            assert_eq!(
                probe.calls.load(Ordering::SeqCst),
                expected_calls + usize::from(continues),
                "mode {mode}: the failed request's calls plus its continuation"
            );
            assert_eq!(
                probe.recovered.load(Ordering::SeqCst),
                expected_refresh,
                "mode {mode}"
            );
            assert_eq!(
                probe.rejected.load(Ordering::SeqCst),
                expected_rejected,
                "mode {mode}"
            );
            let events = chain_events(&session, "main", ..).await.unwrap();
            let ends: Vec<_> = events
                .iter()
                .filter_map(|event| match &event.kind {
                    EventKind::ModelOutputEnd(end) => Some(&end.outcome),
                    _ => None,
                })
                .collect();
            assert_eq!(ends.len(), 1 + usize::from(continues), "mode {mode}");
            if !completed {
                let ModelOutputOutcome::Failed {
                    error,
                    failure: Some(failure),
                    ..
                } = ends[0]
                else {
                    panic!("mode {mode}: {ends:?}");
                };
                let expected_kind = match mode {
                    7 | 10 | 11 | 13 => ModelFailureKind::ServerError,
                    8 => ModelFailureKind::RateLimited,
                    _ => ModelFailureKind::AuthRejected,
                };
                assert_eq!(failure.kind, expected_kind, "mode {mode}");
                assert_eq!(
                    failure.retry_forbidden,
                    matches!(mode, 13 | 15),
                    "mode {mode}"
                );
                assert_eq!(usize::from(failure.attempts), expected_calls, "mode {mode}");
                if matches!(mode, 7 | 8 | 16 | 19) {
                    assert!(failure.waited_ms > 0, "mode {mode}");
                } else {
                    assert_eq!(failure.waited_ms, 0, "mode {mode}");
                }
                assert_eq!(error.as_str(), "fixture rejection", "mode {mode}");
                if !continues {
                    let turn_error = events.iter().find_map(|event| match &event.kind {
                        EventKind::TurnEnd(TurnEndData {
                            outcome: TurnOutcome::Failed { error },
                            ..
                        }) => Some(error.as_str()),
                        _ => None,
                    });
                    assert_eq!(turn_error, Some(error.as_str()), "mode {mode}");
                }
                assert!(
                    matches!(ends[0], ModelOutputOutcome::Failed {
                    presentation: Some(value), ..
                } if value.reason == "unknown_error"),
                    "mode {mode}"
                );
            }
            assert_eq!(
                matches!(ends[0], ModelOutputOutcome::Completed { .. }),
                completed,
                "mode {mode}"
            );
            agent.close().await.unwrap();
        }
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn waiting_to_retry_obeys_stop_steer_and_provider_reload() {
    tokio::time::timeout(Duration::from_secs(15), async {
        for (mode, action) in [14, 22]
            .into_iter()
            .flat_map(|mode| ["stop", "steer", "reload"].map(|action| (mode, action)))
        {
            let probe = RecoveryProbe {
                mode,
                calls: Default::default(),
                recovered: Default::default(),
                rejected: Default::default(),
                drained: Default::default(),
                resume: None,
            };
            let mut builder = AgentBuilder::new(AgentConfig::default());
            builder
                .session_value(Arc::new(ToolHost(Arc::new(MockTools))))
                .session_value(Arc::new(
                    dim_agent_capabilities::history::HistoryRenderer::default(),
                ))
                .session_value(Arc::new(PrepareContextHost(Arc::new(IdentityContext))))
                .session_value(Arc::new(ContextModelHost(None)))
                .session_value(Arc::new(CompactionStrategyHost(None)))
                .require_feature(
                    Feature::new("PrepareModelHost").session("PrepareModelHost", |names| {
                        Ok(Box::new(DefaultPrepareModelPlugin::new(names)?))
                    }),
                );
            defaults::install_with_process_model(&mut builder);
            let hosts = probe.clone();
            builder.plugin(move |_| Ok(Box::new(RecoveryHosts(hosts.clone()))));
            let agent = MinimalHost::start(&builder).await.unwrap();
            let session = agent
                .create_session(SessionConfig::default())
                .await
                .unwrap();
            session
                .submit("wait before retry".into(), InputMode::Queue, "main")
                .await
                .unwrap();
            while probe.calls.load(Ordering::SeqCst) == 0 {
                tokio::task::yield_now().await;
            }
            tokio::task::yield_now().await;

            let mut steered = None;
            match action {
                "stop" => {
                    session.stop("main", "user stopped".into()).await.unwrap();
                }
                "steer" => {
                    steered = Some(
                        session
                            .submit("replacement".into(), InputMode::Steer, "main")
                            .await
                            .unwrap()
                            .id
                            .clone(),
                    );
                }
                "reload" => agent.loader().restart("test.recovery.hosts").await.unwrap(),
                _ => unreachable!(),
            }
            session.idle("main").await.unwrap();
            assert_eq!(probe.recovered.load(Ordering::SeqCst), 0);
            assert_eq!(probe.rejected.load(Ordering::SeqCst), 0);
            let events = chain_events(&session, "main", ..).await.unwrap();
            match action {
                "stop" => {
                    assert_eq!(probe.calls.load(Ordering::SeqCst), 1);
                    let outcomes: Vec<_> = events
                        .iter()
                        .filter_map(|event| match &event.kind {
                            EventKind::ModelOutputEnd(data) => Some(&data.outcome),
                            _ => None,
                        })
                        .collect();
                    assert!(
                        matches!(
                            outcomes.as_slice(),
                            [ModelOutputOutcome::Cancelled { reason }] if reason == "user stopped"
                        ),
                        "{outcomes:?}"
                    );
                    assert!(matches!(
                        &events.iter().rev().find(|event| matches!(event.kind, EventKind::TurnEnd(_))).unwrap().kind,
                        EventKind::TurnEnd(TurnEndData {
                            outcome: TurnOutcome::Cancelled { reason },
                            ..
                        }) if reason == "user stopped"
                    ));
                }
                "steer" => {
                    assert_eq!(probe.calls.load(Ordering::SeqCst), 2);
                    assert!(events.iter().any(|event| matches!(
                        &event.kind,
                        EventKind::InputConsumed(data) if Some(&data.input_id) == steered.as_ref()
                    )));
                    assert!(matches!(
                        &events.iter().rev().find(|event| matches!(event.kind, EventKind::TurnEnd(_))).unwrap().kind,
                        EventKind::TurnEnd(TurnEndData {
                            outcome: TurnOutcome::Completed { .. },
                            ..
                        })
                    ));
                }
                "reload" => {
                    assert_eq!(probe.calls.load(Ordering::SeqCst), 1);
                    assert!(events.iter().any(|event| matches!(
                        &event.kind,
                        EventKind::ModelOutputEnd(ModelOutputEndData {
                            outcome: ModelOutputOutcome::Cancelled { reason },
                            ..
                        }) if reason == CAPABILITY_RELOADING
                    )));
                }
                _ => unreachable!(),
            }
            agent.close().await.unwrap();
        }
    })
    .await
    .unwrap();
}

struct CompletedThenDrained {
    draining: Arc<Notify>,
}
impl ModelDriver for CompletedThenDrained {
    fn invoke(
        &self,
        _: Arc<ModelRequest>,
        _: ModelConnection,
        cancel: CancellationToken,
    ) -> HostFuture<'_, ModelStream> {
        let draining = self.draining.clone();
        Box::pin(async move {
            let chunks = vec![
                ModelChunk::MessageStart {
                    message_id: "complete-before-stop".into(),
                },
                ModelChunk::Text("finished".into()),
                ModelChunk::TextEnd,
                ModelChunk::MessageStop {
                    message_id: "complete-before-stop".into(),
                    reason: ModelStopReason::EndTurn,
                },
            ];
            let stream = futures_util::stream::unfold(
                (chunks.into_iter(), cancel, draining),
                |(mut chunks, cancel, draining)| async move {
                    if let Some(chunk) = chunks.next() {
                        return Some((Ok(chunk), (chunks, cancel, draining)));
                    }
                    draining.notify_one();
                    cancel.cancelled().await;
                    None
                },
            );
            Ok(Box::pin(stream) as ModelStream)
        })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stop_during_native_drain_keeps_an_already_completed_model_result() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let draining = Arc::new(Notify::new());
        let agent = MinimalHost::start(&builder(
            Arc::new(CompletedThenDrained {
                draining: draining.clone(),
            }),
            Arc::new(MockTools),
        ))
        .await
        .unwrap();
        let session = agent
            .create_session(SessionConfig::default())
            .await
            .unwrap();
        session
            .submit("finish before stop".into(), InputMode::Queue, "main")
            .await
            .unwrap();
        draining.notified().await;
        session
            .stop("main", "stop during drain".into())
            .await
            .unwrap();
        let events = chain_events(&session, "main", ..).await.unwrap();
        assert!(events.iter().any(|event| matches!(
            &event.kind,
            EventKind::ModelOutputEnd(ModelOutputEndData {
                outcome: ModelOutputOutcome::Completed { .. },
                ..
            })
        )));
        assert!(events.iter().any(|event| matches!(
            &event.kind,
            EventKind::Text(data) if data.delta == "finished"
        )));
        agent.close().await.unwrap();
    })
    .await
    .unwrap();
}

#[tokio::test(start_paused = true)]
async fn retry_after_sets_the_delay_and_the_budget() {
    // 45 s is the delay as given, above the 30 s backoff cap.
    let (agent, session, probe) = recovery_agent(28, None).await;
    session
        .submit("retry after".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    while probe.calls.load(Ordering::SeqCst) == 0 {
        tokio::task::yield_now().await;
    }
    tokio::time::advance(Duration::from_secs(44)).await;
    tokio::task::yield_now().await;
    assert_eq!(probe.calls.load(Ordering::SeqCst), 1);
    tokio::time::advance(Duration::from_secs(1)).await;
    tokio::task::yield_now().await;
    assert_eq!(probe.calls.load(Ordering::SeqCst), 2);
    session.stop("main", "done".into()).await.unwrap();
    session.idle("main").await.unwrap();
    agent.close().await.unwrap();

    // 400 s does not fit the 300 s budget: the first attempt stops at once.
    let (agent, session, probe) = recovery_agent(29, None).await;
    session
        .submit("retry after".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    session.idle("main").await.unwrap();
    assert_eq!(probe.calls.load(Ordering::SeqCst), 1);
    let failure = model_failure(&session).await;
    assert_eq!(failure.attempts, 1);
    assert_eq!(failure.waited_ms, 0);
    assert_eq!(failure.retry_after_ms, Some(400_000));
    agent.close().await.unwrap();

    // Three 100 s waits fit in the budget and end exactly at 300 s; the
    // fourth does not fit and stops.
    let (agent, session, probe) = recovery_agent(30, None).await;
    let started = tokio::time::Instant::now();
    session
        .submit("retry after".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    session.idle("main").await.unwrap();
    assert!(started.elapsed() >= Duration::from_secs(300));
    assert_eq!(probe.calls.load(Ordering::SeqCst), 4);
    let failure = model_failure(&session).await;
    assert_eq!(failure.attempts, 4);
    assert_eq!(failure.waited_ms, 300_000);
    assert_eq!(failure.retry_after_ms, Some(100_000));
    agent.close().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_rejected_credential_recovers_without_waiting() {
    // The rejection carries `Retry-After: 30`; recovery still runs at once,
    // and the terminal rejection after it leaves the wait at zero.
    let (agent, session, probe) = recovery_agent(31, None).await;
    let started = tokio::time::Instant::now();
    session
        .submit("credential recovery".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    session.idle("main").await.unwrap();
    assert_eq!(probe.calls.load(Ordering::SeqCst), 2);
    assert_eq!(probe.recovered.load(Ordering::SeqCst), 1);
    assert_eq!(probe.rejected.load(Ordering::SeqCst), 1);
    assert_eq!(started.elapsed(), Duration::ZERO);
    let failure = model_failure(&session).await;
    assert_eq!(failure.kind, ModelFailureKind::AuthRejected);
    assert_eq!(failure.attempts, 2);
    assert_eq!(failure.waited_ms, 0);
    agent.close().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn the_backoff_counts_every_retry() {
    // A hinted first retry (5 s) is retry one; the unhinted second one is
    // retry two and waits 2 s × jitter, not 1 s × jitter.
    let (agent, session, probe) = recovery_agent(32, None).await;
    session
        .submit("mixed hints".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    while probe.calls.load(Ordering::SeqCst) == 0 {
        tokio::task::yield_now().await;
    }
    tokio::time::advance(Duration::from_secs(4)).await;
    tokio::task::yield_now().await;
    assert_eq!(probe.calls.load(Ordering::SeqCst), 1);
    tokio::time::advance(Duration::from_secs(1)).await;
    tokio::task::yield_now().await;
    assert_eq!(probe.calls.load(Ordering::SeqCst), 2);
    tokio::time::advance(Duration::from_millis(1_599)).await;
    tokio::task::yield_now().await;
    assert_eq!(probe.calls.load(Ordering::SeqCst), 2);
    tokio::time::advance(Duration::from_millis(401)).await;
    tokio::task::yield_now().await;
    assert_eq!(probe.calls.load(Ordering::SeqCst), 3);
    session.idle("main").await.unwrap();
    agent.close().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn transient_failures_retry_for_about_ninety_seconds() {
    // Seven network failures, then a successful attempt: the wait is the
    // nominal 91 s of backoff with the 0.8-1.0 jitter.
    let (agent, session, probe) = recovery_agent(24, None).await;
    let started = tokio::time::Instant::now();
    session
        .submit("network outage".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    session.idle("main").await.unwrap();
    assert_eq!(probe.calls.load(Ordering::SeqCst), 8);
    assert!(started.elapsed() >= Duration::from_millis(72_800));
    assert!(started.elapsed() <= Duration::from_secs(91));
    let events = chain_events(&session, "main", ..).await.unwrap();
    assert!(events.iter().any(|event| matches!(
        &event.kind,
        EventKind::ModelOutputEnd(ModelOutputEndData {
            outcome: ModelOutputOutcome::Completed { .. },
            ..
        })
    )));
    agent.close().await.unwrap();

    // A server error every time stops at the eighth attempt.
    let (agent, session, probe) = recovery_agent(25, None).await;
    session
        .submit("server outage".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    session.idle("main").await.unwrap();
    assert_eq!(probe.calls.load(Ordering::SeqCst), 8);
    let failure = model_failure(&session).await;
    assert_eq!(failure.kind, ModelFailureKind::ServerError);
    assert_eq!(failure.attempts, 8);
    assert!(
        (72_800..=91_000).contains(&failure.waited_ms),
        "{failure:?}"
    );
    agent.close().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn malformed_responses_are_retried_once() {
    let (agent, session, probe) = recovery_agent(26, None).await;
    session
        .submit("malformed".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    session.idle("main").await.unwrap();
    assert_eq!(probe.calls.load(Ordering::SeqCst), 2);
    let failure = model_failure(&session).await;
    assert_eq!(failure.kind, ModelFailureKind::MalformedResponse);
    assert_eq!(failure.attempts, 2);
    agent.close().await.unwrap();

    // A network failure does not make a second malformed response retryable.
    let (agent, session, probe) = recovery_agent(27, None).await;
    session
        .submit("mixed".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    session.idle("main").await.unwrap();
    assert_eq!(probe.calls.load(Ordering::SeqCst), 3);
    let failure = model_failure(&session).await;
    assert_eq!(failure.kind, ModelFailureKind::MalformedResponse);
    assert_eq!(failure.attempts, 3);
    agent.close().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_running_retry_is_progress_and_clears_when_the_attempt_starts() {
    let resume = Arc::new(Notify::new());
    let (agent, session, probe) = recovery_agent(25, Some(resume.clone())).await;
    session
        .submit("retry progress".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    let mut progress = session.on::<ModelProgress>("main").unwrap().unwrap();
    progress
        .wait_for(|progress| progress.retry.is_some())
        .await
        .unwrap();
    let retry = session
        .get::<ModelProgress>("main")
        .unwrap()
        .unwrap()
        .retry
        .expect("a retry while sleeping");
    assert_eq!((retry.attempt, retry.max_attempts), (2, 8));
    assert_eq!(retry.kind, ModelFailureKind::ServerError);
    assert_eq!(retry.reason.as_deref(), Some("fixture rejection"));
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    assert!(retry.retry_at_ms > now, "{retry:?} {now}");
    tokio::time::advance(Duration::from_secs(1)).await;
    progress
        .wait_for(|progress| progress.retry.is_none())
        .await
        .unwrap();
    assert_eq!(probe.calls.load(Ordering::SeqCst), 2);
    resume.notify_one();
    session.stop("main", "done".into()).await.unwrap();
    session.idle("main").await.unwrap();
    assert!(
        session
            .get::<ModelProgress>("main")
            .unwrap()
            .unwrap()
            .retry
            .is_none()
    );
    agent.close().await.unwrap();

    // A cancelled wait leaves no retry behind.
    let (agent, session, _) = recovery_agent(25, None).await;
    session
        .submit("cancelled retry".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    let mut progress = session.on::<ModelProgress>("main").unwrap().unwrap();
    progress
        .wait_for(|progress| progress.retry.is_some())
        .await
        .unwrap();
    session.stop("main", "user stopped".into()).await.unwrap();
    session.idle("main").await.unwrap();
    assert!(
        session
            .get::<ModelProgress>("main")
            .unwrap()
            .unwrap()
            .retry
            .is_none()
    );
    agent.close().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn an_interrupt_cancels_a_thirty_second_sleep_promptly() {
    let (agent, session, probe) = recovery_agent(25, None).await;
    session
        .submit("slow retry".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    // Five waits of 1, 2, 4, 8 and 16 s, jittered, end within 31 s; the
    // sixth attempt then sleeps for the 30 s cap.
    while probe.calls.load(Ordering::SeqCst) < 6 {
        tokio::time::advance(Duration::from_secs(1)).await;
        tokio::task::yield_now().await;
    }
    let stopped = tokio::time::Instant::now();
    session.stop("main", "user stopped".into()).await.unwrap();
    session.idle("main").await.unwrap();
    assert!(stopped.elapsed() < Duration::from_secs(1));
    let events = chain_events(&session, "main", ..).await.unwrap();
    assert!(events.iter().any(|event| matches!(
        &event.kind,
        EventKind::ModelOutputEnd(ModelOutputEndData {
            outcome: ModelOutputOutcome::Cancelled { .. },
            ..
        })
    )));
    agent.close().await.unwrap();
}

/// An append input written while idle starts no turn; the next driving
/// input's turn consumes it with that input.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_idle_append_starts_no_turn_and_joins_the_next_one() {
    tokio::time::timeout(Duration::from_secs(5), async {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let agent = MinimalHost::start(&builder(Arc::new(RecordingModel(requests.clone())), Arc::new(MockTools))).await.unwrap();
        let session = agent.create_session(SessionConfig::default()).await.unwrap();
        let appended = session.submit("idle context".into(), InputMode::Append, "main").await.unwrap();
        session.idle("main").await.unwrap();
        let events = chain_events(&session, "main", ..).await.unwrap();
        assert!(events.iter().any(|event| matches!(&event.kind,
            EventKind::InputApplied(data) if data.input_id == appended.id && data.mode == Some(InputMode::Append))));
        assert!(!events.iter().any(|event| matches!(event.kind, EventKind::TurnStart(_))));
        assert_eq!(
            session.get::<InputSnapshot>("main").unwrap().unwrap().queued,
            1,
            "the appended input waits for a driving turn"
        );
        session.submit("go".into(), InputMode::Queue, "main").await.unwrap();
        session.idle("main").await.unwrap();
        let requests = requests.lock().unwrap().clone();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].messages.contains(&Arc::new(Message::text(Role::User, "idle context"))));
        assert!(requests[0].messages.contains(&Arc::new(Message::text(Role::User, "go"))));
        let events = chain_events(&session, "main", ..).await.unwrap();
        assert_eq!(events.iter().filter(|event| matches!(event.kind, EventKind::TurnStart(_))).count(), 1);
        agent.close().await.unwrap();
    }).await.unwrap();
}

/// Streams one text chunk, then holds the end of its first output until
/// released; later calls echo.
struct HeldOutput {
    requests: Arc<Mutex<Vec<Arc<ModelRequest>>>>,
    release: Arc<Notify>,
}
impl ModelDriver for HeldOutput {
    fn invoke(
        &self,
        request: Arc<ModelRequest>,
        connection: dim_agent_capabilities::host::ModelConnection,
        cancel: CancellationToken,
    ) -> HostFuture<'_, ModelStream> {
        let first = {
            let mut requests = self.requests.lock().unwrap();
            requests.push(request.clone());
            requests.len() == 1
        };
        if !first {
            return EchoModel.invoke(request, connection, cancel);
        }
        let release = self.release.clone();
        Box::pin(async move {
            use futures_util::StreamExt;
            let head = futures_util::stream::iter(vec![
                Ok(ModelChunk::MessageStart {
                    message_id: "held".into(),
                }),
                Ok(ModelChunk::Text("partial answer".into())),
                Ok(ModelChunk::TextEnd),
            ]);
            let tail = futures_util::stream::once(async move {
                release.notified().await;
                Ok(ModelChunk::MessageStop {
                    message_id: "held".into(),
                    reason: ModelStopReason::EndTurn,
                })
            });
            Ok(Box::pin(head.chain(tail)) as ModelStream)
        })
    }
}

/// A steer applied while the model is outputting waits for the output to
/// end; an output without tool calls then does not end the turn: the same
/// turn calls the model again with the steer.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_steer_during_output_continues_the_turn_instead_of_ending_it() {
    tokio::time::timeout(Duration::from_secs(5), async {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let release = Arc::new(Notify::new());
        let agent = MinimalHost::start(&builder(
            Arc::new(HeldOutput { requests: requests.clone(), release: release.clone() }),
            Arc::new(MockTools),
        )).await.unwrap();
        let session = agent.create_session(SessionConfig::default()).await.unwrap();
        session.submit("first".into(), InputMode::Queue, "main").await.unwrap();
        let mut dispatch = session.dispatch_watch("main").unwrap();
        loop {
            let events = chain_events(&session, "main", ..).await.unwrap();
            if events.iter().any(|event| matches!(event.kind, EventKind::Text(_)))
                && dispatch.borrow().sequence >= events.last().unwrap().sequence
            {
                break;
            }
            dispatch.changed().await.unwrap();
        }
        let steer = session.submit("STEER-MARKER".into(), InputMode::Steer, "main").await.unwrap();
        loop {
            let events = chain_events(&session, "main", ..).await.unwrap();
            if events.iter().any(|event| matches!(&event.kind, EventKind::InputApplied(data) if data.input_id == steer.id))
                && dispatch.borrow().sequence >= events.last().unwrap().sequence
            {
                break;
            }
            dispatch.changed().await.unwrap();
        }
        release.notify_one();
        session.idle("main").await.unwrap();
        let requests = requests.lock().unwrap().clone();
        assert_eq!(requests.len(), 2);
        assert!(requests[1].messages.contains(&Arc::new(Message::text(Role::User, "STEER-MARKER"))));
        let events = chain_events(&session, "main", ..).await.unwrap();
        assert_eq!(events.iter().filter(|event| matches!(event.kind, EventKind::TurnStart(_))).count(), 1);
        assert_eq!(events.iter().filter(|event| matches!(event.kind, EventKind::TurnEnd(_))).count(), 1);
        assert!(events.iter().any(|event| matches!(&event.kind, EventKind::Text(data) if data.delta == "partial answer")));
        agent.close().await.unwrap();
    }).await.unwrap();
}

/// Streams the chunks a test sends to its first call; later calls echo.
struct ChannelModel(Mutex<Option<tokio::sync::mpsc::UnboundedReceiver<ModelChunk>>>);
impl ModelDriver for ChannelModel {
    fn invoke(
        &self,
        request: Arc<ModelRequest>,
        connection: dim_agent_capabilities::host::ModelConnection,
        cancel: CancellationToken,
    ) -> HostFuture<'_, ModelStream> {
        let Some(receiver) = self.0.lock().unwrap().take() else {
            return EchoModel.invoke(request, connection, cancel);
        };
        Box::pin(async move {
            let stream = futures_util::stream::unfold(receiver, |mut receiver| async move {
                receiver.recv().await.map(|chunk| (Ok(chunk), receiver))
            });
            Ok(Box::pin(stream) as ModelStream)
        })
    }
}

/// Output reaches the Timeline per fact: text once per block (split when a
/// block grows past the threshold), thinking once per whole block, a tool
/// call at its start and when complete. Until then it is progress state,
/// cleared as it is committed and when the request ends.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn model_output_is_committed_per_block_and_shown_as_progress_until_then() {
    tokio::time::timeout(Duration::from_secs(5), async {
        let (chunks, receiver) = tokio::sync::mpsc::unbounded_channel();
        let agent = MinimalHost::start(&builder(
            Arc::new(ChannelModel(Mutex::new(Some(receiver)))),
            Arc::new(MockTools),
        ))
        .await
        .unwrap();
        let session = agent
            .create_session(SessionConfig::default())
            .await
            .unwrap();
        session
            .submit("go".into(), InputMode::Queue, "main")
            .await
            .unwrap();
        let mut progress = session.on::<ModelProgress>("main").unwrap().unwrap();
        let outputs = |events: &[Arc<AgentEvent>]| -> Vec<String> {
            events
                .iter()
                .filter_map(|event| match &event.kind {
                    EventKind::Thinking(data) => Some(format!("thinking:{}", data.delta)),
                    EventKind::Text(data) => Some(format!("text:{}", data.delta.len())),
                    EventKind::ToolUseStart(data) => Some(format!("start:{}", data.call_id)),
                    EventKind::ToolUse(data) => Some(format!("use:{}", data.call.id)),
                    _ => None,
                })
                .collect()
        };
        let send = |list: Vec<ModelChunk>| {
            for chunk in list {
                chunks.send(chunk).unwrap();
            }
        };
        send(vec![
            ModelChunk::MessageStart {
                message_id: "blocks".into(),
            },
            ModelChunk::Thinking("deep ".into()),
            ModelChunk::Thinking("thought".into()),
        ]);
        progress
            .wait_for(|progress| progress.thinking == "deep thought")
            .await
            .unwrap();
        let open = session.get::<ModelProgress>("main").unwrap().unwrap();
        let thinking_part = open
            .thinking_part_id
            .clone()
            .expect("open thinking part id");
        let thinking_started = open.thinking_started_at.expect("open thinking start");
        assert!(outputs(&chain_events(&session, "main", ..).await.unwrap()).is_empty());
        send(vec![
            ModelChunk::ThinkingEnd {
                thinking: "deep thought".into(),
            },
            ModelChunk::Text("Hel".into()),
            ModelChunk::Text("lo".into()),
        ]);
        progress
            .wait_for(|progress| progress.text == "Hello" && progress.thinking.is_empty())
            .await
            .unwrap();
        let shown = session.get::<ModelProgress>("main").unwrap().unwrap();
        assert_eq!((shown.text_offset, shown.thinking_offset), (0, 12));
        let first_text_part = shown.text_part_id.clone().expect("first text part id");
        assert!(shown.thinking_part_id.is_none());
        assert!(shown.thinking_started_at.is_none());
        assert_eq!(
            outputs(&chain_events(&session, "main", ..).await.unwrap()),
            ["thinking:deep thought"]
        );
        let piece = "a".repeat(3000);
        send(vec![
            ModelChunk::TextEnd,
            ModelChunk::Text(piece.clone()),
            ModelChunk::Text(piece.clone()),
            ModelChunk::Text(piece),
            ModelChunk::TextEnd,
            ModelChunk::ToolStart {
                id: "call".into(),
                name: "echo".into(),
            },
            ModelChunk::ToolArguments {
                id: "call".into(),
                name: "echo".into(),
                delta: "{\"text\":".into(),
            },
        ]);
        progress
            .wait_for(|progress| {
                progress.text.is_empty()
                    && progress.tools
                        == [ToolProgress {
                            call_id: "call".into(),
                            name: "echo".into(),
                            arguments: "{\"text\":".into(),
                        }]
            })
            .await
            .unwrap();
        assert_eq!(
            outputs(&chain_events(&session, "main", ..).await.unwrap()),
            [
                "thinking:deep thought",
                "text:5",
                "text:6000",
                "text:3000",
                "start:call"
            ]
        );
        let shown = session.get::<ModelProgress>("main").unwrap().unwrap();
        assert_eq!((shown.text_offset, shown.thinking_offset), (9005, 12));
        assert!(shown.thinking_started_at.is_none());
        send(vec![
            ModelChunk::ToolArguments {
                id: "call".into(),
                name: "echo".into(),
                delta: "\"hi\"}".into(),
            },
            ModelChunk::ToolUse(ToolCall {
                id: "call".into(),
                name: "echo".into(),
                input: serde_json::from_value(serde_json::json!({"text":"hi"})).unwrap(),
            }),
            ModelChunk::MessageStop {
                message_id: "blocks".into(),
                reason: ModelStopReason::ToolUse,
            },
        ]);
        drop(chunks);
        session.idle("main").await.unwrap();
        let events = chain_events(&session, "main", ..).await.unwrap();
        assert_eq!(
            outputs(&events)[..6],
            [
                "thinking:deep thought",
                "text:5",
                "text:6000",
                "text:3000",
                "start:call",
                "use:call"
            ]
        );
        assert!(events.iter().any(|event| matches!(&event.kind,
            EventKind::ToolResult(data) if data.call_event_id.is_some())));
        let thinking_event = events
            .iter()
            .find(|event| matches!(event.kind, EventKind::Thinking(_)))
            .expect("thinking event");
        assert_eq!(thinking_event.id, thinking_part);
        assert_eq!(thinking_event.create_at, thinking_started);
        let text_events: Vec<_> = events
            .iter()
            .filter(|event| matches!(event.kind, EventKind::Text(_)))
            .collect();
        assert!(text_events.len() >= 3);
        assert_eq!(text_events[0].id, first_text_part);
        assert!(
            text_events[1..]
                .iter()
                .all(|event| event.id != first_text_part),
            "only the first event carries the block id"
        );
        assert_eq!(
            session.get::<ModelProgress>("main").unwrap().unwrap(),
            ModelProgress::default()
        );
        agent.close().await.unwrap();
    })
    .await
    .unwrap();
}

/// A thinking block's start is one producer-owned fact: progress keeps it
/// across deltas and the durable first event carries it, so clients can show
/// the same elapsed time while streaming and from history. A block that ends
/// and a new one that starts each own their start.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_thinking_block_keeps_its_start_in_progress_and_history() {
    tokio::time::timeout(Duration::from_secs(5), async {
        let (chunks, receiver) = tokio::sync::mpsc::unbounded_channel();
        let agent = MinimalHost::start(&builder(
            Arc::new(ChannelModel(Mutex::new(Some(receiver)))),
            Arc::new(MockTools),
        ))
        .await
        .unwrap();
        let session = agent
            .create_session(SessionConfig::default())
            .await
            .unwrap();
        session
            .submit("go".into(), InputMode::Queue, "main")
            .await
            .unwrap();
        let mut progress = session.on::<ModelProgress>("main").unwrap().unwrap();
        let send = |list: Vec<ModelChunk>| {
            for chunk in list {
                chunks.send(chunk).unwrap();
            }
        };
        send(vec![
            ModelChunk::MessageStart {
                message_id: "thinking-start".into(),
            },
            ModelChunk::Thinking("first".into()),
        ]);
        progress
            .wait_for(|progress| progress.thinking == "first")
            .await
            .unwrap();
        let open = session.get::<ModelProgress>("main").unwrap().unwrap();
        let part_id = open
            .thinking_part_id
            .clone()
            .expect("open thinking part id");
        let started_at = open.thinking_started_at.expect("thinking start");

        tokio::time::sleep(Duration::from_millis(20)).await;
        send(vec![ModelChunk::Thinking(" second".into())]);
        progress
            .wait_for(|progress| progress.thinking == "first second")
            .await
            .unwrap();
        let open = session.get::<ModelProgress>("main").unwrap().unwrap();
        assert_eq!(open.thinking_part_id.as_deref(), Some(part_id.as_str()));
        assert_eq!(open.thinking_started_at, Some(started_at));

        tokio::time::sleep(Duration::from_millis(20)).await;
        send(vec![ModelChunk::ThinkingEnd {
            thinking: "first second".into(),
        }]);
        progress
            .wait_for(|progress| progress.thinking_offset == 12 && progress.thinking.is_empty())
            .await
            .unwrap();
        let open = session.get::<ModelProgress>("main").unwrap().unwrap();
        assert_eq!(
            open.thinking_part_id.as_deref(),
            Some(part_id.as_str()),
            "a committed block stays open until another block replaces it"
        );
        assert_eq!(open.thinking_started_at, Some(started_at));

        send(vec![ModelChunk::Text("answer".into())]);
        progress
            .wait_for(|progress| progress.text == "answer")
            .await
            .unwrap();
        let open = session.get::<ModelProgress>("main").unwrap().unwrap();
        assert_eq!(open.thinking_started_at, None);

        tokio::time::sleep(Duration::from_millis(20)).await;
        send(vec![ModelChunk::Thinking("again".into())]);
        progress
            .wait_for(|progress| progress.thinking == "again")
            .await
            .unwrap();
        let next = session.get::<ModelProgress>("main").unwrap().unwrap();
        let next_part_id = next.thinking_part_id.clone().expect("next part id");
        let next_started_at = next.thinking_started_at.expect("next start");
        assert_ne!(next_part_id, part_id);
        assert!(
            next_started_at > started_at,
            "{next_started_at} <= {started_at}"
        );

        send(vec![
            ModelChunk::ThinkingEnd {
                thinking: "again".into(),
            },
            ModelChunk::ToolStart {
                id: "call".into(),
                name: "echo".into(),
            },
        ]);
        progress
            .wait_for(|progress| progress.tools.iter().any(|tool| tool.call_id == "call"))
            .await
            .unwrap();
        let shown = session.get::<ModelProgress>("main").unwrap().unwrap();
        assert!(
            shown.thinking_part_id.is_none(),
            "a tool start ends the thought"
        );
        assert_eq!(shown.thinking_started_at, None);

        send(vec![
            ModelChunk::ToolArguments {
                id: "call".into(),
                name: "echo".into(),
                delta: "{\"text\":\"hi\"}".into(),
            },
            ModelChunk::ToolUse(ToolCall {
                id: "call".into(),
                name: "echo".into(),
                input: serde_json::from_value(serde_json::json!({"text":"hi"})).unwrap(),
            }),
            ModelChunk::MessageStop {
                message_id: "thinking-start".into(),
                reason: ModelStopReason::ToolUse,
            },
        ]);
        drop(chunks);
        session.idle("main").await.unwrap();
        let events = chain_events(&session, "main", ..).await.unwrap();
        let thinking = events
            .iter()
            .find(|event| matches!(event.kind, EventKind::Thinking(_)))
            .expect("thinking event");
        assert_eq!(thinking.id, part_id);
        assert_eq!(thinking.create_at, started_at);
        let second = events
            .iter()
            .filter(|event| matches!(event.kind, EventKind::Thinking(_)))
            .nth(1)
            .expect("second thinking event");
        assert_eq!(second.id, next_part_id);
        assert_eq!(second.create_at, next_started_at);
        let text = events
            .iter()
            .find(|event| matches!(event.kind, EventKind::Text(_)))
            .expect("text event");
        assert!(
            text.create_at > thinking.create_at,
            "the block spans wall-clock time: {} -> {}",
            thinking.create_at,
            text.create_at
        );
        agent.close().await.unwrap();
    })
    .await
    .unwrap();
}

/// Provider end markers are not durable events: adjacent text or thinking
/// events stay one block, and progress keeps the block id history exposes.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn end_markers_do_not_split_adjacent_blocks() {
    tokio::time::timeout(Duration::from_secs(5), async {
        let (chunks, receiver) = tokio::sync::mpsc::unbounded_channel();
        let agent = MinimalHost::start(&builder(
            Arc::new(ChannelModel(Mutex::new(Some(receiver)))),
            Arc::new(MockTools),
        ))
        .await
        .unwrap();
        let session = agent
            .create_session(SessionConfig::default())
            .await
            .unwrap();
        session
            .submit("go".into(), InputMode::Queue, "main")
            .await
            .unwrap();
        let mut progress = session.on::<ModelProgress>("main").unwrap().unwrap();
        let send = |list: Vec<ModelChunk>| {
            for chunk in list {
                chunks.send(chunk).unwrap();
            }
        };
        send(vec![
            ModelChunk::MessageStart {
                message_id: "ends".into(),
            },
            ModelChunk::Thinking("first".into()),
        ]);
        progress
            .wait_for(|progress| progress.thinking == "first")
            .await
            .unwrap();
        let thinking_id = session
            .get::<ModelProgress>("main")
            .unwrap()
            .unwrap()
            .thinking_part_id
            .expect("open thinking part id");
        send(vec![
            ModelChunk::ThinkingEnd {
                thinking: "first".into(),
            },
            ModelChunk::Thinking("second".into()),
        ]);
        progress
            .wait_for(|progress| {
                progress.thinking.ends_with("second")
                    && progress.thinking_part_id.as_deref() == Some(thinking_id.as_str())
            })
            .await
            .unwrap();
        send(vec![
            ModelChunk::ThinkingEnd {
                thinking: "second".into(),
            },
            ModelChunk::Text("a".into()),
        ]);
        progress
            .wait_for(|progress| progress.text == "a")
            .await
            .unwrap();
        let text_id = session
            .get::<ModelProgress>("main")
            .unwrap()
            .unwrap()
            .text_part_id
            .expect("open text part id");
        send(vec![ModelChunk::TextEnd, ModelChunk::Text("b".into())]);
        progress
            .wait_for(|progress| {
                progress.text.ends_with('b')
                    && progress.text_part_id.as_deref() == Some(text_id.as_str())
            })
            .await
            .unwrap();
        send(vec![
            ModelChunk::TextEnd,
            ModelChunk::MessageStop {
                message_id: "ends".into(),
                reason: ModelStopReason::EndTurn,
            },
        ]);
        drop(chunks);
        session.idle("main").await.unwrap();
        let events = chain_events(&session, "main", ..).await.unwrap();
        let thinking: Vec<_> = events
            .iter()
            .filter_map(|event| match &event.kind {
                EventKind::Thinking(data) => Some((data.delta.as_str(), event.id.as_str())),
                _ => None,
            })
            .collect();
        assert_eq!(
            thinking.iter().map(|(delta, _)| *delta).collect::<Vec<_>>(),
            ["first", "second"]
        );
        assert_eq!(thinking[0].1, thinking_id.as_str());
        let texts: Vec<_> = events
            .iter()
            .filter_map(|event| match &event.kind {
                EventKind::Text(data) => Some((data.delta.as_str(), event.id.as_str())),
                _ => None,
            })
            .collect();
        assert_eq!(
            texts.iter().map(|(delta, _)| *delta).collect::<Vec<_>>(),
            ["a", "b"]
        );
        assert_eq!(texts[0].1, text_id.as_str());
        agent.close().await.unwrap();
    })
    .await
    .unwrap();
}

/// Text buffered under the size threshold is committed once its oldest part
/// is a second old, checked when the next text arrives; the progress offset
/// then counts the published text in Unicode scalar values. Under paused time
/// the timeout is the only liveness guard: time advances to the deadline only
/// when the flow is stuck, so a stuck flow fails instead of hanging the suite.
#[tokio::test(start_paused = true)]
async fn buffered_text_is_committed_once_a_second_old() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let (chunks, receiver) = tokio::sync::mpsc::unbounded_channel();
        let agent = MinimalHost::start(&builder(
            Arc::new(ChannelModel(Mutex::new(Some(receiver)))),
            Arc::new(MockTools),
        ))
        .await
        .unwrap();
        let session = agent
            .create_session(SessionConfig::default())
            .await
            .unwrap();
        session
            .submit("go".into(), InputMode::Queue, "main")
            .await
            .unwrap();
        let mut progress = session.on::<ModelProgress>("main").unwrap().unwrap();
        let texts = async || -> Vec<String> {
            chain_events(&session, "main", ..)
                .await
                .unwrap()
                .iter()
                .filter_map(|event| match &event.kind {
                    EventKind::Text(data) => Some(data.delta.clone()),
                    _ => None,
                })
                .collect()
        };
        for chunk in [
            ModelChunk::MessageStart {
                message_id: "aged".into(),
            },
            ModelChunk::Text("é".into()),
            ModelChunk::Text("ü".into()),
        ] {
            chunks.send(chunk).unwrap();
        }
        progress
            .wait_for(|progress| progress.text == "éü")
            .await
            .unwrap();
        assert!(texts().await.is_empty(), "younger than a second");
        tokio::time::advance(Duration::from_secs(1)).await;
        chunks.send(ModelChunk::Text("ß".into())).unwrap();
        progress
            .wait_for(|progress| progress.text.is_empty() && progress.text_offset == 3)
            .await
            .unwrap();
        assert_eq!(texts().await, ["éüß"]);
        for chunk in [
            ModelChunk::TextEnd,
            ModelChunk::MessageStop {
                message_id: "aged".into(),
                reason: ModelStopReason::EndTurn,
            },
        ] {
            chunks.send(chunk).unwrap();
        }
        drop(chunks);
        session.idle("main").await.unwrap();
        agent.close().await.unwrap();
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_held_input_is_withdrawn_once_and_an_applied_one_is_not() {
    use dim_agent_capabilities::input_queue::{InputQueue, Withdrawal};
    tokio::time::timeout(Duration::from_secs(10), async {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let agent = MinimalHost::start(&builder(
            Arc::new(RecordingModel(requests.clone())),
            Arc::new(BoundaryTools {
                entered: entered.clone(),
                release: release.clone(),
            }),
        ))
        .await
        .unwrap();
        let session = agent
            .create_session(SessionConfig {
                tools: Some(vec!["boundary".into()]),
                permission_policy: dim_agent_runtime::PermissionPolicy {
                    preset: dim_agent_runtime::PermissionPreset::FullAccess,
                    ..Default::default()
                },
                ..Default::default()
            })
            .await
            .unwrap();
        session
            .submit("tool:boundary {}".into(), InputMode::Queue, "main")
            .await
            .unwrap();
        entered.notified().await;
        let first = session
            .submit("first queued".into(), InputMode::Queue, "main")
            .await
            .unwrap();
        let second = session
            .submit("second queued".into(), InputMode::Queue, "main")
            .await
            .unwrap();
        let steered = session
            .submit("applied steer".into(), InputMode::Steer, "main")
            .await
            .unwrap();
        let queue = session.service::<InputQueue>("main").unwrap();
        let withdraw = |input_id: &str| InputWithdrawData {
            input_id: input_id.into(),
            reason: Some("user removed it".into()),
            steer: None,
        };

        let (answer, replacement) = queue.withdraw(withdraw(&first.id)).await.unwrap();
        let Withdrawal::Deleted { sequence } = answer else {
            panic!("held input not deleted: {answer:?}")
        };
        assert!(replacement.is_none());
        assert_eq!(
            queue.withdraw(withdraw(&first.id)).await.unwrap().0,
            Withdrawal::NotHeld
        );
        assert_eq!(
            queue.withdraw(withdraw("unknown")).await.unwrap().0,
            Withdrawal::NotHeld
        );
        let mut dispatch = session.dispatch_watch("main").unwrap();
        while !chain_events(&session, "main", ..)
            .await
            .unwrap()
            .iter()
            .any(|event| {
                matches!(&event.kind,
            EventKind::InputApplied(data) if data.input_id == steered.id)
            })
        {
            dispatch.changed().await.unwrap();
        }
        assert_eq!(
            queue.withdraw(withdraw(&steered.id)).await.unwrap().0,
            Withdrawal::NotHeld
        );
        let user = UserInput {
            content: vec![ContentBlock::Text {
                text: "second, now as steer".into(),
            }],
        };
        let (answer, replacement) = queue
            .withdraw(InputWithdrawData {
                steer: Some(InputMessageData {
                    user: Some(user),
                    mode: Some(InputMode::Steer),
                    ..Default::default()
                }),
                ..withdraw(&second.id)
            })
            .await
            .unwrap();
        assert!(matches!(answer, Withdrawal::Deleted { .. }));
        let replacement = replacement.unwrap();
        assert!(
            queue
                .withdraw(InputWithdrawData {
                    steer: Some(InputMessageData::text("x", Some(InputMode::Queue))),
                    ..withdraw(&second.id)
                })
                .await
                .is_err()
        );

        release.notify_one();
        session.idle("main").await.unwrap();
        let events = chain_events(&session, "main", ..).await.unwrap();
        let deleted: Vec<_> = events
            .iter()
            .filter_map(|event| match &event.kind {
                EventKind::InputDeleted(data) => Some((event.sequence, data.input_id.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(
            deleted,
            [
                (sequence, first.id.clone()),
                (deleted[1].0, second.id.clone())
            ]
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event.kind, EventKind::InputWithdraw(_)))
                .count(),
            5
        );
        for id in [&first.id, &second.id] {
            assert!(!events.iter().any(|event| matches!(&event.kind,
                EventKind::InputApplied(data) if &data.input_id == id)));
            assert!(!events.iter().any(|event| matches!(&event.kind,
                EventKind::InputConsumed(data) if &data.input_id == id)));
        }
        assert!(events.iter().any(|event| matches!(&event.kind,
            EventKind::InputConsumed(data) if data.input_id == replacement.id)));
        let requests = requests.lock().unwrap().clone();
        assert!(
            requests
                .iter()
                .all(|request| !request.messages.iter().any(|message| [
                    Message::text(Role::User, "first queued"),
                    Message::text(Role::User, "second queued")
                ]
                .contains(message)))
        );
        assert!(
            requests
                .last()
                .unwrap()
                .messages
                .contains(&Arc::new(Message::text(Role::User, "second, now as steer")))
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event.kind, EventKind::TurnStart(_)))
                .count(),
            1
        );
        agent.close().await.unwrap();
    })
    .await
    .unwrap();
}

/// One input addressed to the continuation of `turn_id`'s handover.
fn handover_input(text: &str, turn_id: &str, request_id: &str) -> InputMessageData {
    InputMessageData {
        user: Some(UserInput {
            content: vec![ContentBlock::Text { text: text.into() }],
        }),
        mode: Some(InputMode::Queue),
        handover: Some(InputHandover {
            turn_id: turn_id.into(),
            request_id: request_id.into(),
        }),
        ..Default::default()
    }
}

/// The id of the last turn this branch started.
async fn open_turn(session: &dim_agent_runtime::Session) -> String {
    chain_events(session, "main", ..)
        .await
        .unwrap()
        .iter()
        .rev()
        .find_map(|event| match &event.kind {
            EventKind::TurnStart(data) => Some(data.turn_id.clone()),
            _ => None,
        })
        .expect("a started turn")
}

async fn queue_consumed_through(session: &dim_agent_runtime::Session, sequence: u64) {
    let mut dispatch = session.dispatch_watch("main").unwrap();
    while dispatch.borrow_and_update().sequence < sequence {
        dispatch.changed().await.unwrap();
    }
}

/// Waits until InputQueue commits `input/deleted` for `id`.
async fn deleted(session: &dim_agent_runtime::Session, id: &str) -> Vec<Arc<AgentEvent>> {
    let mut dispatch = session.dispatch_watch("main").unwrap();
    loop {
        let events = chain_events(session, "main", ..).await.unwrap();
        if events.iter().any(
            |event| matches!(&event.kind, EventKind::InputDeleted(data) if data.input_id == id),
        ) {
            return events;
        }
        tokio::time::timeout(Duration::from_secs(5), dispatch.changed())
            .await
            .expect("input/deleted never committed")
            .unwrap();
    }
}

async fn boundary_session(agent: &MinimalHost) -> dim_agent_runtime::Session {
    agent
        .create_session(SessionConfig {
            tools: Some(vec!["boundary".into()]),
            permission_policy: dim_agent_runtime::PermissionPolicy {
                preset: dim_agent_runtime::PermissionPreset::FullAccess,
                ..Default::default()
            },
            ..Default::default()
        })
        .await
        .unwrap()
}

/// A handover input waits for the turn it names: InputQueue writes no
/// `input/applied` and the published queue size does not count it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_handover_input_is_held_without_being_applied_or_counted() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let agent = MinimalHost::start(&builder(
            Arc::new(RecordingModel(requests.clone())),
            Arc::new(BoundaryTools {
                entered: entered.clone(),
                release: release.clone(),
            }),
        ))
        .await
        .unwrap();
        let session = boundary_session(&agent).await;
        session
            .submit("tool:boundary {}".into(), InputMode::Queue, "main")
            .await
            .unwrap();
        entered.notified().await;
        let turn = open_turn(&session).await;
        let queued = session
            .submit("operator next".into(), InputMode::Queue, "main")
            .await
            .unwrap();
        queue_consumed_through(&session, queued.sequence).await;
        assert_eq!(
            session
                .get::<InputSnapshot>("main")
                .unwrap()
                .unwrap()
                .queued,
            1
        );
        let handover = session
            .input(handover_input("handover text", &turn, "request-1"), "main")
            .await
            .unwrap();
        queue_consumed_through(&session, handover.sequence).await;
        assert_eq!(
            session
                .get::<InputSnapshot>("main")
                .unwrap()
                .unwrap()
                .queued,
            1,
            "the handover input is not counted"
        );
        let events = chain_events(&session, "main", ..).await.unwrap();
        assert!(!events.iter().any(|event| matches!(&event.kind,
            EventKind::InputApplied(data) if data.input_id == handover.id)));
        assert!(!events.iter().any(|event| matches!(&event.kind,
            EventKind::InputDeleted(data) if data.input_id == handover.id)));
        assert!(!events.iter().any(|event| matches!(&event.kind,
            EventKind::InputConsumed(data) if data.input_id == handover.id)));
        release.notify_one();
        session.idle("main").await.unwrap();
        agent.close().await.unwrap();
    })
    .await
    .unwrap();
}

/// A handover input its turn ends without consuming is deleted exactly
/// once, and no later request carries its text.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_handover_input_is_deleted_at_its_turn_end_and_never_reaches_the_model() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let agent = MinimalHost::start(&builder(
            Arc::new(RecordingModel(requests.clone())),
            Arc::new(BoundaryTools {
                entered: entered.clone(),
                release: release.clone(),
            }),
        ))
        .await
        .unwrap();
        let session = boundary_session(&agent).await;
        session
            .submit("tool:boundary {}".into(), InputMode::Queue, "main")
            .await
            .unwrap();
        entered.notified().await;
        let turn = open_turn(&session).await;
        let handover = session
            .input(handover_input("handover text", &turn, "request-1"), "main")
            .await
            .unwrap();
        queue_consumed_through(&session, handover.sequence).await;
        release.notify_one();
        deleted(&session, &handover.id).await;
        session.idle("main").await.unwrap();
        let events = chain_events(&session, "main", ..).await.unwrap();
        let deleted_twice = events
            .iter()
            .filter(|event| matches!(&event.kind, EventKind::InputDeleted(data) if data.input_id == handover.id))
            .count();
        assert_eq!(deleted_twice, 1, "exactly one input/deleted");
        let turn_end = events
            .iter()
            .find(|event| matches!(event.kind, EventKind::TurnEnd(_)))
            .unwrap();
        let deleted_at = events
            .iter()
            .find(|event| matches!(&event.kind, EventKind::InputDeleted(data) if data.input_id == handover.id))
            .unwrap();
        assert!(turn_end.sequence < deleted_at.sequence);
        session
            .submit("after".into(), InputMode::Queue, "main")
            .await
            .unwrap();
        session.idle("main").await.unwrap();
        let requests = requests.lock().unwrap().clone();
        assert!(requests
            .last()
            .unwrap()
            .messages
            .iter()
            .all(|message| *message != Arc::new(Message::text(Role::User, "handover text"))));
        agent.close().await.unwrap();
    })
    .await
    .unwrap();
}

/// A handover input with no open turn of its own is deleted on arrival.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_handover_input_for_no_open_turn_is_deleted_on_arrival() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let agent = MinimalHost::start(&builder(
            Arc::new(RecordingModel(requests.clone())),
            Arc::new(BoundaryTools {
                entered: entered.clone(),
                release: release.clone(),
            }),
        ))
        .await
        .unwrap();
        let session = boundary_session(&agent).await;
        session
            .submit("tool:boundary {}".into(), InputMode::Queue, "main")
            .await
            .unwrap();
        entered.notified().await;
        let turn = open_turn(&session).await;
        let wrong = session
            .input(
                handover_input("another turn", "turn-that-is-not-open", "request-1"),
                "main",
            )
            .await
            .unwrap();
        let events = deleted(&session, &wrong.id).await;
        assert!(
            !events.iter().any(|event| matches!(&event.kind,
                EventKind::InputApplied(data) if data.input_id == wrong.id)),
            "a dead-on-arrival handover input is not applied"
        );
        assert_eq!(
            session
                .get::<InputSnapshot>("main")
                .unwrap()
                .unwrap()
                .queued,
            0
        );
        release.notify_one();
        session.idle("main").await.unwrap();
        let closed = session
            .input(handover_input("closed turn", &turn, "request-2"), "main")
            .await
            .unwrap();
        deleted(&session, &closed.id).await;
        let events = chain_events(&session, "main", ..).await.unwrap();
        assert!(!events.iter().any(|event| matches!(&event.kind,
            EventKind::InputApplied(data) if data.input_id == closed.id)));
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event.kind, EventKind::TurnStart(_)))
                .count(),
            1,
            "the idle handover input starts no turn"
        );
        agent.close().await.unwrap();
    })
    .await
    .unwrap();
}

/// A handover input its continuation consumes is not deleted at turn end.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_consumed_handover_input_is_not_deleted_at_turn_end() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let slot = Arc::new(Mutex::new(None));
        let mut builder = builder(
            Arc::new(RecordingModel(requests.clone())),
            Arc::new(BoundaryTools {
                entered: entered.clone(),
                release: release.clone(),
            }),
        );
        let copy = slot.clone();
        builder.require_feature(Feature::new("test.timeline-handle").branch(
            "test.timeline-handle",
            move |names| {
                Ok(Box::new(TimelineHandle {
                    timeline: names.key()?,
                    slot: copy.clone(),
                }))
            },
        ));
        let agent = MinimalHost::start(&builder).await.unwrap();
        let session = boundary_session(&agent).await;
        session
            .submit("tool:boundary {}".into(), InputMode::Queue, "main")
            .await
            .unwrap();
        entered.notified().await;
        let turn = open_turn(&session).await;
        let handover = session
            .input(handover_input("handover text", &turn, "request-1"), "main")
            .await
            .unwrap();
        queue_consumed_through(&session, handover.sequence).await;
        let timeline = slot.lock().unwrap().clone().expect("the branch Timeline");
        let consumed = timeline
            .append(
                EventKind::InputConsumed(InputConsumedData {
                    input_id: handover.id.clone(),
                })
                .into(),
            )
            .unwrap();
        queue_consumed_through(&session, consumed.sequence).await;
        release.notify_one();
        session.idle("main").await.unwrap();
        let events = chain_events(&session, "main", ..).await.unwrap();
        assert!(
            !events.iter().any(|event| matches!(&event.kind,
                EventKind::InputDeleted(data) if data.input_id == handover.id)),
            "a consumed handover input is not deleted"
        );
        agent.close().await.unwrap();
    })
    .await
    .unwrap();
}

/// A held handover input neither steals the turn-end release from an
/// operator's queued input nor starts a turn by itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_held_handover_input_does_not_steal_the_next_queued_input() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let agent = MinimalHost::start(&builder(
            Arc::new(RecordingModel(requests.clone())),
            Arc::new(BoundaryTools {
                entered: entered.clone(),
                release: release.clone(),
            }),
        ))
        .await
        .unwrap();
        let session = boundary_session(&agent).await;
        session
            .submit("tool:boundary {}".into(), InputMode::Queue, "main")
            .await
            .unwrap();
        entered.notified().await;
        let turn = open_turn(&session).await;
        let handover = session
            .input(handover_input("handover text", &turn, "request-1"), "main")
            .await
            .unwrap();
        queue_consumed_through(&session, handover.sequence).await;
        let operator = session
            .submit("operator next".into(), InputMode::Queue, "main")
            .await
            .unwrap();
        queue_consumed_through(&session, operator.sequence).await;
        release.notify_one();
        deleted(&session, &handover.id).await;
        session.idle("main").await.unwrap();
        let events = chain_events(&session, "main", ..).await.unwrap();
        assert!(events.iter().any(|event| matches!(&event.kind,
            EventKind::InputApplied(data) if data.input_id == operator.id)));
        assert!(events.iter().any(|event| matches!(&event.kind,
            EventKind::InputConsumed(data) if data.input_id == operator.id)));
        let applied_at = events
            .iter()
            .find(|event| matches!(&event.kind, EventKind::InputApplied(data) if data.input_id == operator.id))
            .unwrap();
        let turn_end = events
            .iter()
            .find(|event| matches!(event.kind, EventKind::TurnEnd(_)))
            .unwrap();
        assert!(turn_end.sequence < applied_at.sequence);
        let requests = requests.lock().unwrap().clone();
        let last = requests.last().unwrap();
        assert!(last
            .messages
            .contains(&Arc::new(Message::text(Role::User, "operator next"))));
        assert!(!last
            .messages
            .iter()
            .any(|message| *message == Arc::new(Message::text(Role::User, "handover text"))));
        agent.close().await.unwrap();
    })
    .await
    .unwrap();
}

/// A handover input written while idle starts no turn.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_handover_input_written_while_idle_starts_no_turn() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let agent = MinimalHost::start(&builder(
            Arc::new(RecordingModel(requests.clone())),
            Arc::new(MockTools),
        ))
        .await
        .unwrap();
        let session = agent
            .create_session(SessionConfig::default())
            .await
            .unwrap();
        let handover = session
            .input(
                handover_input("handover text", "turn-1", "request-1"),
                "main",
            )
            .await
            .unwrap();
        let events = deleted(&session, &handover.id).await;
        assert!(
            !events
                .iter()
                .any(|event| matches!(event.kind, EventKind::TurnStart(_)))
        );
        assert!(!events.iter().any(|event| matches!(&event.kind,
            EventKind::InputApplied(data) if data.input_id == handover.id)));
        assert_eq!(
            session
                .get::<InputSnapshot>("main")
                .unwrap()
                .unwrap()
                .queued,
            0
        );
        assert!(requests.lock().unwrap().is_empty());
        agent.close().await.unwrap();
    })
    .await
    .unwrap();
}

/// A withdrawn handover input is deleted once, by the withdrawal; its
/// turn's end writes no second deletion.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_withdrawn_handover_input_is_deleted_once() {
    use dim_agent_capabilities::input_queue::{InputQueue, Withdrawal};
    tokio::time::timeout(Duration::from_secs(10), async {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let agent = MinimalHost::start(&builder(
            Arc::new(RecordingModel(requests.clone())),
            Arc::new(BoundaryTools {
                entered: entered.clone(),
                release: release.clone(),
            }),
        ))
        .await
        .unwrap();
        let session = boundary_session(&agent).await;
        session
            .submit("tool:boundary {}".into(), InputMode::Queue, "main")
            .await
            .unwrap();
        entered.notified().await;
        let turn = open_turn(&session).await;
        let handover = session
            .input(handover_input("handover text", &turn, "request-1"), "main")
            .await
            .unwrap();
        queue_consumed_through(&session, handover.sequence).await;
        let queue = session.service::<InputQueue>("main").unwrap();
        let (answer, replacement) = queue
            .withdraw(InputWithdrawData {
                input_id: handover.id.clone(),
                reason: Some("user removed it".into()),
                steer: None,
            })
            .await
            .unwrap();
        let Withdrawal::Deleted { sequence } = answer else {
            panic!("held handover input not deleted: {answer:?}")
        };
        assert!(replacement.is_none());
        let events = deleted(&session, &handover.id).await;
        let first = events
            .iter()
            .find(|event| {
                matches!(&event.kind, EventKind::InputDeleted(data) if data.input_id == handover.id)
            })
            .unwrap();
        assert_eq!(first.sequence, sequence);
        release.notify_one();
        session.idle("main").await.unwrap();
        let events = chain_events(&session, "main", ..).await.unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(&event.kind,
                    EventKind::InputDeleted(data) if data.input_id == handover.id))
                .count(),
            1,
            "the turn's end writes no second deletion"
        );
        assert!(!events.iter().any(|event| matches!(&event.kind,
            EventKind::InputApplied(data) if data.input_id == handover.id)));
        agent.close().await.unwrap();
    })
    .await
    .unwrap();
}

fn model_with_vision(model_id: &str, vision: bool) -> ModelSelection {
    ModelSelection {
        provider_id: "test".into(),
        model_id: model_id.into(),
        capability_snapshot: Some(ModelCapabilitySnapshot {
            target: ModelId {
                provider_id: "test".into(),
                model_id: model_id.into(),
            },
            capabilities: ModelCapabilities {
                modalities: vec!["text".into()],
                tool_calling: true,
                streaming: true,
                structured_output: false,
                vision,
                audio: false,
                reasoning: false,
                context_window: 32_000,
                max_output_tokens: 4096,
                provider_state: None,
            },
            reasoning: None,
            service_tier: None,
        }),
        config: None,
        metadata: None,
    }
}
fn image(media_type: &str, data: Option<&str>, url: Option<&str>) -> ContentBlock {
    ContentBlock::Image {
        source: ImageSource {
            source_type: if url.is_some() {
                ImageSourceType::Url
            } else {
                ImageSourceType::Base64
            },
            media_type: media_type.into(),
            data: data.map(str::to_owned),
            url: url.map(str::to_owned),
        },
    }
}
fn sent_parts(request: &ModelRequest) -> Vec<String> {
    request
        .messages
        .iter()
        .flat_map(|message| match &message.content {
            MessageContent::Parts(parts) => parts
                .iter()
                .map(|part| match part {
                    MessagePart::Image { source } => format!("image:{}", source.media_type),
                    MessagePart::Text { text, .. } => text.clone(),
                    _ => String::new(),
                })
                .collect(),
            MessageContent::Text(text) => vec![text.clone()],
        })
        .collect()
}

#[tokio::test]
async fn a_session_switched_to_a_text_only_model_sends_image_placeholders() {
    tokio::time::timeout(Duration::from_secs(5), async {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let agent = MinimalHost::start(&builder(Arc::new(RecordingModel(requests.clone())), Arc::new(MockTools))).await.unwrap();
        let session = agent.create_session(SessionConfig::default()).await.unwrap();
        session
            .configure(ConfigureUpdate { model: Some(model_with_vision("seeing", true)), ..Default::default() }, "main")
            .await
            .unwrap();
        let png = "iVBORw0KGgo=";
        session
            .input(
                InputMessageData {
                    user: Some(UserInput {
                        content: vec![
                            ContentBlock::Text { text: "look".into() },
                            image("image/png", Some(png), None),
                            image("image/png", None, Some("https://example.test/a.png")),
                            image("image/bmp", Some(png), None),
                            image("image/png", Some("not base64!"), None),
                        ],
                    }),
                    mode: Some(InputMode::Queue),
                    ..Default::default()
                },
                "main",
            )
            .await
            .unwrap();
        session.idle("main").await.unwrap();
        let unsendable = "[Image omitted: the image cannot be sent to this model.]";
        assert_eq!(
            sent_parts(requests.lock().unwrap().last().unwrap()),
            ["look", "image:image/png", unsendable, unsendable, unsendable]
        );

        session
            .configure(ConfigureUpdate { model: Some(model_with_vision("text-only", false)), ..Default::default() }, "main")
            .await
            .unwrap();
        session.submit("next".into(), InputMode::Queue, "main").await.unwrap();
        session.idle("main").await.unwrap();
        let request = requests.lock().unwrap().last().unwrap().clone();
        assert_eq!(request.model_id, "text-only");
        let omitted = "[Image omitted: the current model does not accept images.]";
        let parts = sent_parts(&request);
        assert_eq!(parts[..5], ["look", omitted, omitted, omitted, omitted]);
        assert!(parts.contains(&"next".to_string()));
        let history = chain_events(&session, "main", ..).await.unwrap();
        assert!(history.iter().any(|event| matches!(&event.kind, EventKind::Text(data) if data.delta.contains("next"))));
        assert!(history.iter().any(|event| matches!(&event.kind,
            EventKind::InputMessage(data) if data.user.as_ref().is_some_and(|user| user.content.iter().any(|block| matches!(block, ContentBlock::Image { .. }))))));
        assert!(history.iter().any(|event| matches!(&event.kind,
            EventKind::ModelCall(call) if call.request.model_id == "text-only"
                && call.request.messages.iter().any(|message| matches!(&message.content,
                    MessageContent::Parts(parts) if parts.iter().any(|part| matches!(part, MessagePart::Image { .. })))))));
        agent.close().await.unwrap();
    })
    .await
    .unwrap();
}

const TOOL_IMAGE: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jD1sAAAAASUVORK5CYII=";

#[tokio::test]
async fn replacing_a_tool_image_preserves_other_messages_and_the_stored_result() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let agent = MinimalHost::start(&builder(
        Arc::new(RecordingModel(requests.clone())),
        Arc::new(Screenshot(true)),
    ))
    .await
    .unwrap();
    let session = agent
        .create_session(SessionConfig {
            model: model_with_vision("text-only", false),
            tools: Some(vec!["shot".into()]),
            ..Default::default()
        })
        .await
        .unwrap();
    session
        .submit("keep this answer".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    session.idle("main").await.unwrap();
    session
        .submit("tool:shot {}".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    session.idle("main").await.unwrap();
    let sent = requests.lock().unwrap().last().unwrap().clone();
    let events = chain_events(&session, "main", ..).await.unwrap();
    let stored = events
        .iter()
        .rev()
        .find_map(|event| match &event.kind {
            EventKind::ModelCall(call) => Some(call.request.clone()),
            _ => None,
        })
        .unwrap();
    assert!(!Arc::ptr_eq(&sent, &stored));
    assert_eq!(sent.messages.len(), stored.messages.len());
    let mut replaced = 0;
    for (before, after) in stored.messages.iter().zip(&sent.messages) {
        if before == after {
            assert!(Arc::ptr_eq(before, after), "unchanged messages stay shared");
            continue;
        }
        replaced += 1;
        let MessageContent::Parts(parts) = &after.content else {
            panic!("tool results contain parts")
        };
        let [MessagePart::ToolResult { content, .. }] = parts.as_slice() else {
            panic!("one tool result is adapted")
        };
        assert_eq!(
            content,
            &ToolContent::Blocks(vec![
                ContentBlock::Text {
                    text: "screenshot".into()
                },
                ContentBlock::Text {
                    text: "[Image omitted: the current model does not accept images.]".into()
                }
            ])
        );
    }
    assert_eq!(replaced, 1);
    assert!(events.iter().any(|event| matches!(&event.kind,
        EventKind::ToolResult(data) if data.results.iter().any(|result| matches!(
            &result.result.content,
            ToolContent::Blocks(blocks) if blocks.iter().any(|block| matches!(block, ContentBlock::Image { .. }))
        ))
    )));
    agent.close().await.unwrap();
}

/// A tool whose result is a text block, with or without a PNG block after it.
struct Screenshot(bool);
impl ToolDriver for Screenshot {
    fn definitions(&self) -> Vec<ToolDefinition> {
        vec![ToolDefinition {
            risk: Some("session".into()),
            ..definition(
                "shot",
                "Capture the screen.",
                serde_json::json!({"type":"object","properties":{},"additionalProperties":false}),
            )
        }]
    }
    fn execute(
        &self,
        _: ToolCall,
        _: ToolExecutionContext,
        _: CancellationToken,
    ) -> HostFuture<'_, ToolResult> {
        let image = self.0;
        Box::pin(async move {
            let mut blocks = vec![ContentBlock::Text {
                text: "screenshot".into(),
            }];
            if image {
                blocks.push(ContentBlock::Image {
                    source: ImageSource {
                        source_type: ImageSourceType::Base64,
                        media_type: "image/png".into(),
                        data: Some(TOOL_IMAGE.into()),
                        url: None,
                    },
                });
            }
            Ok(ToolResult {
                content: ToolContent::Blocks(blocks),
                structured_content: None,
                is_error: None,
                blob_refs: None,
                background_task: None,
                context_messages: None,
                effects: None,
            })
        })
    }
}

/// The compaction strategy resolves a connection only when it compacts; this
/// turn never does.
struct NoConnection;
impl ResolveModelConnection for NoConnection {
    fn resolve(
        &self,
        _: Arc<ModelRequest>,
        _: CancellationToken,
    ) -> HostFuture<'_, ResolvedModelConnection> {
        Box::pin(async {
            Ok(ResolvedModelConnection {
                connection: ModelConnection::Local,
                failure_policy: None,
                recovery: None,
            })
        })
    }
}

/// The turn's context estimates, one per model call, for a tool that returns a
/// text block with or without an image.
async fn tool_result_estimates(image: bool) -> Vec<f64> {
    let mut builder = AgentBuilder::new(AgentConfig::default());
    builder
        .session_value(Arc::new(ToolHost(Arc::new(Screenshot(image)))))
        .session_value(Arc::new(
            dim_agent_capabilities::history::HistoryRenderer::default(),
        ))
        .session_value(Arc::new(PrepareContextHost(Arc::new(IdentityContext))))
        .require_feature(
            Feature::new("PrepareModelHost").session("PrepareModelHost", |names| {
                Ok(Box::new(DefaultPrepareModelPlugin::new(names)?))
            }),
        );
    defaults::install_compaction(
        &mut builder,
        Arc::new(dim_agent_capabilities::context_model::ProductContextModel),
        Default::default(),
    );
    defaults::install(&mut builder);
    let agent = MinimalHost::start_with(&builder, |root, _| {
        root.provide(ModelHost(Arc::new(EchoModel))).unwrap();
        root.provide(ProviderConnectionResolver(Arc::new(NoConnection)))
            .unwrap();
        Ok(())
    })
    .await
    .unwrap();
    let mut config = SessionConfig {
        tools: Some(vec!["shot".into()]),
        ..Default::default()
    };
    config.model.capability_snapshot = Some(ModelCapabilitySnapshot {
        target: ModelId {
            provider_id: config.model.provider_id.clone(),
            model_id: config.model.model_id.clone(),
        },
        capabilities: ModelCapabilities::with_limits(128_000, 4_096),
        reasoning: None,
        service_tier: None,
    });
    let session = agent.create_session(config).await.unwrap();
    session
        .submit("tool:shot {}".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    session.idle("main").await.unwrap();
    let events = chain_events(&session, "main", ..).await.unwrap();
    assert!(
        matches!(
            events
                .iter()
                .rev()
                .find(|event| matches!(event.kind, EventKind::TurnEnd(_)))
                .unwrap()
                .kind,
            EventKind::TurnEnd(TurnEndData {
                outcome: TurnOutcome::Completed { .. },
                ..
            })
        ),
        "{:?}",
        events
            .iter()
            .filter_map(|event| match &event.kind {
                EventKind::ModelOutputEnd(data) => Some(&data.outcome),
                _ => None,
            })
            .collect::<Vec<_>>()
    );
    let estimates = events
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::ContextBudget(data) => Some(data.estimated_input_tokens),
            _ => None,
        })
        .collect();
    agent.close().await.unwrap();
    estimates
}

#[tokio::test]
async fn a_tool_result_image_counts_as_one_image_in_the_estimate() {
    let with = tool_result_estimates(true).await;
    let without = tool_result_estimates(false).await;
    assert_eq!(with.len(), 2, "{with:?}");
    assert_eq!(without.len(), 2, "{without:?}");
    let counted = with[1] - without[1];
    assert!(
        (1_500. ..=1_700.).contains(&counted),
        "one image, not its base64 size: {with:?} against {without:?}"
    );
}

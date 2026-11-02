use dim_agent_capabilities::{
    adapters::{DefaultPrepareModelPlugin, EchoModel, MockTools},
    defaults,
    history::{
        CheckpointBuilder, HistoryEntry, HistoryRender, HistoryRenderer, project_context_history,
        project_history, projection_checkpoint, summary_message,
    },
    host::*,
};
use dim_agent_runtime::testing::MinimalHost;
use dim_agent_runtime::*;
use dim_agent_types::*;
use futures_util::StreamExt;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, Key, TypeKey};
use rutis::{Ctx, Plugin};
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Copy, Default)]
struct Context {
    foreign_cursor: bool,
    threshold: Option<f64>,
    /// The planned replacement size reported to the retention rule; `None`
    /// reports the size of the replacement this strategy returns.
    replacement: Option<f64>,
    /// The text of the summary message this strategy returns; `None` returns
    /// `"short"`. It can overshoot the planned size, which stays planned
    /// (ContextCompaction §5.2).
    summary: Option<&'static str>,
    /// Window facts override the default 4096 / 64 when nonzero.
    window: f64,
    output: f64,
}
impl ResolveContextModel for Context {
    fn resolve(
        &self,
        _: Arc<ModelRequest>,
        _: CancellationToken,
    ) -> HostFuture<'_, Arc<dyn ContextModel>> {
        let model = *self;
        Box::pin(async move { Ok(Arc::new(model) as Arc<dyn ContextModel>) })
    }
}
impl ContextModel for Context {
    fn context_window(&self) -> f64 {
        if self.window == 0. {
            4096.
        } else {
            self.window
        }
    }
    fn output_tokens(&self) -> f64 {
        if self.output == 0. { 64. } else { self.output }
    }
    fn threshold_tokens(&self) -> f64 {
        self.threshold.unwrap_or(10_000.)
    }
    fn count_tokens(&self, request: &ModelRequest) -> f64 {
        serde_json::to_vec(&request.messages).unwrap().len() as f64
    }
}
fn test_diagnostics() -> StrategyDiagnostics {
    StrategyDiagnostics {
        model_usage: None,
        model_call_count: 1,
        record: CompactionStrategyRecord {
            kind: "test".into(),
            details: JsonObject::new(),
        },
    }
}
/// The test strategy: the legal cut the retention rule allows, or a cut
/// outside the fixed context (`foreign_cursor`), or one that separates the
/// first tool call from its result (`foreign_cursor` with a 10,000 threshold).
impl CompactionStrategy for Context {
    fn name(&self) -> &str {
        "test"
    }
    fn replacement_size(&self, model: &dyn ContextModel) -> f64 {
        self.replacement.unwrap_or_else(|| {
            model.count_tokens(&ModelRequest::new(
                "",
                "",
                vec![dim_agent_capabilities::history::summary_message("short")],
            ))
        })
    }
    fn compact(
        &self,
        input: CompactionInput,
        _: CancellationToken,
    ) -> HostFuture<'_, StrategyOutcome> {
        Box::pin(async move {
            let retain_from = if self.foreign_cursor && self.threshold != Some(10_000.) {
                "foreign-history-entry".into()
            } else if self.foreign_cursor {
                let call = input
                    .history
                    .iter()
                    .position(|entry| match &entry.message.content {
                        MessageContent::Parts(parts) => parts
                            .iter()
                            .any(|part| matches!(part, MessagePart::ToolUse { .. })),
                        MessageContent::Text(_) => false,
                    })
                    .unwrap();
                input.history[call + 1].source.clone()
            } else {
                match input.cut() {
                    Some(cut) => cut.retain_from.clone(),
                    None => {
                        return Ok(StrategyOutcome::Skipped {
                            reason: "nothing_to_compact".into(),
                            diagnostics: test_diagnostics(),
                        });
                    }
                }
            };
            Ok(StrategyOutcome::Completed {
                retain_from,
                replacement: vec![dim_agent_capabilities::history::summary_message(
                    self.summary.unwrap_or("short"),
                )],
                diagnostics: test_diagnostics(),
            })
        })
    }
}

struct ToolThenBudgetFailure(AtomicUsize);
impl ModelDriver for ToolThenBudgetFailure {
    fn invoke(
        &self,
        _: Arc<ModelRequest>,
        _: dim_agent_capabilities::host::ModelConnection,
        _: CancellationToken,
    ) -> HostFuture<'_, ModelStream> {
        let call = self.0.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            if call == 0 {
                return Ok(Box::pin(futures_util::stream::iter(vec![
                    Ok(ModelChunk::MessageStart {
                        message_id: "tool-turn".into(),
                    }),
                    Ok(ModelChunk::ToolStart {
                        id: "echo-call".into(),
                        name: "echo".into(),
                    }),
                    Ok(ModelChunk::ToolUse(ToolCall {
                        id: "echo-call".into(),
                        name: "echo".into(),
                        input: serde_json::from_value(serde_json::json!({"text":"result"}))
                            .unwrap(),
                    })),
                    Ok(ModelChunk::MessageStop {
                        message_id: "tool-turn".into(),
                        reason: ModelStopReason::ToolUse,
                    }),
                ])) as ModelStream);
            }
            Err(HostError::Failed {
                kind: HostErrorKind::Other,
                message: "Provider output budget exhausted".into(),
                model: None,
                presentation: None,
                budget: Some(ProviderOutputBudgetFacts {
                    input_tokens: 800.,
                    context_window: 1000.,
                    requested_max_tokens: 400.,
                    allowed_max_tokens: 200.,
                    ratio: 1.,
                }),
            })
        })
    }
}
struct BudgetFailure(Arc<AtomicUsize>);
impl ModelDriver for BudgetFailure {
    fn invoke(
        &self,
        _: Arc<ModelRequest>,
        _: dim_agent_capabilities::host::ModelConnection,
        _: CancellationToken,
    ) -> HostFuture<'_, ModelStream> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Box::pin(async {
            Err(HostError::Failed {
                kind: HostErrorKind::Other,
                message: "Provider output budget exhausted".into(),
                model: None,
                presentation: None,
                budget: Some(ProviderOutputBudgetFacts {
                    input_tokens: 800.,
                    context_window: 1000.,
                    requested_max_tokens: 400.,
                    allowed_max_tokens: 200.,
                    ratio: 1.,
                }),
            })
        })
    }
}
fn builder(model: Arc<dyn ModelDriver>) -> AgentBuilder {
    builder_with_context(model, HistoryRenderer::default(), Context::default())
}
fn builder_with_context(
    model: Arc<dyn ModelDriver>,
    history: HistoryRenderer,
    context: Context,
) -> AgentBuilder {
    builder_with_prepare(model, history, context, Arc::new(IdentityContext))
}
fn builder_with_prepare(
    model: Arc<dyn ModelDriver>,
    history: HistoryRenderer,
    context: Context,
    prepare: Arc<dyn PrepareContext>,
) -> AgentBuilder {
    let mut builder = AgentBuilder::new(AgentConfig::default());
    builder
        .process_value(Arc::new(ModelHost(model)))
        .session_value(Arc::new(ToolHost(Arc::new(MockTools))))
        .session_value(Arc::new(history))
        .session_value(Arc::new(PrepareContextHost(prepare)))
        .session_value(Arc::new(ContextModelHost(Some(Arc::new(context)))))
        .session_value(Arc::new(CompactionStrategyHost(Some(Arc::new(context)))))
        .require_feature(
            Feature::new("PrepareModelHost").session("PrepareModelHost", |names| {
                Ok(Box::new(DefaultPrepareModelPlugin::new(names)?))
            }),
        );
    defaults::install(&mut builder);
    builder
}

/// Edits the prepared request's timeline prefix: change its first message,
/// drop it, or append a message after the prefix.
#[derive(Clone, Copy)]
enum PrefixEdit {
    Change,
    Drop,
    Append,
}
struct RewritePrefix {
    edit: PrefixEdit,
    /// Edit only a request that already starts with the replacement a
    /// completed compaction stores.
    compacted: bool,
}
impl RewritePrefix {
    fn new(edit: PrefixEdit) -> Self {
        Self {
            edit,
            compacted: false,
        }
    }
    fn compacted(edit: PrefixEdit) -> Self {
        Self {
            edit,
            compacted: true,
        }
    }
}
impl PrepareContext for RewritePrefix {
    fn prepare(
        &self,
        request: Arc<ModelRequest>,
        _: ContextSource,
        _: CancellationToken,
    ) -> HostFuture<'_, Arc<ModelRequest>> {
        let (edit, compacted) = (self.edit, self.compacted);
        Box::pin(async move {
            let mut request = (*request).clone();
            if compacted && !starts_with_replacement(&request) {
                return Ok(Arc::new(request));
            }
            match edit {
                PrefixEdit::Change => {
                    assert!(
                        !request.messages.is_empty(),
                        "the prepared request has no prefix to change"
                    );
                    request.messages[0] = Arc::new(Message::text(Role::User, "changed"))
                }
                PrefixEdit::Drop => {
                    assert!(
                        !request.messages.is_empty(),
                        "the prepared request has no prefix to drop"
                    );
                    request.messages.remove(0);
                }
                PrefixEdit::Append => request
                    .messages
                    .push(Arc::new(Message::text(Role::User, "appended"))),
            }
            Ok(Arc::new(request))
        })
    }
}
/// Whether the request starts with a completed compaction's replacement.
fn starts_with_replacement(request: &ModelRequest) -> bool {
    matches!(
        request.messages.first().map(|message| &message.content),
        Some(MessageContent::Text(text)) if text.starts_with("<dim-summary>")
    )
}

fn external_history(events: &ContextHistory) -> std::result::Result<Vec<HistoryEntry>, String> {
    let mut history = project_context_history(events)?;
    for entry in &mut history {
        if let MessageContent::Text(text) = &mut Arc::make_mut(&mut entry.message).content {
            *text = format!("External: {text}");
        }
    }
    Ok(history)
}

#[tokio::test]
async fn external_pure_history_renderer_drives_the_final_request_and_context_budget() {
    let agent = MinimalHost::start(&builder_with_context(
        Arc::new(EchoModel),
        HistoryRenderer {
            render: HistoryRender::Custom(external_history),
            ..HistoryRenderer::default()
        },
        Context::default(),
    ))
    .await
    .unwrap();
    let session = agent
        .create_session(SessionConfig::default())
        .await
        .unwrap();
    session
        .submit("original".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    session.idle("main").await.unwrap();
    let events = session.history("main", None).await.unwrap();
    assert!(events.iter().any(
        |event| matches!(&event.kind, EventKind::InputMessage(data) if matches!(
            data.user.as_ref().map(|user| user.content.as_slice()),
            Some([ContentBlock::Text { text }]) if text == "original"
        ))
    ));
    assert!(events.iter().any(|event| matches!(&event.kind, EventKind::ModelCall(data) if data.request.messages == vec![Arc::new(Message::text(Role::User, "External: original"))])));
    assert!(
        events
            .iter()
            .any(|event| matches!(&event.kind, EventKind::ContextBudget(_)))
    );
    assert!(events.iter().any(|event| matches!(&event.kind, EventKind::Text(data) if data.delta == "Echo: External: original")));
    agent.close().await.unwrap();
}
/// The first call sends nothing until it is cancelled; later calls echo.
/// Every request is recorded.
#[derive(Default)]
struct SilentFirstCall {
    requests: std::sync::Mutex<Vec<Arc<ModelRequest>>>,
    entered: tokio::sync::Notify,
}
impl ModelDriver for SilentFirstCall {
    fn invoke(
        &self,
        request: Arc<ModelRequest>,
        connection: dim_agent_capabilities::host::ModelConnection,
        cancel: CancellationToken,
    ) -> HostFuture<'_, ModelStream> {
        let call = {
            let mut requests = self.requests.lock().unwrap();
            requests.push(request.clone());
            requests.len()
        };
        Box::pin(async move {
            if call > 1 {
                return EchoModel.invoke(request, connection, cancel).await;
            }
            self.entered.notify_one();
            cancel.cancelled().await;
            Err(HostError::Cancelled("cancelled by the provider".into()))
        })
    }
}

/// A steer applied before the model returned any output replaces the
/// request: its owner cancels it and writes a Cancelled terminal without
/// output, and the Loop then starts the next request of the same turn
/// carrying the steer. The history stays a valid branch boundary.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_steer_before_any_model_output_ends_the_request_and_continues_the_turn() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let model = Arc::new(SilentFirstCall::default());
        let agent = MinimalHost::start(&builder(model.clone())).await.unwrap();
        let session = agent
            .create_session(SessionConfig::default())
            .await
            .unwrap();
        session
            .submit("first".into(), InputMode::Queue, "main")
            .await
            .unwrap();
        model.entered.notified().await;
        let steer = session
            .submit("STEER-MARKER".into(), InputMode::Steer, "main")
            .await
            .unwrap();
        session.idle("main").await.unwrap();
        let requests = model.requests.lock().unwrap().clone();
        let carries = |request: &ModelRequest| {
            serde_json::to_string(&request.messages)
                .unwrap()
                .contains("STEER-MARKER")
        };
        assert_eq!(requests.len(), 2);
        assert!(!carries(&requests[0]));
        assert!(carries(&requests[1]));
        let events = session.history("main", None).await.unwrap();
        let position = |predicate: &dyn Fn(&EventKind) -> bool| {
            events
                .iter()
                .position(|event| predicate(&event.kind))
                .unwrap()
        };
        let starts: Vec<_> = events
            .iter()
            .filter_map(|event| match &event.kind {
                EventKind::ModelStart(data) => Some(data.request_id.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(starts.len(), 2);
        let replaced = &starts[0];
        let terminal = position(
            &|kind| matches!(kind, EventKind::ModelOutputEnd(data) if data.request_id == *replaced),
        );
        assert!(matches!(
            &events[terminal].kind,
            EventKind::ModelOutputEnd(ModelOutputEndData {
                outcome: ModelOutputOutcome::Cancelled { reason },
                ..
            }) if reason == dim_agent_capabilities::steer::REPLACED_BY_STEER
        ));
        assert!(!events.iter().any(|event| matches!(&event.kind,
            EventKind::Text(data) if data.request_id == *replaced)));
        let consumed = position(
            &|kind| matches!(kind, EventKind::InputConsumed(data) if data.input_id == steer.id),
        );
        let next = position(
            &|kind| matches!(kind, EventKind::ModelStart(data) if data.request_id == starts[1]),
        );
        assert!(terminal < consumed && consumed < next);
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(&event.kind, EventKind::TurnStart(_)))
                .count(),
            1
        );
        assert!(matches!(
            events.last().unwrap().kind,
            EventKind::TurnEnd(TurnEndData {
                outcome: TurnOutcome::Completed { .. },
                ..
            })
        ));
        assert!(
            requests[1]
                .messages
                .iter()
                .all(|message| message.role != Role::Assistant)
        );
        dim_agent_capabilities::history::validate_history_boundary(&events).unwrap();
        agent.close().await.unwrap();
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn provider_budget_recovery_uses_one_fixed_source_and_retries_only_once() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let calls = Arc::new(AtomicUsize::new(0));
        let agent = MinimalHost::start(&builder(Arc::new(BudgetFailure(calls.clone()))))
            .await
            .unwrap();
        let session = agent
            .create_session(SessionConfig::default())
            .await
            .unwrap();
        // The turn's input is larger than the summary wrapper plus the
        // pinned share, so the recovery compaction reduces the request.
        session
            .submit("x".repeat(4_000), InputMode::Queue, "main")
            .await
            .unwrap();
        session.idle("main").await.unwrap();
        let events = session.history("main", None).await.unwrap();
        let sources: Vec<_> = events
            .iter()
            .filter_map(|e| match &e.kind {
                EventKind::ContextPrepare(data) => Some(data),
                _ => None,
            })
            .collect();
        assert_eq!(sources.len(), 2);
        assert!(sources[0].recovery.is_none());
        assert!(sources[1].recovery.is_some());
        assert_eq!(sources[0].through, sources[1].through);
        assert_ne!(sources[0].request_id, sources[1].request_id);
        // The retry is prepared from the compacted context: the summary, then
        // the running turn's pinned input (ContextCompaction §5.9).
        // The retry is prepared from the compacted context: the summary, then
        // the running turn's pinned input (ContextCompaction §5.3, §5.9).
        let sent: Vec<_> = events
            .iter()
            .filter_map(|event| match &event.kind {
                EventKind::ModelCall(data) => Some(&data.request),
                _ => None,
            })
            .collect();
        assert_eq!(sent.len(), 2);
        assert_eq!(sent[1].messages.len(), 2, "{:?}", sent[1].messages);
        assert_eq!(
            *sent[1].messages[0],
            dim_agent_capabilities::history::summary_message("short")
        );
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e.kind, EventKind::ContextCompaction(_)))
                .count(),
            1
        );
        assert!(matches!(
            events.last().unwrap().kind,
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn foreign_compaction_cursor_fails_recovery_without_another_model_call() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let calls = Arc::new(AtomicUsize::new(0));
        let agent = MinimalHost::start(&builder_with_context(
            Arc::new(BudgetFailure(calls.clone())),
            HistoryRenderer::default(),
            Context { foreign_cursor: true, ..Context::default() },
        )).await.unwrap();
        let session = agent
            .create_session(SessionConfig::default())
            .await
            .unwrap();
        session
            .submit("x".repeat(500), InputMode::Queue, "main")
            .await
            .unwrap();
        session.idle("main").await.unwrap();
        let events = session.history("main", None).await.unwrap();
        let sources: Vec<_> = events
            .iter()
            .filter_map(|event| match &event.kind {
                EventKind::ContextPrepare(data) => Some(data),
                _ => None,
            })
            .collect();
        assert_eq!(sources.len(), 2);
        assert!(sources[1].recovery.is_some());
        assert_eq!(sources[0].through, sources[1].through);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(events.iter().filter(|event| matches!(event.kind, EventKind::ModelCall(_))).count(), 1);
        let compacted: Vec<_> = events
            .iter()
            .filter_map(|event| match &event.kind {
                EventKind::ContextCompaction(data) => Some(data),
                _ => None,
            })
            .collect();
        assert_eq!(compacted.len(), 1);
        assert_eq!(compacted[0].through, sources[1].through);
        assert!(matches!(
            &compacted[0].outcome,
            ContextCompactionResult::Failed { error, .. } if error.contains("cut is outside the fixed context history")
        ));
        assert!(events.iter().any(|event| matches!(
            &event.kind,
            EventKind::ModelOutputEnd(data)
                if data.request_id == sources[1].request_id
                && matches!(&data.outcome, ModelOutputOutcome::Failed { error, .. } if error.contains("cut is outside the fixed context history"))
        )));
        assert!(matches!(
            &events.last().unwrap().kind,
            EventKind::TurnEnd(TurnEndData { outcome: TurnOutcome::Failed { error }, .. })
                if error.contains("cut is outside the fixed context history")
        ));
        assert!(!session.state("main").unwrap().closed);
        agent.close().await.unwrap();
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn compressor_cursor_cannot_split_an_assistant_tool_use_from_its_result() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let agent = MinimalHost::start(&builder_with_context(
            Arc::new(ToolThenBudgetFailure(AtomicUsize::new(0))),
            HistoryRenderer::default(),
            Context {
                foreign_cursor: true,
                threshold: Some(10_000.),
                ..Context::default()
            },
        ))
        .await
        .unwrap();
        let session = agent
            .create_session(SessionConfig::default())
            .await
            .unwrap();
        session
            .submit("use the tool".into(), InputMode::Queue, "main")
            .await
            .unwrap();
        session.idle("main").await.unwrap();
        let events = session.history("main", None).await.unwrap();
        assert!(events.iter().any(|event| matches!(
            &event.kind,
            EventKind::ContextCompaction(ContextCompactionData {
                outcome: ContextCompactionResult::Failed { error, .. },
                ..
            }) if error == "Compaction cut separates a tool call from its results or context."
        )));
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event.kind, EventKind::ModelCall(_)))
                .count(),
            2
        );
        assert!(matches!(
            &events.last().unwrap().kind,
            EventKind::TurnEnd(TurnEndData { outcome: TurnOutcome::Failed { error }, .. })
                if error == "Compaction cut separates a tool call from its results or context."
        ));
        agent.close().await.unwrap();
    })
    .await
    .unwrap();
}

struct RewriteBaseline {
    timeline: Key<Timeline>,
}
impl RewriteBaseline {
    fn new(names: &Names<'_>) -> rutis::Result<Self> {
        Ok(Self {
            timeline: names.key()?,
        })
    }
}
impl DynPlugin for RewriteBaseline {
    fn name(&self) -> &str {
        "test.rewrite-baseline"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.timeline.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let timeline = ctx.require_at(&self.timeline)?;
            timeline.before(ctx, self.name(), &["context/compaction"], |mut draft| {
                if let EventKind::ContextCompaction(ContextCompactionData {
                    outcome:
                        ContextCompactionResult::Completed {
                            context_baseline, ..
                        },
                    ..
                }) = &mut draft.kind
                {
                    for entry in context_baseline {
                        entry.message =
                            Arc::new(Message::text(Role::User, "host-selected baseline"));
                    }
                }
                Ok(draft)
            })?;
            Ok(Effect::Done)
        })
    }
}

#[tokio::test]
async fn compaction_restores_counts_and_uses_the_committed_context_baseline() {
    let context_input = || InputMessageData {
        user: InputMessageData::text("x".repeat(1_200), None).user,
        blocks: vec![dim_agent_capabilities::context_block::raw(
            "<dim-state source=\"host/selection\">selected text</dim-state>".into(),
            Some("selection".into()),
        )],
        mode: Some(InputMode::Queue),
        ..InputMessageData::default()
    };
    let context = Context {
        threshold: Some(450.),
        ..Context::default()
    };

    let agent = MinimalHost::start(&builder_with_context(
        Arc::new(EchoModel),
        HistoryRenderer::default(),
        context,
    ))
    .await
    .unwrap();
    let session = agent
        .create_session(SessionConfig::default())
        .await
        .unwrap();
    session.input(context_input(), "main").await.unwrap();
    session.idle("main").await.unwrap();
    for text in ["next", "third"] {
        session
            .submit(text.into(), InputMode::Queue, "main")
            .await
            .unwrap();
        session.idle("main").await.unwrap();
    }
    let events = session.history("main", None).await.unwrap();
    let baseline = events
        .iter()
        .find_map(|event| match &event.kind {
            EventKind::ContextCompaction(ContextCompactionData {
                outcome:
                    ContextCompactionResult::Completed {
                        context_baseline, ..
                    },
                ..
            }) => Some(context_baseline),
            _ => None,
        })
        .unwrap();
    assert_eq!(baseline.len(), 1);
    let requests: Vec<_> = events
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::ModelCall(data) => Some(&data.request),
            _ => None,
        })
        .collect();
    assert_eq!(requests.len(), 3);
    assert!(requests.iter().all(|request| {
        request
            .messages
            .iter()
            .any(|message| message.as_ref() == baseline[0].message.as_ref())
    }));
    let compacted = events
        .iter()
        .find_map(|event| match &event.kind {
            EventKind::ContextCompaction(ContextCompactionData {
                request_id: Some(request_id),
                outcome: ContextCompactionResult::Completed { .. },
                ..
            }) => Some(request_id.clone()),
            _ => None,
        })
        .unwrap();
    let budget = events
        .iter()
        .find_map(|event| match &event.kind {
            EventKind::ContextBudget(data) if data.request_id == compacted => Some(data),
            _ => None,
        })
        .unwrap();
    assert_eq!(budget.baseline_source, BaselineSource::FullEstimate);
    assert_eq!(
        budget.estimated_input_tokens, 1321.,
        "budget is measured before the hook rewrites the committed baseline"
    );
    agent.close().await.unwrap();

    let mut forged_builder =
        builder_with_context(Arc::new(EchoModel), HistoryRenderer::default(), context);
    forged_builder.require_feature(
        Feature::new("test.rewrite-baseline").branch("test.rewrite-baseline", |names| {
            Ok(Box::new(RewriteBaseline::new(names)?))
        }),
    );
    let forged_agent = MinimalHost::start(&forged_builder).await.unwrap();
    let forged_session = forged_agent
        .create_session(SessionConfig::default())
        .await
        .unwrap();
    forged_session.input(context_input(), "main").await.unwrap();
    forged_session.idle("main").await.unwrap();
    for text in ["next", "third"] {
        forged_session
            .submit(text.into(), InputMode::Queue, "main")
            .await
            .unwrap();
        forged_session.idle("main").await.unwrap();
    }
    let forged_events = forged_session.history("main", None).await.unwrap();
    let rewritten_requests: Vec<_> = forged_events
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::ModelCall(data) => Some(&data.request),
            _ => None,
        })
        .collect();
    assert_eq!(rewritten_requests.len(), 3);
    assert!(
        rewritten_requests
            .last()
            .unwrap()
            .messages
            .contains(&Arc::new(Message::text(
                Role::User,
                "host-selected baseline",
            )))
    );
    assert!(matches!(
        &forged_events.last().unwrap().kind,
        EventKind::TurnEnd(TurnEndData {
            outcome: TurnOutcome::Completed { .. },
            ..
        })
    ));
    forged_agent.close().await.unwrap();
}

/// The definition of `name`, a tool whose call carries the effects its input
/// names (ContextCompaction §5.10).
fn declare_definition(name: &str) -> ToolDefinition {
    ToolDefinition {
        risk: Some("session".into()),
        ..dim_agent_capabilities::tool_registry::definition(
            name,
            "Declare the effects its input names.",
            serde_json::json!({"type":"object","properties":{"effects":{"type":"array"}}}),
        )
    }
}

/// A `declare` call's result: `text` carrying the effects its input names.
fn declared(
    input: &JsonObject,
    text: impl Into<String>,
) -> std::result::Result<ToolResult, HostError> {
    let effects: Vec<ToolEffect> = serde_json::from_value(
        input
            .get("effects")
            .cloned()
            .unwrap_or_else(|| serde_json::json!([])),
    )
    .map_err(|_| HostError::failed("Invalid effects"))?;
    Ok(ToolResult {
        effects: Some(effects),
        ..ToolResult::text(text)
    })
}

#[path = "context_flow/checkpoint_projection.rs"]
mod checkpoint_projection;
#[path = "context_flow/compacted_bindings.rs"]
mod compacted_bindings;
#[path = "context_flow/compaction_events.rs"]
mod compaction_events;
#[path = "context_flow/context_reads.rs"]
mod context_reads;
#[path = "context_flow/digest.rs"]
mod digest;
#[path = "context_flow/manual_compaction.rs"]
mod manual_compaction;
#[path = "context_flow/pinning.rs"]
mod pinning;
#[path = "context_flow/product_summary.rs"]
mod product_summary;
#[path = "context_flow/replan.rs"]
mod replan;
#[path = "context_flow/retention.rs"]
mod retention;

struct ExpandFinal {
    timeline: Key<Timeline>,
}
impl ExpandFinal {
    fn new(names: &Names<'_>) -> rutis::Result<Self> {
        Ok(Self {
            timeline: names.key()?,
        })
    }
}
impl DynPlugin for ExpandFinal {
    fn name(&self) -> &str {
        "ExpandFinal"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.timeline.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let timeline = ctx.require_at(&self.timeline)?;
            timeline.before(ctx, "expand-final", &["model/call"], |mut draft| {
                if let EventKind::ModelCall(call) = &mut draft.kind {
                    Arc::make_mut(&mut call.request)
                        .messages
                        .push(Arc::new(Message::text(Role::User, "x".repeat(5000))));
                }
                Ok(draft)
            })?;
            Ok(Effect::Done)
        })
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn before_modified_request_is_budget_checked_before_native_model_invocation() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let calls = Arc::new(AtomicUsize::new(0));
        let mut builder = builder(Arc::new(BudgetFailure(calls.clone())));
        builder.require_feature(Feature::new("ExpandFinal").branch("ExpandFinal", |names| {
            Ok(Box::new(ExpandFinal::new(names)?))
        }));
        let agent = MinimalHost::start(&builder).await.unwrap();
        let session = agent
            .create_session(SessionConfig::default())
            .await
            .unwrap();
        session
            .submit("x".repeat(500), InputMode::Queue, "main")
            .await
            .unwrap();
        session.idle("main").await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            session
                .history("main", None)
                .await
                .unwrap()
                .iter()
                .filter(|e| matches!(e.kind, EventKind::ModelOutputEnd(_)))
                .count(),
            2
        );
        agent.close().await.unwrap();
    })
    .await
    .unwrap();
}

struct UsageModel;
impl ModelDriver for UsageModel {
    fn invoke(
        &self,
        request: Arc<ModelRequest>,
        _: dim_agent_capabilities::host::ModelConnection,
        cancel: CancellationToken,
    ) -> HostFuture<'_, ModelStream> {
        Box::pin(async move {
            let stream = EchoModel
                .invoke(
                    request,
                    dim_agent_capabilities::host::ModelConnection::Local,
                    cancel,
                )
                .await?;
            let usage = Usage {
                prompt_tokens: 123.,
                completion_tokens: 2.,
                total_tokens: 125.,
                cache_read_tokens: Some(0.),
                cache_write_tokens: Some(0.),
                cost: None,
                currency: None,
            };
            Ok(
                Box::pin(stream.chain(futures_util::stream::once(async move {
                    Ok(ModelChunk::Usage(usage))
                }))) as ModelStream,
            )
        })
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn provider_usage_baseline_counts_only_new_history_after_a_matching_request() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let agent = MinimalHost::start(&builder(Arc::new(UsageModel)))
            .await
            .unwrap();
        let session = agent
            .create_session(SessionConfig::default())
            .await
            .unwrap();
        session
            .submit("first".into(), InputMode::Queue, "main")
            .await
            .unwrap();
        session.idle("main").await.unwrap();
        session
            .submit("second".into(), InputMode::Queue, "main")
            .await
            .unwrap();
        session.idle("main").await.unwrap();
        let events = session.history("main", None).await.unwrap();
        let budgets: Vec<_> = events
            .iter()
            .filter_map(|e| match &e.kind {
                EventKind::ContextBudget(d) => Some(d),
                _ => None,
            })
            .collect();
        assert_eq!(budgets.len(), 2);
        assert_eq!(budgets[0].baseline_source, BaselineSource::FullEstimate);
        assert_eq!(budgets[1].baseline_source, BaselineSource::ProviderBaseline);
        assert_eq!(budgets[1].baseline_prompt_tokens, Some(123.));
        assert_eq!(
            budgets[1].estimated_input_tokens,
            123. + budgets[1].delta_tokens.unwrap()
        );
        agent.close().await.unwrap();
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn restoring_each_preparation_handoff_settles_without_replaying_work() {
    for boundary in [
        "input-consumed",
        "model-start",
        "context-prepare",
        "model-call",
        "model-output-end",
    ] {
        let store = Arc::new(MemoryTimelineStore::default());
        store
            .create_session("restore", &SessionConfig::default())
            .await
            .unwrap();
        let timeline = Timeline::new(
            rutis::Ctx::root().unwrap(),
            TimelineKey {
                session_id: "restore".into(),
                branch_id: "main".into(),
            },
            Vec::new(),
            store.clone(),
        );
        let input = timeline
            .append(
                EventKind::InputMessage(InputMessageData::text("restore", Some(InputMode::Queue)))
                    .into(),
            )
            .unwrap();
        timeline
            .append(
                EventKind::InputApplied(InputAppliedData {
                    input_id: input.id.clone(),
                    mode: Some(InputMode::Queue),
                })
                .into(),
            )
            .unwrap();
        timeline
            .append(
                EventKind::TurnStart(TurnStartData {
                    turn_id: "turn".into(),
                })
                .into(),
            )
            .unwrap();
        timeline
            .append(
                EventKind::InputConsumed(InputConsumedData {
                    input_id: input.id.clone(),
                })
                .into(),
            )
            .unwrap();
        if boundary != "input-consumed" {
            timeline
                .append(
                    EventKind::ModelStart(ModelStartData {
                        turn_id: "turn".into(),
                        request_id: "request".into(),
                    })
                    .into(),
                )
                .unwrap();
        }
        let request = Arc::new(ModelRequest::new("mock", "echo", vec![]));
        if matches!(
            boundary,
            "context-prepare" | "model-call" | "model-output-end"
        ) {
            timeline
                .append(
                    EventKind::ContextPrepare(ContextPrepareData {
                        turn_id: "turn".into(),
                        request_id: "request".into(),
                        through: timeline.sequence(),
                        request: request.clone(),
                        configuration: None,
                        prompt: None,
                        recovery: None,
                    })
                    .into(),
                )
                .unwrap();
        }
        if matches!(boundary, "model-call" | "model-output-end") {
            timeline
                .append(
                    EventKind::ModelCall(ModelCallData {
                        turn_id: "turn".into(),
                        request_id: "request".into(),
                        request,
                        configuration: None,
                        prompt: None,
                    })
                    .into(),
                )
                .unwrap();
        }
        if boundary == "model-output-end" {
            timeline
                .append(
                    EventKind::ModelOutputEnd(ModelOutputEndData {
                        turn_id: "turn".into(),
                        request_id: "request".into(),
                        outcome: ModelOutputOutcome::Completed {
                            finish_reason: FinishReason::EndTurn,
                        },
                    })
                    .into(),
                )
                .unwrap();
        }
        let persisted = timeline.sequence();
        let mut builder = builder_with_context(
            Arc::new(EchoModel),
            HistoryRenderer::default(),
            Context::default(),
        );
        builder.timeline_store(store);
        let agent = MinimalHost::start(&builder).await.unwrap();
        let session = agent.restore_session("restore".into()).await.unwrap();
        tokio::time::timeout(Duration::from_secs(3), session.idle("main"))
            .await
            .unwrap_or_else(|_| panic!("recovery stuck at {boundary}"))
            .unwrap();
        let events = session.history("main", None).await.unwrap();
        assert!(events.iter().any(|event| matches!(&event.kind, EventKind::TurnEnd(data) if data.outcome == TurnOutcome::Failed { error: CAPABILITY_RELOADING.into() })));
        assert!(
            !events.iter().any(|event| event.sequence > persisted
                && matches!(
                    &event.kind,
                    EventKind::ContextPrepare(_) | EventKind::ModelCall(_) | EventKind::Text(_)
                )),
            "recovery must settle, not rerun preparation or model work"
        );
        if boundary != "input-consumed" && boundary != "model-output-end" {
            assert!(events.iter().any(|event| matches!(&event.kind, EventKind::ModelOutputEnd(data) if matches!(&data.outcome, ModelOutputOutcome::Failed { error, .. } if error == CAPABILITY_RELOADING))));
        }
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event.kind, EventKind::ModelOutputEnd(_)))
                .count(),
            usize::from(boundary != "input-consumed")
        );
        assert_eq!(
            session
                .get::<LoopSnapshot>("main")
                .unwrap()
                .unwrap()
                .turn_id,
            None
        );
        tokio::time::timeout(
            Duration::from_secs(3),
            session.stop("main", "after recovery".into()),
        )
        .await
        .unwrap()
        .unwrap();
        session
            .submit("next input".into(), InputMode::Queue, "main")
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(3), session.idle("main"))
            .await
            .unwrap()
            .unwrap();
        assert!(session.history("main", None).await.unwrap().iter().any(|event| matches!(&event.kind, EventKind::Text(data) if data.delta.contains("next input"))));
        tokio::time::timeout(Duration::from_secs(3), agent.close())
            .await
            .unwrap()
            .unwrap();
    }
}

#[tokio::test]
async fn restoring_idle_unpaused_queue_input_applies_it_without_a_new_submission() {
    let store = Arc::new(MemoryTimelineStore::default());
    store
        .create_session("queued-restore", &SessionConfig::default())
        .await
        .unwrap();
    let timeline = Timeline::new(
        rutis::Ctx::root().unwrap(),
        TimelineKey {
            session_id: "queued-restore".into(),
            branch_id: "main".into(),
        },
        Vec::new(),
        store.clone(),
    );
    let input = timeline
        .append(
            EventKind::InputMessage(InputMessageData::text(
                "queued before restore",
                Some(InputMode::Queue),
            ))
            .into(),
        )
        .unwrap();
    dim_agent_runtime::core::close_timeline(&timeline)
        .await
        .unwrap();
    let mut builder = builder_with_context(
        Arc::new(EchoModel),
        HistoryRenderer::default(),
        Context::default(),
    );
    builder.timeline_store(store);
    let agent = MinimalHost::start(&builder).await.unwrap();
    let session = agent
        .restore_session("queued-restore".into())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), session.idle("main"))
        .await
        .unwrap()
        .unwrap();
    let events = session.history("main", None).await.unwrap();
    assert!(events.iter().any(|event| matches!(&event.kind,
        EventKind::InputApplied(data) if data.input_id == input.id)));
    assert!(events.iter().any(|event| matches!(&event.kind,
        EventKind::Text(data) if data.delta.contains("queued before restore"))));
    assert!(events.iter().any(|event| matches!(&event.kind,
        EventKind::TurnEnd(data) if matches!(data.outcome, TurnOutcome::Completed { .. }))));
    agent.close().await.unwrap();
}

#[tokio::test]
async fn restoring_pending_inputs_runs_each_queued_input_in_its_own_turn() {
    let store = Arc::new(MemoryTimelineStore::default());
    store
        .create_session("applied-restore", &SessionConfig::default())
        .await
        .unwrap();
    let timeline = Timeline::new(
        rutis::Ctx::root().unwrap(),
        TimelineKey {
            session_id: "applied-restore".into(),
            branch_id: "main".into(),
        },
        Vec::new(),
        store.clone(),
    );
    let first = timeline
        .append(
            EventKind::InputMessage(InputMessageData::text("first", Some(InputMode::Queue))).into(),
        )
        .unwrap();
    timeline
        .append(
            EventKind::InputApplied(InputAppliedData {
                input_id: first.id.clone(),
                mode: Some(InputMode::Queue),
            })
            .into(),
        )
        .unwrap();
    let second = timeline
        .append(
            EventKind::InputMessage(InputMessageData::text("second", Some(InputMode::Queue)))
                .into(),
        )
        .unwrap();
    dim_agent_runtime::core::close_timeline(&timeline)
        .await
        .unwrap();
    let mut builder = builder_with_context(
        Arc::new(EchoModel),
        HistoryRenderer::default(),
        Context::default(),
    );
    builder.timeline_store(store);
    let agent = MinimalHost::start(&builder).await.unwrap();
    let session = agent
        .restore_session("applied-restore".into())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), session.idle("main"))
        .await
        .unwrap()
        .unwrap();
    let events = session.history("main", None).await.unwrap();
    let consumed = |id: &str| {
        events
            .iter()
            .position(|event| {
                matches!(&event.kind, EventKind::InputConsumed(data) if data.input_id == id)
            })
            .unwrap()
    };
    let first_end = events
        .iter()
        .position(|event| {
            matches!(&event.kind, EventKind::TurnEnd(data)
            if matches!(&data.outcome, TurnOutcome::Completed { .. }))
        })
        .unwrap();
    assert!(consumed(&first.id) < first_end);
    assert!(first_end < consumed(&second.id));
    assert!(events.iter().any(|event| matches!(&event.kind,
        EventKind::Text(data) if data.delta.contains("second"))));
    agent.close().await.unwrap();
}

#[derive(Default)]
struct CompactionGate {
    armed: std::sync::atomic::AtomicBool,
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
}
/// `Context` whose compaction, once armed, waits for a release and then
/// completes regardless of cancellation, like a host call that finished.
#[derive(Clone)]
struct GatedContext(Context, Arc<CompactionGate>);
impl ResolveContextModel for GatedContext {
    fn resolve(
        &self,
        _: Arc<ModelRequest>,
        _: CancellationToken,
    ) -> HostFuture<'_, Arc<dyn ContextModel>> {
        let model = self.clone();
        Box::pin(async move { Ok(Arc::new(model) as Arc<dyn ContextModel>) })
    }
}
impl ContextModel for GatedContext {
    fn context_window(&self) -> f64 {
        self.0.context_window()
    }
    fn output_tokens(&self) -> f64 {
        self.0.output_tokens()
    }
    fn threshold_tokens(&self) -> f64 {
        self.0.threshold_tokens()
    }
    fn count_tokens(&self, request: &ModelRequest) -> f64 {
        self.0.count_tokens(request)
    }
}
impl CompactionStrategy for GatedContext {
    fn name(&self) -> &str {
        "test"
    }
    fn replacement_size(&self, model: &dyn ContextModel) -> f64 {
        self.0.replacement_size(model)
    }
    fn compact(
        &self,
        input: CompactionInput,
        cancel: CancellationToken,
    ) -> HostFuture<'_, StrategyOutcome> {
        Box::pin(async move {
            if self.1.armed.swap(false, Ordering::SeqCst) {
                self.1.entered.notify_one();
                self.1.release.notified().await;
            }
            self.0.compact(input, cancel).await
        })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_compaction_completed_after_a_stop_is_committed_not_discarded() {
    let gate = Arc::new(CompactionGate::default());
    let context = GatedContext(
        Context {
            threshold: Some(450.),
            ..Context::default()
        },
        gate.clone(),
    );
    let mut builder = AgentBuilder::new(AgentConfig::default());
    builder
        .process_value(Arc::new(ModelHost(Arc::new(EchoModel))))
        .session_value(Arc::new(ToolHost(Arc::new(MockTools))))
        .session_value(Arc::new(HistoryRenderer::default()))
        .session_value(Arc::new(PrepareContextHost(Arc::new(IdentityContext))))
        .session_value(Arc::new(ContextModelHost(Some(Arc::new(context.clone())))))
        .session_value(Arc::new(CompactionStrategyHost(Some(Arc::new(context)))))
        .require_feature(
            Feature::new("PrepareModelHost").session("PrepareModelHost", |names| {
                Ok(Box::new(DefaultPrepareModelPlugin::new(names)?))
            }),
        );
    defaults::install(&mut builder);
    let agent = MinimalHost::start(&builder).await.unwrap();
    let session = agent
        .create_session(SessionConfig::default())
        .await
        .unwrap();
    for text in ["x".repeat(500), "y".repeat(500)] {
        session
            .submit(text, InputMode::Queue, "main")
            .await
            .unwrap();
        session.idle("main").await.unwrap();
    }
    gate.armed.store(true, Ordering::SeqCst);
    // Enough new history to regrow the 0.25 × B the automatic trigger needs
    // after the turn's compaction (ContextCompaction §5.6).
    session
        .submit("z".repeat(800), InputMode::Queue, "main")
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), gate.entered.notified())
        .await
        .expect("the next request compacts");
    let stopping = session.clone();
    let stop = tokio::spawn(async move { stopping.stop("main", "user stop".into()).await });
    let mut dispatch = session.dispatch_watch("main").unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let interrupt = session
                .history("main", None)
                .await
                .unwrap()
                .iter()
                .find(|event| matches!(event.kind, EventKind::StopInterrupt(_)))
                .map(|event| event.sequence);
            if interrupt.is_some_and(|sequence| dispatch.borrow().sequence >= sequence) {
                break;
            }
            tokio::select! {
                _ = dispatch.changed() => {}
                _ = tokio::time::sleep(Duration::from_millis(10)) => {}
            }
        }
    })
    .await
    .unwrap();
    gate.release.notify_one();
    tokio::time::timeout(Duration::from_secs(3), stop)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    session.idle("main").await.unwrap();
    let events = session.history("main", None).await.unwrap();
    let interrupt = events
        .iter()
        .position(|event| matches!(event.kind, EventKind::StopInterrupt(_)))
        .unwrap();
    assert!(
        events[interrupt..].iter().any(|event| matches!(
            &event.kind,
            EventKind::ContextCompaction(data)
                if matches!(data.outcome, ContextCompactionResult::Completed { .. })
        )),
        "the completed compaction is kept"
    );
    assert!(
        !events[interrupt..]
            .iter()
            .any(|event| matches!(event.kind, EventKind::ModelCall(_))),
        "the stopped request still does not reach the model"
    );
    agent.close().await.unwrap();
}

#[tokio::test]
async fn a_single_long_turn_compacts_inside_itself_instead_of_skipping() {
    let context = Context {
        threshold: Some(200.),
        ..Context::default()
    };
    let agent = MinimalHost::start(&builder_with_context(
        Arc::new(EchoModel),
        HistoryRenderer::default(),
        context,
    ))
    .await
    .unwrap();
    let session = agent
        .create_session(SessionConfig {
            tools: Some(vec!["echo".into()]),
            permission_policy: dim_agent_runtime::PermissionPolicy {
                preset: dim_agent_runtime::PermissionPreset::FullAccess,
                ..Default::default()
            },
            ..Default::default()
        })
        .await
        .unwrap();
    let command = format!("tool:echo {{\"text\":\"{}\"}}", "y".repeat(600));
    session
        .submit(command, InputMode::Queue, "main")
        .await
        .unwrap();
    session.idle("main").await.unwrap();
    let events = session.history("main", None).await.unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event.kind, EventKind::TurnStart(_)))
            .count(),
        1
    );
    let completed: Vec<_> = events
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::ContextCompaction(ContextCompactionData {
                outcome: ContextCompactionResult::Completed { retain_from, .. },
                ..
            }) => Some(retain_from.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(completed.len(), 1, "the long turn compacted inside itself");
    let last_call = events
        .iter()
        .rev()
        .find_map(|event| match &event.kind {
            EventKind::ModelCall(data) => Some(data.request.clone()),
            _ => None,
        })
        .unwrap();
    assert!(last_call.messages.len() >= 2);
    agent.close().await.unwrap();
}

/// A request whose prefix differs from the branch's projection is refused
/// before the model sees it.
#[tokio::test]
async fn a_changed_prefix_message_fails_the_preparation() {
    let agent = MinimalHost::start(&builder_with_prepare(
        Arc::new(EchoModel),
        HistoryRenderer::default(),
        Context::default(),
        Arc::new(RewritePrefix::new(PrefixEdit::Change)),
    ))
    .await
    .unwrap();
    let session = agent
        .create_session(SessionConfig::default())
        .await
        .unwrap();
    session
        .submit("original".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    session.idle("main").await.unwrap();
    let events = session.history("main", None).await.unwrap();
    assert!(
        !events
            .iter()
            .any(|event| matches!(event.kind, EventKind::ModelCall(_)))
    );
    assert!(events.iter().any(|event| matches!(
        &event.kind,
        EventKind::ModelOutputEnd(data)
            if matches!(&data.outcome, ModelOutputOutcome::Failed { error, .. }
                if error.contains("must preserve the timeline history prefix"))
    )));
    assert!(matches!(
        events.last().unwrap().kind,
        EventKind::TurnEnd(TurnEndData {
            outcome: TurnOutcome::Failed { .. },
            ..
        })
    ));
    agent.close().await.unwrap();
}

/// A request shorter than the branch's projection is refused the same way.
#[tokio::test]
async fn a_short_request_fails_the_preparation() {
    let agent = MinimalHost::start(&builder_with_prepare(
        Arc::new(EchoModel),
        HistoryRenderer::default(),
        Context::default(),
        Arc::new(RewritePrefix::new(PrefixEdit::Drop)),
    ))
    .await
    .unwrap();
    let session = agent
        .create_session(SessionConfig::default())
        .await
        .unwrap();
    session
        .submit("original".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    session.idle("main").await.unwrap();
    let events = session.history("main", None).await.unwrap();
    assert!(
        !events
            .iter()
            .any(|event| matches!(event.kind, EventKind::ModelCall(_)))
    );
    assert!(events.iter().any(|event| matches!(
        &event.kind,
        EventKind::ModelOutputEnd(data)
            if matches!(&data.outcome, ModelOutputOutcome::Failed { error, .. }
                if error.contains("must preserve the timeline history prefix"))
    )));
    agent.close().await.unwrap();
}

/// The same prefix with a message appended after it is what a renderer may
/// add, so the turn reaches the model with both messages.
#[tokio::test]
async fn a_message_appended_after_the_prefix_reaches_the_model() {
    let agent = MinimalHost::start(&builder_with_prepare(
        Arc::new(EchoModel),
        HistoryRenderer::default(),
        Context::default(),
        Arc::new(RewritePrefix::new(PrefixEdit::Append)),
    ))
    .await
    .unwrap();
    let session = agent
        .create_session(SessionConfig::default())
        .await
        .unwrap();
    session
        .submit("original".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    session.idle("main").await.unwrap();
    let events = session.history("main", None).await.unwrap();
    let sent = events
        .iter()
        .find_map(|event| match &event.kind {
            EventKind::ModelCall(data) => Some(data.request.clone()),
            _ => None,
        })
        .expect("the turn reaches the model");
    assert_eq!(
        sent.messages,
        vec![
            Arc::new(Message::text(Role::User, "original")),
            Arc::new(Message::text(Role::User, "appended")),
        ]
    );
    assert!(events.iter().any(
        |event| matches!(&event.kind, EventKind::Text(data) if data.delta == "Echo: appended")
    ));
    agent.close().await.unwrap();
}

/// A completed compaction's replacement is part of the prefix the check
/// compares: a tail appended after it reaches the model, and a changed
/// replacement is refused before the model.
#[tokio::test]
async fn a_completed_compaction_keeps_its_replacement_in_the_prefix_check() {
    let compacting = || Context {
        threshold: Some(450.),
        ..Context::default()
    };
    let long = "x".repeat(1_200);
    let summary = summary_message("short");

    let agent = MinimalHost::start(&builder_with_prepare(
        Arc::new(EchoModel),
        HistoryRenderer::default(),
        compacting(),
        Arc::new(RewritePrefix::compacted(PrefixEdit::Append)),
    ))
    .await
    .unwrap();
    let session = agent
        .create_session(SessionConfig::default())
        .await
        .unwrap();
    session
        .submit(long.clone(), InputMode::Queue, "main")
        .await
        .unwrap();
    session.idle("main").await.unwrap();
    session
        .submit("after".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    session.idle("main").await.unwrap();
    let events = session.history("main", None).await.unwrap();
    assert!(
        events.iter().any(|event| matches!(
            &event.kind,
            EventKind::ContextCompaction(ContextCompactionData {
                outcome: ContextCompactionResult::Completed { replacement, .. },
                ..
            }) if replacement.first() == Some(&summary)
        )),
        "the first turn completes a compaction whose replacement starts with the summary"
    );
    let calls: Vec<_> = events
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::ModelCall(data) => Some(data.request.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(calls.len(), 2, "both turns reach the model");
    let sent = calls.last().unwrap();
    assert_eq!(
        *sent.messages[0], summary,
        "the checked prefix starts with the replacement"
    );
    assert_eq!(
        sent.messages.last().unwrap().as_ref(),
        &Message::text(Role::User, "appended")
    );
    assert!(events.iter().any(
        |event| matches!(&event.kind, EventKind::Text(data) if data.delta == "Echo: appended")
    ));
    agent.close().await.unwrap();

    let agent = MinimalHost::start(&builder_with_prepare(
        Arc::new(EchoModel),
        HistoryRenderer::default(),
        compacting(),
        Arc::new(RewritePrefix::compacted(PrefixEdit::Change)),
    ))
    .await
    .unwrap();
    let session = agent
        .create_session(SessionConfig::default())
        .await
        .unwrap();
    session
        .submit(long, InputMode::Queue, "main")
        .await
        .unwrap();
    session.idle("main").await.unwrap();
    session
        .submit("after".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    session.idle("main").await.unwrap();
    let events = session.history("main", None).await.unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event.kind, EventKind::ModelCall(_)))
            .count(),
        1,
        "the changed replacement never reaches the model"
    );
    assert!(events.iter().any(|event| matches!(
        &event.kind,
        EventKind::ModelOutputEnd(data)
            if matches!(&data.outcome, ModelOutputOutcome::Failed { error, .. }
                if error.contains("must preserve the timeline history prefix"))
    )));
    assert!(matches!(
        events.last().unwrap().kind,
        EventKind::TurnEnd(TurnEndData {
            outcome: TurnOutcome::Failed { .. },
            ..
        })
    ));
    agent.close().await.unwrap();
}

//! A compaction whose summary overshoots its planned size is re-planned once.
//! The result after it is at most `0.5 × B`, or the record carries the mark
//! naming the parts that cover the excess (ContextCompaction §5.3,
//! acceptance 9).
use super::*;
use dim_agent_capabilities::history::summary_message;
use std::sync::atomic::{AtomicUsize, Ordering};

/// A strategy returning its summaries in order and recording the cut it took
/// each time: a re-plan shows up as a second call inside the same compaction.
struct Overshoot {
    planned: f64,
    summaries: Vec<&'static str>,
    /// Whether a pass continuing an earlier one reports that it has nothing
    /// to add, so the first result stands.
    skip_continuation: bool,
    next: AtomicUsize,
    cuts: std::sync::Mutex<Vec<String>>,
    diagnostics: StrategyDiagnostics,
}
impl Overshoot {
    fn new(planned: f64, summaries: Vec<&'static str>) -> Arc<Self> {
        Self::build(planned, summaries, false)
    }
    fn skipping(planned: f64, summaries: Vec<&'static str>) -> Arc<Self> {
        Self::build(planned, summaries, true)
    }
    fn build(planned: f64, summaries: Vec<&'static str>, skip_continuation: bool) -> Arc<Self> {
        Arc::new(Self {
            planned,
            summaries,
            skip_continuation,
            next: AtomicUsize::new(0),
            cuts: Default::default(),
            diagnostics: StrategyDiagnostics {
                model_call_count: 1,
                model_usage: None,
                record: CompactionStrategyRecord {
                    kind: "test".into(),
                    details: JsonObject::new(),
                },
            },
        })
    }
    fn cuts(&self) -> Vec<String> {
        self.cuts.lock().unwrap().clone()
    }
}
impl CompactionStrategy for Overshoot {
    fn name(&self) -> &str {
        "test"
    }
    fn replacement_size(&self, _: &dyn ContextModel) -> f64 {
        self.planned
    }
    fn compact(
        &self,
        input: CompactionInput,
        _: CancellationToken,
    ) -> HostFuture<'_, StrategyOutcome> {
        Box::pin(async move {
            if self.skip_continuation && input.prior.is_some() {
                return Ok(StrategyOutcome::Skipped {
                    reason: "continuation_not_needed".into(),
                    diagnostics: self.diagnostics.clone(),
                });
            }
            let Some(cut) = input.cut().cloned() else {
                return Ok(StrategyOutcome::Skipped {
                    reason: "nothing_to_compact".into(),
                    diagnostics: self.diagnostics.clone(),
                });
            };
            self.cuts.lock().unwrap().push(cut.retain_from.clone());
            let next = self.next.fetch_add(1, Ordering::SeqCst);
            Ok(StrategyOutcome::Completed {
                retain_from: cut.retain_from,
                replacement: vec![summary_message(
                    self.summaries.get(next).copied().unwrap_or("short"),
                )],
                diagnostics: self.diagnostics.clone(),
            })
        })
    }
}

/// Window 10,240 and output 4,096: `B = 6,144` and the bound 3,072, with a
/// threshold the automatic trigger never reaches.
fn facts() -> Context {
    Context {
        window: 10_240.,
        output: 4_096.,
        threshold: Some(50_000.),
        ..Context::default()
    }
}

/// The session these tests compact, with `strategy` in place of the summary
/// strategy and a model selection the Loop's manual step needs.
async fn replanned_session(
    driver: Arc<dyn ModelDriver>,
    strategy: Arc<Overshoot>,
) -> (MinimalHost, dim_agent_runtime::Session) {
    replanned_session_with(facts(), driver, strategy).await
}

async fn replanned_session_with(
    facts: Context,
    driver: Arc<dyn ModelDriver>,
    strategy: Arc<Overshoot>,
) -> (MinimalHost, dim_agent_runtime::Session) {
    let mut builder = AgentBuilder::new(AgentConfig::default());
    builder
        .process_value(Arc::new(ModelHost(driver)))
        .session_value(Arc::new(ToolHost(Arc::new(MockTools))))
        .session_value(Arc::new(HistoryRenderer::default()))
        .session_value(Arc::new(PrepareContextHost(Arc::new(IdentityContext))))
        .session_value(Arc::new(ContextModelHost(Some(Arc::new(facts)))))
        .session_value(Arc::new(CompactionStrategyHost(Some(strategy))))
        .require_feature(
            Feature::new("PrepareModelHost").session("PrepareModelHost", |names| {
                Ok(Box::new(DefaultPrepareModelPlugin::new(names)?))
            }),
        );
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
    let agent = MinimalHost::start(&builder).await.unwrap();
    let session = agent
        .create_session(SessionConfig::default())
        .await
        .unwrap();
    (agent, session)
}

/// `turns` turns of `chars` characters each.
async fn turns(session: &dim_agent_runtime::Session, turns: usize, chars: usize) {
    for _ in 0..turns {
        session
            .submit("x".repeat(chars), InputMode::Queue, "main")
            .await
            .unwrap();
        session.idle("main").await.unwrap();
    }
}

/// Runs the Loop's manual step and returns the branch's events once its
/// result is committed.
async fn compact_now(session: &dim_agent_runtime::Session) -> (u64, Vec<Arc<AgentEvent>>) {
    let command = session
        .service::<dim_agent_capabilities::agent_loop::AgentLoop>("main")
        .unwrap()
        .compact()
        .unwrap();
    let events = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let events = session.history("main", None).await.unwrap();
            if events.iter().any(|event| {
                matches!(&event.kind, EventKind::ContextCompaction(data)
                    if data.command == Some(command))
            }) {
                return events;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the manual compaction");
    (command, events)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rejected_candidates_keep_their_size_and_report_the_failed_condition() {
    for (chars, summary, window, expected) in [
        (20, 500, 4096., "does not reduce input"),
        (6000, 4500, 4096., "does not fit the model window"),
    ] {
        let summary: &str = Box::leak("s".repeat(summary).into_boxed_str());
        let strategy = Overshoot::new(1., vec![summary; 4]);
        let (agent, session) = replanned_session_with(
            Context {
                window,
                output: 64.,
                threshold: Some(f64::MAX),
                ..Context::default()
            },
            Arc::new(EchoModel),
            strategy.clone(),
        )
        .await;
        turns(&session, 2, chars).await;
        let calls_before = strategy.cuts().len();
        let events = session.history("main", None).await.unwrap();
        let before = session
            .context("main", events.last().unwrap().sequence)
            .await
            .unwrap();
        let history = project_context_history(&before).unwrap();
        let messages = dim_agent_capabilities::history::context_messages(&before, &history);
        for _ in 0..2 {
            let (command, events) = compact_now(&session).await;
            let (error, diagnostics) = events
                .iter()
                .find_map(|event| match &event.kind {
                    EventKind::ContextCompaction(ContextCompactionData {
                        command: Some(id),
                        outcome: ContextCompactionResult::Failed { error, diagnostics },
                        ..
                    }) if *id == command => Some((error, diagnostics.as_ref().unwrap())),
                    _ => None,
                })
                .expect("the rejected candidate has a failed fact and diagnostics");
            assert!(error.contains(expected), "{error}");
            let after = diagnostics
                .estimated_output_tokens
                .expect("the candidate size");
            if expected == "does not reduce input" {
                assert!(after >= diagnostics.estimated_input_tokens);
            } else {
                assert!(after < diagnostics.estimated_input_tokens);
                assert!(after >= window);
            }
            assert_eq!(diagnostics.entries_replaced, 0);
            assert_eq!(diagnostics.model_call_count, 1);
            let context = session
                .context("main", events.last().unwrap().sequence)
                .await
                .unwrap();
            let history = project_context_history(&context).unwrap();
            assert_eq!(
                dim_agent_capabilities::history::context_messages(&context, &history),
                messages
            );
        }
        assert_eq!(
            strategy.cuts().len(),
            calls_before + 2,
            "manual attempts still invoke the strategy"
        );
        agent.close().await.unwrap();
    }
}

/// The one completed result of `command` with its replacement and after-size.
fn completed(
    events: &[Arc<AgentEvent>],
    command: u64,
) -> (CompactionDiagnosticsRecord, Vec<Message>) {
    let data = events
        .iter()
        .find_map(|event| match &event.kind {
            EventKind::ContextCompaction(data) if data.command == Some(command) => Some(data),
            _ => None,
        })
        .expect("the compaction result");
    match &data.outcome {
        ContextCompactionResult::Completed {
            replacement,
            diagnostics,
            ..
        } => ((**diagnostics).clone(), replacement.clone()),
        outcome => panic!("expected a completion, got {outcome:?}"),
    }
}

/// The index of `id` in the branch's events.
fn order(events: &[Arc<AgentEvent>], id: &str) -> usize {
    events
        .iter()
        .position(|event| event.id == id)
        .unwrap_or_else(|| panic!("{id} is an event of the branch"))
}

/// The reviewer's case: a 2,100-character summary against a planned size of
/// 400 lands above `0.5 × B` with a removable tail left, so the re-plan cuts
/// later and the compaction completes within the bound without a mark
/// (ContextCompaction §5.3, acceptance 9).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_summary_over_its_plan_moves_the_cut() {
    let strategy = Overshoot::new(400., vec![Box::leak("d".repeat(2_100).into_boxed_str())]);
    let (agent, session) = replanned_session(Arc::new(EchoModel), strategy.clone()).await;
    turns(&session, 6, 400).await;
    let (command, events) = compact_now(&session).await;
    let (diagnostics, _) = completed(&events, command);
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event.kind, EventKind::ContextCompactionStart(_)))
            .count(),
        1,
        "one start for the one compaction"
    );
    let cuts = strategy.cuts();
    assert_eq!(cuts.len(), 2, "the strategy runs a second pass");
    assert!(
        order(&events, &cuts[1]) > order(&events, &cuts[0]),
        "the re-plan takes a later cut: {cuts:?}"
    );
    assert_eq!(diagnostics.model_call_count, 2, "both passes are counted");
    assert_eq!(diagnostics.insufficient_space, None, "within the bound");
    let after = diagnostics.estimated_output_tokens.expect("the after size");
    assert!(after <= 0.5 * 6_144., "{after}");
    agent.close().await.unwrap();
}

/// A second summary that overshoots again completes marked, and the mark
/// names the summary: after the re-plan its actual size is a part the
/// compaction cannot shrink (ContextCompaction §5.3).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_second_summary_that_overshoots_completes_marked() {
    let strategy = Overshoot::new(
        400.,
        vec![
            Box::leak("d".repeat(2_100).into_boxed_str()),
            Box::leak("e".repeat(3_300).into_boxed_str()),
        ],
    );
    let (agent, session) = replanned_session(Arc::new(EchoModel), strategy.clone()).await;
    turns(&session, 6, 400).await;
    let (command, events) = compact_now(&session).await;
    let (diagnostics, _) = completed(&events, command);
    assert_eq!(strategy.cuts().len(), 2, "there is no third pass");
    let mark = diagnostics
        .insufficient_space
        .expect("the result is still over the bound");
    assert_eq!(mark.parts[0].part, InsufficientSpacePartName::Summary);
    agent.close().await.unwrap();
}

/// The second summary is larger still, so the re-planned result keeps a
/// small tail and lands above the bound although the resident parts fit.
/// The mark is measured from that total, so the record says so and names the
/// summary (ContextCompaction §5.3 step 5). The window and the summaries
/// leave the digest's planned share (§5.10) room for that tail.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_modest_second_overshoot_is_marked() {
    let strategy = Overshoot::new(
        400.,
        vec![
            Box::leak("d".repeat(2_715).into_boxed_str()),
            Box::leak("e".repeat(3_400).into_boxed_str()),
        ],
    );
    let (agent, session) = replanned_session_with(
        Context {
            window: 12_288.,
            ..facts()
        },
        Arc::new(EchoModel),
        strategy.clone(),
    )
    .await;
    turns(&session, 6, 500).await;
    let (command, events) = compact_now(&session).await;
    let (diagnostics, _) = completed(&events, command);
    assert_eq!(strategy.cuts().len(), 2, "the strategy runs a second pass");
    let after = diagnostics.estimated_output_tokens.expect("the after size");
    assert!(
        after > 0.5 * 8_192.,
        "the result is above the bound: {after}"
    );
    let mark = diagnostics
        .insufficient_space
        .expect("the result above the bound is marked");
    assert_eq!(mark.parts.len(), 1, "{:?}", mark.parts);
    assert_eq!(mark.parts[0].part, InsufficientSpacePartName::Summary);
    assert!(
        mark.parts[0].tokens < 0.5 * 8_192.,
        "the resident parts fit on their own: {:?}",
        mark.parts
    );
    agent.close().await.unwrap();
}

/// A second pass that reports nothing to add leaves the first result
/// standing, and the same total-based rule marks it
/// (ContextCompaction §5.3 step 5, §12).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_skipped_re_plan_leaves_the_first_result_marked() {
    let strategy = Overshoot::skipping(400., vec![Box::leak("d".repeat(2_100).into_boxed_str())]);
    let (agent, session) = replanned_session(Arc::new(EchoModel), strategy.clone()).await;
    turns(&session, 6, 400).await;
    let (command, events) = compact_now(&session).await;
    let (diagnostics, _) = completed(&events, command);
    assert_eq!(
        strategy.cuts().len(),
        1,
        "the continuation cut nothing before it skipped"
    );
    let after = diagnostics.estimated_output_tokens.expect("the after size");
    assert!(
        after > 0.5 * 6_144.,
        "the first result is above the bound: {after}"
    );
    let mark = diagnostics
        .insufficient_space
        .expect("the standing first result is marked");
    assert_eq!(mark.parts[0].part, InsufficientSpacePartName::Summary);
    assert!(
        mark.parts[0].tokens < 0.5 * 6_144.,
        "the resident parts fit on their own: {:?}",
        mark.parts
    );
    agent.close().await.unwrap();
}

/// A first pass that replaced everything leaves no later cut, so there is no
/// second pass and the mark applies (ContextCompaction §5.3).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_compaction_that_replaced_everything_is_not_re_planned() {
    let strategy = Overshoot::new(400., vec![Box::leak("f".repeat(3_300).into_boxed_str())]);
    let (agent, session) = replanned_session(Arc::new(EchoModel), strategy.clone()).await;
    // Two turns of 1,600 characters: each entry alone is larger than the tail
    // budget the plan leaves, so the first pass keeps no verbatim tail.
    turns(&session, 2, 1_600).await;
    let (command, events) = compact_now(&session).await;
    let (diagnostics, replacement) = completed(&events, command);
    assert_eq!(strategy.cuts().len(), 1, "no later cut exists");
    assert_eq!(replacement.len(), 1, "the replacement is the summary alone");
    let mark = diagnostics
        .insufficient_space
        .expect("only the summary is too large");
    assert_eq!(mark.parts[0].part, InsufficientSpacePartName::Summary);
    agent.close().await.unwrap();
}

/// Echoes until it has taken `fail_after` calls, then reports an exhausted
/// output budget, so a built history reaches one recovery compaction.
struct BudgetAfter {
    calls: AtomicUsize,
    fail_after: usize,
}
impl ModelDriver for BudgetAfter {
    fn invoke(
        &self,
        request: Arc<ModelRequest>,
        connection: ModelConnection,
        cancel: CancellationToken,
    ) -> HostFuture<'_, ModelStream> {
        if self.calls.fetch_add(1, Ordering::SeqCst) < self.fail_after {
            return EchoModel.invoke(request, connection, cancel);
        }
        Box::pin(async {
            Err(HostError::Failed {
                kind: HostErrorKind::Other,
                message: "Provider output budget exhausted".into(),
                model: None,
                presentation: None,
                budget: Some(ProviderOutputBudgetFacts {
                    input_tokens: 7_000.,
                    context_window: 10_240.,
                    requested_max_tokens: 4_096.,
                    allowed_max_tokens: 3_240.,
                    ratio: 1.,
                }),
            })
        })
    }
}

/// A recovery compaction is one compaction per request; its re-plan is a
/// second pass of that compaction and not a second compaction
/// (ContextCompaction §5.3, acceptance 12).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_recovery_compaction_is_re_planned_once() {
    // The first summary keeps the digest's planned share inside the bound, so
    // the re-plan still finds a later cut for the second pass.
    let strategy = Overshoot::new(400., vec![Box::leak("d".repeat(1_900).into_boxed_str())]);
    let driver = Arc::new(BudgetAfter {
        calls: AtomicUsize::new(0),
        fail_after: 6,
    });
    let (agent, session) = replanned_session(driver, strategy.clone()).await;
    turns(&session, 6, 400).await;
    session
        .submit("y".repeat(400), InputMode::Queue, "main")
        .await
        .unwrap();
    session.idle("main").await.unwrap();
    let events = session.history("main", None).await.unwrap();
    let starts: Vec<_> = events
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::ContextCompactionStart(data) => Some(data.trigger),
            _ => None,
        })
        .collect();
    assert_eq!(
        starts,
        [CompactionTrigger::ProviderOutputBudgetExhausted],
        "one recovery compaction, not two"
    );
    assert_eq!(strategy.cuts().len(), 2, "its re-plan is a second pass");
    let diagnostics = events
        .iter()
        .find_map(|event| match &event.kind {
            EventKind::ContextCompaction(ContextCompactionData {
                outcome: ContextCompactionResult::Completed { diagnostics, .. },
                ..
            }) => Some(&diagnostics.model_call_count),
            _ => None,
        })
        .expect("the recovery compaction completed");
    assert_eq!(*diagnostics, 2, "both passes are counted in one result");
    agent.close().await.unwrap();
}

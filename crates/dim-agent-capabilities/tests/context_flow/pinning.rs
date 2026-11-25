//! Class 2(a) pinning: a cut inside the running turn carries that turn's
//! consumed inputs (ContextCompaction §5.9, acceptance 13), and the record's
//! `replacement_parts` locates them (ContextCompaction §4.3).
use super::*;
use dim_agent_capabilities::history::summary_message;
use dim_agent_runtime::testing::chain_events;

/// Emits a `wait` call carrying `text` when the last message asks for the
/// tool, at most twice: the wait holds the turn long enough for a steer to
/// arrive while the tool runs. Echoes otherwise, so a compaction that keeps
/// the pin — the turn's own words — cannot ask for the tool for ever.
#[derive(Default)]
struct ToolWait {
    text: std::sync::Mutex<String>,
    waits: AtomicUsize,
}
impl ModelDriver for ToolWait {
    fn invoke(
        &self,
        request: Arc<ModelRequest>,
        connection: ModelConnection,
        cancel: CancellationToken,
    ) -> HostFuture<'_, ModelStream> {
        let asks = matches!(&request.messages.last().unwrap().content,
            MessageContent::Text(text) if text == "use the tool")
            && self.waits.fetch_add(1, Ordering::SeqCst) < 2;
        Box::pin(async move {
            if !asks {
                return EchoModel.invoke(request, connection, cancel).await;
            }
            let text = self.text.lock().unwrap().clone();
            Ok(Box::pin(futures_util::stream::iter(vec![
                Ok(ModelChunk::MessageStart {
                    message_id: "tool".into(),
                }),
                Ok(ModelChunk::ToolStart {
                    id: "wait-call".into(),
                    name: "wait".into(),
                }),
                Ok(ModelChunk::ToolUse(ToolCall {
                    id: "wait-call".into(),
                    name: "wait".into(),
                    input: serde_json::from_value(
                        serde_json::json!({ "millis": 300, "text": text }),
                    )
                    .unwrap(),
                })),
                Ok(ModelChunk::MessageStop {
                    message_id: "tool".into(),
                    reason: ModelStopReason::ToolUse,
                }),
            ])) as ModelStream)
        })
    }
}

/// Submits the input, waits for its `wait` call to start, then submits the
/// steer while the tool still runs.
async fn turn_with_steer(session: &dim_agent_runtime::Session, steer: &str) {
    session
        .submit("use the tool".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let events = chain_events(session, "main", ..).await.unwrap();
            if events
                .iter()
                .any(|event| matches!(event.kind, EventKind::ToolCall(_)))
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the tool call starts");
    session
        .submit(steer.into(), InputMode::Steer, "main")
        .await
        .unwrap();
}

/// A session with the `wait` tool and full access, so a turn can continue
/// after its tool result and hold while a steer arrives.
async fn tool_session(
    model: Arc<ToolWait>,
    threshold: f64,
) -> (MinimalHost, dim_agent_runtime::Session) {
    let agent = MinimalHost::start(&builder_with_context(
        model,
        HistoryRenderer::default(),
        Context {
            threshold: Some(threshold),
            ..Context::default()
        },
    ))
    .await
    .unwrap();
    let session = agent
        .create_session(
            SessionConfig {
                tools: Some(vec!["wait".into()]),
                permission_policy: dim_agent_runtime::PermissionPolicy {
                    preset: dim_agent_runtime::PermissionPreset::FullAccess,
                    ..Default::default()
                },
                ..Default::default()
            },
            Default::default(),
        )
        .await
        .unwrap();
    (agent, session)
}

/// The same hosts with a configured model selection, which the Loop's
/// manual step needs to build its request.
fn configured(model: Arc<dyn ModelDriver>, context: Context) -> AgentBuilder {
    let mut builder = AgentBuilder::new(AgentConfig::default());
    builder
        .process_value(Arc::new(ModelHost(model)))
        .session_value(Arc::new(ToolHost(Arc::new(MockTools))))
        .session_value(Arc::new(HistoryRenderer::default()))
        .session_value(Arc::new(PrepareContextHost(Arc::new(IdentityContext))))
        .session_value(Arc::new(ContextModelHost(Some(Arc::new(context)))))
        .session_value(Arc::new(CompactionStrategyHost(Some(Arc::new(context)))))
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
    builder
}

/// The one completed compaction, with its replacement, parts and start.
fn completed(
    events: &[Arc<AgentEvent>],
) -> (
    &ContextCompactionData,
    Vec<Message>,
    Option<ReplacementParts>,
) {
    let data = events
        .iter()
        .find_map(|event| match &event.kind {
            EventKind::ContextCompaction(data)
                if matches!(data.outcome, ContextCompactionResult::Completed { .. }) =>
            {
                Some(data)
            }
            _ => None,
        })
        .expect("no completed compaction");
    let ContextCompactionResult::Completed {
        replacement,
        replacement_parts,
        ..
    } = &data.outcome
    else {
        unreachable!()
    };
    (data, replacement.clone(), *replacement_parts)
}

/// A cut that replaces the running turn's input pins it verbatim, after the
/// strategy's messages (ContextCompaction §5.9).
#[tokio::test]
async fn a_cut_inside_the_running_turn_pins_its_input_verbatim() {
    let model = Arc::new(ToolWait::default());
    *model.text.lock().unwrap() = "t".repeat(2_000);
    let (agent, session) = tool_session(model, 3_000.).await;
    session
        .submit("use the tool".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    session.idle("main").await.unwrap();
    let events = chain_events(&session, "main", ..).await.unwrap();
    let (_, replacement, parts) = completed(&events);
    assert_eq!(replacement.len(), 2, "{replacement:?}");
    assert_eq!(replacement[0], summary_message("short"));
    assert_eq!(
        replacement[1],
        Message::text(Role::User, "use the tool"),
        "the running turn's input is pinned verbatim"
    );
    assert_eq!(
        parts,
        Some(ReplacementParts {
            strategy: 1,
            digest: 0
        })
    );
    let at = events
        .iter()
        .position(|event| {
            matches!(&event.kind, EventKind::ContextCompaction(data)
                if matches!(data.outcome, ContextCompactionResult::Completed { .. }))
        })
        .unwrap();
    let call = events[at..]
        .iter()
        .find_map(|event| match &event.kind {
            EventKind::ModelCall(data) => Some(&data.request),
            _ => None,
        })
        .expect("the request after the compaction");
    assert_eq!(
        call.messages[0].as_ref(),
        &summary_message("short"),
        "the pin follows the summary"
    );
    assert_eq!(
        call.messages[1].as_ref(),
        &Message::text(Role::User, "use the tool")
    );
    agent.close().await.unwrap();
}

/// A steer that joined the turn is one of its consumed inputs, pinned in
/// event order after the input it continued (ContextCompaction §5.9).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_steer_that_joined_the_turn_is_pinned_in_order() {
    let model = Arc::new(ToolWait::default());
    // Small enough that the first tool cycle's request fits the input
    // budget, so the compaction runs on the second cycle's request, when
    // both inputs are consumed (ContextCompaction §5.3, §5.9).
    *model.text.lock().unwrap() = "t".repeat(1_200);
    let (agent, session) = tool_session(model, 3_000.).await;
    turn_with_steer(&session, "use the tool").await;
    session.idle("main").await.unwrap();
    let events = chain_events(&session, "main", ..).await.unwrap();
    let (_, replacement, parts) = completed(&events);
    assert_eq!(replacement.len(), 3, "{replacement:?}");
    assert_eq!(replacement[0], summary_message("short"));
    assert_eq!(
        replacement[1],
        Message::text(Role::User, "use the tool"),
        "the turn's first input comes first"
    );
    assert_eq!(
        replacement[2],
        Message::text(Role::User, "use the tool"),
        "the steer that joined the turn follows it"
    );
    assert_eq!(
        parts,
        Some(ReplacementParts {
            strategy: 1,
            digest: 0
        })
    );
    agent.close().await.unwrap();
}

/// Emits a `wait` call carrying its text the first time a request ends with an
/// input asking to `run`, so each such turn runs one tool cycle and the request
/// after it is large enough to compact. An input it already asked for echoes,
/// so a compaction that pins it cannot ask again for ever.
#[derive(Default)]
struct WaitOnInput {
    text: String,
    asked: std::sync::Mutex<std::collections::HashSet<String>>,
}
impl ModelDriver for WaitOnInput {
    fn invoke(
        &self,
        request: Arc<ModelRequest>,
        connection: ModelConnection,
        cancel: CancellationToken,
    ) -> HostFuture<'_, ModelStream> {
        let asked = match &request.messages.last().unwrap().content {
            MessageContent::Text(text) if text.starts_with("run ") => {
                self.asked.lock().unwrap().insert(text.clone())
            }
            _ => false,
        };
        let text = self.text.clone();
        let request = request.clone();
        Box::pin(async move {
            if !asked {
                return EchoModel.invoke(request, connection, cancel).await;
            }
            Ok(Box::pin(futures_util::stream::iter(vec![
                Ok(ModelChunk::MessageStart {
                    message_id: "tool".into(),
                }),
                Ok(ModelChunk::ToolStart {
                    id: "wait-call".into(),
                    name: "wait".into(),
                }),
                Ok(ModelChunk::ToolUse(ToolCall {
                    id: "wait-call".into(),
                    name: "wait".into(),
                    input: serde_json::from_value(
                        serde_json::json!({ "millis": 300, "text": text }),
                    )
                    .unwrap(),
                })),
                Ok(ModelChunk::MessageStop {
                    message_id: "tool".into(),
                    reason: ModelStopReason::ToolUse,
                }),
            ])) as ModelStream)
        })
    }
}

/// A queued input is written while the previous turn runs and consumed at the
/// next turn's start, so its `input/message` sits before that turn's
/// `turn/start`; a cut inside the turn still pins it (ContextCompaction §5.9).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_queued_input_consumed_at_the_next_turns_start_is_pinned() {
    let model = Arc::new(WaitOnInput {
        text: "t".repeat(4_000),
        ..Default::default()
    });
    let agent = MinimalHost::start(&builder_with_context(
        model,
        HistoryRenderer::default(),
        Context {
            threshold: Some(3_000.),
            ..Context::default()
        },
    ))
    .await
    .unwrap();
    let session = agent
        .create_session(
            SessionConfig {
                tools: Some(vec!["wait".into()]),
                permission_policy: dim_agent_runtime::PermissionPolicy {
                    preset: dim_agent_runtime::PermissionPreset::FullAccess,
                    ..Default::default()
                },
                ..Default::default()
            },
            Default::default(),
        )
        .await
        .unwrap();
    session
        .submit("run one".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let events = chain_events(&session, "main", ..).await.unwrap();
            if events
                .iter()
                .any(|event| matches!(event.kind, EventKind::ToolCall(_)))
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the first turn runs");
    session
        .submit("run two".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    session.idle("main").await.unwrap();
    let events = chain_events(&session, "main", ..).await.unwrap();
    let input = queued(&events, "run two");
    let consumed = events
        .iter()
        .position(
            |event| matches!(&event.kind, EventKind::InputConsumed(data) if data.input_id == input),
        )
        .expect("the second turn consumes the queued input");
    let start = events[..consumed]
        .iter()
        .rposition(|event| matches!(event.kind, EventKind::TurnStart(_)))
        .expect("the second turn started");
    assert!(
        events[start..consumed]
            .iter()
            .all(|event| !matches!(event.kind, EventKind::InputMessage(_))),
        "the queued input's message precedes the turn's start"
    );
    let pinned = replacements(&events)
        .into_iter()
        .flatten()
        .any(|message| serde_json::to_string(&message).unwrap().contains("run two"));
    assert!(pinned, "a cut inside the turn pins the queued input");
    agent.close().await.unwrap();
}

/// The event id of the `input/message` whose operator text is `text`.
fn queued(events: &[Arc<AgentEvent>], text: &str) -> String {
    events
        .iter()
        .find_map(|event| match &event.kind {
            EventKind::InputMessage(InputMessageData {
                user: Some(user), ..
            }) if user.content.iter().any(
                |block| matches!(block, ContentBlock::Text { text: block } if block == text),
            ) =>
            {
                Some(event.id.clone())
            }
            _ => None,
        })
        .expect("the queued input")
}

/// An input consumed at the cut stays in the kept tail and is not repeated
/// in the replacement (ContextCompaction §5.9).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_input_consumed_at_the_cut_is_not_duplicated() {
    let model = Arc::new(ToolWait::default());
    *model.text.lock().unwrap() = "t".repeat(2_000);
    // The first tool cycle's request already reaches the threshold; the cut
    // is the boundary that consumes the steer, which stays in the tail.
    let (agent, session) = tool_session(model, 3_000.).await;
    turn_with_steer(&session, "STEER-MARKER").await;
    session.idle("main").await.unwrap();
    let events = chain_events(&session, "main", ..).await.unwrap();
    let (_, replacement, parts) = completed(&events);
    assert_eq!(
        replacement,
        vec![
            summary_message("short"),
            Message::text(Role::User, "use the tool"),
        ],
        "the input before the cut is pinned; the steer at it is not"
    );
    assert_eq!(
        parts,
        Some(ReplacementParts {
            strategy: 1,
            digest: 0
        })
    );
    let at = events
        .iter()
        .position(|event| {
            matches!(&event.kind, EventKind::ContextCompaction(data)
                if matches!(data.outcome, ContextCompactionResult::Completed { .. }))
        })
        .unwrap();
    let call = events[at..]
        .iter()
        .find_map(|event| match &event.kind {
            EventKind::ModelCall(data) => Some(&data.request),
            _ => None,
        })
        .expect("the request after the compaction");
    let occurrences = call
        .messages
        .iter()
        .filter(|message| {
            serde_json::to_string(message.as_ref())
                .unwrap()
                .contains("STEER-MARKER")
        })
        .count();
    assert_eq!(occurrences, 1, "{:?}", call.messages);
    agent.close().await.unwrap();
}

/// A manual step leaves no running turn, so it pins nothing
/// (ContextCompaction §5.9): the replacement is the strategy's messages.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_compaction_with_no_running_turn_pins_nothing() {
    let agent = MinimalHost::start(&configured(Arc::new(EchoModel), Context::default()))
        .await
        .unwrap();
    let session = agent
        .create_session(SessionConfig::default(), Default::default())
        .await
        .unwrap();
    for text in ["x".repeat(1_500), "second".into()] {
        session
            .submit(text, InputMode::Queue, "main")
            .await
            .unwrap();
        session.idle("main").await.unwrap();
    }
    let command = session
        .service::<dim_agent_capabilities::agent_loop::AgentLoop>("main")
        .unwrap()
        .compact()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let events = chain_events(&session, "main", ..).await.unwrap();
            if events.iter().any(|event| {
                matches!(&event.kind,
                    EventKind::ContextCompaction(data) if data.command == Some(command))
            }) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let events = chain_events(&session, "main", ..).await.unwrap();
    let (data, replacement, parts) = completed(&events);
    assert_eq!(data.command, Some(command));
    assert_eq!(replacement, vec![summary_message("short")]);
    assert_eq!(parts, None, "no pinned inputs, so no parts field");
    agent.close().await.unwrap();
}

/// The marked input of a turn that compacts more than once.
const MARKED: &str = "MARKED-INPUT-0001";

/// Emits one `wait` call for each of its first `cycles` calls, then echoes:
/// one turn long enough to compact more than once. The count is the model's
/// own, so a compaction that drops a tool result from the context does not
/// start the cycle again.
struct WaitCycles {
    cycles: usize,
    calls: AtomicUsize,
    text: String,
}
impl ModelDriver for WaitCycles {
    fn invoke(
        &self,
        request: Arc<ModelRequest>,
        connection: ModelConnection,
        cancel: CancellationToken,
    ) -> HostFuture<'_, ModelStream> {
        let done = self.calls.fetch_add(1, Ordering::SeqCst);
        let (cycles, text) = (self.cycles, self.text.clone());
        Box::pin(async move {
            if done >= cycles {
                return EchoModel.invoke(request, connection, cancel).await;
            }
            Ok(Box::pin(futures_util::stream::iter(vec![
                Ok(ModelChunk::MessageStart {
                    message_id: "tool".into(),
                }),
                Ok(ModelChunk::ToolStart {
                    id: "wait-call".into(),
                    name: "wait".into(),
                }),
                Ok(ModelChunk::ToolUse(ToolCall {
                    id: "wait-call".into(),
                    name: "wait".into(),
                    input: serde_json::from_value(
                        serde_json::json!({ "millis": 300, "text": text }),
                    )
                    .unwrap(),
                })),
                Ok(ModelChunk::MessageStop {
                    message_id: "tool".into(),
                    reason: ModelStopReason::ToolUse,
                }),
            ])) as ModelStream)
        })
    }
}

/// A session with the `wait` tool and full access, whose turns run `cycles`
/// wait cycles over `700`-character results.
async fn cycles_session(
    cycles: usize,
    planned: Option<f64>,
    renderer: HistoryRenderer,
) -> (MinimalHost, dim_agent_runtime::Session) {
    let model = Arc::new(WaitCycles {
        cycles,
        calls: AtomicUsize::new(0),
        text: "p".repeat(700),
    });
    let agent = MinimalHost::start(&builder_with_context(
        model,
        renderer,
        Context {
            threshold: Some(3_000.),
            replacement: planned,
            ..Context::default()
        },
    ))
    .await
    .unwrap();
    let session = agent
        .create_session(
            SessionConfig {
                tools: Some(vec!["wait".into()]),
                permission_policy: dim_agent_runtime::PermissionPolicy {
                    preset: dim_agent_runtime::PermissionPreset::FullAccess,
                    ..Default::default()
                },
                ..Default::default()
            },
            Default::default(),
        )
        .await
        .unwrap();
    (agent, session)
}

/// Every completed compaction's replacement, in order.
fn replacements(events: &[Arc<AgentEvent>]) -> Vec<Vec<Message>> {
    events
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::ContextCompaction(ContextCompactionData {
                outcome: ContextCompactionResult::Completed { replacement, .. },
                ..
            }) => Some(replacement.clone()),
            _ => None,
        })
        .collect()
}

/// The steer the turn consumes between two compactions.
fn steer() -> String {
    format!("STEER-MARKER {}", "s".repeat(260))
}

/// The replacements as one string, for a containment check.
fn text(messages: &[Message]) -> String {
    serde_json::to_string(messages).unwrap()
}

/// Waits until `done` holds for the branch's committed events.
async fn until(
    session: &dim_agent_runtime::Session,
    done: impl Fn(&[Arc<AgentEvent>]) -> bool,
) -> Vec<Arc<AgentEvent>> {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let events = chain_events(session, "main", ..).await.unwrap();
            if done(&events) {
                return events;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the expected event")
}

/// The turn's input is pinned by the first compaction, and the next
/// compaction carries that pin although the recovery snapshot resumes after
/// the turn's `turn/start`: a snapshot is only a cache, so both runs produce
/// the same replacements (ContextCompaction §5.9).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_running_turns_pins_do_not_depend_on_a_snapshot() {
    let run = |renderer| async move {
        let (agent, session) = cycles_session(6, None, renderer).await;
        session
            .submit(MARKED.into(), InputMode::Queue, "main")
            .await
            .unwrap();
        session.idle("main").await.unwrap();
        let events = chain_events(&session, "main", ..).await.unwrap();
        agent.close().await.unwrap();
        replacements(&events)
    };
    let cached = run(HistoryRenderer::default()).await;
    let replayed = run(HistoryRenderer {
        encode_checkpoint: |_, _| Err("snapshot encoding disabled".into()),
        ..HistoryRenderer::default()
    })
    .await;
    assert_eq!(cached, replayed, "a snapshot is only a cache");
    assert!(cached.len() >= 2, "the turn compacts twice: {cached:?}");
    assert!(
        cached.iter().all(
            |replacement| replacement
                .iter()
                .any(|message| serde_json::to_string(message).unwrap().contains(MARKED))
        ),
        "the running turn's input is pinned by every compaction: {cached:?}"
    );
}

/// A steer the turn consumes between two compactions is pinned after the pin
/// carried from the first (ContextCompaction §5.9).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_steer_after_a_compaction_is_pinned_after_the_carried_pin() {
    let (agent, session) = cycles_session(8, Some(1_300.), HistoryRenderer::default()).await;
    session
        .submit(MARKED.into(), InputMode::Queue, "main")
        .await
        .unwrap();
    let events = until(&session, |events| !replacements(events).is_empty()).await;
    let at = events.len();
    until(&session, |events| {
        events[at..]
            .iter()
            .any(|event| matches!(event.kind, EventKind::ToolCall(_)))
    })
    .await;
    session
        .submit(steer(), InputMode::Steer, "main")
        .await
        .unwrap();
    session.idle("main").await.unwrap();
    let events = chain_events(&session, "main", ..).await.unwrap();
    let all = replacements(&events);
    let second = all
        .iter()
        .find(|replacement| text(replacement).contains("STEER-MARKER"))
        .unwrap_or_else(|| panic!("a compaction after the steer: {all:?}"));
    assert_eq!(second[0], summary_message("short"));
    let carried = second
        .iter()
        .position(|message| text(std::slice::from_ref(message)).contains(MARKED))
        .expect("the carried pin");
    let steer = second
        .iter()
        .position(|message| text(std::slice::from_ref(message)).contains("STEER-MARKER"))
        .expect("the steer's pin");
    assert_eq!(
        (carried, steer),
        (1, 2),
        "the carried pin comes first, then the steer the turn consumed since: {second:?}"
    );
    agent.close().await.unwrap();
}

/// A merged pin list over `PINNED_SHARE × B` is truncated equally, and each
/// input keeps the handle naming its own `input/message` id, the carried one
/// included (ContextCompaction §5.9).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_merged_pin_over_the_share_keeps_each_inputs_handle() {
    // Each of the two inputs fits the share alone, so the first compaction
    // pins the turn's input verbatim and without a handle; together they
    // exceed it, and both keep their head and tail with their own id.
    let (agent, session) = cycles_session(8, Some(1_300.), HistoryRenderer::default()).await;
    session
        .submit(
            format!("{MARKED} {}", "m".repeat(260)),
            InputMode::Queue,
            "main",
        )
        .await
        .unwrap();
    let events = until(&session, |events| !replacements(events).is_empty()).await;
    let at = events.len();
    until(&session, |events| {
        events[at..]
            .iter()
            .any(|event| matches!(event.kind, EventKind::ToolCall(_)))
    })
    .await;
    session
        .submit(steer(), InputMode::Steer, "main")
        .await
        .unwrap();
    session.idle("main").await.unwrap();
    let events = chain_events(&session, "main", ..).await.unwrap();
    let all = replacements(&events);
    let second = all
        .iter()
        .find(|replacement| text(replacement).contains("STEER-MARKER"))
        .unwrap_or_else(|| panic!("a compaction after the steer: {all:?}"));
    let ids: Vec<String> = events
        .iter()
        .filter(|event| matches!(event.kind, EventKind::InputMessage(_)))
        .map(|event| event.id.clone())
        .collect();
    assert_eq!(second.len(), 3, "{second:?}");
    assert_eq!(second[0], summary_message("short"));
    let carried = serde_json::to_string(&second[1]).unwrap();
    let steer = serde_json::to_string(&second[2]).unwrap();
    assert!(
        carried.contains(MARKED),
        "the carried pin keeps its head: {carried}"
    );
    assert!(
        carried.contains(&format!("[input {}]", ids[0])),
        "the carried pin names its own input: {carried}"
    );
    assert!(
        steer.contains("STEER-MARKER"),
        "the steer keeps its head: {steer}"
    );
    assert!(
        steer.contains(&format!("[input {}]", ids[1])),
        "the steer names its own input: {steer}"
    );
    agent.close().await.unwrap();
}

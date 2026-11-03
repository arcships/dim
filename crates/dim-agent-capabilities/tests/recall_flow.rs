//! Recall acceptance flows: §14 of `docs/design/plugins/Recall.md`.
//!
//! Test strategy: the flows the local-execution profile can produce on demand
//! run end to end through a live Session and the `recall` tool's contract, so
//! the registration, the pinned read through the Branch facade and the tool
//! result are all exercised. The shapes it cannot produce on demand — a denied
//! or background or sub-agent call, a Session fork, an interrupted output, a
//! turn longer than a page — are recorded as the events their producers write,
//! in the order the Loop writes them, and answered by the same projection the
//! tool runs ([`Narrative`]). Each recorded history must be one context
//! assembly projects, and every entry it places must be told (§14 flow 4).
use dim_agent_capabilities::{
    adapters::EchoModel,
    context_block, defaults,
    history::{HistoryRenderer, project_history},
    host::{HostFuture, ModelConnection, ModelDriver, ModelStream},
    recall::{self, RECALL_TOOL, testing::Narrative},
};
use dim_agent_runtime::testing::MinimalHost;
use dim_agent_runtime::*;
use dim_agent_types::*;
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;

async fn agent() -> MinimalHost {
    agent_with(Arc::new(MemoryTimelineStore::default())).await
}

async fn agent_with(store: Arc<dyn TimelineStore>) -> MinimalHost {
    let mut builder = AgentBuilder::new(AgentConfig::default());
    defaults::mock_hosts_with_model(
        &mut builder,
        HistoryRenderer::default(),
        Arc::new(LineModel),
    );
    defaults::install(&mut builder);
    recall::install(&mut builder);
    builder.timeline_store(store);
    MinimalHost::start(&builder).await.unwrap()
}

/// The echo model, still answering a `tool:` command when a line write of
/// another line ([timeline-branches §7.1, §12.1]) arrives in the same request.
/// The echo model answers the request's last message, which such a write
/// replaces when it is applied between the command and the model step; the
/// command is then sent alone, as the model reads it when the write arrives a
/// step later.
struct LineModel;
impl ModelDriver for LineModel {
    fn invoke(
        &self,
        request: Arc<ModelRequest>,
        connection: ModelConnection,
        cancel: CancellationToken,
    ) -> HostFuture<'_, ModelStream> {
        let messages = &request.messages;
        let mut end = messages.len();
        while end > 0 && said(&messages[end - 1]).starts_with("<dim-branch-") {
            end -= 1;
        }
        let command = end < messages.len()
            && end > 0
            && messages[end - 1].role == Role::User
            && said(&messages[end - 1]).starts_with("tool:");
        if !command {
            return EchoModel.invoke(request, connection, cancel);
        }
        let mut answered = (*request).clone();
        answered.messages.truncate(end);
        EchoModel.invoke(Arc::new(answered), connection, cancel)
    }
}

fn said(message: &Message) -> String {
    let parts: Vec<String> = match &message.content {
        MessageContent::Text(text) => vec![text.clone()],
        MessageContent::Parts(parts) => parts
            .iter()
            .filter_map(|part| match part {
                MessagePart::Text { text, .. } => Some(text.clone()),
                MessagePart::ToolResult { content, .. } => match content {
                    ToolContent::Text(text) => Some(text.clone()),
                    ToolContent::Blocks(_) => None,
                },
                _ => None,
            })
            .collect(),
    };
    parts.join("\n")
}

async fn open(agent: &Agent) -> Session {
    agent
        .create_session(SessionConfig {
            tools: None,
            ..SessionConfig::default()
        })
        .await
        .unwrap()
}

/// Runs one `recall` call on `branch` and reads the text the model got.
async fn recall(session: &Session, branch: &str, arguments: Value) -> (String, bool) {
    let text = match &arguments {
        Value::Object(map) if map.is_empty() => format!("tool:{RECALL_TOOL}"),
        other => format!("tool:{RECALL_TOOL} {other}"),
    };
    let before = tool_results(session, branch).await.len();
    session
        .submit(text, InputMode::Queue, branch)
        .await
        .unwrap();
    session.idle(branch).await.unwrap();
    let results = tool_results(session, branch).await;
    assert!(
        results.len() > before,
        "the {} call wrote no result",
        RECALL_TOOL
    );
    let result = results.last().expect("the recall call's result");
    let text = match &result.content {
        ToolContent::Text(text) => text.clone(),
        other => panic!("expected text, got {other:?}"),
    };
    (text, result.is_error.unwrap_or(false))
}

async fn tool_results(session: &Session, branch: &str) -> Vec<ToolResult> {
    session
        .history(branch, None)
        .await
        .unwrap()
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::ToolResult(data) => data.results.last().map(|item| item.result.clone()),
            _ => None,
        })
        .collect()
}

/// Waits until `holds` reads true of the branch's events, waking on the
/// branch's dispatch progress instead of polling, and fails with the event
/// kinds seen last.
async fn wait_for_events(
    session: &Session,
    branch: &str,
    what: &str,
    mut holds: impl FnMut(&[Arc<AgentEvent>]) -> bool,
) {
    let mut dispatch = session.dispatch_watch(branch).unwrap();
    let mut seen = String::new();
    let waited = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let events = session.history(branch, None).await.unwrap();
            seen = events
                .iter()
                .map(|event| event.kind.key().to_owned())
                .collect::<Vec<_>>()
                .join(",");
            if holds(&events) {
                return;
            }
            dispatch.changed().await.unwrap();
        }
    })
    .await;
    assert!(waited.is_ok(), "waiting until {what}: {seen}");
}

/// The id of the branch's newest `turn/start` line.
fn last_turn(events: &[Arc<AgentEvent>]) -> String {
    events
        .iter()
        .rev()
        .find(|event| matches!(event.kind, EventKind::TurnStart(_)))
        .expect("a started turn")
        .id
        .clone()
}

fn id_of_input(events: &[Arc<AgentEvent>], text: &str) -> String {
    events
        .iter()
        .find_map(|event| match &event.kind {
            EventKind::InputMessage(data)
                if data.user.as_ref().is_some_and(|user| {
                    user.content
                        .iter()
                        .any(|block| matches!(block, ContentBlock::Text { text: at } if at == text))
                }) =>
            {
                Some(event.id.clone())
            }
            _ => None,
        })
        .expect("the input")
}

fn line_of<'a>(text: &'a str, id: &str) -> &'a str {
    text.lines()
        .find(|line| line.split(' ').nth(1) == Some(id))
        .unwrap_or_else(|| panic!("no line for {id} in:\n{text}"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
// §14 1.
async fn the_turn_index_lists_turns_latest_first_and_pages_back_to_nothing_earlier() {
    tokio::time::timeout(Duration::from_secs(60), async {
        let agent = agent().await;
        let session = open(&agent).await;
        for index in 0..21 {
            session
                .submit(format!("task {index}"), InputMode::Queue, "main")
                .await
                .unwrap();
            session.idle("main").await.unwrap();
        }
        let events = session.history("main", None).await.unwrap();
        let turns: Vec<_> = events
            .iter()
            .filter(|event| matches!(event.kind, EventKind::TurnStart(_)))
            .collect();
        assert_eq!(turns.len(), 21);

        let (page, error) = recall(&session, "main", json!({})).await;
        assert!(!error, "{page}");
        let lines: Vec<_> = page.lines().collect();
        // A date line, 20 entries and the last line.
        assert_eq!(lines.len(), 22, "{page}");
        assert!(lines[0].ends_with(" (UTC)"), "{page}");
        let calling = last_turn(&session.history("main", None).await.unwrap());
        assert!(
            lines[1].contains(&format!("{calling} turn: user \"tool:{RECALL_TOOL}\""))
                && lines[1].contains("you called recall")
                && lines[1].ends_with("in progress"),
            "{}",
            lines[1]
        );
        assert!(
            lines[2].contains(&format!("{} turn: user \"task 20\"", turns[20].id))
                && lines[2].contains("— completed"),
            "{}",
            lines[2]
        );
        assert!(lines[21].starts_with("Earlier: before="), "{}", lines[21]);
        assert!(lines[21].ends_with(&format!("Open one: open={calling}.")));

        let before = lines[21]
            .strip_prefix("Earlier: before=")
            .and_then(|rest| rest.split('.').next())
            .unwrap()
            .to_owned();
        let (earlier, error) = recall(&session, "main", json!({ "before": before })).await;
        assert!(!error, "{earlier}");
        let earlier_lines: Vec<_> = earlier.lines().collect();
        assert_eq!(earlier_lines.len(), 4, "{earlier}");
        assert!(earlier_lines[2].contains("task 0"), "{earlier}");
        assert_eq!(earlier_lines[3], "Nothing earlier.", "{earlier}");
        agent.close().await.unwrap();
    })
    .await
    .expect("turn index flow");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
// §14 1.
async fn a_turn_is_indexed_by_its_first_user_text_not_by_the_block_that_came_first() {
    tokio::time::timeout(Duration::from_secs(60), async {
        let agent = agent().await;
        let session = open(&agent).await;
        // A Prompt source writes a rule as an Append input between turns; it
        // waits as pending until the next driven input consumes it (§3.6).
        session
            .input(
                InputMessageData {
                    blocks: vec![context_block::raw(
                        context_block::element(
                            "dim-rule",
                            &[
                                ("source", "AGENTS.md"),
                                ("scope", "branch"),
                                ("op", "replace"),
                            ],
                            "Use pnpm, not npm.",
                        ),
                        Some("rule".into()),
                    )],
                    mode: Some(InputMode::Append),
                    ..InputMessageData::default()
                },
                "main",
            )
            .await
            .unwrap();
        session
            .submit("please deploy to staging".into(), InputMode::Queue, "main")
            .await
            .unwrap();
        session.idle("main").await.unwrap();
        let events = session.history("main", None).await.unwrap();
        let turn = events
            .iter()
            .find(|event| matches!(event.kind, EventKind::TurnStart(_)))
            .expect("a turn")
            .id
            .clone();

        let (page, error) = recall(&session, "main", json!({})).await;
        assert!(!error, "{page}");
        let line = line_of(&page, &turn);
        assert!(
            line.contains("turn: user \"please deploy to staging\""),
            "{line}"
        );
        assert!(!line.contains("dim-rule"), "{line}");

        // The rule is still told as its own line inside the turn.
        let (page, error) = recall(&session, "main", json!({ "open": turn })).await;
        assert!(!error, "{page}");
        assert!(
            page.contains("dim-rule source=AGENTS.md scope=branch op=replace (current)"),
            "{page}"
        );
        assert!(page.contains("user: please deploy to staging"), "{page}");
        agent.close().await.unwrap();
    })
    .await
    .expect("turn index input flow");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
// §14 2.
async fn a_turn_tells_its_input_its_replies_its_calls_with_results_and_an_interrupt_in_order() {
    tokio::time::timeout(Duration::from_secs(60), async {
        let agent = agent().await;
        let session = open(&agent).await;
        session
            .submit(
                "Use the staging bucket, never prod.".into(),
                InputMode::Queue,
                "main",
            )
            .await
            .unwrap();
        session.idle("main").await.unwrap();
        session
            .submit(
                r#"tool:echo {"text":"ok done"}"#.into(),
                InputMode::Queue,
                "main",
            )
            .await
            .unwrap();
        session.idle("main").await.unwrap();
        let events = session.history("main", None).await.unwrap();
        let turns: Vec<_> = events
            .iter()
            .filter(|event| matches!(event.kind, EventKind::TurnStart(_)))
            .map(|event| event.id.clone())
            .collect();

        let (page, error) = recall(&session, "main", json!({ "open": turns[0] })).await;
        assert!(!error, "{page}");
        let lines: Vec<_> = page.lines().collect();
        assert!(
            lines[1].ends_with(" user: Use the staging bucket, never prod."),
            "{page}"
        );
        assert!(
            lines[2].ends_with(" you: Echo: Use the staging bucket, never prod."),
            "{page}"
        );
        assert!(
            lines
                .last()
                .unwrap()
                .starts_with("End of turn. Open one: open=")
        );

        let (page, error) = recall(&session, "main", json!({ "open": turns[1] })).await;
        assert!(!error, "{page}");
        let lines: Vec<_> = page.lines().collect();
        assert!(
            lines[1].ends_with(r#" user: tool:echo {"text":"ok done"}"#),
            "{page}"
        );
        assert!(
            lines[2].contains(" you called echo text=\"ok done\" → ok: \"ok done\""),
            "{page}"
        );
        assert!(lines[3].ends_with(" you: Tool result: ok done"), "{page}");

        session
            .submit(
                r#"tool:wait {"millis":60000}"#.into(),
                InputMode::Queue,
                "main",
            )
            .await
            .unwrap();
        wait_for_events(&session, "main", "the wait call starts", |events| {
            events.iter().any(
                |event| matches!(&event.kind, EventKind::ToolUseStart(data) if data.name == "wait"),
            )
        })
        .await;
        session.stop("main", "user stop".into()).await.unwrap();
        session.idle("main").await.unwrap();
        let events = session.history("main", None).await.unwrap();
        let interrupted = last_turn(&events);
        let (page, error) = recall(&session, "main", json!({ "open": interrupted })).await;
        assert!(!error, "{page}");
        let lines: Vec<_> = page.lines().collect();
        assert!(
            lines[1].ends_with(r#" user: tool:wait {"millis":60000}"#),
            "{page}"
        );
        assert!(
            lines[2].contains(" you called wait millis=60000 → "),
            "{page}"
        );
        assert!(
            lines[3].ends_with(" the user interrupted the turn"),
            "{page}"
        );
        agent.close().await.unwrap();
    })
    .await
    .expect("a turn flow");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
// §14 3.
async fn one_thing_shows_its_full_input_and_result_paged_by_characters() {
    tokio::time::timeout(Duration::from_secs(60), async {
        let agent = agent().await;
        let session = open(&agent).await;
        let long: String = std::iter::repeat_n("报告", 5_000).collect();
        session
            .submit(
                format!("tool:echo {}", json!({"text": long})),
                InputMode::Queue,
                "main",
            )
            .await
            .unwrap();
        session.idle("main").await.unwrap();
        let events = session.history("main", None).await.unwrap();
        let call = events
            .iter()
            .find_map(|event| match &event.kind {
                EventKind::ToolUseStart(_) => Some(event.id.clone()),
                _ => None,
            })
            .unwrap();
        let mut offset = 0;
        let mut whole = String::new();
        loop {
            let (page, error) =
                recall(&session, "main", json!({ "open": call, "offset": offset })).await;
            assert!(!error, "{page}");
            let body = page
                .strip_suffix("End.")
                .or_else(|| page.rsplit_once("Continue: open=").map(|(body, _)| body))
                .unwrap()
                .strip_suffix('\n')
                .unwrap();
            whole.push_str(body);
            match page.lines().last().unwrap() {
                "End." => break,
                last => {
                    let next: usize = last
                        .strip_prefix(&format!("Continue: open={call} offset="))
                        .and_then(|rest| rest.strip_suffix('.'))
                        .unwrap_or_else(|| panic!("unexpected last line {last}"))
                        .parse()
                        .unwrap();
                    assert!(next > offset);
                    offset = next;
                }
            }
        }
        assert_eq!(
            whole.matches("报告").count(),
            10_000,
            "the input and the result are whole"
        );

        let blocks = vec![
            context_block::raw("<dim-rule>\nfirst rule\n</dim-rule>".into(), None),
            context_block::raw("<dim-state>\nsecond state\n</dim-state>".into(), None),
            context_block::raw("third body".into(), None),
        ];
        session
            .input(
                InputMessageData {
                    blocks,
                    ..InputMessageData::text("carry three blocks", Some(InputMode::Queue))
                },
                "main",
            )
            .await
            .unwrap();
        session.idle("main").await.unwrap();
        let events = session.history("main", None).await.unwrap();
        let input = id_of_input(&events, "carry three blocks");
        let (page, error) = recall(&session, "main", json!({ "open": input })).await;
        assert!(!error, "{page}");
        assert!(page.contains("user\n\ncarry three blocks"), "{page}");
        let rule = page.find("<dim-rule>").expect("the rule block");
        let state = page.find("<dim-state>").expect("the state block");
        let third = page.find("third body").expect("the untagged block");
        assert!(page.find("carry three blocks").unwrap() < rule);
        assert!(rule < state && state < third, "{page}");
        agent.close().await.unwrap();
    })
    .await
    .expect("one thing flow");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
// §14 4.
async fn only_the_kinds_of_the_allowlist_are_told_and_nothing_in_context_is_missed() {
    tokio::time::timeout(Duration::from_secs(60), async {
        let agent = agent().await;
        let session = open(&agent).await;
        session
            .input(
                InputMessageData {
                    blocks: vec![
                        context_block::raw(
                            context_block::element(
                                "dim-rule",
                                &[("source", "AGENTS.md"), ("scope", "branch"), ("op", "replace")],
                                "Use pnpm, not npm.",
                            ),
                            Some("rule".into()),
                        ),
                        context_block::raw(
                            context_block::element(
                                "dim-state",
                                &[("source", "AGENTS.md")],
                                "state body",
                            ),
                            None,
                        ),
                        context_block::raw(
                            context_block::element(
                                "dim-notification",
                                &[("task", "b1")],
                                "background done",
                            ),
                            None,
                        ),
                        context_block::retire("<dim-ended>old</dim-ended>".into(), "rule".into()),
                    ],
                    ..InputMessageData::text("read the rules", Some(InputMode::Queue))
                },
                "main",
            )
            .await
            .unwrap();
        session.idle("main").await.unwrap();
        let events = session.history("main", None).await.unwrap();
        let narrative = Narrative::new(&events);

        for event in &events {
            let told = narrative.tells(&event.id);
            let listed = matches!(
                event.kind,
                EventKind::TurnStart(_)
                    | EventKind::TurnEnd(_)
                    | EventKind::InputMessage(_)
                    | EventKind::InputConsumed(_)
                    | EventKind::ModelStart(_)
                    | EventKind::Text(_)
                    | EventKind::ModelContent(_)
                    | EventKind::ModelOutputEnd(_)
                    | EventKind::ToolUseStart(_)
                    | EventKind::ToolUse(_)
                    | EventKind::ToolResult(_)
                    | EventKind::ApprovalRequest(_)
                    | EventKind::ApprovalRespond(_)
                    | EventKind::ApprovalCancelled(_)
                    | EventKind::ExecStarted(_)
                    | EventKind::ExecEnded(_)
                    | EventKind::CollaborationRequest(_)
                    | EventKind::CollaborationMessage(_)
                    | EventKind::CollaborationResult(_)
                    | EventKind::BranchCreate(_)
                    | EventKind::BranchStart(_)
                    | EventKind::StopInterrupt(_)
                    | EventKind::PermissionChanged(_)
                    | EventKind::ContextCompaction(_)
                    | EventKind::SessionForked(_)
            );
            if listed && !matches!(event.kind, EventKind::InputMessage(_) | EventKind::TurnStart(_) | EventKind::TurnEnd(_) | EventKind::BranchStart(_) | EventKind::ContextCompaction(_) | EventKind::ApprovalRequest(_)) {
                assert!(told, "{} is not told", event.kind.key());
            }
            if !listed {
                assert!(!told, "{} is told", event.kind.key());
            }
        }
        for key in ["context/prepare", "model/call", "input/applied"] {
            let event = events
                .iter()
                .find(|event| event.kind.key() == key)
                .unwrap_or_else(|| panic!("the live history holds no {key}"));
            assert!(!narrative.tells(&event.id), "{key} is told");
        }
        for entry in project_history(&events, "main").unwrap() {
            assert!(
                narrative.tells(&entry.event_id),
                "context entry {} comes from an untold event",
                entry.id
            );
        }

        let input = id_of_input(&events, "read the rules");
        let (page, error) = recall(&session, "main", json!({ "open": input })).await;
        assert!(!error, "{page}");
        let lines: Vec<_> = page.lines().collect();
        assert!(lines[0].ends_with(&format!(" {input} user")), "{page}");
        assert_eq!(
            &lines[2..lines.len() - 1],
            [
                "read the rules",
                "",
                "<dim-rule source=\"AGENTS.md\" scope=\"branch\" op=\"replace\">",
                "Use pnpm, not npm.",
                "</dim-rule>",
                "",
                "<dim-state source=\"AGENTS.md\">",
                "state body",
                "</dim-state>",
                "",
                "<dim-notification task=\"b1\">",
                "background done",
                "</dim-notification>",
                "",
                "<dim-ended>old</dim-ended> (ended)",
            ],
            "{page}"
        );
        assert_eq!(lines.last(), Some(&"End."));

        let turn = last_turn(&events);
        let (frame, error) = recall(&session, "main", json!({ "open": turn })).await;
        assert!(!error, "{frame}");
        let blocks = line_of(&frame, &input)
            .splitn(3, ' ')
            .nth(2)
            .unwrap()
            .strip_prefix("user: read the rules + ")
            .expect("the input's own line");
        assert!(
            blocks.starts_with(
                "dim-rule source=AGENTS.md scope=branch op=replace, dim-state source=AGENTS.md, dim-notification task=b1, dim-ended"
            ),
            "{blocks}"
        );
        assert_eq!(blocks.chars().count(), 120, "{blocks}");
        assert!(blocks.ends_with('…'), "{blocks}");
        assert_eq!(
            frame
                .lines()
                .filter(|line| line.split(' ').nth(1) == Some(input.as_str()))
                .count(),
            1,
            "one input with several blocks is one entry"
        );
        agent.close().await.unwrap();
    })
    .await
    .expect("selection flow");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
// §14 6.
async fn search_finds_earlier_words_in_any_order_and_case_and_a_word_deep_in_a_result() {
    tokio::time::timeout(Duration::from_secs(60), async {
        let agent = agent().await;
        let session = open(&agent).await;
        session
            .submit(
                "Deploy the staging build today".into(),
                InputMode::Queue,
                "main",
            )
            .await
            .unwrap();
        session.idle("main").await.unwrap();
        let deep = format!(
            "{} needle-in-the-haystack {}",
            "x".repeat(3_000),
            "y".repeat(3_000)
        );
        session
            .submit(
                format!("tool:echo {}", json!({"text": deep})),
                InputMode::Queue,
                "main",
            )
            .await
            .unwrap();
        session.idle("main").await.unwrap();
        session
            .submit("unrelated words".into(), InputMode::Queue, "main")
            .await
            .unwrap();
        session.idle("main").await.unwrap();

        let (page, error) = recall(&session, "main", json!({ "about": "STAGING deploy" })).await;
        assert!(!error, "{page}");
        assert!(
            page.contains(" user: Deploy the staging build today"),
            "{page}"
        );
        assert_eq!(page.lines().last().unwrap(), "No more.");

        let (page, error) = recall(
            &session,
            "main",
            json!({ "about": "needle-in-the-haystack" }),
        )
        .await;
        assert!(!error, "{page}");
        assert!(page.contains("you called echo text="), "{page}");

        let (page, error) =
            recall(&session, "main", json!({ "tool": "nothing-called-this" })).await;
        assert!(!error, "a search with no match is no failed call: {page}");
        assert_eq!(page, "Nothing matches tool nothing-called-this.");

        let (page, error) = recall(&session, "main", json!({ "tool": "echo" })).await;
        assert!(!error, "{page}");
        assert!(page.contains(" you called echo "), "{page}");
        assert!(
            page.lines().all(|line| line.contains("you called echo")
                || line.ends_with("(UTC)")
                || line.starts_with("No more.")),
            "{page}"
        );
        agent.close().await.unwrap();
    })
    .await
    .expect("search flow");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
// §14 8.
async fn a_child_branch_opens_inherited_ids_and_tells_its_fork_and_the_cut_parent_turn() {
    tokio::time::timeout(Duration::from_secs(60), async {
        let agent = agent().await;
        let session = open(&agent).await;
        session
            .submit("first parent task".into(), InputMode::Queue, "main")
            .await
            .unwrap();
        session.idle("main").await.unwrap();
        session
            .submit(
                r#"tool:branch {"action":"create","prompt":"investigate the logs"}"#.into(),
                InputMode::Queue,
                "main",
            )
            .await
            .unwrap();
        session.idle("main").await.unwrap();
        let child = session
            .branches()
            .into_iter()
            .find(|info| info.parent.as_deref() == Some("main"))
            .expect("the child branch")
            .id;
        session.idle(&child).await.unwrap();

        let inherited = {
            let events = session.history("main", None).await.unwrap();
            id_of_input(&events, "first parent task")
        };
        let (page, error) = recall(&session, &child, json!({ "open": inherited })).await;
        assert!(!error, "{page}");
        assert!(page.contains("user\n\nfirst parent task\nEnd."), "{page}");

        let (page, error) = recall(&session, &child, json!({})).await;
        assert!(!error, "{page}");
        let lines: Vec<_> = page.lines().collect();
        assert!(lines[1].ends_with("in progress"), "{}", lines[1]);
        assert!(
            lines
                .iter()
                .any(|line| line.contains(" this line was opened by main")),
            "{page}"
        );
        assert!(
            lines
                .iter()
                .any(|line| line.ends_with(" continued on the parent line")),
            "{page}"
        );

        let (page, error) = recall(&session, &child, json!({ "open": "not-a-line" })).await;
        assert!(error);
        assert_eq!(page, "No turn or line has id not-a-line.");

        // The opening input on the child is its identity block.
        let (page, error) = recall(&session, &child, json!({})).await;
        assert!(!error, "{page}");
        assert!(
            page.contains("dim-branch-line branch=") && page.contains("parent=main source=tool"),
            "{page}"
        );

        // A message from main reaches the child as a line write. Which turn
        // reads it depends on when it is applied, so a search tells its line
        // wherever it falls (§3.6).
        session
            .submit(
                format!(
                    r#"tool:branch {{"action":"message","to":"{child}","content":"status please"}}"#
                ),
                InputMode::Queue,
                "main",
            )
            .await
            .unwrap();
        session.idle("main").await.unwrap();
        session.idle(&child).await.unwrap();
        let (page, error) = recall(&session, &child, json!({ "about": "status please" })).await;
        assert!(!error, "{page}");
        assert!(page.contains(" line main: \"status please\""), "{page}");
        assert_eq!(page.lines().last(), Some("No more."), "{page}");

        // The child ends itself, and main hears its final message.
        session
            .submit(
                r#"tool:branch {"action":"end","content":"the logs are clean"}"#.into(),
                InputMode::Queue,
                &child,
            )
            .await
            .unwrap();
        wait_for_events(
            &session,
            "main",
            "main receives the line's final message",
            |events| {
                events.iter().any(|event| {
                    matches!(&event.kind, EventKind::InputMessage(data)
                        if data.blocks.iter().any(|block| format!("{block:?}").contains("the logs are clean")))
                })
            },
        )
        .await;
        session.idle("main").await.unwrap();
        // Both writes land on main as its last turn is running or after it, so
        // a search tells each line wherever it falls (§3.6).
        let (page, error) = recall(&session, "main", json!({ "about": "the logs are clean" })).await;
        assert!(!error, "{page}");
        let last = format!(" line {child}'s final message: \"the logs are clean\"");
        assert!(page.contains(&last), "{page}");
        let (page, error) = recall(
            &session,
            "main",
            json!({ "about": "without a message to you" }),
        )
        .await;
        assert!(!error, "{page}");
        assert!(
            page.contains(&format!(" line {child}'s turn ended")),
            "{page}"
        );
        agent.close().await.unwrap();
    })
    .await
    .expect("branch fork flow");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
// §14 9.
async fn a_recall_call_writes_only_the_events_any_tool_call_writes() {
    tokio::time::timeout(Duration::from_secs(60), async {
        let agent = agent().await;
        let session = open(&agent).await;
        session
            .submit(
                r#"tool:echo {"text":"ok"}"#.into(),
                InputMode::Queue,
                "main",
            )
            .await
            .unwrap();
        session.idle("main").await.unwrap();
        let events = session.history("main", None).await.unwrap();
        let counted: std::collections::HashSet<_> =
            events.iter().map(|event| event.kind.key()).collect();
        let before = events.len();
        let (page, error) = recall(&session, "main", json!({})).await;
        assert!(!error, "{page}");
        let events = session.history("main", None).await.unwrap();
        for event in &events[before..] {
            assert!(
                counted.contains(event.kind.key()),
                "a recall call wrote {}, which no tool call wrote",
                event.kind.key()
            );
        }
        agent.close().await.unwrap();
    })
    .await
    .expect("no side effects flow");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
// §14 10.
async fn the_read_only_preset_runs_recall_without_an_approval_request() {
    tokio::time::timeout(Duration::from_secs(60), async {
        let agent = agent().await;
        let session = agent
            .create_session(SessionConfig {
                permission_policy: PermissionPolicy {
                    preset: PermissionPreset::ReadOnly,
                    ..PermissionPolicy::default()
                },
                tools: None,
                ..SessionConfig::default()
            })
            .await
            .unwrap();
        session
            .submit("look back".into(), InputMode::Queue, "main")
            .await
            .unwrap();
        session.idle("main").await.unwrap();
        let (page, error) = recall(&session, "main", json!({})).await;
        assert!(!error, "{page}");
        assert!(page.contains("look back"), "{page}");
        let events = session.history("main", None).await.unwrap();
        assert!(
            !events
                .iter()
                .any(|event| matches!(&event.kind, EventKind::ApprovalRequest(data) if data.call.name == RECALL_TOOL)),
            "recall asked for an approval"
        );
        agent.close().await.unwrap();
    })
    .await
    .expect("permission flow");
}

/// 2026-10-17T00:00:00Z, the day the recorded histories below start on.
const DAY: u64 = 1_792_195_200_000;

fn at(hour: u64, minute: u64) -> u64 {
    DAY + hour * 3_600_000 + minute * 60_000
}

/// A history recorded as the events their producers write, for the shapes the
/// local-execution profile cannot produce on demand.
struct Recorded {
    events: Vec<Arc<AgentEvent>>,
    time: u64,
}

impl Recorded {
    fn new() -> Self {
        Self {
            events: Vec::new(),
            time: at(0, 0),
        }
    }

    /// Puts the clock the events pushed next are written at.
    fn at(&mut self, hour: u64, minute: u64) {
        self.time = at(hour, minute);
    }

    fn push(&mut self, kind: EventKind) -> String {
        let id = format!("e{}", self.events.len() + 1);
        self.events.push(Arc::new(AgentEvent {
            id: id.clone(),
            sequence: self.events.len() as u64 + 1,
            create_at: self.time,
            tags: None,
            kind,
        }));
        id
    }

    fn answer(&self, turn: &str, arguments: Value) -> (String, bool) {
        let narrative = self.narrative();
        // A history the Loop could not have written proves nothing, and
        // nothing context assembly places may go untold (§14 flow 4).
        let placed =
            project_history(&self.events, "main").expect("a history context assembly projects");
        for entry in placed {
            assert!(
                narrative.tells(&entry.event_id),
                "context entry {} comes from an untold event",
                entry.id
            );
        }
        let result = narrative.answer(turn, arguments.as_object().expect("an arguments object"));
        let text = match &result.content {
            ToolContent::Text(text) => text.clone(),
            other => panic!("expected text, got {other:?}"),
        };
        (text, result.is_error.unwrap_or(false))
    }

    fn narrative(&self) -> Narrative<'_> {
        Narrative::new(&self.events)
    }
}

fn input_message(text: &str) -> EventKind {
    EventKind::InputMessage(InputMessageData::text(text, None))
}

/// A message one line sends another, as the branch tool writes it (branches §6).
fn line_message(
    from: &str,
    to: &str,
    text: &str,
    mode: InputMode,
    final_message: bool,
) -> EventKind {
    let block = if final_message {
        context_block::raw(
            context_block::element(
                "dim-branch-message",
                &[("from", from), ("to", to), ("final", "true")],
                &format!("{text}\n\nLine {from} ends with this message."),
            ),
            None,
        )
    } else {
        context_block::raw(
            context_block::element("dim-branch-message", &[("from", from), ("to", to)], text),
            None,
        )
    };
    EventKind::InputMessage(InputMessageData {
        blocks: vec![block],
        mode: Some(mode),
        ..InputMessageData::default()
    })
}

/// A line's turn ending without a message to its parent (branches §12.1).
fn line_idle(branch: &str, status: &str) -> EventKind {
    let body = format!(
        "Line {branch}'s turn ended ({status}) without a message to you, and {branch} is waiting. Read it if you need its state; message it with mode \"steer\" to give it more work, or leave it."
    );
    EventKind::InputMessage(InputMessageData {
        blocks: vec![context_block::raw(
            context_block::element(
                "dim-branch-idle",
                &[("branch", branch), ("status", status)],
                &body,
            ),
            None,
        )],
        mode: Some(InputMode::Steer),
        ..InputMessageData::default()
    })
}

/// The roster of open lines, with the ended notice in the same input when a
/// line was ended from outside (branches §12.1).
fn roster(open: &[(&str, &str)], ended: Option<&str>) -> EventKind {
    let key = Some("branch:line".to_owned());
    let block = if open.is_empty() {
        context_block::retire(
            context_block::element(
                "dim-branch-lines",
                &[("branch", "main")],
                "All lines you opened have ended.",
            ),
            "branch:line".to_owned(),
        )
    } else {
        let listed = open
            .iter()
            .map(|(line, purpose)| format!("- {line}: {purpose}"))
            .collect::<Vec<_>>()
            .join("\n");
        context_block::raw(
            context_block::element(
                "dim-branch-lines",
                &[("branch", "main")],
                &format!(
                    "Lines you opened that are still open. Each is you, working in parallel from the point you opened it; it does not see your work after that point unless you message it.\n{listed}\nThey message you when they find something, need you, or end. Use the branch tool to message or read them, and the `wait` tool to wait for them; end the wait when your lines have nothing left to tell you."
                ),
            ),
            key,
        )
    };
    let mut blocks = vec![block];
    if let Some(branch) = ended {
        blocks.push(context_block::raw(
            context_block::element(
                "dim-branch-ended",
                &[("branch", branch)],
                &format!("Line {branch} was ended from outside its own line, by the user or a host operation. It no longer receives messages; read it if you need what it did."),
            ),
            None,
        ));
    }
    EventKind::InputMessage(InputMessageData {
        blocks,
        mode: Some(if ended.is_some() {
            InputMode::Steer
        } else {
            InputMode::Append
        }),
        ..InputMessageData::default()
    })
}

/// A child's opening input: its identity block under `branch:line`.
fn identity(line: &str, parent: &str) -> EventKind {
    let body = format!(
        "You are line {line} of this session. Line {parent} opened you at this point of its history to work in parallel with it.\n\nYour purpose, as {parent} wrote it:\ninvestigate the logs"
    );
    EventKind::InputMessage(InputMessageData {
        blocks: vec![context_block::raw(
            context_block::element(
                "dim-branch-line",
                &[("branch", line), ("parent", parent), ("source", "tool")],
                &body,
            ),
            Some("branch:line".to_owned()),
        )],
        mode: Some(InputMode::Queue),
        ..InputMessageData::default()
    })
}

/// The `branch/create` the tool writes on the line that opened a child.
fn branch_create(turn: &str, request: &str, call: &str, branch: &str, prompt: &str) -> EventKind {
    EventKind::BranchCreate(BranchCreateData {
        at: BranchCreateAt::Create {},
        source: BranchSource::Tool {
            turn_id: turn.into(),
            request_id: request.into(),
            call_id: call.into(),
        },
        branch: branch.into(),
        prompt: prompt.into(),
    })
}

fn branch_end(turn: &str, request: &str, call: &str) -> EventKind {
    EventKind::BranchEnd(BranchEndData {
        source: BranchSource::Tool {
            turn_id: turn.into(),
            request_id: request.into(),
            call_id: call.into(),
        },
        ancestor: None,
    })
}

fn branch_ended(branch: &str, source: BranchSource) -> EventKind {
    EventKind::BranchEnded(BranchEndedData {
        branch: branch.into(),
        end_event: 12,
        source,
        ancestor: None,
    })
}

/// A turn opened by one input the line received.
fn received_turn(recorded: &mut Recorded, turn: &str, write: EventKind, reply: &str) -> String {
    let input = recorded.push(write);
    recorded.push(turn_start(turn));
    recorded.push(consumed(&input));
    recorded.push(model_start(turn, turn));
    recorded.push(text_delta(turn, turn, reply));
    recorded.push(model_end(turn, turn, output_completed()));
    recorded.push(turn_ends(turn, completed_turn()));
    input
}

/// An input whose one keyed block says `body`, as a Prompt source writes one.
fn keyed_input(text: &str, key: &str, body: &str) -> EventKind {
    EventKind::InputMessage(InputMessageData {
        blocks: vec![context_block::raw(
            format!("<dim-rule>\n{body}\n</dim-rule>"),
            Some(key.into()),
        )],
        ..InputMessageData::text(text, None)
    })
}

fn steer(text: &str) -> EventKind {
    EventKind::InputMessage(InputMessageData::text(text, Some(InputMode::Steer)))
}

/// A `dim-state` input Prompt writes as an Append when a turn's end changes
/// context (`prompt.rs`, `context_changes`).
fn state_change(source: &str, body: &str) -> EventKind {
    EventKind::InputMessage(InputMessageData {
        blocks: vec![context_block::raw(
            format!(
                "<dim-state source=\"{}\">{body}</dim-state>",
                context_block::attr(source)
            ),
            Some(format!(
                "state:{source}:{{\"kind\":\"branch\",\"timeline\":\"main\"}}"
            )),
        )],
        mode: Some(InputMode::Append),
        ..InputMessageData::default()
    })
}

/// Goal's continuation input (`goal.ts`, `writes.continuation`): a queued
/// `dim-goal` block with no user content and no line write.
fn continuation() -> EventKind {
    EventKind::InputMessage(InputMessageData {
        submission_id: Some("goal:continuation".into()),
        blocks: vec![context_block::raw(
            context_block::element(
                "dim-goal",
                &[
                    ("kind", "continuation"),
                    ("count", "1"),
                    ("limit", "20"),
                    ("after", "12"),
                    ("branch", "main"),
                ],
                "Continue working toward the active goal for branch main. If it is completed or blocked, call goal_update. If you are working on a delegated sub-task started from branch main, do not call goal_update.",
            ),
            None,
        )],
        mode: Some(InputMode::Queue),
        ..InputMessageData::default()
    })
}

fn consumed(input: &str) -> EventKind {
    EventKind::InputConsumed(InputConsumedData {
        input_id: input.into(),
    })
}

fn turn_start(turn: &str) -> EventKind {
    EventKind::TurnStart(TurnStartData {
        turn_id: turn.into(),
    })
}

fn turn_ends(turn: &str, outcome: TurnOutcome) -> EventKind {
    EventKind::TurnEnd(TurnEndData {
        turn_id: turn.into(),
        outcome,
    })
}

fn completed_turn() -> TurnOutcome {
    TurnOutcome::Completed {
        reason: "end_turn".into(),
    }
}

fn model_start(turn: &str, request: &str) -> EventKind {
    EventKind::ModelStart(StepData {
        turn_id: turn.into(),
        request_id: request.into(),
    })
}

fn text_delta(turn: &str, request: &str, delta: &str) -> EventKind {
    EventKind::Text(TextData {
        turn_id: turn.into(),
        request_id: request.into(),
        delta: delta.into(),
    })
}

fn thought(turn: &str, request: &str, delta: &str) -> EventKind {
    EventKind::Thinking(ThinkingData {
        turn_id: turn.into(),
        request_id: request.into(),
        delta: delta.into(),
    })
}

fn model_end(turn: &str, request: &str, outcome: ModelOutputOutcome) -> EventKind {
    EventKind::ModelOutputEnd(ModelOutputEndData {
        turn_id: turn.into(),
        request_id: request.into(),
        outcome,
    })
}

fn output_completed() -> ModelOutputOutcome {
    ModelOutputOutcome::Completed {
        finish_reason: FinishReason::EndTurn,
    }
}

fn output_cancelled() -> ModelOutputOutcome {
    ModelOutputOutcome::Cancelled {
        reason: "stopped".into(),
    }
}

fn call_start(turn: &str, request: &str, call: &str, name: &str) -> EventKind {
    EventKind::ToolUseStart(ToolUseStartData {
        turn_id: turn.into(),
        request_id: request.into(),
        call_id: call.into(),
        name: name.into(),
    })
}

fn call_input(turn: &str, request: &str, call: &str, name: &str, input: Value) -> EventKind {
    EventKind::ToolUse(ToolUseData {
        turn_id: turn.into(),
        request_id: request.into(),
        call: ToolCall {
            id: call.into(),
            name: name.into(),
            input: serde_json::from_value(input).expect("an input object"),
        },
    })
}

/// One `step/tool-result`; `dispatched` is the `call_event_id` of the batch.
fn results(
    turn: &str,
    request: &str,
    dispatched: Option<&str>,
    items: &[(&str, &str, bool)],
) -> EventKind {
    EventKind::ToolResult(ToolResultData {
        turn_id: turn.into(),
        request_id: request.into(),
        call_event_id: dispatched.map(str::to_owned),
        results: items
            .iter()
            .map(|(call, text, error)| ToolCallResult {
                call_id: (*call).into(),
                result: if *error {
                    ToolResult::error(*text)
                } else {
                    ToolResult::text(*text)
                },
            })
            .collect(),
    })
}

fn interrupted() -> EventKind {
    EventKind::StopInterrupt(StopInterruptData {
        reason: "user stop".into(),
        scope: InterruptScope::Turn,
    })
}

fn permission(preset: PermissionPreset) -> EventKind {
    EventKind::PermissionChanged(PermissionChangedData {
        policy: PermissionPolicy {
            preset,
            ..PermissionPolicy::default()
        },
    })
}

fn compaction(through: u64) -> EventKind {
    EventKind::ContextCompaction(ContextCompactionData {
        turn_id: None,
        request_id: None,
        command: None,
        start: None,
        through,
        outcome: ContextCompactionResult::Completed {
            retain_from: "e1".into(),
            replacement: vec![history_summary()],
            strategy: "summary".into(),
            diagnostics: Box::new(
                serde_json::from_value(json!({
                    "reason": "auto", "trigger": "threshold", "estimatedInputTokens": 1,
                    "entriesReplaced": 1, "modelCallCount": 1, "durationMs": 1,
                    "strategy": {"kind": "test"}
                }))
                .unwrap(),
            ),
            context_baseline: Vec::new(),
            replacement_parts: None,
            pinned_inputs: None,
            digest_entries: None,
        },
    })
}

/// A compaction that replaced nothing: it skipped (§3.2).
fn skipped_compaction(through: u64) -> EventKind {
    EventKind::ContextCompaction(ContextCompactionData {
        turn_id: None,
        request_id: None,
        command: None,
        start: None,
        through,
        outcome: ContextCompactionResult::Skipped {
            reason: "nothing to compact".into(),
            diagnostics: Box::new(
                serde_json::from_value(json!({
                    "reason": "auto", "trigger": "threshold", "estimatedInputTokens": 1,
                    "entriesReplaced": 0, "modelCallCount": 0, "durationMs": 1,
                    "strategy": {"kind": "test"}
                }))
                .unwrap(),
            ),
        },
    })
}

fn history_summary() -> Message {
    Message::text(
        Role::User,
        "<dim-summary>\nrecorded background\n</dim-summary>",
    )
}

fn branch_start(parent: Option<&str>) -> EventKind {
    EventKind::BranchStart(BranchStartData {
        parent: parent.map(str::to_owned),
        create_event: parent.map(|_| 1),
    })
}

fn session_forked() -> EventKind {
    EventKind::SessionForked(SessionForkedData {})
}

fn output_tool_use() -> ModelOutputOutcome {
    ModelOutputOutcome::Completed {
        finish_reason: FinishReason::ToolUse,
    }
}

#[test]
// §14 2.
fn a_permission_change_and_an_interrupt_are_lines_of_their_own() {
    let mut recorded = Recorded::new();
    recorded.at(4, 0);
    let input = recorded.push(input_message("go"));
    let turn = recorded.push(turn_start("t1"));
    recorded.push(consumed(&input));
    recorded.at(4, 5);
    let changed = recorded.push(permission(PermissionPreset::FullAccess));
    recorded.at(4, 6);
    let stopped = recorded.push(interrupted());
    recorded.push(turn_ends(
        "t1",
        TurnOutcome::Cancelled {
            reason: "user stop".into(),
        },
    ));

    let (page, error) = recorded.answer("t1", json!({ "open": turn }));
    assert!(!error, "{page}");
    assert_eq!(
        page.lines().collect::<Vec<_>>(),
        [
            "2026-10-17 (UTC)",
            &format!("04:00 {input} user: go"),
            &format!("04:05 {changed} permission mode changed to full-access"),
            &format!("04:06 {stopped} the user interrupted the turn"),
            &format!("End of turn. Open one: open={input}."),
        ]
        .map(str::to_owned),
        "{page}"
    );
}

#[test]
// §14 7.
fn every_call_ending_is_folded_onto_its_one_line() {
    let mut recorded = Recorded::new();
    recorded.at(14, 3);
    let input = recorded.push(input_message("go"));
    let turn = recorded.push(turn_start("t1"));
    recorded.push(consumed(&input));

    recorded.at(14, 4);
    recorded.push(model_start("t1", "r1"));
    let denied = recorded.push(call_start("t1", "r1", "c1", "shell"));
    recorded.push(call_input(
        "t1",
        "r1",
        "c1",
        "shell",
        json!({"command": "aws s3 cp report.csv s3://prod-reports/"}),
    ));
    recorded.push(EventKind::ApprovalRequest(ApprovalRequestData {
        approval_id: "a1".into(),
        source: BranchSource::Tool {
            turn_id: "t1".into(),
            request_id: "r1".into(),
            call_id: "c1".into(),
        },
        call: ApprovalCallData {
            name: "shell".into(),
            input: JsonObject::new(),
        },
        reason: "writes outside the workspace".into(),
    }));
    recorded.push(EventKind::ApprovalRespond(ApprovalRespondData {
        approval_id: "a1".into(),
        decision: ApprovalDecision::Deny,
    }));
    recorded.push(model_end("t1", "r1", output_tool_use()));
    recorded.push(results(
        "t1",
        "r1",
        Some("dispatch"),
        &[("c1", "Tool execution denied by user", true)],
    ));

    recorded.at(14, 5);
    let steered = recorded.push(steer("use the staging bucket"));
    recorded.push(consumed(&steered));

    // The output ends before dispatch, so the result carries no
    // `call_event_id`: the call was never run (§3.4).
    recorded.at(14, 6);
    recorded.push(model_start("t1", "r2"));
    let never_ran = recorded.push(call_start("t1", "r2", "c2", "echo"));
    recorded.push(call_input(
        "t1",
        "r2",
        "c2",
        "echo",
        json!({"arg": "value"}),
    ));
    recorded.push(results(
        "t1",
        "r2",
        None,
        &[("c2", "Tool was not executed: the model output ended", true)],
    ));
    recorded.push(model_end("t1", "r2", output_cancelled()));

    recorded.at(14, 7);
    recorded.push(model_start("t1", "r3"));
    let background = recorded.push(call_start("t1", "r3", "c3", "exec"));
    recorded.push(call_input(
        "t1",
        "r3",
        "c3",
        "exec",
        json!({"arg": "value"}),
    ));
    recorded.push(model_end("t1", "r3", output_tool_use()));
    recorded.push(EventKind::ExecStarted(ExecStartedData {
        turn_id: "t1".into(),
        request_id: "r3".into(),
        call_id: "c3".into(),
        task_id: "task-9".into(),
        description: "run the tests".into(),
        output_path: "out.txt".into(),
    }));
    recorded.push(results(
        "t1",
        "r3",
        Some("dispatch"),
        &[("c3", "moved to background task task-9", false)],
    ));
    recorded.at(14, 8);
    recorded.push(EventKind::ExecEnded(ExecEndedData {
        task_id: "task-9".into(),
        status: ExecStatus::Completed,
        result: ToolResult::text("all green"),
    }));

    recorded.at(14, 9);
    recorded.push(model_start("t1", "r4"));
    let delegated = recorded.push(call_start("t1", "r4", "c4", "agent"));
    recorded.push(call_input(
        "t1",
        "r4",
        "c4",
        "agent",
        json!({"arg": "value"}),
    ));
    recorded.push(model_end("t1", "r4", output_tool_use()));
    let request = recorded.push(EventKind::CollaborationRequest(CollaborationRequestData {
        turn_id: "t1".into(),
        request_id: "r4".into(),
        call_id: "c4".into(),
        target: CollaborationTarget::Agent {
            agent_id: "reviewer".into(),
        },
        prompt: "review it".into(),
    }));
    recorded.push(results(
        "t1",
        "r4",
        Some("dispatch"),
        &[("c4", "the sub-agent is working", false)],
    ));
    recorded.push(EventKind::CollaborationResult(CollaborationResultData {
        request_event_id: request,
        agent_id: "reviewer".into(),
        session_id: "s1".into(),
        last_turn_id: None,
        text: "the report".into(),
        outcome: CollaborationOutcome::Completed,
    }));

    recorded.at(14, 10);
    recorded.push(model_start("t1", "r5"));
    let failed = recorded.push(call_start("t1", "r5", "c5", "echo"));
    recorded.push(call_input(
        "t1",
        "r5",
        "c5",
        "echo",
        json!({"arg": "value"}),
    ));
    recorded.push(model_end("t1", "r5", output_tool_use()));
    recorded.push(results(
        "t1",
        "r5",
        Some("dispatch"),
        &[("c5", "it failed", true)],
    ));

    recorded.at(14, 11);
    recorded.push(model_start("t1", "r6"));
    let succeeded = recorded.push(call_start("t1", "r6", "c6", "echo"));
    recorded.push(call_input(
        "t1",
        "r6",
        "c6",
        "echo",
        json!({"arg": "value"}),
    ));
    recorded.push(model_end("t1", "r6", output_tool_use()));
    recorded.at(14, 12);
    recorded.push(results(
        "t1",
        "r6",
        Some("dispatch"),
        &[("c6", "it is fine", false)],
    ));

    // A sub-agent the pinned history leaves working is told by its result
    // until its `collaboration/result` arrives (§3.4).
    recorded.at(14, 13);
    recorded.push(model_start("t1", "r7"));
    let pending = recorded.push(call_start("t1", "r7", "c7", "agent"));
    recorded.push(call_input(
        "t1",
        "r7",
        "c7",
        "agent",
        json!({"arg": "value"}),
    ));
    recorded.push(model_end("t1", "r7", output_tool_use()));
    recorded.push(EventKind::CollaborationRequest(CollaborationRequestData {
        turn_id: "t1".into(),
        request_id: "r7".into(),
        call_id: "c7".into(),
        target: CollaborationTarget::Agent {
            agent_id: "reviewer".into(),
        },
        prompt: "review it again".into(),
    }));
    recorded.push(results(
        "t1",
        "r7",
        Some("dispatch"),
        &[("c7", "the sub-agent is working", false)],
    ));
    recorded.push(turn_ends("t1", completed_turn()));

    let (page, error) = recorded.answer("t9", json!({"open": turn}));
    assert!(!error, "{page}");
    assert_eq!(
        page.lines().collect::<Vec<_>>(),
        [
            "2026-10-17 (UTC)".to_owned(),
            format!("14:03 {input} user: go"),
            format!(
                "14:04 {denied} you called shell command=\"aws s3 cp report.csv s3://prod-reports/\" → denied by the user"
            ),
            format!("14:05 {steered} user (while you were working): use the staging bucket"),
            format!(
                "14:06 {never_ran} you called echo arg=value → not run: \"Tool was not executed: the model output ended\""
            ),
            format!(
                "14:07 {background} you called exec arg=value → ran in background task task-9; finished at 14:08: ok: \"all green\""
            ),
            format!(
                "14:09 {delegated} you called agent arg=value → sub-agent reviewer answered: \"the report\""
            ),
            format!("14:10 {failed} you called echo arg=value → error: \"it failed\""),
            format!("14:11 {succeeded} you called echo arg=value → ok (14:12): \"it is fine\""),
            format!(
                "14:13 {pending} you called agent arg=value → ok: \"the sub-agent is working\""
            ),
            format!("End of turn. Open one: open={input}."),
        ],
        "{page}"
    );

    // `about` searches what opening the call shows: a background task's result
    // is found even though it arrived after the call's own result.
    let (found, error) = recorded.answer("t9", json!({"about": "all green"}));
    assert!(!error, "{found}");
    assert!(
        line_of(&found, &background).contains("all green"),
        "{found}"
    );

    // A cancel of the approval says so, and a call the head ends inside has no
    // result yet.
    let mut cancelled = Recorded::new();
    cancelled.at(9, 0);
    cancelled.push(input_message("go"));
    let turn = cancelled.push(turn_start("t1"));
    cancelled.push(consumed("e1"));
    cancelled.push(model_start("t1", "r1"));
    let cancelled_call = cancelled.push(call_start("t1", "r1", "c1", "shell"));
    cancelled.push(call_input(
        "t1",
        "r1",
        "c1",
        "shell",
        json!({"command": "ls"}),
    ));
    cancelled.push(EventKind::ApprovalRequest(ApprovalRequestData {
        approval_id: "a1".into(),
        source: BranchSource::Tool {
            turn_id: "t1".into(),
            request_id: "r1".into(),
            call_id: "c1".into(),
        },
        call: ApprovalCallData {
            name: "shell".into(),
            input: JsonObject::new(),
        },
        reason: "runs a command".into(),
    }));
    cancelled.push(EventKind::ApprovalCancelled(ApprovalCancelledData {
        approval_id: "a1".into(),
        reason: "the turn stopped".into(),
    }));
    cancelled.push(model_end("t1", "r1", output_tool_use()));
    cancelled.push(results(
        "t1",
        "r1",
        Some("dispatch"),
        &[("c1", "the turn stopped", true)],
    ));
    cancelled.push(model_start("t1", "r2"));
    let writing = cancelled.push(call_start("t1", "r2", "c2", "edit"));
    let (page, error) = cancelled.answer("t1", json!({"open": turn}));
    assert!(!error, "{page}");
    assert!(
        page.contains(&format!(
            "{cancelled_call} you called shell command=ls → approval cancelled: \"the turn stopped\""
        )),
        "{page}"
    );
    assert!(
        page.contains(&format!(
            "{writing} you started calling edit (arguments not received) → no result yet"
        )),
        "{page}"
    );

    // The approval's reason is part of what the call shows, so it is found.
    let (found, error) = cancelled.answer("t1", json!({"about": "turn stopped"}));
    assert!(!error, "{found}");
    assert!(found.contains(&cancelled_call), "{found}");
}

#[test]
// §14 2.
fn a_line_starts_at_the_first_text_that_says_something() {
    let mut recorded = Recorded::new();
    recorded.at(12, 0);
    let input = recorded.push(input_message("go"));
    let turn = recorded.push(turn_start("t1"));
    recorded.push(consumed(&input));
    recorded.push(model_start("t1", "r1"));
    recorded.push(text_delta("t1", "r1", "\n\nDone, the tests pass."));
    recorded.push(model_end("t1", "r1", output_completed()));
    recorded.push(model_start("t1", "r2"));
    let edit = recorded.push(call_start("t1", "r2", "c1", "edit"));
    recorded.push(call_input(
        "t1",
        "r2",
        "c1",
        "edit",
        json!({"path": "a.rs", "old": "fn a()\n{", "new": "fn a() {}"}),
    ));
    recorded.push(model_end("t1", "r2", output_tool_use()));
    recorded.push(results(
        "t1",
        "r2",
        Some("dispatch"),
        &[("c1", "\n\napplied", false)],
    ));
    recorded.push(turn_ends("t1", completed_turn()));

    let (page, error) = recorded.answer("t1", json!({ "open": turn }));
    assert!(!error, "{page}");
    let lines: Vec<_> = page.lines().collect();
    assert!(lines[2].ends_with(" you: Done, the tests pass."), "{page}");
    let call = line_of(&page, &edit);
    // Every key survives: a multi-line value keeps its first line and `…`.
    assert!(call.contains("old=\"fn a()…\""), "{call}");
    assert!(call.contains("new=\"fn a() {}\""), "{call}");
    assert!(call.contains("→ ok: applied"), "{call}");
}

/// A turn with one consumed input and one reply.
fn simple_turn(recorded: &mut Recorded, turn: &str, text: &str, reply: &str) {
    let input = recorded.push(input_message(text));
    recorded.push(turn_start(turn));
    recorded.push(consumed(&input));
    recorded.push(model_start(turn, "r1"));
    recorded.push(text_delta(turn, "r1", reply));
    recorded.push(model_end(turn, "r1", output_completed()));
    recorded.push(turn_ends(turn, completed_turn()));
}

fn bounded(page: &str) {
    for line in page.lines() {
        assert!(
            line.chars().count() <= 600,
            "a line of {} characters: {line}",
            line.chars().count()
        );
    }
}

#[test]
// §14 5.
fn compactions_are_one_line_each_and_ids_taken_before_them_still_open() {
    let mut recorded = Recorded::new();
    recorded.push(branch_start(None));
    recorded.at(13, 0);
    let first = recorded.push(input_message("first task"));
    let first_turn = recorded.push(turn_start("t1"));
    recorded.push(consumed(&first));
    recorded.push(model_start("t1", "r1"));
    recorded.push(text_delta("t1", "r1", "first answer"));
    recorded.push(model_end("t1", "r1", output_completed()));
    recorded.push(turn_ends("t1", completed_turn()));
    recorded.at(13, 10);
    let earlier = recorded.push(compaction(8));
    recorded.at(13, 20);
    simple_turn(&mut recorded, "t2", "second task", "second answer");
    recorded.at(13, 30);
    let latest = recorded.push(compaction(16));
    recorded.at(13, 35);
    let skipped = recorded.push(skipped_compaction(20));
    recorded.at(13, 40);
    simple_turn(&mut recorded, "t3", "third task", "third answer");

    let (page, error) = recorded.answer("t3", json!({}));
    assert!(!error, "{page}");
    {
        let lines: Vec<_> = page.lines().collect();
        assert_eq!(lines[0], "2026-10-17 (UTC)");
        assert!(lines[1].contains("turn: user \"third task\""), "{page}");
        assert_eq!(
            lines[2],
            format!(
                "13:30 {latest} the conversation up to here was summarized (that summary is in your context)"
            )
        );
        assert!(lines[3].contains("turn: user \"second task\""), "{page}");
        assert_eq!(
            lines[4],
            format!("13:10 {earlier} the conversation up to here was summarized")
        );
        assert!(lines[5].contains("turn: user \"first task\""), "{page}");
        assert_eq!(lines.last().copied(), Some("Nothing earlier."));
    }

    // The first turn, and an input from before the first compaction, still open.
    let (thing, error) = recorded.answer("t3", json!({ "open": first }));
    assert!(!error, "{thing}");
    assert!(thing.contains("user\n\nfirst task"), "{thing}");
    let (thing, error) = recorded.answer("t3", json!({ "open": first_turn }));
    assert!(!error, "{thing}");
    assert!(thing.contains("user: first task"), "{thing}");
    assert!(
        thing.ends_with("End of turn. Open one: open=e2."),
        "{thing}"
    );
    let (thing, error) = recorded.answer("t3", json!({ "open": latest }));
    assert!(!error, "{thing}");
    assert!(
        thing.contains(
            "the conversation up to here was summarized (that summary is in your context)"
        ),
        "{thing}"
    );
    assert!(
        !recorded.narrative().tells(&skipped),
        "a compaction that replaced nothing is not told (§3.2)"
    );
}

#[test]
// §14 8.
fn a_copied_history_keeps_its_ids_and_tells_where_the_copy_starts() {
    let mut recorded = Recorded::new();
    recorded.push(branch_start(None));
    recorded.at(10, 0);
    let copied = recorded.push(input_message("copied task"));
    let copied_turn = recorded.push(turn_start("t1"));
    recorded.push(consumed(&copied));
    recorded.push(model_start("t1", "r1"));
    recorded.push(text_delta("t1", "r1", "copied answer"));
    recorded.push(model_end("t1", "r1", output_completed()));
    recorded.push(turn_ends("t1", completed_turn()));
    recorded.at(10, 30);
    let forked = recorded.push(session_forked());
    recorded.at(10, 40);
    simple_turn(&mut recorded, "t2", "own task", "own answer");

    let (page, error) = recorded.answer("t2", json!({}));
    assert!(!error, "{page}");
    let lines: Vec<_> = page.lines().collect();
    assert!(lines[1].contains("turn: user \"own task\""), "{page}");
    assert_eq!(
        lines[2],
        format!("10:30 {forked} this conversation was copied from another session up to here")
    );
    assert!(
        lines[3].contains(&format!("{copied_turn} turn: user \"copied task\"")),
        "{page}"
    );
    assert_eq!(lines.last().copied(), Some("Nothing earlier."));

    let (thing, error) = recorded.answer("t2", json!({ "open": copied }));
    assert!(!error, "{thing}");
    assert!(thing.contains("user\n\ncopied task"), "{thing}");
}

#[test]
// §14 8.
fn a_lines_roster_messages_and_notices_are_told_on_main() {
    let mut recorded = Recorded::new();
    recorded.push(branch_start(None));
    recorded.at(15, 0);
    // t1: main opens two lines. The fold of each `branch/create` writes the
    // roster, and main reads it at the turn's next step.
    let task = recorded.push(input_message("open two lines"));
    recorded.push(turn_start("t1"));
    recorded.push(consumed(&task));
    recorded.push(model_start("t1", "r1"));
    recorded.push(call_start("t1", "r1", "c1", "branch"));
    recorded.push(call_input(
        "t1",
        "r1",
        "c1",
        "branch",
        json!({"action": "create", "prompt": "watch the logs"}),
    ));
    recorded.push(call_start("t1", "r1", "c2", "branch"));
    recorded.push(call_input(
        "t1",
        "r1",
        "c2",
        "branch",
        json!({"action": "create", "prompt": "watch the tests"}),
    ));
    recorded.push(model_end("t1", "r1", output_tool_use()));
    recorded.push(branch_create("t1", "r1", "c1", "C", "watch the logs"));
    let first = recorded.push(roster(&[("C", "watch the logs")], None));
    recorded.push(branch_create("t1", "r1", "c2", "D", "watch the tests"));
    let both = recorded.push(roster(
        &[("C", "watch the logs"), ("D", "watch the tests")],
        None,
    ));
    recorded.push(results(
        "t1",
        "r1",
        Some("r1"),
        &[
            ("c1", "Line created: C", false),
            ("c2", "Line created: D", false),
        ],
    ));
    recorded.push(consumed(&first));
    recorded.push(consumed(&both));
    recorded.push(model_start("t1", "r2"));
    recorded.push(text_delta("t1", "r2", "opened both"));
    recorded.push(model_end("t1", "r2", output_completed()));
    recorded.push(turn_ends("t1", completed_turn()));

    // D is ended from outside: its result is not told, and the roster with
    // its notice wakes main as a steer.
    recorded.at(15, 10);
    let ended_d = recorded.push(branch_ended(
        "D",
        BranchSource::Host {
            operation_id: "user-end".into(),
        },
    ));
    let outside = recorded.push(roster(&[("C", "watch the logs")], Some("D")));
    recorded.push(turn_start("t2"));
    recorded.push(consumed(&outside));
    recorded.push(model_start("t2", "r3"));
    recorded.push(text_delta("t2", "r3", "noted"));
    recorded.push(model_end("t2", "r3", output_completed()));
    recorded.push(turn_ends("t2", completed_turn()));

    // C's idle notice wakes main, and main steers C to finish.
    recorded.at(15, 20);
    let idle = recorded.push(line_idle("C", "completed"));
    let last_turn = recorded.push(turn_start("t3"));
    recorded.push(consumed(&idle));
    recorded.push(model_start("t3", "r4"));
    recorded.push(text_delta("t3", "r4", "asking C to finish"));
    recorded.push(call_start("t3", "r4", "c3", "branch"));
    recorded.push(call_input(
        "t3",
        "r4",
        "c3",
        "branch",
        json!({"action": "message", "to": "C", "mode": "steer", "content": "wrap up"}),
    ));
    recorded.push(model_end("t3", "r4", output_tool_use()));
    recorded.push(results(
        "t3",
        "r4",
        Some("r4"),
        &[("c3", "Sent to C.", false)],
    ));
    // C ends itself: its final message arrives while main is in the turn, then
    // its result, then the roster of no open lines at the next step.
    recorded.at(15, 25);
    let last = recorded.push(line_message(
        "C",
        "main",
        "all done",
        InputMode::Steer,
        true,
    ));
    let ended_c = recorded.push(branch_ended(
        "C",
        BranchSource::Tool {
            turn_id: "c-t4".into(),
            request_id: "c-r5".into(),
            call_id: "c-c1".into(),
        },
    ));
    let retiring = recorded.push(roster(&[], None));
    recorded.push(consumed(&last));
    recorded.push(consumed(&retiring));
    recorded.push(model_start("t3", "r5"));
    recorded.push(text_delta("t3", "r5", "noted C's end"));
    recorded.push(model_end("t3", "r5", output_completed()));
    recorded.push(turn_ends("t3", completed_turn()));

    let (page, error) = recorded.answer("t3", json!({}));
    assert!(!error, "{page}");
    assert!(
        page.contains("turn: line C's turn ended (completed) without a message to you"),
        "{page}"
    );
    assert!(
        page.contains("turn: line D was ended from outside"),
        "{page}"
    );
    assert!(
        !recorded.narrative().tells(&ended_c),
        "branch/ended is not told"
    );
    assert!(
        !recorded.narrative().tells(&ended_d),
        "branch/ended is not told"
    );

    // The first roster is read inside the turn that called create.
    let (page, error) = recorded.answer("t3", json!({ "open": first }));
    assert!(!error, "{page}");
    assert!(
        page.contains("Lines you opened that are still open."),
        "{page}"
    );
    assert!(page.contains("- C: watch the logs"), "{page}");

    // The notice of the line ended from outside opens with its marker.
    let (page, error) = recorded.answer("t3", json!({ "open": outside }));
    assert!(!error, "{page}");
    assert!(page.contains("<dim-branch-ended branch=\"D\">"), "{page}");

    // C's final message and the retiring roster are inside the last turn.
    let (page, error) = recorded.answer("t3", json!({ "open": last_turn }));
    assert!(!error, "{page}");
    assert!(
        page.contains("line C's final message: \"all done\""),
        "{page}"
    );
    assert!(!page.contains("last message"), "{page}");
    let (page, error) = recorded.answer("t3", json!({ "open": last }));
    assert!(!error, "{page}");
    assert!(page.contains("Line C ends with this message."), "{page}");
    let (page, error) = recorded.answer("t3", json!({ "open": retiring }));
    assert!(!error, "{page}");
    assert!(page.contains("All lines you opened have ended."), "{page}");
    assert!(page.contains("</dim-branch-lines> (ended)"), "{page}");
}

#[test]
// §14 8.
fn a_childs_identity_and_its_parents_messages_are_told_on_the_child() {
    let mut recorded = Recorded::new();
    recorded.push(branch_start(None));
    recorded.at(15, 0);
    received_turn(
        &mut recorded,
        "t1",
        input_message("plan the work"),
        "planned",
    );

    recorded.at(15, 10);
    recorded.push(branch_start(Some("main")));
    let opening = received_turn(&mut recorded, "t2", identity("C", "main"), "on it");

    // A steer that wakes the idle line opens its turn: no timing words.
    recorded.at(15, 20);
    received_turn(
        &mut recorded,
        "t3",
        line_message("main", "C", "status please", InputMode::Steer, false),
        "reporting",
    );

    // A steer that opens the turn reads without timing words; one the line
    // reads after its turn's first model step reads with them.
    recorded.at(15, 30);
    let opener = recorded.push(line_message(
        "main",
        "C",
        "act on the logs now",
        InputMode::Steer,
        false,
    ));
    let turn = recorded.push(turn_start("t4"));
    recorded.push(consumed(&opener));
    recorded.push(model_start("t4", "r4"));
    recorded.push(text_delta("t4", "r4", "checking the logs"));
    recorded.at(15, 35);
    let during = recorded.push(line_message(
        "main",
        "C",
        "also check the cache",
        InputMode::Steer,
        false,
    ));
    recorded.push(model_end(
        "t4",
        "r4",
        ModelOutputOutcome::Cancelled {
            reason: "replaced by steer".into(),
        },
    ));
    recorded.push(consumed(&during));
    recorded.push(model_start("t4", "r5"));
    recorded.push(text_delta("t4", "r5", "both clean"));
    // The line ends itself: the call is in t4 and `branch/end` follows it.
    recorded.push(call_start("t4", "r5", "c1", "branch"));
    recorded.push(call_input(
        "t4",
        "r5",
        "c1",
        "branch",
        json!({"action": "end", "content": "the logs are clean"}),
    ));
    recorded.push(model_end("t4", "r5", output_tool_use()));
    let ending = recorded.push(branch_end("t4", "r5", "c1"));
    recorded.push(results(
        "t4",
        "r5",
        Some("r5"),
        &[(
            "c1",
            "Line C is ending; your final message was sent to main.",
            false,
        )],
    ));
    recorded.push(turn_ends("t4", completed_turn()));

    let (page, error) = recorded.answer("t4", json!({}));
    assert!(!error, "{page}");
    assert!(
        page.lines()
            .any(|line| line.contains("this line was opened by main")),
        "{page}"
    );
    assert!(
        page.contains("turn: dim-branch-line branch=C parent=main source=tool (current)"),
        "{page}"
    );
    assert!(
        page.contains("turn: line main: \"status please\""),
        "{page}"
    );
    assert!(
        page.contains("turn: line main: \"act on the logs now\""),
        "{page}"
    );
    assert!(
        !page.contains("(while you were working)"),
        "the steer that opened the turn does not carry the timing words: {page}"
    );
    assert!(
        !recorded.narrative().tells(&ending),
        "branch/end is not a told kind"
    );

    // Inside the turn, the steer that replaced its model step carries them.
    let (page, error) = recorded.answer("t4", json!({ "open": turn }));
    assert!(!error, "{page}");
    assert!(
        page.contains("line main: \"act on the logs now\""),
        "{page}"
    );
    assert!(
        page.contains("line main (while you were working): \"also check the cache\""),
        "{page}"
    );

    let (page, error) = recorded.answer("t4", json!({ "open": opening }));
    assert!(!error, "{page}");
    assert!(page.contains("You are line C of this session"), "{page}");
}

#[test]
// §14 1.
fn a_turn_opened_by_a_line_message_is_indexed_by_that_write_not_by_a_pending_block() {
    let mut recorded = Recorded::new();
    recorded.at(16, 0);
    // A rule written between turns waits as an Append input; the message the
    // line receives consumes it and opens the turn (§3.6).
    let rule = recorded.push(EventKind::InputMessage(InputMessageData {
        blocks: vec![context_block::raw(
            context_block::element(
                "dim-rule",
                &[
                    ("source", "AGENTS.md"),
                    ("scope", "branch"),
                    ("op", "replace"),
                ],
                "Use pnpm, not npm.",
            ),
            Some("rule".into()),
        )],
        mode: Some(InputMode::Append),
        ..InputMessageData::default()
    }));
    let message = recorded.push(line_message(
        "main",
        "C",
        "start now",
        InputMode::Steer,
        false,
    ));
    let turn = recorded.push(turn_start("t1"));
    recorded.push(consumed(&rule));
    recorded.push(consumed(&message));
    recorded.push(model_start("t1", "r1"));
    recorded.push(text_delta("t1", "r1", "on it"));
    recorded.push(model_end("t1", "r1", output_completed()));
    recorded.push(turn_ends("t1", completed_turn()));

    let (page, error) = recorded.answer("t1", json!({}));
    assert!(!error, "{page}");
    let line = line_of(&page, &turn);
    assert!(line.contains("turn: line main: \"start now\""), "{line}");
    assert!(!line.contains("dim-rule"), "{line}");

    // The rule is still a line of the turn.
    let (page, error) = recorded.answer("t1", json!({ "open": turn }));
    assert!(!error, "{page}");
    assert!(
        line_of(&page, &rule)
            .contains("dim-rule source=AGENTS.md scope=branch op=replace (current)"),
        "{page}"
    );
    assert!(
        line_of(&page, &message).contains("line main: \"start now\""),
        "{page}"
    );
}

#[test]
// §14 1.
fn a_turn_a_continuation_opened_is_indexed_by_that_block_not_by_the_append_ahead_of_it() {
    let mut recorded = Recorded::new();
    recorded.at(18, 0);
    // The previous turn's end changed context, so Prompt wrote a `dim-state`
    // Append input, and Goal then queued its continuation. The turn consumes
    // both, the Append first.
    let state = recorded.push(state_change("dim/skills/list", "- a: user a"));
    let continuation = recorded.push(continuation());
    let turn = recorded.push(turn_start("t1"));
    recorded.push(consumed(&state));
    recorded.push(consumed(&continuation));
    recorded.push(model_start("t1", "r1"));
    recorded.push(text_delta("t1", "r1", "still on it"));
    recorded.push(model_end("t1", "r1", output_completed()));
    recorded.push(turn_ends("t1", completed_turn()));

    let (page, error) = recorded.answer("t1", json!({}));
    assert!(!error, "{page}");
    let line = line_of(&page, &turn);
    assert!(
        line.contains("turn: dim-goal kind=continuation count=1 limit=20 after=12 branch=main"),
        "{line}"
    );
    assert!(!line.contains("dim-state"), "{line}");

    // The state block is still a line of the turn.
    let (page, error) = recorded.answer("t1", json!({ "open": turn }));
    assert!(!error, "{page}");
    assert!(
        line_of(&page, &state).contains("dim-state source=dim/skills/list (current)"),
        "{page}"
    );
    assert!(
        line_of(&page, &continuation)
            .contains("dim-goal kind=continuation count=1 limit=20 after=12 branch=main"),
        "{page}"
    );
}

#[test]
// §14 1.
fn a_steer_read_during_the_turn_does_not_title_it() {
    let mut recorded = Recorded::new();
    recorded.at(18, 30);
    let state = recorded.push(state_change("dim/skills/list", "- a: user a"));
    let continuation = recorded.push(continuation());
    let turn = recorded.push(turn_start("t1"));
    recorded.push(consumed(&state));
    recorded.push(consumed(&continuation));
    recorded.push(model_start("t1", "r1"));
    recorded.push(text_delta("t1", "r1", "working"));
    recorded.push(model_end("t1", "r1", output_completed()));

    // The user steers after the first model step: the input joins the turn
    // but does not open it (§3.6).
    let steered = recorded.push(steer("also check the logs"));
    recorded.push(consumed(&steered));
    recorded.push(model_start("t1", "r2"));
    recorded.push(text_delta("t1", "r2", "on it"));
    recorded.push(model_end("t1", "r2", output_completed()));
    recorded.push(turn_ends("t1", completed_turn()));

    let (page, error) = recorded.answer("t1", json!({}));
    assert!(!error, "{page}");
    let line = line_of(&page, &turn);
    assert!(
        line.contains("turn: dim-goal kind=continuation count=1 limit=20 after=12 branch=main"),
        "{line}"
    );
    assert!(!line.contains("user"), "{line}");

    // The steer is still told as the user speaking while the model worked.
    let (page, error) = recorded.answer("t1", json!({ "open": turn }));
    assert!(!error, "{page}");
    assert!(
        line_of(&page, &steered).contains("user (while you were working): also check the logs"),
        "{page}"
    );
}

#[test]
// §14 1.
fn a_turn_that_ended_before_it_started_is_listed_and_opens_empty() {
    let mut recorded = Recorded::new();
    recorded.at(17, 0);
    // A reload lands between `turn/start` and `start_request`, and the
    // rebuilt Loop ends the turn it never filled.
    let empty = recorded.push(turn_start("t1"));
    recorded.push(turn_ends(
        "t1",
        TurnOutcome::Failed {
            error: "reloaded before the first request".into(),
        },
    ));
    recorded.at(17, 10);
    simple_turn(&mut recorded, "t2", "next task", "next answer");

    let (page, error) = recorded.answer("t2", json!({}));
    assert!(!error, "{page}");
    assert_eq!(
        line_of(&page, &empty),
        format!("17:00 {empty} turn: no opening input — failed"),
        "{page}"
    );

    let (page, error) = recorded.answer("t2", json!({ "open": empty }));
    assert!(!error, "{page}");
    assert_eq!(page, "Nothing in this turn.");

    // Only the first page of an empty turn is a page; a later offset is past
    // its end.
    let (page, error) = recorded.answer("t2", json!({ "open": empty, "offset": 1 }));
    assert!(error, "{page}");
    assert_eq!(page, format!("Turn {empty} has 0 lines."));
}

#[test]
// §14 9.
fn every_line_stays_bounded_whatever_the_arguments_and_the_blocks() {
    let mut recorded = Recorded::new();
    let long_name = "n".repeat(1_000);
    let mut many = JsonObject::new();
    for index in 0..1_000 {
        many.insert(format!("k{index}"), Value::from(index));
    }

    recorded.at(9, 0);
    let input = recorded.push(input_message("call it"));
    let turn = recorded.push(turn_start("t1"));
    recorded.push(consumed(&input));
    recorded.push(model_start("t1", "r1"));
    let call = recorded.push(call_start("t1", "r1", "c1", &long_name));
    recorded.push(call_input(
        "t1",
        "r1",
        "c1",
        &long_name,
        Value::Object(many),
    ));
    recorded.push(model_end("t1", "r1", output_tool_use()));
    recorded.push(results(
        "t1",
        "r1",
        Some("dispatch"),
        &[("c1", "ok", false)],
    ));
    recorded.push(turn_ends("t1", completed_turn()));
    let (page, error) = recorded.answer("t1", json!({ "open": turn }));
    assert!(!error, "{page}");
    bounded(&page);
    let line = line_of(&page, &call);
    assert!(
        line.contains(&format!("you called {}… ", &long_name[..119])),
        "{line}"
    );
    assert_eq!(line.matches('…').count(), 2, "{line}");
    bounded(&page);

    recorded.at(9, 10);
    let blocks: Vec<_> = (0..50)
        .map(|index| {
            context_block::raw(
                format!("<block-{index}>\nbody {index}\n</block-{index}>"),
                None,
            )
        })
        .collect();
    let many_blocks = recorded.push(EventKind::InputMessage(InputMessageData {
        blocks,
        ..InputMessageData::default()
    }));
    let second = recorded.push(turn_start("t2"));
    recorded.push(consumed(&many_blocks));
    recorded.push(turn_ends("t2", completed_turn()));
    let (page, error) = recorded.answer("t2", json!({ "open": second }));
    assert!(!error, "{page}");
    bounded(&page);
    assert!(line_of(&page, &many_blocks).contains("body 0"), "{page}");

    recorded.at(9, 20);
    let input = recorded.push(input_message("call a hundred tools"));
    let busy = recorded.push(turn_start("t3"));
    recorded.push(consumed(&input));
    for index in 0..100 {
        let request = format!("r{index}");
        recorded.push(model_start("t3", &request));
        recorded.push(call_start(
            "t3",
            &request,
            &format!("c{index}"),
            &format!("tool-{index}"),
        ));
        recorded.push(call_input(
            "t3",
            &request,
            &format!("c{index}"),
            &format!("tool-{index}"),
            json!({"index": index}),
        ));
        recorded.push(model_end("t3", &request, output_tool_use()));
        recorded.push(results(
            "t3",
            &request,
            Some("dispatch"),
            &[(&format!("c{index}"), "ok", false)],
        ));
    }
    recorded.push(turn_ends("t3", completed_turn()));
    let (page, error) = recorded.answer("t3", json!({}));
    assert!(!error, "{page}");
    bounded(&page);
    let summary = line_of(&page, &busy);
    assert!(summary.starts_with("09:20 "), "{summary}");
    assert!(summary.contains("you called tool-0, tool-1, "), "{summary}");
    assert!(summary.contains('…'), "{summary}");

    let missing = "missing".repeat(1_000);
    for arguments in [
        json!({"tool": missing}),
        json!({"open": missing}),
        json!({"before": missing}),
        json!({(missing.clone()): true}),
    ] {
        let (page, _) = recorded.answer("t3", arguments);
        bounded(&page);
    }
}

#[test]
// §14 11.
fn a_turn_longer_than_a_page_continues_from_its_offset_to_its_end() {
    let mut recorded = Recorded::new();
    recorded.at(8, 0);
    let input = recorded.push(input_message("a long turn"));
    let turn = recorded.push(turn_start("t1"));
    recorded.push(consumed(&input));
    for index in 0..25 {
        let request = format!("r{index}");
        recorded.push(model_start("t1", &request));
        recorded.push(call_start("t1", &request, &format!("c{index}"), "echo"));
        recorded.push(call_input(
            "t1",
            &request,
            &format!("c{index}"),
            "echo",
            json!({"index": index}),
        ));
        recorded.push(model_end("t1", &request, output_tool_use()));
        recorded.push(results(
            "t1",
            &request,
            Some("dispatch"),
            &[(&format!("c{index}"), "done", false)],
        ));
    }
    recorded.push(turn_ends("t1", completed_turn()));

    let mut offset = 0;
    let mut seen = Vec::new();
    loop {
        let (page, error) = recorded.answer("t1", json!({ "open": turn, "offset": offset }));
        assert!(!error, "{page}");
        let entries: Vec<_> = page
            .lines()
            .filter(|line| line.starts_with("08:00 "))
            .collect();
        assert!(entries.len() <= 20, "{page}");
        seen.extend(
            entries
                .iter()
                .map(|line| line.split(' ').nth(1).unwrap().to_owned()),
        );
        match page.lines().last().unwrap() {
            last if last.starts_with("End of turn. Open one: open=") => break,
            last => {
                let next: usize = last
                    .strip_prefix(&format!("More of this turn: open={turn} offset="))
                    .and_then(|rest| rest.strip_suffix('.'))
                    .unwrap_or_else(|| panic!("unexpected last line {last}"))
                    .parse()
                    .unwrap();
                assert_eq!(entries.len(), 20, "{page}");
                offset = next;
            }
        }
    }
    assert_eq!(seen.len(), 26, "the input and 25 calls, each once");
    let unique: std::collections::HashSet<_> = seen.iter().collect();
    assert_eq!(unique.len(), seen.len(), "every line exactly once");
}

#[test]
// §14 12.
fn an_interrupted_output_is_told_with_exactly_its_committed_text() {
    let mut recorded = Recorded::new();
    recorded.at(7, 0);
    let input = recorded.push(input_message("go"));
    let turn = recorded.push(turn_start("t1"));
    recorded.push(consumed(&input));
    recorded.push(model_start("t1", "r1"));
    recorded.push(thought("t1", "r1", "a secret plan"));
    recorded.push(text_delta("t1", "r1", "Hello"));
    let started = recorded.push(call_start("t1", "r1", "c1", "echo"));
    recorded.push(results(
        "t1",
        "r1",
        None,
        &[(
            "c1",
            "Tool was not executed: the model output ended before its arguments were complete",
            true,
        )],
    ));
    recorded.push(model_end("t1", "r1", output_cancelled()));
    recorded.at(7, 5);
    // A steer that arrives between the turn's steps is read while it works.
    let next = recorded.push(steer("again"));
    recorded.push(consumed(&next));
    recorded.at(7, 6);
    recorded.push(model_start("t1", "r2"));
    recorded.push(thought("t1", "r2", "another secret"));
    recorded.push(text_delta("t1", "r2", "half"));
    recorded.push(model_end(
        "t1",
        "r2",
        ModelOutputOutcome::Failed {
            error: "aborted".into(),
            budget: None,
            presentation: None,
        },
    ));
    recorded.push(turn_ends(
        "t1",
        TurnOutcome::Cancelled {
            reason: "stopped".into(),
        },
    ));

    let (page, error) = recorded.answer("t1", json!({ "open": turn }));
    assert!(!error, "{page}");
    let lines: Vec<_> = page.lines().collect();
    assert_eq!(
        lines,
        [
            "2026-10-17 (UTC)",
            &format!("07:00 {input} user: go"),
            "07:00 e4 your reply was cut off (cancelled: stopped): Hello",
            &format!("07:00 {started} you started calling echo (arguments not received) → not run: \"Tool was not executed: the model output ended before its arguments were complete\""),
            &format!("07:05 {next} user (while you were working): again"),
            "07:06 e12 your reply was cut off (failed: aborted): half",
            &format!("End of turn. Open one: open={input}."),
        ]
        .map(str::to_owned),
        "{page}"
    );
    assert!(!page.contains("secret"), "{page}");
}

#[test]
// §14 4.
fn an_input_that_never_counted_and_thinking_are_not_told() {
    let mut recorded = Recorded::new();
    recorded.at(6, 0);
    let withdrawn = recorded.push(input_message("never applied"));
    recorded.push(EventKind::InputWithdraw(InputWithdrawData {
        input_id: withdrawn.clone(),
        reason: None,
    }));
    recorded.push(EventKind::InputDeleted(InputDeletedData {
        input_id: withdrawn.clone(),
    }));
    let pending = recorded.push(input_message("still waiting"));
    recorded.push(EventKind::InputApplied(InputAppliedData {
        input_id: pending.clone(),
        mode: Some(InputMode::Queue),
    }));
    let taken = recorded.push(input_message("counted once"));
    recorded.push(turn_start("t1"));
    recorded.push(consumed(&taken));
    recorded.push(model_start("t1", "r1"));
    recorded.push(thought("t1", "r1", "only thinking"));
    recorded.push(model_end("t1", "r1", output_completed()));

    let narrative = recorded.narrative();
    assert!(narrative.tells(&taken), "the counted input");
    for gone in [&withdrawn, &pending] {
        assert!(!narrative.tells(gone), "an input that never counted");
    }
    let thought = recorded
        .events
        .iter()
        .find(|event| event.kind.key() == "step/thinking")
        .unwrap();
    assert!(!narrative.tells(&thought.id), "thinking is not told");
    let (page, error) = recorded.answer("t1", json!({}));
    assert!(!error, "{page}");
    assert!(page.contains("counted once"), "{page}");
    assert!(!page.contains("never applied"), "{page}");
    assert!(!page.contains("still waiting"), "{page}");
    assert!(!page.contains("only thinking"), "{page}");
}

#[test]
// §14 4.
fn a_live_keyed_block_is_marked_current_and_an_earlier_version_is_not() {
    let mut recorded = Recorded::new();
    recorded.at(6, 0);
    let older = recorded.push(keyed_input("first", "rule", "old rule"));
    let first = recorded.push(turn_start("t1"));
    recorded.push(consumed(&older));
    recorded.push(model_start("t1", "r1"));
    recorded.push(text_delta("t1", "r1", "first answer"));
    recorded.push(model_end("t1", "r1", output_completed()));
    recorded.push(turn_ends("t1", completed_turn()));

    recorded.at(6, 10);
    let newer = recorded.push(keyed_input("second", "rule", "new rule"));
    let second = recorded.push(turn_start("t2"));
    recorded.push(consumed(&newer));
    recorded.push(model_start("t2", "r2"));
    recorded.push(text_delta("t2", "r2", "second answer"));
    recorded.push(model_end("t2", "r2", output_completed()));
    recorded.push(turn_ends("t2", completed_turn()));

    // A queued block context assembly never replayed is no version at all.
    recorded.at(6, 20);
    let pending = recorded.push(keyed_input("third", "rule", "pending rule"));

    let (page, error) = recorded.answer("t2", json!({ "open": second }));
    assert!(!error, "{page}");
    let line = line_of(&page, &newer);
    assert!(line.contains("dim-rule (current)"), "{line}");
    assert!(!page.contains(&pending), "{page}");
    assert!(!page.contains("pending rule"), "{page}");

    // The version the newer block replaced is shown, without a marker.
    let (page, error) = recorded.answer("t2", json!({ "open": first }));
    assert!(!error, "{page}");
    let line = line_of(&page, &older);
    assert!(line.contains("user: first + dim-rule"), "{line}");
    assert!(!line.contains("(current)"), "{line}");
    assert!(!line.contains("(ended)"), "{line}");

    let (page, error) = recorded.answer("t2", json!({ "open": newer }));
    assert!(!error, "{page}");
    assert!(page.contains("(current version in your context)"), "{page}");
    let (page, error) = recorded.answer("t2", json!({ "open": older }));
    assert!(!error, "{page}");
    assert!(page.contains("old rule"), "{page}");
    assert!(
        !page.contains("(current version in your context)"),
        "{page}"
    );
    assert!(!page.contains("(ended)"), "{page}");
}

#[test]
// §14 8.
fn a_grandchild_branch_keeps_the_middle_branch_turn_its_own() {
    let mut recorded = Recorded::new();
    recorded.push(branch_start(None));
    recorded.at(11, 0);
    let root_input = recorded.push(input_message("root task"));
    let root = recorded.push(turn_start("t1"));
    recorded.push(consumed(&root_input));
    recorded.push(model_start("t1", "r1"));
    recorded.push(text_delta("t1", "r1", "root answer"));
    recorded.push(model_end("t1", "r1", output_completed()));

    // The first child forks away before its parent's turn ends.
    recorded.at(11, 10);
    recorded.push(branch_start(Some("main")));
    let middle_input = recorded.push(input_message("middle task"));
    let middle = recorded.push(turn_start("t2"));
    recorded.push(consumed(&middle_input));
    recorded.push(model_start("t2", "r2"));
    recorded.push(text_delta("t2", "r2", "middle answer"));
    recorded.push(model_end("t2", "r2", output_completed()));

    // The grandchild forks away before the middle branch's turn ends.
    recorded.at(11, 20);
    recorded.push(branch_start(Some("b")));
    simple_turn(&mut recorded, "t3", "own task", "own answer");

    let (page, error) = recorded.answer("t3", json!({}));
    assert!(!error, "{page}");
    let middle_line = line_of(&page, &middle);
    assert!(
        middle_line.contains("turn: user \"middle task\""),
        "the middle turn's own input: {middle_line}"
    );
    assert!(
        middle_line.contains("continued on the parent line"),
        "{middle_line}"
    );
    assert!(
        line_of(&page, &root).contains("continued on the parent line"),
        "{page}"
    );
    assert!(
        page.lines()
            .any(|line| line.contains("this line was opened by b")),
        "{page}"
    );

    let (page, error) = recorded.answer("t3", json!({ "open": middle }));
    assert!(!error, "{page}");
    assert!(page.contains("middle task"), "{page}");
    let (page, error) = recorded.answer("t3", json!({ "open": root }));
    assert!(!error, "{page}");
    assert!(page.contains("root task"), "{page}");
    assert!(!page.contains("middle task"), "{page}");
}

#[test]
// §14 13.
fn the_arguments_and_the_paginations_say_how_to_fix_the_call() {
    let mut recorded = Recorded::new();
    recorded.at(5, 0);
    let input = recorded.push(input_message("first need"));
    let turn = recorded.push(turn_start("t1"));
    recorded.push(consumed(&input));
    recorded.push(model_start("t1", "r1"));
    recorded.push(text_delta("t1", "r1", "first answer"));
    recorded.push(model_end("t1", "r1", output_completed()));
    recorded.push(turn_ends("t1", completed_turn()));
    recorded.at(5, 10);
    let between = recorded.push(compaction(6));
    recorded.at(5, 20);
    simple_turn(&mut recorded, "t2", "second need", "second answer");

    for (arguments, expected) in [
        (
            json!({ "open": turn, "about": "x" }),
            "recall: open shows one turn or thing; drop about, tool and before.",
        ),
        (
            json!({ "open": turn, "before": turn }),
            "recall: open shows one turn or thing; drop about, tool and before.",
        ),
        (json!({ "offset": 1 }), "recall: offset needs open."),
        (json!({ "about": 5 }), "recall: about must be a string."),
        (
            json!({ "tool": "" }),
            "recall: tool must be a non-empty string.",
        ),
        (
            json!({ "about": " \t " }),
            "recall: about must be a non-empty string.",
        ),
        (
            json!({ "open": " " }),
            "recall: open must be a non-empty string.",
        ),
        (
            json!({ "about": "x".repeat(201) }),
            "recall: about must be at most 200 characters.",
        ),
        (
            json!({ "nope": 1 }),
            "recall: nope must be a known argument.",
        ),
        (json!({ "open": "nope" }), "No turn or line has id nope."),
        (json!({ "before": "nope" }), "No turn or line has id nope."),
    ] {
        let (page, error) = recorded.answer("t2", arguments.clone());
        assert!(error, "{arguments} is not an error: {page}");
        assert_eq!(page, expected, "{arguments}");
    }
    let (page, error) = recorded.answer("t2", json!({ "open": turn, "offset": 9 }));
    assert!(error, "{page}");
    assert_eq!(page, format!("Turn {turn} has 2 lines."));
    let (page, error) = recorded.answer("t2", json!({ "open": input, "offset": 9_999 }));
    assert!(error, "{page}");
    assert!(page.ends_with(" characters."), "{page}");

    // `before` continues after an index line, a line between turns and a
    // search line.
    let (page, error) = recorded.answer("t2", json!({}));
    assert!(!error, "{page}");
    let lines: Vec<_> = page.lines().collect();
    assert!(
        lines[2].contains(&format!(
            "{between} the conversation up to here was summarized"
        )),
        "{page}"
    );
    assert_eq!(lines.last().copied(), Some("Nothing earlier."));
    let newer_turn = lines[1].split(' ').nth(1).unwrap().to_owned();
    let (earlier, error) = recorded.answer("t2", json!({ "before": newer_turn }));
    assert!(!error, "{earlier}");
    assert!(!earlier.contains("second need"), "{earlier}");
    assert!(earlier.contains("first need"), "{earlier}");
    let (earlier, error) = recorded.answer("t2", json!({ "before": between }));
    assert!(!error, "{earlier}");
    assert!(earlier.contains("first need"), "{earlier}");
    assert!(!earlier.contains("second need"), "{earlier}");

    let (page, error) = recorded.answer("t2", json!({ "about": "need" }));
    assert!(!error, "{page}");
    assert!(
        page.contains("first need") && page.contains("second need"),
        "{page}"
    );
    let lines: Vec<_> = page.lines().collect();
    assert_eq!(lines.last().copied(), Some("No more."));
    let older = lines[2].split(' ').nth(1).unwrap().to_owned();
    let (page, error) = recorded.answer("t2", json!({ "about": "need", "before": older }));
    assert!(!error, "{page}");
    assert!(!page.contains("second need"), "{page}");
    assert_eq!(page.lines().last(), Some("No more."));
}

/// A memory store that counts the events a page read decodes.
#[derive(Default)]
struct PageReads {
    memory: MemoryTimelineStore,
    events: std::sync::atomic::AtomicUsize,
    pages: std::sync::atomic::AtomicUsize,
}

impl TimelineStore for PageReads {
    fn create_session<'a>(
        &'a self,
        session_id: &'a str,
        config: &'a SessionConfig,
    ) -> BoxFuture<'a, Result<()>> {
        self.memory.create_session(session_id, config)
    }
    fn rollback_session<'a>(&'a self, session_id: &'a str) -> BoxFuture<'a, Result<()>> {
        self.memory.rollback_session(session_id)
    }
    fn release_session<'a>(&'a self, session_id: &'a str) -> BoxFuture<'a, Result<()>> {
        self.memory.release_session(session_id)
    }
    fn session_config<'a>(
        &'a self,
        session_id: &'a str,
    ) -> BoxFuture<'a, Result<Option<SessionConfig>>> {
        self.memory.session_config(session_id)
    }
    fn branches<'a>(&'a self, session_id: &'a str) -> BoxFuture<'a, Result<Vec<StoredBranch>>> {
        self.memory.branches(session_id)
    }
    fn head<'a>(&'a self, key: &'a TimelineKey) -> BoxFuture<'a, Result<u64>> {
        self.memory.head(key)
    }
    fn append(
        &self,
        key: TimelineKey,
        events: Vec<StoredEvent>,
        publish: PublishCallback,
    ) -> Result<()> {
        self.memory.append(key, events, publish)
    }
    fn read_segments<'a>(
        &'a self,
        session_id: &'a str,
        segments: Vec<(String, std::ops::RangeInclusive<u64>)>,
    ) -> BoxFuture<'a, Result<Vec<Vec<StoredEvent>>>> {
        self.memory.read_segments(session_id, segments)
    }
    fn read_kind_page<'a>(
        &'a self,
        key: &'a TimelineKey,
        kinds: &'a [&'a str],
        before: u64,
        limit: usize,
    ) -> BoxFuture<'a, Result<Vec<StoredEvent>>> {
        Box::pin(async move {
            let page = self
                .memory
                .read_kind_page(key, kinds, before, limit)
                .await?;
            self.events
                .fetch_add(page.len(), std::sync::atomic::Ordering::SeqCst);
            self.pages.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(page)
        })
    }
    fn trace<'a>(
        &'a self,
        key: &'a TimelineKey,
        sequence: u64,
    ) -> BoxFuture<'a, Result<Option<TimelineTrace>>> {
        self.memory.trace(key, sequence)
    }
    fn query<'a>(
        &'a self,
        key: &'a TimelineKey,
        query: &'a TimelineQuery,
        from: u64,
        through: u64,
    ) -> BoxFuture<'a, Result<TimelineQueryPage>> {
        self.memory.query(key, query, from, through)
    }
    fn save_snapshot<'a>(
        &'a self,
        key: &'a TimelineKey,
        snapshot: RecoverySnapshot,
    ) -> BoxFuture<'a, Result<()>> {
        self.memory.save_snapshot(key, snapshot)
    }
    fn latest_snapshot<'a>(
        &'a self,
        key: &'a TimelineKey,
        through: u64,
        interpreter: &'a str,
    ) -> BoxFuture<'a, Result<Option<RecoverySnapshot>>> {
        self.memory.latest_snapshot(key, through, interpreter)
    }
}

impl PageReads {
    fn decoded(&self) -> usize {
        self.events.load(std::sync::atomic::Ordering::SeqCst)
    }
    fn pages(&self) -> usize {
        self.pages.load(std::sync::atomic::Ordering::SeqCst)
    }
}

/// Runs one `recall` call and checks that its answer is the answer folding
/// the whole pinned history gives for the same arguments; returns the events
/// its pages decoded.
async fn recall_is_the_whole_fold(
    session: &Session,
    reads: &PageReads,
    branch: &str,
    arguments: Value,
) -> (String, usize) {
    let before = reads.decoded();
    let (text, error) = recall(session, branch, arguments.clone()).await;
    let read = reads.decoded() - before;
    let history = session.history(branch, None).await.unwrap();
    let pinned = &history[..history
        .iter()
        .rposition(|event| matches!(event.kind, EventKind::ToolResult(_)))
        .expect("the recall call's result")];
    let calling = pinned
        .iter()
        .rev()
        .find_map(|event| match &event.kind {
            EventKind::TurnStart(data) => Some(data.turn_id.as_str()),
            _ => None,
        })
        .expect("the calling turn");
    let whole = Narrative::new(pinned).answer(calling, arguments.as_object().unwrap());
    let whole_text = match &whole.content {
        ToolContent::Text(text) => text.clone(),
        other => panic!("expected text, got {other:?}"),
    };
    assert_eq!(
        text, whole_text,
        "the paged answer differs from folding the pinned history for {arguments}"
    );
    assert_eq!(error, whole.is_error.unwrap_or(false), "{arguments}");
    (text, read)
}

/// The `before=` id a page ends with.
fn before_of(page: &str) -> String {
    let at = page.find("before=").expect("a page handle") + "before=".len();
    page[at..]
        .split(|c: char| !c.is_ascii_hexdigit() && c != '-')
        .next()
        .expect("an id")
        .to_owned()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
// §5.3 pages and §3.1 the pinned read.
async fn a_long_history_is_answered_from_bounded_pages_read_backwards() {
    tokio::time::timeout(Duration::from_secs(180), async {
        let reads = Arc::new(PageReads::default());
        let agent = agent_with(reads.clone()).await;
        let session = open(&agent).await;
        for task in 0..120 {
            session
                .submit(format!("task {task}"), InputMode::Queue, "main")
                .await
                .unwrap();
            session.idle("main").await.unwrap();
        }
        let history = session.history("main", None).await.unwrap();
        assert!(
            history.len() > 600,
            "the history is long: {}",
            history.len()
        );

        let (index, read) = recall_is_the_whole_fold(&session, &reads, "main", json!({})).await;
        assert!(index.contains("Earlier: before="), "{index}");
        assert!(
            read < history.len() / 2,
            "the index decoded {read} of {} events",
            history.len()
        );
        let pages = reads.pages();
        assert!(pages > 1, "the index read {pages} page(s)");
        assert!(pages <= 3, "the index read {pages} pages");

        let (second, read) = recall_is_the_whole_fold(
            &session,
            &reads,
            "main",
            json!({ "before": before_of(&index) }),
        )
        .await;
        assert!(second.contains("Earlier: before="), "{second}");
        assert!(
            read < history.len() / 2,
            "the second index page decoded {read} of {} events",
            history.len()
        );

        let (search, read) =
            recall_is_the_whole_fold(&session, &reads, "main", json!({ "about": "task" })).await;
        assert!(search.contains("More: before="), "{search}");
        assert!(
            read < history.len() / 2,
            "the search decoded {read} of {} events",
            history.len()
        );

        // A search and a turn that only the oldest pages hold still answer
        // exactly as folding the whole pinned history does.
        let (deep, _) =
            recall_is_the_whole_fold(&session, &reads, "main", json!({ "about": "task 0" })).await;
        assert!(deep.contains("task 0"), "{deep}");
        let first = history
            .iter()
            .find_map(|event| match &event.kind {
                EventKind::TurnStart(_) => Some(event.id.clone()),
                _ => None,
            })
            .expect("the first turn");
        let (turn, _) =
            recall_is_the_whole_fold(&session, &reads, "main", json!({ "open": first })).await;
        assert!(turn.contains("task 0"), "{turn}");

        let newest_input = history
            .iter()
            .rev()
            .find_map(|event| match &event.kind {
                EventKind::InputMessage(data) if data.user.is_some() => Some(event.id.clone()),
                _ => None,
            })
            .expect("the newest user input");
        let (_thing, read) =
            recall_is_the_whole_fold(&session, &reads, "main", json!({ "open": newest_input }))
                .await;
        assert!(
            read < history.len() / 2,
            "opening the newest input decoded {read} of {} events",
            history.len()
        );

        agent.close().await.unwrap();
    })
    .await
    .expect("the long history flow");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
// §3.1 pages through the inherited prefix and §3.6 the cut parent turn.
async fn a_child_branch_pages_through_its_inherited_prefix() {
    tokio::time::timeout(Duration::from_secs(180), async {
        let reads = Arc::new(PageReads::default());
        let agent = agent_with(reads.clone()).await;
        let session = open(&agent).await;
        session
            .submit("first parent task".into(), InputMode::Queue, "main")
            .await
            .unwrap();
        session.idle("main").await.unwrap();
        session
            .submit(
                r#"tool:branch {"action":"create","prompt":"investigate the logs"}"#.into(),
                InputMode::Queue,
                "main",
            )
            .await
            .unwrap();
        session.idle("main").await.unwrap();
        let child = session
            .branches()
            .into_iter()
            .find(|info| info.parent.as_deref() == Some("main"))
            .expect("the child branch")
            .id;
        session.idle(&child).await.unwrap();
        for task in 0..40 {
            session
                .submit(format!("child task {task}"), InputMode::Queue, &child)
                .await
                .unwrap();
            session.idle(&child).await.unwrap();
        }
        let history = session.history(&child, None).await.unwrap();

        let (index, read) = recall_is_the_whole_fold(&session, &reads, &child, json!({})).await;
        assert!(index.contains("Earlier: before="), "{index}");
        assert!(
            read < history.len(),
            "the index decoded {read} of {} events",
            history.len()
        );

        let inherited = {
            let parent = session.history("main", None).await.unwrap();
            id_of_input(&parent, "first parent task")
        };
        let (page, _) =
            recall_is_the_whole_fold(&session, &reads, &child, json!({ "open": inherited })).await;
        assert!(page.contains("first parent task"), "{page}");

        agent.close().await.unwrap();
    })
    .await
    .expect("the child branch pages flow");
}

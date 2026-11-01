use dim_agent_capabilities::{
    context_block::{self, raw, retire},
    history::project_history,
};
use dim_agent_types::*;
use std::sync::Arc;

fn event(sequence: u64, kind: EventKind) -> Arc<AgentEvent> {
    Arc::new(AgentEvent {
        id: format!("event-{sequence}"),
        sequence,
        create_at: sequence,
        tags: None,
        kind,
    })
}

fn input(sequence: u64, text: &str, blocks: Vec<InputBlock>) -> [Arc<AgentEvent>; 2] {
    [
        event(
            sequence,
            EventKind::InputMessage(InputMessageData {
                user: InputMessageData::text(text, None).user,
                blocks,
                ..InputMessageData::default()
            }),
        ),
        event(
            sequence + 1,
            EventKind::InputConsumed(InputConsumedData {
                input_id: format!("event-{sequence}"),
            }),
        ),
    ]
}

fn mode(value: &str) -> InputBlock {
    raw(
        format!("<dim-state source=\"host/mode\">{value}</dim-state>"),
        Some("mode".into()),
    )
}

fn texts(events: &[Arc<AgentEvent>]) -> Vec<String> {
    project_history(events, "main")
        .unwrap()
        .into_iter()
        .map(|entry| match &entry.message.content {
            MessageContent::Text(text) => text.clone(),
            other => panic!("expected text, got {other:?}"),
        })
        .collect()
}

#[test]
fn keyed_block_baseline_belongs_to_each_entry_not_the_latest_prefix() {
    let mut events = input(1, "first", vec![mode("plan")]).to_vec();
    events.extend(input(3, "second", vec![mode("execute")]));
    let history = project_history(&events, "main").unwrap();
    let first = history
        .iter()
        .find(|entry| entry.id == "event-2:block:0")
        .unwrap();
    let second = history
        .iter()
        .find(|entry| entry.id == "event-4:block:0")
        .unwrap();
    assert_eq!(first.baseline.len(), 1);
    assert!(format!("{:?}", first.baseline[0].message).contains("plan"));
    assert_eq!(second.baseline.len(), 1);
    assert!(format!("{:?}", second.baseline[0].message).contains("execute"));
}

#[test]
fn a_retired_key_leaves_the_baseline_after_its_block_is_delivered() {
    let mut events = input(
        1,
        "",
        vec![
            raw("<a/>".into(), Some("a".into())),
            raw("<b/>".into(), Some("b".into())),
        ],
    )
    .to_vec();
    events.extend(input(3, "", vec![retire("<b-ended/>".into(), "b".into())]));
    let history = project_history(&events, "main").unwrap();
    let last = history.last().unwrap();
    assert_eq!(
        last.message.as_ref().clone(),
        Message::text(Role::User, "<b-ended/>")
    );
    assert_eq!(
        last.baseline
            .iter()
            .map(|entry| entry.key.as_str())
            .collect::<Vec<_>>(),
        ["a"]
    );
}

#[test]
fn the_baseline_keeps_each_live_key_at_its_first_delivery_with_its_latest_block() {
    let mut events = input(
        1,
        "",
        vec![
            raw("<b-1/>".into(), Some("b".into())),
            raw("<a-1/>".into(), Some("a".into())),
        ],
    )
    .to_vec();
    events.extend(input(3, "", vec![raw("<b-2/>".into(), Some("b".into()))]));
    let history = project_history(&events, "main").unwrap();
    assert_eq!(
        history
            .last()
            .unwrap()
            .baseline
            .iter()
            .map(|entry| entry.message.as_ref().clone())
            .collect::<Vec<_>>(),
        [
            Message::text(Role::User, "<b-2/>"),
            Message::text(Role::User, "<a-1/>")
        ]
    );
}

#[test]
fn prompt_changes_and_scope_ends_are_not_projected() {
    let events = vec![
        event(
            1,
            EventKind::TurnStart(TurnStartData {
                turn_id: "turn-1".into(),
            }),
        ),
        event(
            2,
            EventKind::PromptChanged(PromptChangedData {
                timeline: "main".into(),
                revision: "r1".into(),
                previous: None,
                reason: PromptChangeReason::Initialize,
                changes: vec![PromptChange::Set {
                    entry: PromptEntry {
                        source: "rules".into(),
                        version: "1".into(),
                        body: "never projected".into(),
                        scope: PromptScope::Turn {
                            timeline: "main".into(),
                            id: "turn-1".into(),
                        },
                        delivery: PromptDelivery::Context,
                        order: 0,
                    },
                }],
            }),
        ),
        event(
            3,
            EventKind::TurnEnd(TurnEndData {
                turn_id: "turn-1".into(),
                outcome: TurnOutcome::Completed {
                    reason: "done".into(),
                },
            }),
        ),
    ];
    assert!(project_history(&events, "main").unwrap().is_empty());
}

#[test]
fn input_consumed_while_a_request_is_open_projects_at_its_position_and_branch_resets_output() {
    let mut events = vec![event(
        1,
        EventKind::ModelStart(StepData {
            turn_id: "parent-turn".into(),
            request_id: "parent-request".into(),
        }),
    )];
    events.extend(input(2, "", vec![mode("child")]));
    events.push(event(
        4,
        EventKind::BranchStart(BranchStartData {
            parent: Some("main".into()),
            create_event: Some(3),
        }),
    ));
    events.extend(input(5, "child input", Vec::new()));
    let history = project_history(&events, "main").unwrap();
    assert_eq!(
        history
            .iter()
            .map(|entry| entry.id.as_str())
            .collect::<Vec<_>>(),
        ["event-3:block:0", "event-6"]
    );
}

/// An input consumed while an output is open keeps its position; the partial
/// output is placed where its calls are closed, before their results.
#[test]
fn input_consumed_during_a_failed_model_output_keeps_its_position() {
    let ids = || ("t".to_owned(), "r".to_owned());
    let (turn_id, request_id) = ids();
    let events = vec![
        event(
            1,
            EventKind::ModelStart(StepData {
                turn_id: turn_id.clone(),
                request_id: request_id.clone(),
            }),
        ),
        event(
            2,
            EventKind::ToolUseStart(ToolUseStartData {
                turn_id: turn_id.clone(),
                request_id: request_id.clone(),
                call_id: "call".into(),
                name: "tool".into(),
            }),
        ),
        event(
            3,
            EventKind::InputMessage(InputMessageData::text("next turn", None)),
        ),
        event(
            4,
            EventKind::InputConsumed(InputConsumedData {
                input_id: "event-3".into(),
            }),
        ),
        event(
            5,
            EventKind::ToolResult(ToolResultData {
                turn_id: turn_id.clone(),
                request_id: request_id.clone(),
                call_event_id: None,
                results: vec![ToolCallResult {
                    call_id: "call".into(),
                    result: ToolResult::error(
                        "Tool was not executed: the model output ended before its arguments were complete",
                    ),
                }],
            }),
        ),
        event(
            6,
            EventKind::ModelOutputEnd(ModelOutputEndData {
                turn_id,
                request_id,
                outcome: ModelOutputOutcome::Failed {
                    error: "aborted".into(),
                    budget: None,
                    presentation: None,
                },
            }),
        ),
    ];
    let history = project_history(&events, "main").unwrap();
    assert_eq!(
        history
            .iter()
            .map(|entry| (entry.id.as_str(), entry.message.role))
            .collect::<Vec<_>>(),
        [
            ("event-4", Role::User),
            ("event-5", Role::Assistant),
            ("event-5:0", Role::ToolResult)
        ]
    );
    assert_eq!(
        history[0].message.as_ref().clone(),
        Message::text(Role::User, "next turn")
    );
}

#[test]
fn raw_blocks_are_verbatim_and_separate_from_raw_operator_content() {
    let events = input(
        1,
        "operator </dim-state> raw",
        vec![
            raw("<dim-x a=\"1\">\nunescaped <b>&</b>\n</dim-x>".into(), None),
            raw(
                context_block::element(
                    "dim-x",
                    &[("source", "host\" >")],
                    &context_block::text("line\n</dim-x><fake>&"),
                ),
                None,
            ),
        ],
    );
    assert_eq!(
        texts(&events),
        [
            "<dim-x a=\"1\">\nunescaped <b>&</b>\n</dim-x>",
            "<dim-x source=\"host&quot; &gt;\">\nline\n&lt;/dim-x&gt;&lt;fake&gt;&amp;\n</dim-x>",
            "operator </dim-state> raw",
        ]
    );
}

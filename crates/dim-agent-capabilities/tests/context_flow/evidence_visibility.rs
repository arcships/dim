use super::*;
use dim_agent_capabilities::history::{context_messages, project_context_history};

fn tool_result(content: &str, structured: &str, context: Option<Message>) -> EventKind {
    let mut result = ToolResult::text(content);
    result.structured_content = Some(
        serde_json::from_value(serde_json::json!({
            "schema_sentinel": structured,
        }))
        .unwrap(),
    );
    result.context_messages = context.map(|message| vec![message]);
    EventKind::ToolResult(ToolResultData {
        turn_id: "turn".into(),
        request_id: "request".into(),
        call_event_id: None,
        results: vec![ToolCallResult {
            call_id: "call".into(),
            result,
        }],
    })
}

fn context(events: Vec<EventKind>) -> ContextHistory {
    let events: Vec<_> = events
        .into_iter()
        .enumerate()
        .map(|(index, kind)| {
            Arc::new(AgentEvent {
                id: format!("event-{index}"),
                sequence: index as u64 + 1,
                create_at: index as u64,
                tags: None,
                kind,
            })
        })
        .collect();
    ContextHistory {
        positions: events
            .iter()
            .map(|event| TimelinePosition {
                branch_id: "main".into(),
                sequence: event.sequence,
            })
            .collect(),
        events,
        checkpoint: None,
        ancestry: Vec::new(),
    }
}

fn diagnostics() -> Box<CompactionDiagnosticsRecord> {
    Box::new(
        serde_json::from_value(serde_json::json!({
            "reason": "manual",
            "trigger": "manual",
            "entriesReplaced": 1,
            "estimatedInputTokens": 1,
            "modelCallCount": 0,
            "durationMs": 0,
            "strategy": {"kind": "test"}
        }))
        .unwrap(),
    )
}

#[test]
fn tool_result_projection_keeps_content_and_explicit_context_but_not_internal_structured_content() {
    let explicit = Message::text(Role::User, "explicit-context-sentinel");
    let context = context(vec![
        EventKind::ModelStart(ModelStartData {
            turn_id: "turn".into(),
            request_id: "request".into(),
        }),
        tool_result(
            "content-sentinel",
            "structured-only-sentinel",
            Some(explicit.clone()),
        ),
    ]);

    let history = project_context_history(&context).unwrap();
    let messages = context_messages(&context, &history);
    let encoded = serde_json::to_string(&messages).unwrap();

    assert!(encoded.contains("content-sentinel"));
    assert!(encoded.contains("explicit-context-sentinel"));
    assert!(!encoded.contains("structured-only-sentinel"));
}

#[test]
fn compaction_replaces_old_tool_context_with_the_strategy_replacement() {
    let evidence = Message::text(
        Role::User,
        "TABLE_TITLE_SENTINEL COLUMN_HEADER_SENTINEL UNIT_SENTINEL SOURCE_SENTINEL",
    );
    let context = context(vec![
        EventKind::ModelStart(ModelStartData {
            turn_id: "turn".into(),
            request_id: "request".into(),
        }),
        tool_result("ordinary-tool-output", "internal-schema", Some(evidence)),
        EventKind::ContextCompaction(ContextCompactionData {
            turn_id: Some("turn".into()),
            request_id: Some("request".into()),
            command: None,
            start: None,
            through: 2,
            outcome: ContextCompactionResult::Completed {
                retain_from: "event-2".into(),
                replacement: vec![dim_agent_capabilities::history::summary_message("summary")],
                strategy: "test".into(),
                diagnostics: diagnostics(),
                context_baseline: Vec::new(),
                replacement_parts: None,
                pinned_inputs: None,
                digest_entries: None,
            },
        }),
    ]);

    let history = project_context_history(&context).unwrap();
    let messages = context_messages(&context, &history);
    let encoded = serde_json::to_string(&messages).unwrap();

    assert!(encoded.contains("summary"));
    assert!(!encoded.contains("TABLE_TITLE_SENTINEL"));
    assert!(!encoded.contains("ordinary-tool-output"));
    assert!(!encoded.contains("internal-schema"));
}

use dim_agent_types::*;
use serde_json::json;

fn branch() -> PromptScope {
    PromptScope::Branch {
        timeline: "main".into(),
    }
}

#[test]
fn prompt_and_context_contract_uses_the_shared_wire_shape() {
    let entry = PromptEntry {
        source: "workspace".into(),
        version: "v2".into(),
        body: "instructions".into(),
        scope: branch(),
        delivery: PromptDelivery::System,
        order: -10,
    };
    let changed = EventDraft::new(EventKind::PromptChanged(PromptChangedData {
        timeline: "main".into(),
        revision: "prompt-2".into(),
        previous: Some(PromptReference {
            timeline: "main".into(),
            revision: "prompt-1".into(),
        }),
        reason: PromptChangeReason::Refresh,
        changes: vec![PromptChange::Set {
            entry: entry.clone(),
        }],
    }));
    let value = serde_json::to_value(&changed).unwrap();
    assert_eq!(value["type"], "prompt");
    assert_eq!(value["name"], "changed");
    assert_eq!(value["data"]["changes"][0]["entry"]["delivery"], "system");
    assert_eq!(
        serde_json::from_value::<EventDraft>(value).unwrap(),
        changed
    );

    let input: InputMessageData = serde_json::from_value(json!({
        "blocks": [
            {"type":"raw","text":"<dim-state>a</dim-state>","key":"state:a"},
            {"type":"raw","text":"<dim-rule>ended</dim-rule>","key":"rule:a","retire":true},
            {"type":"raw","text":"once"}
        ]
    }))
    .unwrap();
    assert!(input.user.is_none());
    assert_eq!(
        serde_json::to_value(&input).unwrap()["blocks"][2],
        json!({"type":"raw","text":"once"})
    );
    assert!(matches!(
        &input.blocks[1],
        InputBlock::Raw { retire: true, .. }
    ));
    for retired in [
        json!({"state": [{"op":"set","source":"s","key":"k","content":"c"}]}),
        json!({"blocks": [{"type":"notification","source":"exec"}]}),
    ] {
        assert!(serde_json::from_value::<InputMessageData>(retired).is_err());
    }
    assert!(serde_json::from_value::<ToolResult>(json!({"content":"ok","prompts":[]})).is_err());
}

#[test]
fn v1_boundary_rejects_new_fields_and_hides_new_events() {
    let legacy_input = json!({
        "id":"1","sequence":1,"create_at":1,
        "type":"input","name":"message","data":{"text":"hello","context":[]}
    });
    assert!(serde_json::from_value::<v1::AgentEvent>(legacy_input).is_err());

    let event = AgentEvent {
        id: "1".into(),
        sequence: 1,
        create_at: 1,
        tags: None,
        kind: EventKind::PromptChanged(PromptChangedData {
            timeline: "main".into(),
            revision: "r1".into(),
            previous: None,
            reason: PromptChangeReason::Initialize,
            changes: Vec::new(),
        }),
    };
    assert!(v1::EventRef::new(&event).is_none());
}

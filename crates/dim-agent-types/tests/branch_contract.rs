use dim_agent_types::*;
use serde_json::json;
use std::sync::Arc;

#[test]
fn branch_creation_requires_internal_position_and_borrows_the_original_v1_payload() {
    let old = json!({"id":"create","sequence":9,"create_at":1750000000123_u64,"tags":["branch"],
    "type":"branch","name":"create","data":{
        "turn_id":"turn","request_id":"request","branch":"child","prompt":"hello","call_id":"call"
    }});
    let wire: v1::AgentEvent = serde_json::from_value(old.clone()).unwrap();
    assert_eq!(serde_json::to_value(wire).unwrap(), old);
    assert!(serde_json::from_value::<AgentEvent>(old.clone()).is_err());
    for at in [
        json!({"kind":"create"}),
        json!({"kind":"sequence","sequence":0}),
        json!({"kind":"sequence","sequence":8}),
    ] {
        let mut internal = old.clone();
        internal["data"]["at"] = at;
        internal["data"]["source"] = json!({
            "kind":"tool","turn_id":"turn","request_id":"request","call_id":"call"
        });
        internal["data"].as_object_mut().unwrap().remove("turn_id");
        internal["data"]
            .as_object_mut()
            .unwrap()
            .remove("request_id");
        internal["data"].as_object_mut().unwrap().remove("call_id");
        let event: AgentEvent = serde_json::from_value(internal.clone()).unwrap();
        assert_eq!(serde_json::to_value(&event).unwrap(), internal);
        assert_eq!(
            serde_json::to_value(v1::EventRef::new(&event).unwrap()).unwrap(),
            old
        );
        assert_eq!(
            serde_json::to_value(v1::SharedEvent::new(Arc::new(event)).unwrap()).unwrap(),
            old
        );
        assert!(serde_json::from_value::<v1::AgentEvent>(internal).is_err());
    }
    for at in [
        json!(null),
        json!({"kind":"create","sequence":2}),
        json!({"kind":"sequence"}),
        json!({"kind":"sequence","sequence":-1}),
        json!({"kind":"sequence","sequence":JS_MAX_SAFE_INTEGER+1}),
    ] {
        let mut internal = old.clone();
        internal["data"]["at"] = at;
        internal["data"]["source"] = json!({
            "kind":"tool","turn_id":"turn","request_id":"request","call_id":"call"
        });
        internal["data"].as_object_mut().unwrap().remove("turn_id");
        internal["data"]
            .as_object_mut()
            .unwrap()
            .remove("request_id");
        internal["data"].as_object_mut().unwrap().remove("call_id");
        assert!(serde_json::from_value::<AgentEvent>(internal).is_err());
    }
}

#[test]
fn every_internal_control_is_excluded_by_the_shared_v1_boundary() {
    for kind in [
        EventKind::InputConsumed(InputConsumedData {
            input_id: "input".into(),
        }),
        EventKind::StopInterrupt(StopInterruptData {
            reason: "stop".into(),
            scope: InterruptScope::Turn,
        }),
        EventKind::ApprovalRequest(ApprovalRequestData {
            approval_id: "approval".into(),
            source: BranchSource::Tool {
                turn_id: "turn".into(),
                request_id: "request".into(),
                call_id: "call".into(),
            },
            call: ApprovalCallData {
                name: "echo".into(),
                input: JsonObject::new(),
            },
            reason: "approval".into(),
        }),
        EventKind::ApprovalRespond(ApprovalRespondData {
            approval_id: "approval".into(),
            decision: ApprovalDecision::Allow,
        }),
        EventKind::BranchStart(BranchStartData {
            parent: None,
            create_event: None,
        }),
        EventKind::BranchResume(BranchResumeData {}),
    ] {
        let event = Arc::new(AgentEvent {
            id: "event".into(),
            sequence: 9,
            create_at: 1750000000123,
            tags: None,
            kind,
        });
        assert!(v1::EventRef::new(&event).is_none());
        assert!(v1::SharedEvent::new(event.clone()).is_none());
        assert!(
            serde_json::from_value::<v1::AgentEvent>(serde_json::to_value(&event).unwrap())
                .is_err()
        );
    }
}

#[test]
fn a_fork_records_its_copy_boundary_as_an_internal_fact() {
    let forked = EventKind::SessionForked(SessionForkedData {});
    let value = serde_json::to_value(&forked).unwrap();
    assert_eq!(value, json!({"type":"session","name":"forked","data":{}}));
    assert_eq!(serde_json::from_value::<EventKind>(value).unwrap(), forked);
    assert_eq!(forked.key(), "session/forked");
    let event = Arc::new(AgentEvent {
        id: "event".into(),
        sequence: 13,
        create_at: 1750000000123,
        tags: None,
        kind: forked,
    });
    assert!(v1::EventRef::new(&event).is_none());
}

#[test]
fn an_end_command_and_its_result_are_internal_branch_facts() {
    let tool = BranchSource::Tool {
        turn_id: "turn".into(),
        request_id: "request".into(),
        call_id: "call".into(),
    };
    let host = BranchSource::Host {
        operation_id: "goal:evaluate:9".into(),
    };
    let cases = [
        (
            EventKind::BranchEnd(BranchEndData {
                source: tool.clone(),
                ancestor: None,
            }),
            "branch/end",
            json!({"type":"branch","name":"end","data":{"source":{"kind":"tool","turn_id":"turn","request_id":"request","call_id":"call"}}}),
        ),
        (
            EventKind::BranchEnd(BranchEndData {
                source: host.clone(),
                ancestor: Some("parent".into()),
            }),
            "branch/end",
            json!({"type":"branch","name":"end","data":{"source":{"kind":"host","operation_id":"goal:evaluate:9"},"ancestor":"parent"}}),
        ),
        (
            EventKind::BranchEnded(BranchEndedData {
                branch: "child".into(),
                end_event: 9,
                source: host.clone(),
                ancestor: None,
            }),
            "branch/ended",
            json!({"type":"branch","name":"ended","data":{"branch":"child","endEvent":9,"source":{"kind":"host","operation_id":"goal:evaluate:9"}}}),
        ),
    ];
    for (kind, key, payload) in cases {
        assert_eq!(kind.key(), key);
        assert!(!kind.is_control());
        let event = Arc::new(AgentEvent {
            id: "end-event".into(),
            sequence: 9,
            create_at: 1750000000123,
            tags: None,
            kind,
        });
        assert_eq!(serde_json::to_value(&event.kind).unwrap(), payload);
        let mut envelope = payload.clone();
        envelope["id"] = json!(event.id);
        envelope["sequence"] = json!(event.sequence);
        envelope["create_at"] = json!(event.create_at);
        assert_eq!(serde_json::to_value(&*event).unwrap(), envelope);
        assert_eq!(
            serde_json::from_value::<AgentEvent>(envelope).unwrap(),
            *event
        );
        assert!(v1::EventRef::new(&event).is_none());
        assert!(v1::SharedEvent::new(event.clone()).is_none());
        assert!(
            serde_json::from_value::<v1::AgentEvent>(serde_json::to_value(&*event).unwrap())
                .is_err()
        );
    }
    for data in [
        json!({"source":{"kind":"tool","turn_id":"turn","request_id":"request","call_id":"call"},"ancestor":null}),
        json!({"ancestor":"parent"}),
        json!({"source":{"kind":"host","operation_id":"goal:evaluate:9"},"unknown":1}),
    ] {
        assert!(
            serde_json::from_value::<EventKind>(json!({"type":"branch","name":"end","data":data}))
                .is_err()
        );
    }
    for data in [
        json!({"branch":"child","endEvent":-1,"source":{"kind":"host","operation_id":"op"}}),
        json!({"branch":"child","endEvent":JS_MAX_SAFE_INTEGER+1,"source":{"kind":"host","operation_id":"op"}}),
        json!({"branch":"child","source":{"kind":"host","operation_id":"op"}}),
        json!({"endEvent":9,"source":{"kind":"host","operation_id":"op"}}),
        json!({"branch":"child","endEvent":9,"source":{"kind":"host","operation_id":"op"},"ancestor":null}),
    ] {
        assert!(
            serde_json::from_value::<EventKind>(
                json!({"type":"branch","name":"ended","data":data})
            )
            .is_err()
        );
    }
}

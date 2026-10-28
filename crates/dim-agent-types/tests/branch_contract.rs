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

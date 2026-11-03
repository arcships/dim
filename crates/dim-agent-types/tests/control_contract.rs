//! Rust timeline controls are intentionally separate from the frozen TS v1 union.
use dim_agent_types::*;
use serde_json::{Value, json};

#[test]
fn internal_controls_use_the_common_event_envelope() {
    let cases = [
        (
            EventKind::StopInterrupt(StopInterruptData {
                reason: "user stop".into(),
                scope: InterruptScope::Turn,
            }),
            "stop/interrupt",
            json!({"type":"stop","name":"interrupt","data":{"reason":"user stop","scope":"turn"}}),
        ),
        (
            EventKind::StopInterrupt(StopInterruptData {
                reason: "closing".into(),
                scope: InterruptScope::Branch,
            }),
            "stop/interrupt",
            json!({"type":"stop","name":"interrupt","data":{"reason":"closing","scope":"branch"}}),
        ),
        (
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
                reason: "permission required".into(),
            }),
            "approval/request",
            json!({"type":"approval","name":"request","data":{"approval_id":"approval","source":{"kind":"tool","turn_id":"turn","request_id":"request","call_id":"call"},"call":{"name":"echo","input":{}},"reason":"permission required"}}),
        ),
        (
            EventKind::ApprovalRespond(ApprovalRespondData {
                approval_id: "approval".into(),
                decision: ApprovalDecision::Allow,
            }),
            "approval/respond",
            json!({"type":"approval","name":"respond","data":{"approval_id":"approval","decision":"allow"}}),
        ),
        (
            EventKind::ApprovalCancelled(ApprovalCancelledData {
                approval_id: "approval".into(),
                reason: "Tool execution cancelled".into(),
            }),
            "approval/cancelled",
            json!({"type":"approval","name":"cancelled","data":{"approval_id":"approval","reason":"Tool execution cancelled"}}),
        ),
    ];
    for (kind, key, payload) in cases {
        assert!(kind.is_control());
        assert_eq!(kind.key(), key);
        let draft = EventDraft::new(kind.clone());
        assert_eq!(serde_json::to_value(&draft).unwrap(), payload);
        assert_eq!(
            serde_json::from_value::<EventDraft>(payload.clone()).unwrap(),
            draft
        );
        let event = AgentEvent {
            id: "control-event".into(),
            sequence: 27,
            create_at: 1_750_000_000_123,
            tags: None,
            kind,
        };
        let mut expected = payload;
        expected["id"] = json!(event.id);
        expected["sequence"] = json!(event.sequence);
        expected["create_at"] = json!(event.create_at);
        assert_eq!(serde_json::to_value(&event).unwrap(), expected);
        assert_eq!(
            serde_json::from_value::<AgentEvent>(expected).unwrap(),
            event
        );
    }
}

#[test]
fn controls_reject_ambiguous_or_unknown_payloads() {
    for value in [
        json!({"type":"stop","name":"interrupt","data":{"reason":"stop"}}),
        json!({"type":"stop","name":"interrupt","data":{"scope":"turn"}}),
        json!({"type":"stop","name":"interrupt","data":{"reason":"stop","scope":"session"}}),
        json!({"type":"stop","name":"interrupt","data":{"reason":"stop","scope":"Turn"}}),
        json!({"type":"stop","name":"interrupt","data":{"reason":"stop","scope":"turn","turn_id":"t"}}),
        json!({"type":"stop","name":"resume","data":{"reason":"resume"}}),
        json!({"type":"stop","name":"resume","data":Value::Null}),
        json!({"type":"stop","name":"resume"}),
        json!({"type":"approval","name":"respond","data":{"approval_id":"a","decision":"always"}}),
        json!({"type":"approval","name":"request","data":{"approval_id":"a"}}),
        json!({"type":"exec","name":"ended","data":{"task_id":"t","status":"running","result":{"content":"output"}}}),
        json!({"type":"exec","name":"started","data":{"turn_id":"t","request_id":"r","call_id":"c","task_id":"e","description":"command"}}),
        json!({"type":"permission","name":"changed","data":{"policy":{"preset":"allow-all"}}}),
        json!({"type":"step","name":"tool-use-start","data":{"turn_id":"t","request_id":"r","call_id":"c"}}),
        json!({"type":"step","name":"tool-result","data":{"turn_id":"t","request_id":"r","call_event_id":null,"results":[]}}),
    ] {
        assert!(
            serde_json::from_value::<EventDraft>(value.clone()).is_err(),
            "accepted {value}"
        );
    }
}

#[test]
fn native_product_events_round_trip_without_a_v1_view() {
    for payload in [
        json!({"type":"permission","name":"changed","data":{"policy":{"preset":"workspace-write","allowedTools":["read"],"disallowedTools":["exec"]}}}),
        json!({"type":"exec","name":"started","data":{"turn_id":"t","request_id":"r","call_id":"c","task_id":"exec-id","description":"cargo test","output_path":"/tmp/output.log"}}),
        json!({"type":"exec","name":"ended","data":{"task_id":"exec-id","status":"completed","result":{"content":"Output path: /tmp/output.log"}}}),
        json!({"type":"exec","name":"ended","data":{"task_id":"exec-id","status":"failed","result":{"content":"exit code 1"}}}),
        json!({"type":"exec","name":"ended","data":{"task_id":"exec-id","status":"cancelled","result":{"content":"stopped"}}}),
        json!({"type":"step","name":"tool-use-start","data":{"turn_id":"t","request_id":"r","call_id":"c","name":"echo"}}),
        json!({"type":"step","name":"tool-result","data":{"turn_id":"t","request_id":"r","results":[{"call_id":"c","result":{"content":"Tool was not executed: the model output ended","isError":true}}]}}),
    ] {
        let draft: EventDraft = serde_json::from_value(payload.clone()).unwrap();
        assert_eq!(serde_json::to_value(&draft).unwrap(), payload);
        let event = AgentEvent {
            id: "product-event".into(),
            sequence: 1,
            create_at: 1,
            tags: None,
            kind: draft.kind,
        };
        assert_eq!(
            serde_json::from_value::<AgentEvent>(serde_json::to_value(&event).unwrap()).unwrap(),
            event
        );
        assert!(v1::EventRef::new(&event).is_none());
    }
    let input = json!({"blocks":[{"type":"raw","text":"<dim-notification source=\"exec\" task=\"exec-id\">\ndone\n</dim-notification>"}],"mode":"steer"});
    let decoded: InputMessageData = serde_json::from_value(input.clone()).unwrap();
    assert_eq!(serde_json::to_value(decoded).unwrap(), input);
    let legacy = json!({"providerId":"p","modelId":"m"});
    let decoded: ModelConfiguration = serde_json::from_value(legacy.clone()).unwrap();
    assert!(decoded.tool_sources.is_empty());
    assert_eq!(serde_json::to_value(decoded).unwrap(), legacy);
}

#[test]
fn permission_history_ignores_removed_policy_keys_and_keeps_the_envelope_strict() {
    let policy = json!({"preset":"full-access","allowedTools":["read"],"disallowedTools":["exec"]});
    let mut legacy = policy.clone();
    legacy["fileAccess"] = json!("read-only");
    legacy["toolApproval"] = json!("ask");
    let value = json!({"type":"permission","name":"changed","data":{"policy":legacy}});
    let draft: EventDraft = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(
        serde_json::to_value(draft).unwrap()["data"]["policy"],
        policy
    );
    let mut invalid = value;
    invalid["data"]["unknown"] = json!(true);
    assert!(serde_json::from_value::<EventDraft>(invalid).is_err());
}

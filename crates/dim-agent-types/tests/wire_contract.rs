use dim_agent_types::*;
use serde_json::{Value, json};
use std::collections::BTreeSet;

fn fixture_values() -> Vec<Value> {
    serde_json::from_str(include_str!("fixtures/events.json")).unwrap()
}

fn fixture(kind: &str, status: Option<&str>) -> Value {
    fixture_values()
        .into_iter()
        .find(|value| {
            format!(
                "{}/{}",
                value["type"].as_str().unwrap(),
                value["name"].as_str().unwrap()
            ) == kind
                && status.is_none_or(|status| value["data"]["status"] == status)
        })
        .expect("named contract fixture")
}

fn equal_json(actual: &Value, expected: &Value) {
    match (actual, expected) {
        (Value::Number(a), Value::Number(b)) => assert_eq!(a.as_f64(), b.as_f64()),
        (Value::Array(a), Value::Array(b)) => {
            assert_eq!(a.len(), b.len());
            for (a, b) in a.iter().zip(b) {
                equal_json(a, b);
            }
        }
        (Value::Object(a), Value::Object(b)) => {
            assert_eq!(
                a.keys().collect::<BTreeSet<_>>(),
                b.keys().collect::<BTreeSet<_>>()
            );
            for (key, value) in a {
                equal_json(value, &b[key]);
            }
        }
        _ => assert_eq!(actual, expected),
    }
}

#[test]
fn frozen_typescript_samples_roundtrip_without_losing_fields() {
    let values = fixture_values();
    let mut kinds = BTreeSet::new();
    for value in &values {
        let event: v1::AgentEvent = serde_json::from_value(value.clone()).unwrap();
        kinds.insert(format!(
            "{}/{}",
            value["type"].as_str().unwrap(),
            value["name"].as_str().unwrap()
        ));
        equal_json(&serde_json::to_value(&event).unwrap(), value);
        if value["type"] == "context"
            && value["name"] == "compaction"
            && value["data"]["status"] == "completed"
        {
            assert!(serde_json::from_value::<AgentEvent>(value.clone()).is_err());
            let mut native = value.clone();
            let data = native["data"].as_object_mut().unwrap();
            data.remove("cursor");
            data.remove("summary");
            data.insert("retain_from".into(), json!("event-2"));
            data.insert(
                "replacement".into(),
                json!([{"role": "user", "content": "summary"}]),
            );
            data.insert("strategy".into(), json!("summary"));
            data.insert(
                "diagnostics".into(),
                json!({"reason": "auto", "trigger": "threshold", "estimatedInputTokens": 1,
                    "entriesReplaced": 1, "modelCallCount": 1, "durationMs": 1,
                    "strategy": {"kind": "summary", "summaryChars": 7}}),
            );
            let internal: AgentEvent = serde_json::from_value(native.clone()).unwrap();
            equal_json(&serde_json::to_value(&internal).unwrap(), &native);
            assert!(v1::SharedEvent::new(std::sync::Arc::new(internal)).is_none());
        }
    }
    assert_eq!(kinds.len(), 22);
    assert_eq!(values.len(), 42);
}

#[test]
fn malformed_events_are_rejected_at_the_wire_boundary() {
    let reject = |value: Value| {
        assert!(
            serde_json::from_value::<v1::AgentEvent>(value.clone()).is_err(),
            "accepted {value}"
        )
    };
    let mut value = fixture("input/message", None);
    value["name"] = json!("start");
    reject(value);
    let mut value = fixture("input/message", None);
    value["data"]["mode"] = json!("invalid");
    reject(value);
    let mut value = fixture("input/message", None);
    value["data"]["mode"] = Value::Null;
    reject(value);
    let mut value = fixture("input/message", None);
    value["tags"] = Value::Null;
    reject(value);
    let mut value = fixture("input/message", None);
    value["sequence"] = json!(0);
    reject(value);
    let mut value = fixture("input/message", None);
    value["sequence"] = json!(-1);
    reject(value);
    let mut value = fixture("input/message", None);
    value["sequence"] = json!(1.5);
    reject(value);
    let mut value = fixture("input/message", None);
    value["create_at"] = json!(JS_MAX_SAFE_INTEGER + 1);
    reject(value);
    let mut value = fixture("model/call", None);
    value["data"]["request"]["providerId"] = Value::Null;
    reject(value);
    let mut value = fixture("model/call", None);
    value["data"]["request"]["tools"][0]["inputSchema"]["type"] = Value::Null;
    reject(value);
    let mut value = fixture("model/call", None);
    value["data"]["request"]["messages"][0]["role"] = json!("system");
    reject(value);
    let mut value = fixture("model/call", None);
    value["data"]["request"]["config"]["reasoning"]["enabled"] = json!(false);
    reject(value);
    let mut value = fixture("model/usage", None);
    value["data"]["usage"]["promptTokens"] = json!("100");
    reject(value);
    let mut value = fixture("step/model-output-end", Some("failed"));
    value["data"].as_object_mut().unwrap().remove("error");
    reject(value);
    let mut value = fixture("step/tool-completed", None);
    value["data"]["result"]["content"][0]["type"] = json!("audio");
    reject(value);
    let mut value = fixture("step/tool-completed", None);
    value["data"]["result"]["backgroundTask"]["wakePolicy"] = json!("soon");
    reject(value);
    let mut value = fixture("step/tool-completed", None);
    value["data"]["result"]["blobRefs"][0]["sizeBytes"] = json!("23");
    reject(value);
    for (kind, status, path) in [
        ("input/message", None, ""),
        ("input/message", None, "/data"),
        ("step/model-output-end", Some("completed"), "/data"),
        ("model/call", None, "/data/request"),
        ("model/call", None, "/data/request/config"),
        ("model/call", None, "/data/request/config/providerOptions"),
    ] {
        let mut value = fixture(kind, status);
        value
            .pointer_mut(path)
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert("unexpectedField".into(), json!(true));
        reject(value);
    }
}

#[test]
fn provider_presentation_roundtrips_and_rejects_invalid_contracts() {
    let old = fixture("step/model-output-end", Some("failed"));
    let old_event: AgentEvent = serde_json::from_value(old.clone()).unwrap();
    equal_json(&serde_json::to_value(old_event).unwrap(), &old);

    let mut event = old;
    event["data"]["presentation"] = json!({
        "source": "fixture_provider",
        "reason": "provider_denied",
        "serverMessage": "contact provider"
    });
    let decoded: AgentEvent = serde_json::from_value(event.clone()).unwrap();
    equal_json(&serde_json::to_value(decoded).unwrap(), &event);
    let decoded_v1: v1::AgentEvent = serde_json::from_value(event.clone()).unwrap();
    equal_json(&serde_json::to_value(decoded_v1).unwrap(), &event);

    for presentation in [
        json!(null),
        json!({"source":" ","reason":"unknown_error"}),
        json!({"source":"fixture_provider","reason":" "}),
        json!({"source":"fixture_provider","reason":"unknown_error","serverMessage":null}),
        json!({"source":"fixture_provider","reason":"unknown_error","requestId":" "}),
        json!({"source":"fixture_provider","reason":"unknown_error","action":" "}),
        json!({"source":"fixture_provider","reason":"unknown_error","unexpected":true}),
    ] {
        let mut invalid = fixture("step/model-output-end", Some("failed"));
        invalid["data"]["presentation"] = presentation;
        assert!(serde_json::from_value::<AgentEvent>(invalid).is_err());
    }
}

#[test]
fn model_failure_roundtrips_and_rejects_invalid_contracts() {
    let old = fixture("step/model-output-end", Some("failed"));
    // An event written before the failure fact still decodes and re-encodes
    // without one.
    let old_event: AgentEvent = serde_json::from_value(old.clone()).unwrap();
    equal_json(&serde_json::to_value(old_event).unwrap(), &old);

    let mut event = old;
    event["data"]["failure"] = json!({
        "kind": "rate_limited",
        "status": 429,
        "providerCode": "rate_limit_exceeded",
        "requestId": "request-429",
        "reason": "slow down",
        "cause": ["slow down"],
        "attempts": 3,
        "waitedMs": 1500,
        "retryAfterMs": 4000
    });
    let decoded: AgentEvent = serde_json::from_value(event.clone()).unwrap();
    equal_json(&serde_json::to_value(decoded).unwrap(), &event);
    let decoded_v1: v1::AgentEvent = serde_json::from_value(event.clone()).unwrap();
    equal_json(&serde_json::to_value(decoded_v1).unwrap(), &event);

    for failure in [
        json!(null),
        json!({"kind":"not_a_kind","attempts":1,"waitedMs":0}),
        json!({"kind":"network","attempts":"many","waitedMs":0}),
        json!({"kind":"network","attempts":1,"waitedMs":0,"unexpected":true}),
        json!({"kind":"network","status":70000,"attempts":1,"waitedMs":0}),
    ] {
        let mut invalid = fixture("step/model-output-end", Some("failed"));
        invalid["data"]["failure"] = failure;
        assert!(
            serde_json::from_value::<AgentEvent>(invalid.clone()).is_err(),
            "{invalid}"
        );
    }
}

#[test]
fn failure_chain_roundtrips_and_rejects_invalid_contracts() {
    use std::sync::Arc;
    // A binary without `failure_chain` cannot decode a record that carries
    // it: `ContextPrepareData` and `ModelCallData` deny unknown fields. The
    // frozen fixtures stay unchanged.
    for kind in ["context/prepare", "model/call"] {
        let old = fixture(kind, None);
        let old_event: AgentEvent = serde_json::from_value(old.clone()).unwrap();
        equal_json(&serde_json::to_value(old_event).unwrap(), &old);

        let mut event = old;
        event["data"]["failure_chain"] = json!({
            "from_request_id": "request-1",
            "attempts": 5,
            "waited_ms": 1500
        });
        let decoded: AgentEvent = serde_json::from_value(event.clone()).unwrap();
        equal_json(&serde_json::to_value(&decoded).unwrap(), &event);
        let outbound =
            serde_json::to_value(v1::SharedEvent::new(Arc::new(decoded)).unwrap()).unwrap();
        assert!(
            outbound["data"].get("failure_chain").is_none(),
            "{outbound}"
        );
    }

    for chain in [
        json!(null),
        json!({"from_request_id": "request-1", "attempts": 5}),
        json!({"from_request_id": 1, "attempts": 5, "waited_ms": 0}),
        json!({"from_request_id": "request-1", "attempts": 300, "waited_ms": 0}),
        json!({"from_request_id": "request-1", "attempts": 5, "waited_ms": 0, "unexpected": true}),
    ] {
        let mut invalid = fixture("context/prepare", None);
        invalid["data"]["failure_chain"] = chain;
        assert!(
            serde_json::from_value::<AgentEvent>(invalid.clone()).is_err(),
            "{invalid}"
        );
    }
}

#[test]
fn reasoning_tokens_are_optional_and_absent_when_unreported() {
    // A `model/usage` record written before the field still decodes and
    // re-encodes without it; the frozen fixtures stay unchanged.
    let old = fixture("model/usage", None);
    let old_event: AgentEvent = serde_json::from_value(old.clone()).unwrap();
    equal_json(&serde_json::to_value(old_event).unwrap(), &old);

    let mut event = old;
    event["data"]["usage"]["reasoningTokens"] = json!(2.0);
    let decoded: AgentEvent = serde_json::from_value(event.clone()).unwrap();
    equal_json(&serde_json::to_value(&decoded).unwrap(), &event);
    // v1 still decodes that record; the projection drops the field, which
    // `a_v1_connection_receives_usage_without_its_reasoning_count` covers.
    let decoded_v1: v1::AgentEvent = serde_json::from_value(event).unwrap();
    assert!(
        matches!(
            &decoded_v1.kind,
            v1::EventKind::Model(v1::ModelEvent::ModelUsage(data))
                if data.usage.reasoning_tokens == Some(2.0)
        ),
        "{decoded_v1:?}"
    );

    // A reported zero is a count; an absent one is unreported, and explicit
    // null is not a count.
    let mut zero = fixture("model/usage", None);
    zero["data"]["usage"]["reasoningTokens"] = json!(0.0);
    let decoded: AgentEvent = serde_json::from_value(zero.clone()).unwrap();
    equal_json(&serde_json::to_value(&decoded).unwrap(), &zero);
    let mut null = fixture("model/usage", None);
    null["data"]["usage"]["reasoningTokens"] = Value::Null;
    assert!(
        serde_json::from_value::<AgentEvent>(null.clone()).is_err(),
        "{null}"
    );
    let mut non_finite = fixture("model/usage", None);
    non_finite["data"]["usage"]["reasoningTokens"] = json!("2");
    assert!(
        serde_json::from_value::<AgentEvent>(non_finite.clone()).is_err(),
        "{non_finite}"
    );
}

#[test]
fn retry_forbidden_roundtrips_and_rejects_invalid_contracts() {
    use std::sync::Arc;
    let old = fixture("step/model-output-end", Some("failed"));
    // An event written before the retry-policy fact still decodes and
    // re-encodes without it.
    let old_event: AgentEvent = serde_json::from_value(old.clone()).unwrap();
    equal_json(&serde_json::to_value(old_event).unwrap(), &old);

    let mut event = old;
    event["data"]["failure"] = json!({
        "kind": "server_error",
        "status": 503,
        "cause": [],
        "attempts": 1,
        "waitedMs": 0,
        "retryForbidden": true
    });
    let decoded: AgentEvent = serde_json::from_value(event.clone()).unwrap();
    equal_json(&serde_json::to_value(&decoded).unwrap(), &event);
    let decoded_v1: v1::AgentEvent = serde_json::from_value(event.clone()).unwrap();
    equal_json(&serde_json::to_value(decoded_v1).unwrap(), &event);
    let outbound = serde_json::to_value(
        v1::SharedEvent::new(Arc::new(
            serde_json::from_value::<AgentEvent>(event.clone()).unwrap(),
        ))
        .unwrap(),
    )
    .unwrap();
    equal_json(&outbound, &event);

    let mut allowed = fixture("step/model-output-end", Some("failed"));
    allowed["data"]["failure"] = json!({
        "kind": "server_error",
        "attempts": 1,
        "waitedMs": 0,
        "retryForbidden": false
    });
    let decoded: AgentEvent = serde_json::from_value(allowed).unwrap();
    let encoded = serde_json::to_value(&decoded).unwrap();
    assert!(
        encoded["data"]["failure"].get("retryForbidden").is_none(),
        "{encoded}"
    );

    for forbidden in [json!("yes"), json!(1), json!(null)] {
        let mut invalid = fixture("step/model-output-end", Some("failed"));
        invalid["data"]["failure"] = json!({
            "kind": "server_error",
            "attempts": 1,
            "waitedMs": 0,
            "retryForbidden": forbidden
        });
        assert!(
            serde_json::from_value::<AgentEvent>(invalid.clone()).is_err(),
            "{invalid}"
        );
    }
}

#[test]
fn handover_roundtrips_and_rejects_invalid_contracts() {
    use std::sync::Arc;
    // The frozen fixtures hold `input/message` records only in the v1
    // {text, mode} shape, so the old record below is written in the
    // internal shape by hand. A binary without `handover` cannot decode a
    // record that carries it: `InputMessageData` denies unknown fields.
    let old = json!({
        "id": "event-1",
        "sequence": 1,
        "create_at": 1790000000123u64,
        "type": "input",
        "name": "message",
        "data": {
            "user": {"content": [{"type": "text", "text": "handover"}]},
            "mode": "queue"
        }
    });
    let old_event: AgentEvent = serde_json::from_value(old.clone()).unwrap();
    equal_json(&serde_json::to_value(old_event).unwrap(), &old);

    let mut event = old.clone();
    event["data"]["handover"] = json!({"turn_id": "turn-2", "request_id": "request-3"});
    let decoded: AgentEvent = serde_json::from_value(event.clone()).unwrap();
    equal_json(&serde_json::to_value(&decoded).unwrap(), &event);
    let outbound = serde_json::to_value(v1::SharedEvent::new(Arc::new(decoded)).unwrap()).unwrap();
    assert!(outbound["data"].get("handover").is_none(), "{outbound}");

    // The v1 inbound view knows only text and mode, so it refuses the
    // field like every field it does not know.
    let mut v1_event = json!({
        "id": "event-1",
        "sequence": 1,
        "create_at": 1790000000123u64,
        "type": "input",
        "name": "message",
        "data": {"text": "handover", "mode": "queue"}
    });
    assert!(serde_json::from_value::<v1::AgentEvent>(v1_event.clone()).is_ok());
    v1_event["data"]["handover"] = json!({"turn_id": "turn-2", "request_id": "request-3"});
    assert!(
        serde_json::from_value::<v1::AgentEvent>(v1_event.clone()).is_err(),
        "{v1_event}"
    );

    for handover in [
        json!(null),
        json!({"turn_id": "turn-2"}),
        json!({"turn_id": 2, "request_id": "request-3"}),
        json!({"turn_id": "turn-2", "request_id": "request-3", "unexpected": true}),
    ] {
        let mut invalid = old.clone();
        invalid["data"]["handover"] = handover;
        assert!(
            serde_json::from_value::<AgentEvent>(invalid.clone()).is_err(),
            "{invalid}"
        );
    }
}

#[test]
fn omitted_modes_and_safe_integer_limits_keep_the_contract() {
    let mut value = fixture("input/message", None);
    value["sequence"] = json!(JS_MAX_SAFE_INTEGER);
    value["create_at"] = json!(JS_MAX_SAFE_INTEGER);
    let mut event: v1::AgentEvent = serde_json::from_value(value.clone()).unwrap();
    let v1::EventKind::Input(v1::InputEvent::InputMessage(input)) = &event.kind else {
        panic!("wrong event kind")
    };
    assert_eq!(input.effective_mode(), InputMode::Queue);
    assert_eq!(input.mode, None);
    equal_json(&serde_json::to_value(&event).unwrap(), &value);
    event.sequence = JS_MAX_SAFE_INTEGER + 1;
    assert!(serde_json::to_value(event).is_err());

    let equivalent: v1::AgentEvent = serde_json::from_str(
        r#"{"id":"number-spelling","sequence":1e0,"create_at":1000.0,"type":"input","name":"message","data":{"text":"hello"}}"#
    ).unwrap();
    assert_eq!(equivalent.sequence, 1);
    assert_eq!(equivalent.create_at, 1000);
}

#[test]
fn request_events_preserve_draft_encoding() {
    use std::sync::Arc;
    let request = Arc::new(ModelRequest::new(
        "p",
        "m",
        vec![Message::text(Role::User, "first")],
    ));
    let call = ModelCallData {
        turn_id: "turn".into(),
        request_id: "request".into(),
        request,
        configuration: None,
        prompt: None,
        failure_chain: None,
    };
    let draft = EventDraft::new(EventKind::ModelCall(call));
    assert_eq!(
        serde_json::to_value(&draft).unwrap(),
        json!({"type":"model","name":"call","data":{
            "turn_id":"turn","request_id":"request",
            "request":{"providerId":"p","modelId":"m","messages":[
                {"role":"user","content":"first"}
            ]}
        }})
    );
}

#[test]
fn producer_numbers_cannot_silently_become_json_null() {
    let mut event: v1::AgentEvent = serde_json::from_value(fixture("model/usage", None)).unwrap();
    let v1::EventKind::Model(v1::ModelEvent::ModelUsage(data)) = &mut event.kind else {
        panic!("wrong event kind")
    };
    data.usage.prompt_tokens = f64::NAN;
    assert!(serde_json::to_value(&event).is_err());
    let v1::EventKind::Model(v1::ModelEvent::ModelUsage(data)) = &mut event.kind else {
        panic!("wrong event kind")
    };
    data.usage.prompt_tokens = 100.0;
    data.usage.cost = Some(f64::INFINITY);
    assert!(serde_json::to_value(event).is_err());
}

#[test]
fn capability_snapshot_validates_identity_limits_and_wire_absence() {
    let snapshot = ModelCapabilitySnapshot {
        target: ModelId {
            provider_id: "p".into(),
            model_id: "m".into(),
        },
        capabilities: ModelCapabilities::with_limits(128_000, 16_384),
        reasoning: None,
        service_tier: None,
    };
    assert_eq!(snapshot.validate_for("p", "m"), Ok(()));
    assert!(snapshot.validate_for("p", "other").is_err());
    let mut invalid = snapshot.clone();
    invalid.capabilities.context_window = 0;
    assert!(invalid.validate_for("p", "m").is_err());

    let request = ModelRequest::new("p", "m", Vec::new());
    assert!(
        !serde_json::to_value(&request)
            .unwrap()
            .as_object()
            .unwrap()
            .contains_key("capabilitySnapshot")
    );
    let mut wire = serde_json::to_value(request).unwrap();
    wire["capabilitySnapshot"] = serde_json::Value::Null;
    assert!(serde_json::from_value::<ModelRequest>(wire).is_err());
    assert!(
        serde_json::from_value::<ModelSelection>(json!({
            "providerId": "p", "modelId": "m", "capabilitySnapshot": null
        }))
        .is_err()
    );
    assert!(
        serde_json::from_value::<ModelConfiguration>(json!({
            "providerId": "p", "modelId": "m", "capabilitySnapshot": null
        }))
        .is_err()
    );
}

#[test]
fn bedrock_continuation_metadata_is_strict_and_roundtrips() {
    for (metadata, expected) in [
        (
            ContentMetadata::Bedrock { signature: None },
            json!({"protocol":"bedrock"}),
        ),
        (
            ContentMetadata::Bedrock {
                signature: Some("signed-A".into()),
            },
            json!({"protocol":"bedrock", "signature":"signed-A"}),
        ),
        (
            ContentMetadata::BedrockRedacted {
                data: "opaque-B".into(),
            },
            json!({"protocol":"bedrock-redacted", "data":"opaque-B"}),
        ),
    ] {
        let value = serde_json::to_value(&metadata).unwrap();
        assert_eq!(value, expected);
        assert_eq!(
            serde_json::from_value::<ContentMetadata>(expected).unwrap(),
            metadata
        );
    }
    assert!(
        serde_json::from_value::<ContentMetadata>(json!({
            "protocol":"bedrock", "signature":null
        }))
        .is_err()
    );
    assert!(
        serde_json::from_value::<ContentMetadata>(json!({
            "protocol":"bedrock-redacted", "data":"opaque", "extra":true
        }))
        .is_err()
    );
}

#[test]
fn tool_effects_round_trip_and_refuse_an_unknown_kind() {
    let result = ToolResult {
        effects: Some(vec![
            ToolEffect::FileWritten {
                path: "/work/dim/src/lib.rs".into(),
            },
            ToolEffect::CommandRun {
                command: "cargo test".into(),
                exit_code: Some(0),
                signal: None,
            },
            ToolEffect::CommandRun {
                command: "npm run dev".into(),
                exit_code: None,
                signal: None,
            },
        ]),
        ..ToolResult::text("done")
    };
    let value = serde_json::to_value(&result).unwrap();
    assert_eq!(
        value["effects"],
        json!([
            {"kind": "fileWritten", "path": "/work/dim/src/lib.rs"},
            {"kind": "commandRun", "command": "cargo test", "exitCode": 0},
            {"kind": "commandRun", "command": "npm run dev"},
        ])
    );
    assert_eq!(serde_json::from_value::<ToolResult>(value).unwrap(), result);
    assert!(
        serde_json::from_value::<ToolResult>(json!({
            "content": "x",
            "effects": [{"kind": "fileDeleted", "path": "/work/dim/src/lib.rs"}]
        }))
        .is_err(),
        "an unknown effect kind is refused at the boundary that decodes it"
    );
    assert!(
        serde_json::from_value::<ToolResult>(json!({
            "content": "x",
            "effects": [{"kind": "commandRun", "command": "ls", "cwd": "/"}]
        }))
        .is_err(),
        "an effect with a field outside the vocabulary is refused"
    );
}

/// The read pages and the compaction digest show an effect by the same rule:
/// a command keeps its first 200 characters, a longer one is cut there and
/// marked with `…`, and a path stays verbatim (#1033).
#[test]
fn a_shortened_effect_cuts_a_command_at_two_hundred_characters() {
    assert_eq!(
        ToolEffect::CommandRun {
            command: "c".repeat(200),
            exit_code: None,
            signal: None,
        }
        .shortened(),
        ToolEffect::CommandRun {
            command: "c".repeat(200),
            exit_code: None,
            signal: None,
        },
        "a 200-character command is unchanged"
    );
    for (exit_code, signal) in [(Some(0), None), (None, Some("SIGTERM".into()))] {
        let cut = ToolEffect::CommandRun {
            command: format!("{}tail", "c".repeat(200)),
            exit_code,
            signal: signal.clone(),
        }
        .shortened();
        assert_eq!(
            cut,
            ToolEffect::CommandRun {
                command: format!("{}…", "c".repeat(200)),
                exit_code,
                signal,
            },
            "a longer command keeps exactly 200 characters plus the cut mark"
        );
    }
    let cut = ToolEffect::CommandRun {
        command: format!("{}tail", "中".repeat(200)),
        exit_code: None,
        signal: None,
    }
    .shortened();
    assert_eq!(
        cut,
        ToolEffect::CommandRun {
            command: format!("{}…", "中".repeat(200)),
            exit_code: None,
            signal: None,
        },
        "a multibyte command is cut on a character boundary"
    );
    let path = format!("/work/dim/src/{}", "x".repeat(400));
    assert_eq!(
        ToolEffect::FileWritten { path: path.clone() }.shortened(),
        ToolEffect::FileWritten { path },
        "a path stays verbatim, whatever its length"
    );
}

#[test]
fn a_v1_connection_receives_a_tool_result_without_its_effects() {
    let event = AgentEvent {
        id: "event-1".into(),
        sequence: 1,
        create_at: 0,
        tags: None,
        parent_call_id: None,
        kind: EventKind::ToolResult(ToolResultData {
            turn_id: "turn-1".into(),
            request_id: "request-1".into(),
            call_event_id: Some("batch-1".into()),
            results: vec![ToolCallResult {
                call_id: "call-1".into(),
                result: ToolResult {
                    effects: Some(vec![ToolEffect::FileWritten {
                        path: "/work/dim/src/lib.rs".into(),
                    }]),
                    ..ToolResult::text("Wrote /work/dim/src/lib.rs")
                },
            }],
        }),
    };
    let shared = v1::SharedEvent::new(std::sync::Arc::new(event)).expect("a dispatched result");
    let value = serde_json::to_value(&shared).unwrap();
    let result = &value["data"]["results"][0]["result"];
    assert!(result.get("effects").is_none(), "{value}");
    assert_eq!(result["content"], "Wrote /work/dim/src/lib.rs");
    assert!(result.get("isError").is_none(), "{value}");
}

#[test]
fn a_v1_connection_receives_usage_without_its_reasoning_count() {
    let event = AgentEvent {
        id: "event-1".into(),
        sequence: 1,
        create_at: 0,
        tags: None,
        parent_call_id: None,
        kind: EventKind::ModelUsage(ModelUsageData {
            turn_id: "turn-1".into(),
            request_id: "request-1".into(),
            usage: Usage {
                prompt_tokens: 100.0,
                completion_tokens: 20.0,
                total_tokens: 120.0,
                cache_read_tokens: Some(30.0),
                cache_write_tokens: Some(10.0),
                reasoning_tokens: Some(2.0),
                cost: Some(0.001),
                currency: Some("USD".into()),
            },
        }),
    };
    let shared = v1::SharedEvent::new(std::sync::Arc::new(event)).expect("a usage fact");
    let value = serde_json::to_value(&shared).unwrap();
    assert!(
        value["data"]["usage"].get("reasoningTokens").is_none(),
        "{value}"
    );
    // Every other field keeps the v1 shape and value.
    equal_json(
        &value["data"],
        &json!({
            "turn_id": "turn-1",
            "request_id": "request-1",
            "usage": {
                "promptTokens": 100,
                "completionTokens": 20,
                "totalTokens": 120,
                "cacheReadTokens": 30,
                "cacheWriteTokens": 10,
                "cost": 0.001,
                "currency": "USD"
            }
        }),
    );
}

#[test]
fn an_adopted_configuration_stores_no_derived_preferences() {
    let settings = ModelConfiguration {
        provider_id: "provider".into(),
        model_id: "model".into(),
        capability_snapshot: None,
        tools: Some(Vec::new()),
        tool_sources: Vec::new(),
        config: Some(ModelConfig::default()),
        metadata: Some(RequestMetadata::default()),
    };
    let value = serde_json::to_value(&settings).unwrap();
    let keys: BTreeSet<&str> = value
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        BTreeSet::from(["providerId", "modelId", "tools", "config", "metadata"]),
        "{value}"
    );
}

#[test]
fn a_system_source_round_trips_and_an_older_record_keeps_its_system() {
    let data = ContextConfiguredData {
        timeline: "main".into(),
        revision: "configuration".into(),
        previous: None,
        prompt: None,
        settings: Box::new(ModelConfiguration {
            provider_id: "provider".into(),
            model_id: "model".into(),
            capability_snapshot: None,
            tools: None,
            tool_sources: Vec::new(),
            config: None,
            metadata: None,
        }),
        system: Some(SystemPrompt::Text(
            "You are helpful.\n\n# How to read your input".into(),
        )),
        system_source: Some("You are helpful.".into()),
    };
    let value = serde_json::to_value(&data).unwrap();
    assert_eq!(value["system_source"], "You are helpful.");
    assert_eq!(
        serde_json::from_value::<ContextConfiguredData>(value.clone()).unwrap(),
        data
    );
    let without = serde_json::to_value(ContextConfiguredData {
        system_source: None,
        ..data.clone()
    })
    .unwrap();
    assert!(without.get("system_source").is_none(), "{without}");
    let mut older = value;
    older.as_object_mut().unwrap().remove("system_source");
    let decoded: ContextConfiguredData = serde_json::from_value(older).unwrap();
    assert_eq!(decoded.system_source, None);
    assert_eq!(decoded.system, data.system);
}

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

/// Version 1 carries display content only: the model-request projection strips
/// `providerMetadata` from text, thinking and tool-use parts, while native
/// continuation stays on the Timeline. A new continuation field or a serializer
/// change that stops stripping exposes native-only metadata and breaks the
/// frozen v1 request shape.
#[test]
fn request_events_share_the_snapshot_and_preserve_draft_encoding() {
    use std::sync::Arc;
    let request = Arc::new(ModelRequest::new(
        "p",
        "m",
        vec![Message::text(Role::User, "first")],
    ));
    let prepare = ContextPrepareData {
        turn_id: "turn".into(),
        request_id: "request".into(),
        through: 4,
        request: request.clone(),
        configuration: None,
        prompt: None,
        recovery: None,
    };
    let call = ModelCallData {
        turn_id: "turn".into(),
        request_id: "request".into(),
        request,
        configuration: None,
        prompt: None,
    };
    assert!(Arc::ptr_eq(&prepare.request, &call.request));
    let mut shared = (*call.request).clone();
    shared.tools = Some(Arc::new(Vec::new()));
    shared.system = Some(Arc::new(SystemPrompt::Text("system".into())));
    let copied = shared.clone();
    assert!(Arc::ptr_eq(&shared.messages[0], &copied.messages[0]));
    assert!(Arc::ptr_eq(
        shared.tools.as_ref().unwrap(),
        copied.tools.as_ref().unwrap()
    ));
    assert!(Arc::ptr_eq(
        shared.system.as_ref().unwrap(),
        copied.system.as_ref().unwrap()
    ));
    assert_eq!(
        serde_json::to_value(&shared).unwrap(),
        serde_json::to_value(&copied).unwrap()
    );
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
        capabilities: ModelCapabilities::default(),
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

#[test]
fn a_v1_connection_receives_a_tool_result_without_its_effects() {
    let event = AgentEvent {
        id: "event-1".into(),
        sequence: 1,
        create_at: 0,
        tags: None,
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

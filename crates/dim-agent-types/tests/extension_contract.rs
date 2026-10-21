//! Extension events (ADR 001, issue #2): types the core does not define are
//! carried unchanged in the common envelope; the core's domains stay reserved
//! for its typed events.
use dim_agent_types::*;
use serde_json::json;

fn event(kind: serde_json::Value) -> serde_json::Value {
    let mut event = json!({"id":"e1","sequence":7,"create_at":1});
    event
        .as_object_mut()
        .unwrap()
        .extend(kind.as_object().unwrap().clone());
    event
}

/// `kind` as event text with `type` first, as events are written, and with
/// `type` last, as a sorted map yields it.
fn texts(kind: &serde_json::Value) -> [String; 2] {
    let field = |key: &str| format!("{key:?}:{}", kind[key]);
    let envelope = r#""id":"e1","sequence":7,"create_at":1"#;
    let (domain, name, data) = (field("type"), field("name"), field("data"));
    [
        format!("{{{envelope},{domain},{name},{data}}}"),
        format!("{{{data},{name},{domain},{envelope}}}"),
    ]
}

#[test]
fn an_undefined_type_round_trips_as_an_extension_event() {
    let wire = event(json!({"type":"goal","name":"set","data":{"objective":"ship","n":[1,2]}}));
    let decoded: AgentEvent = serde_json::from_value(wire.clone()).unwrap();
    let EventKind::Extension(extension) = &decoded.kind else {
        panic!("{decoded:?}");
    };
    assert_eq!(
        (extension.domain(), extension.name(), decoded.kind.key()),
        ("goal", "set", "goal/set")
    );
    assert_eq!(extension.data(), &json!({"objective":"ship","n":[1,2]}));
    assert_eq!(serde_json::to_value(&decoded).unwrap(), wire);
    for text in texts(&json!({"type":"goal","name":"set","data":{"objective":"ship","n":[1,2]}})) {
        assert_eq!(serde_json::from_str::<AgentEvent>(&text).unwrap(), decoded);
    }
    // The frozen v1 union has no view of it.
    assert!(v1::EventRef::new(&decoded).is_none());
}

#[test]
fn reserved_domains_decode_only_through_their_typed_contract() {
    for domain in RESERVED_DOMAINS {
        assert!(
            ExtensionEvent::new(domain, "anything", json!({})).is_err(),
            "{domain}"
        );
    }
    // A core event with a payload outside its contract, or an unknown name in
    // a core domain, fails with its typed decoder's own error, which names the
    // field or name, never as an extension event; whichever comes first in
    // the map.
    for (kind, cause) in [
        (
            json!({"type":"input","name":"message","data":{"blocks":[],"unexpected":1}}),
            "unknown field `unexpected`, expected one of `submission_id`",
        ),
        (
            json!({"type":"turn","name":"end","data":{"status":"completed","reason":"end_turn"}}),
            "missing field `turn_id`",
        ),
        (
            json!({"type":"input","name":"message","data":{"blocks":1}}),
            "invalid type: integer `1`, expected a sequence",
        ),
        (
            json!({"type":"input","name":"forged","data":{}}),
            "unknown variant `forged`, expected one of `message`",
        ),
        (
            json!({"type":"turn","name":"end","data":null}),
            "invalid type: null, expected struct TurnEndData",
        ),
    ] {
        let value = serde_json::from_value::<AgentEvent>(event(kind.clone())).unwrap_err();
        for error in texts(&kind)
            .map(|text| serde_json::from_str::<AgentEvent>(&text).unwrap_err())
            .into_iter()
            .chain([value])
        {
            assert!(error.to_string().starts_with(cause), "{kind}: {error}");
        }
    }
    // Every reserved domain has its typed decoder.
    for domain in RESERVED_DOMAINS {
        let kind = json!({"type":domain,"name":"anything","data":{}});
        let error = serde_json::from_value::<AgentEvent>(event(kind)).unwrap_err();
        assert!(
            error.to_string().starts_with("unknown variant `anything`"),
            "{domain}: {error}"
        );
    }
}

#[test]
fn the_envelope_needs_a_plain_domain_and_name() {
    for (domain, name) in [
        ("", "x"),
        ("x", ""),
        ("a/b", "x"),
        ("x", "a/b"),
        ("x", "*"),
        ("*", "x"),
    ] {
        assert!(
            ExtensionEvent::new(domain, name, json!(null)).is_err(),
            "{domain}/{name}"
        );
        let wire = event(json!({"type":domain,"name":name,"data":null}));
        assert!(
            serde_json::from_value::<AgentEvent>(wire).is_err(),
            "{domain}/{name}"
        );
    }
    let missing_data = event(json!({"type":"goal","name":"set"}));
    assert!(serde_json::from_value::<AgentEvent>(missing_data).is_err());
    let extra = event(json!({"type":"goal","name":"set","data":{},"extra":1}));
    assert!(serde_json::from_value::<AgentEvent>(extra).is_err());
}

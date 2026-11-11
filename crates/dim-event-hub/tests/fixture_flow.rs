//! The EventHub flows that need to observe or control input writes: delivery
//! concurrency, a failed subscription-file write, a refused partial creation,
//! the publish's one `lastFailure` transaction, the subscription limits, a
//! source's row lifetime, a missed deletion notice, an EventHub reload, a
//! producer that applied first, the subscribe/unsubscribe race, and a host
//! without `sessionFiles` (design Appendix D items 3–5, 7–9, 12).
mod support;

use dim_agent_types::InputMode;
use dim_event_hub::{DeclareRequest, PublishRequest, ToTarget, TypeDeclaration};
use serde_json::{Value, json};
use std::{path::Path, sync::Arc, time::Duration};
use support::fixture::{Fixture, directory};
use support::wait_for;

fn im_declaration() -> DeclareRequest {
    DeclareRequest {
        source: "im".into(),
        guide: "The IM guide.".into(),
        types: vec![TypeDeclaration {
            r#type: "im.message".into(),
            description: "An instant message".into(),
            mode: InputMode::Queue,
        }],
    }
}

fn event(id: &str) -> PublishRequest {
    PublishRequest {
        source: "im".into(),
        r#type: "im.message".into(),
        id: Some(id.into()),
        title: Some("Event".into()),
        body: "body".into(),
        attributes: Default::default(),
        to: Vec::new(),
    }
}

fn event_to(id: &str, sessions: &[&str]) -> PublishRequest {
    let mut request = event(id);
    request.to = sessions
        .iter()
        .map(|session| ToTarget {
            session_id: (*session).to_owned(),
            branch_id: None,
            mode: None,
        })
        .collect();
    request
}

fn app_code(error: &dim_agent_capabilities::methods::RpcError) -> String {
    error
        .data
        .as_ref()
        .map(|data| data.app_error.clone())
        .unwrap_or_default()
}

fn ci_declaration() -> DeclareRequest {
    DeclareRequest {
        source: "ci".into(),
        guide: "The CI guide.".into(),
        types: vec![TypeDeclaration {
            r#type: "ci.build".into(),
            description: "A build result".into(),
            mode: InputMode::Append,
        }],
    }
}

async fn mounted() -> (tempfile::TempDir, Fixture) {
    let directory = directory();
    let fixture = Fixture::mount(directory.path()).await;
    fixture.producer.declare(im_declaration()).await.unwrap();
    (directory, fixture)
}

/// A publish to ten branches holds at most eight `sessions.input` calls in
/// flight (design §2.5, Appendix D item 3).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ten_targets_never_run_more_than_eight_inputs() {
    let (_directory, fixture) = mounted().await;
    let sessions: Vec<String> = (0..10).map(|index| format!("session-{index}")).collect();
    let gate = fixture.sessions.gate();
    let producer = fixture.producer.clone();
    let targets: Vec<&str> = sessions.iter().map(String::as_str).collect();
    let many = event_to("many", &targets);
    let task = tokio::spawn(async move { producer.publish(many).await });
    let mut max = gate.max_watch();
    tokio::time::timeout(Duration::from_secs(10), async {
        while *max.borrow() < 8 {
            max.changed().await.unwrap();
        }
    })
    .await
    .expect("eight deliveries run at once");
    gate.release();
    let receipt = task.await.unwrap().unwrap();
    let deliveries = receipt["deliveries"].as_array().unwrap();
    assert_eq!(deliveries.len(), 10, "{receipt}");
    assert!(
        deliveries
            .iter()
            .all(|entry| entry["status"] == json!("delivered"))
    );
    assert_eq!(gate.max(), 8, "never more than eight at once");
    assert_eq!(fixture.sessions.calls().len(), 10);
}

/// A subscribe whose subscription-file write fails retires the guide it wrote
/// and answers `storage` (design §3.3, Appendix D item 6).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_failed_subscription_file_write_retires_the_guide() {
    let (_directory, fixture) = mounted().await;
    let blocked = fixture.block_writes();
    let error = fixture
        .subscribe("session-1", &["im.*"], None)
        .await
        .unwrap_err();
    assert_eq!(app_code(&error), "storage", "{error:?}");
    drop(blocked);
    assert!(fixture.subscriptions(None).await.is_empty());

    let calls = fixture.sessions.calls();
    assert_eq!(calls.len(), 2, "{calls:?}");
    let guide = &calls[0];
    assert_eq!(guide.mode, "append");
    assert!(
        guide.submission_id.starts_with("eventhub-guide:sub_"),
        "{guide:?}"
    );
    assert!(guide.submission_id.ends_with(":im"), "{guide:?}");
    assert_eq!(guide.key_use(), Some("live"));
    assert_eq!(guide.key_name(), Some("eventhub:guide:im"));
    assert_eq!(
        guide.text,
        "<dim-event-guide source=\"im\">The IM guide.</dim-event-guide>"
    );
    let retire = &calls[1];
    assert_eq!(retire.mode, "append");
    assert!(
        retire
            .submission_id
            .starts_with("eventhub-guide-retire:sub_"),
        "{retire:?}"
    );
    assert!(retire.submission_id.ends_with(":im"), "{retire:?}");
    assert_eq!(retire.key_use(), Some("retire"));
    assert_eq!(
        retire.text,
        "<dim-event-guide source=\"im\"></dim-event-guide>"
    );
    assert!(!fixture.file().exists());
}

/// A publish whose `sessions.input` call fails at the transport level answers
/// `transport`, keeps the subscription and records it (design §2.3, Appendix
/// C): the call says nothing about whether the write happened.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_transport_failure_answers_transport_and_keeps_the_subscription() {
    let (_directory, fixture) = mounted().await;
    let subscription = fixture.subscribe("far", &["im.*"], None).await.unwrap();
    fixture.sessions.fail_transport("eventhub:im:unknown");
    let receipt = fixture.producer.publish(event("unknown")).await.unwrap();
    assert_eq!(receipt["deliveries"][0]["status"], json!("failed"));
    assert_eq!(receipt["deliveries"][0]["code"], json!("transport"));
    let stored = fixture.subscriptions(None).await;
    assert_eq!(stored.len(), 1, "{stored:?}");
    assert_eq!(stored[0]["id"], json!(subscription));
    assert_eq!(
        stored[0]["lastFailure"]["code"],
        json!("transport"),
        "{stored:?}"
    );
}

/// A subscribe whose guide write fails at the transport level answers
/// `transport`, stores nothing, and retires the guide in doubt after the ones
/// it wrote (design §3.3, Appendix C).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_subscribe_whose_guide_write_fails_in_transport_stores_nothing() {
    let (_directory, fixture) = mounted().await;
    fixture.sessions.fail_transport("eventhub-guide:");
    let error = fixture
        .subscribe("session-1", &["im.*"], None)
        .await
        .unwrap_err();
    assert_eq!(app_code(&error), "transport", "{error:?}");
    assert!(fixture.subscriptions(None).await.is_empty());
    assert!(!fixture.file().exists());

    let calls = fixture.sessions.calls();
    assert_eq!(calls.len(), 2, "{calls:?}");
    assert_eq!(calls[0].key_use(), Some("live"));
    let retire = calls.last().unwrap();
    assert_eq!(retire.key_use(), Some("retire"));
    assert!(
        retire
            .submission_id
            .starts_with("eventhub-guide-retire:sub_"),
        "{retire:?}"
    );
    assert_eq!(
        retire.text,
        "<dim-event-guide source=\"im\"></dim-event-guide>"
    );
}

/// One publish's `lastFailure` changes land in one transaction, after its
/// deliveries settle; a failed write leaves the whole set as it was, and the
/// next publish writes again (design §2.3, Appendix B).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_publish_last_failure_changes_are_one_transaction() {
    let (_directory, fixture) = mounted().await;
    for session in ["a", "b", "c"] {
        fixture.subscribe(session, &["im.*"], None).await.unwrap();
        fixture.sessions.fail(session, "session_closed");
    }
    let receipt = fixture.producer.publish(event("first")).await.unwrap();
    let deliveries = receipt["deliveries"].as_array().unwrap();
    assert_eq!(deliveries.len(), 3, "{receipt}");
    assert!(
        deliveries
            .iter()
            .all(|entry| entry["status"] == json!("failed"))
    );
    let stored = fixture.subscriptions(None).await;
    let stamps: Vec<&str> = stored
        .iter()
        .map(|subscription| subscription["lastFailure"]["at"].as_str().unwrap())
        .collect();
    assert_eq!(stamps.len(), 3);
    assert!(
        stamps.iter().all(|stamp| *stamp == stamps[0]),
        "one transaction stamps one instant: {stored:?}"
    );
    assert!(
        stored
            .iter()
            .all(|subscription| subscription["lastFailure"]["code"] == json!("session_closed"))
    );

    // The change batch fails as a whole: nothing is written, and the
    // in-memory set keeps the old value.
    fixture.sessions.fail("a", "reloading");
    fixture.sessions.fail("b", "reloading");
    fixture.sessions.accept("c");
    let blocked = fixture.block_writes();
    let before = std::fs::read(fixture.file()).unwrap();
    let receipt = fixture.producer.publish(event("second")).await.unwrap();
    assert_eq!(receipt["deliveries"][0]["status"], json!("failed"));
    assert_eq!(receipt["deliveries"][2]["status"], json!("delivered"));
    let stored = fixture.subscriptions(None).await;
    assert!(
        stored
            .iter()
            .all(|subscription| subscription["lastFailure"]["code"] == json!("session_closed")),
        "a failed write keeps the old set: {stored:?}"
    );
    drop(blocked);
    assert_eq!(std::fs::read(fixture.file()).unwrap(), before);

    // The next publish whose outcome changes the value writes it.
    let receipt = fixture.producer.publish(event("third")).await.unwrap();
    assert_eq!(receipt["deliveries"][2]["status"], json!("delivered"));
    let stored = fixture.subscriptions(None).await;
    let codes: Vec<Value> = stored
        .iter()
        .map(|subscription| subscription["lastFailure"]["code"].clone())
        .collect();
    assert_eq!(
        codes,
        vec![json!("reloading"), json!("reloading"), Value::Null],
        "{stored:?}"
    );
}

/// The per-Session and total limits answer `limit_reached` (design §2.2),
/// decided under the store's lock.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_subscription_limits_answer_limit_reached() {
    let per_session = directory();
    seed(per_session.path(), 32, "full");
    let full = Fixture::mount(per_session.path()).await;
    full.producer.declare(im_declaration()).await.unwrap();
    let error = full.subscribe("full", &["im.*"], None).await.unwrap_err();
    assert_eq!(app_code(&error), "limit_reached");
    // Another Session still has room.
    full.subscribe("room", &["im.*"], None).await.unwrap();
    assert_eq!(full.subscriptions(Some("full")).await.len(), 32);

    let total = directory();
    seed(total.path(), 1024, "many");
    let host = Fixture::mount(total.path()).await;
    host.producer.declare(im_declaration()).await.unwrap();
    let error = host.subscribe("new", &["im.*"], None).await.unwrap_err();
    assert_eq!(app_code(&error), "limit_reached");
}

/// `count` subscriptions for `session`, split over as few Sessions as the
/// 32-per-Session limit allows, written before the host starts.
fn seed(directory: &Path, count: usize, session: &str) {
    let subscriptions: Vec<Value> = (0..count)
        .map(|index| {
            let target = if index < 32 {
                session.to_owned()
            } else {
                format!("{session}-{}", index / 32)
            };
            json!({
                "id": format!("sub_{index}"),
                "types": ["im.*"],
                "target": {"sessionId": target, "branchId": "main"},
                "origin": "client",
                "createdAt": "2026-11-08T09:15:04.120Z",
            })
        })
        .collect();
    std::fs::create_dir_all(directory.join("eventhub")).unwrap();
    let text = json!({"version": 1, "subscriptions": subscriptions}).to_string();
    std::fs::write(
        directory.join("eventhub/subscriptions.json"),
        serde_json::to_vec(&json!({"version": 1, "text": text})).unwrap(),
    )
    .unwrap();
}

/// Another row's source is `duplicate_source`, and a source whose row
/// unloads loses its types while stored subscriptions remain (design §2.1,
/// Appendix D item 8).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn another_rows_source_is_duplicate_and_an_unloaded_source_loses_its_types() {
    let (_directory, fixture) = mounted().await;
    let (second, view) = fixture.producer_row("EventHubSecondProducer").await;
    let ci = DeclareRequest {
        source: "ci".into(),
        guide: String::new(),
        types: vec![TypeDeclaration {
            r#type: "ci.build".into(),
            description: String::new(),
            mode: InputMode::Append,
        }],
    };
    second.declare(ci.clone()).await.unwrap();
    // The same row declaring again replaces its own declaration.
    second.declare(ci.clone()).await.unwrap();
    // Another row's source is refused.
    let error = fixture.producer.declare(ci).await.unwrap_err();
    assert_eq!(error.code, "duplicate_source");

    let subscription = fixture
        .subscribe("subscriber", &["ci.*"], None)
        .await
        .unwrap();
    assert_eq!(fixture.subscriptions(None).await.len(), 1);
    view.shutdown().await.unwrap();
    let sources = fixture.call("eventhub.sources", json!({})).await.unwrap();
    let sources = sources["sources"].as_array().unwrap();
    assert!(
        sources.iter().all(|source| source["source"] != json!("ci")),
        "{sources:?}"
    );
    assert!(
        sources.iter().any(|source| source["source"] == json!("im")),
        "{sources:?}"
    );
    // The stored subscription outlives its producer.
    let stored = fixture.subscriptions(None).await;
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0]["id"], json!(subscription));
}

/// Every guide a subscribe names is read before the first guide is written:
/// a source whose producer unloads while the first write is held does not
/// refuse the subscribe, because its guide was already in hand
/// (design §3.3).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_subscribe_reads_every_guide_before_it_writes_one() {
    let (_directory, fixture) = mounted().await;
    let (second, view) = fixture.producer_row("EventHubLateProducer").await;
    let mut other = im_declaration();
    other.source = "other".into();
    other.guide = "The other guide.".into();
    other.types = vec![TypeDeclaration {
        r#type: "other.note".into(),
        description: "A note".into(),
        mode: InputMode::Queue,
    }];
    second.declare(other).await.unwrap();
    let fixture = Arc::new(fixture);
    fixture.sessions.clear();
    let gate = fixture.sessions.gate();
    let subscribing = {
        let fixture = fixture.clone();
        tokio::spawn(async move {
            fixture
                .subscribe("branch", &["im.*", "other.*"], None)
                .await
        })
    };
    wait_for("the first guide write held", || gate.max() >= 1).await;
    // The second source's producer unloads while the first write is held.
    view.shutdown().await.unwrap();
    gate.release();
    let subscription = subscribing.await.unwrap().unwrap();
    let calls = fixture.sessions.calls();
    assert_eq!(calls.len(), 2, "both guides were written: {calls:?}");
    assert!(
        calls.iter().all(|call| call.key_use() == Some("live")),
        "no guide is retired: {calls:?}"
    );
    let written: Vec<&str> = calls.iter().filter_map(|call| call.key_name()).collect();
    assert_eq!(
        written,
        vec!["eventhub:guide:im", "eventhub:guide:other"],
        "{calls:?}"
    );
    assert_eq!(
        fixture.subscriptions(None).await.len(),
        1,
        "the subscription is stored"
    );
    assert_eq!(
        fixture.subscriptions(None).await[0]["id"],
        json!(subscription)
    );
}

/// A missed deletion notice leaves a subscription that fails
/// `session_closed` and shows its failure (design §3.4, Appendix D item 9).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_missed_notice_leaves_a_session_closed_failure() {
    let (_directory, fixture) = mounted().await;
    let subscription = fixture.subscribe("dead", &["im.*"], None).await.unwrap();
    fixture.sessions.fail("dead", "session_closed");
    let receipt = fixture.producer.publish(event("lost")).await.unwrap();
    assert_eq!(receipt["deliveries"][0]["status"], json!("failed"));
    assert_eq!(receipt["deliveries"][0]["code"], json!("session_closed"));
    let stored = fixture.subscriptions(None).await;
    assert_eq!(stored.len(), 1, "{stored:?}");
    assert_eq!(stored[0]["id"], json!(subscription));
    assert_eq!(
        stored[0]["lastFailure"]["code"],
        json!("session_closed"),
        "{stored:?}"
    );
}

/// A producer that applied before EventHub declares on `eventhub/ready`, and
/// its events arrive (design §2.1, Appendix D item 12).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_producer_that_applied_first_declares_on_the_notice() {
    let directory = directory();
    let fixture = Fixture::mount_producer_first(directory.path()).await;
    // Its own apply found no service; the notice gave it one and it declared
    // then.
    wait_for("the ready notice", || fixture.producer.hub().is_some()).await;
    let sources = fixture.call("eventhub.sources", json!({})).await.unwrap();
    let listed = sources["sources"].as_array().unwrap();
    let im = listed
        .iter()
        .find(|source| source["source"] == json!("im"))
        .unwrap_or_else(|| panic!("the notice's declaration is listed: {sources}"));
    assert_eq!(im["guide"], json!("The IM guide."), "{sources}");
    fixture.subscribe("noticed", &["im.*"], None).await.unwrap();
    let receipt = fixture.producer.publish(event("noticed")).await.unwrap();
    assert_eq!(
        receipt["deliveries"][0]["status"],
        json!("delivered"),
        "{receipt}"
    );
}

/// The subscribe and the unsubscribe of one branch serialize under the
/// creation lock: the unsubscribe never retires the guide of a subscription
/// being created (design §3.3, Appendix B). The assertion is the order of the
/// recorded inputs, not the timing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_concurrent_unsubscribe_does_not_retire_a_guide_being_written() {
    let (_directory, fixture) = mounted().await;
    let fixture = Arc::new(fixture);
    // The only other subscription naming `im` on the branch.
    let other = fixture.subscribe("branch", &["im.*"], None).await.unwrap();
    fixture.sessions.clear();
    // The new subscription's guide write is held in flight; a retire passes
    // the gate, so an unguarded removal would write it.
    let gate = fixture.sessions.gate_live();
    let subscribing = {
        let fixture = fixture.clone();
        tokio::spawn(async move { fixture.subscribe("branch", &["im.*"], None).await })
    };
    wait_for("the guide write in flight", || gate.max() >= 1).await;
    let mut unsubscribing = {
        let fixture = fixture.clone();
        tokio::spawn(async move { fixture.unsubscribe(&other).await })
    };
    // Without the lock the removal runs through and retires the key; with it
    // the removal waits for the creation to store.
    let finished = tokio::select! {
        result = &mut unsubscribing => Some(result),
        _ = tokio::time::sleep(Duration::from_millis(500)) => None,
    };
    gate.release();
    let created = subscribing.await.unwrap().unwrap();
    match finished {
        Some(result) => result.unwrap().unwrap(),
        None => unsubscribing.await.unwrap().unwrap(),
    }

    let calls = fixture.sessions.calls();
    let guide: Vec<_> = calls
        .iter()
        .filter(|call| call.key_name() == Some("eventhub:guide:im"))
        .collect();
    let live = guide
        .iter()
        .position(|call| call.key_use() == Some("live"))
        .expect("the new subscription's guide was written");
    assert!(
        guide[live + 1..]
            .iter()
            .all(|call| call.key_use() != Some("retire")),
        "no retire of the key follows the live guide: {calls:?}"
    );
    let stored = fixture.subscriptions(Some("branch")).await;
    assert_eq!(stored.len(), 1, "{stored:?}");
    assert_eq!(stored[0]["id"], json!(created));
}

/// A creation refused after it wrote one of its guides retires that guide by
/// the `storage` rule, stores nothing, and answers the refusal (design §3.3).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_refused_partial_creation_retires_the_guides_it_wrote() {
    let (_directory, fixture) = mounted().await;
    fixture.producer.declare(ci_declaration()).await.unwrap();
    fixture.sessions.clear();
    // Sources are written in name order: `ci` first, then `im`.
    fixture.sessions.refuse(":im", "rejected");
    let error = fixture
        .subscribe("branch", &["ci.*", "im.*"], None)
        .await
        .unwrap_err();
    assert_eq!(app_code(&error), "rejected", "{error:?}");
    assert!(fixture.subscriptions(None).await.is_empty());

    let calls = fixture.sessions.calls();
    assert_eq!(calls.len(), 3, "{calls:?}");
    assert_eq!(calls[0].key_use(), Some("live"));
    assert!(calls[0].submission_id.ends_with(":ci"), "{:?}", calls[0]);
    assert_eq!(calls[1].key_use(), Some("live"));
    assert!(calls[1].submission_id.ends_with(":im"), "{:?}", calls[1]);
    assert_eq!(calls[2].key_use(), Some("retire"));
    assert!(calls[2].submission_id.ends_with(":ci"), "{:?}", calls[2]);
    assert_eq!(
        calls[2].text,
        "<dim-event-guide source=\"ci\"></dim-event-guide>"
    );
}

/// Without `sessionFiles`, an overflowed body is `truncated="true"` with the
/// preview only, and the delivery reports it (design §3.2, Appendix D
/// item 5).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_host_without_session_files_truncates() {
    let (_directory, fixture) = mounted().await;
    let mut request = event_to("huge", &["cold"]);
    request.body = format!("{}\n{}", "x".repeat(6000), "tail");
    let receipt = fixture.producer.publish(request).await.unwrap();
    let delivery = &receipt["deliveries"][0];
    assert_eq!(delivery["status"], json!("delivered"), "{receipt}");
    assert_eq!(delivery["truncated"], json!(true), "{receipt}");
    let calls = fixture.sessions.calls();
    let text = &calls[0].text;
    assert!(text.contains(" truncated=\"true\""), "{text}");
    assert!(!text.contains(" file=\""), "{text}");
    assert!(!text.contains("tail"), "the preview drops the rest: {text}");
    assert_eq!(calls[0].mode, "queue");
}

/// The `eventSubscriptions` face (service faces #1292): the Rust binding calls
/// the service's own operations and a part of another language calls the same
/// operations by name, so both answer the same.
#[tokio::test]
async fn the_subscription_service_answers_the_same_through_the_binding_and_the_face() {
    use dim_event_hub::{EventSubscriptions, ListCall, SubscriptionCall, UnsubscribeCall};
    use rutis_bridge::session::call_service;

    let directory = directory();
    let fixture = Fixture::mount(directory.path()).await;
    fixture.producer.declare(im_declaration()).await.unwrap();
    let binding = fixture.root.get::<EventSubscriptions>().unwrap();

    let sources: Value = call_service(&fixture.root, "eventSubscriptions", "sources", ())
        .await
        .unwrap();
    assert_eq!(binding.sources(), sources);

    let call = SubscriptionCall {
        session_id: "session".into(),
        types: vec!["im.*".into()],
        matches: None,
        mode: Some(InputMode::Queue),
        branch_id: Some("main".into()),
    };
    let subscribed = binding.subscribe(call).await.unwrap();
    let id = subscribed["subscriptionId"].as_str().unwrap().to_owned();

    let list = ListCall {
        session_id: "session".into(),
    };
    let through_face: Value =
        call_service(&fixture.root, "eventSubscriptions", "list", (list.clone(),))
            .await
            .unwrap();
    assert_eq!(binding.list(list), through_face);
    assert!(
        through_face["subscriptions"]
            .as_array()
            .is_some_and(|subscriptions| subscriptions.iter().any(|s| s["id"] == json!(id))),
        "the subscription is listed: {through_face}"
    );

    binding
        .unsubscribe(UnsubscribeCall {
            session_id: "session".into(),
            subscription_id: id.clone(),
        })
        .await
        .unwrap();
    assert_eq!(
        binding.list(ListCall {
            session_id: "session".into()
        })["subscriptions"]
            .as_array()
            .map(Vec::len),
        Some(0)
    );

    let typed = binding
        .unsubscribe(UnsubscribeCall {
            session_id: "other".into(),
            subscription_id: id.clone(),
        })
        .await
        .unwrap_err();
    let through_face = call_service::<Value>(
        &fixture.root,
        "eventSubscriptions",
        "unsubscribe",
        (UnsubscribeCall {
            session_id: "other".into(),
            subscription_id: id,
        },),
    )
    .await
    .expect_err("another Session's subscription is unknown");
    assert_eq!(through_face.code(), Some("subscription_unknown"));
    assert!(through_face.to_string().contains(&typed.message));
}

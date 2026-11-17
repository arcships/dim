use super::*;
use dim_agent_runtime::InputSnapshot;
use serde_json::json;

fn builder(store: &Arc<SqliteTimelineStore>) -> AgentBuilder {
    let mut builder = AgentBuilder::new(AgentConfig::default());
    dim_agent_capabilities::defaults::mock_hosts(&mut builder);
    dim_agent_capabilities::defaults::install(&mut builder);
    builder.timeline_store(store.clone());
    builder
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn queue_resume_finishes_multiple_source_pages_without_republishing_an_older_count() {
    let database = Database::new();
    let db = SqliteStorage::open(database.0.clone()).unwrap();
    let store = SqliteTimelineStore::new(db.clone());
    let agent = MinimalHost::start(&builder(&store)).await.unwrap();
    let session = agent
        .create_session(SessionConfig::default())
        .await
        .unwrap();
    session
        .submit("initial result".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    settled("the initial result is complete", session.idle("main"))
        .await
        .unwrap();
    let id = session.id().to_owned();
    agent.close().await.unwrap();
    let head = store.head(&key(&id, "main")).await.unwrap();
    let records = (1..=300)
        .map(|offset| {
            record(
                head + offset,
                EventKind::InputMessage(InputMessageData::text(
                    "not processed before close",
                    Some(InputMode::Append),
                )),
            )
        })
        .collect();
    append(&store, key(&id, "main"), records).await.unwrap();
    for _ in 0..2 {
        let agent = MinimalHost::start(&builder(&store)).await.unwrap();
        let session = settled(
            "all new source pages restore",
            agent.restore_session(id.clone()),
        )
        .await
        .unwrap();
        let mut inventory = session.on::<InputSnapshot>("main").unwrap().unwrap();
        assert_eq!(inventory.borrow().queued, 300);
        let idle = settled("recovery results are dispatched", session.idle("main"));
        tokio::pin!(idle);
        loop {
            tokio::select! {
                changed = inventory.changed() => {
                    changed.unwrap();
                    assert_eq!(inventory.borrow_and_update().queued, 300, "an earlier recovery result must not regress the restored inventory");
                }
                result = &mut idle => {
                    result.unwrap();
                    break;
                }
            }
        }
        let history = chain_events(&session, "main", ..).await.unwrap();
        assert_eq!(history.iter().filter(|event| matches!(&event.kind, EventKind::InputApplied(data) if data.input_id.starts_with("event-"))).count(), 300, "source pages apply once across repeated resumes");
        agent.close().await.unwrap();
    }
    store.close().await.unwrap();
    db.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn current_inputs_restore_and_are_removed_when_a_turn_consumes_them() {
    let database = Database::new();
    let db = SqliteStorage::open(database.0.clone()).unwrap();
    let store = SqliteTimelineStore::new(db.clone());
    let agent = MinimalHost::start(&builder(&store)).await.unwrap();
    let session = agent
        .create_session(SessionConfig::default())
        .await
        .unwrap();
    let id = session.id().to_owned();
    let mut inputs = Vec::new();
    for _ in 0..257 {
        inputs.push(
            session
                .input(
                    InputMessageData::text("kept across resume", Some(InputMode::Append)),
                    "main",
                )
                .await
                .unwrap()
                .id
                .clone(),
        );
    }
    settled("all append inputs are published", session.idle("main"))
        .await
        .unwrap();
    assert_eq!(
        session
            .get::<InputSnapshot>("main")
            .unwrap()
            .unwrap()
            .queued,
        257
    );
    agent.close().await.unwrap();
    store.close().await.unwrap();
    db.close().await.unwrap();

    let db = SqliteStorage::open(database.0.clone()).unwrap();
    let store = SqliteTimelineStore::new(db.clone());
    let agent = MinimalHost::start(&builder(&store)).await.unwrap();
    let session = settled(
        "the current inputs restore",
        agent.restore_session(id.clone()),
    )
    .await
    .unwrap();
    assert_eq!(
        session
            .get::<InputSnapshot>("main")
            .unwrap()
            .unwrap()
            .queued,
        257
    );
    session
        .submit("consume restored inputs".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    settled("the restored entries are consumed", session.idle("main"))
        .await
        .unwrap();
    assert_eq!(
        session
            .get::<InputSnapshot>("main")
            .unwrap()
            .unwrap()
            .queued,
        0
    );
    let history = chain_events(&session, "main", ..).await.unwrap();
    let consumed: Vec<_> = history
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::InputConsumed(data) if inputs.contains(&data.input_id) => {
                Some(data.input_id.clone())
            }
            _ => None,
        })
        .collect();
    assert_eq!(consumed, inputs);
    agent.close().await.unwrap();
    store.close().await.unwrap();
    db.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn queue_resume_reuses_an_application_written_by_an_older_owner_after_a_saved_result() {
    let database = Database::new();
    let db = SqliteStorage::open(database.0.clone()).unwrap();
    let store = SqliteTimelineStore::new(db.clone());
    let agent = MinimalHost::start(&builder(&store)).await.unwrap();
    let session = agent
        .create_session(SessionConfig::default())
        .await
        .unwrap();
    session
        .submit("save a compatible result".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    settled("the first result is saved", session.idle("main"))
        .await
        .unwrap();
    let id = session.id().to_owned();
    agent.close().await.unwrap();
    let head = store.head(&key(&id, "main")).await.unwrap();
    let source = record(
        head + 1,
        EventKind::InputMessage(InputMessageData::text(
            "older owner output",
            Some(InputMode::Queue),
        )),
    );
    let input_id = source.event.id.clone();
    append(
        &store,
        key(&id, "main"),
        vec![
            source,
            record(
                head + 2,
                EventKind::InputApplied(InputAppliedData {
                    input_id: input_id.clone(),
                    mode: Some(InputMode::Queue),
                }),
            ),
        ],
    )
    .await
    .unwrap();
    let agent = MinimalHost::start(&builder(&store)).await.unwrap();
    let session = settled(
        "saved state and older output restore",
        agent.restore_session(id),
    )
    .await
    .unwrap();
    settled("the older application completes", session.idle("main"))
        .await
        .unwrap();
    let history = chain_events(&session, "main", ..).await.unwrap();
    assert_eq!(history.iter().filter(|event| matches!(&event.kind, EventKind::InputApplied(data) if data.input_id == input_id)).count(), 1);
    assert_eq!(history.iter().filter(|event| matches!(&event.kind, EventKind::InputConsumed(data) if data.input_id == input_id)).count(), 1);
    agent.close().await.unwrap();
    store.close().await.unwrap();
    db.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn obsolete_queue_nodes_do_not_override_current_business_facts() {
    for broken in ["missing", "cyclic", "summary"] {
        let database = Database::new();
        let db = SqliteStorage::open(database.0.clone()).unwrap();
        let store = SqliteTimelineStore::new(db.clone());
        let agent = MinimalHost::start(&builder(&store)).await.unwrap();
        let session = agent
            .create_session(SessionConfig::default())
            .await
            .unwrap();
        let input = session
            .submit("save state".into(), InputMode::Queue, "main")
            .await
            .unwrap();
        settled("the original queue completes", session.idle("main"))
            .await
            .unwrap();
        let id = session.id().to_owned();
        agent.close().await.unwrap();
        let head = store.head(&key(&id, "main")).await.unwrap();
        let node_id = "broken-queue-node";
        let mut records = Vec::new();
        if broken != "missing" {
            let mut node = record(head + 1, EventKind::Extension(ExtensionEvent::new("input-queue", "node", json!({
                "entry":{"input_id": input.id, "sequence": input.sequence,"kind":{"kind":"input","mode":"append","released":true}},
                "left":if broken == "cyclic" { Some(node_id) } else { None }, "right":null,
                "height":if broken == "summary" { 4 } else { 1 },"queued":1,"handovers":0,"first_held":null
            })).unwrap()));
            Arc::make_mut(&mut node.event).id = node_id.into();
            records.push(node);
        }
        let end = head + records.len() as u64 + 1;
        records.push(record(
            end,
            EventKind::Extension(
                ExtensionEvent::new(
                    "input-queue",
                    "recovered",
                    json!({
                        "schema":"input-queue/v1","source_through":head,
                        "state":{"entries_root":node_id,"queued":1,"turn":null,"starting":false}
                    }),
                )
                .unwrap(),
            ),
        ));
        append(&store, key(&id, "main"), records).await.unwrap();
        let agent = MinimalHost::start(&builder(&store)).await.unwrap();
        let restored = settled(
            "business facts restore independently of obsolete nodes",
            agent.restore_session(id.clone()),
        )
        .await
        .unwrap();
        settled("the restored queue is idle", restored.idle("main"))
            .await
            .unwrap();
        assert_eq!(
            restored
                .get::<InputSnapshot>("main")
                .unwrap()
                .unwrap()
                .queued,
            0
        );
        let kept = store.read(&key(&id, "main"), end..=end).await.unwrap();
        assert_eq!(kept[0].event.kind.key(), "input-queue/recovered");
        agent.close().await.unwrap();
        store.close().await.unwrap();
        db.close().await.unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn queue_decisions_commit_and_resume_once_after_a_failed_commit() {
    let database = Database::new();
    let db = SqliteStorage::open(database.0.clone()).unwrap();
    let store = SqliteTimelineStore::new(db.clone());
    let mut builder = AgentBuilder::new(AgentConfig::default());
    dim_agent_capabilities::defaults::mock_hosts(&mut builder);
    dim_agent_capabilities::defaults::install(&mut builder);
    builder.timeline_store(store.clone());
    let agent = MinimalHost::start(&builder).await.unwrap();
    let session = agent
        .create_session(SessionConfig::default())
        .await
        .unwrap();
    let id = session.id().to_owned();
    let connection = Connection::open(&database.0).unwrap();
    connection.execute_batch("CREATE TRIGGER fail_queue_decision BEFORE INSERT ON timeline_events WHEN NEW.kind = 'input/applied' BEGIN SELECT RAISE(ABORT, 'injected queue decision failure'); END").unwrap();
    let input = session
        .submit("recover this input".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    let failure = settled("queue commit failure closes its Session", session.closed())
        .await
        .unwrap_err();
    assert!(
        failure
            .to_string()
            .contains("injected queue decision failure"),
        "{failure}"
    );
    let history = chain_events(&session, "main", ..).await.unwrap();
    assert!(history.iter().any(|event| event.id == input.id));
    assert!(!history.iter().any(|event| matches!(&event.kind,
        EventKind::InputApplied(data) if data.input_id == input.id)));
    connection
        .execute_batch("DROP TRIGGER fail_queue_decision")
        .unwrap();
    drop(connection);
    let restored = settled(
        "the queue restores its unprocessed source",
        agent.restore_session(id.clone()),
    )
    .await
    .unwrap();
    settled("the restored input completes", restored.idle("main"))
        .await
        .unwrap();
    let head = store.head(&key(&id, "main")).await.unwrap();
    let records = store.read(&key(&id, "main"), 1..=head).await.unwrap();
    let applied: Vec<_> = records
        .iter()
        .filter(|record| {
            matches!(&record.event.kind,
        EventKind::InputApplied(data) if data.input_id == input.id)
        })
        .collect();
    assert_eq!(applied.len(), 1);
    assert_eq!(applied[0].batch_end, applied[0].event.sequence);
    assert!(
        !records
            .iter()
            .any(|record| record.event.kind.key().starts_with("input-queue/")),
        "queue decisions do not create recovery events"
    );
    assert!(
        history
            .iter()
            .all(|event| event.sequence < applied[0].event.sequence)
    );
    agent.close().await.unwrap();
    store.close().await.unwrap();
    db.close().await.unwrap();

    let db = SqliteStorage::open(database.0.clone()).unwrap();
    let store = SqliteTimelineStore::new(db.clone());
    let mut builder = AgentBuilder::new(AgentConfig::default());
    dim_agent_capabilities::defaults::mock_hosts(&mut builder);
    dim_agent_capabilities::defaults::install(&mut builder);
    builder.timeline_store(store.clone());
    let agent = MinimalHost::start(&builder).await.unwrap();
    let restored = settled(
        "the queue restores its saved state",
        agent.restore_session(id.clone()),
    )
    .await
    .unwrap();
    settled("saved queue restore is idle", restored.idle("main"))
        .await
        .unwrap();
    let history = chain_events(&restored, "main", ..).await.unwrap();
    assert_eq!(
        history
            .iter()
            .filter(|event| matches!(&event.kind,
        EventKind::InputApplied(data) if data.input_id == input.id))
            .count(),
        1
    );
    assert_eq!(
        history
            .iter()
            .filter(|event| matches!(&event.kind,
        EventKind::InputConsumed(data) if data.input_id == input.id))
            .count(),
        1
    );
    agent.close().await.unwrap();
    store.close().await.unwrap();
    db.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn obsolete_queue_result_versions_do_not_control_recovery() {
    let database = Database::new();
    let db = SqliteStorage::open(database.0.clone()).unwrap();
    let store = SqliteTimelineStore::new(db.clone());
    let mut builder = AgentBuilder::new(AgentConfig::default());
    dim_agent_capabilities::defaults::mock_hosts(&mut builder);
    dim_agent_capabilities::defaults::install(&mut builder);
    builder.timeline_store(store.clone());
    let agent = MinimalHost::start(&builder).await.unwrap();
    let session = agent
        .create_session(SessionConfig::default())
        .await
        .unwrap();
    session
        .submit(
            "an older compatible result".into(),
            InputMode::Queue,
            "main",
        )
        .await
        .unwrap();
    settled("the compatible queue state is saved", session.idle("main"))
        .await
        .unwrap();
    let id = session.id().to_owned();
    agent.close().await.unwrap();
    let head = store.head(&key(&id, "main")).await.unwrap();
    append(
        &store,
        key(&id, "main"),
        vec![record(head + 1, EventKind::Extension(
        ExtensionEvent::new("input-queue", "recovered", json!({
            "schema":"input-queue/v999", "source_through":head, "state":{"future":"shape"}
        })).unwrap(),
    ))],
    )
    .await
    .unwrap();
    let agent = MinimalHost::start(&builder).await.unwrap();
    let restored = settled(
        "obsolete result schemas are not state inputs",
        agent.restore_session(id.clone()),
    )
    .await
    .unwrap();
    settled("restored queue finishes", restored.idle("main"))
        .await
        .unwrap();
    assert_eq!(
        restored
            .get::<InputSnapshot>("main")
            .unwrap()
            .unwrap()
            .queued,
        0
    );
    let records = store
        .read(&key(&id, "main"), head + 1..=head + 1)
        .await
        .unwrap();
    assert_eq!(records[0].event.sequence, head + 1);
    agent.close().await.unwrap();
    store.close().await.unwrap();
    db.close().await.unwrap();
}

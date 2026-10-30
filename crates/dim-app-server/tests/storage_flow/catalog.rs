use super::*;

#[test]
fn catalog_backfill_failure_preserves_the_legacy_store() {
    let database = Database::new();
    let connection = v16_tables_store(&database.0, 21);
    insert_documented_records(
        &connection,
        &BUSINESS_TABLES
            .iter()
            .copied()
            .filter(|table| *table != "session_catalog")
            .collect::<Vec<_>>(),
    );
    connection.execute("UPDATE timeline_events SET body='damaged' WHERE session_id='s1' AND branch_id='main' AND sequence=8", []).unwrap();
    drop(connection);
    let before = std::fs::read(&database.0).unwrap();
    assert!(SqliteStorage::open(database.0.clone()).is_err());
    assert_eq!(std::fs::read(&database.0).unwrap(), before);
    let connection = Connection::open(&database.0).unwrap();
    assert_eq!(marker(&connection), [21]);
    assert_eq!(schema(&connection), schema_of(21));
}

fn input(sequence: u64, content: Vec<ContentBlock>) -> StoredEvent {
    record(
        sequence,
        EventKind::InputMessage(InputMessageData {
            user: Some(UserInput { content }),
            ..Default::default()
        }),
    )
}

fn batch(mut events: Vec<StoredEvent>) -> Vec<StoredEvent> {
    let end = events.last().unwrap().event.sequence;
    for event in &mut events {
        event.batch_end = end;
    }
    events
}

#[tokio::test]
async fn catalog_projection_preserves_the_preview_window_and_commits_atomically() {
    let database = Database::new();
    let db = SqliteStorage::open(database.0.clone()).unwrap();
    let store = SqliteTimelineStore::new(db.clone());
    let connection = Connection::open(&database.0).unwrap();
    for id in ["boundary", "late", "image", "empty", "failed"] {
        store
            .create_session(id, &SessionConfig::default())
            .await
            .unwrap();
    }
    assert_eq!(
        connection
            .query_row(
                "SELECT count(*) FROM session_catalog WHERE preview IS NULL",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        5
    );
    append(
        &store,
        key("boundary", "main"),
        batch((1..=255).map(|s| text(s, "x")).collect()),
    )
    .await
    .unwrap();
    append(
        &store,
        key("boundary", "main"),
        batch(vec![
            text(256, "x"),
            input(
                257,
                vec![ContentBlock::Text {
                    text: "ä".repeat(130),
                }],
            ),
        ]),
    )
    .await
    .unwrap();
    append(
        &store,
        key("late", "main"),
        batch((1..=256).map(|s| text(s, "x")).collect()),
    )
    .await
    .unwrap();
    append(
        &store,
        key("late", "main"),
        vec![input(
            257,
            vec![ContentBlock::Text {
                text: "outside".into(),
            }],
        )],
    )
    .await
    .unwrap();
    append(&store, key("image", "main"), vec![input(1, vec![])])
        .await
        .unwrap();
    append(
        &store,
        key("image", "main"),
        vec![input(
            2,
            vec![ContentBlock::Text {
                text: "later text".into(),
            }],
        )],
    )
    .await
    .unwrap();
    assert!(
        append(
            &store,
            key("failed", "main"),
            batch(vec![
                input(
                    1,
                    vec![ContentBlock::Text {
                        text: "must roll back".into()
                    }]
                ),
                text(3, "gap")
            ])
        )
        .await
        .is_err()
    );
    let projection = |id: &str| {
        connection
            .query_row(
                "SELECT updated_at_ms, preview, cwd FROM session_catalog WHERE session_id=?1",
                [id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .unwrap()
    };
    assert_eq!(projection("boundary").0, 1257);
    assert_eq!(
        projection("boundary").1,
        Some(format!("{}…", "ä".repeat(119)))
    );
    assert_eq!(projection("late").1, None);
    assert_eq!(projection("image").1, None);
    assert_eq!(projection("failed").1, None);
    assert_eq!(
        connection
            .query_row(
                "SELECT count(*) FROM timeline_events WHERE session_id='failed'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    let before: Vec<_> = ["boundary", "late", "image", "empty", "failed"]
        .map(projection)
        .into();
    store.close().await.unwrap();
    db.close().await.unwrap();
    connection.execute_batch("CREATE INDEX timeline_events_by_end ON timeline_events(session_id, branch_id, last_sequence, sequence); DROP TABLE session_catalog; DROP TABLE agent_store_format; CREATE TABLE agent_store_format (version INTEGER NOT NULL PRIMARY KEY CHECK (version = 21)); INSERT INTO agent_store_format VALUES (21)").unwrap();
    drop(connection);
    let db = SqliteStorage::open(database.0.clone()).unwrap();
    assert_eq!(db.upgraded_from(), Some(21));
    let connection = Connection::open(&database.0).unwrap();
    let after: Vec<_> = ["boundary", "late", "image", "empty", "failed"]
        .map(|id| {
            connection
                .query_row(
                    "SELECT updated_at_ms, preview, cwd FROM session_catalog WHERE session_id=?1",
                    [id],
                    |row| {
                        Ok((
                            row.get::<_, i64>(0)?,
                            row.get::<_, Option<String>>(1)?,
                            row.get::<_, String>(2)?,
                        ))
                    },
                )
                .unwrap()
        })
        .into();
    assert_eq!(
        after, before,
        "backfill and live projection must be column-identical"
    );
    let plan: Vec<String> = connection.prepare("EXPLAIN QUERY PLAN SELECT session_id FROM session_catalog WHERE preview IS NOT NULL ORDER BY updated_at_ms DESC, session_id LIMIT 50").unwrap().query_map([], |row| row.get(3)).unwrap().collect::<rusqlite::Result<_>>().unwrap();
    assert!(
        plan.iter()
            .any(|line| line.contains("session_catalog_with_preview")),
        "{plan:?}"
    );
    let store = SqliteTimelineStore::new(db.clone());
    store.rollback_session("boundary").await.unwrap();
    assert_eq!(
        connection
            .query_row(
                "SELECT count(*) FROM session_catalog WHERE session_id='boundary'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    store.close().await.unwrap();
    db.close().await.unwrap();
}

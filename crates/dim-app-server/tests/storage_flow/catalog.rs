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
            .filter(|table| {
                *table != "session_catalog"
                    && !V28_TABLES.contains(table)
                    && !V29_TABLES.contains(table)
                    && !V33_TABLES.contains(table)
            })
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

/// The catalog preview a Session's first user input decides (#1378): the
/// operator's text alone, never the context a capability appended after it.
#[tokio::test]
async fn catalog_preview_skips_appended_context_after_the_operators_text() {
    let database = Database::new();
    let db = SqliteStorage::open(database.0.clone()).unwrap();
    let store = SqliteTimelineStore::new(db.clone());
    let connection = Connection::open(&database.0).unwrap();
    let hook = "<dim-hooks kind=\"prompt-submit\">read src/parser.rs first</dim-hooks>";
    let typed = "<dim-hooks kind=\"prompt-submit\">the operator pasted a marker</dim-hooks>";
    for (id, content) in [
        (
            "hook",
            vec![
                ContentBlock::Text {
                    text: "Why does the parser stall?".into(),
                },
                ContentBlock::Text { text: hook.into() },
            ],
        ),
        // The first block is the operator's, whatever it opens with.
        ("typed", vec![ContentBlock::Text { text: typed.into() }]),
        (
            "plain",
            vec![ContentBlock::Text {
                text: "Only the operator's words".into(),
            }],
        ),
    ] {
        store
            .create_session(id, &SessionConfig::default())
            .await
            .unwrap();
        append(&store, key(id, "main"), vec![input(1, content)])
            .await
            .unwrap();
    }
    let preview = |id: &str| {
        connection
            .query_row(
                "SELECT preview FROM session_catalog WHERE session_id=?1",
                [id],
                |row| row.get::<_, Option<String>>(0),
            )
            .unwrap()
    };
    assert_eq!(
        preview("hook").as_deref(),
        Some("Why does the parser stall?")
    );
    assert_eq!(preview("typed").as_deref(), Some(typed));
    assert_eq!(
        preview("plain").as_deref(),
        Some("Only the operator's words")
    );
    store.close().await.unwrap();
    db.close().await.unwrap();
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
    let v21 = database.0.with_extension("v21.sqlite");
    let old = Connection::open(&v21).unwrap();
    old.execute_batch("PRAGMA foreign_keys=OFF").unwrap();
    for sql in super::V13_SCHEMA
        .iter()
        .chain(super::V15_ADDITIONS)
        .chain(super::V16_ADDITIONS)
    {
        old.execute_batch(&sql.replace("(version = 13)", "(version = 21)"))
            .unwrap();
    }
    old.execute("INSERT INTO agent_store_format VALUES (21)", [])
        .unwrap();
    old.execute_batch(&format!(
        "ATTACH DATABASE '{}' AS source",
        database.0.display()
    ))
    .unwrap();
    for table in super::BUSINESS_TABLES {
        let present: bool = old
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name=?1)",
                [table],
                |row| row.get(0),
            )
            .unwrap();
        if !present {
            continue;
        }
        let projection = match *table {
            "agents" => "agent_id, profile, is_default",
            "agent_sessions" => "session_id, config, created_at_ms",
            "timeline_events" => {
                "session_id, branch_id, sequence, last_sequence, kind, body, event_id"
            }
            _ => "*",
        };
        old.execute_batch(&format!(
            "INSERT INTO main.{table} SELECT {projection} FROM source.{table}"
        ))
        .unwrap();
    }
    old.execute_batch("DETACH DATABASE source").unwrap();
    assert_eq!(super::schema(&old), super::schema_of(21));
    drop(old);
    let database = Database(v21);
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

/// #1099: the v34 data-only step rewrites `session_catalog.updated_at_ms`
/// as the newest activity event on `main`. A store whose row was inflated by
/// a trailing `branch/resume` is recomputed to the last activity event, a
/// Session with no activity at all lands on its creation time, and every
/// other column of both rows stays as stored.
#[tokio::test]
async fn a_v33_store_recomputes_the_catalog_activity_time_on_upgrade() {
    let database = Database::new();
    let connection = Connection::open(&database.0).unwrap();
    let mut statements = schema_of(33);
    statements.sort_by_key(|sql| !sql.starts_with("CREATE TABLE"));
    for sql in statements {
        connection.execute_batch(&sql).unwrap();
    }
    connection
        .execute("INSERT INTO agent_store_format VALUES (33)", [])
        .unwrap();
    assert_eq!(schema(&connection), schema_of(33));
    connection
        .execute_batch(
            "INSERT INTO agents VALUES ('a1', '{}', 1, NULL);
             INSERT INTO agent_sessions (session_id, config, created_at_ms, origin) VALUES
               ('inflated', '{}', 100, 'client'),
               ('quiet', '{}', 200, 'client');
             INSERT INTO session_owners VALUES ('inflated', 'a1', 3), ('quiet', 'a1', 3);
             INSERT INTO timeline_branches VALUES
               ('inflated', 'main', '{\"id\":\"main\"}', 2),
               ('quiet', 'main', '{\"id\":\"main\"}', 0);",
        )
        .unwrap();
    let event = |id: &str, sequence: u64, create_at: u64, name: &str, data: serde_json::Value| {
        serde_json::json!({"encoding":"event","record":{"event":{
            "id":id,"sequence":sequence,"create_at":create_at,"type":"input","name":name,"data":data
        },"refs":null,"trace":null,"batch_end":sequence}})
    };
    connection
        .execute(
            "INSERT INTO timeline_events VALUES (?1,'main',1,1,'input/message',?2,?3,0,0)",
            params![
                "inflated",
                event(
                    "e1",
                    1,
                    1000,
                    "message",
                    serde_json::json!({"user":{"content":[{"type":"text","text":"Hi"}]}})
                )
                .to_string(),
                "e1"
            ],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO timeline_events VALUES (?1,'main',2,2,'branch/resume',?2,?3,0,0)",
            params![
                "inflated",
                event("e2", 2, 9000, "resume", serde_json::json!({})).to_string(),
                "e2"
            ],
        )
        .unwrap();
    // The v33 writer gave both rows the trailing resume's time. `inflated`
    // carries its first user text as the preview, as a v33 row of a Session
    // with a user message does.
    connection
        .execute(
            "INSERT INTO session_catalog VALUES ('inflated', 9000, 'Hi', '/w'), ('quiet', 9000, NULL, '/w')",
            [],
        )
        .unwrap();
    let stored = |connection: &Connection, session: &str| {
        connection
            .query_row(
                "SELECT updated_at_ms, preview, cwd FROM session_catalog WHERE session_id=?1",
                [session],
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
    let before = [
        stored(&connection, "inflated"),
        stored(&connection, "quiet"),
    ];
    drop(connection);

    let db = SqliteStorage::open(database.0.clone()).unwrap();
    assert_eq!(db.upgraded_from(), Some(33));
    let connection = Connection::open(&database.0).unwrap();
    assert_eq!(schema(&connection), schema_of(STORE_FORMAT));
    assert_eq!(marker(&connection), [i64::from(STORE_FORMAT)]);
    let recomputed = |before: &(i64, Option<String>, String), time: i64| {
        (time, before.1.clone(), before.2.clone())
    };
    assert_eq!(
        stored(&connection, "inflated"),
        recomputed(&before[0], 1000),
        "the upgrade puts the row back on the rule and changes nothing else"
    );
    assert_eq!(
        stored(&connection, "quiet"),
        recomputed(&before[1], 200),
        "a Session with no activity sits at its creation time"
    );
    drop(connection);
    db.close().await.unwrap();
    let db = SqliteStorage::open(database.0.clone()).unwrap();
    assert_eq!(db.upgraded_from(), None);
    db.close().await.unwrap();
}

use super::*;
use dim_agent_runtime::{
    QueryComparison, QueryFilter, QueryOperand, QueryOutput, QueryPage, QuerySelect, QuerySpec,
    QueryValue, TimelineReadScope, TimelineReadSegment,
};
use serde_json::{Value, json};
use std::num::NonZeroU64;

fn eq(path: &str, value: Value) -> QueryFilter {
    QueryFilter::Compare {
        left: QueryOperand::Field { path: path.into() },
        comparison: QueryComparison::Eq,
        right: QueryOperand::Value { value },
    }
}

fn query(fields: &[&str]) -> QuerySpec {
    QuerySpec {
        filter: Some(eq("kind", json!("rows/array"))),
        select: QuerySelect::Fields {
            fields: fields.iter().map(|field| (*field).into()).collect(),
        },
        group_by: vec![],
        pick: None,
        order_by: vec![],
    }
}

fn page() -> QueryPage {
    QueryPage {
        limit: NonZeroU64::new(2).unwrap(),
        after: None,
    }
}

#[tokio::test]
async fn v38_array_upgrade_is_atomic_reuses_versions_and_rejects_missing_locators() {
    let database = Database::new();
    let db = SqliteStorage::open(database.0.clone()).unwrap();
    let store = SqliteTimelineStore::new(db.clone());
    store
        .create_session("rows", &SessionConfig::default())
        .await
        .unwrap();
    let records = (1..=2)
        .map(|sequence| {
            record(
                sequence,
                EventKind::Extension(
                    ExtensionEvent::new("legacy", "array", json!({"changes":[]})).unwrap(),
                ),
            )
        })
        .collect();
    append(&store, key("rows", "main"), records).await.unwrap();
    store.close().await.unwrap();
    db.close().await.unwrap();
    drop(store);
    drop(db);
    let values = (0..257)
        .map(|index| json!({"number":index,"body":"large old value ".repeat(16)}))
        .collect::<Vec<_>>();
    let connection = Connection::open(&database.0).unwrap();
    let mut originals = Vec::new();
    for sequence in 1..=2 {
        let body: String = connection
            .query_row(
                "SELECT body FROM timeline_events WHERE session_id='rows' AND sequence=?1",
                [sequence],
                |row| row.get(0),
            )
            .unwrap();
        let mut wire: Value = serde_json::from_str(&body).unwrap();
        wire["record"]["event"]["data"]["changes"] = json!(values);
        originals.push(wire["record"]["event"].clone());
        connection
            .execute(
                "UPDATE timeline_events SET body=?2 WHERE session_id='rows' AND sequence=?1",
                params![sequence, wire.to_string()],
            )
            .unwrap();
    }
    drop_v40(&connection);
    without_automation_origin(&connection);
    connection.execute_batch("DROP TABLE timeline_array_elements; DROP TABLE agent_store_format; CREATE TABLE agent_store_format (version INTEGER NOT NULL PRIMARY KEY CHECK (version = 38)); INSERT INTO agent_store_format VALUES(38);").unwrap();
    assert_eq!(schema(&connection), schema_of(38));
    let old_events = rows(&connection, "timeline_events");
    drop(connection);
    let mut exported = Vec::new();
    dim_app_server::storage::export_timeline(database.0.clone(), "rows", &mut exported)
        .await
        .unwrap();
    let exported = String::from_utf8(exported).unwrap();
    let exported = exported
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(exported[0]["store_format"], 38);
    assert_eq!(
        exported
            .iter()
            .filter(|line| line["kind"] == "event")
            .map(|line| line["event"].clone())
            .collect::<Vec<_>>(),
        originals
    );
    let connection = Connection::open(&database.0).unwrap();
    assert_eq!(schema(&connection), schema_of(38));
    assert_eq!(rows(&connection, "timeline_events"), old_events);
    connection.execute("UPDATE timeline_events SET body=json_set(body,'$.encoding','broken') WHERE session_id='rows' AND sequence=2",[]).unwrap();
    let broken = rows(&connection, "timeline_events");
    let content = rows(&connection, "timeline_content");
    drop(connection);
    assert!(SqliteStorage::open(database.0.clone()).is_err());
    let connection = Connection::open(&database.0).unwrap();
    assert_eq!(schema(&connection), schema_of(38));
    assert_eq!(marker(&connection), [38]);
    assert_eq!(rows(&connection, "timeline_events"), broken);
    assert_eq!(rows(&connection, "timeline_content"), content);
    connection.execute("UPDATE timeline_events SET body=json_set(body,'$.encoding','event') WHERE session_id='rows' AND sequence=2",[]).unwrap();
    drop(connection);
    let db = SqliteStorage::open(database.0.clone()).unwrap();
    assert_eq!(db.upgraded_from(), Some(38));
    let store = SqliteTimelineStore::new(db.clone());
    let events = store.read(&key("rows", "main"), 1..=2).await.unwrap();
    assert_eq!(
        events
            .iter()
            .map(|record| serde_json::to_value(&record.event).unwrap())
            .collect::<Vec<_>>(),
        originals
    );
    let connection = Connection::open(&database.0).unwrap();
    assert_eq!(schema(&connection), schema_of(STORE_FORMAT));
    assert_eq!(
        connection
            .query_row(
                "SELECT count(*) FROM timeline_array_elements WHERE session_id='rows'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        257
    );
    let locators = rows(&connection, "timeline_array_elements");
    let content = rows(&connection, "timeline_content");
    drop(connection);
    let mut exported = Vec::new();
    dim_app_server::storage::export_timeline(database.0.clone(), "rows", &mut exported)
        .await
        .unwrap();
    let exported = String::from_utf8(exported).unwrap();
    let exported = exported
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(exported[0]["store_format"], STORE_FORMAT);
    assert_eq!(
        exported
            .iter()
            .filter(|line| line["kind"] == "event")
            .map(|line| line["event"].clone())
            .collect::<Vec<_>>(),
        originals
    );
    store.close().await.unwrap();
    db.close().await.unwrap();
    drop(store);
    drop(db);
    let db = SqliteStorage::open(database.0.clone()).unwrap();
    assert_eq!(db.upgraded_from(), None);
    let store = SqliteTimelineStore::new(db.clone());
    let connection = Connection::open(&database.0).unwrap();
    assert_eq!(rows(&connection, "timeline_array_elements"), locators);
    assert_eq!(rows(&connection, "timeline_content"), content);
    connection
        .execute(
            "DELETE FROM timeline_array_elements WHERE session_id='rows' AND element_index=100",
            [],
        )
        .unwrap();
    drop(connection);
    let scope = TimelineReadScope {
        key: key("rows", "main"),
        through: 2,
        segments: vec![TimelineReadSegment {
            branch_id: "main".into(),
            from: 1,
            through: 2,
            stored_through: 2,
            tail: vec![],
        }],
    };
    let mut q = query(&["data.changes[100]"]);
    q.filter = Some(QueryFilter::And {
        conditions: vec![
            eq("kind", json!("legacy/array")),
            eq("id", json!("event-1")),
        ],
    });
    assert!(
        store.query(&scope, &q, Some(&page())).await.is_err(),
        "a selected missing locator must fail instead of returning no element"
    );
    store.branches("rows").await.unwrap();
    store.close().await.unwrap();
    db.close().await.unwrap();
}

#[tokio::test]
async fn large_prompt_arrays_seek_paired_elements_and_preserve_trace_search_and_fork() {
    let mut costs = Vec::new();
    for size in [128u64, 2048] {
        let database = Database::new();
        let db = SqliteStorage::open(database.0.clone()).unwrap();
        let store = SqliteTimelineStore::new(db.clone());
        store
            .create_session("rows", &SessionConfig::default())
            .await
            .unwrap();
        let changes = |prefix: &str| {
            (0..size)
                .map(|index| PromptChange::Set {
                    entry: PromptEntry {
                        source: format!("{prefix}-{index}"),
                        version: "1".into(),
                        body: format!(
                            "body-{prefix}-{index} {} {}",
                            "x".repeat(256),
                            if prefix == "a" && index == 2 {
                                "needleword"
                            } else {
                                "ordinary"
                            }
                        ),
                        scope: PromptScope::Branch {
                            timeline: "main".into(),
                        },
                        delivery: PromptDelivery::System,
                        order: i32::try_from(index).unwrap(),
                    },
                })
                .collect::<Vec<_>>()
        };
        let a = changes("a");
        let b = changes("b");
        let kind = |revision: &str, changes: Vec<PromptChange>| {
            EventKind::PromptChanged(PromptChangedData {
                timeline: "main".into(),
                revision: revision.into(),
                previous: None,
                reason: PromptChangeReason::Refresh,
                changes,
                inputs: Vec::new(),
            })
        };
        let mut first = record(size + 1, kind("a", a.clone()));
        first.trace = Some(TimelineTrace {
            event_id: first.event.id.clone(),
            original: EventDraft {
                parent_call_id: None,
                id: None,
                tags: None,
                kind: kind("before", changes("before")),
            },
            steps: vec![TraceStep {
                handler: "replace-prompt".into(),
                result: Some(EventDraft {
                    parent_call_id: None,
                    id: None,
                    tags: None,
                    kind: first.event.kind.clone(),
                }),
            }],
        });
        let second = record(size + 2, kind("b", b.clone()));
        let mut records = (1..=size)
            .map(|sequence| {
                record(
                    sequence,
                    EventKind::Extension(
                        ExtensionEvent::new("rows", "noise", json!({"old":sequence})).unwrap(),
                    ),
                )
            })
            .collect::<Vec<_>>();
        records.extend([first.clone(), second.clone()]);
        append(&store, key("rows", "main"), records).await.unwrap();
        let read = store
            .read(&key("rows", "main"), size + 1..=size + 2)
            .await
            .unwrap();
        assert_eq!(read[0].event, first.event);
        assert_eq!(
            serde_json::to_value(&read[0].trace).unwrap(),
            serde_json::to_value(&first.trace).unwrap()
        );
        assert_eq!(read[1].event, second.event);
        let connection = Connection::open(&database.0).unwrap();
        let arrays:Vec<String>=connection.prepare("SELECT json_extract(body,'$.record.changes.id') FROM timeline_events WHERE session_id='rows' AND sequence>?1 ORDER BY sequence").unwrap().query_map([i64::try_from(size).unwrap()],|row|row.get(0)).unwrap().collect::<rusqlite::Result<_>>().unwrap();
        assert_eq!(arrays.len(), 2);
        assert_eq!(
            connection
                .query_row(
                    "SELECT count(*) FROM timeline_array_elements WHERE session_id='rows'",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            i64::try_from(size * 3).unwrap()
        );
        assert!(connection.query_row("SELECT max(length(CAST(body AS BLOB))) FROM timeline_events WHERE session_id='rows'",[],|row|row.get::<_,i64>(0)).unwrap()<4096);
        drop(connection);
        let config = store.session_config_text("rows").await.unwrap().unwrap();
        store
            .fork_session("rows", "copy", size + 2, &config)
            .await
            .unwrap();
        let copied = store
            .read(&key("copy", "main"), size + 1..=size + 2)
            .await
            .unwrap();
        assert_eq!(copied[0].event, first.event);
        assert_eq!(
            serde_json::to_value(&copied[0].trace).unwrap(),
            serde_json::to_value(&first.trace).unwrap()
        );
        assert_eq!(copied[1].event, second.event);
        let scope = TimelineReadScope {
            key: key("rows", "main"),
            through: size + 2,
            segments: vec![TimelineReadSegment {
                branch_id: "main".into(),
                from: 1,
                through: size + 2,
                stored_through: size + 2,
                tail: vec![],
            }],
        };
        let mut search = query(&["id"]);
        search.filter = Some(QueryFilter::And {
            conditions: vec![
                eq("kind", json!("prompt/changed")),
                QueryFilter::TextCandidate {
                    field: "data".into(),
                    partition_by: vec![],
                    terms: vec!["needleword".into()],
                },
            ],
        });
        for session in ["rows", "copy"] {
            let mut search_scope = scope.clone();
            search_scope.key.session_id = session.into();
            let QueryOutput::Rows { rows } = store
                .query(&search_scope, &search, Some(&page()))
                .await
                .unwrap()
                .result
            else {
                panic!("search identities")
            };
            assert_eq!(rows.len(), 1);
            assert_eq!(
                rows[0].values[0],
                QueryValue::Value {
                    value: json!(first.event.id)
                }
            );
        }
        store.rollback_session("copy").await.unwrap();
        let connection = Connection::open(&database.0).unwrap();
        assert_eq!(
            connection
                .query_row(
                    "SELECT count(*) FROM timeline_array_elements WHERE session_id='copy'",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
        connection.execute("UPDATE timeline_content SET body='{}' WHERE session_id='rows' AND kind='array-element' AND hash NOT IN (SELECT content_ref FROM timeline_array_elements WHERE session_id='rows' AND ((array_id=?1 AND element_index=2) OR (array_id=?2 AND element_index=7)))",params![arrays[0],arrays[1]]).unwrap();
        drop(connection);
        store.branches("rows").await.unwrap();
        store.take_node_loads();
        store.record_query_statistics();
        for (id, index, expected) in [(&first.event.id, 2, &a[2]), (&second.event.id, 7, &b[7])] {
            let field = format!("data.changes[{index}]");
            let mut q = query(&["id", &field]);
            q.filter = Some(QueryFilter::And {
                conditions: vec![eq("kind", json!("prompt/changed")), eq("id", json!(id))],
            });
            let reply = store.query(&scope, &q, Some(&page())).await.unwrap();
            assert!(reply.next.is_none());
            let QueryOutput::Rows { rows } = reply.result else {
                panic!("selected array fields")
            };
            assert_eq!(rows.len(), 1);
            assert_eq!(
                rows[0].values,
                vec![
                    QueryValue::Value { value: json!(id) },
                    QueryValue::Value {
                        value: serde_json::to_value(expected).unwrap()
                    }
                ]
            );
        }
        assert_eq!(
            store.take_node_loads().loads,
            2,
            "only the two selected contents are decoded"
        );
        let stats = store.take_query_statistics();
        assert!(
            stats
                .iter()
                .flat_map(|stat| &stat.plan)
                .any(|line| line.contains("timeline_events_by_id") && line.contains("event_id=?")),
            "{stats:?}"
        );
        costs.push(stats.iter().map(|stat| stat.vm_steps).sum::<i32>());
        let mut exported = Vec::new();
        assert!(
            dim_app_server::storage::export_timeline(database.0.clone(), "rows", &mut exported)
                .await
                .is_err(),
            "complete export still reads the damaged unselected elements"
        );
        let connection = Connection::open(&database.0).unwrap();
        connection.execute("UPDATE timeline_content SET body='[]' WHERE session_id='rows' AND hash=(SELECT content_ref FROM timeline_array_elements WHERE session_id='rows' AND array_id=?1 AND element_index=2)",[&arrays[0]]).unwrap();
        drop(connection);
        let mut q = query(&["data.changes[2]"]);
        q.filter = Some(eq("id", json!(first.event.id)));
        assert!(
            store.query(&scope, &q, Some(&page())).await.is_err(),
            "selected content is always hash checked"
        );
        store.close().await.unwrap();
        db.close().await.unwrap();
    }
    assert!(
        costs[1] < costs[0] * 2,
        "point work must not grow with old events or the source array: {costs:?}"
    );
}

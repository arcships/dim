use super::*;
use dim_agent_runtime::*;
use serde_json::{Value, json};
use std::num::NonZeroU64;

fn equal(path: &str, value: Value) -> QueryFilter {
    QueryFilter::Compare {
        left: QueryOperand::Field { path: path.into() },
        comparison: QueryComparison::Eq,
        right: QueryOperand::Value { value },
    }
}

fn joined(kind: &[&str], inner: &str, outer: &str) -> QueryFilter {
    QueryFilter::And {
        conditions: vec![
            QueryFilter::In {
                value: QueryOperand::Field {
                    path: "kind".into(),
                },
                values: kind.iter().map(|kind| json!(kind)).collect(),
            },
            QueryFilter::Compare {
                left: QueryOperand::Field { path: inner.into() },
                comparison: QueryComparison::Eq,
                right: QueryOperand::Outer { path: outer.into() },
            },
        ],
    }
}

fn pending(family: usize) -> QuerySpec {
    let (origin, terminal) = match family {
        0 => (
            "input/message",
            joined(&["input/consumed", "input/deleted"], "data.input_id", "id"),
        ),
        1 => (
            "exec/started",
            joined(&["exec/ended"], "data.task_id", "data.task_id"),
        ),
        2 => (
            "step/tool-call",
            joined(&["step/tool-result"], "data.call_event_id", "id"),
        ),
        3 => (
            "collaboration/request",
            QueryFilter::Or {
                conditions: vec![
                    joined(&["collaboration/result"], "data.request_event_id", "id"),
                    QueryFilter::And {
                        conditions: vec![
                            joined(&["collaboration/delivered"], "data.op_event_id", "id"),
                            equal("data.status", json!("error")),
                        ],
                    },
                ],
            },
        ),
        _ => unreachable!(),
    };
    let mut conditions = vec![
        equal("kind", json!(origin)),
        QueryFilter::Not {
            condition: Box::new(QueryFilter::Exists {
                condition: Box::new(terminal),
            }),
        },
    ];
    if family == 0 {
        conditions.push(QueryFilter::Not {
            condition: Box::new(QueryFilter::Present {
                value: QueryOperand::Field {
                    path: "data.handover".into(),
                },
            }),
        });
    } else if family == 2 {
        conditions.push(QueryFilter::Any {
            field: "data.calls".into(),
            condition: Box::new(QueryFilter::Compare {
                left: QueryOperand::Element {
                    path: "name".into(),
                },
                comparison: QueryComparison::Eq,
                right: QueryOperand::Value {
                    value: json!("ask_user"),
                },
            }),
        });
    }
    QuerySpec {
        filter: Some(QueryFilter::And { conditions }),
        select: QuerySelect::Fields {
            fields: vec!["id".into(), "position.sequence".into()],
        },
        group_by: vec![],
        pick: None,
        order_by: vec![],
    }
}

fn origin(family: usize, sequence: u64, other_group: bool) -> EventKind {
    match family {
        0 => {
            let mut input = InputMessageData::text("input", None);
            if other_group {
                input.handover = Some(InputHandover {
                    turn_id: "turn".into(),
                    request_id: "request".into(),
                });
            }
            EventKind::InputMessage(input)
        }
        1 => EventKind::ExecStarted(ExecStartedData {
            turn_id: "turn".into(),
            request_id: "request".into(),
            call_id: "call".into(),
            task_id: format!("task-{sequence}"),
            description: "task".into(),
            output_path: "output".into(),
        }),
        2 => {
            let call = ToolCall {
                id: "call".into(),
                name: if other_group { "other" } else { "ask_user" }.into(),
                input: serde_json::from_value(json!({"question":"choose"})).unwrap(),
            };
            EventKind::ToolCall(ToolCallData {
                turn_id: "turn".into(),
                request_id: "request".into(),
                calls: vec![call.clone(), call],
            })
        }
        3 => EventKind::CollaborationRequest(CollaborationRequestData {
            turn_id: "turn".into(),
            request_id: "request".into(),
            call_id: "call".into(),
            target: CollaborationTarget::Agent {
                agent_id: "member".into(),
            },
            prompt: "work".into(),
        }),
        _ => unreachable!(),
    }
}

fn terminal(family: usize, sequence: u64) -> EventKind {
    let input_id = format!("event-{sequence}");
    match family {
        0 if sequence.is_multiple_of(2) => EventKind::InputDeleted(InputDeletedData { input_id }),
        0 => EventKind::InputConsumed(InputConsumedData { input_id }),
        1 => EventKind::ExecEnded(ExecEndedData {
            task_id: format!("task-{sequence}"),
            status: ExecStatus::Completed,
            result: ToolResult::text("done"),
        }),
        2 => EventKind::ToolResult(ToolResultData {
            turn_id: "turn".into(),
            request_id: "request".into(),
            call_event_id: Some(input_id),
            results: vec![],
        }),
        3 if sequence.is_multiple_of(2) => {
            EventKind::CollaborationDelivered(CollaborationDeliveredData {
                op_event_id: input_id,
                delivery: CollaborationDelivery::Error {
                    error: CollaborationErrorCode::Closed,
                },
            })
        }
        3 => EventKind::CollaborationResult(CollaborationResultData {
            request_event_id: input_id,
            agent_id: "member".into(),
            session_id: "member-session".into(),
            last_turn_id: None,
            text: "done".into(),
            outcome: CollaborationOutcome::Completed,
        }),
        _ => unreachable!(),
    }
}

fn read_scope(session: &str, through: u64) -> TimelineReadScope {
    TimelineReadScope {
        key: key(session, "main"),
        through,
        segments: vec![TimelineReadSegment {
            branch_id: "main".into(),
            from: 1,
            through,
            stored_through: through,
            tail: vec![],
        }],
    }
}

#[tokio::test]
async fn indexed_pending_sets_keep_old_pages_and_do_not_read_closed_or_other_group_history() {
    let mut costs = Vec::new();
    for size in [128, 2048] {
        let mut family_costs = Vec::new();
        for family in 0..4 {
            let database = Database::new();
            let db = SqliteStorage::open(database.0.clone()).unwrap();
            let store = SqliteTimelineStore::new(db.clone());
            let memory = MemoryTimelineStore::default();
            store
                .create_session("members", &SessionConfig::default())
                .await
                .unwrap();
            memory
                .create_session("members", &SessionConfig::default())
                .await
                .unwrap();
            let mut records: Vec<_> = (1..=size)
                .map(|sequence| record(sequence, origin(family, sequence, false)))
                .collect();
            if matches!(family, 0 | 2) {
                records.extend(
                    (size + 1..=size * 2)
                        .map(|sequence| record(sequence, origin(family, sequence, true))),
                );
            }
            let mut closed: Vec<_> = (1..=size - 10).collect();
            closed.sort_by_key(|sequence| sequence % 17);
            for origin in closed {
                records.push(record(records.len() as u64 + 1, terminal(family, origin)));
            }
            let through = records.len() as u64;
            append(&store, key("members", "main"), records.clone())
                .await
                .unwrap();
            memory
                .append(
                    key("members", "main"),
                    records,
                    Box::new(|result| result.unwrap()),
                )
                .unwrap();
            let scope = read_scope("members", through);
            let query = pending(family);
            let mut page = QueryPage {
                limit: NonZeroU64::new(4).unwrap(),
                after: None,
            };
            let mut total = 0;
            let mut vm = 0;
            loop {
                store.record_query_statistics();
                let actual = store.query(&scope, &query, Some(&page)).await.unwrap();
                let statistics = store.take_query_statistics();
                assert!(
                    statistics
                        .iter()
                        .any(|statement| statement.stage == "member_root"),
                    "the pending shape must read its version root: {family}"
                );
                vm += statistics
                    .iter()
                    .map(|statement| statement.vm_steps as i64)
                    .sum::<i64>();
                let expected = memory.query(&scope, &query, Some(&page)).await.unwrap();
                assert_eq!(
                    serde_json::to_value(&actual).unwrap(),
                    serde_json::to_value(expected).unwrap()
                );
                let QueryOutput::Rows { rows } = actual.result else {
                    panic!("pending rows")
                };
                total += rows.len();
                let Some(next) = actual.next else { break };
                page.after = Some(next);
                if total == 4 {
                    let sequence = through + 1;
                    append(
                        &store,
                        key("members", "main"),
                        vec![record(sequence, terminal(family, size))],
                    )
                    .await
                    .unwrap();
                }
            }
            assert_eq!(total, 10);
            family_costs.push(vm);
            let old = read_scope("members", size);
            let first = QueryPage {
                limit: NonZeroU64::new(4).unwrap(),
                after: None,
            };
            assert_eq!(
                serde_json::to_value(store.query(&old, &query, Some(&first)).await.unwrap())
                    .unwrap(),
                serde_json::to_value(memory.query(&old, &query, Some(&first)).await.unwrap())
                    .unwrap()
            );
            store.close().await.unwrap();
            db.close().await.unwrap();
            drop(store);
            drop(db);
            if size == 128 {
                let connection = Connection::open(&database.0).unwrap();
                drop_v40(&connection);
                connection.execute_batch("DROP TABLE timeline_array_elements; DROP TABLE timeline_member_roots; DROP TABLE timeline_member_origins; DROP TABLE timeline_member_terminals; DROP TABLE timeline_member_nodes; DROP TABLE agent_store_format; CREATE TABLE agent_store_format (version INTEGER NOT NULL PRIMARY KEY CHECK (version = 36)); INSERT INTO agent_store_format VALUES (36);").unwrap();
                for sql in V36_ADDITIONS
                    .iter()
                    .filter(|sql| sql.starts_with("CREATE INDEX "))
                {
                    let name = sql.split_whitespace().nth(2).unwrap();
                    connection
                        .execute_batch(&format!("DROP INDEX {name}; {sql}"))
                        .unwrap();
                }
            }
            let db = SqliteStorage::open(database.0.clone()).unwrap();
            if size == 128 {
                assert_eq!(db.upgraded_from(), Some(36));
            }
            let store = SqliteTimelineStore::new(db.clone());
            assert_eq!(
                serde_json::to_value(store.query(&scope, &query, Some(&first)).await.unwrap())
                    .unwrap(),
                serde_json::to_value(memory.query(&scope, &query, Some(&first)).await.unwrap())
                    .unwrap()
            );
            if size == 128 {
                let config = store.session_config_text("members").await.unwrap().unwrap();
                store
                    .fork_session("members", "copy", size, &config)
                    .await
                    .unwrap();
                let copy = read_scope("copy", size);
                let result = store.query(&copy, &query, Some(&first)).await.unwrap();
                let expected = store.query(&old, &query, Some(&first)).await.unwrap();
                assert_eq!(
                    serde_json::to_value(result.result).unwrap(),
                    serde_json::to_value(expected.result).unwrap()
                );
                store.rollback_session("members").await.unwrap();
                let QueryOutput::Rows { rows } = store
                    .query(&copy, &query, Some(&first))
                    .await
                    .unwrap()
                    .result
                else {
                    panic!("copied pending")
                };
                assert_eq!(rows.len(), 4);
                store.rollback_session("copy").await.unwrap();
            }
            store.close().await.unwrap();
            db.close().await.unwrap();
        }
        costs.push(family_costs);
    }
    for (small, large) in costs[0].iter().zip(&costs[1]) {
        assert!(
            *large < *small * 2,
            "a 16x history increase must not scan origins or closed objects: {costs:?}"
        );
    }
}

#[tokio::test]
async fn pending_members_preserve_in_batch_positions_and_fail_on_a_missing_old_root_node() {
    let database = Database::new();
    let db = SqliteStorage::open(database.0.clone()).unwrap();
    let store = SqliteTimelineStore::new(db.clone());
    let memory = MemoryTimelineStore::default();
    store
        .create_session("members", &SessionConfig::default())
        .await
        .unwrap();
    memory
        .create_session("members", &SessionConfig::default())
        .await
        .unwrap();
    let mut records = vec![
        record(
            1,
            EventKind::InputDeleted(InputDeletedData {
                input_id: "event-3".into(),
            }),
        ),
        record(2, origin(0, 2, false)),
        record(3, origin(0, 3, false)),
        record(
            4,
            EventKind::InputConsumed(InputConsumedData {
                input_id: "event-2".into(),
            }),
        ),
        record(
            5,
            EventKind::InputDeleted(InputDeletedData {
                input_id: "event-3".into(),
            }),
        ),
    ];
    for record in &mut records {
        record.batch_end = 5;
    }
    append(&store, key("members", "main"), records.clone())
        .await
        .unwrap();
    memory
        .append(
            key("members", "main"),
            records,
            Box::new(|result| result.unwrap()),
        )
        .unwrap();
    let query = pending(0);
    let page = QueryPage {
        limit: NonZeroU64::new(4).unwrap(),
        after: None,
    };
    for through in 0..=5 {
        let scope = read_scope("members", through);
        assert_eq!(
            serde_json::to_value(store.query(&scope, &query, Some(&page)).await.unwrap()).unwrap(),
            serde_json::to_value(memory.query(&scope, &query, Some(&page)).await.unwrap()).unwrap()
        );
    }
    store.close().await.unwrap();
    db.close().await.unwrap();
    drop(store);
    drop(db);
    Connection::open(&database.0).unwrap().execute("DELETE FROM timeline_member_nodes WHERE id IN (SELECT root_id FROM timeline_member_roots WHERE session_id='members' AND sequence=2)", []).unwrap();
    let db = SqliteStorage::open(database.0.clone()).unwrap();
    let store = SqliteTimelineStore::new(db.clone());
    assert!(
        store
            .query(&read_scope("members", 2), &query, Some(&page))
            .await
            .is_err(),
        "a missing retained node must not become an empty pending set"
    );
    store.close().await.unwrap();
    db.close().await.unwrap();
}

#[test]
fn a_failed_v36_member_backfill_rolls_back_the_schema_marker_and_events() {
    let database = Database::new();
    let connection = Connection::open(&database.0).unwrap();
    let mut statements = schema_of(36);
    statements.sort_by_key(|statement| !statement.starts_with("CREATE TABLE"));
    for statement in statements {
        connection.execute_batch(&statement).unwrap();
    }
    connection
        .execute("INSERT INTO agent_store_format VALUES(36)", [])
        .unwrap();
    insert_documented_records(&connection, BUSINESS_TABLES);
    connection.execute("UPDATE timeline_events SET body=json_set(body,'$.record.event.sequence','invalid') WHERE kind='input/message'", []).unwrap();
    let original = rows(&connection, "timeline_events");
    drop(connection);
    assert!(SqliteStorage::open(database.0.clone()).is_err());
    let connection = Connection::open(&database.0).unwrap();
    assert_eq!(schema(&connection), schema_of(36));
    assert_eq!(marker(&connection), [36]);
    assert_eq!(rows(&connection, "timeline_events"), original);
}

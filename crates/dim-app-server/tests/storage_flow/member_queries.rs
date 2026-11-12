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
        4 => (
            "approval/request",
            joined(
                &["approval/respond", "approval/cancelled"],
                "data.approval_id",
                "data.approval_id",
            ),
        ),
        5 => (
            "collaboration/message",
            joined(&["collaboration/delivered"], "data.op_event_id", "id"),
        ),
        6 => {
            let QueryFilter::And { conditions } = unposted_filter() else {
                unreachable!()
            };
            return QuerySpec {
                filter: Some(QueryFilter::And { conditions }),
                select: QuerySelect::Fields {
                    fields: vec!["id".into(), "position.sequence".into()],
                },
                group_by: vec![],
                pick: None,
                order_by: vec![],
            };
        }
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
        4 => EventKind::ApprovalRequest(ApprovalRequestData {
            approval_id: format!("approval-{sequence}"),
            source: BranchSource::Host {
                operation_id: "approval".into(),
            },
            call: ApprovalCallData {
                name: "shell".into(),
                input: Default::default(),
            },
            reason: "choose".into(),
        }),
        5 => EventKind::CollaborationMessage(CollaborationMessageData {
            turn_id: "turn".into(),
            request_id: "request".into(),
            call_id: "call".into(),
            agent_id: "member".into(),
            session_id: "peer".into(),
            mode: CollaborationMessageMode::Append,
            content: "work".into(),
        }),
        6 => EventKind::ExecEnded(ExecEndedData {
            task_id: format!("task-{sequence}"),
            status: ExecStatus::Completed,
            result: ToolResult::text("done"),
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
        4 => EventKind::ApprovalCancelled(ApprovalCancelledData {
            approval_id: format!("approval-{sequence}"),
            reason: "closed".into(),
        }),
        5 => EventKind::CollaborationDelivered(CollaborationDeliveredData {
            op_event_id: input_id,
            delivery: CollaborationDelivery::Error {
                error: CollaborationErrorCode::Closed,
            },
        }),
        6 => EventKind::InputMessage(InputMessageData::default()),
        _ => unreachable!(),
    }
}

fn closed_record(family: usize, sequence: u64, origin: u64) -> StoredEvent {
    let mut event = record(sequence, terminal(family, origin));
    if family == 6 {
        Arc::make_mut(&mut event.event).tags =
            Some(vec![format!("exec-wake-result:event-{origin}")]);
    }
    event
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
        for family in 0..7 {
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
                records.push(closed_record(family, records.len() as u64 + 1, origin));
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
                        vec![closed_record(family, sequence, size)],
                    )
                    .await
                    .unwrap();
                }
            }
            assert_eq!(total, 10);
            if family == 2 {
                let mut all_tools = query.clone();
                let Some(QueryFilter::And { conditions }) = &mut all_tools.filter else {
                    unreachable!()
                };
                conditions.retain(|condition| !matches!(condition, QueryFilter::Any { .. }));
                let first = QueryPage {
                    limit: NonZeroU64::new(4).unwrap(),
                    after: None,
                };
                store.record_query_statistics();
                let actual = store.query(&scope, &all_tools, Some(&first)).await.unwrap();
                let statistics = store.take_query_statistics();
                assert!(
                    statistics
                        .iter()
                        .any(|statement| statement.stage == "member_group"),
                    "all tools must seek the existing group roots"
                );
                assert_eq!(
                    serde_json::to_value(actual).unwrap(),
                    serde_json::to_value(
                        memory
                            .query(&scope, &all_tools, Some(&first))
                            .await
                            .unwrap()
                    )
                    .unwrap()
                );
                vm += statistics
                    .iter()
                    .map(|statement| i64::from(statement.vm_steps))
                    .sum::<i64>();
            }
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
                without_automation_origin(&connection);
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

fn in_field(path: &str, values: &[&str]) -> QueryFilter {
    QueryFilter::In {
        value: QueryOperand::Field { path: path.into() },
        values: values.iter().map(|value| (*value).into()).collect(),
    }
}

fn unposted_filter() -> QueryFilter {
    let mut conditions = vec![in_field("kind", &["exec/ended"])];
    conditions.extend(vec![
        in_field("data.status", &["completed", "failed"]),
        QueryFilter::Not {
            condition: Box::new(QueryFilter::Exists {
                condition: Box::new(QueryFilter::And {
                    conditions: vec![
                        in_field("kind", &["stop/interrupt"]),
                        in_field("data.scope", &["branch"]),
                        QueryFilter::Compare {
                            left: QueryOperand::Field {
                                path: "position.sequence".into(),
                            },
                            comparison: QueryComparison::Le,
                            right: QueryOperand::Outer {
                                path: "position.sequence".into(),
                            },
                        },
                        QueryFilter::Not {
                            condition: Box::new(QueryFilter::Exists {
                                condition: Box::new(QueryFilter::And {
                                    conditions: vec![
                                        in_field(
                                            "kind",
                                            &["branch/start", "branch/resume", "session/forked"],
                                        ),
                                        QueryFilter::Compare {
                                            left: QueryOperand::Field {
                                                path: "position.sequence".into(),
                                            },
                                            comparison: QueryComparison::Gt,
                                            right: QueryOperand::Outer {
                                                path: "position.sequence".into(),
                                            },
                                        },
                                        QueryFilter::Compare {
                                            left: QueryOperand::Field {
                                                path: "position.sequence".into(),
                                            },
                                            comparison: QueryComparison::Le,
                                            right: QueryOperand::Root {
                                                path: "position.sequence".into(),
                                            },
                                        },
                                    ],
                                }),
                            }),
                        },
                    ],
                }),
            }),
        },
    ]);
    conditions.push(QueryFilter::Not {
        condition: Box::new(QueryFilter::Exists {
            condition: Box::new(QueryFilter::And {
                conditions: vec![
                    in_field("kind", &["input/message"]),
                    QueryFilter::Any {
                        field: "tags".into(),
                        condition: Box::new(QueryFilter::Compare {
                            left: QueryOperand::Element {
                                path: String::new(),
                            },
                            comparison: QueryComparison::Eq,
                            right: QueryOperand::Concat {
                                parts: vec![
                                    QueryOperand::Value {
                                        value: "exec-wake-result:".into(),
                                    },
                                    QueryOperand::Outer { path: "id".into() },
                                ],
                            },
                        }),
                    },
                ],
            }),
        }),
    });
    QueryFilter::And { conditions }
}

#[tokio::test]
async fn v42_notifications_preserve_original_close_boundaries_tails_and_task_identity_after_upgrade()
 {
    for (source, broken, resume) in [
        (41, false, false),
        (41, true, false),
        (42, false, false),
        (42, true, false),
        (43, false, false),
        (43, true, false),
        (42, false, true),
        (43, false, true),
    ] {
        let database = Database::new();
        let db = SqliteStorage::open(database.0.clone()).unwrap();
        let store = SqliteTimelineStore::new(db.clone());
        let memory = MemoryTimelineStore::default();
        for owner in [&*store as &dyn TimelineStore, &memory] {
            owner
                .create_session("external", &SessionConfig::default())
                .await
                .unwrap();
        }
        let start = |task: &str| {
            EventKind::ExecStarted(ExecStartedData {
                turn_id: "t".into(),
                request_id: "r".into(),
                call_id: "c".into(),
                task_id: task.into(),
                description: task.into(),
                output_path: "output".into(),
            })
        };
        let end = |task: &str, status| {
            EventKind::ExecEnded(ExecEndedData {
                task_id: task.into(),
                status,
                result: ToolResult::text("done"),
            })
        };
        let stop = || {
            EventKind::StopInterrupt(StopInterruptData {
                reason: "closed".into(),
                scope: InterruptScope::Branch,
            })
        };
        let kinds = vec![
            start("任务/一"),
            end("任务/一", ExecStatus::Completed),
            start("late"),
            stop(),
            end("late", ExecStatus::Failed),
            EventKind::BranchResume(BranchResumeData {}),
            start("new"),
            end("new", ExecStatus::Failed),
            EventKind::InputMessage(InputMessageData::default()),
            start("cancelled"),
            end("cancelled", ExecStatus::Cancelled),
            EventKind::InputMessage(InputMessageData::default()),
            start("last"),
            end("last", ExecStatus::Completed),
            stop(),
        ];
        let mut records: Vec<_> = kinds
            .into_iter()
            .enumerate()
            .map(|(i, kind)| record(i as u64 + 1, kind))
            .collect();
        if source == 43 && !resume {
            store
                .create_session("steer", &SessionConfig::default())
                .await
                .unwrap();
            append(
                &store,
                key("steer", "main"),
                vec![record(
                    1,
                    EventKind::InputWithdraw(InputWithdrawData {
                        input_id: "event-12".into(),
                        reason: None,
                        steer: Some(InputMessageData {
                            mode: Some(InputMode::Steer),
                            user: Some(UserInput {
                                content: vec![ContentBlock::Text {
                                    text: "checked replacement survives upgrade".into(),
                                }],
                            }),
                            ..InputMessageData::default()
                        }),
                    }),
                )],
            )
            .await
            .unwrap();
        }
        for (at, end) in [(8, 2), (11, 8)] {
            Arc::make_mut(&mut records[at].event).tags =
                Some(vec![format!("exec-wake-result:event-{end}")]);
        }
        append(&store, key("external", "main"), records.clone())
            .await
            .unwrap();
        memory
            .append(
                key("external", "main"),
                records,
                Box::new(|result| result.unwrap()),
            )
            .unwrap();
        store.close().await.unwrap();
        db.close().await.unwrap();
        drop(store);
        drop(db);
        {
            let connection = Connection::open(&database.0).unwrap();
            if source == 41 || source == 42 && resume {
                without_automation_origin(&connection);
            }
            if !resume {
                connection.execute_batch("DROP INDEX timeline_events_branch_interrupts; DELETE FROM timeline_member_roots WHERE index_id IN ('approval/v1','collaboration-delivery/v1','exec-wake/v1'); DELETE FROM timeline_member_origins WHERE index_id IN ('approval/v1','collaboration-delivery/v1','exec-wake/v1') OR index_id='exec/v1'; DELETE FROM timeline_member_terminals WHERE index_id IN ('approval/v1','collaboration-delivery/v1','exec-wake/v1');").unwrap();
            }
            connection.execute_batch(&format!("DROP TABLE agent_store_format; CREATE TABLE agent_store_format (version INTEGER NOT NULL PRIMARY KEY CHECK (version = {source})); INSERT INTO agent_store_format VALUES({source});")).unwrap();
            if broken {
                connection.execute("UPDATE timeline_events SET body=json_set(body,'$.record.event.data.task_id',json('[{}]')) WHERE session_id='external' AND sequence=13", []).unwrap();
            }
        }
        let connection = Connection::open(&database.0).unwrap();
        let facts = rows(&connection, "timeline_events");
        let indexed = [
            "timeline_member_roots",
            "timeline_member_origins",
            "timeline_member_terminals",
        ]
        .map(|table| rows(&connection, table));
        drop(connection);
        if broken {
            assert!(SqliteStorage::open(database.0.clone()).is_err());
            let connection = Connection::open(&database.0).unwrap();
            assert_eq!(marker(&connection), [source]);
            assert_eq!(rows(&connection, "timeline_events"), facts);
            assert_eq!(schema(&connection), schema_of(source as u32));
            let count: i64 = connection
                .query_row(
                    "SELECT count(*) FROM timeline_member_roots WHERE index_id='exec-wake/v1'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(
                count, 0,
                "a failed upgrade must roll back the derived members too"
            );
            continue;
        }
        let db = SqliteStorage::open(database.0.clone()).unwrap();
        assert_eq!(db.upgraded_from(), Some(source as u32));
        let store = SqliteTimelineStore::new(db.clone());
        let connection = Connection::open(&database.0).unwrap();
        assert_eq!(rows(&connection, "timeline_events"), facts);
        if resume {
            assert_eq!(
                [
                    "timeline_member_roots",
                    "timeline_member_origins",
                    "timeline_member_terminals"
                ]
                .map(|table| rows(&connection, table)),
                indexed,
                "resume-format upgrades preserve existing member relationships"
            );
        }
        drop(connection);
        if source == 43 && !resume {
            let restored = store.read(&key("steer", "main"), 1..=1).await.unwrap();
            let EventKind::InputWithdraw(withdrawal) = &restored[0].event.kind else {
                panic!("withdrawal survives the format upgrade");
            };
            assert_eq!(
                withdrawal.steer.as_ref().unwrap().mode,
                Some(InputMode::Steer)
            );
            assert_eq!(
                withdrawal.steer.as_ref().unwrap().user,
                Some(UserInput {
                    content: vec![ContentBlock::Text {
                        text: "checked replacement survives upgrade".into()
                    }]
                })
            );
        }
        let query = QuerySpec {
            filter: Some(unposted_filter()),
            select: QuerySelect::Fields {
                fields: vec!["id".into()],
            },
            group_by: vec![],
            pick: None,
            order_by: vec![],
        };
        let page = QueryPage {
            limit: NonZeroU64::new(10).unwrap(),
            after: None,
        };
        for (through, expected) in [
            (2, vec!["event-2"]),
            (5, vec!["event-2"]),
            (8, vec!["event-2", "event-8"]),
            (9, vec!["event-8"]),
            (12, vec![]),
            (15, vec!["event-14"]),
        ] {
            let scope = read_scope("external", through);
            store.record_query_statistics();
            let actual = store.query(&scope, &query, Some(&page)).await.unwrap();
            let statistics = store.take_query_statistics();
            assert!(
                statistics
                    .iter()
                    .any(|statement| statement.stage == "member_root"),
                "the complete notification predicate must use its root"
            );
            assert_eq!(
                serde_json::to_value(&actual).unwrap(),
                serde_json::to_value(memory.query(&scope, &query, Some(&page)).await.unwrap())
                    .unwrap()
            );
            let QueryOutput::Rows { rows } = actual.result else {
                unreachable!()
            };
            assert_eq!(
                rows.into_iter()
                    .map(|row| row.values[0].clone())
                    .collect::<Vec<_>>(),
                expected
                    .into_iter()
                    .map(|id| QueryValue::Value { value: id.into() })
                    .collect::<Vec<_>>()
            );
        }
        let mut fallback = query.clone();
        if let Some(QueryFilter::And { conditions }) = &mut fallback.filter
            && let QueryFilter::Not { condition } = &mut conditions[2]
            && let QueryFilter::Exists { condition } = condition.as_mut()
            && let QueryFilter::And { conditions } = condition.as_mut()
        {
            conditions[0] = equal("kind", "stop/interrupt".into());
        } else {
            unreachable!("close predicate");
        }
        for through in [8, 9, 15] {
            let scope = read_scope("external", through);
            assert_eq!(
                serde_json::to_value(store.query(&scope, &fallback, Some(&page)).await.unwrap())
                    .unwrap(),
                serde_json::to_value(memory.query(&scope, &fallback, Some(&page)).await.unwrap())
                    .unwrap()
            );
        }
        for (kind, task, expected) in [
            ("exec/started", "任务/一", Some("event-1")),
            ("exec/ended", "任务/一", Some("event-2")),
            ("exec/started", "missing/任务", None),
        ] {
            let query = QuerySpec {
                filter: Some(QueryFilter::And {
                    conditions: vec![in_field("kind", &[kind]), in_field("data.task_id", &[task])],
                }),
                select: QuerySelect::Fields {
                    fields: vec!["id".into()],
                },
                group_by: vec![],
                pick: Some(QueryPick::First),
                order_by: vec![],
            };
            store.record_query_statistics();
            let actual = store
                .query(&read_scope("external", 15), &query, Some(&page))
                .await
                .unwrap();
            let statistics = store.take_query_statistics();
            assert!(
                statistics
                    .iter()
                    .any(|statement| statement.stage == "member_identity")
            );
            let QueryOutput::Rows { rows } = actual.result else {
                unreachable!()
            };
            assert_eq!(
                rows.first().map(|row| row.values[0].clone()),
                expected.map(|id| QueryValue::Value { value: id.into() })
            );
        }
        let mut wake = record(19, EventKind::InputMessage(InputMessageData::default()));
        Arc::make_mut(&mut wake.event).tags = Some(vec!["exec-wake-result:event-14".into()]);
        let tail = vec![
            record(16, EventKind::BranchResume(BranchResumeData {})),
            record(17, start("tail")),
            record(18, end("tail", ExecStatus::Completed)),
            wake,
        ];
        let mut scope = read_scope("external", 19);
        scope.segments[0].stored_through = 15;
        scope.segments[0].tail = tail.iter().map(|row| row.event.clone()).collect();
        let actual = store.query(&scope, &query, Some(&page)).await.unwrap();
        assert_eq!(
            serde_json::to_value(&actual).unwrap(),
            serde_json::to_value(memory.query(&scope, &query, Some(&page)).await.unwrap()).unwrap()
        );
        let QueryOutput::Rows { rows } = actual.result else {
            unreachable!()
        };
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].values[0],
            QueryValue::Value {
                value: "event-18".into()
            }
        );
        append(&store, key("external", "main"), tail).await.unwrap();
        store.close().await.unwrap();
        db.close().await.unwrap();
        drop(store);
        drop(db);
        let db = SqliteStorage::open(database.0.clone()).unwrap();
        assert_eq!(db.upgraded_from(), None);
        let store = SqliteTimelineStore::new(db.clone());
        let QueryOutput::Rows { rows } = store
            .query(&read_scope("external", 19), &query, Some(&page))
            .await
            .unwrap()
            .result
        else {
            unreachable!()
        };
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].values[0],
            QueryValue::Value {
                value: "event-18".into()
            }
        );
        store.close().await.unwrap();
        db.close().await.unwrap();
    }
}

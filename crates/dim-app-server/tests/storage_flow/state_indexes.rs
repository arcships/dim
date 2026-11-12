use super::*;
use dim_agent_runtime::*;
use serde_json::json;
use std::num::NonZeroU64;

fn equal(path: &str, value: serde_json::Value) -> QueryFilter {
    QueryFilter::Compare {
        left: QueryOperand::Field { path: path.into() },
        comparison: QueryComparison::Eq,
        right: QueryOperand::Value { value },
    }
}

#[tokio::test]
async fn restore_field_reads_seek_the_complete_control_and_numeric_keys() {
    let mut costs = Vec::new();
    for rounds in [3, 30] {
        let database = Database::new();
        let db = SqliteStorage::open(database.0.clone()).unwrap();
        let store = SqliteTimelineStore::new(db.clone());
        store
            .create_session("state-reads", &SessionConfig::default())
            .await
            .unwrap();
        let mut records = Vec::new();
        for turn in 0..rounds {
            let scope = PromptScope::Task {
                timeline: "main".into(),
                id: format!("task-{turn}"),
            };
            let mut push = |kind| {
                let sequence = records.len() as u64 + 1;
                records.push(record(sequence, kind));
                sequence
            };
            push(EventKind::TurnStart(TurnStartData {
                turn_id: turn.to_string(),
            }));
            push(EventKind::ContextScope(ContextScopeData {
                scope: scope.clone(),
                status: ContextScopeStatus::Started,
            }));
            let boundary = push(EventKind::InputConsumed(InputConsumedData {
                input_id: format!("input-{turn}"),
            }));
            push(EventKind::Extension(ExtensionEvent::new("context-usage", "recovered", json!({"schema":"context-usage/v4","epoch":"epoch","current":{"at":{"branch":0,"sequence":boundary},"totals":{"bytes":turn}},"source_through":boundary})).unwrap()));
            push(EventKind::ContextScope(ContextScopeData {
                scope,
                status: ContextScopeStatus::Ended,
            }));
            push(EventKind::TurnEnd(TurnEndData {
                turn_id: turn.to_string(),
                outcome: TurnOutcome::Completed {
                    reason: "done".into(),
                },
            }));
        }
        let head = records.len() as u64;
        append(&store, key("state-reads", "main"), records)
            .await
            .unwrap();
        let scope = TimelineReadScope {
            key: key("state-reads", "main"),
            through: head,
            segments: vec![TimelineReadSegment {
                branch_id: "main".into(),
                from: 1,
                through: head,
                stored_through: head,
                tail: vec![],
            }],
        };
        let page = QueryPage {
            limit: NonZeroU64::MIN,
            after: None,
        };
        let mut queries = Vec::new();
        let base = |conditions| QuerySpec {
            filter: Some(QueryFilter::And { conditions }),
            select: QuerySelect::Fields {
                fields: vec!["position.sequence".into()],
            },
            group_by: vec![],
            pick: Some(QueryPick::Last),
            order_by: vec![],
        };
        queries.push((
            "timeline_events_turns",
            base(vec![
                equal("kind", json!("turn/start")),
                equal("data.turn_id", json!("0")),
            ]),
            1,
        ));
        queries.push((
            "timeline_events_scopes",
            base(vec![
                equal("kind", json!("context/scope")),
                equal("data.scope.kind", json!("task")),
                equal("data.scope.timeline", json!("main")),
                equal("data.scope.id", json!("task-0")),
                equal("data.status", json!("ended")),
            ]),
            5,
        ));
        let mut numeric = base(vec![
            equal("kind", json!("context-usage/recovered")),
            equal("data.epoch", json!("epoch")),
            equal("data.current.at.branch", json!(0)),
            QueryFilter::Compare {
                left: QueryOperand::Field {
                    path: "data.current.at.sequence".into(),
                },
                comparison: QueryComparison::Lt,
                right: QueryOperand::Value { value: json!(4) },
            },
        ]);
        numeric.pick = None;
        numeric.order_by = vec![
            QueryOrder {
                key: QueryOrderKey::Field {
                    path: "data.current.at.sequence".into(),
                },
                direction: TimelineQueryDirection::Descending,
            },
            QueryOrder {
                key: QueryOrderKey::Position,
                direction: TimelineQueryDirection::Descending,
            },
        ];
        queries.push(("timeline_events_source_positions", numeric, 4));
        let mut total = 0;
        for (index, query, expected) in queries {
            store.record_query_statistics();
            let QueryOutput::Rows { rows } = store
                .query(&scope, &query, Some(&page))
                .await
                .unwrap()
                .result
            else {
                panic!("state fields")
            };
            assert_eq!(rows.len(), 1);
            assert_eq!(
                rows[0].values,
                vec![QueryValue::Value {
                    value: json!(expected)
                }]
            );
            let statistics = store.take_query_statistics();
            let equality_keys = match index {
                "timeline_events_scopes" => 4,
                "timeline_events_source_positions" => 2,
                _ => 1,
            };
            assert!(
                statistics
                    .iter()
                    .flat_map(|stat| &stat.plan)
                    .any(|plan| plan.contains(index)
                        && plan.matches("<expr>=?").count() >= equality_keys),
                "{index}: {statistics:#?}"
            );
            if index == "timeline_events_source_positions" {
                assert!(
                    statistics
                        .iter()
                        .flat_map(|stat| &stat.plan)
                        .any(|plan| plan.contains(index) && plan.contains("<expr><?")),
                    "the numeric predecessor must bound its source sequence: {statistics:#?}"
                );
            }
            total += statistics.iter().map(|stat| stat.vm_steps).sum::<i32>();
        }
        println!("state keys, {rounds} completed turns: {total} VM steps");
        costs.push(total);
        store.close().await.unwrap();
        db.close().await.unwrap();
    }
    assert!(
        costs[1] < costs[0] * 2,
        "fixed control and boundary reads cannot enumerate completed turns: {costs:?}"
    );
}

fn unfinished(origin: &str, keys: &[(&str, &str)], terminals: &[&str]) -> QueryFilter {
    QueryFilter::And {
        conditions: vec![
            equal("kind", json!(origin)),
            QueryFilter::Not {
                condition: Box::new(QueryFilter::Exists {
                    condition: Box::new(QueryFilter::And {
                        conditions: std::iter::once(QueryFilter::In {
                            value: QueryOperand::Field {
                                path: "kind".into(),
                            },
                            values: terminals.iter().map(|kind| json!(kind)).collect(),
                        })
                        .chain(keys.iter().map(|(inner, outer)| QueryFilter::Compare {
                            left: QueryOperand::Field {
                                path: (*inner).into(),
                            },
                            comparison: QueryComparison::Eq,
                            right: QueryOperand::Outer {
                                path: (*outer).into(),
                            },
                        }))
                        .collect(),
                    }),
                }),
            },
        ],
    }
}

fn preparation(turn: &str, request: &str) -> EventKind {
    EventKind::ContextPrepare(ContextPrepareData {
        turn_id: turn.into(),
        request_id: request.into(),
        through: 0,
        request: Arc::new(ModelRequest::new("fixture", "fixture", vec![])),
        configuration: None,
        prompt: None,
        recovery: None,
        failure_chain: None,
    })
}

fn call(turn: &str, request: &str) -> EventKind {
    EventKind::ModelCall(ModelCallData {
        turn_id: turn.into(),
        request_id: request.into(),
        request: Arc::new(ModelRequest::new("fixture", "fixture", vec![])),
        configuration: None,
        prompt: None,
        failure_chain: None,
    })
}

fn output_end(turn: &str, request: &str) -> EventKind {
    EventKind::ModelOutputEnd(ModelOutputEndData {
        turn_id: turn.into(),
        request_id: request.into(),
        outcome: ModelOutputOutcome::Completed {
            finish_reason: FinishReason::EndTurn,
        },
    })
}

fn field_rows(filter: QueryFilter) -> QuerySpec {
    QuerySpec {
        filter: Some(filter),
        select: QuerySelect::Fields {
            fields: vec!["id".into(), "kind".into()],
        },
        group_by: vec![],
        pick: None,
        order_by: vec![],
    }
}

fn current_scope(
    session: &str,
    through: u64,
    stored: u64,
    tail: Vec<Arc<AgentEvent>>,
) -> TimelineReadScope {
    TimelineReadScope {
        key: key(session, "main"),
        through,
        segments: vec![TimelineReadSegment {
            branch_id: "main".into(),
            from: 1,
            through,
            stored_through: stored,
            tail,
        }],
    }
}

#[tokio::test]
async fn unfinished_operations_match_memory_with_composite_keys_residuals_and_publishing_tail() {
    let mut costs = Vec::new();
    for rounds in [3, 30] {
        let database = Database::new();
        let db = SqliteStorage::open(database.0.clone()).unwrap();
        let store = SqliteTimelineStore::new(db.clone());
        let memory = MemoryTimelineStore::default();
        for target in [
            store.as_ref() as &dyn TimelineStore,
            &memory as &dyn TimelineStore,
        ] {
            target
                .create_session("operations", &SessionConfig::default())
                .await
                .unwrap();
        }
        let mut records = Vec::new();
        let mut push = |kind| {
            let sequence = records.len() as u64 + 1;
            records.push(record(sequence, kind));
            sequence
        };
        for n in 0..rounds {
            let turn = format!("turn-{n}");
            let request = format!("request-{n}");
            push(EventKind::ModelStart(ModelStartData {
                turn_id: turn.clone(),
                request_id: request.clone(),
            }));
            push(preparation(&turn, &request));
            push(call(&turn, &request));
            push(output_end(&turn, &request));
            let command = push(EventKind::ContextCompact(ContextCompactData {}));
            let start = push(EventKind::ContextCompactionStart(
                ContextCompactionStartData {
                    turn_id: None,
                    request_id: None,
                    command: Some(command),
                    through: command,
                    trigger: CompactionTrigger::Manual,
                    strategy: "fixture".into(),
                },
            ));
            push(EventKind::ContextCompaction(ContextCompactionData {
                turn_id: None,
                request_id: None,
                command: Some(command),
                start: Some(start),
                through: command,
                outcome: ContextCompactionResult::Cancelled {
                    reason: "fixture settled".into(),
                },
            }));
        }
        let pending = push(preparation("new-turn", "shared-request"));
        push(call("old-turn", "shared-request"));
        push(output_end("old-turn", "shared-request"));
        push(preparation("closing-in-tail", "tail-request"));
        let command = push(EventKind::ContextCompact(ContextCompactData {}));
        let head = records.len() as u64;
        append(&store, key("operations", "main"), records.clone())
            .await
            .unwrap();
        memory
            .append(
                key("operations", "main"),
                records,
                Box::new(|result| result.unwrap()),
            )
            .unwrap();
        let tail = vec![
            record(head + 1, call("closing-in-tail", "tail-request")).event,
            record(head + 2, output_end("terminal-first", "tail-closed")).event,
            record(head + 3, preparation("terminal-first", "tail-closed")).event,
            record(head + 4, preparation("alive-in-tail", "tail-alive")).event,
        ];
        let prepare = unfinished(
            "context/prepare",
            &[
                ("data.turn_id", "data.turn_id"),
                ("data.request_id", "data.request_id"),
            ],
            &["model/call", "step/model-output-end"],
        );
        let command_filter = QueryFilter::And {
            conditions: vec![
                equal("kind", json!("context/compact")),
                QueryFilter::Not {
                    condition: Box::new(QueryFilter::Exists {
                        condition: Box::new(QueryFilter::Or {
                            conditions: ["context/compaction-start", "context/compaction"]
                                .iter()
                                .map(|kind| QueryFilter::And {
                                    conditions: vec![
                                        equal("kind", json!(kind)),
                                        QueryFilter::Compare {
                                            left: QueryOperand::Field {
                                                path: "data.command".into(),
                                            },
                                            comparison: QueryComparison::Eq,
                                            right: QueryOperand::Outer {
                                                path: "position.sequence".into(),
                                            },
                                        },
                                    ],
                                })
                                .collect(),
                        }),
                    }),
                },
            ],
        };
        let scope = current_scope("operations", head + 4, head, tail);
        let mut queries = vec![
            field_rows(unfinished(
                "model/call",
                &[
                    ("data.turn_id", "data.turn_id"),
                    ("data.request_id", "data.request_id"),
                ],
                &["step/model-output-end"],
            )),
            field_rows(unfinished(
                "step/model-start",
                &[("data.request_id", "data.request_id")],
                &["step/model-output-end"],
            )),
            field_rows(unfinished(
                "context/compaction-start",
                &[("data.start", "position.sequence")],
                &["context/compaction"],
            )),
            field_rows(prepare.clone()),
            field_rows(command_filter.clone()),
            field_rows(QueryFilter::Or {
                conditions: vec![prepare.clone(), command_filter],
            }),
        ];
        let mut residual = field_rows(QueryFilter::And {
            conditions: vec![prepare, equal("data.request_id", json!("shared-request"))],
        });
        residual.pick = Some(QueryPick::Last);
        queries.push(residual);
        let page = QueryPage {
            limit: NonZeroU64::new(16).unwrap(),
            after: None,
        };
        let mut work = 0;
        for query in queries {
            store.record_query_statistics();
            let actual = store.query(&scope, &query, Some(&page)).await.unwrap();
            let expected = memory.query(&scope, &query, Some(&page)).await.unwrap();
            assert_eq!(
                serde_json::to_value(&actual).unwrap(),
                serde_json::to_value(&expected).unwrap(),
                "{query:?}"
            );
            let statistics = store.take_query_statistics();
            assert!(
                statistics.iter().any(|stat| stat.stage == "member_root"),
                "unfinished operations must use member roots: {statistics:#?}"
            );
            work += statistics.iter().map(|stat| stat.vm_steps).sum::<i32>();
        }
        let QueryOutput::Rows { rows } = store
            .query(
                &scope,
                &field_rows(unfinished(
                    "context/prepare",
                    &[
                        ("data.turn_id", "data.turn_id"),
                        ("data.request_id", "data.request_id"),
                    ],
                    &["model/call", "step/model-output-end"],
                )),
                Some(&page),
            )
            .await
            .unwrap()
            .result
        else {
            panic!("pending fields")
        };
        assert_eq!(
            rows.iter()
                .map(|row| row.values[0].clone())
                .collect::<Vec<_>>(),
            [pending, head + 4].map(|at| QueryValue::Value {
                value: json!(format!("event-{at}"))
            })
        );
        assert!(command > pending);
        println!("unfinished queries, {rounds} completed turns: {work} VM steps");
        costs.push(work);
        store.close().await.unwrap();
        db.close().await.unwrap();
    }
    assert!(
        costs[1] < costs[0] * 2,
        "completed operations must not grow current-member work: {costs:?}"
    );
}

#[tokio::test]
async fn merged_output_ids_seek_their_locator_inside_a_physical_row() {
    let mut costs = Vec::new();
    for fragments in [8, 80] {
        let database = Database::new();
        let db = SqliteStorage::open(database.0.clone()).unwrap();
        let store = SqliteTimelineStore::new(db.clone());
        store
            .create_session("output-ids", &SessionConfig::default())
            .await
            .unwrap();
        let records = (1..=fragments)
            .map(|sequence| text(sequence, "content"))
            .collect();
        append(&store, key("output-ids", "main"), records)
            .await
            .unwrap();
        let query = field_rows(QueryFilter::In {
            value: QueryOperand::Field { path: "id".into() },
            values: vec![json!(format!("event-{fragments}")), json!("absent-id")],
        });
        let page = QueryPage {
            limit: NonZeroU64::new(2).unwrap(),
            after: None,
        };
        let mut scope = current_scope("output-ids", fragments, fragments, vec![]);
        scope.segments[0].from = fragments;
        store.record_query_statistics();
        let actual = store.query(&scope, &query, Some(&page)).await.unwrap();
        let QueryOutput::Rows { rows } = actual.result else {
            panic!("output identities")
        };
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].values[0],
            QueryValue::Value {
                value: json!(format!("event-{fragments}"))
            }
        );
        let statistics = store.take_query_statistics();
        assert!(
            statistics
                .iter()
                .flat_map(|stat| &stat.plan)
                .any(|line| line.contains("timeline_output_ids") && line.contains("SEARCH")),
            "{statistics:#?}"
        );
        let work = statistics.iter().map(|stat| stat.vm_steps).sum::<i32>();
        println!("output ID, {fragments} fragments: {work} VM steps");
        costs.push(work);
        store.close().await.unwrap();
        db.close().await.unwrap();
    }
    assert!(
        costs[1] < costs[0] * 2,
        "ID lookup must not enumerate all fragments: {costs:?}"
    );
}

#[tokio::test]
async fn v40_output_identity_upgrade_is_atomic_and_directory_reads_do_not_audit_locators() {
    for invalid_identity in [false, true] {
        let database = Database::new();
        let db = SqliteStorage::open(database.0.clone()).unwrap();
        let store = SqliteTimelineStore::new(db.clone());
        store
            .create_session("upgrade-output", &SessionConfig::default())
            .await
            .unwrap();
        append(
            &store,
            key("upgrade-output", "main"),
            vec![
                text(1, "first"),
                record(
                    2,
                    EventKind::TurnStart(TurnStartData {
                        turn_id: "between outputs".into(),
                    }),
                ),
                text(3, "second"),
            ],
        )
        .await
        .unwrap();
        store.close().await.unwrap();
        db.close().await.unwrap();
        drop(store);
        drop(db);
        let connection = Connection::open(&database.0).unwrap();
        drop_v40(&connection);
        for table in [
            "timeline_member_roots",
            "timeline_member_origins",
            "timeline_member_terminals",
        ] {
            connection.execute(&format!("DELETE FROM {table} WHERE index_id NOT IN ('input/v1','tool/v1','exec/v1','collaboration/v1')"),[]).unwrap();
        }
        without_automation_origin(&connection);
        connection.execute_batch("DROP TABLE agent_store_format;CREATE TABLE agent_store_format (version INTEGER NOT NULL PRIMARY KEY CHECK (version = 39));INSERT INTO agent_store_format VALUES(39)").unwrap();
        let body: String = connection
            .query_row(
                "SELECT body FROM timeline_events WHERE sequence=1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let value: serde_json::Value = serde_json::from_str(&body).unwrap();
        let mut wire = value["records"][0].clone();
        wire["event"]["data"]["turn_id"] = value["turn_id"].clone();
        wire["event"]["data"]["request_id"] = value["request_id"].clone();
        connection
            .execute(
                "UPDATE timeline_events SET body=?1 WHERE sequence=1",
                [json!({"encoding":"event","record":wire}).to_string()],
            )
            .unwrap();
        let original: String = connection
            .query_row(
                "SELECT body FROM timeline_events WHERE sequence=1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        if invalid_identity {
            let mut damaged: serde_json::Value = serde_json::from_str(&original).unwrap();
            damaged["record"]["event"]["id"] = json!("");
            connection
                .execute(
                    "UPDATE timeline_events SET body=?1 WHERE sequence=1",
                    [damaged.to_string()],
                )
                .unwrap();
        }
        drop(connection);
        if invalid_identity {
            assert!(SqliteStorage::open(database.0.clone()).is_err());
            let connection = Connection::open(&database.0).unwrap();
            assert_eq!(
                connection
                    .query_row("SELECT version FROM agent_store_format", [], |row| row
                        .get::<_, u32>(0))
                    .unwrap(),
                39
            );
            assert!(
                !connection
                    .prepare("SELECT 1 FROM sqlite_master WHERE name='timeline_output_ids'")
                    .unwrap()
                    .exists([])
                    .unwrap(),
                "failed upgrade leaves no locator table"
            );
            connection
                .execute(
                    "UPDATE timeline_events SET body=?1 WHERE sequence=1",
                    [original],
                )
                .unwrap();
        }
        let db = SqliteStorage::open(database.0.clone()).unwrap();
        let store = SqliteTimelineStore::new(db.clone());
        let query = field_rows(equal("id", json!("event-1")));
        let QueryOutput::Rows { rows } = store
            .query(
                &current_scope("upgrade-output", 3, 3, vec![]),
                &query,
                Some(&QueryPage {
                    limit: NonZeroU64::MIN,
                    after: None,
                }),
            )
            .await
            .unwrap()
            .result
        else {
            panic!("indexed output")
        };
        assert_eq!(rows.len(), 1);
        store.close().await.unwrap();
        db.close().await.unwrap();
        drop(store);
        drop(db);
        let connection = Connection::open(&database.0).unwrap();
        connection
            .execute(
                "DELETE FROM timeline_output_ids WHERE event_id='event-3'",
                [],
            )
            .unwrap();
        drop(connection);
        let db = SqliteStorage::open(database.0.clone()).unwrap();
        let store = SqliteTimelineStore::new(db.clone());
        store.branches("upgrade-output").await.unwrap();
        let mut exported = Vec::new();
        dim_app_server::storage::export_timeline(
            database.0.clone(),
            "upgrade-output",
            &mut exported,
        )
        .await
        .unwrap();
        assert!(
            String::from_utf8(exported).unwrap().contains("event-3"),
            "export reads source facts rather than auditing locators"
        );
        store.close().await.unwrap();
        db.close().await.unwrap();
    }
}

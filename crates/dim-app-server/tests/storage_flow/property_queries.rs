use super::*;
use dim_agent_runtime::{
    QueryComparison, QueryFilter, QueryOperand, QueryOrder, QueryOrderKey, QueryPage, QueryPick,
    QuerySelect, QuerySpec, TimelineQueryDirection, TimelineReadScope, TimelineReadSegment,
};
use serde_json::{Value, json};
use std::num::NonZeroU64;

#[tokio::test]
async fn recovery_relations_bound_all_physical_roles_before_projection() {
    let mut costs = Vec::new();
    for unrelated in [100, 5_000] {
        let database = Database::new();
        let db = SqliteStorage::open(database.0.clone()).unwrap();
        let store = SqliteTimelineStore::new(db.clone());
        let memory = MemoryTimelineStore::default();
        for storage in [&*store as &dyn TimelineStore, &memory] {
            storage
                .create_session("properties", &SessionConfig::default())
                .await
                .unwrap();
        }
        let mut records: Vec<_> = (1..=unrelated)
            .map(|sequence| {
                record(
                    sequence,
                    EventKind::Extension(
                        ExtensionEvent::new("other", "saved", json!({"payload":"x".repeat(1024)}))
                            .unwrap(),
                    ),
                )
            })
            .collect();
        for (offset, (kind, turn, request)) in [
            ("start", "turn-a", "request-a"),
            ("start", "turn-a", "request-b"),
            ("start", "turn-b", "request-a"),
            ("end", "turn-a", "request-a"),
            ("failed", "turn-a", "request-b"),
        ]
        .into_iter()
        .enumerate()
        {
            records.push(record(
                unrelated + offset as u64 + 1,
                EventKind::Extension(
                    ExtensionEvent::new("work", kind, json!({"turn_id":turn,"request_id":request}))
                        .unwrap(),
                ),
            ));
        }
        append(&store, key("properties", "main"), records.clone())
            .await
            .unwrap();
        memory
            .append(
                key("properties", "main"),
                records,
                Box::new(|result| result.unwrap()),
            )
            .unwrap();
        let mut query = fields(&["data.turn_id", "data.request_id"]);
        let terminal = QueryFilter::And {
            conditions: vec![
                QueryFilter::In {
                    value: QueryOperand::Field {
                        path: "kind".into(),
                    },
                    values: vec![json!("work/end"), json!("work/failed")],
                },
                QueryFilter::Compare {
                    left: QueryOperand::Field {
                        path: "data.turn_id".into(),
                    },
                    comparison: QueryComparison::Eq,
                    right: QueryOperand::Outer {
                        path: "data.turn_id".into(),
                    },
                },
                QueryFilter::Compare {
                    left: QueryOperand::Field {
                        path: "data.request_id".into(),
                    },
                    comparison: QueryComparison::Eq,
                    right: QueryOperand::Outer {
                        path: "data.request_id".into(),
                    },
                },
            ],
        };
        query.filter = Some(QueryFilter::And {
            conditions: vec![
                eq("kind", json!("work/start")),
                QueryFilter::Not {
                    condition: Box::new(QueryFilter::Exists {
                        condition: Box::new(terminal),
                    }),
                },
            ],
        });
        let range = scope(unrelated + 5);
        let mut measured = Vec::new();
        for direction in [
            TimelineQueryDirection::Ascending,
            TimelineQueryDirection::Descending,
        ] {
            query.order_by = vec![QueryOrder {
                key: QueryOrderKey::Position,
                direction,
            }];
            store.record_query_statistics();
            parity(&store, &memory, &range, &query).await;
            let stats = store.take_query_statistics();
            assert!(
                stats
                    .iter()
                    .flat_map(|stat| &stat.plan)
                    .any(|line| line.contains("timeline_events_by_kind")),
                "all recovery roles should use bounded kinds: {stats:?}"
            );
            measured.push(
                stats
                    .iter()
                    .map(|stat| i64::from(stat.vm_steps))
                    .sum::<i64>(),
            );
        }
        costs.push(measured);
        store.close().await.unwrap();
        db.close().await.unwrap();
    }
    assert!(
        costs[1]
            .iter()
            .zip(&costs[0])
            .all(|(large, small)| large < &(small * 2)),
        "50x unrelated history must not enter the recovery role set: {costs:?}"
    );
    println!("recovery physical role VM steps: {costs:?}");
}

#[tokio::test]
async fn mixed_kind_identity_queries_seek_the_association_index() {
    let mut costs = Vec::new();
    for size in [100u64, 5_000] {
        let database = Database::new();
        let db = SqliteStorage::open(database.0.clone()).unwrap();
        let store = SqliteTimelineStore::new(db.clone());
        store
            .create_session("properties", &SessionConfig::default())
            .await
            .unwrap();
        let records = (0..size)
            .flat_map(|n| {
                ["a", "b"]
                    .into_iter()
                    .enumerate()
                    .map(move |(offset, kind)| {
                        record(
                            n * 2 + offset as u64 + 1,
                            EventKind::Extension(
                                ExtensionEvent::new(
                                    "owner",
                                    kind,
                                    json!({"request_id":format!("request-{n}"),
                                    "input_id":format!("input-{n}")}),
                                )
                                .unwrap(),
                            ),
                        )
                    })
            })
            .collect();
        append(&store, key("properties", "main"), records)
            .await
            .unwrap();
        let mut measured = Vec::new();
        for (field, index, prefix) in [
            ("data.request_id", "timeline_events_requests", "request"),
            ("data.input_id", "timeline_events_inputs", "input"),
        ] {
            for present in [true, false] {
                let mut query = fields(&["position.sequence"]);
                query.filter = Some(QueryFilter::And {
                    conditions: vec![
                        QueryFilter::In {
                            value: QueryOperand::Field {
                                path: "kind".into(),
                            },
                            values: vec![json!("owner/a"), json!("owner/b")],
                        },
                        QueryFilter::In {
                            value: QueryOperand::Field { path: field.into() },
                            values: vec![json!(format!(
                                "{prefix}-{}",
                                if present { 0 } else { size }
                            ))],
                        },
                    ],
                });
                for pick in [None, Some(QueryPick::First), Some(QueryPick::Last)] {
                    query.pick = pick;
                    store.record_query_statistics();
                    let reply = store
                        .query(&scope(size * 2), &query, Some(&page(3)))
                        .await
                        .unwrap();
                    let dim_agent_runtime::QueryOutput::Rows { rows } = reply.result else {
                        panic!("selected identity positions")
                    };
                    assert_eq!(
                        rows.len(),
                        if present {
                            if pick.is_some() { 1 } else { 2 }
                        } else {
                            0
                        }
                    );
                    let stats = store.take_query_statistics();
                    assert!(
                        stats
                            .iter()
                            .flat_map(|stat| &stat.plan)
                            .any(|line| line.contains(index) && line.contains("<expr>=?")),
                        "identity constraints must select the association, not just kind: {stats:?}"
                    );
                    measured.push(
                        stats
                            .iter()
                            .map(|stat| i64::from(stat.vm_steps))
                            .sum::<i64>(),
                    );
                }
            }
        }
        costs.push(measured);
        store.close().await.unwrap();
        db.close().await.unwrap();
    }
    for (small, large) in costs[0].iter().zip(&costs[1]) {
        assert!(
            *large < *small * 2,
            "50x unrelated identity history must not be scanned: {costs:?}"
        );
    }
    println!("mixed-kind association VM steps: {costs:?}");
}

#[tokio::test]
async fn mixed_kind_pages_seek_each_type_before_merging() {
    let mut costs = Vec::new();
    for unrelated in [100, 5_000] {
        let database = Database::new();
        let db = SqliteStorage::open(database.0.clone()).unwrap();
        let store = SqliteTimelineStore::new(db.clone());
        store
            .create_session("properties", &SessionConfig::default())
            .await
            .unwrap();
        let mut records = (1..=16)
            .map(|sequence| {
                record(
                    sequence,
                    EventKind::Extension(
                        ExtensionEvent::new(
                            "owner",
                            if sequence % 2 == 0 { "a" } else { "b" },
                            json!({}),
                        )
                        .unwrap(),
                    ),
                )
            })
            .collect::<Vec<_>>();
        records.extend((17..=unrelated + 16).map(|sequence| {
            record(
                sequence,
                EventKind::InputMessage(InputMessageData::text("unrelated history", None)),
            )
        }));
        append(&store, key("properties", "main"), records)
            .await
            .unwrap();
        let mut query = fields(&["position.sequence"]);
        query.filter = Some(QueryFilter::In {
            value: QueryOperand::Field {
                path: "kind".into(),
            },
            values: vec![json!("owner/a"), json!("owner/b"), json!("owner/a")],
        });
        let mut measured = Vec::new();
        for direction in [
            TimelineQueryDirection::Ascending,
            TimelineQueryDirection::Descending,
        ] {
            query.order_by = vec![QueryOrder {
                key: QueryOrderKey::Position,
                direction,
            }];
            for pick in [None, Some(QueryPick::First), Some(QueryPick::Last)] {
                query.pick = pick;
                let mut p = page(3);
                let mut found = Vec::new();
                let mut steps = 0i64;
                loop {
                    store.record_query_statistics();
                    let reply = store
                        .query(&scope(unrelated + 16), &query, Some(&p))
                        .await
                        .unwrap();
                    steps += store
                        .take_query_statistics()
                        .iter()
                        .map(|stat| i64::from(stat.vm_steps))
                        .sum::<i64>();
                    let dim_agent_runtime::QueryOutput::Rows { rows } = reply.result else {
                        panic!("selected positions");
                    };
                    for row in rows {
                        let dim_agent_runtime::QueryValue::Value { value } = &row.values[0] else {
                            panic!("position value");
                        };
                        found.push(value.as_u64().unwrap());
                    }
                    let Some(next) = reply.next else { break };
                    p.after = Some(next);
                }
                let mut expected: Vec<_> = match pick {
                    Some(QueryPick::First) => vec![1],
                    Some(QueryPick::Last) => vec![16],
                    None => (1..=16).collect(),
                };
                if pick.is_none() && direction == TimelineQueryDirection::Descending {
                    expected.reverse();
                }
                assert_eq!(found, expected);
                measured.push(steps);
            }
        }
        costs.push(measured);
        store.close().await.unwrap();
        db.close().await.unwrap();
    }
    for (small, large) in costs[0].iter().zip(&costs[1]) {
        assert!(
            *large < *small * 2,
            "unrelated history must not be scanned: {costs:?}"
        );
    }
}

#[tokio::test]
async fn v37_text_upgrade_removes_opaque_documents_without_losing_query_candidates() {
    let database = Database::new();
    let db = SqliteStorage::open(database.0.clone()).unwrap();
    let store = SqliteTimelineStore::new(db.clone());
    store
        .create_session("properties", &SessionConfig::default())
        .await
        .unwrap();
    append(
        &store,
        key("properties", "main"),
        vec![
            record(
                1,
                EventKind::InputMessage(InputMessageData::text("coreword", None)),
            ),
            record(
                2,
                EventKind::Extension(
                    ExtensionEvent::new("private", "state", json!({"text":"opaqueword"})).unwrap(),
                ),
            ),
            text(3, "streamword"),
            text(4, "suffix"),
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
    without_automation_origin(&connection);
    connection.execute_batch("DROP TABLE timeline_array_elements; DROP TABLE agent_store_format; CREATE TABLE agent_store_format (version INTEGER NOT NULL PRIMARY KEY CHECK (version = 37)); INSERT INTO agent_store_format VALUES(37);").unwrap();
    for sql in V36_ADDITIONS
        .iter()
        .filter(|sql| sql.starts_with("CREATE INDEX "))
    {
        let name = sql.split_whitespace().nth(2).unwrap();
        connection
            .execute_batch(&format!("DROP INDEX {name}; {sql}"))
            .unwrap();
    }
    let opaque: i64 = connection.query_row("INSERT INTO timeline_text(session_id,branch_id,sequence,physical_sequence,property,partition_key) VALUES('properties','main',2,2,'data','event:2') RETURNING id", [], |row| row.get(0)).unwrap();
    connection
        .execute(
            "INSERT INTO timeline_text_fts(rowid,text) VALUES(?1,'opaqueword')",
            [opaque],
        )
        .unwrap();
    assert_eq!(schema(&connection), schema_of(37));
    let facts = rows(&connection, "timeline_events");
    let tails = rows(&connection, "timeline_text_tails");
    let documents = rows(&connection, "timeline_text");
    let docsize_schema: String = connection
        .query_row(
            "SELECT sql FROM sqlite_schema WHERE name='timeline_text_fts_docsize'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let docsizes = connection
        .prepare("SELECT id,sz FROM timeline_text_fts_docsize")
        .unwrap()
        .query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?))
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    connection
        .execute_batch("DROP TABLE timeline_text_fts_docsize")
        .unwrap();
    drop(connection);

    let error = match SqliteStorage::open(database.0.clone()) {
        Ok(_) => panic!("the text cleanup must fail when its required FTS table is missing"),
        Err(error) => error,
    };
    assert!(
        error.to_string().contains("timeline_text_fts_docsize"),
        "{error}"
    );
    let connection = Connection::open(&database.0).unwrap();
    assert_eq!(marker(&connection), [37]);
    assert_eq!(rows(&connection, "timeline_events"), facts);
    assert_eq!(rows(&connection, "timeline_text"), documents);
    connection.execute_batch(&docsize_schema).unwrap();
    for (id, bytes) in docsizes {
        connection
            .execute(
                "INSERT INTO timeline_text_fts_docsize(id,sz) VALUES(?1,?2)",
                params![id, bytes],
            )
            .unwrap();
    }
    drop(connection);

    let db = SqliteStorage::open(database.0.clone()).unwrap();
    assert_eq!(db.upgraded_from(), Some(37));
    let store = SqliteTimelineStore::new(db.clone());
    let connection = Connection::open(&database.0).unwrap();
    assert_eq!(marker(&connection), [i64::from(STORE_FORMAT)]);
    assert_eq!(rows(&connection, "timeline_events"), facts);
    assert_eq!(rows(&connection, "timeline_text_tails"), tails);
    assert_eq!(
        connection
            .query_row(
                "SELECT count(*) FROM timeline_text WHERE sequence=2",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT count(*) FROM timeline_text_fts WHERE rowid=?1",
                [opaque],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT count(*) FROM timeline_text WHERE property='data.delta'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        2
    );
    drop(connection);
    for (kinds, term, expected) in [
        (vec!["input/message"], "coreword", vec!["event-1"]),
        (vec!["input/message"], "absentword", vec![]),
        (vec!["private/state"], "opaqueword", vec!["event-2"]),
        (vec!["private/state"], "absentword", vec!["event-2"]),
        (
            vec!["input/message", "private/state"],
            "absentword",
            vec!["event-2"],
        ),
    ] {
        let mut query = fields(&["id"]);
        query.filter = Some(QueryFilter::And {
            conditions: vec![
                QueryFilter::In {
                    value: QueryOperand::Field {
                        path: "kind".into(),
                    },
                    values: kinds.into_iter().map(Value::from).collect(),
                },
                QueryFilter::TextCandidate {
                    field: "data".into(),
                    partition_by: vec![],
                    terms: vec![term.into()],
                },
            ],
        });
        let dim_agent_runtime::QueryOutput::Rows { rows } = store
            .query(&scope(4), &query, Some(&page(8)))
            .await
            .unwrap()
            .result
        else {
            panic!("candidate rows")
        };
        assert_eq!(
            rows.into_iter()
                .map(|row| row.values[0].clone())
                .collect::<Vec<_>>(),
            expected
                .into_iter()
                .map(|id| dim_agent_runtime::QueryValue::Value { value: json!(id) })
                .collect::<Vec<_>>(),
            "term={term}"
        );
    }
    store.close().await.unwrap();
    db.close().await.unwrap();
    drop(store);
    drop(db);
    let db = SqliteStorage::open(database.0.clone()).unwrap();
    assert_eq!(db.upgraded_from(), None);
    db.close().await.unwrap();
}

fn fields(names: &[&str]) -> QuerySpec {
    QuerySpec {
        filter: None,
        select: QuerySelect::Fields {
            fields: names.iter().map(|name| (*name).into()).collect(),
        },
        group_by: vec![],
        pick: None,
        order_by: vec![],
    }
}

fn eq(path: &str, value: Value) -> QueryFilter {
    QueryFilter::Compare {
        left: QueryOperand::Field { path: path.into() },
        comparison: QueryComparison::Eq,
        right: QueryOperand::Value { value },
    }
}

fn scope(through: u64) -> TimelineReadScope {
    TimelineReadScope {
        key: key("properties", "main"),
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

fn page(limit: u64) -> QueryPage {
    QueryPage {
        limit: NonZeroU64::new(limit).unwrap(),
        after: None,
    }
}

async fn parity(
    store: &SqliteTimelineStore,
    memory: &MemoryTimelineStore,
    scope: &TimelineReadScope,
    query: &QuerySpec,
) {
    let scalar = query.group_by.is_empty()
        && matches!(query.select, QuerySelect::Count | QuerySelect::Exists);
    let mut page = (!scalar).then(|| page(2));
    loop {
        let expected = memory.query(scope, query, page.as_ref()).await.unwrap();
        let actual = store.query(scope, query, page.as_ref()).await.unwrap();
        assert_eq!(
            serde_json::to_value(&actual).unwrap(),
            serde_json::to_value(&expected).unwrap(),
            "query: {query:?}"
        );
        let Some(after) = actual.next else {
            break;
        };
        page.as_mut().unwrap().after = Some(after);
    }
}

#[tokio::test]
async fn property_queries_match_memory_for_grouping_relations_arrays_and_keyset_pages() {
    let database = Database::new();
    let db = SqliteStorage::open(database.0.clone()).unwrap();
    let store = SqliteTimelineStore::new(db.clone());
    let memory = MemoryTimelineStore::default();
    store
        .create_session("properties", &SessionConfig::default())
        .await
        .unwrap();
    memory
        .create_session("properties", &SessionConfig::default())
        .await
        .unwrap();
    let data = [
        json!({"group":1,"number":1,"key":"a","items":[{"id":"yes","ok":false},{"id":"no","ok":true}],"empty":null,"submission_id":"1"}),
        json!({"group":1.0,"number":1.0,"key":"b","items":[{"id":"yes","ok":true},{},null],"submission_id":null}),
        json!({"group":true,"number":u64::MAX,"key":"c","allowed_tools":["tool",null,17,true,{},[]],"submission_id":true}),
        json!({"group":null,"number":u64::MAX-1,"key":"d","items":{"id":"yes","ok":true},"submission_id":1}),
        json!({"number":-1,"key":"a","done":true,"allowed_tools":"tool"}),
        json!({"number":-0.5,"key":"b","done":true}),
        json!({"number":1e-300,"key":"e","text":"中\u{0000}文"}),
        json!({"number":-9e200,"key":"f","items":[],"nested":[{"items":[{"id":"nested"}]}],"object":{"0":"wrong","":["wrong"]}}),
    ];
    let records: Vec<_> = data
        .into_iter()
        .enumerate()
        .map(|(i, data)| {
            record(
                i as u64 + 1,
                EventKind::Extension(ExtensionEvent::new("property", "value", data).unwrap()),
            )
        })
        .collect();
    append(&store, key("properties", "main"), records.clone())
        .await
        .unwrap();
    memory
        .append(
            key("properties", "main"),
            records,
            Box::new(|result| result.unwrap()),
        )
        .unwrap();
    let scope = scope(8);
    let mut query = fields(&[
        "position",
        "id",
        "kind",
        "tags",
        "context.render",
        "data.empty",
        "data.absent",
        "data.items[].id",
        "data.items[0].id",
        "data.items[1].id",
        "data.items[2]",
        "data.items[2].id",
        "data.items[2147483647]",
        "data.nested[0].items[0].id",
        "data.nested[].items[0].id",
        "data.nested[0].items[].id",
        "data.object[0]",
    ]);
    parity(&store, &memory, &scope, &query).await;
    for filter in [
        eq("data.items[0].id", json!("yes")),
        eq("data.items[2]", Value::Null),
        QueryFilter::Present {
            value: QueryOperand::Field {
                path: "data.items[2]".into(),
            },
        },
        QueryFilter::Present {
            value: QueryOperand::Field {
                path: "data.items[2].id".into(),
            },
        },
        eq("data.submission_id", json!("1")),
        QueryFilter::Not {
            condition: Box::new(eq("data.submission_id", json!("1"))),
        },
        QueryFilter::Prefix {
            value: QueryOperand::Field {
                path: "data.text".into(),
            },
            prefix: "中\0文".into(),
        },
        QueryFilter::Prefix {
            value: QueryOperand::Field {
                path: "data.text".into(),
            },
            prefix: "中\0wrong".into(),
        },
        QueryFilter::Contains {
            value: QueryOperand::Field {
                path: "data.text".into(),
            },
            text: "\0文".into(),
        },
    ] {
        query.filter = Some(filter);
        parity(&store, &memory, &scope, &query).await;
    }
    query = fields(&["position.sequence", "data.number"]);
    for direction in [
        TimelineQueryDirection::Ascending,
        TimelineQueryDirection::Descending,
    ] {
        query.order_by = vec![QueryOrder {
            key: QueryOrderKey::Field {
                path: "data.number".into(),
            },
            direction,
        }];
        parity(&store, &memory, &scope, &query).await;
    }
    query.order_by.clear();
    query.filter = Some(QueryFilter::Any {
        field: "data.items".into(),
        condition: Box::new(QueryFilter::And {
            conditions: vec![
                QueryFilter::Compare {
                    left: QueryOperand::Element { path: "id".into() },
                    comparison: QueryComparison::Eq,
                    right: QueryOperand::Value {
                        value: json!("yes"),
                    },
                },
                QueryFilter::Compare {
                    left: QueryOperand::Element { path: "ok".into() },
                    comparison: QueryComparison::Eq,
                    right: QueryOperand::Value { value: json!(true) },
                },
            ],
        }),
    });
    parity(&store, &memory, &scope, &query).await;
    query.filter = Some(QueryFilter::Not {
        condition: Box::new(query.filter.take().unwrap()),
    });
    parity(&store, &memory, &scope, &query).await;
    for value in [json!("tool"), json!(null), json!(17), json!(true)] {
        for condition in [
            QueryFilter::Compare {
                left: QueryOperand::Element {
                    path: String::new(),
                },
                comparison: QueryComparison::Eq,
                right: QueryOperand::Value {
                    value: value.clone(),
                },
            },
            QueryFilter::In {
                value: QueryOperand::Element {
                    path: String::new(),
                },
                values: vec![value],
            },
        ] {
            query.filter = Some(QueryFilter::Any {
                field: "data.allowed_tools".into(),
                condition: Box::new(condition),
            });
            parity(&store, &memory, &scope, &query).await;
        }
    }
    query.filter = Some(QueryFilter::Not {
        condition: Box::new(QueryFilter::Exists {
            condition: Box::new(QueryFilter::And {
                conditions: vec![
                    eq("data.done", json!(true)),
                    QueryFilter::Compare {
                        left: QueryOperand::Field {
                            path: "data.key".into(),
                        },
                        comparison: QueryComparison::Eq,
                        right: QueryOperand::Outer {
                            path: "data.key".into(),
                        },
                    },
                ],
            }),
        }),
    });
    parity(&store, &memory, &scope, &query).await;
    let relation = query.filter.clone().unwrap();
    for local in [
        eq("data.done", json!(true)),
        QueryFilter::Contains {
            value: QueryOperand::Field {
                path: "data.text".into(),
            },
            text: "文".into(),
        },
        QueryFilter::Any {
            field: "data.items".into(),
            condition: Box::new(QueryFilter::Compare {
                left: QueryOperand::Element { path: "id".into() },
                comparison: QueryComparison::Eq,
                right: QueryOperand::Value {
                    value: json!("yes"),
                },
            }),
        },
    ] {
        for filter in [
            QueryFilter::And {
                conditions: vec![local.clone(), relation.clone()],
            },
            QueryFilter::Or {
                conditions: vec![local.clone(), relation.clone()],
            },
            QueryFilter::Not {
                condition: Box::new(QueryFilter::Or {
                    conditions: vec![local, relation.clone()],
                }),
            },
        ] {
            query.filter = Some(filter);
            parity(&store, &memory, &scope, &query).await;
            query.order_by = vec![QueryOrder {
                key: QueryOrderKey::Field {
                    path: "data.done".into(),
                },
                direction: TimelineQueryDirection::Descending,
            }];
            parity(&store, &memory, &scope, &query).await;
            query.order_by.clear();
            query.group_by = vec!["data.done".into()];
            query.pick = Some(QueryPick::Last);
            parity(&store, &memory, &scope, &query).await;
            query.pick = None;
            query.group_by.clear();
        }
    }
    query.filter = Some(QueryFilter::Exists {
        condition: Box::new(QueryFilter::Any {
            field: "data.items".into(),
            condition: Box::new(QueryFilter::Compare {
                left: QueryOperand::Element { path: "id".into() },
                comparison: QueryComparison::Ne,
                right: QueryOperand::Outer {
                    path: "data.key".into(),
                },
            }),
        }),
    });
    parity(&store, &memory, &scope, &query).await;
    query.filter = Some(relation);
    query.select = QuerySelect::Count;
    parity(&store, &memory, &scope, &query).await;
    query.select = QuerySelect::Exists;
    parity(&store, &memory, &scope, &query).await;
    query.filter = None;
    query.select = QuerySelect::Count;
    query.group_by = vec!["data.group".into()];
    parity(&store, &memory, &scope, &query).await;
    query.order_by = vec![QueryOrder {
        key: QueryOrderKey::Count,
        direction: TimelineQueryDirection::Descending,
    }];
    parity(&store, &memory, &scope, &query).await;
    query.select = QuerySelect::Fields {
        fields: vec!["data.number".into(), "position.sequence".into()],
    };
    query.order_by.clear();
    for pick in [QueryPick::First, QueryPick::Last] {
        query.pick = Some(pick);
        parity(&store, &memory, &scope, &query).await;
    }
    query.group_by.clear();
    parity(&store, &memory, &scope, &query).await;
    query.pick = None;
    query.select = QuerySelect::Events;
    parity(&store, &memory, &scope, &query).await;
    store.close().await.unwrap();
    db.close().await.unwrap();
}

#[tokio::test]
async fn property_queries_clip_merged_records_and_keep_the_same_tail_after_commit() {
    let database = Database::new();
    let db = SqliteStorage::open(database.0.clone()).unwrap();
    let store = SqliteTimelineStore::new(db.clone());
    let memory = MemoryTimelineStore::default();
    store
        .create_session("properties", &SessionConfig::default())
        .await
        .unwrap();
    memory
        .create_session("properties", &SessionConfig::default())
        .await
        .unwrap();
    let mut records = vec![text(1, "a"), text(2, "b"), text(3, "c")];
    for record in &mut records {
        Arc::make_mut(&mut record.event).tags =
            Some(vec!["one".into(), "two".into(), "three".into()]);
    }
    append(&store, key("properties", "main"), records.clone())
        .await
        .unwrap();
    memory
        .append(
            key("properties", "main"),
            records,
            Box::new(|result| result.unwrap()),
        )
        .unwrap();
    let tail = text(4, "d");
    let mut scope = scope(4);
    scope.segments[0].from = 2;
    scope.segments[0].stored_through = 3;
    scope.segments[0].tail = vec![tail.event.clone()];
    let mut query = fields(&["data", "data.turn_id", "data.request_id", "position"]);
    query.filter = Some(QueryFilter::And {
        conditions: vec![
            eq("data.request_id", json!("request")),
            QueryFilter::Exists {
                condition: Box::new(QueryFilter::And {
                    conditions: vec![
                        eq("data.delta", json!("d")),
                        QueryFilter::Compare {
                            left: QueryOperand::Field {
                                path: "data.request_id".into(),
                            },
                            comparison: QueryComparison::Eq,
                            right: QueryOperand::Outer {
                                path: "data.request_id".into(),
                            },
                        },
                    ],
                }),
            },
        ],
    });
    query.order_by = vec![QueryOrder {
        key: QueryOrderKey::Position,
        direction: TimelineQueryDirection::Descending,
    }];
    parity(&store, &memory, &scope, &query).await;
    let first = store.query(&scope, &query, Some(&page(1))).await.unwrap();
    append(
        &store,
        key("properties", "main"),
        vec![tail.clone(), text(5, "excluded")],
    )
    .await
    .unwrap();
    memory
        .append(
            key("properties", "main"),
            vec![tail, text(5, "excluded")],
            Box::new(|result| result.unwrap()),
        )
        .unwrap();
    parity(&store, &memory, &scope, &query).await;
    let continued = QueryPage {
        after: first.next,
        ..page(2)
    };
    let expected = store.query(&scope, &query, Some(&continued)).await.unwrap();
    scope.segments[0].stored_through = 4;
    scope.segments[0].tail.clear();
    assert_eq!(
        serde_json::to_value(store.query(&scope, &query, Some(&continued)).await.unwrap()).unwrap(),
        serde_json::to_value(expected).unwrap()
    );
    query.select = QuerySelect::Events;
    parity(&store, &memory, &scope, &query).await;
    store.close().await.unwrap();
    db.close().await.unwrap();
}

#[tokio::test]
async fn property_queries_load_only_selected_request_components_and_filter_bodies_before_limit() {
    let database = Database::new();
    let db = SqliteStorage::open(database.0.clone()).unwrap();
    let store = SqliteTimelineStore::with_content_budget(db.clone(), 1);
    let memory = MemoryTimelineStore::default();
    store
        .create_session("properties", &SessionConfig::default())
        .await
        .unwrap();
    memory
        .create_session("properties", &SessionConfig::default())
        .await
        .unwrap();
    let mut records = vec![
        model_call(1, vec![Message::text(Role::User, "first")]),
        model_call(2, vec![Message::text(Role::User, "second")]),
    ];
    let EventKind::ModelCall(call) = &mut Arc::make_mut(&mut records[1].event).kind else {
        unreachable!()
    };
    call.failure_chain = Some(dim_agent_types::FailureChain {
        from_request_id: "earlier-request".into(),
        attempts: 5,
        waited_ms: 70_000,
    });
    Arc::make_mut(&mut call.request).system =
        Some(Arc::new(SystemPrompt::Text("selected system".into())));
    append(&store, key("properties", "main"), records.clone())
        .await
        .unwrap();
    memory
        .append(
            key("properties", "main"),
            records,
            Box::new(|result| result.unwrap()),
        )
        .unwrap();
    let scope = scope(2);
    let mut query = fields(&[
        "data.request.providerId",
        "data.request.tools",
        "data.request.system",
        "data.turn_id",
        "data.failure_chain",
    ]);
    store.take_node_loads();
    parity(&store, &memory, &scope, &query).await;
    assert_eq!(
        store.take_node_loads().loads,
        0,
        "request headers and absent optional components do not read message nodes"
    );
    query = fields(&["data.request.messages[].role"]);
    parity(&store, &memory, &scope, &query).await;
    query = fields(&[
        "data.request.messages[0].role",
        "data.request.messages[1].role",
    ]);
    parity(&store, &memory, &scope, &query).await;
    query.filter = Some(eq("data.request.messages[0].role", json!("user")));
    parity(&store, &memory, &scope, &query).await;
    query.filter = Some(QueryFilter::Present {
        value: QueryOperand::Field {
            path: "data.request.messages[0].role".into(),
        },
    });
    parity(&store, &memory, &scope, &query).await;
    query.filter = Some(QueryFilter::Compare {
        left: QueryOperand::Field {
            path: "data.turn_id".into(),
        },
        comparison: QueryComparison::Eq,
        right: QueryOperand::Value {
            value: json!("turn-2"),
        },
    });
    query.select = QuerySelect::Fields {
        fields: vec!["data.request".into()],
    };
    parity(&store, &memory, &scope, &query).await;
    query.filter = Some(QueryFilter::Any {
        field: "data.request.messages".into(),
        condition: Box::new(QueryFilter::Compare {
            left: QueryOperand::Element {
                path: "role".into(),
            },
            comparison: QueryComparison::Eq,
            right: QueryOperand::Value {
                value: json!("user"),
            },
        }),
    });
    query.select = QuerySelect::Count;
    parity(&store, &memory, &scope, &query).await;
    query = fields(&["position.sequence"]);
    query.filter = Some(eq("data.request.system", json!("selected system")));
    store.take_node_loads();
    let selected = store.query(&scope, &query, Some(&page(1))).await.unwrap();
    assert_eq!(
        serde_json::to_value(selected.result).unwrap(),
        json!({
            "kind":"rows","rows":[{"group":[],"values":[{"kind":"value","value":2}]}]
        })
    );
    assert_eq!(
        store.take_node_loads().loads,
        0,
        "system filtering does not load message nodes"
    );
    parity(&store, &memory, &scope, &query).await;
    let selection = query.clone();
    query.filter = Some(QueryFilter::And {
        conditions: vec![
            eq("data.request.messages[0].role", json!("user")),
            QueryFilter::Exists {
                condition: Box::new(QueryFilter::And {
                    conditions: vec![
                        eq("data.request.system", json!("selected system")),
                        QueryFilter::Compare {
                            left: QueryOperand::Field {
                                path: "position.sequence".into(),
                            },
                            comparison: QueryComparison::Gt,
                            right: QueryOperand::Outer {
                                path: "position.sequence".into(),
                            },
                        },
                    ],
                }),
            },
        ],
    });
    parity(&store, &memory, &scope, &query).await;
    query = selection;
    let connection = Connection::open(&database.0).unwrap();
    connection
        .execute(
            "UPDATE timeline_content SET body='{}' WHERE kind='node'",
            [],
        )
        .unwrap();
    drop(connection);
    store.query(&scope, &query, Some(&page(1))).await.unwrap();
    query = fields(&["data.request.messages"]);
    assert!(
        store.query(&scope, &query, Some(&page(1))).await.is_err(),
        "reading a selected corrupted message must fail instead of returning an empty request"
    );
    store.close().await.unwrap();
    db.close().await.unwrap();
}

#[tokio::test]
async fn property_position_pages_bound_sql_work_before_decoding() {
    let mut costs = Vec::new();
    for size in [128, 2048] {
        let database = Database::new();
        let db = SqliteStorage::open(database.0.clone()).unwrap();
        let store = SqliteTimelineStore::new(db.clone());
        store
            .create_session("properties", &SessionConfig::default())
            .await
            .unwrap();
        let records = (1..=size)
            .map(|sequence| {
                record(
                    sequence,
                    EventKind::Extension(
                        ExtensionEvent::new(
                            "property",
                            "value",
                            json!({"sequence":sequence,"text":"selected"}),
                        )
                        .unwrap(),
                    ),
                )
            })
            .collect();
        append(&store, key("properties", "main"), records)
            .await
            .unwrap();
        let mut query = fields(&["position.sequence"]);
        query.filter = Some(QueryFilter::And {
            conditions: vec![
                eq("kind", json!("property/value")),
                QueryFilter::Prefix {
                    value: QueryOperand::Field {
                        path: "data.text".into(),
                    },
                    prefix: "select".into(),
                },
            ],
        });
        let mut measured = Vec::new();
        for direction in [
            TimelineQueryDirection::Ascending,
            TimelineQueryDirection::Descending,
        ] {
            query.order_by = vec![QueryOrder {
                key: QueryOrderKey::Position,
                direction,
            }];
            let mut paging = page(16);
            for number in 0..size / 16 {
                store.record_query_statistics();
                let reply = store
                    .query(&scope(size), &query, Some(&paging))
                    .await
                    .unwrap();
                let stats = store.take_query_statistics();
                let dim_agent_runtime::QueryOutput::Rows { rows } = reply.result else {
                    panic!("field rows")
                };
                let expected = if direction == TimelineQueryDirection::Ascending {
                    number * 16 + 1
                } else {
                    size - number * 16
                };
                assert_eq!(
                    rows[0].values[0],
                    dim_agent_runtime::QueryValue::Value {
                        value: expected.into()
                    }
                );
                assert_eq!(rows.len(), 16);
                assert!(
                    stats
                        .iter()
                        .flat_map(|s| &s.plan)
                        .any(|line| line.contains("SEARCH") && line.contains("timeline_events")),
                    "position pages use a stored index: {stats:?}"
                );
                if number < 3 || reply.next.is_none() {
                    measured.push(stats.iter().map(|s| s.vm_steps).sum::<i32>());
                }
                if number + 1 == size / 16 {
                    assert!(
                        reply.next.is_none(),
                        "the last page exhausts the fixed range"
                    );
                } else {
                    paging.after = Some(reply.next.expect("more positions remain"));
                }
            }
        }
        costs.push(measured);
        store.close().await.unwrap();
        db.close().await.unwrap();
    }
    for (small, large) in costs[0].iter().zip(&costs[1]) {
        assert!(
            *large < small * 2,
            "a fixed position page must not sort the growing history: small={small}, large={large}"
        );
    }
}

#[tokio::test]
async fn submission_identity_queries_use_the_index_for_hits_and_misses() {
    let mut costs = Vec::new();
    for size in [128, 2048] {
        let database = Database::new();
        let db = SqliteStorage::open(database.0.clone()).unwrap();
        let store = SqliteTimelineStore::new(db.clone());
        store
            .create_session("properties", &SessionConfig::default())
            .await
            .unwrap();
        let records = (1..=size)
            .map(|sequence| {
                let mut input = InputMessageData::text("input", None);
                input.submission_id = Some(format!("submission-{sequence}"));
                record(sequence, EventKind::InputMessage(input))
            })
            .collect();
        append(&store, key("properties", "main"), records)
            .await
            .unwrap();
        let mut measured = Vec::new();
        for (id, count) in [(format!("submission-{size}"), 1), ("missing".into(), 0)] {
            let mut query = fields(&["id"]);
            query.filter = Some(QueryFilter::And {
                conditions: vec![
                    eq("kind", json!("input/message")),
                    eq("data.submission_id", json!(id)),
                ],
            });
            store.record_query_statistics();
            let result = store
                .query(&scope(size), &query, Some(&page(2)))
                .await
                .unwrap();
            let stats = store.take_query_statistics();
            let dim_agent_runtime::QueryOutput::Rows { rows } = result.result else {
                panic!("field rows")
            };
            assert_eq!(rows.len(), count);
            assert!(result.next.is_none());
            assert!(
                stats
                    .iter()
                    .flat_map(|s| &s.plan)
                    .any(|line| line.contains("SEARCH")
                        && line.contains("timeline_input_submissions")
                        && line.contains("<expr>=?")),
                "submission identity must constrain the expression index: {stats:?}"
            );
            measured.push(stats.iter().map(|s| s.vm_steps).sum::<i32>());
        }
        costs.push(measured);
        store.close().await.unwrap();
        db.close().await.unwrap();
    }
    for (small, large) in costs[0].iter().zip(&costs[1]) {
        assert!(
            *large <= small + 100,
            "fixed identity lookup grew with input history: {costs:?}"
        );
    }
}

#[tokio::test]
async fn a_v34_submission_index_upgrade_preserves_duplicate_identities_and_reopens() {
    let database = Database::new();
    let db = SqliteStorage::open(database.0.clone()).unwrap();
    let store = SqliteTimelineStore::new(db.clone());
    store
        .create_session("properties", &SessionConfig::default())
        .await
        .unwrap();
    let records = (1..=2)
        .map(|sequence| {
            let mut input = InputMessageData::text("duplicate", None);
            input.submission_id = Some("duplicate".into());
            record(sequence, EventKind::InputMessage(input))
        })
        .collect();
    append(&store, key("properties", "main"), records)
        .await
        .unwrap();
    store.close().await.unwrap();
    db.close().await.unwrap();
    let connection = Connection::open(&database.0).unwrap();
    drop_v40(&connection);
    connection
        .execute_batch(
            "PRAGMA foreign_keys=OFF;
        BEGIN; DROP TABLE timeline_array_elements; DROP TABLE timeline_member_roots; DROP TABLE timeline_member_origins; DROP TABLE timeline_member_terminals; DROP TABLE timeline_member_nodes; DROP INDEX timeline_events_requests; DROP INDEX timeline_events_inputs; DROP INDEX timeline_events_users; DROP TRIGGER timeline_text_delete; DROP TABLE timeline_text; DROP TABLE timeline_text_fts; DROP TABLE timeline_text_tails; DROP INDEX timeline_input_submissions;
        DROP TABLE session_owners;
        CREATE TABLE session_owners(session_id TEXT PRIMARY KEY REFERENCES agent_sessions(session_id), agent_id TEXT NOT NULL REFERENCES agents(agent_id), protocol_version INTEGER NOT NULL CHECK(protocol_version IN (1, 2, 3)));
        CREATE INDEX session_owners_by_agent ON session_owners(agent_id, protocol_version, session_id);
        CREATE TEMP TABLE sessions_rows AS SELECT session_id, config, created_at_ms, leader_session_id, origin FROM agent_sessions;
        DROP INDEX agent_sessions_by_leader;
        DROP TABLE agent_sessions;
        CREATE TABLE agent_sessions (session_id TEXT NOT NULL PRIMARY KEY, config TEXT NOT NULL, created_at_ms INTEGER NOT NULL CHECK(created_at_ms >= 0), leader_session_id TEXT REFERENCES agent_sessions(session_id), origin TEXT NOT NULL DEFAULT 'unknown' CHECK (origin IN ('client', 'acp', 'internal', 'unknown')));
        CREATE INDEX agent_sessions_by_leader ON agent_sessions(leader_session_id);
        INSERT INTO agent_sessions(session_id, config, created_at_ms, leader_session_id, origin) SELECT session_id, config, created_at_ms, leader_session_id, origin FROM sessions_rows;
        DROP TABLE sessions_rows;
        DROP TABLE agent_store_format;
        CREATE TABLE agent_store_format (version INTEGER NOT NULL PRIMARY KEY CHECK (version = 34));
        INSERT INTO agent_store_format VALUES (34); COMMIT;
        PRAGMA foreign_keys=ON;",
        )
        .unwrap();
    assert_eq!(schema(&connection), schema_of(34));
    let original = rows(&connection, "timeline_events");
    drop(connection);
    let db = SqliteStorage::open(database.0.clone()).unwrap();
    assert_eq!(db.upgraded_from(), Some(34));
    let connection = Connection::open(&database.0).unwrap();
    assert_eq!(schema(&connection), schema_of(STORE_FORMAT));
    assert_eq!(marker(&connection), [i64::from(STORE_FORMAT)]);
    assert_eq!(rows(&connection, "timeline_events"), original);
    drop(connection);
    let store = SqliteTimelineStore::new(db.clone());
    let mut query = fields(&["id"]);
    query.filter = Some(QueryFilter::And {
        conditions: vec![
            eq("kind", json!("input/message")),
            eq("data.submission_id", json!("duplicate")),
        ],
    });
    let result = store
        .query(&scope(2), &query, Some(&page(2)))
        .await
        .unwrap();
    let dim_agent_runtime::QueryOutput::Rows { rows } = result.result else {
        panic!("field rows")
    };
    assert_eq!(
        rows.len(),
        2,
        "ambiguous historical submissions stay detectable"
    );
    store.close().await.unwrap();
    db.close().await.unwrap();
    let db = SqliteStorage::open(database.0.clone()).unwrap();
    assert_eq!(db.upgraded_from(), None);
    db.close().await.unwrap();
}

#[test]
fn a_malformed_v34_input_rolls_back_the_submission_index_upgrade() {
    let database = Database::new();
    let connection = Connection::open(&database.0).unwrap();
    let mut statements = schema_of(34);
    statements.sort_by_key(|sql| !sql.starts_with("CREATE TABLE"));
    for sql in statements {
        connection.execute_batch(&sql).unwrap();
    }
    connection
        .execute("INSERT INTO agent_store_format VALUES (34)", [])
        .unwrap();
    insert_documented_records(&connection, BUSINESS_TABLES);
    connection
        .execute(
            "UPDATE timeline_events SET body='not json' WHERE kind='input/message'",
            [],
        )
        .unwrap();
    let original = rows(&connection, "timeline_events");
    drop(connection);
    let bytes = std::fs::read(&database.0).unwrap();
    assert!(SqliteStorage::open(database.0.clone()).is_err());
    assert_eq!(
        std::fs::read(&database.0).unwrap(),
        bytes,
        "failed index creation leaves the file intact"
    );
    let connection = Connection::open(&database.0).unwrap();
    assert_eq!(schema(&connection), schema_of(34));
    assert_eq!(marker(&connection), [34]);
    assert_eq!(rows(&connection, "timeline_events"), original);
}

#[tokio::test]
async fn text_candidates_survive_output_merging_reopen_fork_and_session_deletion() {
    let database = Database::new();
    let db = SqliteStorage::open(database.0.clone()).unwrap();
    let store = SqliteTimelineStore::new(db.clone());
    store
        .create_session("properties", &SessionConfig::default())
        .await
        .unwrap();
    let mut records: Vec<_> = (1..=1000)
        .map(|sequence| {
            record(
                sequence,
                EventKind::Extension(
                    ExtensionEvent::new(
                        "property",
                        "value",
                        json!({"text":format!("ordinary text {sequence}")}),
                    )
                    .unwrap(),
                ),
            )
        })
        .collect();
    records.push(text(1001, "the UNIQUE"));
    records.push(text(1002, "NEEDLE is here"));
    records.push(text(1003, "uniqueneedle again\0afterzero 中文 İ"));
    append(&store, key("properties", "main"), records)
        .await
        .unwrap();
    let query = QuerySpec {
        filter: Some(QueryFilter::And {
            conditions: vec![
                eq("kind", json!("step/text")),
                QueryFilter::TextCandidate {
                    field: "data.delta".into(),
                    partition_by: vec!["kind".into(), "data.request_id".into()],
                    terms: vec!["uniqueneedle".into()],
                },
            ],
        }),
        select: QuerySelect::Fields {
            fields: vec!["data.request_id".into()],
        },
        group_by: vec!["data.request_id".into()],
        pick: Some(QueryPick::First),
        order_by: vec![],
    };
    store.record_query_statistics();
    let reply = store
        .query(&scope(1003), &query, Some(&page(16)))
        .await
        .unwrap();
    let statistics = store.take_query_statistics();
    let dim_agent_runtime::QueryOutput::Rows { rows } = reply.result else {
        panic!("request identities")
    };
    assert_eq!(
        rows.len(),
        1,
        "a word crossing two logical deltas must be a candidate"
    );
    assert!(
        statistics
            .iter()
            .any(|statement| statement.stage == "text_candidates"
                && statement
                    .plan
                    .iter()
                    .any(|line| line.contains("VIRTUAL TABLE INDEX"))),
        "{statistics:?}"
    );
    assert_eq!(
        store.take_node_loads().loads,
        0,
        "identity candidates do not load model context"
    );
    let mut fragments = query.clone();
    fragments.group_by.clear();
    fragments.pick = None;
    let dim_agent_runtime::QueryOutput::Rows { rows } = store
        .query(&scope(1003), &fragments, Some(&page(16)))
        .await
        .unwrap()
        .result
    else {
        panic!("logical fragments")
    };
    assert_eq!(
        rows.len(),
        3,
        "multiple matching logical fragments in one physical row must not duplicate that row"
    );
    fragments.select = QuerySelect::Count;
    let dim_agent_runtime::QueryOutput::Count { count } = store
        .query(&scope(1003), &fragments, None)
        .await
        .unwrap()
        .result
    else {
        panic!("logical fragment count")
    };
    assert_eq!(count, 3);
    let mut single = fields(&["id"]);
    single.filter = Some(QueryFilter::And {
        conditions: vec![
            eq("kind", json!("step/text")),
            QueryFilter::TextCandidate {
                field: "data".into(),
                partition_by: vec![],
                terms: vec!["here".into()],
            },
        ],
    });
    let dim_agent_runtime::QueryOutput::Rows { rows } = store
        .query(&scope(1003), &single, Some(&page(16)))
        .await
        .unwrap()
        .result
    else {
        panic!("one matching logical fragment");
    };
    assert_eq!(rows.len(), 1);
    for (select, expected) in [
        (QuerySelect::Count, json!({"kind":"count","count":1})),
        (QuerySelect::Exists, json!({"kind":"exists","exists":true})),
    ] {
        single.select = select;
        let reply = store.query(&scope(1003), &single, None).await.unwrap();
        assert_eq!(serde_json::to_value(reply.result).unwrap(), expected);
    }
    for term in ["中文", "İ", "again\0afterzero", "un", "afterzero"] {
        let mut fallback = query.clone();
        let Some(QueryFilter::And { conditions }) = &mut fallback.filter else {
            unreachable!()
        };
        let QueryFilter::TextCandidate { terms, .. } = &mut conditions[1] else {
            unreachable!()
        };
        *terms = vec![term.into()];
        let dim_agent_runtime::QueryOutput::Rows { rows } = store
            .query(&scope(1003), &fallback, Some(&page(16)))
            .await
            .unwrap()
            .result
        else {
            panic!("fallback candidates")
        };
        assert_eq!(
            rows.len(),
            1,
            "fallback terms cannot remove the matching stream: {term:?}"
        );
    }
    let mut missing = query.clone();
    let Some(QueryFilter::And { conditions }) = &mut missing.filter else {
        unreachable!()
    };
    let QueryFilter::TextCandidate { terms, .. } = &mut conditions[1] else {
        unreachable!()
    };
    *terms = vec!["absentwordxyz".into()];
    let dim_agent_runtime::QueryOutput::Rows { rows } = store
        .query(&scope(1003), &missing, Some(&page(16)))
        .await
        .unwrap()
        .result
    else {
        panic!("request identities")
    };
    assert!(
        rows.is_empty(),
        "a missing indexed word does not enumerate all requests"
    );
    let config = store
        .session_config_text("properties")
        .await
        .unwrap()
        .unwrap();
    store
        .fork_session("properties", "copy", 1003, &config)
        .await
        .unwrap();
    let mut copied = scope(1004);
    copied.key.session_id = "copy".into();
    let dim_agent_runtime::QueryOutput::Rows { rows } = store
        .query(&copied, &query, Some(&page(16)))
        .await
        .unwrap()
        .result
    else {
        panic!("copied identities")
    };
    assert_eq!(rows.len(), 1);
    store.rollback_session("copy").await.unwrap();
    assert!(
        store.query(&copied, &query, Some(&page(16))).await.is_err(),
        "the deleted copy is not queryable"
    );
    store.close().await.unwrap();
    db.close().await.unwrap();
    drop(store);
    drop(db);
    let connection = Connection::open(&database.0).unwrap();
    assert_eq!(
        connection
            .query_row(
                "SELECT count(*) FROM timeline_text WHERE session_id='copy'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    drop_v40(&connection);
    without_automation_origin(&connection);
    connection.execute_batch("DROP TABLE timeline_array_elements; DROP TABLE timeline_member_roots; DROP TABLE timeline_member_origins; DROP TABLE timeline_member_terminals; DROP TABLE timeline_member_nodes; DROP INDEX timeline_events_requests; DROP INDEX timeline_events_inputs; DROP INDEX timeline_events_users; DROP TRIGGER timeline_text_delete; DROP TABLE timeline_text; DROP TABLE timeline_text_fts; DROP TABLE timeline_text_tails; DROP TABLE session_owners; CREATE TABLE session_owners(session_id TEXT PRIMARY KEY REFERENCES agent_sessions(session_id), agent_id TEXT NOT NULL REFERENCES agents(agent_id), protocol_version INTEGER NOT NULL CHECK(protocol_version IN (1, 2, 3))); CREATE INDEX session_owners_by_agent ON session_owners(agent_id, protocol_version, session_id); DROP TABLE agent_store_format; CREATE TABLE agent_store_format (version INTEGER NOT NULL PRIMARY KEY CHECK (version = 35)); INSERT INTO agent_store_format VALUES(35);").unwrap();
    drop(connection);
    let db = SqliteStorage::open(database.0.clone()).unwrap();
    assert_eq!(
        db.upgraded_from(),
        Some(35),
        "the index is backfilled in the format upgrade"
    );
    let store = SqliteTimelineStore::new(db.clone());
    let dim_agent_runtime::QueryOutput::Rows { rows } = store
        .query(&scope(1003), &query, Some(&page(16)))
        .await
        .unwrap()
        .result
    else {
        panic!("reopened identities")
    };
    assert_eq!(rows.len(), 1);
    store.close().await.unwrap();
    db.close().await.unwrap();
}

#[test]
fn a_malformed_v35_row_rolls_back_text_index_creation_and_backfill() {
    let database = Database::new();
    let connection = Connection::open(&database.0).unwrap();
    let mut statements = schema_of(35);
    statements.sort_by_key(|sql| !sql.starts_with("CREATE TABLE"));
    for sql in statements {
        connection.execute_batch(&sql).unwrap();
    }
    connection
        .execute("INSERT INTO agent_store_format VALUES (35)", [])
        .unwrap();
    insert_documented_records(&connection, BUSINESS_TABLES);
    connection.execute("UPDATE timeline_events SET body=json_set(body,'$.encoding','invalid') WHERE kind='input/message'", []).unwrap();
    let original = rows(&connection, "timeline_events");
    drop(connection);
    assert!(SqliteStorage::open(database.0.clone()).is_err());
    let connection = Connection::open(&database.0).unwrap();
    assert_eq!(schema(&connection), schema_of(35));
    assert_eq!(marker(&connection), [35]);
    assert_eq!(rows(&connection, "timeline_events"), original);
}

#[tokio::test]
async fn indexed_text_hits_and_misses_do_not_scan_the_growing_history() {
    let mut costs = Vec::new();
    for size in [128, 2048] {
        let database = Database::new();
        let db = SqliteStorage::open(database.0.clone()).unwrap();
        let store = SqliteTimelineStore::new(db.clone());
        store
            .create_session("properties", &SessionConfig::default())
            .await
            .unwrap();
        let records = (1..=size)
            .map(|sequence| {
                record(
                    sequence,
                    EventKind::InputMessage(InputMessageData::text(
                        if sequence == size {
                            "uniqueneedle"
                        } else {
                            "ordinary content"
                        },
                        None,
                    )),
                )
            })
            .collect();
        let begin = std::time::Instant::now();
        append(&store, key("properties", "main"), records)
            .await
            .unwrap();
        let append_ms = begin.elapsed().as_secs_f64() * 1000.;
        append(
            &store,
            key("properties", "main"),
            vec![record(
                size + 1,
                EventKind::InputDeleted(InputDeletedData {
                    input_id: format!("event-{size}"),
                }),
            )],
        )
        .await
        .unwrap();
        let mut measured = Vec::new();
        for (term, expected) in [("uniqueneedle", 1), ("absentwordxyz", 0)] {
            for (related, through, expected) in [
                (false, size, expected),
                (true, size, expected),
                (true, size + 1, 0),
            ] {
                let mut query = fields(&["id"]);
                query.filter = Some(QueryFilter::And {
                    conditions: vec![
                        eq("kind", json!("input/message")),
                        QueryFilter::TextCandidate {
                            field: "data".into(),
                            partition_by: vec![],
                            terms: vec![term.into()],
                        },
                    ],
                });
                if related {
                    let QueryFilter::And { conditions } = query.filter.as_mut().unwrap() else {
                        unreachable!()
                    };
                    conditions.push(QueryFilter::Not {
                        condition: Box::new(QueryFilter::Exists {
                            condition: Box::new(QueryFilter::And {
                                conditions: vec![
                                    eq("kind", json!("input/deleted")),
                                    QueryFilter::Compare {
                                        left: QueryOperand::Field {
                                            path: "data.input_id".into(),
                                        },
                                        comparison: QueryComparison::Eq,
                                        right: QueryOperand::Outer { path: "id".into() },
                                    },
                                ],
                            }),
                        }),
                    });
                }
                store.record_query_statistics();
                let result = store
                    .query(&scope(through), &query, Some(&page(2)))
                    .await
                    .unwrap();
                let stats = store.take_query_statistics();
                let dim_agent_runtime::QueryOutput::Rows { rows } = result.result else {
                    panic!("candidate rows")
                };
                assert_eq!(rows.len(), expected);
                println!("candidate size={size} term={term} stats={stats:?}");
                assert!(
                    stats
                        .iter()
                        .any(|statement| statement.stage == "text_candidates"
                            && statement
                                .plan
                                .iter()
                                .any(|line| line.contains("VIRTUAL TABLE INDEX"))),
                    "{stats:?}"
                );
                assert!(
                    stats
                        .iter()
                        .map(|statement| statement.full_scan_steps)
                        .sum::<i32>()
                        < 100,
                    "a selective word must not scan event or metadata history: {stats:?}"
                );
                measured.push(
                    stats
                        .iter()
                        .map(|statement| statement.vm_steps)
                        .sum::<i32>(),
                );
                query.select = QuerySelect::Count;
                let dim_agent_runtime::QueryOutput::Count { count } = store
                    .query(&scope(through), &query, None)
                    .await
                    .unwrap()
                    .result
                else {
                    panic!("candidate count")
                };
                assert_eq!(count, expected as u64);
            }
        }
        store.close().await.unwrap();
        db.close().await.unwrap();
        let connection = Connection::open(&database.0).unwrap();
        let disk: Vec<(String, i64)> = connection
            .prepare("SELECT name,sum(pgsize) FROM dbstat GROUP BY name ORDER BY name")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap();
        println!(
            "text index size={size} appendMs={append_ms:.3} vmSteps={measured:?} disk={disk:?}"
        );
        costs.push(measured);
    }
    for (small, large) in costs[0].iter().zip(&costs[1]) {
        assert!(
            *large <= small + 200,
            "indexed word lookup grew with history: {costs:?}"
        );
    }
}

#[tokio::test]
async fn input_identity_relations_keep_the_full_ancestor_range_across_pages_and_tails() {
    let database = Database::new();
    let db = SqliteStorage::open(database.0.clone()).unwrap();
    let store = SqliteTimelineStore::new(db.clone());
    let memory = MemoryTimelineStore::default();
    store
        .create_session("properties", &SessionConfig::default())
        .await
        .unwrap();
    memory
        .create_session("properties", &SessionConfig::default())
        .await
        .unwrap();
    let parent = vec![
        record(
            1,
            EventKind::InputMessage(InputMessageData::text("parent one", None)),
        ),
        record(
            2,
            EventKind::InputMessage(InputMessageData::text("parent two", None)),
        ),
        record(
            3,
            EventKind::BranchCreate(BranchCreateData {
                at: BranchCreateAt::Sequence { sequence: 2 },
                source: BranchSource::Host {
                    operation_id: "op".into(),
                },
                branch: "child".into(),
                prompt: "child".into(),
            }),
        ),
    ];
    append(&store, key("properties", "main"), parent.clone())
        .await
        .unwrap();
    memory
        .append(
            key("properties", "main"),
            parent,
            Box::new(|result| result.unwrap()),
        )
        .unwrap();
    let mut child = vec![
        record(
            1,
            EventKind::InputMessage(InputMessageData::text("child one", None)),
        ),
        record(
            2,
            EventKind::InputDeleted(InputDeletedData {
                input_id: "event-1".into(),
            }),
        ),
        record(
            3,
            EventKind::InputConsumed(InputConsumedData {
                input_id: "child-1".into(),
            }),
        ),
    ];
    for item in &mut child {
        Arc::make_mut(&mut item.event).id = format!("child-{}", item.event.sequence);
    }
    append(&store, key("properties", "child"), child.clone())
        .await
        .unwrap();
    memory
        .append(
            key("properties", "child"),
            child,
            Box::new(|result| result.unwrap()),
        )
        .unwrap();
    let mut range = TimelineReadScope {
        key: key("properties", "child"),
        through: 3,
        segments: vec![
            TimelineReadSegment {
                branch_id: "main".into(),
                from: 1,
                through: 2,
                stored_through: 3,
                tail: vec![],
            },
            TimelineReadSegment {
                branch_id: "child".into(),
                from: 1,
                through: 3,
                stored_through: 3,
                tail: vec![],
            },
        ],
    };
    let relation = QueryFilter::Exists {
        condition: Box::new(QueryFilter::And {
            conditions: vec![
                eq("kind", json!("input/deleted")),
                QueryFilter::Compare {
                    left: QueryOperand::Field {
                        path: "data.input_id".into(),
                    },
                    comparison: QueryComparison::Eq,
                    right: QueryOperand::Outer { path: "id".into() },
                },
            ],
        }),
    };
    let mut query = fields(&["id", "position"]);
    query.filter = Some(QueryFilter::And {
        conditions: vec![
            eq("kind", json!("input/message")),
            QueryFilter::Not {
                condition: Box::new(relation.clone()),
            },
        ],
    });
    let mut nested = query.clone();
    let Some(QueryFilter::And { conditions }) = &mut nested.filter else {
        unreachable!()
    };
    let QueryFilter::Not { condition } = &mut conditions[1] else {
        unreachable!()
    };
    let QueryFilter::Exists { condition } = condition.as_mut() else {
        unreachable!()
    };
    let QueryFilter::And { conditions } = condition.as_mut() else {
        unreachable!()
    };
    conditions[0] = QueryFilter::And {
        conditions: vec![conditions[0].clone(), eq("data.status", json!("absent"))],
    };
    parity(&store, &memory, &range, &nested).await;
    for direction in [
        TimelineQueryDirection::Ascending,
        TimelineQueryDirection::Descending,
    ] {
        query.order_by = vec![QueryOrder {
            key: QueryOrderKey::Position,
            direction,
        }];
        parity(&store, &memory, &range, &query).await;
        let mut paging = page(1);
        let mut ids = Vec::new();
        loop {
            let reply = store.query(&range, &query, Some(&paging)).await.unwrap();
            let dim_agent_runtime::QueryOutput::Rows { rows } = reply.result else {
                panic!("input identities")
            };
            ids.extend(
                rows.into_iter()
                    .map(|row| serde_json::to_value(&row.values[0]).unwrap()),
            );
            let Some(next) = reply.next else { break };
            paging.after = Some(next);
        }
        assert_eq!(
            ids.len(),
            2,
            "a child deletion excludes an ancestor input without narrowing the relation range on later pages"
        );
    }
    let mut tail = record(
        4,
        EventKind::InputDeleted(InputDeletedData {
            input_id: "child-1".into(),
        }),
    );
    Arc::make_mut(&mut tail.event).id = "child-4".into();
    range.through = 4;
    range.segments[1].through = 4;
    range.segments[1].tail = vec![tail.event.clone()];
    parity(&store, &memory, &range, &query).await;
    query.select = QuerySelect::Count;
    query.order_by.clear();
    parity(&store, &memory, &range, &query).await;
    let dim_agent_runtime::QueryOutput::Count { count } =
        store.query(&range, &query, None).await.unwrap().result
    else {
        panic!("live input count")
    };
    assert_eq!(count, 1);
    append(&store, key("properties", "child"), vec![tail.clone()])
        .await
        .unwrap();
    memory
        .append(
            key("properties", "child"),
            vec![tail],
            Box::new(|result| result.unwrap()),
        )
        .unwrap();
    range.segments[1].stored_through = 4;
    range.segments[1].tail.clear();
    parity(&store, &memory, &range, &query).await;
    store.close().await.unwrap();
    db.close().await.unwrap();
}

#[tokio::test]
async fn input_anti_joins_seek_the_full_identity_index_as_history_grows() {
    let mut costs = Vec::new();
    for size in [128, 2048] {
        let database = Database::new();
        let db = SqliteStorage::open(database.0.clone()).unwrap();
        let store = SqliteTimelineStore::new(db.clone());
        store
            .create_session("properties", &SessionConfig::default())
            .await
            .unwrap();
        let mut records = Vec::new();
        for n in 0..size {
            records.push(record(
                n * 2 + 1,
                EventKind::InputMessage(InputMessageData::text("input", None)),
            ));
            if n + 1 < size {
                records.push(record(
                    n * 2 + 2,
                    EventKind::InputConsumed(InputConsumedData {
                        input_id: format!("event-{}", n * 2 + 1),
                    }),
                ));
            }
        }
        append(&store, key("properties", "main"), records)
            .await
            .unwrap();
        let mut query = fields(&["id"]);
        query.filter = Some(QueryFilter::And {
            conditions: vec![
                eq("kind", json!("input/message")),
                QueryFilter::Not {
                    condition: Box::new(QueryFilter::Exists {
                        condition: Box::new(QueryFilter::And {
                            conditions: vec![
                                eq("kind", json!("input/consumed")),
                                QueryFilter::Compare {
                                    left: QueryOperand::Field {
                                        path: "data.input_id".into(),
                                    },
                                    comparison: QueryComparison::Eq,
                                    right: QueryOperand::Outer { path: "id".into() },
                                },
                            ],
                        }),
                    }),
                },
            ],
        });
        let mut bounded = query.clone();
        let Some(QueryFilter::And { conditions }) = &mut bounded.filter else {
            unreachable!()
        };
        let QueryFilter::Not { condition } = &mut conditions[1] else {
            unreachable!()
        };
        let QueryFilter::Exists { condition } = condition.as_mut() else {
            unreachable!()
        };
        let QueryFilter::And { conditions } = condition.as_mut() else {
            unreachable!()
        };
        conditions.push(QueryFilter::Compare {
            left: QueryOperand::Field {
                path: "position.sequence".into(),
            },
            comparison: QueryComparison::Lt,
            right: QueryOperand::Value {
                value: json!(size * 2),
            },
        });
        let dim_agent_runtime::QueryOutput::Rows { rows } = store
            .query(&scope(size * 2 - 1), &bounded, Some(&page(2)))
            .await
            .unwrap()
            .result
        else {
            panic!("pending identities before a turn")
        };
        assert_eq!(rows.len(), 1);
        store.record_query_statistics();
        let result = store
            .query(&scope(size * 2 - 1), &query, Some(&page(2)))
            .await
            .unwrap();
        let dim_agent_runtime::QueryOutput::Rows { rows } = result.result else {
            panic!("pending identities")
        };
        assert_eq!(rows.len(), 1);
        let stats = store.take_query_statistics();
        assert!(
            stats
                .iter()
                .flat_map(|statement| &statement.plan)
                .any(|line| line.contains("timeline_events_inputs") && line.contains("<expr>=?")),
            "a prefix-only index lookup still scans every terminal: {stats:?}"
        );
        costs.push(
            stats
                .iter()
                .map(|statement| statement.vm_steps)
                .sum::<i32>(),
        );
        store.close().await.unwrap();
        db.close().await.unwrap();
    }
    assert!(
        costs[1] < costs[0] * 20,
        "a 16x larger terminal history must not become a quadratic anti-join: {costs:?}"
    );
    println!("input anti-join VM steps: {costs:?}");
}

#[tokio::test]
async fn recall_text_relations_seek_candidates_across_position_pages() {
    let mut costs = Vec::new();
    for size in [128u64, 2048] {
        let database = Database::new();
        let db = SqliteStorage::open(database.0.clone()).unwrap();
        let store = SqliteTimelineStore::new(db.clone());
        store
            .create_session("properties", &SessionConfig::default())
            .await
            .unwrap();
        let mut records = Vec::new();
        for n in 0..size {
            let at = 1 + n * 2;
            records.push(record(
                at,
                EventKind::InputMessage(InputMessageData::text("uniqueneedle", None)),
            ));
            records.push(record(
                at + 1,
                EventKind::InputConsumed(InputConsumedData {
                    input_id: format!("event-{at}"),
                }),
            ));
        }
        append(&store, key("properties", "main"), records)
            .await
            .unwrap();
        let mut query = fields(&["id"]);
        query.filter = Some(QueryFilter::And {
            conditions: vec![
                eq("kind", json!("input/consumed")),
                QueryFilter::Exists {
                    condition: Box::new(QueryFilter::And {
                        conditions: vec![
                            eq("kind", json!("input/message")),
                            QueryFilter::Compare {
                                left: QueryOperand::Field { path: "id".into() },
                                comparison: QueryComparison::Eq,
                                right: QueryOperand::Outer {
                                    path: "data.input_id".into(),
                                },
                            },
                            QueryFilter::TextCandidate {
                                field: "data".into(),
                                partition_by: vec![],
                                terms: vec!["uniqueneedle".into()],
                            },
                        ],
                    }),
                },
            ],
        });
        let mut measured = Vec::new();
        for direction in [
            TimelineQueryDirection::Ascending,
            TimelineQueryDirection::Descending,
        ] {
            query.order_by = vec![QueryOrder {
                key: QueryOrderKey::Position,
                direction,
            }];
            let mut p = page(128);
            let mut all = 0i64;
            let mut first = 0;
            let mut pages = 0;
            loop {
                store.record_query_statistics();
                let r = store
                    .query(&scope(size * 2), &query, Some(&p))
                    .await
                    .unwrap();
                let stats = store.take_query_statistics();
                let steps = stats.iter().map(|s| i64::from(s.vm_steps)).sum::<i64>();
                all += steps;
                if pages == 0 {
                    first = steps;
                }
                let dim_agent_runtime::QueryOutput::Rows { rows } = &r.result else {
                    panic!("Recall candidates")
                };
                assert_eq!(rows.len(), 128);
                assert_eq!(
                    rows[0].values[0],
                    dim_agent_runtime::QueryValue::Value {
                        value: json!(format!(
                            "event-{}",
                            if direction == TimelineQueryDirection::Descending {
                                size * 2 - pages * 256
                            } else {
                                pages * 256 + 2
                            }
                        ))
                    }
                );
                pages += 1;
                let Some(next) = r.next else {
                    break;
                };
                p.after = Some(next);
            }
            println!(
                "Recall relation size={size} direction={direction:?} pages={pages} firstVM={first} totalVM={all}"
            );
            measured.push(first);
            measured.push(all);
        }
        let Some(QueryFilter::And { conditions }) = &mut query.filter else {
            unreachable!()
        };
        let QueryFilter::Exists { condition } = &mut conditions[1] else {
            unreachable!()
        };
        let QueryFilter::And { conditions } = condition.as_mut() else {
            unreachable!()
        };
        let QueryFilter::TextCandidate { terms, .. } = &mut conditions[2] else {
            unreachable!()
        };
        *terms = vec!["absentwordxyz".into()];
        store.record_query_statistics();
        let result = store
            .query(&scope(size * 2), &query, Some(&page(128)))
            .await
            .unwrap();
        let dim_agent_runtime::QueryOutput::Rows { rows } = result.result else {
            panic!("Recall candidates")
        };
        assert!(rows.is_empty());
        assert!(result.next.is_none());
        measured.push(
            store
                .take_query_statistics()
                .iter()
                .map(|s| i64::from(s.vm_steps))
                .sum(),
        );
        costs.push(measured);
        store.close().await.unwrap();
        db.close().await.unwrap();
    }
    for (small, large) in costs[0].iter().zip(&costs[1]) {
        assert!(
            *large < *small * 20,
            "16x more candidates must not produce quadratic first-page or full-pagination work: {costs:?}"
        );
    }
}

#[tokio::test]
async fn mixed_recall_relations_page_inputs_outputs_and_tools_without_rebuilding_the_prefix() {
    let and = |conditions| QueryFilter::And { conditions };
    let or = |conditions| QueryFilter::Or { conditions };
    let kinds = |names: &[&str]| QueryFilter::In {
        value: QueryOperand::Field {
            path: "kind".into(),
        },
        values: names.iter().map(|name| json!(name)).collect(),
    };
    let joined = |inner: &str, outer: &str| QueryFilter::Compare {
        left: QueryOperand::Field { path: inner.into() },
        comparison: QueryComparison::Eq,
        right: QueryOperand::Outer { path: outer.into() },
    };
    let exists = |condition| QueryFilter::Exists {
        condition: Box::new(condition),
    };
    let mut costs = Vec::new();
    for size in [128u64, 2048] {
        let database = Database::new();
        let db = SqliteStorage::open(database.0.clone()).unwrap();
        let store = SqliteTimelineStore::new(db.clone());
        store
            .create_session("properties", &SessionConfig::default())
            .await
            .unwrap();
        let mut records = Vec::new();
        for n in 0..size {
            let at = 1 + n * 7;
            let request = format!("request-{n}");
            let turn = format!("turn-{n}");
            records.extend([
                record(
                    at,
                    EventKind::InputMessage(InputMessageData::text("uniqueneedle", None)),
                ),
                record(
                    at + 1,
                    EventKind::InputConsumed(InputConsumedData {
                        input_id: format!("event-{at}"),
                    }),
                ),
                record(
                    at + 2,
                    EventKind::ModelStart(ModelStartData {
                        turn_id: turn.clone(),
                        request_id: request.clone(),
                    }),
                ),
                record(
                    at + 3,
                    EventKind::Text(DeltaData {
                        turn_id: turn.clone(),
                        request_id: request.clone(),
                        delta: "uniqueneedle".into(),
                    }),
                ),
                record(
                    at + 4,
                    EventKind::ToolUseStart(ToolUseStartData {
                        turn_id: turn.clone(),
                        request_id: request.clone(),
                        call_id: format!("call-{n}"),
                        name: "echo".into(),
                    }),
                ),
                record(
                    at + 5,
                    EventKind::ToolUse(ToolUseData {
                        turn_id: turn.clone(),
                        request_id: request.clone(),
                        call: ToolCall {
                            id: format!("call-{n}"),
                            name: "echo".into(),
                            input: json!({"text":"uniqueneedle"}).as_object().unwrap().clone(),
                        },
                    }),
                ),
                record(
                    at + 6,
                    EventKind::ToolResult(ToolResultData {
                        turn_id: turn,
                        request_id: request,
                        call_event_id: None,
                        results: vec![ToolCallResult {
                            call_id: format!("call-{n}"),
                            result: ToolResult::text("uniqueneedle"),
                        }],
                    }),
                ),
            ]);
        }
        append(&store, key("properties", "main"), records)
            .await
            .unwrap();
        let mut measured = Vec::new();
        for word in ["uniqueneedle", "absentwordxyz"] {
            let text = |field: &str, partition: &[&str]| QueryFilter::TextCandidate {
                field: field.into(),
                partition_by: partition.iter().map(|p| (*p).into()).collect(),
                terms: vec![word.into()],
            };
            let mut query = fields(&[
                "id",
                "kind",
                "position",
                "data.input_id",
                "data.request_id",
                "data.call_id",
            ]);
            query.filter = Some(or(vec![
                and(vec![
                    kinds(&["input/consumed"]),
                    exists(and(vec![
                        kinds(&["input/message"]),
                        joined("id", "data.input_id"),
                        text("data", &[]),
                    ])),
                ]),
                and(vec![
                    kinds(&["step/model-start"]),
                    exists(and(vec![
                        joined("data.request_id", "data.request_id"),
                        or(vec![
                            and(vec![
                                kinds(&["step/text"]),
                                text("data.delta", &["kind", "data.request_id"]),
                            ]),
                            and(vec![kinds(&["step/model-content"]), text("data", &[])]),
                        ]),
                    ])),
                ]),
                and(vec![
                    kinds(&["step/tool-use-start"]),
                    or(vec![
                        exists(and(vec![
                            kinds(&["step/tool-use-start", "step/tool-use", "step/tool-result"]),
                            joined("data.request_id", "data.request_id"),
                            text("data", &[]),
                        ])),
                        exists(and(vec![
                            kinds(&[
                                "exec/started",
                                "collaboration/request",
                                "collaboration/message",
                            ]),
                            joined("data.request_id", "data.request_id"),
                        ])),
                        exists(and(vec![
                            kinds(&["approval/request", "branch/create"]),
                            joined("data.source.request_id", "data.request_id"),
                        ])),
                    ]),
                ]),
                kinds(&[
                    "stop/interrupt",
                    "permission/changed",
                    "context/compaction",
                    "session/forked",
                    "branch/start",
                ]),
            ]));
            query.order_by = vec![QueryOrder {
                key: QueryOrderKey::Position,
                direction: TimelineQueryDirection::Descending,
            }];
            let mut p = page(128);
            let mut seen = std::collections::BTreeSet::new();
            let mut steps = 0i64;
            let mut plans = Vec::new();
            loop {
                store.record_query_statistics();
                let result = store
                    .query(&scope(size * 7), &query, Some(&p))
                    .await
                    .unwrap();
                let stats = store.take_query_statistics();
                steps += stats.iter().map(|s| i64::from(s.vm_steps)).sum::<i64>();
                if plans.is_empty() {
                    plans = stats.iter().flat_map(|s| s.plan.clone()).collect();
                }
                let dim_agent_runtime::QueryOutput::Rows { rows } = result.result else {
                    panic!("mixed Recall candidates")
                };
                for row in rows {
                    assert!(
                        seen.insert(serde_json::to_string(&row.values[0]).unwrap()),
                        "each origin occurs once across pages"
                    );
                }
                let Some(next) = result.next else {
                    break;
                };
                p.after = Some(next);
            }
            assert_eq!(
                seen.len(),
                if word == "uniqueneedle" {
                    size as usize * 3
                } else {
                    0
                }
            );
            assert!(
                plans
                    .iter()
                    .any(|line| line.contains("timeline_events_requests")),
                "request joins must use the existing identity index: {plans:?}"
            );
            println!("Mixed Recall size={size} word={word} totalVM={steps}");
            measured.push(steps);
        }
        costs.push(measured);
        store.close().await.unwrap();
        db.close().await.unwrap();
    }
    for (small, large) in costs[0].iter().zip(&costs[1]) {
        assert!(
            *large < *small * 20,
            "16x more mixed candidates must not become quadratic: {costs:?}"
        );
    }
}

#[tokio::test]
async fn related_text_candidates_keep_unindexed_kinds_when_the_same_filter_is_indexed_elsewhere() {
    let database = Database::new();
    let db = SqliteStorage::open(database.0.clone()).unwrap();
    let store = SqliteTimelineStore::new(db.clone());
    store
        .create_session("properties", &SessionConfig::default())
        .await
        .unwrap();
    append(
        &store,
        key("properties", "main"),
        vec![
            model_call(1, vec![Message::text(Role::User, "ordinary content")]),
            record(
                2,
                EventKind::ModelStart(ModelStartData {
                    turn_id: "turn-1".into(),
                    request_id: "request-1".into(),
                }),
            ),
            record(
                3,
                EventKind::InputMessage(InputMessageData::text("ordinary content", None)),
            ),
            record(
                4,
                EventKind::InputConsumed(InputConsumedData {
                    input_id: "event-3".into(),
                }),
            ),
        ],
    )
    .await
    .unwrap();
    let text = QueryFilter::TextCandidate {
        field: "data".into(),
        partition_by: vec![],
        terms: vec!["absentwordxyz".into()],
    };
    let relation = |kind: &str, inner: &str, outer: &str| QueryFilter::Exists {
        condition: Box::new(QueryFilter::And {
            conditions: vec![
                eq("kind", json!(kind)),
                QueryFilter::Compare {
                    left: QueryOperand::Field { path: inner.into() },
                    comparison: QueryComparison::Eq,
                    right: QueryOperand::Outer { path: outer.into() },
                },
                text.clone(),
            ],
        }),
    };
    let mut query = fields(&["id"]);
    query.filter = Some(QueryFilter::Or {
        conditions: vec![
            QueryFilter::And {
                conditions: vec![
                    eq("kind", json!("input/consumed")),
                    relation("input/message", "id", "data.input_id"),
                ],
            },
            QueryFilter::And {
                conditions: vec![
                    eq("kind", json!("step/model-start")),
                    relation("model/call", "data.request_id", "data.request_id"),
                ],
            },
        ],
    });
    let result = store
        .query(&scope(4), &query, Some(&page(2)))
        .await
        .unwrap();
    let dim_agent_runtime::QueryOutput::Rows { rows } = result.result else {
        panic!("candidate identities")
    };
    assert_eq!(
        rows.len(),
        1,
        "an unindexed request remains a candidate even for an absent word"
    );
    assert_eq!(
        rows[0].values[0],
        dim_agent_runtime::QueryValue::Value {
            value: json!("event-2")
        }
    );
    store.close().await.unwrap();
    db.close().await.unwrap();
}

#[tokio::test]
async fn optional_physical_properties_keep_typed_boolean_semantics() {
    let database = Database::new();
    let db = SqliteStorage::open(database.0.clone()).unwrap();
    let store = SqliteTimelineStore::new(db.clone());
    let memory = MemoryTimelineStore::default();
    for storage in [&*store as &dyn TimelineStore, &memory] {
        storage
            .create_session("properties", &SessionConfig::default())
            .await
            .unwrap();
    }
    let paths = [
        "data.status",
        "data.epoch",
        "data.request_id",
        "data.input_id",
        "data.turn_id",
        "data.scope.kind",
        "data.scope.id",
        "data.scope.timeline",
        "data.current.at.branch",
        "data.current.at.sequence",
    ];
    let values = [
        None,
        Some(Value::Null),
        Some(json!(false)),
        Some(json!(true)),
        Some(json!(1)),
        Some(json!(1.5)),
        Some(json!("completed")),
        Some(json!("other")),
        Some(json!([])),
        Some(json!({})),
    ];
    let records: Vec<_> = values
        .into_iter()
        .enumerate()
        .map(|(i, value)| {
            let mut data = json!({});
            if let Some(value) = value {
                for path in paths {
                    let mut object = &mut data;
                    let mut parts = path.strip_prefix("data.").unwrap().split('.').peekable();
                    while let Some(part) = parts.next() {
                        if parts.peek().is_none() {
                            object[part] = value.clone();
                        } else {
                            object = object
                                .as_object_mut()
                                .unwrap()
                                .entry(part)
                                .or_insert_with(|| json!({}));
                        }
                    }
                }
            }
            record(
                i as u64 + 1,
                EventKind::Extension(ExtensionEvent::new("properties", "saved", data).unwrap()),
            )
        })
        .collect();
    append(&store, key("properties", "main"), records.clone())
        .await
        .unwrap();
    memory
        .append(
            key("properties", "main"),
            records,
            Box::new(|result| result.unwrap()),
        )
        .unwrap();
    for path in paths {
        let value = if path.starts_with("data.current.") {
            json!(1)
        } else {
            json!("completed")
        };
        let field = QueryOperand::Field { path: path.into() };
        let constant = QueryOperand::Value {
            value: value.clone(),
        };
        let mut filters = vec![QueryFilter::In {
            value: field.clone(),
            values: vec![value],
        }];
        for comparison in [
            QueryComparison::Eq,
            QueryComparison::Ne,
            QueryComparison::Lt,
            QueryComparison::Le,
            QueryComparison::Gt,
            QueryComparison::Ge,
        ] {
            filters.push(QueryFilter::Compare {
                left: field.clone(),
                comparison,
                right: constant.clone(),
            });
            filters.push(QueryFilter::Compare {
                left: constant.clone(),
                comparison,
                right: field.clone(),
            });
        }
        for filter in filters {
            for negated in [false, true] {
                for select in [
                    QuerySelect::Fields {
                        fields: vec!["id".into()],
                    },
                    QuerySelect::Events,
                    QuerySelect::Count,
                    QuerySelect::Exists,
                ] {
                    let mut query = fields(&["id"]);
                    query.filter = Some(if negated {
                        QueryFilter::Not {
                            condition: Box::new(filter.clone()),
                        }
                    } else {
                        filter.clone()
                    });
                    query.select = select;
                    parity(&store, &memory, &scope(10), &query).await;
                }
            }
        }
    }
    store.close().await.unwrap();
    db.close().await.unwrap();
}

/// A membership list on a physical column binds its members once per statement
/// role, and a role compiled only to learn whether a physical form exists must
/// not leave its parameters behind. A statement numbers every parameter it
/// binds, so a surplus over the roles that reach the SQL is a leak.
#[tokio::test]
async fn a_discarded_physical_check_leaves_no_parameter_behind() {
    let database = Database::new();
    let db = SqliteStorage::open(database.0.clone()).unwrap();
    let store = SqliteTimelineStore::new(db.clone());
    store
        .create_session("properties", &SessionConfig::default())
        .await
        .unwrap();
    let members = 996;
    let values: Vec<Value> = (0..members)
        .map(|index| json!(format!("probe/absent-{index}")))
        .collect();
    let mut records = Vec::new();
    for sequence in 1..=3 {
        records.push(record(
            sequence,
            EventKind::Extension(
                ExtensionEvent::new("probe", "saved", json!({"tag": "alpha"})).unwrap(),
            ),
        ));
    }
    append(&store, key("properties", "main"), records)
        .await
        .unwrap();
    let mut query = fields(&["position.sequence"]);
    query.filter = Some(QueryFilter::In {
        value: QueryOperand::Field {
            path: "kind".into(),
        },
        values,
    });
    // A row query binds the members twice, once in the physical list and once
    // in the filter, plus four scope values, a session, a property path and the
    // page limit. The count path needs only the physical list plus the same
    // fixed values. A discarded check used to add two more copies to the row
    // statement, which is the surplus this pins.
    let mut count = query.clone();
    count.select = QuerySelect::Count;
    for (label, query, page, expected) in [
        ("selection", query.clone(), Some(page(2)), 2 * members + 7),
        ("count", count, None, members + 5),
    ] {
        store.record_query_statistics();
        store.query(&scope(3), &query, page.as_ref()).await.unwrap();
        let statements = store.take_query_statistics();
        let statement = statements
            .iter()
            .find(|statement| {
                statement.stage == label || (label == "count" && statement.stage == "scalar")
            })
            .unwrap_or_else(|| panic!("{label} prepares a statement"));
        assert_eq!(
            statement.parameters, expected,
            "{label} binds {expected} parameters for {members} members"
        );
    }
    store.close().await.unwrap();
    db.close().await.unwrap();
}

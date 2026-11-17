//! SQL plans, statement work and memory over disposable stores.
//! Run with `cargo run --release -p dim-app-server --example property_query_cost --locked`.
use dim_agent_runtime::*;
use dim_agent_types::*;
use dim_app_server::storage::{SqliteStorage, SqliteTimelineStore, sqlite_memory};
use std::{sync::Arc, time::Instant};
#[path = "../../../xtask/src/allocation.rs"]
mod allocation;
#[global_allocator]
static ALLOCATOR: allocation::MeasuredAllocator = allocation::MeasuredAllocator;
fn eq(path: &str, value: serde_json::Value) -> QueryFilter {
    QueryFilter::Compare {
        left: QueryOperand::Field { path: path.into() },
        comparison: QueryComparison::Eq,
        right: QueryOperand::Value { value },
    }
}
async fn append(store: &SqliteTimelineStore, events: Vec<StoredEvent>) {
    let (tx, rx) = tokio::sync::oneshot::channel();
    store
        .append(
            TimelineKey {
                session_id: "probe".into(),
                branch_id: "main".into(),
            },
            events,
            Box::new(move |r| {
                let _ = tx.send(r);
            }),
        )
        .unwrap();
    rx.await.unwrap().unwrap();
}
fn record(sequence: u64, kind: EventKind) -> StoredEvent {
    StoredEvent {
        event: Arc::new(AgentEvent {
            id: format!("event-{sequence}"),
            sequence,
            create_at: sequence,
            tags: None,
            parent_call_id: None,
            kind,
        }),
        trace: None,
        batch_end: sequence,
    }
}
async fn measure(
    store: &SqliteTimelineStore,
    scope: &TimelineReadScope,
    query: &QuerySpec,
    name: &str,
    n: usize,
    page: Option<&QueryPage>,
) {
    store.record_query_statistics();
    let warm = store.query(scope, query, page).await.unwrap();
    println!(
        "{}",
        serde_json::json!({"name":name,"n":n,"statements":store.take_query_statistics()})
    );
    let _ = store.take_node_loads();
    allocation::reset_peak_now();
    let (sqlite_baseline, _) = sqlite_memory(true);
    let baseline = allocation::live();
    let now = Instant::now();
    for _ in 0..10 {
        let result = store.query(scope, query, page).await.unwrap();
        assert_eq!(
            serde_json::to_value(result.result).unwrap(),
            serde_json::to_value(&warm.result).unwrap()
        );
    }
    println!(
        "{}",
        serde_json::json!({"name":name,"n":n,"iterations":10,"elapsedMs":now.elapsed().as_secs_f64()*1000.,"peakAdditionalBytes":allocation::peak().saturating_sub(baseline),"retainedBytes":allocation::live() as i64-baseline as i64,"sqlitePeakAdditionalBytes":sqlite_memory(false).1-sqlite_baseline,"sqliteRetainedBytes":sqlite_memory(false).0-sqlite_baseline,"nodeLoads":store.take_node_loads().loads,"result":warm.result})
    );
}
#[tokio::main(flavor = "current_thread")]
async fn main() {
    for n in [100, 1_000, 5_000] {
        let path = std::env::temp_dir().join(format!(
            "dim-property-query-{}-{n}.sqlite",
            uuid::Uuid::new_v4()
        ));
        let db = SqliteStorage::open(path.clone()).unwrap();
        let store = SqliteTimelineStore::new(db.clone());
        store
            .create_session("probe", &SessionConfig::default())
            .await
            .unwrap();
        let mut records = Vec::new();
        for i in 0..n {
            for name in if i + 1 < n {
                &["start", "end"][..]
            } else {
                &["start"][..]
            } {
                records.push(record(
                    records.len() as u64 + 1,
                    EventKind::Extension(
                        ExtensionEvent::new("probe", *name, serde_json::json!({"key": i})).unwrap(),
                    ),
                ));
            }
        }
        for i in 0..n {
            let mut input =
                InputMessageData::text(if i == 0 { "input needle" } else { "input" }, None);
            input.submission_id = Some(format!("submission-{i}"));
            records.push(record(
                records.len() as u64 + 1,
                EventKind::InputMessage(input),
            ));
        }
        records.push(record(
            records.len() as u64 + 1,
            EventKind::ModelCall(ModelCallData {
                failure_chain: None,
                turn_id: "t".into(),
                request_id: "r".into(),
                configuration: None,
                prompt: None,
                request: Arc::new(ModelRequest::new(
                    "provider",
                    "model",
                    vec![Message::text(Role::User, "x".repeat(4 * 1024 * 1024))],
                )),
            }),
        ));
        let through = records.len() as u64;
        append(&store, records).await;
        let scope = TimelineReadScope {
            key: TimelineKey {
                session_id: "probe".into(),
                branch_id: "main".into(),
            },
            through,
            segments: vec![TimelineReadSegment {
                branch_id: "main".into(),
                from: 1,
                through,
                stored_through: through,
                tail: vec![],
            }],
        };
        let mut query = QuerySpec {
            filter: None,
            select: QuerySelect::Count,
            group_by: vec![],
            pick: None,
            order_by: vec![],
        };
        measure(&store, &scope, &query, "count", n, None).await;
        query.filter = Some(QueryFilter::And {
            conditions: vec![
                eq("kind", "probe/start".into()),
                QueryFilter::Not {
                    condition: Box::new(QueryFilter::Exists {
                        condition: Box::new(QueryFilter::And {
                            conditions: vec![
                                eq("kind", "probe/end".into()),
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
                },
            ],
        });
        measure(&store, &scope, &query, "unsettled", n, None).await;
        query.select = QuerySelect::Exists;
        for (name, path, value) in [
            (
                "metadata_exists_4MiB_request",
                "data.request.providerId",
                "provider",
            ),
            ("metadata_miss", "data.request_id", "missing"),
        ] {
            query.filter = Some(QueryFilter::And {
                conditions: vec![eq("kind", "model/call".into()), eq(path, value.into())],
            });
            measure(&store, &scope, &query, name, n, None).await;
        }
        for id in [format!("submission-{}", n - 1), "missing".to_owned()] {
            query.filter = Some(QueryFilter::And {
                conditions: vec![
                    eq("kind", "input/message".into()),
                    eq("data.submission_id", id.clone().into()),
                ],
            });
            let name = if id == "missing" {
                "submission_miss"
            } else {
                "submission_hit"
            };
            measure(&store, &scope, &query, name, n, None).await;
        }
        for (name, text) in [("body_hit", "xxx"), ("body_miss", "absent")] {
            query.filter = Some(QueryFilter::And {
                conditions: vec![
                    eq("kind", "model/call".into()),
                    QueryFilter::Any {
                        field: "data.request.messages".into(),
                        condition: Box::new(QueryFilter::Contains {
                            value: QueryOperand::Element {
                                path: "content".into(),
                            },
                            text: text.into(),
                        }),
                    },
                ],
            });
            measure(&store, &scope, &query, name, n, None).await;
        }
        query.select = QuerySelect::Count;
        for (name, text) in [
            ("text_candidates_sparse", "needle"),
            ("text_candidates_empty", "absent"),
        ] {
            query.filter = Some(QueryFilter::And {
                conditions: vec![
                    eq("kind", "input/message".into()),
                    QueryFilter::Any {
                        field: "data.user.content".into(),
                        condition: Box::new(QueryFilter::Contains {
                            value: QueryOperand::Element {
                                path: "text".into(),
                            },
                            text: text.into(),
                        }),
                    },
                ],
            });
            measure(&store, &scope, &query, name, n, None).await;
        }
        query.filter = Some(eq("kind", "probe/start".into()));
        query.select = QuerySelect::Fields {
            fields: vec!["id".into()],
        };
        let mut paging = QueryPage {
            limit: std::num::NonZeroU64::new(16).unwrap(),
            after: None,
        };
        query.pick = Some(QueryPick::Last);
        measure(&store, &scope, &query, "latest_control", n, Some(&paging)).await;
        query.pick = None;
        for direction in [
            TimelineQueryDirection::Ascending,
            TimelineQueryDirection::Descending,
        ] {
            query.order_by = vec![QueryOrder {
                key: QueryOrderKey::Position,
                direction,
            }];
            paging.after = None;
            let mut index = 0;
            loop {
                let result = store.query(&scope, &query, Some(&paging)).await.unwrap();
                if index < 3 || result.next.is_none() {
                    measure(
                        &store,
                        &scope,
                        &query,
                        &format!("position_{direction:?}_{index}"),
                        n,
                        Some(&paging),
                    )
                    .await;
                }
                index += 1;
                let Some(next) = result.next else { break };
                paging.after = Some(next);
            }
        }
        store.close().await.unwrap();
        db.close().await.unwrap();
        drop(store);
        drop(db);
        std::fs::remove_file(path).unwrap();
    }
}

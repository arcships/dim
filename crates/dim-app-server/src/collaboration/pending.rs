use super::*;
use dim_agent_capabilities::methods::{Interruption, MethodDecl, RpcError, SessionMethods, params};
use dim_agent_capabilities::workspace_tools::call_positions;
use dim_agent_runtime::{QueryComparison, QueryCursor, QueryOrder, QueryOrderKey, QueryPick};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Params {
    #[serde(rename = "sessionId")]
    _session_id: String,
    branch_id: String,
    through: Option<u64>,
    after: Option<QueryCursor>,
}

pub(super) fn install(
    ctx: &Ctx,
    service: &Arc<CollaborationSession>,
    methods: &SessionMethods,
) -> rutis::Result<()> {
    let tree = service.tree.clone();
    let finished_tree = tree.clone();
    methods.register(
        ctx,
        "session.collaboration.pending",
        vec![MethodDecl::new(
            "session.collaboration.pending",
            false,
            Interruption::Report,
        )],
        Arc::new(move |call, cancel, _| {
            let tree = tree.clone();
            Box::pin(async move { read(&tree, call.params, &cancel).await })
        }),
    )?;
    methods.register(
        ctx,
        "session.collaboration.finished",
        vec![MethodDecl::new(
            "session.collaboration.finished",
            false,
            Interruption::Report,
        )],
        Arc::new(move |call, cancel, _| {
            let tree = finished_tree.clone();
            Box::pin(async move { finished(&tree, call.params, &cancel).await })
        }),
    )
}

async fn read(
    tree: &TreeCommands,
    value: Value,
    cancel: &CancellationToken,
) -> std::result::Result<Value, RpcError> {
    let p: Params = params(value)?;
    let kind = |kind: &str| QueryFilter::In {
        value: QueryOperand::Field {
            path: "kind".into(),
        },
        values: vec![json!(kind)],
    };
    let mut query = TimelineQuery {
        scope: TimelineQueryScope::Branch,
        from: None,
        through: p.through,
        include_publishing: false,
        query: QuerySpec {
            filter: Some(QueryFilter::And {
                conditions: vec![
                    kind("collaboration/request"),
                    QueryFilter::Not {
                        condition: Box::new(QueryFilter::Exists {
                            condition: Box::new(QueryFilter::Or {
                                conditions: vec![
                                    QueryFilter::And {
                                        conditions: vec![
                                            kind("collaboration/result"),
                                            QueryFilter::Compare {
                                                left: QueryOperand::Field {
                                                    path: "data.request_event_id".into(),
                                                },
                                                comparison: QueryComparison::Eq,
                                                right: QueryOperand::Outer { path: "id".into() },
                                            },
                                        ],
                                    },
                                    QueryFilter::And {
                                        conditions: vec![
                                            kind("collaboration/delivered"),
                                            QueryFilter::Compare {
                                                left: QueryOperand::Field {
                                                    path: "data.op_event_id".into(),
                                                },
                                                comparison: QueryComparison::Eq,
                                                right: QueryOperand::Outer { path: "id".into() },
                                            },
                                            QueryFilter::In {
                                                value: QueryOperand::Field {
                                                    path: "data.status".into(),
                                                },
                                                values: vec![json!("error")],
                                            },
                                        ],
                                    },
                                ],
                            }),
                        }),
                    },
                ],
            }),
            select: QuerySelect::Fields {
                fields: ["id", "position.sequence", "create_at", "data"]
                    .map(String::from)
                    .to_vec(),
            },
            group_by: vec![],
            pick: None,
            order_by: vec![],
        },
        page: Some(QueryPage {
            limit: NonZeroU64::new(128).unwrap(),
            after: p.after,
        }),
    };
    let selected = tokio::select! {
        _ = cancel.cancelled() => return Err(RpcError::interrupted()),
        result = tree.query(p.branch_id.clone(), query.clone()) => result?,
    };
    let QueryOutput::Rows { rows } = selected.result else {
        unreachable!("delegation fields")
    };
    let mut requests: Vec<_> = rows
        .into_iter()
        .map(|row| {
            let [
                QueryValue::Value { value: id },
                QueryValue::Value { value: sequence },
                QueryValue::Value { value: time },
                QueryValue::Value { value: request },
            ] = row.values.as_slice()
            else {
                unreachable!("delegation fields")
            };
            PendingDelegation {
                event_id: id.as_str().expect("id").into(),
                sequence: sequence.as_u64().expect("sequence"),
                started_at: time.as_u64().expect("time"),
                request: serde_json::from_value(request.clone()).expect("stored request"),
                delivery: None,
                position: None,
            }
        })
        .collect();
    if !requests.is_empty() {
        query.through = Some(selected.through);
        query.query.filter = Some(QueryFilter::And {
            conditions: vec![
                kind("collaboration/delivered"),
                QueryFilter::In {
                    value: QueryOperand::Field {
                        path: "data.op_event_id".into(),
                    },
                    values: requests
                        .iter()
                        .map(|request| json!(request.event_id))
                        .collect(),
                },
            ],
        });
        query.query.select = QuerySelect::Fields {
            fields: vec!["data".into()],
        };
        query.query.group_by = vec!["data.op_event_id".into()];
        query.query.pick = Some(QueryPick::Last);
        query.page.as_mut().unwrap().after = None;
        loop {
            let deliveries = tokio::select! {
                _ = cancel.cancelled() => return Err(RpcError::interrupted()),
                result = tree.query(p.branch_id.clone(), query.clone()) => result?,
            };
            let QueryOutput::Rows { rows } = deliveries.result else {
                unreachable!("delivery fields")
            };
            for row in rows {
                let [QueryValue::Value { value }] = row.values.as_slice() else {
                    unreachable!("delivery")
                };
                let delivered: CollaborationDeliveredData =
                    serde_json::from_value(value.clone()).expect("stored delivery");
                let request = requests
                    .iter_mut()
                    .find(|request| request.event_id == delivered.op_event_id)
                    .expect("selected identity");
                request.delivery = Some(delivered.delivery);
            }
            let Some(next) = deliveries.next else {
                break;
            };
            query.page.as_mut().unwrap().after = Some(next);
        }
    }
    let positions = call_positions(
        tree,
        &p.branch_id,
        selected.through,
        requests
            .iter()
            .map(|request| request.request.request_id.clone()),
        cancel,
    )
    .await?;
    for request in &mut requests {
        request.position = positions
            .get(&(
                request.request.request_id.clone(),
                request.request.call_id.clone(),
            ))
            .copied();
    }
    Ok(
        json!({"branchId":p.branch_id,"through":selected.through,"requests":requests,"next":selected.next}),
    )
}

/// The most settled delegations [`finished`] returns: the task list keeps
/// this many.
const FINISHED_LIMIT: u64 = 50;

/// The newest [`FINISHED_LIMIT`] terminal facts at the cutoff — a
/// `collaboration/result` or a failed `collaboration/delivered` — each with
/// its request and that request's latest delivery, newest end first.
async fn finished(
    tree: &TreeCommands,
    value: Value,
    cancel: &CancellationToken,
) -> std::result::Result<Value, RpcError> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct Params {
        #[serde(rename = "sessionId")]
        _session_id: String,
        branch_id: String,
        through: Option<u64>,
    }
    let p: Params = params(value)?;
    let kind = |kind: &str| QueryFilter::In {
        value: QueryOperand::Field {
            path: "kind".into(),
        },
        values: vec![json!(kind)],
    };
    let mut query = TimelineQuery {
        scope: TimelineQueryScope::Branch,
        from: None,
        through: p.through,
        include_publishing: false,
        query: QuerySpec {
            filter: Some(QueryFilter::Or {
                conditions: vec![
                    kind("collaboration/result"),
                    QueryFilter::And {
                        conditions: vec![
                            kind("collaboration/delivered"),
                            QueryFilter::In {
                                value: QueryOperand::Field {
                                    path: "data.status".into(),
                                },
                                values: vec![json!("error")],
                            },
                        ],
                    },
                ],
            }),
            select: QuerySelect::Fields {
                fields: ["kind", "create_at", "data"].map(String::from).to_vec(),
            },
            group_by: vec![],
            pick: None,
            order_by: vec![QueryOrder {
                key: QueryOrderKey::Position,
                direction: dim_agent_runtime::TimelineQueryDirection::Descending,
            }],
        },
        page: Some(QueryPage {
            limit: NonZeroU64::new(FINISHED_LIMIT).unwrap(),
            after: None,
        }),
    };
    let ends = tokio::select! {
        _ = cancel.cancelled() => return Err(RpcError::interrupted()),
        result = tree.query(p.branch_id.clone(), query.clone()) => result?,
    };
    let through = ends.through;
    let QueryOutput::Rows { rows } = ends.result else {
        unreachable!("end fields")
    };
    let ends: Vec<(String, u64, Option<CollaborationResultData>)> = rows
        .into_iter()
        .map(|row| {
            let [
                QueryValue::Value { value: kind },
                QueryValue::Value { value: at },
                QueryValue::Value { value: data },
            ] = row.values.as_slice()
            else {
                unreachable!("end fields")
            };
            let at = at.as_u64().expect("time");
            if kind == "collaboration/result" {
                let result: CollaborationResultData =
                    serde_json::from_value(data.clone()).expect("stored result");
                (result.request_event_id.clone(), at, Some(result))
            } else {
                let delivered: CollaborationDeliveredData =
                    serde_json::from_value(data.clone()).expect("stored delivery");
                (delivered.op_event_id, at, None)
            }
        })
        .collect();
    let mut delegations = Vec::new();
    if !ends.is_empty() {
        let ids: Vec<Value> = ends.iter().map(|(id, _, _)| json!(id)).collect();
        query.through = Some(through);
        query.query.filter = Some(QueryFilter::And {
            conditions: vec![
                kind("collaboration/request"),
                QueryFilter::In {
                    value: QueryOperand::Field { path: "id".into() },
                    values: ids.clone(),
                },
            ],
        });
        query.query.select = QuerySelect::Fields {
            fields: ["id", "position.sequence", "create_at", "data"]
                .map(String::from)
                .to_vec(),
        };
        query.query.order_by = vec![];
        let selected = tokio::select! {
            _ = cancel.cancelled() => return Err(RpcError::interrupted()),
            result = tree.query(p.branch_id.clone(), query.clone()) => result?,
        };
        let QueryOutput::Rows { rows } = selected.result else {
            unreachable!("delegation fields")
        };
        let mut requests: std::collections::HashMap<String, PendingDelegation> = rows
            .into_iter()
            .map(|row| {
                let [
                    QueryValue::Value { value: id },
                    QueryValue::Value { value: sequence },
                    QueryValue::Value { value: time },
                    QueryValue::Value { value: request },
                ] = row.values.as_slice()
                else {
                    unreachable!("delegation fields")
                };
                let id: String = id.as_str().expect("id").into();
                (
                    id.clone(),
                    PendingDelegation {
                        event_id: id,
                        sequence: sequence.as_u64().expect("sequence"),
                        started_at: time.as_u64().expect("time"),
                        request: serde_json::from_value(request.clone()).expect("stored request"),
                        delivery: None,
                        position: None,
                    },
                )
            })
            .collect();
        query.query.filter = Some(QueryFilter::And {
            conditions: vec![
                kind("collaboration/delivered"),
                QueryFilter::In {
                    value: QueryOperand::Field {
                        path: "data.op_event_id".into(),
                    },
                    values: ids,
                },
            ],
        });
        query.query.select = QuerySelect::Fields {
            fields: vec!["data".into()],
        };
        query.query.group_by = vec!["data.op_event_id".into()];
        query.query.pick = Some(QueryPick::Last);
        loop {
            let deliveries = tokio::select! {
                _ = cancel.cancelled() => return Err(RpcError::interrupted()),
                result = tree.query(p.branch_id.clone(), query.clone()) => result?,
            };
            let QueryOutput::Rows { rows } = deliveries.result else {
                unreachable!("delivery fields")
            };
            for row in rows {
                let [QueryValue::Value { value }] = row.values.as_slice() else {
                    unreachable!("delivery")
                };
                let delivered: CollaborationDeliveredData =
                    serde_json::from_value(value.clone()).expect("stored delivery");
                if let Some(request) = requests.get_mut(&delivered.op_event_id) {
                    request.delivery = Some(delivered.delivery);
                }
            }
            let Some(next) = deliveries.next else {
                break;
            };
            query.page.as_mut().unwrap().after = Some(next);
        }
        let positions = call_positions(
            tree,
            &p.branch_id,
            through,
            requests
                .values()
                .map(|request| request.request.request_id.clone()),
            cancel,
        )
        .await?;
        for request in requests.values_mut() {
            request.position = positions
                .get(&(
                    request.request.request_id.clone(),
                    request.request.call_id.clone(),
                ))
                .copied();
        }
        for (id, ended_at, result) in ends {
            if let Some(start) = requests.remove(&id) {
                delegations.push(FinishedDelegation {
                    start,
                    ended_at,
                    result,
                });
            }
        }
    }
    Ok(json!({"branchId":p.branch_id,"through":through,"requests":delegations}))
}

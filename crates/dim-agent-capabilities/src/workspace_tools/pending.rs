use crate::{
    methods::{RpcError, params},
    timeline_tree::TreeCommands,
};
use dim_agent_runtime::*;
use dim_agent_types::{ExecEndedData, ExecStartedData, FinishedExec, PendingExec};
use serde_json::{Value, json};
use std::num::NonZeroU64;
use tokio_util::sync::CancellationToken;

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Params {
    #[serde(rename = "sessionId")]
    _session_id: String,
    branch_id: String,
    through: Option<u64>,
    after: Option<QueryCursor>,
}

pub(super) async fn read(
    tree: &TreeCommands,
    value: Value,
    cancel: &CancellationToken,
) -> std::result::Result<Value, RpcError> {
    let p: Params = params(value)?;
    let query = TimelineQuery {
        scope: TimelineQueryScope::Branch,
        from: None,
        through: p.through,
        include_publishing: false,
        query: QuerySpec {
            filter: Some(QueryFilter::And {
                conditions: vec![
                    QueryFilter::In {
                        value: QueryOperand::Field {
                            path: "kind".into(),
                        },
                        values: vec![json!("exec/started")],
                    },
                    QueryFilter::Not {
                        condition: Box::new(QueryFilter::Exists {
                            condition: Box::new(QueryFilter::And {
                                conditions: vec![
                                    QueryFilter::In {
                                        value: QueryOperand::Field {
                                            path: "kind".into(),
                                        },
                                        values: vec![json!("exec/ended")],
                                    },
                                    QueryFilter::Compare {
                                        left: QueryOperand::Field {
                                            path: "data.task_id".into(),
                                        },
                                        comparison: QueryComparison::Eq,
                                        right: QueryOperand::Outer {
                                            path: "data.task_id".into(),
                                        },
                                    },
                                ],
                            }),
                        }),
                    },
                ],
            }),
            select: QuerySelect::Fields {
                fields: [
                    "id",
                    "position.sequence",
                    "create_at",
                    "data.task_id",
                    "data.turn_id",
                    "data.request_id",
                    "data.call_id",
                    "data.description",
                    "data.title",
                ]
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
        result = tree.query(p.branch_id.clone(), query) => result?,
    };
    let QueryOutput::Rows { rows } = selected.result else {
        unreachable!("task fields")
    };
    let tasks: Vec<PendingExec> = rows
        .into_iter()
        .map(|mut row| {
            let title = match row.values.pop().expect("title field") {
                QueryValue::Missing => None,
                QueryValue::Value { value } => serde_json::from_value(value).expect("stored title"),
                QueryValue::Elements { .. } => unreachable!("title is a scalar field"),
            };
            let values: Vec<_> = row
                .values
                .into_iter()
                .map(|value| {
                    let QueryValue::Value { value } = value else {
                        unreachable!("task field")
                    };
                    value
                })
                .collect();
            PendingExec {
                event_id: values[0].as_str().expect("event id").into(),
                sequence: values[1].as_u64().expect("sequence"),
                started_at: values[2].as_u64().expect("time"),
                task_id: values[3].as_str().expect("task id").into(),
                turn_id: values[4].as_str().expect("turn id").into(),
                request_id: values[5].as_str().expect("request id").into(),
                call_id: values[6].as_str().expect("call id").into(),
                description: values[7].as_str().expect("description").into(),
                title,
                position: None,
            }
        })
        .collect();
    let mut tasks = tasks;
    let positions = call_positions(
        tree,
        &p.branch_id,
        selected.through,
        tasks.iter().map(|task| task.request_id.clone()),
        cancel,
    )
    .await?;
    for task in &mut tasks {
        task.position = positions
            .get(&(task.request_id.clone(), task.call_id.clone()))
            .copied();
    }
    Ok(
        json!({"branchId":p.branch_id,"through":selected.through,"tasks":tasks,"next":selected.next}),
    )
}

/// The newest [`FINISHED_LIMIT`] `exec/ended` facts at the cutoff, each with
/// the `exec/started` it ends, newest end first.
pub(super) async fn finished(
    tree: &TreeCommands,
    value: Value,
    cancel: &CancellationToken,
) -> std::result::Result<Value, RpcError> {
    #[derive(serde::Deserialize)]
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
            filter: Some(kind("exec/ended")),
            select: QuerySelect::Fields {
                fields: vec!["create_at".into(), "data".into()],
            },
            group_by: vec![],
            pick: None,
            order_by: vec![QueryOrder {
                key: QueryOrderKey::Position,
                direction: TimelineQueryDirection::Descending,
            }],
        },
        page: Some(QueryPage {
            limit: NonZeroU64::new(FINISHED_LIMIT).unwrap(),
            after: None,
        }),
    };
    let ended = tokio::select! {
        _ = cancel.cancelled() => return Err(RpcError::interrupted()),
        result = tree.query(p.branch_id.clone(), query.clone()) => result?,
    };
    let through = ended.through;
    let QueryOutput::Rows { rows } = ended.result else {
        unreachable!("end fields")
    };
    let ends: Vec<(u64, ExecEndedData)> = rows
        .into_iter()
        .map(|row| {
            let [
                QueryValue::Value { value: at },
                QueryValue::Value { value: data },
            ] = row.values.as_slice()
            else {
                unreachable!("end fields")
            };
            (
                at.as_u64().expect("time"),
                serde_json::from_value(data.clone()).expect("stored exec end"),
            )
        })
        .collect();
    let mut tasks = Vec::new();
    if !ends.is_empty() {
        query.through = Some(through);
        query.query.filter = Some(QueryFilter::And {
            conditions: vec![
                kind("exec/started"),
                QueryFilter::In {
                    value: QueryOperand::Field {
                        path: "data.task_id".into(),
                    },
                    values: ends.iter().map(|(_, end)| json!(end.task_id)).collect(),
                },
            ],
        });
        query.query.select = QuerySelect::Fields {
            fields: ["id", "position.sequence", "create_at", "data"]
                .map(String::from)
                .to_vec(),
        };
        query.query.order_by = vec![];
        let started = tokio::select! {
            _ = cancel.cancelled() => return Err(RpcError::interrupted()),
            result = tree.query(p.branch_id.clone(), query) => result?,
        };
        let QueryOutput::Rows { rows } = started.result else {
            unreachable!("start fields")
        };
        let mut starts: std::collections::HashMap<String, PendingExec> = rows
            .into_iter()
            .map(|row| {
                let [
                    QueryValue::Value { value: id },
                    QueryValue::Value { value: sequence },
                    QueryValue::Value { value: time },
                    QueryValue::Value { value: data },
                ] = row.values.as_slice()
                else {
                    unreachable!("start fields")
                };
                let data: ExecStartedData =
                    serde_json::from_value(data.clone()).expect("stored exec start");
                (
                    data.task_id.clone(),
                    PendingExec {
                        event_id: id.as_str().expect("id").into(),
                        sequence: sequence.as_u64().expect("sequence"),
                        started_at: time.as_u64().expect("time"),
                        task_id: data.task_id,
                        turn_id: data.turn_id,
                        request_id: data.request_id,
                        call_id: data.call_id,
                        description: data.description,
                        title: data.title,
                        position: None,
                    },
                )
            })
            .collect();
        let positions = call_positions(
            tree,
            &p.branch_id,
            through,
            starts.values().map(|start| start.request_id.clone()),
            cancel,
        )
        .await?;
        for start in starts.values_mut() {
            start.position = positions
                .get(&(start.request_id.clone(), start.call_id.clone()))
                .copied();
        }
        for (ended_at, end) in ends {
            if let Some(start) = starts.remove(&end.task_id) {
                tasks.push(FinishedExec {
                    start,
                    ended_at,
                    end,
                });
            }
        }
    }
    Ok(json!({"branchId":p.branch_id,"through":through,"tasks":tasks}))
}

/// Each call's index in the `step/tool-call` of its model request, for the
/// requests `requests` names whose response made several calls: the order of
/// the tasks one parallel batch dispatched.
pub async fn call_positions(
    tree: &TreeCommands,
    branch_id: &str,
    through: u64,
    requests: impl IntoIterator<Item = String>,
    cancel: &CancellationToken,
) -> std::result::Result<std::collections::HashMap<(String, String), u32>, RpcError> {
    let requests: std::collections::BTreeSet<String> = requests.into_iter().collect();
    let mut positions = std::collections::HashMap::new();
    if requests.is_empty() {
        return Ok(positions);
    }
    let mut query = TimelineQuery {
        scope: TimelineQueryScope::Branch,
        from: None,
        through: Some(through),
        include_publishing: false,
        query: QuerySpec {
            filter: Some(QueryFilter::And {
                conditions: vec![
                    QueryFilter::In {
                        value: QueryOperand::Field {
                            path: "kind".into(),
                        },
                        values: vec![json!("step/tool-call")],
                    },
                    QueryFilter::In {
                        value: QueryOperand::Field {
                            path: "data.request_id".into(),
                        },
                        values: requests.iter().map(|id| json!(id)).collect(),
                    },
                ],
            }),
            select: QuerySelect::Fields {
                fields: vec!["data.request_id".into(), "data.calls[].id".into()],
            },
            group_by: vec![],
            pick: None,
            order_by: vec![],
        },
        page: Some(QueryPage {
            limit: NonZeroU64::new(128).unwrap(),
            after: None,
        }),
    };
    loop {
        let page = tokio::select! {
            _ = cancel.cancelled() => return Err(RpcError::interrupted()),
            result = tree.query(branch_id.to_owned(), query.clone()) => result?,
        };
        let QueryOutput::Rows { rows } = page.result else {
            unreachable!("call fields")
        };
        for row in rows {
            let [
                QueryValue::Value { value: request },
                QueryValue::Elements { values },
            ] = row.values.as_slice()
            else {
                continue;
            };
            if values.len() < 2 {
                continue;
            }
            let request = request.as_str().expect("request id");
            for (index, value) in values.iter().enumerate() {
                if let QueryValue::Value { value } = value
                    && let Some(call) = value.as_str()
                {
                    positions.insert((request.to_owned(), call.to_owned()), index as u32);
                }
            }
        }
        let Some(next) = page.next else {
            return Ok(positions);
        };
        query.page.as_mut().unwrap().after = Some(next);
    }
}

/// The most finished tasks [`finished`] returns: the task list keeps this many.
const FINISHED_LIMIT: u64 = 50;

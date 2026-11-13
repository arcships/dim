use crate::{
    methods::{RpcError, params},
    timeline_tree::TreeCommands,
};
use dim_agent_runtime::*;
use dim_agent_types::PendingExec;
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
    let tasks: Vec<_> = rows
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
            }
        })
        .collect();
    Ok(
        json!({"branchId":p.branch_id,"through":selected.through,"tasks":tasks,"next":selected.next}),
    )
}

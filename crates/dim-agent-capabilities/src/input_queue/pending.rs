use super::*;
use dim_agent_runtime::{
    QueryComparison, QueryCursor, QueryFilter, QueryOperand, QueryOutput, QueryPage, QuerySelect,
    QuerySpec, QueryValue, TimelineQuery, TimelineQueryScope,
};
use serde_json::{Value, json};
use std::num::NonZeroU64;

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
    let mut query = TimelineQuery {
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
                        values: vec![json!("input/message")],
                    },
                    QueryFilter::Not {
                        condition: Box::new(QueryFilter::Present {
                            value: QueryOperand::Field {
                                path: "data.handover".into(),
                            },
                        }),
                    },
                    QueryFilter::Not {
                        condition: Box::new(QueryFilter::Exists {
                            condition: Box::new(QueryFilter::And {
                                conditions: vec![
                                    QueryFilter::In {
                                        value: QueryOperand::Field {
                                            path: "kind".into(),
                                        },
                                        values: vec![
                                            json!("input/consumed"),
                                            json!("input/deleted"),
                                        ],
                                    },
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
            }),
            select: QuerySelect::Fields {
                fields: ["id", "position.sequence", "data.mode", "data.user"]
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
        unreachable!("input fields")
    };
    let mut inputs = Vec::with_capacity(rows.len());
    for row in rows {
        let [
            QueryValue::Value { value: id },
            QueryValue::Value { value: sequence },
            mode,
            user,
        ] = row.values.as_slice()
        else {
            unreachable!("input identity")
        };
        let user = match user {
            QueryValue::Value { value } if !value.is_null() => {
                Some(serde_json::from_value::<UserInput>(value.clone()).expect("stored user input"))
            }
            _ => None,
        };
        let mode = match mode {
            QueryValue::Value { value } if !value.is_null() => {
                serde_json::from_value(value.clone()).expect("stored input mode")
            }
            _ if user.is_some() => InputMode::Queue,
            _ => InputMode::Append,
        };
        inputs.push(PendingInput {
            input_id: id.as_str().expect("input id").into(),
            sequence: sequence.as_u64().expect("input sequence"),
            mode,
            user,
            applied: false,
        });
    }
    if !inputs.is_empty() {
        query.through = Some(selected.through);
        query.query.filter = Some(QueryFilter::And {
            conditions: vec![
                QueryFilter::In {
                    value: QueryOperand::Field {
                        path: "kind".into(),
                    },
                    values: vec![json!("input/applied")],
                },
                QueryFilter::In {
                    value: QueryOperand::Field {
                        path: "data.input_id".into(),
                    },
                    values: inputs.iter().map(|input| json!(input.input_id)).collect(),
                },
            ],
        });
        query.query.select = QuerySelect::Fields {
            fields: vec!["data.input_id".into()],
        };
        query.page.as_mut().unwrap().after = None;
        loop {
            let applied = tokio::select! {
                _ = cancel.cancelled() => return Err(RpcError::interrupted()),
                result = tree.query(p.branch_id.clone(), query.clone()) => result?,
            };
            let QueryOutput::Rows { rows } = applied.result else {
                unreachable!("applied input identities")
            };
            for row in rows {
                let [QueryValue::Value { value }] = row.values.as_slice() else {
                    unreachable!("applied input identity")
                };
                if let Some(input) = inputs.iter_mut().find(|input| value == &input.input_id) {
                    input.applied = true;
                }
            }
            let Some(next) = applied.next else {
                break;
            };
            query.page.as_mut().unwrap().after = Some(next);
        }
    }
    Ok(
        json!({"branchId":p.branch_id,"through":selected.through,"inputs":inputs,"next":selected.next}),
    )
}

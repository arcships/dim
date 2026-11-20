use super::*;
use dim_agent_runtime::{
    QueryComparison, QueryCursor, QueryFilter, QueryOperand, QueryOutput, QueryPage, QuerySelect,
    QuerySpec, QueryValue, TimelineQuery, TimelineQueryScope,
};
use dim_agent_types::PendingQuestionBatch;
use std::num::NonZeroU64;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Params {
    #[serde(rename = "sessionId")]
    _session_id: String,
    branch_id: String,
    through: Option<u64>,
    after: Option<QueryCursor>,
}

pub(super) async fn read(
    tree: &TimelineTree,
    value: Value,
    cancel: &CancellationToken,
) -> Result<Value, RpcError> {
    if cancel.is_cancelled() {
        return Err(RpcError::interrupted());
    }
    let p: Params = params(value)?;
    let equal = |path: &str, value| QueryFilter::Compare {
        left: QueryOperand::Field { path: path.into() },
        comparison: QueryComparison::Eq,
        right: QueryOperand::Value { value },
    };
    let selected = tree
        .query(
            p.branch_id.clone(),
            TimelineQuery {
                scope: TimelineQueryScope::Branch,
                from: None,
                through: p.through,
                include_publishing: false,
                query: QuerySpec {
                    filter: Some(QueryFilter::And {
                        conditions: vec![
                            equal("kind", json!("step/tool-call")),
                            QueryFilter::Any {
                                field: "data.calls".into(),
                                condition: Box::new(QueryFilter::Compare {
                                    left: QueryOperand::Element {
                                        path: "name".into(),
                                    },
                                    comparison: QueryComparison::Eq,
                                    right: QueryOperand::Value { value: json!(TOOL) },
                                }),
                            },
                            QueryFilter::Not {
                                condition: Box::new(QueryFilter::Exists {
                                    condition: Box::new(QueryFilter::And {
                                        conditions: vec![
                                            equal("kind", json!("step/tool-result")),
                                            QueryFilter::Compare {
                                                left: QueryOperand::Field {
                                                    path: "data.call_event_id".into(),
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
                        fields: [
                            "id",
                            "position.sequence",
                            "data.request_id",
                            "data.calls[].name",
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
            },
        )
        .await?;
    let QueryOutput::Rows { rows } = selected.result else {
        unreachable!("question fields")
    };
    let mut batches = Vec::with_capacity(rows.len());
    for row in rows {
        if cancel.is_cancelled() {
            return Err(RpcError::interrupted());
        }
        let [
            QueryValue::Value { value: id },
            QueryValue::Value { value: sequence },
            QueryValue::Value { value: request_id },
            QueryValue::Elements { values: names },
        ] = row.values.as_slice()
        else {
            unreachable!("tool-call identity and names")
        };
        let fields = names
            .iter()
            .enumerate()
            .filter(|(_, name)| matches!(name, QueryValue::Value { value } if value == TOOL))
            .map(|(index, _)| format!("data.calls[{index}]"))
            .collect();
        let body = tree
            .query(
                p.branch_id.clone(),
                TimelineQuery {
                    scope: TimelineQueryScope::Branch,
                    from: None,
                    through: Some(selected.through),
                    include_publishing: false,
                    query: QuerySpec {
                        filter: Some(equal("id", id.clone())),
                        select: QuerySelect::Fields { fields },
                        group_by: vec![],
                        pick: None,
                        order_by: vec![],
                    },
                    page: Some(QueryPage {
                        limit: NonZeroU64::MIN,
                        after: None,
                    }),
                },
            )
            .await?;
        let QueryOutput::Rows { rows } = body.result else {
            unreachable!("question arguments")
        };
        let calls = rows
            .into_iter()
            .next()
            .expect("selected batch at fixed cut")
            .values
            .into_iter()
            .map(|value| {
                let QueryValue::Value { value } = value else {
                    unreachable!("selected tool call")
                };
                serde_json::from_value(value).expect("ToolCall property")
            })
            .collect();
        batches.push(PendingQuestionBatch {
            event_id: id.as_str().expect("event id").into(),
            sequence: sequence.as_u64().expect("sequence"),
            request_id: request_id.as_str().expect("request id").into(),
            calls,
        });
    }
    Ok(
        json!({"branchId":p.branch_id, "through":selected.through, "batches":batches, "next":selected.next}),
    )
}

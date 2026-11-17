use dim_agent_capabilities::workspace_tools::background::unposted_filter;
use dim_agent_runtime::{
    AgentError, QueryComparison, QueryFilter, QueryOperand, QueryOutput, QueryPage, QueryPick,
    QuerySelect, QuerySpec, Result, Session, TimelineQuery, TimelineQueryScope,
};
use dim_agent_types::{AgentEvent, EventKind};
use std::{num::NonZeroU64, sync::Arc};

pub(super) async fn validate_rest_point(session: &Session, at: u64) -> Result<()> {
    if read_one(
        session,
        at,
        in_field("kind", &["turn/start", "turn/end"]),
        QueryPick::Last,
    )
    .await?
    .is_some_and(|event| matches!(event.kind, EventKind::TurnStart(_)))
    {
        return reject("a turn is open");
    }
    let interrupted_at = read_one(
        session,
        at,
        in_field("kind", &["stop/interrupt"]),
        QueryPick::Last,
    )
    .await?
    .map_or(0, |event| event.sequence);
    let pending = QueryFilter::Or {
        conditions: [false, true]
            .into_iter()
            .map(|handover| {
                let present = QueryFilter::Present {
                    value: QueryOperand::Field {
                        path: "data.handover".into(),
                    },
                };
                QueryFilter::And {
                    conditions: vec![
                        in_field("kind", &["input/message"]),
                        if handover {
                            present
                        } else {
                            QueryFilter::Not {
                                condition: Box::new(present),
                            }
                        },
                        QueryFilter::Not {
                            condition: Box::new(QueryFilter::Exists {
                                condition: Box::new(input_relation(&[
                                    "input/consumed",
                                    "input/deleted",
                                ])),
                            }),
                        },
                        QueryFilter::Or {
                            conditions: vec![
                                in_field("data.mode", &["queue", "steer"]),
                                QueryFilter::And {
                                    conditions: vec![
                                        QueryFilter::Not {
                                            condition: Box::new(QueryFilter::Present {
                                                value: QueryOperand::Field {
                                                    path: "data.mode".into(),
                                                },
                                            }),
                                        },
                                        QueryFilter::Present {
                                            value: QueryOperand::Field {
                                                path: "data.user".into(),
                                            },
                                        },
                                        QueryFilter::Not {
                                            condition: Box::new(QueryFilter::In {
                                                value: QueryOperand::Field {
                                                    path: "data.user".into(),
                                                },
                                                values: vec![serde_json::Value::Null],
                                            }),
                                        },
                                    ],
                                },
                            ],
                        },
                        QueryFilter::Or {
                            conditions: vec![
                                QueryFilter::Not {
                                    condition: Box::new(QueryFilter::Exists {
                                        condition: Box::new(input_relation(&["input/applied"])),
                                    }),
                                },
                                QueryFilter::Exists {
                                    condition: Box::new(QueryFilter::And {
                                        conditions: vec![
                                            input_relation(&["input/applied"]),
                                            QueryFilter::Compare {
                                                left: QueryOperand::Field {
                                                    path: "position.sequence".into(),
                                                },
                                                comparison: QueryComparison::Gt,
                                                right: QueryOperand::Value {
                                                    value: interrupted_at.into(),
                                                },
                                            },
                                        ],
                                    }),
                                },
                            ],
                        },
                    ],
                }
            })
            .collect(),
    };
    if read_one(session, at, pending, QueryPick::First)
        .await?
        .is_some()
    {
        return reject("an input is waiting to be consumed");
    }
    let running = QueryFilter::And {
        conditions: vec![
            in_field("kind", &["exec/started"]),
            QueryFilter::Not {
                condition: Box::new(QueryFilter::Exists {
                    condition: Box::new(QueryFilter::And {
                        conditions: vec![
                            in_field("kind", &["exec/ended"]),
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
    };
    if read_one(session, at, running, QueryPick::First)
        .await?
        .is_some()
    {
        return reject("a background task is running");
    }
    if read_one(session, at, unposted_filter(), QueryPick::First)
        .await?
        .is_some()
    {
        return reject("a background task has not woken the branch");
    }
    Ok(())
}

fn reject(cause: &str) -> Result<()> {
    Err(AgentError::InvalidInput(format!(
        "the fork position is not a rest point: {cause}"
    )))
}

fn in_field(path: &str, values: &[&str]) -> QueryFilter {
    QueryFilter::In {
        value: QueryOperand::Field { path: path.into() },
        values: values.iter().map(|value| (*value).into()).collect(),
    }
}

fn input_relation(kinds: &[&str]) -> QueryFilter {
    QueryFilter::And {
        conditions: vec![
            in_field("kind", kinds),
            QueryFilter::Compare {
                left: QueryOperand::Field {
                    path: "data.input_id".into(),
                },
                comparison: QueryComparison::Eq,
                right: QueryOperand::Outer { path: "id".into() },
            },
        ],
    }
}

async fn read_one(
    session: &Session,
    at: u64,
    filter: QueryFilter,
    pick: QueryPick,
) -> Result<Option<Arc<AgentEvent>>> {
    let reply = session
        .query(
            "main",
            TimelineQuery {
                scope: TimelineQueryScope::Branch,
                from: None,
                through: Some(at),
                include_publishing: false,
                query: QuerySpec {
                    filter: Some(filter),
                    select: QuerySelect::Events,
                    group_by: vec![],
                    pick: Some(pick),
                    order_by: vec![],
                },
                page: Some(QueryPage {
                    limit: NonZeroU64::MIN,
                    after: None,
                }),
            },
        )
        .await?;
    let QueryOutput::Events { events } = reply.result else {
        unreachable!("event selection returns events")
    };
    Ok(events.into_iter().next().map(|row| row.event))
}

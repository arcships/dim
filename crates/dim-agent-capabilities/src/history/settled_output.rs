use super::*;
use dim_agent_runtime::{QueryPick, Result};

fn field(path: &str, values: Vec<serde_json::Value>) -> QueryFilter {
    QueryFilter::In {
        value: QueryOperand::Field { path: path.into() },
        values,
    }
}

fn query(request_id: &str, through: u64, kinds: &[&str]) -> TimelineQuery {
    TimelineQuery {
        scope: TimelineQueryScope::Chain,
        from: None,
        through: Some(through),
        include_publishing: false,
        query: QuerySpec {
            filter: Some(QueryFilter::And {
                conditions: vec![
                    field("kind", kinds.iter().map(|kind| (*kind).into()).collect()),
                    field("data.request_id", vec![request_id.into()]),
                ],
            }),
            select: QuerySelect::Fields {
                fields: vec!["id".into(), "position".into(), "kind".into()],
            },
            group_by: vec![],
            pick: None,
            order_by: vec![],
        },
        page: Some(QueryPage {
            limit: NonZeroU64::new(128).unwrap(),
            after: None,
        }),
    }
}

fn successor(
    segments: &[(String, u64)],
    mut position: TimelinePosition,
) -> Option<TimelinePosition> {
    let index = segments
        .iter()
        .position(|(branch, _)| *branch == position.branch_id)
        .expect("query position is in scope");
    if position.sequence < segments[index].1 {
        position.sequence += 1;
        Some(position)
    } else {
        segments[index + 1..]
            .iter()
            .find(|(_, end)| *end > 0)
            .map(|(branch, _)| TimelinePosition {
                branch_id: branch.clone(),
                sequence: 1,
            })
    }
}

/// Reads only the last complete output and subsequent fragments of this
/// settlement. A legacy tool result flushes an output only while it is open.
pub(crate) async fn settled_request_output(
    timeline: &Timeline,
    request_id: &str,
    through: u64,
    before: &TimelinePosition,
    completed: bool,
    legacy_flush: bool,
) -> Result<Option<Message>> {
    let through = if before.branch_id == timeline.branch_id() {
        through.min(before.sequence.saturating_sub(1))
    } else {
        through
    };
    let segments = timeline.segments(through)?;
    let before_rank = segments
        .iter()
        .position(|(branch, _)| *branch == before.branch_id)
        .ok_or_else(|| {
            AgentError::fault("History", "output settlement is outside the branch chain")
        })?;
    let query = |request_id: &str, through: u64, kinds: &[&str]| {
        let mut query = query(request_id, through, kinds);
        let mut alternatives = Vec::new();
        for (rank, (branch, _)) in segments.iter().enumerate().take(before_rank + 1) {
            let mut conditions = vec![field("position.branch_id", vec![branch.clone().into()])];
            if rank == before_rank {
                conditions.push(QueryFilter::Compare {
                    left: QueryOperand::Field {
                        path: "position.sequence".into(),
                    },
                    comparison: dim_agent_runtime::QueryComparison::Lt,
                    right: QueryOperand::Value {
                        value: before.sequence.into(),
                    },
                });
            }
            alternatives.push(QueryFilter::And { conditions });
        }
        query.query.filter = Some(QueryFilter::And {
            conditions: vec![
                query.query.filter.take().expect("request output kind"),
                QueryFilter::Or {
                    conditions: alternatives,
                },
            ],
        });
        query
    };
    let mut controls = Vec::new();
    for kinds in [
        &["step/model-start", "step/model-output-end"][..],
        &["step/tool-result"][..],
        &["branch/start"][..],
    ] {
        let mut boundary = query(request_id, through, kinds);
        if kinds == ["branch/start"] {
            let mut reset = query("", through, &["branch/start"]);
            if let Some(QueryFilter::And { conditions }) = &mut reset.query.filter {
                conditions[0] = field("kind", vec!["branch/start".into()]);
            }
            boundary = reset;
        } else if kinds == ["step/tool-result"] {
            boundary.query.filter = Some(QueryFilter::And {
                conditions: vec![
                    boundary.query.filter.take().expect("output controls"),
                    QueryFilter::Not {
                        condition: Box::new(QueryFilter::Present {
                            value: QueryOperand::Field {
                                path: "data.call_event_id".into(),
                            },
                        }),
                    },
                ],
            });
        }
        boundary.query.pick = Some(QueryPick::Last);
        boundary.page.as_mut().unwrap().limit = NonZeroU64::MIN;
        let QueryOutput::Rows { rows } = timeline.query(boundary).await?.result else {
            unreachable!("output controls")
        };
        for row in rows {
            let [
                _,
                QueryValue::Value { value: position },
                QueryValue::Value { value: kind },
            ] = <[_; 3]>::try_from(row.values).expect("output control fields")
            else {
                unreachable!("output identity")
            };
            let position: TimelinePosition = serde_json::from_value(position)
                .map_err(|error| AgentError::fault("History", error.to_string()))?;
            let rank = segments
                .iter()
                .position(|(branch, _)| *branch == position.branch_id)
                .expect("query position is in scope");
            controls.push(((rank, position.sequence), position, kind));
        }
    }
    let mut begin = None;
    if let Some((_, position, kind)) = controls.into_iter().max_by_key(|(key, _, _)| *key) {
        if legacy_flush && kind.as_str() != Some("step/model-start") {
            return Ok(None);
        }
        let Some(position) = successor(&segments, position) else {
            return Ok(settled_output_message(Vec::new(), completed));
        };
        begin = Some(position);
    } else if legacy_flush {
        return Ok(None);
    }

    let mut parts = Vec::new();
    let mut full = query(request_id, through, &["step/model-content"]);
    full.from = begin.clone();
    full.query.pick = Some(QueryPick::Last);
    full.query.select = QuerySelect::Fields {
        fields: vec!["data.content".into(), "position".into()],
    };
    full.page.as_mut().unwrap().limit = NonZeroU64::MIN;
    let QueryOutput::Rows { rows } = timeline.query(full).await?.result else {
        unreachable!("complete output")
    };
    if let Some(row) = rows.into_iter().next() {
        let [
            QueryValue::Value { value: content },
            QueryValue::Value { value: position },
        ] = <[_; 2]>::try_from(row.values).expect("complete output properties")
        else {
            unreachable!("complete output properties exist")
        };
        parts = serde_json::from_value(content)
            .map_err(|error| AgentError::fault("History", error.to_string()))?;
        let position = serde_json::from_value(position)
            .map_err(|error| AgentError::fault("History", error.to_string()))?;
        let Some(position) = successor(&segments, position) else {
            return Ok(settled_output_message(parts, completed));
        };
        begin = Some(position);
    }
    let mut fragments = query(
        request_id,
        through,
        &[
            "step/thinking",
            "step/text",
            "step/tool-use-start",
            "step/tool-use",
        ],
    );
    fragments.from = begin;
    loop {
        let reply = timeline.query(fragments.clone()).await?;
        let QueryOutput::Rows { rows } = reply.result else {
            unreachable!("output fragment identities")
        };
        for row in rows {
            let QueryValue::Value { value: id } = &row.values[0] else {
                unreachable!("fragment ID")
            };
            let id = id.as_str().expect("fragment ID string");
            let QueryOutput::Events { events } = timeline
                .query(chain_by_id(id, through, QuerySelect::Events))
                .await?
                .result
            else {
                unreachable!("output fragment")
            };
            let row = events
                .into_iter()
                .next()
                .ok_or_else(|| AgentError::fault("History", "missing output fragment"))?;
            fold_output_parts(&mut parts, &row.event.kind)
                .map_err(|error| AgentError::fault("History", error))?;
        }
        let Some(next) = reply.next else { break };
        fragments.page.as_mut().unwrap().after = Some(next);
    }
    Ok(settled_output_message(parts, completed))
}

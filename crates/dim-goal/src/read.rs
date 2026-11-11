use crate::{
    branch::{
        live_inputs,
        recovery::{self, BusinessState},
    },
    fold::Fold,
    text,
};
use dim_agent_capabilities::timeline_tree::TreeCommands;
use dim_agent_runtime::{
    AgentError, BoxFuture, QueryComparison, QueryFilter, QueryOperand, QueryOutput, QueryPage,
    QueryPick, QuerySelect, QueryValue, TimelineQuery, TimelineQueryReply,
};
use dim_agent_types::{GoalRead, GoalView, GoalViewStatus, InputBlock, TimelinePosition};
use std::num::NonZeroU64;
use tokio_util::sync::CancellationToken;

type ReadQuery<'a> = dyn Fn(TimelineQuery) -> BoxFuture<'a, Result<TimelineQueryReply, AgentError>>
    + Send
    + Sync
    + 'a;

pub(crate) struct Loaded {
    pub(crate) fold: Fold,
    pub(crate) repaired: bool,
}

pub(crate) fn pause_reloaded(fold: &mut Fold) {
    let previous = std::mem::take(fold);
    fold.objective = previous.objective;
    fold.state = previous.state;
    fold.judge = previous.judge;
    fold.live = previous.live;
    fold.local_blocks = previous.local_blocks;
    fold.last_block = previous.last_block;
    fold.last_change = previous.last_change;
    fold.state_after = previous.state_after;
    fold.record_after = previous.record_after;
    if fold.objective.is_some() && fold.state == text::State::Active {
        fold.state = text::State::Paused("interrupted_reload".into());
    }
}

pub(crate) async fn current(
    tree: &TreeCommands,
    branch_id: &str,
    cancel: &CancellationToken,
) -> Result<GoalRead, AgentError> {
    let mut opening = recovery::request(
        branch_id,
        None,
        u64::MAX,
        &["branch/start", "session/forked"],
        &["position.sequence"],
    );
    opening.through = None;
    opening.query.pick = Some(QueryPick::Last);
    opening.page.as_mut().expect("opening page").limit = NonZeroU64::MIN;
    let reply = tree.query(branch_id.into(), opening).await?;
    let through = reply.through;
    let QueryOutput::Rows { rows } = reply.result else {
        unreachable!("opening position")
    };
    let start = rows.first().map(|row| match &row.values[0] {
        QueryValue::Value { value } => value.as_u64().expect("opening sequence"),
        _ => unreachable!("opening sequence"),
    });
    let query_fn = |query| -> BoxFuture<'_, _> { Box::pin(tree.query(branch_id.into(), query)) };
    let mut loaded = load(branch_id, start, through, &query_fn, cancel).await?;
    pause_reloaded(&mut loaded.fold);
    Ok(view(branch_id, through, &loaded.fold))
}

pub(crate) async fn load(
    branch: &str,
    start: Option<u64>,
    through: u64,
    query_fn: &ReadQuery<'_>,
    cancel: &CancellationToken,
) -> Result<Loaded, AgentError> {
    let mut query = recovery::request(
        branch,
        start,
        through,
        &[recovery::KEY],
        &["position.sequence", "data"],
    );
    query.query.pick = Some(QueryPick::Last);
    query.page.as_mut().expect("state page").limit = NonZeroU64::MIN;
    let QueryOutput::Rows { mut rows } = query_fn(query.clone()).await?.result else {
        unreachable!("Goal state fields")
    };
    let Some(row) = rows.pop() else {
        let mut fold = legacy(branch, start, through, None, query_fn, cancel).await?;
        let mut repaired = false;
        if !fold.invalidated_after.is_empty() {
            let mut published =
                recovery::request(branch, start, through, &["input/message"], &["data.blocks"]);
            published.query.filter = Some(live_inputs(crate::branch::key_block(text::STATE_KEY)));
            published.query.pick = Some(QueryPick::Last);
            published.page.as_mut().expect("legacy model state").limit = NonZeroU64::MIN;
            let QueryOutput::Rows { rows } = query_fn(published).await?.result else {
                unreachable!("legacy model state")
            };
            if let Some(row) = rows.first() {
                let QueryValue::Value { value } = &row.values[0] else {
                    return Err(AgentError::fault("Goal", "missing legacy model state"));
                };
                let blocks: Vec<InputBlock> = serde_json::from_value(value.clone())
                    .map_err(|error| AgentError::fault("Goal", error.to_string()))?;
                repaired = !crate::branch::model_state_agrees(&fold, &blocks);
            }
        }
        if repaired && fold.objective.is_some() && fold.state == text::State::Active {
            fold.state = text::State::Paused("input_deleted".into());
        }
        return Ok(Loaded { fold, repaired });
    };
    let [
        QueryValue::Value { value: position },
        QueryValue::Value { value: data },
    ] = <[_; 2]>::try_from(row.values).expect("Goal state fields")
    else {
        return Err(AgentError::fault("Goal", "missing business state"));
    };
    let position = position
        .as_u64()
        .ok_or_else(|| AgentError::fault("Goal", "invalid business position"))?;
    let state = BusinessState::decode(data)?;
    let mut fold = Fold::default();
    state.apply(&mut fold, false);
    fold.last_change = position;
    fold.state_after = position;
    fold.last_block = position;
    fold.record_after = position;
    if position == through {
        return Ok(Loaded {
            fold,
            repaired: false,
        });
    }
    let mut deleted =
        recovery::request(branch, Some(position + 1), through, &["input/deleted"], &[]);
    deleted.query.select = QuerySelect::Exists;
    deleted.page = None;
    let QueryOutput::Exists { exists } = query_fn(deleted).await?.result else {
        unreachable!("deletions after the saved Goal state");
    };
    if !exists {
        return Ok(Loaded {
            fold,
            repaired: false,
        });
    }
    let mut affected = recovery::request(
        branch,
        start,
        through,
        &[recovery::KEY],
        &["position.sequence", "data.input_id"],
    );
    affected.query.filter = Some(QueryFilter::And {
        conditions: vec![
            affected.query.filter.take().expect("Goal business state"),
            QueryFilter::Exists {
                condition: Box::new(QueryFilter::And {
                    conditions: vec![
                        kind("input/deleted"),
                        QueryFilter::Compare {
                            left: QueryOperand::Field {
                                path: "data.input_id".into(),
                            },
                            comparison: QueryComparison::Eq,
                            right: QueryOperand::Outer {
                                path: "data.input_id".into(),
                            },
                        },
                        QueryFilter::Compare {
                            left: QueryOperand::Field {
                                path: "position.sequence".into(),
                            },
                            comparison: QueryComparison::Gt,
                            right: QueryOperand::Value {
                                value: position.into(),
                            },
                        },
                    ],
                }),
            },
        ],
    });
    affected.query.pick = Some(QueryPick::First);
    affected.page.as_mut().unwrap().limit = NonZeroU64::MIN;
    let QueryOutput::Rows { rows } = query_fn(affected).await?.result else {
        unreachable!("deleted Goal source")
    };
    let (earliest, earliest_input) = match rows.into_iter().next() {
        Some(row) => {
            let [QueryValue::Value { value: at }, input] = <[_; 2]>::try_from(row.values).unwrap()
            else {
                unreachable!("affected Goal position")
            };
            (
                Some(
                    at.as_u64()
                        .ok_or_else(|| AgentError::fault("Goal", "invalid affected position"))?,
                ),
                Some(input),
            )
        }
        None => (None, None),
    };
    let Some(earliest) = earliest else {
        return Ok(Loaded {
            fold,
            repaired: false,
        });
    };
    let mut before = query.clone();
    before.through = Some(earliest - 1);
    let QueryOutput::Rows { mut rows } = query_fn(before).await?.result else {
        unreachable!("previous Goal state")
    };
    fold = Fold::default();
    if let Some(row) = rows.pop() {
        let [_, QueryValue::Value { value }] =
            <[_; 2]>::try_from(row.values).expect("previous state fields")
        else {
            return Err(AgentError::fault("Goal", "missing previous state"));
        };
        BusinessState::decode(value)?.apply(&mut fold, false);
    } else if let Some(QueryValue::Value { value }) = earliest_input {
        let mut input = recovery::request(
            branch,
            start,
            through,
            &["input/message"],
            &["position.sequence"],
        );
        input.query.filter = Some(QueryFilter::And {
            conditions: vec![
                input.query.filter.take().expect("input kind"),
                QueryFilter::In {
                    value: QueryOperand::Field { path: "id".into() },
                    values: vec![value],
                },
            ],
        });
        let QueryOutput::Rows { rows } = query_fn(input).await?.result else {
            unreachable!("deleted command position")
        };
        let before = match rows.first().and_then(|row| row.values.first()) {
            Some(QueryValue::Value { value }) => value
                .as_u64()
                .ok_or_else(|| AgentError::fault("Goal", "invalid command position"))?
                .saturating_sub(2),
            _ => return Err(AgentError::fault("Goal", "missing deleted command")),
        };
        fold = legacy(branch, start, through, Some(before), query_fn, cancel).await?;
    }
    query.from = Some(TimelinePosition {
        branch_id: branch.into(),
        sequence: earliest,
    });
    query.query.pick = None;
    query.page.as_mut().expect("replay page").limit = NonZeroU64::new(128).unwrap();
    loop {
        if cancel.is_cancelled() {
            return Err(AgentError::Cancelled("Goal read".into()));
        }
        let reply = query_fn(query.clone()).await?;
        let QueryOutput::Rows { rows } = reply.result else {
            unreachable!("affected Goal changes")
        };
        for row in rows {
            let [_, QueryValue::Value { value }] =
                <[_; 2]>::try_from(row.values).expect("Goal replay fields")
            else {
                return Err(AgentError::fault("Goal", "missing affected state"));
            };
            let state = BusinessState::decode(value)?;
            let removed = if let Some(id) = &state.input_id {
                let mut deleted =
                    recovery::request(branch, start, through, &["input/deleted"], &[]);
                deleted.query.filter = Some(QueryFilter::And {
                    conditions: vec![
                        deleted.query.filter.take().expect("delete kind"),
                        QueryFilter::In {
                            value: QueryOperand::Field {
                                path: "data.input_id".into(),
                            },
                            values: vec![id.clone().into()],
                        },
                    ],
                });
                deleted.query.select = QuerySelect::Exists;
                deleted.page = None;
                let QueryOutput::Exists { exists } = query_fn(deleted).await?.result else {
                    unreachable!("deleted Goal input")
                };
                exists
            } else {
                false
            };
            if !removed {
                state.apply(&mut fold, true);
            }
        }
        let Some(next) = reply.next else {
            break;
        };
        query.page.as_mut().expect("replay page").after = Some(next);
    }
    if fold.objective == state.objective && fold.state == state.state && fold.judge == state.judge {
        return Ok(Loaded {
            fold,
            repaired: false,
        });
    }
    if fold.objective.is_some() && fold.state == text::State::Active {
        fold.state = text::State::Paused("input_deleted".into());
    }
    Ok(Loaded {
        fold,
        repaired: true,
    })
}

async fn legacy(
    branch_id: &str,
    start: Option<u64>,
    through: u64,
    before: Option<u64>,
    query_fn: &ReadQuery<'_>,
    cancel: &CancellationToken,
) -> Result<Fold, AgentError> {
    let mut query = recovery::request(branch_id, start, through, &[], &["position.sequence"]);
    let business_inputs = QueryFilter::And {
        conditions: vec![
            QueryFilter::Any {
                field: "data.blocks".into(),
                condition: Box::new(QueryFilter::Prefix {
                    value: QueryOperand::Element {
                        path: "text".into(),
                    },
                    prefix: "<dim-goal ".into(),
                }),
            },
            QueryFilter::Or {
                conditions: [
                    text::STATE_KEY,
                    text::OBJECTIVE_KEY,
                    text::STATUS_KEY,
                    "op=",
                ]
                .into_iter()
                .map(|term| QueryFilter::TextCandidate {
                    field: "data".into(),
                    partition_by: vec![],
                    terms: vec![term.into()],
                })
                .collect(),
            },
            QueryFilter::Any {
                field: "data.blocks".into(),
                condition: Box::new(QueryFilter::Or {
                    conditions: vec![
                        QueryFilter::In {
                            value: QueryOperand::Element { path: "key".into() },
                            values: [text::STATE_KEY, text::OBJECTIVE_KEY, text::STATUS_KEY]
                                .map(Into::into)
                                .to_vec(),
                        },
                        QueryFilter::Contains {
                            value: QueryOperand::Element {
                                path: "text".into(),
                            },
                            text: "kind=\"goal\"".into(),
                        },
                    ],
                }),
            },
        ],
    };
    let mut exists = query.clone();
    exists.query.filter = Some(live_inputs(business_inputs.clone()));
    exists.query.select = QuerySelect::Exists;
    exists.page = None;
    let QueryOutput::Exists { exists } = query_fn(exists).await?.result else {
        unreachable!("Goal legacy presence")
    };
    if !exists {
        return Ok(Fold::default());
    }
    query.from = start.map(|sequence| TimelinePosition {
        branch_id: branch_id.into(),
        sequence,
    });
    query.through = Some(through);
    query.query.pick = None;
    query.page.as_mut().unwrap().limit = NonZeroU64::new(256).unwrap();
    let mut fold = Fold::default();
    query.query.filter = Some(live_inputs(match before {
        Some(before) => QueryFilter::And {
            conditions: vec![
                business_inputs,
                QueryFilter::Compare {
                    left: QueryOperand::Field {
                        path: "position.sequence".into(),
                    },
                    comparison: QueryComparison::Le,
                    right: QueryOperand::Value {
                        value: before.into(),
                    },
                },
            ],
        },
        None => business_inputs,
    }));
    query.query.select = QuerySelect::Fields {
        fields: [
            "position.sequence",
            "id",
            "data.submission_id",
            "data.blocks",
        ]
        .map(String::from)
        .to_vec(),
    };
    loop {
        if cancel.is_cancelled() {
            return Err(AgentError::Cancelled("Goal read".into()));
        }
        let page = query_fn(query.clone()).await?;
        let QueryOutput::Rows { rows } = page.result else {
            unreachable!("Goal input fields")
        };
        let rows: Vec<_> = rows
            .into_iter()
            .map(|row| {
                let [
                    QueryValue::Value { value: sequence },
                    QueryValue::Value { value: id },
                    submission,
                    QueryValue::Value { value: blocks },
                ] = <[_; 4]>::try_from(row.values).expect("Goal input fields")
                else {
                    unreachable!("Goal input fields");
                };
                let blocks: Vec<InputBlock> =
                    serde_json::from_value(blocks).expect("input blocks contract");
                (
                    sequence.as_u64().expect("input sequence"),
                    id,
                    submission,
                    blocks,
                )
            })
            .collect();
        let mut referenced = std::collections::BTreeSet::new();
        for (sequence, _, _, blocks) in &rows {
            for block in blocks {
                let InputBlock::Raw {
                    text: raw,
                    key: Some(key),
                    ..
                } = block
                else {
                    continue;
                };
                if key == text::STATE_KEY
                    && let Some(attrs) = text::marker(raw)
                    && attrs.get("restated").map(String::as_str) != Some("true")
                {
                    if let Some(after) = attrs
                        .get("after")
                        .and_then(|value| crate::fold::sequence_of(value))
                    {
                        referenced.insert(after);
                    } else if attrs.contains_key("trigger") && *sequence < through {
                        referenced.insert(sequence + 1);
                    }
                }
            }
        }
        let referenced: Vec<_> = referenced.into_iter().collect();
        for referenced in referenced.chunks(128) {
            let mut invalidated = recovery::request(
                branch_id,
                start,
                through,
                &["input/message", "input/deleted"],
                &["position.sequence"],
            );
            invalidated.query.filter = Some(QueryFilter::And {
                conditions: vec![
                    invalidated.query.filter.take().expect("input source kinds"),
                    QueryFilter::In {
                        value: QueryOperand::Field {
                            path: "position.sequence".into(),
                        },
                        values: referenced.iter().copied().map(Into::into).collect(),
                    },
                    QueryFilter::Or {
                        conditions: vec![
                            kind("input/deleted"),
                            QueryFilter::And {
                                conditions: vec![
                                    kind("input/message"),
                                    QueryFilter::Exists {
                                        condition: Box::new(QueryFilter::And {
                                            conditions: vec![
                                                kind("input/deleted"),
                                                QueryFilter::Compare {
                                                    left: QueryOperand::Field {
                                                        path: "data.input_id".into(),
                                                    },
                                                    comparison: QueryComparison::Eq,
                                                    right: QueryOperand::Outer {
                                                        path: "id".into(),
                                                    },
                                                },
                                            ],
                                        }),
                                    },
                                ],
                            },
                        ],
                    },
                ],
            });
            let QueryOutput::Rows { rows: invalidated } = query_fn(invalidated).await?.result
            else {
                unreachable!("invalidated Goal references");
            };
            for row in invalidated {
                let QueryValue::Value { value } = &row.values[0] else {
                    unreachable!("referenced position")
                };
                fold.invalidated_after
                    .insert(value.as_u64().expect("input sequence"));
            }
        }
        for (sequence, id, submission, blocks) in rows {
            if let Some(next) = fold.held_next()
                && next < sequence
            {
                pair(query_fn, &query, &mut fold, next).await?;
            }
            let submission = match submission {
                QueryValue::Missing => None,
                QueryValue::Value { value } => Some(value),
                _ => unreachable!("submission scalar"),
            };
            fold.goal_input(
                sequence,
                id.as_str().expect("input id"),
                submission
                    .as_ref()
                    .map(|value| value.as_str().expect("submission id")),
                &blocks,
            );
            fold.compact(start);
        }
        query.page.as_mut().unwrap().after = page.next;
        if query.page.as_ref().unwrap().after.is_none() {
            break;
        }
    }
    if let Some(next) = fold.held_next()
        && next <= before.unwrap_or(through)
    {
        pair(query_fn, &query, &mut fold, next).await?;
    }
    Ok(fold)
}

pub(crate) fn view(branch_id: &str, through: u64, fold: &Fold) -> GoalRead {
    GoalRead {
        branch_id: branch_id.into(),
        through,
        goal: goal(fold.objective.clone(), &fold.state, fold.judge),
    }
}

pub(crate) fn goal(
    objective: Option<String>,
    state: &text::State,
    judge: text::Judge,
) -> Option<GoalView> {
    objective.map(|objective| GoalView {
        objective,
        status: match state {
            text::State::Active => GoalViewStatus::Active,
            text::State::Paused(reason) => GoalViewStatus::Paused(reason.clone()),
            text::State::Completed(_) => GoalViewStatus::Completed,
            text::State::Blocked(_) => GoalViewStatus::Blocked,
            text::State::None => GoalViewStatus::None,
        },
        evaluator: judge == text::Judge::Evaluator,
    })
}

fn kind(name: &str) -> QueryFilter {
    QueryFilter::In {
        value: QueryOperand::Field {
            path: "kind".into(),
        },
        values: vec![name.into()],
    }
}

async fn pair(
    query_fn: &ReadQuery<'_>,
    scope: &TimelineQuery,
    fold: &mut Fold,
    sequence: u64,
) -> Result<(), AgentError> {
    let mut query = scope.clone();
    query.query.filter = Some(live_inputs(QueryFilter::In {
        value: QueryOperand::Field {
            path: "position.sequence".into(),
        },
        values: vec![sequence.into()],
    }));
    query.query.select = QuerySelect::Fields {
        fields: vec!["data.submission_id".into()],
    };
    query.page = Some(QueryPage {
        limit: NonZeroU64::MIN,
        after: None,
    });
    let page = query_fn(query).await?;
    let QueryOutput::Rows { rows } = page.result else {
        unreachable!("trigger identity")
    };
    let submission = rows.first().and_then(|row| match &row.values[0] {
        QueryValue::Missing => None,
        QueryValue::Value { value } => Some(value.as_str().expect("submission id")),
        _ => unreachable!("submission scalar"),
    });
    fold.pair_next(sequence, submission);
    Ok(())
}

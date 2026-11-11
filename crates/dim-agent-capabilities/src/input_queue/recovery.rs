use super::*;
use dim_agent_runtime::{
    QueryComparison, QueryFilter, QueryOperand, QueryOutput, QueryPage, QueryPick, QuerySelect,
    QuerySpec, QueryValue, TimelineQuery, TimelineQueryScope,
};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::num::NonZeroU64;

fn field(path: &str, values: Vec<Value>) -> QueryFilter {
    QueryFilter::In {
        value: QueryOperand::Field { path: path.into() },
        values,
    }
}

fn query(through: u64, kinds: &[&str], fields: &[&str]) -> TimelineQuery {
    TimelineQuery {
        scope: TimelineQueryScope::Branch,
        from: None,
        through: Some(through),
        include_publishing: false,
        query: QuerySpec {
            filter: Some(field(
                "kind",
                kinds.iter().map(|kind| json!(kind)).collect(),
            )),
            select: QuerySelect::Fields {
                fields: fields.iter().map(|field| (*field).into()).collect(),
            },
            group_by: Vec::new(),
            pick: None,
            order_by: Vec::new(),
        },
        page: Some(QueryPage {
            limit: NonZeroU64::new(128).unwrap(),
            after: None,
        }),
    }
}

fn decode<T: DeserializeOwned>(value: QueryValue) -> Result<T> {
    let QueryValue::Value { value } = value else {
        return Err(AgentError::fault(
            "InputQueue",
            "missing queue control field",
        ));
    };
    serde_json::from_value(value)
        .map_err(|error| AgentError::fault("InputQueue", error.to_string()))
}

impl InputQueue {
    pub(super) fn write(
        &self,
        decisions: Vec<Decision>,
        through: u64,
    ) -> Result<(u64, Option<u64>)> {
        if decisions.is_empty() {
            return Ok((through, None));
        }
        let mut deleted = None;
        let drafts = decisions
            .into_iter()
            .enumerate()
            .map(|(index, decision)| {
                match decision {
                    Decision::Apply(input_id, mode) => EventKind::InputApplied(InputAppliedData {
                        input_id,
                        mode: Some(mode),
                    }),
                    Decision::Delete(input_id) => {
                        deleted = Some(index);
                        EventKind::InputDeleted(InputDeletedData { input_id })
                    }
                }
                .into()
            })
            .collect();
        let events = self.timeline.append_batch_registered(drafts, |_| Ok(()))?;
        Ok((
            events.last().expect("nonempty decisions").sequence,
            deleted.map(|index| events[index].sequence),
        ))
    }

    async fn last(&self, mut request: TimelineQuery) -> Result<Option<Vec<QueryValue>>> {
        request.query.pick = Some(QueryPick::Last);
        request.page.as_mut().unwrap().limit = NonZeroU64::MIN;
        let QueryOutput::Rows { mut rows } = self.timeline.query(request).await?.result else {
            unreachable!("queue control fields")
        };
        Ok(rows.pop().map(|row| row.values))
    }

    async fn await_publication(&self, through: u64) -> Result<()> {
        let mut publication = self.timeline.watch();
        while publication.borrow().0 < through {
            if let Some(error) = self.timeline.failure() {
                return Err(error);
            }
            tokio::select! {
                changed = publication.changed() => changed.map_err(|_| AgentError::Closed(self.timeline.branch_id()))?,
                _ = self.lifecycle.cancelled() => return Err(AgentError::Closed("InputQueue".into())),
            }
        }
        Ok(())
    }

    pub(super) async fn rebuild(&self, context: &rutis::Ctx) -> Result<u64> {
        let through = self.timeline.committed();
        self.await_publication(through).await?;
        let mut state = QueueState::default();
        let mut boundary = 0;
        for kind in ["turn/start", "turn/end"] {
            if let Some(row) = self
                .last(query(
                    through,
                    &[kind],
                    &["position.sequence", "data.turn_id"],
                ))
                .await?
            {
                let [sequence, turn] = <[_; 2]>::try_from(row).expect("turn control fields");
                let sequence: u64 = decode(sequence)?;
                if sequence > boundary {
                    boundary = sequence;
                    state.turn = if kind == "turn/start" {
                        Some(decode(turn)?)
                    } else {
                        None
                    };
                }
            }
        }
        let mut entries = Vec::new();
        let mut decisions = Vec::new();
        let mut latest_driver = 0;
        for handover in [false, true] {
            let mut request = query(
                through,
                &["input/message"],
                &[
                    "id",
                    "position.sequence",
                    "data.mode",
                    "data.handover.turn_id",
                ],
            );
            let present = QueryFilter::Present {
                value: QueryOperand::Field {
                    path: "data.handover".into(),
                },
            };
            request.query.filter = Some(QueryFilter::And {
                conditions: vec![
                    request.query.filter.take().unwrap(),
                    if handover {
                        present
                    } else {
                        QueryFilter::Not {
                            condition: Box::new(present),
                        }
                    },
                    QueryFilter::Not {
                        condition: Box::new(QueryFilter::Exists {
                            condition: Box::new(QueryFilter::And {
                                conditions: vec![
                                    field(
                                        "kind",
                                        vec![json!("input/consumed"), json!("input/deleted")],
                                    ),
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
            loop {
                let reply = self.timeline.query(request.clone()).await?;
                dim_agent_runtime::opening_advanced(context);
                let QueryOutput::Rows { rows } = reply.result else {
                    unreachable!("current queue members")
                };
                let ids = rows
                    .iter()
                    .map(|row| match &row.values[0] {
                        QueryValue::Value { value } => Ok(value.clone()),
                        _ => Err(AgentError::fault(
                            "InputQueue",
                            "missing current input identity",
                        )),
                    })
                    .collect::<Result<Vec<_>>>()?;
                let mut applied = std::collections::HashMap::new();
                let mut withdrawn = std::collections::HashMap::new();
                if !ids.is_empty() {
                    let mut controls = query(
                        through,
                        if handover {
                            &["input/withdraw"]
                        } else {
                            &["input/applied", "input/withdraw"]
                        },
                        &["kind", "data.input_id", "position.sequence", "data.mode"],
                    );
                    controls.query.filter = Some(QueryFilter::And {
                        conditions: vec![
                            controls.query.filter.take().expect("input controls"),
                            field("data.input_id", ids),
                        ],
                    });
                    controls.query.group_by = vec!["kind".into(), "data.input_id".into()];
                    controls.query.pick = Some(QueryPick::Last);
                    controls.page.as_mut().expect("control page").limit =
                        NonZeroU64::new(256).unwrap();
                    let controls = self.timeline.query(controls).await?;
                    if controls.next.is_some() {
                        return Err(AgentError::fault(
                            "InputQueue",
                            "too many grouped input controls",
                        ));
                    }
                    let QueryOutput::Rows { rows } = controls.result else {
                        unreachable!("input control fields")
                    };
                    for row in rows {
                        let [kind, id, at, mode] =
                            <[_; 4]>::try_from(row.values).expect("input control fields");
                        let id: String = decode(id)?;
                        let at: u64 = decode(at)?;
                        match decode::<String>(kind)?.as_str() {
                            "input/applied" => {
                                applied.insert(id, (at, mode));
                            }
                            "input/withdraw" => {
                                withdrawn.insert(id, at);
                            }
                            _ => unreachable!("selected input controls"),
                        }
                    }
                }
                for row in rows {
                    let [id, sequence, mode, target] =
                        <[_; 4]>::try_from(row.values).expect("input control fields");
                    let id: String = decode(id)?;
                    let sequence: u64 = decode(sequence)?;
                    let withdraw = withdrawn.remove(&id);
                    if handover {
                        let turn_id: String = decode(target)?;
                        if withdraw.is_some() || state.turn.as_ref() != Some(&turn_id) {
                            decisions.push(Decision::Delete(id));
                        } else {
                            entries.push((
                                sequence,
                                Entry {
                                    id,
                                    kind: EntryKind::Handover { turn_id },
                                },
                            ));
                        }
                        continue;
                    }
                    let (released, applied_at, mode) = if let Some((at, mode)) = applied.remove(&id)
                    {
                        if let Some(withdraw) = withdraw
                            && withdraw < at
                        {
                            return Err(AgentError::fault(
                                "InputQueue",
                                "an input was applied after its withdrawal",
                            ));
                        }
                        (
                            true,
                            at,
                            if mode == QueryValue::Missing {
                                QueryValue::Value {
                                    value: json!(InputMode::default()),
                                }
                            } else {
                                mode
                            },
                        )
                    } else if withdraw.is_some() {
                        decisions.push(Decision::Delete(id));
                        continue;
                    } else {
                        (false, 0, mode)
                    };
                    let mode = match mode {
                        QueryValue::Missing => {
                            let mut user = query(through, &["input/message"], &[]);
                            user.query.filter = Some(QueryFilter::And {
                                conditions: vec![
                                    field("id", vec![json!(id)]),
                                    QueryFilter::Present {
                                        value: QueryOperand::Field {
                                            path: "data.user".into(),
                                        },
                                    },
                                ],
                            });
                            user.query.select = QuerySelect::Exists;
                            user.page = None;
                            let QueryOutput::Exists { exists } =
                                self.timeline.query(user).await?.result
                            else {
                                unreachable!("user presence")
                            };
                            if exists {
                                InputMode::Queue
                            } else {
                                InputMode::Append
                            }
                        }
                        value => decode(value)?,
                    };
                    if released && mode != InputMode::Append {
                        latest_driver = latest_driver.max(applied_at);
                    }
                    state.queued += 1;
                    entries.push((
                        sequence,
                        Entry {
                            id,
                            kind: EntryKind::Input { mode, released },
                        },
                    ));
                }
                let Some(next) = reply.next else { break };
                request.page.as_mut().unwrap().after = Some(next);
            }
        }
        if state.turn.is_none() && latest_driver > boundary {
            let interrupted = match self
                .last(query(through, &["stop/interrupt"], &["position.sequence"]))
                .await?
            {
                Some(row) => {
                    let [at] = <[_; 1]>::try_from(row).expect("interrupt position");
                    decode(at)?
                }
                None => 0,
            };
            state.starting = latest_driver > interrupted;
        }
        entries.sort_by_key(|(sequence, _)| *sequence);
        state.entries = entries.into_iter().map(|(_, entry)| entry).collect();
        let mut held = false;
        let release: Vec<_> = state
            .entries
            .iter()
            .filter_map(|entry| {
                let EntryKind::Input {
                    mode,
                    released: false,
                } = entry.kind
                else {
                    return None;
                };
                if mode == InputMode::Queue {
                    if state.turn.is_some() || state.starting || held {
                        return None;
                    }
                    held = true;
                }
                Some(entry.id.clone())
            })
            .collect();
        for id in release {
            decisions.extend(state.release(&id));
        }
        let (published, _) = self.write(decisions, through)?;
        self.await_publication(published).await?;
        self.publish(published, state.queued);
        *self.state.lock().unwrap() = state;
        Ok(through)
    }
}

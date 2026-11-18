use super::*;
use dim_agent_runtime::{Branch, QueryComparison, QueryPick, QueryRow, QueryValue};
use std::{collections::HashSet, num::NonZeroU64};

const ORIGINS: &[&str] = &[
    "input/consumed",
    "step/model-start",
    "step/tool-use-start",
    "stop/interrupt",
    "permission/changed",
    "context/compaction",
    "session/forked",
];

fn one_of(path: &str, values: impl IntoIterator<Item = Value>) -> QueryFilter {
    QueryFilter::In {
        value: QueryOperand::Field { path: path.into() },
        values: values.into_iter().collect(),
    }
}
fn kinds(values: &[&str]) -> QueryFilter {
    one_of("kind", values.iter().map(|value| json!(value)))
}
fn and(conditions: Vec<QueryFilter>) -> QueryFilter {
    QueryFilter::And { conditions }
}
fn or(conditions: Vec<QueryFilter>) -> QueryFilter {
    QueryFilter::Or { conditions }
}
fn compare(path: &str, comparison: QueryComparison, value: Value) -> QueryFilter {
    QueryFilter::Compare {
        left: QueryOperand::Field { path: path.into() },
        comparison,
        right: QueryOperand::Value { value },
    }
}
fn joined(left: &str, right: &str) -> QueryFilter {
    QueryFilter::Compare {
        left: QueryOperand::Field { path: left.into() },
        comparison: QueryComparison::Eq,
        right: QueryOperand::Outer { path: right.into() },
    }
}
fn value(row: &QueryRow, index: usize) -> &Value {
    let QueryValue::Value { value } = &row.values[index] else {
        panic!("selected required Recall property")
    };
    value
}
fn position(row: &QueryRow, index: usize) -> TimelinePosition {
    let p = value(row, index);
    TimelinePosition {
        branch_id: p["branch_id"].as_str().expect("position branch").into(),
        sequence: p["sequence"].as_u64().expect("position sequence"),
    }
}

struct Search<'a> {
    branch: &'a Branch,
    head: u64,
    chain: Vec<String>,
    cancel: &'a CancellationToken,
}
impl Search<'_> {
    fn request(&self, filter: QueryFilter, select: QuerySelect) -> TimelineQuery {
        TimelineQuery {
            scope: TimelineQueryScope::Chain,
            from: None,
            through: Some(self.head),
            include_publishing: false,
            query: QuerySpec {
                filter: Some(filter),
                select,
                group_by: vec![],
                pick: None,
                order_by: vec![QueryOrder {
                    key: QueryOrderKey::Position,
                    direction: TimelineQueryDirection::Ascending,
                }],
            },
            page: Some(QueryPage {
                limit: NonZeroU64::new(PAGE_EVENTS as u64).unwrap(),
                after: None,
            }),
        }
    }
    fn order(&self, position: &TimelinePosition) -> (usize, u64) {
        (
            self.chain
                .iter()
                .position(|branch| branch == &position.branch_id)
                .expect("position belongs to selected chain"),
            position.sequence,
        )
    }
    async fn joins(&self, filter: QueryFilter) -> Result<Vec<QueryEvent>, HostError> {
        let mut query = self.request(filter, QuerySelect::Events);
        let mut joined = Vec::new();
        loop {
            let reply = tokio::select! {
                _ = self.cancel.cancelled() => return Err(HostError::Cancelled("recall".into())),
                reply = self.branch.query(query.clone()) => reply.map_err(HostError::from_agent)?,
            };
            let QueryOutput::Events { events } = reply.result else {
                unreachable!("Recall joins select events")
            };
            joined.extend(events);
            let Some(next) = reply.next else {
                return Ok(joined);
            };
            query.page.as_mut().unwrap().after = Some(next);
        }
    }
    async fn anchor(&self, id: &str) -> Result<Option<TimelinePosition>, HostError> {
        let mut query = self.request(
            one_of("id", [json!(id)]),
            QuerySelect::Fields {
                fields: ["kind", "position", "data.status", "data.request_id"]
                    .map(String::from)
                    .to_vec(),
            },
        );
        query.page.as_mut().unwrap().limit = NonZeroU64::MIN;
        let reply = self
            .branch
            .query(query.clone())
            .await
            .map_err(HostError::from_agent)?;
        let QueryOutput::Rows { rows } = reply.result else {
            unreachable!("anchor properties")
        };
        let Some(row) = rows.first() else {
            return Ok(None);
        };
        match value(row, 0).as_str().expect("kind") {
            "input/message" => {
                query.query.filter = Some(and(vec![
                    kinds(&["input/consumed"]),
                    one_of("data.input_id", [json!(id)]),
                ]));
                let reply = self
                    .branch
                    .query(query)
                    .await
                    .map_err(HostError::from_agent)?;
                let QueryOutput::Rows { rows } = reply.result else {
                    unreachable!("consumption anchor")
                };
                Ok(rows.first().map(|row| position(row, 1)))
            }
            "step/model-start" => {
                let records = self
                    .joins(or(vec![
                        one_of("id", [json!(id)]),
                        and(vec![
                            kinds(&["step/text", "step/model-content", "step/model-output-end"]),
                            one_of("data.request_id", [value(row, 3).clone()]),
                        ]),
                    ]))
                    .await?;
                let events: Vec<_> = records.into_iter().map(|record| record.event).collect();
                Ok(Narrative::fold(&events, 0, events.len())
                    .ids
                    .contains_key(id)
                    .then(|| position(row, 1)))
            }
            "context/compaction" => Ok((value(row, 2) == "completed").then(|| position(row, 1))),
            "branch/start" => Ok((self.chain.len() > 1
                && &position(row, 1).branch_id == self.chain.last().unwrap())
            .then(|| position(row, 1))),
            "turn/start"
            | "step/tool-use-start"
            | "stop/interrupt"
            | "permission/changed"
            | "session/forked" => Ok(Some(position(row, 1))),
            _ => Ok(None),
        }
    }
    async fn current_keys(
        &self,
        keys: HashSet<String>,
    ) -> Result<HashMap<String, (String, usize)>, HostError> {
        if keys.is_empty() {
            return Ok(HashMap::new());
        }
        let mut query = self.request(
            and(vec![
                kinds(&["input/consumed"]),
                QueryFilter::Exists {
                    condition: Box::new(and(vec![
                        kinds(&["input/message"]),
                        joined("id", "data.input_id"),
                        QueryFilter::Any {
                            field: "data.blocks".into(),
                            condition: Box::new(QueryFilter::In {
                                value: QueryOperand::Element { path: "key".into() },
                                values: keys.iter().map(|key| json!(key)).collect(),
                            }),
                        },
                    ])),
                },
            ]),
            QuerySelect::Fields {
                fields: vec!["data.input_id".into()],
            },
        );
        let mut current = HashMap::new();
        loop {
            check_cancel(self.cancel)?;
            let reply = self
                .branch
                .query(query.clone())
                .await
                .map_err(HostError::from_agent)?;
            let QueryOutput::Rows { rows } = reply.result else {
                unreachable!("key consumption identities")
            };
            let ids: Vec<Value> = rows.iter().map(|row| value(row, 0).clone()).collect();
            if !ids.is_empty() {
                let origins = self.request(
                    and(vec![kinds(&["input/message"]), one_of("id", ids.clone())]),
                    QuerySelect::Fields {
                        fields: vec!["id".into(), "data.blocks[].key".into()],
                    },
                );
                let reply = self
                    .branch
                    .query(origins)
                    .await
                    .map_err(HostError::from_agent)?;
                let QueryOutput::Rows { rows } = reply.result else {
                    unreachable!("key fields")
                };
                let by_id: HashMap<&str, &Vec<QueryValue>> = rows
                    .iter()
                    .map(|row| {
                        let QueryValue::Elements { values } = &row.values[1] else {
                            unreachable!("block key projection")
                        };
                        (value(row, 0).as_str().unwrap(), values)
                    })
                    .collect();
                for id in ids {
                    let id = id.as_str().unwrap();
                    for (index, key) in by_id[id].iter().enumerate() {
                        if let QueryValue::Value {
                            value: Value::String(key),
                        } = key
                            && keys.contains(key)
                        {
                            current.insert(key.to_owned(), (id.to_owned(), index));
                        }
                    }
                }
            }
            let Some(next) = reply.next else {
                return Ok(current);
            };
            query.page.as_mut().unwrap().after = Some(next);
        }
    }
    async fn open_turn(
        &self,
        origin: &QueryRow,
        arguments: &Arguments,
    ) -> Result<ToolResult, HostError> {
        let id = value(origin, 0).as_str().expect("turn event id");
        let start = position(origin, 2);
        let mut end_query = self.request(
            and(vec![
                kinds(&["turn/end"]),
                one_of("data.turn_id", [value(origin, 6).clone()]),
                one_of("position.branch_id", [json!(start.branch_id)]),
            ]),
            QuerySelect::Fields {
                fields: vec!["position.sequence".into()],
            },
        );
        end_query.query.pick = Some(QueryPick::First);
        end_query.query.order_by.clear();
        let reply = self
            .branch
            .query(end_query)
            .await
            .map_err(HostError::from_agent)?;
        let QueryOutput::Rows { rows } = reply.result else {
            unreachable!("turn end")
        };
        let mut conditions = vec![
            kinds(ORIGINS),
            one_of("position.branch_id", [json!(start.branch_id)]),
            compare(
                "position.sequence",
                QueryComparison::Ge,
                json!(start.sequence),
            ),
        ];
        if let Some(end) = rows.first() {
            conditions.push(compare(
                "position.sequence",
                QueryComparison::Le,
                value(end, 0).clone(),
            ));
        }
        let mut query = self.request(
            and(conditions),
            QuerySelect::Fields {
                fields: [
                    "id",
                    "kind",
                    "position",
                    "data.input_id",
                    "data.request_id",
                    "data.call_id",
                ]
                .map(String::from)
                .to_vec(),
            },
        );
        let compaction = self.current_compaction().await?;
        let offset = arguments.offset.unwrap_or(0);
        let mut count = 0;
        let mut lines = Vec::new();
        loop {
            check_cancel(self.cancel)?;
            let reply = self
                .branch
                .query(query.clone())
                .await
                .map_err(HostError::from_agent)?;
            let QueryOutput::Rows { rows } = reply.result else {
                unreachable!("turn candidates")
            };
            if rows.is_empty() {
                if let Some(next) = reply.next {
                    query.page.as_mut().unwrap().after = Some(next);
                    continue;
                }
                break;
            }
            for batch in rows.chunks(8) {
                let anchors: HashSet<&str> = batch
                    .iter()
                    .map(|row| value(row, 0).as_str().unwrap())
                    .collect();
                let events: Vec<_> = self
                    .bodies(batch)
                    .await?
                    .into_iter()
                    .map(|row| row.event)
                    .collect();
                let keys = events
                    .iter()
                    .filter_map(|event| match &event.kind {
                        EventKind::InputMessage(data) => Some(&data.blocks),
                        _ => None,
                    })
                    .flatten()
                    .filter_map(|block| {
                        let InputBlock::Raw { key, .. } = block;
                        key.clone()
                    })
                    .collect();
                let current = self.current_keys(keys).await?;
                let mut narrative = Narrative::fold(&events, 0, events.len());
                mark_current(&mut narrative, &current, compaction.as_deref());
                for thing in &narrative.things {
                    if !anchors.contains(events[thing.anchor].id.as_str()) {
                        continue;
                    }
                    if count >= offset {
                        let line = narrative.thing_line(thing);
                        lines.push((line.time, line.id.to_owned(), line.text));
                    }
                    count += 1;
                    if lines.len() > PAGE_ENTRIES {
                        break;
                    }
                }
                if lines.len() > PAGE_ENTRIES {
                    break;
                }
            }
            if lines.len() > PAGE_ENTRIES || reply.next.is_none() {
                break;
            }
            query.page.as_mut().unwrap().after = reply.next;
        }
        if count == 0 && offset == 0 {
            return Ok(page(
                "turn",
                vec!["Nothing in this turn.".into()],
                &[],
                None,
            ));
        }
        if offset >= count {
            return Ok(ToolResult::error(format!("Turn {id} has {count} lines.")));
        }
        let more = lines.len() > PAGE_ENTRIES;
        lines.truncate(PAGE_ENTRIES);
        let mut output = Vec::new();
        let mut day = None;
        for (time, id, text) in &lines {
            let date = date(*time);
            if day.as_deref() != Some(date.as_str()) {
                output.push(format!("{date} (UTC)"));
                day = Some(date);
            }
            output.push(format!("{} {id} {text}", clock(*time)));
        }
        let end = offset + lines.len();
        output.push(if more {
            format!("More of this turn: open={id} offset={end}.")
        } else {
            format!("End of turn. Open one: open={}.", lines[0].1)
        });
        Ok(page(
            "turn",
            output,
            &lines
                .iter()
                .map(|(_, id, _)| id.as_str())
                .collect::<Vec<_>>(),
            more.then(|| json!({"open":id,"offset":end})),
        ))
    }

    async fn current_compaction(&self) -> Result<Option<String>, HostError> {
        let mut compaction = self.request(
            and(vec![
                kinds(&["context/compaction"]),
                one_of("data.status", [json!("completed")]),
            ]),
            QuerySelect::Fields {
                fields: vec!["id".into()],
            },
        );
        compaction.query.pick = Some(QueryPick::Last);
        compaction.query.order_by.clear();
        compaction.page.as_mut().unwrap().limit = NonZeroU64::MIN;
        let reply = self
            .branch
            .query(compaction)
            .await
            .map_err(HostError::from_agent)?;
        let QueryOutput::Rows { rows } = reply.result else {
            unreachable!("compaction identity")
        };
        Ok(rows
            .first()
            .map(|row| value(row, 0).as_str().unwrap().to_owned()))
    }
    async fn bodies(&self, candidates: &[QueryRow]) -> Result<Vec<QueryEvent>, HostError> {
        let ids: Vec<_> = candidates.iter().map(|row| value(row, 0).clone()).collect();
        let property = |index| {
            candidates
                .iter()
                .filter_map(move |row| match &row.values[index] {
                    QueryValue::Value {
                        value: Value::String(value),
                    } => Some(json!(value)),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        let requests = candidates
            .iter()
            .filter(|row| value(row, 1) == "step/model-start")
            .map(|row| value(row, 4).clone())
            .collect::<Vec<_>>();
        let calls = candidates
            .iter()
            .filter(|row| value(row, 1) == "step/tool-use-start")
            .collect::<Vec<_>>();
        let call_filter = |request_path: &str, call_path: &str| {
            or(calls
                .iter()
                .map(|row| {
                    and(vec![
                        one_of(request_path, [value(row, 4).clone()]),
                        one_of(call_path, [value(row, 5).clone()]),
                    ])
                })
                .collect())
        };
        let mut events = self
            .joins(or(vec![
                one_of("id", ids),
                and(vec![kinds(&["input/message"]), one_of("id", property(3))]),
                and(vec![
                    kinds(&["step/text", "step/model-content", "step/model-output-end"]),
                    one_of("data.request_id", requests),
                ]),
                and(vec![
                    kinds(&["step/tool-use"]),
                    call_filter("data.request_id", "data.call.id"),
                ]),
                and(vec![
                    kinds(&[
                        "exec/started",
                        "collaboration/request",
                        "collaboration/message",
                    ]),
                    call_filter("data.request_id", "data.call_id"),
                ]),
                and(vec![
                    kinds(&["approval/request", "branch/create"]),
                    call_filter("data.source.request_id", "data.source.call_id"),
                ]),
                and(vec![
                    kinds(&["step/tool-result"]),
                    or(calls
                        .iter()
                        .map(|row| {
                            and(vec![
                                one_of("data.request_id", [value(row, 4).clone()]),
                                QueryFilter::Any {
                                    field: "data.results".into(),
                                    condition: Box::new(QueryFilter::In {
                                        value: QueryOperand::Element {
                                            path: "call_id".into(),
                                        },
                                        values: vec![value(row, 5).clone()],
                                    }),
                                },
                            ])
                        })
                        .collect()),
                ]),
            ]))
            .await?;
        let (mut approvals, mut tasks, mut collaborations) = (Vec::new(), Vec::new(), Vec::new());
        for row in &events {
            match &row.event.kind {
                EventKind::ApprovalRequest(data) => approvals.push(json!(data.approval_id)),
                EventKind::ExecStarted(data) => tasks.push(json!(data.task_id)),
                EventKind::CollaborationRequest(_) => collaborations.push(json!(row.event.id)),
                _ => {}
            }
        }
        if !approvals.is_empty() || !tasks.is_empty() || !collaborations.is_empty() {
            events.extend(
                self.joins(or(vec![
                    and(vec![
                        kinds(&["approval/respond", "approval/cancelled"]),
                        one_of("data.approval_id", approvals),
                    ]),
                    and(vec![kinds(&["exec/ended"]), one_of("data.task_id", tasks)]),
                    and(vec![
                        kinds(&["collaboration/result"]),
                        one_of("data.request_event_id", collaborations),
                    ]),
                ]))
                .await?,
            );
        }
        let mut spans: HashMap<String, (u64, u64)> = HashMap::new();
        for row in candidates {
            let p = position(row, 2);
            let span = spans.entry(p.branch_id).or_insert((p.sequence, p.sequence));
            span.0 = span.0.min(p.sequence);
            span.1 = span.1.max(p.sequence);
        }
        for (branch, (first, last)) in spans {
            let in_branch = one_of("position.branch_id", [json!(branch)]);
            let mut boundary = self.request(
                and(vec![
                    in_branch.clone(),
                    kinds(&["turn/start", "turn/end", "branch/start"]),
                    compare("position.sequence", QueryComparison::Le, json!(first)),
                ]),
                QuerySelect::Fields {
                    fields: vec!["position.sequence".into()],
                },
            );
            boundary.query.pick = Some(QueryPick::Last);
            boundary.query.order_by.clear();
            boundary.page.as_mut().unwrap().limit = NonZeroU64::MIN;
            let reply = self
                .branch
                .query(boundary)
                .await
                .map_err(HostError::from_agent)?;
            let QueryOutput::Rows { rows } = reply.result else {
                unreachable!("turn boundary")
            };
            let start = rows
                .first()
                .map_or(first, |row| value(row, 0).as_u64().unwrap());
            events.extend(
                self.joins(and(vec![
                    in_branch,
                    kinds(&["turn/start", "turn/end", "branch/start", "step/model-start"]),
                    compare("position.sequence", QueryComparison::Ge, json!(start)),
                    compare("position.sequence", QueryComparison::Le, json!(last)),
                ]))
                .await?,
            );
        }
        events.sort_by_key(|row| self.order(&row.position));
        events.dedup_by(|a, b| a.position == b.position);
        Ok(events)
    }
}

fn text_candidates(words: &[String]) -> Option<QueryFilter> {
    const LABELS: &str = "current version in your context ended image file with messages denied by the user approval cancelled not run sub agent answered failed ran background task finished at running error result yet ok";
    let word = words
        .iter()
        .filter(|word| {
            (3..=200).contains(&word.len())
                && word.bytes().all(|byte| byte.is_ascii_alphabetic())
                && !LABELS.contains(word.as_str())
        })
        .max_by_key(|word| word.len())?;
    let text = |field: &str, partition_by: &[&str]| QueryFilter::TextCandidate {
        field: field.into(),
        partition_by: partition_by.iter().map(|field| (*field).into()).collect(),
        terms: vec![word.clone()],
    };
    let exists = |condition| QueryFilter::Exists {
        condition: Box::new(condition),
    };
    Some(or(vec![
        and(vec![
            kinds(&["input/consumed"]),
            exists(and(vec![
                kinds(&["input/message"]),
                joined("id", "data.input_id"),
                text("data", &[]),
            ])),
        ]),
        and(vec![
            kinds(&["step/model-start"]),
            exists(and(vec![
                joined("data.request_id", "data.request_id"),
                or(vec![
                    and(vec![
                        kinds(&["step/text"]),
                        text("data.delta", &["kind", "data.request_id"]),
                    ]),
                    and(vec![kinds(&["step/model-content"]), text("data", &[])]),
                ]),
            ])),
        ]),
        and(vec![
            kinds(&["step/tool-use-start"]),
            or(vec![
                exists(and(vec![
                    kinds(&["step/tool-use-start", "step/tool-use", "step/tool-result"]),
                    joined("data.request_id", "data.request_id"),
                    text("data", &[]),
                ])),
                exists(and(vec![
                    kinds(&[
                        "exec/started",
                        "collaboration/request",
                        "collaboration/message",
                    ]),
                    joined("data.request_id", "data.request_id"),
                ])),
                exists(and(vec![
                    kinds(&["approval/request", "branch/create"]),
                    joined("data.source.request_id", "data.request_id"),
                ])),
            ]),
        ]),
        kinds(&[
            "stop/interrupt",
            "permission/changed",
            "context/compaction",
            "session/forked",
            "branch/start",
        ]),
    ]))
}

pub(super) async fn run(
    tree: &TreeCommands,
    branch: &Branch,
    branch_id: &str,
    arguments: &Arguments,
    cancel: &CancellationToken,
) -> Result<ToolResult, HostError> {
    let mut chain = vec![branch_id.to_owned()];
    while let Some(parent) = tree
        .info(chain.last().unwrap().clone())
        .map_err(HostError::from_agent)?
        .parent
    {
        chain.push(parent);
    }
    chain.reverse();
    let search = Search {
        branch,
        head: branch.sequence(),
        chain,
        cancel,
    };
    let mut conditions = vec![or(vec![
        kinds(ORIGINS),
        and(vec![
            kinds(&["branch/start"]),
            one_of("position.branch_id", [json!(branch_id)]),
        ]),
    ])];
    if let Some(filter) = text_candidates(&arguments.words()) {
        conditions.push(filter);
    }
    if let Some(tool) = &arguments.tool {
        conditions.extend([
            kinds(&["step/tool-use-start"]),
            one_of("data.name", [json!(tool)]),
        ]);
    }
    if let Some(id) = &arguments.before {
        let Some(anchor) = search.anchor(id).await? else {
            return Ok(ToolResult::error(format!(
                "No turn or line has id {}.",
                part(id)
            )));
        };
        let (ordinal, sequence) = search.order(&anchor);
        conditions.push(or(vec![
            one_of(
                "position.branch_id",
                search.chain[..ordinal].iter().map(|branch| json!(branch)),
            ),
            and(vec![
                one_of("position.branch_id", [json!(anchor.branch_id)]),
                compare("position.sequence", QueryComparison::Lt, json!(sequence)),
            ]),
        ]));
    }
    let mut query = search.request(
        and(conditions),
        QuerySelect::Fields {
            fields: [
                "id",
                "kind",
                "position",
                "data.input_id",
                "data.request_id",
                "data.call_id",
            ]
            .map(String::from)
            .to_vec(),
        },
    );
    query.query.order_by[0].direction = TimelineQueryDirection::Descending;
    let current_compaction = search.current_compaction().await?;
    let words = arguments.words();
    let mut hits: Vec<(u64, String, String)> = Vec::new();
    loop {
        check_cancel(cancel)?;
        let reply = branch
            .query(query.clone())
            .await
            .map_err(HostError::from_agent)?;
        let QueryOutput::Rows { rows } = reply.result else {
            unreachable!("Recall candidates")
        };
        if rows.is_empty() {
            if let Some(next) = reply.next {
                query.page.as_mut().unwrap().after = Some(next);
                continue;
            }
            break;
        }
        for batch in rows.chunks(8) {
            let anchors: HashSet<&str> = batch
                .iter()
                .map(|row| value(row, 0).as_str().unwrap())
                .collect();
            let records = search.bodies(batch).await?;
            let events: Vec<_> = records.into_iter().map(|row| row.event).collect();
            let keys = events
                .iter()
                .filter_map(|event| match &event.kind {
                    EventKind::InputMessage(data) => Some(&data.blocks),
                    _ => None,
                })
                .flatten()
                .filter_map(|block| {
                    let InputBlock::Raw { key, .. } = block;
                    key.clone()
                })
                .collect();
            let current = search.current_keys(keys).await?;
            let mut narrative = Narrative::fold(&events, 0, events.len());
            mark_current(&mut narrative, &current, current_compaction.as_deref());
            for index in narrative.matches(arguments.tool.as_deref(), &words, None) {
                let thing = &narrative.things[index];
                if !anchors.contains(events[thing.anchor].id.as_str()) {
                    continue;
                }
                let line = narrative.thing_line(thing);
                hits.push((line.time, line.id.to_owned(), line.text));
                if hits.len() > PAGE_ENTRIES {
                    break;
                }
            }
            if hits.len() > PAGE_ENTRIES {
                break;
            }
        }
        if hits.len() > PAGE_ENTRIES || reply.next.is_none() {
            break;
        }
        query.page.as_mut().unwrap().after = reply.next;
    }
    if hits.is_empty() {
        return Ok(page(
            "search",
            vec![if arguments.before.is_some() {
                "No more.".into()
            } else {
                format!("Nothing matches {}.", filters(arguments))
            }],
            &[],
            None,
        ));
    }
    let more = hits.len() > PAGE_ENTRIES;
    hits.truncate(PAGE_ENTRIES);
    let mut output = Vec::new();
    let mut day = None;
    for (time, id, text) in &hits {
        let date = date(*time);
        if day.as_deref() != Some(date.as_str()) {
            output.push(format!("{date} (UTC)"));
            day = Some(date);
        }
        output.push(format!("{} {id} {text}", clock(*time)));
    }
    let last = &hits.last().unwrap().1;
    output.push(if more {
        format!("More: before={last}.")
    } else {
        "No more.".into()
    });
    Ok(page(
        "search",
        output,
        &hits
            .iter()
            .map(|(_, id, _)| id.as_str())
            .collect::<Vec<_>>(),
        more.then(|| json!({"before":last})),
    ))
}

fn mark_current(
    narrative: &mut Narrative<'_>,
    current: &HashMap<String, (String, usize)>,
    current_compaction: Option<&str>,
) {
    for thing in &mut narrative.things {
        let id = &narrative.events[thing.origin].id;
        match &mut thing.kind {
            Kind::Input { input, live, .. } => {
                for (index, block) in input.blocks.iter().enumerate() {
                    let InputBlock::Raw { key, retire, .. } = block;
                    live[index] = !retire
                        && key
                            .as_ref()
                            .is_some_and(|key| current.get(key) == Some(&(id.clone(), index)));
                }
            }
            Kind::Compaction { current } => *current = current_compaction == Some(id.as_str()),
            _ => {}
        }
    }
}

pub(super) async fn open(
    tree: &TreeCommands,
    branch: &Branch,
    branch_id: &str,
    head: u64,
    arguments: &Arguments,
    cancel: &CancellationToken,
) -> Result<ToolResult, HostError> {
    let id = arguments.open.as_ref().expect("open argument");
    let mut chain = vec![branch_id.to_owned()];
    while let Some(parent) = tree
        .info(chain.last().unwrap().clone())
        .map_err(HostError::from_agent)?
        .parent
    {
        chain.push(parent);
    }
    chain.reverse();
    let search = Search {
        branch,
        head,
        chain,
        cancel,
    };
    let mut query = search.request(
        one_of("id", [json!(id)]),
        QuerySelect::Fields {
            fields: [
                "id",
                "kind",
                "position",
                "data.input_id",
                "data.request_id",
                "data.call_id",
                "data.turn_id",
            ]
            .map(String::from)
            .to_vec(),
        },
    );
    let reply = branch
        .query(query.clone())
        .await
        .map_err(HostError::from_agent)?;
    let QueryOutput::Rows { mut rows } = reply.result else {
        unreachable!("Recall origin fields")
    };
    let missing = || ToolResult::error(format!("No turn or line has id {}.", part(id)));
    let Some(row) = rows.first() else {
        return Ok(missing());
    };
    match value(row, 1).as_str().expect("kind") {
        "turn/start" => return search.open_turn(row, arguments).await,
        "input/message" => {
            query.query.filter = Some(and(vec![
                kinds(&["input/consumed"]),
                one_of("data.input_id", [json!(id)]),
            ]));
            let reply = branch.query(query).await.map_err(HostError::from_agent)?;
            let QueryOutput::Rows { rows: consumed } = reply.result else {
                unreachable!("Recall consumption")
            };
            rows = consumed;
            if rows.is_empty() {
                return Ok(missing());
            }
        }
        "branch/start" if search.chain.len() > 1 && position(row, 2).branch_id == branch_id => {}
        kind if ORIGINS.contains(&kind) && kind != "input/consumed" => {}
        _ => return Ok(missing()),
    }
    let records = search.bodies(&rows).await?;
    let events: Vec<_> = records.into_iter().map(|row| row.event).collect();
    let keys = events
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::InputMessage(data) => Some(&data.blocks),
            _ => None,
        })
        .flatten()
        .filter_map(|block| {
            let InputBlock::Raw { key, .. } = block;
            key.clone()
        })
        .collect();
    let current = search.current_keys(keys).await?;
    let compaction = if events
        .iter()
        .any(|event| matches!(event.kind, EventKind::ContextCompaction(_)))
    {
        search.current_compaction().await?
    } else {
        None
    };
    let mut narrative = Narrative::fold(&events, 0, events.len());
    mark_current(&mut narrative, &current, compaction.as_deref());
    Ok(narrative.answer("", arguments))
}
struct TurnSummary {
    position: TimelinePosition,
    id: String,
    time: u64,
    turn_id: String,
    title: Option<(u8, String)>,
    working: bool,
    tools: Vec<(String, usize)>,
}

impl TurnSummary {
    fn line(self, outcome: &str) -> (TimelinePosition, u64, String, String) {
        let mut parts = vec![
            self.title
                .map_or_else(|| "turn: no opening input".into(), |(_, title)| title),
        ];
        if !self.tools.is_empty() {
            let tools = self
                .tools
                .into_iter()
                .map(|(name, count)| {
                    if count == 1 {
                        name
                    } else {
                        format!("{name} ×{count}")
                    }
                })
                .collect::<Vec<_>>()
                .join(", ");
            parts.push(format!("you called {}", part(&tools)));
        }
        parts.push(outcome.into());
        (self.position, self.time, self.id, parts.join(" — "))
    }
}

impl Search<'_> {
    fn before(&self, at: &TimelinePosition, inclusive: bool) -> QueryFilter {
        let (ordinal, sequence) = self.order(at);
        or(vec![
            one_of(
                "position.branch_id",
                self.chain[..ordinal].iter().map(|branch| json!(branch)),
            ),
            and(vec![
                one_of("position.branch_id", [json!(at.branch_id)]),
                compare(
                    "position.sequence",
                    if inclusive {
                        QueryComparison::Le
                    } else {
                        QueryComparison::Lt
                    },
                    json!(sequence),
                ),
            ]),
        ])
    }

    async fn opening_title(&self, id: &str) -> Result<Option<(u8, String)>, HostError> {
        let query = self.request(
            and(vec![kinds(&["input/message"]), one_of("id", [json!(id)])]),
            QuerySelect::Fields {
                fields: vec!["data".into()],
            },
        );
        let reply = self
            .branch
            .query(query)
            .await
            .map_err(HostError::from_agent)?;
        let QueryOutput::Rows { rows } = reply.result else {
            unreachable!("input properties")
        };
        let Some(row) = rows.first() else {
            return Ok(None);
        };
        let input: InputMessageData = serde_json::from_value(value(row, 0).clone())
            .map_err(|error| HostError::failed(error.to_string()))?;
        let current = self
            .current_keys(
                input
                    .blocks
                    .iter()
                    .filter_map(|block| {
                        let InputBlock::Raw { key, .. } = block;
                        key.clone()
                    })
                    .collect(),
            )
            .await?;
        let live: Vec<_> = input
            .blocks
            .iter()
            .enumerate()
            .map(|(index, block)| {
                let InputBlock::Raw { key, retire, .. } = block;
                !retire
                    && key.as_ref().is_some_and(|key| {
                        current
                            .get(key)
                            .is_some_and(|(current, at)| current == id && *at == index)
                    })
            })
            .collect();
        let write = line_write(&input);
        let priority = if input.user.is_some() {
            0
        } else if write.is_some() {
            1
        } else if input.effective_mode() != InputMode::Append {
            2
        } else {
            3
        };
        let title = match (write, &input.user) {
            (Some(write), _) => format!("turn: {}", write_line(&write, false)),
            (None, Some(user)) => format!("turn: user \"{}\"", part(&user_text(user))),
            (None, None) => format!("turn: {}", part(&blocks_name(&input.blocks, &live))),
        };
        Ok(Some((priority, title)))
    }
}

pub(super) async fn index(
    tree: &TreeCommands,
    branch: &Branch,
    branch_id: &str,
    calling_turn: &str,
    head: u64,
    arguments: &Arguments,
    cancel: &CancellationToken,
) -> Result<ToolResult, HostError> {
    let mut chain = vec![branch_id.to_owned()];
    while let Some(parent) = tree
        .info(chain.last().unwrap().clone())
        .map_err(HostError::from_agent)?
        .parent
    {
        chain.push(parent);
    }
    chain.reverse();
    let search = Search {
        branch,
        head,
        chain,
        cancel,
    };
    let before = if let Some(id) = &arguments.before {
        let Some(at) = search.anchor(id).await? else {
            return Ok(ToolResult::error(format!(
                "No turn or line has id {}.",
                part(id)
            )));
        };
        Some(at)
    } else {
        None
    };
    let mut conditions = vec![kinds(&["turn/start"])];
    if let Some(before) = &before {
        conditions.push(search.before(before, false));
    }
    let mut turns = search.request(
        and(conditions),
        QuerySelect::Fields {
            fields: vec!["position".into()],
        },
    );
    turns.query.order_by[0].direction = TimelineQueryDirection::Descending;
    turns.page.as_mut().unwrap().limit = NonZeroU64::new((PAGE_ENTRIES + 1) as u64).unwrap();
    let reply = branch.query(turns).await.map_err(HostError::from_agent)?;
    let QueryOutput::Rows { rows } = reply.result else {
        unreachable!("turn positions")
    };
    let from = if rows.len() > PAGE_ENTRIES {
        rows.last().map(|row| position(row, 0))
    } else {
        None
    };
    let mut conditions = vec![kinds(&[
        "turn/start",
        "turn/end",
        "branch/start",
        "input/consumed",
        "step/model-start",
        "step/tool-use-start",
        "stop/interrupt",
        "permission/changed",
        "context/compaction",
        "session/forked",
    ])];
    if let Some(before) = &before {
        let mut boundary = search.request(
            and(vec![
                kinds(&["turn/start", "turn/end", "branch/start"]),
                QueryFilter::Not {
                    condition: Box::new(search.before(before, false)),
                },
            ]),
            QuerySelect::Fields {
                fields: vec!["kind".into(), "position".into()],
            },
        );
        boundary.query.pick = Some(QueryPick::First);
        boundary.query.order_by.clear();
        boundary.page.as_mut().unwrap().limit = NonZeroU64::MIN;
        let reply = branch
            .query(boundary)
            .await
            .map_err(HostError::from_agent)?;
        let QueryOutput::Rows { rows } = reply.result else {
            unreachable!("turn boundary")
        };
        if let Some(row) = rows.first() {
            conditions.push(if value(row, 0) == "turn/end" {
                search.before(&position(row, 1), true)
            } else {
                search.before(before, false)
            });
        }
    }
    let mut query = search.request(
        and(conditions),
        QuerySelect::Fields {
            fields: [
                "id",
                "kind",
                "position",
                "data.input_id",
                "data.request_id",
                "data.call_id",
                "create_at",
                "data.turn_id",
                "data.name",
                "data.status",
                "data.parent",
            ]
            .map(String::from)
            .to_vec(),
        },
    );
    query.from = from;
    let mut open: Option<TurnSummary> = None;
    let mut entries = std::collections::VecDeque::new();
    let mut push = |line: (TimelinePosition, u64, String, String)| {
        if before
            .as_ref()
            .is_none_or(|before| search.order(&line.0) < search.order(before))
        {
            entries.push_back(line);
            if entries.len() > PAGE_ENTRIES + 1 {
                entries.pop_front();
            }
        }
    };
    let compaction = search.current_compaction().await?;
    loop {
        check_cancel(cancel)?;
        let reply = branch
            .query(query.clone())
            .await
            .map_err(HostError::from_agent)?;
        let QueryOutput::Rows { rows } = reply.result else {
            unreachable!("index properties")
        };
        for row in &rows {
            let kind = value(row, 1).as_str().unwrap();
            let at = position(row, 2);
            let id = value(row, 0).as_str().unwrap();
            let time = value(row, 6).as_u64().unwrap();
            match kind {
                "turn/start" => {
                    if let Some(turn) = open.take() {
                        let outcome = if turn.position.branch_id != branch_id
                            && turn.turn_id != calling_turn
                        {
                            "continued on the parent line"
                        } else {
                            "in progress"
                        };
                        push(turn.line(outcome));
                    }
                    open = Some(TurnSummary {
                        position: at,
                        id: id.into(),
                        time,
                        turn_id: value(row, 7).as_str().unwrap().into(),
                        title: None,
                        working: false,
                        tools: Vec::new(),
                    });
                }
                "turn/end" => {
                    if open
                        .as_ref()
                        .is_some_and(|turn| value(row, 7).as_str() == Some(turn.turn_id.as_str()))
                    {
                        let outcome = value(row, 9).as_str().unwrap();
                        push(open.take().unwrap().line(outcome));
                    }
                }
                "branch/start" => {
                    if let Some(turn) = open.take() {
                        push(turn.line("continued on the parent line"));
                    }
                    if at.branch_id == branch_id
                        && let QueryValue::Value {
                            value: Value::String(parent),
                        } = &row.values[10]
                    {
                        push((
                            at,
                            time,
                            id.into(),
                            format!("this line was opened by {}", part(parent)),
                        ));
                    }
                }
                "input/consumed" if open.is_some() => {
                    let turn = open.as_mut().unwrap();
                    if !turn.working
                        && turn
                            .title
                            .as_ref()
                            .is_none_or(|(priority, _)| *priority > 0)
                        && let Some(title) = search
                            .opening_title(value(row, 3).as_str().unwrap())
                            .await?
                        && turn
                            .title
                            .as_ref()
                            .is_none_or(|(priority, _)| title.0 < *priority)
                    {
                        turn.title = Some(title);
                    }
                }
                "step/model-start" if open.is_some() => open.as_mut().unwrap().working = true,
                "step/tool-use-start" if open.is_some() => {
                    let tools = &mut open.as_mut().unwrap().tools;
                    let name = value(row, 8).as_str().unwrap();
                    if let Some((_, count)) = tools.iter_mut().find(|(tool, _)| tool == name) {
                        *count += 1;
                    } else {
                        tools.push((name.into(), 1));
                    }
                }
                _ if open.is_some() => {}
                _ => {
                    let events: Vec<_> = search
                        .bodies(std::slice::from_ref(row))
                        .await?
                        .into_iter()
                        .map(|record| record.event)
                        .collect();
                    let keys = events
                        .iter()
                        .filter_map(|event| match &event.kind {
                            EventKind::InputMessage(data) => Some(&data.blocks),
                            _ => None,
                        })
                        .flatten()
                        .filter_map(|block| {
                            let InputBlock::Raw { key, .. } = block;
                            key.clone()
                        })
                        .collect();
                    let current = search.current_keys(keys).await?;
                    let mut narrative = Narrative::fold(&events, 0, events.len());
                    mark_current(&mut narrative, &current, compaction.as_deref());
                    if let Some(thing) = narrative
                        .things
                        .iter()
                        .find(|thing| events[thing.anchor].id == id)
                    {
                        let line = narrative.thing_line(thing);
                        push((at, line.time, line.id.into(), line.text));
                    }
                }
            }
        }
        let Some(next) = reply.next else {
            break;
        };
        query.page.as_mut().unwrap().after = Some(next);
    }
    if let Some(turn) = open {
        let outcome = if turn.position.branch_id != branch_id && turn.turn_id != calling_turn {
            "continued on the parent line"
        } else {
            "in progress"
        };
        push(turn.line(outcome));
    }
    let more = entries.len() > PAGE_ENTRIES;
    let lines: Vec<_> = entries.into_iter().rev().take(PAGE_ENTRIES).collect();
    let mut output = Vec::new();
    let mut day = None;
    for (_, time, id, text) in &lines {
        let date = date(*time);
        if day.as_deref() != Some(date.as_str()) {
            output.push(format!("{date} (UTC)"));
            day = Some(date);
        }
        output.push(format!("{} {id} {text}", clock(*time)));
    }
    output.push(if more {
        format!(
            "Earlier: before={}. Open one: open={}.",
            lines.last().unwrap().2,
            lines[0].2
        )
    } else {
        "Nothing earlier.".into()
    });
    Ok(page(
        "turns",
        output,
        &lines
            .iter()
            .map(|(_, _, id, _)| id.as_str())
            .collect::<Vec<_>>(),
        more.then(|| json!({"before":lines.last().unwrap().2})),
    ))
}

use super::*;
use dim_agent_runtime::{
    QueryComparison, QueryFilter, QueryOperand, QueryOutput, QueryPage, QueryPick, QuerySelect,
    QuerySpec, QueryValue, TimelineQuery, TimelineQueryScope,
};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::num::NonZeroU64;

pub(super) const KEY: &str = "prompt-state/recovered";
pub(super) const SCHEMA: &str = "prompt-state/v2";
pub(super) const SOURCES: &[&str] = &[
    "prompt/changed",
    "context/configured",
    "turn/start",
    "turn/end",
    "branch/start",
    "branch/resume",
    "context/scope",
];

#[derive(Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct EntryReference {
    source: String,
    scope: PromptScope,
    event_id: String,
    element_index: u64,
}

#[derive(Serialize)]
struct RecoveryState {
    entries: Vec<EntryReference>,
    revision: Option<PromptReference>,
    system_revision: Option<PromptReference>,
}

#[derive(Serialize)]
struct Recovered {
    schema: &'static str,
    changed_through: Option<TimelinePosition>,
    state: RecoveryState,
}

#[derive(Default)]
pub(super) struct Projection {
    pub(super) snapshot: PromptSnapshot,
    pub(super) configuration: Option<ContextConfiguredData>,
    entries: BTreeMap<Key, (String, u64, TimelinePosition)>,
    scoped: BTreeMap<PromptScope, BTreeSet<String>>,
    changed_through: Option<TimelinePosition>,
    scope_reads: BTreeMap<PromptScope, bool>,
    reset: Option<TimelinePosition>,
    pub(super) source_through: u64,
    pub(super) written_result: u64,
    published_result: u64,
    pub(super) opening: Option<(u64, PromptSnapshot)>,
}

fn request(
    scope: TimelineQueryScope,
    through: u64,
    kinds: &[&str],
    fields: &[&str],
) -> TimelineQuery {
    TimelineQuery {
        scope,
        from: None,
        through: Some(through),
        include_publishing: false,
        query: QuerySpec {
            filter: Some(QueryFilter::In {
                value: QueryOperand::Field {
                    path: "kind".into(),
                },
                values: kinds.iter().map(|kind| json!(kind)).collect(),
            }),
            select: QuerySelect::Fields {
                fields: fields.iter().map(|field| (*field).into()).collect(),
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

fn eq(field: &str, value: Value) -> QueryFilter {
    QueryFilter::Compare {
        left: QueryOperand::Field { path: field.into() },
        comparison: QueryComparison::Eq,
        right: QueryOperand::Value { value },
    }
}

fn decode<T: DeserializeOwned>(value: QueryValue) -> Result<T> {
    let QueryValue::Value { value } = value else {
        return Err(AgentError::fault("Prompt", "missing stored prompt field"));
    };
    serde_json::from_value(value).map_err(|error| AgentError::fault("Prompt", error.to_string()))
}

async fn last(owner: &Prompt, mut query: TimelineQuery) -> Result<Option<Vec<QueryValue>>> {
    query.query.pick = Some(QueryPick::Last);
    query.page.as_mut().unwrap().limit = NonZeroU64::MIN;
    let QueryOutput::Rows { mut rows } = owner.timeline.query(query).await?.result else {
        unreachable!("prompt fields")
    };
    Ok(rows.pop().map(|row| row.values))
}

async fn source_kind(
    owner: &Prompt,
    scope: TimelineQueryScope,
    through: u64,
    id: &str,
    key: &str,
) -> Result<EventKind> {
    let mut query = request(scope, through, &[key], &["data"]);
    query.query.filter = Some(QueryFilter::And {
        conditions: vec![query.query.filter.take().unwrap(), eq("id", json!(id))],
    });
    let [data] = <[_; 1]>::try_from(
        last(owner, query)
            .await?
            .ok_or_else(|| AgentError::fault("Prompt", "missing source fact"))?,
    )
    .expect("source data");
    let data: Value = decode(data)?;
    let (domain, name) = key.split_once('/').expect("source type key");
    serde_json::from_value(json!({"type":domain,"name":name,"data":data}))
        .map_err(|error| AgentError::fault("Prompt", error.to_string()))
}

fn rank(position: &TimelinePosition, segments: &[(String, u64)]) -> Result<(usize, u64)> {
    let index = segments
        .iter()
        .position(|(branch, _)| *branch == position.branch_id)
        .ok_or_else(|| AgentError::fault("Prompt", "scope boundary is outside the branch chain"))?;
    if position.sequence > segments[index].1 {
        return Err(AgentError::fault(
            "Prompt",
            "scope boundary exceeds its visible cutoff",
        ));
    }
    Ok((index, position.sequence))
}

/// Restricts `query` to rows strictly after `at` in prefix order: on `at`'s
/// branch above its sequence, or on any segment after `at`'s segment. The
/// mirror of `at_source`.
fn after_source(
    owner: &Prompt,
    query: &mut TimelineQuery,
    through: u64,
    at: &TimelinePosition,
) -> Result<()> {
    let segments = owner.timeline.segments(through)?;
    if let Some((own, _)) = segments.last()
        && *own == at.branch_id
    {
        // S is on this line's own branch, so everything after it is too.
        query.query.filter = Some(QueryFilter::And {
            conditions: vec![
                query.query.filter.take().expect("source kind filter"),
                eq("position.branch_id", json!(at.branch_id)),
                QueryFilter::Compare {
                    left: QueryOperand::Field {
                        path: "position.sequence".into(),
                    },
                    comparison: QueryComparison::Gt,
                    right: QueryOperand::Value {
                        value: json!(at.sequence),
                    },
                },
            ],
        });
        return Ok(());
    }
    let (last, _) = rank(at, &segments)?;
    let conditions = segments
        .iter()
        .enumerate()
        .filter(|(index, _)| *index >= last)
        .map(|(index, (branch, _))| {
            let branch_filter = eq("position.branch_id", json!(branch));
            if index != last {
                branch_filter
            } else {
                QueryFilter::And {
                    conditions: vec![
                        branch_filter,
                        QueryFilter::Compare {
                            left: QueryOperand::Field {
                                path: "position.sequence".into(),
                            },
                            comparison: QueryComparison::Gt,
                            right: QueryOperand::Value {
                                value: json!(at.sequence),
                            },
                        },
                    ],
                }
            }
        })
        .collect();
    query.query.filter = Some(QueryFilter::And {
        conditions: vec![
            query.query.filter.take().expect("source kind filter"),
            QueryFilter::Or { conditions },
        ],
    });
    Ok(())
}

/// The earliest change after the prefix's last request start that this line did
/// not receive, or `None` when it received them all.
///
/// A request consumes every pending input when it starts, so every change at or
/// before **S**, the last `turn/start`, was delivered. Only a change another
/// line wrote after **S** can be undelivered, and the earliest such change is
/// **E**: one query reads them, one `consumed` call over their union of inputs
/// answers all of them, and E is the first whose inputs are not all consumed.
async fn earliest_undelivered(owner: &Prompt, through: u64) -> Result<Option<TimelinePosition>> {
    let mut last_start = request(
        TimelineQueryScope::Chain,
        through,
        &["turn/start"],
        &["position"],
    );
    last_start.query.pick = Some(QueryPick::Last);
    last_start.page.as_mut().unwrap().limit = NonZeroU64::MIN;
    let Some(row) = last(owner, last_start).await? else {
        // Nothing has started a request, so nothing is consumed: every inherited
        // change is undelivered, and the first one is the answer.
        return first_undelivered(owner, through, None).await;
    };
    let [position] = <[_; 1]>::try_from(row).expect("request start position");
    let start: TimelinePosition = decode(position)?;
    if start.branch_id == owner.identity.branch_id {
        // Every inherited change precedes a request on the line itself.
        return Ok(None);
    }
    first_undelivered(owner, through, Some(start)).await
}

async fn first_undelivered(
    owner: &Prompt,
    through: u64,
    after: Option<TimelinePosition>,
) -> Result<Option<TimelinePosition>> {
    let mut query = request(
        TimelineQueryScope::Chain,
        through,
        &["prompt/changed"],
        &["position", "data.timeline", "data.inputs"],
    );
    if let Some(after) = &after {
        after_source(owner, &mut query, through, after)?;
    }
    let mut changes = Vec::new();
    let mut after = query.page.as_mut().unwrap().after.clone();
    loop {
        let mut page = query.clone();
        page.page.as_mut().unwrap().after = after.clone();
        let reply = owner.timeline.query(page).await?;
        let QueryOutput::Rows { rows } = reply.result else {
            unreachable!("prompt changes after a request start")
        };
        for row in rows {
            let [position, timeline, inputs] =
                <[_; 3]>::try_from(row.values).expect("change header");
            let position: TimelinePosition = decode(position)?;
            let inputs = match inputs {
                QueryValue::Missing => Vec::new(),
                inputs => decode(inputs)?,
            };
            let timeline: String = decode(timeline)?;
            if inputs.is_empty() || timeline == owner.identity.branch_id {
                continue;
            }
            changes.push((position, inputs));
        }
        let Some(next) = reply.next else { break };
        after = Some(next);
    }
    let union: Vec<String> = changes
        .iter()
        .flat_map(|(_, inputs)| inputs.iter().cloned())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let received = consumed(owner, &union, through).await?;
    Ok(changes
        .into_iter()
        .find(|(_, inputs)| !inputs.iter().all(|input| received.contains(input)))
        .map(|(position, _)| position))
}

fn at_source(
    owner: &Prompt,
    query: &mut TimelineQuery,
    through: u64,
    at: Option<&TimelinePosition>,
) -> Result<()> {
    let Some(at) = at else { return Ok(()) };
    if at.branch_id == owner.identity.branch_id {
        query.through = Some(at.sequence);
        return Ok(());
    }
    let segments = owner.timeline.segments(through)?;
    let (last, _) = rank(at, &segments)?;
    let conditions = segments
        .iter()
        .take(last + 1)
        .map(|(branch, _)| {
            let branch_filter = eq("position.branch_id", json!(branch));
            if *branch != at.branch_id {
                branch_filter
            } else {
                QueryFilter::And {
                    conditions: vec![
                        branch_filter,
                        QueryFilter::Compare {
                            left: QueryOperand::Field {
                                path: "position.sequence".into(),
                            },
                            comparison: QueryComparison::Le,
                            right: QueryOperand::Value {
                                value: json!(at.sequence),
                            },
                        },
                    ],
                }
            }
        })
        .collect();
    query.query.filter = Some(QueryFilter::And {
        conditions: vec![
            query.query.filter.take().expect("source kind filter"),
            QueryFilter::Or { conditions },
        ],
    });
    Ok(())
}

async fn reset_at(
    owner: &Prompt,
    through: u64,
    at: Option<&TimelinePosition>,
) -> Result<Option<TimelinePosition>> {
    let mut query = request(
        TimelineQueryScope::Chain,
        through,
        &["branch/start"],
        &["position"],
    );
    at_source(owner, &mut query, through, at)?;
    match last(owner, query).await? {
        Some(row) => {
            let [position] = <[_; 1]>::try_from(row).expect("reset position");
            Ok(Some(decode(position)?))
        }
        None => Ok(None),
    }
}

fn active_at(
    scope: &PromptScope,
    start: Option<&TimelinePosition>,
    end: Option<&TimelinePosition>,
    reset: Option<&TimelinePosition>,
    segments: &[(String, u64)],
) -> Result<bool> {
    if matches!(scope, PromptScope::Branch { .. }) {
        return Ok(true);
    }
    let Some(start) = start else { return Ok(false) };
    let start = rank(start, segments)?;
    if let Some(end) = end
        && rank(end, segments)? >= start
    {
        return Ok(false);
    }
    if matches!(scope, PromptScope::Turn { .. })
        && let Some(reset) = reset
        && rank(reset, segments)? > start
    {
        return Ok(false);
    }
    Ok(true)
}

fn set_active(snapshot: &mut PromptSnapshot, scope: &PromptScope, active: bool) {
    match scope {
        PromptScope::Turn { id, .. } => {
            if active {
                snapshot.turns.insert(id.clone());
            } else {
                snapshot.turns.remove(id);
            }
        }
        PromptScope::Task { .. } => {
            if active {
                snapshot.tasks.insert(scope.clone());
            } else {
                snapshot.tasks.remove(scope);
            }
        }
        _ => {}
    }
}

async fn scope_edges(
    owner: &Prompt,
    scope: &PromptScope,
    through: u64,
    at: Option<&TimelinePosition>,
) -> Result<(Option<TimelinePosition>, Option<TimelinePosition>)> {
    let mut result = Vec::new();
    for started in [true, false] {
        let mut query = match scope {
            PromptScope::Branch { .. } => return Ok((None, None)),
            PromptScope::Turn { timeline, id } => {
                let mut query = request(
                    TimelineQueryScope::Chain,
                    through,
                    &[if started { "turn/start" } else { "turn/end" }],
                    &["position"],
                );
                query.query.filter = Some(QueryFilter::And {
                    conditions: vec![
                        query.query.filter.take().unwrap(),
                        eq("data.turn_id", json!(id)),
                        eq("position.branch_id", json!(timeline)),
                    ],
                });
                query
            }
            PromptScope::Task { timeline, id } => {
                let mut query = request(
                    TimelineQueryScope::Chain,
                    through,
                    &["context/scope"],
                    &["position"],
                );
                query.query.filter = Some(QueryFilter::And {
                    conditions: vec![
                        query.query.filter.take().unwrap(),
                        eq("data.scope.kind", json!("task")),
                        eq("data.scope.timeline", json!(timeline)),
                        eq("data.scope.id", json!(id)),
                        eq(
                            "data.status",
                            json!(if started { "started" } else { "ended" }),
                        ),
                    ],
                });
                query
            }
        };
        at_source(owner, &mut query, through, at)?;
        query.page.as_mut().unwrap().limit = NonZeroU64::MIN;
        result.push(match last(owner, query).await? {
            Some(row) => {
                let [position] = <[_; 1]>::try_from(row).expect("scope position");
                Some(decode(position)?)
            }
            None => None,
        });
    }
    let [start, end] = <[_; 2]>::try_from(result).expect("two scope edges");
    Ok((start, end))
}

/// The `input/message` ids of `inputs` consumed in the chain up to `through`.
///
/// A change names the inputs written with it, and a block delivers the change
/// to the request that consumes the block. A line that inherits the change
/// through its prefix has delivered it only once every one of those inputs was
/// consumed there. The ids are known before this runs, so the query names them
/// and reads no other consumption.
async fn consumed(owner: &Prompt, inputs: &[String], through: u64) -> Result<BTreeSet<String>> {
    if inputs.is_empty() {
        return Ok(BTreeSet::new());
    }
    let mut query = request(
        TimelineQueryScope::Chain,
        through,
        &["input/consumed"],
        &["data.input_id"],
    );
    query.query.filter = Some(QueryFilter::And {
        conditions: vec![
            query.query.filter.take().expect("the kind filter"),
            QueryFilter::In {
                value: QueryOperand::Field {
                    path: "data.input_id".into(),
                },
                values: inputs.iter().map(|input| json!(input)).collect(),
            },
        ],
    });
    let mut consumed = BTreeSet::new();
    loop {
        let reply = owner.timeline.query(query.clone()).await?;
        let QueryOutput::Rows { rows } = reply.result else {
            unreachable!("consumption ids")
        };
        for row in rows {
            let QueryValue::Value {
                value: Value::String(input),
            } = row.values.into_iter().next().expect("one consumption id")
            else {
                // `InputConsumedData.input_id` is a required string, so an
                // event of this kind that decodes carries one. The projection
                // reads the stored body without decoding it, so a row holding
                // none names no input and delivers no change.
                continue;
            };
            consumed.insert(input);
        }
        let Some(next) = reply.next else {
            return Ok(consumed);
        };
        query.page.as_mut().unwrap().after = Some(next);
    }
}

/// Whether `change` has been delivered to a line whose own branch is `branch`.
///
/// A change that names no input is either the reading line's own or delivered
/// by the prefix's configuration, so it applies where it is found. One that
/// names inputs and was written by another line applies only once every one of
/// them was consumed in the prefix: an input never consumed leaves it
/// undelivered, and so does one of them.
pub(super) async fn delivered(
    owner: &Prompt,
    branch: &str,
    change: &PromptChangedData,
    through: u64,
) -> Result<bool> {
    if change.inputs.is_empty() || change.timeline == branch {
        return Ok(true);
    }
    let consumed = consumed(owner, &change.inputs, through).await?;
    Ok(change.inputs.iter().all(|input| consumed.contains(input)))
}

/// The boundary a stored checkpoint row names, resolved from its own schema.
///
/// `SCHEMA` writes `changed_through`; `prompt-state/v1` wrote only
/// `source_through`, so its boundary is the last change at or before that
/// position. The candidate loop needs the boundary before it knows which row it
/// will take, so the schema is resolved here and the result is reused for the
/// row that is selected.
async fn boundary_of(owner: &Prompt, row: &[QueryValue]) -> Result<Boundary> {
    let schema: String = decode(row[1].clone())?;
    match schema.as_str() {
        SCHEMA => Ok(Boundary {
            changed: decode(row[5].clone())?,
        }),
        "prompt-state/v1" => {
            let position: u64 = decode(row[6].clone())?;
            let changed = match last(
                owner,
                request(
                    TimelineQueryScope::Chain,
                    position,
                    &["prompt/changed"],
                    &["position"],
                ),
            )
            .await?
            {
                Some(values) => {
                    let [position] = <[_; 1]>::try_from(values).expect("changed position");
                    Some(decode(position)?)
                }
                None => None,
            };
            Ok(Boundary { changed })
        }
        _ => Err(AgentError::fault("Prompt", "unsupported recovery schema")),
    }
}

/// A checkpoint row's boundary, whatever schema wrote it.
struct Boundary {
    changed: Option<TimelinePosition>,
}

impl Projection {
    /// Applies one source fact. `delivered` says whether a `prompt/changed`
    /// reached this line: the caller answers it because the consumption query
    /// it needs cannot run with the projection locked.
    fn apply(&mut self, id: &str, kind: &EventKind, position: TimelinePosition, delivered: bool) {
        let EventKind::PromptChanged(data) = kind else {
            if let EventKind::ContextConfigured(data) = kind {
                self.configuration = Some(data.clone());
            }
            self.snapshot.apply_kind(kind);
            return;
        };
        // A change splits by delivery: a System entry is delivered by the
        // prefix's configuration, so it takes effect where the fact is found;
        // only the entries that render as blocks wait for their inputs. Only
        // the changes that pass go into the snapshot - re-applying the whole
        // event would put a gate-dropped entry back.
        let mut applied = Vec::new();
        for (index, change) in data.changes.iter().enumerate() {
            if !delivered && !self.system_change(change) {
                continue;
            }
            applied.push(change.clone());
            match change {
                PromptChange::Set { entry } if self.snapshot.active(&entry.scope) => {
                    self.entries.insert(
                        (entry.source.clone(), entry.scope.clone()),
                        (id.to_owned(), index as u64, position.clone()),
                    );
                    self.scoped
                        .entry(entry.scope.clone())
                        .or_default()
                        .insert(entry.source.clone());
                }
                PromptChange::Revoke { source, scope } => {
                    self.entries.remove(&(source.clone(), scope.clone()));
                    if let Some(sources) = self.scoped.get_mut(scope) {
                        sources.remove(source);
                        if sources.is_empty() {
                            self.scoped.remove(scope);
                        }
                    }
                }
                _ => {}
            }
        }
        // A change is a consumed source whether or not it delivered entries:
        // the suffix replay must not read it again.
        self.changed_through = Some(position);
        if !applied.is_empty() {
            self.snapshot
                .changed(&data.timeline, &data.revision, &applied);
        } else {
            // The boundary still moves: the revision names the change the
            // state was read at, whether or not it delivered entries.
            self.snapshot.revision = Some(PromptReference {
                timeline: data.timeline.clone(),
                revision: data.revision.clone(),
            });
        }
    }

    /// Whether `change` takes effect through the System prompt: a System entry
    /// is already in the prefix's configuration, so it is delivered without an
    /// input block.
    fn system_change(&self, change: &PromptChange) -> bool {
        match change {
            PromptChange::Set { entry } => entry.delivery == PromptDelivery::System,
            PromptChange::Revoke { source, scope } => self
                .snapshot
                .entries
                .get(&(source.clone(), scope.clone()))
                .is_some_and(|entry| entry.delivery == PromptDelivery::System),
        }
    }

    fn expire(&mut self, scope: &PromptScope) -> Vec<PromptEntry> {
        self.scoped
            .remove(scope)
            .into_iter()
            .flatten()
            .filter_map(|source| {
                let key = (source, scope.clone());
                self.entries.remove(&key);
                self.snapshot.entries.remove(&key)
            })
            .collect()
    }

    pub(super) fn checkpoint(&mut self, owner: &Prompt, blocks: Vec<InputBlock>) -> Result<()> {
        let recovered = Recovered {
            schema: SCHEMA,
            changed_through: self.changed_through.clone(),
            state: RecoveryState {
                entries: self
                    .entries
                    .iter()
                    .map(
                        |((source, scope), (event_id, element_index, _))| EntryReference {
                            source: source.clone(),
                            scope: scope.clone(),
                            event_id: event_id.clone(),
                            element_index: *element_index,
                        },
                    )
                    .collect(),
                revision: self.snapshot.revision.clone(),
                system_revision: self.snapshot.system_revision.clone(),
            },
        };
        let mut drafts: Vec<EventDraft> = blocks
            .into_iter()
            .map(|block| {
                EventKind::InputMessage(InputMessageData {
                    blocks: vec![block],
                    mode: Some(InputMode::Append),
                    ..Default::default()
                })
                .into()
            })
            .collect();
        drafts.push(
            EventKind::Extension(
                ExtensionEvent::new(
                    "prompt-state",
                    "recovered",
                    serde_json::to_value(recovered)
                        .map_err(|error| AgentError::fault("Prompt", error.to_string()))?,
                )
                .expect("private recovery key"),
            )
            .into(),
        );
        let expected: Vec<_> = drafts.iter().map(|draft| draft.kind.clone()).collect();
        let events = owner.timeline.append_batch_registered(drafts, |events| {
            if events
                .iter()
                .zip(&expected)
                .any(|(event, expected)| event.kind != *expected)
            {
                return Err(AgentError::InvalidInput(
                    "before changed Prompt retirement inputs or saved references".into(),
                ));
            }
            Ok(())
        })?;
        self.written_result = events.last().expect("prompt result").sequence;
        Ok(())
    }

    pub(super) fn publish(&mut self, owner: &Prompt, sequence: u64) {
        if sequence == self.written_result && sequence > self.published_result {
            owner.snapshot.set(self.snapshot.clone());
            self.published_result = sequence;
        }
        if self.written_result <= self.published_result {
            owner.settle_consumed(self.source_through);
        }
    }

    fn finish(&mut self, owner: &Prompt, expired: Vec<PromptEntry>, changed: bool) -> Result<()> {
        if changed || !expired.is_empty() {
            let mut expired = expired;
            expired.sort_by(|a, b| {
                (a.order, &a.source, &a.scope).cmp(&(b.order, &b.source, &b.scope))
            });
            self.checkpoint(
                owner,
                expired
                    .iter()
                    .filter(|entry| entry.delivery != PromptDelivery::System)
                    .map(|entry| context_input(entry, true, None, true))
                    .collect(),
            )?;
        } else if self.written_result <= self.published_result {
            owner.settle_consumed(self.source_through);
        }
        Ok(())
    }

    pub(super) fn step(
        &mut self,
        owner: &Prompt,
        id: &str,
        sequence: u64,
        kind: &EventKind,
        delivered: bool,
    ) -> Result<()> {
        if sequence <= self.source_through || !SOURCES.contains(&kind.key()) {
            return Ok(());
        }
        if let EventKind::TurnStart(data) = kind {
            return self.step_turn(owner, sequence, &data.turn_id, true);
        }
        if let EventKind::TurnEnd(data) = kind {
            return self.step_turn(owner, sequence, &data.turn_id, false);
        }
        let mut expired = Vec::new();
        if let EventKind::ContextScope(data) = kind
            && let Some(changed) = self.scope_reads.get_mut(&data.scope)
        {
            *changed = true;
        }
        match kind {
            EventKind::ContextScope(data) => match data.status {
                ContextScopeStatus::Started => {
                    self.snapshot.tasks.insert(data.scope.clone());
                    owner.snapshot.update(|snapshot| {
                        snapshot.tasks.insert(data.scope.clone());
                    });
                }
                ContextScopeStatus::Ended => {
                    self.snapshot.tasks.remove(&data.scope);
                    owner.snapshot.update(|snapshot| {
                        snapshot.tasks.remove(&data.scope);
                    });
                    expired = self.expire(&data.scope);
                }
            },
            EventKind::BranchStart(_) => {
                self.reset = Some(TimelinePosition {
                    branch_id: owner.identity.branch_id.clone(),
                    sequence,
                });
                for (scope, changed) in &mut self.scope_reads {
                    if matches!(scope, PromptScope::Turn { .. }) {
                        *changed = true;
                    }
                }
                owner.snapshot.update(|snapshot| snapshot.turns.clear());
                let scopes: Vec<_> = self
                    .scoped
                    .keys()
                    .filter(|scope| matches!(scope, PromptScope::Turn { .. }))
                    .cloned()
                    .collect();
                for scope in scopes {
                    expired.extend(self.expire(&scope));
                }
                self.snapshot.turns.clear();
            }
            EventKind::BranchResume(_) => {}
            EventKind::ContextConfigured(data) => {
                self.apply(
                    id,
                    kind,
                    TimelinePosition {
                        branch_id: owner.identity.branch_id.clone(),
                        sequence,
                    },
                    true,
                );
                owner
                    .snapshot
                    .update(|snapshot| snapshot.configuration = Some((*data.settings).clone()));
            }
            _ => self.apply(
                id,
                kind,
                TimelinePosition {
                    branch_id: owner.identity.branch_id.clone(),
                    sequence,
                },
                delivered,
            ),
        }
        self.source_through = sequence;
        if matches!(kind, EventKind::BranchStart(_) | EventKind::BranchResume(_)) {
            self.opening = Some((sequence, self.snapshot.clone()));
        }
        self.finish(owner, expired, matches!(kind, EventKind::PromptChanged(_)))
    }

    fn step_turn(
        &mut self,
        owner: &Prompt,
        sequence: u64,
        turn_id: &str,
        active: bool,
    ) -> Result<()> {
        if sequence <= self.source_through {
            return Ok(());
        }
        let scope = PromptScope::Turn {
            timeline: owner.identity.branch_id.clone(),
            id: turn_id.into(),
        };
        if let Some(changed) = self.scope_reads.get_mut(&scope) {
            *changed = true;
        }
        owner.snapshot.update(|snapshot| {
            if active {
                snapshot.turns.insert(turn_id.to_owned());
            } else {
                snapshot.turns.remove(turn_id);
            }
        });
        let expired = if active {
            self.snapshot.turns.insert(turn_id.to_owned());
            Vec::new()
        } else {
            self.snapshot.turns.remove(turn_id);
            self.expire(&PromptScope::Turn {
                timeline: owner.identity.branch_id.clone(),
                id: turn_id.into(),
            })
        };
        self.source_through = sequence;
        self.finish(owner, expired, false)
    }

    async fn set_scope_at(
        &mut self,
        owner: &Prompt,
        scope: &PromptScope,
        through: u64,
        at: Option<&TimelinePosition>,
    ) -> Result<Option<TimelinePosition>> {
        if matches!(scope, PromptScope::Branch { .. }) {
            return Ok(None);
        }
        let (start, end) = scope_edges(owner, scope, through, at).await?;
        let reset = if at.is_some() && matches!(scope, PromptScope::Turn { .. }) {
            reset_at(owner, through, at).await?
        } else {
            self.reset.clone()
        };
        let segments = owner.timeline.segments(through)?;
        set_active(
            &mut self.snapshot,
            scope,
            active_at(
                scope,
                start.as_ref(),
                end.as_ref(),
                reset.as_ref(),
                &segments,
            )?,
        );
        Ok(end)
    }

    pub(super) async fn restore(owner: &Prompt, through: u64) -> Result<Self> {
        let mut state = Self::default();
        if let Some(row) = last(
            owner,
            request(
                TimelineQueryScope::Chain,
                through,
                &["context/configured"],
                &["data"],
            ),
        )
        .await?
        {
            let [data] = <[_; 1]>::try_from(row).expect("configuration field");
            let data: ContextConfiguredData = decode(data)?;
            state.snapshot.configuration = Some((*data.settings).clone());
            state.configuration = Some(data);
        }
        let saved = request(
            TimelineQueryScope::Chain,
            through,
            &[KEY],
            &[
                "position",
                "data.schema",
                "data.state.entries",
                "data.state.revision",
                "data.state.system_revision",
                "data.changed_through",
                "data.source_through",
            ],
        );
        // A line starts from the newest checkpoint positioned before its first
        // undelivered change. A request consumes every pending input when it
        // starts, so every change at or before the prefix's last `turn/start`
        // was delivered; only a change another line wrote after it can be
        // undelivered, and the earliest of those is `E`. The row taken is the
        // newest one positioned before `E`, or the newest when there is no `E`.
        // Its boundary then precedes every undelivered change, so all the
        // changes it covers were delivered and nothing needs rejecting.
        //
        // Cost, constant in the length of history: one read for the last
        // request start, one for the changes after it, one consumption query
        // over their union of inputs, and at most two candidate reads, the
        // line's own row and then the chain's.
        let undelivered = earliest_undelivered(owner, through).await?;
        let mut scope = TimelineQueryScope::Branch;
        let saved_row = loop {
            let mut query = saved.clone();
            query.scope = scope;
            at_source(owner, &mut query, through, undelivered.as_ref())?;
            let Some(row) = last(owner, query).await? else {
                if scope == TimelineQueryScope::Branch && !owner.timeline.ancestors().is_empty() {
                    // The line has no checkpoint of its own: the chain's are
                    // its candidates.
                    scope = TimelineQueryScope::Chain;
                    continue;
                }
                // Nothing predates the first undelivered change. The gated
                // replay from nothing is right in both directions, a `Set` and
                // a `Revoke` alike, so restore initializes.
                break None;
            };
            let boundary = boundary_of(owner, &row).await?;
            break Some((row, boundary.changed));
        };
        let mut initialize = false;
        if let Some((row, boundary)) = saved_row {
            let [
                position,
                _schema,
                entries,
                revision,
                system_revision,
                _changed,
                _legacy,
            ] = <[_; 7]>::try_from(row).expect("prompt recovery fields");
            let position: TimelinePosition = decode(position)?;
            state.snapshot.revision = decode(revision)?;
            state.snapshot.system_revision = decode(system_revision)?;
            // `boundary_of` resolved this from the row's schema before the row
            // could be accepted; deriving it again here would ask the chain the
            // same question twice for every restore.
            state.changed_through = boundary;
            let segments = owner.timeline.segments(through)?;
            if state.snapshot.revision.is_some() != state.changed_through.is_some() {
                return Err(AgentError::fault(
                    "Prompt",
                    "prompt revision has no matching source boundary",
                ));
            }
            if let Some(changed) = &state.changed_through
                && rank(changed, &segments)? >= rank(&position, &segments)?
            {
                return Err(AgentError::fault(
                    "Prompt",
                    "prompt source boundary does not precede its result",
                ));
            }
            let references: Vec<EntryReference> = decode(entries)?;
            let mut groups: BTreeMap<String, Vec<EntryReference>> = BTreeMap::new();
            for entry in references {
                groups
                    .entry(entry.event_id.clone())
                    .or_default()
                    .push(entry);
            }
            for (id, references) in groups {
                for references in references.chunks(128) {
                    let mut query = request(
                        TimelineQueryScope::Chain,
                        through,
                        &["prompt/changed"],
                        &["position"],
                    );
                    query.query.filter = Some(QueryFilter::And {
                        conditions: vec![query.query.filter.take().unwrap(), eq("id", json!(id))],
                    });
                    let QuerySelect::Fields { fields } = &mut query.query.select else {
                        unreachable!()
                    };
                    fields.extend(
                        references
                            .iter()
                            .map(|entry| format!("data.changes[{}].entry", entry.element_index)),
                    );
                    let values = last(owner, query).await?.ok_or_else(|| {
                        AgentError::fault("Prompt", "missing effective prompt entry")
                    })?;
                    let mut values = values.into_iter();
                    let source: TimelinePosition = decode(values.next().expect("source position"))?;
                    // A checkpoint is written by the line that holds it, so
                    // its entries are its writer's view. A child adopts its
                    // parent's checkpoint, and an entry the child's own prefix
                    // never received must not enter with it: the change the
                    // entry points at is authoritative, so the consumption of
                    // its inputs decides. System entries are delivered by the
                    // prefix's configuration and are adopted either way. This
                    // is also how a checkpoint written before a change carried
                    // its inputs heals.
                    // Only the change's identity and its input names are
                    // read: decoding the whole event here would decode the
                    // entry's own array elements, and a restore must read only
                    // the elements it adopts.
                    let mut header = request(
                        TimelineQueryScope::Chain,
                        through,
                        &["prompt/changed"],
                        &["data.timeline", "data.inputs"],
                    );
                    header.query.filter = Some(QueryFilter::And {
                        conditions: vec![header.query.filter.take().unwrap(), eq("id", json!(id))],
                    });
                    header.page.as_mut().unwrap().limit = NonZeroU64::MIN;
                    let QueryOutput::Rows { rows } = owner.timeline.query(header).await?.result
                    else {
                        unreachable!("change header")
                    };
                    let row = rows
                        .into_iter()
                        .next()
                        .ok_or_else(|| AgentError::fault("Prompt", "missing prompt change"))?;
                    let [timeline, inputs] = <[_; 2]>::try_from(row.values).expect("change header");
                    // A change written before the field existed names no
                    // input: it applies where it is found.
                    let inputs = match inputs {
                        QueryValue::Missing => Vec::new(),
                        inputs => decode(inputs)?,
                    };
                    let change = PromptChangedData {
                        timeline: decode(timeline)?,
                        revision: String::new(),
                        previous: None,
                        reason: PromptChangeReason::Refresh,
                        changes: Vec::new(),
                        inputs,
                    };
                    let delivered =
                        delivered(owner, &owner.identity.branch_id, &change, through).await?;
                    for (reference, value) in references.iter().zip(values) {
                        let entry: PromptEntry = decode(value)?;
                        if entry.source != reference.source || entry.scope != reference.scope {
                            return Err(AgentError::fault(
                                "Prompt",
                                "prompt reference identity disagrees with its entry",
                            ));
                        }
                        if !delivered && entry.delivery != PromptDelivery::System {
                            continue;
                        }
                        let key = (entry.source.clone(), entry.scope.clone());
                        if state
                            .entries
                            .insert(
                                key.clone(),
                                (id.clone(), reference.element_index, source.clone()),
                            )
                            .is_some()
                        {
                            return Err(AgentError::fault("Prompt", "duplicate effective entry"));
                        }
                        state
                            .scoped
                            .entry(entry.scope.clone())
                            .or_default()
                            .insert(entry.source.clone());
                        state.snapshot.entries.insert(key, entry);
                    }
                }
            }
            if position.branch_id == owner.identity.branch_id {
                state.written_result = position.sequence;
                state.published_result = position.sequence;
            }
        } else {
            initialize = true;
        }
        let mut changes = request(
            TimelineQueryScope::Chain,
            through,
            &["prompt/changed"],
            &["id", "position"],
        );
        changes.from = state.changed_through.as_ref().map(|at| TimelinePosition {
            branch_id: at.branch_id.clone(),
            sequence: at.sequence.saturating_add(1),
        });
        let mut changed = false;
        loop {
            let reply = owner.timeline.query(changes.clone()).await?;
            let QueryOutput::Rows { rows } = reply.result else {
                unreachable!("changed identities")
            };
            for row in rows {
                let [id, position] = <[_; 2]>::try_from(row.values).expect("changed identity");
                let id: String = decode(id)?;
                let position: TimelinePosition = decode(position)?;
                let kind = source_kind(
                    owner,
                    TimelineQueryScope::Chain,
                    through,
                    &id,
                    "prompt/changed",
                )
                .await?;
                let EventKind::PromptChanged(data) = &kind else {
                    unreachable!()
                };
                let scopes: BTreeSet<_> = data
                    .changes
                    .iter()
                    .map(|change| match change {
                        PromptChange::Set { entry } => entry.scope.clone(),
                        PromptChange::Revoke { scope, .. } => scope.clone(),
                    })
                    .collect();
                for scope in scopes {
                    state
                        .set_scope_at(owner, &scope, through, Some(&position))
                        .await?;
                }
                let delivered = delivered(owner, &owner.identity.branch_id, data, through).await?;
                state.apply(&id, &kind, position, delivered);
                changed = true;
            }
            let Some(next) = reply.next else { break };
            changes.page.as_mut().unwrap().after = Some(next);
        }
        if state
            .scoped
            .keys()
            .any(|scope| matches!(scope, PromptScope::Turn { .. }))
        {
            state.reset = reset_at(owner, through, None).await?;
        }
        let reset = state.reset.clone();
        let segments = owner.timeline.segments(through)?;
        let scopes: Vec<_> = state.scoped.keys().cloned().collect();
        let mut expired = Vec::new();
        for scope in scopes {
            let end = state.set_scope_at(owner, &scope, through, None).await?;
            let mut remove = Vec::new();
            if let Some(sources) = state.scoped.get(&scope) {
                for source in sources {
                    let key = (source.clone(), scope.clone());
                    let at = &state.entries[&key].2;
                    let at_rank = rank(at, &segments)?;
                    let ended = match &end {
                        Some(end) => rank(end, &segments)? > at_rank,
                        None => false,
                    };
                    let reset = matches!(scope, PromptScope::Turn { .. })
                        && match &reset {
                            Some(reset) => rank(reset, &segments)? > at_rank,
                            None => false,
                        };
                    if ended || reset || !state.snapshot.active(&scope) {
                        remove.push(key);
                    }
                }
            }
            for key in remove {
                state.entries.remove(&key);
                if let Some(entry) = state.snapshot.entries.remove(&key) {
                    expired.push(entry);
                }
                if let Some(sources) = state.scoped.get_mut(&scope) {
                    sources.remove(&key.0);
                }
            }
            if state.scoped.get(&scope).is_some_and(BTreeSet::is_empty) {
                state.scoped.remove(&scope);
            }
        }
        if let Some(configuration) = &state.configuration
            && configuration.prompt == state.snapshot.system_revision
            && let Some(source) = &configuration.system_source
        {
            state.snapshot.system = source.clone();
        } else {
            state.snapshot.rebuild_system();
        }
        state.source_through = through;
        if initialize {
            expired.clear();
        }
        if changed || !expired.is_empty() {
            state.finish(owner, expired, true)?;
            owner
                .publisher
                .await_published(state.written_result)
                .await?;
        }
        Ok(state)
    }
}

impl Prompt {
    pub(super) async fn ensure_scopes(&self, scopes: &[PromptScope], through: u64) -> Result<()> {
        let segments = self.timeline.segments(through)?;
        for scope in scopes {
            let reset = {
                let mut projection = self.projection.lock().unwrap();
                if projection.snapshot.active(scope) {
                    continue;
                }
                projection.scope_reads.insert(scope.clone(), false);
                projection.reset.clone()
            };
            let edges = async {
                let reset = if matches!(scope, PromptScope::Turn { .. }) {
                    reset_at(self, through, None).await?
                } else {
                    reset
                };
                Ok::<_, AgentError>((scope_edges(self, scope, through, None).await?, reset))
            }
            .await;
            let mut projection = self.projection.lock().unwrap();
            let changed = projection
                .scope_reads
                .remove(scope)
                .expect("registered scope read");
            let ((start, end), reset) = edges?;
            if !changed {
                let active = active_at(
                    scope,
                    start.as_ref(),
                    end.as_ref(),
                    reset.as_ref(),
                    &segments,
                )?;
                set_active(&mut projection.snapshot, scope, active);
                self.snapshot
                    .update(|snapshot| set_active(snapshot, scope, active));
            }
        }
        Ok(())
    }

    pub(super) async fn opening_scopes(
        &self,
        snapshot: &mut PromptSnapshot,
        scopes: &[PromptScope],
        through: u64,
    ) -> Result<()> {
        let segments = self.timeline.segments(through)?;
        if scopes.iter().all(|scope| snapshot.active(scope)) {
            return Ok(());
        }
        let reset = if scopes
            .iter()
            .any(|scope| matches!(scope, PromptScope::Turn { .. }))
        {
            reset_at(self, through, None).await?
        } else {
            None
        };
        for scope in scopes {
            if snapshot.active(scope) {
                continue;
            }
            let (start, end) = scope_edges(self, scope, through, None).await?;
            set_active(
                snapshot,
                scope,
                active_at(
                    scope,
                    start.as_ref(),
                    end.as_ref(),
                    reset.as_ref(),
                    &segments,
                )?,
            );
        }
        Ok(())
    }
}

impl Prompt {
    pub(super) async fn advance(&self, through: u64) -> Result<()> {
        if self.branch_phase.phase() != BranchPhase::Active {
            return self.recover_sources(through).await;
        }
        let mut progress = self.dispatcher.watch();
        while self.dispatcher.sequence() < through {
            if let Some(error) = self.dispatcher.failure() {
                return Err(error);
            }
            tokio::select! {
                changed = progress.changed() => changed.map_err(|_|AgentError::Closed("Prompt".into()))?,
                _ = self.lifecycle.cancelled() => return Err(AgentError::Closed("Prompt".into())),
                _ = self.branch_phase.closing() => return Err(AgentError::Cancelled("prompt adoption".into())),
            }
        }
        let written = self.projection.lock().unwrap().written_result;
        self.publisher.await_published(written).await?;
        self.projection.lock().unwrap().publish(self, written);
        Ok(())
    }

    pub(super) async fn recover_sources(&self, through: u64) -> Result<()> {
        let from = self
            .projection
            .lock()
            .unwrap()
            .source_through
            .saturating_add(1);
        if from <= through {
            let mut query = request(
                TimelineQueryScope::Branch,
                through,
                SOURCES,
                &["id", "position", "kind", "data.turn_id"],
            );
            query.from = Some(TimelinePosition {
                branch_id: self.timeline.branch_id(),
                sequence: from,
            });
            loop {
                let reply = self.timeline.query(query.clone()).await?;
                let QueryOutput::Rows { rows } = reply.result else {
                    unreachable!("source fields")
                };
                for row in rows {
                    let [
                        QueryValue::Value { value: id },
                        QueryValue::Value { value: position },
                        QueryValue::Value { value: kind },
                        turn_id,
                    ] = <[_; 4]>::try_from(row.values).expect("source fields")
                    else {
                        unreachable!("source values")
                    };
                    let position: TimelinePosition = serde_json::from_value(position)
                        .map_err(|error| AgentError::fault("Prompt", error.to_string()))?;
                    if position.sequence <= self.projection.lock().unwrap().source_through {
                        continue;
                    }
                    let key = kind.as_str().expect("source kind");
                    if matches!(key, "turn/start" | "turn/end") {
                        let QueryValue::Value { value: turn_id } = turn_id else {
                            unreachable!("turn boundary has a turn id")
                        };
                        let mut state = self.projection.lock().unwrap();
                        state.step_turn(
                            self,
                            position.sequence,
                            turn_id.as_str().expect("turn identity"),
                            key == "turn/start",
                        )?;
                        continue;
                    }
                    let kind = source_kind(
                        self,
                        TimelineQueryScope::Branch,
                        through,
                        id.as_str().expect("source identity"),
                        key,
                    )
                    .await?;
                    let delivered = match &kind {
                        EventKind::PromptChanged(data) => {
                            delivered(self, &self.identity.branch_id, data, through).await?
                        }
                        _ => true,
                    };
                    self.projection.lock().unwrap().step(
                        self,
                        id.as_str().expect("source identity"),
                        position.sequence,
                        &kind,
                        delivered,
                    )?;
                }
                query.page.as_mut().unwrap().after = reply.next;
                if query.page.as_ref().unwrap().after.is_none() {
                    break;
                }
            }
        }
        let written = self.projection.lock().unwrap().written_result;
        self.publisher.await_published(written).await?;
        self.projection.lock().unwrap().publish(self, written);
        Ok(())
    }
}

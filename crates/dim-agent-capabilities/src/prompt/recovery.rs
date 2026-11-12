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

async fn last(timeline: &Timeline, mut query: TimelineQuery) -> Result<Option<Vec<QueryValue>>> {
    query.query.pick = Some(QueryPick::Last);
    query.page.as_mut().unwrap().limit = NonZeroU64::MIN;
    let QueryOutput::Rows { mut rows } = timeline.query(query).await?.result else {
        unreachable!("prompt fields")
    };
    Ok(rows.pop().map(|row| row.values))
}

async fn source_kind(
    timeline: &Timeline,
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
        last(timeline, query)
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

fn at_source(
    timeline: &Timeline,
    query: &mut TimelineQuery,
    through: u64,
    at: Option<&TimelinePosition>,
) -> Result<()> {
    let Some(at) = at else { return Ok(()) };
    if at.branch_id == timeline.branch_id() {
        query.through = Some(at.sequence);
        return Ok(());
    }
    let segments = timeline.segments(through)?;
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
    timeline: &Timeline,
    through: u64,
    at: Option<&TimelinePosition>,
) -> Result<Option<TimelinePosition>> {
    let mut query = request(
        TimelineQueryScope::Chain,
        through,
        &["branch/start"],
        &["position"],
    );
    at_source(timeline, &mut query, through, at)?;
    match last(timeline, query).await? {
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

async fn scope_boundary(
    timeline: &Timeline,
    scope: &PromptScope,
    through: u64,
    started: bool,
    at: Option<&TimelinePosition>,
) -> Result<Option<TimelinePosition>> {
    let mut query = match scope {
        PromptScope::Branch { .. } => return Ok(None),
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
    at_source(timeline, &mut query, through, at)?;
    query.page.as_mut().unwrap().limit = NonZeroU64::MIN;
    Ok(match last(timeline, query).await? {
        Some(row) => {
            let [position] = <[_; 1]>::try_from(row).expect("scope position");
            Some(decode(position)?)
        }
        None => None,
    })
}

async fn scope_edges(
    timeline: &Timeline,
    scope: &PromptScope,
    through: u64,
    at: Option<&TimelinePosition>,
) -> Result<(Option<TimelinePosition>, Option<TimelinePosition>)> {
    Ok((
        scope_boundary(timeline, scope, through, true, at).await?,
        scope_boundary(timeline, scope, through, false, at).await?,
    ))
}

impl Projection {
    fn apply(&mut self, id: &str, kind: &EventKind, position: TimelinePosition) {
        match kind {
            EventKind::PromptChanged(data) => {
                for (index, change) in data.changes.iter().enumerate() {
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
                self.changed_through = Some(position);
            }
            EventKind::ContextConfigured(data) => {
                self.configuration = Some(data.clone());
            }
            _ => {}
        }
        self.snapshot.apply_kind(kind);
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

    fn recovered(&self) -> Recovered {
        Recovered {
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
        }
    }

    pub(super) fn checkpoint(&mut self, owner: &Prompt, blocks: Vec<InputBlock>) -> Result<()> {
        let recovered = self.recovered();
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

    async fn load(
        timeline: &Timeline,
        through: u64,
        migration: bool,
    ) -> Result<(Self, Vec<PromptEntry>, bool)> {
        let mut state = Self::default();
        if let Some(row) = last(
            timeline,
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
        let mut saved = request(
            TimelineQueryScope::Branch,
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
        let mut saved_row = last(timeline, saved.clone()).await?;
        if saved_row.is_none() && !timeline.ancestors().is_empty() {
            saved.scope = TimelineQueryScope::Chain;
            saved_row = last(timeline, saved).await?;
        }
        let mut initialize = false;
        if let Some(row) = saved_row {
            let [
                position,
                schema,
                entries,
                revision,
                system_revision,
                changed,
                legacy_through,
            ] = <[_; 7]>::try_from(row).expect("prompt recovery fields");
            let position: TimelinePosition = decode(position)?;
            let schema: String = decode(schema)?;
            state.snapshot.revision = decode(revision)?;
            state.snapshot.system_revision = decode(system_revision)?;
            state.changed_through = match schema.as_str() {
                SCHEMA => decode(changed)?,
                "prompt-state/v1" => {
                    let legacy_through: u64 = decode(legacy_through)?;
                    let mut query = request(
                        TimelineQueryScope::Chain,
                        through,
                        &["prompt/changed"],
                        &["position"],
                    );
                    at_source(
                        timeline,
                        &mut query,
                        through,
                        Some(&TimelinePosition {
                            branch_id: position.branch_id.clone(),
                            sequence: legacy_through,
                        }),
                    )?;
                    match last(timeline, query).await? {
                        Some(row) => {
                            let [position] = <[_; 1]>::try_from(row).expect("changed position");
                            Some(decode(position)?)
                        }
                        None => None,
                    }
                }
                _ => return Err(AgentError::fault("Prompt", "unsupported recovery schema")),
            };
            let segments = timeline.segments(through)?;
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
                    let values = last(timeline, query).await?.ok_or_else(|| {
                        AgentError::fault("Prompt", "missing effective prompt entry")
                    })?;
                    let mut values = values.into_iter();
                    let source: TimelinePosition = decode(values.next().expect("source position"))?;
                    for (reference, value) in references.iter().zip(values) {
                        let entry: PromptEntry = decode(value)?;
                        if entry.source != reference.source || entry.scope != reference.scope {
                            return Err(AgentError::fault(
                                "Prompt",
                                "prompt reference identity disagrees with its entry",
                            ));
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
            if position.branch_id == timeline.branch_id() {
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
        let legacy = initialize && migration;
        let mut latest = if legacy {
            None
        } else {
            last(timeline, changes.clone()).await?
        };
        if initialize
            && !migration
            && let Some(latest) = &latest
        {
            let mut first = changes.clone();
            first.query.pick = Some(QueryPick::First);
            first.page.as_mut().unwrap().limit = NonZeroU64::MIN;
            let QueryOutput::Rows { rows } = timeline.query(first).await?.result else {
                unreachable!("first adoption identity")
            };
            if rows.first().is_none_or(|row| row.values[0] != latest[0]) {
                return Err(AgentError::InvalidInput(
                    "Prompt references require explicit migration".into(),
                ));
            }
        }
        let mut changed = false;
        {
            loop {
                let rows = if legacy {
                    let reply = timeline.query(changes.clone()).await?;
                    changes.page.as_mut().unwrap().after = reply.next;
                    let QueryOutput::Rows { rows } = reply.result else {
                        unreachable!("changed identities")
                    };
                    rows.into_iter().map(|row| row.values).collect::<Vec<_>>()
                } else {
                    latest.take().into_iter().collect()
                };
                for row in rows {
                    let [id, position] = <[_; 2]>::try_from(row).expect("changed identity");
                    let id: String = decode(id)?;
                    let position: TimelinePosition = decode(position)?;
                    let kind = source_kind(
                        timeline,
                        TimelineQueryScope::Chain,
                        through,
                        &id,
                        "prompt/changed",
                    )
                    .await?;
                    let EventKind::PromptChanged(data) = &kind else {
                        unreachable!()
                    };
                    if !legacy && data.previous != state.snapshot.revision {
                        return Err(AgentError::fault(
                            "Prompt",
                            "unrecorded adoption does not follow saved references",
                        ));
                    }
                    for change in &data.changes {
                        if let PromptChange::Set { entry } = change {
                            set_active(&mut state.snapshot, &entry.scope, true);
                        }
                    }
                    state.apply(&id, &kind, position);
                    changed = true;
                }
                if !legacy || changes.page.as_ref().unwrap().after.is_none() {
                    break;
                }
            }
        }
        state.snapshot.turns.clear();
        state.snapshot.tasks.clear();
        if state
            .scoped
            .keys()
            .any(|scope| matches!(scope, PromptScope::Turn { .. }))
        {
            state.reset = reset_at(timeline, through, None).await?;
        }
        let reset = state.reset.clone();
        let segments = timeline.segments(through)?;
        let scopes: Vec<_> = state.scoped.keys().cloned().collect();
        let mut expired = Vec::new();
        for scope in scopes {
            let end = scope_boundary(timeline, &scope, through, false, None).await?;
            set_active(&mut state.snapshot, &scope, true);
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
                set_active(&mut state.snapshot, &scope, false);
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
        if legacy {
            expired.clear();
        }
        Ok((state, expired, changed))
    }

    pub(super) async fn restore(owner: &Prompt, through: u64) -> Result<Self> {
        let (mut state, expired, changed) = Self::load(&owner.timeline, through, false).await?;
        if changed || !expired.is_empty() {
            state.finish(owner, expired, changed)?;
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
                    reset_at(&self.timeline, through, None).await?
                } else {
                    reset
                };
                Ok::<_, AgentError>((
                    scope_edges(&self.timeline, scope, through, None).await?,
                    reset,
                ))
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
            reset_at(&self.timeline, through, None).await?
        } else {
            None
        };
        for scope in scopes {
            if snapshot.active(scope) {
                continue;
            }
            let (start, end) = scope_edges(&self.timeline, scope, through, None).await?;
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
                        &self.timeline,
                        TimelineQueryScope::Branch,
                        through,
                        id.as_str().expect("source identity"),
                        key,
                    )
                    .await?;
                    self.projection.lock().unwrap().step(
                        self,
                        id.as_str().expect("source identity"),
                        position.sequence,
                        &kind,
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

pub(super) async fn migrate(timeline: &Timeline) -> Result<bool> {
    let through = timeline.committed();
    if last(
        timeline,
        request(TimelineQueryScope::Branch, through, &[KEY], &["id"]),
    )
    .await?
    .is_some()
    {
        return Ok(false);
    }
    let (state, _, changed) = Projection::load(timeline, through, true).await?;
    if !changed {
        return Ok(false);
    }
    let data = serde_json::to_value(state.recovered())
        .map_err(|error| AgentError::fault("Prompt", error.to_string()))?;
    let draft = EventKind::Extension(
        ExtensionEvent::new("prompt-state", "recovered", data.clone())
            .expect("private recovery key"),
    )
    .into();
    let events = timeline.append_batch_registered(vec![draft], |events| {
        if !matches!(&events[0].kind, EventKind::Extension(actual) if actual.key() == KEY && actual.data() == &data) {
            return Err(AgentError::InvalidInput("before changed migrated prompt references".into()));
        }
        Ok(())
    })?;
    let sequence = events[0].sequence;
    let mut published = timeline.watch();
    while published.borrow_and_update().0 < sequence {
        if let Some(error) = timeline.failure() {
            return Err(error);
        }
        published
            .changed()
            .await
            .map_err(|_| AgentError::Closed("Prompt migration publication".into()))?;
    }
    Ok(true)
}

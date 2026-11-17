use super::MemorySession;
use crate::{
    AgentError, QueryComparison, QueryCursor, QueryEvent, QueryFilter, QueryOperand, QueryOrderKey,
    QueryOutput, QueryPage, QueryPick, QueryResult, QueryRow, QuerySelect, QuerySpec, QueryValue,
    Result, TimelineQueryDirection, TimelineReadScope, event_property,
};
use dim_agent_types::{AgentEvent, TimelinePosition};
use serde_json::{Number, Value};
use std::{
    cmp::Ordering,
    collections::{BTreeMap, BTreeSet, BinaryHeap},
    sync::Arc,
};

#[derive(Clone, Copy)]
struct Record<'a> {
    event: &'a Arc<AgentEvent>,
    branch: &'a str,
    ordinal: usize,
}

impl Record<'_> {
    fn field(&self, path: &str) -> Result<QueryValue> {
        event_property(self.event, self.branch, path)
    }

    fn position(&self) -> (usize, u64) {
        (self.ordinal, self.event.sequence)
    }
}

struct Source<'a> {
    session: &'a MemorySession,
    scope: &'a TimelineReadScope,
}

impl<'a> Source<'a> {
    fn records(
        &self,
        after: Option<(usize, u64, bool)>,
    ) -> impl DoubleEndedIterator<Item = Record<'a>> + use<'a> {
        let session = self.session;
        self.scope
            .segments
            .iter()
            .enumerate()
            .flat_map(move |(ordinal, segment)| {
                let events = &session.branches[&segment.branch_id].events;
                let mut low = segment.from;
                let mut high = segment.through;
                if let Some((at, sequence, descending)) = after {
                    match ordinal.cmp(&at) {
                        Ordering::Equal if descending => {
                            high = high.min(sequence.saturating_sub(1))
                        }
                        Ordering::Equal => low = low.max(sequence.saturating_add(1)),
                        Ordering::Less if !descending => high = 0,
                        Ordering::Greater if descending => high = 0,
                        _ => (),
                    }
                }
                let from = usize::try_from(low.saturating_sub(1))
                    .unwrap_or(usize::MAX)
                    .min(events.len());
                let through = usize::try_from(segment.stored_through.min(high))
                    .unwrap_or(usize::MAX)
                    .min(events.len());
                events[from.min(through)..through]
                    .iter()
                    .map(|record| &record.event)
                    .chain(segment.tail.iter().filter(move |event| {
                        event.sequence >= low
                            && event.sequence > segment.stored_through
                            && event.sequence <= high
                    }))
                    .map(move |event| Record {
                        event,
                        branch: &segment.branch_id,
                        ordinal,
                    })
            })
    }
}

#[derive(Debug, Clone)]
struct Scalar(QueryValue);

impl Scalar {
    fn from_value(value: QueryValue) -> Result<Self> {
        if value.scalar_kind().is_none() {
            return Err(AgentError::InvalidInput(
                "ordering and grouping require scalar properties".into(),
            ));
        }
        Ok(Self(value))
    }

    fn number(value: Number) -> Self {
        Self(QueryValue::Value {
            value: Value::Number(value),
        })
    }

    fn value(&self) -> QueryValue {
        self.0.clone()
    }
}

impl Ord for Scalar {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0
            .scalar_cmp(&other.0)
            .expect("sort and group keys are scalar properties")
    }
}
impl PartialOrd for Scalar {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl PartialEq for Scalar {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}
impl Eq for Scalar {}

fn text_terms(terms: &[String]) -> Vec<String> {
    terms
        .iter()
        .map(|term| term.to_lowercase())
        .filter(|term| {
            (3..=200).contains(&term.len()) && term.bytes().all(|byte| byte.is_ascii_alphabetic())
        })
        .collect()
}

struct Related<'a> {
    condition: &'a QueryFilter,
    outer: Vec<&'a str>,
    keys: BTreeSet<Vec<Scalar>>,
}

struct Evaluation<'a> {
    source: Source<'a>,
    related: Vec<Related<'a>>,
    text_groups: Vec<(&'a QueryFilter, BTreeSet<Vec<Scalar>>)>,
}

#[derive(Clone, Copy)]
struct Element<'a> {
    index: usize,
    columns: &'a BTreeMap<&'a str, Vec<QueryValue>>,
}

impl<'a> Evaluation<'a> {
    fn prepare(&mut self, filter: &'a QueryFilter) -> Result<()> {
        match filter {
            QueryFilter::TextCandidate {
                field,
                partition_by,
                terms,
            } if field == "data.delta" && partition_by == &["kind", "data.request_id"] => {
                let terms = text_terms(terms);
                if terms.is_empty() {
                    return Ok(());
                }
                let mut groups: BTreeMap<Vec<Scalar>, (String, Vec<bool>)> = BTreeMap::new();
                for record in self.source.records(None) {
                    if record.event.kind.key() != "step/text" {
                        continue;
                    }
                    let Some(key) = join_key(
                        partition_by
                            .iter()
                            .map(|path| record.field(path))
                            .collect::<Result<Vec<_>>>()?,
                    ) else {
                        continue;
                    };
                    let QueryValue::Value {
                        value: Value::String(delta),
                    } = record.field(field)?
                    else {
                        continue;
                    };
                    let (tail, hits) = groups
                        .entry(key)
                        .or_insert_with(|| (String::new(), vec![false; terms.len()]));
                    let text = std::mem::take(tail) + &delta;
                    let folded = text.to_lowercase();
                    for (hit, term) in hits.iter_mut().zip(&terms) {
                        *hit |= folded.contains(term);
                    }
                    *tail = text
                        .chars()
                        .rev()
                        .take(199)
                        .collect::<Vec<_>>()
                        .into_iter()
                        .rev()
                        .collect();
                }
                self.text_groups.push((
                    filter,
                    groups
                        .into_iter()
                        .filter_map(|(key, (_, hits))| {
                            hits.into_iter().all(|hit| hit).then_some(key)
                        })
                        .collect(),
                ));
            }

            QueryFilter::Exists { condition } => {
                self.prepare(condition)?;
                let mut terms = Vec::new();
                conjuncts(condition, &mut terms);
                let mut inner = Vec::new();
                let mut outer = Vec::new();
                let mut remaining = Vec::new();
                for term in terms {
                    match term {
                        QueryFilter::Compare {
                            left: QueryOperand::Field { path: a },
                            comparison: QueryComparison::Eq,
                            right: QueryOperand::Outer { path: b },
                        }
                        | QueryFilter::Compare {
                            left: QueryOperand::Outer { path: b },
                            comparison: QueryComparison::Eq,
                            right: QueryOperand::Field { path: a },
                        } => {
                            inner.push(a.as_str());
                            outer.push(b.as_str());
                        }
                        _ if has_outer(term) => return Ok(()),
                        _ => remaining.push(term),
                    }
                }
                let mut keys = BTreeSet::new();
                for record in self.source.records(None) {
                    let mut accepted = true;
                    for condition in &remaining {
                        if !self.matches(condition, record, record, None, None)? {
                            accepted = false;
                            break;
                        }
                    }
                    if !accepted {
                        continue;
                    }
                    let values = inner
                        .iter()
                        .map(|path| record.field(path))
                        .collect::<Result<Vec<_>>>()?;
                    if let Some(key) = join_key(values) {
                        keys.insert(key);
                        if inner.is_empty() {
                            break;
                        }
                    }
                }
                self.related.push(Related {
                    condition,
                    outer,
                    keys,
                });
            }
            QueryFilter::And { conditions } | QueryFilter::Or { conditions } => {
                for condition in conditions {
                    self.prepare(condition)?;
                }
            }
            QueryFilter::Not { condition } => self.prepare(condition)?,
            _ => {}
        }
        Ok(())
    }

    fn operand(
        &self,
        operand: &QueryOperand,
        record: Record<'a>,
        root: Record<'a>,
        outer: Option<Record<'a>>,
        element: Option<Element<'_>>,
    ) -> Result<QueryValue> {
        match operand {
            QueryOperand::Field { path } => record.field(path),
            QueryOperand::Root { path } => root.field(path),
            QueryOperand::Concat { parts } => {
                let mut text = String::new();
                for part in parts {
                    let QueryValue::Value {
                        value: Value::String(value),
                    } = self.operand(part, record, root, outer, element)?
                    else {
                        return Ok(QueryValue::Missing);
                    };
                    text.push_str(&value);
                }
                Ok(QueryValue::Value { value: text.into() })
            }
            QueryOperand::Outer { path } => {
                outer.expect("validated correlated operand").field(path)
            }
            QueryOperand::Element { path } => {
                let element = element.expect("validated element operand");
                Ok(element.columns[path.as_str()][element.index].clone())
            }
            QueryOperand::Value { value } => Ok(QueryValue::Value {
                value: value.clone(),
            }),
        }
    }

    fn matches(
        &self,
        filter: &QueryFilter,
        record: Record<'a>,
        root: Record<'a>,
        outer: Option<Record<'a>>,
        element: Option<Element<'_>>,
    ) -> Result<bool> {
        let operand = |value| self.operand(value, record, root, outer, element);
        Ok(match filter {
            QueryFilter::Compare {
                left,
                comparison,
                right,
            } => comparison.matches(&operand(left)?, &operand(right)?),
            QueryFilter::In { value, values } => {
                let value = operand(value)?;
                values.iter().any(|v| {
                    QueryComparison::Eq.matches(&value, &QueryValue::Value { value: v.clone() })
                })
            }
            QueryFilter::Present { value } => match value {
                QueryOperand::Field { path } => crate::event_property_present(record.event, path)?,
                QueryOperand::Outer { path } => crate::event_property_present(
                    outer.expect("validated correlated operand").event,
                    path,
                )?,
                _ => !matches!(operand(value)?, QueryValue::Missing),
            },
            QueryFilter::Prefix { value, prefix } => {
                matches!(operand(value)?, QueryValue::Value { value: Value::String(s) } if s.starts_with(prefix))
            }
            QueryFilter::TextCandidate {
                field,
                partition_by,
                terms,
            } => {
                if field == "data" && partition_by.is_empty() {
                    let terms = text_terms(terms);
                    let text = match record.field(field)? {
                        QueryValue::Value {
                            value: Value::String(text),
                        } => text,
                        QueryValue::Value { value } => value.to_string(),
                        _ => String::new(),
                    }
                    .to_lowercase();
                    terms.iter().all(|term| text.contains(term))
                } else if let Some((_, keys)) = self
                    .text_groups
                    .iter()
                    .find(|(candidate, _)| *candidate == filter)
                {
                    if record.event.kind.key() != "step/text" {
                        true
                    } else {
                        join_key(
                            partition_by
                                .iter()
                                .map(|path| record.field(path))
                                .collect::<Result<Vec<_>>>()?,
                        )
                        .is_none_or(|key| keys.contains(&key))
                    }
                } else {
                    true
                }
            }
            QueryFilter::Contains { value, text } => {
                matches!(operand(value)?, QueryValue::Value { value: Value::String(s) } if s.contains(text))
            }
            QueryFilter::And { conditions } => {
                for condition in conditions {
                    if !self.matches(condition, record, root, outer, element)? {
                        return Ok(false);
                    }
                }
                true
            }
            QueryFilter::Or { conditions } => {
                for condition in conditions {
                    if self.matches(condition, record, root, outer, element)? {
                        return Ok(true);
                    }
                }
                false
            }
            QueryFilter::Not { condition } => {
                !self.matches(condition, record, root, outer, element)?
            }
            QueryFilter::Exists { condition } => {
                if let Some(index) = self
                    .related
                    .iter()
                    .find(|index| std::ptr::eq(index.condition, condition.as_ref()))
                {
                    let values = index
                        .outer
                        .iter()
                        .map(|path| record.field(path))
                        .collect::<Result<Vec<_>>>()?;
                    join_key(values).is_some_and(|key| index.keys.contains(&key))
                } else {
                    for inner in self.source.records(None) {
                        if self.matches(condition, inner, root, Some(record), None)? {
                            return Ok(true);
                        }
                    }
                    false
                }
            }
            QueryFilter::Any { field, condition } => {
                let Some(length) = crate::event_property::array_length(record.event, field)? else {
                    return Ok(false);
                };
                let mut paths = BTreeMap::new();
                element_paths(condition, &mut paths);
                let columns = paths
                    .into_iter()
                    .map(|(path, value)| {
                        Ok((
                            path,
                            crate::event_property::elements(record.event, field, path, value)?,
                        ))
                    })
                    .collect::<Result<BTreeMap<_, _>>>()?;
                for index in 0..length {
                    if self.matches(
                        condition,
                        record,
                        root,
                        outer,
                        Some(Element {
                            index,
                            columns: &columns,
                        }),
                    )? {
                        return Ok(true);
                    }
                }
                false
            }
        })
    }
}

fn element_paths<'a>(filter: &'a QueryFilter, paths: &mut BTreeMap<&'a str, bool>) {
    fn visit<'a>(operand: &'a QueryOperand, value: bool, paths: &mut BTreeMap<&'a str, bool>) {
        match operand {
            QueryOperand::Element { path } => *paths.entry(path).or_default() |= value,
            QueryOperand::Concat { parts } => {
                for part in parts {
                    visit(part, value, paths);
                }
            }
            _ => {}
        }
    }
    let mut operand = |operand, value| visit(operand, value, paths);
    match filter {
        QueryFilter::Compare { left, right, .. } => {
            operand(left, true);
            operand(right, true);
        }
        QueryFilter::Present { value } => operand(value, false),
        QueryFilter::In { value, .. }
        | QueryFilter::Prefix { value, .. }
        | QueryFilter::Contains { value, .. } => operand(value, true),
        QueryFilter::And { conditions } | QueryFilter::Or { conditions } => {
            for condition in conditions {
                element_paths(condition, paths);
            }
        }
        QueryFilter::Not { condition } => element_paths(condition, paths),
        _ => {}
    }
}

fn conjuncts<'a>(filter: &'a QueryFilter, terms: &mut Vec<&'a QueryFilter>) {
    match filter {
        QueryFilter::And { conditions } => {
            for condition in conditions {
                conjuncts(condition, terms);
            }
        }
        _ => terms.push(filter),
    }
}

fn has_outer(filter: &QueryFilter) -> bool {
    fn outer(value: &QueryOperand) -> bool {
        match value {
            QueryOperand::Outer { .. } | QueryOperand::Root { .. } => true,
            QueryOperand::Concat { parts } => parts.iter().any(outer),
            _ => false,
        }
    }
    match filter {
        QueryFilter::TextCandidate { .. } => false,
        QueryFilter::Compare { left, right, .. } => outer(left) || outer(right),
        QueryFilter::In { value, .. }
        | QueryFilter::Present { value }
        | QueryFilter::Prefix { value, .. }
        | QueryFilter::Contains { value, .. } => outer(value),
        QueryFilter::And { conditions } | QueryFilter::Or { conditions } => {
            conditions.iter().any(has_outer)
        }
        QueryFilter::Not { condition }
        | QueryFilter::Any { condition, .. }
        | QueryFilter::Exists { condition } => has_outer(condition),
    }
}

fn join_key(values: Vec<QueryValue>) -> Option<Vec<Scalar>> {
    values
        .into_iter()
        .map(|value| {
            if matches!(value, QueryValue::Missing) {
                None
            } else {
                Scalar::from_value(value).ok()
            }
        })
        .collect()
}

#[derive(Clone, Eq, PartialEq)]
struct SortKey {
    value: Scalar,
    descending: bool,
}
impl Ord for SortKey {
    fn cmp(&self, other: &Self) -> Ordering {
        let order = self.value.cmp(&other.value);
        if self.descending {
            order.reverse()
        } else {
            order
        }
    }
}
impl PartialOrd for SortKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

struct Candidate<'a> {
    record: Record<'a>,
    group: Vec<Scalar>,
    count: u64,
    order: Vec<SortKey>,
}
impl Ord for Candidate<'_> {
    fn cmp(&self, other: &Self) -> Ordering {
        self.order.cmp(&other.order)
    }
}
impl PartialOrd for Candidate<'_> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl PartialEq for Candidate<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.order == other.order
    }
}
impl Eq for Candidate<'_> {}

fn order_keys(query: &QuerySpec, candidate: &Candidate<'_>) -> Result<Vec<SortKey>> {
    let mut keys = Vec::new();
    for order in &query.order_by {
        let descending = order.direction == TimelineQueryDirection::Descending;
        match &order.key {
            QueryOrderKey::Field { path } => keys.push(SortKey {
                value: Scalar::from_value(candidate.record.field(path)?)?,
                descending,
            }),
            QueryOrderKey::Count => keys.push(SortKey {
                value: Scalar::number(candidate.count.into()),
                descending,
            }),
            QueryOrderKey::Position => {
                keys.push(SortKey {
                    value: Scalar::number(candidate.record.ordinal.into()),
                    descending,
                });
                keys.push(SortKey {
                    value: Scalar::number(candidate.record.event.sequence.into()),
                    descending,
                });
            }
        }
    }
    if matches!(query.select, QuerySelect::Count) {
        keys.extend(candidate.group.iter().cloned().map(|value| SortKey {
            value,
            descending: false,
        }));
    } else {
        keys.push(SortKey {
            value: Scalar::number(candidate.record.ordinal.into()),
            descending: false,
        });
        keys.push(SortKey {
            value: Scalar::number(candidate.record.event.sequence.into()),
            descending: false,
        });
    }
    Ok(keys)
}

pub(super) fn execute<'a>(
    session: &'a MemorySession,
    scope: &'a TimelineReadScope,
    query: &'a QuerySpec,
    page: Option<&QueryPage>,
) -> Result<QueryResult> {
    for segment in &scope.segments {
        let branch = session
            .branches
            .get(&segment.branch_id)
            .ok_or_else(|| AgentError::Storage(format!("unknown branch {}", segment.branch_id)))?;
        if segment.stored_through.min(segment.through) > branch.events.len() as u64 {
            return Err(AgentError::Storage(
                "property read exceeds stored history".into(),
            ));
        }
    }
    let binding = scope.query_binding(query)?;
    if page
        .and_then(|p| p.after.as_ref())
        .is_some_and(|after| after.binding != binding)
    {
        return Err(AgentError::InvalidInput(
            "query cursor belongs to another query or scope".into(),
        ));
    }
    let mut evaluation = Evaluation {
        source: Source { session, scope },
        related: Vec::new(),
        text_groups: Vec::new(),
    };
    if let Some(filter) = &query.filter {
        evaluation.prepare(filter)?;
    }
    let grouped = !query.group_by.is_empty() || query.pick.is_some();
    let single_pick = query.group_by.is_empty() && query.pick.is_some();
    let mut groups: BTreeMap<Vec<Scalar>, (Record<'a>, u64)> = BTreeMap::new();
    let mut count = 0;
    let limit = page
        .map(|p| usize::try_from(p.limit.get()).unwrap_or(usize::MAX))
        .unwrap_or(0);
    let mut heap: BinaryHeap<Candidate<'a>> = BinaryHeap::new();
    let mut directions = Vec::new();
    for order in &query.order_by {
        directions.push(order.direction == TimelineQueryDirection::Descending);
        if order.key == QueryOrderKey::Position {
            directions.push(order.direction == TimelineQueryDirection::Descending);
        }
    }
    directions.extend(std::iter::repeat_n(
        false,
        if matches!(query.select, QuerySelect::Count) {
            query.group_by.len()
        } else {
            2
        },
    ));
    let after = page
        .and_then(|p| p.after.as_ref())
        .map(|after| {
            if after.keys.len() != directions.len() {
                return Err(AgentError::InvalidInput(
                    "query cursor has an invalid sort key".into(),
                ));
            }
            after
                .keys
                .iter()
                .cloned()
                .zip(&directions)
                .map(|(value, descending)| {
                    Ok(SortKey {
                        value: Scalar::from_value(value)?,
                        descending: *descending,
                    })
                })
                .collect::<Result<Vec<_>>>()
        })
        .transpose()?;
    let mut push = |mut candidate: Candidate<'a>| -> Result<bool> {
        candidate.order = order_keys(query, &candidate)?;
        if let Some(after) = &after
            && candidate.order <= *after
        {
            return Ok(false);
        }
        if heap.len() < limit.saturating_add(1) {
            heap.push(candidate);
        } else if heap.peek().is_some_and(|last| candidate.order < last.order) {
            *heap.peek_mut().expect("a full page has a last row") = candidate;
        }
        Ok(heap.len() > limit)
    };
    let position_order = match query.order_by.as_slice() {
        [] => Some(TimelineQueryDirection::Ascending),
        [order] if order.key == QueryOrderKey::Position => Some(order.direction),
        _ => None,
    };
    let ordered = !grouped && position_order.is_some();
    let position_after = if ordered {
        page.and_then(|page| page.after.as_ref())
            .map(|cursor| {
                let invalid =
                    || AgentError::InvalidInput("invalid position in query cursor".into());
                let [
                    QueryValue::Value { value: ordinal },
                    QueryValue::Value { value: sequence },
                    ..,
                ] = cursor.keys.as_slice()
                else {
                    return Err(invalid());
                };
                Ok((
                    usize::try_from(ordinal.as_u64().ok_or_else(invalid)?)
                        .map_err(|_| invalid())?,
                    sequence.as_u64().ok_or_else(invalid)?,
                    position_order == Some(TimelineQueryDirection::Descending),
                ))
            })
            .transpose()?
    } else {
        None
    };
    let records: Box<dyn Iterator<Item = Record<'a>>> = if (ordered
        && position_order == Some(TimelineQueryDirection::Descending))
        || (single_pick && query.pick == Some(QueryPick::Last))
    {
        Box::new(evaluation.source.records(position_after).rev())
    } else {
        Box::new(evaluation.source.records(position_after))
    };
    for record in records {
        if let Some(filter) = &query.filter
            && !evaluation.matches(filter, record, record, None, None)?
        {
            continue;
        }
        if let QuerySelect::Characters { field } = &query.select {
            if let QueryValue::Value {
                value: Value::String(text),
            } = record.field(field)?
            {
                count += text.chars().count() as u64;
            }
            continue;
        }
        count += 1;
        if matches!(query.select, QuerySelect::Exists) {
            return Ok(QueryResult {
                result: QueryOutput::Exists { exists: true },
                next: None,
            });
        }
        if grouped {
            let group = query
                .group_by
                .iter()
                .map(|field| Scalar::from_value(record.field(field)?))
                .collect::<Result<Vec<_>>>()?;
            let entry = groups.entry(group).or_insert((record, 0));
            entry.1 += 1;
            if query.pick == Some(QueryPick::Last) && record.position() > entry.0.position() {
                entry.0 = record;
            }
            if single_pick {
                break;
            }
        } else if !matches!(query.select, QuerySelect::Count) {
            let full = push(Candidate {
                record,
                group: Vec::new(),
                count: 1,
                order: Vec::new(),
            })?;
            if full && ordered {
                break;
            }
        }
    }
    for (group, (record, count)) in groups {
        push(Candidate {
            record,
            group,
            count,
            order: Vec::new(),
        })?;
    }
    let mut selected = heap.into_sorted_vec();
    let more = selected.len() > limit;
    selected.truncate(limit);
    let next = if more {
        selected.last().map(|row| QueryCursor {
            binding,
            keys: row.order.iter().map(|key| key.value.value()).collect(),
        })
    } else {
        None
    };
    let result = match &query.select {
        QuerySelect::Exists => QueryOutput::Exists { exists: false },
        QuerySelect::Characters { .. } => QueryOutput::Count { count },
        QuerySelect::Count if query.group_by.is_empty() => QueryOutput::Count { count },
        QuerySelect::Count => QueryOutput::Rows {
            rows: selected
                .into_iter()
                .map(|row| QueryRow {
                    group: row.group.iter().map(Scalar::value).collect(),
                    values: vec![QueryValue::Value {
                        value: row.count.into(),
                    }],
                })
                .collect(),
        },
        QuerySelect::Events => QueryOutput::Events {
            events: selected
                .into_iter()
                .map(|row| QueryEvent {
                    group: row.group.iter().map(Scalar::value).collect(),
                    position: TimelinePosition {
                        branch_id: row.record.branch.into(),
                        sequence: row.record.event.sequence,
                    },
                    event: row.record.event.clone(),
                })
                .collect(),
        },
        QuerySelect::Fields { fields } => QueryOutput::Rows {
            rows: selected
                .into_iter()
                .map(|row| {
                    Ok(QueryRow {
                        group: row.group.iter().map(Scalar::value).collect(),
                        values: fields
                            .iter()
                            .map(|field| row.record.field(field))
                            .collect::<Result<_>>()?,
                    })
                })
                .collect::<Result<_>>()?,
        },
    };
    Ok(QueryResult { result, next })
}

use super::{
    codec::{Reader, WireEvent},
    sqlite::error,
};
use dim_agent_runtime::{
    AgentError, QueryComparison, QueryCursor, QueryEvent, QueryFilter, QueryOperand, QueryOrderKey,
    QueryOutput, QueryPage, QueryPick, QueryResult, QueryRow, QuerySelect, QuerySpec, QueryValue,
    Result, TimelineQueryDirection, TimelineReadScope, event_property, event_property_present,
};
use dim_agent_types::TimelinePosition;
use rusqlite::{
    Connection, OptionalExtension, functions::FunctionFlags, params, params_from_iter,
    types::Value as SqlValue,
};
use serde_json::Value;
use std::collections::BTreeMap;
use tokio_util::sync::CancellationToken;

#[cfg(feature = "testing")]
#[derive(Debug, Clone, serde::Serialize)]
pub struct QueryStatementStats {
    pub stage: &'static str,
    pub plan: Vec<String>,
    /// Placeholders the statement declares. The compiler numbers every parameter
    /// it binds, so this counts the roles that reached the SQL.
    pub parameters: usize,
    pub full_scan_steps: i32,
    pub vm_steps: i32,
    pub sorts: i32,
    pub automatic_index_rows: i32,
    /// Process-wide SQLite allocation after execution, before plan diagnostics.
    pub sqlite_bytes: i64,
    /// Process-wide high-water mark since the last explicit reset, not an isolated statement peak.
    pub sqlite_peak_bytes: i64,
}

/// SQLite's process-wide allocated bytes and high-water mark. Feature `testing` only.
/// `reset_peak` resets the global high-water mark after reading it.
#[cfg(feature = "testing")]
pub fn sqlite_memory(reset_peak: bool) -> (i64, i64) {
    let (mut current, mut peak) = (0, 0);
    // SAFETY: both output pointers remain valid for the duration of the call.
    let result = unsafe {
        rusqlite::ffi::sqlite3_status64(
            rusqlite::ffi::SQLITE_STATUS_MEMORY_USED,
            &mut current,
            &mut peak,
            i32::from(reset_peak),
        )
    };
    assert_eq!(result, rusqlite::ffi::SQLITE_OK);
    (current, peak)
}

#[cfg(feature = "testing")]
pub(super) type Statistics = std::sync::Mutex<Option<Vec<QueryStatementStats>>>;

#[cfg(feature = "testing")]
pub(super) fn record_statement(
    statistics: &Statistics,
    connection: &Connection,
    statement: &rusqlite::Statement<'_>,
    sql: &str,
    parameters: &[SqlValue],
    stage: &'static str,
) -> Result<()> {
    use rusqlite::StatementStatus;
    let mut statistics = statistics.lock().unwrap();
    let Some(statistics) = statistics.as_mut() else {
        return Ok(());
    };
    let (sqlite_bytes, sqlite_peak_bytes) = sqlite_memory(false);
    let plan = connection
        .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
        .map_err(error)?
        .query_map(params_from_iter(parameters), |row| row.get(3))
        .map_err(error)?
        .collect::<rusqlite::Result<Vec<String>>>()
        .map_err(error)?;
    statistics.push(QueryStatementStats {
        stage,
        plan,
        parameters: parameters.len(),
        full_scan_steps: statement.get_status(StatementStatus::FullscanStep),
        vm_steps: statement.get_status(StatementStatus::VmStep),
        sorts: statement.get_status(StatementStatus::Sort),
        automatic_index_rows: statement.get_status(StatementStatus::AutoIndex),
        sqlite_bytes,
        sqlite_peak_bytes,
    });
    Ok(())
}

/// Joins `parts` as a balanced binary tree, so the parser's depth grows with
/// the logarithm of the count instead of the count.
fn balanced(mut parts: Vec<String>, operator: &str) -> String {
    if parts.is_empty() {
        return "0".into();
    }
    while parts.len() > 1 {
        let mut next = Vec::with_capacity(parts.len().div_ceil(2));
        let mut iter = parts.into_iter();
        while let Some(left) = iter.next() {
            next.push(match iter.next() {
                Some(right) => format!("({left}{operator}{right})"),
                None => left,
            });
        }
        parts = next;
    }
    format!("({})", parts.remove(0))
}

/// The set key a member binds, or `None` when it is not a scalar and so can
/// never match.
fn match_key(value: &Value) -> Option<Vec<u8>> {
    let value = QueryValue::Value {
        value: value.clone(),
    };
    matches!(value.scalar_kind(), Some(1..=4))
        .then(|| value.scalar_key().ok())
        .flatten()
}

/// The widest physical `IN` list bound one parameter per member. A longer list
/// contributes no physical clause and the filter still decides the row.
const PHYSICAL_MEMBERS: usize = 1000;

fn property(text: Option<String>) -> rusqlite::Result<QueryValue> {
    text.map_or(Ok(QueryValue::Missing), |text| {
        serde_json::from_str(&text)
            .map(|value| QueryValue::Value { value })
            .map_err(|error| rusqlite::Error::UserFunctionError(Box::new(error)))
    })
}

pub(super) fn register(connection: &Connection) -> Result<()> {
    let flags = FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC;
    connection
        .create_scalar_function("query_characters", 1, flags, |ctx| {
            Ok(match property(ctx.get(0)?)? {
                QueryValue::Value {
                    value: Value::String(text),
                } => text.chars().count() as i64,
                _ => 0,
            })
        })
        .map_err(error)?;
    for (name, prefix) in [("query_prefix", true), ("query_contains", false)] {
        connection
            .create_scalar_function(name, 2, flags, move |ctx| {
                let QueryValue::Value {
                    value: Value::String(value),
                } = property(ctx.get(0)?)?
                else {
                    return Ok(false);
                };
                let text: String = ctx.get(1)?;
                Ok(if prefix {
                    value.starts_with(&text)
                } else {
                    value.contains(&text)
                })
            })
            .map_err(error)?;
    }
    connection
        .create_scalar_function("query_scalar_key", 1, flags, |ctx| {
            property(ctx.get(0)?)?
                .scalar_key()
                .map_err(|error| rusqlite::Error::UserFunctionError(Box::new(error)))
        })
        .map_err(error)?;
    connection
        .create_scalar_function("query_match_key", 1, flags, |ctx| {
            let value = property(ctx.get(0)?)?;
            matches!(value.scalar_kind(), Some(1..=4))
                .then(|| value.scalar_key())
                .transpose()
                .map_err(|error| rusqlite::Error::UserFunctionError(Box::new(error)))
        })
        .map_err(error)?;
    connection
        .create_scalar_function("query_compare", 3, flags, |ctx| {
            let comparison = match ctx.get::<i64>(2)? {
                0 => QueryComparison::Eq,
                1 => QueryComparison::Ne,
                2 => QueryComparison::Lt,
                3 => QueryComparison::Le,
                4 => QueryComparison::Gt,
                5 => QueryComparison::Ge,
                _ => {
                    return Err(rusqlite::Error::UserFunctionError(
                        "invalid comparison".into(),
                    ));
                }
            };
            Ok(comparison.matches(&property(ctx.get(0)?)?, &property(ctx.get(1)?)?))
        })
        .map_err(error)
}

#[derive(Default)]
struct Requirement {
    value: bool,
    join: bool,
}

struct Compiler<'a> {
    fields: Vec<(&'a str, Requirement)>,
    parameters: Vec<SqlValue>,
    alias: usize,
    position_after: Option<(u64, u64, bool)>,
    relation_scope: bool,
    candidate_limit: Option<(u64, bool)>,
    inline_text: Vec<(&'a QueryFilter, String)>,
    text_candidates: Vec<(&'a QueryFilter, Option<String>)>,
    prefixes: Vec<&'a QueryFilter>,
    direct_relations: bool,
    physical_candidates: Option<String>,
    physical_ids: Option<Vec<String>>,
    physical_kinds: Option<&'a [Value]>,
    candidate_index: &'static str,
    candidate_field_order: Option<bool>,
    resolved_relations: Vec<(&'a QueryFilter, String)>,
}

fn kind_values(filter: &QueryFilter) -> Option<&[Value]> {
    match filter {
        QueryFilter::In {
            value: QueryOperand::Field { path },
            values,
        } if path == "kind" => Some(values),
        QueryFilter::Compare {
            left: QueryOperand::Field { path },
            comparison: QueryComparison::Eq,
            right: QueryOperand::Value { value },
        } if path == "kind" => Some(std::slice::from_ref(value)),
        QueryFilter::And { conditions } => conditions.iter().find_map(kind_values),
        _ => None,
    }
}

fn prefix_filters<'a>(filter: &'a QueryFilter, prefixes: &mut Vec<&'a QueryFilter>) {
    match filter {
        QueryFilter::Prefix {
            value: QueryOperand::Field { path },
            ..
        } if !references(path) => {
            if !prefixes.contains(&filter) {
                prefixes.push(filter);
            }
        }
        QueryFilter::And { conditions } | QueryFilter::Or { conditions } => {
            for condition in conditions {
                prefix_filters(condition, prefixes);
            }
        }
        QueryFilter::Not { condition }
        | QueryFilter::Exists { condition }
        | QueryFilter::Any { condition, .. } => prefix_filters(condition, prefixes),
        _ => {}
    }
}

fn equality_values(filter: &QueryFilter, field: &str) -> Option<Vec<Value>> {
    match filter {
        QueryFilter::Compare {
            left: QueryOperand::Field { path },
            comparison: QueryComparison::Eq,
            right: QueryOperand::Value { value },
        }
        | QueryFilter::Compare {
            right: QueryOperand::Field { path },
            comparison: QueryComparison::Eq,
            left: QueryOperand::Value { value },
        } if path == field => Some(vec![value.clone()]),
        QueryFilter::In {
            value: QueryOperand::Field { path },
            values,
        } if path == field => Some(values.clone()),
        QueryFilter::And { conditions } => conditions
            .iter()
            .find_map(|condition| equality_values(condition, field)),
        QueryFilter::Or { conditions } => conditions
            .iter()
            .map(|condition| equality_values(condition, field))
            .collect::<Option<Vec<_>>>()
            .map(|values| values.into_iter().flatten().collect()),
        _ => None,
    }
}

impl<'a> Compiler<'a> {
    fn new(query: &'a QuerySpec) -> Self {
        let mut fields = BTreeMap::new();
        if let Some(filter) = &query.filter {
            required_filter(filter, &mut fields);
        }
        if let QuerySelect::Characters { field } = &query.select {
            fields
                .entry(field.as_str())
                .or_insert_with(Requirement::default)
                .value = true;
        }
        for field in &query.group_by {
            fields
                .entry(field.as_str())
                .or_insert_with(Requirement::default)
                .value = true;
        }
        for order in &query.order_by {
            if let QueryOrderKey::Field { path } = &order.key {
                fields
                    .entry(path)
                    .or_insert_with(Requirement::default)
                    .value = true;
            }
        }
        let mut prefixes = Vec::new();
        if let Some(filter) = &query.filter {
            prefix_filters(filter, &mut prefixes);
        }
        Self {
            fields: fields.into_iter().collect(),
            parameters: Vec::new(),
            alias: 0,
            position_after: None,
            relation_scope: false,
            candidate_limit: None,
            inline_text: Vec::new(),
            text_candidates: Vec::new(),
            prefixes,
            direct_relations: false,
            physical_candidates: None,
            physical_ids: None,
            physical_kinds: query.filter.as_ref().and_then(kind_values),
            candidate_field_order: None,
            candidate_index: [
                ("data.request_id", "timeline_events_requests"),
                ("data.input_id", "timeline_events_inputs"),
                ("data.turn_id", "timeline_events_turns"),
            ]
            .into_iter()
            .find_map(|(field, index)| {
                query
                    .filter
                    .as_ref()
                    .and_then(|filter| equality_values(filter, field))
                    .filter(|values| !values.is_empty() && values.iter().all(Value::is_string))
                    .map(|_| index)
            })
            .unwrap_or("timeline_events_by_kind"),
            resolved_relations: Vec::new(),
        }
    }

    fn bind(&mut self, value: impl Into<SqlValue>) -> String {
        self.parameters.push(value.into());
        format!("?{}", self.parameters.len())
    }

    fn column(&self, alias: &str, field: &str) -> String {
        let index = self
            .fields
            .iter()
            .position(|(path, _)| *path == field)
            .expect("query fields are compiled once");
        format!("{alias}.p{index}")
    }

    fn text_candidate(
        &self,
        filter: &QueryFilter,
        row: &str,
        physical: bool,
        indexed_only: bool,
    ) -> String {
        let QueryFilter::TextCandidate { field, .. } = filter else {
            unreachable!()
        };
        if let Some((_, predicate)) = self
            .inline_text
            .iter()
            .find(|(candidate, _)| *candidate == filter)
        {
            let predicate = predicate
                .replace(
                    "$branch",
                    &format!("{row}.{}", if physical { "branch_id" } else { "branch" }),
                )
                .replace(
                    "$sequence",
                    &format!("{row}.{}", if physical { "sequence" } else { "seq" }),
                );
            let kind = if physical {
                format!("{row}.kind")
            } else {
                format!("json_extract({},'$')", self.column(row, "kind"))
            };
            let unsupported = if field == "data.delta" {
                format!("{kind}<>'step/text'")
            } else {
                format!(
                    "(NOT ({}) OR {kind} IN ('step/text','step/thinking'))",
                    super::text::data_index_sql(&kind)
                )
            };
            return format!("({unsupported} OR ({predicate}))");
        }
        let Some((_, Some(table))) = self
            .text_candidates
            .iter()
            .find(|(candidate, _)| *candidate == filter)
        else {
            return "1".into();
        };
        let sequence = if physical {
            format!("{row}.sequence")
        } else {
            format!("{row}.seq")
        };
        let ordinal = if physical {
            "s.ord".into()
        } else {
            format!("{row}.ord")
        };
        let position = if physical { "physical" } else { "seq" };
        let kind = if physical {
            format!("{row}.kind")
        } else {
            format!("json_extract({},'$')", self.column(row, "kind"))
        };
        let unsupported = if field == "data.delta" {
            format!("{kind}<>'step/text'")
        } else {
            format!("NOT ({})", super::text::data_index_sql(&kind))
        };
        let selected = format!(
            "EXISTS(SELECT 1 FROM {table} hit WHERE hit.ord={ordinal} AND hit.{position}={sequence})"
        );
        if indexed_only {
            selected
        } else {
            format!("({unsupported} OR {selected})")
        }
    }

    fn operand(
        &mut self,
        value: &QueryOperand,
        row: &str,
        outer: Option<&str>,
        element: Option<&str>,
    ) -> String {
        match value {
            QueryOperand::Field { path } => self.column(row, path),
            QueryOperand::Outer { path } => {
                self.column(outer.expect("validated outer reference"), path)
            }
            QueryOperand::Element { path } => {
                let path = self.bind(json_path(path));
                format!("({} -> {path})", element.expect("validated array element"))
            }
            QueryOperand::Value { value } => self.bind(value.to_string()),
        }
    }

    fn filter(
        &mut self,
        filter: &QueryFilter,
        row: &str,
        outer: Option<&str>,
        element: Option<&str>,
    ) -> String {
        match filter {
            QueryFilter::Compare {
                left,
                comparison,
                right,
            } => {
                if *comparison == QueryComparison::Eq
                    && let (
                        QueryOperand::Field { path: inner },
                        QueryOperand::Outer { path: other },
                    )
                    | (
                        QueryOperand::Outer { path: other },
                        QueryOperand::Field { path: inner },
                    ) = (left, right)
                    && !inner.starts_with("element.")
                    && !other.starts_with("element.")
                {
                    let inner = self.column(row, inner).replace(".p", ".k");
                    let other = self
                        .column(outer.expect("validated outer reference"), other)
                        .replace(".p", ".k");
                    return format!("{inner}={other}");
                }
                let left = self.operand(left, row, outer, element);
                let right = self.operand(right, row, outer, element);
                let op = match comparison {
                    QueryComparison::Eq => 0,
                    QueryComparison::Ne => 1,
                    QueryComparison::Lt => 2,
                    QueryComparison::Le => 3,
                    QueryComparison::Gt => 4,
                    QueryComparison::Ge => 5,
                };
                format!("query_compare({left},{right},{op})")
            }
            QueryFilter::In { value, values } => {
                let value = self.operand(value, row, outer, element);
                let keys = values
                    .iter()
                    .filter_map(match_key)
                    .map(|key| self.bind(SqlValue::Blob(key)))
                    .collect::<Vec<_>>();
                if keys.is_empty() {
                    return "0".into();
                }
                let compare = format!("query_match_key({value}) IN ({})", keys.join(","));
                format!("ifnull({compare},0)")
            }
            QueryFilter::Present { value } => {
                format!("({} IS NOT NULL)", self.operand(value, row, outer, element))
            }
            QueryFilter::Prefix { .. } if self.prefixes.contains(&filter) => format!(
                "{row}.t{}",
                self.prefixes
                    .iter()
                    .position(|candidate| *candidate == filter)
                    .unwrap()
            ),
            QueryFilter::Prefix { value, prefix } => {
                let value = self.operand(value, row, outer, element);
                let text = self.bind(prefix.clone());
                format!("query_prefix({value},{text})")
            }
            QueryFilter::TextCandidate { .. } => self.text_candidate(filter, row, false, false),
            QueryFilter::Contains { value, text } => {
                let value = self.operand(value, row, outer, element);
                let text = self.bind(text.clone());
                format!("query_contains({value},{text})")
            }
            QueryFilter::And { conditions } | QueryFilter::Or { conditions } => {
                let and = matches!(filter, QueryFilter::And { .. });
                if conditions.is_empty() {
                    return if and { "1".into() } else { "0".into() };
                }
                let conditions: Vec<_> = conditions
                    .iter()
                    .map(|f| self.filter(f, row, outer, element))
                    .collect();
                balanced(conditions, if and { " AND " } else { " OR " })
            }
            QueryFilter::Not { condition } => {
                format!("NOT ({})", self.filter(condition, row, outer, element))
            }
            QueryFilter::Exists { condition } => {
                if let Some((_, expression)) = self
                    .resolved_relations
                    .iter()
                    .find(|(candidate, _)| *candidate == condition.as_ref())
                {
                    let expression = expression.clone();
                    let mut expression = expression;
                    for (sql, path) in [
                        ("e.event_id", "id"),
                        (
                            "coalesce(json_extract(e.body,'$.record.event.data.request_id'),json_extract(e.body,'$.request_id'))",
                            "data.request_id",
                        ),
                        (
                            "json_extract(e.body,'$.record.event.data.task_id')",
                            "data.task_id",
                        ),
                        (
                            "json_extract(e.body,'$.record.event.data.input_id')",
                            "data.input_id",
                        ),
                    ] {
                        if expression.contains(sql) {
                            expression = expression.replace(
                                sql,
                                &format!("json_extract({},'$')", self.column(row, path)),
                            );
                        }
                    }
                    return expression;
                }
                self.alias += 1;
                let inner = format!("related{}", self.alias);
                let filter = self.filter(condition, &inner, Some(row), None);
                format!("EXISTS (SELECT 1 FROM events {inner} WHERE {filter})")
            }
            QueryFilter::Any { field, condition } => {
                self.alias += 1;
                let alias = format!("element{}", self.alias);
                let array = self.column(row, field);
                let element = format!("({array} -> ('$[' || {alias}.key || ']'))");
                let condition = self.filter(condition, row, outer, Some(&element));
                format!(
                    "CASE WHEN json_type({array})='array' THEN EXISTS (SELECT 1 FROM json_each({array}) {alias} WHERE {condition}) ELSE 0 END"
                )
            }
        }
    }

    fn scope(&mut self, scope: &TimelineReadScope, seek: bool) -> String {
        let rows: Vec<_> = scope
            .segments
            .iter()
            .enumerate()
            .map(|(index, segment)| {
                let ordinal = self.bind(index as i64);
                let branch = self.bind(segment.branch_id.clone());
                let mut from = segment.from;
                let mut through = segment.through.min(segment.stored_through);
                if let Some((at, sequence, descending)) = self.position_after.filter(|_| seek) {
                    match (index as u64).cmp(&at) {
                        std::cmp::Ordering::Equal if descending => {
                            through = through.min(sequence.saturating_sub(1))
                        }
                        std::cmp::Ordering::Equal => from = from.max(sequence.saturating_add(1)),
                        std::cmp::Ordering::Less if !descending => through = 0,
                        std::cmp::Ordering::Greater if descending => through = 0,
                        _ => {}
                    }
                }
                let from = self.bind(from.min(i64::MAX as u64) as i64);
                let through = self.bind(through.min(i64::MAX as u64) as i64);
                format!("({ordinal},{branch},{from},{through})")
            })
            .collect();
        if rows.is_empty() {
            "SELECT 0,NULL,1,0 WHERE 0".into()
        } else {
            format!("VALUES {}", rows.join(","))
        }
    }

    fn physical_relation(
        &mut self,
        condition: &QueryFilter,
        known: Option<&[Value]>,
    ) -> Option<String> {
        if !self.direct_relations {
            return None;
        }
        let QueryFilter::And { conditions } = condition else {
            return None;
        };
        if let Some((at, branches)) = conditions.iter().enumerate().find_map(|(at, filter)| {
            if let QueryFilter::Or { conditions } = filter {
                Some((at, conditions))
            } else {
                None
            }
        }) {
            let mut alternatives = Vec::new();
            for branch in branches {
                let mut branch_conditions = conditions.clone();
                branch_conditions.remove(at);
                match branch {
                    QueryFilter::And { conditions } => branch_conditions.extend(conditions.clone()),
                    condition => branch_conditions.push(condition.clone()),
                }
                alternatives.push(self.physical_relation(
                    &QueryFilter::And {
                        conditions: branch_conditions,
                    },
                    known,
                )?);
            }
            return Some(if alternatives.is_empty() {
                "0".into()
            } else {
                format!("({})", alternatives.join(" OR "))
            });
        }
        let kinds = kind_values(condition)?;
        let join = conditions.iter().find_map(|condition| match condition {
            QueryFilter::Compare {
                left: QueryOperand::Field { path: inner },
                comparison: QueryComparison::Eq,
                right: QueryOperand::Outer { path: outer },
            }
            | QueryFilter::Compare {
                left: QueryOperand::Outer { path: outer },
                comparison: QueryComparison::Eq,
                right: QueryOperand::Field { path: inner },
            } => Some((condition, inner.as_str(), outer.as_str())),
            _ => None,
        })?;
        let non_output = |kinds: &[Value]| {
            kinds.iter().all(|kind| {
                kind.as_str()
                    .is_some_and(|kind| !matches!(kind, "step/text" | "step/thinking"))
            })
        };
        let (inner, outer, index) = match (join.1, join.2) {
            ("id", "data.input_id") if non_output(kinds) => (
                "r.event_id",
                "json_extract(e.body,'$.record.event.data.input_id')",
                " INDEXED BY timeline_events_by_id",
            ),
            ("data.input_id", "id") if known.is_some_and(non_output) => (
                "json_extract(r.body,'$.record.event.data.input_id')",
                "e.event_id",
                "",
            ),
            ("data.source.request_id", "data.request_id") if non_output(kinds) => (
                "json_extract(r.body,'$.record.event.data.source.request_id')",
                "coalesce(json_extract(e.body,'$.record.event.data.request_id'),json_extract(e.body,'$.request_id'))",
                " INDEXED BY timeline_events_by_kind",
            ),
            ("data.request_id", "data.request_id") => (
                "coalesce(json_extract(r.body,'$.record.event.data.request_id'),json_extract(r.body,'$.request_id'))",
                "coalesce(json_extract(e.body,'$.record.event.data.request_id'),json_extract(e.body,'$.request_id'))",
                " INDEXED BY timeline_events_requests",
            ),
            _ => return None,
        };
        let mut extra = Vec::new();
        for condition in conditions {
            if condition == join.0 {
                continue;
            }
            if let QueryFilter::Compare {
                left: QueryOperand::Field { path },
                comparison,
                right: QueryOperand::Value { value },
            } = condition
                && path == "position.sequence"
                && non_output(kinds)
            {
                let op = match comparison {
                    QueryComparison::Eq => "=",
                    QueryComparison::Lt => "<",
                    QueryComparison::Le => "<=",
                    QueryComparison::Gt => ">",
                    QueryComparison::Ge => ">=",
                    QueryComparison::Ne => return None,
                };
                let value = self.bind(i64::try_from(value.as_u64()?).ok()?);
                extra.push(format!("r.sequence{op}{value}"));
            } else {
                extra.push(
                    self.physical_filter_for(condition, false, Some(kinds))?
                        .replace("e.", "r.")
                        .replace("s.ord", "rs.ord"),
                );
            }
        }
        Some(format!(
            "EXISTS(SELECT 1 FROM scope rs CROSS JOIN timeline_events r{index} WHERE r.session_id=e.session_id AND r.branch_id=rs.branch AND rs.lo<=rs.hi AND r.sequence<=rs.hi AND r.last_sequence>=rs.lo AND {inner}=+({outer}) AND ({}))",
            extra.join(") AND (")
        ))
    }

    fn physical_filter(&mut self, filter: &QueryFilter, partial: bool) -> Option<String> {
        self.physical_filter_for(filter, partial, None)
    }

    /// Compiles a physical form only to learn whether one exists. Its result is
    /// dropped, so the parameters it bound are dropped with it: a discarded
    /// check leaves the binder exactly as it found it, both the vector and the
    /// numbering of the parameters that follow.
    fn physical_form_exists(&mut self, filter: &QueryFilter, partial: bool) -> bool {
        let bound = self.parameters.len();
        let form = self.physical_filter(filter, partial);
        self.parameters.truncate(bound);
        form.is_some()
    }

    fn physical_filter_for(
        &mut self,
        filter: &QueryFilter,
        partial: bool,
        known: Option<&[Value]>,
    ) -> Option<String> {
        let known = kind_values(filter).or(known);
        let column = |field: &str| match field {
            "kind" => Some(("e.kind", false)),
            "position.branch_id" => Some(("e.branch_id", false)),
            "position.sequence"
                if known.is_some_and(|kinds| {
                    kinds.iter().all(|kind| {
                        kind.as_str()
                            .is_some_and(|kind| !matches!(kind, "step/text" | "step/thinking"))
                    })
                }) =>
            {
                Some(("e.sequence", false))
            }
            "context.render" => Some(("e.context_render", true)),
            "context.state" => Some(("e.context_state", true)),
            "data.request_id" => Some((
                "coalesce(json_extract(e.body,'$.record.event.data.request_id'),json_extract(e.body,'$.request_id'))",
                false,
            )),
            "data.input_id" => Some(("json_extract(e.body,'$.record.event.data.input_id')", false)),
            "data.turn_id" => Some((
                "coalesce(json_extract(e.body,'$.record.event.data.turn_id'),json_extract(e.body,'$.turn_id'))",
                false,
            )),
            "data.scope.kind" => Some((
                "json_extract(e.body,'$.record.event.data.scope.kind')",
                false,
            )),
            "data.scope.timeline" => Some((
                "json_extract(e.body,'$.record.event.data.scope.timeline')",
                false,
            )),
            "data.scope.id" => Some(("json_extract(e.body,'$.record.event.data.scope.id')", false)),
            "data.status" => Some(("json_extract(e.body,'$.record.event.data.status')", false)),
            "data.epoch" => Some(("json_extract(e.body,'$.record.event.data.epoch')", false)),
            "data.current.at.branch" => Some((
                "json_extract(e.body,'$.record.event.data.current.at.branch')",
                false,
            )),
            "data.current.at.sequence" => Some((
                "json_extract(e.body,'$.record.event.data.current.at.sequence')",
                false,
            )),
            _ => None,
        };
        let type_guard = |path: &str, numeric: bool| {
            let record = format!("json_type(e.body,'$.record.event.{path}')");
            let kind = match path {
                "data.request_id" => format!("coalesce({record},json_type(e.body,'$.request_id'))"),
                "data.turn_id" => format!("coalesce({record},json_type(e.body,'$.turn_id'))"),
                _ => record,
            };
            if numeric {
                format!("({kind} IS 'integer' OR {kind} IS 'real')")
            } else {
                format!("{kind} IS 'text'")
            }
        };
        match filter {
            QueryFilter::Exists { condition } => self
                .resolved_relations
                .iter()
                .find(|(candidate, _)| *candidate == condition.as_ref())
                .map(|(_, expression)| expression.clone())
                .or_else(|| self.physical_relation(condition, known)),
            QueryFilter::TextCandidate { field, .. } => self
                .text_candidates
                .iter()
                .any(|(candidate, _)| *candidate == filter)
                .then_some(())
                .or_else(|| {
                    self.inline_text
                        .iter()
                        .any(|(candidate, _)| *candidate == filter)
                        .then_some(())
                })
                .map(|()| {
                    self.text_candidate(
                        filter,
                        "e",
                        true,
                        known.is_some_and(|kinds| {
                            kinds.iter().all(|kind| {
                                kind.as_str()
                                    .is_some_and(|kind| super::text::indexed(kind, field))
                            })
                        }),
                    )
                }),
            QueryFilter::Prefix {
                value: QueryOperand::Field { path },
                prefix,
            } if path.starts_with("data.")
                && !path.contains("[]")
                && !references(path)
                && known.is_some_and(|kinds| {
                    kinds.iter().all(|kind| {
                        kind.as_str()
                            .is_some_and(|kind| !matches!(kind, "step/text" | "step/thinking"))
                    })
                }) =>
            {
                let path = self.bind(format!("$.record.event{}", &json_path(path)[1..]));
                let prefix = self.bind(prefix.clone());
                Some(format!("query_prefix(e.body -> {path},{prefix})"))
            }
            QueryFilter::Compare {
                left,
                comparison,
                right,
            } => {
                let (path, value, reversed) = match (left, right) {
                    (QueryOperand::Field { path }, QueryOperand::Value { value }) => {
                        (path, value, false)
                    }
                    (QueryOperand::Value { value }, QueryOperand::Field { path }) => {
                        (path, value, true)
                    }
                    _ => return None,
                };
                if path == "id" && partial && *comparison == QueryComparison::Eq {
                    let Value::String(value) = value else {
                        return Some("0".into());
                    };
                    let value = self.bind(value.clone());
                    return Some(format!(
                        "(e.event_id={value} OR e.kind IN ('step/text','step/thinking'))"
                    ));
                }
                if *comparison == QueryComparison::Eq {
                    if path == "kind" && value.as_str() == Some("input/message") {
                        return Some("e.kind='input/message'".into());
                    }
                    if path == "data.submission_id"
                        && let Value::String(value) = value
                    {
                        let value = self.bind(value.clone());
                        return Some(format!(
                            "(json_type(e.body,'$.record.event.data.submission_id') IS 'text' AND json_extract(e.body,'$.record.event.data.submission_id')={value})"
                        ));
                    }
                }
                if matches!(path.as_str(), "data.request_id" | "data.input_id")
                    && (*comparison != QueryComparison::Eq || !value.is_string())
                {
                    return None;
                }
                let (column, boolean) = column(path)?;
                if matches!(
                    path.as_str(),
                    "position.sequence" | "data.current.at.sequence" | "data.current.at.branch"
                ) && !value.is_number()
                {
                    return None;
                }
                let value = match (boolean, value) {
                    (true, Value::Bool(value)) => SqlValue::Integer(i64::from(*value)),
                    (false, Value::String(value)) => SqlValue::Text(value.clone()),
                    (false, Value::Number(value))
                        if matches!(
                            path.as_str(),
                            "position.sequence"
                                | "data.current.at.sequence"
                                | "data.current.at.branch"
                        ) =>
                    {
                        SqlValue::Integer(value.as_i64()?)
                    }
                    _ => return None,
                };
                let op = match (comparison, reversed) {
                    (QueryComparison::Eq, _) => "=",
                    (QueryComparison::Ne, _) => "!=",
                    (QueryComparison::Lt, false) | (QueryComparison::Gt, true) => "<",
                    (QueryComparison::Le, false) | (QueryComparison::Ge, true) => "<=",
                    (QueryComparison::Gt, false) | (QueryComparison::Lt, true) => ">",
                    (QueryComparison::Ge, false) | (QueryComparison::Le, true) => ">=",
                };
                let guard = path
                    .starts_with("data.")
                    .then(|| type_guard(path, matches!(value, SqlValue::Integer(_))));
                let comparison = format!("{column}{op}{}", self.bind(value));
                Some(match guard {
                    Some(guard) => format!("({guard} AND {comparison})"),
                    None => comparison,
                })
            }
            QueryFilter::Present {
                value: QueryOperand::Field { path },
            } if path == "data.user" => {
                Some("json_type(e.body,'$.record.event.data.user') IS NOT NULL".into())
            }
            QueryFilter::Present {
                value: QueryOperand::Field { path },
            } if matches!(
                path.as_str(),
                "kind" | "position.branch_id" | "context.render" | "context.state"
            ) =>
            {
                Some("1".into())
            }
            QueryFilter::In {
                value: QueryOperand::Field { path },
                values,
            } => {
                if path == "kind" && values.len() == 1 && values[0] == "input/message" {
                    return Some("e.kind='input/message'".into());
                }
                if path == "id" && partial {
                    if values.len() > PHYSICAL_MEMBERS {
                        return None;
                    }
                    let ids = values
                        .iter()
                        .filter_map(|value| value.as_str().map(|value| self.bind(value.to_owned())))
                        .collect::<Vec<_>>();
                    return Some(if ids.is_empty() {
                        "0".into()
                    } else {
                        format!(
                            "(e.event_id IN ({}) OR e.kind IN ('step/text','step/thinking'))",
                            ids.join(",")
                        )
                    });
                }
                if matches!(path.as_str(), "data.request_id" | "data.input_id")
                    && values.iter().any(|value| !value.is_string())
                {
                    return None;
                }
                if matches!(
                    path.as_str(),
                    "data.turn_id"
                        | "data.scope.kind"
                        | "data.scope.timeline"
                        | "data.scope.id"
                        | "data.status"
                        | "data.epoch"
                ) && values.iter().any(|value| !value.is_string())
                {
                    return None;
                }
                if matches!(
                    path.as_str(),
                    "position.sequence" | "data.current.at.sequence" | "data.current.at.branch"
                ) && values.iter().any(|value| value.as_i64().is_none())
                {
                    return None;
                }
                let (column, boolean) = column(path)?;
                if values.len() > PHYSICAL_MEMBERS {
                    return None;
                }
                let values = values
                    .iter()
                    .filter_map(|value| match (boolean, value) {
                        (true, Value::Bool(value)) => Some(self.bind(i64::from(*value))),
                        (false, Value::String(value)) => Some(self.bind(value.clone())),
                        (false, Value::Number(value))
                            if matches!(
                                path.as_str(),
                                "position.sequence"
                                    | "data.current.at.sequence"
                                    | "data.current.at.branch"
                            ) =>
                        {
                            Some(self.bind(value.as_i64()?))
                        }
                        _ => None,
                    })
                    .collect::<Vec<_>>();
                Some(if values.is_empty() {
                    "0".into()
                } else {
                    let comparison = format!("{column} IN ({})", values.join(","));
                    if path.starts_with("data.") {
                        let numeric = matches!(
                            path.as_str(),
                            "data.current.at.sequence" | "data.current.at.branch"
                        );
                        format!("({} AND {comparison})", type_guard(path, numeric))
                    } else {
                        comparison
                    }
                })
            }
            QueryFilter::And { conditions } => {
                let mut clauses = Vec::new();
                for condition in conditions {
                    match self.physical_filter_for(condition, partial, known) {
                        Some(clause) => clauses.push(clause),
                        None if partial => (),
                        None => return None,
                    }
                }
                Some(if clauses.is_empty() {
                    "1".into()
                } else {
                    balanced(clauses, " AND ")
                })
            }
            QueryFilter::Or { conditions } => {
                let clauses = conditions
                    .iter()
                    .map(|condition| self.physical_filter_for(condition, partial, known))
                    .collect::<Option<Vec<_>>>()?;
                Some(if clauses.is_empty() {
                    "0".into()
                } else {
                    balanced(clauses, " OR ")
                })
            }
            QueryFilter::Not { condition } => self
                .physical_filter_for(condition, false, known)
                .map(|clause| format!("NOT ({clause})")),
            _ => None,
        }
    }

    fn physical(&mut self, scope: &TimelineReadScope, predicate: &str) -> String {
        let segments = self.scope(scope, !self.relation_scope);
        let (window, source) = if self.relation_scope && self.candidate_limit.is_some() {
            let rows = self.scope(scope, true);
            (format!(", window(ord,branch,lo,hi) AS ({rows})"), "window")
        } else {
            (String::new(), "scope")
        };
        let session = self.bind(scope.key.session_id.clone());
        let lower = if self.physical_ids.is_some() {
            "1".to_owned()
        } else {
            format!(
                "coalesce((SELECT max(p.sequence) FROM timeline_events p WHERE p.session_id={session} AND p.branch_id=s.branch AND p.sequence<=s.lo),s.lo)"
            )
        };
        let bound = if self.physical_candidates.is_none()
            && self.physical_ids.is_none()
            && let Some((limit, descending)) = self.candidate_limit
        {
            let direction = if descending { "DESC" } else { "ASC" };
            let predicate = predicate
                .split('\'')
                .enumerate()
                .map(|(index, fragment)| {
                    if index % 2 == 0 {
                        fragment.replace("e.", "candidate.")
                    } else {
                        fragment.to_owned()
                    }
                })
                .collect::<Vec<_>>()
                .join("'");
            let ordering = match self.candidate_field_order {
                Some(descending) => {
                    let direction = if descending { "DESC" } else { "ASC" };
                    format!(
                        "json_extract(candidate.body,'$.record.event.data.current.at.sequence') {direction},candidate.sequence {direction}"
                    )
                }
                None => format!("candidate.sequence {direction}"),
            };
            let limit = self.bind(limit.min(i64::MAX as u64) as i64);
            let kinds = self.physical_kinds.map(|kinds| {
                kinds
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<std::collections::BTreeSet<_>>()
            });
            if let Some(kinds) = kinds.filter(|kinds| (2..=32).contains(&kinds.len())) {
                let index = self.candidate_index;
                let candidates = kinds
                    .into_iter()
                    .map(|kind| {
                        let kind = self.bind(kind.to_owned());
                        format!(
                            "SELECT sequence FROM (SELECT candidate.sequence
                      FROM timeline_events candidate INDEXED BY {index}
                      WHERE candidate.session_id={session} AND candidate.branch_id=s.branch
                      AND candidate.kind={kind} AND candidate.sequence BETWEEN {lower} AND s.hi
                      AND candidate.last_sequence>=s.lo AND ({predicate})
                      ORDER BY {ordering} LIMIT {limit})"
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(" UNION ALL ");
                format!(
                    "AND e.sequence IN (SELECT sequence FROM ({candidates}) ORDER BY sequence {direction} LIMIT {limit})"
                )
            } else {
                format!(
                    "AND e.sequence IN (SELECT candidate.sequence FROM timeline_events candidate
              WHERE candidate.session_id={session} AND candidate.branch_id=s.branch
              AND candidate.sequence BETWEEN {lower} AND s.hi AND candidate.last_sequence>=s.lo
              AND ({predicate}) ORDER BY {ordering} LIMIT {limit})"
                )
            }
        } else {
            String::new()
        };
        let identities = self
            .physical_ids
            .as_ref()
            .map(|ids| serde_json::to_string(ids).expect("event identities are strings"));
        let identities = identities.map(|ids| self.bind(ids));
        let candidates = self
            .physical_candidates
            .as_ref()
            .map_or(String::new(), |table| {
                format!(
                    " CROSS JOIN (SELECT DISTINCT ord,physical FROM {table}) hit ON hit.ord=s.ord"
                )
            });
        let location = if let Some(identities) = identities {
            format!(" AND e.sequence IN (
              SELECT sequence FROM timeline_events origin INDEXED BY timeline_events_by_id
              WHERE origin.session_id={session} AND origin.branch_id=s.branch AND origin.sequence BETWEEN s.lo AND s.hi
              AND origin.event_id IN (SELECT value FROM json_each({identities}))
              UNION SELECT physical_sequence FROM timeline_output_ids locator INDEXED BY sqlite_autoindex_timeline_output_ids_1
              WHERE locator.session_id={session} AND locator.branch_id=s.branch
              AND locator.event_id IN (SELECT value FROM json_each({identities}))
              AND locator.logical_sequence BETWEEN s.lo AND s.hi)")
        } else if self.physical_candidates.is_some() {
            " AND e.sequence=hit.physical".to_owned()
        } else {
            String::new()
        };
        format!("WITH scope(ord,branch,lo,hi) AS ({segments}){window}, physical AS NOT MATERIALIZED (
          SELECT s.ord,s.branch,s.lo,s.hi,e.sequence,e.last_sequence,e.kind,e.event_id,json_type(e.body,'$.record.event.data.user') AS user_presence,e.body,e.context_render,e.context_state
          FROM {source} s{candidates} CROSS JOIN timeline_events e ON e.session_id={session} AND e.branch_id=s.branch{location}
          WHERE ({predicate}) AND s.lo<=s.hi AND e.sequence<=s.hi AND e.last_sequence>=s.lo
          AND e.sequence>={lower} {bound}
        )")
    }

    fn base(&mut self, scope: &TimelineReadScope, predicate: &str) -> String {
        let physical = self.physical(scope, predicate);
        let (outputs, value) = if let Some(ids) = &self.physical_ids {
            let ids = self.bind(serde_json::to_string(ids).expect("event identities are strings"));
            let session = self.bind(scope.key.session_id.clone());
            (format!("timeline_output_ids locator INDEXED BY sqlite_autoindex_timeline_output_ids_1 CROSS JOIN physical p ON p.branch=locator.branch_id AND p.sequence=locator.physical_sequence
              WHERE locator.session_id={session} AND locator.event_id IN (SELECT value FROM json_each({ids}))
              AND locator.logical_sequence BETWEEN p.lo AND p.hi AND"),
             "json_extract(p.body,'$.records['||(locator.logical_sequence-p.sequence)||']')")
        } else if scope
            .segments
            .iter()
            .all(|segment| segment.from == segment.through)
        {
            (
                "physical p WHERE".to_owned(),
                "json_extract(p.body,'$.records['||(p.lo-p.sequence)||']')",
            )
        } else {
            (
                "physical p,json_each(p.body,'$.records') r WHERE".to_owned(),
                "r.value",
            )
        };
        format!("{physical}, base AS NOT MATERIALIZED (
          SELECT ord,branch,sequence AS seq,kind,coalesce(event_id,json_extract(body,'$.record.event.id')) AS event_id,user_presence,context_render,context_state,
            body AS wire,0 AS output,NULL AS turn_id,NULL AS request_id FROM physical
            WHERE kind NOT IN ('step/text','step/thinking') OR json_extract(body,'$.encoding')='event'
          UNION ALL
          SELECT p.ord,p.branch,json_extract({value},'$.event.sequence'),p.kind,json_extract({value},'$.event.id'),NULL,p.context_render,p.context_state,
            {value},1,json_extract(p.body,'$.turn_id'),json_extract(p.body,'$.request_id')
          FROM {outputs} p.kind IN ('step/text','step/thinking') AND json_extract(p.body,'$.encoding')='output'
            AND json_extract({value},'$.event.sequence') BETWEEN p.lo AND p.hi
        )")
    }

    fn inline(&mut self, field: &str, row: &str, presence: bool) -> String {
        if field.starts_with("element.") {
            return self.column(row, field);
        }
        match field {
            "position.sequence" => format!("CAST({row}.seq AS TEXT)"),
            "position.branch_id" => format!("json_quote({row}.branch)"),
            "position" => format!("json_object('branch_id',{row}.branch,'sequence',{row}.seq)"),
            "kind" => format!("json_quote({row}.kind)"),
            "id" => format!("json_quote({row}.event_id)"),
            "context.render" => {
                format!("CASE WHEN {row}.context_render THEN 'true' ELSE 'false' END")
            }
            "context.state" => {
                format!("CASE WHEN {row}.context_state THEN 'true' ELSE 'false' END")
            }
            "data.user" if presence => format!("{row}.user_presence"),
            _ => {
                let path = self.bind(format!("$.record.event{}", &json_path(field)[1..]));
                let output = match field {
                    "data.turn_id" => format!("json_quote({row}.turn_id)"),
                    "data.request_id" => format!("json_quote({row}.request_id)"),
                    _ => {
                        let output_path = self.bind(format!("$.event{}", &json_path(field)[1..]));
                        if presence {
                            format!("json_type({row}.wire,{output_path})")
                        } else if field == "data" {
                            format!(
                                "json_set({row}.wire -> {output_path},'$.turn_id',{row}.turn_id,'$.request_id',{row}.request_id)"
                            )
                        } else {
                            format!("{row}.wire -> {output_path}")
                        }
                    }
                };
                let input = if presence {
                    format!("json_type({row}.wire,{path})")
                } else {
                    format!("{row}.wire -> {path}")
                };
                let value = format!("CASE WHEN {row}.output THEN {output} ELSE {input} END");
                if presence && field == "data.changes" {
                    return format!(
                        "CASE WHEN json_type({row}.wire,'$.record.changes')='object' THEN 'true' ELSE {value} END"
                    );
                }
                if presence && matches!(field, "data.request.tools" | "data.request.system") {
                    let reference = if field.ends_with("tools") {
                        "tools"
                    } else {
                        "system"
                    };
                    return format!("CASE WHEN json_type({row}.wire,'$.record.refs')='object' THEN
                      CASE WHEN json_extract({row}.wire,'$.record.refs.{reference}') IS NOT NULL THEN 'true' END
                      ELSE {value} END");
                }
                format!("({value})")
            }
        }
    }
}

fn json_path(path: &str) -> String {
    let mut result = "$".to_owned();
    for field in path.split('.').filter(|field| !field.is_empty()) {
        let (name, index) = field.split_at(field.find('[').unwrap_or(field.len()));
        result.push_str(".\"");
        result.push_str(name);
        result.push('"');
        result.push_str(index);
    }
    result
}

fn required_filter<'a>(filter: &'a QueryFilter, fields: &mut BTreeMap<&'a str, Requirement>) {
    let mut operand = |operand: &'a QueryOperand, value: bool| {
        if let QueryOperand::Field { path } | QueryOperand::Outer { path } = operand {
            fields.entry(path).or_default().value |= value;
        }
    };
    match filter {
        QueryFilter::TextCandidate { .. } => {
            fields.entry("kind").or_default().value = true;
        }
        QueryFilter::Compare {
            left,
            comparison,
            right,
        } => {
            operand(left, true);
            operand(right, true);
            if *comparison == QueryComparison::Eq
                && let (QueryOperand::Field { path: inner }, QueryOperand::Outer { path: outer })
                | (QueryOperand::Outer { path: outer }, QueryOperand::Field { path: inner }) =
                    (left, right)
            {
                fields.entry(inner).or_default().join = true;
                fields.entry(outer).or_default().join = true;
            }
        }
        QueryFilter::Present { value } => operand(value, false),
        QueryFilter::Prefix {
            value: QueryOperand::Field { path },
            ..
        } if !references(path) => {}
        QueryFilter::In { value, .. }
        | QueryFilter::Prefix { value, .. }
        | QueryFilter::Contains { value, .. } => operand(value, true),
        QueryFilter::And { conditions } | QueryFilter::Or { conditions } => {
            for condition in conditions {
                required_filter(condition, fields);
            }
        }
        QueryFilter::Not { condition } | QueryFilter::Exists { condition } => {
            required_filter(condition, fields)
        }
        QueryFilter::Any { field, condition } => {
            fields.entry(field).or_default().value = true;
            required_filter(condition, fields);
        }
    }
}

fn references(field: &str) -> bool {
    matches!(field, "data" | "data.request")
        || field
            .strip_prefix("data.changes")
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('.') || rest.starts_with('['))
        || ["messages", "tools", "system"].iter().any(|name| {
            field
                .strip_prefix(&format!("data.request.{name}"))
                .is_some_and(|rest| {
                    rest.is_empty() || rest.starts_with('.') || rest.starts_with('[')
                })
        })
}

fn inline_presence(field: &str) -> bool {
    matches!(
        field,
        "data"
            | "data.changes"
            | "data.request"
            | "data.request.messages"
            | "data.request.tools"
            | "data.request.system"
    )
}

fn value_text(value: QueryValue) -> Result<SqlValue> {
    match value {
        QueryValue::Missing => Ok(SqlValue::Null),
        QueryValue::Value { value } => Ok(SqlValue::Text(value.to_string())),
        QueryValue::Elements { .. } => {
            Err(error("array projections cannot enter scalar query columns"))
        }
    }
}

struct Temporary<'a> {
    connection: &'a mut Connection,
    names: Vec<String>,
}

impl Temporary<'_> {
    fn table(&mut self, columns: &str) -> Result<String> {
        let name = format!("query_{}", uuid::Uuid::new_v4().simple());
        self.connection
            .execute_batch(&format!("CREATE TEMP TABLE {name} ({columns})"))
            .map_err(error)?;
        self.names.push(name.clone());
        Ok(name)
    }
}

impl Drop for Temporary<'_> {
    fn drop(&mut self) {
        if !self.names.is_empty() {
            let _ = self.connection.progress_handler(0, None::<fn() -> bool>);
        }
        for name in &self.names {
            if let Err(error) = self
                .connection
                .execute_batch(&format!("DROP TABLE temp.{name}"))
            {
                tracing::error!(%error, "failed to release query temporary table");
            }
        }
    }
}

pub(super) fn cancelled(cancel: &CancellationToken) -> Result<()> {
    if cancel.is_cancelled() {
        Err(AgentError::Cancelled("timeline query".into()))
    } else {
        Ok(())
    }
}

fn project_wire(
    connection: &Connection,
    session: &str,
    reader: &mut Reader,
    wire: &WireEvent,
    field: &str,
    cancel: &CancellationToken,
) -> Result<QueryValue> {
    loop {
        cancelled(cancel)?;
        if let Some(value) = reader.property(connection, session, wire, field)? {
            return Ok(value);
        }
        reader.begin_hold();
    }
}

fn wire_at(
    connection: &Connection,
    scope: &TimelineReadScope,
    ordinal: usize,
    sequence: u64,
) -> Result<WireEvent> {
    let branch = &scope.segments[ordinal].branch_id;
    let encoded: Option<String> = connection.prepare_cached(
        "SELECT CASE json_extract(body,'$.encoding') WHEN 'event' THEN json_remove(json_extract(body,'$.record'),'$.trace') ELSE
          json_remove(json_set(json_extract(e.body,'$.records['||(?3-e.sequence)||']'),'$.event.data.turn_id',json_extract(e.body,'$.turn_id'),
            '$.event.data.request_id',json_extract(e.body,'$.request_id')),'$.trace') END
         FROM timeline_events e WHERE session_id=?1 AND branch_id=?2 AND sequence<=?3 AND last_sequence>=?3
         ORDER BY sequence DESC LIMIT 1",
    ).map_err(error)?.query_row(
        params![scope.key.session_id,branch,sequence as i64], |row| row.get(0),
    ).optional().map_err(error)?;
    let encoded =
        encoded.ok_or_else(|| error("selected event is missing from its stored interval"))?;
    serde_json::from_str(&encoded).map_err(error)
}

fn safe_filter(filter: &QueryFilter) -> Option<QueryFilter> {
    let safe = |operand: &QueryOperand| match operand {
        QueryOperand::Field { path } => !references(path),
        QueryOperand::Outer { .. } => false,
        _ => true,
    };
    match filter {
        QueryFilter::TextCandidate { .. } => Some(filter.clone()),
        QueryFilter::Compare { left, right, .. } if safe(left) && safe(right) => {
            Some(filter.clone())
        }
        QueryFilter::In { value, .. }
        | QueryFilter::Present { value }
        | QueryFilter::Prefix { value, .. }
        | QueryFilter::Contains { value, .. }
            if safe(value) =>
        {
            Some(filter.clone())
        }
        QueryFilter::And { conditions } => Some(QueryFilter::And {
            conditions: conditions.iter().filter_map(safe_filter).collect(),
        }),
        QueryFilter::Or { conditions } => conditions
            .iter()
            .map(safe_filter)
            .collect::<Option<Vec<_>>>()
            .map(|conditions| QueryFilter::Or { conditions }),
        QueryFilter::Not { condition } => safe_exact(condition).then(|| filter.clone()),
        QueryFilter::Any { field, condition } if !references(field) && safe_exact(condition) => {
            Some(filter.clone())
        }
        _ => None,
    }
}

fn safe_exact(filter: &QueryFilter) -> bool {
    match filter {
        QueryFilter::And { conditions } | QueryFilter::Or { conditions } => {
            conditions.iter().all(safe_exact)
        }
        QueryFilter::Not { condition } => safe_exact(condition),
        _ => safe_filter(filter).is_some(),
    }
}

fn related_filters<'a>(filter: &'a QueryFilter, result: &mut Vec<&'a QueryFilter>) {
    match filter {
        QueryFilter::Exists { condition } => result.push(condition),
        QueryFilter::And { conditions } | QueryFilter::Or { conditions } => {
            for condition in conditions {
                related_filters(condition, result);
            }
        }
        QueryFilter::Not { condition } => related_filters(condition, result),
        _ => (),
    }
}

fn sources<'a>(
    scope: &TimelineReadScope,
    query: &'a QuerySpec,
    compiler: &mut Compiler<'a>,
    temporary: &mut Temporary<'_>,
    reader: &mut Reader,
    cancel: &CancellationToken,
    #[cfg(feature = "testing")] statistics: &Statistics,
) -> Result<String> {
    fn collect<'a>(filter: &'a QueryFilter, filters: &mut Vec<&'a QueryFilter>) {
        match filter {
            QueryFilter::TextCandidate { .. } => {
                if !filters.contains(&filter) {
                    filters.push(filter);
                }
            }
            QueryFilter::And { conditions } | QueryFilter::Or { conditions } => {
                for condition in conditions {
                    collect(condition, filters);
                }
            }
            QueryFilter::Exists { condition }
            | QueryFilter::Not { condition }
            | QueryFilter::Any { condition, .. } => collect(condition, filters),
            _ => {}
        }
    }
    fn inline_filters<'a>(
        filter: &'a QueryFilter,
        known: Option<&[Value]>,
        related: bool,
        result: &mut Vec<&'a QueryFilter>,
    ) {
        let known = kind_values(filter).or(known);
        match filter {
            QueryFilter::TextCandidate {
                field,
                partition_by,
                ..
            } if related
                && ((field == "data"
                    && partition_by.is_empty()
                    && known.is_some_and(|kinds| {
                        kinds.iter().all(|kind| {
                            kind.as_str().is_some_and(|kind| {
                                super::text::indexed(kind, "data")
                                    && !matches!(kind, "step/text" | "step/thinking")
                            })
                        })
                    }))
                    || (field == "data.delta" && partition_by == &["kind", "data.request_id"])) =>
            {
                result.push(filter)
            }
            QueryFilter::And { conditions } | QueryFilter::Or { conditions } => {
                for filter in conditions {
                    inline_filters(filter, known, related, result);
                }
            }
            QueryFilter::Exists { condition } => inline_filters(condition, None, true, result),
            _ => (),
        }
    }
    let mut inline = Vec::new();
    if compiler.direct_relations
        && compiler.relation_scope
        && let Some(filter) = &query.filter
    {
        inline_filters(filter, None, false, &mut inline);
    }
    let mut text = Vec::new();
    if let Some(filter) = &query.filter {
        collect(filter, &mut text);
    }
    for filter in text {
        let QueryFilter::TextCandidate {
            field,
            partition_by,
            terms,
        } = filter
        else {
            unreachable!()
        };
        if inline.contains(&filter)
            && (partition_by.is_empty() || scope.segments.len() == 1)
            && let Some(grams) = super::text::trigrams(terms)
        {
            let session = compiler.bind(scope.key.session_id.clone());
            let predicates = grams.into_iter().map(|grams| {
                    let grams = compiler.bind(grams);
                    if partition_by.is_empty() {
                        format!("EXISTS(SELECT 1 FROM timeline_text_fts WHERE rowid=m.id AND timeline_text_fts MATCH {grams})")
                    } else {
                        format!("EXISTS(SELECT 1 FROM timeline_text hit WHERE hit.session_id=m.session_id AND hit.branch_id=m.branch_id AND hit.property=m.property AND hit.partition_key=m.partition_key AND EXISTS(SELECT 1 FROM timeline_text_fts WHERE rowid=hit.id AND timeline_text_fts MATCH {grams}))")
                    }
                }).collect::<Vec<_>>().join(" AND ");
            let property = compiler.bind(field.clone());
            compiler.inline_text.push((filter, format!("EXISTS(SELECT 1 FROM timeline_text m WHERE m.session_id={session} AND m.branch_id=$branch AND m.sequence=$sequence AND m.property={property} AND {predicates})")));
            continue;
        }
        let table =
            temporary.table("ord INTEGER,seq INTEGER,physical INTEGER,PRIMARY KEY(ord,seq)")?;
        let statement = super::text::candidates(scope, field, partition_by, terms, &table);
        let supported = statement.is_some();
        if let Some((sql, parameters)) = statement {
            let connection = &*temporary.connection;
            connection
                .execute_batch(&format!(
                    "CREATE INDEX {table}_physical ON {table}(ord,physical)"
                ))
                .map_err(error)?;
            let mut statement = connection.prepare(&sql).map_err(error)?;
            statement
                .execute(params_from_iter(&parameters))
                .map_err(error)?;
            #[cfg(feature = "testing")]
            record_statement(
                statistics,
                connection,
                &statement,
                &sql,
                &parameters,
                "text_candidates",
            )?;
            let mut insert = connection
                .prepare(&format!("INSERT OR IGNORE INTO {table} VALUES(?1,?2,?2)"))
                .map_err(error)?;
            for (ordinal, segment) in scope.segments.iter().enumerate() {
                for event in &segment.tail {
                    if event.sequence > segment.stored_through
                        && event.sequence >= segment.from
                        && event.sequence <= segment.through
                    {
                        insert
                            .execute(params![ordinal as i64, event.sequence as i64])
                            .map_err(error)?;
                    }
                }
            }
        }
        compiler
            .text_candidates
            .push((filter, supported.then_some(table)));
    }
    if let Some((limit, _)) = compiler.candidate_limit
        && let Some(members) = super::members::candidates(
            &*temporary.connection,
            scope,
            query,
            usize::try_from(limit).unwrap_or(usize::MAX),
            compiler.position_after,
            cancel,
            #[cfg(feature = "testing")]
            statistics,
        )?
    {
        let table =
            temporary.table("ord INTEGER,seq INTEGER,physical INTEGER,PRIMARY KEY(ord,seq)")?;
        let connection = &*temporary.connection;
        let mut insert = connection
            .prepare(&format!("INSERT INTO {table} VALUES(0,?1,?1)"))
            .map_err(error)?;
        for sequence in members.positions {
            insert
                .execute([i64::try_from(sequence).map_err(error)?])
                .map_err(error)?;
        }
        compiler.physical_candidates = Some(table);
        for terminal in members.terminals {
            compiler.resolved_relations.push((terminal, "0".into()));
        }
    }
    if compiler.direct_relations {
        fn resolve<'a>(
            filter: &'a QueryFilter,
            known: Option<&[Value]>,
            compiler: &mut Compiler<'a>,
            session: &str,
        ) {
            let known = kind_values(filter).or(known);
            match filter {
                QueryFilter::Exists { condition } => {
                    if let Some(expression) = compiler.physical_relation(condition, known) {
                        let session = compiler.bind(session.to_owned());
                        compiler
                            .resolved_relations
                            .push((condition, expression.replace("e.session_id", &session)));
                    }
                }
                QueryFilter::And { conditions } | QueryFilter::Or { conditions } => {
                    for condition in conditions {
                        resolve(condition, known, compiler, session);
                    }
                }
                QueryFilter::Not { condition } => resolve(condition, known, compiler, session),
                _ => {}
            }
        }
        if let Some(filter) = &query.filter {
            resolve(filter, None, compiler, &scope.key.session_id);
        }
    }
    let mut relations = Vec::new();
    if let Some(filter) = &query.filter {
        related_filters(filter, &mut relations);
    }
    let unresolved = relations.iter().any(|relation| {
        !compiler
            .resolved_relations
            .iter()
            .any(|(known, _)| known == relation)
    });
    if unresolved
        || query
            .filter
            .as_ref()
            .is_some_and(|filter| !compiler.physical_form_exists(filter, false))
    {
        compiler.candidate_limit = None;
    }
    let mut roles = Vec::new();
    if let Some(filter) = &query.filter {
        roles.push(filter);
        related_filters(filter, &mut roles);
        roles.retain(|role| {
            *role == filter
                || !compiler
                    .resolved_relations
                    .iter()
                    .any(|(condition, _)| condition == role)
        });
    }
    let physical = if roles.is_empty() {
        "1".into()
    } else {
        roles
            .iter()
            .enumerate()
            .map(|(index, filter)| {
                let predicate = compiler.physical_filter(filter, true).unwrap_or("1".into());
                if index == 0 && compiler.relation_scope && let Some((ordinal, sequence, descending)) = compiler.position_after {
                    let ordinal = compiler.bind(ordinal as i64);
                    let sequence = compiler.bind(sequence as i64);
                    let (op, field) = if descending { ("<", "e.sequence") } else { (">", "e.last_sequence") };
                    format!("({predicate}) AND (s.ord{op}{ordinal} OR (s.ord={ordinal} AND {field}{op}{sequence}))")
                } else { predicate }
            })
            .collect::<Vec<_>>()
            .join(" OR ")
    };
    let physical = match roles
        .iter()
        .map(|role| equality_values(role, "kind"))
        .collect::<Option<Vec<_>>>()
    {
        Some(roles) if roles.len() > 1 => {
            let mut kinds = roles
                .into_iter()
                .flatten()
                .filter_map(|value| value.as_str().map(str::to_owned))
                .collect::<Vec<_>>();
            kinds.sort();
            kinds.dedup();
            if kinds.is_empty() {
                "0".into()
            } else {
                let kinds = kinds
                    .into_iter()
                    .map(|kind| compiler.bind(kind))
                    .collect::<Vec<_>>()
                    .join(",");
                format!("e.kind IN ({kinds}) AND ({physical})")
            }
        }
        _ => physical,
    };
    if compiler.physical_candidates.is_none()
        && let [filter] = roles.as_slice()
        && let Some(kinds) = kind_values(filter)
    {
        fn necessary<'a>(
            filter: &'a QueryFilter,
            kinds: &[Value],
            compiler: &Compiler<'a>,
        ) -> Option<String> {
            match filter {
                QueryFilter::TextCandidate { field, .. }
                    if kinds.iter().all(|kind| {
                        kind.as_str()
                            .is_some_and(|kind| super::text::indexed(kind, field))
                    }) =>
                {
                    compiler
                        .text_candidates
                        .iter()
                        .find(|(candidate, _)| *candidate == filter)
                        .and_then(|(_, table)| table.clone())
                }
                QueryFilter::And { conditions } => conditions
                    .iter()
                    .find_map(|condition| necessary(condition, kinds, compiler)),
                _ => None,
            }
        }
        compiler.physical_candidates = necessary(filter, kinds, compiler);
    }
    if compiler.physical_candidates.is_none()
        && let [filter] = roles.as_slice()
    {
        compiler.physical_ids = equality_values(filter, "id").map(|values| {
            let mut ids: Vec<_> = values
                .into_iter()
                .filter_map(|value| value.as_str().map(str::to_owned))
                .collect();
            ids.sort();
            ids.dedup();
            ids
        });
    }
    let materialized = if roles.len() > 1 {
        "MATERIALIZED"
    } else {
        "NOT MATERIALIZED"
    };
    let base = compiler.base(scope, &physical);
    let fields: Vec<_> = compiler
        .fields
        .iter()
        .map(|(path, requirement)| (*path, requirement.value))
        .collect();
    let projected: Vec<_> = fields
        .iter()
        .enumerate()
        .map(|(i, (field, value))| format!("{} AS p{i}", compiler.inline(field, "b", !value)))
        .collect();
    let columns = if projected.is_empty() {
        String::new()
    } else {
        format!(",{}", projected.join(","))
    };
    let prefixes = compiler.prefixes.clone();
    let tests = prefixes
        .iter()
        .enumerate()
        .map(|(index, filter)| {
            let QueryFilter::Prefix {
                value: QueryOperand::Field { path },
                prefix,
            } = filter
            else {
                unreachable!()
            };
            let value = compiler.inline(path, "b", false);
            let prefix = compiler.bind(prefix.clone());
            format!(",query_prefix({value},{prefix}) AS t{index}")
        })
        .collect::<String>();
    let inline = format!(
        "{base}, inline AS NOT MATERIALIZED (SELECT b.ord,b.branch,b.seq{columns}{tests} FROM base b)"
    );
    let referenced: Vec<_> = fields
        .iter()
        .enumerate()
        .filter(|(_, (path, value))| references(path) && (*value || !inline_presence(path)))
        .map(|(i, (path, _))| (i, *path))
        .collect();
    let content = if referenced.is_empty() {
        None
    } else {
        let columns = referenced
            .iter()
            .map(|(i, _)| format!("p{i} TEXT"))
            .collect::<Vec<_>>()
            .join(",");
        let table = temporary.table(&format!(
            "ord INTEGER,seq INTEGER,{columns},PRIMARY KEY(ord,seq)"
        ))?;
        let connection = &*temporary.connection;
        let candidates = if roles.is_empty() {
            "1".into()
        } else {
            roles
                .into_iter()
                .map(|filter| {
                    safe_filter(filter).map_or("1".into(), |filter| {
                        compiler.filter(&filter, "i", None, None)
                    })
                })
                .collect::<Vec<_>>()
                .join(" OR ")
        };
        let sql = format!("{inline} SELECT b.ord,b.seq,json_remove(json_extract(b.wire,'$.record'),'$.trace') FROM base b JOIN inline i USING(ord,seq)
          WHERE (b.kind IN ('context/prepare','model/call') OR json_type(b.wire,'$.record.changes')='object') AND ({candidates})");
        let mut statement = connection.prepare(&sql).map_err(error)?;
        let mut rows = statement
            .query(params_from_iter(&compiler.parameters))
            .map_err(error)?;
        let placeholders = std::iter::repeat_n("?", referenced.len() + 2)
            .collect::<Vec<_>>()
            .join(",");
        let mut insert = connection
            .prepare(&format!("INSERT INTO {table} VALUES({placeholders})"))
            .map_err(error)?;
        while let Some(row) = rows.next().map_err(error)? {
            cancelled(cancel)?;
            let wire: WireEvent =
                serde_json::from_str(&row.get::<_, String>(2).map_err(error)?).map_err(error)?;
            let mut values = vec![
                row.get::<_, SqlValue>(0).map_err(error)?,
                row.get::<_, SqlValue>(1).map_err(error)?,
            ];
            for (_, field) in &referenced {
                values.push(value_text(project_wire(
                    connection,
                    &scope.key.session_id,
                    reader,
                    &wire,
                    field,
                    cancel,
                )?)?);
            }
            insert.execute(params_from_iter(values)).map_err(error)?;
        }
        drop(rows);
        #[cfg(feature = "testing")]
        record_statement(
            statistics,
            connection,
            &statement,
            &sql,
            &compiler.parameters,
            "body_candidates",
        )?;
        Some(table)
    };
    let tail = if scope.segments.iter().all(|segment| segment.tail.is_empty()) {
        None
    } else {
        let columns = (0..fields.len())
            .map(|i| format!(",p{i} TEXT"))
            .chain((0..prefixes.len()).map(|i| format!(",t{i} INTEGER")))
            .collect::<String>();
        let table = temporary.table(&format!(
            "ord INTEGER,branch TEXT,seq INTEGER{columns},PRIMARY KEY(ord,seq)"
        ))?;
        let connection = &*temporary.connection;
        let placeholders = std::iter::repeat_n("?", fields.len() + prefixes.len() + 3)
            .collect::<Vec<_>>()
            .join(",");
        let mut insert = connection
            .prepare(&format!("INSERT INTO {table} VALUES({placeholders})"))
            .map_err(error)?;
        for (ordinal, segment) in scope.segments.iter().enumerate() {
            for event in &segment.tail {
                cancelled(cancel)?;
                if let Some(table) = compiler.physical_candidates.as_ref().filter(|_| {
                    compiler
                        .resolved_relations
                        .iter()
                        .any(|(_, expression)| expression == "0")
                }) && !connection
                    .prepare_cached(&format!("SELECT 1 FROM {table} WHERE ord=?1 AND seq=?2"))
                    .map_err(error)?
                    .exists(params![ordinal as i64, event.sequence as i64])
                    .map_err(error)?
                {
                    continue;
                }
                let mut values = vec![
                    SqlValue::Integer(ordinal as i64),
                    segment.branch_id.clone().into(),
                    SqlValue::Integer(event.sequence as i64),
                ];
                for (field, value) in &fields {
                    values.push(if *value {
                        value_text(event_property(event, &segment.branch_id, field)?)?
                    } else if event_property_present(event, field)? {
                        "true".to_owned().into()
                    } else {
                        SqlValue::Null
                    });
                }
                for filter in &prefixes {
                    let QueryFilter::Prefix {
                        value: QueryOperand::Field { path },
                        prefix,
                    } = filter
                    else {
                        unreachable!()
                    };
                    let matched = matches!(event_property(event,&segment.branch_id,path)?,QueryValue::Value { value:Value::String(value) } if value.starts_with(prefix));
                    values.push(SqlValue::Integer(i64::from(matched)));
                }
                insert.execute(params_from_iter(values)).map_err(error)?;
            }
        }
        Some(table)
    };
    let columns = fields
        .iter()
        .enumerate()
        .map(|(i, _)| {
            if referenced.iter().any(|(j, _)| *j == i) {
                format!(",CASE WHEN c.ord IS NOT NULL THEN c.p{i} ELSE i.p{i} END AS p{i}")
            } else {
                format!(",i.p{i}")
            }
        })
        .collect::<String>();
    let tests = (0..prefixes.len())
        .map(|index| format!(",i.t{index}"))
        .collect::<String>();
    let columns = columns + &tests;
    let join = content.map_or(String::new(), |table| {
        format!("LEFT JOIN {table} c USING(ord,seq)")
    });
    let tail = tail.map_or(String::new(), |table| {
        format!(" UNION ALL SELECT * FROM {table}")
    });
    let keys = compiler
        .fields
        .iter()
        .enumerate()
        .filter(|(_, (_, requirement))| requirement.join)
        .map(|(i, _)| format!(",query_match_key(p{i}) AS k{i}"))
        .collect::<String>();
    Ok(format!("{inline}, properties AS NOT MATERIALIZED (SELECT i.ord,i.branch,i.seq{columns} FROM inline i {join}{tail}),
      events AS {materialized} (SELECT properties.*{keys} FROM properties)"))
}

struct Sort {
    raw: String,
    descending: bool,
    integer: bool,
}

impl Sort {
    fn sql(&self) -> String {
        if self.integer {
            self.raw.clone()
        } else {
            format!("query_scalar_key({})", self.raw)
        }
    }
}

fn sorting(query: &QuerySpec, compiler: &Compiler<'_>) -> Vec<Sort> {
    let mut keys = Vec::new();
    for order in &query.order_by {
        let descending = order.direction == TimelineQueryDirection::Descending;
        match &order.key {
            QueryOrderKey::Position => {
                keys.push(Sort {
                    raw: "r.ord".into(),
                    descending,
                    integer: true,
                });
                keys.push(Sort {
                    raw: "r.seq".into(),
                    descending,
                    integer: true,
                });
            }
            QueryOrderKey::Field { path } => keys.push(Sort {
                raw: compiler.column("r", path),
                descending,
                integer: false,
            }),
            QueryOrderKey::Count => keys.push(Sort {
                raw: "r.total".into(),
                descending,
                integer: true,
            }),
        }
    }
    if matches!(query.select, QuerySelect::Count) {
        keys.extend(query.group_by.iter().map(|path| Sort {
            raw: compiler.column("r", path),
            descending: false,
            integer: false,
        }));
    } else {
        keys.push(Sort {
            raw: "r.ord".into(),
            descending: false,
            integer: true,
        });
        keys.push(Sort {
            raw: "r.seq".into(),
            descending: false,
            integer: true,
        });
    }
    keys
}

fn after(
    compiler: &mut Compiler<'_>,
    keys: &[Sort],
    cursor: Option<&QueryCursor>,
    binding: &str,
) -> Result<String> {
    let Some(cursor) = cursor else {
        return Ok("1".into());
    };
    if cursor.binding != binding || cursor.keys.len() != keys.len() {
        return Err(AgentError::InvalidInput(
            "query cursor belongs to another query or scope".into(),
        ));
    }
    let mut equals = Vec::new();
    let mut choices = Vec::new();
    for (key, value) in keys.iter().zip(&cursor.keys) {
        let encoded = value.scalar_key()?;
        let value = if key.integer {
            let QueryValue::Value { value } = value else {
                return Err(AgentError::InvalidInput(
                    "invalid position in query cursor".into(),
                ));
            };
            let integer = value
                .as_u64()
                .and_then(|value| i64::try_from(value).ok())
                .ok_or_else(|| {
                    AgentError::InvalidInput("invalid position in query cursor".into())
                })?;
            compiler.bind(integer)
        } else {
            compiler.bind(SqlValue::Blob(encoded))
        };
        let field = key.sql();
        let relation = if key.descending { "<" } else { ">" };
        let prefix = if equals.is_empty() {
            String::new()
        } else {
            format!("{} AND ", equals.join(" AND "))
        };
        choices.push(format!("({prefix}{field}{relation}{value})"));
        equals.push(format!("{field}={value}"));
    }
    Ok(choices.join(" OR "))
}

fn selected_properties(
    connection: &Connection,
    scope: &TimelineReadScope,
    positions: &[(usize, u64)],
    fields: &[&str],
    #[cfg(feature = "testing")] statistics: &Statistics,
) -> Result<Vec<Vec<QueryValue>>> {
    let mut compiler = Compiler {
        fields: Vec::new(),
        parameters: Vec::new(),
        alias: 0,
        position_after: None,
        relation_scope: false,
        candidate_limit: None,
        inline_text: Vec::new(),
        text_candidates: Vec::new(),
        prefixes: Vec::new(),
        direct_relations: false,
        physical_candidates: None,
        physical_ids: None,
        physical_kinds: None,
        candidate_index: "timeline_events_by_kind",
        candidate_field_order: None,
        resolved_relations: Vec::new(),
    };
    let point = TimelineReadScope {
        key: scope.key.clone(),
        through: scope.through,
        segments: positions
            .iter()
            .map(
                |&(ordinal, sequence)| dim_agent_runtime::TimelineReadSegment {
                    branch_id: scope.segments[ordinal].branch_id.clone(),
                    from: sequence,
                    through: sequence,
                    stored_through: sequence,
                    tail: Vec::new(),
                },
            )
            .collect(),
    };
    let base = compiler.base(&point, "1");
    let columns=fields.iter().map(|field| {
        if let Some((array,remaining))=field.split_once("[]") {
            let array=compiler.inline(array,"b",false);
            let remaining=compiler.bind(json_path(remaining.trim_start_matches('.')));
            format!("CASE WHEN json_type({array})='array' THEN
              (SELECT json_group_array(json_object('present',json(value IS NOT NULL),'value',json(value)))
               FROM (SELECT (({array} -> ('$[' || e.key || ']')) -> {remaining}) AS value FROM json_each({array}) e)) END")
        } else { compiler.inline(field,"b",false) }
    }).collect::<Vec<_>>().join(",");
    let sql = format!("{base} SELECT {columns} FROM base b ORDER BY b.ord");
    let mut statement = connection.prepare(&sql).map_err(error)?;
    let encoded = statement
        .query_map(params_from_iter(&compiler.parameters), |row| {
            (0..fields.len())
                .map(|i| row.get::<_, Option<String>>(i))
                .collect::<rusqlite::Result<Vec<_>>>()
        })
        .map_err(error)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(error)?;
    #[cfg(feature = "testing")]
    record_statement(
        statistics,
        connection,
        &statement,
        &sql,
        &compiler.parameters,
        "selected_properties",
    )?;
    if encoded.len() != positions.len() {
        return Err(error("selected property positions are missing"));
    }
    encoded
        .into_iter()
        .map(|encoded| {
            fields
                .iter()
                .zip(encoded)
                .map(|(field, encoded)| {
                    if !field.contains("[]") {
                        return property(encoded).map_err(error);
                    }
                    let Some(encoded) = encoded else {
                        return Ok(QueryValue::Missing);
                    };
                    #[derive(serde::Deserialize)]
                    struct Element {
                        present: u8,
                        value: Value,
                    }
                    let elements: Vec<Element> = serde_json::from_str(&encoded).map_err(error)?;
                    Ok(QueryValue::Elements {
                        values: elements
                            .into_iter()
                            .map(|element| {
                                if element.present != 0 {
                                    QueryValue::Value {
                                        value: element.value,
                                    }
                                } else {
                                    QueryValue::Missing
                                }
                            })
                            .collect(),
                    })
                })
                .collect()
        })
        .collect()
}

#[cfg_attr(feature = "testing", allow(clippy::too_many_arguments))]
pub(super) fn execute(
    connection: &mut Connection,
    scope: &TimelineReadScope,
    query: &QuerySpec,
    page: Option<&QueryPage>,
    reader: &mut Reader,
    cancel: &CancellationToken,
    #[cfg(feature = "testing")] statistics: &Statistics,
) -> Result<QueryResult> {
    cancelled(cancel)?;
    for segment in &scope.segments {
        let stored: i64 = connection
            .query_row(
                "SELECT sequence FROM timeline_branches WHERE session_id=?1 AND branch_id=?2",
                params![scope.key.session_id, segment.branch_id],
                |row| row.get(0),
            )
            .map_err(error)?;
        if segment.stored_through.min(segment.through) > stored as u64 {
            return Err(error("property read exceeds stored history"));
        }
    }
    let binding = scope.query_binding(query)?;
    let mut compiler = Compiler::new(query);
    compiler.direct_relations = scope.segments.iter().all(|segment| segment.tail.is_empty());
    let mut related = Vec::new();
    if let Some(filter) = &query.filter {
        related_filters(filter, &mut related);
    }
    compiler.relation_scope = !related.is_empty();
    let keys = sorting(query, &compiler);
    let after = after(
        &mut compiler,
        &keys,
        page.and_then(|page| page.after.as_ref()),
        &binding,
    )?;
    if query.group_by.is_empty()
        && query.pick.is_none()
        && (query.order_by.is_empty()
            || matches!(
                query.order_by.first().map(|order| &order.key),
                Some(QueryOrderKey::Position)
            ))
        && let Some(cursor) = page.and_then(|page| page.after.as_ref())
    {
        let [
            QueryValue::Value { value: ordinal },
            QueryValue::Value { value: sequence },
            ..,
        ] = cursor.keys.as_slice()
        else {
            unreachable!("validated position cursor")
        };
        compiler.position_after = Some((
            ordinal.as_u64().unwrap(),
            sequence.as_u64().unwrap(),
            keys[0].descending,
        ));
    }
    if query.group_by.is_empty()
        && matches!(
            query.select,
            QuerySelect::Fields { .. } | QuerySelect::Events
        )
        && query
            .order_by
            .iter()
            .all(|order| matches!(order.key, QueryOrderKey::Position))
        && (!related.is_empty()
            || query
                .filter
                .as_ref()
                .is_none_or(|filter| compiler.physical_form_exists(filter, false)))
    {
        let descending = match query.pick {
            Some(QueryPick::Last) => true,
            Some(QueryPick::First) => false,
            None => keys[0].descending,
        };
        compiler.candidate_limit = Some((
            if query.pick.is_some() {
                1
            } else {
                page.expect("row queries have a page")
                    .limit
                    .get()
                    .saturating_add(1)
            },
            descending,
        ));
    }
    if query.group_by.is_empty()
        && query.pick.is_none()
        && matches!(
            query.select,
            QuerySelect::Fields { .. } | QuerySelect::Events
        )
        && page.is_some_and(|page| page.after.is_none())
        && let [first, last] = query.order_by.as_slice()
        && matches!(&first.key, QueryOrderKey::Field { path } if path == "data.current.at.sequence")
        && last.key == QueryOrderKey::Position
        && first.direction == last.direction
        && let Some(filter) = &query.filter
        && kind_values(filter)
            .is_some_and(|kinds| kinds == [serde_json::json!("context-usage/recovered")])
        && equality_values(filter, "data.epoch")
            .is_some_and(|values| values.len() == 1 && values[0].is_string())
        && equality_values(filter, "data.current.at.branch")
            .is_some_and(|values| values.len() == 1 && values[0].as_u64().is_some())
        && compiler.physical_form_exists(filter, false)
    {
        let descending = first.direction == TimelineQueryDirection::Descending;
        compiler.candidate_limit = Some((
            page.expect("a scalar-key page")
                .limit
                .get()
                .saturating_add(1),
            descending,
        ));
        compiler.candidate_field_order = Some(descending);
    }
    if query.group_by.is_empty()
        && matches!(query.select, QuerySelect::Count | QuerySelect::Exists)
        && scope.segments.iter().all(|segment| segment.tail.is_empty())
    {
        let predicate = query.filter.as_ref().map_or(Some("1".into()), |filter| {
            compiler.physical_filter(filter, false)
        });
        if let Some(predicate) = predicate {
            let physical = compiler.physical(scope, &predicate);
            let count = matches!(query.select, QuerySelect::Count);
            let expression = if count {
                "(SELECT coalesce(sum(min(last_sequence,hi)-max(sequence,lo)+1),0) FROM physical)"
            } else {
                "EXISTS(SELECT 1 FROM physical)"
            };
            let sql = format!("{physical} SELECT {expression}");
            let mut statement = connection.prepare(&sql).map_err(error)?;
            let value: i64 = statement
                .query_row(params_from_iter(&compiler.parameters), |row| row.get(0))
                .map_err(error)?;
            #[cfg(feature = "testing")]
            record_statement(
                statistics,
                connection,
                &statement,
                &sql,
                &compiler.parameters,
                "scalar",
            )?;
            return Ok(QueryResult {
                result: if count {
                    QueryOutput::Count {
                        count: value as u64,
                    }
                } else {
                    QueryOutput::Exists { exists: value != 0 }
                },
                next: None,
            });
        }
        compiler.parameters.clear();
    }
    let mut temporary = Temporary {
        connection,
        names: Vec::new(),
    };
    let source = sources(
        scope,
        query,
        &mut compiler,
        &mut temporary,
        reader,
        cancel,
        #[cfg(feature = "testing")]
        statistics,
    )?;
    let connection = &*temporary.connection;
    let filter = query.filter.as_ref().map_or_else(
        || "1".to_owned(),
        |filter| compiler.filter(filter, "a", None, None),
    );
    let filtered = format!("{source}, filtered AS (SELECT a.* FROM events a WHERE {filter})");
    if query.group_by.is_empty()
        && matches!(
            query.select,
            QuerySelect::Count | QuerySelect::Exists | QuerySelect::Characters { .. }
        )
    {
        let expression = match &query.select {
            QuerySelect::Characters { field } => format!(
                "(SELECT coalesce(sum(query_characters({})),0) FROM filtered f)",
                compiler.column("f", field)
            ),
            QuerySelect::Count => "(SELECT count(*) FROM filtered)".into(),
            _ => "EXISTS(SELECT 1 FROM filtered)".into(),
        };
        let sql = format!("{filtered} SELECT {expression}");
        let mut statement = connection.prepare(&sql).map_err(error)?;
        let value: i64 = statement
            .query_row(params_from_iter(&compiler.parameters), |row| row.get(0))
            .map_err(error)?;
        #[cfg(feature = "testing")]
        record_statement(
            statistics,
            connection,
            &statement,
            &sql,
            &compiler.parameters,
            "scalar",
        )?;
        return Ok(QueryResult {
            result: if matches!(
                query.select,
                QuerySelect::Count | QuerySelect::Characters { .. }
            ) {
                QueryOutput::Count {
                    count: value as u64,
                }
            } else {
                QueryOutput::Exists { exists: value != 0 }
            },
            next: None,
        });
    }
    let grouped = !query.group_by.is_empty() || query.pick.is_some();
    let results = if query.group_by.is_empty() && query.pick.is_some() {
        let direction = if query.pick == Some(QueryPick::Last) {
            "DESC"
        } else {
            "ASC"
        };
        format!(
            "{filtered}, results AS (SELECT *,1 AS total FROM filtered ORDER BY ord {direction},seq {direction} LIMIT 1)"
        )
    } else if grouped {
        let group = query
            .group_by
            .iter()
            .map(|path| format!("query_scalar_key({})", compiler.column("f", path)))
            .collect::<Vec<_>>()
            .join(",");
        let partition = if group.is_empty() {
            String::new()
        } else {
            format!("PARTITION BY {group}")
        };
        let direction = if query.pick == Some(QueryPick::Last) {
            "DESC"
        } else {
            "ASC"
        };
        let group_values = query
            .group_by
            .iter()
            .enumerate()
            .map(|(i, path)| {
                format!(
                    ",first_value({}) OVER({partition} ORDER BY f.ord,f.seq) AS g{i}",
                    compiler.column("f", path)
                )
            })
            .collect::<String>();
        format!("{filtered}, ranked AS (SELECT f.*{group_values},count(*) OVER({partition}) AS total,
          row_number() OVER({partition} ORDER BY f.ord {direction},f.seq {direction}) AS rank FROM filtered f),
          results AS (SELECT * FROM ranked WHERE rank=1)")
    } else {
        format!("{filtered}, results AS (SELECT *,1 AS total FROM filtered)")
    };
    let page = page.expect("row queries have a page");
    let order = keys
        .iter()
        .map(|key| {
            format!(
                "{} {}",
                key.sql(),
                if key.descending { "DESC" } else { "ASC" }
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let group = query
        .group_by
        .iter()
        .enumerate()
        .map(|(i, _)| format!(",r.g{i}"))
        .collect::<String>();
    let key_columns = keys
        .iter()
        .map(|key| {
            format!(
                ",{}",
                if key.integer {
                    format!("CAST({} AS TEXT)", key.raw)
                } else {
                    key.raw.clone()
                }
            )
        })
        .collect::<String>();
    let limit = compiler.bind(page.limit.get().saturating_add(1).min(i64::MAX as u64) as i64);
    let sql = format!(
        "{results} SELECT r.ord,r.seq,r.total{group}{key_columns} FROM results r WHERE ({after}) ORDER BY {order} LIMIT {limit}"
    );
    let mut statement = connection.prepare(&sql).map_err(error)?;
    let mut selected = statement
        .query(params_from_iter(&compiler.parameters))
        .map_err(error)?;
    let mut rows = Vec::new();
    let mut events = Vec::new();
    let mut last = None;
    let mut more = false;
    let mut count = 0;
    let mut positions = Vec::new();
    while let Some(row) = selected.next().map_err(error)? {
        cancelled(cancel)?;
        if count == page.limit.get() {
            more = true;
            break;
        }
        count += 1;
        let ordinal = row.get::<_, i64>(0).map_err(error)? as usize;
        let sequence = row.get::<_, i64>(1).map_err(error)? as u64;
        let total = row.get::<_, i64>(2).map_err(error)? as u64;
        let group = (0..query.group_by.len())
            .map(|i| property(row.get(3 + i)?))
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(error)?;
        let cursor_keys = (0..keys.len())
            .map(|i| property(row.get(3 + query.group_by.len() + i)?))
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(error)?;
        last = Some(QueryCursor {
            binding: binding.clone(),
            keys: cursor_keys,
        });
        positions.push((ordinal, sequence, total, group));
    }
    drop(selected);
    #[cfg(feature = "testing")]
    record_statement(
        statistics,
        connection,
        &statement,
        &sql,
        &compiler.parameters,
        "selection",
    )?;
    let inline_fields: Vec<_> = if let QuerySelect::Fields { fields } = &query.select {
        fields
            .iter()
            .filter(|field| !references(field))
            .map(String::as_str)
            .collect()
    } else {
        Vec::new()
    };
    let mut properties = Vec::new();
    if !inline_fields.is_empty() {
        let stored: Vec<_> = positions
            .iter()
            .filter(|&&(ordinal, sequence, _, _)| {
                sequence <= scope.segments[ordinal].stored_through
            })
            .map(|&(ordinal, sequence, _, _)| (ordinal, sequence))
            .collect();
        for batch in stored.chunks(128) {
            cancelled(cancel)?;
            let values = selected_properties(
                connection,
                scope,
                batch,
                &inline_fields,
                #[cfg(feature = "testing")]
                statistics,
            )?;
            properties.extend(values);
        }
    }
    let mut properties = properties.into_iter();
    for (ordinal, sequence, total, group) in positions {
        cancelled(cancel)?;
        let segment = &scope.segments[ordinal];
        let tail = if sequence > segment.stored_through {
            Some(
                segment
                    .tail
                    .iter()
                    .find(|event| event.sequence == sequence)
                    .ok_or_else(|| error("selected tail event is missing"))?,
            )
        } else {
            None
        };
        match &query.select {
            QuerySelect::Count => rows.push(QueryRow {
                group,
                values: vec![QueryValue::Value {
                    value: total.into(),
                }],
            }),
            QuerySelect::Fields { fields } => {
                let mut values = Vec::with_capacity(fields.len());
                let mut wire = None;
                let inline = if tail.is_some() || inline_fields.is_empty() {
                    Vec::new()
                } else {
                    properties
                        .next()
                        .expect("stored property rows preserve selection order")
                };
                let mut inline = inline.into_iter();
                for field in fields {
                    values.push(if let Some(event) = tail {
                        event_property(event, &segment.branch_id, field)?
                    } else if references(field) {
                        if wire.is_none() {
                            wire = Some(wire_at(connection, scope, ordinal, sequence)?);
                        }
                        project_wire(
                            connection,
                            &scope.key.session_id,
                            reader,
                            wire.as_ref().expect("loaded wire"),
                            field,
                            cancel,
                        )?
                    } else {
                        inline
                            .next()
                            .expect("selected inline fields preserve their order")
                    });
                }
                rows.push(QueryRow { group, values });
            }
            QuerySelect::Events => {
                let event = if let Some(event) = tail {
                    (*event).clone()
                } else {
                    let mut wire = wire_at(connection, scope, ordinal, sequence)?;
                    loop {
                        cancelled(cancel)?;
                        match reader.decode(connection, &scope.key.session_id, wire)? {
                            Ok(record) => break record.event,
                            Err(pending) => {
                                wire = pending;
                                reader.begin_hold();
                            }
                        }
                    }
                };
                events.push(QueryEvent {
                    group,
                    position: TimelinePosition {
                        branch_id: segment.branch_id.clone(),
                        sequence,
                    },
                    event,
                });
            }
            QuerySelect::Exists | QuerySelect::Characters { .. } => {
                unreachable!("scalar queries return before row selection")
            }
        }
    }
    Ok(QueryResult {
        result: if matches!(query.select, QuerySelect::Events) {
            QueryOutput::Events { events }
        } else {
            QueryOutput::Rows { rows }
        },
        next: if more { last } else { None },
    })
}

use crate::{AgentError, Result, TimelineQueryDirection, TimelineQueryScope};
use dim_agent_types::{AgentEvent, TimelinePosition};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{cmp::Ordering, num::NonZeroU64, sync::Arc};

/// A property read at a fixed branch boundary (ADR 022).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TimelineQuery {
    pub scope: TimelineQueryScope,
    pub from: Option<TimelinePosition>,
    pub through: Option<u64>,
    #[serde(default)]
    pub include_publishing: bool,
    pub query: QuerySpec,
    pub page: Option<QueryPage>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuerySpec {
    pub filter: Option<QueryFilter>,
    pub select: QuerySelect,
    #[serde(default)]
    pub group_by: Vec<String>,
    pub pick: Option<QueryPick>,
    #[serde(default)]
    pub order_by: Vec<QueryOrder>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum QuerySelect {
    Fields { fields: Vec<String> },
    Events,
    Count,
    Characters { field: String },
    Exists,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueryPick {
    First,
    Last,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueryOrder {
    pub key: QueryOrderKey,
    pub direction: TimelineQueryDirection,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum QueryOrderKey {
    Position,
    Field { path: String },
    Count,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum QueryOperand {
    Field { path: String },
    Outer { path: String },
    Root { path: String },
    Concat { parts: Vec<QueryOperand> },
    Element { path: String },
    Value { value: Value },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueryComparison {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum QueryFilter {
    Compare {
        left: QueryOperand,
        comparison: QueryComparison,
        right: QueryOperand,
    },
    In {
        value: QueryOperand,
        values: Vec<Value>,
    },
    Present {
        value: QueryOperand,
    },
    Prefix {
        value: QueryOperand,
        prefix: String,
    },
    Contains {
        value: QueryOperand,
        text: String,
    },
    /// A superset of rows whose partition's concatenated property contains every term
    /// after Unicode lowercasing. Missing partition keys never join different events.
    /// Consumers must verify their own text; this condition cannot be negated.
    TextCandidate {
        field: String,
        partition_by: Vec<String>,
        terms: Vec<String>,
    },
    And {
        conditions: Vec<QueryFilter>,
    },
    Or {
        conditions: Vec<QueryFilter>,
    },
    Not {
        condition: Box<QueryFilter>,
    },
    Exists {
        condition: Box<QueryFilter>,
    },
    Any {
        field: String,
        condition: Box<QueryFilter>,
    },
}

/// Missing properties and array projections retain their shape independently of JSON null.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum QueryValue {
    Missing,
    Value { value: Value },
    Elements { values: Vec<QueryValue> },
}

impl QueryValue {
    /// Scalar type order; structured values have no scalar order.
    pub fn scalar_kind(&self) -> Option<u8> {
        match self {
            Self::Missing => Some(0),
            Self::Value { value: Value::Null } => Some(1),
            Self::Value {
                value: Value::Bool(_),
            } => Some(2),
            Self::Value {
                value: Value::Number(_),
            } => Some(3),
            Self::Value {
                value: Value::String(_),
            } => Some(4),
            _ => None,
        }
    }

    /// Compares scalar properties without converting integers to floating point.
    pub fn scalar_cmp(&self, other: &Self) -> Result<Ordering> {
        let (Some(left_kind), Some(right_kind)) = (self.scalar_kind(), other.scalar_kind()) else {
            return invalid("ordering and grouping require scalar properties");
        };
        Ok(match (self, other) {
            (
                Self::Value {
                    value: Value::Bool(a),
                },
                Self::Value {
                    value: Value::Bool(b),
                },
            ) => a.cmp(b),
            (
                Self::Value {
                    value: Value::Number(a),
                },
                Self::Value {
                    value: Value::Number(b),
                },
            ) => number_cmp(a, b),
            (
                Self::Value {
                    value: Value::String(a),
                },
                Self::Value {
                    value: Value::String(b),
                },
            ) => a.cmp(b),
            _ => left_kind.cmp(&right_kind),
        })
    }

    /// A lexicographic key for scalar ordering and grouping, preserving integer precision.
    /// Objects and array projections cannot be ordered or grouped.
    pub fn scalar_key(&self) -> Result<Vec<u8>> {
        match self {
            Self::Missing => Ok(vec![0]),
            Self::Value { value: Value::Null } => Ok(vec![1]),
            Self::Value {
                value: Value::Bool(value),
            } => Ok(vec![2, u8::from(*value)]),
            Self::Value {
                value: Value::String(value),
            } => {
                let mut key = Vec::with_capacity(value.len() + 1);
                key.push(4);
                key.extend(value.as_bytes());
                Ok(key)
            }
            Self::Value {
                value: Value::Number(value),
            } => {
                let integer = value
                    .as_i64()
                    .map(|v| (v < 0, v.unsigned_abs()))
                    .or_else(|| value.as_u64().map(|v| (false, v)));
                let (negative, exponent, mantissa) = if let Some((negative, magnitude)) = integer {
                    if magnitude == 0 {
                        return Ok(vec![3, 1]);
                    }
                    let exponent = 63 - magnitude.leading_zeros();
                    (negative, exponent as i16, magnitude << (63 - exponent))
                } else {
                    let value = value.as_f64().expect("JSON numbers are finite");
                    if value == 0.0 {
                        return Ok(vec![3, 1]);
                    }
                    let bits = value.to_bits();
                    let fraction = bits & ((1_u64 << 52) - 1);
                    let stored_exponent = ((bits >> 52) & 0x7ff) as i16;
                    if stored_exponent == 0 {
                        let leading = 63 - fraction.leading_zeros();
                        (
                            value.is_sign_negative(),
                            leading as i16 - 1074,
                            fraction << (63 - leading),
                        )
                    } else {
                        (
                            value.is_sign_negative(),
                            stored_exponent - 1023,
                            ((1_u64 << 52) | fraction) << 11,
                        )
                    }
                };
                let mut key = vec![3, if negative { 0 } else { 2 }];
                key.extend(((exponent + 1074) as u16).to_be_bytes());
                key.extend(mantissa.to_be_bytes());
                if negative {
                    for byte in &mut key[2..] {
                        *byte = !*byte;
                    }
                }
                Ok(key)
            }
            _ => invalid("ordering and grouping require scalar properties"),
        }
    }
}

impl QueryComparison {
    /// Missing, structured and differently typed operands do not match any comparison.
    pub fn matches(self, left: &QueryValue, right: &QueryValue) -> bool {
        let Some(kind) = left.scalar_kind() else {
            return false;
        };
        if kind == 0
            || Some(kind) != right.scalar_kind()
            || (kind == 1 && !matches!(self, Self::Eq | Self::Ne))
        {
            return false;
        }
        let order = left.scalar_cmp(right).expect("both operands are scalars");
        match self {
            Self::Eq => order.is_eq(),
            Self::Ne => !order.is_eq(),
            Self::Lt => order.is_lt(),
            Self::Le => !order.is_gt(),
            Self::Gt => order.is_gt(),
            Self::Ge => !order.is_lt(),
        }
    }
}

fn number_cmp(left: &serde_json::Number, right: &serde_json::Number) -> Ordering {
    fn integer(value: &serde_json::Number) -> Option<i128> {
        value
            .as_i64()
            .map(i128::from)
            .or_else(|| value.as_u64().map(i128::from))
    }
    fn int_float(left: i128, right: f64) -> Ordering {
        if left as f64 == right {
            left.cmp(&(right as i128))
        } else {
            (left as f64).total_cmp(&right)
        }
    }
    match (integer(left), integer(right)) {
        (Some(left), Some(right)) => left.cmp(&right),
        (Some(left), None) => int_float(left, right.as_f64().expect("JSON numbers are finite")),
        (None, Some(right)) => {
            int_float(right, left.as_f64().expect("JSON numbers are finite")).reverse()
        }
        (None, None) => {
            let left = left.as_f64().expect("JSON numbers are finite");
            let right = right.as_f64().expect("JSON numbers are finite");
            if left == right {
                Ordering::Equal
            } else {
                left.total_cmp(&right)
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueryPage {
    pub limit: NonZeroU64,
    pub after: Option<QueryCursor>,
}

/// Continuation data bound to the query and logical scope, without server-side state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueryCursor {
    pub binding: String,
    pub keys: Vec<QueryValue>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueryRow {
    pub group: Vec<QueryValue>,
    pub values: Vec<QueryValue>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueryEvent {
    pub group: Vec<QueryValue>,
    pub position: TimelinePosition,
    pub event: Arc<AgentEvent>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum QueryOutput {
    Rows { rows: Vec<QueryRow> },
    Events { events: Vec<QueryEvent> },
    Count { count: u64 },
    Exists { exists: bool },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueryResult {
    pub result: QueryOutput,
    pub next: Option<QueryCursor>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TimelineQueryReply {
    pub through: u64,
    pub result: QueryOutput,
    pub next: Option<QueryCursor>,
}

impl QuerySpec {
    /// Rejects invalid expression combinations before any store work starts.
    pub fn validate(&self, page: Option<&QueryPage>) -> Result<()> {
        let scalar = self.group_by.is_empty()
            && matches!(
                self.select,
                QuerySelect::Count | QuerySelect::Exists | QuerySelect::Characters { .. }
            );
        if scalar != page.is_none() {
            return invalid("row queries require a page; scalar queries do not accept one");
        }
        if self.pick.is_some()
            && matches!(
                self.select,
                QuerySelect::Count | QuerySelect::Exists | QuerySelect::Characters { .. }
            )
        {
            return invalid("count and exists do not accept pick");
        }
        if !self.group_by.is_empty()
            && (matches!(self.select, QuerySelect::Exists)
                || (!matches!(self.select, QuerySelect::Count) && self.pick.is_none()))
        {
            return invalid("groups require count or an event pick");
        }
        if let QuerySelect::Characters { field } = &self.select {
            if !self.group_by.is_empty() {
                return invalid("character totals do not accept grouping");
            }
            validate_field(field, false)?;
        }
        for field in &self.group_by {
            validate_field(field, false)?;
        }
        if let QuerySelect::Fields { fields } = &self.select {
            for field in fields {
                validate_field(field, true)?;
            }
        }
        for order in &self.order_by {
            match &order.key {
                QueryOrderKey::Field { path } => {
                    validate_field(path, false)?;
                    if matches!(self.select, QuerySelect::Count) && !self.group_by.contains(path) {
                        return invalid("group counts can only order by group fields or count");
                    }
                }
                QueryOrderKey::Count if !matches!(self.select, QuerySelect::Count) => {
                    return invalid("count ordering requires grouped counts");
                }
                QueryOrderKey::Position if matches!(self.select, QuerySelect::Count) => {
                    return invalid("group counts have no event position");
                }
                _ => {}
            }
        }
        if scalar && !self.order_by.is_empty() {
            return invalid("scalar queries do not accept ordering");
        }
        if let Some(filter) = &self.filter {
            filter.validate(false, false)?;
        }
        Ok(())
    }
}

impl QueryFilter {
    fn has_text_candidate(&self) -> bool {
        match self {
            Self::TextCandidate { .. } => true,
            Self::And { conditions } | Self::Or { conditions } => {
                conditions.iter().any(Self::has_text_candidate)
            }
            Self::Not { condition } | Self::Exists { condition } | Self::Any { condition, .. } => {
                condition.has_text_candidate()
            }
            _ => false,
        }
    }
    fn validate(&self, related: bool, element: bool) -> Result<()> {
        let operand = |value: &QueryOperand| value.validate(related, element);
        match self {
            Self::Compare { left, right, .. } => {
                operand(left)?;
                operand(right)
            }
            Self::In { value, values } => {
                operand(value)?;
                if values.iter().any(|v| v.is_array() || v.is_object()) {
                    return invalid("membership values must be scalars");
                }
                Ok(())
            }
            Self::Present { value } | Self::Prefix { value, .. } | Self::Contains { value, .. } => {
                operand(value)
            }
            Self::And { conditions } | Self::Or { conditions } => conditions
                .iter()
                .try_for_each(|condition| condition.validate(related, element)),
            Self::Not { condition } => {
                if condition.has_text_candidate() {
                    return invalid("text candidates cannot be negated");
                }
                condition.validate(related, element)
            }
            Self::TextCandidate {
                field,
                partition_by,
                terms,
            } => {
                if element {
                    return invalid("text candidates cannot start inside an array condition");
                }
                validate_field(field, false)?;
                for field in partition_by {
                    validate_field(field, false)?;
                }
                if terms.is_empty() || terms.iter().any(String::is_empty) {
                    return invalid("text candidates require nonempty terms");
                }
                Ok(())
            }
            Self::Exists { condition } => {
                if element {
                    return invalid("related queries cannot start inside an array condition");
                }
                condition.validate(true, false)
            }
            Self::Any { field, condition } => {
                if element {
                    return invalid("array conditions cannot nest");
                }
                validate_field(field, false)?;
                condition.validate(related, true)
            }
        }
    }
}

impl QueryOperand {
    fn validate(&self, related: bool, element: bool) -> Result<()> {
        match self {
            Self::Field { path } => validate_field(path, false),
            Self::Outer { path } | Self::Root { path } if related => validate_field(path, false),
            Self::Concat { parts } => parts
                .iter()
                .try_for_each(|part| part.validate(related, element)),
            Self::Element { path } if element => validate_path(path, false, true),
            Self::Outer { .. } | Self::Root { .. } => {
                invalid("outer fields require a related query")
            }
            Self::Element { .. } => invalid("element fields require an array condition"),
            Self::Value { value } if value.is_array() || value.is_object() => {
                invalid("comparison literals must be scalars")
            }
            Self::Value { .. } => Ok(()),
        }
    }
}

fn validate_field(path: &str, array: bool) -> Result<()> {
    match path {
        "kind" | "id" | "create_at" | "tags" | "parent_call_id" | "position"
        | "position.branch_id" | "position.sequence" | "context.render" | "context.state"
        | "data" => Ok(()),
        _ => match path.strip_prefix("data.") {
            Some(path) => validate_path(path, array, false),
            None => invalid("a property must name an event field or position"),
        },
    }
}

fn validate_path(path: &str, array: bool, empty: bool) -> Result<()> {
    if empty && path.is_empty() {
        return Ok(());
    }
    let mut arrays = 0;
    for part in path.split('.') {
        let (name, suffix) = part.split_once('[').unwrap_or((part, ""));
        if part.contains('[') {
            let Some(index) = suffix.strip_suffix(']') else {
                return invalid("an array index must end with a closing bracket");
            };
            if index.is_empty() {
                arrays += 1;
            } else if (index != "0" && index.starts_with('0'))
                || !index.bytes().all(|byte| byte.is_ascii_digit())
                || index.parse::<i32>().is_err()
            {
                return invalid("an array index must be a decimal integer from 0 to 2147483647");
            }
        }
        if name.is_empty()
            || name.chars().any(|c| {
                c.is_whitespace()
                    || c.is_control()
                    || matches!(c, '[' | ']' | '*' | '"' | '\'' | '\\' | '$' | '#')
            })
        {
            return invalid("a property path contains an invalid field name");
        }
    }
    if arrays > usize::from(array) {
        return invalid("a projection permits one array path; conditions use any");
    }
    Ok(())
}

fn invalid<T>(message: &str) -> Result<T> {
    Err(AgentError::InvalidInput(message.into()))
}

/// Pins the equivalence a store relies on when it compares properties by
/// `query_match_key` instead of `query_compare` per candidate: `Eq` holds
/// exactly when both sides hold a match key and the keys are equal (#1473).
#[cfg(test)]
mod match_key_proof {
    use super::*;
    use serde_json::{Value, json};

    /// Values that stress integer/float equality, signed zero, precision
    /// boundaries and text that is not ASCII.
    fn candidates() -> Vec<Value> {
        let mut values = vec![
            Value::Null,
            Value::Bool(false),
            Value::Bool(true),
            json!(""),
            json!("0"),
            json!("1"),
            json!("1.0"),
            json!(" "),
            json!("ünïcode"),
            json!("日本語"),
            json!("\u{0}"),
            json!([]),
            json!([1]),
            json!({}),
            json!({"a": 1}),
        ];
        for integer in [
            0i64,
            1,
            -1,
            2,
            -2,
            255,
            256,
            i64::from(i32::MAX),
            i64::from(i32::MIN),
            1 << 30,
            1 << 52,
            (1 << 53) - 1,
            1 << 53,
            (1 << 53) + 1,
            1 << 62,
            i64::MAX,
            i64::MIN,
        ] {
            values.push(Value::from(integer));
        }
        // Above `i64::MAX` a JSON number is a `u64`, and `scalar_key` keeps it exact.
        for unsigned in [(1u64 << 63) + 1, u64::MAX, 1u64 << 63] {
            values.push(Value::from(unsigned));
        }
        for text in ["18446744073709551615", "9223372036854775808"] {
            values.push(text.parse().expect("a JSON number"));
        }
        for float in [
            0.0f64,
            -0.0,
            0.5,
            -0.5,
            1.0,
            -1.0,
            1.5,
            2.0,
            255.0,
            9007199254740992.0,
            9007199254740993.0,
            1.0e300,
            -1.0e300,
            f64::MIN_POSITIVE,
            -f64::MIN_POSITIVE,
            5.0e-324,
            -5.0e-324,
            2.2250738585072014e-308,
            f64::MAX,
            f64::MIN,
            std::f64::consts::PI,
        ] {
            values.push(json!(float));
        }
        values
    }

    fn key(value: &Value) -> Option<Vec<u8>> {
        let value = QueryValue::Value {
            value: value.clone(),
        };
        matches!(value.scalar_kind(), Some(1..=4))
            .then(|| value.scalar_key().expect("a match key is scalar"))
    }

    fn value(value: &Value) -> QueryValue {
        QueryValue::Value {
            value: value.clone(),
        }
    }

    #[test]
    fn equality_matches_exactly_when_both_sides_hold_equal_match_keys() {
        let values = candidates();
        let mut agreeing = 0;
        let mut unequal_keys = 0;
        let mut comparisons = 0;
        for left in &values {
            for right in &values {
                comparisons += 1;
                let matches = QueryComparison::Eq.matches(&value(left), &value(right));
                let (Some(left_key), Some(right_key)) = (key(left), key(right)) else {
                    assert!(
                        !matches,
                        "a value without a match key matched: {left} = {right}"
                    );
                    continue;
                };
                agreeing += 1;
                if left_key != right_key {
                    unequal_keys += 1;
                }
                assert_eq!(
                    matches,
                    left_key == right_key,
                    "{left} = {right}: matches={matches}, keys {} = {}",
                    left_key.len(),
                    right_key.len()
                );
            }
        }
        println!(
            "{} values, {comparisons} pairs, {agreeing} with two match keys, {unequal_keys} of them unequal",
            values.len()
        );
        assert!(
            comparisons > 1000,
            "{comparisons} pairs is too few to prove it"
        );
        assert!(
            unequal_keys > 1000,
            "{unequal_keys} unequal key pairs is too few to prove the negative case"
        );
    }

    #[test]
    fn structured_and_missing_values_never_hold_a_match_key() {
        for value in [
            QueryValue::Missing,
            QueryValue::Value { value: Value::Null },
        ] {
            assert!(value.scalar_kind().is_some());
        }
        for structured in [json!([]), json!([1]), json!({}), json!({"a": 1})] {
            assert_eq!(key(&structured), None, "{structured} holds a match key");
            assert!(
                !QueryComparison::Eq.matches(&value(&structured), &value(&structured)),
                "{structured} matched itself"
            );
        }
        assert_eq!(
            QueryValue::Missing.scalar_key().expect("missing has a key"),
            vec![0]
        );
    }

    /// The pairs most likely to drift apart, spelled out so a failure is readable.
    #[test]
    fn the_named_edge_pairs_agree() {
        for (left, right, equal) in [
            (json!(1), json!(1.0), true),
            (json!(1), json!(1e0), true),
            (json!(9007199254740993i64), json!(9007199254740992.0), false),
            (json!(0), json!(-0.0), true),
            (json!(true), json!(1), false),
            (json!("1"), json!(1), false),
            (json!(null), json!(null), true),
            (json!(""), json!(""), true),
            (json!("a"), json!("A"), false),
            (json!(-0.0), json!(0.0), true),
            (json!(18446744073709551615u64), json!(-1), false),
        ] {
            let matches = QueryComparison::Eq.matches(&value(&left), &value(&right));
            assert_eq!(matches, equal, "{left} = {right}");
            assert_eq!(
                key(&left) == key(&right),
                equal,
                "{left} = {right}: keys disagree with the comparison"
            );
        }
        // A missing operand never matches, even against a missing one.
        assert!(!QueryComparison::Eq.matches(&QueryValue::Missing, &QueryValue::Missing));
        assert!(!QueryComparison::Ne.matches(&QueryValue::Missing, &QueryValue::Missing));
        assert_eq!(key(&json!(null)), Some(vec![1]));
        assert_eq!(key(&json!(0)), key(&json!(-0.0)));
    }
}

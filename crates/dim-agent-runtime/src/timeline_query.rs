//! The one bounded read of a branch's facts ([ADR 019](../../../docs/adr/019-event-query.md),
//! [plugin contract §5.4](../../../docs/design/plugin-contract.md#54-the-event-query)).
//!
//! A capability names what it accepts; storage selects, orders and bounds the
//! events ([ADR 019 D3](../../../docs/adr/019-event-query.md#d3-the-query-addresses-the-event-contract-not-the-storage)).
//! The request and the reply are serde data: they cross languages through the
//! `timeline` service.

use crate::{AgentError, Result};
use dim_agent_types::{AgentEvent, TimelinePosition};
use serde::{Deserialize, Serialize};
use std::{num::NonZeroU64, sync::Arc};

/// Which branches a query covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TimelineQueryScope {
    /// The queried branch alone.
    Branch,
    /// The branch chain: each ancestor up to its cut, root first, then the
    /// queried branch.
    Chain,
}

/// The end a page is taken from. A reply is always ordered by chain position
/// and then sequence, in this direction; the offset counts from that end.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TimelineQueryDirection {
    /// Oldest first: the page is the oldest matching facts, and `offset`
    /// starts it further along.
    #[default]
    Ascending,
    /// Newest first: the page is the newest matching facts, and `offset`
    /// starts it further back.
    Descending,
}

/// The key a read orders by; a chain read orders by chain position first, so
/// `sequence` is the sequence within an event's own branch.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TimelineQueryOrderBy {
    #[default]
    Sequence,
}

/// One of the envelope's two context flags a query may name. Naming either
/// returns the events that carry at least one named flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TimelineQueryFlag {
    /// The flag that puts an event in the rendered context.
    Render,
    /// The flag that puts an event in the projection's checkpoint state.
    State,
}

impl TimelineQueryFlag {
    /// Whether `flags`, the envelope's `(render, state)` pair, carries this
    /// flag.
    pub fn is_set(self, flags: (bool, bool)) -> bool {
        match self {
            Self::Render => flags.0,
            Self::State => flags.1,
        }
    }
}

/// One equality against one JSON scalar of an event's data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TimelineQueryPredicate {
    /// Field names inside the event's data joined by dots; no array indexing
    /// and no wildcards.
    pub path: String,
    /// The JSON scalar the path must equal.
    pub value: serde_json::Value,
}

impl TimelineQueryPredicate {
    /// Whether `event`'s data holds `value` at `path`. A path the event does
    /// not have matches nothing.
    pub fn matches(&self, event: &AgentEvent) -> bool {
        let Ok(kind) = serde_json::to_value(&event.kind) else {
            return false;
        };
        let Some(mut current) = kind.get("data") else {
            return false;
        };
        for segment in self.path.split('.') {
            let Some(next) = current.get(segment) else {
                return false;
            };
            current = next;
        }
        *current == self.value
    }

    /// The path as a JSON path from the root of the event's data, `None` when
    /// its spelling is not the one the contract fixes.
    pub fn json_path(&self) -> Option<String> {
        let valid = self.path.split('.').all(|segment| {
            !segment.is_empty()
                && !segment.chars().any(|c| {
                    c.is_whitespace()
                        || c.is_control()
                        || matches!(c, '.' | '[' | ']' | '*' | '"' | '\'' | '\\' | '$' | '#')
                })
        });
        valid.then(|| format!("$.{path}", path = self.path))
    }

    fn validate(&self) -> Result<()> {
        if self.json_path().is_none() {
            return Err(AgentError::InvalidInput(format!(
                "query path {:?} is not field names joined by dots",
                self.path
            )));
        }
        if matches!(
            self.value,
            serde_json::Value::Array(_) | serde_json::Value::Object(_)
        ) {
            return Err(AgentError::InvalidInput(
                "query predicate compares one JSON scalar".into(),
            ));
        }
        Ok(())
    }
}

/// One bounded read of a branch's facts: what it covers, how far it reaches,
/// what it accepts, and how much of the result it returns.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TimelineQuery {
    pub scope: TimelineQueryScope,
    /// The event the read begins at, named by its id; the event is included.
    /// At most one of this and `from_sequence` may be given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_event: Option<String>,
    /// The sequence the read begins at; the event at it is included. At most
    /// one of this and `from_event` may be given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_sequence: Option<u64>,
    /// The key the reply is ordered by, with chain position first.
    #[serde(default)]
    pub order_by: TimelineQueryOrderBy,
    /// The end a page is taken from; ascending is the default.
    #[serde(default)]
    pub direction: TimelineQueryDirection,
    /// Whether the queried branch's own facts written above the published head
    /// belong to the read, up to the point the reply reports as `through`. An
    /// ancestor is read to its cut either way.
    #[serde(default)]
    pub include_publishing: bool,
    /// Event kinds (`domain/name`) the read accepts; empty accepts every kind.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub kinds: Vec<String>,
    /// The context flags the read accepts; empty constrains nothing, and a
    /// non-empty set accepts events carrying at least one named flag.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub flags: Vec<TimelineQueryFlag>,
    /// When named, only the event with this id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_id: Option<String>,
    /// One equality the event's data must satisfy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub predicate: Option<TimelineQueryPredicate>,
    /// How many matching events the page returns; nonzero by type.
    pub limit: NonZeroU64,
    /// How many matching events the read skips before the page.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub offset: u64,
}

fn is_zero(value: &u64) -> bool {
    *value == 0
}

impl TimelineQuery {
    /// Whether `event` satisfies the selection: the kinds, the flags, the
    /// event id and the one predicate. The scope, the start, the page and the
    /// direction belong to the read, not to a fact.
    pub fn accepts(&self, event: &AgentEvent) -> bool {
        (self.kinds.is_empty() || self.kinds.iter().any(|kind| kind == event.kind.key()))
            && (self.flags.is_empty()
                || self
                    .flags
                    .iter()
                    .any(|flag| flag.is_set(event.kind.context_flags())))
            && self
                .event_id
                .as_ref()
                .is_none_or(|wanted| wanted == &event.id)
            && self
                .predicate
                .as_ref()
                .is_none_or(|predicate| predicate.matches(event))
    }

    /// Checks the input a caller can get wrong before any storage read: the
    /// predicate's spelling and value. The read point is the store's to bound.
    pub fn validate(&self) -> Result<()> {
        if let Some(predicate) = &self.predicate {
            predicate.validate()?;
        }
        if self.from_event.is_some() && self.from_sequence.is_some() {
            return Err(AgentError::InvalidInput(
                "a read begins at an event id or a sequence, not both".into(),
            ));
        }
        if self.from_sequence == Some(0) {
            return Err(AgentError::InvalidInput(
                "timeline sequences start at 1".into(),
            ));
        }
        Ok(())
    }
}

/// The facts a query read, in chain order and then sequence, in the query's
/// direction, each with its position in the same order.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TimelineQueryReply {
    pub events: Vec<Arc<AgentEvent>>,
    /// One position per event, in the same order: the branch the event was
    /// read from and its sequence there.
    pub positions: Vec<TimelinePosition>,
    /// The sequence the read is cut at on the queried branch: what was
    /// published when it ran, or the committed head it asked for. No fact
    /// above it was read; an ancestor is read to its cut. A caller holding
    /// live events releases the ones above this sequence.
    pub through: u64,
}

/// What one branch's read returned: the page, and how many matching events it
/// passed over first, so a chain read can carry the remaining skip into the
/// next branch.
#[derive(Debug, Clone)]
pub struct TimelineQueryPage {
    pub events: Vec<Arc<AgentEvent>>,
    pub positions: Vec<TimelinePosition>,
    pub skipped: u64,
}

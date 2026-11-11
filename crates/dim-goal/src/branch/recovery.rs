use super::*;
use dim_agent_types::{ExtensionEvent, TimelinePosition};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub(crate) const KEY: &str = "goal/state";
const SCHEMA: &str = "goal-state/v1";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BusinessState {
    schema: String,
    pub(crate) operation: String,
    pub(crate) objective: Option<String>,
    pub(crate) state: text::State,
    pub(crate) judge: text::Judge,
    pub(crate) input_id: Option<String>,
}

impl BusinessState {
    pub(super) fn new(
        operation: &str,
        objective: Option<String>,
        state: text::State,
        judge: text::Judge,
        input_id: Option<String>,
    ) -> Self {
        Self {
            schema: SCHEMA.into(),
            operation: operation.into(),
            objective,
            state,
            judge,
            input_id,
        }
    }

    pub(crate) fn decode(data: Value) -> Result<Self, AgentError> {
        let value: Self = serde_json::from_value(data)
            .map_err(|error| AgentError::fault("Goal", error.to_string()))?;
        if value.schema != SCHEMA
            || !matches!(
                value.operation.as_str(),
                "set" | "edit" | "pause" | "resume" | "clear" | "completed" | "blocked"
            )
            || value
                .objective
                .as_ref()
                .is_some_and(|objective| objective.is_empty() || objective.chars().count() > 4000)
            || value.input_id.as_ref().is_some_and(String::is_empty)
        {
            return Err(AgentError::fault("Goal", "invalid business state"));
        }
        Ok(value)
    }

    pub(super) fn draft(&self) -> Result<dim_agent_types::EventDraft, AgentError> {
        let data = serde_json::to_value(self)
            .map_err(|error| AgentError::fault("Goal", error.to_string()))?;
        Ok(EventKind::Extension(
            ExtensionEvent::new("goal", "state", data).expect("Goal business state key"),
        )
        .into())
    }

    pub(crate) fn apply(&self, fold: &mut Fold, replay: bool) {
        if !replay || matches!(self.operation.as_str(), "set" | "clear") {
            fold.objective.clone_from(&self.objective);
            fold.state.clone_from(&self.state);
            fold.judge = self.judge;
        } else {
            match self.operation.as_str() {
                "edit" => fold.objective.clone_from(&self.objective),
                "resume" if fold.objective.is_some() => fold.state = text::State::Active,
                "pause" | "completed" | "blocked" if fold.objective.is_some() => {
                    fold.state.clone_from(&self.state)
                }
                _ => (),
            }
        }
        fold.local_blocks = 1;
        fold.live.state = self.objective.is_some();
    }
}

pub(crate) fn request(
    branch: &str,
    from: Option<u64>,
    through: u64,
    kinds: &[&str],
    fields: &[&str],
) -> TimelineQuery {
    TimelineQuery {
        scope: TimelineQueryScope::Branch,
        from: from.map(|sequence| TimelinePosition {
            branch_id: branch.into(),
            sequence,
        }),
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

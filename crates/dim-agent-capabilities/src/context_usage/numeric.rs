use super::{
    Calibration, Compression, Configuration, Contribution, Key, LastEntry, RESULT, add, fault,
    request,
};
use dim_agent_runtime::{
    QueryComparison, QueryFilter, QueryOperand, QueryOrder, QueryOrderKey, QueryOutput, QueryValue,
    Result, Timeline, TimelineQueryDirection, TimelineQueryScope,
};
use serde::{Deserialize, Serialize};
use std::{num::NonZeroU64, sync::Arc};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Summary {
    pub ordinary: Contribution,
    #[serde(default)]
    pub message_count: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub configuration: Option<Configuration>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compression: Option<Compression>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub calibration: Option<Calibration>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_entry: Option<LastEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub growth: Option<f64>,
}

impl Default for Summary {
    fn default() -> Self {
        Self {
            ordinary: Contribution::default(),
            message_count: Some(0),
            configuration: None,
            compression: None,
            calibration: None,
            local: None,
            last_entry: None,
            growth: None,
        }
    }
}

impl Summary {
    pub fn join(&mut self, next: &Self) -> Result<()> {
        self.growth = match (self.growth, next.growth) {
            (Some(left), Some(right)) => Some(left + right),
            (Some(left), None) if next.ordinary == Contribution::default() => Some(left),
            (None, Some(right)) if self.ordinary == Contribution::default() => Some(right),
            _ => None,
        };
        if self
            .growth
            .is_some_and(|growth| !growth.is_finite() || growth < 0.)
        {
            return Err(fault("invalid numeric history growth"));
        }
        self.ordinary = add(self.ordinary, next.ordinary)?;
        self.message_count = self
            .message_count
            .zip(next.message_count)
            .map(|(left, right)| {
                left.checked_add(right)
                    .ok_or_else(|| fault("message count overflow"))
            })
            .transpose()?;
        if next.configuration.is_some() {
            self.configuration.clone_from(&next.configuration);
            self.calibration = None;
        }
        if next.compression.is_some() {
            self.compression.clone_from(&next.compression);
            self.calibration = None;
        }
        if next.calibration.is_some() {
            self.calibration.clone_from(&next.calibration);
        }
        if next.local.is_some() {
            self.local = next.local;
        }
        if next.last_entry.is_some() {
            self.last_entry.clone_from(&next.last_entry);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Entry {
    pub at: Key,
    pub summary: Summary,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct NumericResult {
    pub at: Key,
    pub totals: Summary,
}

pub(super) struct Numeric {
    timeline: Arc<Timeline>,
    through: u64,
    epoch: String,
    legacy_result: Option<String>,
    initialized: Key,
    pending: Vec<NumericResult>,
}

impl Numeric {
    pub fn new(
        timeline: Arc<Timeline>,
        through: u64,
        epoch: String,
        legacy_result: Option<String>,
        initialized: Key,
    ) -> Self {
        Self {
            timeline,
            through,
            epoch,
            legacy_result,
            initialized,
            pending: Vec::new(),
        }
    }

    pub fn published(&mut self, through: u64) {
        self.through = through;
    }

    pub async fn validate_current(&self, current: Option<&NumericResult>) -> Result<()> {
        if let Some(current) = current {
            if self.legacy_result.is_none()
                && (current.totals.message_count.is_none()
                    || current.totals.compression.as_ref().is_some_and(|value| {
                        value.removed_messages.is_none() || value.replacement_messages.is_none()
                    })
                    || matches!(&current.totals.configuration, Some(Configuration::Ready { counter }) if counter.control_identity.is_none() || counter.configured_tools.is_none()))
            {
                return Err(fault("missing native numeric counting metadata"));
            }
            if current.totals.last_entry.as_ref().is_some_and(|entry| {
                entry.at > current.at || entry.id.as_ref().is_some_and(String::is_empty)
            }) || current
                .totals
                .growth
                .is_some_and(|value| !value.is_finite() || value < 0.)
            {
                return Err(fault("invalid numeric result"));
            }
            super::total(&current.totals)?;
        }
        Ok(())
    }

    pub async fn append(
        &mut self,
        current: Option<NumericResult>,
        entry: Entry,
    ) -> Result<NumericResult> {
        if current
            .as_ref()
            .is_some_and(|current| current.at > entry.at)
        {
            return Err(fault("numeric input precedes the saved prefix"));
        }
        let mut summary = current.map_or_else(Summary::default, |current| current.totals);
        summary.join(&entry.summary)?;
        let reference = NumericResult {
            at: entry.at,
            totals: summary,
        };
        if self
            .pending
            .last()
            .is_some_and(|previous| previous.at == entry.at)
        {
            self.pending.pop();
        }
        self.pending.push(reference.clone());
        Ok(reference)
    }

    pub async fn prefix(
        &self,
        current: Option<&NumericResult>,
        end: Key,
        inclusive: bool,
    ) -> Result<Option<Summary>> {
        let before = |at| at < end || (inclusive && at == end);
        if let Some(current) = current.filter(|current| before(current.at)) {
            return Ok(Some(current.totals.clone()));
        }
        if let Some(reference) = self
            .pending
            .iter()
            .rev()
            .find(|reference| before(reference.at))
        {
            return Ok(Some(reference.totals.clone()));
        }
        for branch in (0..=end.branch).rev() {
            let mut query = request(
                TimelineQueryScope::Chain,
                self.through,
                &[RESULT],
                &["data.current.totals"],
            );
            let equal = |path: &str, value| QueryFilter::Compare {
                left: QueryOperand::Field { path: path.into() },
                comparison: QueryComparison::Eq,
                right: QueryOperand::Value { value },
            };
            let mut conditions = vec![
                query.query.filter.take().expect("numeric result kind"),
                equal("data.epoch", self.epoch.clone().into()),
                equal("data.current.at.branch", branch.into()),
            ];
            if branch == end.branch {
                conditions.push(QueryFilter::Compare {
                    left: QueryOperand::Field {
                        path: "data.current.at.sequence".into(),
                    },
                    comparison: if inclusive {
                        QueryComparison::Le
                    } else {
                        QueryComparison::Lt
                    },
                    right: QueryOperand::Value {
                        value: end.sequence.into(),
                    },
                });
            }
            query.query.filter = Some(QueryFilter::And { conditions });
            query.query.order_by = vec![
                QueryOrder {
                    key: QueryOrderKey::Field {
                        path: "data.current.at.sequence".into(),
                    },
                    direction: TimelineQueryDirection::Descending,
                },
                QueryOrder {
                    key: QueryOrderKey::Position,
                    direction: TimelineQueryDirection::Descending,
                },
            ];
            query.page.as_mut().expect("numeric result page").limit = NonZeroU64::MIN;
            let QueryOutput::Rows { mut rows } = self.timeline.query(query).await?.result else {
                unreachable!("numeric fields")
            };
            if let Some(row) = rows.pop() {
                let [QueryValue::Value { value }] =
                    <[_; 1]>::try_from(row.values).expect("numeric summary field")
                else {
                    return Err(fault("missing numeric summary"));
                };
                return serde_json::from_value(value)
                    .map(Some)
                    .map_err(|error| fault(error.to_string()));
            }
        }
        match &self.legacy_result {
            Some(id) => super::legacy::prefix(&self.timeline, self.through, id, end, inclusive)
                .await
                .map(Some),
            None if before(self.initialized) => Ok(Some(Summary::default())),
            None => Ok(None),
        }
    }

    pub async fn growth(
        &self,
        current: Option<&NumericResult>,
        after: Key,
        through: Key,
    ) -> Result<Option<f64>> {
        if after >= through {
            return Ok(Some(0.));
        }
        let Some(before) = self.prefix(current, after, true).await? else {
            return Ok(None);
        };
        let Some(end) = self.prefix(current, through, true).await? else {
            return Ok(None);
        };
        let value = |summary: Summary| {
            summary
                .growth
                .or_else(|| (summary.message_count == Some(0)).then_some(0.))
        };
        Ok(value(end)
            .zip(value(before))
            .map(|(end, before)| end - before))
    }

    pub fn take_pending(&mut self) -> Vec<NumericResult> {
        std::mem::take(&mut self.pending)
    }
}

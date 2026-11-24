use super::*;

/// The newest `context/configured` on a line's ancestor chain at `through`,
/// each ancestor bounded by its inherited cut. `None` reads the line's head.
/// Prompt's recovery read and the adopted-configuration read share this shape,
/// so the two cannot disagree about what a line adopted.
pub(crate) fn configuration_query(through: Option<u64>) -> TimelineQuery {
    TimelineQuery {
        scope: TimelineQueryScope::Chain,
        from: None,
        through,
        include_publishing: false,
        query: QuerySpec {
            filter: Some(QueryFilter::In {
                value: QueryOperand::Field {
                    path: "kind".into(),
                },
                values: vec![serde_json::json!("context/configured")],
            }),
            select: QuerySelect::Fields {
                fields: vec!["data".into()],
            },
            group_by: vec![],
            pick: Some(QueryPick::Last),
            order_by: vec![],
        },
        page: Some(QueryPage {
            limit: std::num::NonZeroU64::MIN,
            after: None,
        }),
    }
}

pub struct DirectoryDisplay {
    pub entry: BranchDirectoryEntry,
    pub purpose: Option<String>,
    pub cut_messages: Option<u64>,
    pub end: Option<BranchEndData>,
}

impl TreeCommands {
    /// Reads one branch's creation purpose/source and end detail.
    /// Unknown branches and failed history reads return errors.
    pub async fn display(&self, id: String) -> Result<DirectoryDisplay> {
        let tree = self
            .tree
            .upgrade()
            .ok_or_else(|| AgentError::Closed("TimelineTree".into()))?;
        let mut entry = tree
            .directory()
            .into_iter()
            .find(|entry| entry.info.id == id)
            .ok_or(AgentError::UnknownBranch(id))?;
        let mut purpose = None;
        if let (Some(parent), Some(anchor)) = (&entry.info.parent, entry.info.create_anchor) {
            let point = self
                .query(
                    parent.clone(),
                    TimelineQuery {
                        scope: dim_agent_runtime::TimelineQueryScope::Branch,
                        from: Some(dim_agent_types::TimelinePosition {
                            branch_id: parent.clone(),
                            sequence: anchor,
                        }),
                        through: Some(anchor),
                        include_publishing: false,
                        query: QuerySpec {
                            filter: None,
                            select: QuerySelect::Fields {
                                fields: vec!["data.prompt".into(), "data.source".into()],
                            },
                            group_by: vec![],
                            pick: None,
                            order_by: vec![],
                        },
                        page: Some(QueryPage {
                            limit: std::num::NonZeroU64::MIN,
                            after: None,
                        }),
                    },
                )
                .await?;
            let QueryOutput::Rows { rows } = point.result else {
                unreachable!("branch purpose")
            };
            purpose = rows
                .first()
                .and_then(|row| match &row.values[0] {
                    QueryValue::Value { value } => value.as_str(),
                    _ => None,
                })
                .map(|text| text.lines().next().unwrap_or_default().trim().to_owned());
            if let Some(row) = rows.first()
                && let QueryValue::Value { value: source } = &row.values[1]
            {
                entry.info.source =
                    Some(serde_json::from_value(source.clone()).expect("branch source contract"));
            }
        }
        let end = if let Some(at) = entry.end_event.filter(|at| *at <= entry.head) {
            let result = self
                .query(
                    entry.info.id.clone(),
                    TimelineQuery {
                        scope: dim_agent_runtime::TimelineQueryScope::Branch,
                        from: Some(dim_agent_types::TimelinePosition {
                            branch_id: entry.info.id.clone(),
                            sequence: at,
                        }),
                        through: Some(at),
                        include_publishing: false,
                        query: QuerySpec {
                            filter: None,
                            select: QuerySelect::Fields {
                                fields: vec!["data".into()],
                            },
                            group_by: vec![],
                            pick: None,
                            order_by: vec![],
                        },
                        page: Some(QueryPage {
                            limit: std::num::NonZeroU64::MIN,
                            after: None,
                        }),
                    },
                )
                .await?;
            let QueryOutput::Rows { rows } = result.result else {
                unreachable!("branch end")
            };
            rows.first().map(|row| {
                let QueryValue::Value { value } = &row.values[0] else {
                    unreachable!("branch end data")
                };
                serde_json::from_value(value.clone()).expect("branch end contract")
            })
        } else {
            None
        };
        Ok(DirectoryDisplay {
            entry,
            purpose,
            cut_messages: None,
            end,
        })
    }

    /// Whether `id`'s own `branch/end` is committed: the line is ending or
    /// ended, so its facts answer at the cutoff that fact left even while its
    /// scope is closing. An active line answers `false`; an unknown line is an
    /// error.
    pub fn end_committed(&self, id: &str) -> Result<bool> {
        let tree = self
            .tree
            .upgrade()
            .ok_or_else(|| AgentError::Closed("TimelineTree".into()))?;
        let state = tree.state.lock().unwrap();
        if state.ended.contains_key(id) {
            return Ok(true);
        }
        match state.nodes.get(id) {
            Some(node) => Ok(node
                .context
                .as_ref()
                .is_some_and(|context| context.end_event().is_some())),
            None => Err(AgentError::UnknownBranch(id.into())),
        }
    }

    /// The configuration `id` adopted at `through`, or at its head when
    /// omitted, with that cutoff. The cutoff belongs to `id` alone: a value an
    /// ancestor adopted is reported at `id`'s own head, never at the ancestor's
    /// sequence. An ended line reads at its fixed head and is not reopened, and
    /// an unknown line is an error.
    pub async fn adopted_configuration(
        &self,
        id: String,
        through: Option<u64>,
    ) -> Result<(Option<ModelConfiguration>, u64)> {
        let reply = self.query(id, configuration_query(through)).await?;
        let QueryOutput::Rows { mut rows } = reply.result else {
            unreachable!("configuration selects rows")
        };
        let configuration = match rows.pop() {
            Some(row) => {
                let [QueryValue::Value { value }] =
                    <[_; 1]>::try_from(row.values).expect("configuration field")
                else {
                    return Err(AgentError::fault(
                        "TimelineTree",
                        "missing stored configuration field",
                    ));
                };
                let data: ContextConfiguredData = serde_json::from_value(value)
                    .map_err(|error| AgentError::fault("TimelineTree", error.to_string()))?;
                Some((*data.settings).clone())
            }
            None => None,
        };
        Ok((configuration, reply.through))
    }
}

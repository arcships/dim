use super::*;

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
}

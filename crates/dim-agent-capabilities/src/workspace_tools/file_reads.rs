//! Branch-level record of the text files this branch has read.
//!
//! A binding is what the model has seen of a file on this branch: its raw
//! byte revision and the text format `read` resolved (ADR agent-019).
//! Overwriting an existing file requires a binding from this branch whose
//! operation is still in the model's context (#524).

mod face;

use crate::host::HostError;
use dim_agent_runtime::{
    Names, QueryComparison, QueryFilter, QueryOperand, QueryOutput, QueryPage, QueryPick,
    QuerySelect, QuerySpec, QueryValue, Timeline, TimelineQuery, TimelineQueryScope,
};
use dim_agent_types::TimelinePosition;
use rutis::Ctx;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, Key, TypeKey};
use std::{
    collections::HashMap,
    num::NonZeroU64,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub enum TextEncoding {
    Utf8,
    Utf16Le,
    Utf16Be,
    /// Only established by an explicit `read.encoding`; never guessed.
    #[serde(deserialize_with = "super::files::deserialize_legacy_encoding")]
    Legacy {
        /// The requested name, lowercased, as echoed to the model.
        name: String,
        #[serde(skip_serializing)]
        encoding: &'static encoding_rs::Encoding,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EncodingSource {
    Bom,
    User,
    Utf8Policy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Bom {
    None,
    Utf8,
    Utf16Le,
    Utf16Be,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Eol {
    None,
    Lf,
    Crlf,
    Mixed,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TextFileBinding {
    /// SHA-256 of the raw file bytes.
    pub raw_revision: [u8; 32],
    pub encoding: TextEncoding,
    pub encoding_source: EncodingSource,
    pub bom: Bom,
    pub eol: Eol,
}

/// A binding and the sequence the branch had reached when the operation that
/// established it ran.
#[derive(Debug, Clone)]
struct Binding {
    text: TextFileBinding,
    established: u64,
}

/// What `write` may replace an existing file with.
#[derive(Debug, serde::Serialize)]
#[serde(tag = "kind", content = "binding", rename_all = "lowercase")]
pub enum WriteBinding {
    /// No operation on this branch bound the file.
    #[serde(rename = "absent")]
    None,
    /// The operation that bound the file was compacted out of the context.
    Compacted,
    /// The binding of an operation still in the model's context.
    #[serde(rename = "bound")]
    Text(TextFileBinding),
}

/// Bindings keyed by canonical path; lives as long as the branch instance.
pub struct FileReads {
    timeline: Arc<Timeline>,
    bindings: Mutex<HashMap<PathBuf, Binding>>,
}

impl FileReads {
    pub fn new(timeline: Arc<Timeline>) -> Self {
        Self {
            timeline,
            bindings: Mutex::new(HashMap::new()),
        }
    }
    /// The binding `edit` may use: it checks the model's `oldText` against the
    /// file itself, so a compaction does not take it away (#524).
    pub fn binding(&self, path: &Path) -> Option<TextFileBinding> {
        self.bindings
            .lock()
            .unwrap()
            .get(path)
            .map(|binding| binding.text.clone())
    }
    /// The branch's last sequence: the operation starting now is cut out by
    /// every later compaction whose `retain_from` is a later event.
    pub(crate) fn established(&self) -> u64 {
        self.timeline.committed()
    }
    /// The binding `write` may use: only while the operation that established
    /// it is still in the model's context.
    pub fn write_binding(
        &self,
        path: &Path,
    ) -> impl Future<Output = Result<WriteBinding, HostError>> + Send + 'static {
        let binding = self.bindings.lock().unwrap().get(path).cloned();
        let timeline = self.timeline.clone();
        async move {
            let Some(binding) = binding else {
                return Ok(WriteBinding::None);
            };
            Ok(if Self::in_context(&timeline, binding.established).await? {
                WriteBinding::Text(binding.text)
            } else {
                WriteBinding::Compacted
            })
        }
    }
    pub fn record(&self, path: PathBuf, text: TextFileBinding, established: u64) {
        self.bindings
            .lock()
            .unwrap()
            .insert(path, Binding { text, established });
    }
    /// Whether no completed `context/compaction` on this branch cut at or after
    /// the operation that ran when the branch reached `established`.
    ///
    /// `established` is the branch's last sequence when the operation started,
    /// after its tool call committed, and no legal cut separates a tool call
    /// from its result. So a `retain_from` in this range lies after the
    /// operation's result, and one before it keeps the result (#524). The
    /// compactions this check folds are published before it runs: the model
    /// call that led to the write tool was written after them, and a batch is
    /// published only after every batch committed before it.
    async fn in_context(timeline: &Timeline, established: u64) -> Result<bool, HostError> {
        let now = timeline.sequence();
        if now <= established {
            return Ok(true);
        }
        let completed = timeline
            .query(TimelineQuery {
                scope: TimelineQueryScope::Branch,
                from: Some(TimelinePosition {
                    branch_id: timeline.branch_id(),
                    sequence: established + 1,
                }),
                through: Some(now),
                include_publishing: false,
                query: QuerySpec {
                    filter: Some(QueryFilter::And {
                        conditions: vec![
                            QueryFilter::In {
                                value: QueryOperand::Field {
                                    path: "kind".into(),
                                },
                                values: vec!["context/compaction".into()],
                            },
                            QueryFilter::In {
                                value: QueryOperand::Field {
                                    path: "data.status".into(),
                                },
                                values: vec!["completed".into()],
                            },
                        ],
                    }),
                    select: QuerySelect::Fields {
                        fields: vec!["data.retain_from".into()],
                    },
                    group_by: vec![],
                    pick: Some(QueryPick::Last),
                    order_by: vec![],
                },
                page: Some(QueryPage {
                    limit: NonZeroU64::MIN,
                    after: None,
                }),
            })
            .await
            .map_err(|error| {
                HostError::failed(format!("Failed to read the branch timeline: {error}"))
            })?;
        let QueryOutput::Rows { rows } = completed.result else {
            unreachable!("field selection returns rows")
        };
        let Some(row) = rows.into_iter().next() else {
            return Ok(true);
        };
        let [QueryValue::Value { value: retain_from }] = row.values.as_slice() else {
            unreachable!("completed compaction has a retain_from")
        };
        let cut = timeline
            .query(TimelineQuery {
                scope: TimelineQueryScope::Branch,
                from: None,
                through: Some(now),
                include_publishing: false,
                query: QuerySpec {
                    filter: Some(QueryFilter::And {
                        conditions: vec![
                            QueryFilter::In {
                                value: QueryOperand::Field { path: "id".into() },
                                values: vec![retain_from.clone()],
                            },
                            QueryFilter::Compare {
                                left: QueryOperand::Field {
                                    path: "position.sequence".into(),
                                },
                                comparison: QueryComparison::Gt,
                                right: QueryOperand::Value {
                                    value: established.into(),
                                },
                            },
                        ],
                    }),
                    select: QuerySelect::Exists,
                    group_by: vec![],
                    pick: None,
                    order_by: vec![],
                },
                page: None,
            })
            .await
            .map_err(|error| {
                HostError::failed(format!("Failed to read the branch timeline: {error}"))
            })?;
        let QueryOutput::Exists { exists } = cut.result else {
            unreachable!("existence selection returns a boolean")
        };
        Ok(!exists)
    }
}

pub struct FileReadsPlugin {
    instance: rutis::host::InstanceId,
    reads: Key<FileReads>,
    timeline: Key<Timeline>,
}
impl FileReadsPlugin {
    pub fn new(names: &Names<'_>) -> rutis::Result<Self> {
        Ok(Self {
            instance: names.instance(dim_agent_runtime::BRANCH_GROUP)?,
            reads: names.key()?,
            timeline: names.key()?,
        })
    }
}
impl DynPlugin for FileReadsPlugin {
    fn name(&self) -> &str {
        "FileReads"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.timeline.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let timeline = ctx.require_at(&self.timeline)?;
            let owner = Arc::new(FileReads::new(timeline));
            ctx.provide_at::<dyn rutis_bridge::session::HostDispatch>(
                rutis_bridge::session::host_key_in(
                    &dim_agent_runtime::names::name_of::<FileReads>(),
                    self.instance,
                ),
                owner.clone(),
            )?;
            ctx.provide_at(self.reads.clone(), owner)?;
            Ok(Effect::Done)
        })
    }
}

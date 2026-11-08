//! Branch-level record of the text files this branch has read.
//!
//! A binding is what the model has seen of a file on this branch: its raw
//! byte revision and the text format `read` resolved (ADR agent-019).
//! Overwriting an existing file requires a binding from this branch whose
//! operation is still in the model's context (#524).

use crate::{history::chain_by_id, host::HostError};
use dim_agent_runtime::{
    Names, Timeline, TimelineQuery, TimelineQueryDirection, TimelineQueryOrderBy,
    TimelineQueryScope,
};
use dim_agent_types::{ContextCompactionResult, EventKind};
use rutis::Ctx;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, Key, TypeKey};
use std::{
    collections::HashMap,
    num::NonZeroU64,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

/// How many compaction facts one page of the check reads.
const COMPACTION_PAGE: u64 = 8;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TextEncoding {
    Utf8,
    Utf16Le,
    Utf16Be,
    /// Only established by an explicit `read.encoding`; never guessed.
    Legacy {
        /// The requested name, lowercased, as echoed to the model.
        name: String,
        encoding: &'static encoding_rs::Encoding,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncodingSource {
    Bom,
    User,
    Utf8Policy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bom {
    None,
    Utf8,
    Utf16Le,
    Utf16Be,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Eol {
    None,
    Lf,
    Crlf,
    Mixed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
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
#[derive(Debug)]
pub(crate) enum WriteBinding {
    /// No operation on this branch bound the file.
    None,
    /// The operation that bound the file was compacted out of the context.
    Compacted,
    /// The binding of an operation still in the model's context.
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
    pub(crate) fn get(&self, path: &Path) -> Option<TextFileBinding> {
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
    pub(crate) async fn write_binding(&self, path: &Path) -> Result<WriteBinding, HostError> {
        let Some(binding) = self.bindings.lock().unwrap().get(path).cloned() else {
            return Ok(WriteBinding::None);
        };
        Ok(if self.in_context(binding.established).await? {
            WriteBinding::Text(binding.text)
        } else {
            WriteBinding::Compacted
        })
    }
    pub(crate) fn set(&self, path: PathBuf, text: TextFileBinding, established: u64) {
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
    async fn in_context(&self, established: u64) -> Result<bool, HostError> {
        let now = self.timeline.sequence();
        if now <= established {
            return Ok(true);
        }
        let Some(retain_from) = self.latest_completed(established).await? else {
            return Ok(true);
        };
        let cut = self
            .timeline
            .query(chain_by_id(&retain_from))
            .await
            .map_err(|error| {
                HostError::failed(format!("Failed to read the branch timeline: {error}"))
            })?
            .positions
            .into_iter()
            .next();
        Ok(cut.is_none_or(|cut| {
            cut.branch_id != self.timeline.branch_id() || cut.sequence <= established
        }))
    }
    /// The newest completed compaction on this branch above `established`,
    /// read newest first: a compaction that completed earlier always cuts
    /// before a later one, so the newest completed one decides whether the
    /// operation survived.
    async fn latest_completed(&self, established: u64) -> Result<Option<String>, HostError> {
        let mut from_sequence = None;
        loop {
            let reply = self
                .timeline
                .query(TimelineQuery {
                    scope: TimelineQueryScope::Branch,
                    include_publishing: false,
                    from_event: None,
                    from_sequence,
                    order_by: TimelineQueryOrderBy::Sequence,
                    direction: TimelineQueryDirection::Descending,
                    kinds: vec!["context/compaction".into()],
                    flags: Vec::new(),
                    event_id: None,
                    predicate: None,
                    limit: NonZeroU64::new(COMPACTION_PAGE)
                        .expect("a page keeps at least one compaction"),
                    offset: 0,
                })
                .await
                .map_err(|error| {
                    HostError::failed(format!("Failed to read the branch timeline: {error}"))
                })?;
            for event in &reply.events {
                if event.sequence <= established {
                    return Ok(None);
                }
                if let EventKind::ContextCompaction(data) = &event.kind
                    && let ContextCompactionResult::Completed { retain_from, .. } = &data.outcome
                {
                    return Ok(Some(retain_from.clone()));
                }
            }
            let Some(last) = reply.events.last().map(|event| event.sequence) else {
                return Ok(None);
            };
            if (reply.events.len() as u64) < COMPACTION_PAGE || last <= 1 {
                return Ok(None);
            }
            from_sequence = Some(last - 1);
        }
    }
}

pub struct FileReadsPlugin {
    reads: Key<FileReads>,
    timeline: Key<Timeline>,
}
impl FileReadsPlugin {
    pub fn new(names: &Names<'_>) -> rutis::Result<Self> {
        Ok(Self {
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
            ctx.provide_at(self.reads.clone(), Arc::new(FileReads::new(timeline)))?;
            Ok(Effect::Done)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dim_agent_runtime::{MemoryTimelineStore, SessionConfig, TimelineKey, TimelineStore};
    use dim_agent_types::{
        CompactionDiagnosticsRecord, CompactionReason, CompactionStrategyRecord, CompactionTrigger,
        ContextCompactionData,
    };

    async fn timeline() -> Arc<Timeline> {
        let store = Arc::new(MemoryTimelineStore::default());
        store
            .create_session("file-reads", &SessionConfig::default())
            .await
            .expect("the session");
        Arc::new(Timeline::new(
            Ctx::root().unwrap(),
            TimelineKey {
                session_id: "file-reads".into(),
                branch_id: "main".into(),
            },
            Vec::new(),
            store,
        ))
    }

    fn completed(retain_from: &str) -> EventKind {
        EventKind::ContextCompaction(ContextCompactionData {
            turn_id: None,
            request_id: None,
            command: None,
            start: None,
            through: 0,
            outcome: ContextCompactionResult::Completed {
                retain_from: retain_from.into(),
                replacement: Vec::new(),
                strategy: "test".into(),
                diagnostics: Box::new(CompactionDiagnosticsRecord {
                    reason: CompactionReason::Auto,
                    trigger: CompactionTrigger::Threshold,
                    retain_from: None,
                    retain_limit: None,
                    estimated_input_tokens: 0.0,
                    estimated_output_tokens: None,
                    entries_replaced: 0,
                    model_call_count: 0,
                    duration_ms: 0,
                    model_usage: None,
                    insufficient_space: None,
                    strategy: CompactionStrategyRecord {
                        kind: "test".into(),
                        details: serde_json::Map::new(),
                    },
                }),
                context_baseline: Vec::new(),
                replacement_parts: None,
                pinned_inputs: None,
                digest_entries: None,
            },
        })
    }

    fn cancelled() -> EventKind {
        EventKind::ContextCompaction(ContextCompactionData {
            turn_id: None,
            request_id: None,
            command: None,
            start: None,
            through: 0,
            outcome: ContextCompactionResult::Cancelled {
                reason: "cancelled".into(),
            },
        })
    }

    #[tokio::test]
    async fn the_guard_pages_to_a_completed_compaction_beyond_one_page() {
        let timeline = timeline().await;
        timeline
            .append(completed("retained").into())
            .expect("the completed compaction");
        for _ in 0..COMPACTION_PAGE {
            timeline
                .append(cancelled().into())
                .expect("a cancelled compaction");
        }
        let reads = FileReads::new(timeline);
        assert_eq!(
            reads.latest_completed(0).await.unwrap(),
            Some("retained".to_owned())
        );
    }

    #[tokio::test]
    async fn the_guard_stops_at_sequence_one_before_asking_for_zero() {
        let timeline = timeline().await;
        for _ in 0..COMPACTION_PAGE {
            timeline
                .append(cancelled().into())
                .expect("a cancelled compaction");
        }
        let reads = FileReads::new(timeline);
        assert_eq!(reads.latest_completed(0).await.unwrap(), None);
    }
}

use dim_agent_types::EventKind;
use std::sync::Arc;

/// A real TimelineStore port whose I/O worker can pause an accepted input batch.
/// All callbacks stay ordered; the test controls I/O rather than runtime hooks.
pub(super) struct DeferredInputs {
    inner: Arc<dim_agent_runtime::MemoryTimelineStore>,
    writes: tokio::sync::mpsc::UnboundedSender<(
        dim_agent_runtime::TimelineKey,
        Vec<dim_agent_runtime::StoredEvent>,
        dim_agent_runtime::PublishCallback,
    )>,
    worker: tokio::task::JoinHandle<()>,
}
pub(super) struct HeldInput {
    pub(super) submission_id: String,
    pub(super) sequence: u64,
    pub(super) release: tokio::sync::oneshot::Sender<()>,
}
impl DeferredInputs {
    pub(super) fn new() -> (Arc<Self>, tokio::sync::mpsc::UnboundedReceiver<HeldInput>) {
        use dim_agent_runtime::{MemoryTimelineStore, TimelineStore};
        let inner = Arc::new(MemoryTimelineStore::default());
        let backend = inner.clone();
        let (writes, mut pending) = tokio::sync::mpsc::unbounded_channel::<(
            dim_agent_runtime::TimelineKey,
            Vec<dim_agent_runtime::StoredEvent>,
            dim_agent_runtime::PublishCallback,
        )>();
        let (held, receive) = tokio::sync::mpsc::unbounded_channel();
        let worker = tokio::spawn(async move {
            while let Some((key, events, publish)) = pending.recv().await {
                let marked = events.iter().find_map(|record| match &record.event.kind {
                    EventKind::InputMessage(data) => data
                        .submission_id
                        .as_ref()
                        .filter(|id| id.starts_with("settlement:"))
                        .map(|id| (id.clone(), record.event.sequence)),
                    _ => None,
                });
                let after_commit = marked
                    .as_ref()
                    .is_some_and(|(id, _)| id.ends_with(":publication"));
                let mut pause = marked.map(|(submission_id, sequence)| {
                    let (release, wait) = tokio::sync::oneshot::channel();
                    (
                        HeldInput {
                            submission_id,
                            sequence,
                            release,
                        },
                        wait,
                    )
                });
                if !after_commit && let Some((notice, wait)) = pause.take() {
                    held.send(notice).unwrap();
                    let _ = wait.await;
                }
                let (sent, committed) = tokio::sync::oneshot::channel();
                let outcome = match backend.append(
                    key,
                    events,
                    Box::new(move |result| {
                        let _ = sent.send(result);
                    }),
                ) {
                    Ok(()) => committed.await.unwrap(),
                    Err(error) => Err(error),
                };
                if let Some((notice, wait)) = pause {
                    held.send(notice).unwrap();
                    let _ = wait.await;
                }
                publish(outcome);
            }
        });
        (
            Arc::new(Self {
                inner,
                writes,
                worker,
            }),
            receive,
        )
    }
}
impl Drop for DeferredInputs {
    fn drop(&mut self) {
        self.worker.abort();
    }
}
impl dim_agent_runtime::TimelineStore for DeferredInputs {
    fn create_session<'a>(
        &'a self,
        id: &'a str,
        config: &'a dim_agent_runtime::SessionConfig,
    ) -> dim_agent_runtime::BoxFuture<'a, dim_agent_runtime::Result<()>> {
        self.inner.create_session(id, config)
    }
    fn rollback_session<'a>(
        &'a self,
        id: &'a str,
    ) -> dim_agent_runtime::BoxFuture<'a, dim_agent_runtime::Result<()>> {
        self.inner.rollback_session(id)
    }
    fn release_session<'a>(
        &'a self,
        id: &'a str,
    ) -> dim_agent_runtime::BoxFuture<'a, dim_agent_runtime::Result<()>> {
        self.inner.release_session(id)
    }
    fn session_config<'a>(
        &'a self,
        id: &'a str,
    ) -> dim_agent_runtime::BoxFuture<
        'a,
        dim_agent_runtime::Result<Option<dim_agent_runtime::SessionConfig>>,
    > {
        self.inner.session_config(id)
    }
    fn branches<'a>(
        &'a self,
        id: &'a str,
    ) -> dim_agent_runtime::BoxFuture<
        'a,
        dim_agent_runtime::Result<Vec<dim_agent_runtime::StoredBranch>>,
    > {
        self.inner.branches(id)
    }
    fn head<'a>(
        &'a self,
        key: &'a dim_agent_runtime::TimelineKey,
    ) -> dim_agent_runtime::BoxFuture<'a, dim_agent_runtime::Result<u64>> {
        self.inner.head(key)
    }
    fn append(
        &self,
        key: dim_agent_runtime::TimelineKey,
        events: Vec<dim_agent_runtime::StoredEvent>,
        publish: dim_agent_runtime::PublishCallback,
    ) -> dim_agent_runtime::Result<()> {
        self.writes.send((key, events, publish)).map_err(|_| {
            dim_agent_runtime::AgentError::Storage("deferred input worker is closed".into())
        })
    }
    fn read_segments<'a>(
        &'a self,
        id: &'a str,
        segments: Vec<(String, std::ops::RangeInclusive<u64>)>,
    ) -> dim_agent_runtime::BoxFuture<
        'a,
        dim_agent_runtime::Result<Vec<Vec<dim_agent_runtime::StoredEvent>>>,
    > {
        self.inner.read_segments(id, segments)
    }
    fn trace<'a>(
        &'a self,
        key: &'a dim_agent_runtime::TimelineKey,
        sequence: u64,
    ) -> dim_agent_runtime::BoxFuture<
        'a,
        dim_agent_runtime::Result<Option<dim_agent_runtime::TimelineTrace>>,
    > {
        self.inner.trace(key, sequence)
    }
    fn query<'a>(
        &'a self,
        key: &'a dim_agent_runtime::TimelineKey,
        query: &'a dim_agent_runtime::TimelineQuery,
        from: u64,
        through: u64,
    ) -> dim_agent_runtime::BoxFuture<
        'a,
        dim_agent_runtime::Result<dim_agent_runtime::TimelineQueryPage>,
    > {
        self.inner.query(key, query, from, through)
    }
}

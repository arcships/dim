use crate::*;
use dim_agent_types::{AgentEvent, InputMessageData};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::watch;

/// Facade view of one branch, read on query: Loop's execution phase plus
/// branch lifecycle.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct BranchStatus {
    pub running: bool,
    pub stopping: bool,
    pub closed: bool,
    pub failure: Option<String>,
}

/// Read-only publication progress for durable receipt and lifecycle observation.
#[derive(Clone)]
pub struct Publication(pub(crate) Timeline);
impl Publication {
    pub fn sequence(&self) -> u64 {
        self.0.sequence()
    }
    pub fn watch(&self) -> watch::Receiver<(u64, bool)> {
        self.0.watch()
    }
    pub fn failure(&self) -> Option<AgentError> {
        self.0.failure()
    }
    pub fn has_pending(&self) -> bool {
        self.0.has_pending()
    }
}

/// The facade of one published branch for callers outside its subtree:
/// validated input, stop, idle, configuration, read-only history and
/// capability state. It cannot write arbitrary events; the branch tree owns
/// the [`TimelineContext`] behind it.
#[derive(Clone)]
pub struct Branch(pub(crate) Arc<TimelineContext>);
impl Branch {
    pub fn id(&self) -> &str {
        self.0.id()
    }
    pub fn is_closing(&self) -> bool {
        self.0.is_closing()
    }
    /// Reloading while Loop is being replaced.
    pub fn state(&self) -> Result<BranchStatus> {
        self.0.state()
    }
    /// Changes when [`Self::state`] may read differently.
    pub fn watch(&self) -> watch::Receiver<()> {
        self.0.watch()
    }
    pub fn dispatch_watch(&self) -> watch::Receiver<crate::core::DispatchState> {
        self.0.dispatch_watch()
    }
    pub fn publication(&self) -> Publication {
        Publication(self.0.timeline().clone())
    }
    /// Appends validated input. The returned event is committed; whether it
    /// starts a turn shows in Loop's state once it is dispatched. The branch
    /// accepts each submission id once: an input whose id it already holds
    /// writes nothing and returns that input.
    pub async fn input(&self, data: InputMessageData) -> Result<Arc<AgentEvent>> {
        self.0.input(data).await
    }
    /// The published input with submission id `id`; `None` when the branch
    /// holds none. Every input committed before the call is found once it is
    /// published. Fails with [`AgentError::AmbiguousSubmission`] for an id
    /// with several inputs, written before the branch accepted each id once.
    pub async fn lookup(&self, id: &str) -> Result<Option<Arc<AgentEvent>>> {
        self.0.lookup(id).await
    }
    /// Commits a turn interrupt and returns its sequence without waiting for
    /// dispatch or settlement. Safe to call while capabilities reload.
    pub fn interrupt(&self, reason: String) -> Result<u64> {
        self.0.interrupt(reason)
    }
    /// Stops branch execution and returns its confirmed dispatched sequence.
    pub fn stop(&self, reason: String) -> BoxFuture<'static, Result<u64>> {
        self.0.stop(reason)
    }
    /// Ends the branch: interrupts it, settles its work, unloads its subtree.
    pub fn close(&self) -> BoxFuture<'static, Result<()>> {
        self.0.close()
    }
    /// Waits for branch execution to idle and returns its dispatched cutoff.
    pub async fn idle(&self) -> Result<u64> {
        self.0.idle().await
    }
    pub async fn configure(&self, update: ConfigureUpdate) -> Result<ConfigureOutcome> {
        self.0.configure(update).await
    }
    pub fn get<S: Clone + Send + Sync + 'static>(&self) -> Option<S> {
        self.0.state_cell::<S>().map(|s| s.get())
    }
    pub fn on<S: Clone + Send + Sync + 'static>(&self) -> Option<watch::Receiver<S>> {
        self.0.state_cell::<S>().map(|s| s.subscribe())
    }
    /// Reads the current capability service, including after a reload.
    pub fn service<T: ?Sized + PublicService>(&self) -> Result<Arc<T>> {
        self.0.public::<T>()
    }
    /// The branch scope and the capabilities it installed.
    pub fn diagnostics(&self) -> rutis::host::diagnostics::RuntimeDiagnostics {
        self.0.diagnostics()
    }
    /// Published sequence of the branch Timeline.
    pub fn sequence(&self) -> u64 {
        self.0.timeline().sequence()
    }
    /// Outside the branch only published (durable) events are visible;
    /// capabilities read the Timeline itself, through committed events.
    fn published(&self, through: Option<u64>) -> Result<u64> {
        let published = self.sequence();
        match through {
            Some(through) if through > published => Err(AgentError::InvalidInput(format!(
                "future timeline sequence {through}"
            ))),
            Some(through) => Ok(through),
            None => Ok(published),
        }
    }
    pub async fn history(&self, through: Option<u64>) -> Result<Vec<Arc<AgentEvent>>> {
        let through = self.published(through)?;
        self.0.timeline().history(Some(through)).await
    }
    /// The first page of a backward, kind-filtered read of the whole branch
    /// chain, pinned at the published head.
    pub fn page_cursor(&self) -> Result<BranchCursor> {
        self.0.timeline().page_cursor(self.sequence())
    }
    /// One page of that read: up to `limit` stored rows of `kinds`, oldest
    /// first, from the cursor's segment at the pinned head. `kinds` is the
    /// caller's selection; the store skips every other row before decoding.
    pub async fn page(
        &self,
        cursor: BranchCursor,
        limit: usize,
        kinds: &[&str],
    ) -> Result<BranchPage> {
        self.0.timeline().page(cursor, limit, kinds).await
    }
    pub async fn local<R: std::ops::RangeBounds<u64> + Send>(
        &self,
        range: R,
    ) -> Result<Vec<Arc<AgentEvent>>> {
        use std::ops::Bound;
        let start = match range.start_bound() {
            Bound::Included(n) => *n,
            Bound::Excluded(n) => n.saturating_add(1),
            Bound::Unbounded => 1,
        };
        let end = match range.end_bound() {
            Bound::Included(n) => Some(*n),
            Bound::Excluded(n) => Some(n.saturating_sub(1)),
            Bound::Unbounded => None,
        };
        let end = self.published(end)?;
        self.0.timeline().local(start..=end).await
    }
    pub async fn context(&self, through: u64) -> Result<ContextHistory> {
        let through = self.published(Some(through))?;
        self.0.timeline().context(through).await
    }
    /// The cut of the latest completed compaction at or below `through` on this
    /// branch, when it is one of its own events.
    pub async fn compaction_cut(&self, through: u64) -> Result<Option<u64>> {
        let through = self.published(Some(through))?;
        self.0.timeline().compaction_cut(through).await
    }
    /// One page of the rows the context projection consumes after `after`; the
    /// cursor may fall inside an atomic batch and the event limit is a target.
    pub async fn context_page(
        &self,
        after: u64,
        limit: usize,
        through: u64,
    ) -> Result<Vec<Arc<AgentEvent>>> {
        let through = self.published(Some(through))?;
        self.0
            .timeline()
            .stored_context_page(after, limit, through)
            .await
    }
    pub async fn trace(&self, sequence: u64) -> Result<Option<TimelineTrace>> {
        self.published(Some(sequence))?;
        self.0.timeline().trace(sequence).await
    }
}

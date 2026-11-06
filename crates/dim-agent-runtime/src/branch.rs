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
    /// Checks that `sequence` ends an atomic batch, so a cut there can start a
    /// read (ADR 020 D4). See [`Timeline::batch_end`].
    pub async fn batch_end(&self, sequence: u64) -> Result<()> {
        self.0.timeline().batch_end(sequence).await
    }
    /// One bounded read of this branch's facts: the request selects them and
    /// the reply carries each event's position. The read stops at what is
    /// published: the branch's own committed tail belongs to the part that
    /// folds it, through the branch's Timeline.
    pub async fn query(&self, query: TimelineQuery) -> Result<TimelineQueryReply> {
        if query.include_publishing {
            return Err(AgentError::InvalidInput(
                "a facade read stops at the published head".into(),
            ));
        }
        self.0.timeline().query(query).await
    }
}

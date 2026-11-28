use crate::assembly::OpeningAdvanced;
use crate::core::TailLock;
use crate::lifecycle::{CloseOnce, guarded};
use crate::*;
use dim_agent_types::{
    AgentEvent, EventKind, InputMessageData, InterruptScope, StopInterruptData, TimelinePosition,
};
use rutis::Ctx;
use rutis::host::{CtxHost, FiberView};
use serde::{Deserialize, Serialize};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};
use tokio::{
    sync::{Mutex as AsyncMutex, watch},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

/// Runtime owner of one loaded branch: its rutis subtree, the Timeline that
/// holds its durable facts, the Dispatcher that drives its capabilities, and
/// the branch lifecycle. Business state in memory belongs to the capabilities
/// of the subtree, which rebuild it from the Timeline; this owner only finds
/// them per call and never caches their instances or aggregates their state.
///
/// Obtained only from [`TimelineFactory::build`] and held by the branch tree.
/// A caller outside the tree names the line and calls one of the operations of
/// its data face, [`TimelineContext`].
pub struct BranchRuntime {
    id: String,
    ctx: Ctx,
    view: FiberView,
    children: Vec<FiberView>,
    /// Loader row ids the branch profile requires.
    required: Arc<[String]>,
    /// The required views its scope handed and the required parts its
    /// mounter mounted before it published.
    awaited: Vec<FiberView>,
    copies: crate::parts::Copies,
    scope: BranchScope,
    timeline: Timeline,
    dispatcher: Dispatcher,
    lifecycle: BranchLifecycle,
    opening: BranchOpening,
    /// Signals a possible change of [`BranchStatus`]; the status itself is
    /// read from its owners on query.
    changed: watch::Sender<()>,
    /// A failure recorded by the branch's owner outside dispatch.
    failure: Mutex<Option<String>>,
    stop: CancellationToken,
    tasks: Mutex<Option<(JoinHandle<()>, JoinHandle<()>)>>,
    /// The sequence of the line's committed `branch/end`, 0 before it.
    ended: AtomicU64,
    /// Serializes stop and close.
    control: AsyncMutex<()>,
    close: CloseOnce,
    stop_operation: Mutex<Option<StopOperation>>,
    /// Where the facade reads which group a name is in.
    catalog: Arc<rutis_loader::ServiceCatalog>,
    /// The names the assembly marked internal.
    faces: Arc<Faces>,
}
/// How often a wait looks for a reloading Loop's successor.
const LOOP_RETURN_POLL: std::time::Duration = std::time::Duration::from_millis(10);

/// The latest stop and the sequence of its interrupt (0 until written).
type StopOperation = (Arc<CloseOnce<u64>>, Arc<AtomicU64>);

impl BranchRuntime {
    /// Dispatches the opening through `through`, seals registration and waits
    /// for capability receipts and their committed fence. First activations,
    /// dispatch advancement and receipt completion renew the inactivity limit.
    pub async fn finish_opening(
        &self,
        through: u64,
        timeout: std::time::Duration,
        opening: crate::OpeningKind,
    ) -> Result<()> {
        let mut progress = StartupProgress::new(&self.ctx, timeout)?;
        progress
            .wait(
                async {
                    let mut dispatch = self.dispatcher.watch();
                    loop {
                        let state = dispatch.borrow_and_update().clone();
                        if let Some(error) = state.failure {
                            return Err(error);
                        }
                        if state.sequence >= through {
                            break;
                        }
                        dispatch
                            .changed()
                            .await
                            .map_err(|_| AgentError::Closed("branch dispatch".into()))?;
                    }
                    let receipts = self.opening.finish(&self.ctx, self.view.id, opening);
                    tokio::pin!(receipts);
                    loop {
                        if let Some(error) = dispatch.borrow_and_update().failure.clone() {
                            return Err(error);
                        }
                        tokio::select! {
                            result = &mut receipts => { result?; break; }
                            changed = dispatch.changed() => {
                                changed.map_err(|_| AgentError::Closed("branch dispatch".into()))?;
                            }
                        }
                    }
                    let through = self.timeline.committed();
                    loop {
                        let state = dispatch.borrow_and_update().clone();
                        if let Some(error) = state.failure {
                            return Err(error);
                        }
                        if state.sequence >= through {
                            return Ok(());
                        }
                        dispatch
                            .changed()
                            .await
                            .map_err(|_| AgentError::Closed("branch dispatch".into()))?;
                    }
                },
                || vec![self.view.id],
            )
            .await
            .unwrap_or_else(|| Err(AgentError::Build("branch opening timed out".into())))
    }

    /// Starts dispatch once every capability of the subtree is Active.
    #[expect(
        clippy::too_many_arguments,
        reason = "one parameter per handle a branch's BranchRuntime is built from"
    )]
    pub(crate) fn start(
        id: String,
        ctx: Ctx,
        view: FiberView,
        children: Vec<FiberView>,
        required: Arc<[String]>,
        awaited: Vec<FiberView>,
        copies: crate::parts::Copies,
        scope: BranchScope,
        timeline: Timeline,
        dispatcher: Dispatcher,
        lifecycle: BranchLifecycle,
        opening: BranchOpening,
        faults: FaultReporter,
        catalog: Arc<rutis_loader::ServiceCatalog>,
        faces: Arc<Faces>,
    ) -> Arc<Self> {
        let this = Arc::new(Self {
            catalog,
            faces,
            id,
            ctx,
            view,
            children,
            required,
            awaited,
            copies,
            scope,
            timeline,
            dispatcher,
            lifecycle,
            opening,
            changed: watch::channel(()).0,
            failure: Mutex::new(None),
            ended: AtomicU64::new(0),
            stop: CancellationToken::new(),
            tasks: Mutex::new(None),
            control: AsyncMutex::new(()),
            close: CloseOnce::default(),
            stop_operation: Mutex::new(None),
        });
        let dispatcher = this.dispatcher.clone();
        let timeline = this.timeline.clone();
        let ctx = this.ctx.clone();
        let stop = this.stop.clone();
        let driver = tokio::spawn(async move {
            dispatcher.run(timeline, ctx, stop).await;
            if let Some(error) = dispatcher.failure() {
                faults.report(error);
            }
        });
        let weak = Arc::downgrade(&this);
        let stop = this.stop.clone();
        let mut dispatcher = this.dispatcher.watch();
        let monitor = tokio::spawn(async move {
            let mut phase: Option<watch::Receiver<LoopSnapshot>> = None;
            let mut signal = true;
            let mut sequence = dispatcher.borrow().sequence;
            loop {
                let Some(inner) = weak.upgrade() else {
                    return;
                };
                let dispatched = dispatcher.borrow().sequence;
                if dispatched > sequence {
                    sequence = dispatched;
                    if inner.lifecycle.phase() == BranchPhase::Loading
                        && let Err(error) = inner.ctx.emit(OpeningAdvanced::Plugin(inner.view.id))
                    {
                        inner.dispatcher.fail(error.into());
                    }
                }
                let had = phase.is_some();
                if phase.as_ref().is_none_or(|rx| rx.has_changed().is_err()) {
                    phase = inner
                        .ctx
                        .get_at::<dyn LoopState>(&inner.scope.key::<dyn LoopState>())
                        .map(|state| state.watch());
                }
                if signal || had != phase.is_some() {
                    inner.changed.send_replace(());
                }
                drop(inner);
                signal = match phase.as_mut() {
                    Some(rx) => {
                        tokio::select! {
                            _ = stop.cancelled() => return,
                            _ = rx.changed() => true,
                            _ = dispatcher.changed() => true,
                        }
                    }
                    None => {
                        tokio::select! {
                            _ = stop.cancelled() => return,
                            _ = dispatcher.changed() => true,
                            _ = tokio::time::sleep(LOOP_RETURN_POLL) => false,
                        }
                    }
                };
            }
        });
        *this.tasks.lock().unwrap() = Some((driver, monitor));
        this
    }
    /// Marks the branch Active once its owner finished starting it: after
    /// dispatch began and the branch was published. Background work its
    /// capabilities spawned in `apply` begins now.
    pub fn activate(&self) {
        if !self.close.started() {
            self.lifecycle.advance(BranchPhase::Active);
        }
    }
    /// The facade handed to callers inside the tree.
    pub fn branch(self: &Arc<Self>) -> Branch {
        Branch(self.clone())
    }
    /// The line's data face, which [`TimelineFactory::build`] provides in the
    /// branch scope.
    pub fn face(self: &Arc<Self>) -> TimelineContext {
        TimelineContext { line: self.clone() }
    }
    pub fn id(&self) -> &str {
        &self.id
    }
    /// The branch's Timeline, including writes: for the branch tree, which
    /// records branch lifecycle facts on it.
    pub fn timeline(&self) -> &Timeline {
        &self.timeline
    }
    pub(crate) fn mark_failure(&self, error: &AgentError) {
        *self.failure.lock().unwrap() = Some(error.to_string());
        self.changed.send_replace(());
    }
    pub fn is_closing(&self) -> bool {
        self.close.started()
    }
    /// Records the sequence of this line's committed `branch/end`: input is
    /// refused from this call on.
    pub fn mark_ended(&self, sequence: u64) {
        self.ended.store(sequence, Ordering::Release);
    }
    /// Whether this line's `branch/end` has committed.
    pub fn ended(&self) -> bool {
        self.end_event().is_some()
    }
    /// The sequence of this line's committed `branch/end`, `None` before it.
    pub fn end_event(&self) -> Option<u64> {
        match self.ended.load(Ordering::Acquire) {
            0 => None,
            sequence => Some(sequence),
        }
    }
    /// True once the close has settled: its Timeline is closed, so nothing is
    /// committed to it any more, and its subtree is released.
    pub(crate) fn is_closed(&self) -> bool {
        self.lifecycle.phase() == BranchPhase::Closed
    }
    /// Read on query: execution from Loop, failure from dispatch or the
    /// owner, closure from the branch lifecycle. Reloading while Loop is
    /// being replaced.
    pub(crate) fn state(&self) -> Result<BranchStatus> {
        let failure = self
            .dispatcher
            .failure()
            .map(|error| error.to_string())
            .or_else(|| self.failure.lock().unwrap().clone());
        let lifecycle = self.lifecycle.phase();
        if lifecycle == BranchPhase::Closed {
            return Ok(BranchStatus {
                closed: true,
                failure,
                ..BranchStatus::default()
            });
        }
        let phase = match self.loop_state() {
            Ok(state) => state.state().phase,
            Err(_) if lifecycle == BranchPhase::Closing => LoopPhase::Idle,
            Err(error) => return Err(error),
        };
        Ok(BranchStatus {
            running: phase != LoopPhase::Idle,
            stopping: phase == LoopPhase::Stopping,
            closed: false,
            failure,
        })
    }
    /// Changes when [`Self::state`] may read differently.
    pub(crate) fn watch(&self) -> watch::Receiver<()> {
        self.changed.subscribe()
    }
    pub(crate) fn dispatch_watch(&self) -> watch::Receiver<crate::core::DispatchState> {
        self.dispatcher.watch()
    }
    fn loop_state(&self) -> Result<Arc<dyn LoopState>> {
        self.port::<dyn LoopState>("Loop")
    }
    /// Capability ports are looked up per call: a reloading capability is absent.
    /// A branch service a facade reads: a name of the branch group.
    pub(crate) fn public<T: ?Sized + Send + Sync + 'static>(&self) -> Result<Arc<T>> {
        crate::names::in_group::<T>(&self.catalog, crate::BRANCH_GROUP)?;
        self.port::<T>(std::any::type_name::<T>())
    }
    pub(crate) fn port<T: ?Sized + Send + Sync + 'static>(&self, name: &str) -> Result<Arc<T>> {
        self.ctx
            .get_at::<T>(&self.scope.key::<T>())
            .ok_or_else(|| AgentError::Reloading(name.into()))
    }
    /// The branch scope and the capabilities it installed.
    pub(crate) fn diagnostics(&self) -> rutis::host::diagnostics::RuntimeDiagnostics {
        let scope = self.view.id;
        let mut snapshot = self.ctx.diagnostics();
        let rows: std::collections::HashSet<_> = self.children.iter().map(|view| view.id).collect();
        crate::assembly::retain_scope(&mut snapshot, scope, |plugin| rows.contains(&plugin));
        snapshot
    }
    /// This branch's scope view.
    pub(crate) fn scope_view(&self) -> &FiberView {
        &self.view
    }
    /// The branch instance's context, for reads of its data faces by id
    /// ([`crate::route::SessionScope`]).
    pub(crate) fn ctx(&self) -> &Ctx {
        &self.ctx
    }
    /// The catalog this branch's group names come from.
    pub(crate) fn catalog(&self) -> &rutis_loader::ServiceCatalog {
        &self.catalog
    }
    /// The names the assembly marked internal.
    pub(crate) fn faces(&self) -> &Faces {
        &self.faces
    }
    /// The branch group instance this line runs in.
    pub(crate) fn scope_instance(&self) -> rutis::host::InstanceId {
        self.scope.instance
    }
    /// The loader row ids this branch's profile requires.
    pub(crate) fn required_rows(&self) -> &[String] {
        &self.required
    }
    /// The required views its scope handed and the parts its mounter mounted
    /// when it opened.
    pub(crate) fn awaited(&self) -> &[FiberView] {
        &self.awaited
    }
    pub(crate) fn state_cell<S: Clone + Send + Sync + 'static>(&self) -> Option<Arc<StateCell<S>>> {
        self.ctx
            .get_at::<StateCell<S>>(&self.scope.key::<StateCell<S>>())
    }
    pub(crate) async fn configure(&self, update: ConfigureUpdate) -> Result<ConfigureOutcome> {
        if self.close.started() {
            return Err(AgentError::Closed(self.id().into()));
        }
        let control = self
            .ctx
            .get_at::<dyn ConfigureControl>(&self.scope.key::<dyn ConfigureControl>())
            .ok_or_else(|| AgentError::Build("ConfigureControl is not installed".into()))?;
        control.configure(update).await
    }
    /// Validates input from outside the branch and appends it. Whether it
    /// starts a turn is Loop's decision when the input is dispatched. An
    /// input whose submission id the branch already holds writes nothing and
    /// returns the committed input.
    pub(crate) async fn input(&self, data: InputMessageData) -> Result<Arc<AgentEvent>> {
        self.input_commit(data).await.map(|(event, _)| event)
    }
    /// The checks every externally committed input passes before it reaches
    /// the Timeline: the Dispatcher and the Session generation are healthy,
    /// and the input carries well-formed blocks.
    fn check_input(&self, data: &InputMessageData) -> Result<()> {
        if let Some(error) = self.dispatcher.failure() {
            return Err(error);
        }
        if self.close.started() {
            return Err(AgentError::Closed(self.id().into()));
        }
        data.validate()
            .map_err(|reason| AgentError::InvalidInput(reason.into()))
    }
    /// The admission every input append runs under the Timeline's commit
    /// lock: a branch that has ended refuses new input.
    fn admit_input(&self) -> Result<()> {
        if self.ended() {
            return Err(AgentError::BranchEnded(self.id.clone()));
        }
        Ok(())
    }
    /// [`Self::input`], also saying whether this call wrote the input. The
    /// answer comes from the append that decides the duplicate under the
    /// commit lock, so of two concurrent duplicates exactly one is
    /// [`InputCommit::Written`].
    pub(crate) async fn input_commit(
        &self,
        data: InputMessageData,
    ) -> Result<(Arc<AgentEvent>, InputCommit)> {
        self.check_input(&data)?;
        let result = self
            .timeline
            .input_checked(data, None, || self.admit_input())
            .await?;
        match result {
            crate::core::Appended::Written(event) => Ok((event, InputCommit::Written)),
            crate::core::Appended::Existing(sequence) => {
                Ok((self.committed_input(sequence).await?, InputCommit::Existing))
            }
            crate::core::Appended::Moved => Err(AgentError::fault(
                "timeline.input",
                "an unconditional input moved",
            )),
        }
    }
    /// [`Self::input`] only while the branch's last assigned sequence is still
    /// `head`. `None` means a fact committed behind `head`: nothing was
    /// written, no sequence was assigned and the caller decides again, even
    /// for a submission id already committed; otherwise it answers as
    /// [`Self::input`] does.
    pub(crate) async fn input_if_head(
        &self,
        data: InputMessageData,
        head: u64,
    ) -> Result<Option<Arc<AgentEvent>>> {
        self.check_input(&data)?;
        let result = self
            .timeline
            .input_checked(data, Some(head), || self.admit_input())
            .await?;
        match result {
            crate::core::Appended::Written(event) => Ok(Some(event)),
            crate::core::Appended::Existing(sequence) => {
                Ok(Some(self.committed_input(sequence).await?))
            }
            crate::core::Appended::Moved => Ok(None),
        }
    }
    /// Validates a batch of input from outside the branch and appends it as
    /// one atomic commit. Every input is checked as [`Self::input`] checks
    /// one, and the whole batch is admitted once. A batch is never empty and
    /// every input carries a submission id; the ids are distinct. When every
    /// id is already committed the batch writes nothing and returns those
    /// inputs in the given order; when only some are, it fails with
    /// [`AgentError::PartialSubmission`] and writes nothing either.
    pub(crate) async fn inputs(
        &self,
        inputs: Vec<InputMessageData>,
    ) -> Result<Vec<Arc<AgentEvent>>> {
        if let Some(error) = self.dispatcher.failure() {
            return Err(error);
        }
        if self.close.started() {
            return Err(AgentError::Closed(self.id().into()));
        }
        if inputs.is_empty() {
            return Err(AgentError::InvalidInput(
                "an input batch must contain at least one input".into(),
            ));
        }
        for input in &inputs {
            input
                .validate()
                .map_err(|reason| AgentError::InvalidInput(reason.into()))?;
        }
        let result = self
            .timeline
            .inputs_checked(inputs, || {
                if self.ended() {
                    return Err(AgentError::BranchEnded(self.id.clone()));
                }
                Ok(())
            })
            .await?;
        match result {
            crate::core::AppendedBatch::Written(events) => Ok(events),
            crate::core::AppendedBatch::Existing(sequences) => {
                let mut events = Vec::with_capacity(sequences.len());
                for sequence in sequences {
                    events.push(self.committed_input(sequence).await?);
                }
                Ok(events)
            }
        }
    }
    /// The committed input with submission id `id`, once it is published.
    pub(crate) async fn lookup(&self, id: &str) -> Result<Option<Arc<AgentEvent>>> {
        let Some(input) = self
            .timeline
            .submission(id, self.timeline.committed())
            .await?
        else {
            return Ok(None);
        };
        let _ = self
            .timeline
            .watch()
            .wait_for(|&(published, closed)| published >= input.sequence || closed)
            .await;
        if self.timeline.sequence() < input.sequence {
            return Err(self
                .timeline
                .failure()
                .unwrap_or_else(|| AgentError::Closed(self.id().into())));
        }
        let reply = self
            .timeline
            .query(TimelineQuery {
                scope: TimelineQueryScope::Branch,
                include_publishing: false,
                from: Some(TimelinePosition {
                    branch_id: self.id().into(),
                    sequence: input.sequence,
                }),
                through: Some(input.sequence),
                query: QuerySpec {
                    filter: Some(QueryFilter::In {
                        value: QueryOperand::Field { path: "id".into() },
                        values: vec![input.id.into()],
                    }),
                    select: QuerySelect::Events,
                    group_by: vec![],
                    pick: None,
                    order_by: vec![],
                },
                page: Some(QueryPage {
                    limit: std::num::NonZeroU64::MIN,
                    after: None,
                }),
            })
            .await?;
        let QueryOutput::Events { events } = reply.result else {
            unreachable!("event selection returns events")
        };
        events
            .into_iter()
            .next()
            .map(|row| Some(row.event))
            .ok_or_else(|| {
                AgentError::Storage(format!("committed input {} is missing", input.sequence))
            })
    }
    /// Returns an already committed submission, including one awaiting publication.
    async fn committed_input(&self, sequence: u64) -> Result<Arc<AgentEvent>> {
        let reply = self
            .timeline
            .query(TimelineQuery {
                scope: TimelineQueryScope::Branch,
                include_publishing: true,
                from: Some(TimelinePosition {
                    branch_id: self.id().into(),
                    sequence,
                }),
                through: Some(sequence),
                query: QuerySpec {
                    filter: None,
                    select: QuerySelect::Events,
                    group_by: vec![],
                    pick: None,
                    order_by: vec![],
                },
                page: Some(QueryPage {
                    limit: std::num::NonZeroU64::MIN,
                    after: None,
                }),
            })
            .await?;
        let QueryOutput::Events { events } = reply.result else {
            unreachable!("event selection returns events")
        };
        events
            .into_iter()
            .next()
            .map(|row| row.event)
            .ok_or_else(|| AgentError::Storage(format!("committed input {sequence} is missing")))
    }

    /// Waits until dispatch has consumed through `through`. Loop and every
    /// consumer reload as a restart, so a dispatcher that is merely replaced
    /// keeps this waiting; a dispatcher that failed answers its failure.
    pub(crate) async fn wait_dispatched(&self, through: u64) -> Result<()> {
        let mut dispatch = self.dispatcher.watch();
        let mut state = self.watch();
        loop {
            if let Some(error) = self.dispatcher.failure() {
                return Err(error);
            }
            if self.dispatcher.sequence() >= through {
                return Ok(());
            }
            // A line that ended answers here instead of holding its close
            // open: dispatch reaching the asked sequence is the wait, and one
            // that did not is a terminal failure ([ADR 027][027], [rule 8]).
            //
            // [027]: ../../../docs/adr/027-cross-scope-access-by-id.md
            // [rule 8]: ../../../docs/architecture/rust-agent/principles.md
            if self.close.started() || self.is_closing() {
                return match self.dispatcher.sequence() >= through {
                    true => Ok(()),
                    false => Err(self
                        .dispatcher
                        .failure()
                        .or_else(|| self.timeline.failure())
                        .unwrap_or_else(|| AgentError::Closed(self.id.clone()))),
                };
            }
            tokio::select! {
                result = dispatch.changed() => {
                    result.map_err(|_| AgentError::Closed(self.id.clone()))?;
                }
                result = state.changed() => {
                    // The status changed: the loop re-reads it, which settles a
                    // line that started closing.
                    let _ = result;
                }
            }
        }
    }
    /// Waits until the Timeline published `sequence`. A Timeline that closed
    /// before it answers its failure, or `Closed`.
    pub(crate) async fn wait_published(&self, sequence: u64) -> Result<()> {
        let mut log = self.timeline.watch();
        loop {
            let (published, closed) = *log.borrow_and_update();
            if published >= sequence {
                return Ok(());
            }
            if closed {
                return Err(self
                    .timeline
                    .failure()
                    .unwrap_or_else(|| AgentError::Closed(self.id.clone())));
            }
            log.changed()
                .await
                .map_err(|_| AgentError::Closed(self.id.clone()))?;
        }
    }
    /// Waits until Loop has dispatched the Timeline tail and has no unclosed
    /// turn; returns that dispatched sequence.
    pub(crate) async fn idle(&self) -> Result<u64> {
        let mut dispatch = self.dispatcher.watch();
        let mut log = self.timeline.watch();
        let mut phase = self.loop_watch().await?;
        loop {
            if let Some(error) = self.dispatcher.failure() {
                return Err(error);
            }
            let dispatched = self.dispatcher.sequence();
            if phase.borrow().phase == LoopPhase::Idle
                && !self.timeline.has_pending()
                && dispatched >= self.timeline.sequence()
            {
                return Ok(dispatched);
            }
            tokio::select! {
                _ = dispatch.changed() => {},
                _ = log.changed() => {},
                changed = phase.changed() => if changed.is_err() {
                    phase = self.loop_watch().await?;
                },
            }
        }
    }
    pub(crate) fn stop(self: &Arc<Self>, reason: String) -> BoxFuture<'static, Result<u64>> {
        if self.close.started() {
            let id = self.id().to_owned();
            return Box::pin(async move { Err(AgentError::Closed(id)) });
        }
        let mut active = self.stop_operation.lock().unwrap();
        let (operation, written) = match &*active {
            Some((operation, written))
                if !operation.finished() && {
                    let interrupt = written.load(Ordering::Acquire);
                    interrupt == 0
                        || self.dispatcher.sequence() < interrupt
                        || self
                            .loop_state()
                            .is_ok_and(|state| state.state().phase != LoopPhase::Running)
                } =>
            {
                (operation.clone(), written.clone())
            }
            _ => {
                let started = (Arc::new(CloseOnce::default()), Arc::new(AtomicU64::new(0)));
                *active = Some(started.clone());
                started
            }
        };
        let this = self.clone();
        operation.start(self.ctx.handle(), async move {
            let _guard = this.control.lock().await;
            let result = if this.close.started() {
                Err(AgentError::Closed(this.id().into()))
            } else {
                guarded("timeline.stop", async {
                    this.stop_inner(reason, InterruptScope::Turn, Some(&written))
                        .await
                })
                .await
            };
            if let Err(error @ AgentError::Fault { .. }) = &result
                && !this.close.started()
            {
                this.dispatcher.fail(error.clone());
            }
            result
        })
    }
    pub(crate) fn interrupt(&self, reason: String) -> Result<u64> {
        if self.close.started() {
            return Err(AgentError::Closed(self.id().into()));
        }
        self.commit_interrupt(reason, InterruptScope::Turn, None)
    }
    fn commit_interrupt(
        &self,
        reason: String,
        scope: InterruptScope,
        written: Option<&AtomicU64>,
    ) -> Result<u64> {
        if let Some(error) = self.dispatcher.failure() {
            return Err(error);
        }
        let interrupt = self.timeline.append_registered(
            EventKind::StopInterrupt(StopInterruptData { reason, scope }).into(),
            |event| {
                if let Some(written) = written {
                    written.store(event.sequence, Ordering::Release);
                }
                Ok(())
            },
        )?;
        Ok(interrupt.sequence)
    }
    async fn stop_inner(
        &self,
        reason: String,
        scope: InterruptScope,
        written: Option<&AtomicU64>,
    ) -> Result<u64> {
        let interrupt = self.commit_interrupt(reason, scope, written)?;
        self.await_settled(interrupt).await
    }
    /// Waits until dispatch passed `interrupt` and the turn it ended settled.
    /// The interrupt is committed before this, so a running tool sees the stop
    /// (and can write its own terminal result) while this waits ([#2027]).
    async fn await_settled(&self, interrupt: u64) -> Result<u64> {
        let mut dispatch = self.dispatcher.watch();
        let mut phase = self.loop_watch().await?;
        let mut settled_through = None;
        loop {
            if let Some(error) = self.dispatcher.failure() {
                return Err(error);
            }
            let dispatched = self.dispatcher.sequence();
            if settled_through.is_none()
                && dispatched >= interrupt
                && phase.borrow().phase != LoopPhase::Stopping
            {
                settled_through = Some(self.timeline.committed());
            }
            if settled_through.is_some_and(|through| dispatched >= through) {
                return Ok(dispatched);
            }
            tokio::select! {
                _ = dispatch.changed() => {},
                changed = phase.changed() => if changed.is_err() {
                    phase = self.loop_watch().await?;
                },
            }
        }
    }
    /// Loop's state channel. A reloading Loop is a restart of Loop: its
    /// successor takes over from the committed history, the interrupt
    /// included, so this waits for it. When the branch is closing and Loop
    /// is gone, returns the recorded Dispatcher fault, or Closed if none exists.
    async fn loop_watch(&self) -> Result<watch::Receiver<LoopSnapshot>> {
        loop {
            match self.loop_state() {
                Ok(state) => return Ok(state.watch()),
                Err(_) if self.close.started() => {
                    return Err(self
                        .dispatcher
                        .failure()
                        .unwrap_or_else(|| AgentError::Closed(self.id().into())));
                }
                Err(AgentError::Reloading(_)) => {}
                Err(error) => return Err(error),
            }
            if let Some(error) = self.dispatcher.failure() {
                return Err(error);
            }
            tokio::time::sleep(LOOP_RETURN_POLL).await;
        }
    }
    /// Whether this line has work going on: a turn running, or accepted work
    /// dispatch has not reached yet. A line that is closing or failed has
    /// none: nothing runs on it, and its own close settles what it holds.
    pub(crate) fn is_running(&self) -> bool {
        if self.is_closing() || self.dispatcher.failure().is_some() {
            return false;
        }
        !self.turn_idle()
            || self
                .timeline
                .open_tail()
                .is_some_and(|tail| !self.dispatched(tail))
    }
    /// Loop is idle at `tail`: not closing or failed, dispatch has reached it
    /// and Loop is between turns.
    fn idle_at(&self, tail: u64) -> bool {
        !self.is_closing()
            && self.dispatcher.failure().is_none()
            && self.dispatched(tail)
            && self.turn_idle()
    }
    /// Loop is between turns; `false` while it runs one, and while Loop is
    /// being replaced.
    fn turn_idle(&self) -> bool {
        self.loop_state()
            .is_ok_and(|state| state.state().phase == LoopPhase::Idle)
    }
    /// Dispatch has reached `tail`.
    fn dispatched(&self, tail: u64) -> bool {
        self.dispatcher.sequence() == tail
    }
    /// Holds the commit lock and returns it when Loop is idle, nothing is
    /// pending and dispatch has reached the tail; `None` otherwise, without
    /// freezing or rejecting appends. Detached work is judged by Session holds.
    pub(crate) fn lock_idle(&self) -> Option<TailLock<'_>> {
        let tail = self.timeline.lock_tail()?;
        self.idle_at(tail.sequence()).then_some(tail)
    }
    /// Releases the instance after an idle check: no stop is recorded.
    pub fn unload(self: &Arc<Self>) -> BoxFuture<'static, Result<()>> {
        self.finish(true)
    }
    /// Ends the branch: interrupts it, settles, then unloads its subtree.
    pub fn close(self: &Arc<Self>) -> BoxFuture<'static, Result<()>> {
        self.finish(false)
    }
    fn finish(self: &Arc<Self>, unloading: bool) -> BoxFuture<'static, Result<()>> {
        let this = self.clone();
        self.lifecycle.advance(BranchPhase::Closing);
        self.close.start(self.ctx.handle(), async move {
            #[cfg(feature = "testing")]
            crate::testing::closing(&this.timeline.key().session_id, &this.id).await;
            // The closing interrupt is committed before the branch lock: a
            // tool that is running sees the stop, writes its own terminal
            // result, and only then does this wait for the turn to settle. The
            // lock a close takes is the same one a stop holds while it waits,
            // so committing after it would deadlock the two ([#2027]).
            let interrupt = match unloading {
                true => Ok(this.timeline.sequence()),
                false => {
                    this.timeline.thaw();
                    this.commit_interrupt("timeline closed".into(), InterruptScope::Branch, None)
                }
            };
            let _guard = this.control.lock().await;
            let span = tracing::info_span!(
                "timeline.close",
                session_id = %this.timeline.key().session_id,
                branch_id = %this.id,
                outcome = tracing::field::Empty,
            );
            let mut errors = vec![];
            let stopped = match interrupt {
                Ok(interrupt) if !unloading => {
                    guarded("timeline.stop", async {
                        this.await_settled(interrupt).await
                    })
                    .await
                }
                other => other,
            };
            let stopped = match stopped {
                Err(AgentError::Closed(_)) if this.loop_state().is_err() => Ok(0),
                stopped => stopped,
            };
            errors.extend(stopped.err());
            this.stop.cancel();
            let tasks = this.tasks.lock().unwrap().take();
            if let Some((driver, monitor)) = tasks {
                if let Err(error) = driver.await {
                    errors.push(AgentError::fault("timeline driver", error.to_string()));
                }
                if let Err(error) = monitor.await {
                    errors.push(AgentError::fault("timeline monitor", error.to_string()));
                }
            }
            if unloading {
                this.timeline.thaw();
            }
            let members = crate::assembly::with_copies(&this.children, &this.copies);
            if let Err(error) = crate::assembly::shutdown_scope(&this.view, &members).await {
                errors.push(error);
            }
            errors.extend(this.timeline.close().await.err());
            this.lifecycle.advance(BranchPhase::Closed);
            this.changed.send_replace(());
            span.record(
                "outcome",
                if errors.is_empty() {
                    "returned"
                } else {
                    "failure"
                },
            );
            AgentError::aggregate(errors)
        })
    }
}

/// The data face of one line ([ADR 027](../../../docs/adr/027-cross-scope-access-by-id.md)
/// D1): what a caller that names `(Session, line)` reaches, with every
/// operation answering data. The runtime owner behind it ([`BranchRuntime`])
/// stays with the branch tree, which opens, publishes and closes the line.
///
/// `TimelineFactory::build` provides it in the branch scope, and the catalog
/// shares the name across languages, so the same operations serve a Session
/// part (`sessionScope.lineCall`) and a process caller (`sessions.call`).
pub struct TimelineContext {
    line: Arc<BranchRuntime>,
}

/// One input write as the append under the commit lock decided it: a line
/// holds a submission id once, so only the call that wrote it answers
/// `written`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InputWrite {
    pub event: Arc<AgentEvent>,
    pub written: bool,
}

// The operations spell out `std::result::Result` for rutis' export generator,
// which resolves an alias's item type where the alias is defined
// (arcships/rutis#463).
#[rutis::service]
impl TimelineContext {
    /// Commits validated input ([`Timeline::input_checked`]), and says whether
    /// this call wrote it. Whether it starts a turn is Loop's decision when
    /// the input is dispatched.
    pub async fn input(
        &self,
        data: InputMessageData,
    ) -> std::result::Result<InputWrite, AgentError> {
        match self.line.input_commit(data).await? {
            (event, InputCommit::Written) => Ok(InputWrite {
                event,
                written: true,
            }),
            (event, InputCommit::Existing) => Ok(InputWrite {
                event,
                written: false,
            }),
        }
    }
    /// [`Self::input`] only while the line's last assigned sequence is still
    /// `head`: a fact behind `head` writes nothing and answers `null`, even
    /// for a submission id already committed.
    pub async fn input_if_head(
        &self,
        data: InputMessageData,
        head: u64,
    ) -> std::result::Result<Option<Arc<AgentEvent>>, AgentError> {
        self.line.input_if_head(data, head).await
    }
    /// Commits a batch of input as one atomic commit; see
    /// [`BranchRuntime::inputs`] for the contract.
    pub async fn inputs(
        &self,
        inputs: Vec<InputMessageData>,
    ) -> std::result::Result<Vec<Arc<AgentEvent>>, AgentError> {
        self.line.inputs(inputs).await
    }
    /// The published input with submission id `id`; `null` when the line
    /// holds none.
    pub async fn lookup(
        &self,
        id: String,
    ) -> std::result::Result<Option<Arc<AgentEvent>>, AgentError> {
        self.line.lookup(&id).await
    }
    /// Commits a turn interrupt and answers its sequence without waiting for
    /// dispatch or settlement.
    pub fn interrupt(&self, reason: String) -> std::result::Result<u64, AgentError> {
        self.line.interrupt(reason)
    }
    /// Stops the line and answers its confirmed dispatched sequence.
    pub async fn stop(&self, reason: String) -> std::result::Result<u64, AgentError> {
        self.line.stop(reason).await
    }
    /// Waits until dispatch has consumed the line's tail with no unclosed
    /// turn, and answers that dispatched sequence.
    pub async fn idle(&self) -> std::result::Result<u64, AgentError> {
        self.line.idle().await
    }
    /// The line's status as it stands: execution from Loop, failure from
    /// dispatch or the owner, closure from the branch lifecycle.
    pub fn state(&self) -> std::result::Result<BranchStatus, AgentError> {
        self.line.state()
    }
    /// Waits until dispatch has consumed through `through`; a capability that
    /// reloads is a restart, so this waits for its successor.
    pub async fn wait_dispatched(&self, through: u64) -> std::result::Result<(), AgentError> {
        self.line.wait_dispatched(through).await
    }
    /// Waits until the Timeline published `sequence`; a Timeline that closed
    /// answers its failure, or `Closed`.
    pub async fn wait_published(&self, sequence: u64) -> std::result::Result<(), AgentError> {
        self.line.wait_published(sequence).await
    }
}

use crate::core::TailLock;
use crate::lifecycle::{CloseOnce, guarded};
use crate::*;
use dim_agent_types::{AgentEvent, EventKind, InputMessageData, InterruptScope, StopInterruptData};
use rutis::Ctx;
use rutis::host::{CtxHost, FiberView};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU64, Ordering},
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
/// Callers outside the tree get a [`Branch`], which cannot write arbitrary
/// events.
pub struct TimelineContext {
    id: String,
    ctx: Ctx,
    view: FiberView,
    children: Vec<FiberView>,
    copies: crate::parts::Copies,
    scope: BranchScope,
    timeline: Timeline,
    dispatcher: Dispatcher,
    lifecycle: BranchLifecycle,
    opening: Arc<BranchOpening>,
    /// Signals a possible change of [`BranchStatus`]; the status itself is
    /// read from its owners on query.
    changed: watch::Sender<()>,
    /// A failure recorded by the branch's owner outside dispatch.
    failure: Mutex<Option<String>>,
    stop: CancellationToken,
    tasks: Mutex<Option<(JoinHandle<()>, JoinHandle<()>)>>,
    ended: AtomicBool,
    /// Serializes stop and close.
    control: AsyncMutex<()>,
    close: CloseOnce,
    stop_operation: Mutex<Option<StopOperation>>,
    /// Where the facade reads which group a name is in.
    catalog: Arc<rutis_loader::ServiceCatalog>,
}
/// How often a wait looks for a reloading Loop's successor.
const LOOP_RETURN_POLL: std::time::Duration = std::time::Duration::from_millis(10);

/// The latest stop and the sequence of its interrupt (0 until written).
type StopOperation = (Arc<CloseOnce<u64>>, Arc<AtomicU64>);

/// Receipts registered synchronously by the unique branch opening dispatch.
/// Work remains owned and joined by the capability, not by this collector.
pub struct BranchOpening(Mutex<Option<Vec<BoxFuture<'static, Result<()>>>>>);
impl BranchOpening {
    pub fn register(&self, receipt: BoxFuture<'static, Result<()>>) -> Result<()> {
        let mut receipts = self.0.lock().unwrap();
        receipts
            .as_mut()
            .ok_or_else(|| AgentError::Closed("branch opening".into()))?
            .push(receipt);
        Ok(())
    }
    pub(crate) fn new() -> Self {
        Self(Mutex::new(Some(Vec::new())))
    }
    async fn finish(&self) -> Result<()> {
        use futures_util::{StreamExt, stream::FuturesUnordered};
        let receipts = self.0.lock().unwrap().take().unwrap_or_default();
        let mut pending: FuturesUnordered<_> = receipts.into_iter().collect();
        while let Some(result) = pending.next().await {
            result?;
        }
        Ok(())
    }
}

impl TimelineContext {
    /// Called after opening dispatch: seal registration, await capability work,
    /// then dispatch a fixed committed fence before the first input.
    pub async fn finish_opening(&self) -> Result<()> {
        let mut dispatch = self.dispatcher.watch();
        let receipts = self.opening.finish();
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
    }

    /// Starts dispatch once every capability of the subtree is Active.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn start(
        id: String,
        ctx: Ctx,
        view: FiberView,
        children: Vec<FiberView>,
        copies: crate::parts::Copies,
        scope: BranchScope,
        timeline: Timeline,
        dispatcher: Dispatcher,
        lifecycle: BranchLifecycle,
        opening: Arc<BranchOpening>,
        faults: FaultReporter,
        catalog: Arc<rutis_loader::ServiceCatalog>,
    ) -> Arc<Self> {
        let this = Arc::new(Self {
            catalog,
            id,
            ctx,
            view,
            children,
            copies,
            scope,
            timeline,
            dispatcher,
            lifecycle,
            opening,
            changed: watch::channel(()).0,
            failure: Mutex::new(None),
            ended: AtomicBool::new(false),
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
            loop {
                let Some(inner) = weak.upgrade() else {
                    return;
                };
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
                        tokio::select! {_=stop.cancelled()=>return,_=rx.changed()=>true,_=dispatcher.changed()=>true}
                    }
                    None => {
                        tokio::select! {_=stop.cancelled()=>return,_=dispatcher.changed()=>true,_=tokio::time::sleep(LOOP_RETURN_POLL)=>false}
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
    /// The facade handed to callers outside the branch tree.
    pub fn branch(self: &Arc<Self>) -> Branch {
        Branch(self.clone())
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
    pub fn mark_ended(&self) {
        self.ended.store(true, Ordering::Release);
    }
    /// Whether this line's `branch/end` has committed.
    pub fn ended(&self) -> bool {
        self.ended.load(Ordering::Acquire)
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
        let rows: std::collections::HashSet<_> = snapshot
            .plugins
            .iter()
            .filter(|p| p.parent == Some(scope))
            .map(|p| p.id)
            .collect();
        crate::assembly::retain_scope(&mut snapshot, scope, |plugin| rows.contains(&plugin));
        snapshot
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
        if let Some(error) = self.dispatcher.failure() {
            return Err(error);
        }
        if self.close.started() {
            return Err(AgentError::Closed(self.id().into()));
        }
        data.validate()
            .map_err(|reason| AgentError::InvalidInput(reason.into()))?;
        let result = self.timeline.input_checked(data, || {
            if self.ended() {
                return Err(AgentError::BranchEnded(self.id.clone()));
            }
            Ok(())
        })?;
        match result {
            crate::core::Appended::Written(event) => Ok(event),
            crate::core::Appended::Existing(sequence) => self.committed_input(sequence).await,
        }
    }
    /// The committed input with submission id `id`, once it is published.
    pub(crate) async fn lookup(&self, id: &str) -> Result<Option<Arc<AgentEvent>>> {
        let Some(sequence) = self.timeline.submission(id)? else {
            return Ok(None);
        };
        let _ = self
            .timeline
            .watch()
            .wait_for(|&(published, closed)| published >= sequence || closed)
            .await;
        if self.timeline.sequence() < sequence {
            return Err(self
                .timeline
                .failure()
                .unwrap_or_else(|| AgentError::Closed(self.id().into())));
        }
        self.committed_input(sequence).await.map(Some)
    }
    async fn committed_input(&self, sequence: u64) -> Result<Arc<AgentEvent>> {
        self.timeline
            .local(sequence..=sequence)
            .await?
            .pop()
            .ok_or_else(|| AgentError::Storage(format!("committed input {sequence} is missing")))
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
    /// included, so this waits for it. Closed once the branch closes and
    /// Loop is gone for good.
    async fn loop_watch(&self) -> Result<watch::Receiver<LoopSnapshot>> {
        loop {
            match self.loop_state() {
                Ok(state) => return Ok(state.watch()),
                Err(_) if self.close.started() => return Err(AgentError::Closed(self.id().into())),
                Err(AgentError::Reloading(_)) => {}
                Err(error) => return Err(error),
            }
            if let Some(error) = self.dispatcher.failure() {
                return Err(error);
            }
            tokio::time::sleep(LOOP_RETURN_POLL).await;
        }
    }
    /// Holds the commit lock and returns it when Loop is idle, nothing is
    /// pending and dispatch has reached the tail; `None` otherwise, without
    /// freezing or rejecting appends. Detached work is judged by Session holds.
    pub(crate) fn lock_idle(&self) -> Option<TailLock<'_>> {
        if self.is_closing() || self.dispatcher.failure().is_some() {
            return None;
        }
        let tail = self.timeline.lock_tail()?;
        let idle = self.dispatcher.sequence() == tail.sequence()
            && self
                .loop_state()
                .is_ok_and(|state| state.state().phase == LoopPhase::Idle);
        idle.then_some(tail)
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
            let _guard = this.control.lock().await;
            let span = tracing::info_span!(
                "timeline.close",
                session_id = %this.timeline.key().session_id,
                branch_id = %this.id,
                outcome = tracing::field::Empty,
            );
            let mut errors = vec![];
            let stopped = if unloading {
                Ok(this.timeline.sequence())
            } else {
                this.timeline.thaw();
                guarded("timeline.stop", async {
                    this.stop_inner("timeline closed".into(), InterruptScope::Branch, None)
                        .await
                })
                .await
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

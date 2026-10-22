use crate::*;
use crate::{
    assembly::{
        AgentHost, Assembly, BuiltSession, Extension, Owned, Placement, SessionInitialization,
        build_session, own_placement, release, scope_diagnostics, shutdown_scope,
    },
    core::TailLock,
    lifecycle::{CloseOnce, guarded},
};
use dim_agent_types::{AgentEvent, InputMessageData, InputMode};
use rutis::FiberView;
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, Weak},
};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

#[derive(Clone)]
pub struct Agent(Arc<AgentInner>);
struct AgentInner {
    builder: AgentBuilder,
    assembly: Assembly,
    host: AgentHost,
    /// Where Sessions are created, settled with the first Session.
    placed: tokio::sync::OnceCell<Placed>,
    /// Captured at admission, including while placement is still building.
    handle: std::sync::OnceLock<tokio::runtime::Handle>,
    entries: Mutex<BTreeMap<String, Entry>>,
    closing: CancellationToken,
    close: CloseOnce,
}
enum Entry {
    Building(watch::Receiver<Option<Result<Session>>>),
    Ready(Session),
}
/// The loader and Agent instance Sessions go in, and what the Agent created
/// for them itself.
struct Placed {
    placement: Placement,
    owned: Mutex<Option<Owned>>,
}
enum SessionStart {
    Create,
    Prepared(BoxFuture<'static, Result<()>>),
    Restore,
}
impl Agent {
    pub(crate) fn new(builder: AgentBuilder, assembly: Assembly, host: AgentHost) -> Self {
        Self(Arc::new(AgentInner {
            builder,
            host,
            placed: tokio::sync::OnceCell::new(),
            handle: std::sync::OnceLock::new(),
            assembly,
            entries: Mutex::new(BTreeMap::new()),
            closing: CancellationToken::new(),
            close: CloseOnce::default(),
        }))
    }
    /// How many branch openings of this Agent's Sessions are alive: an
    /// opening ends when its branch leaves `Loading` or its build is torn
    /// down. For tests of that release.
    #[cfg(feature = "testing")]
    pub fn live_openings(&self) -> usize {
        self.0.assembly.openings.alive()
    }
    pub async fn create_session(&self, config: SessionConfig) -> Result<Session> {
        self.create(
            Uuid::new_v4().to_string(),
            config,
            SessionStart::Create,
            None,
        )
        .await
    }
    /// Loads the Session extension `group` ([`AgentBuilder::session_extension`])
    /// for this Session only, with `value`, before main initialization.
    /// Admission and rollback remain owned by this Agent.
    pub async fn create_session_with_extension<V: Send + Sync + 'static>(
        &self,
        config: SessionConfig,
        group: impl Into<String>,
        value: V,
    ) -> Result<Session> {
        self.create(
            Uuid::new_v4().to_string(),
            config,
            SessionStart::Create,
            Some(extension(group, value)),
        )
        .await
    }
    /// Reserves runtime capacity before committing the host-owned directory.
    pub async fn create_prepared_session(
        &self,
        id: String,
        config: SessionConfig,
        prepare: BoxFuture<'static, Result<()>>,
    ) -> Result<Session> {
        self.create(id, config, SessionStart::Prepared(prepare), None)
            .await
    }
    /// Loads a Session extension after admission and directory preparation.
    pub async fn create_prepared_session_with_extension<V: Send + Sync + 'static>(
        &self,
        id: String,
        config: SessionConfig,
        prepare: BoxFuture<'static, Result<()>>,
        group: impl Into<String>,
        value: V,
    ) -> Result<Session> {
        self.create(
            id,
            config,
            SessionStart::Prepared(prepare),
            Some(extension(group, value)),
        )
        .await
    }
    /// Restores the original Session identity and its stored configuration.
    pub async fn restore_session(&self, id: String) -> Result<Session> {
        let config = self
            .0
            .assembly
            .store
            .session_config(&id)
            .await?
            .ok_or_else(|| AgentError::Storage(format!("unknown session {id}")))?;
        self.create(id, config, SessionStart::Restore, None).await
    }
    /// The loader and Agent instance Sessions go in; an Agent hosting itself
    /// creates them with its first Session.
    async fn placement(&self) -> Result<&Placed> {
        self.0
            .placed
            .get_or_try_init(|| async {
                let host = match &self.0.host {
                    AgentHost::Placed(placement) => {
                        return Ok(Placed {
                            placement: placement.clone(),
                            owned: Mutex::new(None),
                        });
                    }
                    AgentHost::Private => None,
                    AgentHost::Within(host) => Some(host),
                };
                let (placement, owned) = own_placement(
                    &self.0.builder,
                    host,
                    self.0.assembly.startup_timeout,
                    &self.0.closing,
                )
                .await?;
                Ok(Placed {
                    placement,
                    owned: Mutex::new(Some(owned)),
                })
            })
            .await
    }
    async fn create(
        &self,
        id: String,
        config: SessionConfig,
        start: SessionStart,
        extension: Option<Extension>,
    ) -> Result<Session> {
        if self.0.closing.is_cancelled() {
            return Err(AgentError::Closed("agent".into()));
        }
        let assembly = self.0.assembly.clone();
        let (send, mut recv) = watch::channel(None);
        {
            let mut entries = self.0.entries.lock().unwrap();
            if self.0.closing.is_cancelled() {
                return Err(AgentError::Closed("agent".into()));
            }
            if entries.contains_key(&id) {
                return Err(AgentError::InvalidInput(format!(
                    "session {id} is already loaded"
                )));
            }
            self.0.handle.get_or_init(tokio::runtime::Handle::current);
            entries.insert(id.clone(), Entry::Building(recv.clone()));
        }
        let owner = self.clone();
        tokio::spawn(async move {
            let mut created = false;
            let mut result = async {
                let placement = owner.placement().await?.placement.clone();
                let initialization = match start {
                    SessionStart::Create => {
                        assembly.store.create_session(&id, &config).await?;
                        created = true;
                        SessionInitialization::New
                    }
                    SessionStart::Prepared(prepare) => {
                        prepare.await?;
                        created = true;
                        SessionInitialization::Prepared(assembly.store.branches(&id).await?)
                    }
                    SessionStart::Restore => {
                        SessionInitialization::Restore(assembly.store.branches(&id).await?)
                    }
                };
                build_session(
                    &placement,
                    assembly,
                    id.clone(),
                    config,
                    owner.0.closing.child_token(),
                    initialization,
                    extension,
                )
                .await
                .map(Session::start)
            }
            .await;
            // Publication and close admission are decided under the entries
            // lock (lifecycle §3): once Agent::close has taken its snapshot, a
            // finished build is closed here instead of being published.
            let unpublished = {
                let mut entries = owner.0.entries.lock().unwrap();
                match &result {
                    Ok(session) if !owner.0.closing.is_cancelled() => {
                        *session.0.agent.lock().unwrap() = Arc::downgrade(&owner.0);
                        entries.insert(id.clone(), Entry::Ready(session.clone()));
                        session.activate();
                        None
                    }
                    Ok(session) => Some(session.clone()),
                    Err(_) => None,
                }
            };
            if let Some(session) = unpublished {
                let cancelled = AgentError::Cancelled("agent closed during initialization".into());
                result = match session.close().await {
                    Ok(()) => Err(cancelled),
                    Err(cleanup) => Err(cancelled.with_cleanup(cleanup)),
                };
            }
            if created && let Err(failure) = result {
                result = Err(match owner.0.assembly.store.rollback_session(&id).await {
                    Ok(()) => failure,
                    Err(cleanup) => failure.with_cleanup(cleanup),
                });
            }
            if result.is_err() {
                owner.0.entries.lock().unwrap().remove(&id);
            }
            send.send_replace(Some(result));
        });
        loop {
            if let Some(value) = recv.borrow().clone() {
                return value;
            }
            recv.changed()
                .await
                .map_err(|_| AgentError::fault("session build", "task exited without result"))?;
        }
    }
    /// Snapshot of the tree hosting this Agent's Session subtrees.
    pub fn diagnostics(&self) -> Option<rutis::RuntimeDiagnostics> {
        self.0
            .placed
            .get()
            .map(|placed| placed.placement.anchor.diagnostics())
    }
    pub fn sessions(&self) -> Vec<Session> {
        self.0
            .entries
            .lock()
            .unwrap()
            .values()
            .filter_map(|e| match e {
                Entry::Ready(s) => Some(s.clone()),
                _ => None,
            })
            .collect()
    }
    pub fn get_session(&self, id: &str) -> Option<Session> {
        match self.0.entries.lock().unwrap().get(id) {
            Some(Entry::Ready(s)) => Some(s.clone()),
            _ => None,
        }
    }
    pub fn close(&self) -> BoxFuture<'static, Result<()>> {
        // Freeze admitted builds before cancellation can remove their results.
        // Keep this lock until CloseOnce owns the snapshot so another closer
        // cannot win registration with a later, incomplete snapshot.
        let entries = self.0.entries.lock().unwrap();
        self.0.closing.cancel();
        let Some(handle) = self.0.handle.get() else {
            return Box::pin(async { Ok(()) });
        };
        let mut pending = Vec::new();
        let mut sessions = Vec::new();
        for entry in entries.values() {
            match entry {
                Entry::Building(result) => pending.push(result.clone()),
                Entry::Ready(session) => sessions.push(session.clone()),
            }
        }
        let owner = self.clone();
        self.0.close.start(handle, async move {
            let mut errors = vec![];
            for mut result in pending {
                while result.borrow().is_none() {
                    if result.changed().await.is_err() {
                        break;
                    }
                }
                match result.borrow().clone() {
                    Some(Err(AgentError::Cancelled(_) | AgentError::Closed(_))) => {}
                    Some(Err(error)) => errors.push(error),
                    None => errors.push(AgentError::fault(
                        "session build",
                        "build ended without a result",
                    )),
                    Some(Ok(session)) => sessions.push(session),
                }
            }
            for session in sessions {
                if let Err(error) = session.close().await {
                    errors.push(error);
                }
            }
            let owned = owner
                .0
                .placed
                .get()
                .and_then(|placed| placed.owned.lock().unwrap().take());
            if let Some(owned) = owned
                && let Err(error) = release(owned).await
            {
                errors.push(error);
            }
            AgentError::aggregate(errors)
        })
    }
}
#[derive(Clone)]
pub struct Session(Arc<SessionInner>);
struct SessionInner {
    owner: Arc<SessionAssembly>,
    view: FiberView,
    handle: tokio::runtime::Handle,
    tree: Arc<dyn TimelineTreeControl>,
    created_at: u64,
    close: CloseOnce,
    agent: Mutex<Weak<AgentInner>>,
    supervisor: Mutex<Option<tokio::task::JoinHandle<()>>>,
}
impl Session {
    fn start(built: BuiltSession) -> Self {
        let BuiltSession { owner, view, tree } = built;
        Self(Arc::new(SessionInner {
            owner,
            view,
            handle: tokio::runtime::Handle::current(),
            tree,
            created_at: crate::core::unix_millis(),
            close: CloseOnce::default(),
            agent: Mutex::new(Weak::new()),
            supervisor: Mutex::new(None),
        }))
    }
    /// Publication: starts fault supervision and lets Session capabilities
    /// begin the background work they deferred during assembly.
    fn activate(&self) {
        self.0.owner.lifecycle.advance(SessionPhase::Active);
        let weak = Arc::downgrade(&self.0);
        let mut faults = self.0.owner.faults.watch();
        let stop = self.0.owner.closing.clone();
        let supervisor = tokio::spawn(async move {
            loop {
                if faults.borrow().is_some() {
                    if let Some(inner) = weak.upgrade() {
                        let session = Session(inner);
                        drop(session.close());
                    }
                    return;
                }
                tokio::select! {_=stop.cancelled()=>return,_=faults.changed()=>{}}
            }
        });
        *self.0.supervisor.lock().unwrap() = Some(supervisor);
    }
    pub fn id(&self) -> &str {
        &self.0.owner.id
    }
    pub fn config(&self) -> &SessionConfig {
        &self.0.owner.config
    }
    pub fn created_at(&self) -> u64 {
        self.0.created_at
    }
    fn published(&self, id: &str) -> Result<Branch> {
        self.0
            .tree
            .get(id)
            .ok_or_else(|| AgentError::UnknownBranch(id.into()))
    }
    /// Text input shorthand for [`Self::input`].
    pub async fn submit(
        &self,
        text: String,
        mode: InputMode,
        branch: &str,
    ) -> Result<Arc<AgentEvent>> {
        self.input(InputMessageData::text(text, Some(mode)), branch)
            .await
    }
    pub async fn input(&self, data: InputMessageData, branch: &str) -> Result<Arc<AgentEvent>> {
        // A reported fault precedes the supervisor's automatic close; surface
        // the root cause instead of an opaque closed rejection.
        if let Some(error) = self.0.owner.faults.failure() {
            return Err(error);
        }
        if self.0.owner.closing.is_cancelled() {
            return Err(AgentError::Closed(self.id().into()));
        }
        self.published(branch)?.input(data)
    }
    pub async fn configure(
        &self,
        update: ConfigureUpdate,
        branch: &str,
    ) -> Result<ConfigureOutcome> {
        if let Some(error) = self.0.owner.faults.failure() {
            return Err(error);
        }
        if self.0.owner.closing.is_cancelled() {
            return Err(AgentError::Closed(self.id().into()));
        }
        self.published(branch)?.configure(update).await
    }
    /// Commits a turn interrupt without waiting for dispatch or settlement.
    /// The returned sequence precedes any input submitted after this call.
    pub fn interrupt(&self, branch: &str, reason: String) -> Result<u64> {
        self.published(branch)?.interrupt(reason)
    }
    /// Stops branch execution and returns its confirmed dispatched sequence.
    /// This does not wait for host subscriptions to deliver notifications.
    pub async fn stop(&self, branch: &str, reason: String) -> Result<u64> {
        self.published(branch)?.stop(reason).await
    }
    /// Waits for branch execution to idle and returns its dispatched cutoff.
    /// Later input may advance the Timeline without changing this cutoff.
    pub async fn idle(&self, branch: &str) -> Result<u64> {
        self.published(branch)?.idle().await
    }
    pub async fn context(&self, branch: &str, through: u64) -> Result<ContextHistory> {
        self.published(branch)?.context(through).await
    }
    pub async fn history(
        &self,
        branch: &str,
        through: Option<u64>,
    ) -> Result<Vec<Arc<AgentEvent>>> {
        self.published(branch)?.history(through).await
    }
    pub async fn local<R: std::ops::RangeBounds<u64> + Send>(
        &self,
        branch: &str,
        range: R,
    ) -> Result<Vec<Arc<AgentEvent>>> {
        self.published(branch)?.local(range).await
    }
    /// Reads a local page of complete batches without exceeding the published head.
    pub async fn local_page(
        &self,
        branch: &str,
        after: u64,
        limit: usize,
        through: u64,
    ) -> Result<Vec<Arc<AgentEvent>>> {
        if through > self.sequence(branch)? {
            return Err(AgentError::InvalidInput("future Timeline sequence".into()));
        }
        let key = TimelineKey {
            session_id: self.id().into(),
            branch_id: branch.into(),
        };
        Ok(self
            .0
            .owner
            .assembly
            .store
            .read_page(&key, after, limit, through)
            .await?
            .into_iter()
            .map(|record| record.event)
            .collect())
    }
    pub fn sequence(&self, branch: &str) -> Result<u64> {
        Ok(self.published(branch)?.sequence())
    }
    pub fn state(&self, branch: &str) -> Result<BranchStatus> {
        self.published(branch)?.state()
    }
    /// Changes when [`Self::state`] may read differently.
    pub fn watch(&self, branch: &str) -> Result<watch::Receiver<()>> {
        Ok(self.published(branch)?.watch())
    }
    /// Dispatch progress of one branch: its dispatched sequence and failure.
    pub fn dispatch_watch(
        &self,
        branch: &str,
    ) -> Result<watch::Receiver<crate::core::DispatchState>> {
        Ok(self.published(branch)?.dispatch_watch())
    }
    pub fn branches(&self) -> Vec<TimelineInfo> {
        self.0.tree.list()
    }
    pub async fn branch(&self, parent: String, at: u64, options: BranchOptions) -> Result<String> {
        self.branch_with_admission(parent, at, options, CancellationToken::new())
            .await
    }
    /// Cancellation can reject admission, but never abandons a committed create.
    pub async fn branch_with_admission(
        &self,
        parent: String,
        at: u64,
        options: BranchOptions,
        admission: CancellationToken,
    ) -> Result<String> {
        if self.0.owner.closing.is_cancelled() {
            return Err(AgentError::Closed(self.id().into()));
        }
        self.published(&parent)?;
        Ok(self
            .0
            .tree
            .branch(parent, at, options, admission)
            .await?
            .id()
            .to_owned())
    }
    /// The Session scope and its capability plugins; branches are reported separately.
    pub fn diagnostics(&self) -> Result<rutis::RuntimeDiagnostics> {
        let loaded = self.0.owner.loaded()?;
        Ok(scope_diagnostics(
            &loaded.ctx,
            self.0.view.id,
            &loaded.children,
        ))
    }
    pub fn branch_diagnostics(&self, branch: &str) -> Result<rutis::RuntimeDiagnostics> {
        Ok(self.published(branch)?.diagnostics())
    }
    pub async fn trace(&self, branch: &str, sequence: u64) -> Result<Option<TimelineTrace>> {
        self.published(branch)?.trace(sequence).await
    }
    pub fn get<S: Clone + Send + Sync + 'static>(&self, branch: &str) -> Result<Option<S>> {
        Ok(self.published(branch)?.get::<S>())
    }
    /// Reads a capability service from the requested branch.
    pub fn service<T: ?Sized + BranchService + PublicService>(
        &self,
        branch: &str,
    ) -> Result<Arc<T>> {
        self.published(branch)?.service::<T>()
    }
    /// Observes publication progress without exposing Timeline mutation.
    pub fn publication(&self, branch: &str) -> Result<Publication> {
        Ok(self.published(branch)?.publication())
    }
    /// Reads a service from the currently loaded Session subtree.
    pub fn session_service<T: ?Sized + SessionService + PublicService>(&self) -> Result<Arc<T>> {
        let loaded = self.0.owner.loaded()?;
        loaded
            .ctx
            .get_as::<T>(loaded.scope.key::<T>())
            .ok_or_else(|| AgentError::Reloading(std::any::type_name::<T>().into()))
    }
    /// Reads one named service of the currently loaded Session subtree, such
    /// as the `sessionMethods` provider of a namespace. Like
    /// [`Self::session_service`], a missing key is `Reloading`: telling an
    /// absent name from one whose provider is not active is the caller's.
    pub fn session_named_service<T: ?Sized + SessionService + PublicService>(
        &self,
        name: &str,
    ) -> Result<Arc<T>> {
        let loaded = self.0.owner.loaded()?;
        loaded
            .ctx
            .get_as::<T>(loaded.scope.named_key::<T>(name))
            .ok_or_else(|| AgentError::Reloading(format!("{} {name}", std::any::type_name::<T>())))
    }
    /// The scope of the loaded Session subtree, as its plugin factories
    /// receive it, so a host can read the loader rows this Session runs.
    /// Keys under it stay readable only from inside the Session, so it opens
    /// no service beyond the [`PublicService`] reads above.
    pub fn scope(&self) -> Result<SessionScope> {
        Ok(self.0.owner.loaded()?.scope)
    }
    pub fn on<S: Clone + Send + Sync + 'static>(
        &self,
        branch: &str,
    ) -> Result<Option<watch::Receiver<S>>> {
        Ok(self.published(branch)?.on::<S>())
    }
    /// True when instance release preserves the stored Session for later use.
    pub fn is_unloading(&self) -> bool {
        self.0.owner.unloading.is_cancelled()
    }
    /// True from the moment close starts; the Session accepts no new input.
    pub fn is_closing(&self) -> bool {
        self.0.owner.closing.is_cancelled()
    }
    /// The runtime fault that stopped this Session, if any; separate from cleanup.
    pub fn failure(&self) -> Option<AgentError> {
        self.0.owner.faults.failure()
    }
    /// Observes complete Session cleanup without initiating it.
    pub fn closed(&self) -> BoxFuture<'static, Result<()>> {
        self.0.close.wait()
    }
    /// The host must exclude new uses while checking and releasing an instance.
    /// Idle release records no stop events and emits no business close notice.
    /// Every branch is checked while its commit lock is held; only when all
    /// pass and no [`SessionHold`] exists is each frozen at that tail, and it
    /// stays frozen until its dispatch stops so nothing slips past the release.
    /// A branch whose own close has finished has nothing left to release and
    /// is skipped; one still closing refuses the release.
    /// Finalizers commit afterwards. A refused release never freezes a branch,
    /// so appends only wait briefly; a hold held at the call refuses it
    /// without taking a lock.
    pub async fn unload_if_idle(&self) -> Result<bool> {
        let lifecycle = &self.0.owner.lifecycle;
        if self.is_closing()
            || self.failure().is_some()
            || self.0.owner.assembly.ephemeral_store.is_some()
            || lifecycle.held()
        {
            return Ok(false);
        }
        // Every branch is checked under its commit lock before any is frozen,
        // so a refused release never rejects a concurrent append.
        // This is the only path holding several commit locks at once; taking
        // them in id order keeps concurrent releases from deadlocking (ABBA).
        let mut branches = self.branches();
        branches.sort_by(|a, b| a.id.cmp(&b.id));
        let Some(handles) = branches
            .iter()
            .map(|info| self.0.tree.get(&info.id))
            .collect::<Option<Vec<_>>>()
        else {
            return Ok(false);
        };
        let Some(tails) = handles
            .iter()
            // A closed branch commits nothing more, so there is nothing to
            // check or freeze; Closed is final. Any other branch whose close
            // has started is refused by `lock_idle`.
            .filter(|branch| !branch.0.is_closed())
            .map(|branch| branch.0.lock_idle())
            .collect::<Option<Vec<_>>>()
        else {
            return Ok(false);
        };
        // Decided with every commit lock held: an owner drops its hold only
        // after its last fact is appended, so that fact cannot slip in between
        // the branch checks and this decision. Later holds get `None`.
        if !lifecycle.refuse_holds() {
            return Ok(false);
        }
        tails.into_iter().for_each(TailLock::freeze);
        self.0.owner.unloading.cancel();
        self.finish(true).await?;
        Ok(true)
    }
    pub fn close(&self) -> BoxFuture<'static, Result<()>> {
        self.finish(false)
    }
    fn finish(&self, unloading: bool) -> BoxFuture<'static, Result<()>> {
        self.0.owner.closing.cancel();
        self.0.owner.lifecycle.advance(SessionPhase::Closing);
        let this = self.clone();
        self.0.close.start(&self.0.handle, async move {
            let span = tracing::info_span!(
                "session.close",
                session_id = %this.id(),
                outcome = tracing::field::Empty,
            );
            let mut errors = vec![];
            this.0.tree.begin_close();
            if let Some(error) = this.0.owner.faults.failure() {
                for info in this.0.tree.list() {
                    if let Some(branch) = this.0.tree.get(&info.id) {
                        branch.0.mark_failure(&error);
                    }
                }
            }
            if unloading {
                for info in this.0.tree.list() {
                    if let Some(branch) = this.0.tree.get(&info.id)
                        && let Err(error) = branch.0.unload().await
                    {
                        errors.push(error);
                    }
                }
            }
            if let Err(error) = guarded("tree.close", async { this.0.tree.close().await }).await {
                errors.push(error);
            }
            let children = this
                .0
                .owner
                .loaded
                .lock()
                .unwrap()
                .as_ref()
                .map(|loaded| loaded.children.clone())
                .unwrap_or_default();
            if let Err(e) = shutdown_scope(&this.0.view, &children).await {
                errors.push(e);
            }
            this.0.owner.lifecycle.advance(SessionPhase::Closed);
            let supervisor = this.0.supervisor.lock().unwrap().take();
            if let Some(task) = supervisor
                && let Err(e) = task.await
            {
                errors.push(AgentError::fault("session supervisor", e.to_string()));
            }
            if let Some(agent) = this.0.agent.lock().unwrap().upgrade() {
                agent.entries.lock().unwrap().remove(this.id());
                if let Some(store) = &agent.assembly.ephemeral_store {
                    store.remove_session(this.id());
                }
            }
            *this.0.owner.loaded.lock().unwrap() = None;
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

fn extension<V: Send + Sync + 'static>(group: impl Into<String>, value: V) -> Extension {
    Extension {
        group: group.into(),
        configure: Box::new(move |create| create.with(value)),
    }
}

impl Drop for AgentInner {
    fn drop(&mut self) {
        self.closing.cancel();
        let closing: Vec<_> = self
            .entries
            .get_mut()
            .unwrap()
            .values()
            .filter_map(|entry| match entry {
                Entry::Ready(session) => Some(session.close()),
                Entry::Building(_) => None,
            })
            .collect();
        // What the Agent created for itself outlives its Sessions' business
        // cleanup, then closes.
        if let Some(placed) = self.placed.get()
            && let Some(owned) = placed.owned.lock().unwrap().take()
        {
            placed.placement.anchor.handle().clone().spawn(async move {
                for close in closing {
                    let _ = close.await;
                }
                let _ = release(owned).await;
            });
        }
    }
}

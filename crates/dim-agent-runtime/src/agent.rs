use crate::*;
use crate::{
    assembly::{
        Assembly, BuiltSession, Extension, Placement, SessionInitialization, build_session,
        check_scope, scope_diagnostics, shutdown_scope,
    },
    core::TailLock,
    lifecycle::{CloseOnce, guarded},
};
use dim_agent_types::{AgentEvent, BranchSource, InputMessageData, InputMode};
use rutis::host::{CtxHost, FiberView};
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
    /// The host's loader and the Agent instance Sessions go in.
    placement: Placement,
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
enum SessionStart {
    Create,
    Prepared(BoxFuture<'static, Result<()>>),
    Restore,
}
impl Agent {
    pub(crate) fn new(builder: AgentBuilder, assembly: Assembly, placement: Placement) -> Self {
        Self(Arc::new(AgentInner {
            builder,
            placement,
            handle: std::sync::OnceLock::new(),
            assembly,
            entries: Mutex::new(BTreeMap::new()),
            closing: CancellationToken::new(),
            close: CloseOnce::default(),
        }))
    }
    /// The service names this Agent was built with
    /// ([`AgentBuilder::catalog`]).
    pub fn catalog(&self) -> &rutis_loader::ServiceCatalog {
        self.0.builder.catalog()
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
        let session = self.create(id, config, SessionStart::Restore, None).await?;
        crate::memory::trim_if_large();
        Ok(session)
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
                let placement = owner.0.placement.clone();
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
            if let Err(failure) = result {
                let cleanup = if created {
                    owner.0.assembly.store.rollback_session(&id).await
                } else {
                    owner.0.assembly.store.release_session(&id).await
                };
                result = Err(match cleanup {
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
    pub fn diagnostics(&self) -> Option<rutis::host::diagnostics::RuntimeDiagnostics> {
        Some(self.0.placement.anchor.diagnostics())
    }
    /// Checks how the live assembly is written, over one diagnostics snapshot:
    /// for every Session and branch this Agent hosts, that its profile's
    /// required rows and the required views its scope handed, with the plugins
    /// below them, are `Active`, that no plugin
    /// reads an external service it does not declare, that every declared read
    /// is among the reader's resolved dependencies, and that no scoped service
    /// is provided without an instance key. Each plugin is checked once, by the
    /// innermost scope that contains it, and names the Session it belongs to
    /// and, for a branch plugin, the branch. It reports every finding rather
    /// than the first, and nothing for a sound assembly. The loader's rows are
    /// read once per call; each scope still walks one whole snapshot, so the
    /// cost grows with the square of the live Sessions.
    ///
    /// Creation does not run this. A test that wants the check creates the
    /// Session, branch or scope it wants covered, then calls this once; a
    /// plugin's own tests do the same for the assembly they build.
    ///
    /// A check that could not inspect a Session is an `Err`, never an empty
    /// list: a Session whose loaded assembly is already gone. `Ok` with no
    /// findings means every live scope was inspected and is sound.
    pub fn check_assembly(&self) -> Result<Vec<AssemblyFinding>> {
        let placement = &self.0.placement;
        let snapshot = placement.anchor.diagnostics();
        // Every Session of an Agent is placed by the same loader, so one pass
        // over its rows covers all of them.
        let rows = scope_rows(&placement.loader);
        let parents: std::collections::HashMap<_, _> = snapshot
            .plugins
            .iter()
            .map(|plugin| (plugin.id, plugin.parent))
            .collect();
        let mut findings = Vec::new();
        for session in self.sessions() {
            let owner = &session.0.owner;
            let loaded = owner.loaded().map_err(|_| {
                AgentError::fault(
                    "assembly check",
                    format!("Session {} has no loaded assembly", session.id()),
                )
            })?;
            let branches: Vec<_> = session
                .0
                .tree
                .list()
                .into_iter()
                .filter_map(|info| session.0.tree.get(&info.id))
                .collect();
            /// One scope of a Session: its view, the loader row ids its
            /// profile requires, the required views creation awaited and the
            /// branch.
            type Scope<'a> = (
                &'a FiberView,
                &'a [String],
                &'a [FiberView],
                Option<&'a str>,
            );
            let mut scopes: Vec<Scope> =
                vec![(&session.0.view, &loaded.required[..], &loaded.awaited, None)];
            for branch in &branches {
                scopes.push((
                    branch.0.scope_view(),
                    branch.0.required_rows(),
                    branch.0.awaited(),
                    Some(branch.id()),
                ));
            }
            for (view, required, parts, branch) in scopes.iter().copied() {
                let nested: Vec<FiberView> = scopes
                    .iter()
                    .filter(|(other, _, _, _)| {
                        other.id != view.id && below(&parents, other.id, view.id)
                    })
                    .map(|(other, _, _, _)| (*other).clone())
                    .collect();
                check_scope(
                    &snapshot,
                    &rows,
                    view,
                    required,
                    parts,
                    &nested,
                    (session.id(), branch),
                    &mut findings,
                );
            }
        }
        Ok(findings)
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
        // The close keeps the Agent alive until its Sessions are closed.
        let owner = self.clone();
        self.0.close.start(handle, async move {
            let _owner = owner;
            let mut errors = vec![];
            for mut result in pending {
                while result.borrow().is_none() {
                    if result.changed().await.is_err() {
                        break;
                    }
                }
                match result.borrow().clone() {
                    Some(Err(error)) => errors.extend(error.without_cancellation()),
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
            // A creation abandoned before its instance was found still
            // shuts that instance down; the close ends after it.
            let abandoned = &_owner.0.assembly.abandoned;
            abandoned.close();
            abandoned.wait().await;
            AgentError::aggregate(errors)
        })
    }
}
/// What uses a Session: an owner of accepted detached work holding it, or a
/// line with a turn running or work its dispatch has not reached.
/// [`Session::in_use`] reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InUse {
    /// A [`SessionHold`] exists.
    Held,
    /// A line has a turn running, or accepted work its dispatch has not
    /// reached. A line that failed or is closing has neither.
    Running,
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
        if self.0.tree.ended_head(id).is_some() {
            return Err(AgentError::BranchEnded(id.into()));
        }
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
        if let Some(error) = self.0.owner.faults.failure() {
            return Err(error);
        }
        if self.0.owner.closing.is_cancelled() {
            return Err(AgentError::Closed(self.id().into()));
        }
        self.published(branch)?.input(data).await
    }
    /// See [`Branch::lookup`].
    pub async fn lookup(&self, branch: &str, id: &str) -> Result<Option<Arc<AgentEvent>>> {
        self.published(branch)?.lookup(id).await
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
        let idle = self.published(branch)?.idle().await;
        if idle.is_ok() {
            crate::memory::trim_if_large();
        }
        idle
    }
    /// See [`Branch::query`].
    pub async fn query(&self, branch: &str, query: TimelineQuery) -> Result<TimelineQueryReply> {
        self.published(branch)?.query(query).await
    }
    /// See [`Branch::batch_end`].
    pub async fn batch_end(&self, branch: &str, sequence: u64) -> Result<()> {
        self.published(branch)?.batch_end(sequence).await
    }
    pub fn sequence(&self, branch: &str) -> Result<u64> {
        if let Some(head) = self.0.tree.ended_head(branch) {
            return Ok(head);
        }
        Ok(self.published(branch)?.sequence())
    }
    /// The fixed head of an ended line, `None` for an open or unknown one.
    pub fn ended_head(&self, branch: &str) -> Option<u64> {
        self.0.tree.ended_head(branch)
    }
    pub async fn end_branch(&self, branch: &str, source: BranchSource) -> Result<()> {
        if self.0.owner.closing.is_cancelled() {
            return Err(AgentError::Closed(self.id().into()));
        }
        self.0.tree.end(branch.to_owned(), source).await
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
    pub fn diagnostics(&self) -> Result<rutis::host::diagnostics::RuntimeDiagnostics> {
        let loaded = self.0.owner.loaded()?;
        Ok(scope_diagnostics(
            &loaded.ctx,
            self.0.view.id,
            &loaded.children,
        ))
    }
    pub fn branch_diagnostics(
        &self,
        branch: &str,
    ) -> Result<rutis::host::diagnostics::RuntimeDiagnostics> {
        Ok(self.published(branch)?.diagnostics())
    }
    pub fn get<S: Clone + Send + Sync + 'static>(&self, branch: &str) -> Result<Option<S>> {
        Ok(self.published(branch)?.get::<S>())
    }
    /// Reads a capability service from the requested branch.
    pub fn service<T: ?Sized + PublicService>(&self, branch: &str) -> Result<Arc<T>> {
        self.published(branch)?.service::<T>()
    }
    /// Observes publication progress without exposing Timeline mutation.
    pub fn publication(&self, branch: &str) -> Result<Publication> {
        Ok(self.published(branch)?.publication())
    }
    /// Reads a service of the Session group from the currently loaded
    /// Session subtree.
    pub fn session_service<T: ?Sized + PublicService>(&self) -> Result<Arc<T>> {
        crate::names::in_group::<T>(&self.0.owner.assembly.catalog, SESSION_GROUP)?;
        let loaded = self.0.owner.loaded()?;
        loaded
            .ctx
            .get_at::<T>(&loaded.scope.key::<T>())
            .ok_or_else(|| AgentError::Reloading(std::any::type_name::<T>().into()))
    }
    /// The instance of the loaded Session subtree, so a host can find the
    /// loader rows this Session runs. Keys under it stay readable only from
    /// inside the Session, so it opens no service beyond the
    /// [`PublicService`] reads above.
    pub fn instance(&self) -> Result<rutis::host::InstanceId> {
        Ok(self.0.owner.loaded()?.scope.instance)
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
    /// What uses this Session, or `None` when nothing does: a [`SessionHold`],
    /// or a line with a turn running or work its dispatch has not reached. A
    /// line that failed or is closing keeps nothing. A query that freezes,
    /// refuses and unloads nothing; [`Self::unload_if_idle`] takes its own,
    /// stricter reading under each branch's commit lock before it releases
    /// the instance.
    pub fn in_use(&self) -> Option<InUse> {
        if self.0.owner.lifecycle.held() {
            return Some(InUse::Held);
        }
        self.branches()
            .into_iter()
            .any(|info| {
                self.0
                    .tree
                    .get(&info.id)
                    .is_none_or(|branch| branch.0.is_running())
            })
            .then_some(InUse::Running)
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
            || self.in_use().is_some()
        {
            return Ok(false);
        }
        // Every branch is checked under its commit lock before any is frozen,
        // so a refused release never rejects a concurrent append.
        // This is the only path holding several commit locks at once; taking
        // them in id order keeps concurrent releases from deadlocking (ABBA,
        // idle-unload.md §6.1).
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
            .filter(|branch| !branch.0.is_closed())
            .map(|branch| branch.0.lock_idle())
            .collect::<Option<Vec<_>>>()
        else {
            return Ok(false);
        };
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
            let children = crate::assembly::with_copies(&children, &this.0.owner.copies);
            if let Err(e) = shutdown_scope(&this.0.view, &children).await {
                errors.push(e);
            }
            if let Err(error) = guarded("storage.release_session", async {
                this.0.owner.assembly.store.release_session(this.id()).await
            })
            .await
            {
                errors.push(error);
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
        // A Session's close starts when it is called; nothing waits for it
        // here.
        for entry in self.entries.get_mut().unwrap().values() {
            if let Entry::Ready(session) = entry {
                drop(session.close());
            }
        }
    }
}

/// Whether `ancestor` is above `id` in the snapshot's parent links.
fn below(
    parents: &std::collections::HashMap<rutis::host::PluginId, Option<rutis::host::PluginId>>,
    mut id: rutis::host::PluginId,
    ancestor: rutis::host::PluginId,
) -> bool {
    while let Some(Some(parent)) = parents.get(&id) {
        if *parent == ancestor {
            return true;
        }
        id = *parent;
    }
    false
}

use crate::{
    ServerConfig,
    agent_records::AgentRecords,
    protocol::{AgentInfo, SessionOrigin},
    settings::SettingsService,
    storage::{SessionStore, SqliteStorage},
};
use dim_agent_capabilities::host::ModelSelectionHost;
use dim_agent_runtime::{
    AGENT_GROUP, Agent, AgentBuilder, AgentError, CollaborationRole, InUse, Modules, Names,
    NoConfig, Session, SessionConfig, TimelineKey,
};
pub use dim_agent_types::AgentProfile;
use dim_agent_types::{ContentBlock, EventKind};
use rutis::host::{
    BoxFuture, CtxHost, DynPlugin, Effect, ErrorKind, FiberState, FiberView, Key, TypeKey,
};
use rutis::{Ctx, Error, Event, Plugin};
use rutis_bridge::session::{HostDispatch, host_key};
use rutis_loader::Loader;
use rutis_loader::edit::InstanceResult;
use rutis_loader::resolve::Build;
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    path::{Component, Path},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio_util::sync::CancellationToken;

mod configuration;
pub(crate) const SESSION_NAMESPACE: &str = "session.agent";

mod sessions;
pub use sessions::SessionPermissions;

/// The process default profile carries the process Session defaults.
fn process_default(defaults: &SessionConfig) -> AgentProfile {
    AgentProfile {
        name: "default".into(),
        description: String::new(),
        system_prompt: defaults.system_prompt.clone(),
        tools: defaults.tools.clone(),
        model: None,
    }
}
/// Session configuration of an Agent before explicit request values.
pub(crate) fn session_config(profile: &AgentProfile, defaults: &SessionConfig) -> SessionConfig {
    let mut config = defaults.clone();
    config.system_prompt = profile.system_prompt.clone();
    config.tools = profile.tools.clone();
    if let Some(model) = &profile.model {
        config.model = dim_agent_types::ModelSelection {
            provider_id: model.provider_id.clone(),
            model_id: model.model_id.clone(),
            capability_snapshot: None,
            config: None,
            metadata: None,
        };
    }
    config
}

/// One protocol instance: the Agent and its Sessions.
/// Agents are peers: hosting knows no relationship between them.
pub struct AgentHost {
    pub agent: Agent,
    pub agent_id: String,
    pub default: bool,
    pub profile: AgentProfile,
    /// The leader Session whose Team may address this Agent; `None` makes it
    /// public. Stored with the Agent and restored with it.
    pub owner: Option<String>,
    /// Host of the Agent's Session listeners. It unloads after the Agent scope
    /// has closed every Session.
    pub(crate) ctx: Ctx,
}

/// A new loaded incarnation of a stored Session, before its caller receives it.
/// Listeners resolve the Session through [`AgentIndex::session`]; an event
/// carries ids, never a handle.
#[derive(serde::Serialize, serde::Deserialize)]
pub(crate) struct SessionRestored {
    pub agent_id: String,
    pub session_id: String,
}
impl Event for SessionRestored {
    const NAME: &'static str = "agent/session-restored";
    type Value = ();
}
/// The end of a stored Session that `session.delete` removed while it was
/// not loaded, so no close announced it. A loaded Session ends through its
/// close instead. Emitted on the process root: a delete can outlive its
/// index generation.
#[derive(serde::Serialize, serde::Deserialize)]
pub(crate) struct UnloadedSessionDeleted {
    pub agent_id: String,
    pub session_id: String,
}
impl Event for UnloadedSessionDeleted {
    const NAME: &'static str = "agent/unloaded-session-deleted";
    type Value = ();
}

/// Agent-scoped operations fail before any Session is involved.
#[derive(Debug, thiserror::Error)]
pub enum AgentOpError {
    #[error("unknown agent: {0}")]
    Unknown(String),
    #[error("agent initialization failed: {0}")]
    Build(#[from] AgentError),
    #[error("agent settlement failed: {0}")]
    Settle(AgentError),
    #[error("agent unload failed: {0}")]
    Unload(Arc<ErrorKind>),
}

/// A Session's admission state ([idle unload §6.4](../../../docs/design/appserver/idle-unload.md#64-admission-slots)).
/// Every holder took its reference from the index map, under the index
/// lock, or cloned one it held. With a native store a slot leaves the map
/// only under that lock once nothing else holds it ([`Inner::sweep`]),
/// so the slot a holder has is the one in the map: callers of one Session
/// wait on one gate, and the pass sees every lease. Without one, nothing
/// restores a Session and no pass runs; `remove_version` and
/// `retain_versions` drop slots as Sessions end.
#[derive(Default)]
struct SessionSlot {
    gate: tokio::sync::Mutex<()>,
    uses: AtomicUsize,
    /// Incremented by every `acquire`: while it stays unchanged and `uses`
    /// stays 0, no lease has held the Session (idle unload §6.1).
    acquired: AtomicU64,
    /// Set by `session.close` and by a delete's close: the Session does not
    /// load again in this process (S5c D5).
    closed: AtomicBool,
    /// Set with `closed` by a delete that leaves no rows of the Session: no
    /// close of it must outlive the slot.
    deleted: AtomicBool,
}
/// An accepted request or subscription pins one loaded instance.
pub(crate) struct SessionUse {
    pub host: Arc<AgentHost>,
    pub session: Session,
    slot: Arc<SessionSlot>,
}
impl Clone for SessionUse {
    fn clone(&self) -> Self {
        self.slot.uses.fetch_add(1, Ordering::AcqRel);
        Self {
            host: self.host.clone(),
            session: self.session.clone(),
            slot: self.slot.clone(),
        }
    }
}
impl Drop for SessionUse {
    fn drop(&mut self) {
        self.slot.uses.fetch_sub(1, Ordering::AcqRel);
    }
}

/// What a delete that requires an inactive Session answers
/// ([S5c §3.5](../../../docs/design/appserver/session-deletion-and-fork.md#35-idempotence-and-errors)):
/// it removed the Session, or it refused because something uses it. The App
/// Server owns this answer; the runtime's errors do not carry it.
pub(crate) enum DeleteOutcome {
    /// The call removed the Session's rows, or a loaded Session.
    Removed(bool),
    /// Nothing was removed; this says what uses the Session.
    InUse(String),
}

struct Entry {
    host: Arc<AgentHost>,
    view: FiberView,
}
#[derive(Default)]
struct Inner {
    agents: BTreeMap<String, Entry>,
    sessions: HashMap<String, Arc<SessionSlot>>,
    unavailable_agents: HashMap<String, String>,
    unavailable_sessions: HashMap<String, String>,
    /// Registered by an unloading scope apply; completed by its loader.
    pending: HashMap<String, Arc<AgentHost>>,
    /// Agents removed from the live index whose settlement still runs.
    closing: HashMap<String, Arc<AgentHost>>,
    /// Session id -> owning agent id and negotiated protocol version.
    versions: HashMap<String, (String, u32)>,
    /// Loader instances of the Agents this index created, until they close.
    /// They outlive the index's entries: an unloading scope forgets its
    /// entry, and the index closes the instance when it unloads.
    instances: HashMap<String, FiberView>,
}

impl Inner {
    /// Whether Session `id` was closed or deleted in this process. A lookup:
    /// reading the fact creates no slot.
    fn closed(&self, id: &str) -> bool {
        self.sessions
            .get(id)
            .is_some_and(|slot| slot.closed.load(Ordering::Acquire))
    }
    /// How many uses Session `id` has. A lookup: reading the count creates
    /// no slot.
    fn uses(&self, id: &str) -> usize {
        self.sessions
            .get(id)
            .map_or(0, |slot| slot.uses.load(Ordering::Acquire))
    }
    /// The Agent owning Session `id`: its recorded owner, live or closing,
    /// or the live Agent that has it loaded.
    fn owner(&self, id: &str) -> Option<Arc<AgentHost>> {
        if let Some((agent, _)) = self.versions.get(id) {
            if let Some(entry) = self.agents.get(agent) {
                return Some(entry.host.clone());
            }
            if let Some(host) = self.closing.get(agent) {
                return Some(host.clone());
            }
        }
        self.agents
            .values()
            .find(|entry| entry.host.agent.get_session(id).is_some())
            .map(|entry| entry.host.clone())
    }
    /// Removes every slot nothing needs: nothing but the map holds it,
    /// neither a caller, a lease nor an idle unload mark; its Session is not
    /// loaded; and it records no close the process must keep, so its Session
    /// is open, or deleted with its rows. A closed Session whose rows remain
    /// keeps its slot (S5c D5). The count is read under the index lock,
    /// where every new reference is taken, so it cannot rise meanwhile.
    fn sweep(&mut self) {
        let Inner {
            sessions,
            agents,
            closing,
            ..
        } = self;
        sessions.retain(|id, slot| {
            Arc::strong_count(slot) > 1
                || (slot.closed.load(Ordering::Acquire) && !slot.deleted.load(Ordering::Acquire))
                || agents
                    .values()
                    .map(|entry| &entry.host)
                    .chain(closing.values())
                    .any(|host| host.agent.get_session(id).is_some())
        });
    }
}

/// Process service routing every Agent: creation, addressing and close.
/// Relationships between Agents are not hosting concerns; whoever needs them
/// (a subagent capability, a workflow) tracks its own edges and policies.
/// The process's Agents and the state kept for their Sessions. It has no
/// dependencies and is not restartable: Agents register here, so they and
/// this state outlive a reload of the `Agents` directory logic.
pub(crate) struct AgentDirectory {
    inner: Mutex<Inner>,
    /// The first generation that loads a store removes unfinished delegations;
    /// later generations may run while a creation is in flight.
    finished: AtomicBool,
    default_agent_id: OnceLock<String>,
    /// The protocol instance: it stays while the Agents and their Sessions
    /// do, so clients of an `Agents` reload keep their Sessions.
    server_instance_id: String,
}
impl AgentDirectory {
    /// Registration of a scope that finished apply. A first load completes
    /// the entry with the instance it created; a scope that applies again
    /// in an existing instance enters directly.
    fn register(&self, host: Arc<AgentHost>) {
        let mut inner = self.inner.lock().expect("agent index");
        match inner.instances.get(&host.agent_id).cloned() {
            Some(view) if !inner.pending.contains_key(&host.agent_id) => {
                inner
                    .agents
                    .insert(host.agent_id.clone(), Entry { host, view });
            }
            _ => {
                inner.pending.insert(host.agent_id.clone(), host);
            }
        }
    }
    /// Drops an Agent and its Session protocol versions; idempotent, so
    /// explicit close and scope unload agree.
    fn forget(&self, agent_id: &str) {
        let mut inner = self.inner.lock().expect("agent index");
        inner.agents.remove(agent_id);
        inner.pending.remove(agent_id);
        inner.versions.retain(|_, (owner, _)| owner != agent_id);
    }
}
/// Provides the [`AgentDirectory`] and the `sessions` service. Neither
/// reloads, so consumers of `sessions` never reload with the index.
pub(crate) struct AgentDirectoryPlugin;
impl Plugin for AgentDirectoryPlugin {
    type Deps = ();
    fn name(&self) -> &str {
        "AgentDirectory"
    }
    async fn apply(&self, ctx: &Ctx, (): Self::Deps) -> rutis::Result {
        ctx.provide_as::<AgentDirectory>(Arc::new(AgentDirectory {
            inner: Mutex::new(Inner::default()),
            finished: AtomicBool::new(false),
            default_agent_id: OnceLock::new(),
            server_instance_id: uuid::Uuid::new_v4().to_string(),
        }))?;
        ctx.provide_at::<dyn HostDispatch>(
            host_key("sessions"),
            Arc::new(sessions::SessionsService(ctx.clone())),
        )?;
        ctx.provide_as::<SessionPermissions>(Arc::new(SessionPermissions(ctx.clone())))?;
        Ok(())
    }
}

/// Directory logic and Agent management over the [`AgentDirectory`]:
/// lookups, Session admission and creating and closing Agents through the
/// loader. A reload of it is a reload of the directory logic only.
pub struct AgentIndex {
    directory: Arc<AgentDirectory>,
    builder: AgentBuilder,
    session_defaults: SessionConfig,
    /// Protected profiles resolve Session models through a `ModelSelectionHost`.
    selects_models: bool,
    /// Loading context of `AgentsPlugin`.
    ctx: OnceLock<Ctx>,
    /// The process loader Agents are instances of, and the root they are
    /// created from.
    loader: OnceLock<(Loader, Ctx)>,
    startup_timeout: Duration,
    records: Option<Arc<AgentRecords>>,
    session_store: Option<Arc<SessionStore>>,
    settings: Option<Arc<SettingsService>>,
    stopping: CancellationToken,
    /// Notified once an `idle` call on this generation is admitted and
    /// waiting on its branch. Feature `testing` only.
    #[cfg(feature = "testing")]
    idle_admitted: Arc<tokio::sync::Notify>,
}
impl AgentIndex {
    /// Whether this profile resolves Session models through a host.
    pub(crate) fn selects_models(&self) -> bool {
        self.selects_models
    }
    /// The model selection host, read per call rather than depended on: in
    /// the product profile Provider supplies it, and an inactive or reloading
    /// management owner must not keep Agents, and so Rpc, from loading
    /// (methods contract §3.2). `None` while its provider is not active.
    pub(crate) fn model_selection(&self) -> Option<Arc<ModelSelectionHost>> {
        self.selects_models
            .then(|| self.ctx.get()?.get::<ModelSelectionHost>())
            .flatten()
    }
    const SUMMARY_SCAN_LIMIT: usize = 256;
    const SUMMARY_PREVIEW_CHARS: usize = 120;
    /// Process Session defaults that Agent profiles apply on top of.
    pub fn session_defaults(&self) -> &SessionConfig {
        &self.session_defaults
    }
    pub fn server_instance_id(&self) -> &str {
        &self.directory.server_instance_id
    }
    pub fn default_agent_id(&self) -> &str {
        self.directory
            .default_agent_id
            .get()
            .expect("AgentsPlugin apply fixed the default id")
    }
    /// The signal an `idle` admission notifies, so a test knows the wait is
    /// in flight before it reloads or closes. Feature `testing` only.
    #[cfg(feature = "testing")]
    pub(crate) fn idle_admitted(&self) -> Arc<tokio::sync::Notify> {
        self.idle_admitted.clone()
    }
    pub fn default_host(&self) -> Option<Arc<AgentHost>> {
        self.host(Some(self.default_agent_id()))
    }
    pub fn hosts(&self) -> Vec<Arc<AgentHost>> {
        self.directory
            .inner
            .lock()
            .expect("agent index")
            .agents
            .values()
            .map(|entry| entry.host.clone())
            .collect()
    }
    pub fn host(&self, agent_id: Option<&str>) -> Option<Arc<AgentHost>> {
        let id =
            agent_id.or_else(|| self.directory.default_agent_id.get().map(|id| id.as_str()))?;
        self.directory
            .inner
            .lock()
            .expect("agent index")
            .agents
            .get(id)
            .map(|entry| entry.host.clone())
    }
    /// The owner of a loaded Agent: `None` for an unknown Agent,
    /// `Some(None)` for a public one, `Some(Some(leader))` for an owned one.
    pub(crate) fn owner(&self, agent_id: &str) -> Option<Option<String>> {
        self.directory
            .inner
            .lock()
            .expect("agent index")
            .agents
            .get(agent_id)
            .map(|entry| entry.host.owner.clone())
    }
    /// The leader of every stored member Session; `None` when the process has
    /// no store, so the relations it holds are all there is.
    pub(crate) async fn members(&self) -> dim_agent_runtime::Result<Option<Vec<(String, String)>>> {
        match &self.records {
            Some(records) => records.members().await.map(Some),
            None => Ok(None),
        }
    }
    pub(crate) fn unavailable_agent(&self, id: &str) -> Option<String> {
        self.directory
            .inner
            .lock()
            .expect("agent index")
            .unavailable_agents
            .get(id)
            .cloned()
    }
    pub(crate) fn unavailable_session(&self, id: &str) -> Option<String> {
        self.directory
            .inner
            .lock()
            .expect("agent index")
            .unavailable_sessions
            .get(id)
            .cloned()
    }
    /// Session ids are UUIDs, unique across every Agent of the process.
    pub fn session(&self, session_id: &str) -> Option<(Arc<AgentHost>, Session)> {
        let inner = self.directory.inner.lock().expect("agent index");
        for entry in inner.agents.values() {
            if let Some(session) = entry.host.agent.get_session(session_id) {
                return Some((entry.host.clone(), session));
            }
        }
        None
    }
    /// The Agent owning a Session, also after the Session itself closed and
    /// while its Agent is closing; its archives may still be finishing.
    pub fn host_by_session(&self, session_id: &str) -> Option<Arc<AgentHost>> {
        self.directory
            .inner
            .lock()
            .expect("agent index")
            .owner(session_id)
    }
    pub fn version_of(&self, session_id: &str) -> Option<u32> {
        self.directory
            .inner
            .lock()
            .expect("agent index")
            .versions
            .get(session_id)
            .map(|(_, version)| *version)
    }
    pub fn insert_version(&self, session_id: &str, agent_id: &str, version: u32) {
        self.directory
            .inner
            .lock()
            .expect("agent index")
            .versions
            .insert(session_id.to_owned(), (agent_id.to_owned(), version));
    }
    pub fn remove_version(&self, session_id: &str) {
        if self.session_store.is_some() {
            if let Some((_, slot)) = self.owned_slot(session_id) {
                slot.closed.store(true, Ordering::Release);
            }
            return;
        }
        self.slot(session_id).closed.store(true, Ordering::Release);
        let mut inner = self.directory.inner.lock().unwrap();
        inner.versions.remove(session_id);
        inner.sessions.remove(session_id);
    }
    /// Sessions can also close through the runtime fault supervisor.
    /// Versions of a closing Agent stay until its settlement forgets them.
    pub fn retain_versions(&self) {
        if self.session_store.is_some() {
            return;
        }
        let hosts = self.hosts();
        let mut inner = self.directory.inner.lock().expect("agent index");
        let closing = inner.closing.clone();
        let Inner {
            versions,
            unavailable_sessions,
            sessions,
            ..
        } = &mut *inner;
        versions.retain(|session, (agent, _)| {
            unavailable_sessions.contains_key(session)
                || closing.contains_key(agent)
                || hosts.iter().any(|host| {
                    host.agent_id == *agent && host.agent.get_session(session).is_some()
                })
        });
        sessions.retain(|session, _| versions.contains_key(session));
    }
    pub fn session_count(&self) -> usize {
        self.hosts()
            .iter()
            .map(|host| host.agent.sessions().len())
            .sum()
    }
    /// The public directory: the default Agent and the Agents
    /// `agent.create` made. An owned Agent belongs to one Team and is reached
    /// through Collaboration's tool.
    pub fn list(&self) -> Vec<AgentInfo> {
        self.hosts()
            .into_iter()
            .filter(|host| host.owner.is_none())
            .map(|host| AgentInfo {
                agent_id: host.agent_id.clone(),
                default: host.default,
                name: host.profile.name.clone(),
                description: host.profile.description.clone(),
                sessions: host.agent.sessions().len(),
            })
            .collect()
    }

    /// Loads the default Agent, or the stored Agents, and the stored Session
    /// owners. Each `AgentsPlugin` generation calls it before publishing the
    /// index. Agents an earlier generation loaded are still running and stay
    /// as they are.
    async fn install_default(self: &Arc<Self>) -> Result<(), AgentOpError> {
        let agent_id = self.default_agent_id().to_owned();
        if let Some(records) = &self.records
            && !self.directory.finished.swap(true, Ordering::SeqCst)
        {
            self.remove_unfinished(records).await;
        }
        let stored = match &self.records {
            Some(records) => records.agents().await?,
            None => vec![],
        };
        let owners = match &self.records {
            Some(records) => records.owners().await?,
            None => vec![],
        };
        if stored.is_empty() {
            if !self.running(&agent_id) {
                self.load(
                    agent_id,
                    true,
                    process_default(&self.session_defaults),
                    None,
                )
                .await?;
            }
        } else {
            for record in stored
                .into_iter()
                .filter(|record| !self.running(&record.id))
            {
                let profile = record
                    .profile
                    .and_then(|profile| self.check_profile(&profile).map(|_| profile))
                    .map(|profile| match record.default {
                        true => process_default(&self.session_defaults),
                        false => profile,
                    });
                let loaded = match profile {
                    Ok(profile) => self
                        .load(
                            record.id.clone(),
                            record.default,
                            profile,
                            record.owner.clone(),
                        )
                        .await
                        .map(|_| ())
                        .map_err(|error| error.to_string()),
                    Err(error) => Err(error),
                };
                if let Err(error) = loaded {
                    tracing::error!(agent_id=%record.id, %error, "Stored Agent is unavailable");
                    self.directory
                        .inner
                        .lock()
                        .expect("agent index")
                        .unavailable_agents
                        .insert(record.id, error);
                }
            }
        }
        for owner in owners {
            if let Some((agent_id, version)) = &owner.owner {
                self.insert_version(&owner.session_id, agent_id, *version);
            }
            if let Some(error) = owner.error {
                tracing::error!(session_id=%owner.session_id, %error, "Invalid Session ownership");
                self.directory
                    .inner
                    .lock()
                    .unwrap()
                    .unavailable_sessions
                    .insert(owner.session_id, error);
            }
        }
        Ok(())
    }

    /// An instance of the Agent exists, created by this or an earlier
    /// generation.
    fn running(&self, agent_id: &str) -> bool {
        self.directory
            .inner
            .lock()
            .expect("agent index")
            .instances
            .contains_key(agent_id)
    }
    /// See [`Inner::closed`].
    pub(crate) fn closed(&self, id: &str) -> bool {
        self.directory.inner.lock().expect("agent index").closed(id)
    }
    pub(crate) fn session_available(&self, id: &str) -> bool {
        self.host_by_session(id).is_some() && !self.closed(id)
    }
    pub(crate) async fn read_page(
        &self,
        id: &str,
        after: u64,
        limit: usize,
        through: Option<u64>,
    ) -> dim_agent_runtime::Result<(u64, Vec<Arc<dim_agent_types::AgentEvent>>)> {
        if let Some(store) = &self.session_store {
            let head = match self
                .session(id)
                .map(|(_, session)| (session.sequence("main"), session))
            {
                Some((Ok(head), _)) => head,
                Some((Err(error), session)) if !session.is_unloading() => return Err(error),
                // Not loaded, or released under the read: the store holds
                // every event it published (idle unload D8).
                _ => {
                    store
                        .timeline_store()
                        .head(&dim_agent_runtime::TimelineKey {
                            session_id: id.into(),
                            branch_id: "main".into(),
                        })
                        .await?
                }
            };
            let through = through.unwrap_or(head);
            if through > head {
                return Err(AgentError::InvalidInput("future Timeline sequence".into()));
            }
            return Ok((
                through,
                store
                    .read_page_without_request_bodies(id, after, limit, through)
                    .await?,
            ));
        }
        let (_, session) = self
            .session(id)
            .ok_or_else(|| AgentError::Closed(id.into()))?;
        let through = through.unwrap_or(session.sequence("main")?);
        Ok((
            through,
            session.local_page("main", after, limit, through).await?,
        ))
    }
    /// The stored events of `event_ids` in one read; ids the store does not
    /// hold are absent.
    pub(crate) async fn read_events(
        &self,
        id: &str,
        event_ids: &[String],
    ) -> dim_agent_runtime::Result<HashMap<String, Arc<dim_agent_types::AgentEvent>>> {
        if let Some(store) = &self.session_store {
            return store.read_events(id, event_ids).await;
        }
        let (_, session) = self
            .session(id)
            .ok_or_else(|| AgentError::Closed(id.into()))?;
        Ok(session
            .history("main", None)
            .await?
            .into_iter()
            .filter(|event| event_ids.contains(&event.id))
            .map(|event| (event.id.clone(), event))
            .collect())
    }
    /// The main-branch `step/tool-use-start` of each `(request_id, call_id)`
    /// in one read; calls the store does not hold are absent.
    pub(crate) async fn read_tool_use_starts(
        &self,
        id: &str,
        keys: &[(String, String)],
    ) -> dim_agent_runtime::Result<HashMap<(String, String), Arc<dim_agent_types::AgentEvent>>>
    {
        if let Some(store) = &self.session_store {
            return store.read_tool_use_starts(id, keys).await;
        }
        let (_, session) = self
            .session(id)
            .ok_or_else(|| AgentError::Closed(id.into()))?;
        let mut starts = HashMap::with_capacity(keys.len());
        for event in session.history("main", None).await? {
            let key = match &event.kind {
                EventKind::ToolUseStart(start) => (start.request_id.clone(), start.call_id.clone()),
                _ => continue,
            };
            if keys.contains(&key) {
                starts.insert(key, event);
            }
        }
        Ok(starts)
    }
    /// The error a history window answers without the native Session store.
    fn no_history_store() -> AgentError {
        AgentError::InvalidInput("history windows require the native Session store".into())
    }
    /// Whether this index has the native Session store the history window
    /// reads (session-history-window §5.3).
    pub(crate) fn has_session_store(&self) -> bool {
        self.session_store.is_some()
    }
    /// The newest turn at `through` and the window's first sequence.
    pub(crate) async fn turn_window(
        &self,
        id: &str,
        branch: &str,
        through: u64,
    ) -> dim_agent_runtime::Result<crate::storage::TurnWindow> {
        self.session_store
            .as_ref()
            .ok_or_else(Self::no_history_store)?
            .turn_window(id, branch, through)
            .await
    }
    /// The newest events below `before` in `window_start..=through`, whole
    /// batches at both ends.
    pub(crate) async fn read_page_before(
        &self,
        id: &str,
        branch: &str,
        before: u64,
        limit: usize,
        through: u64,
        window_start: u64,
    ) -> dim_agent_runtime::Result<Vec<dim_agent_runtime::StoredEvent>> {
        self.session_store
            .as_ref()
            .ok_or_else(Self::no_history_store)?
            .read_page_before(id, branch, before, limit, through, window_start)
            .await
    }
    fn slot(&self, id: &str) -> Arc<SessionSlot> {
        self.directory
            .inner
            .lock()
            .unwrap()
            .sessions
            .entry(id.to_owned())
            .or_default()
            .clone()
    }
    /// Session `id`'s owner and its slot, created if needed, in one step
    /// under the index lock. A delete forgets the owner and drops the slot
    /// under that lock too, so a deleted Session never gets a new, open
    /// slot: it stays `session_closed` rather than being restored.
    fn owned_slot(&self, id: &str) -> Option<(Arc<AgentHost>, Arc<SessionSlot>)> {
        let mut inner = self.directory.inner.lock().expect("agent index");
        let host = inner.owner(id)?;
        let slot = inner.sessions.entry(id.to_owned()).or_default().clone();
        Some((host, slot))
    }
    pub(crate) async fn acquire(&self, id: &str) -> dim_agent_runtime::Result<SessionUse> {
        if let Some(reason) = self.unavailable_session(id) {
            return Err(AgentError::Storage(reason));
        }
        let Some((host, slot)) = self.owned_slot(id) else {
            if let Some(store) = &self.session_store
                && store.has_records(id).await?
            {
                return Err(AgentError::Storage(format!(
                    "Session {id} has no available owner"
                )));
            }
            return Err(AgentError::Closed(id.into()));
        };
        let _gate = slot.gate.lock().await;
        if slot.closed.load(Ordering::Acquire) || self.stopping.is_cancelled() {
            return Err(AgentError::Closed(id.into()));
        }
        let session = match host.agent.get_session(id) {
            Some(session) => session,
            None if self.session_store.is_some() => {
                if let Some(settings) = &self.settings {
                    let config = self
                        .session_store
                        .as_ref()
                        .unwrap()
                        .timeline_store()
                        .session_config(id)
                        .await?
                        .ok_or_else(|| AgentError::Storage(format!("unknown session {id}")))?;
                    settings
                        .authorize_project(config.cwd.into(), &self.stopping)
                        .await
                        .map_err(|error| {
                            AgentError::fault("project authorization", error.to_string())
                        })?;
                }
                let session = host.agent.restore_session(id.into()).await?;
                let ctx = self.ctx.get().expect("AgentsPlugin apply sets its context");
                ctx.parallel(SessionRestored {
                    agent_id: host.agent_id.clone(),
                    session_id: id.to_owned(),
                })
                .await?;
                session
            }
            None => return Err(AgentError::Closed(id.into())),
        };
        slot.uses.fetch_add(1, Ordering::AcqRel);
        slot.acquired.fetch_add(1, Ordering::AcqRel);
        Ok(SessionUse {
            host,
            session,
            slot: slot.clone(),
        })
    }
    /// One pass of idle unload ([§6.1](../../../docs/design/appserver/idle-unload.md#61-the-pass)).
    /// `marks` holds the slot and acquire count of each Session the previous
    /// pass found lease-free. One still lease-free with that count has been
    /// for at least the grace period, and is released under its admission
    /// gate, which the pass never waits for. The pass then drops the slots
    /// that nothing holds any more.
    async fn unload_idle(&self, marks: &mut HashMap<String, (Arc<SessionSlot>, u64)>) {
        let mut next = HashMap::new();
        for host in self.hosts() {
            for session in host.agent.sessions() {
                if self.stopping.is_cancelled() {
                    return;
                }
                let id = session.id();
                let Some(slot) = self
                    .directory
                    .inner
                    .lock()
                    .expect("agent index")
                    .sessions
                    .get(id)
                    .cloned()
                else {
                    continue;
                };
                let acquired = slot.acquired.load(Ordering::Acquire);
                if slot.closed.load(Ordering::Acquire) || slot.uses.load(Ordering::Acquire) > 0 {
                    continue;
                }
                if marks.get(id).is_some_and(|(_, marked)| *marked == acquired)
                    && let Ok(_gate) = slot.gate.try_lock()
                    && slot.acquired.load(Ordering::Acquire) == acquired
                {
                    match session.unload_if_idle().await {
                        Ok(true) => {
                            tracing::debug!(session_id = id, "Idle Session unloaded");
                            continue;
                        }
                        Ok(false) => {}
                        Err(error) => {
                            tracing::warn!(session_id = id, %error, "Idle Session unload failed");
                            continue;
                        }
                    }
                }
                next.insert(id.to_owned(), (slot, acquired));
            }
        }
        *marks = next;
        self.directory.inner.lock().expect("agent index").sweep();
    }

    pub(crate) async fn session_catalog(
        &self,
        request: crate::protocol::Catalog,
        agent: Option<String>,
        version: u32,
        include_closed: bool,
        cancel: CancellationToken,
    ) -> dim_agent_runtime::Result<crate::protocol::CatalogResult> {
        let store = self
            .session_store
            .as_ref()
            .ok_or_else(|| AgentError::InvalidInput("a catalog needs the native store".into()))?;
        let hosts = self
            .hosts()
            .into_iter()
            .map(|host| host.agent_id.clone())
            .collect();
        let closed: HashSet<String> = self
            .directory
            .inner
            .lock()
            .expect("agent index")
            .sessions
            .iter()
            .filter(|(_, slot)| slot.closed.load(Ordering::Acquire))
            .map(|(id, _)| id.clone())
            .collect();
        let excluded: Vec<String> = if include_closed {
            Vec::new()
        } else {
            closed.iter().cloned().collect()
        };
        let mut page = store
            .catalog(request, agent, version, hosts, excluded, cancel)
            .await?;
        for summary in &mut page.sessions {
            summary.closed = closed.contains(&summary.session.session_id);
            if let Some((_, session)) = self.session(&summary.session.session_id) {
                match session
                    .state("main")
                    .and_then(|state| Ok((state.running, session.sequence("main")?)))
                {
                    Ok((running, sequence)) => {
                        summary.session.running = running;
                        summary.session.sequence = sequence;
                    }
                    Err(_) if session.is_unloading() => {}
                    Err(error) => {
                        tracing::warn!(session_id=%summary.session.session_id, %error, "Session state was not read for the catalog");
                    }
                }
            }
        }
        Ok(page)
    }

    /// The directory record of one Session whose lease the caller holds, with
    /// the fields a session summary carries. It reads this Session's title
    /// row and no other Session or history; the caller fills
    /// `attached_clients`.
    ///
    /// Opening a Session must survive the reloads a catalog read fails on. A
    /// title that cannot be read now leaves the title fields absent, and a
    /// branch whose Loop is being replaced reports not running; both reach an
    /// attached client as notices.
    pub(crate) async fn session_record(
        &self,
        host: &AgentHost,
        session: &Session,
        version: u32,
    ) -> dim_agent_runtime::Result<crate::protocol::SessionInfo> {
        let title = match (&self.session_store, version >= 3) {
            (_, false) => None,
            (Some(store), true) => Some(match store.metadata(session.id()).await {
                Ok(metadata) => metadata.into(),
                Err(_) => crate::storage::TitleFields::default(),
            }),
            (None, true) => Some(crate::storage::TitleFields::default()),
        };
        let (origin, parent_session_id) = match (&self.session_store, version >= 3) {
            (Some(store), true) => match store.creation_record(session.id()).await {
                Ok((origin, parent)) => (Some(origin), parent),
                Err(_) => (None, None),
            },
            _ => (None, None),
        };
        Ok(crate::protocol::SessionInfo {
            session_id: session.id().to_owned(),
            cwd: session.config().cwd.clone(),
            model: dim_agent_types::ModelId {
                provider_id: session.config().model.provider_id.clone(),
                model_id: session.config().model.model_id.clone(),
            },
            created_at: session.created_at(),
            running: session.state("main").is_ok_and(|state| state.running),
            sequence: session.sequence("main")?,
            attached_clients: 0,
            agent_id: (version >= 3).then(|| host.agent_id.clone()),
            origin,
            parent_session_id,
            title,
        })
    }

    pub(crate) async fn session_summary(
        &self,
        id: &str,
        through: u64,
        created_at: u64,
    ) -> dim_agent_runtime::Result<(Option<String>, u64)> {
        let loaded = match self.session(id) {
            Some((_, session)) => match async {
                let events = session
                    .local_page("main", 0, Self::SUMMARY_SCAN_LIMIT, through)
                    .await?;
                let last = session.local("main", through..=through).await?.pop();
                Ok::<_, AgentError>((events, last))
            }
            .await
            {
                Ok(read) => Some(read),
                // Released under the read: the store holds every event it
                // published (idle unload D8).
                Err(_) if session.is_unloading() => None,
                Err(error) => return Err(error),
            },
            None => None,
        };
        let (events, last) = if let Some(read) = loaded {
            read
        } else {
            let store = self
                .session_store
                .as_ref()
                .ok_or_else(|| AgentError::Closed(id.into()))?;
            let events = store
                .read_page_without_request_bodies(id, 0, Self::SUMMARY_SCAN_LIMIT, through)
                .await?;
            let last = store
                .timeline_store()
                .read(
                    &TimelineKey {
                        session_id: id.into(),
                        branch_id: "main".into(),
                    },
                    through..=through,
                )
                .await?
                .pop()
                .map(|record| record.event);
            (events, last)
        };
        let preview = events
            .iter()
            .find_map(|event| {
                let EventKind::InputMessage(data) = &event.kind else {
                    return None;
                };
                data.user.as_ref()
            })
            .and_then(|user| {
                let text = user
                    .content
                    .iter()
                    .filter_map(|block| {
                        if let ContentBlock::Text { text } = block {
                            Some(text.as_str())
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                (!text.is_empty())
                    .then(|| crate::storage::truncate_preview(&text, Self::SUMMARY_PREVIEW_CHARS))
            });
        let updated_at = last.map_or(created_at, |event| event.create_at);
        Ok((preview, updated_at))
    }

    pub(crate) async fn prepare_session(
        &self,
        host: &AgentHost,
        config: &SessionConfig,
        version: u32,
        leader: Option<&str>,
        origin: SessionOrigin,
    ) -> Option<(String, BoxFuture<'static, dim_agent_runtime::Result<()>>)> {
        let store = self.session_store.clone()?;
        let stored = config.clone();
        let agent_id = host.agent_id.clone();
        let leader = leader.map(str::to_owned);
        Some(self.prepared(config.cwd.clone(), move |id| {
            Box::pin(async move {
                store
                    .create_owned_session(
                        &id,
                        &stored,
                        &agent_id,
                        version,
                        leader.as_deref(),
                        origin,
                    )
                    .await
            })
        }))
    }
    /// A new Session id and the prepare step of its prepared creation: the
    /// project of `cwd` is authorized, then `step` writes the Session's rows.
    /// Creation, `launch` and a fork share it (S5c §9).
    fn prepared(
        &self,
        cwd: String,
        step: impl FnOnce(String) -> BoxFuture<'static, dim_agent_runtime::Result<()>>,
    ) -> (String, BoxFuture<'static, dim_agent_runtime::Result<()>>) {
        let id = uuid::Uuid::new_v4().to_string();
        let step = step(id.clone());
        let settings = self.settings.clone();
        let stopping = self.stopping.clone();
        (
            id,
            Box::pin(async move {
                if let Some(settings) = settings {
                    settings
                        .authorize_project(cwd.into(), &stopping)
                        .await
                        .map_err(|error| {
                            AgentError::fault("project authorization", error.to_string())
                        })?;
                }
                step.await
            }),
        )
    }

    /// A new Session of `host` under protocol `version`, recorded for it.
    pub(crate) async fn create_session(
        &self,
        host: &AgentHost,
        config: SessionConfig,
        version: u32,
        leader: Option<&str>,
        origin: SessionOrigin,
    ) -> dim_agent_runtime::Result<Session> {
        match self
            .prepare_session(host, &config, version, leader, origin)
            .await
        {
            Some((id, prepare)) => {
                let creation = host
                    .agent
                    .create_prepared_session(id.clone(), config, prepare);
                self.admit(&id, &host.agent_id, version, creation).await
            }
            None => {
                let session = host.agent.create_session(config).await?;
                self.insert_version(session.id(), &host.agent_id, version);
                Ok(session)
            }
        }
    }
    /// A prepared creation loads its new id ([S5c §7](../../../docs/design/appserver/session-deletion-and-fork.md#7-serialization-and-races)):
    /// it holds the id's admission gate from before its prepare step commits
    /// the rows, which `session.summaries` then shows, until the Session is
    /// published and its protocol version recorded. A delete of the id waits
    /// for the build and closes what it published. A failed creation keeps
    /// no slot once nothing else holds it.
    pub(crate) async fn admit(
        &self,
        id: &str,
        agent_id: &str,
        version: u32,
        creation: impl Future<Output = dim_agent_runtime::Result<Session>>,
    ) -> dim_agent_runtime::Result<Session> {
        let slot = self.slot(id);
        let gate = slot.gate.lock().await;
        let created = creation.await;
        match &created {
            Ok(_) => self.insert_version(id, agent_id, version),
            Err(_) => {
                drop(gate);
                drop(slot);
                self.directory.inner.lock().expect("agent index").sweep();
            }
        }
        created
    }
    /// `session.delete` ([S5c §3.2](../../../docs/design/appserver/session-deletion-and-fork.md#32-sequence)):
    /// holds S's admission gate, so nothing loads or builds S meanwhile,
    /// closes S if it is loaded, has SessionStore remove what it stores of
    /// S, then forgets S and drops its slot, which stays closed until
    /// nothing else holds it. A stored S that was neither loaded nor closed
    /// ends without a close, so the delete announces it with
    /// [`UnloadedSessionDeleted`]. An unknown id has nothing to remove and
    /// keeps no slot. A delete that fails before the rows go leaves a
    /// Session it closed as `session.close` does, closed in this process with
    /// its rows kept (D5); one it did not close loads again on its next use.
    ///
    /// The reply reports whether this call removed the Session: its stored
    /// rows, or, in a daemon without a native store, a loaded Session. A
    /// Session in use is closed and removed as before;
    /// [`Self::delete_if_inactive`] is the delete that refuses it.
    pub(crate) async fn delete(&self, id: &str) -> dim_agent_runtime::Result<bool> {
        Self::check_session_id(id)?;
        let slot = self.slot(id);
        let gate = slot.gate.lock().await;
        let deleted = self.remove_locked(id, &slot).await?;
        drop(gate);
        drop(slot);
        self.directory.inner.lock().expect("agent index").sweep();
        Ok(deleted)
    }

    /// `session.delete` with `requireInactive`: a Session that is in use is
    /// answered with [`DeleteOutcome::InUse`] and nothing is removed. The
    /// check runs for S under its admission gate, before its led members,
    /// owned Agents, close and store delete, and covers every Session this
    /// delete would remove with it, its led members recursively; their own
    /// deletes need no second check because this one already covered them.
    pub(crate) async fn delete_if_inactive(
        &self,
        id: &str,
    ) -> dim_agent_runtime::Result<DeleteOutcome> {
        Self::check_session_id(id)?;
        let slot = self.slot(id);
        let gate = slot.gate.lock().await;
        let outcome = match self.in_use_reason(id).await? {
            Some(reason) => DeleteOutcome::InUse(reason),
            None => DeleteOutcome::Removed(self.remove_locked(id, &slot).await?),
        };
        drop(gate);
        drop(slot);
        self.directory.inner.lock().expect("agent index").sweep();
        Ok(outcome)
    }

    /// The id names `command-output/<id>`, so only a single path component
    /// can be a Session's (D3); refused before any admission state exists.
    fn check_session_id(id: &str) -> dim_agent_runtime::Result<()> {
        if !matches!(Path::new(id).components().collect::<Vec<_>>()[..],
            [Component::Normal(name)] if name == id)
        {
            return Err(AgentError::InvalidInput(format!("invalid Session id {id}")));
        }
        Ok(())
    }

    /// Removes `id` with its admission gate held: its led members, its owned
    /// Agents, its close, what the store holds of it, and its slot. The
    /// caller drops the gate and the slot and sweeps after this returns.
    async fn remove_locked(
        &self,
        id: &str,
        slot: &Arc<SessionSlot>,
    ) -> dim_agent_runtime::Result<bool> {
        if let Some(records) = &self.records {
            for member in records.led_members(id).await? {
                Box::pin(self.delete(&member)).await?;
            }
            for agent_id in records.owned_agents(id).await? {
                let _ = self.close(&agent_id).await;
                records.delete(&agent_id).await?;
            }
        }
        let loaded = self
            .host_by_session(id)
            .and_then(|host| host.agent.get_session(id));
        if let Some(session) = &loaded {
            slot.closed.store(true, Ordering::Release);
            if let Err(error) = session.close().await {
                tracing::error!(%error, session_id = id, "Session cleanup failed before its delete");
            }
        }
        let unloaded = loaded.is_none() && !slot.closed.load(Ordering::Acquire);
        let stored = match &self.session_store {
            Some(store) => store.delete_session(id).await?,
            None => false,
        };
        let deleted = stored || (self.session_store.is_none() && loaded.is_some());
        let owner = {
            let mut inner = self.directory.inner.lock().expect("agent index");
            let owner = inner.versions.remove(id);
            inner.unavailable_sessions.remove(id);
            if stored || slot.closed.load(Ordering::Acquire) {
                slot.closed.store(true, Ordering::Release);
                slot.deleted.store(true, Ordering::Release);
            }
            owner
        };
        if let Some((agent_id, _)) = &owner {
            let agent_id = agent_id.clone();
            self.release_owned(&agent_id).await;
        }
        if let Some((agent_id, _)) = owner.filter(|_| unloaded && stored) {
            let (_, root) = self
                .loader
                .get()
                .expect("AgentsPlugin apply sets the loader");
            let announcement = Arc::new(UnloadedSessionDeleted {
                agent_id,
                session_id: id.to_owned(),
            });
            if let Err(error) = root.parallel(announcement).await {
                tracing::warn!(%error, session_id = id, "Deletion of an unloaded Session was not announced");
            }
        }
        Ok(deleted)
    }
    /// Why `id` is in use, for a delete that requires an inactive Session:
    /// its uses, its hold or a line of it, or a Session it leads, recursively.
    /// `None` when nothing uses it. A Session that is not loaded has no line
    /// and no hold to read; its uses alone answer. Reading them creates no
    /// slot: a Session with no slot has no uses.
    async fn in_use_reason(&self, id: &str) -> dim_agent_runtime::Result<Option<String>> {
        let mut reasons = Vec::new();
        let uses = self.directory.inner.lock().expect("agent index").uses(id);
        if uses > 0 {
            reasons.push(match uses {
                1 => "1 request or subscription is using it".to_owned(),
                uses => format!("{uses} requests or subscriptions are using it"),
            });
        }
        if let Some((_, session)) = self.session(id) {
            match session.in_use() {
                Some(InUse::Held) => reasons.push("a background task holds it".to_owned()),
                Some(InUse::Running) => reasons.push("a turn is running".to_owned()),
                None => {}
            }
        }
        if !reasons.is_empty() {
            return Ok(Some(reasons.join(", ")));
        }
        if let Some(records) = &self.records {
            for member in records.led_members(id).await? {
                if let Some(reason) = Box::pin(self.in_use_reason(&member)).await? {
                    return Ok(Some(format!("its member {member} is in use: {reason}")));
                }
            }
        }
        Ok(None)
    }
    /// Whether an Agent has any member Session, loaded or stored. An owned
    /// Agent with none is an incomplete delegation, which no request may
    /// address.
    pub(crate) async fn has_members(&self, agent_id: &str) -> bool {
        if self
            .host(Some(agent_id))
            .is_some_and(|host| !host.agent.sessions().is_empty())
        {
            return true;
        }
        match &self.records {
            Some(records) => records
                .member_sessions(agent_id)
                .await
                .map(|members| !members.is_empty())
                .unwrap_or(false),
            None => false,
        }
    }
    /// An owned Agent with no open member Session closes; its record goes
    /// when no stored member Session keeps it. A public Agent stays loaded.
    pub(crate) async fn release_owned(&self, agent_id: &str) {
        let owned = self.owner(agent_id).is_some_and(|owner| owner.is_some())
            || match &self.records {
                Some(records) => matches!(records.agent_owner(agent_id).await, Ok(Some(_))),
                None => false,
            };
        if !owned {
            return;
        }
        match &self.records {
            Some(records) => {
                if records
                    .member_sessions(agent_id)
                    .await
                    .map(|members| !members.is_empty())
                    .unwrap_or(true)
                {
                    return;
                }
                let _ = self.close(agent_id).await;
                if let Err(error) = records.delete(agent_id).await {
                    tracing::warn!(%error, agent_id, "Owned Agent record was not deleted");
                }
            }
            None if !self.has_open_sessions(agent_id) => {
                let _ = self.close(agent_id).await;
            }
            None => {}
        }
    }
    fn has_open_sessions(&self, agent_id: &str) -> bool {
        self.host(Some(agent_id)).is_some_and(|host| {
            host.agent
                .sessions()
                .iter()
                .any(|session| !session.is_closing())
        })
    }
    /// Removes what a delegation cannot leave behind: an owned Agent with no
    /// member Session, and one whose owner Session is gone with its members.
    async fn remove_unfinished(&self, records: &AgentRecords) {
        let (incomplete, orphaned) = match records.unfinished_owned().await {
            Ok(unfinished) => unfinished,
            Err(error) => {
                tracing::error!(%error, "Unfinished delegations could not be read");
                return;
            }
        };
        for agent_id in incomplete {
            if let Err(error) = records.delete(&agent_id).await {
                tracing::error!(%error, %agent_id, "Incomplete delegation was not removed");
            }
        }
        for agent_id in orphaned {
            let members = records.member_sessions(&agent_id).await.unwrap_or_default();
            if let Some(store) = &self.session_store {
                for member in members {
                    if let Err(error) = store.delete_session(&member).await {
                        tracing::error!(%error, session_id = %member, "Orphaned member Session was not removed");
                    }
                }
            }
            if let Err(error) = records.delete(&agent_id).await {
                tracing::error!(%error, %agent_id, "Orphaned owned Agent was not removed");
            }
        }
    }
    /// `session.fork` ([S5c §6](../../../docs/design/appserver/session-deletion-and-fork.md#6-fork)):
    /// acquires the source, restoring it if needed, and checks the cut as
    /// `session.branch_create` does (at most the published `main` head, the
    /// end of an atomic batch, a context boundary) and as a rest point. The
    /// fork is a prepared creation whose prepare step is SessionStore's fork
    /// transaction; it belongs to the source's Agent and protocol version,
    /// and its `main` opens with `branch/resume`.
    pub(crate) async fn fork(&self, source: &str, at: u64) -> dim_agent_runtime::Result<Session> {
        let store = self
            .session_store
            .clone()
            .ok_or_else(|| AgentError::InvalidInput("a fork needs the native store".into()))?;
        let usage = self.acquire(source).await?;
        let session = &usage.session;
        if session.config().collaboration_role == CollaborationRole::Member {
            return Err(AgentError::InvalidInput(
                "a member conversation cannot be forked".into(),
            ));
        }
        let checked = async {
            let boundary =
                dim_agent_capabilities::history::validate_context_boundary_in(session, "main", at)
                    .await?;
            let prefix = session.context("main", at).await?;
            Ok::<_, AgentError>((boundary, prefix))
        }
        .await;
        if session.is_closing() {
            return Err(AgentError::Closed(source.into()));
        }
        let (boundary, prefix) = checked?;
        boundary
            .and_then(|()| rest_point(&prefix.events))
            .map_err(AgentError::InvalidInput)?;
        let version = self
            .version_of(source)
            .ok_or_else(|| AgentError::Closed(source.into()))?;
        let config = session.config().clone();
        let source = source.to_owned();
        let (id, prepare) = self.prepared(config.cwd.clone(), move |fork| {
            Box::pin(async move { store.fork_session(&source, &fork, at).await })
        });
        let creation = usage
            .host
            .agent
            .create_prepared_session(id.clone(), config, prepare);
        self.admit(&id, &usage.host.agent_id, version, creation)
            .await
    }
    /// `agent.create`: one more Agent, a peer of every other. A requested
    /// profile comes from outside the process and may only select provided
    /// tools, each once; without one the process default profile applies.
    /// Callers allocate a fresh id before loading so their own
    /// access policy can precede publication in the shared directory.
    pub(crate) async fn create(
        &self,
        id: uuid::Uuid,
        profile: Option<AgentProfile>,
        owner: Option<&str>,
    ) -> Result<Arc<AgentHost>, AgentOpError> {
        let profile = match profile {
            Some(profile) => {
                self.check_profile(&profile)
                    .map_err(|error| AgentOpError::Build(AgentError::Build(error)))?;
                profile
            }
            None => process_default(&self.session_defaults),
        };
        self.load(id.to_string(), false, profile, owner.map(str::to_owned))
            .await
    }
    /// A profile from outside the process lists each tool of its mount list
    /// once. A tool that no part registers is reported when the Session
    /// reads its tools.
    pub fn check_profile(&self, profile: &AgentProfile) -> Result<(), String> {
        let mut seen = BTreeSet::new();
        for tool in profile.tools.iter().flatten() {
            if !seen.insert(tool) {
                return Err(format!("tool {tool} is listed twice"));
            }
        }
        Ok(())
    }
    /// Closes exactly one Agent; other Agents are unaffected. New business
    /// requests fail with agent_unknown from the removal on; closing requests
    /// still locate the Agent until its settlement finishes.
    pub async fn close(&self, agent_id: &str) -> Result<(), AgentOpError> {
        let entry = {
            let mut inner = self.directory.inner.lock().expect("agent index");
            let Some(entry) = inner.agents.remove(agent_id) else {
                return Err(AgentOpError::Unknown(agent_id.to_owned()));
            };
            inner
                .closing
                .insert(agent_id.to_owned(), entry.host.clone());
            entry
        };
        let closed = entry.host.agent.close().await;
        let unloaded = entry.view.shutdown().await;
        self.directory
            .inner
            .lock()
            .expect("agent index")
            .instances
            .remove(agent_id);
        self.directory
            .inner
            .lock()
            .expect("agent index")
            .closing
            .remove(agent_id);
        unloaded.map_err(AgentOpError::Unload)?;
        closed.map_err(AgentOpError::Settle)
    }
    /// Process shutdown phase one: business settlement of every Agent.
    pub async fn settle_all(&self) -> Vec<AgentError> {
        self.stopping.cancel();
        let mut errors = vec![];
        let mut tasks = tokio::task::JoinSet::new();
        for host in self.hosts() {
            let agent = host.agent.clone();
            tasks.spawn(async move { agent.close().await });
        }
        while let Some(joined) = tasks.join_next().await {
            match joined {
                Ok(Err(error)) => errors.push(error),
                Err(error) => errors.push(AgentError::fault(
                    "close",
                    format!("agent close task failed: {error}"),
                )),
                Ok(Ok(())) => {}
            }
        }
        errors
    }

    async fn load(
        &self,
        agent_id: String,
        default: bool,
        profile: AgentProfile,
        owner: Option<String>,
    ) -> Result<Arc<AgentHost>, AgentOpError> {
        let (loader, root) = self
            .loader
            .get()
            .expect("AgentsPlugin apply sets the loader")
            .clone();
        let spec = AgentSpec {
            builder: self.builder.clone(),
            agent_id: agent_id.clone(),
            default,
            profile,
            owner,
        };
        let mut creation = tokio::spawn(async move {
            #[cfg(test)]
            tests::creation_held(&spec.agent_id, &root).await;
            loader.create_instance(&root, AGENT_GROUP).with(spec).await
        });
        let instance = match tokio::time::timeout(self.startup_timeout, &mut creation).await {
            Ok(Ok(Ok(instance))) => instance,
            Ok(Ok(Err(error))) => {
                self.forget(&agent_id);
                return Err(AgentOpError::Build(AgentError::Build(error.to_string())));
            }
            Ok(Err(error)) => {
                self.forget(&agent_id);
                return Err(AgentOpError::Build(AgentError::fault(
                    "agent creation",
                    error.to_string(),
                )));
            }
            Err(_) => {
                self.forget(&agent_id);
                tokio::spawn(async move {
                    if let Ok(Ok(instance)) = creation.await {
                        let _ = instance.view.shutdown().await;
                    }
                });
                return Err(AgentOpError::Build(AgentError::Build(format!(
                    "agent {agent_id} initialization timed out"
                ))));
            }
        };
        let view = instance.view;
        self.directory
            .inner
            .lock()
            .expect("agent index")
            .instances
            .insert(agent_id.clone(), view.clone());
        let failed = instance
            .report
            .into_iter()
            .find_map(|(row, result)| match result {
                InstanceResult::Active => None,
                InstanceResult::Failed(error) => Some(match error {
                    rutis_loader::LoaderError::Rejected { error, .. } => AgentError::from(error),
                    error => AgentError::Build(error.to_string()),
                }),
                result => Some(AgentError::Build(format!("{row}: {result:?}"))),
            });
        if let Some(error) = failed {
            self.forget(&agent_id);
            let _ = view.shutdown().await;
            self.directory
                .inner
                .lock()
                .expect("agent index")
                .instances
                .remove(&agent_id);
            return Err(error.into());
        }
        let host = {
            let mut inner = self.directory.inner.lock().expect("agent index");
            inner.pending.remove(&agent_id)
        }
        .ok_or_else(|| {
            AgentOpError::Build(AgentError::Build(format!(
                "agent {agent_id} did not register"
            )))
        })?;
        if let Some(records) = &self.records
            && let Err(error) = records
                .save(&agent_id, &host.profile, default, host.owner.as_deref())
                .await
        {
            self.forget(&agent_id);
            let settled = host.agent.close().await;
            let unloaded = view.shutdown().await;
            self.directory
                .inner
                .lock()
                .expect("agent index")
                .instances
                .remove(&agent_id);
            return Err(AgentOpError::Build(
                AgentError::aggregate(
                    std::iter::once(error)
                        .chain(settled.err())
                        .chain(unloaded.err().map(AgentError::from))
                        .collect(),
                )
                .expect_err("Agent record write failed"),
            ));
        }
        self.directory
            .inner
            .lock()
            .expect("agent index")
            .agents
            .insert(
                agent_id,
                Entry {
                    host: host.clone(),
                    view,
                },
            );
        Ok(host)
    }
    /// Drops an Agent from the directory; see [`AgentDirectory::forget`].
    pub(crate) fn forget(&self, agent_id: &str) {
        self.directory.forget(agent_id);
    }
}

/// Whether a fork whose `main` holds `events` starts no work when it loads
/// ([S5c §6.2](../../../docs/design/appserver/session-deletion-and-fork.md#62-the-cut)).
/// The reload rules that start work read these facts: Loop and InputQueue
/// start a turn for an open turn's work or for a Queue or Steer input not
/// yet consumed, unless it was applied before the last interrupt or
/// InputQueue deleted it on a withdrawal;
/// BackgroundExec fails a task it finds open, and a task that ended while
/// its generation was not closing posts a wake input that belongs before
/// the cut. This reads only their public facts;
/// a capability whose reload rule comes to start work from other facts
/// adds them here, so a fork keeps starting nothing.
fn rest_point(events: &[Arc<dim_agent_types::AgentEvent>]) -> Result<(), String> {
    use dim_agent_capabilities::workspace_tools::background::WAKE_RESULT_TAG;
    use dim_agent_types::{ExecStatus, InputMode, InterruptScope};
    let mut turn = false;
    let mut interrupted_at = 0;
    let mut closing = false;
    let mut inputs: BTreeMap<&str, Option<u64>> = BTreeMap::new();
    let mut tasks = BTreeSet::new();
    let mut wakes = BTreeSet::new();
    for event in events {
        match &event.kind {
            EventKind::TurnStart(_) => turn = true,
            EventKind::TurnEnd(_) => turn = false,
            EventKind::BranchStart(_) | EventKind::BranchResume(_) => closing = false,
            EventKind::StopInterrupt(data) => {
                interrupted_at = event.sequence;
                closing |= data.scope == InterruptScope::Branch;
            }
            EventKind::InputMessage(data) => {
                if data.effective_mode() != InputMode::Append {
                    inputs.insert(&event.id, None);
                }
                if let Some(ended) = event
                    .tags
                    .iter()
                    .flatten()
                    .find_map(|tag| tag.strip_prefix(WAKE_RESULT_TAG))
                {
                    wakes.remove(ended);
                }
            }
            EventKind::InputApplied(data) => {
                if let Some(applied) = inputs.get_mut(data.input_id.as_str()) {
                    *applied = Some(event.sequence);
                }
            }
            EventKind::InputConsumed(data) => {
                inputs.remove(data.input_id.as_str());
            }
            EventKind::InputDeleted(data) => {
                inputs.remove(data.input_id.as_str());
            }
            EventKind::ExecStarted(data) => {
                tasks.insert(data.task_id.as_str());
            }
            EventKind::ExecEnded(data) => {
                tasks.remove(data.task_id.as_str());
                if data.status != ExecStatus::Cancelled && !closing {
                    wakes.insert(event.id.as_str());
                }
            }
            _ => {}
        }
    }
    let cause = if turn {
        "a turn is open"
    } else if inputs
        .values()
        .any(|applied| applied.is_none_or(|sequence| sequence > interrupted_at))
    {
        "an input is waiting to be consumed"
    } else if !tasks.is_empty() {
        "a background task is running"
    } else if !wakes.is_empty() {
        "a background task has not woken the branch"
    } else {
        return Ok(());
    };
    Err(format!("the fork position is not a rest point: {cause}"))
}

/// Owns every Agent subtree: the process assembly entry.
pub(crate) struct AgentsPlugin {
    builder: AgentBuilder,
    config: Arc<ServerConfig>,
    /// The process root: top-level loader instances are created from it.
    root: Ctx,
    /// Native storage only with a database.
    stored: Option<(Key<SessionStore>, Key<SqliteStorage>)>,
    /// Settings only with local execution.
    settings: Option<Key<SettingsService>>,
    loader: Key<Loader>,
    directory: Key<AgentDirectory>,
    parts: Key<dim_agent_runtime::PartRegistry>,
}
impl AgentsPlugin {
    pub fn new(
        names: &Names<'_>,
        builder: AgentBuilder,
        config: Arc<ServerConfig>,
        root: Ctx,
    ) -> rutis::Result<Self> {
        Ok(Self {
            stored: config
                .database_path
                .is_some()
                .then(|| Ok::<_, Error>((names.key()?, names.key()?)))
                .transpose()?,
            settings: config
                .local_execution()
                .then(|| names.key::<SettingsService>())
                .transpose()?,
            loader: names.key()?,
            directory: names.key()?,
            parts: names.key()?,
            builder,
            config,
            root,
        })
    }
}
impl DynPlugin for AgentsPlugin {
    fn name(&self) -> &str {
        "Agents"
    }
    fn injects(&self) -> Vec<TypeKey> {
        let mut injects = Vec::new();
        if let Some((store, storage)) = &self.stored {
            injects.push(store.erase());
            injects.push(storage.erase());
        }
        if let Some(settings) = &self.settings {
            injects.push(settings.erase());
        }
        injects.push(self.loader.erase());
        injects.push(self.directory.erase());
        injects.push(self.parts.erase());
        injects
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let stored = match &self.stored {
                Some((store, storage)) => Some((ctx.require_at(store)?, ctx.require_at(storage)?)),
                None => None,
            };
            let settings = match &self.settings {
                Some(settings) => Some(ctx.require_at(settings)?),
                None => None,
            };
            let loader = ctx.require_at(&self.loader)?;
            let directory = ctx.require_at(&self.directory)?;
            let parts = ctx.require_at(&self.parts)?;
            parts.register(
                ctx,
                dim_agent_runtime::PartScope::Session,
                "AgentsSession",
                |mount| {
                    Ok(Some(
                        Box::new(configuration::AgentsSessionPlugin::new(mount.names)?)
                            as Box<dyn DynPlugin>,
                    ))
                },
            )?;
            let mut builder = self.builder.clone();
            let (records, session_store) = match stored {
                Some((store, storage)) => {
                    builder.timeline_store(store.timeline_store());
                    (Some(Arc::new(AgentRecords(storage))), Some(store))
                }
                None => (None, None),
            };
            let default_id = match &records {
                Some(records) => records
                    .agents()
                    .await?
                    .into_iter()
                    .find(|record| record.default)
                    .map(|record| record.id),
                None => None,
            }
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
            let _ = directory.default_agent_id.set(default_id);
            let index = AgentIndex {
                directory,
                builder,
                session_defaults: self.config.session_defaults.clone(),
                ctx: OnceLock::from(ctx.clone()),
                loader: OnceLock::from(((*loader).clone(), self.root.clone())),
                startup_timeout: self.config.startup_timeout,
                records,
                session_store,
                settings,
                selects_models: self.config.protected_profile(),
                stopping: CancellationToken::new(),
                #[cfg(feature = "testing")]
                idle_admitted: Arc::new(tokio::sync::Notify::new()),
            };
            let index = Arc::new(index);
            let cleanup = index.clone();
            ctx.effect(move || async move {
                cleanup.stopping.cancel();
                Ok(())
            })?;
            if self.config.session_execution {
                index.install_default().await.map_err(|error| {
                    rutis::Error::from(ErrorKind::PluginFailed(
                        format!("default Agent failed: {error}").into(),
                    ))
                })?;
            }
            ctx.provide_as::<AgentIndex>(index.clone())?;
            if index.session_store.is_some() {
                let grace = self.config.idle_unload_after;
                ctx.effect(move || {
                    let stopping = index.stopping.clone();
                    let pass = tokio::spawn(async move {
                        let mut marks = HashMap::new();
                        loop {
                            tokio::select! {
                                _ = index.stopping.cancelled() => return,
                                _ = tokio::time::sleep(grace) => {}
                            }
                            index.unload_idle(&mut marks).await;
                        }
                    });
                    async move {
                        stopping.cancel();
                        pass.await.map_err(|error| {
                            rutis::Error::from(ErrorKind::PluginFailed(
                                format!("idle unload pass failed: {error}").into(),
                            ))
                        })
                    }
                })?;
            }
            Ok(Effect::Done)
        })
    }
}

/// The loader row of the Agent group: each Agent is its plugin and its
/// Sessions (`session_row`), with the builder's process values.
pub(crate) fn group_row(
    builder: &AgentBuilder,
    session_row: serde_json::Value,
) -> serde_json::Value {
    builder.agent_row([
        serde_json::json!({ "id": "AgentScope", "name": "AgentScope" }),
        session_row,
    ])
}

/// What an Agent instance is created with.
struct AgentSpec {
    builder: AgentBuilder,
    agent_id: String,
    default: bool,
    profile: AgentProfile,
    owner: Option<String>,
}

/// Registers the module of the Agent instance's plugin.
pub(crate) fn register(builtins: &mut Modules) {
    builtins
        .builtins
        .register_with("AgentScope", |build: &Build, _: &NoConfig| {
            let spec = build.value::<AgentSpec>().ok_or_else(|| {
                rutis::Error::from(ErrorKind::PluginFailed(
                    "Agent created without its specification".into(),
                ))
            })?;
            Ok(AgentScopePlugin(spec))
        });
}

/// One Agent: the plugin of its loader instance. Its Sessions are instances
/// created beside it; it owns their protocol identity. It depends only on the
/// directory and the loader, which do not reload, so it unloads only when its
/// Agent is closed or the process stops; both settle the Sessions first. Its
/// own cleanup repeats that settlement idempotently; effects unload in
/// reverse order.
struct AgentScopePlugin(Arc<AgentSpec>);
impl Plugin for AgentScopePlugin {
    type Deps = (Arc<AgentDirectory>, Arc<Loader>);
    fn name(&self) -> &str {
        "AgentScope"
    }
    async fn apply(&self, ctx: &Ctx, (directory, loader): Self::Deps) -> rutis::Result {
        let spec = &self.0;
        let slot = Arc::new(Mutex::new(None));
        let sessions = ctx.plugin(Sessions(slot.clone()));
        (&sessions).await.map_err(|error| {
            rutis::Error::from(ErrorKind::PluginFailed(
                format!("Session host failed: {error}").into(),
            ))
        })?;
        let sessions = match (sessions.state().state, slot.lock().unwrap().take()) {
            (FiberState::Active, Some(sessions)) => sessions,
            (state, _) => {
                return Err(rutis::Error::from(ErrorKind::PluginFailed(
                    format!("Session host is {state:?}").into(),
                )));
            }
        };
        let host = Arc::new(AgentHost {
            agent: spec.builder.build((*loader).clone(), ctx),
            agent_id: spec.agent_id.clone(),
            default: spec.default,
            profile: spec.profile.clone(),
            owner: spec.owner.clone(),
            ctx: sessions,
        });
        let owner = host.clone();
        let unloading = directory.clone();
        let agent_id = spec.agent_id.clone();
        ctx.effect(move || async move {
            unloading.forget(&agent_id);
            owner.agent.close().await.map_err(Into::into)
        })?;
        directory.register(host);
        Ok(())
    }
}

/// Host of the listeners on one Agent's Sessions. A child of the Agent
/// plugin registered before its settlement, it unloads after the Sessions
/// closed.
struct Sessions(Arc<Mutex<Option<Ctx>>>);
impl Plugin for Sessions {
    type Deps = ();
    fn name(&self) -> &str {
        "Sessions"
    }
    async fn apply(&self, ctx: &Ctx, (): Self::Deps) -> rutis::Result {
        *self.0.lock().unwrap() = Some(ctx.clone());
        Ok(())
    }
}

#[cfg(test)]
mod tests;

//! A raw rutis root with EventHub, the real plugin files and a fixture
//! `sessions` service, so a test can observe every input write, fail one on
//! demand, and hold deliveries in flight together.
#![allow(dead_code)]
use super::wait_for;
use dim_agent_capabilities::file_storage::FileStoragePlugin;
use dim_agent_capabilities::host::{
    SessionFileError, SessionFileErrorCode, SessionFilesHost, SessionFilesService,
    StoredSessionFile,
};
use dim_agent_capabilities::methods::{Owner, RpcError};
use dim_agent_runtime::testing::MinimalHost;
use dim_agent_runtime::{AgentBuilder, AgentConfig, Names, PartRegistry};
use dim_agent_types::InputMode;
use dim_app_server::methods::{ManagementMethods, ManagementMethodsPlugin};
use dim_app_server::plugin_files::PluginFilesPlugin;
use dim_event_hub::{
    DeclareRequest, EventHub, EventHubError, EventHubPlugin, InstanceState, TypeDeclaration,
};
use rutis::Ctx;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, FiberView, Key, TypeKey};
use rutis_bridge::session::{Error as BridgeError, HostDispatch, Reply, Value, host_key};
use serde_json::{Value as Json, json};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

/// One `sessions.input` write as the fake service received it.
#[derive(Debug, Clone)]
pub struct InputCall {
    pub session_id: String,
    pub branch_id: String,
    pub mode: String,
    pub submission_id: String,
    pub text: String,
    pub key: Option<Json>,
}

impl InputCall {
    pub fn key_use(&self) -> Option<&str> {
        self.key.as_ref()?.get("use")?.as_str()
    }
    pub fn key_name(&self) -> Option<&str> {
        self.key.as_ref()?.get("name")?.as_str()
    }
}

struct Inner {
    calls: Mutex<Vec<InputCall>>,
    /// Session id to the error code its input answers; absent accepts.
    failures: Mutex<HashMap<String, &'static str>>,
    /// A submission-id needle and the error code a matching input answers,
    /// so a test can refuse one guide write of a creation (design §3.3).
    refusals: Mutex<Vec<(String, &'static str)>>,
    /// Submission-id needles whose input call fails at the transport level,
    /// so the caller cannot know whether the write happened.
    transports: Mutex<Vec<String>>,
    /// `session|branch|submission` to the input an accepted write committed,
    /// which `lookup` answers (design §2.4).
    accepted: Mutex<HashMap<String, (String, u64)>>,
    /// Session ids `exists` answers `false` for (design §3.4).
    missing: Mutex<std::collections::HashSet<String>>,
    sequence: AtomicU64,
    gate: Mutex<Option<Arc<Gate>>>,
}

/// The fixture `sessions` service (design Appendix F's `Key<dyn HostDispatch>`
/// at `host_key("sessions")`), with the envelope `{ok, value}` the real one
/// answers.
#[derive(Clone)]
pub struct FakeSessions {
    inner: Arc<Inner>,
}

impl Default for FakeSessions {
    fn default() -> Self {
        Self {
            inner: Arc::new(Inner {
                calls: Mutex::new(Vec::new()),
                failures: Mutex::new(HashMap::new()),
                refusals: Mutex::new(Vec::new()),
                transports: Mutex::new(Vec::new()),
                accepted: Mutex::new(HashMap::new()),
                missing: Mutex::new(std::collections::HashSet::new()),
                sequence: AtomicU64::new(0),
                gate: Mutex::new(None),
            }),
        }
    }
}

impl FakeSessions {
    pub fn calls(&self) -> Vec<InputCall> {
        self.inner.calls.lock().unwrap().clone()
    }
    pub fn clear(&self) {
        self.inner.calls.lock().unwrap().clear();
    }
    /// Answers the inputs of `session` with `code` (design §2.3's `failed`).
    pub fn fail(&self, session: &str, code: &'static str) {
        self.inner
            .failures
            .lock()
            .unwrap()
            .insert(session.to_owned(), code);
    }
    pub fn accept(&self, session: &str) {
        self.inner.failures.lock().unwrap().remove(session);
    }
    /// Answers `exists` `false` for `session`, as a deleted Session does
    /// (design §3.4).
    pub fn deleted(&self, session: &str) {
        self.inner
            .missing
            .lock()
            .unwrap()
            .insert(session.to_owned());
    }
    pub fn restored(&self, session: &str) {
        self.inner.missing.lock().unwrap().remove(session);
    }
    /// Answers any input whose submission id contains `needle` with `code`,
    /// so one guide write of a creation fails while its others succeed
    /// (design §3.3).
    pub fn refuse(&self, needle: &str, code: &'static str) {
        self.inner
            .refusals
            .lock()
            .unwrap()
            .push((needle.to_owned(), code));
    }
    /// Makes any input whose submission id contains `needle` fail at the
    /// transport level: the call itself fails, so whether the write happened
    /// is unknown (design Appendix C).
    pub fn fail_transport(&self, needle: &str) {
        self.inner
            .transports
            .lock()
            .unwrap()
            .push(needle.to_owned());
    }
    /// Holds every input call until [`Gate::release`], counting those in
    /// flight.
    pub fn gate(&self) -> Arc<Gate> {
        self.install(Gate::default())
    }
    /// Holds every guide write (`key.use` `live`) until [`Gate::release`];
    /// retires pass, so a test can keep a creation in flight while a removal
    /// runs against the same source (design §3.3).
    pub fn gate_live(&self) -> Arc<Gate> {
        self.install(Gate {
            live_only: true,
            ..Gate::default()
        })
    }
    fn install(&self, gate: Gate) -> Arc<Gate> {
        let gate = Arc::new(gate);
        *self.inner.gate.lock().unwrap() = Some(gate.clone());
        gate
    }
}

impl HostDispatch for FakeSessions {
    fn methods(&self) -> Option<Json> {
        Some(json!({ "input": "async", "lookup": "async", "exists": "async" }))
    }
    fn invoke(&self, method: &str, args: Value) -> Reply {
        let Json::Array(args) = args.json()? else {
            return Err(BridgeError::Value("expected one request object".into()));
        };
        let Ok([write]) = <[Json; 1]>::try_from(args) else {
            return Err(BridgeError::Value("expected one request object".into()));
        };
        match method {
            // The `sessions.lookup` reply (design §2.4).
            "lookup" => {
                let key = format!(
                    "{}|{}|{}",
                    write["sessionId"].as_str().unwrap_or_default(),
                    write["branchId"].as_str().unwrap_or_default(),
                    write["submissionId"].as_str().unwrap_or_default()
                );
                let held = self.inner.accepted.lock().unwrap().get(&key).cloned();
                return Ok(Value::future(async move {
                    Ok(match held {
                        Some((input_id, sequence)) => json!({
                            "ok": true,
                            "value": { "status": "accepted", "inputId": input_id, "sequence": sequence },
                        }),
                        None => json!({ "ok": true, "value": { "status": "unknown" } }),
                    }
                    .into())
                }));
            }
            // The `sessions.exists` read (design §3.4).
            "exists" => {
                let session = write["sessionId"].as_str().unwrap_or_default().to_owned();
                let exists = !self.inner.missing.lock().unwrap().contains(&session);
                return Ok(Value::future(async move {
                    Ok(json!({ "ok": true, "value": { "exists": exists } }).into())
                }));
            }
            "input" => {}
            other => {
                return Err(BridgeError::Value(format!(
                    "the fixture sessions service has no method {other}"
                )));
            }
        }
        let call = InputCall {
            session_id: write["sessionId"].as_str().unwrap_or_default().to_owned(),
            branch_id: write["branchId"].as_str().unwrap_or_default().to_owned(),
            mode: write["mode"].as_str().unwrap_or_default().to_owned(),
            submission_id: write["submissionId"]
                .as_str()
                .unwrap_or_default()
                .to_owned(),
            text: write["rawBlocks"][0]["text"]
                .as_str()
                .unwrap_or_default()
                .to_owned(),
            key: write["rawBlocks"][0]["key"]
                .is_object()
                .then(|| write["rawBlocks"][0]["key"].clone()),
        };
        let inner = self.inner.clone();
        // Recorded at arrival, before any gate: the order of the recorded
        // inputs is the order the writes were made, not the order they
        // finished (design §3.3's race).
        let code = inner
            .failures
            .lock()
            .unwrap()
            .get(&call.session_id)
            .copied()
            .or_else(|| {
                inner
                    .refusals
                    .lock()
                    .unwrap()
                    .iter()
                    .find(|(needle, _)| call.submission_id.contains(needle.as_str()))
                    .map(|(_, code)| *code)
            });
        inner.calls.lock().unwrap().push(call.clone());
        if inner
            .transports
            .lock()
            .unwrap()
            .iter()
            .any(|needle| call.submission_id.contains(needle.as_str()))
        {
            return Err(BridgeError::Transport(
                "the fixture sessions link dropped".into(),
            ));
        }
        Ok(Value::future(async move {
            let gate = inner.gate.lock().unwrap().clone();
            if let Some(gate) = gate
                && gate.holds(&call)
            {
                gate.arrive();
                gate.wait().await;
                gate.leave();
            }
            match code {
                None => {
                    let sequence = inner.sequence.fetch_add(1, Ordering::SeqCst) + 1;
                    let input_id = format!("input-{sequence}");
                    inner.accepted.lock().unwrap().insert(
                        format!(
                            "{}|{}|{}",
                            call.session_id, call.branch_id, call.submission_id
                        ),
                        (input_id.clone(), sequence),
                    );
                    Ok(json!({
                        "ok": true,
                        "value": { "inputId": input_id, "sequence": sequence },
                    })
                    .into())
                }
                Some(code) => Ok(json!({
                    "ok": false,
                    "error": { "code": code, "message": "refused by the fixture", "retryable": false },
                })
                .into()),
            }
        }))
    }
}

/// The gate the fake service holds its input calls behind.
pub struct Gate {
    /// Hold guide writes only; retires pass.
    live_only: bool,
    in_flight: AtomicUsize,
    max: AtomicUsize,
    max_tx: watch::Sender<usize>,
    release: watch::Sender<bool>,
}

impl Default for Gate {
    fn default() -> Self {
        Self {
            live_only: false,
            in_flight: AtomicUsize::new(0),
            max: AtomicUsize::new(0),
            max_tx: watch::channel(0).0,
            release: watch::channel(false).0,
        }
    }
}

impl Gate {
    fn holds(&self, call: &InputCall) -> bool {
        !self.live_only || call.key_use() == Some("live")
    }
    fn arrive(&self) {
        let now = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
        let _ = self.max_tx.send_if_modified(|max| {
            if now > *max {
                *max = now;
                true
            } else {
                false
            }
        });
    }
    async fn wait(&self) {
        let mut release = self.release.subscribe();
        while !*release.borrow() {
            if release.changed().await.is_err() {
                return;
            }
        }
    }
    fn leave(&self) {
        self.in_flight.fetch_sub(1, Ordering::SeqCst);
    }
    pub fn max(&self) -> usize {
        *self.max_tx.borrow()
    }
    pub fn max_watch(&self) -> watch::Receiver<usize> {
        self.max_tx.subscribe()
    }
    pub fn release(&self) {
        self.release.send_replace(true);
    }
}

/// The fixture producer's own state: its row's context and the key it reads
/// `eventHub` with on every call (design Appendix F: a producer reads the
/// service per call), the declaration it makes whenever it finds the service
/// (design §2.1), and how many times it declared.
#[derive(Default)]
pub struct ProducerState {
    installed: Mutex<Option<(Ctx, Key<EventHub>)>>,
    declaration: Mutex<Option<DeclareRequest>>,
    declares: AtomicUsize,
}

impl ProducerState {
    /// The declaration this producer makes whenever it finds the service.
    pub fn declare_on_find(&self, declaration: DeclareRequest) {
        *self.declaration.lock().unwrap() = Some(declaration);
    }
    pub fn declaration(&self) -> Option<DeclareRequest> {
        self.declaration.lock().unwrap().clone()
    }
    fn install(&self, ctx: Ctx, key: Key<EventHub>) {
        *self.installed.lock().unwrap() = Some((ctx, key));
    }
    pub fn ctx(&self) -> Ctx {
        self.installed.lock().unwrap().as_ref().unwrap().0.clone()
    }
    /// The service as the row reads it now: absent while EventHub is off or
    /// reloading (design §2.1, Appendix F).
    pub fn hub(&self) -> Option<Arc<EventHub>> {
        let installed = self.installed.lock().unwrap();
        let (ctx, key) = installed.as_ref()?;
        ctx.get_at(key)
    }
    /// How many declarations this producer made.
    pub fn declares(&self) -> usize {
        self.declares.load(Ordering::SeqCst)
    }
    pub async fn declare(
        &self,
        request: dim_event_hub::DeclareRequest,
    ) -> Result<(), EventHubError> {
        self.declares.fetch_add(1, Ordering::SeqCst);
        let hub = self
            .hub()
            .ok_or_else(|| EventHubError::new("no_caller_row", "eventHub is not installed"))?;
        hub.declare(&self.ctx(), request).await
    }
    pub async fn publish(
        &self,
        request: dim_event_hub::PublishRequest,
    ) -> Result<Json, EventHubError> {
        let hub = self
            .hub()
            .ok_or_else(|| EventHubError::new("no_caller_row", "eventHub is not installed"))?;
        let ctx = self.ctx();
        match hub.publish(&ctx, request.clone()).await {
            // A publish that answers `unknown_source` declares again and
            // retries once (design §2.1's producer pattern).
            Err(error) if error.code == "unknown_source" => {
                let Some(declaration) = self.declaration() else {
                    return Err(error);
                };
                self.declare(declaration).await?;
                hub.publish(&ctx, request).await
            }
            outcome => outcome,
        }
    }
}

struct ProducerPart {
    name: String,
    state: Arc<ProducerState>,
    hub: Key<EventHub>,
}

impl DynPlugin for ProducerPart {
    fn name(&self) -> &str {
        &self.name
    }
    fn injects(&self) -> Vec<TypeKey> {
        // No injected dependency: a producer reads `eventHub` optionally with
        // `get_at`, so nothing reapplies it when EventHub does (design §2.1).
        Vec::new()
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let state = self.state.clone();
            let key = self.hub.clone();
            // Register the notice listener first, then read the service: a
            // service provided between the two steps is not missed (design
            // §2.1).
            {
                let state = state.clone();
                let key = key.clone();
                let listen_ctx = ctx.clone();
                ctx.listen::<dim_event_hub::Ready>().on(move |_, _| {
                    let state = state.clone();
                    let key = key.clone();
                    let ctx = listen_ctx.clone();
                    async move {
                        state.install(ctx.clone(), key);
                        if let Some(declaration) = state.declaration()
                            && let Err(error) = state.declare(declaration).await
                        {
                            tracing::warn!(%error, "the fixture producer could not declare");
                        }
                        Ok(None)
                    }
                })?;
            }
            state.install(ctx.clone(), key);
            if let Some(declaration) = state.declaration()
                && let Err(error) = state.declare(declaration).await
            {
                tracing::warn!(%error, "the fixture producer could not declare");
            }
            Ok(Effect::Done)
        })
    }
}

/// The declaration the fixture producer makes when it is mounted before
/// EventHub: `im` with `im.message`, delivered `queue` (design A.1).
pub fn im_declaration() -> DeclareRequest {
    DeclareRequest {
        source: "im".into(),
        guide: "The IM guide.".into(),
        types: vec![TypeDeclaration {
            r#type: "im.message".into(),
            description: "An instant message".into(),
            mode: InputMode::Queue,
        }],
    }
}

/// A mounted root and the handles a test drives it with.
pub struct Fixture {
    pub root: Ctx,
    pub dir: PathBuf,
    pub sessions: FakeSessions,
    pub producer: Arc<ProducerState>,
    pub management: Arc<ManagementMethods>,
    /// The `eventhub` row, kept so a test can restart it (design §2.1).
    hub: FiberView,
    hub_key: Key<EventHub>,
}

impl Fixture {
    pub async fn mount(dir: &Path) -> Self {
        Self::mount_with(dir, false).await
    }

    /// [`mount`](Self::mount), with the fixture producer mounted before
    /// EventHub: it applies with no service, reads none, and declares on
    /// `eventhub/ready` (design §2.1, Appendix D item 12).
    pub async fn mount_producer_first(dir: &Path) -> Self {
        Self::mount_with(dir, true).await
    }

    async fn mount_with(dir: &Path, producer_first: bool) -> Self {
        // The authorized root is opened component by component without
        // following links, so it must be the canonical path (`/private/var`
        // on macOS, not `/var`).
        let dir = dir.canonicalize().unwrap();
        // The host creates the plugin-files directory it authorizes
        // (`crates/dim-app-server/src/cli.rs` does this for the product).
        std::fs::create_dir_all(dir.join("eventhub")).unwrap();
        let root = Ctx::root().unwrap();
        (&root.plugin(FileStoragePlugin::new(vec![dir.clone()])))
            .await
            .unwrap();
        let mut catalog = dim_app_server::catalog();
        dim_agent_runtime::names::register::<EventHub>(&mut catalog, None);
        let build = rutis_loader::resolve::Build::default();
        let names = Names::copy(&catalog, &build);
        (&root.plugin(PluginFilesPlugin::new(&names, 0).unwrap()))
            .await
            .unwrap();
        (&root.plugin(ManagementMethodsPlugin::new(&names).unwrap()))
            .await
            .unwrap();
        root.provide_as::<PartRegistry>(Arc::new(PartRegistry::default()))
            .unwrap();
        let sessions = FakeSessions::default();
        root.provide_at::<dyn HostDispatch>(host_key("sessions"), Arc::new(sessions.clone()))
            .unwrap();
        let producer = Arc::new(ProducerState::default());
        let state = Arc::new(InstanceState::default());
        let hub_key = names.key().unwrap();
        let hub = if producer_first {
            // The producer applies while EventHub provides nothing: its
            // optional read finds no service, and it declares on the notice.
            producer.declare_on_find(im_declaration());
            (&root.plugin(ProducerPart {
                name: "EventHubFixtureProducer".into(),
                state: producer.clone(),
                hub: hub_key.clone(),
            }))
                .await
                .unwrap();
            let hub = root.plugin(
                EventHubPlugin::new(
                    &names,
                    Arc::new(dim_event_hub::EventHubTexts::default()),
                    state.clone(),
                )
                .unwrap(),
            );
            (&hub).await.unwrap();
            hub
        } else {
            let hub = root.plugin(
                EventHubPlugin::new(
                    &names,
                    Arc::new(dim_event_hub::EventHubTexts::default()),
                    state.clone(),
                )
                .unwrap(),
            );
            (&hub).await.unwrap();
            (&root.plugin(ProducerPart {
                name: "EventHubFixtureProducer".into(),
                state: producer.clone(),
                hub: hub_key.clone(),
            }))
                .await
                .unwrap();
            hub
        };
        let management = root.get::<ManagementMethods>().unwrap();
        Self {
            root,
            dir,
            sessions,
            producer,
            management,
            hub,
            hub_key,
        }
    }

    /// Restarts the `eventhub` row, as a reload of Agents, `pluginFiles`,
    /// `managementMethods` or `partRegistry` does: the plugin instance
    /// survives and `apply` runs again (design §2.1).
    pub async fn restart_hub(&self) {
        self.hub.restart().await.unwrap();
    }

    /// Mounts another producer row, whose context and declaration live until
    /// the returned view is shut down (design §2.1's row lifetime).
    pub async fn producer_row(&self, name: &str) -> (Arc<ProducerState>, rutis::host::FiberView) {
        let state = Arc::new(ProducerState::default());
        let view = self.root.plugin(ProducerPart {
            name: name.to_owned(),
            state: state.clone(),
            hub: self.hub_key.clone(),
        });
        (&view).await.unwrap();
        (state, view)
    }

    /// Calls one `eventhub.*` method through its provider, the way Rpc does.
    pub async fn call(&self, method: &str, params: Json) -> Result<Json, RpcError> {
        let provider = match self.management.owner(method) {
            Owner::Active(provider) => provider,
            _ => panic!("{method} has no provider"),
        };
        let accepted = provider
            .accept(
                method.to_owned(),
                params,
                None,
                CancellationToken::new(),
                |_| {},
            )
            .unwrap();
        let reply = accepted.reply.await.unwrap();
        let _ = accepted.settled.await;
        reply
    }

    /// The `eventhub.subscriptions` list.
    pub async fn subscriptions(&self, session: Option<&str>) -> Vec<Json> {
        let params = match session {
            Some(session) => json!({ "sessionId": session }),
            None => json!({}),
        };
        self.call("eventhub.subscriptions", params).await.unwrap()["subscriptions"]
            .as_array()
            .cloned()
            .unwrap_or_default()
    }

    pub async fn subscribe(
        &self,
        session: &str,
        types: &[&str],
        mode: Option<&str>,
    ) -> Result<String, RpcError> {
        let mut params = json!({
            "types": types,
            "target": { "sessionId": session, "branchId": "main" },
        });
        if let Some(mode) = mode {
            params["mode"] = json!(mode);
        }
        let reply = self.call("eventhub.subscribe", params).await?;
        Ok(reply["subscriptionId"].as_str().unwrap().to_owned())
    }

    /// Removes one subscription through its management method.
    pub async fn unsubscribe(&self, subscription_id: &str) -> Result<(), RpcError> {
        self.call(
            "eventhub.unsubscribe",
            json!({ "subscriptionId": subscription_id }),
        )
        .await?;
        Ok(())
    }

    /// The subscription file's path on disk.
    pub fn file(&self) -> PathBuf {
        self.dir.join("eventhub/subscriptions.json")
    }

    /// Makes the next subscription-file write fail and returns the undo: the
    /// `eventhub` directory loses its write bit, and its bytes stay as they
    /// are (design A.5's storage failure).
    pub fn block_writes(&self) -> BlockedWrites {
        let directory = self.dir.join("eventhub");
        std::fs::create_dir_all(&directory).unwrap();
        let before = std::fs::metadata(&directory)
            .map(|metadata| metadata.permissions())
            .ok();
        if let Some(permissions) = &before {
            use std::os::unix::fs::PermissionsExt;
            let mut read_only = permissions.clone();
            read_only.set_mode(0o500);
            std::fs::set_permissions(&directory, read_only).unwrap();
        }
        BlockedWrites { directory, before }
    }
}

pub struct BlockedWrites {
    directory: PathBuf,
    before: Option<std::fs::Permissions>,
}

impl Drop for BlockedWrites {
    fn drop(&mut self) {
        if let Some(before) = self.before.take() {
            let _ = std::fs::set_permissions(&self.directory, before);
        }
    }
}

/// A disposable plugin-files directory.
pub fn directory() -> tempfile::TempDir {
    let directory = tempfile::tempdir().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    directory
}

/// The product's assembly in a minimal host: the row builder builds EventHub
/// from its factory on every generation, so the state the factory captured is
/// the install's (design §2.1), while the `sessions` service is the fixture's
/// fake, so a test can gate, refuse and look up every input.
pub struct LoaderFixture {
    pub host: MinimalHost,
    pub sessions: FakeSessions,
    pub producer: Arc<ProducerState>,
    pub management: Arc<ManagementMethods>,
    dir: tempfile::TempDir,
}

impl LoaderFixture {
    /// Starts the host, with EventHub built by its factory.
    pub async fn mount() -> Self {
        Self::mount_with_session_files(None).await
    }

    /// [`mount`](Self::mount) with a `sessionFiles` service, whose puts a test
    /// controls (design §3.2).
    pub async fn mount_with_session_files(
        session_files: Option<Arc<dyn SessionFilesService>>,
    ) -> Self {
        let dir = directory();
        let path = dir.path().canonicalize().unwrap();
        std::fs::create_dir_all(path.join("eventhub")).unwrap();
        let mut builder = AgentBuilder::new(AgentConfig::default());
        // The product's catalog registers the names these rows build with
        // (`crates/dim-app-server/src/lib.rs`).
        *builder.catalog_mut() = dim_app_server::catalog();
        builder
            .plugin(|names| Ok(Box::new(PluginFilesPlugin::new(names, 0)?) as Box<dyn DynPlugin>));
        builder.plugin(|names| {
            Ok(Box::new(ManagementMethodsPlugin::new(names)?) as Box<dyn DynPlugin>)
        });
        dim_event_hub::install(&mut builder);
        let producer = Arc::new(ProducerState::default());
        let part = producer.clone();
        builder.plugin(move |names| {
            Ok(Box::new(ProducerPart {
                name: "EventHubFixtureProducer".into(),
                state: part.clone(),
                hub: names.key::<EventHub>()?,
            }) as Box<dyn DynPlugin>)
        });
        let sessions = FakeSessions::default();
        let fake = sessions.clone();
        let path = path.clone();
        let host = MinimalHost::start_with(&builder, move |root, _| {
            root.plugin(FileStoragePlugin::new(vec![path.clone()]));
            root.provide_at::<dyn HostDispatch>(host_key("sessions"), Arc::new(fake.clone()))
                .map_err(dim_agent_runtime::AgentError::from)?;
            if let Some(session_files) = session_files {
                root.provide_as::<SessionFilesHost>(Arc::new(SessionFilesHost(session_files)))
                    .map_err(dim_agent_runtime::AgentError::from)?;
            }
            Ok(())
        })
        .await
        .expect("the loader host assembles");
        wait_for("the eventHub service", || producer.hub().is_some()).await;
        let management = host.root().get::<ManagementMethods>().unwrap();
        Self {
            host,
            sessions,
            producer,
            management,
            dir,
        }
    }

    /// Calls one `eventhub.*` method through its provider, the way Rpc does.
    pub async fn call(&self, method: &str, params: Json) -> Result<Json, RpcError> {
        let provider = match self.management.owner(method) {
            Owner::Active(provider) => provider,
            _ => panic!("{method} has no provider"),
        };
        let accepted = provider
            .accept(
                method.to_owned(),
                params,
                None,
                CancellationToken::new(),
                |_| {},
            )
            .unwrap();
        let reply = accepted.reply.await.unwrap();
        let _ = accepted.settled.await;
        reply
    }

    pub async fn subscribe(
        &self,
        session: &str,
        types: &[&str],
        mode: Option<&str>,
    ) -> Result<String, RpcError> {
        let mut params = json!({
            "types": types,
            "target": { "sessionId": session, "branchId": "main" },
        });
        if let Some(mode) = mode {
            params["mode"] = json!(mode);
        }
        let reply = self.call("eventhub.subscribe", params).await?;
        Ok(reply["subscriptionId"].as_str().unwrap().to_owned())
    }

    pub async fn unsubscribe(&self, subscription_id: &str) -> Result<(), RpcError> {
        self.call(
            "eventhub.unsubscribe",
            json!({ "subscriptionId": subscription_id }),
        )
        .await?;
        Ok(())
    }

    pub async fn subscriptions(&self, session: Option<&str>) -> Vec<Json> {
        let params = match session {
            Some(session) => json!({ "sessionId": session }),
            None => json!({}),
        };
        self.call("eventhub.subscriptions", params).await.unwrap()["subscriptions"]
            .as_array()
            .cloned()
            .unwrap_or_default()
    }

    /// Emits the process notice a successful delete emits, from the host's
    /// root as Agents does (design §9.1).
    pub async fn session_deleted(&self, session: &str) {
        self.host
            .root()
            .parallel(dim_app_server::SessionDeleted {
                agent_id: "fixture-agent".into(),
                session_id: session.to_owned(),
            })
            .await
            .unwrap();
    }

    /// The subscription file's path on disk.
    pub fn file(&self) -> PathBuf {
        self.dir.path().join("eventhub/subscriptions.json")
    }
}

/// A `sessionFiles` service whose every put fails: the marker keeps
/// `truncated="true"` and the receipt `truncated: true` (design §3.2).
pub struct FailingSessionFiles;

fn failed() -> SessionFileError {
    SessionFileError {
        code: SessionFileErrorCode::Storage,
        message: "the fixture refuses every file".into(),
    }
}

impl SessionFilesService for FailingSessionFiles {
    fn put(
        &self,
        _: String,
        _: String,
        _: Vec<u8>,
        _: CancellationToken,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<StoredSessionFile, SessionFileError>>
                + Send
                + '_,
        >,
    > {
        Box::pin(async { Err(failed()) })
    }
    fn register(
        &self,
        _: String,
        _: String,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<(), SessionFileError>> + Send + '_>,
    > {
        Box::pin(async { Err(failed()) })
    }
    fn owns(
        &self,
        _: String,
        _: String,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<bool, SessionFileError>> + Send + '_>,
    > {
        Box::pin(async { Err(failed()) })
    }
    fn owned_paths(
        &self,
        _: String,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Vec<String>, SessionFileError>> + Send + '_>,
    > {
        Box::pin(async { Err(failed()) })
    }
}

/// A `sessionFiles` service that records every put and stores nothing: the
/// retry test counts the one put a body over the limit makes (design §3.2).
#[derive(Default)]
pub struct CountingSessionFiles {
    puts: Mutex<Vec<(String, String)>>,
}

impl CountingSessionFiles {
    /// The puts recorded so far, as `(session, name)`, in order.
    pub fn puts(&self) -> Vec<(String, String)> {
        self.puts.lock().unwrap().clone()
    }
}

impl SessionFilesService for CountingSessionFiles {
    fn put(
        &self,
        session_id: String,
        name: String,
        content: Vec<u8>,
        _: CancellationToken,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<StoredSessionFile, SessionFileError>>
                + Send
                + '_,
        >,
    > {
        self.puts
            .lock()
            .unwrap()
            .push((session_id.clone(), name.clone()));
        let bytes = content.len() as u64;
        Box::pin(async move {
            Ok(StoredSessionFile {
                file_id: format!("file-{session_id}-{name}"),
                path: format!("/{session_id}/{name}"),
                bytes,
            })
        })
    }
    fn register(
        &self,
        _: String,
        _: String,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<(), SessionFileError>> + Send + '_>,
    > {
        Box::pin(async { Ok(()) })
    }
    fn owns(
        &self,
        _: String,
        _: String,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<bool, SessionFileError>> + Send + '_>,
    > {
        Box::pin(async { Ok(false) })
    }
    fn owned_paths(
        &self,
        _: String,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Vec<String>, SessionFileError>> + Send + '_>,
    > {
        Box::pin(async { Ok(Vec::new()) })
    }
}

//! Web through actual C4, Settings, Permission and ToolExecutor boundaries.
use super::*;
use dim_agent_capabilities::tool_registry::{ToolProvider, ToolRegistry, ToolSourceSet};
use dim_agent_runtime::Names;
use dim_agent_runtime::testing::{branch_events, chain_events};
use dim_app_server::managed_provider::{
    AccountSnapshot, CredentialLease, ManagedCatalog, ManagedError, ManagedFuture, ManagedProvider,
    ManagedProviderBinding, ManagedProviderRow, ProviderDescriptor, WebSearchHit, WebSearchRequest,
    WebSearchResponse,
    management::{
        BrowserLogin, BrowserLoginBegin, DeviceLogin, LoginBegin, LoginState,
        ManagedBrowserLoginComplete, ManagedLoginAttempt, Usage,
    },
};
use rutis::host::{BoxFuture, DynPlugin, Effect, FiberView, Key, TypeKey};
use rutis::{Ctx, Plugin};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicUsize};
use tokio::io::AsyncReadExt;
use tokio::sync::watch;

struct SearchProvider {
    /// The account the owner stores; the test moves it directly, as a login
    /// completing without its client does.
    signed_in: watch::Sender<Option<bool>>,
    /// Account observations, which the assembly and reconciliation count.
    observations: Arc<AtomicUsize>,
    /// Account reads, which an installed watch must make unnecessary.
    reads: Arc<AtomicUsize>,
    /// Makes the next account read fail, as a transient store error does.
    fail_status: Arc<AtomicBool>,
}
fn unused<T: Send + 'static>() -> ManagedFuture<T> {
    Box::pin(async { Err(ManagedError::InvalidInput) })
}
fn account(signed_in: bool) -> AccountSnapshot {
    AccountSnapshot {
        credential_epoch: "epoch".into(),
        generation: u64::from(signed_in) + 1,
        account: signed_in.then(|| dim_app_server::managed_provider::AccountView {
            issuer: "https://accounts.example".into(),
            subject: "subject".into(),
            email: None,
            name: None,
            granted_scopes: vec![],
        }),
    }
}
impl ManagedProvider for SearchProvider {
    fn providers(&self) -> Vec<ProviderDescriptor> {
        vec![ProviderDescriptor {
            provider_id: "dimcode-api-oauth".into(),
            display_name: "Dim OAuth".into(),
            login_methods: vec!["device".into()],
            default_directory: None,
        }]
    }
    fn status(&self, _: &str) -> ManagedFuture<AccountSnapshot> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        let fail = self.fail_status.load(Ordering::SeqCst);
        let signed_in = self.signed_in.borrow().unwrap_or(false);
        Box::pin(async move {
            if fail {
                return Err(ManagedError::Transient);
            }
            Ok(account(signed_in))
        })
    }
    fn subscribe_account(&self) -> watch::Receiver<Option<bool>> {
        self.signed_in.subscribe()
    }
    fn observe_account(&self, _: &str) -> ManagedFuture<()> {
        self.observations.fetch_add(1, Ordering::SeqCst);
        let fail = self.fail_status.load(Ordering::SeqCst);
        let signed_in = self.signed_in.clone();
        Box::pin(async move {
            if fail {
                return Err(ManagedError::Transient);
            }
            let current = *signed_in.borrow();
            if let Some(usable) = current {
                signed_in.send_replace(Some(usable));
            }
            Ok(())
        })
    }
    fn recover(&self, _: &str) -> ManagedFuture<AccountSnapshot> {
        unused()
    }
    fn begin_browser(&self, _: BrowserLoginBegin) -> ManagedFuture<BrowserLogin> {
        unused()
    }
    fn complete_browser(&self, _: ManagedBrowserLoginComplete) -> ManagedFuture<AccountSnapshot> {
        unused()
    }
    fn begin_device(&self, _: LoginBegin) -> ManagedFuture<DeviceLogin> {
        unused()
    }
    fn attempt_status(&self, _: &ManagedLoginAttempt) -> Result<LoginState, ManagedError> {
        Err(ManagedError::AttemptNotFound)
    }
    fn cancel_attempt(&self, _: ManagedLoginAttempt) -> ManagedFuture<()> {
        unused()
    }
    fn logout(&self, _: &str) -> ManagedFuture<AccountSnapshot> {
        self.signed_in.send_replace(Some(false));
        Box::pin(async { Ok(account(false)) })
    }
    fn usage(&self, _: &str) -> ManagedFuture<Usage> {
        unused()
    }
    fn web_search(
        &self,
        req: WebSearchRequest,
        _: CancellationToken,
    ) -> ManagedFuture<WebSearchResponse> {
        let signed_in = self.signed_in.borrow().unwrap_or(false);
        Box::pin(async move {
            if !signed_in {
                return Err(ManagedError::TokenMissing);
            }
            if req.query == "fail" {
                return Err(ManagedError::Transient);
            }
            Ok(WebSearchResponse {
                results: vec![
                    WebSearchHit {
                        title: format!("<b>Managed {}</b>", req.query),
                        url: "https://managed.example.com/hit".into(),
                        snippet: "Managed <i>snippet</i>".into(),
                        published_at: Some("2026-10-28".into()),
                    },
                    WebSearchHit {
                        title: "Blocked".into(),
                        url: "https://sub.blocked.test/ignored".into(),
                        snippet: "ignored".into(),
                        published_at: None,
                    },
                ],
            })
        })
    }
    fn catalog(&self, _: &str, _: CancellationToken) -> ManagedFuture<ManagedCatalog> {
        unused()
    }
    fn credential(&self, _: &str, _: &str) -> ManagedFuture<CredentialLease> {
        unused()
    }
    fn recover_rejected(&self, _: &CredentialLease) -> ManagedFuture<CredentialLease> {
        unused()
    }
    fn finish_rejected(&self, _: &CredentialLease) -> ManagedFuture<()> {
        unused()
    }
    fn quiesce(&self) -> BoxFuture<'static, ()> {
        Box::pin(async {})
    }
}
struct SearchProviderPlugin {
    signed_in: watch::Sender<Option<bool>>,
    reads: Arc<AtomicUsize>,
    observations: Arc<AtomicUsize>,
    fail_status: Arc<AtomicBool>,
}
impl SearchProviderPlugin {
    fn new(signed_in: watch::Sender<Option<bool>>, reads: Arc<AtomicUsize>) -> Self {
        Self {
            signed_in,
            reads,
            observations: Arc::new(AtomicUsize::new(0)),
            fail_status: Arc::new(AtomicBool::new(false)),
        }
    }
    /// Also counts `observe_account` calls on `observations`.
    fn observing(mut self, observations: Arc<AtomicUsize>) -> Self {
        self.observations = observations;
        self
    }
    fn with_failure(mut self, fail_status: Arc<AtomicBool>) -> Self {
        self.fail_status = fail_status;
        self
    }
}
impl Plugin for SearchProviderPlugin {
    type Deps = ();
    fn name(&self) -> &str {
        "search-provider-fixture"
    }
    async fn apply(&self, ctx: &Ctx, (): ()) -> rutis::Result {
        ctx.provide_as::<dyn ManagedProvider>(Arc::new(SearchProvider {
            signed_in: self.signed_in.clone(),
            observations: self.observations.clone(),
            reads: self.reads.clone(),
            fail_status: self.fail_status.clone(),
        }))?;
        Ok(())
    }
}

/// A process row that installs a managed provider and its binding, as the
/// product profile does, and exposes both so a test can replace the binding
/// with one that already stores a signed-in account.
struct ProviderRow {
    slot: Arc<OnceLock<(Ctx, FiberView, FiberView)>>,
    signed_in: watch::Sender<Option<bool>>,
    reads: Arc<AtomicUsize>,
    fail: Arc<AtomicBool>,
}
impl DynPlugin for ProviderRow {
    fn name(&self) -> &str {
        "test.managed-provider-row"
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let provider = ctx.plugin(
                SearchProviderPlugin::new(self.signed_in.clone(), self.reads.clone())
                    .with_failure(self.fail.clone()),
            );
            let binding = ctx.plugin(ManagedProviderBinding);
            let _ = self.slot.set((ctx.clone(), provider, binding));
            Ok(Effect::Done)
        })
    }
}

/// A Session tool source that withholds every `sets()` read while armed.
///
/// The registry refreshes its sources, and the branch adopts them, from the
/// last read: a held read holds the adoption, so a test can write the `web`
/// source and still bind a request to the declaration from before it.
struct AdoptionGate {
    armed: AtomicBool,
    reads: watch::Sender<u64>,
    open: std::sync::Mutex<bool>,
    opened: std::sync::Condvar,
}
impl Default for AdoptionGate {
    fn default() -> Self {
        Self {
            armed: AtomicBool::new(false),
            reads: watch::Sender::new(0),
            open: std::sync::Mutex::new(true),
            opened: std::sync::Condvar::new(),
        }
    }
}
impl AdoptionGate {
    /// Holds every read from here on, and answers how many it held so far so
    /// the caller can wait for the next one.
    fn arm(&self) -> u64 {
        *self.open.lock().unwrap() = false;
        self.armed.store(true, Ordering::SeqCst);
        *self.reads.borrow()
    }
    fn release(&self) {
        *self.open.lock().unwrap() = true;
        self.opened.notify_all();
    }
    fn hold(&self) {
        if !self.armed.load(Ordering::SeqCst) {
            return;
        }
        tokio::task::block_in_place(|| {
            let open = self.open.lock().unwrap();
            if *open {
                return;
            }
            self.reads.send_modify(|reads| *reads += 1);
            drop(self.opened.wait_while(open, |open| !*open).unwrap());
        });
    }
    /// Waits until the registry read the held source after `since`: every
    /// earlier source change was announced, and none of them is applied yet.
    async fn reached(&self, since: u64) {
        let mut reads = self.reads.subscribe();
        tokio::time::timeout(Duration::from_secs(10), async {
            while *reads.borrow_and_update() <= since {
                reads.changed().await.expect("the gate outlives the read");
            }
        })
        .await
        .expect("the registry never read the gated tool source");
    }
}

struct GatedSource(Arc<AdoptionGate>);
impl ToolProvider for GatedSource {
    fn sets(&self) -> Vec<Arc<ToolSourceSet>> {
        self.0.hold();
        Vec::new()
    }
}

/// Releases the gate it holds when it goes out of scope: a failed assertion
/// must not leave a held read waiting for the runtime to shut down.
struct ReleaseOnDrop(Arc<AdoptionGate>);
impl Drop for ReleaseOnDrop {
    fn drop(&mut self) {
        self.0.release();
    }
}

struct GatedSourcePlugin {
    gate: Arc<AdoptionGate>,
    registry: Key<ToolRegistry>,
}
impl GatedSourcePlugin {
    fn new(names: &Names<'_>, gate: Arc<AdoptionGate>) -> rutis::Result<Self> {
        Ok(Self {
            gate,
            registry: names.key()?,
        })
    }
}
impl DynPlugin for GatedSourcePlugin {
    fn name(&self) -> &str {
        "test.adoption-gate"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.registry.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        let gate = self.gate.clone();
        Box::pin(async move {
            let registry = ctx.require_at(&self.registry)?;
            registry.register_source(ctx, "gate", Arc::new(GatedSource(gate)))?;
            Ok(Effect::Done)
        })
    }
}

/// The fixtures never inherit the host's own proxy settings.
fn disable_system_proxy() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| unsafe { std::env::set_var("DIM_WEB_SYSTEM_PROXY", "off") });
}

async fn configure(client: &mut Client, document: Value) {
    let read = client
        .rpc("settings.read", json!({"source":"web","scope":"user"}))
        .await;
    assert_ok(&read);
    assert_ok(
        &client
            .rpc(
                "settings.update",
                json!({"source":"web","scope":"user",
        "expectedRevision":read["result"]["revision"],"document":document}),
            )
            .await,
    );
}

async fn create_web(client: &mut Client, preset: &str, interactive: bool) -> String {
    let created = client
        .rpc(
            "session.create",
            json!({"cwd":std::env::temp_dir(),
        "tools":["web_search","web_fetch"],"interactive":interactive,
        "permissionPolicy":{"preset":preset,"allowedTools":[],"disallowedTools":[]}}),
        )
        .await;
    assert_ok(&created);
    let id = created["result"]["sessionId"].as_str().unwrap().to_owned();
    assert_ok(
        &client
            .rpc(
                "session.model.update",
                json!({"sessionId":id,"branchId":"main",
            "providerId":"018ec73f-0000-7000-8000-000000000011","modelId":"fixture"}),
            )
            .await,
    );
    id
}

async fn result(session: &dim_agent_runtime::Session) -> ToolResult {
    let events = chain_events(session, "main", ..).await.unwrap();
    events
        .iter()
        .rev()
        .find_map(|event| {
            if let EventKind::ToolResult(batch) = &event.kind {
                batch.results.first().map(|entry| entry.result.clone())
            } else {
                None
            }
        })
        .unwrap_or_else(|| {
            let outcome = events.iter().rev().find_map(|event| match &event.kind {
                EventKind::TurnEnd(data) => Some(format!("{:?}", data.outcome)),
                _ => None,
            });
            let text = events.iter().rev().find_map(|event| match &event.kind {
                EventKind::Text(data) => Some(data.delta.clone()),
                _ => None,
            });
            panic!(
                "tool result; keys {:?}; last turn {outcome:?}; last text {text:?}",
                events
                    .iter()
                    .map(|event| event.kind.key())
                    .collect::<Vec<_>>()
            )
        })
}

fn text(result: &ToolResult) -> &str {
    match &result.content {
        dim_agent_types::ToolContent::Text(text) => text,
        _ => panic!("Web returns text"),
    }
}

async fn submit(client: &mut Client, id: &str, name: &str, arguments: Value) {
    assert_ok(
        &client
            .rpc(
                "session.submit",
                json!({"sessionId":id,"branchId":"main",
        "text":format!("tool:{name} {arguments}")}),
            )
            .await,
    );
}

async fn idle(client: &mut Client, id: &str) {
    assert_ok(
        &client
            .rpc("session.idle", json!({"sessionId":id,"branchId":"main"}))
            .await,
    );
}

async fn say(client: &mut Client, id: &str, text: &str) {
    assert_ok(
        &client
            .rpc(
                "session.submit",
                json!({"sessionId":id,"branchId":"main","text":text}),
            )
            .await,
    );
    idle(client, id).await;
}

async fn create_with_tools(client: &mut Client, tools: Option<Value>) -> String {
    let mut params = json!({"cwd":std::env::temp_dir(),"interactive":false,
        "permissionPolicy":{"preset":"full-access","allowedTools":[],"disallowedTools":[]}});
    if let Some(tools) = tools {
        params["tools"] = tools;
    }
    let created = client.rpc("session.create", params.clone()).await;
    assert!(
        created.get("error").is_none(),
        "session.create {params} -> {created}"
    );
    let id = created["result"]["sessionId"].as_str().unwrap().to_owned();
    assert_ok(
        &client
            .rpc(
                "session.model.update",
                json!({"sessionId":id,"branchId":"main",
            "providerId":"018ec73f-0000-7000-8000-000000000011","modelId":"fixture"}),
            )
            .await,
    );
    id
}

/// Waits for the branch's adopted tool part to state whether `web_search` is
/// declared. Adoption runs in its own task; yielding lets it finish, so the
/// test never depends on a sleep.
async fn await_search_declared(session: &dim_agent_runtime::Session, present: bool) {
    use dim_agent_capabilities::tool_registry::ToolSourcesView;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let declared = chain_events(session, "main", ..)
            .await
            .unwrap()
            .iter()
            .rev()
            .find_map(|event| match &event.kind {
                EventKind::ContextConfigured(data) => Some(
                    data.settings
                        .tools
                        .as_ref()
                        .map(|tools| tools.iter().any(|tool| tool.name == "web_search")),
                ),
                _ => None,
            });
        if declared == Some(Some(present)) {
            return;
        }
        let sources = session
            .session_service::<ToolSourcesView>()
            .map(|view| view.get());
        assert!(
            tokio::time::Instant::now() < deadline,
            "web_search declaration did not become {present}: {declared:?}; sources {sources:?}"
        );
        tokio::task::yield_now().await;
    }
}

/// The newest turn's outcome on the main branch.
async fn last_turn_outcome(session: &dim_agent_runtime::Session) -> Option<String> {
    chain_events(session, "main", ..)
        .await
        .unwrap()
        .iter()
        .rev()
        .find_map(|event| match &event.kind {
            EventKind::TurnEnd(data) => Some(format!("{:?}", data.outcome)),
            _ => None,
        })
}

/// Every committed tool result on the main branch, oldest first.
async fn tool_results(session: &dim_agent_runtime::Session) -> Vec<ToolResult> {
    chain_events(session, "main", ..)
        .await
        .unwrap()
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::ToolResult(batch) => batch.results.first().map(|entry| entry.result.clone()),
            _ => None,
        })
        .collect()
}

/// Waits for the result after `before` committed results.
async fn next_tool_result(session: &dim_agent_runtime::Session, before: usize) -> ToolResult {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let results = tool_results(session).await;
        if let Some(result) = results.get(before) {
            return result.clone();
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "tool result did not arrive; {} committed",
            results.len()
        );
        tokio::task::yield_now().await;
    }
}

/// A text turn: its first request's declarations, once the turn and any later
/// request are drained.
async fn text_turn(
    client: &mut Client,
    id: &str,
    requests: &mut tokio::sync::mpsc::UnboundedReceiver<Vec<String>>,
    text: &str,
) -> Vec<String> {
    say(client, id, text).await;
    let names = requests.recv().await.unwrap();
    idle(client, id).await;
    while requests.try_recv().is_ok() {}
    names
}

/// A tool turn: its first request's declarations and its result.
async fn tool_turn(
    client: &mut Client,
    session: &dim_agent_runtime::Session,
    id: &str,
    requests: &mut tokio::sync::mpsc::UnboundedReceiver<Vec<String>>,
    name: &str,
    arguments: Value,
) -> (Vec<String>, ToolResult) {
    let before = tool_results(session).await.len();
    submit(client, id, name, arguments).await;
    let names = requests.recv().await.unwrap();
    let result = next_tool_result(session, before).await;
    idle(client, id).await;
    while requests.try_recv().is_ok() {}
    (names, result)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn web_search_is_declared_only_while_a_search_service_is_usable() {
    disable_system_proxy();
    let (base, stop, http) = brave_fixture().await;
    let home = std::fs::canonicalize("/tmp")
        .unwrap()
        .join(format!("dws-{}", uuid::Uuid::new_v4()));
    let signed_in = watch::Sender::new(Some(false));
    let transition = signed_in.clone();
    let reads = Arc::new(AtomicUsize::new(0));
    let reads_row = reads.clone();
    let config = ServerConfig {
        product_home: Some(home.clone()),
        target_home: Some(home.join("rust")),
        managed_provider: Some(ManagedProviderRow(Arc::new(move |builtins, _| {
            let (signed_in, reads) = (signed_in.clone(), reads_row.clone());
            builtins
                .builtins
                .register::<Value, _, _>("SearchProviderFixture", move |_| {
                    Ok(SearchProviderPlugin::new(signed_in.clone(), reads.clone()))
                });
            "SearchProviderFixture".to_owned()
        }))),
        ..Default::default()
    };
    let (server, mut requests, _) = super::session_typescript::start(config).await;
    let mut client = Client::new_version(&server, false, 3).await;
    assert_ok(&client.rpc("provider.create", json!({"providerId":"018ec73f-0000-7000-8000-000000000011","displayName":"Fixture","connection":{"driverKind":"openai-compatible","baseUrl":"http://localhost/v1","auth":{"type":"anonymous"}},"models":[{"modelId":"fixture","displayName":"fixture","status":"active",
            "capabilities":dim_agent_types::CatalogCapabilities::with_limits(128_000, 16_384)}],"enabled":true})).await);
    assert_ok(
        &client
            .rpc(
                "provider.select",
                json!({"providerId":"018ec73f-0000-7000-8000-000000000011","modelId":"fixture"}),
            )
            .await,
    );
    let id = create_with_tools(&mut client, Some(json!(["web_search", "web_fetch"]))).await;
    let session = server.agent().unwrap().get_session(&id).unwrap();

    let names = text_turn(&mut client, &id, &mut requests, "hello").await;
    assert!(!names.iter().any(|name| name == "web_search"), "{names:?}");
    assert!(names.iter().any(|name| name == "web_fetch"), "{names:?}");

    configure(
        &mut client,
        json!({"searchService":{"kind":"brave","endpoint":format!("{base}/search"),"apiKey":"fixture-web-key"}}),
    )
    .await;
    await_search_declared(&session, true).await;
    let (names, searched) = tool_turn(
        &mut client,
        &session,
        &id,
        &mut requests,
        "web_search",
        json!({"query":"configured"}),
    )
    .await;
    assert!(names.iter().any(|name| name == "web_search"), "{names:?}");
    assert_ne!(searched.is_error, Some(true), "{searched:?}");
    assert!(text(&searched).contains("https://example.com/"));

    configure(&mut client, json!({})).await;
    await_search_declared(&session, false).await;
    let names = text_turn(&mut client, &id, &mut requests, "after removal").await;
    assert!(!names.iter().any(|name| name == "web_search"), "{names:?}");

    transition.send_replace(Some(true));
    await_search_declared(&session, true).await;
    let (names, managed) = tool_turn(
        &mut client,
        &session,
        &id,
        &mut requests,
        "web_search",
        json!({"query":"managed"}),
    )
    .await;
    assert!(names.iter().any(|name| name == "web_search"), "{names:?}");
    assert_ne!(managed.is_error, Some(true), "{managed:?}");
    assert!(text(&managed).contains("https://managed.example.com/hit"));

    transition.send_replace(Some(false));
    await_search_declared(&session, false).await;
    let names = text_turn(&mut client, &id, &mut requests, "after logout").await;
    assert!(!names.iter().any(|name| name == "web_search"), "{names:?}");

    transition.send_replace(Some(true));
    await_search_declared(&session, true).await;
    let (_, failed) = tool_turn(
        &mut client,
        &session,
        &id,
        &mut requests,
        "web_search",
        json!({"query":"fail"}),
    )
    .await;
    assert_eq!(failed.is_error, Some(true));
    assert!(text(&failed).contains("search_failed"), "{failed:?}");
    let names = text_turn(&mut client, &id, &mut requests, "after a failure").await;
    assert!(
        names.iter().any(|name| name == "web_search"),
        "a call-time failure keeps the declaration: {names:?}"
    );

    let narrow = create_with_tools(&mut client, Some(json!(["web_fetch"]))).await;
    let names = text_turn(&mut client, &narrow, &mut requests, "narrow").await;
    assert!(!names.iter().any(|name| name == "web_search"), "{names:?}");
    assert!(names.iter().any(|name| name == "web_fetch"), "{names:?}");

    stop.cancel();
    http.await.unwrap();
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// A Brave-compatible search endpoint, its base URL, and the stop that ends
/// it.
async fn brave_fixture() -> (String, CancellationToken, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let stop = CancellationToken::new();
    let stopping = stop.clone();
    let http = tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = stopping.cancelled() => return,
                accepted = listener.accept() => {
                    let (mut socket, _) = accepted.unwrap();
                    tokio::spawn(async move {
                        let mut header = Vec::new();
                        let mut chunk = [0; 4096];
                        while !header.windows(4).any(|part| part == b"\r\n\r\n") {
                            let n = socket.read(&mut chunk).await.unwrap();
                            if n == 0 { return; }
                            header.extend_from_slice(&chunk[..n]);
                        }
                        let body = r#"{"type":"search","web":{"results":[{"title":"fixture","url":"https://example.com/","description":"snippet"}]}}"#;
                        let response = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        );
                        socket.write_all(response.as_bytes()).await.unwrap();
                    });
                }
            }
        }
    });
    (base, stop, http)
}

/// A fixture model provider selected for the process and a Session that mounts
/// both Web tools.
async fn provider_ready(client: &mut Client) {
    assert_ok(&client.rpc("provider.create", json!({"providerId":"018ec73f-0000-7000-8000-000000000011","displayName":"Fixture","connection":{"driverKind":"openai-compatible","baseUrl":"http://localhost/v1","auth":{"type":"anonymous"}},"models":[{"modelId":"fixture","displayName":"fixture","status":"active",
            "capabilities":dim_agent_types::CatalogCapabilities::with_limits(128_000, 16_384)}],"enabled":true})).await);
    assert_ok(
        &client
            .rpc(
                "provider.select",
                json!({"providerId":"018ec73f-0000-7000-8000-000000000011","modelId":"fixture"}),
            )
            .await,
    );
}
async fn provider_session(client: &mut Client) -> String {
    provider_ready(client).await;
    create_with_tools(client, Some(json!(["web_search", "web_fetch"]))).await
}

/// Adding a service changes the declaration at the request the branch adopts
/// it for: a request bound before that adoption does not see `web_search`, and
/// the next one declares it and calls it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn web_search_is_declared_at_the_next_request_after_the_added_service_is_adopted() {
    disable_system_proxy();
    let (base, stop, http) = brave_fixture().await;
    let home = std::fs::canonicalize("/tmp")
        .unwrap()
        .join(format!("dwa-{}", uuid::Uuid::new_v4()));
    let gate = Arc::new(AdoptionGate::default());
    let install = gate.clone();
    let (server, mut requests, _) = super::session_typescript::start_with(
        ServerConfig {
            product_home: Some(home.clone()),
            target_home: Some(home.join("rust")),
            ..Default::default()
        },
        move |builder| {
            builder.require_feature(
                Feature::new("test.adoption-gate").session("test.adoption-gate", move |names| {
                    Ok(Box::new(GatedSourcePlugin::new(names, install.clone())?))
                }),
            );
        },
    )
    .await;
    let mut client = Client::new_version(&server, false, 3).await;
    let id = provider_session(&mut client).await;
    let session = server.agent().unwrap().get_session(&id).unwrap();
    let names = text_turn(&mut client, &id, &mut requests, "hello").await;
    assert!(!names.iter().any(|name| name == "web_search"), "{names:?}");

    // The request after the first adopted source change carries a tool part;
    // from then on a declaration follows the branch's adoption, not a read of
    // the registry, which is what the gate below holds.
    configure(
        &mut client,
        json!({"searchService":{"kind":"brave","endpoint":format!("{base}/search"),"apiKey":"fixture-web-key"}}),
    )
    .await;
    await_search_declared(&session, true).await;
    configure(&mut client, json!({})).await;
    await_search_declared(&session, false).await;

    let since = gate.arm();
    let _released = ReleaseOnDrop(gate.clone());
    configure(
        &mut client,
        json!({"searchService":{"kind":"brave","endpoint":format!("{base}/search"),"apiKey":"fixture-web-key"}}),
    )
    .await;
    gate.reached(since).await;
    let names = text_turn(&mut client, &id, &mut requests, "before the adoption").await;
    assert!(
        !names.iter().any(|name| name == "web_search"),
        "a request bound before the adoption must not see the added service: {names:?}"
    );

    gate.release();
    await_search_declared(&session, true).await;
    let (names, searched) = tool_turn(
        &mut client,
        &session,
        &id,
        &mut requests,
        "web_search",
        json!({"query":"configured"}),
    )
    .await;
    assert!(names.iter().any(|name| name == "web_search"), "{names:?}");
    assert_ne!(searched.is_error, Some(true), "{searched:?}");
    assert!(text(&searched).contains("https://example.com/"));

    stop.cancel();
    http.await.unwrap();
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// Removing a service changes the declaration at the request the branch adopts
/// it for: a request bound before that adoption still declares `web_search`,
/// its call finds no service, and the next request does not declare it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn web_search_stays_declared_with_a_failing_call_until_the_removed_service_is_adopted() {
    disable_system_proxy();
    let (base, stop, http) = brave_fixture().await;
    let home = std::fs::canonicalize("/tmp")
        .unwrap()
        .join(format!("dwr-{}", uuid::Uuid::new_v4()));
    let gate = Arc::new(AdoptionGate::default());
    let install = gate.clone();
    let signed_in = watch::Sender::new(Some(false));
    let transition = signed_in.clone();
    let reads = Arc::new(AtomicUsize::new(0));
    let reads_row = reads.clone();
    let (server, mut requests, _) = super::session_typescript::start_with(
        ServerConfig {
            product_home: Some(home.clone()),
            target_home: Some(home.join("rust")),
            managed_provider: Some(ManagedProviderRow(Arc::new(move |builtins, _| {
                let (signed_in, reads) = (signed_in.clone(), reads_row.clone());
                builtins
                    .builtins
                    .register::<Value, _, _>("SearchProviderFixture", move |_| {
                        Ok(SearchProviderPlugin::new(signed_in.clone(), reads.clone()))
                    });
                "SearchProviderFixture".to_owned()
            }))),
            ..Default::default()
        },
        move |builder| {
            builder.require_feature(
                Feature::new("test.adoption-gate").session("test.adoption-gate", move |names| {
                    Ok(Box::new(GatedSourcePlugin::new(names, install.clone())?))
                }),
            );
        },
    )
    .await;
    let mut client = Client::new_version(&server, false, 3).await;
    let id = provider_session(&mut client).await;
    let session = server.agent().unwrap().get_session(&id).unwrap();
    configure(
        &mut client,
        json!({"searchService":{"kind":"brave","endpoint":format!("{base}/search"),"apiKey":"fixture-web-key"}}),
    )
    .await;
    await_search_declared(&session, true).await;
    let (names, searched) = tool_turn(
        &mut client,
        &session,
        &id,
        &mut requests,
        "web_search",
        json!({"query":"configured"}),
    )
    .await;
    assert!(names.iter().any(|name| name == "web_search"), "{names:?}");
    assert_ne!(searched.is_error, Some(true), "{searched:?}");

    let since = gate.arm();
    let _released = ReleaseOnDrop(gate.clone());
    configure(&mut client, json!({})).await;
    gate.reached(since).await;
    let (names, removed) = tool_turn(
        &mut client,
        &session,
        &id,
        &mut requests,
        "web_search",
        json!({"query":"after the removal"}),
    )
    .await;
    assert!(
        names.iter().any(|name| name == "web_search"),
        "a request bound before the adoption keeps the declaration: {names:?}"
    );
    assert_eq!(removed.is_error, Some(true), "{removed:?}");
    assert!(
        text(&removed).contains("search_unavailable"),
        "a bound request's call must report the missing service: {removed:?}"
    );

    gate.release();
    await_search_declared(&session, false).await;
    let names = text_turn(&mut client, &id, &mut requests, "after the adoption").await;
    assert!(!names.iter().any(|name| name == "web_search"), "{names:?}");

    transition.send_replace(Some(true));
    await_search_declared(&session, true).await;
    let since = gate.arm();
    transition.send_replace(Some(false));
    gate.reached(since).await;
    let (names, signed_out) = tool_turn(
        &mut client,
        &session,
        &id,
        &mut requests,
        "web_search",
        json!({"query":"after the sign-out"}),
    )
    .await;
    assert!(
        names.iter().any(|name| name == "web_search"),
        "a request bound before the sign-out's adoption keeps the declaration: {names:?}"
    );
    assert!(
        text(&signed_out).contains("search_unavailable"),
        "a bound request's call must report the signed-out account: {signed_out:?}"
    );

    gate.release();
    await_search_declared(&session, false).await;
    let names = text_turn(
        &mut client,
        &id,
        &mut requests,
        "after the sign-out's adoption",
    )
    .await;
    assert!(!names.iter().any(|name| name == "web_search"), "{names:?}");

    stop.cancel();
    http.await.unwrap();
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// An account transition the provider reports by itself reaches an open
/// Session's next request: nothing reads the account or a login attempt after
/// the owner changes it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn web_search_follows_an_owner_account_transition_without_an_account_read() {
    disable_system_proxy();
    let home = std::fs::canonicalize("/tmp")
        .unwrap()
        .join(format!("dwt-{}", uuid::Uuid::new_v4()));
    let signed_in = watch::Sender::new(Some(false));
    let transition = signed_in.clone();
    let reads = Arc::new(AtomicUsize::new(0));
    let reads_row = reads.clone();
    let config = ServerConfig {
        product_home: Some(home.clone()),
        target_home: Some(home.join("rust")),
        managed_provider: Some(ManagedProviderRow(Arc::new(move |builtins, _| {
            let (signed_in, reads) = (signed_in.clone(), reads_row.clone());
            builtins
                .builtins
                .register::<Value, _, _>("SearchProviderFixture", move |_| {
                    Ok(SearchProviderPlugin::new(signed_in.clone(), reads.clone()))
                });
            "SearchProviderFixture".to_owned()
        }))),
        ..Default::default()
    };
    let (server, mut requests, _) = super::session_typescript::start(config).await;
    let mut client = Client::new_version(&server, false, 3).await;
    assert_ok(&client.rpc("provider.create", json!({"providerId":"018ec73f-0000-7000-8000-000000000011","displayName":"Fixture","connection":{"driverKind":"openai-compatible","baseUrl":"http://localhost/v1","auth":{"type":"anonymous"}},"models":[{"modelId":"fixture","displayName":"fixture","status":"active",
            "capabilities":dim_agent_types::CatalogCapabilities::with_limits(128_000, 16_384)}],"enabled":true})).await);
    assert_ok(
        &client
            .rpc(
                "provider.select",
                json!({"providerId":"018ec73f-0000-7000-8000-000000000011","modelId":"fixture"}),
            )
            .await,
    );
    let id = create_with_tools(&mut client, Some(json!(["web_search", "web_fetch"]))).await;
    let session = server.agent().unwrap().get_session(&id).unwrap();
    let names = text_turn(&mut client, &id, &mut requests, "hello").await;
    assert!(!names.iter().any(|name| name == "web_search"), "{names:?}");
    assert!(names.iter().any(|name| name == "web_fetch"), "{names:?}");

    let observed = reads.load(Ordering::SeqCst);
    transition.send_replace(Some(true));
    await_search_declared(&session, true).await;
    let (names, managed) = tool_turn(
        &mut client,
        &session,
        &id,
        &mut requests,
        "web_search",
        json!({"query":"managed"}),
    )
    .await;
    assert!(names.iter().any(|name| name == "web_search"), "{names:?}");
    assert_ne!(managed.is_error, Some(true), "{managed:?}");
    assert!(text(&managed).contains("https://managed.example.com/hit"));

    transition.send_replace(Some(false));
    await_search_declared(&session, false).await;
    let names = text_turn(&mut client, &id, &mut requests, "after the sign-out").await;
    assert!(!names.iter().any(|name| name == "web_search"), "{names:?}");
    assert_eq!(
        reads.load(Ordering::SeqCst),
        observed,
        "an account transition must be published from the change, not read back"
    );

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// Replacing the bound provider with one that already stores a signed-in
/// account publishes that account at install: an open Session's next adopted
/// request declares and calls `web_search`, with no account read.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn web_search_is_declared_after_a_signed_in_binding_replaces_a_signed_out_one() {
    disable_system_proxy();
    let home = std::fs::canonicalize("/tmp")
        .unwrap()
        .join(format!("dwv-{}", uuid::Uuid::new_v4()));
    let signed_out = watch::Sender::new(Some(false));
    let signed_in = watch::Sender::new(Some(true));
    let reads = Arc::new(AtomicUsize::new(0));
    let slot = Arc::new(OnceLock::new());
    let row = slot.clone();
    let start = signed_out.clone();
    let first_reads = reads.clone();
    let (server, mut requests, _) = super::session_typescript::start_with(
        ServerConfig {
            product_home: Some(home.clone()),
            target_home: Some(home.join("rust")),
            ..Default::default()
        },
        move |builder| {
            builder.plugin(move |_| {
                Ok(Box::new(ProviderRow {
                    slot: row.clone(),
                    signed_in: start.clone(),
                    reads: first_reads.clone(),
                    fail: Arc::new(AtomicBool::new(false)),
                }))
            });
        },
    )
    .await;
    let mut client = Client::new_version(&server, false, 3).await;
    let id = provider_session(&mut client).await;
    let session = server.agent().unwrap().get_session(&id).unwrap();
    let names = text_turn(&mut client, &id, &mut requests, "hello").await;
    assert!(!names.iter().any(|name| name == "web_search"), "{names:?}");
    assert_eq!(
        reads.load(Ordering::SeqCst),
        0,
        "a watched provider answers without an account read"
    );

    let (ctx, provider, _binding) = slot.get().expect("the provider row was not installed");
    provider.shutdown().await.unwrap();
    ctx.plugin(SearchProviderPlugin::new(signed_in.clone(), reads.clone()));
    await_search_declared(&session, true).await;

    let (names, managed) = tool_turn(
        &mut client,
        &session,
        &id,
        &mut requests,
        "web_search",
        json!({"query":"managed"}),
    )
    .await;
    assert!(names.iter().any(|name| name == "web_search"), "{names:?}");
    assert_ne!(managed.is_error, Some(true), "{managed:?}");
    assert!(text(&managed).contains("https://managed.example.com/hit"));
    assert_eq!(
        reads.load(Ordering::SeqCst),
        0,
        "installing the binding must publish its initialized account"
    );

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// Replacing the bound provider with one that has not read its account yet
/// withdraws the declaration the retired binding published, and only the new
/// binding's own watch decides the declaration afterwards.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn web_search_is_withdrawn_when_a_replacement_binding_has_not_observed_its_account() {
    disable_system_proxy();
    let home = std::fs::canonicalize("/tmp")
        .unwrap()
        .join(format!("dwx-{}", uuid::Uuid::new_v4()));
    let signed_in = watch::Sender::new(Some(true));
    let reads = Arc::new(AtomicUsize::new(0));
    let slot = Arc::new(OnceLock::new());
    let row = slot.clone();
    let start = signed_in.clone();
    let first_reads = reads.clone();
    let (server, mut requests, _) = super::session_typescript::start_with(
        ServerConfig {
            product_home: Some(home.clone()),
            target_home: Some(home.join("rust")),
            ..Default::default()
        },
        move |builder| {
            builder.plugin(move |_| {
                Ok(Box::new(ProviderRow {
                    slot: row.clone(),
                    signed_in: start.clone(),
                    reads: first_reads.clone(),
                    fail: Arc::new(AtomicBool::new(false)),
                }))
            });
        },
    )
    .await;
    let mut client = Client::new_version(&server, false, 3).await;
    let id = provider_session(&mut client).await;
    let session = server.agent().unwrap().get_session(&id).unwrap();
    let names = text_turn(&mut client, &id, &mut requests, "hello").await;
    assert!(names.iter().any(|name| name == "web_search"), "{names:?}");

    let (ctx, _provider, _binding) = slot.get().expect("the provider row was not installed");
    signed_in.send_replace(None);
    ctx.plugin(ManagedProviderBinding);

    await_web_source(&session, false).await;
    let names = text_turn(&mut client, &id, &mut requests, "after the replacement").await;
    assert!(!names.iter().any(|name| name == "web_search"), "{names:?}");

    let second = create_with_tools(&mut client, Some(json!(["web_search", "web_fetch"]))).await;
    let second_session = server.agent().unwrap().get_session(&second).unwrap();
    let names = text_turn(&mut client, &second, &mut requests, "another Session").await;
    assert!(
        !names.iter().any(|name| name == "web_search"),
        "an unobserved binding must not inherit the retired one's account: {names:?}"
    );

    signed_in.send_replace(Some(true));
    await_web_source(&session, true).await;
    await_web_source(&second_session, true).await;
    await_search_declared(&session, true).await;
    await_search_declared(&second_session, true).await;
    let names = text_turn(&mut client, &id, &mut requests, "the new binding signed in").await;
    assert!(names.iter().any(|name| name == "web_search"), "{names:?}");

    signed_in.send_replace(Some(false));
    await_web_source(&session, false).await;
    await_web_source(&second_session, false).await;
    await_search_declared(&second_session, false).await;
    let names = text_turn(
        &mut client,
        &second,
        &mut requests,
        "the new binding signed out",
    )
    .await;
    assert!(!names.iter().any(|name| name == "web_search"), "{names:?}");

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// A replacement binding whose first account observation fails still does not
/// inherit the retired binding's signed-in state; its own watch alone decides
/// the declaration afterwards.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_failed_initial_account_observation_does_not_inherit_the_previous_binding() {
    disable_system_proxy();
    let home = std::fs::canonicalize("/tmp")
        .unwrap()
        .join(format!("dwy-{}", uuid::Uuid::new_v4()));
    let signed_in = watch::Sender::new(Some(true));
    let reads = Arc::new(AtomicUsize::new(0));
    let slot = Arc::new(OnceLock::new());
    let row = slot.clone();
    let start = signed_in.clone();
    let first_reads = reads.clone();
    let (server, mut requests, _) = super::session_typescript::start_with(
        ServerConfig {
            product_home: Some(home.clone()),
            target_home: Some(home.join("rust")),
            ..Default::default()
        },
        move |builder| {
            builder.plugin(move |_| {
                Ok(Box::new(ProviderRow {
                    slot: row.clone(),
                    signed_in: start.clone(),
                    reads: first_reads.clone(),
                    fail: Arc::new(AtomicBool::new(true)),
                }))
            });
        },
    )
    .await;
    let mut client = Client::new_version(&server, false, 3).await;
    let id = provider_session(&mut client).await;
    let session = server.agent().unwrap().get_session(&id).unwrap();
    let names = text_turn(&mut client, &id, &mut requests, "hello").await;
    assert!(names.iter().any(|name| name == "web_search"), "{names:?}");

    let (ctx, _provider, _binding) = slot.get().expect("the provider row was not installed");
    signed_in.send_replace(None);
    ctx.plugin(ManagedProviderBinding);

    await_web_source(&session, false).await;
    let second = create_with_tools(&mut client, Some(json!(["web_search", "web_fetch"]))).await;
    let second_session = server.agent().unwrap().get_session(&second).unwrap();
    let names = text_turn(
        &mut client,
        &second,
        &mut requests,
        "a failed first observation",
    )
    .await;
    assert!(
        !names.iter().any(|name| name == "web_search"),
        "a failed observation must not inherit the retired account: {names:?}"
    );

    signed_in.send_replace(Some(true));
    await_web_source(&session, true).await;
    await_web_source(&second_session, true).await;
    await_search_declared(&second_session, true).await;
    let names = text_turn(
        &mut client,
        &second,
        &mut requests,
        "the binding published its state",
    )
    .await;
    assert!(names.iter().any(|name| name == "web_search"), "{names:?}");

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// An open Session that switches from an explicit adapter to the default
/// managed search observes the account once, so a signed-in account keeps the
/// declaration instead of being hidden until another Session reads it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn switching_an_open_session_to_managed_search_observes_the_account() {
    disable_system_proxy();
    let home = std::fs::canonicalize("/tmp")
        .unwrap()
        .join(format!("dwz-{}", uuid::Uuid::new_v4()));
    let signed_in = watch::Sender::new(Some(true));
    let reads = Arc::new(AtomicUsize::new(0));
    let fail = Arc::new(AtomicBool::new(false));
    let signed_row = signed_in.clone();
    let reads_row = reads.clone();
    let fail_row = fail.clone();
    let config = ServerConfig {
        product_home: Some(home.clone()),
        target_home: Some(home.join("rust")),
        managed_provider: Some(ManagedProviderRow(Arc::new(move |builtins, _| {
            let (signed_in, reads, fail) =
                (signed_row.clone(), reads_row.clone(), fail_row.clone());
            builtins
                .builtins
                .register::<Value, _, _>("SearchProviderFixture", move |_| {
                    Ok(SearchProviderPlugin::new(signed_in.clone(), reads.clone())
                        .observing(reads.clone())
                        .with_failure(fail.clone()))
                });
            "SearchProviderFixture".to_owned()
        }))),
        ..Default::default()
    };
    let (server, mut requests, _) = super::session_typescript::start(config).await;
    let mut client = Client::new_version(&server, false, 3).await;
    provider_ready(&mut client).await;
    configure(
        &mut client,
        json!({"searchService":{"kind":"brave","endpoint":"http://127.0.0.1:1/search","apiKey":"scenario-eval"}}),
    )
    .await;
    let id = create_with_tools(&mut client, Some(json!(["web_search", "web_fetch"]))).await;
    let names = text_turn(&mut client, &id, &mut requests, "the explicit adapter").await;
    assert!(names.iter().any(|name| name == "web_search"), "{names:?}");
    assert_eq!(
        reads.load(Ordering::SeqCst),
        0,
        "an explicit adapter answers without an account read"
    );

    configure(&mut client, json!({})).await;
    await_reads(&reads, 1).await;
    let names = text_turn(&mut client, &id, &mut requests, "the default service").await;
    assert!(
        names.iter().any(|name| name == "web_search"),
        "switching to managed search must observe the signed-in account: {names:?}"
    );

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

async fn await_web_source(session: &dim_agent_runtime::Session, present: bool) {
    use dim_agent_capabilities::tool_registry::ToolSourcesView;
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let has = session
                .session_service::<ToolSourcesView>()
                .map(|view| view.get().0.contains_key("web"))
                .unwrap_or(false);
            if has == present {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("the web source did not become {present}"));
}

async fn await_reads(reads: &AtomicUsize, expected: usize) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while reads.load(Ordering::SeqCst) < expected {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the account was not read");
}

/// A Session's assembly reads the account only when its declaration depends on
/// it: a fetch-only Session and a Session with an explicit search adapter do
/// not.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_web_assembly_reads_the_account_only_when_managed_search_is_needed() {
    disable_system_proxy();
    let home = std::fs::canonicalize("/tmp")
        .unwrap()
        .join(format!("dwm-{}", uuid::Uuid::new_v4()));
    let signed_in = watch::Sender::new(Some(true));
    let reads = Arc::new(AtomicUsize::new(0));
    let fail = Arc::new(AtomicBool::new(false));
    let signed_row = signed_in.clone();
    let reads_row = reads.clone();
    let fail_row = fail.clone();
    let config = ServerConfig {
        product_home: Some(home.clone()),
        target_home: Some(home.join("rust")),
        managed_provider: Some(ManagedProviderRow(Arc::new(move |builtins, _| {
            let (signed_in, reads, fail) =
                (signed_row.clone(), reads_row.clone(), fail_row.clone());
            builtins
                .builtins
                .register::<Value, _, _>("SearchProviderFixture", move |_| {
                    Ok(SearchProviderPlugin::new(signed_in.clone(), reads.clone())
                        .observing(reads.clone())
                        .with_failure(fail.clone()))
                });
            "SearchProviderFixture".to_owned()
        }))),
        ..Default::default()
    };
    let (server, _, _) = super::session_typescript::start(config).await;
    let mut client = Client::new_version(&server, false, 3).await;
    provider_ready(&mut client).await;

    create_with_tools(&mut client, Some(json!(["web_fetch"]))).await;
    assert_eq!(
        reads.load(Ordering::SeqCst),
        0,
        "a fetch-only Session must not read the account"
    );

    configure(
        &mut client,
        json!({"searchService":{"kind":"brave","endpoint":"http://127.0.0.1:1/search","apiKey":"scenario-eval"}}),
    )
    .await;
    create_with_tools(&mut client, Some(json!(["web_search", "web_fetch"]))).await;
    assert_eq!(
        reads.load(Ordering::SeqCst),
        0,
        "an explicit adapter answers without an account read"
    );

    configure(&mut client, json!({})).await;
    await_reads(&reads, 1).await;
    assert_eq!(
        reads.load(Ordering::SeqCst),
        1,
        "switching the open Session to the default service observes the account once"
    );
    create_with_tools(&mut client, Some(json!(["web_search", "web_fetch"]))).await;
    await_reads(&reads, 2).await;
    assert_eq!(
        reads.load(Ordering::SeqCst),
        2,
        "the default service reads the account once for the new Session"
    );

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// A failed account read leaves the last observation: a second Session's
/// transient failure must not withdraw an open Session's search.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_failed_account_read_keeps_an_open_session_search_declared() {
    disable_system_proxy();
    let home = std::fs::canonicalize("/tmp")
        .unwrap()
        .join(format!("dwf-{}", uuid::Uuid::new_v4()));
    let signed_in = watch::Sender::new(Some(true));
    let reads = Arc::new(AtomicUsize::new(0));
    let fail = Arc::new(AtomicBool::new(false));
    let signed_row = signed_in.clone();
    let reads_row = reads.clone();
    let fail_row = fail.clone();
    let config = ServerConfig {
        product_home: Some(home.clone()),
        target_home: Some(home.join("rust")),
        managed_provider: Some(ManagedProviderRow(Arc::new(move |builtins, _| {
            let (signed_in, reads, fail) =
                (signed_row.clone(), reads_row.clone(), fail_row.clone());
            builtins
                .builtins
                .register::<Value, _, _>("SearchProviderFixture", move |_| {
                    Ok(SearchProviderPlugin::new(signed_in.clone(), reads.clone())
                        .observing(reads.clone())
                        .with_failure(fail.clone()))
                });
            "SearchProviderFixture".to_owned()
        }))),
        ..Default::default()
    };
    let (server, mut requests, _) = super::session_typescript::start(config).await;
    let mut client = Client::new_version(&server, false, 3).await;
    provider_ready(&mut client).await;
    let first = create_with_tools(&mut client, Some(json!(["web_search", "web_fetch"]))).await;
    let first_session = server.agent().unwrap().get_session(&first).unwrap();
    let names = text_turn(&mut client, &first, &mut requests, "hello").await;
    assert!(names.iter().any(|name| name == "web_search"), "{names:?}");

    fail.store(true, Ordering::SeqCst);
    create_with_tools(&mut client, Some(json!(["web_search", "web_fetch"]))).await;
    assert_eq!(
        reads.load(Ordering::SeqCst),
        2,
        "the second Session's initial account read failed"
    );
    let managed = server
        .service::<dim_app_server::managed_provider::ManagedSearch>()
        .unwrap();
    assert!(
        managed.usable(),
        "a failed read must not withdraw the last account observation"
    );

    let (names, searched) = tool_turn(
        &mut client,
        &first_session,
        &first,
        &mut requests,
        "web_search",
        json!({"query":"managed"}),
    )
    .await;
    assert!(
        names.iter().any(|name| name == "web_search"),
        "a failed read in another Session must not withdraw an open Session's search: {names:?}"
    );
    assert_ne!(searched.is_error, Some(true), "{searched:?}");
    let names = text_turn(&mut client, &first, &mut requests, "after the failed read").await;
    assert!(
        names.iter().any(|name| name == "web_search"),
        "the withdrawal must not reach later requests either: {names:?}"
    );

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn web_settings_permission_cancellation_and_restore() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let requests = Arc::new(AtomicUsize::new(0));
    let request_count = requests.clone();
    let (slow, mut entered) = tokio::sync::mpsc::unbounded_channel();
    let (closed, mut disconnected) = tokio::sync::mpsc::unbounded_channel();
    let stop = CancellationToken::new();
    let stopping = stop.clone();
    let http = tokio::spawn(async move {
        let mut tasks = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                _ = stopping.cancelled() => break,
                Some(done) = tasks.join_next() => { done.unwrap(); },
                accepted = listener.accept() => {
                    let (mut socket, _) = accepted.unwrap();
                    let count = request_count.clone();
                    let slow = slow.clone();
                    let closed = closed.clone();
                    tasks.spawn(async move {
                        let mut header = Vec::new();
                        let mut chunk = [0; 4096];
                        while !header.windows(4).any(|part| part == b"\r\n\r\n") {
                            let n = socket.read(&mut chunk).await.unwrap();
                            if n == 0 { return; }
                            header.extend_from_slice(&chunk[..n]);
                            assert!(header.len() < 16 * 1024);
                        }
                        let request = String::from_utf8(header).unwrap();
                        assert!(request.starts_with("GET "), "{request}");
                        count.fetch_add(1, Ordering::SeqCst);
                        if request.starts_with("GET /slow ") {
                            socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nConnection: close\r\n\r\npending").await.unwrap();
                            slow.send(()).unwrap();
                            assert_eq!(socket.read(&mut chunk).await.unwrap(), 0);
                            closed.send(()).unwrap();
                            return;
                        }
                        if request.starts_with("GET /deflate/") {
                            use flate2::{Compression, write::{DeflateEncoder, ZlibEncoder}};
                            use std::io::Write;
                            let path = request.split_whitespace().nth(1).unwrap();
                            let raw = path.contains("/raw/");
                            let limited = path.ends_with("/limited");
                            let body = if limited {
                                "x".repeat(5 * 1024 * 1024 + 64 * 1024)
                            } else {
                                "complete page ".repeat(2000)
                            };
                            let mut compressed = if raw {
                                let mut encoder =
                                    DeflateEncoder::new(Vec::new(), Compression::default());
                                encoder.write_all(body.as_bytes()).unwrap();
                                encoder.finish().unwrap()
                            } else {
                                let mut encoder =
                                    ZlibEncoder::new(Vec::new(), Compression::default());
                                encoder.write_all(body.as_bytes()).unwrap();
                                encoder.finish().unwrap()
                            };
                            if !path.ends_with("/complete") {
                                compressed.truncate(compressed.len() - 4);
                            }
                            socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Encoding: deflate\r\nConnection: close\r\n\r\n").await.unwrap();
                            for chunk in compressed.chunks(7) {
                                if socket.write_all(chunk).await.is_err() { break; }
                                tokio::task::yield_now().await;
                            }
                            return;
                        }
                        if request.starts_with("GET /truncated-gzip ") {
                            let mut response =
                                b"HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Encoding: gzip\r\nConnection: close\r\n\r\n".to_vec();
                            response.extend_from_slice(&[
                                0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 0xff, 1, 14, 0, 0xf1, 0xff,
                            ]);
                            response.extend_from_slice(b"<p>partial</p>");
                            socket.write_all(&response).await.unwrap();
                            return;
                        }
                        let (media, body) = if request.starts_with("GET /nonascii ") {
                            ("text/html", "<p>§ 中文 ⎯</p><p>tail</p>")
                        } else if request.starts_with("GET /literal-lt ") {
                            ("text/html", "<p>5 < 10</p><p>tail</p>")
                        } else if request.starts_with("GET /search?") {
                            assert!(request.to_lowercase().contains("x-subscription-token: fixture-web-key\r\n"));
                            ("application/json", r#"{"type":"search","web":{"results":[{"title":"fixture","url":"https://example.com/","description":"fixture-web-key snippet"}]}}"#)
                        } else {
                            ("text/html", "<title>Page</title><main><h1>Web flow</h1><a href='/next'>next</a></main>")
                        };
                        let response = format!("HTTP/1.1 200 OK\r\nContent-Type: {media}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                        socket.write_all(response.as_bytes()).await.unwrap();
                    });
                }
            }
        }
        tasks.abort_all();
        while tasks.join_next().await.is_some() {}
    });
    let home = std::fs::canonicalize("/tmp")
        .unwrap()
        .join(format!("dw-{}", uuid::Uuid::new_v4()));
    let signed_in = watch::Sender::new(Some(false));
    let transition = signed_in.clone();
    let reads = Arc::new(AtomicUsize::new(0));
    let reads_row = reads.clone();
    let config = ServerConfig {
        product_home: Some(home.clone()),
        target_home: Some(home.join("rust")),
        managed_provider: Some(ManagedProviderRow(Arc::new(move |builtins, _| {
            let (signed_in, reads) = (signed_in.clone(), reads_row.clone());
            builtins
                .builtins
                .register::<Value, _, _>("SearchProviderFixture", move |_| {
                    Ok(SearchProviderPlugin::new(signed_in.clone(), reads.clone()))
                });
            "SearchProviderFixture".to_owned()
        }))),
        ..Default::default()
    };
    let mut saved = None;
    for restoring in [false, true] {
        let (server, _model_requests, _) = super::session_typescript::start(config.clone()).await;
        let mut client = Client::new_version(&server, false, 3).await;
        if !restoring {
            assert_ok(&client.rpc("provider.create", json!({"providerId":"018ec73f-0000-7000-8000-000000000011","displayName":"Fixture","connection":{"driverKind":"openai-compatible","baseUrl":"http://localhost/v1","auth":{"type":"anonymous"}},"models":[{"modelId":"fixture","displayName":"fixture","status":"active",
                    "capabilities":dim_agent_types::CatalogCapabilities::with_limits(128_000, 16_384)}],"enabled":true})).await);
            assert_ok(
                &client
                    .rpc(
                        "provider.select",
                        json!({"providerId":"018ec73f-0000-7000-8000-000000000011","modelId":"fixture"}),
                    )
                    .await,
            );
        }
        let id = if restoring {
            let id: String = saved.clone().unwrap();
            let before = requests.load(Ordering::SeqCst);
            assert_ok(&client.rpc("session.attach", json!({"sessionId":id})).await);
            assert_eq!(
                requests.load(Ordering::SeqCst),
                before,
                "restore must not repeat network work"
            );
            id
        } else {
            let id = create_web(&mut client, "full-access", false).await;
            let session = server.agent().unwrap().get_session(&id).unwrap();
            submit(&mut client, &id, "web_search", json!({"query":"initial"})).await;
            idle(&mut client, &id).await;
            let initial = last_turn_outcome(&session).await;
            assert!(
                initial
                    .as_deref()
                    .is_some_and(|outcome| outcome
                        .contains("Tool was not offered in this request: web_search")),
                "{initial:?}"
            );
            transition.send_replace(Some(true));
            await_search_declared(&session, true).await;
            submit(
                &mut client,
                &id,
                "web_search",
                json!({"query":"oauth-default","blocked_domains":["blocked.test"]}),
            )
            .await;
            idle(&mut client, &id).await;
            let managed_hit = result(&session).await;
            assert_ne!(managed_hit.is_error, Some(true), "{managed_hit:?}");
            assert!(text(&managed_hit).contains("Managed oauth-default -site:blocked.test"));
            assert!(text(&managed_hit).contains("https://managed.example.com/hit"));
            assert!(!text(&managed_hit).contains("sub.blocked.test"));
            assert_eq!(
                managed_hit.structured_content.as_ref().unwrap()["results"],
                json!([{
                    "title": "Managed oauth-default -site:blocked.test",
                    "url": "https://managed.example.com/hit",
                    "snippet": "Managed snippet",
                    "published_at": "2026-10-28"
                }])
            );
            submit(
                &mut client,
                &id,
                "web_search",
                json!({"query":"q".repeat(500),"blocked_domains":["blocked.test"]}),
            )
            .await;
            idle(&mut client, &id).await;
            let full_query = result(&session).await;
            assert_ne!(full_query.is_error, Some(true), "{full_query:?}");
            assert!(
                !text(&full_query).contains("-site:"),
                "filters that overflow the relay query limit apply locally only"
            );
            let before = requests.load(Ordering::SeqCst);
            configure(&mut client, json!({"blockPrivateAddresses": true})).await;
            submit(
                &mut client,
                &id,
                "web_fetch",
                json!({"url":"http://198.18.0.1/"}),
            )
            .await;
            idle(&mut client, &id).await;
            let fake_ip = result(&session).await;
            assert_eq!(fake_ip.is_error, Some(true));
            assert!(text(&fake_ip).contains("blocked_address: Reserved benchmark addresses"));
            assert_eq!(requests.load(Ordering::SeqCst), before);
            configure(&mut client, json!({"allowPrivateHosts":["127.0.0.1"],
                "searchService":{"kind":"brave","endpoint":format!("{base}/search"),"apiKey":"fixture-web-key"}})).await;
            submit(
                &mut client,
                &id,
                "web_search",
                json!({"query":"configured"}),
            )
            .await;
            idle(&mut client, &id).await;
            let searched = result(&session).await;
            assert_ne!(searched.is_error, Some(true), "{searched:?}");
            assert!(text(&searched).contains("https://example.com/"));
            assert!(
                !serde_json::to_string(&searched)
                    .unwrap()
                    .contains("fixture-web-key")
            );
            transition.send_replace(Some(false));
            configure(&mut client, json!({"allowPrivateHosts":["127.0.0.1"]})).await;
            await_search_declared(&session, false).await;
            let before = requests.load(Ordering::SeqCst);
            submit(&mut client, &id, "web_search", json!({"query":"removed"})).await;
            idle(&mut client, &id).await;
            let removed = last_turn_outcome(&session).await;
            assert!(
                removed
                    .as_deref()
                    .is_some_and(|outcome| outcome
                        .contains("Tool was not offered in this request: web_search")),
                "{removed:?}"
            );
            assert_eq!(requests.load(Ordering::SeqCst), before);

            let denied = create_web(&mut client, "read-only", true).await;
            let before = requests.load(Ordering::SeqCst);
            submit(&mut client, &denied, "web_fetch", json!({"url":base})).await;
            idle(&mut client, &denied).await;
            assert_eq!(
                result(&server.agent().unwrap().get_session(&denied).unwrap())
                    .await
                    .is_error,
                Some(true)
            );
            assert_eq!(requests.load(Ordering::SeqCst), before);

            let asking = create_web(&mut client, "workspace-write", true).await;
            assert_ok(
                &client
                    .rpc("session.attach", json!({"sessionId":asking}))
                    .await,
            );
            submit(&mut client, &asking, "web_fetch", json!({"url":base})).await;
            let request = client.notice("approval.request").await;
            assert_eq!(
                requests.load(Ordering::SeqCst),
                before,
                "approval precedes I/O"
            );
            assert_ok(
                &client
                    .rpc(
                        "approval.respond",
                        json!({"sessionId":asking,"branchId":"main",
                "requestId":request["params"]["requestId"],"decision":"allow"}),
                    )
                    .await,
            );
            idle(&mut client, &asking).await;
            assert!(
                text(&result(&server.agent().unwrap().get_session(&asking).unwrap()).await)
                    .contains("# Web flow")
            );
            let cancelling = create_web(&mut client, "workspace-write", true).await;
            assert_ok(
                &client
                    .rpc("session.attach", json!({"sessionId":cancelling}))
                    .await,
            );
            let before = requests.load(Ordering::SeqCst);
            submit(&mut client, &cancelling, "web_fetch", json!({"url":base})).await;
            client.notice("approval.request").await;
            assert_ok(
                &client
                    .rpc(
                        "session.stop",
                        json!({"sessionId":cancelling,"branchId":"main"}),
                    )
                    .await,
            );
            idle(&mut client, &cancelling).await;
            assert_eq!(
                requests.load(Ordering::SeqCst),
                before,
                "cancelled approval must not start I/O"
            );
            let cancelled = server.agent().unwrap().get_session(&cancelling).unwrap();
            assert_eq!(
                branch_events(&cancelled, "main", ..)
                    .await
                    .unwrap()
                    .iter()
                    .filter(|event| matches!(event.kind, EventKind::ToolResult(_)))
                    .count(),
                1
            );
            id
        };
        let session = server.agent().unwrap().get_session(&id).unwrap();
        let before = requests.load(Ordering::SeqCst);
        submit(&mut client, &id, "web_fetch", json!({"url":base})).await;
        idle(&mut client, &id).await;
        let fetched = result(&session).await;
        assert!(
            text(&fetched).contains("# Web flow"),
            "restoring={restoring}: {fetched:?}"
        );
        assert_eq!(
            requests.load(Ordering::SeqCst),
            before + 1,
            "restored cache starts empty"
        );
        submit(
            &mut client,
            &id,
            "web_fetch",
            json!({"url":base,"start_char":1000}),
        )
        .await;
        idle(&mut client, &id).await;
        assert_eq!(
            requests.load(Ordering::SeqCst),
            before + 1,
            "pagination uses the Session cache"
        );
        submit(
            &mut client,
            &id,
            "web_fetch",
            json!({"url":format!("{base}/truncated-gzip")}),
        )
        .await;
        idle(&mut client, &id).await;
        let truncated = result(&session).await;
        assert_eq!(truncated.is_error, Some(true), "{truncated:?}");
        assert!(
            text(&truncated).contains("connect_failed"),
            "a gzip stream without its trailer is not a complete page: {truncated:?}"
        );
        submit(
            &mut client,
            &id,
            "web_fetch",
            json!({"url":format!("{base}/nonascii")}),
        )
        .await;
        idle(&mut client, &id).await;
        let page = result(&session).await;
        assert_eq!(page.is_error, None, "{page:?}");
        assert!(text(&page).contains("§"), "{page:?}");
        assert!(text(&page).contains("中文"), "{page:?}");

        let literal = tokio::time::timeout(Duration::from_secs(10), async {
            submit(
                &mut client,
                &id,
                "web_fetch",
                json!({"url":format!("{base}/literal-lt")}),
            )
            .await;
            idle(&mut client, &id).await;
            result(&session).await
        })
        .await
        .expect("a literal '<' in text terminates");
        assert_eq!(literal.is_error, None, "{literal:?}");
        assert!(text(&literal).contains("5 < 10"), "{literal:?}");
        for encoding in ["zlib", "raw"] {
            for ending in ["complete", "truncated", "limited"] {
                submit(
                    &mut client,
                    &id,
                    "web_fetch",
                    json!({"url":format!("{base}/deflate/{encoding}/{ending}")}),
                )
                .await;
                idle(&mut client, &id).await;
                let fetched = result(&session).await;
                if ending == "truncated" {
                    assert_eq!(fetched.is_error, Some(true), "{encoding}: {fetched:?}");
                    assert!(
                        text(&fetched).contains("connect_failed"),
                        "{encoding}: {fetched:?}"
                    );
                } else {
                    assert_ne!(
                        fetched.is_error,
                        Some(true),
                        "{encoding}/{ending}: {fetched:?}"
                    );
                    let data = fetched.structured_content.as_ref().unwrap();
                    assert_eq!(
                        data["download_limited"],
                        ending == "limited",
                        "{encoding}: {fetched:?}"
                    );
                    if ending == "complete" {
                        assert_eq!(data["total"], 28_000, "{encoding}: {fetched:?}");
                        assert!(text(&fetched).contains("complete page complete page"));
                    }
                }
            }
        }
        let results_before = branch_events(&session, "main", ..)
            .await
            .unwrap()
            .iter()
            .filter(|event| matches!(event.kind, EventKind::ToolResult(_)))
            .count();
        submit(
            &mut client,
            &id,
            "web_fetch",
            json!({"url":format!("{base}/slow")}),
        )
        .await;
        tokio::time::timeout(Duration::from_secs(5), entered.recv())
            .await
            .unwrap()
            .unwrap();
        assert_ok(
            &client
                .rpc("session.stop", json!({"sessionId":id,"branchId":"main"}))
                .await,
        );
        tokio::time::timeout(Duration::from_secs(5), disconnected.recv())
            .await
            .unwrap()
            .unwrap();
        idle(&mut client, &id).await;
        let results_after = branch_events(&session, "main", ..)
            .await
            .unwrap()
            .iter()
            .filter(|event| matches!(event.kind, EventKind::ToolResult(_)))
            .count();
        assert_eq!(
            results_after,
            results_before + 1,
            "one terminal tool result on interrupt"
        );
        saved = Some(id);
        drop(client);
        server.shutdown();
        server.join().await.unwrap();
    }
    stop.cancel();
    http.await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

async fn boot_web() -> (
    dim_app_server::RunningServer,
    Client,
    String,
    std::path::PathBuf,
    tokio::sync::mpsc::UnboundedReceiver<Vec<String>>,
) {
    let home = std::fs::canonicalize("/tmp")
        .unwrap()
        .join(format!("dw-net-{}", uuid::Uuid::new_v4()));
    let config = ServerConfig {
        product_home: Some(home.clone()),
        target_home: Some(home.join("rust")),
        ..Default::default()
    };
    // The fixture model reports each request on this channel. Dropping the
    // receiver makes the next turn fail before the tool runs.
    disable_system_proxy();
    let (server, model_requests, _) = super::session_typescript::start(config).await;
    let mut client = Client::new_version(&server, false, 3).await;
    assert_ok(&client.rpc("provider.create", json!({"providerId":"018ec73f-0000-7000-8000-000000000011","displayName":"Fixture","connection":{"driverKind":"openai-compatible","baseUrl":"http://localhost/v1","auth":{"type":"anonymous"}},"models":[{"modelId":"fixture","displayName":"fixture","status":"active",
            "capabilities":dim_agent_types::CatalogCapabilities::with_limits(128_000, 16_384)}],"enabled":true})).await);
    assert_ok(
        &client
            .rpc(
                "provider.select",
                json!({"providerId":"018ec73f-0000-7000-8000-000000000011","modelId":"fixture"}),
            )
            .await,
    );
    let id = create_web(&mut client, "full-access", false).await;
    (server, client, id, home, model_requests)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn web_fetch_blocks_untrusted_targets_and_follows_same_host_redirects() {
    let origin = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin_addr = origin.local_addr().unwrap();
    let base = format!("http://{origin_addr}");
    let hits = Arc::new(AtomicUsize::new(0));
    let seen = Arc::new(std::sync::Mutex::new(String::new()));
    let stop = CancellationToken::new();
    let (hit_count, recorded, stopping) = (hits.clone(), seen.clone(), stop.clone());
    let origin_task = tokio::spawn(async move {
        let mut tasks = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                _ = stopping.cancelled() => break,
                Some(done) = tasks.join_next() => { done.unwrap(); },
                accepted = origin.accept() => {
                    let (mut socket, _) = accepted.unwrap();
                    let (hit_count, recorded) = (hit_count.clone(), recorded.clone());
                    tasks.spawn(async move {
                        let mut header = Vec::new();
                        let mut chunk = [0u8; 4096];
                        while !header.windows(4).any(|part| part == b"\r\n\r\n") {
                            let n = socket.read(&mut chunk).await.unwrap();
                            if n == 0 { return; }
                            header.extend_from_slice(&chunk[..n]);
                            assert!(header.len() < 64 * 1024);
                        }
                        let request = String::from_utf8(header).unwrap();
                        hit_count.fetch_add(1, Ordering::SeqCst);
                        *recorded.lock().unwrap() = request.clone();
                        let path = request.split_whitespace().nth(1).unwrap_or("/");
                        let path = path.split('?').next().unwrap_or(path);
                        let (status, media, body, extra) = if let Some(hops) = path.strip_prefix("/redirect/") {
                            let hops: u32 = hops.parse().unwrap_or(0);
                            let location = if hops == 0 {
                                "/landed".to_owned()
                            } else {
                                format!("/redirect/{}", hops - 1)
                            };
                            (302, "text/plain", String::new(), format!("Location: {location}\r\n"))
                        } else if path == "/cross" {
                            (302, "text/plain", String::new(), "Location: http://127.0.0.2/elsewhere\r\n".to_owned())
                        } else if path == "/landed" {
                            (200, "text/plain", "landed-page".to_owned(), String::new())
                        } else if path == "/plain" {
                            (200, "text/plain", "hello plain".to_owned(), String::new())
                        } else if path == "/data" {
                            (200, "application/json", r#"{"a":1}"#.to_owned(), String::new())
                        } else if path == "/search" {
                            (200, "application/json", r#"{"type":"search","web":{"results":[{"title":"Proxy search","url":"https://example.com/","description":"Fixture result"}]}}"#.to_owned(), String::new())
                        } else if path == "/bin" {
                            (200, "application/octet-stream", "\u{0}".to_owned(), String::new())
                        } else if path == "/missing" {
                            (404, "text/plain", String::new(), String::new())
                        } else if path == "/empty" {
                            (200, "text/plain", String::new(), String::new())
                        } else if path == "/big" {
                            (200, "text/plain", "x".repeat(5 * 1024 * 1024 + 1), String::new())
                        } else {
                            (404, "text/plain", String::new(), String::new())
                        };
                        let response = format!(
                            "HTTP/1.1 {status} OK\r\nContent-Type: {media}\r\nContent-Length: {}\r\nConnection: close\r\n{extra}\r\n{body}",
                            body.len()
                        );
                        socket.write_all(response.as_bytes()).await.unwrap();
                    });
                }
            }
        }
        tasks.abort_all();
    });
    let proxy = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_base = format!("http://{}", proxy.local_addr().unwrap());
    let tunnels = Arc::new(std::sync::Mutex::new(Vec::new()));
    let pending = Arc::new(tokio::sync::Notify::new());
    let disconnected = Arc::new(tokio::sync::Notify::new());
    let (entered, closed) = (pending.clone(), disconnected.clone());
    let (tunnel_log, proxy_stop) = (tunnels.clone(), stop.clone());
    let proxy_task = tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = proxy_stop.cancelled() => break,
                accepted = proxy.accept() => {
                    let (mut client, _) = accepted.unwrap();
                    let tunnel_log = tunnel_log.clone();
                    let (entered, closed) = (entered.clone(), closed.clone());
                    tokio::spawn(async move {
                        let mut header = Vec::new();
                        let mut chunk = [0u8; 1024];
                        let end = loop {
                            let n = client.read(&mut chunk).await.unwrap();
                            if n == 0 { return; }
                            header.extend_from_slice(&chunk[..n]);
                            if let Some(pos) = header.windows(4).position(|part| part == b"\r\n\r\n") {
                                break pos + 4;
                            }
                        };
                        let head = String::from_utf8_lossy(&header[..end]);
                        let request_line = head.lines().next().unwrap_or("");
                        let mut parts = request_line.split_whitespace();
                        let method = parts.next().unwrap_or("");
                        let raw = parts.next().unwrap_or("");
                        // CONNECT names the authority; an absolute-form request
                        // names a URL. Record the authority for both.
                        let (target, path) = if method == "CONNECT" {
                            (raw.to_owned(), None)
                        } else {
                            match raw.split_once("://").and_then(|(_, rest)| {
                                rest.find('/').map(|i| (&rest[..i], &rest[i..]))
                            }) {
                                Some((authority, path)) => {
                                    (authority.to_owned(), Some(path.to_owned()))
                                }
                                None => (raw.to_owned(), Some("/".to_owned())),
                            }
                        };
                        tunnel_log.lock().unwrap().push(target.clone());
                        if target == "pending.invalid:80" || target == "pending.invalid" {
                            entered.notify_one();
                            assert_eq!(client.read(&mut chunk).await.unwrap(), 0);
                            closed.notify_one();
                            return;
                        }
                        if target != origin_addr.to_string()
                            && target != format!("proxy-dns.invalid:{}", origin_addr.port())
                        {
                            client.write_all(b"HTTP/1.1 502 Bad Gateway\r\nConnection: close\r\n\r\n").await.unwrap();
                            return;
                        }
                        let mut upstream =
                            tokio::net::TcpStream::connect(origin_addr).await.unwrap();
                        match path {
                            Some(path) => {
                                let version = parts.next().unwrap_or("HTTP/1.1");
                                let tail = head.split_once("\r\n").map(|(_, tail)| tail).unwrap_or("");
                                let rewritten = format!("{method} {path} {version}\r\n{tail}");
                                upstream.write_all(rewritten.as_bytes()).await.unwrap();
                            }
                            None => {
                                client.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n").await.unwrap();
                            }
                        }
                        if end < header.len() {
                            upstream.write_all(&header[end..]).await.unwrap();
                        }
                        let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
                    });
                }
            }
        }
    });

    let (server, mut client, id, home, _model_requests) = boot_web().await;
    let session = server.agent().unwrap().get_session(&id).unwrap();
    configure(&mut client, json!({"blockPrivateAddresses": true})).await;
    for url in [
        "http://127.0.0.1/",
        "http://0.0.0.0/",
        "http://10.0.0.1/",
        "http://172.16.0.1/",
        "http://192.168.0.1/",
        "http://100.64.0.1/",
        "http://169.254.169.254/",
        "http://224.0.0.1/",
        "http://255.255.255.255/",
        "http://[::1]/",
        "http://[::]/",
        "http://[::ffff:127.0.0.1]/",
        "http://[fe80::1]/",
        "http://[fc00::1]/",
        "http://[ff02::1]/",
        "http://[2001:db8::1]/",
        "http://localhost/",
        "http://sub.localhost/",
    ] {
        submit(&mut client, &id, "web_fetch", json!({"url": url})).await;
        idle(&mut client, &id).await;
        let blocked = result(&session).await;
        assert_eq!(blocked.is_error, Some(true), "{url}: {blocked:?}");
        assert!(
            text(&blocked).contains("blocked_address"),
            "{url}: {}",
            text(&blocked)
        );
    }
    assert_eq!(
        hits.load(Ordering::SeqCst),
        0,
        "blocked literals never connect"
    );
    assert!(tunnels.lock().unwrap().is_empty());

    configure(
        &mut client,
        json!({"blockPrivateAddresses": true, "proxy": proxy_base}),
    )
    .await;
    submit(
        &mut client,
        &id,
        "web_fetch",
        json!({"url": "http://10.0.0.1/"}),
    )
    .await;
    idle(&mut client, &id).await;
    assert!(text(&result(&session).await).contains("blocked_address"));
    assert!(
        tunnels.lock().unwrap().is_empty(),
        "a blocked address does not open a proxy tunnel"
    );
    assert_eq!(hits.load(Ordering::SeqCst), 0);

    configure(
        &mut client,
        json!({"allowPrivateHosts":["127.0.0.1"], "blockPrivateAddresses": true, "proxy": proxy_base}),
    )
    .await;
    submit(
        &mut client,
        &id,
        "web_fetch",
        json!({"url": format!("{base}/plain")}),
    )
    .await;
    idle(&mut client, &id).await;
    let fetched = result(&session).await;
    assert_ne!(fetched.is_error, Some(true), "{fetched:?}");
    assert!(text(&fetched).contains("hello plain"), "{fetched:?}");
    assert_eq!(
        tunnels.lock().unwrap().as_slice(),
        [format!("127.0.0.1:{}", origin_addr.port())]
    );
    let request = seen.lock().unwrap().clone();
    assert!(
        request
            .to_ascii_lowercase()
            .contains(&format!("host: 127.0.0.1:{}", origin_addr.port())),
        "{request}"
    );
    assert!(
        request.contains(&format!("dim-web/{}", env!("CARGO_PKG_VERSION"))),
        "{request}"
    );

    submit(
        &mut client,
        &id,
        "web_fetch",
        json!({"url": "http://198.18.0.1/"}),
    )
    .await;
    idle(&mut client, &id).await;
    assert!(text(&result(&session).await).contains("blocked_address"));
    assert_eq!(tunnels.lock().unwrap().len(), 1);

    // A bare `proxyDns` no longer demands a configured proxy: without one,
    // Web keeps the host's own resolution and connects directly.
    configure(
        &mut client,
        json!({
            "allowPrivateHosts": ["127.0.0.1"],
            "blockPrivateAddresses": true,
            "proxyDns": true,
            "searchService": {"kind":"brave","endpoint":format!("{base}/search"),"apiKey":"fixture-key"}
        }),
    )
    .await;
    await_search_declared(&session, true).await;
    submit(
        &mut client,
        &id,
        "web_fetch",
        json!({"url": format!("{base}/landed?direct")}),
    )
    .await;
    idle(&mut client, &id).await;
    let landed = result(&session).await;
    assert_ne!(landed.is_error, Some(true), "{landed:?}");
    assert!(text(&landed).contains("landed-page"), "{landed:?}");
    submit(&mut client, &id, "web_search", json!({"query": "fixture"})).await;
    idle(&mut client, &id).await;
    let searched = result(&session).await;
    assert_ne!(searched.is_error, Some(true), "{searched:?}");

    let before = hits.load(Ordering::SeqCst);
    configure(
        &mut client,
        json!({
            "allowPrivateHosts": ["127.0.0.1"],
            "blockPrivateAddresses": true,
            "proxy": "http://invalid.example/path",
            "proxyDns": true,
            "searchService": {"kind":"brave","endpoint":format!("{base}/search"),"apiKey":"fixture-key"}
        }),
    )
    .await;
    for (name, args, code) in [
        (
            "web_fetch",
            json!({"url": format!("{base}/landed?invalid")}),
            "connect_failed",
        ),
        (
            "web_search",
            json!({"query": "fixture"}),
            "search_unavailable",
        ),
    ] {
        submit(&mut client, &id, name, args).await;
        idle(&mut client, &id).await;
        let failed = result(&session).await;
        assert!(text(&failed).contains(code), "{name}: {failed:?}");
    }
    assert_eq!(hits.load(Ordering::SeqCst), before);
    assert_eq!(tunnels.lock().unwrap().len(), 1);

    let authority = format!("proxy-dns.invalid:{}", origin_addr.port());
    configure(
        &mut client,
        json!({"blockPrivateAddresses": true, "proxy": proxy_base, "proxyDns": true,
            "searchService":{"kind":"brave","endpoint":format!("http://{authority}/search"),"apiKey":"fixture-key"}}),
    )
    .await;
    for (url, expected) in [
        ("http://127.0.0.1/", "blocked_address"),
        (
            "http://198.18.0.1/",
            "blocked_address: Reserved benchmark addresses are blocked. Proxy DNS applies only to hostname targets.",
        ),
        ("http://[::ffff:127.0.0.1]/", "blocked_address"),
        ("http://localhost/", "blocked_address"),
        ("http://sub.localhost./", "blocked_address"),
    ] {
        submit(&mut client, &id, "web_fetch", json!({"url": url})).await;
        idle(&mut client, &id).await;
        let blocked = result(&session).await;
        assert!(text(&blocked).contains(expected), "{url}: {blocked:?}");
    }
    assert_eq!(tunnels.lock().unwrap().len(), 1);
    for (name, args, expected) in [
        (
            "web_fetch",
            json!({"url":format!("http://{authority}/plain")}),
            "hello plain",
        ),
        ("web_search", json!({"query":"fixture"}), "Proxy search"),
    ] {
        submit(&mut client, &id, name, args).await;
        idle(&mut client, &id).await;
        let reply = result(&session).await;
        assert_ne!(reply.is_error, Some(true), "{name}: {reply:?}");
        assert!(text(&reply).contains(expected), "{name}: {reply:?}");
        let request = seen.lock().unwrap().to_ascii_lowercase();
        assert!(
            request.contains(&format!("host: {authority}\r\n")),
            "{request}"
        );
    }
    assert_eq!(
        &tunnels.lock().unwrap()[1..],
        [authority.clone(), authority]
    );
    let before = hits.load(Ordering::SeqCst);
    submit(
        &mut client,
        &id,
        "web_fetch",
        json!({"url":"http://refused.invalid/"}),
    )
    .await;
    idle(&mut client, &id).await;
    let refused = result(&session).await;
    assert!(
        text(&refused).contains("http_status: HTTP 502"),
        "a proxy refusal surfaces as its status: {refused:?}"
    );
    submit(
        &mut client,
        &id,
        "web_fetch",
        json!({"url":"http://pending.invalid/"}),
    )
    .await;
    tokio::time::timeout(Duration::from_secs(5), pending.notified())
        .await
        .unwrap();
    assert_ok(
        &client
            .rpc("session.stop", json!({"sessionId":id,"branchId":"main"}))
            .await,
    );
    tokio::time::timeout(Duration::from_secs(5), disconnected.notified())
        .await
        .unwrap();
    idle(&mut client, &id).await;
    assert_eq!(result(&session).await.is_error, Some(true));
    assert_eq!(
        &tunnels.lock().unwrap()[3..],
        ["refused.invalid", "pending.invalid"]
    );
    assert_eq!(hits.load(Ordering::SeqCst), before);

    configure(
        &mut client,
        json!({"allowPrivateHosts":["127.0.0.1"], "blockPrivateAddresses": true}),
    )
    .await;
    submit(
        &mut client,
        &id,
        "web_fetch",
        json!({"url": format!("{base}/redirect/4")}),
    )
    .await;
    idle(&mut client, &id).await;
    let followed = result(&session).await;
    assert!(
        text(&followed).contains("landed-page"),
        "five same-host redirects are followed: {followed:?}"
    );
    submit(
        &mut client,
        &id,
        "web_fetch",
        json!({"url": format!("{base}/redirect/5")}),
    )
    .await;
    idle(&mut client, &id).await;
    let too_many = result(&session).await;
    assert!(
        text(&too_many).contains("too_many_redirects"),
        "{too_many:?}"
    );
    submit(
        &mut client,
        &id,
        "web_fetch",
        json!({"url": format!("{base}/cross")}),
    )
    .await;
    idle(&mut client, &id).await;
    let cross = text(&result(&session).await).to_owned();
    assert!(
        cross.contains("Redirected to http://127.0.0.2/elsewhere"),
        "{cross}"
    );
    assert!(cross.contains("call web_fetch"), "{cross}");

    for (path, expect) in [
        ("/data", "\"a\""),
        ("/empty", "(no text content)"),
        ("/bin", "unsupported_content_type"),
        ("/missing", "http_status"),
    ] {
        submit(
            &mut client,
            &id,
            "web_fetch",
            json!({"url": format!("{base}{path}")}),
        )
        .await;
        idle(&mut client, &id).await;
        let page = result(&session).await;
        assert!(text(&page).contains(expect), "{path}: {page:?}");
    }
    submit(
        &mut client,
        &id,
        "web_fetch",
        json!({"url": format!("{base}/big")}),
    )
    .await;
    idle(&mut client, &id).await;
    let limited = result(&session).await;
    assert_ne!(limited.is_error, Some(true), "{limited:?}");
    assert!(
        text(&limited).contains("Download limit reached"),
        "{limited:?}"
    );
    assert_eq!(
        limited.structured_content.as_ref().unwrap()["download_limited"],
        json!(true)
    );
    assert_eq!(
        limited.structured_content.as_ref().unwrap()["total"],
        json!(5 * 1024 * 1024)
    );

    let before = hits.load(Ordering::SeqCst);
    let database = home.join("rust").join("dimcode.sqlite");
    let connection = rusqlite::Connection::open(&database).unwrap();
    connection.busy_timeout(Duration::from_secs(2)).unwrap();
    let changed = connection
        .execute(
            "UPDATE settings_documents SET document = ?1 WHERE source = 'user/web'",
            ["{"],
        )
        .unwrap();
    assert_eq!(changed, 1, "the user web document is in {database:?}");
    submit(
        &mut client,
        &id,
        "web_fetch",
        json!({"url": format!("{base}/plain")}),
    )
    .await;
    idle(&mut client, &id).await;
    let broken = result(&session).await;
    assert!(text(&broken).contains("settings_unavailable"), "{broken:?}");
    assert_eq!(
        hits.load(Ordering::SeqCst),
        before,
        "invalid settings do not open a connection"
    );

    stop.cancel();
    origin_task.await.unwrap();
    proxy_task.await.unwrap();
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test]
async fn web_fetch_times_out_when_the_response_never_finishes() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let stop = CancellationToken::new();
    let stopping = stop.clone();
    let (arrived, mut connections) = tokio::sync::mpsc::unbounded_channel();
    let held = tokio::spawn(async move {
        let mut sockets = Vec::new();
        loop {
            tokio::select! {
                _ = stopping.cancelled() => break,
                accepted = listener.accept() => {
                    sockets.push(accepted.unwrap().0);
                    arrived.send(()).unwrap();
                }
            }
        }
        drop(sockets);
    });
    let (server, mut client, id, home, _model_requests) = boot_web().await;
    configure(&mut client, json!({"allowPrivateHosts":["127.0.0.1"]})).await;
    let session = server.agent().unwrap().get_session(&id).unwrap();
    let before = branch_events(&session, "main", ..)
        .await
        .unwrap()
        .iter()
        .filter(|event| matches!(event.kind, EventKind::ToolResult(_)))
        .count();
    submit(&mut client, &id, "web_fetch", json!({"url": base})).await;
    tokio::time::timeout(Duration::from_secs(10), connections.recv())
        .await
        .unwrap()
        .unwrap();
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(25)).await;
    tokio::time::resume();
    assert!(
        tokio::time::timeout(Duration::from_millis(250), session.idle("main"))
            .await
            .is_err(),
        "the unfinished request must remain active before its 30s deadline"
    );
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(6)).await;
    tokio::time::resume();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let count = branch_events(&session, "main", ..)
                .await
                .unwrap()
                .iter()
                .filter(|event| matches!(event.kind, EventKind::ToolResult(_)))
                .count();
            if count > before {
                break;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    })
    .await
    .expect("web_fetch must settle after its 30s deadline when the response never finishes");
    let timed_out = result(&session).await;
    assert!(
        text(&timed_out).contains("timeout: Call timed out."),
        "{timed_out:?}"
    );
    stop.cancel();
    held.await.unwrap();
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires DIM_TEST_NODE and DIM_TEST_JS_RUNTIME"]
async fn node_managed_search_matches_native_results_and_errors() {
    use dim_app_server::managed_provider::ManagedSearch;
    let home = std::fs::canonicalize("/tmp")
        .unwrap()
        .join(format!("dms-{}", uuid::Uuid::new_v4()));
    let signed_in = watch::Sender::new(Some(true));
    let transition = signed_in.clone();
    let reads = Arc::new(AtomicUsize::new(0));
    let reads_row = reads.clone();
    let mut parts = super::session_typescript::fixture();
    parts.rows = vec![json!({
        "id":"managed-search",
        "name":parts.anchor.parent().unwrap().join("fixtures/managed-search.ts"),
        "provides":{}, "config":{}
    })];
    let config = ServerConfig {
        product_home: Some(home.clone()),
        target_home: Some(home.join("rust")),
        session_typescript: Some(parts),
        managed_provider: Some(ManagedProviderRow(Arc::new(move |builtins, _| {
            let (signed_in, reads) = (signed_in.clone(), reads_row.clone());
            builtins
                .builtins
                .register::<Value, _, _>("SearchProviderFixture", move |_| {
                    Ok(SearchProviderPlugin::new(signed_in.clone(), reads.clone()))
                });
            "SearchProviderFixture".to_owned()
        }))),
        ..Default::default()
    };
    let (server, _, _) = super::session_typescript::start(config).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let search = server.service::<ManagedSearch>().unwrap();
    for (query, logged_in) in [("dim", true), ("fail", true), ("dim", false)] {
        transition.send_replace(Some(logged_in));
        let request = WebSearchRequest {
            query: query.into(),
            count: 2,
        };
        let wire = super::process_typescript::files_available(
            &mut client,
            "searchprobe.search",
            json!(request),
        )
        .await;
        match search.search(request, &CancellationToken::new()).await {
            Ok(reply) => assert_eq!(wire, json!({"reply":reply})),
            Err(error) => {
                let code = match error {
                    dim_app_server::managed_provider::ManagedSearchError::Unavailable => {
                        "unavailable"
                    }
                    dim_app_server::managed_provider::ManagedSearchError::SearchFailed => {
                        "search_failed"
                    }
                };
                assert_eq!(wire["error"]["code"], code);
                assert!(
                    wire["error"]["message"]
                        .as_str()
                        .unwrap()
                        .contains(&error.to_string())
                );
            }
        }
    }
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

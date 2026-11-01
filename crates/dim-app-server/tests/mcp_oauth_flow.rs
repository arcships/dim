#![cfg(unix)]
//! MCP OAuth (F4) through the daemon's public methods: `mcp.auth.*`,
//! `mcp.catalog` and `session.mcp`, against an in-process authorization
//! server and an OAuth-protected MCP server (streamable HTTP and legacy SSE)
//! on loopback. Every home is isolated under /tmp.
use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response, Sse, sse::Event},
    routing::{get, post},
};
use base64::{
    Engine,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use dim_agent_capabilities::{
    defaults, model_provider::ModelProviderPlugin, workspace_tools::WorkspaceToolsConfig,
};
use dim_agent_runtime::{AgentBuilder, AgentConfig};
use dim_app_server::{RunningServer, ServerConfig, serve};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    convert::Infallible,
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::PathBuf,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::UnixStream,
    sync::{Notify, broadcast},
};
use tokio_util::sync::CancellationToken;

/// Everything every test logs, at debug level for every target.
fn logs() -> Arc<Mutex<Vec<u8>>> {
    static LOGS: OnceLock<Arc<Mutex<Vec<u8>>>> = OnceLock::new();
    LOGS.get_or_init(|| {
        let logs = Arc::new(Mutex::new(Vec::new()));
        let sink = logs.clone();
        tracing_subscriber::fmt()
            .with_max_level(tracing::Level::DEBUG)
            .with_ansi(false)
            .with_writer(move || Capture(sink.clone()))
            .init();
        logs
    })
    .clone()
}
struct Capture(Arc<Mutex<Vec<u8>>>);
impl std::io::Write for Capture {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// The OAuth-protected MCP server: streamable HTTP at `/mcp`, legacy SSE at
/// `/sse` with POSTs to `/messages`.
#[derive(Default)]
struct Resource {
    base: OnceLock<String>,
    /// The authorization server its PRM names.
    authority: OnceLock<String>,
    /// Access tokens it accepts, and those granted `files:write`.
    valid: Mutex<HashSet<String>>,
    wide: Mutex<HashSet<String>>,
    /// `tools/list` needs `files:write`.
    require_write: AtomicBool,
    /// Publishes Protected Resource Metadata.
    prm: AtomicBool,
    /// Bearer tokens it received, in order.
    bearers: Mutex<Vec<String>>,
    unauthorized: AtomicUsize,
    events: OnceLock<broadcast::Sender<String>>,
}
impl Resource {
    fn url(&self) -> String {
        format!("{}/mcp", self.base.get().unwrap())
    }
    fn sse_url(&self) -> String {
        format!("{}/sse", self.base.get().unwrap())
    }
    /// The bearer, or the `401` answer to a request without a valid one.
    fn authorize(&self, headers: &HeaderMap, path: &str) -> Result<String, Refusal> {
        let bearer = headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "))
            .map(str::to_owned);
        if let Some(bearer) = &bearer {
            self.bearers.lock().unwrap().push(bearer.clone());
        }
        match bearer {
            Some(bearer) if self.valid.lock().unwrap().contains(&bearer) => Ok(bearer),
            _ => {
                self.unauthorized.fetch_add(1, Ordering::SeqCst);
                let challenge = if self.prm.load(Ordering::SeqCst) {
                    format!(
                        r#"Bearer resource_metadata="{}/.well-known/oauth-protected-resource/{path}", scope="files:read""#,
                        self.base.get().unwrap()
                    )
                } else {
                    r#"Bearer scope="files:read""#.to_owned()
                };
                Err((StatusCode::UNAUTHORIZED, Some(challenge)))
            }
        }
    }
    /// One JSON-RPC message: its result, `None` for a notification, or a
    /// refusal.
    fn dispatch(&self, bearer: &str, message: &Value) -> Result<Option<Value>, Refusal> {
        Ok(Some(match message["method"].as_str().unwrap_or_default() {
            "initialize" => {
                json!({"protocolVersion":message["params"]["protocolVersion"],"capabilities":{"tools":{}},"serverInfo":{"name":"oauth-fixture","version":"1"}})
            }
            "tools/list" => {
                if self.require_write.load(Ordering::SeqCst)
                    && !self.wide.lock().unwrap().contains(bearer)
                {
                    let challenge = format!(
                        r#"Bearer error="insufficient_scope", scope="files:write", resource_metadata="{}/.well-known/oauth-protected-resource/mcp""#,
                        self.base.get().unwrap()
                    );
                    return Err((StatusCode::FORBIDDEN, Some(challenge)));
                }
                json!({"tools":[{"name":"read","description":"Read a file","inputSchema":{"type":"object"}}]})
            }
            method if method.starts_with("notifications/") => return Ok(None),
            _ => return Err((StatusCode::BAD_REQUEST, None)),
        }))
    }
}
/// A refused request: its status and `WWW-Authenticate` challenge.
type Refusal = (StatusCode, Option<String>);
fn answer((status, challenge): Refusal) -> Response {
    match challenge {
        Some(challenge) => (status, [("www-authenticate", challenge)]).into_response(),
        None => status.into_response(),
    }
}
async fn mcp(
    State(resource): State<Arc<Resource>>,
    headers: HeaderMap,
    Json(message): Json<Value>,
) -> Response {
    let bearer = match resource.authorize(&headers, "mcp") {
        Ok(bearer) => bearer,
        Err(refusal) => return answer(refusal),
    };
    match resource.dispatch(&bearer, &message) {
        Ok(Some(result)) => (
            [("mcp-session-id", "oauth-session")],
            Json(json!({"jsonrpc":"2.0","id":message["id"],"result":result})),
        )
            .into_response(),
        Ok(None) => StatusCode::ACCEPTED.into_response(),
        Err(refusal) => answer(refusal),
    }
}
async fn sse(State(resource): State<Arc<Resource>>, headers: HeaderMap) -> Response {
    if let Err(refusal) = resource.authorize(&headers, "sse") {
        return answer(refusal);
    }
    let receiver = resource.events.get().unwrap().subscribe();
    let endpoint = futures_util::stream::once(async {
        Ok::<_, Infallible>(Event::default().event("endpoint").data("/messages"))
    });
    let messages = futures_util::stream::unfold(receiver, |mut receiver| async move {
        let data = receiver.recv().await.ok()?;
        Some((
            Ok::<_, Infallible>(Event::default().event("message").data(data)),
            receiver,
        ))
    });
    Sse::new(futures_util::StreamExt::chain(endpoint, messages)).into_response()
}
async fn messages(
    State(resource): State<Arc<Resource>>,
    headers: HeaderMap,
    Json(message): Json<Value>,
) -> Response {
    let bearer = match resource.authorize(&headers, "sse") {
        Ok(bearer) => bearer,
        Err(refusal) => return answer(refusal),
    };
    match resource.dispatch(&bearer, &message) {
        Ok(Some(result)) => {
            let reply = json!({"jsonrpc":"2.0","id":message["id"],"result":result});
            let _ = resource.events.get().unwrap().send(reply.to_string());
            StatusCode::ACCEPTED.into_response()
        }
        Ok(None) => StatusCode::ACCEPTED.into_response(),
        Err(refusal) => answer(refusal),
    }
}
async fn protected_resource(resource: Arc<Resource>, path: &str) -> Response {
    if !resource.prm.load(Ordering::SeqCst) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let url = format!("{}/{path}", resource.base.get().unwrap());
    Json(json!({"resource":url,"authorization_servers":[resource.authority.get()],"scopes_supported":["files:read"]})).into_response()
}

#[derive(Default)]
struct Authority {
    base: OnceLock<String>,
    resource: Arc<Resource>,
    /// Advertise PKCE `S256`.
    s256: AtomicBool,
    /// Advertise a cleartext, non-loopback authorization endpoint.
    cleartext: AtomicBool,
    /// `expires_in` of tokens issued for an authorization code.
    code_expires_in: AtomicU64,
    reject_refresh: AtomicBool,
    invalid_client: AtomicBool,
    /// Requests that wait for `release` before they are answered: a token
    /// grant type, or `metadata`. `held` tells the test one arrived.
    holds: Mutex<HashSet<&'static str>>,
    held: Notify,
    release: Notify,
    refreshes: AtomicUsize,
    issued: AtomicUsize,
    /// Codes the user approved, with their PKCE challenge and scope.
    codes: Mutex<HashMap<String, (String, String)>>,
    refresh_tokens: Mutex<HashSet<String>>,
    registrations: Mutex<Vec<Value>>,
    /// Each token request's form, with the authenticated client as `client`.
    token_requests: Mutex<Vec<HashMap<String, String>>>,
    revocations: Mutex<Vec<(String, String)>>,
}
impl Authority {
    fn base(&self) -> &str {
        self.base.get().unwrap()
    }
    async fn pause(&self, point: &str) {
        if self.holds.lock().unwrap().contains(point) {
            let released = self.release.notified();
            self.held.notify_waiters();
            released.await;
        }
    }
    /// Holds requests at `point` until `unhold`; the returned future
    /// resolves when one arrives.
    fn hold(&self, point: &'static str) -> tokio::sync::futures::Notified<'_> {
        let held = self.held.notified();
        self.holds.lock().unwrap().insert(point);
        held
    }
    fn unhold(&self, point: &str) {
        self.holds.lock().unwrap().remove(point);
        self.release.notify_waiters();
    }
    fn issue(&self, expires_in: u64, scope: &str) -> Value {
        let n = self.issued.fetch_add(1, Ordering::SeqCst) + 1;
        let (access, refresh) = (format!("access-{n}-secret"), format!("refresh-{n}-secret"));
        self.resource.valid.lock().unwrap().insert(access.clone());
        if scope.split(' ').any(|scope| scope == "files:write") {
            self.resource.wide.lock().unwrap().insert(access.clone());
        }
        self.refresh_tokens.lock().unwrap().insert(refresh.clone());
        json!({"access_token":access,"refresh_token":refresh,"token_type":"Bearer","expires_in":expires_in,"scope":scope})
    }
}
async fn authorization_metadata(State(authority): State<Arc<Authority>>) -> Json<Value> {
    authority.pause("metadata").await;
    let base = authority.base();
    let authorize = if authority.cleartext.load(Ordering::SeqCst) {
        "http://auth.example.test/authorize".to_owned()
    } else {
        format!("{base}/authorize")
    };
    let mut metadata = json!({
        "issuer": base,
        "authorization_endpoint": authorize,
        "token_endpoint": format!("{base}/token"),
        "registration_endpoint": format!("{base}/register"),
        "revocation_endpoint": format!("{base}/revoke"),
        "response_types_supported": ["code"],
        "scopes_supported": ["files:read", "files:write", "offline_access"],
    });
    if authority.s256.load(Ordering::SeqCst) {
        metadata["code_challenge_methods_supported"] = json!(["S256"]);
    }
    Json(metadata)
}
async fn register(
    State(authority): State<Arc<Authority>>,
    Json(request): Json<Value>,
) -> Json<Value> {
    authority
        .registrations
        .lock()
        .unwrap()
        .push(request.clone());
    Json(json!({"client_id":"dcr-client","redirect_uris":request["redirect_uris"]}))
}
async fn token(
    State(authority): State<Arc<Authority>>,
    headers: HeaderMap,
    body: String,
) -> Response {
    let mut form: HashMap<String, String> = url::form_urlencoded::parse(body.as_bytes())
        .into_owned()
        .collect();
    let refused =
        |status: StatusCode, error: &str| (status, Json(json!({"error":error}))).into_response();
    let basic = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Basic "))
        .map(|basic| String::from_utf8(STANDARD.decode(basic).unwrap()).unwrap());
    let client = match basic.as_deref() {
        Some("pre-client:pre-secret") if !form.contains_key("client_id") => "pre-client",
        None if form.get("client_id").map(String::as_str) == Some("dcr-client") => "dcr-client",
        _ => return refused(StatusCode::UNAUTHORIZED, "invalid_client"),
    };
    form.insert("client".into(), client.into());
    authority.token_requests.lock().unwrap().push(form.clone());
    let resource = &authority.resource;
    assert!([resource.url(), resource.sse_url()].contains(&form["resource"]));
    match form["grant_type"].as_str() {
        "authorization_code" => {
            authority.pause("authorization_code").await;
            assert_eq!(form["redirect_uri"], "http://localhost:1456/callback");
            let Some((challenge, scope)) = authority.codes.lock().unwrap().remove(&form["code"])
            else {
                return refused(StatusCode::BAD_REQUEST, "invalid_grant");
            };
            let verified = URL_SAFE_NO_PAD.encode(Sha256::digest(form["code_verifier"].as_bytes()));
            assert_eq!(verified, challenge, "PKCE S256 verifier");
            let expires_in = authority.code_expires_in.load(Ordering::SeqCst);
            Json(authority.issue(expires_in, &scope)).into_response()
        }
        "refresh_token" => {
            authority.refreshes.fetch_add(1, Ordering::SeqCst);
            authority.pause("refresh_token").await;
            if authority.invalid_client.load(Ordering::SeqCst) {
                return refused(StatusCode::UNAUTHORIZED, "invalid_client");
            }
            if authority.reject_refresh.load(Ordering::SeqCst)
                || !authority
                    .refresh_tokens
                    .lock()
                    .unwrap()
                    .remove(&form["refresh_token"])
            {
                return refused(StatusCode::BAD_REQUEST, "invalid_grant");
            }
            Json(authority.issue(3600, "files:read offline_access")).into_response()
        }
        _ => refused(StatusCode::BAD_REQUEST, "unsupported_grant_type"),
    }
}
async fn revoke(State(authority): State<Arc<Authority>>, body: String) -> StatusCode {
    let form: HashMap<String, String> = url::form_urlencoded::parse(body.as_bytes())
        .into_owned()
        .collect();
    let resource = &authority.resource;
    resource.valid.lock().unwrap().remove(&form["token"]);
    authority
        .refresh_tokens
        .lock()
        .unwrap()
        .remove(&form["token"]);
    authority
        .revocations
        .lock()
        .unwrap()
        .push((form["token"].clone(), form["token_type_hint"].clone()));
    StatusCode::OK
}

struct Servers {
    resource: Arc<Resource>,
    authority: Arc<Authority>,
    stop: CancellationToken,
}
impl Servers {
    async fn start() -> Self {
        let stop = CancellationToken::new();
        let resource = Arc::new(Resource::default());
        resource.prm.store(true, Ordering::SeqCst);
        resource.events.set(broadcast::channel(16).0).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        resource
            .base
            .set(format!("http://{}", listener.local_addr().unwrap()))
            .unwrap();
        let (http, legacy) = (resource.clone(), resource.clone());
        tokio::spawn(
            axum::serve(
                listener,
                Router::new()
                    .route(
                        "/mcp",
                        post(mcp)
                            .get(|| async { StatusCode::METHOD_NOT_ALLOWED })
                            .delete(|| async { StatusCode::NO_CONTENT }),
                    )
                    .route("/sse", get(sse))
                    .route("/messages", post(messages))
                    .route(
                        "/.well-known/oauth-protected-resource/mcp",
                        get(move || protected_resource(http.clone(), "mcp")),
                    )
                    .route(
                        "/.well-known/oauth-protected-resource/sse",
                        get(move || protected_resource(legacy.clone(), "sse")),
                    )
                    .with_state(resource.clone()),
            )
            .with_graceful_shutdown(stop.clone().cancelled_owned())
            .into_future(),
        );
        let authority = Arc::new(Authority {
            resource: resource.clone(),
            ..Default::default()
        });
        authority.s256.store(true, Ordering::SeqCst);
        authority.code_expires_in.store(3600, Ordering::SeqCst);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        authority.base.set(base.clone()).unwrap();
        resource.authority.set(base).unwrap();
        tokio::spawn(
            axum::serve(
                listener,
                Router::new()
                    .route(
                        "/.well-known/oauth-authorization-server",
                        get(authorization_metadata),
                    )
                    .route("/register", post(register))
                    .route("/token", post(token))
                    .route("/revoke", post(revoke))
                    .with_state(authority.clone()),
            )
            .with_graceful_shutdown(stop.clone().cancelled_owned())
            .into_future(),
        );
        Self {
            resource,
            authority,
            stop,
        }
    }
}

struct Client(BufReader<UnixStream>, u64);
impl Client {
    async fn rpc(&mut self, method: &str, params: Value) -> Value {
        self.1 += 1;
        let id = self.1;
        let request = json!({"jsonrpc":"2.0","id":id,"method":method,"params":params});
        let stream = self.0.get_mut();
        stream
            .write_all(format!("{request}\n").as_bytes())
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let mut line = String::new();
                assert_ne!(self.0.read_line(&mut line).await.unwrap(), 0);
                let value: Value = serde_json::from_str(&line).unwrap();
                if value["id"] == id {
                    return value;
                }
            }
        })
        .await
        .expect("response deadline")
    }
    async fn ok(&mut self, method: &str, params: Value) -> Value {
        let reply = self.rpc(method, params).await;
        assert!(reply.get("error").is_none(), "{method}: {reply}");
        reply["result"].clone()
    }
    async fn error(&mut self, method: &str, params: Value) -> String {
        let reply = self.rpc(method, params).await;
        reply["error"]["data"]["appError"]
            .as_str()
            .unwrap_or_else(|| panic!("{method} should fail: {reply}"))
            .to_owned()
    }
    /// Waits until `session.mcp` serves the Session again. A process plugin
    /// restart returns once the process plugins are Active; the Session's
    /// parts that depend on it reload after, and meanwhile `session.mcp`
    /// answers `service_reloading`, which asks the client to retry.
    async fn until_serving(&mut self, session: &str) {
        tokio::time::timeout(Duration::from_secs(20), async {
            while self.rpc("session.mcp", json!({"sessionId":session})).await["error"]["data"]
                ["appError"]
                == "service_reloading"
            {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the Session's MCP parts reload");
    }
    /// Waits until the Session reports server `id` in `state`.
    async fn mcp_state(&mut self, session: &str, id: &str, state: &str) -> Value {
        let mut last = Value::Null;
        let reached = tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                let reply = self.ok("session.mcp", json!({"sessionId":session})).await;
                let server = reply["servers"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|server| server["id"] == id)
                    .cloned()
                    .unwrap_or_default();
                if server["state"] == state {
                    return server;
                }
                last = server;
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await;
        reached.unwrap_or_else(|_| panic!("{id} never reached {state}: {last}"))
    }
    async fn catalog_auth(&mut self, params: Value, id: &str) -> Value {
        let catalog = self.ok("mcp.catalog", params).await;
        catalog["servers"]
            .as_array()
            .unwrap()
            .iter()
            .find(|server| server["id"] == id)
            .unwrap()["auth"]
            .clone()
    }
    /// Waits until a callback no longer reaches the attempt because it is
    /// exchanging or closed. Each probe carries a wrong `state`, so it
    /// changes nothing while the attempt is pending.
    async fn until_closed(&mut self, attempt: &Value, callback: &str) {
        let probe =
            json!({"attemptId":attempt,"callbackUrl":callback.replace("state=", "state=x")});
        tokio::time::timeout(Duration::from_secs(20), async {
            while self.error("mcp.auth.complete", probe.clone()).await != "mcp_auth_attempt_closed"
            {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the attempt stays open");
    }
    /// Signs in through `start` and the user's consent; returns the attempt.
    async fn sign_in(&mut self, authority: &Authority, target: Value, code: &str) -> Value {
        let started = self.ok("mcp.auth.start", target).await;
        let callback = consent(authority, &started, code);
        self.ok(
            "mcp.auth.complete",
            json!({"attemptId":started["attemptId"],"callbackUrl":callback}),
        )
        .await;
        started
    }
}

struct Daemon {
    server: RunningServer,
    home: PathBuf,
    work: PathBuf,
}
impl Daemon {
    async fn start(servers: &[(&str, Value)]) -> Self {
        let home = PathBuf::from("/tmp").join(format!("dmo-{}", uuid::Uuid::new_v4()));
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&home)
            .unwrap();
        let work = home.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let entries: serde_json::Map<String, Value> = servers
            .iter()
            .map(|(id, entry)| ((*id).to_owned(), entry.clone()))
            .collect();
        std::fs::write(
            home.join("mcp.json"),
            json!({"mcpServers":entries}).to_string(),
        )
        .unwrap();
        let native = home.join("rust");
        let mut builder = AgentBuilder::new(AgentConfig::default());
        defaults::install_local_execution(
            &mut builder,
            ModelProviderPlugin::new,
            WorkspaceToolsConfig {
                output_root: native.join("outputs"),
            },
            dim_agent_capabilities::timeline_tree::BranchToolConfig::default(),
        );
        dim_app_server::permission_rules::install(&mut builder);
        defaults::mock_model_hosts(&mut builder);
        let server = serve(
            builder,
            ServerConfig {
                product_home: Some(home.clone()),
                target_home: Some(native.clone()),
                unix_path: Some(native.join("daemon.sock")),
                session_defaults: dim_agent_runtime::SessionConfig {
                    tools: Some(vec!["read".into()]),
                    permission_policy: dim_agent_types::PermissionPolicy {
                        preset: dim_agent_types::PermissionPreset::WorkspaceWrite,
                        ..Default::default()
                    },
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .await
        .unwrap();
        Self { server, home, work }
    }
    /// Another connection, for a call that waits while this test goes on.
    async fn connect(&self) -> Client {
        let mut client = Client(
            BufReader::new(
                UnixStream::connect(self.server.unix_path.as_ref().unwrap())
                    .await
                    .unwrap(),
            ),
            0,
        );
        client
            .ok("hello", json!({"protocolVersion":3,"clientKind":"other"}))
            .await;
        client
    }
    async fn client(&self) -> Client {
        let mut client = self.connect().await;
        let caps = dim_agent_types::CatalogCapabilities::with_limits(32000, 2048);
        client.ok("provider.create", json!({"providerId":"018ec73f-0000-7000-8000-000000000001","displayName":"Fixture","connection":{"driverKind":"openai-compatible","baseUrl":"http://127.0.0.1:9/v1","auth":{"type":"bearer"},"credential":{"action":"set","value":"fixture-access"}},"models":[{"modelId":"alpha","displayName":"alpha","status":"active","capabilities":caps}],"enabled":true})).await;
        client
            .ok(
                "provider.select",
                json!({"providerId":"018ec73f-0000-7000-8000-000000000001","modelId":"alpha"}),
            )
            .await;
        client
    }
    async fn session(&self, client: &mut Client) -> String {
        client.ok("session.create", json!({"cwd":self.work})).await["sessionId"]
            .as_str()
            .unwrap()
            .to_owned()
    }
    fn store(&self) -> Value {
        serde_json::from_slice(&std::fs::read(self.home.join("mcp-oauth.json")).unwrap()).unwrap()
    }
    async fn close(self) {
        self.server.shutdown();
        self.server.join().await.unwrap();
        std::fs::remove_dir_all(self.home).unwrap();
    }
}

/// The user's consent at the authorization server: approves a code for the
/// attempt's PKCE challenge and scope, and returns the redirect the browser
/// would follow.
fn consent(authority: &Authority, started: &Value, code: &str) -> String {
    let url = url::Url::parse(started["authorizationUrl"].as_str().unwrap()).unwrap();
    let query: HashMap<_, _> = url.query_pairs().into_owned().collect();
    authority.codes.lock().unwrap().insert(
        code.to_owned(),
        (
            query["code_challenge"].clone(),
            query.get("scope").cloned().unwrap_or_default(),
        ),
    );
    format!(
        "http://localhost:1456/callback?code={code}&state={}&iss={}",
        query["state"],
        url::form_urlencoded::byte_serialize(authority.base().as_bytes()).collect::<String>()
    )
}

/// Whether a token request carried the authorization code.
fn sent(authority: &Authority, code: &str) -> bool {
    authority
        .token_requests
        .lock()
        .unwrap()
        .iter()
        .any(|form| form.get("code").map(String::as_str) == Some(code))
}

async fn arrived(held: tokio::sync::futures::Notified<'_>) {
    tokio::time::timeout(Duration::from_secs(20), held)
        .await
        .expect("the held request arrives");
}

fn user(id: &str) -> Value {
    json!({"source":"user","id":id})
}

async fn until(condition: impl Fn() -> bool) {
    tokio::time::timeout(Duration::from_secs(20), async {
        while !condition() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sign_in_connects_refreshes_once_per_key_and_signs_out() {
    let logs = logs();
    let servers = Servers::start().await;
    let (resource, authority) = (servers.resource.clone(), servers.authority.clone());
    authority.code_expires_in.store(10, Ordering::SeqCst);
    let daemon = Daemon::start(&[("docs", json!({"type":"http","url":resource.url()}))]).await;
    let mut client = daemon.client().await;
    let first = daemon.session(&mut client).await;

    let state = client.mcp_state(&first, "docs", "auth_required").await;
    assert_eq!(state["message"], "authorization required");
    assert!(state["adoptedVersion"].is_null());
    assert_eq!(
        client.catalog_auth(json!({}), "docs").await,
        json!({"status":"required"})
    );

    let started = client.ok("mcp.auth.start", user("docs")).await;
    assert_eq!(started["redirectUri"], "http://localhost:1456/callback");
    assert_eq!(started["resource"], resource.url());
    assert_eq!(started["issuer"], authority.base());
    let url = url::Url::parse(started["authorizationUrl"].as_str().unwrap()).unwrap();
    assert!(
        url.as_str()
            .starts_with(&format!("{}/authorize?", authority.base()))
    );
    let query: HashMap<_, _> = url.query_pairs().into_owned().collect();
    assert_eq!(query["response_type"], "code");
    assert_eq!(query["client_id"], "dcr-client");
    assert_eq!(query["code_challenge_method"], "S256");
    assert_eq!(query["redirect_uri"], "http://localhost:1456/callback");
    assert_eq!(query["resource"], resource.url());
    assert_eq!(query["scope"], "files:read offline_access");
    let registration = authority.registrations.lock().unwrap()[0].clone();
    assert_eq!(registration["client_name"], "Dim");
    assert_eq!(
        registration["redirect_uris"],
        json!(["http://localhost:1456/callback"])
    );
    assert_eq!(registration["token_endpoint_auth_method"], "none");
    assert_eq!(
        registration["grant_types"],
        json!(["authorization_code", "refresh_token"])
    );

    let callback = consent(&authority, &started, "code-1-secret");
    let attempt = started["attemptId"].clone();
    for wrong in [
        callback.replace("localhost:1456", "localhost:1457"),
        callback.replace("/callback?", "/other?"),
        callback.replace("state=", "state=x"),
        callback.replace("iss=", "iss=x"),
    ] {
        assert_eq!(
            client
                .error(
                    "mcp.auth.complete",
                    json!({"attemptId":attempt,"callbackUrl":wrong})
                )
                .await,
            "mcp_auth_callback_invalid"
        );
    }
    assert!(authority.token_requests.lock().unwrap().is_empty());
    client
        .ok(
            "mcp.auth.complete",
            json!({"attemptId":attempt,"callbackUrl":callback}),
        )
        .await;
    assert_eq!(
        client
            .error(
                "mcp.auth.complete",
                json!({"attemptId":attempt,"callbackUrl":callback})
            )
            .await,
        "mcp_auth_attempt_closed"
    );
    assert_eq!(daemon.store()["version"], 1);
    assert_eq!(
        std::fs::metadata(daemon.home.join("mcp-oauth.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );

    client.mcp_state(&first, "docs", "ready").await;
    assert_eq!(authority.refreshes.load(Ordering::SeqCst), 1);
    let refresh_token =
        |daemon: &Daemon| daemon.store()["credentials"][0]["tokens"]["refreshToken"].clone();
    assert_eq!(refresh_token(&daemon), "refresh-2-secret");
    assert!(
        resource
            .bearers
            .lock()
            .unwrap()
            .iter()
            .all(|token| token == "access-2-secret")
    );
    assert_eq!(
        client.catalog_auth(json!({}), "docs").await,
        json!({"status":"authorized"})
    );

    resource.valid.lock().unwrap().remove("access-2-secret");
    client
        .ok("session.mcp.refresh", json!({"sessionId":first}))
        .await;
    until(|| {
        resource.bearers.lock().unwrap().last().map(String::as_str) == Some("access-3-secret")
    })
    .await;
    client.mcp_state(&first, "docs", "ready").await;
    assert_eq!(authority.refreshes.load(Ordering::SeqCst), 2);
    assert_eq!(refresh_token(&daemon), "refresh-3-secret");

    let second = daemon.session(&mut client).await;
    client.mcp_state(&second, "docs", "ready").await;
    let refreshing = authority.hold("refresh_token");
    let before = resource.unauthorized.load(Ordering::SeqCst);
    resource.valid.lock().unwrap().remove("access-3-secret");
    client
        .ok("session.mcp.refresh", json!({"sessionId":first}))
        .await;
    client
        .ok("session.mcp.refresh", json!({"sessionId":second}))
        .await;
    tokio::time::timeout(Duration::from_secs(20), refreshing)
        .await
        .unwrap();
    until(|| resource.unauthorized.load(Ordering::SeqCst) >= before + 2).await;
    authority.unhold("refresh_token");
    until(|| {
        resource
            .bearers
            .lock()
            .unwrap()
            .iter()
            .filter(|token| *token == "access-4-secret")
            .count()
            >= 2
    })
    .await;
    for session in [&first, &second] {
        client.mcp_state(session, "docs", "ready").await;
    }
    assert_eq!(authority.refreshes.load(Ordering::SeqCst), 3);

    let refreshing = authority.hold("refresh_token");
    resource.valid.lock().unwrap().remove("access-4-secret");
    client
        .ok("session.mcp.refresh", json!({"sessionId":first}))
        .await;
    tokio::time::timeout(Duration::from_secs(20), refreshing)
        .await
        .unwrap();
    let release = async {
        let _ = tokio::time::timeout(Duration::from_secs(5), async {
            while !daemon.server.diagnostics().plugins.iter().any(|plugin| {
                matches!(plugin.name.as_str(), "Mcp" | "McpServer" | "McpSession")
                    && plugin.state == rutis::host::FiberState::Unloading
            }) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
        authority.unhold("refresh_token");
    };
    let (restarted, ()) = tokio::join!(daemon.server.restart("Mcp"), release);
    restarted.unwrap();
    assert_eq!(refresh_token(&daemon), "refresh-5-secret");
    assert_eq!(authority.refreshes.load(Ordering::SeqCst), 4);
    for session in [&first, &second] {
        client.until_serving(session).await;
        client.mcp_state(session, "docs", "ready").await;
    }

    let logout = client.ok("mcp.auth.logout", user("docs")).await;
    assert_eq!(logout, json!({"revoked":true}));
    assert_eq!(
        *authority.revocations.lock().unwrap(),
        [
            ("refresh-5-secret".to_owned(), "refresh_token".to_owned()),
            ("access-5-secret".to_owned(), "access_token".to_owned())
        ]
    );
    assert_eq!(daemon.store()["credentials"], json!([]));
    for session in [&first, &second] {
        let state = client.mcp_state(session, "docs", "auth_required").await;
        assert_eq!(state["message"], "authorization required");
    }
    assert_eq!(
        client.catalog_auth(json!({}), "docs").await,
        json!({"status":"required"})
    );
    assert_eq!(
        client.ok("mcp.auth.logout", user("docs")).await,
        json!({"revoked":false})
    );

    let logs = String::from_utf8(logs.lock().unwrap().clone()).unwrap();
    assert!(logs.contains("mcp.auth"), "auth operations are traced");
    let verifiers: Vec<_> = authority
        .token_requests
        .lock()
        .unwrap()
        .iter()
        .filter_map(|form| form.get("code_verifier").cloned())
        .collect();
    assert!(!verifiers.is_empty());
    for secret in ["-secret", "code-1"]
        .into_iter()
        .map(str::to_owned)
        .chain(verifiers)
    {
        assert!(!logs.contains(&secret), "{secret} leaked into logs");
    }
    daemon.close().await;
    servers.stop.cancel();
}

/// A POST `401` without `WWW-Authenticate` reaches rmcp as `UnexpectedServerResponse` text; that
/// form must still count as an auth refusal.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sign_in_refuses_unverifiable_servers_and_expires_rejected_grants() {
    let servers = Servers::start().await;
    let (resource, authority) = (servers.resource.clone(), servers.authority.clone());
    let daemon = Daemon::start(&[
        ("docs", json!({"type":"http","url":resource.url()})),
        (
            "cleartext",
            json!({"type":"http","url":"http://mcp.example.test/mcp"}),
        ),
        (
            "static",
            json!({"type":"http","url":resource.url(),"auth":{"type":"bearer","token":"fixture-static"}}),
        ),
        (
            "local",
            json!({"command":"never-run-this-command","enabled":false}),
        ),
        (
            "off",
            json!({"type":"http","url":resource.url(),"enabled":false}),
        ),
    ])
    .await;
    let mut client = daemon.client().await;
    for id in ["cleartext", "static", "local"] {
        assert_eq!(
            client.error("mcp.auth.start", user(id)).await,
            "mcp_auth_unsupported",
            "{id}"
        );
    }
    assert_eq!(
        client.error("mcp.auth.start", user("off")).await,
        "mcp_unavailable"
    );
    assert_eq!(
        client
            .error(
                "mcp.auth.start",
                json!({"source":"user","id":"docs","clientSecret":"pre-secret"})
            )
            .await,
        "invalid_params"
    );
    let revision = client.ok("mcp.catalog", json!({})).await["userRevision"].clone();
    assert_eq!(
        client.error("mcp.save", json!({"source":"user","id":"mixed","expectedVersion":revision,"configuration":{"type":"http","url":resource.url(),"headers":{"Authorization":"Bearer x"},"oauth":{"clientId":"fixture"}}})).await,
        "mcp_invalid_config"
    );

    authority.s256.store(false, Ordering::SeqCst);
    assert_eq!(
        client.error("mcp.auth.start", user("docs")).await,
        "mcp_auth_discovery_failed"
    );
    authority.s256.store(true, Ordering::SeqCst);
    authority.cleartext.store(true, Ordering::SeqCst);
    assert_eq!(
        client.error("mcp.auth.start", user("docs")).await,
        "mcp_auth_discovery_failed"
    );
    authority.cleartext.store(false, Ordering::SeqCst);
    resource.prm.store(false, Ordering::SeqCst);
    assert_eq!(
        client.error("mcp.auth.start", user("docs")).await,
        "mcp_auth_discovery_failed"
    );
    assert!(authority.registrations.lock().unwrap().is_empty());
    let revision = client.ok("mcp.catalog", json!({})).await["userRevision"].clone();
    client.ok("mcp.save", json!({"source":"user","id":"docs","expectedVersion":revision,"configuration":{"type":"http","url":resource.url(),"oauth":{"authServerMetadataUrl":format!("{}/.well-known/oauth-authorization-server", authority.base())}}})).await;
    let started = client.ok("mcp.auth.start", user("docs")).await;
    assert_eq!(started["resource"], resource.url());
    resource.prm.store(true, Ordering::SeqCst);
    assert!(authority.token_requests.lock().unwrap().is_empty());

    daemon.server.restart("Mcp").await.unwrap();
    let callback = consent(&authority, &started, "code-reload-secret");
    assert_eq!(
        client
            .error(
                "mcp.auth.complete",
                json!({"attemptId":started["attemptId"],"callbackUrl":callback})
            )
            .await,
        "mcp_auth_attempt_closed"
    );

    let started = client.ok("mcp.auth.start", user("docs")).await;
    let denied = consent(&authority, &started, "unused").replace(
        "code=unused",
        "error=access_denied&error_description=private-detail",
    );
    let reply = client
        .rpc(
            "mcp.auth.complete",
            json!({"attemptId":started["attemptId"],"callbackUrl":denied}),
        )
        .await;
    assert_eq!(reply["error"]["data"]["appError"], "mcp_auth_failed");
    assert!(reply.to_string().contains("access_denied"));
    assert!(!reply.to_string().contains("private-detail"));

    let session = daemon.session(&mut client).await;
    client
        .sign_in(&authority, user("docs"), "code-2-secret")
        .await;
    client.mcp_state(&session, "docs", "ready").await;
    authority.reject_refresh.store(true, Ordering::SeqCst);
    resource.valid.lock().unwrap().clear();
    client
        .ok("session.mcp.refresh", json!({"sessionId":session}))
        .await;
    let state = client.mcp_state(&session, "docs", "auth_required").await;
    assert_eq!(state["message"], "authorization expired");
    assert_eq!(
        client.catalog_auth(json!({}), "docs").await,
        json!({"status":"expired"})
    );
    assert!(daemon.store()["credentials"][0].get("tokens").is_none());
    let probe = client.ok("mcp.test", user("docs")).await;
    assert_eq!(probe["status"], "authRequired", "{probe}");
    assert_eq!(probe["expired"], true, "{probe}");
    assert!(probe["tools"].is_null(), "{probe}");
    assert!(probe.get("missingScopes").is_none(), "{probe}");
    authority.reject_refresh.store(false, Ordering::SeqCst);
    let registrations = authority.registrations.lock().unwrap().len();
    client
        .sign_in(&authority, user("docs"), "code-3-secret")
        .await;
    client.mcp_state(&session, "docs", "ready").await;
    assert_eq!(authority.registrations.lock().unwrap().len(), registrations);

    authority.invalid_client.store(true, Ordering::SeqCst);
    resource.valid.lock().unwrap().clear();
    client
        .ok("session.mcp.refresh", json!({"sessionId":session}))
        .await;
    assert_eq!(
        client.mcp_state(&session, "docs", "auth_required").await["message"],
        "authorization expired"
    );
    assert!(daemon.store()["credentials"][0].get("client").is_none());
    authority.invalid_client.store(false, Ordering::SeqCst);
    client
        .sign_in(&authority, user("docs"), "code-4-secret")
        .await;
    client.mcp_state(&session, "docs", "ready").await;
    assert_eq!(
        authority.registrations.lock().unwrap().len(),
        registrations + 1
    );
    daemon.close().await;
    servers.stop.cancel();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn step_up_project_entries_confidential_clients_and_sse() {
    let servers = Servers::start().await;
    let (resource, authority) = (servers.resource.clone(), servers.authority.clone());
    let daemon = Daemon::start(&[
        ("legacy", json!({"type":"sse","url":resource.sse_url()})),
        (
            "pre",
            json!({"type":"http","url":resource.url(),"oauth":{"clientId":"pre-client"}}),
        ),
    ])
    .await;
    std::fs::write(
        daemon.work.join(".mcp.json"),
        json!({"mcpServers":{"docs":{"type":"http","url":resource.url()}}}).to_string(),
    )
    .unwrap();
    let mut client = daemon.client().await;
    let workspace = json!({"cwd":daemon.work});
    let project = json!({"source":"project","id":"docs","cwd":daemon.work});
    let session = daemon.session(&mut client).await;
    client.mcp_state(&session, "docs", "disabled").await;
    client.mcp_state(&session, "legacy", "auth_required").await;
    client.mcp_state(&session, "pre", "auth_required").await;

    // A project entry signs in only once the user enabled it (ADR 003 D3),
    // and shares the record of its key: the same id and URL as a user entry.
    assert_eq!(
        client.error("mcp.auth.start", project.clone()).await,
        "mcp_unavailable"
    );
    let revision = client.ok("mcp.catalog", json!({})).await["userRevision"].clone();
    client.ok("mcp.save", json!({"source":"user","id":"docs","expectedVersion":revision,"configuration":{"type":"http","url":resource.url()}})).await;
    client
        .sign_in(&authority, user("docs"), "code-docs-secret")
        .await;
    assert_eq!(
        client.catalog_auth(workspace.clone(), "docs").await,
        json!({"status":"authorized"})
    );
    let catalog = client.ok("mcp.catalog", workspace.clone()).await;
    client.ok("mcp.set_enabled", json!({"source":"project","id":"docs","cwd":daemon.work,"expectedVersion":catalog["projectRevision"],"enabled":true})).await;
    client.mcp_state(&session, "docs", "ready").await;

    resource.require_write.store(true, Ordering::SeqCst);
    client
        .ok("session.mcp.refresh", json!({"sessionId":session}))
        .await;
    assert_eq!(
        client.mcp_state(&session, "docs", "auth_required").await["message"],
        "authorization required"
    );
    assert_eq!(
        client.catalog_auth(workspace.clone(), "docs").await,
        json!({"status":"authorized","missingScopes":["files:write"]})
    );
    let probe = client.ok("mcp.test", user("docs")).await;
    assert_eq!(probe["status"], "authRequired", "{probe}");
    assert_eq!(probe["missingScopes"], json!(["files:write"]), "{probe}");
    assert!(
        probe.get("expired").is_none(),
        "a scope refusal is not an expired credential: {probe}"
    );
    assert!(probe["tools"].is_null(), "{probe}");
    let started = client
        .sign_in(&authority, project.clone(), "code-write-secret")
        .await;
    let url = url::Url::parse(started["authorizationUrl"].as_str().unwrap()).unwrap();
    let query: HashMap<_, _> = url.query_pairs().into_owned().collect();
    assert_eq!(query["scope"], "files:read offline_access files:write");
    client.mcp_state(&session, "docs", "ready").await;
    assert_eq!(
        client.catalog_auth(workspace.clone(), "docs").await,
        json!({"status":"authorized"})
    );
    resource.require_write.store(false, Ordering::SeqCst);

    client
        .sign_in(&authority, user("legacy"), "code-sse-secret")
        .await;
    client.mcp_state(&session, "legacy", "ready").await;
    let tested = client.ok("mcp.test", user("legacy")).await;
    assert_eq!(tested["status"], "connected", "{tested}");
    assert!(
        tested["latencyMs"].as_u64().is_some(),
        "a probe reports the latency of its attempt: {tested}"
    );
    assert_eq!(tested["tools"][0]["name"], "read");
    client.mcp_state(&session, "legacy", "ready").await;
    let refreshes = authority.refreshes.load(Ordering::SeqCst);
    let sse_token = daemon.store()["credentials"]
        .as_array()
        .unwrap()
        .iter()
        .find(|record| record["serverId"] == "legacy")
        .unwrap()["tokens"]["accessToken"]
        .as_str()
        .unwrap()
        .to_owned();
    resource.valid.lock().unwrap().remove(&sse_token);
    client
        .ok("session.mcp.refresh", json!({"sessionId":session}))
        .await;
    until(|| authority.refreshes.load(Ordering::SeqCst) == refreshes + 1).await;
    client.mcp_state(&session, "legacy", "ready").await;

    let registrations = authority.registrations.lock().unwrap().len();
    let started = client
        .ok(
            "mcp.auth.start",
            json!({"source":"user","id":"pre","clientSecret":"pre-secret"}),
        )
        .await;
    let url = url::Url::parse(started["authorizationUrl"].as_str().unwrap()).unwrap();
    assert!(
        url.query_pairs()
            .any(|(name, value)| name == "client_id" && value == "pre-client")
    );
    let callback = consent(&authority, &started, "code-pre-secret");
    client
        .ok(
            "mcp.auth.complete",
            json!({"attemptId":started["attemptId"],"callbackUrl":callback}),
        )
        .await;
    client.mcp_state(&session, "pre", "ready").await;
    assert_eq!(authority.registrations.lock().unwrap().len(), registrations);
    let exchange = authority
        .token_requests
        .lock()
        .unwrap()
        .last()
        .unwrap()
        .clone();
    assert_eq!(exchange["client"], "pre-client");
    assert!(!exchange.contains_key("client_secret"));
    daemon.close().await;
    servers.stop.cancel();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn settings_inspection_tests_oauth_fields_and_cancellation_keep_credentials_private() {
    let servers = Servers::start().await;
    let (resource, authority) = (servers.resource.clone(), servers.authority.clone());
    let metadata = format!("{}/unused?token=metadata-fixture-secret", authority.base());
    let daemon = Daemon::start(&[("pre", json!({"type":"http","url":resource.url(),"oauth":{"clientId":"pre-client","callbackPort":1457,"scopes":"files:read","authServerMetadataUrl":metadata,"extension":"preserved"}}))]).await;
    let mut client = daemon.client().await;
    let view = client.ok("mcp.inspect", user("pre")).await;
    assert_eq!(view["connections"], json!([]));
    assert_eq!(
        resource.unauthorized.load(Ordering::SeqCst),
        0,
        "inspection must not contact the MCP server"
    );
    let probe = client.ok("mcp.test", user("pre")).await;
    assert_eq!(probe["status"], "authRequired", "{probe}");
    assert_eq!(
        probe["expired"], false,
        "no credential is stored yet: {probe}"
    );
    assert!(probe["tools"].is_null(), "{probe}");
    assert!(
        probe.get("missingScopes").is_none(),
        "the server named no scopes: {probe}"
    );
    assert_eq!(
        resource.unauthorized.load(Ordering::SeqCst),
        1,
        "a probe contacts the server, unlike inspection"
    );
    assert_eq!(view["configuration"]["oauth"]["clientId"], "pre-client");
    assert_eq!(view["configuration"]["oauth"]["callbackPort"], 1457);
    assert_eq!(view["configuration"]["oauth"]["metadataConfigured"], true);
    assert!(!view.to_string().contains("metadata-fixture-secret"));
    assert!(!view.to_string().contains("extension"));
    let before = client.ok("mcp.catalog", json!({})).await;
    client.ok("mcp.update", json!({"source":"user","id":"pre","expectedVersion":before["userRevision"],"patch":{"oauth":{"action":"set","callbackPort":null,"scopes":null}}})).await;
    let db = rusqlite::Connection::open(daemon.home.join("rust/dimcode.sqlite")).unwrap();
    let stored = || {
        let bytes: String = db
            .query_row(
                "SELECT document FROM settings_documents WHERE source='user/mcp'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        serde_json::from_str::<Value>(&bytes).unwrap()["mcpServers"]["pre"].clone()
    };
    assert_eq!(stored()["oauth"]["authServerMetadataUrl"], metadata);
    assert_eq!(stored()["oauth"]["clientId"], "pre-client");
    assert_eq!(stored()["oauth"]["extension"], "preserved");
    assert!(stored()["oauth"].get("callbackPort").is_none());
    assert!(stored()["oauth"].get("scopes").is_none());
    let current = client.ok("mcp.catalog", json!({})).await;
    assert_eq!(client.error("mcp.update", json!({"source":"user","id":"pre","expectedVersion":current["userRevision"],"patch":{"bearer":{"action":"set","token":"must-not-persist-secret"}}})).await, "mcp_invalid_config");
    let started = client
        .ok(
            "mcp.auth.start",
            json!({"source":"user","id":"pre","clientSecret":"pre-secret"}),
        )
        .await;
    assert_eq!(started["redirectUri"], "http://localhost:1456/callback");
    assert!(!started.to_string().contains("pre-secret"));
    let callback = consent(&authority, &started, "cancelled-code-secret");
    client
        .ok("mcp.auth.cancel", json!({"attemptId":started["attemptId"]}))
        .await;
    assert_eq!(
        client
            .error(
                "mcp.auth.complete",
                json!({"attemptId":started["attemptId"],"callbackUrl":callback})
            )
            .await,
        "mcp_auth_attempt_closed"
    );
    assert!(!sent(&authority, "cancelled-code-secret"));
    assert!(
        !daemon.home.join("mcp-oauth.json").exists(),
        "cancelling an attempt must not save credentials"
    );
    client
        .sign_in(
            &authority,
            json!({"source":"user","id":"pre","clientSecret":"pre-secret"}),
            "accepted-code-secret",
        )
        .await;
    let session = daemon.session(&mut client).await;
    client.mcp_state(&session, "pre", "ready").await;
    let snapshot = client.ok("mcp.inspect", user("pre")).await;
    assert!(
        snapshot["connections"]
            .as_array()
            .unwrap()
            .iter()
            .any(|connection| connection["state"]["status"] == "connected"
                && connection["tools"][0]["name"] == "read")
    );
    for secret in [
        "pre-secret",
        "metadata-fixture-secret",
        "accepted-code-secret",
        "access-1-secret",
        "refresh-1-secret",
    ] {
        assert!(!snapshot.to_string().contains(secret));
    }
    let before = stored();
    let tested = client.ok("mcp.test", user("pre")).await;
    assert_eq!(tested["status"], "connected", "{tested}");
    assert_eq!(tested["tools"][0]["name"], "read");
    assert_eq!(
        stored(),
        before,
        "a probe cannot change persisted configuration"
    );
    client.mcp_state(&session, "pre", "ready").await;
    let credentials = daemon.store()["credentials"].clone();
    let started = client
        .ok(
            "mcp.auth.start",
            json!({"source":"user","id":"pre","clientSecret":"pre-secret"}),
        )
        .await;
    let callback = consent(&authority, &started, "inflight-cancelled-code-secret");
    let held = authority.hold("authorization_code");
    let mut completing = daemon.connect().await;
    let complete = json!({"attemptId":started["attemptId"],"callbackUrl":callback});
    let waiting =
        tokio::spawn(async move { completing.error("mcp.auth.complete", complete).await });
    arrived(held).await;
    client
        .ok("mcp.auth.cancel", json!({"attemptId":started["attemptId"]}))
        .await;
    authority.unhold("authorization_code");
    assert_eq!(waiting.await.unwrap(), "mcp_auth_attempt_closed");
    assert_eq!(
        daemon.store()["credentials"],
        credentials,
        "cancellation preserves an existing grant"
    );
    assert_eq!(
        client.catalog_auth(json!({}), "pre").await["status"],
        "authorized"
    );
    let current = client.ok("mcp.catalog", json!({})).await;
    client.ok("mcp.update", json!({"source":"user","id":"pre","expectedVersion":current["userRevision"],"patch":{"oauth":{"action":"remove"}}})).await;
    assert!(stored().get("oauth").is_none());
    daemon.close().await;
    servers.stop.cancel();
}

/// Only the key's current attempt redeems its code (design §7). Sign-out and
/// a newer start supersede an attempt at every await of its exchange until
/// its final currency check: once superseded it sends no code, and tokens a
/// request already out obtains are revoked, never stored or published. After
/// that check, its publication comes first.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sign_out_and_newer_sign_ins_supersede_pending_attempts() {
    let servers = Servers::start().await;
    let (resource, authority) = (servers.resource.clone(), servers.authority.clone());
    let daemon = Daemon::start(&[("docs", json!({"type":"http","url":resource.url()}))]).await;
    let mut client = daemon.client().await;
    let session = daemon.session(&mut client).await;
    client
        .sign_in(&authority, user("docs"), "code-1-secret")
        .await;
    client.mcp_state(&session, "docs", "ready").await;
    let access = |daemon: &Daemon| {
        daemon.store()["credentials"][0]["tokens"]["accessToken"]
            .as_str()
            .unwrap()
            .to_owned()
    };

    let started = client.ok("mcp.auth.start", user("docs")).await;
    let attempt = started["attemptId"].clone();
    let callback = consent(&authority, &started, "code-a-secret");
    let held = authority.hold("refresh_token");
    resource.valid.lock().unwrap().remove(&access(&daemon));
    client
        .ok("session.mcp.refresh", json!({"sessionId":session}))
        .await;
    arrived(held).await;
    let mut signing_out = daemon.connect().await;
    let logout = tokio::spawn(async move { signing_out.ok("mcp.auth.logout", user("docs")).await });
    client.until_closed(&attempt, &callback).await;
    assert_eq!(
        client
            .error(
                "mcp.auth.complete",
                json!({"attemptId":attempt,"callbackUrl":callback})
            )
            .await,
        "mcp_auth_attempt_closed"
    );
    authority.unhold("refresh_token");
    assert_eq!(logout.await.unwrap(), json!({"revoked":true}));
    assert!(!sent(&authority, "code-a-secret"));
    assert_eq!(daemon.store()["credentials"], json!([]));
    client.mcp_state(&session, "docs", "auth_required").await;
    assert_eq!(
        client.catalog_auth(json!({}), "docs").await,
        json!({"status":"required"})
    );

    client
        .sign_in(&authority, user("docs"), "code-2-secret")
        .await;
    client.mcp_state(&session, "docs", "ready").await;
    let first = client.ok("mcp.auth.start", user("docs")).await;
    let callback = consent(&authority, &first, "code-b-secret");
    let complete = json!({"attemptId":first["attemptId"],"callbackUrl":callback});
    let held = authority.hold("refresh_token");
    resource.valid.lock().unwrap().remove(&access(&daemon));
    client
        .ok("session.mcp.refresh", json!({"sessionId":session}))
        .await;
    arrived(held).await;
    let mut completing = daemon.connect().await;
    let waiting = tokio::spawn({
        let complete = complete.clone();
        async move { completing.error("mcp.auth.complete", complete).await }
    });
    client.until_closed(&first["attemptId"], &callback).await;
    assert_eq!(
        client.error("mcp.auth.complete", complete).await,
        "mcp_auth_attempt_closed"
    );
    let second = client.ok("mcp.auth.start", user("docs")).await;
    authority.unhold("refresh_token");
    assert_eq!(waiting.await.unwrap(), "mcp_auth_attempt_closed");
    assert!(!sent(&authority, "code-b-secret"));
    let callback = consent(&authority, &second, "code-c-secret");
    client
        .ok(
            "mcp.auth.complete",
            json!({"attemptId":second["attemptId"],"callbackUrl":callback}),
        )
        .await;
    client.mcp_state(&session, "docs", "ready").await;

    let stored = daemon.store()["credentials"].clone();
    let revoked = authority.revocations.lock().unwrap().len();
    let first = client.ok("mcp.auth.start", user("docs")).await;
    let callback = consent(&authority, &first, "code-d-secret");
    let held = authority.hold("authorization_code");
    let mut completing = daemon.connect().await;
    let waiting = tokio::spawn({
        let complete = json!({"attemptId":first["attemptId"],"callbackUrl":callback});
        async move { completing.error("mcp.auth.complete", complete).await }
    });
    arrived(held).await;
    client.ok("mcp.auth.start", user("docs")).await;
    authority.unhold("authorization_code");
    assert_eq!(waiting.await.unwrap(), "mcp_auth_attempt_closed");
    assert!(sent(&authority, "code-d-secret"));
    let issued = authority.issued.load(Ordering::SeqCst);
    let (access_token, refresh_token) = (
        format!("access-{issued}-secret"),
        format!("refresh-{issued}-secret"),
    );
    assert_eq!(
        authority.revocations.lock().unwrap().as_slice()[revoked..],
        [
            (refresh_token, "refresh_token".to_owned()),
            (access_token.clone(), "access_token".to_owned())
        ]
    );
    assert_eq!(daemon.store()["credentials"], stored);
    assert!(!resource.bearers.lock().unwrap().contains(&access_token));
    client.mcp_state(&session, "docs", "ready").await;

    let first = client.ok("mcp.auth.start", user("docs")).await;
    let callback = consent(&authority, &first, "code-e-secret");
    let discovering = authority.hold("metadata");
    let mut starting = daemon.connect().await;
    let mut newer = tokio::spawn(async move { starting.ok("mcp.auth.start", user("docs")).await });
    arrived(discovering).await;
    let mut writing = dim_app_server::mcp::testing::hold_credential_write(&daemon.home);
    let mut completing = daemon.connect().await;
    let completed = tokio::spawn({
        let complete = json!({"attemptId":first["attemptId"],"callbackUrl":callback});
        async move { completing.ok("mcp.auth.complete", complete).await }
    });
    tokio::time::timeout(Duration::from_secs(20), writing.arrived())
        .await
        .expect("the record write arrives");
    authority.unhold("metadata");
    assert!(
        tokio::time::timeout(Duration::from_millis(500), &mut newer)
            .await
            .is_err(),
        "a newer start returned while a sign-in was publishing"
    );
    drop(writing);
    completed.await.unwrap();
    let second = newer.await.unwrap();
    let issued = authority.issued.load(Ordering::SeqCst);
    assert_eq!(access(&daemon), format!("access-{issued}-secret"));
    let callback = consent(&authority, &second, "code-f-secret");
    client
        .ok(
            "mcp.auth.complete",
            json!({"attemptId":second["attemptId"],"callbackUrl":callback}),
        )
        .await;
    assert_eq!(access(&daemon), format!("access-{}-secret", issued + 1));
    client.mcp_state(&session, "docs", "ready").await;

    let registrations = authority.registrations.lock().unwrap().len();
    let held = authority.hold("metadata");
    let mut starting = daemon.connect().await;
    let waiting = tokio::spawn(async move { starting.error("mcp.auth.start", user("docs")).await });
    arrived(held).await;
    assert_eq!(
        client.ok("mcp.auth.logout", user("docs")).await,
        json!({"revoked":true})
    );
    authority.unhold("metadata");
    assert_eq!(waiting.await.unwrap(), "mcp_auth_attempt_closed");
    assert_eq!(authority.registrations.lock().unwrap().len(), registrations);
    client.ok("mcp.auth.start", user("docs")).await;
    assert_eq!(
        authority.registrations.lock().unwrap().len(),
        registrations + 1
    );
    daemon.close().await;
    servers.stop.cancel();
}

//! The local web entry: a loopback listener that serves the Web workspace
//! assets and admits browsers holding a token issued to the local user. See
//! `docs/design/web-workspace.md` §2.

mod resources;
mod uploads;

use crate::{
    connection::{ConnectionHub, Entry},
    outbound::MAX_FRAME_BYTES,
    protocol::{SERVER_VERSION, WebOpened, WebStatus, WebToggled},
    transport::run_ws,
};
use axum::{
    Router,
    body::{Body, Bytes},
    extract::{State, ws::WebSocketUpgrade},
    http::{HeaderMap, HeaderValue, Method, StatusCode, Uri, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use dim_agent_capabilities::methods::RpcError;
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    net::{Ipv4Addr, SocketAddr},
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::{net::TcpListener, task::JoinHandle};
use tokio_util::sync::CancellationToken;

const FIRST_PORT: u16 = 43201;
const PORT_ATTEMPTS: u16 = 100;
const COOKIE: &str = "dim_web";
const FREE_FAILURES: u32 = 5;
const MAX_BACKOFF: Duration = Duration::from_secs(60);
const CSP: &str = "default-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; connect-src 'self'; frame-ancestors 'none'";

/// Process-wide owner of the entry; at most one is open. The daemon-owned
/// `enabled` switch and the local connections that called `web.open` (the
/// holders) are independent reasons to keep it open. `dimcode web` is a
/// holder; the TUI toggles the switch.
pub(crate) struct WebEntry {
    assets: Option<PathBuf>,
    state: tokio::sync::Mutex<Lifecycle>,
}
struct Lifecycle {
    enabled: bool,
    open: Option<Open>,
}
struct Open {
    address: SocketAddr,
    /// Kept so that `web.open` stays idempotent; only in this process.
    bootstrap: String,
    /// Ids of the local connections holding the entry open.
    holders: HashSet<String>,
    stop: CancellationToken,
    task: JoinHandle<()>,
}

impl WebEntry {
    pub fn new(assets: Option<PathBuf>) -> Self {
        Self {
            assets,
            state: tokio::sync::Mutex::new(Lifecycle {
                enabled: false,
                open: None,
            }),
        }
    }

    /// Adds a holder. The calling connection keeps the entry open until it
    /// disconnects, even when the TUI switch is off.
    pub async fn open(
        &self,
        hub: &Arc<ConnectionHub>,
        holder: &str,
        port: Option<u16>,
    ) -> Result<WebOpened, RpcError> {
        let mut state = self.state.lock().await;
        if let Some(open) = state.open.as_mut().filter(|open| !open.task.is_finished()) {
            open.holders.insert(holder.to_owned());
            return Ok(opened(open));
        }
        state.open = None;
        let open = self
            .start(hub, port, HashSet::from([holder.to_owned()]))
            .await?;
        let reply = opened(&open);
        state.open = Some(open);
        Ok(reply)
    }

    /// Flips the daemon switch without changing holders. Turning it off
    /// leaves the entry open while a foreground `dimcode web` still holds it.
    pub async fn toggle(
        &self,
        hub: &Arc<ConnectionHub>,
        port: Option<u16>,
    ) -> Result<WebToggled, RpcError> {
        let mut state = self.state.lock().await;
        if state.enabled {
            state.enabled = false;
            self.close_if_unused(&mut state, hub).await;
            return Ok(toggled(&state));
        }
        state.enabled = true;
        if !state
            .open
            .as_ref()
            .is_some_and(|open| !open.task.is_finished())
        {
            state.open = None;
            match self.start(hub, port, HashSet::new()).await {
                Ok(open) => state.open = Some(open),
                Err(error) => {
                    state.enabled = false;
                    return Err(error);
                }
            }
        }
        Ok(toggled(&state))
    }

    /// `dimcode web --off`: turn the switch off without disconnecting a
    /// foreground holder.
    pub async fn disable(&self, hub: &Arc<ConnectionHub>) -> WebStatus {
        let mut state = self.state.lock().await;
        state.enabled = false;
        self.close_if_unused(&mut state, hub).await;
        status(hub, &state)
    }

    pub async fn status(&self, hub: &Arc<ConnectionHub>) -> WebStatus {
        status(hub, &*self.state.lock().await)
    }

    /// Called as a local connection ends. When it was the last holder and
    /// the switch is off, the listener closes, every token is dropped and
    /// web connections detach.
    pub async fn release(&self, hub: &Arc<ConnectionHub>, holder: &str) {
        let mut state = self.state.lock().await;
        let Some(open) = state.open.as_mut() else {
            return;
        };
        if !open.holders.remove(holder) {
            return;
        }
        self.close_if_unused(&mut state, hub).await;
    }

    async fn close_if_unused(&self, state: &mut Lifecycle, hub: &Arc<ConnectionHub>) {
        if state.enabled
            || state
                .open
                .as_ref()
                .is_some_and(|open| !open.holders.is_empty())
        {
            return;
        }
        let Some(open) = state.open.take() else {
            return;
        };
        open.stop.cancel();
        hub.close_entry(Entry::Web);
        let _ = open.task.await;
    }

    async fn start(
        &self,
        hub: &Arc<ConnectionHub>,
        port: Option<u16>,
        holders: HashSet<String>,
    ) -> Result<Open, RpcError> {
        let assets = self
            .assets
            .as_ref()
            .and_then(|path| path.canonicalize().ok())
            .filter(|path| path.join("index.html").is_file())
            .map(Assets::Directory)
            .or_else(|| {
                EMBEDDED
                    .iter()
                    .any(|(path, ..)| *path == "/index.html")
                    .then_some(Assets::Embedded)
            })
            .ok_or_else(|| {
                RpcError::app(
                    -32000,
                    "web_assets_missing",
                    "This App Server has no Web workspace",
                )
            })?;
        if let Assets::Directory(root) = &assets {
            check_protocol(root)?;
        }
        let listener = bind(port).await?;
        let address = listener.local_addr().map_err(io_error)?;
        let bootstrap = new_token();
        let stop = hub.closing().child_token();
        let site = Arc::new(Site {
            hub: hub.clone(),
            stopping: stop.clone(),
            assets,
            port: address.port(),
            bootstrap: digest(&bootstrap),
            access: Mutex::new(HashSet::new()),
            uploads: tokio::sync::Semaphore::new(4),
            throttle: Mutex::new(Throttle::default()),
        });
        let router = Router::new()
            .route("/api/health", get(health))
            .route(
                "/api/sessions/{session_id}/files",
                post(uploads::upload).layer(axum::extract::DefaultBodyLimit::max(17 * 1024 * 1024)),
            )
            .route(
                "/api/sessions/{session_id}/files/{file_id}",
                get(uploads::download),
            )
            .route("/api/auth/exchange", post(exchange))
            .route("/api/auth/session", get(session))
            .route("/api/auth/logout", post(logout))
            .route(
                "/api/ui/extensions/{generation}/{*path}",
                get(resources::extension),
            )
            .route(
                "/api/ui/sessions/{session_id}/resources/{id}",
                get(resources::session_resource),
            )
            .route("/ws", get(upgrade))
            .route("/api/{*rest}", axum::routing::any(api_not_found))
            .fallback(asset)
            .layer(axum::middleware::map_response(secure_headers))
            .layer(axum::middleware::from_fn_with_state(
                site.clone(),
                guard_host,
            ))
            .with_state(site);
        let shutdown = stop.clone();
        let task = tokio::spawn(async move {
            if let Err(error) = axum::serve(listener, router)
                .with_graceful_shutdown(shutdown.cancelled_owned())
                .await
            {
                tracing::warn!(%error, "web entry stopped");
            }
        });
        Ok(Open {
            address,
            bootstrap,
            holders,
            stop,
            task,
        })
    }
}

fn status(hub: &ConnectionHub, state: &Lifecycle) -> WebStatus {
    let open = state.open.as_ref().filter(|open| !open.task.is_finished());
    WebStatus {
        enabled: state.enabled,
        open: open.is_some(),
        address: open.map(|open| open.address),
        connections: hub.entry_count(Entry::Web),
    }
}

fn toggled(state: &Lifecycle) -> WebToggled {
    let open = state.open.as_ref().filter(|open| !open.task.is_finished());
    let (url, address) = open.map_or((None, None), |open| {
        (Some(opened(open).url), Some(open.address))
    });
    WebToggled {
        enabled: state.enabled,
        open: open.is_some(),
        url,
        address,
    }
}

fn opened(open: &Open) -> WebOpened {
    WebOpened {
        url: format!("http://{}/#bootstrap={}", open.address, open.bootstrap),
        address: open.address,
    }
}

async fn bind(port: Option<u16>) -> Result<TcpListener, RpcError> {
    let loopback = |port| SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    if let Some(port) = port {
        return TcpListener::bind(loopback(port))
            .await
            .map_err(|error| match error.kind() {
                std::io::ErrorKind::AddrInUse => RpcError::app(
                    -32000,
                    "address_in_use",
                    format!("Port {port} is already in use"),
                ),
                _ => io_error(error),
            });
    }
    for port in FIRST_PORT..FIRST_PORT + PORT_ATTEMPTS {
        if let Ok(listener) = TcpListener::bind(loopback(port)).await {
            return Ok(listener);
        }
    }
    Err(RpcError::app(
        -32000,
        "address_in_use",
        "No free port for the web entry",
    ))
}

fn io_error(error: std::io::Error) -> RpcError {
    RpcError::app(-32000, "web_entry_failed", error.to_string())
}

fn new_token() -> String {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).expect("operating system randomness");
    URL_SAFE_NO_PAD.encode(bytes)
}

fn digest(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

/// The built frontend `build.rs` embedded (feature `embedded-web`): request
/// path, gzip-compressed, bytes. Empty without the feature.
static EMBEDDED: &[(&str, bool, &[u8])] = include!(concat!(env!("OUT_DIR"), "/web_assets.rs"));

/// What `scripts/stage-web.mjs` writes beside a built frontend.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct WebManifest {
    protocol_version: u32,
}

/// A frontend directory that records the protocol it was built for must
/// match this server's; one without the record (a development build) is
/// served as it is.
fn check_protocol(root: &Path) -> Result<(), RpcError> {
    let Some(file) = confined(root, "/dimcode-web.json") else {
        return Ok(());
    };
    let bytes = std::fs::read(file).map_err(io_error)?;
    let manifest: WebManifest = serde_json::from_slice(&bytes).map_err(|_| {
        RpcError::app(
            -32000,
            "web_assets_incompatible",
            "The Web workspace's dimcode-web.json is not readable",
        )
    })?;
    if manifest.protocol_version != crate::protocol::PROTOCOL_VERSION {
        return Err(RpcError::app(
            -32000,
            "web_assets_incompatible",
            "The Web workspace was built for another App Server protocol version",
        ));
    }
    Ok(())
}

/// Where the entry reads the frontend: the embedded build, or a directory
/// given with `--web-assets` (development against a live `vite build --watch`).
enum Assets {
    Embedded,
    Directory(PathBuf),
}

/// One asset, gzip-compressed when `gzip` is set.
struct Asset {
    bytes: std::borrow::Cow<'static, [u8]>,
    gzip: bool,
}

impl Assets {
    async fn get(&self, path: &str) -> Option<Asset> {
        match self {
            Self::Embedded => EMBEDDED
                .iter()
                .find(|(embedded, ..)| *embedded == path)
                .map(|(_, gzip, bytes)| Asset {
                    bytes: (*bytes).into(),
                    gzip: *gzip,
                }),
            Self::Directory(root) => {
                let file = confined(root, path)?;
                let bytes = tokio::fs::read(file).await.ok()?;
                Some(Asset {
                    bytes: bytes.into(),
                    gzip: false,
                })
            }
        }
    }
}

struct Site {
    stopping: CancellationToken,
    uploads: tokio::sync::Semaphore,
    hub: Arc<ConnectionHub>,
    assets: Assets,
    port: u16,
    bootstrap: [u8; 32],
    /// Hashes of access tokens issued by `exchange`.
    access: Mutex<HashSet<[u8; 32]>>,
    throttle: Mutex<Throttle>,
}

#[derive(Default)]
struct Throttle {
    failures: u32,
    blocked_until: Option<Instant>,
}

impl Site {
    /// Requests must come from the page this entry served; `guard_host`
    /// already confined `Host` to the bound address.
    fn origin_allowed(&self, headers: &HeaderMap) -> bool {
        let host = headers.get(header::HOST).and_then(|v| v.to_str().ok());
        let origin = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok());
        matches!((host, origin), (Some(host), Some(origin)) if origin == format!("http://{host}"))
    }
    fn host_allowed(&self, host: &str) -> bool {
        host == format!("127.0.0.1:{}", self.port) || host == format!("localhost:{}", self.port)
    }
    fn authenticated(&self, headers: &HeaderMap) -> bool {
        let Some(token) = cookie(headers) else {
            return false;
        };
        self.access
            .lock()
            .expect("web access tokens")
            .contains(&digest(token))
    }
}

fn cookie(headers: &HeaderMap) -> Option<&str> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .find_map(|pair| pair.trim().strip_prefix(COOKIE)?.strip_prefix('='))
}

fn constant_eq(a: &[u8; 32], b: &[u8; 32]) -> bool {
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn error(status: StatusCode, code: &str, message: &str) -> Response {
    let body = serde_json::json!({ "error": { "code": code, "message": message } });
    (
        status,
        [(header::CONTENT_TYPE, "application/json")],
        body.to_string(),
    )
        .into_response()
}

async fn guard_host(
    State(site): State<Arc<Site>>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let host = request
        .headers()
        .get(header::HOST)
        .and_then(|v| v.to_str().ok());
    if !host.is_some_and(|host| site.host_allowed(host)) {
        return error(
            StatusCode::MISDIRECTED_REQUEST,
            "invalid_host",
            "Unknown host",
        );
    }
    next.run(request).await
}

async fn secure_headers(mut response: Response) -> Response {
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(CSP),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    response
}

async fn health() -> Response {
    let body = serde_json::json!({ "ok": true, "serverVersion": SERVER_VERSION });
    (
        [(header::CONTENT_TYPE, "application/json")],
        body.to_string(),
    )
        .into_response()
}

async fn exchange(State(site): State<Arc<Site>>, headers: HeaderMap, body: Bytes) -> Response {
    if !site.origin_allowed(&headers) {
        return error(StatusCode::FORBIDDEN, "forbidden", "Cross-origin request");
    }
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Exchange {
        token: String,
    }
    let Ok(Exchange { token }) = serde_json::from_slice(&body) else {
        return error(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "Expected {token}",
        );
    };
    {
        let mut throttle = site.throttle.lock().expect("web throttle");
        let now = Instant::now();
        if let Some(until) = throttle.blocked_until.filter(|until| *until > now) {
            let mut response = error(
                StatusCode::TOO_MANY_REQUESTS,
                "rate_limited",
                "Too many attempts",
            );
            let seconds = (until - now).as_secs().max(1);
            response
                .headers_mut()
                .insert(header::RETRY_AFTER, HeaderValue::from(seconds));
            return response;
        }
        if !constant_eq(&digest(&token), &site.bootstrap) {
            throttle.failures += 1;
            if throttle.failures > FREE_FAILURES {
                let exponent = (throttle.failures - FREE_FAILURES - 1).min(6);
                throttle.blocked_until =
                    Some(now + Duration::from_secs(1 << exponent).min(MAX_BACKOFF));
            }
            return error(StatusCode::UNAUTHORIZED, "unauthorized", "Invalid token");
        }
        *throttle = Throttle::default();
    }
    let access = new_token();
    site.access
        .lock()
        .expect("web access tokens")
        .insert(digest(&access));
    let cookie = format!("{COOKIE}={access}; HttpOnly; SameSite=Strict; Path=/");
    (
        StatusCode::NO_CONTENT,
        [(
            header::SET_COOKIE,
            HeaderValue::from_str(&cookie).expect("token is base64url"),
        )],
    )
        .into_response()
}

/// Lets the page tell a revoked sign-in from an entry that is down.
async fn session(State(site): State<Arc<Site>>, headers: HeaderMap) -> Response {
    if site.authenticated(&headers) {
        StatusCode::NO_CONTENT.into_response()
    } else {
        error(StatusCode::UNAUTHORIZED, "unauthorized", "Sign in again")
    }
}

async fn logout(State(site): State<Arc<Site>>, headers: HeaderMap) -> Response {
    if !site.origin_allowed(&headers) {
        return error(StatusCode::FORBIDDEN, "forbidden", "Cross-origin request");
    }
    if let Some(token) = cookie(&headers) {
        site.access
            .lock()
            .expect("web access tokens")
            .remove(&digest(token));
    }
    (
        StatusCode::NO_CONTENT,
        [(
            header::SET_COOKIE,
            HeaderValue::from_static("dim_web=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0"),
        )],
    )
        .into_response()
}

async fn upgrade(
    State(site): State<Arc<Site>>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    if !site.origin_allowed(&headers) {
        return error(StatusCode::FORBIDDEN, "forbidden", "Cross-origin request");
    }
    if !site.authenticated(&headers) {
        return error(StatusCode::UNAUTHORIZED, "unauthorized", "Sign in again");
    }
    let hub = site.hub.clone();
    if hub.control.shutdown().is_cancelled() {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    ws.max_message_size(MAX_FRAME_BYTES)
        .max_frame_size(MAX_FRAME_BYTES)
        .on_upgrade(move |socket| async move {
            let tracker = hub.tasks.clone();
            let task = tracker.spawn(async move {
                run_ws(hub, socket, Entry::Web).await;
            });
            let _ = task.await;
        })
}

async fn api_not_found() -> Response {
    error(StatusCode::NOT_FOUND, "not_found", "Unknown endpoint")
}

/// Serves a frontend file, falling back to `index.html` for application
/// routes.
async fn asset(
    State(site): State<Arc<Site>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
) -> Response {
    if method != Method::GET && method != Method::HEAD {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }
    let path = uri.path();
    let immutable = path.starts_with("/assets/");
    let (path, cache) = match site.assets.get(path).await {
        Some(asset) if immutable => {
            return send(path, asset, "public, max-age=31536000, immutable", &headers);
        }
        Some(asset) => return send(path, asset, "no-cache", &headers),
        None if immutable => return StatusCode::NOT_FOUND.into_response(),
        None => ("/index.html", "no-store"),
    };
    match site.assets.get(path).await {
        Some(asset) => send(path, asset, cache, &headers),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

fn send(path: &str, asset: Asset, cache: &'static str, request: &HeaderMap) -> Response {
    let accepts_gzip = request
        .get_all(header::ACCEPT_ENCODING)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .any(|value| {
            value
                .split(',')
                .any(|coding| coding.trim().starts_with("gzip"))
        });
    let mut response = (
        [
            (header::CONTENT_TYPE, content_type(Path::new(path))),
            (header::CACHE_CONTROL, cache),
            (header::VARY, "Accept-Encoding"),
        ],
        Body::from(if asset.gzip && !accepts_gzip {
            let mut bytes = Vec::new();
            std::io::Read::read_to_end(
                &mut flate2::read::GzDecoder::new(&asset.bytes[..]),
                &mut bytes,
            )
            .expect("build.rs embeds valid gzip");
            bytes.into()
        } else {
            asset.bytes
        }),
    )
        .into_response();
    if asset.gzip && accepts_gzip {
        response
            .headers_mut()
            .insert(header::CONTENT_ENCODING, HeaderValue::from_static("gzip"));
    }
    response
}

/// Resolves a request path to a regular file inside `root`, or `None`.
fn confined(root: &Path, path: &str) -> Option<PathBuf> {
    let relative = Path::new(path.trim_start_matches('/'));
    if path.contains('\\')
        || path.contains('\0')
        || !relative
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
    {
        return None;
    }
    let file = root.join(relative).canonicalize().ok()?;
    (file.starts_with(root) && file.is_file()).then_some(file)
}

fn content_type(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" | "map" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "ttf" => "font/ttf",
        "wasm" => "application/wasm",
        "txt" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

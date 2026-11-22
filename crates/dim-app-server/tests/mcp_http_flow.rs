use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response, Sse, sse::Event},
    routing::post,
};
use dim_agent_capabilities::file_storage::FileStoragePlugin;
use dim_agent_runtime::Names;
use dim_agent_runtime::wait_active;
use dim_agent_types::{ContentBlock, ToolContent};
use dim_app_server::{
    mcp::{
        McpChange, McpError, McpPlugin, McpService,
        config::McpServerSettings,
        server::{McpServerError, McpServerState},
    },
    settings::{SettingsPlugin, SettingsScope, SettingsService},
};
use rutis::Ctx;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, Key, TypeKey};
use rutis_loader::resolve::Build;
use serde_json::{Value, json};
use std::{
    convert::Infallible,
    fs,
    path::PathBuf,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::sync::{Notify, broadcast};
use tokio_util::sync::CancellationToken;

struct Capture {
    service: Arc<OnceLock<Arc<McpService>>>,
    mcp: Key<McpService>,
}
impl DynPlugin for Capture {
    fn name(&self) -> &str {
        "McpHttpTest"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.mcp.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let mcp = ctx.require_at(&self.mcp)?;
            assert!(self.service.set(mcp).is_ok());
            Ok(Effect::Done)
        })
    }
}

struct Fixture {
    root: Ctx,
    publication: mcp_host::Host,
    mcp: mcp_host::Directory,
    home: PathBuf,
}
use crate::mcp_host;
impl Fixture {
    async fn new(url: &str, token: &str) -> Self {
        Self::configured(json!({"type":"http","url":url,"auth":{"type":"bearer","token":token}}))
            .await
    }
    async fn configured(configuration: Value) -> Self {
        let home = std::env::temp_dir().join(format!("dim-mcp-http-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&home).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&home, fs::Permissions::from_mode(0o700)).unwrap();
        }
        let path = home.join("mcp.json");
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(path).unwrap();
        use std::io::Write;
        file.write_all(&serde_json::to_vec(&json!({"mcpServers":{"docs":configuration}})).unwrap())
            .unwrap();
        let root = Ctx::root().unwrap();
        mcp_host::provide_credentials(&root);
        let catalog = dim_app_server::catalog();
        let build = Build::default();
        let names = Names::copy(&catalog, &build);
        let (publication, loader) = mcp_host::Host::start(&root).await;
        let capture = Arc::new(OnceLock::new());
        root.provide_as::<dim_agent_runtime::PartRegistry>(Arc::new(
            dim_agent_runtime::PartRegistry::default(),
        ))
        .unwrap();
        let views = vec![
            root.plugin(FileStoragePlugin::new(vec![
                fs::canonicalize(&home).unwrap(),
            ])),
            root.plugin(SettingsPlugin::new(&names, 0, vec![]).unwrap()),
            root.plugin(
                dim_app_server::methods::ManagementMethodsPlugin::typed(&dim_app_server::catalog())
                    .unwrap(),
            ),
            root.plugin(McpPlugin::new(&names, 0).unwrap()),
            root.plugin(Capture {
                service: capture.clone(),
                mcp: dim_app_server::mcp::owner_key(),
            }),
        ];
        wait_active(
            &root,
            &views,
            Duration::from_secs(5),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        let mcp = mcp_host::Directory::new(capture.get().unwrap().clone(), &root, loader);
        Self {
            root,
            publication,
            mcp,
            home,
        }
    }
    async fn close(&self) {
        self.publication.close().await;
        self.root.shutdown().await.unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.home);
    }
}

struct Remote {
    initializes: AtomicUsize,
    calls: AtomicUsize,
    /// The request id of every `tools/call` the server saw.
    call_ids: Mutex<Vec<Value>>,
    cancelled: AtomicUsize,
    /// The request id and reason of every `notifications/cancelled`.
    cancelled_ids: Mutex<Vec<Value>>,
    deletes: AtomicUsize,
    slow: Notify,
    release_call: Notify,
    deleting: Notify,
    release_delete: Notify,
    repeating: AtomicBool,
    nullable: AtomicBool,
    /// Makes the handshake take longer than a short entry `timeout`.
    slow_initialize: AtomicBool,
    session: AtomicUsize,
    session_check: AtomicBool,
    expire_once: AtomicBool,
    expire_always: AtomicBool,
    grown: AtomicBool,
    events: broadcast::Sender<String>,
}
impl Remote {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            initializes: AtomicUsize::new(0),
            calls: AtomicUsize::new(0),
            call_ids: Mutex::new(Vec::new()),
            cancelled: AtomicUsize::new(0),
            cancelled_ids: Mutex::new(Vec::new()),
            deletes: AtomicUsize::new(0),
            slow: Notify::new(),
            release_call: Notify::new(),
            deleting: Notify::new(),
            release_delete: Notify::new(),
            repeating: AtomicBool::new(false),
            nullable: AtomicBool::new(false),
            slow_initialize: AtomicBool::new(false),
            session: AtomicUsize::new(0),
            session_check: AtomicBool::new(false),
            expire_once: AtomicBool::new(false),
            expire_always: AtomicBool::new(false),
            grown: AtomicBool::new(false),
            events: broadcast::channel(16).0,
        })
    }
}
async fn rpc(
    State(remote): State<Arc<Remote>>,
    headers: HeaderMap,
    Json(message): Json<Value>,
) -> Response {
    if headers.get("authorization").and_then(|h| h.to_str().ok()) != Some("Bearer fixture-only") {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let method = message["method"].as_str().unwrap();
    let checked = remote.session_check.load(Ordering::SeqCst);
    let current_session = format!("s{}", remote.session.load(Ordering::SeqCst));
    if checked
        && method != "initialize"
        && method != "notifications/initialized"
        && headers.get("mcp-session-id").and_then(|h| h.to_str().ok())
            != Some(current_session.as_str())
    {
        return StatusCode::NOT_FOUND.into_response();
    }
    let mut response_session = current_session;
    let result = match method {
        "initialize" => {
            remote.initializes.fetch_add(1, Ordering::SeqCst);
            if remote.slow_initialize.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(1500)).await;
            }
            response_session = format!("s{}", remote.session.fetch_add(1, Ordering::SeqCst) + 1);
            json!({"protocolVersion":message["params"]["protocolVersion"],"capabilities":{"tools":{"listChanged":true}},"serverInfo":{"name":"local-mcp","version":"1"}})
        }
        "notifications/initialized" => return StatusCode::ACCEPTED.into_response(),
        "notifications/cancelled" => {
            remote.cancelled.fetch_add(1, Ordering::SeqCst);
            remote
                .cancelled_ids
                .lock()
                .unwrap()
                .push(message["params"].clone());
            remote.release_call.notify_waiters();
            return StatusCode::ACCEPTED.into_response();
        }
        "tools/list" => {
            if message["params"]["cursor"].as_str() == Some("next") {
                if remote.repeating.load(Ordering::SeqCst) {
                    json!({"tools":[],"nextCursor":"next"})
                } else {
                    json!({"tools":[{"name":"slow","inputSchema":{"type":"object"}}]})
                }
            } else {
                let mut tools = vec![
                    json!({"name":"echo","description":"Echo arguments","inputSchema":{"type":"object","properties":{"value":{"type":"string"}}}}),
                ];
                if remote.nullable.load(Ordering::SeqCst) {
                    tools.push(json!({"name":"spaces","inputSchema":{"type":"object","properties":{"space_id":{"type":["string","null"]}}}}));
                }
                if checked {
                    tools.push(json!({"name":"expire-once","inputSchema":{"type":"object"}}));
                    tools.push(json!({"name":"expire-always","inputSchema":{"type":"object"}}));
                }
                if checked
                    && remote.grown.load(Ordering::SeqCst)
                    && remote.initializes.load(Ordering::SeqCst) >= 2
                {
                    tools.push(json!({"name":"grown","inputSchema":{"type":"object"}}));
                }
                json!({"tools":tools,"nextCursor":"next"})
            }
        }
        "tools/call" => {
            remote.calls.fetch_add(1, Ordering::SeqCst);
            remote.call_ids.lock().unwrap().push(message["id"].clone());
            match message["params"]["name"].as_str().unwrap() {
                "expire" => return StatusCode::NOT_FOUND.into_response(),
                "expire-once" if remote.expire_once.swap(false, Ordering::SeqCst) => {
                    remote.session.fetch_add(1, Ordering::SeqCst);
                    return StatusCode::NOT_FOUND.into_response();
                }
                "expire-once" => {
                    json!({"content":[{"type":"text","text":"expired-once"}]})
                }
                "expire-always" if remote.expire_always.load(Ordering::SeqCst) => {
                    remote.session.fetch_add(1, Ordering::SeqCst);
                    return StatusCode::NOT_FOUND.into_response();
                }
                "expire-always" => {
                    json!({"content":[{"type":"text","text":"expired-always"}]})
                }
                "slow" => {
                    remote.slow.notify_one();
                    remote.release_call.notified().await;
                    json!({"content":[{"type":"text","text":"settled"}]})
                }
                "sleep" => {
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    json!({"content":[{"type":"text","text":"slept"}]})
                }
                "error" => json!({"content":[{"type":"text","text":"denied"}],"isError":true}),
                _ => {
                    json!({"content":[{"type":"text","text":message["params"]["arguments"]["value"]}],"structuredContent":{"cursor":"page-2"},"isError":false})
                }
            }
        }
        _ => return StatusCode::BAD_REQUEST.into_response(),
    };
    let response = json!({"jsonrpc":"2.0","id":message["id"],"result":result});
    if message["method"] == "tools/call" {
        (
            [("content-type", "text/event-stream")],
            format!("event: message\ndata: {response}\n\n"),
        )
            .into_response()
    } else {
        (
            [("mcp-session-id", response_session.as_str())],
            Json(response),
        )
            .into_response()
    }
}
async fn events(State(remote): State<Arc<Remote>>) -> impl IntoResponse {
    let receiver = remote.events.subscribe();
    Sse::new(futures_util::stream::unfold(
        receiver,
        |mut receiver| async move {
            receiver
                .recv()
                .await
                .ok()
                .map(|data| (Ok::<_, Infallible>(Event::default().data(data)), receiver))
        },
    ))
}
async fn delete(State(remote): State<Arc<Remote>>) -> StatusCode {
    remote.deleting.notify_one();
    remote.release_delete.notified().await;
    remote.deletes.fetch_add(1, Ordering::SeqCst);
    StatusCode::NO_CONTENT
}

#[tokio::test]
async fn http_shutdown_aborts_stalled_get_and_bounds_session_delete() {
    let remote = Remote::new();
    let get_started = Arc::new(Notify::new());
    let release_get = CancellationToken::new();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    let stop = CancellationToken::new();
    let server = tokio::spawn(
        axum::serve(
            listener,
            Router::new()
                .route(
                    "/mcp",
                    post(rpc)
                        .get({
                            let started = get_started.clone();
                            let release = release_get.clone();
                            move || {
                                let started = started.clone();
                                let release = release.clone();
                                async move {
                                    started.notify_one();
                                    release.cancelled().await;
                                    StatusCode::NO_CONTENT
                                }
                            }
                        })
                        .delete(delete),
                )
                .with_state(remote.clone()),
        )
        .with_graceful_shutdown(stop.clone().cancelled_owned())
        .into_future(),
    );
    let fixture = Fixture::new(&url, "fixture-only").await;
    let cancel = CancellationToken::new();
    let connection = fixture
        .mcp
        .server(None, "docs".into(), &cancel)
        .await
        .unwrap();
    assert_eq!(connection.tools(&cancel).await.unwrap().len(), 2);
    tokio::time::timeout(Duration::from_secs(5), get_started.notified())
        .await
        .unwrap();
    let closing = {
        let root = fixture.root.clone();
        tokio::spawn(async move { root.shutdown().await })
    };
    tokio::time::timeout(Duration::from_secs(5), remote.deleting.notified())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(8), closing)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(remote.deletes.load(Ordering::SeqCst), 0);
    assert_eq!(connection.state(), McpServerState::Closed);
    assert!(matches!(
        connection.tools(&cancel).await,
        Err(McpServerError::Closed)
    ));
    release_get.cancel();
    remote.release_delete.notify_one();
    stop.cancel();
    server.await.unwrap().unwrap();
}

#[tokio::test]
async fn http_plugin_discovers_calls_cancels_and_drains_the_remote_session() {
    let remote = Remote::new();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    let stop = CancellationToken::new();
    let server = tokio::spawn(
        axum::serve(
            listener,
            Router::new()
                .route("/mcp", post(rpc).get(events).delete(delete))
                .with_state(remote.clone()),
        )
        .with_graceful_shutdown(stop.clone().cancelled_owned())
        .into_future(),
    );
    let fixture = Fixture::new(&url, "fixture-only").await;
    let cancel = CancellationToken::new();
    let connection = fixture
        .mcp
        .server(None, "docs".into(), &cancel)
        .await
        .unwrap();
    assert_eq!(remote.initializes.load(Ordering::SeqCst), 0);
    assert_eq!(connection.state(), McpServerState::Disconnected);
    let (first, second) = tokio::join!(connection.tools(&cancel), connection.tools(&cancel));
    assert_eq!(
        first
            .unwrap()
            .iter()
            .map(|t| t.name.as_str())
            .collect::<Vec<_>>(),
        vec!["echo", "slow"]
    );
    assert_eq!(second.unwrap().len(), 2);
    assert_eq!(remote.initializes.load(Ordering::SeqCst), 1);
    assert_eq!(connection.state(), McpServerState::Connected);
    let again = fixture
        .mcp
        .server(None, "docs".into(), &cancel)
        .await
        .unwrap();
    assert!(Arc::ptr_eq(&connection, &again));
    let catalog = fixture.mcp.catalog(None, &cancel).await.unwrap();
    let valid = catalog.servers["docs"].configuration.clone();
    let mut invalid = valid.clone();
    let McpServerSettings::Remote {
        url: invalid_url, ..
    } = &mut invalid
    else {
        unreachable!()
    };
    *invalid_url = "file:///not-an-http-server".into();
    let revision = fixture
        .mcp
        .update(
            SettingsScope::User,
            catalog.user_revision,
            "docs".into(),
            McpChange::Put(invalid),
            &cancel,
        )
        .await
        .unwrap();
    assert!(matches!(
        fixture.mcp.server(None, "docs".into(), &cancel).await,
        Err(McpError::Server(McpServerError::Configuration(_)))
    ));
    assert_eq!(connection.state(), McpServerState::Connected);
    connection.tools(&cancel).await.unwrap();
    fixture
        .mcp
        .update(
            SettingsScope::User,
            revision,
            "docs".into(),
            McpChange::Put(valid),
            &cancel,
        )
        .await
        .unwrap();
    let restored = fixture
        .mcp
        .server(None, "docs".into(), &cancel)
        .await
        .unwrap();
    assert!(Arc::ptr_eq(&connection, &restored));
    assert_eq!(remote.initializes.load(Ordering::SeqCst), 1);
    let result = connection
        .call(
            "echo".into(),
            json!({"value":"real-http"}).as_object().unwrap().clone(),
            &cancel,
        )
        .await
        .unwrap();
    assert_eq!(
        result.structured_content.as_ref().unwrap()["cursor"],
        "page-2"
    );
    let ToolContent::Blocks(content) = result.content else {
        panic!("expected blocks")
    };
    assert_eq!(
        content,
        vec![
            ContentBlock::Text {
                text: "real-http".into()
            },
            ContentBlock::Text {
                text: "{\"cursor\":\"page-2\"}".into()
            }
        ]
    );
    assert_eq!(
        connection
            .call("error".into(), Default::default(), &cancel)
            .await
            .unwrap()
            .is_error,
        Some(true)
    );
    let mut changes = connection.tool_changes();
    remote
        .events
        .send(json!({"jsonrpc":"2.0","method":"notifications/tools/list_changed"}).to_string())
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), changes.changed())
        .await
        .unwrap()
        .unwrap();
    remote.repeating.store(true, Ordering::SeqCst);
    assert!(matches!(
        connection.tools(&cancel).await,
        Err(McpServerError::InvalidResponse(_))
    ));
    let cancelled = CancellationToken::new();
    let worker = {
        let connection = connection.clone();
        let cancel = cancelled.clone();
        tokio::spawn(async move {
            connection
                .call("slow".into(), Default::default(), &cancel)
                .await
        })
    };
    tokio::time::timeout(Duration::from_secs(5), remote.slow.notified())
        .await
        .unwrap();
    cancelled.cancel();
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(5), worker)
            .await
            .unwrap()
            .unwrap(),
        Err(McpServerError::Cancelled)
    ));
    assert_eq!(remote.cancelled.load(Ordering::SeqCst), 1);
    let closing = {
        let mcp = fixture.mcp.clone();
        tokio::spawn(async move {
            mcp.disconnect(
                SettingsScope::User,
                "docs".into(),
                &CancellationToken::new(),
            )
            .await
        })
    };
    tokio::time::timeout(Duration::from_secs(5), remote.deleting.notified())
        .await
        .unwrap();
    assert!(!closing.is_finished());
    remote.release_delete.notify_one();
    tokio::time::timeout(Duration::from_secs(5), closing)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(remote.deletes.load(Ordering::SeqCst), 1);
    assert_eq!(connection.state(), McpServerState::Closed);
    assert!(matches!(
        connection.tools(&cancel).await,
        Err(McpServerError::Closed)
    ));
    let replacement = fixture
        .mcp
        .server(None, "docs".into(), &cancel)
        .await
        .unwrap();
    assert!(!Arc::ptr_eq(&connection, &replacement));
    remote.repeating.store(false, Ordering::SeqCst);
    replacement.tools(&cancel).await.unwrap();
    let catalog = fixture.mcp.catalog(None, &cancel).await.unwrap();
    remote.release_delete.notify_one();
    fixture
        .mcp
        .update(
            SettingsScope::User,
            catalog.user_revision,
            "docs".into(),
            McpChange::SetEnabled(false),
            &cancel,
        )
        .await
        .unwrap();
    assert_eq!(replacement.state(), McpServerState::Closed);
    assert!(matches!(
        fixture.mcp.server(None, "docs".into(), &cancel).await,
        Err(McpError::Disabled)
    ));
    assert!(matches!(
        replacement.tools(&cancel).await,
        Err(McpServerError::Closed)
    ));
    drop(replacement);
    assert_eq!(remote.deletes.load(Ordering::SeqCst), 2);

    let catalog = fixture.mcp.catalog(None, &cancel).await.unwrap();
    fixture
        .mcp
        .update(
            SettingsScope::User,
            catalog.user_revision,
            "docs".into(),
            McpChange::SetEnabled(true),
            &cancel,
        )
        .await
        .unwrap();
    let current = fixture
        .mcp
        .server(None, "docs".into(), &cancel)
        .await
        .unwrap();
    let worker = {
        let current = current.clone();
        tokio::spawn(async move {
            current
                .call("slow".into(), Default::default(), &CancellationToken::new())
                .await
        })
    };
    tokio::time::timeout(Duration::from_secs(5), remote.slow.notified())
        .await
        .unwrap();
    remote.release_delete.notify_one();
    tokio::time::timeout(Duration::from_secs(5), fixture.close())
        .await
        .unwrap();
    assert!(matches!(
        worker.await.unwrap(),
        Err(McpServerError::Cancelled)
    ));
    assert_eq!(current.state(), McpServerState::Closed);
    assert_eq!(remote.deletes.load(Ordering::SeqCst), 3);
    assert_eq!(remote.cancelled.load(Ordering::SeqCst), 2);

    let rejected = Fixture::new(&url, "rejected").await;
    let connection = rejected
        .mcp
        .server(None, "docs".into(), &cancel)
        .await
        .unwrap();
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(5), connection.tools(&cancel))
            .await
            .unwrap(),
        Err(McpServerError::RequestFailed)
    ));
    assert_eq!(connection.state(), McpServerState::Failed);
    tokio::time::timeout(Duration::from_secs(5), rejected.close())
        .await
        .unwrap();
    fixture.close().await;
    stop.cancel();
    server.await.unwrap().unwrap();
}

/// #953: an entry `timeout` bounds its `tools/call` requests, names itself in
/// the error, and cancels the request server-side.
#[tokio::test]
async fn a_tool_call_waits_its_entry_timeout_and_tells_the_server() {
    let remote = Remote::new();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    let stop = CancellationToken::new();
    let server = tokio::spawn(
        axum::serve(
            listener,
            Router::new()
                .route("/mcp", post(rpc).get(events).delete(delete))
                .with_state(remote.clone()),
        )
        .with_graceful_shutdown(stop.clone().cancelled_owned())
        .into_future(),
    );
    let fixture = Fixture::configured(json!({
        "type":"http","url":url,"auth":{"type":"bearer","token":"fixture-only"},"timeout":1000
    }))
    .await;
    let cancel = CancellationToken::new();
    let connection = fixture
        .mcp
        .server(None, "docs".into(), &cancel)
        .await
        .unwrap();
    let started = std::time::Instant::now();
    let error = tokio::time::timeout(
        Duration::from_secs(5),
        connection.call("sleep".into(), Default::default(), &cancel),
    )
    .await
    .expect("the entry's one-second limit ends the call before this deadline")
    .unwrap_err();
    let elapsed = started.elapsed();
    assert!(elapsed >= Duration::from_millis(900), "{elapsed:?}");
    let message = error.to_string();
    assert!(message.contains("1000 ms"), "{message}");
    assert!(message.contains("the entry's timeout"), "{message}");
    tokio::time::timeout(Duration::from_secs(5), async {
        while remote.cancelled.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the server receives notifications/cancelled for the call");
    let call = remote.call_ids.lock().unwrap().last().cloned().unwrap();
    let notices = remote.cancelled_ids.lock().unwrap().clone();
    assert_eq!(
        notices
            .iter()
            .map(|notice| &notice["requestId"])
            .collect::<Vec<_>>(),
        vec![&call]
    );
    let reason = notices[0]["reason"].as_str().unwrap();
    assert!(reason.contains("1000 ms"), "{reason}");
    assert!(reason.contains("the entry's timeout"), "{reason}");
    fixture.close().await;
    stop.cancel();
    server.await.unwrap().unwrap();
}

/// #953 D20: connecting counts against the call's limit. A call whose limit
/// passed while the connection was starting fails with the entry's timeout
/// and the request never leaves, so the server sees no call and no
/// cancellation.
#[tokio::test]
async fn a_call_whose_deadline_passed_while_connecting_sends_nothing() {
    let remote = Remote::new();
    remote.slow_initialize.store(true, Ordering::SeqCst);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    let stop = CancellationToken::new();
    let server = tokio::spawn(
        axum::serve(
            listener,
            Router::new()
                .route("/mcp", post(rpc).get(events).delete(delete))
                .with_state(remote.clone()),
        )
        .with_graceful_shutdown(stop.clone().cancelled_owned())
        .into_future(),
    );
    let fixture = Fixture::configured(json!({
        "type":"http","url":url,"auth":{"type":"bearer","token":"fixture-only"},"timeout":1000
    }))
    .await;
    let cancel = CancellationToken::new();
    // Acquiring the entry opens nothing; the call starts the connection, and
    // this fixture's handshake outlasts the entry's one-second limit.
    let connection = fixture
        .mcp
        .server(None, "docs".into(), &cancel)
        .await
        .unwrap();
    let started = std::time::Instant::now();
    let error = connection
        .call("echo".into(), Default::default(), &cancel)
        .await
        .unwrap_err();
    assert!(started.elapsed() >= Duration::from_millis(1000));
    let message = error.to_string();
    assert!(message.contains("1000 ms"), "{message}");
    assert!(message.contains("the entry's timeout"), "{message}");
    // A request sent after the deadline would still reach the fixture; give
    // one time to arrive before asserting that none did.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(remote.initializes.load(Ordering::SeqCst), 1);
    assert_eq!(remote.calls.load(Ordering::SeqCst), 0);
    assert_eq!(remote.cancelled.load(Ordering::SeqCst), 0);
    fixture.close().await;
    stop.cancel();
    server.await.unwrap().unwrap();
}

/// #953: without an entry `timeout` the same call waits for the ten-minute
/// default, so a two-second answer arrives.
#[tokio::test]
async fn a_tool_call_without_a_timeout_waits_for_the_default() {
    let remote = Remote::new();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    let stop = CancellationToken::new();
    let server = tokio::spawn(
        axum::serve(
            listener,
            Router::new()
                .route("/mcp", post(rpc).get(events).delete(delete))
                .with_state(remote.clone()),
        )
        .with_graceful_shutdown(stop.clone().cancelled_owned())
        .into_future(),
    );
    let fixture = Fixture::new(&url, "fixture-only").await;
    let cancel = CancellationToken::new();
    let connection = fixture
        .mcp
        .server(None, "docs".into(), &cancel)
        .await
        .unwrap();
    let started = std::time::Instant::now();
    let result = connection
        .call("sleep".into(), Default::default(), &cancel)
        .await
        .unwrap();
    assert!(started.elapsed() >= Duration::from_secs(2));
    let ToolContent::Blocks(blocks) = result.content else {
        panic!("expected blocks")
    };
    assert!(
        blocks
            .iter()
            .any(|block| matches!(block, ContentBlock::Text { text } if text == "slept"))
    );
    fixture.close().await;
    stop.cancel();
    server.await.unwrap().unwrap();
}

/// #953 design 4: an interrupt cancels the call and sends the same
/// cancellation notice a timeout does.
#[tokio::test]
async fn an_interrupt_sends_the_cancellation_notice() {
    let remote = Remote::new();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    let stop = CancellationToken::new();
    let server = tokio::spawn(
        axum::serve(
            listener,
            Router::new()
                .route("/mcp", post(rpc).get(events).delete(delete))
                .with_state(remote.clone()),
        )
        .with_graceful_shutdown(stop.clone().cancelled_owned())
        .into_future(),
    );
    let fixture = Fixture::new(&url, "fixture-only").await;
    let connected = CancellationToken::new();
    let connection = fixture
        .mcp
        .server(None, "docs".into(), &connected)
        .await
        .unwrap();
    let cancelled = CancellationToken::new();
    let worker = {
        let connection = connection.clone();
        let cancel = cancelled.clone();
        tokio::spawn(async move {
            connection
                .call("slow".into(), Default::default(), &cancel)
                .await
        })
    };
    tokio::time::timeout(Duration::from_secs(5), remote.slow.notified())
        .await
        .unwrap();
    cancelled.cancel();
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(5), worker)
            .await
            .unwrap()
            .unwrap(),
        Err(McpServerError::Cancelled)
    ));
    tokio::time::timeout(Duration::from_secs(5), async {
        while remote.cancelled.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the server receives notifications/cancelled for the call");
    let call = remote.call_ids.lock().unwrap().last().cloned().unwrap();
    let notices = remote.cancelled_ids.lock().unwrap().clone();
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0]["requestId"], call);
    fixture.close().await;
    stop.cancel();
    server.await.unwrap().unwrap();
}

#[tokio::test]
async fn http_plugin_accepts_a_type_array_and_stores_the_canonical_form() {
    let remote = Remote::new();
    remote.nullable.store(true, Ordering::SeqCst);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    let stop = CancellationToken::new();
    let server = tokio::spawn(
        axum::serve(
            listener,
            Router::new()
                .route("/mcp", post(rpc).get(events).delete(delete))
                .with_state(remote.clone()),
        )
        .with_graceful_shutdown(stop.clone().cancelled_owned())
        .into_future(),
    );
    let fixture = Fixture::new(&url, "fixture-only").await;
    let cancel = CancellationToken::new();
    let connection = fixture
        .mcp
        .server(None, "docs".into(), &cancel)
        .await
        .unwrap();
    let tools = connection.tools(&cancel).await.unwrap();
    let spaces = tools.iter().find(|tool| tool.name == "spaces").unwrap();
    assert_eq!(
        serde_json::to_value(&spaces.input_schema).unwrap(),
        json!({"type":"object","properties":{"space_id":{"anyOf":[{"type":"string"},{"type":"null"}]}}})
    );
    fixture.close().await;
    stop.cancel();
    server.await.unwrap().unwrap();
}

#[tokio::test]
async fn connection_generations_live_until_the_last_lease_and_force_close_together() {
    let remote = Remote::new();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    let stop = CancellationToken::new();
    let server = tokio::spawn(
        axum::serve(
            listener,
            Router::new()
                .route("/mcp", post(rpc).get(events).delete(delete))
                .with_state(remote.clone()),
        )
        .with_graceful_shutdown(stop.clone().cancelled_owned())
        .into_future(),
    );
    let fixture = Fixture::new(&url, "fixture-only").await;
    let cancel = CancellationToken::new();
    let first = fixture
        .mcp
        .server(None, "docs".into(), &cancel)
        .await
        .unwrap();
    first.tools(&cancel).await.unwrap();
    let catalog = fixture.mcp.catalog(None, &cancel).await.unwrap();
    let mut config = catalog.servers["docs"].configuration.clone();
    if let McpServerSettings::Remote { headers, .. } = &mut config {
        headers.insert("X-Generation".into(), "second".into());
    }
    fixture
        .mcp
        .update(
            SettingsScope::User,
            catalog.user_revision,
            "docs".into(),
            McpChange::Put(config),
            &cancel,
        )
        .await
        .unwrap();
    let second = fixture
        .mcp
        .server(None, "docs".into(), &cancel)
        .await
        .unwrap();
    second.tools(&cancel).await.unwrap();
    assert!(!Arc::ptr_eq(&first, &second));
    assert_eq!(remote.initializes.load(Ordering::SeqCst), 2);
    assert_eq!(remote.deletes.load(Ordering::SeqCst), 0);
    first.tools(&cancel).await.unwrap();
    let second_state = second.subscribe();
    let mut second = Some(second);
    let mut closing = {
        let mcp = fixture.mcp.clone();
        let cancel = cancel.clone();
        tokio::spawn(async move {
            mcp.disconnect(SettingsScope::User, "docs".into(), &cancel)
                .await
        })
    };
    let mut returned_early = false;
    for generation in 0..2 {
        tokio::time::timeout(Duration::from_secs(5), remote.deleting.notified())
            .await
            .unwrap();
        if generation == 0 {
            drop(second.take());
        } else {
            returned_early = tokio::time::timeout(Duration::from_millis(100), &mut closing)
                .await
                .is_ok();
        }
        remote.release_delete.notify_one();
    }
    if !returned_early {
        closing.await.unwrap().unwrap();
    }
    fixture.close().await;
    assert_eq!(first.state(), McpServerState::Closed);
    assert_eq!(*second_state.borrow(), McpServerState::Closed);
    stop.cancel();
    server.await.unwrap().unwrap();
    assert!(
        !returned_early,
        "disconnect returned before the second cleanup completed"
    );
}

#[tokio::test]
async fn cancelling_a_stalled_handshake_drains_the_plugin() {
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    let stop = CancellationToken::new();
    let router = Router::new().route(
        "/mcp",
        post({
            let entered = entered.clone();
            let release = release.clone();
            move || {
                let entered = entered.clone();
                let release = release.clone();
                async move {
                    entered.notify_one();
                    release.notified().await;
                    StatusCode::SERVICE_UNAVAILABLE
                }
            }
        }),
    );
    let server = tokio::spawn(
        axum::serve(listener, router)
            .with_graceful_shutdown(stop.clone().cancelled_owned())
            .into_future(),
    );
    let fixture = Fixture::new(&url, "fixture-only").await;
    let cancel = CancellationToken::new();
    let connection = fixture
        .mcp
        .server(None, "docs".into(), &cancel)
        .await
        .unwrap();
    let task = {
        let connection = connection.clone();
        let cancel = cancel.clone();
        tokio::spawn(async move { connection.tools(&cancel).await })
    };
    tokio::time::timeout(Duration::from_secs(5), entered.notified())
        .await
        .unwrap();
    cancel.cancel();
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .unwrap()
            .unwrap(),
        Err(McpServerError::Cancelled)
    ));
    assert_eq!(connection.state(), McpServerState::Disconnected);
    tokio::time::timeout(Duration::from_secs(5), fixture.close())
        .await
        .unwrap();
    assert_eq!(connection.state(), McpServerState::Closed);
    release.notify_one();
    stop.cancel();
    server.await.unwrap().unwrap();
}

#[tokio::test]
#[ignore = "Requires CONTEXT7_API_KEY and performs real read-only network calls"]
async fn context7_live_through_rust_plugins() {
    let key = std::env::var("CONTEXT7_API_KEY").expect("CONTEXT7_API_KEY is required");
    let fixture = Fixture::new("https://mcp.context7.com/mcp", &key).await;
    let cancel = CancellationToken::new();
    let connection = fixture
        .mcp
        .server(None, "docs".into(), &cancel)
        .await
        .unwrap();
    let tools = connection.tools(&cancel).await.unwrap();
    assert!(tools.iter().any(|t| t.name == "resolve-library-id"));
    assert!(tools.iter().any(|t| t.name == "query-docs"));
    println!(
        "Rust Plugin handshake and discovery passed: {} tools",
        tools.len()
    );
    let result=connection.call("resolve-library-id".into(),json!({"libraryName":"tokio","query":"Official Tokio Rust async runtime documentation"}).as_object().unwrap().clone(),&cancel).await.unwrap();
    assert_ne!(result.is_error, Some(true));
    let ToolContent::Blocks(blocks) = result.content else {
        panic!("expected blocks")
    };
    let text = blocks
        .iter()
        .filter_map(|b| {
            if let ContentBlock::Text { text } = b {
                Some(text.as_str())
            } else {
                None
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("/tokio-rs/tokio"));
    let docs=connection.call("query-docs".into(),json!({"libraryId":"/tokio-rs/tokio","query":"How to spawn an async task with tokio::spawn and await its JoinHandle?"}).as_object().unwrap().clone(),&cancel).await.unwrap();
    assert_ne!(docs.is_error, Some(true));
    let ToolContent::Blocks(blocks) = docs.content else {
        panic!("expected blocks")
    };
    assert!(blocks.iter().any(|b|matches!(b,ContentBlock::Text{text} if text.contains("spawn") && text.contains("JoinHandle"))));
    println!("Rust Plugin resolve-library-id and query-docs passed");
    fixture.close().await;
    assert_eq!(connection.state(), McpServerState::Closed);
    println!("Rust Plugin shutdown passed");
}

async fn stdio_fixture(mode: &str) -> Fixture {
    let fixture = Fixture::configured(json!({"command":"node"})).await;
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mcp-stdio.cjs");
    fs::write(fixture.home.join("mcp.json"), serde_json::to_vec(&json!({"mcpServers":{"docs":{
        "command":"node", "args":[script, mode, "$(not-a-shell); literal argument"],
        "cwd":fixture.home, "env":{"MCP_TEST_VALUE":"configured value", "MCP_PID_FILE":fixture.home.join("pid")}
    }}})).unwrap()).unwrap();
    fixture
}

fn reconnect_script() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mcp-reconnect.cjs")
}

async fn reconnect_fixture(mode: &str) -> Fixture {
    let fixture = Fixture::configured(json!({"command":"node"})).await;
    fs::write(
        fixture.home.join("mcp.json"),
        serde_json::to_vec(&json!({"mcpServers":{"docs":{
            "command":"node", "args":[reconnect_script(), mode],
            "cwd":fixture.home, "env":{
                "MCP_STATE_FILE":fixture.home.join("starts"),
                "MCP_PID_FILE":fixture.home.join("pids")
            }
        }}}))
        .unwrap(),
    )
    .unwrap();
    fixture
}

fn starts(fixture: &Fixture) -> usize {
    fs::read_to_string(fixture.home.join("starts"))
        .map(|text| text.lines().count())
        .unwrap_or(0)
}

async fn wait_not_connected(connection: &dim_app_server::mcp::McpConnection) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while connection.state() == McpServerState::Connected {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn a_transport_failure_carries_its_exit_status_and_stderr_tail() {
    let fixture = reconnect_fixture("exit-after-call").await;
    let cancel = CancellationToken::new();
    let connection = fixture
        .mcp
        .server(None, "docs".into(), &cancel)
        .await
        .unwrap();
    assert!(
        connection
            .call(
                "echo".into(),
                json!({}).as_object().unwrap().clone(),
                &cancel
            )
            .await
            .is_ok()
    );
    wait_not_connected(&connection).await;
    let failure = connection
        .failure()
        .expect("a transport failure is reported");
    assert!(failure.contains("exit status: 7"), "{failure}");
    let lines: Vec<&str> = failure.lines().collect();
    assert!(
        lines.len() <= 21,
        "the tail keeps at most 20 lines: {failure}"
    );
    assert!(lines.contains(&"stderr-line-30"), "{failure}");
    assert!(lines.contains(&"stderr-line-11"), "{failure}");
    assert!(
        !lines.contains(&"stderr-line-10"),
        "the oldest line is evicted: {failure}"
    );
    fixture.close().await;
}

#[tokio::test]
async fn a_stderr_tail_is_bounded_and_decoded_lossily() {
    for mode in ["big-stderr", "long-stderr"] {
        let fixture = reconnect_fixture(mode).await;
        let cancel = CancellationToken::new();
        let connection = fixture
            .mcp
            .server(None, "docs".into(), &cancel)
            .await
            .unwrap();
        assert!(connection.tools(&cancel).await.is_err());
        let failure = connection.failure().expect("a start failure is reported");
        assert!(failure.contains("exit status: 6"), "{failure}");
        assert!(
            failure.len() <= 4 * 1024,
            "the diagnostic stays within 4 KiB"
        );
        if mode == "big-stderr" {
            assert!(failure.contains('\u{fffd}'), "invalid UTF-8 is lossy");
        }
        let tail = failure.split_once('\n').map(|(_, tail)| tail).unwrap_or("");
        assert!(tail.len() <= 4 * 1024, "the tail stays within 4 KiB");
        assert!(
            tail.contains(if mode == "big-stderr" {
                "big-4-"
            } else {
                "long-line-end"
            }),
            "{tail}"
        );
        fixture.close().await;
    }
}

#[tokio::test]
async fn a_failed_http_handshake_bounds_its_diagnostic() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    let stop = CancellationToken::new();
    let app = Router::new().route(
        "/mcp",
        post(|| async {
            (
                StatusCode::BAD_REQUEST,
                format!("remote-diagnostic:{}", "界".repeat(5000)),
            )
        }),
    );
    let server = tokio::spawn(
        axum::serve(listener, app)
            .with_graceful_shutdown(stop.clone().cancelled_owned())
            .into_future(),
    );
    let fixture = Fixture::new(&url, "fixture-only").await;
    let cancel = CancellationToken::new();
    let connection = fixture
        .mcp
        .server(None, "docs".into(), &cancel)
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(5), connection.tools(&cancel))
            .await
            .expect("an explicit initialization rejection finishes")
            .is_err()
    );
    let failure = connection
        .failure()
        .expect("the failed handshake has a diagnostic");
    assert!(failure.contains("remote-diagnostic:"), "{failure}");
    assert!(
        failure.len() <= 4 * 1024,
        "the remote message is bounded: {} bytes",
        failure.len()
    );
    tokio::time::timeout(Duration::from_secs(5), fixture.close())
        .await
        .expect("the rejected connection closes");
    stop.cancel();
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .expect("the rejected connection releases its HTTP request")
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn a_chatty_stderr_is_drained_while_the_server_works() {
    let fixture = reconnect_fixture("chatty").await;
    let cancel = CancellationToken::new();
    let connection = fixture
        .mcp
        .server(None, "docs".into(), &cancel)
        .await
        .unwrap();
    let tools = tokio::time::timeout(Duration::from_secs(10), connection.tools(&cancel))
        .await
        .expect("the server answers while its stderr fills")
        .unwrap();
    assert!(tools.iter().any(|tool| tool.name == "echo"));
    assert!(
        connection
            .call(
                "echo".into(),
                json!({}).as_object().unwrap().clone(),
                &cancel
            )
            .await
            .is_ok()
    );
    wait_not_connected(&connection).await;
    let failure = connection
        .failure()
        .expect("a transport failure is reported");
    assert!(failure.contains("exit status: 8"), "{failure}");
    assert!(failure.contains("chatty-final"), "{failure}");
    fixture.close().await;
}

#[tokio::test]
async fn a_closed_stdio_connection_reconnects_on_its_next_call() {
    let fixture = reconnect_fixture("exit-after-call").await;
    let cancel = CancellationToken::new();
    let connection = fixture
        .mcp
        .server(None, "docs".into(), &cancel)
        .await
        .unwrap();
    assert!(call_echo(&connection, &cancel).await.is_ok());
    wait_not_connected(&connection).await;
    assert_ne!(connection.state(), McpServerState::Connected);
    assert!(call_echo(&connection, &cancel).await.is_ok());
    assert!(call_echo(&connection, &cancel).await.is_ok());
    fixture.close().await;
}

#[tokio::test]
async fn an_in_flight_call_fails_when_the_stdio_process_exits() {
    let fixture = reconnect_fixture("slow-exit").await;
    let cancel = CancellationToken::new();
    let connection = fixture
        .mcp
        .server(None, "docs".into(), &cancel)
        .await
        .unwrap();
    let in_flight = tokio::time::timeout(
        Duration::from_secs(5),
        connection.call(
            "slow".into(),
            json!({}).as_object().unwrap().clone(),
            &cancel,
        ),
    )
    .await
    .unwrap();
    assert!(in_flight.is_err());
    assert!(call_echo(&connection, &cancel).await.is_ok());
    fixture.close().await;
}

#[tokio::test]
async fn two_callers_after_an_exit_share_one_restart() {
    let fixture = reconnect_fixture("exit-after-call").await;
    let cancel = CancellationToken::new();
    let connection = fixture
        .mcp
        .server(None, "docs".into(), &cancel)
        .await
        .unwrap();
    assert!(call_echo(&connection, &cancel).await.is_ok());
    wait_not_connected(&connection).await;
    let (first, second) = tokio::join!(
        call_echo(&connection, &cancel),
        call_echo(&connection, &cancel)
    );
    assert!(first.is_ok());
    assert!(second.is_ok());
    assert_eq!(starts(&fixture), 2, "one restart serves both callers");
    fixture.close().await;
}

#[tokio::test]
async fn cancelling_a_restart_leaves_the_connection_able_to_start_again() {
    let fixture = reconnect_fixture("slow-restart").await;
    let cancel = CancellationToken::new();
    let connection = fixture
        .mcp
        .server(None, "docs".into(), &cancel)
        .await
        .unwrap();
    assert!(call_echo(&connection, &cancel).await.is_ok());
    wait_not_connected(&connection).await;
    let restart = cancel.child_token();
    let attempt = tokio::spawn({
        let connection = connection.clone();
        let restart = restart.clone();
        async move { call_echo(&connection, &restart).await }
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    restart.cancel();
    let cancelled = tokio::time::timeout(Duration::from_secs(8), attempt)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(cancelled, Err(McpServerError::Cancelled)));
    assert_eq!(connection.state(), McpServerState::Disconnected);
    assert!(call_echo(&connection, &cancel).await.is_ok());
    assert_eq!(starts(&fixture), 3);
    fixture.close().await;
}

#[tokio::test]
async fn a_failed_start_waits_for_its_cooldown() {
    let fixture = reconnect_fixture("always-exit").await;
    let cancel = CancellationToken::new();
    let connection = fixture
        .mcp
        .server(None, "docs".into(), &cancel)
        .await
        .unwrap();
    assert!(connection.tools(&cancel).await.is_err());
    assert_eq!(starts(&fixture), 1);
    assert_eq!(connection.state(), McpServerState::Failed);
    let retry = connection.tools(&cancel).await.unwrap_err();
    assert_eq!(
        retry.to_string(),
        "MCP server failed to start; the next attempt is in 1 s"
    );
    assert!(
        !retry.to_string().contains("startup-failure"),
        "the cooldown error carries no stderr"
    );
    assert_eq!(
        starts(&fixture),
        1,
        "a request inside the cooldown starts nothing"
    );
    tokio::time::sleep(Duration::from_millis(1200)).await;
    assert_ne!(
        connection.state(),
        McpServerState::Failed,
        "failed lasts only while the cooldown holds"
    );
    let failure = connection.failure().expect("the reason stays");
    assert!(failure.contains("startup-failure"), "{failure}");
    assert!(connection.tools(&cancel).await.is_err());
    assert_eq!(
        starts(&fixture),
        2,
        "the next attempt starts after the cooldown"
    );
    fixture.close().await;
}

#[tokio::test]
async fn a_failed_restart_replaces_the_reason_and_keeps_it_after_the_cooldown() {
    let fixture = reconnect_fixture("fail-restart").await;
    let cancel = CancellationToken::new();
    let connection = fixture
        .mcp
        .server(None, "docs".into(), &cancel)
        .await
        .unwrap();
    assert!(call_echo(&connection, &cancel).await.is_ok());
    wait_not_connected(&connection).await;
    let failure = connection.failure().expect("the exit is reported");
    assert!(failure.contains("first-exit"), "{failure}");
    assert!(failure.contains("exit status: 7"), "{failure}");
    assert!(connection.tools(&cancel).await.is_err());
    let failure = connection
        .failure()
        .expect("the restart failure is reported");
    assert!(failure.contains("restart-failure"), "{failure}");
    assert!(failure.contains("exit status: 9"), "{failure}");
    assert!(
        !failure.contains("first-exit"),
        "a new failure replaces the old reason: {failure}"
    );
    tokio::time::sleep(Duration::from_millis(1200)).await;
    assert_ne!(connection.state(), McpServerState::Failed);
    let failure = connection.failure().expect("the reason stays");
    assert!(failure.contains("restart-failure"), "{failure}");
    fixture.close().await;
}

#[tokio::test]
async fn an_expired_http_session_reconnects_and_retries_once() {
    let remote = Remote::new();
    remote.session_check.store(true, Ordering::SeqCst);
    remote.expire_once.store(true, Ordering::SeqCst);
    remote.grown.store(true, Ordering::SeqCst);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    let stop = CancellationToken::new();
    let server = tokio::spawn(
        axum::serve(
            listener,
            Router::new()
                .route("/mcp", post(rpc).get(events).delete(delete))
                .with_state(remote.clone()),
        )
        .with_graceful_shutdown(stop.clone().cancelled_owned())
        .into_future(),
    );
    let fixture = Fixture::new(&url, "fixture-only").await;
    let cancel = CancellationToken::new();
    let connection = fixture
        .mcp
        .server(None, "docs".into(), &cancel)
        .await
        .unwrap();
    let before = connection.generation();
    let result = connection
        .call(
            "expire-once".into(),
            json!({}).as_object().unwrap().clone(),
            &cancel,
        )
        .await
        .unwrap();
    let ToolContent::Blocks(content) = result.content else {
        panic!("expected blocks")
    };
    assert_eq!(
        content,
        vec![ContentBlock::Text {
            text: "expired-once".into()
        }]
    );
    assert_eq!(remote.initializes.load(Ordering::SeqCst), 2);
    assert_eq!(remote.calls.load(Ordering::SeqCst), 2);
    assert!(connection.generation() > before);
    let tools = connection.tools(&cancel).await.unwrap();
    assert!(tools.iter().any(|tool| tool.name == "grown"));
    fixture.close().await;
    stop.cancel();
    server.await.unwrap().unwrap();
}

#[tokio::test]
async fn a_second_http_404_fails_without_a_third_attempt() {
    let remote = Remote::new();
    remote.session_check.store(true, Ordering::SeqCst);
    remote.expire_always.store(true, Ordering::SeqCst);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    let stop = CancellationToken::new();
    let server = tokio::spawn(
        axum::serve(
            listener,
            Router::new()
                .route("/mcp", post(rpc).get(events).delete(delete))
                .with_state(remote.clone()),
        )
        .with_graceful_shutdown(stop.clone().cancelled_owned())
        .into_future(),
    );
    let fixture = Fixture::new(&url, "fixture-only").await;
    let cancel = CancellationToken::new();
    let connection = fixture
        .mcp
        .server(None, "docs".into(), &cancel)
        .await
        .unwrap();
    connection.tools(&cancel).await.unwrap();
    let before = connection.generation();
    assert!(matches!(
        connection
            .call(
                "expire-always".into(),
                json!({}).as_object().unwrap().clone(),
                &cancel
            )
            .await,
        Err(McpServerError::SessionExpired)
    ));
    assert_eq!(remote.initializes.load(Ordering::SeqCst), 2);
    assert_eq!(remote.calls.load(Ordering::SeqCst), 2);
    assert_eq!(connection.generation(), before + 1);
    fixture.close().await;
    stop.cancel();
    server.await.unwrap().unwrap();
}

#[tokio::test]
async fn a_reconnect_reaps_the_previous_stdio_process() {
    let fixture = reconnect_fixture("close-stdout").await;
    let cancel = CancellationToken::new();
    let connection = fixture
        .mcp
        .server(None, "docs".into(), &cancel)
        .await
        .unwrap();
    assert!(call_echo(&connection, &cancel).await.is_ok());
    wait_not_connected(&connection).await;
    let pids = read_pids(&fixture);
    let old = pids
        .last()
        .expect("the first process wrote its pid")
        .clone();
    assert!(call_echo(&connection, &cancel).await.is_ok());
    let pids = read_pids(&fixture);
    let new = pids
        .last()
        .expect("the second process wrote its pid")
        .clone();
    assert_ne!(old, new);
    wait_pid_gone(&old).await;
    assert!(pid_alive(&new));
    fixture.close().await;
}

async fn call_echo(
    connection: &dim_app_server::mcp::McpConnection,
    cancel: &CancellationToken,
) -> dim_app_server::mcp::server::Result<dim_agent_types::ToolResult> {
    connection
        .call(
            "echo".into(),
            json!({}).as_object().unwrap().clone(),
            cancel,
        )
        .await
}

fn read_pids(fixture: &Fixture) -> Vec<String> {
    fs::read_to_string(fixture.home.join("pids"))
        .map(|text| text.lines().map(str::to_owned).collect())
        .unwrap_or_default()
}

fn pid_alive(pid: &str) -> bool {
    std::process::Command::new("node")
        .args([
            "-e",
            "try { process.kill(Number(process.argv[1]), 0); process.exit(0); } catch(e) { process.exit(e.code === 'ESRCH' ? 1 : 2); }",
            pid,
        ])
        .status()
        .unwrap()
        .success()
}

async fn wait_pid_gone(pid: &str) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while pid_alive(pid) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the previous child is gone");
}

async fn wait_file(path: &std::path::Path) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while !path.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

async fn assert_child_reaped(fixture: &Fixture) {
    let pid = fs::read_to_string(fixture.home.join("pid")).unwrap();
    let status = tokio::process::Command::new("node").args([
        "-e", "try { process.kill(Number(process.argv[1]), 0); process.exit(1); } catch(e) { process.exit(e.code === 'ESRCH' ? 0 : 2); }", &pid,
    ]).status().await.unwrap();
    assert!(
        status.success(),
        "MCP child must be gone when close completes"
    );
}

#[tokio::test]
#[ignore = "Requires npm ci --prefix runtime --ignore-scripts and Node"]
async fn node_stdio_sdk_discovery_call_cancel_and_process_cleanup() {
    for mode in ["normal", "stubborn"] {
        let fixture = stdio_fixture(mode).await;
        let cancel = CancellationToken::new();
        let mut expected_cwd = fs::canonicalize(&fixture.home).unwrap();
        let project = if mode == "stubborn" {
            let workspace = expected_cwd.join("later-project");
            expected_cwd = workspace.join("relative-cwd");
            fs::create_dir_all(&expected_cwd).unwrap();
            let mut settings: Value =
                serde_json::from_slice(&fs::read(fixture.home.join("mcp.json")).unwrap()).unwrap();
            settings["mcpServers"]["docs"]["cwd"] = json!("relative-cwd");
            fs::write(
                workspace.join(".mcp.json"),
                serde_json::to_vec(&settings).unwrap(),
            )
            .unwrap();
            Some(
                fixture
                    .root
                    .get::<SettingsService>()
                    .unwrap()
                    .authorize_project(workspace, &cancel)
                    .await
                    .unwrap(),
            )
        } else {
            None
        };
        if let Some(project) = project {
            assert!(
                fixture
                    .mcp
                    .server(Some(project), "docs".into(), &cancel)
                    .await
                    .is_err()
            );
            assert!(
                !fixture.home.join("pid").exists(),
                "a project entry the user has not enabled must not spawn"
            );
            let revision = fixture
                .mcp
                .catalog(Some(project), &cancel)
                .await
                .unwrap()
                .project_revision
                .unwrap();
            fixture
                .mcp
                .update(
                    SettingsScope::Project(project),
                    revision,
                    "docs".into(),
                    McpChange::SetEnabled(true),
                    &cancel,
                )
                .await
                .unwrap();
        }
        let connection = fixture
            .mcp
            .server(project, "docs".into(), &cancel)
            .await
            .unwrap();
        assert!(!fixture.home.join("pid").exists(), "apply must not spawn");
        let tools = connection.tools(&cancel).await.unwrap();
        assert_eq!(
            tools.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
            ["inspect", "slow"]
        );
        let result = connection
            .call(
                "inspect".into(),
                json!({"a":7,"b":3}).as_object().unwrap().clone(),
                &cancel,
            )
            .await
            .unwrap();
        let ToolContent::Blocks(blocks) = result.content else {
            panic!("blocks")
        };
        let ContentBlock::Text { text } = &blocks[0] else {
            panic!("text")
        };
        let value: Value = serde_json::from_str(text).unwrap();
        assert_eq!(value["value"], 703);
        assert_eq!(value["env"], "configured value");
        assert_eq!(value["argument"], "$(not-a-shell); literal argument");
        assert_eq!(PathBuf::from(value["cwd"].as_str().unwrap()), expected_cwd);
        let task = {
            let connection = connection.clone();
            let cancel = cancel.clone();
            tokio::spawn(async move {
                connection
                    .call("slow".into(), Default::default(), &cancel)
                    .await
            })
        };
        wait_file(&fixture.home.join("pid.called")).await;
        if let Some(project) = project {
            fixture
                .mcp
                .disconnect(SettingsScope::Project(project), "docs".into(), &cancel)
                .await
                .unwrap();
            assert_eq!(connection.state(), McpServerState::Closed);
            assert_child_reaped(&fixture).await;
        } else {
            cancel.cancel();
        }
        assert!(matches!(
            task.await.unwrap(),
            Err(McpServerError::Cancelled)
        ));
        wait_file(&fixture.home.join("pid.cancelled")).await;
        tokio::time::timeout(Duration::from_secs(10), fixture.close())
            .await
            .unwrap();
        assert_eq!(connection.state(), McpServerState::Closed);
        assert_child_reaped(&fixture).await;
    }
}

#[tokio::test]
#[ignore = "Requires npm ci --prefix runtime --ignore-scripts and Node"]
async fn node_stdio_failed_or_cancelled_handshake_reaps_process_before_returning() {
    for mode in ["silent", "invalid"] {
        let fixture = stdio_fixture(mode).await;
        let cancel = CancellationToken::new();
        let connection = fixture
            .mcp
            .server(None, "docs".into(), &cancel)
            .await
            .unwrap();
        let task = {
            let connection = connection.clone();
            let cancel = cancel.clone();
            tokio::spawn(async move { connection.tools(&cancel).await })
        };
        wait_file(&fixture.home.join("pid")).await;
        if mode == "silent" {
            cancel.cancel();
        }
        let result = tokio::time::timeout(Duration::from_secs(10), task)
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            (mode, result),
            ("silent", Err(McpServerError::Cancelled))
                | ("invalid", Err(McpServerError::RequestFailed))
        ));
        assert!(matches!(
            (mode, connection.state()),
            ("silent", McpServerState::Disconnected)
                | (
                    "invalid",
                    McpServerState::Failed | McpServerState::Disconnected
                )
        ));
        assert_child_reaped(&fixture).await;
        fixture.close().await;
    }
}

fn sse_configuration(url: &str) -> Value {
    json!({"type":"sse", "url":url, "auth":{"type":"bearer","token":"fixture-only"}, "headers":{"X-Fixture":"custom-header"}})
}

#[tokio::test]
async fn legacy_sse_accepts_default_message_events() {
    use futures_util::StreamExt;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/sse", listener.local_addr().unwrap());
    let stop = CancellationToken::new();
    let (messages, _) = broadcast::channel::<String>(8);
    let app = Router::new().route("/sse", axum::routing::get({
        let messages = messages.clone(); let stop = stop.clone();
        move || { let rx = messages.subscribe(); let stop = stop.clone(); async move {
            let endpoint = futures_util::stream::once(async { Ok::<_, Infallible>(Event::default().event("endpoint").data("/post?sessionId=fixture")) });
            let replies = futures_util::stream::unfold(rx, |mut rx| async move {
                let data = rx.recv().await.ok()?;
                Some((Ok::<_, Infallible>(Event::default().data(data)), rx))
            });
            Sse::new(endpoint.chain(replies).take_until(stop.cancelled_owned()))
        }}
    })).route("/post", post(move |Json(message): Json<Value>| {
        let messages = messages.clone(); async move {
            let result = match message["method"].as_str().unwrap() {
                "initialize" => json!({"protocolVersion":message["params"]["protocolVersion"],"capabilities":{"tools":{}},"serverInfo":{"name":"default-event-fixture","version":"1"}}),
                "notifications/initialized" => return StatusCode::ACCEPTED,
                "tools/list" => json!({"tools":[{"name":"subtract","inputSchema":{"type":"object","properties":{"a":{"type":"integer"},"b":{"type":"integer"}},"required":["a","b"]}}]}),
                "tools/call" => {
                    let args = &message["params"]["arguments"];
                    json!({"content":[{"type":"text","text":(args["a"].as_i64().unwrap() - args["b"].as_i64().unwrap()).to_string()}]})
                }
                _ => return StatusCode::BAD_REQUEST,
            };
            messages.send(json!({"jsonrpc":"2.0","id":message["id"],"result":result}).to_string()).unwrap();
            StatusCode::ACCEPTED
        }
    }));
    let server = tokio::spawn(
        axum::serve(listener, app)
            .with_graceful_shutdown(stop.clone().cancelled_owned())
            .into_future(),
    );
    let fixture = Fixture::configured(sse_configuration(&url)).await;
    let cancel = CancellationToken::new();
    let connection = fixture
        .mcp
        .server(None, "docs".into(), &cancel)
        .await
        .unwrap();
    let tools = tokio::time::timeout(Duration::from_secs(3), connection.tools(&cancel)).await;
    if tools.is_err() {
        cancel.cancel();
        fixture.close().await;
    }
    assert_eq!(tools.unwrap().unwrap()[0].name, "subtract");
    let result = connection
        .call(
            "subtract".into(),
            json!({"a":7,"b":3}).as_object().unwrap().clone(),
            &cancel,
        )
        .await
        .unwrap();
    let ToolContent::Blocks(blocks) = result.content else {
        panic!("blocks")
    };
    assert!(matches!(&blocks[0], ContentBlock::Text { text } if text == "4"));
    fixture.close().await;
    stop.cancel();
    server.await.unwrap().unwrap();
}

#[tokio::test]
async fn the_mcp_client_declares_the_build_the_process_reports() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    let stop = CancellationToken::new();
    let client_info: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
    let seen = client_info.clone();
    let app = Router::new().route(
        "/mcp",
        post(move |Json(message): Json<Value>| {
            let seen = seen.clone();
            async move {
                let result = match message["method"].as_str().unwrap() {
                    "initialize" => {
                        seen.lock().unwrap().push(message["params"]["clientInfo"].clone());
                        json!({"protocolVersion":message["params"]["protocolVersion"],"capabilities":{"tools":{}},"serverInfo":{"name":"identity-fixture","version":"1"}})
                    }
                    "notifications/initialized" => return StatusCode::ACCEPTED.into_response(),
                    "tools/list" => json!({"tools":[]}),
                    _ => return StatusCode::BAD_REQUEST.into_response(),
                };
                Json(json!({"jsonrpc":"2.0","id":message["id"],"result":result})).into_response()
            }
        }),
    );
    let server = tokio::spawn(
        axum::serve(listener, app)
            .with_graceful_shutdown(stop.clone().cancelled_owned())
            .into_future(),
    );
    let fixture = Fixture::new(&url, "fixture-only").await;
    let cancel = CancellationToken::new();
    let connection = fixture
        .mcp
        .server(None, "docs".into(), &cancel)
        .await
        .unwrap();
    assert!(connection.tools(&cancel).await.unwrap().is_empty());
    let received = client_info.lock().unwrap().clone();
    assert_eq!(received.len(), 1, "the connection initialized once");
    assert_eq!(
        received[0]["version"],
        json!(dim_agent_capabilities::build::current()),
        "the MCP client declares the label its own process reports"
    );
    if !dim_agent_capabilities::build::release() {
        assert_ne!(
            received[0]["version"],
            json!(env!("CARGO_PKG_VERSION")),
            "a build the release flow did not stamp does not declare the bare version"
        );
    }
    assert_eq!(
        received[0]["name"],
        json!("dim-app-server"),
        "the declared name stays the package name"
    );
    fixture.close().await;
    stop.cancel();
    server.await.unwrap().unwrap();
}

#[tokio::test]
async fn legacy_sse_rejects_cross_origin_endpoints_and_redirects_without_leaking_headers() {
    use axum::routing::get;
    let hits = Arc::new(AtomicUsize::new(0));
    let trap = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let target = format!("http://{}/capture", trap.local_addr().unwrap());
    let stop = CancellationToken::new();
    let capture = {
        let hits = hits.clone();
        move || async move {
            hits.fetch_add(1, Ordering::SeqCst);
            StatusCode::OK
        }
    };
    let trap_task = tokio::spawn(
        axum::serve(
            trap,
            Router::new().route("/capture", get(capture.clone()).post(capture.clone())),
        )
        .with_graceful_shutdown(stop.clone().cancelled_owned())
        .into_future(),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(
        axum::serve(
            listener,
            Router::new()
                .route(
                    "/sse/{case}",
                    get(
                        move |axum::extract::Path(case): axum::extract::Path<String>| {
                            let target = target.clone();
                            async move {
                                let endpoint = match case.as_str() {
                                    "cross" => target,
                                    "userinfo" => format!("http://user@{addr}/capture"),
                                    "fragment" => "/capture#secret".into(),
                                    "empty" => " ".into(),
                                    "redirect" => {
                                        return axum::response::Redirect::temporary(&target)
                                            .into_response();
                                    }
                                    _ => unreachable!(),
                                };
                                (
                                    [("content-type", "text/event-stream")],
                                    format!("event: endpoint\ndata: {endpoint}\n\n"),
                                )
                                    .into_response()
                            }
                        },
                    ),
                )
                .route("/capture", get(capture.clone()).post(capture)),
        )
        .with_graceful_shutdown(stop.clone().cancelled_owned())
        .into_future(),
    );
    for case in ["cross", "userinfo", "fragment", "empty", "redirect"] {
        let fixture =
            Fixture::configured(sse_configuration(&format!("http://{addr}/sse/{case}"))).await;
        let cancel = CancellationToken::new();
        let connection = fixture
            .mcp
            .server(None, "docs".into(), &cancel)
            .await
            .unwrap();
        assert!(
            tokio::time::timeout(Duration::from_secs(5), connection.tools(&cancel))
                .await
                .unwrap()
                .is_err(),
            "{case}"
        );
        assert_eq!(connection.state(), McpServerState::Failed);
        fixture.close().await;
    }
    assert_eq!(hits.load(Ordering::SeqCst), 0);
    stop.cancel();
    server.await.unwrap().unwrap();
    trap_task.await.unwrap().unwrap();
}

#[tokio::test]
async fn legacy_sse_cancels_get_endpoint_discovery_and_initialize_post() {
    use futures_util::StreamExt;
    for phase in ["headers", "endpoint", "initialize"] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/sse", listener.local_addr().unwrap());
        let ready = Arc::new(Notify::new());
        let stop = CancellationToken::new();
        let app = Router::new()
            .route(
                "/sse",
                axum::routing::get({
                    let ready = ready.clone();
                    let stop = stop.clone();
                    move || {
                        let ready = ready.clone();
                        let stop = stop.clone();
                        async move {
                            if phase == "headers" {
                                ready.notify_one();
                                stop.cancelled().await;
                                return StatusCode::SERVICE_UNAVAILABLE.into_response();
                            }
                            if phase == "endpoint" {
                                ready.notify_one();
                            }
                            let event = if phase == "endpoint" {
                                Event::default().comment("waiting")
                            } else {
                                Event::default().event("endpoint").data("/post")
                            };
                            Sse::new(
                                futures_util::stream::once(
                                    async move { Ok::<_, Infallible>(event) },
                                )
                                .chain(futures_util::stream::pending())
                                .take_until(stop.cancelled_owned()),
                            )
                            .into_response()
                        }
                    }
                }),
            )
            .route(
                "/post",
                post({
                    let ready = ready.clone();
                    let stop = stop.clone();
                    move || {
                        let ready = ready.clone();
                        let stop = stop.clone();
                        async move {
                            ready.notify_one();
                            stop.cancelled().await;
                            StatusCode::SERVICE_UNAVAILABLE
                        }
                    }
                }),
            );
        let server = tokio::spawn(
            axum::serve(listener, app)
                .with_graceful_shutdown(stop.clone().cancelled_owned())
                .into_future(),
        );
        let fixture = Fixture::configured(sse_configuration(&url)).await;
        let cancel = CancellationToken::new();
        let connection = fixture
            .mcp
            .server(None, "docs".into(), &cancel)
            .await
            .unwrap();
        let task = {
            let connection = connection.clone();
            let cancel = cancel.clone();
            tokio::spawn(async move { connection.tools(&cancel).await })
        };
        tokio::time::timeout(Duration::from_secs(5), ready.notified())
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), fixture.close())
            .await
            .unwrap();
        assert!(
            matches!(task.await.unwrap(), Err(McpServerError::Cancelled)),
            "{phase}"
        );
        assert_eq!(connection.state(), McpServerState::Closed);
        stop.cancel();
        tokio::time::timeout(Duration::from_secs(5), server)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }
}

#[tokio::test]
#[ignore = "Requires npm ci --prefix runtime --ignore-scripts and Node"]
async fn node_legacy_sse_sdk_discovery_call_notifications_cancel_close_and_no_replay() {
    use tokio::io::AsyncBufReadExt;
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mcp-sse.cjs");
    let mut child = tokio::process::Command::new("node")
        .arg(script)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut lines = tokio::io::BufReader::new(child.stdout.take().unwrap()).lines();
    let base = tokio::time::timeout(Duration::from_secs(10), lines.next_line())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let client = reqwest::Client::new();
    for drop_remote in [false, true] {
        let fixture = Fixture::configured(sse_configuration(&format!("{base}/sse"))).await;
        let cancel = CancellationToken::new();
        let connection = fixture
            .mcp
            .server(None, "docs".into(), &cancel)
            .await
            .unwrap();
        let mut changes = connection.tool_changes();
        let tools = connection.tools(&cancel).await.unwrap();
        assert_eq!(
            tools.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
            ["compute", "slow", "drop"]
        );
        let result = connection
            .call(
                "compute".into(),
                json!({"a":7,"b":3}).as_object().unwrap().clone(),
                &cancel,
            )
            .await
            .unwrap();
        assert_ne!(result.is_error, Some(true));
        assert_eq!(
            result.structured_content.as_ref().unwrap()["cursor"],
            "page-7"
        );
        let ToolContent::Blocks(blocks) = result.content else {
            panic!("blocks")
        };
        assert!(
            blocks
                .iter()
                .any(|b| matches!(b, ContentBlock::Text { text } if text == "703"))
        );
        assert!(
            blocks
                .iter()
                .any(|b| matches!(b, ContentBlock::Text { text } if text.contains("page-7")))
        );
        tokio::time::timeout(Duration::from_secs(5), changes.changed())
            .await
            .unwrap()
            .unwrap();
        if drop_remote {
            assert!(
                connection
                    .call("drop".into(), Default::default(), &cancel)
                    .await
                    .is_err()
            );
            let tools = connection.tools(&cancel).await.unwrap();
            assert_eq!(
                tools
                    .iter()
                    .map(|tool| tool.name.as_str())
                    .collect::<Vec<_>>(),
                ["compute", "slow", "drop"]
            );
        } else {
            let task = {
                let connection = connection.clone();
                let cancel = cancel.clone();
                tokio::spawn(async move {
                    connection
                        .call("slow".into(), Default::default(), &cancel)
                        .await
                })
            };
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    let stats: Value = client
                        .get(format!("{base}/stats"))
                        .send()
                        .await
                        .unwrap()
                        .json()
                        .await
                        .unwrap();
                    if stats["calls"] == 2 {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
            cancel.cancel();
            assert!(matches!(
                task.await.unwrap(),
                Err(McpServerError::Cancelled)
            ));
        }
        fixture.close().await;
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let stats: Value = client
                    .get(format!("{base}/stats"))
                    .send()
                    .await
                    .unwrap()
                    .json()
                    .await
                    .unwrap();
                if stats["active"] == 0 {
                    assert_eq!(stats["cancelled"], 1);
                    assert_eq!(stats["calls"], if drop_remote { 4 } else { 2 });
                    assert_eq!(stats["gets"], if drop_remote { 3 } else { 1 });
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
    }
    child.stdin.take();
    assert!(
        tokio::time::timeout(Duration::from_secs(5), child.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
}

/// The same fixture with the widget tools, app-only tool, model-only tool and
/// the `ui://` document the specification describes.
async fn stdio_widget_fixture() -> Fixture {
    let fixture = Fixture::configured(json!({"command":"node"})).await;
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mcp-stdio.cjs");
    fs::write(
        fixture.home.join("mcp.json"),
        serde_json::to_vec(&json!({"mcpServers":{"docs":{
            "command":"node", "args":[script, "normal"],
            "cwd":fixture.home, "env":{"MCP_PID_FILE":fixture.home.join("pid"), "MCP_WIDGETS":"1"}
        }}}))
        .unwrap(),
    )
    .unwrap();
    fixture
}

#[tokio::test]
#[ignore = "Requires npm ci --prefix runtime --ignore-scripts and Node"]
async fn node_stdio_sdk_serves_widget_metadata_and_its_document() {
    use dim_app_server::mcp::server::{McpTool, ToolScope};

    let fixture = stdio_widget_fixture().await;
    let cancel = CancellationToken::new();
    let connection = fixture
        .mcp
        .server(None, "docs".into(), &cancel)
        .await
        .unwrap();
    let tools = connection.tools(&cancel).await.unwrap();
    let tool = |name: &str| -> McpTool {
        tools
            .iter()
            .find(|tool| tool.name == name)
            .unwrap_or_else(|| panic!("{name} was not discovered"))
            .clone()
    };
    let chart = tool("chart");
    assert_eq!(
        chart.ui.as_ref().and_then(|ui| ui.resource_uri.as_deref()),
        Some("ui://widget/counter.html"),
        "a discovered tool keeps the widget association it declares"
    );
    let refresh = tool("refresh").ui.unwrap();
    assert!(
        !refresh.allows(ToolScope::Model) && refresh.allows(ToolScope::App),
        "an app-only tool is not published to the model"
    );
    let summary = tool("summary").ui.unwrap();
    assert!(
        summary.allows(ToolScope::Model) && !summary.allows(ToolScope::App),
        "a model-only tool is not callable from a view"
    );
    assert_eq!(
        connection
            .discovered()
            .unwrap()
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>(),
        ["inspect", "slow", "chart", "refresh", "summary"]
    );

    let document = connection
        .resource("ui://widget/counter.html".into(), &cancel)
        .await
        .unwrap();
    assert_eq!(
        document.mime_type.as_deref(),
        Some("text/html;profile=mcp-app")
    );
    assert!(
        document
            .text
            .as_deref()
            .is_some_and(|text| text.contains("<p>counter</p>"))
    );
    assert!(
        document
            .meta
            .as_ref()
            .is_some_and(|meta| meta["ui"]["prefersBorder"] == json!(true)),
        "the document's `_meta.ui` reaches the host: {:?}",
        document.meta
    );
    let call = connection
        .call_result(
            "chart".into(),
            json!({"step":2}).as_object().cloned().unwrap(),
            &cancel,
        )
        .await
        .unwrap();
    assert!(
        matches!(&call.content[..], [ContentBlock::Text { text }] if text == "chart 2"),
        "{:?}",
        call.content
    );
    assert_eq!(
        call.structured_content,
        json!({"step":2}).as_object().cloned()
    );
    assert_eq!(
        call.meta
            .as_ref()
            .and_then(|meta| meta.get("widget"))
            .and_then(Value::as_str),
        Some("chart"),
        "a call keeps the result's `_meta` for the view"
    );
    fixture.close().await;
}

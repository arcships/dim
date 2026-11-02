//! Public Session flows through MCP, ToolBinding, Approval and ToolExecutor.
use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response, Sse, sse::Event},
    routing::post,
};
use dim_agent_capabilities::{
    adapters::{DefaultPrepareModelPlugin, MockTools},
    defaults,
    history::HistoryRenderer,
    host::*,
    tool_registry::ToolSourcesView,
};
use dim_agent_runtime::{testing::MinimalHost, *};
use dim_agent_types::*;
use dim_app_server::{
    mcp::{
        self, McpAppError, McpChange, McpPlugin, McpPublication, McpService, McpSession,
        McpSourceState,
    },
    settings::{SettingsPlugin, SettingsScope, SettingsService},
};
use rutis::Ctx;
use rutis::host::{BoxFuture, DynPlugin, Effect, Key, TypeKey};
use rutis_loader::resolve::Build;
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    convert::Infallible,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::sync::{Notify, broadcast};
use tokio_util::sync::CancellationToken;

struct Remote {
    calls: Mutex<Vec<String>>,
    initializes: AtomicUsize,
    deletes: AtomicUsize,
    invalid: AtomicBool,
    listing: Notify,
    hold: AtomicBool,
    release: Notify,
    revision: AtomicUsize,
    events: broadcast::Sender<String>,
    images: AtomicBool,
    nullable: AtomicBool,
    broken: AtomicBool,
    /// 0 none, 1 the declared document, 2 another media type, 3 over the
    /// document limit.
    widgets: AtomicUsize,
    /// Holds a view's call after the server received it, until the request
    /// ends.
    app_hold: AtomicBool,
    app_called: Notify,
}
impl Remote {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            calls: Mutex::default(),
            initializes: AtomicUsize::new(0),
            deletes: AtomicUsize::new(0),
            invalid: AtomicBool::new(false),
            listing: Notify::new(),
            hold: AtomicBool::new(false),
            release: Notify::new(),
            revision: AtomicUsize::new(0),
            events: broadcast::channel(16).0,
            images: AtomicBool::new(false),
            nullable: AtomicBool::new(false),
            broken: AtomicBool::new(false),
            widgets: AtomicUsize::new(0),
            app_hold: AtomicBool::new(false),
            app_called: Notify::new(),
        })
    }
}
async fn rpc(
    State(remote): State<Arc<Remote>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let Some(id) = body.get("id") else {
        return StatusCode::ACCEPTED.into_response();
    };
    let result = match body["method"].as_str().unwrap() {
        "initialize" => {
            remote.initializes.fetch_add(1, Ordering::SeqCst);
            json!({"protocolVersion":"2025-03-26","capabilities":{"tools":{"listChanged":true}},"serverInfo":{"name":"session-test","version":"1"}})
        }
        "tools/list" => {
            if remote.hold.load(Ordering::SeqCst)
                && headers.get("x-generation").is_some_and(|h| h == "slow")
            {
                remote.listing.notify_one();
                remote.release.notified().await;
            }
            if remote.invalid.load(Ordering::SeqCst) {
                return StatusCode::SERVICE_UNAVAILABLE.into_response();
            }
            let mut tools = vec![
                json!({"name":"lookup/item","description":format!("Subtract two numbers, revision {}", remote.revision.load(Ordering::SeqCst)),"inputSchema":{"type":"object","properties":{"a":{"type":"integer"},"b":{"type":"integer"},"rule":{"oneOf":[{"type":"object","properties":{"kind":{"type":"string","const":"manual"}},"required":["kind"],"additionalProperties":false},{"type":"object","properties":{"kind":{"type":"string","const":"minutes"},"every":{"type":"integer","minimum":1}},"required":["kind","every"],"additionalProperties":false}]}},"required":["a","b"]}}),
                json!({"name":"lookup_item","description":"A distinct original name","inputSchema":{"type":"object"}}),
            ];
            if remote.images.load(Ordering::SeqCst) {
                tools.push(json!({"name":"screenshot","description":"Capture the screen and return the PNG.","inputSchema":{"type":"object","properties":{},"additionalProperties":false}}));
            }
            if remote.nullable.load(Ordering::SeqCst) {
                tools.push(json!({"name":"spaces","description":"List spaces","inputSchema":{"type":"object","properties":{"space_id":{"type":["string","null"]}},"required":["space_id"]}}));
            }
            if remote.broken.load(Ordering::SeqCst) {
                tools.push(json!({"name":"broken","inputSchema":{"type":"object","properties":{"x":{"type":["string","file"]}}}}));
            }
            if remote.widgets.load(Ordering::SeqCst) != 0 {
                tools.push(json!({"name":"chart","description":"Render a counter","inputSchema":{"type":"object","properties":{"step":{"type":"integer"}}},"_meta":{"ui":{"resourceUri":"ui://widget/counter.html"}}}));
                tools.push(json!({"name":"refresh","description":"Refresh the view","inputSchema":{"type":"object"},"_meta":{"ui":{"visibility":["app"]}}}));
                tools.push(json!({"name":"summary","description":"Summarize","inputSchema":{"type":"object"},"_meta":{"ui":{"visibility":["model"]}}}));
            }
            json!({"tools":tools})
        }
        "resources/read" => {
            let uri = body["params"]["uri"].as_str().unwrap();
            let ui = json!({"csp":{"connectDomains":["https://api.example"]},"prefersBorder":true});
            match remote.widgets.load(Ordering::SeqCst) {
                2 => {
                    json!({"contents":[{"uri":uri,"mimeType":"text/html","text":"<p>counter</p>"}]})
                }
                3 => {
                    json!({"contents":[{"uri":uri,"mimeType":"text/html;profile=mcp-app","text":"x".repeat(4*1024*1024+1)}]})
                }
                _ => {
                    json!({"contents":[{"uri":uri,"mimeType":"text/html;profile=mcp-app","text":"<p>counter</p>","_meta":{"ui":ui}}]})
                }
            }
        }
        "tools/call" => {
            remote
                .calls
                .lock()
                .unwrap()
                .push(body["params"]["name"].as_str().unwrap().into());
            if body["params"]["name"] == "chart" {
                if remote.app_hold.load(Ordering::SeqCst) {
                    remote.app_called.notify_waiters();
                    std::future::pending::<()>().await;
                }
                json!({"content":[{"type":"text","text":"chart-ok"}],"structuredContent":{"step":2},"_meta":{"widget":"chart"}})
            } else if body["params"]["name"] == "refresh" {
                json!({"isError":true,"content":[{"type":"text","text":"refresh failed"}]})
            } else if body["params"]["name"] == "summary" {
                json!({"content":[{"type":"text","text":"summary-ok"}]})
            } else if body["params"]["name"] == "spaces" {
                json!({"content":[{"type":"text","text":"spaces-ok"}]})
            } else if body["params"]["name"] == "screenshot" {
                json!({"content":[
                    {"type":"text","text":"screenshot captured"},
                    {"type":"image","data":"iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jD1sAAAAASUVORK5CYII=","mimeType":"image/png"}
                ]})
            } else {
                let a = body["params"]["arguments"]["a"].as_i64().unwrap();
                let b = body["params"]["arguments"]["b"].as_i64().unwrap();
                let generation = headers.get("x-generation").unwrap().to_str().unwrap();
                json!({"content":[{"type":"text","text":format!("{generation}:{}", a-b)}]})
            }
        }
        _ => panic!("unexpected method: {body}"),
    };
    let mut response = Json(json!({"jsonrpc":"2.0","id":id,"result":result})).into_response();
    response
        .headers_mut()
        .insert("mcp-session-id", "fixture".parse().unwrap());
    response
}
async fn delete(State(remote): State<Arc<Remote>>) -> StatusCode {
    remote.deletes.fetch_add(1, Ordering::SeqCst);
    StatusCode::NO_CONTENT
}
async fn events(State(remote): State<Arc<Remote>>) -> impl IntoResponse {
    Sse::new(futures_util::stream::unfold(
        remote.events.subscribe(),
        |mut rx| async move {
            rx.recv()
                .await
                .ok()
                .map(|data| (Ok::<_, Infallible>(Event::default().data(data)), rx))
        },
    ))
}

enum Action {
    Call {
        pause: bool,
        name: Option<String>,
    },
    Text,
    /// Ignores cancellation until released, so a Session close waits for it.
    Hold,
}
#[derive(Default)]
struct Model {
    actions: Mutex<VecDeque<Action>>,
    requests: Mutex<Vec<Arc<ModelRequest>>>,
    entered: Notify,
    release: Notify,
}
impl ModelDriver for Model {
    fn invoke(
        &self,
        request: Arc<ModelRequest>,
        _: ModelConnection,
        cancel: CancellationToken,
    ) -> HostFuture<'_, ModelStream> {
        let action = self
            .actions
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(Action::Text);
        self.requests.lock().unwrap().push(request.clone());
        Box::pin(async move {
            let (chunks, reason) = match action {
                Action::Call { pause, name } => {
                    let name = name.unwrap_or_else(|| {
                        request
                            .tools
                            .as_ref()
                            .unwrap()
                            .iter()
                            .find(|t| {
                                t.metadata
                                    .as_ref()
                                    .and_then(|m| m.mcp_original_tool_name.as_deref())
                                    == Some("lookup/item")
                            })
                            .unwrap()
                            .name
                            .clone()
                    });
                    if pause {
                        self.entered.notify_one();
                        tokio::select! { _ = cancel.cancelled() => return Err(HostError::Cancelled("test".into())), _ = self.release.notified() => {} }
                    }
                    let id = uuid::Uuid::new_v4().to_string();
                    (
                        vec![
                            ModelChunk::ToolStart {
                                id: id.clone(),
                                name: name.clone(),
                            },
                            ModelChunk::ToolUse(ToolCall {
                                id,
                                name,
                                input: json!({"a":17,"b":9}).as_object().unwrap().clone(),
                            }),
                        ],
                        ModelStopReason::ToolUse,
                    )
                }
                Action::Text => (
                    vec![ModelChunk::Text("done".into()), ModelChunk::TextEnd],
                    ModelStopReason::EndTurn,
                ),
                Action::Hold => {
                    self.entered.notify_one();
                    self.release.notified().await;
                    (
                        vec![ModelChunk::Text("done".into()), ModelChunk::TextEnd],
                        ModelStopReason::EndTurn,
                    )
                }
            };
            let mut stream = vec![Ok(ModelChunk::MessageStart {
                message_id: "message".into(),
            })];
            stream.extend(chunks.into_iter().map(Ok));
            stream.push(Ok(ModelChunk::MessageStop {
                message_id: "message".into(),
                reason,
            }));
            Ok(Box::pin(futures_util::stream::iter(stream)) as ModelStream)
        })
    }
}

struct Env {
    home: PathBuf,
    agent: MinimalHost,
    model: Arc<Model>,
    directory: Arc<McpService>,
    remote: Arc<Remote>,
    url: String,
    stop: CancellationToken,
    server: tokio::task::JoinHandle<std::io::Result<()>>,
}
/// The host plugins MCP Sessions need, as process rows of the minimal host:
/// management methods, the MCP publication, FileStorage with `home` as root
/// 0, Settings and Mcp.
fn mcp_host_rows(builder: &mut AgentBuilder, home: &Path) {
    builder.plugin(|names| {
        Ok(Box::new(
            dim_app_server::methods::ManagementMethodsPlugin::new(names)?,
        ))
    });
    builder.plugin(|names| Ok(Box::new(McpPublication::new(names)?)));
    let files = dim_agent_capabilities::file_storage::FileStoragePlugin::new(vec![home.into()]);
    builder.plugin(move |_| Ok(Box::new(files.clone())));
    let settings = {
        let catalog = dim_app_server::catalog();
        let build = Build::default();
        SettingsPlugin::new(&Names::copy(&catalog, &build), 0, vec![]).unwrap()
    };
    builder.plugin(move |_| Ok(Box::new(settings.clone())));
    builder.plugin(|names| Ok(Box::new(McpPlugin::new(names, 0)?)));
}
/// Authorizes `home`'s project in the host's Settings.
async fn authorize_project(host: &MinimalHost, home: &Path) {
    host.root()
        .get::<SettingsService>()
        .unwrap()
        .authorize_project(home.join("project"), &CancellationToken::new())
        .await
        .unwrap();
}
impl Env {
    async fn new() -> Self {
        Self::with(|_, _| {}).await
    }
    /// `configure` adds Session capabilities after the MCP Session part.
    async fn with(configure: impl FnOnce(&mut AgentBuilder, &Arc<Remote>)) -> Self {
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
        let home = std::env::temp_dir().join(format!("dim-mcp-session-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(home.join("project")).unwrap();
        let home = home.canonicalize().unwrap();
        std::fs::write(
            home.join("mcp.json"),
            json!({"mcpServers":{"docs":configuration(&url, "one")}}).to_string(),
        )
        .unwrap();
        let model = Arc::new(Model::default());
        let mut builder = AgentBuilder::new(Default::default());
        builder
            .session_value(Arc::new(HistoryRenderer::default()))
            .process_value(Arc::new(ModelHost(model.clone())))
            .session_value(Arc::new(PrepareContextHost(Arc::new(IdentityContext))))
            .session_value(Arc::new(ContextModelHost(None)))
            .session_value(Arc::new(CompactionStrategyHost(None)))
            .require_feature(
                Feature::new("PrepareModelHost").session("PrepareModelHost", |names| {
                    Ok(Box::new(DefaultPrepareModelPlugin::new(names)?))
                }),
            );
        defaults::tool_host(&mut builder, Arc::new(MockTools));
        defaults::install_with_prompt(
            &mut builder,
            Arc::new(EmptyPromptHost),
            Some(ConfigureUpdate {
                sources: vec![],
                model: Some(SessionConfig::default().model),
            }),
        );
        mcp::install(&mut builder);
        mcp_host_rows(&mut builder, &home);
        configure(&mut builder, &remote);
        let agent = MinimalHost::start(&builder).await.unwrap();
        authorize_project(&agent, &home).await;
        let directory = agent.root().get::<McpService>().unwrap();
        Self {
            home,
            agent,
            model,
            directory,
            remote,
            url,
            stop,
            server,
        }
    }
    async fn session(&self, project: bool, preset: PermissionPreset) -> Session {
        self.agent
            .create_session(SessionConfig {
                cwd: self
                    .home
                    .join(if project { "project" } else { "other" })
                    .to_string_lossy()
                    .into(),
                tools: Some(vec![]),
                permission_policy: PermissionPolicy {
                    preset,
                    ..Default::default()
                },
                ..Default::default()
            })
            .await
            .unwrap()
    }
    async fn update(&self, source: SettingsScope, id: &str, change: McpChange) {
        let cancel = CancellationToken::new();
        let catalog = self.directory.catalog(Some(1), &cancel).await.unwrap();
        let version = if source == SettingsScope::User {
            catalog.user_revision
        } else {
            catalog.project_revision.unwrap()
        };
        self.directory
            .update(source, version, id.into(), change, &cancel)
            .await
            .unwrap();
    }
    async fn turn(&self, session: &Session, action: Action) {
        *self.model.actions.lock().unwrap() = VecDeque::from([action, Action::Text]);
        session
            .submit("go".into(), InputMode::Queue, "main")
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), session.idle("main"))
            .await
            .unwrap()
            .unwrap();
    }
    async fn close(self) {
        self.agent.close().await.unwrap();
        self.stop.cancel();
        self.server.await.unwrap().unwrap();
        std::fs::remove_dir_all(self.home).unwrap();
    }
}
/// Fails Session assembly after McpSession applied, once discovery started
/// during assembly would have reached the server.
struct FailAfterMcp {
    remote: Arc<Remote>,
    mcp: Key<McpSession>,
}
impl FailAfterMcp {
    fn new(names: &Names<'_>, remote: Arc<Remote>) -> rutis::Result<Self> {
        Ok(Self {
            remote,
            mcp: names.key()?,
        })
    }
}
impl DynPlugin for FailAfterMcp {
    fn name(&self) -> &str {
        "FailAfterMcp"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.mcp.erase()]
    }
    fn apply<'a>(&'a self, _: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let _ = tokio::time::timeout(Duration::from_millis(500), async {
                while self.remote.initializes.load(Ordering::SeqCst) == 0 {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await;
            Err(rutis::Error::from(rutis::host::ErrorKind::PluginFailed(
                "assembly failed after McpSession".into(),
            )))
        })
    }
}

fn configuration(url: &str, generation: &str) -> Value {
    json!({"type":"http","url":url,"headers":{"X-Generation":generation}})
}
async fn ready(session: &Session) -> String {
    let mcp = session.session_service::<McpSession>().unwrap();
    let mut changed = mcp.subscribe();
    let version = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(McpSourceState::Ready { version }) =
                changed.borrow_and_update().servers.get("docs")
            {
                return version.clone();
            }
            changed.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    adopted(session, Some(&version)).await;
    version
}
async fn adopted(session: &Session, version: Option<&str>) {
    let registry = session.session_service::<ToolSourcesView>().unwrap();
    let mut changed = registry.subscribe();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let current = changed
                .borrow_and_update()
                .0
                .get("mcp:docs")
                .and_then(|s| s.adopted.clone());
            if current.as_deref() == version {
                break;
            }
            changed.changed().await.unwrap();
        }
        loop {
            let events = session.history("main", None).await.unwrap();
            if events.iter().rev().find_map(|e| match &e.kind {
                EventKind::ContextConfigured(c) => Some(
                    c.settings
                        .tool_sources
                        .iter()
                        .find(|s| s.key == "mcp:docs")
                        .map(|s| s.version.as_str()),
                ),
                _ => None,
            }) == Some(version)
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}
async fn results(session: &Session) -> Vec<String> {
    session
        .history("main", None)
        .await
        .unwrap()
        .iter()
        .filter_map(|e| match &e.kind {
            EventKind::ToolResult(r) => Some(serde_json::to_string(&r.results).unwrap()),
            _ => None,
        })
        .collect()
}

/// The failure message the Session reports for the `docs` source.
async fn failure(session: &Session) -> String {
    let mcp = session.session_service::<McpSession>().unwrap();
    let mut changed = mcp.subscribe();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(McpSourceState::Failed { message }) =
                changed.borrow_and_update().servers.get("docs")
            {
                return message.clone();
            }
            changed.changed().await.unwrap();
        }
    })
    .await
    .unwrap()
}

fn declared(tools: &[ToolDefinition], original: &str) -> ToolDefinition {
    tools
        .iter()
        .find(|tool| {
            tool.metadata
                .as_ref()
                .and_then(|metadata| metadata.mcp_original_tool_name.as_deref())
                == Some(original)
        })
        .unwrap_or_else(|| panic!("{original} was not declared"))
        .clone()
}

async fn temporary(
    session: &Session,
    method: &str,
    mut params: Value,
) -> std::result::Result<Value, dim_agent_capabilities::methods::RpcError> {
    params["sessionId"] = json!(session.id());
    session
        .session_service::<dim_agent_capabilities::methods::SessionMethods>()
        .unwrap()
        .provider(method)
        .unwrap()
        .accept(method.into(), params, CancellationToken::new(), |_| {})
        .unwrap()
        .reply
        .await
        .unwrap()
}

#[tokio::test]
async fn temporary_mcp_binds_calls_isolates_and_disposes_without_settings_writes() {
    let env = Env::new().await;
    let first = env.session(false, PermissionPreset::FullAccess).await;
    let second = env.session(false, PermissionPreset::FullAccess).await;
    ready(&first).await;
    ready(&second).await;
    let findings = env.agent.check_assembly().unwrap();
    assert!(findings.is_empty(), "{findings:?}");
    let settings = std::fs::read(env.home.join("mcp.json")).unwrap();
    let binding = uuid::Uuid::new_v4().to_string();
    let input =
        json!({"bindingId":binding,"servers":{"client":configuration(&env.url,"temporary")}});
    temporary(&first, "session.mcp_temporary.attach", input.clone())
        .await
        .unwrap();
    let initialized = env.remote.initializes.load(Ordering::SeqCst);
    temporary(&first, "session.mcp_temporary.attach", input)
        .await
        .unwrap();
    assert_eq!(env.remote.initializes.load(Ordering::SeqCst), initialized);
    env.turn(&first, Action::Text).await;
    let request = env.model.requests.lock().unwrap().last().unwrap().clone();
    let native_id = format!("temporary:{binding}:client");
    let tool = request
        .tools
        .as_ref()
        .unwrap()
        .iter()
        .find(|tool| {
            tool.metadata.as_ref().is_some_and(|metadata| {
                metadata.mcp_server_id.as_deref() == Some(&native_id)
                    && metadata.mcp_original_tool_name.as_deref() == Some("lookup/item")
            })
        })
        .expect("the very first request must include the bound MCP tool");
    assert_eq!(
        serde_json::to_value(&tool.input_schema).unwrap()["properties"]["rule"],
        json!({"oneOf":[
            {"type":"object","properties":{"kind":{"type":"string","const":"manual"}},"required":["kind"],"additionalProperties":false},
            {"type":"object","properties":{"kind":{"type":"string","const":"minutes"},"every":{"type":"integer","minimum":1}},"required":["kind","every"],"additionalProperties":false}
        ]})
    );
    let tool = tool.name.clone();
    env.turn(
        &first,
        Action::Call {
            pause: false,
            name: Some(tool),
        },
    )
    .await;
    assert!(
        results(&first)
            .await
            .last()
            .unwrap()
            .contains("temporary:8")
    );
    let sources = first.session_service::<ToolSourcesView>().unwrap();
    let (key, version) = sources
        .get()
        .0
        .into_iter()
        .find(|(key, _)| key.starts_with("temporary_mcp_"))
        .unwrap();
    let mut changes = sources.subscribe();
    tokio::time::timeout(Duration::from_secs(5), async {
        while env.remote.events.receiver_count() < 3 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    env.remote.revision.store(1, Ordering::SeqCst);
    env.remote
        .events
        .send(json!({"jsonrpc":"2.0","method":"notifications/tools/list_changed"}).to_string())
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while changes
            .borrow_and_update()
            .0
            .get(&key)
            .is_none_or(|state| state.adopted == version.adopted)
        {
            changes.changed().await.unwrap();
        }
    })
    .await
    .expect("temporary tools must adopt list changes");
    env.turn(&first, Action::Text).await;
    assert!(
        env.model
            .requests
            .lock()
            .unwrap()
            .last()
            .unwrap()
            .tools
            .as_ref()
            .unwrap()
            .iter()
            .any(|tool| {
                tool.metadata
                    .as_ref()
                    .is_some_and(|metadata| metadata.mcp_server_id.as_deref() == Some(&native_id))
                    && tool.description.contains("revision 1")
            })
    );
    env.turn(&second, Action::Text).await;
    assert!(
        env.model
            .requests
            .lock()
            .unwrap()
            .last()
            .unwrap()
            .tools
            .as_ref()
            .unwrap()
            .iter()
            .all(|tool| !tool
                .metadata
                .as_ref()
                .is_some_and(|metadata| metadata.mcp_server_id.as_deref() == Some(&native_id)))
    );
    let mut conflict =
        json!({"bindingId":binding,"servers":{"client":configuration(&env.url,"other")}});
    assert_eq!(
        temporary(&first, "session.mcp_temporary.attach", conflict.take())
            .await
            .unwrap_err()
            .data
            .unwrap()
            .app_error,
        "mcp_binding_conflict"
    );
    temporary(
        &first,
        "session.mcp_temporary.renew",
        json!({"bindingId":binding}),
    )
    .await
    .unwrap();
    let deleted = env.remote.deletes.load(Ordering::SeqCst);
    temporary(
        &first,
        "session.mcp_temporary.detach",
        json!({"bindingId":binding}),
    )
    .await
    .unwrap();
    temporary(
        &first,
        "session.mcp_temporary.detach",
        json!({"bindingId":binding}),
    )
    .await
    .unwrap();
    assert_eq!(
        env.remote.deletes.load(Ordering::SeqCst),
        deleted + 1,
        "binding disposal closes its HTTP connection despite retained model requests"
    );
    env.turn(&first, Action::Text).await;
    assert!(
        env.model
            .requests
            .lock()
            .unwrap()
            .last()
            .unwrap()
            .tools
            .as_ref()
            .unwrap()
            .iter()
            .all(|tool| !tool
                .metadata
                .as_ref()
                .is_some_and(|metadata| metadata.mcp_server_id.as_deref() == Some(&native_id)))
    );
    assert_eq!(std::fs::read(env.home.join("mcp.json")).unwrap(), settings);
    assert!(!env.home.join("project/.mcp.json").exists());
    env.close().await;
}

#[tokio::test]
async fn temporary_mcp_failed_setup_cleans_up_and_an_abandoned_binding_expires() {
    let env = Env::new().await;
    let session = env.session(false, PermissionPreset::FullAccess).await;
    ready(&session).await;
    let failed_id = uuid::Uuid::new_v4().to_string();
    let refused = temporary(&session,"session.mcp_temporary.attach",json!({"bindingId":failed_id,"servers":{"client":configuration("http://127.0.0.1:1/mcp","unused")}})).await.unwrap_err();
    assert_eq!(refused.data.unwrap().app_error, "mcp_binding_failed");
    assert_eq!(
        temporary(
            &session,
            "session.mcp_temporary.renew",
            json!({"bindingId":failed_id})
        )
        .await
        .unwrap_err()
        .data
        .unwrap()
        .app_error,
        "mcp_binding_missing"
    );
    let binding = uuid::Uuid::new_v4().to_string();
    let input =
        json!({"bindingId":binding,"servers":{"client":configuration(&env.url,"temporary")}});
    temporary(&session, "session.mcp_temporary.attach", input.clone())
        .await
        .unwrap();
    let deleted = env.remote.deletes.load(Ordering::SeqCst);
    tokio::time::timeout(Duration::from_secs(35), async {
        while env.remote.deletes.load(Ordering::SeqCst) == deleted {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("abandoned MCP resources must expire");
    assert_eq!(
        temporary(
            &session,
            "session.mcp_temporary.renew",
            json!({"bindingId":binding})
        )
        .await
        .unwrap_err()
        .data
        .unwrap()
        .app_error,
        "mcp_binding_missing"
    );
    temporary(&session, "session.mcp_temporary.attach", input)
        .await
        .unwrap();
    let deleted = env.remote.deletes.load(Ordering::SeqCst);
    session.close().await.unwrap();
    assert_eq!(
        env.remote.deletes.load(Ordering::SeqCst),
        deleted + 2,
        "closing the Session disposes both configured and temporary connections"
    );
    env.close().await;
}

async fn state(session: &Session, expected: McpSourceState) {
    let mut state = session.session_service::<McpSession>().unwrap().subscribe();
    tokio::time::timeout(Duration::from_secs(5), async {
        while state.borrow_and_update().servers.get("docs") != Some(&expected) {
            state.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn project_discovery_does_not_connect_until_the_user_enables_it() {
    let env = Env::new().await;
    // The project file asks to be enabled; ADR 003 D3 ignores that.
    let mut project = configuration(&env.url, "project");
    project["enabled"] = json!(true);
    env.update(
        SettingsScope::Project(1),
        "docs",
        McpChange::Put(serde_json::from_value(project).unwrap()),
    )
    .await;
    let file = env.home.join("project/.mcp.json");
    let written = std::fs::read(&file).unwrap();
    let session = env.session(true, PermissionPreset::FullAccess).await;
    state(&session, McpSourceState::Disabled).await;
    assert_eq!(
        env.remote.initializes.load(Ordering::SeqCst),
        0,
        "opening a workspace ran its MCP server"
    );

    env.update(
        SettingsScope::Project(1),
        "docs",
        McpChange::SetEnabled(true),
    )
    .await;
    let enabled = ready(&session).await;
    assert_eq!(env.remote.initializes.load(Ordering::SeqCst), 1);
    assert_eq!(std::fs::read(&file).unwrap(), written);
    env.update(
        SettingsScope::Project(1),
        "docs",
        McpChange::Put(serde_json::from_value(configuration(&env.url, "edited")).unwrap()),
    )
    .await;
    let mcp = session.session_service::<McpSession>().unwrap();
    let mut state = mcp.subscribe();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if matches!(state.borrow_and_update().servers.get("docs"), Some(McpSourceState::Ready { version }) if version != &enabled) { break; }
            state.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    assert_eq!(env.remote.initializes.load(Ordering::SeqCst), 2);
    env.close().await;
}

#[tokio::test]
async fn enabled_project_entry_serves_tools_and_disabling_stops_it_in_every_session() {
    let env = Env::new().await;
    env.update(
        SettingsScope::Project(1),
        "docs",
        McpChange::Put(serde_json::from_value(configuration(&env.url, "project")).unwrap()),
    )
    .await;
    env.update(
        SettingsScope::Project(1),
        "docs",
        McpChange::SetEnabled(true),
    )
    .await;
    let session = env.session(true, PermissionPreset::FullAccess).await;
    ready(&session).await;
    env.turn(
        &session,
        Action::Call {
            pause: false,
            name: None,
        },
    )
    .await;
    assert!(
        results(&session)
            .await
            .last()
            .unwrap()
            .contains("project:8")
    );
    let other = env.session(true, PermissionPreset::FullAccess).await;
    ready(&other).await;
    assert_eq!(env.remote.initializes.load(Ordering::SeqCst), 2);

    *env.model.actions.lock().unwrap() = VecDeque::from([
        Action::Call {
            pause: true,
            name: None,
        },
        Action::Text,
    ]);
    session
        .submit("pause".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), env.model.entered.notified())
        .await
        .unwrap();
    env.update(
        SettingsScope::Project(1),
        "docs",
        McpChange::SetEnabled(false),
    )
    .await;
    assert_eq!(
        env.remote.deletes.load(Ordering::SeqCst),
        2,
        "disabling returned before every Session's generation closed"
    );
    state(&other, McpSourceState::Disabled).await;
    adopted(&other, None).await;
    env.model.release.notify_one();
    tokio::time::timeout(Duration::from_secs(5), session.idle("main"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        env.remote.calls.lock().unwrap().len(),
        1,
        "a disabled server received a call"
    );
    assert!(results(&session).await.last().unwrap().contains("isError"));
    state(&session, McpSourceState::Disabled).await;
    env.close().await;
}

#[tokio::test]
async fn a_type_array_tool_registers_calls_and_stores_the_canonical_form() {
    let env = Env::new().await;
    env.remote.nullable.store(true, Ordering::SeqCst);
    let session = env.session(false, PermissionPreset::FullAccess).await;
    ready(&session).await;
    let canonical = json!({
        "type": "object",
        "properties": {"space_id": {"anyOf": [{"type": "string"}, {"type": "null"}]}},
        "required": ["space_id"]
    });
    env.turn(&session, Action::Text).await;
    let sent = env.model.requests.lock().unwrap()[0].clone();
    let spaces = declared(sent.tools.as_ref().unwrap(), "spaces");
    assert_eq!(
        serde_json::to_value(&spaces.input_schema).unwrap(),
        canonical
    );
    env.turn(
        &session,
        Action::Call {
            pause: false,
            name: Some(spaces.name.clone()),
        },
    )
    .await;
    assert!(
        results(&session)
            .await
            .last()
            .unwrap()
            .contains("spaces-ok")
    );
    let history = session.history("main", None).await.unwrap();
    let called = history
        .iter()
        .rev()
        .find_map(|event| match &event.kind {
            EventKind::ModelCall(data) => Some(data.request.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(
        serde_json::to_value(&declared(called.tools.as_ref().unwrap(), "spaces").input_schema)
            .unwrap(),
        canonical,
        "the stored request carries the canonical form"
    );
    env.close().await;
}

#[tokio::test]
async fn a_refused_tool_schema_names_the_tool_and_the_path_and_publishes_no_tool() {
    let env = Env::new().await;
    env.remote.broken.store(true, Ordering::SeqCst);
    let session = env.session(false, PermissionPreset::FullAccess).await;
    assert_eq!(
        failure(&session).await,
        "invalid MCP server response: tool broken: inputSchema.properties.x.type[1]: unknown variant `file`, expected one of `object`, `array`, `string`, `number`, `integer`, `boolean`, `null`"
    );
    env.turn(&session, Action::Text).await;
    let request = env.model.requests.lock().unwrap()[0].clone();
    assert!(
        request.tools.as_ref().unwrap().iter().all(|tool| {
            tool.metadata
                .as_ref()
                .and_then(|metadata| metadata.mcp_original_tool_name.as_deref())
                != Some("broken")
        }),
        "a refused page must publish no tool"
    );
    env.close().await;
}

#[tokio::test]
async fn a_temporary_binding_refusal_names_the_tool_and_the_path() {
    let env = Env::new().await;
    env.remote.broken.store(true, Ordering::SeqCst);
    let session = env.session(false, PermissionPreset::FullAccess).await;
    let binding = uuid::Uuid::new_v4().to_string();
    let input =
        json!({"bindingId":binding,"servers":{"client":configuration(&env.url,"temporary")}});
    let error = temporary(&session, "session.mcp_temporary.attach", input)
        .await
        .unwrap_err();
    assert_eq!(error.data.as_ref().unwrap().app_error, "mcp_binding_failed");
    assert_eq!(
        error.message,
        "Temporary MCP server initialization or connection failed: invalid MCP server response: tool broken: inputSchema.properties.x.type[1]: unknown variant `file`, expected one of `object`, `array`, `string`, `number`, `integer`, `boolean`, `null`"
    );
    env.close().await;
}

#[tokio::test]
async fn closing_one_session_drains_only_its_connections_with_retained_requests() {
    let env = Env::new().await;
    let first = env.session(false, PermissionPreset::FullAccess).await;
    let second = env.session(false, PermissionPreset::FullAccess).await;
    ready(&first).await;
    ready(&second).await;
    assert_eq!(env.remote.initializes.load(Ordering::SeqCst), 2);
    env.turn(
        &first,
        Action::Call {
            pause: false,
            name: None,
        },
    )
    .await;
    let retained = env.model.requests.lock().unwrap()[0].clone();
    first.close().await.unwrap();
    assert_eq!(env.remote.deletes.load(Ordering::SeqCst), 1);
    assert!(first.session_service::<McpSession>().is_err());
    env.turn(
        &second,
        Action::Call {
            pause: false,
            name: None,
        },
    )
    .await;
    assert!(results(&second).await.last().unwrap().contains("one:8"));
    assert_eq!(
        env.remote.initializes.load(Ordering::SeqCst),
        2,
        "closing a sibling must not reconnect this Session"
    );
    assert_eq!(env.remote.deletes.load(Ordering::SeqCst), 1);
    drop(retained);
    env.close().await;
}

#[tokio::test]
async fn bound_model_request_keeps_old_connection_while_next_request_uses_replacement() {
    let env = Env::new().await;
    let session = env.session(false, PermissionPreset::FullAccess).await;
    let v1 = ready(&session).await;
    env.model.actions.lock().unwrap().extend([
        Action::Call {
            pause: true,
            name: None,
        },
        Action::Text,
    ]);
    session
        .submit("go".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), env.model.entered.notified())
        .await
        .unwrap();
    let declarations = env.model.requests.lock().unwrap()[0].tools.clone().unwrap();
    assert_eq!(declarations.len(), 2);
    assert_ne!(declarations[0].name, declarations[1].name);
    assert!(declarations.iter().all(|t| {
        t.name.len() <= 64
            && t.name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    }));
    env.update(
        SettingsScope::User,
        "docs",
        McpChange::Put(serde_json::from_value(configuration(&env.url, "two")).unwrap()),
    )
    .await;
    let mcp = session.session_service::<McpSession>().unwrap();
    let mut state = mcp.subscribe();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if matches!(state.borrow_and_update().servers.get("docs"), Some(McpSourceState::Ready { version }) if version != &v1) { break; }
            state.changed().await.unwrap();
        }
    }).await.unwrap();
    ready(&session).await;
    assert_eq!(
        env.remote.deletes.load(Ordering::SeqCst),
        0,
        "the bound request owns V1"
    );
    env.model.release.notify_one();
    tokio::time::timeout(Duration::from_secs(5), session.idle("main"))
        .await
        .unwrap()
        .unwrap();
    assert!(results(&session).await[0].contains("one:8"));
    env.turn(
        &session,
        Action::Call {
            pause: false,
            name: None,
        },
    )
    .await;
    assert!(results(&session).await.last().unwrap().contains("two:8"));
    assert_eq!(
        *env.remote.calls.lock().unwrap(),
        vec!["lookup/item", "lookup/item"]
    );
    tokio::time::timeout(Duration::from_secs(5), async {
        while env.remote.deletes.load(Ordering::SeqCst) < 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    env.close().await;
}

#[tokio::test]
async fn project_visibility_disable_refresh_failure_and_permission_use_real_sources() {
    let env = Env::new().await;
    env.update(
        SettingsScope::Project(1),
        "docs",
        McpChange::Put(
            serde_json::from_value(json!({"type":"http","url":env.url,"enabled":false})).unwrap(),
        ),
    )
    .await;
    let user = env.session(false, PermissionPreset::FullAccess).await;
    ready(&user).await;
    let project = env.session(true, PermissionPreset::FullAccess).await;
    let view = project.session_service::<McpSession>().unwrap();
    let mut state = view.subscribe();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if state.borrow_and_update().servers.get("docs") == Some(&McpSourceState::Disabled) {
                break;
            }
            state.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    env.update(SettingsScope::Project(1), "docs", McpChange::Remove)
        .await;
    ready(&project).await;
    assert_eq!(
        env.remote.initializes.load(Ordering::SeqCst),
        2,
        "Sessions own distinct user connections"
    );
    let denied = env.session(false, PermissionPreset::ReadOnly).await;
    ready(&denied).await;
    env.turn(
        &denied,
        Action::Call {
            pause: false,
            name: None,
        },
    )
    .await;
    assert!(
        env.remote.calls.lock().unwrap().is_empty(),
        "unknown MCP risk must not bypass Approval"
    );
    assert!(results(&denied).await.last().unwrap().contains("isError"));
    let old_name = env.model.requests.lock().unwrap()[0]
        .tools
        .as_ref()
        .unwrap()[0]
        .name
        .clone();
    env.update(SettingsScope::User, "docs", McpChange::SetEnabled(false))
        .await;
    adopted(&user, None).await;
    env.turn(
        &user,
        Action::Call {
            pause: false,
            name: Some(old_name),
        },
    )
    .await;
    assert!(env.remote.calls.lock().unwrap().is_empty());
    assert!(
        env.model
            .requests
            .lock()
            .unwrap()
            .last()
            .unwrap()
            .tools
            .as_ref()
            .unwrap()
            .is_empty()
    );
    assert!(user.history("main", None).await.unwrap().iter().any(|event| matches!(
        &event.kind, EventKind::ModelOutputEnd(end)
        if matches!(&end.outcome, ModelOutputOutcome::Failed { error, .. } if error.contains("Tool was not offered"))
    )));
    env.update(SettingsScope::User, "docs", McpChange::SetEnabled(true))
        .await;
    ready(&user).await;
    env.remote.invalid.store(true, Ordering::SeqCst);
    user.session_service::<McpSession>()
        .unwrap()
        .refresh()
        .unwrap();
    adopted(&user, None).await;
    assert!(matches!(
        user.session_service::<McpSession>()
            .unwrap()
            .state()
            .servers["docs"],
        McpSourceState::Failed { .. }
    ));
    env.remote.invalid.store(false, Ordering::SeqCst);
    user.session_service::<McpSession>()
        .unwrap()
        .refresh()
        .unwrap();
    ready(&user).await;
    env.turn(
        &user,
        Action::Call {
            pause: false,
            name: None,
        },
    )
    .await;
    assert!(results(&user).await.last().unwrap().contains("one:8"));
    let connections = env.remote.initializes.load(Ordering::SeqCst);
    project.close().await.unwrap();
    assert!(view.state().closed);
    assert!(view.refresh().is_err());
    env.turn(
        &user,
        Action::Call {
            pause: false,
            name: None,
        },
    )
    .await;
    assert!(results(&user).await.last().unwrap().contains("one:8"));
    assert_eq!(
        env.remote.initializes.load(Ordering::SeqCst),
        connections,
        "closing one Session must not interrupt another Session's connection"
    );
    env.close().await;
}

#[tokio::test]
async fn unreadable_catalog_preserves_adopted_tools_until_a_valid_update() {
    let env = Env::new().await;
    let session = env.session(true, PermissionPreset::FullAccess).await;
    let view = session.session_service::<McpSession>().unwrap();
    let mut version = ready(&session).await;
    for (path, disconnect) in [
        (env.home.join("mcp.json"), false),
        (env.home.join("project/.mcp.json"), true),
    ] {
        let original = std::fs::read(&path).ok();
        std::fs::write(&path, "{ invalid json").unwrap();
        let mut state = view.subscribe();
        view.refresh().unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if state.borrow_and_update().catalog_error.is_some() {
                    break;
                }
                state.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
        assert_eq!(
            view.state().servers.get("docs"),
            Some(&McpSourceState::Ready {
                version: version.clone()
            }),
            "a catalog read error is not an empty or disabled source"
        );
        adopted(&session, Some(&version)).await;
        env.turn(
            &session,
            Action::Call {
                pause: false,
                name: None,
            },
        )
        .await;
        assert!(results(&session).await.last().unwrap().contains("one:8"));
        assert_eq!(env.remote.initializes.load(Ordering::SeqCst), 1);

        if disconnect {
            env.directory
                .disconnect(
                    SettingsScope::User,
                    "docs".into(),
                    &CancellationToken::new(),
                )
                .await
                .unwrap();
            adopted(&session, None).await;
            assert!(matches!(
                view.state().servers["docs"],
                McpSourceState::Failed { .. }
            ));
            assert!(view.state().catalog_error.is_some());
        }
        if let Some(original) = original {
            std::fs::write(&path, original).unwrap();
        } else {
            std::fs::remove_file(&path).unwrap();
        }
        view.refresh().unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if state.borrow_and_update().catalog_error.is_none()
                    && matches!(view.state().servers.get("docs"), Some(McpSourceState::Ready { version: current }) if current != &version)
                {
                    break;
                }
                state.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
        version = ready(&session).await;
    }
    std::fs::write(env.home.join("mcp.json"), r#"{"mcpServers":{}}"#).unwrap();
    view.refresh().unwrap();
    adopted(&session, None).await;
    assert!(view.state().catalog_error.is_none());
    assert!(view.state().servers.is_empty());
    env.close().await;
}

#[tokio::test]
async fn disabling_an_entry_whose_project_file_is_unreadable_withdraws_it() {
    let env = Env::new().await;
    env.update(
        SettingsScope::Project(1),
        "docs",
        McpChange::Put(serde_json::from_value(configuration(&env.url, "project")).unwrap()),
    )
    .await;
    env.update(
        SettingsScope::Project(1),
        "docs",
        McpChange::SetEnabled(true),
    )
    .await;
    let record = env.home.join("mcp-projects.json");
    let granted = std::fs::read(&record).unwrap();
    let session = env.session(true, PermissionPreset::FullAccess).await;
    let view = session.session_service::<McpSession>().unwrap();
    ready(&session).await;

    let file = env.home.join("project/.mcp.json");
    let written = std::fs::read(&file).unwrap();
    std::fs::write(&file, "{ invalid json").unwrap();
    let mut changes = view.subscribe();
    view.refresh().unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while changes.borrow_and_update().catalog_error.is_none() {
            changes.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    ready(&session).await;
    env.directory
        .update(
            SettingsScope::Project(1),
            "unreadable".into(),
            "docs".into(),
            McpChange::SetEnabled(false),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(
        env.remote.deletes.load(Ordering::SeqCst),
        1,
        "a disabled server kept running while its project file was unreadable"
    );
    state(&session, McpSourceState::Disabled).await;
    adopted(&session, None).await;
    assert_eq!(std::fs::read(&file).unwrap(), b"{ invalid json");

    std::fs::write(&record, granted).unwrap();
    std::fs::write(&file, written).unwrap();
    env.directory
        .catalog(Some(1), &CancellationToken::new())
        .await
        .unwrap();
    ready(&session).await;
    assert!(view.state().catalog_error.is_none());
    assert_eq!(env.remote.initializes.load(Ordering::SeqCst), 2);
    env.close().await;
}

#[tokio::test]
async fn a_closing_session_starts_no_connection() {
    let env = Env::new().await;
    env.update(
        SettingsScope::Project(1),
        "docs",
        McpChange::Put(serde_json::from_value(configuration(&env.url, "project")).unwrap()),
    )
    .await;
    let session = env.session(true, PermissionPreset::FullAccess).await;
    state(&session, McpSourceState::Disabled).await;
    *env.model.actions.lock().unwrap() = VecDeque::from([Action::Hold]);
    session
        .submit("hold".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), env.model.entered.notified())
        .await
        .unwrap();
    let closing = session.close();
    env.update(
        SettingsScope::Project(1),
        "docs",
        McpChange::SetEnabled(true),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        env.remote.initializes.load(Ordering::SeqCst),
        0,
        "a closing Session connected an MCP server"
    );
    env.model.release.notify_one();
    tokio::time::timeout(Duration::from_secs(5), closing)
        .await
        .unwrap()
        .unwrap();
    env.close().await;
}

#[tokio::test]
async fn slow_source_isolated_notifications_adopt_and_disconnect_withdraws_without_replay() {
    let env = Env::new().await;
    env.remote.hold.store(true, Ordering::SeqCst);
    env.update(
        SettingsScope::User,
        "slow",
        McpChange::Put(serde_json::from_value(configuration(&env.url, "slow")).unwrap()),
    )
    .await;
    let session = tokio::time::timeout(
        Duration::from_secs(2),
        env.session(false, PermissionPreset::FullAccess),
    )
    .await
    .unwrap();
    let v1 = ready(&session).await;
    tokio::time::timeout(Duration::from_secs(5), env.remote.listing.notified())
        .await
        .unwrap();
    let view = session.session_service::<McpSession>().unwrap();
    assert_eq!(view.state().servers["slow"], McpSourceState::Connecting);
    env.turn(
        &session,
        Action::Call {
            pause: false,
            name: None,
        },
    )
    .await;
    assert!(results(&session).await[0].contains("one:8"));

    env.update(SettingsScope::User, "slow", McpChange::SetEnabled(false))
        .await;
    let mut status = view.subscribe();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if status.borrow_and_update().servers.get("slow") == Some(&McpSourceState::Disabled) {
                break;
            }
            status.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    env.remote.release.notify_one();
    assert_eq!(
        ready(&session).await,
        v1,
        "another source's configuration must not replace docs"
    );

    tokio::time::timeout(Duration::from_secs(5), async {
        while env.remote.events.receiver_count() == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    env.remote.revision.store(1, Ordering::SeqCst);
    env.remote
        .events
        .send(json!({"jsonrpc":"2.0","method":"notifications/tools/list_changed"}).to_string())
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async { loop {
        if matches!(status.borrow_and_update().servers.get("docs"), Some(McpSourceState::Ready { version }) if version != &v1) { break; }
        status.changed().await.unwrap();
    }}).await.unwrap();
    ready(&session).await;
    env.turn(
        &session,
        Action::Call {
            pause: false,
            name: None,
        },
    )
    .await;
    let declarations = env
        .model
        .requests
        .lock()
        .unwrap()
        .last()
        .unwrap()
        .tools
        .clone()
        .unwrap();
    assert_eq!(declarations.len(), 2, "cancelled slow list must not return");
    assert!(declarations[0].description.contains("revision 1"));
    let calls = env.remote.calls.lock().unwrap().len();
    env.directory
        .disconnect(
            SettingsScope::User,
            "docs".into(),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    adopted(&session, None).await;
    assert!(matches!(
        view.state().servers["docs"],
        McpSourceState::Failed { .. }
    ));
    assert_eq!(env.remote.calls.lock().unwrap().len(), calls);
    view.refresh().unwrap();
    ready(&session).await;
    env.turn(
        &session,
        Action::Call {
            pause: false,
            name: None,
        },
    )
    .await;
    assert_eq!(env.remote.calls.lock().unwrap().len(), calls + 1);
    env.close().await;
}

#[tokio::test]
async fn an_unpublished_session_never_starts_discovery() {
    let env = Env::with(|builder, remote| {
        let remote = remote.clone();
        builder.require_feature(
            Feature::new("FailAfterMcp").session("FailAfterMcp", move |names| {
                Ok(Box::new(FailAfterMcp::new(names, remote.clone())?))
            }),
        );
    })
    .await;
    let created = env
        .agent
        .create_session(SessionConfig {
            cwd: env.home.join("other").to_string_lossy().into(),
            tools: Some(vec![]),
            ..Default::default()
        })
        .await;
    let error = created.err().expect("assembly fails").to_string();
    assert!(
        error.contains("assembly failed after McpSession"),
        "{error}"
    );
    assert_eq!(env.remote.initializes.load(Ordering::SeqCst), 0);
    env.close().await;
}

#[tokio::test]
async fn a_shadowed_user_server_closes_its_unused_connection() {
    let env = Env::new().await;
    let session = env.session(true, PermissionPreset::FullAccess).await;
    ready(&session).await;
    env.turn(&session, Action::Text).await;
    assert_eq!(env.remote.initializes.load(Ordering::SeqCst), 1);
    let mut shadow = configuration(&env.url, "project");
    shadow["enabled"] = json!(false);
    env.update(
        SettingsScope::Project(1),
        "docs",
        McpChange::Put(serde_json::from_value(shadow).unwrap()),
    )
    .await;
    let mut state = session.session_service::<McpSession>().unwrap().subscribe();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !matches!(
            state.borrow_and_update().servers.get("docs"),
            Some(McpSourceState::Disabled)
        ) {
            state.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    env.turn(&session, Action::Text).await;
    tokio::time::timeout(Duration::from_secs(5), async {
        while env.remote.deletes.load(Ordering::SeqCst) < 1 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the unused user connection closes");
    assert_eq!(env.remote.initializes.load(Ordering::SeqCst), 1);
    env.close().await;
}

const TOOL_IMAGE: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jD1sAAAAASUVORK5CYII=";

/// The Part 1 flow: an MCP tool returns a text block and a PNG, and the model
/// request that follows carries the image instead of failing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_image_tool_results_reach_the_model_and_the_session_keeps_working() {
    use dim_agent_aimux::{AimuxModels, AimuxPlugin, ConfiguredModelsPlugin};
    tokio::time::timeout(Duration::from_secs(30), async {
        let requests: Arc<Mutex<Vec<Value>>> = Default::default();
        let seen = requests.clone();
        let router = Router::new().route(
            "/v1/chat/completions",
            post(move |Json(body): Json<Value>| {
                let seen = seen.clone();
                async move {
                    let tool_done = body["messages"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|message| message["role"] == "tool");
                    let offered = body["tools"].as_array().map(|tools| {
                        tools
                            .iter()
                            .find_map(|tool| {
                                tool["function"]["name"]
                                    .as_str()
                                    .filter(|name| name.contains("screenshot"))
                                    .map(str::to_owned)
                            })
                    });
                    let (delta, finish) = match (tool_done, offered.flatten()) {
                        (true, _) => (json!({"content":"the screen shows one pixel"}), "stop"),
                        (false, None) => (json!({"content":"waiting for the tool"}), "stop"),
                        (false, Some(name)) => (
                            json!({"tool_calls":[{"index":0,"id":"call-image","type":"function","function":{"name":name,"arguments":"{}"}}]}),
                            "tool_calls",
                        ),
                    };
                    seen.lock().unwrap().push(body);
                    (
                        [("content-type", "text/event-stream")],
                        format!(
                            "data: {}\n\ndata: [DONE]\n\n",
                            json!({"id":"remote","choices":[{"index":0,"delta":delta,"finish_reason":finish}]})
                        ),
                    )
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/v1", listener.local_addr().unwrap());
        let stop = CancellationToken::new();
        let cancel = stop.clone();
        let http = tokio::spawn(async move {
            axum::serve(listener, router)
                .with_graceful_shutdown(cancel.cancelled_owned())
                .await
                .unwrap();
        });

        let remote = Remote::new();
        remote.images.store(true, Ordering::SeqCst);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mcp_url = format!("http://{}/mcp", listener.local_addr().unwrap());
        let mcp_stop = CancellationToken::new();
        let mcp_cancel = mcp_stop.clone();
        let mcp = tokio::spawn(
            axum::serve(
                listener,
                Router::new()
                    .route("/mcp", post(rpc).get(events).delete(delete))
                    .with_state(remote.clone()),
            )
            .with_graceful_shutdown(mcp_cancel.cancelled_owned())
            .into_future(),
        );

        let home = std::env::temp_dir().join(format!("dim-mcp-image-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(home.join("project")).unwrap();
        let home = home.canonicalize().unwrap();
        std::fs::write(
            home.join("mcp.json"),
            json!({"mcpServers":{"docs":configuration(&mcp_url, "one")}}).to_string(),
        )
        .unwrap();
        let models = Arc::new(
            AimuxModels::from_config(
                serde_json::from_value(json!({
                    "defaultModel":{"providerId":"local","modelId":"test-model"},
                    "providers":[{"id":"local","kind":"openai-compatible","baseUrl":url,
                        "models":[{"id":"test-model","contextWindow":128000,"maxOutputTokens":4096,
                            "config":{"maxTokens":4096,"temperature":0}}]}]
                }))
                .unwrap(),
            )
            .unwrap(),
        );
        let mut builder = AgentBuilder::new(AgentConfig {
            model: models.default_model().clone(),
            ..Default::default()
        });
        builder
            .session_value(Arc::new(HistoryRenderer::default()))
            .session_value(Arc::new(PrepareContextHost(Arc::new(IdentityContext))))
            .session_value(Arc::new(ContextModelHost(None)))
            .session_value(Arc::new(CompactionStrategyHost(None)));
        defaults::tool_host(&mut builder, Arc::new(MockTools));
        defaults::install_with_prompt_capability(
            &mut builder,
            dim_agent_capabilities::model_provider::ModelProviderPlugin::process,
        );
        builder
            .session_value(Arc::new(PromptHost(Arc::new(EmptyPromptHost))))
            .session_value(Arc::new(
                dim_agent_capabilities::prompt::InitialPromptConfiguration::fixed(Some(
                    ConfigureUpdate {
                        sources: vec![],
                        model: Some(ModelSelection {
                            provider_id: "local".into(),
                            model_id: "test-model".into(),
                            capability_snapshot: None,
                            config: None,
                            metadata: None,
                        }),
                    },
                )),
            ))
            .session_value(Arc::new(dim_agent_capabilities::permission::CommandRulesHost(
                Arc::new(dim_agent_capabilities::permission::NoCommandRules),
            )));
        dim_agent_aimux::install(&mut builder);
        mcp::install(&mut builder);
        mcp_host_rows(&mut builder, &home);
        builder
            .plugin(|_| Ok(Box::new(AimuxPlugin)))
            .plugin(move |_| Ok(Box::new(ConfiguredModelsPlugin(models.clone()))));
        let agent = MinimalHost::start(&builder).await.unwrap();
        authorize_project(&agent, &home).await;

        let session = agent
            .create_session(SessionConfig {
                cwd: home.join("project").to_string_lossy().into(),
                tools: Some(vec![]),
                model: ModelSelection {
                    provider_id: "local".into(),
                    model_id: "test-model".into(),
                    capability_snapshot: None,
                    config: None,
                    metadata: None,
                },
                permission_policy: PermissionPolicy {
                    preset: PermissionPreset::FullAccess,
                    ..Default::default()
                },
                ..Default::default()
            })
            .await
            .unwrap();
        // The tool enters the request once its source is adopted.
        ready(&session).await;
        let mut ran = false;
        for _ in 0..3 {
            session
                .submit("capture the screen".into(), InputMode::Queue, "main")
                .await
                .unwrap();
            session.idle("main").await.unwrap();
            let events = session.history("main", None).await.unwrap();
            assert!(
                matches!(events.last().unwrap().kind, EventKind::TurnEnd(TurnEndData { outcome: TurnOutcome::Completed { .. }, .. })),
                "{:?}",
                events.iter().filter_map(|event| match &event.kind { EventKind::ModelOutputEnd(data) => Some(&data.outcome), _ => None }).collect::<Vec<_>>()
            );
            ran = events.iter().any(|event| matches!(&event.kind, EventKind::ToolResult(data) if matches!(&data.results[0].result.content, ToolContent::Blocks(blocks) if blocks.iter().any(|block| matches!(block, ContentBlock::Image { .. })))));
            if ran {
                break;
            }
        }
        assert!(ran, "the MCP image tool ran and the stored tool result keeps the image");
        let bodies = requests.lock().unwrap().clone();
        let next = bodies
            .iter()
            .rev()
            .find(|body| {
                body["messages"]
                    .as_array()
                    .is_some_and(|messages| messages.iter().any(|message| message["role"] == "tool"))
            })
            .unwrap();
        let tool = next["messages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|message| message["role"] == "tool")
            .unwrap();
        assert!(
            tool["content"].as_str().unwrap().contains("[Image: sent in the following user message as tool output.]"),
            "{tool}"
        );
        let user = next["messages"].as_array().unwrap().last().unwrap();
        assert_eq!(user["role"], "user");
        assert!(
            user["content"][1]["image_url"]["url"].as_str().unwrap() == format!("data:image/png;base64,{TOOL_IMAGE}"),
            "{user}"
        );
        session
            .submit("thanks".into(), InputMode::Queue, "main")
            .await
            .unwrap();
        session.idle("main").await.unwrap();
        assert!(
            matches!(session.history("main", None).await.unwrap().last().unwrap().kind, EventKind::TurnEnd(TurnEndData { outcome: TurnOutcome::Completed { .. }, .. })),
            "a later turn also completes"
        );
        agent.close().await.unwrap();
        stop.cancel();
        http.await.unwrap();
        mcp_stop.cancel();
        mcp.await.unwrap().unwrap();
        std::fs::remove_dir_all(&home).unwrap();
    })
    .await
    .unwrap();
}

/// The published names the Session offers the model, from the request the
/// binding made for a turn.
async fn offered(env: &Env, session: &Session) -> Vec<ToolDefinition> {
    env.turn(session, Action::Text).await;
    env.model
        .requests
        .lock()
        .unwrap()
        .last()
        .unwrap()
        .tools
        .as_ref()
        .unwrap()
        .as_ref()
        .clone()
}

fn arguments(value: Value) -> JsonObject {
    value.as_object().cloned().unwrap()
}

fn original(tool: &ToolDefinition) -> Option<&str> {
    tool.metadata
        .as_ref()
        .and_then(|metadata| metadata.mcp_original_tool_name.as_deref())
}

#[tokio::test]
async fn a_view_mounts_its_document_and_calls_the_entry_it_came_from() {
    let env = Env::new().await;
    env.remote.widgets.store(1, Ordering::SeqCst);
    let session = env.session(false, PermissionPreset::ReadOnly).await;
    ready(&session).await;
    let tools = offered(&env, &session).await;
    let chart = declared(&tools, "chart").name.clone();
    assert!(
        tools.iter().all(|tool| original(tool) != Some("refresh")),
        "a tool whose visibility excludes the model is published to no model"
    );
    let mcp = session.session_service::<McpSession>().unwrap();

    let widget = mcp.widget(&chart).await.unwrap();
    assert_eq!(widget.server, "docs");
    assert_eq!(widget.name, "chart");
    assert_eq!(widget.uri, "ui://widget/counter.html");
    assert_eq!(widget.mime_type, "text/html;profile=mcp-app");
    assert_eq!(widget.text.as_deref(), Some("<p>counter</p>"));
    assert!(widget.blob.is_none());
    assert_eq!(widget.description.as_deref(), Some("Render a counter"));
    assert!(
        widget
            .input_schema
            .properties
            .as_ref()
            .unwrap()
            .contains_key("step")
    );
    assert_eq!(
        Value::Object(widget.app.clone().unwrap()),
        json!({"csp":{"connectDomains":["https://api.example"]},"prefersBorder":true})
    );
    let wire = serde_json::to_string(&widget).unwrap();
    assert!(
        !wire.contains("127.0.0.1"),
        "no endpoint reaches a view: {wire}"
    );
    assert!(
        !wire.contains("X-Generation"),
        "no header reaches a view: {wire}"
    );

    let plain = declared(&tools, "lookup/item").name.clone();
    assert_eq!(mcp.widget(&plain).await.unwrap_err(), McpAppError::Absent);
    assert_eq!(
        mcp.widget("mcp_nowhere_absent").await.unwrap_err(),
        McpAppError::Absent
    );

    let chart_call = mcp
        .call("docs", "chart", arguments(json!({"step":2})))
        .await
        .unwrap();
    assert_eq!(
        chart_call.content,
        ToolContent::Blocks(vec![ContentBlock::Text {
            text: "chart-ok".into()
        }])
    );
    assert_eq!(
        chart_call.structured_content,
        Some(arguments(json!({"step":2})))
    );
    assert_eq!(chart_call.meta, Some(arguments(json!({"widget":"chart"}))));
    assert_eq!(chart_call.is_error, None);

    assert_eq!(
        mcp.call("docs", "missing", arguments(json!({})))
            .await
            .unwrap_err(),
        McpAppError::Absent
    );
    assert_eq!(
        mcp.call("other", "chart", arguments(json!({})))
            .await
            .unwrap_err(),
        McpAppError::UnknownServer
    );
    assert_eq!(
        mcp.call("docs", "summary", arguments(json!({})))
            .await
            .unwrap_err(),
        McpAppError::NotCallable
    );
    assert!(
        !env.remote
            .calls
            .lock()
            .unwrap()
            .contains(&"summary".to_string()),
        "a tool the server declares model-only is never sent"
    );
    let failed = mcp
        .call("docs", "refresh", arguments(json!({})))
        .await
        .unwrap();
    assert_eq!(
        failed.is_error,
        Some(true),
        "the server's own failure is the call's answer"
    );
    assert!(
        env.remote
            .calls
            .lock()
            .unwrap()
            .contains(&"refresh".to_string())
    );

    let events = session.history("main", None).await.unwrap();
    assert!(
        events.iter().all(|event| !matches!(
            event.kind,
            EventKind::ToolCall(_) | EventKind::ToolStart(_) | EventKind::ToolResult(_)
        )),
        "a view's call writes no fact"
    );
    env.turn(&session, Action::Text).await;
    let next = env.model.requests.lock().unwrap().last().unwrap().clone();
    assert!(
        !serde_json::to_string(&next.messages)
            .unwrap()
            .contains("chart-ok"),
        "a view's result never enters the model context"
    );
    env.close().await;
}

#[tokio::test]
async fn a_mount_refuses_an_unsupported_document_and_an_unusable_entry() {
    let env = Env::new().await;
    env.remote.widgets.store(1, Ordering::SeqCst);
    let session = env.session(false, PermissionPreset::FullAccess).await;
    ready(&session).await;
    let tools = offered(&env, &session).await;
    let chart = declared(&tools, "chart").name.clone();
    let mcp = session.session_service::<McpSession>().unwrap();

    env.remote.widgets.store(2, Ordering::SeqCst);
    assert_eq!(
        mcp.widget(&chart).await.unwrap_err(),
        McpAppError::Unsupported
    );
    env.remote.widgets.store(3, Ordering::SeqCst);
    assert_eq!(
        mcp.widget(&chart).await.unwrap_err(),
        McpAppError::Unsupported
    );
    env.remote.widgets.store(1, Ordering::SeqCst);
    assert!(mcp.widget(&chart).await.is_ok());

    env.update(SettingsScope::User, "docs", McpChange::SetEnabled(false))
        .await;
    adopted(&session, None).await;
    assert_eq!(
        mcp.widget(&chart).await.unwrap_err(),
        McpAppError::Unavailable,
        "the view learns its entry is disabled, not that its tool name is unknown"
    );
    assert_eq!(
        mcp.call("docs", "chart", arguments(json!({})))
            .await
            .unwrap_err(),
        McpAppError::Unavailable
    );
    assert_eq!(
        mcp.widget("mcp_nowhere_absent").await.unwrap_err(),
        McpAppError::Absent,
        "a name the Session never held is absent"
    );
    assert!(
        env.remote.calls.lock().unwrap().is_empty(),
        "nothing was sent to a disabled entry"
    );
    env.close().await;
}

#[tokio::test]
async fn a_replaced_connection_generation_re_reads_the_document() {
    let env = Env::new().await;
    env.remote.widgets.store(1, Ordering::SeqCst);
    let session = env.session(false, PermissionPreset::FullAccess).await;
    let version = ready(&session).await;
    let tools = offered(&env, &session).await;
    let chart = declared(&tools, "chart").name.clone();
    let mcp = session.session_service::<McpSession>().unwrap();
    assert!(mcp.widget(&chart).await.is_ok());

    env.update(SettingsScope::User, "docs", McpChange::SetEnabled(false))
        .await;
    adopted(&session, None).await;
    assert_eq!(
        mcp.widget(&chart).await.unwrap_err(),
        McpAppError::Unavailable
    );

    env.update(SettingsScope::User, "docs", McpChange::SetEnabled(true))
        .await;
    let next = ready(&session).await;
    assert_ne!(
        version, next,
        "a replaced generation is a new discovery for a client's cache"
    );
    let widget = mcp.widget(&chart).await.unwrap();
    assert_eq!(widget.server, "docs");
    assert_eq!(widget.name, "chart");
    assert_eq!(widget.text.as_deref(), Some("<p>counter</p>"));
    env.close().await;
}

#[tokio::test]
async fn a_view_call_the_connection_took_but_did_not_answer_is_unknown() {
    let env = Env::new().await;
    env.remote.widgets.store(1, Ordering::SeqCst);
    let session = env.session(false, PermissionPreset::FullAccess).await;
    ready(&session).await;
    env.remote.app_hold.store(true, Ordering::SeqCst);
    let mcp = session.session_service::<McpSession>().unwrap();
    let worker = tokio::spawn({
        let mcp = mcp.clone();
        async move {
            mcp.call("docs", "chart", arguments(json!({"step":2})))
                .await
        }
    });
    tokio::time::timeout(Duration::from_secs(5), env.remote.app_called.notified())
        .await
        .expect("the server received the call");
    session.close().await.unwrap();
    assert_eq!(
        worker.await.unwrap().unwrap_err(),
        McpAppError::Unknown,
        "the server may have run a call whose answer never arrived"
    );
    env.close().await;
}

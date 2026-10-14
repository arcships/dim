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
use dim_agent_runtime::capability_plugin;
use dim_agent_runtime::*;
use dim_agent_types::*;
use dim_app_server::{
    mcp::{self, McpChange, McpPlugin, McpService, McpSession, McpSourceState},
    settings::{SettingsPlugin, SettingsService, SettingsSource},
};
use rutis::Ctx;
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    convert::Infallible,
    path::PathBuf,
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
            json!({"tools":[
                {"name":"lookup/item","description":format!("Subtract two numbers, revision {}", remote.revision.load(Ordering::SeqCst)),"inputSchema":{"type":"object","properties":{"a":{"type":"integer"},"b":{"type":"integer"}},"required":["a","b"]}},
                {"name":"lookup_item","description":"A distinct original name","inputSchema":{"type":"object"}}
            ]})
        }
        "tools/call" => {
            remote
                .calls
                .lock()
                .unwrap()
                .push(body["params"]["name"].as_str().unwrap().into());
            let a = body["params"]["arguments"]["a"].as_i64().unwrap();
            let b = body["params"]["arguments"]["b"].as_i64().unwrap();
            let generation = headers.get("x-generation").unwrap().to_str().unwrap();
            json!({"content":[{"type":"text","text":format!("{generation}:{}", a-b)}]})
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
    Call { pause: bool, name: Option<String> },
    Text,
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
    root: Ctx,
    publication: mcp_host::Host,
    home: PathBuf,
    agent: Agent,
    model: Arc<Model>,
    directory: Arc<McpService>,
    remote: Arc<Remote>,
    url: String,
    stop: CancellationToken,
    server: tokio::task::JoinHandle<std::io::Result<()>>,
}
#[path = "fixtures/mcp_host.rs"]
mod mcp_host;
impl Env {
    async fn new() -> Self {
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
        let root = Ctx::root().unwrap();
        let (publication, _) = mcp_host::Host::start(&root).await;
        let views = vec![
            root.plugin(capability_plugin(
                dim_agent_capabilities::file_storage::FileStoragePlugin::new(vec![home.clone()]),
            )),
            root.plugin(capability_plugin(SettingsPlugin::new(0, vec![]))),
            root.plugin(dim_agent_runtime::capability_plugin(McpPlugin)),
        ];
        wait_active(
            &root,
            &views,
            Duration::from_secs(5),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        root.get::<SettingsService>()
            .unwrap()
            .authorize_project(home.join("project"), &CancellationToken::new())
            .await
            .unwrap();
        let directory = root.get::<McpService>().unwrap();
        let model = Arc::new(Model::default());
        let mut builder = AgentBuilder::new(Default::default());
        builder
            .session_value(Arc::new(HistoryRenderer::default()))
            .session_value(Arc::new(ModelHost(model.clone())))
            .session_value(Arc::new(PrepareContextHost(Arc::new(IdentityContext))))
            .session_value(Arc::new(ContextModelHost(None)))
            .session_plugin(DefaultPrepareModelPlugin::new);
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
        let agent = builder.build_in(&root);
        Self {
            root,
            publication,
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
                tools: vec![],
                permission_policy: PermissionPolicy {
                    preset,
                    ..Default::default()
                },
                ..Default::default()
            })
            .await
            .unwrap()
    }
    async fn update(&self, source: SettingsSource, id: &str, change: McpChange) {
        let cancel = CancellationToken::new();
        let catalog = self.directory.catalog(Some(1), &cancel).await.unwrap();
        let version = if source == SettingsSource::UserMcp {
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
        // A rejected tool can end its turn without requesting the text step.
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
        self.publication.close().await;
        self.root.shutdown().await.unwrap();
        self.stop.cancel();
        self.server.await.unwrap().unwrap();
        std::fs::remove_dir_all(self.home).unwrap();
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
        // Registry acceptance precedes the branch's persisted adoption.
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

#[tokio::test]
async fn disabled_project_entry_does_not_connect() {
    let env = Env::new().await;
    let mut disabled = configuration(&env.url, "project");
    disabled["enabled"] = json!(false);
    env.update(
        SettingsSource::ProjectMcp(1),
        "docs",
        McpChange::Put(serde_json::from_value(disabled).unwrap()),
    )
    .await;
    let session = env.session(true, PermissionPreset::FullAccess).await;
    let mut state = session.session_service::<McpSession>().unwrap().subscribe();
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
    let initializes = env.remote.initializes.load(Ordering::SeqCst);
    env.close().await;
    assert_eq!(
        initializes, 0,
        "disabled project entry made an external request"
    );
}

#[tokio::test]
async fn enabled_project_entry_connects_and_disconnect_blocks_an_already_bound_request() {
    let env = Env::new().await;
    env.update(
        SettingsSource::ProjectMcp(1),
        "docs",
        McpChange::Put(serde_json::from_value(configuration(&env.url, "project")).unwrap()),
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
    assert_eq!(env.remote.initializes.load(Ordering::SeqCst), 1);

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
    env.directory
        .disconnect(
            SettingsSource::ProjectMcp(1),
            "docs".into(),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    env.model.release.notify_one();
    tokio::time::timeout(Duration::from_secs(5), session.idle("main"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        env.remote.calls.lock().unwrap().len(),
        1,
        "retained tool binding bypassed disconnect"
    );
    assert!(results(&session).await.last().unwrap().contains("isError"));
    tokio::time::timeout(Duration::from_secs(5), async {
        while env.remote.deletes.load(Ordering::SeqCst) < 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(matches!(
        session
            .session_service::<McpSession>()
            .unwrap()
            .state()
            .servers["docs"],
        McpSourceState::Failed { .. }
    ));
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
        SettingsSource::UserMcp,
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
        SettingsSource::ProjectMcp(1),
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
    env.update(SettingsSource::ProjectMcp(1), "docs", McpChange::Remove)
        .await;
    ready(&project).await;
    assert_eq!(
        env.remote.initializes.load(Ordering::SeqCst),
        1,
        "Sessions share the user connection"
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
    env.update(
        SettingsSource::UserMcp,
        "docs",
        McpChange::SetEnabled(false),
    )
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
    env.update(SettingsSource::UserMcp, "docs", McpChange::SetEnabled(true))
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
            // Preserving sources must also preserve their failure subscription;
            // a broken catalog does not keep a dead connection callable.
            env.directory
                .disconnect(
                    SettingsSource::UserMcp,
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
    // A successfully read empty directory really removes the source.
    std::fs::write(env.home.join("mcp.json"), r#"{"mcpServers":{}}"#).unwrap();
    view.refresh().unwrap();
    adopted(&session, None).await;
    assert!(view.state().catalog_error.is_none());
    assert!(view.state().servers.is_empty());
    env.close().await;
}

#[tokio::test]
async fn slow_source_isolated_notifications_adopt_and_disconnect_withdraws_without_replay() {
    let env = Env::new().await;
    env.remote.hold.store(true, Ordering::SeqCst);
    env.update(
        SettingsSource::UserMcp,
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

    // A completed list from a cancelled worker must not republish a disabled source.
    env.update(
        SettingsSource::UserMcp,
        "slow",
        McpChange::SetEnabled(false),
    )
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
            SettingsSource::UserMcp,
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

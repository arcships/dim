use super::*;
use dim_agent_capabilities::{
    adapters::{DefaultPrepareModelPlugin, EchoModel},
    history::HistoryRenderer,
    host::{
        CompactionStrategyHost, ContextModelHost, IdentityContext, ModelChunk, ModelConnection,
        ModelDriver, ModelHost, ModelStopReason, ModelStream, PrepareContextHost,
    },
    methods::SessionMethods,
    tool_registry::definition,
};
use dim_agent_runtime::SESSION_GROUP;
use dim_agent_types::{MessageContent, MessagePart, ModelRequest, Role};
use dim_app_server::session_typescript::SessionTypeScriptConfig;
use rutis::host::{CtxHost, InstanceId};
use std::path::PathBuf;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires DIM_TEST_NODE and DIM_TEST_JS_RUNTIME"]
async fn node_session_part_calls_provider_auxiliary_model_without_timeline_writes() {
    use axum::{
        Json, Router, body::Body, extract::State, http::HeaderMap, response::IntoResponse,
        routing::post,
    };
    let (sent, mut requests) = tokio::sync::mpsc::unbounded_channel();
    let (released, mut cancellations) = tokio::sync::mpsc::unbounded_channel();
    type HttpState = (
        tokio::sync::mpsc::UnboundedSender<(Value, String)>,
        tokio::sync::mpsc::UnboundedSender<tokio::sync::oneshot::Receiver<()>>,
    );
    async fn complete(
        State((sent, released)): State<HttpState>,
        headers: HeaderMap,
        Json(body): Json<Value>,
    ) -> axum::response::Response {
        let hanging = body["model"] == "hang";
        sent.send((body, headers["authorization"].to_str().unwrap().into()))
            .unwrap();
        if hanging {
            let (held, closed) = tokio::sync::oneshot::channel::<()>();
            released.send(closed).unwrap();
            let body = Body::from_stream(futures_util::stream::once(async move {
                let _held = held;
                std::future::pending::<Result<String, std::io::Error>>().await
            }));
            return axum::response::Response::builder()
                .header("content-type", "text/event-stream")
                .body(body)
                .unwrap();
        }
        (
            [("content-type", "text/event-stream")],
            format!(
                "data: {}\n\ndata: [DONE]\n\n",
                json!({"id":"auxiliary", "choices":[{"index":0,
                    "delta":{"content":"A short title"}, "finish_reason":"stop"}],
                    "usage":{"prompt_tokens":7,"completion_tokens":3,"total_tokens":10}})
            ),
        )
            .into_response()
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = Router::new()
        .route("/{*path}", post(complete))
        .with_state((sent, released));
    let http = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let home = PathBuf::from("/tmp").join(format!("dim-aux-{}", uuid::Uuid::new_v4()));
    let mut parts = fixture();
    parts.rows = vec![json!({
        "id":"auxiliary", "name":parts.anchor.parent().unwrap().join("fixtures/auxiliary-model.ts"),
        "provides":{},
        "config":{},
    })];
    let (server, agent) = launch_with_agent(
        Arc::new(MockTools),
        ServerConfig {
            product_home: Some(home.clone()),
            target_home: Some(home.join("rust")),
            session_typescript: Some(parts.clone()),
            ..Default::default()
        },
    )
    .await;
    let mut client = Client::new_version(&server, false, 3).await;
    assert_ok(&client.rpc("provider.create", json!({"providerId":"018ec73f-0000-7000-8000-000000000009","displayName":"Auxiliary fixture","connection":{"driverKind":"openai-compatible","baseUrl":format!("http://{address}/v1"),"auth":{"type":"bearer"},"credential":{"action":"set","value":"auxiliary-fixture-secret"}},"models":[{"modelId":"title", "displayName":"Title", "status":"active",
            "capabilities":dim_agent_types::CatalogCapabilities::with_limits(128_000, 16_384)},
            {"modelId":"hang", "displayName":"Hang", "status":"active",
            "capabilities":dim_agent_types::CatalogCapabilities::with_limits(128_000, 16_384)}],"enabled":true})).await);
    assert_ok(
        &client
            .rpc(
                "provider.select",
                json!({"providerId":"018ec73f-0000-7000-8000-000000000009","modelId":"title"}),
            )
            .await,
    );
    let created = client
        .rpc("session.create", json!({"cwd":home,"tools":[]}))
        .await;
    assert_ok(&created);
    let sid = created["result"]["sessionId"].as_str().unwrap();
    let session = agent.get_session(sid).unwrap();
    let before = session.history("main", None).await.unwrap();
    let request = json!({"model":{"providerId":"018ec73f-0000-7000-8000-000000000009","modelId":"title"}, "request":{
        "system":"Write a title", "messages":[{"role":"user","content":"hello"}], "maxOutputTokens":32
    }});
    let reply = client
        .rpc(
            "session.auxiliary.complete",
            json!({"sessionId":sid,"request":request}),
        )
        .await;
    assert_ok(&reply);
    assert_eq!(reply["result"]["text"], "A short title");
    assert_eq!(reply["result"]["stopReason"], "end_turn");
    assert_eq!(reply["result"]["usage"]["totalTokens"], 10.0);
    assert!(!reply.to_string().contains("auxiliary-fixture-secret"));
    let (body, authorization) = requests.recv().await.unwrap();
    assert_eq!(authorization, "Bearer auxiliary-fixture-secret");
    assert_eq!(body["model"], "title");
    assert!(body.get("tools").is_none(), "{body}");
    assert!(body.to_string().contains("Write a title"));

    for (request, code) in [
        (
            json!({"model":{"providerId":"018ec73f-0000-7000-8000-000000000009","modelId":"title"},"request":{"messages":[]}}),
            "invalid_request",
        ),
        (
            json!({"model":{"providerId":"missing","modelId":"title"},"request":{"messages":[{"role":"user","content":"hello"}]}}),
            "model_unavailable",
        ),
        (
            json!({"model":{"providerId":"018ec73f-0000-7000-8000-000000000009","modelId":"title"},"request":{"messages":[{"role":"user","content":"hello"}],"maxOutputTokens":0}}),
            "invalid_request",
        ),
        (
            json!({"model":{"providerId":"018ec73f-0000-7000-8000-000000000009","modelId":"title"},"request":{"messages":[{"role":"user","content":"hello"}]},"unexpected":true}),
            "invalid_request",
        ),
        (
            json!({"model":{"providerId":"018ec73f-0000-7000-8000-000000000009","modelId":"title","extra":1},"request":{"messages":[{"role":"user","content":"hello"}]}}),
            "invalid_request",
        ),
    ] {
        let reply = client
            .rpc(
                "session.auxiliary.complete",
                json!({"sessionId":sid,"request":request}),
            )
            .await;
        assert_ok(&reply);
        assert_eq!(reply["result"]["error"]["code"], code, "{reply}");
        assert!(reply["result"]["error"]["message"].is_string());
        assert!(!reply.to_string().contains("auxiliary-fixture-secret"));
    }
    assert!(
        requests.try_recv().is_err(),
        "invalid calls must not reach the model"
    );
    let after = session.history("main", None).await.unwrap();
    assert_eq!(
        before.iter().map(|event| &event.id).collect::<Vec<_>>(),
        after.iter().map(|event| &event.id).collect::<Vec<_>>()
    );
    let pending = client
        .send(
            "session.auxiliary.complete",
            json!({"sessionId":sid,"cancellable":true,"request":{
                "model":{"providerId":"018ec73f-0000-7000-8000-000000000009","modelId":"hang"},
                "request":{"messages":[{"role":"user","content":"wait"}]}
            }}),
        )
        .await;
    let (body, _) = requests.recv().await.unwrap();
    assert_eq!(body["model"], "hang");
    let mut closer = Client::new_version(&server, false, 3).await;
    let closed = tokio::time::timeout(
        Duration::from_secs(5),
        closer.rpc("session.close", json!({"sessionId":sid})),
    )
    .await
    .expect("caller cleanup must settle the hanging auxiliary call");
    assert_ok(&closed);
    assert_eq!(
        client.response(pending).await["error"]["data"]["appError"],
        "operation_interrupted"
    );
    let closed = cancellations.recv().await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(5), closed)
            .await
            .expect("the model HTTP stream must close without unloading Provider")
            .is_err()
    );
    drop((client, closer, session, agent));
    server.shutdown();
    server.join().await.unwrap();
    http.abort();

    let server = launch(
        Arc::new(MockTools),
        ServerConfig {
            session_typescript: Some(parts),
            ..Default::default()
        },
    )
    .await;
    let mut client = Client::new_version(&server, false, 3).await;
    let created = client.rpc("session.create", create(json!([]))).await;
    assert_ok(&created);
    let missing = client
        .rpc(
            "session.auxiliary.complete",
            json!({"sessionId":created["result"]["sessionId"],"request":request}),
        )
        .await;
    assert_eq!(
        missing["error"]["data"]["appError"], "unknown_method",
        "{missing}"
    );
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// The row of a repository plugin built from its `dim-plugin.json` `code`
/// fields, named by the absolute path of its entry.
pub(super) fn manifest_row(plugin: &str) -> Value {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../runtime/plugins")
        .join(plugin)
        .canonicalize()
        .unwrap();
    let manifest: Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("dim-plugin.json")).unwrap())
            .unwrap();
    let code = &manifest["code"];
    json!({
        "id": plugin,
        "name": dir.join(code["entry"].as_str().unwrap()),
        "inject": code["inject"],
        "provides": code.get("provides").cloned().unwrap_or_else(|| json!({})),
        "config": code.get("config").cloned().unwrap_or_else(|| json!({})),
    })
}

pub(super) fn fixture() -> SessionTypeScriptConfig {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    SessionTypeScriptConfig {
        python: None,
        acquire: None,
        node: std::env::var_os("DIM_TEST_NODE")
            .expect("DIM_TEST_NODE")
            .into(),
        js_runtime: std::env::var_os("DIM_TEST_JS_RUNTIME")
            .expect("DIM_TEST_JS_RUNTIME")
            .into(),
        anchor: root.join("runtime/package.json"),
        rows: vec![json!({
            "id": "typescript", "name": root.join("runtime/fixtures/session-part.ts"),
            "provides": {},
            "config": {},
        })],
    }
}

fn observers(lifecycle_file: &std::path::Path) -> SessionTypeScriptConfig {
    let mut parts = fixture();
    let root = parts.anchor.parent().unwrap().join("fixtures");
    parts.rows = [
        ("observer", "session-observer"),
        ("second", "session-observer-second"),
    ]
    .into_iter()
    .map(|(id, module)| {
        json!({
            "id":id, "name":root.join(format!("{module}.ts")), "provides":{},
            "config":{"lifecycleFile":lifecycle_file},
        })
    })
    .collect();
    parts
}

/// Drain real bootstrap broadcasts before publication so the loading-time
/// sampling assertion does not depend on scheduler timing.
struct DrainLoadingBroadcast {
    dispatcher: rutis::host::Key<dim_agent_runtime::Dispatcher>,
    identity: rutis::host::Key<dim_agent_runtime::TimelineIdentity>,
    lifecycle: rutis::host::Key<dim_agent_runtime::SessionLifecycle>,
}
impl DrainLoadingBroadcast {
    fn new(names: &dim_agent_runtime::Names<'_>) -> rutis::Result<Self> {
        Ok(Self {
            dispatcher: names.key()?,
            identity: names.key()?,
            lifecycle: names.key()?,
        })
    }
}
impl rutis::host::DynPlugin for DrainLoadingBroadcast {
    fn name(&self) -> &str {
        "test.drain-loading-broadcast"
    }
    fn injects(&self) -> Vec<rutis::host::TypeKey> {
        vec![
            self.dispatcher.erase(),
            self.identity.erase(),
            self.lifecycle.erase(),
        ]
    }
    fn apply<'a>(
        &'a self,
        ctx: &'a rutis::Ctx,
    ) -> rutis::host::BoxFuture<'a, rutis::Result<rutis::host::Effect>> {
        Box::pin(async move {
            let dispatcher = ctx.require_at(&self.dispatcher)?;
            let identity = ctx.require_at(&self.identity)?;
            let lifecycle = ctx.require_at(&self.lifecycle)?;
            let emitter = ctx.clone();
            let channel =
                dim_agent_capabilities::session_broadcast::session_channel(&identity.session_id);
            let drained = Arc::new(tokio::sync::Notify::new());
            let fence = drained.clone();
            ctx.listen::<dim_agent_capabilities::session_broadcast::SessionBroadcast>()
                .name(channel.clone())
                .on(move |event, _| {
                    if matches!(&event.record, dim_agent_capabilities::session_broadcast::SessionBroadcastEvent::Fence { recipient, .. } if recipient == "test.loading")
                    {
                        fence.notify_one();
                    }
                    async { Ok(None) }
                })?;
            let session_id = identity.session_id.clone();
            dispatcher.subscribe_all(ctx, self.name(), move |_| {
                let emitter = emitter.clone();
                let session_id = session_id.clone();
                let lifecycle = lifecycle.clone();
                let drained = drained.clone();
                Box::pin(async move {
                    if lifecycle.phase() == dim_agent_runtime::SessionPhase::Loading {
                        emitter.emit(dim_agent_capabilities::session_broadcast::SessionBroadcast::new(
                            &session_id,
                            dim_agent_capabilities::session_broadcast::SessionBroadcastEvent::Fence {
                                recipient: "test.loading".into(), token: 0,
                            },
                        )).map_err(|error| dim_agent_runtime::AgentError::fault("bootstrap fence", error.to_string()))?;
                        drained.notified().await;
                    }
                    Ok(())
                })
            })?;
            Ok(rutis::host::Effect::Done)
        })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires DIM_TEST_NODE and DIM_TEST_JS_RUNTIME"]
async fn node_session_typescript_broadcast_isolation_fences_and_cleanup() {
    let home = std::env::temp_dir().join(format!(
        "dm-c4-{}",
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    ));
    std::fs::create_dir_all(&home).unwrap();
    let audit = home.join("observers.jsonl");
    let (mut builder, _requests, _release) = setup();
    let (send, mut controls) = tokio::sync::mpsc::unbounded_channel();
    builder.require_feature(ControlledGate::feature(send));
    builder.require_feature(
        Feature::new("test.drain-loading-broadcast")
            .branch("test.drain-loading-broadcast", |names| {
                Ok(Box::new(DrainLoadingBroadcast::new(names)?))
            }),
    );
    let mut parts = observers(&audit);
    parts.rows.extend(fixture().rows);
    let server = serve(
        builder,
        ServerConfig {
            unix_path: Some(std::env::temp_dir().join(format!(
                "dm-ts-{}/server.sock",
                &uuid::Uuid::new_v4().simple().to_string()[..8]
            ))),
            database_path: Some(home.join("sessions.sqlite")),
            session_typescript: Some(parts),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let mut client = Client::new_version(&server, false, 3).await;
    let first = new_session(&mut client, json!(["echo"])).await;
    let (session_instance, ctx, _, _) = controls.recv().await.unwrap();
    let second = new_session(&mut client, json!(["echo"])).await;
    let first_session = server.agent().unwrap().get_session(&first).unwrap();
    let first_after = first_session.sequence("main").unwrap();
    let second_after = server
        .agent()
        .unwrap()
        .get_session(&second)
        .unwrap()
        .sequence("main")
        .unwrap();
    let initial = when_served(
        &mut client,
        "session.observer.status",
        json!({"sessionId":first}),
    )
    .await;
    assert_ok(&initial);
    let loading: Vec<Value> = first_session
        .local("main", ..=first_after)
        .await
        .unwrap()
        .iter()
        .map(|event| json!({"sessionId":first,"branchId":"main","event":event}))
        .collect();
    assert!(
        loading.ends_with(initial["result"]["records"].as_array().unwrap()),
        "{initial}"
    );
    assert_eq!(initial["result"]["binding"]["sessionId"], first);
    assert!(
        initial["result"]["binding"]["config"]
            .get("interactive")
            .is_none()
    );
    assert_eq!(
        initial["result"]["binding"]["config"]["collaborationRole"],
        "leader"
    );
    let methods = client
        .rpc("session.typescript.status", json!({"sessionId":first}))
        .await;
    assert_ok(&methods);
    server.restart("Agents").await.unwrap();
    for (namespace, before) in [("observer", &initial), ("typescript", &methods)] {
        let after = client
            .rpc(
                &format!("session.{namespace}.status"),
                json!({"sessionId":first}),
            )
            .await;
        assert_ok(&after);
        assert_eq!(
            after["result"]["generation"], before["result"]["generation"],
            "an index reload must not reload Session rows"
        );
    }
    assert_ok(
        &client
            .rpc("session.attach", json!({"sessionId":first}))
            .await,
    );
    assert_ok(
        &client
            .rpc("session.observer.arm", json!({"sessionId":first}))
            .await,
    );
    submit(&mut client, &first, "held leaf").await;
    assert_ok(
        &client
            .rpc("session.observer.entered", json!({"sessionId":first}))
            .await,
    );
    assert!(
        client
            .notices
            .iter()
            .any(|notice| notice["method"] == "session.event"
                && notice["params"]["event"]["type"] == "step"
                && notice["params"]["event"]["name"] == "text"),
        "native notifications/fence must pass the stalled leaf"
    );
    submit(&mut client, &second, "other session proceeds").await;
    let second_session = server.agent().unwrap().get_session(&second).unwrap();
    let second_through = second_session.sequence("main").unwrap();
    let other = client
        .rpc(
            "session.observer.through",
            json!({"sessionId":second,"sequence":second_through,"after":second_after}),
        )
        .await;
    assert_ok(&other);
    assert_eq!(other["result"]["pid"], initial["result"]["pid"]);
    assert!(
        other["result"]["records"]
            .as_array()
            .unwrap()
            .iter()
            .all(|record| record["sessionId"] == second)
    );
    assert_ok(
        &client
            .rpc("session.observer.release", json!({"sessionId":first}))
            .await,
    );
    let through = first_session.sequence("main").unwrap();
    let expected: Vec<Value> = first_session
        .local("main", first_after + 1..)
        .await
        .unwrap()
        .iter()
        .map(|event| json!({"sessionId":first,"branchId":"main","event":event}))
        .collect();
    for namespace in ["observer", "second"] {
        let observed = client
            .rpc(
                &format!("session.{namespace}.through"),
                json!({"sessionId":first,"sequence":through,"after":first_after}),
            )
            .await;
        assert_ok(&observed);
        assert_eq!(observed["result"]["records"], json!(expected));
    }
    let child = first_session
        .branch(
            "main".into(),
            through,
            dim_agent_runtime::BranchOptions {
                prompt: "child branch observation".into(),
                source: dim_agent_types::BranchSource::Host {
                    operation_id: "observation".into(),
                },
            },
        )
        .await
        .unwrap();
    first_session.idle(&child).await.unwrap();
    let child_history = first_session.local(&child, ..).await.unwrap();
    let child_records = client
        .rpc(
            "session.observer.through",
            json!({"sessionId":first,
        "branchId":child,"sequence":first_session.sequence(&child).unwrap()}),
        )
        .await;
    assert_ok(&child_records);
    let actual_child: Vec<_> = child_records["result"]["records"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|record| record["branchId"] == child)
        .cloned()
        .collect();
    let expected_child: Vec<_> = child_history
        .iter()
        .map(|event| json!({"sessionId":first,"branchId":child,"event":event}))
        .collect();
    assert_eq!(actual_child, expected_child);
    let view = session_copy(
        &ctx,
        session_instance,
        "SessionObserverFixture(session.observer)",
    )
    .await;
    tokio::time::timeout(Duration::from_secs(5), view.restart())
        .await
        .unwrap()
        .unwrap();
    let mut state = view.watch();
    tokio::time::timeout(Duration::from_secs(5), async {
        while state.borrow().state != rutis::host::FiberState::Active {
            state.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    let reloaded = client
        .rpc("session.observer.status", json!({"sessionId":first}))
        .await;
    assert_ok(&reloaded);
    assert_ne!(
        reloaded["result"]["generation"],
        initial["result"]["generation"]
    );
    assert_eq!(
        reloaded["result"]["records"],
        json!([]),
        "completed emissions are not replayed"
    );
    let audit_rows = || {
        std::fs::read_to_string(&audit)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .collect::<Vec<_>>()
    };
    assert!(
        audit_rows()
            .iter()
            .any(|row| row["generation"] == initial["result"]["generation"]
                && row["closed"] == true
                && row["settled"] == true)
    );
    assert_ok(
        &client
            .rpc("session.observer.throw", json!({"sessionId":first}))
            .await,
    );
    submit(&mut client, &first, "listener failure is local").await;
    assert_ok(
        &client
            .rpc(
                "session.observer.through",
                json!({"sessionId":first,"sequence":first_session.sequence("main").unwrap()}),
            )
            .await,
    );
    assert_ok(
        &client
            .rpc("session.observer.arm", json!({"sessionId":first}))
            .await,
    );
    submit(&mut client, &first, "close while a listener is held").await;
    assert_ok(
        &client
            .rpc("session.observer.entered", json!({"sessionId":first}))
            .await,
    );
    let before_close = client
        .rpc("session.observer.status", json!({"sessionId":first}))
        .await;
    assert_ok(&before_close);
    assert_ok(
        &client
            .rpc("session.close", json!({"sessionId":first}))
            .await,
    );
    assert!(
        audit_rows()
            .iter()
            .any(|row| row["generation"] == reloaded["result"]["generation"]
                && row["closed"] == true
                && row["settled"] == true
                && row["count"].as_u64().unwrap()
                    >= before_close["result"]["records"].as_array().unwrap().len() as u64)
    );
    submit(&mut client, &second, "survives the other Session close").await;
    assert_ok(
        &client
            .rpc(
                "session.observer.through",
                json!({"sessionId":second,"sequence":second_session.sequence("main").unwrap()}),
            )
            .await,
    );
    assert!(second_session.unload_if_idle().await.unwrap());
    let restored = when_served(
        &mut client,
        "session.observer.status",
        json!({"sessionId":second}),
    )
    .await;
    assert_ok(&restored);
    assert_ne!(
        restored["result"]["generation"],
        other["result"]["generation"]
    );
    let restored_session = server.agent().unwrap().get_session(&second).unwrap();
    let restoring: Vec<Value> = restored_session
        .local("main", ..)
        .await
        .unwrap()
        .iter()
        .map(|event| json!({"sessionId":second,"branchId":"main","event":event}))
        .collect();
    assert!(
        restoring.ends_with(restored["result"]["records"].as_array().unwrap()),
        "{restored}"
    );
    submit(&mut client, &second, "restored channel receives new facts").await;
    let current = server.agent().unwrap().get_session(&second).unwrap();
    assert_ok(
        &client
            .rpc(
                "session.observer.through",
                json!({"sessionId":second,"sequence":current.sequence("main").unwrap()}),
            )
            .await,
    );
    assert_ok(
        &client
            .rpc("session.observer.arm", json!({"sessionId":second}))
            .await,
    );
    submit(&mut client, &second, "shutdown with pending delivery").await;
    assert_ok(
        &client
            .rpc("session.observer.entered", json!({"sessionId":second}))
            .await,
    );
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// A Session row's `sessionLifecycle.hold()` (plugin contract R2) is a hold
/// its row owns: the Session cannot be released while it lasts, `release`
/// ends it once, and the row's unload ends the holds it still has.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires DIM_TEST_NODE and DIM_TEST_JS_RUNTIME"]
async fn node_session_row_hold_lasts_until_released_or_its_row_unloads() {
    let home = std::env::temp_dir().join(format!(
        "dm-hold-{}",
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    ));
    std::fs::create_dir_all(&home).unwrap();
    let audit = home.join("observers.jsonl");
    let (mut builder, _requests, _) = setup();
    let (send, mut controls) = tokio::sync::mpsc::unbounded_channel();
    builder.require_feature(ControlledGate::feature(send));
    let server = serve(
        builder,
        ServerConfig {
            unix_path: Some(std::env::temp_dir().join(format!(
                "dm-ts-{}/server.sock",
                &uuid::Uuid::new_v4().simple().to_string()[..8]
            ))),
            database_path: Some(home.join("sessions.sqlite")),
            session_typescript: Some(observers(&audit)),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let mut client = Client::new_version(&server, false, 3).await;
    async fn call(client: &mut Client, method: &str, params: Value) -> Value {
        let reply = client.rpc(method, params).await;
        assert_ok(&reply);
        reply["result"].clone()
    }

    let id = new_session(&mut client, json!(["echo"])).await;
    controls.recv().await.unwrap();
    let session = server.agent().unwrap().get_session(&id).unwrap();
    let hold = when_served(
        &mut client,
        "session.observer.hold_session",
        json!({"sessionId":id}),
    )
    .await["result"]
        .clone();
    assert!(hold.is_u64(), "{hold}");
    assert!(!session.unload_if_idle().await.unwrap());
    let release = json!({"sessionId":id, "id":hold});
    assert_eq!(
        call(
            &mut client,
            "session.observer.release_session",
            release.clone()
        )
        .await,
        json!(true)
    );
    assert_eq!(
        call(&mut client, "session.observer.release_session", release).await,
        json!(false)
    );
    assert!(session.unload_if_idle().await.unwrap());

    let id = new_session(&mut client, json!(["echo"])).await;
    let (session_instance, ctx, _, _) = controls.recv().await.unwrap();
    let session = server.agent().unwrap().get_session(&id).unwrap();
    let hold = when_served(
        &mut client,
        "session.observer.hold_session",
        json!({"sessionId":id}),
    )
    .await["result"]
        .clone();
    assert!(hold.is_u64(), "{hold}");
    assert!(!session.unload_if_idle().await.unwrap());
    let view = session_copy(
        &ctx,
        session_instance,
        "SessionObserverFixture(session.observer)",
    )
    .await;
    tokio::time::timeout(Duration::from_secs(5), view.restart())
        .await
        .unwrap()
        .unwrap();
    let release = json!({"sessionId":id, "id":hold});
    assert_eq!(
        call(&mut client, "session.observer.release_session", release).await,
        json!(false)
    );
    assert!(session.unload_if_idle().await.unwrap());
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// Idle unload §13 item 4, the C4 part: with a grace of
/// 200 ms, a Session that Session TypeScript parts observe unloads once
/// nothing holds it, the parts' generation ends, and nothing keeps the
/// released instance alive, even when the Session's own leaf is stalled in a
/// listener: that leaf's generation ends with the unload, so the listener
/// returns. Restored, the Session's new events reach the observers again,
/// and the restored instance is released in the same way.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires DIM_TEST_NODE and DIM_TEST_JS_RUNTIME"]
async fn node_session_typescript_observes_a_session_across_idle_unloads() {
    let home = std::env::temp_dir().join(format!(
        "dm-iu-{}",
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    ));
    std::fs::create_dir_all(&home).unwrap();
    let audit = home.join("observers.jsonl");
    let (mut builder, _requests, _release) = setup();
    let loads = super::sessions::idle_unload::Loads::default();
    loads.install(&mut builder);
    let server = serve(
        builder,
        ServerConfig {
            unix_path: Some(home.join("run/server.sock")),
            database_path: Some(home.join("sessions.sqlite")),
            session_typescript: Some(observers(&audit)),
            idle_unload_after: Duration::from_millis(200),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let mut client = Client::new_version(&server, false, 3).await;
    let id = new_session(&mut client, json!(["echo"])).await;
    let attach = json!({"sessionId":id});
    for (round, text) in ["before the unload", "after the restore"]
        .into_iter()
        .enumerate()
    {
        assert_ok(&client.rpc("session.attach", attach.clone()).await);
        assert_eq!(loads.of(&id), (round + 1, round == 1));
        let session = server.agent().unwrap().get_session(&id).unwrap();
        let after = session.sequence("main").unwrap();
        let status = when_served(
            &mut client,
            "session.observer.status",
            json!({"sessionId":id}),
        )
        .await;
        assert_ok(&status);
        submit(&mut client, &id, text).await;
        let through = session.sequence("main").unwrap();
        let expected: Vec<Value> = session
            .local("main", after + 1..)
            .await
            .unwrap()
            .iter()
            .map(|event| json!({"sessionId":id,"branchId":"main","event":event}))
            .collect();
        for namespace in ["observer", "second"] {
            let observed = client
                .rpc(
                    &format!("session.{namespace}.through"),
                    json!({"sessionId":id,"sequence":through,"after":after}),
                )
                .await;
            assert_ok(&observed);
            assert_eq!(observed["result"]["records"], json!(expected));
        }
        assert_ok(&client.rpc("session.detach", attach.clone()).await);
        unloaded(session).await;
        loads.released(&id).await;
        let generation = &status["result"]["generation"];
        assert!(
            std::fs::read_to_string(&audit)
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str::<Value>(line).unwrap())
                .any(|row| row["generation"] == *generation && row["closed"] == true),
            "the observer generation {generation} ended with the unload"
        );
    }
    assert_ok(&client.rpc("session.attach", attach.clone()).await);
    let session = server.agent().unwrap().get_session(&id).unwrap();
    assert_ok(&when_served(&mut client, "session.observer.arm", json!({"sessionId":id})).await);
    submit(&mut client, &id, "stalled leaf").await;
    assert_ok(
        &client
            .rpc("session.observer.entered", json!({"sessionId":id}))
            .await,
    );
    assert_ok(&client.rpc("session.detach", attach.clone()).await);
    unloaded(session).await;
    loads.released(&id).await;
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires DIM_TEST_NODE and DIM_TEST_JS_RUNTIME"]
async fn node_session_typescript_creation_config_and_misplaced_rows() {
    let audit = std::env::temp_dir().join(format!("dim-c4-config-{}.jsonl", uuid::Uuid::new_v4()));
    let (server, _requests, _) = start(ServerConfig {
        session_typescript: Some(observers(&audit)),
        ..Default::default()
    })
    .await;
    for (interactive, role) in [
        (true, dim_agent_runtime::CollaborationRole::Leader),
        (false, dim_agent_runtime::CollaborationRole::Leader),
        (true, dim_agent_runtime::CollaborationRole::Member),
        (false, dim_agent_runtime::CollaborationRole::Member),
    ] {
        let session = server
            .agent()
            .unwrap()
            .create_session(dim_agent_runtime::SessionConfig {
                cwd: std::env::temp_dir().to_str().unwrap().into(),
                tools: Some(vec!["echo".into()]),
                interactive,
                collaboration_role: role,
                ..Default::default()
            })
            .await
            .unwrap();
        let methods = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match session
                    .session_service::<SessionMethods>()
                    .map(|table| table.provider("session.observer.status"))
                {
                    Ok(Some(methods)) => break methods,
                    Ok(None) | Err(dim_agent_runtime::AgentError::Reloading(_)) => {
                        tokio::task::yield_now().await;
                    }
                    Err(error) => panic!("observer methods: {error}"),
                }
            }
        })
        .await
        .expect("observer methods publication");
        let status = methods
            .accept(
                "session.observer.status".into(),
                json!({"sessionId":session.id()}),
                CancellationToken::new(),
                |_| {},
            )
            .unwrap()
            .reply
            .await
            .unwrap()
            .unwrap();
        assert_eq!(status["binding"]["config"], json!(session.config()));
        assert_eq!(
            status["eligible"],
            interactive && role == dim_agent_runtime::CollaborationRole::Leader
        );
        session.close().await.unwrap();
    }
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_file(&audit).unwrap();

    {
        let mut parts = observers(&audit);
        let root = parts.anchor.parent().unwrap().join("fixtures");
        parts.rows[0] = json!({
            "id":"observer", "name":root.join("misplaced-branch-part.ts"),
            "provides":{}, "config":{},
        });
        let (mut builder, mut requests, _) = setup();
        let (send, mut controls) = tokio::sync::mpsc::unbounded_channel();
        builder.require_feature(ControlledGate::feature(send));
        let server = serve(
            builder,
            ServerConfig {
                unix_path: Some(std::env::temp_dir().join(format!(
                    "dm-ts-{}/server.sock",
                    &uuid::Uuid::new_v4().simple().to_string()[..8]
                ))),
                session_typescript: Some(parts),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let mut client = Client::new_version(&server, false, 3).await;
        let id = new_session(&mut client, json!(["echo"])).await;
        let (session_instance, _, loader, _) = controls.recv().await.unwrap();
        let row = loader
            .entries()
            .into_iter()
            .find(|entry| entry.id == "observer" && entry.instance.is_none())
            .unwrap()
            .view
            .unwrap();
        let mut state = row.watch();
        tokio::time::timeout(Duration::from_secs(10), async {
            while state.borrow_and_update().state != rutis::host::FiberState::Active {
                state.changed().await.unwrap();
            }
        })
        .await
        .unwrap_or_else(|_| panic!("the registering row is Active: {:?}", row.state()));
        assert_ok(
            &when_served(
                &mut client,
                "session.second.status",
                json!({"sessionId":id}),
            )
            .await,
        );
        submit(&mut client, &id, "native continues after a misplaced row").await;
        assert!(requests.recv().await.unwrap().contains(&"echo".into()));
        let sequence = server
            .agent()
            .unwrap()
            .get_session(&id)
            .unwrap()
            .sequence("main")
            .unwrap();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        let reply = loop {
            let reply = client
                .rpc(
                    "session.second.through",
                    json!({"sessionId":id,"sequence":sequence}),
                )
                .await;
            if !waiting(&reply) {
                break reply;
            }
            if tokio::time::Instant::now() >= deadline {
                let rows: Vec<_> = loader
                    .entries()
                    .into_iter()
                    .filter(|entry| {
                        entry.instance.as_ref().is_some_and(|instance| {
                            instance.chain.iter().any(|(_, id)| *id == session_instance)
                        })
                    })
                    .map(|entry| format!("{} {:?}", entry.id, entry.status))
                    .collect();
                panic!("session.second.through is not served; rows: {rows:?}");
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        };
        assert_ok(&reply);
        let agent = server.agent().unwrap();
        assert_eq!(
            super::branch_typescript::count(&agent, &id, "main", "<c5-marker").await,
            0
        );
        server.shutdown();
        server.join().await.unwrap();
    }
    std::fs::remove_file(audit).unwrap();
}

struct CheckedModel {
    names: tokio::sync::mpsc::UnboundedSender<Vec<String>>,
    /// The full definitions each request carries; a dropped receiver is fine.
    tools: tokio::sync::mpsc::UnboundedSender<Vec<ToolDefinition>>,
}
impl ModelDriver for CheckedModel {
    fn invoke(
        &self,
        request: Arc<ModelRequest>,
        connection: ModelConnection,
        cancel: CancellationToken,
    ) -> HostFuture<'_, ModelStream> {
        self.names
            .send(
                request
                    .tools
                    .as_deref()
                    .into_iter()
                    .flatten()
                    .map(|tool| tool.name.clone())
                    .collect(),
            )
            .unwrap();
        let tools: Vec<ToolDefinition> = request
            .tools
            .as_deref()
            .into_iter()
            .flatten()
            .cloned()
            .collect();
        let _ = self.tools.send(tools);
        let batch = request.messages.last().is_some_and(|message| {
            message.role == Role::User
                && match &message.content {
                    MessageContent::Text(text) => text == "batch",
                    MessageContent::Parts(parts) => parts.iter().any(
                        |part| matches!(part, MessagePart::Text { text, .. } if text == "batch"),
                    ),
                }
        });
        if batch {
            return Box::pin(async {
                let chunks = vec![
                    ModelChunk::MessageStart {
                        message_id: "batch".into(),
                    },
                    ModelChunk::ToolStart {
                        id: "fast".into(),
                        name: "ts_echo".into(),
                    },
                    ModelChunk::ToolArguments {
                        id: "fast".into(),
                        name: "ts_echo".into(),
                        delta: r#"{"text":"fast"}"#.into(),
                    },
                    ModelChunk::ToolUse(ToolCall {
                        id: "fast".into(),
                        name: "ts_echo".into(),
                        input: serde_json::from_value(json!({"text":"fast"})).unwrap(),
                    }),
                    ModelChunk::ToolStart {
                        id: "slow".into(),
                        name: "native_slow".into(),
                    },
                    ModelChunk::ToolArguments {
                        id: "slow".into(),
                        name: "native_slow".into(),
                        delta: "{}".into(),
                    },
                    ModelChunk::ToolUse(ToolCall {
                        id: "slow".into(),
                        name: "native_slow".into(),
                        input: Default::default(),
                    }),
                    ModelChunk::MessageStop {
                        message_id: "batch".into(),
                        reason: ModelStopReason::ToolUse,
                    },
                ];
                Ok(Box::pin(futures_util::stream::iter(chunks.into_iter().map(Ok))) as ModelStream)
            });
        }
        EchoModel.invoke(request, connection, cancel)
    }
}

struct SlowTools(Arc<tokio::sync::Notify>);
impl ToolDriver for SlowTools {
    fn definitions(&self) -> Vec<ToolDefinition> {
        let mut tools = MockTools.definitions();
        tools.push(definition(
            "native_slow",
            "Held batch tool",
            json!({"type":"object"}),
        ));
        tools
    }
    fn execute(
        &self,
        call: ToolCall,
        context: ToolExecutionContext,
        cancel: CancellationToken,
    ) -> HostFuture<'_, ToolResult> {
        if call.name != "native_slow" {
            return MockTools.execute(call, context, cancel);
        }
        Box::pin(async move {
            self.0.notified().await;
            assert!(
                !cancel.is_cancelled(),
                "another tool's normal completion cancelled this batch"
            );
            Ok(ToolResult::text("native slow completed"))
        })
    }
}

/// The tool names each model request offers.
type ModelRequests = tokio::sync::mpsc::UnboundedReceiver<Vec<String>>;
/// The full tool definitions each model request offers.
type RecordedTools = tokio::sync::mpsc::UnboundedReceiver<Vec<ToolDefinition>>;

pub(super) async fn start(
    config: ServerConfig,
) -> (RunningServer, ModelRequests, Arc<tokio::sync::Notify>) {
    let (server, requests, release, _) = start_recording(config).await;
    (server, requests, release)
}

/// `start` that also returns the full tool definitions each model request
/// carries.
pub(super) async fn start_recording(
    mut config: ServerConfig,
) -> (
    RunningServer,
    ModelRequests,
    Arc<tokio::sync::Notify>,
    RecordedTools,
) {
    config.unix_path = Some(std::env::temp_dir().join(format!(
        "dm-ts-{}/server.sock",
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    )));
    let (builder, requests, release, tools) = setup_recording();
    let server = serve(builder, config).await.unwrap();
    (server, requests, release, tools)
}

pub(super) fn setup() -> (AgentBuilder, ModelRequests, Arc<tokio::sync::Notify>) {
    let (builder, requests, release, _) = setup_recording();
    (builder, requests, release)
}

fn setup_recording() -> (
    AgentBuilder,
    ModelRequests,
    Arc<tokio::sync::Notify>,
    RecordedTools,
) {
    let (names, requests) = tokio::sync::mpsc::unbounded_channel();
    let (tools, recorded) = tokio::sync::mpsc::unbounded_channel();
    let release = Arc::new(tokio::sync::Notify::new());
    let mut builder = AgentBuilder::new(AgentConfig::default());
    defaults::install(&mut builder);
    builder
        .session_value(Arc::new(HistoryRenderer::default()))
        .process_value(Arc::new(ModelHost(Arc::new(CheckedModel { names, tools }))))
        .session_value(Arc::new(PrepareContextHost(Arc::new(IdentityContext))))
        .session_value(Arc::new(ContextModelHost(None)))
        .session_value(Arc::new(CompactionStrategyHost(None)))
        .require_feature(
            Feature::new("PrepareModelHost").session("PrepareModelHost", |names| {
                Ok(Box::new(DefaultPrepareModelPlugin::new(names)?))
            }),
        );
    defaults::tool_host(&mut builder, Arc::new(SlowTools(release.clone())));
    (builder, requests, release, recorded)
}

/// Waits until the configured plugin row `row` is Active, so the parts its
/// `apply` registers exist. A plugin row is an optional extension: `serve`
/// returns while it may still be loading, and a Session that opens before its
/// parts are registered mounts them only after it is published (process
/// TypeScript §2.1, ADR 006 D3).
pub(super) async fn plugin_active(server: &RunningServer, row: &Value) {
    let name = row["name"].as_str().unwrap();
    let loader = server
        .diagnostics()
        .plugins
        .into_iter()
        .find(|plugin| plugin.name == "rutis-loader")
        .expect("the process loader")
        .id;
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if server.diagnostics().plugins.iter().any(|plugin| {
                plugin.name == name
                    && plugin.parent == Some(loader)
                    && plugin.state == rutis::host::FiberState::Active
            }) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("plugin row {name} is not Active"));
}

/// The reply of `method` once the optional row that serves it has registered
/// it: a Session is published without waiting for its optional rows, and a
/// namespace is `unknown_method` until its row registers it.
pub(super) async fn when_served(client: &mut Client, method: &str, params: Value) -> Value {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let reply = client.rpc(method, params.clone()).await;
            if !waiting(&reply) {
                return reply;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{method} is served"))
}

/// Reconciles the loader's layers with a fixture layer `name` of
/// `patches` last, or without it.
pub(super) async fn fixture_layer(
    loader: &rutis_loader::Loader,
    name: &str,
    patches: Option<Vec<rutis_loader::Patch>>,
) -> rutis_loader::report::ReconcileReport {
    let mut layers = loader.layers();
    layers.retain(|layer| layer.name != name);
    layers.extend(patches.map(|patches| rutis_loader::Layer::new(name, patches)));
    loader.reconcile(layers, None).await.unwrap()
}

/// Whether `reply` says the row that serves its method is not serving yet.
pub(super) fn waiting(reply: &Value) -> bool {
    ["unknown_method", "service_reloading"].contains(
        &reply["error"]["data"]["appError"]
            .as_str()
            .unwrap_or_default(),
    )
}

/// The copies of the registered part `part` that the Session `session`
/// mounted so far: a part registered by a TypeScript row is a plugin
/// definition, and a mounter of that Session mounts its own copy. The mounter
/// is a child of the Session's scope row, so the walk reads the mounter's
/// enclosing fibers rather than its immediate parent.
pub(super) fn session_copies(
    ctx: &rutis::Ctx,
    session: InstanceId,
    part: &str,
) -> Vec<rutis::host::FiberView> {
    let plugins = ctx.diagnostics().plugins;
    let by_id = |id: rutis::host::PluginId| plugins.iter().find(|plugin| plugin.id == id);
    let in_session = |mut id: Option<rutis::host::PluginId>| {
        while let Some(enclosing) = id.and_then(by_id) {
            if enclosing.instance == session {
                return true;
            }
            id = enclosing.parent;
        }
        false
    };
    plugins
        .iter()
        .filter(|plugin| plugin.name == part)
        .filter(|plugin| {
            plugin
                .parent
                .and_then(by_id)
                .filter(|mounter| mounter.name.starts_with("Session"))
                .is_some_and(|mounter| in_session(mounter.parent))
        })
        .filter_map(|plugin| ctx.view(plugin.id))
        .collect()
}

/// The one copy of `part` in the Session `session`, once it is mounted.
pub(super) async fn session_copy(
    ctx: &rutis::Ctx,
    session: InstanceId,
    part: &str,
) -> rutis::host::FiberView {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(view) = session_copies(ctx, session, part).pop() {
                return view;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("the Session mounts a copy of {part}"))
}

pub(super) async fn new_session(client: &mut Client, tools: Value) -> String {
    let mut params = create(tools);
    params["permissionPolicy"]["allowedTools"] = json!(["ts_echo", "native_slow", "echo", "wait"]);
    let created = client.rpc("session.create", params).await;
    assert_ok(&created);
    created["result"]["sessionId"].as_str().unwrap().into()
}

pub(super) async fn submit(client: &mut Client, session: &str, text: &str) {
    assert_ok(
        &client
            .rpc(
                "session.submit",
                json!({"sessionId":session,"branchId":"main","text":text}),
            )
            .await,
    );
    assert_ok(
        &client
            .rpc(
                "session.idle",
                json!({"sessionId":session,"branchId":"main"}),
            )
            .await,
    );
}

async fn started(client: &mut Client, id: u64) {
    loop {
        let notice = client.notice("management.progress").await;
        if notice["params"]["id"] == id && notice["params"]["data"]["phase"] == "started" {
            return;
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires DIM_TEST_NODE and DIM_TEST_JS_RUNTIME"]
async fn node_session_typescript_tools_methods_and_generation_lifetimes() {
    let (server, mut requests, release) = start(ServerConfig {
        session_typescript: Some(fixture()),
        ..Default::default()
    })
    .await;
    plugin_active(&server, &fixture().rows[0]).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let first = new_session(&mut client, json!(["ts_echo", "native_slow", "echo"])).await;
    let second = new_session(&mut client, json!(["ts_echo", "echo"])).await;
    submit(&mut client, &first, r#"tool:ts_echo {"text":"first"}"#).await;
    assert!(requests.recv().await.unwrap().contains(&"ts_echo".into()));
    let status = client
        .rpc("session.typescript.status", json!({"sessionId":first}))
        .await;
    assert_eq!(status["result"]["calls"], 1, "{status}");
    let other = client
        .rpc("session.typescript.status", json!({"sessionId":second}))
        .await;
    assert_eq!(other["result"]["calls"], 0);
    assert_eq!(status["result"]["pid"], other["result"]["pid"]);
    assert_ne!(
        status["result"]["generation"],
        other["result"]["generation"]
    );
    let listed = client
        .rpc("session.methods", json!({"sessionId":first}))
        .await;
    assert!(
        listed["result"]["methods"]
            .as_array()
            .unwrap()
            .iter()
            .any(|decl| decl["name"] == "session.typescript.drain")
    );

    assert_ok(
        &client
            .rpc(
                "session.submit",
                json!({"sessionId":first,"branchId":"main","text":"batch"}),
            )
            .await,
    );
    assert_ok(
        &client
            .rpc("session.typescript.wait_fast", json!({"sessionId":first}))
            .await,
    );
    release.notify_one();
    assert_ok(
        &client
            .rpc("session.idle", json!({"sessionId":first,"branchId":"main"}))
            .await,
    );
    let session = server.agent().unwrap().get_session(&first).unwrap();
    let history = session.history("main", None).await.unwrap();
    let results: Vec<_> = history
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::ToolResult(result) => Some(result),
            _ => None,
        })
        .collect();
    assert!(results.iter().any(|batch| {
        batch.results.iter().any(|result| {
            result.call_id == "slow"
                && result.result.content
                    == dim_agent_types::ToolContent::Text("native slow completed".into())
        })
    }));
    let first_result = &results[0].results[0].result;
    let dim_agent_types::ToolContent::Text(text) = &first_result.content else {
        panic!("text result")
    };
    let call: Value = serde_json::from_str(text).unwrap();
    assert_eq!(call["sessionId"], first);
    assert_eq!(call["branchId"], "main");
    assert_eq!(call["arguments"]["text"], "first");
    assert!(!call["callId"].as_str().unwrap().is_empty());
    assert!(!call["turnId"].as_str().unwrap().is_empty());
    assert!(call["config"]["cwd"].is_string());

    let error = client
        .rpc("session.typescript.error", json!({"sessionId":first}))
        .await;
    assert_eq!(error["error"]["code"], -32000);
    assert_eq!(
        error["error"]["data"]["appError"],
        format!("owned_{}", status["result"]["generation"].as_str().unwrap())
    );
    assert_eq!(
        client
            .rpc("session.typescript.malformed", json!({"sessionId":first}))
            .await["error"]["code"],
        -32603
    );
    let empty = client
        .rpc("session.typescript.undefined", json!({"sessionId":first}))
        .await;
    assert_ok(&empty);
    assert_eq!(empty["result"], Value::Null);
    for method in ["extra_value", "extra_error", "handed_off"] {
        assert_eq!(
            client
                .rpc(
                    &format!("session.typescript.{method}"),
                    json!({"sessionId":first})
                )
                .await["error"]["code"],
            -32603
        );
    }
    let progress_call = client
        .send("session.typescript.status", json!({"sessionId":first}))
        .await;
    let progress = client.read("TypeScript progress before reply").await;
    assert_eq!(progress["method"], "management.progress");
    assert_eq!(progress["params"]["id"], progress_call);
    assert_eq!(progress["params"]["data"]["phase"], "started");
    assert_ok(&client.response(progress_call).await);
    submit(&mut client, &first, r#"tool:ts_echo {"text":"malformed"}"#).await;
    assert!(session.history("main", None).await.unwrap().iter().any(|event| matches!(&event.kind, EventKind::ToolResult(batch) if batch.results.iter().any(|result| result.result.is_error == Some(true)))));

    assert_ok(&client.rpc("session.submit", json!({"sessionId":first,"branchId":"main","text":r#"tool:ts_echo {"text":"cancel"}"#})).await);
    assert_ok(
        &client
            .rpc("session.typescript.wait_tool", json!({"sessionId":first}))
            .await,
    );
    assert_ok(
        &client
            .rpc("session.stop", json!({"sessionId":first,"branchId":"main"}))
            .await,
    );
    assert_ok(
        &client
            .rpc("session.idle", json!({"sessionId":first,"branchId":"main"}))
            .await,
    );

    let retained = session
        .session_service::<SessionMethods>()
        .unwrap()
        .provider("session.typescript.report")
        .unwrap();
    let report = client
        .send("session.typescript.report", json!({"sessionId":first}))
        .await;
    started(&mut client, report).await;
    let drain = client
        .send("session.typescript.drain", json!({"sessionId":first}))
        .await;
    started(&mut client, drain).await;
    let mut closer = Client::new_version(&server, false, 3).await;
    assert_ok(
        &closer
            .rpc("session.close", json!({"sessionId":first}))
            .await,
    );
    assert_eq!(
        client.response(report).await["error"]["data"]["appError"],
        "operation_interrupted"
    );
    let drained = client.response(drain).await;
    assert_eq!(drained["result"]["cancelled"], true, "{drained}");
    assert_eq!(drained["result"]["alive"], true);
    assert_eq!(
        retained
            .accept(
                "session.typescript.status".into(),
                json!({}),
                CancellationToken::new(),
                |_| {}
            )
            .err()
            .unwrap()
            .data
            .unwrap()
            .app_error,
        "service_reloading"
    );
    submit(&mut client, &second, r#"tool:ts_echo {"text":"second"}"#).await;
    assert_eq!(
        client
            .rpc("session.typescript.status", json!({"sessionId":second}))
            .await["result"]["calls"],
        1
    );
    server.shutdown();
    server.join().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires DIM_TEST_NODE and DIM_TEST_JS_RUNTIME"]
async fn node_session_typescript_restore_fork_delete_and_disabled_parts() {
    let home = std::env::temp_dir().join(format!(
        "dm-ts-{}",
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    ));
    let config = ServerConfig {
        database_path: Some(home.join("sessions.sqlite")),
        session_typescript: Some(fixture()),
        ..Default::default()
    };
    let (server, _requests, _) = start(config.clone()).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let session_id = new_session(&mut client, json!(["ts_echo", "echo"])).await;
    submit(&mut client, &session_id, r#"tool:ts_echo {"text":"saved"}"#).await;
    drop(client);
    server.shutdown();
    server.join().await.unwrap();

    let (server, mut requests, _) = start(config.clone()).await;
    plugin_active(&server, &fixture().rows[0]).await;
    assert!(server.agent().unwrap().get_session(&session_id).is_none());
    let mut client = Client::new_version(&server, false, 3).await;
    submit(
        &mut client,
        &session_id,
        r#"tool:ts_echo {"text":"restored"}"#,
    )
    .await;
    assert!(requests.recv().await.unwrap().contains(&"ts_echo".into()));
    assert_eq!(
        client
            .rpc("session.typescript.status", json!({"sessionId":session_id}))
            .await["result"]["calls"],
        1
    );
    let source = server.agent().unwrap().get_session(&session_id).unwrap();
    let forked = client
        .rpc(
            "session.fork",
            json!({"sessionId":session_id,"at":source.sequence("main").unwrap()}),
        )
        .await;
    assert_ok(&forked);
    let fork_id = forked["result"]["sessionId"].as_str().unwrap();
    let source_status = client
        .rpc("session.typescript.status", json!({"sessionId":session_id}))
        .await;
    let fork_status = client
        .rpc("session.typescript.status", json!({"sessionId":fork_id}))
        .await;
    assert_ok(&fork_status);
    assert_eq!(source_status["result"]["pid"], fork_status["result"]["pid"]);
    assert_ne!(
        source_status["result"]["generation"],
        fork_status["result"]["generation"]
    );
    assert_eq!(fork_status["result"]["calls"], 0);
    submit(&mut client, fork_id, r#"tool:ts_echo {"text":"forked"}"#).await;
    assert_eq!(
        client
            .rpc("session.typescript.status", json!({"sessionId":fork_id}))
            .await["result"]["calls"],
        1
    );
    let fork = server.agent().unwrap().get_session(fork_id).unwrap();
    let history = fork.history("main", None).await.unwrap();
    let result = history
        .iter()
        .rev()
        .find_map(|event| match &event.kind {
            EventKind::ToolResult(batch) => batch.results.first(),
            _ => None,
        })
        .unwrap();
    let dim_agent_types::ToolContent::Text(text) = &result.result.content else {
        panic!("text result")
    };
    assert_eq!(
        serde_json::from_str::<Value>(text).unwrap()["sessionId"],
        fork_id
    );
    let drain = client
        .send("session.typescript.drain", json!({"sessionId":fork_id}))
        .await;
    started(&mut client, drain).await;
    let mut deleter = Client::new_version(&server, false, 3).await;
    assert_ok(
        &deleter
            .rpc("session.delete", json!({"sessionId":fork_id}))
            .await,
    );
    assert_eq!(client.response(drain).await["result"]["alive"], true);
    assert_eq!(
        client
            .rpc("session.typescript.status", json!({"sessionId":fork_id}))
            .await["error"]["data"]["appError"],
        "session_closed"
    );
    assert_eq!(
        client
            .rpc("session.typescript.status", json!({"sessionId":session_id}))
            .await["result"]["calls"],
        1
    );
    drop(client);
    server.shutdown();
    server.join().await.unwrap();

    let mut config = config;
    config.session_typescript.as_mut().unwrap().rows[0]["disabled"] = json!(true);
    let (server, mut requests, _) = start(config).await;
    let mut client = Client::new_version(&server, false, 3).await;
    submit(&mut client, &session_id, r#"tool:echo {"text":"native"}"#).await;
    assert!(!requests.recv().await.unwrap().contains(&"ts_echo".into()));
    assert_eq!(
        client
            .rpc("session.typescript.status", json!({"sessionId":session_id}))
            .await["error"]["data"]["appError"],
        "unknown_method"
    );
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// The Session's instance, the gate's context, the loader and the registry.
pub(super) type Control = (
    InstanceId,
    rutis::Ctx,
    Arc<rutis_loader::Loader>,
    Arc<dim_agent_capabilities::tool_registry::ToolRegistry>,
);
pub(super) struct ControlledGate {
    instance: InstanceId,
    controls: tokio::sync::mpsc::UnboundedSender<Control>,
    loader: rutis::host::Key<rutis_loader::Loader>,
    rows: rutis::host::Key<rutis_loader::runtime::SessionRows>,
    registry: rutis::host::Key<dim_agent_capabilities::tool_registry::ToolRegistry>,
}
impl ControlledGate {
    fn new(
        names: &dim_agent_runtime::Names<'_>,
        controls: tokio::sync::mpsc::UnboundedSender<Control>,
    ) -> rutis::Result<Self> {
        Ok(Self {
            instance: names.instance(SESSION_GROUP)?,
            controls,
            loader: names.key()?,
            rows: rutis_loader::runtime::SessionRows::key("session-node"),
            registry: names.key()?,
        })
    }
    /// A feature whose Session part sends each Session's [`Control`].
    pub(super) fn feature(controls: tokio::sync::mpsc::UnboundedSender<Control>) -> Feature {
        Feature::new("ControlledTypeScriptDependency")
            .session("ControlledTypeScriptDependency", move |names| {
                Ok(Box::new(Self::new(names, controls.clone())?))
            })
    }
}
impl rutis::host::DynPlugin for ControlledGate {
    fn name(&self) -> &str {
        "ControlledTypeScriptDependency"
    }
    fn injects(&self) -> Vec<rutis::host::TypeKey> {
        vec![
            self.loader.erase(),
            self.rows.erase(),
            self.registry.erase(),
        ]
    }
    fn apply<'a>(
        &'a self,
        ctx: &'a rutis::Ctx,
    ) -> rutis::host::BoxFuture<'a, rutis::Result<rutis::host::Effect>> {
        Box::pin(async move {
            let loader = ctx.require_at(&self.loader)?;
            let registry = ctx.require_at(&self.registry)?;
            self.controls
                .send((self.instance, ctx.clone(), loader, registry))
                .unwrap();
            Ok(rutis::host::Effect::Done)
        })
    }
}

struct GateValue;
impl rutis_bridge::session::HostDispatch for GateValue {
    fn invoke(&self, _: &str, _: rutis_bridge::session::Value) -> rutis_bridge::session::Reply {
        Err(rutis_bridge::session::Error::Value(
            "fixture dependency is not a tool".into(),
        ))
    }
    fn methods(&self) -> Option<Value> {
        Some(json!({"declaration":"sync", "execute":"async"}))
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires DIM_TEST_NODE and DIM_TEST_JS_RUNTIME"]
async fn node_session_typescript_pending_parts_keep_native_sessions_available() {
    let mut parts = fixture();
    let entry = PathBuf::from(parts.rows.remove(0)["name"].as_str().unwrap());
    parts.rows.push(json!({"id":"typescript", "name":entry.with_file_name("session-late.ts"), "provides":{}, "instanceNames":{"lateGate":"session"}}));
    let (mut builder, mut requests, _) = setup();
    let (send, mut controls) = tokio::sync::mpsc::unbounded_channel();
    builder.require_feature(ControlledGate::feature(send));
    let server = serve(
        builder,
        ServerConfig {
            unix_path: Some(std::env::temp_dir().join(format!(
                "dm-ts-{}/server.sock",
                &uuid::Uuid::new_v4().simple().to_string()[..8]
            ))),
            session_typescript: Some(parts),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let mut client = Client::new_version(&server, false, 3).await;
    let session = new_session(&mut client, json!(["late", "echo"])).await;
    let (session_instance, ctx, _, _) = controls.recv().await.unwrap();
    let view = session_copy(&ctx, session_instance, "LateSessionTypeScriptFixture").await;
    assert_eq!(view.state().state, rutis::host::FiberState::Pending);
    assert_eq!(
        client
            .rpc("session.late.status", json!({"sessionId":session}))
            .await["error"]["data"]["appError"],
        "unknown_method"
    );
    submit(&mut client, &session, r#"tool:echo {"text":"before"}"#).await;
    assert!(!requests.recv().await.unwrap().contains(&"late".into()));
    ctx.provide_at::<dyn rutis_bridge::session::HostDispatch>(
        rutis_bridge::session::host_key_in("lateGate", session_instance),
        Arc::new(GateValue),
    )
    .unwrap();
    let mut state = view.watch();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if state.borrow().state == rutis::host::FiberState::Active {
                break;
            }
            state.changed().await.unwrap();
        }
    })
    .await
    .unwrap_or_else(|error| panic!("late provider: {error}; view={:?}", view.state()));
    assert_eq!(
        client
            .rpc("session.late.status", json!({"sessionId":session}))
            .await["result"],
        "active"
    );
    // Once active, the late provider's tool is offered to this Session's
    // next request without a reload (#241).
    submit(&mut client, &session, r#"tool:echo {"text":"after"}"#).await;
    let mut offered = false;
    while let Ok(names) = requests.try_recv() {
        offered |= names.contains(&"late".into());
    }
    assert!(
        offered,
        "the late provider's tool is offered once it is active"
    );
    server.shutdown();
    server.join().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires DIM_TEST_NODE and DIM_TEST_JS_RUNTIME"]
async fn node_session_typescript_rejects_invalid_boundaries_without_disabling_native_tools() {
    let mut parts = fixture();
    parts.rows[0]["config"] = json!({"badDeclaration":true});
    let (server, mut requests, _) = start(ServerConfig {
        session_typescript: Some(parts),
        ..Default::default()
    })
    .await;
    let mut client = Client::new_version(&server, false, 3).await;
    let created = client
        .rpc("session.create", create(json!(["ts_echo", "echo"])))
        .await;
    assert_ok(&created);
    let session = created["result"]["sessionId"].as_str().unwrap();
    submit(&mut client, session, r#"tool:echo {"text":"native"}"#).await;
    assert!(!requests.recv().await.unwrap().contains(&"ts_echo".into()));
    let status = when_served(
        &mut client,
        "session.typescript.status",
        json!({"sessionId":session}),
    )
    .await;
    assert!(
        status["result"]["refused"]
            .as_str()
            .is_some_and(|refused| refused.contains("must declare its risk")),
        "{status}"
    );
    assert_ok(
        &client
            .rpc("session.close", json!({"sessionId":session}))
            .await,
    );
    server.shutdown();
    server.join().await.unwrap();

    for bad_methods in [json!(true), json!("unsupported"), Value::Null] {
        let mut parts = fixture();
        if !bad_methods.is_null() {
            parts.rows[0]["config"] = json!({"badMethods":bad_methods});
        } else {
            parts.rows[0]["provides"]["ghost"] = json!({"read":"sync"});
        }
        let (mut builder, mut requests, _) = setup();
        let (send, mut controls) = tokio::sync::mpsc::unbounded_channel();
        builder.require_feature(ControlledGate::feature(send));
        let server = serve(
            builder,
            ServerConfig {
                unix_path: Some(std::env::temp_dir().join(format!(
                    "dm-ts-{}/server.sock",
                    &uuid::Uuid::new_v4().simple().to_string()[..8]
                ))),
                session_typescript: Some(parts),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let mut client = Client::new_version(&server, false, 3).await;
        let session = new_session(&mut client, json!(["ts_echo", "echo"])).await;
        let (session_instance, ctx, loader, _) = controls.recv().await.unwrap();
        if !bad_methods.is_null() {
            let view = session_copy(&ctx, session_instance, "SessionTypeScriptFixture").await;
            let mut state = view.watch();
            tokio::time::timeout(Duration::from_secs(5), async {
                while state.borrow().state != rutis::host::FiberState::Failed {
                    state.changed().await.unwrap();
                }
            })
            .await
            .unwrap();
        } else {
            let entry = loader
                .entries()
                .into_iter()
                .find(|entry| entry.id == "typescript" && entry.instance.is_none())
                .unwrap();
            assert!(
                matches!(entry.status, rutis_loader::report::EntryStatus::Unresolved(ref error) if error.to_string().contains("provides differs")),
                "{:?}",
                entry.status
            );
        }
        assert_eq!(
            client
                .rpc("session.typescript.status", json!({"sessionId":session}))
                .await["error"]["data"]["appError"],
            "unknown_method"
        );
        submit(&mut client, &session, r#"tool:echo {"text":"native"}"#).await;
        assert!(!requests.recv().await.unwrap().contains(&"ts_echo".into()));
        server.shutdown();
        server.join().await.unwrap();
    }

    for (namespace, expected) in [
        ("session.create", "invalid Session namespace"),
        ("session.delete", "invalid Session namespace"),
        ("session.fork", "invalid Session namespace"),
        ("session.input", "is registered already"),
    ] {
        let mut parts = fixture();
        parts.rows[0]["config"] = json!({"namespace":namespace});
        let (mut builder, _, _) = setup();
        let (send, mut controls) = tokio::sync::mpsc::unbounded_channel();
        builder.require_feature(ControlledGate::feature(send));
        let server = serve(
            builder,
            ServerConfig {
                unix_path: Some(std::env::temp_dir().join(format!(
                    "dm-ts-{}/server.sock",
                    &uuid::Uuid::new_v4().simple().to_string()[..8]
                ))),
                session_typescript: Some(parts),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let mut client = Client::new_version(&server, false, 3).await;
        new_session(&mut client, json!(["echo"])).await;
        let (session_instance, ctx, _, _) = controls.recv().await.unwrap();
        let view = session_copy(&ctx, session_instance, "SessionTypeScriptFixture").await;
        let mut state = view.watch();
        let failed = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let snapshot = state.borrow_and_update().clone();
                if snapshot.state == rutis::host::FiberState::Failed {
                    break snapshot;
                }
                state.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
        assert!(
            failed
                .error
                .as_ref()
                .is_some_and(|error| error.to_string().contains(expected)),
            "{namespace}: {failed:?}"
        );
        server.shutdown();
        server.join().await.unwrap();
    }

    let mut parts = fixture();
    let mut repeated = parts.rows[0].clone();
    repeated["id"] = json!("different-row");
    repeated["provides"] = json!({"other":{"read":"sync"}});
    parts.rows.push(repeated);
    let (builder, _, _) = setup();
    let error = serve(
        builder,
        ServerConfig {
            session_typescript: Some(parts),
            ..Default::default()
        },
    )
    .await
    .err()
    .expect("inconsistent module snapshots must fail");
    assert!(
        error.to_string().contains("same provides snapshot"),
        "{error}"
    );
}

/// A `type` array is admitted and offered to the model in canonical form; a
/// schema the declaration parser still refuses names the tool and the path.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires DIM_TEST_NODE and DIM_TEST_JS_RUNTIME"]
async fn node_session_typescript_accepts_type_array_schemas_and_names_a_refused_one() {
    let mut parts = fixture();
    parts.rows[0]["config"] = json!({"typeArray": true});
    let (server, _requests, _, mut recorded) = start_recording(ServerConfig {
        session_typescript: Some(parts),
        ..Default::default()
    })
    .await;
    plugin_active(&server, &fixture().rows[0]).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let session = new_session(&mut client, json!(["ts_echo", "ts_nullable", "echo"])).await;
    submit(&mut client, &session, r#"tool:echo {"text":"offered"}"#).await;
    let offered = recorded.recv().await.unwrap();
    let nullable = offered
        .iter()
        .find(|tool| tool.name == "ts_nullable")
        .unwrap_or_else(|| panic!("ts_nullable is offered: {offered:?}"));
    assert_eq!(
        serde_json::to_value(&nullable.input_schema).unwrap(),
        json!({
            "type": "object",
            "properties": {"space_id": {"anyOf": [{"type": "string"}, {"type": "null"}]}},
            "required": ["space_id"]
        })
    );
    server.shutdown();
    server.join().await.unwrap();

    let mut parts = fixture();
    parts.rows[0]["config"] = json!({"badTypeArray": true});
    let (server, _requests, _) = start(ServerConfig {
        session_typescript: Some(parts),
        ..Default::default()
    })
    .await;
    let mut client = Client::new_version(&server, false, 3).await;
    let session = new_session(&mut client, json!(["ts_echo", "echo"])).await;
    let status = when_served(
        &mut client,
        "session.typescript.status",
        json!({"sessionId":session}),
    )
    .await;
    assert!(
        status["result"]["refused"].as_str().is_some_and(|refused| {
            refused.contains("tool ts_broken")
                && refused.contains("inputSchema.properties.x.type[1]")
        }),
        "{status}"
    );
    server.shutdown();
    server.join().await.unwrap();
}

/// #207: Session creation does not wait for TypeScript parts, so a published
/// Session can hold a Failed part. A part that failed before registering its
/// namespace never held it, so the namespace answers the non-retryable
/// `unknown_method`, not `service_reloading`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires DIM_TEST_NODE and DIM_TEST_JS_RUNTIME"]
async fn node_session_typescript_failed_part_namespace_stays_unknown() {
    let mut parts = fixture();
    parts.rows[0]["config"] = json!({"badMethods":true});
    let (mut builder, _requests, _) = setup();
    let (send, mut controls) = tokio::sync::mpsc::unbounded_channel();
    builder.require_feature(ControlledGate::feature(send));
    let server = serve(
        builder,
        ServerConfig {
            unix_path: Some(std::env::temp_dir().join(format!(
                "dm-ts-{}/server.sock",
                &uuid::Uuid::new_v4().simple().to_string()[..8]
            ))),
            session_typescript: Some(parts),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let mut client = Client::new_version(&server, false, 3).await;
    let session = new_session(&mut client, json!(["echo"])).await;
    let (session_instance, ctx, _, _) = controls.recv().await.unwrap();
    let part = session_copy(&ctx, session_instance, "SessionTypeScriptFixture").await;
    let published = part.state();
    let mut state = part.watch();
    tokio::time::timeout(Duration::from_secs(5), async {
        while state.borrow().state != rutis::host::FiberState::Failed {
            state.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    let mut replies = vec![];
    for _ in 0..3 {
        let reply = client
            .rpc("session.typescript.status", json!({"sessionId":session}))
            .await;
        replies.push((
            reply["error"]["code"].clone(),
            reply["error"]["data"]["appError"].clone(),
        ));
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(
        replies,
        vec![(json!(-32601), json!("unknown_method")); 3],
        "part once session.create replied {published:?}, now {:?}",
        part.state()
    );
    server.shutdown();
    server.join().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires DIM_TEST_NODE and DIM_TEST_JS_RUNTIME"]
async fn node_session_typescript_tool_only_first_request_on_create_and_restore() {
    let home = std::env::temp_dir().join(format!("dm-ts-tool-{}", uuid::Uuid::new_v4()));
    let mut parts = fixture();
    let entry = PathBuf::from(parts.rows[0]["name"].as_str().unwrap());
    parts.rows[0]["name"] = json!(entry.with_file_name("session-tool-only.ts"));
    parts.rows[0]["provides"] = serde_json::from_str(include_str!(
        "../../../../runtime/fixtures/session-tool-only.json"
    ))
    .unwrap();
    let config = ServerConfig {
        database_path: Some(home.join("sessions.sqlite")),
        session_typescript: Some(parts),
        ..Default::default()
    };
    let mut saved: Option<String> = None;
    for text in ["created", "restored"] {
        // Once the plugin row has registered its part, the part settles
        // before a Session that opens is published (ADR 006 D3,
        // Publication), so its tool is in the first request.
        let (server, mut requests, _) = start(config.clone()).await;
        plugin_active(
            &server,
            &config.session_typescript.as_ref().unwrap().rows[0],
        )
        .await;
        let mut client = Client::new_version(&server, false, 3).await;
        let session_id = match &saved {
            Some(id) => {
                assert!(server.agent().unwrap().get_session(id).is_none());
                id.clone()
            }
            None => new_session(&mut client, json!(["ts_echo", "echo"])).await,
        };
        submit(
            &mut client,
            &session_id,
            &format!(r#"tool:ts_echo {{"text":"{text}"}}"#),
        )
        .await;
        assert!(requests.recv().await.unwrap().contains(&"ts_echo".into()));
        let session = server.agent().unwrap().get_session(&session_id).unwrap();
        let history = session.history("main", None).await.unwrap();
        let result = history
            .iter()
            .rev()
            .find_map(|event| match &event.kind {
                EventKind::ToolResult(batch) => batch.results.first(),
                _ => None,
            })
            .unwrap();
        assert_ne!(result.result.is_error, Some(true));
        let dim_agent_types::ToolContent::Text(content) = &result.result.content else {
            panic!("text result")
        };
        assert_eq!(
            serde_json::from_str::<Value>(content).unwrap()["arguments"]["text"],
            text
        );
        saved = Some(session_id);
        drop(client);
        server.shutdown();
        server.join().await.unwrap();
    }
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires DIM_TEST_NODE and DIM_TEST_JS_RUNTIME"]
async fn node_session_typescript_row_reload_settles_tools_and_rejects_retained_handlers() {
    let (mut builder, _requests, _) = setup();
    let (send, mut controls) = tokio::sync::mpsc::unbounded_channel();
    builder.require_feature(ControlledGate::feature(send));
    // This flow needs the fixture before publication, including a late registration.
    builder.require_part("SessionTypeScriptFixture");
    let server = serve(
        builder,
        ServerConfig {
            unix_path: Some(std::env::temp_dir().join(format!(
                "dm-ts-{}/server.sock",
                &uuid::Uuid::new_v4().simple().to_string()[..8]
            ))),
            session_typescript: Some(fixture()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    plugin_active(&server, &fixture().rows[0]).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let session_id = new_session(&mut client, json!(["ts_echo", "echo"])).await;
    let (session_instance, ctx, _, registry) = controls.recv().await.unwrap();
    let retained = registry.contributed("ts_echo").unwrap();
    let old = client
        .rpc("session.typescript.status", json!({"sessionId":session_id}))
        .await;
    assert_ok(&client.rpc("session.submit", json!({"sessionId":session_id,"branchId":"main","text":r#"tool:ts_echo {"text":"cancel"}"#})).await);
    assert_ok(
        &client
            .rpc(
                "session.typescript.wait_tool",
                json!({"sessionId":session_id}),
            )
            .await,
    );
    let view = session_copy(&ctx, session_instance, "SessionTypeScriptFixture").await;
    tokio::time::timeout(Duration::from_secs(5), view.restart())
        .await
        .unwrap()
        .unwrap();
    let mut state = view.watch();
    tokio::time::timeout(Duration::from_secs(5), async {
        while state.borrow().state != rutis::host::FiberState::Active {
            state.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    assert_ok(
        &client
            .rpc(
                "session.idle",
                json!({"sessionId":session_id,"branchId":"main"}),
            )
            .await,
    );
    let session = server.agent().unwrap().get_session(&session_id).unwrap();
    assert!(session.history("main", None).await.unwrap().iter().any(|event| {
        matches!(&event.kind, EventKind::ToolResult(batch) if batch.results.iter().any(|result| {
            result.result.is_error == Some(true)
                && result.result.content == dim_agent_types::ToolContent::Text("tool provider generation is unloading; this call may already have taken effect. Check the operation's state before repeating it with the current provider.".into())
        }))
    }), "an interrupted native wait must preserve uncertainty about the foreign call's effects");
    let result = retained
        .handler
        .execute(
            ToolCall {
                id: "retained".into(),
                name: "ts_echo".into(),
                input: serde_json::from_value(json!({"text":"must not execute"})).unwrap(),
            },
            ToolExecutionContext {
                turn_id: "retained".into(),
                request_id: "retained".into(),
                branch_id: "main".into(),
            },
            CancellationToken::new(),
        )
        .await;
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("generation is unloading")
    );
    let current = client
        .rpc("session.typescript.status", json!({"sessionId":session_id}))
        .await;
    assert_ok(&current);
    assert_ne!(current["result"]["generation"], old["result"]["generation"]);
    assert_eq!(current["result"]["calls"], 0);
    submit(
        &mut client,
        &session_id,
        r#"tool:echo {"text":"native survives row reload"}"#,
    )
    .await;
    server.shutdown();
    server.join().await.unwrap();
}

/// `dim-app-server --session-rows` installs the file's rows as the
/// Session TypeScript parts; without it the namespace is unknown.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires DIM_TEST_NODE and DIM_TEST_JS_RUNTIME"]
async fn node_session_rows_flag_installs_session_methods() {
    use std::process::Stdio;
    let parts = fixture();
    let directory = PathBuf::from("/tmp").join(format!("dim-rows-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&directory).unwrap();
    let rows = directory.join("session-rows.json");
    std::fs::write(&rows, serde_json::to_vec(&parts.rows).unwrap()).unwrap();
    for installed in [true, false] {
        let socket = directory.join(format!("ipc-{installed}/server.sock"));
        let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_dim-app-server"));
        command
            .args(["--mock-model", "--no-ws", "--unix"])
            .arg(&socket);
        command
            .arg("--home")
            .arg(directory.join(format!("home-{installed}")));
        if installed {
            command.arg("--node").arg(&parts.node);
            command.arg("--js-runtime").arg(&parts.js_runtime);
            command.arg("--session-rows").arg(&rows);
        }
        let mut child = command
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut line = String::new();
        tokio::time::timeout(
            Duration::from_secs(30),
            BufReader::new(child.stdout.take().unwrap()).read_line(&mut line),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(line.contains("unixPath"), "{line}");
        let mut client = Client {
            transport: Transport::Unix(BufReader::new(UnixStream::connect(&socket).await.unwrap())),
            id: 0,
            notices: vec![],
            instance: Value::Null,
            features: Value::Null,
        };
        assert_ok(
            &client
                .rpc("hello", json!({"protocolVersion":3,"clientKind":"other"}))
                .await,
        );
        let created = client.rpc("session.create", create(json!(["echo"]))).await;
        assert_ok(&created);
        let sid = created["result"]["sessionId"].clone();
        let status = if installed {
            when_served(
                &mut client,
                "session.typescript.status",
                json!({"sessionId":sid}),
            )
            .await
        } else {
            client
                .rpc("session.typescript.status", json!({"sessionId":sid}))
                .await
        };
        let methods = client
            .rpc("session.methods", json!({"sessionId":sid}))
            .await;
        assert_ok(&methods);
        let listed = methods.to_string().contains("session.typescript.status");
        assert_eq!(listed, installed, "{methods}");
        if installed {
            assert_eq!(status["result"]["calls"], 0, "{status}");
        } else {
            assert_eq!(
                status["error"]["data"]["appError"], "unknown_method",
                "{status}"
            );
        }
        child.kill().await.unwrap();
    }
    std::fs::remove_dir_all(directory).unwrap();
}

/// A tool a TypeScript row registers declares its call's effects, and the
/// bridge carries them on the `step/tool-result` the Session records
/// (session boundary A.6).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires DIM_TEST_NODE and DIM_TEST_JS_RUNTIME"]
async fn node_session_typescript_tool_result_carries_declared_effects() {
    let (server, mut requests, _) = start(ServerConfig {
        session_typescript: Some(fixture()),
        ..Default::default()
    })
    .await;
    plugin_active(&server, &fixture().rows[0]).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let id = new_session(&mut client, json!(["ts_echo", "echo"])).await;
    submit(&mut client, &id, r#"tool:ts_echo {"text":"declares"}"#).await;
    assert!(requests.recv().await.unwrap().contains(&"ts_echo".into()));
    let session = server.agent().unwrap().get_session(&id).unwrap();
    let events = session.history("main", None).await.unwrap();
    let declared = events
        .iter()
        .find_map(|event| match &event.kind {
            EventKind::ToolResult(batch) => batch
                .results
                .iter()
                .find(|result| result.result.effects.is_some())
                .map(|result| result.result.clone()),
            _ => None,
        })
        .expect("the bridge carries the declaration on the step record");
    assert_eq!(
        declared.effects,
        Some(vec![
            dim_agent_types::ToolEffect::FileWritten {
                path: "/work/fixture.rs".into()
            },
            dim_agent_types::ToolEffect::CommandRun {
                command: "npm test".into(),
                exit_code: Some(0),
                signal: None,
            },
        ])
    );
    server.shutdown();
    server.join().await.unwrap();
}

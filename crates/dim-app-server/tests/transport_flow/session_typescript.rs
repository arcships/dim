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
use dim_agent_runtime::SessionScope;
use dim_agent_types::{MessageContent, MessagePart, ModelRequest, Role};
use dim_app_server::session_typescript::SessionTypeScriptConfig;
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
        "provides":serde_json::from_str::<Value>(include_str!("../../../../runtime/fixtures/auxiliary-model.json")).unwrap(),
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
    assert_ok(&client.rpc("provider.save", json!({"provider":{
        "providerId":"aux", "displayName":"Auxiliary fixture", "driverKind":"openai-compatible",
        "baseUrl":format!("http://{address}/v1"), "auth":{"type":"bearer"},
        "credential":{"action":"set","value":"auxiliary-fixture-secret"},
        "models":[{"modelId":"title", "displayName":"Title", "status":"active",
            "capabilities":dim_agent_types::ModelCapabilities::default()},
            {"modelId":"hang", "displayName":"Hang", "status":"active",
            "capabilities":dim_agent_types::ModelCapabilities::default()}], "enabled":true
    }})).await);
    assert_ok(
        &client
            .rpc(
                "provider.select",
                json!({"providerId":"aux","modelId":"title"}),
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
    let request = json!({"model":{"providerId":"aux","modelId":"title"}, "request":{
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
            json!({"model":{"providerId":"aux","modelId":"title"},"request":{"messages":[]}}),
            "invalid_request",
        ),
        (
            json!({"model":{"providerId":"missing","modelId":"title"},"request":{"messages":[{"role":"user","content":"hello"}]}}),
            "model_unavailable",
        ),
        (
            json!({"model":{"providerId":"aux","modelId":"title"},"request":{"messages":[{"role":"user","content":"hello"}],"maxOutputTokens":0}}),
            "invalid_request",
        ),
        (
            json!({"model":{"providerId":"aux","modelId":"title"},"request":{"messages":[{"role":"user","content":"hello"}]},"unexpected":true}),
            "invalid_request",
        ),
        (
            json!({"model":{"providerId":"aux","modelId":"title","extra":1},"request":{"messages":[{"role":"user","content":"hello"}]}}),
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
    // Closing the caller propagates cancellation without unloading Provider.
    let pending = client
        .send(
            "session.auxiliary.complete",
            json!({"sessionId":sid,"cancellable":true,"request":{
                "model":{"providerId":"aux","modelId":"hang"},
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

    // A known name without Provider keeps only the foreign row Pending.
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
        missing["error"]["data"]["appError"], "service_reloading",
        "{missing}"
    );
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

pub(super) fn fixture() -> SessionTypeScriptConfig {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    SessionTypeScriptConfig {
        process_rows: Vec::new(),
        node: std::env::var_os("DIM_TEST_NODE")
            .expect("DIM_TEST_NODE")
            .into(),
        js_runtime: std::env::var_os("DIM_TEST_JS_RUNTIME")
            .expect("DIM_TEST_JS_RUNTIME")
            .into(),
        anchor: root.join("runtime/package.json"),
        rows: vec![json!({
            "id": "typescript", "name": root.join("runtime/fixtures/session-part.ts"),
            "provides": serde_json::from_str::<Value>(include_str!("../../../../runtime/fixtures/session-part.json")).unwrap(),
            "config": {},
        })],
    }
}

fn observers(lifecycle_file: &std::path::Path) -> SessionTypeScriptConfig {
    let mut parts = fixture();
    let root = parts.anchor.parent().unwrap().join("fixtures");
    parts.rows = [
        (
            "observer",
            "session-observer",
            include_str!("../../../../runtime/fixtures/session-observer.json"),
        ),
        (
            "second",
            "session-observer-second",
            include_str!("../../../../runtime/fixtures/session-observer-second.json"),
        ),
    ]
    .into_iter()
    .map(|(id, module, provides)| {
        json!({
            "id":id, "name":root.join(format!("{module}.ts")),
            "provides":serde_json::from_str::<Value>(provides).unwrap(),
            "config":{"lifecycleFile":lifecycle_file},
        })
    })
    .collect();
    parts
}

/// Drain real bootstrap broadcasts before publication so the loading-time
/// sampling assertion does not depend on scheduler timing.
struct DrainLoadingBroadcast(dim_agent_runtime::BranchScope);
struct LoadingFence(Arc<tokio::sync::Notify>);
impl rutis::Listener<dim_agent_capabilities::session_broadcast::SessionBroadcastEvent>
    for LoadingFence
{
    fn call<'a>(
        &'a self,
        _: &'a rutis::Ctx,
        record: &'a dim_agent_capabilities::session_broadcast::SessionBroadcastEvent,
    ) -> rutis::BoxFuture<'a, Result<Option<()>, rutis::CordisError>> {
        if matches!(record, dim_agent_capabilities::session_broadcast::SessionBroadcastEvent::Fence { recipient, .. } if recipient == "test.loading")
        {
            self.0.notify_one();
        }
        Box::pin(async { Ok(None) })
    }
}
impl TypedPlugin for DrainLoadingBroadcast {
    type Deps = (
        InBranch<dim_agent_runtime::Dispatcher>,
        InBranch<dim_agent_runtime::TimelineIdentity>,
        InSession<dim_agent_runtime::SessionLifecycle>,
    );
    fn name(&self) -> &str {
        "test.drain-loading-broadcast"
    }
    fn apply<'a>(
        &'a self,
        ctx: &'a rutis::Ctx,
        (InBranch(dispatcher), InBranch(identity), InSession(lifecycle)): Self::Deps,
    ) -> rutis::BoxFuture<'a, Result<rutis::Effect, rutis::CordisError>> {
        Box::pin(async move {
            let emitter = ctx.clone();
            let channel =
                dim_agent_capabilities::session_broadcast::session_channel(&identity.session_id);
            let drained = Arc::new(tokio::sync::Notify::new());
            ctx.events()
                .on(ctx, &channel, LoadingFence(drained.clone()))?;
            dispatcher.subscribe_all(ctx, self.name(), move |_| {
                let emitter = emitter.clone();
                let channel = channel.clone();
                let lifecycle = lifecycle.clone();
                let drained = drained.clone();
                Box::pin(async move {
                    if lifecycle.phase() == dim_agent_runtime::SessionPhase::Loading {
                        emitter.events().emit(&emitter, &channel, Arc::new(
                            dim_agent_capabilities::session_broadcast::SessionBroadcastEvent::Fence {
                                recipient: "test.loading".into(), token: 0,
                            }
                        )).map_err(|error| dim_agent_runtime::AgentError::fault("bootstrap fence", error.to_string()))?;
                        drained.notified().await;
                    }
                    Ok(())
                })
            })?;
            Ok(rutis::Effect::Done)
        })
    }
}
impl Capability for DrainLoadingBroadcast {
    fn keys(&self) -> KeysOf<Self> {
        scoped::<Self::Deps, _>(&self.0)
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
    builder.session_plugin(move |scope| ControlledGate(*scope, send.clone()));
    builder.timeline_plugin(|scope| DrainLoadingBroadcast(*scope));
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
            trace_directory: home.join("trace"),
            session_typescript: Some(parts),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let mut client = Client::new_version(&server, false, 3).await;
    let first = new_session(&mut client, json!(["echo"])).await;
    let (scope, _, loader, _) = controls.recv().await.unwrap();
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
    let initial = client
        .rpc("session.observer.status", json!({"sessionId":first}))
        .await;
    assert_ok(&initial);
    assert_eq!(
        initial["result"]["records"],
        json!([]),
        "loading-time observations are not replayed"
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
                source: dim_agent_types::BranchSource::Tool {
                    turn_id: "turn".into(),
                    request_id: "request".into(),
                    call_id: "call".into(),
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
    let view = loader
        .entries()
        .into_iter()
        .find(|entry| {
            entry.id == "observer"
                && entry.instance.as_ref().is_some_and(|instance| {
                    instance.chain.iter().any(|(_, id)| *id == scope.instance)
                })
        })
        .unwrap()
        .view
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), view.restart())
        .await
        .unwrap()
        .unwrap();
    let mut state = view.watch();
    tokio::time::timeout(Duration::from_secs(5), async {
        while state.borrow().state != rutis::FiberState::Active {
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
    submit(&mut client, &first, "close with queued observations").await;
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
                    == before_close["result"]["records"].as_array().unwrap().len() as u64)
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
    let restored = client
        .rpc("session.observer.status", json!({"sessionId":second}))
        .await;
    assert_ok(&restored);
    assert_ne!(
        restored["result"]["generation"],
        other["result"]["generation"]
    );
    assert_eq!(restored["result"]["records"], json!([]));
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires DIM_TEST_NODE and DIM_TEST_JS_RUNTIME"]
async fn node_session_typescript_broadcast_creation_config_and_bind_failures() {
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
                tools: vec!["echo".into()],
                interactive,
                collaboration_role: role,
                ..Default::default()
            })
            .await
            .unwrap();
        // Native member Sessions have no client protocol owner; use public C1.
        // The optional observer can still be publishing after native creation.
        let methods = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match session.session_named_service::<SessionMethods>("session.observer") {
                    Ok(methods) => break methods,
                    Err(dim_agent_runtime::AgentError::Reloading(_)) => {
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

    for bad in ["throw", "value", "future"] {
        let mut parts = observers(&audit);
        parts.rows[0]["config"] = json!({"badBind":bad});
        let (mut builder, mut requests, _) = setup();
        let (send, mut controls) = tokio::sync::mpsc::unbounded_channel();
        builder.session_plugin(move |scope| ControlledGate(*scope, send.clone()));
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
        let (scope, _, loader, _) = controls.recv().await.unwrap();
        let view = loader
            .entries()
            .into_iter()
            .find(|entry| {
                entry.id == "observer"
                    && entry.instance.as_ref().is_some_and(|instance| {
                        instance.chain.iter().any(|(_, id)| *id == scope.instance)
                    })
            })
            .unwrap()
            .view
            .unwrap();
        let mut state = view.watch();
        tokio::time::timeout(Duration::from_secs(5), async {
            while state.borrow().state != rutis::FiberState::Failed {
                state.changed().await.unwrap();
            }
        })
        .await
        .unwrap_or_else(|error| panic!("{bad}: {error}; {:?}", view.state()));
        assert_eq!(
            client
                .rpc("session.observer.status", json!({"sessionId":id}))
                .await["error"]["data"]["appError"],
            "service_unavailable"
        );
        submit(&mut client, &id, "native continues after failed bind").await;
        assert!(requests.recv().await.unwrap().contains(&"echo".into()));
        let sequence = server
            .agent()
            .unwrap()
            .get_session(&id)
            .unwrap()
            .sequence("main")
            .unwrap();
        assert_ok(
            &client
                .rpc(
                    "session.second.through",
                    json!({"sessionId":id,"sequence":sequence}),
                )
                .await,
        );
        server.shutdown();
        server.join().await.unwrap();
    }
    std::fs::remove_file(audit).unwrap();
}

struct CheckedModel(tokio::sync::mpsc::UnboundedSender<Vec<String>>);
impl ModelDriver for CheckedModel {
    fn invoke(
        &self,
        request: Arc<ModelRequest>,
        connection: ModelConnection,
        cancel: CancellationToken,
    ) -> HostFuture<'_, ModelStream> {
        self.0
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

pub(super) async fn start(
    mut config: ServerConfig,
) -> (
    RunningServer,
    tokio::sync::mpsc::UnboundedReceiver<Vec<String>>,
    Arc<tokio::sync::Notify>,
) {
    config.unix_path = Some(std::env::temp_dir().join(format!(
        "dm-ts-{}/server.sock",
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    )));
    let (builder, recv, release) = setup();
    let server = serve(builder, config).await.unwrap();
    (server, recv, release)
}

pub(super) fn setup() -> (
    AgentBuilder,
    tokio::sync::mpsc::UnboundedReceiver<Vec<String>>,
    Arc<tokio::sync::Notify>,
) {
    let (send, recv) = tokio::sync::mpsc::unbounded_channel();
    let release = Arc::new(tokio::sync::Notify::new());
    let mut builder = AgentBuilder::new(AgentConfig::default());
    defaults::install(&mut builder);
    builder
        .session_value(Arc::new(HistoryRenderer::default()))
        .session_value(Arc::new(ModelHost(Arc::new(CheckedModel(send)))))
        .session_value(Arc::new(PrepareContextHost(Arc::new(IdentityContext))))
        .session_value(Arc::new(ContextModelHost(None)))
        .session_value(Arc::new(CompactionStrategyHost(None)))
        .session_plugin(DefaultPrepareModelPlugin::new);
    defaults::tool_host(&mut builder, Arc::new(SlowTools(release.clone())));
    (builder, recv, release)
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
    let mut client = Client::new_version(&server, false, 3).await;
    let first = new_session(&mut client, json!(["ts_echo", "native_slow", "echo"])).await;
    let second = new_session(&mut client, json!(["ts_echo", "echo"])).await;
    submit(&mut client, &first, r#"tool:ts_echo {"text":"first"}"#).await;
    assert!(requests.recv().await.unwrap().contains(&"ts_echo".into()));
    let status = client
        .rpc("session.typescript.status", json!({"sessionId":first}))
        .await;
    assert_eq!(status["result"]["calls"], 1);
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
    assert_eq!(error["error"]["code"], -32011);
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
        .session_named_service::<SessionMethods>("session.typescript")
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
    assert_eq!(drained["result"]["cancelled"], true);
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
        trace_directory: home.join("trace"),
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
    assert!(
        !server
            .diagnostics()
            .plugins
            .iter()
            .any(|part| part.name.ends_with("runtime/fixtures/session-part.ts"))
    );
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

pub(super) type Control = (
    SessionScope,
    rutis::Ctx,
    Arc<rutis_loader::Loader>,
    Arc<dim_agent_capabilities::tool_registry::ToolRegistry>,
);
pub(super) struct ControlledGate(
    pub SessionScope,
    pub tokio::sync::mpsc::UnboundedSender<Control>,
);
impl TypedPlugin for ControlledGate {
    type Deps = (
        Arc<rutis_loader::Loader>,
        rutis::KeyedGate<rutis_loader::RuntimeRows>,
        InSession<dim_agent_capabilities::tool_registry::ToolRegistry>,
    );
    fn name(&self) -> &str {
        "ControlledTypeScriptDependency"
    }
    fn apply<'a>(
        &'a self,
        ctx: &'a rutis::Ctx,
        (loader, _, InSession(registry)): Self::Deps,
    ) -> rutis::BoxFuture<'a, Result<rutis::Effect, rutis::CordisError>> {
        Box::pin(async move {
            self.1
                .send((self.0, ctx.clone(), loader, registry))
                .unwrap();
            Ok(rutis::Effect::Done)
        })
    }
}
impl Capability for ControlledGate {
    fn keys(&self) -> KeysOf<Self> {
        // Observe the settled initial metadata, rather than a fiber replaced
        // by RuntimeRows' offline-to-online refresh (as in the C4 spike).
        ((), rutis::DepKey::dynamic("session-node"), self.0)
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
    let mut gate = parts.rows.remove(0);
    let entry = PathBuf::from(gate["name"].as_str().unwrap());
    gate["id"] = json!("disabled-gate");
    gate["disabled"] = json!(true);
    parts.rows.push(json!({"id":"typescript", "name":entry.with_file_name("session-late.ts"), "provides":serde_json::from_str::<Value>(include_str!("../../../../runtime/fixtures/session-late.json")).unwrap()}));
    parts.rows.push(gate);
    let (mut builder, mut requests, _) = setup();
    let (send, mut controls) = tokio::sync::mpsc::unbounded_channel();
    builder.session_plugin(move |scope| ControlledGate(*scope, send.clone()));
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
    let (scope, ctx, loader, _) = controls.recv().await.unwrap();
    let view = loader
        .entries()
        .into_iter()
        .find(|entry| {
            entry.id == "typescript"
                && entry.instance.as_ref().is_some_and(|instance| {
                    instance.chain.iter().any(|(_, id)| *id == scope.instance)
                })
        })
        .unwrap()
        .view
        .unwrap();
    assert_eq!(view.state().state, rutis::FiberState::Pending);
    assert_eq!(
        client
            .rpc("session.late.status", json!({"sessionId":session}))
            .await["error"]["data"]["appError"],
        "service_reloading"
    );
    submit(&mut client, &session, r#"tool:echo {"text":"before"}"#).await;
    assert!(!requests.recv().await.unwrap().contains(&"late".into()));
    assert!(
        !server
            .diagnostics()
            .plugins
            .iter()
            .any(|part| part.name.ends_with("runtime/fixtures/session-part.ts"))
    );
    ctx.provide_as::<dyn rutis_bridge::session::HostDispatch>(
        rutis_bridge::session::host_key_in("toolContribution:ts_echo", scope.instance),
        Arc::new(GateValue),
    )
    .unwrap();
    let mut state = view.watch();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if state.borrow().state == rutis::FiberState::Active {
                break;
            }
            state.changed().await.unwrap();
        }
    })
    .await
    .unwrap_or_else(|error| {
        let current = loader
            .entries()
            .into_iter()
            .filter(|entry| entry.id == "typescript")
            .map(|entry| {
                (
                    entry.instance,
                    entry.status,
                    entry.view.map(|view| view.state()),
                )
            })
            .collect::<Vec<_>>();
        panic!(
            "late provider: {error}; view={:?}; current={current:?}",
            view.state()
        )
    });
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
    let trace_home = std::env::temp_dir().join(format!("dm-ts-trace-{}", uuid::Uuid::new_v4()));
    let (server, mut requests, _) = start(ServerConfig {
        session_typescript: Some(parts),
        trace_directory: trace_home.clone(),
        ..Default::default()
    })
    .await;
    let mut client = Client::new_version(&server, false, 3).await;
    let mut params = create(json!(["ts_echo", "echo"]));
    params["trace"] = json!(true);
    let created = client.rpc("session.create", params).await;
    assert_ok(&created);
    let session = created["result"]["sessionId"].as_str().unwrap();
    let trace_path = created["result"]["tracePath"].as_str().unwrap();
    submit(&mut client, session, r#"tool:echo {"text":"native"}"#).await;
    assert!(!requests.recv().await.unwrap().contains(&"ts_echo".into()));
    assert_ok(
        &client
            .rpc("session.typescript.status", json!({"sessionId":session}))
            .await,
    );
    assert_ok(
        &client
            .rpc("session.close", json!({"sessionId":session}))
            .await,
    );
    let records = read_trace(trace_path).await;
    assert!(
        records
            .iter()
            .any(|row| row["record"]["fields"]["operation"] == "tool.attach"
                && row["record"]["fields"]["outcome"] == "invalid-declaration:ts_echo"),
        "{records:?}"
    );
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(trace_home).unwrap();

    for bad_methods in [json!(true), json!("unsupported"), Value::Null] {
        let mut parts = fixture();
        if !bad_methods.is_null() {
            parts.rows[0]["config"] = json!({"badMethods":bad_methods});
        } else {
            parts.rows[0]["provides"]["toolContribution:ghost"] =
                json!({"declaration":"sync", "execute":"async"});
        }
        let (mut builder, mut requests, _) = setup();
        let (send, mut controls) = tokio::sync::mpsc::unbounded_channel();
        builder.session_plugin(move |scope| ControlledGate(*scope, send.clone()));
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
        let (scope, _, loader, _) = controls.recv().await.unwrap();
        let entry = loader
            .entries()
            .into_iter()
            .find(|entry| {
                entry.id == "typescript"
                    && entry.instance.as_ref().is_some_and(|instance| {
                        instance.chain.iter().any(|(_, id)| *id == scope.instance)
                    })
            })
            .unwrap();
        if !bad_methods.is_null() {
            let view = entry.view.unwrap();
            let mut state = view.watch();
            tokio::time::timeout(Duration::from_secs(5), async {
                while state.borrow().state != rutis::FiberState::Failed {
                    state.changed().await.unwrap();
                }
            })
            .await
            .unwrap();
        } else {
            assert!(
                matches!(entry.status, rutis_loader::EntryStatus::Unresolved(ref error) if error.to_string().contains("provides differs")),
                "{:?}",
                entry.status
            );
        }
        assert_eq!(
            client
                .rpc("session.typescript.status", json!({"sessionId":session}))
                .await["error"]["data"]["appError"],
            "service_unavailable"
        );
        submit(&mut client, &session, r#"tool:echo {"text":"native"}"#).await;
        assert!(!requests.recv().await.unwrap().contains(&"ts_echo".into()));
        server.shutdown();
        server.join().await.unwrap();
    }

    for (change, expected) in [
        (
            json!({"sessionEvents:":{"bind":"sync"}}),
            "invalid sessionEvents",
        ),
        (
            json!({"sessionEvents:observer":{"bind":"async"}}),
            "invalid sessionEvents",
        ),
        (
            json!({"sessionEvents:observer":{"bind":"sync","other":"sync"}}),
            "invalid sessionEvents",
        ),
        (
            json!({"toolContribution:echo":{"declaration":"sync","execute":"async"}}),
            "collides",
        ),
        (
            json!({"toolContribution:ts_echo":{"declaration":"async","execute":"async"}}),
            "method modes",
        ),
        (
            json!({"sessionMethods:session.create":{"describe":"async","call":"async"}}),
            "invalid Session namespace",
        ),
        (
            json!({"sessionMethods:session.delete":{"describe":"async","call":"async"}}),
            "invalid Session namespace",
        ),
        (
            json!({"sessionMethods:session.fork":{"describe":"async","call":"async"}}),
            "invalid Session namespace",
        ),
        (
            json!({"sessionMethods:session.input":{"describe":"async","call":"async"}}),
            "duplicate Session method namespace",
        ),
        (
            json!({"sessionMethods:shell":{"describe":"async","call":"async"}}),
            "duplicate Session method namespace",
        ),
    ] {
        let mut parts = fixture();
        parts.rows[0]["disabled"] = json!(true);
        parts.rows[0]["provides"] = change;
        let (mut builder, _, _) = setup();
        // A disabled foreign row must not claim the native ShellInput owner.
        dim_agent_capabilities::shell_input::install(&mut builder);
        let error = serve(
            builder,
            ServerConfig {
                session_typescript: Some(parts),
                ..Default::default()
            },
        )
        .await
        .err()
        .expect("invalid structural input must fail");
        assert!(error.to_string().contains(expected), "{error}");
    }

    let mut parts = fixture();
    let mut repeated = parts.rows[0].clone();
    repeated["id"] = json!("different-row");
    repeated["provides"] = json!({});
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

/// #207: Session creation does not wait for TypeScript parts, so a published
/// Session can hold a Failed part. Its namespace has no provider until the
/// part is fixed, so it answers the non-retryable `service_unavailable`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires DIM_TEST_NODE and DIM_TEST_JS_RUNTIME"]
async fn node_session_typescript_failed_part_namespace_is_unavailable() {
    let mut parts = fixture();
    parts.rows[0]["config"] = json!({"badMethods":true});
    let (mut builder, _requests, _) = setup();
    let (send, mut controls) = tokio::sync::mpsc::unbounded_channel();
    builder.session_plugin(move |scope| ControlledGate(*scope, send.clone()));
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
    let (scope, _, loader, _) = controls.recv().await.unwrap();
    let part = || {
        loader
            .entries()
            .into_iter()
            .find(|entry| {
                entry.id == "typescript"
                    && entry.instance.as_ref().is_some_and(|instance| {
                        instance.chain.iter().any(|(_, id)| *id == scope.instance)
                    })
            })
            .unwrap()
    };
    let published = part().status;
    let mut state = part().view.unwrap().watch();
    tokio::time::timeout(Duration::from_secs(5), async {
        while state.borrow().state != rutis::FiberState::Failed {
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
        vec![(json!(-32000), json!("service_unavailable")); 3],
        "part once session.create replied {published:?}, now {:?}",
        part().status
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
        trace_directory: home.join("trace"),
        session_typescript: Some(parts),
        ..Default::default()
    };
    let mut saved: Option<String> = None;
    for text in ["created", "restored"] {
        let (server, mut requests, _) = start(config.clone()).await;
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
    builder.session_plugin(move |scope| ControlledGate(*scope, send.clone()));
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
    let mut client = Client::new_version(&server, false, 3).await;
    let session_id = new_session(&mut client, json!(["ts_echo", "echo"])).await;
    let (scope, _, loader, registry) = controls.recv().await.unwrap();
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
    let view = loader
        .entries()
        .into_iter()
        .find(|entry| {
            entry.id == "typescript"
                && entry.instance.as_ref().is_some_and(|instance| {
                    instance.chain.iter().any(|(_, id)| *id == scope.instance)
                })
        })
        .unwrap()
        .view
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), view.restart())
        .await
        .unwrap()
        .unwrap();
    let mut state = view.watch();
    tokio::time::timeout(Duration::from_secs(5), async {
        while state.borrow().state != rutis::FiberState::Active {
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires DIM_TEST_NODE and DIM_TEST_JS_RUNTIME"]
async fn node_session_typescript_unload_cancels_a_stalled_describe() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut parts = fixture();
    parts.rows[0]["config"] =
        json!({"hangDescribe":{"host":"127.0.0.1","port":listener.local_addr().unwrap().port()}});
    let (mut builder, mut requests, _) = setup();
    let (send, mut controls) = tokio::sync::mpsc::unbounded_channel();
    builder.session_plugin(move |scope| ControlledGate(*scope, send.clone()));
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
    let creating = client
        .send("session.create", create(json!(["ts_echo", "echo"])))
        .await;
    let (mut socket, _) = tokio::time::timeout(Duration::from_secs(5), listener.accept())
        .await
        .unwrap()
        .unwrap();
    let (scope, _, loader, _) = controls.recv().await.unwrap();
    let view = loader
        .entries()
        .into_iter()
        .find(|entry| {
            entry.id == "typescript"
                && entry.instance.as_ref().is_some_and(|instance| {
                    instance.chain.iter().any(|(_, id)| *id == scope.instance)
                })
        })
        .unwrap()
        .view
        .unwrap();
    assert_eq!(view.state().state, rutis::FiberState::Loading);
    tokio::time::timeout(Duration::from_secs(5), view.dispose())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        tokio::time::timeout(
            Duration::from_secs(5),
            tokio::io::AsyncReadExt::read(&mut socket, &mut [0])
        )
        .await
        .unwrap()
        .unwrap(),
        0,
        "foreign row cleanup must close its socket"
    );
    let created = client.response(creating).await;
    assert_ok(&created);
    let session_id = created["result"]["sessionId"].as_str().unwrap();
    submit(
        &mut client,
        session_id,
        r#"tool:echo {"text":"native after cancelled binding"}"#,
    )
    .await;
    assert!(!requests.recv().await.unwrap().contains(&"ts_echo".into()));
    server.shutdown();
    server.join().await.unwrap();
}

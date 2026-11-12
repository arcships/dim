use super::*;
use dim_agent_capabilities::methods::{Interruption, MethodCall, MethodDecl, SessionMethods};
use dim_agent_types::InputMode;
use rutis::host::CtxHost;

/// Registers one Session namespace with the Session's method table and
/// records what the table answered: "" when it took the registration.
struct Claim {
    namespace: &'static str,
    answer: Arc<std::sync::Mutex<Option<String>>>,
    called: Arc<std::sync::atomic::AtomicBool>,

    methods: rutis::host::Key<SessionMethods>,
}
impl Claim {
    fn new(
        names: &dim_agent_runtime::Names<'_>,
        namespace: &'static str,
        answer: Arc<std::sync::Mutex<Option<String>>>,
        called: Arc<std::sync::atomic::AtomicBool>,
    ) -> rutis::Result<Self> {
        Ok(Self {
            namespace,
            answer,
            called,
            methods: names.key()?,
        })
    }
}
impl rutis::host::DynPlugin for Claim {
    fn name(&self) -> &str {
        "Claim"
    }
    fn injects(&self) -> Vec<rutis::host::TypeKey> {
        vec![self.methods.erase()]
    }
    fn apply<'a>(
        &'a self,
        ctx: &'a rutis::Ctx,
    ) -> rutis::host::BoxFuture<'a, rutis::Result<rutis::host::Effect>> {
        Box::pin(async move {
            let methods = ctx.require_at(&self.methods)?;
            let called = self.called.clone();
            let answer = methods
                .register(
                    ctx,
                    self.namespace,
                    vec![MethodDecl::new(
                        &format!("{}.echo", self.namespace),
                        false,
                        Interruption::Drain,
                    )],
                    Arc::new(move |_, _, _| {
                        called.store(true, std::sync::atomic::Ordering::SeqCst);
                        Box::pin(async { Ok(json!({})) })
                    }),
                )
                .err()
                .map(|error| error.to_string())
                .unwrap_or_default();
            *self.answer.lock().unwrap() = Some(answer);
            Ok(rutis::host::Effect::Done)
        })
    }
}

/// Methods contract A.1: a Session namespace that equals, contains or lies
/// inside a built-in, or lies in a reserved root, is refused when it
/// registers; one under `session` that does neither registers and serves.
/// (Management precedence: `management.rs`.)
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_session_namespace_registers_unless_it_overlaps_a_builtin() {
    for (namespace, refused) in [
        ("session", true),
        ("session.mcp", true),
        ("session.permission", true),
        ("session.create.draft", true),
        ("agent.notes", true),
        ("Shell", true),
        ("session.echo", false),
    ] {
        let answer = Arc::new(std::sync::Mutex::new(None));
        let called = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (server, _) = launch_with(Arc::new(MockTools), ServerConfig::default(), {
            let (answer, called) = (answer.clone(), called.clone());
            move |builder| {
                builder.require_feature(Feature::new("Claim").session("Claim", move |names| {
                    Ok(Box::new(Claim::new(
                        names,
                        namespace,
                        answer.clone(),
                        called.clone(),
                    )?))
                }));
            }
        })
        .await;
        let mut client = Client::new_version(&server, false, 3).await;
        let created = client.rpc("session.create", create(json!([]))).await;
        let sid = created["result"]["sessionId"].as_str().unwrap().to_owned();
        let answer = answer.lock().unwrap().clone().unwrap();
        if refused {
            assert!(
                answer.contains(&format!("invalid Session namespace {namespace}")),
                "{namespace}: {answer}"
            );
        } else {
            assert_eq!(answer, "", "{namespace}");
            assert_ok(
                &client
                    .rpc(&format!("{namespace}.echo"), json!({"sessionId": sid}))
                    .await,
            );
            assert!(called.load(std::sync::atomic::Ordering::SeqCst));
        }
        server.shutdown();
        server.join().await.unwrap();
    }
}

/// Methods contract A.1: a name a management namespace holds is management's
/// even when its params name a Session, so `session.methods` does not list
/// it from a Session namespace whose calls would never reach that namespace.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_methods_does_not_list_a_name_management_holds() {
    use std::os::unix::fs::PermissionsExt;
    let home = std::path::PathBuf::from("/tmp").join(format!("dm-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&home).unwrap();
    std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
    let answer = Arc::new(std::sync::Mutex::new(None));
    let called = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (server, _) = launch_with(
        Arc::new(MockTools),
        ServerConfig {
            product_home: Some(home.clone()),
            target_home: Some(home.join("rust")),
            ..Default::default()
        },
        {
            let (answer, called) = (answer.clone(), called.clone());
            move |builder| {
                builder.require_feature(dim_agent_runtime::Feature::new("Claim").session(
                    "Claim",
                    move |names| {
                        Ok(Box::new(Claim::new(
                            names,
                            "skills",
                            answer.clone(),
                            called.clone(),
                        )?))
                    },
                ));
            }
        },
    )
    .await;
    let mut client = Client::new_version(&server, false, 3).await;
    assert_ok(
        &client
            .rpc(
                "provider.create",
                json!({
                    "providerId":"018ec73f-0000-7000-8000-000000000004",
                    "displayName":"Fixture",
                    "connection":{
                        "driverKind":"openai-compatible",
                        "baseUrl":"http://localhost/v1",
                        "auth":{"type":"anonymous"}
                    },
                    "models":[{"modelId":"fixture","displayName":"fixture","status":"active",
                        "capabilities":dim_agent_types::CatalogCapabilities::with_limits(128_000, 16_384)}],
                    "enabled":true
                }),
            )
            .await,
    );
    assert_ok(
        &client
            .rpc(
                "provider.select",
                json!({
                    "providerId":"018ec73f-0000-7000-8000-000000000004",
                    "modelId":"fixture"
                }),
            )
            .await,
    );
    let created = client.rpc("session.create", create(json!([]))).await;
    assert_ok(&created);
    let sid = created["result"]["sessionId"].as_str().unwrap().to_owned();
    assert_eq!(answer.lock().unwrap().clone().unwrap(), "");
    let listed = client
        .rpc("session.methods", json!({"sessionId":sid}))
        .await;
    assert_ok(&listed);
    let names: Vec<&str> = listed["result"]["methods"]
        .as_array()
        .unwrap()
        .iter()
        .map(|decl| decl["name"].as_str().unwrap())
        .collect();
    assert!(!names.contains(&"skills.echo"), "{listed}");
    let reply = client.rpc("skills.echo", json!({"sessionId":sid})).await;
    assert_eq!(
        reply["error"]["data"]["appError"], "unknown_method",
        "{reply}"
    );
    assert!(!called.load(std::sync::atomic::Ordering::SeqCst));
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// Contributes the `notes` Session methods from outside App Server, as any
/// plugin does: a child plugin registers the namespace with the Session's
/// method table, and a test restarts the child to reload the provider alone.
/// Rpc never names it.
#[derive(Clone, Default)]
struct Notes(Arc<NotesState>);
#[derive(Default)]
struct NotesState {
    /// The provider of each Session, most recent last.
    views: std::sync::Mutex<Vec<rutis::host::FiberView>>,
    /// Holds the next provider start until notified.
    hold: std::sync::Mutex<Option<Arc<tokio::sync::Notify>>>,
    /// `notes.wait` started, and later settled after its token fired.
    entered: tokio::sync::Notify,
    settled: tokio::sync::Notify,
    /// Releases a held `notes.stream`; then it reports whether its token
    /// fired.
    release: tokio::sync::Notify,
    streamed: tokio::sync::Mutex<Option<tokio::sync::oneshot::Sender<bool>>>,
    /// `notes.flood` sent all its progress.
    flooded: tokio::sync::Notify,
}
impl Notes {
    fn install(&self, builder: &mut AgentBuilder) {
        let notes = self.clone();
        builder.require_feature(
            Feature::new("NotesOwner").session("NotesOwner", move |names| {
                Ok(Box::new(NotesOwner::new(names, notes.clone())?))
            }),
        );
    }
    /// The next provider start waits until the returned gate is notified.
    fn hold_next(&self) -> Arc<tokio::sync::Notify> {
        let gate = Arc::new(tokio::sync::Notify::new());
        *self.0.hold.lock().unwrap() = Some(gate.clone());
        gate
    }
    /// Restarts the provider of the most recently created Session.
    async fn restart(&self) {
        let view = self.0.views.lock().unwrap().last().cloned().unwrap();
        view.restart().await.unwrap();
    }
}
struct NotesOwner {
    notes: Notes,

    methods: rutis::host::Key<SessionMethods>,
}
impl NotesOwner {
    fn new(names: &dim_agent_runtime::Names<'_>, notes: Notes) -> rutis::Result<Self> {
        Ok(Self {
            notes,
            methods: names.key()?,
        })
    }
}
impl rutis::host::DynPlugin for NotesOwner {
    fn name(&self) -> &str {
        "NotesOwner"
    }
    fn injects(&self) -> Vec<rutis::host::TypeKey> {
        vec![self.methods.erase()]
    }
    fn apply<'a>(
        &'a self,
        ctx: &'a rutis::Ctx,
    ) -> rutis::host::BoxFuture<'a, rutis::Result<rutis::host::Effect>> {
        Box::pin(async move {
            let methods = ctx.require_at(&self.methods)?;
            let view = dim_agent_runtime::mount_active(
                ctx,
                NotesProvider {
                    methods,
                    notes: self.notes.clone(),
                },
            )
            .await?;
            self.notes.0.views.lock().unwrap().push(view);
            Ok(rutis::host::Effect::Done)
        })
    }
}
struct NotesProvider {
    methods: Arc<SessionMethods>,
    notes: Notes,
}
impl Plugin for NotesProvider {
    type Deps = ();
    fn name(&self) -> &str {
        "Notes"
    }
    async fn apply(&self, ctx: &rutis::Ctx, (): Self::Deps) -> rutis::Result {
        let hold = self.notes.0.hold.lock().unwrap().take();
        if let Some(gate) = hold {
            gate.notified().await;
        }
        let state = self.notes.0.clone();
        self.methods.register(
            ctx,
            "notes",
            vec![
                MethodDecl::new("notes.echo", false, Interruption::Drain),
                MethodDecl {
                    progress: true,
                    ..MethodDecl::new("notes.wait", true, Interruption::Report)
                },
                MethodDecl {
                    progress: true,
                    ..MethodDecl::new("notes.stream", false, Interruption::Drain)
                },
                MethodDecl {
                    progress: true,
                    ..MethodDecl::new("notes.flood", false, Interruption::Drain)
                },
            ],
            Arc::new(move |call, cancel, progress| {
                let MethodCall { method, params, .. } = call;
                let state = state.clone();
                Box::pin(async move {
                    match method.as_str() {
                        "notes.echo" => progress.send(json!("undeclared")),
                        "notes.wait" => {
                            state.entered.notify_one();
                            cancel.cancelled().await;
                            progress.send(json!("after the reply"));
                            state.settled.notify_one();
                        }
                        "notes.flood" => {
                            let text = "x".repeat(params["bytes"].as_u64().unwrap() as usize);
                            for step in 0..params["count"].as_u64().unwrap() {
                                progress.send(json!({"step": step, "text": text.as_str()}));
                            }
                            state.flooded.notify_one();
                        }
                        _ => {
                            for step in 0..3 {
                                progress.send(json!({"step": step}));
                            }
                            if params["hold"] == true {
                                state.release.notified().await;
                                progress.send(json!({"step": 3}));
                                if let Some(streamed) = state.streamed.lock().await.take() {
                                    let _ = streamed.send(cancel.is_cancelled());
                                }
                            }
                        }
                    }
                    Ok(json!({"method": method, "params": params}))
                })
            }),
        )?;
        Ok(())
    }
}
fn app_error(reply: &Value) -> &str {
    reply["error"]["data"]["appError"]
        .as_str()
        .unwrap_or_default()
}

/// Methods contract A.1 and Appendix C: Rpc routes a name that names a
/// Session through that Session, which answers from the namespaces
/// registered in it; a name that names no Session is `unknown_method`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn contributed_session_methods_dispatch_through_the_session_they_name() {
    let notes = Notes::default();
    let (server, _) = launch_with(Arc::new(MockTools), ServerConfig::default(), |builder| {
        notes.install(builder)
    })
    .await;
    let mut client = Client::new_version(&server, false, 3).await;
    let created = client.rpc("session.create", create(json!([]))).await;
    let sid = created["result"]["sessionId"].as_str().unwrap().to_owned();
    let params = json!({"sessionId": sid, "branchId": "main", "text": "hi"});
    let reply = client.rpc("notes.echo", params.clone()).await;
    assert_eq!(
        reply["result"],
        json!({"method": "notes.echo", "params": params})
    );
    assert!(
        client
            .features
            .as_array()
            .unwrap()
            .contains(&json!("session.methods"))
    );
    assert_eq!(
        client
            .rpc("session.methods", json!({"sessionId": sid}))
            .await["result"],
        json!({"methods": [
            {"name": "notes.echo", "mutating": false},
            {"name": "notes.flood", "mutating": false},
            {"name": "notes.stream", "mutating": false},
            {"name": "notes.wait", "mutating": true},
            {"name": "session.agent.get", "mutating": false},
            {"name": "session.agent.update", "mutating": true},
            {"name": "session.collaboration.approval.get", "mutating": false},
            {"name": "session.collaboration.cancel", "mutating": true},
            {"name": "session.collaboration.pending", "mutating": false},
            {"name": "session.input.pending", "mutating": false},
            {"name": "session.input.withdraw", "mutating": true},
            {"name": "session.language.get", "mutating": false},
            {"name": "session.language.update", "mutating": true},
            {"name": "session.tool.get", "mutating": false},
            {"name": "session.turn.get", "mutating": false},
        ]})
    );
    let mut remote = Client::new_version(&server, true, 3).await;
    assert_ok(&remote.rpc("notes.echo", json!({"sessionId": sid})).await);

    for (method, params) in [
        ("other.echo", json!({"sessionId": sid})),
        ("other.echo", json!([1])),
        ("notes.Echo", json!({"sessionId": sid})),
        ("session.notes", json!({"sessionId": sid})),
        ("notes.echo", json!({})),
        ("notes.echo", json!([sid])),
    ] {
        let reply = client.rpc(method, params).await;
        assert_eq!(reply["error"]["code"], -32601, "{method}: {reply}");
        assert_eq!(app_error(&reply), "unknown_method", "{method}");
    }
    let reply = client.rpc("notes.echo", json!({"sessionId": 7})).await;
    assert_eq!(reply["error"]["code"], -32602, "{reply}");
    for method in ["notes", "notes.missing"] {
        let reply = client.rpc(method, json!({"sessionId": sid})).await;
        assert_eq!(app_error(&reply), "unknown_method", "{method}");
    }
    let unknown = client
        .rpc(
            "notes.echo",
            json!({"sessionId": uuid::Uuid::new_v4().to_string()}),
        )
        .await;
    assert_eq!(app_error(&unknown), "session_closed");
    let mut v2 = Client::new_version(&server, false, 2).await;
    for method in ["notes.echo", "session.methods"] {
        let reply = v2.rpc(method, json!({"sessionId": sid})).await;
        assert_eq!(app_error(&reply), "unknown_method", "{method}");
    }
    // A Session recorded at an older version passes the version check from a
    // newer connection, so the method dispatches through it. That is the
    // direction a v2 connection could not reach before.
    let created = v2.rpc("session.create", create(json!([]))).await;
    let other = created["result"]["sessionId"].as_str().unwrap();
    let reply = client.rpc("notes.echo", json!({"sessionId": other})).await;
    assert_ok(&reply);

    assert_ok(&client.rpc("session.close", json!({"sessionId": sid})).await);
    let closed = client.rpc("notes.echo", json!({"sessionId": sid})).await;
    assert_eq!(app_error(&closed), "session_closed");
    server.shutdown();
    server.join().await.unwrap();
}

/// A reloading provider affects only its namespace: admitted `Report` calls
/// are interrupted, and a withdrawn namespace is retryable until the next
/// generation registers it again. Closing the Session
/// interrupts its calls the same way.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reloading_or_closed_session_namespace_answers_as_specified() {
    let notes = Notes::default();
    let (server, _) = launch_with(Arc::new(MockTools), ServerConfig::default(), |builder| {
        notes.install(builder)
    })
    .await;
    let mut client = Client::new_version(&server, false, 3).await;
    let created = client.rpc("session.create", create(json!([]))).await;
    let sid = created["result"]["sessionId"].as_str().unwrap().to_owned();
    let wait = client.send("notes.wait", json!({"sessionId": sid})).await;
    notes.0.entered.notified().await;
    let gate = notes.hold_next();
    let restart = tokio::spawn({
        let notes = notes.clone();
        async move { notes.restart().await }
    });
    let interrupted = client.response(wait).await;
    assert_eq!(app_error(&interrupted), "operation_interrupted");
    notes.0.settled.notified().await;
    let reloading = client.rpc("notes.echo", json!({"sessionId": sid})).await;
    assert_eq!(reloading["error"]["code"], -32000);
    assert_eq!(app_error(&reloading), "service_reloading");
    assert_ok(&client.rpc("daemon.ping", json!({})).await);
    assert_eq!(
        client
            .rpc("session.methods", json!({"sessionId": sid}))
            .await["result"],
        json!({"methods": [{"name": "session.agent.get", "mutating": false}, {"name": "session.agent.update", "mutating": true}, {"name": "session.collaboration.approval.get", "mutating": false},
            {"name": "session.collaboration.cancel", "mutating": true},
            {"name": "session.collaboration.pending", "mutating": false},
            {"name": "session.input.pending", "mutating": false}, {"name": "session.input.withdraw", "mutating": true},
            {"name": "session.language.get", "mutating": false}, {"name": "session.language.update", "mutating": true},
            {"name": "session.tool.get", "mutating": false},
            {"name": "session.turn.get", "mutating": false}]})
    );
    gate.notify_one();
    restart.await.unwrap();
    assert_ok(&client.rpc("notes.echo", json!({"sessionId": sid})).await);

    let wait = client.send("notes.wait", json!({"sessionId": sid})).await;
    notes.0.entered.notified().await;
    let mut other = Client::new_version(&server, false, 3).await;
    assert_ok(&other.rpc("session.close", json!({"sessionId": sid})).await);
    assert_eq!(
        app_error(&client.response(wait).await),
        "operation_interrupted"
    );
    for method in ["notes.echo", "session.methods"] {
        let closed = client.rpc(method, json!({"sessionId": sid})).await;
        assert_eq!(app_error(&closed), "session_closed", "{method}");
    }
    assert!(
        !client
            .notices
            .iter()
            .any(|notice| notice["method"] == "management.progress"),
        "{:?}",
        client.notices
    );
    server.shutdown();
    server.join().await.unwrap();
}

/// Rpc acquires the Session through the index, which restores one that is
/// not loaded, before it dispatches.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_session_method_restores_an_unloaded_session() {
    let directory = std::env::temp_dir().join(format!("dim-methods-{}", uuid::Uuid::new_v4()));
    let config = ServerConfig {
        database_path: Some(directory.join("dimcode.sqlite")),
        ..Default::default()
    };
    let notes = Notes::default();
    let (server, _) = launch_with(Arc::new(MockTools), config.clone(), |builder| {
        notes.install(builder)
    })
    .await;
    let mut client = Client::new_version(&server, false, 3).await;
    let created = client.rpc("session.create", create(json!([]))).await;
    let sid = created["result"]["sessionId"].as_str().unwrap().to_owned();
    drop(client);
    server.shutdown();
    server.join().await.unwrap();

    let (server, agent) = launch_with(Arc::new(MockTools), config, |builder| {
        notes.install(builder)
    })
    .await;
    assert!(agent.get_session(&sid).is_none());
    let mut client = Client::new_version(&server, false, 3).await;
    assert_ok(&client.rpc("notes.echo", json!({"sessionId": sid})).await);
    assert!(agent.get_session(&sid).is_some());
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(directory).unwrap();
}

/// Reads frames until the reply to `id`, returning the progress of `id`
/// received before it.
async fn progress_until_reply(client: &mut Client, id: u64) -> (Vec<Value>, Value) {
    let mut progress = vec![];
    loop {
        let frame = client
            .read(&format!("response {id} or management.progress"))
            .await;
        if frame["id"] == id {
            return (progress, frame);
        }
        if frame["method"] == "management.progress" {
            assert_eq!(frame["params"]["id"], id, "{frame}");
            progress.push(frame["params"].clone());
        }
    }
}

/// Methods contract A.2 `progress`: a method that declares it sends progress
/// on its call, which reaches the calling connection only and before the
/// reply. Undeclared progress and progress after a disconnect are dropped,
/// and a disconnect cancels nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn call_progress_reaches_only_the_caller_before_its_reply() {
    let notes = Notes::default();
    let (server, _) = launch_with(Arc::new(MockTools), ServerConfig::default(), |builder| {
        notes.install(builder)
    })
    .await;
    let mut caller = Client::new_version(&server, false, 3).await;
    let created = caller.rpc("session.create", create(json!([]))).await;
    let sid = created["result"]["sessionId"].as_str().unwrap().to_owned();
    let mut watcher = Client::new_version(&server, true, 3).await;
    assert_ok(
        &watcher
            .rpc("session.attach", json!({"sessionId": sid}))
            .await,
    );

    let id = caller.send("notes.stream", json!({"sessionId": sid})).await;
    let (progress, reply) = progress_until_reply(&mut caller, id).await;
    assert_ok(&reply);
    assert_eq!(
        progress,
        (0..3)
            .map(|step| json!({"id": id, "method": "notes.stream", "data": {"step": step}}))
            .collect::<Vec<_>>()
    );
    let id = caller.send("notes.echo", json!({"sessionId": sid})).await;
    let (progress, reply) = progress_until_reply(&mut caller, id).await;
    assert_ok(&reply);
    assert!(progress.is_empty(), "{progress:?}");

    let (streamed, cancelled) = tokio::sync::oneshot::channel();
    *notes.0.streamed.lock().await = Some(streamed);
    let mut leaving = Client::new_version(&server, false, 3).await;
    let id = leaving
        .send("notes.stream", json!({"sessionId": sid, "hold": true}))
        .await;
    assert_eq!(
        leaving.notice("management.progress").await["params"]["id"],
        id
    );
    drop(leaving);
    tokio::time::timeout(Duration::from_secs(5), async {
        while caller.rpc("daemon.ping", json!({})).await["result"]["connections"] != 2 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the caller's connection closed");
    notes.0.release.notify_one();
    assert!(!cancelled.await.unwrap(), "a disconnect cancels nothing");
    assert_ok(&watcher.rpc("daemon.ping", json!({})).await);
    for client in [&caller, &watcher] {
        assert!(
            !client
                .notices
                .iter()
                .any(|notice| notice["method"] == "management.progress"),
            "{:?}",
            client.notices
        );
    }
    server.shutdown();
    server.join().await.unwrap();
}

/// Methods contract A.2: progress is best effort. A caller that reads nothing
/// while its call floods progress keeps its connection: the progress the
/// connection cannot queue is dropped, what arrives keeps its order before the
/// reply, and the reply arrives. A notice over the frame limit is dropped too.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn progress_a_caller_cannot_take_is_dropped_and_its_reply_arrives() {
    let notes = Notes::default();
    let (server, _) = launch_with(Arc::new(MockTools), ServerConfig::default(), |builder| {
        notes.install(builder)
    })
    .await;
    let mut caller = Client::new_version(&server, false, 3).await;
    let created = caller.rpc("session.create", create(json!([]))).await;
    let sid = created["result"]["sessionId"].as_str().unwrap().to_owned();
    let count = 2048;
    let id = caller
        .send(
            "notes.flood",
            json!({"sessionId": sid, "count": count, "bytes": 16 * 1024}),
        )
        .await;
    notes.0.flooded.notified().await;
    let (progress, reply) = progress_until_reply(&mut caller, id).await;
    assert_ok(&reply);
    let steps: Vec<u64> = progress
        .iter()
        .map(|progress| progress["data"]["step"].as_u64().unwrap())
        .collect();
    assert!(
        !steps.is_empty() && steps.len() < count,
        "{} of {count} arrived",
        steps.len()
    );
    assert!(steps.windows(2).all(|pair| pair[0] < pair[1]), "{steps:?}");

    let id = caller
        .send(
            "notes.flood",
            json!({"sessionId": sid, "count": 1, "bytes": 17 * 1024 * 1024}),
        )
        .await;
    let (progress, reply) = progress_until_reply(&mut caller, id).await;
    assert_ok(&reply);
    assert!(progress.is_empty());
    let id = caller.send("notes.stream", json!({"sessionId": sid})).await;
    let (progress, reply) = progress_until_reply(&mut caller, id).await;
    assert_ok(&reply);
    assert_eq!(progress.len(), 3, "{progress:?}");
    server.shutdown();
    server.join().await.unwrap();
}

/// Session access A.4: InputQueue serves `session.input.withdraw` through
/// `sessionMethods`; an applied or unknown input is `notHeld` and nothing
/// is written, and v3 history shows the command.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn input_withdraw_answers_through_the_session_and_writes_only_commands() {
    let (server, _) = launch_with(Arc::new(MockTools), ServerConfig::default(), |_| {}).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let created = client.rpc("session.create", create(json!([]))).await;
    let sid = created["result"]["sessionId"].as_str().unwrap().to_owned();
    let submitted = client
        .rpc(
            "session.submit",
            json!({"sessionId": sid, "branchId": "main", "text": "hello"}),
        )
        .await;
    let input_id = submitted["result"]["event"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_ok(
        &client
            .rpc(
                "session.idle",
                json!({"sessionId": sid, "branchId": "main"}),
            )
            .await,
    );
    for id in [input_id.as_str(), "unknown"] {
        let reply = client
            .rpc(
                "session.input.withdraw",
                json!({"sessionId": sid, "branchId": "main", "inputId": id, "reason": "changed my mind"}),
            )
            .await;
        assert_eq!(
            reply["result"],
            json!({"status": "notHeld"}),
            "{id}: {reply}"
        );
    }
    for params in [
        json!({"sessionId": sid, "branchId": "main"}),
        json!({"sessionId": sid, "branchId": "main", "inputId": ""}),
        json!({"sessionId": sid, "branchId": "main", "inputId": "x", "reason": "r".repeat(2049)}),
        json!({"sessionId": sid, "branchId": "main", "inputId": "x", "extra": 1}),
    ] {
        let reply = client.rpc("session.input.withdraw", params).await;
        assert!(reply.get("error").is_some(), "{reply}");
    }
    let history = client
        .rpc(
            "session.history",
            json!({"sessionId": sid, "branchId": "main", "after": 0, "limit": 100}),
        )
        .await;
    let names: Vec<_> = history["result"]["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|event| {
            event["name"]
                .as_str()
                .map(|name| format!("{}/{name}", event["type"].as_str().unwrap_or_default()))
        })
        .collect();
    assert_eq!(
        names
            .iter()
            .filter(|name| *name == "input/withdraw")
            .count(),
        2,
        "{names:?}"
    );
    assert!(
        !names.iter().any(|name| name == "input/deleted"),
        "{names:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn independent_clients_replace_a_held_input_only_once() {
    let (server, mut requests, release) = session_typescript::start(ServerConfig::default()).await;
    let mut first = Client::new_version(&server, false, 3).await;
    let mut second = Client::new_version(&server, false, 3).await;
    let mut params = create(json!(["native_slow"]));
    params["permissionPolicy"]["allowedTools"] = json!(["native_slow"]);
    let created = first.rpc("session.create", params).await;
    assert_ok(&created);
    let sid = created["result"]["sessionId"].as_str().unwrap();
    let driving = first
        .rpc(
            "session.submit",
            json!({
                "sessionId": sid, "branchId": "main", "text": "tool:native_slow {}"
            }),
        )
        .await;
    assert_ok(&driving);
    requests.recv().await.unwrap();
    let queued = first
        .rpc(
            "session.submit",
            json!({
                "sessionId": sid, "branchId": "main", "text": "original queued input"
            }),
        )
        .await;
    assert_ok(&queued);
    let input_id = queued["result"]["event"]["id"].as_str().unwrap();
    let steer = |id: &str, text: &str| {
        json!({
            "sessionId": sid, "branchId": "main", "inputId": id,
            "steer": {"content": [{"type": "text", "text": text}]}
        })
    };
    let (a, b) = tokio::join!(
        first.rpc("session.input.withdraw", steer(input_id, "candidate A")),
        second.rpc("session.input.withdraw", steer(input_id, "candidate B")),
    );
    assert_ok(&a);
    assert_ok(&b);
    let (winner, loser) = if a["result"]["status"] == "deleted" {
        (&a, &b)
    } else {
        (&b, &a)
    };
    assert_eq!(loser["result"], json!({"status": "notHeld"}));
    let receipt = &winner["result"]["steer"];
    assert!(receipt["inputId"].is_string(), "{winner}");
    assert_eq!(
        receipt["sequence"].as_u64().unwrap() + 1,
        winner["result"]["sequence"]
    );
    for id in [
        input_id,
        "unknown",
        driving["result"]["event"]["id"].as_str().unwrap(),
    ] {
        let reply = second
            .rpc("session.input.withdraw", steer(id, "must not appear"))
            .await;
        assert_eq!(reply["result"], json!({"status": "notHeld"}), "{reply}");
    }
    let session = server.agent().unwrap().get_session(sid).unwrap();
    let events = chain_events(&session, "main", ..).await.unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(&event.kind,
        EventKind::InputDeleted(data) if data.input_id == input_id))
            .count(),
        1
    );
    let replacements: Vec<_> = events
        .iter()
        .filter(|event| {
            matches!(&event.kind,
        EventKind::InputMessage(data) if data.mode == Some(InputMode::Steer))
        })
        .collect();
    assert_eq!(replacements.len(), 1);
    assert_eq!(replacements[0].id, receipt["inputId"]);
    assert_eq!(replacements[0].sequence, receipt["sequence"]);
    let mut distinct = Vec::new();
    for _ in 0..2 {
        let reply = first.rpc("session.submit", json!({
            "sessionId": sid, "branchId": "main", "text": "identical queued text", "mode": "queue"
        })).await;
        assert_ok(&reply);
        distinct.push(reply["result"]["event"]["id"].as_str().unwrap().to_owned());
    }
    let (a, b) = tokio::join!(
        first.rpc(
            "session.input.withdraw",
            steer(&distinct[0], "identical replacement")
        ),
        second.rpc(
            "session.input.withdraw",
            steer(&distinct[1], "identical replacement")
        ),
    );
    for reply in [&a, &b] {
        assert_eq!(reply["result"]["status"], "deleted", "{reply}");
        assert!(reply["result"]["steer"]["inputId"].is_string());
    }
    assert_ne!(
        a["result"]["steer"]["inputId"],
        b["result"]["steer"]["inputId"]
    );
    for _ in 0..2 {
        assert_ok(&first.rpc("session.submit", json!({
            "sessionId": sid, "branchId": "main", "text": "identical replacement", "mode": "steer"
        })).await);
    }
    let plain = first.rpc("session.submit", json!({
        "sessionId": sid, "branchId": "main", "text": "plain withdrawal wins", "mode": "queue"
    })).await;
    assert_ok(&plain);
    let plain_id = plain["result"]["event"]["id"].as_str().unwrap();
    let removed = first
        .rpc(
            "session.input.withdraw",
            json!({
                "sessionId": sid, "branchId": "main", "inputId": plain_id
            }),
        )
        .await;
    assert_eq!(removed["result"]["status"], "deleted", "{removed}");
    let losing = second
        .rpc("session.input.withdraw", steer(plain_id, "must not appear"))
        .await;
    assert_eq!(losing["result"], json!({"status": "notHeld"}));
    release.notify_one();
    session.idle("main").await.unwrap();
    let events = chain_events(&session, "main", ..).await.unwrap();
    assert!(!events.iter().any(|event| matches!(&event.kind,
        EventKind::InputConsumed(data) if data.input_id == input_id)));
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(&event.kind,
        EventKind::InputConsumed(data) if data.input_id == replacements[0].id))
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(&event.kind,
        EventKind::InputMessage(data) if data.mode == Some(InputMode::Steer)))
            .count(),
        5
    );
    server.shutdown();
    server.join().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tool_origin_reads_complete_arguments_at_a_fixed_cut_without_executing_again() {
    let (server, agent) = launch_with_agent(Arc::new(MockTools), ServerConfig::default()).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let created=client.rpc("session.create",json!({"cwd":std::env::temp_dir(),"tools":["echo","branch"],"permissionPolicy":{"allowedTools":["echo","branch"]}})).await;
    assert_ok(&created);
    let id = created["result"]["sessionId"].as_str().unwrap();
    let session = agent.get_session(id).unwrap();
    assert_ok(
        &client
            .rpc(
                "session.submit",
                json!({"sessionId":id,"branchId":"main","text":r#"tool:echo {"text":"the original arguments"}"#}),
            )
            .await,
    );
    session.idle("main").await.unwrap();
    let history = chain_events(&session, "main", ..).await.unwrap();
    let origin = history
        .iter()
        .find(|event| matches!(event.kind, EventKind::ToolUse(_)))
        .unwrap();
    let EventKind::ToolUse(data) = &origin.kind else {
        unreachable!()
    };
    let head = session.sequence("main").unwrap();
    let params = json!({"sessionId":id,"branchId":"main","requestId":data.request_id,"callId":data.call.id,"through":head});
    let read = client.rpc("session.tool.get", params.clone()).await;
    assert_ok(&read);
    assert_eq!(read["result"]["through"], head);
    assert_eq!(read["result"]["call"], serde_json::to_value(data).unwrap());
    let mut earlier = params.clone();
    earlier["through"] = json!(origin.sequence - 1);
    let read = client.rpc("session.tool.get", earlier).await;
    assert_ok(&read);
    assert!(read["result"]["call"].is_null());
    let mut other = params;
    other["requestId"] = json!("another-request");
    let read = client.rpc("session.tool.get", other).await;
    assert_ok(&read);
    assert!(read["result"]["call"].is_null());
    assert_eq!(
        session.sequence("main").unwrap(),
        head,
        "origin reads are nonmutating"
    );
    let child=client.rpc("session.branch_create",json!({"sessionId":id,"parentBranchId":"main","at":head,"prompt":r#"tool:echo {"text":"child arguments"}"#})).await;
    assert_ok(&child);
    let child = child["result"]["branch"]["branchId"]
        .as_str()
        .unwrap()
        .to_owned();
    session.idle(&child).await.unwrap();
    let history = chain_events(&session, &child, ..).await.unwrap();
    let data = history
        .iter()
        .rev()
        .find_map(|event| match &event.kind {
            EventKind::ToolUse(data) => Some(data),
            _ => None,
        })
        .unwrap();
    assert_ok(
        &client
            .rpc(
                "session.branch_end",
                json!({"sessionId":id,"branchId":child}),
            )
            .await,
    );
    let read=client.rpc("session.tool.get",json!({"sessionId":id,"branchId":child,"requestId":data.request_id,"callId":data.call.id})).await;
    assert_ok(&read);
    assert_eq!(
        read["result"]["call"],
        serde_json::to_value(data).unwrap(),
        "an ended branch retains its committed call arguments"
    );
    drop(client);
    drop(agent);
    server.shutdown();
    server.join().await.unwrap();
}

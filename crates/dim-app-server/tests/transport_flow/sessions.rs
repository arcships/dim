//! The Agents `sessions` service (sessions service design §16), called by
//! name through `Keyed<dyn HostDispatch>` as a process plugin calls it.
use super::*;
use dim_agent_capabilities::permission::{
    CommandRulesHost, NoCommandRules, Permission, PermissionState,
};
use dim_agent_runtime::{Agent, Session, SessionScope};
use dim_agent_types::{InputBlock, InputMessageData, PermissionPolicy, UserInput};
use dim_app_server::methods::ManagementMethods;
use rutis::{DepKey, Keyed};
use rutis_bridge::session::HostDispatch;
use std::sync::OnceLock;

#[path = "workflow_reports.rs"]
mod workflow_reports;

/// The `sessions` service as a consumer injects it. `serve` takes no extra
/// process rows, so a Session part injects it by name for the test.
#[derive(Clone, Default)]
struct Sessions {
    service: Arc<OnceLock<Arc<dyn HostDispatch>>>,
    context: Arc<OnceLock<rutis::Ctx>>,
    /// How often the consumer part applied; an index reload must not reload it.
    applied: Arc<AtomicUsize>,
}
struct SessionsConsumer(Sessions);
impl TypedPlugin for SessionsConsumer {
    type Deps = Keyed<dyn HostDispatch>;
    fn name(&self) -> &str {
        "sessions-consumer"
    }
    fn apply<'a>(
        &'a self,
        ctx: &'a rutis::Ctx,
        Keyed(service): Self::Deps,
    ) -> rutis::BoxFuture<'a, Result<rutis::Effect, rutis::CordisError>> {
        Box::pin(async move {
            self.0.applied.fetch_add(1, Ordering::SeqCst);
            let _ = self.0.service.set(service);
            let _ = self.0.context.set(ctx.clone());
            Ok(rutis::Effect::Done)
        })
    }
}
impl Capability for SessionsConsumer {
    fn keys(&self) -> KeysOf<Self> {
        DepKey::dynamic("sessions")
    }
}
impl Sessions {
    /// One call with one request object; the reply envelope.
    async fn call(&self, method: &str, request: Value) -> Value {
        self.raw(method, json!([request])).await
    }
    async fn raw(&self, method: &str, args: Value) -> Value {
        let service = self.service.get().expect("a Session injected sessions");
        match service.invoke(method, args.into()).unwrap() {
            rutis_bridge::session::Value::Reference(reply) if reply.is_future() => {
                reply.wait_async().await.unwrap().json().unwrap()
            }
            reply => panic!("sessions replies asynchronously: {:?}", reply.json()),
        }
    }
    async fn ok(&self, method: &str, request: Value) -> Value {
        let reply = self.call(method, request).await;
        assert_eq!(reply["ok"], true, "{reply}");
        reply["value"].clone()
    }
    /// The error code; asserts the envelope's shape.
    async fn error(&self, method: &str, request: Value) -> String {
        let reply = self.call(method, request).await;
        assert_eq!(reply["ok"], false, "{reply}");
        assert!(reply["error"]["message"].is_string(), "{reply}");
        let code = reply["error"]["code"].as_str().unwrap().to_owned();
        assert_eq!(
            reply["error"]["retryable"],
            matches!(code.as_str(), "reloading" | "publication_unknown"),
            "{reply}"
        );
        code
    }
}

/// Host Session defaults of these tests: the bound of a launch without a
/// starter.
fn host_policy() -> Value {
    json!({"preset":"workspace-write","allowedTools":["confirm"],"disallowedTools":["blocked"]})
}
struct Served {
    server: RunningServer,
    agent: Agent,
    sessions: Sessions,
    client: Client,
    /// Executions of the `confirm` tool, which asks for approval.
    calls: Arc<AtomicUsize>,
}
/// Permission's command rules from a Session part that reloads with the
/// Agents index, as it injects Collaboration, which injects the index. While
/// held, a reload waits in `apply`, so every branch's Permission, and with it
/// `PermissionState`, stays unloaded.
#[derive(Clone)]
struct HeldRules {
    hold: Arc<tokio::sync::watch::Sender<bool>>,
    /// Notified when a held reload waits.
    held: Arc<tokio::sync::Notify>,
}
struct HeldRulesPart(HeldRules, SessionScope);
impl TypedPlugin for HeldRulesPart {
    type Deps = (Arc<dim_app_server::collaboration::Collaboration>,);
    fn name(&self) -> &str {
        "held-rules"
    }
    fn apply<'a>(
        &'a self,
        ctx: &'a rutis::Ctx,
        _: Self::Deps,
    ) -> rutis::BoxFuture<'a, Result<rutis::Effect, rutis::CordisError>> {
        Box::pin(async move {
            let mut hold = self.0.hold.subscribe();
            if *hold.borrow_and_update() {
                self.0.held.notify_one();
                let _ = hold.wait_for(|held| !held).await;
            }
            ctx.provide_as(
                self.1.key::<CommandRulesHost>(),
                Arc::new(CommandRulesHost(Arc::new(NoCommandRules))),
            )?;
            Ok(rutis::Effect::Done)
        })
    }
}
impl Capability for HeldRulesPart {
    fn keys(&self) -> KeysOf<Self> {
        scoped::<Self::Deps, _>(&self.1)
    }
}

/// How a test's daemon differs from the default one.
#[derive(Default)]
struct Setup {
    database: Option<std::path::PathBuf>,
    home: Option<std::path::PathBuf>,
    typescript: Option<dim_app_server::session_typescript::SessionTypeScriptConfig>,
    store: Option<Arc<dyn dim_agent_runtime::TimelineStore>>,
    controls: Option<tokio::sync::mpsc::UnboundedSender<super::session_typescript::Control>>,
    /// Binds Permission's command rules through `HeldRules`.
    rules: Option<HeldRules>,
    /// Configured models: launches resolve through a model-selection host.
    models: Option<Arc<dim_agent_aimux::AimuxModels>>,
}
async fn serve_sessions(database: Option<std::path::PathBuf>) -> Served {
    serve_with(Setup {
        database,
        ..Default::default()
    })
    .await
}
/// Without Collaboration, whose Session parts reload with the Agents index,
/// so a pending approval outlives an index reload; only `Setup::rules` adds
/// a part that reloads with it.
async fn serve_with(setup: Setup) -> Served {
    let sessions = Sessions::default();
    let consumer = sessions.clone();
    let calls = Arc::new(AtomicUsize::new(0));
    let mut config = ServerConfig {
        database_path: setup.database,
        model_catalog: setup.models,
        product_home: setup.home,
        session_typescript: setup.typescript,
        unix_path: Some(
            std::path::PathBuf::from("/tmp")
                .join(format!("dim-sessions-{}/server.sock", uuid::Uuid::new_v4())),
        ),
        ..Default::default()
    };
    config.session_defaults.tools = vec!["confirm".into()];
    config.session_defaults.permission_policy = serde_json::from_value(host_policy()).unwrap();
    let mut builder = AgentBuilder::new(AgentConfig::default());
    builder.timeline_store(
        setup
            .store
            .unwrap_or_else(|| Arc::new(dim_agent_runtime::MemoryTimelineStore::default())),
    );
    if let Some(controls) = setup.controls {
        builder.session_plugin(move |scope| {
            super::session_typescript::ControlledGate(*scope, controls.clone())
        });
    }
    match setup.rules {
        None => defaults::install(&mut builder),
        Some(rules) => {
            use dim_agent_capabilities::{
                host::{EmptyPromptHost, PromptHost},
                model_provider::ModelProviderPlugin,
                prompt::InitialPromptConfiguration,
            };
            builder
                .session_value(Arc::new(PromptHost(Arc::new(EmptyPromptHost))))
                .session_value(Arc::new(InitialPromptConfiguration(None)))
                .session_plugin(move |scope| HeldRulesPart(rules.clone(), *scope));
            defaults::install_with_prompt_capability(&mut builder, ModelProviderPlugin::new);
        }
    }
    defaults::mock_model_hosts(&mut builder);
    dim_agent_capabilities::defaults::tool_host(
        &mut builder,
        Arc::new(ConfirmTool {
            calls: calls.clone(),
            seen: Arc::new(Mutex::new(vec![])),
        }),
    );
    builder.session_plugin(move |_| SessionsConsumer(consumer.clone()));
    let server = serve(builder, config).await.unwrap();
    let agent = server.agent().unwrap();
    let client = Client::new_version(&server, false, 3).await;
    Served {
        server,
        agent,
        sessions,
        client,
        calls,
    }
}
/// A client Session; creating it also gives the test its consumer part.
async fn client_session(client: &mut Client, agent: &Agent, policy: Value) -> Session {
    let created = client
        .rpc(
            "session.create",
            json!({"cwd":std::env::temp_dir(),"tools":["confirm"],"permissionPolicy":policy}),
        )
        .await;
    assert_ok(&created);
    agent
        .get_session(created["result"]["sessionId"].as_str().unwrap())
        .unwrap()
}
fn policy(value: Value) -> PermissionPolicy {
    serde_json::from_value(value).unwrap()
}
/// The launched Session is non-interactive, starts with the effective policy
/// and is served to current clients.
async fn assert_launched(client: &mut Client, agent: &Agent, reply: &Value, expected: Value) {
    assert_eq!(reply["policy"], expected);
    let session = agent
        .get_session(reply["sessionId"].as_str().unwrap())
        .unwrap();
    assert!(!session.config().interactive);
    assert_eq!(session.config().permission_policy, policy(expected.clone()));
    assert_eq!(
        session.get::<PermissionState>("main").unwrap().unwrap(),
        PermissionState {
            version: 0,
            policy: policy(expected),
        }
    );
    let listed = client.rpc("session.list", json!({})).await;
    assert!(
        listed["result"]
            .as_array()
            .unwrap()
            .iter()
            .any(|info| info["sessionId"] == reply["sessionId"]),
        "{listed}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn launch_caps_the_policy_by_the_starters_current_policy_or_the_host_defaults() {
    let Served {
        server,
        agent,
        sessions,
        mut client,
        ..
    } = serve_sessions(None).await;
    let cwd = std::env::temp_dir();
    // Created with full access, then narrowed: the bound is the dispatched
    // policy, not the creation-time config.
    let starter = client_session(&mut client, &agent, json!({"preset":"full-access"})).await;
    let narrowed = json!({"preset":"workspace-write","allowedTools":["confirm","echo"],"disallowedTools":["wait"]});
    starter
        .service::<Permission>("main")
        .unwrap()
        .update(0, policy(narrowed.clone()))
        .await
        .unwrap();
    let at = json!({"sessionId":starter.id(),"branchId":"main"});

    let reply = sessions
        .ok(
            "launch",
            json!({"cwd":cwd,"starter":at,"policy":{"preset":"full-access","allowedTools":["confirm","other"],"disallowedTools":["x"]}}),
        )
        .await;
    assert_launched(
        &mut client,
        &agent,
        &reply,
        json!({"preset":"workspace-write","allowedTools":["confirm","echo"],"disallowedTools":["x","wait"]}),
    )
    .await;
    // No requested policy: the starter's own.
    let reply = sessions.ok("launch", json!({"cwd":cwd,"starter":at})).await;
    assert_launched(&mut client, &agent, &reply, narrowed).await;
    // No starter: capped by the host defaults; no policy means read-only.
    let reply = sessions
        .ok(
            "launch",
            json!({"cwd":cwd,"policy":{"preset":"full-access"}}),
        )
        .await;
    assert_launched(&mut client, &agent, &reply, host_policy()).await;
    let reply = sessions.ok("launch", json!({"cwd":cwd})).await;
    assert_launched(
        &mut client,
        &agent,
        &reply,
        json!({"preset":"read-only","allowedTools":[],"disallowedTools":["blocked"]}),
    )
    .await;

    for (request, code) in [
        (json!({"cwd":cwd,"agent":"missing"}), "agent_unknown"),
        (json!({"cwd":"relative"}), "rejected"),
        (
            json!({"cwd":cwd,"policy":{"allowedTools":[" "]}}),
            "rejected",
        ),
        (json!({"cwd":cwd,"source":"dim-scheduler"}), "rejected"),
        (
            json!({"cwd":cwd,"starter":{"sessionId":"missing","branchId":"main"}}),
            "session_closed",
        ),
        (
            json!({"cwd":cwd,"starter":{"sessionId":starter.id(),"branchId":"missing"}}),
            "branch_missing",
        ),
    ] {
        assert_eq!(
            sessions.error("launch", request.clone()).await,
            code,
            "{request}"
        );
    }
    assert_ok(
        &client
            .rpc("session.close", json!({"sessionId":starter.id()}))
            .await,
    );
    assert_eq!(
        sessions
            .error("launch", json!({"cwd":cwd,"starter":at}))
            .await,
        "session_closed"
    );
    // Shape errors are data too.
    for (method, args) in [
        ("launch", json!([])),
        ("launch", json!([{"cwd":cwd}, {}])),
        ("close", json!([{}])),
    ] {
        let reply = sessions.raw(method, args).await;
        assert_eq!(reply["error"]["code"], "rejected", "{reply}");
    }
    assert_eq!(agent.sessions().len(), 4, "failed launches create nothing");
    server.shutdown();
    server.join().await.unwrap();
}

/// A private directory for a native store, removed on drop.
struct Store(std::path::PathBuf);
impl Store {
    fn new() -> Self {
        use std::os::unix::fs::DirBuilderExt;
        let path = std::path::PathBuf::from("/tmp").join(format!("dss-{}", uuid::Uuid::new_v4()));
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&path)
            .unwrap();
        Self(path)
    }
    fn database(&self) -> std::path::PathBuf {
        self.0.join("dimcode.sqlite")
    }
}
impl Drop for Store {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
async fn launched(sessions: &Sessions, agent: &Agent, policy: Value) -> Session {
    let reply = sessions
        .ok(
            "launch",
            json!({"cwd":std::env::temp_dir(),"policy":policy}),
        )
        .await;
    agent
        .get_session(reply["sessionId"].as_str().unwrap())
        .unwrap()
}
fn input(session: &Session, submission: &str, content: Value) -> Value {
    json!({"sessionId":session.id(),"branchId":"main","mode":"queue","submissionId":submission,
        "rawBlocks":[{"text":"<dim-test kind=\"run\">marker</dim-test>"}],"content":content})
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn input_replies_after_publication_and_lookup_waits_for_committed_inputs() {
    let store = Store::new();
    let Served {
        server,
        agent,
        sessions,
        mut client,
        ..
    } = serve_sessions(Some(store.database())).await;
    client_session(&mut client, &agent, host_policy()).await;
    let session = launched(&sessions, &agent, json!({})).await;
    let submission = format!("test:{}", uuid::Uuid::new_v4());
    let mut request = input(
        &session,
        &submission,
        json!([{"type":"text","text":"hello"}]),
    );
    request["rawBlocks"].as_array_mut().unwrap().extend([
        json!({"text":"live","key":{"use":"live","name":"test/live"}}),
        json!({"text":"retire","key":{"use":"retire","name":"test/old"}}),
    ]);
    let receipt = sessions.ok("input", request).await;
    let sequence = receipt["sequence"].as_u64().unwrap();
    // Replied after publication: readers see it at once.
    assert!(session.sequence("main").unwrap() >= sequence);
    let event = session
        .local("main", sequence..=sequence)
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(event.id, receipt["inputId"].as_str().unwrap());
    let EventKind::InputMessage(data) = &event.kind else {
        panic!("{event:?}")
    };
    assert_eq!(
        *data,
        InputMessageData {
            submission_id: Some(submission.clone()),
            user: Some(UserInput {
                content: vec![dim_agent_types::ContentBlock::Text {
                    text: "hello".into()
                }],
            }),
            blocks: vec![
                InputBlock::Raw {
                    text: "<dim-test kind=\"run\">marker</dim-test>".into(),
                    key: None,
                    retire: false,
                },
                InputBlock::Raw {
                    text: "live".into(),
                    key: Some("test/live".into()),
                    retire: false,
                },
                InputBlock::Raw {
                    text: "retire".into(),
                    key: Some("test/old".into()),
                    retire: true,
                },
            ],
            mode: Some(dim_agent_types::InputMode::Queue),
        }
    );
    let at = |submission: &str| json!({"sessionId":session.id(),"branchId":"main","submissionId":submission});
    assert_eq!(
        sessions.ok("lookup", at(&submission)).await,
        json!({"status":"accepted","inputId":event.id,"sequence":sequence})
    );
    assert_eq!(
        sessions.ok("lookup", at("test:other")).await,
        json!({"status":"unknown"})
    );
    for _ in 0..2 {
        sessions
            .ok("input", input(&session, "test:twice", json!([])))
            .await;
    }
    assert_eq!(
        sessions.error("lookup", at("test:twice")).await,
        "ambiguous"
    );
    let mut missing_mode = input(&session, "test:invalid", json!([]));
    missing_mode.as_object_mut().unwrap().remove("mode");
    let mut empty = input(&session, "test:invalid", json!([{"type":"text","text":""}]));
    empty["rawBlocks"] = json!([]);
    let mut bad_key = input(&session, "test:invalid", json!([]));
    bad_key["rawBlocks"][0]["key"] = json!({"use":"forever","name":"test/key"});
    let mut no_branch = input(&session, "test:invalid", json!([]));
    no_branch["branchId"] = json!("missing");
    let mut no_session = input(&session, "test:invalid", json!([]));
    no_session["sessionId"] = json!("missing");
    for (request, code) in [
        (input(&session, "", json!([])), "rejected"),
        (missing_mode, "rejected"),
        (empty, "rejected"),
        (bad_key, "rejected"),
        (no_branch, "branch_missing"),
        (no_session, "session_closed"),
    ] {
        assert_eq!(
            sessions.error("input", request.clone()).await,
            code,
            "{request}"
        );
    }
    assert_eq!(sessions.error("lookup", at("")).await, "rejected");
    assert_eq!(
        sessions.ok("lookup", at("test:invalid")).await,
        json!({"status":"unknown"}),
        "rejected inputs write nothing"
    );

    // Hold publication: the input commits, its reply waits, and a lookup
    // that starts after the commit waits too instead of reading `unknown`.
    session.idle("main").await.unwrap();
    let publication = session.publication("main").unwrap();
    let lock = rusqlite::Connection::open(store.database()).unwrap();
    lock.execute_batch("BEGIN IMMEDIATE").unwrap();
    let pending = format!("test:{}", uuid::Uuid::new_v4());
    let write = tokio::spawn({
        let sessions = sessions.clone();
        let request = input(&session, &pending, json!([]));
        async move { sessions.call("input", request).await }
    });
    tokio::time::timeout(Duration::from_secs(5), async {
        while !publication.has_pending() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let found = tokio::spawn({
        let sessions = sessions.clone();
        let request = at(&pending);
        async move { sessions.call("lookup", request).await }
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(!write.is_finished() && !found.is_finished());
    lock.execute_batch("COMMIT").unwrap();
    let receipt = write.await.unwrap();
    assert_eq!(receipt["ok"], true, "{receipt}");
    assert_eq!(
        found.await.unwrap()["value"],
        json!({"status":"accepted","inputId":receipt["value"]["inputId"],"sequence":receipt["value"]["sequence"]})
    );
    server.shutdown();
    server.join().await.unwrap();
}

/// Waits until the branch holds an event of `key`.
async fn event_written(session: &Session, key: &str) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !session
            .history("main", None)
            .await
            .unwrap()
            .iter()
            .any(|event| event.kind.key() == key)
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("event was written");
}
/// A `confirm` call waits for approval: workspace-write asks for it, and the
/// host bound allows it unconditionally only where the request does too.
async fn awaiting_approval(sessions: &Sessions, agent: &Agent) -> Session {
    let session = launched(sessions, agent, json!({"preset":"workspace-write"})).await;
    sessions
        .ok(
            "input",
            input(
                &session,
                "test:confirm",
                json!([{"type":"text","text":"tool:confirm {}"}]),
            ),
        )
        .await;
    event_written(&session, "approval/request").await;
    session
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn idle_covers_written_turns_and_waits_on_a_pending_approval_until_stop() {
    let Served {
        server,
        agent,
        sessions,
        mut client,
        calls,
    } = serve_sessions(None).await;
    client_session(&mut client, &agent, host_policy()).await;
    let session = launched(&sessions, &agent, json!({})).await;
    let at = json!({"sessionId":session.id(),"branchId":"main"});
    let receipt = sessions
        .ok(
            "input",
            input(
                &session,
                "test:hello",
                json!([{"type":"text","text":"hello"}]),
            ),
        )
        .await;
    let cutoff = sessions.ok("idle", at.clone()).await["sequence"]
        .as_u64()
        .unwrap();
    let history = session.local("main", ..=cutoff).await.unwrap();
    let consumed = history
        .iter()
        .position(|event| matches!(&event.kind, EventKind::InputConsumed(data) if data.input_id == receipt["inputId"]))
        .expect("the input was consumed by the cutoff");
    assert!(
        history[consumed..]
            .iter()
            .any(|event| matches!(event.kind, EventKind::TurnEnd(_))),
        "its turn ended by the cutoff"
    );
    // An idle branch answers at once with the same cutoff.
    assert_eq!(sessions.ok("idle", at.clone()).await["sequence"], cutoff);

    let session = awaiting_approval(&sessions, &agent).await;
    let at = json!({"sessionId":session.id(),"branchId":"main"});
    let idle = tokio::spawn({
        let sessions = sessions.clone();
        let at = at.clone();
        async move { sessions.call("idle", at).await }
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        !idle.is_finished(),
        "a pending approval keeps the turn open"
    );
    let stopped = sessions.ok("stop", at.clone()).await["sequence"]
        .as_u64()
        .unwrap();
    let idle = idle.await.unwrap();
    assert_eq!(idle["ok"], true, "{idle}");
    let cutoff = idle["value"]["sequence"].as_u64().unwrap();
    assert!(cutoff >= stopped);
    let history = session.local("main", ..=cutoff).await.unwrap();
    let interrupt = history
        .iter()
        .position(|event| matches!(&event.kind, EventKind::StopInterrupt(data) if data.reason == "Stopped by automation"))
        .expect("stop wrote its interrupt");
    assert!(
        history[interrupt..]
            .iter()
            .any(|event| matches!(event.kind, EventKind::TurnEnd(_)))
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0, "the asked tool never ran");

    for (method, request, code) in [
        (
            "idle",
            json!({"sessionId":session.id(),"branchId":"missing"}),
            "branch_missing",
        ),
        (
            "stop",
            json!({"sessionId":"missing","branchId":"main"}),
            "session_closed",
        ),
        (
            "idle",
            json!({"sessionId":session.id(),"branchId":"main","timeoutMs":10}),
            "rejected",
        ),
    ] {
        assert_eq!(sessions.error(method, request).await, code);
    }
    server.shutdown();
    server.join().await.unwrap();
}

/// An index reload after launch admitted creation must not reply a retryable
/// `reloading` while the Session is still created: the reply is the
/// creation's own outcome (§6.1).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn launch_admitted_before_an_index_reload_replies_the_created_session() {
    let store = Store::new();
    let Served {
        server,
        agent,
        sessions,
        mut client,
        ..
    } = serve_sessions(Some(store.database())).await;
    client_session(&mut client, &agent, host_policy()).await;
    let before = agent.sessions().len();
    // Hold the writer so creation waits after admission, on its rows.
    let lock = rusqlite::Connection::open(store.database()).unwrap();
    lock.execute_batch("BEGIN IMMEDIATE").unwrap();
    let launch = tokio::spawn({
        let sessions = sessions.clone();
        let cwd = std::env::temp_dir();
        async move { sessions.call("launch", json!({"cwd":cwd})).await }
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!launch.is_finished());
    let (restarted, ()) = tokio::join!(server.restart("Agents"), async {
        tokio::time::sleep(Duration::from_millis(100)).await;
        lock.execute_batch("COMMIT").unwrap();
    });
    restarted.unwrap();
    let reply = launch.await.unwrap();
    assert_eq!(reply["ok"], true, "{reply}");
    let session = agent
        .get_session(reply["value"]["sessionId"].as_str().unwrap())
        .unwrap();
    assert!(!session.config().interactive);
    assert_eq!(agent.sessions().len(), before + 1);
    server.shutdown();
    server.join().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn idle_ends_reloading_on_an_index_reload_and_session_closed_on_close() {
    let Served {
        server,
        agent,
        sessions,
        mut client,
        ..
    } = serve_sessions(None).await;
    client_session(&mut client, &agent, host_policy()).await;
    let session = awaiting_approval(&sessions, &agent).await;
    let at = json!({"sessionId":session.id(),"branchId":"main"});
    let waiting = || {
        let sessions = sessions.clone();
        let at = at.clone();
        tokio::spawn(async move { sessions.call("idle", at).await })
    };

    let applied = sessions.applied.load(Ordering::SeqCst);
    let idle = waiting();
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(!idle.is_finished());
    server.restart("Agents").await.unwrap();
    let ended = idle.await.unwrap();
    assert_eq!(ended["error"]["code"], "reloading", "{ended}");
    assert_eq!(ended["error"]["retryable"], true);
    // The consumer stayed loaded, and its next call reaches the new index.
    assert_eq!(sessions.applied.load(Ordering::SeqCst), applied);
    let idle = waiting();
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(!idle.is_finished(), "the approval is still pending");

    // A close settles the turn; the wait must not report that as idle.
    session.close().await.unwrap();
    let ended = idle.await.unwrap();
    assert_eq!(ended["error"]["code"], "session_closed", "{ended}");
    assert_eq!(sessions.error("idle", at.clone()).await, "session_closed");
    server.shutdown();
    server.join().await.unwrap();
}

/// Issue #62: a process plugin launches a Session, writes input, waits for
/// idle and withdraws a held input by its `inputId`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_consumer_launches_writes_waits_and_withdraws_by_input_id() {
    let Served {
        server,
        agent,
        sessions,
        mut client,
        calls,
    } = serve_sessions(None).await;
    client_session(&mut client, &agent, host_policy()).await;
    // The first input's approval keeps its turn open, so the second is held.
    let session = awaiting_approval(&sessions, &agent).await;
    let at = json!({"sessionId":session.id(),"branchId":"main"});
    let first = sessions
        .ok(
            "lookup",
            json!({"sessionId":session.id(),"branchId":"main","submissionId":"test:confirm"}),
        )
        .await;
    let held = sessions
        .ok(
            "input",
            input(
                &session,
                "test:held",
                json!([{"type":"text","text":"later"}]),
            ),
        )
        .await;
    let withdraw = |input: &Value, reason: Option<&str>| {
        let mut request = at.clone();
        request["inputId"] = input["inputId"].clone();
        if let Some(reason) = reason {
            request["reason"] = json!(reason);
        }
        request
    };
    let deleted = sessions
        .ok("withdraw", withdraw(&held, Some("superseded")))
        .await;
    assert_eq!(deleted["status"], "deleted", "{deleted}");
    let sequence = deleted["sequence"].as_u64().unwrap();
    // Replied after publication.
    assert!(session.sequence("main").unwrap() >= sequence);
    // Already deleted, applied or unknown: nothing is written.
    for request in [
        withdraw(&held, None),
        withdraw(&first, None),
        withdraw(&json!({"inputId":"unknown"}), None),
    ] {
        assert_eq!(
            sessions.ok("withdraw", request).await,
            json!({"status":"notHeld"})
        );
    }
    let mut no_branch = withdraw(&held, None);
    no_branch["branchId"] = json!("missing");
    for (request, code) in [
        (withdraw(&json!({"inputId":""}), None), "rejected"),
        (withdraw(&held, Some(&"x".repeat(4096))), "rejected"),
        (no_branch, "branch_missing"),
    ] {
        assert_eq!(sessions.error("withdraw", request).await, code);
    }

    sessions.ok("stop", at.clone()).await;
    let cutoff = sessions.ok("idle", at).await["sequence"].as_u64().unwrap();
    let history = session.local("main", ..=cutoff).await.unwrap();
    let held = held["inputId"].as_str().unwrap();
    let first = first["inputId"].as_str().unwrap();
    assert!(history.iter().any(|event| matches!(&event.kind,
        EventKind::InputWithdraw(data) if data.input_id == held && data.reason.as_deref() == Some("superseded"))));
    assert!(history.iter().any(|event| event.sequence == sequence
        && matches!(&event.kind,
        EventKind::InputDeleted(data) if data.input_id == held)));
    assert!(!history.iter().any(|event| matches!(&event.kind,
        EventKind::InputApplied(data) if data.input_id == held)));
    assert!(history.iter().any(|event| matches!(&event.kind,
        EventKind::InputConsumed(data) if data.input_id == first)));
    assert_eq!(
        history
            .iter()
            .filter(|event| matches!(event.kind, EventKind::InputDeleted(_)))
            .count(),
        1
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    server.shutdown();
    server.join().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn history_pages_the_branch_through_a_fixed_sequence_on_batch_boundaries() {
    let Served {
        server,
        agent,
        sessions,
        mut client,
        ..
    } = serve_sessions(None).await;
    client_session(&mut client, &agent, host_policy()).await;
    let session = launched(&sessions, &agent, json!({})).await;
    let at = |extra: Value| {
        let mut request = json!({"sessionId":session.id(),"branchId":"main"});
        request
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        request
    };
    let receipt = sessions
        .ok(
            "input",
            input(
                &session,
                "test:hello",
                json!([{"type":"text","text":"hello"}]),
            ),
        )
        .await;
    // Readable right after the reply (fixed-prefix read).
    let page = sessions
        .ok("history", at(json!({"atLeast":receipt["sequence"]})))
        .await;
    assert!(page["through"].as_u64().unwrap() >= receipt["sequence"].as_u64().unwrap());
    let first = sessions.ok("idle", at(json!({}))).await["sequence"]
        .as_u64()
        .unwrap();
    // A two-event batch: a withdrawal and its Steer replacement.
    let batch = client
        .rpc(
            "session.input.withdraw",
            json!({"sessionId":session.id(),"branchId":"main","inputId":"unknown",
                "steer":{"content":[{"type":"text","text":"steer"}]}}),
        )
        .await;
    assert_eq!(batch["result"]["status"], "notHeld", "{batch}");
    let inside = batch["result"]["steer"]["sequence"].as_u64().unwrap() - 1;
    let head = sessions.ok("idle", at(json!({}))).await["sequence"]
        .as_u64()
        .unwrap();

    // Pages of the whole branch through a fixed head, each on a batch
    // boundary; one may run past `limit` to finish its batch.
    let mut after = 0;
    let mut events = vec![];
    let mut through = None;
    while through != Some(after) {
        let mut request = at(json!({"after":after,"limit":1}));
        if let Some(through) = through {
            request["through"] = json!(through);
        }
        let page = sessions.ok("history", request).await;
        through = Some(page["through"].as_u64().unwrap());
        let page = page["events"].as_array().unwrap().clone();
        assert!(!page.is_empty());
        after = page.last().unwrap()["sequence"].as_u64().unwrap();
        events.extend(page);
    }
    assert_eq!(through, Some(head));
    assert_eq!(
        Value::Array(events),
        serde_json::to_value(session.local("main", ..=head).await.unwrap()).unwrap()
    );
    // An earlier fixed sequence bounds the page.
    let page = sessions.ok("history", at(json!({"through":first}))).await;
    assert_eq!(page["through"], first);
    assert_eq!(
        page["events"].as_array().unwrap().last().unwrap()["sequence"],
        first
    );

    for (request, code) in [
        (at(json!({"after":inside})), "rejected"),
        (at(json!({"after":first,"through":first - 1})), "rejected"),
        (at(json!({"limit":0})), "rejected"),
        (at(json!({"limit":257})), "rejected"),
        (at(json!({"atLeast":first,"through":first})), "rejected"),
        (at(json!({"atLeast":head + 1})), "future_sequence"),
        (at(json!({"through":head + 1})), "future_sequence"),
        (
            json!({"sessionId":session.id(),"branchId":"missing"}),
            "branch_missing",
        ),
    ] {
        assert_eq!(
            sessions.error("history", request.clone()).await,
            code,
            "{request}"
        );
    }
    server.shutdown();
    server.join().await.unwrap();
}

/// Sessions §6.8 and §8.7: a consumer deletes a Session it launched through
/// S5c's delete. A wait on the run ends `session_closed`, so do later calls,
/// and deleting an absent Session succeeds. An index reload does not cut a
/// started delete short. A delete that fails after its close replies
/// `failed` and leaves the Session closed with its rows; the consumer's retry
/// deletes it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_consumer_deletes_its_launched_session_and_retries_a_failed_delete() {
    use std::os::unix::fs::PermissionsExt;
    let store = Store::new();
    let Served {
        server,
        agent,
        sessions,
        mut client,
        ..
    } = serve_sessions(Some(store.database())).await;
    client_session(&mut client, &agent, host_policy()).await;
    let stored = |id: &str| {
        rusqlite::Connection::open(store.database())
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM agent_sessions WHERE session_id=?1",
                [id],
                |row| row.get::<_, i64>(0),
            )
            .unwrap()
            > 0
    };
    let delete = |id: &str| json!({"sessionId":id});

    // A loaded run whose turn waits on an approval.
    let session = awaiting_approval(&sessions, &agent).await;
    let id = session.id().to_owned();
    let at = json!({"sessionId":id,"branchId":"main"});
    let idle = tokio::spawn({
        let sessions = sessions.clone();
        let at = at.clone();
        async move { sessions.call("idle", at).await }
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(!idle.is_finished());
    assert_eq!(sessions.ok("delete", delete(&id)).await, json!({}));
    let ended = idle.await.unwrap();
    assert_eq!(ended["error"]["code"], "session_closed", "{ended}");
    assert!(agent.get_session(&id).is_none() && !stored(&id));
    for (method, request) in [
        ("idle", at.clone()),
        ("history", at.clone()),
        ("input", input(&session, "test:after", json!([]))),
        (
            "launch",
            json!({"cwd":std::env::temp_dir(),"starter":at.clone()}),
        ),
    ] {
        assert_eq!(
            sessions.error(method, request).await,
            "session_closed",
            "{method}"
        );
    }
    // Already deleted, or never created.
    for absent in [id.clone(), uuid::Uuid::new_v4().to_string()] {
        assert_eq!(sessions.ok("delete", delete(&absent)).await, json!({}));
    }
    for request in [
        delete("a/b"),
        delete(".."),
        json!({}),
        json!({"sessionId":id,"force":true}),
    ] {
        assert_eq!(
            sessions.error("delete", request.clone()).await,
            "rejected",
            "{request}"
        );
    }

    // The writer is held, so the delete waits in its close while Agents
    // reloads; it then replies its own outcome, not `reloading`.
    let id = launched(&sessions, &agent, json!({})).await.id().to_owned();
    let lock = rusqlite::Connection::open(store.database()).unwrap();
    lock.execute_batch("BEGIN IMMEDIATE").unwrap();
    let deleting = tokio::spawn({
        let sessions = sessions.clone();
        let request = delete(&id);
        async move { sessions.call("delete", request).await }
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!deleting.is_finished());
    let (restarted, ()) = tokio::join!(server.restart("Agents"), async {
        tokio::time::sleep(Duration::from_millis(100)).await;
        lock.execute_batch("COMMIT").unwrap();
    });
    restarted.unwrap();
    assert_eq!(deleting.await.unwrap(), json!({"ok": true, "value": {}}));
    assert!(!stored(&id));

    // An entry of the run's command output the daemon cannot remove.
    let id = launched(&sessions, &agent, json!({})).await.id().to_owned();
    let locked = store.0.join("command-output").join(&id).join("locked");
    std::fs::create_dir_all(&locked).unwrap();
    std::fs::write(locked.join("exec.log"), b"output").unwrap();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o500)).unwrap();
    assert_eq!(
        std::fs::write(locked.join("probe"), b"")
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::PermissionDenied,
        "file permissions must bind the test's user"
    );
    let failed = sessions.error("delete", delete(&id)).await;
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(failed, "failed");
    // Closed in this process with its rows kept (S5c D5).
    assert!(stored(&id));
    assert_eq!(
        sessions
            .error("idle", json!({"sessionId":id,"branchId":"main"}))
            .await,
        "session_closed"
    );
    assert_eq!(sessions.ok("delete", delete(&id)).await, json!({}));
    assert!(!stored(&id) && !store.0.join("command-output").join(&id).exists());
    server.shutdown();
    server.join().await.unwrap();
}

/// Sessions §6.1 and D5: a starter whose Permission is reloading fails the
/// launch with `reloading` instead of falling back to the host bound; once
/// Permission is back, the same launch succeeds.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_launch_fails_reloading_while_the_starters_permission_reloads() {
    let rules = HeldRules {
        hold: Arc::new(tokio::sync::watch::channel(false).0),
        held: Arc::default(),
    };
    let Served {
        server,
        agent,
        sessions,
        mut client,
        ..
    } = serve_with(Setup {
        rules: Some(rules.clone()),
        ..Default::default()
    })
    .await;
    let starter = client_session(&mut client, &agent, host_policy()).await;
    let request =
        json!({"cwd":std::env::temp_dir(),"starter":{"sessionId":starter.id(),"branchId":"main"}});

    rules.hold.send_replace(true);
    let (restarted, ()) = tokio::join!(server.restart("Agents"), async {
        rules.held.notified().await;
        assert!(starter.get::<PermissionState>("main").unwrap().is_none());
        let reply = sessions.call("launch", request.clone()).await;
        assert_eq!(reply["error"]["code"], "reloading", "{reply}");
        assert_eq!(reply["error"]["retryable"], true);
        assert!(
            reply["error"]["message"]
                .as_str()
                .unwrap()
                .contains("Permission"),
            "{reply}"
        );
        rules.hold.send_replace(false);
    });
    restarted.unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while starter.get::<PermissionState>("main").unwrap().is_none() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the starter's Permission is back");
    let reply = sessions.ok("launch", request).await;
    assert_launched(&mut client, &agent, &reply, host_policy()).await;
    server.shutdown();
    server.join().await.unwrap();
}

/// Sessions §6.1, the `model` row: with a model-selection host (configured
/// models, as in the product), a launch resolves its model through the host,
/// the request's over the Agent profile's; a model the host cannot resolve
/// fails the launch and creates nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_launch_resolves_its_model_through_the_model_selection_host() {
    use dim_agent_aimux::{AimuxModels, config::ModelConfigFile};
    let models = Arc::new(
        AimuxModels::from_config(
            serde_json::from_value::<ModelConfigFile>(json!({
                "defaultModel":{"providerId":"local","modelId":"alpha"},
                "providers":[{"id":"local","kind":"openai-compatible","baseUrl":"http://127.0.0.1:9/v1",
                    "models":[{"id":"alpha","config":{"maxTokens":64}},{"id":"beta","config":{"maxTokens":32}}]}]
            }))
            .unwrap(),
        )
        .unwrap(),
    );
    let store = Store::new();
    let Served {
        server,
        agent,
        sessions,
        mut client,
        ..
    } = serve_with(Setup {
        database: Some(store.database()),
        models: Some(models),
        ..Default::default()
    })
    .await;
    client_session(&mut client, &agent, host_policy()).await;
    let cwd = std::env::temp_dir();
    for (model, expected, max_tokens) in [
        (
            Some(json!({"providerId":"local","modelId":"beta"})),
            "beta",
            32.0,
        ),
        (None, "alpha", 64.0),
    ] {
        let mut request = json!({"cwd":cwd});
        if let Some(model) = model {
            request["model"] = model;
        }
        let reply = sessions.ok("launch", request).await;
        let session = agent
            .get_session(reply["sessionId"].as_str().unwrap())
            .unwrap();
        let selected = &session.config().model;
        assert_eq!(
            (selected.provider_id.as_str(), selected.model_id.as_str()),
            ("local", expected)
        );
        // Resolved by the host: it carries the catalog's model defaults.
        assert_eq!(
            selected
                .config
                .as_ref()
                .and_then(|config| config.max_tokens),
            Some(max_tokens)
        );
    }
    let before = agent.sessions().len();
    assert_eq!(
        sessions
            .error(
                "launch",
                json!({"cwd":cwd,"model":{"providerId":"local","modelId":"missing"}}),
            )
            .await,
        "failed"
    );
    assert_eq!(agent.sessions().len(), before, "nothing was created");
    server.shutdown();
    server.join().await.unwrap();
}

/// Sessions §5 and §17 #6: a launch whose caller drops the call after
/// creation started still creates and registers its Session, so Rpc serves
/// it. The native writer is held so the call is dropped while creation waits
/// on its rows.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_dropped_launch_still_registers_its_session() {
    let store = Store::new();
    let Served {
        server,
        agent,
        sessions,
        mut client,
        ..
    } = serve_sessions(Some(store.database())).await;
    client_session(&mut client, &agent, host_policy()).await;
    let known: Vec<String> = agent
        .sessions()
        .iter()
        .map(|session| session.id().to_owned())
        .collect();
    let lock = rusqlite::Connection::open(store.database()).unwrap();
    lock.execute_batch("BEGIN IMMEDIATE").unwrap();
    let launch = tokio::spawn({
        let sessions = sessions.clone();
        let cwd = std::env::temp_dir();
        async move { sessions.call("launch", json!({"cwd":cwd})).await }
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!launch.is_finished());
    launch.abort();
    assert!(launch.await.unwrap_err().is_cancelled());
    lock.execute_batch("COMMIT").unwrap();
    // Rpc refuses a loaded Session whose protocol version is not registered
    // (`session_protocol_version`), so an attach that succeeds shows both.
    let launched = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let created = agent
                .sessions()
                .into_iter()
                .find(|session| !known.iter().any(|id| id == session.id()));
            if let Some(session) = created
                && client
                    .rpc("session.attach", json!({"sessionId":session.id()}))
                    .await
                    .get("error")
                    .is_none()
            {
                return session;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the dropped launch registered its Session");
    assert!(!launched.config().interactive);
    server.shutdown();
    server.join().await.unwrap();
}

/// Sessions §6.2 and §6.6, §17 #1: `input` and a deleting `withdraw` reply
/// only once their event is published. When the store fails that write,
/// the event stays committed but unpublished, and the reply is the
/// retryable `publication_unknown`, not a receipt that a read right after
/// it could not find.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn input_and_withdraw_reply_publication_unknown_when_the_store_fails_the_write() {
    let store = Store::new();
    let Served {
        server,
        agent,
        sessions,
        mut client,
        ..
    } = serve_sessions(Some(store.database())).await;
    client_session(&mut client, &agent, host_policy()).await;
    // The native store fails the next write of `kind` for `session`.
    let fail = |session: &Session, kind: &str| {
        rusqlite::Connection::open(store.database())
            .unwrap()
            .execute_batch(&format!(
                "CREATE TRIGGER \"fail-{kind}\" BEFORE INSERT ON timeline_events \
                 WHEN NEW.session_id = '{}' AND NEW.kind = '{kind}' \
                 BEGIN SELECT RAISE(ABORT, 'injected write failure'); END",
                session.id()
            ))
            .unwrap();
    };

    let session = launched(&sessions, &agent, json!({})).await;
    fail(&session, "input/message");
    let lost = input(
        &session,
        "test:lost",
        json!([{"type":"text","text":"lost"}]),
    );
    assert_eq!(sessions.error("input", lost).await, "publication_unknown");

    // The first input's approval keeps its turn open, so the second is held.
    let session = awaiting_approval(&sessions, &agent).await;
    let held = sessions
        .ok(
            "input",
            input(
                &session,
                "test:held",
                json!([{"type":"text","text":"later"}]),
            ),
        )
        .await;
    fail(&session, "input/deleted");
    assert_eq!(
        sessions
            .error(
                "withdraw",
                json!({"sessionId":session.id(),"branchId":"main","inputId":held["inputId"]}),
            )
            .await,
        "publication_unknown"
    );
    server.shutdown();
    // The faulted Sessions report the injected failure as cleanup errors.
    let _ = server.join().await;
}

/// The generated process export reads live state and keeps its directory
/// binding across index reload, including a temporarily absent Permission.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires DIM_TEST_NODE and DIM_TEST_JS_RUNTIME"]
async fn node_session_permissions_reads_live_policy_and_survives_index_reload() {
    let store = Store::new();
    let mut parts = super::session_typescript::fixture();
    parts.rows.clear();
    parts.process_rows = vec![json!({
        "id":"permission-reader", "name":parts.anchor.parent().unwrap().join("fixtures/session-permissions.ts"),
        "provides":{"managementMethods:permissions":{"describe":"async","call":"async"}},
        "config":{},
    })];
    let rules = HeldRules {
        hold: Arc::new(tokio::sync::watch::channel(false).0),
        held: Arc::default(),
    };
    let Served {
        server,
        agent,
        sessions,
        mut client,
        ..
    } = serve_with(Setup {
        database: Some(store.database()),
        home: Some(store.0.clone()),
        typescript: Some(parts),
        rules: Some(rules.clone()),
        ..Default::default()
    })
    .await;
    super::process_typescript::select_model(&mut client).await;
    let defaults = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let reply = client.rpc("permissions.defaults", json!({})).await;
            if reply.get("result").is_some() {
                break reply["result"].clone();
            }
            assert_eq!(
                reply["error"]["data"]["appError"], "service_reloading",
                "{reply}"
            );
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(defaults["policy"].as_str().unwrap()).unwrap(),
        host_policy()
    );
    let starter = client_session(&mut client, &agent, json!({"preset":"full-access"})).await;
    let at = json!({"sessionId":starter.id(),"branchId":"main"});
    let before = starter.sequence("main").unwrap();
    let initial = client.rpc("permissions.branch", at.clone()).await;
    assert_ok(&initial);
    assert_eq!(initial["result"]["version"], "0");
    assert_eq!(
        serde_json::from_str::<Value>(initial["result"]["policy"].as_str().unwrap()).unwrap()["preset"],
        "full-access"
    );
    assert_eq!(
        starter.sequence("main").unwrap(),
        before,
        "reads append no events"
    );
    let narrowed =
        json!({"preset":"read-only","allowedTools":["confirm"],"disallowedTools":["blocked"]});
    let changed = starter
        .service::<Permission>("main")
        .unwrap()
        .update(0, policy(narrowed.clone()))
        .await
        .unwrap();
    let live = client.rpc("permissions.branch", at.clone()).await;
    assert_ok(&live);
    assert_eq!(live["result"]["version"], changed.to_string());
    assert_eq!(
        serde_json::from_str::<Value>(live["result"]["policy"].as_str().unwrap()).unwrap(),
        narrowed
    );
    assert_eq!(
        starter.config().permission_policy.preset,
        dim_agent_types::PermissionPreset::FullAccess
    );
    for (request, code) in [
        (
            json!({"sessionId":"missing","branchId":"main"}),
            "session_closed",
        ),
        (
            json!({"sessionId":starter.id(),"branchId":"missing"}),
            "branch_missing",
        ),
    ] {
        let read = client.rpc("permissions.branch", request.clone()).await;
        assert_ok(&read);
        let error = &read["result"]["error"];
        assert_eq!(error["name"], "SessionsError", "{read}");
        assert_eq!(error["code"], code, "{read}");
        assert_eq!(error["retryable"], false, "no envelope retryable field");
        let data: Value = serde_json::from_str(error["data"].as_str().unwrap()).unwrap();
        assert_eq!(data, json!({"code":code,"message":error["message"]}));
        let legacy = client.rpc("permissions.legacy", request).await;
        assert_ok(&legacy);
        let legacy: Value = serde_json::from_str(legacy["result"].as_str().unwrap()).unwrap();
        assert_eq!(legacy["ok"], false);
        assert_eq!(legacy["error"]["code"], code);
        assert_eq!(legacy["error"]["retryable"], false);
    }
    rules.hold.send_replace(true);
    let (restarted, ()) = tokio::join!(server.restart("Agents"), async {
        rules.held.notified().await;
        assert!(starter.get::<PermissionState>("main").unwrap().is_none());
        // Rpc also reloads with the index; call the unchanged process owner directly.
        let owner = sessions
            .context
            .get()
            .unwrap()
            .get_as::<ManagementMethods>(rutis::TypeKey::keyed_dynamic::<ManagementMethods>(
                "permissions",
            ))
            .unwrap();
        let call = owner
            .accept(
                "permissions.branch".into(),
                at.clone(),
                CancellationToken::new(),
                |_| {},
            )
            .unwrap();
        let read = call.reply.await.unwrap().unwrap();
        assert_eq!(read["error"]["name"], "SessionsError");
        assert_eq!(read["error"]["code"], "reloading", "{read}");
        assert!(
            read["error"]["message"]
                .as_str()
                .unwrap()
                .contains("Permission")
        );
        rules.hold.send_replace(false);
    });
    restarted.unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while starter.get::<PermissionState>("main").unwrap().is_none() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let after = client.rpc("permissions.branch", at).await;
    assert_ok(&after);
    assert_eq!(
        after["result"], live["result"],
        "same consumer generation and live state"
    );
    let after = client.rpc("permissions.defaults", json!({})).await;
    assert_eq!(after["result"], defaults);
    server.shutdown();
    server.join().await.unwrap();
}

#[path = "../../src/agents/sessions/tests/deferred_inputs.rs"]
mod deferred_inputs;
use deferred_inputs::DeferredInputs;

fn settlement_parts() -> dim_app_server::session_typescript::SessionTypeScriptConfig {
    let mut parts = super::session_typescript::fixture();
    parts.rows.clear();
    parts.process_rows = vec![json!({
        "id":"settlement-owner", "name":parts.anchor.parent().unwrap().join("fixtures/input-settlement.ts"),
        "provides":{"managementMethods:settlement":{"describe":"async","call":"async"}},"config":{},
    })];
    parts
}
async fn settlement_status(client: &mut Client) -> Value {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let reply = client.rpc("settlement.status", json!({})).await;
            if let Some(value) = reply.get("result") {
                break serde_json::from_str(value.as_str().unwrap()).unwrap();
            }
            assert_eq!(
                reply["error"]["data"]["appError"], "service_reloading",
                "{reply}"
            );
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires DIM_TEST_NODE and DIM_TEST_JS_RUNTIME"]
async fn node_input_settlement_outlives_index_reload_and_drains_the_original_row() {
    let (store, mut held) = DeferredInputs::new();
    let (send, mut controls) = tokio::sync::mpsc::unbounded_channel();
    let Served {
        server,
        agent,
        sessions,
        mut client,
        ..
    } = serve_with(Setup {
        store: Some(store),
        typescript: Some(settlement_parts()),
        controls: Some(send),
        ..Default::default()
    })
    .await;
    let target = client_session(&mut client, &agent, host_policy()).await;
    let (_, _, loader, _) = controls.recv().await.unwrap();
    let owner = loader.get("settlement-owner").unwrap().view.unwrap();
    let before = settlement_status(&mut client).await;
    assert!(
        owner
            .effects()
            .iter()
            .all(|effect| effect.label != "sessions.input")
    );
    let write = |id: &str| {
        let mut write = input(&target, id, json!([]));
        write["mode"] = json!("append");
        write
    };
    let request = client
        .send("settlement.input", write("settlement:index:publication"))
        .await;
    let waiting = tokio::time::timeout(Duration::from_secs(5), held.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(waiting.submission_id, "settlement:index:publication");
    assert!(target.publication("main").unwrap().sequence() < waiting.sequence);
    assert_eq!(
        owner
            .effects()
            .iter()
            .filter(|effect| effect.label == "sessions.input")
            .count(),
        1
    );
    let (restarted, ()) = tokio::join!(server.restart("Agents"), async {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let probe = sessions
                    .call("idle", json!({"sessionId":"missing","branchId":"main"}))
                    .await;
                if probe["error"]["code"] == "reloading" {
                    break;
                }
                assert_eq!(probe["error"]["code"], "session_closed");
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        waiting.release.send(()).unwrap();
    });
    restarted.unwrap();
    let reply = client.response(request).await;
    assert_ok(&reply);
    let receipt: Value = serde_json::from_str(reply["result"].as_str().unwrap()).unwrap();
    assert_eq!(receipt["ok"], true, "{receipt}");
    assert!(
        target.publication("main").unwrap().sequence()
            >= receipt["value"]["sequence"].as_u64().unwrap()
    );
    assert_eq!(
        settlement_status(&mut client).await["generation"],
        before["generation"]
    );
    assert!(
        owner
            .effects()
            .iter()
            .all(|effect| effect.label != "sessions.input")
    );
    assert_ok(
        &client
            .rpc("settlement.arm", write("test:late-row-call"))
            .await,
    );
    let request = client
        .send("settlement.input", write("settlement:row:append"))
        .await;
    let waiting = tokio::time::timeout(Duration::from_secs(5), held.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(waiting.submission_id, "settlement:row:append");
    let (restarted, ()) = tokio::join!(loader.restart("settlement-owner"), async {
        let mut changed = owner.watch();
        tokio::time::timeout(Duration::from_secs(5), async {
            while changed.borrow_and_update().state != rutis::FiberState::Unloading {
                changed.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
        assert!(
            owner
                .effects()
                .iter()
                .any(|effect| effect.label == "sessions.input")
        );
        waiting.release.send(()).unwrap();
    });
    restarted.unwrap();
    let reply = client.response(request).await;
    assert_ok(&reply);
    let receipt: Value = serde_json::from_str(reply["result"].as_str().unwrap()).unwrap();
    assert_eq!(receipt["ok"], true, "{receipt}");
    let after = settlement_status(&mut client).await;
    assert_ne!(after["generation"], before["generation"]);
    assert_eq!(
        after["cleanup"]["error"]["code"], "reloading",
        "late cleanup cannot register native input"
    );
    assert_eq!(sessions.ok("lookup", json!({"sessionId":target.id(),"branchId":"main","submissionId":"test:late-row-call"})).await, json!({"status":"unknown"}));
    let history = target.local("main", ..).await.unwrap();
    for id in ["settlement:index:publication", "settlement:row:append"] {
        assert_eq!(history.iter().filter(|event| matches!(&event.kind, EventKind::InputMessage(data) if data.submission_id.as_deref() == Some(id))).count(), 1);
    }
    assert!(
        owner
            .effects()
            .iter()
            .all(|effect| effect.label != "sessions.input")
    );
    server.shutdown();
    server.join().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rust_named_input_keeps_its_outcome_across_reload_and_a_dropped_result() {
    let (store, mut held) = DeferredInputs::new();
    let Served {
        server,
        agent,
        sessions,
        mut client,
        ..
    } = serve_with(Setup {
        store: Some(store),
        ..Default::default()
    })
    .await;
    let target = client_session(&mut client, &agent, host_policy()).await;
    let caller = sessions.clone();
    let write = |id| {
        let mut write = input(&target, id, json!([]));
        write["mode"] = json!("append");
        write
    };
    let request = write("settlement:rust:publication");
    let call = tokio::spawn(async move { caller.ok("input", request).await });
    let waiting = tokio::time::timeout(Duration::from_secs(5), held.recv())
        .await
        .unwrap()
        .unwrap();
    // No C1 call holds Rpc's drain here; wait for the complete index reload.
    tokio::time::timeout(Duration::from_secs(5), server.restart("Agents"))
        .await
        .expect("index reload must finish while the admitted writer awaits publication")
        .unwrap();
    assert!(target.publication("main").unwrap().sequence() < waiting.sequence);
    waiting.release.send(()).unwrap();
    let receipt = call.await.unwrap();
    assert!(
        target.publication("main").unwrap().sequence() >= receipt["sequence"].as_u64().unwrap()
    );

    let caller = sessions.clone();
    let request = write("settlement:rust:dropped");
    let call = tokio::spawn(async move { caller.ok("input", request).await });
    let waiting = tokio::time::timeout(Duration::from_secs(5), held.recv())
        .await
        .unwrap()
        .unwrap();
    call.abort();
    assert!(call.await.unwrap_err().is_cancelled());
    waiting.release.send(()).unwrap();
    let recovered = sessions.ok("lookup", json!({"sessionId":target.id(),"branchId":"main","submissionId":"settlement:rust:dropped"})).await;
    assert_eq!(
        recovered["status"], "accepted",
        "dropping a result does not end its admitted writer"
    );
    let history = target.local("main", ..).await.unwrap();
    for id in ["settlement:rust:publication", "settlement:rust:dropped"] {
        assert_eq!(history.iter().filter(|event| matches!(&event.kind, EventKind::InputMessage(data) if data.submission_id.as_deref() == Some(id))).count(), 1);
    }
    server.shutdown();
    server.join().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires DIM_TEST_NODE and DIM_TEST_JS_RUNTIME"]
async fn node_input_preparation_refuses_a_retired_row_before_handoff() {
    let store = Store::new();
    let rules = HeldRules {
        hold: Arc::new(tokio::sync::watch::channel(false).0),
        held: Arc::default(),
    };
    let (send, mut controls) = tokio::sync::mpsc::unbounded_channel();
    let Served {
        server,
        agent,
        sessions,
        mut client,
        ..
    } = serve_with(Setup {
        database: Some(store.database()),
        home: Some(store.0.clone()),
        typescript: Some(settlement_parts()),
        rules: Some(rules.clone()),
        controls: Some(send),
        ..Default::default()
    })
    .await;
    super::process_typescript::select_model(&mut client).await;
    let target = client_session(&mut client, &agent, host_policy()).await;
    let (_, _, loader, _) = controls.recv().await.unwrap();
    let owner = loader.get("settlement-owner").unwrap().view.unwrap();
    let before = settlement_status(&mut client).await;
    let request = input(&target, "test:retired-before-handoff", json!([]));
    assert!(target.unload_if_idle().await.unwrap());
    rules.hold.send_replace(true);
    let call = client.send("settlement.input", request.clone()).await;
    tokio::time::timeout(Duration::from_secs(5), rules.held.notified())
        .await
        .unwrap();
    assert!(
        owner
            .effects()
            .iter()
            .any(|effect| effect.label == "sessions.input")
    );
    let (restarted, ()) = tokio::join!(loader.restart("settlement-owner"), async {
        let mut changed = owner.watch();
        tokio::time::timeout(Duration::from_secs(5), async {
            while changed.borrow_and_update().state != rutis::FiberState::Unloading {
                changed.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
        rules.hold.send_replace(false);
    });
    restarted.unwrap();
    let reply = client.response(call).await;
    assert_ok(&reply);
    let reply: Value = serde_json::from_str(reply["result"].as_str().unwrap()).unwrap();
    assert_eq!(reply["error"]["code"], "reloading", "{reply}");
    assert_eq!(sessions.ok("lookup", json!({"sessionId":target.id(),"branchId":"main","submissionId":"test:retired-before-handoff"})).await, json!({"status":"unknown"}));
    assert_ne!(
        settlement_status(&mut client).await["generation"],
        before["generation"]
    );
    let reply = client.rpc("settlement.input", request).await;
    assert_ok(&reply);
    let reply: Value = serde_json::from_str(reply["result"].as_str().unwrap()).unwrap();
    assert_eq!(reply["ok"], true, "the replacement row can submit: {reply}");
    let current = agent.get_session(target.id()).unwrap();
    assert_eq!(current.local("main", ..).await.unwrap().iter().filter(|event| matches!(&event.kind, EventKind::InputMessage(data) if data.submission_id.as_deref() == Some("test:retired-before-handoff"))).count(), 1);
    server.shutdown();
    server.join().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires DIM_TEST_NODE and DIM_TEST_JS_RUNTIME"]
async fn node_crash_does_not_release_the_original_rows_native_input_drain() {
    let (store, mut held) = DeferredInputs::new();
    let (send, mut controls) = tokio::sync::mpsc::unbounded_channel();
    let Served {
        server,
        agent,
        sessions,
        mut client,
        ..
    } = serve_with(Setup {
        store: Some(store),
        typescript: Some(settlement_parts()),
        controls: Some(send),
        ..Default::default()
    })
    .await;
    let target = client_session(&mut client, &agent, host_policy()).await;
    let (_, _, loader, _) = controls.recv().await.unwrap();
    let owner = loader.get("settlement-owner").unwrap().view.unwrap();
    let before = settlement_status(&mut client).await;
    let mut write = input(&target, "settlement:crash:append", json!([]));
    write["mode"] = json!("append");
    let request = client.send("settlement.input", write).await;
    let waiting = tokio::time::timeout(Duration::from_secs(5), held.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(waiting.submission_id, "settlement:crash:append");
    let pid = before["pid"].as_u64().unwrap().to_string();
    assert!(
        std::process::Command::new("kill")
            .args(["-KILL", &pid])
            .status()
            .unwrap()
            .success()
    );
    let reply = client.response(request).await;
    assert!(
        reply.get("error").is_some(),
        "the dead Node cannot return its receipt: {reply}"
    );
    let (restarted, ()) = tokio::join!(loader.restart("SessionNodeRuntime"), async {
        let mut changed = owner.watch();
        tokio::time::timeout(Duration::from_secs(5), async {
            while changed.borrow_and_update().state != rutis::FiberState::Unloading {
                changed.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
        assert!(
            owner
                .effects()
                .iter()
                .any(|effect| effect.label == "sessions.input"),
            "bridge failure is not native writer settlement"
        );
        waiting.release.send(()).unwrap();
    });
    restarted.unwrap();
    let after = settlement_status(&mut client).await;
    assert_ne!(after["pid"], before["pid"]);
    let recovered = sessions.ok("lookup", json!({"sessionId":target.id(),"branchId":"main","submissionId":"settlement:crash:append"})).await;
    assert_eq!(recovered["status"], "accepted");
    assert_eq!(target.local("main", ..).await.unwrap().iter().filter(|event| matches!(&event.kind, EventKind::InputMessage(data) if data.submission_id.as_deref() == Some("settlement:crash:append"))).count(), 1);
    assert!(
        owner
            .effects()
            .iter()
            .all(|effect| effect.label != "sessions.input")
    );
    server.shutdown();
    server.join().await.unwrap();
}

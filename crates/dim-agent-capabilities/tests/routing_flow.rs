//! Flow: a Session's parts and their runtimes reach its own lines and faces by
//! id, and answer `SessionClosing`, `NoLine`, `NoService` or a reach refusal
//! otherwise
//! ([#2127](https://github.com/arcships/dim/issues/2127)).

use dim_agent_capabilities::defaults;
use dim_agent_runtime::testing::{MinimalHost, hold_close};
use dim_agent_runtime::*;
use dim_agent_types::BranchSource;
use rutis::Ctx;
use rutis::host::{CtxHost, DynPlugin, Effect, InstanceId, TypeKey};
use rutis_bridge::session::{
    Connection, Dispatch, Endpoint, Error as BridgeError, HostDispatch, Reply, Value, host_key,
    host_key_in,
};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// A Session part that hands its context to the test, where a language's
/// Session runtime would run.
struct HandsSession(Arc<Mutex<Option<Ctx>>>);

impl DynPlugin for HandsSession {
    fn name(&self) -> &str {
        "RoutingSessionProbe"
    }
    fn injects(&self) -> Vec<TypeKey> {
        Vec::new()
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> rutis::host::BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            *self.0.lock().unwrap() = Some(ctx.clone());
            Ok(Effect::Done)
        })
    }
}

struct Rig {
    host: MinimalHost,
    /// The context of the last Session's probe part, once that Session opened.
    session_ctx: Arc<Mutex<Option<Ctx>>>,
}

impl Rig {
    async fn start() -> Self {
        let session_ctx = Arc::new(Mutex::new(None));
        let probe = session_ctx.clone();
        let mut builder = AgentBuilder::new(AgentConfig::default());
        builder.timeline_store(TimelineStoreBinding::new(Arc::new(
            MemoryTimelineStore::default(),
        )));
        defaults::mock_hosts(&mut builder);
        defaults::install(&mut builder);
        // The product marks the same names (its `register_faces`); a route
        // resolves data faces only.
        builder.internal_name("loopState", "hot-path");
        // The product shares the faces a route resolves (`share_names`); the
        // rig shares the ones its cases use and leaves a native-only name
        // unshared.
        for (name, group) in [
            ("approval", "branch"),
            ("branchLine", "branch"),
            ("dispatcher", "branch"),
            ("inputQueue", "branch"),
            ("permission", "branch"),
            ("timeline", "branch"),
            ("sessionConfig", "session"),
            ("treeCommands", "session"),
            ("uiResources", "session"),
            ("sessionLoader", "session"),
            // The mechanism names the product also shares: a route refuses them
            // for being registration points or lifecycle services, not for
            // being unshared.
            ("guideEntries", "session"),
            ("sessionLifecycle", "session"),
            ("sessionMethods", "session"),
            ("sessionScope", "session"),
            ("toolRegistry", "session"),
            ("branchLifecycle", "branch"),
            // A face the catalog shares but this rig's profile does not
            // install.
            ("notInstalled", "branch"),
            ("notInstalledHere", "session"),
        ] {
            builder.catalog_mut().register_shared_instance(name, group);
        }
        builder.require_feature(
            Feature::new("RoutingSessionProbe").session("RoutingSessionProbe", move |_| {
                Ok(Box::new(HandsSession(probe.clone())))
            }),
        );
        let host = MinimalHost::start(&builder).await.unwrap();
        Self { host, session_ctx }
    }

    async fn open(&self) -> Session {
        self.host
            .create_session(SessionConfig::default(), Default::default())
            .await
            .expect("a Session opens")
    }

    fn session_ctx(&self) -> Ctx {
        self.session_ctx
            .lock()
            .unwrap()
            .take()
            .expect("the Session's part loaded")
    }

    fn scope(&self, session: &Session) -> Arc<SessionScope> {
        // The facade offers no read for the router; its owner hands out a
        // handle only the runtime can build.
        self.host
            .routed_session(session.id())
            .expect("the owning instance serves its scope")
            .scope()
            .clone()
    }
}

/// A language runtime's side of a rutis session: it names a service in the
/// instance the runtime lives in, then at the process.
struct Names {
    ctx: Ctx,
    instance: InstanceId,
}

impl Names {
    fn dispatch(&self, target: &str) -> Arc<dyn HostDispatch> {
        self.ctx
            .get_at::<dyn HostDispatch>(&host_key_in(target, self.instance))
            .or_else(|| self.ctx.get_at::<dyn HostDispatch>(&host_key(target)))
            .unwrap_or_else(|| panic!("{target} is not served"))
    }
}

impl Dispatch for Names {
    fn invoke(&self, _: &Connection, target: &str, method: &str, args: Value) -> Reply {
        self.dispatch(target).invoke(method, args)
    }
}

struct Language {
    _host: Connection,
    client: Connection,
}

impl Language {
    fn open(ctx: &Ctx, instance: InstanceId) -> Self {
        let (host, far) = rutis_bridge::memory::pair();
        let peer = |id: &str| Endpoint::rust(rutis_bridge::channel::PeerId::new(id).unwrap());
        let names = || Names {
            ctx: ctx.clone(),
            instance,
        };
        let host = Connection::open(host, Arc::new(names()), peer("scope-host")).unwrap();
        let client = Connection::open(far, Arc::new(names()), peer("scope-client")).unwrap();
        Self {
            _host: host,
            client,
        }
    }

    async fn call(
        &self,
        face: &str,
        method: &str,
        call: impl serde::Serialize,
    ) -> std::result::Result<serde_json::Value, BridgeError> {
        let call: Value = serde_json::to_value(call).unwrap().into();
        let reply = self
            .client
            .invoke_settled(face, method, Value::List(vec![call]))
            .unwrap();
        tokio::time::timeout(Duration::from_secs(30), reply)
            .await
            .expect("the far end answers")?
            .json()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_session_part_reaches_its_own_line_and_its_own_faces() {
    let rig = Rig::start().await;
    let session = rig.open().await;
    let scope = rig.scope(&session);

    let sequence = scope
        .line_call(LineCall::new("main", "timeline", "sequence", vec![]))
        .await
        .unwrap();
    assert_eq!(
        sequence,
        Some(serde_json::json!(session.sequence("main").unwrap()))
    );
    assert_eq!(
        scope
            .line_call(LineCall::new("absent", "timeline", "sequence", vec![]))
            .await
            .unwrap_err(),
        RouteError::NoLine {
            session: session.id().into(),
            line: "absent".into()
        }
    );

    let config = scope
        .call(SessionCall::new("sessionConfig", "get", vec![]))
        .await
        .unwrap();
    assert_eq!(
        config,
        Some(serde_json::to_value(session.config()).unwrap())
    );

    rig.host.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_session_language_call_reaches_its_own_line_and_names_no_session() {
    let rig = Rig::start().await;
    let session = rig.open().await;
    let ctx = rig.session_ctx();
    let language = Language::open(&ctx, session.instance().unwrap());
    language.client.ready().await.unwrap();

    let sequence = language
        .call(
            "sessionScope",
            "lineCall",
            LineCall::new("main", "timeline", "sequence", vec![]),
        )
        .await
        .unwrap();
    assert_eq!(
        sequence,
        serde_json::json!(session.sequence("main").unwrap())
    );
    let carried = serde_json::json!({
        "sessionId": session.id(),
        "lineId": "main",
        "service": "timeline",
        "method": "sequence",
        "args": [],
    });
    let failed = language
        .call("sessionScope", "lineCall", carried)
        .await
        .expect_err("a Session call names no Session");
    assert!(failed.to_string().contains("unknown field"), "{failed}");
    rig.host.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_closing_session_answers_session_closing_for_its_lines_too() {
    let rig = Rig::start().await;
    let session = rig.open().await;
    let id = session.id().to_owned();
    let scope = rig.scope(&session);
    let mut held = hold_close(&id, "main");
    let closing = {
        let session = session.clone();
        tokio::spawn(async move { session.close().await })
    };
    held.arrived().await;

    assert_eq!(
        scope
            .line_call(LineCall::new("main", "timeline", "sequence", vec![]))
            .await
            .unwrap_err(),
        RouteError::SessionClosing(id.clone())
    );
    assert_eq!(
        scope
            .call(SessionCall::new("sessionConfig", "get", vec![]))
            .await
            .unwrap_err(),
        RouteError::SessionClosing(id.clone())
    );
    held.release();
    closing.await.unwrap().unwrap();
    rig.host.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_closed_line_answers_no_line_and_the_sessions_other_calls_still_arrive() {
    let rig = Rig::start().await;
    let session = rig.open().await;
    let id = session.id().to_owned();
    let scope = rig.scope(&session);
    let source = || BranchSource::Host {
        operation_id: "routing".into(),
    };
    let line_id = session
        .branch(
            "main".into(),
            0,
            BranchOptions {
                prompt: "a line the route reaches".into(),
                source: source(),
            },
        )
        .await
        .unwrap();
    assert!(
        scope
            .line_call(LineCall::new(&line_id, "timeline", "sequence", vec![]))
            .await
            .is_ok()
    );

    session.end_branch(&line_id, source()).await.unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let answer = scope
            .line_call(LineCall::new(&line_id, "timeline", "sequence", vec![]))
            .await;
        if matches!(answer, Err(RouteError::NoLine { .. })) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the ended line closes: {answer:?}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        scope
            .call(SessionCall::new("sessionConfig", "get", vec![]))
            .await
            .is_ok(),
        "one line's end does not affect the Session's other calls"
    );
    assert!(
        scope
            .line_call(LineCall::new("main", "timeline", "sequence", vec![]))
            .await
            .is_ok(),
        "the Session's own main line still answers {id}"
    );
    rig.host.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_idle_unloaded_session_answers_session_closing_until_it_loads_again() {
    let rig = Rig::start().await;
    let session = rig.open().await;
    let id = session.id().to_owned();
    let scope = rig.scope(&session);

    assert!(
        session.unload_if_idle().await.unwrap(),
        "an idle Session unloads"
    );
    assert_eq!(
        scope
            .line_call(LineCall::new("main", "timeline", "sequence", vec![]))
            .await
            .unwrap_err(),
        RouteError::SessionClosing(id.clone()),
        "a scope of a Session that is gone reports its Session as closing"
    );

    let restored = rig
        .host
        .restore_session(id.clone(), Default::default())
        .await
        .unwrap();
    assert!(
        rig.scope(&restored)
            .line_call(LineCall::new("main", "timeline", "sequence", vec![]))
            .await
            .is_ok(),
        "the Session's new generation answers"
    );
    restored.close().await.unwrap();
    rig.host.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_owner_failure_keeps_its_code_through_the_route() {
    use dim_agent_capabilities::input_queue::InputQueue;
    use dim_agent_types::{InputMessageData, InputWithdrawData};

    let rig = Rig::start().await;
    let session = rig.open().await;
    let scope = rig.scope(&session);
    let command = InputWithdrawData {
        input_id: "x".into(),
        reason: None,
        steer: Some(InputMessageData::text("x", None)),
    };
    let direct = session
        .service::<InputQueue>("main")
        .expect("the line serves its input queue")
        .withdraw(command.clone())
        .await
        .unwrap_err();
    let routed = scope
        .line_call(LineCall::new(
            "main",
            "inputQueue",
            "withdraw",
            vec![serde_json::to_value(&command).unwrap()],
        ))
        .await
        .unwrap_err();
    let RouteError::Owner { code, .. } = &routed else {
        panic!("the owner's failure crosses as Owner: {routed:?}");
    };
    assert_eq!(
        code.as_deref(),
        Some("InvalidInput"),
        "direct {direct:?}, routed {routed:?}"
    );
    session.close().await.unwrap();
    rig.host.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_void_reply_stays_undefined_and_a_value_stays_a_value() {
    let rig = Rig::start().await;
    let session = rig.open().await;
    let scope = rig.scope(&session);

    let void = scope
        .call(SessionCall::new(
            "sessionLoader",
            "remove",
            vec![serde_json::json!({"instanceId": 0})],
        ))
        .await
        .unwrap();
    assert_eq!(
        void, None,
        "a method that answers nothing crosses as undefined"
    );

    // A value the owner replied stays a value.
    let value = scope
        .line_call(LineCall::new("main", "timeline", "sequence", vec![]))
        .await
        .unwrap();
    assert!(value.is_some(), "a reply stays a value: {value:?}");
    session.close().await.unwrap();
    rig.host.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn routing_resolves_only_data_faces_of_the_scope_it_reaches() {
    let rig = Rig::start().await;
    let session = rig.open().await;
    let scope = rig.scope(&session);

    for service in ["sessionMethods", "toolRegistry", "guideEntries"] {
        assert_eq!(
            scope
                .call(SessionCall::new(service, "register", vec![]))
                .await
                .unwrap_err(),
            RouteError::NotDataFace {
                service: service.into()
            },
            "{service} is a registration point"
        );
    }
    for service in ["sessionLifecycle", "branchLifecycle"] {
        assert_eq!(
            scope
                .call(SessionCall::new(service, "closing", vec![]))
                .await
                .unwrap_err(),
            RouteError::NotDataFace {
                service: service.into()
            },
            "{service} is a lifecycle service"
        );
    }
    assert_eq!(
        scope
            .line_call(LineCall::new("main", "wait", "wait", vec![]))
            .await
            .unwrap_err(),
        RouteError::MethodNotRoutable {
            service: "wait".into(),
            method: "wait".into()
        },
        "an operation that needs a caller-side cancellation is not routed"
    );
    assert_eq!(
        scope
            .call(SessionCall::new("sessionIdentity", "get", vec![]))
            .await
            .unwrap_err(),
        RouteError::NotDataFace {
            service: "sessionIdentity".into()
        },
        "a native-only registration is not a data face"
    );
    for (service, group) in [
        ("sessionMethods", "session"),
        ("toolRegistry", "session"),
        ("guideEntries", "session"),
    ] {
        assert_eq!(
            scope
                .call(SessionCall::new(service, "register", vec![]))
                .await
                .unwrap_err(),
            RouteError::NotDataFace {
                service: service.into()
            },
            "{service} is a registration point of the {group} group"
        );
    }
    for service in ["sessionLifecycle", "branchLifecycle"] {
        assert_eq!(
            scope
                .call(SessionCall::new(service, "activated", vec![]))
                .await
                .unwrap_err(),
            RouteError::NotDataFace {
                service: service.into()
            },
            "{service} is a scope lifecycle service"
        );
    }
    assert_eq!(
        scope
            .call(SessionCall::new("sessionScope", "lineCall", vec![]))
            .await
            .unwrap_err(),
        RouteError::NotDataFace {
            service: "sessionScope".into()
        },
        "the Session's own scope is not one of the faces it resolves"
    );
    assert_eq!(
        scope
            .call(SessionCall::new("uiResources", "publishFile", vec![]))
            .await
            .unwrap_err(),
        RouteError::MethodNotRoutable {
            service: "uiResources".into(),
            method: "publishFile".into()
        },
        "an operation bound to the calling row is direct-only"
    );
    assert_eq!(
        scope
            .line_call(LineCall::new("main", "dispatcher", "listen", vec![]))
            .await
            .unwrap_err(),
        RouteError::MethodNotRoutable {
            service: "dispatcher".into(),
            method: "listen".into()
        },
        "an operation that hands the caller a subscription is not routed"
    );
    assert_eq!(
        scope
            .call(SessionCall::new("treeCommands", "create", vec![]))
            .await
            .unwrap_err(),
        RouteError::MethodNotRoutable {
            service: "treeCommands".into(),
            method: "create".into()
        }
    );
    assert_eq!(
        scope
            .line_call(LineCall::new("main", "loopState", "state", vec![]))
            .await
            .unwrap_err(),
        RouteError::NotDataFace {
            service: "loopState".into()
        },
        "an internal face is not a data face"
    );
    assert_eq!(
        scope
            .call(SessionCall::new("timeline", "sequence", vec![]))
            .await
            .unwrap_err(),
        RouteError::WrongScope {
            service: "timeline".into(),
            group: "session".into()
        },
        "a line face is not a Session face"
    );
    assert_eq!(
        scope
            .line_call(LineCall::new("main", "sessionConfig", "get", vec![]))
            .await
            .unwrap_err(),
        RouteError::WrongScope {
            service: "sessionConfig".into(),
            group: "branch".into()
        },
        "a Session face is not a line face"
    );
    assert_eq!(
        scope
            .line_call(LineCall::new("main", "notInstalled", "read", vec![]))
            .await
            .unwrap_err(),
        RouteError::NoService {
            service: "notInstalled".into()
        },
        "the holder answers a face the line does not install"
    );
    assert_eq!(
        scope
            .call(SessionCall::new("notInstalledHere", "read", vec![]))
            .await
            .unwrap_err(),
        RouteError::NoService {
            service: "notInstalledHere".into()
        },
        "the holder answers a face the Session does not install"
    );
    let error = scope
        .line_call(LineCall::new("main", "timeline", "absent", vec![]))
        .await
        .unwrap_err();
    let RouteError::Owner { name, message, .. } = error else {
        panic!("the owner's failure: {error:?}");
    };
    assert_eq!(name, "no-method", "the owner's own failure keeps its name");
    assert!(message.contains("absent"), "{message}");
    rig.host.close().await.unwrap();
}

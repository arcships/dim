//! Flow: a Session's parts and their runtimes reach its own lines and faces by
//! id, and answer `SessionClosing`, `NoLine`, `NoService` or a reach refusal
//! otherwise
//! ([#2127](https://github.com/arcships/dim/issues/2127)).

use dim_agent_capabilities::defaults;
use dim_agent_runtime::testing::{MinimalHost, hold_close};
use dim_agent_runtime::*;
use dim_agent_types::{BranchSource, InputMessageData, InputMode};
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
            ("timelineContext", "branch"),
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

/// A wait admitted for a sequence the line never dispatches ends with the
/// line instead of holding its close open ([#2227](https://github.com/arcships/dim/issues/2227),
/// design rule 8).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_dispatched_wait_settles_when_the_line_ends() {
    let rig = Rig::start().await;
    let session = rig.open().await;
    let scope = rig.scope(&session);
    let child = session
        .branch(
            "main".into(),
            0,
            BranchOptions {
                prompt: String::new(),
                source: BranchSource::Tool {
                    turn_id: "wait-ends".into(),
                    request_id: "wait-ends".into(),
                    call_id: "wait-ends".into(),
                },
            },
        )
        .await
        .unwrap();
    let beyond = session.sequence(&child).unwrap() + 1000;
    let line = child.clone();
    let waiting = tokio::spawn(async move {
        scope
            .line_call(LineCall::new(
                line,
                "timelineContext",
                "waitDispatched",
                vec![serde_json::json!(beyond)],
            ))
            .await
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    session
        .end_branch(
            &child,
            BranchSource::Host {
                operation_id: "wait-ends".into(),
            },
        )
        .await
        .unwrap();
    let settled = tokio::time::timeout(Duration::from_secs(10), waiting)
        .await
        .expect("the wait settles with the line")
        .unwrap();
    assert!(
        settled.is_err(),
        "a line that ended before the sequence fails the wait: {settled:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_line_answers_its_own_operations_through_a_routed_call() {
    let rig = Rig::start().await;
    let session = rig.open().await;
    let scope = rig.scope(&session);
    let input = || {
        let mut data = InputMessageData::text("routed input", Some(InputMode::Queue));
        data.submission_id = Some("routing-input".into());
        data
    };
    let call = |method: &str, args: Vec<serde_json::Value>| {
        scope.line_call(LineCall::new("main", "timelineContext", method, args))
    };

    let first = call("input", vec![serde_json::to_value(input()).unwrap()])
        .await
        .unwrap()
        .expect("input answers its write");
    assert_eq!(first["written"], serde_json::json!(true), "{first}");
    let sequence = first["event"]["sequence"]
        .as_u64()
        .expect("a committed event carries its sequence");

    let again = call("input", vec![serde_json::to_value(input()).unwrap()])
        .await
        .unwrap()
        .expect("input answers its write");
    assert_eq!(
        again["written"],
        serde_json::json!(false),
        "the line holds a submission id once: {again}"
    );
    assert_eq!(again["event"], first["event"]);

    call("waitPublished", vec![serde_json::json!(sequence)])
        .await
        .unwrap();
    call("waitDispatched", vec![serde_json::json!(sequence)])
        .await
        .unwrap();
    let state = call("state", vec![])
        .await
        .unwrap()
        .expect("state answers the line's status");
    assert!(state.get("running").is_some(), "{state}");

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

/// A submission id is optional, but one that is present must not be empty:
/// the face refuses an empty id on every input operation and on lookup,
/// and still writes an input that carries none ([#2227](https://github.com/arcships/dim/pull/2227)).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_empty_submission_id_is_refused_and_an_absent_one_writes() {
    let rig = Rig::start().await;
    let session = rig.open().await;
    let scope = rig.scope(&session);
    let empty = serde_json::json!({
        "mode": "queue",
        "submission_id": "",
        "blocks": [{"type": "raw", "text": "<dim-test></dim-test>"}],
    });
    for (method, args) in [
        ("input", vec![empty.clone()]),
        ("inputIfHead", vec![empty.clone(), serde_json::json!(0)]),
        ("inputs", vec![serde_json::json!([empty.clone()])]),
        ("lookup", vec![serde_json::json!("")]),
    ] {
        let refused = scope
            .line_call(LineCall::new("main", "timelineContext", method, args))
            .await;
        let error = refused.expect_err(method);
        assert!(
            error.to_string().contains("submissionId must not be empty"),
            "{method} refuses the empty id: {error}"
        );
    }
    let absent = serde_json::json!({
        "mode": "queue",
        "blocks": [{"type": "raw", "text": "<dim-test></dim-test>"}],
    });
    for _ in 0..2 {
        let written = scope
            .line_call(LineCall::new(
                "main",
                "timelineContext",
                "input",
                vec![absent.clone()],
            ))
            .await
            .expect("an input without an id writes")
            .expect("input answers its write");
        assert_eq!(written["written"], serde_json::json!(true), "{written}");
    }
    rig.host.close().await.unwrap();
}

/// `inputIfHead` writes only while the line's last sequence is still the head
/// the caller read, and answers `null` without writing once a fact committed
/// behind it ([#2227](https://github.com/arcships/dim/pull/2227)).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn input_if_head_writes_only_at_the_head_it_read() {
    let rig = Rig::start().await;
    let session = rig.open().await;
    let scope = rig.scope(&session);
    let call = |method: &str, args: Vec<serde_json::Value>| {
        scope.line_call(LineCall::new("main", "timelineContext", method, args))
    };
    let message = |id: &str| {
        let mut data = InputMessageData::text("conditional input", Some(InputMode::Append));
        data.submission_id = Some(id.into());
        serde_json::to_value(data).unwrap()
    };
    let head = || async {
        scope
            .line_call(LineCall::new("main", "timeline", "sequence", vec![]))
            .await
            .unwrap()
            .and_then(|value| value.as_u64())
            .expect("the line answers its head")
    };

    let at = head().await;
    let written = call(
        "inputIfHead",
        vec![message("head:first"), serde_json::json!(at)],
    )
    .await
    .unwrap()
    .expect("inputIfHead answers");
    assert_eq!(written["data"]["submission_id"], "head:first", "{written}");
    let stale = call(
        "inputIfHead",
        vec![message("head:stale"), serde_json::json!(at)],
    )
    .await
    .unwrap();
    assert_eq!(
        stale,
        Some(serde_json::Value::Null),
        "a fact behind the head writes nothing"
    );
    let found = call("lookup", vec![serde_json::json!("head:stale")])
        .await
        .unwrap();
    assert_eq!(found, Some(serde_json::Value::Null), "nothing was written");
    rig.host.close().await.unwrap();
}

/// `inputs` commits a batch at once and answers a resubmission of the whole
/// batch with the first inputs; a batch that only partly repeats writes
/// nothing ([#2227](https://github.com/arcships/dim/pull/2227)).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn inputs_commit_a_batch_once_and_refuse_a_partial_repeat() {
    let rig = Rig::start().await;
    let session = rig.open().await;
    let scope = rig.scope(&session);
    let call = |args: Vec<serde_json::Value>| {
        scope.line_call(LineCall::new("main", "timelineContext", "inputs", args))
    };
    let message = |id: &str| {
        let mut data = InputMessageData::text("batch input", Some(InputMode::Append));
        data.submission_id = Some(id.into());
        serde_json::to_value(data).unwrap()
    };

    let batch = serde_json::json!([message("batch:a"), message("batch:b")]);
    let first = call(vec![batch.clone()])
        .await
        .unwrap()
        .expect("inputs answers its events");
    let events = first.as_array().expect("one event per input");
    assert_eq!(events.len(), 2, "{first}");
    assert_eq!(
        events[1]["sequence"].as_u64(),
        events[0]["sequence"].as_u64().map(|sequence| sequence + 1),
        "one commit, in order: {first}"
    );
    let again = call(vec![batch]).await.unwrap();
    assert_eq!(
        again,
        Some(first.clone()),
        "a repeated batch answers the first"
    );
    let partial = call(vec![serde_json::json!([
        message("batch:a"),
        message("batch:c")
    ])])
    .await;
    assert!(partial.is_err(), "a partial repeat is refused: {partial:?}");
    let found = scope
        .line_call(LineCall::new(
            "main",
            "timelineContext",
            "lookup",
            vec![serde_json::json!("batch:c")],
        ))
        .await
        .unwrap();
    assert_eq!(found, Some(serde_json::Value::Null), "nothing was written");
    rig.host.close().await.unwrap();
}

/// `interrupt` commits a turn interrupt and answers its sequence without
/// waiting for a turn to settle ([#2227](https://github.com/arcships/dim/pull/2227)).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interrupt_answers_the_sequence_it_committed() {
    let rig = Rig::start().await;
    let session = rig.open().await;
    let scope = rig.scope(&session);
    let before = scope
        .line_call(LineCall::new("main", "timeline", "sequence", vec![]))
        .await
        .unwrap()
        .and_then(|value| value.as_u64())
        .unwrap();
    let interrupt = scope
        .line_call(LineCall::new(
            "main",
            "timelineContext",
            "interrupt",
            vec![serde_json::json!("routing interrupt")],
        ))
        .await
        .unwrap()
        .and_then(|value| value.as_u64())
        .expect("interrupt answers a sequence");
    assert!(
        interrupt > before,
        "the interrupt is a new fact: {interrupt} > {before}"
    );
    rig.host.close().await.unwrap();
}

/// An input that reaches a line after its close started is refused `Closed`
/// by the line itself and writes nothing, while the holder can still select
/// the closing line; the route answers `no_line` for it
/// ([#2227](https://github.com/arcships/dim/pull/2227)).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_input_racing_a_close_is_refused_by_the_line() {
    let rig = Rig::start().await;
    let session = rig.open().await;
    let id = session.id().to_owned();
    let scope = rig.scope(&session);
    let source = || BranchSource::Host {
        operation_id: "close-race".into(),
    };
    let line_id = session
        .branch(
            "main".into(),
            0,
            BranchOptions {
                prompt: "a line that closes".into(),
                source: source(),
            },
        )
        .await
        .unwrap();
    let mut held = hold_close(&id, &line_id);
    let ending = {
        let session = session.clone();
        let line_id = line_id.clone();
        tokio::spawn(async move { session.end_branch(&line_id, source()).await })
    };
    held.arrived().await;

    let line = rig
        .host
        .routed_session(&id)
        .expect("the owning instance serves its scope")
        .line(&line_id)
        .expect("the holder still selects a closing line");
    let mut data = InputMessageData::text("too late", Some(InputMode::Append));
    data.submission_id = Some("close:late".into());
    let refused = line.context().input(data).await;
    assert!(
        matches!(refused, Err(AgentError::Closed(_))),
        "the closing line refuses the input: {refused:?}"
    );
    assert!(
        matches!(
            scope
                .line_call(LineCall::new(&line_id, "timeline", "sequence", vec![]))
                .await,
            Err(RouteError::NoLine { .. })
        ),
        "the route does not select a closing line"
    );
    held.release();
    ending.await.unwrap().unwrap();
    rig.host.close().await.unwrap();
}

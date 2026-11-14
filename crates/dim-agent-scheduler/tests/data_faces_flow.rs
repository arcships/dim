use dim_agent_capabilities::defaults;
use dim_agent_runtime::{AgentBuilder, AgentConfig};
use dim_agent_scheduler::{Loops, ScheduleJobs, Wakeups};
use dim_app_server::{RunningServer, ServerConfig, serve};
use rutis::Ctx;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, TypeKey};
use rutis_bridge::session::{Connection, Dispatch, Endpoint, HostDispatch, Reply, Value, host_key};
use serde_json::json;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

struct Probe(Arc<OnceLock<Ctx>>);

impl DynPlugin for Probe {
    fn name(&self) -> &str {
        "scheduler-face-probe"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![
            host_key("scheduleJobs").erase(),
            host_key("wakeups").erase(),
            host_key("loops").erase(),
            TypeKey::of::<ScheduleJobs>(),
            TypeKey::of::<Wakeups>(),
            TypeKey::of::<Loops>(),
            TypeKey::of::<dim_app_server::plugin_files::PluginFileTransactions>(),
        ]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            self.0.set(ctx.clone()).ok();
            Ok(Effect::Done)
        })
    }
}

struct Faces(Ctx);

impl Dispatch for Faces {
    fn invoke(&self, _: &Connection, target: &str, method: &str, args: Value) -> Reply {
        self.0
            .require_at::<dyn HostDispatch>(&host_key(target))
            .unwrap()
            .invoke(method, args)
    }
}

struct Rig {
    _home: tempfile::TempDir,
    server: RunningServer,
    ctx: Ctx,
    host: Connection,
    client: Connection,
    holds: Arc<dim_agent_scheduler::testing::Holds>,
}

impl Rig {
    async fn start() -> Self {
        let home = tempfile::tempdir().unwrap();
        std::fs::set_permissions(
            home.path(),
            std::os::unix::fs::PermissionsExt::from_mode(0o700),
        )
        .unwrap();
        std::fs::create_dir_all(home.path().join("scheduler")).unwrap();
        let mut builder = AgentBuilder::new(AgentConfig::default());
        defaults::install(&mut builder);
        defaults::mock_hosts(&mut builder);
        let holds = Arc::new(dim_agent_scheduler::testing::Holds::default());
        dim_agent_scheduler::install_with_testing(
            &mut builder,
            dim_agent_scheduler::SchedulerTexts::default(),
            dim_agent_scheduler::testing::TestingOptions {
                holds: holds.clone(),
                clock: dim_agent_scheduler::testing::clock(home.path()),
                signals: dim_agent_scheduler::testing::signals(home.path()),
            },
        )
        .unwrap();
        let observed = Arc::new(OnceLock::new());
        let probe = observed.clone();
        builder.plugin(move |_| Ok(Box::new(Probe(probe.clone())) as Box<dyn DynPlugin>));
        let server = serve(
            builder,
            ServerConfig {
                startup_timeout: Duration::from_secs(60),
                database_path: Some(home.path().join("dimcode.sqlite")),
                product_home: Some(home.path().to_owned()),
                unix_path: Some(home.path().join("native.sock")),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let ctx = observed
            .get()
            .expect("the probe depends on both faces")
            .clone();
        let (a, b) = rutis_bridge::memory::pair();
        let host = Connection::open(
            a,
            Arc::new(Faces(ctx.clone())),
            Endpoint::rust(rutis_bridge::channel::PeerId::new("scheduler-host").unwrap()),
        )
        .unwrap();
        let client = Connection::open(
            b,
            Arc::new(Faces(ctx.clone())),
            Endpoint::rust(rutis_bridge::channel::PeerId::new("scheduler-client").unwrap()),
        )
        .unwrap();
        client.ready().await.unwrap();
        Self {
            _home: home,
            server,
            ctx,
            host,
            client,
            holds,
        }
    }

    async fn call(
        &self,
        face: &str,
        method: &str,
        request: serde_json::Value,
    ) -> serde_json::Value {
        tokio::time::timeout(
            Duration::from_secs(30),
            self.client
                .invoke_settled(face, method, Value::List(vec![request.into()]))
                .unwrap(),
        )
        .await
        .unwrap()
        .unwrap()
        .json()
        .unwrap()
    }

    async fn close(self) {
        self.client.close(rutis_bridge::session::Error::Transport(
            "test complete".into(),
        ));
        self.host.closed().await;
        self.server.shutdown();
        self.server.join().await.unwrap();
    }

    async fn hold_loops_file(&self) -> (std::sync::mpsc::Sender<()>, tokio::task::JoinHandle<()>) {
        let files = self
            .ctx
            .require::<dim_app_server::plugin_files::PluginFileTransactions>()
            .unwrap();
        let (release, held) = std::sync::mpsc::channel();
        let (entered, entered_rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            files
                .transact("scheduler/loops.json", move |_| {
                    entered.send(()).unwrap();
                    held.recv_timeout(Duration::from_secs(30)).unwrap();
                    (None, ())
                })
                .await
                .unwrap();
        });
        entered_rx.await.unwrap();
        (release, task)
    }

    async fn hold_wakeup_file(&self) -> (std::sync::mpsc::Sender<()>, tokio::task::JoinHandle<()>) {
        let files = self
            .ctx
            .require::<dim_app_server::plugin_files::PluginFileTransactions>()
            .unwrap();
        let (release, held) = std::sync::mpsc::channel();
        let (entered, entered_rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            files
                .transact("scheduler/wakeups.json", move |_| {
                    entered.send(()).unwrap();
                    held.recv_timeout(Duration::from_secs(30)).unwrap();
                    (None, ())
                })
                .await
                .unwrap();
        });
        entered_rx.await.unwrap();
        (release, task)
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn scheduler_faces_match_the_native_operations_over_a_bridge() {
    let rig = Rig::start().await;
    let caller = json!({"sessionId":"saved-session", "branchId":"other", "cwd":rig._home.path()});
    let created = rig.call("scheduleJobs", "call", json!({"action":"create", "caller":caller,
        "job":{"name":"bridge job", "schedule":{"at":"2030-01-01T00:00:00Z"}, "prompt":"check it"}})).await;
    let id = created["records"]["job"]["id"].as_str().unwrap();
    let get = json!({"action":"get", "job_id":id});
    let wire = rig
        .call(
            "scheduleJobs",
            "call",
            json!({"action":"get", "job_id":id, "caller":caller}),
        )
        .await;
    let native = rig
        .ctx
        .require::<ScheduleJobs>()
        .unwrap()
        .call(
            serde_json::from_value(get).unwrap(),
            serde_json::from_value(caller.clone()).unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        wire,
        json!({"content":native.content,"records":native.records})
    );
    let missing = rig
        .call(
            "scheduleJobs",
            "call",
            json!({"action":"get","job_id":"missing","caller":caller}),
        )
        .await;
    let native = rig
        .ctx
        .require::<ScheduleJobs>()
        .unwrap()
        .call(
            serde_json::from_value(json!({"action":"get","job_id":"missing"})).unwrap(),
            serde_json::from_value(caller).unwrap(),
        )
        .await
        .err()
        .unwrap();
    assert_eq!(
        missing,
        json!({"code":native.code,"message":native.message})
    );

    let created = rig.call("wakeups", "create", json!({"sessionId":"saved-session", "branchId":"other", "at":"2020-01-01T00:00:00Z", "prompt":"already expired"})).await;
    let id = created["wakeupId"].as_str().unwrap();
    let listed = rig
        .call(
            "wakeups",
            "list",
            json!({"sessionId":"saved-session","branchId":"other"}),
        )
        .await;
    let native = rig
        .ctx
        .require::<Wakeups>()
        .unwrap()
        .list("saved-session", "other")
        .await
        .unwrap();
    let record = &native[0];
    assert_eq!(
        listed,
        json!({"wakeups":[{"wakeupId":record.wakeup_id, "at":record.at, "prompt":record.prompt,"state":record.state.name()}]})
    );
    assert_eq!(listed["wakeups"][0]["state"], "expired");
    assert_eq!(
        rig.call(
            "wakeups",
            "cancel",
            json!({"sessionId":"saved-session","wakeupId":id})
        )
        .await,
        json!({"state":"expired"})
    );
    let created = rig.call("wakeups", "create", json!({"sessionId":"saved-session", "branchId":"main", "at":"2030-01-01T00:00:00Z", "prompt":"later"})).await;
    assert_eq!(
        rig.call(
            "wakeups",
            "cancel",
            json!({"sessionId":"saved-session","wakeupId":created["wakeupId"]})
        )
        .await,
        json!({})
    );
    let listed = rig
        .call("wakeups", "list", json!({"sessionId":"saved-session"}))
        .await;
    assert_eq!(listed["wakeups"][0]["state"], "cancelled");
    rig.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn malformed_bridge_calls_return_the_documented_data_errors() {
    let rig = Rig::start().await;
    let valid =
        json!({"sessionId":"s", "branchId":"main", "at":"2030-01-01T00:00:00Z", "prompt":"later"});
    for (field, value) in [
        ("sessionId", json!("")),
        ("branchId", json!("")),
        ("prompt", json!("")),
        ("at", json!("bad")),
        ("prompt", json!("字".repeat(10923))),
        ("extra", json!(true)),
    ] {
        let mut request = valid.clone();
        request[field] = value;
        assert_eq!(
            rig.call("wakeups", "create", request).await["code"],
            "invalid_params",
            "{field}"
        );
    }
    for (method, request) in [
        ("create", json!({})),
        ("list", json!({"sessionId":""})),
        ("list", json!({"sessionId":"s","branchId":""})),
        ("cancel", json!({"sessionId":"s","wakeupId":""})),
    ] {
        assert_eq!(
            rig.call("wakeups", method, request).await["code"],
            "invalid_params",
            "{method}"
        );
    }
    assert_eq!(
        rig.call("scheduleJobs", "call", json!({"action":"unknown"}))
            .await["code"],
        "schedule_invalid"
    );
    for args in [
        Value::List(vec![]),
        Value::List(vec![valid.clone().into(), json!(1).into()]),
        Value::List(vec![valid.clone().into(), Value::Signal, json!(1).into()]),
    ] {
        let result = rig
            .client
            .invoke_settled("wakeups", "create", args)
            .unwrap()
            .await
            .unwrap()
            .json()
            .unwrap();
        assert_eq!(result["code"], "invalid_params");
    }
    rig.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bridge_cancellation_accepts_signals_and_promises_and_keeps_committed_writes() {
    let rig = Rig::start().await;
    let args = Value::List(vec![json!({"sessionId":"s"}).into(), Value::Signal]);
    let reply = rig
        .client
        .invoke_settled("wakeups", "list", args)
        .unwrap()
        .await
        .unwrap()
        .json()
        .unwrap();
    assert_eq!(reply, json!({"wakeups":[]}));
    let (release, held) = rig.hold_wakeup_file().await;
    let cancel = tokio_util::sync::CancellationToken::new();
    let signal = cancel.clone();
    let cancelled = Value::future(async move {
        signal.cancelled().await;
        Ok(json!(null).into())
    });
    let args = Value::List(vec![json!({"sessionId":"s"}).into(), cancelled]);
    let call = rig.client.invoke_settled("wakeups", "list", args).unwrap();
    let mut call = Box::pin(call);
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut call)
            .await
            .is_err()
    );
    cancel.cancel();
    let reply = tokio::time::timeout(Duration::from_secs(5), call)
        .await
        .unwrap()
        .unwrap()
        .json()
        .unwrap();
    assert_eq!(reply["code"], "cancelled");
    release.send(()).unwrap();
    held.await.unwrap();

    let (release, held) = rig.hold_wakeup_file().await;
    let request = json!({"sessionId":"s","branchId":"main","at":"2030-01-01T00:00:00Z","prompt":"keep committed"});
    let call = rig
        .client
        .invoke_settled(
            "wakeups",
            "create",
            Value::List(vec![request.into(), Value::Signal]),
        )
        .unwrap();
    let mut call = Box::pin(call);
    tokio::time::timeout(Duration::from_secs(5), async {
        tokio::select! {
            _ = rig.holds.wakeup_create.notified() => (),
            result = &mut call => panic!("the held write must stay pending: {result:?}"),
        }
    })
    .await
    .unwrap();
    drop(call);
    release.send(()).unwrap();
    held.await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let listed = rig.call("wakeups", "list", json!({"sessionId":"s"})).await;
            if !listed["wakeups"].as_array().unwrap().is_empty() {
                assert_eq!(listed["wakeups"][0]["prompt"], "keep committed");
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();

    let (release, held) = rig.hold_wakeup_file().await;
    let cancel = tokio_util::sync::CancellationToken::new();
    let signal = cancel.clone();
    let args = Value::List(vec![
        json!({"sessionId":"s","branchId":"main","at":"2030-01-01T00:00:00Z","prompt":"finish accepted"}).into(),
        Value::future(async move { signal.cancelled().await; Ok(json!(null).into()) }),
    ]);
    let mut call = Box::pin(
        rig.client
            .invoke_settled("wakeups", "create", args)
            .unwrap(),
    );
    tokio::time::timeout(Duration::from_secs(5), async {
        tokio::select! {
            _ = rig.holds.wakeup_create.notified() => (),
            result = &mut call => panic!("the held write must stay pending: {result:?}"),
        }
    })
    .await
    .unwrap();
    cancel.cancel();
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut call)
            .await
            .is_err(),
        "an accepted write settles before replying"
    );
    release.send(()).unwrap();
    held.await.unwrap();
    let reply = tokio::time::timeout(Duration::from_secs(5), call)
        .await
        .unwrap()
        .unwrap()
        .json()
        .unwrap();
    assert!(reply["wakeupId"].is_string(), "{reply}");
    rig.close().await;
}

/// The `loops` face answers the same records and errors as the native binding
/// across a bridge: `start`, `list`, `next` and `stop` over one loop, and a
/// missing target as the owner's code.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn loops_face_matches_the_native_operations_over_a_bridge() {
    let rig = Rig::start().await;
    let created = rig
        .call(
            "loops",
            "start",
            json!({"sessionId":"saved-session","branchId":"other","prompt":"check it"}),
        )
        .await;
    let loop_id = created["loopId"].as_str().unwrap().to_owned();
    assert_eq!(created["state"], "running");

    let listed = rig
        .call(
            "loops",
            "list",
            json!({"sessionId":"saved-session","branchId":"other"}),
        )
        .await;
    let records = listed["loops"].as_array().unwrap();
    assert_eq!(records.len(), 1, "{listed}");
    let record = &records[0];
    assert_eq!(record["loopId"], json!(loop_id));
    assert_eq!(record["branchId"], "other");
    assert_eq!(record["state"], "running");
    assert_eq!(record["round"], json!(1));
    assert!(record["expiresAt"].is_string(), "{record}");

    let armed = rig
        .call(
            "loops",
            "next",
            json!({"sessionId":"saved-session","loopId":loop_id,"delaySeconds":600}),
        )
        .await;
    assert_eq!(armed["state"], "running", "next: {armed}");
    assert!(armed["nextAt"].is_string(), "{armed}");

    let stopped = rig
        .call(
            "loops",
            "stop",
            json!({"sessionId":"saved-session","loopId":loop_id}),
        )
        .await;
    assert_eq!(stopped["state"], "stopped");
    assert!(stopped["settledAt"].is_string(), "{stopped}");
    let again = rig
        .call(
            "loops",
            "stop",
            json!({"sessionId":"saved-session","loopId":loop_id}),
        )
        .await;
    assert_eq!(again["settled"], json!(false), "{again}");

    let missing = rig
        .call(
            "loops",
            "stop",
            json!({"sessionId":"saved-session","loopId":"missing"}),
        )
        .await;
    let native = rig
        .ctx
        .require::<Loops>()
        .unwrap()
        .stop("saved-session", "missing")
        .await
        .err()
        .unwrap();
    assert_eq!(
        missing,
        json!({"code":native.code,"message":native.message})
    );
    rig.close().await;
}

/// The face's malformed requests answer `invalid_params`, the code the
/// `session.loop.*` handler gives the same requests, because one parse serves
/// both.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn malformed_loop_face_calls_return_the_documented_data_errors() {
    let rig = Rig::start().await;
    let valid = json!({"sessionId":"s","branchId":"main","prompt":"poll"});
    for (field, value) in [
        ("sessionId", json!("")),
        ("branchId", json!("")),
        ("prompt", json!("")),
        ("prompt", json!("字".repeat(10923))),
        ("extra", json!(true)),
    ] {
        let mut request = valid.clone();
        request[field] = value;
        assert_eq!(
            rig.call("loops", "start", request).await["code"],
            "invalid_params",
            "{field}"
        );
    }
    for (method, request) in [
        ("start", json!({})),
        ("list", json!({"sessionId":""})),
        ("list", json!({"sessionId":"s","branchId":""})),
        ("stop", json!({"sessionId":"s","loopId":""})),
        ("next", json!({"sessionId":"s","loopId":""})),
    ] {
        assert_eq!(
            rig.call("loops", method, request).await["code"],
            "invalid_params",
            "{method}"
        );
    }
    for args in [
        Value::List(vec![]),
        Value::List(vec![valid.clone().into(), json!(1).into()]),
        Value::List(vec![valid.clone().into(), Value::Signal, json!(1).into()]),
    ] {
        let result = rig
            .client
            .invoke_settled("loops", "start", args)
            .unwrap()
            .await
            .unwrap()
            .json()
            .unwrap();
        assert_eq!(result["code"], "invalid_params");
    }
    rig.close().await;
}

/// A cancelled or dropped `loops` read stops its wait, and an accepted `start`
/// settles in the owner's task tracker so a caller that leaves cannot abandon
/// it: cancellation before handover returns `cancelled` and writes nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn loop_face_cancellation_stops_reads_and_keeps_committed_starts() {
    let rig = Rig::start().await;
    let args = Value::List(vec![json!({"sessionId":"s"}).into(), Value::Signal]);
    let reply = rig
        .client
        .invoke_settled("loops", "list", args)
        .unwrap()
        .await
        .unwrap()
        .json()
        .unwrap();
    assert_eq!(reply, json!({"loops":[]}));

    let (release, held) = rig.hold_loops_file().await;
    let cancel = tokio_util::sync::CancellationToken::new();
    let signal = cancel.clone();
    let cancelled = Value::future(async move {
        signal.cancelled().await;
        Ok(json!(null).into())
    });
    let args = Value::List(vec![json!({"sessionId":"s"}).into(), cancelled]);
    let mut call = Box::pin(rig.client.invoke_settled("loops", "list", args).unwrap());
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut call)
            .await
            .is_err()
    );
    cancel.cancel();
    let reply = tokio::time::timeout(Duration::from_secs(5), call)
        .await
        .unwrap()
        .unwrap()
        .json()
        .unwrap();
    assert_eq!(reply["code"], "cancelled");
    release.send(()).unwrap();
    held.await.unwrap();

    let (release, held) = rig.hold_loops_file().await;
    let request = json!({"sessionId":"s","branchId":"main","prompt":"keep committed"});
    let mut call = Box::pin(
        rig.client
            .invoke_settled(
                "loops",
                "start",
                Value::List(vec![request.into(), Value::Signal]),
            )
            .unwrap(),
    );
    tokio::time::timeout(Duration::from_secs(5), async {
        tokio::select! {
            _ = rig.holds.loop_start.notified() => (),
            result = &mut call => panic!("the held write must stay pending: {result:?}"),
        }
    })
    .await
    .unwrap();
    drop(call);
    release.send(()).unwrap();
    held.await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let listed = rig.call("loops", "list", json!({"sessionId":"s"})).await;
            if !listed["loops"].as_array().unwrap().is_empty() {
                assert_eq!(listed["loops"][0]["round"], json!(1));
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();

    let (release, held) = rig.hold_loops_file().await;
    let cancel = tokio_util::sync::CancellationToken::new();
    let signal = cancel.clone();
    let args = Value::List(vec![
        json!({"sessionId":"s","branchId":"main","prompt":"finish accepted"}).into(),
        Value::future(async move {
            signal.cancelled().await;
            Ok(json!(null).into())
        }),
    ]);
    let mut call = Box::pin(rig.client.invoke_settled("loops", "start", args).unwrap());
    tokio::time::timeout(Duration::from_secs(5), async {
        tokio::select! {
            _ = rig.holds.loop_start.notified() => (),
            result = &mut call => panic!("the held write must stay pending: {result:?}"),
        }
    })
    .await
    .unwrap();
    cancel.cancel();
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut call)
            .await
            .is_err(),
        "an accepted write settles before replying"
    );
    release.send(()).unwrap();
    held.await.unwrap();
    let reply = tokio::time::timeout(Duration::from_secs(5), call)
        .await
        .unwrap()
        .unwrap()
        .json()
        .unwrap();
    assert!(reply["loopId"].is_string(), "{reply}");
    rig.close().await;
}

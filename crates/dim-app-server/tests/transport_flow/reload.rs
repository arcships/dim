use super::*;

pub(super) struct NamedExtension(pub String, pub Arc<AtomicUsize>);
impl TypedPlugin for NamedExtension {
    type Deps = ();
    fn name(&self) -> &str {
        &self.0
    }
    fn apply<'a>(
        &'a self,
        _: &'a rutis::Ctx,
        (): Self::Deps,
    ) -> rutis::BoxFuture<'a, Result<rutis::Effect, rutis::CordisError>> {
        Box::pin(async move {
            self.1.fetch_add(1, Ordering::SeqCst);
            Ok(rutis::Effect::Done)
        })
    }
}
impl Capability for NamedExtension {
    fn keys(&self) -> KeysOf<Self> {}
}

#[tokio::test]
async fn scoped_plugin_names_cannot_replace_process_modules() {
    let names = [
        "FileStorage",
        "Rpc",
        "FileStorage#2",
        "SessionScope",
        "AgentScope",
    ];
    let counts: Vec<_> = names
        .iter()
        .map(|_| Arc::new(AtomicUsize::new(0)))
        .collect();
    let (server, agent) = launch_with(Arc::new(MockTools), ServerConfig::default(), |builder| {
        for (name, count) in names.into_iter().zip(&counts) {
            let session = count.clone();
            builder.session_plugin(move |_| NamedExtension(name.into(), session.clone()));
            let branch = count.clone();
            builder.timeline_plugin(move |_| NamedExtension(name.into(), branch.clone()));
        }
    })
    .await;
    let mut client = Client::new(&server, true).await;
    for _ in 0..2 {
        let created = client.rpc("session.create", create(json!([]))).await;
        assert_ok(&created);
        assert!(
            agent
                .get_session(created["result"]["sessionId"].as_str().unwrap())
                .is_some()
        );
    }
    // Both scope levels must still load each extension, not just avoid a crash.
    for (name, count) in names.into_iter().zip(counts) {
        assert_eq!(count.load(Ordering::SeqCst), 4, "{name}");
    }
    server.restart("Rpc").await.unwrap();
    let listed = client.rpc("session.list", json!({})).await;
    assert_ok(&listed);
    assert_eq!(listed["result"].as_array().unwrap().len(), 2);
    server.shutdown();
    server.join().await.unwrap();

    // Group IDs are public instance selectors, unlike plugin display names.
    let mut builder = AgentBuilder::new(AgentConfig::default());
    builder.session_extension("Rpc", "extension", |_, _: &()| {
        NamedExtension("extension".into(), Arc::new(AtomicUsize::new(0)))
    });
    let error = serve(builder, ServerConfig::default())
        .await
        .err()
        .expect("a group must not shadow a process row");
    assert!(
        error.to_string().contains("reserved extension group: Rpc"),
        "{error}"
    );
}

async fn app_error(client: &mut Client, method: &str) -> Value {
    client.rpc(method, json!({})).await["error"]["data"]["appError"].clone()
}

/// A tool that finishes only when the test releases it, even when cancelled.
struct HoldTool {
    started: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
}
impl ToolDriver for HoldTool {
    fn definitions(&self) -> Vec<ToolDefinition> {
        vec![dim_agent_capabilities::tool_registry::definition(
            "hold",
            "Reload test",
            json!({"type":"object"}),
        )]
    }
    fn execute(
        &self,
        _call: ToolCall,
        _context: ToolExecutionContext,
        _cancel: CancellationToken,
    ) -> HostFuture<'_, ToolResult> {
        Box::pin(async move {
            self.started.notify_one();
            self.release.notified().await;
            Ok(ToolResult::text("released"))
        })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn process_plugin_reloads_keep_sessions() {
    let started = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let tool = HoldTool {
        started: started.clone(),
        release: release.clone(),
    };
    let server = launch(Arc::new(tool), ServerConfig::default()).await;
    let mut client = Client::new(&server, false).await;
    let created = client
        .rpc(
            "session.create",
            json!({"cwd":std::env::temp_dir(),"tools":["hold"],"permissionPolicy":{"allowedTools":["hold"]}}),
        )
        .await;
    assert_ok(&created);
    let sid = created["result"]["sessionId"].as_str().unwrap().to_owned();

    // Rpc: accepted commands drain before unload; new requests in the window
    // are rejected as retryable and succeed once the new generation registers.
    // Stop waits for the held tool without client input, holding the drain open.
    assert_ok(
        &client
            .rpc(
                "session.submit",
                json!({"sessionId":sid,"text":"tool:hold {}","mode":"queue"}),
            )
            .await,
    );
    tokio::time::timeout(Duration::from_secs(5), started.notified())
        .await
        .expect("tool started");
    let stop = client.send("session.stop", json!({"sessionId":sid})).await;
    assert_ok(&client.rpc("daemon.ping", json!({})).await);
    let mut observer = Client::new(&server, false).await;
    {
        let restart = server.restart("Rpc");
        tokio::pin!(restart);
        let rejected = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            tokio::select! {
                biased;
                result = &mut restart => panic!("reload finished before the window: {result:?}"),
                error = app_error(&mut observer, "daemon.ping") => {
                    if error == "service_reloading" {
                        return;
                    }
                }
            }
        }
    })
    .await;
        assert!(rejected.is_ok(), "no request observed the reload window");
        release.notify_one();
        restart.await.unwrap();
    }
    assert_ok(&client.response(stop).await);
    assert_ok(&observer.rpc("daemon.ping", json!({})).await);
    let listed = observer.rpc("session.list", json!({})).await;
    assert_eq!(listed["result"][0]["sessionId"], sid);

    // Connection: connections drop, listeners rebind, Sessions remain attachable.
    server.restart("Connection").await.unwrap();
    let mut reconnected = Client::new(&server, false).await;
    assert_eq!(reconnected.instance, client.instance);
    assert_ok(
        &reconnected
            .rpc("session.attach", json!({"sessionId":sid}))
            .await,
    );

    // Agents: only the directory reloads. Sessions, the protocol instance
    // and connections stay. Session capabilities that depend on the
    // directory reload with it; meanwhile requests are retryable.
    server.restart("Agents").await.unwrap();
    let listed = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let listed = reconnected.rpc("session.list", json!({})).await;
            if listed["error"]["data"]["appError"] != "service_reloading" {
                return listed;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("Session capabilities returned");
    assert_eq!(listed["result"][0]["sessionId"], sid);
    let mut fresh = Client::new(&server, false).await;
    assert_eq!(fresh.instance, client.instance);
    assert_ok(&fresh.rpc("session.create", create(json!([]))).await);

    // Plugins without an explicit restart are rejected.
    for plugin in ["ServerControl", "Missing"] {
        assert!(server.restart(plugin).await.is_err(), "{plugin}");
    }

    server.shutdown();
    assert_eq!(server.join().await.unwrap(), ShutdownReason::Graceful);
}

#[test]
fn process_initialization_is_covered_by_the_startup_deadline() {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()
        .unwrap()
        .block_on(async {
            let (release, held) = std::sync::mpsc::channel();
            let (entered, ready) = tokio::sync::oneshot::channel();
            let worker = tokio::task::spawn_blocking(move || {
                entered.send(()).unwrap();
                held.recv().unwrap();
            });
            ready.await.unwrap();
            let config = ServerConfig {
                session_execution: false,
                startup_timeout: Duration::from_millis(50),
                ..Default::default()
            };
            // FileStorage cannot open its roots until the only file worker is
            // released. Rollback may wait for it, but startup must not succeed.
            let (result, ()) = tokio::join!(
                serve(AgentBuilder::new(AgentConfig::default()), config),
                async {
                    tokio::time::sleep(Duration::from_millis(250)).await;
                    release.send(()).unwrap();
                    worker.await.unwrap();
                }
            );
            let error = match result {
                Err(error) => error.to_string(),
                Ok(server) => {
                    server.shutdown();
                    server.join().await.unwrap();
                    panic!("process initialization exceeded the deadline without failing");
                }
            };
            assert!(error.contains("timed out"), "{error}");
            assert!(error.contains("FileStorage"), "{error}");
            let server = serve(
                AgentBuilder::new(AgentConfig::default()),
                ServerConfig {
                    session_execution: false,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
            server.shutdown();
            server.join().await.unwrap();
        });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dropping_serve_during_startup_releases_the_listener() {
    let path = std::path::PathBuf::from("/tmp")
        .join(format!("dim-startup-{}/server.sock", uuid::Uuid::new_v4()));
    let config = ServerConfig {
        unix_path: Some(path.clone()),
        websocket_address: None,
        ..Default::default()
    };
    let mut starting = Box::pin(serve(
        AgentBuilder::new(AgentConfig::default()),
        config.clone(),
    ));
    // Startup advances only while `serve` is polled, while its plugins run
    // on their own; polling once per check drops it after the transport bound
    // the socket and before startup can finish.
    tokio::time::timeout(Duration::from_secs(5), async {
        while !path.exists() {
            assert!(
                futures_util::poll!(&mut starting).is_pending(),
                "startup finished before the transport bound"
            );
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("transport bound the socket");
    drop(starting);
    let server = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match serve(AgentBuilder::new(AgentConfig::default()), config.clone()).await {
                Ok(server) => return server,
                Err(_) => tokio::time::sleep(Duration::from_millis(20)).await,
            }
        }
    })
    .await
    .expect("listener released after cancelled startup");
    server.shutdown();
    assert_eq!(server.join().await.unwrap(), ShutdownReason::Graceful);
}

/// Fails Session cleanup while the flag is set.
pub(super) struct FailingCleanup(pub(super) Arc<std::sync::atomic::AtomicBool>);
impl TypedPlugin for FailingCleanup {
    type Deps = ();
    fn name(&self) -> &'static str {
        "test.failing-cleanup"
    }
    fn apply<'a>(
        &'a self,
        ctx: &'a rutis::Ctx,
        (): Self::Deps,
    ) -> rutis::BoxFuture<'a, Result<rutis::Effect, rutis::CordisError>> {
        let fail = self.0.clone();
        let result = ctx.effect(move || {
            rutis::Effect::AsyncDisposer(Box::new(move || {
                Box::pin(async move {
                    if fail.load(Ordering::SeqCst) {
                        Err(rutis::CordisError::PluginFailed("cleanup".into()))
                    } else {
                        Ok(())
                    }
                })
            }))
        });
        Box::pin(async move { result.map(|_| rutis::Effect::Done) })
    }
}
impl Capability for FailingCleanup {
    fn keys(&self) -> KeysOf<Self> {}
}

/// Echo model whose first invocation waits for the test to release it.
struct GatedModel {
    entered: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
    gated: std::sync::atomic::AtomicBool,
}
impl dim_agent_capabilities::host::ModelDriver for GatedModel {
    fn invoke(
        &self,
        request: Arc<dim_agent_types::ModelRequest>,
        _: dim_agent_capabilities::host::ModelConnection,
        cancel: CancellationToken,
    ) -> HostFuture<'_, dim_agent_capabilities::host::ModelStream> {
        Box::pin(async move {
            if self.gated.swap(false, Ordering::SeqCst) {
                self.entered.notify_one();
                tokio::select! {
                    _ = cancel.cancelled() => {
                        return Err(dim_agent_capabilities::host::HostError::Cancelled("test".into()));
                    }
                    _ = self.release.notified() => {}
                }
            }
            dim_agent_capabilities::host::ModelDriver::invoke(
                &dim_agent_capabilities::adapters::EchoModel,
                request,
                dim_agent_capabilities::host::ModelConnection::Local,
                cancel,
            )
            .await
        })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reloaded_loop_fails_the_unfinished_turn_and_later_turns_run() {
    use dim_agent_capabilities::{
        adapters::DefaultPrepareModelPlugin,
        history::HistoryRenderer,
        host::{
            CompactionStrategyHost, ContextModelHost, IdentityContext, ModelHost,
            PrepareContextHost,
        },
    };
    use dim_agent_types::{EventKind, TurnOutcome};
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let mut builder = AgentBuilder::new(AgentConfig::default());
    defaults::install(&mut builder);
    builder
        .session_value(Arc::new(HistoryRenderer::default()))
        .session_value(Arc::new(ModelHost(Arc::new(GatedModel {
            entered: entered.clone(),
            release: release.clone(),
            gated: std::sync::atomic::AtomicBool::new(true),
        }))))
        .session_value(Arc::new(PrepareContextHost(Arc::new(IdentityContext))))
        .session_value(Arc::new(ContextModelHost(None)))
        .session_value(Arc::new(CompactionStrategyHost(None)))
        .session_plugin(DefaultPrepareModelPlugin::new);
    let tools = ToolReload::default();
    tools.install(&mut builder, Arc::new(MockTools));
    let server = serve(
        builder,
        ServerConfig {
            unix_path: Some(
                std::path::PathBuf::from("/tmp")
                    .join(format!("dim-loop-reload-{}/s", uuid::Uuid::new_v4())),
            ),
            websocket_address: None,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let mut client = Client::new(&server, false).await;
    let created = client.rpc("session.create", create(json!([]))).await;
    assert_ok(&created);
    let sid = created["result"]["sessionId"].as_str().unwrap().to_owned();
    let session = server.agent().unwrap().get_session(&sid).unwrap();
    let loop_generation = || {
        session
            .branch_diagnostics("main")
            .unwrap()
            .plugins
            .into_iter()
            .find(|plugin| plugin.name == "Loop")
            .map(|plugin| (plugin.generation, plugin.state))
    };
    let (before, _) = loop_generation().unwrap();
    assert_ok(
        &client
            .rpc("session.submit", json!({"sessionId":sid,"text":"held"}))
            .await,
    );
    tokio::time::timeout(Duration::from_secs(5), entered.notified())
        .await
        .expect("model request in flight");

    // Loop reloads through ToolHost → ToolRegistry → PrepareModelHost; the
    // model request is not part of that chain.
    tools.restart().await;
    tokio::time::timeout(Duration::from_secs(5), async {
        while !loop_generation().is_some_and(|(generation, state)| {
            generation > before && state == rutis::FiberState::Active
        }) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("Loop reloaded");
    release.notify_one();
    let ended = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let end = session
                .history("main", None)
                .await
                .unwrap()
                .into_iter()
                .find_map(|event| match &event.kind {
                    EventKind::TurnEnd(data) => Some(data.outcome.clone()),
                    _ => None,
                });
            if let Some(outcome) = end {
                return outcome;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("turn ended");
    assert_eq!(
        ended,
        TurnOutcome::Failed {
            error: "capability reloading".into()
        }
    );

    // The rebuilt Loop runs later turns normally.
    assert_ok(
        &client
            .rpc("session.submit", json!({"sessionId":sid,"text":"again"}))
            .await,
    );
    assert_ok(&client.rpc("session.idle", json!({"sessionId":sid})).await);
    assert!(
        session
            .history("main", None)
            .await
            .unwrap()
            .iter()
            .any(|event| matches!(
                &event.kind,
                EventKind::TurnEnd(data) if matches!(data.outcome, TurnOutcome::Completed { .. })
            ))
    );
    server.shutdown();
    server.join().await.unwrap();
}

/// Echo model that records the tool names declared in each request.
struct DeclaredTools(std::sync::Mutex<Vec<Vec<String>>>);
impl dim_agent_capabilities::host::ModelDriver for DeclaredTools {
    fn invoke(
        &self,
        request: Arc<dim_agent_types::ModelRequest>,
        _: dim_agent_capabilities::host::ModelConnection,
        cancel: CancellationToken,
    ) -> HostFuture<'_, dim_agent_capabilities::host::ModelStream> {
        let names = request
            .tools
            .as_deref()
            .into_iter()
            .flatten()
            .map(|tool| tool.name.clone())
            .collect();
        self.0.lock().unwrap().push(names);
        dim_agent_capabilities::host::ModelDriver::invoke(
            &dim_agent_capabilities::adapters::EchoModel,
            request,
            dim_agent_capabilities::host::ModelConnection::Local,
            cancel,
        )
    }
}

/// A Session capability outside the defaults contributing a stateful tool.
struct ProbeContributor(dim_agent_runtime::SessionScope);
struct ProbeTool(AtomicUsize);
impl dim_agent_capabilities::tool_registry::ToolHandler for ProbeTool {
    fn execute(
        &self,
        _call: ToolCall,
        _context: ToolExecutionContext,
        _cancel: CancellationToken,
    ) -> HostFuture<'_, ToolResult> {
        let calls = self.0.fetch_add(1, Ordering::SeqCst) + 1;
        Box::pin(async move { Ok(ToolResult::text(format!("probe call {calls}"))) })
    }
}
impl TypedPlugin for ProbeContributor {
    type Deps = ();
    fn name(&self) -> &str {
        "Probe"
    }
    fn apply<'a>(
        &'a self,
        ctx: &'a rutis::Ctx,
        (): Self::Deps,
    ) -> rutis::BoxFuture<'a, Result<rutis::Effect, rutis::CordisError>> {
        use dim_agent_capabilities::tool_registry::{ToolContribution, definition};
        Box::pin(async move {
            ctx.provide_as(
                self.0.named_key::<ToolContribution>("probe"),
                Arc::new(ToolContribution {
                    definition: definition("probe", "Count calls", json!({"type":"object"})),
                    handler: Arc::new(ProbeTool(AtomicUsize::new(0))),
                }),
            )?;
            Ok(rutis::Effect::Done)
        })
    }
}
impl Capability for ProbeContributor {
    fn keys(&self) -> KeysOf<Self> {}
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn contributed_tools_follow_session_config_and_survive_tool_host_reload() {
    use dim_agent_capabilities::{
        adapters::DefaultPrepareModelPlugin,
        history::HistoryRenderer,
        host::{
            CompactionStrategyHost, ContextModelHost, IdentityContext, ModelHost,
            PrepareContextHost,
        },
    };
    use dim_agent_types::EventKind;
    let declared = Arc::new(DeclaredTools(Default::default()));
    let mut builder = AgentBuilder::new(AgentConfig::default());
    defaults::install(&mut builder);
    builder
        .session_value(Arc::new(HistoryRenderer::default()))
        .session_value(Arc::new(ModelHost(declared.clone())))
        .session_value(Arc::new(PrepareContextHost(Arc::new(IdentityContext))))
        .session_value(Arc::new(ContextModelHost(None)))
        .session_value(Arc::new(CompactionStrategyHost(None)))
        .session_plugin(DefaultPrepareModelPlugin::new)
        .session_tool("probe")
        .session_plugin(|scope| ProbeContributor(*scope));
    let tools = ToolReload::default();
    tools.install(&mut builder, Arc::new(MockTools));
    let server = serve(
        builder,
        ServerConfig {
            unix_path: Some(
                std::path::PathBuf::from("/tmp")
                    .join(format!("dim-contrib-reload-{}/s", uuid::Uuid::new_v4())),
            ),
            websocket_address: None,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let mut client = Client::new(&server, false).await;
    // `branch` and `task` remain registered contributions but are not enabled.
    let created = client
        .rpc(
            "session.create",
            json!({"cwd":std::env::temp_dir(),"tools":["probe","echo"],"permissionPolicy":{"allowedTools":["probe","echo"]}}),
        )
        .await;
    assert_ok(&created);
    let sid = created["result"]["sessionId"].as_str().unwrap().to_owned();
    let session = server.agent().unwrap().get_session(&sid).unwrap();
    let generation = |name: &str| {
        session
            .diagnostics()
            .unwrap()
            .plugins
            .into_iter()
            .find(|plugin| plugin.name == name && plugin.state == rutis::FiberState::Active)
            .map(|plugin| plugin.generation)
    };
    let loop_generation = || {
        session
            .branch_diagnostics("main")
            .unwrap()
            .plugins
            .into_iter()
            .find(|plugin| plugin.name == "Loop" && plugin.state == rutis::FiberState::Active)
            .map(|plugin| plugin.generation)
    };
    let last_result = async || {
        session
            .history("main", None)
            .await
            .unwrap()
            .iter()
            .rev()
            .find_map(|event| match &event.kind {
                EventKind::ToolResult(data) => Some(serde_json::to_string(&data.results).unwrap()),
                _ => None,
            })
            .unwrap()
    };
    for expected in ["probe call 1", "probe call 2"] {
        if expected == "probe call 2" {
            // ToolHost → ToolRegistry → ToolBinding → Loop reloads. Registry
            // readiness does not imply the branch accepts input; wait for its
            // new Loop generation. The contributor is not downstream and
            // keeps its state.
            let (probe, before) = (generation("Probe"), loop_generation().unwrap());
            tools.restart().await;
            tokio::time::timeout(Duration::from_secs(5), async {
                while !loop_generation().is_some_and(|current| current > before) {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .expect("Loop reloaded");
            assert_eq!(generation("Probe"), probe);
        }
        assert_ok(
            &client
                .rpc(
                    "session.submit",
                    json!({"sessionId":sid,"text":"tool:probe {}","mode":"queue"}),
                )
                .await,
        );
        assert_ok(&client.rpc("session.idle", json!({"sessionId":sid})).await);
        let result = last_result().await;
        assert!(result.contains(expected), "{result}");
        // Fixed tools are declared in `SessionConfig.tools` order.
        assert_eq!(
            declared.0.lock().unwrap().last().unwrap(),
            &["probe", "echo"]
        );
    }
    server.shutdown();
    server.join().await.unwrap();
}

/// A reload is a restart of Loop: a stop while Loop reloads waits for its
/// successor, which takes the interrupt over from the history, instead of
/// failing as retryable and leaving a second interrupt behind on retry.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stop_while_loop_reloads_waits_for_its_successor() {
    use dim_agent_capabilities::{
        adapters::DefaultPrepareModelPlugin,
        history::HistoryRenderer,
        host::{
            CompactionStrategyHost, ContextModelHost, IdentityContext, ModelHost,
            PrepareContextHost,
        },
    };
    use dim_agent_types::EventKind;
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let mut builder = AgentBuilder::new(AgentConfig::default());
    defaults::install(&mut builder);
    builder
        .session_value(Arc::new(HistoryRenderer::default()))
        .session_value(Arc::new(ModelHost(Arc::new(GatedModel {
            entered: entered.clone(),
            release: release.clone(),
            gated: std::sync::atomic::AtomicBool::new(true),
        }))))
        .session_value(Arc::new(PrepareContextHost(Arc::new(IdentityContext))))
        .session_value(Arc::new(ContextModelHost(None)))
        .session_value(Arc::new(CompactionStrategyHost(None)))
        .session_plugin(DefaultPrepareModelPlugin::new);
    let tools = ToolReload::default();
    tools.install(&mut builder, Arc::new(MockTools));
    let server = serve(
        builder,
        ServerConfig {
            unix_path: Some(
                std::path::PathBuf::from("/tmp")
                    .join(format!("dim-stop-reload-{}/s", uuid::Uuid::new_v4())),
            ),
            websocket_address: None,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let mut client = Client::new(&server, false).await;
    let created = client.rpc("session.create", create(json!([]))).await;
    assert_ok(&created);
    let sid = created["result"]["sessionId"].as_str().unwrap().to_owned();
    let session = server.agent().unwrap().get_session(&sid).unwrap();
    assert_ok(
        &client
            .rpc("session.submit", json!({"sessionId":sid,"text":"held"}))
            .await,
    );
    tokio::time::timeout(Duration::from_secs(5), entered.notified())
        .await
        .expect("model request in flight");
    // Loop stays unloaded until the provider is let go.
    let gate = tools.hold_next();
    let restart = tokio::spawn({
        let tools = tools.clone();
        async move { tools.restart().await }
    });
    tokio::time::timeout(Duration::from_secs(5), async {
        while session.state("main").is_ok() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("Loop unloaded");
    let stop = tokio::spawn({
        let session = session.clone();
        async move { session.stop("main", "stopped while reloading".into()).await }
    });
    tokio::time::timeout(Duration::from_secs(5), async {
        while !session
            .history("main", None)
            .await
            .unwrap()
            .iter()
            .any(|event| matches!(event.kind, EventKind::StopInterrupt(_)))
        {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("interrupt written");
    assert!(!stop.is_finished(), "stop waits for the reloading Loop");
    gate.notify_one();
    restart.await.unwrap();
    release.notify_one();
    tokio::time::timeout(Duration::from_secs(5), stop)
        .await
        .expect("stop returned")
        .unwrap()
        .unwrap();
    let interrupts = session
        .history("main", None)
        .await
        .unwrap()
        .iter()
        .filter(|event| matches!(event.kind, EventKind::StopInterrupt(_)))
        .count();
    assert_eq!(interrupts, 1);
    server.shutdown();
    server.join().await.unwrap();
}

/// State notifications follow the status across a Loop reload: a client that
/// subscribes while Loop is gone gets no stale state, and every client sees
/// the branch settle once the successor takes over, without subscribing again.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn state_notifications_follow_a_loop_reload() {
    use dim_agent_capabilities::{
        adapters::DefaultPrepareModelPlugin,
        history::HistoryRenderer,
        host::{
            CompactionStrategyHost, ContextModelHost, IdentityContext, ModelHost,
            PrepareContextHost,
        },
    };
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let mut builder = AgentBuilder::new(AgentConfig::default());
    defaults::install(&mut builder);
    builder
        .session_value(Arc::new(HistoryRenderer::default()))
        .session_value(Arc::new(ModelHost(Arc::new(GatedModel {
            entered: entered.clone(),
            release: release.clone(),
            gated: std::sync::atomic::AtomicBool::new(true),
        }))))
        .session_value(Arc::new(PrepareContextHost(Arc::new(IdentityContext))))
        .session_value(Arc::new(ContextModelHost(None)))
        .session_value(Arc::new(CompactionStrategyHost(None)))
        .session_plugin(DefaultPrepareModelPlugin::new);
    let tools = ToolReload::default();
    tools.install(&mut builder, Arc::new(MockTools));
    let server = serve(
        builder,
        ServerConfig {
            unix_path: Some(
                std::path::PathBuf::from("/tmp")
                    .join(format!("dim-state-reload-{}/s", uuid::Uuid::new_v4())),
            ),
            websocket_address: None,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let mut client = Client::new(&server, false).await;
    let created = client.rpc("session.create", create(json!([]))).await;
    assert_ok(&created);
    let sid = created["result"]["sessionId"].as_str().unwrap().to_owned();
    let session = server.agent().unwrap().get_session(&sid).unwrap();
    assert_ok(&client.rpc("session.attach", json!({"sessionId":sid})).await);
    assert_ok(
        &client
            .rpc("session.submit", json!({"sessionId":sid,"text":"held"}))
            .await,
    );
    tokio::time::timeout(Duration::from_secs(5), entered.notified())
        .await
        .expect("model request in flight");
    let gate = tools.hold_next();
    let restart = tokio::spawn({
        let tools = tools.clone();
        async move { tools.restart().await }
    });
    tokio::time::timeout(Duration::from_secs(5), async {
        while session.state("main").is_ok() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("Loop unloaded");
    // Subscribes while Loop is gone: there is no state to report yet.
    let mut watcher = Client::new(&server, false).await;
    assert_ok(
        &watcher
            .rpc("session.attach", json!({"sessionId":sid}))
            .await,
    );
    gate.notify_one();
    restart.await.unwrap();
    release.notify_one();
    for client in [&mut client, &mut watcher] {
        loop {
            let notice = client.notice("session.state").await;
            if notice["params"]["running"] == false {
                break;
            }
        }
    }
    server.shutdown();
    server.join().await.unwrap();
}

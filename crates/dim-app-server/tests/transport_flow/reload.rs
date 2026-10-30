use super::*;
use dim_agent_types::{SystemPrompt, SystemPromptPart};
use rutis::host::CtxHost;

pub(super) struct NamedExtension(pub String, pub Arc<AtomicUsize>);
impl Plugin for NamedExtension {
    type Deps = ();
    fn name(&self) -> &str {
        &self.0
    }
    async fn apply(&self, _: &rutis::Ctx, (): Self::Deps) -> rutis::Result {
        self.1.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
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
        for (index, (name, count)) in names.into_iter().zip(&counts).enumerate() {
            let (session, branch) = (count.clone(), count.clone());
            builder.require_feature(
                Feature::new(format!("test.named-extension-{index}"))
                    .session(format!("test.named-session-{index}"), move |_| {
                        Ok(Box::new(NamedExtension(name.into(), session.clone())))
                    })
                    .branch(format!("test.named-branch-{index}"), move |_| {
                        Ok(Box::new(NamedExtension(name.into(), branch.clone())))
                    }),
            );
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
    for (name, count) in names.into_iter().zip(counts) {
        assert_eq!(count.load(Ordering::SeqCst), 4, "{name}");
    }
    server.restart("Rpc").await.unwrap();
    let listed = client.rpc("session.list", json!({})).await;
    assert_ok(&listed);
    assert_eq!(listed["result"].as_array().unwrap().len(), 2);
    server.shutdown();
    server.join().await.unwrap();

    let mut builder = AgentBuilder::new(AgentConfig::default());
    builder.session_extension_part("Rpc", "extension", |_, _: &()| {
        Ok(Box::new(NamedExtension(
            "extension".into(),
            Arc::new(AtomicUsize::new(0)),
        )))
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
    use std::{
        io::{Read, Write},
        os::{fd::AsRawFd, unix::net::UnixStream, unix::process::CommandExt},
    };
    unsafe extern "C" {
        fn read(fd: i32, buffer: *mut u8, count: usize) -> isize;
        fn write(fd: i32, buffer: *const u8, count: usize) -> isize;
    }

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

    let (mut parent, child) = UnixStream::pair().unwrap();
    parent
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let spawning = std::thread::spawn(move || {
        let fd = child.as_raw_fd();
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command.arg("--list").stdout(std::process::Stdio::null());
        // SAFETY: the child uses only async-signal-safe read/write before exec;
        // its socket and both one-byte buffers remain live throughout the calls.
        unsafe {
            command.pre_exec(move || {
                let mut byte = 1_u8;
                if write(fd, &byte, 1) != 1 || read(fd, &mut byte, 1) != 1 {
                    return Err(std::io::Error::from_raw_os_error(5));
                }
                Ok(())
            });
        }
        command.spawn().unwrap().wait().unwrap()
    });
    let mut ready = [0];
    parent.read_exact(&mut ready).unwrap();
    assert_eq!(ready, [1]);
    let reloaded =
        tokio::time::timeout(Duration::from_secs(10), server.restart("Connection")).await;
    parent.write_all(&[1]).unwrap();
    assert!(spawning.join().unwrap().success());
    reloaded
        .expect("Connection reload must settle while a child holds inherited descriptors")
        .unwrap();
    let mut reconnected = Client::new(&server, false).await;
    assert_eq!(reconnected.instance, client.instance);
    assert_ok(
        &reconnected
            .rpc("session.attach", json!({"sessionId":sid}))
            .await,
    );

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
impl Plugin for FailingCleanup {
    type Deps = ();
    fn name(&self) -> &'static str {
        "test.failing-cleanup"
    }
    async fn apply(&self, ctx: &rutis::Ctx, (): Self::Deps) -> rutis::Result {
        let fail = self.0.clone();
        ctx.effect(move || async move {
            if fail.load(Ordering::SeqCst) {
                Err(rutis::Error::from(rutis::host::ErrorKind::PluginFailed(
                    "cleanup".into(),
                )))
            } else {
                Ok(())
            }
        })?;
        Ok(())
    }
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
        .process_value(Arc::new(ModelHost(Arc::new(GatedModel {
            entered: entered.clone(),
            release: release.clone(),
            gated: std::sync::atomic::AtomicBool::new(true),
        }))))
        .session_value(Arc::new(PrepareContextHost(Arc::new(IdentityContext))))
        .session_value(Arc::new(ContextModelHost(None)))
        .session_value(Arc::new(CompactionStrategyHost(None)))
        .require_feature(
            Feature::new("PrepareModelHost").session("PrepareModelHost", |names| {
                Ok(Box::new(DefaultPrepareModelPlugin::new(names)?))
            }),
        );
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

    tools.restart().await;
    tokio::time::timeout(Duration::from_secs(5), async {
        while !loop_generation().is_some_and(|(generation, state)| {
            generation > before && state == rutis::host::FiberState::Active
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
/// Registers `probe`; the count is the test's, so it continues across the
/// contributor's generations.
struct ProbeContributor(
    Arc<AtomicUsize>,
    rutis::host::Key<dim_agent_capabilities::tool_registry::ToolRegistry>,
);
struct ProbeTool(Arc<AtomicUsize>);
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
impl ProbeContributor {
    fn new(names: &dim_agent_runtime::Names<'_>, calls: Arc<AtomicUsize>) -> rutis::Result<Self> {
        Ok(Self(calls, names.key()?))
    }
}
impl rutis::host::DynPlugin for ProbeContributor {
    fn name(&self) -> &str {
        "Probe"
    }
    fn injects(&self) -> Vec<rutis::host::TypeKey> {
        vec![self.1.erase()]
    }
    fn apply<'a>(
        &'a self,
        ctx: &'a rutis::Ctx,
    ) -> rutis::host::BoxFuture<'a, rutis::Result<rutis::host::Effect>> {
        use dim_agent_capabilities::tool_registry::{ToolContribution, definition};
        Box::pin(async move {
            let tools = ctx.require_at(&self.1)?;
            tools.register(
                ctx,
                ToolContribution {
                    definition: definition("probe", "Count calls", json!({"type":"object"})),
                    handler: Arc::new(ProbeTool(self.0.clone())),
                },
            )?;
            Ok(rutis::host::Effect::Done)
        })
    }
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
        .process_value(Arc::new(ModelHost(declared.clone())))
        .session_value(Arc::new(PrepareContextHost(Arc::new(IdentityContext))))
        .session_value(Arc::new(ContextModelHost(None)))
        .session_value(Arc::new(CompactionStrategyHost(None)))
        .require_feature(
            Feature::new("PrepareModelHost").session("PrepareModelHost", |names| {
                Ok(Box::new(DefaultPrepareModelPlugin::new(names)?))
            }),
        )
        .require_feature(Feature::new("Probe").session("Probe", {
            let calls = Arc::new(AtomicUsize::new(0));
            move |names| Ok(Box::new(ProbeContributor::new(names, calls.clone())?))
        }));
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
            .find(|plugin| plugin.name == name && plugin.state == rutis::host::FiberState::Active)
            .map(|plugin| plugin.generation)
    };
    let loop_generation = || {
        session
            .branch_diagnostics("main")
            .unwrap()
            .plugins
            .into_iter()
            .find(|plugin| plugin.name == "Loop" && plugin.state == rutis::host::FiberState::Active)
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
            let (probe, before) = (generation("Probe").unwrap(), loop_generation().unwrap());
            tools.restart().await;
            tokio::time::timeout(Duration::from_secs(5), async {
                while !loop_generation().is_some_and(|current| current > before) {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .expect("Loop reloaded");
            assert!(generation("Probe").is_some_and(|current| current > probe));
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
        .process_value(Arc::new(ModelHost(Arc::new(GatedModel {
            entered: entered.clone(),
            release: release.clone(),
            gated: std::sync::atomic::AtomicBool::new(true),
        }))))
        .session_value(Arc::new(PrepareContextHost(Arc::new(IdentityContext))))
        .session_value(Arc::new(ContextModelHost(None)))
        .session_value(Arc::new(CompactionStrategyHost(None)))
        .require_feature(
            Feature::new("PrepareModelHost").session("PrepareModelHost", |names| {
                Ok(Box::new(DefaultPrepareModelPlugin::new(names)?))
            }),
        );
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
        .process_value(Arc::new(ModelHost(Arc::new(GatedModel {
            entered: entered.clone(),
            release: release.clone(),
            gated: std::sync::atomic::AtomicBool::new(true),
        }))))
        .session_value(Arc::new(PrepareContextHost(Arc::new(IdentityContext))))
        .session_value(Arc::new(ContextModelHost(None)))
        .session_value(Arc::new(CompactionStrategyHost(None)))
        .require_feature(
            Feature::new("PrepareModelHost").session("PrepareModelHost", |names| {
                Ok(Box::new(DefaultPrepareModelPlugin::new(names)?))
            }),
        );
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

/// The frame's first paragraph: the guide opens with it.
const FRAME_FIRST: &str = "# How to read your input";
/// The entry the guide probe registers from its own `apply`.
const GUIDE_PROBE_ENTRY: &str =
    r#"<dim-guide-probe source="test">: an entry a contributor registered for this flow."#;

fn system_of(prompt: &SystemPrompt) -> String {
    match prompt {
        SystemPrompt::Text(text) => text.clone(),
        SystemPrompt::Parts(parts) => parts
            .iter()
            .map(|part| match part {
                SystemPromptPart::Text { text, .. } => text.as_str(),
            })
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

/// The system text of the last model call on `branch`.
pub(super) async fn last_system(session: &dim_agent_runtime::Session, branch: &str) -> String {
    let events = session.history(branch, None).await.unwrap();
    let request = events
        .iter()
        .rev()
        .find_map(|event| match &event.kind {
            EventKind::ModelCall(data) => Some(data.request.clone()),
            _ => None,
        })
        .expect("a model call");
    system_of(request.system.as_deref().expect("a system text"))
}

/// A Session part that mounts a guide probe; the owner keeps the probe's
/// view so the test reloads one generation of it.
struct GuideProbeOwner {
    flag: Arc<std::sync::atomic::AtomicBool>,
    views: Arc<std::sync::Mutex<Vec<rutis::host::FiberView>>>,
    guide: rutis::host::Key<dim_agent_capabilities::prompt_guide::GuideEntries>,
}
impl rutis::Plugin for GuideProbeOwner {
    type Deps = ();
    async fn apply(&self, ctx: &rutis::Ctx, _: ()) -> rutis::Result {
        let view = ctx.plugin(GuideProbe {
            flag: self.flag.clone(),
            guide: self.guide.clone(),
        });
        (&view).await.map_err(|error| {
            rutis::Error::from(rutis::host::ErrorKind::PluginFailed(
                format!("guide probe failed: {error}").into(),
            ))
        })?;
        self.views.lock().unwrap().push(view);
        Ok(())
    }
}

/// A contributor: while the test's flag is set, each generation registers
/// an entry for the marker it writes, from its own `apply`.
struct GuideProbe {
    flag: Arc<std::sync::atomic::AtomicBool>,
    guide: rutis::host::Key<dim_agent_capabilities::prompt_guide::GuideEntries>,
}
impl rutis::host::DynPlugin for GuideProbe {
    fn name(&self) -> &str {
        "GuideProbe"
    }
    fn injects(&self) -> Vec<rutis::host::TypeKey> {
        vec![self.guide.erase()]
    }
    fn apply<'a>(
        &'a self,
        ctx: &'a rutis::Ctx,
    ) -> rutis::host::BoxFuture<'a, rutis::Result<rutis::host::Effect>> {
        use dim_agent_capabilities::prompt_guide::{GuideEntry, GuideScope};
        Box::pin(async move {
            let guide = ctx.require_at(&self.guide)?;
            if self.flag.load(Ordering::SeqCst) {
                guide.register(
                    ctx,
                    GuideScope::Session,
                    GuideEntry {
                        markers: vec!["dim-guide-probe".into()],
                        order: 65,
                        text: GUIDE_PROBE_ENTRY.into(),
                    },
                )?;
            }
            Ok(rutis::host::Effect::Done)
        })
    }
}

/// An entry registers from a contributor's `apply`, is in the request
/// while that generation is loaded, and leaves the request with it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_entry_leaves_with_its_contributor() {
    let flag = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let views = Arc::new(std::sync::Mutex::new(Vec::new()));
    let (install_flag, install_views) = (flag.clone(), views.clone());
    let (server, agent) = launch_with(
        Arc::new(MockTools),
        ServerConfig::default(),
        move |builder| {
            builder.require_feature(Feature::new("GuideProbeOwner").session(
                "GuideProbeOwner",
                move |names| {
                    Ok(Box::new(GuideProbeOwner {
                        flag: install_flag.clone(),
                        views: install_views.clone(),
                        guide: names.key()?,
                    }))
                },
            ));
        },
    )
    .await;
    let mut client = Client::new(&server, false).await;
    let created = client.rpc("session.create", create(json!(["echo"]))).await;
    assert_ok(&created);
    let sid = created["result"]["sessionId"].as_str().unwrap().to_owned();
    let session = agent.get_session(&sid).unwrap();

    // Loaded, the contributor's entry is part of the request's guide.
    assert_ok(
        &client
            .rpc(
                "session.submit",
                json!({"sessionId":sid,"text":"while loaded"}),
            )
            .await,
    );
    assert_ok(&client.rpc("session.idle", json!({"sessionId":sid})).await);
    let loaded = last_system(&session, "main").await;
    assert!(loaded.starts_with(FRAME_FIRST), "{loaded:?}");
    assert!(loaded.contains(GUIDE_PROBE_ENTRY), "{loaded:?}");

    // The next generation does not register: the entry leaves the request
    // as soon as its contributor is gone.
    flag.store(false, Ordering::SeqCst);
    let view = views.lock().unwrap().last().cloned().expect("a probe");
    view.restart().await.unwrap();
    assert_ok(
        &client
            .rpc(
                "session.submit",
                json!({"sessionId":sid,"text":"after reload"}),
            )
            .await,
    );
    assert_ok(&client.rpc("session.idle", json!({"sessionId":sid})).await);
    let after = last_system(&session, "main").await;
    assert!(after.starts_with(FRAME_FIRST), "{after:?}");
    assert!(!after.contains(GUIDE_PROBE_ENTRY), "{after:?}");

    server.shutdown();
    server.join().await.unwrap();
}

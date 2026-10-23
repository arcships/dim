//! ToolBinding with a dynamic tool source (`tool-contributions.md` §7.7):
//! adoption, request snapshots, version pinning and release.

mod local_support;

use dim_agent_capabilities::{
    defaults,
    history::HistoryRenderer,
    host::*,
    tool_binding::ToolBinding,
    tool_registry::{
        SessionToolSource, ToolContribution, ToolHandler, ToolProvider, ToolProviderChanged,
        ToolRegistry, ToolSourceSet, ToolSourcesState, ToolSourcesView, definition,
    },
};
use dim_agent_runtime::*;
use dim_agent_types::*;
use local_support::*;
use rutis::{BoxFuture, CordisError, Ctx, Effect, EventKey, TypedPlugin};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio_util::sync::CancellationToken;

/// Calls and handler drops, by source version.
#[derive(Default)]
struct Log {
    calls: Mutex<Vec<String>>,
    dropped: Mutex<Vec<String>>,
}
struct Handler {
    version: String,
    log: Arc<Log>,
    hold: Option<Arc<Gate>>,
}
impl Drop for Handler {
    fn drop(&mut self) {
        self.log.dropped.lock().unwrap().push(self.version.clone());
    }
}
impl ToolHandler for Handler {
    fn execute(
        &self,
        call: ToolCall,
        _: ToolExecutionContext,
        _: CancellationToken,
    ) -> HostFuture<'_, ToolResult> {
        Box::pin(async move {
            self.log
                .calls
                .lock()
                .unwrap()
                .push(format!("{}:{}", self.version, call.name));
            if let Some(gate) = &self.hold {
                gate.entered.notify_one();
                gate.release.notified().await;
            }
            Ok(ToolResult::text(format!(
                "{} ran {}",
                self.version, call.name
            )))
        })
    }
}

struct TestSource {
    sets: Mutex<Vec<Arc<ToolSourceSet>>>,
    // Initial values are moved into the next Session's source.
    emitter: Option<(Ctx, SessionScope)>,
}
impl SessionService for TestSource {}
impl PublicService for TestSource {}
impl TestSource {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            sets: Mutex::default(),
            emitter: None,
        })
    }
    fn seed(&self, sets: Vec<Arc<ToolSourceSet>>) {
        assert!(self.emitter.is_none(), "seed before creating the Session");
        *self.sets.lock().unwrap() = sets;
    }
    fn offer(&self, sets: Vec<Arc<ToolSourceSet>>) {
        let (ctx, scope) = self
            .emitter
            .as_ref()
            .expect("offer through the Session's TestSource service");
        *self.sets.lock().unwrap() = sets;
        ctx.events()
            .emit(
                ctx,
                &EventKey::<ToolProviderChanged>::of().instance(scope.instance),
                Arc::new(ToolProviderChanged),
            )
            .unwrap();
    }
}
impl ToolProvider for TestSource {
    fn sets(&self) -> Vec<Arc<ToolSourceSet>> {
        self.sets.lock().unwrap().clone()
    }
}

fn set(
    key: &str,
    version: &str,
    names: &[&str],
    log: &Arc<Log>,
    hold: Option<Arc<Gate>>,
) -> Arc<ToolSourceSet> {
    Arc::new(ToolSourceSet {
        key: key.into(),
        version: version.into(),
        tools: names
            .iter()
            .map(|name| {
                let mut definition =
                    definition(name, "Source tool", serde_json::json!({"type":"object"}));
                definition.risk = Some("session".into());
                Arc::new(ToolContribution {
                    definition,
                    handler: Arc::new(Handler {
                        version: version.into(),
                        log: log.clone(),
                        hold: hold.clone(),
                    }),
                })
            })
            .collect(),
    })
}

struct SourcePlugin {
    scope: SessionScope,
    source: Arc<TestSource>,
    name: &'static str,
}
impl TypedPlugin for SourcePlugin {
    type Deps = ();
    fn name(&self) -> &str {
        "TestSource"
    }
    fn apply<'a>(
        &'a self,
        ctx: &'a Ctx,
        (): Self::Deps,
    ) -> BoxFuture<'a, std::result::Result<Effect, CordisError>> {
        Box::pin(async move {
            let source = Arc::new(TestSource {
                sets: Mutex::new(std::mem::take(&mut *self.source.sets.lock().unwrap())),
                emitter: Some((ctx.clone(), self.scope)),
            });
            ctx.provide_as(self.scope.key::<TestSource>(), source.clone())?;
            ctx.provide_as(
                self.scope.named_key::<SessionToolSource>(self.name),
                Arc::new(SessionToolSource(source)),
            )?;
            Ok(Effect::Done)
        })
    }
}
impl Capability for SourcePlugin {
    fn keys(&self) -> KeysOf<Self> {}
}

struct Env {
    agent: Agent,
    model: Arc<ScriptedModel>,
}

fn selection(model_id: &str) -> ModelSelection {
    ModelSelection {
        provider_id: "mock".into(),
        model_id: model_id.into(),
        capability_snapshot: Some(ModelCapabilitySnapshot {
            target: ModelId {
                provider_id: "mock".into(),
                model_id: model_id.into(),
            },
            capabilities: ModelCapabilities {
                context_window: if model_id == "second" { 98765 } else { 135791 },
                ..Default::default()
            },
            reasoning: None,
            service_tier: None,
        }),
        config: None,
        metadata: None,
    }
}

fn env(source: Arc<TestSource>, store: Option<Arc<MemoryTimelineStore>>) -> Env {
    env_named(source, store, "mcp")
}

/// `env` with the test source registered under `name`.
fn env_named(
    source: Arc<TestSource>,
    store: Option<Arc<MemoryTimelineStore>>,
    name: &'static str,
) -> Env {
    let model = Arc::new(ScriptedModel::default());
    let mut builder = AgentBuilder::new(AgentConfig::default());
    if let Some(store) = store {
        builder.timeline_store(store);
    }
    builder
        .session_value(Arc::new(HistoryRenderer::default()))
        .session_value(Arc::new(ModelHost(model.clone())))
        .session_value(Arc::new(PrepareContextHost(Arc::new(IdentityContext))))
        .session_value(Arc::new(ContextModelHost(None)))
        .session_value(Arc::new(CompactionStrategyHost(None)))
        .session_plugin(dim_agent_capabilities::adapters::DefaultPrepareModelPlugin::new);
    defaults::tool_host(&mut builder, Arc::new(GateTools(Arc::new(Gate::default()))));
    defaults::install_with_prompt(
        &mut builder,
        Arc::new(EmptyPromptHost),
        Some(ConfigureUpdate {
            sources: Vec::new(),
            model: Some(selection("scripted")),
        }),
    );
    builder
        .session_tool_source(name)
        .session_plugin(move |scope| SourcePlugin {
            scope: *scope,
            source: source.clone(),
            name,
        });
    Env {
        agent: builder.build(),
        model,
    }
}

impl Env {
    async fn session(&self) -> Session {
        self.agent
            .create_session(SessionConfig {
                tools: vec!["gate".into()],
                ..SessionConfig::default()
            })
            .await
            .unwrap()
    }
    /// Runs one turn on `branch` whose model makes `calls`, then replies.
    async fn turn(&self, session: &Session, branch: &str, calls: &[&'static str]) {
        for name in calls {
            self.model.push(Step::Tool(name, serde_json::json!({})));
        }
        self.model.push(Step::Text("done"));
        session
            .submit("go".into(), InputMode::Queue, branch)
            .await
            .unwrap();
        session.idle(branch).await.unwrap();
    }
}

/// Waits until `branch` adopted exactly these source versions.
async fn adopted(session: &Session, branch: &str, sources: &[(&str, &str)]) {
    let expected: Vec<ToolSourceVersion> = sources
        .iter()
        .map(|(key, version)| ToolSourceVersion {
            key: (*key).into(),
            version: (*version).into(),
        })
        .collect();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let current = session
                .history(branch, None)
                .await
                .unwrap()
                .iter()
                .rev()
                .find_map(|event| match &event.kind {
                    EventKind::ContextConfigured(data) => Some(data.settings.tool_sources.clone()),
                    _ => None,
                });
            let registered = current.as_ref().map(|sources| {
                sources
                    .iter()
                    .filter(|source| !source.key.starts_with("#contributed:"))
                    .cloned()
                    .collect::<Vec<_>>()
            });
            if registered.as_ref() == Some(&expected) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("tool set adopted");
}

fn declared(request: &ModelRequest) -> Vec<String> {
    request
        .tools
        .iter()
        .flat_map(|tools| tools.iter())
        .map(|tool| tool.name.clone())
        .collect()
}

async fn last_result(session: &Session, branch: &str) -> String {
    session
        .history(branch, None)
        .await
        .unwrap()
        .iter()
        .rev()
        .find_map(|event| match &event.kind {
            EventKind::ToolResult(data) => match &data.results[0].result.content {
                ToolContent::Text(text) => Some(text.clone()),
                _ => None,
            },
            _ => None,
        })
        .unwrap()
}

struct ContributorEnabled;
struct EnableContributor;
impl TypedPlugin for EnableContributor {
    type Deps = ();
    fn name(&self) -> &str {
        "EnableContributor"
    }
    fn apply<'a>(
        &'a self,
        ctx: &'a Ctx,
        (): (),
    ) -> BoxFuture<'a, std::result::Result<Effect, CordisError>> {
        Box::pin(async move {
            ctx.provide(ContributorEnabled)?;
            Ok(Effect::Done)
        })
    }
}
impl Capability for EnableContributor {
    fn keys(&self) -> KeysOf<Self> {}
}
struct OptionalContributor {
    scope: SessionScope,
    generations: Arc<AtomicUsize>,
    hold: Arc<Gate>,
    /// Every generation's contribution, to observe its release.
    provided: Arc<Mutex<Vec<std::sync::Weak<ToolContribution>>>>,
}
struct OptionalHandler {
    generation: usize,
    stopping: CancellationToken,
    hold: Arc<Gate>,
}
impl ToolHandler for OptionalHandler {
    fn execute(
        &self,
        call: ToolCall,
        _: ToolExecutionContext,
        cancel: CancellationToken,
    ) -> HostFuture<'_, ToolResult> {
        Box::pin(async move {
            if self.stopping.is_cancelled() {
                return Err(HostError::Cancelled("provider unloaded".into()));
            }
            if call.input.get("hold") == Some(&serde_json::json!(true)) {
                self.hold.entered.notify_one();
                tokio::select! {
                    biased;
                    _ = self.stopping.cancelled() => return Err(HostError::Cancelled("provider unloaded".into())),
                    _ = cancel.cancelled() => return Err(HostError::Cancelled("call cancelled".into())),
                    _ = self.hold.release.notified() => {}
                }
            }
            Ok(ToolResult::text(format!("generation {}", self.generation)))
        })
    }
}
impl TypedPlugin for OptionalContributor {
    type Deps = Arc<ContributorEnabled>;
    fn name(&self) -> &str {
        "OptionalContributor"
    }
    fn apply<'a>(
        &'a self,
        ctx: &'a Ctx,
        _: Self::Deps,
    ) -> BoxFuture<'a, std::result::Result<Effect, CordisError>> {
        Box::pin(async move {
            let generation = self.generations.fetch_add(1, Ordering::SeqCst) + 1;
            let contribution = Arc::new(ToolContribution {
                definition: definition(
                    "optional",
                    "Report provider generation",
                    serde_json::json!({"type":"object"}),
                ),
                handler: Arc::new(OptionalHandler {
                    generation,
                    stopping: ctx.cancellation_token(),
                    hold: self.hold.clone(),
                }),
            });
            self.provided
                .lock()
                .unwrap()
                .push(Arc::downgrade(&contribution));
            ctx.provide_as(
                self.scope.named_key::<ToolContribution>("optional"),
                contribution,
            )?;
            Ok(Effect::Done)
        })
    }
}
impl Capability for OptionalContributor {
    fn keys(&self) -> KeysOf<Self> {}
}

async fn plugin_state(view: &rutis::FiberView, state: rutis::FiberState) {
    tokio::time::timeout(Duration::from_secs(5), async {
        view.watch()
            .wait_for(|snapshot| snapshot.state == state)
            .await
            .unwrap();
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn optional_contributors_do_not_gate_the_loop_and_follow_new_generations() {
    let root = Ctx::root().unwrap();
    let enabled = root.plugin(capability_plugin(EnableContributor));
    plugin_state(&enabled, rutis::FiberState::Active).await;
    let generations = Arc::new(AtomicUsize::new(0));
    let hold = Arc::new(Gate::default());
    let held = hold.clone();
    let provided: Arc<Mutex<Vec<std::sync::Weak<ToolContribution>>>> = Arc::default();
    let mut builder = AgentBuilder::new(AgentConfig::default());
    defaults::mock_hosts(&mut builder);
    defaults::install_with_prompt(
        &mut builder,
        Arc::new(EmptyPromptHost),
        Some(ConfigureUpdate {
            sources: Vec::new(),
            model: Some(selection("scripted")),
        }),
    );
    builder
        .session_tool("optional")
        .session_tool("absent")
        .session_plugin({
            let provided = provided.clone();
            move |scope| OptionalContributor {
                scope: *scope,
                generations: generations.clone(),
                hold: held.clone(),
                provided: provided.clone(),
            }
        });
    let agent = builder.build_in(&root);
    let session = agent
        .create_session(SessionConfig {
            tools: vec!["optional".into(), "absent".into(), "echo".into()],
            permission_policy: PermissionPolicy {
                allowed_tools: vec!["echo".into(), "optional".into()],
                ..PermissionPolicy::default()
            },
            ..SessionConfig::default()
        })
        .await
        .unwrap();
    session
        .submit("tool:optional {}".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    session.idle("main").await.unwrap();
    assert_eq!(
        declared(&requests(&session).await[0]),
        // `branch` is an active contribution, so it is offered unlisted.
        ["optional", "echo", "branch"]
    );
    assert_eq!(last_result(&session, "main").await, "generation 1");
    let snapshot = session.diagnostics().unwrap();
    let registry = snapshot
        .plugins
        .iter()
        .find(|plugin| plugin.name == "ToolRegistry")
        .unwrap();
    let registry_id = registry.id;
    let registry_generation = registry.generation;
    let provider = root
        .view(
            snapshot
                .plugins
                .iter()
                .find(|plugin| plugin.name == "OptionalContributor")
                .unwrap()
                .id,
        )
        .unwrap();
    let before = session.sequence("main").unwrap();
    session
        .submit(
            "tool:optional {\"hold\":true}".into(),
            InputMode::Queue,
            "main",
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), hold.entered.notified())
        .await
        .unwrap();
    enabled.dispose().await.unwrap();
    plugin_state(&provider, rutis::FiberState::Pending).await;
    session.idle("main").await.unwrap();
    assert!(
        last_result(&session, "main")
            .await
            .contains("provider unloaded")
    );
    let history = session.history("main", None).await.unwrap();
    assert_eq!(
        history
            .iter()
            .filter(
                |event| event.sequence > before && matches!(event.kind, EventKind::ToolResult(_))
            )
            .count(),
        1
    );
    session
        .submit(
            "tool:echo {\"text\":\"still running\"}".into(),
            InputMode::Queue,
            "main",
        )
        .await
        .unwrap();
    session.idle("main").await.unwrap();
    assert!(
        last_result(&session, "main")
            .await
            .contains("still running")
    );
    let current = session.diagnostics().unwrap();
    let registry = current
        .plugins
        .iter()
        .find(|plugin| plugin.id == registry_id)
        .unwrap();
    assert_eq!(registry.state, rutis::FiberState::Active);
    assert_eq!(registry.generation, registry_generation);

    // The provider's next generation is offered without a Session reload
    // (#241); this configuration has no tool part, so the next request
    // declares it with no adoption fact.
    let reloaded = session.sequence("main").unwrap();
    let enabled = root.plugin(capability_plugin(EnableContributor));
    plugin_state(&enabled, rutis::FiberState::Active).await;
    plugin_state(&provider, rutis::FiberState::Active).await;
    session
        .submit("tool:optional {}".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    session.idle("main").await.unwrap();
    assert_eq!(last_result(&session, "main").await, "generation 2");
    // Generation 1 is released once its requests finished: nothing that
    // binds through `bind_current` pins it.
    assert!(provided.lock().unwrap()[0].upgrade().is_none());
    assert!(
        !session
            .history("main", None)
            .await
            .unwrap()
            .iter()
            .any(|event| event.sequence > reloaded
                && matches!(event.kind, EventKind::ContextConfigured(_))),
        "a contributed change writes no adoption without a tool part"
    );
    let branches = session.branch_diagnostics("main").unwrap();
    let previous_loop = branches
        .plugins
        .iter()
        .find(|plugin| plugin.name == "Loop")
        .unwrap();
    let loop_view = root.view(previous_loop.id).unwrap();
    root.view(registry_id).unwrap().restart().await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop_view
            .watch()
            .wait_for(|snapshot| {
                snapshot.generation > previous_loop.generation
                    && snapshot.state == rutis::FiberState::Active
            })
            .await
            .unwrap();
    })
    .await
    .unwrap();
    session
        .submit("tool:optional {}".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    session.idle("main").await.unwrap();
    assert_eq!(last_result(&session, "main").await, "generation 2");
    agent.close().await.unwrap();
    root.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn contributors_are_current_for_the_first_request_without_initial_configuration() {
    let root = Ctx::root().unwrap();
    let enabled = root.plugin(capability_plugin(EnableContributor));
    plugin_state(&enabled, rutis::FiberState::Active).await;
    let store = Arc::new(MemoryTimelineStore::default());
    let generations = Arc::new(AtomicUsize::new(0));
    let hold = Arc::new(Gate::default());
    let build = || {
        let mut builder = AgentBuilder::new(AgentConfig::default());
        defaults::mock_hosts(&mut builder);
        defaults::install(&mut builder);
        builder
            .timeline_store(store.clone())
            .session_tool("optional")
            .session_plugin({
                let generations = generations.clone();
                let hold = hold.clone();
                move |scope| OptionalContributor {
                    scope: *scope,
                    generations: generations.clone(),
                    hold: hold.clone(),
                    provided: Arc::default(),
                }
            });
        builder.build_in(&root)
    };
    let agent = build();
    let session = agent
        .create_session(SessionConfig {
            tools: vec!["optional".into(), "echo".into()],
            permission_policy: PermissionPolicy {
                allowed_tools: vec!["echo".into(), "optional".into()],
                ..PermissionPolicy::default()
            },
            ..SessionConfig::default()
        })
        .await
        .unwrap();
    let provider = root
        .view(
            session
                .diagnostics()
                .unwrap()
                .plugins
                .iter()
                .find(|plugin| plugin.name == "OptionalContributor")
                .unwrap()
                .id,
        )
        .unwrap();
    enabled.dispose().await.unwrap();
    plugin_state(&provider, rutis::FiberState::Pending).await;
    let enabled = root.plugin(capability_plugin(EnableContributor));
    plugin_state(&provider, rutis::FiberState::Active).await;
    session
        .submit("tool:optional {}".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    session.idle("main").await.unwrap();
    // Without a tool part each request declares the current contributors, so
    // the first request already reaches the provider's second generation.
    assert_eq!(
        declared(&requests(&session).await[0]),
        ["optional", "echo", "branch"]
    );
    assert_eq!(last_result(&session, "main").await, "generation 2");
    let id = session.id().to_owned();
    agent.close().await.unwrap();

    let agent = build();
    let restored = agent.restore_session(id).await.unwrap();
    restored
        .submit("tool:optional {}".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    restored.idle("main").await.unwrap();
    assert_eq!(
        declared(requests(&restored).await.last().unwrap()),
        ["optional", "echo", "branch"]
    );
    assert_eq!(last_result(&restored, "main").await, "generation 3");
    agent.close().await.unwrap();
    enabled.dispose().await.unwrap();
    root.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_accepted_source_change_is_adopted_after_its_emitter_closes() {
    struct Emitter(tokio::sync::mpsc::UnboundedSender<Ctx>);
    impl rutis::Plugin for Emitter {
        fn name(&self) -> &str {
            "SourceEmitter"
        }
        fn apply<'a>(
            &'a self,
            ctx: &'a Ctx,
        ) -> BoxFuture<'a, std::result::Result<Effect, CordisError>> {
            Box::pin(async move {
                self.0.send(ctx.clone()).unwrap();
                Ok(Effect::Done)
            })
        }
    }
    struct Pause(Arc<Gate>);
    impl rutis::Listener<ToolProviderChanged> for Pause {
        fn call<'a>(
            &'a self,
            _: &'a Ctx,
            _: &'a ToolProviderChanged,
        ) -> BoxFuture<'a, std::result::Result<Option<()>, CordisError>> {
            Box::pin(async move {
                self.0.entered.notify_one();
                self.0.release.notified().await;
                Ok(None)
            })
        }
    }

    let (source, log) = (TestSource::new(), Arc::new(Log::default()));
    source.seed(vec![set("mcp:a", "v1", &["lookup"], &log, None)]);
    let env = env(source.clone(), None);
    let session = env.session().await;
    source.seed(vec![set("mcp:a", "v1", &["lookup"], &log, None)]);
    let other = env.session().await;
    adopted(&session, "main", &[("mcp:a", "v1")]).await;
    adopted(&other, "main", &[("mcp:a", "v1")]).await;
    let source = session.session_service::<TestSource>().unwrap();
    let (ctx, scope) = source.emitter.as_ref().unwrap();
    let key = EventKey::<ToolProviderChanged>::of().instance(scope.instance);

    let (send, mut receive) = tokio::sync::mpsc::unbounded_channel();
    let emitter = ctx.plugin(Emitter(send));
    (&emitter).await.unwrap();
    let emitting = receive.recv().await.unwrap();

    // Hold one dispatch after its registry read so the next accepted change
    // remains queued until its emitter has started closing.
    let gate = Arc::new(Gate::default());
    ctx.events().once(ctx, &key, Pause(gate.clone())).unwrap();
    ctx.events()
        .emit(ctx, &key, Arc::new(ToolProviderChanged))
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), gate.entered.notified())
        .await
        .unwrap();
    *source.sets.lock().unwrap() = vec![set("mcp:a", "v2", &["lookup"], &log, None)];
    emitting
        .events()
        .emit(&emitting, &key, Arc::new(ToolProviderChanged))
        .unwrap();
    let disposal = emitter.shutdown();
    gate.release.notify_one();
    tokio::time::timeout(Duration::from_secs(5), disposal)
        .await
        .unwrap()
        .unwrap();

    adopted(&session, "main", &[("mcp:a", "v2")]).await;
    // The other Session has its own source state and scoped notification key.
    assert_eq!(
        other.session_service::<ToolSourcesView>().unwrap().get().0["mcp:a"]
            .adopted
            .as_deref(),
        Some("v1")
    );
    env.turn(&session, "main", &["lookup"]).await;
    assert_eq!(last_result(&session, "main").await, "v2 ran lookup");
    env.turn(&other, "main", &["lookup"]).await;
    assert_eq!(last_result(&other, "main").await, "v1 ran lookup");
    env.agent.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_call_runs_the_version_its_request_declared_and_old_versions_are_released() {
    let (source, log, hold) = (
        TestSource::new(),
        Arc::new(Log::default()),
        Arc::new(Gate::default()),
    );
    source.seed(vec![set(
        "mcp:a",
        "v1",
        &["lookup"],
        &log,
        Some(hold.clone()),
    )]);
    let env = env(source.clone(), None);
    let session = env.session().await;
    let source = session.session_service::<TestSource>().unwrap();
    adopted(&session, "main", &[("mcp:a", "v1")]).await;

    // The call starts on v1; v2 is adopted while it runs.
    env.model.push(Step::Tool("lookup", serde_json::json!({})));
    env.model.push(Step::Text("done"));
    session
        .submit("go".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    hold.entered.notified().await;
    source.offer(vec![set("mcp:a", "v2", &["lookup"], &log, None)]);
    adopted(&session, "main", &[("mcp:a", "v2")]).await;
    hold.release.notify_one();
    session.idle("main").await.unwrap();
    assert_eq!(last_result(&session, "main").await, "v1 ran lookup");
    assert_eq!(
        declared(&requests(&session).await[0]),
        ["gate", "branch", "lookup"]
    );

    // The next request declares and runs v2; binding it releases v1.
    env.turn(&session, "main", &["lookup"]).await;
    assert_eq!(last_result(&session, "main").await, "v2 ran lookup");
    assert!(log.dropped.lock().unwrap().contains(&"v1".to_owned()));
    assert!(!log.dropped.lock().unwrap().contains(&"v2".to_owned()));

    // A withdrawn source is no longer declared or run.
    source.offer(Vec::new());
    adopted(&session, "main", &[]).await;
    let calls = log.calls.lock().unwrap().len();
    env.turn(&session, "main", &[]).await;
    assert_eq!(log.calls.lock().unwrap().len(), calls);
    assert_eq!(
        declared(requests(&session).await.last().unwrap()),
        ["gate", "branch"]
    );
    env.agent.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn conflicting_sets_are_rejected_whole_and_recover_once_the_name_is_free() {
    let (source, log) = (TestSource::new(), Arc::new(Log::default()));
    // `gate` is a fixed tool; `b` repeats `a`'s tool.
    source.seed(vec![
        set("mcp:a", "a1", &["find"], &log, None),
        set("mcp:b", "b1", &["find", "other"], &log, None),
        set("mcp:c", "c1", &["gate"], &log, None),
        set("mcp:d", "d1", &["branch"], &log, None),
    ]);
    let env = env(source.clone(), None);
    let session = env.session().await;
    let source = session.session_service::<TestSource>().unwrap();
    adopted(&session, "main", &[("mcp:a", "a1")]).await;
    let state = session.session_service::<ToolSourcesView>().unwrap().get();
    assert_eq!(state.0["mcp:a"].adopted.as_deref(), Some("a1"));
    assert_eq!(state.0["mcp:b"].adopted, None);
    assert_eq!(state.0["mcp:b"].rejected.as_ref().unwrap().version, "b1");
    assert!(state.0["mcp:c"].rejected.is_some());
    assert!(state.0["mcp:d"].rejected.is_some());
    env.turn(&session, "main", &[]).await;
    assert_eq!(
        declared(&requests(&session).await[0]),
        ["gate", "branch", "find"]
    );

    // Withdrawing `a` frees the name: `b` is adopted as a whole.
    source.offer(vec![set("mcp:b", "b1", &["find", "other"], &log, None)]);
    adopted(&session, "main", &[("mcp:b", "b1")]).await;
    env.turn(&session, "main", &["other"]).await;
    assert_eq!(last_result(&session, "main").await, "b1 ran other");
    assert_eq!(
        declared(requests(&session).await.last().unwrap()),
        ["gate", "branch", "find", "other"]
    );
    env.agent.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_name_freed_in_the_same_refresh_admits_the_rejected_set() {
    let (source, log) = (TestSource::new(), Arc::new(Log::default()));
    source.seed(vec![set("mcp:z", "z1", &["lookup"], &log, None)]);
    let env = env(source.clone(), None);
    let session = env.session().await;
    let source = session.session_service::<TestSource>().unwrap();
    adopted(&session, "main", &[("mcp:z", "z1")]).await;
    // `a` wants the name `z` holds.
    source.offer(vec![
        set("mcp:a", "a1", &["lookup"], &log, None),
        set("mcp:z", "z1", &["lookup"], &log, None),
    ]);
    tokio::time::timeout(Duration::from_secs(5), async {
        while session
            .session_service::<ToolSourcesView>()
            .unwrap()
            .get()
            .0
            .get("mcp:a")
            .is_none_or(|status| status.rejected.is_none())
        {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("a rejected");
    // One change frees the name: `a` is checked before `z` is replaced, and
    // still adopted in the same refresh.
    source.offer(vec![
        set("mcp:a", "a1", &["lookup"], &log, None),
        set("mcp:z", "z2", &["other"], &log, None),
    ]);
    adopted(&session, "main", &[("mcp:a", "a1"), ("mcp:z", "z2")]).await;
    env.turn(&session, "main", &["lookup"]).await;
    assert_eq!(last_result(&session, "main").await, "a1 ran lookup");
    env.agent.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_contributed_tool_is_offered_without_being_listed() {
    // `branch` is contributed by a Session capability and is not named in the
    // Session's tool list; the row being active is the opt-in (#364).
    let (source, log) = (TestSource::new(), Arc::new(Log::default()));
    source.seed(vec![set("mcp:a", "v1", &["lookup"], &log, None)]);
    let env = env(source, None);
    let session = env
        .agent
        .create_session(SessionConfig {
            tools: vec!["gate".into()],
            ..SessionConfig::default()
        })
        .await
        .unwrap();
    adopted(&session, "main", &[("mcp:a", "v1")]).await;
    env.turn(&session, "main", &[]).await;
    let declared = declared(&requests(&session).await[0]);
    assert!(declared.contains(&"branch".to_owned()), "{declared:?}");
    assert!(declared.contains(&"gate".to_owned()), "{declared:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_child_branch_binds_the_inherited_tools() {
    let (source, log) = (TestSource::new(), Arc::new(Log::default()));
    source.seed(vec![set("mcp:a", "v1", &["lookup"], &log, None)]);
    let env = env(source.clone(), None);
    let session = env.session().await;
    adopted(&session, "main", &[("mcp:a", "v1")]).await;
    env.model.push(Step::Tool("lookup", serde_json::json!({})));
    env.model.push(Step::Text("done"));
    let child = session
        .branch(
            "main".into(),
            session.sequence("main").unwrap(),
            BranchOptions {
                prompt: "child".into(),
                source: BranchSource::Host {
                    operation_id: "child".into(),
                },
            },
        )
        .await
        .unwrap();
    session.idle(&child).await.unwrap();
    assert_eq!(last_result(&session, &child).await, "v1 ran lookup");
    env.agent.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_restored_branch_adopts_the_new_versions_before_its_first_request() {
    let store = Arc::new(MemoryTimelineStore::default());
    let (source, log) = (TestSource::new(), Arc::new(Log::default()));
    source.seed(vec![set("mcp:a", "v1", &["lookup"], &log, None)]);
    let env1 = env(source, Some(store.clone()));
    let session = env1.session().await;
    adopted(&session, "main", &[("mcp:a", "v1")]).await;
    let id = session.id().to_owned();
    env1.agent.close().await.unwrap();

    // A new process: the source offers a new version of the same key.
    let source = TestSource::new();
    source.seed(vec![set("mcp:a", "v2", &["lookup"], &log, None)]);
    let env2 = env(source, Some(store));
    let restored = env2.agent.restore_session(id).await.unwrap();
    adopted(&restored, "main", &[("mcp:a", "v2")]).await;
    env2.turn(&restored, "main", &["lookup"]).await;
    assert_eq!(last_result(&restored, "main").await, "v2 ran lookup");
    env2.agent.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_model_update_and_a_tool_adoption_both_take_effect() {
    let (source, log) = (TestSource::new(), Arc::new(Log::default()));
    let env = env(source.clone(), None);
    let session = env.session().await;
    let source = session.session_service::<TestSource>().unwrap();
    let (configured, _) = tokio::join!(
        session.configure(
            ConfigureUpdate {
                sources: Vec::new(),
                model: Some(selection("second")),
            },
            "main",
        ),
        async { source.offer(vec![set("mcp:a", "v1", &["lookup"], &log, None)]) },
    );
    assert_eq!(configured.unwrap(), ConfigureOutcome::Adopted);
    adopted(&session, "main", &[("mcp:a", "v1")]).await;
    env.turn(&session, "main", &["lookup"]).await;
    let request = requests(&session).await.pop().unwrap();
    assert_eq!(request.model_id, "second");
    assert_eq!(
        request
            .capability_snapshot
            .as_ref()
            .unwrap()
            .capabilities
            .context_window,
        98765
    );
    assert_eq!(declared(&request), ["gate", "branch", "lookup"]);
    assert_eq!(last_result(&session, "main").await, "v1 ran lookup");
    env.agent.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tool_updates_wait_for_branch_activation_and_do_not_run_after_failed_assembly() {
    struct StartupGate {
        scope: BranchScope,
        gate: Arc<Gate>,
        ready:
            tokio::sync::mpsc::UnboundedSender<(Arc<Timeline>, Arc<ToolRegistry>, Arc<TestSource>)>,
        fail: bool,
    }
    impl TypedPlugin for StartupGate {
        type Deps = (
            BranchGate<ToolBinding>,
            InBranch<Timeline>,
            InSession<ToolRegistry>,
            InSession<TestSource>,
        );
        fn name(&self) -> &str {
            "ToolBindingStartupGate"
        }
        fn apply<'a>(
            &'a self,
            _ctx: &'a Ctx,
            (_, InBranch(timeline), InSession(registry), InSession(source)): Self::Deps,
        ) -> BoxFuture<'a, std::result::Result<Effect, CordisError>> {
            Box::pin(async move {
                self.ready.send((timeline, registry, source)).unwrap();
                self.gate.release.notified().await;
                if self.fail {
                    return Err(CordisError::PluginFailed("assembly rejected".into()));
                }
                Ok(Effect::Done)
            })
        }
    }
    impl Capability for StartupGate {
        fn keys(&self) -> KeysOf<Self> {
            scoped::<Self::Deps, _>(&self.scope)
        }
    }

    for fail in [false, true] {
        let (source, log) = (TestSource::new(), Arc::new(Log::default()));
        source.seed(vec![set("mcp:a", "v1", &["lookup"], &log, None)]);
        let model = Arc::new(ScriptedModel::default());
        let mut builder = AgentBuilder::new(AgentConfig::default());
        builder
            .session_value(Arc::new(HistoryRenderer::default()))
            .session_value(Arc::new(ModelHost(model.clone())))
            .session_value(Arc::new(PrepareContextHost(Arc::new(IdentityContext))))
            .session_value(Arc::new(ContextModelHost(None)))
            .session_value(Arc::new(CompactionStrategyHost(None)))
            .session_plugin(dim_agent_capabilities::adapters::DefaultPrepareModelPlugin::new);
        defaults::tool_host(&mut builder, Arc::new(GateTools(Arc::new(Gate::default()))));
        defaults::install_with_prompt(
            &mut builder,
            Arc::new(EmptyPromptHost),
            Some(ConfigureUpdate {
                sources: Vec::new(),
                model: Some(selection("scripted")),
            }),
        );
        let provider = source.clone();
        builder
            .session_tool_source("mcp")
            .session_plugin(move |scope| SourcePlugin {
                scope: *scope,
                source: provider.clone(),
                name: "mcp",
            });
        let gate = Arc::new(Gate::default());
        let hold = gate.clone();
        let (send, mut receive) = tokio::sync::mpsc::unbounded_channel();
        builder.timeline_plugin(move |scope| StartupGate {
            scope: *scope,
            gate: hold.clone(),
            ready: send.clone(),
            fail,
        });
        let env = Env {
            agent: builder.build(),
            model,
        };
        let agent = env.agent.clone();
        let creating = tokio::spawn(async move {
            agent
                .create_session(SessionConfig {
                    tools: vec!["gate".into()],
                    ..SessionConfig::default()
                })
                .await
        });
        let (timeline, registry, source) =
            tokio::time::timeout(Duration::from_secs(5), receive.recv())
                .await
                .unwrap()
                .unwrap();
        let before = timeline.sequence();
        let mut published = timeline.watch();
        let mut sources = registry.sources_state();
        source.offer(vec![set("mcp:a", "v2", &["lookup"], &log, None)]);
        tokio::time::timeout(
            Duration::from_secs(5),
            sources.wait_for(|state| state.0["mcp:a"].adopted.as_deref() == Some("v2")),
        )
        .await
        .unwrap()
        .unwrap();
        // The branch remains in assembly even though ToolBinding is available.
        let premature = tokio::time::timeout(Duration::from_millis(150), published.changed())
            .await
            .is_ok();
        gate.release.notify_one();
        let created = tokio::time::timeout(Duration::from_secs(5), creating)
            .await
            .unwrap()
            .unwrap();
        if fail {
            assert!(created.is_err());
            assert_eq!(
                timeline.sequence(),
                before,
                "failed assembly adopted an update"
            );
        } else {
            let session = created.unwrap();
            adopted(&session, "main", &[("mcp:a", "v2")]).await;
            env.turn(&session, "main", &["lookup"]).await;
            assert_eq!(last_result(&session, "main").await, "v2 ran lookup");
        }
        env.agent.close().await.unwrap();
        assert!(
            !premature,
            "tool update was adopted before branch activation"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn registered_sources_keep_their_names_apart_from_contributed_tools() {
    // `contributed` is an ordinary source name: the registry's own sets use
    // `#contributed`, which no registered name may contain.
    let (source, log) = (TestSource::new(), Arc::new(Log::default()));
    source.seed(vec![set("contributed:a", "v1", &["lookup"], &log, None)]);
    let env = env_named(source, None, "contributed");
    let session = env.session().await;
    adopted(&session, "main", &[("contributed:a", "v1")]).await;
    env.turn(&session, "main", &["lookup"]).await;
    assert_eq!(last_result(&session, "main").await, "v1 ran lookup");

    let invalid = env_named(TestSource::new(), None, "#contributed");
    let error = invalid
        .agent
        .create_session(SessionConfig {
            tools: vec!["gate".into()],
            ..SessionConfig::default()
        })
        .await
        .err()
        .expect("an invalid source name fails the Session")
        .to_string();
    assert!(error.contains("tool source name"), "{error}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn contributed_tools_follow_their_provider_until_the_session_closes() {
    /// Holds the branch's unload, so every Session capability stays loaded
    /// while the Session is already closing.
    struct CloseGate(Arc<Gate>);
    impl TypedPlugin for CloseGate {
        type Deps = ();
        fn name(&self) -> &str {
            "CloseGate"
        }
        fn apply<'a>(
            &'a self,
            ctx: &'a Ctx,
            (): (),
        ) -> BoxFuture<'a, std::result::Result<Effect, CordisError>> {
            Box::pin(async move {
                let gate = self.0.clone();
                ctx.effect(move || {
                    Effect::AsyncDisposer(Box::new(move || {
                        Box::pin(async move {
                            gate.entered.notify_one();
                            gate.release.notified().await;
                            Ok(())
                        })
                    }))
                })?;
                Ok(Effect::Done)
            })
        }
    }
    impl Capability for CloseGate {
        fn keys(&self) -> KeysOf<Self> {}
    }

    let root = Ctx::root().unwrap();
    let enabled = root.plugin(capability_plugin(EnableContributor));
    plugin_state(&enabled, rutis::FiberState::Active).await;
    let gate = Arc::new(Gate::default());
    let hold = gate.clone();
    let mut builder = AgentBuilder::new(AgentConfig::default());
    defaults::mock_hosts(&mut builder);
    defaults::install(&mut builder);
    builder
        .session_tool("optional")
        .session_plugin(|scope| OptionalContributor {
            scope: *scope,
            generations: Arc::default(),
            hold: Arc::default(),
            provided: Arc::default(),
        })
        .timeline_plugin(move |_| CloseGate(hold.clone()));
    let agent = builder.build_in(&root);
    let session = agent
        .create_session(SessionConfig {
            tools: vec!["optional".into(), "echo".into()],
            ..SessionConfig::default()
        })
        .await
        .unwrap();
    let provider = root
        .view(
            session
                .diagnostics()
                .unwrap()
                .plugins
                .iter()
                .find(|plugin| plugin.name == "OptionalContributor")
                .unwrap()
                .id,
        )
        .unwrap();
    /// The version of `optional` the registry offers now.
    fn offered(state: &ToolSourcesState) -> Option<String> {
        state
            .0
            .get("#contributed:optional")
            .and_then(|status| status.adopted.clone())
    }
    async fn follow(
        sources: &mut tokio::sync::watch::Receiver<ToolSourcesState>,
        expected: impl Fn(Option<String>) -> bool,
    ) -> Option<String> {
        let state = tokio::time::timeout(
            Duration::from_secs(5),
            sources.wait_for(|state| expected(offered(state))),
        )
        .await
        .expect("the registry followed its provider")
        .unwrap();
        offered(&state)
    }
    let mut sources = session
        .session_service::<ToolSourcesView>()
        .unwrap()
        .subscribe();
    let first = follow(&mut sources, |version| version.is_some()).await;

    // While the Session is open, the registry follows the provider without
    // waiting for a request: retirement withdraws the tool, and the next
    // generation is offered under a new version.
    enabled.dispose().await.unwrap();
    plugin_state(&provider, rutis::FiberState::Pending).await;
    follow(&mut sources, |version| version.is_none()).await;
    let enabled = root.plugin(capability_plugin(EnableContributor));
    plugin_state(&provider, rutis::FiberState::Active).await;
    let second = follow(&mut sources, |version| version.is_some()).await;
    assert_ne!(second, first);

    let closing = tokio::spawn(session.close());
    tokio::time::timeout(Duration::from_secs(5), gate.entered.notified())
        .await
        .unwrap();
    // The provider retires after the close started: the registry's
    // background refresh has ended, so nothing rereads or withdraws its tool.
    enabled.dispose().await.unwrap();
    plugin_state(&provider, rutis::FiberState::Pending).await;
    let refreshed = tokio::time::timeout(Duration::from_millis(150), sources.changed())
        .await
        .is_ok();
    gate.release.notify_one();
    tokio::time::timeout(Duration::from_secs(5), closing)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(!refreshed, "a closing Session refreshed its tools");
    assert_eq!(offered(&sources.borrow()), second);
    agent.close().await.unwrap();
    root.shutdown().await.unwrap();
}

/// The version of `key` in the newest tool part `branch` adopted.
async fn adopted_version(session: &Session, branch: &str, key: &str) -> Option<String> {
    session
        .history(branch, None)
        .await
        .unwrap()
        .iter()
        .rev()
        .find_map(|event| match &event.kind {
            EventKind::ContextConfigured(data) => Some(
                data.settings
                    .tool_sources
                    .iter()
                    .find(|source| source.key == key)
                    .map(|source| source.version.clone()),
            ),
            _ => None,
        })
        .flatten()
}

/// Hands out the Session's registry, to resolve versions as `bind` does.
struct RegistryProbe {
    scope: SessionScope,
    slot: Arc<Mutex<Option<Arc<ToolRegistry>>>>,
}
impl TypedPlugin for RegistryProbe {
    type Deps = (InSession<ToolRegistry>,);
    fn name(&self) -> &str {
        "RegistryProbe"
    }
    fn apply<'a>(
        &'a self,
        _: &'a Ctx,
        (InSession(registry),): Self::Deps,
    ) -> BoxFuture<'a, std::result::Result<Effect, CordisError>> {
        Box::pin(async move {
            *self.slot.lock().unwrap() = Some(registry);
            Ok(Effect::Done)
        })
    }
}
impl Capability for RegistryProbe {
    fn keys(&self) -> KeysOf<Self> {
        scoped::<Self::Deps, _>(&self.scope)
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_persisted_contributed_version_names_one_generation_across_restores() {
    let root = Ctx::root().unwrap();
    let enabled = root.plugin(capability_plugin(EnableContributor));
    plugin_state(&enabled, rutis::FiberState::Active).await;
    let store = Arc::new(MemoryTimelineStore::default());
    let generations = Arc::new(AtomicUsize::new(0));
    let log = Arc::new(Log::default());
    let registry: Arc<Mutex<Option<Arc<ToolRegistry>>>> = Arc::default();
    // Each build is a new process: a new registry and provider generation,
    // with a dynamic source whose own version stays the same, so the
    // configuration keeps a tool part that records contributed versions.
    let build = || {
        let source = TestSource::new();
        source.seed(vec![set("mcp:a", "v1", &["lookup"], &log, None)]);
        let mut builder = AgentBuilder::new(AgentConfig::default());
        defaults::mock_hosts(&mut builder);
        defaults::install_with_prompt(
            &mut builder,
            Arc::new(EmptyPromptHost),
            Some(ConfigureUpdate {
                sources: Vec::new(),
                model: Some(selection("scripted")),
            }),
        );
        let generations = generations.clone();
        builder
            .timeline_store(store.clone())
            .session_tool("optional")
            .session_plugin(move |scope| OptionalContributor {
                scope: *scope,
                generations: generations.clone(),
                hold: Arc::default(),
                provided: Arc::default(),
            })
            .session_tool_source("mcp")
            .session_plugin(move |scope| SourcePlugin {
                scope: *scope,
                source: source.clone(),
                name: "mcp",
            })
            .session_plugin({
                let slot = registry.clone();
                move |scope| RegistryProbe {
                    scope: *scope,
                    slot: slot.clone(),
                }
            });
        builder.build_in(&root)
    };
    let config = SessionConfig {
        tools: vec!["optional".into(), "echo".into()],
        permission_policy: PermissionPolicy {
            allowed_tools: vec!["echo".into(), "optional".into()],
            ..PermissionPolicy::default()
        },
        ..SessionConfig::default()
    };
    let agent = build();
    let session = agent.create_session(config).await.unwrap();
    session
        .submit("tool:optional {}".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    session.idle("main").await.unwrap();
    assert_eq!(last_result(&session, "main").await, "generation 1");
    let before = adopted_version(&session, "main", "#contributed:optional")
        .await
        .expect("the tool part records the contributed version");
    let id = session.id().to_owned();
    agent.close().await.unwrap();

    let agent = build();
    let restored = agent.restore_session(id).await.unwrap();
    restored
        .submit("tool:optional {}".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    restored.idle("main").await.unwrap();
    assert_eq!(last_result(&restored, "main").await, "generation 2");
    // The restored branch adopted the new generation under a version the
    // first process never used, so the persisted one still names generation 1.
    let after = adopted_version(&restored, "main", "#contributed:optional")
        .await
        .unwrap();
    assert_ne!(after, before);
    // `bind` resolves a prefix's versions through `source`: the persisted one
    // resolves to nothing in the restored process, never to generation 2.
    let registry = registry.lock().unwrap().clone().unwrap();
    assert!(registry.source("#contributed:optional", &before).is_none());
    let current = registry.source("#contributed:optional", &after).unwrap();
    assert_eq!(current.tools[0].definition.name, "optional");
    assert_eq!(
        adopted_version(&restored, "main", "mcp:a").await.as_deref(),
        Some("v1")
    );
    agent.close().await.unwrap();
    enabled.dispose().await.unwrap();
    root.shutdown().await.unwrap();
}

/// The declarations the foreign doubles answered: their generation and
/// whether the test was serving a request then.
#[derive(Default)]
struct Declarations {
    serving: std::sync::atomic::AtomicBool,
    calls: Mutex<Vec<(&'static str, bool)>>,
}
impl Declarations {
    fn of(&self, generation: &str) -> Vec<bool> {
        let calls = self.calls.lock().unwrap();
        calls
            .iter()
            .filter(|(name, _)| *name == generation)
            .map(|(_, serving)| *serving)
            .collect()
    }
}
/// A foreign row's tool as ToolRegistry sees it. Outside a request its
/// declaration does not answer (up to 5 s), like a runtime that is stopping:
/// a background reader that called it would be held there.
struct ForeignTool(&'static str, Arc<Declarations>);
impl rutis_bridge::session::HostDispatch for ForeignTool {
    fn invoke(
        &self,
        method: &str,
        _: rutis_bridge::session::Value,
    ) -> rutis_bridge::session::Reply {
        let value = match method {
            "declaration" => {
                let serving = self.1.serving.load(Ordering::SeqCst);
                self.1.calls.lock().unwrap().push((self.0, serving));
                let deadline = std::time::Instant::now() + Duration::from_secs(5);
                while !self.1.serving.load(Ordering::SeqCst) && std::time::Instant::now() < deadline
                {
                    std::thread::sleep(Duration::from_millis(5));
                }
                let mut tool = definition(
                    "foreign",
                    "Foreign tool",
                    serde_json::json!({"type":"object"}),
                );
                tool.risk = Some("session".into());
                serde_json::to_value(tool).unwrap()
            }
            _ => serde_json::to_value(ToolResult::text(format!("{} ran", self.0))).unwrap(),
        };
        Ok(value.into())
    }
}
type ForeignSlot = Arc<
    Mutex<
        Option<(
            Ctx,
            rutis::ServiceWriter<dyn rutis_bridge::session::HostDispatch>,
        )>,
    >,
>;
/// Provides generation A under the row's tool name with its generation token,
/// and hands the test a writer that replaces it in place.
struct ForeignContributor {
    scope: SessionScope,
    declarations: Arc<Declarations>,
    slot: ForeignSlot,
}
impl TypedPlugin for ForeignContributor {
    type Deps = ();
    fn name(&self) -> &str {
        "ForeignContributor"
    }
    fn apply<'a>(
        &'a self,
        ctx: &'a Ctx,
        (): (),
    ) -> BoxFuture<'a, std::result::Result<Effect, CordisError>> {
        Box::pin(async move {
            let instance = self.scope.instance;
            ctx.provide_as(
                rutis::TypeKey::keyed_dynamic::<CancellationToken>("toolContribution:foreign")
                    .with_instance(instance),
                Arc::new(ctx.cancellation_token()),
            )?;
            let (_, writer) = ctx.provide_mut_as::<dyn rutis_bridge::session::HostDispatch>(
                rutis_bridge::session::host_key_in("toolContribution:foreign", instance),
                Arc::new(ForeignTool("A", self.declarations.clone())),
            )?;
            *self.slot.lock().unwrap() = Some((ctx.clone(), writer));
            Ok(Effect::Done)
        })
    }
}
impl Capability for ForeignContributor {
    fn keys(&self) -> KeysOf<Self> {}
}
/// A root plugin whose activation wakes every registry's background refresh.
struct Wake;
impl TypedPlugin for Wake {
    type Deps = ();
    fn name(&self) -> &str {
        "Wake"
    }
    fn apply<'a>(
        &'a self,
        _: &'a Ctx,
        (): (),
    ) -> BoxFuture<'a, std::result::Result<Effect, CordisError>> {
        Box::pin(async { Ok(Effect::Done) })
    }
}
impl Capability for Wake {
    fn keys(&self) -> KeysOf<Self> {}
}

/// Waits until the newest tool part of `main` meets `expected` for the
/// contributed `foreign` set's version.
async fn foreign_adopted(session: &Session, expected: impl Fn(Option<&str>) -> bool) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if expected(
                adopted_version(session, "main", "#contributed:foreign")
                    .await
                    .as_deref(),
            ) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the tool part was adopted");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_replaced_foreign_provider_is_read_only_by_a_request() {
    let root = Ctx::root().unwrap();
    let declarations = Arc::new(Declarations::default());
    let slot: ForeignSlot = Arc::default();
    let (source, log) = (TestSource::new(), Arc::new(Log::default()));
    // A dynamic source gives the branch a tool part, so its requests bind
    // the adopted versions (`bind`) and adoption runs in the background.
    source.seed(vec![set("mcp:a", "v1", &["lookup"], &log, None)]);
    let mut builder = AgentBuilder::new(AgentConfig::default());
    defaults::mock_hosts(&mut builder);
    defaults::install_with_prompt(
        &mut builder,
        Arc::new(EmptyPromptHost),
        Some(ConfigureUpdate {
            sources: Vec::new(),
            model: Some(selection("scripted")),
        }),
    );
    builder
        .session_tool("foreign")
        .session_plugin({
            let (declarations, slot) = (declarations.clone(), slot.clone());
            move |scope| ForeignContributor {
                scope: *scope,
                declarations: declarations.clone(),
                slot: slot.clone(),
            }
        })
        .session_tool_source("mcp")
        .session_plugin(move |scope| SourcePlugin {
            scope: *scope,
            source: source.clone(),
            name: "mcp",
        });
    let agent = builder.build_in(&root);
    // Branch initialization serves the first request and reads A.
    declarations.serving.store(true, Ordering::SeqCst);
    let session = agent
        .create_session(SessionConfig {
            tools: vec!["foreign".into(), "echo".into()],
            permission_policy: PermissionPolicy {
                allowed_tools: vec!["echo".into(), "foreign".into()],
                ..PermissionPolicy::default()
            },
            ..SessionConfig::default()
        })
        .await
        .unwrap();
    foreign_adopted(&session, |version| version.is_some()).await;
    assert_eq!(declarations.of("A"), [true]);

    // Replace A by B in place, with no absent provider between them, and
    // wake the background refresh.
    declarations.serving.store(false, Ordering::SeqCst);
    let (ctx, writer) = slot.lock().unwrap().take().unwrap();
    writer
        .set(&ctx, Arc::new(ForeignTool("B", declarations.clone())))
        .unwrap();
    let wake = root.plugin(capability_plugin(Wake));
    plugin_state(&wake, rutis::FiberState::Active).await;
    // The registry withdrew A and the branch adopted the withdrawal in the
    // background; neither read B's declaration.
    foreign_adopted(&session, |version| version.is_none()).await;
    assert!(
        declarations.of("B").is_empty(),
        "a background reader called B"
    );

    // The next request attaches B, so the request after it declares B.
    declarations.serving.store(true, Ordering::SeqCst);
    session
        .submit(
            "tool:echo {\"text\":\"next\"}".into(),
            InputMode::Queue,
            "main",
        )
        .await
        .unwrap();
    session.idle("main").await.unwrap();
    foreign_adopted(&session, |version| version.is_some()).await;
    session
        .submit("tool:foreign {}".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    session.idle("main").await.unwrap();
    assert_eq!(last_result(&session, "main").await, "B ran");
    assert_eq!(declarations.of("B"), [true]);
    tokio::time::timeout(Duration::from_secs(10), agent.close())
        .await
        .unwrap()
        .unwrap();
    wake.dispose().await.unwrap();
    root.shutdown().await.unwrap();
}

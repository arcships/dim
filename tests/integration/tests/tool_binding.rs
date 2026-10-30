//! ToolBinding with a dynamic tool source (`tool-contributions.md` §7.7):
//! adoption, request snapshots, version pinning and release.

mod local_support;

use dim_agent_capabilities::{
    defaults,
    history::HistoryRenderer,
    host::*,
    tool_binding::ToolBinding,
    tool_registry::{
        ToolContribution, ToolHandler, ToolProvider, ToolProviderChanged, ToolRegistry,
        ToolSourceSet, ToolSourcesState, ToolSourcesView, definition, tool_provider_changes,
    },
};
use dim_agent_runtime::*;
use dim_agent_types::*;
use local_support::*;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, ErrorKind, InstanceId, Key, TypeKey};
use rutis::{Ctx, Plugin};
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
    emitter: Option<(Ctx, InstanceId)>,
}
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
        let (ctx, session) = self
            .emitter
            .as_ref()
            .expect("offer through the Session's TestSource service");
        *self.sets.lock().unwrap() = sets;
        ctx.emit(ToolProviderChanged {
            name: tool_provider_changes(*session),
        })
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
    session: InstanceId,
    name: &'static str,
    key: Key<TestSource>,
    tools: Key<ToolRegistry>,
    source: Arc<TestSource>,
}
/// Registers `source` as the Session tool source `name` of every Session.
fn add_source(builder: &mut AgentBuilder, source: Arc<TestSource>, name: &'static str) {
    names::register::<TestSource>(builder.catalog_mut(), Some(SESSION_GROUP));
    builder.require_feature(
        Feature::new("TestSource").session("TestSource", move |names| {
            Ok(Box::new(SourcePlugin {
                session: names.instance(SESSION_GROUP)?,
                name,
                key: names.key()?,
                tools: names.key()?,
                source: source.clone(),
            }))
        }),
    );
}
impl DynPlugin for SourcePlugin {
    fn name(&self) -> &str {
        "TestSource"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.tools.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let tools = ctx.require_at(&self.tools)?;
            let source = Arc::new(TestSource {
                sets: Mutex::new(std::mem::take(&mut *self.source.sets.lock().unwrap())),
                emitter: Some((ctx.clone(), self.session)),
            });
            ctx.provide_at(self.key.clone(), source.clone())?;
            tools.register_source(ctx, self.name, source)?;
            Ok(Effect::Done)
        })
    }
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
        .process_value(Arc::new(ModelHost(model.clone())))
        .session_value(Arc::new(PrepareContextHost(Arc::new(IdentityContext))))
        .session_value(Arc::new(ContextModelHost(None)))
        .session_value(Arc::new(CompactionStrategyHost(None)))
        .require_feature(
            Feature::new("PrepareModelHost").session("PrepareModelHost", |names| {
                Ok(Box::new(
                    dim_agent_capabilities::adapters::DefaultPrepareModelPlugin::new(names)?,
                ))
            }),
        );
    defaults::tool_host(&mut builder, Arc::new(GateTools(Arc::new(Gate::default()))));
    defaults::install_with_prompt(
        &mut builder,
        Arc::new(EmptyPromptHost),
        Some(ConfigureUpdate {
            sources: Vec::new(),
            model: Some(selection("scripted")),
        }),
    );
    add_source(&mut builder, source, name);
    Env {
        agent: builder.build(),
        model,
    }
}

impl Env {
    async fn session(&self) -> Session {
        self.agent
            .create_session(SessionConfig {
                tools: Some(vec!["gate".into()]),
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
            let sources = current.map(|sources| {
                sources
                    .into_iter()
                    .filter(|source| !source.key.starts_with("#contributed:"))
                    .collect::<Vec<_>>()
            });
            if sources.as_ref() == Some(&expected) {
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
impl Plugin for EnableContributor {
    type Deps = ();
    fn name(&self) -> &str {
        "EnableContributor"
    }
    async fn apply(&self, ctx: &Ctx, (): ()) -> rutis::Result {
        ctx.provide(ContributorEnabled)?;
        Ok(())
    }
}
struct OptionalContributor {
    generations: Arc<AtomicUsize>,
    hold: Arc<Gate>,
    /// Every generation's handler, to observe its release.
    provided: Arc<Mutex<Vec<std::sync::Weak<OptionalHandler>>>>,
    enabled: Key<ContributorEnabled>,
    tools: Key<ToolRegistry>,
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
impl DynPlugin for OptionalContributor {
    fn name(&self) -> &str {
        "OptionalContributor"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.enabled.erase(), self.tools.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let _enabled = ctx.require_at(&self.enabled)?;
            let tools = ctx.require_at(&self.tools)?;
            let generation = self.generations.fetch_add(1, Ordering::SeqCst) + 1;
            let handler = Arc::new(OptionalHandler {
                generation,
                stopping: ctx.cancellation_token(),
                hold: self.hold.clone(),
            });
            self.provided.lock().unwrap().push(Arc::downgrade(&handler));
            tools.register(
                ctx,
                ToolContribution {
                    definition: definition(
                        "optional",
                        "Report provider generation",
                        serde_json::json!({"type":"object"}),
                    ),
                    handler,
                },
            )?;
            Ok(Effect::Done)
        })
    }
}
/// Adds the `optional` tool, whose contributor runs while the process
/// provides `ContributorEnabled`.
fn add_optional(
    builder: &mut AgentBuilder,
    generations: Arc<AtomicUsize>,
    hold: Arc<Gate>,
    provided: Arc<Mutex<Vec<std::sync::Weak<OptionalHandler>>>>,
) {
    names::register::<ContributorEnabled>(builder.catalog_mut(), None);
    builder.require_feature(Feature::new("OptionalContributor").session(
        "OptionalContributor",
        move |names| {
            Ok(Box::new(OptionalContributor {
                generations: generations.clone(),
                hold: hold.clone(),
                provided: provided.clone(),
                enabled: names.key()?,
                tools: names.key()?,
            }))
        },
    ));
}

async fn plugin_state(view: &rutis::host::FiberView, state: rutis::host::FiberState) {
    tokio::time::timeout(Duration::from_secs(5), async {
        view.watch()
            .wait_for(|snapshot| snapshot.state == state)
            .await
            .unwrap();
    })
    .await
    .unwrap();
}

fn unconfigured_builder(prepare: Arc<dyn PrepareModel>) -> AgentBuilder {
    let mut builder = AgentBuilder::new(AgentConfig::default());
    defaults::tool_host(
        &mut builder,
        Arc::new(dim_agent_capabilities::adapters::MockTools),
    );
    builder
        .session_value(Arc::new(HistoryRenderer::default()))
        .process_value(Arc::new(ModelHost(Arc::new(
            dim_agent_capabilities::adapters::EchoModel,
        ))))
        .session_value(Arc::new(PrepareContextHost(Arc::new(IdentityContext))))
        .session_value(Arc::new(ContextModelHost(None)))
        .session_value(Arc::new(CompactionStrategyHost(None)))
        .session_value(Arc::new(PrepareModelHost(prepare)));
    defaults::install(&mut builder);
    builder
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_preparing_request_keeps_its_contributor_generation() {
    struct PausedPrepare {
        config: Arc<SessionConfig>,
        ready: Arc<tokio::sync::Notify>,
        resume: Mutex<Option<std::sync::mpsc::Receiver<()>>>,
    }
    impl PrepareModel for PausedPrepare {
        fn prepare(
            &self,
            turn_id: &str,
            request_id: &str,
            history: &[Message],
            available_tools: &[ToolDefinition],
        ) -> std::result::Result<ModelSettings, HostError> {
            let settings = dim_agent_capabilities::adapters::DefaultPrepareModel {
                config: self.config.clone(),
            }
            .prepare(turn_id, request_id, history, available_tools)?;
            if let Some(resume) = self.resume.lock().unwrap().take() {
                self.ready.notify_one();
                resume
                    .recv_timeout(Duration::from_secs(10))
                    .expect("release prepared request");
            }
            Ok(settings)
        }
    }
    for replace in [false, true] {
        let dirs = Dirs::new();
        let root = Ctx::root().unwrap();
        let enabled = root.plugin(EnableContributor);
        plugin_state(&enabled, rutis::host::FiberState::Active).await;
        let config = SessionConfig {
            cwd: dirs.root.to_string_lossy().into_owned(),
            tools: Some(vec!["optional".into(), "echo".into()]),
            permission_policy: PermissionPolicy {
                allowed_tools: vec!["optional".into(), "echo".into()],
                ..PermissionPolicy::default()
            },
            ..SessionConfig::default()
        };
        let ready = Arc::new(tokio::sync::Notify::new());
        let (resume, receiver) = std::sync::mpsc::channel();
        let mut builder = unconfigured_builder(Arc::new(PausedPrepare {
            config: Arc::new(config.clone()),
            ready: ready.clone(),
            resume: Mutex::new(Some(receiver)),
        }));
        let provided = Arc::new(Mutex::new(Vec::new()));
        add_optional(
            &mut builder,
            Arc::default(),
            Arc::default(),
            provided.clone(),
        );
        let agent = builder.build_in(&root);
        let session = agent.create_session(config).await.unwrap();
        session
            .submit("tool:optional {}".into(), InputMode::Queue, "main")
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), ready.notified())
            .await
            .expect("preparation read its declaration");
        enabled.dispose().await.unwrap();
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
        plugin_state(&provider, rutis::host::FiberState::Pending).await;
        assert!(
            provided.lock().unwrap()[0].upgrade().is_some(),
            "preparation keeps its original source alive"
        );
        if replace {
            root.plugin(EnableContributor);
            plugin_state(&provider, rutis::host::FiberState::Active).await;
        }
        resume.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(5), session.idle("main"))
            .await
            .unwrap()
            .unwrap();
        let sent = requests(&session).await;
        assert_eq!(declared(&sent[0]), ["optional", "echo"]);
        assert!(
            last_result(&session, "main")
                .await
                .contains("provider unloaded"),
            "the pinned retired handler must run instead of the replacement"
        );
        assert!(
            provided.lock().unwrap()[0].upgrade().is_none(),
            "the settled request releases the retired generation"
        );
        let next = if replace {
            "tool:optional {}"
        } else {
            "tool:echo {\"text\":\"host still works\"}"
        };
        session
            .submit(next.into(), InputMode::Queue, "main")
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), session.idle("main"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            last_result(&session, "main").await,
            if replace {
                "generation 2"
            } else {
                "host still works"
            }
        );
        agent.close().await.unwrap();
        root.shutdown().await.unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn custom_preparation_keeps_none_empty_and_filtered_declarations() {
    struct Selected(Option<Vec<ToolDefinition>>);
    impl PrepareModel for Selected {
        fn prepare(
            &self,
            _: &str,
            _: &str,
            _: &[Message],
            available_tools: &[ToolDefinition],
        ) -> std::result::Result<ModelSettings, HostError> {
            assert!(available_tools.iter().any(|tool| tool.name == "optional"));
            let mut settings = ModelSettings::new("mock", "echo");
            settings.tools = self.0.clone();
            Ok(settings)
        }
    }
    let echo = dim_agent_capabilities::adapters::MockTools
        .definitions()
        .into_iter()
        .find(|tool| tool.name == "echo")
        .unwrap();
    let unavailable = definition(
        "unavailable",
        "Unavailable tool",
        serde_json::json!({"type":"object"}),
    );
    for (selected, valid) in [
        (None, true),
        (Some(vec![]), true),
        (Some(vec![echo]), true),
        (Some(vec![unavailable]), false),
    ] {
        let dirs = Dirs::new();
        let root = Ctx::root().unwrap();
        let enabled = root.plugin(EnableContributor);
        plugin_state(&enabled, rutis::host::FiberState::Active).await;
        let mut builder = unconfigured_builder(Arc::new(Selected(selected.clone())));
        add_optional(&mut builder, Arc::default(), Arc::default(), Arc::default());
        let agent = builder.build_in(&root);
        let session = agent
            .create_session(SessionConfig {
                cwd: dirs.root.to_string_lossy().into_owned(),
                tools: Some(vec!["optional".into(), "echo".into()]),
                permission_policy: PermissionPolicy {
                    allowed_tools: vec!["echo".into()],
                    ..PermissionPolicy::default()
                },
                ..SessionConfig::default()
            })
            .await
            .unwrap();
        let calls_echo = valid && selected.as_ref().is_some_and(|tools| !tools.is_empty());
        session
            .submit(
                if calls_echo {
                    "tool:echo {\"text\":\"selected\"}"
                } else {
                    "hello"
                }
                .into(),
                InputMode::Queue,
                "main",
            )
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), session.idle("main"))
            .await
            .unwrap()
            .unwrap();
        let sent = requests(&session).await;
        if valid {
            assert!(!sent.is_empty());
            assert_eq!(sent[0].tools.as_deref(), selected.as_ref());
            if calls_echo {
                assert_eq!(last_result(&session, "main").await, "selected");
            }
        } else {
            assert!(sent.is_empty());
            assert!(session.history("main", None).await.unwrap().iter().any(|event| matches!(&event.kind, EventKind::TurnEnd(data) if data.outcome == TurnOutcome::Failed { error: "invalid input: tool unavailable has no handler".into() })));
        }
        agent.close().await.unwrap();
        root.shutdown().await.unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn optional_contributors_do_not_gate_the_loop_and_follow_new_generations() {
    let root = Ctx::root().unwrap();
    let enabled = root.plugin(EnableContributor);
    plugin_state(&enabled, rutis::host::FiberState::Active).await;
    let generations = Arc::new(AtomicUsize::new(0));
    let hold = Arc::new(Gate::default());
    let held = hold.clone();
    let provided: Arc<Mutex<Vec<std::sync::Weak<OptionalHandler>>>> = Arc::default();
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
    add_optional(&mut builder, generations, held, provided.clone());
    let agent = builder.build_in(&root);
    let session = agent
        .create_session(SessionConfig {
            tools: Some(vec!["optional".into(), "absent".into(), "echo".into()]),
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
    assert_eq!(declared(&requests(&session).await[0]), ["optional", "echo"]);
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
    plugin_state(&provider, rutis::host::FiberState::Pending).await;
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
    assert_eq!(registry.state, rutis::host::FiberState::Active);
    assert_eq!(registry.generation, registry_generation);

    // The provider's next generation is offered without a Session reload
    // (#241); this configuration has no tool part, so the next request
    // declares it with no adoption fact.
    let reloaded = session.sequence("main").unwrap();
    let enabled = root.plugin(EnableContributor);
    plugin_state(&enabled, rutis::host::FiberState::Active).await;
    plugin_state(&provider, rutis::host::FiberState::Active).await;
    session
        .submit("tool:optional {}".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    session.idle("main").await.unwrap();
    assert_eq!(last_result(&session, "main").await, "generation 2");
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
    let previous_provider_generation = provider.watch().borrow().generation;
    root.view(registry_id).unwrap().restart().await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop_view
            .watch()
            .wait_for(|snapshot| {
                snapshot.generation > previous_loop.generation
                    && snapshot.state == rutis::host::FiberState::Active
            })
            .await
            .unwrap();
        provider
            .watch()
            .wait_for(|snapshot| {
                snapshot.generation > previous_provider_generation
                    && snapshot.state == rutis::host::FiberState::Active
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
    assert_eq!(last_result(&session, "main").await, "generation 3");
    agent.close().await.unwrap();
    root.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn contributors_are_current_for_the_first_request_without_initial_configuration() {
    let root = Ctx::root().unwrap();
    let enabled = root.plugin(EnableContributor);
    plugin_state(&enabled, rutis::host::FiberState::Active).await;
    let store = Arc::new(MemoryTimelineStore::default());
    let generations = Arc::new(AtomicUsize::new(0));
    let hold = Arc::new(Gate::default());
    let build = || {
        let mut builder = AgentBuilder::new(AgentConfig::default());
        defaults::mock_hosts(&mut builder);
        defaults::install(&mut builder);
        builder.timeline_store(store.clone());
        add_optional(
            &mut builder,
            generations.clone(),
            hold.clone(),
            Arc::default(),
        );
        builder.build_in(&root)
    };
    let agent = build();
    let session = agent
        .create_session(SessionConfig {
            tools: Some(vec!["optional".into(), "echo".into()]),
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
    plugin_state(&provider, rutis::host::FiberState::Pending).await;
    let enabled = root.plugin(EnableContributor);
    plugin_state(&provider, rutis::host::FiberState::Active).await;
    session
        .submit("tool:optional {}".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    session.idle("main").await.unwrap();
    assert_eq!(declared(&requests(&session).await[0]), ["optional", "echo"]);
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
        ["optional", "echo"]
    );
    assert_eq!(last_result(&restored, "main").await, "generation 3");
    agent.close().await.unwrap();
    enabled.dispose().await.unwrap();
    root.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_accepted_source_change_is_adopted_after_its_emitter_closes() {
    struct Emitter(tokio::sync::mpsc::UnboundedSender<Ctx>);
    impl rutis::host::DynPlugin for Emitter {
        fn name(&self) -> &str {
            "SourceEmitter"
        }
        fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
            Box::pin(async move {
                self.0.send(ctx.clone()).unwrap();
                Ok(Effect::Done)
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
    let (ctx, session_instance) = source.emitter.as_ref().unwrap();
    let key = tool_provider_changes(*session_instance);

    let (send, mut receive) = tokio::sync::mpsc::unbounded_channel();
    let emitter = ctx.plugin(Emitter(send));
    (&emitter).await.unwrap();
    let emitting = receive.recv().await.unwrap();

    let gate = Arc::new(Gate::default());
    let pause = gate.clone();
    ctx.listen::<ToolProviderChanged>()
        .name(key.clone())
        .once()
        .on(move |_, _| {
            let gate = pause.clone();
            async move {
                gate.entered.notify_one();
                gate.release.notified().await;
                Ok(None)
            }
        })
        .unwrap();
    ctx.emit(ToolProviderChanged { name: key.clone() }).unwrap();
    tokio::time::timeout(Duration::from_secs(5), gate.entered.notified())
        .await
        .unwrap();
    *source.sets.lock().unwrap() = vec![set("mcp:a", "v2", &["lookup"], &log, None)];
    emitting
        .emit(ToolProviderChanged { name: key.clone() })
        .unwrap();
    let disposal = emitter.shutdown();
    gate.release.notify_one();
    tokio::time::timeout(Duration::from_secs(5), disposal)
        .await
        .unwrap()
        .unwrap();

    adopted(&session, "main", &[("mcp:a", "v2")]).await;
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
    assert_eq!(declared(&requests(&session).await[0]), ["gate", "lookup"]);

    env.turn(&session, "main", &["lookup"]).await;
    assert_eq!(last_result(&session, "main").await, "v2 ran lookup");
    assert!(log.dropped.lock().unwrap().contains(&"v1".to_owned()));
    assert!(!log.dropped.lock().unwrap().contains(&"v2".to_owned()));

    source.offer(Vec::new());
    adopted(&session, "main", &[]).await;
    let calls = log.calls.lock().unwrap().len();
    env.turn(&session, "main", &[]).await;
    assert_eq!(log.calls.lock().unwrap().len(), calls);
    assert_eq!(declared(requests(&session).await.last().unwrap()), ["gate"]);
    env.agent.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn conflicting_sets_are_rejected_whole_and_recover_once_the_name_is_free() {
    let (source, log) = (TestSource::new(), Arc::new(Log::default()));
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
    assert_eq!(declared(&requests(&session).await[0]), ["gate", "find"]);

    source.offer(vec![set("mcp:b", "b1", &["find", "other"], &log, None)]);
    adopted(&session, "main", &[("mcp:b", "b1")]).await;
    env.turn(&session, "main", &["other"]).await;
    assert_eq!(last_result(&session, "main").await, "b1 ran other");
    assert_eq!(
        declared(requests(&session).await.last().unwrap()),
        ["gate", "find", "other"]
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
async fn a_session_without_a_mount_list_declares_every_registered_tool() {
    // BranchTool registers `branch`. A Session without a mount list declares
    // it with the host tools; one whose list does not name it does not (#364).
    for (tools, expected) in [
        (None, vec!["gate", "branch", "lookup"]),
        (Some(vec!["gate".to_owned()]), vec!["gate", "lookup"]),
    ] {
        let (source, log) = (TestSource::new(), Arc::new(Log::default()));
        source.seed(vec![set("mcp:a", "v1", &["lookup"], &log, None)]);
        let env = env(source, None);
        let session = env
            .agent
            .create_session(SessionConfig {
                tools,
                ..SessionConfig::default()
            })
            .await
            .unwrap();
        adopted(&session, "main", &[("mcp:a", "v1")]).await;
        env.turn(&session, "main", &[]).await;
        assert_eq!(declared(&requests(&session).await[0]), expected);
        env.agent.close().await.unwrap();
    }
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
    assert_eq!(declared(&request), ["gate", "lookup"]);
    assert_eq!(last_result(&session, "main").await, "v1 ran lookup");
    env.agent.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tool_updates_wait_for_branch_activation_and_do_not_run_after_failed_assembly() {
    struct StartupGate {
        gate: Arc<Gate>,
        ready:
            tokio::sync::mpsc::UnboundedSender<(Arc<Timeline>, Arc<ToolRegistry>, Arc<TestSource>)>,
        fail: bool,
        binding: Key<ToolBinding>,
        timeline: Key<Timeline>,
        registry: Key<ToolRegistry>,
        source: Key<TestSource>,
    }
    impl DynPlugin for StartupGate {
        fn name(&self) -> &str {
            "ToolBindingStartupGate"
        }
        fn injects(&self) -> Vec<TypeKey> {
            vec![
                self.binding.erase(),
                self.timeline.erase(),
                self.registry.erase(),
                self.source.erase(),
            ]
        }
        fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
            Box::pin(async move {
                let timeline = ctx.require_at(&self.timeline)?;
                let registry = ctx.require_at(&self.registry)?;
                let source = ctx.require_at(&self.source)?;
                self.ready.send((timeline, registry, source)).unwrap();
                self.gate.release.notified().await;
                if self.fail {
                    return Err(rutis::Error::from(ErrorKind::PluginFailed(
                        "assembly rejected".into(),
                    )));
                }
                Ok(Effect::Done)
            })
        }
    }

    for fail in [false, true] {
        let (source, log) = (TestSource::new(), Arc::new(Log::default()));
        source.seed(vec![set("mcp:a", "v1", &["lookup"], &log, None)]);
        let model = Arc::new(ScriptedModel::default());
        let mut builder = AgentBuilder::new(AgentConfig::default());
        builder
            .session_value(Arc::new(HistoryRenderer::default()))
            .process_value(Arc::new(ModelHost(model.clone())))
            .session_value(Arc::new(PrepareContextHost(Arc::new(IdentityContext))))
            .session_value(Arc::new(ContextModelHost(None)))
            .session_value(Arc::new(CompactionStrategyHost(None)))
            .require_feature(Feature::new("PrepareModelHost").session(
                "PrepareModelHost",
                |names| {
                    Ok(Box::new(
                        dim_agent_capabilities::adapters::DefaultPrepareModelPlugin::new(names)?,
                    ))
                },
            ));
        defaults::tool_host(&mut builder, Arc::new(GateTools(Arc::new(Gate::default()))));
        defaults::install_with_prompt(
            &mut builder,
            Arc::new(EmptyPromptHost),
            Some(ConfigureUpdate {
                sources: Vec::new(),
                model: Some(selection("scripted")),
            }),
        );
        add_source(&mut builder, source.clone(), "mcp");
        let gate = Arc::new(Gate::default());
        let hold = gate.clone();
        let (send, mut receive) = tokio::sync::mpsc::unbounded_channel();
        builder.require_feature(Feature::new("ToolBindingStartupGate").branch(
            "ToolBindingStartupGate",
            move |names| {
                Ok(Box::new(StartupGate {
                    gate: hold.clone(),
                    ready: send.clone(),
                    fail,
                    binding: names.key()?,
                    timeline: names.key()?,
                    registry: names.key()?,
                    source: names.key()?,
                }))
            },
        ));
        let env = Env {
            agent: builder.build(),
            model,
        };
        let agent = env.agent.clone();
        let creating = tokio::spawn(async move {
            agent
                .create_session(SessionConfig {
                    tools: Some(vec!["gate".into()]),
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
            tools: Some(vec!["gate".into()]),
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
    impl Plugin for CloseGate {
        type Deps = ();
        fn name(&self) -> &str {
            "CloseGate"
        }
        async fn apply(&self, ctx: &Ctx, (): ()) -> rutis::Result {
            let gate = self.0.clone();
            ctx.effect(move || async move {
                gate.entered.notify_one();
                gate.release.notified().await;
                Ok(())
            })?;
            Ok(())
        }
    }

    let root = Ctx::root().unwrap();
    let enabled = root.plugin(EnableContributor);
    plugin_state(&enabled, rutis::host::FiberState::Active).await;
    let gate = Arc::new(Gate::default());
    let hold = gate.clone();
    let mut builder = AgentBuilder::new(AgentConfig::default());
    defaults::mock_hosts(&mut builder);
    defaults::install(&mut builder);
    add_optional(&mut builder, Arc::default(), Arc::default(), Arc::default());
    builder.require_feature(
        Feature::new("CloseGate")
            .branch("CloseGate", move |_| Ok(Box::new(CloseGate(hold.clone())))),
    );
    let agent = builder.build_in(&root);
    let session = agent
        .create_session(SessionConfig {
            tools: Some(vec!["optional".into(), "echo".into()]),
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

    enabled.dispose().await.unwrap();
    plugin_state(&provider, rutis::host::FiberState::Pending).await;
    follow(&mut sources, |version| version.is_none()).await;
    let enabled = root.plugin(EnableContributor);
    plugin_state(&provider, rutis::host::FiberState::Active).await;
    let second = follow(&mut sources, |version| version.is_some()).await;
    assert_ne!(second, first);

    let closing = tokio::spawn(session.close());
    tokio::time::timeout(Duration::from_secs(5), gate.entered.notified())
        .await
        .unwrap();
    enabled.dispose().await.unwrap();
    plugin_state(&provider, rutis::host::FiberState::Pending).await;
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
    slot: Arc<Mutex<Option<Arc<ToolRegistry>>>>,
    registry: Key<ToolRegistry>,
}
impl DynPlugin for RegistryProbe {
    fn name(&self) -> &str {
        "RegistryProbe"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.registry.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let registry = ctx.require_at(&self.registry)?;
            *self.slot.lock().unwrap() = Some(registry);
            Ok(Effect::Done)
        })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_persisted_contributed_version_names_one_generation_across_restores() {
    let root = Ctx::root().unwrap();
    let enabled = root.plugin(EnableContributor);
    plugin_state(&enabled, rutis::host::FiberState::Active).await;
    let store = Arc::new(MemoryTimelineStore::default());
    let generations = Arc::new(AtomicUsize::new(0));
    let log = Arc::new(Log::default());
    let registry: Arc<Mutex<Option<Arc<ToolRegistry>>>> = Arc::default();
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
        builder.timeline_store(store.clone());
        add_optional(
            &mut builder,
            generations.clone(),
            Arc::default(),
            Arc::default(),
        );
        add_source(&mut builder, source, "mcp");
        let slot = registry.clone();
        builder.require_feature(Feature::new("RegistryProbe").session(
            "RegistryProbe",
            move |names| {
                Ok(Box::new(RegistryProbe {
                    slot: slot.clone(),
                    registry: names.key()?,
                }))
            },
        ));
        builder.build_in(&root)
    };
    let config = SessionConfig {
        tools: Some(vec!["optional".into(), "echo".into()]),
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
    let after = adopted_version(&restored, "main", "#contributed:optional")
        .await
        .unwrap();
    assert_ne!(after, before);
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

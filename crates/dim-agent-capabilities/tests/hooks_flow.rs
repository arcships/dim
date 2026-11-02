//! Hook points through the public Session interfaces and real commands
//! (`docs/design/plugins/Hooks.md`, appendix D).
#![cfg(unix)]
use dim_agent_capabilities::{
    defaults,
    execution_settings::ExecutionSettingsHost,
    history::HistoryRenderer,
    hooks::{
        HookSourceId, HookSourceSnapshot, HookSourceState, HookSourcesHost, HookSourcesSnapshot,
        HooksBranchPlugin, HooksDocument, HooksSessionPlugin, ReadHookSources,
    },
    host::*,
    host_platform::{
        ExecShellRuntime, HostPlatform, OutputEncoding, ShellEdition, ShellSource, ShellStatus,
    },
    methods::{RpcError, SessionMethods},
    model_provider::ModelProviderPlugin,
    permission::{CommandRulesHost, ReadCommandRules},
    skill_settings::SkillSettingsHost,
    skills::{MemorySkillSettings, SkillsCatalogPlugin},
    tool_registry::{ToolContribution, ToolHandler, ToolProvider, ToolRegistry, ToolSourceSet},
    workspace_tools::WorkspaceToolsConfig,
};
use dim_agent_runtime::testing::MinimalHost;
use dim_agent_runtime::*;
use dim_agent_types::*;
use rutis::Ctx;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, FiberView, Key, TypeKey};
use std::{
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio_util::sync::CancellationToken;

/// The test compaction strategy: the legal cut the retention rule allows and
/// a one-message summary.
#[derive(Clone, Copy, Default)]
struct Small;
impl ResolveContextModel for Small {
    fn resolve(
        &self,
        _: Arc<ModelRequest>,
        _: CancellationToken,
    ) -> HostFuture<'_, Arc<dyn ContextModel>> {
        Box::pin(async move { Ok(Arc::new(Small) as Arc<dyn ContextModel>) })
    }
}
impl ContextModel for Small {
    fn context_window(&self) -> f64 {
        4096.
    }
    fn output_tokens(&self) -> f64 {
        64.
    }
    fn threshold_tokens(&self) -> f64 {
        10_000.
    }
    fn count_tokens(&self, request: &ModelRequest) -> f64 {
        serde_json::to_vec(&request.messages).unwrap().len() as f64
    }
}
fn diagnostics() -> StrategyDiagnostics {
    StrategyDiagnostics {
        model_usage: None,
        model_call_count: 1,
        record: CompactionStrategyRecord {
            kind: "test".into(),
            details: JsonObject::new(),
        },
    }
}
impl CompactionStrategy for Small {
    fn name(&self) -> &str {
        "test"
    }
    fn replacement_size(&self, model: &dyn ContextModel) -> f64 {
        model.count_tokens(&ModelRequest::new(
            "",
            "",
            vec![dim_agent_capabilities::history::summary_message("short")],
        ))
    }
    fn compact(
        &self,
        input: CompactionInput,
        _: CancellationToken,
    ) -> HostFuture<'_, StrategyOutcome> {
        Box::pin(async move {
            Ok(match input.cut() {
                Some(cut) => StrategyOutcome::Completed {
                    retain_from: cut.retain_from.clone(),
                    replacement: vec![dim_agent_capabilities::history::summary_message("short")],
                    diagnostics: diagnostics(),
                },
                None => StrategyOutcome::Skipped {
                    reason: "nothing_to_compact".into(),
                    diagnostics: diagnostics(),
                },
            })
        })
    }
}

/// The hook documents the test serves.
#[derive(Default)]
struct Sources {
    user: Mutex<Option<HooksDocument>>,
    /// Whether the workspace's hooks are enabled, and its document.
    project: Mutex<Option<(bool, HooksDocument)>>,
    /// The documents plugins registered, with their directories.
    plugins: Mutex<Vec<(String, PathBuf, HooksDocument)>>,
    home: Mutex<Option<PathBuf>>,
}
impl Sources {
    fn set(&self, document: HooksDocument) {
        *self.user.lock().unwrap() = Some(document);
    }
    fn set_project(&self, document: HooksDocument, enabled: bool) {
        *self.project.lock().unwrap() = Some((enabled, document));
    }
    fn set_plugin(&self, id: &str, dir: &Path, document: HooksDocument) {
        self.plugins
            .lock()
            .unwrap()
            .push((id.to_owned(), dir.to_owned(), document));
    }
}
impl ReadHookSources for Sources {
    fn sources(&self, _: &Path, _: CancellationToken) -> HostFuture<'_, HookSourcesSnapshot> {
        let user = self.user.lock().unwrap().clone();
        let project = self.project.lock().unwrap().clone();
        let plugins = self.plugins.lock().unwrap().clone();
        Box::pin(async move {
            let mut sources = vec![HookSourceSnapshot {
                id: HookSourceId::User,
                path: None,
                enabled: true,
                state: if user.is_some() {
                    HookSourceState::Ok
                } else {
                    HookSourceState::Missing
                },
                document: user,
            }];
            if let Some((enabled, document)) = project {
                sources.push(HookSourceSnapshot {
                    id: HookSourceId::Project,
                    path: None,
                    enabled,
                    state: HookSourceState::Ok,
                    document: Some(document),
                });
            }
            for (id, dir, document) in plugins {
                sources.push(HookSourceSnapshot {
                    id: HookSourceId::Plugin(id),
                    path: Some(dir),
                    enabled: true,
                    state: HookSourceState::Ok,
                    document: Some(document),
                });
            }
            Ok(HookSourcesSnapshot { sources })
        })
    }
    fn home(&self) -> Option<PathBuf> {
        self.home.lock().unwrap().clone()
    }
}

/// The scripted model records each request, then answers like the echo model,
/// or fails on the provider budget so the turn compacts and retries. A hold
/// keeps the next answer waiting, so a call can be made while a turn runs.
#[derive(Default)]
struct Model {
    requests: Mutex<Vec<Arc<ModelRequest>>>,
    exhaust: bool,
    /// Holds request number `hold_from` (1-based) until `release` fires, so
    /// a turn stays open while the test acts.
    hold_from: Mutex<Option<usize>>,
    release: Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
}
impl ModelDriver for Model {
    fn invoke(
        &self,
        request: Arc<ModelRequest>,
        connection: ModelConnection,
        cancel: CancellationToken,
    ) -> HostFuture<'_, ModelStream> {
        let index = {
            let mut requests = self.requests.lock().unwrap();
            requests.push(request.clone());
            requests.len()
        };
        if self
            .hold_from
            .lock()
            .unwrap()
            .is_some_and(|from| index >= from)
        {
            let receiver = self.release.lock().unwrap().take();
            if let Some(receiver) = receiver {
                return Box::pin(async move {
                    tokio::select! {
                        _ = receiver => {}
                        _ = cancel.cancelled() => {}
                    }
                    dim_agent_capabilities::adapters::EchoModel
                        .invoke(request, connection, cancel)
                        .await
                });
            }
        }
        if self.exhaust {
            return Box::pin(async {
                Err(HostError::Failed {
                    kind: HostErrorKind::Other,
                    message: "Provider output budget exhausted".into(),
                    model: None,
                    presentation: None,
                    budget: Some(ProviderOutputBudgetFacts {
                        input_tokens: 800.,
                        context_window: 1000.,
                        requested_max_tokens: 400.,
                        allowed_max_tokens: 200.,
                        ratio: 1.,
                    }),
                })
            });
        }
        if let Some(calls) = last_user_text(&request).and_then(tool_batch) {
            return Box::pin(async move {
                let message = uuid::Uuid::new_v4().to_string();
                let mut chunks = vec![Ok(ModelChunk::MessageStart {
                    message_id: message.clone(),
                })];
                for call in calls {
                    chunks.push(Ok(ModelChunk::ToolStart {
                        id: call.id.clone(),
                        name: call.name.clone(),
                    }));
                    chunks.push(Ok(ModelChunk::ToolUse(call)));
                }
                chunks.push(Ok(ModelChunk::MessageStop {
                    message_id: message,
                    reason: ModelStopReason::ToolUse,
                }));
                Ok(Box::pin(futures_util::stream::iter(chunks)) as ModelStream)
            });
        }
        dim_agent_capabilities::adapters::EchoModel.invoke(request, connection, cancel)
    }
}

/// The text parts of one message, as the echo model reads them.
fn message_text(message: &Message) -> String {
    match &message.content {
        MessageContent::Text(text) => text.clone(),
        MessageContent::Parts(parts) => parts
            .iter()
            .filter_map(|part| match part {
                MessagePart::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

fn last_user_text(request: &ModelRequest) -> Option<String> {
    request
        .messages
        .last()
        .filter(|message| message.role == Role::User)
        .map(|message| message_text(message))
}

/// The calls of a `tools: name input | name input` submission, in order, so a
/// test can drive one batch of several calls.
fn tool_batch(text: String) -> Option<Vec<ToolCall>> {
    let commands = text.strip_prefix("tools:")?;
    Some(
        commands
            .split(" | ")
            .map(|command| {
                let command = command.trim();
                let (name, input) = command.split_once(' ').unwrap_or((command, "{}"));
                ToolCall {
                    id: uuid::Uuid::new_v4().to_string(),
                    name: name.into(),
                    input: serde_json::from_str(input).expect("test tool command JSON"),
                }
            })
            .collect(),
    )
}

/// The Session's command rules as the test sets them.
#[derive(Default)]
struct TestRules(Mutex<Vec<CommandRule>>);
impl TestRules {
    fn set(&self, rules: Vec<CommandRule>) {
        *self.0.lock().unwrap() = rules;
    }
}
impl ReadCommandRules for TestRules {
    fn rules(&self, _: CancellationToken) -> HostFuture<'_, Vec<CommandRule>> {
        let rules = self.0.lock().unwrap().clone();
        Box::pin(async move { Ok(rules) })
    }
}
impl Model {
    fn requests(&self) -> Vec<Arc<ModelRequest>> {
        self.requests.lock().unwrap().clone()
    }
    /// Holds request `index` (1-based) and later ones until the returned
    /// sender fires, and returns that sender.
    fn hold(&self, index: usize) -> tokio::sync::oneshot::Sender<()> {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        *self.hold_from.lock().unwrap() = Some(index);
        *self.release.lock().unwrap() = Some(receiver);
        sender
    }
}

/// What a tool declaration in the test's assembly is known by.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Extra {
    /// The host's tools only.
    #[default]
    None,
    /// A registered `deploy` tool, with risk `execute_process`.
    Registered,
    /// A tool source offering an MCP-style `lookup` tool, so `ToolBinding`
    /// adopts a tool part.
    Served,
}

/// The handler of the test's registered `deploy` tool.
struct Deploy;
impl ToolHandler for Deploy {
    fn execute(
        &self,
        _: ToolCall,
        _: ToolExecutionContext,
        _: CancellationToken,
    ) -> HostFuture<'_, ToolResult> {
        Box::pin(async move { Ok(ToolResult::text("deployed")) })
    }
}

/// Registers `deploy`, a tool other than `exec` with risk `execute_process`.
struct RegisteredTool {
    registry: Key<ToolRegistry>,
}
impl DynPlugin for RegisteredTool {
    fn name(&self) -> &str {
        "RegisteredTool"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.registry.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let registry = ctx.require_at(&self.registry)?;
            let mut definition = dim_agent_capabilities::tool_registry::definition(
                "deploy",
                "Deploy",
                serde_json::json!({"type": "object"}),
            );
            definition.risk = Some("execute_process".into());
            registry.register(
                ctx,
                ToolContribution {
                    definition,
                    handler: Arc::new(Deploy),
                },
            )?;
            Ok(Effect::Done)
        })
    }
}

/// The handler of the test's MCP-style `lookup` tool.
struct Lookup;
impl ToolHandler for Lookup {
    fn execute(
        &self,
        _: ToolCall,
        _: ToolExecutionContext,
        _: CancellationToken,
    ) -> HostFuture<'_, ToolResult> {
        Box::pin(async move { Ok(ToolResult::text("looked up")) })
    }
}

/// A tool source offering one tool with MCP metadata, which names it
/// `mcp__<server>__<original>` for matchers (G.2).
struct Served;
impl ToolProvider for Served {
    fn sets(&self) -> Vec<Arc<ToolSourceSet>> {
        let mut definition = dim_agent_capabilities::tool_registry::definition(
            "lookup",
            "Look up an item",
            serde_json::json!({"type": "object"}),
        );
        definition.metadata = Some(ToolMetadata {
            mcp_server_id: Some("server".into()),
            mcp_original_tool_name: Some("lookup_item".into()),
            ..Default::default()
        });
        vec![Arc::new(ToolSourceSet {
            key: "served".into(),
            version: "1".into(),
            tools: vec![Arc::new(ToolContribution {
                definition,
                handler: Arc::new(Lookup),
            })],
        })]
    }
}

/// Registers the `served` source, so the branch declares a tool part.
struct ServedSource {
    registry: Key<ToolRegistry>,
}
impl DynPlugin for ServedSource {
    fn name(&self) -> &str {
        "ServedSource"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.registry.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let registry = ctx.require_at(&self.registry)?;
            registry.register_source(ctx, "served", Arc::new(Served))?;
            Ok(Effect::Done)
        })
    }
}

/// Reaches the branch's Timeline and the Session's registry, so a test can
/// write the configuration a branch runs with, ahead of any adoption.
#[derive(Default)]
struct ToolsProbe {
    timeline: Mutex<Option<Arc<Timeline>>>,
    registry: Mutex<Option<Arc<ToolRegistry>>>,
}
/// Captures the Timeline and the registry for [`ToolsProbe`].
struct ToolsProbePlugin {
    probe: Arc<ToolsProbe>,
    timeline: Key<Timeline>,
    registry: Key<ToolRegistry>,
}
impl DynPlugin for ToolsProbePlugin {
    fn name(&self) -> &str {
        "ToolsProbe"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.timeline.erase(), self.registry.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            *self.probe.timeline.lock().unwrap() = Some(ctx.require_at(&self.timeline)?);
            *self.probe.registry.lock().unwrap() = Some(ctx.require_at(&self.registry)?);
            Ok(Effect::Done)
        })
    }
}

struct Env {
    agent: MinimalHost,
    sources: Arc<Sources>,
    model: Arc<Model>,
    dir: PathBuf,
    /// The Session's command rules as the test sets them.
    rules: Arc<TestRules>,
    /// The `HooksSession` instance the test can unload, when the harness
    /// mounts it through an owner (`Env::with_mounted_hooks`).
    hooks: Arc<Mutex<Vec<FiberView>>>,
    /// The `HooksBranch` instance the same owner mounts, per branch.
    branch_hooks: Arc<Mutex<Vec<FiberView>>>,
    /// The tools the Session's configuration mounts.
    mounted: Vec<String>,
    probe: Arc<ToolsProbe>,
}
impl Env {
    async fn new() -> Self {
        Self::build(Arc::new(Model::default()), None, false, Extra::None).await
    }

    async fn with(model: Arc<Model>, strategy: Option<Arc<Small>>) -> Self {
        Self::build(model, strategy, false, Extra::None).await
    }

    /// The same host, with `HooksSession` mounted as a child of a test owner
    /// so a test can unload it mid-call.
    async fn with_mounted_hooks(model: Arc<Model>, strategy: Option<Arc<Small>>) -> Self {
        Self::build(model, strategy, true, Extra::None).await
    }

    /// A Session whose registry declares `extra` and whose configuration
    /// mounts `mounted`.
    async fn with_tools(extra: Extra, mounted: &[&str]) -> Self {
        let mut env = Self::build(Arc::new(Model::default()), None, false, extra).await;
        env.mounted = mounted.iter().map(|name| (*name).to_owned()).collect();
        env
    }

    async fn build(
        model: Arc<Model>,
        strategy: Option<Arc<Small>>,
        mounted_hooks: bool,
        extra: Extra,
    ) -> Self {
        let context_model: Option<Arc<dyn ResolveContextModel>> = match &strategy {
            Some(strategy) => {
                let model: Arc<dyn ResolveContextModel> = strategy.clone();
                Some(model)
            }
            None => None,
        };
        let compaction: Option<Arc<dyn CompactionStrategy>> = match strategy {
            Some(strategy) => {
                let compaction: Arc<dyn CompactionStrategy> = strategy;
                Some(compaction)
            }
            None => None,
        };
        let dir = std::env::temp_dir().join(format!("dim-hooks-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("work")).unwrap();
        std::fs::create_dir_all(dir.join("home")).unwrap();
        let dir = dir.canonicalize().unwrap();
        let mut platform = HostPlatform::current(ShellStatus::Available(ExecShellRuntime {
            executable_path: "/bin/sh".into(),
            edition: ShellEdition::Sh,
            source: ShellSource::Fallback,
            output_encoding: OutputEncoding::Utf8,
        }))
        .unwrap()
        .with_execution_settings(ExecutionSettingsHost::fixed(Default::default()));
        platform.home = dir.join("home");
        let sources = Arc::new(Sources::default());
        *sources.home.lock().unwrap() = Some(dir.join("home"));
        let rules = Arc::new(TestRules::default());
        let mut builder = AgentBuilder::new(AgentConfig::default());
        builder.timeline_store(Arc::new(MemoryTimelineStore::default()));
        builder
            .session_value(Arc::new(HistoryRenderer::default()))
            .process_value(Arc::new(ModelHost(model.clone())))
            .session_value(Arc::new(PrepareContextHost(Arc::new(IdentityContext))))
            .session_value(Arc::new(ContextModelHost(context_model)))
            .session_value(Arc::new(CommandRulesHost(rules.clone())))
            .session_value(Arc::new(CompactionStrategyHost(compaction)))
            .session_value(Arc::new(HookSourcesHost(sources.clone())))
            .require_feature(Feature::new("PrepareModelHost").session(
                "PrepareModelHost",
                |names| {
                    Ok(Box::new(
                        dim_agent_capabilities::adapters::DefaultPrepareModelPlugin::new(names)?,
                    ))
                },
            ));
        defaults::install_local_execution(
            &mut builder,
            ModelProviderPlugin::new,
            WorkspaceToolsConfig {
                output_root: dir.join("outputs"),
            },
            dim_agent_capabilities::timeline_tree::BranchToolConfig::default(),
        );
        builder.plugin(|names| Ok(Box::new(SkillsCatalogPlugin::new(names)?)));
        let probe = Arc::new(ToolsProbe::default());
        let captured = probe.clone();
        builder.require_feature(
            Feature::new("ToolsProbe").branch("ToolsProbe", move |names| {
                Ok(Box::new(ToolsProbePlugin {
                    probe: captured.clone(),
                    timeline: names.key()?,
                    registry: names.key()?,
                }))
            }),
        );
        match extra {
            Extra::None => {}
            Extra::Registered => {
                builder.require_feature(Feature::new("RegisteredTool").session(
                    "RegisteredTool",
                    |names| {
                        Ok(Box::new(RegisteredTool {
                            registry: names.key()?,
                        }))
                    },
                ));
            }
            Extra::Served => {
                builder.require_feature(Feature::new("ServedSource").session(
                    "ServedSource",
                    |names| {
                        Ok(Box::new(ServedSource {
                            registry: names.key()?,
                        }))
                    },
                ));
            }
        }
        let hooks: Arc<Mutex<Vec<FiberView>>> = Arc::new(Mutex::new(Vec::new()));
        let branch_hooks: Arc<Mutex<Vec<FiberView>>> = Arc::new(Mutex::new(Vec::new()));
        if mounted_hooks {
            // Both Hooks parts are mounted as children of test owners, so a
            // test can unload either mid-call.
            let views = hooks.clone();
            builder.require_feature(Feature::new("HooksTest").session(
                "HooksSessionOwner",
                move |names| {
                    let plugin = HooksSessionPlugin::new(names)?;
                    Ok(Box::new(HooksSessionOwner {
                        injects: plugin.injects(),
                        plugin: Mutex::new(Some(plugin)),
                        views: views.clone(),
                    }))
                },
            ));
            let views = branch_hooks.clone();
            builder.require_feature(Feature::new("HooksBranchTest").branch(
                "HooksBranchOwner",
                move |names| {
                    let plugin = HooksBranchPlugin::new(names)?;
                    Ok(Box::new(HooksBranchOwner {
                        plugin: Mutex::new(Some(plugin)),
                        views: views.clone(),
                    }))
                },
            ));
        } else {
            dim_agent_capabilities::hooks::install(&mut builder);
        }
        let agent = MinimalHost::start_with(&builder, |root, _| {
            root.provide_as::<HostPlatform>(Arc::new(platform))?;
            root.provide_as::<SkillSettingsHost>(Arc::new(SkillSettingsHost(Arc::new(
                MemorySkillSettings::default(),
            ))))?;
            Ok(())
        })
        .await
        .unwrap();
        Self {
            agent,
            sources,
            model,
            dir,
            rules,
            hooks,
            branch_hooks,
            mounted: vec!["read".into(), "exec".into()],
            probe,
        }
    }

    /// Disposes the mounted `HooksBranch`, as unloading the plugin does.
    async fn unload_branch_hooks(&self) {
        let view = self
            .branch_hooks
            .lock()
            .unwrap()
            .last()
            .cloned()
            .expect("a mounted HooksBranch");
        view.dispose().await.unwrap();
    }

    /// Unloads and reloads `HooksSession`, as a provider reload does: its
    /// token fires before it is withdrawn.
    async fn restart_hooks(&self) {
        let view = self
            .hooks
            .lock()
            .unwrap()
            .last()
            .cloned()
            .expect("a mounted HooksSession");
        view.restart().await.unwrap();
    }

    async fn session(&self) -> Session {
        self.session_with(PermissionPreset::FullAccess).await
    }

    async fn session_with(&self, preset: PermissionPreset) -> Session {
        self.agent
            .create_session(SessionConfig {
                cwd: self.dir.join("work").to_string_lossy().into_owned(),
                tools: Some(self.mounted.clone()),
                permission_policy: PermissionPolicy {
                    preset,
                    ..Default::default()
                },
                ..Default::default()
            })
            .await
            .unwrap()
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }

    /// Writes a `context/configured` whose tool part declares `names`, from
    /// the Session's own declarations, so a test can change the branch's part
    /// mid-flight as removing a tool source does.
    async fn declare_tools(&self, session: &Session, names: &[&str]) {
        let registry = self
            .probe
            .registry
            .lock()
            .unwrap()
            .clone()
            .expect("the registry probe");
        let tools: Vec<ToolDefinition> = registry
            .definitions()
            .into_iter()
            .filter(|tool| names.contains(&tool.name.as_str()))
            .collect();
        let events = events(session).await;
        let previous = events
            .iter()
            .rev()
            .find_map(|event| match &event.kind {
                EventKind::ContextConfigured(data) => Some(data.clone()),
                _ => None,
            })
            .expect("a context/configured");
        let timeline = self
            .probe
            .timeline
            .lock()
            .unwrap()
            .clone()
            .expect("the timeline probe");
        timeline
            .append(
                EventKind::ContextConfigured(ContextConfiguredData {
                    timeline: "main".into(),
                    revision: uuid::Uuid::new_v4().to_string(),
                    previous: Some(PromptReference {
                        timeline: previous.timeline.clone(),
                        revision: previous.revision.clone(),
                    }),
                    prompt: previous.prompt.clone(),
                    settings: Box::new(ModelConfiguration {
                        tools: Some(tools),
                        tool_sources: Vec::new(),
                        ..(*previous.settings).clone()
                    }),
                    system: previous.system.clone(),
                    system_source: previous.system_source.clone(),
                })
                .into(),
            )
            .unwrap();
    }

    async fn close(self) {
        self.agent.close().await.unwrap();
        std::fs::remove_dir_all(&self.dir).unwrap();
    }
}

/// Mounts the real `HooksSession` as a child, so a test can unload it and
/// read what an unload settles (A.5, A.6.2's reload row).
struct HooksSessionOwner {
    plugin: Mutex<Option<HooksSessionPlugin>>,
    /// What the mounted plugin injects; the owner is ordered by the same
    /// services, so it mounts after `SubmitChecks` is provided.
    injects: Vec<TypeKey>,
    views: Arc<Mutex<Vec<FiberView>>>,
}
impl DynPlugin for HooksSessionOwner {
    fn name(&self) -> &str {
        "HooksSessionOwner"
    }
    fn injects(&self) -> Vec<TypeKey> {
        self.injects.clone()
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let plugin = self
                .plugin
                .lock()
                .unwrap()
                .take()
                .expect("the owner mounts its HooksSession once");
            let view = dim_agent_runtime::mount_active(ctx, plugin).await?;
            self.views.lock().unwrap().push(view);
            Ok(Effect::Done)
        })
    }
}

/// Mounts the real `HooksBranch` as a child, so a test can unload it and
/// read what an unload settles (A.6.1's reload row).
struct HooksBranchOwner {
    plugin: Mutex<Option<HooksBranchPlugin>>,
    views: Arc<Mutex<Vec<FiberView>>>,
}
impl DynPlugin for HooksBranchOwner {
    fn name(&self) -> &str {
        "HooksBranchOwner"
    }
    fn injects(&self) -> Vec<TypeKey> {
        self.plugin
            .lock()
            .unwrap()
            .as_ref()
            .map(DynPlugin::injects)
            .unwrap_or_default()
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let plugin = self
                .plugin
                .lock()
                .unwrap()
                .take()
                .expect("the owner mounts its HooksBranch once");
            let view = dim_agent_runtime::mount_active(ctx, plugin).await?;
            self.views.lock().unwrap().push(view);
            Ok(Effect::Done)
        })
    }
}

fn document(hooks: serde_json::Value) -> HooksDocument {
    serde_json::from_value(serde_json::json!({ "hooks": hooks })).unwrap()
}
fn command(group: serde_json::Value) -> serde_json::Value {
    serde_json::json!({ "hooks": [group] })
}

async fn turn(session: &Session, text: &str) {
    session
        .submit(text.into(), InputMode::Queue, "main")
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), session.idle("main"))
        .await
        .unwrap()
        .unwrap();
}

async fn events(session: &Session) -> Vec<Arc<AgentEvent>> {
    session.local("main", ..).await.unwrap()
}

/// The raw blocks of every input, in order.
async fn blocks(session: &Session) -> Vec<InputBlock> {
    events(session)
        .await
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::InputMessage(data) => Some(data.blocks.clone()),
            _ => None,
        })
        .flatten()
        .collect()
}

fn block_text(block: &InputBlock) -> &str {
    match block {
        InputBlock::Raw { text, .. } => text,
    }
}

/// The text of every hook block of `kind`, in order.
async fn hook_blocks(session: &Session, kind: &str) -> Vec<String> {
    blocks(session)
        .await
        .iter()
        .filter(|block| block_text(block).contains(&format!("<dim-hooks kind=\"{kind}\"")))
        .map(|block| block_text(block).to_owned())
        .collect()
}

/// Waits for a hook block of `kind` to be written.
/// Waits for the command's marker file.
async fn wait_file(path: &Path, message: &str) {
    for _ in 0..500 {
        if path.exists() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("{message}");
}

async fn wait_blocks(session: &Session, kind: &str, count: usize) -> Vec<String> {
    for _ in 0..500 {
        let found = hook_blocks(session, kind).await;
        if found.len() >= count {
            return found;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!(
        "{count} {kind} blocks were not written: {:?}",
        hook_blocks(session, kind).await
    );
}

/// The text of the last model request.
fn last_request(model: &Model) -> String {
    let requests = model.requests();
    let request = requests.last().expect("a model request");
    serde_json::to_string(&request.messages).unwrap()
}

fn sleep(millis: u64) -> String {
    format!("sleep {}", millis as f64 / 1000.0)
}

fn marker_command(path: &Path, text: &str) -> String {
    format!("printf '{text}' >> {}", path.display())
}

/// Waits for an event matching `find` on `main`.
async fn wait_event<T>(session: &Session, find: impl Fn(&EventKind) -> Option<T>) -> T {
    for _ in 0..500 {
        if let Some(found) = session
            .local("main", ..)
            .await
            .unwrap()
            .iter()
            .find_map(|event| find(&event.kind))
        {
            return found;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the event was not committed");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_start_writes_one_keyed_block_per_hook_and_the_model_sees_it() {
    let env = Env::new().await;
    env.sources.set(document(serde_json::json!({
        "SessionStart": [command(serde_json::json!({
            "type": "command", "command": "printf 'first context'"
        }))],
        "Stop": [command(serde_json::json!({
            "type": "command", "command": "printf '{\"hookSpecificOutput\":{\"additionalContext\":\"stop context\"}}'"
        }))],
    })));
    let session = env.session().await;
    let blocks = wait_blocks(&session, "session-start", 1).await;
    assert_eq!(blocks.len(), 1, "{blocks:?}");
    assert!(blocks[0].contains("first context"), "{blocks:?}");
    let keyed = blocks_of_key(&session, "hooks:session-start/1").await;
    assert_eq!(keyed, 1, "the block is keyed once");

    turn(&session, "hello").await;
    let request = last_request(&env.model);
    assert!(
        request.contains("first context"),
        "the first request carries it"
    );
    env.close().await;
}

/// §3.4: `SessionStart`'s output is dropped when the branch moved on while
/// its command ran. The command announces that it started and waits on a
/// gate; the first turn runs to its `turn/end` before the test releases the
/// gate, and the command writes `finished` after it. The next turn is the
/// barrier: the point's write, if any, happens right after that marker and
/// so before it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_start_context_is_dropped_once_the_first_turn_has_run() {
    let env = Env::new().await;
    let entered = env.path("session-start-entered");
    let gate = env.path("session-start-gate");
    let finished = env.path("session-start-finished");
    env.sources.set(document(serde_json::json!({
        "SessionStart": [command(serde_json::json!({
            "type": "command",
            "command": format!(
                "printf 'entered' > {}; while [ ! -f {} ]; do sleep 0.05; done; printf 'finished' > {}; printf 'late context'",
                entered.display(),
                gate.display(),
                finished.display(),
            ),
        }))],
    })));
    let session = env.session().await;
    for _ in 0..500 {
        if entered.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(entered.exists(), "the SessionStart hook did not start");
    turn(&session, "hello").await;
    std::fs::write(&gate, "").unwrap();
    for _ in 0..500 {
        if finished.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(finished.exists(), "the SessionStart command did not end");
    turn(&session, "again").await;
    let late = hook_blocks(&session, "session-start").await;
    assert!(
        late.is_empty(),
        "SessionStart context was appended after the first turn: {late:?}"
    );
    env.close().await;
}

/// §3.4: an interrupt while the `SessionStart` command runs drops its output
/// too. The interrupt is committed on an idle `main`, which is a
/// `stop/interrupt` fact like any other; the turn after the marker is the
/// barrier.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_start_context_is_dropped_by_an_interrupt() {
    let env = Env::new().await;
    let entered = env.path("session-start-interrupt-entered");
    let gate = env.path("session-start-interrupt-gate");
    let finished = env.path("session-start-interrupt-finished");
    env.sources.set(document(serde_json::json!({
        "SessionStart": [command(serde_json::json!({
            "type": "command",
            "command": format!(
                "printf 'entered' > {}; while [ ! -f {} ]; do sleep 0.05; done; printf 'finished' > {}; printf 'late context'",
                entered.display(),
                gate.display(),
                finished.display(),
            ),
        }))],
    })));
    let session = env.session().await;
    for _ in 0..500 {
        if entered.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(entered.exists(), "the SessionStart hook did not start");
    session.interrupt("main", "stop".into()).unwrap();
    std::fs::write(&gate, "").unwrap();
    for _ in 0..500 {
        if finished.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(finished.exists(), "the SessionStart command did not end");
    turn(&session, "after").await;
    let late = hook_blocks(&session, "session-start").await;
    assert!(
        late.is_empty(),
        "SessionStart context was appended after an interrupt: {late:?}"
    );
    env.close().await;
}

async fn blocks_of_key(session: &Session, key: &str) -> usize {
    blocks(session)
        .await
        .iter()
        .filter(|block| matches!(block, InputBlock::Raw { key: Some(found), .. } if found == key))
        .count()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn post_tool_use_follows_the_protocol_for_success_block_and_failure() {
    let env = Env::new().await;
    let ok = env.path("ok.sh");
    std::fs::write(
        &ok,
        "#!/bin/sh\nprintf '{\"hookSpecificOutput\":{\"additionalContext\":\"tool context\"}}'\n",
    )
    .unwrap();
    let blocked = env.path("blocked.sh");
    std::fs::write(&blocked, "#!/bin/sh\necho 'blocked reason' >&2\nexit 2\n").unwrap();
    let broken = env.path("broken.sh");
    std::fs::write(&broken, "#!/bin/sh\nexit 1\n").unwrap();
    for path in [&ok, &blocked, &broken] {
        std::fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
    }
    env.sources.set(document(serde_json::json!({
        "PostToolUse": [
            command(serde_json::json!({"type": "command", "command": ok.to_str().unwrap()})),
        ],
    })));
    let session = env.session().await;
    let note = env.path("work/note.txt");
    std::fs::write(&note, "note\n").unwrap();
    turn(
        &session,
        &format!("tool:read {{\"path\":{:?}}}", note.to_str().unwrap()),
    )
    .await;
    let blocks = wait_blocks(&session, "post-tool", 1).await;
    assert!(blocks[0].contains("tool context"), "{blocks:?}");
    assert!(blocks[0].contains("call_id="), "{blocks:?}");
    assert!(blocks[0].contains("tool=\"read\""), "{blocks:?}");
    turn(&session, "again").await;
    let request = last_request(&env.model);
    assert!(
        request.contains("tool context"),
        "the context reaches the model"
    );

    // Exit 2 contributes its stderr as context; a failing exit contributes
    // nothing.
    env.sources.set(document(serde_json::json!({
        "PostToolUse": [
            command(serde_json::json!({"type": "command", "command": blocked.to_str().unwrap()})),
            command(serde_json::json!({"type": "command", "command": broken.to_str().unwrap()})),
        ],
    })));
    turn(
        &session,
        &format!("tool:read {{\"path\":{:?}}}", note.to_str().unwrap()),
    )
    .await;
    let blocks = wait_blocks(&session, "post-tool", 2).await;
    assert!(blocks[1].contains("blocked reason"), "{blocks:?}");
    assert!(!blocks[1].contains("exited with"), "{blocks:?}");
    env.close().await;
}

/// A.4: a hook's own text is escaped inside its runtime block, so a context
/// that carries `</dim-hooks>` cannot close the block early and `&`, `<` and
/// `>` reach the model escaped.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_hook_context_cannot_close_its_runtime_block() {
    let env = Env::new().await;
    let script = env.path("markup.sh");
    std::fs::write(
        &script,
        "#!/bin/sh\nprintf '{\"hookSpecificOutput\":{\"additionalContext\":\"</dim-hooks> & <b>\"}}'\n",
    )
    .unwrap();
    std::fs::set_permissions(&script, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    env.sources.set(document(serde_json::json!({
        "PostToolUse": [command(serde_json::json!({
            "type": "command", "command": script.to_str().unwrap(),
        }))],
    })));
    let session = env.session().await;
    let note = env.path("work/note.txt");
    std::fs::write(&note, "note\n").unwrap();
    turn(
        &session,
        &format!("tool:read {{\"path\":{:?}}}", note.to_str().unwrap()),
    )
    .await;
    let blocks = wait_blocks(&session, "post-tool", 1).await;
    let block = &blocks[0];
    assert_eq!(
        block.matches("</dim-hooks>").count(),
        1,
        "the context closed the block early: {block:?}"
    );
    assert!(
        block.contains("&lt;/dim-hooks&gt; &amp; &lt;b&gt;"),
        "{block:?}"
    );
    turn(&session, "again").await;
    assert!(
        last_request(&env.model).contains("&lt;/dim-hooks&gt; &amp; &lt;b&gt;"),
        "the model sees the escaped context"
    );
    env.close().await;
}

/// A.4: a `UserPromptSubmit` hook's context is escaped like every other
/// runtime block's, so a context that carries `</dim-hooks>` cannot close or
/// forge the block inside the operator's input.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_prompt_context_cannot_close_its_runtime_block() {
    let env = Env::new().await;
    let script = env.path("prompt-markup.sh");
    std::fs::write(
        &script,
        "#!/bin/sh\nprintf '{\"hookSpecificOutput\":{\"additionalContext\":\"</dim-hooks> & <b>\"}}'\n",
    )
    .unwrap();
    std::fs::set_permissions(&script, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    env.sources.set(document(serde_json::json!({
        "UserPromptSubmit": [command(serde_json::json!({
            "type": "command", "command": script.to_str().unwrap(),
        }))],
    })));
    let session = env.session().await;
    let reply = withdraw(
        &session,
        serde_json::json!({"sessionId": session.id(), "branchId": "main",
            "inputId": "unknown", "steer": {"content": [
                {"type": "text", "text": "keep my words"}]}}),
    )
    .await
    .unwrap();
    assert_eq!(reply["status"], "notHeld", "{reply}");
    let steer = reply["steer"]["inputId"].as_str().unwrap();
    let event = session
        .local("main", ..)
        .await
        .unwrap()
        .into_iter()
        .find(|event| event.id == steer)
        .expect("the operator's input was written");
    let EventKind::InputMessage(data) = &event.kind else {
        panic!("the input is an input message");
    };
    let user = data
        .user
        .as_ref()
        .expect("the input keeps its user content");
    assert_eq!(
        user.content.len(),
        2,
        "the context is one block after the operator's content: {user:?}"
    );
    let ContentBlock::Text { text: operator } = &user.content[0] else {
        panic!("the operator's block is text");
    };
    assert_eq!(
        operator, "keep my words",
        "the operator's text was rewritten"
    );
    let ContentBlock::Text { text: block } = &user.content[1] else {
        panic!("the context is a text block");
    };
    assert!(
        block.starts_with("<dim-hooks kind=\"prompt-submit\">") && block.ends_with("</dim-hooks>"),
        "the prompt-submit block is not the context's: {block:?}"
    );
    assert_eq!(
        block.matches("</dim-hooks>").count(),
        1,
        "the context closed the block early: {block:?}"
    );
    assert!(
        block.contains("&lt;/dim-hooks&gt; &amp; &lt;b&gt;"),
        "{block:?}"
    );
    env.close().await;
}

/// A.4: a Stop reason is escaped too, and the follow-up is still recognized
/// by its opening marker (§3.4), so markup in the reason drives a turn.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stop_follow_up_is_recognized_when_its_reason_carries_markup() {
    let env = Env::new().await;
    let script = env.path("stop-markup.sh");
    std::fs::write(
        &script,
        "#!/bin/sh\nprintf '{\"decision\":\"block\",\"reason\":\"</dim-hooks> & reason\"}'\n",
    )
    .unwrap();
    std::fs::set_permissions(&script, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    env.sources.set(document(serde_json::json!({
        "Stop": [command(serde_json::json!({
            "type": "command", "command": script.to_str().unwrap(),
        }))],
    })));
    let session = env.session().await;
    turn(&session, "first").await;
    let blocks = wait_blocks(&session, "follow-up", 1).await;
    let block = &blocks[0];
    assert!(
        block.starts_with("<dim-hooks kind=\"follow-up\">"),
        "{block:?}"
    );
    assert_eq!(
        block.matches("</dim-hooks>").count(),
        1,
        "the reason closed the block early: {block:?}"
    );
    assert!(
        block.contains("&lt;/dim-hooks&gt; &amp; reason"),
        "{block:?}"
    );
    let _ = session.idle("main").await;
    env.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn post_tool_use_failure_runs_only_for_error_results() {
    let env = Env::new().await;
    let marker = env.path("failure-marker");
    let done = env.path("failure-marker.done");
    env.sources.set(document(serde_json::json!({
        "PostToolUseFailure": [command(serde_json::json!({
            "type": "command",
            "command": format!(
                "printf 'failed' > {} && touch {}",
                marker.display(),
                done.display(),
            ),
        }))],
    })));
    let session = env.session().await;
    let note = env.path("work/note.txt");
    std::fs::write(&note, "note\n").unwrap();
    turn(
        &session,
        &format!("tool:read {{\"path\":{:?}}}", note.to_str().unwrap()),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(!marker.exists(), "a success runs only PostToolUse");
    turn(
        &session,
        &format!(
            "tool:read {{\"path\":{:?}}}",
            env.path("work/missing.txt").to_str().unwrap()
        ),
    )
    .await;
    wait_file(&done, "PostToolUseFailure did not run").await;
    assert_eq!(std::fs::read_to_string(&marker).unwrap(), "failed");
    env.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_timeout_is_a_failure_and_a_blocking_failure_still_answers() {
    let env = Env::new().await;
    let slow = env.path("slow.sh");
    std::fs::write(&slow, "#!/bin/sh\nsleep 5\n").unwrap();
    std::fs::set_permissions(&slow, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    env.sources.set(document(serde_json::json!({
        "PostToolUse": [command(serde_json::json!({
            "type": "command", "command": slow.to_str().unwrap(), "timeout": 1
        }))],
    })));
    let session = env.session().await;
    let started = std::time::Instant::now();
    turn(&session, "tool:read {}").await;
    assert!(started.elapsed() < Duration::from_secs(5));
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(hook_blocks(&session, "post-tool").await.is_empty());
    env.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stop_follow_ups_are_bounded_at_three_and_flag_the_driven_turns() {
    let env = Env::new().await;
    let seen = env.path("stop-payloads");
    env.sources.set(document(serde_json::json!({
        "Stop": [command(serde_json::json!({
            "type": "command",
            "command": format!("{{ cat; echo; }} >> {}; echo again >&2; exit 2", seen.display()),
        }))],
    })));
    let session = env.session().await;
    turn(&session, "go").await;
    let blocks = wait_blocks(&session, "follow-up", 3).await;
    assert_eq!(blocks.len(), 3, "{blocks:?}");
    assert!(blocks[0].contains("again"), "{blocks:?}");
    session.idle("main").await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    let payloads: Vec<serde_json::Value> = std::fs::read_to_string(&seen)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(
        hook_blocks(&session, "follow-up").await.len(),
        3,
        "{:?}",
        payloads
            .iter()
            .map(|payload| payload["stop_hook_active"].clone())
            .collect::<Vec<_>>()
    );
    assert_eq!(payloads.len(), 3, "{payloads:?}");
    assert_eq!(
        payloads
            .iter()
            .map(|payload| payload["stop_hook_active"].clone())
            .collect::<Vec<_>>(),
        vec![
            serde_json::json!(false),
            serde_json::json!(true),
            serde_json::json!(true)
        ]
    );
    env.close().await;
}

/// §3.4: a Stop follow-up is written by a block, not by context alone. Each
/// Stop point runs the handler once, and the drain runs the points in order,
/// so a second `run` line proves the first point has written everything it
/// would write.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_context_only_stop_writes_no_follow_up_and_starts_no_turn() {
    let env = Env::new().await;
    let runs = env.path("stop-runs");
    env.sources.set(document(serde_json::json!({
        "Stop": [command(serde_json::json!({
            "type": "command",
            "command": format!(
                "printf 'run\\n' >> {}; printf '{{\"hookSpecificOutput\":{{\"additionalContext\":\"stop context\"}}}}'",
                runs.display(),
            ),
        }))],
    })));
    let session = env.session().await;
    turn(&session, "first").await;
    turn(&session, "barrier").await;
    for _ in 0..500 {
        let count = std::fs::read_to_string(&runs)
            .map(|text| text.lines().count())
            .unwrap_or(0);
        if count >= 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        std::fs::read_to_string(&runs)
            .unwrap_or_default()
            .lines()
            .count(),
        2,
        "the second Stop point did not run"
    );
    assert!(
        hook_blocks(&session, "follow-up").await.is_empty(),
        "a context-only Stop wrote a follow-up"
    );
    let starts = events(&session)
        .await
        .iter()
        .filter(|event| matches!(event.kind, EventKind::TurnStart(_)))
        .count();
    assert_eq!(starts, 2, "a context-only Stop started a turn");
    env.close().await;
}

/// A.4: a `Stop` block with an empty reason, here exit 2 with no stderr,
/// writes no follow-up; the point still runs, and the turn ends.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stop_block_without_a_reason_writes_no_follow_up() {
    let env = Env::new().await;
    let runs = env.path("stop-runs");
    env.sources.set(document(serde_json::json!({
        "Stop": [command(serde_json::json!({
            "type": "command",
            "command": format!("printf 'run\\n' >> {}; exit 2", runs.display()),
        }))],
    })));
    let session = env.session().await;
    turn(&session, "first").await;
    turn(&session, "barrier").await;
    for _ in 0..500 {
        let count = std::fs::read_to_string(&runs)
            .map(|text| text.lines().count())
            .unwrap_or(0);
        if count >= 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        std::fs::read_to_string(&runs)
            .unwrap_or_default()
            .lines()
            .count(),
        2,
        "the second Stop point did not run"
    );
    assert!(
        hook_blocks(&session, "follow-up").await.is_empty(),
        "an empty-reason Stop block wrote a follow-up"
    );
    let starts = events(&session)
        .await
        .iter()
        .filter(|event| matches!(event.kind, EventKind::TurnStart(_)))
        .count();
    assert_eq!(starts, 2, "an empty-reason Stop block started a turn");
    env.close().await;
}

/// A4: the follow-up carries the block reasons first, then the contexts, in
/// catalog order.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stop_follow_up_carries_the_block_reason_then_the_context() {
    let env = Env::new().await;
    env.sources.set(document(serde_json::json!({
        "Stop": [{
            "hooks": [
                {
                    "type": "command",
                    "command": "printf '{\"decision\":\"block\",\"reason\":\"stop reason\"}'",
                },
                {
                    "type": "command",
                    "command": "printf '{\"hookSpecificOutput\":{\"additionalContext\":\"stop context\"}}'",
                },
            ],
        }],
    })));
    let session = env.session().await;
    turn(&session, "first").await;
    let blocks = wait_blocks(&session, "follow-up", 1).await;
    let text = &blocks[0];
    let reason = text.find("stop reason").expect("the block reason");
    let context = text.find("stop context").expect("the context");
    assert!(reason < context, "the reason comes first: {text:?}");
    env.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_async_handler_does_not_delay_the_turn_or_the_next_point() {
    let env = Env::new().await;
    let late = env.path("late");
    let now = env.path("now");
    let done = env.path("now.done");
    env.sources.set(document(serde_json::json!({
        "PostToolUse": [
            command(serde_json::json!({
                "type": "command", "async": true,
                "command": format!("{}; printf 'late' > {}", sleep(1500), late.display()),
            })),
            command(serde_json::json!({
                "type": "command",
                "command": format!(
                    "printf 'now' > {} && touch {}; printf '{{\"hookSpecificOutput\":{{\"additionalContext\":\"context\"}}}}'",
                    now.display(),
                    done.display(),
                ),
            })),
        ],
    })));
    let session = env.session().await;
    let note = env.path("work/note.txt");
    std::fs::write(&note, "note\n").unwrap();
    let started = std::time::Instant::now();
    turn(
        &session,
        &format!("tool:read {{\"path\":{:?}}}", note.to_str().unwrap()),
    )
    .await;
    assert!(started.elapsed() < Duration::from_millis(1200));
    for _ in 0..100 {
        if done.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        done.exists(),
        "the PostToolUse hook did not write its marker"
    );
    assert_eq!(std::fs::read_to_string(&now).unwrap(), "now");
    assert!(!late.exists(), "the async handler is still running");
    let blocks = wait_blocks(&session, "post-tool", 1).await;
    assert_eq!(blocks.len(), 1, "the async handler writes no context");
    tokio::time::sleep(Duration::from_millis(2000)).await;
    assert_eq!(std::fs::read_to_string(&late).unwrap(), "late");
    assert_eq!(hook_blocks(&session, "post-tool").await.len(), 1);
    env.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn permission_request_answers_through_approval() {
    let env = Env::new().await;
    env.sources.set(document(serde_json::json!({
        "PermissionRequest": [command(serde_json::json!({
            "type": "command",
            "command": "printf '{\"hookSpecificOutput\":{\"decision\":{\"behavior\":\"allow\"}}}'",
        }))],
    })));
    let session = env.session_with(PermissionPreset::WorkspaceWrite).await;
    turn(&session, "tool:exec {\"command\":\"true\"}").await;
    assert!(
        wait_event(&session, |kind| match kind {
            EventKind::ApprovalRespond(data) => Some(data.decision),
            _ => None,
        })
        .await
            == ApprovalDecision::Allow
    );
    assert_eq!(last_result(&session).await.is_error, None);

    env.sources.set(document(serde_json::json!({
        "PermissionRequest": [command(serde_json::json!({
            "type": "command",
            "command": "printf '{\"hookSpecificOutput\":{\"decision\":{\"behavior\":\"deny\"}}}'",
        }))],
    })));
    turn(&session, "tool:exec {\"command\":\"true\"}").await;
    assert_eq!(last_result(&session).await.is_error, Some(true));
    env.close().await;
}

/// A5: a failed `PermissionRequest` handler follows its `onFailure`. `turn`
/// returns only after the approval is answered and the turn ended, so the
/// hook has finished and its decision is committed before the test reads it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_permission_request_with_block_denies_the_approval() {
    let env = Env::new().await;
    let failed = env.path("blocked-failure");
    env.sources.set(document(serde_json::json!({
        "PermissionRequest": [command(serde_json::json!({
            "type": "command",
            "command": format!("printf 'failed' > {}; exit 1", failed.display()),
            "onFailure": "block",
        }))],
    })));
    let session = env.session_with(PermissionPreset::WorkspaceWrite).await;
    turn(&session, "tool:exec {\"command\":\"true\"}").await;
    assert!(failed.exists(), "the hook command did not run");
    assert_eq!(
        wait_event(&session, |kind| match kind {
            EventKind::ApprovalRespond(data) => Some(data.decision),
            _ => None,
        })
        .await,
        ApprovalDecision::Deny
    );
    assert_eq!(last_result(&session).await.is_error, Some(true));
    env.close().await;
}

/// A5: `onFailure: continue` leaves a failed `PermissionRequest` with no
/// opinion. A second handler at the same point answers Allow, so the hook's
/// own answer is what the test reads: A.5 turns no deny plus an allow into
/// allow, and a failure counted as a deny would win the combination instead.
/// `turn` returns only after the approval is answered and the turn ended.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_permission_request_with_continue_answers_allow() {
    let env = Env::new().await;
    let failed = env.path("continued-failure");
    env.sources.set(document(serde_json::json!({
        "PermissionRequest": [{
            "hooks": [
                {
                    "type": "command",
                    "command": format!("printf 'failed' > {}; exit 1", failed.display()),
                    "onFailure": "continue",
                },
                {
                    "type": "command",
                    "command": "printf '{\"hookSpecificOutput\":{\"decision\":{\"behavior\":\"allow\"}}}'",
                },
            ],
        }],
    })));
    let session = env.session_with(PermissionPreset::WorkspaceWrite).await;
    turn(&session, "tool:exec {\"command\":\"true\"}").await;
    assert!(failed.exists(), "the failing handler did not run");
    let answered: Vec<ApprovalDecision> = events(&session)
        .await
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::ApprovalRespond(data) => Some(data.decision),
            _ => None,
        })
        .collect();
    assert_eq!(answered, vec![ApprovalDecision::Allow]);
    assert_eq!(last_result(&session).await.is_error, None);
    env.close().await;
}

/// A5: a handler that times out with `onFailure: block` denies too.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_timed_out_permission_request_with_block_denies_the_approval() {
    let env = Env::new().await;
    env.sources.set(document(serde_json::json!({
        "PermissionRequest": [command(serde_json::json!({
            "type": "command",
            "command": "sleep 5",
            "timeout": 1,
            "onFailure": "block",
        }))],
    })));
    let session = env.session_with(PermissionPreset::WorkspaceWrite).await;
    let started = std::time::Instant::now();
    turn(&session, "tool:exec {\"command\":\"true\"}").await;
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "the hook was not cut off"
    );
    assert_eq!(
        wait_event(&session, |kind| match kind {
            EventKind::ApprovalRespond(data) => Some(data.decision),
            _ => None,
        })
        .await,
        ApprovalDecision::Deny
    );
    assert_eq!(last_result(&session).await.is_error, Some(true));
    env.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_user_answer_cancels_the_hook_and_does_not_delay_the_next_point() {
    let env = Env::new().await;
    let late = env.path("late-hook");
    let failure = env.path("failure-marker");
    env.sources.set(document(serde_json::json!({
        "PermissionRequest": [command(serde_json::json!({
            "type": "command",
            "command": format!("{}; printf 'late' > {}", sleep(3000), late.display()),
        }))],
        "PostToolUseFailure": [command(serde_json::json!({
            "type": "command",
            "command": format!("printf 'failed' > {}", failure.display()),
        }))],
    })));
    let session = env.session_with(PermissionPreset::WorkspaceWrite).await;
    session
        .submit(
            "tool:exec {\"command\":\"true\"}".into(),
            InputMode::Queue,
            "main",
        )
        .await
        .unwrap();
    let approval_id = wait_event(&session, |kind| match kind {
        EventKind::ApprovalRequest(data) => Some(data.approval_id.clone()),
        _ => None,
    })
    .await;
    session
        .service::<dim_agent_capabilities::approval::Approval>("main")
        .unwrap()
        .respond(&approval_id, ApprovalDecision::Deny)
        .await
        .unwrap();
    // A cancelled PermissionRequest must not hold the branch's next point:
    // the hook sleeps three seconds and this asks for the failure marker
    // within two.
    for _ in 0..200 {
        if failure.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        failure.exists(),
        "the next hook point waited for the PermissionRequest hook"
    );
    session.idle("main").await.unwrap();
    tokio::time::sleep(Duration::from_millis(3500)).await;
    assert!(
        !late.exists(),
        "the user's answer did not terminate the running hook"
    );
    let approved = session
        .local("main", ..)
        .await
        .unwrap()
        .iter()
        .filter(|event| matches!(event.kind, EventKind::ApprovalRespond(_)))
        .count();
    assert_eq!(approved, 1, "only the user's answer is committed");
    env.close().await;
}

/// F11, the reverse of the test above: an approval's run is a task spawned
/// when the request is folded, so its `PermissionRequest` command starts
/// while an earlier `PostToolUse` hook on the same branch still runs. The
/// slow hook writes its end marker only after the test releases it, so the
/// order needs no timing race.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_permission_request_does_not_wait_for_a_running_post_tool_hook() {
    let env = Env::new().await;
    let order = env.path("order");
    let started = env.path("post-tool-started");
    let release = env.path("post-tool-release");
    env.sources.set(document(serde_json::json!({
        "PostToolUse": [{
            "matcher": "read|Read",
            "hooks": [{
                "type": "command",
                "command": format!(
                    "printf 'started' > {}; while [ ! -f {} ]; do sleep 0.05; done; printf 'post-tool\\n' >> {}",
                    started.display(),
                    release.display(),
                    order.display(),
                ),
            }],
        }],
        "PermissionRequest": [command(serde_json::json!({
            "type": "command",
            "command": format!("printf 'permission\\n' >> {}", order.display()),
        }))],
    })));
    let session = env.session_with(PermissionPreset::WorkspaceWrite).await;
    let note = env.path("work/note.txt");
    std::fs::write(&note, "note\n").unwrap();
    turn(
        &session,
        &format!("tool:read {{\"path\":{:?}}}", note.to_str().unwrap()),
    )
    .await;
    for _ in 0..500 {
        if started.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(started.exists(), "the PostToolUse hook did not start");
    session
        .submit(
            "tool:exec {\"command\":\"true\"}".into(),
            InputMode::Queue,
            "main",
        )
        .await
        .unwrap();
    let approval_id = wait_event(&session, |kind| match kind {
        EventKind::ApprovalRequest(data) => Some(data.approval_id.clone()),
        _ => None,
    })
    .await;
    // The release file does not exist yet, so the slow hook cannot have
    // ended: a permission marker now proves its point did not wait for it.
    for _ in 0..500 {
        if std::fs::read_to_string(&order).is_ok_and(|text| text.contains("permission")) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let seen = std::fs::read_to_string(&order).unwrap_or_default();
    assert!(
        seen.contains("permission"),
        "the PermissionRequest hook waited for the running PostToolUse hook: {seen:?}"
    );
    assert!(
        !seen.contains("post-tool"),
        "the PostToolUse hook ended before the permission marker: {seen:?}"
    );
    session
        .service::<dim_agent_capabilities::approval::Approval>("main")
        .unwrap()
        .respond(&approval_id, ApprovalDecision::Deny)
        .await
        .unwrap();
    std::fs::write(&release, "").unwrap();
    for _ in 0..500 {
        if std::fs::read_to_string(&order).is_ok_and(|text| text.contains("post-tool")) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        std::fs::read_to_string(&order).unwrap_or_default(),
        "permission\npost-tool\n"
    );
    session.idle("main").await.unwrap();
    env.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn notification_runs_on_an_approval_and_writes_nothing() {
    let env = Env::new().await;
    let seen = env.path("notified");
    env.sources.set(document(serde_json::json!({
        "Notification": [command(serde_json::json!({
            "type": "command",
            "command": format!("cat >> {}", seen.display()),
        }))],
    })));
    let session = env.session_with(PermissionPreset::WorkspaceWrite).await;
    session
        .submit(
            "tool:exec {\"command\":\"true\"}".into(),
            InputMode::Queue,
            "main",
        )
        .await
        .unwrap();
    let approval_id = wait_event(&session, |kind| match kind {
        EventKind::ApprovalRequest(data) => Some(data.approval_id.clone()),
        _ => None,
    })
    .await;
    session
        .service::<dim_agent_capabilities::approval::Approval>("main")
        .unwrap()
        .respond(&approval_id, ApprovalDecision::Deny)
        .await
        .unwrap();
    session.idle("main").await.unwrap();
    for _ in 0..500 {
        let bytes = std::fs::read_to_string(&seen).unwrap_or_default();
        if let Ok(payload) = serde_json::from_str::<serde_json::Value>(bytes.trim()) {
            assert_eq!(payload["notification_type"], "permission_prompt");
            assert_eq!(payload["hook_event_name"], "Notification");
            assert!(hook_blocks(&session, "post-tool").await.is_empty());
            assert!(hook_blocks(&session, "follow-up").await.is_empty());
            env.close().await;
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("Notification did not run");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_hook_sees_its_environment_and_no_plugin_root() {
    let env = Env::new().await;
    let seen = env.path("environment");
    let stdin = env.path("payload");
    env.sources.set(document(serde_json::json!({
        "SessionStart": [command(serde_json::json!({
            "type": "command",
            "command": format!("env > {}; cat > {}; printf 'context'", seen.display(), stdin.display()),
        }))],
    })));
    let session = env.session().await;
    wait_blocks(&session, "session-start", 1).await;
    for _ in 0..500 {
        if stdin.exists() && !std::fs::read_to_string(&stdin).unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let environment = std::fs::read_to_string(&seen).unwrap();
    let line = |name: &str| {
        environment
            .lines()
            .find(|line| line.starts_with(&format!("{name}=")))
            .map(str::to_owned)
    };
    assert_eq!(
        line("DIM_SESSION_ID"),
        Some(format!("DIM_SESSION_ID={}", session.id()))
    );
    assert_eq!(
        line("DIM_PROJECT_DIR"),
        Some(format!(
            "DIM_PROJECT_DIR={}",
            env.dir.join("work").display()
        ))
    );
    assert_eq!(
        line("DIM_HOOK_EVENT"),
        Some("DIM_HOOK_EVENT=SessionStart".into())
    );
    assert_eq!(
        line("DIMCODE_HOME"),
        Some(format!("DIMCODE_HOME={}", env.dir.join("home").display()))
    );
    assert_eq!(line("CLAUDE_PLUGIN_ROOT"), None);
    let payload: serde_json::Value =
        serde_json::from_str(std::fs::read_to_string(&stdin).unwrap().trim()).unwrap();
    assert_eq!(payload["hook_event_name"], "SessionStart");
    assert_eq!(payload["source"], "startup");
    assert_eq!(payload["dim_source"], "user");
    assert_eq!(payload["permission_mode"], "full-access");
    assert_eq!(payload["branch_id"], "main");
    assert_eq!(payload["session_id"], session.id());
    env.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn identical_commands_at_one_point_run_once() {
    let env = Env::new().await;
    let runs = env.path("runs");
    let repeat = format!(
        "{}; printf '{{\"hookSpecificOutput\":{{\"additionalContext\":\"context\"}}}}'",
        marker_command(&runs, "run\n")
    );
    env.sources.set(document(serde_json::json!({
        "PostToolUse": [
            command(serde_json::json!({"type": "command", "command": repeat})),
            command(serde_json::json!({"type": "command", "command": repeat})),
        ],
    })));
    let session = env.session().await;
    let note = env.path("work/note.txt");
    std::fs::write(&note, "note\n").unwrap();
    turn(
        &session,
        &format!("tool:read {{\"path\":{:?}}}", note.to_str().unwrap()),
    )
    .await;
    wait_blocks(&session, "post-tool", 1).await;
    assert_eq!(std::fs::read_to_string(&runs).unwrap(), "run\n");
    assert_eq!(hook_blocks(&session, "post-tool").await.len(), 1);
    env.close().await;
}

/// A.2: user and project handlers have no plugin directory, so their
/// identical commands at one point still run once, across sources.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_identical_command_in_the_user_and_project_sources_runs_once() {
    let env = Env::new().await;
    let runs = env.path("runs");
    let repeat = format!(
        "{}; printf '{{\"hookSpecificOutput\":{{\"additionalContext\":\"context\"}}}}'",
        marker_command(&runs, "run\n")
    );
    env.sources.set(document(serde_json::json!({
        "PostToolUse": [
            command(serde_json::json!({"type": "command", "command": repeat})),
        ],
    })));
    env.sources.set_project(
        document(serde_json::json!({
            "PostToolUse": [
                command(serde_json::json!({"type": "command", "command": repeat.clone()})),
            ],
        })),
        true,
    );
    let session = env.session().await;
    let note = env.path("work/note.txt");
    std::fs::write(&note, "note\n").unwrap();
    turn(
        &session,
        &format!("tool:read {{\"path\":{:?}}}", note.to_str().unwrap()),
    )
    .await;
    wait_blocks(&session, "post-tool", 1).await;
    assert_eq!(std::fs::read_to_string(&runs).unwrap(), "run\n");
    assert_eq!(hook_blocks(&session, "post-tool").await.len(), 1);
    env.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hook_output_is_dropped_after_an_interrupt() {
    let env = Env::new().await;
    env.sources.set(document(serde_json::json!({
        "PostToolUse": [command(serde_json::json!({
            "type": "command",
            "command": format!("{}; printf 'late context'", sleep(400)),
        }))],
    })));
    let session = env.session().await;
    let note = env.path("work/note.txt");
    std::fs::write(&note, "note\n").unwrap();
    session
        .submit(
            format!("tool:read {{\"path\":{:?}}}", note.to_str().unwrap()),
            InputMode::Queue,
            "main",
        )
        .await
        .unwrap();
    wait_event(&session, |kind| match kind {
        EventKind::ToolResult(_) => Some(()),
        _ => None,
    })
    .await;
    session.interrupt("main", "stop".into()).unwrap();
    let _ = session.idle("main").await;
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert!(
        hook_blocks(&session, "post-tool").await.is_empty(),
        "an interrupt drops the output"
    );
    env.close().await;
}

/// §3.4 drops an output only for operator input the branch *consumed*. An
/// input queued during a turn is not consumed until the turn ends, so a slow
/// `PostToolUse` hook still writes its context. The model holds the turn's
/// second request, so the turn cannot end while the test acts.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_queued_operator_input_does_not_drop_the_turns_post_tool_output() {
    let env = Env::new().await;
    let (entered, gate, finished) = (
        env.path("queued-entered"),
        env.path("queued-gate"),
        env.path("queued-finished"),
    );
    env.sources.set(document(serde_json::json!({
        "PostToolUse": [command(serde_json::json!({
            "type": "command",
            "command": format!(
                "printf 'entered' > {}; while [ ! -f {} ]; do sleep 0.05; done; printf 'finished' > {}; printf '{{\"hookSpecificOutput\":{{\"additionalContext\":\"post-tool context\"}}}}'",
                entered.display(),
                gate.display(),
                finished.display(),
            ),
        }))],
    })));
    let release = env.model.hold(2);
    let session = env.session().await;
    let note = env.path("work/note.txt");
    std::fs::write(&note, "note\n").unwrap();
    session
        .submit(
            format!("tool:read {{\"path\":{:?}}}", note.to_str().unwrap()),
            InputMode::Queue,
            "main",
        )
        .await
        .unwrap();
    wait_file(&entered, "the PostToolUse hook did not start").await;
    session
        .submit("queued".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    std::fs::write(&gate, "").unwrap();
    wait_file(&finished, "the PostToolUse hook did not end").await;
    let blocks = wait_blocks(&session, "post-tool", 1).await;
    assert_eq!(blocks.len(), 1, "{blocks:?}");
    let _ = release.send(());
    session.idle("main").await.unwrap();
    env.close().await;
}

/// §3.4: a source input is not the operator's, even when it carries `user`
/// content. Every source input starts with its `<dim-…>` marker block
/// (session access A.2), and such an input drops nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_source_input_with_user_content_does_not_drop_the_output() {
    let env = Env::new().await;
    let (entered, gate, finished) = (
        env.path("source-entered"),
        env.path("source-gate"),
        env.path("source-finished"),
    );
    env.sources.set(document(serde_json::json!({
        "PostToolUse": [command(serde_json::json!({
            "type": "command",
            "command": format!(
                "printf 'entered' > {}; while [ ! -f {} ]; do sleep 0.05; done; printf 'finished' > {}; printf '{{\"hookSpecificOutput\":{{\"additionalContext\":\"post-tool context\"}}}}'",
                entered.display(),
                gate.display(),
                finished.display(),
            ),
        }))],
    })));
    let release = env.model.hold(2);
    let session = env.session().await;
    let note = env.path("work/note.txt");
    std::fs::write(&note, "note\n").unwrap();
    session
        .submit(
            format!("tool:read {{\"path\":{:?}}}", note.to_str().unwrap()),
            InputMode::Queue,
            "main",
        )
        .await
        .unwrap();
    wait_file(&entered, "the PostToolUse hook did not start").await;
    session
        .input(
            InputMessageData {
                blocks: vec![InputBlock::Raw {
                    text: "<dim-scheduler kind=\"wake\">\nsource note\n</dim-scheduler>".into(),
                    key: None,
                    retire: false,
                }],
                user: Some(UserInput {
                    content: vec![ContentBlock::Text {
                        text: "scheduler note".into(),
                    }],
                }),
                mode: Some(InputMode::Queue),
                ..InputMessageData::default()
            },
            "main",
        )
        .await
        .unwrap();
    std::fs::write(&gate, "").unwrap();
    wait_file(&finished, "the PostToolUse hook did not end").await;
    let blocks = wait_blocks(&session, "post-tool", 1).await;
    assert_eq!(blocks.len(), 1, "{blocks:?}");
    let _ = release.send(());
    session.idle("main").await.unwrap();
    env.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_second_session_start_does_not_run_the_hooks_again() {
    let env = Env::new().await;
    let runs = env.path("starts");
    env.sources.set(document(serde_json::json!({
        "SessionStart": [command(serde_json::json!({
            "type": "command",
            "command": format!("{}; printf 'context'", marker_command(&runs, "start\n")),
        }))],
    })));
    let session = env.session().await;
    wait_blocks(&session, "session-start", 1).await;
    let id = session.id().to_owned();
    session.close().await.unwrap();
    let restored = env.agent.restore_session(id).await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(std::fs::read_to_string(&runs).unwrap(), "start\n");
    assert_eq!(hook_blocks(&restored, "session-start").await.len(), 1);
    env.close().await;
}

async fn last_result(session: &Session) -> ToolResult {
    session
        .history("main", None)
        .await
        .unwrap()
        .iter()
        .rev()
        .find_map(|event| match &event.kind {
            EventKind::ToolResult(data) => Some(data.results[0].result.clone()),
            _ => None,
        })
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_session_start_block_is_still_in_context_after_compaction() {
    let env = Env::with(
        Arc::new(Model {
            exhaust: true,
            ..Model::default()
        }),
        Some(Arc::new(Small)),
    )
    .await;
    env.sources.set(document(serde_json::json!({
        "SessionStart": [command(serde_json::json!({
            "type": "command", "command": "printf 'start context'",
        }))],
    })));
    let session = env.session().await;
    let blocks = wait_blocks(&session, "session-start", 1).await;
    assert!(blocks[0].contains("start context"), "{blocks:?}");
    session
        .submit("x".repeat(4_000), InputMode::Queue, "main")
        .await
        .unwrap();
    let _ = session.idle("main").await;
    assert!(
        session
            .history("main", None)
            .await
            .unwrap()
            .iter()
            .any(|event| matches!(event.kind, EventKind::ContextCompaction(_))),
        "the turn compacted"
    );
    // The retry after the recovery compaction is the request the model saw.
    let requests = env.model.requests();
    assert_eq!(requests.len(), 1);
    let text = serde_json::to_string(&requests[0].messages).unwrap();
    assert!(text.contains("start context"), "{text}");
    env.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_project_source_runs_nothing_until_the_user_enables_it() {
    let env = Env::new().await;
    let runs = env.path("project-runs");
    let project = document(serde_json::json!({
        "SessionStart": [command(serde_json::json!({
            "type": "command",
            "command": format!(
                "{}; printf 'project context'",
                marker_command(&runs, "run\n")
            ),
        }))],
    }));
    env.sources.set_project(project.clone(), false);
    let session = env.session().await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!runs.exists(), "a disabled project source runs nothing");
    assert!(hook_blocks(&session, "session-start").await.is_empty());

    env.sources.set_project(project, true);
    let session = env.session().await;
    let blocks = wait_blocks(&session, "session-start", 1).await;
    assert!(blocks[0].contains("project context"), "{blocks:?}");
    assert_eq!(std::fs::read_to_string(&runs).unwrap(), "run\n");
    env.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn disable_all_hooks_turns_every_source_off() {
    let env = Env::new().await;
    let runs = env.path("all-off");
    let handler = serde_json::json!({
        "type": "command",
        "command": format!("{}; printf 'context'", marker_command(&runs, "run\n")),
    });
    env.sources.set(
        serde_json::from_value(serde_json::json!({
            "disableAllHooks": true,
            "hooks": {"SessionStart": [{"hooks": [handler]}]},
        }))
        .unwrap(),
    );
    let session = env.session().await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!runs.exists());
    assert!(hook_blocks(&session, "session-start").await.is_empty());
    env.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_plugin_source_runs_like_any_other_source() {
    let env = Env::new().await;
    let plugin = env.path("plugin");
    std::fs::create_dir_all(&plugin).unwrap();
    let seen = env.path("plugin-environment");
    let payload_file = env.path("plugin-payload");
    let user_seen = env.path("user-environment");
    env.sources.set(document(serde_json::json!({
        "SessionStart": [
            command(serde_json::json!({
                "type": "command",
                "command": format!("printf 'user context'; env > {}", user_seen.display()),
            })),
            command(serde_json::json!({"type": "command", "command": "printf 'shared'"})),
        ],
    })));
    env.sources.set_plugin(
        "kit",
        &plugin,
        document(serde_json::json!({
            "SessionStart": [
                command(serde_json::json!({
                    "type": "command",
                    "command": format!(
                        "printf 'plugin context'; env > {}; cat > {}",
                        seen.display(),
                        payload_file.display()
                    ),
                })),
                command(serde_json::json!({"type": "command", "command": "printf 'shared'"})),
            ],
        })),
    );
    let session = env.session().await;
    let blocks = wait_blocks(&session, "session-start", 4).await;
    assert_eq!(blocks.len(), 4, "{blocks:?}");
    assert!(blocks[0].contains("user context"), "{blocks:?}");
    assert!(blocks[1].contains("shared"), "{blocks:?}");
    assert!(blocks[2].contains("plugin context"), "{blocks:?}");
    // The plugin's "shared" is a different command from the user's: it runs
    // with its own `DIM_PLUGIN_ROOT` (A.2).
    assert!(blocks[3].contains("shared"), "{blocks:?}");

    // The plugin's command reads its directory; the user's reads none.
    let environment = std::fs::read_to_string(&seen).unwrap();
    assert!(
        environment
            .lines()
            .any(|line| line == format!("DIM_PLUGIN_ROOT={}", plugin.display())),
        "{environment}"
    );
    assert!(
        !environment
            .lines()
            .any(|line| line.starts_with("CLAUDE_PLUGIN_ROOT=")),
        "{environment}"
    );
    let user_environment = std::fs::read_to_string(&user_seen).unwrap();
    assert!(
        !user_environment
            .lines()
            .any(|line| line.starts_with("DIM_PLUGIN_ROOT=")),
        "{user_environment}"
    );
    let payload: serde_json::Value =
        serde_json::from_str(std::fs::read_to_string(&payload_file).unwrap().trim()).unwrap();
    assert_eq!(payload["dim_source"], "plugin:kit");

    // `disableAllHooks` in the user document turns the plugin source off too.
    let mut off = document(serde_json::json!({
        "SessionStart": [command(serde_json::json!({
            "type": "command", "command": "printf 'user context'"
        }))],
    }));
    off.disable_all_hooks = true;
    env.sources.set(off);
    let second = env.session().await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(hook_blocks(&second, "session-start").await.is_empty());
    env.close().await;
}

/// A.2: `DIM_PLUGIN_ROOT` gives the same command string different meanings in
/// two plugins, so both handlers run at one point, each with its own root.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn identical_commands_from_two_plugins_run_with_their_own_roots() {
    let env = Env::new().await;
    let runs = env.path("runs");
    let plugins: Vec<(&str, PathBuf)> = ["alpha", "beta"]
        .into_iter()
        .map(|id| {
            let dir = env.path(id);
            std::fs::create_dir_all(dir.join("scripts")).unwrap();
            let script = dir.join("scripts").join("check.sh");
            std::fs::write(
                &script,
                format!(
                    "printf '{id} context %s' \"$DIM_PLUGIN_ROOT\"; \
                     printf '{id} %s\\n' \"$DIM_PLUGIN_ROOT\" >> {}",
                    runs.display()
                ),
            )
            .unwrap();
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
            (id, dir)
        })
        .collect();
    for (id, dir) in &plugins {
        env.sources.set_plugin(
            id,
            dir.as_path(),
            document(serde_json::json!({
                "SessionStart": [command(serde_json::json!({
                    "type": "command",
                    "command": "$DIM_PLUGIN_ROOT/scripts/check.sh",
                }))],
            })),
        );
    }
    let session = env.session().await;
    let blocks = wait_blocks(&session, "session-start", 2).await;
    assert_eq!(blocks.len(), 2, "{blocks:?}");
    for (index, (id, dir)) in plugins.iter().enumerate() {
        let context = format!("{id} context {}", dir.display());
        assert!(blocks[index].contains(&context), "{blocks:?}");
    }
    wait_file(&runs, "the marker file").await;
    let mut lines: Vec<String> = std::fs::read_to_string(&runs)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect();
    lines.sort();
    let mut expected: Vec<String> = plugins
        .iter()
        .map(|(id, dir)| format!("{id} {}", dir.display()))
        .collect();
    expected.sort();
    assert_eq!(lines, expected);
    env.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn only_the_follow_ups_turn_reports_stop_hook_active() {
    let env = Env::new().await;
    let seen = env.path("payloads");
    // Blocks once, then lets the driven turn end: the case reads the compact
    // payload the hook receives on stdin.
    let script = format!(
        "sleep 0.3; payload=$(cat); printf '%s\\n' \"$payload\" >> {seen}; \
         case \"$payload\" in *'\"stop_hook_active\":false'*) printf 'again' >&2; exit 2;; esac",
        seen = seen.display()
    );
    env.sources.set(document(serde_json::json!({
        "Stop": [command(serde_json::json!({"type": "command", "command": script}))],
    })));
    let session = env.session().await;
    session
        .submit("go".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    // A second producer writes its own input right after the first turn ends,
    // before the Stop hook has finished. It is not a follow-up.
    wait_event(&session, |kind| match kind {
        EventKind::TurnEnd(_) => Some(()),
        _ => None,
    })
    .await;
    session
        .input(
            InputMessageData {
                blocks: vec![dim_agent_capabilities::context_block::raw(
                    "<other/>".into(),
                    None,
                )],
                mode: Some(InputMode::Queue),
                ..InputMessageData::default()
            },
            "main",
        )
        .await
        .unwrap();
    session.idle("main").await.unwrap();
    // Three turns run; each Stop hook sleeps before it records.
    let payloads = || -> Vec<serde_json::Value> {
        std::fs::read_to_string(&seen)
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    };
    for _ in 0..500 {
        if payloads().len() >= 3 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
    let payloads = payloads();
    let active: Vec<bool> = payloads
        .iter()
        .map(|payload| payload["stop_hook_active"].as_bool().unwrap())
        .collect();
    // The operator's turn and the other producer's turn are not follow-ups;
    // every turn a Hooks follow-up drives is.
    assert!(active.len() >= 3, "{payloads:?}");
    assert_eq!(active[..2], [false, false], "{payloads:?}");
    assert!(active[2..].iter().all(|active| *active), "{payloads:?}");
    env.close().await;
}

/// Calls `session.input.withdraw` as Rpc does, through the Session.
async fn withdraw(
    session: &Session,
    params: serde_json::Value,
) -> std::result::Result<serde_json::Value, RpcError> {
    let methods = session
        .session_service::<SessionMethods>()
        .unwrap()
        .provider("session.input.withdraw")
        .unwrap();
    let accepted = methods
        .accept(
            "session.input.withdraw".into(),
            params,
            CancellationToken::new(),
            |_| {},
        )
        .unwrap();
    accepted.reply.await.unwrap()
}

fn app_error(error: &RpcError) -> &str {
    &error.data.as_ref().unwrap().app_error
}

/// Appendix D 6: a blocked steer writes neither the withdrawal nor the new input, and
/// the old input is applied later.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_blocked_steer_writes_nothing_and_the_old_input_still_applies() {
    let env = Env::new().await;
    env.sources.set(document(serde_json::json!({
        "UserPromptSubmit": [command(serde_json::json!({
            "type": "command", "command": "echo 'not happening' >&2; exit 2",
        }))],
    })));
    let session = env.session().await;
    let release = env.model.hold(1);
    session
        .submit("first".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    wait_event(&session, |kind| {
        matches!(kind, EventKind::TurnStart(_)).then_some(())
    })
    .await;
    let old = session
        .submit("old".into(), InputMode::Queue, "main")
        .await
        .unwrap();

    let refused = withdraw(
        &session,
        serde_json::json!({"sessionId": session.id(), "branchId": "main",
            "inputId": old.id, "steer": {"content": [{"type": "text", "text": "new"}]}}),
    )
    .await
    .expect_err("a blocked steer fails");
    assert_eq!(app_error(&refused), "input_blocked");
    assert_eq!(refused.message, "Hook user: not happening");
    let local = session.local("main", ..).await.unwrap();
    assert!(
        !local.iter().any(|event| matches!(
            &event.kind,
            EventKind::InputWithdraw(data) if data.input_id == old.id
        )),
        "a blocked steer wrote its withdrawal"
    );
    assert!(
        local.iter().all(|event| !matches!(&event.kind,
            EventKind::InputMessage(data)
                if data.user.as_ref().is_some_and(|user| user.content
                    == vec![ContentBlock::Text { text: "new".into() }]))),
        "a blocked steer wrote the replacement input"
    );
    assert!(
        !local.iter().any(|event| matches!(
            &event.kind,
            EventKind::InputDeleted(data) if data.input_id == old.id
        )),
        "a blocked steer deleted the old input"
    );

    let _ = release.send(());
    let mut applied = false;
    for _ in 0..500 {
        applied = session
            .local("main", ..)
            .await
            .unwrap()
            .iter()
            .any(|event| {
                matches!(&event.kind,
                EventKind::InputApplied(data) if data.input_id == old.id)
            });
        if applied {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(applied, "the old input was not applied after the block");
    for _ in 0..500 {
        if last_request(&env.model).contains("old") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        last_request(&env.model).contains("old"),
        "the model did not see the old input"
    );
    env.close().await;
}

/// Appendix D 5: the hook's context is a text block after the operator's own content,
/// and the input carries no raw block, so it stays operator input (Goal A5
/// restarts its count on it).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_prompt_context_follows_the_operator_text_and_keeps_it_operator_input() {
    let env = Env::new().await;
    env.sources.set(document(serde_json::json!({
        "UserPromptSubmit": [command(serde_json::json!({
            "type": "command",
            "command": "printf '{\"hookSpecificOutput\":{\"additionalContext\":\"prompt context\"}}'",
        }))],
    })));
    let session = env.session().await;
    let reply = withdraw(
        &session,
        serde_json::json!({"sessionId": session.id(), "branchId": "main",
            "inputId": "unknown", "steer": {"content": [{"type": "text", "text": "new"}]}}),
    )
    .await
    .unwrap();
    assert_eq!(reply["status"], "notHeld", "{reply}");
    let steer = reply["steer"]["inputId"].as_str().unwrap();
    let event = session
        .local("main", ..)
        .await
        .unwrap()
        .into_iter()
        .find(|event| event.id == steer)
        .expect("the steer input was written");
    let EventKind::InputMessage(data) = &event.kind else {
        panic!("the steer input is an input message");
    };
    let user = data
        .user
        .as_ref()
        .expect("the steer keeps its user content");
    assert_eq!(
        user.content.len(),
        2,
        "the context is one text block after the operator's content: {user:?}"
    );
    let ContentBlock::Text { text: operator } = &user.content[0] else {
        panic!("the operator's block is text");
    };
    assert_eq!(operator, "new");
    let ContentBlock::Text { text: context } = &user.content[1] else {
        panic!("the context is text");
    };
    assert!(
        context.starts_with("<dim-hooks kind=\"prompt-submit\">"),
        "{context}"
    );
    assert!(context.contains("prompt context"), "{context}");
    assert!(
        data.blocks.is_empty(),
        "the input carries no raw block, so it stays operator input"
    );
    env.close().await;
}

/// Appendix D 5: only the operator-input entries call the checks, so an input another
/// owner writes (a Goal or Scheduler input) runs no prompt hook.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_source_input_runs_no_prompt_hook() {
    let env = Env::new().await;
    let runs = env.path("prompt-runs");
    env.sources.set(document(serde_json::json!({
        "UserPromptSubmit": [command(serde_json::json!({
            "type": "command", "command": marker_command(&runs, "ran\n"),
        }))],
    })));
    let session = env.session().await;
    session
        .input(
            InputMessageData {
                user: Some(UserInput {
                    content: vec![ContentBlock::Text {
                        text: "continue the goal".into(),
                    }],
                }),
                blocks: vec![dim_agent_capabilities::context_block::raw(
                    "<dim-goal kind=\"continuation\"/>".into(),
                    None,
                )],
                mode: Some(InputMode::Queue),
                ..InputMessageData::default()
            },
            "main",
        )
        .await
        .unwrap();
    session.idle("main").await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        !runs.exists(),
        "a Goal or Scheduler input runs no UserPromptSubmit hook"
    );
    env.close().await;
}

/// A5: the reason is the first block in catalog order, and the handlers after
/// it are terminated.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_handlers_after_the_first_block_are_terminated() {
    let env = Env::new().await;
    let gate = env.path("gate");
    let late = env.path("late-handler");
    env.sources.set(document(serde_json::json!({
        "UserPromptSubmit": [{
            "hooks": [
                {"type": "command", "command": "echo 'first block' >&2; exit 2"},
                {"type": "command", "command": format!(
                    "while [ ! -e {} ]; do sleep 0.05; done; printf ran > {}",
                    gate.display(), late.display())},
            ],
        }],
    })));
    let session = env.session().await;
    let refused = withdraw(
        &session,
        serde_json::json!({"sessionId": session.id(), "branchId": "main",
            "inputId": "unknown", "steer": {"content": [{"type": "text", "text": "new"}]}}),
    )
    .await
    .expect_err("a blocked steer fails");
    assert_eq!(refused.message, "Hook user: first block");
    // The gate releases the second handler, so a surviving one writes its
    // marker; termination is what keeps it from running.
    std::fs::write(&gate, "go").unwrap();
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(
        !late.exists(),
        "a handler after the first block was not terminated"
    );
    env.close().await;
}

/// A.5: each handler's result stays at its own catalog position when the
/// first finishes before the second, so both contexts arrive in catalog
/// order.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_proceeding_handlers_keep_their_contexts_in_catalog_order() {
    let env = Env::new().await;
    // The second handler waits for the first one's last act, so the first
    // result is ready first.
    let first_done = env.path("first-done");
    env.sources.set(document(serde_json::json!({
        "UserPromptSubmit": [{
            "hooks": [
                {"type": "command", "command": format!(
                    "printf '{{\"hookSpecificOutput\":{{\"additionalContext\":\"first context\"}}}}'; \
                     printf done > {}", first_done.display())},
                {"type": "command", "command": format!(
                    "while [ ! -e {} ]; do sleep 0.05; done; \
                     printf '{{\"hookSpecificOutput\":{{\"additionalContext\":\"second context\"}}}}'",
                    first_done.display())},
            ],
        }],
    })));
    let session = env.session().await;
    let reply = withdraw(
        &session,
        serde_json::json!({"sessionId": session.id(), "branchId": "main",
            "inputId": "unknown", "steer": {"content": [{"type": "text", "text": "new"}]}}),
    )
    .await
    .unwrap();
    let steer = reply["steer"]["inputId"].as_str().unwrap();
    let event = session
        .local("main", ..)
        .await
        .unwrap()
        .into_iter()
        .find(|event| event.id == steer)
        .expect("the steer input was written");
    let EventKind::InputMessage(data) = &event.kind else {
        panic!("the steer input is an input message");
    };
    let user = data
        .user
        .as_ref()
        .expect("the steer keeps its user content");
    assert_eq!(
        user.content.len(),
        3,
        "both contexts follow the operator's content: {user:?}"
    );
    let text = |index: usize| match &user.content[index] {
        ContentBlock::Text { text } => text.as_str(),
        block => panic!("block {index} is not text: {block:?}"),
    };
    assert_eq!(text(0), "new");
    assert!(text(1).contains("first context"), "{user:?}");
    assert!(text(2).contains("second context"), "{user:?}");
    env.close().await;
}

/// A.5: with handlers A, B and C in catalog order finishing in the order
/// A, C, B, each result is still read against its own handler: B's failure
/// blocks by B's `onFailure`, and the message names B's source.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failure_finishing_after_a_later_result_blocks_by_its_own_handler() {
    let env = Env::new().await;
    let first_done = env.path("first-done");
    let third_done = env.path("third-done");
    env.sources.set(document(serde_json::json!({
        "UserPromptSubmit": [command(serde_json::json!({
            "type": "command", "onFailure": "continue", "command": format!(
                "printf '{{\"hookSpecificOutput\":{{\"additionalContext\":\"first context\"}}}}'; \
                 printf done > {}", first_done.display()),
        }))],
    })));
    // B and C are plugin sources in registration order after the user's, so
    // the catalog order is the user's handler, B's, then C's.
    env.sources.set_plugin(
        "second",
        &env.path("plugin-second"),
        document(serde_json::json!({
            "UserPromptSubmit": [command(serde_json::json!({
                "type": "command", "onFailure": "block", "command": format!(
                    "while [ ! -e {first} ] || [ ! -e {third} ]; do sleep 0.05; done; exit 1",
                    first = first_done.display(), third = third_done.display()),
            }))],
        })),
    );
    env.sources.set_plugin(
        "third",
        &env.path("plugin-third"),
        document(serde_json::json!({
            "UserPromptSubmit": [command(serde_json::json!({
                "type": "command", "onFailure": "continue", "command": format!(
                    "while [ ! -e {} ]; do sleep 0.05; done; \
                     printf '{{\"hookSpecificOutput\":{{\"additionalContext\":\"third context\"}}}}'; \
                     printf done > {}",
                    first_done.display(), third_done.display()),
            }))],
        })),
    );
    let session = env.session().await;
    let refused = withdraw(
        &session,
        serde_json::json!({"sessionId": session.id(), "branchId": "main",
            "inputId": "unknown", "steer": {"content": [{"type": "text", "text": "new"}]}}),
    )
    .await
    .expect_err("the second handler blocks");
    assert_eq!(app_error(&refused), "input_blocked");
    assert_eq!(refused.message, "Hook plugin:second did not finish");
    assert!(
        session
            .local("main", ..)
            .await
            .unwrap()
            .iter()
            .all(|event| !matches!(&event.kind,
                EventKind::InputMessage(data) if data.user.as_ref().is_some_and(|user| matches!(
                    user.content.first(), Some(ContentBlock::Text { text }) if text == "new")))),
        "a blocked submission wrote the steer input"
    );
    env.close().await;
}

/// Runs `command` and waits until it has written `started` and its pid; the
/// handler then blocks until the test ends or the part unloads.
fn blocking_prompt(started: &Path, pid_file: &Path, on_failure: Option<&str>) -> serde_json::Value {
    let mut handler = serde_json::json!({
        "type": "command",
        "command": format!(
            "printf '%s' $$ > {pid}; printf started > {started}; sleep 30",
            pid = pid_file.display(),
            started = started.display(),
        ),
    });
    if let Some(on_failure) = on_failure {
        handler["onFailure"] = serde_json::json!(on_failure);
    }
    command(handler)
}

/// Starts a turn the model holds, queues a second input and withdraws it as a
/// `steer` in the background, returning the call and the hold's sender.
async fn steer_into_a_pending_prompt(
    env: &Env,
) -> (
    Session,
    tokio::sync::oneshot::Sender<()>,
    String,
    tokio::task::JoinHandle<std::result::Result<serde_json::Value, RpcError>>,
) {
    let session = env.session().await;
    let release = env.model.hold(1);
    session
        .submit("first".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    wait_event(&session, |kind| {
        matches!(kind, EventKind::TurnStart(_)).then_some(())
    })
    .await;
    let old = session
        .submit("old".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    let call = {
        let session = session.clone();
        let input_id = old.id.clone();
        tokio::spawn(async move {
            withdraw(
                &session,
                serde_json::json!({"sessionId": session.id(), "branchId": "main",
                    "inputId": input_id, "steer": {"content": [{"type": "text", "text": "new"}]}}),
            )
            .await
        })
    };
    (session, release, old.id.clone(), call)
}

/// Waits until `pid` names no live process, up to a bounded number of polling
/// steps.
async fn wait_gone(pid: u32) {
    for _ in 0..1000 {
        let alive = std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stderr(std::process::Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false);
        if !alive {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("process {pid} outlived the unload");
}

fn prompted_pid(path: &Path) -> u32 {
    std::fs::read_to_string(path)
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}

/// A.5 and A.6.2's reload row: a `UserPromptSubmit` check whose part unloads
/// mid-call settles by its handler's `onFailure: block`, so the entry refuses
/// the input and nothing is written.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unload_settles_a_pending_prompt_check_by_on_failure() {
    let env = Env::with_mounted_hooks(Arc::new(Model::default()), None).await;
    let started = env.path("prompt-started");
    let pid_file = env.path("prompt-pid");
    env.sources.set(document(serde_json::json!({
        "UserPromptSubmit": [blocking_prompt(&started, &pid_file, Some("block"))],
    })));
    let (session, release, old_id, call) = steer_into_a_pending_prompt(&env).await;
    wait_file(&started, "the prompt hook did not start").await;
    env.restart_hooks().await;
    let refused = call
        .await
        .unwrap()
        .expect_err("an unloaded check with onFailure block refuses the steer");
    assert_eq!(app_error(&refused), "input_blocked");
    assert_eq!(refused.message, "Hook user did not finish");
    let local = session.local("main", ..).await.unwrap();
    assert!(
        !local.iter().any(|event| matches!(
            &event.kind,
            EventKind::InputWithdraw(data) if data.input_id == old_id
        )),
        "a settled block wrote its withdrawal"
    );
    assert!(
        !local.iter().any(|event| matches!(
            &event.kind,
            EventKind::InputDeleted(data) if data.input_id == old_id
        )),
        "a settled block deleted the old input"
    );
    assert!(
        local.iter().all(|event| !matches!(&event.kind,
            EventKind::InputMessage(data)
                if data.user.as_ref().is_some_and(|user| user.content
                    == vec![ContentBlock::Text { text: "new".into() }]))),
        "a settled block wrote the replacement input"
    );
    wait_gone(prompted_pid(&pid_file)).await;
    let _ = release.send(());

    env.close().await;
}

/// A.5 and A.6.2's reload row: with the default `onFailure` the same unload
/// lets the entry write the input.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unload_with_the_default_on_failure_writes_the_input() {
    let env = Env::with_mounted_hooks(Arc::new(Model::default()), None).await;
    let started = env.path("prompt-started");
    let pid_file = env.path("prompt-pid");
    env.sources.set(document(serde_json::json!({
        "UserPromptSubmit": [blocking_prompt(&started, &pid_file, None)],
    })));
    let (session, release, old_id, call) = steer_into_a_pending_prompt(&env).await;
    wait_file(&started, "the prompt hook did not start").await;
    env.restart_hooks().await;
    let reply = call
        .await
        .unwrap()
        .expect("an unloaded check with the default onFailure proceeds");
    assert_eq!(reply["status"], "deleted", "{reply}");
    assert!(reply["steer"]["inputId"].is_string(), "{reply}");
    let local = session.local("main", ..).await.unwrap();
    assert!(
        local.iter().any(|event| matches!(
            &event.kind,
            EventKind::InputWithdraw(data) if data.input_id == old_id
        )),
        "the withdrawal was written"
    );
    assert!(
        local.iter().any(|event| matches!(&event.kind,
            EventKind::InputMessage(data)
                if data.user.as_ref().is_some_and(|user| user.content
                    == vec![ContentBlock::Text { text: "new".into() }]))),
        "the replacement input was written"
    );
    wait_gone(prompted_pid(&pid_file)).await;
    let _ = release.send(());

    env.close().await;
}

fn result_text(result: &ToolResult) -> &str {
    match &result.content {
        ToolContent::Text(text) => text,
        other => panic!("a text result was expected: {other:?}"),
    }
}

async fn approval_request_count(session: &Session) -> usize {
    events(session)
        .await
        .iter()
        .filter(|event| matches!(event.kind, EventKind::ApprovalRequest(_)))
        .count()
}

/// Answers the request `approval_id` on `main`.
async fn answer(session: &Session, approval_id: &str, decision: ApprovalDecision) {
    session
        .service::<dim_agent_capabilities::approval::Approval>("main")
        .unwrap()
        .respond(approval_id, decision)
        .await
        .unwrap();
}

/// Appendix D 2: under a full-access preset, a `PreToolUse` deny for one call
/// of three: that call writes no `approval/request` and never runs, its error
/// text is the hook's in the one batch `step/tool-result`, and the other two
/// run. Hooks writes no `step/tool-result` of its own.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_pre_tool_use_deny_fails_one_call_of_a_batch_and_runs_the_others() {
    let env = Env::new().await;
    env.sources.set(document(serde_json::json!({
        "PreToolUse": [command(serde_json::json!({
            "type": "command",
            "command": "input=$(cat); case \"$input\" in *ran-denied*) \
                        printf 'blocked by the guard' >&2; exit 2;; esac",
        }))],
    })));
    let session = env.session().await;
    let ran_one = env.path("ran-one");
    let ran_denied = env.path("ran-denied");
    let ran_three = env.path("ran-three");
    turn(
        &session,
        &format!(
            "tools: exec {{\"command\":\"printf one > {}\"}} | \
             exec {{\"command\":\"printf denied > {}\"}} | \
             exec {{\"command\":\"printf three > {}\"}}",
            ran_one.display(),
            ran_denied.display(),
            ran_three.display(),
        ),
    )
    .await;
    assert!(ran_one.exists(), "the first call did not run");
    assert!(ran_three.exists(), "the third call did not run");
    assert!(!ran_denied.exists(), "the denied call ran");
    let local = events(&session).await;
    let batches: Vec<&ToolResultData> = local
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::ToolResult(data) => Some(data),
            _ => None,
        })
        .collect();
    assert_eq!(
        batches.len(),
        1,
        "the calls did not settle in one batch result"
    );
    assert_eq!(
        batches[0].results.len(),
        3,
        "a call of the batch is missing"
    );
    let calls: Vec<ToolCall> = local
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::ToolCall(data) => Some(data.calls.clone()),
            _ => None,
        })
        .flatten()
        .collect();
    let result_of = |marker: &Path| {
        let call = calls
            .iter()
            .find(|call| {
                call.input
                    .get("command")
                    .and_then(|command| command.as_str())
                    .is_some_and(|command| command.contains(&marker.display().to_string()))
            })
            .unwrap_or_else(|| panic!("no call writes {}", marker.display()));
        batches[0]
            .results
            .iter()
            .find(|result| result.call_id == call.id)
            .expect("the call has a result")
            .result
            .clone()
    };
    let denied = result_of(&ran_denied);
    assert_eq!(denied.is_error, Some(true));
    assert_eq!(result_text(&denied), "Hook user: blocked by the guard");
    assert_eq!(result_of(&ran_one).is_error, None);
    assert_eq!(result_of(&ran_three).is_error, None);
    assert_eq!(
        approval_request_count(&session).await,
        0,
        "a full-access call asked for approval"
    );
    env.close().await;
}

/// Appendix D 3: under a preset that asks for `exec`, a `PreToolUse` `allow`
/// runs the call with no `approval/request`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_pre_tool_use_allow_runs_a_call_the_preset_would_ask_about() {
    let env = Env::new().await;
    env.sources.set(document(serde_json::json!({
        "PreToolUse": [command(serde_json::json!({
            "type": "command",
            "command": "printf '{\"hookSpecificOutput\":{\"permissionDecision\":\"allow\"}}'",
        }))],
    })));
    let session = env.session_with(PermissionPreset::WorkspaceWrite).await;
    let ran = env.path("allowed-ran");
    turn(
        &session,
        &format!(
            "tool:exec {{\"command\":\"printf ran > {}\"}}",
            ran.display()
        ),
    )
    .await;
    assert_eq!(
        approval_request_count(&session).await,
        0,
        "an allowed call asked for approval"
    );
    assert!(ran.exists(), "the allowed call did not run");
    assert_eq!(last_result(&session).await.is_error, None);
    env.close().await;
}

/// Appendix D 3: an `ask` from a hook at the same point produces an ordinary
/// `approval/request`, and the user's answer decides the call.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_pre_tool_use_ask_asks_the_user_like_any_other_ask() {
    let env = Env::new().await;
    env.sources.set(document(serde_json::json!({
        "PreToolUse": [command(serde_json::json!({
            "type": "command",
            "command": "printf '{\"hookSpecificOutput\":{\"permissionDecision\":\"ask\",\
                       \"permissionDecisionReason\":\"the guard is unsure\"}}'",
        }))],
    })));
    let session = env.session_with(PermissionPreset::WorkspaceWrite).await;
    let ran = env.path("asked-ran");
    session
        .submit(
            format!(
                "tool:exec {{\"command\":\"printf ran > {}\"}}",
                ran.display()
            ),
            InputMode::Queue,
            "main",
        )
        .await
        .unwrap();
    let approval_id = wait_event(&session, |kind| match kind {
        EventKind::ApprovalRequest(data) => Some(data.approval_id.clone()),
        _ => None,
    })
    .await;
    let reason = events(&session)
        .await
        .iter()
        .find_map(|event| match &event.kind {
            EventKind::ApprovalRequest(data) if data.approval_id == approval_id => {
                Some(data.reason.clone())
            }
            _ => None,
        })
        .unwrap();
    assert_eq!(reason, "Hook user: the guard is unsure");
    answer(&session, &approval_id, ApprovalDecision::Deny).await;
    session.idle("main").await.unwrap();
    assert_eq!(
        result_text(&last_result(&session).await),
        "Tool execution denied by user"
    );
    assert!(!ran.exists(), "a denied call ran");
    env.close().await;
}

/// Appendix D 3: a command-rule deny is not overridden, and no hook runs for
/// it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_command_rule_deny_runs_no_pre_tool_use_hook() {
    let env = Env::new().await;
    let ran = env.path("rule-hook-ran");
    env.sources.set(document(serde_json::json!({
        "PreToolUse": [command(serde_json::json!({
            "type": "command",
            "command": format!(
                "printf ran > {}; printf '{{\"hookSpecificOutput\":{{\"permissionDecision\":\"allow\"}}}}'",
                ran.display(),
            ),
        }))],
    })));
    env.rules.set(vec![CommandRule {
        command: "rm -rf".into(),
        matching: CommandMatch::Exact,
        decision: CommandRuleDecision::Deny,
    }]);
    let session = env.session_with(PermissionPreset::WorkspaceWrite).await;
    turn(&session, "tool:exec {\"command\":\"rm -rf\"}").await;
    assert_eq!(
        result_text(&last_result(&session).await),
        "Command denied by command rule `rm -rf` (exact)"
    );
    assert!(!ran.exists(), "a hook ran for a denied call");
    env.close().await;
}

/// Appendix D 3: Approval's recheck after an answer runs no hook; the marker
/// counts the hook's runs.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_recheck_after_an_answer_runs_no_pre_tool_use_hook() {
    let env = Env::new().await;
    let runs = env.path("recheck-runs");
    env.sources.set(document(serde_json::json!({
        "PreToolUse": [command(serde_json::json!({
            "type": "command",
            "command": format!(
                "printf x >> {}; \
                 printf '{{\"hookSpecificOutput\":{{\"permissionDecision\":\"ask\",\
                 \"permissionDecisionReason\":\"the guard is unsure\"}}}}'",
                runs.display(),
            ),
        }))],
    })));
    let session = env.session_with(PermissionPreset::WorkspaceWrite).await;
    session
        .submit(
            "tool:exec {\"command\":\"true\"}".into(),
            InputMode::Queue,
            "main",
        )
        .await
        .unwrap();
    let approval_id = wait_event(&session, |kind| match kind {
        EventKind::ApprovalRequest(data) => Some(data.approval_id.clone()),
        _ => None,
    })
    .await;
    answer(&session, &approval_id, ApprovalDecision::Allow).await;
    session.idle("main").await.unwrap();
    assert_eq!(last_result(&session).await.is_error, None);
    assert_eq!(
        std::fs::read_to_string(&runs).unwrap(),
        "x",
        "the recheck ran the hook again"
    );
    env.close().await;
}

/// Appendix D 9: a pasted Claude group with matcher `Bash` runs for `exec`,
/// and its script reads `tool_input.command` from stdin.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_pasted_bash_matcher_runs_for_exec_and_reads_the_command() {
    let env = Env::new().await;
    let stdin_file = env.path("pre-tool-stdin");
    env.sources.set(document(serde_json::json!({
        "PreToolUse": [{
            "matcher": "Bash",
            "hooks": [{
                "type": "command",
                "command": format!("printf ran > {}.marker; cat > {}", stdin_file.display(), stdin_file.display()),
            }],
        }],
    })));
    let session = env.session().await;
    turn(&session, "tool:exec {\"command\":\"echo hi\"}").await;
    // `cat` returns once its standard input closes, so the file is complete
    // before the turn ends.
    let stdin = std::fs::read_to_string(&stdin_file).expect("the Bash matcher ran for exec");
    let payload: serde_json::Value = serde_json::from_str(&stdin).unwrap();
    assert_eq!(payload["hook_event_name"], "PreToolUse");
    assert_eq!(payload["tool_name"], "exec");
    assert_eq!(payload["tool_input"]["command"], "echo hi");
    assert_eq!(payload["risk"], "execute_process");
    assert_eq!(payload["permission_decision"], "allow");
    assert_eq!(payload["branch_id"], "main");
    assert_eq!(payload["dim_source"], "user");
    assert_eq!(payload["permission_mode"], "full-access");
    assert!(
        payload["tool_use_id"]
            .as_str()
            .is_some_and(|id| !id.is_empty()),
        "{payload}"
    );
    assert!(
        payload["turn_id"].as_str().is_some_and(|id| !id.is_empty()),
        "{payload}"
    );
    env.close().await;
}

/// A handler that appends `text` to `path`, so a test can count its runs.
fn marking(path: &Path, text: &str) -> String {
    format!("printf '{text}\\n' >> {}", path.display())
}

/// A `PermissionRequest` handler that appends `ran` and answers allow, so
/// the turn needs no operator answer.
fn allowing(path: &Path) -> String {
    format!(
        "printf 'ran\\n' >> {path}; printf '%s' '{decision}'",
        path = path.display(),
        decision = r#"{"hookSpecificOutput":{"decision":{"behavior":"allow"}}}"#,
    )
}

/// G.2: a Session with no MCP server adopts no tool part, and the built-in
/// alias still selects `exec` at `PermissionRequest`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_pasted_permission_matcher_runs_without_an_adopted_tool_part() {
    let env = Env::new().await;
    let ran = env.path("permission-ran");
    env.sources.set(document(serde_json::json!({
        "PermissionRequest": [{
            "matcher": "Bash",
            "hooks": [{"type": "command", "command": allowing(&ran)}],
        }],
    })));
    let session = env.session_with(PermissionPreset::WorkspaceWrite).await;
    turn(&session, "tool:exec {\"command\":\"true\"}").await;
    assert!(
        ran.exists(),
        "the Bash matcher did not select exec at PermissionRequest"
    );
    assert_eq!(last_result(&session).await.is_error, None);
    env.close().await;
}

/// G.2: without an adopted tool part, `PostToolUse` and `PostToolUseFailure`
/// groups match `exec` by `Bash` and `read` by `Read`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_pasted_post_tool_matchers_run_for_exec_and_read_without_an_adopted_tool_part() {
    let env = Env::new().await;
    let (bash_ok, bash_failed) = (env.path("bash-ok"), env.path("bash-failed"));
    let (read_ok, read_failed) = (env.path("read-ok"), env.path("read-failed"));
    env.sources.set(document(serde_json::json!({
        "PostToolUse": [
            {"matcher": "Bash", "hooks": [{"type": "command", "command": marking(&bash_ok, "bash")}]},
            {"matcher": "Read", "hooks": [{"type": "command", "command": marking(&read_ok, "read")}]},
        ],
        "PostToolUseFailure": [
            {"matcher": "Bash", "hooks": [{"type": "command", "command": marking(&bash_failed, "bash")}]},
            {"matcher": "Read", "hooks": [{"type": "command", "command": marking(&read_failed, "read")}]},
        ],
    })));
    let session = env.session().await;
    turn(&session, "tool:exec {\"command\":\"true\"}").await;
    wait_file(&bash_ok, "the Bash matcher did not select exec").await;
    turn(&session, "tool:exec {\"command\":\"exit 1\"}").await;
    wait_file(
        &bash_failed,
        "the Bash matcher did not select a failed exec",
    )
    .await;
    let note = env.path("work/note.txt");
    std::fs::write(&note, "note\n").unwrap();
    turn(
        &session,
        &format!("tool:read {{\"path\":{:?}}}", note.to_str().unwrap()),
    )
    .await;
    wait_file(&read_ok, "the Read matcher did not select read").await;
    turn(
        &session,
        &format!(
            "tool:read {{\"path\":{:?}}}",
            env.path("work/missing.txt").to_str().unwrap()
        ),
    )
    .await;
    wait_file(
        &read_failed,
        "the Read matcher did not select a failed read",
    )
    .await;
    env.close().await;
}

/// G.2: the alias is the call's own declaration, so `Bash` does not select
/// `read` and `Read` does not select `exec`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_pasted_matcher_does_not_select_the_other_tool() {
    let env = Env::new().await;
    let (bash, read) = (env.path("bash-ran"), env.path("read-ran"));
    env.sources.set(document(serde_json::json!({
        "PostToolUse": [
            {"matcher": "Bash", "hooks": [{"type": "command", "command": marking(&bash, "bash")}]},
            {"matcher": "Read", "hooks": [{"type": "command", "command": marking(&read, "read")}]},
        ],
    })));
    let session = env.session().await;
    let note = env.path("work/note.txt");
    std::fs::write(&note, "note\n").unwrap();
    turn(
        &session,
        &format!("tool:read {{\"path\":{:?}}}", note.to_str().unwrap()),
    )
    .await;
    wait_file(&read, "the Read matcher did not select read").await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(!bash.exists(), "the Bash matcher selected read");
    turn(&session, "tool:exec {\"command\":\"true\"}").await;
    wait_file(&bash, "the Bash matcher did not select exec").await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        std::fs::read_to_string(&read).unwrap(),
        "read\n",
        "the Read matcher selected exec"
    );
    env.close().await;
}

/// G.2: `Bash` selects a registered tool with risk `execute_process` other
/// than `exec` on `PreToolUse`, `PermissionRequest` and `PostToolUse`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bash_selects_a_registered_tool_with_risk_execute_process() {
    let env = Env::with_tools(Extra::Registered, &["read", "exec", "deploy"]).await;
    let (pre, permission, post) = (
        env.path("pre-ran"),
        env.path("permission-ran"),
        env.path("post-ran"),
    );
    env.sources.set(document(serde_json::json!({
        "PreToolUse": [{
            "matcher": "Bash",
            "hooks": [{"type": "command", "command": marking(&pre, "pre")}],
        }],
        "PermissionRequest": [{
            "matcher": "Bash",
            "hooks": [{"type": "command", "command": allowing(&permission)}],
        }],
        "PostToolUse": [{
            "matcher": "Bash",
            "hooks": [{"type": "command", "command": marking(&post, "post")}],
        }],
    })));
    let session = env.session_with(PermissionPreset::WorkspaceWrite).await;
    turn(&session, "tool:deploy {}").await;
    assert!(
        pre.exists(),
        "the Bash matcher did not select deploy at PreToolUse"
    );
    assert!(
        permission.exists(),
        "the Bash matcher did not select deploy at PermissionRequest"
    );
    wait_file(
        &post,
        "the Bash matcher did not select deploy at PostToolUse",
    )
    .await;
    assert_eq!(last_result(&session).await.is_error, None);
    env.close().await;
}

/// G.2: with an adopted tool part, the built-in matchers still run, and an
/// MCP tool still matches `mcp__<server>__<tool>`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn matchers_keep_selecting_tools_with_an_adopted_tool_part() {
    let env = Env::with_tools(Extra::Served, &["read", "exec"]).await;
    let (bash, mcp) = (env.path("bash-ran"), env.path("mcp-ran"));
    env.sources.set(document(serde_json::json!({
        "PostToolUse": [
            {"matcher": "Bash", "hooks": [{"type": "command", "command": marking(&bash, "bash")}]},
            {"matcher": "mcp__server__lookup_item", "hooks": [{"type": "command", "command": marking(&mcp, "mcp")}]},
        ],
    })));
    let session = env.session().await;
    turn(&session, "tool:exec {\"command\":\"true\"}").await;
    wait_file(&bash, "the Bash matcher did not select exec").await;
    turn(&session, "tool:lookup {\"query\":\"x\"}").await;
    wait_file(&mcp, "the MCP alias did not select lookup").await;
    assert_eq!(last_result(&session).await.is_error, None);
    env.close().await;
}

/// G.2: with an adopted tool part, `PreToolUse` selects an MCP tool by its
/// alias.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_adopted_tool_part_alias_selects_at_pre_tool_use() {
    let env = Env::with_tools(Extra::Served, &["read", "exec"]).await;
    let pre = env.path("pre-ran");
    env.sources.set(document(serde_json::json!({
        "PreToolUse": [{
            "matcher": "mcp__server__lookup_item",
            "hooks": [{"type": "command", "command": marking(&pre, "pre")}],
        }],
    })));
    let session = env.session().await;
    turn(&session, "tool:lookup {\"query\":\"x\"}").await;
    assert!(
        pre.exists(),
        "the MCP alias did not select lookup at PreToolUse"
    );
    assert_eq!(last_result(&session).await.is_error, None);
    env.close().await;
}

/// G.2: with an adopted tool part, `PermissionRequest` selects an MCP tool
/// by its alias.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_adopted_tool_part_alias_selects_at_permission_request() {
    let env = Env::with_tools(Extra::Served, &["read", "exec"]).await;
    let permission = env.path("permission-ran");
    env.sources.set(document(serde_json::json!({
        "PermissionRequest": [{
            "matcher": "mcp__server__lookup_item",
            "hooks": [{"type": "command", "command": allowing(&permission)}],
        }],
    })));
    let session = env.session_with(PermissionPreset::WorkspaceWrite).await;
    session
        .submit(
            "tool:lookup {\"query\":\"x\"}".into(),
            InputMode::Queue,
            "main",
        )
        .await
        .unwrap();
    // The hook answers the approval; a group that the alias does not select
    // leaves the call waiting for an answer that never comes.
    wait_file(
        &permission,
        "the MCP alias did not select lookup at PermissionRequest",
    )
    .await;
    session.idle("main").await.unwrap();
    assert_eq!(last_result(&session).await.is_error, None);
    env.close().await;
}

/// G.2: a batch's alias is the declaration in force where the batch folded,
/// so a reconfiguration consumed while the queue drains does not change it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_tool_batch_keeps_the_alias_of_the_declaration_it_folded_under() {
    let env = Env::with_tools(Extra::Served, &["read", "exec"]).await;
    let (holding, release, mcp) = (
        env.path("holding"),
        env.path("release"),
        env.path("mcp-ran"),
    );
    env.sources.set(document(serde_json::json!({
        "PostToolUse": [
            {"hooks": [{"type": "command", "command": format!(
                "printf started > {}; while [ ! -e {} ]; do sleep 0.02; done",
                holding.display(),
                release.display(),
            )}]},
            {"matcher": "mcp__server__lookup_item", "hooks": [{"type": "command", "command": marking(&mcp, "mcp")}]},
        ],
    })));
    let session = env.session().await;
    let note = env.path("work/note.txt");
    std::fs::write(&note, "note\n").unwrap();
    session
        .submit(
            format!("tool:read {{\"path\":{:?}}}", note.to_str().unwrap()),
            InputMode::Queue,
            "main",
        )
        .await
        .unwrap();
    wait_file(&holding, "the PostToolUse hook did not start").await;
    session
        .submit(
            "tool:lookup {\"query\":\"x\"}".into(),
            InputMode::Queue,
            "main",
        )
        .await
        .unwrap();
    wait_event(&session, |kind| {
        let EventKind::ToolResult(data) = kind else {
            return None;
        };
        data.results
            .iter()
            .any(|result| {
                matches!(&result.result.content, ToolContent::Text(text) if text == "looked up")
            })
            .then_some(())
    })
    .await;
    // The configuration a source's removal leaves: the registered tools with
    // no sources, which no longer declares `lookup`.
    env.declare_tools(&session, &["read", "exec"]).await;
    // Idle returns once the dispatcher has delivered the reconfiguration, so
    // the branch has folded it before the queue moves.
    session.idle("main").await.unwrap();
    std::fs::write(&release, "").unwrap();
    wait_file(
        &mcp,
        "the batch took the alias of a declaration folded after it",
    )
    .await;
    env.close().await;
}

/// Appendix D 10: two denying handlers that finish in reverse catalog order
/// yield the first handler's reason. The handlers order themselves with
/// marker files, not with sleeps.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_first_deny_in_catalog_order_is_the_reason() {
    let env = Env::new().await;
    let finished = env.path("second-finished");
    env.sources.set(document(serde_json::json!({
        "PreToolUse": [{
            "hooks": [
                {
                    "type": "command",
                    "command": format!(
                        "while [ ! -e {} ]; do sleep 0.05; done; \
                         printf 'first denies' >&2; exit 2",
                        finished.display(),
                    ),
                },
                {
                    "type": "command",
                    "command": format!(
                        "printf done > {}; printf 'second denies' >&2; exit 2",
                        finished.display(),
                    ),
                },
            ],
        }],
    })));
    let session = env.session().await;
    let ran = env.path("reverse-order-ran");
    turn(
        &session,
        &format!(
            "tool:exec {{\"command\":\"printf ran > {}\"}}",
            ran.display()
        ),
    )
    .await;
    let result = last_result(&session).await;
    assert_eq!(result.is_error, Some(true));
    assert_eq!(result_text(&result), "Hook user: first denies");
    assert!(!ran.exists(), "a denied call ran");
    env.close().await;
}

/// §4: an interrupt while a `PreToolUse` command runs settles the call as
/// cancelled, and the command is terminated.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_interrupt_while_pre_tool_use_runs_cancels_the_call_and_ends_the_command() {
    let env = Env::new().await;
    let started = env.path("pre-tool-started");
    let pid_file = env.path("pre-tool-pid");
    env.sources.set(document(serde_json::json!({
        "PreToolUse": [command(serde_json::json!({
            "type": "command",
            "command": format!(
                "printf '%s' $$ > {pid}; printf started > {started}; sleep 30",
                pid = pid_file.display(),
                started = started.display(),
            ),
        }))],
    })));
    let session = env.session().await;
    let ran = env.path("interrupted-ran");
    session
        .submit(
            format!(
                "tool:exec {{\"command\":\"printf ran > {}\"}}",
                ran.display()
            ),
            InputMode::Queue,
            "main",
        )
        .await
        .unwrap();
    wait_file(&started, "the PreToolUse hook did not start").await;
    session.interrupt("main", "stop".into()).unwrap();
    tokio::time::timeout(Duration::from_secs(10), session.idle("main"))
        .await
        .expect("the interrupted turn settled")
        .unwrap();
    assert!(!ran.exists(), "the interrupted call ran");
    assert_eq!(last_result(&session).await.is_error, Some(true));
    assert!(
        events(&session).await.iter().any(|event| matches!(
            &event.kind,
            EventKind::TurnEnd(TurnEndData {
                outcome: TurnOutcome::Cancelled { .. },
                ..
            })
        )),
        "the turn did not end cancelled"
    );
    wait_gone(prompted_pid(&pid_file)).await;
    env.close().await;
}

/// §1 and A.6.1: a Hooks unload while a `PreToolUse` command runs drops the
/// advice as none, so Permission decides alone, and the command is
/// terminated.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_hooks_unload_drops_a_pending_pre_tool_use_advice() {
    let env = Env::with_mounted_hooks(Arc::new(Model::default()), None).await;
    let started = env.path("pre-tool-started");
    let pid_file = env.path("pre-tool-pid");
    env.sources.set(document(serde_json::json!({
        "PreToolUse": [command(serde_json::json!({
            "type": "command",
            "command": format!(
                "printf '%s' $$ > {pid}; printf started > {started}; sleep 30",
                pid = pid_file.display(),
                started = started.display(),
            ),
        }))],
    })));
    let session = env.session_with(PermissionPreset::WorkspaceWrite).await;
    let ran = env.path("unloaded-ran");
    session
        .submit(
            format!(
                "tool:exec {{\"command\":\"printf ran > {}\"}}",
                ran.display()
            ),
            InputMode::Queue,
            "main",
        )
        .await
        .unwrap();
    wait_file(&started, "the PreToolUse hook did not start").await;
    env.unload_branch_hooks().await;
    let approval_id = wait_event(&session, |kind| match kind {
        EventKind::ApprovalRequest(data) => Some(data.approval_id.clone()),
        _ => None,
    })
    .await;
    answer(&session, &approval_id, ApprovalDecision::Deny).await;
    tokio::time::timeout(Duration::from_secs(10), session.idle("main"))
        .await
        .expect("the turn settled")
        .unwrap();
    assert_eq!(
        result_text(&last_result(&session).await),
        "Tool execution denied by user"
    );
    assert!(!ran.exists(), "a denied call ran");
    wait_gone(prompted_pid(&pid_file)).await;
    env.close().await;
}

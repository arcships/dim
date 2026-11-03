//! Shared fixture for PromptSources and Skills flows: temporary home and cwd,
//! process services on a host root, a scripted model and a gated tool.
#![allow(dead_code)]

use dim_agent_capabilities::{
    defaults,
    execution_settings::{ExecutionSettings, ExecutionSettingsHost},
    history::HistoryRenderer,
    host::*,
    host_platform::{
        ExecShellRuntime, HostPlatform, Os, OutputEncoding, ShellEdition, ShellSource, ShellStatus,
    },
    model_provider::ModelProviderPlugin,
    permission::{CommandRulesHost, NoCommandRules},
    prompt_sources,
    skill_settings::{SkillSettings, SkillSettingsHost},
    skills::{self, MemorySkillSettings, SkillsCatalog, SkillsCatalogPlugin},
    timeline_tree::TreeCommands,
};
use dim_agent_runtime::testing::MinimalHost;
use dim_agent_runtime::*;
use dim_agent_types::*;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, Key, TypeKey};
use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

/// A temporary directory tree removed on drop.
pub struct Dirs {
    pub root: PathBuf,
    pub home: PathBuf,
}
impl Dirs {
    pub fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "dim-local-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let home = root.join("home");
        std::fs::create_dir_all(&home).unwrap();
        Self { root, home }
    }
    /// Creates `rel` under the root and returns its path.
    pub fn dir(&self, rel: &str) -> PathBuf {
        let path = self.root.join(rel);
        std::fs::create_dir_all(&path).unwrap();
        path
    }
    pub fn write(&self, path: &Path, content: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }
}
impl Drop for Dirs {
    fn drop(&mut self) {
        #[cfg(unix)]
        restore_permissions(&self.root);
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
#[cfg(unix)]
fn restore_permissions(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755));
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                restore_permissions(&path);
            } else {
                let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644));
            }
        }
    }
}

pub fn skill_file(name: &str, description: &str, body: &str) -> String {
    format!("---\nname: {name}\ndescription: {description}\n---\n{body}\n")
}

pub fn platform(home: &Path) -> HostPlatform {
    let mut platform = HostPlatform::current(ShellStatus::Available(ExecShellRuntime {
        executable_path: "/bin/zsh".into(),
        edition: ShellEdition::Zsh,
        source: ShellSource::Login,
        output_encoding: OutputEncoding::Utf8,
    }))
    .unwrap()
    .with_execution_settings(ExecutionSettingsHost::fixed(ExecutionSettings::default()));
    platform.home = home.to_owned();
    platform.os = Os::Linux;
    platform.arch = "x64".into();
    platform.timezone = Some("Asia/Shanghai".into());
    platform.env.clear();
    platform
}

pub enum Step {
    Tool(&'static str, serde_json::Value),
    Text(&'static str),
}

/// Replies with scripted steps, then with plain text.
#[derive(Default)]
pub struct ScriptedModel(Mutex<VecDeque<Step>>);
impl ScriptedModel {
    pub fn push(&self, step: Step) {
        self.0.lock().unwrap().push_back(step);
    }
}
impl ModelDriver for ScriptedModel {
    fn invoke(
        &self,
        _: Arc<ModelRequest>,
        _: ModelConnection,
        _: CancellationToken,
    ) -> HostFuture<'_, ModelStream> {
        let step = self.0.lock().unwrap().pop_front();
        Box::pin(async move {
            let id = "message".to_owned();
            let (body, reason) = match step {
                Some(Step::Tool(name, input)) => {
                    let call = ToolCall {
                        id: format!("call-{}", uuid_like()),
                        name: name.into(),
                        input: input.as_object().unwrap().clone(),
                    };
                    let start = ModelChunk::ToolStart {
                        id: call.id.clone(),
                        name: call.name.clone(),
                    };
                    (
                        vec![start, ModelChunk::ToolUse(call)],
                        ModelStopReason::ToolUse,
                    )
                }
                Some(Step::Text(text)) => (
                    vec![ModelChunk::Text(text.into()), ModelChunk::TextEnd],
                    ModelStopReason::EndTurn,
                ),
                None => (
                    vec![ModelChunk::Text("ok".into()), ModelChunk::TextEnd],
                    ModelStopReason::EndTurn,
                ),
            };
            let mut chunks = vec![Ok(ModelChunk::MessageStart {
                message_id: id.clone(),
            })];
            chunks.extend(body.into_iter().map(Ok));
            chunks.push(Ok(ModelChunk::MessageStop {
                message_id: id,
                reason,
            }));
            Ok(Box::pin(futures_util::stream::iter(chunks)) as ModelStream)
        })
    }
}
fn uuid_like() -> usize {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    NEXT.fetch_add(1, Ordering::SeqCst)
}
/// `gate` blocks until released, giving tests a window inside a turn.
#[derive(Default)]
pub struct Gate {
    pub entered: Notify,
    pub release: Notify,
}
pub struct GateTools(pub Arc<Gate>);
impl ToolDriver for GateTools {
    fn definitions(&self) -> Vec<ToolDefinition> {
        let mut gate = dim_agent_capabilities::tool_registry::definition(
            "gate",
            "Wait until the test releases it.",
            serde_json::json!({"type":"object"}),
        );
        gate.risk = Some("session".into());
        vec![gate]
    }
    fn execute(
        &self,
        _: ToolCall,
        _: ToolExecutionContext,
        _: CancellationToken,
    ) -> HostFuture<'_, ToolResult> {
        Box::pin(async move {
            self.0.entered.notify_one();
            self.0.release.notified().await;
            Ok(ToolResult::text("gated"))
        })
    }
}

/// Holds the next Session's assembly open until released, while its other
/// capabilities load concurrently: a window before the Session is published.
/// A flow that needs another capability applied waits for it separately.
#[derive(Default)]
pub struct AssemblyHold {
    armed: std::sync::atomic::AtomicBool,
    pub entered: Notify,
    pub release: Notify,
}
impl AssemblyHold {
    pub fn arm(&self) {
        self.armed.store(true, Ordering::SeqCst);
    }
}
struct AssemblyHoldPlugin(Arc<AssemblyHold>);
impl rutis::Plugin for AssemblyHoldPlugin {
    type Deps = ();
    fn name(&self) -> &str {
        "AssemblyHold"
    }
    async fn apply(&self, _: &rutis::Ctx, (): ()) -> rutis::Result {
        if self.0.armed.swap(false, Ordering::SeqCst) {
            self.0.entered.notify_one();
            self.0.release.notified().await;
        }
        Ok(())
    }
}

/// Records what each Session's `SessionLifecycle` reports, and can hold one
/// Session's unload to keep a close in progress.
#[derive(Default)]
pub struct LifecycleProbe {
    pub events: Mutex<Vec<(String, &'static str)>>,
    pub recorded: Notify,
    lifecycles: Mutex<std::collections::HashMap<String, Arc<SessionLifecycle>>>,
    trees: Mutex<std::collections::HashMap<String, Arc<TreeCommands>>>,
    hold_close: std::sync::atomic::AtomicBool,
    pub close_held: Notify,
    pub release_close: Notify,
}
impl LifecycleProbe {
    pub fn hold_next_close(&self) {
        self.hold_close.store(true, Ordering::SeqCst);
    }
    /// The `SessionLifecycle` a part of `session` injects, as an owner of
    /// detached work uses it.
    pub fn lifecycle(&self, session: &str) -> Arc<SessionLifecycle> {
        self.lifecycles.lock().unwrap()[session].clone()
    }
    /// The branch tree commands a part of `session` injects, which reach each
    /// published branch's own operations.
    pub fn tree(&self, session: &str) -> Arc<TreeCommands> {
        self.trees.lock().unwrap()[session].clone()
    }
    pub fn count(&self, event: &str) -> usize {
        self.events
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, recorded)| *recorded == event)
            .count()
    }
    pub async fn wait_for(&self, event: &str, count: usize) {
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                let recorded = self.recorded.notified();
                if self.count(event) >= count {
                    return;
                }
                recorded.await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("{count} {event} events: {:?}", self.events.lock().unwrap()));
    }
    fn record(&self, session: &str, event: &'static str) {
        self.events.lock().unwrap().push((session.into(), event));
        self.recorded.notify_waiters();
    }
}
struct LifecycleProbePlugin {
    probe: Arc<LifecycleProbe>,
    lifecycle: Key<SessionLifecycle>,
    identity: Key<SessionIdentity>,
    tree: Key<TreeCommands>,
}
impl LifecycleProbePlugin {
    fn new(names: &Names<'_>, probe: Arc<LifecycleProbe>) -> rutis::Result<Self> {
        Ok(Self {
            probe,
            lifecycle: names.key()?,
            identity: names.key()?,
            tree: names.key()?,
        })
    }
}
impl DynPlugin for LifecycleProbePlugin {
    fn name(&self) -> &str {
        "LifecycleProbe"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![
            self.lifecycle.erase(),
            self.identity.erase(),
            self.tree.erase(),
        ]
    }
    fn apply<'a>(&'a self, ctx: &'a rutis::Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let lifecycle = ctx.require_at(&self.lifecycle)?;
            let identity = ctx.require_at(&self.identity)?;
            let tree = ctx.require_at(&self.tree)?;
            let probe = self.probe.clone();
            let id = identity.session_id.clone();
            probe
                .lifecycles
                .lock()
                .unwrap()
                .insert(id.clone(), lifecycle.clone());
            probe.trees.lock().unwrap().insert(id.clone(), tree);
            probe.record(&id, "applied");
            let task = tokio::spawn(async move {
                let published = lifecycle.activated().await;
                probe.record(&id, if published { "active" } else { "inactive" });
                if published {
                    lifecycle.closing().await;
                    probe.record(&id, "closing");
                }
            });
            let probe = self.probe.clone();
            ctx.effect(move || async move {
                if probe.hold_close.swap(false, Ordering::SeqCst) {
                    probe.close_held.notify_one();
                    probe.release_close.notified().await;
                }
                let _ = task.await;
                Ok(())
            })?;
            Ok(Effect::Done)
        })
    }
}

pub struct Env {
    pub root: rutis::Ctx,
    pub agent: MinimalHost,
    pub model: Arc<ScriptedModel>,
    pub gate: Arc<Gate>,
    pub today: Arc<Mutex<String>>,
    pub settings: Arc<dyn SkillSettings>,
}
impl Env {
    pub fn catalog(&self) -> Arc<SkillsCatalog> {
        self.root.get::<SkillsCatalog>().unwrap()
    }
    pub async fn session(&self, cwd: &Path, system_prompt: &str) -> Result<Session> {
        self.agent
            .create_session(SessionConfig {
                cwd: cwd.to_string_lossy().into_owned(),
                system_prompt: system_prompt.into(),
                tools: Some(vec!["skill".into(), "gate".into()]),
                ..SessionConfig::default()
            })
            .await
    }
}

pub struct Options {
    pub store: Option<Arc<MemoryTimelineStore>>,
    pub settings: Arc<dyn SkillSettings>,
    pub today: Arc<Mutex<String>>,
    pub timezone: &'static str,
    pub hold: Option<Arc<AssemblyHold>>,
    pub probe: Option<Arc<LifecycleProbe>>,
    /// Skills' factory configuration: the text replacing the built-in
    /// instructions.
    pub instructions: Option<Arc<str>>,
    /// Register the Skills parts in the profile without installing the
    /// capability, as a host whose profile takes them by name does: the
    /// catalog plugin registers them either way.
    pub require_without_install: bool,
}
impl Default for Options {
    fn default() -> Self {
        Self {
            store: None,
            settings: Arc::new(MemorySkillSettings::default()),
            today: Arc::new(Mutex::new("2026-09-30".into())),
            timezone: "Asia/Shanghai",
            hold: None,
            probe: None,
            instructions: None,
            require_without_install: false,
        }
    }
}

/// The product profile: PromptSources and Skills with scripted hosts.
pub async fn env(dirs: &Dirs, options: Options) -> Env {
    let mut host = platform(&dirs.home);
    host.timezone = Some(options.timezone.into());
    let settings = options.settings.clone();
    let model = Arc::new(ScriptedModel::default());
    let gate = Arc::new(Gate::default());
    let mut builder = AgentBuilder::new(AgentConfig::default());
    if let Some(store) = options.store {
        builder.timeline_store(store);
    }
    builder
        .session_value(Arc::new(HistoryRenderer::default()))
        .process_value(Arc::new(ModelHost(model.clone())))
        .session_value(Arc::new(ToolHost(Arc::new(GateTools(gate.clone())))))
        .session_value(Arc::new(PrepareContextHost(Arc::new(IdentityContext))))
        .session_value(Arc::new(ContextModelHost(None)))
        .session_value(Arc::new(CommandRulesHost(Arc::new(NoCommandRules))))
        .session_value(Arc::new(CompactionStrategyHost(None)))
        .require_feature(
            Feature::new("PrepareModelHost").session("PrepareModelHost", |names| {
                Ok(Box::new(
                    dim_agent_capabilities::adapters::DefaultPrepareModelPlugin::new(names)?,
                ))
            }),
        );
    defaults::install_with_prompt_capability(&mut builder, ModelProviderPlugin::new);
    let today = options.today.clone();
    prompt_sources::install_with_clock(
        &mut builder,
        Arc::new(move || today.lock().unwrap().clone()),
        std::time::Duration::from_millis(10),
    );
    if options.require_without_install {
        for part in ["Skills", "SkillsBranch"] {
            builder.require_part(part);
        }
    } else if let Some(instructions) = options.instructions {
        skills::install_with_config(
            &mut builder,
            skills::SkillsConfig {
                instructions: Some(instructions),
            },
        );
    } else {
        skills::install(&mut builder);
    }
    if let Some(hold) = options.hold {
        builder.require_feature(
            Feature::new("AssemblyHold").session("AssemblyHold", move |_| {
                Ok(Box::new(AssemblyHoldPlugin(hold.clone())))
            }),
        );
    }
    if let Some(probe) = options.probe {
        builder.require_feature(
            Feature::new("LifecycleProbe").session("LifecycleProbe", move |names| {
                Ok(Box::new(LifecycleProbePlugin::new(names, probe.clone())?))
            }),
        );
    }
    builder.plugin(|names| Ok(Box::new(SkillsCatalogPlugin::new(names)?)));
    let agent = MinimalHost::start_with(&builder, move |root, _| {
        root.provide_as::<HostPlatform>(Arc::new(host))?;
        root.provide_as::<SkillSettingsHost>(Arc::new(SkillSettingsHost(settings)))?;
        Ok(())
    })
    .await
    .unwrap();
    Env {
        root: agent.root().clone(),
        agent,
        model,
        gate,
        today: options.today,
        settings: options.settings,
    }
}

pub fn system_text(prompt: &SystemPrompt) -> String {
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

/// Every model request of `branch`, oldest first.
pub async fn requests(session: &Session) -> Vec<Arc<ModelRequest>> {
    session
        .history("main", None)
        .await
        .unwrap()
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::ModelCall(data) => Some(data.request.clone()),
            _ => None,
        })
        .collect()
}

pub fn request_text(request: &ModelRequest) -> String {
    request
        .messages
        .iter()
        .map(|message| match &message.content {
            MessageContent::Text(text) => text.clone(),
            MessageContent::Parts(parts) => parts
                .iter()
                .filter_map(|part| match part {
                    MessagePart::Text { text, .. } => Some(text.clone()),
                    MessagePart::ToolResult { content, .. } => match content {
                        ToolContent::Text(text) => Some(text.clone()),
                        ToolContent::Blocks(_) => None,
                    },
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n"),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The input messages of `main`, oldest first.
pub async fn inputs(session: &Session) -> Vec<InputMessageData> {
    session
        .history("main", None)
        .await
        .unwrap()
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::InputMessage(data) => Some(data.clone()),
            _ => None,
        })
        .collect()
}

/// The `dim-state` value an input delivers for `source`, unescaped;
/// `Some(None)` clears it.
pub fn state_value(input: &InputMessageData, source: &str) -> Option<Option<String>> {
    let open = format!("<dim-state source=\"{source}\"");
    input.blocks.iter().find_map(|block| {
        let InputBlock::Raw { text, retire, .. } = block;
        let rest = text.strip_prefix(&open)?;
        if *retire {
            return Some(None);
        }
        let body = rest.strip_prefix('>')?.strip_suffix("</dim-state>")?;
        Some(Some(
            body.replace("&lt;", "<")
                .replace("&gt;", ">")
                .replace("&amp;", "&"),
        ))
    })
}

/// Tool results of `main`, oldest first.
pub async fn tool_results(session: &Session) -> Vec<ToolResult> {
    session
        .history("main", None)
        .await
        .unwrap()
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::ToolResult(data) => Some(
                data.results
                    .iter()
                    .map(|item| item.result.clone())
                    .collect::<Vec<_>>(),
            ),
            _ => None,
        })
        .flatten()
        .collect()
}

pub fn result_text(result: &ToolResult) -> String {
    match &result.content {
        ToolContent::Text(text) => text.clone(),
        ToolContent::Blocks(_) => String::new(),
    }
}

pub async fn run(session: &Session, text: &str) {
    session
        .submit(text.into(), InputMode::Queue, "main")
        .await
        .unwrap();
    session.idle("main").await.unwrap();
}

/// The in-memory store, holding the publication of one batch until the test
/// releases it: a window inside an adoption, after it composed what it
/// publishes and before it returned.
pub struct HoldingStore {
    memory: MemoryTimelineStore,
    selected: Mutex<Option<String>>,
    held: Mutex<Option<PublishCallback>>,
    /// The branch whose read is held, and how many matching reads pass
    /// before it; `None` when no read hold is armed.
    read_branch: Mutex<Option<(String, usize)>>,
    /// The ordinal of the read to hold on the branch the next `BranchCreate`
    /// names, while the flow has not created it yet.
    next_branch_read: Mutex<Option<usize>>,
    gate: tokio::sync::Semaphore,
    /// The segments of every read, by branch.
    reads: Mutex<Vec<(String, std::ops::RangeInclusive<u64>)>>,
    /// Notified when a batch is held.
    pub entered: Notify,
    /// Notified when a held read starts.
    pub read_entered: Notify,
}

impl Default for HoldingStore {
    fn default() -> Self {
        Self {
            memory: MemoryTimelineStore::default(),
            selected: Mutex::new(None),
            held: Mutex::new(None),
            read_branch: Mutex::new(None),
            next_branch_read: Mutex::new(None),
            gate: tokio::sync::Semaphore::new(0),
            reads: Mutex::new(Vec::new()),
            entered: Notify::new(),
            read_entered: Notify::new(),
        }
    }
}

impl HoldingStore {
    /// Holds the next batch on `branch` that carries a `context/configured`.
    pub fn hold_next_configuration_on(&self, branch: &str) {
        *self.selected.lock().unwrap() = Some(branch.to_string());
    }
    /// Holds the `nth` read of `branch`'s segments from now (1 is the next):
    /// the caller parks inside an adoption, after it started reading and
    /// before it composed.
    ///
    /// This keys the hold on the reads' order, not on which read it is: the
    /// store sees a read as its branch and a range, and reads a flow makes
    /// of one branch are not distinguishable by those, so a flow holds a
    /// read by its position in the branch's read order. A fresh branch's
    /// open composes nothing, so the branch's first read is its guide
    /// adoption's, and a flow that means that read arms 1; the close flow
    /// holds the next read of its branch while the read it means is the next
    /// one. An unrelated read of the same branch would take the hold
    /// instead; the flows contain that: their assertions still demand the
    /// entry the branch ran with, so a hold an unrelated read took fails the
    /// flow rather than passing it.
    pub fn hold_read_of(&self, branch: &str, nth: usize) {
        *self.read_branch.lock().unwrap() = Some((branch.to_string(), nth.max(1)));
    }
    /// Holds the next read of `branch`'s segments.
    pub fn hold_next_read_of(&self, branch: &str) {
        self.hold_read_of(branch, 1);
    }
    /// Holds the `nth` read of the branch the next `BranchCreate` names: a
    /// fork's child is created inside the call, so its id does not exist
    /// when the hold is armed. The store learns the id from the batch that
    /// creates the branch; reads of other branches pass untouched.
    pub fn hold_read_of_the_next_branch(&self, nth: usize) {
        *self.next_branch_read.lock().unwrap() = Some(nth.max(1));
    }
    /// Publishes the held batch; the test calls it once the work it wants
    /// inside the adoption's window is done.
    pub fn release(&self) {
        if let Some(publish) = self.held.lock().unwrap().take() {
            publish(Ok(()));
        }
    }
    /// Lets the held read continue.
    pub fn release_read(&self) {
        self.gate.add_permits(1);
    }
    /// The segments read since the last call, by branch.
    pub fn take(&self) -> Vec<(String, std::ops::RangeInclusive<u64>)> {
        std::mem::take(&mut *self.reads.lock().unwrap())
    }
}

impl TimelineStore for HoldingStore {
    fn create_session<'a>(
        &'a self,
        id: &'a str,
        config: &'a SessionConfig,
    ) -> BoxFuture<'a, Result<()>> {
        self.memory.create_session(id, config)
    }
    fn rollback_session<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<()>> {
        self.memory.rollback_session(id)
    }
    fn release_session<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<()>> {
        self.memory.release_session(id)
    }
    fn session_config<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<Option<SessionConfig>>> {
        self.memory.session_config(id)
    }
    fn branches<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<Vec<StoredBranch>>> {
        self.memory.branches(id)
    }
    fn head<'a>(&'a self, key: &'a TimelineKey) -> BoxFuture<'a, Result<u64>> {
        self.memory.head(key)
    }
    fn append(
        &self,
        key: TimelineKey,
        events: Vec<StoredEvent>,
        publish: PublishCallback,
    ) -> Result<()> {
        if let Some(created) = events.iter().find_map(|event| match &event.event.kind {
            EventKind::BranchCreate(data) => Some(data.branch.clone()),
            _ => None,
        }) && let Some(nth) = self.next_branch_read.lock().unwrap().take()
        {
            self.hold_read_of(&created, nth);
        }
        let hold = {
            let mut selected = self.selected.lock().unwrap();
            let hold = selected.as_deref() == Some(key.branch_id.as_str())
                && events
                    .iter()
                    .any(|event| matches!(event.event.kind, EventKind::ContextConfigured(_)));
            if hold {
                selected.take();
            }
            hold
        };
        if !hold {
            return self.memory.append(key, events, publish);
        }
        self.memory.append(key, events, Box::new(|_| ()))?;
        *self.held.lock().unwrap() = Some(publish);
        self.entered.notify_one();
        Ok(())
    }
    fn read_segments<'a>(
        &'a self,
        id: &'a str,
        segments: Vec<(String, std::ops::RangeInclusive<u64>)>,
    ) -> BoxFuture<'a, Result<Vec<Vec<StoredEvent>>>> {
        let hold = {
            let mut selected = self.read_branch.lock().unwrap();
            let mut hold = false;
            if let Some((branch, skip)) = selected.as_mut()
                && segments.iter().any(|(read, _)| read == branch)
            {
                if *skip <= 1 {
                    hold = true;
                } else {
                    *skip -= 1;
                }
            }
            if hold {
                *selected = None;
            }
            hold
        };
        self.reads.lock().unwrap().extend(segments.iter().cloned());
        Box::pin(async move {
            if hold {
                self.read_entered.notify_one();
                self.gate
                    .acquire()
                    .await
                    .expect("the read gate stays open")
                    .forget();
            }
            self.memory.read_segments(id, segments).await
        })
    }
    fn trace<'a>(
        &'a self,
        key: &'a TimelineKey,
        sequence: u64,
    ) -> BoxFuture<'a, Result<Option<TimelineTrace>>> {
        self.memory.trace(key, sequence)
    }
    fn query<'a>(
        &'a self,
        key: &'a TimelineKey,
        query: &'a dim_agent_runtime::TimelineQuery,
        from: u64,
        through: u64,
    ) -> BoxFuture<'a, Result<dim_agent_runtime::TimelineQueryPage>> {
        self.memory.query(key, query, from, through)
    }
}

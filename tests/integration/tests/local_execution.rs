//! exec, BackgroundExec and wait flows on a real Unix shell
//! (`docs/architecture/rust-agent/plugins/{BackgroundExec,WaitBranch}.md`).
#![cfg(unix)]
#[path = "local_support/compaction.rs"]
mod compaction_support;
#[path = "local_support/process.rs"]
mod process_support;
use dim_agent_capabilities::{
    adapters::DefaultPrepareModelPlugin,
    execution_settings::{ExecutionSettings, ExecutionSettingsHost},
    file_storage::FileStoragePlugin,
    history::HistoryRenderer,
    host::*,
    host_platform::{
        EXEC_SHELL_CONFIG_INVALID, EXEC_SHELL_UNAVAILABLE, ExecShellRuntime, HostPlatform,
        HostPlatformConfig, HostPlatformPlugin, OutputEncoding, ShellEdition, ShellSource,
        ShellStatus, resolve_shell,
    },
    timeline_tree::TreeCommands,
    wait,
    workspace_tools::{
        self, WorkspaceToolsConfig,
        background::{BackgroundExec, WAKE_RESULT_TAG, WAKE_TASK_TAG, testing},
        exec::{Decoder, truncate_preview},
    },
};
use dim_agent_runtime::testing::MinimalHost;
use dim_agent_runtime::*;
use dim_agent_types::*;
use process_support::{alive, gone, pid_in};
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, ErrorKind, Key, TypeKey};
use rutis::{Ctx, Plugin};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    ffi::OsString,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio_util::sync::CancellationToken;

/// Replies with queued tool batches, then with text. `$TASK` in an input is
/// replaced by the last `Task ID:` the model has seen.
#[derive(Default)]
struct Script {
    replies: Mutex<VecDeque<Vec<(String, Value)>>>,
    count: AtomicUsize,
}
impl Script {
    fn push(&self, calls: &[(&str, Value)]) {
        self.replies.lock().unwrap().push_back(
            calls
                .iter()
                .map(|(name, input)| (name.to_string(), input.clone()))
                .collect(),
        );
    }
}
fn last_task(text: &str) -> Option<String> {
    let start = text.rfind("Task ID: ")? + "Task ID: ".len();
    Some(
        text[start..]
            .chars()
            .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '-')
            .collect(),
    )
}
fn substitute(value: &Value, task: &Option<String>) -> Value {
    match value {
        Value::String(text) if text == "$TASK" => json!(task.clone().unwrap_or_default()),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(key, value)| (key.clone(), substitute(value, task)))
                .collect(),
        ),
        other => other.clone(),
    }
}
impl ModelDriver for Script {
    fn invoke(
        &self,
        request: Arc<ModelRequest>,
        _: ModelConnection,
        _: CancellationToken,
    ) -> HostFuture<'_, ModelStream> {
        Box::pin(async move {
            let seen = serde_json::to_string(&request.messages).unwrap();
            let task = last_task(&seen);
            let id = format!("m{}", self.count.fetch_add(1, Ordering::Relaxed));
            let mut chunks = vec![Ok(ModelChunk::MessageStart {
                message_id: id.clone(),
            })];
            let reason = match self.replies.lock().unwrap().pop_front() {
                Some(calls) => {
                    for (index, (name, input)) in calls.into_iter().enumerate() {
                        let Value::Object(input) = substitute(&input, &task) else {
                            unreachable!()
                        };
                        chunks.push(Ok(ModelChunk::ToolStart {
                            id: format!("{id}-{index}"),
                            name: name.clone(),
                        }));
                        chunks.push(Ok(ModelChunk::ToolUse(ToolCall {
                            id: format!("{id}-{index}"),
                            name,
                            input,
                        })));
                    }
                    ModelStopReason::ToolUse
                }
                None => {
                    chunks.push(Ok(ModelChunk::Text("done".into())));
                    chunks.push(Ok(ModelChunk::TextEnd));
                    ModelStopReason::EndTurn
                }
            };
            chunks.push(Ok(ModelChunk::MessageStop {
                message_id: id,
                reason,
            }));
            Ok(Box::pin(futures_util::stream::iter(chunks)) as ModelStream)
        })
    }
}

/// Provides the process HostPlatform the design lets tests construct.
struct PlatformPlugin(Arc<HostPlatform>);
impl Plugin for PlatformPlugin {
    type Deps = ();
    fn name(&self) -> &str {
        "TestHostPlatform"
    }
    async fn apply(&self, ctx: &Ctx, (): Self::Deps) -> rutis::Result {
        ctx.provide_as::<HostPlatform>(self.0.clone())?;
        Ok(())
    }
}
/// `platform` provided by [`PlatformPlugin`], a row of the harness's host.
fn test_platform(platform: HostPlatform) -> impl FnOnce(&mut AgentBuilder) {
    let platform = Arc::new(platform);
    move |builder| {
        builder.plugin(move |_| Ok(Box::new(PlatformPlugin(platform.clone()))));
    }
}
/// FileStorage over `home` and the HostPlatform plugin reading
/// `exec-shell.json` there, rows of the harness's host.
fn configured_platform(home: PathBuf) -> impl FnOnce(&mut AgentBuilder) {
    move |builder| {
        let files = FileStoragePlugin::new(vec![home]);
        builder.plugin(move |_| Ok(Box::new(files.clone())));
        builder.plugin(|names| {
            Ok(Box::new(HostPlatformPlugin::new(
                names,
                HostPlatformConfig {
                    execution: names.key()?,
                    exec_shell_file: Some((0, "exec-shell.json".into())),
                },
            )?))
        });
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Hook {
    None,
    RewriteTask,
}

/// Branch probe: keeps each branch Timeline, its plugin context (to restart
/// one capability) and the tree commands for inspection, with an optional
/// `before` hook on `exec/started`.
type Probed = Vec<(String, Arc<Timeline>, Ctx)>;
#[derive(Clone, Default)]
struct Seen {
    timelines: Arc<Mutex<Probed>>,
    tree: Arc<Mutex<Option<Arc<TreeCommands>>>>,
}
impl Seen {
    fn timeline(&self, branch: &str) -> Arc<Timeline> {
        self.timelines
            .lock()
            .unwrap()
            .iter()
            .rfind(|(id, ..)| id == branch)
            .map(|(_, timeline, _)| timeline.clone())
            .expect("probed branch")
    }
    /// Restarts `branch`'s BackgroundExec of the latest probed Session and
    /// nothing else.
    fn restart_background(
        &self,
        branch: &str,
    ) -> BoxFuture<'static, std::result::Result<(), Arc<ErrorKind>>> {
        let id = self
            .tree()
            .get(branch)
            .unwrap()
            .diagnostics()
            .plugins
            .into_iter()
            .find(|plugin| plugin.name == "BackgroundExec")
            .unwrap()
            .id;
        let timelines = self.timelines.lock().unwrap();
        let (.., ctx) = timelines.iter().rfind(|(id, ..)| id == branch).unwrap();
        ctx.view(id).unwrap().restart()
    }
    fn tree(&self) -> Arc<TreeCommands> {
        self.tree.lock().unwrap().clone().expect("probed tree")
    }
}
struct Probe {
    hook: Hook,
    seen: Seen,
    timeline: Key<Timeline>,
    identity: Key<TimelineIdentity>,
    tree: Key<TreeCommands>,
}
impl Probe {
    fn new(names: &Names<'_>, hook: Hook, seen: Seen) -> rutis::Result<Self> {
        Ok(Self {
            hook,
            seen,
            timeline: names.key()?,
            identity: names.key()?,
            tree: names.key()?,
        })
    }
}
impl DynPlugin for Probe {
    fn name(&self) -> &str {
        "Probe"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![
            self.timeline.erase(),
            self.identity.erase(),
            self.tree.erase(),
        ]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let timeline = ctx.require_at(&self.timeline)?;
            let identity = ctx.require_at(&self.identity)?;
            let tree = ctx.require_at(&self.tree)?;
            self.seen.timelines.lock().unwrap().push((
                identity.branch_id.clone(),
                timeline.clone(),
                ctx.clone(),
            ));
            *self.seen.tree.lock().unwrap() = Some(tree);
            let hook = self.hook;
            if hook == Hook::RewriteTask {
                timeline.before(ctx, "probe", &["exec/started"], move |mut draft| {
                    if let EventKind::ExecStarted(data) = &mut draft.kind {
                        data.task_id = format!("{}-hooked", data.task_id);
                    }
                    Ok(draft)
                })?;
            }
            Ok(Effect::Done)
        })
    }
}

struct Harness {
    root: Ctx,
    store: Arc<MemoryTimelineStore>,
    agent: MinimalHost,
    model: Arc<Script>,
    cwd: PathBuf,
    output_root: PathBuf,
    seen: Seen,
}

fn scratch(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("dim-local-exec-{}-{name}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&path).unwrap();
    std::fs::canonicalize(path).unwrap()
}

async fn platform(extra: &[(&str, &str)]) -> HostPlatform {
    let mut env: Vec<_> = std::env::vars_os().collect();
    env.retain(|(key, _)| key != "SHELL");
    env.push(("SHELL".into(), "/bin/bash".into()));
    let mut platform = HostPlatform::current(resolve_shell(Ok(None), &env).await)
        .unwrap()
        .with_execution_settings(ExecutionSettingsHost::fixed(ExecutionSettings::default()));
    platform.env.extend(
        extra
            .iter()
            .map(|(key, value)| (OsString::from(key), OsString::from(value))),
    );
    platform
}

impl Harness {
    async fn new(platform: HostPlatform, hook: Hook) -> Self {
        Self::with_context(platform, hook, false).await
    }
    /// The same assembly with a context model and a strategy bound, so a flow
    /// test can compact (`local_support/compaction.rs`).
    async fn new_compacting(platform: HostPlatform, hook: Hook) -> Self {
        Self::with_context(platform, hook, true).await
    }
    async fn with_context(mut platform: HostPlatform, hook: Hook, compacting: bool) -> Self {
        let cwd = scratch("cwd");
        platform.home = cwd.clone();
        platform.env.retain(|(name, _)| name != "HOME");
        platform.env.push(("HOME".into(), cwd.as_os_str().into()));
        Self::with_platform_context(test_platform(platform), hook, cwd, compacting).await
    }
    async fn with_platform(
        platform: impl FnOnce(&mut AgentBuilder),
        hook: Hook,
        cwd: PathBuf,
    ) -> Self {
        Self::with_platform_context(platform, hook, cwd, false).await
    }
    /// A host whose `platform` rows provide HostPlatform, Active before the
    /// harness runs.
    async fn with_platform_context(
        platform: impl FnOnce(&mut AgentBuilder),
        hook: Hook,
        cwd: PathBuf,
        compacting: bool,
    ) -> Self {
        let store = Arc::new(MemoryTimelineStore::default());
        let output_root = scratch("output");
        let model = Arc::new(Script::default());
        let seen = Seen::default();
        let mut builder = AgentBuilder::new(AgentConfig::default());
        platform(&mut builder);
        builder
            .timeline_store(store.clone())
            .session_value(Arc::new(HistoryRenderer::default()))
            .process_value(Arc::new(ModelHost(model.clone())))
            .session_value(Arc::new(PrepareContextHost(Arc::new(IdentityContext))))
            .session_value(Arc::new(ContextModelHost(compacting.then(|| {
                Arc::new(compaction_support::Window::default()) as Arc<dyn ResolveContextModel>
            }))))
            .session_value(Arc::new(CompactionStrategyHost(compacting.then(|| {
                Arc::new(compaction_support::Window::default()) as Arc<dyn CompactionStrategy>
            }))))
            .require_feature(
                Feature::new("PrepareModelHost").session("PrepareModelHost", |names| {
                    Ok(Box::new(DefaultPrepareModelPlugin::new(names)?))
                }),
            );
        if compacting {
            dim_agent_capabilities::defaults::install_with_prompt(
                &mut builder,
                Arc::new(EmptyPromptHost),
                Some(ConfigureUpdate {
                    sources: Vec::new(),
                    model: Some(ModelSelection {
                        provider_id: "mock".into(),
                        model_id: "window".into(),
                        capability_snapshot: None,
                        config: None,
                        metadata: None,
                    }),
                }),
            );
        } else {
            dim_agent_capabilities::defaults::install(&mut builder);
        }
        workspace_tools::install(
            &mut builder,
            WorkspaceToolsConfig {
                output_root: output_root.clone(),
            },
        );
        wait::install(&mut builder);
        let probes = seen.clone();
        builder.require_feature(Feature::new("Probe").branch("Probe", move |names| {
            Ok(Box::new(Probe::new(names, hook, probes.clone())?))
        }));
        let agent = MinimalHost::start_with(&builder, |root, _| {
            root.provide_as::<ExecutionSettingsHost>(Arc::new(ExecutionSettingsHost::fixed(
                ExecutionSettings::default(),
            )))?;
            Ok(())
        })
        .await
        .unwrap();
        let process: Vec<_> = agent
            .loader()
            .entries()
            .into_iter()
            .filter(|row| row.instance.is_none())
            .filter_map(|row| row.view)
            .collect();
        wait_active(
            agent.root(),
            &process,
            Duration::from_secs(5),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        Self {
            root: agent.root().clone(),
            store,
            agent,
            model,
            cwd,
            output_root,
            seen,
        }
    }
    fn config(&self) -> SessionConfig {
        SessionConfig {
            cwd: self.cwd.to_string_lossy().into_owned(),
            tools: Some(vec!["exec".into(), "exec_stop".into(), "wait".into()]),
            permission_policy: dim_agent_runtime::PermissionPolicy {
                preset: dim_agent_runtime::PermissionPreset::FullAccess,
                ..Default::default()
            },
            ..SessionConfig::default()
        }
    }
    async fn session(&self) -> Session {
        self.agent.create_session(self.config()).await.unwrap()
    }
    /// Runs one user turn whose model issues `calls`, then replies `done`.
    async fn run(&self, session: &Session, calls: &[(&str, Value)]) {
        self.model.push(calls);
        session
            .submit("go".into(), InputMode::Queue, "main")
            .await
            .unwrap();
    }
    async fn shutdown(self) {
        self.agent.close().await.unwrap();
    }
}

async fn local_events(session: &Session) -> Vec<Arc<AgentEvent>> {
    session.local("main", ..).await.unwrap()
}

async fn until(
    session: &Session,
    what: &str,
    done: impl Fn(&[Arc<AgentEvent>]) -> bool,
) -> Vec<Arc<AgentEvent>> {
    for _ in 0..1500 {
        let events = local_events(session).await;
        if done(&events) {
            return events;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!(
        "timed out waiting for {what}: {:#?}",
        local_events(session).await
    );
}

fn results(events: &[Arc<AgentEvent>]) -> Vec<ToolResult> {
    events
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::ToolResult(data) => Some(data.results.iter().map(|r| r.result.clone())),
            _ => None,
        })
        .flatten()
        .collect()
}

fn text(result: &ToolResult) -> &str {
    match &result.content {
        ToolContent::Text(text) => text,
        ToolContent::Blocks(_) => panic!("exec returns text"),
    }
}

fn code(result: &ToolResult) -> Option<&str> {
    result.structured_content.as_ref()?.get("code")?.as_str()
}

fn count(events: &[Arc<AgentEvent>], key: &str) -> usize {
    events
        .iter()
        .filter(|event| event.kind.key() == key)
        .count()
}

fn ended(events: &[Arc<AgentEvent>]) -> Vec<(Arc<AgentEvent>, ExecEndedData)> {
    events
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::ExecEnded(data) => Some((event.clone(), data.clone())),
            _ => None,
        })
        .collect()
}

fn notifications(events: &[Arc<AgentEvent>]) -> Vec<(String, String)> {
    let tag = |event: &AgentEvent, prefix: &str| {
        event
            .tags
            .iter()
            .flatten()
            .find_map(|tag| tag.strip_prefix(prefix).map(str::to_owned))
    };
    events
        .iter()
        .filter(|event| matches!(event.kind, EventKind::InputMessage(_)))
        .filter_map(|event| Some((tag(event, WAKE_TASK_TAG)?, tag(event, WAKE_RESULT_TAG)?)))
        .collect()
}

fn task_of(result: &ToolResult) -> String {
    last_task(text(result)).expect("running content names the task")
}

fn kill(pid: &str) {
    assert!(
        std::process::Command::new("kill")
            .arg(pid)
            .status()
            .unwrap()
            .success()
    );
}
fn tool_results(events: &[Arc<AgentEvent>]) -> usize {
    count(events, "step/tool-result")
}

/// The local execution profile is sound once its Session and a branch are
/// live: the assembly check reports nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_local_execution_assembly_passes_the_assembly_check() {
    let harness = Harness::new(platform(&[]).await, Hook::None).await;
    let session = harness.session().await;
    session
        .branch(
            "main".into(),
            session.sequence("main").unwrap(),
            BranchOptions {
                prompt: "check".into(),
                source: BranchSource::Host {
                    operation_id: "check".into(),
                },
            },
        )
        .await
        .unwrap();
    let findings = harness.agent.check_assembly().unwrap();
    assert!(findings.is_empty(), "{findings:?}");
    harness.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn exec_and_external_shell_callers_use_live_shell_preferences() {
    use tokio::io::AsyncReadExt;

    let home = scratch("shell-config");
    let config = home.join("exec-shell.json");
    std::fs::write(&config, "invalid json").unwrap();
    let harness = Harness::with_platform(
        configured_platform(home.clone()),
        Hook::None,
        scratch("cwd"),
    )
    .await;
    let platform = harness.root.get::<HostPlatform>().unwrap();
    let session = harness.session().await;

    let refusal = platform
        .build_process_spec("true", &harness.cwd)
        .await
        .err()
        .unwrap();
    assert_eq!(refusal.code, EXEC_SHELL_CONFIG_INVALID);
    assert!(refusal.reason.contains("Invalid exec shell config"));

    for (index, executable) in ["/bin/sh", "/bin/bash"].into_iter().enumerate() {
        std::fs::write(
            &config,
            json!({"version": 1, "executablePath": executable}).to_string(),
        )
        .unwrap();
        let command = "printf '%s' \"$0\"";
        let (spec, shell) = platform
            .build_process_spec(command, &harness.cwd)
            .await
            .unwrap();
        assert_eq!(spec.executable, PathBuf::from(executable));
        assert_eq!(shell.executable_path, spec.executable);
        let mut process = platform.spawn(spec).unwrap();
        let mut output = Vec::new();
        process.stdout().read_to_end(&mut output).await.unwrap();
        assert!(process.wait().await.unwrap().success());
        let mut decoder = Decoder::default();
        let decoded = format!("{}{}", decoder.write(&output), decoder.end());
        assert_eq!(truncate_preview(&decoded), executable);

        harness
            .run(&session, &[("exec", json!({"command": command}))])
            .await;
        session.idle("main").await.unwrap();
        let events = local_events(&session).await;
        assert_eq!(tool_results(&events), index + 1);
        assert!(text(results(&events).last().unwrap()).contains(executable));
        assert!(Arc::ptr_eq(
            &platform,
            &harness.root.get::<HostPlatform>().unwrap()
        ));
    }

    std::fs::write(&config, r#"{"version":1,"executablePath":"/missing/sh"}"#).unwrap();
    let marker = harness.cwd.join("must-not-run");
    let command = "touch must-not-run";
    let refusal = platform
        .build_process_spec(command, &harness.cwd)
        .await
        .err()
        .unwrap();
    assert_eq!(refusal.code, EXEC_SHELL_UNAVAILABLE);
    assert!(refusal.reason.contains("/missing/sh"));
    harness
        .run(&session, &[("exec", json!({"command": command}))])
        .await;
    session.idle("main").await.unwrap();
    let events = local_events(&session).await;
    assert_eq!(
        code(results(&events).last().unwrap()),
        Some("exec_shell_unavailable")
    );
    assert!(!marker.exists());

    std::fs::remove_file(&config).unwrap();
    harness
        .run(
            &session,
            &[("exec", json!({"command": "printf recovered"}))],
        )
        .await;
    session.idle("main").await.unwrap();
    let events = local_events(&session).await;
    assert!(text(results(&events).last().unwrap()).contains("recovered"));
    harness.shutdown().await;
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn foreground_output_is_canonical_and_offloaded_like_ts() {
    let harness = Harness::new(platform(&[]).await, Hook::None).await;
    let session = harness.session().await;
    harness
        .run(
            &session,
            &[(
                "exec",
                json!({"command": "printf 'ok\\377\\n'; printf 'err\\n' >&2; exit 3"}),
            )],
        )
        .await;
    let events = until(&session, "result", |events| tool_results(events) == 1).await;
    let result = &results(&events)[0];
    assert_eq!(result.is_error, Some(true));
    assert!(text(result).contains("ok<0xFF>"), "{}", text(result));
    assert!(text(result).contains("err"));
    assert!(text(result).ends_with("\n\nCommand exited with code 3."));
    assert!(!text(result).contains('\u{FFFD}'));

    session.idle("main").await.unwrap();
    harness
        .run(
            &session,
            &[(
                "exec",
                json!({"command": "awk 'BEGIN { for (i=0; i<40000; i++) printf \"x\" }'"}),
            )],
        )
        .await;
    let events = until(&session, "offload", |events| tool_results(events) == 2).await;
    let result = &results(&events)[1];
    let content = text(result);
    let path = content
        .strip_prefix("Output saved to: ")
        .and_then(|rest| rest.lines().next())
        .expect(content);
    assert!(path.starts_with(&*harness.output_root.join(session.id()).to_string_lossy()));
    assert!(content.contains("Output bytes: 40000\n"));
    assert_eq!(std::fs::read(path).unwrap().len(), 40_000);
    let mode =
        std::os::unix::fs::PermissionsExt::mode(&std::fs::metadata(path).unwrap().permissions());
    assert_eq!(mode & 0o777, 0o600);

    session.idle("main").await.unwrap();
    harness
        .run(
            &session,
            &[(
                "exec",
                json!({"command": "awk 'BEGIN { for (i=0; i<672000; i++) printf \"%0100d\", 0 }' | tr 0 y; echo end"}),
            )],
        )
        .await;
    let events = until(&session, "truncation", |events| tool_results(events) == 3).await;
    let result = &results(&events)[2];
    assert!(text(result).contains("Output file truncated at 67108864 bytes."));
    assert_eq!(result.is_error, None);
    session.close().await.unwrap();
    harness.shutdown().await;
}

#[test]
fn exit_drains_output_even_when_offload_is_slow() {
    use std::sync::Condvar;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .max_blocking_threads(1)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let harness = Harness::new(platform(&[]).await, Hook::None).await;
        let session = harness.session().await;
        let mut sizes = Vec::new();
        for attempt in 0..8 {
            harness.run(&session, &[("exec", json!({"command": format!(
                "printf start > started-{attempt}; while [ ! -f go-{attempt} ]; do sleep 0.01; done; awk 'BEGIN {{ for (i=0; i<40000; i++) printf \"x\" }}'; printf done > done-{attempt}"
            )}))]).await;
            tokio::time::timeout(Duration::from_secs(5), async {
                while !harness.cwd.join(format!("started-{attempt}")).exists() {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            }).await.unwrap();
            let released = Arc::new((Mutex::new(false), Condvar::new()));
            let blocker = released.clone();
            let (entered, ready) = tokio::sync::oneshot::channel();
            let occupied = tokio::task::spawn_blocking(move || {
                entered.send(()).unwrap();
                let _ = blocker.1.wait_timeout_while(blocker.0.lock().unwrap(), Duration::from_secs(5), |done| !*done).unwrap();
            });
            ready.await.unwrap();
            std::fs::write(harness.cwd.join(format!("go-{attempt}")), "go").unwrap();
            let produced = tokio::time::timeout(Duration::from_secs(3), async {
                while !harness.cwd.join(format!("done-{attempt}")).exists() {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            }).await;
            tokio::time::sleep(Duration::from_millis(250)).await;
            *released.0.lock().unwrap() = true;
            released.1.notify_one();
            occupied.await.unwrap();
            produced.expect("all output must reach the pipe before releasing offload");
            tokio::time::timeout(Duration::from_secs(5), session.idle("main")).await.unwrap().unwrap();
            let results = results(&local_events(&session).await);
            let result = results.last().unwrap();
            assert_eq!(result.is_error, None);
            let path = result.structured_content.as_ref().unwrap()["outputPath"].as_str().unwrap();
            let bytes = std::fs::read(path).unwrap();
            assert!(bytes.iter().all(|byte| *byte == b'x'));
            sizes.push(bytes.len());
        }
        harness.run(&session, &[("exec", json!({"command": "/bin/sh -c 'sleep 30 & printf inherited'"}))]).await;
        let events = tokio::time::timeout(Duration::from_secs(5), until(&session, "inherited pipe", |events| tool_results(events) == 9)).await.unwrap();
        let results = results(&events);
        assert!(text(results.last().unwrap()).contains("inherited"), "{}", text(results.last().unwrap()));
        session.idle("main").await.unwrap();
        harness.run(&session, &[("exec", json!({"command": "/bin/sh -c 'yes x & printf inherited'"}))]).await;
        tokio::time::timeout(Duration::from_secs(5), until(&session, "writing descendant", |events| tool_results(events) == 10)).await.unwrap();
        harness.shutdown().await;
        assert_eq!(sizes, vec![40_000; 8]);
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unavailable_shell_or_a_missing_execution_port_refuses_the_payload() {
    let unavailable = HostPlatform::current(ShellStatus::Unavailable {
        code: EXEC_SHELL_UNAVAILABLE,
        reason: "Selected command shell is unavailable: /opt/zsh".into(),
    })
    .unwrap();
    let harness = Harness::new(unavailable, Hook::None).await;
    let session = harness.session().await;
    let marker = harness.cwd.join("a");
    harness
        .run(
            &session,
            &[(
                "exec",
                json!({"command": format!("touch {}", marker.display())}),
            )],
        )
        .await;
    let events = until(&session, "result", |events| tool_results(events) == 1).await;
    let result = &results(&events)[0];
    assert_eq!(code(result), Some("exec_shell_unavailable"));
    assert!(text(result).contains(EXEC_SHELL_UNAVAILABLE));
    assert!(text(result).contains("/opt/zsh"));
    assert!(!marker.exists());
    session.close().await.unwrap();
    harness.shutdown().await;

    let without_port = HostPlatform::current(ShellStatus::Available(ExecShellRuntime {
        executable_path: "/bin/sh".into(),
        edition: ShellEdition::Sh,
        source: ShellSource::Fallback,
        output_encoding: OutputEncoding::Utf8,
    }))
    .unwrap();
    let harness = Harness::new(without_port, Hook::None).await;
    let session = harness.session().await;
    let marker = harness.cwd.join("b");
    harness
        .run(
            &session,
            &[(
                "exec",
                json!({"command": format!("touch {}", marker.display())}),
            )],
        )
        .await;
    let events = until(&session, "result", |events| tool_results(events) == 1).await;
    let result = &results(&events)[0];
    assert_eq!(code(result), Some("isolation_config_invalid"));
    assert!(
        text(result).contains("no execution settings port"),
        "{}",
        text(result)
    );
    assert!(!marker.exists());
    session.close().await.unwrap();
    harness.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn find_scans_only_its_starting_points() {
    let harness = Harness::new(platform(&[]).await, Hook::None).await;
    let session = harness.session().await;
    std::fs::create_dir_all(harness.cwd.join("src/node_modules")).unwrap();
    harness
        .run(
            &session,
            &[
                (
                    "exec",
                    json!({"command": "find -L src -name node_modules -type d"}),
                ),
                (
                    "exec",
                    json!({"command": "find src -not -path '*/node_modules/*' ! -name build"}),
                ),
                ("exec", json!({"command": "find src/node_modules -name x"})),
            ],
        )
        .await;
    let events = until(&session, "results", |events| tool_results(events) == 1).await;
    let results = results(&events);
    assert_eq!(text(&results[0]), "src/node_modules\n");
    assert_eq!(results[1].is_error, None, "{}", text(&results[1]));
    assert_eq!(code(&results[2]), Some("exec_high_cost_target_refused"));
    session.close().await.unwrap();
    harness.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn commands_see_the_service_environment_with_forced_variables() {
    let harness = Harness::new(
        platform(&[("FORCE_COLOR", "1"), ("DIM_SERVICE_MARK", "kept")]).await,
        Hook::None,
    )
    .await;
    let session = harness.session().await;
    harness
        .run(
            &session,
            &[
                ("exec", json!({"command": "env"})),
                ("exec", json!({"command": "env", "runInBackground": true})),
            ],
        )
        .await;
    let events = until(&session, "background env", |events| {
        !ended(events).is_empty() && tool_results(events) >= 1
    })
    .await;
    let foreground = text(&results(&events)[0]).to_owned();
    let background = &ended(&events)[0].1.result;
    let path = text(background)
        .strip_prefix("Output saved to: ")
        .and_then(|rest| rest.lines().next())
        .unwrap()
        .to_owned();
    for output in [foreground, std::fs::read_to_string(path).unwrap()] {
        for forced in [
            "TERM=dumb",
            "NO_COLOR=1",
            "PAGER=cat",
            "GIT_PAGER=cat",
            "GH_PAGER=cat",
        ] {
            assert!(
                output.lines().any(|line| line == forced),
                "{forced}: {output}"
            );
        }
        assert!(output.contains("DIM_SERVICE_MARK="));
        assert!(!output.contains("FORCE_COLOR"));
    }
    session.close().await.unwrap();
    harness.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn timeout_and_cancellation_release_the_whole_tree_before_the_result() {
    let harness = Harness::new(platform(&[]).await, Hook::None).await;
    let session = harness.session().await;
    let pid_file = harness.cwd.join("timeout.pid");
    let command = format!("sh -c 'sleep 100 & echo $! > {}; wait'", pid_file.display());
    harness
        .run(
            &session,
            &[("exec", json!({ "command": command, "timeoutMs": 1000 }))],
        )
        .await;
    let pid = pid_in(&pid_file).await;
    assert!(alive(&pid));
    let events = until(&session, "timeout", |events| tool_results(events) == 1).await;
    let result = &results(&events)[0];
    assert!(
        text(result).ends_with("Command timed out."),
        "{}",
        text(result)
    );
    assert_eq!(
        result.effects,
        Some(vec![ToolEffect::CommandRun {
            command: command.clone(),
            exit_code: None,
            signal: None,
        }]),
        "the tool stopped the command and observed no exit"
    );
    assert!(gone(&pid).await);

    session.idle("main").await.unwrap();
    let pid_file = harness.cwd.join("cancel.pid");
    let command = format!("sh -c 'sleep 100 & echo $! > {}; wait'", pid_file.display());
    harness
        .run(&session, &[("exec", json!({"command": command}))])
        .await;
    let pid = pid_in(&pid_file).await;
    session.stop("main", "user stop".into()).await.unwrap();
    let events = until(&session, "cancel", |events| tool_results(events) == 2).await;
    assert!(!alive(&pid) || gone(&pid).await);
    assert!(text(&results(&events)[1]).contains("Command cancelled."));
    assert_eq!(
        results(&events)[1].effects,
        Some(vec![ToolEffect::CommandRun {
            command: command.clone(),
            exit_code: None,
            signal: None,
        }]),
        "a command that started is declared however it ended"
    );
    session.close().await.unwrap();
    harness.shutdown().await;
}

/// A command cancelled after it started is declared with no exit status, and
/// the digest of a later compaction lists it (ContextCompaction §5.10,
/// acceptance 18(a)).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_cancelled_command_reaches_the_digest_without_an_exit_status() {
    let harness = Harness::new_compacting(platform(&[]).await, Hook::None).await;
    let session = harness.session().await;
    let started = harness.cwd.join("cancel-started");
    let command = format!(
        "awk 'BEGIN {{ for (i = 0; i < 4000; i++) printf \"y\" }}'; echo started > {}; sleep 100",
        started.display()
    );
    harness
        .run(&session, &[("exec", json!({"command": command}))])
        .await;
    for _ in 0..500 {
        if started.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(started.exists(), "the command started before the stop");
    session.stop("main", "user stop".into()).await.unwrap();
    let events = until(&session, "cancel", |events| tool_results(events) == 1).await;
    let declared = ToolEffect::CommandRun {
        command: command.clone(),
        exit_code: None,
        signal: None,
    };
    assert!(text(&results(&events)[0]).contains("Command cancelled."));
    assert_eq!(results(&events)[0].effects, Some(vec![declared.clone()]));

    // A later turn gives the compaction a cut that replaces the cancelled
    // call.
    session
        .submit("x".repeat(8_000), InputMode::Queue, "main")
        .await
        .unwrap();
    session.idle("main").await.unwrap();
    let (compaction, events) = compaction_support::compact_now(&session).await;
    let (entries, replacement) = compaction_support::digest(&events, compaction);
    let entries = entries.expect("the digest lists the cancelled command");
    assert!(!entries.dropped);
    assert_eq!(entries.entries.len(), 1, "{:?}", entries.entries);
    let ToolEffect::CommandRun {
        command: listed,
        exit_code,
        signal,
    } = &entries.entries[0]
    else {
        panic!("a command entry: {:?}", entries.entries[0])
    };
    assert!(
        (command.len() <= 200 && listed == &command)
            || (listed.starts_with(&command[..200]) && listed.ends_with('…')),
        "{listed}"
    );
    assert_eq!(
        (exit_code, signal),
        (&None, &None),
        "no exit status was seen"
    );
    let rendered = compaction_support::message_text(&replacement[1]);
    assert!(
        rendered.contains("ran exit not observed: awk 'BEGIN {"),
        "{rendered}"
    );
    session.close().await.unwrap();
    harness.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn exec_results_declare_the_command_they_started() {
    let harness = Harness::new(platform(&[]).await, Hook::None).await;
    let session = harness.session().await;
    harness
        .run(
            &session,
            &[
                ("exec", json!({ "command": "echo declared" })),
                ("exec", json!({ "command": "exit 7" })),
            ],
        )
        .await;
    let events = until(&session, "effects", |events| tool_results(events) == 1).await;
    let declared = results(&events);
    assert_eq!(
        declared[0].effects,
        Some(vec![ToolEffect::CommandRun {
            command: "echo declared".into(),
            exit_code: Some(0),
            signal: None,
        }])
    );
    assert_eq!(
        declared[1].effects,
        Some(vec![ToolEffect::CommandRun {
            command: "exit 7".into(),
            exit_code: Some(7),
            signal: None,
        }]),
        "a non-zero exit is an error result that still declares the exit"
    );
    assert_eq!(declared[1].is_error, Some(true));

    harness
        .run(
            &session,
            &[("exec", json!({ "command": "sleep 1 & wait" }))],
        )
        .await;
    let events = until(&session, "refused", |events| tool_results(events) == 2).await;
    let refused = &results(&events)[2];
    assert_eq!(
        refused.structured_content.as_ref().unwrap()["code"],
        "exec_invalid_request"
    );
    assert_eq!(
        refused.effects, None,
        "a command that never started declares nothing"
    );
    session.close().await.unwrap();
    harness.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn background_completion_is_recorded_once_and_wakes_with_a_bounded_notice() {
    let harness = Harness::new(platform(&[]).await, Hook::None).await;
    let session = harness.session().await;
    let command =
        "sleep 1; awk 'BEGIN { for (i=0; i<200000; i++) printf \"z\" }'; echo tail-marker";
    harness
        .run(
            &session,
            &[(
                "exec",
                json!({ "command": command, "runInBackground": true }),
            )],
        )
        .await;
    let events = until(&session, "wake", |events| !notifications(events).is_empty()).await;
    let started: Vec<_> = events
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::ExecStarted(data) => Some((event.sequence, data.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(started.len(), 1);
    let (started_at, data) = &started[0];
    let result_at = events
        .iter()
        .find(|event| event.kind.key() == "step/tool-result")
        .unwrap()
        .sequence;
    assert!(*started_at < result_at);
    assert_eq!(
        results(&events)[0].effects,
        Some(vec![ToolEffect::CommandRun {
            command: command.into(),
            exit_code: None,
            signal: None,
        }]),
        "the handoff observed that the command started and nothing more"
    );
    let running = text(&results(&events)[0]).to_owned();
    assert!(running.starts_with("Command started in background.\nTask ID: "));
    assert!(running.contains(&format!("Task ID: {}", data.task_id)));
    assert!(running.contains(&format!("Output: {}", data.output_path)));

    let ends = ended(&events);
    assert_eq!(ends.len(), 1);
    let (end_event, end) = &ends[0];
    assert_eq!(end.status, ExecStatus::Completed);
    assert_eq!(
        notifications(&events),
        vec![(data.task_id.clone(), end_event.id.clone())]
    );
    let wake = events
        .iter()
        .find(|event| {
            matches!(&event.kind, EventKind::InputMessage(input) if !input.blocks.is_empty())
        })
        .unwrap();
    assert!(wake.sequence > end_event.sequence);
    let EventKind::InputMessage(input) = &wake.kind else {
        unreachable!()
    };
    assert_eq!(input.mode, Some(InputMode::Steer));
    let [InputBlock::Raw { text: notice, .. }] = input.blocks.as_slice() else {
        panic!("{input:?}");
    };
    assert!(notice.contains("[Preview truncated at 16 KiB.]"));
    assert!(notice.len() < 16 * 1024 + 1024);
    assert!(text(&end.result).contains(&format!("Output saved to: {}", data.output_path)));
    let full = std::fs::read_to_string(&data.output_path).unwrap();
    assert_eq!(full.len(), 200_000 + "tail-marker\n".len());
    until(&session, "woken turn", |events| {
        count(events, "turn/end") == 2
    })
    .await;
    session.close().await.unwrap();
    harness.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_model_stops_its_detached_exec_and_keeps_an_already_ended_result() {
    let harness = Harness::new(platform(&[]).await, Hook::None).await;
    let session = harness.session().await;
    let pid_file = harness.cwd.join("model-stop.pid");
    harness.run(&session, &[("exec", json!({"command":format!("sh -c 'sleep 100 & echo $! > {}; wait'", pid_file.display()),"runInBackground":true,"timeoutMs":-1}))]).await;
    let events = until(&session, "detached task", |events| {
        count(events, "turn/end") == 1
    })
    .await;
    let task = task_of(&results(&events)[0]);
    assert!(text(&results(&events)[0]).contains("exec_stop"));
    let pid = pid_in(&pid_file).await;
    harness
        .run(&session, &[("exec_stop", json!({"taskId":task}))])
        .await;
    let events = until(&session, "model stop", |events| results(events).len() == 2).await;
    let stopped = &results(&events)[1];
    assert_ne!(stopped.is_error, Some(true));
    let terminal: ExecEndedData = serde_json::from_str(text(stopped)).unwrap();
    assert_eq!(terminal.task_id, task);
    assert_eq!(terminal.status, ExecStatus::Cancelled);
    assert!(gone(&pid).await);
    assert_eq!(ended(&events).len(), 1);
    session.idle("main").await.unwrap();
    harness
        .run(
            &session,
            &[("exec_stop", json!({"taskId":"member-session"}))],
        )
        .await;
    let events = until(&session, "invalid target", |events| {
        results(events).len() == 3
    })
    .await;
    assert_eq!(code(&results(&events)[2]), Some("exec_task_unknown"));
    assert!(text(&results(&events)[2]).contains("No command was stopped"));
    assert_eq!(ended(&events).len(), 1);
    session.idle("main").await.unwrap();
    harness
        .run(
            &session,
            &[(
                "exec",
                json!({"command":"echo already-done","runInBackground":true}),
            )],
        )
        .await;
    let events = until(&session, "completed command", |events| {
        ended(events).len() == 2
    })
    .await;
    let completed = ended(&events)[1].1.clone();
    assert_eq!(completed.status, ExecStatus::Completed);
    session.idle("main").await.unwrap();
    harness
        .run(
            &session,
            &[("exec_stop", json!({"taskId":completed.task_id}))],
        )
        .await;
    let events = until(&session, "already ended stop", |events| {
        results(events).len() == 5
    })
    .await;
    let again: ExecEndedData = serde_json::from_str(text(&results(&events)[4])).unwrap();
    assert_eq!(again, completed);
    assert_eq!(ended(&events).len(), 2);
    session.close().await.unwrap();
    harness.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stop_and_completion_settle_once_in_dispatch_order() {
    let harness = Harness::new(platform(&[]).await, Hook::None).await;
    let session = harness.session().await;
    let pid_file = harness.cwd.join("bg.pid");
    harness
        .run(
            &session,
            &[(
                "exec",
                json!({
                    "command": format!("sh -c 'sleep 100 & echo $! > {}; wait'", pid_file.display()),
                    "runInBackground": true,
                    "timeoutMs": -1
                }),
            )],
        )
        .await;
    let events = until(&session, "start", |events| count(events, "turn/end") == 1).await;
    let task = task_of(&results(&events)[0]);
    let pid = pid_in(&pid_file).await;
    let background = session.service::<BackgroundExec>("main").unwrap();
    let (first, second) = tokio::join!(background.stop(&task), background.stop(&task));
    let (first, second) = (first.unwrap(), second.unwrap());
    assert_eq!(first, second);
    assert_eq!(first.status, ExecStatus::Cancelled);
    assert!(text(&first.result).contains("Command cancelled by user."));
    assert!(gone(&pid).await);
    assert!(matches!(
        background.stop("exec-unknown").await,
        Err(workspace_tools::background::TaskError::Unknown)
    ));

    harness
        .run(
            &session,
            &[(
                "exec",
                json!({"command": "echo quick", "runInBackground": true}),
            )],
        )
        .await;
    let events = until(&session, "quick", |events| ended(events).len() == 2).await;
    let quick = task_of(&results(&events)[1]);
    until(&session, "quick wake", |events| {
        notifications(events).iter().any(|(task, _)| *task == quick)
    })
    .await;
    let adopted = background.stop(&quick).await.unwrap();
    assert_eq!(adopted.status, ExecStatus::Completed);
    let events = events_after_idle(&session).await;
    assert_eq!(ended(&events).len(), 2);
    assert_eq!(
        notifications(&events)
            .into_iter()
            .map(|(task, _)| task)
            .collect::<Vec<_>>(),
        vec![quick]
    );
    session.close().await.unwrap();
    harness.shutdown().await;
}

async fn events_after_idle(session: &Session) -> Vec<Arc<AgentEvent>> {
    session.idle("main").await.unwrap();
    local_events(session).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_turn_stop_keeps_background_work_and_its_wake_starts_a_turn() {
    let harness = Harness::new(platform(&[]).await, Hook::None).await;
    let session = harness.session().await;
    harness
        .run(
            &session,
            &[
                (
                    "exec",
                    json!({"command": "while [ ! -f finish-after-stop ]; do sleep 0.01; done; echo survived", "runInBackground": true}),
                ),
                ("wait", json!({"timeoutMs": 60000})),
            ],
        )
        .await;
    until(&session, "started", |events| {
        count(events, "exec/started") == 1
    })
    .await;
    session.stop("main", "user stop".into()).await.unwrap();
    std::fs::write(harness.cwd.join("finish-after-stop"), "").unwrap();
    let events = until(&session, "wake", |events| !notifications(events).is_empty()).await;
    let (_, end) = &ended(&events)[0];
    assert_eq!(end.status, ExecStatus::Completed);
    assert!(text(&end.result).contains("survived"));
    until(&session, "woken turn", |events| {
        count(events, "turn/end") == 2
    })
    .await;
    session.idle("main").await.unwrap();
    assert_eq!(count(&local_events(&session).await, "turn/start"), 2);
    harness
        .run(&session, &[("wait", json!({"timeoutMs": 300}))])
        .await;
    let events = until(&session, "later wait", |events| tool_results(events) == 2).await;
    let wait = results(&events).pop().unwrap();
    assert_eq!(
        wait.structured_content.as_ref().unwrap()["reason"],
        "timeout",
        "{}",
        text(&wait)
    );
    session.close().await.unwrap();
    harness.shutdown().await;
}

fn sleeper(pid_file: &Path) -> Value {
    json!({
        "command": format!("sh -c 'sleep 100 & echo $! > {}; wait'", pid_file.display()),
        "runInBackground": true,
        "timeoutMs": -1
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn closing_a_branch_cancels_only_its_tasks_without_a_wake() {
    let harness = Harness::new(platform(&[]).await, Hook::None).await;
    let session = harness.session().await;
    let main_pid = harness.cwd.join("main.pid");
    harness.run(&session, &[("exec", sleeper(&main_pid))]).await;
    until(&session, "main start", |events| {
        count(events, "turn/end") == 1
    })
    .await;
    let main = pid_in(&main_pid).await;
    assert!(alive(&main));
    let child = session
        .branch(
            "main".into(),
            session.sequence("main").unwrap(),
            BranchOptions {
                prompt: "child".into(),
                source: BranchSource::Host {
                    operation_id: "fork".into(),
                },
            },
        )
        .await
        .unwrap();
    session.idle(&child).await.unwrap();
    let child_pid = harness.cwd.join("child.pid");
    harness.model.push(&[("exec", sleeper(&child_pid))]);
    session
        .submit("go".into(), InputMode::Queue, &child)
        .await
        .unwrap();
    let pid = pid_in(&child_pid).await;
    session.idle(&child).await.unwrap();
    let timeline = harness.seen.timeline(&child);
    harness
        .seen
        .tree()
        .get(&child)
        .unwrap()
        .close()
        .await
        .unwrap();
    assert!(gone(&pid).await);
    let child_events = timeline.local(..).await.unwrap();
    let ends = ended(&child_events);
    assert_eq!(ends.len(), 1);
    assert_eq!(ends[0].1.status, ExecStatus::Cancelled);
    assert!(notifications(&child_events).is_empty());
    assert!(alive(&main));
    assert!(ended(&local_events(&session).await).is_empty());
    session.close().await.unwrap();
    assert!(gone(&main).await);
    let ends = ended(&harness.seen.timeline("main").local(..).await.unwrap());
    assert_eq!(ends.len(), 1);
    assert_eq!(ends[0].1.status, ExecStatus::Cancelled);
    harness.shutdown().await;
}

/// Appends events to the store of an unloaded Session, as a generation that
/// stopped right after writing them would have left them.
async fn stored(harness: &Harness, session_id: &str, kinds: Vec<EventKind>) -> Vec<String> {
    let key = TimelineKey {
        session_id: session_id.into(),
        branch_id: "main".into(),
    };
    let mut sequence = harness.store.head(&key).await.unwrap();
    let mut ids = vec![];
    for kind in kinds {
        sequence += 1;
        let event = Arc::new(AgentEvent {
            id: uuid::Uuid::new_v4().to_string(),
            sequence,
            create_at: 0,
            tags: None,
            kind,
        });
        ids.push(event.id.clone());
        let (sent, committed) = tokio::sync::oneshot::channel();
        harness
            .store
            .append(
                key.clone(),
                vec![StoredEvent {
                    event,
                    trace: None,
                    batch_end: sequence,
                }],
                Box::new(move |result| {
                    let _ = sent.send(result);
                }),
            )
            .unwrap();
        committed.await.unwrap().unwrap();
    }
    ids
}

/// Unloads the idle Session. A task's supervisor drops its hold right after
/// appending `exec/ended`, so a test that already saw what followed that end
/// may still find the hold for a moment.
async fn unload_once_released(session: &Session) {
    for _ in 0..500 {
        if session.unload_if_idle().await.unwrap() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("the idle Session was never released");
}

/// Idle unload R2: a running task holds its Session, so an idle unload is
/// refused until the task has ended and its woken turn has run. A restart
/// then wakes once for every end no wake reported, and never again for one
/// that was.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn idle_unload_waits_for_tasks_and_a_restart_wakes_for_every_unreported_end() {
    let harness = Harness::new(platform(&[]).await, Hook::None).await;
    let session = harness.session().await;
    let pid_file = harness.cwd.join("unload.pid");
    harness
        .run(
            &session,
            &[(
                "exec",
                json!({
            "command": format!("/bin/bash -c 'sleep 100 & echo $! > {}; wait'", pid_file.display()),
                    "runInBackground": true, "timeoutMs": -1,
                }),
            )],
        )
        .await;
    let pid = pid_in(&pid_file).await;
    session.idle("main").await.unwrap();
    let events = local_events(&session).await;
    let task = events
        .iter()
        .find_map(|event| match &event.kind {
            EventKind::ExecStarted(data) => Some(&data.task_id),
            _ => None,
        })
        .unwrap();
    assert_eq!(
        session
            .service::<BackgroundExec>("main")
            .unwrap()
            .output(task)
            .await
            .unwrap(),
        ""
    );
    assert!(!session.unload_if_idle().await.unwrap());
    assert!(alive(&pid));
    kill(&pid);
    let events = until(&session, "woken turn", |events| {
        count(events, "turn/end") == 2
    })
    .await;
    let (reported, end) = ended(&events).remove(0);
    assert_eq!(end.status, ExecStatus::Completed);
    session.idle("main").await.unwrap();
    unload_once_released(&session).await;
    let id = session.id().to_owned();
    let started = |task_id: &str| {
        EventKind::ExecStarted(ExecStartedData {
            turn_id: "t".into(),
            request_id: "r".into(),
            call_id: "c".into(),
            task_id: task_id.into(),
            description: task_id.into(),
            output_path: format!("/tmp/{task_id}.log"),
        })
    };
    let lost = stored(
        &harness,
        &id,
        vec![
            started("exec-orphan"),
            started("exec-lost"),
            EventKind::ExecEnded(ExecEndedData {
                task_id: "exec-lost".into(),
                status: ExecStatus::Completed,
                result: ToolResult::text("lost result"),
            }),
        ],
    )
    .await
    .pop()
    .unwrap();

    let restored = harness.agent.restore_session(id).await.unwrap();
    let events = until(&restored, "recovery", |events| {
        ended(events).len() == 3 && notifications(events).len() == 3
    })
    .await;
    let ends = ended(&events);
    for task in ["exec-orphan", "exec-lost"] {
        assert_eq!(
            restored
                .service::<BackgroundExec>("main")
                .unwrap()
                .output(task)
                .await,
            Err(workspace_tools::background::TaskError::Unknown)
        );
    }
    let (orphan_event, orphan) = &ends[2];
    assert_eq!(orphan.task_id, "exec-orphan");
    assert_eq!(orphan.status, ExecStatus::Failed);
    assert!(text(&orphan.result).contains(
        "Background task exec-orphan did not survive a restart of its owner; its final result is unknown. Output so far: /tmp/exec-orphan.log."
    ));
    restored.idle("main").await.unwrap();
    let mut woken: Vec<_> = notifications(&local_events(&restored).await)
        .into_iter()
        .map(|(_, result_event_id)| result_event_id)
        .collect();
    woken.sort();
    let mut expected = vec![reported.id.clone(), lost, orphan_event.id.clone()];
    expected.sort();
    assert_eq!(woken, expected);
    let settled = restored
        .service::<BackgroundExec>("main")
        .unwrap()
        .stop("exec-orphan")
        .await
        .unwrap();
    assert_eq!(settled.status, ExecStatus::Failed);
    restored.close().await.unwrap();
    harness.shutdown().await;
}

/// Idle unload R2: tasks running together each hold the Session until their
/// own end is recorded, whether stopped (`cancelled`, no wake) or failed, so
/// it unloads only once the last end's woken turn has run.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn running_tasks_hold_the_session_until_the_last_one_ends() {
    let harness = Harness::new(platform(&[]).await, Hook::None).await;
    let session = harness.session().await;
    let (stopped_file, failing_file) = (
        harness.cwd.join("stopped.pid"),
        harness.cwd.join("failing.pid"),
    );
    let failing = json!({
        "command": format!("sh -c 'sleep 100 & echo $! > {}; wait $!'", failing_file.display()),
        "runInBackground": true,
        "timeoutMs": -1
    });
    harness
        .run(
            &session,
            &[("exec", sleeper(&stopped_file)), ("exec", failing)],
        )
        .await;
    let events = until(&session, "start", |events| count(events, "turn/end") == 1).await;
    let stopped = task_of(&results(&events)[0]);
    pid_in(&stopped_file).await;
    let failing = pid_in(&failing_file).await;
    session.idle("main").await.unwrap();
    assert!(!session.unload_if_idle().await.unwrap());
    let background = session.service::<BackgroundExec>("main").unwrap();
    assert_eq!(
        background.stop(&stopped).await.unwrap().status,
        ExecStatus::Cancelled
    );
    session.idle("main").await.unwrap();
    assert!(!session.unload_if_idle().await.unwrap());
    kill(&failing);
    let events = until(&session, "woken turn", |events| {
        count(events, "turn/end") == 2
    })
    .await;
    let statuses: Vec<_> = ended(&events).iter().map(|(_, end)| end.status).collect();
    assert_eq!(statuses, vec![ExecStatus::Cancelled, ExecStatus::Failed]);
    session.idle("main").await.unwrap();
    unload_once_released(&session).await;
    harness.shutdown().await;
}

/// Idle unload R2: a reload of BackgroundExec ends its running tasks
/// `cancelled` and their holds go with them; a start the reloading instance
/// still receives is refused and takes none, so the Session unloads after.
/// A start in a closing Session finds no hold to take and is refused too.
/// A refused start stops its command and writes no `exec/started`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_reload_or_a_closing_session_refuses_a_start_and_leaves_no_hold() {
    let harness = Harness::new(platform(&[]).await, Hook::None).await;
    let session = harness.session().await;
    let (running_file, late_file) = (
        harness.cwd.join("running.pid"),
        harness.cwd.join("late.pid"),
    );
    harness
        .run(&session, &[("exec", sleeper(&running_file))])
        .await;
    until(&session, "start", |events| count(events, "turn/end") == 1).await;
    let running = pid_in(&running_file).await;
    let mut held = testing::hold_start(&harness.output_root.join(session.id()));
    harness
        .run(&session, &[("exec", sleeper(&late_file))])
        .await;
    held.arrived().await;
    let late = pid_in(&late_file).await;
    harness.seen.restart_background("main").await.unwrap();
    assert!(gone(&running).await);
    drop(held);
    let events = until(&session, "refused start", |events| {
        count(events, "turn/end") == 2
    })
    .await;
    assert!(gone(&late).await);
    assert_eq!(count(&events, "exec/started"), 1);
    let ends = ended(&events);
    assert_eq!(ends.len(), 1);
    assert_eq!(ends[0].1.status, ExecStatus::Cancelled);
    assert_eq!(code(&results(&events)[1]), Some("background_start_failed"));
    session.idle("main").await.unwrap();
    assert!(session.unload_if_idle().await.unwrap());

    let session = harness.session().await;
    let pid_file = harness.cwd.join("closing.pid");
    let mut held = testing::hold_start(&harness.output_root.join(session.id()));
    harness.run(&session, &[("exec", sleeper(&pid_file))]).await;
    held.arrived().await;
    let pid = pid_in(&pid_file).await;
    let closing = session.close();
    drop(held);
    closing.await.unwrap();
    assert!(gone(&pid).await);
    let events = harness.seen.timeline("main").local(..).await.unwrap();
    assert_eq!(count(&events, "exec/started"), 0);
    harness.shutdown().await;
}

/// An end ordered after the branch-close interrupt does not wake, even when
/// it is not `cancelled` (the command exited on its own just before the
/// close reached it); a later generation does not post it either.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_end_after_the_branch_close_interrupt_does_not_wake() {
    let harness = Harness::new(platform(&[]).await, Hook::None).await;
    let session = harness.session().await;
    session.idle("main").await.unwrap();
    assert!(session.unload_if_idle().await.unwrap());
    let id = session.id().to_owned();
    stored(
        &harness,
        &id,
        vec![
            EventKind::ExecStarted(ExecStartedData {
                turn_id: "t".into(),
                request_id: "r".into(),
                call_id: "c".into(),
                task_id: "exec-late".into(),
                description: "exec-late".into(),
                output_path: "/tmp/exec-late.log".into(),
            }),
            EventKind::StopInterrupt(StopInterruptData {
                reason: "branch closed".into(),
                scope: InterruptScope::Branch,
            }),
            EventKind::ExecEnded(ExecEndedData {
                task_id: "exec-late".into(),
                status: ExecStatus::Completed,
                result: ToolResult::text("finished just before the close"),
            }),
        ],
    )
    .await;
    let restored = harness.agent.restore_session(id).await.unwrap();
    restored.idle("main").await.unwrap();
    assert!(notifications(&local_events(&restored).await).is_empty());
    restored.close().await.unwrap();
    harness.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_before_hook_owns_the_task_identity() {
    let harness = Harness::new(platform(&[]).await, Hook::RewriteTask).await;
    let session = harness.session().await;
    let pid_file = harness.cwd.join("hooked.pid");
    harness.run(&session, &[("exec", sleeper(&pid_file))]).await;
    let events = until(&session, "start", |events| count(events, "turn/end") == 1).await;
    let task = task_of(&results(&events)[0]);
    assert!(task.ends_with("-hooked"), "{task}");
    let stopped = session
        .service::<BackgroundExec>("main")
        .unwrap()
        .stop(&task)
        .await
        .unwrap();
    assert_eq!(stopped.task_id, task);
    assert_eq!(ended(&events_after_idle(&session).await)[0].1.task_id, task);
    session.close().await.unwrap();
    harness.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_long_foreground_command_moves_to_the_background_after_30_seconds() {
    let harness = Harness::new(platform(&[]).await, Hook::None).await;
    let session = harness.session().await;
    harness
        .run(
            &session,
            &[(
                "exec",
                json!({"command": "echo early; sleep 31; echo late"}),
            )],
        )
        .await;
    let events = until(&session, "handoff", |events| tool_results(events) == 1).await;
    let running = text(&results(&events)[0]).to_owned();
    assert!(
        running.starts_with("Command still running after 30000ms.\nTask ID: exec-"),
        "{running}"
    );
    assert!(running.ends_with("Preview:\nearly\n"), "{running}");
    let started = events
        .iter()
        .find(|event| event.kind.key() == "exec/started")
        .unwrap()
        .sequence;
    let result = events
        .iter()
        .find(|event| event.kind.key() == "step/tool-result")
        .unwrap()
        .sequence;
    assert!(started < result);
    let events = until(&session, "late", |events| !notifications(events).is_empty()).await;
    assert!(text(&ended(&events)[0].1.result).contains("late"));
    session.close().await.unwrap();
    harness.shutdown().await;
}

fn wait_texts(events: &[Arc<AgentEvent>]) -> Vec<String> {
    results(events)
        .iter()
        .map(|result| text(result).to_owned())
        .filter(|text| text.starts_with("wait"))
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn wait_publishes_and_accepts_its_exact_timeout_boundaries() {
    let harness = Harness::new(platform(&[]).await, Hook::None).await;
    let session = harness.session().await;
    harness
        .run(
            &session,
            &[
                ("wait", json!({"timeoutMs":0})),
                ("wait", json!({"timeoutMs":1.5})),
                ("wait", json!({"timeoutMs":300001})),
            ],
        )
        .await;
    let events = until(&session, "invalid wait timeouts", |events| {
        results(events).len() == 3
    })
    .await;
    assert!(
        results(&events)
            .iter()
            .all(|result| result.is_error == Some(true))
    );
    assert!(text(&results(&events)[2]).contains("less than or equal to 300000"));
    let tool = events
        .iter()
        .find_map(|event| match &event.kind {
            EventKind::ModelCall(call) => call
                .request
                .tools
                .as_ref()?
                .iter()
                .find(|tool| tool.name == "wait"),
            _ => None,
        })
        .unwrap();
    let timeout = &tool.input_schema.properties.as_ref().unwrap()["timeoutMs"];
    assert_eq!(timeout.schema_type, Some(SchemaType::Integer));
    assert_eq!(timeout.extensions["minimum"], 1);
    assert_eq!(timeout.extensions["maximum"], 300000);
    assert_eq!(timeout.extensions["default"], 300000);
    session.idle("main").await.unwrap();
    harness
        .run(&session, &[("wait", json!({"timeoutMs":1}))])
        .await;
    let events = until(&session, "minimum timeout", |events| {
        results(events).len() == 4
    })
    .await;
    assert_eq!(text(&results(&events)[3]), "wait ended: timeout");
    session.idle("main").await.unwrap();
    harness
        .run(&session, &[("wait", json!({"timeoutMs":300000}))])
        .await;
    until(&session, "maximum wait started", |events| {
        count(events, "step/tool-call") == 3
    })
    .await;
    session
        .submit("continue now".into(), InputMode::Steer, "main")
        .await
        .unwrap();
    let events = until(&session, "maximum wait steered", |events| {
        results(events).len() == 5
    })
    .await;
    assert_eq!(text(&results(&events)[4]), "wait ended: steer");
    session.close().await.unwrap();
    harness.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn wait_ends_on_the_matching_completion_not_on_the_input_that_started_the_turn() {
    let harness = Harness::new(platform(&[]).await, Hook::None).await;
    let session = harness.session().await;
    harness.model.push(&[(
        "exec",
        json!({"command": "sleep 1; echo one", "runInBackground": true}),
    )]);
    harness
        .run(
            &session,
            &[("wait", json!({"taskId": "$TASK", "timeoutMs": 20000}))],
        )
        .await;
    let events = until(&session, "wait", |events| wait_texts(events).len() == 1).await;
    let task = task_of(&results(&events)[0]);
    assert_eq!(
        wait_texts(&events),
        vec![format!("wait ended: notification ({task})")]
    );
    session.idle("main").await.unwrap();

    harness.model.push(&[(
        "exec",
        json!({"command": "echo two", "runInBackground": true}),
    )]);
    harness
        .run(
            &session,
            &[("wait", json!({"taskId": "exec-other", "timeoutMs": 1500}))],
        )
        .await;
    let events = until(&session, "timeout", |events| wait_texts(events).len() == 2).await;
    assert_eq!(wait_texts(&events)[1], "wait ended: timeout");
    session.close().await.unwrap();
    harness.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn wait_ends_on_a_steer_or_a_stop_and_refuses_a_second_wait() {
    let harness = Harness::new(platform(&[]).await, Hook::None).await;
    let session = harness.session().await;
    harness
        .run(&session, &[("wait", json!({"timeoutMs": 20000}))])
        .await;
    until(&session, "tool call", |events| {
        count(events, "step/tool-call") == 1
    })
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    session
        .submit("steer".into(), InputMode::Steer, "main")
        .await
        .unwrap();
    let events = until(&session, "steer", |events| wait_texts(events).len() == 1).await;
    assert_eq!(wait_texts(&events)[0], "wait ended: steer");
    session.idle("main").await.unwrap();

    harness
        .run(
            &session,
            &[
                ("wait", json!({"timeoutMs": 20000})),
                ("wait", json!({"timeoutMs": 20000})),
            ],
        )
        .await;
    until(&session, "second batch", |events| {
        count(events, "step/tool-call") == 2
    })
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    session.stop("main", "user stop".into()).await.unwrap();
    let events = until(&session, "stop", |events| wait_texts(events).len() == 3).await;
    let mut texts = wait_texts(&events)[1..].to_vec();
    texts.sort();
    assert_eq!(
        texts,
        vec![
            "wait ended: interrupted".to_owned(),
            "wait is already running in this session.".to_owned()
        ]
    );
    session.close().await.unwrap();
    harness.shutdown().await;
}

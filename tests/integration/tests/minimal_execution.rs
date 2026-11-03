//! Integration tests for the persistent shell execution capability.
//!
//! Covers the acceptance scenarios from docs/design/appserver/persistent-shell.md §10.

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
    host_platform::{HostPlatform, HostPlatformConfig, HostPlatformPlugin, resolve_shell},
    model_provider::ModelProviderPlugin,
    permission::{CommandRulesHost, NoCommandRules},
    skill_settings::SkillSettingsHost,
    skills::{MemorySkillSettings, SkillsCatalogPlugin},
};
use dim_agent_runtime::testing::MinimalHost;
use dim_agent_runtime::*;
use dim_agent_types::*;
use process_support::{gone, pid_in};
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

/// Replies with queued tool batches, then with text.
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
impl ModelDriver for Script {
    fn invoke(
        &self,
        _request: Arc<ModelRequest>,
        _: ModelConnection,
        _: CancellationToken,
    ) -> HostFuture<'_, ModelStream> {
        Box::pin(async move {
            let id = format!("m{}", self.count.fetch_add(1, Ordering::Relaxed));
            let mut chunks = vec![Ok(ModelChunk::MessageStart {
                message_id: id.clone(),
            })];
            let reason = match self.replies.lock().unwrap().pop_front() {
                Some(calls) => {
                    for (index, (name, input)) in calls.into_iter().enumerate() {
                        let Value::Object(input) = input else {
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
                    home: None,
                },
            )?))
        });
    }
}

fn scratch(name: &str) -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let path = std::env::temp_dir().join(format!(
        "dim-minimal-exec-{}-{}-{name}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
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

struct Harness {
    agent: MinimalHost,
    model: Arc<Script>,
    cwd: PathBuf,
    compacting: bool,
}

impl Harness {
    async fn new(platform: HostPlatform) -> Self {
        Self::with_context(platform, false).await
    }
    /// The same assembly with a context model and a strategy bound, so a flow
    /// test can compact (`local_support/compaction.rs`).
    async fn new_compacting(platform: HostPlatform) -> Self {
        Self::with_context(platform, true).await
    }
    async fn with_context(mut platform: HostPlatform, compacting: bool) -> Self {
        platform.home = scratch("home");
        let cwd = scratch("cwd");
        platform.env.retain(|(name, _)| name != "HOME");
        platform.env.push(("HOME".into(), cwd.as_os_str().into()));
        Self::with_platform_context(test_platform(platform), cwd, compacting).await
    }
    async fn with_platform(platform: impl FnOnce(&mut AgentBuilder), cwd: PathBuf) -> Self {
        Self::with_platform_context(platform, cwd, false).await
    }
    /// A host whose `platform` rows provide HostPlatform, Active before the
    /// harness runs.
    async fn with_platform_context(
        platform: impl FnOnce(&mut AgentBuilder),
        cwd: PathBuf,
        compacting: bool,
    ) -> Self {
        let model = Arc::new(Script::default());
        let mut builder = AgentBuilder::new(AgentConfig::default());
        platform(&mut builder);
        builder
            .timeline_store(Arc::new(MemoryTimelineStore::default()))
            .session_value(Arc::new(HistoryRenderer::default()))
            .process_value(Arc::new(ModelHost(model.clone())))
            .session_value(Arc::new(PrepareContextHost(Arc::new(IdentityContext))))
            .session_value(Arc::new(ContextModelHost(compacting.then(|| {
                Arc::new(compaction_support::Window::default()) as Arc<dyn ResolveContextModel>
            }))))
            .session_value(Arc::new(CommandRulesHost(Arc::new(NoCommandRules))))
            .session_value(Arc::new(CompactionStrategyHost(compacting.then(|| {
                Arc::new(compaction_support::Window::default()) as Arc<dyn CompactionStrategy>
            }))))
            .require_feature(
                Feature::new("PrepareModelHost").session("PrepareModelHost", |names| {
                    Ok(Box::new(DefaultPrepareModelPlugin::new(names)?))
                }),
            );
        dim_agent_capabilities::defaults::install_minimal_execution(
            &mut builder,
            ModelProviderPlugin::new,
        );
        builder.plugin(|names| Ok(Box::new(SkillsCatalogPlugin::new(names)?)));
        let agent = MinimalHost::start_with(&builder, |root, _| {
            root.provide_as::<SkillSettingsHost>(Arc::new(SkillSettingsHost(Arc::new(
                MemorySkillSettings::default(),
            ))))?;
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
            agent,
            model,
            cwd,
            compacting,
        }
    }
    fn config(&self) -> SessionConfig {
        let mut config = SessionConfig {
            cwd: self.cwd.to_string_lossy().into_owned(),
            tools: Some(vec!["execute".into()]),
            permission_policy: PermissionPolicy {
                preset: PermissionPreset::FullAccess,
                ..Default::default()
            },
            ..SessionConfig::default()
        };
        if self.compacting {
            config.model = ModelSelection {
                provider_id: "mock".into(),
                model_id: "window".into(),
                capability_snapshot: None,
                config: None,
                metadata: None,
            };
        }
        config
    }
    async fn session(&self) -> Session {
        self.agent.create_session(self.config()).await.unwrap()
    }
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
        ToolContent::Blocks(_) => panic!("execute returns text"),
    }
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
        .filter_map(|event| {
            Some((
                tag(event, "exec-wake-task:")?,
                tag(event, "exec-wake-result:")?,
            ))
        })
        .collect()
}

fn task_of(result: &ToolResult) -> String {
    let text = text(result);
    let start = text
        .find("Task ID: ")
        .expect("running content names the task")
        + "Task ID: ".len();
    text[start..]
        .chars()
        .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '-')
        .collect()
}

fn tool_results(events: &[Arc<AgentEvent>]) -> usize {
    count(events, "step/tool-result")
}

/// #552: the entry TimelineTree registers for `<dim-branch-line>`.
const IDENTITY_ENTRY: &str = r#"<dim-branch-line branch="C" parent="main" source="tool">: which line of this session you are, placed by the runtime at the start of a line's own history. A later block of this name replaces the earlier one; it is state about the line, not a request."#;

fn system_text(prompt: &SystemPrompt) -> String {
    match prompt {
        SystemPrompt::Text(text) => text.clone(),
        SystemPrompt::Parts(parts) => parts
            .iter()
            .map(|part| {
                let SystemPromptPart::Text { text, .. } = part;
                text.as_str()
            })
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

/// The system of a branch's last model call.
async fn last_system(session: &Session, branch: &str) -> String {
    session
        .history(branch, None)
        .await
        .unwrap()
        .iter()
        .rev()
        .find_map(|event| match &event.kind {
            EventKind::ModelCall(data) => data.request.system.as_deref().map(system_text),
            _ => None,
        })
        .expect("a model call carries the system")
}

/// The minimal execution profile is sound once its Session and a branch are
/// live: the assembly check reports nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_minimal_execution_assembly_passes_the_assembly_check() {
    let harness = Harness::new(platform(&[]).await).await;
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

/// #552: this profile installs no branch tool, so nothing writes the roster
/// and nothing explains it. A host line still carries `<dim-branch-line>`,
/// and the Session part that writes it explains the marker there.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_line_carries_the_identity_entry_without_the_branch_tool() {
    let harness = Harness::new(platform(&[]).await).await;
    let session = harness.session().await;
    let at = session.sequence("main").unwrap();
    let child = session
        .branch(
            "main".into(),
            at,
            BranchOptions {
                prompt: "review the tests".into(),
                source: BranchSource::Host {
                    operation_id: "test".into(),
                },
            },
        )
        .await
        .expect("the minimal profile opens a host line");
    session
        .submit("go".into(), InputMode::Queue, &child)
        .await
        .unwrap();
    session.idle(&child).await.unwrap();

    let events = session.history(&child, None).await.unwrap();
    let opening = events
        .iter()
        .find_map(|event| match &event.kind {
            EventKind::InputMessage(data) => data.blocks.iter().find_map(|block| match block {
                InputBlock::Raw { text, .. } if text.starts_with("<dim-branch-line") => {
                    Some(text.clone())
                }
                _ => None,
            }),
            _ => None,
        })
        .expect("the line's opening input carries its identity");
    assert!(
        opening.starts_with(&format!(
            "<dim-branch-line branch=\"{child}\" parent=\"main\" source=\"host\">"
        )),
        "{opening}"
    );
    let system = last_system(&session, &child).await;
    assert_eq!(system.matches(IDENTITY_ENTRY).count(), 1, "{system}");
    assert!(
        !system.contains("<dim-branch-lines"),
        "no part writes the roster here, so no entry explains it: {system}"
    );
    session.close().await.unwrap();
    harness.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn close_settles_every_admitted_shell_task() {
    let harness = Harness::new(platform(&[]).await).await;
    let session = harness.session().await;
    harness
        .run(
            &session,
            &[(
                "execute",
                json!({"command": "sleep 60", "runInBackground": true}),
            )],
        )
        .await;
    until(&session, "background admission", |events| {
        tool_results(events) == 1
    })
    .await;
    harness
        .run(
            &session,
            &[("execute", json!({"command": "sleep 60", "timeoutMs": 20}))],
        )
        .await;
    until(&session, "occupying admission", |events| {
        count(events, "exec/started") == 2 && tool_results(events) == 2
    })
    .await;
    session.close().await.unwrap();
    let events = local_events(&session).await;
    for start in events.iter().filter_map(|event| match &event.kind {
        EventKind::ExecStarted(data) => Some(data),
        _ => None,
    }) {
        assert_eq!(
            ended(&events)
                .iter()
                .filter(|(_, end)| end.task_id == start.task_id)
                .count(),
            1,
            "admitted task {} must settle exactly once",
            start.task_id
        );
    }
    harness.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn new_persistent_shells_and_environment_refresh_use_current_preferences() {
    let home = scratch("shell-config");
    let config = home.join("exec-shell.json");
    let set_shell = |shell: &str| {
        std::fs::write(
            &config,
            json!({"version":1,"executablePath":shell}).to_string(),
        )
        .unwrap()
    };
    set_shell("/bin/bash");
    let harness = Harness::with_platform(configured_platform(home.clone()), scratch("cwd")).await;
    let session = harness.session().await;
    let alternate = if Path::new("/bin/zsh").is_file() {
        "/bin/zsh"
    } else {
        "/usr/bin/bash"
    };
    set_shell(alternate);
    session
        .configure(
            dim_agent_capabilities::prompt_sources::refresh_update(),
            "main",
        )
        .await
        .unwrap();
    harness
        .run(
            &session,
            &[("execute", json!({"command":"printf 'selected=%s' \"$0\""}))],
        )
        .await;
    session.idle("main").await.unwrap();
    let events = local_events(&session).await;
    assert!(text(results(&events).last().unwrap()).contains(&format!("selected={alternate}")));
    let environment = events
        .iter()
        .rev()
        .find_map(|event| match &event.kind {
            EventKind::InputMessage(input) => input.blocks.iter().find_map(|block| match block {
                InputBlock::Raw { text, .. }
                    if text.contains("<dim-state source=\"dim/environment/context\">") =>
                {
                    Some(text)
                }
                _ => None,
            }),
            _ => None,
        })
        .unwrap();
    assert!(
        environment.contains(&format!("<shell>{alternate}</shell>")),
        "{environment}"
    );
    set_shell("/bin/bash");
    harness
        .run(
            &session,
            &[("execute", json!({"command":"printf 'retained=%s' \"$0\""}))],
        )
        .await;
    session.idle("main").await.unwrap();
    assert!(
        text(results(&local_events(&session).await).last().unwrap())
            .contains(&format!("retained={alternate}"))
    );
    harness
        .run(&session, &[("execute", json!({"command":"exit 7"}))])
        .await;
    session.idle("main").await.unwrap();
    harness
        .run(
            &session,
            &[("execute", json!({"command":"printf 'reset=%s' \"$0\""}))],
        )
        .await;
    session.idle("main").await.unwrap();
    let events = local_events(&session).await;
    assert!(text(results(&events).last().unwrap()).contains("reset=/bin/bash"));
    harness.shutdown().await;
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn foreground_echo_returns_output() {
    let harness = Harness::new(platform(&[]).await).await;
    let session = harness.session().await;
    harness
        .run(&session, &[("execute", json!({"command": "echo hello"}))])
        .await;
    let events = until(&session, "tool result", |events| tool_results(events) == 1).await;
    let result = &results(&events)[0];
    assert!(text(result).contains("hello"), "{}", text(result));
    assert_eq!(result.is_error, None);
    let declared = events
        .iter()
        .find_map(|event| match &event.kind {
            EventKind::ModelCall(data) => data
                .request
                .tools
                .as_ref()?
                .iter()
                .find_map(|tool| (tool.name == "execute").then(|| tool.description.clone())),
            _ => None,
        })
        .unwrap();
    assert!(
        declared.contains("Process isolation in /settings")
            && !declared.contains("host-isolation.json"),
        "{declared}"
    );
    session.close().await.unwrap();
    harness.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn failing_command_reports_exit_code() {
    let harness = Harness::new(platform(&[]).await).await;
    let session = harness.session().await;
    harness
        .run(
            &session,
            &[("execute", json!({"command": "sh -c 'exit 3'"}))],
        )
        .await;
    let events = until(&session, "tool result", |events| tool_results(events) == 1).await;
    let result = &results(&events)[0];
    assert_eq!(result.is_error, Some(true));
    assert!(text(result).contains("exit code 3"), "{}", text(result));
    session.close().await.unwrap();
    harness.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn state_persists_across_calls() {
    let harness = Harness::new(platform(&[]).await).await;
    std::fs::create_dir(harness.cwd.join("child")).unwrap();
    let session = harness.session().await;
    harness
        .run(
            &session,
            &[
                ("execute", json!({"command": "export FOO=bar && cd child"})),
                ("execute", json!({"command": "echo $FOO && pwd"})),
            ],
        )
        .await;
    let events = until(&session, "tool results", |events| {
        results(events).len() == 2
    })
    .await;
    let result = &results(&events)[1];
    assert!(text(result).contains("bar"), "{}", text(result));
    assert!(
        text(result).contains(&*harness.cwd.join("child").to_string_lossy()),
        "{}",
        text(result)
    );
    session.close().await.unwrap();
    harness.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn background_task_returns_task_id_and_completes() {
    let harness = Harness::new(platform(&[]).await).await;
    let session = harness.session().await;
    harness
        .run(
            &session,
            &[(
                "execute",
                json!({"command": "true", "runInBackground": true}),
            )],
        )
        .await;
    let events = until(&session, "exec started", |events| {
        count(events, "exec/started") == 1 && tool_results(events) == 1
    })
    .await;
    let result = &results(&events)[0];
    let task_id = task_of(result);
    assert!(!task_id.is_empty());
    assert!(
        text(result).contains("Background command started."),
        "{}",
        text(result)
    );

    let events = until(&session, "exec ended", |events| ended(events).len() == 1).await;
    let (end_event, end) = &ended(&events)[0];
    assert_eq!(end.status, ExecStatus::Completed);
    assert_eq!(end.task_id, task_id);
    let start = events
        .iter()
        .find(|event| {
            matches!(&event.kind,
        EventKind::ExecStarted(data) if data.task_id == task_id)
        })
        .unwrap();
    assert!(
        start.sequence < end_event.sequence,
        "completion preceded admission"
    );

    let events = until(&session, "wake", |events| !notifications(events).is_empty()).await;
    assert_eq!(
        notifications(&events),
        vec![(task_id.clone(), end_event.id.clone())]
    );
    session.close().await.unwrap();
    harness.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn timeout_occupies_shell_and_wakes() {
    let harness = Harness::new(platform(&[]).await).await;
    let session = harness.session().await;
    harness
        .run(
            &session,
            &[(
                "execute",
                json!({"command": "sleep 0.5 && echo done", "timeoutMs": 100}),
            )],
        )
        .await;
    let events = until(&session, "exec started", |events| {
        count(events, "exec/started") == 1 && tool_results(events) == 1
    })
    .await;
    let result = &results(&events)[0];
    let task_id = task_of(result);
    assert!(!task_id.is_empty());
    assert!(
        text(result).contains("occupies the shell"),
        "{}",
        text(result)
    );

    let events = until(&session, "exec ended", |events| ended(events).len() == 1).await;
    let (end_event, end) = &ended(&events)[0];
    assert_eq!(end.status, ExecStatus::Completed);
    assert_eq!(end.task_id, task_id);

    let events = until(&session, "wake", |events| !notifications(events).is_empty()).await;
    assert_eq!(
        notifications(&events),
        vec![(task_id.clone(), end_event.id.clone())]
    );
    session.close().await.unwrap();
    harness.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stop_background_task() {
    let harness = Harness::new(platform(&[]).await).await;
    let session = harness.session().await;
    let pid_file = harness.cwd.join("bg.pid");
    harness
        .run(
            &session,
            &[(
                "execute",
                json!({
                    "command": format!("sh -c 'sleep 100 & echo $! > {}; wait'", pid_file.display()),
                    "runInBackground": true
                }),
            )],
        )
        .await;
    let events = until(&session, "start", |events| count(events, "turn/end") == 1).await;
    let task_id = task_of(&results(&events)[0]);
    let pid = pid_in(&pid_file).await;

    let stop = session
        .service::<dyn dim_agent_capabilities::exec_stop::ExecStop>("main")
        .unwrap();
    let ended = stop.stop(task_id.clone()).await.unwrap();
    assert_eq!(ended.status, ExecStatus::Cancelled);
    assert!(gone(&pid).await);
    session.close().await.unwrap();
    harness.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shell_exit_settles_tasks() {
    let harness = Harness::new(platform(&[]).await).await;
    let session = harness.session().await;
    harness
        .run(&session, &[("execute", json!({"command": "exit 7"}))])
        .await;
    let events = until(&session, "tool result", |events| tool_results(events) == 1).await;
    let result = &results(&events)[0];
    assert!(
        text(result).contains("[shell exited: code 7]"),
        "{}",
        text(result)
    );

    harness
        .run(&session, &[("execute", json!({"command": "echo after"}))])
        .await;
    let events = until(&session, "reset", |events| tool_results(events) == 2).await;
    let result = &results(&events)[1];
    assert!(text(result).contains("after"), "{}", text(result));
    assert!(
        text(result).contains("[shell was reset]"),
        "{}",
        text(result)
    );
    harness
        .run(
            &session,
            &[(
                "execute",
                json!({"command": "exec /bin/sh -c 'exec </dev/null >/dev/null 2>&1; sleep 0.2; exit 7'"}),
            )],
        )
        .await;
    let events = until(&session, "exit after EOF", |events| {
        tool_results(events) == 3
    })
    .await;
    let result = &results(&events)[2];
    assert!(
        text(result).contains("[shell exited: code 7]"),
        "{}",
        text(result)
    );
    session.close().await.unwrap();
    harness.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn completion_markers_stay_out_of_captures() {
    let harness = Harness::new(platform(&[]).await).await;
    let session = harness.session().await;
    let background = json!({"command": "sleep 0.2", "runInBackground": true});

    harness
        .run(&session, &[("execute", background.clone())])
        .await;
    until(&session, "start", |events| tool_results(events) == 1).await;
    harness
        .run(
            &session,
            &[("execute", json!({"command": "sleep 0.6 && echo fg"}))],
        )
        .await;
    let events = until(&session, "foreground", |events| tool_results(events) == 2).await;
    let result = &results(&events)[1];
    assert!(text(result).contains("fg"), "{}", text(result));
    assert!(!text(result).contains("__DIM_"), "{}", text(result));

    harness.run(&session, &[("execute", background)]).await;
    until(&session, "start", |events| tool_results(events) == 3).await;
    harness
        .run(
            &session,
            &[(
                "execute",
                json!({"command": "sleep 0.6 && echo occ", "timeoutMs": 100}),
            )],
        )
        .await;
    let events = until(&session, "occupying end", |events| ended(events).len() == 3).await;
    let occupying = task_of(&results(&events)[3]);
    let (_, end) = ended(&events)
        .into_iter()
        .find(|(_, data)| data.task_id == occupying)
        .unwrap();
    assert_eq!(end.status, ExecStatus::Completed);
    assert!(text(&end.result).contains("occ"), "{}", text(&end.result));
    let output_path = end.result.structured_content.as_ref().unwrap()["outputPath"]
        .as_str()
        .unwrap()
        .to_owned();
    let output = std::fs::read_to_string(output_path).unwrap();
    assert!(output.contains("occ"), "{output}");
    assert!(!output.contains("__DIM_"), "{output}");
    session.close().await.unwrap();
    harness.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn occupying_task_settles_after_large_output() {
    let harness = Harness::new(platform(&[]).await).await;
    let session = harness.session().await;
    harness
        .run(
            &session,
            &[(
                "execute",
                json!({"command": "sleep 0.3 && seq 1 30000 && echo tail", "timeoutMs": 100}),
            )],
        )
        .await;
    let events = until(&session, "exec ended", |events| ended(events).len() == 1).await;
    let (_, end) = &ended(&events)[0];
    assert_eq!(end.status, ExecStatus::Completed);
    let output_path = end.result.structured_content.as_ref().unwrap()["outputPath"]
        .as_str()
        .unwrap()
        .to_owned();
    let output = std::fs::read_to_string(output_path).unwrap();
    assert!(
        output.ends_with("30000\ntail\n"),
        "{}",
        &output[output.len() - 100..]
    );
    assert!(!output.contains("__DIM_"));
    session.close().await.unwrap();
    harness.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shell_exit_kills_background_jobs() {
    let harness = Harness::new(platform(&[]).await).await;
    let session = harness.session().await;
    let pid_file = harness.cwd.join("bg.pid");
    harness
        .run(
            &session,
            &[(
                "execute",
                json!({
                    "command": format!("sh -c 'sleep 100 & echo $! > {}; wait'", pid_file.display()),
                    "runInBackground": true
                }),
            )],
        )
        .await;
    let events = until(&session, "start", |events| tool_results(events) == 1).await;
    let task_id = task_of(&results(&events)[0]);
    let pid = pid_in(&pid_file).await;

    harness
        .run(&session, &[("execute", json!({"command": "exit 7"}))])
        .await;
    let events = until(&session, "job settled", |events| ended(events).len() == 1).await;
    let (_, end) = &ended(&events)[0];
    assert_eq!(end.task_id, task_id);
    assert_eq!(end.status, ExecStatus::Failed);
    assert!(
        text(&end.result).contains("shell was reset"),
        "{}",
        text(&end.result)
    );
    assert!(gone(&pid).await);
    session.close().await.unwrap();
    harness.shutdown().await;
}

/// An `execute` call declares the command the persistent shell ran with the
/// exit status it observed, and a later compaction's digest lists it
/// (ContextCompaction §5.10, acceptance 18(a)).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn execute_declares_the_command_it_ran_in_the_digest() {
    let harness = Harness::new_compacting(platform(&[]).await).await;
    let session = harness.session().await;
    let command = "printf 'line\\n'; (exit 3)";
    harness
        .model
        .push(&[("execute", json!({"command": command}))]);
    session
        .submit("x".repeat(8_000), InputMode::Queue, "main")
        .await
        .unwrap();
    session.idle("main").await.unwrap();
    let events = until(&session, "the command", |events| tool_results(events) == 1).await;
    let result = &results(&events)[0];
    assert!(
        text(result).contains("[Command finished with exit code 3]"),
        "{}",
        text(result)
    );
    let declared = ToolEffect::CommandRun {
        command: command.into(),
        exit_code: Some(3),
        signal: None,
    };
    assert_eq!(result.effects, Some(vec![declared.clone()]));

    // A later turn gives the compaction a cut that replaces the call.
    session
        .submit("next".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    session.idle("main").await.unwrap();
    let (compaction, events) = compaction_support::compact_now(&session).await;
    let (entries, replacement) = compaction_support::digest(&events, compaction);
    let entries = entries.expect("the digest lists the command");
    assert_eq!(entries.entries, vec![declared]);
    assert!(!entries.dropped);
    let rendered = compaction_support::message_text(&replacement[1]);
    assert!(
        rendered.contains("ran exit 3: printf 'line\\\\n'; (exit 3)"),
        "{rendered}"
    );
    session.close().await.unwrap();
    harness.shutdown().await;
}

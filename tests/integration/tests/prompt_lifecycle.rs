use dim_agent_capabilities::{
    adapters::{DefaultPrepareModelPlugin, EchoModel, MockTools},
    defaults,
    history::HistoryRenderer,
    host::*,
};
use dim_agent_runtime::testing::{branch_events, chain_events, timeline_events};
use dim_agent_runtime::{TimelineQueryScope, testing::MinimalHost, *};
use dim_agent_types::*;
use rutis::Ctx;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, Key, TypeKey};
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
enum Reply {
    Entry(PromptEntry),
    Counted(Arc<AtomicUsize>, PromptEntry),
    Fail,
    Panic,
    Wait(Arc<Notify>, PromptEntry),
    IgnoreCancel(Arc<Notify>, Arc<Notify>, PromptEntry),
    Startup(
        Arc<Notify>,
        CancellationToken,
        Option<Arc<Notify>>,
        PromptEntry,
    ),
}

struct Prompts(Mutex<BTreeMap<String, Reply>>);
impl LoadPrompt for Prompts {
    fn load(
        &self,
        target: PromptSourceTarget,
        cancel: CancellationToken,
    ) -> HostFuture<'_, Option<PromptEntry>> {
        let reply = self.0.lock().unwrap().get(&target.source).cloned();
        Box::pin(async move {
            match reply {
                Some(Reply::Entry(entry)) => Ok(Some(entry)),
                Some(Reply::Counted(calls, entry)) => {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok(Some(entry))
                }
                Some(Reply::Fail) => Err(HostError::failed("load failed")),
                Some(Reply::Panic) => panic!("prompt host panicked"),
                Some(Reply::Wait(release, entry)) => tokio::select! {
                    _ = cancel.cancelled() => Err(HostError::Cancelled("cancelled".into())),
                    _ = release.notified() => Ok(Some(entry)),
                },
                Some(Reply::IgnoreCancel(started, release, entry)) => {
                    started.notify_one();
                    release.notified().await;
                    Ok(Some(entry))
                }
                Some(Reply::Startup(started, cancelled, release, entry)) => {
                    started.notify_one();
                    cancel.cancelled().await;
                    cancelled.cancel();
                    if let Some(release) = release {
                        release.notified().await;
                        Ok(Some(entry))
                    } else {
                        Err(HostError::Cancelled("startup cancelled".into()))
                    }
                }
                None => Ok(None),
            }
        })
    }
}

fn entry(source: &str, version: &str, body: &str) -> PromptEntry {
    PromptEntry {
        source: source.into(),
        version: version.into(),
        body: body.into(),
        scope: PromptScope::Branch {
            timeline: "main".into(),
        },
        delivery: PromptDelivery::System,
        order: 0,
    }
}

fn context_entry(source: &str, version: &str, body: &str) -> PromptEntry {
    PromptEntry {
        delivery: PromptDelivery::Context,
        ..entry(source, version, body)
    }
}

fn configuration(model: &str) -> ModelSelection {
    ModelSelection {
        provider_id: "test".into(),
        model_id: model.into(),
        capability_snapshot: Some(ModelCapabilitySnapshot {
            target: ModelId {
                provider_id: "test".into(),
                model_id: model.into(),
            },
            capabilities: ModelCapabilities::with_limits(
                if model == "two" { 98765 } else { 135791 },
                6789,
            ),
            reasoning: None,
            service_tier: None,
        }),
        config: None,
        metadata: None,
    }
}

fn builder(host: Arc<Prompts>) -> AgentBuilder {
    let mut builder = AgentBuilder::new(AgentConfig::default());
    defaults::mock_hosts(&mut builder);
    defaults::install_with_prompt(&mut builder, host, None);
    builder
}

fn builder_with_initial(host: Arc<Prompts>, initial: ConfigureUpdate) -> AgentBuilder {
    let mut builder = AgentBuilder::new(AgentConfig::default());
    defaults::mock_hosts(&mut builder);
    defaults::install_with_prompt(&mut builder, host, Some(initial));
    builder
}

#[tokio::test]
async fn model_adoption_preserves_enabled_tools_on_initialization_and_refresh() {
    for initial in [false, true] {
        let host = Arc::new(Prompts(Mutex::new(BTreeMap::new())));
        let update = ConfigureUpdate {
            sources: vec![],
            model: Some(configuration("selected")),
        };
        let agent = MinimalHost::start(&if initial {
            builder_with_initial(host, update.clone())
        } else {
            builder(host)
        })
        .await
        .unwrap();
        let session = agent
            .create_session(SessionConfig {
                tools: Some(vec!["echo".into()]),
                ..Default::default()
            })
            .await
            .unwrap();
        if !initial {
            session.configure(update, "main").await.unwrap();
        }
        for model in ["selected", "refreshed"] {
            if model == "refreshed" {
                session
                    .configure(
                        ConfigureUpdate {
                            sources: vec![],
                            model: Some(configuration(model)),
                        },
                        "main",
                    )
                    .await
                    .unwrap();
            }
            session
                .submit(
                    "tool:echo {\"text\":\"still available\"}".into(),
                    InputMode::Queue,
                    "main",
                )
                .await
                .unwrap();
            session.idle("main").await.unwrap();
            let events = chain_events(&session, "main", ..).await.unwrap();
            let call = events
                .iter()
                .rev()
                .find_map(|event| match &event.kind {
                    EventKind::ModelCall(data) => Some(data),
                    _ => None,
                })
                .unwrap();
            assert_eq!(call.request.model_id, model);
            assert_eq!(
                call.request.capability_snapshot,
                configuration(model).capability_snapshot
            );
            assert_eq!(
                call.request
                    .tools
                    .as_ref()
                    .unwrap()
                    .iter()
                    .map(|tool| tool.name.as_str())
                    .collect::<Vec<_>>(),
                ["echo"],
            );
            let result = events
                .iter()
                .rev()
                .find_map(|event| match &event.kind {
                    EventKind::ToolResult(data) => Some(data),
                    _ => None,
                })
                .unwrap();
            assert_eq!(result.results.len(), 1);
            assert_eq!(
                result.results[0].result,
                ToolResult::text("still available")
            );
        }
        agent.close().await.unwrap();
    }
}

#[tokio::test]
async fn input_guide_survives_configuration_refresh_and_revocation_without_duplication() {
    let host = Arc::new(Prompts(Mutex::new(BTreeMap::new())));
    let agent = MinimalHost::start(&builder(host.clone())).await.unwrap();
    let session = agent
        .create_session(SessionConfig {
            system_prompt: "legacy rules".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    for (version, expected) in [
        (0, "legacy rules"),
        (1, "configured rules"),
        (2, "refreshed rules"),
        (3, ""),
    ] {
        if version != 0 {
            if version == 3 {
                host.0.lock().unwrap().remove("rules");
            } else {
                host.0.lock().unwrap().insert(
                    "rules".into(),
                    Reply::Entry(entry("rules", &version.to_string(), expected)),
                );
            }
            session
                .configure(
                    ConfigureUpdate {
                        sources: vec![PromptSourceTarget {
                            source: "rules".into(),
                            scope: PromptScope::Branch {
                                timeline: "main".into(),
                            },
                        }],
                        model: Some(configuration("configured-model")),
                    },
                    "main",
                )
                .await
                .unwrap();
        }
        session
            .submit(format!("turn {version}"), InputMode::Queue, "main")
            .await
            .unwrap();
        session.idle("main").await.unwrap();
        let events = chain_events(&session, "main", ..).await.unwrap();
        let call = events
            .iter()
            .rev()
            .find_map(|event| match &event.kind {
                EventKind::ModelCall(data) => Some(data),
                _ => None,
            })
            .unwrap();
        let Some(SystemPrompt::Text(system)) = call.request.system.as_deref() else {
            panic!("system guide missing")
        };
        assert_eq!(system.matches("# How to read your input").count(), 1);
        if expected.is_empty() {
            assert!(system.starts_with("# How to read your input"));
            assert!(!system.contains("refreshed rules"));
        } else {
            assert!(system.starts_with(&format!("{expected}\n\n# How to read your input")));
        }
        if version != 0 {
            let published = events
                .iter()
                .rev()
                .find_map(|event| match &event.kind {
                    EventKind::ContextConfigured(data) => Some(data),
                    _ => None,
                })
                .unwrap();
            assert_eq!(published.system.as_ref(), call.request.system.as_deref());
        }
    }
    agent.close().await.unwrap();
}

#[derive(Clone, Copy)]
enum ConfigurationTamper {
    PromptVersion,
    PromptBody,
    ConfigurationRevision,
}

struct TamperConfiguration(ConfigurationTamper, Key<Timeline>);
impl TamperConfiguration {
    fn new(names: &Names<'_>, tamper: ConfigurationTamper) -> rutis::Result<Self> {
        Ok(Self(tamper, names.key()?))
    }
}
impl DynPlugin for TamperConfiguration {
    fn name(&self) -> &str {
        "TamperConfiguration"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.1.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let timeline = ctx.require_at(&self.1)?;
            let tamper = self.0;
            timeline.before(
                ctx,
                "TamperConfiguration",
                &["prompt/changed", "context/configured"],
                move |mut draft| {
                    match (&mut draft.kind, tamper) {
                        (
                            EventKind::PromptChanged(data),
                            ConfigurationTamper::PromptVersion | ConfigurationTamper::PromptBody,
                        ) => {
                            let PromptChange::Set { entry } = &mut data.changes[0] else {
                                panic!("expected set prompt change")
                            };
                            match tamper {
                                ConfigurationTamper::PromptVersion => {
                                    entry.version = "forged".into()
                                }
                                ConfigurationTamper::PromptBody => entry.body = "forged".into(),
                                ConfigurationTamper::ConfigurationRevision => unreachable!(),
                            }
                        }
                        (
                            EventKind::ContextConfigured(data),
                            ConfigurationTamper::ConfigurationRevision,
                        ) => data.revision = "forged".into(),
                        _ => {}
                    }
                    Ok(draft)
                },
            )?;
            Ok(Effect::Done)
        })
    }
}

struct GatedContext {
    started: Arc<Notify>,
    release: Arc<Notify>,
    calls: AtomicUsize,
}
impl PrepareContext for GatedContext {
    fn prepare(
        &self,
        request: Arc<ModelRequest>,
        _: ContextSource,
        _: CancellationToken,
    ) -> HostFuture<'_, Arc<ModelRequest>> {
        Box::pin(async move {
            if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                self.started.notify_one();
                self.release.notified().await;
            }
            Ok(request)
        })
    }
}

fn builder_with_context(host: Arc<Prompts>, prepare: Arc<dyn PrepareContext>) -> AgentBuilder {
    let mut builder = AgentBuilder::new(AgentConfig::default());
    builder
        .session_value(Arc::new(HistoryRenderer::default()))
        .process_value(Arc::new(ModelHost(Arc::new(EchoModel))))
        .session_value(Arc::new(ToolHost(Arc::new(MockTools))))
        .session_value(Arc::new(PrepareContextHost(prepare)))
        .session_value(Arc::new(ContextModelHost(None)))
        .session_value(Arc::new(CompactionStrategyHost(None)))
        .require_feature(
            Feature::new("PrepareModelHost").session("PrepareModelHost", |names| {
                Ok(Box::new(DefaultPrepareModelPlugin::new(names)?))
            }),
        );
    defaults::install_with_prompt(&mut builder, host, None);
    builder
}

struct RejectSecondRequest(Arc<AtomicUsize>, Key<Timeline>);
impl RejectSecondRequest {
    fn new(names: &Names<'_>, calls: Arc<AtomicUsize>) -> rutis::Result<Self> {
        Ok(Self(calls, names.key()?))
    }
}
impl DynPlugin for RejectSecondRequest {
    fn name(&self) -> &str {
        "RejectSecondPromptRequest"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.1.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let timeline = ctx.require_at(&self.1)?;
            let calls = self.0.clone();
            timeline.before(
                ctx,
                "RejectSecondPromptRequest",
                &["prompt/configure-request"],
                move |mut draft| {
                    if calls.fetch_add(1, Ordering::SeqCst) == 1 {
                        draft.tags = Some(vec!["x".repeat(16 * 1024)]);
                    }
                    Ok(draft)
                },
            )?;
            Ok(Effect::Done)
        })
    }
}

#[tokio::test]
async fn configuration_publication_preserves_trusted_before_changes_atomically() {
    for tamper in [
        ConfigurationTamper::PromptVersion,
        ConfigurationTamper::PromptBody,
        ConfigurationTamper::ConfigurationRevision,
    ] {
        let host = Arc::new(Prompts(Mutex::new(BTreeMap::from([(
            "rules".into(),
            Reply::Entry(entry("rules", "1", "trusted")),
        )]))));
        let mut builder = builder(host);
        builder.require_feature(
            Feature::new("TamperConfiguration").branch("TamperConfiguration", move |names| {
                Ok(Box::new(TamperConfiguration::new(names, tamper)?))
            }),
        );
        let agent = MinimalHost::start(&builder).await.unwrap();
        let session = agent
            .create_session(SessionConfig::default())
            .await
            .unwrap();

        assert_eq!(
            session
                .configure(
                    ConfigureUpdate {
                        sources: vec![PromptSourceTarget {
                            source: "rules".into(),
                            scope: PromptScope::Branch {
                                timeline: "main".into(),
                            },
                        }],
                        model: Some(configuration("one")),
                    },
                    "main",
                )
                .await
                .unwrap(),
            ConfigureOutcome::Adopted
        );
        let events = chain_events(&session, "main", ..).await.unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(
                    event.kind,
                    EventKind::PromptChanged(_) | EventKind::ContextConfigured(_)
                ))
                .count(),
            2
        );
        let changed = events
            .iter()
            .find_map(|event| match &event.kind {
                EventKind::PromptChanged(data) => Some(data),
                _ => None,
            })
            .unwrap();
        let configured = events
            .iter()
            .find_map(|event| match &event.kind {
                EventKind::ContextConfigured(data) => Some(data),
                _ => None,
            })
            .unwrap();
        let PromptChange::Set { entry } = &changed.changes[0] else {
            panic!("expected prompt set")
        };
        match tamper {
            ConfigurationTamper::PromptVersion => assert_eq!(entry.version, "forged"),
            ConfigurationTamper::PromptBody => assert_eq!(entry.body, "forged"),
            ConfigurationTamper::ConfigurationRevision => assert_eq!(configured.revision, "forged"),
        }
        assert_eq!(
            session
                .get::<dim_agent_capabilities::prompt_state::PromptSnapshot>("main")
                .unwrap()
                .unwrap()
                .entries
                .get(&(entry.source.clone(), entry.scope.clone())),
            Some(entry)
        );
        session
            .submit("use final configuration".into(), InputMode::Queue, "main")
            .await
            .unwrap();
        session.idle("main").await.unwrap();
        assert!(chain_events(&session, "main", ..).await.unwrap().iter().any(|event| matches!(&event.kind,
            EventKind::ModelCall(data) if data.configuration.as_ref().is_some_and(|reference| reference.revision == configured.revision))));
        agent.close().await.unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn prepared_model_call_keeps_its_configuration_while_later_turn_uses_refresh() {
    for frozen in [false, true] {
        let started = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let host = Arc::new(Prompts(Mutex::new(BTreeMap::from([(
            "rules".into(),
            Reply::Entry(entry("rules", "1", "rules one")),
        )]))));
        let prepare = Arc::new(GatedContext {
            started: started.clone(),
            release: release.clone(),
            calls: AtomicUsize::new(0),
        });
        let agent = MinimalHost::start(&builder_with_context(host.clone(), prepare))
            .await
            .unwrap();
        let session = agent
            .create_session(SessionConfig::default())
            .await
            .unwrap();
        let target = PromptSourceTarget {
            source: "rules".into(),
            scope: PromptScope::Branch {
                timeline: "main".into(),
            },
        };
        session
            .configure(
                ConfigureUpdate {
                    sources: vec![target.clone()],
                    model: Some(configuration("one")),
                },
                "main",
            )
            .await
            .unwrap();
        let version_one = chain_events(&session, "main", ..)
            .await
            .unwrap()
            .into_iter()
            .find_map(|event| match &event.kind {
                EventKind::ContextConfigured(data) => Some(data.clone()),
                _ => None,
            })
            .unwrap();
        let child = if frozen {
            session
                .submit("first".into(), InputMode::Queue, "main")
                .await
                .unwrap();
            "main".to_owned()
        } else {
            session
                .branch(
                    "main".into(),
                    session.sequence("main").unwrap(),
                    BranchOptions {
                        prompt: "first".into(),
                        source: BranchSource::Host {
                            operation_id: "child".into(),
                        },
                    },
                )
                .await
                .unwrap()
        };
        started.notified().await;

        host.0.lock().unwrap().insert(
            "rules".into(),
            Reply::Entry(entry("rules", "2", "rules two")),
        );
        if frozen {
            session
                .service::<dim_agent_capabilities::prompt::Prompt>("main")
                .unwrap()
                .configure_loaded(
                    Some(configuration("two")),
                    vec![(target, Some(entry("rules", "2", "rules two")))],
                )
                .await
                .unwrap();
        } else {
            session
                .configure(
                    ConfigureUpdate {
                        sources: vec![target],
                        model: Some(configuration("two")),
                    },
                    "main",
                )
                .await
                .unwrap();
        }
        release.notify_waiters();
        session.idle(&child).await.unwrap();

        session
            .submit("second".into(), InputMode::Queue, "main")
            .await
            .unwrap();
        session.idle("main").await.unwrap();

        let child_events = chain_events(&session, &child, ..).await.unwrap();
        let first_call = child_events
            .iter()
            .find_map(|event| match &event.kind {
                EventKind::ModelCall(data) => Some(data),
                _ => None,
            })
            .unwrap();
        let child_configured = child_events
            .iter()
            .find_map(|event| match &event.kind {
                EventKind::ContextConfigured(data)
                    if first_call.configuration.as_ref().is_some_and(|reference| {
                        reference.timeline == data.timeline && reference.revision == data.revision
                    }) =>
                {
                    Some(data)
                }
                _ => None,
            })
            .expect("the first call references a recorded configuration");
        assert_eq!(first_call.request.model_id, "one");
        assert_eq!(
            first_call
                .request
                .capability_snapshot
                .as_ref()
                .unwrap()
                .capabilities
                .context_window,
            135791
        );
        assert_eq!(
            first_call.request.system.as_deref(),
            child_configured.system.as_ref()
        );
        assert_eq!(
            first_call.configuration,
            Some(PromptReference {
                timeline: version_one.timeline.clone(),
                revision: version_one.revision.clone(),
            }),
            "the prepared call keeps the configuration it was prepared with"
        );
        assert_eq!(first_call.prompt, version_one.prompt);

        let main_events = chain_events(&session, "main", ..).await.unwrap();
        let latest_configured = main_events
            .iter()
            .rev()
            .find_map(|event| match &event.kind {
                EventKind::ContextConfigured(data) => Some(data),
                _ => None,
            })
            .unwrap();
        let second_call = main_events
            .iter()
            .rev()
            .find_map(|event| match &event.kind {
                EventKind::ModelCall(data) => Some(data),
                _ => None,
            })
            .unwrap();
        assert_eq!(second_call.request.model_id, "two");
        assert_eq!(
            second_call
                .request
                .capability_snapshot
                .as_ref()
                .unwrap()
                .capabilities
                .context_window,
            98765
        );
        assert_eq!(
            second_call.request.system.as_deref(),
            latest_configured.system.as_ref()
        );
        assert_eq!(
            second_call.configuration,
            Some(PromptReference {
                timeline: "main".into(),
                revision: latest_configured.revision.clone(),
            })
        );
        assert_eq!(second_call.prompt, latest_configured.prompt);
        agent.close().await.unwrap();
    }
}

/// A line sends the System its configuration holds. The parent block reaches
/// the line between two of its turns; each request sends the System its
/// `configuration` reference holds, and each reference resolves in the line's
/// chain, which holds the configuration the line inherited.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_line_records_the_system_it_sends_across_a_parent_block() {
    let host = Arc::new(Prompts(Mutex::new(BTreeMap::new())));
    let agent = MinimalHost::start(&builder(host)).await.unwrap();
    let session = agent
        .create_session(SessionConfig::default())
        .await
        .unwrap();
    session
        .configure(
            ConfigureUpdate {
                sources: vec![],
                model: Some(configuration("one")),
            },
            "main",
        )
        .await
        .unwrap();
    session
        .submit("on main".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    session.idle("main").await.unwrap();
    let child = session
        .branch(
            "main".into(),
            session.sequence("main").unwrap(),
            BranchOptions {
                prompt: "on the line".into(),
                source: BranchSource::Host {
                    operation_id: "child".into(),
                },
            },
        )
        .await
        .unwrap();
    session.idle(&child).await.unwrap();

    session
        .submit("move main".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    session.idle("main").await.unwrap();
    let mut events = branch_events(&session, &child, ..).await.unwrap();
    for _ in 0..600 {
        if parent_block(&events).is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
        events = branch_events(&session, &child, ..).await.unwrap();
    }
    session
        .submit("again".into(), InputMode::Queue, &child)
        .await
        .unwrap();
    session.idle(&child).await.unwrap();

    let events = branch_events(&session, &child, ..).await.unwrap();
    let chain = chain_events(&session, &child, ..).await.unwrap();
    let calls: Vec<(u64, &ModelCallData)> = events
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::ModelCall(data) => Some((event.sequence, data)),
            _ => None,
        })
        .collect();
    assert_eq!(calls.len(), 2, "one request per turn on the line");
    let (block, text) =
        parent_block(&events[..]).expect("main moved after the first turn, so the line was told");
    assert!(
        text.contains(r#"branch="main""#),
        "the block names the line it was cut from: {text}"
    );
    assert!(
        calls[0].0 < block && block < calls[1].0,
        "the block reached the line between its two requests: {calls:?} {block}"
    );
    let Some(SystemPrompt::Text(first_system)) = calls[0].1.request.system.as_deref() else {
        panic!("the first request carries a composed system");
    };
    assert!(
        first_system.contains("<dim-branch-parent"),
        "the line composes the parent entry into its System: {first_system}"
    );
    for (sequence, call) in &calls {
        let reference = call
            .configuration
            .as_ref()
            .unwrap_or_else(|| panic!("the request at {sequence} references a configuration"));
        let configured = chain
            .iter()
            .find_map(|event| match &event.kind {
                EventKind::ContextConfigured(data)
                    if data.revision == reference.revision
                        && data.timeline == reference.timeline =>
                {
                    Some(data)
                }
                _ => None,
            })
            .unwrap_or_else(|| {
                panic!("the request at {sequence} references a missing configuration")
            });
        assert_eq!(
            call.request.system.as_deref(),
            configured.system.as_ref(),
            "the request at {sequence} sends the System its configuration holds"
        );
    }
    agent.close().await.unwrap();
}

/// The `<dim-branch-parent>` block on the line: its sequence and text.
fn parent_block(events: &[Arc<AgentEvent>]) -> Option<(u64, String)> {
    events.iter().find_map(|event| match &event.kind {
        EventKind::InputMessage(data) => data.blocks.iter().find_map(|block| match block {
            InputBlock::Raw { text, .. } if text.starts_with("<dim-branch-parent") => {
                Some((event.sequence, text.clone()))
            }
            _ => None,
        }),
        _ => None,
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn configuration_is_atomic_failure_safe_and_inherited() {
    for frozen in [false, true] {
        let host = Arc::new(Prompts(Mutex::new(BTreeMap::from([
            (
                "rules".into(),
                Reply::Entry(entry("rules", "1", "rules one")),
            ),
            ("broken".into(), Reply::Fail),
        ]))));
        let agent = MinimalHost::start(&builder(host.clone())).await.unwrap();
        let session = agent
            .create_session(SessionConfig::default())
            .await
            .unwrap();
        let frozen_sources = vec![(
            PromptSourceTarget {
                source: "rules".into(),
                scope: PromptScope::Branch {
                    timeline: "main".into(),
                },
            },
            Some(entry("rules", "1", "rules one")),
        )];
        let adopted = if frozen {
            session
                .service::<dim_agent_capabilities::prompt::Prompt>("main")
                .unwrap()
                .configure_loaded(Some(configuration("one")), frozen_sources.clone())
                .await
                .unwrap()
        } else {
            session
                .configure(
                    ConfigureUpdate {
                        sources: frozen_sources
                            .iter()
                            .map(|(target, _)| target.clone())
                            .collect(),
                        model: Some(configuration("one")),
                    },
                    "main",
                )
                .await
                .unwrap()
        };
        assert_eq!(adopted, ConfigureOutcome::Adopted);
        assert_eq!(
            session
                .service::<dim_agent_capabilities::prompt::Prompt>("main")
                .unwrap()
                .configure_loaded(Some(configuration("one")), frozen_sources)
                .await
                .unwrap(),
            ConfigureOutcome::Unchanged
        );
        assert_eq!(
            session
                .get::<dim_agent_capabilities::prompt_state::PromptSnapshot>("main")
                .unwrap()
                .unwrap()
                .configuration
                .unwrap()
                .model_id,
            "one"
        );
        let events = chain_events(&session, "main", ..).await.unwrap();
        let changed = events
            .iter()
            .position(|event| matches!(event.kind, EventKind::PromptChanged(_)))
            .unwrap();
        assert!(matches!(
            events[changed + 1].kind,
            EventKind::ContextConfigured(_)
        ));
        let configured_before = events
            .iter()
            .filter(|event| matches!(event.kind, EventKind::ContextConfigured(_)))
            .count();
        assert!(
            session
                .configure(
                    ConfigureUpdate {
                        sources: vec![PromptSourceTarget {
                            source: "broken".into(),
                            scope: PromptScope::Branch {
                                timeline: "main".into()
                            }
                        }],
                        model: Some(configuration("broken")),
                    },
                    "main"
                )
                .await
                .is_err()
        );
        assert_eq!(
            chain_events(&session, "main", ..)
                .await
                .unwrap()
                .iter()
                .filter(|event| matches!(event.kind, EventKind::ContextConfigured(_)))
                .count(),
            configured_before
        );
        let at = session.sequence("main").unwrap();
        let child = session
            .branch(
                "main".into(),
                at,
                BranchOptions {
                    prompt: "branch".into(),
                    source: BranchSource::Host {
                        operation_id: "prompt".into(),
                    },
                },
            )
            .await
            .unwrap();
        let inherited = chain_events(&session, &child, ..).await.unwrap();
        assert!(inherited.iter().any(|event| matches!(&event.kind, EventKind::PromptChanged(data) if data.changes.iter().any(|change| matches!(change, PromptChange::Set { entry } if entry.version == "1")))));
        session.idle(&child).await.unwrap();
        session
            .configure(
                ConfigureUpdate {
                    sources: vec![],
                    model: Some(configuration("two")),
                },
                "main",
            )
            .await
            .unwrap();
        assert_eq!(
            session
                .get::<dim_agent_capabilities::prompt_state::PromptSnapshot>(&child)
                .unwrap()
                .unwrap()
                .configuration
                .unwrap()
                .model_id,
            "one"
        );
        assert_eq!(
            session
                .get::<dim_agent_capabilities::prompt_state::PromptSnapshot>("main")
                .unwrap()
                .unwrap()
                .configuration
                .unwrap()
                .model_id,
            "two"
        );
        host.0.lock().unwrap().remove("rules");
        assert_eq!(
            session
                .configure(
                    ConfigureUpdate {
                        sources: vec![PromptSourceTarget {
                            source: "rules".into(),
                            scope: PromptScope::Branch {
                                timeline: "main".into()
                            }
                        }],
                        model: None,
                    },
                    &child
                )
                .await
                .unwrap(),
            ConfigureOutcome::Adopted
        );
        let child_state = session
            .get::<dim_agent_capabilities::prompt_state::PromptSnapshot>(&child)
            .unwrap()
            .unwrap();
        let parent_state = session
            .get::<dim_agent_capabilities::prompt_state::PromptSnapshot>("main")
            .unwrap()
            .unwrap();
        assert!(child_state.entries.is_empty());
        assert_eq!(parent_state.system, "rules one");
        host.0.lock().unwrap().insert(
            "rules".into(),
            Reply::Entry(entry("rules", "2", "child rules")),
        );
        assert_eq!(
            session
                .configure(
                    ConfigureUpdate {
                        sources: vec![PromptSourceTarget {
                            source: "rules".into(),
                            scope: PromptScope::Branch {
                                timeline: "main".into()
                            }
                        }],
                        model: None,
                    },
                    &child
                )
                .await
                .unwrap(),
            ConfigureOutcome::Adopted
        );
        assert_eq!(
            session
                .get::<dim_agent_capabilities::prompt_state::PromptSnapshot>(&child)
                .unwrap()
                .unwrap()
                .system,
            "child rules"
        );
        assert_eq!(
            session
                .get::<dim_agent_capabilities::prompt_state::PromptSnapshot>("main")
                .unwrap()
                .unwrap()
                .system,
            "rules one"
        );
        agent.close().await.unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn overlapping_load_is_superseded_and_close_joins_cancellation() {
    for frozen in [false, true] {
        let started = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let host = Arc::new(Prompts(Mutex::new(BTreeMap::from([(
            "rules".into(),
            Reply::IgnoreCancel(
                started.clone(),
                release.clone(),
                entry("rules", "old", "old"),
            ),
        )]))));
        let agent = MinimalHost::start(&builder(host.clone())).await.unwrap();
        let session = agent
            .create_session(SessionConfig::default())
            .await
            .unwrap();
        let target = PromptSourceTarget {
            source: "rules".into(),
            scope: PromptScope::Branch {
                timeline: "main".into(),
            },
        };
        let mut first = tokio::spawn({
            let session = session.clone();
            let target = target.clone();
            async move {
                session
                    .configure(
                        ConfigureUpdate {
                            sources: vec![target],
                            model: None,
                        },
                        "main",
                    )
                    .await
                    .unwrap()
            }
        });
        started.notified().await;
        host.0
            .lock()
            .unwrap()
            .insert("rules".into(), Reply::Entry(entry("rules", "new", "new")));
        let replacement = tokio::spawn({
            let session = session.clone();
            async move {
                if frozen {
                    session
                        .service::<dim_agent_capabilities::prompt::Prompt>("main")
                        .unwrap()
                        .configure_loaded(
                            Some(configuration("one")),
                            vec![(target, Some(entry("rules", "new", "new")))],
                        )
                        .await
                        .unwrap()
                } else {
                    session
                        .configure(
                            ConfigureUpdate {
                                sources: vec![target],
                                model: Some(configuration("one")),
                            },
                            "main",
                        )
                        .await
                        .unwrap()
                }
            }
        });
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut first)
                .await
                .is_err()
        );
        release.notify_waiters();
        assert_eq!(first.await.unwrap(), ConfigureOutcome::Superseded);
        assert_eq!(replacement.await.unwrap(), ConfigureOutcome::Adopted);
        let snapshot = session
            .get::<dim_agent_capabilities::prompt_state::PromptSnapshot>("main")
            .unwrap()
            .unwrap();
        assert!(
            snapshot
                .entries
                .values()
                .all(|entry| entry.version != "old")
        );
        let started = Arc::new(Notify::new());
        let finish = Arc::new(Notify::new());
        host.0.lock().unwrap().insert(
            "rules".into(),
            Reply::IgnoreCancel(
                started.clone(),
                finish.clone(),
                entry("rules", "late", "late"),
            ),
        );
        let pending = tokio::spawn({
            let session = session.clone();
            async move {
                session
                    .configure(
                        ConfigureUpdate {
                            sources: vec![PromptSourceTarget {
                                source: "rules".into(),
                                scope: PromptScope::Branch {
                                    timeline: "main".into(),
                                },
                            }],
                            model: None,
                        },
                        "main",
                    )
                    .await
            }
        });
        started.notified().await;
        let closing = tokio::spawn(async move { agent.close().await });
        assert!(
            tokio::time::timeout(Duration::from_millis(50), async {
                while !closing.is_finished() {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .is_err()
        );
        finish.notify_waiters();
        tokio::time::timeout(Duration::from_secs(2), closing)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(matches!(
            pending.await.unwrap(),
            Ok(ConfigureOutcome::Superseded) | Err(AgentError::Closed(_))
        ));
    }
}

#[tokio::test]
async fn initial_configuration_loads_before_start_and_failure_rolls_back() {
    let target = PromptSourceTarget {
        source: "rules".into(),
        scope: PromptScope::Branch {
            timeline: "main".into(),
        },
    };
    let initial = ConfigureUpdate {
        sources: vec![target.clone()],
        model: Some(configuration("initial")),
    };
    let host = Arc::new(Prompts(Mutex::new(BTreeMap::from([(
        "rules".into(),
        Reply::Entry(entry("rules", "1", "initial rules")),
    )]))));
    let agent = MinimalHost::start(&builder_with_initial(host, initial.clone()))
        .await
        .unwrap();
    let session = agent
        .create_session(SessionConfig::default())
        .await
        .unwrap();
    let events = chain_events(&session, "main", ..).await.unwrap();
    let events = events
        .iter()
        .filter(|event| event.kind.key() != "prompt-state/recovered")
        .collect::<Vec<_>>();
    assert!(matches!(events[0].kind, EventKind::BranchStart(_)));
    assert!(matches!(events[1].kind, EventKind::PromptChanged(_)));
    assert!(matches!(events[2].kind, EventKind::ContextConfigured(_)));
    assert_eq!(
        session
            .get::<dim_agent_capabilities::prompt_state::PromptSnapshot>("main")
            .unwrap()
            .unwrap()
            .entries
            .values()
            .next()
            .unwrap()
            .version,
        "1"
    );
    agent.close().await.unwrap();

    let failing = Arc::new(Prompts(Mutex::new(BTreeMap::from([(
        "rules".into(),
        Reply::Fail,
    )]))));
    let result = MinimalHost::start(&builder_with_initial(failing, initial))
        .await
        .unwrap()
        .create_session(SessionConfig::default())
        .await;
    let error = match result {
        Ok(_) => panic!("initial load failure unexpectedly created a session"),
        Err(error) => error,
    };
    let cause = match &error {
        AgentError::Multiple(errors) => &errors[0],
        cause => cause,
    };
    assert!(
        matches!(cause, AgentError::InvalidInput(message) if message == "prompt source rules failed: load failed")
    );
}

struct OpeningObserved(Arc<Notify>, CancellationToken, Key<Dispatcher>);
impl OpeningObserved {
    fn new(
        names: &Names<'_>,
        observed: Arc<Notify>,
        cancelled: CancellationToken,
    ) -> rutis::Result<Self> {
        Ok(Self(observed, cancelled, names.key()?))
    }
}
impl DynPlugin for OpeningObserved {
    fn name(&self) -> &str {
        "OpeningObserved"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.2.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let dispatcher = ctx.require_at(&self.2)?;
            let observed = self.0.clone();
            dispatcher.subscribe(
                ctx,
                "OpeningObserved",
                &["branch/start", "branch/resume"],
                move |_| {
                    observed.notify_one();
                    Box::pin(async { Ok(()) })
                },
            )?;
            let cancelled = self.1.clone();
            dispatcher.subscribe(ctx, "StartupDrain", &["stop/interrupt"], move |_| {
                let cancelled = cancelled.clone();
                Box::pin(async move {
                    cancelled.cancelled().await;
                    Ok(())
                })
            })?;
            Ok(Effect::Done)
        })
    }
}

struct OpeningFailure(Key<Dispatcher>, Key<BranchOpening>);

struct SlowOpening {
    dispatcher: Key<Dispatcher>,
    opening: Key<BranchOpening>,
    timeline: Key<Timeline>,
    receipts: bool,
    completed: Arc<AtomicUsize>,
}
impl DynPlugin for SlowOpening {
    fn name(&self) -> &str {
        "SlowOpening"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![
            self.dispatcher.erase(),
            self.opening.erase(),
            self.timeline.erase(),
        ]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let dispatcher = ctx.require_at(&self.dispatcher)?;
            let opening = ctx.require_at(&self.opening)?;
            let timeline = ctx.require_at(&self.timeline)?;
            let receipts = self.receipts;
            let completed = self.completed.clone();
            let pending = dispatcher.clone();
            dispatcher.subscribe(
                ctx,
                "SlowOpening",
                &["branch/resume", "test/opening"],
                move |event| {
                    let (opening, completed, timeline, dispatcher) = (
                        opening.clone(),
                        completed.clone(),
                        timeline.clone(),
                        pending.clone(),
                    );
                    Box::pin(async move {
                        if event.kind.key() == "test/opening" {
                            tokio::time::sleep(Duration::from_millis(200)).await;
                            completed.fetch_add(1, Ordering::SeqCst);
                            return Ok(());
                        }
                        if receipts {
                            for step in 1..=7 {
                                let (timeline, completed) = (timeline.clone(), completed.clone());
                                opening.register(Box::pin(async move {
                                    tokio::time::sleep(Duration::from_millis(200 * step)).await;
                                    assert!(
                                        timeline_events(
                                            &timeline,
                                            TimelineQueryScope::Branch,
                                            ..=1001
                                        )
                                        .await?
                                        .len()
                                            >= 1001
                                    );
                                    completed.fetch_add(1, Ordering::SeqCst);
                                    Ok(())
                                }))?;
                            }
                        } else {
                            opening.register(Box::pin(async move {
                                assert!(
                                    timeline_events(&timeline, TimelineQueryScope::Branch, ..=1001)
                                        .await?
                                        .len()
                                        >= 1001
                                );
                                let mut dispatched = dispatcher.watch();
                                for step in 0..7 {
                                    let event = timeline.append(
                                        EventKind::Extension(
                                            ExtensionEvent::new(
                                                "test",
                                                "opening",
                                                serde_json::json!({"step":step}),
                                            )
                                            .unwrap(),
                                        )
                                        .into(),
                                    )?;
                                    loop {
                                        let state = dispatched.borrow_and_update().clone();
                                        if let Some(error) = state.failure {
                                            return Err(error);
                                        }
                                        if state.sequence >= event.sequence {
                                            break;
                                        }
                                        dispatched.changed().await.map_err(|_| {
                                            AgentError::Closed("opening dispatch".into())
                                        })?;
                                    }
                                }
                                Ok(())
                            }))?;
                        }
                        Ok(())
                    })
                },
            )?;
            Ok(Effect::Done)
        })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_restore_keeps_opening_while_dispatch_or_opening_receipts_advance() {
    for receipts in [false, true] {
        let store = Arc::new(MemoryTimelineStore::default());
        store
            .create_session("slow-restore", &SessionConfig::default())
            .await
            .unwrap();
        let timeline = Timeline::new(
            Ctx::root().unwrap(),
            TimelineKey {
                session_id: "slow-restore".into(),
                branch_id: "main".into(),
            },
            Vec::new(),
            store.clone(),
        );
        timeline
            .append(
                EventKind::BranchStart(BranchStartData {
                    parent: None,
                    create_event: None,
                })
                .into(),
            )
            .unwrap();
        for step in 0..1000 {
            timeline
                .append(
                    EventKind::Extension(
                        ExtensionEvent::new("test", "history", serde_json::json!({"step":step}))
                            .unwrap(),
                    )
                    .into(),
                )
                .unwrap();
        }
        core::close_timeline(&timeline).await.unwrap();
        let completed = Arc::new(AtomicUsize::new(0));
        let seen = completed.clone();
        let mut builder = builder(Arc::new(Prompts(Mutex::new(BTreeMap::new()))));
        builder
            .timeline_store(store)
            .startup_timeout(Duration::from_millis(500));
        builder.require_feature(
            Feature::new("SlowOpening").branch("SlowOpening", move |names| {
                Ok(Box::new(SlowOpening {
                    dispatcher: names.key()?,
                    opening: names.key()?,
                    timeline: names.key()?,
                    receipts,
                    completed: seen.clone(),
                }))
            }),
        );
        let agent = MinimalHost::start(&builder).await.unwrap();
        let started = std::time::Instant::now();
        let session = tokio::time::timeout(
            Duration::from_secs(20),
            agent.restore_session("slow-restore".into()),
        )
        .await
        .expect("progressing restore finishes")
        .unwrap();
        assert_eq!(completed.load(Ordering::SeqCst), 7);
        assert!(started.elapsed() > Duration::from_secs(1));
        assert!(session.state("main").is_ok());
        agent.close().await.unwrap();
    }
}

impl OpeningFailure {
    fn new(names: &Names<'_>) -> rutis::Result<Self> {
        Ok(Self(names.key()?, names.key()?))
    }
}
impl DynPlugin for OpeningFailure {
    fn name(&self) -> &str {
        "OpeningFailure"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.0.erase(), self.1.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let dispatcher = ctx.require_at(&self.0)?;
            let opening = ctx.require_at(&self.1)?;
            dispatcher.subscribe(ctx, "OpeningFailure", &["branch/start"], move |_| {
                let opening = opening.clone();
                Box::pin(async move {
                    opening.register(Box::pin(std::future::pending()))?;
                    opening.register(Box::pin(async {
                        Err(AgentError::InvalidInput("opening rejected".into()))
                    }))?;
                    Ok(())
                })
            })?;
            Ok(Effect::Done)
        })
    }
}

#[tokio::test]
async fn opening_failure_is_not_hidden_by_an_earlier_pending_receipt() {
    let mut builder = builder(Arc::new(Prompts(Mutex::new(BTreeMap::new()))));
    builder.require_feature(
        Feature::new("OpeningFailure").branch("OpeningFailure", |names| {
            Ok(Box::new(OpeningFailure::new(names)?))
        }),
    );
    let agent = MinimalHost::start(&builder).await.unwrap();
    let result = tokio::time::timeout(
        Duration::from_secs(1),
        agent.create_session(SessionConfig::default()),
    )
    .await
    .expect("opening failure must be prompt");
    assert!(
        matches!(result, Err(AgentError::InvalidInput(message)) if message == "opening rejected")
    );
    agent.close().await.unwrap();
}

#[tokio::test]
async fn startup_opening_dispatch_is_free_and_close_or_deadline_joins_the_host() {
    for start in ["create", "restore", "resume"] {
        let restore = start != "create";
        for timeout in [false, true] {
            for cooperative in [false, true] {
                let started = Arc::new(Notify::new());
                let cancelled = CancellationToken::new();
                let release = Arc::new(Notify::new());
                let observed = Arc::new(Notify::new());
                let host = Arc::new(Prompts(Mutex::new(BTreeMap::from([(
                    "rules".into(),
                    Reply::Startup(
                        started.clone(),
                        cancelled.clone(),
                        (!cooperative).then(|| release.clone()),
                        entry("rules", "late", "must not publish"),
                    ),
                )]))));
                let store = Arc::new(MemoryTimelineStore::default());
                if restore {
                    store
                        .create_session("startup", &SessionConfig::default())
                        .await
                        .unwrap();
                }
                if start == "resume" {
                    let timeline = Timeline::new(
                        rutis::Ctx::root().unwrap(),
                        TimelineKey {
                            session_id: "startup".into(),
                            branch_id: "main".into(),
                        },
                        Vec::new(),
                        store.clone(),
                    );
                    timeline
                        .append(
                            EventKind::BranchStart(BranchStartData {
                                parent: None,
                                create_event: None,
                            })
                            .into(),
                        )
                        .unwrap();
                    dim_agent_runtime::core::close_timeline(&timeline)
                        .await
                        .unwrap();
                }
                let mut builder = builder_with_initial(
                    host,
                    ConfigureUpdate {
                        sources: vec![PromptSourceTarget {
                            source: "rules".into(),
                            scope: PromptScope::Branch {
                                timeline: "main".into(),
                            },
                        }],
                        model: Some(configuration("late-model")),
                    },
                );
                builder.timeline_store(store.clone());
                builder.startup_timeout(if timeout {
                    Duration::from_millis(300)
                } else {
                    Duration::from_secs(5)
                });
                let signal = observed.clone();
                let cancel_signal = cancelled.clone();
                builder.require_feature(Feature::new("OpeningObserved").branch(
                    "OpeningObserved",
                    move |names| {
                        Ok(Box::new(OpeningObserved::new(
                            names,
                            signal.clone(),
                            cancel_signal.clone(),
                        )?))
                    },
                ));
                let agent = MinimalHost::start(&builder).await.unwrap();
                let creating = agent.clone();
                let mut creation = tokio::spawn(async move {
                    if restore {
                        creating.restore_session("startup".into()).await
                    } else {
                        creating.create_session(SessionConfig::default()).await
                    }
                });
                tokio::time::timeout(Duration::from_secs(1), started.notified())
                    .await
                    .unwrap();
                let dispatched =
                    tokio::time::timeout(Duration::from_millis(100), observed.notified()).await;
                if dispatched.is_err() {
                    release.notify_one();
                }
                assert!(dispatched.is_ok(), "blocked host blocked opening dispatch");
                assert!(
                    !creation.is_finished(),
                    "published before initial prompt was ready"
                );
                let closing = if timeout {
                    None
                } else {
                    let agent = agent.clone();
                    Some(tokio::spawn(async move { agent.close().await }))
                };
                tokio::time::timeout(Duration::from_secs(1), cancelled.cancelled())
                    .await
                    .expect("host must see cancellation before unload");
                if !cooperative {
                    assert!(
                        tokio::time::timeout(Duration::from_millis(30), &mut creation)
                            .await
                            .is_err(),
                        "startup dropped a live host"
                    );
                    if let Some(closing) = &closing {
                        assert!(!closing.is_finished());
                    }
                    release.notify_one();
                }
                let result = tokio::time::timeout(Duration::from_secs(1), creation)
                    .await
                    .unwrap()
                    .unwrap();
                let error = match result {
                    Ok(_) => panic!("cancelled startup published a session"),
                    Err(error) => error,
                };
                let cause = match &error {
                    AgentError::Multiple(errors) => &errors[0],
                    cause => cause,
                };
                if timeout {
                    assert!(
                        matches!(cause, AgentError::Build(message) if message.contains("timed out")),
                        "{error:?}"
                    );
                } else {
                    assert!(matches!(cause, AgentError::Cancelled(_)), "{error:?}");
                }
                if let Some(closing) = closing {
                    tokio::time::timeout(Duration::from_secs(1), closing)
                        .await
                        .unwrap()
                        .unwrap()
                        .unwrap();
                } else {
                    agent.close().await.unwrap();
                }
                if restore {
                    let key = TimelineKey {
                        session_id: "startup".into(),
                        branch_id: "main".into(),
                    };
                    let records = store
                        .read(&key, 1..=store.head(&key).await.unwrap())
                        .await
                        .unwrap();
                    assert!(
                        !records.iter().any(|record| matches!(
                            record.event.kind,
                            EventKind::ContextConfigured(_)
                        )),
                        "cancelled initial result published"
                    );
                }
            }
        }
    }
}

#[tokio::test]
async fn child_opening_deadline_or_close_joins_before_recording_failure() {
    for timeout in [false, true] {
        let host = Arc::new(Prompts(Mutex::new(BTreeMap::from([(
            "rules".into(),
            Reply::Entry(entry("rules", "1", "ready")),
        )]))));
        let store = Arc::new(MemoryTimelineStore::default());
        let mut builder = builder_with_initial(
            host.clone(),
            ConfigureUpdate {
                sources: vec![PromptSourceTarget {
                    source: "rules".into(),
                    scope: PromptScope::Branch {
                        timeline: "main".into(),
                    },
                }],
                model: Some(configuration("initial")),
            },
        );
        builder.timeline_store(store.clone());
        builder.startup_timeout(if timeout {
            Duration::from_millis(300)
        } else {
            Duration::from_secs(5)
        });
        let agent = MinimalHost::start(&builder).await.unwrap();
        let session = agent
            .create_session(SessionConfig::default())
            .await
            .unwrap();
        let started = Arc::new(Notify::new());
        let cancelled = CancellationToken::new();
        let release = Arc::new(Notify::new());
        host.0.lock().unwrap().insert(
            "rules".into(),
            Reply::Startup(
                started.clone(),
                cancelled.clone(),
                Some(release.clone()),
                entry("rules", "late", "late"),
            ),
        );
        let caller = session.clone();
        let at = session.sequence("main").unwrap();
        let mut creation = tokio::spawn(async move {
            caller
                .branch(
                    "main".into(),
                    at,
                    BranchOptions {
                        prompt: "first input must not run".into(),
                        source: BranchSource::Host {
                            operation_id: "startup-child".into(),
                        },
                    },
                )
                .await
        });
        tokio::time::timeout(Duration::from_secs(1), started.notified())
            .await
            .unwrap();
        let closing = if timeout {
            None
        } else {
            let session = session.clone();
            Some(tokio::spawn(async move { session.close().await }))
        };
        tokio::time::timeout(Duration::from_secs(1), cancelled.cancelled())
            .await
            .unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(30), &mut creation)
                .await
                .is_err()
        );
        if let Some(closing) = &closing {
            assert!(!closing.is_finished());
        }
        release.notify_one();
        let result = tokio::time::timeout(Duration::from_secs(1), creation)
            .await
            .unwrap()
            .unwrap();
        assert!(result.is_err());
        if let Some(closing) = closing {
            tokio::time::timeout(Duration::from_secs(1), closing)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
        }
        let key = TimelineKey {
            session_id: session.id().into(),
            branch_id: "main".into(),
        };
        let records = store
            .read(&key, 1..=store.head(&key).await.unwrap())
            .await
            .unwrap();
        let child = records
            .iter()
            .find_map(|record| match &record.event.kind {
                EventKind::BranchCreate(data) => Some(data.branch.clone()),
                _ => None,
            })
            .unwrap();
        assert_eq!(
            records
                .iter()
                .filter(|record| matches!(record.event.kind, EventKind::BranchCreateFailed(_)))
                .count(),
            1
        );
        let key = TimelineKey {
            branch_id: child,
            ..key
        };
        let child = store
            .read(&key, 1..=store.head(&key).await.unwrap())
            .await
            .unwrap();
        assert!(
            child
                .iter()
                .any(|record| matches!(record.event.kind, EventKind::BranchStart(_)))
        );
        assert!(!child.iter().any(|record| matches!(
            record.event.kind,
            EventKind::InputMessage(_) | EventKind::ContextConfigured(_)
        )));
        agent.close().await.unwrap();
    }
}

#[tokio::test]
async fn restoring_a_session_keeps_its_adopted_system_and_model() {
    let store = Arc::new(MemoryTimelineStore::default());
    let config: SessionConfig = serde_json::from_value(serde_json::json!({
        "cwd": ".", "model": {"providerId": "legacy", "modelId": "legacy-model"},
        "systemPrompt": "", "tools": ["echo"]
    }))
    .unwrap();
    assert!(config.model.config.is_none());
    assert!(config.model.metadata.is_none());
    store
        .create_session("restored-prompt", &config)
        .await
        .unwrap();
    let timeline = Timeline::new(
        rutis::Ctx::root().unwrap(),
        TimelineKey {
            session_id: "restored-prompt".into(),
            branch_id: "main".into(),
        },
        Vec::new(),
        store.clone(),
    );
    let tools = Some(MockTools.definitions());
    let tool_sources = vec![ToolSourceVersion {
        key: "workspace".into(),
        version: "workspace-v1".into(),
    }];
    let initial_config = Some(ModelConfig {
        max_tokens: Some(100.0),
        ..Default::default()
    });
    let initial_metadata = Some(RequestMetadata {
        extensions: serde_json::json!({"oldModel": true})
            .as_object()
            .unwrap()
            .clone(),
        ..Default::default()
    });
    timeline
        .append(
            EventKind::ContextConfigured(ContextConfiguredData {
                timeline: "main".into(),
                revision: "adopted".into(),
                previous: None,
                prompt: None,
                settings: Box::new(ModelConfiguration {
                    provider_id: "test".into(),
                    model_id: "adopted-model".into(),
                    capability_snapshot: configuration("adopted-model").capability_snapshot,
                    tools: tools.clone(),
                    tool_sources: tool_sources.clone(),
                    config: initial_config.clone(),
                    metadata: initial_metadata.clone(),
                }),
                system: Some(SystemPrompt::Text("adopted system".into())),
                system_source: None,
            })
            .into(),
        )
        .unwrap();
    dim_agent_runtime::core::close_timeline(&timeline)
        .await
        .unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let host = Arc::new(Prompts(Mutex::new(BTreeMap::from([(
        "rules".into(),
        Reply::Counted(calls.clone(), entry("rules", "new", "new system")),
    )]))));
    let mut builder = builder_with_initial(
        host,
        ConfigureUpdate {
            sources: vec![PromptSourceTarget {
                source: "rules".into(),
                scope: PromptScope::Branch {
                    timeline: "main".into(),
                },
            }],
            model: Some(configuration("initial-model")),
        },
    );
    builder.timeline_store(store);
    let agent = MinimalHost::start(&builder).await.unwrap();
    let session = agent
        .restore_session("restored-prompt".into())
        .await
        .unwrap();
    assert_eq!(
        session.session_service::<SessionConfig>().unwrap().model,
        config.model
    );
    assert_eq!(
        session
            .service::<dyn LoopState>("main")
            .unwrap()
            .state()
            .phase,
        LoopPhase::Idle
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let events = chain_events(&session, "main", ..).await.unwrap();
    let configured: Vec<_> = events
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::ContextConfigured(data) => Some(data),
            _ => None,
        })
        .collect();
    assert_eq!(configured.len(), 2);
    assert_eq!(
        session
            .get::<dim_agent_capabilities::prompt_state::PromptSnapshot>("main")
            .unwrap()
            .unwrap()
            .configuration
            .as_ref(),
        configured.last().map(|data| data.settings.as_ref())
    );
    for data in &configured {
        assert_eq!(data.settings.model_id, "adopted-model");
        assert_eq!(
            data.system,
            Some(SystemPrompt::Text("adopted system".into())),
            "a record written without a source keeps the System it adopted"
        );
        assert_eq!(data.system_source, None);
    }
    let (tools, tool_sources) = (
        configured[1].settings.tools.clone(),
        configured[1].settings.tool_sources.clone(),
    );
    assert!(tool_sources.is_empty());
    assert!(
        !events
            .iter()
            .any(|event| matches!(event.kind, EventKind::PromptChanged(_)))
    );
    session
        .submit("continue".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    session.idle("main").await.unwrap();
    let call = chain_events(&session, "main", ..)
        .await
        .unwrap()
        .into_iter()
        .find_map(|event| match &event.kind {
            EventKind::ModelCall(data) => Some(data.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(call.request.model_id, "adopted-model");
    assert_eq!(
        call.request.capability_snapshot,
        configuration("adopted-model").capability_snapshot
    );
    assert_eq!(call.request.config, initial_config);
    assert_eq!(call.request.metadata, initial_metadata);
    assert_eq!(
        call.request.system.as_deref(),
        Some(&SystemPrompt::Text("adopted system".into()))
    );
    for selection in [
        ModelSelection {
            provider_id: "new-provider".into(),
            model_id: "new-model".into(),
            capability_snapshot: None,
            config: Some(ModelConfig {
                temperature: Some(0.7),
                ..Default::default()
            }),
            metadata: Some(RequestMetadata {
                extensions: serde_json::json!({"newModel": true})
                    .as_object()
                    .unwrap()
                    .clone(),
                ..Default::default()
            }),
        },
        configuration("identity-only-model"),
    ] {
        assert_eq!(
            session
                .configure(
                    ConfigureUpdate {
                        sources: Vec::new(),
                        model: Some(selection.clone())
                    },
                    "main"
                )
                .await
                .unwrap(),
            ConfigureOutcome::Adopted
        );
        session
            .submit("after model selection".into(), InputMode::Queue, "main")
            .await
            .unwrap();
        session.idle("main").await.unwrap();
        let events = chain_events(&session, "main", ..).await.unwrap();
        let configured = events
            .iter()
            .rev()
            .find_map(|event| match &event.kind {
                EventKind::ContextConfigured(data) => Some(&data.settings),
                _ => None,
            })
            .unwrap();
        assert_eq!(configured.tools, tools);
        assert_eq!(configured.tool_sources, tool_sources);
        let request = events
            .iter()
            .rev()
            .find_map(|event| match &event.kind {
                EventKind::ModelCall(data) => Some(&data.request),
                _ => None,
            })
            .unwrap();
        assert_eq!(request.provider_id, selection.provider_id);
        assert_eq!(request.model_id, selection.model_id);
        assert_eq!(
            request.config,
            dim_agent_capabilities::context_model::adopted_config(
                selection.capability_snapshot.as_ref(),
                selection.config.as_ref(),
            )
        );
        assert_eq!(request.metadata, selection.metadata);
        assert_eq!(request.tools.as_deref(), tools.as_ref());
    }
    agent.close().await.unwrap();
}

#[tokio::test]
async fn restoring_a_never_started_session_starts_it() {
    let store = Arc::new(MemoryTimelineStore::default());
    store
        .create_session("empty-prompt-restore", &SessionConfig::default())
        .await
        .unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let host = Arc::new(Prompts(Mutex::new(BTreeMap::from([(
        "rules".into(),
        Reply::Counted(calls.clone(), entry("rules", "1", "initial system")),
    )]))));
    let mut builder = builder_with_initial(
        host,
        ConfigureUpdate {
            sources: vec![PromptSourceTarget {
                source: "rules".into(),
                scope: PromptScope::Branch {
                    timeline: "main".into(),
                },
            }],
            model: Some(configuration("initial-model")),
        },
    );
    builder.timeline_store(store);
    let agent = MinimalHost::start(&builder).await.unwrap();
    let session = agent
        .restore_session("empty-prompt-restore".into())
        .await
        .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(matches!(
        chain_events(&session, "main", ..).await.unwrap()[0].kind,
        EventKind::BranchStart(BranchStartData { parent: None, .. })
    ));
    agent.close().await.unwrap();
}

#[tokio::test]
async fn an_undelivered_initial_context_prompt_is_adopted_again_in_a_child() {
    let calls = Arc::new(AtomicUsize::new(0));
    let target = PromptSourceTarget {
        source: "rules".into(),
        scope: PromptScope::Branch {
            timeline: "main".into(),
        },
    };
    let host = Arc::new(Prompts(Mutex::new(BTreeMap::from([(
        "rules".into(),
        Reply::Counted(calls.clone(), context_entry("rules", "1", "inherited")),
    )]))));
    let agent = MinimalHost::start(&builder_with_initial(
        host,
        ConfigureUpdate {
            sources: vec![target],
            model: None,
        },
    ))
    .await
    .unwrap();
    let session = agent
        .create_session(SessionConfig::default())
        .await
        .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    // `main` runs no turn, so the block that delivers the initial entry is
    // never consumed: the entry is in force but the model never received it.
    let undelivered = chain_events(&session, "main", ..).await.unwrap();
    let main_change = undelivered
        .iter()
        .find_map(|event| match &event.kind {
            EventKind::PromptChanged(data) => Some(data),
            _ => None,
        })
        .expect("main wrote its initial change");
    assert_eq!(
        main_change.inputs.len(),
        1,
        "an Initialize change names its delivery input"
    );
    assert!(
        undelivered
            .iter()
            .all(|event| !matches!(event.kind, EventKind::InputConsumed(_))),
        "nothing consumed the initial delivery input"
    );
    assert!(
        !undelivered
            .iter()
            .any(|event| matches!(event.kind, EventKind::ContextConfigured(_)))
    );
    let child = session
        .branch(
            "main".into(),
            session.sequence("main").unwrap(),
            BranchOptions {
                prompt: "branch".into(),
                source: BranchSource::Host {
                    operation_id: "prompt".into(),
                },
            },
        )
        .await
        .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    // The child's prefix never consumed the delivery block, so it does not
    // inherit the change as delivered: it adopts the source itself, and its
    // own change names its own delivery input.
    let events = branch_events(&session, &child, ..).await.unwrap();
    let changes: Vec<_> = events
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::PromptChanged(data) if data.timeline == child => Some(data.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(changes.len(), 1, "the child adopts the source once");
    let change = &changes[0];
    assert_eq!(
        change.reason,
        PromptChangeReason::Refresh,
        "the child inherited main's revision, so its change is a refresh"
    );
    assert_eq!(change.changes.len(), 1);
    let PromptChange::Set { entry } = &change.changes[0] else {
        panic!("the child sets the rule: {:?}", change.changes)
    };
    assert_eq!(entry.source, "rules");
    assert_eq!(
        change.inputs.len(),
        1,
        "the child's change names its own delivery input"
    );
    let named: Vec<_> = events
        .iter()
        .filter(|event| change.inputs.contains(&event.id))
        .collect();
    assert_eq!(named.len(), 1, "the named input is on the child");
    assert!(
        matches!(named[0].kind, EventKind::InputMessage(_)),
        "the named input is the block that delivers the change"
    );
    assert!(
        session
            .get::<dim_agent_capabilities::prompt_state::PromptSnapshot>(&child)
            .unwrap()
            .unwrap()
            .entries
            .values()
            .any(|entry| entry.version == "1")
    );
    agent.close().await.unwrap();
}

/// The companion: when `main` consumed the initial block, the child inherits
/// the change as delivered and records none of its own.
#[tokio::test]
async fn a_delivered_initial_context_prompt_is_inherited_and_unchanged_in_a_child() {
    let calls = Arc::new(AtomicUsize::new(0));
    let target = PromptSourceTarget {
        source: "rules".into(),
        scope: PromptScope::Branch {
            timeline: "main".into(),
        },
    };
    let host = Arc::new(Prompts(Mutex::new(BTreeMap::from([(
        "rules".into(),
        Reply::Counted(calls.clone(), context_entry("rules", "1", "inherited")),
    )]))));
    let agent = MinimalHost::start(&builder_with_initial(
        host,
        ConfigureUpdate {
            sources: vec![target],
            model: None,
        },
    ))
    .await
    .unwrap();
    let session = agent
        .create_session(SessionConfig::default())
        .await
        .unwrap();
    // One turn consumes the initial delivery block.
    session
        .submit("first".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    session.idle("main").await.unwrap();
    let main = chain_events(&session, "main", ..).await.unwrap();
    let main_change = main
        .iter()
        .find_map(|event| match &event.kind {
            EventKind::PromptChanged(data) => Some(data.clone()),
            _ => None,
        })
        .expect("main wrote its initial change");
    assert!(
        main.iter().any(|event| matches!(
            &event.kind,
            EventKind::InputConsumed(data) if main_change.inputs.contains(&data.input_id)
        )),
        "main consumed the initial delivery input"
    );
    let child = session
        .branch(
            "main".into(),
            session.sequence("main").unwrap(),
            BranchOptions {
                prompt: "branch".into(),
                source: BranchSource::Host {
                    operation_id: "prompt".into(),
                },
            },
        )
        .await
        .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let events = branch_events(&session, &child, ..).await.unwrap();
    assert!(
        !events.iter().any(
            |event| matches!(&event.kind, EventKind::PromptChanged(data) if data.timeline == child)
        ),
        "the child inherits the delivered change and writes none"
    );
    assert!(
        session
            .get::<dim_agent_capabilities::prompt_state::PromptSnapshot>(&child)
            .unwrap()
            .unwrap()
            .entries
            .values()
            .any(|entry| entry.version == "1")
    );
    agent.close().await.unwrap();
}

#[tokio::test]
async fn same_body_new_version_reconfigures_but_identical_update_is_unchanged() {
    let host = Arc::new(Prompts(Mutex::new(BTreeMap::from([(
        "rules".into(),
        Reply::Entry(entry("rules", "1", "same body")),
    )]))));
    let agent = MinimalHost::start(&builder(host.clone())).await.unwrap();
    let session = agent
        .create_session(SessionConfig::default())
        .await
        .unwrap();
    let target = PromptSourceTarget {
        source: "rules".into(),
        scope: PromptScope::Branch {
            timeline: "main".into(),
        },
    };
    let update = ConfigureUpdate {
        sources: vec![target.clone()],
        model: Some(configuration("one")),
    };
    assert_eq!(
        session.configure(update.clone(), "main").await.unwrap(),
        ConfigureOutcome::Adopted
    );
    assert_eq!(
        session.configure(update, "main").await.unwrap(),
        ConfigureOutcome::Unchanged
    );
    host.0.lock().unwrap().insert(
        "rules".into(),
        Reply::Entry(entry("rules", "2", "same body")),
    );
    assert_eq!(
        session
            .configure(
                ConfigureUpdate {
                    sources: vec![target],
                    model: None,
                },
                "main",
            )
            .await
            .unwrap(),
        ConfigureOutcome::Adopted
    );
    let configured = chain_events(&session, "main", ..)
        .await
        .unwrap()
        .into_iter()
        .filter_map(|event| match &event.kind {
            EventKind::ContextConfigured(data) => Some(data.prompt.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(configured.len(), 2);
    assert_ne!(configured[0], configured[1]);
    agent.close().await.unwrap();
}

#[tokio::test]
async fn duplicate_targets_are_rejected_before_loading() {
    let host = Arc::new(Prompts(Mutex::new(BTreeMap::new())));
    let agent = MinimalHost::start(&builder(host)).await.unwrap();
    let session = agent
        .create_session(SessionConfig::default())
        .await
        .unwrap();
    let target = PromptSourceTarget {
        source: "duplicate".into(),
        scope: PromptScope::Branch {
            timeline: "main".into(),
        },
    };
    let error = session
        .configure(
            ConfigureUpdate {
                sources: vec![target.clone(), target],
                model: Some(configuration("one")),
            },
            "main",
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("duplicate prompt target"));
    agent.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn large_request_can_supersede_a_consumed_load_without_a_history_byte_budget() {
    let release = Arc::new(Notify::new());
    let host = Arc::new(Prompts(Mutex::new(BTreeMap::from([(
        "rules".into(),
        Reply::Wait(release.clone(), entry("rules", "1", "valid")),
    )]))));
    let mut builder = builder(host);
    builder.require_feature(Feature::new("RejectSecondPromptRequest").branch(
        "RejectSecondPromptRequest",
        |names| {
            Ok(Box::new(RejectSecondRequest::new(
                names,
                Arc::new(AtomicUsize::new(0)),
            )?))
        },
    ));
    let agent = MinimalHost::start(&builder).await.unwrap();
    let session = agent
        .create_session(SessionConfig::default())
        .await
        .unwrap();
    let first = tokio::spawn({
        let session = session.clone();
        async move {
            session
                .configure(
                    ConfigureUpdate {
                        sources: vec![PromptSourceTarget {
                            source: "rules".into(),
                            scope: PromptScope::Branch {
                                timeline: "main".into(),
                            },
                        }],
                        model: Some(configuration("one")),
                    },
                    "main",
                )
                .await
        }
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    let second = tokio::spawn({
        let session = session.clone();
        async move {
            session
                .configure(
                    ConfigureUpdate {
                        sources: vec![PromptSourceTarget {
                            source: "rules".into(),
                            scope: PromptScope::Branch {
                                timeline: "main".into(),
                            },
                        }],
                        model: Some(configuration("one")),
                    },
                    "main",
                )
                .await
        }
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    release.notify_waiters();
    assert_eq!(first.await.unwrap().unwrap(), ConfigureOutcome::Superseded);
    assert_eq!(second.await.unwrap().unwrap(), ConfigureOutcome::Adopted);
    agent.close().await.unwrap();
}

#[tokio::test]
async fn loader_panic_is_reported_to_the_agent_supervisor() {
    let host = Arc::new(Prompts(Mutex::new(BTreeMap::from([(
        "panic".into(),
        Reply::Panic,
    )]))));
    let agent = MinimalHost::start(&builder(host)).await.unwrap();
    let session = agent
        .create_session(SessionConfig::default())
        .await
        .unwrap();
    let error = session
        .configure(
            ConfigureUpdate {
                sources: vec![PromptSourceTarget {
                    source: "panic".into(),
                    scope: PromptScope::Branch {
                        timeline: "main".into(),
                    },
                }],
                model: None,
            },
            "main",
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("prompt host panicked"));
    let supervised = session
        .configure(
            ConfigureUpdate {
                sources: Vec::new(),
                model: Some(configuration("never")),
            },
            "main",
        )
        .await
        .unwrap_err();
    assert!(supervised.to_string().contains("prompt host panicked"));
    agent.close().await.unwrap();
}

struct RetargetModel {
    replace_snapshot: bool,
    timeline: Key<Timeline>,
}
impl RetargetModel {
    fn new(names: &Names<'_>, replace_snapshot: bool) -> rutis::Result<Self> {
        Ok(Self {
            replace_snapshot,
            timeline: names.key()?,
        })
    }
}
impl DynPlugin for RetargetModel {
    fn name(&self) -> &str {
        "RetargetModel"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.timeline.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let timeline = ctx.require_at(&self.timeline)?;
            let replace_snapshot = self.replace_snapshot;
            timeline.before(ctx, "retarget", &["model/call"], move |mut draft| {
                if let EventKind::ModelCall(call) = &mut draft.kind {
                    let request = Arc::make_mut(&mut call.request);
                    request.model_id = "two".into();
                    if replace_snapshot {
                        request.capability_snapshot = configuration("two").capability_snapshot;
                    }
                }
                Ok(draft)
            })?;
            Ok(Effect::Done)
        })
    }
}

#[tokio::test]
async fn before_retargeting_requires_matching_capabilities_without_remerging_parameters() {
    for replace_snapshot in [false, true] {
        let mut original = configuration("one");
        original.config = Some(ModelConfig {
            max_tokens: Some(321.),
            temperature: Some(0.37),
            ..Default::default()
        });
        let mut builder = builder_with_initial(
            Arc::new(Prompts(Mutex::new(BTreeMap::new()))),
            ConfigureUpdate {
                sources: vec![],
                model: Some(original.clone()),
            },
        );
        builder.require_feature(
            Feature::new("RetargetModel").branch("RetargetModel", move |names| {
                Ok(Box::new(RetargetModel::new(names, replace_snapshot)?))
            }),
        );
        let agent = MinimalHost::start(&builder).await.unwrap();
        let session = agent
            .create_session(SessionConfig::default())
            .await
            .unwrap();
        session
            .submit("hello".into(), InputMode::Queue, "main")
            .await
            .unwrap();
        session.idle("main").await.unwrap();
        let events = chain_events(&session, "main", ..).await.unwrap();
        let request = events
            .iter()
            .find_map(|event| match &event.kind {
                EventKind::ModelCall(call) => Some(&call.request),
                _ => None,
            })
            .unwrap();
        assert_eq!(request.model_id, "two");
        assert_eq!(request.config, original.config);
        let outcomes: Vec<_> = events
            .iter()
            .filter_map(|event| match &event.kind {
                EventKind::ModelOutputEnd(end) => Some(&end.outcome),
                _ => None,
            })
            .collect();
        assert_eq!(
            outcomes.len(),
            1,
            "Invalid snapshots must not enter budget recovery"
        );
        if replace_snapshot {
            assert!(matches!(outcomes[0], ModelOutputOutcome::Completed { .. }));
            assert_eq!(
                request
                    .capability_snapshot
                    .as_ref()
                    .unwrap()
                    .capabilities
                    .context_window,
                98765
            );
            assert!(
                events
                    .iter()
                    .any(|event| matches!(event.kind, EventKind::Text(_)))
            );
        } else {
            assert!(
                matches!(outcomes[0], ModelOutputOutcome::Failed { error, budget: None, .. } if error.contains("snapshot target"))
            );
            assert!(
                !events
                    .iter()
                    .any(|event| matches!(event.kind, EventKind::Text(_)))
            );
        }
        agent.close().await.unwrap();
    }
}

#[tokio::test]
async fn frozen_configuration_rejects_mismatched_identity_and_nonbranch_system_entries() {
    let agent = MinimalHost::start(&builder(Arc::new(Prompts(Mutex::new(BTreeMap::new())))))
        .await
        .unwrap();
    let session = agent
        .create_session(SessionConfig::default())
        .await
        .unwrap();
    let before = session.sequence("main").unwrap();
    for (target, value) in [
        (
            PromptSourceTarget {
                source: "rules".into(),
                scope: PromptScope::Branch {
                    timeline: "main".into(),
                },
            },
            entry("other", "1", "invalid"),
        ),
        (
            PromptSourceTarget {
                source: "rules".into(),
                scope: PromptScope::Turn {
                    timeline: "main".into(),
                    id: "turn".into(),
                },
            },
            PromptEntry {
                scope: PromptScope::Turn {
                    timeline: "main".into(),
                    id: "turn".into(),
                },
                ..entry("rules", "1", "invalid")
            },
        ),
    ] {
        assert!(matches!(
            session
                .service::<dim_agent_capabilities::prompt::Prompt>("main")
                .unwrap()
                .configure_loaded(Some(configuration("invalid")), vec![(target, Some(value))])
                .await,
            Err(AgentError::InvalidInput(_))
        ));
        assert_eq!(session.sequence("main").unwrap(), before);
    }
    agent.close().await.unwrap();
}

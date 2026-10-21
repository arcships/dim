use dim_agent_capabilities::{
    adapters::{DefaultPrepareModelPlugin, EchoModel, MockTools},
    defaults,
    history::HistoryRenderer,
    host::*,
};
use dim_agent_runtime::*;
use dim_agent_types::*;
use rutis::{BoxFuture, CordisError, Ctx, Effect, TypedPlugin};
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
            capabilities: ModelCapabilities {
                context_window: if model == "two" { 98765 } else { 135791 },
                max_output_tokens: 6789,
                ..Default::default()
            },
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
        let agent = if initial {
            builder_with_initial(host, update.clone())
        } else {
            builder(host)
        }
        .build();
        let session = agent
            .create_session(SessionConfig {
                tools: vec!["echo".into()],
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
            let events = session.history("main", None).await.unwrap();
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
    let agent = builder(host.clone()).build();
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
        let events = session.history("main", None).await.unwrap();
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

struct TamperConfiguration(ConfigurationTamper, BranchScope);
impl TypedPlugin for TamperConfiguration {
    type Deps = (InBranch<Timeline>,);
    fn name(&self) -> &str {
        "TamperConfiguration"
    }
    fn apply<'a>(
        &'a self,
        ctx: &'a Ctx,
        (InBranch(timeline),): Self::Deps,
    ) -> BoxFuture<'a, std::result::Result<Effect, CordisError>> {
        Box::pin(async move {
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
impl Capability for TamperConfiguration {
    fn keys(&self) -> KeysOf<Self> {
        scoped::<Self::Deps, _>(&self.1)
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
        .session_value(Arc::new(ModelHost(Arc::new(EchoModel))))
        .session_value(Arc::new(ToolHost(Arc::new(MockTools))))
        .session_value(Arc::new(PrepareContextHost(prepare)))
        .session_value(Arc::new(ContextModelHost(None)))
        .session_value(Arc::new(CompactionStrategyHost(None)))
        .session_plugin(DefaultPrepareModelPlugin::new);
    defaults::install_with_prompt(&mut builder, host, None);
    builder
}

struct RejectSecondRequest(Arc<AtomicUsize>, BranchScope);
impl TypedPlugin for RejectSecondRequest {
    type Deps = (InBranch<Timeline>,);
    fn name(&self) -> &str {
        "RejectSecondPromptRequest"
    }
    fn apply<'a>(
        &'a self,
        ctx: &'a Ctx,
        (InBranch(timeline),): Self::Deps,
    ) -> BoxFuture<'a, std::result::Result<Effect, CordisError>> {
        Box::pin(async move {
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
impl Capability for RejectSecondRequest {
    fn keys(&self) -> KeysOf<Self> {
        scoped::<Self::Deps, _>(&self.1)
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
        builder.timeline_plugin(move |scope| TamperConfiguration(tamper, *scope));
        let agent = builder.build();
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
        let events = session.history("main", None).await.unwrap();
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
        assert!(session.history("main", None).await.unwrap().iter().any(|event| matches!(&event.kind,
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
        let agent = builder_with_context(host.clone(), prepare).build();
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
        let version_one = session
            .history("main", None)
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

        let child_events = session.history(&child, None).await.unwrap();
        let child_configured = child_events
            .iter()
            .find_map(|event| match &event.kind {
                EventKind::ContextConfigured(data) => Some(data),
                _ => None,
            })
            .unwrap();
        let first_call = child_events
            .iter()
            .find_map(|event| match &event.kind {
                EventKind::ModelCall(data) => Some(data),
                _ => None,
            })
            .unwrap();
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
                timeline: "main".into(),
                revision: version_one.revision.clone(),
            })
        );
        assert_eq!(first_call.prompt, version_one.prompt);

        let main_events = session.history("main", None).await.unwrap();
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
        let agent = builder(host.clone()).build();
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
        let events = session.history("main", None).await.unwrap();
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
            session
                .history("main", None)
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
        let inherited = session.history(&child, None).await.unwrap();
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
        let agent = builder(host.clone()).build();
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
    let agent = builder_with_initial(host, initial.clone()).build();
    let session = agent
        .create_session(SessionConfig::default())
        .await
        .unwrap();
    let events = session.history("main", None).await.unwrap();
    // The root starts, then Prompt delivers the initial configuration
    // before the Session takes input.
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
    let result = builder_with_initial(failing, initial)
        .build()
        .create_session(SessionConfig::default())
        .await;
    let error = match result {
        Ok(_) => panic!("initial load failure unexpectedly created a session"),
        Err(error) => error,
    };
    // The opening receipt fails after the root's `branch/start` dispatches: the
    // Session is not created, and the original cause is reported first.
    let cause = match &error {
        AgentError::Multiple(errors) => &errors[0],
        cause => cause,
    };
    assert!(
        matches!(cause, AgentError::InvalidInput(message) if message == "prompt source rules failed: load failed")
    );
}

struct OpeningObserved(BranchScope, Arc<Notify>, CancellationToken);
impl TypedPlugin for OpeningObserved {
    type Deps = InBranch<Dispatcher>;
    fn name(&self) -> &str {
        "OpeningObserved"
    }
    fn apply<'a>(
        &'a self,
        ctx: &'a Ctx,
        InBranch(dispatcher): Self::Deps,
    ) -> BoxFuture<'a, std::result::Result<Effect, CordisError>> {
        Box::pin(async move {
            let observed = self.1.clone();
            dispatcher.subscribe(
                ctx,
                "OpeningObserved",
                &["branch/start", "branch/resume"],
                move |_| {
                    observed.notify_one();
                    Box::pin(async { Ok(()) })
                },
            )?;
            // A close must signal the host independently of dispatch drain
            // and capability unload, even if another handler is waiting.
            let cancelled = self.2.clone();
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
impl Capability for OpeningObserved {
    fn keys(&self) -> KeysOf<Self> {
        scoped::<Self::Deps, _>(&self.0)
    }
}

struct OpeningFailure(BranchScope);
impl TypedPlugin for OpeningFailure {
    type Deps = (InBranch<Dispatcher>, InBranch<BranchOpening>);
    fn name(&self) -> &str {
        "OpeningFailure"
    }
    fn apply<'a>(
        &'a self,
        ctx: &'a Ctx,
        (InBranch(dispatcher), InBranch(opening)): Self::Deps,
    ) -> BoxFuture<'a, std::result::Result<Effect, CordisError>> {
        Box::pin(async move {
            dispatcher.subscribe(ctx, "OpeningFailure", &["branch/start"], move |_| {
                let opening = opening.clone();
                Box::pin(async move {
                    // A later failure must not wait behind an earlier receipt.
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
impl Capability for OpeningFailure {
    fn keys(&self) -> KeysOf<Self> {
        scoped::<Self::Deps, _>(&self.0)
    }
}

#[tokio::test]
async fn opening_failure_is_not_hidden_by_an_earlier_pending_receipt() {
    let mut builder = builder(Arc::new(Prompts(Mutex::new(BTreeMap::new()))));
    builder.timeline_plugin(|scope| OpeningFailure(*scope));
    let agent = builder.build();
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
    // A restored Session that never started starts on restore; one that did
    // resumes, its main branch opening from stored history.
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
                builder.timeline_plugin(move |scope| {
                    OpeningObserved(*scope, signal.clone(), cancel_signal.clone())
                });
                let agent = builder.build();
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
                // Only a branch opening from stored history shares it.
                assert_eq!(
                    agent.live_openings(),
                    usize::from(start == "resume"),
                    "{start}"
                );
                let dispatched =
                    tokio::time::timeout(Duration::from_millis(100), observed.notified()).await;
                // Release even on the old implementation so a red run is bounded.
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
                // The cancelled build released its branch opening.
                assert_eq!(agent.live_openings(), 0, "{start}");
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
        let agent = builder.build();
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
        // The child opens with its ancestry, `main` up to the cut.
        assert_eq!(agent.live_openings(), 1);
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
        // The cancelled build released the child's opening.
        assert_eq!(agent.live_openings(), 0);
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
    let agent = builder.build();
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
    // `branch/resume` compares the sources; a system source stays as adopted.
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let events = session.history("main", None).await.unwrap();
    let configured: Vec<_> = events
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::ContextConfigured(data) => Some(data),
            _ => None,
        })
        .collect();
    // The stored source version is gone, so ToolBinding re-adopts the
    // current tools once; the model and the system prompt stay adopted.
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
            Some(SystemPrompt::Text("adopted system".into()))
        );
    }
    // Model updates below keep this re-adopted tool part.
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
    let call = session
        .history("main", None)
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
        let events = session.history("main", None).await.unwrap();
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
        assert_eq!(request.config, selection.config);
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
    let agent = builder.build();
    let session = agent
        .restore_session("empty-prompt-restore".into())
        .await
        .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(matches!(
        session.history("main", None).await.unwrap()[0].kind,
        EventKind::BranchStart(BranchStartData { parent: None, .. })
    ));
    agent.close().await.unwrap();
}

#[tokio::test]
async fn context_only_initial_prompt_is_inherited_and_unchanged_in_a_child() {
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
    let agent = builder_with_initial(
        host,
        ConfigureUpdate {
            sources: vec![target],
            model: None,
        },
    )
    .build();
    let session = agent
        .create_session(SessionConfig::default())
        .await
        .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(
        !session
            .history("main", None)
            .await
            .unwrap()
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
    // The child compares the sources at `branch/start`; nothing changed.
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert!(
        !session
            .local(&child, ..)
            .await
            .unwrap()
            .iter()
            .any(|event| matches!(event.kind, EventKind::PromptChanged(_)))
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
    let agent = builder(host.clone()).build();
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
    let configured = session
        .history("main", None)
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
    let agent = builder(host).build();
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
    builder.timeline_plugin(|scope| RejectSecondRequest(Arc::new(AtomicUsize::new(0)), *scope));
    let agent = builder.build();
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
    let agent = builder(host).build();
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
    scope: BranchScope,
    replace_snapshot: bool,
}
impl TypedPlugin for RetargetModel {
    type Deps = (InBranch<Timeline>,);
    fn name(&self) -> &str {
        "RetargetModel"
    }
    fn apply<'a>(
        &'a self,
        ctx: &'a Ctx,
        (InBranch(timeline),): Self::Deps,
    ) -> BoxFuture<'a, std::result::Result<Effect, CordisError>> {
        Box::pin(async move {
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
impl Capability for RetargetModel {
    fn keys(&self) -> KeysOf<Self> {
        scoped::<Self::Deps, _>(&self.scope)
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
        builder.timeline_plugin(move |scope| RetargetModel {
            scope: *scope,
            replace_snapshot,
        });
        let agent = builder.build();
        let session = agent
            .create_session(SessionConfig::default())
            .await
            .unwrap();
        session
            .submit("hello".into(), InputMode::Queue, "main")
            .await
            .unwrap();
        session.idle("main").await.unwrap();
        let events = session.history("main", None).await.unwrap();
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
    let agent = builder(Arc::new(Prompts(Mutex::new(BTreeMap::new())))).build();
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

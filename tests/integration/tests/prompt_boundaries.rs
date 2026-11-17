use dim_agent_capabilities::{defaults, host::*, prompt_state::PromptSnapshot};
use dim_agent_runtime::testing::chain_events;
use dim_agent_runtime::{testing::MinimalHost, *};
use dim_agent_types::*;
use rutis::Ctx;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, Key, TypeKey};
use std::sync::Arc;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

struct GatedPrompts {
    started: Arc<Notify>,
    release: Arc<Notify>,
}

impl LoadPrompt for GatedPrompts {
    fn load(
        &self,
        target: PromptSourceTarget,
        _: CancellationToken,
    ) -> HostFuture<'_, Option<PromptEntry>> {
        Box::pin(async move {
            if target.source == "blocked" {
                self.started.notify_one();
                self.release.notified().await;
            }
            Ok(Some(prompt(
                &target.source,
                "1",
                &target.source,
                target.scope,
            )))
        })
    }
}

fn scope(timeline: &str) -> PromptScope {
    PromptScope::Branch {
        timeline: timeline.into(),
    }
}

fn prompt(source: &str, version: &str, body: &str, scope: PromptScope) -> PromptEntry {
    PromptEntry {
        source: source.into(),
        version: version.into(),
        body: body.into(),
        scope,
        delivery: PromptDelivery::Context,
        order: 0,
    }
}

fn assert_healthy(session: &Session) {
    let state = session.state("main").unwrap();
    assert!(!state.closed, "branch unexpectedly closed: {state:?}");
    assert!(
        state.failure.is_none(),
        "branch unexpectedly failed: {state:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rejected_prompt_load_does_not_publish_or_poison_the_pending_load() {
    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let mut builder = AgentBuilder::new(AgentConfig::default());
    defaults::mock_hosts(&mut builder);
    defaults::install_with_prompt(
        &mut builder,
        Arc::new(GatedPrompts {
            started: started.clone(),
            release: release.clone(),
        }),
        None,
    );
    builder.require_feature(
        Feature::new("TamperControl").branch("TamperControl", |names| {
            Ok(Box::new(TamperControl::new(
                names,
                "prompt/configure-request",
            )?))
        }),
    );
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
                            source: "blocked".into(),
                            scope: scope("main"),
                        }],
                        model: None,
                    },
                    "main",
                )
                .await
        }
    });
    started.notified().await;
    let requests_before = chain_events(&session, "main", ..)
        .await
        .unwrap()
        .iter()
        .filter(|event| matches!(event.kind, EventKind::PromptConfigureRequest(_)))
        .count();
    let second = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        session.configure(
            ConfigureUpdate {
                sources: vec![PromptSourceTarget {
                    source: "other".into(),
                    scope: scope("main"),
                }],
                model: None,
            },
            "main",
        ),
    )
    .await
    .expect("registration rejection must be immediate");
    let requests_after = chain_events(&session, "main", ..)
        .await
        .unwrap()
        .iter()
        .filter(|event| matches!(event.kind, EventKind::PromptConfigureRequest(_)))
        .count();
    release.notify_one();

    assert!(matches!(second, Err(AgentError::InvalidInput(_))));
    assert_eq!(requests_after, requests_before);
    assert_eq!(
        tokio::time::timeout(std::time::Duration::from_secs(1), first)
            .await
            .expect("accepted prompt load did not settle")
            .unwrap()
            .unwrap(),
        ConfigureOutcome::Adopted
    );
    assert_healthy(&session);
    session
        .submit("still usable".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    session.idle("main").await.unwrap();
    assert_healthy(&session);
    agent.close().await.unwrap();
}

struct TamperControl(&'static str, Key<Timeline>);

impl TamperControl {
    fn new(names: &Names<'_>, key: &'static str) -> rutis::Result<Self> {
        Ok(Self(key, names.key()?))
    }
}

impl DynPlugin for TamperControl {
    fn name(&self) -> &str {
        "TamperControl"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.1.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let timeline = ctx.require_at(&self.1)?;
            timeline.before(ctx, "TamperControl", &[self.0], |mut draft| {
                match &mut draft.kind {
                    EventKind::PromptConfigureRequest(data)
                    | EventKind::PromptConfigureResult(data) => {
                        data.operation_id = "unknown".into()
                    }
                    _ => unreachable!(),
                }
                Ok(draft)
            })?;
            Ok(Effect::Done)
        })
    }
}

#[tokio::test]
async fn rewritten_control_identity_is_registered_without_leaking_activity() {
    for key in ["prompt/configure-request", "prompt/configure-result"] {
        let mut builder = AgentBuilder::new(AgentConfig::default());
        defaults::mock_hosts(&mut builder);
        defaults::install_with_prompt(
            &mut builder,
            Arc::new(GatedPrompts {
                started: Arc::new(Notify::new()),
                release: Arc::new(Notify::new()),
            }),
            None,
        );
        builder.require_feature(
            Feature::new("TamperControl").branch("TamperControl", move |names| {
                Ok(Box::new(TamperControl::new(names, key)?))
            }),
        );
        let agent = MinimalHost::start(&builder).await.unwrap();
        let session = agent
            .create_session(SessionConfig::default())
            .await
            .unwrap();
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            session.configure(
                ConfigureUpdate {
                    sources: vec![PromptSourceTarget {
                        source: "rules".into(),
                        scope: scope("main"),
                    }],
                    model: Some(ModelSelection {
                        provider_id: "mock".into(),
                        model_id: "rewritten-control".into(),
                        capability_snapshot: None,
                        config: None,
                        metadata: None,
                    }),
                },
                "main",
            ),
        )
        .await
        .expect("rewritten control must settle the caller");
        assert_eq!(result.unwrap(), ConfigureOutcome::Adopted);
        assert!(
            chain_events(&session, "main", ..)
                .await
                .unwrap()
                .iter()
                .any(|event| event.kind.key() == key
                    && matches!(&event.kind,
                    EventKind::PromptConfigureRequest(data) | EventKind::PromptConfigureResult(data)
                        if data.operation_id == "unknown"))
        );
        assert!(
            session
                .get::<PromptSnapshot>("main")
                .unwrap()
                .unwrap()
                .entries
                .contains_key(&("rules".into(), scope("main")))
        );
        session
            .submit("still usable".into(), InputMode::Queue, "main")
            .await
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(1), session.idle("main"))
            .await
            .unwrap()
            .unwrap();
        assert_healthy(&session);
        tokio::time::timeout(std::time::Duration::from_secs(1), agent.close())
            .await
            .unwrap()
            .unwrap();
    }
}

struct EndScopeBeforeAdoption(PromptScope, Key<Timeline>, Key<Dispatcher>);

impl EndScopeBeforeAdoption {
    fn new(names: &Names<'_>, scope: PromptScope) -> rutis::Result<Self> {
        Ok(Self(scope, names.key()?, names.key()?))
    }
}

impl DynPlugin for EndScopeBeforeAdoption {
    fn name(&self) -> &str {
        "EndScopeBeforeAdoption"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.1.erase(), self.2.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let timeline = ctx.require_at(&self.1)?;
            let dispatcher = ctx.require_at(&self.2)?;
            timeline.append(
                EventKind::ContextScope(ContextScopeData {
                    scope: self.0.clone(),
                    status: ContextScopeStatus::Started,
                })
                .into(),
            )?;
            let scope = self.0.clone();
            dispatcher.subscribe(
                ctx,
                "EndScopeBeforeAdoption",
                &["prompt/configure-result"],
                move |_| {
                    let timeline = timeline.clone();
                    let scope = scope.clone();
                    Box::pin(async move {
                        timeline.append(
                            EventKind::ContextScope(ContextScopeData {
                                scope,
                                status: ContextScopeStatus::Ended,
                            })
                            .into(),
                        )?;
                        Ok(())
                    })
                },
            )?;
            Ok(Effect::Done)
        })
    }
}

#[tokio::test]
async fn scope_ended_in_committed_tail_supersedes_loading_result() {
    let scope = PromptScope::Task {
        timeline: "main".into(),
        id: "task".into(),
    };
    let mut builder = AgentBuilder::new(AgentConfig::default());
    defaults::mock_hosts(&mut builder);
    let task_scope = scope.clone();
    builder.require_feature(Feature::new("EndScopeBeforeAdoption").branch(
        "EndScopeBeforeAdoption",
        move |names| {
            Ok(Box::new(EndScopeBeforeAdoption::new(
                names,
                task_scope.clone(),
            )?))
        },
    ));
    defaults::install_with_prompt(
        &mut builder,
        Arc::new(GatedPrompts {
            started: Arc::new(Notify::new()),
            release: Arc::new(Notify::new()),
        }),
        None,
    );
    let agent = MinimalHost::start(&builder).await.unwrap();
    let session = agent
        .create_session(SessionConfig::default())
        .await
        .unwrap();
    let result = session
        .configure(
            ConfigureUpdate {
                sources: vec![PromptSourceTarget {
                    source: "task rules".into(),
                    scope,
                }],
                model: None,
            },
            "main",
        )
        .await
        .unwrap();
    assert_eq!(result, ConfigureOutcome::Superseded);
    session.idle("main").await.unwrap();
    assert!(
        !chain_events(&session, "main", ..)
            .await
            .unwrap()
            .iter()
            .any(|event| matches!(event.kind, EventKind::PromptChanged(_)))
    );
    assert_healthy(&session);
    agent.close().await.unwrap();
}

struct TamperRetirement {
    timeline: Key<Timeline>,
    active: Arc<std::sync::atomic::AtomicBool>,
    captured: Arc<std::sync::Mutex<Option<Arc<Timeline>>>>,
    change: &'static str,
}

impl DynPlugin for TamperRetirement {
    fn name(&self) -> &str {
        "TamperRetirement"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.timeline.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let timeline = ctx.require_at(&self.timeline)?;
            *self.captured.lock().unwrap() = Some(timeline.clone());
            let active = self.active.clone();
            let change = self.change;
            timeline.before(
                ctx,
                "TamperRetirement",
                &["input/message", "prompt-state/recovered"],
                move |mut draft| {
                    if active.load(std::sync::atomic::Ordering::SeqCst) {
                        match &mut draft.kind {
                            EventKind::InputMessage(data) => {
                                for InputBlock::Raw { key, retire, .. } in &mut data.blocks {
                                    if *retire {
                                        match change {
                                            "key" => *key = Some("wrong-key".into()),
                                            "retire" => *retire = false,
                                            _ => (),
                                        }
                                    }
                                }
                            }
                            EventKind::Extension(data) if change == "result" => {
                                let mut value = data.data().clone();
                                value["schema"] = "wrong-schema".into();
                                *data = ExtensionEvent::new("prompt-state", "recovered", value)
                                    .unwrap();
                            }
                            _ => (),
                        }
                    }
                    Ok(draft)
                },
            )?;
            Ok(Effect::Done)
        })
    }
}

#[tokio::test]
async fn scope_retirement_rejects_inconsistent_before_batches_and_retries_after_reload() {
    for change in ["key", "retire", "result"] {
        let store = Arc::new(MemoryTimelineStore::default());
        let active = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let captured = Arc::new(std::sync::Mutex::new(None));
        let mut builder = AgentBuilder::new(AgentConfig::default());
        builder.timeline_store(store.clone());
        defaults::mock_hosts(&mut builder);
        defaults::install_with_prompt(
            &mut builder,
            Arc::new(GatedPrompts {
                started: Arc::new(Notify::new()),
                release: Arc::new(Notify::new()),
            }),
            None,
        );
        let control = active.clone();
        let probe = captured.clone();
        builder.require_feature(Feature::new("TamperRetirement").branch(
            "TamperRetirement",
            move |names| {
                Ok(Box::new(TamperRetirement {
                    timeline: names.key()?,
                    active: control.clone(),
                    captured: probe.clone(),
                    change,
                }))
            },
        ));
        let agent = MinimalHost::start(&builder).await.unwrap();
        let session = agent
            .create_session(SessionConfig::default())
            .await
            .unwrap();
        let id = session.id().to_owned();
        let task = PromptScope::Task {
            timeline: "main".into(),
            id: "task".into(),
        };
        let timeline = captured.lock().unwrap().clone().unwrap();
        timeline
            .append(
                EventKind::ContextScope(ContextScopeData {
                    scope: task.clone(),
                    status: ContextScopeStatus::Started,
                })
                .into(),
            )
            .unwrap();
        session
            .configure(
                ConfigureUpdate {
                    sources: vec![PromptSourceTarget {
                        source: "task-rules".into(),
                        scope: task.clone(),
                    }],
                    model: None,
                },
                "main",
            )
            .await
            .unwrap();
        session.idle("main").await.unwrap();
        active.store(true, std::sync::atomic::Ordering::SeqCst);
        let ended = timeline
            .append(
                EventKind::ContextScope(ContextScopeData {
                    scope: task.clone(),
                    status: ContextScopeStatus::Ended,
                })
                .into(),
            )
            .unwrap();
        let failure = tokio::time::timeout(std::time::Duration::from_secs(2), session.idle("main"))
            .await
            .unwrap();
        assert!(
            failure.is_err(),
            "the inconsistent {change} batch must fail"
        );
        let key = TimelineKey {
            session_id: id.clone(),
            branch_id: "main".into(),
        };
        let stored = store
            .read(&key, ended.sequence..=timeline.committed())
            .await
            .unwrap();
        assert!(
            stored
                .iter()
                .all(|record| record.event.kind.key() != "prompt-state/recovered")
        );
        assert!(stored.iter().all(|record| !matches!(&record.event.kind, EventKind::InputMessage(data) if data.blocks.iter().any(|InputBlock::Raw {retire,..}| *retire))));
        let _ = agent.close().await;
        active.store(false, std::sync::atomic::Ordering::SeqCst);
        let agent = MinimalHost::start(&builder).await.unwrap();
        let restored = agent.restore_session(id).await.unwrap();
        restored.idle("main").await.unwrap();
        assert!(
            !restored
                .get::<PromptSnapshot>("main")
                .unwrap()
                .unwrap()
                .entries
                .contains_key(&("task-rules".into(), task))
        );
        let events = chain_events(&restored, "main", ended.sequence + 1..)
            .await
            .unwrap();
        assert!(events.iter().any(|event| matches!(&event.kind, EventKind::InputMessage(data) if data.blocks.iter().any(|InputBlock::Raw {retire,..}| *retire))), "reload must settle the original retirement");
        agent.close().await.unwrap();
    }
}

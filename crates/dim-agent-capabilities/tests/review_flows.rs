//! Independent review through the public Agent and host extension contracts.
use dim_agent_capabilities::{
    adapters::DefaultPrepareModelPlugin, defaults, host::*, tool_registry::definition,
};
use dim_agent_runtime::*;
use dim_agent_types::*;
use rutis::host::CtxHost;
use rutis::{Ctx, Plugin};
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

async fn soon<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(3), future)
        .await
        .expect("public lifecycle stalled")
}
fn builder(model: Arc<dyn ModelDriver>, tools: Arc<dyn ToolDriver>) -> AgentBuilder {
    let mut builder = AgentBuilder::new(AgentConfig::default());
    builder.timeline_store(Arc::new(dim_agent_runtime::MemoryTimelineStore::default()));
    builder
        .process_value(Arc::new(ModelHost(model)))
        .session_value(Arc::new(ToolHost(tools)))
        .session_value(Arc::new(
            dim_agent_capabilities::history::HistoryRenderer::default(),
        ))
        .session_value(Arc::new(PrepareContextHost(Arc::new(IdentityContext))))
        .session_value(Arc::new(ContextModelHost(None)))
        .session_value(Arc::new(CompactionStrategyHost(None)))
        .require_feature(
            Feature::new("PrepareModelHost").session("PrepareModelHost", |names| {
                Ok(Box::new(DefaultPrepareModelPlugin::new(names)?))
            }),
        );
    defaults::install(&mut builder);
    builder
}
struct TwoToolsModel;
impl ModelDriver for TwoToolsModel {
    fn invoke(
        &self,
        _: Arc<ModelRequest>,
        _: dim_agent_capabilities::host::ModelConnection,
        _: CancellationToken,
    ) -> HostFuture<'_, ModelStream> {
        Box::pin(async {
            Ok(Box::pin(futures_util::stream::iter(vec![
                Ok(ModelChunk::MessageStart {
                    message_id: "batch".into(),
                }),
                Ok(ModelChunk::ToolStart {
                    id: "first".into(),
                    name: "panic".into(),
                }),
                Ok(ModelChunk::ToolStart {
                    id: "second".into(),
                    name: "slow".into(),
                }),
                Ok(ModelChunk::ToolUse(ToolCall {
                    id: "first".into(),
                    name: "panic".into(),
                    input: JsonObject::new(),
                })),
                Ok(ModelChunk::ToolUse(ToolCall {
                    id: "second".into(),
                    name: "slow".into(),
                    input: JsonObject::new(),
                })),
                Ok(ModelChunk::MessageStop {
                    message_id: "batch".into(),
                    reason: ModelStopReason::ToolUse,
                }),
            ])) as ModelStream)
        })
    }
}
struct PanicAndSlowTools {
    entered: Arc<Notify>,
    release: Arc<Notify>,
    finished: Arc<AtomicUsize>,
}
impl ToolDriver for PanicAndSlowTools {
    fn definitions(&self) -> Vec<ToolDefinition> {
        ["panic", "slow"]
            .map(|name| definition(name, name, serde_json::json!({"type":"object"})))
            .to_vec()
    }
    fn execute(
        &self,
        call: ToolCall,
        _: ToolExecutionContext,
        _: CancellationToken,
    ) -> HostFuture<'_, ToolResult> {
        if call.name == "panic" {
            panic!("native tool constructor failed");
        }
        Box::pin(async move {
            self.entered.notify_one();
            self.release.notified().await;
            self.finished.fetch_add(1, Ordering::SeqCst);
            Ok(ToolResult::text("native side effect completed"))
        })
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn one_tool_panic_does_not_abandon_other_native_cleanup_or_reorder_actual_results() {
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let finished = Arc::new(AtomicUsize::new(0));
    let agent = builder(
        Arc::new(TwoToolsModel),
        Arc::new(PanicAndSlowTools {
            entered: entered.clone(),
            release: release.clone(),
            finished: finished.clone(),
        }),
    )
    .build();
    let session = soon(agent.create_session(SessionConfig {
        tools: Some(vec!["panic".into(), "slow".into()]),
        permission_policy: PermissionPolicy {
            preset: PermissionPreset::FullAccess,
            ..Default::default()
        },
        ..SessionConfig::default()
    }))
    .await
    .unwrap();
    session
        .submit("run tools".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    soon(entered.notified()).await;
    assert!(!session.state("main").unwrap().closed);
    let waiter = session.clone();
    let mut idle = tokio::spawn(async move { waiter.idle("main").await });
    assert!(
        tokio::time::timeout(Duration::from_millis(10), &mut idle)
            .await
            .is_err()
    );
    assert_eq!(finished.load(Ordering::SeqCst), 0);
    release.notify_one();
    let _ = soon(idle).await.unwrap();
    let _ = soon(session.close()).await;
    assert_eq!(finished.load(Ordering::SeqCst), 1);
    let events = session.history("main", None).await.unwrap();
    let results = events
        .iter()
        .find_map(|event| match &event.kind {
            EventKind::ToolResult(data) => Some(&data.results),
            _ => None,
        })
        .expect("the complete batch result is committed before framework close");
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].call_id, "first");
    assert_eq!(results[1].call_id, "second");
    assert_eq!(results[0].result.is_error, Some(true));
    assert_eq!(
        results[1].result.content,
        ToolContent::Text("native side effect completed".into())
    );
    let _ = agent.close().await;
}

#[tokio::test]
async fn append_only_input_remains_pending_while_idle_returns_and_a_later_queue_consumes_it() {
    let mut builder = AgentBuilder::new(AgentConfig::default());
    defaults::mock_hosts(&mut builder);
    defaults::install(&mut builder);
    let agent = builder.build();
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    let appended = session
        .submit("pending context".into(), InputMode::Append, "main")
        .await
        .unwrap();
    soon(session.idle("main")).await.unwrap();
    let before = session.history("main", None).await.unwrap();
    assert!(!before.iter().any(|event| matches!(
        event.kind,
        EventKind::TurnStart(_) | EventKind::InputConsumed(_)
    )));
    assert_eq!(
        session
            .get::<InputSnapshot>("main")
            .unwrap()
            .unwrap()
            .queued,
        1
    );
    session
        .submit("start".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    soon(session.idle("main")).await.unwrap();
    assert!(session.history("main", None).await.unwrap().iter().any(
        |event| matches!(&event.kind,EventKind::InputConsumed(data) if data.input_id==appended.id)
    ));
    soon(agent.close()).await.unwrap();
}

struct EmptyLoop(tokio::sync::watch::Sender<LoopSnapshot>);
impl LoopState for EmptyLoop {
    fn state(&self) -> LoopSnapshot {
        LoopSnapshot::default()
    }
    fn watch(&self) -> tokio::sync::watch::Receiver<LoopSnapshot> {
        self.0.subscribe()
    }
}
struct ExtensionControls(Arc<AtomicUsize>, rutis::host::Key<dyn LoopState>);
impl Plugin for ExtensionControls {
    type Deps = ();
    fn name(&self) -> &str {
        "review.extension-controls"
    }
    async fn apply(&self, ctx: &Ctx, (): Self::Deps) -> rutis::Result {
        self.0.fetch_add(1, Ordering::SeqCst);
        let live = self.0.clone();
        ctx.effect(|| async move {
            live.fetch_sub(1, Ordering::SeqCst);
            Ok(())
        })?;
        ctx.provide_at(
            self.1.clone(),
            Arc::new(EmptyLoop(
                tokio::sync::watch::channel(LoopSnapshot::default()).0,
            )) as Arc<dyn LoopState>,
        )?;
        Ok(())
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_branch_failing_after_its_root_is_built_rolls_it_back_before_releasing_its_registry_slot()
{
    let live = Arc::new(AtomicUsize::new(0));
    let resources = live.clone();
    let mut builder = AgentBuilder::new(AgentConfig::default());
    dim_agent_capabilities::register_names(&mut builder);
    builder
        .require_feature(
            Feature::new("TimelineTree")
                .session("TimelineTree", |names| {
                    Ok(Box::new(
                        dim_agent_capabilities::timeline_tree::TimelineTreePlugin::new(names)?,
                    ))
                })
                .branch("TreeTimeline", |names| {
                    Ok(Box::new(
                        dim_agent_capabilities::timeline_tree::TreeTimelinePlugin::new(names)?,
                    ))
                }),
        )
        .require_feature(Feature::new("Prompt").session("PromptGuide", |names| {
            Ok(Box::new(
                dim_agent_capabilities::prompt_guide::PromptGuidePlugin::new(names)?,
            ))
        }))
        .require_feature(
            Feature::new("review.extension-controls")
                .branch("review.extension-controls", move |names| {
                    Ok(Box::new(ExtensionControls(resources.clone(), names.key()?)))
                }),
        );
    let agent = builder.build();
    let session = soon(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    assert_eq!(live.load(Ordering::SeqCst), 1);
    for _ in 0..2 {
        let result = soon(session.branch(
            "main".into(),
            0,
            BranchOptions {
                prompt: String::new(),
                source: BranchSource::Host {
                    operation_id: "review".into(),
                },
            },
        ))
        .await;
        assert!(
            matches!(result, Err(AgentError::InvalidInput(_))),
            "{result:?}"
        );
        assert_eq!(session.branches().len(), 1);
        assert_eq!(
            live.load(Ordering::SeqCst),
            1,
            "failed branch cleanup must finish before returning its error"
        );
    }
    let _ = agent.close().await;
    assert_eq!(live.load(Ordering::SeqCst), 0);
}

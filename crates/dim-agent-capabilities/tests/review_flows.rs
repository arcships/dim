//! Independent review through the public Agent and host extension contracts.
use dim_agent_capabilities::{
    adapters::DefaultPrepareModelPlugin, defaults, host::*, tool_registry::definition,
};
use dim_agent_runtime::testing::MinimalHost;
use dim_agent_runtime::testing::chain_events;
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
    let agent = MinimalHost::start(&builder(
        Arc::new(TwoToolsModel),
        Arc::new(PanicAndSlowTools {
            entered: entered.clone(),
            release: release.clone(),
            finished: finished.clone(),
        }),
    ))
    .await
    .unwrap();
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
    let events = chain_events(&session, "main", ..).await.unwrap();
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
    let agent = MinimalHost::start(&builder).await.unwrap();
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

/// One batch of calls, each name answered by the same fixture driver. The
/// read sets `read_saw_it` when it enters before the exclusive call before it
/// returned.
#[derive(Default)]
struct Segmented {
    /// Calls in the order they started.
    started: std::sync::Mutex<Vec<String>>,
    /// Set by each call after it yields a few times, by its own name.
    returned: Arc<std::sync::Mutex<std::collections::HashSet<String>>>,
    /// The call each name must not enter before, from `preceded_by`.
    preceded_by: Vec<(&'static str, &'static str)>,
    /// Names that entered before the call they follow returned.
    early: Arc<std::sync::Mutex<Vec<String>>>,
}
impl Segmented {
    fn started(&self) -> Vec<String> {
        self.started.lock().unwrap().clone()
    }
    fn early(&self) -> Vec<String> {
        self.early.lock().unwrap().clone()
    }
    fn returned(&self, name: &str) -> bool {
        self.returned.lock().unwrap().contains(name)
    }
}
impl ToolDriver for Segmented {
    fn definitions(&self) -> Vec<ToolDefinition> {
        ["x0", "x1", "c0", "c1", "c2", "ask0", "ask1"]
            .map(|name| definition(name, name, serde_json::json!({"type":"object"})))
            .to_vec()
    }
    fn concurrency(
        &self,
        call: &ToolCall,
    ) -> dim_agent_capabilities::tool_registry::ToolConcurrency {
        if call.name.starts_with('c') || call.name.starts_with("ask") {
            dim_agent_capabilities::tool_registry::ToolConcurrency::Concurrent
        } else {
            dim_agent_capabilities::tool_registry::ToolConcurrency::Exclusive
        }
    }
    fn execute(
        &self,
        call: ToolCall,
        _: ToolExecutionContext,
        _: CancellationToken,
    ) -> HostFuture<'_, ToolResult> {
        self.started.lock().unwrap().push(call.name.clone());
        // A call that must follow another asserts that it did, on entry.
        if let Some((_, before)) = self.preceded_by.iter().find(|(name, _)| *name == call.name)
            && !self.returned(before)
        {
            self.early.lock().unwrap().push(call.name.clone());
        }
        let name = call.name.clone();
        let returned = self.returned.clone();
        Box::pin(async move {
            // Yield several times before releasing the step, so a call that
            // follows under a plain `join_all` would enter first.
            for _ in 0..8 {
                tokio::task::yield_now().await;
            }
            returned.lock().unwrap().insert(name.clone());
            Ok(ToolResult::text(format!("{name} done")))
        })
    }
}

/// One request carrying `names` as one batch, then text.
struct BatchModel(&'static [&'static str]);
impl ModelDriver for BatchModel {
    fn invoke(
        &self,
        request: Arc<ModelRequest>,
        _: dim_agent_capabilities::host::ModelConnection,
        _: CancellationToken,
    ) -> HostFuture<'_, ModelStream> {
        let batch = dim_agent_capabilities::adapters::EchoModel::last_message(&request)
            .is_some_and(|message| message.role == Role::User);
        let names = if batch { self.0.to_vec() } else { Vec::new() };
        Box::pin(async move {
            let mut chunks = vec![Ok(ModelChunk::MessageStart {
                message_id: "batch".into(),
            })];
            if names.is_empty() {
                chunks.push(Ok(ModelChunk::Text("done".into())));
                chunks.push(Ok(ModelChunk::TextEnd));
                chunks.push(Ok(ModelChunk::MessageStop {
                    message_id: "batch".into(),
                    reason: ModelStopReason::EndTurn,
                }));
                return Ok(Box::pin(futures_util::stream::iter(chunks)) as ModelStream);
            }
            for name in names {
                let id = format!("call-{name}");
                chunks.push(Ok(ModelChunk::ToolStart {
                    id: id.clone(),
                    name: name.into(),
                }));
                chunks.push(Ok(ModelChunk::ToolUse(ToolCall {
                    id,
                    name: name.into(),
                    input: JsonObject::new(),
                })));
            }
            chunks.push(Ok(ModelChunk::MessageStop {
                message_id: "batch".into(),
                reason: ModelStopReason::ToolUse,
            }));
            Ok(Box::pin(futures_util::stream::iter(chunks)) as ModelStream)
        })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_read_of_a_batch_does_not_start_before_the_exclusive_call_before_it_returns() {
    // `[x0, c1]`: c1 must not start before x0 returns. x0 yields before
    // releasing its step, so a plain `join_all` would let c1 enter first.
    let tools = Arc::new(Segmented {
        preceded_by: vec![("c1", "x0")],
        ..Default::default()
    });
    let agent = MinimalHost::start(&builder(Arc::new(BatchModel(&["x0", "c1"])), tools.clone()))
        .await
        .unwrap();
    let session = soon(agent.create_session(SessionConfig {
        tools: Some(vec!["x0".into(), "c1".into()]),
        permission_policy: PermissionPolicy {
            preset: PermissionPreset::FullAccess,
            ..Default::default()
        },
        ..SessionConfig::default()
    }))
    .await
    .unwrap();
    session
        .submit("go".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    soon(session.idle("main")).await.unwrap();
    assert_eq!(tools.started(), ["x0", "c1"], "call order is kept");
    assert!(
        tools.early().is_empty(),
        "these calls entered before the call they follow returned: {:?}",
        tools.early()
    );
    let _ = agent.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_read_starts_after_the_step_before_it_and_before_the_step_after() {
    // `[c0, x1, c2]`: c0 is a read of its own step, x1 orders, and c2 is a
    // read of the step after it. Each call asserts on entry that the call it
    // follows returned, and each yields before releasing, so a plain
    // `join_all` lets both later calls enter first.
    let tools = Arc::new(Segmented {
        preceded_by: vec![("x1", "c0"), ("c2", "x1")],
        ..Default::default()
    });
    let agent = MinimalHost::start(&builder(
        Arc::new(BatchModel(&["c0", "x1", "c2"])),
        tools.clone(),
    ))
    .await
    .unwrap();
    let session = soon(agent.create_session(SessionConfig {
        tools: Some(vec!["c0".into(), "x1".into(), "c2".into()]),
        permission_policy: PermissionPolicy {
            preset: PermissionPreset::FullAccess,
            ..Default::default()
        },
        ..SessionConfig::default()
    }))
    .await
    .unwrap();
    session
        .submit("go".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    soon(session.idle("main")).await.unwrap();
    assert_eq!(tools.started(), ["c0", "x1", "c2"], "call order is kept");
    assert!(
        tools.early().is_empty(),
        "these calls entered before the call they follow returned: {:?}",
        tools.early()
    );
    let _ = agent.close().await;
}

/// Two reads of one batch, each of which needs an approval question.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn both_calls_of_one_batch_ask_before_either_is_answered() {
    let tools = Arc::new(Segmented::default());
    let agent = MinimalHost::start(&builder(
        Arc::new(BatchModel(&["ask0", "ask1"])),
        tools.clone(),
    ))
    .await
    .unwrap();
    let session = soon(agent.create_session(SessionConfig {
        tools: Some(vec!["ask0".into(), "ask1".into()]),
        // Under workspace-write, an unclassified risk asks.
        permission_policy: PermissionPolicy {
            preset: PermissionPreset::WorkspaceWrite,
            ..Default::default()
        },
        ..SessionConfig::default()
    }))
    .await
    .unwrap();
    session
        .submit("go".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    let approval = session
        .service::<dim_agent_capabilities::approval::Approval>("main")
        .unwrap();
    // Both questions are pending at the same time: admission of the whole
    // batch runs before any call's step does.
    soon(async {
        loop {
            let mut pending = Vec::new();
            approval.replay(|request| pending.push(request.call.name.clone()));
            if pending.len() == 2 {
                return pending;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await;
    assert!(tools.started().is_empty(), "nothing runs before an answer");
    for _ in 0..2 {
        let mut pending = Vec::new();
        approval.replay(|request| pending.push(request.approval_id.clone()));
        for id in pending {
            approval.respond(&id, ApprovalDecision::Deny).await.unwrap();
        }
    }
    soon(session.idle("main")).await.unwrap();
    let _ = agent.close().await;
}

/// An approved read runs while its sibling of the same step still waits.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_approved_concurrent_call_runs_while_its_sibling_still_waits() {
    let tools = Arc::new(Segmented::default());
    let agent = MinimalHost::start(&builder(
        Arc::new(BatchModel(&["ask0", "ask1"])),
        tools.clone(),
    ))
    .await
    .unwrap();
    let session = soon(agent.create_session(SessionConfig {
        tools: Some(vec!["ask0".into(), "ask1".into()]),
        permission_policy: PermissionPolicy {
            preset: PermissionPreset::WorkspaceWrite,
            ..Default::default()
        },
        ..SessionConfig::default()
    }))
    .await
    .unwrap();
    session
        .submit("go".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    let approval = session
        .service::<dim_agent_capabilities::approval::Approval>("main")
        .unwrap();
    // Answer only the later call, then require the earlier one to be pending
    // and the later one to have run.
    let second = soon(async {
        loop {
            let mut pending = Vec::new();
            approval.replay(|request| pending.push(request.clone()));
            pending.sort_by(|left, right| left.call.name.cmp(&right.call.name));
            if pending.len() == 2 {
                return pending[1].approval_id.clone();
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await;
    approval
        .respond(&second, ApprovalDecision::Allow)
        .await
        .unwrap();
    soon(async {
        loop {
            if !tools.started().is_empty() {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
    let mut pending = Vec::new();
    approval.replay(|request| pending.push(request.call.name.clone()));
    assert_eq!(
        pending,
        ["ask0"],
        "the sibling is still waiting while the later call ran"
    );
    let first = soon(async {
        let mut pending = Vec::new();
        approval.replay(|request| pending.push(request.approval_id.clone()));
        pending.remove(0)
    })
    .await;
    approval
        .respond(&first, ApprovalDecision::Deny)
        .await
        .unwrap();
    soon(session.idle("main")).await.unwrap();
    let _ = agent.close().await;
}

/// A batch `[x0, x1, x2, x3]` where `x2` is denied during admission. Its
/// step settles while `x0` still runs, and the settled-prefix rule must keep
/// the calls after `x0` from starting early.
struct DeniedStep {
    /// Reaches while `x0` is inside the handler.
    entered: Arc<Notify>,
    returned: Arc<std::sync::Mutex<std::collections::HashSet<String>>>,
    /// Calls that entered before the call they follow returned.
    early: Arc<std::sync::Mutex<Vec<String>>>,
    /// Calls in the order they started.
    started: Arc<std::sync::Mutex<Vec<String>>>,
}
impl DeniedStep {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            entered: Arc::default(),
            returned: Arc::default(),
            early: Arc::default(),
            started: Arc::default(),
        })
    }
    fn returned(&self, name: &str) -> bool {
        self.returned.lock().unwrap().contains(name)
    }
    fn early(&self) -> Vec<String> {
        self.early.lock().unwrap().clone()
    }
    fn started(&self) -> Vec<String> {
        self.started.lock().unwrap().clone()
    }
}
impl ToolDriver for DeniedStep {
    fn definitions(&self) -> Vec<ToolDefinition> {
        ["x0", "x1", "x2", "x3"]
            .map(|name| definition(name, name, serde_json::json!({"type":"object"})))
            .to_vec()
    }
    fn execute(
        &self,
        call: ToolCall,
        _: ToolExecutionContext,
        _: CancellationToken,
    ) -> HostFuture<'_, ToolResult> {
        self.started.lock().unwrap().push(call.name.clone());
        for before in ["x0", "x1"] {
            let follows =
                (call.name == "x1" && before == "x0") || (call.name == "x3" && before == "x1");
            if follows && !self.returned(before) {
                self.early.lock().unwrap().push(call.name.clone());
            }
        }
        if call.name == "x0" {
            let entered = self.entered.clone();
            let returned = self.returned.clone();
            return Box::pin(async move {
                entered.notify_one();
                // Long enough that the later call's admission, including its
                // denial, is finished well before `x0` returns.
                for _ in 0..32 {
                    tokio::task::yield_now().await;
                }
                returned.lock().unwrap().insert("x0".into());
                Ok(ToolResult::text("x0 done"))
            });
        }
        let name = call.name.clone();
        let returned = self.returned.clone();
        Box::pin(async move {
            for _ in 0..8 {
                tokio::task::yield_now().await;
            }
            returned.lock().unwrap().insert(name.clone());
            Ok(ToolResult::text(format!("{name} done")))
        })
    }
}

/// One request carrying `names` as one batch, then text.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_denied_step_settles_first_without_starting_the_steps_after_it() {
    let tools = DeniedStep::new();
    let builder = builder(
        Arc::new(BatchModel(&["x0", "x1", "x2", "x3"])),
        tools.clone(),
    );
    let agent = MinimalHost::start(&builder).await.unwrap();
    let session = soon(agent.create_session(SessionConfig {
        tools: Some(vec!["x0".into(), "x1".into(), "x2".into(), "x3".into()]),
        permission_policy: PermissionPolicy {
            // `x2` is unclassified, so it asks; the test denies it at
            // admission, which settles its own step while `x0` runs.
            preset: PermissionPreset::WorkspaceWrite,
            allowed_tools: vec!["x0".into(), "x1".into(), "x3".into()],
            ..Default::default()
        },
        ..SessionConfig::default()
    }))
    .await
    .unwrap();
    session
        .submit("go".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    // `x0` holds the batch. `x2` asks for approval and the test denies it, so
    // its step settles while `x0` still runs; the calls after `x0` must wait.
    // The denial is a fact only after Approval recorded it, so the test waits
    // for that request and never for a timer.
    soon(tools.entered.notified()).await;
    // `x2` asks for approval and is denied. Its step settles while `x0` still
    // runs, because `x0` yields before returning, so a prefix rule that counts
    // any settled step lets `x1` start early.
    let approval = session
        .service::<dim_agent_capabilities::approval::Approval>("main")
        .unwrap();
    let denied = soon(async {
        loop {
            let mut asked = None;
            approval.replay(|request| {
                if request.call.name == "x2" {
                    asked = Some(request.approval_id.clone());
                }
            });
            if let Some(id) = asked {
                return id;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
    approval
        .respond(&denied, ApprovalDecision::Deny)
        .await
        .unwrap();
    soon(session.idle("main")).await.unwrap();
    assert_eq!(
        tools.started(),
        ["x0", "x1", "x3"],
        "the denied call never ran and the rest kept their order"
    );
    assert!(
        tools.early().is_empty(),
        "these calls entered before the call they follow returned: {:?}",
        tools.early()
    );
    let events = chain_events(&session, "main", ..).await.unwrap();
    let results = events
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::ToolResult(data) => Some(data.results.clone()),
            _ => None,
        })
        .flat_map(|results| results.into_iter())
        .collect::<Vec<_>>();
    let denied = results
        .iter()
        .find(|item| item.call_id == "call-x2")
        .expect("the denied call has a result");
    assert_eq!(denied.result.is_error, Some(true), "{denied:?}");
    let _ = agent.close().await;
}

//! The `codemode` tool through the real Agent runtime: the model calls it,
//! the program calls pinned tools through the branch `ToolExecutor` gate, and
//! child `step/tool-call`/`step/tool-result` facts carry the child call and
//! its effects before the outer result settles.
use dim_agent_capabilities::{
    adapters::{DefaultPrepareModelPlugin, MockTools},
    approval::Approval,
    defaults,
    history::HistoryRenderer,
    host::*,
    methods::{RpcError, SessionMethods},
    tool_registry::definition,
};
use dim_agent_runtime::{
    AgentBuilder, AgentConfig, Feature, MemoryTimelineStore, Session, SessionConfig,
    testing::{MinimalHost, branch_events},
};
use dim_agent_types::*;
use rutis::host::CtxHost;
use serde_json::json;
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio_util::sync::CancellationToken;

#[derive(Default)]
struct Script {
    batches: Mutex<VecDeque<Vec<ToolCall>>>,
    requests: Mutex<Vec<Arc<ModelRequest>>>,
}

impl Script {
    fn push(&self, calls: &[(&str, &str, serde_json::Value)]) {
        let calls = calls
            .iter()
            .map(|(name, id, input)| ToolCall {
                id: (*id).into(),
                name: (*name).into(),
                input: input.as_object().unwrap().clone(),
            })
            .collect();
        self.batches.lock().unwrap().push_back(calls);
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
            self.requests.lock().unwrap().push(request);
            let id = format!("m{}", self.requests.lock().unwrap().len());
            let mut chunks = vec![Ok(ModelChunk::MessageStart {
                message_id: id.clone(),
            })];
            let reason = match self.batches.lock().unwrap().pop_front() {
                Some(calls) => {
                    for call in calls {
                        chunks.push(Ok(ModelChunk::ToolStart {
                            id: call.id.clone(),
                            name: call.name.clone(),
                        }));
                        chunks.push(Ok(ModelChunk::ToolUse(call)));
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

struct TestTools {
    inner: MockTools,
}

impl ToolDriver for TestTools {
    fn definitions(&self) -> Vec<ToolDefinition> {
        let mut definitions = self.inner.definitions();
        definitions.push(ToolDefinition {
            risk: Some("session".into()),
            ..definition(
                "write_effect",
                "Return a confirmed write effect.",
                json!({"type": "object", "additionalProperties": false}),
            )
        });
        definitions.push(ToolDefinition {
            risk: Some("execute_process".into()),
            ..definition(
                "run",
                "Run a child command that needs approval.",
                json!({"type": "object", "additionalProperties": false}),
            )
        });
        definitions
    }

    fn execute(
        &self,
        call: ToolCall,
        context: ToolExecutionContext,
        cancel: CancellationToken,
    ) -> HostFuture<'_, ToolResult> {
        match call.name.as_str() {
            "write_effect" => Box::pin(async move {
                let mut result = ToolResult::text("wrote");
                result.structured_content = Some(
                    json!({"path": "/tmp/codemode"})
                        .as_object()
                        .unwrap()
                        .clone(),
                );
                result.effects = Some(vec![ToolEffect::FileWritten {
                    path: "/tmp/codemode".into(),
                }]);
                Ok(result)
            }),
            "run" => Box::pin(async move { Ok(ToolResult::text("ran")) }),
            _ => self.inner.execute(call, context, cancel),
        }
    }
}

struct Fixture {
    agent: MinimalHost,
    model: Arc<Script>,
}

impl Fixture {
    async fn new() -> Self {
        let model = Arc::new(Script::default());
        let mut builder = AgentBuilder::new(AgentConfig::default());
        builder
            .timeline_store(Arc::new(MemoryTimelineStore::default()))
            .session_value(Arc::new(HistoryRenderer::default()))
            .process_value(Arc::new(ModelHost(model.clone())))
            .session_value(Arc::new(PrepareContextHost(Arc::new(IdentityContext))))
            .session_value(Arc::new(ContextModelHost(None)))
            .session_value(Arc::new(CompactionStrategyHost(None)))
            .require_feature(
                Feature::new("PrepareModelHost").session("PrepareModelHost", |names| {
                    Ok(Box::new(DefaultPrepareModelPlugin::new(names)?))
                }),
            );
        defaults::install(&mut builder);
        defaults::tool_host(&mut builder, Arc::new(TestTools { inner: MockTools }));
        dim_agent_questions::install(&mut builder);
        dim_codemode::install_required(&mut builder);
        let agent = MinimalHost::start(&builder).await.unwrap();
        Self { agent, model }
    }

    async fn session(&self) -> Session {
        self.session_with_policy(PermissionPolicy::default()).await
    }

    async fn session_with_policy(&self, permission_policy: PermissionPolicy) -> Session {
        self.session_with_tools(
            vec!["codemode".into(), "echo".into()],
            false,
            permission_policy,
        )
        .await
    }

    async fn session_with_tools(
        &self,
        tools: Vec<String>,
        interactive: bool,
        permission_policy: PermissionPolicy,
    ) -> Session {
        self.agent
            .create_session(SessionConfig {
                tools: Some(tools),
                interactive,
                permission_policy,
                ..SessionConfig::default()
            })
            .await
            .unwrap()
    }

    async fn restart(&self, diagnostics: rutis::host::diagnostics::RuntimeDiagnostics, name: &str) {
        let id = diagnostics
            .plugins
            .iter()
            .find(|plugin| plugin.name == name)
            .unwrap()
            .id;
        self.agent.root().view(id).unwrap().restart().await.unwrap();
    }

    async fn shutdown(self) {
        self.agent.close().await.unwrap();
    }
}

async fn result(session: &Session, call_id: &str) -> ToolResult {
    tokio::time::timeout(Duration::from_secs(10), session.idle("main"))
        .await
        .unwrap()
        .unwrap();
    let events = branch_events(session, "main", ..).await.unwrap();
    events
        .iter()
        .find_map(|event| match &event.kind {
            EventKind::ToolResult(data) => data
                .results
                .iter()
                .find(|result| result.call_id == call_id)
                .map(|result| result.result.clone()),
            _ => None,
        })
        .expect("the codemode result is committed")
}

async fn respond(
    session: &Session,
    params: serde_json::Value,
) -> Result<serde_json::Value, RpcError> {
    let methods = session
        .session_service::<SessionMethods>()
        .unwrap()
        .provider("session.question.respond")
        .unwrap();
    let accepted = methods.accept(
        "session.question.respond".into(),
        params,
        None,
        CancellationToken::new(),
        |_| {},
    )?;
    accepted.reply.await.unwrap()
}

fn text(result: &ToolResult) -> &str {
    match &result.content {
        ToolContent::Text(text) => text,
        ToolContent::Blocks(_) => panic!("expected text"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_program_calls_a_pinned_tool_and_the_outer_result_carries_its_value() {
    let fixture = Fixture::new().await;
    let session = fixture.session().await;
    let code = r#"const value = await tools.call("echo", {text: "hello"}); return value;"#;
    fixture
        .model
        .push(&[("codemode", "c1", json!({ "code": code }))]);
    session
        .submit("run".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    let result = result(&session, "c1").await;
    assert_eq!(text(&result), r#"{"content":"hello"}"#);
    assert_eq!(result.is_error, None);
    let events = branch_events(&session, "main", ..).await.unwrap();
    let child_call = events
        .iter()
        .find(|event| {
            event.parent_call_id.as_deref() == Some("c1")
                && matches!(&event.kind, EventKind::ToolCall(data) if data.calls.iter().any(|call| call.name == "echo"))
        })
        .expect("the child echo batch is committed");
    let child_id = match &child_call.kind {
        EventKind::ToolCall(data) => data.calls[0].id.clone(),
        _ => unreachable!(),
    };
    assert!(
        events.iter().any(|event| {
            event.parent_call_id.as_deref() == Some("c1")
                && matches!(&event.kind, EventKind::ToolResult(data)
                    if data.call_event_id.as_deref() == Some(child_call.id.as_str())
                        && data.results.iter().any(|item| item.call_id == child_id))
        }),
        "the child result closes its child batch"
    );
    fixture.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_child_result_reaches_the_program_whole() {
    let fixture = Fixture::new().await;
    let session = fixture
        .session_with_tools(
            vec!["codemode".into(), "write_effect".into()],
            false,
            PermissionPolicy::default(),
        )
        .await;
    let code = r#"const value = await tools.call("write_effect", {}); return value;"#;
    fixture
        .model
        .push(&[("codemode", "c1", json!({ "code": code }))]);
    session
        .submit("write".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    let result = result(&session, "c1").await;
    let value: serde_json::Value = serde_json::from_str(text(&result)).unwrap();
    assert_eq!(value["content"], "wrote");
    assert_eq!(value["structuredContent"], json!({"path": "/tmp/codemode"}));
    fixture.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_child_ask_user_waits_and_returns_the_answer() {
    let fixture = Fixture::new().await;
    let session = fixture
        .session_with_tools(
            vec!["codemode".into(), "ask_user".into()],
            true,
            PermissionPolicy::default(),
        )
        .await;
    let code = r#"const answer = await tools.call("ask_user", {questions: [{id: "q", question: "Pick", options: [{label: "A"}, {label: "B"}]}]}); return {selected: answer.structuredContent.answers[0].selected[0]};"#;
    fixture
        .model
        .push(&[("codemode", "c1", json!({ "code": code }))]);
    session
        .submit("ask".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    let mut asked = None;
    for _ in 0..1000 {
        let events = branch_events(&session, "main", ..).await.unwrap();
        asked = events.iter().find_map(|event| match &event.kind {
            EventKind::ToolCall(data) if event.parent_call_id.as_deref() == Some("c1") => data
                .calls
                .iter()
                .find(|call| call.name == "ask_user")
                .map(|call| (data.request_id.clone(), call.id.clone())),
            _ => None,
        });
        if asked.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let (request_id, call_id) = asked.expect("the child ask_user batch is committed");
    let pending = session
        .session_service::<SessionMethods>()
        .unwrap()
        .provider("session.question.pending")
        .unwrap()
        .accept(
            "session.question.pending".into(),
            json!({"sessionId": session.id(), "branchId": "main"}),
            None,
            CancellationToken::new(),
            |_| {},
        )
        .unwrap();
    let pending = pending.reply.await.unwrap().unwrap();
    assert!(
        pending["batches"].to_string().contains(&call_id),
        "the child ask_user batch is pending"
    );
    respond(
        &session,
        json!({
            "sessionId": session.id(),
            "branchId": "main",
            "requestId": request_id,
            "callId": call_id,
            "answers": [{"questionId": "q", "selected": ["A"]}],
        }),
    )
    .await
    .unwrap();
    let result = result(&session, "c1").await;
    let value: serde_json::Value = serde_json::from_str(text(&result)).unwrap();
    assert_eq!(
        value["selected"], "A",
        "the program sees the answer: {value}"
    );
    fixture.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_child_branch_records_the_child_call_as_its_source() {
    let fixture = Fixture::new().await;
    let session = fixture
        .session_with_tools(
            vec!["codemode".into(), "branch".into()],
            false,
            PermissionPolicy::default(),
        )
        .await;
    let code = r#"const line = await tools.call("branch", {action: "create", prompt: "child line"}); return {content: line.content};"#;
    fixture
        .model
        .push(&[("codemode", "c1", json!({ "code": code }))]);
    session
        .submit("branch".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    let result = result(&session, "c1").await;
    assert!(
        text(&result).contains("Line created"),
        "the child branch result reaches the program: {}",
        text(&result)
    );
    let events = branch_events(&session, "main", ..).await.unwrap();
    let child_call = events
        .iter()
        .find_map(|event| match &event.kind {
            EventKind::ToolCall(data) if event.parent_call_id.as_deref() == Some("c1") => data
                .calls
                .iter()
                .find(|call| call.name == "branch")
                .map(|call| call.id.clone()),
            _ => None,
        })
        .expect("the child branch batch is committed");
    assert!(
        events.iter().any(|event| matches!(
            &event.kind,
            EventKind::BranchCreate(data)
                if matches!(&data.source, BranchSource::Tool { call_id, .. } if call_id == &child_call)
        )),
        "branch/create names the child call"
    );
    fixture.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_completed_child_effect_survives_the_parent_reload() {
    let fixture = Fixture::new().await;
    let session = fixture
        .session_with_tools(
            vec!["codemode".into(), "write_effect".into(), "wait".into()],
            false,
            PermissionPolicy::default(),
        )
        .await;
    let code = r#"await tools.call("write_effect", {}); await tools.call("wait", {millis: 1000}); return "done";"#;
    fixture
        .model
        .push(&[("codemode", "c1", json!({ "code": code }))]);
    session
        .submit("effect".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    for _ in 0..1000 {
        let events = branch_events(&session, "main", ..).await.unwrap();
        let completed = events.iter().any(|event| {
            matches!(&event.kind, EventKind::ToolResult(data)
            if event.parent_call_id.as_deref() == Some("c1")
                && data.results.iter().any(|item| item.result.effects.is_some()))
        });
        if completed {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    fixture
        .restart(session.branch_diagnostics("main").unwrap(), "ToolExecutor")
        .await;
    let result = result(&session, "c1").await;
    let events = branch_events(&session, "main", ..).await.unwrap();
    let results = events
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::ToolResult(data) => data
                .results
                .iter()
                .find(|item| item.call_id == "c1")
                .map(|item| item.result.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(
        result.effects.as_ref().is_some_and(|effects| {
            effects.iter().any(
            |effect| matches!(effect, ToolEffect::FileWritten { path } if path == "/tmp/codemode")
        )
        }),
        "the completed child effect survives: {:?} / {:?}",
        result.effects,
        results
    );
    fixture.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_child_approval_names_the_child_call() {
    let fixture = Fixture::new().await;
    let session = fixture
        .session_with_tools(
            vec!["codemode".into(), "run".into()],
            false,
            PermissionPolicy {
                preset: PermissionPreset::WorkspaceWrite,
                ..PermissionPolicy::default()
            },
        )
        .await;
    let code = r#"const value = await tools.call("run", {}); return {content: value.content};"#;
    fixture
        .model
        .push(&[("codemode", "c1", json!({ "code": code }))]);
    session
        .submit("approve".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    let asked = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let events = branch_events(&session, "main", ..).await.unwrap();
            if let Some((id, child)) = events.iter().find_map(|event| match &event.kind {
                EventKind::ApprovalRequest(data) => match &data.source {
                    BranchSource::Tool { call_id, .. }
                        if event.parent_call_id.as_deref() == Some(call_id) =>
                    {
                        Some((data.approval_id.clone(), call_id.clone()))
                    }
                    _ => None,
                },
                _ => None,
            }) {
                return (id, child);
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await;
    let (approval_id, child_call) = match asked {
        Ok(asked) => asked,
        Err(_) => {
            let events = branch_events(&session, "main", ..).await.unwrap();
            let summary = events
                .iter()
                .filter(|event| event.parent_call_id.is_some())
                .map(|event| format!("{} {:?}", event.kind.key(), event.parent_call_id))
                .collect::<Vec<_>>();
            panic!("the child approval request appears: {summary:?}");
        }
    };
    let child_batch = branch_events(&session, "main", ..)
        .await
        .unwrap()
        .into_iter()
        .find(|event| {
            matches!(&event.kind, EventKind::ToolCall(data)
            if event.parent_call_id.as_deref() == Some("c1")
                && data.calls.iter().any(|call| call.id == child_call))
        });
    assert!(child_batch.is_some(), "the approval names the child batch");
    session
        .service::<Approval>("main")
        .unwrap()
        .respond(&approval_id, ApprovalDecision::Allow)
        .await
        .unwrap();
    let result = result(&session, "c1").await;
    let value: serde_json::Value = serde_json::from_str(text(&result)).unwrap();
    assert_eq!(value["content"], "ran");
    fixture.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_open_child_fails_when_tool_executor_reloads() {
    let fixture = Fixture::new().await;
    let session = fixture
        .session_with_tools(
            vec!["codemode".into(), "wait".into()],
            false,
            PermissionPolicy::default(),
        )
        .await;
    let code = r#"await tools.call("wait", {millis: 1000}); return "done";"#;
    fixture
        .model
        .push(&[("codemode", "c1", json!({ "code": code }))]);
    session
        .submit("wait".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    let mut child = None;
    for _ in 0..1000 {
        let events = branch_events(&session, "main", ..).await.unwrap();
        child = events.iter().find_map(|event| match &event.kind {
            EventKind::ToolCall(data) if event.parent_call_id.as_deref() == Some("c1") => data
                .calls
                .iter()
                .find(|call| call.name == "wait")
                .map(|call| call.id.clone()),
            _ => None,
        });
        if child.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let child = child.expect("the child wait batch is committed");
    fixture
        .restart(session.branch_diagnostics("main").unwrap(), "ToolExecutor")
        .await;
    let result = result(&session, "c1").await;
    assert!(
        text(&result).contains("Capability reload") || text(&result).contains("reload"),
        "the outer result names the reload: {}",
        text(&result)
    );
    let events = branch_events(&session, "main", ..).await.unwrap();
    assert!(
        events
            .iter()
            .any(|event| matches!(&event.kind, EventKind::ToolResult(data)
            if event.parent_call_id.as_deref() == Some("c1")
                && data.results.iter().any(|item| item.call_id == child))),
        "the open child has a terminal result"
    );
    fixture.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_denied_child_is_a_catchable_program_error() {
    let fixture = Fixture::new().await;
    let session = fixture
        .session_with_policy(PermissionPolicy {
            disallowed_tools: vec!["echo".into()],
            ..PermissionPolicy::default()
        })
        .await;
    let code = r#"try { await tools.call("echo", {text: "x"}); return "allowed"; } catch (error) { return error.message; }"#;
    fixture
        .model
        .push(&[("codemode", "c1", json!({ "code": code }))]);
    session
        .submit("run".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    let result = result(&session, "c1").await;
    let value: serde_json::Value = serde_json::from_str(text(&result)).unwrap();
    assert!(
        value.as_str().unwrap().contains("echo"),
        "the denial names the tool: {value}"
    );
    fixture.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_sessions_with_main_branches_keep_their_own_runners() {
    let fixture = Fixture::new().await;
    let first = fixture.session().await;
    let second = fixture.session().await;
    fixture
        .model
        .push(&[("codemode", "c1", json!({ "code": "return \"first\";" }))]);
    first
        .submit("run first".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    let first_result = result(&first, "c1").await;
    fixture
        .model
        .push(&[("codemode", "c2", json!({ "code": "return \"second\";" }))]);
    second
        .submit("run second".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    let second_result = result(&second, "c2").await;
    assert_eq!(text(&first_result), "\"first\"");
    assert_eq!(text(&second_result), "\"second\"");
    fixture.shutdown().await;
}

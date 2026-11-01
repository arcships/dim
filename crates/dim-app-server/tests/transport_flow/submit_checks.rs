//! The operator-input entries consult the Session's `submitChecks` before
//! writing (Hooks A.6.2, appendix D items 5 and 6).
use super::*;
use dim_agent_capabilities::{
    host::{HostFuture, ModelConnection, ModelDriver, ModelStream},
    input_queue::{CheckSubmission, SubmitCheckReply, SubmitCheckRequest, SubmitChecks},
};
use dim_agent_runtime::{Feature, Names};
use dim_agent_types::{ContentBlock, InputMessageData, ModelRequest};
use rutis::Ctx;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, Key, TypeKey};
use std::sync::Mutex;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Default)]
enum Answer {
    #[default]
    Proceed,
    ProceedWith(Vec<String>),
    Block(String),
}

/// The check the test drives, so an entry's order of work is observable.
#[derive(Default)]
struct TestCheck {
    answer: Mutex<Answer>,
    requests: Mutex<Vec<SubmitCheckRequest>>,
    entered: Mutex<Option<tokio::sync::mpsc::UnboundedSender<()>>>,
    gate: Mutex<Option<Arc<tokio::sync::Semaphore>>>,
}
impl TestCheck {
    fn answer(&self) -> Answer {
        self.answer.lock().unwrap().clone()
    }
    fn set(&self, answer: Answer) {
        *self.answer.lock().unwrap() = answer;
    }
    fn entries(&self) -> Vec<&'static str> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .map(|request| request.entry.name())
            .collect()
    }
}
impl CheckSubmission for TestCheck {
    fn check(
        &self,
        request: SubmitCheckRequest,
        _: CancellationToken,
    ) -> HostFuture<'_, SubmitCheckReply> {
        Box::pin(async move {
            self.requests.lock().unwrap().push(request);
            let gate = self.gate.lock().unwrap().clone();
            if let Some(entered) = self.entered.lock().unwrap().take() {
                let _ = entered.send(());
            }
            if let Some(gate) = gate {
                let _ = gate.acquire().await;
            }
            Ok(match self.answer() {
                Answer::Proceed => SubmitCheckReply::Proceed { blocks: Vec::new() },
                Answer::ProceedWith(blocks) => SubmitCheckReply::Proceed { blocks },
                Answer::Block(message) => SubmitCheckReply::Block { message },
            })
        })
    }
}

struct CheckPart {
    checks: Key<SubmitChecks>,
    check: Arc<TestCheck>,
}
impl DynPlugin for CheckPart {
    fn name(&self) -> &str {
        "SubmitCheckTest"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.checks.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let checks = ctx.require_at(&self.checks)?;
            checks.register(ctx, "test", self.check.clone())?;
            Ok(Effect::Done)
        })
    }
}

/// Echoes like the mock model and keeps each request for inspection.
#[derive(Default)]
struct Recording(Mutex<Vec<Arc<ModelRequest>>>);
impl ModelDriver for Recording {
    fn invoke(
        &self,
        request: Arc<ModelRequest>,
        _: ModelConnection,
        cancel: CancellationToken,
    ) -> HostFuture<'_, ModelStream> {
        self.0.lock().unwrap().push(request.clone());
        dim_agent_capabilities::adapters::EchoModel.invoke(request, ModelConnection::Local, cancel)
    }
}
impl Recording {
    /// The texts of the parts of the last user message of the last request.
    fn last_user_texts(&self) -> Vec<String> {
        let requests = self.0.lock().unwrap();
        let request = requests.last().expect("a model request");
        let messages = serde_json::to_value(&request.messages).unwrap();
        let user = messages.as_array().unwrap().last().unwrap();
        user["content"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|part| part["text"].as_str().map(str::to_owned))
            .collect()
    }
}

async fn launch(
    check: Arc<TestCheck>,
) -> (RunningServer, dim_agent_runtime::Agent, Arc<Recording>) {
    let model = Arc::new(Recording::default());
    let mut builder = AgentBuilder::new(AgentConfig::default());
    builder.timeline_store(Arc::new(dim_agent_runtime::MemoryTimelineStore::default()));
    defaults::install(&mut builder);
    defaults::mock_model_hosts_with_model(
        &mut builder,
        dim_agent_capabilities::history::HistoryRenderer::default(),
        model.clone(),
    );
    defaults::tool_host(&mut builder, Arc::new(MockTools));
    dim_app_server::collaboration::install(&mut builder);
    builder.require_feature(Feature::new("SubmitCheckTest").session(
        "SubmitCheckTest",
        move |names: &Names<'_>| {
            Ok(Box::new(CheckPart {
                checks: names.key()?,
                check: check.clone(),
            }))
        },
    ));
    let server = serve(
        builder,
        ServerConfig {
            unix_path: Some(std::path::PathBuf::from("/tmp").join(format!(
                "dim-server-test-{}/server.sock",
                uuid::Uuid::new_v4()
            ))),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let agent = server.agent().unwrap();
    (server, agent, model)
}

/// The operator inputs of a branch, in order.
async fn inputs(session: &dim_agent_runtime::Session) -> Vec<InputMessageData> {
    session
        .local("main", ..)
        .await
        .unwrap()
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::InputMessage(data) if data.user.is_some() => Some(data.clone()),
            _ => None,
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_block_fails_the_operator_entries_and_writes_nothing() {
    let check = Arc::new(TestCheck::default());
    check.set(Answer::Block("Hook test: no prompts today".into()));
    let (server, agent, _) = launch(check.clone()).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let created = client
        .rpc(
            "session.create",
            json!({"cwd": std::env::temp_dir(), "tools": ["branch"]}),
        )
        .await;
    assert_ok(&created);
    let id = created["result"]["sessionId"].as_str().unwrap().to_owned();
    let session = agent.get_session(&id).unwrap();

    let submission = uuid::Uuid::new_v4().to_string();
    let refused = client
        .rpc(
            "session.submit",
            json!({"sessionId": id, "branchId": "main", "submissionId": submission.clone(),
                "text": "delete everything"}),
        )
        .await;
    assert_eq!(refused["error"]["code"], -32000);
    assert_eq!(refused["error"]["data"]["appError"], "input_blocked");
    assert_eq!(refused["error"]["message"], "Hook test: no prompts today");
    assert!(inputs(&session).await.is_empty(), "a block wrote an input");
    // Appendix D item 5: a blocked submission stays unknown to
    // `session.input_lookup`, and a retry with its id runs the checks again.
    let looked_up = client
        .rpc(
            "session.input_lookup",
            json!({"sessionId": id, "branchId": "main", "submissionId": submission.clone()}),
        )
        .await;
    assert_ok(&looked_up);
    assert_eq!(
        looked_up["result"],
        json!({"status": "unknown"}),
        "a blocked submission stays unknown to session.input_lookup"
    );
    let refused = client
        .rpc(
            "session.submit",
            json!({"sessionId": id, "branchId": "main", "submissionId": submission.clone(),
                "text": "delete everything"}),
        )
        .await;
    assert_eq!(refused["error"]["data"]["appError"], "input_blocked");
    assert!(inputs(&session).await.is_empty(), "a block wrote an input");

    let branches = session.branches().len();
    let refused = client
        .rpc(
            "session.branch_create",
            json!({"sessionId": id, "parentBranchId": "main",
                "at": session.sequence("main").unwrap(), "prompt": "child work"}),
        )
        .await;
    assert_eq!(refused["error"]["data"]["appError"], "input_blocked");
    assert_eq!(
        session.branches().len(),
        branches,
        "a block created a branch"
    );

    assert_eq!(check.entries(), ["submit", "submit", "branch"]);
    server.shutdown();
    server.join().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_allowed_prompt_carries_the_checks_context_after_the_operator_text() {
    let check = Arc::new(TestCheck::default());
    check.set(Answer::ProceedWith(vec!["check context".into()]));
    let (server, agent, model) = launch(check.clone()).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let created = client
        .rpc(
            "session.create",
            json!({"cwd": std::env::temp_dir(), "tools": []}),
        )
        .await;
    assert_ok(&created);
    let id = created["result"]["sessionId"].as_str().unwrap().to_owned();
    let session = agent.get_session(&id).unwrap();

    let submitted = client
        .rpc(
            "session.submit",
            json!({"sessionId": id, "branchId": "main", "text": "hello"}),
        )
        .await;
    assert_ok(&submitted);
    let input = inputs(&session).await.pop().expect("the input was written");
    let user = input.user.expect("the input is operator input");
    assert_eq!(
        user.content,
        vec![
            ContentBlock::Text {
                text: "hello".into()
            },
            ContentBlock::Text {
                text: "check context".into()
            },
        ],
        "the context follows the operator's own content"
    );
    assert!(
        input.blocks.is_empty(),
        "the input carries no raw block, so it stays operator input"
    );

    let idle = client
        .rpc("session.idle", json!({"sessionId": id, "branchId": "main"}))
        .await;
    assert_ok(&idle);
    assert_eq!(model.last_user_texts(), ["hello", "check context"]);
    server.shutdown();
    server.join().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_proceeding_branch_create_drops_the_checks_context() {
    let check = Arc::new(TestCheck::default());
    check.set(Answer::ProceedWith(vec!["check context".into()]));
    let (server, agent, _) = launch(check.clone()).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let created = client
        .rpc(
            "session.create",
            json!({"cwd": std::env::temp_dir(), "tools": ["branch"]}),
        )
        .await;
    assert_ok(&created);
    let id = created["result"]["sessionId"].as_str().unwrap().to_owned();
    let session = agent.get_session(&id).unwrap();

    let created = client
        .rpc(
            "session.branch_create",
            json!({"sessionId": id, "parentBranchId": "main",
                "at": session.sequence("main").unwrap(), "prompt": "child work"}),
        )
        .await;
    assert_ok(&created);
    let child = created["result"]["branch"]["branchId"]
        .as_str()
        .unwrap()
        .to_owned();
    let first = session
        .local(&child, ..)
        .await
        .unwrap()
        .into_iter()
        .find_map(|event| match &event.kind {
            EventKind::InputMessage(data) => data.user.clone(),
            _ => None,
        })
        .expect("the branch's first input");
    assert_eq!(
        first.content,
        vec![ContentBlock::Text {
            text: "child work".into()
        }],
        "a branch's first input is its prompt alone"
    );
    server.shutdown();
    server.join().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_client_that_disconnects_during_a_check_still_gets_its_input_written() {
    let check = Arc::new(TestCheck::default());
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let (entered, mut entering) = tokio::sync::mpsc::unbounded_channel();
    *check.gate.lock().unwrap() = Some(gate.clone());
    *check.entered.lock().unwrap() = Some(entered);
    let (server, agent, _) = launch(check.clone()).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let created = client
        .rpc(
            "session.create",
            json!({"cwd": std::env::temp_dir(), "tools": []}),
        )
        .await;
    assert_ok(&created);
    let id = created["result"]["sessionId"].as_str().unwrap().to_owned();
    let session = agent.get_session(&id).unwrap();

    client
        .send(
            "session.submit",
            json!({"sessionId": id, "branchId": "main", "text": "still here"}),
        )
        .await;
    entering.recv().await.expect("the check started");
    drop(client);
    gate.add_permits(1);
    for _ in 0..500 {
        if !inputs(&session).await.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        inputs(&session).await.len(),
        1,
        "a client's disconnect cancels nothing; the input is written"
    );
    server.shutdown();
    server.join().await.unwrap();
}

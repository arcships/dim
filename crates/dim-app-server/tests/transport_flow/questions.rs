//! Questions through the public RPC interface (design Appendix D): Rpc
//! routes `session.question.respond` to the Session it names, the answer
//! arrives as the batch's `step/tool-result`, a fork cannot cut a pending
//! question, and an answer names its call by request as well as call id.
use super::deletion::Store;
use super::*;
use dim_agent_capabilities::host::{ModelChunk, ModelConnection, ModelDriver, ModelStream};
use dim_agent_types::{MessageContent, MessagePart, ModelRequest, ToolContent};

const ASK: &str = r#"tool:ask_user {"questions":[{"header":"Database","question":"Which database?","options":[{"label":"PostgreSQL","description":"managed"},{"label":"SQLite"}]}]}"#;

async fn launch_questions(store: &Store) -> RunningServer {
    launch_with(
        Arc::new(MockTools),
        store.config(),
        dim_agent_questions::install,
    )
    .await
    .0
}
async fn session(client: &mut Client, interactive: bool) -> String {
    let mut params = create(json!(["ask_user"]));
    params["interactive"] = json!(interactive);
    let created = client.rpc("session.create", params).await;
    assert_ok(&created);
    let sid = created["result"]["sessionId"].as_str().unwrap().to_owned();
    assert_ok(&client.rpc("session.attach", json!({"sessionId":sid})).await);
    sid
}
async fn submit(client: &mut Client, sid: &str, text: &str) {
    assert_ok(
        &client
            .rpc(
                "session.submit",
                json!({"sessionId":sid,"branchId":"main","text":text}),
            )
            .await,
    );
}
/// The next `session.event` step named `name`, as its notice params.
async fn step(client: &mut Client, name: &str) -> Value {
    loop {
        let notice = client.notice("session.event").await;
        let event = &notice["params"]["event"];
        if event["type"] == "step" && event["name"] == name {
            return notice["params"].clone();
        }
    }
}
fn respond(sid: &str, request: &str, call: &str, answers: Value) -> Value {
    json!({"sessionId":sid,"branchId":"main","requestId":request,"callId":call,"answers":answers})
}
/// The `request_id` of a `step/tool-call` notice: with the call id, the name
/// a client answers the call by.
fn request(asked: &Value) -> String {
    asked["event"]["data"]["request_id"]
        .as_str()
        .unwrap()
        .to_owned()
}
fn app_error(reply: &Value) -> &str {
    reply["error"]["data"]["appError"]
        .as_str()
        .unwrap_or_else(|| panic!("expected an error reply: {reply}"))
}
/// Waits until the handler registered `call` of `request`: an empty answer
/// list is refused as invalid only while the call waits.
async fn waiting(client: &mut Client, sid: &str, request: &str, call: &str) {
    for _ in 0..500 {
        let reply = client
            .rpc(
                "session.question.respond",
                respond(sid, request, call, json!([])),
            )
            .await;
        match app_error(&reply) {
            "invalid_params" => return,
            "question_not_pending" => tokio::time::sleep(Duration::from_millis(20)).await,
            other => panic!("{other}: {reply}"),
        }
    }
    panic!("{call} never waited");
}

/// Acceptance 1, 4 and 8 on the wire: the method is listed for the Session,
/// rejected answers keep the call waiting, the first answer is the call's
/// result and a second finds nothing; the batch's `step/tool-result` carries
/// the answer and a fork at the pending call is refused.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_client_answers_a_waiting_question_through_rpc() {
    let store = Store::new();
    let server = launch_questions(&store).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let sid = session(&mut client, true).await;
    let methods = client
        .rpc("session.methods", json!({"sessionId":sid}))
        .await;
    assert!(
        methods["result"]["methods"]
            .as_array()
            .unwrap()
            .contains(&json!({"name":"session.question.respond","mutating":true})),
        "{methods}"
    );

    submit(&mut client, &sid, ASK).await;
    let asked = step(&mut client, "tool-call").await;
    assert_eq!(asked["branchId"], "main");
    let call = &asked["event"]["data"]["calls"][0];
    assert_eq!(call["name"], "ask_user");
    let call_id = call["id"].as_str().unwrap().to_owned();
    let request_id = request(&asked);
    let call_sequence = asked["event"]["sequence"].as_u64().unwrap();
    waiting(&mut client, &sid, &request_id, &call_id).await;

    // A fork cuts only at a rest point, so never at a pending question.
    let fork = client
        .rpc("session.fork", json!({"sessionId":sid,"at":call_sequence}))
        .await;
    assert_eq!(app_error(&fork), "session_invalid_input");
    assert!(
        fork["error"]["message"]
            .as_str()
            .unwrap()
            .contains("without results"),
        "{fork}"
    );

    let sqlite = json!([{"questionId":"1","selected":["SQLite"]}]);
    for (params, code) in [
        (
            json!({"sessionId":sid,"branchId":"main","requestId":request_id,"callId":call_id}),
            "invalid_params",
        ),
        // The request is part of the call's name.
        (
            json!({"sessionId":sid,"branchId":"main","callId":call_id,"answers":sqlite}),
            "invalid_params",
        ),
        (
            respond(
                &sid,
                &request_id,
                &call_id,
                json!([{"questionId":"1","selected":["MySQL"]}]),
            ),
            "invalid_params",
        ),
        (
            json!({"sessionId":sid,"branchId":"main","requestId":request_id,"callId":call_id,"declined":true,"extra":1}),
            "invalid_params",
        ),
        (
            respond(&sid, &request_id, "other", sqlite.clone()),
            "question_not_pending",
        ),
        (
            respond(&sid, "other", &call_id, sqlite.clone()),
            "question_not_pending",
        ),
        (
            json!({"sessionId":sid,"branchId":"child","requestId":request_id,"callId":call_id,"declined":true}),
            "question_not_pending",
        ),
    ] {
        let reply = client.rpc("session.question.respond", params).await;
        assert_eq!(app_error(&reply), code, "{reply}");
    }
    let answer = respond(&sid, &request_id, &call_id, sqlite);
    let accepted = client.rpc("session.question.respond", answer.clone()).await;
    assert_eq!(accepted["result"], json!({}), "{accepted}");
    let again = client.rpc("session.question.respond", answer).await;
    assert_eq!(app_error(&again), "question_not_pending");
    assert_eq!(again["error"]["code"], -32000);

    let settled = step(&mut client, "tool-result").await;
    let data = &settled["event"]["data"];
    assert_eq!(data["call_event_id"], asked["event"]["id"]);
    assert_eq!(data["results"][0]["call_id"], call_id.as_str());
    assert_eq!(
        data["results"][0]["result"],
        json!({
            "content": "User answered:\n1. Which database?\n   -> SQLite",
            "structuredContent": {"answers":[{"questionId":"1","selected":["SQLite"]}],"declined":false}
        })
    );
    assert_ok(
        &client
            .rpc("session.idle", json!({"sessionId":sid,"branchId":"main"}))
            .await,
    );
    server.shutdown();
    server.join().await.unwrap();
}

/// Acceptance 2 on the wire: `session.create {interactive: false}` declares
/// `ask_user` and lists the method, and the call returns the non-interactive
/// error at once.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_non_interactive_session_answers_ask_user_at_once() {
    let store = Store::new();
    let server = launch_questions(&store).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let sid = session(&mut client, false).await;
    let methods = client
        .rpc("session.methods", json!({"sessionId":sid}))
        .await;
    assert!(
        methods["result"]["methods"]
            .as_array()
            .unwrap()
            .contains(&json!({"name":"session.question.respond","mutating":true}))
    );
    submit(&mut client, &sid, ASK).await;
    let settled = step(&mut client, "tool-result").await;
    assert_eq!(
        settled["event"]["data"]["results"][0]["result"],
        json!({
            "content": "No one is available to answer questions in this session. Proceed without asking.",
            "isError": true
        })
    );
    assert_ok(
        &client
            .rpc("session.idle", json!({"sessionId":sid,"branchId":"main"}))
            .await,
    );
    server.shutdown();
    server.join().await.unwrap();
}

/// Echoes like the mock model, but names every tool call it proposes `c1`,
/// as a provider whose call ids restart in each response; keeps every
/// request.
#[derive(Default)]
struct ReusedCallIds(std::sync::Mutex<Vec<Arc<ModelRequest>>>);
impl ModelDriver for ReusedCallIds {
    fn invoke(
        &self,
        request: Arc<ModelRequest>,
        connection: ModelConnection,
        cancel: CancellationToken,
    ) -> HostFuture<'_, ModelStream> {
        self.0.lock().unwrap().push(request.clone());
        Box::pin(async move {
            let stream = dim_agent_capabilities::adapters::EchoModel
                .invoke(request, connection, cancel)
                .await?;
            Ok(Box::pin(stream.map(|chunk| {
                chunk.map(|chunk| match chunk {
                    ModelChunk::ToolStart { name, .. } => ModelChunk::ToolStart {
                        id: "c1".into(),
                        name,
                    },
                    ModelChunk::ToolUse(call) => ModelChunk::ToolUse(ToolCall {
                        id: "c1".into(),
                        ..call
                    }),
                    chunk => chunk,
                })
            })) as ModelStream)
        })
    }
}

/// §4.3 and acceptance 10 on the wire: the model reuses `c1` in a later
/// turn. Client B's late answer and late decline to turn 1's call return
/// `question_not_pending`, turn 2's call still takes client A's answer, and
/// the model reads that answer as turn 2's result.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_late_answer_from_another_client_never_reaches_a_reused_call_id() {
    use dim_agent_capabilities::{
        adapters::DefaultPrepareModelPlugin,
        history::HistoryRenderer,
        host::{
            CompactionStrategyHost, ContextModelHost, IdentityContext, ModelHost,
            PrepareContextHost,
        },
    };
    let store = Store::new();
    let model = Arc::new(ReusedCallIds::default());
    let mut builder = AgentBuilder::new(AgentConfig::default());
    builder.timeline_store(Arc::new(dim_agent_runtime::MemoryTimelineStore::default()));
    defaults::install(&mut builder);
    builder
        .session_value(Arc::new(HistoryRenderer::default()))
        .session_value(Arc::new(ModelHost(model.clone())))
        .session_value(Arc::new(PrepareContextHost(Arc::new(IdentityContext))))
        .session_value(Arc::new(ContextModelHost(None)))
        .session_value(Arc::new(CompactionStrategyHost(None)))
        .session_plugin(DefaultPrepareModelPlugin::new);
    defaults::tool_host(&mut builder, Arc::new(MockTools));
    dim_agent_questions::install(&mut builder);
    let mut config = store.config();
    config.unix_path = Some(std::path::PathBuf::from("/tmp").join(format!(
        "dim-questions-test-{}/server.sock",
        uuid::Uuid::new_v4()
    )));
    let server = serve(builder, config).await.unwrap();
    let mut a = Client::new_version(&server, false, 3).await;
    let sid = session(&mut a, true).await;
    let mut b = Client::new_version(&server, false, 3).await;
    assert_ok(&b.rpc("session.attach", json!({"sessionId":sid})).await);
    let ask = |question: &str| {
        format!(
            r#"tool:ask_user {{"questions":[{{"id":"decision","question":"{question}","allowCustom":false,"options":[{{"label":"Yes"}},{{"label":"No"}}]}}]}}"#
        )
    };
    let choose = |label: &str| json!([{"questionId":"decision","selected":[label]}]);
    let idle = json!({"sessionId":sid,"branchId":"main"});

    submit(&mut a, &sid, &ask("Deploy staging?")).await;
    let first = step(&mut a, "tool-call").await;
    assert_eq!(first["event"]["data"]["calls"][0]["id"], "c1");
    assert_eq!(step(&mut b, "tool-call").await["event"], first["event"]);
    let r1 = request(&first);
    waiting(&mut a, &sid, &r1, "c1").await;
    let accepted = a
        .rpc(
            "session.question.respond",
            respond(&sid, &r1, "c1", choose("Yes")),
        )
        .await;
    assert_eq!(accepted["result"], json!({}), "{accepted}");
    assert_ok(&a.rpc("session.idle", idle.clone()).await);

    submit(&mut a, &sid, &ask("Delete production data?")).await;
    let second = step(&mut a, "tool-call").await;
    assert_eq!(second["event"]["data"]["calls"][0]["id"], "c1");
    let r2 = request(&second);
    assert_ne!(r1, r2, "each model request has its own id");
    waiting(&mut a, &sid, &r2, "c1").await;
    // Client B still holds turn 1's call and answers or declines it late.
    for late in [
        respond(&sid, &r1, "c1", choose("Yes")),
        json!({"sessionId":sid,"branchId":"main","requestId":r1,"callId":"c1","declined":true}),
    ] {
        let reply = b.rpc("session.question.respond", late).await;
        assert_eq!(app_error(&reply), "question_not_pending", "{reply}");
    }
    let accepted = a
        .rpc(
            "session.question.respond",
            respond(&sid, &r2, "c1", choose("No")),
        )
        .await;
    assert_eq!(accepted["result"], json!({}), "{accepted}");
    let settled = loop {
        let settled = step(&mut a, "tool-result").await;
        if settled["event"]["data"]["request_id"] == r2.as_str() {
            break settled;
        }
    };
    let expected = "User answered:\n1. Delete production data?\n   -> No";
    assert_eq!(
        settled["event"]["data"]["results"][0]["result"]["content"],
        expected
    );
    assert_ok(&a.rpc("session.idle", idle).await);
    // The model reads it as turn 2's `c1` result, after turn 1's.
    let request = model.0.lock().unwrap().last().unwrap().clone();
    let read: Vec<_> = request
        .messages
        .iter()
        .flat_map(|message| match &message.content {
            MessageContent::Parts(parts) => parts
                .iter()
                .filter_map(|part| match part {
                    MessagePart::ToolResult {
                        tool_use_id,
                        content,
                        ..
                    } if tool_use_id == "c1" => Some(content.clone()),
                    _ => None,
                })
                .collect(),
            MessageContent::Text(_) => Vec::new(),
        })
        .collect();
    assert_eq!(
        read,
        [
            ToolContent::Text("User answered:\n1. Deploy staging?\n   -> Yes".into()),
            ToolContent::Text(expected.into()),
        ]
    );
    server.shutdown();
    server.join().await.unwrap();
}

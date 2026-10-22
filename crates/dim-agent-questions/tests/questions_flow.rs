//! Questions through the Agent runtime (design Appendix D): a scripted model
//! calls `ask_user`, a client answers through `sessionMethods`
//! `session.question` and the answer is the call's tool result, committed by
//! ToolExecutor. Interrupt, reload, close and branches settle through the
//! existing tool-call lifecycle.
use dim_agent_capabilities::{
    adapters::{DefaultPrepareModelPlugin, MockTools},
    defaults,
    history::HistoryRenderer,
    host::*,
    methods::{RpcError, SessionMethods},
    tool_registry::ToolContribution,
};
use dim_agent_runtime::*;
use dim_agent_types::*;
use rutis::{BoxFuture, CordisError, Ctx, Effect, TypedPlugin};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    result::Result,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio_util::sync::CancellationToken;

const CANCELLED: &str = "The question was cancelled before the user answered.";
const RELOADED: &str = "Tool execution cancelled: capability reloading";

/// Replies with queued tool batches, then with text; keeps every request.
#[derive(Default)]
struct Script {
    batches: Mutex<VecDeque<Vec<ToolCall>>>,
    requests: Mutex<Vec<Arc<ModelRequest>>>,
}
impl Script {
    fn push(&self, calls: &[(&str, &str, Value)]) {
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
    fn last_request(&self) -> Arc<ModelRequest> {
        self.requests.lock().unwrap().last().unwrap().clone()
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

/// Reaches the `ask_user` contribution of the latest Session, to call the
/// tool contract directly where no model output can (a duplicate call id).
#[derive(Clone, Default)]
struct Probe(Arc<Mutex<Option<(Ctx, SessionScope)>>>);
impl Probe {
    fn tool(&self) -> Arc<ToolContribution> {
        let (ctx, scope) = self.0.lock().unwrap().clone().unwrap();
        ctx.get_as::<ToolContribution>(scope.named_key::<ToolContribution>("ask_user"))
            .unwrap()
    }
}
struct ProbePlugin(SessionScope, Probe);
impl TypedPlugin for ProbePlugin {
    type Deps = ();
    fn name(&self) -> &str {
        "test.questions-probe"
    }
    fn apply<'a>(
        &'a self,
        ctx: &'a Ctx,
        (): Self::Deps,
    ) -> BoxFuture<'a, Result<Effect, CordisError>> {
        *self.1.0.lock().unwrap() = Some((ctx.clone(), self.0));
        Box::pin(async { Ok(Effect::Done) })
    }
}
impl Capability for ProbePlugin {
    fn keys(&self) -> KeysOf<Self> {}
}

struct Fixture {
    root: Ctx,
    agent: Agent,
    model: Arc<Script>,
    probe: Probe,
}
impl Fixture {
    fn new() -> Self {
        let root = Ctx::root_with(tokio::runtime::Handle::current());
        let model = Arc::new(Script::default());
        let probe = Probe::default();
        let mut builder = AgentBuilder::new(AgentConfig::default());
        builder
            .timeline_store(Arc::new(MemoryTimelineStore::default()))
            .session_value(Arc::new(HistoryRenderer::default()))
            .session_value(Arc::new(ModelHost(model.clone())))
            .session_value(Arc::new(PrepareContextHost(Arc::new(IdentityContext))))
            .session_value(Arc::new(ContextModelHost(None)))
            .session_value(Arc::new(CompactionStrategyHost(None)))
            .session_plugin(DefaultPrepareModelPlugin::new);
        defaults::install(&mut builder);
        // The host's `wait` holds a batch open next to `ask_user`.
        defaults::tool_host(&mut builder, Arc::new(MockTools));
        dim_agent_questions::install(&mut builder);
        let slot = probe.clone();
        builder.session_plugin(move |scope| ProbePlugin(*scope, slot.clone()));
        let agent = builder.build_in(&root);
        Self {
            root,
            agent,
            model,
            probe,
        }
    }
    async fn session(&self, interactive: bool) -> Session {
        self.agent
            .create_session(SessionConfig {
                tools: vec!["ask_user".into(), "wait".into()],
                interactive,
                ..SessionConfig::default()
            })
            .await
            .unwrap()
    }
    /// Restarts the named plugin alone.
    async fn restart(&self, diagnostics: rutis::RuntimeDiagnostics, name: &str) {
        let id = diagnostics
            .plugins
            .iter()
            .find(|plugin| plugin.name == name)
            .unwrap()
            .id;
        self.root.view(id).unwrap().restart().await.unwrap();
    }
    async fn shutdown(self) {
        self.agent.close().await.unwrap();
        self.root.shutdown().await.unwrap();
    }
}

fn database() -> Value {
    json!({"header":"Database","question":"Which database should the service use?",
        "options":[{"label":"PostgreSQL","description":"managed, relational"},{"label":"SQLite","description":"single file"}]})
}
fn features() -> Value {
    json!({"question":"Which features ship first?","multiSelect":true,
        "options":[{"label":"Auth"},{"label":"Billing"},{"label":"Search"}]})
}
fn today() -> Value {
    json!({"id":"ship","question":"Ship today?","allowCustom":false,
        "options":[{"label":"Yes"},{"label":"No"}]})
}

/// Calls `session.question.respond` as Rpc does, through the Session.
async fn respond(session: &Session, params: Value) -> Result<Value, RpcError> {
    let methods = session
        .session_named_service::<SessionMethods>("session.question")
        .unwrap();
    let accepted = methods.accept(
        "session.question.respond".into(),
        params,
        CancellationToken::new(),
        |_| {},
    )?;
    accepted.reply.await.unwrap()
}
/// A call as a client names it: its branch, the `request_id` of its
/// `step/tool-call` and its call id.
type Asked = (String, String, String);
fn params(session: &Session, (branch, request, call): &Asked, answers: Value) -> Value {
    json!({"sessionId":session.id(),"branchId":branch,"requestId":request,"callId":call,"answers":answers})
}
fn declined(session: &Session, (branch, request, call): &Asked) -> Value {
    json!({"sessionId":session.id(),"branchId":branch,"requestId":request,"callId":call,"declined":true})
}
fn code(error: &RpcError) -> &str {
    &error.data.as_ref().unwrap().app_error
}

/// Whether `asked` waits: an empty answer list is refused as invalid only
/// while it does, and as not pending otherwise.
async fn waits(session: &Session, asked: &Asked) -> bool {
    match respond(session, params(session, asked, json!([]))).await {
        Err(error) if code(&error) == "invalid_params" => true,
        Err(error) if code(&error) == "question_not_pending" => false,
        other => panic!("{asked:?}: {other:?}"),
    }
}
/// Names the latest call `call` of `branch` as a client does, from the
/// `step/tool-call` that holds it.
fn named(events: &[Arc<AgentEvent>], branch: &str, call: &str) -> Option<Asked> {
    events.iter().rev().find_map(|event| match &event.kind {
        EventKind::ToolCall(data) if data.calls.iter().any(|c| c.id == call) => {
            Some((branch.to_owned(), data.request_id.clone(), call.to_owned()))
        }
        _ => None,
    })
}
/// Waits until the latest call `call` of `branch` waits, and names it.
async fn waiting(session: &Session, branch: &str, call: &str) -> Asked {
    for _ in 0..2000 {
        let events = session.local(branch, ..).await.unwrap();
        if let Some(asked) = named(&events, branch, call)
            && waits(session, &asked).await
        {
            return asked;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("{call} never waited");
}

fn result(events: &[Arc<AgentEvent>], call: &str) -> ToolResult {
    events
        .iter()
        .rev()
        .find_map(|event| match &event.kind {
            EventKind::ToolResult(data) => data
                .results
                .iter()
                .find(|result| result.call_id == call)
                .map(|result| result.result.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no result for {call}"))
}
fn text(result: &ToolResult) -> &str {
    match &result.content {
        ToolContent::Text(text) => text,
        ToolContent::Blocks(_) => panic!("ask_user returns text"),
    }
}
fn last_turn(events: &[Arc<AgentEvent>]) -> TurnOutcome {
    events
        .iter()
        .rev()
        .find_map(|event| match &event.kind {
            EventKind::TurnEnd(data) => Some(data.outcome.clone()),
            _ => None,
        })
        .unwrap()
}
async fn turn(session: &Session, branch: &str) -> Vec<Arc<AgentEvent>> {
    tokio::time::timeout(Duration::from_secs(10), session.idle(branch))
        .await
        .unwrap()
        .unwrap();
    session.local(branch, ..).await.unwrap()
}

/// Acceptance 1, 4 and 8: answers and a decline become the result the model
/// reads; rejected answers keep the call waiting; the first of two answers
/// wins; the Timeline holds no Questions event.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn answers_and_declines_become_the_results_the_model_reads() {
    let fixture = Fixture::new();
    let session = fixture.session(true).await;
    fixture.model.push(&[(
        "ask_user",
        "c1",
        json!({"questions":[database(), features(), today()]}),
    )]);
    session
        .submit("plan".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    let c1 = waiting(&session, "main", "c1").await;
    let valid = json!([
        {"questionId":"ship","selected":["Yes"]},
        {"questionId":"1","selected":["PostgreSQL"]},
        {"questionId":"2","selected":["Auth","Search"],"custom":"audit log"}
    ]);
    for (answers, reason) in [
        (json!([{"questionId":"1","selected":["MySQL"]}]), "MySQL"),
        (
            json!([{"questionId":"1","selected":["PostgreSQL","SQLite"]},{"questionId":"2","selected":["Auth"]},{"questionId":"ship","selected":["No"]}]),
            "question 1 takes a single answer",
        ),
        (
            json!([{"questionId":"1","selected":["SQLite"]},{"questionId":"2","selected":["Auth"]},{"questionId":"ship","selected":[],"custom":"Friday"}]),
            "question ship does not accept a custom answer",
        ),
        (
            json!([{"questionId":"1","selected":[]},{"questionId":"2","selected":["Auth"]},{"questionId":"ship","selected":["No"]}]),
            "question 1 has no answer",
        ),
        (
            json!([{"questionId":"1","selected":["SQLite"]},{"questionId":"ship","selected":["No"]}]),
            "question 2 needs exactly one answer",
        ),
        (
            json!([{"questionId":"9","selected":["SQLite"]}]),
            "unknown question id",
        ),
        (
            json!([{"questionId":"1","selected":["SQLite"]},{"questionId":"1","selected":["PostgreSQL"]},{"questionId":"2","selected":["Auth"]},{"questionId":"ship","selected":["No"]}]),
            "question 1 needs exactly one answer",
        ),
        (
            json!([{"questionId":"1","selected":["SQLite"]},{"questionId":"2","selected":["Auth","Auth"]},{"questionId":"ship","selected":["No"]}]),
            "\"Auth\" is not an option of question 2 or is selected twice",
        ),
        (
            json!([{"questionId":"1","selected":["SQLite"]},{"questionId":"2","selected":[],"custom":" "},{"questionId":"ship","selected":["No"]}]),
            "the custom answer of question 2 is empty",
        ),
    ] {
        let error = respond(&session, params(&session, &c1, answers))
            .await
            .unwrap_err();
        assert_eq!(code(&error), "invalid_params", "{reason}");
        assert!(error.message.contains(reason), "{}", error.message);
    }
    let mut both = params(&session, &c1, valid.clone());
    both["declined"] = json!(true);
    let mut neither = both.clone();
    neither.as_object_mut().unwrap().remove("answers");
    neither.as_object_mut().unwrap().remove("declined");
    // The request is part of the call's name.
    let mut unnamed = params(&session, &c1, valid.clone());
    unnamed.as_object_mut().unwrap().remove("requestId");
    for wrong in [both, neither, unnamed] {
        assert_eq!(
            code(&respond(&session, wrong).await.unwrap_err()),
            "invalid_params"
        );
    }
    // Two clients answer: exactly one is accepted.
    let (first, second) = tokio::join!(
        respond(&session, params(&session, &c1, valid.clone())),
        respond(&session, params(&session, &c1, valid.clone())),
    );
    let mut replies = [first, second];
    replies.sort_by_key(Result::is_err);
    assert_eq!(replies[0].as_ref().unwrap(), &json!({}));
    assert_eq!(
        code(replies[1].as_ref().unwrap_err()),
        "question_not_pending"
    );
    let events = turn(&session, "main").await;
    let answered = result(&events, "c1");
    let expected = "User answered:\n1. Which database should the service use?\n   -> PostgreSQL\n2. Which features ship first?\n   -> Auth, Search, (custom) audit log\n3. Ship today?\n   -> Yes";
    assert_eq!(text(&answered), expected);
    assert_eq!(answered.is_error, None);
    assert_eq!(
        Value::Object(answered.structured_content.clone().unwrap()),
        json!({"declined":false,"answers":[
            {"questionId":"1","selected":["PostgreSQL"]},
            {"questionId":"2","selected":["Auth","Search"],"custom":"audit log"},
            {"questionId":"ship","selected":["Yes"]}]})
    );
    // The model reads the answer only as the call's tool result.
    let request = fixture.model.last_request();
    let read = request
        .messages
        .iter()
        .find_map(|message| match &message.content {
            MessageContent::Parts(parts) => parts.iter().find_map(|part| match part {
                MessagePart::ToolResult {
                    tool_use_id,
                    content,
                    ..
                } if tool_use_id == "c1" => Some(content.clone()),
                _ => None,
            }),
            MessageContent::Text(_) => None,
        });
    assert_eq!(read, Some(ToolContent::Text(expected.into())));
    let declared = request
        .tools
        .as_ref()
        .unwrap()
        .iter()
        .find(|tool| tool.name == "ask_user")
        .unwrap();
    assert_eq!(declared.risk.as_deref(), Some("session"));

    fixture
        .model
        .push(&[("ask_user", "c2", json!({"questions":[today()]}))]);
    session
        .submit("again".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    let c2 = waiting(&session, "main", "c2").await;
    assert_eq!(
        respond(&session, declined(&session, &c2)).await.unwrap(),
        json!({})
    );
    let events = turn(&session, "main").await;
    let declined = result(&events, "c2");
    assert_eq!(
        text(&declined),
        "User declined to answer. Proceed without these answers."
    );
    assert_eq!(declined.is_error, None);
    assert_eq!(
        Value::Object(declined.structured_content.unwrap()),
        json!({"answers":[],"declined":true})
    );
    assert!(matches!(last_turn(&events), TurnOutcome::Completed { .. }));
    assert!(
        events
            .iter()
            .all(|event| !matches!(event.kind, EventKind::Extension(_))),
        "Questions writes no events"
    );
    fixture.shutdown().await;
}

/// Acceptance 2 and Appendix C: invalid input is an error result that
/// registers nothing; a non-interactive Session declares the same tools and
/// answers at once; a second waiting call with the same id is refused.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn invalid_input_non_interactive_sessions_and_duplicate_ids_never_wait() {
    let fixture = Fixture::new();
    let session = fixture.session(true).await;
    let two = |options: Value| json!({"questions":[{"question":"Pick","options":options}]});
    fixture.model.push(&[
        ("ask_user", "none", json!({"questions":[]})),
        (
            "ask_user",
            "five",
            json!({"questions":[today(), database(), features(), {"question":"a","options":[{"label":"x"},{"label":"y"}]}, {"question":"b","options":[{"label":"x"},{"label":"y"}]}]}),
        ),
        ("ask_user", "single", two(json!([{"label":"only"}]))),
        ("ask_user", "same", two(json!([{"label":"a"},{"label":"a"}]))),
        (
            "ask_user",
            "header",
            json!({"questions":[{"header":"h".repeat(41),"question":"Pick","options":[{"label":"a"},{"label":"b"}]}]}),
        ),
        (
            "ask_user",
            "ids",
            json!({"questions":[{"id":"2","question":"a","options":[{"label":"x"},{"label":"y"}]},{"question":"b","options":[{"label":"x"},{"label":"y"}]}]}),
        ),
        (
            "ask_user",
            "unknown",
            json!({"questions":[database()],"timeoutMs":5}),
        ),
        (
            "ask_user",
            "blank",
            json!({"questions":[{"question":" ","options":[{"label":"a"},{"label":"b"}]}]}),
        ),
        ("ask_user", "empty", two(json!([{"label":"a"},{"label":" "}]))),
    ]);
    session
        .submit("bad".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    let events = turn(&session, "main").await;
    for (call, reason) in [
        ("none", "questions must hold 1 to 4 questions"),
        ("five", "questions must hold 1 to 4 questions"),
        ("single", "question 1 must offer 2 to 6 options"),
        ("same", "must be non-empty and unique"),
        ("header", "exceeds 40 characters"),
        ("ids", "question id \"2\" is empty or not unique"),
        ("unknown", "unknown field `timeoutMs`"),
        ("blank", "question 1 has no text"),
        ("empty", "must be non-empty and unique"),
    ] {
        let rejected = result(&events, call);
        assert_eq!(rejected.is_error, Some(true), "{call}");
        assert!(
            text(&rejected).starts_with("Invalid ask_user input: ")
                && text(&rejected).contains(reason),
            "{call}: {}",
            text(&rejected)
        );
        let asked = named(&events, "main", call).unwrap();
        assert_eq!(
            code(
                &respond(&session, params(&session, &asked, json!([])))
                    .await
                    .unwrap_err()
            ),
            "question_not_pending"
        );
    }
    let interactive_tools = fixture.model.last_request().tools.clone();

    // Call ids are unique only by convention: a second call with the key of a
    // waiting one is refused through the tool contract and never answered.
    let tool = fixture.probe.tool();
    let call = ToolCall {
        id: "dup".into(),
        name: "ask_user".into(),
        input: json!({"questions":[today()]}).as_object().unwrap().clone(),
    };
    let context = ToolExecutionContext {
        turn_id: "t".into(),
        request_id: "r".into(),
        branch_id: "main".into(),
    };
    let dup: Asked = ("main".into(), "r".into(), "dup".into());
    let (handler, first_call, first_context) =
        (tool.handler.clone(), call.clone(), context.clone());
    let first = tokio::spawn(async move {
        handler
            .execute(first_call, first_context, CancellationToken::new())
            .await
    });
    let mut registered = false;
    for _ in 0..2000 {
        registered = waits(&session, &dup).await;
        if registered {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(registered, "dup never waited");
    let duplicate = tokio::time::timeout(
        Duration::from_secs(10),
        tool.handler
            .execute(call.clone(), context.clone(), CancellationToken::new()),
    )
    .await
    .expect("a duplicate never waits")
    .unwrap();
    assert_eq!(duplicate.is_error, Some(true));
    assert_eq!(
        text(&duplicate),
        "Invalid ask_user input: duplicate call id dup"
    );
    // A token that fired before the handler started ends the call before it
    // looks at the table, and the waiting call is untouched.
    let fired = CancellationToken::new();
    fired.cancel();
    match tool.handler.execute(call, context, fired).await {
        Err(HostError::Cancelled(reason)) => assert_eq!(reason, CANCELLED),
        other => panic!("{other:?}"),
    }
    assert!(waits(&session, &dup).await);
    let answer = json!([{"questionId":"ship","selected":["No"]}]);
    respond(&session, params(&session, &dup, answer))
        .await
        .unwrap();
    let answered = tokio::time::timeout(Duration::from_secs(10), first)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(text(&answered), "User answered:\n1. Ship today?\n   -> No");

    let quiet = fixture.session(false).await;
    fixture
        .model
        .push(&[("ask_user", "c1", json!({"questions":[database()]}))]);
    quiet
        .submit("ask".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    let events = turn(&quiet, "main").await;
    let refused = result(&events, "c1");
    assert_eq!(refused.is_error, Some(true));
    assert_eq!(
        text(&refused),
        "No one is available to answer questions in this session. Proceed without asking."
    );
    assert_eq!(fixture.model.last_request().tools, interactive_tools);
    fixture.shutdown().await;
}

/// Acceptance 3 and 6, §4.2: a stop cancels a waiting call and a later answer
/// finds nothing; an answer given before the stop is kept; a Questions or
/// ToolExecutor reload reload-settles the batch and a Session close
/// interrupts it, both through ToolExecutor, and nothing is asked again. After
/// a Questions reload the next request asks through the new generation.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn interrupt_reload_and_close_settle_a_waiting_call() {
    let fixture = Fixture::new();
    let session = fixture.session(true).await;
    let ask = || json!({"questions":[today()]});
    let yes = || json!([{"questionId":"ship","selected":["Yes"]}]);

    // Stop, then answer.
    fixture.model.push(&[("ask_user", "c1", ask())]);
    session
        .submit("stop".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    let c1 = waiting(&session, "main", "c1").await;
    session.stop("main", "user stopped".into()).await.unwrap();
    let events = turn(&session, "main").await;
    let cancelled = result(&events, "c1");
    assert_eq!(cancelled.is_error, Some(true));
    assert_eq!(text(&cancelled), CANCELLED);
    assert!(matches!(last_turn(&events), TurnOutcome::Cancelled { .. }));
    assert_eq!(
        code(
            &respond(&session, params(&session, &c1, yes()))
                .await
                .unwrap_err()
        ),
        "question_not_pending"
    );

    // Answer, then stop while a slow call holds the batch open.
    fixture.model.push(&[
        ("ask_user", "c2", ask()),
        ("wait", "slow", json!({"millis":60000})),
    ]);
    session
        .submit("answer".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    let c2 = waiting(&session, "main", "c2").await;
    respond(&session, params(&session, &c2, yes()))
        .await
        .unwrap();
    session.stop("main", "user stopped".into()).await.unwrap();
    let events = turn(&session, "main").await;
    let kept = result(&events, "c2");
    assert_eq!(text(&kept), "User answered:\n1. Ship today?\n   -> Yes");
    assert_eq!(kept.is_error, None);
    assert_eq!(result(&events, "slow").is_error, Some(true));
    assert!(matches!(last_turn(&events), TurnOutcome::Cancelled { .. }));

    // The Questions part reloads: its waiters end as reloading.
    fixture.model.push(&[("ask_user", "c3", ask())]);
    session
        .submit("reload".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    let c3 = waiting(&session, "main", "c3").await;
    let unloaded = fixture.probe.tool();
    fixture
        .restart(session.diagnostics().unwrap(), "Questions")
        .await;
    let events = turn(&session, "main").await;
    assert_eq!(text(&result(&events, "c3")), RELOADED);
    assert!(matches!(last_turn(&events), TurnOutcome::Failed { .. }));
    assert_eq!(
        code(
            &respond(&session, params(&session, &c3, yes()))
                .await
                .unwrap_err()
        ),
        "question_not_pending"
    );
    // A call that still reaches the unloaded generation, from a request built
    // before the reload, finds its table closed instead of waiting on nothing.
    let late = ToolCall {
        id: "late".into(),
        name: "ask_user".into(),
        input: ask().as_object().unwrap().clone(),
    };
    let context = ToolExecutionContext {
        turn_id: "t".into(),
        request_id: "r".into(),
        branch_id: "main".into(),
    };
    let closed = tokio::time::timeout(
        Duration::from_secs(10),
        unloaded
            .handler
            .execute(late, context, CancellationToken::new()),
    )
    .await
    .expect("the unloaded generation never waits");
    match closed {
        Err(HostError::Cancelled(reason)) => assert_eq!(reason, CAPABILITY_RELOADING),
        other => panic!("{other:?}"),
    }
    // ToolRegistry follows the new generation (#241): the next request asks
    // through it, and the call waits for its own answer.
    fixture.model.push(&[("ask_user", "c3b", ask())]);
    session
        .submit("after reload".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    let c3b = waiting(&session, "main", "c3b").await;
    respond(&session, params(&session, &c3b, yes()))
        .await
        .unwrap();
    let events = turn(&session, "main").await;
    assert_eq!(
        text(&result(&events, "c3b")),
        "User answered:\n1. Ship today?\n   -> Yes"
    );
    assert!(matches!(last_turn(&events), TurnOutcome::Completed { .. }));

    // ToolExecutor reloads: the batch token ends the wait.
    let session = fixture.session(true).await;
    fixture.model.push(&[("ask_user", "c4", ask())]);
    session
        .submit("executor".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    let c4 = waiting(&session, "main", "c4").await;
    fixture
        .restart(session.branch_diagnostics("main").unwrap(), "ToolExecutor")
        .await;
    let events = turn(&session, "main").await;
    assert_eq!(text(&result(&events, "c4")), RELOADED);
    assert!(matches!(last_turn(&events), TurnOutcome::Failed { .. }));
    assert_eq!(
        code(
            &respond(&session, params(&session, &c4, yes()))
                .await
                .unwrap_err()
        ),
        "question_not_pending"
    );

    // `session.close` is a business close: it interrupts the branch, so the
    // waiting call ends as interrupted, and a restore asks nothing again.
    let session = fixture.session(true).await;
    fixture.model.push(&[("ask_user", "c5", ask())]);
    session
        .submit("close".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    waiting(&session, "main", "c5").await;
    tokio::time::timeout(Duration::from_secs(10), session.close())
        .await
        .unwrap()
        .unwrap();
    let restored = fixture
        .agent
        .restore_session(session.id().to_owned())
        .await
        .unwrap();
    let events = turn(&restored, "main").await;
    let settled = result(&events, "c5");
    assert_eq!(settled.is_error, Some(true));
    assert_eq!(text(&settled), CANCELLED);
    assert!(matches!(last_turn(&events), TurnOutcome::Cancelled { .. }));
    let calls = events
        .iter()
        .filter(|event| matches!(&event.kind, EventKind::ToolCall(data) if data.calls.iter().any(|call| call.id == "c5")))
        .count();
    assert_eq!(calls, 1, "a settled question is never asked again");
    fixture.shutdown().await;
}

/// §4.3: each branch's questions are answered with that branch's id, and the
/// same call id on two branches names two independent waiting calls.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn branches_answer_their_own_questions_by_branch_id() {
    let fixture = Fixture::new();
    let session = fixture.session(true).await;
    session
        .submit("start".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    turn(&session, "main").await;
    let at = session.sequence("main").unwrap();
    fixture
        .model
        .push(&[("ask_user", "c1", json!({"questions":[database()]}))]);
    session
        .submit("main asks".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    let main_c1 = waiting(&session, "main", "c1").await;
    fixture
        .model
        .push(&[("ask_user", "c1", json!({"questions":[today()]}))]);
    let child = session
        .branch(
            "main".into(),
            at,
            BranchOptions {
                prompt: "child asks".into(),
                source: BranchSource::Host {
                    operation_id: "questions-test".into(),
                },
            },
        )
        .await
        .unwrap();
    let child_c1 = waiting(&session, &child, "c1").await;
    respond(
        &session,
        params(
            &session,
            &child_c1,
            json!([{"questionId":"ship","selected":["No"]}]),
        ),
    )
    .await
    .unwrap();
    // main's call with the same id still waits for its own questions.
    assert!(waits(&session, &main_c1).await);
    respond(
        &session,
        params(
            &session,
            &main_c1,
            json!([{"questionId":"1","selected":["SQLite"]}]),
        ),
    )
    .await
    .unwrap();
    let child_events = turn(&session, &child).await;
    assert_eq!(
        text(&result(&child_events, "c1")),
        "User answered:\n1. Ship today?\n   -> No"
    );
    let main_events = turn(&session, "main").await;
    assert_eq!(
        text(&result(&main_events, "c1")),
        "User answered:\n1. Which database should the service use?\n   -> SQLite"
    );
    fixture.shutdown().await;
}

/// §4.3 and acceptance 10: call ids may repeat across model requests. Client
/// A answers turn 1's `c1`; turn 2 asks another question, also as `c1`.
/// Client B's late answer and late decline to turn 1's call find nothing and
/// leave turn 2's call waiting; its own answer is the result the model reads.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_late_answer_never_reaches_a_later_call_that_reuses_its_id() {
    let fixture = Fixture::new();
    let session = fixture.session(true).await;
    let ask = |question: &str| {
        json!({"questions":[{"id":"decision","question":question,"allowCustom":false,
            "options":[{"label":"Yes"},{"label":"No"}]}]})
    };
    let choose = |label: &str| json!([{"questionId":"decision","selected":[label]}]);
    fixture
        .model
        .push(&[("ask_user", "c1", ask("Deploy staging?"))]);
    session
        .submit("deploy".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    let first = waiting(&session, "main", "c1").await;
    // Client A answers turn 1.
    assert_eq!(
        respond(&session, params(&session, &first, choose("Yes")))
            .await
            .unwrap(),
        json!({})
    );
    turn(&session, "main").await;

    fixture
        .model
        .push(&[("ask_user", "c1", ask("Delete production data?"))]);
    session
        .submit("clean up".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    let second = waiting(&session, "main", "c1").await;
    assert_ne!(first.1, second.1, "the later call has its own request");
    // Client B answers and declines turn 1's call late.
    for late in [
        params(&session, &first, choose("Yes")),
        declined(&session, &first),
    ] {
        assert_eq!(
            code(&respond(&session, late).await.unwrap_err()),
            "question_not_pending"
        );
    }
    assert!(waits(&session, &second).await, "the later call still waits");
    assert_eq!(
        respond(&session, params(&session, &second, choose("No")))
            .await
            .unwrap(),
        json!({})
    );
    let events = turn(&session, "main").await;
    let expected = "User answered:\n1. Delete production data?\n   -> No";
    assert_eq!(text(&result(&events, "c1")), expected);
    // The model reads it as turn 2's `c1` result, after turn 1's.
    let read: Vec<_> = fixture
        .model
        .last_request()
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
    fixture.shutdown().await;
}

//! Flows for the deferred completion request: a worker `completed` that
//! arrives while a driving input is pending is kept, and evaluated at the turn
//! end that consumes the last of them (Pursuit §4.1, §5, acceptance 11).
use crate::support;
use dim_agent_runtime::SessionLines;

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use dim_agent_capabilities::adapters::EchoModel;
use dim_agent_capabilities::host::{
    HostFuture, ModelChunk, ModelConnection, ModelDriver, ModelStopReason, ModelStream,
};
use dim_agent_types::{
    AgentEvent, BranchCreateData, BranchSource, EventKind, ExecEndedData, ExecStartedData,
    ExecStatus, InputMode, Message, MessageContent, ModelRequest, Role, ToolResult,
};
use serde_json::json;
use support::*;
use tokio_util::sync::CancellationToken;

const WAIT: Duration = Duration::from_secs(20);
const NEGATIVE: Duration = Duration::from_millis(750);

fn is_evaluator(request: &ModelRequest) -> bool {
    request.messages.iter().any(|message| {
        matches!(&message.content, MessageContent::Text(text)
            if text.contains("<dim-goal kind=\"evaluate\""))
    })
}

/// Whether a user message is one of Goal's append-mode inputs rather than a
/// conversational input: the state records (`kind="state"`), the evaluator's
/// conclusion (`kind="report"`), the retirement blocks that end a legacy
/// status key (`kind="status"`, text.rs writes them only into the append
/// batches of the state inputs) and the triggers `goal_update` and an edit,
/// pause or clear write (`kind="goal"` with an op other than `set` or
/// `resume`, which drive a turn). These blocks ride along with a tool result
/// they were committed ahead of, so the message they leave at the tail of a
/// request says nothing about what drives the turn. Driving blocks keep their
/// meaning: `op="set"` and `op="resume"`, continuations, evaluations.
fn goal_input_is_append(text: &str) -> bool {
    if !text.starts_with("<dim-goal ") || !text.ends_with("</dim-goal>") {
        return false;
    }
    let head = text.lines().next().unwrap_or_default();
    if ["kind=\"state\"", "kind=\"report\"", "kind=\"status\""]
        .iter()
        .any(|kind| head.contains(kind))
    {
        return true;
    }
    head.contains("kind=\"goal\"")
        && [
            "op=\"completed\"",
            "op=\"blocked\"",
            "op=\"edit\"",
            "op=\"pause\"",
            "op=\"clear\"",
        ]
        .iter()
        .any(|op| head.contains(op))
}

/// The latest message that drives the script: `EchoModel::last_message`'s
/// selection, with Goal's append-mode inputs skipped the same way runtime
/// state and rules are. `matches` and `last_user_text` share this selection,
/// so a held turn is judged on the same tail the script text is read from.
fn select_last(request: &ModelRequest) -> Option<&Message> {
    request
        .messages
        .iter()
        .rev()
        .map(AsRef::as_ref)
        .find(|message: &&Message| {
            message.role != Role::User
                || !match &message.content {
                    MessageContent::Text(text) => {
                        dim_agent_capabilities::context_block::is_state_or_rule(text)
                            || goal_input_is_append(text)
                    }
                    MessageContent::Parts(parts) => matches!(
                        parts.as_slice(),
                        [dim_agent_types::MessagePart::Text { text, .. }]
                            if dim_agent_capabilities::context_block::is_state_or_rule(text)
                                || goal_input_is_append(text)
                    ),
                }
        })
}

fn last_user_text(request: &ModelRequest) -> Option<&str> {
    let message = select_last(request)?;
    if message.role != Role::User {
        return None;
    }
    match &message.content {
        MessageContent::Text(text) => Some(text),
        MessageContent::Parts(parts) => parts.iter().find_map(|part| match part {
            dim_agent_types::MessagePart::Text { text, .. } => Some(text.as_str()),
            _ => None,
        }),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Halt {
    /// The report turn's model call: the `goal_update` tool has not run yet.
    Report,
    /// The report turn's model call after the tool result: it has run.
    AfterTool,
    /// A turn whose last user message is exactly this text.
    Text(String),
}

#[derive(Default)]
struct ScriptedModel {
    calls: AtomicUsize,
    halts: Mutex<VecDeque<Halt>>,
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
}

impl ScriptedModel {
    fn halt(&self, halt: Halt) {
        self.halts.lock().unwrap().push_back(halt);
    }
    async fn entered(&self) {
        tokio::time::timeout(WAIT, self.entered.notified())
            .await
            .expect("the held model call");
    }
    fn release(&self) {
        self.release.notify_one();
    }
    fn matches(request: &ModelRequest) -> Option<Halt> {
        let message = select_last(request)?;
        match message.role {
            Role::ToolResult => Some(Halt::AfterTool),
            Role::User => {
                let text = last_user_text(request)?;
                if text.starts_with("tool:goal_update") {
                    Some(Halt::Report)
                } else {
                    Some(Halt::Text(text.to_owned()))
                }
            }
            Role::Assistant => None,
        }
    }
    fn evaluation(index: usize) -> ModelStream {
        Box::pin(futures_util::stream::iter(vec![
            Ok(ModelChunk::MessageStart {
                message_id: format!("evaluation-{index}"),
            }),
            Ok(ModelChunk::Text(
                "Read the tests and ran them.\nDONE: all tests pass".into(),
            )),
            Ok(ModelChunk::TextEnd),
            Ok(ModelChunk::MessageStop {
                message_id: format!("evaluation-{index}"),
                reason: ModelStopReason::EndTurn,
            }),
        ]))
    }
}

impl ModelDriver for ScriptedModel {
    fn invoke(
        &self,
        request: Arc<ModelRequest>,
        connection: ModelConnection,
        cancel: CancellationToken,
    ) -> HostFuture<'_, ModelStream> {
        let index = self.calls.fetch_add(1, Ordering::SeqCst);
        let matched = Self::matches(&request);
        let held = matched.is_some_and(|kind| {
            let mut halts = self.halts.lock().unwrap();
            if halts.front() == Some(&kind) {
                halts.pop_front();
                true
            } else {
                false
            }
        });
        if !held {
            if is_evaluator(&request) {
                return Box::pin(async move { Ok(Self::evaluation(index)) });
            }
            return EchoModel.invoke(request, connection, cancel);
        }
        Box::pin(async move {
            self.entered.notify_one();
            self.release.notified().await;
            if is_evaluator(&request) {
                return Ok(Self::evaluation(index));
            }
            EchoModel.invoke(request, connection, cancel).await
        })
    }
}

struct Harness {
    fixture: Fixture,
    id: String,
    session: dim_agent_runtime::Session,
}

async fn evaluator_harness(model: Arc<ScriptedModel>) -> Harness {
    let mut fixture = Fixture::start_with(model).await;
    fixture.set_limit(5).await;
    let id = fixture.new_session(&["goal_update", "wait"]).await;
    let session = fixture.agent.get_session(&id).unwrap();
    fixture
        .goal_on(
            &id,
            "set",
            json!({"objective": "make the tests pass", "judge": "evaluator"}),
        )
        .await
        .unwrap();
    Harness {
        fixture,
        id,
        session,
    }
}

impl Harness {
    async fn submit(&self, text: &str, mode: InputMode) -> Arc<AgentEvent> {
        self.session
            .submit(text.to_owned(), mode, "main")
            .await
            .unwrap()
    }
    async fn report(&self) -> Arc<AgentEvent> {
        self.submit(
            &format!(
                "tool:goal_update {}",
                json!({"status": "completed", "summary": "all tests pass"})
            ),
            InputMode::Queue,
        )
        .await
    }
    async fn goal_view(&mut self) -> serde_json::Value {
        self.fixture
            .client
            .rpc("session.goal.get", json!({"sessionId": self.id}))
            .await
            .unwrap()
    }
    async fn create_for(&self, evaluation: u64) -> Vec<(u64, BranchCreateData)> {
        let operation = format!("goal:evaluate:{evaluation}");
        branch_creates(&self.session, "main")
            .await
            .into_iter()
            .filter(|(_, data)| match &data.source {
                BranchSource::Host { operation_id } => *operation_id == operation,
                _ => false,
            })
            .collect()
    }
    async fn evaluations(&self) -> Vec<(u64, BranchCreateData)> {
        branch_creates(&self.session, "main")
            .await
            .into_iter()
            .filter(|(_, data)| {
                matches!(&data.source, BranchSource::Host { operation_id } if operation_id.starts_with("goal:evaluate:"))
            })
            .collect()
    }
    async fn turn_ends(&self, after: u64) -> Vec<u64> {
        branch_events(&self.session, "main", ..)
            .await
            .unwrap()
            .iter()
            .filter(|event| event.sequence > after && matches!(event.kind, EventKind::TurnEnd(_)))
            .map(|event| event.sequence)
            .collect()
    }
    /// The turn containing `sequence`: its `turn/start` and `turn/end`.
    async fn turn_containing(&self, sequence: u64) -> Option<(u64, u64)> {
        let events = branch_events(&self.session, "main", ..).await.unwrap();
        let start = events
            .iter()
            .filter(|event| {
                event.sequence < sequence && matches!(event.kind, EventKind::TurnStart(_))
            })
            .max_by_key(|event| event.sequence)?
            .sequence;
        let end = events
            .iter()
            .filter(|event| event.sequence > start && matches!(event.kind, EventKind::TurnEnd(_)))
            .min_by_key(|event| event.sequence)?
            .sequence;
        Some((start, end))
    }
    async fn close(self) {
        self.fixture.close().await;
    }
}

/// ①: no driving input at the report turn end: the evaluator is created at
/// that turn end, exactly as before.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn beta_1_report_without_driving_input_creates_at_its_turn_end() {
    let model = Arc::new(ScriptedModel::default());
    model.halt(Halt::AfterTool);
    let harness = evaluator_harness(model.clone()).await;
    harness.report().await;
    model.entered().await;
    model.release();

    let request = wait_goal(&harness.session, "main", r#"kind="goal" op="completed""#).await;
    wait_event(&harness.session, "main", |kind| {
        matches!(kind, EventKind::BranchCreate(_))
    })
    .await;
    let (_, report_end) = harness
        .turn_containing(request.sequence)
        .await
        .unwrap_or_else(|| panic!("the turn holding the request {}", request.sequence));
    let creates = branch_creates(&harness.session, "main").await;
    assert_eq!(creates.len(), 1, "one evaluator: {creates:#?}");
    assert_eq!(
        creates[0].1.source,
        BranchSource::Host {
            operation_id: format!("goal:evaluate:{report_end}")
        },
        "the evaluator is created at the report turn end"
    );
    let after: Vec<_> = goal_writes(&harness.session, "main")
        .await
        .into_iter()
        .filter(|block| block.sequence > request.sequence)
        .filter(|block| block.marker.contains("kind=\"continuation\""))
        .collect();
    assert!(
        after.is_empty(),
        "no continuation after the request: {after:#?}"
    );
    harness.close().await;
}

async fn queued_report_variant(queue_before_goal_update: bool) {
    let model = Arc::new(ScriptedModel::default());
    model.halt(Halt::Report);
    if !queue_before_goal_update {
        model.halt(Halt::AfterTool);
    }
    let mut harness = evaluator_harness(model.clone()).await;
    harness.report().await;
    model.entered().await;
    let queue = if queue_before_goal_update {
        Some(harness.submit("keep going", InputMode::Queue).await)
    } else {
        None
    };
    model.release();
    let queue = match queue {
        Some(queue) => queue,
        None => {
            wait_goal(&harness.session, "main", r#"kind="goal" op="completed""#).await;
            harness.goal_view().await;
            model.entered().await;
            let queue = harness.submit("keep going", InputMode::Queue).await;
            model.release();
            queue
        }
    };

    let request = wait_goal(&harness.session, "main", r#"kind="goal" op="completed""#).await;
    wait_event(&harness.session, "main", |kind| {
        matches!(kind, EventKind::BranchCreate(_))
    })
    .await;
    let (_, report_end) = harness
        .turn_containing(request.sequence)
        .await
        .unwrap_or_else(|| panic!("the turn holding the request {}", request.sequence));
    let ends = harness.turn_ends(report_end).await;
    let queue_end = ends[0];
    assert!(
        queue_end > report_end,
        "the queued input drives the next turn"
    );
    assert!(
        harness.create_for(report_end).await.is_empty(),
        "the report turn end creates no evaluator"
    );
    let creates = harness.create_for(queue_end).await;
    assert_eq!(
        creates.len(),
        1,
        "the evaluator is created at the queue drain turn end: {creates:#?}"
    );
    assert_eq!(creates[0].1.prompt.matches("all tests pass").count(), 1);
    match queue_before_goal_update {
        true => assert!(
            queue.sequence < request.sequence,
            "the queue lands before the report's completed trigger"
        ),
        false => assert!(
            queue.sequence > request.sequence,
            "the queue lands after the report's completed trigger, so last_operator is newer"
        ),
    }
    harness.close().await;
}

/// ②: a Queue input during the report turn, before `goal_update`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn beta_2a_queue_before_goal_update_defers_the_evaluation() {
    queued_report_variant(true).await;
}

/// ②, gap 1: a Queue input after `goal_update` still defers it; a
/// `last_operator` floor would drop the request.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn beta_2b_queue_after_goal_update_keeps_the_request() {
    queued_report_variant(false).await;
}

/// ③: a Steer applied after the report turn end and before Goal's decision
/// reads. The turn/start fold is held open, so only `pending_driving` keeps
/// the evaluator from being created at the report turn end.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn beta_3_steer_at_the_end_defers_the_evaluation_by_one_turn() {
    let model = Arc::new(ScriptedModel::default());
    model.halt(Halt::AfterTool);
    let harness = evaluator_harness(model.clone()).await;
    harness.report().await;
    model.entered().await;
    let min = harness.session.sequence("main").unwrap();
    let mut decision = dim_goal::testing::hold_stage(
        &harness.id,
        "main",
        dim_goal::testing::Stage::Decision,
        min,
        None,
    );
    model.release();
    let report_end = decision.arrived().await;

    let mut turn_gate = dim_goal::testing::hold_turn_start(
        &harness.id,
        "main",
        harness.session.sequence("main").unwrap(),
    );
    let steer = harness.submit("check the logs", InputMode::Steer).await;
    let turn_start = turn_gate.arrived().await;
    let mut no_create = dim_goal::testing::hold_stage(
        &harness.id,
        "main",
        dim_goal::testing::Stage::Create,
        report_end,
        Some(report_end),
    );
    decision.release();
    let unexpected = tokio::time::timeout(NEGATIVE, no_create.arrived()).await;
    assert!(
        unexpected.is_err(),
        "the report turn end creates no evaluator while the steer is pending"
    );
    turn_gate.release();

    wait_event(&harness.session, "main", |kind| {
        matches!(kind, EventKind::BranchCreate(_))
    })
    .await;
    let ends = harness.turn_ends(report_end).await;
    let steer_end = *ends
        .iter()
        .find(|sequence| **sequence > report_end)
        .expect("the steer turn ends");
    assert!(
        steer_end > report_end,
        "report {report_end} steer {steer_end} turn/start {turn_start} input {}",
        steer.sequence
    );
    let creates = harness.create_for(steer_end).await;
    assert_eq!(creates.len(), 1, "one evaluator: {creates:#?}");
    assert!(harness.create_for(report_end).await.is_empty());
    harness.close().await;
}

/// ④: two queued inputs defer the evaluation to the turn end that drains the
/// queue.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn beta_4_two_queued_inputs_defer_to_the_drain_turn_end() {
    let model = Arc::new(ScriptedModel::default());
    model.halt(Halt::Report);
    model.halt(Halt::AfterTool);
    let mut harness = evaluator_harness(model.clone()).await;
    harness.report().await;
    model.entered().await;
    let first = harness.submit("first queued", InputMode::Queue).await;
    model.release();
    wait_goal(&harness.session, "main", r#"kind="goal" op="completed""#).await;
    harness.goal_view().await;
    model.entered().await;
    let second = harness.submit("second queued", InputMode::Queue).await;
    model.release();

    wait_event(&harness.session, "main", |kind| {
        matches!(kind, EventKind::BranchCreate(_))
    })
    .await;
    let request = wait_goal(&harness.session, "main", r#"kind="goal" op="completed""#).await;
    let (_, report_end) = harness
        .turn_containing(request.sequence)
        .await
        .unwrap_or_else(|| panic!("the turn holding the request {}", request.sequence));
    let ends = harness.turn_ends(report_end).await;
    assert!(ends.len() >= 2, "the report and two queued turns: {ends:?}");
    let first_end = ends[0];
    let second_end = ends[1];
    assert!(harness.create_for(report_end).await.is_empty());
    assert!(harness.create_for(first_end).await.is_empty());
    let creates = harness.create_for(second_end).await;
    assert_eq!(
        creates.len(),
        1,
        "the evaluator is created after the queue drains ({:?} {:?}): {creates:#?}",
        first.sequence,
        second.sequence
    );
    harness.close().await;
}

/// Gap 2, `edit`: an edit in the request's own turn, an edit in a deferred
/// turn, and a pause with its resume all supersede the retained request.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn beta_5_edit_supersedes_the_request() {
    let model = Arc::new(ScriptedModel::default());
    model.halt(Halt::AfterTool);
    model.halt(Halt::AfterTool);
    model.halt(Halt::Text("hold the queued turn".into()));
    let mut harness = evaluator_harness(model.clone()).await;

    // The request's own turn: the edit is folded while the turn is open.
    harness.report().await;
    model.entered().await;
    let request = wait_goal(&harness.session, "main", r#"kind="goal" op="completed""#).await;
    harness.goal_view().await;
    let min = harness.session.sequence("main").unwrap();
    let mut done = dim_goal::testing::hold_stage(
        &harness.id,
        "main",
        dim_goal::testing::Stage::Done,
        min,
        None,
    );
    harness
        .fixture
        .goal_on(
            &harness.id,
            "edit",
            json!({"objective": "make the other tests pass"}),
        )
        .await
        .unwrap();
    let view = harness.goal_view().await;
    assert_eq!(view["goal"]["objective"], "make the other tests pass");
    model.release();
    let report_end = done.arrived().await;
    done.release();
    harness.fixture.idle_on(&harness.id, "main").await;
    let creates = harness.evaluations().await;
    assert!(
        creates.is_empty(),
        "an edit in the request's turn supersedes it: {creates:#?} for {} at {report_end}",
        request.sequence
    );

    // A deferred turn: the report turn keeps the request, and the edit lands
    // while the queued input drives the next turn.
    harness
        .fixture
        .goal_on(
            &harness.id,
            "set",
            json!({"objective": "make the tests pass", "judge": "evaluator"}),
        )
        .await
        .unwrap();
    harness.report().await;
    model.entered().await;
    harness
        .submit("hold the queued turn", InputMode::Queue)
        .await;
    model.release();
    model.entered().await;
    harness.goal_view().await;
    harness
        .fixture
        .goal_on(
            &harness.id,
            "edit",
            json!({"objective": "make the other tests pass"}),
        )
        .await
        .unwrap();
    let view = harness.goal_view().await;
    assert_eq!(view["goal"]["objective"], "make the other tests pass");
    model.release();
    harness.fixture.idle_on(&harness.id, "main").await;
    let creates = harness.evaluations().await;
    assert!(
        creates.is_empty(),
        "an edit in the deferred turn supersedes the request: {creates:#?}"
    );

    // A pause supersedes it, and a resume does not bring it back.
    harness
        .fixture
        .goal_on(
            &harness.id,
            "set",
            json!({"objective": "make the tests pass", "judge": "evaluator"}),
        )
        .await
        .unwrap();
    model.halt(Halt::AfterTool);
    harness.report().await;
    model.entered().await;
    harness.goal_view().await;
    harness
        .fixture
        .goal_on(&harness.id, "pause", json!({}))
        .await
        .unwrap();
    let view = harness.goal_view().await;
    assert_eq!(view["goal"]["status"]["kind"], "paused");
    model.release();
    harness.fixture.idle_on(&harness.id, "main").await;
    harness
        .fixture
        .goal_on(&harness.id, "resume", json!({}))
        .await
        .unwrap();
    harness.fixture.idle_on(&harness.id, "main").await;
    let creates = harness.evaluations().await;
    assert!(
        creates.is_empty(),
        "a pause supersedes the request and a resume does not revive it: {creates:#?}"
    );
    harness.close().await;
}

/// Gap 2, `pause`: the pause supersedes the retained request.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn beta_6_pause_supersedes_a_deferred_request() {
    let model = Arc::new(ScriptedModel::default());
    model.halt(Halt::AfterTool);
    model.halt(Halt::Text("hold the queued turn".into()));
    let mut harness = evaluator_harness(model.clone()).await;
    harness.report().await;
    model.entered().await;
    harness
        .submit("hold the queued turn", InputMode::Queue)
        .await;
    let min = harness.session.sequence("main").unwrap();
    let mut done = dim_goal::testing::hold_stage(
        &harness.id,
        "main",
        dim_goal::testing::Stage::Done,
        min,
        None,
    );
    model.release();
    let report_end = done.arrived().await;
    done.release();
    assert!(
        harness.create_for(report_end).await.is_empty(),
        "the report turn end creates no evaluator while the queued input is pending"
    );
    model.entered().await;

    harness
        .fixture
        .goal_on(&harness.id, "pause", json!({}))
        .await
        .unwrap();
    let view = harness.goal_view().await;
    assert_eq!(view["goal"]["status"]["kind"], "paused");
    model.release();

    wait_goal(&harness.session, "main", r#"status="paused""#).await;
    harness.fixture.idle_on(&harness.id, "main").await;
    let creates = harness.evaluations().await;
    assert!(
        creates.is_empty(),
        "a pause leaves no evaluator for the old request: {creates:#?}"
    );
    harness.close().await;
}

/// Gap 3: a settle that asks for the turn end's decision again after the
/// evaluator was created writes no second evaluator and no continuation.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn beta_7_a_started_creation_is_not_repeated() {
    let model = Arc::new(ScriptedModel::default());
    model.halt(Halt::AfterTool);
    let mut harness = evaluator_harness(model.clone()).await;
    harness.report().await;
    model.entered().await;
    let min = harness.session.sequence("main").unwrap();
    let mut decision = dim_goal::testing::hold_stage(
        &harness.id,
        "main",
        dim_goal::testing::Stage::Decision,
        min,
        None,
    );
    model.release();
    let report_end = decision.arrived().await;

    let mut create_gate = dim_goal::testing::hold_stage(
        &harness.id,
        "main",
        dim_goal::testing::Stage::Create,
        report_end,
        Some(report_end),
    );
    let mut done = dim_goal::testing::hold_stage(
        &harness.id,
        "main",
        dim_goal::testing::Stage::Done,
        report_end,
        Some(report_end),
    );
    let mut second = dim_goal::testing::hold_stage(
        &harness.id,
        "main",
        dim_goal::testing::Stage::Decision,
        report_end,
        Some(report_end),
    );
    decision.release();
    create_gate.arrived().await;

    // A background task opens and a cancelled end closes it: the settle asks
    // for the turn-end decision again, and the running step answers with a
    // retry.
    let task = format!("probe-{report_end}");
    harness.fixture.probe.emit(
        "main",
        EventKind::ExecStarted(ExecStartedData {
            turn_id: format!("probe-{report_end}"),
            request_id: format!("probe-{report_end}"),
            call_id: format!("probe-{report_end}"),
            task_id: task.clone(),
            description: "probe".into(),
            title: None,
            output_path: "/dev/null".into(),
        }),
    );
    harness.goal_view().await;
    harness.fixture.probe.emit(
        "main",
        EventKind::ExecEnded(ExecEndedData {
            task_id: task,
            status: ExecStatus::Cancelled,
            result: ToolResult::default(),
        }),
    );
    harness.goal_view().await;

    create_gate.release();
    wait_event(&harness.session, "main", |kind| {
        matches!(kind, EventKind::BranchCreate(_))
    })
    .await;
    harness.goal_view().await;
    done.arrived().await;
    done.release();
    let retried = tokio::time::timeout(NEGATIVE, second.arrived()).await;
    assert!(
        retried.is_err(),
        "the settled turn end takes no second decision"
    );

    harness.fixture.idle_on(&harness.id, "main").await;
    let creates = harness.create_for(report_end).await;
    assert_eq!(
        creates.len(),
        1,
        "one evaluator for the turn end: {creates:#?}"
    );
    let after: Vec<_> = goal_writes(&harness.session, "main")
        .await
        .into_iter()
        .filter(|block| block.sequence > report_end)
        .filter(|block| block.marker.contains("kind=\"continuation\""))
        .collect();
    assert!(
        after.is_empty(),
        "no continuation after the created evaluator: {after:#?}"
    );
    harness.close().await;
}

/// ④: a later turn end whose decision runs while the creation for the request
/// is started creates nothing more for that request.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn beta_9_a_later_turn_end_creates_nothing_for_a_started_request() {
    let model = Arc::new(ScriptedModel::default());
    model.halt(Halt::AfterTool);
    let mut harness = evaluator_harness(model.clone()).await;
    harness.report().await;
    model.entered().await;
    let min = harness.session.sequence("main").unwrap();
    let mut decision = dim_goal::testing::hold_stage(
        &harness.id,
        "main",
        dim_goal::testing::Stage::Decision,
        min,
        None,
    );
    model.release();
    let report_end = decision.arrived().await;

    // The creation is started and not issued: its `branch/create` is not
    // committed, so the request is still pending in the fold.
    let mut create_gate = dim_goal::testing::hold_stage(
        &harness.id,
        "main",
        dim_goal::testing::Stage::Create,
        report_end,
        Some(report_end),
    );
    let mut next = dim_goal::testing::hold_stage(
        &harness.id,
        "main",
        dim_goal::testing::Stage::Done,
        report_end + 1,
        None,
    );
    decision.release();
    create_gate.arrived().await;

    harness.submit("keep going", InputMode::Queue).await;
    let next_end = tokio::time::timeout(WAIT, next.arrived())
        .await
        .expect("the next turn's decision");
    assert!(next_end > report_end, "the next turn end");
    create_gate.release();
    harness.fixture.idle_on(&harness.id, "main").await;

    let creates = harness.evaluations().await;
    assert_eq!(
        creates.len(),
        1,
        "the request's creation is started once: {creates:#?}"
    );
    assert_eq!(
        creates[0].1.source,
        BranchSource::Host {
            operation_id: format!("goal:evaluate:{report_end}")
        }
    );
    harness.close().await;
}

/// The admission boundary: an input committed after the report turn end and
/// not yet folded still keeps the evaluator from starting at that turn end.
/// The creation waits for the fold to catch up, rechecks, and the queue's
/// turn end creates the evaluator (Pursuit §4.1, C4, C5).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn beta_10_an_input_committed_behind_the_fold_defers_the_creation() {
    let model = Arc::new(ScriptedModel::default());
    model.halt(Halt::AfterTool);
    let harness = evaluator_harness(model.clone()).await;
    harness.report().await;
    model.entered().await;
    let min = harness.session.sequence("main").unwrap();
    let mut decision = dim_goal::testing::hold_stage(
        &harness.id,
        "main",
        dim_goal::testing::Stage::Decision,
        min,
        None,
    );
    model.release();
    let report_end = decision.arrived().await;

    // The queue commits after the report turn end, and Goal's fold is held
    // before it, so the decision reads a fold the input has not reached.
    let mut input_gate = dim_goal::testing::hold_input(&harness.id, "main", report_end);
    let queued = harness.submit("keep going", InputMode::Queue).await;
    let held = input_gate.arrived().await;
    assert!(
        queued.sequence == held && held > report_end,
        "the queued input at {} waits to fold at {held} after the report turn end {report_end}",
        queued.sequence
    );
    let mut early = dim_goal::testing::hold_stage(
        &harness.id,
        "main",
        dim_goal::testing::Stage::Create,
        report_end,
        Some(report_end),
    );
    decision.release();
    let created = tokio::time::timeout(NEGATIVE, early.arrived()).await;
    assert!(
        created.is_err(),
        "the report turn end creates no evaluator while a committed input is not folded"
    );
    input_gate.release();

    wait_event(&harness.session, "main", |kind| {
        matches!(kind, EventKind::BranchCreate(_))
    })
    .await;
    assert!(
        harness.create_for(report_end).await.is_empty(),
        "the report turn end creates no evaluator"
    );
    let ends = harness.turn_ends(report_end).await;
    let queue_end = ends[0];
    let creates = harness.create_for(queue_end).await;
    assert_eq!(
        creates.len(),
        1,
        "the queue drain turn end creates the evaluator: {creates:#?}"
    );
    harness.close().await;
}

/// The same admission boundary one step later: the Queue commits while the
/// claim waits for an earlier cutoff, and is still unfolded when that cutoff
/// is reached. The claim catches up to the newer cutoff before it reads the
/// fold, so the request waits for the queue's turn end (Pursuit §4.1, C4, C5).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn beta_11_a_queue_committed_during_the_catch_up_defers_the_creation() {
    let model = Arc::new(ScriptedModel::default());
    model.halt(Halt::AfterTool);
    let harness = evaluator_harness(model.clone()).await;
    harness.report().await;
    model.entered().await;
    let min = harness.session.sequence("main").unwrap();
    let mut decision = dim_goal::testing::hold_stage(
        &harness.id,
        "main",
        dim_goal::testing::Stage::Decision,
        min,
        None,
    );
    model.release();
    let report_end = decision.arrived().await;

    // An input that folds behind the report turn end is the cutoff the claim
    // waits for. It drives nothing, so the decision is still to create.
    let mut earlier_gate = dim_goal::testing::hold_input(&harness.id, "main", report_end);
    let earlier = harness.submit("a note", InputMode::Append).await;
    let held = earlier_gate.arrived().await;
    assert!(
        earlier.sequence == held && held > report_end,
        "the append input at {} waits to fold at {held} after the report turn end {report_end}",
        earlier.sequence
    );

    let mut cutoff_gate = dim_goal::testing::hold_stage(
        &harness.id,
        "main",
        dim_goal::testing::Stage::Cutoff,
        held,
        None,
    );
    let mut early = dim_goal::testing::hold_stage(
        &harness.id,
        "main",
        dim_goal::testing::Stage::Create,
        report_end,
        Some(report_end),
    );
    decision.release();
    let cutoff = cutoff_gate.arrived().await;
    assert!(
        cutoff >= held,
        "the claim's cutoff {cutoff} is not earlier than the input held behind the fold at {held}"
    );

    // The queue commits behind the cutoff the claim read, and its fold stays
    // held when the dispatcher reaches that cutoff.
    let queue_gate = dim_goal::testing::hold_input(&harness.id, "main", cutoff + 1);
    let queued = harness.submit("keep going", InputMode::Queue).await;
    assert!(
        queued.sequence > cutoff,
        "the queued input at {} commits after the claim's cutoff {cutoff}",
        queued.sequence
    );
    cutoff_gate.release();
    earlier_gate.release();

    let created = tokio::time::timeout(NEGATIVE, early.arrived()).await;
    assert!(
        created.is_err(),
        "the report turn end creates no evaluator while a committed input is not folded"
    );
    queue_gate.release();

    wait_event(&harness.session, "main", |kind| {
        matches!(kind, EventKind::BranchCreate(_))
    })
    .await;
    assert!(
        harness.create_for(report_end).await.is_empty(),
        "the report turn end creates no evaluator"
    );
    let ends = harness.turn_ends(report_end).await;
    let queue_end = ends[0];
    let creates = harness.create_for(queue_end).await;
    assert_eq!(
        creates.len(),
        1,
        "the queue drain turn end creates the evaluator: {creates:#?}"
    );
    harness.close().await;
}

/// The turn end's continuation reads the same committed cutoff: a Queue that
/// commits while that wait runs leaves the continuation behind, so the turn
/// end writes none and the queue's own turn end decides (Pursuit §4.1, C4,
/// C5).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn beta_12_a_queue_committed_during_the_catch_up_holds_the_continuation() {
    let model = Arc::new(ScriptedModel::default());
    let mut fixture = Fixture::start_with(model).await;
    fixture.set_limit(5).await;
    let id = fixture.new_session(&["goal_update", "wait"]).await;
    let session = fixture.agent.get_session(&id).unwrap();

    let mut write = dim_goal::testing::hold_write(&id, "main");
    fixture
        .goal_on(
            &id,
            "set",
            json!({"objective": "make the tests pass", "judge": "evaluator"}),
        )
        .await
        .unwrap();
    let set_end = tokio::time::timeout(WAIT, write.arrived())
        .await
        .expect("the set turn's continuation reached its write in time");

    let mut earlier_gate = dim_goal::testing::hold_input(&id, "main", set_end);
    let earlier = session
        .submit("a note".to_owned(), InputMode::Append, "main")
        .await
        .unwrap();
    let held = earlier_gate.arrived().await;
    assert!(
        earlier.sequence == held && held > set_end,
        "the append input at {} waits to fold at {held} after the set turn end {set_end}",
        earlier.sequence
    );

    let mut cutoff_gate =
        dim_goal::testing::hold_stage(&id, "main", dim_goal::testing::Stage::Cutoff, held, None);
    write.release();
    let cutoff = cutoff_gate.arrived().await;
    assert!(
        cutoff >= held,
        "the recheck's cutoff {cutoff} is not earlier than the input held behind the fold at {held}"
    );

    let queue_gate = dim_goal::testing::hold_input(&id, "main", cutoff + 1);
    let queued = session
        .submit("keep going".to_owned(), InputMode::Queue, "main")
        .await
        .unwrap();
    assert!(
        queued.sequence > cutoff,
        "the queued input at {} commits after the recheck's cutoff {cutoff}",
        queued.sequence
    );
    cutoff_gate.release();
    earlier_gate.release();

    let stale = tokio::time::timeout(NEGATIVE, async {
        loop {
            let writes = goal_writes(&session, "main").await;
            if let Some(block) = writes.iter().find(|block| {
                block.marker.contains(r#"kind="continuation""#)
                    && block.marker.contains(&format!(r#"after="{set_end}""#))
            }) {
                return block.marker.clone();
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    assert!(
        stale.is_err(),
        "the set turn end writes no continuation while a committed input is not folded: {stale:?}"
    );

    queue_gate.release();
    let queue_end = tokio::time::timeout(WAIT, async {
        loop {
            if let Some(end) = turn_ends(&session, "main")
                .await
                .into_iter()
                .find(|end| *end > set_end)
            {
                return end;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the queued input's turn end");
    let continued = tokio::time::timeout(WAIT, async {
        loop {
            let writes = goal_writes(&session, "main").await;
            if let Some(block) = writes.iter().find(|block| {
                block.marker.contains(r#"kind="continuation""#)
                    && block.marker.contains(&format!(r#"after="{queue_end}""#))
            }) {
                return block.marker.clone();
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    assert!(
        continued.is_ok(),
        "the queued input's turn end decides the continuation: {continued:?}"
    );
    fixture.close().await;
}

/// Gap 4: with a background task still open, withdrawing the queued driving
/// input still creates the evaluation at the turn end.
#[tokio::test]
async fn beta_8_an_open_task_does_not_block_the_deferred_evaluation() {
    use support::parts::{Env, raw};

    let env = Env::start().await;
    env.goal("set", json!({"objective": "ship it", "judge": "evaluator"}))
        .await
        .unwrap();
    let set = env.wait_for("main", r#"op="set""#).await;
    env.turn("main", &[&set]);
    let continuation = env.wait_for("main", r#"kind="continuation""#).await;

    env.turn_start("main", "work");
    env.consume("main", &continuation.id);
    env.tool(
        "main",
        json!({"status": "completed", "summary": "all tests pass"}),
    )
    .await;
    env.exec_started("main", "work", "task-1");
    let queued = env.write_message(
        "main",
        vec![raw("keep going", None, false)],
        InputMode::Queue,
    );
    let mut done = dim_goal::testing::hold_stage(
        env.session_id(),
        "main",
        dim_goal::testing::Stage::Done,
        0,
        None,
    );
    let end = env.end_turn("main", "work", "completed");
    done.arrived().await;
    assert!(
        env.creates("main").await.is_empty(),
        "the turn end creates no evaluator while the driving input is pending and the task is open"
    );

    let deleted = env.emit(
        "main",
        EventKind::InputDeleted(dim_agent_types::InputDeletedData {
            input_id: queued.id.clone(),
        }),
    );
    env.dispatched("main", deleted.sequence).await;
    done.release();

    tokio::time::timeout(WAIT, async {
        loop {
            if !env.creates("main").await.is_empty() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the withdrawn input creates the evaluation despite the open task");
    let creates = env.creates("main").await;
    assert_eq!(creates.len(), 1, "one evaluator: {creates:#?}");
    assert_eq!(
        creates[0].source,
        BranchSource::Host {
            operation_id: format!("goal:evaluate:{}", end.sequence)
        }
    );
    env.close().await;
}

/// The request the loop builds after a `goal_update` report whose input was
/// consumed together with the tool result: the report's state record and
/// completed trigger sit at the tail, and the selection must reach past them
/// to the tool result. `goal_update` writes both blocks as append-mode inputs
/// in one atomic batch before its result commits (Goal A.5, R5), so under load
/// the input events land ahead of the result and the loop renders them after
/// it; a tail read that stops at the block never recognizes the tool turn.
#[test]
fn scripted_matches_skip_goal_inputs_riding_a_tool_result() {
    let state_record = r#"<dim-goal kind="state" status="completed" judge="evaluator" trigger="goal:request-id" branch="main">
<objective>
make the tests pass
</objective>
This is the user's standing goal for branch main. If you are on another line of this Session, the goal belongs to branch main: treat it as background, do not continue it, and do not call goal_update for it.
goal_update set the goal status for branch main to completed. Do not continue working toward a goal for branch main on your own.
<result>
all tests pass
</result>
</dim-goal>"#;
    let completed = r#"<dim-goal kind="goal" op="completed" branch="main">
Result:
all tests pass

goal_update reported the goal for branch main as completed.
</dim-goal>"#;
    let blocked = r#"<dim-goal kind="goal" op="blocked" branch="main">
Blocker:
needs a decision

goal_update reported the goal for branch main as blocked.
</dim-goal>"#;
    let edit = r#"<dim-goal kind="goal" op="edit" branch="main">
Objective:
make the other tests pass

The user asked to edit the objective of the goal for branch main.
</dim-goal>"#;
    let pause = r#"<dim-goal kind="goal" op="pause" reason="user" branch="main">
The user asked to pause the goal for branch main.
</dim-goal>"#;
    let cleared = r#"<dim-goal kind="goal" op="clear" branch="main">
The user asked to clear the goal for branch main.
</dim-goal>"#;
    let retired_status = r#"<dim-goal kind="status" status="cleared" branch="main">
The goal for branch main is not in effect.
</dim-goal>"#;
    let conclusion = r#"<dim-goal kind="report" result="completed" evaluation="1" evaluator="evaluate" branch="main">
Evaluator report:
all tests pass
</dim-goal>"#;
    let set = r#"<dim-goal kind="goal" op="set" judge="evaluator" branch="main">
Objective:
make the tests pass

The user asked to set the goal for branch main to this objective.
</dim-goal>"#;
    let resume = r#"<dim-goal kind="goal" op="resume" branch="main">
The user asked to resume the goal for branch main.
</dim-goal>"#;
    let continuation = r#"<dim-goal kind="continuation" count="1" after="9" idle="0" branch="main">
Continue working toward the active goal for branch main.
</dim-goal>"#;

    let request = |tail: Vec<dim_agent_types::Message>| {
        dim_agent_capabilities::host::ModelSettings::new("mock", "mock")
            .request(tail.into_iter().map(Arc::new).collect())
    };
    let tool_result = || dim_agent_types::Message::text(Role::ToolResult, "goal updated");

    // The append inputs consumed with the report's tool result do not hide
    // it: the selection is the tool result either way.
    for blocks in [
        vec![state_record],
        vec![completed],
        vec![blocked],
        vec![edit],
        vec![pause],
        vec![cleared],
        vec![conclusion],
        vec![state_record, completed],
        vec![state_record, retired_status],
    ] {
        let mut messages = vec![tool_result()];
        messages.extend(
            blocks
                .iter()
                .map(|text| dim_agent_types::Message::text(Role::User, *text)),
        );
        let found = ScriptedModel::matches(&request(messages));
        assert_eq!(
            found,
            Some(Halt::AfterTool),
            "a riding block must not hide the tool result: {blocks:?}"
        );
    }

    // A conversational input drives a turn the blocks ride along with: a
    // queued input starts the turn, the goal change appends commit behind it,
    // and the selection falls past them to the driving text.
    let messages = vec![
        dim_agent_types::Message::text(Role::User, "hold the queued turn"),
        dim_agent_types::Message::text(Role::User, state_record),
        dim_agent_types::Message::text(Role::User, completed),
    ];
    assert_eq!(
        ScriptedModel::matches(&request(messages.clone())),
        Some(Halt::Text("hold the queued turn".into()))
    );
    assert_eq!(
        last_user_text(&request(messages)),
        Some("hold the queued turn")
    );

    // Driving inputs are never skipped: the tail block itself is the match.
    for driving in [set, resume, continuation] {
        let messages = vec![
            tool_result(),
            dim_agent_types::Message::text(Role::User, driving),
        ];
        assert_eq!(
            ScriptedModel::matches(&request(messages)),
            Some(Halt::Text(driving.to_owned())),
            "a driving input keeps its meaning"
        );
    }

    // Plain user text and the report command are unaffected.
    let report_turn = vec![dim_agent_types::Message::text(
        Role::User,
        r#"tool:goal_update {"status": "completed", "summary": "all tests pass"}"#,
    )];
    assert_eq!(
        ScriptedModel::matches(&request(report_turn)),
        Some(Halt::Report)
    );
    assert_eq!(ScriptedModel::matches(&request(vec![])), None);
}

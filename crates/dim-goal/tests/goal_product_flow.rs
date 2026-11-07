//! The pinned Node Goal flows (transport_flow/goal.rs) against the native
//! Goal in the product profile: `serve` with the defaults and
//! `dim_goal::install`, no TypeScript row. Each flow keeps its scenario and
//! compares Goal's writes with the literal texts of support/texts.rs byte for
//! byte.
mod support;

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use dim_agent_capabilities::adapters::EchoModel;
use dim_agent_capabilities::history::summary_message;
use dim_agent_capabilities::host::{
    HostError, HostFuture, ModelChunk, ModelConnection, ModelDriver, ModelStopReason, ModelStream,
};
use dim_agent_capabilities::timeline_tree::TreeCommands;
use dim_agent_runtime::{BranchOptions, Session};
use dim_agent_types::{
    BranchSource, ContextBaselineEntry, ContextCompactData, ContextCompactionData,
    ContextCompactionResult, EventKind, Message, MessageContent, ModelRequest, Role, ToolCall,
};
use serde_json::{Value, json};
use support::texts;
use support::*;
use tokio_util::sync::CancellationToken;

/// Text with `<`, `&`, quotes, an entity-like run and a blank line.
const SPECIAL: &str = "ship <it> & \"now\"\n\nthen &amp; done";

/// Whether the request is the evaluator's: its prompt carries Goal's
/// evaluate marker (Pursuit §5).
fn is_evaluator(request: &ModelRequest) -> bool {
    request.messages.iter().any(|message| {
        matches!(&message.content, MessageContent::Text(text)
            if text.contains("<dim-goal kind=\"evaluate\""))
    })
}

/// The `count` attribute of a Goal write's marker.
fn count_of(marker: &str) -> Option<usize> {
    marker
        .split("count=\"")
        .nth(1)?
        .split('"')
        .next()?
        .parse()
        .ok()
}

/// The sequence of the newest evaluator conclusion input on `branch`.
async fn conclusion_sequence(session: &Session, branch: &str) -> u64 {
    branch_events(session, branch, ..)
        .await
        .unwrap()
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::InputMessage(data) => data.blocks.first().and_then(|block| {
                let dim_agent_types::InputBlock::Raw { text, .. } = block;
                text.starts_with(r#"<dim-goal kind="report""#)
                    .then_some(event.sequence)
            }),
            _ => None,
        })
        .next_back()
        .unwrap_or_else(|| panic!("no conclusion input on {branch}"))
}

/// The `after` attribute of a Goal write's marker.
fn after_of(marker: &str) -> Option<u64> {
    marker
        .split("after=\"")
        .nth(1)?
        .split('"')
        .next()?
        .parse()
        .ok()
}

/// Answers the evaluator's prompt with a DONE conclusion; echoes otherwise.
struct EvaluatingModel;
impl ModelDriver for EvaluatingModel {
    fn invoke(
        &self,
        request: Arc<ModelRequest>,
        connection: ModelConnection,
        cancel: CancellationToken,
    ) -> HostFuture<'_, ModelStream> {
        if !is_evaluator(&request) {
            return EchoModel.invoke(request, connection, cancel);
        }
        Box::pin(async move {
            Ok(Box::pin(futures_util::stream::iter(vec![
                Ok(ModelChunk::MessageStart {
                    message_id: "evaluation".into(),
                }),
                Ok(ModelChunk::Text(
                    "Read the tests and ran them.\nDONE: all tests pass".into(),
                )),
                Ok(ModelChunk::TextEnd),
                Ok(ModelChunk::MessageStop {
                    message_id: "evaluation".into(),
                    reason: ModelStopReason::EndTurn,
                }),
            ])) as ModelStream)
        })
    }
}

/// Holds the evaluator's turn in a long tool wait; echoes otherwise.
struct WaitingModel;
impl ModelDriver for WaitingModel {
    fn invoke(
        &self,
        request: Arc<ModelRequest>,
        connection: ModelConnection,
        cancel: CancellationToken,
    ) -> HostFuture<'_, ModelStream> {
        if !is_evaluator(&request) {
            return EchoModel.invoke(request, connection, cancel);
        }
        Box::pin(async move {
            let call = ToolCall {
                id: "evaluation-wait".into(),
                name: "wait".into(),
                input: json!({"millis": 60000}).as_object().unwrap().clone(),
            };
            Ok(Box::pin(futures_util::stream::iter(vec![
                Ok(ModelChunk::MessageStart {
                    message_id: "evaluation".into(),
                }),
                Ok(ModelChunk::ToolStart {
                    id: call.id.clone(),
                    name: call.name.clone(),
                }),
                Ok(ModelChunk::ToolUse(call)),
                Ok(ModelChunk::MessageStop {
                    message_id: "evaluation".into(),
                    reason: ModelStopReason::ToolUse,
                }),
            ])) as ModelStream)
        })
    }
}

/// Answers the evaluator's first turn without a conclusion and its second
/// with CONTINUE; echoes otherwise.
#[derive(Default)]
struct RevisingModel {
    evaluator_turns: AtomicUsize,
}
impl ModelDriver for RevisingModel {
    fn invoke(
        &self,
        request: Arc<ModelRequest>,
        connection: ModelConnection,
        cancel: CancellationToken,
    ) -> HostFuture<'_, ModelStream> {
        if !is_evaluator(&request) {
            return EchoModel.invoke(request, connection, cancel);
        }
        let turn = self.evaluator_turns.fetch_add(1, Ordering::SeqCst);
        let text = if turn == 0 {
            "I read the tests.".to_owned()
        } else {
            "Still missing.\nCONTINUE:\n- tests still fail".to_owned()
        };
        Box::pin(async move {
            Ok(Box::pin(futures_util::stream::iter(vec![
                Ok(ModelChunk::MessageStart {
                    message_id: "evaluation".into(),
                }),
                Ok(ModelChunk::Text(text)),
                Ok(ModelChunk::TextEnd),
                Ok(ModelChunk::MessageStop {
                    message_id: "evaluation".into(),
                    reason: ModelStopReason::EndTurn,
                }),
            ])) as ModelStream)
        })
    }
}

/// Answers the evaluator's turn with BLOCKED; echoes otherwise.
struct BlockedModel;
impl ModelDriver for BlockedModel {
    fn invoke(
        &self,
        request: Arc<ModelRequest>,
        connection: ModelConnection,
        cancel: CancellationToken,
    ) -> HostFuture<'_, ModelStream> {
        if !is_evaluator(&request) {
            return EchoModel.invoke(request, connection, cancel);
        }
        Box::pin(async move {
            Ok(Box::pin(futures_util::stream::iter(vec![
                Ok(ModelChunk::MessageStart {
                    message_id: "evaluation".into(),
                }),
                Ok(ModelChunk::Text(
                    "Blocked on credentials.\nBLOCKED: need credentials".into(),
                )),
                Ok(ModelChunk::TextEnd),
                Ok(ModelChunk::MessageStop {
                    message_id: "evaluation".into(),
                    reason: ModelStopReason::EndTurn,
                }),
            ])) as ModelStream)
        })
    }
}

/// Fails the evaluator's turn; echoes every other request.
struct FailingEvaluator;
impl ModelDriver for FailingEvaluator {
    fn invoke(
        &self,
        request: Arc<ModelRequest>,
        connection: ModelConnection,
        cancel: CancellationToken,
    ) -> HostFuture<'_, ModelStream> {
        if !is_evaluator(&request) {
            return EchoModel.invoke(request, connection, cancel);
        }
        Box::pin(async move { Err(HostError::failed("fixture evaluator failure")) })
    }
}

/// Fails a turn whose last operator text is `fail`; echoes otherwise.
struct FailingModel;
impl ModelDriver for FailingModel {
    fn invoke(
        &self,
        request: Arc<ModelRequest>,
        connection: ModelConnection,
        cancel: CancellationToken,
    ) -> HostFuture<'_, ModelStream> {
        let fails = request.messages.iter().rev().find_map(|message| {
            (message.role == Role::User).then(|| match &message.content {
                MessageContent::Text(text) => text.contains("fail"),
                _ => false,
            })
        });
        if fails != Some(true) {
            return EchoModel.invoke(request, connection, cancel);
        }
        Box::pin(async move { Err(HostError::failed("fixture failure")) })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn goal_continues_to_the_limit_and_stops_on_completion() {
    let mut fixture = Fixture::start().await;
    fixture.set_limit(2).await;

    // Limit (Appendix D 1, 2): set drives a turn; each normal end writes a
    // continuation until the third decision pauses with `limit`.
    let id = fixture.new_session(&["goal_update", "wait"]).await;
    let session = fixture.agent.get_session(&id).unwrap();
    let sequence = fixture
        .goal_on(&id, "set", json!({"objective": "ship <it>"}))
        .await
        .unwrap();
    assert!(sequence > 0);
    wait_goal(&session, "main", r#"reason="limit""#).await;
    fixture.idle_on(&id, "main").await;
    let writes = goal_writes(&session, "main").await;
    assert_eq!(writes.len(), 5, "{writes:#?}");
    assert_eq!(writes[1].sequence, sequence, "the set trigger");
    assert_eq!(writes[1].text, texts::set("main", "ship <it>"));
    assert_eq!(
        writes[0].mode,
        dim_agent_types::InputMode::Append,
        "the record commits first"
    );
    wait_goal(
        &session,
        "main",
        &format!(
            r#"kind="state" status="active" trigger="{}""#,
            writes[1].submission_id.as_deref().unwrap()
        ),
    )
    .await;
    assert_eq!(
        goal_block(&session, "main", r#"kind="state" status="active""#).await,
        texts::set_state("main", "ship <it>", &texts::trigger(&writes[1]), false),
        "the record that answers the set"
    );
    assert!(
        writes.iter().any(|block| block
            .marker
            .contains(r#"kind="continuation" count="1" limit="2""#)),
        "{writes:#?}"
    );
    assert!(
        writes.iter().any(|block| block
            .marker
            .contains(r#"kind="continuation" count="2" limit="2""#)),
        "{writes:#?}"
    );
    assert!(
        writes.iter().any(|block| block
            .marker
            .contains(r#"kind="state" status="paused" reason="limit""#)),
        "{writes:#?}"
    );
    // Every model-facing text, byte for byte. The three turns are the set's,
    // and the two continuations'.
    let ends = turn_ends(&session, "main").await;
    assert_eq!(ends.len(), 3, "three turns: {ends:?}");
    assert_eq!(
        goal_blocks(&session, "main", r#"op="set""#).await,
        vec![texts::set("main", "ship <it>")],
        "the set trigger is what the model reads"
    );
    assert_eq!(
        goal_block(&session, "main", r#"count="1""#).await,
        texts::continuation("main", 1, Some(2), 0, ends[0])
    );
    assert_eq!(
        goal_block(&session, "main", r#"count="2""#).await,
        texts::continuation("main", 2, Some(2), 1, ends[1])
    );
    assert_eq!(
        goal_block(
            &session,
            "main",
            r#"kind="state" status="paused" reason="limit""#
        )
        .await,
        texts::automatic_pause(
            "main",
            "ship <it>",
            "limit",
            "the automatic continuation limit was reached",
            ends[2]
        )
    );
    assert_goal_tool(&session, "main").await;

    // A child branch cut after the goal starts with none: Goal retires the
    // inherited keys once (§3.4, Appendix D 7).
    let child = session
        .branch(
            "main".into(),
            session.sequence("main").unwrap(),
            BranchOptions {
                prompt: "child task".into(),
                source: BranchSource::Host {
                    operation_id: "goal-child".into(),
                },
            },
        )
        .await
        .unwrap();
    wait_goal(&session, &child, r#"reason="new_branch""#).await;
    fixture.idle_on(&id, &child).await;
    let child_writes = goal_writes(&session, &child).await;
    assert_eq!(child_writes.len(), 1, "{child_writes:#?}");
    assert_eq!(goal_writes(&session, "main").await.len(), 5);
    let opening = branch_events(&session, &child, ..)
        .await
        .unwrap()
        .iter()
        .find(|event| matches!(event.kind, EventKind::BranchStart(_)))
        .expect("the child's branch/start")
        .sequence;
    assert_eq!(
        goal_blocks(&session, &child, r#"reason="new_branch""#).await,
        texts::start_clear(&child, opening, (false, false)),
        "the new-branch clear is one retired record"
    );

    // Completion (Appendix D 1): the model's goal_update records `completed`
    // and no continuation follows it.
    fixture.set_limit(5).await;
    let id = fixture.new_session(&["goal_update", "wait"]).await;
    let session = fixture.agent.get_session(&id).unwrap();
    fixture
        .goal_on(&id, "set", json!({"objective": "finish"}))
        .await
        .unwrap();
    fixture
        .submit_on(
            &id,
            "main",
            &format!(
                "tool:goal_update {}",
                json!({"status": "completed", "summary": SPECIAL})
            ),
        )
        .await;
    wait_goal(&session, "main", r#"op="completed""#).await;
    fixture.idle_on(&id, "main").await;
    assert_eq!(
        goal_blocks(&session, "main", r#"op="set""#).await,
        vec![texts::set("main", "finish")],
        "the second session's set"
    );
    let finish_trigger = wait_goal(&session, "main", r#"op="set""#).await;
    wait_goal(
        &session,
        "main",
        &format!(
            r#"kind="state" status="active" trigger="{}""#,
            finish_trigger.submission_id.as_deref().unwrap()
        ),
    )
    .await;
    assert_eq!(
        goal_block(&session, "main", r#"kind="state" status="active""#).await,
        texts::set_state("main", "finish", &texts::trigger(&finish_trigger), false),
        "the second session's record"
    );
    assert_eq!(
        goal_block(&session, "main", r#"op="completed""#).await,
        texts::report("main", "completed", SPECIAL),
        "the completed report"
    );
    wait_tool_text(&session, "main", "goal_update recorded: completed.").await;
    let completed_block = goal_block(&session, "main", r#"op="completed""#).await;
    assert!(!completed_block.contains("<it>"), "{completed_block}");
    assert_eq!(block_value(&completed_block, "Result:"), SPECIAL);
    let writes = goal_writes(&session, "main").await;
    let completed = writes
        .iter()
        .find(|block| block.marker.contains(r#"op="completed""#))
        .expect("the completed report");
    assert_eq!(
        goal_block(&session, "main", r#"status="completed""#).await,
        texts::reported(
            "main",
            "finish",
            "completed",
            SPECIAL,
            &texts::trigger(completed)
        ),
        "the record that answers the report"
    );
    let completed = completed.sequence;
    assert!(
        writes
            .iter()
            .all(|block| block.sequence <= completed || !block.text.contains("continuation")),
        "{writes:#?}"
    );
    let results: Vec<String> = branch_events(&session, "main", ..)
        .await
        .unwrap()
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::ToolResult(data) => Some(serde_json::to_string(data).unwrap()),
            _ => None,
        })
        .collect();
    assert!(
        results
            .iter()
            .any(|result| result.contains("goal_update recorded: completed.")),
        "{results:#?}"
    );

    let error = fixture
        .goal_on(&id, "set", json!({"objective": ""}))
        .await
        .unwrap_err();
    assert_eq!(dim_client::app_error(&error), Some("invalid_params"));
    let error = fixture
        .goal_on(&id, "set", json!({"branchId": "missing", "objective": "x"}))
        .await
        .unwrap_err();
    assert_eq!(dim_client::app_error(&error), Some("session_invalid_input"));
    fixture.goal_on(&id, "pause", json!({})).await.unwrap();
    fixture.goal_on(&id, "resume", json!({})).await.unwrap();
    let clear = fixture.goal_on(&id, "clear", json!({})).await.unwrap();
    let clear_trigger = wait_goal(&session, "main", r#"op="clear""#).await;
    fixture.idle_on(&id, "main").await;
    assert_eq!(
        goal_block(&session, "main", r#"op="pause" reason="user""#).await,
        texts::pause("main")
    );
    assert_eq!(
        goal_block(&session, "main", r#"op="resume""#).await,
        texts::resume("main")
    );
    let _ = clear;
    wait_goal(
        &session,
        "main",
        &format!(
            r#"status="cleared" trigger="{}""#,
            clear_trigger.submission_id.as_deref().unwrap()
        ),
    )
    .await;
    assert_eq!(
        goal_blocks(&session, "main", r#"op="clear""#).await,
        vec![texts::clear("main")],
        "the user's clear"
    );
    assert_eq!(
        goal_block(&session, "main", r#"status="cleared""#).await,
        texts::cleared("main", &texts::trigger(&clear_trigger)),
        "the record that answers it"
    );

    // The tool's rejection text, and a `blocked` report.
    fixture
        .submit_on(
            &id,
            "main",
            r#"tool:goal_update {"status":"nope","summary":"x"}"#,
        )
        .await;
    wait_tool_text(&session, "main", texts::TOOL_INVALID).await;
    fixture
        .submit_report_on(&id, "blocked", "needs credentials")
        .await;
    wait_goal(&session, "main", r#"op="blocked""#).await;
    fixture.idle_on(&id, "main").await;
    assert_eq!(
        goal_block(&session, "main", r#"op="blocked""#).await,
        texts::report("main", "blocked", "needs credentials")
    );
    wait_tool_text(&session, "main", "goal_update recorded: blocked.").await;
    let written = goal_writes(&session, "main").await;
    let after: Vec<&Block> = written
        .iter()
        .filter(|block| block.sequence > completed)
        .collect();
    let at = |op: &str| {
        after
            .iter()
            .position(|block| block.marker.contains(&format!(r#"op="{op}""#)))
            .unwrap_or_else(|| panic!("no {op}: {after:#?}"))
    };
    assert!(
        at("pause") < at("resume") && at("resume") < at("clear"),
        "{after:#?}"
    );
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn goal_restore_refolds_without_repeating_decisions() {
    let mut fixture = Fixture::start().await;
    fixture.set_limit(1).await;
    let id = fixture.new_session(&["goal_update", "wait"]).await;
    let session = fixture.agent.get_session(&id).unwrap();
    fixture
        .goal_on(&id, "set", json!({"objective": SPECIAL}))
        .await
        .unwrap();
    wait_goal(&session, "main", r#"reason="limit""#).await;
    fixture.idle_on(&id, "main").await;
    assert_eq!(
        block_value(
            &goal_block(&session, "main", r#"op="set""#).await,
            "Objective:"
        ),
        SPECIAL
    );
    let edited = format!("{SPECIAL} (edited)");
    let edit = fixture
        .goal_on(&id, "edit", json!({"objective": edited}))
        .await
        .unwrap();
    let edit_trigger = wait_goal(&session, "main", r#"op="edit""#).await;
    fixture.idle_on(&id, "main").await;
    assert_eq!(
        goal_block(&session, "main", r#"op="edit""#).await,
        texts::edit("main", &edited)
    );
    // The edit is a trigger: its record states the new objective and the
    // status the goal actually has, still the limit pause.
    let _ = edit;
    wait_goal(
        &session,
        "main",
        &format!(
            r#"kind="state" status="paused" reason="limit" trigger="{}""#,
            edit_trigger.submission_id.as_deref().unwrap()
        ),
    )
    .await;
    assert_eq!(
        goal_block(
            &session,
            "main",
            &format!(
                r#"kind="state" status="paused" reason="limit" trigger="{}""#,
                edit_trigger.submission_id.as_deref().unwrap()
            )
        )
        .await,
        texts::paused(
            "main",
            &edited,
            "limit",
            "the automatic continuation limit was reached",
            &texts::trigger(&edit_trigger),
            false
        ),
        "the edit's record"
    );
    let before = goal_writes(&session, "main").await;
    assert_eq!(before.len(), 6, "{before:#?}");
    drop(session);
    let mut fixture = fixture.restart(Arc::new(EchoModel)).await;
    fixture.submit_on(&id, "main", "hello again").await;
    fixture.idle_on(&id, "main").await;
    let session = fixture.agent.get_session(&id).unwrap();
    assert_eq!(goal_writes(&session, "main").await, before);
    assert_eq!(
        block_value(
            &goal_block(&session, "main", r#"op="edit""#).await,
            "Objective:"
        ),
        edited
    );
    fixture.goal_on(&id, "resume", json!({})).await.unwrap();
    wait_goal(&session, "main", r#"kind="continuation" count="1""#).await;
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn goal_pending_input_and_interrupts() {
    let mut fixture = Fixture::start().await;
    fixture.set_limit(1).await;

    // Appendix D 4: the goal is set while a turn runs, and the operator
    // queues more input. The turn's end finds Queue input pending, so it
    // decides nothing; the turn that consumes them decides.
    let id = fixture.new_session(&["goal_update", "wait"]).await;
    let session = fixture.agent.get_session(&id).unwrap();
    fixture
        .submit_on(&id, "main", r#"tool:wait {"millis":1500}"#)
        .await;
    wait_event(&session, "main", |kind| {
        matches!(kind, EventKind::ToolCall(_))
    })
    .await;
    fixture
        .goal_on(&id, "set", json!({"objective": "ship"}))
        .await
        .unwrap();
    fixture.submit_on(&id, "main", "and the changelog").await;
    wait_goal(&session, "main", r#"reason="limit""#).await;
    fixture.idle_on(&id, "main").await;
    let ends = turn_ends(&session, "main").await;
    let writes = goal_writes(&session, "main").await;
    let decided: Vec<&String> = writes
        .iter()
        .map(|block| &block.marker)
        .filter(|line| line.contains("after="))
        .collect();
    assert!(
        decided
            .iter()
            .all(|line| !line.contains(&format!(r#"after="{}""#, ends[0]))),
        "{ends:?} {writes:#?}"
    );
    assert!(
        decided
            .iter()
            .any(|line| line.contains(r#"kind="continuation" count="1""#)),
        "{writes:#?}"
    );
    let continuation = writes
        .iter()
        .find(|block| block.marker.contains(r#"count="1""#))
        .expect("the first continuation")
        .sequence;
    let deciding = ends
        .iter()
        .rev()
        .find(|end| **end < continuation)
        .copied()
        .expect("the deciding turn");
    assert_eq!(
        goal_block(&session, "main", r#"count="1""#).await,
        texts::continuation("main", 1, Some(1), 0, deciding)
    );

    // Appendix D 3: an interrupt after the goal starts pauses it, and no
    // continuation follows.
    fixture.set_limit(5).await;
    let id = fixture.new_session(&["goal_update", "wait"]).await;
    let session = fixture.agent.get_session(&id).unwrap();
    fixture
        .submit_on(&id, "main", r#"tool:wait {"millis":1500}"#)
        .await;
    wait_event(&session, "main", |kind| {
        matches!(kind, EventKind::ToolCall(_))
    })
    .await;
    fixture
        .goal_on(&id, "set", json!({"objective": "ship"}))
        .await
        .unwrap();
    wait_goal(&session, "main", r#"op="set""#).await;
    fixture
        .client
        .rpc("session.stop", json!({"sessionId": id, "branchId": "main"}))
        .await
        .unwrap();
    wait_goal(&session, "main", r#"reason="interrupted""#).await;
    fixture.idle_on(&id, "main").await;
    let interrupt = branch_events(&session, "main", ..)
        .await
        .unwrap()
        .iter()
        .rev()
        .find(|event| matches!(event.kind, EventKind::StopInterrupt(_)))
        .expect("the interrupt")
        .sequence;
    assert_eq!(
        goal_block(&session, "main", r#"reason="interrupted""#).await,
        texts::automatic_pause(
            "main",
            "ship",
            "interrupted",
            "the user interrupted the turn",
            interrupt
        )
    );
    let writes = goal_writes(&session, "main").await;
    assert!(
        writes
            .iter()
            .all(|block| !block.text.contains("continuation")),
        "{writes:#?}"
    );
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn goal_settings_resume_unload_and_fork() {
    let mut fixture = Fixture::start().await;
    fixture.set_limit(1).await;
    let id = fixture.new_session(&["goal_update", "wait"]).await;
    let session = fixture.agent.get_session(&id).unwrap();
    fixture
        .goal_on(&id, "set", json!({"objective": "ship"}))
        .await
        .unwrap();
    wait_goal(&session, "main", r#"reason="limit""#).await;
    fixture.idle_on(&id, "main").await;

    // Appendix D 11: completed, then resume reopens the goal with the count
    // restarted. Appendix D 8: read the committed limit at the next decision,
    // without reloading Goal or waiting for a settings.changed callback.
    fixture.submit_report_on(&id, "completed", "done").await;
    wait_goal(&session, "main", r#"op="completed""#).await;
    fixture.idle_on(&id, "main").await;
    fixture.set_limit(3).await;
    fixture.goal_on(&id, "resume", json!({})).await.unwrap();
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let writes = goal_writes(&session, "main").await;
            let limits = writes
                .iter()
                .filter(|block| block.marker.contains(r#"reason="limit""#))
                .count();
            if limits == 2 {
                assert!(
                    writes
                        .iter()
                        .any(|block| block.marker.contains(r#"count="3" limit="3""#)),
                    "{writes:#?}"
                );
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the resumed goal pauses at the new limit");
    fixture.idle_on(&id, "main").await;

    // Appendix D 8: an unloaded Session is restored by a goal method, which
    // writes.
    assert!(session.unload_if_idle().await.unwrap());
    drop(session);
    fixture.goal_on(&id, "pause", json!({})).await.unwrap();
    let session = fixture.agent.get_session(&id).unwrap();
    wait_goal(&session, "main", r#"op="pause" reason="user""#).await;
    fixture.idle_on(&id, "main").await;

    // Appendix D 7: a fork writes `session/forked` after the copied facts.
    // The forked `main`'s goal starts empty: one start `clear` after that
    // fact, and no continuation.
    let at = session.sequence("main").unwrap();
    let fork_id = fixture.fork_on(&id, at).await;
    let forked = fixture.agent.get_session(&fork_id).unwrap();
    wait_goal(&forked, "main", r#"reason="new_branch""#).await;
    fixture.idle_on(&fork_id, "main").await;
    let events = branch_events(&forked, "main", ..).await.unwrap();
    let opening = events
        .iter()
        .find(|event| matches!(&event.kind, EventKind::SessionForked(_)))
        .expect("the fork's fact")
        .sequence;
    let writes = goal_writes(&forked, "main").await;
    let own: Vec<&Block> = writes
        .iter()
        .filter(|block| block.sequence > opening)
        .collect();
    assert_eq!(own.len(), 1, "{writes:#?}");
    assert!(
        own[0]
            .marker
            .contains(&format!(r#"reason="new_branch" after="{opening}""#)),
        "{own:#?}"
    );
    assert_eq!(
        goal_blocks(&forked, "main", r#"reason="new_branch""#).await,
        texts::start_clear("main", opening, (false, false)),
        "the fork's new-branch clear is one retired record"
    );
    assert!(
        goal_writes(&session, "main")
            .await
            .iter()
            .all(|block| !block.marker.contains("new_branch"))
    );
    fixture.close().await;

    // Appendix D 8: an assembly without Goal has no goal methods.
    let mut fixture = Fixture::start_without_goal().await;
    let id = fixture.new_session(&[]).await;
    let error = fixture
        .goal_on(&id, "set", json!({"objective": "x"}))
        .await
        .unwrap_err();
    assert_eq!(dim_client::app_error(&error), Some("unknown_method"));
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn goal_on_a_line_behaves_as_on_main() {
    let mut fixture = Fixture::start().await;
    fixture.set_limit(1000).await;
    let id = fixture
        .new_session(&["goal_update", "wait", "branch"])
        .await;
    let session = fixture.agent.get_session(&id).unwrap();
    let at = session.sequence("main").unwrap();
    let created = fixture
        .client
        .rpc(
            "session.branch_create",
            json!({"sessionId":id,"parentBranchId":"main","at":at,"prompt":"line work"}),
        )
        .await
        .unwrap();
    let child = created["branch"]["branchId"].as_str().unwrap().to_owned();
    fixture.idle_on(&id, &child).await;
    let opening = branch_events(&session, &child, ..)
        .await
        .unwrap()
        .iter()
        .find_map(|event| match &event.kind {
            EventKind::InputMessage(data) => data.blocks.first().map(|block| {
                let dim_agent_types::InputBlock::Raw { text, .. } = block;
                (text.clone(), data.user.clone())
            }),
            _ => None,
        })
        .expect("the line's opening input");
    let marker = format!(r#"<dim-branch-line branch="{child}" parent="main" source="host">"#);
    assert!(opening.0.starts_with(&marker), "{}", opening.0);
    assert_eq!(
        first_user_text(&session, &child).await,
        "line work",
        "the creator's prompt stays the opening input's words"
    );
    let line_set = fixture
        .goal_on(
            &id,
            "set",
            json!({"branchId": &child, "objective": "line objective"}),
        )
        .await
        .unwrap();
    let line_trigger = wait_goal(&session, &child, r#"op="set""#).await;
    assert_eq!(
        goal_blocks(&session, &child, r#"op="set""#).await,
        vec![texts::set(&child, "line objective")],
        "the line's set"
    );
    let _ = line_set;
    wait_goal(
        &session,
        &child,
        &format!(
            r#"kind="state" status="active" trigger="{}""#,
            line_trigger.submission_id.as_deref().unwrap()
        ),
    )
    .await;
    wait_goal(&session, &child, r#"count="1""#).await;
    fixture.idle_on(&id, &child).await;
    let operator = session
        .submit(
            "keep going".into(),
            dim_agent_types::InputMode::Queue,
            &child,
        )
        .await
        .unwrap()
        .sequence;
    let (reset, expected) = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let writes = goal_writes(&session, &child).await;
            let reset = writes.iter().find(|block| {
                block.marker.contains(r#"kind="continuation""#)
                    && after_of(&block.marker).is_some_and(|after| after > operator)
            });
            if let Some(block) = reset {
                let raced = writes
                    .iter()
                    .filter(|other| {
                        other.marker.contains(r#"kind="continuation""#)
                            && other.sequence > operator
                            && other.sequence < block.sequence
                    })
                    .count();
                return (block.marker.clone(), 1 + raced);
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("a continuation after the operator input");
    assert!(
        reset.contains(&format!(r#"count="{expected}""#)),
        "the continuation after the operator input counts {expected}: {reset}"
    );
    let after = after_of(&reset).expect("the continuation's after");
    // The operator's input restarts the count; the continuation after it
    // states the streak as a literal, so a wrong streak fails here.
    let idle = 2;
    assert_eq!(
        goal_block(&session, &child, &format!(r#"count="{expected}""#)).await,
        texts::continuation(&child, expected, Some(1000), idle, after),
        "the line's continuation"
    );
    assert!(goal_writes(&session, "main").await.is_empty());
    fixture
        .goal_on(&id, "pause", json!({"branchId": &child}))
        .await
        .unwrap();
    wait_goal(&session, &child, r#"op="pause""#).await;
    fixture.idle_on(&id, &child).await;
    assert_eq!(
        goal_block(&session, &child, r#"op="pause""#).await,
        texts::pause(&child)
    );
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn goal_evaluator_judge_creates_an_evaluator_and_completes_on_done() {
    let mut fixture = Fixture::start_with(Arc::new(EvaluatingModel)).await;
    fixture.set_limit(5).await;
    let id = fixture.new_session(&["goal_update", "wait"]).await;
    let session = fixture.agent.get_session(&id).unwrap();
    let judged = fixture
        .goal_on(
            &id,
            "set",
            json!({"objective": "make the tests pass", "judge": "evaluator"}),
        )
        .await
        .unwrap();
    let judged_trigger = wait_goal(&session, "main", r#"op="set""#).await;
    assert_eq!(
        goal_blocks(&session, "main", r#"op="set""#).await,
        vec![texts::evaluator_set("main", "make the tests pass")],
        "the judged set"
    );
    let _ = judged;
    wait_goal(
        &session,
        "main",
        &format!(
            r#"kind="state" status="active" judge="evaluator" trigger="{}""#,
            judged_trigger.submission_id.as_deref().unwrap()
        ),
    )
    .await;
    fixture
        .submit_report_on(&id, "completed", "all tests pass")
        .await;
    wait_event(&session, "main", |kind| {
        matches!(kind, EventKind::BranchCreate(_))
    })
    .await;
    fixture.idle_on(&id, "main").await;
    let creates = branch_creates(&session, "main").await;
    assert_eq!(creates.len(), 1, "{creates:#?}");
    let (created, creation) = &creates[0];
    let evaluation = turn_ends(&session, "main")
        .await
        .last()
        .copied()
        .expect("a turn end");
    assert_eq!(
        creation.source,
        BranchSource::Host {
            operation_id: format!("goal:evaluate:{evaluation}")
        },
        "{creation:#?}"
    );
    let writes = goal_writes(&session, "main").await;
    let request = writes
        .iter()
        .find(|block| block.marker.contains(r#"kind="goal" op="completed""#))
        .unwrap_or_else(|| panic!("the worker's request: {writes:#?}"));
    assert!(
        *created > request.sequence,
        "the request precedes the creation: {writes:#?}"
    );
    assert!(
        writes
            .iter()
            .all(|block| !block.marker.contains("continuation")),
        "{writes:#?}"
    );
    let child = &creation.branch;
    let prompt = first_user_text(&session, child).await;
    assert_eq!(
        prompt,
        texts::evaluator_prompt("main", evaluation, "make the tests pass", "all tests pass"),
        "the evaluator's prompt"
    );
    wait_goal(&session, "main", r#"kind="state" status="completed""#).await;
    fixture.idle_on(&id, "main").await;
    let conclusion = conclusion_sequence(&session, "main").await;
    let done = goal_block(&session, "main", r#"kind="state" status="completed""#).await;
    assert_eq!(
        done,
        texts::evaluated(
            "main",
            "make the tests pass",
            "completed",
            "all tests pass",
            conclusion
        ),
        "the evaluator's completion"
    );
    // The conclusion it decides on is one input of one block, and that
    // block's marker is the report of this evaluation and evaluator
    // (Pursuit §5).
    let conclusions: Vec<(usize, String)> = branch_events(&session, "main", ..)
        .await
        .unwrap()
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::InputMessage(data) => data.blocks.first().and_then(|block| {
                let dim_agent_types::InputBlock::Raw { text, .. } = block;
                text.starts_with(r#"<dim-goal kind="report""#)
                    .then(|| (data.blocks.len(), text.clone()))
            }),
            _ => None,
        })
        .collect();
    assert_eq!(conclusions.len(), 1, "{conclusions:#?}");
    assert_eq!(
        conclusions[0].0, 1,
        "the conclusion input carries the report alone"
    );
    assert_eq!(
        conclusions[0].1,
        texts::conclusion("main", child, evaluation, "done", "DONE: all tests pass"),
        "the conclusion input"
    );
    let writes = goal_writes(&session, "main").await;
    assert_eq!(
        writes
            .iter()
            .filter(|block| block.marker.contains(r#"kind="state" status="completed""#))
            .count(),
        1,
        "{writes:#?}"
    );
    wait_event(
        &session,
        "main",
        |kind| matches!(kind, EventKind::BranchEnded(data) if data.branch == *child),
    )
    .await;
    fixture.idle_on(&id, "main").await;
    let head = session.sequence(child).unwrap();
    assert_eq!(session.ended_head(child), Some(head));
    let line_events = session
        .session_service::<TreeCommands>()
        .unwrap()
        .page(child.to_owned(), 0, 256, head)
        .await
        .unwrap();
    assert_eq!(
        line_events
            .iter()
            .filter(|event| matches!(event.kind, EventKind::TurnEnd(_)))
            .count(),
        1,
        "one evaluator turn"
    );
    let endings: Vec<Value> = branch_events(&session, "main", ..)
        .await
        .unwrap()
        .iter()
        .filter(
            |event| matches!(&event.kind, EventKind::BranchEnded(data) if data.branch == *child),
        )
        .map(|event| serde_json::to_value(&event.kind).unwrap())
        .collect();
    assert_eq!(endings.len(), 1, "{endings:#?}");
    assert_eq!(
        endings[0]["data"]["source"],
        json!({"kind": "host", "operation_id": format!("goal:evaluate:{evaluation}")}),
        "{endings:#?}"
    );
    assert_eq!(endings[0]["data"]["branch"], *child, "{endings:#?}");
    let line = fixture
        .client
        .rpc(
            "session.history",
            json!({"sessionId":id,"branchId":child,"after":0,"limit":256}),
        )
        .await
        .unwrap();
    assert_eq!(line["done"], true, "{line:#?}");
    assert!(
        line["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| event["type"] == "branch"
                && event["name"] == "end"
                && event["data"]["source"]["operation_id"]
                    == format!("goal:evaluate:{evaluation}")),
        "{line:#?}"
    );
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn goal_evaluator_restart_writes_no_second_result() {
    let mut fixture = Fixture::start_with(Arc::new(WaitingModel)).await;
    fixture.set_limit(5).await;
    let id = fixture.new_session(&["goal_update", "wait"]).await;
    let session = fixture.agent.get_session(&id).unwrap();
    fixture
        .goal_on(
            &id,
            "set",
            json!({"objective": "ship it", "judge": "evaluator"}),
        )
        .await
        .unwrap();
    fixture.submit_report_on(&id, "completed", "done").await;
    wait_event(&session, "main", |kind| {
        matches!(kind, EventKind::BranchCreate(_))
    })
    .await;
    let creates = branch_creates(&session, "main").await;
    let child = creates[0].1.branch.clone();
    wait_event(&session, &child, |kind| {
        matches!(kind, EventKind::ToolCall(_))
    })
    .await;
    drop(session);
    let mut fixture = fixture.restart(Arc::new(EvaluatingModel)).await;
    fixture.submit_on(&id, "main", "back again").await;
    fixture.idle_on(&id, "main").await;
    let session = fixture.agent.get_session(&id).unwrap();
    assert_eq!(branch_creates(&session, "main").await.len(), 1);
    let writes = goal_writes(&session, "main").await;
    assert!(
        writes
            .iter()
            .all(|block| !block.marker.contains(r#"kind="state" status="completed""#)),
        "a replayed turn end reports nothing: {writes:#?}"
    );
    assert!(
        writes
            .iter()
            .all(|block| !block.marker.contains(r#"reason="evaluation_failed""#)),
        "{writes:#?}"
    );
    fixture.submit_on(&id, &child, "carry on").await;
    wait_goal(&session, "main", r#"kind="state" status="completed""#).await;
    fixture.idle_on(&id, "main").await;
    let writes = goal_writes(&session, "main").await;
    assert_eq!(
        writes
            .iter()
            .filter(|block| block.marker.contains(r#"kind="state" status="completed""#))
            .count(),
        1,
        "{writes:#?}"
    );
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn goal_evaluator_asks_again_and_continues() {
    let mut fixture = Fixture::start_with(Arc::new(RevisingModel::default())).await;
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
    fixture
        .submit_report_on(&id, "completed", "all tests pass")
        .await;
    wait_event(&session, "main", |kind| {
        matches!(kind, EventKind::BranchCreate(_))
    })
    .await;
    let evaluation: u64 = evaluation_of(&session, "main").await.parse().unwrap();
    let child = branch_creates(&session, "main").await[0].1.branch.clone();

    // The first evaluator turn ends without a conclusion: Goal asks again on
    // the evaluator's own line.
    wait_goal(&session, &child, r#"kind="evaluate-again""#).await;
    let first_end = turn_ends(&session, &child).await[0];
    assert_eq!(
        goal_block(&session, &child, r#"kind="evaluate-again""#).await,
        texts::evaluate_again(&child, first_end),
        "the evaluator's follow-up"
    );

    // The second ends with CONTINUE: its report decides the evaluation, and
    // the working branch continues.
    let needle = format!(r#"evaluation="{evaluation}""#);
    wait_goal(&session, "main", &needle).await;
    fixture.idle_on(&id, "main").await;
    assert_eq!(
        goal_block(&session, "main", r#"kind="report""#).await,
        texts::conclusion(
            "main",
            &child,
            evaluation,
            "continue",
            "CONTINUE:\n- tests still fail"
        ),
        "the CONTINUE report"
    );
    let continuation = goal_block(&session, "main", &needle).await;
    let count = count_of(&continuation).expect("the continuation's count");
    let after = after_of(&continuation).expect("the continuation's after");
    assert_eq!(
        continuation,
        texts::evaluated_continuation("main", count, Some(5), 0, after, evaluation),
        "the evaluator's continuation"
    );
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn goal_evaluator_blocks_or_fails_the_evaluation() {
    // An evaluator that reports BLOCKED completes the goal as blocked.
    let mut fixture = Fixture::start_with(Arc::new(BlockedModel)).await;
    fixture.set_limit(5).await;
    let id = fixture.new_session(&["goal_update", "wait"]).await;
    let session = fixture.agent.get_session(&id).unwrap();
    fixture
        .goal_on(
            &id,
            "set",
            json!({"objective": "ship it", "judge": "evaluator"}),
        )
        .await
        .unwrap();
    fixture.submit_report_on(&id, "completed", "done").await;
    wait_event(&session, "main", |kind| {
        matches!(kind, EventKind::BranchCreate(_))
    })
    .await;
    let evaluation: u64 = evaluation_of(&session, "main").await.parse().unwrap();
    let child = branch_creates(&session, "main").await[0].1.branch.clone();
    wait_goal(&session, "main", r#"kind="state" status="blocked""#).await;
    fixture.idle_on(&id, "main").await;
    let conclusion = conclusion_sequence(&session, "main").await;
    assert_eq!(
        goal_block(&session, "main", r#"kind="state" status="blocked""#).await,
        texts::evaluated("main", "ship it", "blocked", "need credentials", conclusion),
        "the evaluator's block"
    );
    assert_eq!(
        goal_block(&session, "main", r#"kind="report""#).await,
        texts::conclusion(
            "main",
            &child,
            evaluation,
            "blocked",
            "BLOCKED: need credentials"
        ),
        "the BLOCKED report"
    );
    fixture.close().await;

    // An evaluator whose turn fails pauses the goal with `evaluation_failed`.
    let mut fixture = Fixture::start_with(Arc::new(FailingEvaluator)).await;
    fixture.set_limit(5).await;
    let id = fixture.new_session(&["goal_update", "wait"]).await;
    let session = fixture.agent.get_session(&id).unwrap();
    fixture
        .goal_on(
            &id,
            "set",
            json!({"objective": "ship it", "judge": "evaluator"}),
        )
        .await
        .unwrap();
    fixture.submit_report_on(&id, "completed", "done").await;
    wait_event(&session, "main", |kind| {
        matches!(kind, EventKind::BranchCreate(_))
    })
    .await;
    let evaluation: u64 = evaluation_of(&session, "main").await.parse().unwrap();
    let child = branch_creates(&session, "main").await[0].1.branch.clone();
    wait_goal(&session, "main", r#"reason="evaluation_failed""#).await;
    fixture.idle_on(&id, "main").await;
    let conclusion = conclusion_sequence(&session, "main").await;
    assert_eq!(
        goal_block(&session, "main", r#"reason="evaluation_failed""#).await,
        texts::evaluated_pause(
            "main",
            "ship it",
            "evaluation_failed",
            "the evaluator could not check the result",
            conclusion
        ),
        "the failed evaluation"
    );
    assert_eq!(
        goal_block(&session, "main", r#"kind="report""#).await,
        texts::conclusion(
            "main",
            &child,
            evaluation,
            "failed",
            "the evaluator's turn did not complete"
        ),
        "the evaluator's failure report"
    );
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn goal_turn_failure_pauses_the_goal() {
    let mut fixture = Fixture::start_with(Arc::new(FailingModel)).await;
    fixture.set_limit(5).await;
    let id = fixture.new_session(&["goal_update", "wait"]).await;
    let session = fixture.agent.get_session(&id).unwrap();
    fixture
        .submit_on(&id, "main", r#"tool:wait {"millis":1500}"#)
        .await;
    wait_event(&session, "main", |kind| {
        matches!(kind, EventKind::ToolCall(_))
    })
    .await;
    fixture
        .goal_on(&id, "set", json!({"objective": "ship"}))
        .await
        .unwrap();
    fixture.submit_on(&id, "main", "now fail").await;
    wait_goal(&session, "main", r#"reason="turn_failed""#).await;
    fixture.idle_on(&id, "main").await;
    let failed = turn_ends(&session, "main")
        .await
        .last()
        .copied()
        .expect("a turn end");
    assert_eq!(
        goal_block(&session, "main", r#"reason="turn_failed""#).await,
        texts::automatic_pause(
            "main",
            "ship",
            "turn_failed",
            "the last turn failed",
            failed
        ),
        "the failed turn's pause"
    );
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn goal_registers_the_entry_for_the_marker_it_writes() {
    let mut fixture = Fixture::start().await;
    fixture.set_limit(1).await;

    // `session.goal.set` writes the `<dim-goal` input and drives a turn.
    let id = fixture.new_session(&["goal_update", "wait"]).await;
    let session = fixture.agent.get_session(&id).unwrap();
    fixture
        .goal_on(&id, "set", json!({"objective": "ship it"}))
        .await
        .unwrap();
    wait_goal(&session, "main", r#"reason="limit""#).await;
    fixture.idle_on(&id, "main").await;

    // The request that carries the `<dim-goal` input carries the entry too
    // (A.5.1): the Session part registers it for the Session, so every
    // branch's System holds it while the row is loaded.
    let request = last_request(&session, "main").await;
    assert!(
        serde_json::to_string(&request.messages)
            .unwrap()
            .contains("<dim-goal "),
        "the last request carried no <dim-goal input: {request:#?}"
    );
    let system = last_system(&session, "main").await;
    assert!(system.contains(texts::ENTRY), "{system:?}");

    fixture.close().await;
}

/// Records every request and echoes, so a change's first request can be read
/// once the branch settles.
struct RecordingModel {
    requests: Mutex<Vec<Arc<ModelRequest>>>,
}

impl Default for RecordingModel {
    fn default() -> Self {
        Self {
            requests: Mutex::new(Vec::new()),
        }
    }
}

impl ModelDriver for RecordingModel {
    fn invoke(
        &self,
        request: Arc<ModelRequest>,
        connection: ModelConnection,
        cancel: CancellationToken,
    ) -> HostFuture<'_, ModelStream> {
        self.requests.lock().unwrap().push(request.clone());
        EchoModel.invoke(request, connection, cancel)
    }
}

/// R5: a change and the record that answers it commit as one atomic batch,
/// the record first, so the change's first turn starts with the record
/// already ahead of its trigger. `set` on a goal-free branch and `resume`
/// from paused; twenty runs each, because the guarantee is the order of the
/// batch's two inputs, not one timing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn goal_a_changes_first_request_holds_its_record_before_the_trigger() {
    let mut runs = 0;
    for case in ["set", "resume"] {
        for run in 0..20 {
            let model = Arc::new(RecordingModel::default());
            let mut fixture = Fixture::start_with(model.clone()).await;
            fixture.set_limit(1).await;
            let id = fixture.id.clone();
            if case == "resume" {
                fixture
                    .goal("set", json!({"objective": "ship it"}))
                    .await
                    .unwrap();
                fixture.idle_on(&id, "main").await;
                fixture.goal("pause", json!({})).await.unwrap();
                fixture.idle_on(&id, "main").await;
            }
            let before = model.requests.lock().unwrap().len();
            let params = if case == "set" {
                json!({"objective": "ship it"})
            } else {
                json!({})
            };
            let sequence = fixture
                .goal(case, params)
                .await
                .unwrap_or_else(|error| panic!("{case} run {run}: {error}"));
            fixture.idle_on(&id, "main").await;

            // The trigger event names itself by submission id; the record the
            // change wrote names that id in its `trigger` attribute.
            let events = fixture.events("main", sequence - 1).await;
            let trigger = events
                .iter()
                .find(|event| event.sequence == sequence)
                .unwrap_or_else(|| panic!("{case} run {run}: the trigger event"));
            let EventKind::InputMessage(data) = &trigger.kind else {
                panic!("{case} run {run}: the trigger is not an input");
            };
            let submission = data
                .submission_id
                .clone()
                .unwrap_or_else(|| panic!("{case} run {run}: the trigger's submission id"));
            let record = texts::set_state(
                "main",
                "ship it",
                &texts::Answer::Trigger(submission.clone()),
                false,
            );
            let trigger_text = if case == "set" {
                texts::set("main", "ship it")
            } else {
                texts::resume("main")
            };

            let text = {
                let requests = model.requests.lock().unwrap();
                let first = requests.get(before).unwrap_or_else(|| {
                    panic!(
                        "{case} run {run}: the change drove no request ({} recorded)",
                        requests.len()
                    )
                });
                request_text(first)
            };
            let record_at = text.find(&record).unwrap_or_else(|| {
                panic!("{case} run {run}: the first request holds no record: {text}")
            });
            let trigger_at = text.find(&trigger_text).unwrap_or_else(|| {
                panic!("{case} run {run}: the first request holds no trigger: {text}")
            });
            assert!(
                record_at < trigger_at,
                "{case} run {run}: the record follows its trigger: {text}"
            );
            runs += 1;
            fixture.close().await;
        }
    }
    assert_eq!(runs, 40, "every case ran twenty times");
}

/// The text of a request's messages, as one string: the inputs the model read.
fn request_text(request: &ModelRequest) -> String {
    request
        .messages
        .iter()
        .filter_map(|message| match &message.content {
            MessageContent::Text(text) => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Records every request; the first holds until the flow releases it, then
/// asks for the `wait` tool, and the later ones answer with text.
struct OrderingModel {
    requests: Mutex<Vec<Arc<ModelRequest>>>,
    entered: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
    calls: AtomicUsize,
}
impl Default for OrderingModel {
    fn default() -> Self {
        Self {
            requests: Mutex::new(Vec::new()),
            entered: Arc::new(tokio::sync::Notify::new()),
            release: Arc::new(tokio::sync::Notify::new()),
            calls: AtomicUsize::new(0),
        }
    }
}
impl ModelDriver for OrderingModel {
    fn invoke(
        &self,
        request: Arc<ModelRequest>,
        _connection: ModelConnection,
        _cancel: CancellationToken,
    ) -> HostFuture<'_, ModelStream> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        self.requests.lock().unwrap().push(request);
        let entered = self.entered.clone();
        let release = self.release.clone();
        Box::pin(async move {
            if call == 0 {
                entered.notify_one();
                release.notified().await;
                let call = ToolCall {
                    id: "wait-1".into(),
                    name: "wait".into(),
                    input: json!({"millis": 10}).as_object().unwrap().clone(),
                };
                return Ok(Box::pin(futures_util::stream::iter(vec![
                    Ok(ModelChunk::MessageStart {
                        message_id: "m1".into(),
                    }),
                    Ok(ModelChunk::ToolStart {
                        id: call.id.clone(),
                        name: call.name.clone(),
                    }),
                    Ok(ModelChunk::ToolUse(call)),
                    Ok(ModelChunk::MessageStop {
                        message_id: "m1".into(),
                        reason: ModelStopReason::ToolUse,
                    }),
                ])) as ModelStream);
            }
            Ok(Box::pin(futures_util::stream::iter(vec![
                Ok(ModelChunk::MessageStart {
                    message_id: "m2".into(),
                }),
                Ok(ModelChunk::Text("ok".into())),
                Ok(ModelChunk::TextEnd),
                Ok(ModelChunk::MessageStop {
                    message_id: "m2".into(),
                    reason: ModelStopReason::EndTurn,
                }),
            ])) as ModelStream)
        })
    }
}

/// Holds the calls whose index is in `hold`; a held call asks for the `wait`
/// tool once released, and the others answer with text.
struct ScheduledModel {
    hold: Vec<usize>,
    requests: Mutex<Vec<Arc<ModelRequest>>>,
    entered: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
    calls: AtomicUsize,
}

impl ScheduledModel {
    fn new(hold: Vec<usize>) -> Self {
        Self {
            hold,
            requests: Mutex::new(Vec::new()),
            entered: Arc::new(tokio::sync::Notify::new()),
            release: Arc::new(tokio::sync::Notify::new()),
            calls: AtomicUsize::new(0),
        }
    }
}

impl ModelDriver for ScheduledModel {
    fn invoke(
        &self,
        request: Arc<ModelRequest>,
        _connection: ModelConnection,
        _cancel: CancellationToken,
    ) -> HostFuture<'_, ModelStream> {
        let index = self.calls.fetch_add(1, Ordering::SeqCst);
        let hold = self.hold.contains(&index);
        self.requests.lock().unwrap().push(request);
        let entered = self.entered.clone();
        let release = self.release.clone();
        Box::pin(async move {
            let message_id = format!("m{index}");
            if hold {
                entered.notify_one();
                release.notified().await;
                let call = ToolCall {
                    id: format!("wait-{index}"),
                    name: "wait".into(),
                    input: json!({"millis": 10}).as_object().unwrap().clone(),
                };
                return Ok(Box::pin(futures_util::stream::iter(vec![
                    Ok(ModelChunk::MessageStart {
                        message_id: message_id.clone(),
                    }),
                    Ok(ModelChunk::ToolStart {
                        id: call.id.clone(),
                        name: call.name.clone(),
                    }),
                    Ok(ModelChunk::ToolUse(call)),
                    Ok(ModelChunk::MessageStop {
                        message_id,
                        reason: ModelStopReason::ToolUse,
                    }),
                ])) as ModelStream);
            }
            Ok(Box::pin(futures_util::stream::iter(vec![
                Ok(ModelChunk::MessageStart {
                    message_id: message_id.clone(),
                }),
                Ok(ModelChunk::Text("ok".into())),
                Ok(ModelChunk::TextEnd),
                Ok(ModelChunk::MessageStop {
                    message_id,
                    reason: ModelStopReason::EndTurn,
                }),
            ])) as ModelStream)
        })
    }
}

/// D1/D3: a Queue `set` held behind a running turn is overtaken by an Append
/// `pause`. The record Goal writes for the pause is the newest state the next
/// request, the fold and the clients read; the held `set` trigger is consumed
/// later but states no status, so nothing contradicts the record.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn goal_a_queued_set_overtaken_by_a_pause_reads_as_paused_everywhere() {
    let model = Arc::new(OrderingModel::default());
    let mut fixture = Fixture::start_with(model.clone()).await;
    fixture.set_limit(5).await;
    let id = fixture.new_session(&["goal_update", "wait"]).await;
    let session = fixture.agent.get_session(&id).unwrap();

    fixture.submit_on(&id, "main", "work").await;
    tokio::time::timeout(Duration::from_secs(15), model.entered.notified())
        .await
        .expect("the first request");

    // The Queue `set` is held while the turn runs; the Append `pause` passes
    // it and is released at once.
    let set = fixture
        .goal_on(&id, "set", json!({"objective": "ship it"}))
        .await
        .unwrap();
    let pause = fixture.goal_on(&id, "pause", json!({})).await.unwrap();
    assert!(set < pause);
    let pause_trigger = wait_goal(&session, "main", r#"op="pause" reason="user""#).await;
    let record = wait_goal(
        &session,
        "main",
        &format!(
            r#"kind="state" status="paused" reason="user" trigger="{}""#,
            pause_trigger.submission_id.as_deref().unwrap()
        ),
    )
    .await;
    assert_eq!(
        record.text,
        texts::paused(
            "main",
            "ship it",
            "user",
            "",
            &texts::trigger(&pause_trigger),
            false
        )
    );

    // The request the running turn builds next reads the paused record last,
    // and the held `set` is not in it.
    model.release.notify_one();
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let len = model.requests.lock().unwrap().len();
            if len >= 2 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the second request");
    let request = request_text(&model.requests.lock().unwrap()[1]);
    let newest = request
        .rsplit(r#"<dim-goal kind="state""#)
        .next()
        .expect("the split");
    assert!(newest.contains(r#"status="paused""#), "{request}");
    assert!(
        !request.contains(r#"<dim-goal kind="goal" op="set""#),
        "the held set stays out of the running turn: {request}"
    );
    let newest_at = request
        .rfind(r#"<dim-goal kind="state""#)
        .expect("a state record in the request");
    if let Some(active_at) = request.find("The goal for branch main is active.") {
        assert!(
            active_at < newest_at,
            "every record stating the goal active precedes the newest one: {request}"
        );
    }

    // The fold and the clients read the same state: one live record, paused,
    // and the held set's turn end continues nothing.
    fixture.idle_on(&id, "main").await;
    let reply = fixture
        .client
        .rpc("session.keys", json!({"sessionId":id,"branchId":"main"}))
        .await
        .unwrap();
    let entries = reply["entries"].as_array().unwrap();
    let added: Vec<&Value> = entries
        .iter()
        .filter(|entry| entry["key"] == "goal:state")
        .collect();
    assert_eq!(added.len(), 1, "{entries:#?}");
    assert_eq!(added[0]["text"].as_str(), Some(record.text.as_str()));
    let writes = goal_writes(&session, "main").await;
    assert!(
        writes
            .iter()
            .all(|block| !block.marker.contains(r#"kind="continuation""#)),
        "a paused goal continues nothing: {writes:#?}"
    );
    fixture.close().await;
}

/// D3: a compaction inside a turn with a goal active carries the state record
/// from the compaction baseline, and Goal writes nothing for the compaction.
#[tokio::test]
async fn goal_a_compaction_inside_a_turn_carries_the_record_from_the_baseline() {
    let model = Arc::new(ScheduledModel::new(vec![0, 2, 3]));
    let mut fixture = Fixture::start_with(model.clone()).await;
    fixture.set_limit(1).await;
    let id = fixture.new_session(&["goal_update", "wait"]).await;
    let session = fixture.agent.get_session(&id).unwrap();

    // Turn 1: the Queue `set` drives it, Goal answers inside it, and the
    // turn's second request consumes the record.
    let set = fixture
        .goal_on(&id, "set", json!({"objective": "ship it"}))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(15), model.entered.notified())
        .await
        .expect("the request the set drove");
    let set_trigger = wait_goal(&session, "main", r#"op="set""#).await;
    let _ = set;
    let record = wait_goal(
        &session,
        "main",
        &format!(
            r#"kind="state" status="active" trigger="{}""#,
            set_trigger.submission_id.as_deref().unwrap()
        ),
    )
    .await;
    model.release.notify_one();
    // The turn ends with the record behind it; Goal continues the goal and
    // turn 2's first request is held.
    tokio::time::timeout(Duration::from_secs(15), model.entered.notified())
        .await
        .expect("the continuation's request");

    // Turn 2 runs whole: the cut is inside it and the record is behind it, so
    // only the baseline carries the record.
    let step = fixture
        .probe
        .emit("main", EventKind::ContextCompact(ContextCompactData {}));
    let compaction = fixture.probe.emit(
        "main",
        EventKind::ContextCompaction(ContextCompactionData {
            turn_id: None,
            request_id: None,
            command: None,
            start: None,
            through: step.sequence,
            outcome: ContextCompactionResult::Completed {
                retain_from: step.id.clone(),
                replacement: vec![summary_message("compacted")],
                strategy: "test".into(),
                diagnostics: Box::new(
                    serde_json::from_value(json!({
                        "reason":"auto", "trigger":"threshold", "estimatedInputTokens":1,
                        "entriesReplaced":1, "modelCallCount":1, "durationMs":1,
                        "strategy":{"kind":"test"}
                    }))
                    .unwrap(),
                ),
                context_baseline: vec![ContextBaselineEntry {
                    key: "goal:state".into(),
                    source_event: record.id.clone(),
                    message: Arc::new(Message::text(Role::User, record.text.clone())),
                }],
                replacement_parts: None,
                pinned_inputs: None,
                digest_entries: None,
            },
        }),
    );
    let writes: Vec<(u64, String)> = goal_writes(&session, "main")
        .await
        .into_iter()
        .map(|block| (block.sequence, block.text))
        .collect();

    // The next request of the turn is rendered from the compaction and
    // carries the snapshot from the baseline, exactly once.
    model.release.notify_one();
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if model.requests.lock().unwrap().len() >= 4 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the request after the compaction");
    let request = request_text(&model.requests.lock().unwrap()[3]);
    assert!(request.contains("compacted"), "{request}");
    assert_eq!(
        request.matches(&record.text).count(),
        1,
        "the record is carried from the baseline once: {request}"
    );

    // Nothing about the compaction is Goal's to answer.
    let after: Vec<(u64, String)> = goal_writes(&session, "main")
        .await
        .into_iter()
        .map(|block| (block.sequence, block.text))
        .collect();
    assert_eq!(after, writes, "Goal wrote nothing for the compaction");
    assert!(
        after
            .iter()
            .all(|(sequence, _)| *sequence < compaction.sequence),
        "every Goal write precedes the compaction: {after:#?}"
    );

    // Turn 2 ends; the limit pauses the goal, and the test closes on a
    // settled branch.
    model.release.notify_one();
    wait_goal(
        &session,
        "main",
        r#"kind="state" status="paused" reason="limit""#,
    )
    .await;
    fixture.close().await;
}

/// D3: every change Goal answers leaves exactly one record, naming the change
/// it answers, and `session.keys` shows that record as the newest state.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn goal_each_change_answers_with_one_newest_state_record() {
    let mut fixture = Fixture::start().await;
    fixture.set_limit(5).await;
    let id = fixture.new_session(&["goal_update", "wait"]).await;
    let session = fixture.agent.get_session(&id).unwrap();

    let steps = [
        ("set", json!({"objective": "ship it"})),
        ("edit", json!({"objective": "ship it twice"})),
        ("pause", json!({})),
        ("resume", json!({})),
    ];
    for (method, params) in steps {
        let sequence = fixture.goal_on(&id, method, params).await.unwrap();
        let trigger = wait_goal(&session, "main", &format!(r#"op="{method}""#)).await;
        assert_eq!(trigger.sequence, sequence, "{method}: the trigger");
        let needle = format!(r#"trigger="{}""#, trigger.submission_id.as_deref().unwrap());
        let record = wait_goal(&session, "main", &needle).await;
        let answers: Vec<Block> = goal_writes(&session, "main")
            .await
            .into_iter()
            .filter(|block| {
                block.marker.contains(r#"kind="state""#) && block.marker.contains(&needle)
            })
            .collect();
        assert_eq!(answers.len(), 1, "{method}: {answers:#?}");
        let reply = fixture
            .client
            .rpc("session.keys", json!({"sessionId":id,"branchId":"main"}))
            .await
            .unwrap();
        let entries = reply["entries"].as_array().unwrap();
        let states: Vec<&Value> = entries
            .iter()
            .filter(|entry| entry["key"] == "goal:state")
            .collect();
        assert_eq!(states.len(), 1, "{method}: {entries:#?}");
        assert_eq!(states[0]["text"].as_str(), Some(record.text.as_str()));
    }

    // The worker's report is a change too, and its record states the result.
    fixture.submit_report_on(&id, "completed", "done").await;
    let report = wait_goal(&session, "main", r#"op="completed""#).await;
    let record = wait_goal(
        &session,
        "main",
        &format!(
            r#"kind="state" status="completed" trigger="{}""#,
            report.submission_id.as_deref().unwrap()
        ),
    )
    .await;
    assert_eq!(
        record.text,
        texts::reported(
            "main",
            "ship it twice",
            "completed",
            "done",
            &texts::trigger(&report)
        )
    );

    // The user's clear retires the record; the key holds no live entry after.
    let clear = fixture.goal_on(&id, "clear", json!({})).await.unwrap();
    let clear_trigger = wait_goal(&session, "main", r#"op="clear""#).await;
    let record = wait_goal(
        &session,
        "main",
        &format!(
            r#"status="cleared" trigger="{}""#,
            clear_trigger.submission_id.as_deref().unwrap()
        ),
    )
    .await;
    let _ = clear;
    assert_eq!(
        record.text,
        texts::cleared("main", &texts::trigger(&clear_trigger))
    );
    let reply = fixture
        .client
        .rpc("session.keys", json!({"sessionId":id,"branchId":"main"}))
        .await
        .unwrap();
    let entries = reply["entries"].as_array().unwrap();
    assert!(
        entries.iter().all(|entry| entry["key"] != "goal:state"),
        "a retired record leaves no live entry: {entries:#?}"
    );
    fixture.close().await;
}

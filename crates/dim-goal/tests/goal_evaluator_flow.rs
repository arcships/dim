//! The `goal.test.mjs` Pursuit cases: the evaluator judge, its line, its
//! conclusions and the ends it records. The part harness supplies every fact
//! the Loop and TimelineTree would write; expectations are Goal's inputs.
use crate::support;

use std::time::Duration;

use dim_agent_types::{
    BranchCreateAt, BranchCreateData, BranchCreateFailedData, BranchEndedData, BranchSource,
    BranchStartData, EventKind, ExecEndedData, ExecStartedData, ExecStatus, InputBlock,
    InputConsumedData, InputMode, ToolResult, TurnStartData,
};
use serde_json::json;
use support::parts::*;
use support::texts;

/// Waits for the next `branch/create` on `branch`.
async fn next_create(env: &Env, branch: &str, seen: usize) -> BranchCreateData {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let creates = env.creates(branch).await;
            if let Some(create) = creates.into_iter().nth(seen) {
                return create;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("a branch/create")
}

/// A worker turn that ends with the goal's completion request, and the
/// evaluator Goal creates for it. The evaluator clears the inherited goal on
/// its own line, as every new line does; its parts are mounted on return.
async fn completion_requested(env: &Env) -> (u64, BranchCreateData) {
    completion_requested_with(env, None).await
}

/// [`completion_requested`] under the Settings limit `limit`.
async fn completion_requested_with(env: &Env, limit: Option<u64>) -> (u64, BranchCreateData) {
    env.goal("set", json!({"objective": "ship it", "judge": "evaluator"}))
        .await
        .expect("set writes");
    let set = env.wait_for("main", r#"op="set""#).await;
    let end = env.turn("main", &[&set]);
    let limit = limit
        .map(|limit| format!(r#" limit="{limit}""#))
        .unwrap_or_default();
    let continuation = env
        .wait_for(
            "main",
            &format!(
                r#"kind="continuation" count="1"{limit} after="{}""#,
                end.sequence
            ),
        )
        .await;
    env.turn_start("main", "work");
    env.consume("main", &continuation.id);
    env.tool(
        "main",
        json!({"status": "completed", "summary": "all tests pass"}),
    )
    .await;
    let end = env.end_turn("main", "work", "completed");
    let create = next_create(env, "main", 0).await;
    assert_eq!(
        create.source,
        BranchSource::Host {
            operation_id: format!("goal:evaluate:{}", end.sequence)
        }
    );
    let evaluator = create.branch.clone();
    tokio::time::timeout(Duration::from_secs(15), async {
        while !env.has_branch(&evaluator) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the evaluator's parts activate");
    env.wait_for(&evaluator, r#"reason="new_branch""#).await;
    (end.sequence, create)
}

/// Writes one conclusion about `evaluation` on `branch`, as the evaluator's
/// line reports it.
fn write_conclusion(env: &Env, evaluator: &str, evaluation: u64, result: &str, text: &str) -> u64 {
    let conclusion = texts::conclusion("main", evaluator, evaluation, result, text);
    env.write_message(
        "main",
        vec![raw(&conclusion, None, false)],
        InputMode::Append,
    )
    .sequence
}

/// The Goal writes on `branch`, newest first, that contain `needle`.
async fn writes_with(env: &Env, branch: &str, needle: &str) -> Vec<Block> {
    env.writes(branch)
        .await
        .into_iter()
        .filter(|block| block.text.contains(needle))
        .collect()
}

/// Waits for the evaluated continuation of `evaluation` on `branch`: the
/// conclusion input itself also names the evaluation.
async fn wait_evaluated(env: &Env, branch: &str, evaluation: u64) -> Block {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let writes = env.writes(branch).await;
            if let Some(block) = writes.iter().find(|block| {
                block.marker.contains(r#"kind="continuation""#)
                    && block
                        .marker
                        .contains(&format!(r#"evaluation="{evaluation}""#))
            }) {
                return block.clone();
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("an evaluated continuation")
}

/// The evaluated continuations of `evaluation` on `main`, as markers with
/// their sequences.
async fn evaluated_writes(env: &Env, evaluation: u64) -> Vec<String> {
    writes_with(env, "main", &format!(r#"evaluation="{evaluation}""#))
        .await
        .into_iter()
        .filter(|block| block.marker.contains(r#"kind="continuation""#))
        .map(|block| format!("{} at {}", block.marker, block.sequence))
        .collect()
}

/// The pending inputs of `branch`, as string slices for a turn.
async fn pending_refs(env: &Env, branch: &str) -> Vec<String> {
    env.pending_inputs(branch).await
}

/// Drives one completed turn on `branch` that takes in what is pending.
async fn line_turn(env: &Env, branch: &str, turn_id: &str, steps: &[(&str, &[&str])]) {
    let pending = pending_refs(env, branch).await;
    let pending: Vec<&str> = pending.iter().map(String::as_str).collect();
    env.turn_steps(branch, turn_id, &pending, "completed", steps);
}

#[tokio::test]
async fn pursuit_1_the_judge_is_recorded_kept_by_edit_and_a_completion_is_a_request() {
    let env = Env::start().await;
    env.goal("set", json!({"objective": "ship it", "judge": "evaluator"}))
        .await
        .expect("set writes");
    let set = env.wait_for("main", r#"op="set""#).await;
    assert_eq!(set.text, texts::evaluator_set("main", "ship it"));

    env.goal("edit", json!({"objective": "ship it twice"}))
        .await
        .expect("edit writes");
    let edit = env.wait_for("main", r#"op="edit""#).await;
    assert_eq!(edit.text, texts::edit("main", "ship it twice"));

    let end = env.turn("main", &[&set, &edit]);
    let continuation = env.wait_for("main", r#"kind="continuation""#).await;
    assert_eq!(
        continuation.text,
        texts::continuation("main", 1, None, 0, end.sequence)
    );

    // The completion is a request: the goal stays active and an evaluator
    // takes the turn end.
    env.turn_start("main", "work");
    env.consume("main", &continuation.id);
    env.tool(
        "main",
        json!({"status": "completed", "summary": "all tests pass"}),
    )
    .await;
    let end = env.end_turn("main", "work", "completed");
    let create = next_create(&env, "main", 0).await;
    assert!(
        writes_with(&env, "main", r#"status="completed""#)
            .await
            .is_empty(),
        "a request does not complete the goal"
    );

    // A later turn end without a request decides the limit, not an
    // evaluation.
    env.turn_start("main", "next");
    let end2 = env.end_turn("main", "next", "completed");
    let next = env
        .wait_for(
            "main",
            &format!(r#"kind="continuation" count="2" after="{}""#, end2.sequence),
        )
        .await;
    assert_eq!(
        next.text,
        texts::continuation("main", 2, None, 0, end2.sequence)
    );
    assert_eq!(env.creates("main").await.len(), 1);

    // clear returns to the model judge.
    env.goal("clear", json!({})).await.expect("clear writes");
    env.wait_for("main", r#"op="clear""#).await;
    env.goal("set", json!({"objective": "again"}))
        .await
        .expect("set writes");
    let again = env
        .writes("main")
        .await
        .into_iter()
        .rev()
        .find(|block| block.text.contains(r#"op="set""#))
        .expect("the second set");
    assert_eq!(again.text, texts::set("main", "again"));
    env.turn_start("main", "model");
    env.tool("main", json!({"status": "completed", "summary": "done"}))
        .await;
    env.end_turn("main", "model", "completed");
    let completed = env.wait_for("main", "Result:\ndone").await;
    assert_eq!(completed.text, texts::report("main", "completed", "done"));
    assert_eq!(env.creates("main").await.len(), 1);
    let _ = (end.sequence, create);
    env.close().await;
}

#[tokio::test]
async fn pursuit_2_7_10_a_conclusion_decides_once_from_its_own_evaluator() {
    // done: the evaluator completes the goal, once.
    let env = Env::start().await;
    let (evaluation, create) = completion_requested(&env).await;
    let evaluator = create.branch.clone();
    let conclusion_sequence = write_conclusion(
        &env,
        &evaluator,
        evaluation,
        "done",
        "DONE: all 3 tests pass",
    );
    let completed = env.wait_for("main", r#"status="completed""#).await;
    assert_eq!(
        completed.text,
        texts::evaluated(
            "main",
            "ship it",
            "completed",
            "all 3 tests pass",
            conclusion_sequence
        )
    );
    env.wait_for("main", r#"kind="report""#).await;
    let conclusion = env.written_input("main", r#"kind="report""#).await;
    assert_eq!(
        match &conclusion.blocks[0] {
            InputBlock::Raw { text, .. } => text.clone(),
        },
        texts::conclusion(
            "main",
            &evaluator,
            evaluation,
            "done",
            "DONE: all 3 tests pass"
        )
    );

    // The same conclusion again, and a foreign evaluator's, decide nothing.
    let before = env.settled("main").await.len();
    write_conclusion(&env, &evaluator, evaluation, "done", "DONE: again");
    write_conclusion(&env, "F", evaluation, "done", "DONE: forged");
    assert_eq!(env.settled("main").await.len(), before + 2);
    env.close().await;

    // blocked.
    let env = Env::start().await;
    let (evaluation, create) = completion_requested(&env).await;
    let conclusion_sequence = write_conclusion(
        &env,
        &create.branch,
        evaluation,
        "blocked",
        "BLOCKED: a deploy key",
    );
    let blocked = env.wait_for("main", r#"status="blocked""#).await;
    assert_eq!(
        blocked.text,
        texts::evaluated(
            "main",
            "ship it",
            "blocked",
            "a deploy key",
            conclusion_sequence
        )
    );
    env.close().await;

    // continue: one Pursuit continuation, with the Goal count.
    let env = Env::start().await;
    let (evaluation, create) = completion_requested(&env).await;
    write_conclusion(
        &env,
        &create.branch,
        evaluation,
        "continue",
        "CONTINUE:\n- two tests fail: <output>\n- the changelog",
    );
    let needle =
        format!(r#"kind="continuation" count="2" after="{evaluation}" evaluation="{evaluation}""#);
    let continuation = env.wait_for("main", &needle).await;
    assert_eq!(
        continuation.text,
        texts::evaluated_continuation("main", 2, None, 0, evaluation, evaluation)
    );
    env.close().await;

    // failed: the evaluation fails.
    let env = Env::start().await;
    let (evaluation, create) = completion_requested(&env).await;
    let conclusion_sequence = write_conclusion(
        &env,
        &create.branch,
        evaluation,
        "failed",
        "the evaluator's turn did not complete",
    );
    let pause = env.wait_for("main", r#"reason="evaluation_failed""#).await;
    assert_eq!(
        pause.text,
        texts::evaluated_pause(
            "main",
            "ship it",
            "evaluation_failed",
            "the evaluator could not check the result",
            conclusion_sequence
        )
    );
    env.close().await;

    // A conclusion after the user paused the goal decides nothing.
    let env = Env::start().await;
    let (evaluation, create) = completion_requested(&env).await;
    env.goal("pause", json!({})).await.expect("pause writes");
    env.wait_for("main", r#"op="pause""#).await;
    let before = env.settled("main").await.len();
    write_conclusion(&env, &create.branch, evaluation, "done", "DONE: late");
    assert_eq!(env.settled("main").await.len(), before + 1);
    env.close().await;
}

#[tokio::test]
async fn pursuit_4_8_the_report_is_the_last_conclusion_line_cut_to_4000() {
    // The reply's last conclusion line is the report.
    let env = Env::start().await;
    let (evaluation, create) = completion_requested(&env).await;
    line_turn(
        &env,
        &create.branch,
        "eval-1",
        &[("r1", &["Checked the tree.\nDONE: all 3 tests pass"])],
    )
    .await;
    env.wait_for("main", r#"kind="report""#).await;
    let conclusion = env.written_input("main", r#"kind="report""#).await;
    assert_eq!(
        match &conclusion.blocks[0] {
            InputBlock::Raw { text, .. } => text.clone(),
        },
        texts::conclusion(
            "main",
            &create.branch,
            evaluation,
            "done",
            "DONE: all 3 tests pass"
        )
    );
    env.close().await;

    // An earlier conclusion line loses to the last one.
    let env = Env::start().await;
    let (_, create) = completion_requested(&env).await;
    line_turn(
        &env,
        &create.branch,
        "eval-1",
        &[(
            "r1",
            &["DONE: early\nnot yet\nBLOCKED: the build host is offline"],
        )],
    )
    .await;
    let report = env.wait_for("main", r#"kind="report""#).await;
    let blocked = env.wait_for("main", r#"status="blocked""#).await;
    assert_eq!(
        blocked.text,
        texts::evaluated(
            "main",
            "ship it",
            "blocked",
            "the build host is offline",
            report.sequence
        )
    );
    env.close().await;

    // A multiline CONTINUE is reported whole.
    let env = Env::start().await;
    let (evaluation, create) = completion_requested(&env).await;
    line_turn(
        &env,
        &create.branch,
        "eval-1",
        &[("r1", &["x\nCONTINUE:\n- a: <out>\n- b"])],
    )
    .await;
    env.wait_for("main", r#"kind="report""#).await;
    let conclusion = env.written_input("main", r#"kind="report""#).await;
    assert_eq!(
        match &conclusion.blocks[0] {
            InputBlock::Raw { text, .. } => text.clone(),
        },
        texts::conclusion(
            "main",
            &create.branch,
            evaluation,
            "continue",
            "CONTINUE:\n- a: <out>\n- b"
        )
    );
    env.close().await;

    // A lowercase prefix is no conclusion, and the whole reply is cut to
    // 4000 characters.
    let env = Env::start().await;
    let (_, create) = completion_requested(&env).await;
    let first: &[&str] = &["not done yet"];
    let pending = pending_refs(&env, &create.branch).await;
    let pending: Vec<&str> = pending.iter().map(String::as_str).collect();
    env.turn_steps(
        &create.branch,
        "eval-1",
        &pending,
        "completed",
        &[("r1", first)],
    );
    env.wait_for(&create.branch, r#"kind="evaluate-again""#)
        .await;
    let whole = format!(
        "Done: a lowercase prefix is not a conclusion {}",
        "z".repeat(5000)
    );
    let second: &[&str] = &[&whole[..100], &whole[100..]];
    let pending = pending_refs(&env, &create.branch).await;
    let pending: Vec<&str> = pending.iter().map(String::as_str).collect();
    env.turn_steps(
        &create.branch,
        "eval-2",
        &pending,
        "completed",
        &[("r1", second)],
    );
    env.wait_for("main", r#"kind="report""#).await;
    let conclusion = env.written_input("main", r#"kind="report""#).await;
    let InputBlock::Raw { text, .. } = &conclusion.blocks[0];
    assert!(text.contains(r#"result="continue""#), "{text}");
    let body = text
        .split_once("Evaluator report:\n")
        .and_then(|(_, rest)| rest.strip_suffix("\n</dim-goal>"))
        .expect("a report body");
    assert_eq!(body.chars().count(), 4000);
    assert!(
        body.starts_with("Done: a lowercase prefix"),
        "{}",
        &body[..40]
    );
    env.close().await;
}

#[tokio::test]
async fn pursuit_1_a_completion_request_creates_one_evaluator_and_continues_nothing() {
    let env = Env::start().await;
    let (evaluation, create) = completion_requested(&env).await;
    assert_eq!(env.creates("main").await.len(), 1);
    match create.at {
        BranchCreateAt::Sequence { sequence } => assert_eq!(sequence, evaluation),
        BranchCreateAt::Create {} => {
            let event=env.all_events("main").await.into_iter().find(|event|matches!(&event.kind,dim_agent_types::EventKind::BranchCreate(data) if data.branch==create.branch)).unwrap();
            assert_eq!(
                event.sequence,
                evaluation + 1,
                "head anchoring adds only the structural creation fact"
            );
        }
    }
    assert_eq!(
        create.prompt,
        texts::evaluator_prompt("main", evaluation, "ship it", "all tests pass")
    );
    assert_eq!(
        env.settled("main").await.len(),
        5,
        "the set trigger and its record, one continuation, the request and its record"
    );
    assert!(
        writes_with(&env, "main", r#"status="completed""#)
            .await
            .is_empty(),
        "the goal stays active until the evaluator decides"
    );
    env.close().await;
}

#[tokio::test]
async fn pursuit_1_the_evaluator_done_completes_the_goal_by_the_evaluator() {
    let env = Env::start().await;
    let (evaluation, create) = completion_requested(&env).await;
    let conclusion_sequence = write_conclusion(
        &env,
        &create.branch,
        evaluation,
        "done",
        "DONE: all 3 tests pass",
    );
    let completed = env.wait_for("main", r#"status="completed""#).await;
    assert_eq!(
        completed.text,
        texts::evaluated(
            "main",
            "ship it",
            "completed",
            "all 3 tests pass",
            conclusion_sequence
        )
    );
    // The completed goal writes no continuation at the next turn end.
    let before = env.settled("main").await.len();
    env.turn_start("main", "next");
    env.end_turn("main", "next", "completed");
    assert_eq!(env.settled("main").await.len(), before);
    env.close().await;
}

#[tokio::test]
async fn pursuit_13_e1_the_evaluator_role_comes_from_the_line_source_not_its_prompt() {
    // A host line that is not an evaluation reports nothing, whatever its
    // prompt says.
    let env = Env::start().await;
    let foreign = env
        .open_branch(
            "main",
            &texts::evaluator_prompt("main", 7, "ship it", "all tests pass"),
            BranchSource::Host {
                operation_id: "goal:other:7".into(),
            },
        )
        .await;
    line_turn(&env, &foreign, "t1", &[("r1", &["DONE: everything"])]).await;
    assert!(
        env.settled("main").await.is_empty(),
        "a host line that is not an evaluation reports nothing"
    );
    assert!(
        env.writes(&foreign).await.is_empty(),
        "the foreign line writes nothing"
    );
    env.close().await;

    // The evaluator line's report reaches the branch it evaluates.
    let env = Env::start().await;
    let (evaluation, create) = completion_requested(&env).await;
    line_turn(&env, &create.branch, "t1", &[("r1", &["DONE: all good"])]).await;
    env.wait_for("main", r#"kind="report""#).await;
    let report = env.written_input("main", r#"kind="report""#).await;
    assert_eq!(
        match &report.blocks[0] {
            InputBlock::Raw { text, .. } => text.clone(),
        },
        texts::conclusion("main", &create.branch, evaluation, "done", "DONE: all good")
    );
    env.close().await;
}

#[tokio::test]
async fn pursuit_13_e2_a_decided_evaluation_ends_its_evaluator_once() {
    for result in ["done", "blocked", "continue", "failed"] {
        let env = Env::start().await;
        let (evaluation, create) = completion_requested(&env).await;
        let evaluator = create.branch.clone();
        write_conclusion(
            &env,
            &evaluator,
            evaluation,
            result,
            &format!("{result} report"),
        );
        tokio::time::timeout(Duration::from_secs(15), async {
            while env.ended("main").await.is_empty() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the evaluator ends");
        let ended = env.ended("main").await;
        assert_eq!(ended.len(), 1, "{result}");
        assert_eq!(ended[0].branch, evaluator, "{result}");
        assert_eq!(
            ended[0].source,
            BranchSource::Host {
                operation_id: format!("goal:evaluate:{evaluation}")
            },
            "{result}"
        );
        env.goal("pause", json!({})).await.expect("pause writes");
        env.wait_for("main", r#"op="pause""#).await;
        assert_eq!(env.ended("main").await.len(), 1, "{result}: ended once");
        env.close().await;
    }
}

#[tokio::test]
async fn pursuit_13_e2_a_superseded_evaluation_ends_its_evaluator() {
    let env = Env::start().await;
    let (evaluation, create) = completion_requested(&env).await;
    env.goal("pause", json!({})).await.expect("pause writes");
    tokio::time::timeout(Duration::from_secs(15), async {
        while env.ended("main").await.is_empty() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the evaluator ends");
    let ended = env.ended("main").await;
    assert_eq!(ended.len(), 1);
    assert_eq!(ended[0].branch, create.branch);
    assert_eq!(
        ended[0].source,
        BranchSource::Host {
            operation_id: format!("goal:evaluate:{evaluation}")
        }
    );
    env.close().await;
}

/// The pause an evaluated continuation decides is stated at the fold the record
/// writer composes from: a Queue that commits in the settlement window is still
/// pending there, so the pause waits for that Queue's own turn end (#2119,
/// Pursuit §4.1, C4, C5).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pursuit_5_a_queue_committed_before_the_evaluated_pause_holds_it() {
    let env = Env::start().await;
    env.set_limit(1);
    let (evaluation, create) = completion_requested_with(&env, Some(1)).await;

    // The limit is reached, so the conclusion decides a pause. Its record
    // writer is held where it settles the decisions and composes the record.
    let mut held = dim_goal::testing::hold_stage(
        env.session_id(),
        "main",
        dim_goal::testing::Stage::Record,
        evaluation,
        None,
    );
    let conclusion = write_conclusion(
        &env,
        &create.branch,
        evaluation,
        "continue",
        "CONTINUE:\n- one test fails: <output>",
    );
    assert_eq!(
        held.arrived().await,
        conclusion,
        "the pause's record writer answers the conclusion at {conclusion}"
    );

    let queued = env.emit(
        "main",
        message(vec![raw("keep going", None, false)], InputMode::Queue),
    );
    assert!(
        queued.sequence > conclusion,
        "the Queue at {} commits after the conclusion at {conclusion}",
        queued.sequence
    );
    env.dispatched("main", queued.sequence).await;
    held.release();

    let end = env.turn_ids("main", &[&queued.id]);
    env.dispatched("main", end.sequence).await;
    let stale = tokio::time::timeout(Duration::from_millis(750), async {
        loop {
            let stale: Vec<String> = env
                .settled("main")
                .await
                .iter()
                .filter(|block| {
                    block.marker.contains(r#"kind="state""#)
                        && block.marker.contains(r#"status="paused""#)
                        && block.marker.contains(&format!(r#"after="{conclusion}""#))
                })
                .map(|block| format!("{} at {}", block.marker, block.sequence))
                .collect();
            if !stale.is_empty() {
                return stale;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    assert!(
        stale.is_err(),
        "the conclusion at {conclusion} writes no pause once the Queue at {} is pending in the fold, yet it wrote {:?}",
        queued.sequence,
        stale.unwrap_or_default()
    );

    let pause = env.wait_for("main", r#"reason="limit""#).await;
    assert_eq!(
        pause.text,
        texts::evaluated_pause(
            "main",
            "ship it",
            "limit",
            "the automatic continuation limit was reached",
            end.sequence
        ),
        "the Queue's own turn end decides the pause"
    );
    env.close().await;
}

/// An evaluator's end that folds after the pause that settled it was abandoned
/// does not pause the goal: the evaluation was ended for that pause, not by its
/// own failure, and the Queue's turn decides (#2119, Pursuit §4.1, C4, C5).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pursuit_5_an_abandoned_pause_does_not_fail_an_in_flight_evaluation() {
    let env = Env::start().await;
    env.set_limit(1);
    let (evaluation, create) = completion_requested_with(&env, Some(1)).await;

    // A Queue drives a turn on the goal's branch, and its end decides the
    // limit pause. The pause's record writer is held where it settled the
    // decisions: the settlement ended the in-flight evaluator.
    let mut held = dim_goal::testing::hold_stage(
        env.session_id(),
        "main",
        dim_goal::testing::Stage::Record,
        evaluation,
        None,
    );
    let queued = env.emit(
        "main",
        message(vec![raw("keep going", None, false)], InputMode::Queue),
    );
    let end = env.turn_ids("main", &[&queued.id]);
    assert_eq!(
        held.arrived().await,
        end.sequence,
        "the pause's record writer answers the turn end at {}",
        end.sequence
    );

    // Another driving input commits in the settlement window: the pause is
    // abandoned, and the evaluator's end folds after it.
    let second = env.emit(
        "main",
        message(vec![raw("keep going", None, false)], InputMode::Queue),
    );
    env.dispatched("main", second.sequence).await;
    held.release();
    // The settlement's own end is delivered while the record still holds its
    // decisions, so that handler is dropped. The race this case states is the
    // same end folding after the record, which the case writes here.
    let outside = env.emit(
        "main",
        EventKind::BranchEnded(BranchEndedData {
            branch: create.branch.clone(),
            end_event: 0,
            source: BranchSource::Host {
                operation_id: "user".into(),
            },
            ancestor: None,
        }),
    );
    env.dispatched("main", outside.sequence).await;

    // The Queue's own turn end decides the pause the abandoned record left
    // open; the evaluator's end states nothing over it.
    let next = env.turn_ids("main", &[&second.id]);
    let pause = env
        .wait_for("main", r#"kind="state" status="paused""#)
        .await;
    assert_eq!(
        pause.text,
        texts::evaluated_pause(
            "main",
            "ship it",
            "limit",
            "the automatic continuation limit was reached",
            next.sequence
        ),
        "the evaluator's end states no pause over the Queue at {}",
        second.sequence
    );
    env.close().await;
}

/// A pause the Queue abandons leaves the evaluator it settled open: the
/// settlement cancels and drains the decisions, and the evaluator ends only
/// once a folded fact makes it unwanted (#2119, Pursuit §4.1, §13 E2).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_pause_a_queue_abandons_leaves_its_evaluator_to_conclude() {
    let env = Env::start().await;
    env.set_limit(1);
    let (evaluation, create) = completion_requested_with(&env, Some(1)).await;

    // A Queue drives a turn whose end decides the limit pause, and the pause's
    // record writer is held after it settled the decisions.
    let mut record = dim_goal::testing::hold_stage(
        env.session_id(),
        "main",
        dim_goal::testing::Stage::Record,
        evaluation,
        None,
    );
    let queued = env.emit(
        "main",
        message(vec![raw("keep going", None, false)], InputMode::Queue),
    );
    let end = env.turn_ids("main", &[&queued.id]);
    assert_eq!(
        record.arrived().await,
        end.sequence,
        "the pause's record writer answers the turn end at {}",
        end.sequence
    );

    // A second Queue commits in the settlement window: the pause is abandoned,
    // and the evaluation its writer settled is still wanted.
    let second = env.emit(
        "main",
        message(vec![raw("keep going", None, false)], InputMode::Queue),
    );
    env.dispatched("main", second.sequence).await;
    let ended: Vec<String> = env
        .ended("main")
        .await
        .iter()
        .filter(|end| end.branch == create.branch)
        .map(|end| format!("{:?}", end.source))
        .collect();
    assert!(
        ended.is_empty(),
        "the Queue at {} abandons the pause at {}, but its settlement ended the evaluator: {ended:?}",
        second.sequence,
        end.sequence
    );
    // The turn end returns only after the record writer released admission, so
    // the conclusion below folds into a decision the settlement still takes.
    let mut done = dim_goal::testing::hold_stage(
        env.session_id(),
        "main",
        dim_goal::testing::Stage::Done,
        end.sequence,
        Some(end.sequence),
    );
    record.release();
    done.arrived().await;
    done.release();

    // The abandoned pause leaves the evaluation to conclude.
    let conclusion = write_conclusion(&env, &create.branch, evaluation, "done", "DONE: verified");
    let completed = env.wait_for("main", r#"status="completed""#).await;
    assert_eq!(
        completed.text,
        texts::evaluated("main", "ship it", "completed", "verified", conclusion),
        "the evaluator the pause settled concludes for the goal the Queue kept running"
    );
    env.close().await;
}

/// The end of an evaluator the settlement ended for a pause folds after that
/// pause: it is cleanup, not the evaluation's own failure, and it does not
/// restate the pause (#2119, Pursuit §13 E2).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_settled_evaluator_does_not_restate_the_pause_that_settled_it() {
    let env = Env::start().await;
    env.set_limit(1);
    let (evaluation, _create) = completion_requested_with(&env, Some(1)).await;

    let queued = env.emit(
        "main",
        message(vec![raw("keep going", None, false)], InputMode::Queue),
    );
    let end = env.turn_ids("main", &[&queued.id]);
    let pause = env
        .wait_for("main", r#"kind="state" status="paused""#)
        .await;
    assert_eq!(
        pause.text,
        texts::evaluated_pause(
            "main",
            "ship it",
            "limit",
            "the automatic continuation limit was reached",
            end.sequence
        ),
        "the end of the evaluator at {evaluation} does not restate the limit pause"
    );
    let restated: Vec<String> = env
        .writes("main")
        .await
        .iter()
        .filter(|block| block.marker.contains(r#"reason="evaluation_failed""#))
        .map(|block| format!("{} at {}", block.marker, block.sequence))
        .collect();
    assert!(
        restated.is_empty(),
        "the ended evaluator restated the pause as its own failure: {restated:?}"
    );
    env.close().await;
}

/// An operator interrupt that folds before an evaluated pause reads its
/// cutoff is stated by the interrupt's own pause: the writer re-decides the
/// conclusion at the fold it writes from, where the interrupt already follows
/// the evaluated turn end (#2119, Pursuit §4.1, §5, C4).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_evaluated_pause_does_not_answer_a_later_interrupt() {
    let env = Env::start().await;
    env.set_limit(1);
    let (evaluation, create) = completion_requested_with(&env, Some(1)).await;

    // The limit is reached, so the conclusion decides a pause. Its record
    // writer is held after it settled the decisions, before it composes.
    let mut held = dim_goal::testing::hold_stage(
        env.session_id(),
        "main",
        dim_goal::testing::Stage::Record,
        evaluation,
        None,
    );
    let conclusion = write_conclusion(
        &env,
        &create.branch,
        evaluation,
        "continue",
        "CONTINUE:\n- one test fails: <output>",
    );
    assert_eq!(
        held.arrived().await,
        conclusion,
        "the pause's record writer answers the conclusion at {conclusion}"
    );

    // An operator interrupt commits in that window and folds before the writer
    // reads its cutoff again.
    let interrupt = env.interrupt("main");
    env.dispatched("main", interrupt.sequence).await;
    held.release();

    // The interrupt states the pause the cutoff now gives; the conclusion's
    // record does not answer it.
    let pause = env
        .wait_for("main", &format!(r#"after="{}""#, interrupt.sequence))
        .await;
    assert!(
        pause.marker.contains(r#"status="paused""#)
            && pause.marker.contains(r#"reason="interrupted""#),
        "the interrupt at {} states the pause: {pause:#?}",
        interrupt.sequence
    );
    let stale: Vec<String> = env
        .writes("main")
        .await
        .iter()
        .filter(|block| {
            block.marker.contains(r#"kind="state""#)
                && block.marker.contains(r#"status="paused""#)
                && block.marker.contains(&format!(r#"after="{}""#, conclusion))
        })
        .map(|block| format!("{} at {}", block.marker, block.sequence))
        .collect();
    assert!(
        stale.is_empty(),
        "the conclusion at {conclusion} writes no pause once the interrupt at {} follows the evaluated turn end, yet it wrote {stale:?}",
        interrupt.sequence
    );
    env.close().await;
}

/// An ordinary Append that commits between the conclusion's decision and its
/// record leaves the pause unwritten, and the conclusion the caller still owns
/// states its continuation (#2119).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_appended_note_before_the_evaluated_pause_returns_the_conclusion_to_its_continuation() {
    let env = Env::start().await;
    env.set_limit(1);
    let (evaluation, create) = completion_requested_with(&env, Some(1)).await;

    // The limit is reached, so the conclusion decides a pause. Its record waits
    // for the writer before it takes the write guard.
    let mut writer = dim_goal::testing::hold_stage(
        env.session_id(),
        "main",
        dim_goal::testing::Stage::Writer,
        evaluation,
        None,
    );
    let conclusion = write_conclusion(
        &env,
        &create.branch,
        evaluation,
        "continue",
        "CONTINUE:\n- one test fails: <output>",
    );
    let written = tokio::time::timeout(Duration::from_secs(15), writer.arrived())
        .await
        .expect("the conclusion writes its record");
    assert_eq!(
        written, conclusion,
        "the conclusion at {conclusion} writes its record"
    );

    // An ordinary Append commits before the record takes the guard: the fold
    // resets the continuation count, so no limit pause is due.
    let note = env.emit("main", user("a note", InputMode::Append));
    env.dispatched("main", note.sequence).await;
    let mut pause = dim_goal::testing::hold_stage(
        env.session_id(),
        "main",
        dim_goal::testing::Stage::Append,
        conclusion,
        Some(conclusion),
    );
    writer.release();

    let recorded = tokio::time::timeout(Duration::from_millis(750), pause.arrived())
        .await
        .is_ok();
    assert!(
        !recorded,
        "the Append at {} takes the limit pause back, but the record at {conclusion} reached its append",
        note.sequence
    );
    let continuation = env
        .wait_for(
            "main",
            &format!(
                r#"kind="continuation" count="1" limit="1" after="{evaluation}" evaluation="{evaluation}""#
            ),
        )
        .await;
    assert_eq!(
        continuation.text,
        texts::evaluated_continuation("main", 1, Some(1), 0, evaluation, evaluation),
        "the conclusion at {conclusion} continues the goal the Append left running"
    );
    env.close().await;
}

#[tokio::test]
async fn pursuit_13_e3_an_outside_end_before_the_conclusion_fails_the_evaluation() {
    let env = Env::start().await;
    let (evaluation, create) = completion_requested(&env).await;
    let outside = env.emit(
        "main",
        dim_agent_types::EventKind::BranchEnded(BranchEndedData {
            branch: create.branch.clone(),
            end_event: 0,
            source: BranchSource::Host {
                operation_id: "user".into(),
            },
            ancestor: None,
        }),
    );
    let pause = env.wait_for("main", r#"reason="evaluation_failed""#).await;
    assert_eq!(
        pause.text,
        texts::evaluated_pause(
            "main",
            "ship it",
            "evaluation_failed",
            "the evaluator could not check the result",
            outside.sequence
        )
    );
    let ended = env.ended("main").await;
    assert!(
        !ended.iter().any(|end| end.source
            == BranchSource::Host {
                operation_id: format!("goal:evaluate:{evaluation}")
            }),
        "an ended line is not ended twice"
    );

    // A late conclusion decides nothing.
    let before = env.settled("main").await.len();
    write_conclusion(&env, &create.branch, evaluation, "done", "DONE: late");
    assert_eq!(env.settled("main").await.len(), before + 1);
    env.close().await;
}

#[tokio::test]
async fn pursuit_13_e3_an_outside_end_after_the_conclusion_decides_nothing() {
    let env = Env::start().await;
    let (evaluation, create) = completion_requested(&env).await;
    write_conclusion(&env, &create.branch, evaluation, "done", "DONE: verified");
    env.wait_for("main", r#"status="completed""#).await;
    let ended = env.ended("main").await;
    assert_eq!(ended.len(), 1, "the conclusion ends the evaluator");
    let before = env.settled("main").await.len();
    env.write_message(
        "main",
        vec![raw(
            "<dim-goal kind=\"report\" result=\"done\" evaluation=\"1\" evaluator=\"E\" branch=\"main\">\nEvaluator report:\nDONE: ignored\n</dim-goal>",
            None,
            false,
        )],
        InputMode::Append,
    );
    env.emit(
        "main",
        dim_agent_types::EventKind::BranchEnded(BranchEndedData {
            branch: create.branch.clone(),
            end_event: 0,
            source: BranchSource::Host {
                operation_id: "user".into(),
            },
            ancestor: None,
        }),
    );
    assert_eq!(env.settled("main").await.len(), before + 1);
    env.close().await;
}

#[tokio::test]
async fn pursuit_4_5_a_report_without_a_valid_evaluation_or_evaluator_decides_nothing() {
    let env = Env::start().await;
    let (evaluation, create) = completion_requested(&env).await;
    let evaluator = create.branch.clone();
    let writes = [
        format!(r#"result="done" evaluator="{evaluator}""#),
        format!(r#"result="done" evaluation="abc" evaluator="{evaluator}""#),
        format!(r#"result="done" evaluation="{evaluation}""#),
        r#"result="done""#.to_owned(),
    ];
    let before = env.settled("main").await.len();
    for attrs in &writes {
        let text = format!(
            "<dim-goal kind=\"report\" {attrs} branch=\"main\">\nEvaluator report:\nDONE: x\n</dim-goal>"
        );
        env.write_message("main", vec![raw(&text, None, false)], InputMode::Append);
    }
    assert_eq!(
        env.settled("main").await.len(),
        before + writes.len(),
        "no malformed report decides anything"
    );
    assert!(
        writes_with(&env, "main", r#"status="completed""#)
            .await
            .is_empty(),
        "no malformed report completes the goal"
    );

    let conclusion_sequence = write_conclusion(
        &env,
        &evaluator,
        evaluation,
        "done",
        "DONE: all 3 tests pass",
    );
    let completed = env.wait_for("main", r#"status="completed""#).await;
    assert_eq!(
        completed.text,
        texts::evaluated(
            "main",
            "ship it",
            "completed",
            "all 3 tests pass",
            conclusion_sequence
        )
    );
    env.close().await;
}

#[tokio::test]
async fn pursuit_4_a_report_outside_the_first_block_decides_nothing() {
    let env = Env::start().await;
    let (evaluation, create) = completion_requested(&env).await;
    let report = texts::conclusion("main", &create.branch, evaluation, "done", "DONE: later");
    env.write_message(
        "main",
        vec![
            raw("a note before the report", None, false),
            raw(&report, None, false),
        ],
        InputMode::Append,
    );
    assert!(
        env.settled("main")
            .await
            .iter()
            .all(|block| !block.text.contains(r#"status="completed""#)),
        "no conclusion after the first block"
    );
    env.close().await;
}

#[tokio::test]
async fn pursuit_2_a_continue_conclusion_writes_one_continuation_or_pauses_at_the_limit() {
    let env = Env::start().await;
    let (evaluation, create) = completion_requested(&env).await;
    write_conclusion(
        &env,
        &create.branch,
        evaluation,
        "continue",
        "CONTINUE:\n- two tests fail: <output>",
    );
    let needle =
        format!(r#"kind="continuation" count="2" after="{evaluation}" evaluation="{evaluation}""#);
    let continuation = env.wait_for("main", &needle).await;
    assert_eq!(
        continuation.text,
        texts::evaluated_continuation("main", 2, None, 0, evaluation, evaluation)
    );
    env.close().await;

    let env = Env::start().await;
    env.set_limit(1);
    let (evaluation, create) = completion_requested_with(&env, Some(1)).await;
    let conclusion = write_conclusion(
        &env,
        &create.branch,
        evaluation,
        "continue",
        "CONTINUE:\n- still missing",
    );
    let pause = env.wait_for("main", r#"reason="limit""#).await;
    assert_eq!(
        pause.text,
        texts::evaluated_pause(
            "main",
            "ship it",
            "limit",
            "the automatic continuation limit was reached",
            conclusion
        )
    );
    env.close().await;
}

/// A Queue that commits while the `continue` conclusion waits for its cutoff
/// leaves the evaluated continuation behind: the recheck reads the fold at the
/// latest committed cutoff, so the Queue's own turn decides (Pursuit §4.1, §5).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pursuit_2_a_continue_conclusion_reads_the_latest_committed_cutoff() {
    let env = Env::start().await;
    let (evaluation, create) = completion_requested(&env).await;
    let through = env.timeline("main").committed() + 1;
    let mut cutoff_gate = dim_goal::testing::hold_stage(
        env.session_id(),
        "main",
        dim_goal::testing::Stage::Cutoff,
        through,
        None,
    );
    let conclusion = texts::conclusion(
        "main",
        &create.branch,
        evaluation,
        "continue",
        "CONTINUE:\n- still missing",
    );
    let conclusion = env.write_message(
        "main",
        vec![raw(&conclusion, None, false)],
        InputMode::Append,
    );
    assert!(
        conclusion.sequence >= through,
        "the conclusion input at {} follows the registered cutoff {through}",
        conclusion.sequence
    );
    let cutoff = cutoff_gate.arrived().await;
    assert!(
        cutoff >= conclusion.sequence,
        "the conclusion's write waits at the latest committed cutoff {cutoff}, after its input at {}",
        conclusion.sequence
    );

    let queue_gate = dim_goal::testing::hold_input(
        env.session_id(),
        "main",
        env.timeline("main").committed() + 1,
    );
    let queued = env.write_message(
        "main",
        vec![raw("keep going", None, false)],
        InputMode::Queue,
    );
    assert!(
        queued.sequence > cutoff,
        "the queued input at {} commits after the conclusion's cutoff {cutoff}",
        queued.sequence
    );
    cutoff_gate.release();

    let stale = tokio::time::timeout(Duration::from_millis(750), async {
        loop {
            let writes = writes_with(&env, "main", &format!(r#"evaluation="{evaluation}""#)).await;
            if let Some(block) = writes
                .iter()
                .find(|block| block.marker.contains(r#"kind="continuation""#))
            {
                return block.marker.clone();
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    assert!(
        stale.is_err(),
        "the continue conclusion writes no continuation while a committed Queue is not folded: {stale:?}"
    );

    queue_gate.release();
    env.dispatched("main", queued.sequence).await;
    let writes = env.settled("main").await;
    assert!(
        !writes.iter().any(|block| {
            block.marker.contains(r#"kind="continuation""#)
                && block
                    .marker
                    .contains(&format!(r#"evaluation="{evaluation}""#))
        }),
        "the continue conclusion writes no continuation after the Queue folds: {writes:#?}"
    );
    assert!(
        env.pending_inputs("main").await.contains(&queued.id),
        "the Queue drives the next turn instead of the evaluated continuation"
    );
    env.close().await;
}

/// A Queue that commits inside the evaluated continuation's submission lookup,
/// then is consumed by the next turn while `turn_end` still names the judged
/// end: the recheck sees the started turn and the append refuses the head that
/// moved behind it (#1989).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pursuit_5_a_consumed_queue_that_started_a_turn_holds_the_evaluated_continuation() {
    let env = Env::start_gated().await;
    let (evaluation, create) = completion_requested(&env).await;
    let (entered, release) = env.gated().hold_query("input/message");
    let conclusion = texts::conclusion(
        "main",
        &create.branch,
        evaluation,
        "continue",
        "CONTINUE:\n- still missing",
    );
    env.write_message(
        "main",
        vec![raw(&conclusion, None, false)],
        InputMode::Append,
    );
    tokio::time::timeout(Duration::from_secs(15), entered)
        .await
        .expect("the evaluated continuation's submission lookup starts")
        .expect("the held lookup is the continuation's");
    let queued = env.write_message(
        "main",
        vec![raw("keep going", None, false)],
        InputMode::Queue,
    );
    env.dispatched("main", queued.sequence).await;
    env.turn_start("main", "next");
    env.consume("main", &queued.id);
    release.send(()).expect("the lookup answers");
    let writes = env.settled("main").await;
    let stale: Vec<String> = writes
        .iter()
        .filter(|block| {
            block.marker.contains(r#"kind="continuation""#)
                && block
                    .marker
                    .contains(&format!(r#"evaluation="{evaluation}""#))
        })
        .map(|block| format!("{} at {}", block.marker, block.sequence))
        .collect();
    assert!(
        stale.is_empty(),
        "the continue conclusion writes no evaluated continuation after the Queue at {} opened a new turn inside the lookup window, yet it wrote {stale:?}",
        queued.sequence
    );
    env.close().await;
}

/// A Queue whose turn starts and is consumed before the `continue`
/// conclusion takes its first capture, while the conclusion waits to read its
/// settings: the decision is bound to the judged turn, so the newer turn
/// decides and the conclusion writes no evaluated continuation (#1989).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pursuit_5_a_turn_already_open_before_the_conclusion_holds_the_evaluated_continuation() {
    let env = Env::start_gated().await;
    let (evaluation, create) = completion_requested(&env).await;
    let mut continued = dim_goal::testing::hold_stage(
        env.session_id(),
        "main",
        dim_goal::testing::Stage::Continue,
        env.timeline("main").committed() + 1,
        None,
    );
    let conclusion = write_conclusion(
        &env,
        &create.branch,
        evaluation,
        "continue",
        "CONTINUE:\n- still missing",
    );
    let held = tokio::time::timeout(Duration::from_secs(15), continued.arrived())
        .await
        .expect("the conclusion task reaches its settings read");
    assert_eq!(
        held, conclusion,
        "the held step follows the conclusion at {conclusion}"
    );
    let queued = env.write_message(
        "main",
        vec![raw("keep going", None, false)],
        InputMode::Queue,
    );
    env.dispatched("main", queued.sequence).await;
    env.turn_start("main", "next");
    let consumed = env.consume("main", &queued.id);
    env.dispatched("main", consumed.sequence).await;
    let mut done = dim_goal::testing::hold_stage(
        env.session_id(),
        "main",
        dim_goal::testing::Stage::Done,
        conclusion,
        Some(conclusion),
    );
    continued.release();
    done.arrived().await;
    let stale = evaluated_writes(&env, evaluation).await;
    assert!(
        stale.is_empty(),
        "the continue conclusion writes no evaluated continuation after the Queue at {} opened a turn before its first capture, but it wrote {stale:?}",
        queued.sequence
    );
    done.release();
    env.close().await;
}

/// A Queue whose turn starts, is consumed and ends before the `continue`
/// conclusion takes its first capture, while the conclusion waits to read its
/// settings: the conclusion names a turn end that is no longer the branch's
/// latest, so it decides nothing and the newer turn end decides (#1989).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pursuit_5_a_turn_that_ended_before_the_conclusion_holds_the_evaluated_continuation() {
    let env = Env::start_gated().await;
    let (evaluation, create) = completion_requested(&env).await;
    let mut continued = dim_goal::testing::hold_stage(
        env.session_id(),
        "main",
        dim_goal::testing::Stage::Continue,
        env.timeline("main").committed() + 1,
        None,
    );
    let conclusion = write_conclusion(
        &env,
        &create.branch,
        evaluation,
        "continue",
        "CONTINUE:\n- still missing",
    );
    let held = tokio::time::timeout(Duration::from_secs(15), continued.arrived())
        .await
        .expect("the conclusion task reaches its settings read");
    assert_eq!(
        held, conclusion,
        "the held step follows the conclusion at {conclusion}"
    );
    let queued = env.write_message(
        "main",
        vec![raw("keep going", None, false)],
        InputMode::Queue,
    );
    env.dispatched("main", queued.sequence).await;
    env.turn_start("main", "next");
    env.consume("main", &queued.id);
    let mut turn_end = dim_goal::testing::hold_stage(
        env.session_id(),
        "main",
        dim_goal::testing::Stage::Decision,
        env.timeline("main").committed() + 1,
        None,
    );
    let ended = env.end_turn("main", "next", "completed");
    env.dispatched("main", ended.sequence).await;
    assert!(
        ended.sequence > evaluation,
        "the newer turn end at {} follows the judged end at {evaluation}",
        ended.sequence
    );
    let held_end = tokio::time::timeout(Duration::from_secs(15), turn_end.arrived())
        .await
        .expect("the newer turn end reaches its decision");
    assert_eq!(
        held_end, ended.sequence,
        "the held decision follows the turn end at {}",
        ended.sequence
    );
    let mut done = dim_goal::testing::hold_stage(
        env.session_id(),
        "main",
        dim_goal::testing::Stage::Done,
        conclusion,
        Some(conclusion),
    );
    continued.release();
    done.arrived().await;
    let stale = evaluated_writes(&env, evaluation).await;
    assert!(
        stale.is_empty(),
        "the continue conclusion writes no evaluated continuation after the Queue at {} opened and ended a turn before its first capture, but it wrote {stale:?}",
        queued.sequence
    );
    done.release();
    turn_end.release();
    let regular = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let writes = env.writes("main").await;
            if let Some(block) = writes.iter().find(|block| {
                block.marker.contains(r#"kind="continuation""#)
                    && block
                        .marker
                        .contains(&format!(r#"after="{}""#, ended.sequence))
            }) {
                return block.sequence;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the newer turn end decides its own continuation");
    env.dispatched("main", regular).await;
    env.close().await;
}

/// A turn interrupt that commits after the evaluated continuation took its
/// decision, before its append: the append is refused against the moved head,
/// and the retry must not continue a goal the interrupt paused (#1989).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pursuit_5_an_interrupt_in_the_evaluated_write_window_holds_the_continuation() {
    let env = Env::start().await;
    let (evaluation, create) = completion_requested(&env).await;
    let mut write = dim_goal::testing::hold_write(env.session_id(), "main");
    let conclusion = write_conclusion(
        &env,
        &create.branch,
        evaluation,
        "continue",
        "CONTINUE:\n- still missing",
    );
    assert_eq!(
        write.arrived().await,
        evaluation,
        "the held write belongs to the decision for the evaluated turn end"
    );
    let interrupt = env.interrupt("main");
    env.dispatched("main", interrupt.sequence).await;
    let mut done = dim_goal::testing::hold_stage(
        env.session_id(),
        "main",
        dim_goal::testing::Stage::Done,
        conclusion,
        Some(conclusion),
    );
    write.release();
    done.arrived().await;
    let stale = evaluated_writes(&env, evaluation).await;
    assert!(
        stale.is_empty(),
        "the continue conclusion writes no evaluated continuation after the interrupt at {}, but it wrote {stale:?}",
        interrupt.sequence
    );
    done.release();
    env.close().await;
}

/// A Queue that the turn end saw pending is withdrawn before the turn-end
/// decision: the end belongs to the evaluator-first decision again, not to an
/// ordinary continuation (#1989).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pursuit_1_a_withdrawn_queue_returns_the_turn_end_to_the_evaluator() {
    let env = Env::start_gated().await;
    env.goal("set", json!({"objective": "ship it", "judge": "evaluator"}))
        .await
        .expect("set writes");
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

    let mut decision = dim_goal::testing::hold_stage(
        env.session_id(),
        "main",
        dim_goal::testing::Stage::Decision,
        env.timeline("main").committed() + 1,
        None,
    );
    let ended = env.end_turn("main", "work", "completed");
    env.dispatched("main", ended.sequence).await;
    assert_eq!(
        decision.arrived().await,
        ended.sequence,
        "the held decision follows the turn end at {}",
        ended.sequence
    );
    let queued = env.write_message(
        "main",
        vec![raw("keep going", None, false)],
        InputMode::Queue,
    );
    env.dispatched("main", queued.sequence).await;
    let mut cutoff = dim_goal::testing::hold_stage(
        env.session_id(),
        "main",
        dim_goal::testing::Stage::Cutoff,
        queued.sequence,
        Some(queued.sequence),
    );
    let mut done = dim_goal::testing::hold_stage(
        env.session_id(),
        "main",
        dim_goal::testing::Stage::Done,
        ended.sequence,
        Some(ended.sequence),
    );
    decision.release();
    assert_eq!(
        cutoff.arrived().await,
        queued.sequence,
        "the turn-end decision reaches the Queue's cutoff with the Queue still pending"
    );
    env.delete_id("main", &queued.id);
    let deleted = env.timeline("main").committed();
    env.dispatched("main", deleted).await;
    cutoff.release();
    done.arrived().await;
    let creates = env.creates("main").await;
    assert_eq!(
        creates.len(),
        1,
        "the withdrawn Queue at {} returns the turn end to the evaluator-first decision: {creates:#?}",
        queued.sequence
    );
    let stale = env
        .writes("main")
        .await
        .into_iter()
        .filter(|block| {
            block.marker.contains(r#"kind="continuation""#)
                && block
                    .marker
                    .contains(&format!(r#"after="{}""#, ended.sequence))
        })
        .map(|block| format!("{} at {}", block.marker, block.sequence))
        .collect::<Vec<_>>();
    assert!(
        stale.is_empty(),
        "the withdrawn Queue at {} leaves the turn end to the evaluator, but an ordinary continuation was written: {stale:?}",
        queued.sequence
    );
    done.release();
    env.close().await;
}

/// A Queue that held the evaluator-first decision back is withdrawn while the
/// turn end waits for its progress query: the limit pause the writer decided is
/// no longer the answer, and the turn end creates the evaluator (Pursuit §4.1,
/// #2119).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_queue_withdrawn_before_the_pause_record_keeps_the_evaluator() {
    let env = Env::start_gated().await;
    env.set_limit(1);
    env.goal("set", json!({"objective": "ship it", "judge": "evaluator"}))
        .await
        .expect("set writes");
    let set = env.wait_for("main", r#"op="set""#).await;
    let first = env.turn("main", &[&set]);
    let continuation = env
        .wait_for(
            "main",
            &format!(
                r#"kind="continuation" count="1" limit="1" after="{}""#,
                first.sequence
            ),
        )
        .await;
    env.turn_start("main", "work");
    env.consume("main", &continuation.id);
    env.tool(
        "main",
        json!({"status": "completed", "summary": "all tests pass"}),
    )
    .await;
    // The Queue holds the evaluator-first decision back, so the turn end is
    // about to state the limit pause instead.
    let queued = env.write_message(
        "main",
        vec![raw("keep going", None, false)],
        InputMode::Queue,
    );
    env.dispatched("main", queued.sequence).await;

    let (progress, release_progress) = env.gated().hold_query("step/tool-call");
    let ended = env.end_turn("main", "work", "completed");
    progress.await.expect("the turn-end progress query starts");

    // The two ends this window can take: the record writer states the pause at
    // its append, or the turn end is decided again for the evaluator.
    let mut pause = dim_goal::testing::hold_stage(
        env.session_id(),
        "main",
        dim_goal::testing::Stage::Append,
        ended.sequence,
        Some(ended.sequence),
    );
    let mut retaken = dim_goal::testing::hold_stage(
        env.session_id(),
        "main",
        dim_goal::testing::Stage::Decision,
        ended.sequence,
        Some(ended.sequence),
    );
    env.delete_id("main", &queued.id);
    let deleted = env.timeline("main").committed();
    env.dispatched("main", deleted).await;
    release_progress
        .send(())
        .expect("the progress query answers");

    let recorded = tokio::select! {
        _ = pause.arrived() => true,
        _ = retaken.arrived() => false,
    };
    assert!(
        !recorded,
        "the withdrawn Queue at {} leaves the turn end at {} to the evaluator, but its limit pause reached the append",
        queued.sequence, ended.sequence
    );
    let mut done = dim_goal::testing::hold_stage(
        env.session_id(),
        "main",
        dim_goal::testing::Stage::Done,
        ended.sequence,
        Some(ended.sequence),
    );
    retaken.release();
    assert_eq!(
        done.arrived().await,
        ended.sequence,
        "the retaken decision creates the evaluator for the turn end at {}",
        ended.sequence
    );
    done.release();

    let creates = env.creates("main").await;
    assert_eq!(
        creates.len(),
        1,
        "the withdrawn Queue at {} keeps one evaluator for the turn end at {}: {creates:#?}",
        queued.sequence,
        ended.sequence
    );
    assert_eq!(
        creates[0].source,
        BranchSource::Host {
            operation_id: format!("goal:evaluate:{}", ended.sequence)
        },
        "the evaluator was created for the turn end at {}",
        ended.sequence
    );
    let paused = env
        .writes("main")
        .await
        .into_iter()
        .filter(|block| {
            block
                .marker
                .contains(r#"kind="state" status="paused" reason="limit""#)
                && block
                    .marker
                    .contains(&format!(r#"after="{}""#, ended.sequence))
        })
        .map(|block| format!("{} at {}", block.marker, block.sequence))
        .collect::<Vec<_>>();
    assert!(
        paused.is_empty(),
        "the withdrawn Queue at {} leaves the turn end at {} to the evaluator, but the limit pause was recorded: {paused:?}",
        queued.sequence,
        ended.sequence
    );
    env.close().await;
}

/// A `continue` conclusion that arrives while a background task is open waits
/// for the task: its cancelled end takes the conclusion once, and a second
/// cancelled end writes nothing more (Pursuit §5).
#[tokio::test]
async fn pursuit_8_a_continue_conclusion_waits_for_a_cancelled_background_task() {
    let env = Env::start_gated().await;
    env.goal("set", json!({"objective": "ship it", "judge": "evaluator"}))
        .await
        .expect("set writes");
    let set = env.wait_for("main", r#"op="set""#).await;
    env.turn_start("main", "work");
    env.consume("main", &set.id);
    env.exec_started("main", "work", "task-1");
    env.tool(
        "main",
        json!({"status": "completed", "summary": "all tests pass"}),
    )
    .await;
    let end = env.end_turn("main", "work", "completed");
    let create = next_create(&env, "main", 0).await;
    // Hold the continuation's publication so the settle retry runs while the
    // fold has not seen the written input.
    let (written, release) = env.gated().hold_continuation();
    write_conclusion(
        &env,
        &create.branch,
        end.sequence,
        "continue",
        "CONTINUE:\n- still missing",
    );
    env.exec_ended("main", "task-1", "cancelled");
    written.await.expect("the continuation write is committed");
    tokio::time::sleep(Duration::from_millis(50)).await;
    release.send(()).unwrap();
    let evaluated = wait_evaluated(&env, "main", end.sequence).await;
    assert_eq!(
        evaluated.text,
        texts::evaluated_continuation("main", 1, None, 0, end.sequence, end.sequence),
        "the cancelled end takes the deferred conclusion"
    );
    let writes = env.settled("main").await;
    assert_eq!(
        writes
            .iter()
            .filter(|block| block.marker.contains(r#"kind="continuation""#))
            .count(),
        1,
        "the settle retry writes no second continuation: {writes:#?}"
    );

    env.consume("main", &evaluated.id);
    env.exec_started("main", "work", "task-2");
    env.exec_ended("main", "task-2", "cancelled");
    let writes = env.settled("main").await;
    assert_eq!(
        writes
            .iter()
            .filter(|block| block.marker.contains(r#"kind="continuation""#))
            .count(),
        1,
        "a second cancelled end writes no second continuation: {writes:#?}"
    );
    env.close().await;
}

/// Seeds the history of a `continue` conclusion that arrived while a
/// background task was open, optionally with the cancelled end already
/// committed, and restores the Session. Returns the evaluation sequence.
async fn deferred_conclusion(env: Env, cancelled: bool) -> (Env, u64) {
    let mut seed = env.seed_new("load-deferred").await;
    let set = seed.append(
        "main",
        message(
            vec![raw(&texts::set("main", "ship it"), None, false)],
            InputMode::Queue,
        ),
    );
    seed.append(
        "main",
        goal_state(Some("ship it"), serde_json::json!({"kind":"active"}), true),
    );
    seed.append(
        "main",
        EventKind::TurnStart(TurnStartData {
            turn_id: "work".into(),
        }),
    );
    seed.append(
        "main",
        EventKind::InputConsumed(InputConsumedData {
            input_id: set.id.clone(),
        }),
    );
    seed.append(
        "main",
        EventKind::ExecStarted(ExecStartedData {
            turn_id: "work".into(),
            request_id: "request".into(),
            call_id: "call-1".into(),
            task_id: "task-1".into(),
            description: "background work".into(),
            title: None,
            output_path: "/dev/null".into(),
        }),
    );
    seed.append(
        "main",
        message(
            vec![raw(
                &texts::report("main", "completed", "all tests pass"),
                None,
                false,
            )],
            InputMode::Append,
        ),
    );
    let end = seed.append("main", turn_end_with("work", "completed"));
    let create = seed.append(
        "main",
        EventKind::BranchCreate(BranchCreateData {
            at: BranchCreateAt::Sequence {
                sequence: end.sequence,
            },
            source: BranchSource::Host {
                operation_id: format!("goal:evaluate:{}", end.sequence),
            },
            branch: "eval".into(),
            prompt: texts::evaluator_prompt("main", end.sequence, "ship it", "all tests pass"),
        }),
    );
    seed.append(
        "eval",
        EventKind::BranchStart(BranchStartData {
            parent: Some("main".into()),
            create_event: Some(create.sequence),
        }),
    );
    seed.append(
        "eval",
        message(vec![raw("line work", None, false)], InputMode::Queue),
    );
    seed.append(
        "main",
        message(
            vec![raw(
                &texts::conclusion(
                    "main",
                    "eval",
                    end.sequence,
                    "continue",
                    "CONTINUE:\n- still missing",
                ),
                None,
                false,
            )],
            InputMode::Append,
        ),
    );
    if cancelled {
        seed.append(
            "main",
            EventKind::ExecEnded(ExecEndedData {
                task_id: "task-1".into(),
                status: ExecStatus::Cancelled,
                result: ToolResult::default(),
            }),
        );
    }
    seed.finish().await;

    let env = env.restore_seeded("load-deferred").await;
    (env, end.sequence)
}

/// The deferred conclusion is re-derived from the fold, so a reload between
/// the conclusion and the cancelled end keeps it (Pursuit §5).
#[tokio::test]
async fn pursuit_8_a_reload_discards_a_deferred_continue_conclusion() {
    let (env, _) = deferred_conclusion(Env::start().await, false).await;
    let before = env.settled("main").await.len();
    assert_eq!(
        env.goal("get", json!({})).await.unwrap()["goal"]["status"]["kind"],
        "paused"
    );
    env.exec_ended("main", "task-1", "cancelled");
    assert_eq!(
        env.settled("main").await.len(),
        before,
        "old conclusions and tasks cannot resume the Goal"
    );
    env.close().await;
}

#[tokio::test]
async fn pursuit_8_a_reload_after_a_cancelled_end_does_not_take_the_old_conclusion() {
    let (env, _) = deferred_conclusion(Env::start().await, true).await;
    let before = env.settled("main").await.len();
    assert_eq!(
        env.goal("get", json!({})).await.unwrap()["goal"]["status"]["kind"],
        "paused"
    );
    env.exec_ended("main", "task-1", "cancelled");
    assert_eq!(
        env.settled("main").await.len(),
        before,
        "old conclusions and tasks cannot resume the Goal"
    );
    env.close().await;
}

#[tokio::test]
async fn pursuit_5_a_failed_conclusion_or_a_failed_creation_gives_one_pause() {
    // A failed conclusion.
    let env = Env::start().await;
    let (evaluation, create) = completion_requested(&env).await;
    let conclusion_sequence = write_conclusion(
        &env,
        &create.branch,
        evaluation,
        "failed",
        "the evaluator's turn did not complete",
    );
    let pause = env.wait_for("main", r#"reason="evaluation_failed""#).await;
    assert_eq!(
        pause.text,
        texts::evaluated_pause(
            "main",
            "ship it",
            "evaluation_failed",
            "the evaluator could not check the result",
            conclusion_sequence
        )
    );
    env.close().await;

    // A creation that failed after the fact.
    let env = Env::start().await;
    let (_evaluation, create) = completion_requested(&env).await;
    let create_event = env
        .all_events("main")
        .await
        .into_iter()
        .find(|event| matches!(event.kind, dim_agent_types::EventKind::BranchCreate(_)))
        .expect("the create fact")
        .sequence;
    let failed = env.emit(
        "main",
        dim_agent_types::EventKind::BranchCreateFailed(BranchCreateFailedData::Failed {
            branch: create.branch.clone(),
            create_event,
            error: "branch creation failed".into(),
        }),
    );
    let pause = env.wait_for("main", r#"reason="evaluation_failed""#).await;
    assert_eq!(
        pause.text,
        texts::evaluated_pause(
            "main",
            "ship it",
            "evaluation_failed",
            "the evaluator could not check the result",
            failed.sequence
        )
    );
    env.close().await;
}

#[tokio::test]
async fn pursuit_8_the_evaluator_reports_the_streamed_conclusion_of_its_final_request() {
    let env = Env::start().await;
    let (evaluation, create) = completion_requested(&env).await;
    let first: &[&str] = &["DONE: stale"];
    let second: &[&str] = &["checked the tree\nDONE", ": all 3 tests pass"];
    let pending = pending_refs(&env, &create.branch).await;
    let pending: Vec<&str> = pending.iter().map(String::as_str).collect();
    env.turn_steps(
        &create.branch,
        "eval-1",
        &pending,
        "completed",
        &[("r1", first), ("r2", second)],
    );
    env.wait_for("main", r#"kind="report""#).await;
    let report = env.written_input("main", r#"kind="report""#).await;
    assert_eq!(
        match &report.blocks[0] {
            InputBlock::Raw { text, .. } => text.clone(),
        },
        texts::conclusion(
            "main",
            &create.branch,
            evaluation,
            "done",
            "DONE: all 3 tests pass"
        )
    );
    assert!(
        env.writes(&create.branch).await.len() == 1,
        "the evaluator line only carries its inherited clear"
    );
    env.close().await;
}

#[tokio::test]
async fn pursuit_4_the_evaluator_is_asked_once_then_its_whole_reply_is_the_report() {
    let env = Env::start().await;
    let (_, create) = completion_requested(&env).await;
    let pending = pending_refs(&env, &create.branch).await;
    let pending: Vec<&str> = pending.iter().map(String::as_str).collect();
    let first: &[&str] = &["not done yet"];
    let end = env.turn_steps(
        &create.branch,
        "eval-1",
        &pending,
        "completed",
        &[("r1", first)],
    );
    let again = env
        .wait_for(&create.branch, r#"kind="evaluate-again""#)
        .await;
    assert_eq!(
        again.text,
        texts::evaluate_again(&create.branch, end.sequence)
    );
    assert_eq!(again.mode, InputMode::Queue);

    let whole = format!("still working {}", "y".repeat(5000));
    let second: &[&str] = &[&whole[..100], &whole[100..]];
    let pending = pending_refs(&env, &create.branch).await;
    let pending: Vec<&str> = pending.iter().map(String::as_str).collect();
    env.turn_steps(
        &create.branch,
        "eval-2",
        &pending,
        "completed",
        &[("r1", second)],
    );
    env.wait_for("main", r#"kind="report""#).await;
    let report = env.written_input("main", r#"kind="report""#).await;
    let InputBlock::Raw { text, .. } = &report.blocks[0];
    assert!(text.contains(r#"result="continue""#), "{text}");
    let body = text
        .split_once("Evaluator report:\n")
        .and_then(|(_, rest)| rest.strip_suffix("\n</dim-goal>"))
        .expect("a report body");
    assert_eq!(body.chars().count(), 4000);
    assert!(body.starts_with("still working y"), "{}", &body[..40]);
    env.close().await;
}

#[tokio::test]
async fn pursuit_5_a_failed_or_cancelled_evaluator_turn_reports_failed() {
    for status in ["failed", "cancelled"] {
        let env = Env::start().await;
        let (evaluation, create) = completion_requested(&env).await;
        let pending = pending_refs(&env, &create.branch).await;
        let pending: Vec<&str> = pending.iter().map(String::as_str).collect();
        env.turn_steps(&create.branch, "eval-1", &pending, status, &[]);
        let reported = env.wait_for("main", r#"kind="report""#).await;
        let report = env.written_input("main", r#"kind="report""#).await;
        assert_eq!(
            match &report.blocks[0] {
                InputBlock::Raw { text, .. } => text.clone(),
            },
            texts::conclusion(
                "main",
                &create.branch,
                evaluation,
                "failed",
                "the evaluator's turn did not complete"
            ),
            "{status}"
        );
        let pause = env.wait_for("main", r#"reason="evaluation_failed""#).await;
        assert_eq!(
            pause.text,
            texts::evaluated_pause(
                "main",
                "ship it",
                "evaluation_failed",
                "the evaluator could not check the result",
                reported.sequence
            ),
            "{status}"
        );
        env.close().await;
    }
}

/// A conclusion or an evaluator failure that folds while a change holds task
/// admission is decided before that change states its own record (#2158).
#[tokio::test]
async fn a_decision_folded_in_the_settlement_window_is_taken() {
    for case in ["done", "blocked", "evaluation_failed"] {
        let env = Env::start().await;
        let (evaluation, create) = completion_requested(&env).await;
        let head = env.timeline("main").committed();
        env.dispatched("main", head).await;

        // The operator pause settles the decisions and stops before it
        // composes its record and trigger.
        let mut change = dim_goal::testing::hold_stage(
            env.session_id(),
            "main",
            dim_goal::testing::Stage::Change,
            head,
            Some(head),
        );
        let pause = env.goal("pause", json!({}));
        let (pause, fact) = tokio::join!(pause, async {
            assert_eq!(
                change.arrived().await,
                head,
                "{case}: the pause's writer is held at the cutoff it read"
            );
            let fact = match case {
                "done" => {
                    write_conclusion(&env, &create.branch, evaluation, "done", "DONE: verified")
                }
                "blocked" => write_conclusion(
                    &env,
                    &create.branch,
                    evaluation,
                    "blocked",
                    "BLOCKED: a deploy key",
                ),
                _ => {
                    env.emit(
                        "main",
                        EventKind::BranchEnded(BranchEndedData {
                            branch: create.branch.clone(),
                            end_event: 0,
                            source: BranchSource::Host {
                                operation_id: "user".into(),
                            },
                            ancestor: None,
                        }),
                    )
                    .sequence
                }
            };
            env.dispatched("main", fact).await;
            change.release();
            fact
        });
        pause.expect("the pause writes");
        let (needle, expected) = match case {
            "done" => (
                r#"status="completed""#,
                texts::evaluated("main", "ship it", "completed", "verified", fact),
            ),
            "blocked" => (
                r#"status="blocked""#,
                texts::evaluated("main", "ship it", "blocked", "a deploy key", fact),
            ),
            _ => (
                r#"reason="evaluation_failed""#,
                texts::evaluated_pause(
                    "main",
                    "ship it",
                    "evaluation_failed",
                    "the evaluator could not check the result",
                    fact,
                ),
            ),
        };
        let record = env
            .writes("main")
            .await
            .into_iter()
            .find(|block| block.text.contains(needle));
        assert_eq!(
            record.map(|block| block.text),
            Some(expected),
            "{case}: the fact folded at {fact} inside the settlement window is decided"
        );
        env.close().await;
    }
}

/// A decision that folds while a Set/Resume loads the current work is refused
/// admission after the settlement's own drain: the change states it before the
/// record that supersedes its fact (#2158).
#[tokio::test]
async fn a_decision_folded_while_a_change_loads_its_work_is_taken() {
    for case in ["done", "evaluation_failed"] {
        let env = Env::start().await;
        let (evaluation, create) = completion_requested(&env).await;
        let head = env.timeline("main").committed();
        env.dispatched("main", head).await;

        // The resume settles the decisions and then loads the branch's current
        // work, which waits for publication and queries the Timeline. The
        // window is held there, past the settlement's first drain.
        let mut query = dim_goal::testing::hold_stage(
            env.session_id(),
            "main",
            dim_goal::testing::Stage::Query,
            head,
            Some(head),
        );
        let resume = env.goal("resume", json!({}));
        let (resume, fact) = tokio::join!(resume, async {
            assert_eq!(
                query.arrived().await,
                head,
                "{case}: the resume's current-work load is held at the cutoff it read"
            );
            let fact = match case {
                "done" => {
                    write_conclusion(&env, &create.branch, evaluation, "done", "DONE: verified")
                }
                _ => {
                    env.emit(
                        "main",
                        EventKind::BranchEnded(BranchEndedData {
                            branch: create.branch.clone(),
                            end_event: 0,
                            source: BranchSource::Host {
                                operation_id: "user".into(),
                            },
                            ancestor: None,
                        }),
                    )
                    .sequence
                }
            };
            env.dispatched("main", fact).await;
            query.release();
            fact
        });
        resume.expect("the resume writes");
        let writes = env.writes("main").await;
        let expected = match case {
            "done" => texts::evaluated("main", "ship it", "completed", "verified", fact),
            _ => texts::evaluated_pause(
                "main",
                "ship it",
                "evaluation_failed",
                "the evaluator could not check the result",
                fact,
            ),
        };
        let markers: Vec<&str> = writes.iter().map(|block| block.marker.as_str()).collect();
        let record = writes.iter().find(|block| block.text == expected).unwrap_or_else(|| {
            panic!(
                "{case}: the fact folded at {fact} while the resume loaded its work is decided; writes: {markers:?}"
            )
        });
        let change = writes
            .iter()
            .find(|block| block.marker.contains(r#"op="resume""#))
            .expect("the resume trigger");
        assert!(
            record.sequence < change.sequence,
            "{case}: the decision at {fact} is stated at {} before the change writes at {}",
            record.sequence,
            change.sequence
        );
        env.close().await;
    }
}

/// A `done` or `blocked` conclusion that folds while a change holds task
/// admission states its decision even with a background task still open: the
/// background wait holds a `continue`, not a terminal conclusion (#2158).
#[tokio::test]
async fn a_terminal_conclusion_folded_in_the_settlement_window_is_stated() {
    for case in ["done", "blocked"] {
        let env = Env::start().await;
        let (evaluation, create) = completion_requested(&env).await;

        // A background task the branch started is still open when the
        // conclusion arrives.
        env.exec_started("main", "work", "task-1");
        let head = env.timeline("main").committed();
        env.dispatched("main", head).await;

        // The operator pause settles the decisions and stops before it
        // composes its record and trigger.
        let mut change = dim_goal::testing::hold_stage(
            env.session_id(),
            "main",
            dim_goal::testing::Stage::Change,
            head,
            Some(head),
        );
        let pause = env.goal("pause", json!({}));
        let (pause, fact) = tokio::join!(pause, async {
            assert_eq!(
                change.arrived().await,
                head,
                "{case}: the pause's writer is held at the cutoff it read"
            );
            let fact = match case {
                "done" => {
                    write_conclusion(&env, &create.branch, evaluation, "done", "DONE: verified")
                }
                _ => write_conclusion(
                    &env,
                    &create.branch,
                    evaluation,
                    "blocked",
                    "BLOCKED: a deploy key",
                ),
            };
            env.dispatched("main", fact).await;
            change.release();
            fact
        });
        pause.expect("the pause writes");
        let expected = match case {
            "done" => texts::evaluated("main", "ship it", "completed", "verified", fact),
            _ => texts::evaluated("main", "ship it", "blocked", "a deploy key", fact),
        };
        let writes = env.writes("main").await;
        let markers: Vec<&str> = writes.iter().map(|block| block.marker.as_str()).collect();
        assert!(
            writes.iter().any(|block| block.text == expected),
            "{case}: the conclusion folded at {fact} while a task is open is stated; writes: {markers:?}"
        );
        env.close().await;
    }
}

/// A deferred record whose append fails keeps its place: a later change states
/// the decision instead of losing it with the failed write (#2158).
#[tokio::test]
async fn a_failed_deferred_record_leaves_its_decision_to_the_next_change() {
    let env = Env::start().await;
    let (_, create) = completion_requested(&env).await;
    let head = env.timeline("main").committed();
    env.dispatched("main", head).await;

    // The operator pause settles the decisions and stops before it composes
    // its record and trigger.
    let mut change = dim_goal::testing::hold_stage(
        env.session_id(),
        "main",
        dim_goal::testing::Stage::Change,
        head,
        Some(head),
    );
    let pause = env.goal("pause", json!({}));
    let (pause, failed) = tokio::join!(pause, async {
        assert_eq!(
            change.arrived().await,
            head,
            "the pause's writer is held at the cutoff it read"
        );
        // The evaluator's line ends in that window: its failure is deferred to
        // the settlement the pause holds.
        let failed = env
            .emit(
                "main",
                EventKind::BranchEnded(BranchEndedData {
                    branch: create.branch.clone(),
                    end_event: 0,
                    source: BranchSource::Host {
                        operation_id: "user".into(),
                    },
                    ancestor: None,
                }),
            )
            .sequence;
        env.dispatched("main", failed).await;

        // The deferred record's append is refused: its published status no
        // longer matches the state its writer states.
        let failure = env.probe.fail_next_state();
        change.release();
        failure
            .await
            .expect("the writer refuses the deferred failure report");
        failed
    });
    assert_eq!(
        env.probe.failed_states(),
        1,
        "the writer's own check refuses the deferred record at {failed}"
    );
    assert!(
        pause.is_err(),
        "the change whose deferred record was refused reports the failure: {pause:?}"
    );

    // The next change states what the failed one kept.
    env.goal("resume", json!({}))
        .await
        .expect("the resume writes");
    let writes = env.writes("main").await;
    let markers: Vec<&str> = writes.iter().map(|block| block.marker.as_str()).collect();
    let expected = texts::evaluated_pause(
        "main",
        "ship it",
        "evaluation_failed",
        "the evaluator could not check the result",
        failed,
    );
    assert!(
        writes.iter().any(|block| block.text == expected),
        "the deferred record refused at {failed} is stated by the next change; writes: {markers:?}"
    );
    env.close().await;
}

/// A conclusion or an evaluator failure that commits after the evaluation
/// request drained and folds before its batch is stated first: the request
/// writes only while the cutoff that drain read still stands (#2172).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_decision_folded_in_the_evaluation_request_window_is_stated() {
    for case in ["done", "blocked", "evaluation_failed"] {
        let env = Env::start().await;
        let (evaluation, create) = completion_requested(&env).await;
        let head = env.timeline("main").committed();
        env.dispatched("main", head).await;

        // The request settles its decisions, drains, and stops before it
        // commits the record that states the request.
        let mut batch = dim_goal::testing::hold_stage(
            env.session_id(),
            "main",
            dim_goal::testing::Stage::Batch,
            head,
            Some(head),
        );
        let report = env.tool(
            "main",
            json!({"status": "completed", "summary": "all tests pass"}),
        );
        let (_, fact) = tokio::join!(report, async {
            assert_eq!(
                batch.arrived().await,
                head,
                "{case}: the request is held at the cutoff it drained"
            );
            let fact = match case {
                "done" => {
                    write_conclusion(&env, &create.branch, evaluation, "done", "DONE: verified")
                }
                "blocked" => write_conclusion(
                    &env,
                    &create.branch,
                    evaluation,
                    "blocked",
                    "BLOCKED: a deploy key",
                ),
                _ => {
                    env.emit(
                        "main",
                        EventKind::BranchEnded(BranchEndedData {
                            branch: create.branch.clone(),
                            end_event: 0,
                            source: BranchSource::Host {
                                operation_id: "user".into(),
                            },
                            ancestor: None,
                        }),
                    )
                    .sequence
                }
            };
            assert_eq!(
                fact,
                head + 1,
                "{case}: the decision commits behind the request's drain"
            );
            env.dispatched("main", fact).await;
            batch.release();
            fact
        });
        let writes = env.writes("main").await;
        let markers: Vec<&str> = writes.iter().map(|block| block.marker.as_str()).collect();
        assert_decision_and_state(&writes, &markers, case, fact);
        env.close().await;
    }
}

/// A conclusion that commits behind the evaluation request's drain and folds
/// after its batch leaves that batch unwritten: the request drains again,
/// states the decision, and composes its own record from the fold that holds
/// it (#2172).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_decision_folded_while_the_evaluation_request_batch_is_held_is_stated() {
    for case in ["done", "blocked"] {
        let env = Env::start().await;
        let (evaluation, create) = completion_requested(&env).await;
        let head = env.timeline("main").committed();
        env.dispatched("main", head).await;

        // The request is held before its batch, and the fold holds the
        // decision the test commits in that window.
        let mut batch = dim_goal::testing::hold_stage(
            env.session_id(),
            "main",
            dim_goal::testing::Stage::Batch,
            head,
            Some(head),
        );
        let mut folded = dim_goal::testing::hold_input(env.session_id(), "main", head + 1);
        let report = env.tool(
            "main",
            json!({"status": "completed", "summary": "all tests pass"}),
        );
        let (_, fact) = tokio::join!(report, async {
            assert_eq!(
                batch.arrived().await,
                head,
                "{case}: the request is held at the cutoff it drained"
            );
            let fact = match case {
                "done" => {
                    write_conclusion(&env, &create.branch, evaluation, "done", "DONE: verified")
                }
                _ => write_conclusion(
                    &env,
                    &create.branch,
                    evaluation,
                    "blocked",
                    "BLOCKED: a deploy key",
                ),
            };
            assert_eq!(
                fact,
                head + 1,
                "{case}: the decision commits behind the request's drain"
            );
            assert_eq!(
                folded.arrived().await,
                fact,
                "{case}: the fold holds the decision"
            );
            batch.release();
            folded.release();
            fact
        });
        let writes = env.writes("main").await;
        let markers: Vec<&str> = writes.iter().map(|block| block.marker.as_str()).collect();
        assert_decision_and_state(&writes, &markers, case, fact);
        env.close().await;
    }
}

/// The evaluation request writes its record and its trigger as one adjacent
/// pair, record first, on the branch that admits it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_evaluation_request_writes_one_adjacent_pair() {
    let env = Env::start().await;
    let line = env
        .open_branch(
            "main",
            "line work",
            BranchSource::Host {
                operation_id: "test:line".into(),
            },
        )
        .await;
    env.goal(
        "set",
        json!({"objective": "ship it", "judge": "evaluator", "branchId": line}),
    )
    .await
    .expect("set writes");
    env.wait_for(&line, r#"op="set""#).await;
    let head = env.timeline(&line).committed();
    env.dispatched(&line, head).await;

    let result = env
        .tool(
            &line,
            json!({"status": "completed", "summary": "all tests pass"}),
        )
        .await;
    assert_ne!(result.is_error, Some(true), "the request is admitted");
    let written: Vec<_> = env
        .all_events(&line)
        .await
        .into_iter()
        .filter(|event| event.sequence > head)
        .collect();
    let markers: Vec<&str> = written
        .iter()
        .map(|event| match &event.kind {
            EventKind::InputMessage(data) => match &data.blocks[0] {
                InputBlock::Raw { text, .. } => text.lines().next().unwrap_or_default(),
            },
            other => other.key(),
        })
        .collect();
    assert_eq!(
        written.len(),
        2,
        "the request writes its record and trigger and nothing else: {markers:#?}"
    );
    assert_eq!(written[0].sequence, head + 1, "the record comes first");
    assert_eq!(
        written[1].sequence,
        head + 2,
        "the trigger follows the record"
    );
    assert!(
        markers[0].contains(r#"kind="state""#) && markers[0].contains(r#"trigger="goal:"#),
        "the first input is the record that names the trigger: {markers:#?}"
    );
    assert!(
        markers[1].contains(r#"kind="goal" op="completed""#),
        "the second input is the request's trigger: {markers:#?}"
    );
    env.close().await;
}

/// A branch end that commits between the request's drain and its append wins
/// admission: the request is refused and writes neither its record nor its
/// trigger, even while the ending line's Timeline is still open (#2172 review).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_end_that_wins_the_evaluation_request_admission_writes_no_input() {
    let env = Env::start_controllable().await;
    let line = env
        .open_branch(
            "main",
            "line work",
            BranchSource::Host {
                operation_id: "test:line".into(),
            },
        )
        .await;
    env.goal(
        "set",
        json!({"objective": "ship it", "judge": "evaluator", "branchId": line}),
    )
    .await
    .expect("set writes");
    env.wait_for(&line, r#"op="set""#).await;
    // A Tool end of a running line commits its `branch/end` and is parked
    // until the line's turn ends: the branch has ended while its Timeline is
    // still open.
    env.probe.set_running(&line, true);
    let head = env.timeline(&line).committed();
    env.dispatched(&line, head).await;

    // The request settles its decisions, drains, and stops before it appends.
    let mut batch = dim_goal::testing::hold_stage(
        env.session_id(),
        &line,
        dim_goal::testing::Stage::Batch,
        head,
        Some(head),
    );
    let report = env.tool(
        &line,
        json!({"status": "completed", "summary": "all tests pass"}),
    );
    let (result, end) = tokio::join!(report, async {
        assert_eq!(
            batch.arrived().await,
            head,
            "the request is held at the cutoff it drained"
        );
        let commands = env.probe.commands(&line);
        commands
            .end(
                line.clone(),
                BranchSource::Tool {
                    turn_id: "turn".into(),
                    request_id: "request".into(),
                    call_id: "end".into(),
                },
            )
            .await
            .expect("the line's end is admitted and parked");
        let end = env.timeline(&line).committed();
        assert_eq!(end, head + 1, "the branch end commits behind the drain");
        env.dispatched(&line, end).await;
        batch.release();
        end
    });
    assert_eq!(
        result.is_error,
        Some(true),
        "the ended branch refuses the evaluation request"
    );
    let after: Vec<String> = env
        .writes(&line)
        .await
        .into_iter()
        .filter(|block| block.sequence > end)
        .map(|block| format!("{} at {}", block.marker, block.sequence))
        .collect();
    assert!(
        after.is_empty(),
        "the refused request writes neither its record nor its trigger, and the branch takes no\
         other input after its end; writes after {end}: {after:#?}"
    );
    env.probe.set_running(&line, false);
    env.close().await;
}

/// A close that starts between the request's drain and its append wins
/// admission: the request is refused and writes neither its record nor its
/// trigger (#2172 review).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_close_that_wins_the_evaluation_request_admission_writes_no_input() {
    let env = Env::start().await;
    let line = env
        .open_branch(
            "main",
            "line work",
            BranchSource::Host {
                operation_id: "test:line".into(),
            },
        )
        .await;
    env.goal(
        "set",
        json!({"objective": "ship it", "judge": "evaluator", "branchId": line}),
    )
    .await
    .expect("set writes");
    env.wait_for(&line, r#"op="set""#).await;
    let head = env.timeline(&line).committed();
    env.dispatched(&line, head).await;

    let mut batch = dim_goal::testing::hold_stage(
        env.session_id(),
        &line,
        dim_goal::testing::Stage::Batch,
        head,
        Some(head),
    );
    let mut close = dim_agent_runtime::testing::hold_close(env.session_id(), &line);
    let report = env.tool(
        &line,
        json!({"status": "completed", "summary": "all tests pass"}),
    );
    let (result, ()) = tokio::join!(report, async {
        assert_eq!(
            batch.arrived().await,
            head,
            "the request is held at the cutoff it drained"
        );
        let branch = env
            .probe
            .commands(&line)
            .get(&line)
            .expect("the line is open");
        let closing = tokio::spawn(async move { branch.close().await });
        close.arrived().await;
        batch.release();
        close.release();
        closing
            .await
            .expect("the close task")
            .expect("the line closes");
    });
    assert_eq!(
        result.is_error,
        Some(true),
        "the closing branch refuses the evaluation request"
    );
    let after: Vec<String> = env
        .writes(&line)
        .await
        .into_iter()
        .filter(|block| block.sequence > head)
        .map(|block| format!("{} at {}", block.marker, block.sequence))
        .collect();
    assert!(
        after
            .iter()
            .all(|marker| !marker.contains(r#"trigger="goal:"#)
                && !marker.contains(r#"op="completed""#)),
        "the refused request writes neither its record nor its trigger; writes after {head}: {after:#?}"
    );
    env.close().await;
}

/// Asserts that the decision committed at `fact` has its own state record and
/// that the request's record keeps it instead of restoring `active`.
fn assert_decision_and_state(writes: &[Block], markers: &[&str], case: &str, fact: u64) {
    let (needle, expected) = match case {
        "done" => (
            r#"status="completed""#,
            texts::evaluated("main", "ship it", "completed", "verified", fact),
        ),
        "blocked" => (
            r#"status="blocked""#,
            texts::evaluated("main", "ship it", "blocked", "a deploy key", fact),
        ),
        _ => (
            r#"reason="evaluation_failed""#,
            texts::evaluated_pause(
                "main",
                "ship it",
                "evaluation_failed",
                "the evaluator could not check the result",
                fact,
            ),
        ),
    };
    assert!(
        writes.iter().any(|block| block.text == expected),
        "{case}: the decision committed at {fact} is stated; writes: {markers:#?}"
    );
    let last = writes
        .iter()
        .rev()
        .find(|block| block.marker.contains(r#"kind="state""#))
        .expect("the request's state record");
    assert!(
        last.marker.contains(needle),
        "{case}: the goal keeps the state decided at {fact}; writes: {markers:#?}"
    );
}

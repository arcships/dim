//! Goal's cases over the no-Loop part harness: the real Dispatcher,
//! Timeline, TimelineTree, SessionMethods, ToolRegistry, PromptGuide,
//! storage and Settings, with the facts the Loop would write supplied by the
//! test. Every expectation is a literal text or a written input.
use crate::support;

use dim_agent_types::{BranchSource, EventKind, InputMode, ToolContent, ToolResult};
use serde_json::json;
use support::parts::*;
use support::texts;

fn tool_text(result: &ToolResult) -> String {
    match &result.content {
        ToolContent::Text(text) => text.clone(),
        ToolContent::Blocks(_) => panic!("a text result"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn goal_reads_current_settings_and_stops_decisions_on_read_failure() {
    let env = Env::start().await;
    env.set_limit(1);
    let set = env.set("ship it").await;
    let end = env.turn("main", &[&set]);
    let continuation = env.wait_for("main", r#"kind="continuation""#).await;
    assert_eq!(
        continuation.text,
        texts::continuation("main", 1, Some(1), 0, end.sequence)
    );
    assert_eq!(
        env.writes("main").await.len(),
        3,
        "the set trigger, its state record and one continuation"
    );

    // An unreadable document produces no automatic input.
    std::fs::write(env.home().join("goal.json"), r#"{"maxContinuations":0}"#).unwrap();
    env.turn("main", &[&continuation]);
    assert_eq!(
        env.settled("main").await.len(),
        3,
        "an unreadable limit must not produce an automatic input"
    );

    // An explicit null is not a value either (A4).
    std::fs::write(env.home().join("goal.json"), r#"{"maxContinuations":null}"#).unwrap();
    env.turn("main", &[]);
    assert_eq!(
        env.settled("main").await.len(),
        3,
        "an explicit null must not produce an automatic input"
    );

    let interrupt = env.interrupt("main");
    let pause = env.wait_for("main", r#"reason="interrupted""#).await;
    assert_eq!(
        pause.text,
        texts::automatic_pause(
            "main",
            "ship it",
            "interrupted",
            "the user interrupted the turn",
            interrupt.sequence
        )
    );
    env.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_line_without_a_goal_reads_no_settings_at_its_turn_ends() {
    let env = Env::start().await;
    env.set_limit(1);
    for step in 0..3 {
        let input = env.emit("main", user(&format!("step {step}"), InputMode::Queue));
        env.turn_ids("main", &[&input.id]);
    }
    assert!(
        env.settled("main").await.is_empty(),
        "no goal, so nothing is written"
    );
    env.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_goal_update_tool_writes_its_report_and_refuses_bad_arguments() {
    let env = Env::start().await;
    let set = env.set("ship it").await;
    assert_eq!(set.blocks.len(), 1, "a change trigger is one block");

    let refused = env
        .tool("main", json!({"status": "elsewhere", "summary": "x"}))
        .await;
    assert_eq!(refused.is_error, Some(true));
    assert_eq!(tool_text(&refused), texts::TOOL_INVALID);
    let refused = env.tool("main", json!({"status": "completed"})).await;
    assert_eq!(refused.is_error, Some(true));
    assert_eq!(tool_text(&refused), texts::TOOL_INVALID);
    assert_eq!(
        env.settled("main").await.len(),
        2,
        "the set trigger and its record; refusals write nothing"
    );

    let accepted = env
        .tool(
            "main",
            json!({"status": "completed", "summary": "all done"}),
        )
        .await;
    assert_eq!(accepted.is_error, None);
    assert_eq!(tool_text(&accepted), "goal_update recorded: completed.");
    let report = env.wait_for("main", r#"op="completed""#).await;
    assert_eq!(report.text, texts::report("main", "completed", "all done"));
    assert_eq!(report.mode, InputMode::Append);

    let accepted = env
        .tool("main", json!({"status": "blocked", "summary": "need keys"}))
        .await;
    assert_eq!(tool_text(&accepted), "goal_update recorded: blocked.");
    let report = env.wait_for("main", r#"op="blocked""#).await;
    assert_eq!(report.text, texts::report("main", "blocked", "need keys"));
    env.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_guide_entry_and_the_tool_declaration_are_the_shipped_texts() {
    let env = Env::start().await;
    let guide = env.guide("main");
    assert!(guide.contains(texts::ENTRY), "{guide:?}");
    let definitions = env.definitions();
    let tool = definitions
        .iter()
        .find(|definition| definition.name == "goal_update")
        .unwrap_or_else(|| panic!("no goal_update: {definitions:#?}"));
    assert_eq!(tool.description, texts::TOOL_DESCRIPTION);
    assert_eq!(
        serde_json::to_value(&tool.input_schema).unwrap(),
        json!({
            "type": "object",
            "properties": {
                "status": {"type": "string", "enum": ["completed", "blocked"]},
                "summary": {"type": "string", "minLength": 1, "maxLength": 2000},
            },
            "required": ["status", "summary"],
            "additionalProperties": false,
        })
    );
    env.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_absent_limit_continues_past_twenty_turns() {
    let env = Env::start().await;
    let set = env.set("ship it").await;
    env.turn("main", &[&set]);
    let mut continuation = env.wait_for("main", r#"kind="continuation""#).await;
    for count in 1..21 {
        assert!(
            !continuation.marker.contains("limit="),
            "an absent limit writes no limit: {continuation:#?}"
        );
        let end = env.tool_turn("main", &[&continuation]);
        continuation = env
            .wait_for(
                "main",
                &format!(
                    r#"kind="continuation" count="{}" after="{}""#,
                    count + 1,
                    end.sequence
                ),
            )
            .await;
    }
    let writes = env.settled("main").await;
    assert_eq!(
        writes.len(),
        23,
        "the set trigger, its record and 21 continuations: {writes:#?}"
    );
    assert!(
        writes
            .iter()
            .all(|block| !block.marker.contains(r#"op="pause""#)),
        "an absent limit never pauses: {writes:#?}"
    );
    env.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_explicit_limit_pauses_after_the_fifth_continuation_turn() {
    let env = Env::start().await;
    env.set_limit(5);
    let set = env.set("ship it").await;
    env.turn("main", &[&set]);
    let mut continuation = env.wait_for("main", r#"kind="continuation""#).await;
    for count in 1..5 {
        let end = env.tool_turn("main", &[&continuation]);
        continuation = env
            .wait_for(
                "main",
                &format!(
                    r#"kind="continuation" count="{}" limit="5" after="{}""#,
                    count + 1,
                    end.sequence
                ),
            )
            .await;
    }
    let end = env.tool_turn("main", &[&continuation]);
    let pause = env.wait_for("main", r#"reason="limit""#).await;
    assert_eq!(
        pause.text,
        texts::automatic_pause(
            "main",
            "ship it",
            "limit",
            "the automatic continuation limit was reached",
            end.sequence
        )
    );
    assert_eq!(
        env.settled("main").await.len(),
        8,
        "the set trigger, its record, five continuations and the pause"
    );
    env.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn three_continued_text_only_turns_pause_no_progress() {
    let env = Env::start().await;
    let set = env.set("ship it").await;
    let end = env.turn("main", &[&set]);
    let first = env
        .wait_for("main", r#"kind="continuation" count="1""#)
        .await;
    assert_eq!(
        first.text,
        texts::continuation("main", 1, None, 0, end.sequence)
    );

    let end = env.turn("main", &[&first]);
    let second = env
        .wait_for("main", r#"kind="continuation" count="2""#)
        .await;
    assert_eq!(
        second.text,
        texts::continuation("main", 2, None, 1, end.sequence)
    );

    let end = env.turn("main", &[&second]);
    let third = env
        .wait_for("main", r#"kind="continuation" count="3""#)
        .await;
    assert_eq!(
        third.text,
        texts::continuation("main", 3, None, 2, end.sequence)
    );

    let end = env.turn("main", &[&third]);
    let pause = env.wait_for("main", r#"reason="no_progress""#).await;
    assert_eq!(
        pause.text,
        texts::automatic_pause(
            "main",
            "ship it",
            "no_progress",
            "the goal made no tool call in its last 3 continued turns",
            end.sequence
        )
    );
    assert_eq!(
        env.settled("main").await.len(),
        6,
        "the set trigger, its record, three continuations and the pause"
    );
    env.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_tool_call_or_operator_input_resets_the_streak() {
    // A tool call in a continued turn resets the streak.
    let env = Env::start().await;
    let set = env.set("ship it").await;
    env.turn("main", &[&set]);
    let first = env
        .wait_for("main", r#"kind="continuation" count="1""#)
        .await;
    let end = env.tool_turn("main", &[&first]);
    let second = env
        .wait_for("main", r#"kind="continuation" count="2""#)
        .await;
    assert_eq!(
        second.text,
        texts::continuation("main", 2, None, 0, end.sequence)
    );
    env.close().await;

    // An operator input restarts the streak: without it the third continued
    // turn would pause.
    let env = Env::start().await;
    let set = env.set("ship it").await;
    env.turn("main", &[&set]);
    let first = env
        .wait_for("main", r#"kind="continuation" count="1""#)
        .await;
    env.turn("main", &[&first]);
    let second = env
        .wait_for("main", r#"kind="continuation" count="2""#)
        .await;
    env.turn("main", &[&second]);
    let third = env
        .wait_for("main", r#"kind="continuation" count="3""#)
        .await;
    let operator = env.emit("main", user("also the changelog", InputMode::Queue));
    let end = env.turn_ids("main", &[&third.id, &operator.id]);
    let fourth = env
        .wait_for(
            "main",
            &format!(
                r#"kind="continuation" count="1" after="{}" idle="1""#,
                end.sequence
            ),
        )
        .await;
    assert_eq!(
        fourth.text,
        texts::continuation("main", 1, None, 1, end.sequence)
    );
    assert!(
        env.settled("main")
            .await
            .iter()
            .all(|block| !block.text.contains(r#"reason="no_progress""#)),
        "the operator input keeps the goal running"
    );
    env.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reload_drops_the_streak_and_explicit_resume_starts_at_zero() {
    let env = Env::start().await;
    let mut seed = env.seed_new("load-streak").await;
    let set = seed.append(
        "main",
        message(
            vec![raw(&texts::set("main", "ship it"), None, false)],
            InputMode::Queue,
        ),
    );
    seed.append(
        "main",
        goal_state(Some("ship it"), serde_json::json!({"kind":"active"}), false),
    );
    seed.append(
        "main",
        EventKind::TurnStart(dim_agent_types::TurnStartData {
            turn_id: "t1".into(),
        }),
    );
    seed.append(
        "main",
        EventKind::InputConsumed(dim_agent_types::InputConsumedData {
            input_id: set.id.clone(),
        }),
    );
    let end = seed.append("main", turn_end_with("t1", "completed"));
    let first = seed.append(
        "main",
        message(
            vec![raw(
                &texts::continuation("main", 1, None, 0, end.sequence),
                None,
                false,
            )],
            InputMode::Queue,
        ),
    );
    seed.append(
        "main",
        EventKind::TurnStart(dim_agent_types::TurnStartData {
            turn_id: "t2".into(),
        }),
    );
    seed.append(
        "main",
        EventKind::InputConsumed(dim_agent_types::InputConsumedData {
            input_id: first.id.clone(),
        }),
    );
    let end = seed.append("main", turn_end_with("t2", "completed"));
    seed.append(
        "main",
        message(
            vec![raw(
                &texts::continuation("main", 2, None, 1, end.sequence),
                None,
                false,
            )],
            InputMode::Queue,
        ),
    );
    seed.finish().await;

    let env = env.restore_seeded("load-streak").await;
    assert_eq!(
        env.settled("main").await.len(),
        3,
        "the restore repeats no decision"
    );
    assert_eq!(
        env.goal("get", serde_json::json!({})).await.unwrap()["goal"]["status"]["kind"],
        "paused"
    );
    let old = env.writes("main").await[2].clone();
    env.turn("main", &[&old]);
    assert_eq!(env.settled("main").await.len(), 3);
    env.goal("resume", serde_json::json!({})).await.unwrap();
    let resume = env.wait_for("main", r#"op="resume""#).await;
    let end = env.turn("main", &[&resume]);
    let first = env
        .wait_for(
            "main",
            &format!(r#"kind="continuation" count="1" after="{}""#, end.sequence),
        )
        .await;
    assert_eq!(
        first.text,
        texts::continuation("main", 1, None, 0, end.sequence)
    );
    env.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_open_background_task_defers_the_turn_end_until_its_end() {
    // A completed background end does not wake in this harness; its wake is a
    // separate input whose turn end decides.
    let env = Env::start().await;
    let set = env.set("ship it").await;
    env.turn("main", &[&set]);
    let first = env
        .wait_for("main", r#"kind="continuation" count="1""#)
        .await;
    env.turn_start("main", "bg");
    env.tool_call("main", "bg");
    env.consume("main", &first.id);
    env.exec_started("main", "bg", "task-1");
    env.end_turn("main", "bg", "completed");
    assert_eq!(
        env.settled("main").await.len(),
        3,
        "an open task defers the turn end"
    );
    env.exec_ended("main", "task-1", "completed");
    assert_eq!(
        env.settled("main").await.len(),
        3,
        "a completed end waits for its wake"
    );

    let wake = env.write_message(
        "main",
        vec![raw(
            "<dim-notification task=\"task-1\">\nbackground task finished\n</dim-notification>",
            None,
            false,
        )],
        InputMode::Queue,
    );
    let end = env.turn_ids("main", &[&wake.id]);
    let second = env
        .wait_for("main", r#"kind="continuation" count="2""#)
        .await;
    assert_eq!(
        second.text,
        texts::continuation("main", 2, None, 0, end.sequence)
    );
    env.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cancelled_background_end_takes_the_deferred_decision_once() {
    let env = Env::start().await;
    let set = env.set("ship it").await;
    env.turn("main", &[&set]);
    let first = env
        .wait_for("main", r#"kind="continuation" count="1""#)
        .await;
    env.turn_start("main", "bg");
    env.tool_call("main", "bg");
    env.consume("main", &first.id);
    env.exec_started("main", "bg", "task-1");
    let end = env.end_turn("main", "bg", "completed");
    assert_eq!(env.settled("main").await.len(), 3);

    env.exec_ended("main", "task-1", "cancelled");
    let second = env
        .wait_for("main", r#"kind="continuation" count="2""#)
        .await;
    assert_eq!(
        second.text,
        texts::continuation("main", 2, None, 0, end.sequence)
    );
    assert_eq!(
        env.settled("main").await.len(),
        4,
        "the deferred decision runs exactly once"
    );
    env.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reloaded_goal_does_not_wait_for_or_continue_after_an_old_task() {
    let env = Env::start().await;
    let mut seed = env.seed_new("load-open-task").await;
    let set = seed.append(
        "main",
        message(
            vec![raw(&texts::set("main", "ship it"), None, false)],
            InputMode::Queue,
        ),
    );
    seed.append(
        "main",
        goal_state(Some("ship it"), serde_json::json!({"kind":"active"}), false),
    );
    seed.append(
        "main",
        EventKind::TurnStart(dim_agent_types::TurnStartData {
            turn_id: "t1".into(),
        }),
    );
    seed.append(
        "main",
        EventKind::InputConsumed(dim_agent_types::InputConsumedData {
            input_id: set.id.clone(),
        }),
    );
    let end = seed.append("main", turn_end_with("t1", "completed"));
    let first = seed.append(
        "main",
        message(
            vec![raw(
                &texts::continuation("main", 1, None, 0, end.sequence),
                None,
                false,
            )],
            InputMode::Queue,
        ),
    );
    seed.append(
        "main",
        EventKind::TurnStart(dim_agent_types::TurnStartData {
            turn_id: "t2".into(),
        }),
    );
    seed.append(
        "main",
        EventKind::InputConsumed(dim_agent_types::InputConsumedData {
            input_id: first.id.clone(),
        }),
    );
    seed.append(
        "main",
        EventKind::ToolCall(dim_agent_types::ToolCallData {
            turn_id: "t2".into(),
            request_id: "r".into(),
            calls: vec![dim_agent_types::ToolCall {
                id: "c".into(),
                name: "exec".into(),
                input: serde_json::Map::new(),
            }],
        }),
    );
    seed.append(
        "main",
        EventKind::ExecStarted(dim_agent_types::ExecStartedData {
            turn_id: "t2".into(),
            request_id: "r".into(),
            call_id: "c".into(),
            task_id: "task-1".into(),
            description: "background work".into(),
            title: None,
            output_path: "/dev/null".into(),
        }),
    );
    seed.append("main", turn_end_with("t2", "completed"));
    seed.finish().await;

    let env = env.restore_seeded("load-open-task").await;
    assert_eq!(
        env.settled("main").await.len(),
        2,
        "the open task defers the end"
    );
    env.exec_ended("main", "task-1", "cancelled");
    assert_eq!(
        env.goal("get", serde_json::json!({})).await.unwrap()["goal"]["status"]["kind"],
        "paused"
    );
    assert_eq!(
        env.settled("main").await.len(),
        2,
        "ending an old task does not restart the Goal"
    );
    env.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn set_reopens_the_goal_and_the_count_restarts() {
    let env = Env::start().await;
    env.set_limit(2);
    let set = env.set("ship it").await;
    let end = env.turn("main", &[&set]);
    let first = env.wait_for("main", r#"kind="continuation""#).await;
    assert_eq!(
        first.text,
        texts::continuation("main", 1, Some(2), 0, end.sequence)
    );

    env.tool("main", json!({"status": "completed", "summary": "done"}))
        .await;
    let report = env.wait_for("main", r#"op="completed""#).await;
    assert_eq!(report.text, texts::report("main", "completed", "done"));
    env.turn("main", &[&first]);
    assert_eq!(
        env.settled("main").await.len(),
        5,
        "a completed goal writes no continuation"
    );

    env.goal("resume", json!({})).await.expect("resume writes");
    let resume = env.wait_for("main", r#"op="resume""#).await;
    assert_eq!(resume.text, texts::resume("main"));
    let end = env.turn("main", &[&report, &resume]);
    let again = env
        .wait_for(
            "main",
            &format!(
                r#"kind="continuation" count="1" limit="2" after="{}""#,
                end.sequence
            ),
        )
        .await;
    assert_eq!(
        again.text,
        texts::continuation("main", 1, Some(2), 0, end.sequence),
        "resume restarts the count"
    );
    env.close().await;
}

/// A Queue that commits while the continuation's submission lookup runs
/// leaves the decision's head behind it: the append refuses the stale
/// continuation, and the Queue's own turn decides (#1989).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_queue_committed_during_the_submission_lookup_holds_the_continuation() {
    let env = Env::start_gated().await;
    env.set_limit(5);
    let set = env.set("make the tests pass").await;
    let _set_end = env.turn("main", &[&set]);
    let first = env
        .wait_for("main", r#"kind="continuation" count="1""#)
        .await;

    let (entered, release) = env.gated().hold_query("input/message");
    let second_end = env.tool_turn("main", &[&first]);
    tokio::time::timeout(std::time::Duration::from_secs(15), entered)
        .await
        .expect("the continuation's submission lookup starts")
        .expect("the held lookup is the continuation's");
    let queued = env
        .session
        .submit("keep going".to_owned(), InputMode::Queue, "main")
        .await
        .expect("the operator input commits");
    assert!(
        queued.sequence > second_end.sequence,
        "the Queue at {} commits after the turn end at {}",
        queued.sequence,
        second_end.sequence
    );

    release.send(()).expect("the lookup answers");
    env.dispatched("main", queued.sequence).await;
    let writes = env.settled("main").await;
    let stale: Vec<String> = writes
        .iter()
        .filter(|block| {
            block.marker.contains(r#"kind="continuation""#)
                && block
                    .marker
                    .contains(&format!(r#"after="{}""#, second_end.sequence))
        })
        .map(|block| format!("{} at {}", block.marker, block.sequence))
        .collect();
    assert!(
        stale.is_empty(),
        "the turn end at {} writes no continuation after the Queue at {} commits inside the lookup window, yet it wrote {stale:?}",
        second_end.sequence,
        queued.sequence
    );
    assert!(
        env.pending_inputs("main").await.contains(&queued.id),
        "the Queue drives the next turn"
    );
    env.close().await;
}

/// Control for the case above: the same Queue, committed before the turn end
/// decides, holds the continuation. Only the commit point differs.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_queue_committed_before_the_decision_holds_the_continuation() {
    let env = Env::start_gated().await;
    env.set_limit(5);
    let set = env.set("make the tests pass").await;
    let _set_end = env.turn("main", &[&set]);
    let first = env
        .wait_for("main", r#"kind="continuation" count="1""#)
        .await;

    let queued = env
        .session
        .submit("keep going".to_owned(), InputMode::Queue, "main")
        .await
        .expect("the operator input commits");
    env.dispatched("main", queued.sequence).await;
    let second_end = env.tool_turn("main", &[&first]);
    let writes = env.settled("main").await;
    assert!(
        !writes.iter().any(|block| {
            block.marker.contains(r#"kind="continuation""#)
                && block
                    .marker
                    .contains(&format!(r#"after="{}""#, second_end.sequence))
        }),
        "a Queue the fold holds before the decision writes no continuation: {writes:#?}"
    );
    env.close().await;
}

/// The pause a turn end decides is stated at the fold the record writer
/// composes from: a Queue that commits in the settlement window is still
/// pending there, so the pause waits for that Queue's own turn end instead of
/// stating the state it already replaced (#2119, C4, C5).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_queue_committed_before_the_pause_record_holds_the_pause() {
    let env = Env::start_gated().await;
    env.set_limit(1);
    let set = env.set("make the tests pass").await;
    let _set_end = env.turn("main", &[&set]);
    let first = env
        .wait_for("main", r#"kind="continuation" count="1""#)
        .await;

    // The limit is reached, so this turn end decides a pause. Its record
    // writer is held where it settles the decisions and composes the record.
    let mut held = dim_goal::testing::hold_stage(
        env.session_id(),
        "main",
        dim_goal::testing::Stage::Record,
        first.sequence,
        None,
    );
    let second_end = env.tool_turn("main", &[&first]);
    assert_eq!(
        held.arrived().await,
        second_end.sequence,
        "the pause's record writer answers the turn end at {}",
        second_end.sequence
    );

    // A driving input commits and folds behind it, and the goal's decisions
    // are settled, so nothing else states the state that input opens. It is no
    // operator input, so the count that reached the limit stands.
    let queued = env.emit(
        "main",
        message(vec![raw("keep going", None, false)], InputMode::Queue),
    );
    assert!(
        queued.sequence > second_end.sequence,
        "the Queue at {} commits after the turn end at {}",
        queued.sequence,
        second_end.sequence
    );
    env.dispatched("main", queued.sequence).await;
    held.release();

    let end = env.turn_ids("main", &[&queued.id]);
    env.dispatched("main", end.sequence).await;
    let stale = tokio::time::timeout(std::time::Duration::from_millis(750), async {
        loop {
            let stale: Vec<String> = env
                .settled("main")
                .await
                .iter()
                .filter(|block| {
                    block.marker.contains(r#"kind="state""#)
                        && block.marker.contains(r#"status="paused""#)
                        && block
                            .marker
                            .contains(&format!(r#"after="{}""#, second_end.sequence))
                })
                .map(|block| format!("{} at {}", block.marker, block.sequence))
                .collect();
            if !stale.is_empty() {
                return stale;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await;
    assert!(
        stale.is_err(),
        "the turn end at {} writes no pause once the Queue at {} is pending in the fold, yet it wrote {:?}",
        second_end.sequence,
        queued.sequence,
        stale.unwrap_or_default()
    );

    let pause = env.wait_for("main", r#"reason="limit""#).await;
    assert_eq!(
        pause.text,
        texts::automatic_pause(
            "main",
            "make the tests pass",
            "limit",
            "the automatic continuation limit was reached",
            end.sequence
        ),
        "the Queue's own turn end decides the pause"
    );
    env.close().await;
}

/// The pause record's append is as atomic as its decision: a Queue that
/// commits after the writer read the fold but before the append commits makes
/// the append write nothing, and the Queue's own turn end decides the pause
/// (#2119, C4, C5).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_queue_committed_before_the_pause_append_holds_the_pause() {
    let env = Env::start_gated().await;
    env.set_limit(1);
    let set = env.set("make the tests pass").await;
    let _set_end = env.turn("main", &[&set]);
    let first = env
        .wait_for("main", r#"kind="continuation" count="1""#)
        .await;

    // The limit is reached, so this turn end decides a pause. Its record
    // writer is held after it read the fold and before its append commits.
    let mut held = dim_goal::testing::hold_stage(
        env.session_id(),
        "main",
        dim_goal::testing::Stage::Append,
        first.sequence,
        None,
    );
    let second_end = env.tool_turn("main", &[&first]);
    assert_eq!(
        held.arrived().await,
        second_end.sequence,
        "the pause's record writer answers the turn end at {}",
        second_end.sequence
    );

    // A driving input commits in that window. The append commits only while
    // the head it composed from stands, so it writes nothing.
    let queued = env.emit(
        "main",
        message(vec![raw("keep going", None, false)], InputMode::Queue),
    );
    assert!(
        queued.sequence > second_end.sequence,
        "the Queue at {} commits after the turn end at {}",
        queued.sequence,
        second_end.sequence
    );
    env.dispatched("main", queued.sequence).await;
    held.release();

    let end = env.turn_ids("main", &[&queued.id]);
    let pause = env.wait_for("main", r#"reason="limit""#).await;
    assert_eq!(
        pause.text,
        texts::automatic_pause(
            "main",
            "make the tests pass",
            "limit",
            "the automatic continuation limit was reached",
            end.sequence
        ),
        "the Queue's own turn end decides the pause at {}",
        end.sequence
    );
    env.close().await;
}

/// An operator interrupt that folds in a turn-end pause's write window follows
/// the ended turn: the interrupt's own pause states the fact, and the turn end
/// states no pause of its own (#2119, Pursuit §4.1, C4).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_turn_end_pause_does_not_answer_a_later_interrupt() {
    let env = Env::start_gated().await;
    env.set_limit(1);
    let set = env.set("make the tests pass").await;
    let _set_end = env.turn("main", &[&set]);
    let first = env
        .wait_for("main", r#"kind="continuation" count="1""#)
        .await;

    // The limit is reached, so this turn end decides a pause. Its record writer
    // is held after it settled the decisions, before it composes.
    let mut held = dim_goal::testing::hold_stage(
        env.session_id(),
        "main",
        dim_goal::testing::Stage::Record,
        first.sequence,
        None,
    );
    let second_end = env.tool_turn("main", &[&first]);
    assert_eq!(
        held.arrived().await,
        second_end.sequence,
        "the pause's record writer answers the turn end at {}",
        second_end.sequence
    );

    // An operator interrupt commits in that window and folds before the writer
    // reads its cutoff again.
    let interrupt = env.interrupt("main");
    env.dispatched("main", interrupt.sequence).await;
    held.release();

    // The interrupt states the pause; the ended turn states none.
    let pause = env.wait_for("main", r#"status="paused""#).await;
    assert!(
        pause.marker.contains(r#"reason="interrupted""#),
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
                && block
                    .marker
                    .contains(&format!(r#"after="{}""#, second_end.sequence))
        })
        .map(|block| format!("{} at {}", block.marker, block.sequence))
        .collect();
    assert!(
        stale.is_empty(),
        "the turn end at {} writes no pause once the interrupt at {} follows it, yet it wrote {stale:?}",
        second_end.sequence,
        interrupt.sequence
    );
    env.close().await;
}

/// A state record whose append fails is reported to its caller: the writer
/// does not resume the decisions it settled, so the same pause is not decided
/// again without a new fact (#2119).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_state_record_takes_no_second_decision() {
    let env = Env::start_gated().await;
    env.set_limit(1);
    let set = env.set("make the tests pass").await;
    let _set_end = env.turn("main", &[&set]);
    let first = env
        .wait_for("main", r#"kind="continuation" count="1""#)
        .await;

    // The decision and any second one it may take are held where they read
    // the fold, so the case sees each of them.
    let mut decision = dim_goal::testing::hold_stage(
        env.session_id(),
        "main",
        dim_goal::testing::Stage::Decision,
        first.sequence,
        None,
    );
    let mut second = dim_goal::testing::hold_stage(
        env.session_id(),
        "main",
        dim_goal::testing::Stage::Decision,
        first.sequence,
        None,
    );

    // The pause's append fails: its published status no longer matches the
    // state its writer states, which the writer checks under the commit.
    let failure = env.probe.fail_next_state();
    let second_end = env.tool_turn("main", &[&first]);
    assert_eq!(
        decision.arrived().await,
        second_end.sequence,
        "the turn end at {} decides",
        second_end.sequence
    );
    decision.release();
    failure.await.expect("the writer refuses the pause");
    assert_eq!(
        env.probe.failed_states(),
        1,
        "the writer's own check refuses the pause at {}",
        second_end.sequence
    );
    assert!(
        !env.writes("main").await.iter().any(|block| block
            .marker
            .contains(&format!(r#"after="{}""#, second_end.sequence))),
        "a refused record writes nothing"
    );

    // The failed decision is not taken again while no fact moves the fold.
    let retried =
        tokio::time::timeout(std::time::Duration::from_millis(750), second.arrived()).await;
    assert!(
        retried.is_err(),
        "a failed state record takes no second decision: it answered {:?}",
        retried.ok()
    );
    env.close().await;
}

/// A state record whose append fails releases the settlement it held without
/// taking up the interrupt that settlement deferred: no new fact, so the
/// interrupt states no pause of its own, and the branch reports the failure to
/// a read because the settlement cancelled the writer's caller (#2119).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_state_record_takes_up_no_interrupt_it_deferred() {
    let env = Env::start().await;
    let set = env.set("make the tests pass").await;
    let end = env.turn("main", &[&set]);
    let evaluation = end.sequence;
    let evaluator = env
        .open_branch(
            "main",
            "check the result",
            BranchSource::Host {
                operation_id: format!("goal:evaluate:{evaluation}"),
            },
        )
        .await;
    env.dispatched("main", env.timeline("main").committed())
        .await;

    // The failure report's record writer is held where it settled the
    // decisions, before it composes its record.
    let mut record = dim_goal::testing::hold_stage(
        env.session_id(),
        "main",
        dim_goal::testing::Stage::Record,
        evaluation,
        None,
    );
    let conclusion = texts::conclusion(
        "main",
        &evaluator,
        evaluation,
        "failed",
        "the evaluator failed",
    );
    let conclusion = env
        .write_message(
            "main",
            vec![raw(&conclusion, None, false)],
            InputMode::Append,
        )
        .sequence;
    assert_eq!(
        record.arrived().await,
        conclusion,
        "the failure report's record writer answers the conclusion at {conclusion}"
    );

    // An operator interrupt commits in that window: task admission is
    // suspended, so its decision waits in the fold.
    let interrupt = env.interrupt("main");
    env.dispatched("main", interrupt.sequence).await;

    // The failure report's append fails. The interrupt's own record is held
    // where it would state its pause, so the case sees whether it is taken up,
    // and the writer that failed is held after it released the settlement and
    // before it answers its caller, so the interrupt's record has every chance
    // to run while nothing says the writer failed yet. The bound only keeps the
    // case from waiting forever: reaching either hold is what it must not do.
    let mut again = dim_goal::testing::hold_stage(
        env.session_id(),
        "main",
        dim_goal::testing::Stage::Record,
        interrupt.sequence,
        Some(interrupt.sequence),
    );
    let mut released = dim_goal::testing::hold_stage(
        env.session_id(),
        "main",
        dim_goal::testing::Stage::Released,
        conclusion,
        Some(conclusion),
    );
    let failure = env.probe.fail_next_state();
    record.release();
    failure
        .await
        .expect("the writer refuses the failure report");
    assert_eq!(
        env.probe.failed_states(),
        1,
        "the writer's own check refuses the failure report at {conclusion}"
    );
    assert_eq!(
        released.arrived().await,
        conclusion,
        "the writer that failed holds its answer after it released the settlement"
    );
    let taken = tokio::time::timeout(std::time::Duration::from_millis(750), again.arrived()).await;
    assert!(
        taken.is_err(),
        "a failed record takes up the interrupt it deferred, yet it answered {:?}",
        taken.ok()
    );
    let mut published = dim_goal::testing::hold_stage(
        env.session_id(),
        "main",
        dim_goal::testing::Stage::Published,
        conclusion,
        Some(conclusion),
    );
    released.release();
    let pauses: Vec<String> = env
        .writes("main")
        .await
        .iter()
        .filter(|block| block.marker.contains(r#"status="paused""#))
        .map(|block| format!("{} at {}", block.marker, block.sequence))
        .collect();
    assert!(
        pauses.is_empty(),
        "a failed record states no pause of its own, yet it wrote {pauses:?}"
    );

    // The settlement cancelled the writer's caller, so the branch publishes the
    // failure as it does for any write whose caller is gone: a live read
    // reports it instead of a state the branch could not state. Releasing the
    // writer does not wait for the publication, so the read waits on the held
    // step it follows (#2199).
    assert_eq!(
        tokio::time::timeout(std::time::Duration::from_secs(10), published.arrived())
            .await
            .expect("the failure publication arrives"),
        conclusion,
        "the failure publication answers the conclusion at {conclusion}"
    );
    published.release();
    let error = env
        .goal("get", json!({}))
        .await
        .expect_err("a failed record whose caller the settlement cancelled is published");
    assert_eq!(
        error.data.as_ref().map(|data| data.app_error.as_str()),
        Some("session_invalid_input"),
        "the read reports the refused write: {error:?}"
    );
    env.close().await;
}

/// A change whose state record fails reports the failure to its caller and
/// releases the settlement it held without taking up the interrupt that
/// settlement deferred: the interrupt reaches no append, and a live read still
/// answers the goal the branch holds in memory (#2119).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_change_takes_up_no_interrupt_it_deferred() {
    let env = Env::start().await;
    let set = env.set("make the tests pass").await;
    let _end = env.turn("main", &[&set]);
    env.wait_for("main", r#"kind="continuation""#).await;
    let head = env.timeline("main").committed();
    env.dispatched("main", head).await;

    // The pause's writer is held where it settled the decisions, before it
    // composes its record.
    let mut change = dim_goal::testing::hold_stage(
        env.session_id(),
        "main",
        dim_goal::testing::Stage::Change,
        head,
        Some(head),
    );
    let pause = env.goal("pause", json!({}));
    let (pause, taken) = tokio::join!(pause, async {
        assert_eq!(
            change.arrived().await,
            head,
            "the pause's writer is held at the cutoff it read"
        );

        // An operator interrupt commits in that window: task admission is
        // suspended, so its decision waits in the fold, and its own record is
        // held where it would state its pause.
        let interrupt = env.interrupt("main");
        env.dispatched("main", interrupt.sequence).await;
        let mut again = dim_goal::testing::hold_stage(
            env.session_id(),
            "main",
            dim_goal::testing::Stage::Record,
            interrupt.sequence,
            Some(interrupt.sequence),
        );

        // The pause's append fails: its published status no longer matches the
        // state its writer states, which the writer checks under the commit.
        let failure = env.probe.fail_next_state();
        change.release();
        failure.await.expect("the writer refuses the pause");
        // The bound only keeps the case from waiting forever: reaching the hold
        // is what the writer that failed must not do.
        tokio::time::timeout(std::time::Duration::from_millis(750), again.arrived())
            .await
            .ok()
    });
    assert!(
        pause.is_err(),
        "the call whose state record was refused reports the failure: {pause:?}"
    );
    assert_eq!(
        env.probe.failed_states(),
        1,
        "the writer's own check refuses the pause"
    );
    assert!(
        taken.is_none(),
        "a failed change takes up the interrupt it deferred, yet it answered {taken:?}"
    );
    let pauses: Vec<String> = env
        .writes("main")
        .await
        .iter()
        .filter(|block| block.marker.contains(r#"status="paused""#))
        .map(|block| format!("{} at {}", block.marker, block.sequence))
        .collect();
    assert!(
        pauses.is_empty(),
        "a failed change states no pause, yet it wrote {pauses:?}"
    );

    // The failure is the caller's, not the branch's: a live read answers the
    // goal in memory.
    let read = env
        .goal("get", json!({}))
        .await
        .expect("a live read answers the goal in memory");
    assert_eq!(
        read["goal"]["status"]["kind"], "active",
        "a failed change leaves the state the branch holds: {read}"
    );
    env.close().await;
}

/// A state record queued behind a change whose append fails returns before the
/// settlement that change took, so it takes up no interrupt: nothing new moved
/// the fold after the failed write, and the pause the interrupt states waits
/// for a later settlement (#2119).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_queued_record_takes_up_no_interrupt_a_failed_change_deferred() {
    let env = Env::start().await;
    env.set_limit(1);
    let set = env.set("make the tests pass").await;
    env.turn("main", &[&set]);
    let first = env
        .wait_for("main", r#"kind="continuation" count="1""#)
        .await;

    // The second turn end decides the limit pause, and its record waits for the
    // writer before it takes the write guard: the change below writes first and
    // the record queues behind it.
    let mut writer = dim_goal::testing::hold_stage(
        env.session_id(),
        "main",
        dim_goal::testing::Stage::Writer,
        0,
        None,
    );
    let end = env.tool_turn("main", &[&first]);
    let written = tokio::time::timeout(std::time::Duration::from_secs(15), writer.arrived())
        .await
        .expect("the turn end writes its record");
    assert_eq!(
        written, end.sequence,
        "the turn end at {} writes its record",
        end.sequence
    );

    // The pause's writer holds the guard where it settled the decisions, and an
    // operator interrupt commits in that window: it supersedes the pause the
    // turn end decided and waits in the fold, because the settlement holds task
    // admission.
    let mut change = dim_goal::testing::hold_stage(
        env.session_id(),
        "main",
        dim_goal::testing::Stage::Change,
        0,
        None,
    );
    let (pause, taken) = tokio::join!(env.goal("pause", json!({})), async {
        tokio::time::timeout(std::time::Duration::from_secs(15), change.arrived())
            .await
            .expect("the change takes the guard and settles the decisions");
        let interrupt = env.interrupt("main");
        env.dispatched("main", interrupt.sequence).await;
        let mut again = dim_goal::testing::hold_stage(
            env.session_id(),
            "main",
            dim_goal::testing::Stage::Record,
            interrupt.sequence,
            Some(interrupt.sequence),
        );

        // The change's append fails, so it states nothing and leaves the
        // interrupt its settlement deferred with the fold.
        let failure = env.probe.fail_next_state();
        change.release();
        failure.await.expect("the writer refuses the pause");

        // The queued record now takes the guard it waited for. The bound only
        // keeps the case from waiting forever: reaching the hold is what it must
        // not do.
        writer.release();
        tokio::time::timeout(std::time::Duration::from_millis(750), again.arrived())
            .await
            .ok()
    });
    assert!(
        pause.is_err(),
        "the change whose state record was refused reports the failure: {pause:?}"
    );
    assert_eq!(
        env.probe.failed_states(),
        1,
        "the writer's own check refuses the pause"
    );
    assert!(
        taken.is_none(),
        "a queued record takes up the interrupt a failed change deferred, yet it answered {taken:?}"
    );
    let pauses: Vec<String> = env
        .writes("main")
        .await
        .iter()
        .filter(|block| block.marker.contains(r#"status="paused""#))
        .map(|block| format!("{} at {}", block.marker, block.sequence))
        .collect();
    assert!(
        pauses.is_empty(),
        "a failed change and the record queued behind it state no pause, yet they wrote {pauses:?}"
    );
    env.close().await;
}

/// A record whose fact a newer change already answered reports that answer
/// instead of deciding again: the turn end the edit moved states no pause, the
/// continuation count the edit kept still stands, and the next turn end pauses
/// at the limit (#2119).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_turn_end_answered_by_a_newer_change_states_no_pause() {
    let env = Env::start().await;
    env.set_limit(1);
    let set = env.set("make the tests pass").await;
    env.turn("main", &[&set]);
    let first = env
        .wait_for("main", r#"kind="continuation" count="1""#)
        .await;

    // The second turn end decides the limit pause, and its record waits for the
    // writer before it takes the write guard.
    let mut writer = dim_goal::testing::hold_stage(
        env.session_id(),
        "main",
        dim_goal::testing::Stage::Writer,
        0,
        None,
    );
    let end = env.tool_turn("main", &[&first]);
    assert_eq!(
        writer.arrived().await,
        end.sequence,
        "the turn end at {} writes its record",
        end.sequence
    );

    // The edit commits while the record waits: the goal stays active with the
    // new objective, and the count the limit pause read still stands.
    env.goal("edit", json!({"objective": "ship the fix"}))
        .await
        .expect("the edit writes");
    let edit = env.wait_for("main", r#"op="edit""#).await;
    let mut released = dim_goal::testing::hold_stage(
        env.session_id(),
        "main",
        dim_goal::testing::Stage::Released,
        end.sequence,
        Some(end.sequence),
    );
    writer.release();
    assert_eq!(
        released.arrived().await,
        end.sequence,
        "the record at {} answers its caller instead of deciding again",
        end.sequence
    );
    released.release();

    // The edit's state is the newest one, and the answered record wrote no
    // pause over it.
    let writes = env.writes("main").await;
    let states: Vec<String> = writes
        .iter()
        .filter(|block| block.marker.contains(r#"kind="state""#))
        .map(|block| block.text.clone())
        .collect();
    assert!(
        states
            .iter()
            .all(|state| !state.contains(r#"status="paused""#)),
        "the answered record states no pause, yet the states are {states:?}"
    );
    assert_eq!(
        states.last(),
        Some(&texts::edit_state(
            "main",
            "ship the fix",
            &texts::trigger(&edit),
            false
        )),
        "the edit states the newest goal"
    );

    // The count still stands at the limit the edit did not reset: the next turn
    // end pauses there instead of continuing.
    let next = env.tool_turn("main", &[]);
    let pause = env
        .wait_for("main", r#"kind="state" status="paused""#)
        .await;
    assert_eq!(
        pause.text,
        texts::automatic_pause(
            "main",
            "ship the fix",
            "limit",
            "the automatic continuation limit was reached",
            next.sequence
        ),
        "the turn end at {} still pauses at the limit",
        next.sequence
    );
    env.close().await;
}

/// A turn end whose fact a newer change answered answers its live caller
/// instead of deciding again: an input deletion takes the deferred decision
/// once more, and that caller must stop at the answered record rather than
/// loop between a decision that still pauses and a fold that still calls the
/// fact stale (#2119).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_live_turn_end_whose_fact_a_newer_change_answered_decides_once() {
    let env = Env::start().await;
    env.set_limit(1);
    let set = env.set("make the tests pass").await;
    env.turn("main", &[&set]);
    let first = env
        .wait_for("main", r#"kind="continuation" count="1""#)
        .await;

    let mut writer = dim_goal::testing::hold_stage(
        env.session_id(),
        "main",
        dim_goal::testing::Stage::Writer,
        0,
        None,
    );
    let end = env.tool_turn("main", &[&first]);
    assert_eq!(
        writer.arrived().await,
        end.sequence,
        "the turn end at {} writes its record",
        end.sequence
    );

    env.goal("edit", json!({"objective": "ship the fix"}))
        .await
        .expect("the edit writes");
    env.wait_for("main", r#"op="edit""#).await;

    // Deleting an ordinary input takes the deferred turn-end decision again,
    // so the answered fact reaches a caller that is still alive.
    let note = env.emit(
        "main",
        message(vec![raw("a note", None, false)], InputMode::Append),
    );
    let mut second = dim_goal::testing::hold_stage(
        env.session_id(),
        "main",
        dim_goal::testing::Stage::Writer,
        0,
        Some(end.sequence),
    );
    let mut third = dim_goal::testing::hold_stage(
        env.session_id(),
        "main",
        dim_goal::testing::Stage::Writer,
        0,
        Some(end.sequence),
    );
    let mut done = dim_goal::testing::hold_stage(
        env.session_id(),
        "main",
        dim_goal::testing::Stage::Done,
        0,
        Some(end.sequence),
    );
    env.delete_id("main", &note.id);
    assert_eq!(
        second.arrived().await,
        end.sequence,
        "the deleted input states the turn end's record once more"
    );
    writer.release();
    second.release();

    tokio::select! {
        arrived = done.arrived() => assert_eq!(
            arrived, end.sequence,
            "the answered record answers its caller at {}",
            end.sequence
        ),
        arrived = third.arrived() => panic!(
            "the answered record decided again at {arrived}: the live caller loops between the pause and the stale fact"
        ),
    }
    done.release();
    env.close().await;
}

/// An ordinary Append that commits between the turn-end decision and the
/// record's own cutoff leaves the pause unwritten, and the turn end the caller
/// still owns states its continuation (#2119).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_appended_note_before_the_pause_record_returns_the_turn_end_to_its_continuation() {
    let env = Env::start().await;
    env.set_limit(1);
    let set = env.set("make the tests pass").await;
    env.turn("main", &[&set]);
    let first = env
        .wait_for("main", r#"kind="continuation" count="1""#)
        .await;

    // The second turn end decides the limit pause, and its record waits for the
    // writer before it takes the write guard.
    let mut writer = dim_goal::testing::hold_stage(
        env.session_id(),
        "main",
        dim_goal::testing::Stage::Writer,
        0,
        None,
    );
    let end = env.tool_turn("main", &[&first]);
    let written = tokio::time::timeout(std::time::Duration::from_secs(15), writer.arrived())
        .await
        .expect("the turn end writes its record");
    assert_eq!(
        written, end.sequence,
        "the turn end at {} writes its record",
        end.sequence
    );

    // An ordinary Append commits before the record takes the guard: the fold
    // resets the continuation count without a driving input, so no limit pause
    // is due.
    let note = env.emit("main", user("a note", InputMode::Append));
    env.dispatched("main", note.sequence).await;
    let mut pause = dim_goal::testing::hold_stage(
        env.session_id(),
        "main",
        dim_goal::testing::Stage::Append,
        end.sequence,
        Some(end.sequence),
    );
    writer.release();

    let recorded = tokio::time::timeout(std::time::Duration::from_millis(750), pause.arrived())
        .await
        .is_ok();
    assert!(
        !recorded,
        "the Append at {} takes the limit pause back, but the record at {} reached its append",
        note.sequence, end.sequence
    );
    let continuation = env
        .wait_for(
            "main",
            &format!(
                r#"kind="continuation" count="1" limit="1" after="{}""#,
                end.sequence
            ),
        )
        .await;
    assert_eq!(
        continuation.text,
        texts::continuation("main", 1, Some(1), 0, end.sequence),
        "the turn end at {} continues the goal the Append left running",
        end.sequence
    );
    env.close().await;
}

/// The pause a turn end decides is stated with the reason the fold gives at
/// the record's own cutoff: an ordinary Append resets the continuation count
/// without opening a turn, so the limit pause is no longer the answer (#2119).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_appended_note_returns_the_turn_end_to_its_continuation() {
    let env = Env::start_gated().await;
    env.set_limit(1);
    let set = env.set("make the tests pass").await;
    let _set_end = env.turn("main", &[&set]);
    let first = env
        .wait_for("main", r#"kind="continuation" count="1""#)
        .await;

    // The second turn end decides the limit pause, and its record writer is
    // held where it settled the decisions, before it composes the record.
    let mut record = dim_goal::testing::hold_stage(
        env.session_id(),
        "main",
        dim_goal::testing::Stage::Record,
        first.sequence,
        None,
    );
    let end = env.tool_turn("main", &[&first]);
    assert_eq!(
        record.arrived().await,
        end.sequence,
        "the pause's record writer answers the turn end at {}",
        end.sequence
    );

    // An ordinary Append commits in the settlement window: the fold resets the
    // continuation count without a driving input, so no limit pause is due.
    let note = env.emit("main", user("a note", InputMode::Append));
    env.dispatched("main", note.sequence).await;
    let mut pause = dim_goal::testing::hold_stage(
        env.session_id(),
        "main",
        dim_goal::testing::Stage::Append,
        end.sequence,
        Some(end.sequence),
    );
    let mut retaken = dim_goal::testing::hold_stage(
        env.session_id(),
        "main",
        dim_goal::testing::Stage::Decision,
        end.sequence,
        Some(end.sequence),
    );
    record.release();

    let recorded = tokio::select! {
        _ = pause.arrived() => true,
        _ = retaken.arrived() => false,
    };
    assert!(
        !recorded,
        "the Append at {} takes the limit pause back, but the record at {} reached its append",
        note.sequence, end.sequence
    );
    retaken.release();
    let continuation = env
        .wait_for(
            "main",
            &format!(
                r#"kind="continuation" count="1" limit="1" after="{}""#,
                end.sequence
            ),
        )
        .await;
    assert_eq!(
        continuation.text,
        texts::continuation("main", 1, Some(1), 0, end.sequence),
        "the turn end at {} continues the goal the Append left running",
        end.sequence
    );
    env.close().await;
}

/// The closing pause is stated at the fold, not at the snapshot the close
/// started from: a change that committed before it and is still waiting for the
/// fold wins, and no record is written while the fold is behind (#2119).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_closing_pause_writes_nothing_while_a_committed_change_is_unfolded() {
    let env = Env::start_gated().await;
    env.set("ship it").await;

    // The clear commits, and its record's publication is held: the fold has
    // not seen the branch lose its goal when the close starts.
    let (written, release) = env.gated().hold_state(None);
    env.goal("clear", json!({})).await.expect("clear writes");
    tokio::time::timeout(std::time::Duration::from_secs(15), written)
        .await
        .expect("the clear's record commits")
        .expect("the held record is the clear's");

    // The publication is released once the close has had its chance to state
    // the pause: whichever way its catch-up ends, the clear is what stands.
    let (env, ()) = tokio::join!(env.restore(), async {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        release.send(()).expect("the record publishes");
    });
    let read = env.goal("get", json!({})).await.unwrap();
    assert!(
        read["goal"].is_null(),
        "the clear wins over the closing pause: {read}"
    );
    let pauses: Vec<String> = env
        .writes("main")
        .await
        .iter()
        .filter(|block| block.marker.contains(r#"reason="interrupted_unload""#))
        .map(|block| format!("{} at {}", block.marker, block.sequence))
        .collect();
    assert!(
        pauses.is_empty(),
        "the close states no pause the clear already replaced, yet it wrote {pauses:?}"
    );
    env.close().await;
}

/// The closing pause states the fold its snapshot was taken from and commits
/// only while that fold is still the branch's head: a fact that commits after
/// the close caught up leaves no pause labeled with the newer sequence (#2119).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_change_committed_after_the_close_caught_up_leaves_no_pause() {
    let env = Env::start_gated().await;
    env.set("ship it").await;
    let head = env.timeline("main").committed();
    env.dispatched("main", head).await;

    // The close catches up, reads the fold it states and is held before its
    // append.
    let timeline = env.timeline("main");
    let mut closing = dim_goal::testing::hold_stage(
        env.session_id(),
        "main",
        dim_goal::testing::Stage::Closing,
        head,
        None,
    );
    let change = texts::pause("main");
    let (env, ()) = tokio::join!(env.restore(), async {
        closing.arrived().await;
        timeline
            .append(message(vec![raw(&change, None, false)], InputMode::Append).into())
            .expect("the change commits inside the close");
        closing.release();
    });
    let pauses: Vec<String> = env
        .writes("main")
        .await
        .iter()
        .filter(|block| block.marker.contains(r#"reason="interrupted_unload""#))
        .map(|block| format!("{} at {}", block.marker, block.sequence))
        .collect();
    assert!(
        pauses.is_empty(),
        "the close states no pause at a head its snapshot does not hold, yet it wrote {pauses:?}"
    );
    env.close().await;
}

/// A creation that folds after the evaluation it belongs to was answered is
/// ended by the settlement that holds task admission, with no later input to
/// wake it (#2119, Pursuit §13 E2).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_creation_that_folds_while_the_settlement_holds_admission_is_ended() {
    let env = Env::start().await;
    let set = env.set("make the tests pass").await;
    let end = env.turn("main", &[&set]);
    let evaluation = end.sequence;
    let first = env
        .open_branch(
            "main",
            "check the result",
            BranchSource::Host {
                operation_id: format!("goal:evaluate:{evaluation}"),
            },
        )
        .await;
    env.dispatched("main", env.timeline("main").committed())
        .await;

    // The conclusion's record writer is held where it settled the decisions,
    // before it composes its record.
    let mut record = dim_goal::testing::hold_stage(
        env.session_id(),
        "main",
        dim_goal::testing::Stage::Record,
        evaluation,
        None,
    );
    let conclusion = texts::conclusion("main", &first, evaluation, "done", "DONE: verified");
    let conclusion = env
        .write_message(
            "main",
            vec![raw(&conclusion, None, false)],
            InputMode::Append,
        )
        .sequence;
    assert_eq!(
        record.arrived().await,
        conclusion,
        "the conclusion's record writer answers the conclusion at {conclusion}"
    );

    // A creation for the evaluation the conclusion already answered commits its
    // `branch/create` while admission is suspended: nothing evaluates it any
    // more, and its cleanup is owed.
    let evaluator = env
        .open_branch(
            "main",
            "check the result",
            BranchSource::Host {
                operation_id: format!("goal:evaluate:{evaluation}"),
            },
        )
        .await;
    env.dispatched("main", env.timeline("main").committed())
        .await;

    // The record's own fold is held too, so no later fact takes the cleanup up:
    // only the settlement that suspended admission owes it.
    let written = dim_goal::testing::hold_input(env.session_id(), "main", conclusion);
    let mut ended = dim_agent_capabilities::timeline_tree::testing::hold_end_record(
        env.session_id(),
        &evaluator,
    );
    record.release();
    // Reaching the held end is the assertion: the case has no earlier fact to
    // wait for, so a regression hangs here instead of failing one.
    ended.arrived().await;
    ended.release();
    written.release();
    env.dispatched("main", env.timeline("main").committed())
        .await;
    env.close().await;
}

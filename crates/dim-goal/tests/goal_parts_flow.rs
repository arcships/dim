//! Goal's cases over the no-Loop part harness: the real Dispatcher,
//! Timeline, TimelineTree, SessionMethods, ToolRegistry, PromptGuide,
//! storage and Settings, with the facts the Loop would write supplied by the
//! test. Every expectation is a literal text or a written input.
mod support;

use dim_agent_types::{EventKind, InputMode, ToolContent, ToolResult};
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
async fn a_completed_turn_drives_the_continuation_and_the_limit_pauses_it() {
    let env = Env::start().await;
    env.set_limit(1);
    let set = env.set("ship it").await;
    let end = env.turn("main", &[&set]);
    let continuation = env.wait_for("main", r#"kind="continuation""#).await;
    assert_eq!(
        continuation.text,
        texts::continuation("main", 1, Some(1), 0, end.sequence)
    );
    let end = env.turn("main", &[&continuation]);
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
    assert_eq!(env.settled("main").await.len(), 4);
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

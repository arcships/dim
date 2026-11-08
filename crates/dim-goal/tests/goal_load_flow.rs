//! The `goal.test.mjs` load cases: a Session that already carries history is
//! restored, and Goal rebuilds its fold and takes the decisions the previous
//! generation owed before any live fact.
mod support;

use std::sync::Arc;
use std::time::Duration;

use dim_agent_capabilities::history::{HistoryRenderer, context_messages, summary_message};
use dim_agent_runtime::{BranchOptions, TimelineQueryDirection};
use dim_agent_types::{
    BranchCreateAt, BranchCreateData, BranchSource, ContextCompactionData, ContextCompactionResult,
    InputMode,
};
use support::parts::*;
use support::texts;
use tokio_util::sync::CancellationToken;

/// One seeded evaluation: the set, the worker's completed request and the
/// evaluator's creation, as the main branch wrote them.
fn seed_evaluation(seed: &mut Seed, evaluator: &str) -> (u64, u64) {
    let (objective, status) = texts::legacy_evaluator_set_blocks("main", "ship it");
    let set = seed.append(
        "main",
        message(
            vec![
                raw(&objective, Some("goal:objective"), false),
                raw(&status, Some("goal:status"), false),
            ],
            InputMode::Queue,
        ),
    );
    seed.append(
        "main",
        dim_agent_types::EventKind::TurnStart(dim_agent_types::TurnStartData {
            turn_id: "work".into(),
        }),
    );
    seed.append(
        "main",
        dim_agent_types::EventKind::InputConsumed(dim_agent_types::InputConsumedData {
            input_id: set.id.clone(),
        }),
    );
    seed.append(
        "main",
        message(
            vec![raw(
                &texts::legacy_report("main", "completed", "all tests pass"),
                Some("goal:status"),
                false,
            )],
            InputMode::Append,
        ),
    );
    let end = seed.append(
        "main",
        dim_agent_types::EventKind::TurnEnd(dim_agent_types::TurnEndData {
            turn_id: "work".into(),
            outcome: dim_agent_types::TurnOutcome::Completed {
                reason: "stop".into(),
            },
        }),
    );
    let evaluation = end.sequence;
    let create = seed.append(
        "main",
        dim_agent_types::EventKind::BranchCreate(BranchCreateData {
            at: BranchCreateAt::Sequence {
                sequence: evaluation,
            },
            source: BranchSource::Host {
                operation_id: format!("goal:evaluate:{evaluation}"),
            },
            branch: evaluator.into(),
            prompt: texts::evaluator_prompt("main", evaluation, "ship it", "all tests pass"),
        }),
    );
    seed.append(
        evaluator,
        dim_agent_types::EventKind::BranchStart(dim_agent_types::BranchStartData {
            parent: Some("main".into()),
            create_event: Some(create.sequence),
        }),
    );
    // The prompt input the tree writes on a new line: a restore tells a line
    // whose creation completed by its opening and this first Queue input.
    seed.append(
        evaluator,
        message(vec![raw("line work", None, false)], InputMode::Queue),
    );
    (evaluation, create.sequence)
}

fn pause(seed: &mut Seed) {
    seed.append(
        "main",
        message(
            vec![raw(
                &texts::legacy_pause("main"),
                Some("goal:status"),
                false,
            )],
            InputMode::Append,
        ),
    );
}

/// Polls until `branch` has an end fact from `source`.
async fn wait_ended(env: &Env, branch: &str, source: BranchSource) {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if env
                .ended(branch)
                .await
                .iter()
                .any(|end| end.source == source)
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the line ends");
}

#[tokio::test]
async fn e2_a_load_ends_a_settled_evaluator_once_unless_its_end_is_recorded() {
    // The previous generation superseded the evaluation but never recorded
    // the end: the rebuild owes it.
    let env = Env::start().await;
    let mut seed = env.seed_new("load-settled").await;
    let (evaluation, _) = seed_evaluation(&mut seed, "eval");
    pause(&mut seed);
    seed.finish().await;
    let env = env.restore_seeded("load-settled").await;
    wait_ended(
        &env,
        "main",
        BranchSource::Host {
            operation_id: format!("goal:evaluate:{evaluation}"),
        },
    )
    .await;
    assert_eq!(env.ended("main").await.len(), 1, "the end is written once");
    assert_eq!(
        env.settled("main").await.len(),
        3,
        "the load writes no pause or continuation"
    );
    env.close().await;

    // The end already in history is not written twice.
    let env = Env::start().await;
    let mut seed = env.seed_new("load-recorded").await;
    let (evaluation, _) = seed_evaluation(&mut seed, "eval");
    pause(&mut seed);
    seed.append(
        "main",
        dim_agent_types::EventKind::BranchEnded(dim_agent_types::BranchEndedData {
            branch: "eval".into(),
            end_event: 0,
            source: BranchSource::Host {
                operation_id: format!("goal:evaluate:{evaluation}"),
            },
            ancestor: None,
        }),
    );
    seed.finish().await;
    let env = env.restore_seeded("load-recorded").await;
    assert_eq!(
        env.settled("main").await.len(),
        3,
        "an end already in history is not written twice"
    );
    assert_eq!(env.ended("main").await.len(), 1);
    env.close().await;
}

#[tokio::test]
async fn e2_an_evaluation_whose_creation_failed_has_no_evaluator_to_end() {
    let env = Env::start().await;
    let mut seed = env.seed_new("load-failed").await;
    let (evaluation, create_sequence) = seed_evaluation(&mut seed, "eval");
    let create = seed.append(
        "main",
        dim_agent_types::EventKind::BranchCreateFailed(
            dim_agent_types::BranchCreateFailedData::Failed {
                branch: "eval".into(),
                create_event: create_sequence,
                error: "branch creation failed".into(),
            },
        ),
    );
    let _ = create;
    pause(&mut seed);
    seed.finish().await;
    let env = env.restore_seeded("load-failed").await;
    assert!(
        env.ended("main").await.is_empty(),
        "a failed creation leaves no line to end"
    );
    assert_eq!(
        env.settled("main").await.len(),
        3,
        "the load writes no evaluation_failed pause"
    );
    let _ = evaluation;
    env.close().await;
}

#[tokio::test]
async fn e2_a_restored_evaluator_already_asked_writes_no_further_follow_up() {
    let env = Env::start().await;
    let mut seed = env.seed_new("load-asked").await;
    let (evaluation, _) = seed_evaluation(&mut seed, "eval");
    for turn in ["t1", "t2"] {
        seed.append(
            "eval",
            dim_agent_types::EventKind::TurnStart(dim_agent_types::TurnStartData {
                turn_id: turn.into(),
            }),
        );
        seed.append(
            "eval",
            dim_agent_types::EventKind::ModelStart(dim_agent_types::StepData {
                turn_id: turn.into(),
                request_id: format!("{turn}-r"),
            }),
        );
        seed.append(
            "eval",
            dim_agent_types::EventKind::Text(dim_agent_types::DeltaData {
                turn_id: turn.into(),
                request_id: format!("{turn}-r"),
                delta: "still checking".into(),
            }),
        );
        seed.append(
            "eval",
            dim_agent_types::EventKind::TurnEnd(dim_agent_types::TurnEndData {
                turn_id: turn.into(),
                outcome: dim_agent_types::TurnOutcome::Completed {
                    reason: "stop".into(),
                },
            }),
        );
    }
    seed.finish().await;

    let env = env.restore_seeded("load-asked").await;
    assert_eq!(
        env.writes("eval").await.len(),
        1,
        "the restored evaluator carries only its inherited clear"
    );
    let pending = env.pending_inputs("eval").await;
    let pending: Vec<&str> = pending.iter().map(String::as_str).collect();
    env.turn_steps(
        "eval",
        "t3",
        &pending,
        "completed",
        &[("r3", &["done, I think"])],
    );
    let needle = format!(r#"count="1" after="{evaluation}" evaluation="{evaluation}""#);
    let continuation = env.wait_for("main", &needle).await;
    assert_eq!(
        continuation.text,
        texts::evaluated_continuation("main", 1, None, 0, evaluation, evaluation)
    );
    assert_eq!(
        env.writes("eval").await.len(),
        1,
        "no further follow-up after a reload"
    );
    env.close().await;
}

#[tokio::test]
async fn pursuit_6_a_turn_end_already_in_the_restored_evaluator_history_reports_nothing() {
    let env = Env::start().await;
    let mut seed = env.seed_new("load-turn").await;
    let (_, _) = seed_evaluation(&mut seed, "eval");
    seed.append(
        "eval",
        dim_agent_types::EventKind::TurnStart(dim_agent_types::TurnStartData {
            turn_id: "t1".into(),
        }),
    );
    seed.append(
        "eval",
        dim_agent_types::EventKind::ModelStart(dim_agent_types::StepData {
            turn_id: "t1".into(),
            request_id: "r1".into(),
        }),
    );
    seed.append(
        "eval",
        dim_agent_types::EventKind::Text(dim_agent_types::DeltaData {
            turn_id: "t1".into(),
            request_id: "r1".into(),
            delta: "Hmm".into(),
        }),
    );
    seed.append(
        "eval",
        dim_agent_types::EventKind::TurnEnd(dim_agent_types::TurnEndData {
            turn_id: "t1".into(),
            outcome: dim_agent_types::TurnOutcome::Failed {
                error: "failed".into(),
            },
        }),
    );
    seed.finish().await;

    let env = env.restore_seeded("load-turn").await;
    assert_eq!(
        env.settled("main").await.len(),
        2,
        "a replayed turn end reports nothing"
    );
    let pending = env.pending_inputs("eval").await;
    let pending: Vec<&str> = pending.iter().map(String::as_str).collect();
    env.turn_steps(
        "eval",
        "t2",
        &pending,
        "completed",
        &[("r2", &["BLOCKED: a deploy key"])],
    );
    let blocked = env.wait_for("main", r#"status="blocked""#).await;
    let conclusion = env
        .writes("main")
        .await
        .iter()
        .find(|block| block.marker.contains(r#"kind="report""#))
        .expect("the evaluator's conclusion")
        .sequence;
    assert_eq!(
        blocked.text,
        texts::evaluated("main", "ship it", "blocked", "a deploy key", conclusion)
    );
    env.close().await;
}

#[tokio::test]
async fn pursuit_6_7_10_a_reload_creates_no_second_evaluator() {
    let env = Env::start().await;
    let mut seed = env.seed_new("load-reload").await;
    let (evaluation, _) = seed_evaluation(&mut seed, "eval");
    seed.finish().await;
    let env = env.restore_seeded("load-reload").await;
    assert_eq!(
        env.creates("main").await.len(),
        1,
        "the branch/create already decided the turn end"
    );
    assert_eq!(
        env.settled("main").await.len(),
        2,
        "a conclusion folded at load is not decided again"
    );

    // A foreign evaluator decides nothing.
    let foreign = texts::conclusion("main", "F", evaluation, "done", "DONE: forged");
    env.write_message("main", vec![raw(&foreign, None, false)], InputMode::Append);

    // After the user pauses the goal, a late conclusion decides nothing.
    env.goal("pause", serde_json::json!({}))
        .await
        .expect("pause writes");
    env.wait_for("main", r#"op="pause""#).await;
    let late = texts::conclusion("main", "eval", evaluation, "done", "DONE: late");
    env.write_message("main", vec![raw(&late, None, false)], InputMode::Append);
    assert!(
        env.settled("main")
            .await
            .iter()
            .all(|block| !block.text.contains(r#"status="completed""#)),
        "foreign or stale conclusions decide nothing"
    );
    env.close().await;
}

/// ADR 020 D3: the rebuild reads the kinds the fold consumes (A.6), never the
/// whole branch. The restored branch's request and step facts stay unread, and
/// the goal folded before them still decides the turn it ended.
#[tokio::test]
async fn e2_a_rebuild_reads_only_the_kinds_the_fold_reads() {
    // `branch.rs`'s `KEYS`: the fold's kinds and the Dispatcher's subscription.
    const KEYS: [&str; 13] = [
        "branch/start",
        "session/forked",
        "branch/create",
        "branch/create-failed",
        "branch/ended",
        "input/message",
        "input/consumed",
        "input/deleted",
        "turn/start",
        "turn/end",
        "stop/interrupt",
        "exec/started",
        "exec/ended",
    ];
    let env = Env::start_refusing().await;
    let mut seed = env.seed_new("load-kinds").await;
    let (objective, status) = texts::legacy_set_blocks("main", "ship it");
    let set = seed.append(
        "main",
        message(
            vec![
                raw(&objective, Some("goal:objective"), false),
                raw(&status, Some("goal:status"), false),
            ],
            InputMode::Queue,
        ),
    );
    seed.append(
        "main",
        dim_agent_types::EventKind::TurnStart(dim_agent_types::TurnStartData {
            turn_id: "work".into(),
        }),
    );
    seed.append(
        "main",
        dim_agent_types::EventKind::InputConsumed(dim_agent_types::InputConsumedData {
            input_id: set.id.clone(),
        }),
    );
    // Facts the fold never reads: the request body and the step rows a turn
    // leaves between its start and its end.
    seed.append(
        "main",
        dim_agent_types::EventKind::ModelCall(dim_agent_types::ModelCallData {
            turn_id: "work".into(),
            request_id: "r1".into(),
            request: Arc::new(dim_agent_types::ModelRequest::new(
                "test",
                "test-model",
                Vec::new(),
            )),
            configuration: None,
            prompt: None,
            failure_chain: None,
        }),
    );
    seed.append(
        "main",
        dim_agent_types::EventKind::Text(dim_agent_types::DeltaData {
            turn_id: "work".into(),
            request_id: "r1".into(),
            delta: "checking".into(),
        }),
    );
    seed.append(
        "main",
        dim_agent_types::EventKind::ToolResult(dim_agent_types::ToolResultData {
            turn_id: "work".into(),
            request_id: "r1".into(),
            call_event_id: None,
            results: Vec::new(),
        }),
    );
    let end = seed.append(
        "main",
        dim_agent_types::EventKind::TurnEnd(dim_agent_types::TurnEndData {
            turn_id: "work".into(),
            outcome: dim_agent_types::TurnOutcome::Completed {
                reason: "stop".into(),
            },
        }),
    );
    seed.finish().await;

    let env = env.restore_seeded("load-kinds").await;
    // The continuation the goal owes the ended turn proves the rebuild folded
    // the goal block and its status, which sit before the noise.
    let continuation = env.wait_for("main", r#"kind="continuation""#).await;
    assert_eq!(
        continuation.text,
        texts::continuation("main", 1, None, 0, end.sequence),
        "the fold is the one the kinds behind the noise leave"
    );
    let events = env.all_events("main").await;
    let kinds: Vec<&str> = events.iter().map(|event| event.kind.key()).collect();
    for kind in ["model/call", "step/text", "step/tool-result"] {
        assert!(kinds.contains(&kind), "the history holds {kind}: {kinds:?}");
    }

    let expected: Vec<String> = KEYS.iter().map(|kind| (*kind).to_owned()).collect();
    let rebuilds: Vec<Read> = env
        .reads()
        .into_iter()
        .filter(|read| read.include_publishing && read.kinds == expected)
        .collect();
    assert!(
        !rebuilds.is_empty(),
        "the restore ran the full fold and the store saw its read"
    );
    for read in rebuilds {
        assert_eq!(
            read.branch, "main",
            "the rebuild reads the branch it folds: {read:?}"
        );
    }
    // The cheap path reads the inputs and the opening in one scan (D4).
    let scan: Vec<String> = [
        "input/message",
        "input/deleted",
        "branch/start",
        "session/forked",
    ]
    .iter()
    .map(|kind| (*kind).to_owned())
    .collect();
    let probes: Vec<Read> = env
        .reads()
        .into_iter()
        .filter(|read| read.include_publishing && read.kinds == scan)
        .collect();
    assert!(
        !probes.is_empty(),
        "the rebuild probes for a goal before folding: {:?}",
        env.reads()
    );
    env.close().await;
}

/// The inherited goal check reads one ancestor at a time, each page bounded
/// below the one before it, so its reads stay proportional to the prefix
/// rather than to its square, and none of them is keyed on the branch being
/// opened (A11).
#[tokio::test]
async fn e2_an_inherited_check_reads_its_ancestor_page_by_page() {
    let env = Env::start_refusing().await;
    let mut seed = env.seed_new("load-inherited").await;
    // More than three pages of inputs and no goal block anywhere: the check
    // walks the whole prefix and finds nothing.
    let mut inputs = Vec::new();
    for index in 0..200 {
        inputs.push(seed.append("main", user(&format!("note {index}"), InputMode::Queue)));
    }
    let cut = inputs.last().unwrap().sequence;
    let create = seed.append(
        "main",
        dim_agent_types::EventKind::BranchCreate(BranchCreateData {
            at: BranchCreateAt::Sequence { sequence: cut },
            source: BranchSource::Host {
                operation_id: "test:child".into(),
            },
            branch: "child".into(),
            prompt: "child work".into(),
        }),
    );
    seed.append(
        "child",
        dim_agent_types::EventKind::BranchStart(dim_agent_types::BranchStartData {
            parent: Some("main".into()),
            create_event: Some(create.sequence),
        }),
    );
    // The first Queue input a completed creation leaves on its line.
    seed.append(
        "child",
        message(vec![raw("line work", None, false)], InputMode::Queue),
    );
    seed.finish().await;

    let env = env.restore_seeded("load-inherited").await;
    // 200 inputs read 64 to a page, newest first from the cut: 64, 64, 64 and
    // a short page of 8.
    let pages = 200_usize.div_ceil(64);
    let scan_read = |read: &Read| {
        read.kinds == ["input/message".to_owned()]
            && read.direction == TimelineQueryDirection::Descending
    };
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if env.reads().iter().filter(|read| scan_read(read)).count() >= pages {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the inherited check finishes");
    let writes = env.settled("child").await;
    assert!(
        writes.is_empty(),
        "nothing is inherited, so the check writes nothing: {writes:?}"
    );

    let scan: Vec<Read> = env.reads().into_iter().filter(scan_read).collect();
    assert_eq!(
        scan.len(),
        pages,
        "one page per 64 facts, not one read per row: {scan:?}"
    );
    for read in &scan {
        assert_eq!(
            read.branch, "main",
            "the check reads the ancestor, never the branch it opens: {read:?}"
        );
        assert_eq!(
            read.offset, 0,
            "a page starts at its own bound, not at an offset: {read:?}"
        );
        assert_eq!(read.limit, 64, "a page keeps 64 facts: {read:?}");
        assert_eq!(
            read.from, 1,
            "a descending page is bounded by its through: {read:?}"
        );
        assert!(
            !read.include_publishing,
            "the check reads only what the ancestor published: {read:?}"
        );
    }
    // Each page starts below the oldest fact of the one before it: the cut,
    // then the fact before the 64th newest, the 128th, and the 192nd.
    let bounds: Vec<u64> = scan.iter().map(|read| read.through).collect();
    let expected: Vec<u64> = std::iter::once(cut)
        .chain((1..pages).map(|page| inputs[199 - 64 * page].sequence))
        .collect();
    assert_eq!(
        bounds, expected,
        "the pages walk the prefix downward without skipping: {scan:?}"
    );
    env.close().await;
}

/// D3: a change trigger the previous generation never answered is answered
/// once at load, naming the trigger; the next load writes nothing more.
#[tokio::test]
async fn e2_a_load_answers_an_unanswered_change_once() {
    let env = Env::start().await;
    let mut seed = env.seed_new("load-unanswered").await;
    seed.append(
        "main",
        dim_agent_types::EventKind::BranchStart(dim_agent_types::BranchStartData {
            parent: None,
            create_event: None,
        }),
    );
    let set = seed.append(
        "main",
        message(
            vec![raw(&texts::set("main", "ship it"), None, false)],
            InputMode::Queue,
        ),
    );
    seed.append(
        "main",
        message(
            vec![raw(
                &texts::set_state(
                    "main",
                    "ship it",
                    &texts::Answer::After(set.sequence),
                    false,
                ),
                Some("goal:state"),
                false,
            )],
            InputMode::Append,
        ),
    );
    let pause = seed.append(
        "main",
        message(
            vec![raw(&texts::pause("main"), None, false)],
            InputMode::Append,
        ),
    );
    seed.finish().await;

    let env = env.restore_seeded("load-unanswered").await;
    let record = env
        .wait_for(
            "main",
            &format!(
                r#"kind="state" status="paused" reason="user" after="{}""#,
                pause.sequence
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
            &texts::Answer::After(pause.sequence),
            false
        )
    );
    let writes = env.settled("main").await;
    assert_eq!(
        writes
            .iter()
            .filter(|block| block.marker.contains(r#"kind="state""#))
            .count(),
        2,
        "the answered change and the one answered at load: {writes:#?}"
    );

    env.close().await;

    // A history whose newest record already answers the pause answers nothing
    // when it loads.
    let env = Env::start().await;
    let mut seed = env.seed_new("load-answered").await;
    seed.append(
        "main",
        dim_agent_types::EventKind::BranchStart(dim_agent_types::BranchStartData {
            parent: None,
            create_event: None,
        }),
    );
    let set = seed.append(
        "main",
        message(
            vec![raw(&texts::set("main", "ship it"), None, false)],
            InputMode::Queue,
        ),
    );
    seed.append(
        "main",
        message(
            vec![raw(
                &texts::set_state(
                    "main",
                    "ship it",
                    &texts::Answer::After(set.sequence),
                    false,
                ),
                Some("goal:state"),
                false,
            )],
            InputMode::Append,
        ),
    );
    let pause = seed.append(
        "main",
        message(
            vec![raw(&texts::pause("main"), None, false)],
            InputMode::Append,
        ),
    );
    seed.append(
        "main",
        message(
            vec![raw(
                &texts::paused(
                    "main",
                    "ship it",
                    "user",
                    "",
                    &texts::Answer::After(pause.sequence),
                    false,
                ),
                Some("goal:state"),
                false,
            )],
            InputMode::Append,
        ),
    );
    seed.finish().await;
    let env = env.restore_seeded("load-answered").await;
    let writes = env.settled("main").await;
    assert_eq!(
        writes
            .iter()
            .filter(|block| block.marker.contains(r#"kind="state""#))
            .count(),
        2,
        "a load repeats no answer: {writes:#?}"
    );
    env.close().await;
}

/// D5: an old two-key history reads by the old rules, is not backfilled at
/// load, and its first answered change writes the state record.
#[tokio::test]
async fn e2_an_old_history_answers_its_first_change_with_a_state_record() {
    let env = Env::start().await;
    let mut seed = env.seed_new("load-old").await;
    let (objective, status) = texts::legacy_set_blocks("main", "ship it");
    let set = seed.append(
        "main",
        message(
            vec![
                raw(&objective, Some("goal:objective"), false),
                raw(&status, Some("goal:status"), false),
            ],
            InputMode::Queue,
        ),
    );
    seed.append(
        "main",
        dim_agent_types::EventKind::InputConsumed(dim_agent_types::InputConsumedData {
            input_id: set.id.clone(),
        }),
    );
    seed.finish().await;

    let env = env.restore_seeded("load-old").await;
    let before = env.settled("main").await;
    assert_eq!(before.len(), 1, "no backfill at load: {before:#?}");
    // The old keys still read: a turn end continues the goal.
    let end = env.turn_ids("main", &[]);
    let continuation = env.wait_for("main", r#"kind="continuation""#).await;
    assert_eq!(
        continuation.text,
        texts::continuation("main", 1, None, 0, end.sequence)
    );

    // The first change answered after the upgrade writes the record.
    env.goal("pause", serde_json::json!({}))
        .await
        .expect("pause writes");
    let pause = env.wait_for("main", r#"op="pause" reason="user""#).await;
    let record = env
        .wait_for(
            "main",
            &format!(
                r#"kind="state" status="paused" reason="user" trigger="{}""#,
                pause.submission_id.as_deref().unwrap()
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
            &texts::trigger(&pause),
            false
        )
    );
    let input = env
        .written_input("main", r#"kind="state" status="paused""#)
        .await;
    let dim_agent_types::InputBlock::Raw { key, .. } = &input.blocks[0];
    assert_eq!(key.as_deref(), Some("goal:state"));
    env.close().await;
}

/// R1: the first state record written on an old history retires the old keys
/// in its own input, so the keyed baseline — what `session.keys` lists and a
/// compaction copies — holds only the record, and the next request after a
/// compaction carries the paused state and no old block.
#[tokio::test]
async fn e2_the_first_state_record_retires_the_old_keys() {
    let env = Env::start().await;
    let mut seed = env.seed_new("load-retire").await;
    let (objective, status) = texts::legacy_set_blocks("main", "ship it");
    let set = seed.append(
        "main",
        message(
            vec![
                raw(&objective, Some("goal:objective"), false),
                raw(&status, Some("goal:status"), false),
            ],
            InputMode::Queue,
        ),
    );
    seed.append(
        "main",
        dim_agent_types::EventKind::InputConsumed(dim_agent_types::InputConsumedData {
            input_id: set.id.clone(),
        }),
    );
    seed.finish().await;
    let env = env.restore_seeded("load-retire").await;

    // The user pauses; the record that answers states the branch, and the old
    // keys still live ride in the same input as retires.
    env.goal("pause", serde_json::json!({}))
        .await
        .expect("pause writes");
    let trigger = env.wait_for("main", r#"op="pause""#).await;
    let record = env
        .wait_for(
            "main",
            &format!(
                r#"kind="state" status="paused" reason="user" trigger="{}""#,
                trigger.submission_id.as_deref().unwrap()
            ),
        )
        .await;
    let input = env
        .written_input("main", r#"kind="state" status="paused""#)
        .await;
    let keys: Vec<_> = input
        .blocks
        .iter()
        .map(|block| {
            let dim_agent_types::InputBlock::Raw { key, retire, .. } = block;
            (key.as_deref(), *retire)
        })
        .collect();
    assert_eq!(
        keys,
        [
            (Some("goal:state"), false),
            (Some("goal:objective"), true),
            (Some("goal:status"), true),
        ],
        "the record retires the old keys in its own input"
    );

    // A turn consumes the change and the record: the delivered keyed state
    // from here on is what `session.keys` lists and a compaction copies.
    let end = env.turn("main", &[&trigger, &record]);
    let renderer = HistoryRenderer::default();
    let timeline = env.timeline("main");
    let (_, history) = renderer
        .rendered_context(&timeline, timeline.sequence())
        .await
        .expect("the branch renders");
    let baseline = history.last().expect("the record's entry").baseline.clone();
    assert_eq!(
        baseline
            .iter()
            .map(|entry| entry.key.as_str())
            .collect::<Vec<_>>(),
        ["goal:state"],
        "no old key stays live: {baseline:#?}"
    );
    let stated = match &baseline[0].message.content {
        dim_agent_types::MessageContent::Text(text) => text.clone(),
        other => panic!("a state record is one text: {other:?}"),
    };
    assert!(
        stated.contains(r#"kind="state" status="paused""#),
        "{stated}"
    );

    // A compaction after the consume copies that baseline; the next request
    // carries the paused record and no old block.
    env.emit(
        "main",
        dim_agent_types::EventKind::ContextCompaction(ContextCompactionData {
            turn_id: None,
            request_id: None,
            command: None,
            start: None,
            through: end.sequence,
            outcome: ContextCompactionResult::Completed {
                retain_from: end.id.clone(),
                replacement: vec![summary_message("compacted")],
                strategy: "test".into(),
                diagnostics: Box::new(
                    serde_json::from_value(serde_json::json!({
                        "reason":"auto", "trigger":"threshold", "estimatedInputTokens":1,
                        "entriesReplaced":1, "modelCallCount":1, "durationMs":1,
                        "strategy":{"kind":"test"}
                    }))
                    .unwrap(),
                ),
                context_baseline: baseline.to_vec(),
                replacement_parts: None,
                pinned_inputs: None,
                digest_entries: None,
            },
        }),
    );
    let (context, history) = renderer
        .rendered_context(&timeline, timeline.sequence())
        .await
        .expect("the compacted branch renders");
    let request = context_messages(&context, &history)
        .iter()
        .filter_map(|message| match &message.content {
            dim_agent_types::MessageContent::Text(text) => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        request.contains(r#"kind="state" status="paused""#),
        "the paused state is carried: {request}"
    );
    assert!(
        !request.contains(r#"kind="goal" op="set""#),
        "no old objective block: {request}"
    );
    assert!(
        !request.contains(r#"kind="status" status="active""#),
        "no old status block: {request}"
    );
    env.close().await;
}

/// D4: a branch whose own segment carries no Goal block rebuilds without the
/// full fold: the probes read the opening and the inputs, and none of the 13
/// kinds is read.
#[tokio::test]
async fn e2_a_branch_without_a_goal_skips_the_full_fold() {
    let env = Env::start_refusing().await;
    let mut seed = env.seed_new("load-cheap").await;
    seed.append(
        "main",
        dim_agent_types::EventKind::BranchStart(dim_agent_types::BranchStartData {
            parent: None,
            create_event: None,
        }),
    );
    for index in 0..40 {
        seed.append("main", user(&format!("note {index}"), InputMode::Queue));
    }
    seed.finish().await;

    let before = env.reads().len();
    let env = env.restore_seeded("load-cheap").await;
    assert!(
        env.settled("main").await.is_empty(),
        "no goal, so nothing is written"
    );
    const KEYS: [&str; 13] = [
        "branch/start",
        "session/forked",
        "branch/create",
        "branch/create-failed",
        "branch/ended",
        "input/message",
        "input/consumed",
        "input/deleted",
        "turn/start",
        "turn/end",
        "stop/interrupt",
        "exec/started",
        "exec/ended",
    ];
    let expected: Vec<String> = KEYS.iter().map(|kind| (*kind).to_owned()).collect();
    let reads = env.reads();
    let reads = &reads[before..];
    let full: Vec<&Read> = reads
        .iter()
        .filter(|read| read.include_publishing && read.kinds == expected)
        .collect();
    assert!(full.is_empty(), "the full fold never runs: {full:?}");
    // The cheap path reads the inputs and the opening in one scan (D4).
    let scan: Vec<String> = [
        "input/message",
        "input/deleted",
        "branch/start",
        "session/forked",
    ]
    .iter()
    .map(|kind| (*kind).to_owned())
    .collect();
    let probes: Vec<&Read> = reads
        .iter()
        .filter(|read| read.include_publishing && read.kinds == scan)
        .collect();
    assert!(
        !probes.is_empty(),
        "the rebuild probes for a goal: {reads:?}"
    );
    assert!(
        probes.iter().all(|read| read.branch == "main"),
        "the probes read the branch itself: {probes:?}"
    );
    env.close().await;
}

/// R3: a start clear that also retires the old keys is one retired state
/// record; the old-key retires in the same input belong to its statement. A
/// reload whose newest Goal input is one takes the cheap path, not the full
/// fold.
#[tokio::test]
async fn e2_a_start_clear_with_old_key_retires_skips_the_full_fold() {
    let env = Env::start_refusing().await;
    let mut seed = env.seed_new("load-start-clear").await;
    seed.append(
        "main",
        dim_agent_types::EventKind::BranchStart(dim_agent_types::BranchStartData {
            parent: None,
            create_event: None,
        }),
    );
    // The opening's start clear, with both old keys retired in its input.
    let cleared = texts::start_clear("main", 1, (true, true));
    seed.append(
        "main",
        message(
            vec![
                raw(&cleared[0], Some("goal:state"), true),
                raw(&cleared[1], Some("goal:objective"), true),
                raw(&cleared[2], Some("goal:status"), true),
            ],
            InputMode::Append,
        ),
    );
    for index in 0..10 {
        seed.append("main", user(&format!("note {index}"), InputMode::Queue));
    }
    seed.finish().await;

    let before = env.reads().len();
    let env = env.restore_seeded("load-start-clear").await;
    assert_eq!(
        env.writes("main").await.len(),
        1,
        "the opening's clear is the only Goal input; the rebuild writes nothing"
    );
    const KEYS: [&str; 13] = [
        "branch/start",
        "session/forked",
        "branch/create",
        "branch/create-failed",
        "branch/ended",
        "input/message",
        "input/consumed",
        "input/deleted",
        "turn/start",
        "turn/end",
        "stop/interrupt",
        "exec/started",
        "exec/ended",
    ];
    let expected: Vec<String> = KEYS.iter().map(|kind| (*kind).to_owned()).collect();
    let reads = env.reads();
    let reads = &reads[before..];
    let full: Vec<&Read> = reads
        .iter()
        .filter(|read| read.include_publishing && read.kinds == expected)
        .collect();
    assert!(full.is_empty(), "the full fold never runs: {full:?}");
    let scan: Vec<String> = [
        "input/message",
        "input/deleted",
        "branch/start",
        "session/forked",
    ]
    .iter()
    .map(|kind| (*kind).to_owned())
    .collect();
    let probes: Vec<&Read> = reads
        .iter()
        .filter(|read| read.include_publishing && read.kinds == scan)
        .collect();
    assert!(
        !probes.is_empty(),
        "the rebuild probes the newest Goal block: {reads:?}"
    );
    env.close().await;
}

/// R5, R3: the newest Goal block is a `clear` trigger whose record sits
/// immediately before it in the same atomic batch. The scan classifies the
/// pair by the record, so the branch's own history still reads as no goal and
/// the rebuild keeps the cheap path.
#[tokio::test]
async fn e2_a_clear_batch_takes_the_cheap_rebuild() {
    let env = Env::start_refusing().await;
    let mut seed = env.seed_new("load-clear-batch").await;
    seed.append(
        "main",
        dim_agent_types::EventKind::BranchStart(dim_agent_types::BranchStartData {
            parent: None,
            create_event: None,
        }),
    );
    let set = seed.append(
        "main",
        message(
            vec![raw(&texts::set("main", "ship it"), None, false)],
            InputMode::Queue,
        ),
    );
    seed.append(
        "main",
        message(
            vec![raw(
                &texts::set_state(
                    "main",
                    "ship it",
                    &texts::Answer::After(set.sequence),
                    false,
                ),
                Some("goal:state"),
                false,
            )],
            InputMode::Append,
        ),
    );
    let record = dim_agent_types::EventKind::InputMessage(dim_agent_types::InputMessageData {
        submission_id: Some("test:clear-record".into()),
        user: None,
        blocks: vec![raw(
            &texts::cleared("main", &texts::Answer::Trigger("test:clear".into())),
            Some("goal:state"),
            true,
        )],
        mode: Some(InputMode::Append),
        handover: None,
    });
    let trigger = dim_agent_types::EventKind::InputMessage(dim_agent_types::InputMessageData {
        submission_id: Some("test:clear".into()),
        user: None,
        blocks: vec![raw(&texts::clear("main"), None, false)],
        mode: Some(InputMode::Append),
        handover: None,
    });
    seed.append_batch("main", vec![record, trigger]);
    seed.finish().await;

    let before = env.reads().len();
    let env = env.restore_seeded("load-clear-batch").await;
    assert_eq!(
        env.writes("main").await.len(),
        4,
        "the set and its record, the clear's record and its trigger; the rebuild writes nothing"
    );
    let reads = env.reads();
    let reads = &reads[before..];
    let full: Vec<&Read> = reads
        .iter()
        .filter(|read| read.kinds.len() == 13 && read.include_publishing)
        .collect();
    assert!(full.is_empty(), "the full fold never runs: {full:?}");
    let scan = [
        "input/message",
        "input/deleted",
        "branch/start",
        "session/forked",
    ]
    .iter()
    .map(|kind| (*kind).to_owned())
    .collect::<Vec<_>>();
    assert!(
        reads
            .iter()
            .any(|read| read.include_publishing && read.kinds == scan),
        "the rebuild probes the newest Goal block: {reads:?}"
    );
    env.close().await;
}

/// R6.1: a deletion whose compensating record never committed. The fold
/// recomputes to the state before the withdrawn change while the newest
/// published `goal:state` block still names it. The rebuild restates the
/// fold's state once, the keyed read a client uses shows it, and a second load
/// writes nothing.
#[tokio::test]
async fn e3_a_rebuild_restates_the_state_a_deletion_left_published() {
    const KEYS: [&str; 13] = [
        "branch/start",
        "session/forked",
        "branch/create",
        "branch/create-failed",
        "branch/ended",
        "input/message",
        "input/consumed",
        "input/deleted",
        "turn/start",
        "turn/end",
        "stop/interrupt",
        "exec/started",
        "exec/ended",
    ];
    let env = Env::start_refusing().await;
    let mut seed = env.seed_new("load-reconcile").await;
    seed.append(
        "main",
        dim_agent_types::EventKind::BranchStart(dim_agent_types::BranchStartData {
            parent: None,
            create_event: None,
        }),
    );
    let first = seed.append(
        "main",
        message(
            vec![raw(&texts::set("main", "first"), None, false)],
            InputMode::Queue,
        ),
    );
    seed.append(
        "main",
        message(
            vec![raw(
                &texts::set_state(
                    "main",
                    "first",
                    &texts::Answer::After(first.sequence),
                    false,
                ),
                Some("goal:state"),
                false,
            )],
            InputMode::Append,
        ),
    );
    let second = seed.append(
        "main",
        message(
            vec![raw(&texts::edit("main", "second"), None, false)],
            InputMode::Append,
        ),
    );
    seed.append(
        "main",
        message(
            vec![raw(
                &texts::edit_state(
                    "main",
                    "second",
                    &texts::Answer::After(second.sequence),
                    false,
                ),
                Some("goal:state"),
                false,
            )],
            InputMode::Append,
        ),
    );
    // The deletion of the second change's trigger. A live branch answers it
    // with a compensating record; this history stops before that record
    // commits.
    let deletion = seed.append(
        "main",
        dim_agent_types::EventKind::InputDeleted(dim_agent_types::InputDeletedData {
            input_id: second.id.clone(),
        }),
    );
    seed.finish().await;

    let before = env.reads().len();
    let env = env.restore_seeded("load-reconcile").await;
    env.settled("main").await;
    let first_state = texts::restated("main", "first", deletion.sequence, false);
    let keys = env.keys("main").await;
    assert_eq!(keys.len(), 1, "one live key: {keys:?}");
    assert_eq!(keys[0].0, "goal:state");
    assert_eq!(
        keys[0].1, first_state,
        "the published block states the recomputed state, not the withdrawn one"
    );
    let writes = env.writes("main").await;
    assert_eq!(
        writes.len(),
        5,
        "exactly one new record over the four seeded inputs: {writes:#?}"
    );
    let reads = env.reads();
    let reads = &reads[before..];
    let full: Vec<&Read> = reads
        .iter()
        .filter(|read| read.kinds.iter().map(String::as_str).eq(KEYS))
        .collect();
    assert!(
        !full.is_empty(),
        "the deletion sends the rebuild to the full fold: {reads:?}"
    );
    let scan = [
        "input/message",
        "input/deleted",
        "branch/start",
        "session/forked",
    ]
    .iter()
    .map(|kind| (*kind).to_owned())
    .collect::<Vec<_>>();
    assert!(
        reads
            .iter()
            .any(|read| read.kinds == scan && read.direction == TimelineQueryDirection::Descending),
        "the scan classified the deletion's branch: {reads:?}"
    );

    // It settles once: the published block now agrees with the fold.
    let env = env.restore().await;
    assert_eq!(
        env.writes("main").await.len(),
        5,
        "a second load writes nothing"
    );
    let keys = env.keys("main").await;
    assert_eq!(keys[0].1, first_state, "and the state still holds");
    env.close().await;
}

/// R7.1: a restatement yields to the record a later decision writes. The
/// loaded history carries a lost compensation — the newest published
/// `goal:state` names a change whose trigger was deleted and no record
/// followed — and a `continue` conclusion the settle pass still owes. The
/// rebuild restates the recomputed state; the paused end of the background
/// task then settles the conclusion over the limit, and the pause record it
/// writes is the one the keyed read serves. The restatement neither claims
/// the rebuild's cutoff as the newest `after` (which would drop the pause)
/// nor publishes the pre-pause state over it, and a second load writes
/// nothing.
#[tokio::test]
async fn e3_a_restatement_yields_to_the_record_a_later_decision_writes() {
    let env = Env::start_refusing().await;
    // The history already used its one continuation: the settle pass decides
    // the deferred conclusion as a pause, which is a state record.
    env.set_limit(1);
    let mut seed = env.seed_new("load-restate-yields").await;
    seed.append(
        "main",
        dim_agent_types::EventKind::BranchStart(dim_agent_types::BranchStartData {
            parent: None,
            create_event: None,
        }),
    );
    let set = seed.append(
        "main",
        message(
            vec![raw(&texts::evaluator_set("main", "ship it"), None, false)],
            InputMode::Queue,
        ),
    );
    seed.append(
        "main",
        dim_agent_types::EventKind::InputConsumed(dim_agent_types::InputConsumedData {
            input_id: set.id.clone(),
        }),
    );
    seed.append(
        "main",
        message(
            vec![raw(
                &texts::set_state("main", "ship it", &texts::Answer::After(set.sequence), true),
                Some("goal:state"),
                false,
            )],
            InputMode::Append,
        ),
    );
    let edit = seed.append(
        "main",
        message(
            vec![raw(&texts::edit("main", "second"), None, false)],
            InputMode::Queue,
        ),
    );
    seed.append(
        "main",
        dim_agent_types::EventKind::InputConsumed(dim_agent_types::InputConsumedData {
            input_id: edit.id.clone(),
        }),
    );
    seed.append(
        "main",
        message(
            vec![raw(
                &texts::edit_state("main", "second", &texts::Answer::After(edit.sequence), true),
                Some("goal:state"),
                false,
            )],
            InputMode::Append,
        ),
    );
    seed.append(
        "main",
        dim_agent_types::EventKind::TurnStart(dim_agent_types::TurnStartData {
            turn_id: "work".into(),
        }),
    );
    seed.append(
        "main",
        dim_agent_types::EventKind::ExecStarted(dim_agent_types::ExecStartedData {
            turn_id: "work".into(),
            request_id: "request".into(),
            call_id: "call-1".into(),
            task_id: "task-1".into(),
            description: "background work".into(),
            output_path: "/dev/null".into(),
        }),
    );
    seed.append(
        "main",
        message(
            vec![raw(
                &texts::legacy_report("main", "completed", "all tests pass"),
                Some("goal:status"),
                false,
            )],
            InputMode::Append,
        ),
    );
    let end = seed.append("main", turn_end_with("work", "completed"));
    // The continuation the completed turn end wrote, already used: the limit
    // the settle pass reads leaves the deferred conclusion a pause.
    let continuation = seed.append(
        "main",
        message(
            vec![raw(
                &texts::continuation("main", 1, Some(1), 0, end.sequence),
                None,
                false,
            )],
            InputMode::Queue,
        ),
    );
    seed.append(
        "main",
        dim_agent_types::EventKind::InputConsumed(dim_agent_types::InputConsumedData {
            input_id: continuation.id.clone(),
        }),
    );
    let create = seed.append(
        "main",
        dim_agent_types::EventKind::BranchCreate(BranchCreateData {
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
        dim_agent_types::EventKind::BranchStart(dim_agent_types::BranchStartData {
            parent: Some("main".into()),
            create_event: Some(create.sequence),
        }),
    );
    seed.append(
        "eval",
        message(vec![raw("line work", None, false)], InputMode::Queue),
    );
    let conclusion = seed.append(
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
    // The second change's trigger is deleted and no record follows: the fold
    // recomputes to the first goal while the published block still names it.
    let deletion = seed.append(
        "main",
        dim_agent_types::EventKind::InputDeleted(dim_agent_types::InputDeletedData {
            input_id: edit.id.clone(),
        }),
    );
    seed.finish().await;

    let env = env.restore_seeded("load-restate-yields").await;
    let before = env.settled("main").await;
    assert!(
        before
            .iter()
            .any(|block| block.text == texts::restated("main", "ship it", deletion.sequence, true)),
        "the load restates the recomputed state: {before:#?}"
    );

    // The background task's cancelled end settles the branch: the deferred
    // conclusion is decided over the limit and writes its pause record.
    env.exec_ended("main", "task-1", "cancelled");
    let pause = env.wait_for("main", r#"reason="limit""#).await;
    assert_eq!(
        pause.text,
        texts::evaluated_pause(
            "main",
            "ship it",
            "limit",
            "the automatic continuation limit was reached",
            conclusion.sequence,
        ),
        "the restatement yields to the record the later decision writes"
    );
    let keys = env.keys("main").await;
    assert_eq!(keys.len(), 1, "one live key: {keys:?}");
    assert_eq!(keys[0].0, "goal:state");
    assert_eq!(
        keys[0].1, pause.text,
        "the published block states the conclusion's outcome"
    );
    let writes = env.settled("main").await.len();

    // It settles once: the published block now agrees with the fold.
    let env = env.restore().await;
    assert_eq!(
        env.writes("main").await.len(),
        writes,
        "a second load writes nothing"
    );
    assert_eq!(
        env.keys("main").await[0].1,
        pause.text,
        "and the state still holds"
    );
    env.close().await;
}

/// R6.2: a change issued while the branch part is still rebuilding waits for
/// the rebuild and the facts it held, so its record states the change applied
/// to the complete fold.
///
/// The change is a `goal_update` call: `session.goal.*` checks the tree first
/// (`crates/dim-goal/src/session.rs:164`), so it refuses a branch that is still
/// building, while the tool reaches the Owner directly
/// (`crates/dim-goal/src/session.rs:217`). The child line is opened without
/// waiting for the create, and its Goal part's first read is held, so the
/// change meets the Owner before its rebuild has folded anything. A child has
/// no goal of its own, so the complete fold leaves the state retired: what the
/// case pins is that the change waits instead of composing against the empty
/// fold and failing on the unpublished branch.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn e3_a_change_waits_for_the_branch_rebuild() {
    const SCAN: [&str; 4] = [
        "input/message",
        "input/deleted",
        "branch/start",
        "session/forked",
    ];
    let env = Env::start_gated().await;
    // The child's scan, not any read main still owes.
    let (entered, release) = env.gated().hold_read_other_than(&SCAN, Some("main"));
    // The create is not awaited: its build waits for the rebuild it starts.
    let commands = env.probe.commands("main");
    let at = env.probe.timeline("main").sequence();
    let create = tokio::spawn(async move {
        commands
            .branch(
                "main".to_owned(),
                at,
                BranchOptions {
                    prompt: "line work".into(),
                    source: BranchSource::Host {
                        operation_id: "r62-child".into(),
                    },
                },
                CancellationToken::new(),
            )
            .await
    });
    // The child's id is on the parent's committed `branch/create`; the Owner
    // enters the table at the start of its part's apply, before the rebuild.
    let child = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if let Some(create) = env.creates("main").await.last() {
                return create.branch.clone();
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the parent holds its branch/create");
    tokio::time::timeout(Duration::from_secs(15), entered)
        .await
        .expect("the child's rebuild reached its scan")
        .expect("the gate signals");

    // The change must wait for the rebuild, never compose from the one that
    // has not run yet.
    let tools = env.probe.tools();
    let handler = tools
        .contributed("goal_update")
        .expect("goal_update is registered")
        .handler
        .clone();
    let call = dim_agent_types::ToolCall {
        id: "r62-call".to_owned(),
        name: "goal_update".to_owned(),
        input: serde_json::json!({"status": "blocked", "summary": "wait for the rebuild"})
            .as_object()
            .cloned()
            .expect("an object"),
    };
    let context = dim_agent_capabilities::host::ToolExecutionContext {
        turn_id: "r62-turn".to_owned(),
        request_id: "r62-request".to_owned(),
        branch_id: child.clone(),
    };
    let mut report = Box::pin(handler.execute(call, context, CancellationToken::new()));
    assert!(
        tokio::time::timeout(Duration::from_millis(300), &mut report)
            .await
            .is_err(),
        "the change composed before the rebuild finished"
    );
    release.send(()).expect("the gate is armed");
    let result = report.await.expect("the tool runs");
    let opened = tokio::time::timeout(Duration::from_secs(15), create)
        .await
        .expect("the child opens")
        .expect("the create task");
    assert_eq!(opened.expect("the child branch").id(), child);
    let dim_agent_types::ToolContent::Text(text) = &result.content else {
        panic!("the change replies with text: {result:?}");
    };
    assert!(
        text.contains("goal_update recorded: blocked."),
        "the change ran once the branch was ready: {result:?}"
    );
    let trigger = env.wait_for(&child, r#"op="blocked""#).await;
    let record = env.wait_for(&child, r#"status="cleared""#).await;
    assert_eq!(
        record.text,
        texts::cleared(&child, &texts::trigger(&trigger)),
        "the record states the change applied to the complete fold"
    );
    env.close().await;
}

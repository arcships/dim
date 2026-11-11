mod support;

use dim_agent_capabilities::history::{HistoryRenderer, context_messages, summary_message};
use dim_agent_runtime::{BranchOptions, TimelineQueryDirection};
use dim_agent_types::{
    BranchCreateAt, BranchCreateData, BranchSource, ContextCompactionData, ContextCompactionResult,
};
use dim_agent_types::{EventKind, ExtensionEvent, InputConsumedData, InputMode};
use serde_json::json;
use std::time::Duration;
use support::parts::*;
use support::texts;
use tokio_util::sync::CancellationToken;

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

async fn states(env: &Env) -> Vec<serde_json::Value> {
    env.all_events("main")
        .await
        .into_iter()
        .filter_map(|event| match &event.kind {
            EventKind::Extension(data) if data.key() == "goal/state" => Some(data.data().clone()),
            _ => None,
        })
        .collect()
}

fn state(objective: Option<&str>, status: serde_json::Value) -> EventKind {
    EventKind::Extension(ExtensionEvent::new("goal","state",json!({"schema":"goal-state/v1","operation":if objective.is_some(){"set"}else{"clear"},"objective":objective,"state":status,"judge":"model","input_id":null})).unwrap())
}

#[tokio::test]
async fn a_new_owner_pauses_an_unclosed_goal_without_writing_or_replaying_work() {
    let env = Env::start_refusing().await;
    let mut seed = env.seed_new("unclosed").await;
    seed.append("main", state(Some("ship it"), json!({"kind":"active"})));
    let old = seed.append(
        "main",
        message(
            vec![raw(
                &texts::continuation("main", 4, None, 1, 1),
                None,
                false,
            )],
            InputMode::Queue,
        ),
    );
    seed.append("main", turn_end_with("old", "completed"));
    seed.finish().await;
    let before = env.reads().len();
    let env = env.restore_seeded("unclosed").await;
    let read = env.goal("get", json!({})).await.unwrap();
    assert_eq!(read["goal"]["objective"], "ship it");
    assert_eq!(
        read["goal"]["status"],
        json!({"kind":"paused","reason":"interrupted_reload"})
    );
    assert_eq!(states(&env).await.len(), 1);
    assert_eq!(env.settled("main").await.len(), 1);
    assert!(env.reads()[before..].iter().all(|query|!matches!(&query.select,dim_agent_runtime::QuerySelect::Fields {fields} if fields.iter().any(|field|field=="data.blocks"))));
    env.emit(
        "main",
        EventKind::InputConsumed(InputConsumedData {
            input_id: old.id.clone(),
        }),
    );
    env.turn_ids("main", &[]);
    assert_eq!(
        states(&env).await.len(),
        1,
        "late old events do not save recovery or restart execution"
    );
    env.goal("resume", json!({})).await.unwrap();
    let resume = env.wait_for("main", r#"op="resume""#).await;
    let end = env.turn("main", &[&resume]);
    let continuation = env
        .wait_for("main", r#"kind="continuation" count="1""#)
        .await;
    assert_eq!(
        continuation.text,
        texts::continuation("main", 1, None, 0, end.sequence)
    );
    assert_eq!(states(&env).await.len(), 2);
    env.close().await;
}

#[tokio::test]
async fn only_goal_business_changes_save_state() {
    let env = Env::start_refusing().await;
    let ordinary = env.emit("main", user("ordinary chat", InputMode::Queue));
    env.turn_ids("main", &[&ordinary.id]);
    assert!(states(&env).await.is_empty());
    let goal = env.set("ship it").await;
    let mut driver = goal;
    for count in 1..=2 {
        let end = env.turn("main", &[&driver]);
        driver = env
            .wait_for("main", &format!(r#"kind="continuation" count="{count}""#))
            .await;
        assert_eq!(
            driver.text,
            texts::continuation("main", count, None, count - 1, end.sequence)
        );
    }
    assert_eq!(states(&env).await.len(), 1);
    env.tool("main", json!({"status":"completed","summary":"done"}))
        .await;
    assert_eq!(
        env.goal("get", json!({})).await.unwrap()["goal"]["status"]["kind"],
        "completed"
    );
    let saved = states(&env).await;
    assert_eq!(saved.len(), 2);
    assert_eq!(saved[0]["objective"], "ship it");
    assert_eq!(saved[1]["objective"], "ship it");
    assert_eq!(saved[1]["state"], json!({"kind":"completed","text":"done"}));
    assert!(env.all_events("main").await.iter().all(|event|!matches!(&event.kind,EventKind::Extension(data) if matches!(data.key(),"goal/recovered"|"goal/driving-node"|"goal/input-boundary"))));
    let queries = env.reads().len();
    env.goal("get", json!({})).await.unwrap();
    assert_eq!(env.reads().len(), queries);
    assert_eq!(states(&env).await.len(), 2);
    env.close().await;
}

#[tokio::test]
async fn graceful_unload_saves_one_pause_and_reload_preserves_it() {
    let env = Env::start_refusing().await;
    env.set("ship it").await;
    let env = env.restore().await;
    let read = env.goal("get", json!({})).await.unwrap();
    assert_eq!(
        read["goal"]["status"],
        json!({"kind":"paused","reason":"interrupted_unload"})
    );
    let saved = states(&env).await;
    assert_eq!(saved.len(), 2);
    assert_eq!(saved[1]["operation"], "pause");
    let env = env.restore().await;
    assert_eq!(states(&env).await.len(), 2);
    env.close().await;
}

#[tokio::test]
async fn finished_and_cleared_goals_load_without_new_facts() {
    for (name, objective, status, kind) in [
        (
            "paused",
            Some("ship it"),
            json!({"kind":"paused","text":"user"}),
            "paused",
        ),
        (
            "completed",
            Some("ship it"),
            json!({"kind":"completed","text":"done"}),
            "completed",
        ),
        (
            "blocked",
            Some("ship it"),
            json!({"kind":"blocked","text":"needs credentials"}),
            "blocked",
        ),
        ("clear", None, json!({"kind":"none"}), "none"),
    ] {
        let env = Env::start().await;
        let mut seed = env.seed_new(name).await;
        seed.append("main", state(objective, status));
        seed.finish().await;
        let env = env.restore_seeded(name).await;
        let read = env.goal("get", json!({})).await.unwrap();
        if kind == "none" {
            assert!(read["goal"].is_null());
        } else {
            assert_eq!(read["goal"]["status"]["kind"], kind);
        }
        assert_eq!(states(&env).await.len(), 1);
        assert!(env.settled("main").await.is_empty());
        env.close().await;
    }
}

#[tokio::test]
async fn an_invalid_latest_business_state_never_uses_an_older_one() {
    for case in ["schema", "objective", "field"] {
        let env = Env::start().await;
        let mut seed = env.seed_new(case).await;
        seed.append(
            "main",
            state(Some("valid older goal"), json!({"kind":"active"})),
        );
        let EventKind::Extension(valid) =
            state(Some("invalid latest goal"), json!({"kind":"active"}))
        else {
            unreachable!()
        };
        let mut invalid = valid.data().clone();
        match case {
            "schema" => invalid["schema"] = json!("goal-state/future"),
            "objective" => invalid["objective"] = json!(17),
            "field" => {
                invalid.as_object_mut().unwrap().remove("state");
            }
            _ => unreachable!(),
        };
        seed.append(
            "main",
            EventKind::Extension(ExtensionEvent::new("goal", "state", invalid).unwrap()),
        );
        seed.finish().await;
        let error = tokio::time::timeout(Duration::from_secs(15), env.restore_failure(case))
            .await
            .unwrap();
        assert!(error.to_string().contains("Goal"), "{case}: {error}");
        env.close().await;
    }
}

#[tokio::test]
async fn obsolete_execution_snapshots_do_not_override_the_business_state() {
    let env = Env::start().await;
    let mut seed = env.seed_new("obsolete").await;
    seed.append(
        "main",
        state(Some("ship it"), json!({"kind":"paused","text":"user"})),
    );
    for name in ["recovered", "driving-node", "input-boundary"] {
        seed.append(
            "main",
            EventKind::Extension(
                ExtensionEvent::new("goal", name, json!({"schema":"unknown obsolete cache"}))
                    .unwrap(),
            ),
        );
    }
    seed.finish().await;
    let env = env.restore_seeded("obsolete").await;
    assert_eq!(
        env.goal("get", json!({})).await.unwrap()["goal"]["status"],
        json!({"kind":"paused","reason":"user"})
    );
    assert_eq!(states(&env).await.len(), 1);
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
async fn e2_an_inherited_check_skips_unrelated_inputs() {
    let env = Env::start_refusing().await;
    let mut seed = env.seed_new("load-inherited").await;
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
    let pages = 3;
    let scan_read = |read: &Read| {
        read.kinds == ["input/message".to_owned()]
            && read.direction == TimelineQueryDirection::Descending
            && read.branch == "main"
            && read.through == cut
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
        "unrelated inputs produce no rows to paginate: {scan:?}"
    );
    for read in &scan {
        assert_eq!(
            read.branch, "main",
            "the check reads the ancestor, never the branch it opens: {read:?}"
        );
        assert_eq!(read.limit, 64, "a page keeps 64 facts: {read:?}");
        assert_eq!(
            read.from, 1,
            "a descending page is bounded by its through: {read:?}"
        );
    }
    assert_eq!(
        scan[0].through, cut,
        "the check uses the fixed ancestor cut"
    );
    env.close().await;
}

/// D3: a change trigger the previous generation never answered is answered
/// once at load, naming the trigger; the next load writes nothing more.

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
    let reads = env.reads();
    let reads = &reads[before..];
    let full: Vec<&Read> = reads
        .iter()
        .filter(|read| matches!(read.select, dim_agent_runtime::QuerySelect::Events))
        .collect();
    assert!(full.is_empty(), "the full fold never runs: {full:?}");
    let scan: Vec<String> = ["input/message"]
        .iter()
        .map(|kind| (*kind).to_owned())
        .collect();
    let probes: Vec<&Read> = reads.iter().filter(|read| read.kinds == scan).collect();
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
    let reads = env.reads();
    let reads = &reads[before..];
    let full: Vec<&Read> = reads
        .iter()
        .filter(|read| matches!(read.select, dim_agent_runtime::QuerySelect::Events))
        .collect();
    assert!(full.is_empty(), "the full fold never runs: {full:?}");
    let scan: Vec<String> = ["input/message"]
        .iter()
        .map(|kind| (*kind).to_owned())
        .collect();
    let probes: Vec<&Read> = reads.iter().filter(|read| read.kinds == scan).collect();
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn e3_a_change_waits_for_the_branch_rebuild() {
    const SCAN: [&str; 2] = ["branch/start", "session/forked"];
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

#[tokio::test]
async fn records_pair_with_the_original_next_input_across_candidate_pages() {
    for (gap, candidate) in [(false, false), (false, true), (true, false)] {
        let env = Env::start().await;
        let mut seed = env.seed_new("pair-position").await;
        seed.append(
            "main",
            dim_agent_types::EventKind::BranchStart(dim_agent_types::BranchStartData {
                parent: None,
                create_event: None,
            }),
        );
        for _ in 0..255 {
            seed.append(
                "main",
                message(
                    vec![raw("<dim-goal invalid>", None, false)],
                    InputMode::Append,
                ),
            );
        }
        let state = texts::set_state(
            "main",
            "paired objective",
            &texts::Answer::Trigger("pair".into()),
            false,
        );
        seed.append(
            "main",
            message(
                vec![raw(&state, Some("goal:state"), false)],
                InputMode::Append,
            ),
        );
        if gap {
            seed.append(
                "main",
                dim_agent_types::EventKind::Text(dim_agent_types::DeltaData {
                    turn_id: "t".into(),
                    request_id: "r".into(),
                    delta: "not a Goal input".into(),
                }),
            );
        }
        let mut trigger = input_message(
            if candidate {
                vec![raw("<dim-goal invalid>", None, false)]
            } else {
                vec![]
            },
            InputMode::Append,
        );
        trigger.submission_id = Some("pair".into());
        seed.append("main", dim_agent_types::EventKind::InputMessage(trigger));
        seed.finish().await;
        let env = env.restore_seeded("pair-position").await;
        env.goal("edit", serde_json::json!({"objective": "edited objective"}))
            .await
            .unwrap();
        let writes = env.settled("main").await;
        let record = writes
            .iter()
            .rev()
            .find(|block| block.marker.contains(r#"kind="state""#))
            .unwrap();
        if gap {
            assert!(
                record.marker.contains(r#"status="none""#),
                "a filtered-out event breaks the pair: {record:?}"
            );
        } else {
            assert!(
                record.marker.contains(r#"status="paused""#),
                "the adjacent non-candidate input pairs: {record:?}"
            );
            assert!(record.text.contains("edited objective"));
        }
        env.close().await;
    }
}

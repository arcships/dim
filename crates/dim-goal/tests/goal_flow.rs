//! Goal's flows through its assembled parts: the Session part writes change
//! triggers by branch id, the branch part folds the branch's committed facts,
//! states the state in a `goal:state` record and decides continuation. The
//! product profile's agent loop drives the turns these inputs queue.
mod support;

use dim_agent_types::{EventKind, InputMode};
use serde_json::json;
use support::*;

const OBJECTIVE: &str = "ship it";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_set_asks_for_the_objective_and_goal_states_it_in_a_record() {
    let mut fixture = Fixture::start().await;
    let sequence = fixture
        .goal("set", json!({"objective": OBJECTIVE}))
        .await
        .unwrap();
    let writes = fixture.settled_writes("main").await;
    assert_eq!(writes.len(), 2, "{writes:#?}");
    assert_eq!(
        writes[1].sequence, sequence,
        "the method replies with the trigger's sequence"
    );
    assert_eq!(writes[0].mode, InputMode::Append);
    assert_eq!(writes[0].blocks, 1, "{}", writes[0].text);
    assert_eq!(
        writes[0].text,
        texts::set_state("main", OBJECTIVE, &texts::trigger(&writes[1]), false)
    );
    assert_eq!(writes[1].mode, InputMode::Queue);
    assert_eq!(writes[1].blocks, 1, "{}", writes[1].text);
    assert_eq!(writes[1].text, texts::set("main", OBJECTIVE));
    let all = fixture.events("main", 0).await;
    let state = all
        .iter()
        .find(|event| event.sequence == writes[0].sequence)
        .expect("the state record");
    let EventKind::InputMessage(data) = &state.kind else {
        panic!("the state record is an input message")
    };
    let dim_agent_types::InputBlock::Raw { text, key, retire } = &data.blocks[0];
    assert_eq!(
        text,
        &texts::set_state("main", OBJECTIVE, &texts::trigger(&writes[1]), false)
    );
    assert_eq!(key.as_deref(), Some("goal:state"));
    assert!(!retire);
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_completed_turn_drives_the_continuation_and_the_limit_pauses_it() {
    let mut fixture = Fixture::start().await;
    fixture.set_limit(1).await;
    let id = fixture.id.clone();
    fixture
        .goal("set", json!({"objective": OBJECTIVE}))
        .await
        .unwrap();
    let paused = fixture.wait_for("main", r#"reason="limit""#).await;
    fixture.idle_on(&id, "main").await;
    let ends = turn_ends(fixture.default_session(), "main").await;
    assert_eq!(ends.len(), 2, "two turns: the set's, the continuation's");
    let continuation = fixture.wait_for("main", r#"kind="continuation""#).await;
    assert_eq!(
        continuation.text,
        texts::continuation("main", 1, Some(1), 0, ends[0]),
        "the continuation counts the turn that drove it"
    );
    assert_eq!(continuation.mode, InputMode::Queue);
    assert_eq!(
        paused.text,
        texts::automatic_pause(
            "main",
            OBJECTIVE,
            "limit",
            "the automatic continuation limit was reached",
            ends[1]
        )
    );
    let writes = fixture.settled_writes("main").await;
    assert_eq!(writes.len(), 4, "{writes:#?}");
    fixture.close().await;
}

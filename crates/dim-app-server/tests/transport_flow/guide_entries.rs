//! The plugin bridge's guide-entry route ([Prompt §6.1]): a TypeScript row
//! registers an entry from its `apply`, the System the Session adopts next
//! carries it, and it leaves with the row that registered it.
//!
//! [Prompt §6.1]: ../../../../docs/design/plugins/Prompt.md#61-the-plugin-bridge
use super::reload::last_system;
use super::session_typescript::{
    ControlledGate, fixture, new_session, session_copies, setup, submit, when_served,
};
use super::*;
use dim_agent_capabilities::prompt_guide::GuideEntries;
use dim_agent_runtime::names::name_of;
use dim_app_server::session_typescript::SessionTypeScriptConfig;
use rutis_bridge::session::{HostDispatch, host_key_in};
use std::path::{Path, PathBuf};

/// The marker and the entry the fixture registers for it.
const MARKER: &str = "dim-guide-fixture";
const ENTRY: &str = "<dim-guide-fixture>: a fixture marker the runtime writes.";

fn scratch(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "dm-{label}-{}",
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The product's Session rows, with the guide-entry fixture as their only
/// row: it registers `ENTRY` while `registerFile` exists.
fn parts(register_file: &Path) -> SessionTypeScriptConfig {
    let mut parts = fixture();
    let root = parts.anchor.parent().unwrap().join("fixtures");
    parts.rows = vec![json!({
        "id": "guide",
        "name": root.join("guide-entries.ts"),
        "provides": {},
        "config": { "registerFile": register_file, "marker": MARKER },
    })];
    parts
}

/// The model's tool-name channel is held for the run: the fixture's model
/// driver reports to it and panics once its receiver is gone.
async fn launch(
    register_file: &Path,
    controls: tokio::sync::mpsc::UnboundedSender<super::session_typescript::Control>,
) -> (
    RunningServer,
    Client,
    tokio::sync::mpsc::UnboundedReceiver<Vec<String>>,
) {
    let (mut builder, requests, _) = setup();
    builder.require_feature(ControlledGate::feature(controls));
    let server = serve(
        builder,
        ServerConfig {
            unix_path: Some(std::env::temp_dir().join(format!(
                "dm-guide-{}/server.sock",
                &uuid::Uuid::new_v4().simple().to_string()[..8]
            ))),
            session_typescript: Some(parts(register_file)),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let client = Client::new_version(&server, false, 3).await;
    (server, client, requests)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires DIM_TEST_NODE and DIM_TEST_JS_RUNTIME"]
async fn node_guide_entries_registration_reaches_the_next_adoption_and_leaves_with_its_row() {
    let dir = scratch("guide-entry");
    let register_file = dir.join("register");
    std::fs::write(&register_file, "on").unwrap();
    let (send, mut controls) = tokio::sync::mpsc::unbounded_channel();
    let (server, mut client, _requests) = launch(&register_file, send).await;

    let id = new_session(&mut client, json!(["echo"])).await;
    let (session_instance, ctx, _, _) = controls.recv().await.unwrap();
    let session = server.agent().unwrap().get_session(&id).unwrap();
    // The row's part registers from its own `apply` when the Session opens.
    let status = when_served(
        &mut client,
        "session.guide_fixture.status",
        json!({"sessionId":id}),
    )
    .await;
    assert_ok(&status);
    assert_eq!(status["result"]["registered"], true, "{status}");

    // Registration: the System the Session adopts next carries the entry.
    submit(&mut client, &id, "while registered").await;
    assert_ok(
        &client
            .rpc("session.idle", json!({"sessionId":id,"branchId":"main"}))
            .await,
    );
    let before = last_system(&session, "main").await;
    assert!(before.contains(ENTRY), "{before:?}");

    // Unload: the restarted row's copy registers nothing, and the entry is
    // gone from the next adoption.
    std::fs::remove_file(&register_file).unwrap();
    let copies = session_copies(&ctx, session_instance, "GuideEntriesFixture");
    assert_eq!(copies.len(), 1, "one copy per Session");
    copies[0].restart().await.unwrap();
    let status = when_served(
        &mut client,
        "session.guide_fixture.status",
        json!({"sessionId":id}),
    )
    .await;
    assert_ok(&status);
    assert_eq!(status["result"]["registered"], false, "{status}");
    submit(&mut client, &id, "after the reload").await;
    assert_ok(
        &client
            .rpc("session.idle", json!({"sessionId":id,"branchId":"main"}))
            .await,
    );
    let after = last_system(&session, "main").await;
    assert!(!after.contains(ENTRY), "{after:?}");

    server.shutdown();
    server.join().await.unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires DIM_TEST_NODE and DIM_TEST_JS_RUNTIME"]
async fn node_guide_entries_without_a_caller_row_registers_nothing() {
    let dir = scratch("guide-no-caller");
    let register_file = dir.join("register");
    std::fs::write(&register_file, "on").unwrap();
    let (send, mut controls) = tokio::sync::mpsc::unbounded_channel();
    let (server, mut client, _requests) = launch(&register_file, send).await;
    let id = new_session(&mut client, json!(["echo"])).await;
    let (session_instance, ctx, _, _) = controls.recv().await.unwrap();
    when_served(
        &mut client,
        "session.guide_fixture.status",
        json!({"sessionId":id}),
    )
    .await;

    // A call from Rust reaches the dispatch outside any runtime row.
    let dispatch = ctx
        .get_at::<dyn HostDispatch>(&host_key_in(&name_of::<GuideEntries>(), session_instance))
        .expect("the Session serves guideEntries");
    let reply = dispatch.invoke(
        "register",
        rutis_bridge::session::Value::Data(json!([
            "session",
            {"markers": ["dim-guide-no-caller"], "order": 56, "text": "<dim-guide-no-caller>: refused."}
        ])),
    );
    let error = reply.expect_err("a call without a caller row is refused");
    assert_eq!(error.code(), Some("no_caller_row"), "{error}");

    // Nothing was registered: the next adoption carries neither the fixture's
    // refused entry nor its own marker under another name.
    let session = server.agent().unwrap().get_session(&id).unwrap();
    submit(&mut client, &id, "after the refused registration").await;
    assert_ok(
        &client
            .rpc("session.idle", json!({"sessionId":id,"branchId":"main"}))
            .await,
    );
    let system = last_system(&session, "main").await;
    assert!(!system.contains("dim-guide-no-caller"), "{system:?}");

    server.shutdown();
    server.join().await.unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires DIM_TEST_NODE and DIM_TEST_JS_RUNTIME"]
async fn node_guide_entries_refusals_reach_the_row_as_codes() {
    let dir = scratch("guide-refusal");
    let register_file = dir.join("register");
    std::fs::write(&register_file, "on").unwrap();
    let (send, mut controls) = tokio::sync::mpsc::unbounded_channel();
    let (server, mut client, _requests) = launch(&register_file, send).await;
    let id = new_session(&mut client, json!(["echo"])).await;
    controls.recv().await.unwrap();
    when_served(
        &mut client,
        "session.guide_fixture.status",
        json!({"sessionId":id}),
    )
    .await;

    // The fixture's own marker is taken; a second entry for it is refused.
    let duplicate = when_served(
        &mut client,
        "session.guide_fixture.register",
        json!({"sessionId":id,"entry":{"markers":[MARKER],"order":55,"text":"<dim-guide-fixture>: a second entry."}}),
    )
    .await;
    assert_ok(&duplicate);
    assert_eq!(duplicate["result"]["ok"], false, "{duplicate}");
    assert_eq!(
        duplicate["result"]["code"], "duplicate_marker",
        "{duplicate}"
    );
    assert!(
        duplicate["result"]["message"]
            .as_str()
            .unwrap()
            .contains(MARKER),
        "{duplicate}"
    );

    // An entry over the table's limit is refused before anything is stored.
    let oversized = when_served(
        &mut client,
        "session.guide_fixture.register",
        json!({"sessionId":id,"entry":{"markers":["dim-guide-big"],"order":55,"text":"x".repeat(4097)}}),
    )
    .await;
    assert_ok(&oversized);
    assert_eq!(oversized["result"]["ok"], false, "{oversized}");
    assert_eq!(oversized["result"]["code"], "too_large", "{oversized}");

    server.shutdown();
    server.join().await.unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

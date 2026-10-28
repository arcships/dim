//! Goal (docs/design/plugins/Goal.md) as a TypeScript plugin, one row: its
//! Session part writes change inputs by branch id, and its branch part folds
//! the branch's events and decides continuation. Pursuit (Pursuit.md) adds the
//! evaluator judge: a completion request creates an evaluator branch whose
//! conclusion the working branch decides on.
use super::*;
use dim_agent_capabilities::{
    adapters::DefaultPrepareModelPlugin,
    history::HistoryRenderer,
    host::{
        CompactionStrategyHost, ContextModelHost, IdentityContext, ModelChunk, ModelConnection,
        ModelDriver, ModelHost, ModelStopReason, ModelStream, PrepareContextHost,
    },
};
use dim_agent_types::{BranchCreateData, BranchSource, MessageContent, ModelRequest};
use dim_app_server::session_typescript::SessionTypeScriptConfig;
use std::path::{Path, PathBuf};

fn goal_rows() -> SessionTypeScriptConfig {
    let mut config = session_typescript::fixture();
    config.rows = vec![session_typescript::manifest_row("goal")];
    config
}
fn product_home() -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let home = PathBuf::from("/tmp").join(format!("dg-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&home).unwrap();
    std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
    home
}
async fn select_model(client: &mut Client) {
    let models = [
        json!({"modelId":"fixture","displayName":"fixture","status":"active",
        "capabilities":dim_agent_types::ModelCapabilities::default()}),
    ];
    assert_ok(&client.rpc("provider.create", json!({"providerId":"018ec73f-0000-7000-8000-000000000005","displayName":"Fixture","connection":{"driverKind":"openai-compatible","baseUrl":"http://localhost/v1","auth":{"type":"anonymous"}},"models":models,"enabled":true})).await);
    assert_ok(
        &client
            .rpc(
                "provider.select",
                json!({"providerId":"018ec73f-0000-7000-8000-000000000005","modelId":"fixture"}),
            )
            .await,
    );
}
async fn set_limit(client: &mut Client, limit: u32) {
    let user = json!({"source":"goal","scope":"user"});
    let read = client.rpc("settings.read", user).await;
    assert_ok(&read);
    assert_ok(
        &client
            .rpc(
                "settings.update",
                json!({"source":"goal","scope":"user","expectedRevision":read["result"]["revision"],
                    "document":{"maxContinuations":limit}}),
            )
            .await,
    );
}
async fn new_session(client: &mut Client) -> String {
    let mut params = create(json!(["goal_update", "wait"]));
    params["permissionPolicy"]["allowedTools"] = json!(["goal_update", "wait"]);
    let created = client.rpc("session.create", params).await;
    assert_ok(&created);
    created["result"]["sessionId"].as_str().unwrap().to_owned()
}
async fn goal(client: &mut Client, method: &str, params: Value) -> Value {
    client.rpc(&format!("session.goal.{method}"), params).await
}
/// Goal's blocks on `branch`, in order: each input's first `<dim-goal>`
/// marker line, and the input's sequence.
async fn goal_writes(session: &dim_agent_runtime::Session, branch: &str) -> Vec<(u64, String)> {
    session
        .local(branch, ..)
        .await
        .unwrap()
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::InputMessage(data) => data.blocks.first().and_then(|block| {
                let dim_agent_types::InputBlock::Raw { text, .. } = block;
                text.starts_with("<dim-goal ")
                    .then(|| (event.sequence, text.lines().next().unwrap().to_owned()))
            }),
            _ => None,
        })
        .collect()
}
/// Waits until a Goal block on `branch` contains `needle`.
async fn wait_for(session: &dim_agent_runtime::Session, branch: &str, needle: &str) {
    let found = tokio::time::timeout(Duration::from_secs(15), async {
        while !goal_writes(session, branch)
            .await
            .iter()
            .any(|(_, line)| line.contains(needle))
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    assert!(
        found.is_ok(),
        "no Goal block with {needle}: {:#?}",
        session.local(branch, ..).await
    );
}
async fn idle(client: &mut Client, session: &str, branch: &str) {
    assert_ok(
        &client
            .rpc(
                "session.idle",
                json!({"sessionId":session,"branchId":branch}),
            )
            .await,
    );
}
/// The value a change block carries after its `label` line, decoded by the
/// block encoding of Goal A.5.
fn block_value(text: &str, label: &str) -> String {
    let start = text.find(&format!("\n{label}\n")).unwrap() + label.len() + 2;
    let end = text.rfind("\n\n").unwrap();
    text[start..end]
        .replace("&quot;", "\"")
        .replace("&gt;", ">")
        .replace("&lt;", "<")
        .replace("&amp;", "&")
}
/// The full text of the last Goal block on `branch` whose marker contains `needle`.
async fn goal_block(session: &dim_agent_runtime::Session, branch: &str, needle: &str) -> String {
    session
        .local(branch, ..)
        .await
        .unwrap()
        .iter()
        .rev()
        .find_map(|event| match &event.kind {
            EventKind::InputMessage(data) => data.blocks.first().and_then(|block| {
                let dim_agent_types::InputBlock::Raw { text, .. } = block;
                (text.starts_with("<dim-goal ") && text.lines().next().unwrap().contains(needle))
                    .then(|| text.clone())
            }),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no Goal block with {needle}"))
}
/// Text with `<`, `&`, quotes, an entity-like run and a blank line.
const SPECIAL: &str = "ship <it> & \"now\"\n\nthen &amp; done";

fn cleanup(home: &Path) {
    let _ = std::fs::remove_dir_all(home);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires DIM_TEST_NODE and DIM_TEST_JS_RUNTIME"]
async fn node_goal_continues_to_the_limit_and_stops_on_completion() {
    let home = product_home();
    let mut builder = AgentBuilder::new(AgentConfig::default());
    builder.timeline_store(Arc::new(dim_agent_runtime::MemoryTimelineStore::default()));
    let (server, agent) = launch_goal(&home, builder).await;
    let mut client = Client::new_version(&server, false, 3).await;
    select_model(&mut client).await;
    set_limit(&mut client, 2).await;

    // Limit (Appendix D 1, 2): set drives a turn; each normal end writes a
    // continuation until the third decision pauses with `limit`.
    let id = new_session(&mut client).await;
    let session = agent.get_session(&id).unwrap();
    let set = goal(
        &mut client,
        "set",
        json!({"sessionId":id,"objective":"ship <it>"}),
    )
    .await;
    assert_ok(&set);
    assert!(set["result"]["sequence"].is_u64(), "{set}");
    wait_for(&session, "main", r#"reason="limit""#).await;
    idle(&mut client, &id, "main").await;
    let writes: Vec<String> = goal_writes(&session, "main")
        .await
        .into_iter()
        .map(|(_, line)| line)
        .collect();
    assert_eq!(writes.len(), 4, "{writes:#?}");
    assert!(writes[0].contains(r#"op="set""#), "{writes:#?}");
    assert!(
        writes[1].contains(r#"kind="continuation" count="1" limit="2""#),
        "{writes:#?}"
    );
    assert!(
        writes[2].contains(r#"kind="continuation" count="2" limit="2""#),
        "{writes:#?}"
    );
    assert!(
        writes[3].contains(r#"op="pause" reason="limit""#),
        "{writes:#?}"
    );

    // A child branch cut after the goal starts with none: Goal retires the
    // inherited keys once (§3.4, Appendix D 7).
    let child = session
        .branch(
            "main".into(),
            session.sequence("main").unwrap(),
            dim_agent_runtime::BranchOptions {
                prompt: "child task".into(),
                source: dim_agent_types::BranchSource::Host {
                    operation_id: "goal-child".into(),
                },
            },
        )
        .await
        .unwrap();
    wait_for(&session, &child, r#"reason="new_branch""#).await;
    idle(&mut client, &id, &child).await;
    let child_writes = goal_writes(&session, &child).await;
    assert_eq!(child_writes.len(), 1, "{child_writes:#?}");
    assert_eq!(goal_writes(&session, "main").await.len(), 4);

    // Completion (Appendix D 1): the model's goal_update records `completed`
    // and no continuation follows it.
    set_limit(&mut client, 5).await;
    let id = new_session(&mut client).await;
    let session = agent.get_session(&id).unwrap();
    assert_ok(
        &goal(
            &mut client,
            "set",
            json!({"sessionId":id,"objective":"finish"}),
        )
        .await,
    );
    assert_ok(
        &client
            .rpc(
                "session.submit",
                json!({"sessionId":id,"branchId":"main",
                    "text":format!("tool:goal_update {}", json!({"status":"completed","summary":SPECIAL}))}),
            )
            .await,
    );
    wait_for(&session, "main", r#"op="completed""#).await;
    idle(&mut client, &id, "main").await;
    let completed_block = goal_block(&session, "main", r#"op="completed""#).await;
    assert!(!completed_block.contains("<it>"), "{completed_block}");
    assert_eq!(block_value(&completed_block, "Result:"), SPECIAL);
    let writes = goal_writes(&session, "main").await;
    let (completed, _) = writes
        .iter()
        .find(|(_, line)| line.contains(r#"op="completed""#))
        .unwrap()
        .clone();
    assert!(
        writes
            .iter()
            .all(|(sequence, line)| *sequence <= completed || !line.contains("continuation")),
        "{writes:#?}"
    );
    let results: Vec<String> = session
        .local("main", ..)
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

    for (params, error) in [
        (json!({"sessionId":id,"objective":""}), "invalid_params"),
        (
            json!({"sessionId":id,"branchId":"missing","objective":"x"}),
            "session_invalid_input",
        ),
    ] {
        assert_eq!(
            goal(&mut client, "set", params).await["error"]["data"]["appError"],
            error
        );
    }
    for method in ["pause", "resume", "clear"] {
        assert_ok(&goal(&mut client, method, json!({"sessionId":id})).await);
    }
    wait_for(&session, "main", r#"op="clear""#).await;
    let after: Vec<String> = goal_writes(&session, "main")
        .await
        .into_iter()
        .filter(|(sequence, _)| *sequence > completed)
        .map(|(_, line)| line)
        .collect();
    let at = |op: &str| {
        after
            .iter()
            .position(|line| line.contains(&format!(r#"op="{op}""#)))
            .unwrap_or_else(|| panic!("no {op}: {after:#?}"))
    };
    assert!(
        at("pause") < at("resume") && at("resume") < at("clear"),
        "{after:#?}"
    );
    server.shutdown();
    server.join().await.unwrap();
    cleanup(&home);
}

async fn launch_goal(
    home: &Path,
    builder: AgentBuilder,
) -> (RunningServer, dim_agent_runtime::Agent) {
    launch_goal_with(
        home,
        builder,
        Arc::new(dim_agent_capabilities::adapters::EchoModel),
    )
    .await
}

async fn launch_goal_with(
    home: &Path,
    mut builder: AgentBuilder,
    model: Arc<dyn ModelDriver>,
) -> (RunningServer, dim_agent_runtime::Agent) {
    defaults::install(&mut builder);
    builder
        .session_value(Arc::new(HistoryRenderer::default()))
        .process_value(Arc::new(ModelHost(model)))
        .session_value(Arc::new(PrepareContextHost(Arc::new(IdentityContext))))
        .session_value(Arc::new(ContextModelHost(None)))
        .session_value(Arc::new(CompactionStrategyHost(None)))
        .require_feature(
            Feature::new("PrepareModelHost").session("PrepareModelHost", |names| {
                Ok(Box::new(DefaultPrepareModelPlugin::new(names)?))
            }),
        );
    // Goal stays an ordinary optional row: its tool is adopted whenever it
    // becomes active (#241), with no dependency forcing it first (#223).
    dim_agent_capabilities::defaults::tool_host(&mut builder, Arc::new(MockTools));
    let server = serve(
        builder,
        ServerConfig {
            unix_path: Some(home.join("server.sock")),
            product_home: Some(home.to_owned()),
            target_home: Some(home.join("rust")),
            session_typescript: Some(goal_rows()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let agent = server.agent().unwrap();
    (server, agent)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires DIM_TEST_NODE and DIM_TEST_JS_RUNTIME"]
async fn node_goal_restore_refolds_without_repeating_decisions() {
    let home = product_home();
    let (server, agent) = launch_goal(&home, AgentBuilder::new(AgentConfig::default())).await;
    let mut client = Client::new_version(&server, false, 3).await;
    select_model(&mut client).await;
    set_limit(&mut client, 1).await;
    let id = new_session(&mut client).await;
    let session = agent.get_session(&id).unwrap();
    assert_ok(
        &goal(
            &mut client,
            "set",
            json!({"sessionId":id,"objective":SPECIAL}),
        )
        .await,
    );
    wait_for(&session, "main", r#"reason="limit""#).await;
    idle(&mut client, &id, "main").await;
    assert_eq!(
        block_value(
            &goal_block(&session, "main", r#"op="set""#).await,
            "Objective:"
        ),
        SPECIAL
    );
    let edited = format!("{SPECIAL} (edited)");
    assert_ok(
        &goal(
            &mut client,
            "edit",
            json!({"sessionId":id,"objective":edited}),
        )
        .await,
    );
    wait_for(&session, "main", r#"op="edit""#).await;
    idle(&mut client, &id, "main").await;
    let before = goal_writes(&session, "main").await;
    assert_eq!(before.len(), 4, "{before:#?}");
    drop(client);
    server.shutdown();
    server.join().await.unwrap();

    let (server, agent) = launch_goal(&home, AgentBuilder::new(AgentConfig::default())).await;
    let mut client = Client::new_version(&server, false, 3).await;
    assert_ok(
        &client
            .rpc(
                "session.submit",
                json!({"sessionId":id,"branchId":"main","text":"hello again"}),
            )
            .await,
    );
    idle(&mut client, &id, "main").await;
    let session = agent.get_session(&id).unwrap();
    assert_eq!(goal_writes(&session, "main").await, before);
    assert_eq!(
        block_value(
            &goal_block(&session, "main", r#"op="edit""#).await,
            "Objective:"
        ),
        edited
    );
    assert_ok(&goal(&mut client, "resume", json!({"sessionId":id})).await);
    wait_for(&session, "main", r#"kind="continuation" count="1""#).await;
    server.shutdown();
    server.join().await.unwrap();
    cleanup(&home);
}

/// Waits until a local event on `branch` matches. A branch that is still
/// loading has no published events yet.
async fn wait_event(
    session: &dim_agent_runtime::Session,
    branch: &str,
    matches: impl Fn(&EventKind) -> bool,
) {
    for _ in 0..750 {
        let Ok(events) = session.local(branch, ..).await else {
            tokio::time::sleep(Duration::from_millis(20)).await;
            continue;
        };
        if events.iter().any(|event| matches(&event.kind)) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let events = session.local(branch, ..).await.unwrap_or_default();
    panic!(
        "event did not arrive: {:#?}",
        events
            .iter()
            .map(|event| serde_json::to_string(&event.kind).unwrap())
            .collect::<Vec<_>>()
    );
}
/// Sequences of the `turn/end` events on `branch`.
async fn turn_ends(session: &dim_agent_runtime::Session, branch: &str) -> Vec<u64> {
    session
        .local(branch, ..)
        .await
        .unwrap()
        .iter()
        .filter(|event| matches!(event.kind, EventKind::TurnEnd(_)))
        .map(|event| event.sequence)
        .collect()
}
async fn submit(client: &mut Client, session: &str, text: &str) {
    assert_ok(
        &client
            .rpc(
                "session.submit",
                json!({"sessionId":session,"branchId":"main","text":text}),
            )
            .await,
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires DIM_TEST_NODE and DIM_TEST_JS_RUNTIME"]
async fn node_goal_pending_input_and_interrupts() {
    let home = product_home();
    let mut builder = AgentBuilder::new(AgentConfig::default());
    builder.timeline_store(Arc::new(dim_agent_runtime::MemoryTimelineStore::default()));
    let (server, agent) = launch_goal(&home, builder).await;
    let mut client = Client::new_version(&server, false, 3).await;
    select_model(&mut client).await;
    set_limit(&mut client, 1).await;

    // Appendix D 4: the goal is set while a turn runs, and the operator
    // queues more input. The turn's end finds Queue input pending, so it
    // decides nothing; the turn that consumes them decides.
    let id = new_session(&mut client).await;
    let session = agent.get_session(&id).unwrap();
    submit(&mut client, &id, r#"tool:wait {"millis":1500}"#).await;
    wait_event(&session, "main", |kind| {
        matches!(kind, EventKind::ToolCall(_))
    })
    .await;
    assert_ok(
        &goal(
            &mut client,
            "set",
            json!({"sessionId":id,"objective":"ship"}),
        )
        .await,
    );
    submit(&mut client, &id, "and the changelog").await;
    wait_for(&session, "main", r#"reason="limit""#).await;
    idle(&mut client, &id, "main").await;
    let ends = turn_ends(&session, "main").await;
    let writes = goal_writes(&session, "main").await;
    let decided: Vec<&String> = writes
        .iter()
        .map(|(_, line)| line)
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

    // Appendix D 3: an interrupt after the goal starts pauses it, and no
    // continuation follows.
    set_limit(&mut client, 5).await;
    let id = new_session(&mut client).await;
    let session = agent.get_session(&id).unwrap();
    submit(&mut client, &id, r#"tool:wait {"millis":1500}"#).await;
    wait_event(&session, "main", |kind| {
        matches!(kind, EventKind::ToolCall(_))
    })
    .await;
    assert_ok(
        &goal(
            &mut client,
            "set",
            json!({"sessionId":id,"objective":"ship"}),
        )
        .await,
    );
    wait_for(&session, "main", r#"op="set""#).await;
    assert_ok(
        &client
            .rpc("session.stop", json!({"sessionId":id,"branchId":"main"}))
            .await,
    );
    wait_for(&session, "main", r#"reason="interrupted""#).await;
    idle(&mut client, &id, "main").await;
    let writes = goal_writes(&session, "main").await;
    assert!(
        writes
            .iter()
            .all(|(_, line)| !line.contains("continuation")),
        "{writes:#?}"
    );
    server.shutdown();
    server.join().await.unwrap();
    cleanup(&home);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires DIM_TEST_NODE and DIM_TEST_JS_RUNTIME"]
async fn node_goal_settings_resume_unload_and_fork() {
    let home = product_home();
    let mut builder = AgentBuilder::new(AgentConfig::default());
    builder.timeline_store(Arc::new(dim_agent_runtime::MemoryTimelineStore::default()));
    let (server, agent) = launch_goal(&home, builder).await;
    let mut client = Client::new_version(&server, false, 3).await;
    select_model(&mut client).await;
    set_limit(&mut client, 1).await;
    let id = new_session(&mut client).await;
    let session = agent.get_session(&id).unwrap();
    assert_ok(
        &goal(
            &mut client,
            "set",
            json!({"sessionId":id,"objective":"ship"}),
        )
        .await,
    );
    wait_for(&session, "main", r#"reason="limit""#).await;
    idle(&mut client, &id, "main").await;

    // Appendix D 11: completed, then resume reopens the goal with the count
    // restarted. Appendix D 8: read the committed limit at the next decision,
    // without reloading Goal or waiting for a settings.changed callback.
    submit(
        &mut client,
        &id,
        &format!(
            "tool:goal_update {}",
            json!({"status":"completed","summary":"done"})
        ),
    )
    .await;
    wait_for(&session, "main", r#"op="completed""#).await;
    idle(&mut client, &id, "main").await;
    set_limit(&mut client, 3).await;
    assert_ok(&goal(&mut client, "resume", json!({"sessionId":id})).await);
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let writes = goal_writes(&session, "main").await;
            let limits = writes
                .iter()
                .filter(|(_, line)| line.contains(r#"reason="limit""#))
                .count();
            if limits == 2 {
                assert!(
                    writes
                        .iter()
                        .any(|(_, line)| line.contains(r#"count="3" limit="3""#)),
                    "{writes:#?}"
                );
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the resumed goal pauses at the new limit");
    idle(&mut client, &id, "main").await;

    // Appendix D 8: an unloaded Session is restored by a goal method, which
    // writes.
    assert!(session.unload_if_idle().await.unwrap());
    drop(session);
    assert_ok(&goal(&mut client, "pause", json!({"sessionId":id})).await);
    let session = agent.get_session(&id).unwrap();
    wait_for(&session, "main", r#"op="pause" reason="user""#).await;
    idle(&mut client, &id, "main").await;

    // Appendix D 7: a fork writes `session/forked` after the copied facts.
    // The forked `main`'s goal starts empty: one start `clear` after that
    // fact, and no continuation.
    let fork = client
        .rpc(
            "session.fork",
            json!({"sessionId":id,"at":session.sequence("main").unwrap()}),
        )
        .await;
    assert_ok(&fork);
    let fork_id = fork["result"]["sessionId"].as_str().unwrap().to_owned();
    let forked = agent.get_session(&fork_id).unwrap();
    wait_for(&forked, "main", r#"reason="new_branch""#).await;
    idle(&mut client, &fork_id, "main").await;
    let events = forked.local("main", ..).await.unwrap();
    let opening = events
        .iter()
        .find(|event| matches!(&event.kind, EventKind::SessionForked(_)))
        .expect("the fork's fact")
        .sequence;
    let writes = goal_writes(&forked, "main").await;
    let own: Vec<&String> = writes
        .iter()
        .filter(|(sequence, _)| *sequence > opening)
        .map(|(_, line)| line)
        .collect();
    assert_eq!(own.len(), 1, "{writes:#?}");
    assert!(
        own[0].contains(&format!(r#"reason="new_branch" after="{opening}""#)),
        "{own:#?}"
    );
    assert!(
        goal_writes(&session, "main")
            .await
            .iter()
            .all(|(_, line)| !line.contains("new_branch"))
    );

    server.shutdown();
    server.join().await.unwrap();

    cleanup(&home);

    // Appendix D 8: an assembly without Goal has no goal methods.
    let home = product_home();
    let server = launch(
        Arc::new(MockTools),
        ServerConfig {
            product_home: Some(home.clone()),
            target_home: Some(home.join("rust")),
            ..Default::default()
        },
    )
    .await;
    let mut client = Client::new_version(&server, false, 3).await;
    select_model(&mut client).await;
    let created = client.rpc("session.create", create(json!([]))).await;
    assert_ok(&created);
    let id = created["result"]["sessionId"].as_str().unwrap().to_owned();
    assert_eq!(
        goal(&mut client, "set", json!({"sessionId":id,"objective":"x"})).await["error"]["data"]["appError"],
        "unknown_method"
    );
    server.shutdown();
    server.join().await.unwrap();
    cleanup(&home);
}

/// Whether the request is the evaluator's: its prompt's first line is Goal's
/// evaluate marker (Pursuit §5).
fn is_evaluator(request: &ModelRequest) -> bool {
    request.messages.iter().any(|message| {
        matches!(&message.content, MessageContent::Text(text)
            if text.contains("<dim-goal kind=\"evaluate\""))
    })
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
            return dim_agent_capabilities::adapters::EchoModel.invoke(request, connection, cancel);
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
            return dim_agent_capabilities::adapters::EchoModel.invoke(request, connection, cancel);
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

/// The `branch/create` events on `branch` in order, with their sequences.
async fn branch_creates(
    session: &dim_agent_runtime::Session,
    branch: &str,
) -> Vec<(u64, BranchCreateData)> {
    session
        .local(branch, ..)
        .await
        .unwrap()
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::BranchCreate(data) => Some((event.sequence, data.clone())),
            _ => None,
        })
        .collect()
}

/// The user text of the first input on `branch`. A branch that is still being
/// built has no published events yet.
async fn first_user_text(session: &dim_agent_runtime::Session, branch: &str) -> String {
    for _ in 0..750 {
        let Ok(events) = session.local(branch, ..).await else {
            tokio::time::sleep(Duration::from_millis(20)).await;
            continue;
        };
        for event in events {
            if let EventKind::InputMessage(data) = &event.kind
                && let Some(user) = &data.user
                && let Some(dim_agent_types::ContentBlock::Text { text }) = user.content.first()
            {
                return text.clone();
            }
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("no user input on {branch}")
}

/// Submits the worker's `goal_update`.
async fn submit_report(client: &mut Client, session: &str, status: &str, summary: &str) {
    assert_ok(
        &client
            .rpc(
                "session.submit",
                json!({"sessionId":session,"branchId":"main",
                    "text":format!("tool:goal_update {}", json!({"status":status,"summary":summary}))}),
            )
            .await,
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires DIM_TEST_NODE and DIM_TEST_JS_RUNTIME"]
async fn node_goal_evaluator_judge_creates_an_evaluator_and_completes_on_done() {
    let home = product_home();
    let (server, agent) = launch_goal_with(
        &home,
        AgentBuilder::new(AgentConfig::default()),
        Arc::new(EvaluatingModel),
    )
    .await;
    let mut client = Client::new_version(&server, false, 3).await;
    select_model(&mut client).await;
    set_limit(&mut client, 5).await;
    let id = new_session(&mut client).await;
    let session = agent.get_session(&id).unwrap();
    assert_ok(
        &goal(
            &mut client,
            "set",
            json!({"sessionId":id,"objective":"make the tests pass","judge":"evaluator"}),
        )
        .await,
    );
    submit_report(&mut client, &id, "completed", "all tests pass").await;
    wait_event(&session, "main", |kind| {
        matches!(kind, EventKind::BranchCreate(_))
    })
    .await;
    idle(&mut client, &id, "main").await;
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
    let (request, _) = writes
        .iter()
        .find(|(_, line)| line.contains(r#"op="completed""#) && !line.contains("by="))
        .unwrap_or_else(|| panic!("the worker's request: {writes:#?}"));
    assert!(
        *request < *created,
        "the request precedes the creation: {writes:#?}"
    );
    assert!(
        writes
            .iter()
            .all(|(_, line)| !line.contains("continuation")),
        "{writes:#?}"
    );
    let child = &creation.branch;
    let prompt = first_user_text(&session, child).await;
    let marker = format!(r#"<dim-goal kind="evaluate" branch="main" evaluation="{evaluation}">"#);
    assert!(prompt.starts_with(&marker), "{prompt}");
    assert!(
        prompt.contains("Objective:\nmake the tests pass"),
        "{prompt}"
    );
    assert!(
        prompt.contains("Reported result:\nall tests pass"),
        "{prompt}"
    );
    wait_for(&session, "main", r#"by="evaluator""#).await;
    idle(&mut client, &id, "main").await;
    let done = goal_block(&session, "main", r#"by="evaluator""#).await;
    assert!(done.contains(r#"op="completed""#), "{done}");
    assert_eq!(block_value(&done, "Result:"), "all tests pass");
    // The conclusion it decides on is one input of one block, and that
    // block's marker is the report of this evaluation and evaluator
    // (Pursuit §5).
    let (blocks, marker) = session
        .local("main", ..)
        .await
        .unwrap()
        .iter()
        .find_map(|event| match &event.kind {
            EventKind::InputMessage(data) => data.blocks.first().and_then(|block| {
                let dim_agent_types::InputBlock::Raw { text, .. } = block;
                text.starts_with(r#"<dim-goal kind="report""#)
                    .then(|| (data.blocks.len(), text.lines().next().unwrap().to_owned()))
            }),
            _ => None,
        })
        .expect("the evaluator's conclusion input");
    assert_eq!(blocks, 1, "the conclusion input carries the report alone");
    assert_eq!(
        marker,
        format!(
            r#"<dim-goal kind="report" result="done" evaluation="{evaluation}" evaluator="{child}" branch="main">"#
        ),
        "the conclusion marker"
    );
    assert_eq!(
        turn_ends(&session, child).await.len(),
        1,
        "one evaluator turn"
    );
    let writes = goal_writes(&session, "main").await;
    assert!(
        writes
            .iter()
            .filter(|(_, line)| line.contains(r#"by="evaluator""#))
            .count()
            == 1,
        "{writes:#?}"
    );
    server.shutdown();
    server.join().await.unwrap();
    cleanup(&home);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires DIM_TEST_NODE and DIM_TEST_JS_RUNTIME"]
async fn node_goal_evaluator_restart_writes_no_second_result() {
    let home = product_home();
    let (server, agent) = launch_goal_with(
        &home,
        AgentBuilder::new(AgentConfig::default()),
        Arc::new(WaitingModel),
    )
    .await;
    let mut client = Client::new_version(&server, false, 3).await;
    select_model(&mut client).await;
    set_limit(&mut client, 5).await;
    let id = new_session(&mut client).await;
    let session = agent.get_session(&id).unwrap();
    assert_ok(
        &goal(
            &mut client,
            "set",
            json!({"sessionId":id,"objective":"ship it","judge":"evaluator"}),
        )
        .await,
    );
    submit_report(&mut client, &id, "completed", "done").await;
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
    drop(client);
    server.shutdown();
    server.join().await.unwrap();

    let (server, agent) = launch_goal_with(
        &home,
        AgentBuilder::new(AgentConfig::default()),
        Arc::new(EvaluatingModel),
    )
    .await;
    let mut client = Client::new_version(&server, false, 3).await;
    submit(&mut client, &id, "back again").await;
    idle(&mut client, &id, "main").await;
    let session = agent.get_session(&id).unwrap();
    assert_eq!(branch_creates(&session, "main").await.len(), 1);
    let writes = goal_writes(&session, "main").await;
    assert!(
        writes
            .iter()
            .all(|(_, line)| !line.contains(r#"by="evaluator""#)),
        "a replayed turn end reports nothing: {writes:#?}"
    );
    assert!(
        writes
            .iter()
            .all(|(_, line)| !line.contains(r#"reason="evaluation_failed""#)),
        "{writes:#?}"
    );
    assert_ok(
        &client
            .rpc(
                "session.submit",
                json!({"sessionId":id,"branchId":child,"text":"carry on"}),
            )
            .await,
    );
    wait_for(&session, "main", r#"by="evaluator""#).await;
    idle(&mut client, &id, "main").await;
    let writes = goal_writes(&session, "main").await;
    assert!(
        writes
            .iter()
            .filter(|(_, line)| line.contains(r#"by="evaluator""#))
            .count()
            == 1,
        "{writes:#?}"
    );
    server.shutdown();
    server.join().await.unwrap();
    cleanup(&home);
}

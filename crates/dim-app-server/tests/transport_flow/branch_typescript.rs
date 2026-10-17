use super::*;
use dim_agent_capabilities::{
    adapters::{DefaultPrepareModelPlugin, EchoModel},
    history::HistoryRenderer,
    host::{
        CompactionStrategyHost, ContextModelHost, IdentityContext, ModelConnection, ModelDriver,
        ModelHost, ModelStream, PrepareContextHost,
    },
};
use dim_agent_runtime::BranchScope;
use dim_agent_types::{MessageContent, MessagePart, ModelRequest, Role};
use dim_app_server::session_typescript::SessionTypeScriptConfig;
use std::path::PathBuf;

/// Records the text of each request's last user message, then echoes it.
struct Recorded(tokio::sync::mpsc::UnboundedSender<String>);
impl ModelDriver for Recorded {
    fn invoke(
        &self,
        request: Arc<ModelRequest>,
        connection: ModelConnection,
        cancel: CancellationToken,
    ) -> dim_agent_capabilities::host::HostFuture<'_, ModelStream> {
        let text = request
            .messages
            .iter()
            .rev()
            .find(|message| message.role == Role::User)
            .map(|message| match &message.content {
                MessageContent::Text(text) => text.clone(),
                MessageContent::Parts(parts) => parts
                    .iter()
                    .filter_map(|part| match part {
                        MessagePart::Text { text, .. } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect(),
            })
            .unwrap_or_default();
        let _ = self.0.send(text);
        EchoModel.invoke(request, connection, cancel)
    }
}

/// Hands the test each branch scope and the loader, to restart one row.
struct Controls(
    BranchScope,
    tokio::sync::mpsc::UnboundedSender<(BranchScope, Arc<rutis_loader::Loader>)>,
);
impl TypedPlugin for Controls {
    type Deps = (Arc<rutis_loader::Loader>,);
    fn name(&self) -> &str {
        "test.branch-controls"
    }
    fn apply<'a>(
        &'a self,
        _: &'a rutis::Ctx,
        (loader,): Self::Deps,
    ) -> rutis::BoxFuture<'a, Result<rutis::Effect, rutis::CordisError>> {
        Box::pin(async move {
            let _ = self.1.send((self.0, loader));
            Ok(rutis::Effect::Done)
        })
    }
}
impl Capability for Controls {
    fn keys(&self) -> KeysOf<Self> {
        ((),)
    }
}

fn parts(lifecycle: &std::path::Path, fail_on: Option<&str>) -> SessionTypeScriptConfig {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let fixtures = root.join("runtime/fixtures");
    SessionTypeScriptConfig {
        node: std::env::var_os("DIM_TEST_NODE")
            .expect("DIM_TEST_NODE")
            .into(),
        js_runtime: std::env::var_os("DIM_TEST_JS_RUNTIME")
            .expect("DIM_TEST_JS_RUNTIME")
            .into(),
        anchor: root.join("runtime/package.json"),
        rows: vec![
            json!({
                "id": "echo", "name": fixtures.join("branch-part.ts"),
                "provides": serde_json::from_str::<Value>(include_str!("../../../../runtime/fixtures/branch-part.json")).unwrap(),
                "config": {"lifecycleFile": lifecycle, "failOn": fail_on},
            }),
            json!({
                "id": "tree", "name": fixtures.join("branch-tree.ts"),
                "provides": serde_json::from_str::<Value>(include_str!("../../../../runtime/fixtures/branch-tree.json")).unwrap(),
                "config": {},
            }),
        ],
    }
}

async fn start(
    parts: SessionTypeScriptConfig,
) -> (
    RunningServer,
    tokio::sync::mpsc::UnboundedReceiver<String>,
    tokio::sync::mpsc::UnboundedReceiver<(BranchScope, Arc<rutis_loader::Loader>)>,
) {
    let (send, requests) = tokio::sync::mpsc::unbounded_channel();
    let (control, controls) = tokio::sync::mpsc::unbounded_channel();
    let mut builder = AgentBuilder::new(AgentConfig::default());
    defaults::install(&mut builder);
    builder
        .session_value(Arc::new(HistoryRenderer::default()))
        .session_value(Arc::new(ModelHost(Arc::new(Recorded(send)))))
        .session_value(Arc::new(PrepareContextHost(Arc::new(IdentityContext))))
        .session_value(Arc::new(ContextModelHost(None)))
        .session_value(Arc::new(CompactionStrategyHost(None)))
        .session_plugin(DefaultPrepareModelPlugin::new)
        .timeline_plugin(move |scope| Controls(*scope, control.clone()));
    dim_agent_capabilities::defaults::tool_host(
        &mut builder,
        Arc::new(dim_agent_capabilities::adapters::MockTools),
    );
    let server = serve(
        builder,
        ServerConfig {
            unix_path: Some(std::env::temp_dir().join(format!(
                "dm-c5-{}/server.sock",
                &uuid::Uuid::new_v4().simple().to_string()[..8]
            ))),
            session_typescript: Some(parts),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    (server, requests, controls)
}

async fn tree(client: &mut Client, session: &str, method: &str, mut params: Value) -> Value {
    params["sessionId"] = json!(session);
    let reply = client.rpc(&format!("session.tree.{method}"), params).await;
    assert_ok(&reply);
    reply["result"].clone()
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

/// The `c5:reply` blocks committed on one branch, in order.
async fn replies(server: &RunningServer, session: &str, branch: &str) -> Vec<(u64, String)> {
    let session = server.agent().unwrap().get_session(session).unwrap();
    session
        .history(branch, None)
        .await
        .unwrap()
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::InputMessage(input) => input.blocks.iter().find_map(|block| match block {
                dim_agent_types::InputBlock::Raw {
                    text,
                    key: Some(key),
                    ..
                } if key == "c5:reply" => Some((event.sequence, text.clone())),
                _ => None,
            }),
            _ => None,
        })
        .collect()
}

/// branch-typescript.md, Verification: a real Node branch part consumes its
/// branch's events in order and writes input from owned work; a Session part
/// writes through TreeCommands and reads the owner's consumed fold; branches
/// and Sessions stay independent; a row reload reconstructs without repeating
/// its side effect.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires DIM_TEST_NODE and DIM_TEST_JS_RUNTIME"]
async fn node_branch_typescript_parts_consume_write_and_serve_their_fold() {
    let home = std::env::temp_dir().join(format!(
        "dm-c5-{}",
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    ));
    std::fs::create_dir_all(&home).unwrap();
    let lifecycle = home.join("branch-parts.jsonl");
    let (server, mut requests, mut controls) = start(parts(&lifecycle, None)).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let created = client.rpc("session.create", create(json!([]))).await;
    assert_ok(&created);
    let first = created["result"]["sessionId"].as_str().unwrap().to_owned();
    let (main_scope, loader) = controls.recv().await.unwrap();

    // The part replies to the turn its "ask" input drove, from owned work;
    // the reply input reaches the model like any other input.
    let asked = tree(
        &mut client,
        &first,
        "write",
        json!({"branchId":"main","text":"ask one"}),
    )
    .await;
    let asked = asked["sequence"].as_u64().unwrap();
    assert!(asked > 0);
    assert_eq!(requests.recv().await.unwrap(), "ask one");
    let reply_text = requests.recv().await.unwrap();
    assert!(reply_text.starts_with("reply after:"), "{reply_text}");
    idle(&mut client, &first, "main").await;
    let written = replies(&server, &first, "main").await;
    assert_eq!(written.len(), 1, "{written:?}");
    let (reply_sequence, marker) = written[0].clone();
    assert!(reply_sequence > asked);

    // The Session part reads the owner's fold, not one recomputed from
    // history: `through` is the part's own consumed cutoff.
    let read = tree(
        &mut client,
        &first,
        "read",
        json!({"branchId":"main","name":"echoState"}),
    )
    .await;
    assert_eq!(read["methods"], json!(["get"]));
    let state = read["state"].clone();
    assert_eq!(
        state["identity"],
        json!({"sessionId":first,"branchId":"main"})
    );
    assert_eq!(state["replies"], json!([marker]));
    assert!(
        state["consumed"]
            .as_array()
            .unwrap()
            .contains(&json!(asked))
    );
    assert!(
        state["consumed"]
            .as_array()
            .unwrap()
            .contains(&json!(reply_sequence))
    );
    assert!(state["through"].as_u64().unwrap() >= reply_sequence);
    // Two public services of one part coexist under their own names.
    let notes = tree(
        &mut client,
        &first,
        "read",
        json!({"branchId":"main","name":"echoNotes"}),
    )
    .await;
    assert_eq!(notes["state"], json!({"notes":"main"}));
    let missing = tree(
        &mut client,
        &first,
        "read",
        json!({"branchId":"main","name":"missing"}),
    )
    .await;
    assert_eq!(missing["error"]["code"], "reloading", "{missing}");
    let unknown = tree(
        &mut client,
        &first,
        "read",
        json!({"branchId":"nowhere","name":"echoState"}),
    )
    .await;
    assert_eq!(unknown["error"]["code"], "unknown_branch", "{unknown}");
    let rejected = tree(&mut client, &first, "write", json!({"branchId":"main"})).await;
    assert_eq!(rejected["error"]["code"], "invalid_params", "{rejected}");

    // A second branch has its own part instance, subscription and fold.
    let child = tree(
        &mut client,
        &first,
        "branch",
        json!({"parent":"main","at":reply_sequence}),
    )
    .await;
    let child = child["id"].as_str().unwrap().to_owned();
    let (child_scope, _) = controls.recv().await.unwrap();
    assert_ne!(child_scope.instance, main_scope.instance);
    idle(&mut client, &first, &child).await;
    let child_state = tree(
        &mut client,
        &first,
        "read",
        json!({"branchId":child,"name":"echoState"}),
    )
    .await;
    assert_eq!(child_state["state"]["identity"]["branchId"], json!(child));
    assert_ne!(child_state["state"]["generation"], state["generation"]);
    assert_eq!(child_state["state"]["replies"], json!([]));
    let listed = tree(&mut client, &first, "list", json!({})).await;
    assert_eq!(listed["value"].as_array().unwrap().len(), 2, "{listed}");

    // Another Session shares the runtime but nothing else.
    let created = client.rpc("session.create", create(json!([]))).await;
    assert_ok(&created);
    let second = created["result"]["sessionId"].as_str().unwrap().to_owned();
    let other = tree(
        &mut client,
        &second,
        "read",
        json!({"branchId":"main","name":"echoState"}),
    )
    .await;
    assert_eq!(other["state"]["identity"]["sessionId"], json!(second));
    assert_eq!(other["state"]["replies"], json!([]));
    assert_ne!(other["state"]["generation"], state["generation"]);

    // A row reload ends the old generation's work and subscription; the new
    // one reconstructs the same fold from the Timeline and repeats no reply.
    let view = loader
        .entries()
        .into_iter()
        .find(|entry| {
            entry.id == "echo"
                && entry.instance.as_ref().is_some_and(|instance| {
                    instance
                        .chain
                        .iter()
                        .any(|(_, id)| *id == main_scope.instance)
                })
        })
        .unwrap()
        .view
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), view.restart())
        .await
        .unwrap()
        .unwrap();
    let mut watch = view.watch();
    tokio::time::timeout(Duration::from_secs(5), async {
        while watch.borrow().state != rutis::FiberState::Active {
            watch.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    let reloaded = tree(
        &mut client,
        &first,
        "read",
        json!({"branchId":"main","name":"echoState"}),
    )
    .await;
    assert_ne!(reloaded["state"]["generation"], state["generation"]);
    assert_eq!(reloaded["state"]["replies"], state["replies"]);
    assert_eq!(reloaded["state"]["consumed"], state["consumed"]);
    let ended: Vec<Value> = std::fs::read_to_string(&lifecycle)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(
        ended
            .iter()
            .any(|row| row["generation"] == state["generation"] && row["pending"] == 0),
        "{ended:?}"
    );
    // Only the new subscription consumes: one more ask, one more reply.
    tree(
        &mut client,
        &first,
        "write",
        json!({"branchId":"main","text":"ask two"}),
    )
    .await;
    idle(&mut client, &first, "main").await;
    let mut written = replies(&server, &first, "main").await;
    for _ in 0..50 {
        if written.len() == 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
        idle(&mut client, &first, "main").await;
        written = replies(&server, &first, "main").await;
    }
    assert_eq!(written.len(), 2, "{written:?}");
    // The child's history holds only the inherited reply of its prefix.
    assert_eq!(replies(&server, &first, &child).await, written[..1]);

    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// Failures keep their native paths: a throwing handler fails the
/// Dispatcher instead of continuing as healthy processing, a failed bind
/// fails only its row and publishes no service, and invalid declarations
/// fail assembly.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires DIM_TEST_NODE and DIM_TEST_JS_RUNTIME"]
async fn node_branch_typescript_failures_follow_native_paths() {
    let home = std::env::temp_dir().join(format!(
        "dm-c5-{}",
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    ));
    std::fs::create_dir_all(&home).unwrap();
    let lifecycle = home.join("branch-parts.jsonl");
    let (server, _requests, _controls) = start(parts(&lifecycle, Some("boom"))).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let created = client.rpc("session.create", create(json!([]))).await;
    assert_ok(&created);
    let session = created["result"]["sessionId"].as_str().unwrap().to_owned();
    tree(
        &mut client,
        &session,
        "write",
        json!({"branchId":"main","text":"boom"}),
    )
    .await;
    let failed = client
        .rpc(
            "session.idle",
            json!({"sessionId":session,"branchId":"main"}),
        )
        .await;
    assert!(failed.get("error").is_some(), "{failed}");
    server.shutdown();
    let joined = server.join().await;
    assert!(
        joined
            .as_ref()
            .is_err_and(|error| error.to_string().contains("fixture handler failure")),
        "{joined:?}"
    );

    let mut failing = parts(&lifecycle, None);
    failing.rows[0]["config"]["failBind"] = json!(true);
    let (server, _requests, _controls) = start(failing).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let created = client.rpc("session.create", create(json!([]))).await;
    assert_ok(&created);
    let session = created["result"]["sessionId"].as_str().unwrap().to_owned();
    let read = tree(
        &mut client,
        &session,
        "read",
        json!({"branchId":"main","name":"echoState"}),
    )
    .await;
    assert_eq!(read["error"]["code"], "reloading", "{read}");
    // The native branch keeps working.
    tree(
        &mut client,
        &session,
        "write",
        json!({"branchId":"main","text":"still native"}),
    )
    .await;
    idle(&mut client, &session, "main").await;
    server.shutdown();
    server.join().await.unwrap();

    for provides in [
        json!({"branchPart:echo":{"bind":"async"},"toolContribution:mixed":{"declaration":"sync","execute":"async"}}),
        json!({"branchPart:echo":{"bind":"sync"}}),
        json!({"branchPart:echo":{"bind":"async"},"branchPart:other":{"bind":"async"}}),
        json!({"branchPart:echo":{"bind":"async"},"settings":{"get":"sync"}}),
        json!({"branchPart:echo":{"bind":"async"},"echoState":{"get":"later"}}),
    ] {
        let mut invalid = parts(&lifecycle, None);
        invalid.rows[0]["provides"] = provides.clone();
        let mut config = ServerConfig {
            unix_path: Some(std::env::temp_dir().join(format!(
                "dm-c5-{}/server.sock",
                &uuid::Uuid::new_v4().simple().to_string()[..8]
            ))),
            session_typescript: Some(invalid),
            ..Default::default()
        };
        config.startup_timeout = Duration::from_secs(5);
        let mut builder = AgentBuilder::new(AgentConfig::default());
        defaults::install(&mut builder);
        let rejected = serve(builder, config).await;
        assert!(
            matches!(rejected, Err(dim_app_server::ServerError::Configuration(_))),
            "{provides} must fail assembly"
        );
    }
    std::fs::remove_dir_all(home).unwrap();
}

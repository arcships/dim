use super::deletion::Store;
use super::*;

fn create_with_branch() -> Value {
    json!({
        "cwd": std::env::temp_dir(),
        "tools": ["branch"],
        "permissionPolicy": {"allowedTools": ["branch"]}
    })
}

/// The last rest point of `branch`, as a client reads it.
async fn through(client: &mut Client, session: &str, branch: &str) -> u64 {
    let page = client
        .rpc(
            "session.history",
            json!({"sessionId":session,"branchId":branch,"after":0,"limit":256}),
        )
        .await;
    assert_ok(&page);
    page["result"]["through"].as_u64().unwrap()
}

/// Opens a child of `parent` at its last rest point.
async fn open_line(client: &mut Client, session: &str, parent: &str, prompt: &str) -> String {
    let at = through(client, session, parent).await;
    let created = client
        .rpc(
            "session.branch_create",
            json!({"sessionId":session,"parentBranchId":parent,"at":at,"prompt":prompt}),
        )
        .await;
    assert_ok(&created);
    created["result"]["branch"]["branchId"]
        .as_str()
        .unwrap()
        .to_owned()
}

async fn directory(client: &mut Client, session: &str) -> Vec<Value> {
    let reply = client
        .rpc("session.branches", json!({"sessionId":session}))
        .await;
    assert_ok(&reply);
    reply["result"]["branches"].as_array().unwrap().clone()
}

fn row<'a>(rows: &'a [Value], id: &str) -> &'a Value {
    rows.iter()
        .find(|row| row["branchId"] == id)
        .unwrap_or_else(|| panic!("{id} is missing: {rows:?}"))
}

/// The sequence of the line's own `branch/end`, read through its history.
async fn own_end(client: &mut Client, session: &str, branch: &str) -> u64 {
    let line = client
        .rpc(
            "session.history",
            json!({"sessionId":session,"branchId":branch,"after":0,"limit":256}),
        )
        .await;
    assert_ok(&line);
    line["result"]["events"]
        .as_array()
        .unwrap()
        .iter()
        .rev()
        .find(|event| event["type"] == "branch" && event["name"] == "end")
        .unwrap_or_else(|| panic!("{branch} holds its own end: {line}"))["sequence"]
        .as_u64()
        .unwrap()
}

/// Waits until the branch's published head reaches `at_least`.
async fn published_head(session: &dim_agent_runtime::Session, branch: &str, at_least: u64) {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while session.sequence(branch).unwrap() < at_least {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the parent publishes what the tree recorded");
}

/// #1165: the directory lists main, its child and grandchild; a cascade end
/// records each ended line with the sequence of its own `branch/end`, and
/// `session.history` reads an ended line up to the head the directory gives.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn branches_list_main_and_its_lines_and_read_an_ended_line_through_its_head() {
    let store = Store::new();
    let (server, agent) = launch_with_agent(Arc::new(MockTools), store.config()).await;
    let mut client = Client::new_version(&server, false, 3).await;
    assert!(
        client
            .features
            .as_array()
            .unwrap()
            .contains(&json!("session.branches")),
        "the feature is advertised: {:?}",
        client.features
    );
    let created = client.rpc("session.create", create_with_branch()).await;
    assert_ok(&created);
    let id = created["result"]["sessionId"].as_str().unwrap().to_owned();
    let session = agent.get_session(&id).unwrap();
    session.idle("main").await.unwrap();
    let child = open_line(&mut client, &id, "main", "child work").await;
    session.idle(&child).await.unwrap();
    let grandchild = open_line(&mut client, &id, &child, "grand work").await;
    session.idle(&grandchild).await.unwrap();

    let rows = directory(&mut client, &id).await;
    assert_eq!(rows.len(), 3, "{rows:?}");
    assert_eq!(rows[0]["branchId"], "main", "main comes first: {rows:?}");
    assert_eq!(rows[0]["state"], "open");
    assert!(rows[0].get("endEvent").is_none());
    let open = row(&rows, &child);
    assert_eq!(open["state"], "open");
    assert!(open["purpose"].is_null());
    assert!(open["cutMessages"].is_null());
    let detail = client
        .rpc(
            "session.branch.get",
            json!({"sessionId":id,"branchId":child}),
        )
        .await;
    assert_ok(&detail);
    assert_eq!(detail["result"]["purpose"], "child work");
    assert_eq!(detail["result"]["cutMessages"], 0);
    assert_eq!(open["parentBranchId"], "main");
    assert!(open["branchPoint"].as_u64().is_some(), "{open}");
    assert!(open["createEvent"].as_u64().is_some(), "{open}");
    assert_eq!(open["source"]["kind"], "host", "{open}");
    assert_eq!(row(&rows, &grandchild)["parentBranchId"], child);

    assert_ok(
        &client
            .rpc(
                "session.branch_end",
                json!({"sessionId":id,"branchId":child}),
            )
            .await,
    );
    let rows = directory(&mut client, &id).await;
    let ended = row(&rows, &child);
    let cascaded = row(&rows, &grandchild);
    assert_eq!(ended["state"], "ended", "{rows:?}");
    assert_eq!(ended["endEvent"], own_end(&mut client, &id, &child).await);
    assert_eq!(ended["head"], session.sequence(&child).unwrap());
    assert_eq!(cascaded["state"], "ended");
    let detail = client
        .rpc(
            "session.branch.get",
            json!({"sessionId":id,"branchId":child}),
        )
        .await;
    assert_ok(&detail);
    assert_eq!(detail["result"]["end"]["source"]["kind"], "host");
    let detail = client
        .rpc(
            "session.branch.get",
            json!({"sessionId":id,"branchId":grandchild}),
        )
        .await;
    assert_ok(&detail);
    assert_eq!(detail["result"]["end"]["ancestor"], child);
    assert_eq!(
        cascaded["endEvent"],
        own_end(&mut client, &id, &grandchild).await
    );
    assert_eq!(cascaded["head"], session.sequence(&grandchild).unwrap());
    assert_eq!(rows[0]["state"], "open", "main stays open");

    let head = ended["head"].as_u64().unwrap();
    let read = client
        .rpc(
            "session.history",
            json!({"sessionId":id,"branchId":child,"after":0,"limit":256,"through":head}),
        )
        .await;
    assert_ok(&read);
    assert_eq!(read["result"]["done"], true, "{read}");
    assert_eq!(read["result"]["through"], head);
    assert!(read["result"]["events"].as_array().unwrap().len() > 1);

    drop(client);
    drop(agent);
    server.shutdown();
    server.join().await.unwrap();
}

/// #1165: after a restart the directory keeps ids, parents, branch points,
/// create events and states; ended lines keep their fixed head and
/// `endEvent`, and open lines' heads are at or above their old value.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn branches_answer_the_same_directory_after_a_restart() {
    let store = Store::new();
    let (server, agent) = launch_with_agent(Arc::new(MockTools), store.config()).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let created = client.rpc("session.create", create_with_branch()).await;
    assert_ok(&created);
    let id = created["result"]["sessionId"].as_str().unwrap().to_owned();
    let session = agent.get_session(&id).unwrap();
    session.idle("main").await.unwrap();
    let child = open_line(&mut client, &id, "main", "child work").await;
    session.idle(&child).await.unwrap();
    let grandchild = open_line(&mut client, &id, &child, "grand work").await;
    session.idle(&grandchild).await.unwrap();
    let open = open_line(&mut client, &id, "main", "second child").await;
    session.idle(&open).await.unwrap();
    assert_ok(
        &client
            .rpc(
                "session.branch_end",
                json!({"sessionId":id,"branchId":child}),
            )
            .await,
    );
    let before = directory(&mut client, &id).await;
    drop(client);
    drop(agent);
    server.shutdown();
    server.join().await.unwrap();

    let (server, agent) = launch_with_agent(Arc::new(MockTools), store.config()).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let after = directory(&mut client, &id).await;
    assert_eq!(before.len(), after.len(), "{after:?}");
    for was in &before {
        let id = was["branchId"].as_str().unwrap();
        let now = row(&after, id);
        for field in [
            "parentBranchId",
            "branchPoint",
            "createEvent",
            "source",
            "state",
            "purpose",
            "cutMessages",
            "end",
        ] {
            assert_eq!(was.get(field), now.get(field), "{id} keeps {field}: {now}");
        }
        if was["state"] == "ended" {
            assert_eq!(was["head"], now["head"], "{id} keeps its fixed head");
            assert_eq!(was["endEvent"], now["endEvent"], "{id} keeps its end");
        } else {
            assert!(
                now["head"].as_u64().unwrap() >= was["head"].as_u64().unwrap(),
                "{id} head moves forward: {now}"
            );
        }
    }
    drop(client);
    drop(agent);
    server.shutdown();
    server.join().await.unwrap();
}

/// #1165: a fork's directory lists the lines that ended before the fork,
/// with the end their source recorded, and those lines have no events in
/// the fork.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_fork_lists_the_lines_that_ended_before_the_fork() {
    let store = Store::new();
    let (server, agent) = launch_with_agent(Arc::new(MockTools), store.config()).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let created = client.rpc("session.create", create_with_branch()).await;
    assert_ok(&created);
    let id = created["result"]["sessionId"].as_str().unwrap().to_owned();
    let session = agent.get_session(&id).unwrap();
    session.idle("main").await.unwrap();
    let child = open_line(&mut client, &id, "main", "child work").await;
    session.idle(&child).await.unwrap();
    let main_head = session.sequence("main").unwrap();
    assert_ok(
        &client
            .rpc(
                "session.branch_end",
                json!({"sessionId":id,"branchId":child}),
            )
            .await,
    );
    // The end is recorded on main when the tree closes the line; a fork cut
    // taken after it copies that record, one taken before does not.
    published_head(&session, "main", main_head + 1).await;
    let ended = row(&directory(&mut client, &id).await, &child).clone();
    let at = through(&mut client, &id, "main").await;
    let forked = client
        .rpc("session.fork", json!({"sessionId":id,"at":at}))
        .await;
    assert_ok(&forked);
    let fork = forked["result"]["sessionId"].as_str().unwrap().to_owned();

    let rows = directory(&mut client, &fork).await;
    assert_eq!(rows[0]["branchId"], "main", "{rows:?}");
    let copied = row(&rows, &child);
    assert_eq!(copied["state"], "ended", "{rows:?}");
    assert_eq!(copied["endEvent"], ended["endEvent"]);
    assert_eq!(copied["parentBranchId"], "main");
    let read = client
        .rpc(
            "session.history",
            json!({"sessionId":fork,"branchId":child,"after":0,"limit":256}),
        )
        .await;
    assert_ok(&read);
    assert!(
        read["result"]["events"].as_array().unwrap().is_empty(),
        "the fork holds none of the line's events: {read}"
    );
    drop(client);
    drop(agent);
    server.shutdown();
    server.join().await.unwrap();
}

/// #1165: a v1/v2 client has no `session.branches`, and a closed Session
/// answers the existing closed error.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn branches_require_v3_and_the_session() {
    let store = Store::new();
    let (server, agent) = launch_with_agent(Arc::new(MockTools), store.config()).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let created = client.rpc("session.create", create_with_branch()).await;
    assert_ok(&created);
    let id = created["result"]["sessionId"].as_str().unwrap().to_owned();

    let mut v2 = Client::new_version(&server, false, 2).await;
    let refused = v2.rpc("session.branches", json!({"sessionId":id})).await;
    assert_eq!(refused["error"]["code"], -32601, "{refused}");
    assert_eq!(
        refused["error"]["data"]["appError"], "unknown_method",
        "{refused}"
    );
    drop(v2);

    assert_ok(&client.rpc("session.close", json!({"sessionId":id})).await);
    let closed = client
        .rpc("session.branches", json!({"sessionId":id}))
        .await;
    assert_eq!(
        closed["error"]["data"]["appError"], "session_closed",
        "{closed}"
    );
    drop(client);
    drop(agent);
    server.shutdown();
    server.join().await.unwrap();
}

/// #1165: a line's own end commits while the turn that wrote it still runs.
/// This model issues the `branch` tool's `end` and then holds the turn on
/// `wait`, so the window the directory reports as `ending` stays open.
struct HoldModel;
impl HoldModel {
    fn text(message: &dim_agent_types::Message) -> String {
        match &message.content {
            dim_agent_types::MessageContent::Text(text) => text.clone(),
            dim_agent_types::MessageContent::Parts(parts) => parts
                .iter()
                .filter_map(|part| match part {
                    dim_agent_types::MessagePart::Text { text, .. } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n"),
        }
    }
    fn call(id: &str, name: &str, input: Value) -> Vec<dim_agent_capabilities::host::ModelChunk> {
        use dim_agent_capabilities::host::{ModelChunk, ModelStopReason};
        vec![
            ModelChunk::MessageStart {
                message_id: id.into(),
            },
            ModelChunk::ToolStart {
                id: id.into(),
                name: name.into(),
            },
            ModelChunk::ToolUse(dim_agent_types::ToolCall {
                id: id.into(),
                name: name.into(),
                input: serde_json::from_value(input).expect("tool input"),
            }),
            ModelChunk::MessageStop {
                message_id: id.into(),
                reason: ModelStopReason::ToolUse,
            },
        ]
    }
}
impl dim_agent_capabilities::host::ModelDriver for HoldModel {
    fn invoke(
        &self,
        request: Arc<dim_agent_types::ModelRequest>,
        connection: dim_agent_capabilities::host::ModelConnection,
        cancel: CancellationToken,
    ) -> HostFuture<'_, dim_agent_capabilities::host::ModelStream> {
        let messages: Vec<&dim_agent_types::Message> =
            request.messages.iter().map(AsRef::as_ref).collect();
        let held = messages.iter().any(|message| {
            message.role == dim_agent_types::Role::User && Self::text(message).trim() == "hold"
        });
        let results = messages
            .iter()
            .filter(|message| message.role == dim_agent_types::Role::ToolResult)
            .count();
        let holding = dim_agent_capabilities::adapters::EchoModel::last_message(&request)
            .is_some_and(|message| {
                message.role == dim_agent_types::Role::User && Self::text(message).trim() == "hold"
            });
        if holding {
            return Box::pin(async {
                Ok(Box::pin(futures_util::stream::iter(
                    Self::call(
                        "hold-end",
                        "branch",
                        json!({"action":"end","content":"the work is done"}),
                    )
                    .into_iter()
                    .map(Ok),
                ))
                    as dim_agent_capabilities::host::ModelStream)
            });
        }
        if held && results == 1 {
            return Box::pin(async {
                Ok(Box::pin(futures_util::stream::iter(
                    Self::call("hold-wait", "wait", json!({"millis":20000}))
                        .into_iter()
                        .map(Ok),
                ))
                    as dim_agent_capabilities::host::ModelStream)
            });
        }
        dim_agent_capabilities::adapters::EchoModel.invoke(request, connection, cancel)
    }
}

async fn launch_holding() -> (RunningServer, dim_agent_runtime::Agent) {
    let mut builder = AgentBuilder::new(AgentConfig::default());
    builder.timeline_store(Arc::new(dim_agent_runtime::MemoryTimelineStore::default()));
    defaults::install(&mut builder);
    defaults::mock_model_hosts_with_model(
        &mut builder,
        dim_agent_capabilities::history::HistoryRenderer::default(),
        Arc::new(HoldModel),
    );
    defaults::tool_host(&mut builder, Arc::new(MockTools));
    dim_app_server::collaboration::install(&mut builder);
    let server = serve(
        builder,
        ServerConfig {
            unix_path: Some(std::path::PathBuf::from("/tmp").join(format!(
                "dim-server-test-{}/server.sock",
                uuid::Uuid::new_v4()
            ))),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let agent = server.agent().unwrap();
    (server, agent)
}

async fn open_tool_line(
    client: &mut Client,
    session: &dim_agent_runtime::Session,
    id: &str,
) -> String {
    let text = format!(
        "tool:branch {}",
        json!({"action": "create", "prompt": "watch the tests"})
    );
    assert_ok(
        &client
            .rpc(
                "session.submit",
                json!({"sessionId":id,"branchId":"main","text":text}),
            )
            .await,
    );
    session.idle("main").await.unwrap();
    let child = session
        .branches()
        .into_iter()
        .find(|info| info.parent.as_deref() == Some("main"))
        .expect("main opened a line")
        .id;
    session.idle(&child).await.unwrap();
    child
}

/// Waits until the line's directory row reads `state`, and returns it.
async fn row_in_state(client: &mut Client, id: &str, line: &str, state: &str) -> Value {
    let waited = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let rows = directory(client, id).await;
            let row = row(&rows, line).clone();
            if row["state"] == state {
                return row;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await;
    waited.unwrap_or_else(|_| panic!("{line} never read {state}"))
}

/// #1165: the wire reports a line whose end is pending as `ending` with its
/// committed `branch/end`, and as `ended`, with the same event, once its turn
/// ends.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_line_whose_end_is_pending_reads_ending_on_the_wire() {
    let (server, agent) = launch_holding().await;
    let mut client = Client::new_version(&server, false, 3).await;
    let created = client
        .rpc(
            "session.create",
            json!({
                "cwd": std::env::temp_dir(),
                "tools": ["branch", "wait"],
                "permissionPolicy": {"allowedTools": ["branch", "wait"]}
            }),
        )
        .await;
    assert_ok(&created);
    let id = created["result"]["sessionId"].as_str().unwrap().to_owned();
    let session = agent.get_session(&id).unwrap();
    assert_ok(&client.rpc("session.attach", json!({"sessionId": id})).await);
    let child = open_tool_line(&mut client, &session, &id).await;
    assert_ok(
        &client
            .rpc(
                "session.submit",
                json!({"sessionId":id,"branchId":child,"text":"hold"}),
            )
            .await,
    );
    let ending = row_in_state(&mut client, &id, &child, "ending").await;
    let end = own_end(&mut client, &id, &child).await;
    assert_eq!(
        ending["endEvent"], end,
        "the pending end is the line's own branch/end"
    );
    assert!(
        ending["head"].as_u64().unwrap() >= end,
        "the ending head includes the committed end: {ending}"
    );
    assert_ok(
        &client
            .rpc("session.stop", json!({"sessionId":id,"branchId":child}))
            .await,
    );
    let ended = row_in_state(&mut client, &id, &child, "ended").await;
    assert_eq!(
        ended["endEvent"], end,
        "the turn's end closes the same event: {ended}"
    );
    assert_eq!(ended["head"], session.sequence(&child).unwrap());
    drop(client);
    drop(agent);
    server.shutdown();
    server.join().await.unwrap();
}

/// The product's prompt sources with a replaceable clock and a 10 ms check:
/// a flow moves the date while the Session is idle and waits for the append.
async fn launch_prompt_sources(
    store: &Store,
    today: Arc<std::sync::Mutex<String>>,
) -> (RunningServer, dim_agent_runtime::Agent, std::path::PathBuf) {
    let home = std::env::temp_dir().join(format!(
        "dp-{}",
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    ));
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&home)
            .unwrap();
    }
    let mut builder = AgentBuilder::new(AgentConfig::default());
    builder.timeline_store(Arc::new(dim_agent_runtime::MemoryTimelineStore::default()));
    defaults::install_with_prompt_capability(
        &mut builder,
        dim_agent_capabilities::model_provider::ModelProviderPlugin::new,
    );
    builder.session_value(Arc::new(
        dim_agent_capabilities::permission::CommandRulesHost(Arc::new(
            dim_agent_capabilities::permission::NoCommandRules,
        )),
    ));
    defaults::mock_model_hosts(&mut builder);
    defaults::tool_host(&mut builder, Arc::new(MockTools));
    dim_app_server::collaboration::install(&mut builder);
    dim_agent_capabilities::prompt_sources::install_with_clock(
        &mut builder,
        Arc::new(move || today.lock().unwrap().clone()),
        Duration::from_millis(10),
    );
    let platform = Arc::new(prompt_platform(&home));
    builder.plugin(move |_| Ok(Box::new(PromptPlatform(platform.clone()))));
    let mut config = store.config();
    config.unix_path = Some(home.join("server.sock"));
    let server = serve(builder, config).await.unwrap();
    let agent = server.agent().unwrap();
    (server, agent, home)
}

fn prompt_platform(home: &std::path::Path) -> dim_agent_capabilities::host_platform::HostPlatform {
    use dim_agent_capabilities::host_platform::{HostPlatform, Os, ShellStatus};
    let mut platform = HostPlatform::current(ShellStatus::Unavailable {
        code: "PROMPT_BRANCH",
        reason: "the flow runs no command".into(),
    })
    .unwrap();
    platform.home = home.to_owned();
    platform.os = Os::Linux;
    platform.arch = "x64".into();
    platform.timezone = Some("Asia/Shanghai".into());
    platform.env.clear();
    platform
}

struct PromptPlatform(Arc<dim_agent_capabilities::host_platform::HostPlatform>);
impl rutis::host::DynPlugin for PromptPlatform {
    fn name(&self) -> &str {
        "PromptBranchPlatform"
    }
    fn injects(&self) -> Vec<rutis::host::TypeKey> {
        Vec::new()
    }
    fn apply<'a>(
        &'a self,
        ctx: &'a rutis::Ctx,
    ) -> rutis::host::BoxFuture<'a, rutis::Result<rutis::host::Effect>> {
        Box::pin(async move {
            ctx.provide_as::<dim_agent_capabilities::host_platform::HostPlatform>(self.0.clone())?;
            Ok(rutis::host::Effect::Done)
        })
    }
}

/// Whether a v3 history event delivers the raw block `text`.
fn delivers(event: &Value, text: &str) -> bool {
    event["type"] == "input"
        && event["name"] == "message"
        && event["data"]["blocks"].as_array().is_some_and(|blocks| {
            blocks
                .iter()
                .any(|block| block["text"].as_str() == Some(text))
        })
}

/// Waits until `main`'s history delivers the raw block `text`, and returns
/// the published head that holds it.
async fn until_delivered(client: &mut Client, session: &str, text: &str) -> u64 {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let page = client
                .rpc(
                    "session.history",
                    json!({"sessionId":session,"branchId":"main","after":0,"limit":256}),
                )
                .await;
            assert_ok(&page);
            if page["result"]["events"]
                .as_array()
                .is_some_and(|events| events.iter().any(|event| delivers(event, text)))
            {
                return page["result"]["through"].as_u64().unwrap();
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the periodic check appends the value")
}

/// The text of every message of `request`, as the model reads it.
fn sent_text(request: &dim_agent_types::ModelRequest) -> String {
    request
        .messages
        .iter()
        .map(|message| match &message.content {
            dim_agent_types::MessageContent::Text(text) => text.clone(),
            dim_agent_types::MessageContent::Parts(parts) => parts
                .iter()
                .filter_map(|part| match part {
                    dim_agent_types::MessagePart::Text { text, .. } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n"),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The last `model/call` request of `branch`'s own facts.
async fn last_request(
    session: &dim_agent_runtime::Session,
    branch: &str,
) -> Arc<dim_agent_types::ModelRequest> {
    use dim_agent_runtime::testing::branch_events;
    let events = branch_events(session, branch, ..).await.unwrap();
    events
        .iter()
        .rev()
        .find_map(|event| match &event.kind {
            dim_agent_types::EventKind::ModelCall(data) => Some(data.request.clone()),
            _ => None,
        })
        .expect("the branch made a model request")
}

const NEW_DAY: &str = "<dim-state source=\"dim/environment/date\">2026-10-01</dim-state>";
const OLD_DAY: &str = "<dim-state source=\"dim/environment/date\">2026-09-30</dim-state>";

/// #1361: main adopts a State source while idle, appending the change and its
/// input. A line cut after that append but before main's next request
/// consumes the input inherits the new value in its prompt state but not the
/// block in the history it continues, and its own check finds the values
/// equal. Its first request must still carry the new value.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_line_cut_before_main_consumes_an_adopted_state_change_carries_the_new_value() {
    let store = Store::new();
    let today = Arc::new(std::sync::Mutex::new("2026-09-30".to_owned()));
    let (server, agent, home) = launch_prompt_sources(&store, today.clone()).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let created = client
        .rpc(
            "session.create",
            json!({
                "cwd": home,
                "tools": ["branch"],
                "permissionPolicy": {"allowedTools": ["branch"]}
            }),
        )
        .await;
    assert_ok(&created);
    let id = created["result"]["sessionId"].as_str().unwrap().to_owned();
    assert_ok(
        &client
            .rpc(
                "session.submit",
                json!({"sessionId":id,"branchId":"main","text":"first"}),
            )
            .await,
    );
    assert_ok(
        &client
            .rpc("session.idle", json!({"sessionId":id,"branchId":"main"}))
            .await,
    );
    let session = agent.get_session(&id).unwrap();
    let first = last_request(&session, "main").await;
    assert_eq!(
        sent_text(&first).matches(OLD_DAY).count(),
        1,
        "main's first request carries the date it started with"
    );

    *today.lock().unwrap() = "2026-10-01".to_owned();
    let at = until_delivered(&mut client, &id, NEW_DAY).await;
    let created = client
        .rpc(
            "session.branch_create",
            json!({"sessionId":id,"parentBranchId":"main","at":at,"prompt":"child work"}),
        )
        .await;
    assert_ok(&created);
    let child = created["result"]["branch"]["branchId"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_ok(
        &client
            .rpc("session.idle", json!({"sessionId":id,"branchId":child}))
            .await,
    );

    let first = last_request(&session, &child).await;
    let text = sent_text(&first);
    assert_eq!(
        text.matches(NEW_DAY).count(),
        1,
        "the line's first request carries the value main adopted before the cut:\n{text}"
    );
    assert_eq!(
        text.matches(OLD_DAY).count(),
        1,
        "the inherited prefix still carries the value it started with:\n{text}"
    );

    // A fork is a new Session whose next request consumes the pending input
    // it copied, so the same cut does not lose the value there.
    let forked = client
        .rpc("session.fork", json!({"sessionId":id,"at":at}))
        .await;
    assert_ok(&forked);
    let fork = forked["result"]["sessionId"].as_str().unwrap().to_owned();
    assert_ok(
        &client
            .rpc(
                "session.submit",
                json!({"sessionId":fork,"branchId":"main","text":"after the fork"}),
            )
            .await,
    );
    assert_ok(
        &client
            .rpc("session.idle", json!({"sessionId":fork,"branchId":"main"}))
            .await,
    );
    let fork_session = agent.get_session(&fork).unwrap();
    let first = last_request(&fork_session, "main").await;
    assert_eq!(
        sent_text(&first).matches(NEW_DAY).count(),
        1,
        "the fork consumes the pending input it copied"
    );

    drop(client);
    drop(agent);
    server.shutdown();
    server.join().await.unwrap();
}

/// The revoke block `dim/instructions/project` renders when its AGENTS.md
/// leaves the rule in force.
const RULE_REVOKE: &str = "<dim-rule source=\"dim/instructions/project\" scope=\"branch\" op=\"revoke\">\nThis rule no longer applies to branch. Other scopes are unaffected.\n</dim-rule>";

/// #1361, the retire direction: main drops a rule while idle, appending the
/// revoke and its input. A line cut before main consumes that input inherits
/// the set block but not the revoke, so its own check would find nothing to
/// do. Its first request must show the rule as no longer applying.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_line_cut_before_main_consumes_a_removed_rule_does_not_keep_it_live() {
    let store = Store::new();
    let today = Arc::new(std::sync::Mutex::new("2026-09-30".to_owned()));
    let (server, agent, home) = launch_prompt_sources(&store, today).await;
    std::fs::write(home.join("AGENTS.md"), "RULE-ONE").unwrap();
    let mut client = Client::new_version(&server, false, 3).await;
    let created = client
        .rpc(
            "session.create",
            json!({
                "cwd": home,
                "tools": ["branch"],
                "permissionPolicy": {"allowedTools": ["branch"]}
            }),
        )
        .await;
    assert_ok(&created);
    let id = created["result"]["sessionId"].as_str().unwrap().to_owned();
    assert_ok(
        &client
            .rpc(
                "session.submit",
                json!({"sessionId":id,"branchId":"main","text":"first"}),
            )
            .await,
    );
    assert_ok(
        &client
            .rpc("session.idle", json!({"sessionId":id,"branchId":"main"}))
            .await,
    );
    let session = agent.get_session(&id).unwrap();
    let first = last_request(&session, "main").await;
    assert_eq!(
        sent_text(&first).matches("RULE-ONE").count(),
        1,
        "main's first request carries the rule it started with"
    );

    std::fs::remove_file(home.join("AGENTS.md")).unwrap();
    let at = until_delivered(&mut client, &id, RULE_REVOKE).await;
    let created = client
        .rpc(
            "session.branch_create",
            json!({"sessionId":id,"parentBranchId":"main","at":at,"prompt":"child work"}),
        )
        .await;
    assert_ok(&created);
    let child = created["result"]["branch"]["branchId"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_ok(
        &client
            .rpc("session.idle", json!({"sessionId":id,"branchId":child}))
            .await,
    );

    let first = last_request(&session, &child).await;
    let text = sent_text(&first);
    let set = text
        .find("RULE-ONE")
        .expect("the inherited prefix still shows the rule");
    let revoke = text
        .find(RULE_REVOKE)
        .expect("the line revokes the rule the prefix still shows");
    assert!(set < revoke, "the revoke follows the rule it ends:\n{text}");
    assert_eq!(
        text.matches(RULE_REVOKE).count(),
        1,
        "the rule is revoked once:\n{text}"
    );

    drop(client);
    drop(agent);
    server.shutdown();
    server.join().await.unwrap();
}

/// #1361: after a line re-adopts the value it inherited undelivered, a
/// reload rebuilds the same state and its next request carries the value
/// once — the line's own consumed block.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reloaded_line_does_not_deliver_an_undelivered_ancestor_change_again() {
    let store = Store::new();
    let today = Arc::new(std::sync::Mutex::new("2026-09-30".to_owned()));
    let (server, agent, home) = launch_prompt_sources(&store, today.clone()).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let created = client
        .rpc(
            "session.create",
            json!({
                "cwd": home,
                "tools": ["branch"],
                "permissionPolicy": {"allowedTools": ["branch"]}
            }),
        )
        .await;
    assert_ok(&created);
    let id = created["result"]["sessionId"].as_str().unwrap().to_owned();
    assert_ok(
        &client
            .rpc(
                "session.submit",
                json!({"sessionId":id,"branchId":"main","text":"first"}),
            )
            .await,
    );
    assert_ok(
        &client
            .rpc("session.idle", json!({"sessionId":id,"branchId":"main"}))
            .await,
    );
    let session = agent.get_session(&id).unwrap();
    *today.lock().unwrap() = "2026-10-01".to_owned();
    let at = until_delivered(&mut client, &id, NEW_DAY).await;
    let created = client
        .rpc(
            "session.branch_create",
            json!({"sessionId":id,"parentBranchId":"main","at":at,"prompt":"child work"}),
        )
        .await;
    assert_ok(&created);
    let child = created["result"]["branch"]["branchId"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_ok(
        &client
            .rpc("session.idle", json!({"sessionId":id,"branchId":child}))
            .await,
    );
    let first = last_request(&session, &child).await;
    assert_eq!(
        sent_text(&first).matches(NEW_DAY).count(),
        1,
        "the line re-adopts the value before its first request"
    );

    drop(client);
    drop(agent);
    server.shutdown();
    server.join().await.unwrap();

    let (server, agent, _) = launch_prompt_sources(&store, today).await;
    let mut client = Client::new_version(&server, false, 3).await;
    assert_ok(&client.rpc("session.attach", json!({"sessionId":id})).await);
    assert_ok(
        &client
            .rpc(
                "session.submit",
                json!({"sessionId":id,"branchId":child,"text":"after the reload"}),
            )
            .await,
    );
    assert_ok(
        &client
            .rpc("session.idle", json!({"sessionId":id,"branchId":child}))
            .await,
    );
    let session = agent.get_session(&id).unwrap();
    let reloaded = last_request(&session, &child).await;
    let text = sent_text(&reloaded);
    assert_eq!(
        text.matches(NEW_DAY).count(),
        1,
        "the reloaded line keeps the value once, from its own consumed block:\n{text}"
    );
    assert_eq!(
        text.matches(OLD_DAY).count(),
        1,
        "the inherited prefix still carries the value it started with:\n{text}"
    );

    drop(client);
    drop(agent);
    server.shutdown();
    server.join().await.unwrap();
}

/// The inputs `branch` wrote that carry the raw block `text`.
async fn deliveries(session: &dim_agent_runtime::Session, branch: &str, text: &str) -> usize {
    use dim_agent_runtime::testing::chain_events;
    let events = chain_events(session, branch, ..).await.unwrap();
    events
        .iter()
        .filter(|event| match &event.kind {
            dim_agent_types::EventKind::InputMessage(data) => data.blocks.iter().any(|block| {
                matches!(block, dim_agent_types::InputBlock::Raw { text: carried, .. } if carried == text)
            }),
            _ => false,
        })
        .count()
}

/// #1361: a value the line adopted itself stays delivered once across idle
/// check periods — the delivered view still holds the line's own change.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_idle_main_does_not_deliver_one_adopted_value_again() {
    let store = Store::new();
    let today = Arc::new(std::sync::Mutex::new("2026-09-30".to_owned()));
    let (server, agent, home) = launch_prompt_sources(&store, today.clone()).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let created = client
        .rpc("session.create", json!({"cwd": home, "tools": []}))
        .await;
    assert_ok(&created);
    let id = created["result"]["sessionId"].as_str().unwrap().to_owned();
    assert_ok(
        &client
            .rpc(
                "session.submit",
                json!({"sessionId":id,"branchId":"main","text":"first"}),
            )
            .await,
    );
    assert_ok(
        &client
            .rpc("session.idle", json!({"sessionId":id,"branchId":"main"}))
            .await,
    );
    let session = agent.get_session(&id).unwrap();
    *today.lock().unwrap() = "2026-10-01".to_owned();
    until_delivered(&mut client, &id, NEW_DAY).await;
    // Several 10 ms check periods pass with the line idle.
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(
        deliveries(&session, "main", NEW_DAY).await,
        1,
        "one adopted value is one delivery input"
    );

    // A reload rebuilds the line's own change as delivered, so its open
    // adopts nothing and the value keeps one delivery.
    drop(client);
    drop(agent);
    server.shutdown();
    server.join().await.unwrap();
    let (server, agent, _) = launch_prompt_sources(&store, today).await;
    let mut client = Client::new_version(&server, false, 3).await;
    assert_ok(&client.rpc("session.attach", json!({"sessionId":id})).await);
    assert_ok(
        &client
            .rpc(
                "session.submit",
                json!({"sessionId":id,"branchId":"main","text":"after the reload"}),
            )
            .await,
    );
    assert_ok(
        &client
            .rpc("session.idle", json!({"sessionId":id,"branchId":"main"}))
            .await,
    );
    let session = agent.get_session(&id).unwrap();
    assert_eq!(
        deliveries(&session, "main", NEW_DAY).await,
        1,
        "the reloaded line does not deliver the value again"
    );
    let reloaded = last_request(&session, "main").await;
    assert_eq!(
        sent_text(&reloaded).matches(NEW_DAY).count(),
        1,
        "the reloaded request carries the value once"
    );

    drop(client);
    drop(agent);
    server.shutdown();
    server.join().await.unwrap();
}

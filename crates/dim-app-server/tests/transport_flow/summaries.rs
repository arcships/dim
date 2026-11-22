use super::*;
use dim_agent_runtime::testing::branch_events as read_branch_events;
use dim_agent_types::{EventKind, InputBlock, InputMessageData, ROOT_BRANCH_ID};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn summaries_are_v3_filtered_and_bounded() {
    let server = launch(Arc::new(MockTools), ServerConfig::default()).await;
    let mut client = Client::new_version(&server, false, 3).await;
    assert!(
        client
            .features
            .as_array()
            .unwrap()
            .contains(&json!("session.summaries"))
    );

    let empty = client.rpc("session.create", create(json!([]))).await;
    let empty_id = empty["result"]["sessionId"].as_str().unwrap().to_owned();
    let empty_session = server.agent().unwrap().get_session(&empty_id).unwrap();

    let acted = client.rpc("session.create", create(json!([]))).await;
    let acted_id = acted["result"]["sessionId"].as_str().unwrap().to_owned();
    let acted_session = server.agent().unwrap().get_session(&acted_id).unwrap();
    while dim_agent_runtime::core::unix_millis() <= acted_session.created_at() {
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    acted_session
        .input(InputMessageData::text("the first thing said", None), "main")
        .await
        .unwrap();

    let created = client.rpc("session.create", create(json!([]))).await;
    let session_id = created["result"]["sessionId"].as_str().unwrap().to_owned();
    let session = server.agent().unwrap().get_session(&session_id).unwrap();
    session
        .input(
            InputMessageData {
                blocks: vec![InputBlock::Raw {
                    text: "runtime state".into(),
                    key: Some("state".into()),
                    retire: false,
                }],
                ..Default::default()
            },
            "main",
        )
        .await
        .unwrap();
    let long = "界".repeat(130);
    session
        .input(InputMessageData::text(&long, None), "main")
        .await
        .unwrap();

    let image = client.rpc("session.create", create(json!([]))).await;
    let image_id = image["result"]["sessionId"].as_str().unwrap();
    let image_session = server.agent().unwrap().get_session(image_id).unwrap();
    image_session
        .input(
            serde_json::from_value(json!({
                "user": {"content": [{"type":"image", "source": {
                    "type":"url", "mediaType":"image/png", "url":"https://example.test/image.png"
                }}]}
            }))
            .unwrap(),
            "main",
        )
        .await
        .unwrap();
    image_session
        .input(
            InputMessageData::text("not the first user input", None),
            "main",
        )
        .await
        .unwrap();

    let bounded = client.rpc("session.create", create(json!([]))).await;
    let bounded_id = bounded["result"]["sessionId"].as_str().unwrap().to_owned();
    let bounded_session = server.agent().unwrap().get_session(&bounded_id).unwrap();
    for index in 0..256 {
        bounded_session
            .input(
                InputMessageData {
                    blocks: vec![InputBlock::Raw {
                        text: format!("raw {index}"),
                        key: None,
                        retire: false,
                    }],
                    ..Default::default()
                },
                "main",
            )
            .await
            .unwrap();
    }
    bounded_session
        .input(InputMessageData::text("past the bound", None), "main")
        .await
        .unwrap();

    let summaries = client.rpc("session.summaries", json!({})).await;
    assert_ok(&summaries);
    let rows = summaries["result"].as_array().unwrap();
    let empty_row = rows
        .iter()
        .find(|row| row["sessionId"] == empty_id)
        .unwrap();
    let initial = read_branch_events(
        &empty_session,
        "main",
        ..=empty_row["sequence"].as_u64().unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(
        empty_row["updatedAt"],
        activity_time(&initial).unwrap_or(empty_session.created_at())
    );
    assert!(empty_row.get("preview").is_none());
    let acted_row = rows
        .iter()
        .find(|row| row["sessionId"] == acted_id)
        .unwrap();
    let through = acted_row["sequence"].as_u64().unwrap();
    let acted_events = read_branch_events(&acted_session, "main", ..=through)
        .await
        .unwrap();
    let said = acted_events
        .iter()
        .find_map(|event| match &event.kind {
            EventKind::InputMessage(data) if data.user.is_some() => Some(event.create_at),
            _ => None,
        })
        .expect("the session's user message");
    assert!(
        said > acted_session.created_at(),
        "the user message's event time is past the creation time"
    );
    assert_eq!(
        acted_row["updatedAt"],
        activity_time(&acted_events).expect("the session's user message is its activity"),
        "the row's activity time is its newest activity event's own"
    );
    assert_ne!(
        acted_row["updatedAt"],
        acted_session.created_at(),
        "the row's activity time is not the creation time"
    );
    assert!(acted_row.get("preview").is_some());
    let row = rows
        .iter()
        .find(|row| row["sessionId"] == session_id)
        .unwrap();
    let preview = row["preview"].as_str().unwrap();
    assert_eq!(preview.chars().count(), 120);
    assert!(preview.ends_with('…'));
    let through = row["sequence"].as_u64().unwrap();
    let all = read_branch_events(&session, "main", ..=through)
        .await
        .unwrap();
    assert_eq!(
        row["updatedAt"],
        activity_time(&all).unwrap_or(session.created_at())
    );
    assert!(
        rows.iter()
            .find(|row| row["sessionId"] == image_id)
            .unwrap()
            .get("preview")
            .is_none()
    );
    let bounded_row = rows
        .iter()
        .find(|row| row["sessionId"] == bounded_id)
        .unwrap();
    assert!(bounded_row.get("preview").is_none());

    let agent = client.rpc("agent.create", json!({})).await;
    let agent_id = agent["result"]["agentId"].as_str().unwrap();
    let mut params = create(json!([]));
    params["agentId"] = json!(agent_id);
    assert_ok(&client.rpc("session.create", params).await);
    let filtered = client
        .rpc("session.summaries", json!({"agentId":agent_id}))
        .await;
    assert_eq!(filtered["result"].as_array().unwrap().len(), 1);
    assert!(
        filtered["result"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["agentId"] == agent_id)
    );

    let mut old = Client::new_version(&server, false, 2).await;
    assert_eq!(
        old.rpc("session.summaries", json!({})).await["error"]["data"]["appError"],
        "unknown_method"
    );
    server.shutdown();
    server.join().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn summaries_keep_healthy_rows_while_other_sessions_close() {
    let directory = std::env::temp_dir().join(format!("dim-summary-{}", uuid::Uuid::new_v4()));
    let server = launch(
        Arc::new(MockTools),
        ServerConfig {
            database_path: Some(directory.join("dimcode.sqlite")),
            ..Default::default()
        },
    )
    .await;
    let mut writer = Client::new_version(&server, false, 3).await;
    let healthy = writer.rpc("session.create", create(json!([]))).await;
    assert_ok(&healthy);
    let healthy_id = healthy["result"]["sessionId"].as_str().unwrap().to_owned();
    assert_ok(
        &writer
            .rpc(
                "session.submit",
                json!({"sessionId":healthy_id,"branchId":"main","text":"retained summary"}),
            )
            .await,
    );
    assert_ok(
        &writer
            .rpc(
                "session.idle",
                json!({"sessionId":healthy_id,"branchId":"main"}),
            )
            .await,
    );
    let mut closing = Vec::new();
    for _ in 0..96 {
        let created = writer.rpc("session.create", create(json!([]))).await;
        assert_ok(&created);
        closing.push(created["result"]["sessionId"].as_str().unwrap().to_owned());
    }
    let first = Client::new_version(&server, false, 3).await;
    let second = Client::new_version(&server, true, 3).await;
    let finished = CancellationToken::new();
    let read = |mut client: Client| {
        let finished = finished.clone();
        async move {
            let mut replies = Vec::new();
            loop {
                replies.push(client.rpc("session.summaries", json!({})).await);
                if finished.is_cancelled() {
                    return replies;
                }
            }
        }
    };
    let close = async {
        for id in closing {
            assert_ok(&writer.rpc("session.close", json!({"sessionId":id})).await);
        }
        finished.cancel();
    };
    let (first, second, ()) = tokio::join!(read(first), read(second), close);
    let final_reply = writer.rpc("session.summaries", json!({})).await;
    server.shutdown();
    server.join().await.unwrap();

    for reply in first.iter().chain(&second).chain([&final_reply]) {
        assert_ok(reply);
        let retained = reply["result"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["sessionId"] == healthy_id)
            .expect("closing other sessions must not hide the healthy row");
        assert_eq!(retained["preview"], "retained summary");
    }
    assert_eq!(final_reply["result"].as_array().unwrap().len(), 1);
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn product_management_without_execution_still_exposes_summaries() {
    let home = std::env::temp_dir().join(format!(
        "ds-{}",
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    ));
    let native = home.join("rust");
    let server = serve(
        AgentBuilder::new(AgentConfig::default()),
        ServerConfig {
            unix_path: Some(native.join("daemon.sock")),
            target_home: Some(native),
            product_home: Some(home.clone()),
            session_execution: false,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert!(server.agent().is_none());
    let mut client = Client::new_version(&server, false, 3).await;
    assert!(
        client
            .features
            .as_array()
            .unwrap()
            .contains(&json!("session.summaries"))
    );
    let summaries = client.rpc("session.summaries", json!({})).await;
    assert_ok(&summaries);
    assert_eq!(summaries["result"], json!([]));
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// A Session this daemon closed stays out of `session.summaries` until the
/// read asks for it; `includeClosed` lists it marked `closed`, and an
/// unknown parameter is still refused.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn summaries_list_a_closed_session_only_when_asked() {
    let store = deletion::Store::new();
    let (server, _) = launch_with_agent(Arc::new(MockTools), store.config()).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let sid = deletion::session(&mut client).await;
    assert_ok(&client.rpc("session.close", json!({"sessionId":sid})).await);

    let open = client.rpc("session.summaries", json!({})).await;
    assert_ok(&open);
    let rows = open["result"].as_array().unwrap();
    assert!(
        !rows.iter().any(|row| row["sessionId"] == sid),
        "the closed Session is not listed: {open}"
    );
    assert!(
        rows.iter().all(|row| row["closed"] == false),
        "every summary carries closed: {open}"
    );

    let including = client
        .rpc("session.summaries", json!({"includeClosed":true}))
        .await;
    assert_ok(&including);
    let rows = including["result"].as_array().unwrap();
    let row = rows
        .iter()
        .find(|row| row["sessionId"] == sid)
        .unwrap_or_else(|| panic!("the closed Session is listed: {including}"));
    assert_eq!(row["closed"], true, "{including}");
    assert!(
        rows.iter()
            .filter(|row| row["sessionId"] != sid)
            .all(|row| row["closed"] == false),
        "{including}"
    );

    let refused = client
        .rpc("session.summaries", json!({"unknownField":true}))
        .await;
    assert_eq!(refused["error"]["code"], -32602, "{refused}");
    assert_eq!(
        refused["error"]["data"]["appError"], "invalid_params",
        "{refused}"
    );

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
}

/// Holds a tool line's opening turn on `wait`, so the line has its opening
/// event and no activity of its own while the listings read it.
struct HeldToolLine;
impl HeldToolLine {
    fn hold(id: &str) -> Vec<dim_agent_capabilities::host::ModelChunk> {
        use dim_agent_capabilities::host::{ModelChunk, ModelStopReason};
        vec![
            ModelChunk::MessageStart {
                message_id: id.into(),
            },
            ModelChunk::ToolStart {
                id: id.into(),
                name: "wait".into(),
            },
            ModelChunk::ToolUse(ToolCall {
                id: id.into(),
                name: "wait".into(),
                input: serde_json::from_value(json!({"millis": 20000})).expect("tool input"),
            }),
            ModelChunk::MessageStop {
                message_id: id.into(),
                reason: ModelStopReason::ToolUse,
            },
        ]
    }
}
impl dim_agent_capabilities::host::ModelDriver for HeldToolLine {
    fn invoke(
        &self,
        request: Arc<dim_agent_types::ModelRequest>,
        connection: dim_agent_capabilities::host::ModelConnection,
        cancel: CancellationToken,
    ) -> HostFuture<'_, dim_agent_capabilities::host::ModelStream> {
        let tool_line = request
            .messages
            .iter()
            .any(|message| match &message.content {
                dim_agent_types::MessageContent::Text(text) => text.contains("source=\"tool\""),
                dim_agent_types::MessageContent::Parts(parts) => parts.iter().any(|part| {
                    matches!(
                        part,
                        dim_agent_types::MessagePart::Text { text, .. }
                            if text.contains("source=\"tool\"")
                    )
                }),
            });
        if tool_line {
            return Box::pin(async {
                Ok(Box::pin(futures_util::stream::iter(
                    Self::hold("held-tool-line").into_iter().map(Ok),
                ))
                    as dim_agent_capabilities::host::ModelStream)
            });
        }
        dim_agent_capabilities::adapters::EchoModel.invoke(request, connection, cancel)
    }
}

async fn launch_holding_tool_line(
    config: ServerConfig,
) -> (RunningServer, dim_agent_runtime::Agent) {
    launch_holding_tool_line_with(
        config,
        Arc::new(dim_agent_runtime::MemoryTimelineStore::default()),
    )
    .await
}

async fn launch_holding_tool_line_with(
    config: ServerConfig,
    timeline: Arc<dyn dim_agent_runtime::TimelineStore>,
) -> (RunningServer, dim_agent_runtime::Agent) {
    let mut builder = AgentBuilder::new(AgentConfig::default());
    builder.timeline_store(dim_agent_runtime::TimelineStoreBinding::new(timeline));
    defaults::install(&mut builder);
    defaults::mock_model_hosts_with_model(
        &mut builder,
        dim_agent_capabilities::history::HistoryRenderer::default(),
        Arc::new(HeldToolLine),
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
            ..config
        },
    )
    .await
    .unwrap();
    let agent = server.agent().unwrap();
    (server, agent)
}

/// A tool line switched to while its first turn runs has an opening event but
/// no activity of its own: both listings date it by that opening, not by the
/// Session's creation (session-catalog §3.2).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_empty_tool_line_is_dated_by_its_opening_in_both_listings() {
    let store = deletion::Store::new();
    for config in [ServerConfig::default(), store.config()] {
        let (server, agent) = launch_holding_tool_line(config).await;
        let mut client = Client::new_version(&server, false, 3).await;
        let created = client
            .rpc(
                "session.create",
                json!({
                    "cwd": std::env::temp_dir(),
                    "tools": ["branch", "wait"],
                    "permissionPolicy": {"preset": "full-access"}
                }),
            )
            .await;
        assert_ok(&created);
        let id = created["result"]["sessionId"].as_str().unwrap().to_owned();
        let session = agent.get_session(&id).unwrap();
        while dim_agent_runtime::core::unix_millis() <= session.created_at() {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        let open = format!(
            "tool:branch {}",
            json!({"action": "create", "prompt": "hold this line"})
        );
        assert_ok(
            &client
                .rpc(
                    "session.submit",
                    json!({"sessionId": id, "branchId": "main", "text": open}),
                )
                .await,
        );
        let child = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Some(branch) = session
                    .branches()
                    .into_iter()
                    .find(|branch| branch.parent.as_deref() == Some("main"))
                {
                    return branch.id;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("main opens the tool line");
        tokio::time::timeout(Duration::from_secs(10), async {
            while !session.state(&child).is_ok_and(|state| state.running) {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("the tool line starts its first turn");
        assert_ok(
            &client
                .rpc(
                    "session.branch_set_default",
                    json!({"sessionId": id, "branchId": child}),
                )
                .await,
        );
        let cutoff = session.sequence(&child).unwrap();
        let events = read_branch_events(&session, &child, ..=cutoff)
            .await
            .unwrap();
        let opening = events
            .first()
            .expect("the line has its opening event")
            .create_at;
        assert!(
            opening > session.created_at(),
            "the opening event is later than the Session's creation: {opening} vs {}",
            session.created_at()
        );
        assert!(
            activity_time(&events).is_none(),
            "the line has no activity of its own yet"
        );
        let summaries = client.rpc("session.summaries", json!({})).await;
        assert_ok(&summaries);
        let row = summaries["result"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["sessionId"] == json!(id))
            .unwrap_or_else(|| panic!("the Session is listed: {summaries}"));
        assert_eq!(
            row["updatedAt"],
            json!(opening),
            "the line is dated by its own opening event, not the Session's creation"
        );
        session
            .stop(&child, "the listing read the line".into())
            .await
            .unwrap();
        drop(client);
        drop(session);
        drop(agent);
        server.shutdown();
        server.join().await.unwrap();
    }
}

/// Delegates to the in-memory store and holds the first summary activity read
/// on a line that is not the root, so a test can move the default branch away
/// and end that line while the read is in flight.
struct SummaryHoldStore {
    memory: dim_agent_runtime::MemoryTimelineStore,
    entered: std::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    release: tokio::sync::Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
    armed: AtomicBool,
}

impl SummaryHoldStore {
    fn new() -> (
        Arc<Self>,
        tokio::sync::oneshot::Receiver<()>,
        tokio::sync::oneshot::Sender<()>,
    ) {
        let (entered, entered_rx) = tokio::sync::oneshot::channel();
        let (release, release_rx) = tokio::sync::oneshot::channel();
        (
            Arc::new(Self {
                memory: dim_agent_runtime::MemoryTimelineStore::default(),
                entered: std::sync::Mutex::new(Some(entered)),
                release: tokio::sync::Mutex::new(Some(release_rx)),
                armed: AtomicBool::new(true),
            }),
            entered_rx,
            release,
        )
    }

    /// The summary's activity read: the newest activity timestamp on a line
    /// other than the root (session catalog §3.2).
    fn holds(
        &self,
        scope: &dim_agent_runtime::TimelineReadScope,
        query: &dim_agent_runtime::QuerySpec,
    ) -> bool {
        scope.key.branch_id != ROOT_BRANCH_ID
            && query.pick == Some(dim_agent_runtime::QueryPick::Last)
            && matches!(
                &query.select,
                dim_agent_runtime::QuerySelect::Fields { fields }
                    if fields.len() == 1 && fields[0] == "create_at"
            )
            && query
                .filter
                .as_ref()
                .is_some_and(|filter| mentions_kind(filter, "turn/end"))
            && self.armed.swap(false, Ordering::SeqCst)
    }
}

fn mentions_kind(filter: &dim_agent_runtime::QueryFilter, kind: &str) -> bool {
    use dim_agent_runtime::QueryFilter;
    match filter {
        QueryFilter::In { values, .. } => values.iter().any(|value| value.as_str() == Some(kind)),
        QueryFilter::And { conditions } | QueryFilter::Or { conditions } => conditions
            .iter()
            .any(|condition| mentions_kind(condition, kind)),
        QueryFilter::Not { condition } | QueryFilter::Exists { condition } => {
            mentions_kind(condition, kind)
        }
        _ => false,
    }
}

impl dim_agent_runtime::TimelineStore for SummaryHoldStore {
    fn query<'a>(
        &'a self,
        scope: &'a dim_agent_runtime::TimelineReadScope,
        query: &'a dim_agent_runtime::QuerySpec,
        page: Option<&'a dim_agent_runtime::QueryPage>,
    ) -> dim_agent_runtime::BoxFuture<'a, dim_agent_runtime::Result<dim_agent_runtime::QueryResult>>
    {
        Box::pin(async move {
            if self.holds(scope, query) {
                if let Some(entered) = self.entered.lock().unwrap().take() {
                    let _ = entered.send(());
                }
                if let Some(release) = self.release.lock().await.take() {
                    let _ = release.await;
                }
            }
            self.memory.query(scope, query, page).await
        })
    }
    fn create_session<'a>(
        &'a self,
        id: &'a str,
        config: &'a dim_agent_runtime::SessionConfig,
    ) -> dim_agent_runtime::BoxFuture<'a, dim_agent_runtime::Result<()>> {
        self.memory.create_session(id, config)
    }
    fn rollback_session<'a>(
        &'a self,
        id: &'a str,
    ) -> dim_agent_runtime::BoxFuture<'a, dim_agent_runtime::Result<()>> {
        self.memory.rollback_session(id)
    }
    fn release_session<'a>(
        &'a self,
        id: &'a str,
    ) -> dim_agent_runtime::BoxFuture<'a, dim_agent_runtime::Result<()>> {
        self.memory.release_session(id)
    }
    fn session_config<'a>(
        &'a self,
        id: &'a str,
    ) -> dim_agent_runtime::BoxFuture<
        'a,
        dim_agent_runtime::Result<Option<dim_agent_runtime::SessionConfig>>,
    > {
        self.memory.session_config(id)
    }
    fn branches<'a>(
        &'a self,
        id: &'a str,
    ) -> dim_agent_runtime::BoxFuture<
        'a,
        dim_agent_runtime::Result<Vec<dim_agent_runtime::StoredBranch>>,
    > {
        self.memory.branches(id)
    }
    fn head<'a>(
        &'a self,
        key: &'a dim_agent_runtime::TimelineKey,
    ) -> dim_agent_runtime::BoxFuture<'a, dim_agent_runtime::Result<u64>> {
        self.memory.head(key)
    }
    fn append(
        &self,
        key: dim_agent_runtime::TimelineKey,
        events: Vec<dim_agent_runtime::StoredEvent>,
        publish: dim_agent_runtime::PublishCallback,
    ) -> dim_agent_runtime::Result<()> {
        self.memory.append(key, events, publish)
    }
    fn batch_end<'a>(
        &'a self,
        key: &'a dim_agent_runtime::TimelineKey,
        sequence: u64,
    ) -> dim_agent_runtime::BoxFuture<'a, dim_agent_runtime::Result<()>> {
        self.memory.batch_end(key, sequence)
    }
    fn read_segments<'a>(
        &'a self,
        session_id: &'a str,
        segments: Vec<(String, std::ops::RangeInclusive<u64>)>,
    ) -> dim_agent_runtime::BoxFuture<
        'a,
        dim_agent_runtime::Result<Vec<Vec<dim_agent_runtime::StoredEvent>>>,
    > {
        self.memory.read_segments(session_id, segments)
    }
    fn trace<'a>(
        &'a self,
        key: &'a dim_agent_runtime::TimelineKey,
        sequence: u64,
    ) -> dim_agent_runtime::BoxFuture<
        'a,
        dim_agent_runtime::Result<Option<dim_agent_runtime::TimelineTrace>>,
    > {
        self.memory.trace(key, sequence)
    }
    fn save_snapshot<'a>(
        &'a self,
        key: &'a dim_agent_runtime::TimelineKey,
        snapshot: dim_agent_types::RecoverySnapshot,
    ) -> dim_agent_runtime::BoxFuture<'a, dim_agent_runtime::Result<()>> {
        self.memory.save_snapshot(key, snapshot)
    }
    fn latest_snapshot<'a>(
        &'a self,
        key: &'a dim_agent_runtime::TimelineKey,
        through: u64,
        interpreter: &'a str,
    ) -> dim_agent_runtime::BoxFuture<
        'a,
        dim_agent_runtime::Result<Option<dim_agent_types::RecoverySnapshot>>,
    > {
        self.memory.latest_snapshot(key, through, interpreter)
    }
}

/// A storeless listing reads a row's default branch and its activity in more
/// than one read. Another client moving the default back to the root ends the
/// line between them; the listing still answers the row, from the Session's
/// new default (session catalog §3.3).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_storeless_summary_survives_its_default_line_ending_mid_read() {
    let (store, entered, release) = SummaryHoldStore::new();
    let (server, agent) = launch_holding_tool_line_with(ServerConfig::default(), store).await;
    let mut mover = Client::new_version(&server, false, 3).await;
    let mut reader = Client::new_version(&server, false, 3).await;
    let created = reader
        .rpc(
            "session.create",
            json!({
                "cwd": std::env::temp_dir(),
                "tools": ["branch", "wait"],
                "permissionPolicy": {"preset": "full-access"}
            }),
        )
        .await;
    assert_ok(&created);
    let id = created["result"]["sessionId"].as_str().unwrap().to_owned();
    let session = agent.get_session(&id).unwrap();
    while dim_agent_runtime::core::unix_millis() <= session.created_at() {
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    let open = format!(
        "tool:branch {}",
        json!({"action": "create", "prompt": "hold this line"})
    );
    assert_ok(
        &reader
            .rpc(
                "session.submit",
                json!({"sessionId": id, "branchId": "main", "text": open}),
            )
            .await,
    );
    let child = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(branch) = session
                .branches()
                .into_iter()
                .find(|branch| branch.parent.as_deref() == Some("main"))
            {
                return branch.id;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("main opens the tool line");
    tokio::time::timeout(Duration::from_secs(10), async {
        while !session.state(&child).is_ok_and(|state| state.running) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the tool line starts its first turn");
    let opening = read_branch_events(&session, &child, ..=session.sequence(&child).unwrap())
        .await
        .unwrap()
        .first()
        .expect("the line has its opening event")
        .create_at;
    assert_ok(
        &reader
            .rpc(
                "session.branch_set_default",
                json!({"sessionId": id, "branchId": child}),
            )
            .await,
    );
    let reading = tokio::spawn(async move { reader.rpc("session.summaries", json!({})).await });
    tokio::time::timeout(Duration::from_secs(10), entered)
        .await
        .expect("the listing reads the line's activity")
        .unwrap();
    assert_ok(
        &mover
            .rpc(
                "session.branch_set_default",
                json!({"sessionId": id, "branchId": "main"}),
            )
            .await,
    );
    let ended = tokio::time::timeout(
        Duration::from_secs(20),
        mover.rpc(
            "session.branch_end",
            json!({"sessionId": id, "branchId": child}),
        ),
    )
    .await
    .expect("the line ends while the listing holds its activity read");
    assert_ok(&ended);
    // The ended line's notice starts a turn on the root once `branch/ended` is
    // recorded; waiting for it leaves the retried row a settled root.
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let events = read_branch_events(&session, "main", ..).await.unwrap();
            let recorded = events
                .iter()
                .rfind(|event| matches!(event.kind, EventKind::BranchEnded(_)))
                .map(|event| event.sequence);
            if recorded.is_some_and(|recorded| {
                events.iter().any(|event| {
                    event.sequence > recorded && matches!(event.kind, EventKind::TurnEnd(_))
                })
            }) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the ended line's notice turn ends on the root");
    release.send(()).unwrap();
    let summaries = reading.await.unwrap();
    assert_ok(&summaries);
    let row = summaries["result"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["sessionId"] == json!(id))
        .unwrap_or_else(|| panic!("the Session is listed: {summaries}"));
    let activity = activity_time(
        &read_branch_events(&session, "main", ..=session.sequence("main").unwrap())
            .await
            .unwrap(),
    )
    .expect("the root line is active");
    assert!(
        activity > opening,
        "the root's activity follows the line's opening: {activity} vs {opening}"
    );
    assert_eq!(
        row["updatedAt"],
        json!(activity),
        "the row follows the Session's new default, not the line it left"
    );
    drop(mover);
    drop(session);
    drop(agent);
    server.shutdown();
    server.join().await.unwrap();
}

/// A storeless listing decides the selected line's lifecycle in one tree
/// lookup: a concurrent move and end makes it an ended line, so the row takes
/// the bounded re-read from the Session's new default instead of failing the
/// listing as an unknown branch (session catalog §3.3).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_storeless_summary_reclassifies_a_line_ended_mid_lookup() {
    let (server, agent) = launch_holding_tool_line(ServerConfig::default()).await;
    let mut mover = Client::new_version(&server, false, 3).await;
    let mut reader = Client::new_version(&server, false, 3).await;
    let created = reader
        .rpc(
            "session.create",
            json!({
                "cwd": std::env::temp_dir(),
                "tools": ["branch", "wait"],
                "permissionPolicy": {"preset": "full-access"}
            }),
        )
        .await;
    assert_ok(&created);
    let id = created["result"]["sessionId"].as_str().unwrap().to_owned();
    let session = agent.get_session(&id).unwrap();
    while dim_agent_runtime::core::unix_millis() <= session.created_at() {
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    let open = format!(
        "tool:branch {}",
        json!({"action": "create", "prompt": "hold this line"})
    );
    assert_ok(
        &reader
            .rpc(
                "session.submit",
                json!({"sessionId": id, "branchId": "main", "text": open}),
            )
            .await,
    );
    let child = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(branch) = session
                .branches()
                .into_iter()
                .find(|branch| branch.parent.as_deref() == Some("main"))
            {
                return branch.id;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("main opens the tool line");
    tokio::time::timeout(Duration::from_secs(10), async {
        while !session.state(&child).is_ok_and(|state| state.running) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the tool line starts its first turn");
    assert_ok(
        &reader
            .rpc(
                "session.branch_set_default",
                json!({"sessionId": id, "branchId": child}),
            )
            .await,
    );
    let mut held = dim_agent_runtime::testing::hold_lifecycle(&id, &child);
    let reading = tokio::spawn(async move { reader.rpc("session.summaries", json!({})).await });
    let stopped = tokio::time::timeout(Duration::from_secs(10), held.arrived())
        .await
        .expect("the listing reads the line's lifecycle");
    assert_eq!(stopped, child, "the held lookup names the selected line");
    assert_ok(
        &mover
            .rpc(
                "session.branch_set_default",
                json!({"sessionId": id, "branchId": "main"}),
            )
            .await,
    );
    let ended = tokio::time::timeout(
        Duration::from_secs(20),
        mover.rpc(
            "session.branch_end",
            json!({"sessionId": id, "branchId": child}),
        ),
    )
    .await
    .expect("the line ends while the listing holds its lookup");
    assert_ok(&ended);
    // The ended line's notice starts a turn on the root once `branch/ended` is
    // recorded; waiting for it leaves the retried row a settled root.
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let events = read_branch_events(&session, "main", ..).await.unwrap();
            let recorded = events
                .iter()
                .rfind(|event| matches!(event.kind, EventKind::BranchEnded(_)))
                .map(|event| event.sequence);
            if recorded.is_some_and(|recorded| {
                events.iter().any(|event| {
                    event.sequence > recorded && matches!(event.kind, EventKind::TurnEnd(_))
                })
            }) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the ended line's notice turn ends on the root");
    held.release();
    let summaries = reading.await.unwrap();
    assert_ok(&summaries);
    let row = summaries["result"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["sessionId"] == json!(id))
        .unwrap_or_else(|| panic!("the Session is listed: {summaries}"));
    let activity = activity_time(
        &read_branch_events(&session, "main", ..=session.sequence("main").unwrap())
            .await
            .unwrap(),
    )
    .expect("the root line is active");
    assert_eq!(
        row["updatedAt"],
        json!(activity),
        "the row follows the Session's new default after the line ended"
    );
    drop(held);
    drop(mover);
    drop(session);
    drop(agent);
    server.shutdown();
    server.join().await.unwrap();
}

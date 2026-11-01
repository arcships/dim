use super::*;

/// What one model invocation streams: `chunks` thinking fragments, which are
/// one durable event each, or one text fragment of `bytes`.
#[derive(Clone, Copy)]
enum Plan {
    Chunks(usize),
    Huge(usize),
}

/// Streams the next queued plan; a turn without a plan streams one chunk.
#[derive(Default)]
struct StreamingModel(std::sync::Mutex<std::collections::VecDeque<Plan>>);

impl StreamingModel {
    fn push(&self, plan: Plan) {
        self.0.lock().unwrap().push_back(plan);
    }
}

impl dim_agent_capabilities::host::ModelDriver for StreamingModel {
    fn invoke(
        &self,
        _: Arc<dim_agent_types::ModelRequest>,
        _: dim_agent_capabilities::host::ModelConnection,
        _: CancellationToken,
    ) -> HostFuture<'_, dim_agent_capabilities::host::ModelStream> {
        use dim_agent_capabilities::host::{ModelChunk, ModelStopReason, ModelStream};
        let plan = self
            .0
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(Plan::Chunks(1));
        Box::pin(async move {
            let chunks = match plan {
                Plan::Chunks(count) => (0..count)
                    .flat_map(|index| {
                        let chunk = format!("chunk {index} ");
                        [
                            Ok(ModelChunk::Thinking(chunk.clone())),
                            Ok(ModelChunk::ThinkingEnd { thinking: chunk }),
                        ]
                    })
                    .collect(),
                Plan::Huge(bytes) => vec![Ok(ModelChunk::Text("H".repeat(bytes)))],
            };
            let mut stream = vec![Ok(ModelChunk::MessageStart {
                message_id: "stream".into(),
            })];
            stream.extend(chunks);
            stream.push(Ok(ModelChunk::MessageStop {
                message_id: "stream".into(),
                reason: ModelStopReason::EndTurn,
            }));
            Ok(Box::pin(futures_util::stream::iter(stream)) as ModelStream)
        })
    }
}

/// Captures the main branch's Timeline so a flow can commit several events as
/// one atomic batch, which a fixed `through` must not point inside.
#[derive(Default)]
struct BatchProbe(std::sync::Mutex<Option<Arc<dim_agent_runtime::Timeline>>>);

struct BatchProbePlugin(
    Arc<BatchProbe>,
    rutis::host::Key<dim_agent_runtime::Timeline>,
);

impl BatchProbePlugin {
    fn new(names: &dim_agent_runtime::Names<'_>, probe: Arc<BatchProbe>) -> rutis::Result<Self> {
        Ok(Self(probe, names.key()?))
    }
}

impl rutis::host::DynPlugin for BatchProbePlugin {
    fn name(&self) -> &str {
        "history-window-batch-probe"
    }
    fn injects(&self) -> Vec<rutis::host::TypeKey> {
        vec![self.1.erase()]
    }
    fn apply<'a>(
        &'a self,
        ctx: &'a rutis::Ctx,
    ) -> rutis::host::BoxFuture<'a, rutis::Result<rutis::host::Effect>> {
        Box::pin(async move {
            *self.0.0.lock().unwrap() = Some(ctx.require_at(&self.1)?.clone());
            Ok(rutis::host::Effect::Done)
        })
    }
}

async fn serve_streaming(
    directory: &std::path::Path,
    model: Arc<StreamingModel>,
) -> (RunningServer, Arc<BatchProbe>) {
    use dim_agent_capabilities::{
        adapters::DefaultPrepareModelPlugin,
        history::HistoryRenderer,
        host::{
            CompactionStrategyHost, ContextModelHost, IdentityContext, ModelHost,
            PrepareContextHost,
        },
    };
    std::fs::create_dir_all(directory).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let mut builder = AgentBuilder::new(AgentConfig::default());
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
    dim_agent_capabilities::defaults::tool_host(&mut builder, Arc::new(MockTools));
    let batch = Arc::new(BatchProbe::default());
    let probe = batch.clone();
    builder.require_feature(
        Feature::new("history-window-batch-probe")
            .branch("history-window-batch-probe", move |names| {
                Ok(Box::new(BatchProbePlugin::new(names, probe.clone())?))
            }),
    );
    let server = serve(
        builder,
        ServerConfig {
            unix_path: Some(directory.join("server.sock")),
            database_path: Some(directory.join("dimcode.sqlite")),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    (server, batch)
}

/// A short path under `/tmp`: a socket path must stay below `SUN_LEN`, which a
/// runner's `temp_dir` can exceed.
fn new_directory(name: &str) -> std::path::PathBuf {
    std::path::PathBuf::from("/tmp").join(format!("dim-hw-{name}-{}", uuid::Uuid::new_v4()))
}

fn error_name(value: &Value) -> &str {
    value["error"]["data"]["appError"]
        .as_str()
        .or_else(|| value["error"]["message"].as_str())
        .unwrap_or("")
}

fn text_of(event: &Value) -> Option<&str> {
    event["data"]["user"]["content"]
        .as_array()
        .and_then(|content| content.first())
        .and_then(|block| block["text"].as_str())
}

fn sequences(page: &Value) -> Vec<u64> {
    page["result"]["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|event| event["sequence"].as_u64().unwrap())
        .collect()
}

async fn turn(client: &mut Client, model: &StreamingModel, id: &str, text: &str, plan: Plan) {
    model.push(plan);
    assert_ok(
        &client
            .rpc(
                "session.submit",
                json!({"sessionId":id,"branchId":"main","text":text}),
            )
            .await,
    );
    assert_ok(
        &client
            .rpc("session.idle", json!({"sessionId":id,"branchId":"main"}))
            .await,
    );
}

/// Pages a whole window with `latestTurn` and returns the reply sequence and
/// the number of requests, encoded bytes and elapsed milliseconds.
async fn read_window(client: &mut Client, id: &str) -> (Vec<Value>, usize, usize, u128) {
    let started = std::time::Instant::now();
    let mut requests = 0;
    let mut bytes = 0;
    let mut pages = Vec::new();
    let mut before: Option<u64> = None;
    let mut pinned: Option<u64> = None;
    loop {
        let mut params = json!({"sessionId":id,"branchId":"main","latestTurn":true});
        if let Some(before) = before {
            params["before"] = json!(before);
        }
        if let Some(through) = pinned {
            params["through"] = json!(through);
        }
        let page = client.rpc("session.history", params).await;
        assert_ok(&page);
        requests += 1;
        bytes += serde_json::to_vec(&page).unwrap().len();
        pinned = Some(page["result"]["through"].as_u64().unwrap());
        before = page["result"]["nextBefore"].as_u64();
        let done = page["result"]["done"].as_bool().unwrap();
        pages.push(page);
        if done {
            break;
        }
        assert!(requests < 16, "the window must be bounded");
    }
    (pages, requests, bytes, started.elapsed().as_millis())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_latest_turn_window_carries_the_prompt_and_folds_like_forward_paging() {
    let directory = new_directory("small");
    let model = Arc::new(StreamingModel::default());
    let (server, _) = serve_streaming(&directory, model.clone()).await;
    let mut client = Client::new_version(&server, false, 3).await;
    assert!(
        client
            .features
            .as_array()
            .unwrap()
            .contains(&json!("session.history.latestTurn")),
        "{:?}",
        client.features
    );
    let created = client.rpc("session.create", create(json!([]))).await;
    assert_ok(&created);
    let id = created["result"]["sessionId"].as_str().unwrap().to_owned();
    turn(&mut client, &model, &id, "hello window", Plan::Chunks(8)).await;

    let page = client
        .rpc(
            "session.history",
            json!({"sessionId":id,"branchId":"main","latestTurn":true}),
        )
        .await;
    assert_ok(&page);
    assert_eq!(page["result"]["done"], true, "{page}");
    let next_before = page["result"]["nextBefore"].as_u64().unwrap();
    let turn_start = page["result"]["turnStart"].as_u64().unwrap();
    let through = page["result"]["through"].as_u64().unwrap();
    let events = page["result"]["events"].as_array().unwrap();
    let first = &events[0];
    assert_eq!(first["sequence"], next_before);
    assert_eq!(
        (first["type"].as_str(), first["name"].as_str()),
        (Some("input"), Some("message")),
        "{first}"
    );
    assert_eq!(text_of(first), Some("hello window"));
    assert!(
        events.iter().any(|event| event["type"] == "turn"
            && event["name"] == "start"
            && event["sequence"] == turn_start),
        "{page}"
    );
    let forward = client
        .rpc(
            "session.history",
            json!({"sessionId":id,"branchId":"main","after":next_before - 1,
                   "limit":256,"through":through}),
        )
        .await;
    assert_ok(&forward);
    assert_eq!(sequences(&page), sequences(&forward));

    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_latest_turn_window_bounds_a_large_turn_and_continues_with_before() {
    let directory = new_directory("large");
    let model = Arc::new(StreamingModel::default());
    let (server, _) = serve_streaming(&directory, model.clone()).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let created = client.rpc("session.create", create(json!([]))).await;
    assert_ok(&created);
    let id = created["result"]["sessionId"].as_str().unwrap().to_owned();
    // The turn holds more than one page; its first page has no turn/start.
    turn(
        &mut client,
        &model,
        &id,
        "long turn prompt",
        Plan::Chunks(4_300),
    )
    .await;

    let head = client
        .rpc(
            "session.history",
            json!({"sessionId":id,"branchId":"main","latestTurn":true}),
        )
        .await;
    assert_ok(&head);
    assert_eq!(head["result"]["done"], false, "{head}");
    assert!(head["result"]["events"].as_array().unwrap().len() <= 4096);
    let turn_start = head["result"]["turnStart"].as_u64().unwrap();
    let next_before = head["result"]["nextBefore"].as_u64().unwrap();
    assert!(next_before > turn_start, "{head}");
    assert!(
        !head["result"]["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| event["type"] == "turn" && event["name"] == "start"),
        "the newest page alone has no turn/start"
    );

    let rest = client
        .rpc(
            "session.history",
            json!({"sessionId":id,"branchId":"main","latestTurn":true,
                   "before":next_before}),
        )
        .await;
    assert_ok(&rest);
    assert_eq!(rest["result"]["done"], true, "{rest}");
    let rest_sequences = sequences(&rest);
    assert_eq!(rest_sequences[0], rest["result"]["nextBefore"]);
    assert_eq!(
        *rest_sequences.last().unwrap() + 1,
        next_before,
        "the second page ends where the first begins"
    );
    assert_eq!(rest["result"]["turnStart"].as_u64().unwrap(), turn_start);
    assert!(
        rest["result"]["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| event["type"] == "input" && text_of(event) == Some("long turn prompt")),
        "the prompt is in the window"
    );

    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_latest_turn_window_returns_a_batch_above_the_target_and_rejects_a_frame_breaker() {
    let directory = new_directory("bytes");
    let model = Arc::new(StreamingModel::default());
    let (server, _) = serve_streaming(&directory, model.clone()).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let created = client.rpc("session.create", create(json!([]))).await;
    assert_ok(&created);
    let id = created["result"]["sessionId"].as_str().unwrap().to_owned();
    turn(
        &mut client,
        &model,
        &id,
        "big result",
        Plan::Huge(5 * 1024 * 1024),
    )
    .await;
    let (pages, requests, _, _) = read_window(&mut client, &id).await;
    assert_eq!(
        requests, 3,
        "the batch above the target is returned whole, then the prefix"
    );
    assert!(
        pages.iter().any(|page| page["result"]["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| event["data"]["delta"]
                .as_str()
                .is_some_and(|delta| delta.len() == 5 * 1024 * 1024))),
        "the large delta is readable"
    );

    turn(
        &mut client,
        &model,
        &id,
        "frame breaker",
        Plan::Huge(17 * 1024 * 1024),
    )
    .await;
    let first = client
        .rpc(
            "session.history",
            json!({"sessionId":id,"branchId":"main","latestTurn":true}),
        )
        .await;
    assert_ok(&first);
    assert_eq!(first["result"]["done"], false, "{first}");
    let before = first["result"]["nextBefore"].as_u64().unwrap();
    let breaker = client
        .rpc(
            "session.history",
            json!({"sessionId":id,"branchId":"main","latestTurn":true,"before":before}),
        )
        .await;
    assert_eq!(error_name(&breaker), "history_too_large", "{breaker}");

    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_latest_turn_window_measures_the_three_issue_sizes() {
    let directory = new_directory("measure");
    let model = Arc::new(StreamingModel::default());
    let (server, _) = serve_streaming(&directory, model.clone()).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let mut table = Vec::new();
    for chunks in [1_946, 3_112, 6_852] {
        let created = client.rpc("session.create", create(json!([]))).await;
        assert_ok(&created);
        let id = created["result"]["sessionId"].as_str().unwrap().to_owned();
        turn(
            &mut client,
            &model,
            &id,
            &format!("turn of {chunks} chunks"),
            Plan::Chunks(chunks),
        )
        .await;
        let (pages, requests, bytes, millis) = read_window(&mut client, &id).await;
        let head = pages[0]["result"]["through"].as_u64().unwrap();
        assert!(
            head >= chunks as u64 && head < chunks as u64 + 64,
            "head {head} for {chunks} chunks"
        );
        let expected = if head > 4_096 { 2 } else { 1 };
        assert_eq!(
            requests, expected,
            "head {head} needs {expected} requests, got {requests}"
        );
        assert!(bytes < 2 * 4 * 1024 * 1024, "{bytes}");
        eprintln!("head {head}: requests {requests}, encoded {bytes} bytes, {millis} ms");
        table.push((head, requests, bytes, millis));
    }
    assert_eq!(table.len(), 3);

    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_branch_without_a_turn_returns_its_newest_events() {
    let directory = new_directory("noturn");
    let model = Arc::new(StreamingModel::default());
    let (server, _) = serve_streaming(&directory, model.clone()).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let created = client.rpc("session.create", create(json!([]))).await;
    assert_ok(&created);
    let id = created["result"]["sessionId"].as_str().unwrap().to_owned();
    assert_ok(
        &client
            .rpc(
                "session.submit",
                json!({"sessionId":id,"branchId":"main","text":"appended note","mode":"append"}),
            )
            .await,
    );
    let page = client
        .rpc(
            "session.history",
            json!({"sessionId":id,"branchId":"main","latestTurn":true}),
        )
        .await;
    assert_ok(&page);
    assert_eq!(page["result"]["done"], true, "{page}");
    assert!(page["result"].get("turnStart").is_none(), "{page}");
    assert!(
        page["result"]["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| event["type"] == "input" && text_of(event) == Some("appended note")),
        "{page}"
    );

    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(directory).unwrap();
}

/// A pinned `through` must end a batch: the window read answers the storage
/// error the forward read gives, not a page that stops below the pin.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_latest_turn_window_rejects_a_through_inside_a_batch() {
    let directory = new_directory("batch");
    let model = Arc::new(StreamingModel::default());
    let (server, batch) = serve_streaming(&directory, model.clone()).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let created = client.rpc("session.create", create(json!([]))).await;
    assert_ok(&created);
    let id = created["result"]["sessionId"].as_str().unwrap().to_owned();
    let timeline = batch.0.lock().unwrap().clone().unwrap();
    let inside = timeline
        .append_batch_registered(
            ["first in batch", "second in batch"]
                .into_iter()
                .map(|text| {
                    EventKind::InputMessage(dim_agent_types::InputMessageData::text(
                        text,
                        Some(dim_agent_types::InputMode::Append),
                    ))
                    .into()
                })
                .collect(),
            |_| Ok(()),
        )
        .unwrap()[0]
        .sequence;
    assert_ok(
        &client
            .rpc("session.idle", json!({"sessionId":id,"branchId":"main"}))
            .await,
    );

    let forward = client
        .rpc(
            "session.history",
            json!({"sessionId":id,"branchId":"main","through":inside}),
        )
        .await;
    assert_eq!(forward["error"]["code"], -32602, "{forward}");
    assert!(
        forward["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("inside an atomic batch")),
        "{forward}"
    );
    let window = client
        .rpc(
            "session.history",
            json!({"sessionId":id,"branchId":"main","latestTurn":true,"through":inside}),
        )
        .await;
    assert_eq!(window["error"]["code"], -32602, "{window}");
    assert!(
        window["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("inside an atomic batch")),
        "{window}"
    );

    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(directory).unwrap();
}

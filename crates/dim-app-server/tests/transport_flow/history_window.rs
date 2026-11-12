use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn display_windows_and_live_compaction_use_one_typed_detail_with_a_frame_budget() {
    use dim_agent_types::{ContextCompactionData, InputMode};
    let directory = new_directory("display");
    let model = Arc::new(StreamingModel::default());
    let (server, probe) = serve_streaming(&directory, model.clone()).await;
    let mut reader = Client::new_version(&server, false, 3).await;
    assert!(
        reader
            .features
            .as_array()
            .unwrap()
            .contains(&json!("session.history.display"))
    );
    assert!(
        reader
            .features
            .as_array()
            .unwrap()
            .contains(&json!("session.history.detail"))
    );
    let created = reader.rpc("session.create", create(json!([]))).await;
    assert_ok(&created);
    let id = created["result"]["sessionId"].as_str().unwrap();
    let mut v2 = Client::new_version(&server, false, 2).await;
    let unavailable = v2
        .rpc(
            "session.history.display",
            json!({"sessionId":id,"branchId":"main","after":0}),
        )
        .await;
    assert_eq!(unavailable["error"]["code"], -32601);
    drop(v2);
    turn(&mut reader, &model, id, "seed", Plan::Chunks(1)).await;
    let timeline = probe.timeline.lock().unwrap().clone().unwrap();
    let input = timeline
        .append(
            EventKind::InputMessage(dim_agent_types::InputMessageData::text(
                "retained",
                Some(InputMode::Append),
            ))
            .into(),
        )
        .unwrap();
    assert_ok(
        &reader
            .rpc("session.idle", json!({"sessionId":id,"branchId":"main"}))
            .await,
    );
    assert_ok(
        &reader
            .rpc("session.attach", json!({"sessionId":id,"display":true}))
            .await,
    );
    let mut legacy = Client::new_version(&server, false, 3).await;
    assert_ok(&legacy.rpc("session.attach", json!({"sessionId":id})).await);
    let data: ContextCompactionData = serde_json::from_value(json!({
        "through":timeline.sequence(),"status":"completed","retain_from":input.id,
        "replacement":[{"role":"user","content":"中".repeat(800_000)}],
        "strategy":"summary","diagnostics":{
            "reason":"auto","trigger":"threshold","estimatedInputTokens":700_000,
            "entriesReplaced":1,"modelCallCount":1,"durationMs":1,"strategy":{"kind":"summary"}
        },"context_baseline":[]
    }))
    .unwrap();
    let expected = serde_json::to_value(&data).unwrap();
    let event = timeline
        .append(EventKind::ContextCompaction(data).into())
        .unwrap();
    assert_ok(
        &reader
            .rpc("session.idle", json!({"sessionId":id,"branchId":"main"}))
            .await,
    );
    let notice = reader.notice("session.display").await;
    let summary = &notice["params"]["item"];
    assert_eq!(summary["id"], event.id);
    assert_eq!(summary["type"], "display");
    assert_eq!(summary["data"], json!({"strategy":"summary"}));
    assert!(serde_json::to_vec(&notice).unwrap().len() < 1024);
    let original = loop {
        let notice = legacy.notice("session.event").await;
        if notice["params"]["event"]["id"] == event.id {
            break notice;
        }
    };
    assert!(
        original["params"]["event"]["data"] == expected,
        "legacy live content must be complete"
    );
    let params = json!({"sessionId":id,"branchId":"main","through":event.sequence,"latestTurn":true,"limit":1});
    let display = reader.rpc("session.history.display", params.clone()).await;
    assert_ok(&display);
    assert_eq!(display["result"]["items"], json!([summary]));
    let tail = reader
        .rpc(
            "session.history.display",
            json!({
                "sessionId":id,"branchId":"main","tail":true,"through":event.sequence,"limit":1
            }),
        )
        .await;
    assert_ok(&tail);
    assert_eq!(tail["result"]["items"], json!([summary]));
    assert!(tail["result"]["turnStart"].is_null());
    let summary_bytes = serde_json::to_vec(&display).unwrap().len();
    assert!(summary_bytes < 1024);
    let full = legacy.rpc("session.history", params).await;
    assert_ok(&full);
    assert!(
        full["result"]["events"][0]["data"] == expected,
        "legacy window content must be complete"
    );
    for field in ["through", "nextAfter", "nextBefore", "done", "turnStart"] {
        assert_eq!(display["result"][field], full["result"][field], "{field}");
    }
    drop(legacy);
    let detail_started = std::time::Instant::now();
    let reply = reader
        .rpc(
            "session.history.detail",
            json!({"sessionId":id,"branchId":"main","eventId":event.id,"sequence":event.sequence}),
        )
        .await;
    assert_ok(&reply);
    let detail: dim_agent_types::CompactionDetail =
        serde_json::from_value(reply["result"].clone()).unwrap();
    assert_eq!(detail.event_id, event.id);
    assert_eq!(detail.sequence, event.sequence);
    assert_eq!(serde_json::to_value(&detail.data).unwrap(), expected);
    let detail_bytes = serde_json::to_vec(&reply).unwrap().len();
    eprintln!(
        "compaction display: summary={summary_bytes}B, detail={detail_bytes}B/1request/{}ms",
        detail_started.elapsed().as_millis()
    );
    for (event_id, sequence, error) in [
        ("absent", event.sequence, "detail_not_found"),
        (input.id.as_str(), input.sequence, "detail_not_found"),
        (event.id.as_str(), event.sequence + 100, "invalid_params"),
    ] {
        let reply = reader
            .rpc(
                "session.history.detail",
                json!({"sessionId":id,"branchId":"main","eventId":event_id,"sequence":sequence}),
            )
            .await;
        if error == "invalid_params" {
            assert_eq!(reply["error"]["code"], -32602, "{reply}");
        } else {
            assert_eq!(error_name(&reply), error, "{reply}");
        }
    }
    let offset = reader.rpc("session.history.detail",json!({"sessionId":id,"branchId":"main","eventId":event.id,"sequence":event.sequence,"offset":0})).await;
    assert_eq!(offset["error"]["code"], -32602);
    assert_ok(&reader.rpc("session.attach", json!({"sessionId":id})).await);
    let next = timeline
        .append(
            EventKind::ContextCompaction(serde_json::from_value(expected.clone()).unwrap()).into(),
        )
        .unwrap();
    assert_ok(
        &reader
            .rpc("session.idle", json!({"sessionId":id,"branchId":"main"}))
            .await,
    );
    let reverted = loop {
        let notice = reader.notice("session.event").await;
        if notice["params"]["event"]["id"] == next.id {
            break notice;
        }
    };
    assert!(
        reverted["params"]["event"]["data"] == expected,
        "replacement attachment must restore full events"
    );
    assert_ok(
        &reader
            .rpc("session.attach", json!({"sessionId":id,"display":true}))
            .await,
    );
    let mut oversized = expected.clone();
    oversized["replacement"][0]["content"] = json!("x".repeat(17 * 1024 * 1024));
    let oversized = timeline
        .append(EventKind::ContextCompaction(serde_json::from_value(oversized).unwrap()).into())
        .unwrap();
    assert_ok(
        &reader
            .rpc("session.idle", json!({"sessionId":id,"branchId":"main"}))
            .await,
    );
    let rejected=reader.rpc("session.history.detail",json!({"sessionId":id,"branchId":"main","eventId":oversized.id,"sequence":oversized.sequence})).await;
    assert_eq!(error_name(&rejected), "detail_too_large", "{rejected}");
    let summary=reader.rpc("session.history.display",json!({"sessionId":id,"branchId":"main","through":oversized.sequence,"latestTurn":true,"limit":1})).await;
    assert_ok(&summary);
    assert!(serde_json::to_vec(&summary).unwrap().len() < 1024);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn display_tail_pages_inside_a_batch() {
    let directory = new_directory("display-tail");
    let (server, probe) = serve_streaming(&directory, Arc::new(StreamingModel::default())).await;
    let mut client = Client::new_version(&server, false, 3).await;
    assert!(
        client
            .features
            .as_array()
            .unwrap()
            .contains(&json!("session.history.display.tail"))
    );
    let created = client.rpc("session.create", create(json!([]))).await;
    assert_ok(&created);
    let id = created["result"]["sessionId"].as_str().unwrap();
    let timeline = probe.timeline.lock().unwrap().clone().unwrap();
    let batch = timeline
        .append_batch_registered(
            (0..12)
                .map(|i| {
                    EventKind::InputMessage(dim_agent_types::InputMessageData::text(
                        format!("tail-{i}"),
                        Some(dim_agent_types::InputMode::Append),
                    ))
                    .into()
                })
                .collect(),
            |_| Ok(()),
        )
        .unwrap();
    assert_ok(
        &client
            .rpc("session.idle", json!({"sessionId":id,"branchId":"main"}))
            .await,
    );
    let through = batch.last().unwrap().sequence;
    let first = client
        .rpc(
            "session.history.display",
            json!({
                "sessionId":id,"branchId":"main","tail":true,"through":through,"limit":3
            }),
        )
        .await;
    assert_ok(&first);
    assert_eq!(first["result"]["items"].as_array().unwrap().len(), 3);
    assert_eq!(first["result"]["nextBefore"], through - 2);
    assert!(first["result"]["turnStart"].is_null());
    let second = client
        .rpc(
            "session.history.display",
            json!({
                "sessionId":id,"branchId":"main","tail":true,"through":through,
                "before":first["result"]["nextBefore"],"limit":3
            }),
        )
        .await;
    assert_ok(&second);
    assert_eq!(second["result"]["items"].as_array().unwrap().len(), 3);
    assert_eq!(second["result"]["nextBefore"], through - 5);
    for transcript in [false, true] {
        let mut before = None;
        let mut found = Vec::new();
        for _ in 0..4 {
            let mut params = json!({"sessionId":id,"branchId":"main","tail":true,"through":through,"limit":3,"transcript":transcript});
            if let Some(cursor) = before {
                params["before"] = json!(cursor);
            }
            let reply = client.rpc("session.history.display", params).await;
            assert_ok(&reply);
            let page = &reply["result"];
            let next = page["nextBefore"].as_u64().unwrap();
            assert!(before.is_none_or(|before| next < before));
            before = Some(next);
            found.extend(
                page["items"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|event| event["sequence"].as_u64().unwrap()),
            );
        }
        found.sort_unstable();
        assert_eq!(
            found,
            batch.iter().map(|event| event.sequence).collect::<Vec<_>>()
        );
    }
    let expected = client
        .rpc(
            "session.history.display",
            json!({
                "sessionId":id,"branchId":"main","tail":true,"transcript":true,"limit":3
            }),
        )
        .await;
    assert_ok(&expected);
    let hidden = timeline
        .append_batch_registered(
            (0..100)
                .map(|_| {
                    EventKind::Extension(
                        dim_agent_types::ExtensionEvent::new(
                            "test-hidden",
                            "saved",
                            json!({"payload":"not displayed"}),
                        )
                        .unwrap(),
                    )
                    .into()
                })
                .collect(),
            |_| Ok(()),
        )
        .unwrap();
    assert_ok(
        &client
            .rpc("session.idle", json!({"sessionId":id,"branchId":"main"}))
            .await,
    );
    let selected = client
        .rpc(
            "session.history.display",
            json!({
                "sessionId":id,"branchId":"main","tail":true,"transcript":true,
                "through":hidden.last().unwrap().sequence,"limit":3
            }),
        )
        .await;
    assert_ok(&selected);
    assert_eq!(selected["result"]["items"], expected["result"]["items"]);
    let invalid = client
        .rpc(
            "session.history.display",
            json!({
                "sessionId":id,"branchId":"main","tail":true,"latestTurn":true
            }),
        )
        .await;
    assert_eq!(invalid["error"]["code"], -32602);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(directory).unwrap();
}

#[cfg(feature = "testing")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hidden_model_requests_are_not_rebuilt_for_windows_or_forward_pages() {
    use dim_agent_runtime::{
        QueryFilter, QueryOperand, QueryOutput, QueryPage, QueryPick, QuerySelect, QuerySpec,
        TimelineQuery, TimelineQueryScope,
    };
    let directory = new_directory("hidden-request");
    let model = Arc::new(StreamingModel::default());
    let (server, probe) = serve_streaming(&directory, model.clone()).await;
    let storage = server
        .service::<dim_app_server::storage::SessionStore>()
        .unwrap();
    let mut writer = Client::new_version(&server, false, 3).await;
    let created = writer.rpc("session.create", create(json!([]))).await;
    assert_ok(&created);
    let id = created["result"]["sessionId"].as_str().unwrap();
    turn(
        &mut writer,
        &model,
        id,
        &"H".repeat(2 * 1024 * 1024),
        Plan::Chunks(1),
    )
    .await;
    let timeline = probe.timeline.lock().unwrap().as_ref().unwrap().clone();
    let through = timeline.sequence();
    let reply = timeline
        .query(TimelineQuery {
            scope: TimelineQueryScope::Branch,
            from: None,
            through: Some(through),
            include_publishing: false,
            query: QuerySpec {
                filter: Some(QueryFilter::In {
                    value: QueryOperand::Field {
                        path: "kind".into(),
                    },
                    values: vec!["model/call".into()],
                }),
                select: QuerySelect::Fields {
                    fields: vec!["position.sequence".into()],
                },
                group_by: vec![],
                pick: Some(QueryPick::Last),
                order_by: vec![],
            },
            page: Some(QueryPage {
                limit: std::num::NonZeroU64::MIN,
                after: None,
            }),
        })
        .await
        .unwrap();
    let QueryOutput::Rows { rows } = reply.result else {
        panic!("the model call position")
    };
    let dim_agent_runtime::QueryValue::Value { value } = &rows[0].values[0] else {
        panic!("the call sequence")
    };
    let call = value.as_u64().unwrap();
    let mut reader = Client::new_version(&server, false, 3).await;
    assert_ok(&reader.rpc("session.attach", json!({"sessionId":id})).await);
    for params in [
        json!({"sessionId":id,"branchId":"main","through":through,"before":call+1,"limit":1}),
        json!({"sessionId":id,"branchId":"main","through":call,"after":call-1,"limit":1}),
    ] {
        let backward = params.get("before").is_some();
        let _ = storage.timeline_diagnostics().take_node_loads();
        let page = reader.rpc("session.history", params).await;
        assert_ok(&page);
        assert!(page["result"]["events"].as_array().unwrap().is_empty());
        assert_eq!(page["result"]["nextAfter"], call);
        assert_eq!(page["result"]["done"], !backward);
        if backward {
            assert_eq!(page["result"]["nextBefore"], call);
        }
        assert_eq!(
            storage.timeline_diagnostics().take_node_loads().loads,
            0,
            "a hidden request must not load stored message nodes"
        );
    }
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(directory).unwrap();
}

/// What one model invocation streams: `chunks` thinking fragments, which are
/// one durable event each, or one text fragment of `bytes`.
#[derive(Clone, Copy)]
pub(super) enum Plan {
    Chunks(usize),
    Huge(usize),
}

/// Streams the next queued plan; a turn without a plan streams one chunk.
#[derive(Default)]
pub(super) struct StreamingModel(std::sync::Mutex<std::collections::VecDeque<Plan>>);

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

/// Captures the current branch's Timeline for atomic batches and can hold
/// the next thinking fact before dispatch completes.
#[derive(Default)]
pub(super) struct BatchProbe {
    timeline: std::sync::Mutex<Option<Arc<dim_agent_runtime::Timeline>>>,
    hold: std::sync::atomic::AtomicBool,
    entered: tokio::sync::Notify,
    resume: tokio::sync::Notify,
}

struct BatchProbePlugin(
    Arc<BatchProbe>,
    rutis::host::Key<dim_agent_runtime::Timeline>,
    rutis::host::Key<dim_agent_runtime::Dispatcher>,
);

impl BatchProbePlugin {
    fn new(names: &dim_agent_runtime::Names<'_>, probe: Arc<BatchProbe>) -> rutis::Result<Self> {
        Ok(Self(probe, names.key()?, names.key()?))
    }
}

impl rutis::host::DynPlugin for BatchProbePlugin {
    fn name(&self) -> &str {
        "history-window-batch-probe"
    }
    fn injects(&self) -> Vec<rutis::host::TypeKey> {
        vec![self.1.erase(), self.2.erase()]
    }
    fn apply<'a>(
        &'a self,
        ctx: &'a rutis::Ctx,
    ) -> rutis::host::BoxFuture<'a, rutis::Result<rutis::host::Effect>> {
        Box::pin(async move {
            *self.0.timeline.lock().unwrap() = Some(ctx.require_at(&self.1)?.clone());
            let probe = self.0.clone();
            ctx.require_at(&self.2)?.subscribe(
                ctx,
                "history-window-dispatch-gate",
                &["step/thinking"],
                move |_| {
                    let probe = probe.clone();
                    Box::pin(async move {
                        if probe.hold.swap(false, Ordering::SeqCst) {
                            probe.entered.notify_one();
                            probe.resume.notified().await;
                        }
                        Ok(())
                    })
                },
            )?;
            Ok(rutis::host::Effect::Done)
        })
    }
}

pub(super) async fn serve_streaming(
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
pub(super) fn new_directory(name: &str) -> std::path::PathBuf {
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

pub(super) async fn turn(
    client: &mut Client,
    model: &StreamingModel,
    id: &str,
    text: &str,
    plan: Plan,
) {
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
    let mut client = Client::new_version(&server, false, 4).await;
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
    assert!(events.iter().any(|event| {
        text_of(event) == Some("hello window") && event["sequence"].as_u64().unwrap() < turn_start
    }));
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
async fn a_latest_turn_waiting_for_dispatch_survives_the_line_ending() {
    let directory = new_directory("ended");
    let model = Arc::new(StreamingModel::default());
    let (server, batch) = serve_streaming(&directory, model.clone()).await;
    let mut client = Client::new_version(&server, false, 4).await;
    let created = client
        .rpc("session.create", create(json!(["branch"])))
        .await;
    assert_ok(&created);
    let id = created["result"]["sessionId"].as_str().unwrap().to_owned();
    turn(&mut client, &model, &id, "parent prompt", Plan::Chunks(1)).await;
    let parent = client
        .rpc(
            "session.history",
            json!({"sessionId":id,"branchId":"main","after":0,"limit":1}),
        )
        .await;
    assert_ok(&parent);
    let at = parent["result"]["through"].as_u64().unwrap();
    batch.hold.store(true, Ordering::SeqCst);
    let created = client
        .rpc(
            "session.branch_create",
            json!({"sessionId":id,
        "parentBranchId":"main","at":at,"prompt":"child prompt"}),
        )
        .await;
    assert_ok(&created);
    let branch = created["result"]["branch"]["branchId"].as_str().unwrap();
    tokio::time::timeout(Duration::from_secs(10), batch.entered.notified())
        .await
        .expect("the streaming turn reaches the dispatch gate");
    let timeline = batch.timeline.lock().unwrap().clone().unwrap();
    let mut reader = Client::new_version(&server, false, 4).await;
    let params = json!({"sessionId":id,"branchId":branch,"latestTurn":true});
    let waiting = reader.send("session.history", params.clone()).await;
    let future = reader
        .rpc(
            "session.history",
            json!({"sessionId":id,"branchId":branch,
        "latestTurn":true,"through":timeline.sequence()}),
        )
        .await;
    assert_eq!(
        future["error"]["message"], "Future dispatched history position",
        "{future}"
    );
    assert!(
        !reader.notices.iter().any(|reply| reply["id"] == waiting),
        "the history call must remain pending behind dispatch"
    );
    let end_query = dim_agent_runtime::TimelineQuery {
        scope: dim_agent_runtime::TimelineQueryScope::Branch,
        include_publishing: false,
        from: None,
        through: None,
        query: dim_agent_runtime::QuerySpec {
            filter: Some(dim_agent_runtime::QueryFilter::And {
                conditions: vec![
                    dim_agent_runtime::QueryFilter::In {
                        value: dim_agent_runtime::QueryOperand::Field {
                            path: "kind".into(),
                        },
                        values: vec!["branch/end".into()],
                    },
                    dim_agent_runtime::QueryFilter::Compare {
                        left: dim_agent_runtime::QueryOperand::Field {
                            path: "position.sequence".into(),
                        },
                        comparison: dim_agent_runtime::QueryComparison::Gt,
                        right: dim_agent_runtime::QueryOperand::Value {
                            value: timeline.sequence().into(),
                        },
                    },
                ],
            }),
            select: dim_agent_runtime::QuerySelect::Exists,
            group_by: vec![],
            pick: None,
            order_by: vec![],
        },
        page: None,
    };
    let ending = client
        .send(
            "session.branch_end",
            json!({"sessionId":id,"branchId":branch}),
        )
        .await;
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if matches!(
                timeline.query(end_query.clone()).await.unwrap().result,
                dim_agent_runtime::QueryOutput::Exists { exists: true }
            ) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the end is committed while history waits");
    batch.resume.notify_one();
    let open = reader.response(waiting).await;
    assert_ok(&open);
    assert_ok(&client.response(ending).await);
    let ended = client.rpc("session.history", params.clone()).await;
    assert_ok(&ended);
    assert_eq!(ended["result"]["turnStart"], open["result"]["turnStart"]);
    assert!(
        ended["result"]["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| text_of(event) == Some("child prompt")),
        "{ended}"
    );
    assert!(
        ended["result"]["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| event["type"] == "branch" && event["name"] == "end"),
        "{ended}"
    );
    let mut pinned = params;
    pinned["through"] = open["result"]["through"].clone();
    let pinned = client.rpc("session.history", pinned).await;
    assert_ok(&pinned);
    assert_eq!(pinned["result"], open["result"]);
    let late = client
        .rpc(
            "session.submit",
            json!({"sessionId":id,"branchId":branch,"text":"late"}),
        )
        .await;
    assert_eq!(error_name(&late), "branch_ended");
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_latest_turn_window_bounds_a_large_turn_and_continues_with_before() {
    let directory = new_directory("large");
    let model = Arc::new(StreamingModel::default());
    let (server, _) = serve_streaming(&directory, model.clone()).await;
    let mut client = Client::new_version(&server, false, 4).await;
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
    let mut client = Client::new_version(&server, false, 4).await;
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
    let mut client = Client::new_version(&server, false, 4).await;
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
    let mut client = Client::new_version(&server, false, 4).await;
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
    let mut client = Client::new_version(&server, false, 4).await;
    let created = client.rpc("session.create", create(json!([]))).await;
    assert_ok(&created);
    let id = created["result"]["sessionId"].as_str().unwrap().to_owned();
    let timeline = batch.timeline.lock().unwrap().clone().unwrap();
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

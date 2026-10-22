use super::*;

/// v3 exposes Agents: parallel Agents isolate their Sessions, agent.list
/// reports them and session.list routes by agentId.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn parallel_agents_isolate_sessions_and_listing() {
    for ws in [false, true] {
        let server = launch(Arc::new(MockTools), ServerConfig::default()).await;
        let mut client = Client::new_version(&server, ws, 3).await;

        let first = client.rpc("agent.create", json!({})).await;
        assert_ok(&first);
        let first = first["result"]["agentId"].as_str().unwrap().to_owned();
        let second = client.rpc("agent.create", json!({})).await;
        assert_ok(&second);
        let second = second["result"]["agentId"].as_str().unwrap().to_owned();
        assert_ne!(first, second);

        let mut params = create(json!([]));
        params["agentId"] = json!(first);
        let created = client.rpc("session.create", params).await;
        assert_ok(&created);
        let first_session = created["result"]["sessionId"].as_str().unwrap().to_owned();
        let mut params = create(json!([]));
        params["agentId"] = json!(second);
        let created = client.rpc("session.create", params).await;
        assert_ok(&created);
        let second_session = created["result"]["sessionId"].as_str().unwrap().to_owned();

        assert_ok(
            &client
                .rpc(
                    "session.submit",
                    json!({"sessionId":first_session,"branchId":"main","text":"one"}),
                )
                .await,
        );
        assert_ok(
            &client
                .rpc(
                    "session.submit",
                    json!({"sessionId":second_session,"branchId":"main","text":"two"}),
                )
                .await,
        );

        let listed = client.rpc("agent.list", json!({})).await;
        let agents = listed["result"].as_array().unwrap();
        assert_eq!(agents.len(), 3);
        let first_info = agents
            .iter()
            .find(|info| info["agentId"] == first.as_str())
            .unwrap();
        assert_eq!(first_info["default"], false);
        assert_eq!(first_info["sessions"], 1);
        let default_info = agents.iter().find(|info| info["default"] == true).unwrap();
        assert_eq!(default_info["sessions"], 0);
        assert_ne!(default_info["agentId"], first.as_str());

        // Unfiltered v3 lists every Agent's Sessions with their owner.
        let listed = client.rpc("session.list", json!({})).await;
        let sessions = listed["result"].as_array().unwrap();
        assert_eq!(sessions.len(), 2);
        for session in sessions {
            let owner = session["agentId"].as_str().unwrap();
            assert!(owner == first || owner == second);
        }
        // Filtering narrows to one Agent; v1 keeps the default-Agent view.
        let listed = client.rpc("session.list", json!({"agentId":first})).await;
        let sessions = listed["result"].as_array().unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0]["agentId"], first.as_str());
        assert_eq!(sessions[0]["sessionId"], first_session.as_str());

        let ping = client.rpc("daemon.ping", json!({})).await;
        assert_eq!(ping["result"]["sessions"], 2);

        server.shutdown();
        server.join().await.unwrap();
    }
}

/// agent.close settles that Agent's Sessions and leaves other Agents working;
/// unknown or repeated closes report agent_unknown.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn agent_close_settles_sessions_and_leaves_other_agents() {
    let server = launch(Arc::new(MockTools), ServerConfig::default()).await;
    let mut client = Client::new_version(&server, false, 3).await;

    let first = client.rpc("agent.create", json!({})).await;
    let first = first["result"]["agentId"].as_str().unwrap().to_owned();
    let second = client.rpc("agent.create", json!({})).await;
    let second = second["result"]["agentId"].as_str().unwrap().to_owned();

    let mut params = create(json!([]));
    params["agentId"] = json!(first);
    let closed_session = client.rpc("session.create", params).await;
    let closed_session = closed_session["result"]["sessionId"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut params = create(json!([]));
    params["agentId"] = json!(second);
    let kept_session = client.rpc("session.create", params).await;
    let kept_session = kept_session["result"]["sessionId"]
        .as_str()
        .unwrap()
        .to_owned();

    // Attach so the closing Session reports session.closed to this client.
    assert_ok(
        &client
            .rpc("session.attach", json!({"sessionId":closed_session}))
            .await,
    );

    assert_ok(&client.rpc("agent.close", json!({"agentId":first})).await);
    let closed = client.notice("session.closed").await;
    assert_eq!(closed["params"]["sessionId"], closed_session.as_str());

    let error = client
        .rpc(
            "session.submit",
            json!({"sessionId":closed_session,"branchId":"main","text":"late"}),
        )
        .await;
    assert_eq!(error["error"]["data"]["appError"], "session_closed");

    let listed = client.rpc("agent.list", json!({})).await;
    let agents = listed["result"].as_array().unwrap();
    assert_eq!(agents.len(), 2);
    assert!(agents.iter().all(|info| info["agentId"] != first.as_str()));

    let repeated = client.rpc("agent.close", json!({"agentId":first})).await;
    assert_eq!(repeated["error"]["data"]["appError"], "agent_unknown");
    let unknown = client
        .rpc("agent.close", json!({"agentId":"missing"}))
        .await;
    assert_eq!(unknown["error"]["data"]["appError"], "agent_unknown");

    // The default Agent is the v1/v2 compatibility target; it stays closable
    // only through an Agents restart.
    let default_agent = client.rpc("agent.list", json!({})).await["result"]
        .as_array()
        .unwrap()
        .iter()
        .find(|info| info["default"] == true)
        .unwrap()["agentId"]
        .as_str()
        .unwrap()
        .to_owned();
    let refused = client
        .rpc("agent.close", json!({"agentId":default_agent}))
        .await;
    assert_eq!(refused["error"]["data"]["appError"], "agent_default");

    assert_ok(
        &client
            .rpc(
                "session.submit",
                json!({"sessionId":kept_session,"branchId":"main","text":"still here"}),
            )
            .await,
    );

    server.shutdown();
    server.join().await.unwrap();
}

/// v1 and v2 clients keep the single default-Agent view; only v3 addresses
/// Agents explicitly.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn legacy_versions_stay_on_the_default_agent() {
    let server = launch(Arc::new(MockTools), ServerConfig::default()).await;

    let mut first = Client::new_version(&server, false, 1).await;
    let created = first.rpc("session.create", create(json!([]))).await;
    assert_ok(&created);
    let legacy_session = created["result"]["sessionId"].as_str().unwrap().to_owned();
    assert_ok(
        &first
            .rpc(
                "session.submit",
                json!({"sessionId":legacy_session,"text":"v1"}),
            )
            .await,
    );
    let listed = first.rpc("session.list", json!({})).await;
    let sessions = listed["result"].as_array().unwrap();
    assert_eq!(sessions.len(), 1);
    assert!(sessions[0].get("agentId").is_none());
    let rejected = first.rpc("agent.create", json!({})).await;
    assert_eq!(rejected["error"]["data"]["appError"], "unknown_method");
    let mut with_agent = create(json!([]));
    with_agent["agentId"] = json!("anything");
    let rejected = first.rpc("session.create", with_agent).await;
    assert_eq!(rejected["error"]["code"], -32602);

    let mut second = Client::new_version(&server, false, 2).await;
    let created = second.rpc("session.create", create(json!([]))).await;
    assert_ok(&created);

    let mut third = Client::new_version(&server, false, 3).await;
    let created = third.rpc("session.create", create(json!([]))).await;
    assert_ok(&created);
    let listed = third.rpc("agent.list", json!({})).await;
    let default_info = listed["result"]
        .as_array()
        .unwrap()
        .iter()
        .find(|info| info["default"] == true)
        .unwrap();
    // v3 without agentId and both legacy Sessions belong to the default Agent.
    assert_eq!(default_info["sessions"], 3);
    let mut params = create(json!([]));
    params["agentId"] = json!("missing");
    let unknown = third.rpc("session.create", params).await;
    assert_eq!(unknown["error"]["data"]["appError"], "agent_unknown");

    server.shutdown();
    server.join().await.unwrap();
}

/// Restarting `Agents` reloads the directory only: every Agent keeps
/// running with its Sessions, and the protocol instance and connections stay.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn agents_reload_keeps_every_agent() {
    let server = launch(Arc::new(MockTools), ServerConfig::default()).await;
    let mut client = Client::new_version(&server, false, 3).await;

    let extra = client.rpc("agent.create", json!({})).await;
    let extra = extra["result"]["agentId"].as_str().unwrap().to_owned();
    let mut params = create(json!([]));
    params["agentId"] = json!(extra);
    assert_ok(&client.rpc("session.create", params).await);

    server.restart("Agents").await.unwrap();

    let listed = client.rpc("agent.list", json!({})).await;
    let agents = listed["result"].as_array().unwrap();
    assert_eq!(agents.len(), 2);
    let kept = agents
        .iter()
        .find(|agent| agent["agentId"] == extra)
        .unwrap();
    assert_eq!(kept["sessions"], 1);
    let fresh = Client::new_version(&server, false, 3).await;
    assert_eq!(fresh.instance, client.instance);

    server.shutdown();
    server.join().await.unwrap();
}

async fn launch_with_plugin<P: Capability>(
    new: impl Fn() -> P + Send + Sync + 'static,
) -> RunningServer {
    let mut builder = AgentBuilder::new(AgentConfig::default());
    defaults::install(&mut builder);
    defaults::mock_model_hosts(&mut builder);
    dim_agent_capabilities::defaults::tool_host(&mut builder, Arc::new(MockTools));
    builder.session_plugin(move |_| new());
    let config = ServerConfig {
        unix_path: Some(std::path::PathBuf::from("/tmp").join(format!(
            "dim-agent-agents-test-{}/server.sock",
            uuid::Uuid::new_v4()
        ))),
        ..ServerConfig::default()
    };
    serve(builder, config).await.unwrap()
}

/// A session.close racing an agent.close waits for the existing settlement.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_close_waits_for_a_concurrent_agent_close_settlement() {
    let gate = std::sync::Arc::new(tokio::sync::Notify::new());
    let fail = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let server = launch_with_plugin({
        let gate = gate.clone();
        let fail = fail.clone();
        move || GatedCleanup {
            gate: gate.clone(),
            fail: fail.clone(),
        }
    })
    .await;
    let mut client = Client::new_version(&server, false, 3).await;

    let agent = client.rpc("agent.create", json!({})).await;
    assert_ok(&agent);
    let agent = agent["result"]["agentId"].as_str().unwrap().to_owned();
    let mut params = create(json!([]));
    params["agentId"] = json!(agent);
    let created = client.rpc("session.create", params).await;
    assert_ok(&created);
    let sid = created["result"]["sessionId"].as_str().unwrap().to_owned();

    // agent.close settles through the gated cleanup; session.close races it
    // from inside the closing window, after the Agent left the live index.
    let closing = client.send("agent.close", json!({"agentId":agent})).await;
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let listed = client.rpc("agent.list", json!({})).await;
        let gone = listed["result"]
            .as_array()
            .unwrap()
            .iter()
            .all(|info| info["agentId"] != agent.as_str());
        if gone {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "agent.close did not remove the Agent from the live index"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let session_close = client.send("session.close", json!({"sessionId":sid})).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(300), client.response(session_close))
            .await
            .is_err(),
        "session.close must wait for the Agent's settlement"
    );

    gate.notify_one();
    let session_reply = client.response(session_close).await;
    assert_ok(&session_reply);
    assert_ok(&client.response(closing).await);

    server.shutdown();
    server.join().await.unwrap();
}

/// Cleanup failures keep their details in the server log and return a fixed
/// outbound message; the Agent is gone and later Agents close cleanly.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn agent_close_cleanup_failure_returns_a_fixed_outbound_message() {
    let fail = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let server = launch_with_plugin({
        let fail = fail.clone();
        move || super::reload::FailingCleanup(fail.clone())
    })
    .await;
    let mut client = Client::new_version(&server, false, 3).await;

    let agent = client.rpc("agent.create", json!({})).await;
    assert_ok(&agent);
    let agent = agent["result"]["agentId"].as_str().unwrap().to_owned();
    let mut params = create(json!([]));
    params["agentId"] = json!(agent);
    assert_ok(&client.rpc("session.create", params).await);

    let failed = client.rpc("agent.close", json!({"agentId":agent})).await;
    assert_eq!(failed["error"]["code"], -32000);
    // Fixed outbound text: no internal paths or cleanup details leak.
    assert_eq!(failed["error"]["message"], "Agent unload failed");
    assert!(failed["error"].get("data").is_none());

    // The Agent is gone.
    let listed = client.rpc("agent.list", json!({})).await;
    let agents = listed["result"].as_array().unwrap();
    assert!(agents.iter().all(|info| info["agentId"] != agent.as_str()));

    // A healthy Agent closes cleanly afterwards.
    fail.store(false, std::sync::atomic::Ordering::SeqCst);
    let next = client.rpc("agent.create", json!({})).await;
    assert_ok(&next);
    let next = next["result"]["agentId"].as_str().unwrap().to_owned();
    assert_ok(&client.rpc("agent.close", json!({"agentId":next})).await);

    server.shutdown();
    server.join().await.unwrap();
}

/// Holds Session settlement open until released, then fails cleanup while
/// the flag is set; a never-set flag covers the clean-settlement case.
struct GatedCleanup {
    gate: std::sync::Arc<tokio::sync::Notify>,
    fail: std::sync::Arc<std::sync::atomic::AtomicBool>,
}
impl TypedPlugin for GatedCleanup {
    type Deps = ();
    fn name(&self) -> &'static str {
        "test.gated-cleanup"
    }
    fn apply<'a>(
        &'a self,
        ctx: &'a rutis::Ctx,
        (): Self::Deps,
    ) -> rutis::BoxFuture<'a, Result<rutis::Effect, rutis::CordisError>> {
        let gate = self.gate.clone();
        let fail = self.fail.clone();
        let result = ctx.effect(move || {
            rutis::Effect::AsyncDisposer(Box::new(move || {
                let gate = gate.clone();
                let fail = fail.clone();
                Box::pin(async move {
                    gate.notified().await;
                    if fail.load(std::sync::atomic::Ordering::SeqCst) {
                        Err(rutis::CordisError::PluginFailed("cleanup".into()))
                    } else {
                        Ok(())
                    }
                })
            }))
        });
        Box::pin(async move { result.map(|_| rutis::Effect::Done) })
    }
}
impl Capability for GatedCleanup {
    fn keys(&self) -> KeysOf<Self> {}
}

/// A session.close inside the closing window reports the settlement failure
/// with the same fixed message as the direct close path.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_close_reports_the_settlement_failure_of_a_concurrent_agent_close() {
    let gate = std::sync::Arc::new(tokio::sync::Notify::new());
    let fail = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let server = launch_with_plugin({
        let gate = gate.clone();
        let fail = fail.clone();
        move || GatedCleanup {
            gate: gate.clone(),
            fail: fail.clone(),
        }
    })
    .await;
    let mut client = Client::new_version(&server, false, 3).await;

    let agent = client.rpc("agent.create", json!({})).await;
    assert_ok(&agent);
    let agent = agent["result"]["agentId"].as_str().unwrap().to_owned();
    let mut params = create(json!([]));
    params["agentId"] = json!(agent);
    let created = client.rpc("session.create", params).await;
    assert_ok(&created);
    let sid = created["result"]["sessionId"].as_str().unwrap().to_owned();

    let closing = client.send("agent.close", json!({"agentId":agent})).await;
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let listed = client.rpc("agent.list", json!({})).await;
        let gone = listed["result"]
            .as_array()
            .unwrap()
            .iter()
            .all(|info| info["agentId"] != agent.as_str());
        if gone {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "agent.close did not remove the Agent from the live index"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let session_close = client.send("session.close", json!({"sessionId":sid})).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(300), client.response(session_close))
            .await
            .is_err(),
        "session.close must wait for the Agent's settlement"
    );

    // The gated settlement then fails: both closes report fixed messages.
    gate.notify_one();
    let session_reply = client.response(session_close).await;
    assert_eq!(session_reply["error"]["code"], -32000);
    assert_eq!(session_reply["error"]["message"], "Session cleanup failed");
    let agent_reply = client.response(closing).await;
    assert_eq!(agent_reply["error"]["message"], "Agent unload failed");

    server.shutdown();
    server.join().await.unwrap();
}

/// Echoes like the mock model and keeps each request for inspection.
#[derive(Default)]
struct RecordingModel(std::sync::Mutex<Vec<Arc<dim_agent_types::ModelRequest>>>);
impl dim_agent_capabilities::host::ModelDriver for RecordingModel {
    fn invoke(
        &self,
        request: Arc<dim_agent_types::ModelRequest>,
        _: dim_agent_capabilities::host::ModelConnection,
        cancel: CancellationToken,
    ) -> HostFuture<'_, dim_agent_capabilities::host::ModelStream> {
        self.0.lock().unwrap().push(request.clone());
        dim_agent_capabilities::adapters::EchoModel.invoke(
            request,
            dim_agent_capabilities::host::ModelConnection::Local,
            cancel,
        )
    }
}
impl RecordingModel {
    /// System prompt, sorted tool names and model id of the latest request.
    fn last(&self) -> (Value, Vec<String>, String) {
        let request = serde_json::to_value(&**self.0.lock().unwrap().last().unwrap()).unwrap();
        let mut tools: Vec<String> = request["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap().to_owned())
            .collect();
        tools.sort();
        let model = request["modelId"].as_str().unwrap().to_owned();
        (request["system"].clone(), tools, model)
    }
}

/// An Agent profile supplies the Session defaults of that Agent: prompt,
/// tools and model apply unless session.create gives them; agent.list shows
/// its name and description; the default Agent and a profile-less
/// agent.create use the process default profile; a profile naming a tool
/// the process does not provide, or a tool twice, is rejected.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn agent_profile_supplies_session_defaults() {
    use dim_agent_capabilities::{
        adapters::DefaultPrepareModelPlugin,
        history::HistoryRenderer,
        host::{
            CompactionStrategyHost, ContextModelHost, IdentityContext, ModelHost,
            PrepareContextHost,
        },
    };
    let model = Arc::new(RecordingModel::default());
    let mut builder = AgentBuilder::new(AgentConfig::default());
    defaults::install(&mut builder);
    builder
        .session_value(Arc::new(HistoryRenderer::default()))
        .session_value(Arc::new(ModelHost(model.clone())))
        .session_value(Arc::new(PrepareContextHost(Arc::new(IdentityContext))))
        .session_value(Arc::new(ContextModelHost(None)))
        .session_value(Arc::new(CompactionStrategyHost(None)))
        .session_plugin(DefaultPrepareModelPlugin::new);
    dim_agent_capabilities::defaults::tool_host(&mut builder, Arc::new(MockTools));
    let config = ServerConfig {
        unix_path: Some(std::path::PathBuf::from("/tmp").join(format!(
            "dim-agent-profile-test-{}/server.sock",
            uuid::Uuid::new_v4()
        ))),
        ..ServerConfig::default()
    };
    let server = serve(builder, config).await.unwrap();
    let mut client = Client::new_version(&server, false, 3).await;
    async fn turn(client: &mut Client, params: Value) -> Value {
        let created = client.rpc("session.create", params).await;
        assert_ok(&created);
        let session = created["result"]["sessionId"].clone();
        assert_ok(
            &client
                .rpc(
                    "session.submit",
                    json!({"sessionId":session,"branchId":"main","text":"hi"}),
                )
                .await,
        );
        assert_ok(
            &client
                .rpc(
                    "session.idle",
                    json!({"sessionId":session,"branchId":"main"}),
                )
                .await,
        );
        created
    }

    let agent = client
        .rpc(
            "agent.create",
            json!({"profile":{
                "name":"reviewer",
                "description":"Reviews changes",
                "systemPrompt":"You review.",
                "tools":["echo"],
                "model":{"providerId":"mock","modelId":"profiled"}
            }}),
        )
        .await;
    assert_ok(&agent);
    let agent = agent["result"]["agentId"].clone();

    let listed = client.rpc("agent.list", json!({})).await;
    let agents = listed["result"].as_array().unwrap();
    assert_eq!(agents.len(), 2);
    let info = agents.iter().find(|info| info["agentId"] == agent).unwrap();
    assert_eq!(info["name"], "reviewer");
    assert_eq!(info["description"], "Reviews changes");
    assert_eq!(info["sessions"], 0);
    let default = agents.iter().find(|info| info["default"] == true).unwrap();
    assert_eq!(default["name"], "default");
    assert_eq!(default["description"], "");

    let cwd = std::env::temp_dir();
    let created = turn(&mut client, json!({"cwd":cwd,"agentId":agent})).await;
    assert_eq!(created["result"]["model"]["modelId"], "profiled");
    assert_eq!(
        model.last(),
        (
            json!(format!(
                "You review.\n\n{}",
                include_str!("../../../dim-agent-capabilities/src/input-guide.txt").trim_end()
            )),
            vec!["echo".into()],
            "profiled".into()
        )
    );

    // Explicit request values win over the profile.
    let created = turn(
        &mut client,
        json!({"cwd":cwd,"agentId":agent,"systemPrompt":"Explicit.","tools":["wait"],"model":{"modelId":"explicit"}}),
    )
    .await;
    assert_eq!(created["result"]["model"]["providerId"], "mock");
    assert_eq!(created["result"]["model"]["modelId"], "explicit");
    assert_eq!(
        model.last(),
        (
            json!(format!(
                "Explicit.\n\n{}",
                include_str!("../../../dim-agent-capabilities/src/input-guide.txt").trim_end()
            )),
            vec!["wait".into()],
            "explicit".into()
        )
    );

    // The default Agent and a profile-less Agent use the process defaults.
    let defaults = ServerConfig::default().session_defaults;
    let mut tools = defaults.tools.clone();
    tools.sort();
    let expected = (
        json!(include_str!("../../../dim-agent-capabilities/src/input-guide.txt").trim_end()),
        tools,
        defaults.model.model_id.clone(),
    );
    turn(&mut client, json!({"cwd":cwd})).await;
    assert_eq!(model.last(), expected);
    let plain = client.rpc("agent.create", json!({})).await;
    assert_ok(&plain);
    turn(
        &mut client,
        json!({"cwd":cwd,"agentId":plain["result"]["agentId"]}),
    )
    .await;
    assert_eq!(model.last(), expected);

    // Profile tools must be provided by the process: host or contribution.
    let rejected = client
        .rpc(
            "agent.create",
            json!({"profile":{"name":"broken","description":"","systemPrompt":"","tools":["branch","missing"]}}),
        )
        .await;
    assert_eq!(rejected["error"]["data"]["appError"], "agent_build_failed");
    let duplicated = client
        .rpc(
            "agent.create",
            json!({"profile":{"name":"twice","description":"","systemPrompt":"","tools":["echo","echo"]}}),
        )
        .await;
    assert_eq!(
        duplicated["error"]["data"]["appError"],
        "agent_build_failed"
    );
    let listed = client.rpc("agent.list", json!({})).await;
    assert_eq!(listed["result"].as_array().unwrap().len(), 3);

    server.shutdown();
    server.join().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn product_session_model_priority_is_request_then_profile_then_default() {
    let home = std::env::temp_dir().join(format!("dim-profile-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&home).unwrap();
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
    let capabilities = dim_agent_types::ModelCapabilities::default();
    let models = ["alpha", "beta"].map(
        |id| json!({"modelId":id,"displayName":id,"status":"active","capabilities":capabilities}),
    );
    assert_ok(&client.rpc("provider.save", json!({"provider":{
        "providerId":"profile-fixture","displayName":"Fixture","driverKind":"openai-compatible",
        "baseUrl":"http://localhost/v1","auth":{"type":"anonymous"},"models":models,"enabled":true
    }})).await);
    assert_ok(
        &client
            .rpc(
                "provider.select",
                json!({"providerId":"profile-fixture","modelId":"alpha"}),
            )
            .await,
    );
    let profiled = client
        .rpc(
            "agent.create",
            json!({"profile":{
                "name":"profiled","description":"fixture","systemPrompt":"work","tools":[],
                "model":{"providerId":"profile-fixture","modelId":"beta"}
            }}),
        )
        .await;
    assert_ok(&profiled);
    for (agent, explicit, expected) in [
        (
            Some(profiled["result"]["agentId"].clone()),
            Some("alpha"),
            "alpha",
        ),
        (None, None, "alpha"),
        (Some(profiled["result"]["agentId"].clone()), None, "beta"),
    ] {
        let mut params = json!({"cwd":home});
        if let Some(agent) = agent {
            params["agentId"] = agent;
        }
        if let Some(model) = explicit {
            params["model"] = json!({"provider":"profile-fixture","modelId":model});
        }
        let created = client.rpc("session.create", params).await;
        assert_ok(&created);
        assert_eq!(created["result"]["model"]["modelId"], expected);
    }
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

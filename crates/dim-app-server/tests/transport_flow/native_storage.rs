use super::*;
use rusqlite::Connection;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn summaries_read_dormant_sessions_without_loading_them() {
    let directory = std::env::temp_dir().join(format!("dim-summary-{}", uuid::Uuid::new_v4()));
    let config = ServerConfig {
        database_path: Some(directory.join("dimcode.sqlite")),
        ..Default::default()
    };
    let (server, agent) = launch_with_agent(Arc::new(MockTools), config.clone()).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let mut expected = std::collections::HashMap::new();
    for index in 0..64 {
        let created = client.rpc("session.create", create(json!([]))).await;
        let session_id = created["result"]["sessionId"].as_str().unwrap().to_owned();
        let text = format!("persisted preview {index}");
        assert_ok(
            &client
                .rpc(
                    "session.submit",
                    json!({"sessionId":session_id,"branchId":"main","text":text}),
                )
                .await,
        );
        assert_ok(
            &client
                .rpc(
                    "session.idle",
                    json!({"sessionId":session_id,"branchId":"main"}),
                )
                .await,
        );
        expected.insert(session_id, text);
    }
    drop(client);
    drop(agent);
    server.shutdown();
    server.join().await.unwrap();

    let database = Connection::open(directory.join("dimcode.sqlite")).unwrap();
    let (server, agent) = launch_with_agent(Arc::new(MockTools), config).await;
    assert!(agent.sessions().is_empty());
    let mut client = Client::new_version(&server, false, 3).await;
    let reply = client.rpc("session.summaries", json!({})).await;
    assert_ok(&reply);
    assert_eq!(reply["result"].as_array().unwrap().len(), 64);
    for row in reply["result"].as_array().unwrap() {
        let id = row["sessionId"].as_str().unwrap();
        let body: String = database.query_row("SELECT body FROM timeline_events WHERE session_id=?1 AND branch_id='main' ORDER BY sequence DESC LIMIT 1", [id], |r| r.get(0)).unwrap();
        let stored: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(row["preview"], expected[id]);
        assert_eq!(row["updatedAt"], stored["record"]["event"]["create_at"]);
    }
    assert!(agent.sessions().is_empty());

    let mut writer = Client::new_version(&server, false, 3).await;
    let active = writer.rpc("session.create", create(json!([]))).await;
    let active_id = active["result"]["sessionId"].as_str().unwrap();
    let second_reader = Client::new_version(&server, false, 3).await;
    let read = |mut reader: Client| async move {
        let start = std::time::Instant::now();
        for _ in 0..8 {
            let reply = reader.rpc("session.summaries", json!({})).await;
            assert_ok(&reply);
            assert_eq!(reply["result"].as_array().unwrap().len(), 65);
        }
        start.elapsed()
    };
    let write = async {
        let start = std::time::Instant::now();
        for index in 0..8 {
            assert_ok(&writer.rpc("session.submit", json!({"sessionId":active_id,"branchId":"main","text":format!("live {index}")})).await);
            assert_ok(
                &writer
                    .rpc(
                        "session.idle",
                        json!({"sessionId":active_id,"branchId":"main"}),
                    )
                    .await,
            );
        }
        start.elapsed()
    };
    let (read_a, read_b, persisted) = tokio::join!(read(client), read(second_reader), write);
    eprintln!(
        "64 dormant sessions, 8 refreshes per reader: {read_a:?}, {read_b:?}; 8 persisted turns: {persisted:?}"
    );
    assert_eq!(
        agent.sessions().len(),
        1,
        "summary reads must not load historical sessions"
    );
    server.shutdown();
    server.join().await.unwrap();
    drop(database);
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn product_submit_acknowledges_only_a_committed_input() {
    let directory = std::path::PathBuf::from("/tmp").join(format!("ds-{}", uuid::Uuid::new_v4()));
    let database = directory.join("dimcode.sqlite");
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&directory)
        .unwrap();
    let mut builder = AgentBuilder::new(AgentConfig::default());
    defaults::install(&mut builder);
    defaults::mock_model_hosts(&mut builder);
    dim_agent_capabilities::defaults::tool_host(&mut builder, Arc::new(MockTools));
    let server = serve(
        builder,
        ServerConfig {
            database_path: Some(database.clone()),
            unix_path: Some(directory.join("server.sock")),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let mut client = Client::new_version(&server, false, 3).await;
    let created = client.rpc("session.create", create(json!([]))).await;
    assert_ok(&created);
    let sid = created["result"]["sessionId"].as_str().unwrap();
    let agent = server.agent().unwrap();
    let session = agent.get_session(sid).unwrap();
    let publication = session.publication("main").unwrap();
    assert_ok(
        &client
            .rpc("session.idle", json!({"sessionId":sid,"branchId":"main"}))
            .await,
    );
    let lock = Connection::open(&database).unwrap();
    lock.execute_batch("BEGIN IMMEDIATE").unwrap();
    let submission = uuid::Uuid::new_v4().to_string();
    let request = client
        .send(
            "session.submit",
            json!({
                "sessionId":sid,"branchId":"main","submissionId":submission,"text":"durable draft"
            }),
        )
        .await;
    tokio::time::timeout(Duration::from_secs(2), async {
        while !publication.has_pending() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(50), client.response(request))
            .await
            .is_err()
    );
    let mut observer = Client::new_version(&server, true, 3).await;
    let lookup = json!({"sessionId":sid,"branchId":"main","submissionId":submission});
    let looked_up = observer.send("session.input_lookup", lookup.clone()).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(50), observer.response(looked_up))
            .await
            .is_err()
    );
    lock.execute_batch("COMMIT").unwrap();
    let found = observer.response(looked_up).await;
    let receipt = client.response(request).await;
    assert_ok(&receipt);
    let event = &receipt["result"]["event"];
    assert_eq!(found["result"]["status"], "accepted");
    assert_eq!(found["result"]["event"], *event);
    let stored: String = lock.query_row(
        "SELECT body FROM timeline_events WHERE session_id=?1 AND branch_id='main' AND sequence=?2",
        rusqlite::params![sid, event["sequence"].as_i64().unwrap()],
        |row| row.get(0),
    ).unwrap();
    assert!(stored.contains("durable draft"));
    let accepted = observer.rpc("session.input_lookup", lookup).await;
    assert_eq!(accepted["result"]["status"], "accepted");
    assert_eq!(accepted["result"]["event"], *event);
    assert_ok(
        &client
            .rpc("session.idle", json!({"sessionId":sid,"branchId":"main"}))
            .await,
    );
    lock.execute_batch("BEGIN IMMEDIATE").unwrap();
    let before = publication.sequence();
    let pending_id = uuid::Uuid::new_v4().to_string();
    let pending = client
        .send(
            "session.submit",
            json!({
                "sessionId":sid,"branchId":"main","submissionId":pending_id,"text":"reload draft"
            }),
        )
        .await;
    tokio::time::timeout(Duration::from_secs(2), async {
        while !publication.has_pending() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    server.restart("Rpc").await.unwrap();
    let interrupted = client.response(pending).await;
    assert_eq!(
        interrupted["error"]["data"]["appError"],
        "submission_unknown"
    );
    lock.execute_batch("COMMIT").unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        let mut published = publication.watch();
        published
            .wait_for(|&(sequence, _)| sequence > before)
            .await
            .unwrap();
    })
    .await
    .unwrap();
    assert_eq!(
        observer
            .rpc(
                "session.input_lookup",
                json!({
                    "sessionId":sid,"branchId":"main","submissionId":pending_id
                })
            )
            .await["result"]["status"],
        "accepted"
    );
    server.shutdown();
    server.join().await.unwrap();
    drop(lock);
    drop(agent);
    tokio::fs::remove_dir_all(directory).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn process_model_and_home_storage_serve_independent_sessions() {
    tokio::time::timeout(Duration::from_secs(15), async {
        use dim_agent_aimux::{AimuxModels, config::ModelConfigFile};
        use dim_agent_capabilities::{history::HistoryRenderer, host::*};
        let requests = Arc::new(AtomicUsize::new(0));
        let count = requests.clone();
        let router = axum::Router::new().route("/v1/chat/completions", axum::routing::post(move || {
            let count = count.clone();
            async move {
                count.fetch_add(1, Ordering::SeqCst);
                axum::response::Response::builder().header("content-type", "text/event-stream")
                    .body(axum::body::Body::from("data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"process response\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n")).unwrap()
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
        let cancel = CancellationToken::new();
        let stop = cancel.clone();
        let model_server = tokio::spawn(async move {
            axum::serve(listener, router).with_graceful_shutdown(stop.cancelled_owned()).await.unwrap();
        });
        let models = Arc::new(AimuxModels::from_config(serde_json::from_value::<ModelConfigFile>(json!({
            "defaultModel":{"providerId":"local","modelId":"test"},
            "providers":[{"id":"local","kind":"openai-compatible","baseUrl":endpoint,"models":[{"id":"test","contextWindow":128000,"maxOutputTokens":4096,"config":{"maxTokens":64}}]}]
        })).unwrap()).unwrap());
        let home = std::path::PathBuf::from("/tmp").join(format!("dim-l0-{}", uuid::Uuid::new_v4()));
        let mut builder = AgentBuilder::new(AgentConfig { model: models.default_model().clone(), ..Default::default() });
        defaults::install_with_process_model(&mut builder);
        builder.session_value(Arc::new(HistoryRenderer::default()))
            .session_value(Arc::new(PrepareContextHost(Arc::new(IdentityContext))))
            .session_value(Arc::new(ContextModelHost(None)))
            .session_value(Arc::new(CompactionStrategyHost(None)));
        dim_agent_aimux::install(&mut builder);
        dim_agent_capabilities::defaults::tool_host(&mut builder, Arc::new(MockTools));
        dim_app_server::collaboration::install(&mut builder);
        let config = ServerConfig {
            target_home: Some(home.clone()), model_catalog: Some(models.clone()),
            unix_path: Some(home.join("ipc/server.sock")),
            session_defaults: dim_agent_runtime::SessionConfig { model: dim_agent_types::ModelSelection { provider_id: models.default_model().provider_id.clone(), model_id: models.default_model().model_id.clone(), capability_snapshot: None, config: None, metadata: None }, ..Default::default() },
            ..Default::default()
        };
        let server = serve(builder, config).await.unwrap();
        let agent = server.agent().unwrap();
        let mut client = Client::new_version(&server, false, 3).await;
        let mut sessions = Vec::new();
        for _ in 0..2 {
            let created = client.rpc("session.create", create(json!([]))).await;
            assert_ok(&created);
            let id = created["result"]["sessionId"].as_str().unwrap().to_owned();
            assert_ok(&client.rpc("session.submit", json!({"sessionId":id,"branchId":"main","text":"text"})).await);
            assert_ok(&client.rpc("session.idle", json!({"sessionId":id,"branchId":"main"})).await);
            let history = agent.get_session(&id).unwrap().history("main", None).await.unwrap();
            assert!(history.iter().any(|event| matches!(&event.kind, dim_agent_types::EventKind::Text(text) if text.delta == "process response")));
            sessions.push(id);
        }
        assert_ne!(sessions[0], sessions[1]);
        assert_eq!(requests.load(Ordering::SeqCst), 2);
        let snapshot = agent.diagnostics().unwrap();
        for name in ["FileStorage", "SqliteStorage", "Aimux", "ConfiguredModels"] {
            assert_eq!(snapshot.plugins.iter().filter(|plugin| plugin.name == name).count(), 1, "{name}");
        }
        drop(client); server.shutdown(); server.join().await.unwrap();
        assert_eq!(Connection::open(home.join("dimcode.sqlite")).unwrap().query_row("SELECT COUNT(*) FROM agent_sessions", [], |row| row.get::<_, i64>(0)).unwrap(), 2);
        cancel.cancel(); model_server.await.unwrap();
        tokio::fs::remove_dir_all(home).await.unwrap();
    }).await.unwrap();
}

struct InitialPromptHost {
    loads: Arc<AtomicUsize>,
    delivery: dim_agent_types::PromptDelivery,
}
impl dim_agent_capabilities::host::LoadPrompt for InitialPromptHost {
    fn load(
        &self,
        target: dim_agent_runtime::PromptSourceTarget,
        _: CancellationToken,
    ) -> HostFuture<'_, Option<dim_agent_types::PromptEntry>> {
        self.loads.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            Ok(Some(dim_agent_types::PromptEntry {
                source: target.source,
                version: "1".into(),
                body: "startup rules".into(),
                scope: target.scope,
                delivery: self.delivery,
                order: 0,
            }))
        })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn summaries_read_atomic_configuration_tails_and_survive_restart() {
    let directory =
        std::env::temp_dir().join(format!("dim-summary-batch-{}", uuid::Uuid::new_v4()));
    let config = ServerConfig {
        database_path: Some(directory.join("dimcode.sqlite")),
        ..Default::default()
    };
    let build = || {
        let mut builder = AgentBuilder::new(AgentConfig::default());
        defaults::install_with_prompt(
            &mut builder,
            Arc::new(InitialPromptHost {
                loads: Arc::new(AtomicUsize::new(0)),
                delivery: dim_agent_types::PromptDelivery::System,
            }),
            None,
        );
        defaults::mock_model_hosts(&mut builder);
        defaults::tool_host(&mut builder, Arc::new(MockTools));
        builder
    };
    let server = serve(build(), config.clone()).await.unwrap();
    let mut client = Client::new_version(&server, true, 3).await;
    let created = client.rpc("session.create", create(json!([]))).await;
    assert_ok(&created);
    let id = created["result"]["sessionId"].as_str().unwrap();
    let session = server.agent().unwrap().get_session(id).unwrap();
    session.idle("main").await.unwrap();
    session
        .configure(
            dim_agent_runtime::ConfigureUpdate {
                sources: vec![dim_agent_runtime::PromptSourceTarget {
                    source: "summary".into(),
                    scope: dim_agent_types::PromptScope::Branch {
                        timeline: "main".into(),
                    },
                }],
                model: Some(dim_agent_types::ModelSelection {
                    provider_id: "test".into(),
                    model_id: "summary".into(),
                    capability_snapshot: None,
                    config: None,
                    metadata: None,
                }),
            },
            "main",
        )
        .await
        .unwrap();
    let through = session.sequence("main").unwrap();
    assert!(
        session
            .local_page("main", through - 1, 1, through)
            .await
            .is_err(),
        "regression fixture must end with a multi-event atomic batch"
    );
    let history = session.local("main", ..=through).await.unwrap();
    let reply = client.rpc("session.summaries", json!({})).await;
    assert_ok(&reply);
    assert_eq!(reply["result"][0]["sequence"], through);
    assert_eq!(
        reply["result"][0]["updatedAt"],
        history.last().unwrap().create_at
    );
    drop(session);
    drop(client);
    server.shutdown();
    server.join().await.unwrap();

    let database = Connection::open(directory.join("dimcode.sqlite")).unwrap();
    let body: String = database.query_row(
        "SELECT body FROM timeline_events WHERE session_id=?1 AND branch_id='main' ORDER BY sequence DESC LIMIT 1",
        [id], |row| row.get(0),
    ).unwrap();
    let stored: Value = serde_json::from_str(&body).unwrap();
    let server = serve(build(), config).await.unwrap();
    let mut client = Client::new_version(&server, true, 3).await;
    let reply = client.rpc("session.summaries", json!({})).await;
    assert_ok(&reply);
    assert_eq!(
        reply["result"][0]["updatedAt"],
        stored["record"]["event"]["create_at"]
    );
    assert!(server.agent().unwrap().sessions().is_empty());
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    drop(database);
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn database_session_creation_adopts_initial_prompt_once_across_restart() {
    let directory = std::env::temp_dir().join(format!("dim-prompt-{}", uuid::Uuid::new_v4()));
    let config = ServerConfig {
        database_path: Some(directory.join("dimcode.sqlite")),
        ..Default::default()
    };
    let loads = Arc::new(AtomicUsize::new(0));
    let build = || {
        let mut builder = AgentBuilder::new(AgentConfig::default());
        defaults::install_with_prompt(
            &mut builder,
            Arc::new(InitialPromptHost {
                loads: loads.clone(),
                delivery: dim_agent_types::PromptDelivery::Context,
            }),
            Some(dim_agent_runtime::ConfigureUpdate {
                sources: vec![dim_agent_runtime::PromptSourceTarget {
                    source: "startup".into(),
                    scope: dim_agent_types::PromptScope::Branch {
                        timeline: "main".into(),
                    },
                }],
                model: None,
            }),
        );
        defaults::mock_model_hosts(&mut builder);
        dim_agent_capabilities::defaults::tool_host(&mut builder, Arc::new(MockTools));
        dim_app_server::collaboration::install(&mut builder);
        builder
    };
    let server = serve(build(), config.clone()).await.unwrap();
    let agent = server.agent().unwrap();
    let mut client = Client::new_version(&server, true, 3).await;
    let created = client.rpc("session.create", create(json!([]))).await;
    assert_ok(&created);
    let id = created["result"]["sessionId"].as_str().unwrap().to_owned();
    let before = agent
        .get_session(&id)
        .unwrap()
        .history("main", None)
        .await
        .unwrap();
    assert_eq!(
        before
            .iter()
            .filter(|event| matches!(&event.kind, dim_agent_types::EventKind::PromptChanged(_)))
            .count(),
        1
    );
    assert!(before.iter().any(|event| matches!(&event.kind,
        dim_agent_types::EventKind::PromptChanged(data)
            if data.changes.iter().any(|change| matches!(change,
                dim_agent_types::PromptChange::Set { entry }
                    if entry.source == "startup" && entry.body == "startup rules")))));
    assert_eq!(loads.load(Ordering::SeqCst), 1);
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    drop(agent);

    let server = serve(build(), config).await.unwrap();
    let agent = server.agent().unwrap();
    assert_eq!(loads.load(Ordering::SeqCst), 1);
    assert!(
        agent.sessions().is_empty(),
        "startup only loads the directory"
    );
    let mut client = Client::new_version(&server, true, 3).await;
    assert_ok(&client.rpc("session.attach", json!({"sessionId": id})).await);
    let after = agent
        .get_session(&id)
        .unwrap()
        .history("main", None)
        .await
        .unwrap();
    assert_eq!(&after[..before.len()], before.as_slice());
    assert_eq!(
        after
            .iter()
            .filter(|event| matches!(&event.kind, dim_agent_types::EventKind::PromptChanged(_)))
            .count(),
        1
    );
    server.shutdown();
    server.join().await.unwrap();
    drop(agent);
    tokio::fs::remove_dir_all(directory).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn damaged_native_records_do_not_hide_healthy_agents_or_sessions() {
    let directory = std::env::temp_dir().join(format!("dim-isolation-{}", uuid::Uuid::new_v4()));
    let database = directory.join("dimcode.sqlite");
    let config = ServerConfig {
        database_path: Some(database.clone()),
        ..Default::default()
    };
    let build = || {
        let mut builder = AgentBuilder::new(AgentConfig::default());
        defaults::install(&mut builder);
        defaults::mock_model_hosts(&mut builder);
        dim_agent_capabilities::defaults::tool_host(&mut builder, Arc::new(MockTools));
        dim_app_server::collaboration::install(&mut builder);
        builder
    };
    let server = serve(build(), config.clone()).await.unwrap();
    let mut client = Client::new_version(&server, true, 3).await;
    let default_agent = client.rpc("agent.list", json!({})).await["result"][0]["agentId"]
        .as_str()
        .unwrap()
        .to_owned();
    let healthy_agent = client.rpc("agent.create", json!({})).await["result"]["agentId"]
        .as_str()
        .unwrap()
        .to_owned();
    let damaged_agent = client.rpc("agent.create", json!({})).await["result"]["agentId"]
        .as_str()
        .unwrap()
        .to_owned();
    let session_for = |agent: &str| {
        let mut request = create(json!([]));
        request["agentId"] = json!(agent);
        request
    };
    let default_session = client
        .rpc("session.create", session_for(&default_agent))
        .await;
    assert_ok(&default_session);
    let default_session = default_session["result"]["sessionId"]
        .as_str()
        .unwrap()
        .to_owned();
    let healthy = client
        .rpc("session.create", session_for(&healthy_agent))
        .await;
    assert_ok(&healthy);
    let healthy = healthy["result"]["sessionId"].as_str().unwrap().to_owned();
    let damaged_event = client
        .rpc("session.create", session_for(&healthy_agent))
        .await;
    assert_ok(&damaged_event);
    let damaged_event = damaged_event["result"]["sessionId"]
        .as_str()
        .unwrap()
        .to_owned();
    let damaged_reference = client
        .rpc("session.create", session_for(&healthy_agent))
        .await;
    assert_ok(&damaged_reference);
    let damaged_reference = damaged_reference["result"]["sessionId"]
        .as_str()
        .unwrap()
        .to_owned();
    let damaged_agent_session = client
        .rpc("session.create", session_for(&damaged_agent))
        .await;
    assert_ok(&damaged_agent_session);
    let damaged_agent_session = damaged_agent_session["result"]["sessionId"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_ok(
        &client
            .rpc(
                "session.submit",
                json!({"sessionId":damaged_event,"branchId":"main","text":"persisted event"}),
            )
            .await,
    );
    assert_ok(
        &client
            .rpc(
                "session.idle",
                json!({"sessionId":damaged_event,"branchId":"main"}),
            )
            .await,
    );
    drop(client);
    server.shutdown();
    server.join().await.unwrap();

    let connection = Connection::open(&database).unwrap();
    connection
        .pragma_update(None, "foreign_keys", false)
        .unwrap();
    connection
        .execute(
            "UPDATE agents SET profile = '{' WHERE agent_id = ?1",
            [&default_agent],
        )
        .unwrap();
    let stored_profile: String = connection
        .query_row(
            "SELECT profile FROM agents WHERE agent_id = ?1",
            [&damaged_agent],
            |row| row.get(0),
        )
        .unwrap();
    let mut stored_profile: Value = serde_json::from_str(&stored_profile).unwrap();
    stored_profile["tools"] = json!(["echo", "echo"]);
    connection
        .execute(
            "UPDATE agents SET profile = ?1 WHERE agent_id = ?2",
            rusqlite::params![stored_profile.to_string(), damaged_agent],
        )
        .unwrap();
    connection
        .execute(
            "UPDATE timeline_events SET body = '{' WHERE session_id = ?1 AND sequence = 1",
            [&damaged_event],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO timeline_events(session_id, branch_id, sequence, last_sequence, kind, body) VALUES (?1, 'missing', 1, 1, 'step/text', '{}')",
            [&damaged_reference],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO session_owners VALUES ('orphan-owner', ?1, 3)",
            [&healthy_agent],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO timeline_branches VALUES ('orphan-branch', 'main', '{}', 0)",
            [],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO timeline_content VALUES ('orphan-content', 'hash', 'messages', '{}')",
            [],
        )
        .unwrap();
    drop(connection);

    let server = serve(build(), config).await.unwrap();
    assert!(
        server.agent().is_none(),
        "damaged default Agent must not be replaced"
    );
    let mut client = Client::new_version(&server, true, 3).await;
    let agents = client.rpc("agent.list", json!({})).await;
    assert_ok(&agents);
    assert_eq!(agents["result"].as_array().unwrap().len(), 1);
    assert_eq!(agents["result"][0]["agentId"], healthy_agent);
    let listed = client
        .rpc("session.list", json!({"agentId":healthy_agent}))
        .await;
    assert_ok(&listed);
    let listed = listed["result"].as_array().unwrap();
    assert_eq!(
        listed.len(),
        3,
        "valid directories remain visible even if their events are damaged"
    );
    assert!(listed.iter().any(|item| item["sessionId"] == healthy));
    assert_ok(
        &client
            .rpc(
                "session.submit",
                json!({"sessionId":healthy,"branchId":"main","text":"still usable"}),
            )
            .await,
    );
    assert_ok(
        &client
            .rpc(
                "session.idle",
                json!({"sessionId":healthy,"branchId":"main"}),
            )
            .await,
    );
    assert_ok(
        &client
            .rpc("session.create", session_for(&healthy_agent))
            .await,
    );
    for id in [
        default_session.as_str(),
        damaged_event.as_str(),
        damaged_reference.as_str(),
        damaged_agent_session.as_str(),
        "orphan-owner",
        "orphan-branch",
        "orphan-content",
    ] {
        let result = client
            .rpc(
                "session.submit",
                json!({"sessionId":id,"branchId":"main","text":"must fail"}),
            )
            .await;
        assert_eq!(result["error"]["code"], -32603, "{result}");
        assert!(result["error"]["data"].is_null(), "{result}");
    }
    for version in [1, 2] {
        let mut old = Client::new_version(&server, true, version).await;
        for id in [
            default_session.as_str(),
            damaged_event.as_str(),
            damaged_reference.as_str(),
            damaged_agent_session.as_str(),
            "orphan-owner",
        ] {
            let mut request = json!({"sessionId":id,"text":"wrong protocol"});
            if version == 2 {
                request["branchId"] = json!("main");
            }
            let result = old.rpc("session.submit", request).await;
            assert_eq!(
                result["error"]["data"]["appError"], "session_protocol_version",
                "{result}"
            );
        }
    }
    for id in [&default_agent, &damaged_agent] {
        let result = client.rpc("session.create", session_for(id)).await;
        assert_eq!(result["error"]["code"], -32603, "{result}");
        assert!(result["error"]["data"].is_null(), "{result}");
    }
    let mut legacy = Client::new_version(&server, true, 1).await;
    let rejected = legacy.rpc("session.create", create(json!([]))).await;
    assert_eq!(rejected["error"]["code"], -32603);
    assert!(rejected["error"]["data"].is_null());
    assert_eq!(
        client
            .rpc(
                "session.submit",
                json!({"sessionId":"unknown","branchId":"main","text":"unknown"})
            )
            .await["error"]["data"]["appError"],
        "session_closed"
    );
    drop(legacy);
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    let connection = Connection::open(&database).unwrap();
    assert_eq!(
        connection
            .query_row(
                "SELECT body FROM timeline_events WHERE session_id = ?1 AND sequence = 1",
                [&damaged_event],
                |row| row.get::<_, String>(0)
            )
            .unwrap(),
        "{"
    );
    let profile: String = connection
        .query_row(
            "SELECT profile FROM agents WHERE agent_id = ?1",
            [&damaged_agent],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&profile).unwrap()["tools"],
        json!(["echo", "echo"])
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT count(*) FROM session_owners WHERE session_id = 'orphan-owner'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    drop(connection);
    tokio::fs::remove_dir_all(directory).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn agent_sessions_restore_owners_branches_and_continue_with_original_ids() {
    let directory = std::env::temp_dir().join(format!("dim-native-{}", uuid::Uuid::new_v4()));
    let config = ServerConfig {
        database_path: Some(directory.join("dimcode.sqlite")),
        ..Default::default()
    };
    let (server, original_agent) = launch_with_agent(Arc::new(MockTools), config.clone()).await;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&directory).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }
    let mut client = Client::new_version(&server, false, 3).await;
    let original_instance = client.instance.clone();
    let mut parameters = create(json!(["branch"]));
    parameters["permissionPolicy"]["allowedTools"] = json!(["branch"]);
    let created = client.rpc("session.create", parameters).await;
    assert_ok(&created);
    let session_id = created["result"]["sessionId"].as_str().unwrap().to_owned();
    assert_ok(
        &client
            .rpc(
                "session.submit",
                json!({"sessionId": session_id, "branchId": "main", "text": "before restart"}),
            )
            .await,
    );
    assert_ok(
        &client
            .rpc(
                "session.idle",
                json!({"sessionId": session_id, "branchId": "main"}),
            )
            .await,
    );
    let original = original_agent.get_session(&session_id).unwrap();
    let before = original.history("main", None).await.unwrap();
    let created = client.rpc("session.branch_create", json!({"sessionId": session_id,
        "parentBranchId": "main", "at": original.sequence("main").unwrap(), "prompt": "child input"})).await;
    assert_ok(&created);
    let child_id = created["result"]["branch"]["branchId"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_ok(
        &client
            .rpc(
                "session.idle",
                json!({"sessionId": session_id, "branchId": child_id}),
            )
            .await,
    );

    let created = client.rpc("agent.create", json!({})).await;
    assert_ok(&created);
    let peer_id = created["result"]["agentId"].as_str().unwrap().to_owned();
    let mut parameters = create(json!([]));
    parameters["agentId"] = json!(peer_id);
    let created = client.rpc("session.create", parameters).await;
    assert_ok(&created);
    let peer_session_id = created["result"]["sessionId"].as_str().unwrap().to_owned();
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    drop(original);
    drop(original_agent);

    let (server, recovered_agent) = launch_with_agent(Arc::new(MockTools), config).await;
    let mut client = Client::new_version(&server, false, 3).await;
    assert_ne!(client.instance, original_instance);
    let listed = client.rpc("session.list", json!({})).await;
    assert_ok(&listed);
    let sessions = listed["result"].as_array().unwrap();
    assert_eq!(sessions.len(), 2);
    assert_eq!(
        sessions
            .iter()
            .find(|session| session["sessionId"] == peer_session_id)
            .unwrap()["agentId"],
        peer_id
    );
    assert!(recovered_agent.sessions().is_empty());
    assert_ok(
        &client
            .rpc("session.attach", json!({"sessionId": session_id}))
            .await,
    );
    let recovered = recovered_agent.get_session(&session_id).unwrap();
    let after = recovered.history("main", None).await.unwrap();
    assert_eq!(&after[..before.len()], &before);
    assert!(
        recovered
            .branches()
            .iter()
            .any(|branch| branch.id == child_id)
    );
    let child_history = recovered.history(&child_id, Some(0)).await.unwrap();
    assert_eq!(&child_history[..before.len()], &before);
    assert_ok(
        &client
            .rpc("session.attach", json!({"sessionId": session_id}))
            .await,
    );
    assert_ok(
        &client
            .rpc(
                "session.submit",
                json!({"sessionId": session_id, "branchId": child_id, "text": "after restart"}),
            )
            .await,
    );
    assert_ok(
        &client
            .rpc(
                "session.idle",
                json!({"sessionId": session_id, "branchId": child_id}),
            )
            .await,
    );
    assert!(recovered.local(&child_id, ..).await.unwrap().iter().any(|event|
        matches!(&event.kind, dim_agent_types::EventKind::Text(data) if data.delta.contains("after restart"))));
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    drop(recovered);
    drop(recovered_agent);
    tokio::fs::remove_dir_all(directory).await.unwrap();
}

/// `session.create {interactive}` takes only a boolean and is creation data
/// of the stored Session record on v13: only `false` is written, an
/// interactive record keeps the fields written before `interactive` existed,
/// and a record written before the field existed restores as interactive.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interactive_is_stored_with_the_session_and_absence_means_interactive() {
    let directory = std::env::temp_dir().join(format!("dim-interactive-{}", uuid::Uuid::new_v4()));
    let database = directory.join("dimcode.sqlite");
    let config = ServerConfig {
        database_path: Some(database.clone()),
        ..Default::default()
    };
    let (server, agent) = launch_with_agent(Arc::new(MockTools), config.clone()).await;
    let mut client = Client::new_version(&server, false, 3).await;
    for invalid in [Value::Null, json!("false"), json!(0)] {
        let mut parameters = create(json!([]));
        parameters["interactive"] = invalid;
        let rejected = client.rpc("session.create", parameters).await;
        assert_eq!(rejected["error"]["code"], -32602, "{rejected}");
    }
    let created = client.rpc("session.create", create(json!([]))).await;
    assert_ok(&created);
    let interactive = created["result"]["sessionId"].as_str().unwrap().to_owned();
    let mut parameters = create(json!([]));
    parameters["interactive"] = json!(false);
    let created = client.rpc("session.create", parameters).await;
    assert_ok(&created);
    let automated = created["result"]["sessionId"].as_str().unwrap().to_owned();
    assert!(
        agent
            .get_session(&interactive)
            .unwrap()
            .config()
            .interactive
    );
    assert!(!agent.get_session(&automated).unwrap().config().interactive);
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    drop(agent);

    let connection = Connection::open(&database).unwrap();
    let stored = |id: &str| -> serde_json::Map<String, Value> {
        let config: String = connection
            .query_row(
                "SELECT config FROM agent_sessions WHERE session_id=?1",
                [id],
                |row| row.get(0),
            )
            .unwrap();
        serde_json::from_str(&config).unwrap()
    };
    let mut fields: Vec<_> = stored(&interactive)
        .into_iter()
        .map(|(key, _)| key)
        .collect();
    fields.sort();
    assert_eq!(
        fields,
        [
            "collaborationRole",
            "cwd",
            "model",
            "permissionPolicy",
            "systemPrompt",
            "tools"
        ]
    );
    assert_eq!(stored(&automated)["interactive"], false);
    // The record a daemon before C2 wrote for this Session: its six fields,
    // stored directly so that restore reads bytes this code never produced.
    let pre_c2 = json!({
        "permissionPolicy": {"preset": "read-only", "allowedTools": ["echo", "wait"], "disallowedTools": []},
        "cwd": std::env::temp_dir(),
        "model": {"providerId": "mock", "modelId": "deterministic"},
        "systemPrompt": "",
        "tools": [],
        "collaborationRole": "leader"
    });
    let updated = connection
        .execute(
            "UPDATE agent_sessions SET config = ?1 WHERE session_id = ?2",
            rusqlite::params![pre_c2.to_string(), interactive],
        )
        .unwrap();
    assert_eq!(updated, 1);
    drop(connection);

    let (server, agent) = launch_with_agent(Arc::new(MockTools), config).await;
    let mut client = Client::new_version(&server, false, 3).await;
    for id in [&interactive, &automated] {
        assert_ok(&client.rpc("session.attach", json!({"sessionId": id})).await);
    }
    assert!(
        agent
            .get_session(&interactive)
            .unwrap()
            .config()
            .interactive
    );
    assert!(!agent.get_session(&automated).unwrap().config().interactive);
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    drop(agent);
    tokio::fs::remove_dir_all(directory).await.unwrap();
}

/// The process seeds the default Agent, so a stored default profile from an
/// earlier version (a mount list frozen before tools were added) follows the
/// current process default on the next start, and its row is rewritten.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_stored_default_agent_follows_the_process_default_profile() {
    let directory =
        std::env::temp_dir().join(format!("dim-default-agent-{}", uuid::Uuid::new_v4()));
    let database = directory.join("dimcode.sqlite");
    let config = ServerConfig {
        database_path: Some(database.clone()),
        ..Default::default()
    };
    let process_tools = config.session_defaults.tools.clone();
    let (server, agent) = launch_with_agent(Arc::new(MockTools), config.clone()).await;
    server.shutdown();
    server.join().await.unwrap();
    drop(agent);

    let connection = Connection::open(&database).unwrap();
    let stored = || -> Value {
        let profile: String = connection
            .query_row(
                "SELECT profile FROM agents WHERE is_default = 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        serde_json::from_str(&profile).unwrap()
    };
    let mut frozen = stored();
    frozen["tools"] = json!(["echo"]);
    connection
        .execute(
            "UPDATE agents SET profile = ?1 WHERE is_default = 1",
            [frozen.to_string()],
        )
        .unwrap();

    let (server, agent) = launch_with_agent(Arc::new(MockTools), config).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let created = client
        .rpc("session.create", json!({"cwd":std::env::temp_dir()}))
        .await;
    assert_ok(&created);
    let session = created["result"]["sessionId"].as_str().unwrap();
    assert_eq!(
        agent.get_session(session).unwrap().config().tools,
        process_tools
    );
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    drop(agent);
    assert_eq!(stored()["tools"], json!(process_tools));
    drop(connection);
    tokio::fs::remove_dir_all(directory).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stored_directories_outlive_idle_instances() {
    tokio::time::timeout(Duration::from_secs(15), async {
        let directory = std::env::temp_dir().join(format!("dim-lazy-{}", uuid::Uuid::new_v4()));
        let database = directory.join("dimcode.sqlite");
        let config = ServerConfig {
            database_path: Some(database.clone()),
            ..Default::default()
        };
        let build = || {
            let mut builder = AgentBuilder::new(AgentConfig::default());
            defaults::mock_model_hosts(&mut builder);
            defaults::install(&mut builder);
            dim_agent_capabilities::defaults::tool_host(&mut builder, Arc::new(MockTools));
            dim_app_server::collaboration::install(&mut builder);
            builder
        };
        let server = serve(build(), config.clone()).await.unwrap();
        let mut client = Client::new_version(&server, true, 3).await;
        let mut ids = vec![];
        for _ in 0..3 {
            let created = client.rpc("session.create", create(json!([]))).await;
            assert_ok(&created);
            let id = created["result"]["sessionId"].as_str().unwrap().to_owned();
            assert_ok(&client.rpc("session.close", json!({"sessionId":id})).await);
            ids.push(id);
        }
        drop(client);
        server.shutdown();
        server.join().await.unwrap();

        let server = serve(build(), config).await.unwrap();
        let agent = server.agent().unwrap();
        assert!(agent.sessions().is_empty());
        let mut client = Client::new_version(&server, true, 3).await;
        let listed = client.rpc("session.list", json!({})).await;
        assert_ok(&listed);
        assert_eq!(listed["result"].as_array().unwrap().len(), 3);
        assert!(
            agent.sessions().is_empty(),
            "listing must not restore instances"
        );
        let created_at = listed["result"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["sessionId"] == ids[0])
            .unwrap()["createdAt"]
            .clone();
        let mut concurrent = Client::new_version(&server, true, 3).await;
        let (first_attach, second_attach) = tokio::join!(
            client.rpc("session.attach", json!({"sessionId":ids[0]})),
            concurrent.rpc("session.attach", json!({"sessionId":ids[0]})),
        );
        assert_ok(&first_attach);
        assert_ok(&second_attach);
        assert_eq!(
            agent.sessions().len(),
            1,
            "concurrent requests share one instance"
        );
        assert_ok(
            &concurrent
                .rpc("session.detach", json!({"sessionId":ids[0]}))
                .await,
        );
        drop(concurrent);
        let connection = Connection::open(&database).unwrap();
        for table in ["agent_sessions", "session_owners", "timeline_branches"] {
            assert_eq!(
                connection
                    .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| row
                        .get::<_, i64>(0))
                    .unwrap(),
                3
            );
        }
        drop(connection);
        let first = agent.get_session(&ids[0]).unwrap();
        let before = first.sequence("main").unwrap();
        assert_ok(
            &client
                .rpc("session.detach", json!({"sessionId":ids[0]}))
                .await,
        );
        assert_ok(
            &client
                .rpc("session.attach", json!({"sessionId":ids[1]}))
                .await,
        );
        assert!(first.unload_if_idle().await.unwrap());
        assert!(first.is_unloading());
        let connection = Connection::open(&database).unwrap();
        let head: i64 = connection
            .query_row(
                "SELECT sequence FROM timeline_branches WHERE session_id=?1 AND branch_id='main'",
                [&ids[0]],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            head as u64, before,
            "idle release must append no settlement events"
        );
        assert!(agent.get_session(&ids[0]).is_none());
        assert_ok(
            &client
                .rpc("session.detach", json!({"sessionId":ids[1]}))
                .await,
        );
        assert_ok(
            &client
                .rpc("session.attach", json!({"sessionId":ids[0]}))
                .await,
        );
        assert_ok(
            &client
                .rpc(
                    "session.submit",
                    json!({"sessionId":ids[0],"branchId":"main","text":"continued after unload"}),
                )
                .await,
        );
        assert_ok(
            &client
                .rpc(
                    "session.idle",
                    json!({"sessionId":ids[0],"branchId":"main"}),
                )
                .await,
        );
        let listed = client.rpc("session.list", json!({})).await;
        let item = listed["result"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["sessionId"] == ids[0])
            .unwrap();
        assert_eq!(item["createdAt"], created_at);
        drop(connection);
        drop(client);
        server.shutdown();
        server.join().await.unwrap();
        tokio::fs::remove_dir_all(directory).await.unwrap();
    })
    .await
    .expect("idle unload must settle");
}

struct FailingInitialPrompt(Arc<std::sync::atomic::AtomicBool>);
impl dim_agent_capabilities::host::LoadPrompt for FailingInitialPrompt {
    fn load(
        &self,
        _: dim_agent_runtime::PromptSourceTarget,
        _: CancellationToken,
    ) -> HostFuture<'_, Option<dim_agent_types::PromptEntry>> {
        Box::pin(async move {
            if self.0.load(Ordering::SeqCst) {
                Err(dim_agent_capabilities::host::HostError::failed(
                    "initial prompt unavailable",
                ))
            } else {
                Ok(None)
            }
        })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_creation_rolls_back_storage_but_failed_restore_preserves_it() {
    let directory =
        std::env::temp_dir().join(format!("dim-create-rollback-{}", uuid::Uuid::new_v4()));
    let database = directory.join("dimcode.sqlite");
    let config = ServerConfig {
        database_path: Some(database.clone()),
        ..Default::default()
    };
    let failing = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let build = || {
        let mut builder = AgentBuilder::new(AgentConfig::default());
        defaults::install_with_prompt(
            &mut builder,
            Arc::new(FailingInitialPrompt(failing.clone())),
            Some(dim_agent_runtime::ConfigureUpdate {
                sources: vec![dim_agent_runtime::PromptSourceTarget {
                    source: "startup".into(),
                    scope: dim_agent_types::PromptScope::Branch {
                        timeline: "main".into(),
                    },
                }],
                model: None,
            }),
        );
        defaults::mock_model_hosts(&mut builder);
        dim_agent_capabilities::defaults::tool_host(&mut builder, Arc::new(MockTools));
        dim_app_server::collaboration::install(&mut builder);
        builder
    };
    let tables = [
        "agent_sessions",
        "session_owners",
        "timeline_branches",
        "timeline_events",
        "timeline_content",
    ];
    let counts = |connection: &Connection| -> Vec<i64> {
        tables
            .iter()
            .map(|table| {
                connection
                    .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                        row.get(0)
                    })
                    .unwrap()
            })
            .collect()
    };
    let server = serve(build(), config.clone()).await.unwrap();
    let mut client = Client::new_version(&server, true, 3).await;
    let failed = client.rpc("session.create", create(json!([]))).await;
    assert!(failed.get("error").is_some(), "{failed}");
    let listed = client.rpc("session.list", json!({})).await;
    assert_eq!(listed["result"], json!([]));
    assert_eq!(counts(&Connection::open(&database).unwrap()), vec![0; 5]);
    assert!(server.agent().unwrap().sessions().is_empty());
    failing.store(false, Ordering::SeqCst);
    let created = client.rpc("session.create", create(json!([]))).await;
    assert_ok(&created);
    let id = created["result"]["sessionId"].as_str().unwrap().to_owned();
    assert_ok(&client.rpc("session.attach", json!({"sessionId":id})).await);
    assert_ok(
        &client
            .rpc(
                "session.submit",
                json!({"sessionId":id,"branchId":"main","text":"preserve this history"}),
            )
            .await,
    );
    assert_ok(
        &client
            .rpc("session.idle", json!({"sessionId":id,"branchId":"main"}))
            .await,
    );
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    let connection = Connection::open(&database).unwrap();
    let before = counts(&connection);
    assert!(before[3] > 0);
    connection.execute("UPDATE timeline_events SET body='invalid stored event' WHERE session_id=?1 AND sequence=1", [&id]).unwrap();
    let server = serve(build(), config).await.unwrap();
    let mut client = Client::new_version(&server, true, 3).await;
    let restored = client.rpc("session.attach", json!({"sessionId":id})).await;
    assert!(restored.get("error").is_some(), "{restored}");
    let listed = client.rpc("session.list", json!({})).await;
    assert_eq!(listed["result"].as_array().unwrap().len(), 1);
    assert_eq!(
        counts(&connection),
        before,
        "restore failure must not roll back existing history"
    );
    let body: String = connection
        .query_row(
            "SELECT body FROM timeline_events WHERE session_id=?1 AND sequence=1",
            [&id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(body, "invalid stored event");
    drop(connection);
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    tokio::fs::remove_dir_all(directory).await.unwrap();
}

/// `--vacuum` is the offline maintenance entry: it refuses a store a daemon
/// owns and rewrites an idle one, reporting its size before and after (#600).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn vacuum_mode_reclaims_an_offline_store_and_refuses_a_held_one() {
    use dim_app_server::storage::SqliteStorage;

    async fn vacuum(database: &std::path::Path) -> std::process::Output {
        tokio::process::Command::new(env!("CARGO_BIN_EXE_dim-app-server"))
            .args(["--vacuum", "--database"])
            .arg(database)
            .output()
            .await
            .unwrap()
    }

    let directory = std::env::temp_dir().join(format!("dim-vacuum-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&directory).unwrap();
    let database = directory.join("dimcode.sqlite");
    let db = SqliteStorage::open(database.clone()).unwrap();
    let connection = Connection::open(&database).unwrap();
    connection
        .execute_batch(
            "INSERT INTO agent_sessions(session_id, config, created_at_ms) VALUES ('s', '{}', 0);
             INSERT INTO timeline_branches VALUES ('s', 'main', '{}', 0);",
        )
        .unwrap();
    connection.execute_batch("BEGIN").unwrap();
    for sequence in 1..=2000 {
        connection
            .execute(
                "INSERT INTO timeline_events (session_id, branch_id, sequence, last_sequence, kind, body) \
                 VALUES ('s', 'main', ?1, ?1, 'turn/start', ?2)",
                rusqlite::params![sequence, "x".repeat(4000)],
            )
            .unwrap();
    }
    connection.execute_batch("COMMIT").unwrap();
    connection
        .execute_batch("DELETE FROM timeline_events; PRAGMA wal_checkpoint(TRUNCATE)")
        .unwrap();
    drop(connection);

    let refused = vacuum(&database).await;
    assert_eq!(refused.status.code(), Some(1));
    assert!(
        String::from_utf8(refused.stderr)
            .unwrap()
            .contains("another process owns the SQLite database")
    );

    db.close().await.unwrap();
    let output = vacuum(&database).await;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .starts_with("Vacuumed ")
    );
    drop(db);
    tokio::fs::remove_dir_all(directory).await.unwrap();
}

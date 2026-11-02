use super::*;
use dim_agent_types::{InputBlock, InputMessageData};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn summaries_are_v3_filtered_bounded_and_leave_list_strict() {
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
    let initial = empty_session
        .local("main", ..=empty_row["sequence"].as_u64().unwrap())
        .await
        .unwrap();
    assert_eq!(
        empty_row["updatedAt"],
        initial
            .last()
            .map_or(empty_session.created_at(), |event| event.create_at)
    );
    assert!(empty_row.get("preview").is_none());
    let row = rows
        .iter()
        .find(|row| row["sessionId"] == session_id)
        .unwrap();
    let preview = row["preview"].as_str().unwrap();
    assert_eq!(preview.chars().count(), 120);
    assert!(preview.ends_with('…'));
    let through = row["sequence"].as_u64().unwrap();
    let last = session.local("main", through..=through).await.unwrap();
    assert_eq!(row["updatedAt"], last[0].create_at);
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

    let list = client.rpc("session.list", json!({})).await;
    assert_ok(&list);
    assert!(
        list["result"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| { row.get("preview").is_none() && row.get("updatedAt").is_none() })
    );

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

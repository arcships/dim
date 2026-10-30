use super::deletion::Store;
use super::*;
use dim_app_server::storage::initialize_session_catalog;
use rusqlite::params;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn catalog_pages_and_searches_a_large_dormant_store_without_reading_history() {
    let store = Store::new();
    let (server, agent) = launch_with_agent(Arc::new(MockTools), store.config()).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let seed = client.rpc("session.create", create(json!([]))).await;
    assert_ok(&seed);
    let seed = seed["result"]["sessionId"].as_str().unwrap().to_owned();
    drop(client);
    drop(agent);
    server.shutdown();
    server.join().await.unwrap();
    let mut connection = store.connection();
    let tx = connection.transaction().unwrap();
    for index in 0..2300 {
        let id = format!("catalog-{index:04}");
        tx.execute("INSERT INTO agent_sessions SELECT ?1, config, created_at_ms FROM agent_sessions WHERE session_id=?2", params![id,seed]).unwrap();
        tx.execute("INSERT INTO session_owners SELECT ?1,agent_id,protocol_version FROM session_owners WHERE session_id=?2", params![id,seed]).unwrap();
        tx.execute(
            "INSERT INTO timeline_branches VALUES (?1,'main','{\"id\":\"main\"}',?2)",
            params![id, i64::from(index < 300)],
        )
        .unwrap();
        if index < 300 {
            let event = json!({"encoding":"event","record":{"event":{"id":format!("input-{index}"),"sequence":1,"create_at":1000+index,"type":"input","name":"message","data":{"user":{"content":[{"type":"text","text":format!("preview {index}")}]}}},"refs":null,"trace":null,"batch_end":1}});
            tx.execute(
                "INSERT INTO timeline_events VALUES (?1,'main',1,1,'input/message',?2,?3)",
                params![id, event.to_string(), format!("input-{index}")],
            )
            .unwrap();
        }
        initialize_session_catalog(&tx, &id).unwrap();
    }
    tx.execute(
        "UPDATE session_catalog SET updated_at_ms=900000 WHERE preview IS NULL",
        [],
    )
    .unwrap();
    tx.execute(
        "INSERT INTO session_metadata VALUES ('catalog-0001',1,'Ä title %_','manual')",
        [],
    )
    .unwrap();
    tx.execute("UPDATE agent_sessions SET config=json_set(config,'$.model.providerId','unique-provider','$.model.modelId','unique-model','$.cwd','/Ä-workspace') WHERE session_id='catalog-0001'", []).unwrap();
    tx.execute(
        "UPDATE session_catalog SET cwd='/Ä-workspace' WHERE session_id='catalog-0001'",
        [],
    )
    .unwrap();
    tx.commit().unwrap();
    let (server, agent) = launch_with_agent(Arc::new(MockTools), store.config()).await;
    let mut client = Client::new_version(&server, false, 3).await;
    assert!(
        client
            .features
            .as_array()
            .unwrap()
            .contains(&json!("session.catalog"))
    );
    assert!(agent.sessions().is_empty());
    let start = std::time::Instant::now();
    let page = client
        .rpc("session.catalog", json!({"withPreview":true}))
        .await;
    eprintln!(
        "2300 dormant Sessions, first catalog page: {:?}",
        start.elapsed()
    );
    assert_ok(&page);
    let mut ids: Vec<String> = page["result"]["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["sessionId"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(ids.len(), 50);
    assert_eq!(ids[0], "catalog-0299");
    let cursor = page["result"]["nextCursor"].as_str().unwrap().to_owned();
    for params in [
        json!({"cursor":cursor}),
        json!({"cursor":cursor,"withPreview":true,"query":"changed"}),
        json!({"cursor":"not-a-cursor"}),
        json!({"limit":0}),
        json!({"limit":201}),
    ] {
        let reply = client.rpc("session.catalog", params).await;
        assert_eq!(reply["error"]["code"], -32602, "{reply}");
    }
    connection
        .execute(
            "UPDATE session_catalog SET updated_at_ms=500000 WHERE session_id='catalog-0000'",
            [],
        )
        .unwrap();
    let mut next = Some(cursor);
    while let Some(cursor) = next {
        let page = client
            .rpc(
                "session.catalog",
                json!({"withPreview":true,"cursor":cursor,"limit":47}),
            )
            .await;
        assert_ok(&page);
        ids.extend(
            page["result"]["sessions"]
                .as_array()
                .unwrap()
                .iter()
                .map(|row| row["sessionId"].as_str().unwrap().to_owned()),
        );
        next = page["result"]["nextCursor"].as_str().map(str::to_owned);
    }
    let expected: Vec<_> = (1..300)
        .rev()
        .map(|index| format!("catalog-{index:04}"))
        .collect();
    assert_eq!(
        ids, expected,
        "an updated row moves forward without losing any other row"
    );
    for query in [
        "ä TITLE",
        "%_",
        "preview 1",
        "CATALOG-0001",
        "UNIQUE-PROVIDER",
        "UNIQUE-MODEL",
        "/ä-workspace",
    ] {
        let page = client
            .rpc(
                "session.catalog",
                json!({"withPreview":true,"query":query,"limit":200}),
            )
            .await;
        assert_ok(&page);
        assert!(
            page["result"]["sessions"]
                .as_array()
                .unwrap()
                .iter()
                .any(|row| row["sessionId"] == "catalog-0001"),
            "{page}"
        );
    }
    connection
        .execute(
            "UPDATE session_catalog SET updated_at_ms=500000 WHERE preview IS NOT NULL",
            [],
        )
        .unwrap();
    let mut cursor = None;
    let mut tied = Vec::new();
    loop {
        let mut params = json!({"withPreview":true,"limit":37});
        if let Some(cursor) = cursor {
            params["cursor"] = cursor;
        }
        let page = client.rpc("session.catalog", params).await;
        assert_ok(&page);
        tied.extend(
            page["result"]["sessions"]
                .as_array()
                .unwrap()
                .iter()
                .map(|row| row["sessionId"].as_str().unwrap().to_owned()),
        );
        cursor = page["result"].get("nextCursor").cloned();
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(
        tied,
        (0..300)
            .map(|index| format!("catalog-{index:04}"))
            .collect::<Vec<_>>()
    );
    let no_match = client
        .rpc("session.catalog", json!({"cwd":"/absent-workspace"}))
        .await;
    assert_eq!(no_match["result"]["sessions"], json!([]));
    let all = client.rpc("session.summaries", json!({})).await;
    assert_ok(&all);
    assert_eq!(all["result"].as_array().unwrap().len(), 2301);
    assert!(
        all["result"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["running"] == false)
    );
    assert!(agent.sessions().is_empty());
    connection
        .execute(
            "UPDATE timeline_events SET body='unreadable history' WHERE session_id='catalog-0001'",
            [],
        )
        .unwrap();
    let page = client
        .rpc("session.catalog", json!({"query":"ä title"}))
        .await;
    assert_ok(&page);
    assert_eq!(page["result"]["sessions"][0]["preview"], "preview 1");
    for version in [1, 2] {
        let mut old = Client::new_version(&server, false, version).await;
        let reply = old.rpc("session.catalog", json!({})).await;
        assert_eq!(reply["error"]["code"], -32601);
    }
    drop(client);
    drop(agent);
    server.shutdown();
    server.join().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn catalog_is_not_advertised_without_native_storage() {
    let server = launch(Arc::new(MockTools), ServerConfig::default()).await;
    let mut client = Client::new_version(&server, false, 3).await;
    assert!(
        !client
            .features
            .as_array()
            .unwrap()
            .contains(&json!("session.catalog"))
    );
    assert_ok(&client.rpc("session.summaries", json!({})).await);
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
}

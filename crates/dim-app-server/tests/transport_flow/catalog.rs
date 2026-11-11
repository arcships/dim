use super::deletion::Store;
use super::*;
use dim_app_server::storage::{initialize_session_catalog, initialize_session_member_index};
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
        tx.execute("INSERT INTO agent_sessions (session_id, config, created_at_ms, leader_session_id) SELECT ?1, config, created_at_ms, leader_session_id FROM agent_sessions WHERE session_id=?2", params![id,seed]).unwrap();
        tx.execute("INSERT INTO session_owners SELECT ?1,agent_id,protocol_version FROM session_owners WHERE session_id=?2", params![id,seed]).unwrap();
        tx.execute(
            "INSERT INTO timeline_branches VALUES (?1,'main','{\"id\":\"main\"}',?2)",
            params![id, i64::from(index < 300)],
        )
        .unwrap();
        if index < 300 {
            let event = json!({"encoding":"event","record":{"event":{"id":format!("input-{index}"),"sequence":1,"create_at":1000+index,"type":"input","name":"message","data":{"user":{"content":[{"type":"text","text":format!("preview {index}")}]}}},"refs":null,"trace":null,"batch_end":1}});
            tx.execute(
                "INSERT INTO timeline_events VALUES (?1,'main',1,1,'input/message',?2,?3,0,0)",
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
            "UPDATE timeline_events SET body='{}' WHERE session_id='catalog-0001'",
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

/// #557: a Session's creation origin is reported by every read, the catalog
/// lists conversations by default, and both classes are opt-in.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn catalog_reports_creation_origins_and_filters_the_hidden_classes() {
    let store = Store::new();
    let (server, agent) = launch_with_agent(Arc::new(MockTools), store.config()).await;
    let mut client = Client::new_version(&server, false, 3).await;
    assert!(
        client
            .features
            .as_array()
            .unwrap()
            .contains(&json!("session.catalog.origins"))
    );
    let conversation = client.rpc("session.create", create(json!([]))).await;
    assert_ok(&conversation);
    let conversation = conversation["result"]["sessionId"]
        .as_str()
        .unwrap()
        .to_owned();
    let declared = client
        .rpc(
            "session.create",
            json!({"cwd":std::env::temp_dir(),"tools":[],"origin":"acp"}),
        )
        .await;
    assert_ok(&declared);
    let acp = declared["result"]["sessionId"].as_str().unwrap().to_owned();
    let forked = client
        .rpc("session.fork", json!({"sessionId":acp,"at":0}))
        .await;
    assert_ok(&forked);
    let forked = forked["result"]["sessionId"].as_str().unwrap().to_owned();
    for origin in ["internal", "unknown", "other"] {
        let refused = client
            .rpc(
                "session.create",
                json!({"cwd":std::env::temp_dir(),"tools":[],"origin":origin}),
            )
            .await;
        assert_eq!(refused["error"]["code"], -32602, "{origin}: {refused}");
    }
    drop(client);
    drop(agent);
    server.shutdown();
    server.join().await.unwrap();
    let child = "internal-child".to_owned();
    let mut connection = store.connection();
    let tx = connection.transaction().unwrap();
    tx.execute(
        "INSERT INTO agent_sessions (session_id, config, created_at_ms, leader_session_id, origin) SELECT ?1, config, created_at_ms, session_id, 'internal' FROM agent_sessions WHERE session_id=?2",
        params![child, conversation],
    )
    .unwrap();
    tx.execute(
        "INSERT INTO session_owners SELECT ?1, agent_id, protocol_version FROM session_owners WHERE session_id=?2",
        params![child, conversation],
    )
    .unwrap();
    tx.execute(
        "INSERT INTO timeline_branches VALUES (?1,'main','{\"id\":\"main\"}',0)",
        [&child],
    )
    .unwrap();
    initialize_session_catalog(&tx, &child).unwrap();
    initialize_session_member_index(&tx, &child).unwrap();
    tx.commit().unwrap();

    let plan: Vec<String> = connection
        .prepare(
            "EXPLAIN QUERY PLAN SELECT c.session_id FROM session_catalog c \
             INDEXED BY session_catalog_by_activity \
             CROSS JOIN agent_sessions s ON s.session_id=c.session_id \
             WHERE s.origin != 'internal' AND s.origin != 'acp' \
             ORDER BY c.updated_at_ms DESC, c.session_id LIMIT 51",
        )
        .unwrap()
        .query_map([], |row| row.get(3))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert!(
        plan.iter()
            .any(|line| line.contains("session_catalog_by_activity")),
        "{plan:?}"
    );
    assert!(
        !plan.iter().any(|line| line.contains("TEMP B-TREE")),
        "a filtered page must stop in index order, not sort the store: {plan:?}"
    );

    let (server, agent) = launch_with_agent(Arc::new(MockTools), store.config()).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let listed = |value: &Value| -> Vec<String> {
        value["result"]["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["sessionId"].as_str().unwrap().to_owned())
            .collect()
    };
    let page = client.rpc("session.catalog", json!({})).await;
    assert_ok(&page);
    let ids = listed(&page);
    assert!(
        ids.contains(&conversation) && ids.contains(&forked),
        "{ids:?}"
    );
    assert!(!ids.contains(&acp) && !ids.contains(&child), "{ids:?}");
    let row = |page: &Value, id: &str| -> Value {
        page["result"]["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["sessionId"] == id)
            .unwrap_or_else(|| panic!("{id} is missing from the catalog page"))
            .clone()
    };
    assert_eq!(row(&page, &conversation)["origin"], "client");
    assert_eq!(row(&page, &forked)["origin"], "client");
    assert!(row(&page, &forked).get("parentSessionId").is_none());

    let with_acp = client
        .rpc("session.catalog", json!({"withAcpSessions":true}))
        .await;
    assert_ok(&with_acp);
    let ids = listed(&with_acp);
    assert!(ids.contains(&acp) && !ids.contains(&child), "{ids:?}");
    assert_eq!(row(&with_acp, &acp)["origin"], "acp");

    let with_internal = client
        .rpc("session.catalog", json!({"withInternal":true}))
        .await;
    assert_ok(&with_internal);
    let ids = listed(&with_internal);
    assert!(ids.contains(&child) && !ids.contains(&acp), "{ids:?}");
    let child_row = row(&with_internal, &child);
    assert_eq!(child_row["origin"], "internal");
    assert_eq!(child_row["parentSessionId"], conversation);

    let attached = client.rpc("session.attach", json!({"sessionId":acp})).await;
    assert_ok(&attached);
    assert_eq!(attached["result"]["origin"], "acp");
    assert!(attached["result"].get("parentSessionId").is_none());
    let attached = client
        .rpc("session.attach", json!({"sessionId":child}))
        .await;
    assert_ok(&attached);
    assert_eq!(attached["result"]["origin"], "internal");
    assert_eq!(attached["result"]["parentSessionId"], conversation);

    let summaries = client.rpc("session.summaries", json!({})).await;
    assert_ok(&summaries);
    let rows = summaries["result"].as_array().unwrap();
    let found = |id: &str| -> Value {
        rows.iter()
            .find(|row| row["sessionId"] == id)
            .unwrap_or_else(|| panic!("{id} is missing from session.summaries"))
            .clone()
    };
    assert_eq!(found(&child)["origin"], "internal");
    assert_eq!(found(&acp)["origin"], "acp");
    assert_eq!(found(&conversation)["origin"], "client");

    let cursor = client
        .rpc("session.catalog", json!({"limit":1,"withAcpSessions":true}))
        .await;
    assert_ok(&cursor);
    let cursor = cursor["result"]["nextCursor"].as_str().unwrap().to_owned();
    for params in [
        json!({"limit":1,"cursor":cursor}),
        json!({"limit":1,"cursor":cursor,"withAcpSessions":true,"withInternal":true}),
    ] {
        let reply = client.rpc("session.catalog", params).await;
        assert_eq!(reply["error"]["code"], -32602, "{reply}");
    }
    let continued = client
        .rpc(
            "session.catalog",
            json!({"limit":1,"cursor":cursor,"withAcpSessions":true}),
        )
        .await;
    assert_ok(&continued);
    drop(client);
    drop(agent);
    server.shutdown();
    server.join().await.unwrap();
}

/// #557: both class filters are conditions of the catalog query, so a page
/// behind 2000 hidden Sessions still fills with the rows the client shows.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn catalog_filters_creation_origin_before_the_page_boundary() {
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
    let seed_row = |tx: &rusqlite::Transaction<'_>,
                    id: &str,
                    created: i64,
                    origin: &str,
                    cwd: &str| {
        tx.execute(
            "INSERT INTO agent_sessions (session_id, config, created_at_ms, origin) SELECT ?1, config, ?3, ?4 FROM agent_sessions WHERE session_id=?2",
            params![id, seed, created, origin],
        )
        .unwrap();
        tx.execute(
            "INSERT INTO session_owners SELECT ?1, agent_id, protocol_version FROM session_owners WHERE session_id=?2",
            params![id, seed],
        )
        .unwrap();
        tx.execute(
            "INSERT INTO timeline_branches VALUES (?1,'main','{\"id\":\"main\"}',0)",
            [id],
        )
        .unwrap();
        initialize_session_catalog(tx, id).unwrap();
        tx.execute(
            "UPDATE session_catalog SET cwd=?2 WHERE session_id=?1",
            params![id, cwd],
        )
        .unwrap();
    };
    for index in 0..2000 {
        seed_row(
            &tx,
            &format!("internal-{index:04}"),
            9_000_000_000_000 + i64::from(index),
            "internal",
            &format!("/internal/{index}"),
        );
    }
    for index in 0..60 {
        seed_row(
            &tx,
            &format!("conversation-{index:02}"),
            8_000_000_000_000 + i64::from(index),
            "client",
            &format!("/conversation/{index}"),
        );
    }
    tx.commit().unwrap();

    let (server, agent) = launch_with_agent(Arc::new(MockTools), store.config()).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let start = std::time::Instant::now();
    let page = client.rpc("session.catalog", json!({"limit":50})).await;
    eprintln!(
        "2060 Sessions, 2000 internal in front: default first page {:?}",
        start.elapsed()
    );
    assert_ok(&page);
    let ids: Vec<String> = page["result"]["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["sessionId"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        ids.len(),
        50,
        "the page must be full of rows the client shows"
    );
    assert!(
        ids.iter().all(|id| id.starts_with("conversation-")),
        "{ids:?}"
    );
    let internal = client
        .rpc("session.catalog", json!({"limit":50,"withInternal":true}))
        .await;
    assert_ok(&internal);
    assert!(
        internal["result"]["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["sessionId"].as_str().unwrap().starts_with("internal-")),
        "{internal}"
    );
    drop(client);
    drop(agent);
    server.shutdown();
    server.join().await.unwrap();
}

/// #920: `cwd` accepts a set of workspaces, and one page lists all of them in
/// activity order.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn catalog_lists_and_pages_a_set_of_workspaces() {
    let store = seeded_workspace_store(&[("/workspace/a", 1000), ("/workspace/b", 2000)]).await;
    let (server, agent) = launch_with_agent(Arc::new(MockTools), store.config()).await;
    let mut client = Client::new_version(&server, false, 3).await;
    assert!(
        client
            .features
            .as_array()
            .unwrap()
            .contains(&json!("session.catalog.workspaces")),
        "a store-backed daemon advertises the set"
    );
    let listed = |value: &Value| -> Vec<String> {
        value["result"]["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["sessionId"].as_str().unwrap().to_owned())
            .collect()
    };
    let first = client
        .rpc(
            "session.catalog",
            json!({"cwd":["/workspace/a","/workspace/b"],"limit":4}),
        )
        .await;
    assert_ok(&first);
    assert_eq!(
        listed(&first),
        ["b-2", "b-1", "b-0", "a-2"],
        "the set lists both workspaces in activity order"
    );
    let cursor = first["result"]["nextCursor"].as_str().unwrap().to_owned();
    let second = client
        .rpc(
            "session.catalog",
            json!({"cwd":["/workspace/a","/workspace/b"],"limit":4,"cursor":cursor}),
        )
        .await;
    assert_ok(&second);
    assert_eq!(listed(&second), ["a-1", "a-0"]);
    assert!(second["result"].get("nextCursor").is_none());
    let one = client
        .rpc("session.catalog", json!({"cwd":"/workspace/a"}))
        .await;
    assert_ok(&one);
    assert_eq!(listed(&one), ["a-2", "a-1", "a-0"]);
    drop(client);
    drop(agent);
    server.shutdown();
    server.join().await.unwrap();
}

/// A cursor continues under the set it was taken under and no other: the set
/// compares in canonical order, so a reordered set and a one-element list are
/// the same filter.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn catalog_cursor_belongs_to_one_set_of_workspaces() {
    let store = seeded_workspace_store(&[("/workspace/a", 1000), ("/workspace/b", 2000)]).await;
    let (server, agent) = launch_with_agent(Arc::new(MockTools), store.config()).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let first = client
        .rpc(
            "session.catalog",
            json!({"cwd":["/workspace/a","/workspace/b"],"limit":2}),
        )
        .await;
    assert_ok(&first);
    let cursor = first["result"]["nextCursor"].as_str().unwrap().to_owned();
    for params in [
        json!({"cwd":["/workspace/a","/workspace/c"],"limit":2,"cursor":cursor}),
        json!({"cwd":"/workspace/a","limit":2,"cursor":cursor}),
    ] {
        let reply = client.rpc("session.catalog", params).await;
        assert_eq!(reply["error"]["code"], -32602, "{reply}");
    }
    let reordered = client
        .rpc(
            "session.catalog",
            json!({"cwd":["/workspace/b","/workspace/a"],"limit":2,"cursor":cursor}),
        )
        .await;
    assert_ok(&reordered);
    let single = client
        .rpc("session.catalog", json!({"cwd":"/workspace/a","limit":1}))
        .await;
    assert_ok(&single);
    let cursor = single["result"]["nextCursor"].as_str().unwrap().to_owned();
    let listed = client
        .rpc(
            "session.catalog",
            json!({"cwd":["/workspace/a"],"limit":1,"cursor":cursor}),
        )
        .await;
    assert_ok(&listed);
    assert_eq!(listed["result"]["sessions"][0]["sessionId"], "a-1");
    drop(client);
    drop(agent);
    server.shutdown();
    server.join().await.unwrap();
}

/// A `cwd` the catalog cannot read as a set is refused as invalid parameters.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn catalog_refuses_a_cwd_that_is_not_a_set_of_workspaces() {
    let store = seeded_workspace_store(&[("/workspace/a", 1000)]).await;
    let (server, agent) = launch_with_agent(Arc::new(MockTools), store.config()).await;
    let mut client = Client::new_version(&server, false, 3).await;
    for cwd in [json!([]), json!(["/workspace/a", 7])] {
        let reply = client.rpc("session.catalog", json!({"cwd":cwd})).await;
        assert_eq!(reply["error"]["code"], -32602, "{reply}");
        assert_eq!(reply["error"]["data"]["appError"], "invalid_params");
    }
    drop(client);
    drop(agent);
    server.shutdown();
    server.join().await.unwrap();
}

/// A daemon without a native store has no catalog, so it lists no set of
/// workspaces either.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn catalog_workspaces_is_not_advertised_without_native_storage() {
    let server = launch(Arc::new(MockTools), ServerConfig::default()).await;
    let client = Client::new_version(&server, false, 3).await;
    assert!(
        !client
            .features
            .as_array()
            .unwrap()
            .contains(&json!("session.catalog.workspaces"))
    );
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
}

/// A store holding three Sessions in each of `workspaces`, in the activity
/// order `workspaces` gives: the later workspace's Sessions come first.
async fn seeded_workspace_store(workspaces: &[(&str, i64)]) -> Store {
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
    for (workspace, created) in workspaces {
        for index in 0..3 {
            let id = format!(
                "{}-{index}",
                workspace.rsplit('/').next().unwrap_or_default()
            );
            let created = created + i64::from(index);
            tx.execute(
                "INSERT INTO agent_sessions (session_id, config, created_at_ms) SELECT ?1, config, ?3 FROM agent_sessions WHERE session_id=?2",
                params![id, seed, created],
            )
            .unwrap();
            tx.execute(
                "INSERT INTO session_owners SELECT ?1, agent_id, protocol_version FROM session_owners WHERE session_id=?2",
                params![id, seed],
            )
            .unwrap();
            tx.execute(
                "INSERT INTO timeline_branches VALUES (?1,'main','{\"id\":\"main\"}',0)",
                [&id],
            )
            .unwrap();
            initialize_session_catalog(&tx, &id).unwrap();
            tx.execute(
                "UPDATE session_catalog SET cwd=?2 WHERE session_id=?1",
                params![id, workspace],
            )
            .unwrap();
        }
    }
    tx.commit().unwrap();
    store
}

/// The `updatedAt` row of `id` in one listing method's reply.
async fn listed_updated_at(client: &mut Client, id: &str, method: &str) -> i64 {
    let reply = client.rpc(method, json!({})).await;
    let rows = match &reply["result"] {
        Value::Array(rows) => rows,
        Value::Object(_) => reply["result"]["sessions"]
            .as_array()
            .unwrap_or_else(|| panic!("{method} reply: {reply}")),
        _ => panic!("{method} reply: {reply}"),
    };
    rows.iter()
        .find(|entry| entry["sessionId"] == id)
        .unwrap_or_else(|| panic!("{id} is missing from {method}: {reply}"))["updatedAt"]
        .as_u64()
        .unwrap() as i64
}

/// #1099: a Session's activity time follows its own activity. A Session
/// created and never written to sits at its creation time; a user message
/// moves the row while its turn still runs, and the completed turn moves it
/// again; and both listings report the same value.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn catalog_activity_time_follows_user_activity_not_housekeeping() {
    use super::deletion::{eventually, turn};
    let store = Store::new();
    let (server, agent) = launch_with_agent(Arc::new(MockTools), store.config()).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let created = client.rpc("session.create", create(json!([]))).await;
    assert_ok(&created);
    let id = created["result"]["sessionId"].as_str().unwrap().to_owned();

    // The stored projection and the newest activity event on `main`.
    let row = || -> i64 {
        store
            .connection()
            .query_row(
                "SELECT updated_at_ms FROM session_catalog WHERE session_id=?1",
                [&id],
                |row| row.get(0),
            )
            .unwrap()
    };
    let activity = || -> i64 {
        store
            .connection()
            .query_row(
                "SELECT json_extract(body, '$.record.event.create_at') FROM timeline_events \
                 WHERE session_id=?1 AND branch_id='main' \
                   AND kind IN ('input/message','turn/end','branch/create','session/forked') \
                   AND NOT (kind='input/message' AND json_extract(body,'$.record.event.data.user') IS NULL) \
                 ORDER BY sequence DESC LIMIT 1",
                [&id],
                |row| row.get(0),
            )
            .unwrap()
    };

    // A Session created and never written to sits at its creation time, in
    // both listings.
    let creation_time = row();
    assert_eq!(
        listed_updated_at(&mut client, &id, "session.summaries").await,
        creation_time
    );
    assert_eq!(
        listed_updated_at(&mut client, &id, "session.catalog").await,
        creation_time
    );

    // A user message moves the row to its own time while the turn runs; the
    // value is the same in the projection and both listings. The turn's
    // `tool:wait` holds it 400 ms, so the row is read between the message's
    // commit and the turn's end.
    assert_ok(
        &client
            .rpc(
                "session.submit",
                json!({"sessionId":id,"branchId":"main","text":"tool:wait {\"millis\":400}"}),
            )
            .await,
    );
    eventually("the input message moves the activity row", || {
        row() > creation_time
    })
    .await;
    let message_time = activity();
    assert!(message_time > creation_time);
    assert_eq!(row(), message_time);
    assert_eq!(
        listed_updated_at(&mut client, &id, "session.summaries").await,
        message_time
    );
    assert_eq!(
        listed_updated_at(&mut client, &id, "session.catalog").await,
        message_time
    );
    assert_ok(
        &client
            .rpc("session.idle", json!({"sessionId":id,"branchId":"main"}))
            .await,
    );

    // The completed turn moves it again.
    let end_time = activity();
    assert!(end_time > message_time);
    assert_eq!(row(), end_time);
    assert_eq!(
        listed_updated_at(&mut client, &id, "session.summaries").await,
        end_time
    );
    assert_eq!(
        listed_updated_at(&mut client, &id, "session.catalog").await,
        end_time
    );

    // Another full turn moves it the same way.
    turn(&mut client, &id, "second turn").await;
    let second = activity();
    assert!(second > end_time);
    assert_eq!(row(), second);
    drop(client);
    drop(agent);
    server.shutdown();
    server.join().await.unwrap();
}

/// #1171: a Session this daemon closed stays out of the catalog until the
/// read asks for it; `includeClosed` lists it marked `closed`, an unknown
/// parameter is still refused, and a restart forgets the close, so the
/// default read lists the Session again with no closed mark.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn catalog_lists_a_closed_session_only_when_asked() {
    use super::deletion;
    let store = Store::new();
    let (server, agent) = launch_with_agent(Arc::new(MockTools), store.config()).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let open = deletion::session(&mut client).await;
    let closed = deletion::session(&mut client).await;
    assert_ok(
        &client
            .rpc("session.close", json!({"sessionId":closed}))
            .await,
    );
    assert!(
        client
            .features
            .as_array()
            .unwrap()
            .contains(&json!("session.catalog.includeClosed")),
        "the feature is advertised: {:?}",
        client.features
    );

    let page = client.rpc("session.catalog", json!({})).await;
    assert_ok(&page);
    let rows = page["result"]["sessions"].as_array().unwrap();
    assert!(
        rows.iter().any(|row| row["sessionId"] == open),
        "the open Session is listed: {page}"
    );
    assert!(
        !rows.iter().any(|row| row["sessionId"] == closed),
        "the closed Session is not listed: {page}"
    );
    assert!(
        rows.iter().all(|row| row["closed"] == false),
        "every row carries closed: {page}"
    );

    let including = client
        .rpc("session.catalog", json!({"includeClosed":true}))
        .await;
    assert_ok(&including);
    let rows = including["result"]["sessions"].as_array().unwrap();
    let row = rows
        .iter()
        .find(|row| row["sessionId"] == closed)
        .unwrap_or_else(|| panic!("the closed Session is listed: {including}"));
    assert_eq!(row["closed"], true, "{including}");
    assert!(
        rows.iter()
            .filter(|row| row["sessionId"] != closed)
            .all(|row| row["closed"] == false),
        "{including}"
    );

    let refused = client
        .rpc("session.catalog", json!({"unknownField":true}))
        .await;
    assert_eq!(refused["error"]["code"], -32602, "{refused}");
    assert_eq!(
        refused["error"]["data"]["appError"], "invalid_params",
        "{refused}"
    );

    drop(client);
    drop(agent);
    server.shutdown();
    server.join().await.unwrap();

    // The closed flag is process-local: a daemon that restarts on the same
    // store lists the Session again, with no closed mark.
    let (server, agent) = launch_with_agent(Arc::new(MockTools), store.config()).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let page = client.rpc("session.catalog", json!({})).await;
    assert_ok(&page);
    let rows = page["result"]["sessions"].as_array().unwrap();
    let row = rows
        .iter()
        .find(|row| row["sessionId"] == closed)
        .unwrap_or_else(|| panic!("the restarted daemon lists it: {page}"));
    assert_eq!(row["closed"], false, "{page}");
    drop(client);
    drop(agent);
    server.shutdown();
    server.join().await.unwrap();
}

/// #1171: `includeClosed` is part of the cursor's filter set, so a cursor
/// taken under one filter is refused by the other and continues under its
/// own.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn catalog_cursor_belongs_to_one_include_closed_filter() {
    use super::deletion;
    let store = Store::new();
    let (server, agent) = launch_with_agent(Arc::new(MockTools), store.config()).await;
    let mut client = Client::new_version(&server, false, 3).await;
    for _ in 0..3 {
        deletion::session(&mut client).await;
    }
    let first = client.rpc("session.catalog", json!({"limit":1})).await;
    assert_ok(&first);
    let open_cursor = first["result"]["nextCursor"].as_str().unwrap().to_owned();
    let first_including = client
        .rpc("session.catalog", json!({"limit":1,"includeClosed":true}))
        .await;
    assert_ok(&first_including);
    let closed_cursor = first_including["result"]["nextCursor"]
        .as_str()
        .unwrap()
        .to_owned();
    for params in [
        json!({"limit":1,"cursor":open_cursor,"includeClosed":true}),
        json!({"limit":1,"cursor":closed_cursor}),
    ] {
        let reply = client.rpc("session.catalog", params).await;
        assert_eq!(reply["error"]["code"], -32602, "{reply}");
        assert_eq!(
            reply["error"]["data"]["appError"], "invalid_params",
            "{reply}"
        );
    }
    assert_ok(
        &client
            .rpc("session.catalog", json!({"limit":1,"cursor":open_cursor}))
            .await,
    );
    assert_ok(
        &client
            .rpc(
                "session.catalog",
                json!({"limit":1,"cursor":closed_cursor,"includeClosed":true}),
            )
            .await,
    );
    drop(client);
    drop(agent);
    server.shutdown();
    server.join().await.unwrap();
}

use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn configuration_management_preserves_live_intent_and_separates_save_acknowledgement() {
    use std::os::unix::fs::PermissionsExt;
    let home = std::path::PathBuf::from("/tmp").join(format!("dc-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&home).unwrap();
    std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
    let native = home.join("rust");
    let (server, _) = launch_with_agent(
        Arc::new(MockTools),
        ServerConfig {
            product_home: Some(home.clone()),
            target_home: Some(native.clone()),
            ..Default::default()
        },
    )
    .await;
    let mut client = Client::new_version(&server, false, 3).await;
    let mut legacy = Client::new_version(&server, false, 2).await;
    let mut remote = Client::new_version(&server, true, 3).await;
    assert!(
        client
            .features
            .as_array()
            .unwrap()
            .contains(&json!("mcp.save"))
    );
    assert!(
        !remote
            .features
            .as_array()
            .unwrap()
            .contains(&json!("mcp.save"))
    );
    assert_eq!(
        legacy.rpc("mcp.catalog", json!({})).await["error"]["code"],
        -32601
    );
    assert_eq!(
        remote.rpc("mcp.catalog", json!({})).await["error"]["data"]["appError"],
        "local_transport_required"
    );
    let before = client.rpc("mcp.catalog", json!({"cwd":home})).await;
    assert_ok(&before);
    let mut edit = json!({"source":"user", "id":"private", "expectedVersion":before["result"]["userRevision"],
        "configuration":{"command":"never-start-this-fixture","enabled":false,"env":{"TOKEN":"secret-never-return"}}});
    let mut wrong_source = edit.clone();
    wrong_source["expectedVersion"] = before["result"]["projectRevision"].clone();
    assert_eq!(
        client.rpc("mcp.save", wrong_source).await["error"]["data"]["appError"],
        "mcp_settings_conflict"
    );
    assert_ok(&client.rpc("mcp.save", edit.clone()).await);
    let saved = client.rpc("mcp.catalog", json!({})).await;
    assert_ok(&saved);
    assert_ne!(
        saved["result"]["userRevision"],
        before["result"]["userRevision"]
    );
    assert_eq!(saved["result"]["servers"][0]["id"], "private");
    assert_eq!(saved["result"]["servers"][0]["enabled"], false);
    assert!(!saved.to_string().contains("secret-never-return"));
    assert_eq!(
        client.rpc("mcp.save", edit.clone()).await["error"]["data"]["appError"],
        "mcp_settings_conflict"
    );
    edit["expectedVersion"] = saved["result"]["userRevision"].clone();
    edit["configuration"]["enabled"] = json!(true);
    // Failed persistence is not an applied-but-unsaved edit: both the source
    // revision and its prior document remain authoritative.
    let fault = rusqlite::Connection::open(native.join("dimcode.sqlite")).unwrap();
    fault.execute_batch("CREATE TRIGGER fail_save BEFORE UPDATE ON settings_documents BEGIN SELECT RAISE(ABORT, 'secret-never-return'); END").unwrap();
    let failed = client.rpc("mcp.save", edit.clone()).await;
    assert_eq!(failed["error"]["data"]["appError"], "mcp_settings_read");
    assert!(!failed.to_string().contains("secret-never-return"));
    let unchanged = client.rpc("mcp.catalog", json!({})).await;
    assert_ok(&unchanged);
    assert_eq!(
        unchanged["result"]["userRevision"],
        saved["result"]["userRevision"]
    );
    assert_eq!(unchanged["result"]["servers"], saved["result"]["servers"]);
    fault.execute_batch("DROP TRIGGER fail_save").unwrap();
    drop(fault);
    server.shutdown();
    server.join().await.unwrap();
    // Restart reads committed source state, not an in-memory loader edit or a
    // rewritten legacy file. The failed edit must not become durable later.
    let (server, _) = launch_with_agent(
        Arc::new(MockTools),
        ServerConfig {
            product_home: Some(home.clone()),
            target_home: Some(native),
            ..Default::default()
        },
    )
    .await;
    let mut client = Client::new_version(&server, false, 3).await;
    let restored = client.rpc("mcp.catalog", json!({})).await;
    assert_ok(&restored);
    assert_eq!(
        restored["result"]["userRevision"],
        saved["result"]["userRevision"]
    );
    assert_eq!(restored["result"]["servers"], saved["result"]["servers"]);
    // The unchanged revision remains usable for a successful retry.
    assert_ok(&client.rpc("mcp.save", edit).await);
    let retried = client.rpc("mcp.catalog", json!({})).await;
    assert_ok(&retried);
    assert_ne!(
        retried["result"]["userRevision"],
        saved["result"]["userRevision"]
    );
    assert_eq!(retried["result"]["servers"][0]["enabled"], true);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reconnect_locates_a_published_submission_without_resubmitting() {
    for ws in [false, true] {
        let (server, agent) = launch_with_agent(Arc::new(MockTools), ServerConfig::default()).await;
        let mut client = Client::new_version(&server, ws, 3).await;
        let created = client.rpc("session.create", create(json!([]))).await;
        assert_ok(&created);
        let sid = created["result"]["sessionId"].as_str().unwrap();
        let session = agent.get_session(sid).unwrap();
        let submission = uuid::Uuid::new_v4().to_string();
        let mut dispatched = session.dispatch_watch("main").unwrap();
        client
            .send(
                "session.submit",
                json!({
                    "sessionId":sid,"branchId":"main","submissionId":submission,"text":"recover me"
                }),
            )
            .await;
        // Observe server admission without reading its RPC response.
        let admitted = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(event) = session
                    .history("main", None)
                    .await
                    .unwrap()
                    .into_iter()
                    .find(|event| {
                        matches!(&event.kind, EventKind::InputMessage(data)
                        if data.submission_id.as_deref() == Some(&submission))
                    })
                {
                    break event;
                }
                dispatched.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
        drop(client);
        let mut reconnect = Client::new_version(&server, !ws, 3).await;
        let lookup = json!({"sessionId":sid,"branchId":"main","submissionId":submission});
        let recovered = reconnect.rpc("session.input_lookup", lookup.clone()).await;
        assert_ok(&recovered);
        assert_eq!(recovered["result"]["status"], "accepted");
        assert_eq!(
            recovered["result"]["event"],
            serde_json::to_value(&admitted).unwrap()
        );
        let absent = reconnect
            .rpc(
                "session.input_lookup",
                json!({
                    "sessionId":sid,"branchId":"main","submissionId":uuid::Uuid::new_v4().to_string()
                }),
            )
            .await;
        assert_eq!(absent["result"], json!({"status":"unknown"}));
        // Correlation identities do not silently deduplicate new submissions.
        assert_ok(&reconnect.rpc("session.submit", json!({
            "sessionId":sid,"branchId":"main","submissionId":submission,"text":"another input"
        })).await);
        let ambiguous = reconnect.rpc("session.input_lookup", lookup).await;
        assert_eq!(
            ambiguous["error"]["data"]["appError"],
            "submission_ambiguous"
        );
        for identity in [json!("invalid"), json!(null), json!(5)] {
            let rejected = reconnect.rpc("session.submit", json!({
                "sessionId":sid,"branchId":"main","submissionId":identity,"text":"invalid identity"
            })).await;
            assert_eq!(rejected["error"]["code"], -32602);
        }
        let mut legacy = Client::new_version(&server, ws, 2).await;
        let rejected = legacy
            .rpc(
                "session.history",
                json!({
                    "sessionId":sid,"branchId":"main","after":0,"limit":1
                }),
            )
            .await;
        assert_eq!(rejected["error"]["code"], -32601);
        let rejected = legacy
            .rpc(
                "session.input_lookup",
                json!({
                    "sessionId":sid,"branchId":"main","submissionId":submission
                }),
            )
            .await;
        assert_eq!(rejected["error"]["code"], -32601);
        server.shutdown();
        server.join().await.unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fixed_history_pages_share_live_projection_and_advance_across_filtered_records() {
    for ws in [false, true] {
        let (server, agent) = launch_with_agent(Arc::new(MockTools), ServerConfig::default()).await;
        let mut client = Client::new_version(&server, ws, 3).await;
        let mut params = create(json!([]));
        params["systemPrompt"] = json!("private system prompt");
        let created = client.rpc("session.create", params).await;
        assert_ok(&created);
        let sid = created["result"]["sessionId"].as_str().unwrap();
        let session = agent.get_session(sid).unwrap();
        let initial = session.sequence("main").unwrap();
        assert_ok(&client.rpc("session.attach", json!({"sessionId":sid})).await);
        assert_ok(
            &client
                .rpc(
                    "session.submit",
                    json!({
                        "sessionId":sid,"branchId":"main","text":"first"
                    }),
                )
                .await,
        );
        assert_ok(
            &client
                .rpc("session.idle", json!({"sessionId":sid,"branchId":"main"}))
                .await,
        );
        let first = client
            .rpc(
                "session.history",
                json!({
                    "sessionId":sid,"branchId":"main","after":0,"limit":1
                }),
            )
            .await;
        assert_ok(&first);
        let through = first["result"]["through"].as_u64().unwrap();
        let expected: Vec<_> = client
            .notices
            .iter()
            .filter(|notice| {
                notice["method"] == "session.event"
                    && notice["params"]["branchId"] == "main"
                    && notice["params"]["event"]["sequence"].as_u64().unwrap() <= through
            })
            .map(|notice| notice["params"]["event"].clone())
            .collect();
        let second = client
            .rpc(
                "session.submit",
                json!({
                    "sessionId":sid,"branchId":"main","text":"later"
                }),
            )
            .await;
        assert_ok(&second);
        assert_ok(
            &client
                .rpc("session.idle", json!({"sessionId":sid,"branchId":"main"}))
                .await,
        );
        assert!(session.sequence("main").unwrap() > through);
        let mut page = first["result"].clone();
        let mut all = Vec::new();
        let mut filtered_page = false;
        loop {
            assert_eq!(page["through"], through);
            let events = page["events"].as_array().unwrap();
            filtered_page |= events.is_empty();
            all.extend(events.iter().cloned());
            if page["done"] == true {
                break;
            }
            let after = page["nextAfter"].as_u64().unwrap();
            let response = client
                .rpc(
                    "session.history",
                    json!({
                        "sessionId":sid,"branchId":"main","after":after,"limit":1,"through":through
                    }),
                )
                .await;
            assert_ok(&response);
            page = response["result"].clone();
            assert!(page["nextAfter"].as_u64().unwrap() > after);
        }
        assert!(
            filtered_page,
            "filtered pages must advance without visible events"
        );
        assert_eq!(page["nextAfter"], through);
        let live_part: Vec<_> = all
            .iter()
            .filter(|event| event["sequence"].as_u64().unwrap() > initial)
            .cloned()
            .collect();
        assert_eq!(live_part, expected);
        assert!(
            all.windows(2)
                .all(|pair| pair[0]["sequence"].as_u64().unwrap()
                    < pair[1]["sequence"].as_u64().unwrap())
        );
        assert!(
            !serde_json::to_string(&all)
                .unwrap()
                .contains("private system prompt")
        );
        assert!(
            !all.iter()
                .any(|event| event["id"] == second["result"]["event"]["id"])
        );
        let complete = client.rpc("session.history", json!({
            "sessionId":sid,"branchId":"main","after":through,"limit":9007199254740991u64,"through":through
        })).await;
        assert_eq!(
            complete["result"],
            json!({"events":[],"through":through,"nextAfter":through,"done":true})
        );
        for invalid in [
            json!({"after":-1,"limit":1}),
            json!({"after":0.5,"limit":1}),
            json!({"after":9007199254740992u64,"limit":1}),
            json!({"after":0,"limit":0}),
            json!({"after":0,"limit":1.5}),
            json!({"after":0,"limit":9007199254740992u64}),
            json!({"after":0,"limit":1,"through":null}),
            json!({"after":through+1,"limit":1,"through":through}),
            json!({"after":0,"limit":1,"through":session.sequence("main").unwrap()+1}),
        ] {
            let mut params = invalid;
            params["sessionId"] = json!(sid);
            params["branchId"] = json!("main");
            let rejected = client.rpc("session.history", params).await;
            assert_eq!(rejected["error"]["code"], -32602, "{rejected}");
        }
        server.shutdown();
        server.join().await.unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn local_product_management_starts_without_configuration_and_isolates_invalid_sources() {
    let home = std::env::temp_dir().join(format!(
        "dim-p-{}",
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    ));
    std::fs::create_dir_all(&home).unwrap();
    let home = std::fs::canonicalize(home).unwrap();
    let broken = b"{private-configuration-secret";
    std::fs::write(home.join("mcp.json"), broken).unwrap();
    std::fs::write(home.join("skills.json"), broken).unwrap();
    let native = home.join("rust");
    std::fs::create_dir_all(&native).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&native, std::fs::Permissions::from_mode(0o700)).unwrap();
    let server = serve(
        AgentBuilder::new(AgentConfig::default()),
        ServerConfig {
            unix_path: Some(native.join("daemon.sock")),
            product_home: Some(home.clone()),
            target_home: Some(native),
            session_execution: false,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert!(server.agent().is_none());
    assert!(
        !home.join("auth.json").exists(),
        "startup must not read or generate account state"
    );
    let mut local = Client::new_version(&server, false, 3).await;
    assert!(
        local
            .features
            .as_array()
            .unwrap()
            .contains(&json!("account.providers"))
    );
    assert!(
        !local
            .features
            .as_array()
            .unwrap()
            .contains(&json!("session.history"))
    );
    let empty = local.rpc("account.providers", json!({})).await;
    assert_ok(&empty);
    assert_eq!(empty["result"], json!([]));
    assert_eq!(
        local
            .rpc("account.status", json!({"providerId":"managed-fixture"}))
            .await["error"]["data"]["appError"],
        "account_provider_unavailable"
    );
    let managed_empty = json!({"authenticated":false,"recoveryRequired":false,"expiresAt":null,"accountIdPreview":null});
    assert_eq!(
        local.rpc("account.codex.status", json!({})).await["result"],
        managed_empty
    );
    assert_eq!(
        local.rpc("account.grok.status", json!({})).await["result"],
        managed_empty
    );
    let codex = local
        .rpc("account.codex.login.browser.begin", json!({}))
        .await;
    assert_ok(&codex);
    let attempt = codex["result"]["attemptId"].as_str().unwrap();
    assert_eq!(
        local
            .rpc("account.codex.login.status", json!({"attemptId":attempt}))
            .await["result"]["status"],
        "pending"
    );
    assert_ok(
        &local
            .rpc("account.codex.login.cancel", json!({"attemptId":attempt}))
            .await,
    );
    assert_eq!(
        local
            .rpc("account.codex.login.status", json!({"attemptId":attempt}))
            .await["result"]["status"],
        "cancelled"
    );
    assert_eq!(
        local
            .rpc(
                "account.codex.login.browser.complete",
                json!({"attemptId":attempt,"callbackUrl":"https://outside.test/?code=private"})
            )
            .await["error"]["data"]["appError"],
        "login_attempt_closed"
    );
    assert_eq!(
        local
            .rpc(
                "account.grok.login.status",
                json!({"attemptId":uuid::Uuid::new_v4()})
            )
            .await["error"]["data"]["appError"],
        "login_attempt_unknown"
    );
    assert_eq!(
        local
            .rpc("account.recover", json!({"providerId":"managed-fixture"}))
            .await["error"]["data"]["appError"],
        "account_provider_unavailable"
    );
    assert_eq!(
        local
            .rpc("provider.catalog", json!({"providerId":"missing"}))
            .await["result"],
        json!({"catalog":null})
    );
    assert_eq!(
        local.rpc("provider.selection", json!({"cwd":home})).await["result"],
        json!({"selection":null})
    );
    assert_eq!(
        local
            .rpc("provider.selection", json!({"cwd":"relative"}))
            .await["error"]["data"]["appError"],
        "invalid_params"
    );
    assert_eq!(
        local.rpc("session.create", create(json!([]))).await["error"]["data"]["appError"],
        "product_integration_pending"
    );
    assert_eq!(
        local
            .rpc(
                "account.login.status",
                json!({"providerId":"managed-fixture","attemptId":"invalid"})
            )
            .await["error"]["data"]["appError"],
        "account_provider_unavailable"
    );
    let mut remote = Client::new_version(&server, true, 3).await;
    assert_eq!(remote.features, json!([]));
    assert_eq!(
        remote
            .rpc(
                "provider.import_legacy",
                json!({"sourcePath":"/unused/legacy.sqlite"})
            )
            .await["error"]["data"]["appError"],
        "local_transport_required"
    );
    for method in [
        "account.status",
        "account.logout",
        "account.codex.status",
        "account.codex.logout",
        "account.grok.status",
        "account.grok.logout",
        "provider.refresh",
    ] {
        assert_eq!(
            remote
                .rpc(
                    method,
                    if method.starts_with("account.codex") || method.starts_with("account.grok") {
                        json!({})
                    } else {
                        json!({"providerId":"managed-fixture"})
                    }
                )
                .await["error"]["data"]["appError"],
            "local_transport_required"
        );
    }
    let mut old = Client::new_version(&server, false, 2).await;
    for (method, params) in [
        (
            "provider.import_legacy",
            json!({"sourcePath":"/unused/legacy.sqlite"}),
        ),
        ("account.recover", json!({"providerId":"managed-fixture"})),
        (
            "account.login.browser.begin",
            json!({"providerId":"managed-fixture","directory":"https://issuer.example","redirectUri":"http://localhost:54321/auth/callback"}),
        ),
        (
            "account.login.browser.complete",
            json!({"providerId":"managed-fixture","attemptId":uuid::Uuid::new_v4().to_string(),"callbackUrl":"http://localhost:54321/auth/callback?code=private&state=state"}),
        ),
        (
            "provider.models.set_enabled",
            json!({"providerId":"managed-fixture","expectedVersion":0,"modelIds":[]}),
        ),
        ("account.codex.status", json!({})),
        ("account.codex.recover", json!({})),
        ("account.codex.login.browser.begin", json!({})),
        (
            "account.codex.login.browser.complete",
            json!({"attemptId":uuid::Uuid::new_v4(),"callbackUrl":"http://localhost:1455/auth/callback?code=private&state=state"}),
        ),
        (
            "account.codex.login.status",
            json!({"attemptId":uuid::Uuid::new_v4()}),
        ),
        (
            "account.codex.login.cancel",
            json!({"attemptId":uuid::Uuid::new_v4()}),
        ),
        ("account.codex.logout", json!({})),
        ("account.grok.status", json!({})),
        ("account.grok.recover", json!({})),
        ("account.grok.login.begin", json!({})),
        (
            "account.grok.login.status",
            json!({"attemptId":uuid::Uuid::new_v4()}),
        ),
        (
            "account.grok.login.cancel",
            json!({"attemptId":uuid::Uuid::new_v4()}),
        ),
        ("account.grok.logout", json!({})),
    ] {
        assert!(local.features.as_array().unwrap().contains(&json!(method)));
        assert_eq!(
            remote.rpc(method, params.clone()).await["error"]["data"]["appError"],
            "local_transport_required"
        );
        assert_eq!(
            old.rpc(method, params).await["error"]["data"]["appError"],
            "unknown_method"
        );
    }
    for version in [1, 2, 3] {
        let mut remote = Client::new_version(&server, true, version).await;
        for force in [false, true] {
            assert_eq!(
                remote.rpc("daemon.shutdown", json!({"force":force})).await["error"]["data"]["appError"],
                "local_transport_required"
            );
        }
    }
    assert_eq!(
        old.rpc("account.status", json!({"providerId":"managed-fixture"}))
            .await["error"]["data"]["appError"],
        "unknown_method"
    );
    for method in ["account.codex.status", "account.grok.status"] {
        assert_eq!(
            old.rpc(method, json!({})).await["error"]["data"]["appError"],
            "unknown_method"
        );
    }
    std::fs::write(home.join("auth.json"), broken).unwrap();
    for method in ["account.codex.status", "account.grok.status"] {
        let invalid = local.rpc(method, json!({})).await;
        assert_eq!(
            invalid["error"]["data"]["appError"],
            "account_invalid_store"
        );
        assert!(!invalid.to_string().contains("private-configuration-secret"));
    }
    assert_ok(&local.rpc("daemon.ping", json!({})).await);
    assert_eq!(
        local.rpc("session.list", json!({})).await["result"],
        json!([])
    );
    for file in ["auth.json", "mcp.json", "skills.json"] {
        assert_eq!(std::fs::read(home.join(file)).unwrap(), broken);
    }
    assert_ok(&local.rpc("daemon.shutdown", json!({})).await);
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn local_product_operations_use_real_tools_and_survive_rpc_reload() {
    use dim_agent_capabilities::{
        model_provider::ModelProviderPlugin, tool_registry::ToolSourcesView,
        workspace_tools::WorkspaceToolsConfig,
    };
    use dim_agent_runtime::SessionConfig;
    use dim_agent_types::{PermissionPolicy, PermissionPreset};
    use dim_app_server::mcp::McpSession;
    use std::os::unix::{ffi::OsStringExt, fs::DirBuilderExt};

    // The deterministic EchoModel interprets the last user message. Wait
    // for the capability's final Append before submitting its tool grammar.
    async fn skills_posted(session: &dim_agent_runtime::Session, enabled: bool) {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                // Prompt delivered the current view's list, which names the
                // Skill or not. The Session follows the catalog after the
                // RPC returns, so its view may still contain the previous list.
                let Ok(view) = session
                    .service::<dim_agent_capabilities::skills::SkillsBranch>("main")
                    .unwrap()
                    .session()
                    .current()
                else {
                    tokio::task::yield_now().await;
                    continue;
                };
                let current = match &view.list {
                    Some(list) => {
                        format!("<dim-state source=\"dim/skills/list\">{list}</dim-state>")
                    }
                    None => {
                        "<dim-state source=\"dim/skills/list\" cleared=\"true\"></dim-state>".into()
                    }
                };
                let events = session.history("main", None).await.unwrap();
                let delivered = events.iter().rev().find_map(|event| match &event.kind {
                    EventKind::InputMessage(data) => data.blocks.iter().find_map(|block| {
                        let dim_agent_types::InputBlock::Raw { text, .. } = block;
                        text.starts_with("<dim-state source=\"dim/skills/list\"")
                            .then(|| text.clone())
                    }),
                    _ => None,
                });
                if view.skills.contains_key("product-check") == enabled
                    && delivered.as_ref() == Some(&current)
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }

    let home = std::path::PathBuf::from("/tmp").join(format!("dp-{}", uuid::Uuid::new_v4()));
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&home)
        .unwrap();
    let native = home.join("rust");
    let work = home.join("work");
    std::fs::create_dir_all(&work).unwrap();
    std::fs::write(work.join("AGENTS.md"), "Product prompt before refresh.\n").unwrap();
    let skill_dir = work
        .join(".agents/skills")
        .join(std::ffi::OsString::from_vec(if cfg!(target_os = "linux") {
            b"product-\xff".to_vec()
        } else {
            b"product-check".to_vec()
        }));
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: product-check\ndescription: Product fixture\n---\nProduct skill body\n",
    )
    .unwrap();
    std::fs::write(home.join("skills.json"), br#"{"disabledSkillIds":[]}"#).unwrap();
    std::fs::write(home.join("mcp.json"), json!({"mcpServers":{"docs":{"type":"http","url":"https://example.invalid/private-secret","headers":{"Authorization":"private-secret"},"enabled":true}}}).to_string()).unwrap();
    let mut builder = AgentBuilder::new(AgentConfig::default());
    defaults::install_local_execution(
        &mut builder,
        ModelProviderPlugin::new,
        WorkspaceToolsConfig {
            output_root: native.join("outputs"),
        },
    );
    dim_app_server::permission_rules::install(&mut builder);
    defaults::mock_model_hosts(&mut builder);
    let server = serve(
        builder,
        ServerConfig {
            product_home: Some(home.clone()),
            target_home: Some(native.clone()),
            unix_path: Some(native.join("daemon.sock")),
            session_defaults: SessionConfig {
                tools: [
                    "read", "write", "edit", "glob", "grep", "exec", "wait", "skill",
                ]
                .map(String::from)
                .to_vec(),
                permission_policy: PermissionPolicy {
                    preset: PermissionPreset::WorkspaceWrite,
                    ..Default::default()
                },
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .await
    .unwrap();
    // A shared daemon must authorize a workspace opened after startup. Catalog
    // reads preserve intent and never expose connection configuration secrets.
    std::fs::write(work.join(".mcp.json"), json!({"mcpServers":{"docs":{"command":"missing-mcp-program","args":["private-secret"],"env":{"TOKEN":"private-secret"},"enabled":false}},"extension":"preserved"}).to_string()).unwrap();
    let mut client = Client::new_version(&server, false, 3).await;
    for method in [
        "mcp.catalog",
        "mcp.set_enabled",
        "mcp.disconnect",
        "session.mcp",
        "session.mcp.refresh",
    ] {
        assert!(client.features.as_array().unwrap().contains(&json!(method)));
    }
    // ADR 003: no host-local approval methods or catalog approval state.
    for (method, params) in [
        (
            "mcp.approve",
            json!({"cwd":work,"id":"docs","approvalToken":"any"}),
        ),
        ("mcp.revoke", json!({"cwd":work,"id":"docs"})),
    ] {
        assert!(!client.features.as_array().unwrap().contains(&json!(method)));
        assert_eq!(
            client.rpc(method, params).await["error"]["data"]["appError"],
            "unknown_method"
        );
    }
    let catalog = client.rpc("mcp.catalog", json!({"cwd":work})).await;
    assert_ok(&catalog);
    assert_eq!(
        catalog["result"]["servers"],
        json!([{"id":"docs","source":"project","enabled":false,"transport":"stdio"}])
    );
    assert!(!catalog.to_string().contains("private-secret"));
    let revision = catalog["result"]["projectRevision"].clone();
    assert_eq!(client.rpc("mcp.set_enabled", json!({"cwd":work,"id":"docs","source":"project","expectedVersion":"stale","enabled":true})).await["error"]["data"]["appError"], "mcp_settings_conflict");
    assert_eq!(
        client
            .rpc(
                "mcp.set_enabled",
                json!({"id":"docs","source":"project","expectedVersion":revision,"enabled":true})
            )
            .await["error"]["data"]["appError"],
        "invalid_params"
    );
    assert!(
        client
            .features
            .as_array()
            .unwrap()
            .contains(&json!("session.exec.stop"))
    );
    let mut remote = Client::new_version(&server, true, 3).await;
    assert_eq!(remote.features, json!([]));
    for (method, params) in [
        ("session.create", json!({"cwd":work})),
        ("session.list", json!({})),
        ("skills.catalog", json!({})),
        ("mcp.catalog", json!({})),
        ("session.mcp", json!({"sessionId":"remote"})),
        (
            "session.permission.get",
            json!({"sessionId":"remote","branchId":"main"}),
        ),
    ] {
        assert_eq!(
            remote.rpc(method, params).await["error"]["data"]["appError"],
            "local_transport_required"
        );
    }
    let entry = |id: &str, max: u64| {
        let caps = dim_agent_types::ModelCapabilities {
            context_window: 32000,
            max_output_tokens: max,
            ..Default::default()
        };
        json!({"modelId":id,"displayName":id,"status":"active","capabilities":caps})
    };
    assert_ok(&client.rpc("provider.save",json!({"provider":{"providerId":"public-fixture","displayName":"Fixture","driverKind":"openai-compatible","baseUrl":"https://model.example/v1","auth":{"type":"bearer"},"credential":{"action":"set","value":"fixture-access"},"models":[entry("alpha",2048),entry("beta",4096)],"enabled":true}})).await);
    // Select the model as a user would; the no-selection fallback costs
    // seconds in a debug build and is covered by provider_flow.
    assert_ok(
        &client
            .rpc(
                "provider.select",
                json!({"providerId":"public-fixture","modelId":"alpha"}),
            )
            .await,
    );
    let created = client.rpc("session.create", json!({"cwd":work})).await;
    assert_ok(&created);
    assert_eq!(created["result"]["model"]["modelId"], "alpha");
    let sid = created["result"]["sessionId"].as_str().unwrap().to_owned();
    let session = server.agent().unwrap().get_session(&sid).unwrap();
    let branch = json!({"sessionId":sid,"branchId":"main"});
    assert_ok(&client.rpc("session.attach", json!({"sessionId":sid})).await);
    async fn mcp_disabled(client: &mut Client) {
        loop {
            let notice = client.notice("session.mcp.state").await;
            if notice["params"]["servers"][0]["state"] == "disabled" {
                assert!(notice["params"]["servers"][0]["adoptedVersion"].is_null());
                break;
            }
        }
    }
    mcp_disabled(&mut client).await;
    let state = client.rpc("session.mcp", json!({"sessionId":sid})).await;
    assert_ok(&state);
    assert_eq!(state["result"]["servers"][0]["state"], "disabled");
    assert_ok(
        &client
            .rpc("session.mcp.refresh", json!({"sessionId":sid}))
            .await,
    );
    assert_ok(
        &client
            .rpc(
                "mcp.disconnect",
                json!({"cwd":work,"id":"docs","source":"project"}),
            )
            .await,
    );
    // Reload discards owner handles; the same attachment must acquire the new
    // native state subscriptions and publish their initial snapshot.
    let old_mcp = session.session_service::<McpSession>().unwrap();
    let old_tools = session.session_service::<ToolSourcesView>().unwrap();
    client
        .notices
        .retain(|notice| notice["method"] != "session.mcp.state");
    server.restart("Mcp").await.unwrap();
    // Restart waits for process Plugins, not Session dependents. A disabled
    // notification still buffered in the socket cannot prove their readiness.
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let (Ok(mcp), Ok(tools)) = (
                session.session_service::<McpSession>(),
                session.session_service::<ToolSourcesView>(),
            ) && !Arc::ptr_eq(&mcp, &old_mcp)
                && !Arc::ptr_eq(&tools, &old_tools)
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("Session MCP owners replaced after process reload");
    mcp_disabled(&mut client).await;
    assert_ok(&client.rpc("session.mcp", json!({"sessionId":sid})).await);
    assert_ok(&client.rpc("mcp.set_enabled", json!({"cwd":work,"id":"docs","source":"project","expectedVersion":revision,"enabled":true})).await);
    let enabled = client.rpc("mcp.catalog", json!({"cwd":work})).await;
    assert_eq!(enabled["result"]["servers"][0]["enabled"], true);
    // Only the new owner can report this transition: the server was disabled
    // before reload, and its configured executable does not exist. An old
    // disabled snapshot or a successful RPC cannot prove the watch was rebound.
    loop {
        let notice = client.notice("session.mcp.state").await;
        if notice["params"]["servers"][0]["state"] == "failed" {
            assert_eq!(notice["params"]["servers"][0]["id"], "docs");
            assert_eq!(notice["params"]["closed"], false);
            break;
        }
    }
    // The choice is recorded for the user; the project file and its revision
    // do not change, so the same revision disables it again.
    assert_eq!(enabled["result"]["projectRevision"], revision);
    assert_ok(&client.rpc("mcp.set_enabled", json!({"cwd":work,"id":"docs","source":"project","expectedVersion":revision,"enabled":false})).await);
    let saved: Value =
        serde_json::from_slice(&std::fs::read(work.join(".mcp.json")).unwrap()).unwrap();
    assert_eq!(saved["extension"], "preserved");
    assert_eq!(saved["mcpServers"]["docs"]["enabled"], false);
    mcp_disabled(&mut client).await;
    // A damaged external file reports its error rather than an empty catalog;
    // optional MCP failures leave ordinary workspace tools available.
    std::fs::write(work.join(".mcp.json"), b"{broken-private-secret").unwrap();
    let broken = client.rpc("mcp.catalog", json!({"cwd":work})).await;
    assert!(broken.get("error").is_some());
    assert!(!broken.to_string().contains("private-secret"));
    assert_ok(
        &client
            .rpc("session.mcp.refresh", json!({"sessionId":sid}))
            .await,
    );
    // After import, even corrupt legacy user files cannot replace loader intent.
    std::fs::write(home.join("skills.json"), b"{broken").unwrap();
    assert_ok(&client.rpc("session.skills", branch.clone()).await);
    assert_ok(&client.rpc("skills.refresh", json!({"cwd":work})).await);
    let skills = client.rpc("skills.catalog", json!({"cwd":work})).await;
    assert_ok(&skills);
    let skill = skills["result"]["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["name"] == "product-check")
        .unwrap();
    if cfg!(target_os = "linux") {
        assert!(skill["filePath"].as_str().unwrap().contains('\u{fffd}'));
    }
    let skill_id = skill["id"].clone();
    assert_ok(
        &client
            .rpc(
                "skills.set_enabled",
                json!({"cwd":work,"id":skill_id,"enabled":false}),
            )
            .await,
    );
    skills_posted(&session, false).await;
    assert_ok(
        &client
            .rpc(
                "skills.set_enabled",
                json!({"cwd":work,"id":skill_id,"enabled":true}),
            )
            .await,
    );
    skills_posted(&session, true).await;
    assert_ok(&client.rpc("session.skills", branch.clone()).await);

    // The Session created with corrupted optional Skills remains usable after
    // refresh, and the file tools execute through the real WorkspaceTools.
    assert_ok(&client.rpc("session.submit", json!({"sessionId":sid,"branchId":"main","text":"tool:write {\"path\":\"result.txt\",\"content\":\"product output\\n\"}"})).await);
    assert_ok(&client.rpc("session.idle", branch.clone()).await);
    assert_eq!(
        std::fs::read_to_string(work.join("result.txt")).unwrap(),
        "product output\n"
    );
    assert_ok(&client.rpc("session.submit", json!({"sessionId":sid,"branchId":"main","text":"tool:exec {\"command\":\"printf started; sleep 60\",\"runInBackground\":true,\"timeoutMs\":-1}"})).await);
    let approval = client.notice("approval.request").await;
    assert_ok(&client.rpc("approval.respond", json!({"sessionId":sid,"branchId":"main","requestId":approval["params"]["requestId"],"decision":"allow"})).await);
    let task = loop {
        let event = client.notice("session.event").await;
        if event["params"]["event"]["type"] == "exec"
            && event["params"]["event"]["name"] == "started"
        {
            break event["params"]["event"]["data"]["task_id"].clone();
        }
    };
    let ended = client
        .rpc(
            "session.exec.stop",
            json!({"sessionId":sid,"branchId":"main","taskId":task}),
        )
        .await;
    assert_ok(&ended);
    assert_eq!(ended["result"]["status"], "cancelled");
    assert_ok(&client.rpc("session.idle", branch.clone()).await);
    let policy = client.rpc("session.permission.get", branch.clone()).await;
    assert_ok(&policy);
    let changed = client.rpc("session.permission.update", json!({"sessionId":sid,"branchId":"main","expectedVersion":policy["result"]["version"],"policy":{"preset":"read-only","allowedTools":[],"disallowedTools":[]}})).await;
    assert_ok(&changed);
    assert_eq!(client.rpc("session.permission.update", json!({"sessionId":sid,"branchId":"main","expectedVersion":policy["result"]["version"],"policy":policy["result"]["policy"]})).await["error"]["data"]["appError"], "permission_conflict");
    // AGENTS.md arrives as a rule input; the system never carries it.
    async fn project_rule(session: &dim_agent_runtime::Session) -> (String, String) {
        let context = session
            .context("main", session.sequence("main").unwrap())
            .await
            .unwrap();
        let system = serde_json::to_string(
            &dim_agent_capabilities::history::context_configuration(&context)
                .unwrap()
                .system,
        )
        .unwrap();
        let rule = context
            .events
            .iter()
            .rev()
            .find_map(|event| match &event.kind {
                EventKind::InputMessage(data) => data.blocks.iter().find_map(|block| {
                    let dim_agent_types::InputBlock::Raw { text, .. } = block;
                    text.starts_with("<dim-rule source=\"dim/instructions/project\"")
                        .then(|| text.clone())
                }),
                _ => None,
            })
            .unwrap();
        (system, rule)
    }
    let (system, rule) = project_rule(&session).await;
    assert!(rule.contains("Product prompt before refresh."));
    assert!(!system.contains("Product prompt before refresh."));
    std::fs::write(work.join("AGENTS.md"), "Product prompt after refresh.\n").unwrap();
    assert_ok(&client.rpc("session.prompt.refresh", branch.clone()).await);
    let (system, rule) = project_rule(&session).await;
    assert!(rule.contains("op=\"replace\"") && rule.contains("Product prompt after refresh."));
    assert!(!system.contains("Product prompt"));
    assert_ok(
        &client
            .rpc(
                "session.model.update",
                json!({"sessionId":sid,"branchId":"main","providerId":"public-fixture","modelId":"beta"}),
            )
            .await,
    );
    let context = session
        .context("main", session.sequence("main").unwrap())
        .await
        .unwrap();
    assert_eq!(
        dim_agent_capabilities::history::context_configuration(&context)
            .unwrap()
            .settings
            .model_id,
        "beta"
    );

    // A locked database delays owner publication. Rpc reload cancels its
    // response wait while the admitted Permission change remains owned.
    let lock = rusqlite::Connection::open(native.join("dimcode.sqlite")).unwrap();
    lock.execute_batch("BEGIN IMMEDIATE").unwrap();
    let pending = client.send("session.permission.update", json!({"sessionId":sid,"branchId":"main","expectedVersion":changed["result"]["version"],"policy":{"preset":"workspace-write","allowedTools":[],"disallowedTools":[]}})).await;
    let publication = session.publication("main").unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while !publication.has_pending() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(2), server.restart("Rpc"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        client.response(pending).await["error"]["data"]["appError"],
        "operation_interrupted"
    );
    lock.execute_batch("COMMIT").unwrap();
    let mut dispatched = session.dispatch_watch("main").unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if session
                .get::<dim_agent_capabilities::permission::PermissionState>("main")
                .unwrap()
                .unwrap()
                .policy
                .preset
                == PermissionPreset::WorkspaceWrite
            {
                break;
            }
            dispatched.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    assert_eq!(
        client.rpc("session.permission.get", branch).await["result"]["policy"]["preset"],
        "workspace-write"
    );
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// Each Session's ModelProvider progress cell. The writer lands separately;
/// the test sets the cell as ModelProvider would.
struct ProgressCapture {
    scope: dim_agent_runtime::BranchScope,
    cells: ProgressCells,
}
type ProgressCells = Arc<
    std::sync::Mutex<
        std::collections::HashMap<
            String,
            Arc<dim_agent_runtime::StateCell<dim_agent_runtime::ModelProgress>>,
        >,
    >,
>;
impl TypedPlugin for ProgressCapture {
    type Deps = (
        InBranch<dim_agent_runtime::StateCell<dim_agent_runtime::ModelProgress>>,
        InSession<dim_agent_runtime::SessionIdentity>,
    );
    fn name(&self) -> &str {
        "product-test.progress-capture"
    }
    fn apply<'a>(
        &'a self,
        _: &'a rutis::Ctx,
        (InBranch(cell), InSession(identity)): Self::Deps,
    ) -> rutis::BoxFuture<'a, Result<rutis::Effect, rutis::CordisError>> {
        Box::pin(async move {
            let session = identity.session_id.clone();
            self.cells.lock().unwrap().insert(session, cell);
            Ok(rutis::Effect::Done)
        })
    }
}
impl Capability for ProgressCapture {
    fn keys(&self) -> KeysOf<Self> {
        scoped::<Self::Deps, _>(&self.scope)
    }
}

/// `session.model.progress` carries each branch's current snapshot to v3
/// subscribers: on attach, on change (coalesced) and again on re-attach.
/// It is not a Timeline event and v2 subscribers never receive it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn model_progress_reaches_v3_subscribers_without_persisting() {
    use dim_agent_runtime::{ModelProgress, ToolProgress};
    let cells = ProgressCells::default();
    let captured = cells.clone();
    let (server, agent) = launch_with(Arc::new(MockTools), ServerConfig::default(), |builder| {
        builder.timeline_plugin(move |scope| ProgressCapture {
            scope: *scope,
            cells: captured.clone(),
        });
    })
    .await;
    let mut client = Client::new_version(&server, false, 3).await;
    assert!(
        client
            .features
            .as_array()
            .unwrap()
            .contains(&json!("session.model.progress"))
    );
    let created = client.rpc("session.create", create(json!([]))).await;
    assert_ok(&created);
    let sid = created["result"]["sessionId"].as_str().unwrap().to_owned();
    let session = agent.get_session(&sid).unwrap();
    let head = session.sequence("main").unwrap();
    assert_ok(&client.rpc("session.attach", json!({"sessionId":sid})).await);
    let empty = json!({"sessionId":sid,"branchId":"main","turnId":null,"requestId":null,
        "text":"","textOffset":0,"thinking":"","thinkingOffset":0,"tools":[]});
    assert_eq!(
        client.notice("session.model.progress").await["params"],
        empty
    );

    let cell = cells.lock().unwrap().get(&sid).unwrap().clone();
    let progress = |text: &str| ModelProgress {
        turn_id: Some("turn-1".into()),
        request_id: Some("request-1".into()),
        text: text.into(),
        text_offset: 3,
        thinking: "weighing".into(),
        thinking_offset: 5,
        tools: vec![ToolProgress {
            call_id: "call-1".into(),
            name: "echo".into(),
            arguments: "{\"text\":\"he".into(),
        }],
    };
    // A burst of changes reaches the client coalesced, ending at the latest.
    let updates = 200;
    for count in 1..=updates {
        cell.set(progress(&"x".repeat(count)));
    }
    let mut notices = 0;
    let last = loop {
        let notice = client.notice("session.model.progress").await;
        notices += 1;
        if notice["params"]["text"] == "x".repeat(updates) {
            break notice;
        }
    };
    assert!(notices < updates, "{notices} notices for {updates} changes");
    assert_eq!(
        last["params"],
        json!({"sessionId":sid,"branchId":"main","turnId":"turn-1","requestId":"request-1",
            "text":"x".repeat(updates),"textOffset":3,"thinking":"weighing","thinkingOffset":5,
            "tools":[{"callId":"call-1","name":"echo","arguments":"{\"text\":\"he"}]})
    );

    // A new subscription receives the current snapshot.
    let mut other = Client::new_version(&server, true, 3).await;
    assert_ok(&other.rpc("session.attach", json!({"sessionId":sid})).await);
    assert_eq!(
        other.notice("session.model.progress").await["params"],
        last["params"]
    );
    cell.set(ModelProgress::default());
    assert_eq!(
        client.notice("session.model.progress").await["params"],
        empty
    );
    assert_eq!(
        other.notice("session.model.progress").await["params"],
        empty
    );
    // Display state only: nothing reached the Timeline.
    assert_eq!(session.sequence("main").unwrap(), head);

    // v2 subscribers observe Sessions of their own version; no progress.
    let mut legacy = Client::new_version(&server, false, 2).await;
    let created = legacy.rpc("session.create", create(json!([]))).await;
    assert_ok(&created);
    let legacy_sid = created["result"]["sessionId"].as_str().unwrap().to_owned();
    assert_ok(
        &legacy
            .rpc("session.attach", json!({"sessionId":legacy_sid}))
            .await,
    );
    let legacy_cell = cells.lock().unwrap().get(&legacy_sid).unwrap().clone();
    legacy_cell.set(progress("unseen"));
    tokio::time::sleep(Duration::from_millis(200)).await;
    legacy.rpc("daemon.ping", json!({})).await;
    assert!(
        legacy
            .notices
            .iter()
            .all(|notice| notice["method"] != "session.model.progress"),
        "{:?}",
        legacy.notices
    );
    server.shutdown();
    server.join().await.unwrap();
}

/// Provides the Session `ModelHost` from a child plugin; restarting it
/// reloads ModelProvider.
struct ModelHostOwner {
    scope: dim_agent_runtime::SessionScope,
    views: Arc<std::sync::Mutex<Vec<rutis::FiberView>>>,
}
impl TypedPlugin for ModelHostOwner {
    type Deps = ();
    fn name(&self) -> &str {
        "ModelHostOwner"
    }
    fn apply<'a>(
        &'a self,
        ctx: &'a rutis::Ctx,
        (): Self::Deps,
    ) -> rutis::BoxFuture<'a, Result<rutis::Effect, rutis::CordisError>> {
        Box::pin(async move {
            let view = ctx.plugin(capability_plugin(ModelHostPlugin(self.scope)));
            self.views.lock().unwrap().push(view);
            Ok(rutis::Effect::Done)
        })
    }
}
impl Capability for ModelHostOwner {
    fn keys(&self) -> KeysOf<Self> {}
}
struct ModelHostPlugin(dim_agent_runtime::SessionScope);
impl TypedPlugin for ModelHostPlugin {
    type Deps = ();
    fn name(&self) -> &str {
        "ModelHost"
    }
    fn apply<'a>(
        &'a self,
        ctx: &'a rutis::Ctx,
        (): Self::Deps,
    ) -> rutis::BoxFuture<'a, Result<rutis::Effect, rutis::CordisError>> {
        Box::pin(async move {
            ctx.provide_as(
                self.0.key::<dim_agent_capabilities::host::ModelHost>(),
                Arc::new(dim_agent_capabilities::host::ModelHost(Arc::new(
                    dim_agent_capabilities::adapters::EchoModel,
                ))),
            )?;
            Ok(rutis::Effect::Done)
        })
    }
}
impl Capability for ModelHostPlugin {
    fn keys(&self) -> KeysOf<Self> {}
}

/// A ModelProvider reload replaces its progress cell; the subscription
/// follows the new cell without re-attaching.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn model_progress_follows_a_reloaded_provider() {
    use dim_agent_capabilities::{
        adapters::DefaultPrepareModelPlugin,
        history::HistoryRenderer,
        host::{CompactionStrategyHost, ContextModelHost, IdentityContext, PrepareContextHost},
    };
    let cells = ProgressCells::default();
    let captured = cells.clone();
    let views = Arc::new(std::sync::Mutex::new(vec![]));
    let mut builder = AgentBuilder::new(AgentConfig::default());
    defaults::install(&mut builder);
    let owners = views.clone();
    builder
        .session_value(Arc::new(HistoryRenderer::default()))
        .session_value(Arc::new(PrepareContextHost(Arc::new(IdentityContext))))
        .session_value(Arc::new(ContextModelHost(None)))
        .session_value(Arc::new(CompactionStrategyHost(None)))
        .session_plugin(DefaultPrepareModelPlugin::new)
        .session_plugin(move |scope| ModelHostOwner {
            scope: *scope,
            views: owners.clone(),
        });
    dim_agent_capabilities::defaults::tool_host(&mut builder, Arc::new(MockTools));
    builder.timeline_plugin(move |scope| ProgressCapture {
        scope: *scope,
        cells: captured.clone(),
    });
    let server = serve(
        builder,
        ServerConfig {
            unix_path: Some(
                std::path::PathBuf::from("/tmp")
                    .join(format!("dim-progress-reload-{}/s", uuid::Uuid::new_v4())),
            ),
            websocket_address: None,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let mut client = Client::new_version(&server, false, 3).await;
    let created = client.rpc("session.create", create(json!([]))).await;
    assert_ok(&created);
    let sid = created["result"]["sessionId"].as_str().unwrap().to_owned();
    assert_ok(&client.rpc("session.attach", json!({"sessionId":sid})).await);
    client.notice("session.model.progress").await;
    let before = cells.lock().unwrap().get(&sid).unwrap().clone();
    let view = views.lock().unwrap().last().cloned().unwrap();
    view.restart().await.unwrap();
    let after = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let cell = cells.lock().unwrap().get(&sid).unwrap().clone();
            if !Arc::ptr_eq(&cell, &before) {
                return cell;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("ModelProvider reloaded");
    after.set(dim_agent_runtime::ModelProgress {
        text: "after reload".into(),
        ..Default::default()
    });
    let notice = client.notice("session.model.progress").await;
    assert_eq!(notice["params"]["text"], "after reload");
    server.shutdown();
    server.join().await.unwrap();
}

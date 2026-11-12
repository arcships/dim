use super::web_entry::{frontend_fixture, http, post, web_client};
use super::*;
use dim_app_server::ui_extensions::{PublishFile, UiResources};

fn manifest() -> Value {
    json!({"name":"preview","ui":{"version":1,"root":"ui","panels":[{"id":"viewer","title":{"default":"Preview"},"icon":"icon.svg","entry":"panel.js","placement":"session-panel","requires":["session.state","session.history","session.submit","session.interrupt","resources"],"methods":[]}]}})
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn content_and_resources_follow_plugin_session_and_publisher_lifetimes() {
    let home = super::plugin_manager::home();
    let mut config = super::plugin_manager::config(&home);
    config.product_home = None;
    config.target_home = Some(home.clone());
    config.file_roots = vec![home.canonicalize().unwrap()];
    let assets = frontend_fixture();
    config.web_assets = Some(assets.clone());
    let source = home.join("preview");
    std::fs::create_dir_all(source.join("ui")).unwrap();
    std::fs::write(source.join("dim-plugin.json"), manifest().to_string()).unwrap();
    std::fs::write(source.join("ui/panel.js"), "export const revision = 1").unwrap();
    std::fs::write(source.join("ui/lazy.js"), "export const lazy = 1").unwrap();
    std::fs::write(source.join("ui/panel.js.map"), "private source map").unwrap();
    let stale = home.join("ui-cache").join(uuid::Uuid::new_v4().to_string());
    std::fs::create_dir_all(&stale).unwrap();
    std::fs::write(stale.join("abandoned.js"), "old generation").unwrap();
    let v1 = br#"{"version":1,"installed":[],"names":{"unused":{"off":true}}}"#;
    std::fs::write(home.join("plugins.json"), v1).unwrap();
    let (server, agent) = launch_with_agent(Arc::new(MockTools), config).await;
    assert!(
        !stale.exists(),
        "abandoned cache generations must be removed at startup"
    );
    let mut local = Client::new_version(&server, false, 4).await;
    let opened = local.rpc("web.open", json!({"port":0})).await["result"].clone();
    let address = opened["address"].as_str().unwrap();
    let bootstrap = opened["url"]
        .as_str()
        .unwrap()
        .split_once("#bootstrap=")
        .unwrap()
        .1;
    let (status, headers, _) = http(
        address,
        post(
            address,
            "/api/auth/exchange",
            &format!("http://{address}"),
            &json!({"token":bootstrap}).to_string(),
        ),
    )
    .await;
    assert_eq!(status, 204);
    let cookie = headers
        .lines()
        .find_map(|line| line.strip_prefix("set-cookie: "))
        .unwrap()
        .split(';')
        .next()
        .unwrap();
    let request = |method: &str, path: &str, extra: &str| {
        format!(
            "{method} {path} HTTP/1.1\r\nHost: {address}\r\nCookie: {cookie}\r\n{extra}Connection: close\r\n\r\n"
        )
    };
    let mut browser = web_client(address, cookie).await;
    for method in [
        "ui.extensions.list",
        "session.history",
        "session.submit",
        "session.stop",
    ] {
        assert!(
            browser
                .features
                .as_array()
                .unwrap()
                .contains(&json!(method)),
            "missing panel method {method}: {}",
            browser.features
        );
    }
    let installed = local
        .rpc("plugin.install", json!({"source":{"path":source}}))
        .await;
    assert!(installed.get("error").is_none(), "{installed}");
    assert_eq!(
        std::fs::read(home.join("plugins.json.v1.backup")).unwrap(),
        v1
    );
    let record: Value =
        serde_json::from_slice(&std::fs::read(home.join("plugins.json")).unwrap()).unwrap();
    assert_eq!(record["version"], 2);
    assert_eq!(record["names"]["unused"]["off"], true);
    assert_eq!(record["installed"][0]["kinds"], json!(["ui"]));
    let changed = browser.notice("ui.extensions.changed").await;
    let directory = browser.rpc("ui.extensions.list", json!({})).await["result"].clone();
    assert_eq!(changed["params"]["revision"], directory["revision"]);
    assert_eq!(
        directory["extensions"][0]["pluginName"], "preview",
        "{directory}; {installed}"
    );
    assert!(!directory.to_string().contains(home.to_str().unwrap()));
    assert_eq!(directory["errors"], json!([]));
    assert!(
        directory["extensions"][0]["panels"][0]
            .get("icon")
            .is_none(),
        "a missing icon must keep the panel available: {directory}"
    );
    let plain = home.join("plain");
    std::fs::create_dir(&plain).unwrap();
    std::fs::write(plain.join("dim-plugin.json"), r#"{"name":"plain"}"#).unwrap();
    let installed_plain = local
        .rpc("plugin.install", json!({"source":{"path":plain}}))
        .await;
    assert!(installed_plain.get("error").is_none(), "{installed_plain}");
    local.rpc("plugin.reload", json!({"name":"plain"})).await;
    assert_eq!(
        browser.rpc("ui.extensions.list", json!({})).await["result"]["revision"],
        directory["revision"],
        "a package without UI must not invalidate the directory"
    );
    let generation = directory["extensions"][0]["generation"].as_str().unwrap();
    let path = format!("/api/ui/extensions/{generation}/panel.js");
    let (status, headers, body) = http(address, request("GET", &path, "")).await;
    assert_eq!((status, body.as_str()), (200, "export const revision = 1"));
    assert!(headers.contains("text/javascript"));
    assert!(headers.contains("private, no-cache"));
    assert!(headers.contains("nosniff"));
    assert_eq!(
        http(
            address,
            request(
                "GET",
                &path,
                &format!("If-None-Match: \"{generation}\"\r\n")
            )
        )
        .await
        .0,
        304
    );
    assert_eq!(
        http(
            address,
            request("GET", &path, "").replace(&format!("Cookie: {cookie}\r\n"), "")
        )
        .await
        .0,
        401
    );
    assert_eq!(
        http(
            address,
            request("GET", &path, "Origin: http://evil.test\r\n")
        )
        .await
        .0,
        403
    );
    assert_eq!(
        http(
            address,
            request("GET", &path, "Sec-Fetch-Site: cross-site\r\n")
        )
        .await
        .0,
        403
    );
    for suffix in [
        "missing.js",
        "panel.js.map",
        "%2e%2e/dim-plugin.json",
        "%2fetc/passwd",
        "ui%5c..%5csecret",
    ] {
        assert_eq!(
            http(
                address,
                request(
                    "GET",
                    &format!("/api/ui/extensions/{generation}/{suffix}"),
                    ""
                )
            )
            .await
            .0,
            404
        );
    }
    std::fs::write(source.join("ui/lazy.js"), "export const lazy = 2").unwrap();
    assert_eq!(
        http(
            address,
            request(
                "GET",
                &format!("/api/ui/extensions/{generation}/lazy.js"),
                ""
            )
        )
        .await
        .2,
        "export const lazy = 1"
    );
    let icon = source.join("ui/icon.svg");
    std::fs::write(&icon, "<svg xmlns=\"http://www.w3.org/2000/svg\"/>").unwrap();
    let reload = local.rpc("plugin.reload", json!({"name":"preview"})).await;
    assert!(reload.get("error").is_none(), "{reload}");
    assert_ne!(reload["result"]["status"], "failed", "{reload}");
    assert_eq!(http(address, request("GET", &path, "")).await.0, 404);
    let next = browser.rpc("ui.extensions.list", json!({})).await["result"].clone();
    assert_ne!(next["extensions"][0]["generation"], generation);
    assert!(next["revision"].as_u64().unwrap() > directory["revision"].as_u64().unwrap());
    assert_eq!(next["extensions"][0]["panels"][0]["icon"], "icon.svg");
    let icon_path = format!(
        "/api/ui/extensions/{}/icon.svg",
        next["extensions"][0]["generation"].as_str().unwrap()
    );
    assert_eq!(http(address, request("GET", &icon_path, "")).await.0, 200);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let permissions = std::fs::metadata(&icon).unwrap().permissions();
        std::fs::set_permissions(&icon, std::fs::Permissions::from_mode(0o000)).unwrap();
        local.rpc("plugin.reload", json!({"name":"preview"})).await;
        std::fs::set_permissions(&icon, permissions).unwrap();
        let unreadable = browser.rpc("ui.extensions.list", json!({})).await["result"].clone();
        assert_eq!(unreadable["errors"], json!([]), "{unreadable}");
        assert_eq!(unreadable["extensions"][0]["pluginName"], "preview");
        assert!(
            unreadable["extensions"][0]["panels"][0]
                .get("icon")
                .is_none()
        );
    }
    let mut renamed = manifest();
    renamed["name"] = json!("renamed");
    std::fs::write(source.join("dim-plugin.json"), renamed.to_string()).unwrap();
    let rejected = local.rpc("plugin.reload", json!({"name":"preview"})).await;
    assert!(rejected.get("error").is_some());
    assert_eq!(
        browser.rpc("ui.extensions.list", json!({})).await["result"]["extensions"],
        json!([])
    );
    std::fs::write(source.join("dim-plugin.json"), manifest().to_string()).unwrap();
    local.rpc("plugin.reload", json!({"name":"preview"})).await;

    let disabled = local
        .rpc("plugin.disable", json!({"name":"UiExtensions"}))
        .await;
    assert!(disabled.get("error").is_none(), "{disabled}");
    assert!(
        browser
            .rpc("ui.extensions.list", json!({}))
            .await
            .get("error")
            .is_some()
    );
    let enabled = local
        .rpc("plugin.enable", json!({"name":"UiExtensions"}))
        .await;
    assert!(enabled.get("error").is_none(), "{enabled}");
    let restored = browser.rpc("ui.extensions.list", json!({})).await;
    assert_eq!(
        restored["result"]["extensions"][0]["pluginName"], "preview",
        "{restored}"
    );
    assert!(restored["result"]["revision"].as_u64().unwrap() > next["revision"].as_u64().unwrap());
    assert_ne!(
        restored["result"]["extensions"][0]["generation"],
        next["extensions"][0]["generation"]
    );

    let created = local.rpc("session.create", json!({"cwd":home})).await;
    let id = created["result"]["sessionId"]
        .as_str()
        .unwrap_or_else(|| panic!("{created}"));
    let session = agent.get_session(id).unwrap();
    let resources = session.session_service::<UiResources>().unwrap();
    let publisher = rutis::Ctx::root().unwrap();
    let stranger = rutis::Ctx::root().unwrap();
    let relative = resources
        .publish_file(
            &publisher,
            PublishFile {
                path: "media.txt".into(),
                media_type: "text/plain".into(),
            },
        )
        .await
        .unwrap_err();
    assert_eq!(relative.code, "invalid_params");
    let file = home.canonicalize().unwrap().join("media.txt");
    std::fs::write(&file, "0123456789").unwrap();
    let reference = resources
        .publish_file(
            &publisher,
            PublishFile {
                path: file.to_string_lossy().into_owned(),
                media_type: "text/plain".into(),
            },
        )
        .await
        .unwrap();
    let media = format!("/api/ui/sessions/{id}/resources/{}", reference.id);
    for (range, status, expected) in [
        ("bytes=2-5", 206, "2345"),
        ("bytes=-3", 206, "789"),
        ("bytes=7-", 206, "789"),
        ("bytes=20-", 416, ""),
        ("bytes=0-0,2-2", 200, "0123456789"),
        ("items=0-2", 200, "0123456789"),
    ] {
        let (got, headers, body) = http(
            address,
            request("GET", &media, &format!("Range: {range}\r\n")),
        )
        .await;
        assert_eq!(
            (got, body.as_str()),
            (status, expected),
            "{range}: {headers}"
        );
        assert!(headers.contains("private, no-store"));
        if status == 416 {
            assert!(headers.contains("bytes */10"));
        }
    }
    let (status, headers, body) =
        http(address, request("HEAD", &media, "Range: bytes=2-5\r\n")).await;
    assert_eq!(status, 200);
    assert!(headers.contains("content-length: 10"));
    assert!(body.is_empty());
    assert!(
        resources
            .withdraw(&stranger, reference.clone())
            .await
            .is_err()
    );
    let forbidden = resources
        .publish_file(
            &publisher,
            PublishFile {
                path: "/etc/passwd".into(),
                media_type: "text/plain".into(),
            },
        )
        .await;
    assert!(forbidden.is_err());
    let active = resources
        .publish_file(
            &publisher,
            PublishFile {
                path: file.to_string_lossy().into_owned(),
                media_type: "image/svg+xml".into(),
            },
        )
        .await
        .unwrap();
    let active_path = format!("/api/ui/sessions/{id}/resources/{}", active.id);
    let (_, headers, _) = http(address, request("GET", &active_path, "")).await;
    assert!(headers.contains("application/octet-stream"));
    assert!(headers.contains("content-disposition: attachment"));
    resources.withdraw(&publisher, reference).await.unwrap();
    assert_eq!(http(address, request("GET", &media, "")).await.0, 404);
    publisher.shutdown().await.unwrap();
    assert_eq!(http(address, request("GET", &active_path, "")).await.0, 404);

    let large = home.canonicalize().unwrap().join("large.bin");
    std::fs::File::create(&large)
        .unwrap()
        .set_len(64 * 1024 * 1024)
        .unwrap();
    for close_session in [false, true] {
        let published = resources
            .publish_file(
                &stranger,
                PublishFile {
                    path: large.to_string_lossy().into_owned(),
                    media_type: "video/mp4".into(),
                },
            )
            .await
            .unwrap();
        let path = format!("/api/ui/sessions/{id}/resources/{}", published.id);
        let mut socket = TcpStream::connect(address).await.unwrap();
        socket
            .write_all(request("GET", &path, "").as_bytes())
            .await
            .unwrap();
        let mut reader = BufReader::new(socket);
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        assert!(line.contains("200 OK"));
        loop {
            line.clear();
            reader.read_line(&mut line).await.unwrap();
            if line == "\r\n" {
                break;
            }
        }
        tokio::time::timeout(Duration::from_secs(5), async {
            if close_session {
                let closed = local.rpc("session.close", json!({"sessionId":id})).await;
                assert!(closed.get("error").is_none(), "{closed}");
            } else {
                resources.withdraw(&stranger, published).await.unwrap();
            }
        })
        .await
        .expect("withdrawal and closing must finish while the HTTP consumer stops reading");
        drop(reader);
        assert_ne!(http(address, request("GET", &path, "")).await.0, 200);
    }
    stranger.shutdown().await.unwrap();

    let mut unsupported = manifest();
    unsupported["ui"]["version"] = json!(9);
    let mut escaping_icon = manifest();
    escaping_icon["ui"]["panels"][0]["icon"] = json!("../secret.svg");
    let mut missing_entry = manifest();
    missing_entry["ui"]["panels"][0]["entry"] = json!("missing.js");
    for invalid in [unsupported, escaping_icon, missing_entry] {
        std::fs::write(source.join("dim-plugin.json"), invalid.to_string()).unwrap();
        local.rpc("plugin.reload", json!({"name":"preview"})).await;
        let unavailable = browser.rpc("ui.extensions.list", json!({})).await;
        assert_eq!(unavailable["result"]["extensions"], json!([]));
        assert_eq!(unavailable["result"]["errors"][0]["pluginName"], "preview");
    }
    local.rpc("plugin.disable", json!({"name":"preview"})).await;
    assert_eq!(
        browser.rpc("ui.extensions.list", json!({})).await["result"]["errors"],
        json!([])
    );
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
    std::fs::remove_dir_all(assets).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires DIM_TEST_NODE and DIM_TEST_JS_RUNTIME"]
async fn node_typescript_publishes_files_without_binary_bridge_callbacks() {
    let home = super::plugin_manager::home();
    let mut config = super::plugin_manager::config(&home);
    let home = home.canonicalize().unwrap();
    config.product_home = None;
    config.target_home = Some(home.clone());
    config.file_roots = vec![home.clone()];
    let path = home.join("result.txt");
    std::fs::write(&path, "finite result").unwrap();
    let mut parts = super::session_typescript::fixture();
    parts.rows = vec![
        json!({"id":"ui-resource-fixture","name":parts.anchor.parent().unwrap().join("fixtures/ui-resources.ts"),"provides":{},"config":{"path":path}}),
    ];
    config.session_typescript = Some(parts);
    let server = launch(Arc::new(MockTools), config).await;
    let mut client = Client::new_version(&server, false, 4).await;
    let created = client.rpc("session.create", json!({"cwd":home})).await;
    let id = created["result"]["sessionId"]
        .as_str()
        .unwrap_or_else(|| panic!("{created}"));
    let published = super::session_typescript::when_served(
        &mut client,
        "session.preview.publish",
        json!({"sessionId":id}),
    )
    .await;
    assert_eq!(published["result"]["sessionId"], id, "{published}");
    assert!(published["result"]["id"].as_str().is_some());
    assert_eq!(published["result"].as_object().unwrap().len(), 2);
    let withdrawn = client
        .rpc(
            "session.preview.withdraw",
            json!({"sessionId":id,"reference":published["result"]}),
        )
        .await;
    assert_eq!(withdrawn["result"], json!({}), "{withdrawn}");
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_record_migration_and_unknown_versions_preserve_original_bytes() {
    let home = super::plugin_manager::home();
    let config = super::plugin_manager::config(&home);
    let record = br#"{"version":1,"installed":[],"names":{}}"#;
    std::fs::write(home.join("plugins.json"), record).unwrap();
    std::fs::write(home.join("plugins.json.v1.backup"), "different record").unwrap();
    let source = home.join("preview");
    std::fs::create_dir_all(source.join("ui")).unwrap();
    std::fs::write(source.join("dim-plugin.json"), manifest().to_string()).unwrap();
    std::fs::write(source.join("ui/panel.js"), "export {}").unwrap();
    let server = launch(Arc::new(MockTools), config.clone()).await;
    let mut client = Client::new_version(&server, false, 4).await;
    let refused = client
        .rpc("plugin.install", json!({"source":{"path":source}}))
        .await;
    assert!(refused.get("error").is_some(), "{refused}");
    assert_eq!(std::fs::read(home.join("plugins.json")).unwrap(), record);
    server.shutdown();
    server.join().await.unwrap();
    let unknown = br#"{"version":900,"installed":[],"names":{}}"#;
    std::fs::write(home.join("plugins.json"), unknown).unwrap();
    assert!(
        try_launch_with(Arc::new(MockTools), config, |_| {})
            .await
            .is_err()
    );
    assert_eq!(std::fs::read(home.join("plugins.json")).unwrap(), unknown);
    std::fs::remove_dir_all(home).unwrap();
}

fn decode(reply: &Value) -> Vec<u8> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(reply["data"].as_str().unwrap())
        .unwrap()
}

/// Waits until the Session's method table lists `method`, then returns the
/// listing it matched.
async fn wait_listed(client: &mut Client, session: &str, method: &str) -> Value {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let listed = client
            .rpc("session.methods", json!({"sessionId":session}))
            .await;
        let methods = listed["result"]["methods"].clone();
        if methods
            .as_array()
            .is_some_and(|methods| methods.iter().any(|entry| entry["name"] == method))
        {
            return methods;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "{method} is listed: {listed}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_local_client_reads_snapshot_resources_by_generation() {
    let home = super::plugin_manager::home();
    let mut config = super::plugin_manager::config(&home);
    config.product_home = None;
    config.target_home = Some(home.clone());
    config.file_roots = vec![home.canonicalize().unwrap()];
    let assets = frontend_fixture();
    config.web_assets = Some(assets.clone());
    let source = home.join("preview");
    std::fs::create_dir_all(source.join("ui")).unwrap();
    std::fs::write(source.join("dim-plugin.json"), manifest().to_string()).unwrap();
    let panel = b"export const revision = 1";
    std::fs::write(source.join("ui/panel.js"), panel).unwrap();
    std::fs::write(source.join("ui/panel.js.map"), "private source map").unwrap();
    std::fs::write(
        home.join("plugins.json"),
        r#"{"version":1,"installed":[],"names":{"unused":{"off":true}}}"#,
    )
    .unwrap();
    let (server, _agent) = launch_with_agent(Arc::new(MockTools), config).await;
    let mut local = Client::new_version(&server, false, 4).await;
    assert!(
        local
            .features
            .as_array()
            .unwrap()
            .contains(&json!("ui.extensions.read")),
        "a local client must discover the read method"
    );
    let opened = local.rpc("web.open", json!({"port":0})).await["result"].clone();
    let address = opened["address"].as_str().unwrap().to_owned();
    let bootstrap = opened["url"]
        .as_str()
        .unwrap()
        .split_once("#bootstrap=")
        .unwrap()
        .1;
    let (status, headers, _) = http(
        &address,
        post(
            &address,
            "/api/auth/exchange",
            &format!("http://{address}"),
            &json!({"token":bootstrap}).to_string(),
        ),
    )
    .await;
    assert_eq!(status, 204);
    let cookie = headers
        .lines()
        .find_map(|line| line.strip_prefix("set-cookie: "))
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let mut browser = web_client(&address, &cookie).await;
    let installed = local
        .rpc("plugin.install", json!({"source":{"path":source}}))
        .await;
    assert!(installed.get("error").is_none(), "{installed}");
    let directory = local.rpc("ui.extensions.list", json!({})).await["result"].clone();
    let generation = directory["extensions"][0]["generation"]
        .as_str()
        .unwrap()
        .to_owned();

    let first = local
        .rpc(
            "ui.extensions.read",
            json!({"generation":generation,"path":"panel.js","limit":13}),
        )
        .await;
    assert_ok(&first);
    assert_eq!(first["result"]["offset"], 0);
    assert_eq!(first["result"]["bytes"], 13);
    assert_eq!(first["result"]["total"], panel.len());
    assert_eq!(first["result"]["mediaType"], "text/javascript");
    assert_eq!(decode(&first["result"]), &panel[..13]);
    let second = local
        .rpc(
            "ui.extensions.read",
            json!({"generation":generation,"path":"panel.js","offset":13,"limit":13}),
        )
        .await;
    assert_ok(&second);
    assert_eq!(second["result"]["offset"], 13);
    assert_eq!(second["result"]["bytes"], panel.len() - 13);
    assert_eq!(decode(&second["result"]), &panel[13..]);
    assert_eq!(
        second["result"]["offset"].as_u64().unwrap() + second["result"]["bytes"].as_u64().unwrap(),
        second["result"]["total"].as_u64().unwrap(),
        "the last page ends the resource, so no empty read is needed"
    );
    let end = local
        .rpc(
            "ui.extensions.read",
            json!({"generation":generation,"path":"panel.js","offset":panel.len()}),
        )
        .await;
    assert_ok(&end);
    assert_eq!(end["result"]["bytes"], 0);
    assert_eq!(end["result"]["offset"], panel.len());
    assert_eq!(decode(&end["result"]), Vec::<u8>::new());
    let beyond = local
        .rpc(
            "ui.extensions.read",
            json!({"generation":generation,"path":"panel.js","offset":panel.len() + 1}),
        )
        .await;
    assert_eq!(
        beyond["error"]["data"]["appError"], "invalid_params",
        "{beyond}"
    );
    for path in ["missing.js", "panel.js.map"] {
        let reply = local
            .rpc(
                "ui.extensions.read",
                json!({"generation":generation,"path":path}),
            )
            .await;
        assert_eq!(
            reply["error"]["data"]["appError"], "resource_gone",
            "{path}: {reply}"
        );
    }
    for path in ["../x", "/x", ""] {
        let reply = local
            .rpc(
                "ui.extensions.read",
                json!({"generation":generation,"path":path}),
            )
            .await;
        assert_eq!(
            reply["error"]["data"]["appError"], "invalid_params",
            "{path:?}: {reply}"
        );
    }
    let zero = local
        .rpc(
            "ui.extensions.read",
            json!({"generation":generation,"path":"panel.js","limit":0}),
        )
        .await;
    assert_eq!(
        zero["error"]["data"]["appError"], "invalid_params",
        "{zero}"
    );
    let unknown = local
        .rpc(
            "ui.extensions.read",
            json!({"generation":generation,"path":"panel.js","limt":13}),
        )
        .await;
    assert_eq!(
        unknown["error"]["data"]["appError"], "invalid_params",
        "{unknown}"
    );

    let reload = local.rpc("plugin.reload", json!({"name":"preview"})).await;
    assert!(reload.get("error").is_none(), "{reload}");
    let next = local.rpc("ui.extensions.list", json!({})).await["result"].clone();
    let next_generation = next["extensions"][0]["generation"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_ne!(next_generation, generation);
    let stale = local
        .rpc(
            "ui.extensions.read",
            json!({"generation":generation,"path":"panel.js","limit":13}),
        )
        .await;
    assert_eq!(
        stale["error"]["data"]["appError"], "resource_gone",
        "{stale}"
    );
    let fresh = local
        .rpc(
            "ui.extensions.read",
            json!({"generation":next_generation,"path":"panel.js","limit":13}),
        )
        .await;
    assert_ok(&fresh);
    assert_eq!(decode(&fresh["result"]), &panel[..13]);

    let denied = browser
        .rpc(
            "ui.extensions.read",
            json!({"generation":next_generation,"path":"panel.js"}),
        )
        .await;
    assert_eq!(
        denied["error"]["data"]["appError"], "unknown_method",
        "{denied}"
    );
    assert!(
        !browser
            .features
            .as_array()
            .unwrap()
            .contains(&json!("ui.extensions.read")),
        "the web entry must not advertise the read method: {}",
        browser.features
    );
    let asset = format!("/api/ui/extensions/{next_generation}/panel.js");
    let (status, _, body) = http(
        &address,
        format!("GET {asset} HTTP/1.1\r\nHost: {address}\r\nCookie: {cookie}\r\nConnection: close\r\n\r\n"),
    )
    .await;
    assert_eq!((status, body.as_str()), (200, "export const revision = 1"));
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
    std::fs::remove_dir_all(assets).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_local_client_reads_session_resources_with_its_session() {
    let home = super::plugin_manager::home();
    let mut config = super::plugin_manager::config(&home);
    config.product_home = None;
    let home = home.canonicalize().unwrap();
    config.target_home = Some(home.clone());
    config.file_roots = vec![home.clone()];
    let data: Vec<u8> = (0..300_000u32).map(|index| (index % 251) as u8).collect();
    let media = home.join("media.bin");
    std::fs::write(&media, &data).unwrap();
    let (server, agent) = launch_with_agent(Arc::new(MockTools), config).await;
    let mut local = Client::new_version(&server, false, 4).await;
    let created = local.rpc("session.create", json!({"cwd":home})).await;
    assert_ok(&created);
    let id = created["result"]["sessionId"].as_str().unwrap().to_owned();
    let methods = wait_listed(&mut local, &id, "session.ui.resources.read").await;
    assert!(
        methods
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["name"] == "session.ui.resources.read"),
        "{methods}"
    );
    let resources = {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(service) = agent
                .get_session(&id)
                .and_then(|session| session.session_service::<UiResources>().ok())
            {
                break service;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "UiResources is provided"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    };
    let publisher = rutis::Ctx::root().unwrap();
    let reference = resources
        .publish_file(
            &publisher,
            PublishFile {
                path: media.display().to_string(),
                media_type: "application/octet-stream".into(),
            },
        )
        .await
        .unwrap();
    let page = local
        .rpc(
            "session.ui.resources.read",
            json!({"sessionId":id,"id":reference.id,"offset":100_000,"limit":65_536}),
        )
        .await;
    assert_ok(&page);
    assert_eq!(page["result"]["offset"], 100_000);
    assert_eq!(page["result"]["bytes"], 65_536);
    assert_eq!(page["result"]["total"], 300_000);
    assert_eq!(page["result"]["mediaType"], "application/octet-stream");
    assert_eq!(decode(&page["result"]), &data[100_000..165_536]);
    let past = local
        .rpc(
            "session.ui.resources.read",
            json!({"sessionId":id,"id":reference.id,"offset":300_001}),
        )
        .await;
    assert_eq!(
        past["error"]["data"]["appError"], "invalid_params",
        "{past}"
    );
    let unknown = local
        .rpc(
            "session.ui.resources.read",
            json!({"sessionId":id,"id":reference.id,"ofset":100_000}),
        )
        .await;
    assert_eq!(
        unknown["error"]["data"]["appError"], "invalid_params",
        "{unknown}"
    );

    let active = resources
        .publish_file(
            &publisher,
            PublishFile {
                path: media.display().to_string(),
                media_type: "image/svg+xml".into(),
            },
        )
        .await
        .unwrap();
    let normalized = local
        .rpc(
            "session.ui.resources.read",
            json!({"sessionId":id,"id":active.id,"limit":8}),
        )
        .await;
    assert_ok(&normalized);
    assert_eq!(
        normalized["result"]["mediaType"],
        "application/octet-stream"
    );

    resources
        .withdraw(&publisher, reference.clone())
        .await
        .unwrap();
    let withdrawn = local
        .rpc(
            "session.ui.resources.read",
            json!({"sessionId":id,"id":reference.id}),
        )
        .await;
    assert_eq!(
        withdrawn["error"]["data"]["appError"], "resource_gone",
        "{withdrawn}"
    );

    let other = local.rpc("session.create", json!({"cwd":home})).await;
    assert_ok(&other);
    let other_id = other["result"]["sessionId"].as_str().unwrap().to_owned();
    wait_listed(&mut local, &other_id, "session.ui.resources.read").await;
    let elsewhere = local
        .rpc(
            "session.ui.resources.read",
            json!({"sessionId":other_id,"id":active.id}),
        )
        .await;
    assert_eq!(
        elsewhere["error"]["data"]["appError"], "resource_gone",
        "a resource belongs to its own Session: {elsewhere}"
    );

    let second = resources
        .publish_file(
            &publisher,
            PublishFile {
                path: media.display().to_string(),
                media_type: "application/octet-stream".into(),
            },
        )
        .await
        .unwrap();
    publisher.shutdown().await.unwrap();
    let unloaded = local
        .rpc(
            "session.ui.resources.read",
            json!({"sessionId":id,"id":second.id}),
        )
        .await;
    assert_eq!(
        unloaded["error"]["data"]["appError"], "resource_gone",
        "a publisher's resources end with it: {unloaded}"
    );

    let deleted = local.rpc("session.delete", json!({"sessionId":id})).await;
    assert!(deleted.get("error").is_none(), "{deleted}");
    let closed = local
        .rpc(
            "session.ui.resources.read",
            json!({"sessionId":id,"id":second.id}),
        )
        .await;
    assert_eq!(
        closed["error"]["data"]["appError"], "session_closed",
        "{closed}"
    );
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_withdrawal_racing_a_page_read_never_returns_part_of_a_page() {
    let home = super::plugin_manager::home();
    let mut config = super::plugin_manager::config(&home);
    config.product_home = None;
    let home = home.canonicalize().unwrap();
    config.target_home = Some(home.clone());
    config.file_roots = vec![home.clone()];
    let data: Vec<u8> = (0..(2 * 1024 * 1024u32))
        .map(|index| (index % 251) as u8)
        .collect();
    let media = home.join("large.bin");
    std::fs::write(&media, &data).unwrap();
    let (server, agent) = launch_with_agent(Arc::new(MockTools), config).await;
    let mut local = Client::new_version(&server, false, 4).await;
    let created = local.rpc("session.create", json!({"cwd":home})).await;
    assert_ok(&created);
    let id = created["result"]["sessionId"].as_str().unwrap().to_owned();
    wait_listed(&mut local, &id, "session.ui.resources.read").await;
    let resources = agent
        .get_session(&id)
        .unwrap()
        .session_service::<UiResources>()
        .unwrap();
    let publisher = rutis::Ctx::root().unwrap();
    let reference = resources
        .publish_file(
            &publisher,
            PublishFile {
                path: media.display().to_string(),
                media_type: "application/octet-stream".into(),
            },
        )
        .await
        .unwrap();
    // Send the page request without waiting, then withdraw. The withdrawal
    // waits for the page's read token, so the reply is either the whole page
    // or `resource_gone`, never part of one.
    let request = local
        .send(
            "session.ui.resources.read",
            json!({"sessionId":id,"id":reference.id,"limit":1024 * 1024}),
        )
        .await;
    resources
        .withdraw(&publisher, reference.clone())
        .await
        .unwrap();
    let reply = local.response(request).await;
    if reply.get("error").is_some() {
        assert_eq!(
            reply["error"]["data"]["appError"], "resource_gone",
            "{reply}"
        );
    } else {
        assert_eq!(reply["result"]["bytes"], 1024 * 1024, "{reply}");
        assert_eq!(decode(&reply["result"]), &data[..1024 * 1024]);
    }
    publisher.shutdown().await.unwrap();
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_daemon_restart_ends_old_generations_and_session_resources() {
    let home = super::plugin_manager::home();
    let mut config = super::plugin_manager::config(&home);
    config.product_home = None;
    let home = home.canonicalize().unwrap();
    config.target_home = Some(home.clone());
    config.file_roots = vec![home.clone()];
    let source = home.join("preview");
    std::fs::create_dir_all(source.join("ui")).unwrap();
    std::fs::write(source.join("dim-plugin.json"), manifest().to_string()).unwrap();
    std::fs::write(source.join("ui/panel.js"), "export const revision = 1").unwrap();
    let asset: Vec<u8> = (0..(1024 * 1024u32))
        .map(|index| (index % 251) as u8)
        .collect();
    std::fs::write(source.join("ui/asset.bin"), &asset).unwrap();
    let media = home.join("media.bin");
    std::fs::write(&media, &asset).unwrap();
    std::fs::write(
        home.join("plugins.json"),
        r#"{"version":1,"installed":[],"names":{"unused":{"off":true}}}"#,
    )
    .unwrap();
    let (generation, session, resource) = {
        let (server, agent) = launch_with_agent(Arc::new(MockTools), config.clone()).await;
        let mut local = Client::new_version(&server, false, 4).await;
        let installed = local
            .rpc("plugin.install", json!({"source":{"path":source}}))
            .await;
        assert!(installed.get("error").is_none(), "{installed}");
        let directory = local.rpc("ui.extensions.list", json!({})).await["result"].clone();
        let generation = directory["extensions"][0]["generation"]
            .as_str()
            .unwrap()
            .to_owned();
        let created = local.rpc("session.create", json!({"cwd":home})).await;
        assert_ok(&created);
        let session = created["result"]["sessionId"].as_str().unwrap().to_owned();
        wait_listed(&mut local, &session, "session.ui.resources.read").await;
        let resources = agent
            .get_session(&session)
            .unwrap()
            .session_service::<UiResources>()
            .unwrap();
        let publisher = rutis::Ctx::root().unwrap();
        let reference = resources
            .publish_file(
                &publisher,
                PublishFile {
                    path: media.display().to_string(),
                    media_type: "application/octet-stream".into(),
                },
            )
            .await
            .unwrap();
        let page = local
            .rpc(
                "ui.extensions.read",
                json!({"generation":generation,"path":"asset.bin","limit":256 * 1024}),
            )
            .await;
        assert_ok(&page);
        assert_eq!(decode(&page["result"]), &asset[..256 * 1024]);
        server.shutdown();
        server.join().await.unwrap();
        drop(agent);
        drop(publisher);
        (generation, session, reference.id)
    };
    let (server, _agent) = launch_with_agent(Arc::new(MockTools), config).await;
    let mut local = Client::new_version(&server, false, 4).await;
    let stale = local
        .rpc(
            "ui.extensions.read",
            json!({"generation":generation,"path":"asset.bin","limit":256 * 1024}),
        )
        .await;
    assert_eq!(
        stale["error"]["data"]["appError"], "resource_gone",
        "{stale}"
    );
    let directory = local.rpc("ui.extensions.list", json!({})).await["result"].clone();
    let next_generation = directory["extensions"][0]["generation"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_ne!(next_generation, generation);
    let fresh = local
        .rpc(
            "ui.extensions.read",
            json!({"generation":next_generation,"path":"asset.bin","limit":256 * 1024}),
        )
        .await;
    assert_ok(&fresh);
    assert_eq!(decode(&fresh["result"]), &asset[..256 * 1024]);
    let old_resource = super::session_typescript::when_served(
        &mut local,
        "session.ui.resources.read",
        json!({"sessionId":session,"id":resource}),
    )
    .await;
    assert_eq!(
        old_resource["error"]["data"]["appError"], "resource_gone",
        "{old_resource}"
    );
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// A Session created by an older protocol version serves its resources too:
/// the entry used to refuse any Session not recorded at the daemon's own
/// version (#1459).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_older_sessions_resource_is_served_to_the_web_entry() {
    let home = super::plugin_manager::home();
    let mut config = super::plugin_manager::config(&home);
    config.product_home = None;
    let home = home.canonicalize().unwrap();
    config.target_home = Some(home.clone());
    config.file_roots = vec![home.clone()];
    config.web_assets = Some(frontend_fixture());
    let media = home.join("media.txt");
    std::fs::write(&media, "0123456789").unwrap();
    let (server, agent) = launch_with_agent(Arc::new(MockTools), config).await;
    let mut v3 = Client::new_version(&server, false, 3).await;
    let created = v3.rpc("session.create", json!({"cwd":home})).await;
    assert_ok(&created);
    let id = created["result"]["sessionId"].as_str().unwrap().to_owned();
    // The web entry is a v4 connection; the Session is recorded at 3.
    let opened = v3.rpc("web.open", json!({"port":0})).await;
    assert!(opened.get("error").is_none(), "{opened}");
    let opened = opened["result"].clone();
    let address = opened["address"].as_str().unwrap().to_owned();
    let bootstrap = opened["url"]
        .as_str()
        .unwrap()
        .split_once("#bootstrap=")
        .unwrap()
        .1;
    let (status, headers, _) = http(
        &address,
        post(
            &address,
            "/api/auth/exchange",
            &format!("http://{address}"),
            &json!({"token":bootstrap}).to_string(),
        ),
    )
    .await;
    assert_eq!(status, 204);
    let cookie = headers
        .lines()
        .find_map(|line| line.strip_prefix("set-cookie: "))
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let resources = agent
        .get_session(&id)
        .unwrap()
        .session_service::<UiResources>()
        .unwrap();
    let publisher = rutis::Ctx::root().unwrap();
    let reference = resources
        .publish_file(
            &publisher,
            PublishFile {
                path: media.display().to_string(),
                media_type: "text/plain".into(),
            },
        )
        .await
        .unwrap();
    let path = format!("/api/ui/sessions/{id}/resources/{}", reference.id);
    let (status, _, body) = http(
        &address,
        format!(
            "GET {path} HTTP/1.1\r\nHost: {address}\r\nCookie: {cookie}\r\nConnection: close\r\n\r\n"
        ),
    )
    .await;
    assert_eq!((status, body.as_str()), (200, "0123456789"));
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

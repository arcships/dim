use super::*;
use tokio::io::AsyncReadExt;
use tokio_tungstenite::tungstenite::{client::IntoClientRequest, http::HeaderValue};

/// One HTTP/1.1 exchange on a fresh connection: status, head, body.
pub(super) async fn http(address: &str, request: String) -> (u16, String, String) {
    let mut stream = TcpStream::connect(address).await.unwrap();
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut bytes = Vec::new();
    stream.read_to_end(&mut bytes).await.unwrap();
    let text = String::from_utf8_lossy(&bytes).into_owned();
    let (head, body) = text.split_once("\r\n\r\n").unwrap();
    let status = head.split(' ').nth(1).unwrap().parse().unwrap();
    (status, head.to_owned(), body.to_owned())
}
fn get(address: &str, path: &str) -> String {
    format!("GET {path} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n")
}
pub(super) fn post(address: &str, path: &str, origin: &str, body: &str) -> String {
    format!(
        "POST {path} HTTP/1.1\r\nHost: {address}\r\nOrigin: {origin}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}
async fn web_socket(
    address: &str,
    cookie: Option<&str>,
) -> Result<WebSocketStream<MaybeTlsStream<TcpStream>>, u16> {
    let mut request = format!("ws://{address}/ws").into_client_request().unwrap();
    request.headers_mut().insert(
        "Origin",
        HeaderValue::from_str(&format!("http://{address}")).unwrap(),
    );
    if let Some(cookie) = cookie {
        request
            .headers_mut()
            .insert("Cookie", HeaderValue::from_str(cookie).unwrap());
    }
    match tokio_tungstenite::connect_async(request).await {
        Ok((stream, _)) => Ok(stream),
        Err(tokio_tungstenite::tungstenite::Error::Http(response)) => {
            Err(response.status().as_u16())
        }
        Err(error) => panic!("{error}"),
    }
}
pub(super) async fn web_client(address: &str, cookie: &str) -> Client {
    let mut client = Client {
        transport: Transport::Ws(Box::new(web_socket(address, Some(cookie)).await.unwrap())),
        id: 0,
        notices: vec![],
        instance: Value::Null,
        features: Value::Null,
    };
    let hello = client
        .rpc("hello", json!({"protocolVersion":3,"clientKind":"web"}))
        .await;
    client.features = hello["result"]["features"].clone();
    client
}

/// A frontend directory as `scripts/stage-web.mjs` lays it out.
pub(super) fn frontend_fixture() -> std::path::PathBuf {
    let assets = std::env::temp_dir().join(format!("dim-web-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(assets.join("assets")).unwrap();
    std::fs::write(
        assets.join("index.html"),
        "<!doctype html><title>Dim</title>",
    )
    .unwrap();
    std::fs::write(assets.join("assets/app-1.js"), "export {}").unwrap();
    std::fs::write(
        assets.join("dimcode-web.json"),
        r#"{"version":"0.0.1","protocolVersion":3,"sourceCommit":"x"}"#,
    )
    .unwrap();
    assets
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn web_entry_serves_the_frontend_to_local_callers_only() {
    let assets = frontend_fixture();
    let server = launch(
        Arc::new(MockTools),
        ServerConfig {
            web_assets: Some(assets.clone()),
            ..Default::default()
        },
    )
    .await;
    let mut local = Client::new_version(&server, false, 3).await;
    assert!(
        local
            .features
            .as_array()
            .unwrap()
            .contains(&json!("web.open"))
    );
    let mut bare = Client::new_version(&server, true, 3).await;
    assert_eq!(
        bare.rpc("web.open", json!({})).await["error"]["data"]["appError"],
        "local_transport_required"
    );
    let mut old = Client::new_version(&server, false, 2).await;
    assert_eq!(
        old.rpc("web.status", json!({})).await["error"]["data"]["appError"],
        "unknown_method"
    );

    let opened = local.rpc("web.open", json!({"port":0})).await;
    let address = opened["result"]["address"].as_str().unwrap().to_owned();
    let (status, head, body) = http(&address, get(&address, "/assets/app-1.js")).await;
    assert_eq!(status, 200);
    assert!(head.to_ascii_lowercase().contains("immutable"), "{head}");
    assert_eq!(body, "export {}");
    let (status, head, body) = http(&address, get(&address, "/chat/new")).await;
    assert_eq!(status, 200);
    assert!(!head.to_ascii_lowercase().contains("content-encoding"));
    assert!(body.starts_with("<!doctype html>"), "{body}");

    let mut second = Client::new_version(&server, false, 3).await;
    assert_eq!(second.rpc("web.open", json!({})).await, opened);
    drop(local);
    let mut observer = Client::new_version(&server, false, 3).await;
    assert_eq!(
        observer.rpc("web.status", json!({})).await["result"]["open"],
        true
    );
    drop(second);
    tokio::time::timeout(Duration::from_secs(5), async {
        while observer.rpc("web.status", json!({})).await["result"]["open"] == true {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the entry closes with its last holder");
    assert!(TcpStream::connect(&address).await.is_err());
    std::fs::remove_dir_all(assets).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn web_switch_outlives_its_client_and_disable_closes_it() {
    let assets = frontend_fixture();
    let server = launch(
        Arc::new(MockTools),
        ServerConfig {
            web_assets: Some(assets.clone()),
            ..Default::default()
        },
    )
    .await;
    let mut local = Client::new_version(&server, false, 3).await;
    let toggled = local.rpc("web.toggle", json!({"port":0})).await;
    assert_ok(&toggled);
    assert_eq!(toggled["result"]["enabled"], true);
    assert_eq!(toggled["result"]["open"], true);
    let address = toggled["result"]["address"].as_str().unwrap().to_owned();
    assert!(
        toggled["result"]["url"]
            .as_str()
            .unwrap()
            .contains("#bootstrap=")
    );
    drop(local);

    let mut observer = Client::new_version(&server, false, 3).await;
    let status = observer.rpc("web.status", json!({})).await["result"].clone();
    assert_eq!(status["enabled"], true);
    assert_eq!(status["open"], true);
    assert_eq!(status["address"], address.as_str());

    let disabled = observer.rpc("web.disable", json!({})).await;
    assert_ok(&disabled);
    assert_eq!(disabled["result"]["enabled"], false);
    assert_eq!(disabled["result"]["open"], false);
    assert!(TcpStream::connect(&address).await.is_err());
    server.shutdown();
    std::fs::remove_dir_all(assets).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_web_assets_directory_without_a_build_is_reported() {
    let empty = std::env::temp_dir().join(format!("dim-web-empty-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&empty).unwrap();
    let server = launch(
        Arc::new(MockTools),
        ServerConfig {
            web_assets: Some(empty.clone()),
            ..Default::default()
        },
    )
    .await;
    let mut local = Client::new_version(&server, false, 3).await;
    assert_eq!(
        local.rpc("web.open", json!({})).await["error"]["data"]["appError"],
        "web_assets_missing"
    );
    std::fs::remove_dir_all(empty).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_frontend_installed_while_the_daemon_runs_is_served_without_a_restart() {
    let home = std::env::temp_dir().join(format!("dim-web-home-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&home).unwrap();
    let server = launch(
        Arc::new(MockTools),
        ServerConfig {
            web_assets: Some(home.join("web")),
            ..Default::default()
        },
    )
    .await;
    let mut local = Client::new_version(&server, false, 3).await;
    assert_eq!(
        local.rpc("web.open", json!({})).await["error"]["data"]["appError"],
        "web_assets_missing"
    );
    std::fs::rename(frontend_fixture(), home.join("web")).unwrap();
    assert!(local.rpc("web.open", json!({})).await["result"]["url"].is_string());
    server.shutdown();
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_web_assets_directory_built_for_another_protocol_is_refused() {
    let assets = std::env::temp_dir().join(format!("dim-web-old-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&assets).unwrap();
    std::fs::write(assets.join("index.html"), "<!doctype html>").unwrap();
    std::fs::write(
        assets.join("dimcode-web.json"),
        r#"{"version":"0.0.1","protocolVersion":2,"sourceCommit":"x"}"#,
    )
    .unwrap();
    let server = launch(
        Arc::new(MockTools),
        ServerConfig {
            web_assets: Some(assets.clone()),
            ..Default::default()
        },
    )
    .await;
    let mut local = Client::new_version(&server, false, 3).await;
    assert_eq!(
        local.rpc("web.open", json!({})).await["error"]["data"]["appError"],
        "web_assets_incompatible"
    );
    std::fs::write(
        assets.join("dimcode-web.json"),
        r#"{"version":"0.0.1","protocolVersion":3,"sourceCommit":"x"}"#,
    )
    .unwrap();
    assert!(local.rpc("web.open", json!({})).await["result"]["url"].is_string());
    std::fs::remove_dir_all(assets).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn web_entry_admits_token_holders_to_sessions_and_safe_management() {
    use dim_agent_capabilities::{
        model_provider::ModelProviderPlugin, workspace_tools::WorkspaceToolsConfig,
    };
    use dim_agent_runtime::SessionConfig;
    use std::os::unix::fs::DirBuilderExt;

    let home = std::path::PathBuf::from("/tmp").join(format!("dw-{}", uuid::Uuid::new_v4()));
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&home)
        .unwrap();
    let native = home.join("rust");
    let assets = home.join("web");
    std::fs::create_dir_all(assets.join("assets")).unwrap();
    std::fs::write(
        assets.join("index.html"),
        "<!doctype html><title>Dim</title>",
    )
    .unwrap();
    std::fs::write(assets.join("assets/app-1.js"), "export {}").unwrap();
    std::fs::write(home.join("secret.txt"), "private").unwrap();
    let mut builder = AgentBuilder::new(AgentConfig::default());
    defaults::install_local_execution(
        &mut builder,
        ModelProviderPlugin::new,
        WorkspaceToolsConfig {
            output_root: native.join("outputs"),
        },
        dim_agent_capabilities::timeline_tree::BranchToolConfig::default(),
    );
    dim_app_server::permission_rules::install(&mut builder);
    defaults::mock_model_hosts(&mut builder);
    let server = serve(
        builder,
        ServerConfig {
            product_home: Some(home.clone()),
            target_home: Some(native.clone()),
            unix_path: Some(native.join("daemon.sock")),
            web_assets: Some(assets),
            session_defaults: SessionConfig {
                tools: Some(vec!["read".into()]),
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let mut local = Client::new_version(&server, false, 3).await;
    let caps = dim_agent_types::CatalogCapabilities::with_limits(32000, 2048);
    assert_ok(&local.rpc("provider.create", json!({"providerId":"018ec73f-0000-7000-8000-000000000011","displayName":"Fixture","connection":{"driverKind":"openai-compatible","baseUrl":"http://127.0.0.1:1/v1","auth":{"type":"bearer"},"credential":{"action":"set","value":"fixture-secret"}},"models":[{"modelId":"alpha","displayName":"alpha","status":"active","capabilities":caps}],"enabled":true})).await);
    let opened = local.rpc("web.open", json!({"port":0})).await["result"].clone();
    let address = opened["address"].as_str().unwrap().to_owned();
    let url = opened["url"].as_str().unwrap();
    assert_eq!(
        url.split_once("/#bootstrap=").unwrap().0,
        format!("http://{address}")
    );
    let bootstrap = url.split_once("#bootstrap=").unwrap().1.to_owned();
    assert_eq!(local.rpc("web.open", json!({})).await["result"], opened);
    let origin = format!("http://{address}");

    let (status, head, body) = http(&address, get(&address, "/chat/abc")).await;
    assert_eq!(
        (status, body.as_str()),
        (200, "<!doctype html><title>Dim</title>")
    );
    assert!(
        head.contains("content-security-policy: default-src 'self'"),
        "{head}"
    );
    assert!(head.contains("cache-control: no-store"));
    let (status, head, _) = http(&address, get(&address, "/assets/app-1.js")).await;
    assert_eq!(status, 200);
    assert!(head.contains("immutable"));
    assert_eq!(
        http(&address, get(&address, "/assets/missing.js")).await.0,
        404
    );
    let (_, _, body) = http(&address, get(&address, "/../secret.txt")).await;
    assert!(!body.contains("private"));
    assert_eq!(http(&address, get(&address, "/api/unknown")).await.0, 404);
    let rebound = get(&address, "/").replace(&format!("Host: {address}"), "Host: evil.test");
    assert_eq!(http(&address, rebound).await.0, 421);

    let exchange = |origin: &str, token: &str| {
        post(
            &address,
            "/api/auth/exchange",
            origin,
            &json!({"token":token}).to_string(),
        )
    };
    assert_eq!(
        http(&address, exchange("http://evil.test", &bootstrap))
            .await
            .0,
        403
    );
    assert_eq!(http(&address, exchange(&origin, "wrong")).await.0, 401);
    assert_eq!(web_socket(&address, None).await.unwrap_err(), 401);
    assert_eq!(
        web_socket(&address, Some("dim_web=forged"))
            .await
            .unwrap_err(),
        401
    );
    let (status, head, _) = http(&address, exchange(&origin, &bootstrap)).await;
    assert_eq!(status, 204);
    let set_cookie = head
        .lines()
        .find_map(|line| line.strip_prefix("set-cookie: "))
        .unwrap();
    assert!(set_cookie.ends_with("; HttpOnly; SameSite=Strict; Path=/"));
    let cookie = set_cookie.split(';').next().unwrap().to_owned();
    assert!(cookie.starts_with("dim_web="));
    let check = |cookie: &str| {
        get(&address, "/api/auth/session")
            .replace("\r\n\r\n", &format!("\r\nCookie: {cookie}\r\n\r\n"))
    };
    assert_eq!(http(&address, check(&cookie)).await.0, 204);
    assert_eq!(http(&address, check("dim_web=forged")).await.0, 401);

    let mut stream = web_socket(&address, Some(&cookie)).await.unwrap();
    stream
        .send(Message::Text(
            json!({"jsonrpc":"2.0","id":1,"method":"hello","params":{"protocolVersion":2,"clientKind":"web"}}).to_string().into(),
        ))
        .await
        .unwrap();
    let Some(Ok(Message::Text(reply))) = stream.next().await else {
        panic!("hello reply")
    };
    let reply: Value = serde_json::from_str(&reply).unwrap();
    assert_eq!(reply["error"]["data"]["appError"], "protocol_version");

    let mut web = web_client(&address, &cookie).await;
    let features = web.features.as_array().unwrap();
    for method in [
        "session.create",
        "session.permission.update",
        "provider.catalog",
        "account.providers",
        "provider.create",
        "account.status",
        "account.login.begin",
        "account.referral",
        "account.reset_batches",
        "account.apply_reset",
        "account.codex.status",
        "account.grok.status",
        "settings.read",
        "mcp.catalog",
        "mcp.configuration",
        "mcp.inspect",
        "mcp.test",
        "mcp.auth.cancel",
        "plugin.list",
        "plugin.install",
        "skills.catalog",
    ] {
        assert!(features.contains(&json!(method)), "{method}");
    }
    for method in [
        "web.open",
        "web.status",
        "daemon.shutdown",
        "ui.open",
        "ui.shutdown",
        "provider.import_legacy",
    ] {
        assert!(!features.contains(&json!(method)), "{method}");
    }
    let created = web.rpc("session.create", json!({"cwd":home})).await;
    assert_ok(&created);
    let session_id = created["result"]["sessionId"].as_str().unwrap();
    assert_ok(
        &web.rpc(
            "session.permission.get",
            json!({"sessionId":session_id,"branchId":"main"}),
        )
        .await,
    );
    for (method, params) in [
        ("provider.list", json!({})),
        ("provider.management", json!({})),
        ("account.providers", json!({})),
        ("account.codex.status", json!({})),
        ("account.grok.status", json!({})),
        ("permission.rules", json!({})),
        ("mcp.catalog", json!({})),
        ("plugin.list", json!({})),
        (
            "provider.catalog",
            json!({"providerId":"018ec73f-0000-7000-8000-000000000011"}),
        ),
        ("provider.selection", json!({"cwd":home})),
    ] {
        let reply = web.rpc(method, params).await;
        assert_ok(&reply);
        assert!(!reply.to_string().contains("fixture-secret"), "{method}");
    }
    let created = web.rpc("provider.create", json!({
        "providerId":"018ec73f-0000-7000-8000-000000000012", "displayName":"Web fixture",
        "connection":{"driverKind":"openai-compatible", "baseUrl":"http://127.0.0.1:1/v1",
            "auth":{"type":"bearer"}, "credential":{"action":"set","value":"web-fixture-secret"},
            "headers":{"X-Private":"header-fixture-secret"},
            "network":{"proxy":"http://proxy-user:proxy-fixture-secret@127.0.0.1:1234"}},
        "models":[], "enabled":false
    })).await;
    assert_ok(&created);
    let view = web
        .rpc(
            "provider.management",
            json!({"providerId":"018ec73f-0000-7000-8000-000000000012"}),
        )
        .await;
    assert_ok(&view);
    for secret in [
        "web-fixture-secret",
        "header-fixture-secret",
        "proxy-fixture-secret",
        "proxy-user",
    ] {
        assert!(!created.to_string().contains(secret));
        assert!(!view.to_string().contains(secret));
    }
    assert_eq!(
        view["result"]["providers"][0]["connection"]["credentialConfigured"],
        true
    );
    assert_eq!(
        view["result"]["providers"][0]["connection"]["headerNames"],
        json!(["X-Private"])
    );
    let settings = web
        .rpc(
            "settings.read",
            json!({"source":"model-log","scope":"user"}),
        )
        .await;
    assert_ok(&settings);
    assert_ok(
        &web.rpc(
            "settings.update",
            json!({"source":"model-log","scope":"user",
        "expectedRevision":settings["result"]["revision"],"document":{"enabled":false}}),
        )
        .await,
    );
    let execution = web
        .rpc(
            "settings.read",
            json!({"source":"execution","scope":"user"}),
        )
        .await;
    assert_ok(&execution);
    assert_eq!(execution["result"]["document"]["sandbox"], false);
    assert_eq!(execution["result"]["document"]["network"], false);
    let backend = web.rpc("host.execution.backend", json!({})).await;
    assert_ok(&backend);
    assert!(backend["result"]["available"].is_boolean(), "{backend}");
    let updated = web
        .rpc(
            "settings.update",
            json!({"source":"execution","scope":"user",
        "expectedRevision":execution["result"]["revision"],
        "document":{"sandbox":true,"network":true}}),
        )
        .await;
    assert_ok(&updated);
    assert_eq!(updated["result"]["document"]["sandbox"], true);
    assert_eq!(updated["result"]["document"]["network"], true);
    let mut bare = Client::new_version(&server, true, 3).await;
    for method in [
        "provider.management",
        "account.status",
        "settings.read",
        "mcp.test",
        "mcp.inspect",
        "plugin.install",
    ] {
        assert!(!bare.features.as_array().unwrap().contains(&json!(method)));
        assert_eq!(
            bare.rpc(method, json!({})).await["error"]["data"]["appError"],
            "local_transport_required"
        );
    }
    for (method, params) in [
        ("daemon.shutdown", json!({})),
        ("web.open", json!({})),
        ("web.status", json!({})),
        ("ui.status", json!({})),
        ("ui.shutdown", json!({"uiId":"other-ui"})),
        ("account.credential", json!({"providerId":"x"})),
        ("provider.import_legacy", json!({"path":"/private/config"})),
        ("settings.read", json!({"source":"web","scope":"user"})),
        ("settings.read", json!({"source":"mcp","scope":"user"})),
        (
            "settings.update",
            json!({"source":"web","scope":"user","document":{}}),
        ),
    ] {
        assert_eq!(
            web.rpc(method, params).await["error"]["data"]["appError"],
            "unknown_method",
            "{method}"
        );
    }
    assert_eq!(
        local.rpc("web.status", json!({})).await["result"]["connections"],
        1
    );

    let mut observer = Client::new_version(&server, false, 3).await;
    drop(local);
    let closed = tokio::time::timeout(Duration::from_secs(5), async {
        let Transport::Ws(stream) = &mut web.transport else {
            unreachable!()
        };
        loop {
            match stream.next().await {
                None | Some(Err(_)) | Some(Ok(Message::Close(_))) => break,
                _ => {}
            }
        }
    })
    .await;
    assert!(closed.is_ok());
    assert!(TcpStream::connect(&address).await.is_err());
    assert_eq!(
        observer.rpc("web.status", json!({})).await["result"],
        json!({"enabled":false,"open":false,"connections":0})
    );
    server.shutdown();
}

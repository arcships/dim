use super::*;
use tokio::io::AsyncReadExt;
use tokio_tungstenite::tungstenite::{client::IntoClientRequest, http::HeaderValue};

/// One HTTP/1.1 exchange on a fresh connection: status, head, body.
async fn http(address: &str, request: String) -> (u16, String, String) {
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
fn post(address: &str, path: &str, origin: &str, body: &str) -> String {
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
async fn web_client(address: &str, cookie: &str) -> Client {
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn web_entry_requires_installed_assets_and_a_local_caller() {
    let server = launch(Arc::new(MockTools), ServerConfig::default()).await;
    let mut local = Client::new_version(&server, false, 3).await;
    assert!(
        local
            .features
            .as_array()
            .unwrap()
            .contains(&json!("web.open"))
    );
    assert_eq!(
        local.rpc("web.open", json!({})).await["error"]["data"]["appError"],
        "web_assets_missing"
    );
    assert_eq!(
        local.rpc("web.status", json!({})).await["result"],
        json!({"open":false,"connections":0})
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
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn web_entry_admits_token_holders_to_session_methods_only() {
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
    );
    defaults::mock_model_hosts(&mut builder);
    let server = serve(
        builder,
        ServerConfig {
            product_home: Some(home.clone()),
            target_home: Some(native.clone()),
            unix_path: Some(native.join("daemon.sock")),
            web_assets: Some(assets),
            session_defaults: SessionConfig {
                tools: vec!["read".into()],
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let mut local = Client::new_version(&server, false, 3).await;
    let caps = dim_agent_types::ModelCapabilities {
        context_window: 32000,
        max_output_tokens: 2048,
        ..Default::default()
    };
    assert_ok(&local.rpc("provider.save", json!({"provider":{"providerId":"web-fixture","displayName":"Fixture","driverKind":"openai-compatible","baseUrl":"https://model.example/v1","auth":{"type":"bearer"},"credential":{"action":"set","value":"fixture-secret"},"models":[{"modelId":"alpha","displayName":"alpha","status":"active","capabilities":caps}],"enabled":true}})).await);
    let opened = local.rpc("web.open", json!({})).await["result"].clone();
    let address = opened["address"].as_str().unwrap().to_owned();
    let url = opened["url"].as_str().unwrap();
    assert_eq!(
        url.split_once("/#bootstrap=").unwrap().0,
        format!("http://{address}")
    );
    let bootstrap = url.split_once("#bootstrap=").unwrap().1.to_owned();
    // Opening again reuses the running entry and its link.
    assert_eq!(local.rpc("web.open", json!({})).await["result"], opened);
    let origin = format!("http://{address}");

    // Static assets: immutable bundles, SPA fallback, confinement, headers.
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
    // A rebinding host name never reaches the entry.
    let rebound = get(&address, "/").replace(&format!("Host: {address}"), "Host: evil.test");
    assert_eq!(http(&address, rebound).await.0, 421);

    // Sign-in: same origin and the bootstrap token only.
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

    // The browser speaks protocol v3 only.
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

    // Session methods, including execution Session methods, and catalog reads.
    let mut web = web_client(&address, &cookie).await;
    let features = web.features.as_array().unwrap();
    for method in [
        "session.create",
        "session.permission.update",
        "provider.catalog",
    ] {
        assert!(features.contains(&json!(method)), "{method}");
    }
    for method in ["web.open", "provider.save", "mcp.catalog", "skills.catalog"] {
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
    // Catalog reads serve the model picker and carry no credential.
    for (method, params) in [
        ("provider.list", json!({})),
        ("provider.catalog", json!({"providerId":"web-fixture"})),
        ("provider.selection", json!({"cwd":home})),
    ] {
        let reply = web.rpc(method, params).await;
        assert_ok(&reply);
        assert!(!reply.to_string().contains("fixture-secret"), "{method}");
    }
    for (method, params) in [
        ("daemon.shutdown", json!({})),
        ("web.close", json!({})),
        (
            "provider.delete",
            json!({"providerId":"x","expectedVersion":1}),
        ),
        ("account.logout", json!({"providerId":"x"})),
        ("skills.catalog", json!({})),
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

    // Closing revokes every token and detaches web connections.
    assert_ok(&local.rpc("web.close", json!({})).await);
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
        local.rpc("web.status", json!({})).await["result"],
        json!({"open":false,"connections":0})
    );
    server.shutdown();
}

//! The configured User-Agent belongs to one App Server: two Servers in one
//! process keep theirs apart, and one shutting down leaves the other's in
//! place (`docs/design/appserver/provider-management.md` §2, Model request
//! identity).
use super::*;
use dim_agent_capabilities::{
    defaults,
    host::{HttpModelConnection, ModelConnection, ModelProtocol},
    model_provider::ModelProviderPlugin,
    timeline_tree::BranchToolConfig,
    workspace_tools::WorkspaceToolsConfig,
};
use dim_agent_runtime::{AgentBuilder, AgentConfig};
use dim_app_server::{RunningServer, ServerConfig, serve};
use std::{path::Path, time::Duration};

/// The User-Agent of every model request the fixture endpoint received.
type UserAgents = Arc<Mutex<Vec<String>>>;

async fn endpoint(
    State(seen): State<UserAgents>,
    headers: HeaderMap,
) -> Sse<impl futures_util::Stream<Item = Result<Event, std::convert::Infallible>>> {
    seen.lock().unwrap().push(
        headers
            .get("user-agent")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_owned(),
    );
    let first = Event::default().data(
        json!({"id":"identity","choices":[{"index":0,"delta":{"content":"ok"},"finish_reason":null}]})
            .to_string(),
    );
    let last = Event::default().data(
        json!({"id":"identity","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]})
            .to_string(),
    );
    Sse::new(stream::iter([
        Ok::<_, std::convert::Infallible>(first),
        Ok(last),
    ]))
}

fn identity_home(name: &str) -> PathBuf {
    let directory =
        std::env::temp_dir().join(format!("dim-{name}-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&directory).unwrap();
    directory.canonicalize().unwrap()
}

/// Writes the user's own `provider.json` under `home`.
fn write_provider_document(home: &Path, user_agent: &str) {
    std::fs::write(
        home.join("provider.json"),
        json!({"userAgent": user_agent}).to_string(),
    )
    .unwrap();
}

/// One App Server with its own product home, so each reads its own
/// `provider.json`.
async fn launch_identity_server(home: &Path) -> RunningServer {
    let mut builder = AgentBuilder::new(AgentConfig::default());
    defaults::install_local_execution(
        &mut builder,
        ModelProviderPlugin::process,
        WorkspaceToolsConfig {
            boundary: None,
            output_root: home.join("rust/outputs"),
        },
        BranchToolConfig::default(),
    );
    defaults::mock_hosts(&mut builder);
    serve(
        builder,
        ServerConfig {
            product_home: Some(home.to_owned()),
            target_home: Some(home.join("rust")),
            websocket_address: Some(([127, 0, 0, 1], 0).into()),
            ..Default::default()
        },
    )
    .await
    .unwrap()
}

/// The User-Agent one model request through `server` carries.
async fn sent_user_agent(server: &RunningServer, seen: &UserAgents, base: &str) -> String {
    let host = server.service::<ModelHost>().expect("a model host");
    let request = Arc::new(ModelRequest::new(
        "local",
        "same",
        vec![Message::text(Role::User, "hello")],
    ));
    let connection = ModelConnection::Http(HttpModelConnection {
        protocol: ModelProtocol::OpenaiCompatible,
        base_url: base.to_owned(),
        network: Default::default(),
        headers: Default::default(),
        bearer: None,
        api_key: None,
        preset: None,
    });
    let mut output = host
        .0
        .invoke(request, connection, CancellationToken::new())
        .await
        .unwrap();
    while let Some(chunk) = output.next().await {
        chunk.unwrap();
    }
    seen.lock().unwrap().pop().expect("one request arrived")
}

/// Two Servers in one process keep their own configured User-Agent, and one
/// closing leaves the other's in place.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_app_servers_keep_their_configured_user_agent_apart() {
    tokio::time::timeout(Duration::from_secs(60), async {
        let seen: UserAgents = Arc::new(Mutex::new(Vec::new()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/v1", listener.local_addr().unwrap());
        let app = Router::new().fallback(endpoint).with_state(seen.clone());
        let endpoint_server =
            tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let first_home = identity_home("pid-first");
        let second_home = identity_home("pid-second");
        write_provider_document(&first_home, "first-client/1");
        write_provider_document(&second_home, "second-client/2");
        let first = launch_identity_server(&first_home).await;
        let second = launch_identity_server(&second_home).await;

        for (server, expected) in [
            (&first, "first-client/1"),
            (&second, "second-client/2"),
            (&first, "first-client/1"),
        ] {
            let user_agent = sent_user_agent(server, &seen, &base).await;
            assert!(
                user_agent.starts_with(expected),
                "{user_agent} does not lead with {expected}"
            );
        }

        first.shutdown();
        first.join().await.unwrap();
        let user_agent = sent_user_agent(&second, &seen, &base).await;
        assert!(
            user_agent.starts_with("second-client/2"),
            "closing the other Server left this one's identity alone: {user_agent}"
        );

        second.shutdown();
        second.join().await.unwrap();
        endpoint_server.abort();
        std::fs::remove_dir_all(first_home).unwrap();
        std::fs::remove_dir_all(second_home).unwrap();
    })
    .await
    .unwrap();
}

/// `DIM_USER_AGENT` wins over the configured setting, and clearing the
/// setting falls back to the environment's value rather than the built-in
/// identity.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_environment_value_wins_over_the_configured_identity() {
    tokio::time::timeout(Duration::from_secs(60), async {
        let seen: UserAgents = Arc::new(Mutex::new(Vec::new()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/v1", listener.local_addr().unwrap());
        let app = Router::new().fallback(endpoint).with_state(seen.clone());
        let endpoint_server =
            tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let home = identity_home("pid-env");
        write_provider_document(&home, "configured-client/2");
        let mut builder = AgentBuilder::new(AgentConfig::default());
        defaults::install_local_execution(
            &mut builder,
            ModelProviderPlugin::process,
            WorkspaceToolsConfig {
                boundary: None,
                output_root: home.join("rust/outputs"),
            },
            BranchToolConfig::default(),
        );
        defaults::mock_hosts(&mut builder);
        let server = serve(
            builder,
            ServerConfig {
                product_home: Some(home.clone()),
                target_home: Some(home.join("rust")),
                websocket_address: Some(([127, 0, 0, 1], 0).into()),
                user_agent: Some("private-client/7".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();

        let user_agent = sent_user_agent(&server, &seen, &base).await;
        assert!(
            user_agent.starts_with("private-client/7"),
            "the environment value wins over the setting: {user_agent}"
        );

        let settings = server
            .service::<dim_app_server::settings::SettingsService>()
            .expect("the Server serves settings");
        let cancel = CancellationToken::new();
        let current = settings
            .read_declared(
                "provider",
                dim_app_server::settings::SettingsScope::User,
                &cancel,
            )
            .await
            .unwrap();
        settings
            .update_declared(
                "provider",
                dim_app_server::settings::SettingsScope::User,
                &current.revision,
                json!({}),
                dim_app_server::settings::Writer::Client,
                &cancel,
            )
            .await
            .unwrap();
        let user_agent = sent_user_agent(&server, &seen, &base).await;
        assert!(
            user_agent.starts_with("private-client/7"),
            "clearing the setting falls back to the environment: {user_agent}"
        );

        server.shutdown();
        server.join().await.unwrap();
        endpoint_server.abort();
        std::fs::remove_dir_all(home).unwrap();
    })
    .await
    .unwrap();
}

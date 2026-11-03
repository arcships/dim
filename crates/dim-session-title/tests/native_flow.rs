#![cfg(unix)]
mod support;
use axum::{Router, routing::post};
use dim_agent_capabilities::{adapters::MockTools, defaults};
use dim_agent_runtime::{AgentBuilder, AgentConfig};
use dim_agent_types::EventKind;
use dim_app_server::{ServerConfig, serve};
use dim_client::{Client, Message};
use rutis::host::CtxHost;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::{sync::Arc, time::Duration};
use support::*;
use tokio::sync::{mpsc, oneshot};

type Generation = (
    dim_session_title::GenerateTitleRequest,
    oneshot::Sender<Value>,
);

struct Generator(mpsc::UnboundedSender<Generation>);
impl rutis_bridge::session::HostDispatch for Generator {
    fn methods(&self) -> Option<Value> {
        Some(json!({"generate":"async"}))
    }
    fn invoke(
        &self,
        method: &str,
        args: rutis_bridge::session::Value,
    ) -> rutis_bridge::session::Reply {
        use rutis_bridge::session::{Value as V, settle};
        assert_eq!(method, "generate");
        let mut args = args.list()?.into_iter();
        let request = serde_json::from_value(args.next().unwrap().json()?).unwrap();
        let cancellation = args.next().unwrap().reference()?;
        let (release, reply) = oneshot::channel();
        self.0.send((request, release)).unwrap();
        Ok(V::future(async move {
            tokio::select! {
                reply = reply => Ok(reply.unwrap().into()),
                _ = async {
                    let returned = cancellation.call_async(V::List(vec![])).await?;
                    settle(returned).await
                } => Ok(json!({"kind":"cancelled"}).into()),
            }
        }))
    }
}
struct GeneratorPlugin(Arc<Generator>);
impl rutis::Plugin for GeneratorPlugin {
    type Deps = ();
    fn name(&self) -> &str {
        "TitleGeneratorFixture"
    }
    async fn apply(&self, ctx: &rutis::Ctx, (): ()) -> rutis::Result {
        ctx.provide_at(
            rutis_bridge::session::host_key(dim_session_title::GENERATOR),
            self.0.clone() as Arc<dyn rutis_bridge::session::HostDispatch>,
        )?;
        Ok(())
    }
}

async fn entry(client: &mut Client, id: &str) -> Value {
    let listed = client.rpc("session.summaries", json!({})).await.unwrap();

    listed
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["sessionId"] == id)
        .unwrap()
        .clone()
}

async fn titled(client: &mut Client, id: &str, text: &str, source: &str) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let current = entry(client, id).await;
            if current["title"] == text
                && current["titleSource"] == source
                && client.notices.iter().any(|notice| {
                    matches!(notice,
                    Message::Notice { method, params }
                    if method == "session.notice" && params["sessionId"] == id
                        && params["kind"] == "title"
                        && params["data"]["titleRevision"] == current["titleRevision"])
                })
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the committed title and notice arrive");
}

async fn create(client: &mut Client, home: &std::path::Path, interactive: bool) -> String {
    let created = client
        .rpc(
            "session.create",
            json!({"cwd":home,
        "tools":["wait"],"interactive":interactive,
        "permissionPolicy":{"preset":"full-access","allowedTools":[],"disallowedTools":[]}}),
        )
        .await
        .unwrap();

    let id = created["sessionId"].as_str().unwrap().to_owned();
    client
        .rpc("session.attach", json!({"sessionId":id}))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let reply = client
                .rpc("session.rename", json!({"sessionId":id,"title":0}))
                .await
                .unwrap_err();
            if dim_client::app_error(&reply) == Some("invalid_params") {
                break;
            }
            assert!(waiting(&reply), "{reply}");
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the built-in SessionTitle part activates");
    id
}

async fn submit(client: &mut Client, id: &str, text: &str) {
    client
        .rpc(
            "session.submit",
            json!({"sessionId":id,"branchId":"main","text":text}),
        )
        .await
        .unwrap();
}

async fn idle(client: &mut Client, id: &str) {
    client
        .rpc("session.idle", json!({"sessionId":id,"branchId":"main"}))
        .await
        .unwrap();
}

fn title_builder(provider: &str) -> AgentBuilder {
    let mut builder = AgentBuilder::new(AgentConfig::default());
    dim_session_title::install(&mut builder);
    defaults::install_with_prompt(
        &mut builder,
        Arc::new(dim_agent_capabilities::host::EmptyPromptHost),
        Some(dim_agent_runtime::ConfigureUpdate {
            sources: vec![],
            model: Some(dim_agent_types::ModelSelection {
                provider_id: provider.into(),
                model_id: "main".into(),
                capability_snapshot: None,
                config: None,
                metadata: None,
            }),
        }),
    );
    defaults::mock_model_hosts(&mut builder);
    defaults::tool_host(&mut builder, Arc::new(MockTools));
    dim_app_server::collaboration::install(&mut builder);
    builder
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn native_session_title_generation_rename_settings_restore_fork_and_close() {
    let store = Store::new();
    let choice =
        json!({"version":1,"installed":[],"names":{"session-title":{"use":"default"}}}).to_string();
    std::fs::write(store.0.join("plugins.json"), &choice).unwrap();
    let (sent, mut requests) = mpsc::unbounded_channel::<Request>();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = Router::new()
        .route("/{*path}", post(complete))
        .with_state(sent);
    let http = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let builder = title_builder("018ec73f-0000-7000-8000-000000000008");
    let server = serve(
        builder,
        ServerConfig {
            unix_path: Some(
                PathBuf::from("/tmp")
                    .join(format!("dim-title-{}/server.sock", uuid::Uuid::new_v4())),
            ),
            product_home: Some(store.0.clone()),
            ..store.config()
        },
    )
    .await
    .unwrap();
    let agent = server.agent().unwrap();
    let mut client = client(&server).await;
    let plugins = client.rpc("plugin.list", json!({})).await.unwrap();
    assert_eq!(
        plugins
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["name"] == "session-title")
            .unwrap()["use"],
        "builtin"
    );
    assert_eq!(
        std::fs::read_to_string(store.0.join("plugins.json")).unwrap(),
        choice
    );
    client.rpc("provider.create", json!({"providerId":"018ec73f-0000-7000-8000-000000000008","displayName":"Title fixture","connection":{"driverKind":"openai-compatible","baseUrl":format!("http://{address}/v1"),"auth":{"type":"anonymous"}},"models":[{"modelId":"main","displayName":"Main","status":"active",
            "capabilities":dim_agent_types::CatalogCapabilities::with_limits(128_000, 16_384)},
            {"modelId":"explicit","displayName":"Explicit","status":"active",
            "capabilities":dim_agent_types::CatalogCapabilities::with_limits(128_000, 16_384)}],"enabled":true})).await.unwrap();
    client
        .rpc(
            "provider.select",
            json!({"providerId":"018ec73f-0000-7000-8000-000000000008","modelId":"main"}),
        )
        .await
        .unwrap();
    let defaults = client
        .rpc(
            "settings.read",
            json!({"source":"session-title","scope":"user"}),
        )
        .await
        .unwrap();

    assert_eq!(
        defaults["document"],
        json!({"enabled":true,"maxLength":80,"model":null})
    );
    for document in [
        json!({"maxLength":9}),
        json!({"model":{"providerId":"x"}}),
        json!({"extra":true}),
    ] {
        let rejected = client
            .rpc(
                "settings.update",
                json!({"source":"session-title","scope":"user",
            "expectedRevision":defaults["revision"],"document":document}),
            )
            .await
            .unwrap_err();
        assert_eq!(
            dim_client::app_error(&rejected),
            Some("invalid_params"),
            "{rejected}"
        );
    }
    let unchanged = client
        .rpc(
            "settings.read",
            json!({"source":"session-title","scope":"user"}),
        )
        .await
        .unwrap();
    assert_eq!(unchanged, defaults);
    let project = client
        .rpc(
            "settings.read",
            json!({"source":"session-title","scope":{"project":0}}),
        )
        .await;
    assert!(project.is_err(), "{project:?}");

    let id = create(&mut client, &store.0, true).await;
    let first = r#"tool:wait {"millis":60000}"#;
    submit(&mut client, &id, first).await;
    let (request, release) = tokio::time::timeout(Duration::from_secs(10), requests.recv())
        .await
        .expect("the adopted model receives the title request")
        .unwrap();
    assert_eq!(request["model"], "main");
    assert_eq!(request["messages"].as_array().unwrap().len(), 2);
    assert_eq!(request["messages"][1]["content"], first);
    assert!(
        request["messages"][0]["content"]
            .as_str()
            .unwrap()
            .contains("at most 8 words")
    );
    assert!(request.get("tools").is_none());
    release
        .send("“First conversation!!!”\nDo not use this line".into())
        .unwrap();
    titled(&mut client, &id, "First conversation", "auto").await;
    let session = agent.get_session(&id).unwrap();
    assert!(
        !session
            .local("main", ..)
            .await
            .unwrap()
            .iter()
            .any(|event| matches!(event.kind, EventKind::TurnEnd(_))),
        "title generation does not wait for the controlled main turn"
    );
    client
        .rpc("session.stop", json!({"sessionId":id,"branchId":"main"}))
        .await
        .unwrap();
    idle(&mut client, &id).await;
    let before_rename = session.local("main", ..).await.unwrap();
    let renamed = when_served(
        &mut client,
        "session.rename",
        json!({"sessionId":id,"title":"  Manual 😀  "}),
    )
    .await;

    assert_eq!(renamed["title"], "Manual 😀");
    titled(&mut client, &id, "Manual 😀", "manual").await;
    assert_eq!(
        before_rename
            .iter()
            .map(|event| &event.id)
            .collect::<Vec<_>>(),
        session
            .local("main", ..)
            .await
            .unwrap()
            .iter()
            .map(|event| &event.id)
            .collect::<Vec<_>>()
    );
    submit(&mut client, &id, "later input stays private").await;
    idle(&mut client, &id).await;
    assert!(requests.try_recv().is_err());

    when_served(
        &mut client,
        "session.rename",
        json!({"sessionId":id,"title":null}),
    )
    .await;
    submit(&mut client, &id, "manual rename during generation").await;
    let (_, release) = tokio::time::timeout(Duration::from_secs(10), requests.recv())
        .await
        .unwrap()
        .unwrap();
    when_served(
        &mut client,
        "session.rename",
        json!({"sessionId":id,"title":"Winner"}),
    )
    .await;
    let mut held = dim_app_server::storage::testing::hold_title_write(&id);
    release.send("Stale automatic title".into()).unwrap();
    tokio::time::timeout(Duration::from_secs(10), held.arrived())
        .await
        .unwrap();
    idle(&mut client, &id).await;
    let unloading = {
        let session = session.clone();
        tokio::spawn(async move { session.unload_if_idle().await })
    };
    drop(held);
    assert!(unloading.await.unwrap().unwrap());
    assert_eq!(entry(&mut client, &id).await["title"], "Winner");
    when_served(
        &mut client,
        "session.rename",
        json!({"sessionId":id,"title":"Restored"}),
    )
    .await;
    titled(&mut client, &id, "Restored", "manual").await;
    let session = agent.get_session(&id).unwrap();
    when_served(
        &mut client,
        "session.rename",
        json!({"sessionId":id,"title":null}),
    )
    .await;
    configure(
        &mut client,
        json!({"enabled":true,"maxLength":10,
        "model":{"providerId":"018ec73f-0000-7000-8000-000000000008","modelId":"explicit"}}),
    )
    .await;
    submit(&mut client, &id, "regenerate after clear").await;
    let (request, release) = tokio::time::timeout(Duration::from_secs(10), requests.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(request["model"], "explicit");
    assert_eq!(request["messages"][1]["content"], first);
    release.send("😀".repeat(12)).unwrap();
    titled(&mut client, &id, &"😀".repeat(10), "auto").await;
    idle(&mut client, &id).await;

    let fork = client
        .rpc(
            "session.fork",
            json!({"sessionId":id,"at":session.sequence("main").unwrap()}),
        )
        .await
        .unwrap();

    let fork_id = fork["sessionId"].as_str().unwrap().to_owned();
    assert_eq!(entry(&mut client, &fork_id).await["title"], Value::Null);
    client
        .rpc("session.attach", json!({"sessionId":fork_id}))
        .await
        .unwrap();
    submit(&mut client, &fork_id, "fork's new input").await;
    let (request, release) = tokio::time::timeout(Duration::from_secs(10), requests.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(request["messages"][1]["content"], first);
    release.send("Fork title".into()).unwrap();
    titled(&mut client, &fork_id, "Fork title", "auto").await;

    let background = create(&mut client, &store.0, false).await;
    submit(&mut client, &background, "no automatic background title").await;
    idle(&mut client, &background).await;
    when_served(
        &mut client,
        "session.rename",
        json!({"sessionId":background,"title":"Manual bg"}),
    )
    .await;
    titled(&mut client, &background, "Manual bg", "manual").await;
    assert!(requests.try_recv().is_err());

    let interrupted = create(&mut client, &store.0, true).await;
    submit(&mut client, &interrupted, first).await;
    let (_, release) = tokio::time::timeout(Duration::from_secs(10), requests.recv())
        .await
        .unwrap()
        .unwrap();
    client
        .rpc(
            "session.stop",
            json!({"sessionId":interrupted,"branchId":"main"}),
        )
        .await
        .unwrap();
    idle(&mut client, &interrupted).await;
    release.send("Stopped".into()).unwrap();
    titled(&mut client, &interrupted, "Stopped", "auto").await;

    let closing = create(&mut client, &store.0, true).await;
    submit(&mut client, &closing, "cancel the auxiliary call").await;
    let (_, mut release) = tokio::time::timeout(Duration::from_secs(10), requests.recv())
        .await
        .unwrap()
        .unwrap();
    agent.get_session(&closing).unwrap().close().await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), release.closed())
        .await
        .unwrap();
    assert_eq!(entry(&mut client, &closing).await["title"], Value::Null);

    let overlapping = create(&mut client, &store.0, true).await;
    let excerpt = "😀".repeat(2001);
    submit(&mut client, &overlapping, &excerpt).await;
    let (request, release) = tokio::time::timeout(Duration::from_secs(10), requests.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(request["messages"][1]["content"], "😀".repeat(2000));
    submit(
        &mut client,
        &overlapping,
        "second input while generation is active",
    )
    .await;
    idle(&mut client, &overlapping).await;
    assert!(
        requests.try_recv().is_err(),
        "overlapping inputs share one title attempt"
    );
    release.send("\"Bounded. \nprivate\"".into()).unwrap();
    titled(&mut client, &overlapping, "Bounded", "auto").await;
    for title in [
        json!(0),
        json!(""),
        json!("   "),
        json!("line\nbreak"),
        json!("\u{0001}text"),
        json!("😀".repeat(11)),
    ] {
        let rejected = client
            .rpc(
                "session.rename",
                json!({"sessionId":overlapping,"title":title}),
            )
            .await
            .unwrap_err();
        assert_eq!(
            dim_client::app_error(&rejected),
            Some("invalid_params"),
            "{rejected}"
        );
    }

    configure(&mut client, json!({"enabled":false})).await;
    let disabled = create(&mut client, &store.0, true).await;
    submit(&mut client, &disabled, "disabled first input").await;
    idle(&mut client, &disabled).await;
    configure(&mut client, json!({"enabled":true})).await;
    submit(&mut client, &disabled, "enabled later input").await;
    let (request, release) = tokio::time::timeout(Duration::from_secs(10), requests.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(request["messages"][1]["content"], "disabled first input");
    release.send("Enabled".into()).unwrap();
    titled(&mut client, &disabled, "Enabled", "auto").await;

    let images = create(&mut client, &store.0, true).await;
    client.rpc("session.submit", json!({"sessionId":images,"branchId":"main","user":{"content":[
        {"type":"image","source":{"type":"url","mediaType":"image/png","url":"https://example.com/image.png"}}
    ]}})).await.unwrap();
    idle(&mut client, &images).await;
    submit(
        &mut client,
        &images,
        "text after the image-only first input",
    )
    .await;
    idle(&mut client, &images).await;
    assert!(
        requests.try_recv().is_err(),
        "an image-only first input has no automatic title"
    );
    client
        .rpc("plugin.disable", json!({"name":"session-title"}))
        .await
        .unwrap();
    let error = client
        .rpc(
            "settings.read",
            json!({"source":"session-title","scope":"user"}),
        )
        .await
        .unwrap_err();
    assert_eq!(
        dim_client::app_error(&error),
        Some("settings_unknown_source")
    );
    client
        .rpc("plugin.enable", json!({"name":"session-title"}))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let listed = client.rpc("plugin.list", json!({})).await.unwrap();
            if listed
                .as_array()
                .unwrap()
                .iter()
                .any(|p| p["name"] == "session-title" && p["status"] == "running")
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let document = client
        .rpc(
            "settings.read",
            json!({"source":"session-title","scope":"user"}),
        )
        .await
        .unwrap();
    assert_eq!(document["document"]["enabled"], true);
    let renamed = when_served(
        &mut client,
        "session.rename",
        json!({"sessionId":images,"title":"Reactivated"}),
    )
    .await;
    assert_eq!(renamed["title"], "Reactivated");
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    http.abort();
}

#[tokio::test]
async fn optional_title_generation_routes_falls_back_and_cancels_without_gating_the_session() {
    let store = Store::new();
    let (sent, mut requests) = mpsc::unbounded_channel::<Request>();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let http = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new()
                .route("/{*path}", post(complete))
                .with_state(sent),
        )
        .await
        .unwrap();
    });
    let (sent, mut generations) = mpsc::unbounded_channel::<Generation>();
    let generator = Arc::new(Generator(sent));
    let provider = "018ec73f-0000-7000-8000-000000000009";
    let mut builder = title_builder(provider);
    builder.plugin(move |_| Ok(Box::new(GeneratorPlugin(generator.clone()))));
    let server = serve(
        builder,
        ServerConfig {
            unix_path: Some(PathBuf::from("/tmp").join(format!(
                "dim-title-routing-{}/server.sock",
                uuid::Uuid::new_v4()
            ))),
            product_home: Some(store.0.clone()),
            ..store.config()
        },
    )
    .await
    .unwrap();
    let agent = server.agent().unwrap();
    let mut client = client(&server).await;
    client.rpc("provider.create", json!({"providerId":provider,"displayName":"Routing fixture",
        "connection":{"driverKind":"openai-compatible","baseUrl":format!("http://{address}/v1"),"auth":{"type":"anonymous"}},
        "models":[{"modelId":"main","displayName":"Main","status":"active",
            "capabilities":dim_agent_types::CatalogCapabilities::with_limits(128_000,16_384)}],"enabled":true})).await.unwrap();
    client
        .rpc(
            "provider.select",
            json!({"providerId":provider,"modelId":"main"}),
        )
        .await
        .unwrap();

    let id = create(&mut client, &store.0, true).await;
    submit(&mut client, &id, "a dedicated title request").await;
    let (request, release) = tokio::time::timeout(Duration::from_secs(10), generations.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(request.provider_id, provider);
    assert_eq!(request.text, "a dedicated title request");
    release
        .send(json!({"kind":"generated","title":"Dedicated title"}))
        .unwrap();
    titled(&mut client, &id, "Dedicated title", "auto").await;
    assert!(
        requests.try_recv().is_err(),
        "a generated title never calls the fallback model"
    );

    configure(
        &mut client,
        json!({"model":{"providerId":provider,"modelId":"main"}}),
    )
    .await;
    let explicit = create(&mut client, &store.0, true).await;
    submit(&mut client, &explicit, "use the explicit title model").await;
    let (request, release) = tokio::time::timeout(Duration::from_secs(10), requests.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(request["model"], "main");
    release.send("Explicit model".into()).unwrap();
    titled(&mut client, &explicit, "Explicit model", "auto").await;
    assert!(
        generations.try_recv().is_err(),
        "an explicit model bypasses the optional service"
    );
    configure(&mut client, json!({"model":null})).await;

    for reply in [
        json!({"kind":"unavailable","reason":"unsupported_provider"}),
        json!({"kind":"unavailable","reason":"quota_exhausted"}),
        json!({"kind":"failed"}),
        json!({"kind":"generated","title":"  "}),
    ] {
        let fallback = create(&mut client, &store.0, true).await;
        submit(&mut client, &fallback, "first request for fallback").await;
        let (_, release) = tokio::time::timeout(Duration::from_secs(10), generations.recv())
            .await
            .unwrap()
            .unwrap();
        release.send(reply).unwrap();
        let (_, release) = tokio::time::timeout(Duration::from_secs(10), requests.recv())
            .await
            .unwrap()
            .unwrap();
        release.send("Fallback model".into()).unwrap();
        titled(&mut client, &fallback, "Fallback model", "auto").await;
        assert!(
            requests.try_recv().is_err(),
            "only one fallback request is made"
        );
    }

    let failed = create(&mut client, &store.0, true).await;
    submit(
        &mut client,
        &failed,
        "request text\n survives model failure??",
    )
    .await;
    let (_, release) = tokio::time::timeout(Duration::from_secs(10), generations.recv())
        .await
        .unwrap()
        .unwrap();
    release.send(json!({"kind":"failed"})).unwrap();
    let (_, release) = tokio::time::timeout(Duration::from_secs(10), requests.recv())
        .await
        .unwrap()
        .unwrap();
    release.send("   ".into()).unwrap();
    titled(
        &mut client,
        &failed,
        "request text survives model failure??",
        "auto",
    )
    .await;

    let deadline = create(&mut client, &store.0, true).await;
    submit(
        &mut client,
        &deadline,
        "the deadline keeps the request text",
    )
    .await;
    let (_, mut release) = tokio::time::timeout(Duration::from_secs(10), generations.recv())
        .await
        .unwrap()
        .unwrap();
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(30)).await;
    tokio::time::resume();
    tokio::time::timeout(Duration::from_secs(5), release.closed())
        .await
        .unwrap();
    titled(
        &mut client,
        &deadline,
        "the deadline keeps the request text",
        "auto",
    )
    .await;
    assert!(
        requests.try_recv().is_err(),
        "the shared deadline never starts another model call"
    );

    let manual = create(&mut client, &store.0, true).await;
    submit(
        &mut client,
        &manual,
        "rename while the service is generating",
    )
    .await;
    let (_, release) = tokio::time::timeout(Duration::from_secs(10), generations.recv())
        .await
        .unwrap()
        .unwrap();
    when_served(
        &mut client,
        "session.rename",
        json!({"sessionId":manual,"title":"Manual winner"}),
    )
    .await;
    release
        .send(json!({"kind":"generated","title":"Stale service title"}))
        .unwrap();
    titled(&mut client, &manual, "Manual winner", "manual").await;

    let closing = create(&mut client, &store.0, true).await;
    submit(&mut client, &closing, "close the optional service request").await;
    let (_, mut release) = tokio::time::timeout(Duration::from_secs(10), generations.recv())
        .await
        .unwrap()
        .unwrap();
    agent.get_session(&closing).unwrap().close().await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), release.closed())
        .await
        .unwrap();
    assert_eq!(entry(&mut client, &closing).await["title"], Value::Null);
    assert!(
        requests.try_recv().is_err(),
        "closing never starts a fallback model call"
    );

    client
        .rpc("plugin.disable", json!({"name":"TitleGeneratorFixture"}))
        .await
        .unwrap();
    let withdrawn = create(&mut client, &store.0, true).await;
    submit(
        &mut client,
        &withdrawn,
        "title after the optional provider was withdrawn",
    )
    .await;
    let (_, release) = tokio::time::timeout(Duration::from_secs(10), requests.recv())
        .await
        .unwrap()
        .unwrap();
    release.send("Provider withdrawn".into()).unwrap();
    titled(&mut client, &withdrawn, "Provider withdrawn", "auto").await;
    assert!(generations.try_recv().is_err());

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    http.abort();
}

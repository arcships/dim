//! Modality image slice through the management methods: the owner's snapshot,
//! resolution order and CAS, with a fixture adapter in the process.
#![cfg(unix)]
use dim_agent_capabilities::{adapters::MockTools, defaults, host::ToolDriver};
use dim_agent_runtime::{AgentBuilder, AgentConfig};
use dim_agent_types::{CatalogCapabilities, ModelId};
use dim_app_server::{RunningServer, ServerConfig, serve};
use dim_modality::{AdapterOperation, ModalityAdapter, ModalityAdapterService};
use futures_util::{SinkExt, StreamExt};
use rutis::{Ctx, Plugin};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpStream, UnixStream};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, tungstenite::Message};

enum Transport {
    Unix(BufReader<UnixStream>),
    Ws(Box<WebSocketStream<MaybeTlsStream<TcpStream>>>),
}
struct Client {
    transport: Transport,
    id: u64,
    notices: Vec<Value>,
}
impl Client {
    async fn new_version(server: &RunningServer, ws: bool, version: u32) -> Self {
        let transport = if ws {
            Transport::Ws(Box::new(
                tokio_tungstenite::connect_async_with_config(
                    format!("ws://{}/ws", server.websocket_address.unwrap()),
                    Some(
                        tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default()
                            .max_frame_size(Some(128 * 1024 * 1024))
                            .max_message_size(Some(128 * 1024 * 1024)),
                    ),
                    false,
                )
                .await
                .unwrap()
                .0,
            ))
        } else {
            Transport::Unix(BufReader::new(
                UnixStream::connect(server.unix_path.as_ref().unwrap())
                    .await
                    .unwrap(),
            ))
        };
        let mut this = Self {
            transport,
            id: 0,
            notices: vec![],
        };
        let result = this
            .rpc(
                "hello",
                json!({"protocolVersion":version,"clientKind":"other"}),
            )
            .await;
        assert_eq!(result["result"]["protocolVersion"], version);
        if version == 3 {
            assert!(result["result"]["features"].is_array());
        }
        this
    }
    async fn send(&mut self, method: &str, params: Value) -> u64 {
        self.id += 1;
        let value = json!({"jsonrpc":"2.0","id":self.id,"method":method,"params":params});
        self.raw(value).await;
        self.id
    }
    async fn raw(&mut self, value: Value) {
        let text = value.to_string();
        match &mut self.transport {
            Transport::Unix(stream) => {
                stream.get_mut().write_all(text.as_bytes()).await.unwrap();
                stream.get_mut().write_all(b"\n").await.unwrap();
            }
            Transport::Ws(stream) => stream.send(Message::Text(text.into())).await.unwrap(),
        }
    }
    async fn read(&mut self, expected: &str) -> Value {
        tokio::time::timeout(Duration::from_secs(30), async {
            match &mut self.transport {
                Transport::Unix(stream) => {
                    let mut line = String::new();
                    assert_ne!(
                        stream.read_line(&mut line).await.unwrap(),
                        0,
                        "connection unexpectedly closed"
                    );
                    serde_json::from_str(&line).unwrap()
                }
                Transport::Ws(stream) => loop {
                    match stream.next().await.unwrap().unwrap() {
                        Message::Text(text) => break serde_json::from_str(&text).unwrap(),
                        Message::Ping(_) | Message::Pong(_) => {}
                        other => panic!("unexpected websocket: {other:?}"),
                    }
                },
            }
        })
        .await
        .unwrap_or_else(|error| panic!("deadline waiting for {expected}: {error}"))
    }
    async fn response(&mut self, id: u64) -> Value {
        if let Some(index) = self.notices.iter().position(|v| v["id"] == id) {
            return self.notices.remove(index);
        }
        loop {
            let value = self.read(&format!("response {id}")).await;
            if value["id"] == id {
                return value;
            }
            self.notices.push(value);
        }
    }
    async fn rpc(&mut self, method: &str, params: Value) -> Value {
        let id = self.send(method, params).await;
        self.response(id).await
    }
}

/// A product profile with Modality installed, as the product's installer does.
async fn launch_with(
    tools: Arc<dyn ToolDriver>,
    mut config: ServerConfig,
    extra: impl FnOnce(&mut AgentBuilder),
) -> RunningServer {
    config.unix_path = Some(std::path::PathBuf::from("/tmp").join(format!(
        "dim-modality-test-{}/server.sock",
        uuid::Uuid::new_v4()
    )));
    let mut builder = AgentBuilder::new(AgentConfig::default());
    builder.timeline_store(Arc::new(dim_agent_runtime::MemoryTimelineStore::default()));
    defaults::install(&mut builder);
    defaults::mock_model_hosts(&mut builder);
    dim_agent_capabilities::defaults::tool_host(&mut builder, tools);
    dim_app_server::collaboration::install(&mut builder);
    dim_modality::install(&mut builder);
    extra(&mut builder);
    serve(builder, config).await.unwrap()
}
fn assert_ok(value: &Value) {
    assert!(value.get("error").is_none(), "{value}");
}

/// The first slice's only adapter: it serves the image operations a test
/// declares per model id, and nothing else.
struct FixtureAdapter {
    /// Model id to the operation ids it serves; `None` serves all three.
    served: Mutex<Option<BTreeMap<String, BTreeSet<String>>>>,
    available: AtomicBool,
}
impl Default for FixtureAdapter {
    fn default() -> Self {
        Self {
            served: Mutex::new(None),
            available: AtomicBool::new(true),
        }
    }
}
impl FixtureAdapter {
    fn serve(&self, served: &[(&str, &[&str])]) {
        *self.served.lock().unwrap() = Some(
            served
                .iter()
                .map(|(model, operations)| {
                    (
                        (*model).to_owned(),
                        operations
                            .iter()
                            .map(|operation| (*operation).to_owned())
                            .collect(),
                    )
                })
                .collect(),
        );
    }
    fn set_available(&self, available: bool) {
        self.available.store(available, Ordering::SeqCst);
    }
}
impl ModalityAdapter for FixtureAdapter {
    fn available(&self) -> bool {
        self.available.load(Ordering::SeqCst)
    }
    fn operations(&self, model: &ModelId) -> Vec<AdapterOperation> {
        let served = self.served.lock().unwrap();
        let operations: BTreeSet<String> = match served.as_ref() {
            Some(served) => served.get(&model.model_id).cloned().unwrap_or_default(),
            None => ["generate", "edit", "enhance"]
                .into_iter()
                .map(str::to_owned)
                .collect(),
        };
        operations
            .into_iter()
            .map(|operation| AdapterOperation {
                category: "image".into(),
                operation,
            })
            .collect()
    }
}

struct FixturePlugin(Arc<FixtureAdapter>);
impl Plugin for FixturePlugin {
    type Deps = ();
    fn name(&self) -> &str {
        "ModalityFixture"
    }
    async fn apply(&self, ctx: &Ctx, (): ()) -> rutis::Result {
        ctx.provide(ModalityAdapterService(self.0.clone()))?;
        Ok(())
    }
}

const PROVIDER: &str = "018ec73f-0000-7000-8000-0000000000aa";

fn home(prefix: &str) -> std::path::PathBuf {
    let home = std::path::PathBuf::from("/tmp").join(format!("{prefix}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&home).unwrap();
    home
}

async fn create_provider(client: &mut Client) {
    let mut vision = CatalogCapabilities::with_limits(128_000, 16_384);
    vision.vision = true;
    let text = CatalogCapabilities::with_limits(128_000, 16_384);
    assert_ok(
        &client
            .rpc(
                "provider.create",
                json!({
                    "providerId": PROVIDER,
                    "displayName": "Fixture",
                    "connection": {"driverKind": "openai-compatible", "baseUrl": "http://127.0.0.1:1/v1", "auth": {"type": "anonymous"}},
                    "models": [
                        {"modelId": "alpha", "displayName": "alpha", "status": "active", "capabilities": vision},
                        {"modelId": "beta", "displayName": "beta", "status": "active", "capabilities": text}
                    ],
                    "enabled": true
                }),
            )
            .await,
    );
}
fn model(model_id: &str) -> Value {
    json!({"providerId": PROVIDER, "modelId": model_id})
}
fn operation<'a>(snapshot: &'a Value, operation: &str) -> &'a Value {
    operation_in(snapshot, "image", operation)
}
fn operation_in<'a>(snapshot: &'a Value, category: &str, operation: &str) -> &'a Value {
    snapshot["result"]["categories"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["category"].as_str() == Some(category))
        .unwrap()["operations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["operation"].as_str() == Some(operation))
        .unwrap()
}

/// One expected A.1 operation and its Settings key.
type ExpectedOperation = (&'static str, Option<&'static str>);

fn assert_operations(categories: &[Value], category: &str, expected: &[ExpectedOperation]) {
    let entry = categories
        .iter()
        .find(|entry| entry["category"].as_str() == Some(category))
        .unwrap();
    let operations: Vec<(String, Option<String>)> = entry["operations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|operation| {
            (
                operation["operation"].as_str().unwrap().to_owned(),
                operation["key"].as_str().map(str::to_owned),
            )
        })
        .collect();
    let wanted: Vec<(String, Option<String>)> = expected
        .iter()
        .map(|(operation, key)| ((*operation).to_owned(), key.map(str::to_owned)))
        .collect();
    assert_eq!(operations, wanted, "{category}");
}

/// A non-chat media model that declares modality operations.
fn media(model_id: &str, operations: &[&str]) -> Value {
    json!({
        "modelId": model_id, "displayName": model_id, "status": "active",
        "capabilities": {"modalities":["image"],"toolCalling":false,"streaming":true,
            "structuredOutput":false,"vision":false,"audio":false,"reasoning":false},
        "metadata": {"raw": {"type":"image-generation", "modalityCapabilities": operations}}
    })
}
/// A chat-selectable model that also declares modality operations.
fn chat(model_id: &str, operations: &[&str]) -> Value {
    let capabilities = CatalogCapabilities::with_limits(128_000, 16_384);
    json!({
        "modelId": model_id, "displayName": model_id, "status": "active",
        "capabilities": capabilities,
        "metadata": {"raw": {"modalityCapabilities": operations}}
    })
}
async fn create_provider_with(client: &mut Client, models: Vec<Value>) {
    assert_ok(
        &client
            .rpc(
                "provider.create",
                json!({
                    "providerId": PROVIDER,
                    "displayName": "Fixture",
                    "connection": {"driverKind": "openai-compatible", "baseUrl": "http://127.0.0.1:1/v1", "auth": {"type": "anonymous"}},
                    "models": models,
                    "enabled": true
                }),
            )
            .await,
    );
}

async fn start(
    fixture: Option<Arc<FixtureAdapter>>,
    prefix: &str,
) -> (RunningServer, std::path::PathBuf) {
    let home = home(prefix);
    let server = start_in(home.clone(), fixture).await;
    (server, home)
}

async fn start_in(home: std::path::PathBuf, fixture: Option<Arc<FixtureAdapter>>) -> RunningServer {
    let config = ServerConfig {
        product_home: Some(home.clone()),
        target_home: Some(home.join("native")),
        ..Default::default()
    };
    launch_with(Arc::new(MockTools), config, |builder| {
        if let Some(fixture) = fixture.clone() {
            builder.plugin(move |_| Ok(Box::new(FixturePlugin(fixture.clone()))));
        }
    })
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn capabilities_and_configure_resolve_defaults_through_the_fixture_adapter() {
    let fixture = Arc::new(FixtureAdapter::default());
    let (server, home) = start(Some(fixture.clone()), "dm").await;
    let mut client = Client::new_version(&server, false, 3).await;
    create_provider(&mut client).await;
    assert_ok(
        &client
            .rpc(
                "provider.select",
                json!({"providerId": PROVIDER, "modelId": "beta"}),
            )
            .await,
    );

    let initial = client.rpc("modality.capabilities", json!({})).await;
    assert_ok(&initial);
    let categories = initial["result"]["categories"].as_array().unwrap();
    assert_eq!(categories.len(), 9);
    assert!(
        categories
            .iter()
            .all(|category| category["category"] != "video" || category["available"] == false)
    );
    let video = categories
        .iter()
        .find(|category| category["category"] == "video")
        .unwrap();
    assert_eq!(video["reasons"][0]["dimension"], "build");
    assert!(
        initial["result"]["revision"]
            .as_str()
            .unwrap()
            .contains("modality")
    );

    let generate = operation(&initial, "generate");
    assert_eq!(generate["available"], true);
    assert_eq!(generate["key"], "modality.image.generate");
    assert!(generate["default"].is_null());
    assert_eq!(
        generate["options"],
        json!([
            {"providerId": PROVIDER, "modelId": "alpha", "available": true},
            {"providerId": PROVIDER, "modelId": "beta", "available": true}
        ])
    );
    assert_eq!(
        generate["effective"],
        json!({"providerId": PROVIDER, "modelId": "beta", "source": "automatic"})
    );
    let understand = operation(&initial, "understand");
    assert_eq!(understand["available"], true);
    assert_eq!(
        understand["options"],
        json!([{"providerId": PROVIDER, "modelId": "alpha", "available": true}])
    );
    assert_eq!(
        understand["effective"],
        json!({"providerId": PROVIDER, "modelId": "alpha", "source": "automatic"})
    );

    let revision = initial["result"]["revision"].as_str().unwrap().to_owned();
    let configured = client
        .rpc(
            "modality.configure",
            json!({"expectedRevision": revision, "category": "image", "operation": "generate", "model": model("alpha")}),
        )
        .await;
    assert_ok(&configured);
    let generate = operation(&configured, "generate");
    assert_eq!(
        generate["default"],
        json!({"providerId": PROVIDER, "modelId": "alpha", "available": true})
    );
    assert_eq!(
        generate["effective"],
        json!({"providerId": PROVIDER, "modelId": "alpha", "source": "default"})
    );
    assert_eq!(
        operation(&configured, "edit")["effective"],
        json!({"providerId": PROVIDER, "modelId": "beta", "source": "automatic"}),
        "configuring one operation leaves the others on automatic"
    );
    let reread = client.rpc("modality.capabilities", json!({})).await;
    assert_eq!(
        configured["result"], reread["result"],
        "reply equals the next read"
    );
    let edit = client
        .rpc(
            "modality.configure",
            json!({"expectedRevision": reread["result"]["revision"], "category": "image", "operation": "edit", "model": model("beta")}),
        )
        .await;
    assert_ok(&edit);
    assert_eq!(
        operation(&edit, "edit")["effective"],
        json!({"providerId": PROVIDER, "modelId": "beta", "source": "default"})
    );

    let conflict = client
        .rpc(
            "modality.configure",
            json!({"expectedRevision": revision, "category": "image", "operation": "generate", "model": model("beta")}),
        )
        .await;
    assert_eq!(conflict["error"]["data"]["appError"], "modality_conflict");
    let stale = client.rpc("modality.capabilities", json!({})).await;
    assert_eq!(
        operation(&stale, "generate")["default"]["modelId"],
        "alpha",
        "a conflicted write changes nothing"
    );

    let unseen = client
        .rpc(
            "modality.configure",
            json!({"expectedRevision": stale["result"]["revision"], "category": "image", "operation": "generate", "model": model("gamma")}),
        )
        .await;
    assert_eq!(
        unseen["error"]["data"]["appError"], "modality_invalid_selection",
        "an unselectable model is rejected"
    );
    let not_vision = client
        .rpc(
            "modality.configure",
            json!({"expectedRevision": stale["result"]["revision"], "category": "image", "operation": "understand", "model": model("beta")}),
        )
        .await;
    assert_eq!(
        not_vision["error"]["data"]["appError"], "modality_invalid_selection",
        "understanding accepts only selectable vision models"
    );
    let unknown = client
        .rpc(
            "modality.configure",
            json!({"expectedRevision": stale["result"]["revision"], "category": "image", "operation": "video", "model": null}),
        )
        .await;
    assert_eq!(
        unknown["error"]["data"]["appError"],
        "modality_unknown_operation"
    );
    let other = client
        .rpc(
            "modality.configure",
            json!({"expectedRevision": stale["result"]["revision"], "category": "video", "operation": "generate", "model": model("alpha")}),
        )
        .await;
    assert_eq!(
        other["error"]["data"]["appError"], "modality_invalid_selection",
        "configuring is a settings write; a model outside the video options is refused"
    );
    let missing_model = client
        .rpc(
            "modality.configure",
            json!({"expectedRevision": stale["result"]["revision"], "category": "image", "operation": "generate"}),
        )
        .await;
    assert_eq!(
        missing_model["error"]["data"]["appError"], "invalid_params",
        "an absent model is malformed; only null clears"
    );

    let catalog = client
        .rpc("provider.catalog", json!({"providerId": PROVIDER}))
        .await;
    assert_ok(&catalog);
    let version = catalog["result"]["catalog"]["version"].as_u64().unwrap();
    assert_ok(
        &client
            .rpc(
                "provider.models.set_enabled",
                json!({"providerId": PROVIDER, "expectedVersion": version, "modelIds": ["beta"]}),
            )
            .await,
    );
    let removed = client.rpc("modality.capabilities", json!({})).await;
    let generate = operation(&removed, "generate");
    assert_eq!(generate["default"]["available"], false, "{generate}");
    assert!(
        generate["default"]["reason"]["detail"]
            .as_str()
            .unwrap()
            .contains("no longer selectable")
    );
    assert!(
        generate["effective"].is_null(),
        "an unavailable default never substitutes another model"
    );

    let cleared = client
        .rpc(
            "modality.configure",
            json!({"expectedRevision": removed["result"]["revision"], "category": "image", "operation": "generate", "model": null}),
        )
        .await;
    assert_ok(&cleared);
    let generate = operation(&cleared, "generate");
    assert!(generate["default"].is_null());
    assert_eq!(generate["effective"]["source"], "automatic");

    let edit_only = client
        .rpc(
            "modality.configure",
            json!({"expectedRevision": cleared["result"]["revision"], "category": "image", "operation": "edit", "model": model("alpha")}),
        )
        .await;
    assert_eq!(
        edit_only["error"]["data"]["appError"], "modality_invalid_selection",
        "a disabled model is not a valid default"
    );
    fixture.serve(&[("beta", &["generate"])]);
    let beta_edit = client
        .rpc(
            "modality.configure",
            json!({"expectedRevision": cleared["result"]["revision"], "category": "image", "operation": "edit", "model": model("beta")}),
        )
        .await;
    assert_eq!(
        beta_edit["error"]["data"]["appError"], "modality_invalid_selection",
        "the adapter serves edit only for the models it declares"
    );
    let beta_generate = client
        .rpc(
            "modality.configure",
            json!({"expectedRevision": cleared["result"]["revision"], "category": "image", "operation": "generate", "model": model("beta")}),
        )
        .await;
    assert_ok(&beta_generate);
    assert_eq!(
        operation(&beta_generate, "generate")["effective"]["modelId"],
        "beta",
        "selecting a nondefault model changes the model a task resolves"
    );
    assert!(
        !operation(&beta_generate, "edit")["options"]
            .as_array()
            .unwrap()
            .iter()
            .any(|option| option["available"] == true),
        "the adapter serves edit only for the models it declares"
    );
    let unserved = client.rpc("modality.capabilities", json!({})).await;
    let edit = operation(&unserved, "edit");
    assert_eq!(
        edit["default"]["available"], false,
        "a selectable default that no adapter serves is unavailable: {edit}"
    );
    assert_eq!(edit["default"]["reason"]["dimension"], "adapter");
    assert!(edit["effective"].is_null());

    fixture.set_available(false);
    let offline = client.rpc("modality.capabilities", json!({})).await;
    let generate = operation(&offline, "generate");
    assert_eq!(generate["available"], false);
    assert_eq!(generate["reasons"][0]["dimension"], "runtime");
    let refused = client
        .rpc(
            "modality.configure",
            json!({"expectedRevision": offline["result"]["revision"], "category": "image", "operation": "generate", "model": model("beta")}),
        )
        .await;
    assert_eq!(
        refused["error"]["data"]["appError"], "modality_invalid_selection",
        "an unavailable adapter cannot serve a new default"
    );

    let mut bare = Client::new_version(&server, true, 3).await;
    for method in ["modality.capabilities", "modality.configure"] {
        assert_eq!(
            bare.rpc(method, json!({})).await["error"]["data"]["appError"],
            "local_transport_required",
            "{method}"
        );
    }

    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// The new Provider read exists for the media models execution rejects: when
/// an adapter serves one, the operation offers it instead of reporting that
/// no selectable model is served.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_declared_media_model_served_by_the_adapter_is_an_option() {
    let fixture = Arc::new(FixtureAdapter::default());
    fixture.serve(&[("image-media", &["generate"])]);
    let (server, home) = start(Some(fixture), "dm-declared-adapter").await;
    let mut client = Client::new_version(&server, false, 3).await;
    create_provider_with(&mut client, vec![media("image-media", &["image.generate"])]).await;
    let snapshot = client.rpc("modality.capabilities", json!({})).await;
    assert_ok(&snapshot);
    let generate = operation(&snapshot, "generate");
    assert_eq!(
        generate["options"],
        json!([{"providerId": PROVIDER, "modelId": "image-media", "available": true}]),
        "a declared model the adapter serves is an option without execution selectability: {generate}"
    );
    assert_eq!(generate["available"], true);
    assert_eq!(
        generate["effective"],
        json!({"providerId": PROVIDER, "modelId": "image-media", "source": "automatic"})
    );
    let understand = operation(&snapshot, "understand");
    assert!(
        understand["options"].as_array().unwrap().is_empty(),
        "the model is not in the execution read understanding uses: {understand}"
    );
    let configured = client
        .rpc(
            "modality.configure",
            json!({"expectedRevision": snapshot["result"]["revision"], "category": "image", "operation": "generate", "model": model("image-media")}),
        )
        .await;
    assert_ok(&configured);
    assert_eq!(
        operation(&configured, "generate")["effective"],
        json!({"providerId": PROVIDER, "modelId": "image-media", "source": "default"})
    );
    let edit = operation(&snapshot, "edit");
    assert_eq!(edit["available"], false);
    assert_eq!(
        edit["reasons"][0]["detail"], "No selectable model is served by an available adapter",
        "the adapter narrows the candidates per operation"
    );
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// An adapter that serves no model either Provider read names is not the same
/// as a build without an adapter: the operation says which case it is.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_adapter_serving_no_provider_model_is_not_reported_as_missing() {
    let fixture = Arc::new(FixtureAdapter::default());
    fixture.serve(&[("unknown-model", &["generate"])]);
    let (server, home) = start(Some(fixture), "dm-adapter-idle").await;
    let mut client = Client::new_version(&server, false, 3).await;
    create_provider_with(&mut client, vec![media("image-media", &["image.generate"])]).await;
    let snapshot = client.rpc("modality.capabilities", json!({})).await;
    assert_ok(&snapshot);
    let generate = operation(&snapshot, "generate");
    assert_eq!(generate["available"], false);
    assert_eq!(generate["reasons"][0]["dimension"], "adapter");
    assert_eq!(
        generate["reasons"][0]["detail"], "The adapter serves no image model Provider lists",
        "a collected adapter that serves no candidate is not a missing adapter: {generate}"
    );
    assert_eq!(
        generate["options"],
        json!([{"providerId": PROVIDER, "modelId": "image-media", "available": true}]),
        "an operation no adapter serves falls back to the declared models"
    );
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_product_build_reports_image_operations_without_an_adapter() {
    let (server, home) = start(None, "dm-product").await;
    let mut client = Client::new_version(&server, false, 3).await;
    create_provider(&mut client).await;
    let snapshot = client.rpc("modality.capabilities", json!({})).await;
    assert_ok(&snapshot);
    let categories = snapshot["result"]["categories"].as_array().unwrap();
    assert_eq!(categories.len(), 9, "every category is present");
    for category in categories
        .iter()
        .filter(|category| category["category"] != "image")
    {
        assert_eq!(category["available"], false, "{category}");
        assert_eq!(category["reasons"][0]["dimension"], "build", "{category}");
        assert!(
            category["operations"]
                .as_array()
                .unwrap()
                .iter()
                .all(|operation| operation["reasons"][0]["dimension"] == "build"),
            "{category}"
        );
    }
    for name in ["generate", "edit", "enhance"] {
        let entry = operation(&snapshot, name);
        assert_eq!(entry["available"], false, "{entry}");
        assert_eq!(entry["reasons"][0]["dimension"], "adapter");
        assert!(entry["options"].as_array().unwrap().is_empty());
        assert!(entry["default"].is_null());
        assert!(entry["effective"].is_null());
        let refused = client
            .rpc(
                "modality.configure",
                json!({"expectedRevision": snapshot["result"]["revision"], "category": "image", "operation": name, "model": model("alpha")}),
            )
            .await;
        assert_eq!(
            refused["error"]["data"]["appError"], "modality_invalid_selection",
            "{name} cannot have a default without an adapter: {refused}"
        );
    }
    let understand = operation(&snapshot, "understand");
    assert_eq!(
        understand["available"], true,
        "understanding resolves a selectable vision model"
    );
    assert_eq!(
        understand["options"],
        json!([{"providerId": PROVIDER, "modelId": "alpha", "available": true}])
    );
    assert_eq!(
        understand["effective"],
        json!({"providerId": PROVIDER, "modelId": "alpha", "source": "automatic"})
    );
    let configured = client
        .rpc(
            "modality.configure",
            json!({"expectedRevision": snapshot["result"]["revision"], "category": "image", "operation": "understand", "model": model("alpha")}),
        )
        .await;
    assert_ok(&configured);
    assert_eq!(
        operation(&configured, "understand")["default"],
        json!({"providerId": PROVIDER, "modelId": "alpha", "available": true})
    );
    assert_eq!(
        operation(&configured, "understand")["effective"]["source"],
        "default"
    );
    let reread = client.rpc("modality.capabilities", json!({})).await;
    assert_eq!(configured["result"], reread["result"]);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn clearing_an_operation_after_all_defaults_matches_the_next_read() {
    let fixture = Arc::new(FixtureAdapter::default());
    let (server, home) = start(Some(fixture), "dm-clear").await;
    let mut client = Client::new_version(&server, false, 3).await;
    let mut vision = CatalogCapabilities::with_limits(128_000, 16_384);
    vision.vision = true;
    assert_ok(
        &client
            .rpc(
                "provider.create",
                json!({
                    "providerId": PROVIDER,
                    "displayName": "Fixture",
                    "connection": {"driverKind": "openai-compatible", "baseUrl": "http://127.0.0.1:1/v1", "auth": {"type": "anonymous"}},
                    "models": [
                        {"modelId": "alpha", "displayName": "alpha", "status": "active", "capabilities": vision},
                        {"modelId": "gamma", "displayName": "gamma", "status": "active", "capabilities": vision}
                    ],
                    "enabled": true
                }),
            )
            .await,
    );
    assert_ok(
        &client
            .rpc(
                "provider.select",
                json!({"providerId": PROVIDER, "modelId": "gamma"}),
            )
            .await,
    );
    let initial = client.rpc("modality.capabilities", json!({})).await;
    assert_ok(&initial);
    let mut revision = initial["result"]["revision"].as_str().unwrap().to_owned();
    for operation in ["generate", "edit", "enhance", "understand"] {
        let configured = client
            .rpc(
                "modality.configure",
                json!({"expectedRevision": revision, "category": "image", "operation": operation, "model": model("alpha")}),
            )
            .await;
        assert_ok(&configured);
        revision = configured["result"]["revision"]
            .as_str()
            .unwrap()
            .to_owned();
    }
    let cleared = client
        .rpc(
            "modality.configure",
            json!({"expectedRevision": revision, "category": "image", "operation": "understand", "model": null}),
        )
        .await;
    assert_ok(&cleared);
    assert_eq!(
        operation(&cleared, "understand")["effective"],
        json!({"providerId": PROVIDER, "modelId": "gamma", "source": "automatic"}),
        "the reply uses the global selection that serves the cleared operation"
    );
    let next = client.rpc("modality.capabilities", json!({})).await;
    assert_eq!(
        cleared["result"], next["result"],
        "reply equals the next read"
    );
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_configured_default_survives_a_restart() {
    let home = home("dm-reload");
    let fixture = Arc::new(FixtureAdapter::default());
    let server = start_in(home.clone(), Some(fixture.clone())).await;
    let mut client = Client::new_version(&server, false, 3).await;
    create_provider(&mut client).await;
    assert_ok(
        &client
            .rpc(
                "provider.select",
                json!({"providerId": PROVIDER, "modelId": "beta"}),
            )
            .await,
    );
    let initial = client.rpc("modality.capabilities", json!({})).await;
    assert_ok(&initial);
    let saved = client
        .rpc(
            "modality.configure",
            json!({"expectedRevision": initial["result"]["revision"], "category": "image", "operation": "generate", "model": model("alpha")}),
        )
        .await;
    assert_ok(&saved);
    let revision = saved["result"]["revision"].as_str().unwrap().to_owned();
    server.shutdown();
    server.join().await.unwrap();

    let server = start_in(home.clone(), Some(fixture)).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let restored = client.rpc("modality.capabilities", json!({})).await;
    assert_ok(&restored);
    assert_eq!(
        operation(&restored, "generate")["default"]["modelId"],
        "alpha",
        "the persisted default is read from the modality settings document"
    );
    assert_eq!(restored["result"]["revision"], revision);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_a1_category_and_operation_is_reported_with_its_default_key() {
    let (server, home) = start(None, "dm-catalog").await;
    let mut client = Client::new_version(&server, false, 3).await;
    let snapshot = client.rpc("modality.capabilities", json!({})).await;
    assert_ok(&snapshot);
    let categories = snapshot["result"]["categories"].as_array().unwrap();
    let names: Vec<Option<&str>> = categories
        .iter()
        .map(|category| category["category"].as_str())
        .collect();
    assert_eq!(
        names,
        [
            Some("image"),
            Some("video"),
            Some("speech"),
            Some("music"),
            Some("audio"),
            Some("asset"),
            Some("motion"),
            Some("realtime"),
            Some("ocr"),
        ]
    );
    assert_operations(
        categories,
        "image",
        &[
            ("generate", Some("modality.image.generate")),
            ("edit", Some("modality.image.edit")),
            ("enhance", Some("modality.image.enhance")),
            ("understand", Some("modality.image.understand")),
        ],
    );
    assert_operations(
        categories,
        "video",
        &[
            ("generate", Some("modality.video")),
            ("edit", Some("modality.video")),
            ("enhance", Some("modality.video")),
        ],
    );
    assert_operations(
        categories,
        "speech",
        &[
            ("synthesize", Some("modality.speech.synthesize")),
            ("transcribe", Some("modality.speech.transcribe")),
        ],
    );
    for (category, operation, key) in [
        ("music", "generate", "modality.music"),
        ("audio", "generate", "modality.audio"),
        ("asset", "generate", "modality.asset"),
        ("motion", "generate", "modality.motion"),
        ("realtime", "connect", "modality.realtime"),
    ] {
        assert_operations(categories, category, &[(operation, Some(key))]);
    }
    assert_operations(categories, "ocr", &[("recognize", None)]);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn declared_models_are_the_options_of_an_operation_without_an_adapter() {
    let (server, home) = start(None, "dm-declared").await;
    let mut client = Client::new_version(&server, false, 3).await;
    create_provider_with(
        &mut client,
        vec![
            media("video-edit", &["video.edit"]),
            media("video-generate", &["video.generate", "video.edit"]),
            chat("text-model", &[]),
        ],
    )
    .await;
    assert_ok(
        &client
            .rpc(
                "provider.select",
                json!({"providerId": PROVIDER, "modelId": "text-model"}),
            )
            .await,
    );
    let snapshot = client.rpc("modality.capabilities", json!({})).await;
    assert_ok(&snapshot);
    let generate = operation_in(&snapshot, "video", "generate");
    assert_eq!(generate["available"], false);
    assert_eq!(generate["reasons"][0]["dimension"], "build");
    assert_eq!(
        generate["options"],
        json!([{"providerId": PROVIDER, "modelId": "video-generate", "available": true}]),
        "only a declared model is an option, in Provider's order"
    );
    assert_eq!(
        generate["effective"],
        json!({"providerId": PROVIDER, "modelId": "video-generate", "source": "automatic"}),
        "the global selection does not serve video, so the first declared option resolves"
    );
    let edit = operation_in(&snapshot, "video", "edit");
    assert_eq!(
        edit["options"],
        json!([
            {"providerId": PROVIDER, "modelId": "video-edit", "available": true},
            {"providerId": PROVIDER, "modelId": "video-generate", "available": true}
        ])
    );
    let enhance = operation_in(&snapshot, "video", "enhance");
    assert!(enhance["options"].as_array().unwrap().is_empty());
    assert!(enhance["effective"].is_null());
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn defaults_for_category_keyed_and_per_operation_keys_survive_a_restart() {
    let home = home("dm-keys");
    let server = start_in(home.clone(), None).await;
    let mut client = Client::new_version(&server, false, 3).await;
    create_provider_with(
        &mut client,
        vec![
            media("video-a", &["video.generate"]),
            media("speech-a", &["speech.synthesize", "speech.transcribe"]),
        ],
    )
    .await;
    let initial = client.rpc("modality.capabilities", json!({})).await;
    assert_ok(&initial);
    let video = client
        .rpc(
            "modality.configure",
            json!({"expectedRevision": initial["result"]["revision"], "category": "video", "model": model("video-a")}),
        )
        .await;
    assert_ok(&video);
    for operation in ["generate", "edit", "enhance"] {
        assert_eq!(
            operation_in(&video, "video", operation)["default"]["modelId"],
            "video-a",
            "the category key governs every video operation"
        );
    }
    let speech = client
        .rpc(
            "modality.configure",
            json!({"expectedRevision": video["result"]["revision"], "category": "speech", "operation": "synthesize", "model": model("speech-a")}),
        )
        .await;
    assert_ok(&speech);
    assert_eq!(
        operation_in(&speech, "speech", "synthesize")["default"]["modelId"],
        "speech-a"
    );
    assert!(
        operation_in(&speech, "speech", "transcribe")["default"].is_null(),
        "speech keys each operation"
    );
    let conflict = client
        .rpc(
            "modality.configure",
            json!({"expectedRevision": initial["result"]["revision"], "category": "video", "model": model("video-a")}),
        )
        .await;
    assert_eq!(
        conflict["error"]["data"]["appError"], "modality_conflict",
        "a stale revision writes nothing"
    );
    let outside = client
        .rpc(
            "modality.configure",
            json!({"expectedRevision": speech["result"]["revision"], "category": "video", "model": model("speech-a")}),
        )
        .await;
    assert_eq!(
        outside["error"]["data"]["appError"],
        "modality_invalid_selection"
    );
    assert!(
        outside["error"]["message"]
            .as_str()
            .unwrap()
            .contains("video"),
        "the refusal names the operation: {outside}"
    );
    let revision = speech["result"]["revision"].as_str().unwrap().to_owned();
    server.shutdown();
    server.join().await.unwrap();

    let server = start_in(home.clone(), None).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let restored = client.rpc("modality.capabilities", json!({})).await;
    assert_ok(&restored);
    assert_eq!(restored["result"]["revision"], revision);
    for operation in ["generate", "edit", "enhance"] {
        assert_eq!(
            operation_in(&restored, "video", operation)["default"]["modelId"],
            "video-a",
            "{operation}"
        );
    }
    assert_eq!(
        operation_in(&restored, "speech", "synthesize")["default"]["modelId"],
        "speech-a"
    );
    assert!(operation_in(&restored, "speech", "transcribe")["default"].is_null());
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// A model that declares only a later operation of a category-keyed category
/// is a valid default, so re-saving the category without `operation` keeps it
/// even though the earlier rows carry the persisted default as unavailable.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_category_wide_resave_accepts_the_default_of_its_last_operation() {
    let (server, home) = start(None, "dm-category-resave").await;
    let mut client = Client::new_version(&server, false, 3).await;
    create_provider_with(&mut client, vec![media("video-edit", &["video.edit"])]).await;
    let initial = client.rpc("modality.capabilities", json!({})).await;
    assert_ok(&initial);
    let edited = client
        .rpc(
            "modality.configure",
            json!({"expectedRevision": initial["result"]["revision"], "category": "video", "operation": "edit", "model": model("video-edit")}),
        )
        .await;
    assert_ok(&edited);
    let resaved = client
        .rpc(
            "modality.configure",
            json!({"expectedRevision": edited["result"]["revision"], "category": "video", "model": model("video-edit")}),
        )
        .await;
    assert_ok(&resaved);
    for operation in ["generate", "edit", "enhance"] {
        assert_eq!(
            operation_in(&resaved, "video", operation)["default"]["modelId"],
            "video-edit",
            "the category-wide save keeps the stored default: {operation}"
        );
    }
    let next = client.rpc("modality.capabilities", json!({})).await;
    assert_eq!(
        resaved["result"], next["result"],
        "reply equals the next read"
    );
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn operations_without_a_default_key_or_unknown_answers_are_unknown() {
    let (server, home) = start(None, "dm-unknown").await;
    let mut client = Client::new_version(&server, false, 3).await;
    let snapshot = client.rpc("modality.capabilities", json!({})).await;
    assert_ok(&snapshot);
    let revision = snapshot["result"]["revision"].clone();
    for params in [
        json!({"expectedRevision": revision, "category": "ocr", "operation": "recognize", "model": null}),
        json!({"expectedRevision": revision, "category": "ocr", "model": null}),
        json!({"expectedRevision": revision, "category": "video", "operation": "unknown", "model": null}),
        json!({"expectedRevision": revision, "category": "image", "model": null}),
        json!({"expectedRevision": revision, "category": "unknown", "operation": "generate", "model": null}),
    ] {
        let reply = client.rpc("modality.configure", params.clone()).await;
        assert_eq!(
            reply["error"]["data"]["appError"], "modality_unknown_operation",
            "{params}"
        );
    }
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn effective_resolves_over_declared_options_and_stays_null_for_an_unavailable_default() {
    let (server, home) = start(None, "dm-effective").await;
    let mut client = Client::new_version(&server, false, 3).await;
    create_provider_with(
        &mut client,
        vec![
            media("media-option", &["video.generate"]),
            chat("chat-option", &["video.generate"]),
        ],
    )
    .await;
    assert_ok(
        &client
            .rpc(
                "provider.select",
                json!({"providerId": PROVIDER, "modelId": "chat-option"}),
            )
            .await,
    );
    let initial = client.rpc("modality.capabilities", json!({})).await;
    assert_ok(&initial);
    let generate = operation_in(&initial, "video", "generate");
    assert!(generate["default"].is_null());
    assert_eq!(
        generate["effective"],
        json!({"providerId": PROVIDER, "modelId": "chat-option", "source": "automatic"}),
        "automatic resolution prefers the global selection when it serves the operation"
    );
    let configured = client
        .rpc(
            "modality.configure",
            json!({"expectedRevision": initial["result"]["revision"], "category": "video", "operation": "generate", "model": model("chat-option")}),
        )
        .await;
    assert_ok(&configured);
    assert_eq!(
        operation_in(&configured, "video", "generate")["effective"]["source"],
        "default"
    );
    let catalog = client
        .rpc("provider.catalog", json!({"providerId": PROVIDER}))
        .await;
    assert_ok(&catalog);
    assert_ok(
        &client
            .rpc(
                "provider.models.set_enabled",
                json!({"providerId": PROVIDER, "expectedVersion": catalog["result"]["catalog"]["version"], "modelIds": []}),
            )
            .await,
    );
    let disabled = client.rpc("modality.capabilities", json!({})).await;
    assert_ok(&disabled);
    let generate = operation_in(&disabled, "video", "generate");
    assert_eq!(generate["default"]["available"], false, "{generate}");
    assert_eq!(generate["default"]["reason"]["dimension"], "provider");
    assert!(
        generate["effective"].is_null(),
        "an unavailable default never substitutes another model"
    );
    assert_eq!(
        generate["options"],
        json!([
            {"providerId": PROVIDER, "modelId": "media-option", "available": true},
            {"providerId": PROVIDER, "modelId": "chat-option", "available": false,
                "reason": {"dimension": "provider", "detail": "Model is no longer selectable"}}
        ]),
        "the declared media model stays; the persisted default follows as unavailable"
    );
    let cleared = client
        .rpc(
            "modality.configure",
            json!({"expectedRevision": disabled["result"]["revision"], "category": "video", "operation": "generate", "model": null}),
        )
        .await;
    assert_ok(&cleared);
    assert_eq!(
        operation_in(&cleared, "video", "generate")["effective"],
        json!({"providerId": PROVIDER, "modelId": "media-option", "source": "automatic"})
    );
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

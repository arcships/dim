//! Modality image slice through the management methods: the owner's snapshot,
//! resolution order and CAS, with a fixture adapter in the process.
use super::*;
use dim_agent_types::{CatalogCapabilities, ModelId};
use dim_app_server::modality::{AdapterOperation, ModalityAdapter, ModalityAdapterService};
use rutis::Ctx;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

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
    snapshot["result"]["categories"]
        .as_array()
        .unwrap()
        .iter()
        .find(|category| category["category"] == "image")
        .unwrap()["operations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["operation"] == operation)
        .unwrap()
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
    .0
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
    assert_eq!(other["error"]["data"]["appError"], "modality_unavailable");
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

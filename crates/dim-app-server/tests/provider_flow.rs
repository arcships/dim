use axum::{
    Json, Router,
    extract::State,
    http::HeaderMap,
    response::sse::{Event, Sse},
    routing::post,
};
use dim_agent_capabilities::{
    file_storage::FileStoragePlugin,
    host::{
        AuxiliaryErrorCode, AuxiliaryModelHost, AuxiliaryRequest, AwsCredentials, BedrockAuth,
        ModelChunk, ModelConnection, ModelHost, ModelStopReason, ProviderConnectionResolver,
    },
};
use dim_agent_runtime::{Names, names::ServiceCatalog};
use dim_agent_types::{Message, ModelId, ModelRequest, Role};
use dim_app_server::{
    managed_provider::ManagedProvidersPlugin,
    model_catalog::ModelEntry,
    provider::{
        ConnectionInput, CreateProvider, CredentialUpdate, ModelPreferences, ProviderError,
        ProviderPlugin, ProviderService, StaticAuth,
    },
    storage::SqliteStoragePlugin,
};
use futures_util::{StreamExt, stream};
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, ErrorKind, FiberState, Key, TypeKey};
use rutis::{Ctx, Plugin};
use rutis_loader::resolve::Build;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, LazyLock, Mutex, OnceLock},
};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug)]
struct Seen {
    path: String,
    authorization: Option<String>,
    api_key: Option<String>,
    marker: Option<String>,
    app_code: Option<String>,
    session_id: Option<String>,
    body: Value,
}

async fn completion(
    State(seen): State<Arc<Mutex<Vec<Seen>>>>,
    uri: axum::http::Uri,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Sse<impl futures_util::Stream<Item = Result<Event, std::convert::Infallible>>> {
    seen.lock().unwrap().push(Seen {
        path: uri.path().into(),
        authorization: headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned),
        api_key: headers
            .get("api-key")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned),
        marker: headers
            .get("x-marker")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned),
        app_code: headers
            .get("app-code")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned),
        session_id: headers
            .get("x-opencode-session")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned),
        body: body.clone(),
    });
    let frames = if uri.path().ends_with("/responses") {
        vec![
            json!({"type":"response.created","response":{"id":"resp-1","created_at":1,"model":body["model"]}}),
            json!({"type":"response.output_item.added","output_index":0,"item":{"type":"message","id":"msg-1","role":"assistant","content":[]}}),
            json!({"type":"response.content_part.added","item_id":"msg-1","output_index":0,"content_index":0,"part":{"type":"output_text","text":"","annotations":[]}}),
            json!({"type":"response.output_text.delta","item_id":"msg-1","output_index":0,"content_index":0,"delta":"ok"}),
            json!({"type":"response.output_item.done","output_index":0,"item":{"type":"message","id":"msg-1","role":"assistant","content":[{"type":"output_text","text":"ok","annotations":[]}]}}),
            json!({"type":"response.completed","response":{"id":"resp-1","status":"completed","usage":{"input_tokens":7,"output_tokens":3,"total_tokens":10}}}),
        ]
    } else if uri.path().contains(":streamGenerateContent") {
        vec![
            json!({"candidates":[{"content":{"role":"model","parts":[{"text":"ok"}]},"finishReason":"STOP","index":0}],"usageMetadata":{"promptTokenCount":7,"candidatesTokenCount":3,"totalTokenCount":10}}),
        ]
    } else if uri.path().ends_with("/messages") {
        vec![
            json!({"type":"message_start","message":{"id":"msg-1","type":"message","role":"assistant","model":body["model"],"content":[],"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":7,"output_tokens":0}}}),
            json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
            json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"ok"}}),
            json!({"type":"content_block_stop","index":0}),
            json!({"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":3}}),
            json!({"type":"message_stop"}),
        ]
    } else {
        vec![
            json!({"id":"x","object":"chat.completion.chunk","created":0,"model":body["model"],"choices":[{"index":0,"delta":{"content":"ok"},"finish_reason":"stop"}]}),
        ]
    };
    Sse::new(stream::iter(frames.into_iter().map(|frame| {
        let event = Event::default().data(frame.to_string());
        Ok(match frame["type"].as_str() {
            Some(kind) => event.event(kind),
            None => event,
        })
    })))
}

struct Capture {
    captured: Arc<OnceLock<(Arc<ProviderService>, Arc<ProviderConnectionResolver>)>>,
    provider: Key<ProviderService>,
    resolver: Key<ProviderConnectionResolver>,
}
impl Capture {
    fn new(
        names: &Names<'_>,
        captured: Arc<OnceLock<(Arc<ProviderService>, Arc<ProviderConnectionResolver>)>>,
    ) -> rutis::Result<Self> {
        Ok(Self {
            captured,
            provider: names.key()?,
            resolver: names.key()?,
        })
    }
}
impl DynPlugin for Capture {
    fn name(&self) -> &str {
        "test.provider.capture"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.provider.erase(), self.resolver.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let provider = ctx.require_at(&self.provider)?;
            let resolver = ctx.require_at(&self.resolver)?;
            self.captured.set((provider, resolver)).map_err(|_| {
                rutis::Error::from(ErrorKind::PluginFailed("duplicate capture".into()))
            })?;
            Ok(Effect::Done)
        })
    }
}
/// Process-level names from the App Server's catalog, for parts mounted on a
/// test root.
fn names() -> Names<'static> {
    static CATALOG: LazyLock<ServiceCatalog> = LazyLock::new(dim_app_server::catalog);
    static BUILD: LazyLock<Build> = LazyLock::new(Build::default);
    Names::copy(&CATALOG, &BUILD)
}
/// Provider keyed from the App Server's catalog, mounted on a test root.
fn provider_part(make: fn(&Names<'_>) -> rutis::Result<ProviderPlugin>) -> ProviderPlugin {
    make(&names()).unwrap()
}

async fn active(view: &rutis::host::FiberView) {
    let mut status = view.watch();
    while status.borrow().state != FiberState::Active {
        assert_ne!(
            status.borrow().state,
            FiberState::Failed,
            "{:?}",
            *status.borrow()
        );
        status.changed().await.unwrap();
    }
}
fn model(id: &str, max: u64) -> ModelEntry {
    serde_json::from_value(json!({
        "modelId":id, "displayName":id, "status":"active",
        "capabilities":{"modalities":["text"],"toolCalling":true,"streaming":true,"structuredOutput":true,"vision":false,"audio":false,"reasoning":false,"contextWindow":32000,"maxOutputTokens":max}
    })).unwrap()
}
const PROXY: &str = "00000000-0000-4000-8000-000000000001";
const DIRECT: &str = "00000000-0000-4000-8000-000000000002";
const EXTERNAL: &str = "00000000-0000-4000-8000-000000000003";
const ANONYMOUS: &str = "00000000-0000-4000-8000-000000000004";
const REMOTE: &str = "00000000-0000-4000-8000-000000000005";
const RECREATED: &str = "00000000-0000-4000-8000-000000000006";
const INVALID_AZURE: &str = "00000000-0000-4000-8000-000000000007";
const INVALID_ROUTE: &str = "00000000-0000-4000-8000-000000000008";
const WRONG_AWS: &str = "00000000-0000-4000-8000-000000000009";
const PROVIDER_A: &str = "00000000-0000-4000-8000-000000000010";
const PROVIDER_B: &str = "00000000-0000-4000-8000-000000000011";
const MISSING: &str = "00000000-0000-4000-8000-000000000012";
const MEDIA: &str = "00000000-0000-4000-8000-000000000013";
const AUX: &str = "00000000-0000-4000-8000-000000000014";
const HANG: &str = "00000000-0000-4000-8000-000000000015";
const PAGED_ANTHROPIC: &str = "00000000-0000-4000-8000-000000000017";
const PAGED_ANTHROPIC_UNVERSIONED: &str = "00000000-0000-4000-8000-000000000018";
const PAGED_GOOGLE: &str = "00000000-0000-4000-b000-000000000000";
const REMOTE_KIND: &str = "00000000-0000-4000-8000-000000000019";
const STORED_KIND: &str = "00000000-0000-4000-8000-000000000020";

fn input(
    id: &str,
    base: &str,
    auth: StaticAuth,
    credential: Option<&str>,
    marker: &str,
    max: u64,
) -> CreateProvider {
    CreateProvider {
        provider_id: id.into(),
        display_name: id.into(),
        connection: ConnectionInput {
            driver_kind: "openai-compatible".into(),
            base_url: base.into(),
            auth,
            network: None,
            headers: Some(HashMap::from([("x-marker".into(), marker.into())])),
            credential: credential.map(|value| CredentialUpdate::Set {
                value: value.into(),
            }),
        },
        models: vec![model("same", max)],
        enabled: true,
    }
}
fn target(provider: &str) -> ModelId {
    ModelId {
        provider_id: provider.into(),
        model_id: "same".into(),
    }
}

#[tokio::test]
async fn provider_network_is_request_local_and_covers_catalogs_and_stream_bodies() {
    use axum::{
        body::Body,
        http::{StatusCode, Uri},
        response::{IntoResponse, Response},
    };
    use dim_agent_capabilities::host::ModelNetwork;
    type Requests = Arc<Mutex<Vec<(Uri, HeaderMap)>>>;
    async fn endpoint(State(seen): State<Requests>, uri: Uri, headers: HeaderMap) -> Response {
        seen.lock().unwrap().push((uri.clone(), headers));
        if uri.path().contains("redirect") {
            return (
                StatusCode::TEMPORARY_REDIRECT,
                [("location", "/v1/chat/completions")],
            )
                .into_response();
        }
        if uri.path().ends_with("/models") {
            if uri.path().contains("slow") {
                return Response::builder()
                    .header("content-type", "application/json")
                    .body(Body::from_stream(stream::pending::<
                        Result<String, std::io::Error>,
                    >()))
                    .unwrap();
            }
            return Json(
                json!({"data":[{"id":"same","context_window":32000,"max_output_tokens":47}]}),
            )
            .into_response();
        }
        let first = Event::default().data(json!({"id":"network","choices":[{"index":0,"delta":{"content":"network reply"},"finish_reason":null}]}).to_string());
        let last = Event::default().data(
            json!({"id":"network","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]})
                .to_string(),
        );
        let events = stream::iter([Ok::<_, std::convert::Infallible>(first)]);
        if uri.path().contains("slow") {
            Sse::new(events.chain(stream::pending())).into_response()
        } else {
            Sse::new(events.chain(stream::iter([Ok(last)]))).into_response()
        }
    }
    let direct: Requests = Arc::new(Mutex::new(Vec::new()));
    let proxied: Requests = Arc::new(Mutex::new(Vec::new()));
    let mut servers = Vec::new();
    let mut addresses = Vec::new();
    for seen in [&direct, &proxied] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        addresses.push(listener.local_addr().unwrap());
        let app = Router::new().fallback(endpoint).with_state(seen.clone());
        servers.push(tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap()
        }));
    }
    let base = format!("http://{}/v1", addresses[0]);
    let proxy = format!("http://user:proxy-secret@{}", addresses[1]);
    let home = std::env::temp_dir().join(format!("provider-network-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&home).unwrap();
    let home = home.canonicalize().unwrap();
    let cancel = CancellationToken::new();
    for restart in [false, true] {
        let root = Ctx::root().unwrap();
        root.plugin(
            dim_app_server::methods::ManagementMethodsPlugin::typed(&dim_app_server::catalog())
                .unwrap(),
        );
        root.plugin(FileStoragePlugin::new(vec![home.clone()]));
        root.plugin(SqliteStoragePlugin::new(home.join("native.sqlite")));
        root.plugin(ManagedProvidersPlugin);
        root.plugin(dim_app_server::codex_oauth::OpenAICodexOAuthPlugin::new(&names(), 0).unwrap());
        root.plugin(dim_app_server::grok_oauth::GrokOAuthPlugin::new(&names(), 0).unwrap());
        active(&root.plugin(dim_agent_aimux::AimuxPlugin)).await;
        active(&root.plugin(provider_part(ProviderPlugin::new))).await;
        let provider = root.get::<ProviderService>().unwrap();
        let resolver = root.get::<ProviderConnectionResolver>().unwrap();
        let host = root.get::<ModelHost>().unwrap();
        let mut config = input(
            PROXY,
            &base,
            StaticAuth::Bearer,
            Some("model-secret"),
            "network",
            47,
        );
        if !restart {
            config.connection.network = Some(ModelNetwork {
                proxy: Some(proxy.clone()),
                timeout_ms: Some(3000),
            });
            let saved = provider.create(config.clone(), &cancel).await.unwrap();
            assert!(saved.connection.as_ref().unwrap().proxy_url.is_some());
            assert_eq!(saved.connection.as_ref().unwrap().timeout_ms, Some(3000));
            let mut other = config.clone();
            other.provider_id = DIRECT.into();
            other.connection.network = None;
            provider.create(other, &cancel).await.unwrap();
            let kept = provider
                .update(
                    config.provider_id.clone(),
                    saved.version.unwrap(),
                    serde_json::from_value(json!({"displayName":config.display_name})).unwrap(),
                    &cancel,
                )
                .await
                .unwrap();
            assert!(kept.connection.as_ref().unwrap().proxy_url.is_some());
            for network in [
                json!({"proxy":{"action":"set","value":"socks5://localhost:1080"}}),
                json!({"proxy":{"action":"set","value":"https://host/path"}}),
                json!({"proxy":{"action":"set","value":"https://host?secret=value"}}),
                json!({"timeoutMs":{"action":"set","value":0}}),
                json!({"timeoutMs":{"action":"set","value":9_007_199_254_740_992u64}}),
            ] {
                if let Ok(patch) = serde_json::from_value(json!({"network":network})) {
                    assert!(
                        provider
                            .update(PROXY.into(), kept.version.unwrap(), patch, &cancel)
                            .await
                            .is_err()
                    );
                }
            }
        }
        let listing = provider.list(&cancel).await.unwrap();
        let summary = listing
            .providers
            .iter()
            .find(|p| p.provider_id == PROXY)
            .unwrap();
        assert!(summary.proxy_configured);
        assert!(
            !serde_json::to_string(&listing)
                .unwrap()
                .contains("proxy-secret")
        );
        let mut version = summary.version;
        for id in [PROXY, DIRECT] {
            let selection = provider
                .configuration(Some(target(id)), None, ModelPreferences::default(), &cancel)
                .await
                .unwrap();
            let request = Arc::new(ModelRequest {
                capability_snapshot: selection.capability_snapshot,
                config: selection.config,
                ..ModelRequest::new(id, "same", vec![Message::text(Role::User, "hello")])
            });
            let connection = resolver
                .0
                .resolve(request.clone(), cancel.clone())
                .await
                .unwrap();
            let mut output = host
                .0
                .invoke(request, connection.connection, cancel.clone())
                .await
                .unwrap();
            let mut text = String::new();
            while let Some(chunk) = output.next().await {
                if let ModelChunk::Text(value) = chunk.unwrap() {
                    text.push_str(&value);
                }
            }
            assert_eq!(text, "network reply");
        }
        assert_eq!(direct.lock().unwrap().len(), if restart { 2 } else { 1 });
        assert_eq!(proxied.lock().unwrap().len(), if restart { 3 } else { 1 });
        let refreshed = provider.refresh_for(PROXY.into(), &cancel).await.unwrap();
        assert_eq!(refreshed.version, version + 1);
        version = refreshed.version;
        {
            let seen = proxied.lock().unwrap();
            for (uri, headers) in seen.iter() {
                assert_eq!(uri.authority().unwrap().as_str(), addresses[0].to_string());
                assert_eq!(headers["authorization"], "Bearer model-secret");
                assert_eq!(
                    headers["proxy-authorization"],
                    "Basic dXNlcjpwcm94eS1zZWNyZXQ="
                );
            }
        }
        if restart {
            let unused = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let dead = format!("http://user:private-proxy@{}", unused.local_addr().unwrap());
            drop(unused);
            // A network-only change saves alone: a dead proxy must not make it
            // unsavable.
            let saved = provider
                .update(PROXY.into(), version, serde_json::from_value(json!({"network":{"proxy":{"action":"set","value":dead},"timeoutMs":{"action":"set","value":500}}})).unwrap(), &cancel)
                .await
                .unwrap();
            assert!(
                !serde_json::to_string(&saved)
                    .unwrap()
                    .contains("private-proxy")
            );
            assert_eq!(
                direct.lock().unwrap().len(),
                2,
                "a network-only update lists nothing"
            );
            version = saved.version.unwrap();
            let saved = provider
                .update(PROXY.into(), version, serde_json::from_value(json!({"network":{"proxy":{"action":"clear"},"timeoutMs":{"action":"clear"}}})).unwrap(), &cancel)
                .await
                .unwrap();
            assert_eq!(
                direct.lock().unwrap().len(),
                2,
                "a network-only update lists nothing"
            );
            assert_eq!(saved.version, Some(version + 1));
            version = provider
                .refresh_for(PROXY.into(), &cancel)
                .await
                .unwrap()
                .version;
            assert_eq!(direct.lock().unwrap().len(), 3);
            assert!(
                provider
                    .update(PROXY.into(), version, serde_json::from_value(json!({"baseUrl":format!("http://{}/slow/v1", addresses[0]),"network":{"timeoutMs":{"action":"set","value":500}}})).unwrap(), &cancel)
                    .await
                    .is_err(),
                "a listing that outlives the catalog timeout fails the update"
            );
            // Re-point while the record is disabled: an enabled custom record
            // lists the new endpoint, and this one never answers a listing.
            version = provider
                .update(PROXY.into(), version, serde_json::from_value(json!({"baseUrl":format!("http://{}/slow/v1", addresses[0]),"network":{"timeoutMs":{"action":"set","value":500}},"enabled":false})).unwrap(), &cancel)
                .await
                .unwrap()
                .version.unwrap();
            version = provider
                .update(
                    PROXY.into(),
                    version,
                    serde_json::from_value(json!({"enabled":true})).unwrap(),
                    &cancel,
                )
                .await
                .unwrap()
                .version
                .unwrap();
            // The re-point dropped the old endpoint's models, so the model
            // the request below names is added back as a user model.
            version = provider
                .create_model(PROXY.into(), version, model("same", 21), &cancel)
                .await
                .unwrap()
                .version;
            assert!(
                tokio::time::timeout(
                    std::time::Duration::from_secs(3),
                    provider.refresh_for(PROXY.into(), &cancel)
                )
                .await
                .unwrap()
                .is_err()
            );
            assert_eq!(
                provider
                    .catalog_for(PROXY.into(), &cancel)
                    .await
                    .unwrap()
                    .unwrap()
                    .version,
                version
            );
            let selection = provider
                .configuration(
                    Some(target(PROXY)),
                    None,
                    ModelPreferences::default(),
                    &cancel,
                )
                .await
                .unwrap();
            let request = Arc::new(ModelRequest {
                capability_snapshot: selection.capability_snapshot,
                ..ModelRequest::new(PROXY, "same", vec![Message::text(Role::User, "hello")])
            });
            let connection = resolver
                .0
                .resolve(request.clone(), cancel.clone())
                .await
                .unwrap();
            let mut output = host
                .0
                .invoke(request.clone(), connection.connection, cancel.clone())
                .await
                .unwrap();
            let (mut text, mut failed, mut stopped) = (String::new(), false, false);
            tokio::time::timeout(std::time::Duration::from_secs(3), async {
                while let Some(chunk) = output.next().await {
                    match chunk {
                        Ok(ModelChunk::Text(value)) => text.push_str(&value),
                        Ok(ModelChunk::MessageStop { .. }) => stopped = true,
                        Err(_) => failed = true,
                        _ => {}
                    }
                }
            })
            .await
            .unwrap();
            assert_eq!(text, "network reply");
            assert!(failed && !stopped);
            version = provider
                .update(
                    PROXY.into(),
                    version,
                    serde_json::from_value(
                        json!({"baseUrl":format!("http://{}/redirect/v1", addresses[0]),"enabled":false}),
                    )
                    .unwrap(),
                    &cancel,
                )
                .await
                .unwrap()
                .version
                .unwrap();
            provider
                .update(
                    PROXY.into(),
                    version,
                    serde_json::from_value(json!({"enabled":true})).unwrap(),
                    &cancel,
                )
                .await
                .unwrap();
            let before = direct.lock().unwrap().len();
            let connection = resolver
                .0
                .resolve(request.clone(), cancel.clone())
                .await
                .unwrap();
            assert!(
                host.0
                    .invoke(request, connection.connection, cancel.clone())
                    .await
                    .is_err()
            );
            assert_eq!(direct.lock().unwrap().len(), before + 1);
        }
        root.shutdown().await.unwrap();
    }
    for server in servers {
        server.abort();
    }
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test]
async fn external_secrets_are_request_local_and_follow_plugin_lifetime() {
    use dim_agent_capabilities::host::{HostError, HostErrorKind, HostFuture};
    use dim_app_server::provider::{ProviderSecret, ProviderSecretHost, ResolveProviderSecret};
    use tokio::sync::{mpsc, oneshot};
    type Lookup = (
        String,
        String,
        oneshot::Sender<Result<ProviderSecret, HostError>>,
    );
    #[derive(Clone)]
    struct Secrets(mpsc::Sender<Lookup>);
    impl ResolveProviderSecret for Secrets {
        fn resolve(
            &self,
            id: String,
            reference: String,
            _: CancellationToken,
        ) -> HostFuture<'_, ProviderSecret> {
            Box::pin(async move {
                let (reply, response) = oneshot::channel();
                self.0
                    .send((id, reference, reply))
                    .await
                    .map_err(|_| HostError::failed("fixture closed"))?;
                response
                    .await
                    .map_err(|_| HostError::failed("fixture closed"))?
            })
        }
    }
    impl Plugin for Secrets {
        type Deps = ();
        fn name(&self) -> &str {
            "test.secrets"
        }
        async fn apply(&self, ctx: &Ctx, (): Self::Deps) -> rutis::Result {
            ctx.provide(ProviderSecretHost(Arc::new(self.clone())))?;
            Ok(())
        }
    }

    let seen = Arc::new(Mutex::new(Vec::new()));
    let catalog_auth = Arc::new(Mutex::new(Vec::new()));
    let recorded = catalog_auth.clone();
    let app = Router::new().route("/v1/chat/completions", post(completion))
        .route("/v1/models", axum::routing::get(move |headers: HeaderMap| {
            recorded.lock().unwrap().push(headers["authorization"].to_str().unwrap().to_owned());
            async { Json(json!({"data":[{"id":"same", "context_window":32000,"max_output_tokens":43}]})) }
        })).with_state(seen.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/v1", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let home = std::env::temp_dir().join(format!("provider-secrets-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&home).unwrap();
    let home = home.canonicalize().unwrap();
    let root = Ctx::root().unwrap();
    root.plugin(
        dim_app_server::methods::ManagementMethodsPlugin::typed(&dim_app_server::catalog())
            .unwrap(),
    );
    root.plugin(FileStoragePlugin::new(vec![home.clone()]));
    root.plugin(SqliteStoragePlugin::new(home.join("native.sqlite")));
    root.plugin(ManagedProvidersPlugin);
    root.plugin(dim_app_server::codex_oauth::OpenAICodexOAuthPlugin::new(&names(), 0).unwrap());
    root.plugin(dim_app_server::grok_oauth::GrokOAuthPlugin::new(&names(), 0).unwrap());
    active(&root.plugin(dim_agent_aimux::AimuxPlugin)).await;
    let plain = root.plugin(provider_part(ProviderPlugin::new));
    active(&plain).await;
    let provider = root.get::<ProviderService>().unwrap();
    let cancel = CancellationToken::new();
    let mut config = input(EXTERNAL, &base, StaticAuth::Bearer, None, "secrets", 43);
    config.connection.credential = Some(CredentialUpdate::SetRef {
        reference: "opaque-account-key".into(),
    });
    let mut invalid = config.clone();
    invalid.connection.auth = StaticAuth::ApiKey {
        header_name: "x-key".into(),
        prefix: "bad\r\nprefix".into(),
    };
    assert!(provider.create(invalid, &cancel).await.is_err());
    let summary = provider.create(config.clone(), &cancel).await.unwrap();
    assert!(summary.connection.as_ref().unwrap().credential_configured);
    assert!(
        !serde_json::to_string(&summary)
            .unwrap()
            .contains("opaque-account-key")
    );
    assert!(matches!(
        provider.refresh_for(EXTERNAL.into(), &cancel).await,
        Err(ProviderError::Unsupported(_))
    ));
    let mut anonymous = config.clone();
    anonymous.provider_id = ANONYMOUS.into();
    anonymous.connection.auth = StaticAuth::Anonymous;
    provider.create(anonymous, &cancel).await.unwrap();
    let anonymous = provider
        .configuration(
            Some(target(ANONYMOUS)),
            None,
            ModelPreferences::default(),
            &cancel,
        )
        .await
        .unwrap();
    let anonymous = Arc::new(ModelRequest {
        capability_snapshot: anonymous.capability_snapshot,
        ..ModelRequest::new(ANONYMOUS, "same", vec![])
    });
    let anonymous = root
        .get::<ProviderConnectionResolver>()
        .unwrap()
        .0
        .resolve(anonymous, cancel.clone())
        .await
        .unwrap();
    assert!(
        matches!(anonymous.connection, ModelConnection::Http(connection) if connection.bearer.is_none())
    );
    let selection = provider
        .configuration(
            Some(target(EXTERNAL)),
            None,
            ModelPreferences::default(),
            &cancel,
        )
        .await
        .unwrap();
    let request = Arc::new(ModelRequest {
        capability_snapshot: selection.capability_snapshot,
        config: selection.config,
        ..ModelRequest::new(EXTERNAL, "same", vec![Message::text(Role::User, "hello")])
    });
    assert!(matches!(
        root.get::<ProviderConnectionResolver>()
            .unwrap()
            .0
            .resolve(request.clone(), cancel.clone())
            .await,
        Err(HostError::Failed {
            kind: HostErrorKind::Unsupported,
            ..
        })
    ));
    plain.dispose().await.unwrap();

    let view = root.plugin(provider_part(ProviderPlugin::with_secret_host));
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(50), active(&view))
            .await
            .is_err()
    );
    assert!(root.get::<ProviderService>().is_none());
    let (send, mut lookups) = mpsc::channel::<Lookup>(8);
    let secrets = root.plugin(Secrets(send));
    active(&secrets).await;
    tokio::time::timeout(std::time::Duration::from_secs(5), active(&view))
        .await
        .unwrap();
    let provider = root.get::<ProviderService>().unwrap();
    let resolver = root.get::<ProviderConnectionResolver>().unwrap();
    let host = root.get::<ModelHost>().unwrap();
    let probe_id = "018ec73f-4d19-7f61-bc79-7130618bb452";
    let probe_input = json!({"providerId":probe_id,"connection":{"driverKind":"openai-compatible","auth":{"type":"bearer"},"baseUrl":base,"credential":{"action":"set_ref","reference":"unsaved-reference"}}});
    let before_probe = serde_json::to_value(provider.list(&cancel).await.unwrap()).unwrap();
    let probe = {
        let owner = provider.clone();
        let input = serde_json::from_value(probe_input.clone()).unwrap();
        tokio::spawn(async move { owner.probe(input, &CancellationToken::new()).await })
    };
    let (id, reference, reply) = lookups.recv().await.unwrap();
    assert_eq!(id, probe_id);
    assert_eq!(reference, "unsaved-reference");
    assert!(
        reply
            .send(Ok(ProviderSecret::Token("probe-resolved".into())))
            .is_ok()
    );
    assert_eq!(probe.await.unwrap().unwrap().models[0].model_id, "same");
    assert_eq!(
        serde_json::to_value(provider.list(&cancel).await.unwrap()).unwrap(),
        before_probe
    );
    let cancelled_probe = CancellationToken::new();
    let probe = {
        let owner = provider.clone();
        let cancel = cancelled_probe.clone();
        let input = serde_json::from_value(probe_input.clone()).unwrap();
        tokio::spawn(async move { owner.probe(input, &cancel).await })
    };
    let (_, _, reply) = lookups.recv().await.unwrap();
    cancelled_probe.cancel();
    assert!(matches!(
        probe.await.unwrap(),
        Err(ProviderError::Cancelled)
    ));
    assert!(reply.is_closed());
    let spawn = |cancel: CancellationToken| {
        let resolver = resolver.clone();
        let request = request.clone();
        tokio::spawn(async move { resolver.0.resolve(request, cancel).await })
    };
    for token in ["resolved-first", "resolved-rotated"] {
        let task = spawn(cancel.clone());
        let (id, reference, reply) = lookups.recv().await.unwrap();
        assert_eq!(id, EXTERNAL);
        assert_eq!(reference, "opaque-account-key");
        assert!(reply.send(Ok(ProviderSecret::Token(token.into()))).is_ok());
        let resolved = task.await.unwrap().unwrap();
        assert!(resolved.recovery.is_none());
        let mut output = host
            .0
            .invoke(request.clone(), resolved.connection, cancel.clone())
            .await
            .unwrap();
        let mut text = String::new();
        while let Some(chunk) = output.next().await {
            if let ModelChunk::Text(delta) = chunk.unwrap() {
                text.push_str(&delta);
            }
        }
        assert_eq!(text, "ok");
        assert_eq!(
            seen.lock()
                .unwrap()
                .last()
                .unwrap()
                .authorization
                .as_deref(),
            Some(format!("Bearer {token}").as_str())
        );
    }
    let owner = provider.clone();
    let refresh = tokio::spawn(async move {
        owner
            .refresh_for(EXTERNAL.into(), &CancellationToken::new())
            .await
    });
    let (_, _, reply) = lookups.recv().await.unwrap();
    assert!(
        reply
            .send(Ok(ProviderSecret::Token("catalog-resolved".into())))
            .is_ok()
    );
    assert_eq!(refresh.await.unwrap().unwrap().version, 2);
    assert_eq!(
        *catalog_auth.lock().unwrap(),
        ["Bearer probe-resolved", "Bearer catalog-resolved"]
    );
    let db = rusqlite::Connection::open(home.join("native.sqlite")).unwrap();
    let record: String = db
        .query_row(
            "SELECT record FROM providers WHERE provider_id=?1",
            [EXTERNAL],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&record).unwrap()["credential"],
        json!({"secretRef":"opaque-account-key"})
    );
    assert!(!record.contains("resolved"));
    drop(db);

    for result in [
        Ok(ProviderSecret::Token("".into())),
        Ok(ProviderSecret::Token("injected\r\nsecret".into())),
        Ok(ProviderSecret::Aws(AwsCredentials {
            access_key_id: "access".into(),
            secret_access_key: "secret".into(),
            session_token: None,
        })),
        Err(HostError::failed("sensitive-backend-secret")),
    ] {
        let task = spawn(cancel.clone());
        let (_, _, reply) = lookups.recv().await.unwrap();
        assert!(reply.send(result).is_ok());
        let error = task.await.unwrap().err().unwrap().to_string();
        assert!(!error.contains("sensitive-backend-secret"));
        assert!(!error.contains("injected"));
    }
    assert_eq!(seen.lock().unwrap().len(), 2);

    let stopped = CancellationToken::new();
    let task = spawn(stopped.clone());
    let (_, _, reply) = lookups.recv().await.unwrap();
    stopped.cancel();
    assert!(matches!(task.await.unwrap(), Err(HostError::Cancelled(_))));
    assert!(reply.is_closed());
    let task = spawn(cancel.clone());
    let (_, _, reply) = lookups.recv().await.unwrap();
    let probe = {
        let owner = provider.clone();
        let input = serde_json::from_value(probe_input).unwrap();
        tokio::spawn(async move { owner.probe(input, &CancellationToken::new()).await })
    };
    let (_, _, probe_reply) = lookups.recv().await.unwrap();

    tokio::time::timeout(std::time::Duration::from_secs(5), secrets.dispose())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        task.await.unwrap(),
        Err(HostError::Failed {
            kind: HostErrorKind::OwnerClosed,
            ..
        })
    ));
    assert!(reply.is_closed());
    assert!(matches!(
        probe.await.unwrap(),
        Err(ProviderError::Cancelled)
    ));
    assert!(probe_reply.is_closed());

    assert!(provider.list(&cancel).await.is_err());
    root.shutdown().await.unwrap();
    server.abort();
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test]
async fn remote_catalog_refresh_preserves_edits_and_rejects_stale_or_partial_results() {
    use axum::{
        http::{StatusCode, Uri},
        routing::get,
    };
    use tokio::sync::{mpsc, oneshot};
    type Exchange = (Uri, HeaderMap, oneshot::Sender<(StatusCode, Json<Value>)>);
    let (send, mut requests) = mpsc::channel::<Exchange>(8);
    let app = Router::new().route(
        "/{*path}",
        get(move |uri: Uri, headers: HeaderMap| {
            let send = send.clone();
            async move {
                let (reply, response) = oneshot::channel();
                send.send((uri, headers, reply)).await.unwrap();
                response
                    .await
                    .unwrap_or((StatusCode::INTERNAL_SERVER_ERROR, Json(json!({}))))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let home = std::env::temp_dir().join(format!("provider-refresh-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&home).unwrap();
    let home = home.canonicalize().unwrap();
    let root = Ctx::root().unwrap();
    root.plugin(
        dim_app_server::methods::ManagementMethodsPlugin::typed(&dim_app_server::catalog())
            .unwrap(),
    );
    root.plugin(FileStoragePlugin::new(vec![home.clone()]));
    root.plugin(SqliteStoragePlugin::new(home.join("native.sqlite")));
    root.plugin(ManagedProvidersPlugin);
    root.plugin(dim_app_server::codex_oauth::OpenAICodexOAuthPlugin::new(&names(), 0).unwrap());
    root.plugin(dim_app_server::grok_oauth::GrokOAuthPlugin::new(&names(), 0).unwrap());
    root.plugin(dim_agent_aimux::AimuxPlugin);
    let view = root.plugin(provider_part(ProviderPlugin::new));
    tokio::time::timeout(std::time::Duration::from_secs(5), active(&view))
        .await
        .unwrap();
    let provider = root.get::<ProviderService>().unwrap();
    let cancel = CancellationToken::new();
    let probe_id = "018ec73f-4d19-7f61-bc79-7130618bb451";
    let connection = json!({"driverKind":"openai-compatible","auth":{"type":"bearer"},"baseUrl":format!("{base}/draft/v1"),"credential":{"action":"set","value":"probe-secret"}});
    let before_probe = serde_json::to_value(provider.list(&cancel).await.unwrap()).unwrap();
    let probe = {
        let provider = provider.clone();
        let cancel = cancel.clone();
        let input =
            serde_json::from_value(json!({"providerId":probe_id,"connection":connection})).unwrap();
        tokio::spawn(async move { provider.probe(input, &cancel).await })
    };
    let (uri, headers, reply) = requests.recv().await.unwrap();
    assert_eq!(uri.path(), "/draft/v1/models");
    assert_eq!(headers["authorization"], "Bearer probe-secret");
    reply
        .send((
            StatusCode::OK,
            Json(json!({"data":[{"id":"unsaved","context_window":321,"max_output_tokens":123}]})),
        ))
        .unwrap();
    let probed = probe.await.unwrap().unwrap();
    assert_eq!(probed.models[0].model_id, "unsaved");
    assert_eq!(probed.models[0].capabilities.max_output_tokens, 123);
    assert_eq!(
        serde_json::to_value(provider.list(&cancel).await.unwrap()).unwrap(),
        before_probe
    );
    let rejected_probe = {
        let provider = provider.clone();
        let cancel = cancel.clone();
        let input =
            serde_json::from_value(json!({"providerId":probe_id,"connection":connection})).unwrap();
        tokio::spawn(async move { provider.probe(input, &cancel).await })
    };
    requests
        .recv()
        .await
        .unwrap()
        .2
        .send((
            StatusCode::UNAUTHORIZED,
            Json(json!({"error":"probe-secret must not escape"})),
        ))
        .unwrap();
    assert!(matches!(
        rejected_probe.await.unwrap(),
        Err(ProviderError::AuthFailed)
    ));
    assert_eq!(
        serde_json::to_value(provider.list(&cancel).await.unwrap()).unwrap(),
        before_probe
    );
    let mut config = input(
        REMOTE,
        &format!("{base}/tenant/v1beta"),
        StaticAuth::Bearer,
        Some("catalog-key"),
        "refresh",
        37,
    );
    config.connection.driver_kind = "new-api-router".into();
    provider.create(config.clone(), &cancel).await.unwrap();
    let admitted = serde_json::to_value(
        provider
            .management(Some(REMOTE.into()), &cancel)
            .await
            .unwrap(),
    )
    .unwrap();
    let probe = {
        let provider = provider.clone();
        let cancel = cancel.clone();
        let input = serde_json::from_value(json!({"providerId":REMOTE,"connection":{"baseUrl":format!("{base}/changed/v1"),"headers":[{"name":"x-draft","action":"set","value":"unsaved-header"}]}})).unwrap();
        tokio::spawn(async move { provider.probe(input, &cancel).await })
    };
    let (uri, headers, reply) = requests.recv().await.unwrap();
    assert_eq!(uri.path(), "/changed/v1/models");
    assert_eq!(headers["authorization"], "Bearer catalog-key");
    assert_eq!(headers["x-draft"], "unsaved-header");
    reply
        .send((StatusCode::OK, Json(json!({"data":[]}))))
        .unwrap();
    assert!(probe.await.unwrap().unwrap().models.is_empty());
    assert_eq!(
        serde_json::to_value(
            provider
                .management(Some(REMOTE.into()), &cancel)
                .await
                .unwrap()
        )
        .unwrap(),
        admitted
    );
    let edited = provider
        .update_model_config(
            REMOTE.into(),
            1,
            "same".into(),
            serde_json::from_value(json!({"displayName":"My model","maxOutputTokens":91})).unwrap(),
            &cancel,
        )
        .await
        .unwrap();
    let disabled = provider
        .set_enabled_models(REMOTE.into(), edited.version, vec![], &cancel)
        .await
        .unwrap();
    let spawn = |id: &str, cancel: CancellationToken| {
        let provider = provider.clone();
        let id = id.to_owned();
        tokio::spawn(async move { provider.refresh_for(id, &cancel).await })
    };
    let refresh = spawn(REMOTE, cancel.clone());
    let (uri, headers, reply) = requests.recv().await.unwrap();
    assert_eq!(uri.path(), "/tenant/v1/models");
    assert_eq!(headers["authorization"], "Bearer catalog-key");
    assert_eq!(headers["x-marker"], "refresh");
    reply.send((StatusCode::OK, Json(json!({"data":[
        {"id":"same","context_window":65001,"max_output_tokens":407,"endpoint_type":"anthropic"},
        {"id":"other","context_window":83003,"max_output_tokens":509,"endpoint_type":"openai-response"}
    ]})))).unwrap();
    let result = refresh.await.unwrap().unwrap();
    assert_eq!(result.version, disabled.version + 1);
    assert_eq!(result.disabled_model_ids, ["same"]);
    assert_eq!(result.models[0].name, "My model");
    assert_eq!(result.models[0].config.max_tokens, Some(91.0));
    assert_eq!(result.models[1].capabilities.context_window, 83003);
    assert_eq!(result.models[1].config.max_tokens, Some(509.0));
    let refresh = spawn(REMOTE, cancel.clone());
    requests.recv().await.unwrap().2.send((StatusCode::OK,
        Json(json!({"data":[{"id":"other","context_window":83003,"max_output_tokens":509,"endpoint_type":"openai-response"}]})))).unwrap();
    let missing = refresh.await.unwrap().unwrap();
    let chosen = provider
        .set_enabled_models(
            REMOTE.into(),
            missing.version,
            vec!["other".into()],
            &cancel,
        )
        .await
        .unwrap();
    assert!(chosen.disabled_model_ids.contains(&"same".into()));
    let refresh = spawn(REMOTE, cancel.clone());
    requests.recv().await.unwrap().2.send((StatusCode::OK, Json(json!({"data":[
        {"id":"same","context_window":65001,"max_output_tokens":407,"endpoint_type":"anthropic"},
        {"id":"other","context_window":83003,"max_output_tokens":509,"endpoint_type":"openai-response"}
    ]})))).unwrap();
    let result = refresh.await.unwrap().unwrap();
    assert!(result.disabled_model_ids.contains(&"same".into()));
    assert_eq!(result.models[0].name, "My model");
    assert_eq!(result.models[0].config.max_tokens, Some(91.0));
    let selection = provider
        .configuration(
            Some(ModelId {
                provider_id: REMOTE.into(),
                model_id: "other".into(),
            }),
            None,
            ModelPreferences::default(),
            &cancel,
        )
        .await
        .unwrap();
    let resolver = root.get::<ProviderConnectionResolver>().unwrap();
    let resolved = resolver
        .0
        .resolve(
            Arc::new(ModelRequest {
                capability_snapshot: selection.capability_snapshot,
                ..ModelRequest::new(REMOTE, "other", vec![])
            }),
            cancel.clone(),
        )
        .await
        .unwrap();
    let ModelConnection::Http(connection) = resolved.connection else {
        panic!("HTTP connection expected")
    };
    assert_eq!(
        connection.protocol,
        dim_agent_capabilities::host::ModelProtocol::OpenaiResponses
    );

    for (status, body) in [
        (
            StatusCode::UNAUTHORIZED,
            json!({"error":"secret must not be echoed"}),
        ),
        (StatusCode::OK, json!({"unexpected":[]})),
        (StatusCode::OK, json!({"data":[{}]})),
        (
            StatusCode::OK,
            json!({"data":[{"id":"bad","endpoint_type":"invalid"}]}),
        ),
        (
            StatusCode::OK,
            json!({"data":[{"id":"bad","context_window":0}]}),
        ),
        (
            StatusCode::OK,
            json!({"data":[{"id":"bad","limit":{"output":1.5}}]}),
        ),
    ] {
        let refresh = spawn(REMOTE, cancel.clone());
        requests
            .recv()
            .await
            .unwrap()
            .2
            .send((status, Json(body)))
            .unwrap();
        assert!(refresh.await.unwrap().is_err());
        assert_eq!(
            provider
                .catalog_for(REMOTE.into(), &cancel)
                .await
                .unwrap()
                .unwrap()
                .version,
            result.version
        );
    }
    let refresh = spawn(REMOTE, cancel.clone());
    let (_, _, reply) = requests.recv().await.unwrap();
    let saved = provider
        .update(
            REMOTE.into(),
            result.version,
            serde_json::from_value(json!({"displayName":"Changed while refreshing"})).unwrap(),
            &cancel,
        )
        .await
        .unwrap();
    reply
        .send((StatusCode::OK, Json(json!({"data":[]}))))
        .unwrap();
    assert!(matches!(
        refresh.await.unwrap(),
        Err(ProviderError::Conflict(_))
    ));
    let cancelled = CancellationToken::new();
    let refresh = spawn(REMOTE, cancelled.clone());
    let (_, _, reply) = requests.recv().await.unwrap();
    cancelled.cancel();
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(2), refresh)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
    drop(reply);
    assert_eq!(
        provider
            .catalog_for(REMOTE.into(), &cancel)
            .await
            .unwrap()
            .unwrap()
            .version,
        saved.version.unwrap()
    );

    let refresh = spawn(REMOTE, cancel.clone());
    requests
        .recv()
        .await
        .unwrap()
        .2
        .send((StatusCode::OK, Json(json!({"data":[]}))))
        .unwrap();
    let empty = refresh.await.unwrap().unwrap();
    assert!(empty.models.is_empty());
    assert_eq!(empty.disabled_model_ids, ["same"]);
    assert!(
        provider
            .list(&cancel)
            .await
            .unwrap()
            .providers
            .iter()
            .find(|p| p.provider_id == REMOTE)
            .unwrap()
            .enabled
    );

    for (id, driver, header, prefix, path, first, next, second) in [
        (
            PAGED_GOOGLE,
            "gemini",
            "x-goog-api-key",
            "/tenant/v1beta/",
            "/tenant/v1beta/models",
            json!({"models":[{"name":"models/first","inputTokenLimit":17003,"outputTokenLimit":103}],"nextPageToken":"a+b"}),
            "pageToken=a%2Bb",
            json!({"models":[{"name":"models/last","inputTokenLimit":19009,"outputTokenLimit":109,"supportedGenerationMethods":["embedContent"]}]}),
        ),
        (
            PAGED_ANTHROPIC,
            "anthropic",
            "x-api-key",
            "/tenant/v1/",
            "/tenant/v1/models",
            json!({"data":[{"id":"first","type":"model"}],"has_more":true,"last_id":"a+b"}),
            "after_id=a%2Bb",
            json!({"data":[{"id":"last"}],"has_more":false}),
        ),
        (
            PAGED_ANTHROPIC_UNVERSIONED,
            "anthropic",
            "x-api-key",
            "/tenant",
            "/tenant/v1/models",
            json!({"data":[{"id":"first","type":"model"}],"has_more":true,"last_id":"a+b"}),
            "after_id=a%2Bb",
            json!({"data":[{"id":"last"}],"has_more":false}),
        ),
    ] {
        let mut config = input(
            id,
            &format!("{base}{prefix}"),
            StaticAuth::ApiKey {
                header_name: header.into(),
                prefix: String::new(),
            },
            Some("page-key"),
            "pages",
            41,
        );
        config.connection.driver_kind = driver.into();
        config.models = Vec::new();
        provider.create(config, &cancel).await.unwrap();
        assert!(
            provider
                .configuration(
                    Some(ModelId {
                        provider_id: id.into(),
                        model_id: "first".into()
                    }),
                    None,
                    ModelPreferences::default(),
                    &cancel
                )
                .await
                .is_err()
        );
        let refresh = spawn(id, cancel.clone());
        let (uri, headers, reply) = requests.recv().await.unwrap();
        assert_eq!(uri.path(), path);
        assert_eq!(uri.query(), None);
        assert_eq!(headers[header], "page-key");
        if driver == "anthropic" {
            assert_eq!(headers["anthropic-version"], "2023-06-01");
        }
        reply.send((StatusCode::OK, Json(first.clone()))).unwrap();
        let (uri, _, reply) = requests.recv().await.unwrap();
        assert_eq!(uri.path(), path);
        assert_eq!(uri.query(), Some(next));
        reply.send((StatusCode::OK, Json(second))).unwrap();
        let page = refresh.await.unwrap().unwrap();
        assert_eq!(
            page.models
                .iter()
                .map(|m| m.id.as_str())
                .collect::<Vec<_>>(),
            ["first", "last"]
        );
        if driver == "gemini" {
            assert_eq!(page.models[1].config.max_tokens, Some(109.0));
            assert!(page.models[1].unavailable_reason.is_some());
        } else {
            assert!(
                page.models.iter().all(|m| m.unavailable_reason.is_none()),
                "Anthropic's `type: \"model\"` is an object discriminator, not a modality; every remote Anthropic model stays non-media"
            );
        }
        let refresh = spawn(id, cancel.clone());
        requests
            .recv()
            .await
            .unwrap()
            .2
            .send((StatusCode::OK, Json(first.clone())))
            .unwrap();
        requests
            .recv()
            .await
            .unwrap()
            .2
            .send((StatusCode::OK, Json(first)))
            .unwrap();
        assert!(matches!(refresh.await.unwrap(), Err(ProviderError::Failed)));
        assert_eq!(
            provider
                .catalog_for(id.into(), &cancel)
                .await
                .unwrap()
                .unwrap()
                .version,
            page.version
        );
    }
    let aba = input(
        RECREATED,
        &base,
        StaticAuth::Bearer,
        Some("old-key"),
        "old",
        23,
    );
    provider.create(aba.clone(), &cancel).await.unwrap();
    let refresh = spawn(RECREATED, cancel.clone());
    let (_, _, reply) = requests.recv().await.unwrap();
    provider
        .delete_static(RECREATED.into(), 1, &cancel)
        .await
        .unwrap();
    let mut replacement = aba;
    replacement.connection.credential = Some(CredentialUpdate::Set {
        value: "new-key".into(),
    });
    replacement.models = vec![model("replacement", 71)];
    provider.create(replacement, &cancel).await.unwrap();
    reply
        .send((StatusCode::OK, Json(json!({"data":[]}))))
        .unwrap();
    assert!(
        refresh.await.unwrap().is_err(),
        "A recreated connection must reject the old refresh"
    );
    assert_eq!(
        provider
            .catalog_for(RECREATED.into(), &cancel)
            .await
            .unwrap()
            .unwrap()
            .models[0]
            .id,
        "replacement"
    );
    let builtin = provider
        .list(&cancel)
        .await
        .unwrap()
        .providers
        .into_iter()
        .find(|p| p.provider_id == "openai")
        .unwrap();
    provider
        .update(builtin.provider_id, builtin.version, serde_json::from_value(json!({"baseUrl":format!("{base}/v1"),"credential":{"action":"set","value":"builtin-key"},"enabled":true})).unwrap(), &cancel)
        .await
        .unwrap();
    let refresh = spawn("openai", cancel.clone());
    requests
        .recv()
        .await
        .unwrap()
        .2
        .send((StatusCode::OK, Json(json!({"data":[]}))))
        .unwrap();
    let builtin_empty = refresh.await.unwrap().unwrap();
    assert!(builtin_empty.models.is_empty());
    assert!(
        provider
            .refresh_for("openai-codex".into(), &cancel)
            .await
            .unwrap_err()
            .to_string()
            .contains("does not support")
    );
    let current = provider
        .catalog_for(REMOTE.into(), &cancel)
        .await
        .unwrap()
        .unwrap();
    let manual = provider
        .create_model(REMOTE.into(), current.version, model("manual", 77), &cancel)
        .await
        .unwrap();
    assert!(manual.models.iter().any(|model| model.id == "manual"));
    let refresh = spawn(REMOTE, cancel.clone());
    requests.recv().await.unwrap().2.send((StatusCode::OK, Json(json!({"data":[
        {"id":"gone","context_window":3000,"max_output_tokens":90,"endpoint_type":"openai-response"},
        {"id":"stable","context_window":4000,"max_output_tokens":100,"endpoint_type":"gemini"}
    ]})))).unwrap();
    let fresh = refresh.await.unwrap().unwrap();
    assert!(fresh.models.iter().any(|model| model.id == "manual"));
    let edited_default = provider.update_model_config(REMOTE.into(), fresh.version, "gone".into(),
        Some(serde_json::from_value(json!({"displayName":"Former default","endpoint":{"action":"set","value":"anthropic"}})).unwrap()), &cancel).await.unwrap();
    let disabled_user = provider
        .set_enabled_models(
            REMOTE.into(),
            edited_default.version,
            vec!["stable".into()],
            &cancel,
        )
        .await
        .unwrap();
    assert!(
        disabled_user
            .disabled_model_ids
            .iter()
            .any(|id| id == "manual")
    );
    let refresh = spawn(REMOTE, cancel.clone());
    requests.recv().await.unwrap().2.send((StatusCode::OK, Json(json!({"data":[
        {"id":"stable","context_window":4000,"max_output_tokens":100,"endpoint_type":"gemini"}
    ]})))).unwrap();
    let disappeared = refresh.await.unwrap().unwrap();
    let mut user = model("gone", 222);
    user.metadata =
        Some(serde_json::from_value(json!({"raw":{"endpointType":"openai-response"}})).unwrap());
    let created = provider
        .create_model(REMOTE.into(), disappeared.version, user.clone(), &cancel)
        .await
        .unwrap();
    let gone = created
        .models
        .iter()
        .find(|model| model.id == "gone")
        .unwrap();
    assert_eq!(
        gone.name, "gone",
        "a disappeared default's descriptor is not inherited"
    );
    assert_eq!(
        gone.endpoint,
        Some(dim_agent_capabilities::host::ModelProtocol::OpenaiResponses)
    );
    assert!(!created.disabled_model_ids.iter().any(|id| id == "gone"));
    for duplicate in ["stable", "gone"] {
        assert!(matches!(
            provider
                .create_model(
                    REMOTE.into(),
                    created.version,
                    model(duplicate, 333),
                    &cancel
                )
                .await,
            Err(dim_app_server::provider::ProviderError::Conflict(_))
        ));
    }
    assert!(matches!(
        provider
            .create_model(
                REMOTE.into(),
                created.version - 1,
                model("stale", 333),
                &cancel
            )
            .await,
        Err(dim_app_server::provider::ProviderError::Conflict(_))
    ));
    let edited_user = provider
        .update_model_config(
            REMOTE.into(),
            created.version,
            "gone".into(),
            Some(
                serde_json::from_value(json!({"displayName":"My user model","maxOutputTokens":333,
            "endpoint":{"action":"set","value":"gemini"}}))
                .unwrap(),
            ),
            &cancel,
        )
        .await
        .unwrap();
    let gone = edited_user
        .models
        .iter()
        .find(|model| model.id == "gone")
        .unwrap();
    assert_eq!(gone.name, "My user model");
    assert_eq!(gone.config.max_tokens, Some(333.0));
    assert_eq!(
        gone.endpoint,
        Some(dim_agent_capabilities::host::ModelProtocol::Gemini)
    );
    let selected = provider
        .selection(Some(&home), &cancel)
        .await
        .unwrap()
        .map(|selection| selection.version);
    provider
        .select(
            Some(&home),
            ModelId {
                provider_id: REMOTE.into(),
                model_id: "gone".into(),
            },
            selected,
            &cancel,
        )
        .await
        .unwrap();
    let deleted = provider
        .delete_model(REMOTE.into(), edited_user.version, "gone".into(), &cancel)
        .await
        .unwrap();
    assert!(
        provider
            .selection(Some(&home), &cancel)
            .await
            .unwrap()
            .is_none()
    );
    assert!(!deleted.models.iter().any(|model| model.id == "gone"));
    assert!(matches!(
        provider
            .delete_model(REMOTE.into(), deleted.version, "stable".into(), &cancel)
            .await,
        Err(dim_app_server::provider::ProviderError::Unsupported(_))
    ));
    let recreated = provider
        .create_model(REMOTE.into(), deleted.version, user, &cancel)
        .await
        .unwrap();
    let gone = recreated
        .models
        .iter()
        .find(|model| model.id == "gone")
        .unwrap();
    assert_eq!(gone.name, "gone");
    assert_eq!(
        gone.endpoint,
        Some(dim_agent_capabilities::host::ModelProtocol::OpenaiResponses)
    );
    assert!(!recreated.disabled_model_ids.iter().any(|id| id == "gone"));
    let route_only = provider
        .update_model_config(
            REMOTE.into(),
            recreated.version,
            "stable".into(),
            Some(
                serde_json::from_value(
                    json!({"endpoint":{"action":"set","value":"openai-response"}}),
                )
                .unwrap(),
            ),
            &cancel,
        )
        .await
        .unwrap();
    let refresh = spawn(REMOTE, cancel.clone());
    requests.recv().await.unwrap().2.send((StatusCode::OK, Json(json!({"data":[
        {"id":"stable","context_window":9002,"max_output_tokens":902,"endpoint_type":"anthropic"}
    ]})))).unwrap();
    let followed = refresh.await.unwrap().unwrap();
    assert_eq!(followed.version, route_only.version + 1);
    let stable = followed
        .models
        .iter()
        .find(|model| model.id == "stable")
        .unwrap();
    assert_eq!(
        stable.config.max_tokens,
        Some(902.0),
        "route-only edits leave descriptors following discovery"
    );
    assert_eq!(
        stable.endpoint,
        Some(dim_agent_capabilities::host::ModelProtocol::OpenaiResponses)
    );
    let cleared = provider
        .update_model_config(
            REMOTE.into(),
            followed.version,
            "stable".into(),
            Some(serde_json::from_value(json!({"endpoint":{"action":"clear"}})).unwrap()),
            &cancel,
        )
        .await
        .unwrap();
    assert_eq!(
        cleared
            .models
            .iter()
            .find(|model| model.id == "stable")
            .unwrap()
            .endpoint,
        Some(dim_agent_capabilities::host::ModelProtocol::Anthropic)
    );
    let before_collision = serde_json::to_value(&cleared).unwrap();
    let refresh = spawn(REMOTE, cancel.clone());
    requests
        .recv()
        .await
        .unwrap()
        .2
        .send((
            StatusCode::OK,
            Json(json!({"data":[
                {"id":"gone","context_window":5000,"max_output_tokens":500}
            ]})),
        ))
        .unwrap();
    assert!(matches!(
        refresh.await.unwrap(),
        Err(dim_app_server::provider::ProviderError::Conflict(_))
    ));
    assert_eq!(
        serde_json::to_value(
            provider
                .catalog_for(REMOTE.into(), &cancel)
                .await
                .unwrap()
                .unwrap()
        )
        .unwrap(),
        before_collision
    );
    let retained = provider
        .catalog_for(REMOTE.into(), &cancel)
        .await
        .unwrap()
        .unwrap();
    root.shutdown().await.unwrap();
    {
        let db = rusqlite::Connection::open(home.join("native.sqlite")).unwrap();
        db.execute("UPDATE providers SET record=json_set(record,'$.definitionFingerprint','previous-bundle') WHERE provider_id='openai'", []).unwrap();
        db.execute("UPDATE providers SET record=json_set(record,'$.modelPreferences',json(?1)) WHERE provider_id='openai'",
            [json!({"gpt-5-nano":{"reasoningEffort":"high"}}).to_string()]).unwrap();
        db.execute("UPDATE providers SET record=json_set(record,'$.modelPreferences',json(?1)) WHERE provider_id=?2",
            [json!({"gone":{"reasoningEffort":"high"}}).to_string(), REMOTE.into()]).unwrap();
    }
    let root = Ctx::root().unwrap();
    root.plugin(
        dim_app_server::methods::ManagementMethodsPlugin::typed(&dim_app_server::catalog())
            .unwrap(),
    );
    root.plugin(FileStoragePlugin::new(vec![home.clone()]));
    root.plugin(SqliteStoragePlugin::new(home.join("native.sqlite")));
    root.plugin(ManagedProvidersPlugin);
    root.plugin(dim_app_server::codex_oauth::OpenAICodexOAuthPlugin::new(&names(), 0).unwrap());
    root.plugin(dim_app_server::grok_oauth::GrokOAuthPlugin::new(&names(), 0).unwrap());
    root.plugin(dim_agent_aimux::AimuxPlugin);
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        active(&root.plugin(provider_part(ProviderPlugin::new))),
    )
    .await
    .unwrap();
    let reopened = root.get::<ProviderService>().unwrap();
    let upgraded = reopened
        .catalog_for("openai".into(), &cancel)
        .await
        .unwrap()
        .unwrap();
    assert!(upgraded.models.is_empty());
    assert_eq!(upgraded.version, builtin_empty.version + 1);
    assert!(
        reopened
            .list(&cancel)
            .await
            .unwrap()
            .providers
            .iter()
            .find(|p| p.provider_id == "openai")
            .unwrap()
            .enabled
    );
    assert_eq!(
        serde_json::to_value(
            reopened
                .catalog_for(REMOTE.into(), &cancel)
                .await
                .unwrap()
                .unwrap()
        )
        .unwrap(),
        serde_json::to_value(retained).unwrap(),
        "user models, routes, enables and descriptors survive restart"
    );
    let selected = reopened
        .configuration(
            Some(ModelId {
                provider_id: REMOTE.into(),
                model_id: "gone".into(),
            }),
            None,
            ModelPreferences::default(),
            &cancel,
        )
        .await
        .unwrap();
    assert!(
        selected.config.unwrap().reasoning.is_none(),
        "stored preferences are not applied implicitly"
    );
    let db = rusqlite::Connection::open(home.join("native.sqlite")).unwrap();
    let record: String = db
        .query_row(
            "SELECT record FROM providers WHERE provider_id='openai'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let record: Value = serde_json::from_str(&record).unwrap();
    assert_eq!(
        record["modelPreferences"],
        json!({"gpt-5-nano":{"reasoningEffort":"high"}}),
        "reseed preserves optional preference fields without introducing nulls"
    );
    drop(db);
    assert!(provider.refresh_for(REMOTE.into(), &cancel).await.is_err());
    root.shutdown().await.unwrap();
    server.abort();
    let _ = server.await;
    std::fs::remove_dir_all(home).unwrap();
}

/// A patch that changes a connection field follows the record's models: a
/// custom provider lists the new endpoint and stores that listing in the same
/// compare-and-set write, a failed listing fails the update and writes
/// nothing, a builtin keeps its bundled models without a request, and a
/// driver that cannot list saves the connection alone.
#[tokio::test]
async fn connection_updates_list_the_new_endpoint_and_keep_bundled_models() {
    use axum::{
        http::{StatusCode, Uri},
        routing::get,
    };
    use tokio::sync::{mpsc, oneshot};
    type Exchange = (Uri, oneshot::Sender<(StatusCode, Json<Value>)>);
    let (send, mut requests) = mpsc::channel::<Exchange>(8);
    let app = Router::new().route(
        "/{*path}",
        get(move |uri: Uri| {
            let send = send.clone();
            async move {
                let (reply, response) = oneshot::channel();
                send.send((uri, reply)).await.unwrap();
                response
                    .await
                    .unwrap_or((StatusCode::INTERNAL_SERVER_ERROR, Json(json!({}))))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let home = std::env::temp_dir().join(format!("provider-update-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&home).unwrap();
    let home = home.canonicalize().unwrap();
    let root = Ctx::root().unwrap();
    root.plugin(
        dim_app_server::methods::ManagementMethodsPlugin::typed(&dim_app_server::catalog())
            .unwrap(),
    );
    root.plugin(FileStoragePlugin::new(vec![home.clone()]));
    root.plugin(SqliteStoragePlugin::new(home.join("native.sqlite")));
    root.plugin(ManagedProvidersPlugin);
    root.plugin(dim_app_server::codex_oauth::OpenAICodexOAuthPlugin::new(&names(), 0).unwrap());
    root.plugin(dim_app_server::grok_oauth::GrokOAuthPlugin::new(&names(), 0).unwrap());
    root.plugin(dim_agent_aimux::AimuxPlugin);
    let view = root.plugin(provider_part(ProviderPlugin::new));
    tokio::time::timeout(std::time::Duration::from_secs(5), active(&view))
        .await
        .unwrap();
    let provider = root.get::<ProviderService>().unwrap();
    let cancel = CancellationToken::new();

    let custom = input(
        REMOTE,
        &format!("{base}/old/v1"),
        StaticAuth::Bearer,
        Some("first-key"),
        "first",
        31,
    );
    provider.create(custom, &cancel).await.unwrap();
    let moved = {
        let provider = provider.clone();
        let cancel = cancel.clone();
        let patch = serde_json::from_value(json!({"baseUrl":format!("{base}/new/v1")})).unwrap();
        tokio::spawn(async move { provider.update(REMOTE.into(), 1, patch, &cancel).await })
    };
    let (uri, reply) = requests.recv().await.unwrap();
    assert_eq!(uri.path(), "/new/v1/models");
    reply
        .send((
            StatusCode::OK,
            Json(json!({"data":[
                {"id":"new-model","context_window":64000,"max_output_tokens":512},
                {"id":"second-model","context_window":32000,"max_output_tokens":256}
            ]})),
        ))
        .unwrap();
    let updated = moved.await.unwrap().unwrap();
    assert_eq!(
        serde_json::to_value(&updated).unwrap()["modelsListed"],
        json!("listed")
    );
    assert_eq!(
        updated.version,
        Some(2),
        "the connection and its listing are one write"
    );
    let catalog = provider
        .catalog_for(REMOTE.into(), &cancel)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        catalog
            .models
            .iter()
            .map(|model| model.id.as_str())
            .collect::<Vec<_>>(),
        ["new-model", "second-model"]
    );

    let refused = {
        let provider = provider.clone();
        let cancel = cancel.clone();
        let patch = serde_json::from_value(json!({
            "baseUrl":format!("{base}/down/v1"),
            "credential":{"action":"set","value":"second-key"}
        }))
        .unwrap();
        tokio::spawn(async move { provider.update(REMOTE.into(), 2, patch, &cancel).await })
    };
    let (uri, reply) = requests.recv().await.unwrap();
    assert_eq!(uri.path(), "/down/v1/models");
    reply
        .send((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error":"down"})),
        ))
        .unwrap();
    assert!(refused.await.unwrap().is_err());
    let unchanged = provider
        .management(Some(REMOTE.into()), &cancel)
        .await
        .unwrap()
        .providers
        .remove(0);
    assert_eq!(unchanged.version, Some(2));
    assert_eq!(
        unchanged.connection.as_ref().unwrap().base_url,
        format!("{base}/new/v1")
    );
    assert_eq!(
        unchanged
            .models
            .as_ref()
            .unwrap()
            .iter()
            .map(|model| model.model.model_id.as_str())
            .collect::<Vec<_>>(),
        ["new-model", "second-model"]
    );

    // A network-only patch changes how the endpoint is reached, not what it
    // serves: it saves alone, without a listing.
    let timed = {
        let provider = provider.clone();
        let cancel = cancel.clone();
        let patch =
            serde_json::from_value(json!({"network":{"timeoutMs":{"action":"set","value":4000}}}))
                .unwrap();
        tokio::spawn(async move { provider.update(REMOTE.into(), 2, patch, &cancel).await })
    };
    let timed = tokio::time::timeout(std::time::Duration::from_secs(2), timed)
        .await
        .expect("a network-only patch saves without a listing")
        .unwrap()
        .unwrap();
    assert_eq!(timed.version, Some(3));
    assert_eq!(timed.connection.as_ref().unwrap().timeout_ms, Some(4000));
    assert_eq!(
        serde_json::to_value(&timed).unwrap().get("modelsListed"),
        None
    );
    assert!(
        requests.try_recv().is_err(),
        "a network-only patch sends no listing request"
    );

    // A kept credential is no change: it saves with the name and lists
    // nothing, and on its own it is the empty patch.
    let renamed = {
        let provider = provider.clone();
        let cancel = cancel.clone();
        let patch = serde_json::from_value(json!({
            "displayName":"Renamed remote",
            "credential":{"action":"keep"}
        }))
        .unwrap();
        tokio::spawn(async move { provider.update(REMOTE.into(), 3, patch, &cancel).await })
    };
    let renamed = tokio::time::timeout(std::time::Duration::from_secs(2), renamed)
        .await
        .expect("a kept credential saves without a listing")
        .unwrap()
        .unwrap();
    assert_eq!(renamed.display_name, "Renamed remote");
    assert_eq!(
        renamed.version,
        Some(4),
        "a name change with a kept credential saves alone"
    );
    assert_eq!(
        serde_json::to_value(&renamed).unwrap().get("modelsListed"),
        None
    );
    assert_eq!(
        renamed.connection.as_ref().unwrap().base_url,
        format!("{base}/new/v1")
    );
    assert!(
        requests.try_recv().is_err(),
        "a kept credential sends no listing request"
    );
    let kept = provider
        .update(
            REMOTE.into(),
            4,
            serde_json::from_value(json!({"credential":{"action":"keep"}})).unwrap(),
            &cancel,
        )
        .await
        .unwrap_err();
    assert!(
        matches!(&kept, ProviderError::InvalidConfig(reason) if reason.contains("must not be empty")),
        "a keep-only patch answers as the empty patch: {kept:?}"
    );
    assert!(
        requests.try_recv().is_err(),
        "a keep-only patch sends no listing request"
    );

    // An invalid patch is reported with its reason, not as a generic failure.
    let invalid = provider
        .update(
            REMOTE.into(),
            4,
            serde_json::from_value(json!({"baseUrl":"not a url"})).unwrap(),
            &cancel,
        )
        .await
        .unwrap_err();
    assert!(
        matches!(invalid, ProviderError::InvalidConfig(_)),
        "{invalid:?}"
    );
    let invalid = provider
        .update(
            REMOTE.into(),
            4,
            serde_json::from_value(json!({
                "credential":{"action":"set","value":"third-key"},
                "network":{"proxy":{"action":"set","value":"socks5://localhost:1080"}}
            }))
            .unwrap(),
            &cancel,
        )
        .await
        .unwrap_err();
    assert!(
        matches!(invalid, ProviderError::InvalidConfig(_)),
        "{invalid:?}"
    );
    let untouched = provider
        .management(Some(REMOTE.into()), &cancel)
        .await
        .unwrap()
        .providers
        .remove(0);
    assert_eq!(untouched.version, Some(4));
    assert_eq!(untouched.display_name, "Renamed remote");
    assert_eq!(
        untouched.connection.as_ref().unwrap().base_url,
        format!("{base}/new/v1")
    );

    let seeded = provider
        .management(Some("deepseek".into()), &cancel)
        .await
        .unwrap()
        .providers
        .remove(0);
    let bundled: Vec<String> = seeded
        .models
        .as_ref()
        .unwrap()
        .iter()
        .map(|model| model.model.model_id.clone())
        .collect();
    assert!(!bundled.is_empty());
    let keyed = provider
        .update(
            "deepseek".into(),
            seeded.version.unwrap(),
            serde_json::from_value(json!({
                "baseUrl":format!("{base}/bundled"),
                "credential":{"action":"set","value":"builtin-key"},
                "enabled":true
            }))
            .unwrap(),
            &cancel,
        )
        .await
        .unwrap();
    assert_eq!(
        serde_json::to_value(&keyed).unwrap()["modelsListed"],
        json!("bundled")
    );
    assert_eq!(
        keyed
            .models
            .as_ref()
            .unwrap()
            .iter()
            .map(|model| model.model.model_id.clone())
            .collect::<Vec<_>>(),
        bundled,
        "a builtin keeps the bundled catalog's models"
    );
    let repointed = provider
        .update(
            "deepseek".into(),
            keyed.version.unwrap(),
            serde_json::from_value(json!({"baseUrl":format!("{base}/bundled-2")})).unwrap(),
            &cancel,
        )
        .await
        .unwrap();
    assert_eq!(
        serde_json::to_value(&repointed).unwrap()["modelsListed"],
        json!("bundled")
    );
    assert!(
        requests.try_recv().is_err(),
        "no listing request reaches a builtin's endpoint"
    );

    let azure = {
        let mut azure = input(
            INVALID_AZURE,
            "https://fixture.services.ai.azure.com/openai/",
            StaticAuth::ApiKey {
                header_name: "api-key".into(),
                prefix: String::new(),
            },
            Some("azure-key"),
            "azure",
            11,
        );
        azure.connection.driver_kind = "azure-openai".into();
        azure
    };
    provider.create(azure, &cancel).await.unwrap();
    let saved = provider
        .update(
            INVALID_AZURE.into(),
            1,
            serde_json::from_value(json!({"credential":{"action":"set","value":"next-azure-key"}}))
                .unwrap(),
            &cancel,
        )
        .await
        .unwrap();
    assert_eq!(
        serde_json::to_value(&saved).unwrap()["modelsListed"],
        json!("unsupported")
    );
    assert!(saved.connection.as_ref().unwrap().credential_configured);
    assert!(requests.try_recv().is_err());

    root.shutdown().await.unwrap();
    server.abort();
    let _ = server.await;
    std::fs::remove_dir_all(home).unwrap();
}

/// A listing that collides with a user model fails the update and the refresh
/// with one message that names every colliding ID and the fix, writes nothing,
/// and the same update lists those IDs as defaults once they are deleted.
#[tokio::test]
async fn a_listing_that_collides_with_a_user_model_names_it_and_writes_nothing() {
    use axum::{
        http::{StatusCode, Uri},
        routing::get,
    };
    use tokio::sync::{mpsc, oneshot};
    type Exchange = (Uri, oneshot::Sender<(StatusCode, Json<Value>)>);
    let (send, mut requests) = mpsc::channel::<Exchange>(8);
    let app = Router::new().route(
        "/{*path}",
        get(move |uri: Uri| {
            let send = send.clone();
            async move {
                let (reply, response) = oneshot::channel();
                send.send((uri, reply)).await.unwrap();
                response
                    .await
                    .unwrap_or((StatusCode::INTERNAL_SERVER_ERROR, Json(json!({}))))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let home = std::env::temp_dir().join(format!("provider-conflict-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&home).unwrap();
    let home = home.canonicalize().unwrap();
    let root = Ctx::root().unwrap();
    root.plugin(
        dim_app_server::methods::ManagementMethodsPlugin::typed(&dim_app_server::catalog())
            .unwrap(),
    );
    root.plugin(FileStoragePlugin::new(vec![home.clone()]));
    root.plugin(SqliteStoragePlugin::new(home.join("native.sqlite")));
    root.plugin(ManagedProvidersPlugin);
    root.plugin(dim_app_server::codex_oauth::OpenAICodexOAuthPlugin::new(&names(), 0).unwrap());
    root.plugin(dim_app_server::grok_oauth::GrokOAuthPlugin::new(&names(), 0).unwrap());
    root.plugin(dim_agent_aimux::AimuxPlugin);
    let view = root.plugin(provider_part(ProviderPlugin::new));
    tokio::time::timeout(std::time::Duration::from_secs(5), active(&view))
        .await
        .unwrap();
    let provider = root.get::<ProviderService>().unwrap();
    let cancel = CancellationToken::new();

    provider
        .create(
            input(
                REMOTE,
                &format!("{base}/old/v1"),
                StaticAuth::Bearer,
                Some("first-key"),
                "first",
                31,
            ),
            &cancel,
        )
        .await
        .unwrap();
    let mut version = 1;
    for id in ["m", "n"] {
        version = provider
            .create_model(REMOTE.into(), version, model(id, 41), &cancel)
            .await
            .unwrap()
            .version;
    }
    let before: Vec<String> = provider
        .management(Some(REMOTE.into()), &cancel)
        .await
        .unwrap()
        .providers
        .remove(0)
        .models
        .unwrap()
        .iter()
        .map(|model| model.model.model_id.clone())
        .collect();
    assert_eq!(before, ["same", "m", "n"]);
    let moved = {
        let provider = provider.clone();
        let cancel = cancel.clone();
        let patch = serde_json::from_value(json!({"baseUrl":format!("{base}/new/v1")})).unwrap();
        tokio::spawn(async move {
            provider
                .update(REMOTE.into(), version, patch, &cancel)
                .await
        })
    };
    let (uri, reply) = requests.recv().await.unwrap();
    assert_eq!(uri.path(), "/new/v1/models");
    reply
        .send((
            StatusCode::OK,
            Json(json!({"data":[
                {"id":"m","context_window":64000,"max_output_tokens":512},
                {"id":"n","context_window":64000,"max_output_tokens":512},
                {"id":"fresh","context_window":64000,"max_output_tokens":512}
            ]})),
        ))
        .unwrap();
    let conflict = moved.await.unwrap().unwrap_err();
    assert_eq!(
        conflict.to_string(),
        "Discovered models conflict with user-added models: m, n. Delete them through provider.models.delete, then retry."
    );
    let unchanged = provider
        .management(Some(REMOTE.into()), &cancel)
        .await
        .unwrap()
        .providers
        .remove(0);
    assert_eq!(unchanged.version, Some(version));
    assert_eq!(
        unchanged.connection.as_ref().unwrap().base_url,
        format!("{base}/old/v1")
    );
    assert_eq!(
        unchanged
            .models
            .as_ref()
            .unwrap()
            .iter()
            .map(|model| model.model.model_id.clone())
            .collect::<Vec<_>>(),
        before,
        "a failed listing replaces nothing"
    );

    version = provider
        .delete_model(REMOTE.into(), version, "m".into(), &cancel)
        .await
        .unwrap()
        .version;
    version = provider
        .delete_model(REMOTE.into(), version, "n".into(), &cancel)
        .await
        .unwrap()
        .version;
    let moved = {
        let provider = provider.clone();
        let cancel = cancel.clone();
        let patch = serde_json::from_value(json!({"baseUrl":format!("{base}/new/v1")})).unwrap();
        tokio::spawn(async move {
            provider
                .update(REMOTE.into(), version, patch, &cancel)
                .await
        })
    };
    let (uri, reply) = requests.recv().await.unwrap();
    assert_eq!(uri.path(), "/new/v1/models");
    reply
        .send((
            StatusCode::OK,
            Json(json!({"data":[
                {"id":"m","context_window":64000,"max_output_tokens":512},
                {"id":"n","context_window":64000,"max_output_tokens":512},
                {"id":"fresh","context_window":64000,"max_output_tokens":512}
            ]})),
        ))
        .unwrap();
    let updated = moved.await.unwrap().unwrap();
    assert_eq!(
        serde_json::to_value(&updated).unwrap()["modelsListed"],
        json!("listed")
    );
    let catalog = provider
        .catalog_for(REMOTE.into(), &cancel)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        catalog
            .models
            .iter()
            .map(|model| model.id.as_str())
            .collect::<Vec<_>>(),
        ["m", "n", "fresh"],
        "the deleted user models are the endpoint's defaults now"
    );

    // The refresh path answers the same message.
    version = provider
        .create_model(REMOTE.into(), catalog.version, model("k", 51), &cancel)
        .await
        .unwrap()
        .version;
    let refresh = {
        let provider = provider.clone();
        let cancel = cancel.clone();
        tokio::spawn(async move { provider.refresh_for(REMOTE.into(), &cancel).await })
    };
    let (uri, reply) = requests.recv().await.unwrap();
    assert_eq!(uri.path(), "/new/v1/models");
    reply
        .send((
            StatusCode::OK,
            Json(json!({"data":[
                {"id":"k","context_window":64000,"max_output_tokens":512}
            ]})),
        ))
        .unwrap();
    let conflict = refresh.await.unwrap().unwrap_err();
    assert_eq!(
        conflict.to_string(),
        "Discovered models conflict with user-added models: k. Delete them through provider.models.delete, then retry."
    );
    let unchanged = provider
        .catalog_for(REMOTE.into(), &cancel)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(unchanged.version, version);
    assert_eq!(
        unchanged
            .models
            .iter()
            .map(|model| model.id.as_str())
            .collect::<Vec<_>>(),
        ["m", "n", "fresh", "k"],
        "a failed refresh replaces nothing"
    );

    version = provider
        .delete_model(REMOTE.into(), version, "k".into(), &cancel)
        .await
        .unwrap()
        .version;
    let refresh = {
        let provider = provider.clone();
        let cancel = cancel.clone();
        tokio::spawn(async move { provider.refresh_for(REMOTE.into(), &cancel).await })
    };
    let (uri, reply) = requests.recv().await.unwrap();
    assert_eq!(uri.path(), "/new/v1/models");
    reply
        .send((
            StatusCode::OK,
            Json(json!({"data":[
                {"id":"k","context_window":64000,"max_output_tokens":512}
            ]})),
        ))
        .unwrap();
    let refreshed = refresh.await.unwrap().unwrap();
    assert_eq!(refreshed.version, version + 1);
    assert_eq!(
        refreshed
            .models
            .iter()
            .map(|model| model.id.as_str())
            .collect::<Vec<_>>(),
        ["k"]
    );

    root.shutdown().await.unwrap();
    server.abort();
    let _ = server.await;
    std::fs::remove_dir_all(home).unwrap();
}

/// A connection change that lands on a disabled record drops the record's own
/// models in the same write, without a request, so enabling it leaves an empty
/// listable catalog that the picker's refresh fills from the new endpoint. A
/// bundled catalog and a driver that cannot list stay as they are.
#[tokio::test]
async fn a_disabled_re_point_drops_an_own_catalog_and_the_refill_lists_the_new_endpoint() {
    use axum::{
        http::{StatusCode, Uri},
        routing::get,
    };
    use tokio::sync::{mpsc, oneshot};
    type Exchange = (Uri, oneshot::Sender<(StatusCode, Json<Value>)>);
    let (send, mut requests) = mpsc::channel::<Exchange>(8);
    let app = Router::new().route(
        "/{*path}",
        get(move |uri: Uri| {
            let send = send.clone();
            async move {
                let (reply, response) = oneshot::channel();
                send.send((uri, reply)).await.unwrap();
                response
                    .await
                    .unwrap_or((StatusCode::INTERNAL_SERVER_ERROR, Json(json!({}))))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let home = std::env::temp_dir().join(format!("provider-disabled-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&home).unwrap();
    let home = home.canonicalize().unwrap();
    let root = Ctx::root().unwrap();
    root.plugin(
        dim_app_server::methods::ManagementMethodsPlugin::typed(&dim_app_server::catalog())
            .unwrap(),
    );
    root.plugin(FileStoragePlugin::new(vec![home.clone()]));
    root.plugin(SqliteStoragePlugin::new(home.join("native.sqlite")));
    root.plugin(ManagedProvidersPlugin);
    root.plugin(dim_app_server::codex_oauth::OpenAICodexOAuthPlugin::new(&names(), 0).unwrap());
    root.plugin(dim_app_server::grok_oauth::GrokOAuthPlugin::new(&names(), 0).unwrap());
    root.plugin(dim_agent_aimux::AimuxPlugin);
    let view = root.plugin(provider_part(ProviderPlugin::new));
    tokio::time::timeout(std::time::Duration::from_secs(5), active(&view))
        .await
        .unwrap();
    let provider = root.get::<ProviderService>().unwrap();
    let cancel = CancellationToken::new();

    let mut create = input(
        REMOTE,
        &format!("{base}/old/v1"),
        StaticAuth::Bearer,
        Some("first-key"),
        "first",
        21,
    );
    create.models = vec![model("old", 21)];
    provider.create(create, &cancel).await.unwrap();
    let disabled = provider
        .update(
            REMOTE.into(),
            1,
            serde_json::from_value(json!({
                "enabled":false,
                "baseUrl":format!("{base}/new/v1")
            }))
            .unwrap(),
            &cancel,
        )
        .await
        .unwrap();
    assert_eq!(
        serde_json::to_value(&disabled).unwrap()["modelsListed"],
        json!("disabled")
    );
    assert_eq!(disabled.version, Some(2));
    assert_eq!(
        disabled.connection.as_ref().unwrap().base_url,
        format!("{base}/new/v1")
    );
    assert!(
        disabled.models.as_ref().unwrap().is_empty(),
        "the old endpoint's models are dropped"
    );
    assert!(
        requests.try_recv().is_err(),
        "a disabled re-point sends no listing request"
    );

    let enabled = provider
        .update(
            REMOTE.into(),
            2,
            serde_json::from_value(json!({"enabled":true})).unwrap(),
            &cancel,
        )
        .await
        .unwrap();
    assert_eq!(enabled.version, Some(3));
    assert_eq!(
        serde_json::to_value(&enabled).unwrap().get("modelsListed"),
        None
    );
    assert!(enabled.models.as_ref().unwrap().is_empty());

    // What the picker's refill sends: `provider.refresh` lists the new
    // endpoint and stores it.
    let refresh = {
        let provider = provider.clone();
        let cancel = cancel.clone();
        tokio::spawn(async move { provider.refresh_for(REMOTE.into(), &cancel).await })
    };
    let (uri, reply) = requests.recv().await.unwrap();
    assert_eq!(uri.path(), "/new/v1/models");
    reply
        .send((
            StatusCode::OK,
            Json(json!({"data":[
                {"id":"new-model","context_window":64000,"max_output_tokens":512}
            ]})),
        ))
        .unwrap();
    let refreshed = refresh.await.unwrap().unwrap();
    assert_eq!(
        refreshed
            .models
            .iter()
            .map(|model| model.id.as_str())
            .collect::<Vec<_>>(),
        ["new-model"],
        "the old endpoint's model is gone and the new one is listed"
    );

    // A builtin's models are the bundled catalog's, not the endpoint's.
    let seeded = provider
        .management(Some("deepseek".into()), &cancel)
        .await
        .unwrap()
        .providers
        .remove(0);
    let bundled: Vec<String> = seeded
        .models
        .as_ref()
        .unwrap()
        .iter()
        .map(|model| model.model.model_id.clone())
        .collect();
    let kept = provider
        .update(
            "deepseek".into(),
            seeded.version.unwrap(),
            serde_json::from_value(json!({
                "enabled":false,
                "credential":{"action":"set","value":"builtin-key"}
            }))
            .unwrap(),
            &cancel,
        )
        .await
        .unwrap();
    assert_eq!(
        serde_json::to_value(&kept).unwrap()["modelsListed"],
        json!("disabled")
    );
    assert_eq!(
        kept.models
            .as_ref()
            .unwrap()
            .iter()
            .map(|model| model.model.model_id.clone())
            .collect::<Vec<_>>(),
        bundled,
        "a disabled re-point keeps a builtin's bundled models"
    );
    assert!(requests.try_recv().is_err());

    // A driver that cannot list keeps what the user declared.
    let mut azure = input(
        INVALID_AZURE,
        "https://fixture.services.ai.azure.com/openai/",
        StaticAuth::ApiKey {
            header_name: "api-key".into(),
            prefix: String::new(),
        },
        Some("azure-key"),
        "azure",
        11,
    );
    azure.connection.driver_kind = "azure-openai".into();
    provider.create(azure, &cancel).await.unwrap();
    let azure = provider
        .update(
            INVALID_AZURE.into(),
            1,
            serde_json::from_value(json!({
                "enabled":false,
                "credential":{"action":"set","value":"next-azure-key"}
            }))
            .unwrap(),
            &cancel,
        )
        .await
        .unwrap();
    assert_eq!(
        serde_json::to_value(&azure).unwrap()["modelsListed"],
        json!("disabled")
    );
    assert_eq!(
        azure
            .models
            .as_ref()
            .unwrap()
            .iter()
            .map(|model| model.model.model_id.as_str())
            .collect::<Vec<_>>(),
        ["same"],
        "an unlistable record keeps its user-declared models"
    );
    assert!(requests.try_recv().is_err());

    root.shutdown().await.unwrap();
    server.abort();
    let _ = server.await;
    std::fs::remove_dir_all(home).unwrap();
}

/// A listing item's `type: "model"` names the object, not a model kind: an
/// OpenAI-compatible provider lists those models available and a Session
/// adopts one, an already stored catalog keeps them available without a
/// refresh, and an `embedding` item stays unavailable.
#[tokio::test]
async fn object_kind_listings_classify_their_models_as_chat() {
    use axum::{
        http::{StatusCode, Uri},
        routing::get,
    };
    use tokio::sync::{mpsc, oneshot};
    type Exchange = (Uri, oneshot::Sender<(StatusCode, Json<Value>)>);
    let (send, mut requests) = mpsc::channel::<Exchange>(8);
    let app = Router::new().route(
        "/{*path}",
        get(move |uri: Uri| {
            let send = send.clone();
            async move {
                let (reply, response) = oneshot::channel();
                send.send((uri, reply)).await.unwrap();
                response
                    .await
                    .unwrap_or((StatusCode::INTERNAL_SERVER_ERROR, Json(json!({}))))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let home = std::env::temp_dir().join(format!("provider-kind-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&home).unwrap();
    let home = home.canonicalize().unwrap();
    let root = Ctx::root().unwrap();
    root.plugin(
        dim_app_server::methods::ManagementMethodsPlugin::typed(&dim_app_server::catalog())
            .unwrap(),
    );
    root.plugin(FileStoragePlugin::new(vec![home.clone()]));
    root.plugin(SqliteStoragePlugin::new(home.join("native.sqlite")));
    root.plugin(ManagedProvidersPlugin);
    root.plugin(dim_app_server::codex_oauth::OpenAICodexOAuthPlugin::new(&names(), 0).unwrap());
    root.plugin(dim_app_server::grok_oauth::GrokOAuthPlugin::new(&names(), 0).unwrap());
    root.plugin(dim_agent_aimux::AimuxPlugin);
    let view = root.plugin(provider_part(ProviderPlugin::new));
    tokio::time::timeout(std::time::Duration::from_secs(5), active(&view))
        .await
        .unwrap();
    let provider = root.get::<ProviderService>().unwrap();
    let cancel = CancellationToken::new();
    let mut config = input(
        REMOTE_KIND,
        &format!("{base}/v1"),
        StaticAuth::Bearer,
        Some("kind-key"),
        "kind",
        17,
    );
    config.models = Vec::new();
    provider.create(config, &cancel).await.unwrap();
    let listed = provider
        .list(&cancel)
        .await
        .unwrap()
        .providers
        .into_iter()
        .find(|entry| entry.provider_id == REMOTE_KIND)
        .unwrap();
    assert!(
        listed.remote_model_list,
        "provider.list says whether provider.refresh can list a provider's models"
    );
    let refresh = {
        let provider = provider.clone();
        let cancel = cancel.clone();
        tokio::spawn(async move { provider.refresh_for(REMOTE_KIND.into(), &cancel).await })
    };
    let (uri, reply) = requests.recv().await.unwrap();
    assert_eq!(uri.path(), "/v1/models");
    reply
        .send((
            StatusCode::OK,
            Json(json!({"data":[
                {"id":"vendor/model-a","object":"model","type":"model","context_window":64000,"max_output_tokens":512},
                {"id":"vendor/embed-a","object":"model","type":"embedding","context_window":8000}
            ]})),
        ))
        .unwrap();
    let catalog = refresh.await.unwrap().unwrap();
    let listed = catalog
        .models
        .iter()
        .find(|model| model.id == "vendor/model-a")
        .unwrap();
    assert!(
        listed.unavailable_reason.is_none(),
        "`type: \"model\"` names the listing object, not a model kind"
    );
    assert!(
        catalog
            .models
            .iter()
            .find(|model| model.id == "vendor/embed-a")
            .unwrap()
            .unavailable_reason
            .is_some()
    );
    provider
        .configuration(
            Some(ModelId {
                provider_id: REMOTE_KIND.into(),
                model_id: "vendor/model-a".into(),
            }),
            None,
            ModelPreferences::default(),
            &cancel,
        )
        .await
        .unwrap();

    let mut stored = input(
        STORED_KIND,
        &format!("{base}/stored/v1"),
        StaticAuth::Anonymous,
        None,
        "stored",
        23,
    );
    stored.models[0].metadata =
        Some(serde_json::from_value(json!({"raw":{"type":"model"}})).unwrap());
    stored.models.push(model("same-embedding", 29));
    stored.models[1].metadata =
        Some(serde_json::from_value(json!({"raw":{"type":"embedding"}})).unwrap());
    provider.create(stored, &cancel).await.unwrap();
    let catalog = provider
        .catalog_for(STORED_KIND.into(), &cancel)
        .await
        .unwrap()
        .unwrap();
    assert!(
        catalog
            .models
            .iter()
            .find(|model| model.id == "same")
            .unwrap()
            .unavailable_reason
            .is_none(),
        "a stored catalog keeps `type: \"model\"` models available with no refresh"
    );
    assert!(
        catalog
            .models
            .iter()
            .find(|model| model.id == "same-embedding")
            .unwrap()
            .unavailable_reason
            .is_some()
    );
    provider
        .configuration(
            Some(ModelId {
                provider_id: STORED_KIND.into(),
                model_id: "same".into(),
            }),
            None,
            ModelPreferences::default(),
            &cancel,
        )
        .await
        .unwrap();

    root.shutdown().await.unwrap();
    server.abort();
    let _ = server.await;
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test]
async fn vendor_routes_agree_across_selection_and_actual_http_execution() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = Router::new()
        .route("/{*path}", post(completion))
        .with_state(seen.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let home = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target/vendor-route-flow")
        .join(uuid::Uuid::new_v4().to_string());
    std::fs::create_dir_all(&home).unwrap();
    let root = Ctx::root().unwrap();
    root.plugin(
        dim_app_server::methods::ManagementMethodsPlugin::typed(&dim_app_server::catalog())
            .unwrap(),
    );
    root.plugin(FileStoragePlugin::new(vec![home.clone()]));
    root.plugin(SqliteStoragePlugin::new(home.join("native.sqlite")));
    root.plugin(ManagedProvidersPlugin);
    root.plugin(dim_app_server::codex_oauth::OpenAICodexOAuthPlugin::new(&names(), 0).unwrap());
    root.plugin(dim_app_server::grok_oauth::GrokOAuthPlugin::new(&names(), 0).unwrap());
    active(&root.plugin(dim_agent_aimux::AimuxPlugin)).await;
    active(&root.plugin(provider_part(ProviderPlugin::new))).await;
    let provider = root.get::<ProviderService>().unwrap();
    let resolver = root.get::<ProviderConnectionResolver>().unwrap();
    let host = root.get::<ModelHost>().unwrap();
    let cancel = CancellationToken::new();
    for (index, (driver, model_id, suffix, max_field, app_code)) in [
        (
            "custom",
            "plain",
            "/v1/chat/completions",
            "max_tokens",
            None,
        ),
        (
            "deepseek",
            "deepseek-v4-pro",
            "/v1/chat/completions",
            "max_tokens",
            None,
        ),
        (
            "moonshotai",
            "kimi-k2.5",
            "/v1/chat/completions",
            "max_completion_tokens",
            None,
        ),
        (
            "aihubmix",
            "plain",
            "/v1/chat/completions",
            "max_tokens",
            Some("WHVL9885"),
        ),
        (
            "aihubmix",
            "anthropic/claude-test",
            "/v1/responses",
            "max_output_tokens",
            Some("WHVL9885"),
        ),
        (
            "aihubmix-responses",
            "plain",
            "/v1/responses",
            "max_output_tokens",
            Some("custom-code"),
        ),
        (
            "aihubmix",
            "custom-header",
            "/v1/chat/completions",
            "max_tokens",
            Some("fixture-secret"),
        ),
        (
            "zenmux",
            "plain",
            "/v1/chat/completions",
            "max_tokens",
            None,
        ),
        (
            "zenmux",
            "anthropic/claude-test",
            "/anthropic/v1/messages",
            "max_tokens",
            None,
        ),
        (
            "azure-openai",
            "deployment-alias",
            "/openai/v1/responses",
            "max_output_tokens",
            None,
        ),
        (
            "xai",
            "grok-alias",
            "/v1/chat/completions",
            "max_completion_tokens",
            None,
        ),
        (
            "xai-responses",
            "grok-alias",
            "/v1/responses",
            "max_output_tokens",
            None,
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let id = format!("00000000-0000-4000-9000-{index:012}");
        let base = format!("http://{address}/{id}/v1/");
        let mut input = input(
            &id,
            &base,
            StaticAuth::Bearer,
            Some("fixture-secret"),
            "kept",
            1357,
        );
        input.connection.driver_kind = driver.into();
        let azure = driver == "azure-openai";
        if azure {
            input.connection.base_url = "https://fixture.services.ai.azure.com/openai/".into();
            input.connection.auth = StaticAuth::ApiKey {
                header_name: "api-key".into(),
                prefix: String::new(),
            };
        }
        let mut entry = model(model_id, 1357);
        entry.capabilities.reasoning = true;
        entry.metadata = Some(
            serde_json::from_value(json!({
                "reasoning":{"supported":true,"effortOptions":["high","low"]},
                "serviceTier":{"supported":true,"options":["priority"]}
            }))
            .unwrap(),
        );
        input.models = vec![entry];
        if app_code == Some("custom-code") {
            input
                .connection
                .headers
                .as_mut()
                .unwrap()
                .insert("app-code".into(), "custom-code".into());
        }
        if model_id == "custom-header" {
            input.connection.auth = StaticAuth::CustomHeader {
                header_name: "app-code".into(),
            };
            input
                .connection
                .headers
                .as_mut()
                .unwrap()
                .insert("Authorization".into(), "Bearer fixture-secret".into());
        }
        provider.create(input, &cancel).await.unwrap();
        let target = ModelId {
            provider_id: id.clone(),
            model_id: model_id.into(),
        };
        let anthropic = suffix.ends_with("/messages");
        let xai = matches!(driver, "xai" | "xai-responses");
        if xai {
            assert!(
                provider
                    .configuration(
                        Some(target.clone()),
                        None,
                        ModelPreferences {
                            service_tier: Some("priority".into()),
                            ..Default::default()
                        },
                        &cancel
                    )
                    .await
                    .is_err()
            );
        }
        let selection = provider
            .configuration(
                Some(target),
                None,
                ModelPreferences {
                    reasoning_effort: Some("high".into()),
                    service_tier: (!anthropic && !xai).then(|| "priority".into()),
                },
                &cancel,
            )
            .await
            .unwrap();
        if index == 0 {
            let unchanged = provider
                .apply_preferences(
                    &dim_agent_types::ModelConfiguration {
                        provider_id: selection.provider_id.clone(),
                        model_id: selection.model_id.clone(),
                        capability_snapshot: selection.capability_snapshot.clone(),
                        config: None,
                        metadata: None,
                        tools: None,
                        tool_sources: vec![],
                    },
                    ModelPreferences::default(),
                    &cancel,
                )
                .await
                .unwrap();
            assert!(unchanged.config.is_none());
            assert!(unchanged.metadata.is_none());
        }
        let selection = provider
            .apply_preferences(
                &dim_agent_types::ModelConfiguration {
                    provider_id: selection.provider_id.clone(),
                    model_id: selection.model_id.clone(),
                    capability_snapshot: selection.capability_snapshot.clone(),
                    config: selection.config.clone(),
                    metadata: selection.metadata.clone(),
                    tools: None,
                    tool_sources: vec![],
                },
                ModelPreferences {
                    reasoning_effort: Some("low".into()),
                    ..Default::default()
                },
                &cancel,
            )
            .await
            .unwrap();
        let mut request =
            ModelRequest::new(&id, model_id, vec![Message::text(Role::User, "hello")]);
        request.capability_snapshot = selection.capability_snapshot;
        request.config = selection.config;
        let request = Arc::new(request);
        let mut resolved = resolver
            .0
            .resolve(request.clone(), cancel.clone())
            .await
            .unwrap();
        let ModelConnection::Http(connection) = &mut resolved.connection else {
            panic!("HTTP expected")
        };
        if azure {
            assert_eq!(
                connection.base_url,
                "https://fixture.services.ai.azure.com/openai/v1"
            );
            connection.base_url = format!("http://{address}/{id}/openai/v1");
        }
        assert_eq!(
            connection
                .headers
                .keys()
                .filter(|k| k.eq_ignore_ascii_case("app-code"))
                .count(),
            usize::from(app_code.is_some())
        );
        let mut output = host
            .0
            .invoke(request, resolved.connection, cancel.clone())
            .await
            .unwrap();
        let mut text = String::new();
        let mut stopped = false;
        while let Some(chunk) = output.next().await {
            match chunk.unwrap() {
                ModelChunk::Text(value) => text.push_str(&value),
                ModelChunk::MessageStop { .. } => stopped = true,
                _ => {}
            }
        }
        assert_eq!(text, "ok", "{driver}/{model_id}");
        assert!(stopped);
        let requests = seen.lock().unwrap();
        assert_eq!(requests.len(), index + 1);
        let sent = requests.last().unwrap();
        assert_eq!(sent.path, format!("/{id}{suffix}"));
        assert_eq!(
            sent.authorization.as_deref(),
            (!azure).then_some("Bearer fixture-secret")
        );
        assert_eq!(sent.api_key.as_deref(), azure.then_some("fixture-secret"));
        assert_eq!(sent.marker.as_deref(), Some("kept"));
        assert_eq!(sent.app_code.as_deref(), app_code);
        assert_eq!(sent.body["model"], model_id);
        assert_eq!(sent.body[max_field], 1357, "{driver}/{model_id}");
        for other in ["max_tokens", "max_completion_tokens", "max_output_tokens"] {
            if other != max_field {
                assert!(sent.body.get(other).is_none());
            }
        }
        if anthropic {
            assert_eq!(sent.body["output_config"]["effort"], "low");
        } else if suffix.ends_with("/responses") {
            assert_eq!(sent.body["reasoning"]["effort"], "low");
            assert_eq!(
                sent.body.get("service_tier").and_then(Value::as_str),
                (!xai).then_some("priority")
            );
            if xai {
                assert_eq!(sent.body["store"], false);
                assert_eq!(sent.body["include"], json!(["reasoning.encrypted_content"]));
            }
        } else {
            assert_eq!(sent.body["reasoning_effort"], "low");
            assert_eq!(
                sent.body.get("service_tier").and_then(Value::as_str),
                (!xai).then_some("priority")
            );
        }
    }
    for base in [
        "http://fixture.openai.azure.com/openai",
        "https://fixture.openai.azure.com.evil.test/openai",
        "https://fixture.openai.azure.com/",
        "https://fixture.openai.azure.com/openai/v1",
        "https://fixture.openai.azure.com/openai?api-version=v1",
    ] {
        let mut invalid = input(
            INVALID_AZURE,
            base,
            StaticAuth::Bearer,
            Some("key"),
            "",
            1357,
        );
        invalid.connection.driver_kind = "azure-openai".into();
        assert!(provider.create(invalid, &cancel).await.is_err(), "{base}");
    }
    let azure = provider
        .list(&cancel)
        .await
        .unwrap()
        .providers
        .into_iter()
        .find(|p| p.provider_id == "azure-openai")
        .unwrap();
    assert_eq!(
        azure.auth,
        StaticAuth::ApiKey {
            header_name: "Authorization".into(),
            prefix: "Bearer ".into()
        }
    );
    root.shutdown().await.unwrap();
    server.abort();
    let _ = server.await;
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test]
async fn new_api_metadata_routes_and_builtin_routes_survive_restart() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = Router::new()
        .route("/{*path}", post(completion))
        .with_state(seen.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let home = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target/new-api-flow")
        .join(uuid::Uuid::new_v4().to_string());
    std::fs::create_dir_all(&home).unwrap();
    let mut saved_version = 0;
    for restart in [false, true] {
        let root = Ctx::root().unwrap();
        root.plugin(
            dim_app_server::methods::ManagementMethodsPlugin::typed(&dim_app_server::catalog())
                .unwrap(),
        );
        root.plugin(FileStoragePlugin::new(vec![home.clone()]));
        root.plugin(SqliteStoragePlugin::new(home.join("native.sqlite")));
        root.plugin(ManagedProvidersPlugin);
        root.plugin(dim_app_server::codex_oauth::OpenAICodexOAuthPlugin::new(&names(), 0).unwrap());
        root.plugin(dim_app_server::grok_oauth::GrokOAuthPlugin::new(&names(), 0).unwrap());
        active(&root.plugin(dim_agent_aimux::AimuxPlugin)).await;
        active(&root.plugin(provider_part(ProviderPlugin::new))).await;
        let provider = root.get::<ProviderService>().unwrap();
        let resolver = root.get::<ProviderConnectionResolver>().unwrap();
        let host = root.get::<ModelHost>().unwrap();
        let cancel = CancellationToken::new();
        for (index, (model_id, raw, suffix)) in [
            (
                "claude-explicit",
                json!({"endpointType":"openai-response"}),
                Some("/v1/responses"),
            ),
            (
                "by-owner",
                json!({"ownedBy":"Google"}),
                Some("/v1beta/models/by-owner:streamGenerateContent"),
            ),
            ("claude-inferred", json!({}), Some("/v1/messages")),
            (
                "claude-limited",
                json!({"supportedEndpointTypes":["openai","openai-response"]}),
                Some("/v1/chat/completions"),
            ),
            (
                "only-one",
                json!({"supportedEndpointTypes":["future-kind","anthropic","anthropic"]}),
                Some("/v1/messages"),
            ),
            (
                "type-hint",
                json!({"type":"openai-response"}),
                Some("/v1/responses"),
            ),
            (
                "snake",
                json!({"endpoint_type":"anthropic"}),
                Some("/v1/messages"),
            ),
            (
                "claude-empty",
                json!({"supportedEndpointTypes":[]}),
                Some("/v1/messages"),
            ),
            ("superclaude", json!({}), Some("/v1/chat/completions")),
            (
                "selectable",
                json!({"supportedEndpointTypes":["anthropic","gemini"],"selectableEndpointTypes":["openai-response"]}),
                Some("/v1/responses"),
            ),
            (
                "ambiguous",
                json!({"supportedEndpointTypes":["anthropic","gemini"]}),
                None,
            ),
            (
                "future-only",
                json!({"supportedEndpointTypes":["future-kind"]}),
                None,
            ),
            ("video", json!({"endpointType":"video-generation"}), None),
            (
                "compact",
                json!({"endpointType":"openai-response-compact"}),
                None,
            ),
        ].into_iter().enumerate() {
            let id = format!("00000000-0000-4000-a000-{index:012}");
            if !restart {
                let mut entry = model(model_id, 765);
                entry.metadata = Some(serde_json::from_value(json!({"raw":raw})).unwrap());
                let mut input = input(
                    &id,
                    &format!("http://{address}/prefix/V1beta/"),
                    StaticAuth::Bearer,
                    Some("router-key"),
                    "router",
                    765,
                );
                input.connection.driver_kind = "new-api-router".into();
                input.models = vec![entry];
                provider.create(input, &cancel).await.unwrap();
            }
            let target = ModelId {
                provider_id: id.clone(),
                model_id: model_id.into(),
            };
            let catalog = provider
                .catalog_for(id.clone(), &cancel)
                .await
                .unwrap()
                .unwrap();
            let selection = provider
                .configuration(Some(target), None, ModelPreferences::default(), &cancel)
                .await;
            let Some(suffix) = suffix else {
                assert!(catalog.models[0].unavailable_reason.is_some(), "{model_id}");
                assert!(selection.is_err(), "{model_id}");
                continue;
            };
            assert!(catalog.models[0].unavailable_reason.is_none(), "{model_id}");
            let selection = selection.unwrap();
            assert_eq!(
                selection.metadata.as_ref().unwrap().extensions["providerProtocol"],
                json!(catalog.models[0].endpoint.unwrap())
            );
            let mut request =
                ModelRequest::new(&id, model_id, vec![Message::text(Role::User, "hello")]);
            request.capability_snapshot = selection.capability_snapshot;
            request.config = selection.config;
            request.metadata = selection.metadata;
            let request = Arc::new(request);
            let resolved = resolver
                .0
                .resolve(request.clone(), cancel.clone())
                .await
                .unwrap();
            let mut stream = host
                .0
                .invoke(request.clone(), resolved.connection, cancel.clone())
                .await
                .unwrap();
            let mut text = String::new();
            let mut stopped = false;
            while let Some(chunk) = stream.next().await {
                match chunk.unwrap() {
                    ModelChunk::Text(value) => text.push_str(&value),
                    ModelChunk::MessageStop { .. } => stopped = true,
                    _ => {}
                }
            }
            assert_eq!(text, "ok");
            assert!(stopped);
            let sent = seen.lock().unwrap().last().unwrap().clone();
            assert_eq!(sent.path, format!("/prefix{suffix}"));
            assert_eq!(sent.authorization.as_deref(), Some("Bearer router-key"));
            if !restart && model_id == "claude-explicit" {
                provider
                    .select(
                        Some(&home),
                        ModelId {
                            provider_id: id.clone(),
                            model_id: model_id.into(),
                        },
                        None,
                        &cancel,
                    )
                    .await
                    .unwrap();
                let default = provider
                    .configuration(None, Some(&home), ModelPreferences::default(), &cancel)
                    .await
                    .unwrap();
                assert_eq!(
                    default.metadata, request.metadata,
                    "default selection stamps the chosen route too"
                );
                let changed = provider
                    .update_model_config(
                        id.clone(),
                        catalog.version,
                        model_id.into(),
                        Some(
                            serde_json::from_value(
                                json!({"endpoint":{"action":"set","value":"anthropic"}}),
                            )
                            .unwrap(),
                        ),
                        &cancel,
                    )
                    .await
                    .unwrap();
                assert!(
                    matches!(
                        provider
                            .update_model_config(
                                id.clone(),
                                changed.version,
                                model_id.into(),
                                None,
                                &cancel
                            )
                            .await,
                        Err(dim_app_server::provider::ProviderError::NotFound)
                    ),
                    "a route-only edit does not create a descriptor override"
                );
                assert!(matches!(
                    resolver.0.resolve(request.clone(), cancel.clone()).await,
                    Err(dim_agent_capabilities::host::HostError::Failed {
                        kind: dim_agent_capabilities::host::HostErrorKind::InvalidRequest,
                        ..
                    })
                ));
                let mut current = dim_agent_types::ModelConfiguration {
                    provider_id: id.clone(),
                    model_id: model_id.into(),
                    capability_snapshot: request.capability_snapshot.clone(),
                    config: request.config.clone(),
                    metadata: request.metadata.clone(),
                    tools: None,
                    tool_sources: vec![],
                };
                assert!(
                    provider
                        .apply_preferences(&current, ModelPreferences::default(), &cancel)
                        .await
                        .is_err()
                );
                current.metadata = None;
                let unstamped = provider
                    .apply_preferences(&current, ModelPreferences::default(), &cancel)
                    .await
                    .unwrap();
                assert!(
                    unstamped.metadata.is_none(),
                    "untargeted preferences do not stamp legacy configurations"
                );
                let mut unstamped_request = (*request).clone();
                unstamped_request.metadata = None;
                assert!(
                    resolver
                        .0
                        .resolve(Arc::new(unstamped_request), cancel.clone())
                        .await
                        .is_ok()
                );
                let selected = provider
                    .configuration(
                        Some(ModelId {
                            provider_id: id.clone(),
                            model_id: model_id.into(),
                        }),
                        None,
                        ModelPreferences::default(),
                        &cancel,
                    )
                    .await
                    .unwrap();
                assert_eq!(
                    selected.metadata.as_ref().unwrap().extensions["providerProtocol"],
                    json!("anthropic")
                );
                let mut selected_request = (*request).clone();
                selected_request.metadata = selected.metadata.clone();
                assert!(
                    resolver
                        .0
                        .resolve(Arc::new(selected_request), cancel.clone())
                        .await
                        .is_ok()
                );
                current.metadata = selected.metadata;
                assert!(
                    provider
                        .apply_preferences(&current, ModelPreferences::default(), &cancel)
                        .await
                        .is_ok()
                );
                let renamed = provider
                    .update_model_config(
                        id.clone(),
                        changed.version,
                        model_id.into(),
                        Some(
                            serde_json::from_value(json!({"displayName":"Edited descriptor"}))
                                .unwrap(),
                        ),
                        &cancel,
                    )
                    .await
                    .unwrap();
                assert_eq!(renamed.models[0].name, "Edited descriptor");
                let reset = provider
                    .update_model_config(
                        id.clone(),
                        renamed.version,
                        model_id.into(),
                        None,
                        &cancel,
                    )
                    .await
                    .unwrap();
                assert_eq!(reset.models[0].name, catalog.models[0].name);
                assert_eq!(
                    reset.models[0].endpoint,
                    Some(dim_agent_capabilities::host::ModelProtocol::Anthropic)
                );
                let cleared = provider
                    .update_model_config(
                        id.clone(),
                        reset.version,
                        model_id.into(),
                        Some(
                            serde_json::from_value(json!({"endpoint":{"action":"clear"}})).unwrap(),
                        ),
                        &cancel,
                    )
                    .await
                    .unwrap();
                assert_eq!(cleared.models[0].endpoint, catalog.models[0].endpoint);
            }
        }
        for raw in [
            json!({"endpointType":"unknown"}),
            json!({"endpointType":7}),
            json!({"supportedEndpointTypes":"openai"}),
            json!({"selectableEndpointTypes":[""]}),
            json!({"supportedEndpointTypes":[5]}),
        ] {
            let mut entry = model("invalid", 1000);
            entry.metadata = Some(serde_json::from_value(json!({"raw":raw})).unwrap());
            let mut input = input(
                INVALID_ROUTE,
                &format!("http://{address}"),
                StaticAuth::Anonymous,
                None,
                "bad",
                1000,
            );
            input.connection.driver_kind = "new-api-router".into();
            input.models = vec![entry];
            assert!(provider.create(input, &cancel).await.is_err());
            assert!(
                provider
                    .catalog_for(INVALID_ROUTE.into(), &cancel)
                    .await
                    .unwrap()
                    .is_none()
            );
        }
        let builtin = provider
            .list(&cancel)
            .await
            .unwrap()
            .providers
            .into_iter()
            .find(|p| p.provider_id == "opencode-go")
            .unwrap();
        if !restart {
            saved_version = provider
                .update(builtin.provider_id, builtin.version, serde_json::from_value(json!({"baseUrl":format!("http://{address}/go/v1"),"headers":[{"name":"X-OpenCode-Session","action":"set","value":"stale"}],"credential":{"action":"set","value":"builtin-key"},"enabled":true})).unwrap(), &cancel)
                .await
                .unwrap()
                .version.unwrap();
        } else {
            assert_eq!(
                builtin.version, saved_version,
                "routing fingerprints must be stable"
            );
        }
        for (model_id, path) in [
            ("qwen3.8-max", "/go/v1/messages"),
            ("gpt-5.6-luna", "/go/v1/responses"),
        ] {
            let selection = provider
                .configuration(
                    Some(ModelId {
                        provider_id: "opencode-go".into(),
                        model_id: model_id.into(),
                    }),
                    None,
                    ModelPreferences::default(),
                    &cancel,
                )
                .await
                .unwrap();
            let mut request = ModelRequest::new(
                "opencode-go",
                model_id,
                vec![Message::text(Role::User, "hello")],
            );
            request.capability_snapshot = selection.capability_snapshot;
            request.config = selection.config;
            assert!(
                resolver
                    .0
                    .resolve(Arc::new(request.clone()), cancel.clone())
                    .await
                    .is_err()
            );
            request.metadata = Some(dim_agent_types::RequestMetadata {
                session_id: Some("test-session".into()),
                ..Default::default()
            });
            let request = Arc::new(request);
            let resolved = resolver
                .0
                .resolve(request.clone(), cancel.clone())
                .await
                .unwrap();
            let mut stream = host
                .0
                .invoke(request, resolved.connection, cancel.clone())
                .await
                .unwrap();
            let mut stopped = false;
            while let Some(chunk) = stream.next().await {
                stopped |= matches!(chunk.unwrap(), ModelChunk::MessageStop { .. });
            }
            assert!(stopped);
            let sent = seen.lock().unwrap().last().unwrap().clone();
            assert_eq!(sent.path, path);
            assert_eq!(sent.session_id.as_deref(), Some("test-session"));
            assert_eq!(sent.authorization.as_deref(), Some("Bearer builtin-key"));
        }
        root.shutdown().await.unwrap();
    }
    server.abort();
    let _ = server.await;
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test]
async fn public_static_provider_selection_resolution_execution_and_lifecycle_flow() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = Router::new()
        .route("/{*path}", post(completion))
        .with_state(seen.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let base = format!("http://{address}");
    let home = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target/provider-flow")
        .join(uuid::Uuid::new_v4().to_string());
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(home.join("auth.json"), "{broken").unwrap();

    let root = Ctx::root().unwrap();
    root.plugin(
        dim_app_server::methods::ManagementMethodsPlugin::typed(&dim_app_server::catalog())
            .unwrap(),
    );
    let files = root.plugin(FileStoragePlugin::new(vec![home.clone()]));
    let oauth = root.plugin(ManagedProvidersPlugin);
    root.plugin(dim_app_server::codex_oauth::OpenAICodexOAuthPlugin::new(&names(), 0).unwrap());
    root.plugin(dim_app_server::grok_oauth::GrokOAuthPlugin::new(&names(), 0).unwrap());
    let sqlite = root.plugin(SqliteStoragePlugin::new(home.join("native.sqlite")));
    let provider_plugin = root.plugin(provider_part(ProviderPlugin::new));
    let aimux = root.plugin(dim_agent_aimux::AimuxPlugin);
    let captured = Arc::new(OnceLock::new());
    let capture = root.plugin(Capture::new(&names(), captured.clone()).unwrap());
    for plugin in [&files, &oauth, &sqlite, &provider_plugin, &aimux, &capture] {
        active(plugin).await;
    }
    let (provider, resolver) = captured.get().unwrap();
    let cancel = CancellationToken::new();

    let managed_id = "018ec73f-4d19-7f61-bc79-7130618bb450";
    let mut editable = model("editable", 111);
    editable.capabilities.reasoning = true;
    editable.metadata = Some(
        serde_json::from_value(json!({
            "reasoning":{"supported":true,"effortOptions":["low","high"]},
            "serviceTier":{"supported":true,"options":["default","priority"]}
        }))
        .unwrap(),
    );
    let creation = json!({
        "providerId":managed_id,"displayName":"Editable","enabled":true,
        "connection":{"driverKind":"openai-responses","auth":{"type":"bearer"},"baseUrl":base,
            "credential":{"action":"set","value":"management-secret"},
            "headers":{"X-Private":"private-header"},
            "network":{"proxy":"http://proxy-user:proxy-password@127.0.0.1:9","timeoutMs":1234}},
        "models":[editable]
    });
    let created = provider
        .create(serde_json::from_value(creation.clone()).unwrap(), &cancel)
        .await
        .unwrap();
    assert_eq!(created.version, Some(1));
    assert_eq!(
        created.models.as_ref().unwrap()[0].model.model_id,
        "editable"
    );
    assert_eq!(created.models.as_ref().unwrap()[0].source, "default");
    let safe = serde_json::to_value(&created).unwrap();
    assert_eq!(safe["connection"]["proxyUrl"], "http://127.0.0.1:9/");
    assert_eq!(safe["connection"]["proxyAuthConfigured"], true);
    for secret in [
        "management-secret",
        "private-header",
        "proxy-user",
        "proxy-password",
    ] {
        assert!(!safe.to_string().contains(secret));
    }
    assert!(matches!(
        provider
            .create(serde_json::from_value(creation.clone()).unwrap(), &cancel)
            .await,
        Err(ProviderError::Conflict(_))
    ));
    for id in ["human-name", "018EC73F-4D19-7F61-BC79-7130618BB450"] {
        let mut invalid = creation.clone();
        invalid["providerId"] = json!(id);
        assert!(matches!(
            provider
                .create(serde_json::from_value(invalid).unwrap(), &cancel)
                .await,
            Err(ProviderError::InvalidConfig(_))
        ));
    }
    for patch in [
        json!({"baseUrl":null}),
        json!({"driverKind":"gemini"}),
        json!({"headers":[{"name":"x-test","action":"set","value":"a"},{"name":"X-Test","action":"remove"}]}),
    ] {
        assert!(serde_json::from_value::<dim_app_server::provider::ProviderPatch>(patch).is_err());
    }
    for patch in [
        json!({}),
        json!({"headers":[]}),
        json!({"network":{}}),
        json!({"headers":[],"network":{}}),
    ] {
        assert!(matches!(
            provider
                .update(
                    managed_id.into(),
                    1,
                    serde_json::from_value(patch).unwrap(),
                    &cancel
                )
                .await,
            Err(ProviderError::InvalidConfig(_))
        ));
        assert_eq!(
            provider
                .management(Some(managed_id.into()), &cancel)
                .await
                .unwrap()
                .providers[0]
                .version,
            Some(1)
        );
    }
    // The connection edit runs on a disabled record: an enabled custom
    // record lists the new endpoint, and this fixture answers completions
    // only. The edit drops the endpoint's own models, so the model the rest
    // of the flow selects is added back as a user model.
    let disabled = provider
        .update(
            managed_id.into(),
            1,
            serde_json::from_value(json!({"enabled":false})).unwrap(),
            &cancel,
        )
        .await
        .unwrap();
    assert_eq!(disabled.version, Some(2));
    let edited = provider.update(managed_id.into(), 2, serde_json::from_value(json!({
        "displayName":"Renamed", "headers":[{"name":"x-private","action":"remove"},{"name":"X-Preserved","action":"set","value":"preserved"}],
        "network":{"timeoutMs":{"action":"clear"}}
    })).unwrap(), &cancel).await.unwrap();
    assert_eq!(edited.version, Some(3));
    assert_eq!(
        serde_json::to_value(&edited).unwrap()["modelsListed"],
        json!("disabled")
    );
    assert!(edited.models.as_ref().unwrap().is_empty());
    let edited = provider
        .update(
            managed_id.into(),
            3,
            serde_json::from_value(json!({"enabled":true})).unwrap(),
            &cancel,
        )
        .await
        .unwrap();
    assert_eq!(edited.version, Some(4));
    let connection = edited.connection.unwrap();
    assert!(connection.credential_configured && connection.proxy_auth_configured);
    assert_eq!(connection.header_names, ["X-Preserved"]);
    assert_eq!(connection.timeout_ms, None);
    let re_added = provider
        .create_model(managed_id.into(), 4, editable.clone(), &cancel)
        .await
        .unwrap();
    assert!(
        re_added.models.iter().any(|model| model.id == "editable"),
        "the dropped model is a user model now"
    );
    let preferred = provider.update_preferences(managed_id.into(), 5, "editable".into(), serde_json::from_value(json!({
        "reasoningEffort":{"action":"set","value":"high"},"serviceTier":{"action":"set","value":"priority"}
    })).unwrap(), &cancel).await.unwrap();
    assert_eq!(preferred.version, Some(6));
    let preference_target = ModelId {
        provider_id: managed_id.into(),
        model_id: "editable".into(),
    };
    let implicit = provider
        .configuration(
            Some(preference_target.clone()),
            None,
            ModelPreferences::default(),
            &cancel,
        )
        .await
        .unwrap();
    assert_eq!(implicit.config.unwrap().reasoning, None);
    let adopted = provider
        .configuration(
            Some(preference_target),
            None,
            preferred.preferences["editable"].clone(),
            &cancel,
        )
        .await
        .unwrap();
    assert_eq!(
        adopted.config.unwrap().reasoning.unwrap().effort.as_deref(),
        Some("high")
    );
    let preferred = provider
        .update_preferences(
            managed_id.into(),
            6,
            "editable".into(),
            serde_json::from_value(json!({
                "reasoningEffort":{"action":"clear"}
            }))
            .unwrap(),
            &cancel,
        )
        .await
        .unwrap();
    assert_eq!(preferred.preferences["editable"].reasoning_effort, None);
    assert_eq!(
        preferred.preferences["editable"].service_tier.as_deref(),
        Some("priority")
    );
    assert!(matches!(
        provider
            .update_preferences(
                managed_id.into(),
                7,
                "editable".into(),
                serde_json::from_value(json!({
                    "reasoningEffort":{"action":"set","value":"unsupported"}
                }))
                .unwrap(),
                &cancel
            )
            .await,
        Err(ProviderError::InvalidConfig(_))
    ));
    let disconnected = provider
        .update(
            managed_id.into(),
            7,
            serde_json::from_value(json!({"credential":{"action":"clear"},"enabled":false}))
                .unwrap(),
            &cancel,
        )
        .await
        .unwrap();
    assert_eq!(disconnected.version, Some(8));
    assert!(
        !disconnected
            .connection
            .as_ref()
            .unwrap()
            .credential_configured
    );
    assert!(matches!(
        provider
            .update(
                managed_id.into(),
                8,
                serde_json::from_value(json!({"enabled":true})).unwrap(),
                &cancel
            )
            .await,
        Err(ProviderError::AuthRequired)
    ));
    let management_snapshot = serde_json::to_value(
        provider
            .management(Some(managed_id.into()), &cancel)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(management_snapshot["providers"][0]["version"], 8);
    assert_eq!(
        management_snapshot["providers"][0]["preferences"]["editable"],
        json!({"serviceTier":"priority"})
    );
    assert_eq!(
        management_snapshot["providers"][0]["connection"]["proxyUrl"],
        "http://127.0.0.1:9/"
    );

    let stale_id = "018ec73f-4d19-7f61-bc79-7130618bb451";
    let mut stale_creation = creation.clone();
    stale_creation["providerId"] = json!(stale_id);
    provider
        .create(serde_json::from_value(stale_creation).unwrap(), &cancel)
        .await
        .unwrap();
    provider.update_preferences(stale_id.into(), 1, "editable".into(), serde_json::from_value(json!({
        "reasoningEffort":{"action":"set","value":"high"},"serviceTier":{"action":"set","value":"priority"}
    })).unwrap(), &cancel).await.unwrap();
    provider
        .update_model_config(
            stale_id.into(),
            2,
            "editable".into(),
            Some(
                serde_json::from_value(json!({
                    "reasoning":{"supported":true,"effortOptions":["low"]}
                }))
                .unwrap(),
            ),
            &cancel,
        )
        .await
        .unwrap();
    let stale = provider
        .management(Some(stale_id.into()), &cancel)
        .await
        .unwrap();
    assert_eq!(
        stale.providers[0].preferences["editable"]
            .reasoning_effort
            .as_deref(),
        Some("high")
    );
    let cleared = provider
        .update_preferences(
            stale_id.into(),
            3,
            "editable".into(),
            serde_json::from_value(json!({
                "serviceTier":{"action":"clear"}
            }))
            .unwrap(),
            &cancel,
        )
        .await
        .unwrap();
    assert_eq!(
        cleared.preferences["editable"].reasoning_effort.as_deref(),
        Some("high")
    );
    assert_eq!(cleared.preferences["editable"].service_tier, None);
    let reset = provider
        .update_preferences(
            stale_id.into(),
            4,
            "editable".into(),
            serde_json::from_value(json!({
                "serviceTier":{"action":"set","value":"priority"}
            }))
            .unwrap(),
            &cancel,
        )
        .await
        .unwrap();
    assert_eq!(
        reset.preferences["editable"].reasoning_effort.as_deref(),
        Some("high")
    );
    assert_eq!(
        reset.preferences["editable"].service_tier.as_deref(),
        Some("priority")
    );
    let cleared = provider
        .update_preferences(
            stale_id.into(),
            5,
            "editable".into(),
            serde_json::from_value(json!({
                "reasoningEffort":{"action":"clear"}
            }))
            .unwrap(),
            &cancel,
        )
        .await
        .unwrap();
    assert_eq!(cleared.preferences["editable"].reasoning_effort, None);
    assert_eq!(
        cleared.preferences["editable"].service_tier.as_deref(),
        Some("priority")
    );
    provider
        .delete_static(stale_id.into(), 6, &cancel)
        .await
        .unwrap();

    let bedrock = provider
        .list(&cancel)
        .await
        .unwrap()
        .providers
        .into_iter()
        .find(|provider| provider.provider_id == "aws-bedrock")
        .unwrap();
    assert_eq!(bedrock.auth, StaticAuth::AwsBedrock);
    let bedrock = provider
        .update(bedrock.provider_id.clone(), bedrock.version, serde_json::from_value(json!({"baseUrl":"https://bedrock-runtime.us-west-2.amazonaws.com","headers":[{"name":"x-marker","action":"set","value":"safe"}],"credential":{"action":"set_aws","value":{"accessKeyId":"fixture-access","secretAccessKey":"fixture-secret","sessionToken":"fixture-session"}},"enabled":true})).unwrap(), &cancel)
        .await
        .unwrap();
    assert!(bedrock.connection.as_ref().unwrap().credential_configured);
    let catalog = provider
        .catalog_for("aws-bedrock".into(), &cancel)
        .await
        .unwrap()
        .unwrap();
    let model_id = catalog.models[0].id.clone();
    let selection = provider
        .configuration(
            Some(ModelId {
                provider_id: "aws-bedrock".into(),
                model_id: model_id.clone(),
            }),
            None,
            ModelPreferences::default(),
            &cancel,
        )
        .await
        .unwrap();
    let mut request = ModelRequest::new(
        "aws-bedrock",
        model_id,
        vec![Message::text(Role::User, "hello")],
    );
    request.capability_snapshot = selection.capability_snapshot;
    let bedrock_request = Arc::new(request);
    let resolved = resolver
        .0
        .resolve(bedrock_request.clone(), cancel.clone())
        .await
        .unwrap();
    assert!(resolved.recovery.is_none());
    let ModelConnection::Bedrock(connection) = resolved.connection else {
        panic!("Bedrock connection expected")
    };
    assert!(matches!(
        connection.auth,
        BedrockAuth::SigV4 { region, credentials }
            if region == "us-west-2" && credentials.access_key_id == "fixture-access"
                && credentials.secret_access_key == "fixture-secret"
                && credentials.session_token.as_deref() == Some("fixture-session")
    ));
    assert_eq!(connection.headers["x-marker"], "safe");
    let public = serde_json::to_string(&provider.list(&cancel).await.unwrap()).unwrap();
    assert!(!public.contains("fixture-access") && !public.contains("fixture-secret"));
    for base_url in [
        "http://bedrock-runtime.us-west-2.amazonaws.com",
        "https://bedrock-runtime.us-west-2.amazonaws.com:444",
        "https://bedrock-runtime.us-west-2.amazonaws.com/path",
        "https://bedrock-runtime.us-west-2.amazonaws.com?secret=value",
        "https://bedrock-runtime.us-west-2.amazonaws.com.attacker.invalid",
    ] {
        let patch = serde_json::from_value(json!({"baseUrl":base_url})).unwrap();
        assert!(
            provider
                .update(
                    bedrock.provider_id.clone(),
                    bedrock.version.unwrap(),
                    patch,
                    &cancel
                )
                .await
                .is_err()
        );
    }
    for header in [
        "Authorization",
        "Host",
        "X-Amz-Date",
        "x-amz-content-sha256",
        "X-Amz-Security-Token",
        "Content-Type",
    ] {
        let patch = serde_json::from_value(
            json!({"headers":[{"name":header,"action":"set","value":"reserved"}]}),
        )
        .unwrap();
        assert!(
            provider
                .update(
                    bedrock.provider_id.clone(),
                    bedrock.version.unwrap(),
                    patch,
                    &cancel
                )
                .await
                .is_err()
        );
    }
    for (access, secret, session) in [
        ("", "secret", None),
        ("access", "", None),
        ("access\n", "secret", None),
        ("access", "secret", Some("bad\r\ntoken")),
    ] {
        let mut value = json!({"accessKeyId":access,"secretAccessKey":secret});
        if let Some(session) = session {
            value["sessionToken"] = json!(session);
        }
        let patch =
            serde_json::from_value(json!({"credential":{"action":"set_aws","value":value}}))
                .unwrap();
        assert!(
            provider
                .update(
                    bedrock.provider_id.clone(),
                    bedrock.version.unwrap(),
                    patch,
                    &cancel
                )
                .await
                .is_err()
        );
    }
    let mut wrong_driver = input(WRONG_AWS, &base, StaticAuth::Bearer, None, "", 4096);
    wrong_driver.connection.credential = Some(CredentialUpdate::SetAws {
        value: AwsCredentials {
            access_key_id: "access".into(),
            secret_access_key: "secret".into(),
            session_token: None,
        },
    });
    assert!(
        provider
            .create(wrong_driver.clone(), &cancel)
            .await
            .is_err()
    );
    wrong_driver.connection.auth = StaticAuth::AwsBedrock;
    assert!(provider.create(wrong_driver, &cancel).await.is_err());
    assert_eq!(
        serde_json::to_string(&provider.list(&cancel).await.unwrap()).unwrap(),
        public
    );
    assert!(
        provider
            .update(
                bedrock.provider_id.clone(),
                bedrock.version.unwrap() - 1,
                serde_json::from_value(json!({"enabled":true})).unwrap(),
                &cancel
            )
            .await
            .is_err()
    );
    assert_eq!(
        provider
            .update(
                bedrock.provider_id.clone(),
                bedrock.version.unwrap(),
                serde_json::from_value(json!({"enabled":true})).unwrap(),
                &cancel
            )
            .await
            .unwrap()
            .version,
        Some(bedrock.version.unwrap() + 1)
    );

    let a = provider
        .create(
            input(
                PROVIDER_A,
                &format!("{base}/a"),
                StaticAuth::Bearer,
                Some("key-a"),
                "secret-a",
                1234,
            ),
            &cancel,
        )
        .await
        .unwrap();
    let b = provider
        .create(
            input(
                PROVIDER_B,
                &format!("{base}/b"),
                StaticAuth::Anonymous,
                None,
                "public-b",
                5678,
            ),
            &cancel,
        )
        .await
        .unwrap();
    assert!(
        a.connection.as_ref().unwrap().credential_configured
            && !b.connection.as_ref().unwrap().credential_configured
    );
    let public = serde_json::to_string(&provider.list(&cancel).await.unwrap()).unwrap();
    assert!(
        !public.contains("key-a") && !public.contains("secret-a") && !public.contains("public-b")
    );
    assert!(
        provider
            .create(
                input(MISSING, &base, StaticAuth::Bearer, None, "x", 100),
                &cancel
            )
            .await
            .is_err()
    );

    let global = provider
        .select(None, target(PROVIDER_B), None, &cancel)
        .await
        .unwrap();
    assert!(
        provider
            .select(None, target(PROVIDER_A), None, &cancel)
            .await
            .is_err()
    );
    let workspace = std::path::Path::new("/workspace/provider-flow");
    provider
        .select(Some(workspace), target(PROVIDER_A), None, &cancel)
        .await
        .unwrap();
    let chosen = provider
        .configuration(None, Some(workspace), ModelPreferences::default(), &cancel)
        .await
        .unwrap();
    assert_eq!(
        (
            chosen.provider_id.as_str(),
            chosen.config.as_ref().unwrap().max_tokens
        ),
        (PROVIDER_A, Some(1234.0))
    );
    let catalog = provider
        .set_enabled_models(PROVIDER_A.into(), a.version.unwrap(), vec![], &cancel)
        .await
        .unwrap();
    let fallback = provider
        .configuration(None, Some(workspace), ModelPreferences::default(), &cancel)
        .await
        .unwrap();
    assert_eq!(fallback.provider_id, PROVIDER_B);
    assert_eq!(
        provider
            .selection(Some(workspace), &cancel)
            .await
            .unwrap()
            .unwrap()
            .provider_id,
        PROVIDER_A
    );
    assert_eq!(global.version, 1);
    provider
        .set_enabled_models(
            PROVIDER_A.into(),
            catalog.version,
            vec!["same".into()],
            &cancel,
        )
        .await
        .unwrap();

    for (id, suffix) in [(PROVIDER_A, "a"), (PROVIDER_B, "b")] {
        let selection = provider
            .configuration(Some(target(id)), None, ModelPreferences::default(), &cancel)
            .await
            .unwrap();
        let mut request = ModelRequest::new(id, "same", vec![Message::text(Role::User, "hello")]);
        request.capability_snapshot = selection.capability_snapshot;
        request.config = selection.config;
        let request = Arc::new(request);
        let resolved = resolver
            .0
            .resolve(request.clone(), cancel.clone())
            .await
            .unwrap();
        let ModelConnection::Http(connection) = &resolved.connection else {
            panic!("HTTP expected")
        };
        assert!(connection.base_url.ends_with(&format!("/{suffix}")));
        let mut output = root
            .get::<ModelHost>()
            .unwrap()
            .0
            .invoke(request, resolved.connection, cancel.clone())
            .await
            .unwrap();
        while let Some(chunk) = output.next().await {
            if let ModelChunk::Text(text) = chunk.unwrap() {
                assert_eq!(text, "ok");
            }
        }
    }
    {
        let requests = seen.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert!(requests[0].path.starts_with("/a/") && requests[1].path.starts_with("/b/"));
        assert_eq!(requests[0].authorization.as_deref(), Some("Bearer key-a"));
        assert_eq!(requests[1].authorization, None);
        assert_eq!(requests[0].marker.as_deref(), Some("secret-a"));
        assert_eq!(requests[1].marker.as_deref(), Some("public-b"));
        assert_eq!(requests[0].body["max_tokens"], 1234);
        assert_eq!(requests[1].body["max_tokens"], 5678);
    }

    let builtin = provider
        .management(Some("deepseek".into()), &cancel)
        .await
        .unwrap();
    let builtin = provider.update("deepseek".into(), builtin.providers[0].version.unwrap(), serde_json::from_value(json!({"baseUrl":format!("{base}/builtin"),"credential":{"action":"set","value":"user-model-key"},"enabled":true})).unwrap(), &cancel).await.unwrap();
    let custom = provider
        .catalog_for(PROVIDER_A.into(), &cancel)
        .await
        .unwrap()
        .unwrap();
    for (id, version) in [
        (PROVIDER_A, custom.version),
        ("deepseek", builtin.version.unwrap()),
    ] {
        let catalog = provider
            .create_model(id.into(), version, model("user-added", 321), &cancel)
            .await
            .unwrap();
        assert!(catalog.models.iter().any(|model| model.id == "user-added"));
        let selected = provider
            .configuration(
                Some(ModelId {
                    provider_id: id.into(),
                    model_id: "user-added".into(),
                }),
                None,
                ModelPreferences::default(),
                &cancel,
            )
            .await
            .unwrap();
        let mut request = ModelRequest::new(
            id,
            "user-added",
            vec![Message::text(Role::User, "user model")],
        );
        request.config = selected.config;
        request.capability_snapshot = selected.capability_snapshot;
        let request = Arc::new(request);
        let resolved = resolver
            .0
            .resolve(request.clone(), cancel.clone())
            .await
            .unwrap();
        let mut output = root
            .get::<ModelHost>()
            .unwrap()
            .0
            .invoke(request, resolved.connection, cancel.clone())
            .await
            .unwrap();
        while let Some(chunk) = output.next().await {
            chunk.unwrap();
        }
        let snapshot = provider.management(Some(id.into()), &cancel).await.unwrap();
        assert_eq!(
            snapshot.providers[0]
                .models
                .as_ref()
                .unwrap()
                .iter()
                .find(|entry| entry.model.model_id == "user-added")
                .unwrap()
                .source,
            "user"
        );
    }
    {
        let requests = seen.lock().unwrap();
        assert_eq!(requests.len(), 4);
        assert_eq!(requests[2].body["model"], "user-added");
        assert_eq!(requests[3].body["model"], "user-added");
        assert_eq!(
            requests[3].authorization.as_deref(),
            Some("Bearer user-model-key")
        );
    }

    let mut media = input(MEDIA, &base, StaticAuth::Anonymous, None, "media", 99);
    media.models[0].metadata =
        Some(serde_json::from_value(json!({"raw":{"type":"embedding"}})).unwrap());
    provider.create(media, &cancel).await.unwrap();
    assert!(
        provider
            .select(None, target(MEDIA), Some(global.version), &cancel)
            .await
            .is_err()
    );
    assert!(
        provider
            .configuration(
                Some(target(MEDIA)),
                None,
                ModelPreferences::default(),
                &cancel
            )
            .await
            .is_err()
    );

    let before = provider
        .catalog_for(PROVIDER_B.into(), &cancel)
        .await
        .unwrap()
        .unwrap();
    let edited = provider
        .update_model_config(
            PROVIDER_B.into(),
            before.version,
            "same".into(),
            Some(
                serde_json::from_value(json!({"maxOutputTokens":3333,"displayName":"Edited"}))
                    .unwrap(),
            ),
            &cancel,
        )
        .await
        .unwrap();
    assert_eq!(edited.models[0].config.max_tokens, Some(3333.));
    let reset = provider
        .update_model_config(
            PROVIDER_B.into(),
            edited.version,
            "same".into(),
            None,
            &cancel,
        )
        .await
        .unwrap();
    assert_eq!(reset.models[0].config.max_tokens, Some(5678.));
    assert!(
        provider
            .update_model_config(
                PROVIDER_B.into(),
                edited.version,
                "same".into(),
                None,
                &cancel
            )
            .await
            .is_err()
    );

    let disabled = provider
        .set_enabled_models(PROVIDER_B.into(), reset.version, vec![], &cancel)
        .await
        .unwrap();
    let returned = provider
        .create_model(
            PROVIDER_B.into(),
            disabled.version,
            model("other", 765),
            &cancel,
        )
        .await
        .unwrap();
    assert!(returned.disabled_model_ids.contains(&"same".into()));
    assert!(
        provider
            .configuration(
                Some(target(PROVIDER_B)),
                None,
                ModelPreferences::default(),
                &cancel
            )
            .await
            .is_err()
    );

    {
        let database = rusqlite::Connection::open(home.join("native.sqlite")).unwrap();
        database.execute("UPDATE provider_selections SET provider_id='missing-provider', model_id='missing' WHERE scope='workspace:/workspace/provider-flow'", []).unwrap();
    }
    provider
        .set_enabled_models(
            PROVIDER_B.into(),
            returned.version,
            vec!["same".into(), "other".into()],
            &cancel,
        )
        .await
        .unwrap();
    assert_eq!(
        provider
            .configuration(None, Some(workspace), ModelPreferences::default(), &cancel)
            .await
            .unwrap()
            .provider_id,
        PROVIDER_B
    );
    assert_eq!(
        provider
            .selection(Some(workspace), &cancel)
            .await
            .unwrap()
            .unwrap()
            .provider_id,
        "missing-provider"
    );

    let builtin = provider
        .list(&cancel)
        .await
        .unwrap()
        .providers
        .into_iter()
        .find(|p| p.provider_id == "anthropic")
        .unwrap();
    let builtin = provider
.update(builtin.provider_id.clone(), builtin.version, serde_json::from_value(json!({"baseUrl":format!("{base}/builtin"),"credential":{"action":"set","value":"builtin-key"},"enabled":true})).unwrap(), &cancel)
        .await
        .unwrap();
    let builtin_models = provider
        .catalog_for(builtin.provider_id.clone(), &cancel)
        .await
        .unwrap()
        .unwrap();
    let builtin_target = ModelId {
        provider_id: builtin.provider_id.clone(),
        model_id: builtin_models
            .models
            .iter()
            .find(|m| m.unavailable_reason.is_none())
            .unwrap()
            .id
            .clone(),
    };
    let selection = provider
        .select(None, builtin_target, Some(global.version), &cancel)
        .await
        .unwrap();
    provider
        .delete_static(
            builtin.provider_id.clone(),
            builtin.version.unwrap(),
            &cancel,
        )
        .await
        .unwrap();
    assert_eq!(
        provider
            .selection(None, &cancel)
            .await
            .unwrap()
            .unwrap()
            .provider_id,
        "anthropic"
    );
    assert_ne!(
        provider
            .configuration(None, None, ModelPreferences::default(), &cancel)
            .await
            .unwrap()
            .provider_id,
        "anthropic"
    );
    assert_eq!(
        provider
            .selection(None, &cancel)
            .await
            .unwrap()
            .unwrap()
            .version,
        selection.version
    );
    let persisted = serde_json::to_value(provider.list(&cancel).await.unwrap()).unwrap();
    let last_selection = provider
        .configuration(
            Some(target(PROVIDER_A)),
            None,
            ModelPreferences::default(),
            &cancel,
        )
        .await
        .unwrap();
    capture.dispose().await.unwrap();
    provider_plugin.dispose().await.unwrap();
    assert!(matches!(
        provider.list(&cancel).await,
        Err(dim_agent_capabilities::host::HostError::Failed {
            kind: dim_agent_capabilities::host::HostErrorKind::OwnerClosed,
            ..
        })
    ));
    let request = Arc::new(ModelRequest {
        capability_snapshot: last_selection.capability_snapshot,
        ..ModelRequest::new(PROVIDER_A, "same", vec![])
    });
    assert!(matches!(
        resolver.0.resolve(request, cancel.clone()).await,
        Err(dim_agent_capabilities::host::HostError::Failed {
            kind: dim_agent_capabilities::host::HostErrorKind::OwnerClosed,
            ..
        })
    ));
    root.shutdown().await.unwrap();

    let restarted = Ctx::root().unwrap();
    restarted.plugin(
        dim_app_server::methods::ManagementMethodsPlugin::typed(&dim_app_server::catalog())
            .unwrap(),
    );
    let plugins = [
        restarted.plugin(FileStoragePlugin::new(vec![home.clone()])),
        restarted.plugin(SqliteStoragePlugin::new(home.join("native.sqlite"))),
        restarted.plugin(ManagedProvidersPlugin),
        restarted
            .plugin(dim_app_server::codex_oauth::OpenAICodexOAuthPlugin::new(&names(), 0).unwrap()),
        restarted.plugin(dim_app_server::grok_oauth::GrokOAuthPlugin::new(&names(), 0).unwrap()),
        restarted.plugin(provider_part(ProviderPlugin::new)),
        restarted.plugin(dim_agent_aimux::AimuxPlugin),
    ];
    for plugin in &plugins {
        active(plugin).await;
    }
    let reloaded = restarted.get::<ProviderService>().unwrap();
    assert_eq!(
        serde_json::to_value(
            reloaded
                .management(Some(managed_id.into()), &cancel)
                .await
                .unwrap()
        )
        .unwrap(),
        management_snapshot
    );

    assert_eq!(
        serde_json::to_value(reloaded.list(&cancel).await.unwrap()).unwrap(),
        persisted
    );
    let resolved = restarted
        .get::<ProviderConnectionResolver>()
        .unwrap()
        .0
        .resolve(bedrock_request, cancel.clone())
        .await
        .unwrap();
    assert!(
        matches!(resolved.connection, ModelConnection::Bedrock(connection)
        if matches!(&connection.auth, BedrockAuth::SigV4 { credentials, region }
            if credentials.secret_access_key == "fixture-secret"
                && credentials.session_token.as_deref() == Some("fixture-session")
                && region == "us-west-2"))
    );
    assert_eq!(
        reloaded
            .configuration(
                Some(target(PROVIDER_A)),
                None,
                ModelPreferences::default(),
                &cancel
            )
            .await
            .unwrap()
            .config
            .unwrap()
            .max_tokens,
        Some(1234.)
    );
    assert_eq!(
        std::fs::read_to_string(home.join("auth.json")).unwrap(),
        "{broken"
    );
    restarted.shutdown().await.unwrap();

    let collision = {
        let database = rusqlite::Connection::open(home.join("native.sqlite")).unwrap();
        let body: String = database
            .query_row(
                "SELECT record FROM providers WHERE provider_id='openai'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let mut old: Value = serde_json::from_str(&body).unwrap();
        old["displayName"] = json!("Old definition");
        old["definitionFingerprint"] = json!("old");
        old["baseUrl"] = json!(format!("{base}/preserved"));
        old["credential"] = json!("preserved-secret");
        old["headers"] = json!({"x-old":"preserved-header"});
        old["disabledModelIds"] = json!(["missing-model"]);
        old["modelConfigOverrides"] = json!([model("missing-model", 817)]);
        old["models"] = json!([]);
        database
            .execute(
                "UPDATE providers SET record=?1 WHERE provider_id='openai'",
                [old.to_string()],
            )
            .unwrap();
        let custom: String = database
            .query_row(
                "SELECT record FROM providers WHERE provider_id=?1",
                [PROVIDER_A],
                |r| r.get(0),
            )
            .unwrap();
        let mut removed: Value = serde_json::from_str(&custom).unwrap();
        removed["kind"] = json!("builtin");
        database.execute("INSERT INTO providers(provider_id,version,record) VALUES ('removed-builtin',?1,?2)", rusqlite::params![removed["version"].as_i64().unwrap(), removed.to_string()]).unwrap();
        database
            .execute(
                "INSERT INTO providers(provider_id,version,record) VALUES ('0-broken',1,'{broken')",
                [],
            )
            .unwrap();
        database
            .execute(
                "UPDATE providers SET record='{broken' WHERE provider_id='anthropic'",
                [],
            )
            .unwrap();
        let body: String = database
            .query_row(
                "SELECT record FROM providers WHERE provider_id='deepseek'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let mut collision: Value = serde_json::from_str(&body).unwrap();
        let model = collision["models"][0].clone();
        let id = model["modelId"].as_str().unwrap().to_owned();
        collision["definitionFingerprint"] = json!("older-definition");
        collision["models"] = json!([]);
        collision["userModels"] = json!([model]);
        collision["disabledModelIds"] = json!([id]);
        collision["enabled"] = json!(false);
        database
            .execute(
                "UPDATE providers SET record=?1 WHERE provider_id='deepseek'",
                [collision.to_string()],
            )
            .unwrap();
        (id, collision)
    };
    let isolated = Ctx::root().unwrap();
    isolated.plugin(
        dim_app_server::methods::ManagementMethodsPlugin::typed(&dim_app_server::catalog())
            .unwrap(),
    );
    let plugins = [
        isolated.plugin(FileStoragePlugin::new(vec![home.clone()])),
        isolated.plugin(SqliteStoragePlugin::new(home.join("native.sqlite"))),
        isolated.plugin(ManagedProvidersPlugin),
        isolated
            .plugin(dim_app_server::codex_oauth::OpenAICodexOAuthPlugin::new(&names(), 0).unwrap()),
        isolated.plugin(dim_app_server::grok_oauth::GrokOAuthPlugin::new(&names(), 0).unwrap()),
        isolated.plugin(provider_part(ProviderPlugin::new)),
        isolated.plugin(dim_agent_aimux::AimuxPlugin),
    ];
    for plugin in &plugins {
        active(plugin).await;
    }
    let usable = isolated.get::<ProviderService>().unwrap();
    for id in ["0-broken", "anthropic"] {
        assert!(usable.catalog_for(id.into(), &cancel).await.is_err());
    }
    let listing = usable.list(&cancel).await.unwrap();
    assert!(
        listing
            .providers
            .iter()
            .any(|p| p.provider_id == PROVIDER_A)
    );
    let upgraded = listing
        .providers
        .iter()
        .find(|p| p.provider_id == "openai")
        .unwrap();
    assert_eq!(upgraded.version, 2);
    assert_ne!(upgraded.display_name, "Old definition");
    assert_eq!(upgraded.base_url, format!("{base}/preserved"));
    assert!(upgraded.credential_configured);
    assert!(
        !listing
            .providers
            .iter()
            .find(|p| p.provider_id == "removed-builtin")
            .unwrap()
            .enabled
    );
    let catalog = usable
        .catalog_for("openai".into(), &cancel)
        .await
        .unwrap()
        .unwrap();
    assert!(!catalog.models.is_empty());
    assert!(!catalog.models.iter().any(|m| m.id == "missing-model"));
    assert_eq!(catalog.disabled_model_ids, ["missing-model"]);
    assert_eq!(
        listing
            .errors
            .iter()
            .map(|e| e.provider_id.as_str())
            .collect::<Vec<_>>(),
        ["0-broken", "anthropic", "grok-build", "openai-codex"]
    );
    assert_eq!(
        usable
            .configuration(None, None, ModelPreferences::default(), &cancel)
            .await
            .unwrap()
            .provider_id,
        PROVIDER_A
    );
    assert!(
        usable
            .configuration(
                Some(target("0-broken")),
                None,
                ModelPreferences::default(),
                &cancel
            )
            .await
            .is_err()
    );
    let view = usable
        .management(Some("deepseek".into()), &cancel)
        .await
        .unwrap();
    assert_eq!(view.errors.len(), 1);
    assert!(view.errors[0].message.contains(&collision.0));
    assert_eq!(
        view.providers[0].version,
        collision.1["version"]
            .as_u64()
            .map(|version| version as u32)
    );
    let candidate_models = view.providers[0].models.as_ref().unwrap();
    assert_eq!(candidate_models.len(), 1);
    assert_eq!(candidate_models[0].source, "user");
    assert!(!candidate_models[0].enabled);
    {
        let database = rusqlite::Connection::open(home.join("native.sqlite")).unwrap();
        let body: String = database
            .query_row(
                "SELECT record FROM providers WHERE provider_id='deepseek'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(serde_json::from_str::<Value>(&body).unwrap(), collision.1);
    }
    let deleted = usable
        .delete_model(
            "deepseek".into(),
            view.providers[0].version.unwrap(),
            collision.0.clone(),
            &cancel,
        )
        .await
        .unwrap();
    assert!(!deleted.disabled_model_ids.contains(&collision.0));
    let waiting = usable
        .management(Some("deepseek".into()), &cancel)
        .await
        .unwrap();
    assert_eq!(waiting.errors.len(), 1);
    assert!(waiting.errors[0].message.contains("restart"));
    plugins[5].dispose().await.unwrap();
    let replacement = isolated.plugin(provider_part(ProviderPlugin::new));
    active(&replacement).await;
    let replaced = isolated
        .get::<ProviderService>()
        .unwrap()
        .management(Some("deepseek".into()), &cancel)
        .await
        .unwrap();
    assert!(replaced.errors.is_empty());
    let bundled = replaced.providers[0]
        .models
        .as_ref()
        .unwrap()
        .iter()
        .find(|model| model.model.model_id == collision.0)
        .unwrap();
    assert_eq!(bundled.source, "default");
    assert!(bundled.enabled);
    isolated.shutdown().await.unwrap();
    {
        let database = rusqlite::Connection::open(home.join("native.sqlite")).unwrap();
        let body: String = database
            .query_row(
                "SELECT record FROM providers WHERE provider_id='openai'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let upgraded: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(upgraded["credential"], "preserved-secret");
        assert_eq!(upgraded["headers"]["x-old"], "preserved-header");
        assert_eq!(
            upgraded["modelConfigOverrides"][0]["capabilities"]["maxOutputTokens"],
            817
        );
        for id in ["0-broken", "anthropic"] {
            assert_eq!(
                database
                    .query_row(
                        "SELECT record FROM providers WHERE provider_id=?1",
                        [id],
                        |row| row.get::<_, String>(0)
                    )
                    .unwrap(),
                "{broken"
            );
        }
    }
    server.abort();
    let _ = server.await;
    let _ = std::fs::remove_dir_all(home);
}

#[tokio::test]
async fn legacy_import_preserves_sources_model_origins_and_scoped_choices() {
    let home = std::env::temp_dir().join(format!("provider-import-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&home).unwrap();
    let home = home.canonicalize().unwrap();
    let source = home.join("legacy.sqlite");
    {
        let db = rusqlite::Connection::open(&source).unwrap();
        db.execute_batch(
            "CREATE TABLE providers (
            providerId TEXT PRIMARY KEY, source TEXT, displayName TEXT, driverKind TEXT,
            defaultBaseUrl TEXT, baseUrl TEXT, auth TEXT, credential TEXT,
            accountUsageSettings TEXT, headers TEXT, proxy TEXT, timeoutMs INTEGER,
            retryConfig TEXT, models TEXT, modelConfigOverrides TEXT, capabilityDefaults TEXT,
            activeModelId TEXT, modelsSource TEXT, catalogVersion TEXT, modelsUpdatedAt TEXT,
            metadata TEXT, enabled INTEGER, createdAt TEXT, updatedAt TEXT, version INTEGER);
            CREATE TABLE provider_selections (
            selectionId TEXT PRIMARY KEY, scope TEXT, cwd TEXT, providerId TEXT, modelId TEXT,
            createdAt TEXT, updatedAt TEXT, version INTEGER);",
        )
        .unwrap();
        for (id, origin, models, overrides, metadata, usage) in [
            (
                "legacy-defaults",
                "remote",
                vec![model("default", 101)],
                vec![model("default", 202), model("disappeared", 303)],
                json!({"modelPreferences":{"default":{"reasoningEffort":"high"}}}),
                json!({}),
            ),
            (
                "legacy-users",
                "user",
                vec![model("manual", 401)],
                vec![model("manual", 402), model("orphan", 403)],
                json!({"disabledModelIds":["manual"]}),
                Value::Null,
            ),
            (
                "unsupported-usage",
                "remote",
                vec![model("default", 101)],
                vec![],
                json!({}),
                json!({"unsupported":"must-not-be-echoed"}),
            ),
            (
                "unsupported-preference",
                "remote",
                vec![model("default", 101)],
                vec![],
                json!({"modelPreferences":{"default":{"future":"must-not-be-echoed"}}}),
                Value::Null,
            ),
        ] {
            db.execute("INSERT INTO providers (providerId,source,displayName,driverKind,baseUrl,auth,credential,accountUsageSettings,models,modelConfigOverrides,activeModelId,modelsSource,metadata,enabled,version) VALUES (?1,'custom','Legacy','openai-responses','https://model.example/v1',?2,?3,?4,?5,?6,'disappeared',?7,?8,1,1)", rusqlite::params![id, json!({"type":"bearer"}).to_string(), json!({"type":"bearer","token":"legacy-secret"}).to_string(), if usage.is_null() { None } else { Some(usage.to_string()) }, serde_json::to_string(&models).unwrap(), serde_json::to_string(&overrides).unwrap(), origin, metadata.to_string()]).unwrap();
        }
        db.execute("INSERT INTO provider_selections(selectionId,scope,providerId,modelId,version) VALUES ('chosen','global','legacy-defaults','default',1)", []).unwrap();
        db.execute("INSERT INTO provider_selections(selectionId,scope,providerId,modelId,version) VALUES ('later','global','legacy-users','manual',1)", []).unwrap();
    }
    let original = std::fs::read(&source).unwrap();
    let root = Ctx::root().unwrap();
    root.plugin(
        dim_app_server::methods::ManagementMethodsPlugin::typed(&dim_app_server::catalog())
            .unwrap(),
    );
    let plugins = [
        root.plugin(FileStoragePlugin::new(vec![home.clone()])),
        root.plugin(SqliteStoragePlugin::new(home.join("native.sqlite"))),
        root.plugin(ManagedProvidersPlugin),
        root.plugin(dim_app_server::codex_oauth::OpenAICodexOAuthPlugin::new(&names(), 0).unwrap()),
        root.plugin(dim_app_server::grok_oauth::GrokOAuthPlugin::new(&names(), 0).unwrap()),
        root.plugin(provider_part(ProviderPlugin::new)),
        root.plugin(dim_agent_aimux::AimuxPlugin),
    ];
    for plugin in &plugins {
        active(plugin).await;
    }
    let provider = root.get::<ProviderService>().unwrap();
    let cancel = CancellationToken::new();
    let result = serde_json::to_value(
        provider
            .import_legacy(source.clone(), &cancel)
            .await
            .unwrap(),
    )
    .unwrap();
    let items = result["items"].as_array().unwrap();
    assert_eq!(
        items
            .iter()
            .filter(|item| item["status"] == "imported")
            .count(),
        3
    );
    assert_eq!(
        items
            .iter()
            .filter(|item| item["status"] == "rejected")
            .count(),
        3
    );
    assert!(!result.to_string().contains("legacy-secret"));
    assert!(!result.to_string().contains("must-not-be-echoed"));
    assert_eq!(std::fs::read(&source).unwrap(), original);
    let defaults = provider
        .management(Some("legacy-defaults".into()), &cancel)
        .await
        .unwrap();
    let entry = &defaults.providers[0];
    let models = entry.models.as_ref().unwrap();
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].source, "override");
    assert_eq!(models[0].model.capabilities.max_output_tokens, 202);
    assert_eq!(
        entry.preferences["default"].reasoning_effort.as_deref(),
        Some("high")
    );
    let users = provider
        .management(Some("legacy-users".into()), &cancel)
        .await
        .unwrap();
    let models = users.providers[0].models.as_ref().unwrap();
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].source, "user");
    assert_eq!(models[0].model.capabilities.max_output_tokens, 402);
    assert!(!models[0].enabled);
    assert_eq!(
        provider
            .selection(None, &cancel)
            .await
            .unwrap()
            .unwrap()
            .provider_id,
        "legacy-defaults"
    );
    let repeated = serde_json::to_value(
        provider
            .import_legacy(source.clone(), &cancel)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        repeated["items"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|item| item["status"] == "already_imported")
            .count(),
        3
    );
    let edited = provider
        .update(
            "legacy-defaults".into(),
            entry.version.unwrap(),
            serde_json::from_value(json!({"displayName":"Edited legacy Provider"})).unwrap(),
            &cancel,
        )
        .await
        .unwrap();
    assert_eq!(edited.display_name, "Edited legacy Provider");
    assert!(edited.connection.as_ref().unwrap().credential_configured);
    assert_eq!(
        edited.models.as_ref().unwrap()[0]
            .model
            .capabilities
            .max_output_tokens,
        202
    );
    assert!(matches!(
        provider
            .update(
                "legacy-defaults".into(),
                entry.version.unwrap(),
                serde_json::from_value(json!({"enabled":false})).unwrap(),
                &cancel
            )
            .await,
        Err(ProviderError::Conflict(_))
    ));
    provider
        .delete_static("legacy-defaults".into(), edited.version.unwrap(), &cancel)
        .await
        .unwrap();
    provider
        .import_legacy(source.clone(), &cancel)
        .await
        .unwrap();
    assert!(matches!(
        provider
            .management(Some("legacy-defaults".into()), &cancel)
            .await,
        Err(ProviderError::NotFound)
    ));
    assert!(provider.selection(None, &cancel).await.unwrap().is_none());
    assert_eq!(std::fs::read(&source).unwrap(), original);
    root.shutdown().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// Never answers, so a call stays in flight until it is cancelled.
async fn hang() -> &'static str {
    std::future::pending().await
}

#[tokio::test]
async fn auxiliary_model_calls_the_given_model_once_without_tools_or_fallback() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = Router::new()
        .route("/hang/{*path}", post(hang))
        .route("/{*path}", post(completion))
        .with_state(seen.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let base = format!("http://{address}");
    let home = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target/provider-flow")
        .join(uuid::Uuid::new_v4().to_string());
    std::fs::create_dir_all(&home).unwrap();

    let root = Ctx::root().unwrap();
    root.plugin(
        dim_app_server::methods::ManagementMethodsPlugin::typed(&dim_app_server::catalog())
            .unwrap(),
    );
    root.plugin(FileStoragePlugin::new(vec![home.clone()]));
    root.plugin(SqliteStoragePlugin::new(home.join("native.sqlite")));
    root.plugin(ManagedProvidersPlugin);
    root.plugin(dim_app_server::codex_oauth::OpenAICodexOAuthPlugin::new(&names(), 0).unwrap());
    root.plugin(dim_app_server::grok_oauth::GrokOAuthPlugin::new(&names(), 0).unwrap());
    active(&root.plugin(dim_agent_aimux::AimuxPlugin)).await;
    let view = root.plugin(provider_part(ProviderPlugin::new));
    active(&view).await;
    let provider = root.get::<ProviderService>().unwrap();
    let auxiliary = root.get::<AuxiliaryModelHost>().unwrap();
    let cancel = CancellationToken::new();
    let saved = provider
        .create(
            input(
                AUX,
                &base,
                StaticAuth::Bearer,
                Some("aux-secret"),
                AUX,
                4096,
            ),
            &cancel,
        )
        .await
        .unwrap();
    provider
        .create(
            input(
                HANG,
                &format!("{base}/hang"),
                StaticAuth::Bearer,
                Some("hang-secret"),
                HANG,
                4096,
            ),
            &cancel,
        )
        .await
        .unwrap();
    let request = |max_output_tokens| AuxiliaryRequest {
        system: Some("Title this conversation".into()),
        messages: vec![Message::text(Role::User, "hello")],
        max_output_tokens,
    };
    let max_tokens = |body: &Value| {
        body.get("max_tokens")
            .or_else(|| body.get("max_completion_tokens"))
            .and_then(Value::as_u64)
    };

    let reply = auxiliary
        .0
        .complete(target(AUX), request(Some(32)), cancel.clone())
        .await
        .unwrap();
    assert_eq!(reply.text, "ok");
    assert_eq!(reply.stop_reason, ModelStopReason::EndTurn);
    let sent = seen.lock().unwrap().pop().unwrap();
    assert_eq!(sent.marker.as_deref(), Some(AUX));
    assert_eq!(sent.authorization.as_deref(), Some("Bearer aux-secret"));
    assert_eq!(sent.body["model"], "same");
    assert!(sent.body.to_string().contains("Title this conversation"));
    assert!(sent.body.to_string().contains("hello"));
    assert!(sent.body.get("tools").is_none());
    assert_eq!(max_tokens(&sent.body), Some(32));
    auxiliary
        .0
        .complete(target(AUX), request(Some(1_000_000)), cancel.clone())
        .await
        .unwrap();
    assert_eq!(
        max_tokens(&seen.lock().unwrap().pop().unwrap().body),
        Some(4096)
    );

    let mut empty = request(None);
    empty.messages.clear();
    for invalid in [empty, request(Some(0))] {
        let error = auxiliary
            .0
            .complete(target(AUX), invalid, cancel.clone())
            .await
            .unwrap_err();
        assert_eq!(error.code, AuxiliaryErrorCode::InvalidRequest);
    }
    assert!(seen.lock().unwrap().is_empty());

    let call_cancel = CancellationToken::new();
    let call = auxiliary
        .0
        .complete(target(HANG), request(None), call_cancel.clone());
    tokio::pin!(call);
    tokio::select! {
        result = &mut call => panic!("unexpected {result:?}"),
        _ = tokio::time::sleep(std::time::Duration::from_millis(100)) => {}
    }
    call_cancel.cancel();
    let error = tokio::time::timeout(std::time::Duration::from_secs(5), call)
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(error.code, AuxiliaryErrorCode::Cancelled);
    assert!(!error.message.contains("secret"));
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_millis(100),
            auxiliary
                .0
                .complete(target(HANG), request(None), cancel.clone()),
        )
        .await
        .is_err()
    );

    let disabled = provider
        .set_enabled_models(AUX.into(), saved.version.unwrap(), vec![], &cancel)
        .await
        .unwrap();
    let error = auxiliary
        .0
        .complete(target(AUX), request(None), cancel.clone())
        .await
        .unwrap_err();
    assert_eq!(error.code, AuxiliaryErrorCode::ModelUnavailable);
    assert!(seen.lock().unwrap().is_empty());
    provider
        .delete_static(AUX.into(), disabled.version, &cancel)
        .await
        .unwrap();
    let error = auxiliary
        .0
        .complete(target(AUX), request(None), cancel.clone())
        .await
        .unwrap_err();
    assert_eq!(error.code, AuxiliaryErrorCode::ModelUnavailable);
    assert!(seen.lock().unwrap().is_empty());

    let pending = tokio::spawn({
        let auxiliary = auxiliary.clone();
        async move {
            auxiliary
                .0
                .complete(target(HANG), request(None), CancellationToken::new())
                .await
        }
    });
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    view.dispose().await.unwrap();
    let error = tokio::time::timeout(std::time::Duration::from_secs(5), pending)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert_eq!(error.code, AuxiliaryErrorCode::Cancelled);
    root.shutdown().await.unwrap();
    server.abort();
    std::fs::remove_dir_all(home).unwrap();
}

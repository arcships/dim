use super::*;
use axum::{Router, body::Body, http::StatusCode, routing::post};
use dim_agent_capabilities::{
    adapters::DefaultPrepareModelPlugin,
    history::HistoryRenderer,
    host::{
        CompactionStrategyHost, ContextModelHost, IdentityContext, ModelProtocol,
        PrepareContextHost,
    },
    model_provider::ModelProviderPlugin,
    workspace_tools::WorkspaceToolsConfig,
};
use dim_agent_runtime::Feature;
use dim_app_server::{
    managed_provider::{
        AccountSnapshot, AccountView, CredentialLease, LeaseOwner, ManagedCatalog, ManagedError,
        ManagedFuture, ManagedModel, ManagedProvider, ManagedProviderRow, ProviderDescriptor,
        WebSearchRequest, WebSearchResponse,
        management::{
            ApplyReset, BrowserLogin, BrowserLoginBegin, DeviceLogin, LoginBegin, LoginState,
            ManagedBrowserLoginComplete, ManagedLoginAttempt, Referral, ResetApplied, ResetBatches,
            Usage,
        },
    },
    model_catalog::ModelEntry,
};
use rutis::Ctx;
use rutis::host::BoxFuture;
use std::sync::{Mutex as StdMutex, Weak};
use tokio::sync::watch;

/// What the account's catalog reports for its second model.
#[derive(Clone, Copy, PartialEq)]
enum Limited {
    /// The catalog holds the one fixture model.
    Absent,
    /// A second model the account can use.
    Available,
    /// A second model whose limits are undeclared, so it cannot be used.
    Unavailable,
    /// A second model the catalog cannot identify, so only the provider's own
    /// statement about the id can give it limits.
    Unidentified,
    ProviderUnavailable,
    /// The same account's refreshed catalog declares reasoning efforts and
    /// image input for the fixture model, as an upstream capability roll-out
    /// does (#2189).
    Evolved,
    /// The same account's refreshed catalog stops declaring reasoning for the
    /// fixture model, so its row carries no metadata at all, the way a model
    /// whose supported levels are all dropped reads (#2189).
    Stripped,
    /// The same account's catalog stops listing the fixture model at all, the
    /// way a model upstream retires for one listing reads (#2189).
    Delisted,
}

/// A signed-in managed account whose first credential the relay rejects.
struct Fixture {
    base_url: String,
    account: StdMutex<AccountSnapshot>,
    limited: StdMutex<Limited>,
    recovered: AtomicUsize,
    released: StdMutex<Vec<String>>,
    watch: watch::Sender<Option<bool>>,
}
impl LeaseOwner for Fixture {
    fn release(&self, lease_id: &str) {
        self.released.lock().unwrap().push(lease_id.into());
    }
}
struct Provider(Arc<Fixture>);
impl Provider {
    fn lease(&self, bearer: &str, id: &str) -> CredentialLease {
        let owner: Weak<dyn LeaseOwner> = Arc::downgrade(&self.0) as Weak<Fixture>;
        CredentialLease::new(
            self.0.account.lock().unwrap().clone(),
            ModelProtocol::OpenaiCompatible,
            self.0.base_url.clone(),
            Default::default(),
            bearer.into(),
            id.into(),
            owner,
        )
    }
}
fn ready<T: Send + 'static>(value: Result<T, ManagedError>) -> ManagedFuture<T> {
    Box::pin(async move { value })
}
impl ManagedProvider for Provider {
    fn providers(&self) -> Vec<ProviderDescriptor> {
        vec![ProviderDescriptor {
            provider_id: "managed-fixture".into(),
            display_name: "Managed fixture".into(),
            login_methods: vec!["browser".into()],
            default_directory: Some("https://accounts.example".into()),
        }]
    }
    fn status(&self, provider: &str) -> ManagedFuture<AccountSnapshot> {
        ready(match provider {
            "fail" => Err(ManagedError::InvalidStore),
            _ if *self.0.limited.lock().unwrap() == Limited::ProviderUnavailable => {
                Err(ManagedError::Transient)
            }
            _ => Ok(self.0.account.lock().unwrap().clone()),
        })
    }
    fn subscribe_account(&self) -> watch::Receiver<Option<bool>> {
        self.0.watch.subscribe()
    }
    fn recover(&self, _: &str) -> ManagedFuture<AccountSnapshot> {
        ready(Ok(self.0.account.lock().unwrap().clone()))
    }
    fn begin_browser(&self, _: BrowserLoginBegin) -> ManagedFuture<BrowserLogin> {
        ready(Err(ManagedError::InvalidInput))
    }
    fn complete_browser(&self, _: ManagedBrowserLoginComplete) -> ManagedFuture<AccountSnapshot> {
        ready(Err(ManagedError::InvalidInput))
    }
    fn begin_device(&self, _: LoginBegin) -> ManagedFuture<DeviceLogin> {
        ready(Err(ManagedError::InvalidInput))
    }
    fn attempt_status(&self, _: &ManagedLoginAttempt) -> Result<LoginState, ManagedError> {
        Ok(LoginState::Failed {
            error: "private-error-secret".into(),
        })
    }
    fn cancel_attempt(&self, _: ManagedLoginAttempt) -> ManagedFuture<()> {
        ready(Ok(()))
    }
    fn logout(&self, _: &str) -> ManagedFuture<AccountSnapshot> {
        ready(Ok(self.0.account.lock().unwrap().clone()))
    }
    fn usage(&self, provider: &str) -> ManagedFuture<Usage> {
        ready(match provider {
            "managed-fixture" => Ok(serde_json::from_value(json!({
                "credits":{"usedUnits":25000,"totalUnits":300000},
                "windows":[{"durationHours":5,"tokenUsed":1200,"tokenCap":10000}],
                "features":[{"key":"web_search","used":211}]}))
            .unwrap()),
            _ => Err(ManagedError::TokenMissing),
        })
    }
    fn referral(&self, _: &str) -> ManagedFuture<Referral> {
        ready(Ok(serde_json::from_value(
            json!({"enabled":true,"eligible":false,
            "invitedCount":5,"paidCount":2,"rewardGrantedUnits":12000,
            "inviterRebateBp":2000,"inviteeRebateBp":1000}),
        )
        .unwrap()))
    }
    fn reset_batches(&self, _: &str) -> ManagedFuture<ResetBatches> {
        ready(Ok(serde_json::from_value(
            json!({"window":[],"monthlyFull":[]}),
        )
        .unwrap()))
    }
    fn apply_reset(&self, request: ApplyReset) -> ManagedFuture<ResetApplied> {
        assert_eq!(request.subscription_id, 42);
        assert_eq!(request.idempotency_key, "confirmed-action");
        ready(Ok(serde_json::from_value(
            json!({"credits":{"usedUnits":0},"windows":[],
            "resets":{"window":{"availableCount":0},"monthlyFull":{"availableCount":0}},
            "replayed":false,"appliedAt":"2026-10-30T00:00:00Z"}),
        )
        .unwrap()))
    }
    fn web_search(
        &self,
        _: WebSearchRequest,
        _: CancellationToken,
    ) -> ManagedFuture<WebSearchResponse> {
        ready(Err(ManagedError::TokenMissing))
    }
    fn catalog(&self, _: &str, _: CancellationToken) -> ManagedFuture<ManagedCatalog> {
        let entry: ModelEntry = serde_json::from_value(json!({"modelId":"fixture-model",
        "displayName":"Fixture model","status":"active",
        "metadata":{"reasoning":{"supported":true,"effortOptions":["low","high"]}},
        "capabilities":dim_agent_types::CatalogCapabilities {
            reasoning: true,
            ..dim_agent_types::CatalogCapabilities::with_limits(32_000, 128)
        }}))
        .unwrap();
        let mut models = vec![ManagedModel {
            entry,
            protocol: ModelProtocol::OpenaiCompatible,
            unavailable_reason: None,
            zdr: false,
        }];
        match *self.0.limited.lock().unwrap() {
            Limited::Evolved => {
                let entry = &mut models[0].entry;
                entry.capabilities.reasoning = true;
                entry.capabilities.vision = true;
                entry.capabilities.modalities =
                    ["text", "image"].into_iter().map(str::to_owned).collect();
                let metadata = entry.metadata.get_or_insert_default();
                let reasoning = metadata.reasoning.get_or_insert_default();
                reasoning.supported = true;
                reasoning.effort_options = Some(
                    ["low", "medium", "high", "xhigh"]
                        .into_iter()
                        .map(str::to_owned)
                        .collect(),
                );
                reasoning.effort = Some("high".into());
            }
            Limited::Stripped => {
                let entry = &mut models[0].entry;
                entry.capabilities.reasoning = false;
                entry.metadata = None;
            }
            _ => {}
        }
        match *self.0.limited.lock().unwrap() {
            Limited::Absent | Limited::Evolved | Limited::Stripped => {}
            Limited::Delisted => models.clear(),
            Limited::ProviderUnavailable => return ready(Err(ManagedError::Transient)),
            Limited::Available => {
                let entry: ModelEntry = serde_json::from_value(json!({"modelId":"limited-model",
                    "displayName":"Limited model","status":"active",
                    "capabilities":dim_agent_types::CatalogCapabilities::with_limits(64_000, 256)}))
                .unwrap();
                models.push(ManagedModel {
                    entry,
                    protocol: ModelProtocol::OpenaiCompatible,
                    unavailable_reason: None,
                    zdr: false,
                });
            }
            Limited::Unavailable => {
                let entry: ModelEntry = serde_json::from_value(json!({"modelId":"limited-model",
                    "displayName":"Limited model","status":"active",
                    "capabilities":dim_agent_types::CatalogCapabilities::default()}))
                .unwrap();
                models.push(ManagedModel {
                    entry,
                    protocol: ModelProtocol::OpenaiCompatible,
                    unavailable_reason: Some("limits_undeclared".into()),
                    zdr: false,
                });
            }
            Limited::Unidentified => {
                // The account's listing writes where it matched an entry, as
                // `dim-oauth` does: the derivation on read replaces it.
                let listed = |id: &str, entry: Value| -> ModelEntry {
                    let mut value: Value = entry;
                    value["metadata"]["matchedProviderId"] = json!("dimcode-api-oauth");
                    value["metadata"]["matchedModelId"] = json!(id);
                    value["metadata"]["matchStrategy"] = json!("dim-oauth-exact");
                    serde_json::from_value(value).unwrap()
                };
                let entry = listed(
                    "house-deployment",
                    json!({"modelId":"house-deployment",
                    "displayName":"House deployment","status":"active",
                    "capabilities":dim_agent_types::CatalogCapabilities::default()}),
                );
                models.push(ManagedModel {
                    entry,
                    protocol: ModelProtocol::OpenaiCompatible,
                    unavailable_reason: Some("limits_undeclared".into()),
                    zdr: false,
                });
                // A listing that states one limit itself: the window is the
                // account's own number and only the output cap is missing.
                let entry = listed(
                    "house-partial",
                    json!({"modelId":"house-partial",
                    "displayName":"House partial","status":"active",
                    "metadata":{"capabilityOverrideFields":["contextWindow"]},
                    "capabilities":dim_agent_types::CatalogCapabilities {
                        context_window: Some(200_000), ..Default::default() }}),
                );
                models.push(ManagedModel {
                    entry,
                    protocol: ModelProtocol::OpenaiCompatible,
                    unavailable_reason: Some("limits_undeclared".into()),
                    zdr: false,
                });
            }
        }
        ready(Ok(ManagedCatalog {
            account: self.0.account.lock().unwrap().clone(),
            models,
        }))
    }
    fn credential(&self, _: &str, _: &str) -> ManagedFuture<CredentialLease> {
        ready(Ok(self.lease("initial-fixture-secret", "initial")))
    }
    fn recover_rejected(&self, _: &CredentialLease) -> ManagedFuture<CredentialLease> {
        self.0.recovered.fetch_add(1, Ordering::SeqCst);
        ready(Ok(self.lease("recovered-fixture-secret", "recovered")))
    }
    fn finish_rejected(&self, _: &CredentialLease) -> ManagedFuture<()> {
        ready(Ok(()))
    }
    fn quiesce(&self) -> BoxFuture<'static, ()> {
        Box::pin(async {})
    }
}
struct FixturePlugin(Arc<Fixture>);
impl Plugin for FixturePlugin {
    type Deps = ();
    fn name(&self) -> &str {
        "ManagedFixture"
    }
    async fn apply(&self, ctx: &Ctx, (): ()) -> rutis::Result {
        ctx.provide_as::<dyn ManagedProvider>(Arc::new(Provider(self.0.clone())))?;
        Ok(())
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn managed_model_writes_expose_typed_catalog_recovery() {
    let home = std::env::temp_dir().join(format!(
        "dm-{}",
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    ));
    std::fs::create_dir_all(&home).unwrap();
    let home = home.canonicalize().unwrap();
    let fixture = Arc::new(Fixture {
        base_url: "http://127.0.0.1:1/v1".into(),
        account: StdMutex::new(
            serde_json::from_value(json!({
                "credentialEpoch":"private-epoch", "generation":1,
                "account":{"issuer":"https://accounts.example", "subject":"fixture",
                    "email":"private@example.com", "name":"Private name", "grantedScopes":[]}
            }))
            .unwrap(),
        ),
        limited: StdMutex::new(Limited::Absent),
        recovered: AtomicUsize::new(0),
        released: StdMutex::new(vec![]),
        watch: watch::channel(Some(true)).0,
    });
    let provider = fixture.clone();
    let server = launch(
        Arc::new(MockTools),
        ServerConfig {
            product_home: Some(home.clone()),
            target_home: Some(home.join("rust")),
            managed_provider: Some(ManagedProviderRow(Arc::new(move |builtins, _| {
                let provider = provider.clone();
                builtins
                    .builtins
                    .register::<Value, _, _>("ManagedFixture", move |_| {
                        Ok(FixturePlugin(provider.clone()))
                    });
                "ManagedFixture".to_owned()
            }))),
            ..Default::default()
        },
    )
    .await;
    let mut client = Client::new_version(&server, false, 3).await;
    let refreshed = client
        .rpc("provider.refresh", json!({"providerId":"managed-fixture"}))
        .await;
    assert_ok(&refreshed);
    let mut version = refreshed["result"]["version"].as_u64().unwrap();
    for change in ["generation", "epoch", "identity", "logout"] {
        let account = {
            let mut account = fixture.account.lock().unwrap();
            account.generation += 1;
            match change {
                "epoch" => account.credential_epoch = "private-recovered-epoch".into(),
                "identity" => account.account.as_mut().unwrap().subject = "other-account".into(),
                "logout" => account.account = None,
                _ => {}
            }
            account.account.clone()
        };
        for (method, fields) in [
            ("provider.models.set_enabled", json!({"modelIds":[]})),
            (
                "provider.models.edit",
                json!({"modelId":"fixture-model","edit":{"displayName":"Edited"}}),
            ),
            ("provider.models.reset", json!({"modelId":"fixture-model"})),
            (
                "provider.preferences.update",
                json!({"modelId":"fixture-model","patch":{"reasoningEffort":{"action":"set","value":"high"}}}),
            ),
        ] {
            let mut params = fields.as_object().unwrap().clone();
            params.insert("providerId".into(), json!("managed-fixture"));
            params.insert("expectedVersion".into(), json!(version));
            let stale = client.rpc(method, Value::Object(params.clone())).await;
            assert_eq!(stale["error"]["code"], -32000, "{method}: {stale}");
            assert_eq!(
                stale["error"]["data"],
                json!({
                    "appError":"provider_catalog_stale", "providerId":"managed-fixture",
                    "expectedVersion":version, "currentVersion":version,
                    "issuer":account.as_ref().map(|account| &account.issuer),
                    "subject":account.as_ref().map(|account| &account.subject),
                    "requiredAction":if account.is_some() { "provider.refresh" } else { "account.login" }
                }),
                "{change}, {method}: {stale}"
            );
            params.insert("expectedVersion".into(), json!(version - 1));
            let conflict = client.rpc(method, Value::Object(params)).await;
            assert_eq!(
                conflict["error"]["data"],
                json!({"appError":"provider_conflict"}),
                "{method}: {conflict}"
            );
        }
        let read = client
            .rpc(
                "provider.management",
                json!({"providerId":"managed-fixture"}),
            )
            .await;
        assert_ok(&read);
        assert_eq!(read["result"]["providers"][0]["version"], version);
        if account.is_some() {
            let refreshed = client
                .rpc("provider.refresh", json!({"providerId":"managed-fixture"}))
                .await;
            assert_ok(&refreshed);
            assert_eq!(refreshed["result"]["version"], version + 1);
            let retried = client
                .rpc(
                    "provider.models.set_enabled",
                    json!({"providerId":"managed-fixture",
                "expectedVersion":version + 1,"modelIds":[]}),
                )
                .await;
            assert_ok(&retried);
            assert_eq!(retried["result"]["version"], version + 2);
            version += 2;
        }
    }
    assert_ok(&client.rpc("daemon.shutdown", json!({})).await);
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_injected_managed_provider_serves_accounts_and_recovers_credentials() {
    let home = std::env::temp_dir().join(format!(
        "dm-{}",
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    ));
    std::fs::create_dir_all(&home).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
    let home = home.canonicalize().unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/v1", listener.local_addr().unwrap());
    let seen = Arc::new(StdMutex::new(vec![]));
    let seen_handler = seen.clone();
    let app = Router::new().route("/v1/chat/completions", post(move |headers: axum::http::HeaderMap| {
        let seen = seen_handler.clone();
        async move {
            let bearer = headers.get("authorization").unwrap().to_str().unwrap().to_owned();
            seen.lock().unwrap().push(bearer.clone());
            if bearer == "Bearer initial-fixture-secret" {
                return axum::response::Response::builder().status(StatusCode::UNAUTHORIZED)
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"error":{"message":"rejected"}}"#)).unwrap();
            }
            axum::response::Response::builder().header("content-type", "text/event-stream").body(Body::from(concat!(
                "data: {\"id\":\"fixture\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"managed reply\"},\"finish_reason\":null}]}\n\n",
                "data: {\"id\":\"fixture\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
                "data: [DONE]\n\n"))).unwrap()
        }
    }));
    let http = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let fixture = Arc::new(Fixture {
        base_url: base,
        account: StdMutex::new(AccountSnapshot {
            credential_epoch: "epoch".into(),
            generation: 1,
            account: Some(AccountView {
                issuer: "https://accounts.example".into(),
                subject: "fixture".into(),
                email: Some("fixture@example.com".into()),
                name: Some("Fixture".into()),
                granted_scopes: vec!["model".into()],
            }),
        }),
        limited: StdMutex::new(Limited::Absent),
        recovered: AtomicUsize::new(0),
        released: StdMutex::new(vec![]),
        watch: watch::channel(Some(true)).0,
    });
    let provider = fixture.clone();
    let mut builder = AgentBuilder::new(AgentConfig::default());
    defaults::install_local_execution(
        &mut builder,
        ModelProviderPlugin::process,
        WorkspaceToolsConfig {
            boundary: None,
            output_root: home.join("output"),
        },
        dim_agent_capabilities::timeline_tree::BranchToolConfig::default(),
    );
    dim_app_server::permission_rules::install(&mut builder);
    builder
        .session_value(Arc::new(HistoryRenderer::default()))
        .session_value(Arc::new(PrepareContextHost(Arc::new(IdentityContext))))
        .session_value(Arc::new(ContextModelHost(None)))
        .session_value(Arc::new(CompactionStrategyHost(None)))
        .require_feature(
            Feature::new("PrepareModelHost").session("PrepareModelHost", |names| {
                Ok(Box::new(DefaultPrepareModelPlugin::new(names)?))
            }),
        );
    let assets = web_entry::frontend_fixture();
    let config = ServerConfig {
        unix_path: Some(home.join("daemon.sock")),
        product_home: Some(home.clone()),
        target_home: Some(home.join("rust")),
        web_assets: Some(assets.clone()),
        managed_provider: Some(ManagedProviderRow(Arc::new(move |builtins, _| {
            let provider = provider.clone();
            builtins
                .builtins
                .register::<Value, _, _>("ManagedFixture", move |_| {
                    Ok(FixturePlugin(provider.clone()))
                });
            "ManagedFixture".to_owned()
        }))),
        session_defaults: dim_agent_runtime::SessionConfig {
            tools: Some(["read"].map(String::from).to_vec()),
            ..Default::default()
        },
        ..Default::default()
    };
    let server = serve(builder, config).await.unwrap();
    let mut client = Client::new_version(&server, false, 3).await;
    let opened = client.rpc("web.open", json!({"port":0})).await;
    assert_ok(&opened);
    let address = opened["result"]["address"].as_str().unwrap();
    let token = opened["result"]["url"]
        .as_str()
        .unwrap()
        .split_once("#bootstrap=")
        .unwrap()
        .1;
    let (status, head, _) = web_entry::http(
        address,
        web_entry::post(
            address,
            "/api/auth/exchange",
            &format!("http://{address}"),
            &json!({"token":token}).to_string(),
        ),
    )
    .await;
    assert_eq!(status, 204);
    let cookie = head
        .lines()
        .find_map(|line| line.strip_prefix("set-cookie: "))
        .unwrap()
        .split(';')
        .next()
        .unwrap();
    let mut web = web_entry::web_client(address, cookie).await;
    for method in [
        "account.providers",
        "account.status",
        "account.usage",
        "account.referral",
        "account.reset_batches",
        "account.apply_reset",
        "account.recover",
        "account.login.status",
        "provider.management",
    ] {
        assert!(web.features.as_array().unwrap().contains(&json!(method)));
        let params = match method {
            "account.providers" => json!({}),
            "account.login.status" => {
                json!({"providerId":"managed-fixture","attemptId":"failed-attempt"})
            }
            "account.apply_reset" => json!({"providerId":"managed-fixture","subscriptionId":42,
                "resetType":"window","idempotencyKey":"confirmed-action"}),
            _ => json!({"providerId":"managed-fixture"}),
        };
        let reply = web.rpc(method, params).await;
        assert_ok(&reply);
        for secret in [
            "initial-fixture-secret",
            "private-error-secret",
            "refresh_token",
        ] {
            assert!(!reply.to_string().contains(secret), "{method}");
        }
        if method == "account.providers" {
            assert_eq!(
                reply["result"][0]["defaultDirectory"],
                "https://accounts.example"
            );
        }
    }
    assert_eq!(
        web.rpc(
            "account.login.begin",
            json!({"providerId":"managed-fixture",
        "directory":"https://accounts.example"})
        )
        .await["error"]["data"]["appError"],
        "account_invalid_input"
    );
    assert_eq!(
        web.rpc("account.logout", json!({"providerId":"managed-fixture"}))
            .await["result"]["generation"],
        1
    );
    assert_eq!(
        web.rpc("daemon.shutdown", json!({})).await["error"]["data"]["appError"],
        "unknown_method"
    );
    assert_eq!(
        web.rpc(
            "account.referral",
            json!({"providerId":"managed-fixture",
        "issuer":"https://attacker.invalid"})
        )
        .await["error"]["code"],
        -32602
    );
    assert_eq!(
        client.rpc("account.providers", json!({})).await["result"][0]["providerId"],
        "managed-fixture"
    );
    let status = client
        .rpc("account.status", json!({"providerId":"managed-fixture"}))
        .await;
    assert_eq!(status["result"]["account"]["email"], "fixture@example.com");
    let usage = client
        .rpc("account.usage", json!({"providerId":"managed-fixture"}))
        .await;
    assert_eq!(usage["result"]["credits"]["totalUnits"], 300000);
    assert_eq!(usage["result"]["windows"][0]["durationHours"], 5);
    assert_eq!(
        client
            .rpc("account.usage", json!({"providerId":"other"}))
            .await["error"]["data"]["appError"],
        "account_required"
    );
    let error = client
        .rpc("account.status", json!({"providerId":"fail"}))
        .await;
    assert_eq!(error["error"]["data"]["appError"], "account_invalid_store");
    let failed = client
        .rpc(
            "account.login.status",
            json!({"providerId":"managed-fixture","attemptId":"failed-attempt"}),
        )
        .await;
    assert_eq!(
        failed["result"],
        json!({"status":"failed","error":"Login failed"})
    );
    assert_eq!(
        client
            .rpc(
                "account.credential",
                json!({"providerId":"managed-fixture"})
            )
            .await["error"]["code"],
        -32601
    );
    let unrefreshed = client
        .rpc(
            "provider.management",
            json!({"providerId":"managed-fixture"}),
        )
        .await;
    assert_ok(&unrefreshed);
    assert_eq!(
        unrefreshed["result"]["providers"][0]["version"],
        Value::Null
    );
    assert_eq!(unrefreshed["result"]["providers"][0]["models"], Value::Null);
    assert_ok(
        &client
            .rpc("provider.refresh", json!({"providerId":"managed-fixture"}))
            .await,
    );
    assert_ok(&client.rpc("provider.models.edit", json!({"providerId":"managed-fixture","expectedVersion":1,"modelId":"fixture-model","edit":{"displayName":"Edited fixture"}})).await);
    assert_ok(&client.rpc("provider.preferences.update", json!({"providerId":"managed-fixture","expectedVersion":2,"modelId":"fixture-model","patch":{"reasoningEffort":{"action":"set","value":"high"}}})).await);
    assert_ok(
        &client
            .rpc(
                "provider.models.set_enabled",
                json!({"providerId":"managed-fixture","expectedVersion":3,"modelIds":[]}),
            )
            .await,
    );
    let same_account = client
        .rpc("provider.refresh", json!({"providerId":"managed-fixture"}))
        .await;
    assert_ok(&same_account);
    assert_eq!(
        same_account["result"]["models"][0]["name"],
        "Edited fixture"
    );
    assert_eq!(
        same_account["result"]["disabledModelIds"],
        json!(["fixture-model"])
    );
    let retained = client
        .rpc(
            "provider.management",
            json!({"providerId":"managed-fixture"}),
        )
        .await;
    assert_eq!(
        retained["result"]["providers"][0]["preferences"]["fixture-model"]["reasoningEffort"],
        "high"
    );
    assert_ok(&client.rpc("provider.models.set_enabled", json!({"providerId":"managed-fixture","expectedVersion":5,"modelIds":["fixture-model"]})).await);
    // A catalog that holds one unavailable model still toggles the rest. The
    // client's toggle sends the whole enabled set, the unavailable model
    // included, and a set that omits the model leaves its state alone.
    *fixture.limited.lock().unwrap() = Limited::Unavailable;
    let mixed = client
        .rpc("provider.refresh", json!({"providerId":"managed-fixture"}))
        .await;
    assert_ok(&mixed);
    assert_eq!(mixed["result"]["disabledModelIds"], json!([]));
    let startup = client
        .rpc(
            "session.create",
            json!({"cwd":home,
        "model":{"provider":"managed-fixture","modelId":"limited-model"},
        "startup":{"contextWindow":64000,"maxOutputTokens":256}}),
        )
        .await;
    assert_ok(&startup);
    let startup_id = startup["result"]["sessionId"].as_str().unwrap();
    for turn in 0..2 {
        if turn == 1 {
            assert_ok(
                &client
                    .rpc(
                        "session.model.update",
                        json!({"sessionId":startup_id,"branchId":"main",
                "modelPreferences":{"reasoningEffort":{"action":"clear"}}}),
                    )
                    .await,
            );
        }
        assert_ok(
            &client
                .rpc(
                    "session.submit",
                    json!({"sessionId":startup_id,"branchId":"main","text":"startup"}),
                )
                .await,
        );
        assert_ok(
            &client
                .rpc(
                    "session.idle",
                    json!({"sessionId":startup_id,"branchId":"main"}),
                )
                .await,
        );
        assert_eq!(
            seen.lock().unwrap().len(),
            (turn + 1) * 2,
            "managed startup reaches the relay with adopted limits"
        );
    }
    let unchanged = client
        .rpc("provider.catalog", json!({"providerId":"managed-fixture"}))
        .await;
    assert_eq!(
        unchanged["result"]["catalog"]["models"][1]["unavailableReason"],
        "limits_undeclared"
    );
    let listed = client
        .rpc(
            "provider.management",
            json!({"providerId":"managed-fixture"}),
        )
        .await;
    assert_eq!(
        listed["result"]["providers"][0]["models"][1]["unavailableReason"],
        "limits_undeclared"
    );
    // The set keeps an unavailable entry's enablement, so the view offers no
    // toggle for it; the available entry beside it still offers one.
    let actions = listed["result"]["providers"][0]["models"][1]["actions"]
        .as_array()
        .unwrap()
        .clone();
    assert!(!actions.contains(&json!("set_enabled")), "{actions:?}");
    assert!(actions.contains(&json!("edit")), "{actions:?}");
    let actions = listed["result"]["providers"][0]["models"][0]["actions"]
        .as_array()
        .unwrap()
        .clone();
    assert!(actions.contains(&json!("set_enabled")), "{actions:?}");
    assert_ok(
        &client
            .rpc(
                "provider.models.set_enabled",
                json!({"providerId":"managed-fixture","expectedVersion":7,
                    "modelIds":["fixture-model","limited-model"]}),
            )
            .await,
    );
    let omitted = client
        .rpc(
            "provider.models.set_enabled",
            json!({"providerId":"managed-fixture","expectedVersion":8,
                "modelIds":["fixture-model"]}),
        )
        .await;
    assert_ok(&omitted);
    assert_eq!(omitted["result"]["disabledModelIds"], json!([]));
    // An entry that was disabled while it was usable and is unavailable now
    // keeps that disable when a set names it: the set governs only the
    // entries the account can use.
    *fixture.limited.lock().unwrap() = Limited::Available;
    let usable = client
        .rpc("provider.refresh", json!({"providerId":"managed-fixture"}))
        .await;
    assert_ok(&usable);
    assert_eq!(usable["result"]["disabledModelIds"], json!([]));
    let disabled = client
        .rpc(
            "provider.models.set_enabled",
            json!({"providerId":"managed-fixture","expectedVersion":10,
                "modelIds":["fixture-model"]}),
        )
        .await;
    assert_ok(&disabled);
    assert_eq!(
        disabled["result"]["disabledModelIds"],
        json!(["limited-model"])
    );
    *fixture.limited.lock().unwrap() = Limited::Unavailable;
    let frozen = client
        .rpc("provider.refresh", json!({"providerId":"managed-fixture"}))
        .await;
    assert_ok(&frozen);
    assert_eq!(
        frozen["result"]["disabledModelIds"],
        json!(["limited-model"])
    );
    let named = client
        .rpc(
            "provider.models.set_enabled",
            json!({"providerId":"managed-fixture","expectedVersion":12,
                "modelIds":["fixture-model","limited-model"]}),
        )
        .await;
    assert_ok(&named);
    assert_eq!(
        named["result"]["disabledModelIds"],
        json!(["limited-model"])
    );
    let created = client
        .rpc(
            "session.create",
            json!({"cwd":home,"model":{"provider":"managed-fixture","modelId":"fixture-model"}}),
        )
        .await;
    assert_ok(&created);
    let sid = created["result"]["sessionId"].as_str().unwrap();
    assert_ok(
        &client
            .rpc(
                "session.submit",
                json!({"sessionId":sid,"branchId":"main","text":"Hello"}),
            )
            .await,
    );
    server
        .agent()
        .unwrap()
        .get_session(sid)
        .unwrap()
        .idle("main")
        .await
        .unwrap();
    let history = client
        .rpc(
            "session.history",
            json!({"sessionId":sid,"branchId":"main","after":0,"limit":1000}),
        )
        .await;
    assert_ok(&history);
    let history = history.to_string();
    assert!(history.contains("managed reply"), "{history}");
    assert!(!history.contains("fixture-secret"));
    assert_eq!(
        *seen.lock().unwrap(),
        [
            "Bearer initial-fixture-secret",
            "Bearer recovered-fixture-secret",
            "Bearer initial-fixture-secret",
            "Bearer recovered-fixture-secret",
            "Bearer initial-fixture-secret",
            "Bearer recovered-fixture-secret"
        ]
    );
    assert_eq!(fixture.recovered.load(Ordering::SeqCst), 3);
    let selected = client
        .rpc(
            "provider.select",
            json!({"cwd":home,"providerId":"managed-fixture","modelId":"fixture-model"}),
        )
        .await;
    assert_ok(&selected);
    *fixture.limited.lock().unwrap() = Limited::ProviderUnavailable;
    let unavailable_default = client
        .rpc(
            "session.create",
            json!({"cwd":home,"model":{"modelId":"fixture-model"},"startup":{}}),
        )
        .await;
    let message = unavailable_default["error"]["message"].as_str().unwrap();
    assert!(message.contains("Model is unavailable"), "{message}");
    assert!(message.contains("catalog error:"), "{message}");
    assert!(
        message.contains("Managed account operation failed"),
        "{message}"
    );
    *fixture.limited.lock().unwrap() = Limited::Unavailable;
    let default_provider = uuid::Uuid::new_v4().to_string();
    assert_ok(&client.rpc("provider.create", json!({"providerId":default_provider,
        "displayName":"Default provider","enabled":true,
        "connection":{"driverKind":"openai-compatible","baseUrl":"http://127.0.0.1:1/v1","auth":{"type":"anonymous"}},
        "models":[{"modelId":"default-model","displayName":"Default model","status":"active",
            "capabilities":dim_agent_types::CatalogCapabilities::with_limits(32000,128)}]})).await);
    assert_ok(
        &client
            .rpc(
                "provider.select",
                json!({"cwd":home,"providerId":default_provider,"modelId":"default-model",
                    "expectedVersion":selected["result"]["version"]}),
            )
            .await,
    );
    *fixture.limited.lock().unwrap() = Limited::ProviderUnavailable;
    let unavailable = client
        .rpc("provider.catalog", json!({"providerId":"managed-fixture"}))
        .await;
    assert!(unavailable.get("error").is_some(), "{unavailable}");
    for duplicate in [false, true] {
        let provider_id = uuid::Uuid::new_v4().to_string();
        assert_ok(&client.rpc("provider.create", json!({"providerId":provider_id,
            "displayName":"Healthy provider","enabled":true,
            "connection":{"driverKind":"openai-compatible","baseUrl":"http://127.0.0.1:1/v1","auth":{"type":"anonymous"}},
            "models":[{"modelId":"reachable-model","displayName":"Reachable model","status":"active",
                "capabilities":dim_agent_types::CatalogCapabilities::with_limits(32000,128)}]})).await);
        let found = client
            .rpc(
                "session.create",
                json!({"cwd":home,"model":{"modelId":"reachable-model"},"startup":{}}),
            )
            .await;
        if duplicate {
            assert!(
                found["error"]["message"]
                    .as_str()
                    .unwrap()
                    .contains("ambiguous"),
                "{found}"
            );
        } else {
            assert_ok(&found);
            assert_eq!(found["result"]["model"]["providerId"], provider_id);
        }
    }
    let explicit = client
        .rpc(
            "session.create",
            json!({"cwd":home,"model":{"provider":"managed-fixture","modelId":"fixture-model"},"startup":{}}),
        )
        .await;
    assert!(explicit.get("error").is_some(), "{explicit}");
    *fixture.limited.lock().unwrap() = Limited::Unavailable;
    {
        let mut account = fixture.account.lock().unwrap();
        account.generation += 1;
        account.account.as_mut().unwrap().subject = "different-account".into();
    }
    let stale = client
        .rpc(
            "provider.management",
            json!({"providerId":"managed-fixture"}),
        )
        .await;
    assert_ok(&stale);
    assert_eq!(stale["result"]["providers"][0]["version"], 13);
    assert_eq!(
        stale["result"]["providers"][0]["models"][0]["unavailableReason"],
        "account_changed"
    );
    assert!(
        !stale["result"]["providers"][0]["actions"]
            .as_array()
            .unwrap()
            .contains(&json!("preferences.update"))
    );
    let changed = client
        .rpc("provider.refresh", json!({"providerId":"managed-fixture"}))
        .await;
    assert_ok(&changed);
    assert_eq!(changed["result"]["models"][0]["name"], "Fixture model");
    assert_eq!(changed["result"]["disabledModelIds"], json!([]));
    let cleared = client
        .rpc(
            "provider.management",
            json!({"providerId":"managed-fixture"}),
        )
        .await;
    assert_eq!(cleared["result"]["providers"][0]["preferences"], json!({}));
    {
        let mut account = fixture.account.lock().unwrap();
        account.generation += 1;
        account.account = None;
    }
    let signed_out = client
        .rpc(
            "provider.management",
            json!({"providerId":"managed-fixture"}),
        )
        .await;
    assert_ok(&signed_out);
    assert_eq!(signed_out["result"]["providers"][0]["version"], 14);
    assert_eq!(
        signed_out["result"]["providers"][0]["models"][0]["unavailableReason"],
        "account_required"
    );
    assert_ok(&client.rpc("daemon.shutdown", json!({})).await);
    server.join().await.unwrap();
    let mut released = fixture.released.lock().unwrap().clone();
    released.sort();
    assert_eq!(
        released,
        [
            "initial",
            "initial",
            "initial",
            "recovered",
            "recovered",
            "recovered"
        ]
    );
    http.abort();
    std::fs::remove_dir_all(assets).unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// A managed model whose stored reason is `limits_undeclared` becomes usable
/// when an edit declares its limits: the whole set then governs it. A reset
/// puts the stored reason back in force, and a set that omits it leaves its
/// enablement as it was.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_managed_model_made_usable_by_its_edit_is_governed_by_its_effective_limits() {
    let home = std::env::temp_dir().join(format!(
        "dm-{}",
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    ));
    std::fs::create_dir_all(&home).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
    let home = home.canonicalize().unwrap();
    let fixture = Arc::new(Fixture {
        base_url: "http://127.0.0.1:1/v1".into(),
        account: StdMutex::new(AccountSnapshot {
            credential_epoch: "epoch".into(),
            generation: 1,
            account: Some(AccountView {
                issuer: "https://accounts.example".into(),
                subject: "fixture".into(),
                email: Some("fixture@example.com".into()),
                name: Some("Fixture".into()),
                granted_scopes: vec!["model".into()],
            }),
        }),
        limited: StdMutex::new(Limited::Unavailable),
        recovered: AtomicUsize::new(0),
        released: StdMutex::new(vec![]),
        watch: watch::channel(Some(true)).0,
    });
    let provider = fixture.clone();
    let mut builder = AgentBuilder::new(AgentConfig::default());
    defaults::install_local_execution(
        &mut builder,
        ModelProviderPlugin::process,
        WorkspaceToolsConfig {
            boundary: None,
            output_root: home.join("output"),
        },
        dim_agent_capabilities::timeline_tree::BranchToolConfig::default(),
    );
    dim_app_server::permission_rules::install(&mut builder);
    builder
        .session_value(Arc::new(HistoryRenderer::default()))
        .session_value(Arc::new(PrepareContextHost(Arc::new(IdentityContext))))
        .session_value(Arc::new(ContextModelHost(None)))
        .session_value(Arc::new(CompactionStrategyHost(None)))
        .require_feature(
            Feature::new("PrepareModelHost").session("PrepareModelHost", |names| {
                Ok(Box::new(DefaultPrepareModelPlugin::new(names)?))
            }),
        );
    let config = ServerConfig {
        unix_path: Some(home.join("daemon.sock")),
        product_home: Some(home.clone()),
        target_home: Some(home.join("rust")),
        managed_provider: Some(ManagedProviderRow(Arc::new(move |builtins, _| {
            let provider = provider.clone();
            builtins
                .builtins
                .register::<Value, _, _>("ManagedFixture", move |_| {
                    Ok(FixturePlugin(provider.clone()))
                });
            "ManagedFixture".to_owned()
        }))),
        session_defaults: dim_agent_runtime::SessionConfig {
            tools: Some(["read"].map(String::from).to_vec()),
            ..Default::default()
        },
        ..Default::default()
    };
    let server = serve(builder, config).await.unwrap();
    let mut client = Client::new_version(&server, false, 3).await;
    let created = client
        .rpc("provider.refresh", json!({"providerId":"managed-fixture"}))
        .await;
    assert_ok(&created);
    assert_eq!(created["result"]["version"], 1);
    let listed = client
        .rpc(
            "provider.management",
            json!({"providerId":"managed-fixture"}),
        )
        .await;
    assert_ok(&listed);
    let models = listed["result"]["providers"][0]["models"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(models[1]["model"]["modelId"], "limited-model");
    assert_eq!(models[1]["unavailableReason"], "limits_undeclared");
    assert!(
        !models[1]["actions"]
            .as_array()
            .unwrap()
            .contains(&json!("set_enabled")),
        "{models:?}"
    );
    // The edit declares the limits the base entry lacks; the stored reason
    // stays, and the effective one is derived from what the read now holds.
    assert_ok(
        &client
            .rpc(
                "provider.models.edit",
                json!({"providerId":"managed-fixture","expectedVersion":1,
                    "modelId":"limited-model",
                    "edit":{"contextWindow":128000,"maxOutputTokens":4096}}),
            )
            .await,
    );
    let usable = client
        .rpc(
            "provider.management",
            json!({"providerId":"managed-fixture"}),
        )
        .await;
    assert_ok(&usable);
    assert_eq!(
        usable["result"]["providers"][0]["models"][1]["unavailableReason"],
        Value::Null
    );
    assert!(
        usable["result"]["providers"][0]["models"][1]["actions"]
            .as_array()
            .unwrap()
            .contains(&json!("set_enabled")),
        "the model's own edit makes it toggleable"
    );
    // A set that omits the now-usable model disables it.
    let disabled = client
        .rpc(
            "provider.models.set_enabled",
            json!({"providerId":"managed-fixture","expectedVersion":2,
                "modelIds":["fixture-model"]}),
        )
        .await;
    assert_ok(&disabled);
    assert_eq!(
        disabled["result"]["disabledModelIds"],
        json!(["limited-model"])
    );
    let reread = client
        .rpc(
            "provider.management",
            json!({"providerId":"managed-fixture"}),
        )
        .await;
    assert_eq!(
        reread["result"]["providers"][0]["models"][1]["enabled"],
        false
    );
    // A set that names it enables it again.
    let enabled = client
        .rpc(
            "provider.models.set_enabled",
            json!({"providerId":"managed-fixture","expectedVersion":3,
                "modelIds":["fixture-model","limited-model"]}),
        )
        .await;
    assert_ok(&enabled);
    assert_eq!(enabled["result"]["disabledModelIds"], json!([]));
    // Reset removes the override: the stored reason applies again, so the
    // set does not govern the model and a set that omits it changes nothing.
    assert_ok(
        &client
            .rpc(
                "provider.models.reset",
                json!({"providerId":"managed-fixture","expectedVersion":4,
                    "modelId":"limited-model"}),
            )
            .await,
    );
    let reset = client
        .rpc(
            "provider.management",
            json!({"providerId":"managed-fixture"}),
        )
        .await;
    assert_ok(&reset);
    assert_eq!(
        reset["result"]["providers"][0]["models"][1]["unavailableReason"],
        "limits_undeclared"
    );
    let omitted = client
        .rpc(
            "provider.models.set_enabled",
            json!({"providerId":"managed-fixture","expectedVersion":5,
                "modelIds":["fixture-model"]}),
        )
        .await;
    assert_ok(&omitted);
    assert_eq!(omitted["result"]["disabledModelIds"], json!([]));
    assert_ok(&client.rpc("daemon.shutdown", json!({})).await);
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// An account provider identifies its models with the statements its own
/// record makes, exactly as any other provider does: an id the catalog cannot
/// read gains the limits of the row its alias names, an alias-only edit writes
/// no descriptor override, clearing the alias returns the model to
/// `limits_undeclared`, and a refresh keeps the statement.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_account_identifies_its_models_with_its_prefix_and_aliases() {
    let home = std::env::temp_dir().join(format!(
        "dm-{}",
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    ));
    std::fs::create_dir_all(&home).unwrap();
    let home = home.canonicalize().unwrap();
    let fixture = Arc::new(Fixture {
        base_url: "http://127.0.0.1:1/v1".into(),
        account: StdMutex::new(
            serde_json::from_value(json!({
                "credentialEpoch":"epoch", "generation":1,
                "account":{"issuer":"https://accounts.example", "subject":"fixture",
                    "email":"fixture@example.com", "name":"Fixture", "grantedScopes":["model"]}
            }))
            .unwrap(),
        ),
        limited: StdMutex::new(Limited::Unidentified),
        recovered: AtomicUsize::new(0),
        released: StdMutex::new(vec![]),
        watch: watch::channel(Some(true)).0,
    });
    let provider = fixture.clone();
    let server = launch(
        Arc::new(MockTools),
        ServerConfig {
            product_home: Some(home.clone()),
            target_home: Some(home.join("rust")),
            managed_provider: Some(ManagedProviderRow(Arc::new(move |builtins, _| {
                let provider = provider.clone();
                builtins
                    .builtins
                    .register::<Value, _, _>("ManagedFixture", move |_| {
                        Ok(FixturePlugin(provider.clone()))
                    });
                "ManagedFixture".to_owned()
            }))),
            ..Default::default()
        },
    )
    .await;
    let mut client = Client::new_version(&server, false, 3).await;
    let refreshed = client
        .rpc("provider.refresh", json!({"providerId":"managed-fixture"}))
        .await;
    assert_ok(&refreshed);
    let mut version = refreshed["result"]["version"].as_u64().unwrap();
    let house = |catalog: &Value| -> usize {
        catalog["result"]["models"]
            .as_array()
            .unwrap()
            .iter()
            .position(|model| model["id"] == "house-deployment")
            .expect("the account lists the model no rule reads")
    };
    let at = house(&refreshed);
    assert_eq!(
        refreshed["result"]["models"][at]["unavailableReason"],
        "limits_undeclared"
    );
    // Nothing named the id, so the entry says so and names no row.
    let identified = |reply: &Value, at: usize| -> Value {
        reply["result"]["models"][at]["metadata"]["identification"].clone()
    };
    assert_eq!(
        identified(&refreshed, at),
        Value::Null,
        "an id no rule reads is unidentified, not left without an answer"
    );
    assert_eq!(
        (
            refreshed["result"]["models"][at]["metadata"].get("matchedModelId"),
            refreshed["result"]["models"][at]["metadata"].get("matchedProviderId"),
            refreshed["result"]["models"][at]["metadata"].get("matchStrategy"),
        ),
        (None, None, None),
        "the listing's own record of where it matched does not survive the read"
    );
    // The account's own statement about the id is what gives it the target
    // row's limits, which the catalog alone cannot.
    let aliased = client
        .rpc(
            "provider.models.edit",
            json!({"providerId":"managed-fixture","expectedVersion":version,
                "modelId":"house-deployment",
                "edit":{"alias":{"action":"set","value":"kimi-k3"}}}),
        )
        .await;
    assert_ok(&aliased);
    let at = house(&aliased);
    assert_eq!(
        aliased["result"]["models"][at]["unavailableReason"],
        Value::Null,
        "{}",
        aliased["result"]["models"][at]
    );
    assert_eq!(
        aliased["result"]["models"][at]["capabilities"]["contextWindow"],
        json!(1_048_576)
    );
    // A model the alias identified publishes the same record as any provider's
    // model: the rule that named the row, the row itself, and each limit the
    // account did not state with the row that supplied it.
    let metadata = &aliased["result"]["models"][at]["metadata"];
    assert_eq!(metadata["identification"]["rule"], "token");
    assert_eq!(metadata["identification"]["narrowedByDisplayName"], false);
    assert_eq!(metadata["matchedProviderId"], "moonshot");
    assert_eq!(metadata["matchedModelId"], "kimi-k3");
    assert_eq!(metadata["matchStrategy"], "moonshot-exact");
    let filled: Vec<(String, u64, String, String)> = metadata["identification"]["filled"]
        .as_array()
        .unwrap()
        .iter()
        .map(|limit| {
            (
                limit["field"].as_str().unwrap().into(),
                limit["value"].as_u64().unwrap(),
                limit["providerId"].as_str().unwrap().into(),
                limit["modelId"].as_str().unwrap().into(),
            )
        })
        .collect();
    assert_eq!(
        filled,
        [
            (
                "contextWindow".to_owned(),
                1_048_576,
                "moonshot".to_owned(),
                "kimi-k3".to_owned()
            ),
            (
                "maxOutputTokens".to_owned(),
                1_048_576,
                "moonshot".to_owned(),
                "kimi-k3".to_owned()
            ),
        ],
        "every supplied limit names its supplier"
    );
    version = aliased["result"]["version"].as_u64().unwrap();
    // A limit the account states itself is never listed as supplied, whatever
    // the identified row says.
    let partial = |reply: &Value| -> usize {
        reply["result"]["models"]
            .as_array()
            .unwrap()
            .iter()
            .position(|model| model["id"] == "house-partial")
            .expect("the account lists the model that states its own window")
    };
    let partial_edit = client
        .rpc(
            "provider.models.edit",
            json!({"providerId":"managed-fixture","expectedVersion":version,"modelId":"house-partial",
                "edit":{"alias":{"action":"set","value":"kimi-k3"}}}),
        )
        .await;
    assert_ok(&partial_edit);
    let partial_at = partial(&partial_edit);
    assert_eq!(
        partial_edit["result"]["models"][partial_at]["capabilities"]["contextWindow"],
        json!(200_000),
        "the account's own number stands"
    );
    assert_eq!(
        partial_edit["result"]["models"][partial_at]["capabilities"]["maxOutputTokens"],
        json!(1_048_576)
    );
    let limits: Vec<&str> =
        partial_edit["result"]["models"][partial_at]["metadata"]["identification"]["filled"]
            .as_array()
            .unwrap()
            .iter()
            .map(|limit| limit["field"].as_str().unwrap())
            .collect();
    assert_eq!(
        limits,
        ["maxOutputTokens"],
        "a limit the entry states is not one something else supplied"
    );
    version = partial_edit["result"]["version"].as_u64().unwrap();
    let stated = client
        .rpc(
            "provider.management",
            json!({"providerId":"managed-fixture"}),
        )
        .await;
    assert_ok(&stated);
    assert_eq!(
        stated["result"]["providers"][0]["models"][at]["alias"], "kimi-k3",
        "management says what the entry stands for"
    );
    let record = |home: &std::path::Path| -> Value {
        let store = rusqlite::Connection::open(home.join("rust").join("dimcode.sqlite")).unwrap();
        dim_app_server::provider::testing::stored_provider(&store, "managed-fixture")
            .unwrap()
            .unwrap()
    };
    assert_eq!(
        record(&home)["modelConfigOverrides"],
        json!([]),
        "an alias-only edit writes no override for the reset path to leave behind"
    );
    // Clearing the statement puts the model back where it was.
    let cleared = client
        .rpc(
            "provider.models.edit",
            json!({"providerId":"managed-fixture","expectedVersion":version,
                "modelId":"house-deployment",
                "edit":{"alias":{"action":"clear"}}}),
        )
        .await;
    assert_ok(&cleared);
    let at = house(&cleared);
    assert_eq!(
        cleared["result"]["models"][at]["unavailableReason"],
        "limits_undeclared"
    );
    assert_eq!(cleared["result"]["models"][at].get("alias"), None);
    assert_eq!(
        identified(&cleared, at),
        Value::Null,
        "the cleared alias leaves nothing naming the id"
    );
    assert_eq!(
        (
            cleared["result"]["models"][at]["metadata"].get("matchedModelId"),
            cleared["result"]["models"][at]["metadata"].get("matchedProviderId"),
            cleared["result"]["models"][at]["metadata"].get("matchStrategy"),
        ),
        (None, None, None),
        "no stale row survives the statement that named it"
    );
    assert_eq!(
        cleared["result"]["models"][at]["missingLimits"],
        json!(["contextWindow", "maxOutputTokens"])
    );
    version = cleared["result"]["version"].as_u64().unwrap();
    // A refresh rewrites the account's listing, not the user's statement about
    // its ids, so the alias still applies to the entry it names.
    let aliased = client
        .rpc(
            "provider.models.edit",
            json!({"providerId":"managed-fixture","expectedVersion":version,
                "modelId":"house-deployment",
                "edit":{"alias":{"action":"set","value":"kimi-k3"}}}),
        )
        .await;
    assert_ok(&aliased);
    let refreshed = client
        .rpc("provider.refresh", json!({"providerId":"managed-fixture"}))
        .await;
    assert_ok(&refreshed);
    let at = house(&refreshed);
    assert_eq!(
        refreshed["result"]["models"][at]["unavailableReason"],
        Value::Null,
        "{}",
        refreshed["result"]["models"][at]
    );
    assert_eq!(
        refreshed["result"]["models"][at]["capabilities"]["contextWindow"],
        json!(1_048_576)
    );
    // The listing is rewritten by the refresh, so the record is derived again
    // from the statements that outlive it and says what it said on the write.
    assert_eq!(
        (
            &refreshed["result"]["models"][at]["metadata"]["identification"],
            &refreshed["result"]["models"][at]["metadata"]["matchedModelId"],
        ),
        (
            &aliased["result"]["models"][at]["metadata"]["identification"],
            &aliased["result"]["models"][at]["metadata"]["matchedModelId"],
        ),
        "a refresh recomputes what the alias names"
    );
    assert_ok(&client.rpc("daemon.shutdown", json!({})).await);
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// A regression for the override freezing the account's own declarations
/// (#2189): an override that only supplies the model's limits must not stop a
/// same-account refresh from serving the reasoning efforts and image input the
/// refreshed catalog declares, on the effective catalog the picker reads. The
/// edit goes through the public `provider.models.edit` path against the
/// persisted Provider record, and the catalog evolves between two refreshes.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_limits_only_override_does_not_freeze_refreshed_capabilities() {
    let home = std::env::temp_dir().join(format!(
        "dm-{}",
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    ));
    std::fs::create_dir_all(&home).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
    let home = home.canonicalize().unwrap();
    let fixture = Arc::new(Fixture {
        base_url: "http://127.0.0.1:1/v1".into(),
        account: StdMutex::new(AccountSnapshot {
            credential_epoch: "epoch".into(),
            generation: 1,
            account: Some(AccountView {
                issuer: "https://accounts.example".into(),
                subject: "fixture".into(),
                email: Some("fixture@example.com".into()),
                name: Some("Fixture".into()),
                granted_scopes: vec!["model".into()],
            }),
        }),
        limited: StdMutex::new(Limited::Absent),
        recovered: AtomicUsize::new(0),
        released: StdMutex::new(vec![]),
        watch: watch::channel(Some(true)).0,
    });
    let provider = fixture.clone();
    let server = launch(
        Arc::new(MockTools),
        ServerConfig {
            product_home: Some(home.clone()),
            target_home: Some(home.join("rust")),
            managed_provider: Some(ManagedProviderRow(Arc::new(move |builtins, _| {
                let provider = provider.clone();
                builtins
                    .builtins
                    .register::<Value, _, _>("ManagedFixture", move |_| {
                        Ok(FixturePlugin(provider.clone()))
                    });
                "ManagedFixture".to_owned()
            }))),
            session_defaults: dim_agent_runtime::SessionConfig {
                tools: Some(["read"].map(String::from).to_vec()),
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .await;
    let mut client = Client::new_version(&server, false, 3).await;
    let created = client
        .rpc("provider.refresh", json!({"providerId":"managed-fixture"}))
        .await;
    assert_ok(&created);
    assert_eq!(created["result"]["version"], 1);
    let model = &created["result"]["models"][0];
    assert_eq!(model["capabilities"]["vision"], false);
    assert_eq!(
        model["metadata"]["reasoning"]["effortOptions"],
        json!(["low", "high"]),
        "the old catalog row declares the short list"
    );
    // The public edit path supplies just the limits.
    assert_ok(
        &client
            .rpc(
                "provider.models.edit",
                json!({"providerId":"managed-fixture","expectedVersion":1,
                    "modelId":"fixture-model",
                    "edit":{"contextWindow":100000,"maxOutputTokens":8192}}),
            )
            .await,
    );
    // The same account's refreshed catalog now declares the new capabilities.
    *fixture.limited.lock().unwrap() = Limited::Evolved;
    let refreshed = client
        .rpc("provider.refresh", json!({"providerId":"managed-fixture"}))
        .await;
    assert_ok(&refreshed);
    let model = &refreshed["result"]["models"][0];
    assert_eq!(
        model["capabilities"]["contextWindow"],
        json!(100000),
        "the edited window stands"
    );
    assert_eq!(
        model["capabilities"]["maxOutputTokens"],
        json!(8192),
        "the edited output stands"
    );
    assert_eq!(
        model["capabilities"]["vision"], true,
        "the refreshed row's image input reaches the effective entry"
    );
    assert_eq!(
        model["capabilities"]["modalities"],
        json!(["text", "image"])
    );
    assert_eq!(
        model["metadata"]["reasoning"]["effortOptions"],
        json!(["low", "medium", "high", "xhigh"]),
        "the refreshed row's declared efforts reach the effective entry"
    );
    let management = client
        .rpc(
            "provider.management",
            json!({"providerId":"managed-fixture"}),
        )
        .await;
    assert_ok(&management);
    let managed = management["result"]["providers"][0]["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|model| model["model"]["modelId"] == json!("fixture-model"))
        .cloned()
        .expect("the management view lists the model");
    assert_eq!(
        managed["model"]["capabilities"]["vision"], true,
        "the picker reads the refreshed declaration too"
    );
    assert_eq!(
        managed["model"]["metadata"]["reasoning"]["effortOptions"],
        json!(["low", "medium", "high", "xhigh"])
    );
    assert_eq!(
        managed["model"]["capabilities"]["contextWindow"],
        json!(100000)
    );
    assert_ok(&client.rpc("daemon.shutdown", json!({})).await);
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// The pin the fix must not discard (#2189): a capability the user explicitly
/// edited wins over the refreshed account declaration, while a capability the
/// edit never touched still follows the refresh.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_explicit_capability_edit_survives_a_same_account_refresh() {
    let home = std::env::temp_dir().join(format!(
        "dm-{}",
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    ));
    std::fs::create_dir_all(&home).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
    let home = home.canonicalize().unwrap();
    let fixture = Arc::new(Fixture {
        base_url: "http://127.0.0.1:1/v1".into(),
        account: StdMutex::new(AccountSnapshot {
            credential_epoch: "epoch".into(),
            generation: 1,
            account: Some(AccountView {
                issuer: "https://accounts.example".into(),
                subject: "fixture".into(),
                email: Some("fixture@example.com".into()),
                name: Some("Fixture".into()),
                granted_scopes: vec!["model".into()],
            }),
        }),
        limited: StdMutex::new(Limited::Absent),
        recovered: AtomicUsize::new(0),
        released: StdMutex::new(vec![]),
        watch: watch::channel(Some(true)).0,
    });
    let provider = fixture.clone();
    let server = launch(
        Arc::new(MockTools),
        ServerConfig {
            product_home: Some(home.clone()),
            target_home: Some(home.join("rust")),
            managed_provider: Some(ManagedProviderRow(Arc::new(move |builtins, _| {
                let provider = provider.clone();
                builtins
                    .builtins
                    .register::<Value, _, _>("ManagedFixture", move |_| {
                        Ok(FixturePlugin(provider.clone()))
                    });
                "ManagedFixture".to_owned()
            }))),
            session_defaults: dim_agent_runtime::SessionConfig {
                tools: Some(["read"].map(String::from).to_vec()),
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .await;
    let mut client = Client::new_version(&server, false, 3).await;
    let created = client
        .rpc("provider.refresh", json!({"providerId":"managed-fixture"}))
        .await;
    assert_ok(&created);
    // The user pins reasoning off on a model whose catalog row declares it.
    assert_ok(
        &client
            .rpc(
                "provider.models.edit",
                json!({"providerId":"managed-fixture","expectedVersion":1,
                    "modelId":"fixture-model",
                    "edit":{"reasoning":{"supported":false}}}),
            )
            .await,
    );
    *fixture.limited.lock().unwrap() = Limited::Evolved;
    let refreshed = client
        .rpc("provider.refresh", json!({"providerId":"managed-fixture"}))
        .await;
    assert_ok(&refreshed);
    let model = &refreshed["result"]["models"][0];
    assert_eq!(
        model["capabilities"]["reasoning"], false,
        "the explicit reasoning edit wins over the refreshed declaration"
    );
    assert_eq!(
        model["metadata"]["reasoning"]["supported"], false,
        "the pin covers the reasoning descriptor the picker reads"
    );
    assert!(
        model["metadata"]["reasoning"]["effortOptions"].is_null()
            || model["metadata"]["reasoning"]["effortOptions"] == json!([]),
        "a pinned-off reasoning offers no efforts: {}",
        model["metadata"]["reasoning"]
    );
    assert_eq!(
        model["capabilities"]["vision"], true,
        "a capability the edit never touched follows the refresh"
    );
    assert_eq!(
        model["capabilities"]["modalities"],
        json!(["text", "image"])
    );
    assert_ok(&client.rpc("daemon.shutdown", json!({})).await);
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// The fixture server and account the override tests share.
async fn override_fixture() -> (RunningServer, Arc<Fixture>, std::path::PathBuf) {
    let home = std::env::temp_dir().join(format!(
        "dm-{}",
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    ));
    std::fs::create_dir_all(&home).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
    let home = home.canonicalize().unwrap();
    let fixture = Arc::new(Fixture {
        base_url: "http://127.0.0.1:1/v1".into(),
        account: StdMutex::new(AccountSnapshot {
            credential_epoch: "epoch".into(),
            generation: 1,
            account: Some(AccountView {
                issuer: "https://accounts.example".into(),
                subject: "fixture".into(),
                email: Some("fixture@example.com".into()),
                name: Some("Fixture".into()),
                granted_scopes: vec!["model".into()],
            }),
        }),
        limited: StdMutex::new(Limited::Absent),
        recovered: AtomicUsize::new(0),
        released: StdMutex::new(vec![]),
        watch: watch::channel(Some(true)).0,
    });
    let provider = fixture.clone();
    let server = launch(
        Arc::new(MockTools),
        ServerConfig {
            product_home: Some(home.clone()),
            target_home: Some(home.join("rust")),
            managed_provider: Some(ManagedProviderRow(Arc::new(move |builtins, _| {
                let provider = provider.clone();
                builtins
                    .builtins
                    .register::<Value, _, _>("ManagedFixture", move |_| {
                        Ok(FixturePlugin(provider.clone()))
                    });
                "ManagedFixture".to_owned()
            }))),
            session_defaults: dim_agent_runtime::SessionConfig {
                tools: Some(["read"].map(String::from).to_vec()),
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .await;
    (server, fixture, home)
}

fn stored(home: &std::path::Path) -> Value {
    let store = rusqlite::Connection::open(home.join("rust").join("dimcode.sqlite")).unwrap();
    dim_app_server::provider::testing::stored_provider(&store, "managed-fixture")
        .unwrap()
        .unwrap()
}

fn rewrite_stored(home: &std::path::Path, record: &Value) {
    let store = rusqlite::Connection::open(home.join("rust").join("dimcode.sqlite")).unwrap();
    dim_app_server::provider::testing::replace_provider(
        &store,
        "managed-fixture",
        record["version"].as_u64().unwrap() as u32,
        &record.to_string(),
    )
    .unwrap();
}

/// A regression for the upgrade silently discarding a capability the user
/// edited (#2189): an override written before capability edits were marked is
/// the same clone with only its limits marked, so the first read after the
/// upgrade must still pin the edit, and a refresh that then rewrites the
/// account's rows must keep it pinned while the fields the user never touched
/// follow the refreshed declarations.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_pre_upgrade_capability_edit_is_pinned_on_the_first_read() {
    let (server, fixture, home) = override_fixture().await;
    let mut client = Client::new_version(&server, false, 3).await;
    let created = client
        .rpc("provider.refresh", json!({"providerId":"managed-fixture"}))
        .await;
    assert_ok(&created);
    // The user pins reasoning off on a model whose catalog row declares it.
    assert_ok(
        &client
            .rpc(
                "provider.models.edit",
                json!({"providerId":"managed-fixture","expectedVersion":1,
                    "modelId":"fixture-model",
                    "edit":{"reasoning":{"supported":false}}}),
            )
            .await,
    );
    // The record the pre-#2189 edit path wrote is this same clone without the
    // capability marks and without the normalization record, so rewriting it
    // without them is exactly what an upgrade reads, not an approximation of
    // one.
    let mut record = stored(&home);
    let marks = &mut record["modelConfigOverrides"][0]["metadata"]["capabilityOverrideFields"];
    *marks = json!(
        marks
            .as_array()
            .unwrap()
            .iter()
            .filter(|field| !matches!(
                field.as_str().unwrap(),
                "toolCalling" | "vision" | "reasoning" | "modalityCapabilities"
            ))
            .cloned()
            .collect::<Vec<_>>()
    );
    assert_eq!(marks, &json!([]));
    if let Some(entry) = record["modelConfigOverrides"][0].as_object_mut() {
        entry.remove("normalizedOverride");
    }
    rewrite_stored(&home, &record);
    // The first read after the upgrade, before any refresh: the pin must
    // stand where the clone kept it.
    let management = client
        .rpc(
            "provider.management",
            json!({"providerId":"managed-fixture"}),
        )
        .await;
    assert_ok(&management);
    let models = management["result"]["providers"][0]["models"]
        .as_array()
        .unwrap();
    let at = models
        .iter()
        .position(|model| model["model"]["modelId"] == json!("fixture-model"))
        .unwrap();
    assert_eq!(
        models[at]["model"]["capabilities"]["reasoning"], false,
        "an unmarked capability edit must survive the first read of the upgrade: {}",
        models[at]["model"]
    );
    // The refreshed catalog declares reasoning and image input. The pin wins
    // over the first, and the capability the user never edited follows the
    // second.
    *fixture.limited.lock().unwrap() = Limited::Evolved;
    let refreshed = client
        .rpc("provider.refresh", json!({"providerId":"managed-fixture"}))
        .await;
    assert_ok(&refreshed);
    let model = &refreshed["result"]["models"][0];
    assert_eq!(
        model["capabilities"]["reasoning"], false,
        "the unmarked pin must win over the refreshed declaration too"
    );
    assert_eq!(
        model["capabilities"]["vision"], true,
        "a capability the edit never touched follows the refresh"
    );
    assert_eq!(
        model["capabilities"]["modalities"],
        json!(["text", "image"])
    );
    // The read that rewrote the rows also fastened the pin, so the stored
    // override now carries the mark the pre-upgrade write could not.
    let record = stored(&home);
    let marks = record["modelConfigOverrides"][0]["metadata"]["capabilityOverrideFields"]
        .as_array()
        .unwrap()
        .iter()
        .map(|field| field.as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    assert!(
        marks.iter().any(|field| field == "reasoning"),
        "the refresh must fasten the pin it carried across: {marks:?}"
    );
    assert_ok(&client.rpc("daemon.shutdown", json!({})).await);
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// A regression for the pin that a refreshed row without metadata dropped
/// (#2189): the row keeps the capability but nothing to hang it on, and the
/// pin the user wrote must still win over it, mark and all, or the next
/// refresh silently loses the edit.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_reasoning_pin_survives_a_row_without_reasoning_metadata() {
    let (server, fixture, home) = override_fixture().await;
    let mut client = Client::new_version(&server, false, 3).await;
    let created = client
        .rpc("provider.refresh", json!({"providerId":"managed-fixture"}))
        .await;
    assert_ok(&created);
    // The user pins the reasoning declaration to a narrower offer than the
    // catalog row's.
    assert_ok(
        &client
            .rpc(
                "provider.models.edit",
                json!({"providerId":"managed-fixture","expectedVersion":1,
                    "modelId":"fixture-model",
                    "edit":{"reasoning":{"supported":true,"effortOptions":["low"]}}}),
            )
            .await,
    );
    // The same account stops declaring reasoning: the refreshed row carries
    // no reasoning metadata at all.
    *fixture.limited.lock().unwrap() = Limited::Stripped;
    let refreshed = client
        .rpc("provider.refresh", json!({"providerId":"managed-fixture"}))
        .await;
    assert_ok(&refreshed);
    let model = &refreshed["result"]["models"][0];
    assert_eq!(
        model["capabilities"]["reasoning"], true,
        "the pinned reasoning must win over a row that declares none: {}",
        model["capabilities"]
    );
    assert_eq!(
        model["metadata"]["reasoning"]["supported"], true,
        "the pinned descriptor must survive the row without metadata: {}",
        model["metadata"]
    );
    assert_eq!(
        model["metadata"]["reasoning"]["effortOptions"],
        json!(["low"]),
        "the pinned offer must survive the row without metadata"
    );
    let marks = model["metadata"]["capabilityOverrideFields"]
        .as_array()
        .map(|fields| {
            fields
                .iter()
                .map(|field| field.as_str().unwrap().to_owned())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    assert!(
        marks.iter().any(|field| field == "reasoning"),
        "the mark must survive the row without metadata, or the next refresh loses the pin: {marks:?}"
    );
    assert_ok(&client.rpc("daemon.shutdown", json!({})).await);
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// A regression for the limits pin a metadata-less row silently reverted
/// (#2189): the rebase copies the pinned values onto a row that declares no
/// metadata, but the row offers nowhere to hang the marks, so unless the
/// marks are kept the stored override reads as unmarked afterwards and the
/// next rebase serves the row's values instead of the user's.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn limits_marks_survive_a_row_without_metadata() {
    let (server, fixture, home) = override_fixture().await;
    let mut client = Client::new_version(&server, false, 3).await;
    let created = client
        .rpc("provider.refresh", json!({"providerId":"managed-fixture"}))
        .await;
    assert_ok(&created);
    // The user edits only the limits.
    assert_ok(
        &client
            .rpc(
                "provider.models.edit",
                json!({"providerId":"managed-fixture","expectedVersion":1,
                    "modelId":"fixture-model",
                    "edit":{"contextWindow":100000,"maxOutputTokens":8192}}),
            )
            .await,
    );
    // The same account stops declaring reasoning, so the refreshed row carries
    // no metadata at all.
    *fixture.limited.lock().unwrap() = Limited::Stripped;
    let refreshed = client
        .rpc("provider.refresh", json!({"providerId":"managed-fixture"}))
        .await;
    assert_ok(&refreshed);
    let model = &refreshed["result"]["models"][0];
    assert_eq!(
        model["capabilities"]["contextWindow"],
        json!(100000),
        "the edited window must win over the row without metadata: {}",
        model["capabilities"]
    );
    assert_eq!(
        model["capabilities"]["maxOutputTokens"],
        json!(8192),
        "the edited output must win over the row without metadata"
    );
    let marks = |home: &std::path::Path| {
        let record = stored(home);
        record["modelConfigOverrides"][0]["metadata"]["capabilityOverrideFields"]
            .as_array()
            .map(|fields| {
                fields
                    .iter()
                    .map(|field| field.as_str().unwrap().to_owned())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    };
    assert!(
        marks(&home).iter().any(|field| field == "contextWindow")
            && marks(&home).iter().any(|field| field == "maxOutputTokens"),
        "the stored override must keep the limits marks, or the next refresh reverts the edit: {:?}",
        marks(&home)
    );
    // One refresh later the same edit must still stand: the values the first
    // metadata-less rebase stored are the base every later read compares
    // against.
    let refreshed = client
        .rpc("provider.refresh", json!({"providerId":"managed-fixture"}))
        .await;
    assert_ok(&refreshed);
    let model = &refreshed["result"]["models"][0];
    assert_eq!(
        model["capabilities"]["contextWindow"],
        json!(100000),
        "the edited window must survive the second refresh too: {}",
        model["capabilities"]
    );
    assert_eq!(
        model["capabilities"]["maxOutputTokens"],
        json!(8192),
        "the edited output must survive the second refresh too"
    );
    assert_ok(&client.rpc("daemon.shutdown", json!({})).await);
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// The modality half of the pin that a refreshed row without metadata must
/// not drop (#2189): the edit writes two raw keys, and both the projection
/// and the mark must survive the row that offers nothing to hang them on.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_modality_capabilities_pin_survives_a_row_without_metadata() {
    let (server, fixture, home) = override_fixture().await;
    let mut client = Client::new_version(&server, false, 3).await;
    let created = client
        .rpc("provider.refresh", json!({"providerId":"managed-fixture"}))
        .await;
    assert_ok(&created);
    // The user declares the model's modality capabilities where the catalog
    // row declares none.
    assert_ok(
        &client
            .rpc(
                "provider.models.edit",
                json!({"providerId":"managed-fixture","expectedVersion":1,
                    "modelId":"fixture-model",
                    "edit":{"modalityCapabilities":["image.generate"]}}),
            )
            .await,
    );
    // The same account stops declaring reasoning, so the refreshed row carries
    // no metadata at all.
    *fixture.limited.lock().unwrap() = Limited::Stripped;
    let refreshed = client
        .rpc("provider.refresh", json!({"providerId":"managed-fixture"}))
        .await;
    assert_ok(&refreshed);
    let model = &refreshed["result"]["models"][0];
    assert_eq!(
        model["metadata"]["raw"]["imageGeneration"],
        json!(true),
        "the pinned image-generation projection must survive the row without metadata: {}",
        model["metadata"]
    );
    assert_eq!(
        model["metadata"]["raw"]["modalityCapabilities"],
        json!(["image.generate"]),
        "the pinned capability list must survive the row without metadata"
    );
    let marks = model["metadata"]["capabilityOverrideFields"]
        .as_array()
        .map(|fields| {
            fields
                .iter()
                .map(|field| field.as_str().unwrap().to_owned())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    assert!(
        marks.iter().any(|field| field == "modalityCapabilities"),
        "the mark must survive the row without metadata, or the next refresh loses the pin: {marks:?}"
    );
    assert_ok(&client.rpc("daemon.shutdown", json!({})).await);
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// The gap in the pin chain (#2189): a same-account listing that drops the
/// model for one refresh and then brings it back with new capabilities must
/// not freeze them. The override is a clone of the row it edited, so its
/// unedited capability values belong to the row the clone was taken from; the
/// write that drops the row is the last chance to tell the user's pins from
/// the clone's capture, and the read must not mistake the capture for edits
/// once the row comes back changed.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unedited_capability_follows_the_row_across_a_listing_gap() {
    let (server, fixture, home) = override_fixture().await;
    let mut client = Client::new_version(&server, false, 3).await;
    let created = client
        .rpc("provider.refresh", json!({"providerId":"managed-fixture"}))
        .await;
    assert_ok(&created);
    // The user edits only the limits, so no capability field is marked.
    assert_ok(
        &client
            .rpc(
                "provider.models.edit",
                json!({"providerId":"managed-fixture","expectedVersion":1,
                    "modelId":"fixture-model",
                    "edit":{"contextWindow":100000,"maxOutputTokens":8192}}),
            )
            .await,
    );
    // The same account stops listing the model for one refresh, then
    // another, and brings it back declaring capabilities the rows never
    // declared while the override was written: nothing the clone captured
    // may answer for them.
    *fixture.limited.lock().unwrap() = Limited::Delisted;
    for round in 0..2 {
        let dropped = client
            .rpc("provider.refresh", json!({"providerId":"managed-fixture"}))
            .await;
        assert_ok(&dropped);
        assert!(
            dropped["result"]["models"]
                .as_array()
                .unwrap()
                .iter()
                .all(|model| model["id"] != json!("fixture-model")),
            "the delisted refresh must not serve the model (round {round})"
        );
    }
    *fixture.limited.lock().unwrap() = Limited::Evolved;
    let returned = client
        .rpc("provider.refresh", json!({"providerId":"managed-fixture"}))
        .await;
    assert_ok(&returned);
    let model = &returned["result"]["models"][0];
    assert_eq!(model["id"], json!("fixture-model"));
    assert_eq!(
        model["capabilities"]["contextWindow"],
        json!(100000),
        "the edited window must survive the gap"
    );
    assert_eq!(
        model["capabilities"]["maxOutputTokens"],
        json!(8192),
        "the edited output must survive the gap"
    );
    assert_eq!(
        model["capabilities"]["vision"], true,
        "a capability the edit never touched must follow the returning row across the gap: {}",
        model["capabilities"]
    );
    assert_eq!(
        model["capabilities"]["modalities"],
        json!(["text", "image"]),
        "the returning row's modalities must reach the effective entry"
    );
    assert_eq!(
        model["metadata"]["reasoning"]["effortOptions"],
        json!(["low", "medium", "high", "xhigh"]),
        "the returning row's declared efforts must not freeze at the values the clone captured: {}",
        model["metadata"]["reasoning"]
    );
    assert_ok(&client.rpc("daemon.shutdown", json!({})).await);
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// The explicit half of the gap chain (#2189): a capability the user pinned
/// before the model dropped out of the listing must still win after the gap,
/// while every capability the edit left alone follows the returning row.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_explicit_capability_pin_wins_across_a_listing_gap() {
    let (server, fixture, home) = override_fixture().await;
    let mut client = Client::new_version(&server, false, 3).await;
    let created = client
        .rpc("provider.refresh", json!({"providerId":"managed-fixture"}))
        .await;
    assert_ok(&created);
    // The user pins reasoning off and edits the limits, so the override holds
    // both kinds of marks.
    assert_ok(
        &client
            .rpc(
                "provider.models.edit",
                json!({"providerId":"managed-fixture","expectedVersion":1,
                    "modelId":"fixture-model",
                    "edit":{"contextWindow":100000,
                        "reasoning":{"supported":false}}}),
            )
            .await,
    );
    // The model drops out of the listing for one refresh and comes back
    // declaring capabilities the rows never declared while the override was
    // written.
    *fixture.limited.lock().unwrap() = Limited::Delisted;
    let dropped = client
        .rpc("provider.refresh", json!({"providerId":"managed-fixture"}))
        .await;
    assert_ok(&dropped);
    *fixture.limited.lock().unwrap() = Limited::Evolved;
    let returned = client
        .rpc("provider.refresh", json!({"providerId":"managed-fixture"}))
        .await;
    assert_ok(&returned);
    let model = &returned["result"]["models"][0];
    assert_eq!(model["id"], json!("fixture-model"));
    assert_eq!(
        model["capabilities"]["reasoning"], false,
        "the explicit reasoning pin must win after the gap too"
    );
    assert!(
        model["metadata"]["reasoning"]["effortOptions"].is_null()
            || model["metadata"]["reasoning"]["effortOptions"] == json!([]),
        "the pinned-off reasoning offers no efforts after the gap: {}",
        model["metadata"]["reasoning"]
    );
    assert_eq!(
        model["capabilities"]["contextWindow"],
        json!(100000),
        "the edited window must survive the gap beside the pin"
    );
    assert_eq!(
        model["capabilities"]["vision"], true,
        "a capability the edit never touched must follow the returning row across the gap: {}",
        model["capabilities"]
    );
    assert_eq!(
        model["capabilities"]["modalities"],
        json!(["text", "image"])
    );
    assert_ok(&client.rpc("daemon.shutdown", json!({})).await);
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// The legacy half of the gap chain (#2189): an override written before
/// capability edits were marked is the same clone with only its limits
/// marked, and it cannot be told from a normalized one by the marks alone.
/// When the row disappears before any post-upgrade write can rebase the
/// clone, the dropping write must still record what the clone pinned against
/// the last row it saw, so the returning refresh applies only those pins:
/// a capability the pre-upgrade user genuinely edited still wins after the
/// gap, and one the clone merely captured follows the returning row instead
/// of freezing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_pre_upgrade_clone_applies_only_its_pins_across_a_listing_gap() {
    let (server, fixture, home) = override_fixture().await;
    let mut client = Client::new_version(&server, false, 3).await;
    let created = client
        .rpc("provider.refresh", json!({"providerId":"managed-fixture"}))
        .await;
    assert_ok(&created);
    // The pre-upgrade user edits the limits and pins reasoning off; the edit
    // path of that era wrote the same clone but marked only the limits.
    assert_ok(
        &client
            .rpc(
                "provider.models.edit",
                json!({"providerId":"managed-fixture","expectedVersion":1,
                    "modelId":"fixture-model",
                    "edit":{"contextWindow":100000,"maxOutputTokens":8192,
                        "reasoning":{"supported":false}}}),
            )
            .await,
    );
    // Rewriting the stored override without the capability marks and without
    // the normalization record reproduces what the pre-#2189 edit path wrote,
    // so an upgrade reads exactly this shape.
    let mut record = stored(&home);
    let override_entry = &mut record["modelConfigOverrides"][0];
    let marks = &mut override_entry["metadata"]["capabilityOverrideFields"];
    *marks = json!(
        marks
            .as_array()
            .unwrap()
            .iter()
            .filter(|field| !matches!(
                field.as_str().unwrap(),
                "toolCalling" | "vision" | "reasoning" | "modalityCapabilities"
            ))
            .cloned()
            .collect::<Vec<_>>()
    );
    if let Some(entry) = override_entry.as_object_mut() {
        entry.remove("normalizedOverride");
    }
    rewrite_stored(&home, &record);
    // The model disappears before any refresh can rebase the legacy clone
    // against a row it is listed in, and comes back declaring capabilities
    // the rows never declared while the clone was taken.
    *fixture.limited.lock().unwrap() = Limited::Delisted;
    let dropped = client
        .rpc("provider.refresh", json!({"providerId":"managed-fixture"}))
        .await;
    assert_ok(&dropped);
    *fixture.limited.lock().unwrap() = Limited::Evolved;
    let returned = client
        .rpc("provider.refresh", json!({"providerId":"managed-fixture"}))
        .await;
    assert_ok(&returned);
    let model = &returned["result"]["models"][0];
    assert_eq!(model["id"], json!("fixture-model"));
    assert_eq!(
        model["capabilities"]["contextWindow"],
        json!(100000),
        "the legacy clone's edited window must survive the gap"
    );
    assert_eq!(
        model["capabilities"]["maxOutputTokens"],
        json!(8192),
        "the legacy clone's edited output must survive the gap"
    );
    assert_eq!(
        model["capabilities"]["reasoning"], false,
        "the capability the pre-upgrade user pinned must still win after the gap"
    );
    assert!(
        model["metadata"]["reasoning"]["effortOptions"].is_null()
            || model["metadata"]["reasoning"]["effortOptions"] == json!([]),
        "the pinned-off reasoning offers no efforts after the gap: {}",
        model["metadata"]["reasoning"]
    );
    assert_eq!(
        model["capabilities"]["vision"], true,
        "the returning row's vision must not freeze at the value the legacy clone captured: {}",
        model["capabilities"]
    );
    assert_eq!(
        model["capabilities"]["modalities"],
        json!(["text", "image"])
    );
    assert_ok(&client.rpc("daemon.shutdown", json!({})).await);
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// A regression for the legacy clone the first edit carries across the
/// upgrade (#2189): an account-model edit on a record whose stored override
/// predates capability marks must fasten the pins the clone holds before the
/// edit declares the override normalized, or the pin the pre-upgrade user
/// wrote is lost inside that same edit.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_legacy_clone_keeps_its_unmarked_pins_through_the_first_edit() {
    let (server, _fixture, home) = override_fixture().await;
    let mut client = Client::new_version(&server, false, 3).await;
    let created = client
        .rpc("provider.refresh", json!({"providerId":"managed-fixture"}))
        .await;
    assert_ok(&created);
    // The pre-upgrade user pins reasoning off; the edit path of that era
    // wrote the same clone without the capability marks.
    assert_ok(
        &client
            .rpc(
                "provider.models.edit",
                json!({"providerId":"managed-fixture","expectedVersion":1,
                    "modelId":"fixture-model",
                    "edit":{"reasoning":{"supported":false}}}),
            )
            .await,
    );
    // Rewriting the stored override without the capability marks and without
    // the normalization record reproduces exactly the shape an upgrade
    // reads.
    let mut record = stored(&home);
    let override_entry = &mut record["modelConfigOverrides"][0];
    let marks = &mut override_entry["metadata"]["capabilityOverrideFields"];
    *marks = json!(
        marks
            .as_array()
            .unwrap()
            .iter()
            .filter(|field| !matches!(
                field.as_str().unwrap(),
                "toolCalling" | "vision" | "reasoning" | "modalityCapabilities"
            ))
            .cloned()
            .collect::<Vec<_>>()
    );
    if let Some(entry) = override_entry.as_object_mut() {
        entry.remove("normalizedOverride");
    }
    rewrite_stored(&home, &record);
    // The first operation after the upgrade is another edit on the same
    // model, before any refresh could normalize the legacy clone. The edit
    // must keep the pin the clone carried rather than dropping it inside the
    // write that declares the override normalized.
    let edited = client
        .rpc(
            "provider.models.edit",
            json!({"providerId":"managed-fixture","expectedVersion":2,
                "modelId":"fixture-model",
                "edit":{"contextWindow":100000}}),
        )
        .await;
    assert_ok(&edited);
    let model = &edited["result"]["models"][0];
    assert_eq!(
        model["capabilities"]["reasoning"], false,
        "the unmarked pin the legacy clone carried must survive the first edit: {}",
        model["capabilities"]
    );
    assert_eq!(
        model["capabilities"]["contextWindow"],
        json!(100000),
        "the edit itself lands beside the pin"
    );
    // The stored override the edit wrote pins every difference it carries
    // and records the pin set as complete.
    let record = stored(&home);
    let override_entry = &record["modelConfigOverrides"][0];
    assert_eq!(
        override_entry["normalizedOverride"],
        json!(true),
        "the edit records the pin set as complete: {}",
        override_entry
    );
    let marks = override_entry["metadata"]["capabilityOverrideFields"]
        .as_array()
        .unwrap()
        .iter()
        .map(|field| field.as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    assert!(
        marks.iter().any(|field| field == "reasoning"),
        "the pin the clone carried must be marked before the set is declared complete: {marks:?}"
    );
    assert!(
        marks.iter().any(|field| field == "contextWindow"),
        "the edit's own mark lands beside it: {marks:?}"
    );
    assert_ok(&client.rpc("daemon.shutdown", json!({})).await);
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

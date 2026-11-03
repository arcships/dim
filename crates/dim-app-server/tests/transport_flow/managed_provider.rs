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

/// What the account's catalog reports for its second model.
#[derive(Clone, Copy, PartialEq)]
enum Limited {
    /// The catalog holds the one fixture model.
    Absent,
    /// A second model the account can use.
    Available,
    /// A second model whose limits are undeclared, so it cannot be used.
    Unavailable,
    ProviderUnavailable,
}

/// A signed-in managed account whose first credential the relay rejects.
struct Fixture {
    base_url: String,
    account: StdMutex<AccountSnapshot>,
    limited: StdMutex<Limited>,
    recovered: AtomicUsize,
    released: StdMutex<Vec<String>>,
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
    fn catalog(&self, _: &str) -> ManagedFuture<ManagedCatalog> {
        let entry: ModelEntry = serde_json::from_value(json!({"modelId":"fixture-model",
            "displayName":"Fixture model","status":"active",
            "metadata":{"reasoning":{"supported":true,"effortOptions":["low","high"]}},
            "capabilities":dim_agent_types::CatalogCapabilities {
                reasoning: true, ..dim_agent_types::CatalogCapabilities::with_limits(32_000, 128) }}))
        .unwrap();
        let mut models = vec![ManagedModel {
            entry,
            protocol: ModelProtocol::OpenaiCompatible,
            unavailable_reason: None,
            zdr: false,
        }];
        match *self.0.limited.lock().unwrap() {
            Limited::Absent => {}
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
    });
    let provider = fixture.clone();
    let mut builder = AgentBuilder::new(AgentConfig::default());
    defaults::install_local_execution(
        &mut builder,
        ModelProviderPlugin::process,
        WorkspaceToolsConfig {
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
    });
    let provider = fixture.clone();
    let mut builder = AgentBuilder::new(AgentConfig::default());
    defaults::install_local_execution(
        &mut builder,
        ModelProviderPlugin::process,
        WorkspaceToolsConfig {
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

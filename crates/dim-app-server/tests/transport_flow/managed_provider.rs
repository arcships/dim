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
            BrowserLogin, BrowserLoginBegin, DeviceLogin, LoginBegin, LoginState,
            ManagedBrowserLoginComplete, ManagedLoginAttempt, Usage,
        },
    },
    model_catalog::ModelEntry,
};
use rutis::Ctx;
use rutis::host::BoxFuture;
use std::sync::{Mutex as StdMutex, Weak};

/// A signed-in managed account whose first credential the relay rejects.
struct Fixture {
    base_url: String,
    account: StdMutex<AccountSnapshot>,
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
        }]
    }
    fn status(&self, provider: &str) -> ManagedFuture<AccountSnapshot> {
        ready(match provider {
            "fail" => Err(ManagedError::InvalidStore),
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
            "capabilities":dim_agent_types::ModelCapabilities {
                context_window: 32_000, max_output_tokens: 128, reasoning: true, ..Default::default() }}))
        .unwrap();
        ready(Ok(ManagedCatalog {
            account: self.0.account.lock().unwrap().clone(),
            models: vec![ManagedModel {
                entry,
                protocol: ModelProtocol::OpenaiCompatible,
                unavailable_reason: None,
                zdr: false,
            }],
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
            "Bearer recovered-fixture-secret"
        ]
    );
    assert_eq!(fixture.recovered.load(Ordering::SeqCst), 1);
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
    assert_eq!(stale["result"]["providers"][0]["version"], 6);
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
    assert_eq!(signed_out["result"]["providers"][0]["version"], 7);
    assert_eq!(
        signed_out["result"]["providers"][0]["models"][0]["unavailableReason"],
        "account_required"
    );
    assert_ok(&client.rpc("daemon.shutdown", json!({})).await);
    server.join().await.unwrap();
    let mut released = fixture.released.lock().unwrap().clone();
    released.sort();
    assert_eq!(released, ["initial", "recovered"]);
    http.abort();
    std::fs::remove_dir_all(home).unwrap();
}

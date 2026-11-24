#![cfg(unix)]
//! DimOAuth through the process's ManagedProviders: login, catalog, leases,
//! recovery and the `auth.json` account record, against a local account
//! service.
#[path = "fixtures/dim_oauth.rs"]
mod oauth;

use dim_agent_capabilities::{
    adapters::EchoModel,
    defaults,
    file_storage::{FileLease, FileStorage, FileStorageError, FileStoragePlugin},
    history::HistoryRenderer,
    host::{
        HostError, HostFuture, ModalityConnectionHost, ModalityTarget, ModelConnection,
        ModelDriver, ModelHost, ModelProtocol, ModelStream, ProviderConnectionResolver,
        SelectableModelsHost,
    },
};
use dim_agent_runtime::{AgentBuilder, AgentConfig, Names};
use dim_agent_types::{
    Message, ModelCapabilities, ModelCapabilitySnapshot, ModelId, ModelRequest, Role,
};
use dim_app_server::{
    RunningServer, ServerConfig,
    managed_provider::{
        AccountSnapshot, ManagedModel, ManagedProvider, ManagedProviderBinding, ManagedProviders,
        ManagedProvidersPlugin,
        management::{
            BrowserLoginBegin, LoginBegin, LoginState, ManagedBrowserLoginComplete,
            ManagedLoginAttempt,
        },
        owner_key,
    },
    model_catalog::{BundledCatalog, ProviderProjection, raw_kind},
    provider::{ProviderError, ProviderPlugin, ProviderService},
    serve,
    storage::SqliteStoragePlugin,
};
use dim_client::{Client, ConnectionConfig, Server};
use dim_oauth::{ClientIds, DimOAuthPlugin};
use rutis::Ctx;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, FiberState, TypeKey};
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::{Arc, OnceLock, atomic::Ordering},
    time::{Duration, Instant},
};
use tokio_util::sync::CancellationToken;

const PROVIDER: &str = "dimcode-api-oauth";

struct Capture(Arc<OnceLock<Arc<ManagedProviders>>>);
impl DynPlugin for Capture {
    fn name(&self) -> &str {
        "capture-managed"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![owner_key().erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let owner = ctx.require_at(&owner_key())?;
            let _ = self.0.set(owner);
            Ok(Effect::Done)
        })
    }
}

struct ProviderCapture(Arc<OnceLock<Arc<dyn ManagedProvider>>>);
impl DynPlugin for ProviderCapture {
    fn name(&self) -> &str {
        "capture-managed-provider"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![TypeKey::of::<dyn ManagedProvider>()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let provider = ctx.require::<dyn ManagedProvider>()?;
            let _ = self.0.set(provider);
            Ok(Effect::Done)
        })
    }
}

async fn active(view: &rutis::host::FiberView) {
    let mut watch = view.watch();
    while watch.borrow().state != FiberState::Active {
        assert_ne!(
            watch.borrow().state,
            FiberState::Failed,
            "{:?}",
            *watch.borrow()
        );
        watch.changed().await.unwrap();
    }
}

fn home() -> PathBuf {
    let home = std::env::temp_dir().join(format!(
        "do-{}",
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    ));
    std::fs::create_dir_all(&home).unwrap();
    std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
    home.canonicalize().unwrap()
}

/// One App Server process's provider over `home`.
struct Process {
    root: Ctx,
    providers: Arc<ManagedProviders>,
}
async fn process(home: &Path) -> Process {
    let root = Ctx::root().unwrap();
    let catalog = dim_app_server::catalog();
    let build = rutis_loader::resolve::Build::default();
    let names = Names::copy(&catalog, &build);
    let captured = Arc::new(OnceLock::new());
    let ids = ClientIds {
        global: "global-client".into(),
        cn: "cn-client".into(),
    };
    // The management table the process providers register with.
    root.plugin(dim_app_server::methods::ManagementMethodsPlugin::typed(&catalog).unwrap());
    let views = [
        root.plugin(FileStoragePlugin::new(vec![home.to_path_buf()])),
        root.plugin(ManagedProvidersPlugin::new(None)),
        root.plugin(DimOAuthPlugin::new(&names, 0, ids).unwrap()),
        root.plugin(ManagedProviderBinding),
        root.plugin(Capture(captured.clone())),
    ];
    for view in &views {
        active(view).await;
    }
    Process {
        providers: captured.get().unwrap().clone(),
        root,
    }
}

fn cancel() -> CancellationToken {
    CancellationToken::new()
}
fn auth(home: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(home.join("auth.json")).unwrap()).unwrap()
}

async fn login(providers: &ManagedProviders, directory: &str) -> AccountSnapshot {
    login_attempt(providers, directory).await.0
}

async fn login_attempt(
    providers: &ManagedProviders,
    directory: &str,
) -> (AccountSnapshot, ManagedLoginAttempt) {
    let device = providers
        .begin_device(
            LoginBegin {
                provider_id: PROVIDER.into(),
                directory: directory.into(),
                email: None,
            },
            &cancel(),
        )
        .await
        .unwrap();
    assert_eq!(device.user_code, "USER-CODE");
    let attempt = ManagedLoginAttempt {
        provider_id: PROVIDER.into(),
        attempt_id: device.attempt_id,
    };
    let account = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match providers.attempt_status(&attempt, &cancel()).unwrap() {
                LoginState::Pending => tokio::task::yield_now().await,
                LoginState::Completed { account } => break account,
                state => panic!("device login ended as {state:?}"),
            }
        }
    })
    .await
    .unwrap();
    (account, attempt)
}

async fn await_usable(providers: &ManagedProviders, expected: bool) {
    let mut usable = providers.subscribe_usable();
    tokio::time::timeout(Duration::from_secs(10), async {
        while *usable.borrow_and_update() != expected {
            usable.changed().await.unwrap();
        }
    })
    .await
    .expect("the account observation was not published");
}

async fn title_call(
    root: &Ctx,
    provider: &str,
    cancel: CancellationToken,
) -> dim_session_title::GenerateTitleReply {
    use rutis_bridge::session::{Value as V, host_key, settle};
    let service = root
        .get_at(&host_key(dim_session_title::GENERATOR))
        .unwrap();
    let callback = V::callback(move |_| {
        let cancel = cancel.clone();
        Ok(V::cancellable_future(async move {
            cancel.cancelled().await;
            Ok(V::Undefined)
        }))
    });
    let call = service
        .invoke(
            "generate",
            V::List(vec![
                json!({"providerId":provider,"text":"the opening user request"}).into(),
                callback,
            ]),
        )
        .unwrap();
    serde_json::from_value(settle(call).await.unwrap().json().unwrap()).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn optional_title_service_owns_authentication_rejection_and_cancellation() {
    use dim_session_title::{GenerateTitleReply as Reply, TitleUnavailableReason as Reason};
    let fixture = oauth::start().await;
    let home = home();
    let Process { root, providers } = process(&home).await;
    assert_eq!(
        title_call(&root, "other-provider", cancel()).await,
        Reply::Unavailable {
            reason: Reason::UnsupportedProvider
        }
    );
    assert_eq!(
        title_call(&root, PROVIDER, cancel()).await,
        Reply::Unavailable {
            reason: Reason::AccountUnavailable
        }
    );
    assert_eq!(fixture.service.calls("POST /v1/session/rename"), 0);
    let service = root
        .get_at(&rutis_bridge::session::host_key(
            dim_session_title::GENERATOR,
        ))
        .unwrap();
    for request in [
        json!({"providerId":PROVIDER,"text":""}),
        json!({"providerId":PROVIDER,"text":"a".repeat(2001)}),
        json!({"providerId":PROVIDER,"text":"valid","issuer":"https://injected.example"}),
    ] {
        assert!(service.invoke("generate", json!([request]).into()).is_err());
    }
    login(&providers, &fixture.base).await;
    assert_eq!(
        title_call(&root, PROVIDER, cancel()).await,
        Reply::Generated {
            title: "Account title".into()
        }
    );
    {
        let requests = fixture.service.title_requests.lock().unwrap();
        assert_eq!(requests[0].0, "access-0");
        assert_eq!(
            requests[0].1,
            json!({"messages":[{"role":"user","content":"the opening user request"}]})
        );
    }
    fixture
        .service
        .rejected
        .lock()
        .unwrap()
        .push("access-0".into());
    assert_eq!(
        title_call(&root, PROVIDER, cancel()).await,
        Reply::Generated {
            title: "Account title".into()
        }
    );
    assert_eq!(fixture.service.refreshes.load(Ordering::SeqCst), 1);
    assert_eq!(
        fixture
            .service
            .title_requests
            .lock()
            .unwrap()
            .last()
            .unwrap()
            .0,
        "access-1"
    );
    for (code, reason) in [
        ("session_rename_disabled", Reason::Disabled),
        ("session_rename_not_entitled", Reason::NotEntitled),
        ("session_rename_quota_exhausted", Reason::QuotaExhausted),
    ] {
        *fixture.service.title_response.lock().unwrap() = Some((
            axum::http::StatusCode::FORBIDDEN,
            json!({"error":{"code":code,"message":"private upstream message"}}),
        ));
        assert_eq!(
            title_call(&root, PROVIDER, cancel()).await,
            Reply::Unavailable { reason }
        );
    }
    assert!(
        providers
            .status(PROVIDER, &cancel())
            .await
            .unwrap()
            .account
            .is_some()
    );
    assert_eq!(fixture.service.refreshes.load(Ordering::SeqCst), 1);
    *fixture.service.title_response.lock().unwrap() =
        Some((axum::http::StatusCode::OK, json!({"title":4})));
    assert_eq!(title_call(&root, PROVIDER, cancel()).await, Reply::Failed);

    fixture
        .service
        .rejected
        .lock()
        .unwrap()
        .push("access-1".into());
    fixture.service.refresh_wait.store(true, Ordering::SeqCst);
    let cancellation = cancel();
    let pending = {
        let root = root.clone();
        let cancellation = cancellation.clone();
        tokio::spawn(async move { title_call(&root, PROVIDER, cancellation).await })
    };
    tokio::time::timeout(Duration::from_secs(10), async {
        while fixture.service.refreshes.load(Ordering::SeqCst) < 2 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    cancellation.cancel();
    fixture.service.refresh_release.notify_one();
    assert_eq!(pending.await.unwrap(), Reply::Cancelled);
    assert_eq!(
        auth(&home)["nextApiOauth"]["access"],
        "access-2",
        "title cancellation settles and persists an accepted token rotation"
    );
    fixture.service.refresh_wait.store(false, Ordering::SeqCst);

    fixture.service.title_wait.store(true, Ordering::SeqCst);
    for closing in [false, true] {
        let calls = fixture.service.calls("POST /v1/session/rename");
        let cancellation = cancel();
        let pending = {
            let root = root.clone();
            let cancellation = cancellation.clone();
            tokio::spawn(async move { title_call(&root, PROVIDER, cancellation).await })
        };
        tokio::time::timeout(Duration::from_secs(10), async {
            while fixture.service.calls("POST /v1/session/rename") == calls {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        if closing {
            tokio::time::timeout(Duration::from_secs(10), root.shutdown())
                .await
                .unwrap()
                .unwrap();
        } else {
            cancellation.cancel();
        }
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(10), pending)
                .await
                .unwrap()
                .unwrap(),
            Reply::Cancelled
        );
        fixture.service.title_release.notify_waiters();
    }
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn device_login_publishes_identity_catalog_usage_and_leases() {
    let fixture = oauth::start().await;
    let home = home();
    let Process { root, providers } = process(&home).await;
    let descriptors = providers.providers(&cancel()).await.unwrap();
    let directory = match std::env::var("DIMCODE_NPM_PACKAGE").as_deref() {
        Ok("dimagent") => "https://dimagent.com",
        _ => "https://dimagent.cn",
    };
    assert_eq!(descriptors[0].default_directory.as_deref(), Some(directory));
    let ids: Vec<_> = descriptors.into_iter().map(|p| p.provider_id).collect();
    assert_eq!(ids, [PROVIDER]);
    let signed_out = providers.status(PROVIDER, &cancel()).await.unwrap();
    assert!(signed_out.account.is_none());

    let account = login(&providers, &fixture.base).await;
    assert_eq!(
        fixture
            .service
            .calls("POST /oauth/device_authorization cn-client"),
        1
    );
    let view = account.account.as_ref().unwrap();
    assert_eq!(view.subject, "fixture-subject");
    assert_eq!(view.email.as_deref(), Some("fixture@example.com"));
    assert_eq!(view.name.as_deref(), Some("Fixture"));
    assert_eq!(view.granted_scopes.len(), 4);
    assert_eq!(account.generation, signed_out.generation + 1);
    assert_eq!(
        providers.status(PROVIDER, &cancel()).await.unwrap(),
        account
    );
    // The record is plain JSON with private permissions.
    let stored = auth(&home);
    assert_eq!(stored["dimAccountState"]["state"], "authenticated");
    assert_eq!(stored["nextApiOauth"]["clientId"], "cn-client");
    assert_eq!(
        std::fs::metadata(home.join("auth.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );

    let catalog = providers.catalog(PROVIDER, &cancel()).await.unwrap();
    assert_eq!(catalog.account, account);
    let model = |id: &str| {
        catalog
            .models
            .iter()
            .find(|m| m.entry.model_id == id)
            .unwrap()
            .clone()
    };
    let chat = model("fixture-model");
    assert_eq!(chat.protocol, ModelProtocol::OpenaiCompatible);
    assert_eq!(chat.unavailable_reason, None);
    assert!(chat.zdr);
    assert_eq!(chat.entry.display_name, "Fixture model");
    assert_eq!(chat.entry.capabilities.context_window, Some(32000));
    assert!(chat.entry.capabilities.reasoning);
    assert_eq!(model("fixture-claude").protocol, ModelProtocol::Anthropic);
    assert_eq!(model("fixture-claude").unavailable_reason, None);
    assert_eq!(
        model("fixture-image").unavailable_reason.as_deref(),
        Some("unsupported_model_type")
    );
    assert_eq!(
        model("fixture-gemini").unavailable_reason.as_deref(),
        Some("unsupported_endpoint")
    );
    // An unavailable status names the entry before its kind does.
    assert_eq!(
        model("fixture-image-off").unavailable_reason.as_deref(),
        Some("server_unavailable")
    );

    // A non-chat model still leases: its stored reason excludes a
    // conversational use, not a non-conversational one.
    let image = providers
        .credential(PROVIDER, "fixture-image", &cancel())
        .await
        .unwrap();
    assert_eq!(image.protocol, ModelProtocol::OpenaiCompatible);
    for excluded in ["fixture-gemini", "fixture-image-off"] {
        let refused = providers
            .credential(PROVIDER, excluded, &cancel())
            .await
            .err()
            .unwrap();
        assert_eq!(refused.code, "account_unavailable", "{excluded}");
    }
    let lease = providers
        .credential(PROVIDER, "fixture-model", &cancel())
        .await
        .unwrap();
    assert_eq!(lease.bearer, "access-0");
    assert_eq!(lease.base_url, format!("{}/v1", fixture.base));
    assert_eq!(lease.headers["X-Title"], "DimCode");
    assert_eq!(lease.account, account);

    let usage = providers.usage(PROVIDER, &cancel()).await.unwrap();
    let plan = usage.plan.unwrap();
    assert_eq!(plan.name.as_deref(), Some("Fixture plan"));
    assert_eq!(plan.cancel_at_period_end, Some(true));
    assert_eq!(
        plan.current_term_start.as_deref(),
        Some("2026-09-28T00:00:00Z")
    );
    assert_eq!(plan.last_term_status.as_deref(), Some("expired"));
    let credits = usage.credits.unwrap();
    assert_eq!(credits.total_units, Some(300000));
    assert_eq!(credits.remaining_units, Some(275000));
    assert_eq!(credits.resets_at.as_deref(), Some("2026-10-28T00:00:00Z"));
    // Incomplete windows and meters without a key are skipped.
    assert_eq!(usage.windows.len(), 1);
    assert_eq!(usage.features.len(), 1);
    assert_eq!(usage.features[0].key, "web_search");

    let searched = root
        .get::<dim_app_server::managed_provider::ManagedSearch>()
        .unwrap()
        .search(
            dim_app_server::managed_provider::WebSearchRequest {
                query: "dim query".into(),
                count: 5,
            },
            &cancel(),
        )
        .await
        .unwrap();
    assert_eq!(searched.results.len(), 2);
    assert_eq!(searched.results[0].title, "Dim OAuth hit");
    assert_eq!(searched.results[0].url, "https://docs.example.com/oauth");
    assert_eq!(searched.results[0].snippet, "Account search snippet");
    assert_eq!(
        searched.results[0].published_at.as_deref(),
        Some("2026-10-28T00:00:00Z")
    );
    assert_eq!(searched.results[1].snippet, "Fallback description");
    assert_eq!(fixture.service.calls("POST /v1/web-search dim query:5"), 1);

    let out = providers.logout(PROVIDER, &cancel()).await.unwrap();
    assert!(out.account.is_none());
    assert_eq!(out.generation, account.generation + 1);
    assert_eq!(fixture.service.calls("POST /oauth/revoke refresh-0"), 1);
    let required = providers
        .credential(PROVIDER, "fixture-model", &cancel())
        .await
        .err()
        .unwrap();
    assert_eq!(required.code, "account_required");
    drop(lease);
    root.shutdown().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// A completed device login is published from the account transition itself:
/// neither the attempt nor the account is read after the login starts.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn account_transitions_publish_without_a_client_read() {
    let fixture = oauth::start().await;
    let home = home();
    let Process { root, providers } = process(&home).await;
    let mut usable = providers.subscribe_usable();
    assert!(!providers.usable());
    let device = providers
        .begin_device(
            LoginBegin {
                provider_id: PROVIDER.into(),
                directory: fixture.base.clone(),
                email: None,
            },
            &cancel(),
        )
        .await
        .unwrap();
    assert_eq!(device.user_code, "USER-CODE");
    let signed_in = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if *usable.borrow_and_update() {
                return true;
            }
            usable.changed().await.unwrap();
        }
    })
    .await;
    assert!(
        signed_in.is_ok_and(|signed_in| signed_in),
        "the completed login was not published: {:?}",
        auth(&home)["dimAccountState"]["state"]
    );
    let lease = providers
        .credential(PROVIDER, "fixture-model", &cancel())
        .await
        .unwrap();
    providers.finish_rejected(&lease, &cancel()).await.unwrap();
    let signed_out = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if !*usable.borrow_and_update() {
                return true;
            }
            usable.changed().await.unwrap();
        }
    })
    .await;
    assert!(
        signed_out.is_ok_and(|signed_out| signed_out),
        "the settled rejection was not published: {:?}",
        auth(&home)["dimAccountState"]["state"]
    );
    root.shutdown().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// A completed login attempt is history: polling it after logout must not
/// republish its account over the watch's signed-out transition.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_completed_login_attempt_does_not_reenable_search_after_logout() {
    let fixture = oauth::start().await;
    let home = home();
    let Process { root, providers } = process(&home).await;
    let (_, attempt) = login_attempt(&providers, &fixture.base).await;
    await_usable(&providers, true).await;
    providers.logout(PROVIDER, &cancel()).await.unwrap();
    await_usable(&providers, false).await;
    let state = providers.attempt_status(&attempt, &cancel()).unwrap();
    assert!(
        matches!(state, LoginState::Completed { .. }),
        "the completed attempt stays readable as history: {state:?}"
    );
    assert!(
        !providers.usable(),
        "polling a completed attempt must not re-enable the account"
    );
    root.shutdown().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// A signed-in `auth.json`, as a completed login in another process leaves
/// it.
async fn signed_in_record(fixture: &oauth::Fixture) -> Vec<u8> {
    let source = home();
    let Process { root, providers } = process(&source).await;
    login(&providers, &fixture.base).await;
    let bytes = std::fs::read(source.join("auth.json")).unwrap();
    root.shutdown().await.unwrap();
    std::fs::remove_dir_all(source).unwrap();
    bytes
}

/// A signed-out `auth.json` a host without Dim's writer may leave.
fn signed_out_record() -> Vec<u8> {
    serde_json::to_vec_pretty(&json!({
        "dimAccountState": {
            "schemaVersion": 1,
            "credentialEpoch": uuid::Uuid::new_v4().to_string(),
            "generation": 1,
            "credentialRevision": 1,
            "state": "signed_out",
        }
    }))
    .unwrap()
}

/// Another process's login and logout reach this process's published account
/// at its next account read, in both directions.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn account_reads_follow_another_process_writes() {
    let fixture = oauth::start().await;
    let home = home();
    let first = process(&home).await;
    let second = process(&home).await;
    let service = second.root.get::<dyn ManagedProvider>().unwrap();
    let mut account = service.subscribe_account();
    tokio::time::timeout(Duration::from_secs(10), async {
        while *account.borrow_and_update() != Some(false) {
            account.changed().await.unwrap();
        }
    })
    .await
    .expect("the empty account file was not initialized");
    assert!(!second.providers.usable());

    login(&first.providers, &fixture.base).await;
    assert!(
        !second.providers.usable(),
        "another process's login must not move this watch by itself"
    );
    second.providers.status(PROVIDER, &cancel()).await.unwrap();
    await_usable(&second.providers, true).await;

    first.providers.logout(PROVIDER, &cancel()).await.unwrap();
    assert!(
        second.providers.usable(),
        "another process's logout must not move this watch by itself"
    );
    second.providers.status(PROVIDER, &cancel()).await.unwrap();
    await_usable(&second.providers, false).await;

    first.root.shutdown().await.unwrap();
    second.root.shutdown().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// A new Session on one server observes a sign-in or sign-out another server
/// wrote to the shared product home, without an `account.status` call.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_new_session_sees_an_account_change_another_server_wrote() {
    let fixture = oauth::start().await;
    let home = home();
    let first_home = home.join("first");
    let second_home = home.join("second");
    let first_capture = Arc::new(OnceLock::new());
    let second_capture = Arc::new(OnceLock::new());
    let ids = || ClientIds {
        global: "global-client".into(),
        cn: "cn-client".into(),
    };

    // Start the observing server first and wait for its startup observation,
    // so its watch holds that reading before the other server signs in.
    let (model, mut requests) = tokio::sync::mpsc::unbounded_channel();
    let model = Arc::new(RecordingModel(model));
    let mut second_builder = AgentBuilder::new(AgentConfig::default());
    defaults::install(&mut second_builder);
    defaults::mock_hosts_with_model(&mut second_builder, HistoryRenderer::default(), model);
    let capture = second_capture.clone();
    second_builder.plugin(move |_| Ok(Box::new(Capture(capture.clone())) as Box<dyn DynPlugin>));
    let second_provider = Arc::new(OnceLock::new());
    let capture = second_provider.clone();
    second_builder
        .plugin(move |_| Ok(Box::new(ProviderCapture(capture.clone())) as Box<dyn DynPlugin>));
    let second = serve(
        second_builder,
        ServerConfig {
            product_home: Some(home.clone()),
            database_path: Some(second_home.join("dimcode.sqlite")),
            unix_path: Some(second_home.join("second.sock")),
            managed_provider: Some(dim_oauth::row(ids())),
            startup_timeout: Duration::from_secs(60),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let mut observing = second_provider
        .get()
        .expect("the observing server's managed provider was not captured")
        .subscribe_account();
    tokio::time::timeout(Duration::from_secs(10), async {
        while *observing.borrow_and_update() != Some(false) {
            observing.changed().await.unwrap();
        }
    })
    .await
    .expect("the observing server did not publish a signed-out account");

    let mut first_builder = AgentBuilder::new(AgentConfig::default());
    defaults::install(&mut first_builder);
    defaults::mock_hosts(&mut first_builder);
    let capture = first_capture.clone();
    first_builder.plugin(move |_| Ok(Box::new(Capture(capture.clone())) as Box<dyn DynPlugin>));
    let first = serve(
        first_builder,
        ServerConfig {
            product_home: Some(home.clone()),
            database_path: Some(first_home.join("dimcode.sqlite")),
            unix_path: Some(first_home.join("first.sock")),
            managed_provider: Some(dim_oauth::row(ids())),
            startup_timeout: Duration::from_secs(60),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    let first_providers = first_capture
        .get()
        .expect("the first server's managed provider owner was not captured")
        .clone();
    assert!(
        !second_capture
            .get()
            .expect("the second server's managed provider owner was not captured")
            .usable(),
        "the observing server starts signed out"
    );
    login(&first_providers, &fixture.base).await;
    assert!(
        !second_capture.get().unwrap().usable(),
        "another server's login must not move this server's watch by itself"
    );

    let mut client = client_for(&second).await;
    select_model(&mut client).await;
    let id = create_web_session(&mut client, &home).await;
    let tools = submit_and_record(&mut client, &id, "signed in elsewhere", &mut requests).await;
    assert!(
        tools.iter().any(|name| name == "web_search"),
        "a new Session must observe the other server's sign-in: {tools:?}"
    );
    assert!(second_capture.get().unwrap().usable());

    first_providers.logout(PROVIDER, &cancel()).await.unwrap();
    let id = create_web_session(&mut client, &home).await;
    let tools = submit_and_record(&mut client, &id, "signed out elsewhere", &mut requests).await;
    assert!(
        !tools.iter().any(|name| name == "web_search"),
        "a new Session must observe the other server's sign-out: {tools:?}"
    );

    drop(client);
    first.shutdown();
    second.shutdown();
    first.join().await.unwrap();
    second.join().await.unwrap();
    drop(fixture);
    std::fs::remove_dir_all(home).unwrap();
}

/// A new Session's assembly and a switch to managed search both await one
/// account observation; a renewal holding the account lease must not stall
/// either of them.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_new_session_and_a_search_switch_do_not_wait_for_the_renewal_lease() {
    let fixture = oauth::start().await;
    let home = home();
    let state = home.join("state");
    let (model, mut requests) = tokio::sync::mpsc::unbounded_channel();
    let model = Arc::new(RecordingModel(model));
    let mut builder = AgentBuilder::new(AgentConfig::default());
    defaults::install(&mut builder);
    defaults::mock_hosts_with_model(&mut builder, HistoryRenderer::default(), model);
    let capture = Arc::new(OnceLock::new());
    let captured = capture.clone();
    builder.plugin(move |_| Ok(Box::new(Capture(captured.clone())) as Box<dyn DynPlugin>));
    let server = serve(
        builder,
        ServerConfig {
            product_home: Some(home.clone()),
            database_path: Some(state.join("dimcode.sqlite")),
            unix_path: Some(state.join("server.sock")),
            managed_provider: Some(dim_oauth::row(ClientIds {
                global: "global-client".into(),
                cn: "cn-client".into(),
            })),
            startup_timeout: Duration::from_secs(60),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let providers = capture
        .get()
        .expect("the server's managed provider was not captured")
        .clone();
    // The login grant is already inside the freshness window, so the
    // background renewal refreshes it and keeps the account lease across the
    // refresh request.
    *fixture.service.expires_in.lock().unwrap() = Some(json!("0"));
    fixture.service.refresh_wait.store(true, Ordering::SeqCst);
    login(&providers, &fixture.base).await;
    tokio::time::timeout(
        Duration::from_secs(10),
        fixture.service.refresh_started.notified(),
    )
    .await
    .expect("the background renewal did not start");

    // A switch to managed search reconciles through this call.
    let switched = Instant::now();
    assert!(providers.refresh_usable(&cancel()).await);
    assert!(
        switched.elapsed() < Duration::from_secs(5),
        "a search switch waited for the renewal's account lease"
    );

    // Session assembly awaits the same call before it decides its declaration.
    let started = Instant::now();
    let mut client = client_for(&server).await;
    select_model(&mut client).await;
    let id = create_web_session(&mut client, &home).await;
    let tools = submit_and_record(&mut client, &id, "unchanged account", &mut requests).await;
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "a new Session waited for the renewal's account lease"
    );
    assert!(
        tools.iter().any(|name| name == "web_search"),
        "a signed-in Session did not declare web_search: {tools:?}"
    );

    fixture.service.refresh_wait.store(false, Ordering::SeqCst);
    fixture.service.refresh_release.notify_one();
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    drop(fixture);
    std::fs::remove_dir_all(home).unwrap();
}

/// Records the tool names of every model request and answers like the mock
/// model.
struct RecordingModel(tokio::sync::mpsc::UnboundedSender<Vec<String>>);
impl ModelDriver for RecordingModel {
    fn invoke(
        &self,
        request: Arc<ModelRequest>,
        connection: ModelConnection,
        cancel: CancellationToken,
    ) -> HostFuture<'_, ModelStream> {
        let tools = request
            .tools
            .as_deref()
            .into_iter()
            .flatten()
            .map(|tool| tool.name.clone())
            .collect();
        let _ = self.0.send(tools);
        EchoModel.invoke(request, connection, cancel)
    }
}

async fn client_for(server: &RunningServer) -> Client {
    Client::connect(&ConnectionConfig {
        endpoint: server.unix_path.clone().unwrap(),
        server: Server {
            program: std::env::current_exe().unwrap(),
            embedded: false,
        },
        profile: None,
        client_kind: "cli",
        client_name: "dim-oauth-shared-home-test",
        protocol_version: 4,
    })
    .await
    .unwrap()
}

async fn select_model(client: &mut Client) {
    let created = client
        .rpc(
            "provider.create",
            json!({"providerId":"018ec73f-0000-7000-8000-000000000031","displayName":"Fixture",
                "connection":{"driverKind":"openai-compatible","baseUrl":"http://localhost/v1","auth":{"type":"anonymous"}},
                "models":[{"modelId":"fixture","displayName":"fixture","status":"active",
                    "capabilities":dim_agent_types::CatalogCapabilities::with_limits(128_000, 16_384)}],
                "enabled":true}),
        )
        .await
        .unwrap_or_else(|error| panic!("provider.create: {error}"));
    assert!(created.get("error").is_none(), "{created}");
    let selected = client
        .rpc(
            "provider.select",
            json!({"providerId":"018ec73f-0000-7000-8000-000000000031","modelId":"fixture"}),
        )
        .await
        .unwrap_or_else(|error| panic!("provider.select: {error}"));
    assert!(selected.get("error").is_none(), "{selected}");
}

async fn create_web_session(client: &mut Client, home: &Path) -> String {
    let created = client
        .rpc(
            "session.create",
            json!({"cwd":home,"tools":["web_search","web_fetch"],"interactive":false,
                "permissionPolicy":{"preset":"full-access","allowedTools":[],"disallowedTools":[]}}),
        )
        .await
        .unwrap_or_else(|error| panic!("session.create: {error}"));
    created["sessionId"].as_str().unwrap().to_owned()
}

async fn submit_and_record(
    client: &mut Client,
    id: &str,
    text: &str,
    requests: &mut tokio::sync::mpsc::UnboundedReceiver<Vec<String>>,
) -> Vec<String> {
    let submitted = client
        .rpc(
            "session.submit",
            json!({"sessionId":id,"branchId":"main","text":text}),
        )
        .await
        .unwrap_or_else(|error| panic!("session.submit: {error}"));
    assert!(submitted.get("error").is_none(), "{submitted}");
    let tools = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let tools = requests.recv().await.expect("the model host stopped");
            if tools.iter().any(|name| name == "web_fetch") {
                return tools;
            }
        }
    })
    .await
    .expect("the Session did not reach the model");
    let idle = client
        .rpc("session.idle", json!({"sessionId":id,"branchId":"main"}))
        .await
        .unwrap_or_else(|error| panic!("session.idle: {error}"));
    assert!(idle.get("error").is_none(), "{idle}");
    tools
}

/// An account read that waits for the account lease must publish what the
/// store holds when it reads: a logout that commits meanwhile is not reopened
/// by the observation it saw first.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_read_waiting_for_the_store_does_not_reopen_search_after_a_logout() {
    let fixture = oauth::start().await;
    let record = signed_in_record(&fixture).await;
    let home = home();
    std::fs::write(home.join("auth.json"), signed_out_record()).unwrap();
    let Process { root, providers } = process(&home).await;
    // The startup observation publishes before this test changes the shared
    // file, so no observation left from startup can publish a later state.
    let mut account = root
        .get::<dyn ManagedProvider>()
        .unwrap()
        .subscribe_account();
    tokio::time::timeout(Duration::from_secs(10), async {
        while *account.borrow_and_update() != Some(false) {
            account.changed().await.unwrap();
        }
    })
    .await
    .expect("the startup observation did not publish the signed-out record");
    std::fs::write(home.join("auth.json"), record.clone()).unwrap();
    providers.status(PROVIDER, &cancel()).await.unwrap();
    await_usable(&providers, true).await;
    providers.logout(PROVIDER, &cancel()).await.unwrap();
    await_usable(&providers, false).await;

    // Another process signs in while this store's watch stays withdrawn.
    std::fs::write(home.join("auth.json"), record).unwrap();
    assert!(!providers.usable());
    let files = root.get::<FileStorage>().unwrap();
    let path = files.path(0, "auth.json").unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let held = loop {
        match files.lock(&path, &cancel()).await {
            Ok(lease) => break lease,
            Err(FileStorageError::Locked) => {
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "the account lease stayed held"
                );
                tokio::task::yield_now().await;
            }
            Err(error) => panic!("the account lease failed: {error}"),
        }
    };
    let reading = tokio::spawn({
        let providers = providers.clone();
        async move { providers.status(PROVIDER, &cancel()).await }
    });
    // The read reaches the store while the lease is held; it must wait for the
    // lease before answering, not return the snapshot it read first.
    for _ in 0..1_000 {
        if reading.is_finished() {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert!(
        !reading.is_finished(),
        "a status read must wait for the account lease before answering"
    );
    // The logout lands while the read waits for the account lease.
    std::fs::write(home.join("auth.json"), signed_out_record()).unwrap();
    drop(held);
    let status = reading.await.unwrap().unwrap();
    assert!(
        status.account.is_none(),
        "the read must answer from the store it read under the lease"
    );
    assert!(
        !providers.usable(),
        "an earlier read must not reopen search after a logout"
    );
    root.shutdown().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn account_referrals_and_credit_resets_stay_with_the_authenticated_owner() {
    use dim_app_server::managed_provider::management::{ApplyReset, ResetType};

    let fixture = oauth::start().await;
    let home = home();
    let Process { root, providers } = process(&home).await;
    let account = login(&providers, &fixture.base).await;
    let usage = providers.usage(PROVIDER, &cancel()).await.unwrap();
    assert_eq!(usage.subscription_id, Some(42));
    assert_eq!(usage.resets.unwrap().window.available_count, 2);
    assert_eq!(
        usage.subscription_url.unwrap(),
        format!("{}/console/subscription", fixture.base)
    );
    assert_eq!(
        usage.pricing_url.unwrap(),
        format!("{}/console/pricing", fixture.base)
    );
    let referral = providers.referral(PROVIDER, &cancel()).await.unwrap();
    assert_eq!(referral.reward_granted_units, Some(12000));
    let invite = url::Url::parse(referral.invite_url.as_deref().unwrap()).unwrap();
    assert_eq!(
        invite.query_pairs().collect::<Vec<_>>(),
        [("aff".into(), "FRIEND +&".into())]
    );
    assert!(
        !serde_json::to_string(&referral)
            .unwrap()
            .contains("private-service-secret")
    );
    let batches = providers.reset_batches(PROVIDER, &cancel()).await.unwrap();
    assert_eq!(batches.window[0].grant_id, 7);
    assert_eq!(batches.window[0].subscription_id, Some(42));
    assert!(batches.monthly_full.is_empty());
    assert!(
        !serde_json::to_string(&batches)
            .unwrap()
            .contains("private-service-secret")
    );
    fixture
        .service
        .rejected
        .lock()
        .unwrap()
        .push("access-0".into());
    let request = || ApplyReset {
        provider_id: PROVIDER.into(),
        subscription_id: 42,
        reset_type: ResetType::Window,
        idempotency_key: "same-confirmed-action".into(),
    };
    let applied = providers.apply_reset(request(), &cancel()).await.unwrap();
    assert!(!applied.replayed);
    assert_eq!(applied.windows[0].token_used, 0);
    assert_eq!(applied.resets.window.available_count, 1);
    assert_eq!(fixture.service.refreshes.load(Ordering::SeqCst), 1);
    assert!(
        !serde_json::to_string(&applied)
            .unwrap()
            .contains("private-service-secret")
    );
    let replayed = providers.apply_reset(request(), &cancel()).await.unwrap();
    assert!(replayed.replayed);
    assert_eq!(
        fixture.service.reset_keys.lock().unwrap().as_slice(),
        ["same-confirmed-action"]
    );
    fixture
        .service
        .reset_forbidden
        .store(true, Ordering::SeqCst);
    let denied = providers
        .apply_reset(request(), &cancel())
        .await
        .unwrap_err();
    assert_eq!(denied.code, "account_reset_denied");
    assert!(!denied.message.contains("private-service-secret"));
    assert_eq!(fixture.service.refreshes.load(Ordering::SeqCst), 1);
    assert_eq!(
        providers.status(PROVIDER, &cancel()).await.unwrap(),
        account
    );
    let mut invalid = request();
    invalid.idempotency_key.clear();
    assert_eq!(
        providers
            .apply_reset(invalid, &cancel())
            .await
            .unwrap_err()
            .code,
        "account_invalid_input"
    );
    assert_eq!(fixture.service.calls("POST /api/me/usage-resets/apply"), 4);
    providers.logout(PROVIDER, &cancel()).await.unwrap();
    assert_eq!(
        providers
            .referral(PROVIDER, &cancel())
            .await
            .unwrap_err()
            .code,
        "account_required"
    );
    root.shutdown().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_relay_model_without_declared_limits_is_published_as_undeclared() {
    let fixture = oauth::start().await;
    *fixture.service.models.lock().unwrap() = Some(json!({"data":[
        {"id":"zzzz-relay-undisclosed","dim":{"id":"zzzz-relay-undisclosed","endpoint_type":"openai"}},
        {"id":"zzzz-relay-declared","dim":{"id":"zzzz-relay-declared","endpoint_type":"openai",
            "contextWindow":64000,"maxOutputTokens":8000}}
    ]}));
    let home = home();
    let Process { root, providers } = process(&home).await;
    login(&providers, &fixture.base).await;
    let catalog = providers.catalog(PROVIDER, &cancel()).await.unwrap();
    let model = |id: &str| {
        catalog
            .models
            .iter()
            .find(|model| model.entry.model_id == id)
            .unwrap()
            .clone()
    };
    // No relay value and no bundled match: the limit stays absent and the
    // model keeps a reason instead of being given a substitute window.
    let undeclared = model("zzzz-relay-undisclosed");
    assert_eq!(
        undeclared.entry.capabilities.missing_limits(),
        ["contextWindow", "maxOutputTokens"]
    );
    assert_eq!(
        undeclared.unavailable_reason.as_deref(),
        Some("limits_undeclared")
    );
    let declared = model("zzzz-relay-declared");
    assert!(declared.entry.capabilities.missing_limits().is_empty());
    assert_eq!(declared.unavailable_reason, None);
    root.shutdown().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// The distinct model ids the shipped catalog lists, in catalog order. A case
/// picks the id it asserts on from here by property, never by name.
fn listed_ids(catalog: &BundledCatalog) -> Vec<String> {
    let mut seen = HashSet::new();
    catalog
        .providers()
        .flat_map(|provider| provider.models.into_iter().map(|model| model.model_id))
        .filter(|id| seen.insert(id.clone()))
        .collect()
}

/// The shipped id a case uses as a media-only name: the lookup takes it with
/// no kind, its own kind is not `chat`, and stating it as `chat` leaves no row
/// to join. Every account serving asserts that a listing of this name takes no
/// limit from the media rows it matches.
fn media_only_id(catalog: &BundledCatalog) -> String {
    let projection = ProviderProjection::default();
    listed_ids(catalog)
        .into_iter()
        .find(|id| {
            let Some(entry) = catalog.fallback(&projection, id, None) else {
                return false;
            };
            entry.capabilities.missing_limits().is_empty()
                && entry
                    .metadata
                    .as_ref()
                    .and_then(|metadata| raw_kind(&metadata.raw))
                    .is_some_and(|kind| kind != "chat")
                && catalog.fallback(&projection, id, Some("chat")).is_none()
        })
        .expect("a shipped id whose name matches only media rows")
}

/// The shipped id a case uses as a chat-only name: the lookup takes it with no
/// kind, its own kind is `chat`, and stating it as an image-generation model
/// leaves no row to join.
fn chat_only_id(catalog: &BundledCatalog) -> String {
    let projection = ProviderProjection::default();
    listed_ids(catalog)
        .into_iter()
        .find(|id| {
            let Some(entry) = catalog.fallback(&projection, id, None) else {
                return false;
            };
            entry.capabilities.missing_limits().is_empty()
                && entry
                    .metadata
                    .as_ref()
                    .and_then(|metadata| raw_kind(&metadata.raw))
                    .as_deref()
                    == Some("chat")
                && catalog
                    .fallback(&projection, id, Some("image-generation"))
                    .is_none()
        })
        .expect("a shipped id whose name matches only chat rows")
}

/// The model one account listing publishes for `id`. The listing states
/// `endpoint_type` and no limit and no reasoning, so a limit the published
/// model carries can only have come from a bundled row the lookup found. Each
/// serving needs its own process, because a listing cannot carry one id twice.
async fn published_account_model(id: &str, mut dim: Value) -> ManagedModel {
    dim["id"] = json!(id);
    let fixture = oauth::start().await;
    *fixture.service.models.lock().unwrap() = Some(json!({"data":[{"id":id,"dim":dim}]}));
    let home = home();
    let Process { root, providers } = process(&home).await;
    login(&providers, &fixture.base).await;
    let catalog = providers.catalog(PROVIDER, &cancel()).await.unwrap();
    let model = catalog
        .models
        .into_iter()
        .find(|model| model.entry.model_id == id)
        .unwrap();
    root.shutdown().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
    model
}

/// The `type` the account published for a model, read as the one reader reads
/// any stated kind.
fn published_kind(model: &ManagedModel) -> Option<String> {
    model
        .entry
        .metadata
        .as_ref()
        .and_then(|metadata| raw_kind(&metadata.raw))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_account_model_stating_chat_takes_no_media_row() {
    let id = media_only_id(&BundledCatalog::load().unwrap());
    let model = published_account_model(&id, json!({"endpoint_type":"openai","type":"chat"})).await;
    assert_eq!(
        model.entry.capabilities.missing_limits(),
        ["contextWindow", "maxOutputTokens"],
        "a listing that states chat takes no limit from the media rows its name matches"
    );
    assert_eq!(published_kind(&model).as_deref(), Some("chat"));
    assert_eq!(
        model.unavailable_reason.as_deref(),
        Some("limits_undeclared")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_account_model_stating_chat_under_raw_takes_no_media_row() {
    let id = media_only_id(&BundledCatalog::load().unwrap());
    let model =
        published_account_model(&id, json!({"endpoint_type":"openai","raw":{"type":"chat"}})).await;
    assert_eq!(
        model.entry.capabilities.missing_limits(),
        ["contextWindow", "maxOutputTokens"],
        "a listing that states chat under raw takes no limit from the media rows its name matches"
    );
    assert_eq!(published_kind(&model).as_deref(), Some("chat"));
    assert_eq!(
        model.unavailable_reason.as_deref(),
        Some("limits_undeclared")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_account_model_flagged_for_image_generation_takes_no_chat_row() {
    let id = chat_only_id(&BundledCatalog::load().unwrap());
    let model = published_account_model(
        &id,
        json!({"endpoint_type":"openai","imageGeneration":true}),
    )
    .await;
    assert_eq!(
        model.entry.capabilities.missing_limits(),
        ["contextWindow", "maxOutputTokens"],
        "a listing flagged for image generation takes no limit from the chat rows its name matches"
    );
    assert_eq!(published_kind(&model).as_deref(), Some("image-generation"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rejected_credentials_recover_once_and_only_the_owner_signs_out() {
    let fixture = oauth::start().await;
    let home = home();
    let Process { root, providers } = process(&home).await;
    login(&providers, &fixture.base).await;
    let lease = providers
        .credential(PROVIDER, "fixture-model", &cancel())
        .await
        .unwrap();
    fixture
        .service
        .rejected
        .lock()
        .unwrap()
        .push("access-0".into());
    let recovered = providers.recover(&lease, &cancel()).await.unwrap();
    assert_eq!(recovered.bearer, "access-1");
    assert_eq!(recovered.account, lease.account);
    assert_eq!(fixture.service.refreshes.load(Ordering::SeqCst), 1);
    // A second rejection of the original grant adopts the newer one.
    let again = providers.recover(&lease, &cancel()).await.unwrap();
    assert_eq!(again.bearer, "access-1");
    assert_eq!(fixture.service.refreshes.load(Ordering::SeqCst), 1);
    // Final rejection of a superseded grant never removes its successor.
    providers.finish_rejected(&lease, &cancel()).await.unwrap();
    assert!(
        providers
            .status(PROVIDER, &cancel())
            .await
            .unwrap()
            .account
            .is_some()
    );
    providers
        .finish_rejected(&recovered, &cancel())
        .await
        .unwrap();
    let out = providers.status(PROVIDER, &cancel()).await.unwrap();
    assert!(out.account.is_none());
    assert!(auth(&home).get("nextApiOauth").is_none());

    // A rejected refresh signs the account out instead of retrying.
    login(&providers, &fixture.base).await;
    let lease = providers
        .credential(PROVIDER, "fixture-model", &cancel())
        .await
        .unwrap();
    fixture.service.reject_refresh.store(true, Ordering::SeqCst);
    let rejected = providers.recover(&lease, &cancel()).await.err().unwrap();
    assert_eq!(rejected.code, "account_required");
    assert!(
        providers
            .status(PROVIDER, &cancel())
            .await
            .unwrap()
            .account
            .is_none()
    );
    drop((lease, again, recovered));
    root.shutdown().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_expired_grant_rotates_once_across_two_processes() {
    let fixture = oauth::start().await;
    let home = home();
    let first = process(&home).await;
    let second = process(&home).await;
    // The issued grant is already inside the freshness window: the first
    // process's background refresher and both requests contend for it.
    *fixture.service.expires_in.lock().unwrap() = Some(json!("0"));
    login(&first.providers, &fixture.base).await;
    let token = cancel();
    let (a, b) = tokio::join!(
        first
            .providers
            .credential(PROVIDER, "fixture-model", &token),
        second
            .providers
            .credential(PROVIDER, "fixture-model", &token),
    );
    let (a, b) = (a.unwrap(), b.unwrap());
    assert_eq!(fixture.service.refreshes.load(Ordering::SeqCst), 1);
    assert_eq!(a.bearer, "access-1");
    assert_eq!(b.bearer, "access-1");
    assert_eq!(auth(&home)["nextApiOauth"]["refresh"], "refresh-1");
    drop((a, b));
    first.root.shutdown().await.unwrap();
    second.root.shutdown().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_idle_process_renews_its_grant_before_it_expires() {
    let fixture = oauth::start().await;
    let home = home();
    let Process { root, providers } = process(&home).await;
    // Enters the freshness window about a second after login.
    *fixture.service.expires_in.lock().unwrap() = Some(json!(31));
    login(&providers, &fixture.base).await;
    // No request asks for it: the process renews and persists the grant.
    tokio::time::timeout(Duration::from_secs(10), async {
        while auth(&home)["nextApiOauth"]["refresh"] != "refresh-1" {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the grant is renewed in the background");
    assert_eq!(fixture.service.refreshes.load(Ordering::SeqCst), 1);
    // The renewed grant is fresh: a request uses it without another refresh.
    let lease = providers
        .credential(PROVIDER, "fixture-model", &cancel())
        .await
        .unwrap();
    assert_eq!(lease.bearer, "access-1");
    assert_eq!(fixture.service.refreshes.load(Ordering::SeqCst), 1);
    drop(lease);
    // Shutdown ends the refresher instead of waiting out the hour.
    tokio::time::timeout(Duration::from_secs(5), root.shutdown())
        .await
        .unwrap()
        .unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// An account file written by the TypeScript plugin 0.2.0: a PluginFiles
/// record whose token digest that implementation computed.
fn typescript_record(issuer: &str, digest: &str) -> Vec<u8> {
    let tokens = json!({"type":"oauth","access":"legacy-access","refresh":"legacy-refresh",
        "expires":4102444800000u64,"issuer":issuer,"tokenEndpoint":format!("{issuer}/oauth/token"),
        "clientId":"legacy-client","relayBaseUrl":format!("{issuer}/v1"),"scope":"openid profile",
        "extra":{"～":1,"😀":2,"Z":0.1,"ä":[1e21,2.5,0,1e-7],"a\u{2028}b":"line\u{2028}sep \"quoted\""}});
    let doc = json!({
        "nextApiOauth": tokens,
        "dimAccountState": {"schemaVersion":1,"credentialEpoch":"0b6f9a52-7e4b-4a59-9d0c-9a0c1c2f7e11",
            "generation":3,"credentialRevision":5,"state":"authenticated",
            "identity":{"issuer":issuer,"subject":"legacy-subject","email":"legacy@example.com"},
            "tokenDigest":digest},
    });
    let text = format!("{}\n", serde_json::to_string_pretty(&doc).unwrap());
    serde_json::to_vec(&json!({"version":1,"text":text})).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn typescript_account_files_load_and_other_owners_fields_survive() {
    let fixture = oauth::start().await;
    let home = home();
    // A file holding only another owner's record is signed out for Dim.
    std::fs::write(
        home.join("auth.json"),
        serde_json::to_vec(&json!({"openaiCodex":{"type":"oauth","access":"codex"}})).unwrap(),
    )
    .unwrap();
    let Process { root, providers } = process(&home).await;
    let signed_out = providers.status(PROVIDER, &cancel()).await.unwrap();
    assert!(signed_out.account.is_none());
    assert_eq!(auth(&home)["openaiCodex"]["access"], "codex");

    std::fs::write(
        home.join("auth.json"),
        typescript_record(
            "https://dimagent.cn",
            "e18499a3329ac96db2ed0301e879c81c49c5967bda8821baf603186bfd416f97",
        ),
    )
    .unwrap();
    let account = providers.status(PROVIDER, &cancel()).await.unwrap();
    assert_eq!(
        account.credential_epoch,
        "0b6f9a52-7e4b-4a59-9d0c-9a0c1c2f7e11"
    );
    assert_eq!(account.generation, 3);
    assert_eq!(
        account.account.unwrap().email.as_deref(),
        Some("legacy@example.com")
    );
    // Another owner's field beside the Dim record is kept on the next write.
    let mut doc = auth(&home);
    let text: Value = serde_json::from_str(doc["text"].as_str().unwrap()).unwrap();
    doc["openaiCodex"] = json!({"type":"oauth","access":"codex"});
    std::fs::write(home.join("auth.json"), serde_json::to_vec(&doc).unwrap()).unwrap();
    let out = providers.logout(PROVIDER, &cancel()).await.unwrap();
    assert_eq!(out.generation, 4);
    let stored = auth(&home);
    assert_eq!(stored["openaiCodex"]["access"], "codex");
    assert_eq!(
        stored["dimAccountState"]["credentialEpoch"],
        text["dimAccountState"]["credentialEpoch"]
    );
    assert!(stored.get("text").is_none());

    // A digest mismatch needs explicit recovery, which confirms the identity online.
    std::fs::write(
        home.join("auth.json"),
        typescript_record(&fixture.base, &"0".repeat(64)),
    )
    .unwrap();
    let mismatch = providers.status(PROVIDER, &cancel()).await.err().unwrap();
    assert_eq!(mismatch.code, "account_recovery_required");
    let recovered = providers
        .recover_account(PROVIDER, &cancel())
        .await
        .unwrap();
    assert_eq!(recovered.generation, 1);
    assert_ne!(recovered.credential_epoch, account.credential_epoch);
    assert_eq!(recovered.account.unwrap().subject, "fixture-subject");
    assert_eq!(fixture.service.calls("GET /oauth/userinfo"), 1);
    root.shutdown().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn corrupt_records_reset_once_with_a_backup_and_unknown_schema_is_kept() {
    let home = home();
    let Process { root, providers } = process(&home).await;
    let corrupt = serde_json::to_vec(&json!({"dimAccountState":{"schemaVersion":1,
        "credentialEpoch":"not-a-uuid","generation":1,"credentialRevision":1,"state":"signed_out"},
        "otherProvider":{"secret":"kept"}}))
    .unwrap();
    std::fs::write(home.join("auth.json"), &corrupt).unwrap();
    let invalid = providers.status(PROVIDER, &cancel()).await.err().unwrap();
    assert_eq!(invalid.code, "account_invalid_store");
    let reset = providers.logout(PROVIDER, &cancel()).await.unwrap();
    assert!(reset.account.is_none());
    assert_eq!(
        std::fs::read(home.join("auth.json.dim-reset-backup")).unwrap(),
        corrupt
    );
    assert_eq!(auth(&home)["otherProvider"]["secret"], "kept");
    // An existing backup blocks another reset.
    std::fs::write(home.join("auth.json"), &corrupt).unwrap();
    let blocked = providers.logout(PROVIDER, &cancel()).await.err().unwrap();
    assert_eq!(blocked.code, "account_reset_blocked");
    assert_eq!(std::fs::read(home.join("auth.json")).unwrap(), corrupt);

    let unknown = serde_json::to_vec(&json!({"dimAccountState":{"schemaVersion":2}})).unwrap();
    std::fs::write(home.join("auth.json"), &unknown).unwrap();
    for error in [
        providers.status(PROVIDER, &cancel()).await.err().unwrap(),
        providers.logout(PROVIDER, &cancel()).await.err().unwrap(),
    ] {
        assert_eq!(error.code, "account_unsupported_schema");
    }
    assert_eq!(std::fs::read(home.join("auth.json")).unwrap(), unknown);
    root.shutdown().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// Holds the account lease from outside the server, as another process's
/// account writer does. The root keeps its FileStorage alive while the lease
/// is held.
async fn hold_account_lease(home: &Path) -> (Ctx, FileLease) {
    let root = Ctx::root().unwrap();
    let view = root.plugin(FileStoragePlugin::new(vec![home.to_path_buf()]));
    active(&view).await;
    let files = root.get::<FileStorage>().unwrap();
    let path = files.path(0, "auth.json").unwrap();
    let held = files.lock(&path, &cancel()).await.unwrap();
    (root, held)
}

/// The startup observation leaves an empty home signed out without creating
/// `auth.json`: a missing Dim record is published, not persisted.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_empty_home_starts_signed_out_without_creating_the_account_file() {
    let home = home();
    let Process { root, .. } = process(&home).await;
    let mut account = root
        .get::<dyn ManagedProvider>()
        .unwrap()
        .subscribe_account();
    tokio::time::timeout(Duration::from_secs(10), async {
        while *account.borrow_and_update() != Some(false) {
            account.changed().await.unwrap();
        }
    })
    .await
    .expect("the store never published its initialized observation");
    assert!(
        !home.join("auth.json").exists(),
        "the startup observation created the account file"
    );
    root.shutdown().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// A shared `auth.json` that an external writer populated while the startup
/// observation waited for the account lease keeps its bytes.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_startup_observation_does_not_overwrite_a_shared_account_file() {
    let home = home();
    let (_holding, held) = hold_account_lease(&home).await;
    let Process { root, .. } = process(&home).await;
    let shared = serde_json::to_vec(&json!({"otherProvider":{"secret":"kept"}})).unwrap();
    std::fs::write(home.join("auth.json"), &shared).unwrap();
    drop(held);
    let mut account = root
        .get::<dyn ManagedProvider>()
        .unwrap()
        .subscribe_account();
    tokio::time::timeout(Duration::from_secs(10), async {
        while *account.borrow_and_update() != Some(false) {
            account.changed().await.unwrap();
        }
    })
    .await
    .expect("the store never published its initialized observation");
    assert_eq!(
        std::fs::read(home.join("auth.json")).unwrap(),
        shared,
        "the startup observation replaced the shared account file"
    );
    root.shutdown().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// The account observation a new Session or a Settings change triggers
/// publishes the stored state without creating or rewriting `auth.json`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_account_observation_does_not_create_the_account_file() {
    let home = home();
    let Process { root, .. } = process(&home).await;
    let shared = serde_json::to_vec(&json!({"otherProvider":{"secret":"kept"}})).unwrap();
    std::fs::write(home.join("auth.json"), &shared).unwrap();
    let provider = root.get::<dyn ManagedProvider>().unwrap();
    provider.observe_account(PROVIDER).await.unwrap();
    let mut account = provider.subscribe_account();
    assert_eq!(
        *account.borrow_and_update(),
        Some(false),
        "the observation published the shared file's signed-out state"
    );
    assert_eq!(
        std::fs::read(home.join("auth.json")).unwrap(),
        shared,
        "the account observation rewrote the shared account file"
    );
    root.shutdown().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// The background account paths leave a legacy record's bytes alone: neither
/// the startup observation nor the renewal task normalizes `auth.json`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_legacy_account_record_is_not_rewritten_in_the_background() {
    let home = home();
    let (_holding, held) = hold_account_lease(&home).await;
    let Process { root, .. } = process(&home).await;
    let legacy = serde_json::to_vec(&json!({
        "nextApiOauth": {
            "type": "oauth",
            "access": "legacy-access",
            "refresh": "legacy-refresh",
            "clientId": "legacy-client",
            "expires": 4102444800000u64,
            "issuer": "https://dimcode.cn",
            "tokenEndpoint": "https://dimcode.cn/oauth/token",
            "authorizationEndpoint": "https://dimcode.cn/oauth/authorize",
            "relayBaseUrl": "https://dimcode.cn/v1",
        },
        "otherProvider": {"secret": "kept"},
    }))
    .unwrap();
    std::fs::write(home.join("auth.json"), &legacy).unwrap();
    drop(held);
    let mut account = root
        .get::<dyn ManagedProvider>()
        .unwrap()
        .subscribe_account();
    tokio::time::timeout(Duration::from_secs(10), async {
        while *account.borrow_and_update() != Some(false) {
            account.changed().await.unwrap();
        }
    })
    .await
    .expect("the store never published its initialized observation");
    assert_eq!(
        std::fs::read(home.join("auth.json")).unwrap(),
        legacy,
        "the background account paths rewrote the legacy account record"
    );
    root.shutdown().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// A signed-in record whose grant still stores a legacy `clientSecret`, with
/// the token digest the account schema requires over that grant.
fn client_secret_record() -> Vec<u8> {
    use sha2::{Digest, Sha256};
    let tokens = json!({
        "type": "oauth",
        "access": "client-secret-access",
        "refresh": "client-secret-refresh",
        "clientId": "client-secret-id",
        "clientSecret": "client-secret",
        "expires": 4102444800000u64,
        "issuer": "https://accounts.example",
        "tokenEndpoint": "https://accounts.example/token",
        "relayBaseUrl": "https://accounts.example/v1",
    });
    let digest = format!("{:x}", Sha256::digest(serde_jcs::to_vec(&tokens).unwrap()));
    serde_json::to_vec(&json!({
        "nextApiOauth": tokens,
        "dimAccountState": {
            "schemaVersion": 1,
            "credentialEpoch": uuid::Uuid::new_v4().to_string(),
            "generation": 1,
            "credentialRevision": 1,
            "state": "authenticated",
            "identity": {"issuer": "https://accounts.example", "subject": "client-secret-subject"},
            "tokenDigest": digest,
        }
    }))
    .unwrap()
}

/// An unchanged observation answers from the atomic read while a renewal
/// holds the account lease, and publishes nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unchanged_account_observation_does_not_wait_for_the_renewal_lease() {
    let fixture = oauth::start().await;
    let home = home();
    let Process { root, providers } = process(&home).await;
    // The login grant is already inside the freshness window, so the
    // background renewal refreshes it and keeps the account lease across the
    // refresh request.
    *fixture.service.expires_in.lock().unwrap() = Some(json!("0"));
    fixture.service.refresh_wait.store(true, Ordering::SeqCst);
    login(&providers, &fixture.base).await;
    let provider = root.get::<dyn ManagedProvider>().unwrap();
    let mut account = provider.subscribe_account();
    tokio::time::timeout(Duration::from_secs(10), async {
        while *account.borrow_and_update() != Some(true) {
            account.changed().await.unwrap();
        }
    })
    .await
    .expect("the login was not published");
    tokio::time::timeout(
        Duration::from_secs(10),
        fixture.service.refresh_started.notified(),
    )
    .await
    .expect("the background renewal did not start");
    tokio::time::timeout(Duration::from_secs(5), provider.observe_account(PROVIDER))
        .await
        .expect("an unchanged observation waited for the renewal's account lease")
        .unwrap();
    assert!(
        !account.has_changed().unwrap(),
        "an unchanged observation must not publish over the watch"
    );
    fixture.service.refresh_wait.store(false, Ordering::SeqCst);
    fixture.service.refresh_release.notify_one();
    root.shutdown().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// A changed observation waits for the account lease, rereads the file and
/// publishes what that read holds.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_observation_of_a_changed_account_reads_it_under_the_lease() {
    let fixture = oauth::start().await;
    let home = home();
    let Process { root, providers } = process(&home).await;
    login(&providers, &fixture.base).await;
    await_usable(&providers, true).await;
    let (_holding, held) = hold_account_lease(&home).await;
    std::fs::write(home.join("auth.json"), signed_out_record()).unwrap();
    let reading = tokio::spawn({
        let provider = root.get::<dyn ManagedProvider>().unwrap();
        async move { provider.observe_account(PROVIDER).await }
    });
    for _ in 0..1_000 {
        if reading.is_finished() {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert!(
        !reading.is_finished(),
        "an observation of a changed record must read it under the account lease"
    );
    drop(held);
    reading.await.unwrap().unwrap();
    await_usable(&providers, false).await;
    root.shutdown().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// A signed-in record that still stores a `clientSecret` is observed without
/// the account lease: validation must not exclude the legacy field.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_record_with_a_client_secret_is_observed_without_the_lease() {
    let home = home();
    std::fs::write(home.join("auth.json"), client_secret_record()).unwrap();
    let Process { root, .. } = process(&home).await;
    let provider = root.get::<dyn ManagedProvider>().unwrap();
    let mut account = provider.subscribe_account();
    tokio::time::timeout(Duration::from_secs(10), async {
        while *account.borrow_and_update() != Some(true) {
            account.changed().await.unwrap();
        }
    })
    .await
    .expect("the startup observation did not publish the signed-in record");
    let (_holding, held) = hold_account_lease(&home).await;
    tokio::time::timeout(Duration::from_secs(5), provider.observe_account(PROVIDER))
        .await
        .expect("an unchanged observation of a record with a client secret took the account lease")
        .unwrap();
    drop(held);
    root.shutdown().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// A home without an account file is signed out, and observing that state
/// does not take the account lease either.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_empty_home_observes_signed_out_without_the_account_lease() {
    let home = home();
    let Process { root, .. } = process(&home).await;
    let provider = root.get::<dyn ManagedProvider>().unwrap();
    let mut account = provider.subscribe_account();
    tokio::time::timeout(Duration::from_secs(10), async {
        while *account.borrow_and_update() != Some(false) {
            account.changed().await.unwrap();
        }
    })
    .await
    .expect("the store never published its initialized observation");
    let (_holding, held) = hold_account_lease(&home).await;
    tokio::time::timeout(Duration::from_secs(5), provider.observe_account(PROVIDER))
        .await
        .expect("an empty home took the account lease to observe its signed-out state")
        .unwrap();
    assert!(
        !home.join("auth.json").exists(),
        "the observation created the account file"
    );
    drop(held);
    root.shutdown().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// The bounded wait a Session puts on its account observation also ends the
/// observation: after the timeout nothing keeps polling the account lease.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_timed_out_observation_leaves_no_lease_retry() {
    let fixture = oauth::start().await;
    let home = home();
    let Process { root, providers } = process(&home).await;
    login(&providers, &fixture.base).await;
    await_usable(&providers, true).await;
    let provider = root.get::<dyn ManagedProvider>().unwrap();
    let mut account = provider.subscribe_account();
    account.borrow_and_update();
    let (_holding, held) = hold_account_lease(&home).await;
    std::fs::write(home.join("auth.json"), signed_out_record()).unwrap();
    // The observation stays blocked on the held lease until this wait ends.
    assert!(providers.refresh_usable(&cancel()).await);
    drop(held);
    let published = tokio::time::timeout(Duration::from_secs(5), account.changed()).await;
    assert!(
        published.is_err(),
        "a timed-out observation kept polling the account lease and published after it was released"
    );
    root.shutdown().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// Dropping a caller's observation ends its lease wait instead of leaving a
/// retry behind.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_dropped_observation_ends_its_lease_wait() {
    let fixture = oauth::start().await;
    let home = home();
    let Process { root, providers } = process(&home).await;
    login(&providers, &fixture.base).await;
    await_usable(&providers, true).await;
    let provider = root.get::<dyn ManagedProvider>().unwrap();
    let mut account = provider.subscribe_account();
    account.borrow_and_update();
    let (_holding, held) = hold_account_lease(&home).await;
    std::fs::write(home.join("auth.json"), signed_out_record()).unwrap();
    let observation = provider.observe_account(PROVIDER);
    tokio::task::yield_now().await;
    drop(observation);
    drop(held);
    let published = tokio::time::timeout(Duration::from_secs(5), account.changed()).await;
    assert!(
        published.is_err(),
        "an observation its caller dropped kept polling the account lease and published after it was released"
    );
    root.shutdown().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn browser_login_checks_the_callback_and_quiesce_stops_admission() {
    let fixture = oauth::start().await;
    let home = home();
    let Process { root, providers } = process(&home).await;
    let begin = |redirect: &str| BrowserLoginBegin {
        provider_id: PROVIDER.into(),
        directory: fixture.base.clone(),
        email: Some(" user@example.com ".into()),
        redirect_uri: redirect.into(),
    };
    let insecure = providers
        .begin_browser(begin("https://127.0.0.1:1455/auth/callback"), &cancel())
        .await
        .err()
        .unwrap();
    assert_eq!(insecure.code, "account_invalid_input");
    let redirect = "http://127.0.0.1:1455/auth/callback";
    let start = providers
        .begin_browser(begin(redirect), &cancel())
        .await
        .unwrap();
    let url = url::Url::parse(&start.authorization_url).unwrap();
    assert_eq!(url.path(), "/oauth/authorize");
    let query: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
    assert_eq!(query["client_id"], "cn-client");
    assert_eq!(query["redirect_uri"], redirect);
    assert_eq!(query["code_challenge_method"], "S256");
    assert_eq!(query["login_hint"], "user@example.com");
    let complete = |attempt: &str, state: &str| ManagedBrowserLoginComplete {
        provider_id: PROVIDER.into(),
        attempt_id: attempt.into(),
        callback_url: format!("{redirect}?code=browser-code&state={state}"),
    };
    let forged = providers
        .complete_browser(complete(&start.attempt_id, "forged"), &cancel())
        .await
        .err()
        .unwrap();
    assert_eq!(forged.code, "account_invalid_input");
    let attempt = ManagedLoginAttempt {
        provider_id: PROVIDER.into(),
        attempt_id: start.attempt_id.clone(),
    };
    assert!(matches!(
        providers.attempt_status(&attempt, &cancel()).unwrap(),
        LoginState::Failed { .. }
    ));
    let closed = providers
        .complete_browser(complete(&start.attempt_id, &query["state"]), &cancel())
        .await
        .err()
        .unwrap();
    assert_eq!(closed.code, "login_attempt_closed");

    let start = providers
        .begin_browser(begin(redirect), &cancel())
        .await
        .unwrap();
    let state = url::Url::parse(&start.authorization_url)
        .unwrap()
        .query_pairs()
        .find(|(k, _)| k == "state")
        .unwrap()
        .1
        .into_owned();
    let account = providers
        .complete_browser(complete(&start.attempt_id, &state), &cancel())
        .await
        .unwrap();
    assert_eq!(account.account.unwrap().subject, "fixture-subject");
    assert_eq!(
        fixture
            .service
            .calls("POST /oauth/token authorization_code"),
        1
    );

    providers.quiesce().await;
    let stopped = providers.status(PROVIDER, &cancel()).await.err().unwrap();
    assert_eq!(stopped.code, "account_provider_unavailable");
    root.shutdown().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// A fresh grant is read without the account lease: it answers while another
/// holder owns `auth.json`, and leaves that holder's marker untouched. A
/// grant inside the freshness window still takes the lease, so two processes
/// rotate it once.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_fresh_grant_is_read_without_the_account_lease() {
    let fixture = oauth::start().await;
    let home = home();
    let first = process(&home).await;
    let second = process(&home).await;
    let account = login(&first.providers, &fixture.base).await;
    let files = second.root.get::<FileStorage>().unwrap();
    let path = files.path(0, "auth.json").unwrap();
    let held = files.lock(&path, &cancel()).await.unwrap();
    let marker = home.join("auth.json.lock");
    let holder = std::fs::read(&marker).unwrap();
    assert!(!holder.is_empty());
    for providers in [&first.providers, &second.providers] {
        let lease = tokio::time::timeout(
            Duration::from_secs(5),
            providers.credential(PROVIDER, "fixture-model", &cancel()),
        )
        .await
        .expect("a fresh grant does not wait for the account lease")
        .unwrap();
        assert_eq!(lease.bearer, "access-0");
        assert_eq!(lease.account, account);
    }
    assert_eq!(std::fs::read(&marker).unwrap(), holder);
    drop(held);
    assert!(!marker.exists());

    first.providers.logout(PROVIDER, &cancel()).await.unwrap();
    *fixture.service.expires_in.lock().unwrap() = Some(json!(0));
    login(&first.providers, &fixture.base).await;
    let token = cancel();
    let (a, b) = tokio::join!(
        first
            .providers
            .credential(PROVIDER, "fixture-model", &token),
        second
            .providers
            .credential(PROVIDER, "fixture-model", &token),
    );
    let (a, b) = (a.unwrap(), b.unwrap());
    assert_eq!(fixture.service.refreshes.load(Ordering::SeqCst), 1);
    assert_eq!(a.bearer, "access-2");
    assert_eq!(b.bearer, "access-2");
    assert_eq!(auth(&home)["nextApiOauth"]["refresh"], "refresh-2");
    drop((a, b));
    first.root.shutdown().await.unwrap();
    second.root.shutdown().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// Timing harness for #604: a fresh-grant lookup must not wait for the
/// account lease. Run with `cargo test -p dim-oauth --test dim_oauth_flow
/// fresh_credential_lookup_benchmark -- --ignored --nocapture`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "timing harness"]
async fn fresh_credential_lookup_benchmark() {
    const LOOKUPS: usize = 500;
    let fixture = oauth::start().await;
    let home = home();
    let Process { root, providers } = process(&home).await;
    login(&providers, &fixture.base).await;
    for _ in 0..10 {
        drop(
            providers
                .credential(PROVIDER, "fixture-model", &cancel())
                .await
                .unwrap(),
        );
    }
    let started = Instant::now();
    for _ in 0..LOOKUPS {
        drop(
            providers
                .credential(PROVIDER, "fixture-model", &cancel())
                .await
                .unwrap(),
        );
    }
    let per_lookup = started.elapsed().as_secs_f64() * 1e6 / LOOKUPS as f64;
    eprintln!("fresh credential lookup: {per_lookup:.1} us over {LOOKUPS} lookups");
    assert_eq!(fixture.service.refreshes.load(Ordering::SeqCst), 0);
    root.shutdown().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// Provider's process-level model host is never invoked by these flows.
struct UnusedModel;
impl ModelDriver for UnusedModel {
    fn invoke(
        &self,
        _: Arc<ModelRequest>,
        _: ModelConnection,
        _: CancellationToken,
    ) -> HostFuture<'_, ModelStream> {
        Box::pin(async { Err(HostError::failed("the test never invokes a model")) })
    }
}
struct ModelHostFixture;
impl DynPlugin for ModelHostFixture {
    fn name(&self) -> &str {
        "test.model-host"
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            ctx.provide(ModelHost(Arc::new(UnusedModel)))?;
            Ok(Effect::Done)
        })
    }
}

/// The process with Provider mounted over the managed account's SQLite
/// storage, as the App Server mounts it.
async fn provider_process(home: &Path) -> Process {
    let process = process(home).await;
    let catalog = dim_app_server::catalog();
    let build = rutis_loader::resolve::Build::default();
    let names = Names::copy(&catalog, &build);
    let views = [
        process
            .root
            .plugin(SqliteStoragePlugin::new(home.join("native.sqlite"))),
        process
            .root
            .plugin(dim_app_server::codex_oauth::OpenAICodexOAuthPlugin::new(&names, 0).unwrap()),
        process
            .root
            .plugin(dim_app_server::grok_oauth::GrokOAuthPlugin::new(&names, 0).unwrap()),
        process.root.plugin(ModelHostFixture),
        process.root.plugin(ProviderPlugin::new(&names).unwrap()),
    ];
    for view in &views {
        active(view).await;
    }
    process
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn managed_catalog_writes_distinguish_account_changes_from_version_conflicts() {
    let fixture = oauth::start().await;
    let home = home();
    let Process { root, providers } = provider_process(&home).await;
    let mut account = login(&providers, &fixture.base).await;
    let provider = root.get::<ProviderService>().unwrap();
    let catalog = provider.refresh(PROVIDER.into(), &cancel()).await.unwrap();
    let mut version = catalog.version;

    let revision = auth(&home)["dimAccountState"]["credentialRevision"]
        .as_u64()
        .unwrap();
    let lease = providers
        .credential(PROVIDER, "fixture-model", &cancel())
        .await
        .unwrap();
    let rotated = providers.recover(&lease, &cancel()).await.unwrap();
    assert_eq!(rotated.account, account);
    assert_eq!(
        auth(&home)["dimAccountState"]["credentialRevision"],
        revision + 1
    );
    let written = provider
        .set_enabled_models(PROVIDER.into(), version, vec![], &cancel())
        .await
        .unwrap();
    version = written.version;
    drop((lease, rotated));

    for recover in [false, true] {
        let next = if recover {
            let mut doc = auth(&home);
            doc["dimAccountState"]["tokenDigest"] = json!("0".repeat(64));
            std::fs::write(home.join("auth.json"), serde_json::to_vec(&doc).unwrap()).unwrap();
            let next = providers
                .recover_account(PROVIDER, &cancel())
                .await
                .unwrap();
            assert_ne!(next.credential_epoch, account.credential_epoch);
            next
        } else {
            let next = login(&providers, &fixture.base).await;
            assert_eq!(next.credential_epoch, account.credential_epoch);
            assert_eq!(next.generation, account.generation + 1);
            next
        };
        assert_eq!(next.account, account.account);
        account = next;
        assert!(matches!(
            provider
                .set_enabled_models(PROVIDER.into(), version - 1, vec![], &cancel())
                .await,
            Err(ProviderError::Conflict(_))
        ));
        let stale = provider
            .set_enabled_models(PROVIDER.into(), version, vec![], &cancel())
            .await
            .unwrap_err();
        match stale {
            ProviderError::CatalogStale {
                provider_id,
                expected_version,
                current_version,
                issuer,
                subject,
            } => {
                assert_eq!(provider_id, PROVIDER);
                assert_eq!(expected_version, version);
                assert_eq!(current_version, version);
                let identity = account.account.as_ref().unwrap();
                assert_eq!(issuer.as_deref(), Some(identity.issuer.as_str()));
                assert_eq!(subject.as_deref(), Some(identity.subject.as_str()));
            }
            other => panic!("expected a stale catalog after account change: {other:?}"),
        }
        let refreshed = provider.refresh(PROVIDER.into(), &cancel()).await.unwrap();
        assert_eq!(refreshed.version, version + 1);
        let written = provider
            .set_enabled_models(PROVIDER.into(), refreshed.version, vec![], &cancel())
            .await
            .unwrap();
        assert_eq!(written.version, version + 2);
        version = written.version;
    }

    providers.logout(PROVIDER, &cancel()).await.unwrap();
    assert!(matches!(
        provider
            .set_enabled_models(PROVIDER.into(), version, vec![], &cancel())
            .await,
        Err(ProviderError::CatalogStale {
            issuer: None,
            subject: None,
            ..
        })
    ));
    let read = provider
        .management(PROVIDER.into(), None, &cancel())
        .await
        .unwrap();
    assert_eq!(read.providers[0].info.version, Some(version));
    let overview = provider
        .management_page(
            dim_app_server::provider::ManagementPageRequest {
                query: Some("Dim OAuth".into()),
                accounts_only: true,
                ..Default::default()
            },
            &cancel(),
        )
        .await
        .unwrap();
    assert_eq!(overview.providers.len(), 1);
    assert_eq!(
        overview.providers[0].version,
        Some(version),
        "searching the account descriptor still reads its stored catalog version"
    );
    let cross_field = provider
        .management_page(
            dim_app_server::provider::ManagementPageRequest {
                query: Some(format!("{PROVIDER} Dim OAuth")),
                accounts_only: true,
                ..Default::default()
            },
            &cancel(),
        )
        .await
        .unwrap();
    assert!(
        cross_field.providers.is_empty(),
        "search matches each field separately"
    );
    root.shutdown().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// A managed bundle's media model leases for its non-conversational use, and
/// a chat request for the same model is refused before the lease.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn managed_media_model_leases_for_modality_and_chat_refuses_it() {
    let fixture = oauth::start().await;
    let home = home();
    let Process { root, providers } = provider_process(&home).await;
    login(&providers, &fixture.base).await;
    let provider = root.get::<ProviderService>().unwrap();
    provider
        .refresh_for(PROVIDER.into(), &cancel())
        .await
        .unwrap();
    let operations = root
        .get::<SelectableModelsHost>()
        .unwrap()
        .selectable_for_modality(cancel())
        .await
        .unwrap()
        .models
        .into_iter()
        .find(|model| {
            model.target.provider_id == PROVIDER && model.target.model_id == "fixture-image"
        })
        .expect("the managed media model is a modality option");
    assert_eq!(operations.operations, ["image.generate"]);
    let connection = root
        .get::<ModalityConnectionHost>()
        .unwrap()
        .0
        .resolve_for_modality(
            ModalityTarget {
                target: ModelId {
                    provider_id: PROVIDER.into(),
                    model_id: "fixture-image".into(),
                },
                operation: "image.generate".into(),
            },
            cancel(),
        )
        .await
        .unwrap();
    assert_eq!(connection.model, "fixture-image");
    assert_eq!(connection.protocol, ModelProtocol::OpenaiCompatible);
    assert!(connection.resolved.recovery.is_some());
    let request = Arc::new(ModelRequest {
        capability_snapshot: Some(ModelCapabilitySnapshot {
            target: ModelId {
                provider_id: PROVIDER.into(),
                model_id: "fixture-image".into(),
            },
            capabilities: ModelCapabilities::with_limits(32000, 1000),
            reasoning: None,
            service_tier: None,
        }),
        ..ModelRequest::new(
            PROVIDER,
            "fixture-image",
            vec![Message::text(Role::User, "hello")],
        )
    });
    let refused = root
        .get::<ProviderConnectionResolver>()
        .unwrap()
        .0
        .resolve(request, cancel())
        .await
        .err()
        .unwrap();
    assert_eq!(refused.to_string(), "Model is unavailable");
    root.shutdown().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// A record persisted before the projection fix stores a chat-kind reason for
/// a model the relay marked unavailable; the status alone excludes it from
/// every use, so neither read lists it and no credential is leased.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stored_unavailable_media_model_is_excluded_from_every_use() {
    let fixture = oauth::start().await;
    let home = home();
    let Process { root, providers } = provider_process(&home).await;
    login(&providers, &fixture.base).await;
    let provider = root.get::<ProviderService>().unwrap();
    provider
        .refresh_for(PROVIDER.into(), &cancel())
        .await
        .unwrap();
    let database = rusqlite::Connection::open(home.join("native.sqlite")).unwrap();
    let body: String = database
        .query_row(
            "SELECT entry FROM provider_models WHERE provider_id=?1 AND model_id='fixture-image' AND source='default'",
            [PROVIDER],
            |row| row.get(0),
        )
        .unwrap();
    let mut model: Value = serde_json::from_str(&body).unwrap();
    // The refresh wrote the kind reason; the status is the only unavailable
    // fact, exactly as a record from before the projection fix could hold it.
    assert_eq!(model["unavailableReason"], "unsupported_model_type");
    model["entry"]["status"] = json!("unavailable");
    database
        .execute(
            "UPDATE provider_models SET entry=?2 WHERE provider_id=?1 AND model_id='fixture-image' AND source='default'",
            rusqlite::params![PROVIDER, model.to_string()],
        )
        .unwrap();
    drop(database);

    let listed = root
        .get::<SelectableModelsHost>()
        .unwrap()
        .selectable_for_modality(cancel())
        .await
        .unwrap()
        .models;
    assert!(
        listed
            .iter()
            .all(|model| model.target.provider_id != PROVIDER
                || model.target.model_id != "fixture-image"),
        "the stored status excludes the model from the modality read"
    );
    let refused = root
        .get::<ModalityConnectionHost>()
        .unwrap()
        .0
        .resolve_for_modality(
            ModalityTarget {
                target: ModelId {
                    provider_id: PROVIDER.into(),
                    model_id: "fixture-image".into(),
                },
                operation: "image.generate".into(),
            },
            cancel(),
        )
        .await
        .err()
        .unwrap();
    assert_eq!(refused.to_string(), "Model is unavailable");
    root.shutdown().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn directory_reads_share_only_in_flight_work_and_cancel_independently() {
    let fixture = oauth::start().await;
    let home = home();
    let Process { root, providers } = process(&home).await;
    login(&providers, &fixture.base).await;
    fixture.service.models_wait.store(true, Ordering::SeqCst);
    let first_cancel = cancel();
    let first = {
        let providers = providers.clone();
        let token = first_cancel.clone();
        tokio::spawn(async move { providers.catalog(PROVIDER, &token).await })
    };
    tokio::time::timeout(Duration::from_secs(10), async {
        while fixture.service.calls("GET /v1/models") == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let second_cancel = cancel();
    let mut second = Box::pin(providers.catalog(PROVIDER, &second_cancel));
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut second)
            .await
            .is_err()
    );
    first_cancel.cancel();
    assert_eq!(
        first.await.unwrap().err().unwrap().code,
        "account_cancelled"
    );
    fixture.service.models_wait.store(false, Ordering::SeqCst);
    fixture.service.models_release.notify_waiters();
    let result = second.await.unwrap();
    assert!(!result.models.is_empty());
    assert_eq!(fixture.service.calls("GET /v1/models"), 1);
    providers.catalog(PROVIDER, &cancel()).await.unwrap();
    assert_eq!(
        fixture.service.calls("GET /v1/models"),
        2,
        "a completed result is never cached"
    );
    fixture.service.models_wait.store(true, Ordering::SeqCst);
    let final_cancel = cancel();
    let pending = {
        let providers = providers.clone();
        let token = final_cancel.clone();
        tokio::spawn(async move { providers.catalog(PROVIDER, &token).await })
    };
    tokio::time::timeout(Duration::from_secs(10), async {
        while fixture.service.calls("GET /v1/models") < 3 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    final_cancel.cancel();
    assert_eq!(
        pending.await.unwrap().err().unwrap().code,
        "account_cancelled"
    );
    fixture.service.models_wait.store(false, Ordering::SeqCst);
    providers.catalog(PROVIDER, &cancel()).await.unwrap();
    assert_eq!(
        fixture.service.calls("GET /v1/models"),
        4,
        "a cancelled flight cannot absorb the next read"
    );
    fixture.service.models_wait.store(true, Ordering::SeqCst);
    let pending = {
        let providers = providers.clone();
        tokio::spawn(async move { providers.catalog(PROVIDER, &cancel()).await })
    };
    tokio::time::timeout(Duration::from_secs(10), async {
        while fixture.service.calls("GET /v1/models") < 5 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    providers.logout(PROVIDER, &cancel()).await.unwrap();
    assert_eq!(
        pending.await.unwrap().err().unwrap().code,
        "account_cancelled",
        "logout retires the old account's directory request"
    );
    root.shutdown().await.unwrap();
    fixture.service.models_release.notify_waiters();
    std::fs::remove_dir_all(home).unwrap();
}

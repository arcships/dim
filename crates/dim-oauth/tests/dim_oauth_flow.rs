#![cfg(unix)]
//! DimOAuth through the process's ManagedProviders: login, catalog, leases,
//! recovery and the `auth.json` account record, against a local account
//! service.
#[path = "fixtures/dim_oauth.rs"]
mod oauth;

use dim_agent_capabilities::{
    file_storage::{FileStorage, FileStoragePlugin},
    host::{
        HostError, HostFuture, ModalityConnectionHost, ModalityTarget, ModelConnection,
        ModelDriver, ModelHost, ModelProtocol, ModelStream, ProviderConnectionResolver,
        SelectableModelsHost,
    },
};
use dim_agent_runtime::Names;
use dim_agent_types::{
    Message, ModelCapabilities, ModelCapabilitySnapshot, ModelId, ModelRequest, Role,
};
use dim_app_server::{
    managed_provider::{
        AccountSnapshot, ManagedProviderBinding, ManagedProviders, ManagedProvidersPlugin,
        management::{
            BrowserLoginBegin, LoginBegin, LoginState, ManagedBrowserLoginComplete,
            ManagedLoginAttempt,
        },
    },
    provider::{ProviderPlugin, ProviderService},
    storage::SqliteStoragePlugin,
};
use dim_oauth::{ClientIds, DimOAuthPlugin};
use rutis::Ctx;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, FiberState, Key, TypeKey};
use serde_json::{Value, json};
use std::{
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::{Arc, OnceLock, atomic::Ordering},
    time::{Duration, Instant},
};
use tokio_util::sync::CancellationToken;

const PROVIDER: &str = "dimcode-api-oauth";

struct Capture {
    captured: Arc<OnceLock<Arc<ManagedProviders>>>,
    owner: Key<ManagedProviders>,
}
impl Capture {
    fn new(
        names: &Names<'_>,
        captured: Arc<OnceLock<Arc<ManagedProviders>>>,
    ) -> rutis::Result<Self> {
        Ok(Self {
            captured,
            owner: names.key()?,
        })
    }
}
impl DynPlugin for Capture {
    fn name(&self) -> &str {
        "capture-managed"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.owner.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let owner = ctx.require_at(&self.owner)?;
            let _ = self.captured.set(owner);
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
        root.plugin(ManagedProvidersPlugin),
        root.plugin(DimOAuthPlugin::new(&names, 0, ids).unwrap()),
        root.plugin(ManagedProviderBinding::new(&names).unwrap()),
        root.plugin(Capture::new(&names, captured.clone()).unwrap()),
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
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match providers.attempt_status(&attempt, &cancel()).unwrap() {
                LoginState::Pending => tokio::time::sleep(Duration::from_millis(50)).await,
                LoginState::Completed { account } => break account,
                state => panic!("device login ended as {state:?}"),
            }
        }
    })
    .await
    .unwrap()
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

    let searched = providers
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
        .selectable_for_modality()
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
        .resolve_modality_connection(ModalityTarget {
            target: ModelId {
                provider_id: PROVIDER.into(),
                model_id: "fixture-image".into(),
            },
            operation: "image.generate".into(),
        })
        .await
        .unwrap();
    assert_eq!(connection.model, "fixture-image");
    assert_eq!(connection.protocol, ModelProtocol::OpenaiCompatible);
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
    let (version, body): (u32, String) = database
        .query_row(
            "SELECT version,record FROM providers WHERE provider_id=?1",
            [PROVIDER],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    let mut body: Value = serde_json::from_str(&body).unwrap();
    let model = body["models"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|model| model["entry"]["modelId"] == "fixture-image")
        .unwrap();
    // The refresh wrote the kind reason; the status is the only unavailable
    // fact, exactly as a record from before the projection fix could hold it.
    assert_eq!(model["unavailableReason"], "unsupported_model_type");
    model["entry"]["status"] = json!("unavailable");
    database
        .execute(
            "UPDATE providers SET version=?2,record=?3 WHERE provider_id=?1",
            rusqlite::params![PROVIDER, version, body.to_string()],
        )
        .unwrap();
    drop(database);

    let listed = root
        .get::<SelectableModelsHost>()
        .unwrap()
        .selectable_for_modality()
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
        .resolve_modality_connection(ModalityTarget {
            target: ModelId {
                provider_id: PROVIDER.into(),
                model_id: "fixture-image".into(),
            },
            operation: "image.generate".into(),
        })
        .await
        .err()
        .unwrap();
    assert_eq!(refused.to_string(), "Model is unavailable");
    root.shutdown().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

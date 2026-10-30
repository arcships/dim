#![cfg(unix)]
//! DimOAuth through the process's ManagedProviders: login, catalog, leases,
//! recovery and the `auth.json` account record, against a local account
//! service.
#[path = "fixtures/dim_oauth.rs"]
mod oauth;

use dim_agent_capabilities::{file_storage::FileStoragePlugin, host::ModelProtocol};
use dim_agent_runtime::Names;
use dim_app_server::managed_provider::{
    AccountSnapshot, ManagedProviderBinding, ManagedProviders, ManagedProvidersPlugin,
    management::{
        BrowserLoginBegin, LoginBegin, LoginState, ManagedBrowserLoginComplete, ManagedLoginAttempt,
    },
};
use dim_oauth::{ClientIds, DimOAuthPlugin};
use rutis::Ctx;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, FiberState, Key, TypeKey};
use serde_json::{Value, json};
use std::{
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::{Arc, OnceLock, atomic::Ordering},
    time::Duration,
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn device_login_publishes_identity_catalog_usage_and_leases() {
    let fixture = oauth::start().await;
    let home = home();
    let Process { root, providers } = process(&home).await;
    let ids: Vec<_> = providers
        .providers(&cancel())
        .await
        .unwrap()
        .into_iter()
        .map(|p| p.provider_id)
        .collect();
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
    assert_eq!(chat.entry.capabilities.context_window, 32000);
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

    let unavailable = providers
        .credential(PROVIDER, "fixture-image", &cancel())
        .await;
    assert!(unavailable.is_err());
    let lease = providers
        .credential(PROVIDER, "fixture-model", &cancel())
        .await
        .unwrap();
    assert_eq!(lease.bearer, "access-0");
    assert_eq!(lease.base_url, format!("{}/v1", fixture.base));
    assert_eq!(lease.headers["X-Title"], "DimCode");
    assert_eq!(lease.account, account);

    let usage = providers.usage(PROVIDER, &cancel()).await.unwrap();
    assert_eq!(usage.plan.unwrap().name.as_deref(), Some("Fixture plan"));
    let credits = usage.credits.unwrap();
    assert_eq!(credits.total_units, Some(300000));
    assert_eq!(credits.remaining_units, None);
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

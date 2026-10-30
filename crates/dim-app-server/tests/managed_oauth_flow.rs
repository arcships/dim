use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::post,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use dim_agent_capabilities::{
    file_storage::FileStoragePlugin,
    host::{
        AuxiliaryErrorCode, AuxiliaryModelHost, AuxiliaryRequest, CredentialRecoveryPhase,
        CredentialRejection, HostError, HostErrorKind, ModelChunk, ModelHost,
        ProviderConnectionResolver,
    },
};
use dim_agent_runtime::Names;
use dim_agent_types::{Message, ModelId, ModelRequest, Role};
use dim_app_server::{
    codex_oauth::{
        AttemptStatus, BrowserBegin, OAuthError, OpenAICodexOAuthConfig, OpenAICodexOAuthPlugin,
        OpenAICodexOAuthService,
    },
    grok_oauth::{GrokOAuthPlugin, GrokOAuthService},
    provider::{ModelPreferences, ProviderPlugin, ProviderService},
};
use futures_util::StreamExt;
use rutis::Ctx;
use rutis::host::{CtxHost, FiberState, FiberView};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use url::Url;

#[derive(Clone)]
struct Fixture {
    base: String,
    refreshes: Arc<AtomicUsize>,
    polls: Arc<AtomicUsize>,
    device_requests: Arc<AtomicUsize>,
    block_refresh: Arc<AtomicBool>,
    started: Arc<Notify>,
    release: Arc<Notify>,
    rejection: Arc<Mutex<Option<(StatusCode, &'static str)>>>,
    device_response: Arc<Mutex<Option<Value>>>,
    model_requests: Arc<Mutex<Vec<(String, HeaderMap, Value)>>>,
    model_rejections: Arc<AtomicUsize>,
}
fn jwt(claims: Value) -> String {
    format!(
        "e30.{}.fixture",
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap())
    )
}
async fn token(State(f): State<Fixture>, body: String) -> Response {
    let fields: HashMap<_, _> = url::form_urlencoded::parse(body.as_bytes())
        .into_owned()
        .collect();
    match fields["grant_type"].as_str() {
        "authorization_code" => {
            assert_eq!(fields["client_id"], "codex-fixture");
            assert_eq!(
                fields["redirect_uri"],
                "http://localhost:1455/auth/callback"
            );
            assert!((43..=128).contains(&fields["code_verifier"].len()));
            let code = &fields["code"];
            if code == "blocked" {
                f.started.notify_one();
                f.release.notified().await;
            }
            let mut value = json!({
                "access_token": jwt(json!({"sub":"not-the-chatgpt-account", "https://api.openai.com/auth":{"chatgpt_account_id":code}})),
                "refresh_token":format!("refresh-{code}"), "token_type":"Bearer", "expires_in":3600
            });
            if code == "missing-refresh" {
                value.as_object_mut().unwrap().remove("refresh_token");
            }
            if code == "overflow" {
                value["expires_in"] = json!(u64::MAX);
            }
            if code == "oversized" {
                value["padding"] = json!("x".repeat(1024 * 1024));
            }
            if code == "ts-claims" {
                value["access_token"] = json!(jwt(json!({
                    "https://api.openai.com/auth.chatgpt_account_id":"namespaced-account",
                    "chatgpt_account_id":"lower-priority-account",
                    "https://api.openai.com/auth.plan_type":"pro",
                    "plan_type":"lower-priority-plan", "email":"access@example.test"
                })));
                value["id_token"] = json!(jwt(json!({
                    "name":"Display Name", "preferred_username":"username",
                    "email":"identity@example.test"
                })));
            }
            Json(value).into_response()
        }
        "refresh_token" => {
            let count = f.refreshes.fetch_add(1, Ordering::SeqCst) + 1;
            if f.block_refresh.load(Ordering::SeqCst) {
                f.started.notify_one();
                f.release.notified().await;
            }
            if let Some((status, error)) = *f.rejection.lock().unwrap() {
                return (status, Json(json!({"error":error}))).into_response();
            }
            if fields["refresh_token"] == "refresh-account-change" {
                return Json(json!({
                    "access_token": jwt(json!({"https://api.openai.com/auth":{"chatgpt_account_id":"account-after-refresh"}})),
                    "refresh_token":"refresh-after-account-change", "expires_in":3600,
                    "token_type":"Bearer"
                }))
                .into_response();
            }
            Json(json!({"access_token":format!("rotated-{count}"), "refresh_token":format!("rotated-refresh-{count}"), "expires_in":"3600", "token_type":"Bearer"})).into_response()
        }
        "urn:ietf:params:oauth:grant-type:device_code" => {
            assert_eq!(fields["client_id"], "grok-fixture");
            assert_eq!(fields["device_code"], "device-secret");
            match f.polls.fetch_add(1, Ordering::SeqCst) {
                0 => (StatusCode::BAD_REQUEST, Json(json!({"error":"authorization_pending"}))).into_response(),
                1 => (StatusCode::BAD_REQUEST, Json(json!({"error":"slow_down"}))).into_response(),
                _ => Json(json!({"access_token":jwt(json!({"sub":"grok-account", "email":"grok@example.test", "name":"Grok Name"})), "id_token":jwt(json!({"tier":"super"})), "refresh_token":"grok-refresh", "expires_in":3600})).into_response(),
            }
        }
        other => panic!("unexpected grant: {other}"),
    }
}
async fn device(State(f): State<Fixture>, body: String) -> Json<Value> {
    let fields: HashMap<_, _> = url::form_urlencoded::parse(body.as_bytes())
        .into_owned()
        .collect();
    assert_eq!(fields["client_id"], "grok-fixture");
    assert_eq!(fields["scope"], "openid offline_access");
    assert_eq!(
        fields.len(),
        2,
        "Provider device extensions leaked into Grok"
    );
    f.device_requests.fetch_add(1, Ordering::SeqCst);
    Json(f.device_response.lock().unwrap().clone().unwrap_or_else(|| json!({
        "device_code":"device-secret", "user_code":"USER-42", "verification_uri":format!("{}/activate", f.base), "expires_in":30, "interval":1
    })))
}
async fn fixture() -> (Fixture, tokio::task::JoinHandle<()>, PathBuf) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let f = Fixture {
        base: format!("http://{}", listener.local_addr().unwrap()),
        refreshes: Arc::default(),
        polls: Arc::default(),
        device_requests: Arc::default(),
        block_refresh: Arc::default(),
        started: Arc::default(),
        release: Arc::default(),
        rejection: Arc::default(),
        device_response: Arc::default(),
        model_requests: Arc::default(),
        model_rejections: Arc::default(),
    };
    let app = Router::new()
        .route("/token", post(token))
        .route("/device", post(device))
        .route("/relay/{*path}", post(model))
        .with_state(f.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let home = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target/managed-oauth-flow")
        .join(uuid::Uuid::new_v4().to_string());
    fs::create_dir_all(&home).unwrap();
    (f, server, home)
}
async fn model(
    State(f): State<Fixture>,
    uri: Uri,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    f.model_requests
        .lock()
        .unwrap()
        .push((uri.path().into(), headers, body.clone()));
    if f.model_rejections
        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
        .is_ok()
    {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error":{"message":"rejected","type":"authentication_error"}})),
        )
            .into_response();
    }
    let frames = if uri.path().ends_with("/responses") {
        vec![
            json!({"type":"response.created","response":{"id":"r","model":body["model"],"created_at":1}}),
            json!({"type":"response.output_item.added","output_index":0,"item":{"id":"m","type":"message","role":"assistant","content":[]}}),
            json!({"type":"response.content_part.added","item_id":"m","output_index":0,"content_index":0,"part":{"type":"output_text","text":"","annotations":[]}}),
            json!({"type":"response.output_text.delta","item_id":"m","output_index":0,"content_index":0,"delta":"ok"}),
            json!({"type":"response.output_text.done","item_id":"m","output_index":0,"content_index":0,"text":"ok"}),
            json!({"type":"response.output_item.done","output_index":0,"item":{"id":"m","type":"message","role":"assistant","status":"completed","content":[{"type":"output_text","text":"ok","annotations":[]}]}}),
            json!({"type":"response.completed","response":{"id":"r","model":body["model"],"created_at":1,"status":"completed","usage":{"input_tokens":7,"output_tokens":2,"total_tokens":9}}}),
        ]
    } else {
        vec![
            json!({"id":"r","object":"chat.completion.chunk","created":1,"model":body["model"],"choices":[{"index":0,"delta":{"content":"ok"},"finish_reason":"stop"}]}),
        ]
    };
    let mut body: String = frames
        .into_iter()
        .map(|frame| format!("data: {frame}\n\n"))
        .collect();
    body.push_str("data: [DONE]\n\n");
    ([("content-type", "text/event-stream")], body).into_response()
}
fn config(base: &str, codex: bool) -> OpenAICodexOAuthConfig {
    OpenAICodexOAuthConfig {
        issuer: Url::parse(base).unwrap(),
        client_id: if codex {
            "codex-fixture"
        } else {
            "grok-fixture"
        }
        .into(),
        scope: "openid offline_access".into(),
        authorization: codex.then(|| Url::parse(&format!("{base}/authorize")).unwrap()),
        token: Url::parse(&format!("{base}/token")).unwrap(),
        device: (!codex).then(|| Url::parse(&format!("{base}/device")).unwrap()),
        redirect: codex.then(|| Url::parse("http://localhost:1455/auth/callback").unwrap()),
        relay: Url::parse(&format!("{base}/relay")).unwrap(),
    }
}
async fn active(fiber: &FiberView) {
    let mut watch = fiber.watch();
    tokio::time::timeout(Duration::from_secs(5), async {
        while watch.borrow().state != FiberState::Active {
            assert_ne!(watch.borrow().state, FiberState::Failed, "{watch:?}");
            watch.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
}
async fn codex(
    home: &Path,
    base: &str,
) -> (Ctx, FiberView, FiberView, Arc<OpenAICodexOAuthService>) {
    let root = Ctx::root().unwrap();
    let catalog = dim_app_server::catalog();
    let build = rutis_loader::resolve::Build::default();
    let names = Names::copy(&catalog, &build);
    root.plugin(dim_app_server::methods::ManagementMethodsPlugin::typed(&catalog).unwrap());
    let files = root.plugin(FileStoragePlugin::new(vec![home.to_owned()]));
    let plugin =
        root.plugin(OpenAICodexOAuthPlugin::with_config(&names, 0, config(base, true)).unwrap());
    active(&files).await;
    active(&plugin).await;
    let service = root.get::<OpenAICodexOAuthService>().unwrap();
    (root, files, plugin, service)
}
fn callback(begin: &BrowserBegin, code: &str) -> Url {
    let mut callback = Url::parse("http://localhost:1455/auth/callback").unwrap();
    let state = begin
        .authorization_url
        .query_pairs()
        .find(|(key, _)| key == "state")
        .unwrap()
        .1
        .into_owned();
    callback
        .query_pairs_mut()
        .append_pair("code", code)
        .append_pair("state", &state);
    callback
}
async fn login(service: &OpenAICodexOAuthService, account: &str) {
    let cancel = CancellationToken::new();
    let begin = service.begin_browser(&cancel).await.unwrap();
    service
        .complete_browser(begin.attempt_id, callback(&begin, account), &cancel)
        .await
        .unwrap();
    assert_eq!(
        service.attempt_status(begin.attempt_id).await.unwrap(),
        AttemptStatus::Completed
    );
}
fn read(home: &Path) -> Value {
    serde_json::from_slice(&fs::read(home.join("auth.json")).unwrap()).unwrap()
}
fn write(home: &Path, value: &Value) {
    fs::write(
        home.join("auth.json"),
        serde_json::to_vec_pretty(value).unwrap(),
    )
    .unwrap();
}

/// Login and refresh preserve the existing TS contract; a token response that omits `expires_in`
/// defaults to 3600 seconds.
#[tokio::test]
async fn codex_login_refresh_logout_and_restart_preserve_account_boundaries() {
    let (f, server, home) = fixture().await;
    write(
        &home,
        &json!({"unknown":{"keep":true}, "xaiGrokBuild":{"unrelated":"preserved"}}),
    );
    let (_root, files, plugin, a) = codex(&home, &f.base).await;
    let (_other_root, other_files, other_plugin, b) = codex(&home, &f.base).await;
    let cancel = CancellationToken::new();
    assert!(!a.status(&cancel).await.unwrap().authenticated);

    let stale = a.begin_browser(&cancel).await.unwrap();
    login(&b, "account-first").await;
    b.logout(&cancel).await.unwrap();
    assert!(matches!(
        a.complete_browser(stale.attempt_id, callback(&stale, "stale-account"), &cancel)
            .await,
        Err(OAuthError::AccountChanged)
    ));
    assert!(read(&home).get("openaiCodex").is_none());

    login(&a, "账户甲乙丙丁戊庚").await;
    assert_eq!(
        a.status(&cancel)
            .await
            .unwrap()
            .account_id_preview
            .as_deref(),
        Some("账户甲…丁戊庚")
    );
    let first = a.credential(None, &cancel).await.unwrap();
    assert_eq!(first.account_id.as_deref(), Some("账户甲乙丙丁戊庚"));
    let initial = read(&home);
    let switching = a.begin_browser(&cancel).await.unwrap();
    let (one, two) = tokio::join!(
        a.credential(Some(&first), &cancel),
        b.credential(Some(&first), &cancel)
    );
    let refreshed = one.unwrap();
    assert_eq!(refreshed.bearer(), "rotated-1");
    assert_eq!(two.unwrap().bearer(), "rotated-1");
    assert_eq!(f.refreshes.load(Ordering::SeqCst), 1);
    let after = read(&home);
    assert_eq!(
        initial["openaiCodexAccountState"]["generation"],
        after["openaiCodexAccountState"]["generation"]
    );
    assert_eq!(after["openaiCodexAccountState"]["credentialRevision"], 2);
    a.finish_rejected(&first, &cancel).await.unwrap();
    assert!(
        a.status(&cancel).await.unwrap().authenticated,
        "late rejection deleted rotated grant"
    );

    a.complete_browser(
        switching.attempt_id,
        callback(&switching, "account-second"),
        &cancel,
    )
    .await
    .unwrap();
    assert!(matches!(
        a.credential(Some(&refreshed), &cancel).await,
        Err(OAuthError::AccountChanged)
    ));
    assert_ne!(
        initial["openaiCodexAccountState"]["generation"],
        read(&home)["openaiCodexAccountState"]["generation"]
    );
    let intact = fs::read(home.join("auth.json")).unwrap();
    for code in ["missing-refresh", "overflow", "oversized"] {
        let attempt = a.begin_browser(&cancel).await.unwrap();
        assert!(
            a.complete_browser(attempt.attempt_id, callback(&attempt, code), &cancel)
                .await
                .is_err()
        );
        assert_eq!(
            fs::read(home.join("auth.json")).unwrap(),
            intact,
            "failed login inherited old grant"
        );
    }
    let attempt = a.begin_browser(&cancel).await.unwrap();
    let mut duplicate = callback(&attempt, "never");
    duplicate
        .query_pairs_mut()
        .append_pair("state", "duplicate");
    assert!(matches!(
        a.complete_browser(attempt.attempt_id, duplicate, &cancel)
            .await,
        Err(OAuthError::InvalidCallback(_))
    ));
    a.cancel_attempt(attempt.attempt_id).await.unwrap();
    assert!(
        a.complete_browser(attempt.attempt_id, callback(&attempt, "never"), &cancel)
            .await
            .is_err()
    );
    plugin.dispose().await.unwrap();
    assert!(matches!(a.status(&cancel).await, Err(OAuthError::Closed)));
    assert_eq!(
        b.credential(None, &cancel)
            .await
            .unwrap()
            .account_id
            .as_deref(),
        Some("account-second")
    );
    other_plugin.dispose().await.unwrap();
    other_files.dispose().await.unwrap();
    files.dispose().await.unwrap();
    let (_root, files, plugin, restarted) = codex(&home, &f.base).await;
    assert_eq!(
        restarted
            .credential(None, &cancel)
            .await
            .unwrap()
            .account_id
            .as_deref(),
        Some("account-second")
    );
    let rejected = restarted.credential(None, &cancel).await.unwrap();
    restarted.finish_rejected(&rejected, &cancel).await.unwrap();
    let tombstone = read(&home);
    assert!(tombstone.get("openaiCodex").is_none());
    assert_eq!(
        tombstone["openaiCodexAccountState"]["credentialRevision"],
        0
    );
    assert_eq!(tombstone["unknown"]["keep"], true);
    assert_eq!(tombstone["xaiGrokBuild"]["unrelated"], "preserved");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(home.join("auth.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    plugin.dispose().await.unwrap();
    files.dispose().await.unwrap();
    server.abort();
    fs::remove_dir_all(home).unwrap();
}

#[tokio::test]
async fn recovery_and_refresh_settle_without_the_caller_and_preserve_failures() {
    let (f, server, home) = fixture().await;
    let (_root, files, plugin, service) = codex(&home, &f.base).await;
    let cancel = CancellationToken::new();
    login(&service, "account-original").await;
    let old_lease = service.credential(None, &cancel).await.unwrap();
    let mut legacy = read(&home);
    legacy
        .as_object_mut()
        .unwrap()
        .remove("openaiCodexAccountState");
    legacy["openaiCodex"]["unknownField"] = json!({"keep":42});
    legacy["openaiCodex"]["scope"] = json!("openid offline_access narrower");
    legacy["openaiCodex"]["clientSecret"] = json!("obsolete-fixture-secret");
    write(&home, &legacy);
    assert!(service.status(&cancel).await.unwrap().recovery_required);
    assert!(matches!(
        service.credential(None, &cancel).await,
        Err(OAuthError::RecoveryRequired)
    ));
    service.recover(&cancel).await.unwrap();
    assert!(read(&home)["openaiCodex"].get("clientSecret").is_none());
    assert!(matches!(
        service.credential(Some(&old_lease), &cancel).await,
        Err(OAuthError::AccountChanged)
    ));
    assert_eq!(read(&home)["openaiCodex"]["unknownField"]["keep"], 42);
    assert_eq!(
        read(&home)["openaiCodex"]["scope"],
        "openid offline_access narrower"
    );
    let current = service.credential(None, &cancel).await.unwrap();
    for (status, error) in [
        (StatusCode::BAD_REQUEST, "invalid_client"),
        (StatusCode::TOO_MANY_REQUESTS, "invalid_grant"),
        (StatusCode::BAD_GATEWAY, "invalid_grant"),
    ] {
        *f.rejection.lock().unwrap() = Some((status, error));
        let before = fs::read(home.join("auth.json")).unwrap();
        assert!(matches!(
            service.credential(Some(&current), &cancel).await,
            Err(OAuthError::Transient(_))
        ));
        assert_eq!(fs::read(home.join("auth.json")).unwrap(), before);
    }
    *f.rejection.lock().unwrap() = None;
    f.block_refresh.store(true, Ordering::SeqCst);
    let caller = {
        let service = service.clone();
        tokio::spawn(async move {
            service
                .credential(Some(&current), &CancellationToken::new())
                .await
        })
    };
    tokio::time::timeout(Duration::from_secs(3), f.started.notified())
        .await
        .unwrap();
    caller.abort();
    let disposal = tokio::spawn(async move { plugin.dispose().await });
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert!(
        !disposal.is_finished(),
        "Plugin stopped before settling refresh"
    );
    f.release.notify_one();
    tokio::time::timeout(Duration::from_secs(3), disposal)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(read(&home)["openaiCodex"]["access"], "rotated-5");
    files.dispose().await.unwrap();
    let (_root, files, plugin, service) = codex(&home, &f.base).await;
    f.block_refresh.store(false, Ordering::SeqCst);
    let restored = service.credential(None, &cancel).await.unwrap();
    *f.rejection.lock().unwrap() = Some((StatusCode::BAD_REQUEST, "invalid_grant"));
    assert!(matches!(
        service.credential(Some(&restored), &cancel).await,
        Err(OAuthError::Rejected)
    ));
    assert!(!service.status(&cancel).await.unwrap().authenticated);

    let mut future = read(&home);
    future["openaiCodexAccountState"]["schemaVersion"] = json!(2);
    write(&home, &future);
    let bytes = fs::read(home.join("auth.json")).unwrap();
    assert!(matches!(
        service.logout(&cancel).await,
        Err(OAuthError::UnsupportedSchema)
    ));
    assert!(matches!(
        service.recover(&cancel).await,
        Err(OAuthError::UnsupportedSchema)
    ));
    assert!(matches!(
        service.begin_browser(&cancel).await,
        Err(OAuthError::UnsupportedSchema)
    ));
    assert_eq!(fs::read(home.join("auth.json")).unwrap(), bytes);
    plugin.dispose().await.unwrap();
    files.dispose().await.unwrap();
    server.abort();
    fs::remove_dir_all(home).unwrap();
}

#[tokio::test]
async fn refresh_account_change_commits_rotated_grant_and_invalidates_old_lease() {
    let (f, server, home) = fixture().await;
    let (_root, files, plugin, service) = codex(&home, &f.base).await;
    let cancel = CancellationToken::new();
    login(&service, "account-change").await;
    let old = service.credential(None, &cancel).await.unwrap();
    let before = read(&home);

    assert!(matches!(
        service.credential(Some(&old), &cancel).await,
        Err(OAuthError::AccountChanged)
    ));
    let after = read(&home);
    assert_eq!(
        after["openaiCodex"]["refresh"],
        "refresh-after-account-change"
    );
    assert_eq!(after["openaiCodex"]["accountId"], "account-after-refresh");
    assert_ne!(
        before["openaiCodexAccountState"]["generation"],
        after["openaiCodexAccountState"]["generation"]
    );
    assert!(matches!(
        service.credential(Some(&old), &cancel).await,
        Err(OAuthError::AccountChanged)
    ));
    service.finish_rejected(&old, &cancel).await.unwrap();
    let current = service.credential(None, &cancel).await.unwrap();
    assert_eq!(current.bearer(), after["openaiCodex"]["access"]);
    assert_eq!(
        read(&home)["openaiCodex"]["refresh"],
        "refresh-after-account-change"
    );

    plugin.dispose().await.unwrap();
    files.dispose().await.unwrap();
    server.abort();
    fs::remove_dir_all(home).unwrap();
}

#[tokio::test]
async fn minimal_ts_legacy_records_recover_only_from_fixed_plugin_config() {
    for codex_owner in [true, false] {
        let (f, server, home) = fixture().await;
        let key = if codex_owner {
            "openaiCodex"
        } else {
            "xaiGrokBuild"
        };
        let minimal = json!({
            (key): {
                "type":"oauth", "refresh":"legacy-refresh", "access":"legacy-access",
                "expires":1
            }
        });
        write(&home, &minimal);
        let before = fs::read(home.join("auth.json")).unwrap();
        let root = Ctx::root().unwrap();
        let catalog = dim_app_server::catalog();
        let build = rutis_loader::resolve::Build::default();
        let names = Names::copy(&catalog, &build);
        root.plugin(dim_app_server::methods::ManagementMethodsPlugin::typed(&catalog).unwrap());
        let files = root.plugin(FileStoragePlugin::new(vec![home.clone()]));
        let plugin = if codex_owner {
            root.plugin(
                OpenAICodexOAuthPlugin::with_config(&names, 0, config(&f.base, true)).unwrap(),
            )
        } else {
            root.plugin(GrokOAuthPlugin::with_config(&names, 0, config(&f.base, false)).unwrap())
        };
        active(&files).await;
        active(&plugin).await;
        let cancel = CancellationToken::new();
        if codex_owner {
            let service = root.get::<OpenAICodexOAuthService>().unwrap();
            assert!(service.status(&cancel).await.unwrap().recovery_required);
            assert_eq!(fs::read(home.join("auth.json")).unwrap(), before);
            service.recover(&cancel).await.unwrap();
        } else {
            let service = root.get::<GrokOAuthService>().unwrap();
            assert!(service.status(&cancel).await.unwrap().recovery_required);
            assert_eq!(fs::read(home.join("auth.json")).unwrap(), before);
            service.recover(&cancel).await.unwrap();
        }
        let recovered = read(&home);
        assert_eq!(recovered[key]["issuer"], f.base);
        assert_eq!(recovered[key]["tokenEndpoint"], format!("{}/token", f.base));
        assert_eq!(
            recovered[key]["clientId"],
            if codex_owner {
                "codex-fixture"
            } else {
                "grok-fixture"
            }
        );
        assert_eq!(recovered[key]["scope"], "openid offline_access");
        assert_eq!(recovered[key]["relayBaseUrl"], format!("{}/relay", f.base));

        let mut incomplete_managed = recovered.clone();
        incomplete_managed[key]
            .as_object_mut()
            .unwrap()
            .remove("issuer");
        write(&home, &incomplete_managed);
        let result = if codex_owner {
            root.get::<OpenAICodexOAuthService>()
                .unwrap()
                .recover(&cancel)
                .await
        } else {
            root.get::<GrokOAuthService>()
                .unwrap()
                .recover(&cancel)
                .await
        };
        assert!(matches!(result, Err(OAuthError::InvalidStore(_))));
        assert_eq!(read(&home), incomplete_managed);

        let mut untrusted = minimal.clone();
        untrusted[key]["tokenEndpoint"] = json!("https://attacker.example/token");
        write(&home, &untrusted);
        let result = if codex_owner {
            root.get::<OpenAICodexOAuthService>()
                .unwrap()
                .recover(&cancel)
                .await
        } else {
            root.get::<GrokOAuthService>()
                .unwrap()
                .recover(&cancel)
                .await
        };
        assert!(
            matches!(result, Err(OAuthError::InvalidStore(_))),
            "a production issuer must not be combined with a local or attacker-controlled token endpoint"
        );
        assert_eq!(read(&home), untrusted);

        let mut explicit_null = minimal.clone();
        explicit_null[key]["issuer"] = Value::Null;
        write(&home, &explicit_null);
        let result = if codex_owner {
            root.get::<OpenAICodexOAuthService>()
                .unwrap()
                .recover(&cancel)
                .await
        } else {
            root.get::<GrokOAuthService>()
                .unwrap()
                .recover(&cancel)
                .await
        };
        assert!(matches!(result, Err(OAuthError::InvalidStore(_))));
        assert_eq!(read(&home), explicit_null);

        plugin.dispose().await.unwrap();
        files.dispose().await.unwrap();
        server.abort();
        fs::remove_dir_all(home).unwrap();
    }
}

#[tokio::test]
async fn browser_cancellation_and_external_record_changes_do_not_commit_stale_identity() {
    let (f, server, home) = fixture().await;
    let (_root, files, plugin, service) = codex(&home, &f.base).await;
    let cancel = CancellationToken::new();
    login(&service, "original").await;
    let original = read(&home);
    let begin = service.begin_browser(&cancel).await.unwrap();
    let id = begin.attempt_id;
    let completing = {
        let service = service.clone();
        tokio::spawn(async move {
            service
                .complete_browser(id, callback(&begin, "blocked"), &CancellationToken::new())
                .await
        })
    };
    tokio::time::timeout(Duration::from_secs(3), f.started.notified())
        .await
        .unwrap();
    service.cancel_attempt(id).await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(1), completing)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
    f.release.notify_one();
    assert_eq!(
        service.attempt_status(id).await.unwrap(),
        AttemptStatus::Cancelled
    );
    assert_eq!(read(&home), original);

    let old = service.credential(None, &cancel).await.unwrap();
    let mut changed = original.clone();
    changed["openaiCodex"]["access"] = json!("outside-token");
    write(&home, &changed);
    assert!(service.status(&cancel).await.unwrap().recovery_required);
    assert!(matches!(
        service.credential(None, &cancel).await,
        Err(OAuthError::RecoveryRequired)
    ));
    assert!(matches!(
        service.finish_rejected(&old, &cancel).await,
        Err(OAuthError::RecoveryRequired)
    ));
    assert_eq!(read(&home), changed);
    service.recover(&cancel).await.unwrap();
    assert_ne!(
        read(&home)["openaiCodexAccountState"]["generation"],
        original["openaiCodexAccountState"]["generation"]
    );
    assert!(matches!(
        service.credential(Some(&old), &cancel).await,
        Err(OAuthError::AccountChanged)
    ));

    let mut untrusted = read(&home);
    untrusted["openaiCodex"]["tokenEndpoint"] = json!("https://auth.x.ai/oauth2/token");
    write(&home, &untrusted);
    assert!(matches!(
        service.recover(&cancel).await,
        Err(OAuthError::InvalidStore(_))
    ));
    assert_eq!(read(&home), untrusted);
    assert_eq!(f.refreshes.load(Ordering::SeqCst), 1);
    plugin.dispose().await.unwrap();
    files.dispose().await.unwrap();
    server.abort();
    fs::remove_dir_all(home).unwrap();
}

#[tokio::test]
async fn grok_device_flow_observes_slowdown_expiry_cancellation_and_uri_trust() {
    let (f, server, home) = fixture().await;
    let root = Ctx::root().unwrap();
    let catalog = dim_app_server::catalog();
    let build = rutis_loader::resolve::Build::default();
    let names = Names::copy(&catalog, &build);
    root.plugin(dim_app_server::methods::ManagementMethodsPlugin::typed(&catalog).unwrap());
    let files = root.plugin(FileStoragePlugin::new(vec![home.clone()]));
    let plugin =
        root.plugin(GrokOAuthPlugin::with_config(&names, 0, config(&f.base, false)).unwrap());
    active(&files).await;
    active(&plugin).await;
    let grok = root.get::<GrokOAuthService>().unwrap();
    let cancel = CancellationToken::new();
    let started = std::time::Instant::now();
    let begin = grok.begin_device(&cancel).await.unwrap();
    assert_eq!(begin.user_code, "USER-42");
    assert_eq!(
        begin.verification_uri.as_str(),
        format!("{}/activate", f.base)
    );
    tokio::time::timeout(Duration::from_secs(12), async {
        loop {
            match grok.attempt_status(begin.attempt_id).await.unwrap() {
                AttemptStatus::Pending => tokio::time::sleep(Duration::from_millis(20)).await,
                AttemptStatus::Completed => break,
                state => panic!("device flow: {state:?}"),
            }
        }
    })
    .await
    .unwrap();
    assert!(
        started.elapsed() >= Duration::from_secs(8),
        "slow_down was ignored"
    );
    assert_eq!(
        grok.credential(None, &cancel)
            .await
            .unwrap()
            .account_id
            .as_deref(),
        Some("grok-account")
    );
    let record = read(&home);
    assert_eq!(record["xaiGrokBuild"]["accountLabel"], "grok@example.test");
    assert_eq!(record["xaiGrokBuild"]["email"], "grok@example.test");
    assert_eq!(record["xaiGrokBuild"]["planType"], "super");
    let successful = fs::read(home.join("auth.json")).unwrap();
    let cancelled = grok.begin_device(&cancel).await.unwrap();
    grok.cancel_attempt(cancelled.attempt_id).await.unwrap();
    assert_eq!(
        grok.attempt_status(cancelled.attempt_id).await.unwrap(),
        AttemptStatus::Cancelled
    );
    let response = json!({"device_code":"device-secret", "user_code":"USER-42", "verification_uri":format!("{}/activate", f.base), "expires_in":1, "interval":2});
    *f.device_response.lock().unwrap() = Some(response.clone());
    let expired = grok.begin_device(&cancel).await.unwrap();
    tokio::time::sleep(Duration::from_millis(1100)).await;
    assert_eq!(
        grok.attempt_status(expired.attempt_id).await.unwrap(),
        AttemptStatus::Expired
    );
    assert_eq!(
        f.polls.load(Ordering::SeqCst),
        3,
        "expired/cancelled attempt sent a token request"
    );
    for uri in [
        "https://auth.openai.com/activate",
        "http://127.0.0.1:1/activate",
        "https://evil.example/activate",
    ] {
        let mut unsafe_response = response.clone();
        unsafe_response["verification_uri"] = json!(uri);
        *f.device_response.lock().unwrap() = Some(unsafe_response);
        assert!(matches!(
            grok.begin_device(&cancel).await,
            Err(OAuthError::InvalidResponse(_))
        ));
    }
    assert_eq!(fs::read(home.join("auth.json")).unwrap(), successful);
    *f.device_response.lock().unwrap() = None;
    grok.begin_device(&cancel).await.unwrap();
    plugin.dispose().await.unwrap();
    assert!(matches!(
        grok.status(&cancel).await,
        Err(OAuthError::Closed)
    ));
    files.dispose().await.unwrap();
    server.abort();
    fs::remove_dir_all(home).unwrap();
}

#[tokio::test]
async fn managed_provider_selection_http_recovery_and_restart_flow() {
    let (f, server, home) = fixture().await;
    let catalog = dim_app_server::catalog();
    let build = rutis_loader::resolve::Build::default();
    let names = Names::copy(&catalog, &build);
    let (root, _, _, codex) = codex(&home, &f.base).await;
    let grok_plugin =
        root.plugin(GrokOAuthPlugin::with_config(&names, 0, config(&f.base, false)).unwrap());
    let dim = root.plugin(dim_app_server::managed_provider::ManagedProvidersPlugin);
    let sqlite = root.plugin(dim_app_server::storage::SqliteStoragePlugin::new(
        home.join("native.sqlite"),
    ));
    let provider_plugin = root.plugin(ProviderPlugin::new(&names).unwrap());
    let aimux = root.plugin(dim_agent_aimux::AimuxPlugin);
    for plugin in [&grok_plugin, &dim, &sqlite, &provider_plugin, &aimux] {
        active(plugin).await;
    }
    let provider = root.get::<ProviderService>().unwrap();
    let resolver = root.get::<ProviderConnectionResolver>().unwrap();
    let host = root.get::<ModelHost>().unwrap();
    let grok = root.get::<GrokOAuthService>().unwrap();
    let cancel = CancellationToken::new();
    for id in ["openai-codex", "grok-build"] {
        let catalog = provider
            .catalog_for(id.into(), &cancel)
            .await
            .unwrap()
            .unwrap();
        assert!(!catalog.models.is_empty());
        assert!(
            catalog
                .models
                .iter()
                .all(|m| m.unavailable_reason.is_some())
        );
    }
    login(&codex, "account-a").await;
    f.polls.store(2, Ordering::SeqCst);
    let begin = grok.begin_device(&cancel).await.unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        while grok.attempt_status(begin.attempt_id).await.unwrap() == AttemptStatus::Pending {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        grok.attempt_status(begin.attempt_id).await.unwrap(),
        AttemptStatus::Completed
    );

    for (index, (id, model_id)) in [
        ("openai-codex", "gpt-5.5"),
        ("grok-build", "grok-build-0.1"),
    ]
    .into_iter()
    .enumerate()
    {
        let target = ModelId {
            provider_id: id.into(),
            model_id: model_id.into(),
        };
        let summary = provider
            .list(&cancel)
            .await
            .unwrap()
            .providers
            .into_iter()
            .find(|p| p.provider_id == id)
            .unwrap();
        let mut network_input =
            json!({"network":{"timeoutMs":{"action":"set","value":6000 + index}}});
        provider
            .update(
                id.into(),
                summary.version,
                serde_json::from_value(network_input.clone()).unwrap(),
                &cancel,
            )
            .await
            .unwrap();
        let catalog = provider
            .catalog_for(id.into(), &cancel)
            .await
            .unwrap()
            .unwrap();
        assert!(
            catalog
                .models
                .iter()
                .find(|m| m.id == model_id)
                .unwrap()
                .unavailable_reason
                .is_none()
        );
        assert!(
            catalog
                .models
                .iter()
                .filter(|m| m.id.contains("image") || m.id.contains("video"))
                .all(|m| m.unavailable_reason.is_some())
        );
        let edited = provider
            .update_model_config(
                id.into(),
                catalog.version,
                model_id.into(),
                Some(serde_json::from_value(json!({"maxOutputTokens":4321})).unwrap()),
                &cancel,
            )
            .await
            .unwrap();
        let selected = provider
            .select(
                None,
                target.clone(),
                if index == 0 { None } else { Some(1) },
                &cancel,
            )
            .await
            .unwrap();
        assert_eq!(selected.provider_id, id);
        let config = provider
            .configuration(None, None, ModelPreferences::default(), &cancel)
            .await
            .unwrap();
        assert_eq!(config.provider_id, id);
        assert_eq!(config.model_id, model_id);
        assert_eq!(config.config.as_ref().unwrap().max_tokens, Some(4321.0));
        let mut request = ModelRequest::new(id, model_id, vec![Message::text(Role::User, "hello")]);
        request.config = config.config;
        request.capability_snapshot = config.capability_snapshot;
        let request = Arc::new(request);
        let resolved = resolver
            .0
            .resolve(request.clone(), cancel.clone())
            .await
            .unwrap();
        let rejected = resolved.recovery.unwrap();
        f.model_rejections.store(1, Ordering::SeqCst);
        let error = host
            .0
            .invoke(request.clone(), resolved.connection, cancel.clone())
            .await
            .err()
            .unwrap();
        assert!(matches!(
            &error,
            HostError::Failed {
                kind: HostErrorKind::Http {
                    status: Some(401),
                    ..
                },
                ..
            }
        ));
        assert_eq!(
            rejected.rejection(&error, CredentialRecoveryPhase::Initial),
            CredentialRejection::Recoverable
        );
        network_input["network"] = json!({"timeoutMs":{"action":"set","value":9000 + index}});
        provider
            .update(
                id.into(),
                edited.version,
                serde_json::from_value(network_input).unwrap(),
                &cancel,
            )
            .await
            .unwrap();
        let (first, concurrent) = tokio::join!(
            rejected.recover(cancel.clone()),
            rejected.recover(cancel.clone())
        );
        let recovered = first.unwrap();
        concurrent.unwrap();
        assert_eq!(f.refreshes.load(Ordering::SeqCst), index + 1);
        assert!(
            matches!(&recovered.connection, dim_agent_capabilities::host::ModelConnection::Http(connection) if connection.network.timeout_ms == Some(6000 + index as u64))
        );
        let fresh = resolver
            .0
            .resolve(request.clone(), cancel.clone())
            .await
            .unwrap();
        assert!(
            matches!(fresh.connection, dim_agent_capabilities::host::ModelConnection::Http(connection) if connection.network.timeout_ms == Some(9000 + index as u64))
        );
        let latest = recovered.recovery;
        assert_eq!(
            latest.rejection(&error, CredentialRecoveryPhase::Recovered),
            CredentialRejection::Terminal
        );
        let mut stream = host
            .0
            .invoke(request.clone(), recovered.connection, cancel.clone())
            .await
            .unwrap();
        let mut text = String::new();
        let mut finished = false;
        while let Some(chunk) = stream.next().await {
            match chunk.unwrap() {
                ModelChunk::Text(delta) => text.push_str(&delta),
                ModelChunk::MessageStop { .. } => finished = true,
                _ => {}
            }
        }
        assert_eq!(text, "ok");
        assert!(finished);
        {
            let requests = f.model_requests.lock().unwrap();
            assert_eq!(requests.len(), (index + 1) * 2);
            let (path, headers, body) = &requests[index * 2 + 1];
            assert_eq!(
                headers["authorization"],
                format!("Bearer rotated-{}", index + 1)
            );
            assert_eq!(body["model"], model_id);
            assert_eq!(
                body,
                &requests[index * 2].2,
                "recovery changed the adopted request"
            );
            if index == 0 {
                assert_eq!(path, "/relay/responses");
                assert_eq!(headers["chatgpt-account-id"], "account-a");
                assert_eq!(headers["oai-product-sku"], "codex");
                assert_eq!(headers["originator"], "codex-tui");
                assert_eq!(
                    headers["user-agent"],
                    "codex-tui/0.144.1 (Ubuntu 22.4.0; x86_64) xterm-256color (codex-tui; 0.144.1)"
                );
                assert_eq!(body["store"], false);
                assert!(body.get("max_output_tokens").is_none());
            } else {
                assert_eq!(path, "/relay/chat/completions");
                assert!(!headers.contains_key("chatgpt-account-id"));
                assert!(!headers.contains_key("oai-product-sku"));
                assert_eq!(body["max_tokens"], 4321);
            }
        }
        rejected.finish_rejected(cancel.clone()).await.unwrap();
        assert!(
            provider
                .list(&cancel)
                .await
                .unwrap()
                .providers
                .iter()
                .find(|p| p.provider_id == id)
                .unwrap()
                .credential_configured
        );
        if index == 0 {
            login(&codex, "account-b").await;
            assert!(latest.recover(cancel.clone()).await.is_err());
            latest.finish_rejected(cancel.clone()).await.unwrap();
            assert_eq!(
                codex
                    .credential(None, &cancel)
                    .await
                    .unwrap()
                    .account_id
                    .as_deref(),
                Some("account-b")
            );
        } else {
            latest.finish_rejected(cancel.clone()).await.unwrap();
            assert!(!grok.status(&cancel).await.unwrap().authenticated);
            assert!(
                resolver
                    .0
                    .resolve(request.clone(), cancel.clone())
                    .await
                    .is_err()
            );
            assert!(
                provider
                    .configuration(Some(target), None, ModelPreferences::default(), &cancel)
                    .await
                    .is_err()
            );
        }

        let summary = provider
            .list(&cancel)
            .await
            .unwrap()
            .providers
            .into_iter()
            .find(|p| p.provider_id == id)
            .unwrap();
        let mut input = json!({});
        input["credential"] = json!({"action":"set","value":"must-not-be-stored"});
        assert!(
            provider
                .update(
                    id.into(),
                    summary.version,
                    serde_json::from_value(input.clone()).unwrap(),
                    &cancel
                )
                .await
                .is_err()
        );
        input.as_object_mut().unwrap().remove("credential");
        input["baseUrl"] = json!("https://outside.test");
        assert!(
            provider
                .update(
                    id.into(),
                    summary.version,
                    serde_json::from_value(input).unwrap(),
                    &cancel
                )
                .await
                .is_err()
        );
    }

    f.polls.store(2, Ordering::SeqCst);
    let begin = grok.begin_device(&cancel).await.unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        while grok.attempt_status(begin.attempt_id).await.unwrap() == AttemptStatus::Pending {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(grok.status(&cancel).await.unwrap().authenticated);
    let auxiliary = root.get::<AuxiliaryModelHost>().unwrap();
    let sent = f.model_requests.lock().unwrap().len();
    f.model_rejections.store(2, Ordering::SeqCst);
    let error = auxiliary
        .0
        .complete(
            ModelId {
                provider_id: "grok-build".into(),
                model_id: "grok-build-0.1".into(),
            },
            AuxiliaryRequest {
                system: None,
                messages: vec![Message::text(Role::User, "hello")],
                max_output_tokens: None,
            },
            cancel.clone(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, AuxiliaryErrorCode::Failed);
    assert_eq!(f.model_requests.lock().unwrap().len(), sent + 2);
    assert!(!grok.status(&cancel).await.unwrap().authenticated);

    let before = read(&home);
    let list = provider.list(&cancel).await.unwrap();
    let summary = list
        .providers
        .iter()
        .find(|p| p.provider_id == "openai-codex")
        .unwrap();
    provider
        .delete_static("openai-codex".into(), summary.version, &cancel)
        .await
        .unwrap();
    assert_eq!(
        read(&home),
        before,
        "disabling a provider must not log out its account"
    );
    assert!(
        provider
            .configuration(None, None, ModelPreferences::default(), &cancel)
            .await
            .is_err()
    );
    let updated = provider
        .update(
            "openai-codex".into(),
            summary.version + 1,
            serde_json::from_value(json!({"enabled":true})).unwrap(),
            &cancel,
        )
        .await
        .unwrap();
    assert!(updated.connection.unwrap().credential_configured);
    let old_request = {
        let selection = provider
            .configuration(
                Some(ModelId {
                    provider_id: "openai-codex".into(),
                    model_id: "gpt-5.5".into(),
                }),
                None,
                ModelPreferences::default(),
                &cancel,
            )
            .await
            .unwrap();
        let mut request = ModelRequest::new("openai-codex", "gpt-5.5", vec![]);
        request.capability_snapshot = selection.capability_snapshot;
        Arc::new(request)
    };
    let old = resolver
        .0
        .resolve(old_request.clone(), cancel.clone())
        .await
        .unwrap()
        .recovery
        .unwrap();
    root.shutdown().await.unwrap();
    assert!(matches!(
        old.recover(cancel.clone()).await,
        Err(HostError::Failed {
            kind: HostErrorKind::OwnerClosed,
            ..
        })
    ));
    let db = rusqlite::Connection::open(home.join("native.sqlite")).unwrap();
    let record: String = db
        .query_row(
            "SELECT record FROM providers WHERE provider_id='openai-codex'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let record: Value = serde_json::from_str(&record).unwrap();
    assert_eq!(record["credentialAuthority"], "openai_codex");
    assert!(record["credential"].is_null());
    assert!(!record.to_string().contains("rotated-"));
    drop(db);
    let (restarted, _, _, _) = self::codex(&home, &f.base).await;
    let plugins = [
        restarted.plugin(GrokOAuthPlugin::with_config(&names, 0, config(&f.base, false)).unwrap()),
        restarted.plugin(dim_app_server::managed_provider::ManagedProvidersPlugin),
        restarted.plugin(dim_app_server::storage::SqliteStoragePlugin::new(
            home.join("native.sqlite"),
        )),
        restarted.plugin(ProviderPlugin::new(&names).unwrap()),
        restarted.plugin(dim_agent_aimux::AimuxPlugin),
    ];
    for plugin in &plugins {
        active(plugin).await;
    }
    let provider = restarted.get::<ProviderService>().unwrap();
    let selected = provider
        .configuration(None, None, ModelPreferences::default(), &cancel)
        .await
        .unwrap();
    assert_eq!(selected.provider_id, "openai-codex");
    assert_eq!(
        provider
            .selection(None, &cancel)
            .await
            .unwrap()
            .unwrap()
            .provider_id,
        "grok-build"
    );
    let catalog = provider
        .catalog_for("openai-codex".into(), &cancel)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        catalog
            .models
            .iter()
            .find(|m| m.id == "gpt-5.5")
            .unwrap()
            .config
            .max_tokens,
        Some(4321.0)
    );
    restarted
        .get::<ProviderConnectionResolver>()
        .unwrap()
        .0
        .resolve(old_request, cancel)
        .await
        .unwrap();
    restarted.shutdown().await.unwrap();
    server.abort();
    fs::remove_dir_all(home).unwrap();
}

#[tokio::test]
async fn login_retains_ts_claim_precedence_and_bounds_pending_attempts() {
    let (f, server, home) = fixture().await;
    let catalog = dim_app_server::catalog();
    let build = rutis_loader::resolve::Build::default();
    let names = Names::copy(&catalog, &build);
    let (root, files, plugin, service) = codex(&home, &f.base).await;
    let cancel = CancellationToken::new();
    login(&service, "ts-claims").await;
    let record = read(&home);
    assert_eq!(record["openaiCodex"]["accountId"], "namespaced-account");
    assert_eq!(record["openaiCodex"]["accountLabel"], "Display Name");
    assert_eq!(record["openaiCodex"]["planType"], "pro");
    assert_eq!(record["openaiCodex"]["email"], "identity@example.test");
    let mut attempts = Vec::new();
    for _ in 0..32 {
        attempts.push(service.begin_browser(&cancel).await.unwrap());
    }
    assert!(service.begin_browser(&cancel).await.is_err());
    service
        .cancel_attempt(attempts[0].attempt_id)
        .await
        .unwrap();
    service.begin_browser(&cancel).await.unwrap();

    let grok_plugin =
        root.plugin(GrokOAuthPlugin::with_config(&names, 0, config(&f.base, false)).unwrap());
    active(&grok_plugin).await;
    let grok = root.get::<GrokOAuthService>().unwrap();
    *f.device_response.lock().unwrap() = Some(json!({
        "device_code":"device-secret", "user_code":"USER-42",
        "verification_uri":format!("{}/activate", f.base),
        "expires_in":7200, "interval":3600
    }));
    let mut attempts = Vec::new();
    for _ in 0..32 {
        attempts.push(grok.begin_device(&cancel).await.unwrap());
    }
    assert!(grok.begin_device(&cancel).await.is_err());
    assert_eq!(f.device_requests.load(Ordering::SeqCst), 32);
    grok.cancel_attempt(attempts[0].attempt_id).await.unwrap();
    grok.begin_device(&cancel).await.unwrap();
    assert_eq!(f.device_requests.load(Ordering::SeqCst), 33);
    grok_plugin.dispose().await.unwrap();
    plugin.dispose().await.unwrap();
    files.dispose().await.unwrap();
    server.abort();
    fs::remove_dir_all(home).unwrap();
}

use super::*;
use axum::{
    Json, Router,
    body::Bytes,
    extract::State,
    http::HeaderMap,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use dim_agent_capabilities::file_storage::FileStoragePlugin;
use rutis::host::CtxHost;
use serde_json::{Value, json};

#[derive(Clone)]
struct OpenAiFixture {
    issuer: String,
    nonce: Arc<Mutex<String>>,
    requests: Arc<Mutex<Vec<HashMap<String, String>>>>,
}

impl OpenAiFixture {
    fn signature(&self, client_id: &str) -> String {
        let fixture: Value = serde_json::from_str(include_str!("test-signing-key.json")).unwrap();
        let key = ring::signature::RsaKeyPair::from_pkcs8(
            &URL_SAFE_NO_PAD
                .decode(fixture["signing"].as_str().unwrap())
                .unwrap(),
        )
        .unwrap();
        let part = |value: Value| URL_SAFE_NO_PAD.encode(value.to_string());
        let header = part(json!({"alg":"RS256","kid":fixture["public"]["kid"]}));
        let identity = part(json!({
            "iss":self.issuer,"sub":"test-subject","aud":client_id,
            "nonce":*self.nonce.lock().unwrap(),"exp":now()+3600,"email":"user@example.invalid"
        }));
        let signed = format!("{header}.{identity}");
        let mut buffer = vec![0; key.public().modulus_len()];
        key.sign(
            &ring::signature::RSA_PKCS1_SHA256,
            &ring::rand::SystemRandom::new(),
            signed.as_bytes(),
            &mut buffer,
        )
        .unwrap();
        format!("{signed}.{}", URL_SAFE_NO_PAD.encode(buffer))
    }
}

async fn tokens(State(state): State<OpenAiFixture>, body: Bytes) -> Response {
    let params: HashMap<String, String> = url::form_urlencoded::parse(&body).into_owned().collect();
    assert_eq!(params["resource"], format!("{}/v1", state.issuer));
    assert!(!params.contains_key("client_secret"));
    let client_id = &params["client_id"];
    assert_eq!(client_id, "oaiapp_fixture");
    let output = match params["grant_type"].as_str() {
        "authorization_code" => {
            assert_eq!(params["code"], "fixture-code");
            assert_eq!(params["code_verifier"].len(), 43);
            json!({"access_token":"first-access","refresh_token":"first-refresh",
                "id_token":state.signature(client_id),"scope":SCOPES,"expires_in":3600,"token_type":"Bearer"})
        }
        "refresh_token" => {
            assert_eq!(params["refresh_token"], "first-refresh");
            assert!(!params.contains_key("scope"));
            json!({"access_token":"rotated-access","refresh_token":"rotated-refresh",
                "scope":SCOPES,"expires_in":3600,"token_type":"Bearer"})
        }
        _ => panic!("unexpected grant"),
    };
    state.requests.lock().unwrap().push(params);
    Json(output).into_response()
}

#[tokio::test]
async fn chatgpt_plan_registration_rotation_restart_and_revocation() {
    registration(false).await;
    registration(true).await;
}

async fn registration(migrate_legacy: bool) {
    let home = tempfile::tempdir().unwrap();
    let root = rutis::Ctx::root().unwrap();
    root.plugin(FileStoragePlugin::new(vec![
        home.path().canonicalize().unwrap(),
    ]))
    .await
    .unwrap();
    let files = root.get::<FileStorage>().unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let fixture = OpenAiFixture {
        issuer: format!("http://{}", listener.local_addr().unwrap()),
        nonce: Default::default(),
        requests: Default::default(),
    };
    let endpoints = Router::new()
        .route(
            "/.well-known/openid-configuration",
            get(|State(state): State<OpenAiFixture>| async move {
                Json(
                    json!({"issuer": state.issuer, "jwks_uri": format!("{}/keys", state.issuer),
                "revocation_endpoint": format!("{}/revoke", state.issuer)}),
                )
            }),
        )
        .route(
            "/keys",
            get(|| async {
                let key: Value =
                    serde_json::from_str(include_str!("test-signing-key.json")).unwrap();
                Json(json!({"keys":[key["public"]]}))
            }),
        )
        .route("/api/accounts/oauth/token", post(tokens))
        .route(
            "/v1/models",
            get(|headers: HeaderMap| async move {
                assert_eq!(headers["authorization"], "Bearer rotated-access");
                Json(json!({"models":[
                    {"slug":"gpt-6.2-sol","display_name":"Fixture Sol","visibility":"list"},
                    {"slug":"hidden-model","display_name":"Hidden","visibility":"hidden"},
                    {"slug":"gpt-6.1-sol","display_name":"Fixture 6.1","visibility":"list"}
                ]}))
            }),
        )
        .route(
            "/revoke",
            post(|body: Bytes| async move {
                let params: HashMap<String, String> =
                    url::form_urlencoded::parse(&body).into_owned().collect();
                assert_eq!(params["token"], "rotated-refresh");
                assert_eq!(params["token_type_hint"], "refresh_token");
                assert_eq!(params["client_id"], "oaiapp_fixture");
                StatusCode::OK
            }),
        )
        .with_state(fixture.clone());
    let serve = tokio::spawn(async move { axum::serve(listener, endpoints).await.unwrap() });
    let make_owner = || {
        let mut owner = ChatGpt::new(files.clone(), 0).unwrap();
        let config = Arc::get_mut(&mut owner).unwrap();
        config.issuer = Url::parse(&fixture.issuer).unwrap();
        config.api = config.issuer.join("/v1/").unwrap();
        owner
    };
    let owner = make_owner();
    let stop = CancellationToken::new();
    let redirect = "http://127.0.0.1:19455/auth/callback";
    assert!(owner.status(&stop).await.unwrap().account.is_none());
    assert!(!home.path().join("chatgpt-account.json").exists());
    assert!(matches!(
        owner
            .begin("http://localhost:19455/auth/callback", None, false, &stop)
            .await,
        Err(ManagedError::InvalidInput)
    ));
    let BeginReply::Browser(cancelled) = owner.begin(redirect, None, true, &stop).await.unwrap()
    else {
        panic!("expected a browser attempt for a signed-out account");
    };
    let cancelled = Uuid::parse_str(&cancelled.attempt_id).unwrap();
    owner.cancel(cancelled).unwrap();
    assert!(matches!(
        owner.complete(cancelled, redirect).await,
        Err(ManagedError::AttemptClosed)
    ));
    assert!(!owner.legacy_codex_hidden(&stop).await.unwrap());
    let BeginReply::Browser(sign_in) = owner
        .begin(redirect, None, migrate_legacy, &stop)
        .await
        .unwrap()
    else {
        panic!("expected a browser attempt for a signed-out account");
    };
    let authorization: HashMap<_, _> = Url::parse(&sign_in.authorization_url)
        .unwrap()
        .query_pairs()
        .into_owned()
        .collect();
    let id = Uuid::parse_str(&sign_in.attempt_id).unwrap();
    assert_eq!(authorization["client_id"], REGISTER);
    assert_eq!(authorization["agent_name_hint"], "Dim");
    assert_eq!(authorization["redirect_uri"], redirect);
    assert_eq!(authorization["scope"], SCOPES);
    assert_eq!(authorization["code_challenge_method"], "S256");
    assert!(authorization["ext_agent_host_id"].starts_with("urn:uuid:"));
    assert!(matches!(
        owner
            .complete(
                id,
                &format!("{redirect}?state=wrong&code=fixture-code&client_id=oaiapp_fixture")
            )
            .await,
        Err(ManagedError::InvalidCallback)
    ));
    assert!(fixture.requests.lock().unwrap().is_empty());
    assert!(!owner.legacy_codex_hidden(&stop).await.unwrap());
    *fixture.nonce.lock().unwrap() = authorization["nonce"].clone();
    let snapshot = owner
        .complete(
            id,
            &format!(
                "{redirect}?state={}&code=fixture-code&client_id=oaiapp_fixture",
                authorization["state"]
            ),
        )
        .await
        .unwrap();
    assert_eq!(
        owner.legacy_codex_hidden(&stop).await.unwrap(),
        migrate_legacy
    );
    if !migrate_legacy {
        assert!(matches!(
            owner.begin(redirect, None, true, &stop).await.unwrap(),
            BeginReply::Migrated { migrated: true }
        ));
        assert!(owner.legacy_codex_hidden(&stop).await.unwrap());
    }
    let first = owner.credential(None, &stop).await.unwrap();
    assert_eq!(first.account, snapshot);
    assert_eq!(first.bearer, "first-access");
    assert_eq!(
        URL_SAFE_NO_PAD.encode(Sha256::digest(
            fixture.requests.lock().unwrap()[0]["code_verifier"].as_bytes()
        )),
        authorization["code_challenge"]
    );
    let (renewed, joined) = tokio::join!(
        owner.credential(Some((&first.account, &first.bearer)), &stop),
        owner.credential(Some((&first.account, &first.bearer)), &stop),
    );
    assert_eq!(renewed.unwrap().bearer, "rotated-access");
    assert_eq!(joined.unwrap().bearer, "rotated-access");
    assert_eq!(
        fixture.requests.lock().unwrap().len(),
        2,
        "one refresh rotates the grant"
    );
    owner.close().await;
    let owner = make_owner();
    assert!(owner.legacy_codex_hidden(&stop).await.unwrap());
    let catalog = owner.catalog(&stop).await.unwrap();
    assert_eq!(catalog.account, snapshot);
    let models = catalog
        .models
        .iter()
        .map(|m| m.entry.model_id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(models, ["gpt-6.2-sol", "gpt-6.1-sol"]);
    assert!(owner.logout(&stop).await.unwrap());
    assert!(owner.status(&stop).await.unwrap().account.is_none());
    assert!(owner.legacy_codex_hidden(&stop).await.unwrap());
    assert!(matches!(
        owner
            .credential(Some((&first.account, &first.bearer)), &stop)
            .await,
        Err(ManagedError::TokenMissing)
    ));
    let profiles = owner.profiles(&stop).await.unwrap();
    let BeginReply::Browser(returning) = owner
        .begin(redirect, Some(profiles[0].id), false, &stop)
        .await
        .unwrap()
    else {
        panic!("expected a browser attempt for a revoked grant");
    };
    let returning: HashMap<_, _> = Url::parse(&returning.authorization_url)
        .unwrap()
        .query_pairs()
        .into_owned()
        .collect();
    assert_eq!(returning["client_id"], "oaiapp_fixture");
    assert_eq!(
        returning["ext_agent_host_id"],
        authorization["ext_agent_host_id"]
    );
    assert_eq!(returning.get("agent_name_hint"), None);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(home.path().join("chatgpt-account.json"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    owner.close().await;
    root.shutdown().await.unwrap();
    serve.abort();
}

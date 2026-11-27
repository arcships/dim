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

fn remote_model(value: Value) -> RemoteModel {
    serde_json::from_value(value).unwrap()
}

#[test]
fn chatgpt_reasoning_levels_are_filtered_and_defaulted() {
    let model = remote_model(json!({
        "slug":"gpt-fixture","display_name":"Fixture","visibility":"list",
        "supported_reasoning_levels":[
            {"effort":"low","description":"low"},
            {"effort":"ultra","description":"not accepted"},
            {"effort":"high"},
            {"effort":"max"},
            {"effort":"low"},
            {"effort":7},
            "bogus"
        ],
        "default_reasoning_level":"high"
    }));
    let (capabilities, metadata) = project_remote_model(&model);
    assert!(capabilities.reasoning);
    let reasoning = metadata.unwrap().reasoning.unwrap();
    assert!(reasoning.supported);
    assert_eq!(reasoning.effort_options.unwrap(), ["low", "high", "max"]);
    assert_eq!(reasoning.effort.as_deref(), Some("high"));
}

#[test]
fn chatgpt_reasoning_default_is_dropped_when_not_offered() {
    for (levels, default) in [
        (json!([{"effort":"low"},{"effort":"ultra"}]), json!("ultra")),
        (json!([{"effort":"low"}]), json!(42)),
    ] {
        let model = remote_model(json!({
            "slug":"gpt-fixture","display_name":"Fixture","visibility":"list",
            "supported_reasoning_levels":levels,
            "default_reasoning_level":default
        }));
        let (_, metadata) = project_remote_model(&model);
        let reasoning = metadata.unwrap().reasoning.unwrap();
        assert_eq!(reasoning.effort_options.unwrap(), ["low"]);
        assert_eq!(reasoning.effort, None);
    }
}

#[test]
fn chatgpt_input_modalities_are_lenient_and_project_vision() {
    let model = remote_model(json!({
        "slug":"gpt-fixture","display_name":"Fixture","visibility":"list",
        "input_modalities":[" IMAGE ","text","pdf",7,null,"image"]
    }));
    let (capabilities, _) = project_remote_model(&model);
    assert_eq!(capabilities.modalities, ["image", "text", "pdf"]);
    assert!(capabilities.vision);

    let text_only = remote_model(json!({
        "slug":"gpt-fixture","display_name":"Fixture","visibility":"list",
        "input_modalities":["text"]
    }));
    let (capabilities, _) = project_remote_model(&text_only);
    assert_eq!(capabilities.modalities, ["text"]);
    assert!(!capabilities.vision);
}

#[test]
fn chatgpt_capability_fields_tolerate_wrong_types() {
    for value in [
        json!({"models":[{
            "slug":"gpt-fixture","display_name":"Fixture","visibility":"list",
            "supported_reasoning_levels":"not an array",
            "default_reasoning_level":["low"],
            "input_modalities":{"input":["image"]}
        }]}),
        json!({"models":[{
            "slug":"gpt-fixture","display_name":"Fixture","visibility":"list",
            "supported_reasoning_levels":null,
            "default_reasoning_level":null,
            "input_modalities":null
        }]}),
    ] {
        let list: ModelList = serde_json::from_value(value).unwrap();
        let (capabilities, metadata) = project_remote_model(&list.models[0]);
        assert_eq!(capabilities.modalities, ["text"]);
        assert!(!capabilities.vision);
        assert!(!capabilities.reasoning);
        assert!(metadata.is_none());
    }
}

#[test]
fn chatgpt_no_surviving_reasoning_levels_keeps_reasoning_undeclared() {
    for levels in [
        json!([{"effort":"ultra"},{"effort":"bogus"}]),
        json!([]),
        json!([{"effort":"lowest"}]),
    ] {
        let model = remote_model(json!({
            "slug":"gpt-fixture","display_name":"Fixture","visibility":"list",
            "supported_reasoning_levels":levels,
            "default_reasoning_level":"low"
        }));
        let (capabilities, metadata) = project_remote_model(&model);
        assert!(!capabilities.reasoning);
        assert!(metadata.is_none());
    }
}

/// The picker and the implicit default must stay inside what the model's
/// actual request path accepts (#2189): the aimux Responses model rejects
/// `none` and `minimal` for the minor-versioned gpt-6 slugs with an
/// unsupported warning that fails the turn, so the projection must not offer
/// them. Levels keep the order the catalog declares them in, so the same
/// filtering applies to any selection or default, which only ever names a
/// declared level. Levels the request path does not constrain (pre-gpt-6
/// slugs) pass through unchanged.
#[test]
fn chatgpt_reasoning_levels_stay_within_the_request_paths_acceptance() {
    // `gpt-6.2-sol`'s request path accepts low/medium/high/xhigh/max: `none`
    // and `minimal` must never be offered, and a rejected declared default
    // must not survive either.
    let model = remote_model(json!({
        "slug":"gpt-6.2-sol","display_name":"Sol","visibility":"list",
        "supported_reasoning_levels":[
            {"effort":"none","description":"off"},
            {"effort":"minimal","description":"tiny"},
            {"effort":"low"},
            {"effort":"medium"},
            {"effort":"high"},
            {"effort":"xhigh"},
            {"effort":"max"}
        ],
        "default_reasoning_level":"minimal"
    }));
    let (capabilities, metadata) = project_remote_model(&model);
    assert!(capabilities.reasoning);
    let reasoning = metadata.unwrap().reasoning.unwrap();
    let offered = reasoning.effort_options.unwrap();
    assert_eq!(
        offered,
        ["low", "medium", "high", "xhigh", "max"],
        "only levels the request path accepts, in catalog order"
    );
    // Every offered level is one the request path itself accepts for the
    // slug, so a level the route rejects is never published (#2189). The
    // published offer may still be narrower than the table, since the
    // projection keeps only levels inside what the wire accepts and the
    // endpoint declares both.
    let accepted = dim_agent_aimux::accepted_reasoning_efforts("gpt-6.2-sol")
        .expect("the request path constrains the slug");
    for effort in &offered {
        assert!(
            accepted.contains(&effort.as_str()),
            "{effort} must be one the request path accepts"
        );
    }
    assert_eq!(
        reasoning.effort, None,
        "a default the request path rejects cannot become the implicit default"
    );

    // `gpt-6-sol`'s request path accepts `none`; the explicit major-only
    // spelling keeps it.
    let model = remote_model(json!({
        "slug":"gpt-6-sol","display_name":"Sol","visibility":"list",
        "supported_reasoning_levels":[
            {"effort":"none"},
            {"effort":"low"},
            {"effort":"high"},
            {"effort":"max"}
        ],
        "default_reasoning_level":"high"
    }));
    let (_, metadata) = project_remote_model(&model);
    let reasoning = metadata.unwrap().reasoning.unwrap();
    let offered = reasoning.effort_options.unwrap();
    assert_eq!(offered, ["none", "low", "high", "max"]);
    for effort in &offered {
        assert!(
            dim_agent_aimux::accepted_reasoning_efforts("gpt-6-sol")
                .expect("the request path constrains the slug")
                .contains(&effort.as_str()),
            "{effort} must survive the request path"
        );
    }
    assert_eq!(reasoning.effort.as_deref(), Some("high"));

    // A slug the request path does not constrain is not filtered: every
    // declared level it accepts is offered as before.
    let model = remote_model(json!({
        "slug":"gpt-fixture","display_name":"Fixture","visibility":"list",
        "supported_reasoning_levels":[
            {"effort":"none"},
            {"effort":"minimal"},
            {"effort":"low"}
        ],
        "default_reasoning_level":"low"
    }));
    let (_, metadata) = project_remote_model(&model);
    let reasoning = metadata.unwrap().reasoning.unwrap();
    assert_eq!(
        reasoning.effort_options.unwrap(),
        ["none", "minimal", "low"],
        "unconstrained slugs keep every accepted level"
    );
}

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

/// A signed-in store whose access token stays valid for the test run.
fn signed_in_store(now: u64) -> Store {
    let profile = Uuid::new_v4();
    Store {
        version: 1,
        host_id: Uuid::new_v4(),
        generation: 0,
        active: Some(profile),
        pending_client_id: None,
        legacy_codex_hidden: false,
        profiles: vec![Profile {
            id: profile,
            client_id: "oaiapp_fixture".into(),
            subject: "fixture-subject".into(),
            email: None,
            name: None,
            tokens: Some(Grant {
                access: "fixture-access".into(),
                refresh: "fixture-refresh".into(),
                expires_at: now + 3600,
                earliest_refresh_at: None,
                scopes: SCOPES.split_whitespace().map(str::to_owned).collect(),
            }),
        }],
    }
}

#[tokio::test]
async fn chatgpt_catalog_projects_declared_capabilities() {
    let home = tempfile::tempdir().unwrap();
    let root = rutis::Ctx::root().unwrap();
    root.plugin(FileStoragePlugin::new(vec![
        home.path().canonicalize().unwrap(),
    ]))
    .await
    .unwrap();
    let files = root.get::<FileStorage>().unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let issuer = format!("http://{}", listener.local_addr().unwrap());
    let endpoints = Router::new().route(
        "/v1/models",
        get(|headers: HeaderMap| async move {
            assert_eq!(headers["authorization"], "Bearer fixture-access");
            Json(json!({"models":[
                {
                    "slug":"gpt-fixture-rich","display_name":"Rich","visibility":"list",
                    "supported_reasoning_levels":[
                        {"effort":"low","description":"low"},
                        {"effort":"ultra","description":"not accepted"},
                        {"effort":"high"},
                        {"effort":"max"},
                        {"effort":"bogus"},
                        {"effort":"low"}
                    ],
                    "default_reasoning_level":"high",
                    "input_modalities":["text","image"]
                },
                {
                    "slug":"gpt-fixture-plain","display_name":"Plain","visibility":"list",
                    "supported_reasoning_levels":[{"effort":"ultra"}],
                    "default_reasoning_level":"ultra",
                    "input_modalities":["text"]
                },
                {
                    "slug":"gpt-6.2-sol","display_name":"Sol","visibility":"list",
                    "supported_reasoning_levels":[
                        {"effort":"none"},
                        {"effort":"minimal"},
                        {"effort":"low"},
                        {"effort":"medium"},
                        {"effort":"high"},
                        {"effort":"xhigh"},
                        {"effort":"max"}
                    ],
                    "default_reasoning_level":"minimal",
                    "input_modalities":["text","image"]
                }
            ]}))
        }),
    );
    let serve = tokio::spawn(async move { axum::serve(listener, endpoints).await.unwrap() });
    let mut owner = ChatGpt::new(files.clone(), 0).unwrap();
    let config = Arc::get_mut(&mut owner).unwrap();
    config.issuer = Url::parse(&issuer).unwrap();
    config.api = config.issuer.join("/v1/").unwrap();
    let cancel = CancellationToken::new();
    let mut lease = owner.lock(&cancel).await.unwrap();
    write(&mut lease, &signed_in_store(now())).await.unwrap();
    drop(lease);
    let catalog = owner.catalog(&cancel).await.unwrap();
    assert_eq!(catalog.models.len(), 3);
    let rich = catalog.models[0].entry.clone();
    assert_eq!(rich.model_id, "gpt-fixture-rich");
    assert!(rich.capabilities.reasoning);
    assert_eq!(rich.capabilities.modalities, ["text", "image"]);
    assert!(rich.capabilities.vision);
    let reasoning = rich
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.reasoning.clone())
        .expect("a listed model with surviving levels declares reasoning");
    assert!(reasoning.supported);
    assert_eq!(reasoning.effort_options.unwrap(), ["low", "high", "max"]);
    assert_eq!(reasoning.effort.as_deref(), Some("high"));
    let plain = catalog.models[1].entry.clone();
    assert_eq!(plain.model_id, "gpt-fixture-plain");
    assert!(!plain.capabilities.reasoning);
    assert_eq!(plain.capabilities.modalities, ["text"]);
    assert!(!plain.capabilities.vision);
    assert!(plain.metadata.is_none());
    let sol = catalog.models[2].entry.clone();
    assert_eq!(sol.model_id, "gpt-6.2-sol");
    assert!(sol.capabilities.reasoning);
    assert!(sol.capabilities.vision);
    let sol_reasoning = sol
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.reasoning.clone())
        .expect("Sol declares reasoning");
    assert_eq!(
        sol_reasoning.effort_options.unwrap(),
        ["low", "medium", "high", "xhigh", "max"],
        "levels the request path rejects never reach the picker"
    );
    assert_eq!(
        sol_reasoning.effort, None,
        "the rejected declared default is not the implicit default"
    );
    owner.close().await;
    root.shutdown().await.unwrap();
    serve.abort();
}

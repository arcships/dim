//! Declared Settings sources through a TypeScript row loaded on C4's path
//! (settings-service §11): the row registers source `example` when it
//! starts, its owner code uses `settings`, and local clients use
//! `settings.*`.
use super::*;
use dim_agent_runtime::Names;
use dim_app_server::{
    session_typescript::SessionTypeScriptConfig,
    settings::{Settings, SettingsChanged, SourceDeclaration, settings_changes},
};
use rutis::Ctx;
use rutis::host::{BoxFuture, DynPlugin, Effect, Key, TypeKey};
use std::path::PathBuf;

const OWNER: &str = "example-owner";

fn repository() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}
/// The owner row; its code registers source `example` (or the sources of
/// `config.settings`) when it starts.
fn owner_row(id: &str) -> Value {
    json!({
        "id": id,
        "name": repository().join("runtime/fixtures/settings-owner.ts"),
        "provides": {},
        "config": {},
    })
}
fn declaration() -> Value {
    serde_json::from_str::<Value>(include_str!(
        "../../../../runtime/fixtures/settings-owner.settings.json"
    ))
    .unwrap()[0]
        .clone()
}
fn rows(rows: Vec<Value>) -> SessionTypeScriptConfig {
    SessionTypeScriptConfig {
        python: None,
        acquire: None,
        node: std::env::var_os("DIM_TEST_NODE")
            .expect("DIM_TEST_NODE")
            .into(),
        js_runtime: std::env::var_os("DIM_TEST_JS_RUNTIME")
            .expect("DIM_TEST_JS_RUNTIME")
            .into(),
        anchor: repository().join("runtime/package.json"),
        rows,
    }
}
fn product_home() -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let home = PathBuf::from("/tmp").join(format!("ds-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&home).unwrap();
    std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
    home
}
fn config(home: &std::path::Path, session_typescript: SessionTypeScriptConfig) -> ServerConfig {
    ServerConfig {
        product_home: Some(home.to_owned()),
        target_home: Some(home.join("rust")),
        session_typescript: Some(session_typescript),
        ..Default::default()
    }
}
/// A protected local profile without a TypeScript row: the client methods, the
/// authorization map and `session.create` need no runtime of their own.
fn local_config(home: &std::path::Path) -> ServerConfig {
    ServerConfig {
        model_catalog: Some(configured_models()),
        target_home: Some(home.to_owned()),
        ..Default::default()
    }
}
/// The `--model-config` catalog: one selectable model, so `session.create`
/// resolves a model without a product home. No request reaches it.
fn configured_models() -> Arc<dim_agent_aimux::AimuxModels> {
    let file: dim_agent_aimux::config::ModelConfigFile = serde_json::from_value(json!({
        "defaultModel": {"providerId":"local","modelId":"main"},
        "providers": [{
            "id": "local",
            "kind": "openai-compatible",
            "apiKeyEnv": null,
            "baseUrl": "http://127.0.0.1:1/v1",
            "compatible": null,
            "models": [{
                "id": "main",
                "contextWindow": 128000,
                "maxOutputTokens": 16384,
                "config": {"maxTokens": 16384}
            }]
        }]
    }))
    .unwrap();
    Arc::new(dim_agent_aimux::AimuxModels::from_config(file).unwrap())
}

/// Registers a declared source with project scope, the way a C4 owner row does
/// (§3.1): `settings.read`/`settings.update` need one to take `{project: root}`.
struct ProjectSource(Key<Settings>);
impl ProjectSource {
    fn new(names: &Names<'_>) -> rutis::Result<Self> {
        Ok(Self(names.key()?))
    }
}
impl DynPlugin for ProjectSource {
    fn name(&self) -> &str {
        "test.settings-project-source"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.0.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            ctx.require_at(&self.0)?.register(
                ctx,
                SourceDeclaration {
                    source: "example".into(),
                    user_file: "example.json".into(),
                    project_file: Some("example.project.json".into()),
                    schema: json!({
                        "type": "object",
                        "properties": {
                            "maxResults": {"type": "integer", "minimum": 1, "default": 5},
                        },
                        "additionalProperties": false,
                    }),
                    user_only: vec![],
                },
            )?;
            Ok(Effect::Done)
        })
    }
}

#[derive(Clone)]
struct Changes(Arc<tokio::sync::watch::Sender<Vec<SettingsChanged>>>);
/// A Session part with a Rust listener on `settings_changes("example")`.
struct Probe(Changes);
impl Plugin for Probe {
    type Deps = ();
    fn name(&self) -> &str {
        "test.settings-probe"
    }
    async fn apply(&self, ctx: &Ctx, (): Self::Deps) -> rutis::Result {
        let changes = self.0.clone();
        ctx.listen::<SettingsChanged>()
            .name(settings_changes("example"))
            .on(move |event, _| {
                let changes = changes.clone();
                async move {
                    changes.0.send_modify(|log| log.push((*event).clone()));
                    Ok(None)
                }
            })?;
        Ok(())
    }
}

/// Reads `source` once its owner row has started and registered it: the
/// row applies after native startup, and the source appears only then.
pub(super) async fn registered(client: &mut Client, source: &str) -> Value {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let read = client
                .rpc("settings.read", json!({"source":source,"scope":"user"}))
                .await;
            if read["error"]["data"]["appError"] != "settings_unknown_source" {
                break read;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap()
}

/// A call to the owner's Session method; the `settings` envelope it returned.
async fn owner(client: &mut Client, session: &str, method: &str, mut params: Value) -> Value {
    params["sessionId"] = json!(session);
    let reply = client
        .rpc(&format!("session.example.{method}"), params)
        .await;
    assert_ok(&reply);
    reply["result"].clone()
}

/// §11 items 1–3 with the owner code running in the Node runtime, and §4.5
/// event delivery to that TypeScript part.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires DIM_TEST_NODE and DIM_TEST_JS_RUNTIME"]
async fn node_settings_typescript_owner_reads_and_writes() {
    let home = product_home();
    let log = Changes(Arc::new(tokio::sync::watch::Sender::new(Vec::new())));
    let probe = log.clone();
    let (server, _) = launch_with(
        Arc::new(MockTools),
        config(&home, rows(vec![owner_row(OWNER)])),
        move |builder| {
            builder.require_feature(
                Feature::new("test.settings-probe").session("test.settings-probe", move |_| {
                    Ok(Box::new(Probe(probe.clone())))
                }),
            );
        },
    )
    .await;
    let mut client = Client::new_version(&server, false, 3).await;
    let models = [
        json!({"modelId":"fixture","displayName":"fixture","status":"active",
        "capabilities":dim_agent_types::CatalogCapabilities::with_limits(128_000, 16_384)}),
    ];
    assert_ok(&client.rpc("provider.create", json!({"providerId":"018ec73f-0000-7000-8000-000000000010","displayName":"Fixture","connection":{"driverKind":"openai-compatible","baseUrl":"http://localhost/v1","auth":{"type":"anonymous"}},"models":models,"enabled":true})).await);
    assert_ok(
        &client
            .rpc(
                "provider.select",
                json!({"providerId":"018ec73f-0000-7000-8000-000000000010","modelId":"fixture"}),
            )
            .await,
    );
    let created = client.rpc("session.create", create(json!([]))).await;
    assert_ok(&created);
    let session = created["result"]["sessionId"].as_str().unwrap().to_owned();
    let custom = owner(&mut client, &session, "custom_read", json!({})).await;
    assert_eq!(custom["value"]["document"], json!({"count":3}), "{custom}");
    let revision = &custom["value"]["revision"];
    let update = |count| json!({"expectedRevision":revision, "count":count});
    let rejected = owner(&mut client, &session, "custom_update", update(-1)).await;
    assert_eq!(rejected["error"]["code"], "invalid_document", "{rejected}");
    let written = owner(&mut client, &session, "custom_update", update(6)).await;
    assert_eq!(
        written["value"]["document"],
        json!({"count":6}),
        "{written}"
    );
    assert_eq!(
        owner(&mut client, &session, "custom_read", json!({})).await,
        written
    );
    let stale = owner(&mut client, &session, "custom_update", update(5)).await;
    assert_eq!(stale["error"]["code"], "conflict", "{stale}");
    let user = json!({"source":"example","scope":"user"});
    let mut changes = log.0.subscribe();

    let read = owner(&mut client, &session, "read", user.clone()).await;
    assert_eq!(read["ok"], true, "{read}");
    assert_eq!(read["value"]["document"], json!({"maxResults": 5}));
    let loader = server.service::<rutis_loader::Loader>().unwrap();
    let id = loader
        .get(OWNER)
        .unwrap()
        .view
        .unwrap()
        .instance()
        .to_string();
    let declarations = owner(&mut client, &session, "declared", json!({"instanceId":id})).await;
    assert_eq!(
        declarations,
        serde_json::from_str::<Value>(include_str!(
            "../../../../runtime/fixtures/settings-owner.settings.json"
        ))
        .unwrap()
    );
    assert_eq!(
        owner(&mut client, &session, "declared", json!({"instanceId":"0"})).await,
        json!([])
    );
    let first = read["value"]["revision"].clone();
    let write = |revision: &Value, document: Value| json!({"source":"example","scope":"user","expectedRevision":revision,"document":document});
    let written = owner(
        &mut client,
        &session,
        "update",
        write(&first, json!({"maxResults": 7})),
    )
    .await;
    assert_eq!(
        written["value"]["document"],
        json!({"maxResults": 7}),
        "{written}"
    );
    let second = written["value"]["revision"].clone();
    assert_ne!(second, first);
    for (document, revision, code) in [
        (json!({"maxResults": 8}), &first, "conflict"),
        (json!({"maxResults": 0}), &second, "invalid_document"),
    ] {
        let rejected = owner(&mut client, &session, "update", write(revision, document)).await;
        assert_eq!(rejected["ok"], false, "{rejected}");
        assert_eq!(rejected["error"]["code"], code, "{rejected}");
    }
    let read = owner(&mut client, &session, "read", user.clone()).await;
    assert_eq!(read["value"]["revision"], second);

    let service = json!({"kind":"web","apiKey":"web-search"});
    let edited = client
        .rpc(
            "settings.update",
            write(&second, json!({"maxResults": 7, "searchService": service})),
        )
        .await;
    assert_ok(&edited);
    let third = edited["result"]["revision"].clone();
    let rejected = owner(
        &mut client,
        &session,
        "update",
        write(
            &third,
            json!({"maxResults": 7, "searchService": {"kind":"other","apiKey":"web-search"}}),
        ),
    )
    .await;
    assert_eq!(rejected["error"]["code"], "user_only_field", "{rejected}");
    let kept = owner(
        &mut client,
        &session,
        "update",
        write(&third, json!({"maxResults": 9, "searchService": service})),
    )
    .await;
    assert_eq!(kept["ok"], true, "{kept}");
    let fourth = kept["value"]["revision"].clone();

    // Item 3: one change per successful write on either path, none for a
    // rejected one, in the §4.5 shape.
    let changed = tokio::time::timeout(
        Duration::from_secs(5),
        changes.wait_for(|log| log.len() >= 3),
    )
    .await
    .unwrap()
    .unwrap()
    .clone();
    assert_eq!(
        changed
            .iter()
            .map(|event| serde_json::to_value(event).unwrap())
            .collect::<Vec<_>>(),
        [&second, &third, &fourth]
            .map(|revision| json!({"source":"example","scope":"user","revision":revision}))
    );

    assert_eq!(log.0.borrow().len(), 3);

    // §4.5 delivery: the TypeScript part listens to `settings/example` and
    // `settings/projects` with `ctx.on`, and receives the same changes in
    // commit order, and a project authorized after it loaded.
    let delivered = |events: &Value, name: &str| -> Vec<Value> {
        events
            .as_array()
            .unwrap()
            .iter()
            .filter(|event| event["name"] == name)
            .map(|event| event["payload"].clone())
            .collect()
    };
    let project = home.join("project");
    std::fs::create_dir_all(&project).unwrap();
    let mut other = create(json!([]));
    other["cwd"] = json!(project);
    assert_ok(&client.rpc("session.create", other).await);
    let events = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let events = owner(&mut client, &session, "events", json!({})).await;
            if !delivered(&events, "settings/projects").is_empty() {
                break events;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        delivered(&events, "settings/example"),
        changed
            .iter()
            .map(|event| serde_json::to_value(event).unwrap())
            .collect::<Vec<_>>()
    );
    let authorized = delivered(&events, "settings/projects");
    assert_eq!(authorized.len(), 1, "{events}");
    assert_eq!(authorized[0]["cwd"], json!(project.canonicalize().unwrap()));
    assert!(authorized[0]["root"].is_u64(), "{events}");
    server.shutdown();
    server.join().await.unwrap();
    let _ = std::fs::remove_dir_all(&home);
}

/// The client path (§4.3) and the registration boundary (§3.1, §11 item 5):
/// local connections read and write registered sources, Rust sources and
/// paired devices are not served, and invalid or conflicting registrations
/// fail the registering row.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires DIM_TEST_NODE and DIM_TEST_JS_RUNTIME"]
async fn node_settings_clients_and_declaration_boundaries() {
    let home = product_home();
    let (server, _) = launch_with_agent(
        Arc::new(MockTools),
        config(&home, rows(vec![owner_row(OWNER)])),
    )
    .await;
    let mut client = Client::new_version(&server, false, 3).await;
    for method in ["settings.read", "settings.update", "settings.sources"] {
        assert!(client.features.as_array().unwrap().contains(&json!(method)));
    }
    let mut remote = Client::new_version(&server, true, 3).await;
    assert_eq!(
        remote
            .rpc("settings.read", json!({"source":"example","scope":"user"}))
            .await["error"]["data"]["appError"],
        "local_transport_required"
    );
    assert_eq!(
        remote.rpc("settings.sources", json!({})).await["error"]["data"]["appError"],
        "local_transport_required"
    );
    let read = registered(&mut client, "example").await;
    assert_ok(&read);
    assert_eq!(read["result"]["document"], json!({"maxResults": 5}));
    let first = read["result"]["revision"].clone();
    let service = json!({"kind":"web","apiKey":"web-search"});
    let written = client
        .rpc(
            "settings.update",
            json!({"source":"example","scope":"user","expectedRevision":first,"document":{"searchService":service}}),
        )
        .await;
    assert_ok(&written);
    assert_eq!(
        written["result"]["document"],
        json!({"maxResults": 5, "searchService": service})
    );
    let second = written["result"]["revision"].clone();
    assert_ne!(second, first);
    for (params, error) in [
        (
            json!({"source":"example","scope":"user","expectedRevision":first,"document":{}}),
            "settings_conflict",
        ),
        (
            json!({"source":"example","scope":"user","expectedRevision":second,"document":{"maxResults":0}}),
            "invalid_params",
        ),
        (
            json!({"source":"example","scope":{"project":0},"expectedRevision":second,"document":{}}),
            "invalid_params",
        ),
        (
            json!({"source":"example","scope":"workspace","expectedRevision":second,"document":{}}),
            "invalid_params",
        ),
        (
            json!({"source":"mcp","scope":"user","expectedRevision":second,"document":{}}),
            "settings_unknown_source",
        ),
    ] {
        assert_eq!(
            client.rpc("settings.update", params).await["error"]["data"]["appError"],
            error
        );
    }
    assert_eq!(
        client
            .rpc("settings.read", json!({"source":"mcp","scope":"user"}))
            .await["error"]["data"]["appError"],
        "settings_unknown_source"
    );
    server.shutdown();
    server.join().await.unwrap();

    let failing = |declarations: Vec<Value>| {
        let mut row = owner_row(OWNER);
        row["config"]["settings"] = json!(declarations);
        row
    };
    let mut rust = declaration();
    rust["source"] = json!("mcp");
    let mut malformed = declaration();
    malformed["userfile"] = json!("example.json");
    for (row, reason) in [
        (failing(vec![rust]), "settings source mcp"),
        (
            failing(vec![declaration(), declaration()]),
            "settings source example",
        ),
        (failing(vec![malformed]), "settings request"),
    ] {
        let (server, _) =
            launch_with_agent(Arc::new(MockTools), config(&home, rows(vec![row]))).await;
        tokio::time::timeout(Duration::from_secs(10), async {
            while !server.diagnostics().plugins.iter().any(|plugin| {
                plugin.state == rutis::host::FiberState::Failed
                    && plugin
                        .error
                        .as_ref()
                        .is_some_and(|error| error.to_string().contains(reason))
            }) {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("{reason}: the owner row did not fail"));
        let mut client = Client::new_version(&server, false, 3).await;
        assert_eq!(
            client
                .rpc("settings.read", json!({"source":"example","scope":"user"}))
                .await["error"]["data"]["appError"],
            "settings_unknown_source"
        );
        server.shutdown();
        server.join().await.unwrap();
    }
    let _ = std::fs::remove_dir_all(&home);
}

/// §4.3 discovery is local-only by admission: an authenticated Web entry
/// connection is answered `unknown_method` for `settings.sources`, while the
/// allowlisted `settings.read` still serves `model-log`. The guarantee is the
/// absence of `settings.sources` from `WEB_MANAGEMENT_METHODS` and from the
/// per-source admission of `settings.read`/`settings.update`, so the entry's
/// `||` branch denies it by decision, not by oversight.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_web_entry_is_not_served_settings_sources() {
    let home = product_home();
    let assets = super::web_entry::frontend_fixture();
    let (server, _) = launch_with_agent(
        Arc::new(MockTools),
        ServerConfig {
            product_home: Some(home.clone()),
            target_home: Some(home.join("rust")),
            web_assets: Some(assets.clone()),
            ..Default::default()
        },
    )
    .await;
    let mut local = Client::new_version(&server, false, 3).await;
    let opened = local.rpc("web.open", json!({"port":0})).await["result"].clone();
    let address = opened["address"].as_str().unwrap().to_owned();
    let bootstrap = opened["url"]
        .as_str()
        .unwrap()
        .split_once("#bootstrap=")
        .unwrap()
        .1
        .to_owned();
    let origin = format!("http://{address}");
    let (status, head, _) = super::web_entry::http(
        &address,
        super::web_entry::post(
            &address,
            "/api/auth/exchange",
            &origin,
            &json!({"token": bootstrap}).to_string(),
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
        .unwrap()
        .to_owned();
    let mut web = super::web_entry::web_client(&address, &cookie).await;
    assert_eq!(
        web.rpc("settings.sources", json!({})).await["error"]["data"]["appError"],
        "unknown_method"
    );
    assert_eq!(
        web.rpc("settings.projects", json!({})).await["error"]["data"]["appError"],
        "unknown_method"
    );
    let read = web
        .rpc(
            "settings.read",
            json!({"source":"model-log","scope":"user"}),
        )
        .await;
    assert_ok(&read);
    assert_eq!(read["result"]["document"], json!({"enabled": false}));
    server.shutdown();
    server.join().await.unwrap();
    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&assets);
}

/// §4.3: `settings.projects` is the read of the map a client needs to pass
/// `{project: root}`. It grows with every authorization — including one a
/// Session never made — and it is not pushed: one read answers it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_local_client_lists_authorized_project_roots() {
    let home = product_home();
    let project = home.join("project");
    let hook_only = home.join("hook-only");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir_all(&hook_only).unwrap();
    let (server, _) = launch_with(Arc::new(MockTools), local_config(&home), |builder| {
        builder.plugin(|names| Ok(Box::new(ProjectSource::new(names)?) as Box<dyn DynPlugin>));
    })
    .await;
    let mut client = Client::new_version(&server, false, 3).await;
    assert!(
        client
            .features
            .as_array()
            .unwrap()
            .contains(&json!("settings.projects")),
        "{}",
        client.features
    );
    assert_eq!(
        client.rpc("settings.projects", json!({})).await["result"],
        json!({"projects": []})
    );
    assert_eq!(
        client
            .rpc("settings.projects", json!({"unexpected": true}))
            .await["error"]["data"]["appError"],
        "invalid_params"
    );

    assert_ok(&client.rpc("session.create", json!({"cwd": project})).await);
    let cwd = project.canonicalize().unwrap();
    let listed = client.rpc("settings.projects", json!({})).await;
    let projects = listed["result"]["projects"].as_array().unwrap();
    assert_eq!(projects.len(), 1, "{listed}");
    assert_eq!(projects[0]["cwd"], json!(cwd));
    let root = projects[0]["root"].as_u64().expect("root number");

    // The listed root is exactly the scope `settings.read` takes.
    let read = client
        .rpc(
            "settings.read",
            json!({"source":"example","scope":{"project": root}}),
        )
        .await;
    assert_ok(&read);
    assert_eq!(read["result"]["revision"], "missing");
    assert_eq!(read["result"]["document"], json!({"maxResults": 5}));

    // Authorizing the same cwd again adds no duplicate.
    assert_ok(&client.rpc("session.create", json!({"cwd": project})).await);
    assert_eq!(
        client.rpc("settings.projects", json!({})).await["result"],
        listed["result"]
    );

    // A root this process never authorized is not a scope.
    let unknown = client
        .rpc(
            "settings.read",
            json!({"source":"example","scope":{"project": root + 1000}}),
        )
        .await;
    assert_eq!(unknown["error"]["data"]["appError"], "invalid_params");

    // A root authorized for a cwd with no Session is listed too: the read is
    // Settings' map, not the Session directory.
    assert_ok(&client.rpc("hooks.catalog", json!({"cwd": hook_only})).await);
    let listed = client.rpc("settings.projects", json!({})).await;
    let projects = listed["result"]["projects"].as_array().unwrap();
    let hook_cwd = hook_only.canonicalize().unwrap();
    let cwds: Vec<Value> = projects
        .iter()
        .map(|project| project["cwd"].clone())
        .collect();
    assert_eq!(cwds, vec![json!(hook_cwd), json!(cwd)], "{listed}");
    let hook_root = projects[0]["root"].as_u64().unwrap();
    assert_ne!(hook_root, root);
    assert_ok(
        &client
            .rpc(
                "settings.read",
                json!({"source":"example","scope":{"project": hook_root}}),
            )
            .await,
    );

    server.shutdown();
    server.join().await.unwrap();
    let _ = std::fs::remove_dir_all(&home);
}

/// §4.1/§4.3: a root is a number this process assigned, never a durable
/// identity. A restarted daemon on the same home lists nothing until
/// something authorizes a project again.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn project_roots_are_process_local_across_a_restart() {
    let home = product_home();
    let project = home.join("project");
    std::fs::create_dir_all(&project).unwrap();
    let cwd = project.canonicalize().unwrap();
    for round in 0..2 {
        let (server, _) = launch_with_agent(Arc::new(MockTools), local_config(&home)).await;
        let mut client = Client::new_version(&server, false, 3).await;
        assert_eq!(
            client.rpc("settings.projects", json!({})).await["result"],
            json!({"projects": []}),
            "round {round}"
        );
        assert_ok(&client.rpc("session.create", json!({"cwd": project})).await);
        let listed = client.rpc("settings.projects", json!({})).await;
        let projects = listed["result"]["projects"].as_array().unwrap();
        assert_eq!(projects.len(), 1, "round {round}: {listed}");
        assert_eq!(projects[0]["cwd"], json!(cwd), "round {round}: {listed}");
        server.shutdown();
        server.join().await.unwrap();
    }
    let _ = std::fs::remove_dir_all(&home);
}

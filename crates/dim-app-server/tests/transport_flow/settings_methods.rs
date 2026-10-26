//! Declared Settings sources through a TypeScript row loaded on C4's path
//! (settings-service §11): the row's package declares source `example` next
//! to its `provides`, its owner code uses `settings`, and local clients use
//! `settings.*`.
use super::*;
use dim_app_server::{
    session_typescript::SessionTypeScriptConfig,
    settings::{SettingsChanged, settings_changes},
};
use rutis::Ctx;
use std::path::PathBuf;

const OWNER: &str = "example-owner";

fn repository() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}
/// The owner row: `provides` and `settings` come from the package's own
/// declaration files, as a package would hand them to the host.
fn owner_row(id: &str) -> Value {
    json!({
        "id": id,
        "name": repository().join("runtime/fixtures/settings-owner.ts"),
        "provides": {},
        "config": {"declarations": serde_json::from_str::<Value>(include_str!("../../../../runtime/fixtures/settings-owner.settings.json")).unwrap()},
    })
}
fn rows(rows: Vec<Value>) -> SessionTypeScriptConfig {
    SessionTypeScriptConfig {
        python: None,
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
    // A product Session needs a selected model; this one is never called.
    let models = [
        json!({"modelId":"fixture","displayName":"fixture","status":"active",
        "capabilities":dim_agent_types::ModelCapabilities::default()}),
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
    let user = json!({"source":"example","scope":"user"});
    let mut changes = log.0.subscribe();

    // Item 1: the default document, a CAS write, a stale revision and
    // invalid documents that write nothing.
    let read = owner(&mut client, &session, "read", user.clone()).await;
    assert_eq!(read["ok"], true, "{read}");
    assert_eq!(read["value"]["document"], json!({"maxResults": 5}));
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

    // Item 2: a client may change the user-only field; the owner may not.
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

/// The client path (§4.3) and the declaration boundary (§3.1, §11 item 5):
/// local connections read and write declared sources, Rust sources and
/// paired devices are not served, and invalid or conflicting declarations
/// fail assembly.
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
    for method in ["settings.read", "settings.update"] {
        assert!(client.features.as_array().unwrap().contains(&json!(method)));
    }
    let mut remote = Client::new_version(&server, true, 3).await;
    assert_eq!(
        remote
            .rpc("settings.read", json!({"source":"example","scope":"user"}))
            .await["error"]["data"]["appError"],
        "local_transport_required"
    );
    // The owner's row registers the source when it loads, after startup.
    let read = json!({
        "result": super::process_typescript::files_available(
            &mut client,
            "settings.read",
            json!({"source":"example","scope":"user"}),
        )
        .await
    });
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
    // Rust sources keep their owners' methods.
    assert_eq!(
        client
            .rpc("settings.read", json!({"source":"mcp","scope":"user"}))
            .await["error"]["data"]["appError"],
        "settings_unknown_source"
    );
    server.shutdown();
    server.join().await.unwrap();

    // A declaration naming a Rust source, a second registration of one
    // source by another plugin, and a malformed entry each fail the plugin
    // that registers it; the App Server starts.
    let mut rust = owner_row(OWNER);
    rust["config"]["declarations"][0]["source"] = json!("mcp");
    let mut copy = owner_row("second-owner");
    copy["name"] = json!(repository().join("runtime/fixtures/settings-owner-second.ts"));
    let mut malformed = owner_row(OWNER);
    malformed["config"]["declarations"][0]["userfile"] = json!("example.json");
    // Of two plugins registering one source, whichever loads second fails.
    for (failing, plugins, reason) in [
        (vec![rust], &[OWNER][..], "belongs to another document type"),
        (
            vec![owner_row(OWNER), copy],
            &[OWNER, "second-owner"][..],
            "settings source example is already registered",
        ),
        (vec![malformed], &[OWNER][..], "settings"),
    ] {
        let server = launch(Arc::new(MockTools), config(&home, rows(failing))).await;
        let mut client = Client::new_version(&server, false, 3).await;
        let failed = loop {
            let listed = client.rpc("plugin.list", json!({})).await;
            let entries: Vec<Value> = listed["result"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|entry| plugins.contains(&entry["id"].as_str().unwrap()))
                .cloned()
                .collect();
            let settled = entries
                .iter()
                .all(|entry| entry["status"] == "failed" || entry["status"] == "running");
            if settled {
                let failed: Vec<Value> = entries
                    .into_iter()
                    .filter(|entry| entry["status"] == "failed")
                    .collect();
                break failed;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        };
        assert_eq!(failed.len(), 1, "{reason}: {failed:?}");
        assert!(
            failed[0]["error"].as_str().unwrap().contains(reason),
            "{reason}: {failed:?}"
        );
        drop(client);
        server.shutdown();
        server.join().await.unwrap();
    }
    let _ = std::fs::remove_dir_all(&home);
}

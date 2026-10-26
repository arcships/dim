//! Plugin management over the loader's layers (PluginManager §3, §8.1):
//! `plugin.list` projects the composed rows, `plugin.set_enabled` edits the
//! editable `user` layer, which persists as `plugins.json` in the home and
//! survives a restart, and a row the `locked` layer locks cannot be disabled.
use super::*;
use dim_app_server::plugins::PluginDefaults;
use std::path::{Path, PathBuf};

fn home() -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let home = PathBuf::from("/tmp").join(format!("dp-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&home).unwrap();
    std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
    home
}

/// The host infrastructure a client needs, locked as a product locks it.
const LOCKED: &[&str] = &[
    "ServerControl",
    "FileStorage",
    "SqliteStorage",
    "SessionStore",
    "ManagementMethods",
    "Settings",
    "PluginManager",
    "AgentDirectory",
    "CollaborationDirectory",
    "Agents",
    "Collaboration",
    "Connection",
    "Rpc",
    "Transport",
];

fn config(home: &Path, locked: &[&str]) -> ServerConfig {
    ServerConfig {
        product_home: Some(home.to_owned()),
        target_home: Some(home.join("rust")),
        plugin_defaults: PluginDefaults {
            disabled: vec![],
            locked: locked.iter().map(|id| id.to_string()).collect(),
        },
        ..Default::default()
    }
}

async fn plugin(client: &mut Client, id: &str) -> Value {
    let listed = client.rpc("plugin.list", json!({})).await;
    assert_ok(&listed);
    listed["result"]
        .as_array()
        .unwrap()
        .iter()
        .find(|plugin| plugin["id"] == id)
        .unwrap_or_else(|| panic!("{id} not listed: {listed}"))
        .clone()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn disabling_a_plugin_persists_across_restart_and_locked_rows_refuse() {
    let home = home();
    let server = launch(Arc::new(MockTools), config(&home, LOCKED)).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let listed = plugin(&mut client, "PluginManager").await;
    assert_eq!(listed["kind"], "builtin", "{listed}");
    assert_eq!(listed["status"], "running", "{listed}");
    let rpc = plugin(&mut client, "Rpc").await;
    assert_eq!(rpc["locked"], true, "{rpc}");

    // The locked layer sits above the editable one.
    let refused = client
        .rpc("plugin.set_enabled", json!({"id": "Rpc", "enabled": false}))
        .await;
    assert_eq!(
        refused["error"]["data"]["appError"], "plugin_locked",
        "{refused}"
    );
    let missing = client
        .rpc(
            "plugin.set_enabled",
            json!({"id": "nope", "enabled": false}),
        )
        .await;
    assert_eq!(
        missing["error"]["data"]["appError"], "plugin_not_found",
        "{missing}"
    );

    // A built-in plugin is disabled, not removed, and stays so after a restart.
    let disabled = client
        .rpc(
            "plugin.set_enabled",
            json!({"id": "GrokOAuth", "enabled": false}),
        )
        .await;
    assert_ok(&disabled);
    assert_eq!(disabled["result"]["status"], "disabled", "{disabled}");
    assert!(home.join("rust/plugins.json").is_file());
    drop(client);
    server.shutdown();
    server.join().await.unwrap();

    let server = launch(Arc::new(MockTools), config(&home, LOCKED)).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let grok = plugin(&mut client, "GrokOAuth").await;
    assert_eq!(grok["enabled"], false, "{grok}");
    assert_eq!(grok["status"], "disabled", "{grok}");
    // A row that is not locked is optional: one that needs the disabled
    // plugin's service waits for it and startup does not.
    let provider = plugin(&mut client, "Provider").await;
    assert_eq!(provider["status"], "pending", "{provider}");
    let enabled = client
        .rpc(
            "plugin.set_enabled",
            json!({"id": "GrokOAuth", "enabled": true}),
        )
        .await;
    assert_eq!(enabled["result"]["status"], "running", "{enabled}");
    // Provider, which needs it, loads again.
    running(&mut client, "Provider").await;
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// A plugin directory from the default config is a group row of the
/// `defaults` layer: `plugin.list` shows it `default`, and disabling it is a
/// `user` layer edit that a restart keeps.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires DIM_TEST_NODE and DIM_TEST_JS_RUNTIME"]
async fn node_a_default_plugin_is_disabled_and_enabled_through_the_user_layer() {
    let home = home();
    let mut parts = super::session_typescript::fixture();
    let plugin = home.join("example");
    std::fs::create_dir_all(&plugin).unwrap();
    std::fs::write(
        plugin.join("index.ts"),
        format!(
            "export {{ default }} from {:?}\n",
            parts.rows[0]["name"].as_str().unwrap()
        ),
    )
    .unwrap();
    std::fs::write(
        plugin.join("plugin.json"),
        json!({"entry": "index.ts", "provides": {}}).to_string(),
    )
    .unwrap();
    parts.rows = vec![dim_app_server::plugins::read_plugin(&plugin, None).unwrap()];
    // The mock model profile, so a Session can run.
    let launch_once = || {
        launch(
            Arc::new(MockTools),
            ServerConfig {
                target_home: Some(home.join("rust")),
                session_typescript: Some(parts.clone()),
                plugin_defaults: PluginDefaults {
                    disabled: vec![],
                    locked: LOCKED.iter().map(|id| id.to_string()).collect(),
                },
                ..Default::default()
            },
        )
    };
    let server = launch_once().await;
    let mut client = Client::new_version(&server, false, 3).await;
    // A plugin row is optional: startup does not wait for it.
    let example = running(&mut client, "example").await;
    assert_eq!(example["kind"], "default", "{example}");
    let disabled = client
        .rpc(
            "plugin.set_enabled",
            json!({"id": "example", "enabled": false}),
        )
        .await;
    assert_eq!(disabled["result"]["status"], "disabled", "{disabled}");
    drop(client);
    server.shutdown();
    server.join().await.unwrap();

    let server = launch_once().await;
    let mut client = Client::new_version(&server, false, 3).await;
    assert_eq!(plugin_entry(&mut client).await["enabled"], false);
    let enabled = client
        .rpc(
            "plugin.set_enabled",
            json!({"id": "example", "enabled": true}),
        )
        .await;
    assert_eq!(enabled["result"]["status"], "running", "{enabled}");
    // Its Session part runs again: the fixture's method is served.
    let created = client.rpc("session.create", create(json!(["echo"]))).await;
    assert_ok(&created);
    let status = client
        .rpc(
            "session.typescript.status",
            json!({"sessionId": created["result"]["sessionId"]}),
        )
        .await;
    assert_eq!(status["result"]["calls"], 0, "{status}");
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

async fn plugin_entry(client: &mut Client) -> Value {
    plugin(client, "example").await
}

/// The plugin once it runs; a plugin row loads after startup.
async fn running(client: &mut Client, id: &str) -> Value {
    for _ in 0..200 {
        let entry = plugin(client, id).await;
        if entry["status"] == "running" {
            return entry;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("{id} never ran: {}", plugin(client, id).await);
}

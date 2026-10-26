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

/// A plugin directory whose Session part registers the tool `greet`,
/// answering `reply`.
fn greeter(directory: &Path, reply: &str) {
    let sdk = std::env::var("DIM_TEST_RUTIS_SDK").expect("DIM_TEST_RUTIS_SDK");
    std::fs::create_dir_all(directory).unwrap();
    std::fs::write(
        directory.join("plugin.json"),
        json!({"entry": "index.ts", "inject": ["partRegistry"], "provides": {}}).to_string(),
    )
    .unwrap();
    std::fs::write(
        directory.join("index.ts"),
        format!(
            r#"import {{ definePlugin }} from {sdk:?}
const session = definePlugin({{
  name: 'GreeterSession', inject: ['toolRegistry'], provides: {{}},
  async apply(ctx) {{
    await ctx.use('toolRegistry').register(
      {{ name: 'greet', description: 'Greets', inputSchema: {{ type: 'object' }}, source: 'extension', risk: 'session' }},
      async () => ({{ content: {reply:?} }}),
    )
  }},
}})
export default definePlugin({{
  name: 'Greeter', inject: ['partRegistry'], provides: {{}},
  async apply(ctx) {{
    await ctx.use('partRegistry').registerDefinition('session', 'GreeterSession', session)
  }},
}})
"#
        ),
    )
    .unwrap();
}

/// The text of the tool result `text` (`tool:NAME {json}`) produces.
async fn call(client: &mut Client, server: &RunningServer, session: &str, text: &str) -> String {
    super::session_typescript::submit(client, session, text).await;
    let session = server.agent().unwrap().get_session(session).unwrap();
    session
        .local("main", ..)
        .await
        .unwrap()
        .into_iter()
        .rev()
        .find_map(|event| match &event.kind {
            dim_agent_types::EventKind::ToolResult(batch) => {
                batch
                    .results
                    .first()
                    .map(|entry| match &entry.result.content {
                        dim_agent_types::ToolContent::Text(text) => text.clone(),
                        other => format!("{other:?}"),
                    })
            }
            _ => None,
        })
        .expect("a tool result")
}

async fn session(client: &mut Client) -> String {
    let mut params = create(json!(["greet", "plugin_list"]));
    params["permissionPolicy"]["allowedTools"] = json!(["greet", "plugin_list", "ts_echo"]);
    let created = client.rpc("session.create", params).await;
    assert_ok(&created);
    created["result"]["sessionId"].as_str().unwrap().into()
}

/// Install from a path runs at once; after an edit, reload brings in the new
/// code without a restart and leaves the other plugins' generations alone;
/// a restart keeps the install; remove leaves the directory. From Git,
/// update moves to the new commit and remove deletes the clone. The agent's
/// `plugin_list` tool reports the same plugins (PluginManager §14 1, 2, 6, 7).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires DIM_TEST_NODE, DIM_TEST_JS_RUNTIME and DIM_TEST_RUTIS_SDK"]
async fn node_install_reload_update_and_remove_plugins() {
    let home = home();
    let parts = super::session_typescript::fixture();
    let launch_once = || {
        launch(
            Arc::new(MockTools),
            ServerConfig {
                target_home: Some(home.join("rust")),
                session_typescript: Some(parts.clone()),
                ..Default::default()
            },
        )
    };
    let source = home.join("src/greeter");
    greeter(&source, "v1");
    let server = launch_once().await;
    let mut client = Client::new_version(&server, false, 3).await;
    let installed = client
        .rpc("plugin.install", json!({"source": {"path": source}}))
        .await;
    assert_ok(&installed);
    assert_eq!(installed["result"]["id"], "greeter", "{installed}");
    assert_eq!(installed["result"]["status"], "running", "{installed}");
    assert_eq!(installed["result"]["restartRequired"], false, "{installed}");
    let again = client
        .rpc("plugin.install", json!({"source": {"path": source}}))
        .await;
    assert_eq!(
        again["error"]["data"]["appError"], "plugin_exists",
        "{again}"
    );

    let sid = session(&mut client).await;
    assert_eq!(
        call(&mut client, &server, &sid, "tool:greet {}").await,
        "v1"
    );
    let other = client
        .rpc("session.typescript.status", json!({"sessionId": sid}))
        .await;
    assert_ok(&other);

    // Edit, reload: the same Session's next call runs the new code.
    greeter(&source, "v2");
    let reloaded = client.rpc("plugin.reload", json!({"id": "greeter"})).await;
    assert_ok(&reloaded);
    assert_eq!(reloaded["result"]["status"], "running", "{reloaded}");
    assert_eq!(
        call(&mut client, &server, &sid, "tool:greet {}").await,
        "v2"
    );
    let unchanged = client
        .rpc("session.typescript.status", json!({"sessionId": sid}))
        .await;
    assert_eq!(
        unchanged["result"]["generation"], other["result"]["generation"],
        "another plugin in the same runtime keeps its generation"
    );
    let listed = call(&mut client, &server, &sid, "tool:plugin_list {}").await;
    assert!(listed.contains("greeter (installed): running"), "{listed}");
    let builtin = client.rpc("plugin.reload", json!({"id": "Rpc"})).await;
    assert_eq!(
        builtin["error"]["data"]["appError"], "not_reloadable",
        "{builtin}"
    );
    drop(client);
    server.shutdown();
    server.join().await.unwrap();

    // The install is in the user layer.
    let server = launch_once().await;
    let mut client = Client::new_version(&server, false, 3).await;
    let greeter_entry = running(&mut client, "greeter").await;
    assert_eq!(greeter_entry["kind"], "installed", "{greeter_entry}");
    let removed = client.rpc("plugin.remove", json!({"id": "greeter"})).await;
    assert_ok(&removed);
    assert_eq!(removed["result"]["status"], "removed", "{removed}");
    assert!(
        source.join("plugin.json").is_file(),
        "a path plugin's directory stays"
    );
    let builtin = client.rpc("plugin.remove", json!({"id": "Rpc"})).await;
    assert_eq!(
        builtin["error"]["data"]["appError"], "not_removable",
        "{builtin}"
    );

    // From Git: clone, update to a new commit, remove the clone.
    let repository = home.join("src/greeter-repo");
    greeter(&repository, "git-1");
    let git = |args: &[&str]| {
        let status = std::process::Command::new("git")
            .current_dir(&repository)
            .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
            .args(args)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    };
    git(&["init", "--quiet", "--initial-branch", "main"]);
    git(&["add", "."]);
    git(&["commit", "--quiet", "-m", "one"]);
    let url = format!("file://{}", repository.display());
    let cloned = client
        .rpc(
            "plugin.install",
            json!({"source": {"git": url, "ref": "main"}, "id": "greeter"}),
        )
        .await;
    assert_ok(&cloned);
    let clone = home.join("rust/plugins/greeter");
    assert!(clone.join(".git").exists());
    let sid = session(&mut client).await;
    assert_eq!(
        call(&mut client, &server, &sid, "tool:greet {}").await,
        "git-1"
    );
    greeter(&repository, "git-2");
    git(&["commit", "--quiet", "-am", "two"]);
    let updated = client.rpc("plugin.update", json!({"id": "greeter"})).await;
    assert_ok(&updated);
    assert!(updated["result"]["commit"].is_string(), "{updated}");
    assert_eq!(
        call(&mut client, &server, &sid, "tool:greet {}").await,
        "git-2"
    );
    assert_ok(&client.rpc("plugin.remove", json!({"id": "greeter"})).await);
    assert!(!clone.exists(), "removing a Git plugin deletes its clone");
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// The repository's default config file loads its five TypeScript plugins,
/// as the default CLI does (PluginManager §7, §14 8).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires DIM_TEST_NODE, DIM_TEST_JS_RUNTIME and npm ci in each runtime/plugins package"]
async fn node_the_repository_default_config_loads_its_plugins() {
    let home = home();
    let file = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../runtime/plugins.default.json");
    let (rows, defaults) = dim_app_server::plugins::DefaultsFile::read(&file).unwrap();
    let mut parts = super::session_typescript::fixture();
    parts.anchor = file.canonicalize().unwrap();
    parts.rows = rows;
    let server = launch(
        Arc::new(MockTools),
        ServerConfig {
            product_home: Some(home.clone()),
            target_home: Some(home.join("rust")),
            session_typescript: Some(parts),
            plugin_defaults: defaults,
            ..Default::default()
        },
    )
    .await;
    let mut client = Client::new_version(&server, false, 3).await;
    for id in ["goal", "web", "session-title", "workflow", "scheduler"] {
        let plugin = running(&mut client, id).await;
        assert_eq!(plugin["kind"], "default", "{plugin}");
    }
    assert_eq!(plugin(&mut client, "Rpc").await["locked"], true);
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// A plugin installed while the App Server runs registers its business
/// configuration source from its code: `settings.*` serves it at once, with
/// no restart, and a reload keeps its document (PluginManager §6, §14 4).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires DIM_TEST_NODE, DIM_TEST_JS_RUNTIME and DIM_TEST_RUTIS_SDK"]
async fn node_an_installed_plugin_registers_its_settings_source() {
    let home = home();
    let sdk = std::env::var("DIM_TEST_RUTIS_SDK").expect("DIM_TEST_RUTIS_SDK");
    let plugin = home.join("src/configured");
    std::fs::create_dir_all(&plugin).unwrap();
    std::fs::write(
        plugin.join("plugin.json"),
        json!({"entry": "index.ts", "inject": ["settings"], "provides": {}}).to_string(),
    )
    .unwrap();
    std::fs::write(
        plugin.join("index.ts"),
        format!(
            r#"import {{ definePlugin }} from {sdk:?}
export default definePlugin({{
  name: 'Configured', inject: ['settings'], provides: {{}},
  async apply(ctx) {{
    await ctx.use('settings').register({{
      source: 'configured', userFile: 'configured.json',
      schema: {{ type: 'object', properties: {{ greeting: {{ type: 'string', default: 'hello' }} }}, additionalProperties: false }},
    }})
  }},
}})
"#
        ),
    )
    .unwrap();
    let mut config = config(&home, LOCKED);
    config.session_typescript = Some(super::session_typescript::fixture());
    let server = launch(Arc::new(MockTools), config).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let unknown = client
        .rpc(
            "settings.read",
            json!({"source": "configured", "scope": "user"}),
        )
        .await;
    assert_eq!(
        unknown["error"]["data"]["appError"], "settings_unknown_source",
        "{unknown}"
    );
    let installed = client
        .rpc("plugin.install", json!({"source": {"path": plugin}}))
        .await;
    assert_eq!(installed["result"]["status"], "running", "{installed}");
    assert_eq!(installed["result"]["restartRequired"], false, "{installed}");
    let read = client
        .rpc(
            "settings.read",
            json!({"source": "configured", "scope": "user"}),
        )
        .await;
    assert_eq!(
        read["result"]["document"],
        json!({"greeting": "hello"}),
        "{read}"
    );
    let updated = client
        .rpc(
            "settings.update",
            json!({"source": "configured", "scope": "user",
                "expectedRevision": read["result"]["revision"], "document": {"greeting": "hi"}}),
        )
        .await;
    assert_ok(&updated);
    assert_ok(
        &client
            .rpc("plugin.reload", json!({"id": "configured"}))
            .await,
    );
    let kept = client
        .rpc(
            "settings.read",
            json!({"source": "configured", "scope": "user"}),
        )
        .await;
    assert_eq!(
        kept["result"]["document"],
        json!({"greeting": "hi"}),
        "{kept}"
    );
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

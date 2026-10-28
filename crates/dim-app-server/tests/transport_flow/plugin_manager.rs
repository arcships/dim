//! PluginManager (docs/design/plugins/PluginManager.md): `plugins.json`,
//! the `plugin.*` methods and the loader rows they command.
use super::session_typescript::{fixture, start, submit, when_served};
use super::*;
use std::path::{Path, PathBuf};

fn home() -> PathBuf {
    PathBuf::from("/tmp").join(format!(
        "dm-pm-{}",
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    ))
}

/// A server on `home` whose default configuration locks the protocol rows
/// and PluginManager.
fn config(home: &Path) -> ServerConfig {
    std::fs::create_dir_all(home).unwrap();
    let defaults = home.join("defaults.json");
    std::fs::write(
        &defaults,
        r#"{"plugins":{},"locked":["PluginManager","Rpc","Transport"]}"#,
    )
    .unwrap();
    ServerConfig {
        product_home: Some(home.to_owned()),
        target_home: Some(home.join("rust")),
        plugin_defaults: Some(defaults),
        ..Default::default()
    }
}

async fn call(client: &mut Client, method: &str, params: Value) -> Value {
    when_served(client, method, params).await
}

fn app_error(reply: &Value) -> &str {
    reply["error"]["data"]["appError"]
        .as_str()
        .unwrap_or_default()
}

fn item<'a>(list: &'a Value, name: &str) -> &'a Value {
    list["result"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["name"] == name)
        .unwrap_or_else(|| panic!("{name} in {list}"))
}

fn plugin_dir(root: &Path, name: &str) -> PathBuf {
    let dir = root.join(name);
    std::fs::create_dir_all(dir.join("skills")).unwrap();
    std::fs::write(
        dir.join("dim-plugin.json"),
        json!({"name": name, "skills": "skills"}).to_string(),
    )
    .unwrap();
    dir
}

fn git(dir: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .current_dir(dir)
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

/// Makes `ssh://fixture/<path>` reach the repository at `<path>` on this
/// machine: Git runs this stand-in for ssh, which runs the remote command
/// locally. Only Git child processes read these variables.
fn ssh_stand_in() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let script = std::env::temp_dir().join(format!("dm-ssh-{}", std::process::id()));
        std::fs::write(&script, "#!/bin/sh\nexec sh -c \"$2\"\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        // SAFETY: set once, before this test starts Git; nothing else in
        // the process reads these variables.
        unsafe {
            std::env::set_var("GIT_SSH_COMMAND", &script);
            std::env::set_var("GIT_SSH_VARIANT", "simple");
        }
    });
}

fn plugins_entries(home: &Path) -> Vec<String> {
    match std::fs::read_dir(home.join("plugins")) {
        Ok(entries) => entries
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect(),
        Err(_) => vec![],
    }
}

/// Acceptance 6–9 for plugins without code, and the record surviving a
/// restart (acceptance 5): built-ins turned off and back on, refusals that
/// change nothing, and a Git plugin installed and updated over ssh.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn plugin_configuration_installs_switches_and_survives_a_restart() {
    ssh_stand_in();
    let home = home();
    let config = config(&home);
    let (server, _, _) = start(config.clone()).await;
    let mut client = Client::new_version(&server, false, 3).await;

    let list = call(&mut client, "plugin.list", json!({})).await;
    let manager = item(&list, "PluginManager");
    assert_eq!(manager["locked"], true, "{manager}");
    assert_eq!(manager["use"], "builtin");
    assert_eq!(manager["status"], "running");
    assert_eq!(item(&list, "PermissionRules")["status"], "running");
    let listed = list["result"].as_array().unwrap();
    assert!(listed.iter().all(|item| item["name"] != "agent"), "{list}");

    // Acceptance 7.
    let refused = call(
        &mut client,
        "plugin.disable",
        json!({"name":"PluginManager"}),
    )
    .await;
    assert_eq!(app_error(&refused), "plugin_locked", "{refused}");

    // Acceptance 8.
    let refused = call(
        &mut client,
        "plugin.install",
        json!({"source":{"git":"file:///tmp/x.git"}}),
    )
    .await;
    assert_eq!(app_error(&refused), "invalid_source", "{refused}");
    let source = plugin_dir(&home.join("src"), "notes");
    let refused = call(
        &mut client,
        "plugin.install",
        json!({"source":{"path":source},"id":"../x"}),
    )
    .await;
    assert_eq!(app_error(&refused), "invalid_id", "{refused}");
    let refused = call(
        &mut client,
        "plugin.install",
        json!({"source":{"git":"ssh://fixture/nonexistent/repo.git"}}),
    )
    .await;
    assert_eq!(app_error(&refused), "acquire_failed", "{refused}");
    assert!(
        plugins_entries(&home).is_empty(),
        "{:?}",
        plugins_entries(&home)
    );
    assert!(!home.join("plugins.json").exists());

    let installed = call(
        &mut client,
        "plugin.install",
        json!({"source":{"path":source}}),
    )
    .await;
    assert_eq!(installed["result"]["id"], "notes", "{installed}");
    assert_eq!(installed["result"]["enabled"], true);
    assert_eq!(installed["result"]["skipped"], json!(["skills"]));
    let again = call(
        &mut client,
        "plugin.install",
        json!({"source":{"path":source}}),
    )
    .await;
    assert_eq!(app_error(&again), "plugin_exists", "{again}");

    // Acceptance 6.
    let replacement = plugin_dir(&home.join("src"), "PermissionRules");
    let installed = call(
        &mut client,
        "plugin.install",
        json!({"source":{"path":replacement},"id":"rules-plus"}),
    )
    .await;
    assert_eq!(installed["result"]["enabled"], true, "{installed}");
    let list = call(&mut client, "plugin.list", json!({})).await;
    assert_eq!(
        item(&list, "PermissionRules")["use"],
        "rules-plus",
        "{list}"
    );
    let reply = client.rpc("permission.rules", json!({})).await;
    assert!(reply.get("error").is_some(), "{reply}");
    let back = call(
        &mut client,
        "plugin.enable",
        json!({"name":"PermissionRules","use":"builtin"}),
    )
    .await;
    assert_eq!(back["result"]["status"], "running", "{back}");
    assert_ok(&call(&mut client, "permission.rules", json!({})).await);

    // Acceptance 9.
    let repo = home.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(
        repo.join("dim-plugin.json"),
        r#"{"name":"git-notes","skills":"skills"}"#,
    )
    .unwrap();
    std::fs::create_dir_all(repo.join("skills")).unwrap();
    std::fs::write(repo.join("skills/one.md"), "one").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "one"]);
    let first = git(&repo, &["rev-parse", "HEAD"]);
    let url = format!("ssh://fixture{}", repo.display());
    let installed = call(&mut client, "plugin.install", json!({"source":{"git":url}})).await;
    assert_eq!(installed["result"]["id"], "git-notes", "{installed}");
    let first_dir = home.join("plugins/git-notes").join(&first);
    assert!(first_dir.join("dim-plugin.json").is_file());
    std::fs::write(repo.join("skills/two.md"), "two").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "two"]);
    let second = git(&repo, &["rev-parse", "HEAD"]);
    let updated = call(&mut client, "plugin.update", json!({"id":"git-notes"})).await;
    assert_eq!(updated["result"]["commit"], second.as_str(), "{updated}");
    assert!(
        home.join("plugins/git-notes")
            .join(&second)
            .join("skills/two.md")
            .is_file()
    );
    assert!(!first_dir.exists());
    let local = call(&mut client, "plugin.update", json!({"id":"notes"})).await;
    assert_eq!(app_error(&local), "not_updatable", "{local}");
    let pinned = call(
        &mut client,
        "plugin.update",
        json!({"id":"git-notes","ref":"main"}),
    )
    .await;
    assert_eq!(pinned["result"]["commit"], second.as_str(), "{pinned}");
    let record: Value =
        serde_json::from_slice(&std::fs::read(home.join("plugins.json")).unwrap()).unwrap();
    let entry = record["installed"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["id"] == "git-notes");
    assert_eq!(entry.unwrap()["source"]["ref"], "main", "{record}");

    let kept = plugin_dir(&home.join("plugins"), "kept");
    std::fs::create_dir_all(kept.join("abcdef1234")).unwrap();
    assert_ok(
        &call(
            &mut client,
            "plugin.install",
            json!({"source":{"path":kept}}),
        )
        .await,
    );

    // Acceptance 5.
    assert_ok(
        &call(
            &mut client,
            "plugin.disable",
            json!({"name":"PermissionRules"}),
        )
        .await,
    );
    let record: Value =
        serde_json::from_slice(&std::fs::read(home.join("plugins.json")).unwrap()).unwrap();
    assert_eq!(
        record["names"]["PermissionRules"],
        json!({"use": "builtin", "off": true}),
        "{record}"
    );
    drop(client);
    server.shutdown();
    server.join().await.unwrap();

    let (server, _, _) = start(config.clone()).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let list = call(&mut client, "plugin.list", json!({})).await;
    let rules = item(&list, "PermissionRules");
    assert_eq!(rules["off"], true, "{rules}");
    assert_eq!(rules["status"], "off");
    let reply = client.rpc("permission.rules", json!({})).await;
    assert!(reply.get("error").is_some(), "{reply}");
    assert!(kept.join("dim-plugin.json").is_file());
    assert!(kept.join("abcdef1234").is_dir());
    assert!(home.join("plugins/git-notes").join(&second).is_dir());
    assert_ok(
        &call(
            &mut client,
            "plugin.enable",
            json!({"name":"PermissionRules"}),
        )
        .await,
    );

    std::fs::write(source.join("dim-plugin.json"), "{").unwrap();
    let list = call(&mut client, "plugin.list", json!({})).await;
    let notes = item(&list, "notes");
    assert_eq!(notes["status"], "failed", "{notes}");
    assert!(
        notes["error"]
            .as_str()
            .unwrap()
            .contains("invalid_manifest"),
        "{notes}"
    );
    assert_ok(&call(&mut client, "permission.rules", json!({})).await);

    assert_ok(&call(&mut client, "plugin.remove", json!({"id":"git-notes"})).await);
    assert!(!home.join("plugins/git-notes").exists());
    assert_ok(&call(&mut client, "plugin.remove", json!({"id":"notes"})).await);
    assert!(source.join("dim-plugin.json").is_file());
    assert_ok(&call(&mut client, "plugin.remove", json!({"id":"kept"})).await);
    assert!(kept.join("dim-plugin.json").is_file());
    let builtin = call(&mut client, "plugin.remove", json!({"id":"builtin"})).await;
    assert_eq!(app_error(&builtin), "not_removable", "{builtin}");
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// The path rule holds when a directory PluginManager would create under
/// lies below a symbolic link: a Git install is refused and nothing is
/// created outside `<home>/plugins`, with the link at `.staging` or at
/// `plugins` itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn plugin_storage_refuses_directories_below_symbolic_links() {
    ssh_stand_in();
    let home = home();
    let config = config(&home);
    let outside = PathBuf::from("/tmp").join(format!(
        "dm-out-{}",
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    ));
    std::fs::create_dir_all(&outside).unwrap();
    let repo = home.join("repo");
    std::fs::create_dir_all(repo.join("skills")).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(
        repo.join("dim-plugin.json"),
        r#"{"name":"linked","skills":"skills"}"#,
    )
    .unwrap();
    std::fs::write(repo.join("skills/one.md"), "one").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "one"]);
    let url = format!("ssh://fixture{}", repo.display());

    std::fs::create_dir_all(home.join("plugins")).unwrap();
    std::os::unix::fs::symlink(&outside, home.join("plugins/.staging")).unwrap();
    for round in 0..2 {
        if round == 1 {
            std::fs::remove_dir_all(home.join("plugins")).unwrap();
            std::os::unix::fs::symlink(&outside, home.join("plugins")).unwrap();
        }
        let (server, _, _) = start(config.clone()).await;
        let mut client = Client::new_version(&server, false, 3).await;
        let refused = call(&mut client, "plugin.install", json!({"source":{"git":url}})).await;
        assert_eq!(app_error(&refused), "path_outside_plugins", "{refused}");
        assert_eq!(
            std::fs::read_dir(&outside).unwrap().count(),
            0,
            "round {round}"
        );
        drop(client);
        server.shutdown();
        server.join().await.unwrap();
    }
    std::fs::remove_dir_all(&outside).unwrap();
    std::fs::remove_file(home.join("plugins")).unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// A TypeScript plugin of one tool, importing the SDK from where the tests
/// find it; `text` is what its tool answers.
fn write_tool_plugin(dir: &Path, name: &str, tool: &str, text: &str) {
    std::fs::create_dir_all(dir).unwrap();
    let sdk = std::env::var("DIM_TEST_RUTIS_SDK").expect("DIM_TEST_RUTIS_SDK");
    std::fs::write(
        dir.join("dim-plugin.json"),
        json!({"name": name, "code": {"entry": "index.ts", "inject": ["partRegistry"]}})
            .to_string(),
    )
    .unwrap();
    std::fs::write(
        dir.join("index.ts"),
        format!(
            r#"import {{ definePlugin }} from {sdk:?}
const part = definePlugin({{
  name: 'ToolPart',
  inject: ['toolRegistry'],
  async apply(ctx: any) {{
    await ctx.use('toolRegistry').register(
      {{ name: {tool:?}, description: 'test tool', inputSchema: {{ type: 'object' }}, source: 'extension', risk: 'session' }},
      async () => ({{ content: {text:?} }}),
    )
  }},
}})
export default definePlugin({{
  name: {name:?},
  inject: ['partRegistry'],
  async apply(ctx: any) {{
    await ctx.use('partRegistry').registerDefinition('session', 'ToolPart', part)
  }},
}})
"#
        ),
    )
    .unwrap();
}

async fn last_tool_text(server: &RunningServer, session: &str) -> String {
    let session = server.agent().unwrap().get_session(session).unwrap();
    let history = session.history("main", None).await.unwrap();
    let result = history
        .iter()
        .rev()
        .find_map(|event| match &event.kind {
            EventKind::ToolResult(batch) => batch.results.first(),
            _ => None,
        })
        .unwrap();
    let dim_agent_types::ToolContent::Text(content) = &result.result.content else {
        panic!("text result")
    };
    content.clone()
}

/// Acceptance 2, 3, 5 and 6 with code: a TypeScript plugin installed from
/// a local path, its tool used, edited and reloaded in the open Session,
/// replacing a built-in, turned off and on.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires DIM_TEST_NODE and DIM_TEST_JS_RUNTIME"]
async fn node_plugin_manager_installs_reloads_and_replaces_typescript_plugins() {
    let home = home();
    let mut config = config(&home);
    config.product_home = None;
    let mut parts = fixture();
    parts.rows.clear();
    config.session_typescript = Some(parts);
    let (server, mut requests, _) = start(config).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let source = home.join("src/my-tool");
    write_tool_plugin(&source, "my-tool", "my_echo", "version one");

    let installed = call(
        &mut client,
        "plugin.install",
        json!({"source":{"path":source}}),
    )
    .await;
    assert_eq!(installed["result"]["status"], "running", "{installed}");
    let mut params = create(json!(["my_echo", "echo"]));
    params["permissionPolicy"]["allowedTools"] = json!(["my_echo", "echo"]);
    let created = client.rpc("session.create", params).await;
    assert_ok(&created);
    let session = created["result"]["sessionId"].as_str().unwrap().to_owned();
    submit(&mut client, &session, "tool:my_echo {}").await;
    assert!(requests.recv().await.unwrap().contains(&"my_echo".into()));
    assert_eq!(last_tool_text(&server, &session).await, "version one");

    // Acceptance 3.
    write_tool_plugin(&source, "my-tool", "my_echo", "the second version");
    let reloaded = call(&mut client, "plugin.reload", json!({"name":"my-tool"})).await;
    assert_eq!(reloaded["result"]["status"], "running", "{reloaded}");
    submit(&mut client, &session, "tool:my_echo {}").await;
    while requests.try_recv().is_ok() {}
    assert_eq!(
        last_tool_text(&server, &session).await,
        "the second version"
    );

    // Acceptance 5.
    assert_ok(&call(&mut client, "plugin.disable", json!({"name":"my-tool"})).await);
    submit(&mut client, &session, "plain").await;
    let tools = requests.recv().await.unwrap();
    assert!(!tools.contains(&"my_echo".into()), "{tools:?}");
    let enabled = call(&mut client, "plugin.enable", json!({"name":"my-tool"})).await;
    assert_eq!(enabled["result"]["status"], "running", "{enabled}");

    let list = call(&mut client, "plugin.list", json!({})).await;
    assert_eq!(item(&list, "my-tool")["status"], "running", "{list}");

    // Acceptance 4.
    let agent_source = home.join("src/agent-tool");
    write_tool_plugin(&agent_source, "agent-tool", "agent_echo", "first");
    let tools = json!([
        "plugin_install",
        "plugin_reload",
        "plugin_list",
        "agent_echo"
    ]);
    let mut params = create(tools.clone());
    params["permissionPolicy"]["allowedTools"] = tools;
    let created = client.rpc("session.create", params).await;
    assert_ok(&created);
    let agent = created["result"]["sessionId"].as_str().unwrap().to_owned();
    let install = json!({"source": {"path": agent_source}});
    submit(
        &mut client,
        &agent,
        &format!("tool:plugin_install {install}"),
    )
    .await;
    let reply: Value = serde_json::from_str(&last_tool_text(&server, &agent).await).unwrap();
    assert_eq!(reply["status"], "running", "{reply}");
    submit(&mut client, &agent, "tool:agent_echo {}").await;
    assert_eq!(last_tool_text(&server, &agent).await, "first");
    write_tool_plugin(&agent_source, "agent-tool", "agent_echo", "after the edit");
    submit(
        &mut client,
        &agent,
        r#"tool:plugin_reload {"name":"agent-tool"}"#,
    )
    .await;
    submit(&mut client, &agent, "tool:agent_echo {}").await;
    assert_eq!(last_tool_text(&server, &agent).await, "after the edit");
    drop(client);
    server.shutdown();
    server.join().await.unwrap();

    // Acceptance 6.
    let mut config = self::config(&home);
    let mut parts = fixture();
    parts.rows.clear();
    config.session_typescript = Some(parts);
    let (server, _, _) = start(config).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let replacement = home.join("src/rules");
    write_tool_plugin(&replacement, "PermissionRules", "rules_echo", "replaced");
    let installed = call(
        &mut client,
        "plugin.install",
        json!({"source":{"path":replacement},"id":"rules-ts"}),
    )
    .await;
    assert_eq!(installed["result"]["status"], "running", "{installed}");
    let data = home.join("rules-ts");
    assert!(std::fs::symlink_metadata(&data).unwrap().is_dir());
    let reply = client.rpc("permission.rules", json!({})).await;
    assert!(reply.get("error").is_some(), "{reply}");

    let outside = home.join("../").join(format!(
        "{}-outside",
        home.file_name().unwrap().to_string_lossy()
    ));
    std::fs::create_dir_all(&outside).unwrap();
    std::os::unix::fs::symlink(&outside, home.join("linked")).unwrap();
    let linked = home.join("src/linked");
    write_tool_plugin(&linked, "linked", "linked_echo", "linked");
    let installed = call(
        &mut client,
        "plugin.install",
        json!({"source":{"path":linked}}),
    )
    .await;
    assert_eq!(installed["result"]["status"], "running", "{installed}");
    assert!(
        std::fs::symlink_metadata(home.join("linked"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 0);
    std::fs::remove_dir_all(&outside).unwrap();
    let back = call(
        &mut client,
        "plugin.enable",
        json!({"name":"PermissionRules","use":"builtin"}),
    )
    .await;
    assert_eq!(back["result"]["status"], "running", "{back}");
    assert_ok(&call(&mut client, "permission.rules", json!({})).await);
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// Polls `plugin.list` until every name in `names` is running.
async fn running(client: &mut Client, names: &[&str]) -> Value {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        let list = call(client, "plugin.list", json!({})).await;
        if names
            .iter()
            .all(|name| item(&list, name)["status"] == "running")
        {
            return list;
        }
        if tokio::time::Instant::now() > deadline {
            let items: Vec<&Value> = names.iter().map(|name| item(&list, name)).collect();
            panic!("{names:?} do not run: {items:#?}");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Acceptance 1 and 10: the default configuration file loads the default
/// plugins, and a default plugin's Settings source is there while it runs.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires DIM_TEST_NODE and DIM_TEST_JS_RUNTIME"]
async fn node_default_plugins_load_and_their_settings_follow_them() {
    let home = home();
    let mut config = config(&home);
    let mut parts = fixture();
    parts.rows.clear();
    config.plugin_defaults = Some(parts.anchor.with_file_name("default-plugins.json"));
    config.session_typescript = Some(parts);
    let (server, _, _) = start(config).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let defaults = ["goal", "web", "session-title", "workflow", "scheduler"];
    let list = running(&mut client, &defaults).await;
    for name in defaults {
        assert_eq!(item(&list, name)["use"], "default", "{list}");
    }
    assert_eq!(item(&list, "Agents")["use"], "builtin", "{list}");

    let read = json!({"source": "session-title", "scope": "user"});
    assert_ok(&call(&mut client, "settings.read", read.clone()).await);
    assert_ok(
        &call(
            &mut client,
            "plugin.disable",
            json!({"name": "session-title"}),
        )
        .await,
    );
    let gone = client.rpc("settings.read", read.clone()).await;
    assert_eq!(app_error(&gone), "settings_unknown_source", "{gone}");
    assert_ok(
        &call(
            &mut client,
            "plugin.enable",
            json!({"name": "session-title"}),
        )
        .await,
    );
    running(&mut client, &["session-title"]).await;
    assert_ok(&call(&mut client, "settings.read", read).await);
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// A Python plugin of one tool answering `text`, in one module.
fn write_python_plugin(dir: &Path, text: &str) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(
        dir.join("dim-plugin.json"),
        json!({"name": "py-tool", "code": {"entry": "plugin.py", "inject": ["partRegistry"]}})
            .to_string(),
    )
    .unwrap();
    std::fs::write(
        dir.join("plugin.py"),
        format!(
            r#"from rutis import define_plugin

inject = ["partRegistry"]
provides = {{}}


async def part_apply(ctx, config):
    async def execute(call, cancelled):
        return {{"content": {text:?}}}

    await ctx.use("toolRegistry").register({{
        "name": "py_tool", "description": "test tool", "inputSchema": {{"type": "object"}},
        "source": "extension", "risk": "session",
    }}, execute)


part = define_plugin(part_apply, inject=["toolRegistry"], name="PyToolPart")


async def apply(ctx, config):
    await ctx.use("partRegistry").registerDefinition("session", "PyToolPart", part)
"#
        ),
    )
    .unwrap();
}

/// Acceptance 11: a Python plugin installed from a local path is imported
/// through its link under `<home>/plugins/.python/`, the Python runtime's
/// import path, registers its tool, and reload picks up an edit.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires DIM_TEST_NODE and DIM_TEST_JS_RUNTIME"]
async fn node_python_plugin_installs_and_reloads_through_its_link() {
    let home = home();
    let mut config = config(&home);
    config.product_home = None;
    let python = home.join("rust/plugins/.python");
    std::fs::create_dir_all(&python).unwrap();
    let mut parts = fixture();
    parts.rows.clear();
    parts.python = Some(dim_app_server::session_typescript::PythonConfig {
        interpreter: std::env::var_os("DIM_TEST_PYTHON")
            .expect("DIM_TEST_PYTHON")
            .into(),
        project: python.canonicalize().unwrap(),
        packages: None,
    });
    config.session_typescript = Some(parts);
    let (server, _requests, _) = start(config).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let source = home.join("src/py-tool");
    write_python_plugin(&source, "python one");

    let installed = call(
        &mut client,
        "plugin.install",
        json!({"source":{"path":source}}),
    )
    .await;
    assert_eq!(installed["result"]["status"], "running", "{installed}");
    let link = python.join("py_tool");
    assert_eq!(
        std::fs::read_link(&link).unwrap(),
        source.canonicalize().unwrap()
    );
    let mut params = create(json!(["py_tool"]));
    params["permissionPolicy"]["allowedTools"] = json!(["py_tool"]);
    let created = client.rpc("session.create", params).await;
    assert_ok(&created);
    let session = created["result"]["sessionId"].as_str().unwrap().to_owned();
    submit(&mut client, &session, "tool:py_tool {}").await;
    assert_eq!(last_tool_text(&server, &session).await, "python one");

    write_python_plugin(&source, "python after the edit");
    let reloaded = call(&mut client, "plugin.reload", json!({"name":"py-tool"})).await;
    assert_eq!(reloaded["result"]["status"], "running", "{reloaded}");
    submit(&mut client, &session, "tool:py_tool {}").await;
    assert_eq!(
        last_tool_text(&server, &session).await,
        "python after the edit"
    );

    assert_ok(&call(&mut client, "plugin.remove", json!({"id":"py-tool"})).await);
    assert!(std::fs::symlink_metadata(&link).is_err());
    assert!(source.join("plugin.py").is_file());
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

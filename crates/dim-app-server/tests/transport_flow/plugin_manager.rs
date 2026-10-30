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
    assert!(installed["result"].get("skipped").is_none(), "{installed}");
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
    last_tool_result(server, session).await.0
}

/// The text of the last tool result in `session` and whether it is an error.
async fn last_tool_result(server: &RunningServer, session: &str) -> (String, bool) {
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
    (content.clone(), result.result.is_error == Some(true))
}

/// Runs `command` through the `plugin` tool in `session`.
async fn plugin(
    client: &mut Client,
    server: &RunningServer,
    session: &str,
    command: &str,
) -> (String, bool) {
    let input = json!({ "command": command });
    submit(client, session, &format!("tool:plugin {input}")).await;
    last_tool_result(server, session).await
}

/// The JSON reply of a `plugin` command that succeeded.
async fn plugin_reply(
    client: &mut Client,
    server: &RunningServer,
    session: &str,
    command: &str,
) -> Value {
    let (text, error) = plugin(client, server, session, command).await;
    assert!(!error, "{command}: {text}");
    serde_json::from_str(&text).unwrap_or_else(|_| panic!("{command}: {text}"))
}

fn session_with(policy: Value, cwd: &Path) -> Value {
    json!({"cwd": cwd, "permissionPolicy": policy, "tools": ["plugin"]})
}

/// §6: the `plugin` tool routes each command to the `plugin.*` method of
/// its name with the same result and errors; `help` and a malformed command
/// give usage; Permission decides each call, admitting `list` and `help`
/// under the read-only preset and not `config`, whose documents may hold
/// secrets; the default plugin-authoring Skill
/// is listed.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn plugin_tool_runs_plugin_commands() {
    let home = home();
    let mut config = config(&home);
    let authoring = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../runtime/plugins/plugin-authoring")
        .canonicalize()
        .unwrap();
    std::fs::write(
        config.plugin_defaults.as_ref().unwrap(),
        json!({"plugins": {"plugin-authoring": authoring}, "locked": ["PluginManager", "Rpc", "Transport"]})
            .to_string(),
    )
    .unwrap();
    config.unix_path = Some(home.join("ipc/server.sock"));
    let server = serve(product_builder(), config).await.unwrap();
    let mut client = Client::new_version(&server, false, 3).await;
    select_fixture_model(&mut client).await;
    let work = home.join("work");
    plugin_dir(&work, "content");

    let full = json!({"preset": "full-access", "allowedTools": [], "disallowedTools": []});
    let created = client
        .rpc("session.create", session_with(full, &work))
        .await;
    assert_ok(&created);
    let session = created["result"]["sessionId"].as_str().unwrap().to_owned();
    let skills = plugin_skill_ids(&mut client).await;
    assert!(
        skills.iter().any(|id| id.ends_with("plugin-authoring")),
        "{skills:?}"
    );

    let (help, error) = plugin(&mut client, &server, &session, "help").await;
    assert!(
        !error && help.starts_with("Commands: list | install"),
        "{help}"
    );
    let (help, error) = plugin(&mut client, &server, &session, "help config").await;
    assert!(
        !error && help.starts_with("Usage: config <name> [set <key> <value>]"),
        "{help}"
    );
    let (usage, error) = plugin(&mut client, &server, &session, "enable").await;
    assert!(
        error && usage.starts_with("Usage: enable <name>"),
        "{usage}"
    );
    let (usage, error) = plugin(&mut client, &server, &session, "install a b").await;
    assert!(error && usage.starts_with("Usage: install"), "{usage}");
    let (refused, error) =
        plugin(&mut client, &server, &session, r"install C:\work\my-plugin").await;
    assert!(
        error && refused.starts_with("invalid_source:") && refused.contains(r"C:\work\my-plugin"),
        "{refused}"
    );
    let (list, error) = plugin(&mut client, &server, &session, "frobnicate").await;
    assert!(error && list.starts_with("Commands:"), "{list}");

    let installed = plugin_reply(&mut client, &server, &session, "install content").await;
    assert_eq!(installed["id"], "content", "{installed}");
    let listed = plugin_reply(&mut client, &server, &session, "list").await;
    assert_eq!(
        listed,
        call(&mut client, "plugin.list", json!({})).await["result"]
    );
    assert_eq!(
        item(&json!({"result": listed}), "plugin-authoring")["use"],
        "default"
    );
    assert_eq!(
        plugin_reply(&mut client, &server, &session, "disable content").await,
        json!({})
    );
    let list = call(&mut client, "plugin.list", json!({})).await;
    assert_eq!(item(&list, "content")["off"], true, "{list}");
    plugin_reply(
        &mut client,
        &server,
        &session,
        "enable content --use content",
    )
    .await;
    plugin_reply(&mut client, &server, &session, "reload content").await;
    let (refused, error) = plugin(&mut client, &server, &session, "update content").await;
    let method = call(&mut client, "plugin.update", json!({"id": "content"})).await;
    assert!(
        error && refused.starts_with(&format!("{}:", app_error(&method))),
        "{refused} {method}"
    );
    let (refused, error) = plugin(&mut client, &server, &session, "disable PluginManager").await;
    assert!(error && refused.starts_with("plugin_locked:"), "{refused}");
    assert_eq!(
        plugin_reply(&mut client, &server, &session, "config content").await,
        json!({"name": "content", "sources": []})
    );
    let (refused, error) = plugin(&mut client, &server, &session, "config content set a 1").await;
    assert!(error && refused.contains("no Settings source"), "{refused}");
    let (refused, error) = plugin(&mut client, &server, &session, "config missing").await;
    assert!(
        error && refused.starts_with("plugin_not_found:"),
        "{refused}"
    );
    assert_eq!(
        plugin_reply(&mut client, &server, &session, "remove content").await,
        json!({})
    );

    let read_only = json!({"preset": "read-only", "allowedTools": [], "disallowedTools": []});
    let created = client
        .rpc("session.create", session_with(read_only, &work))
        .await;
    assert_ok(&created);
    let watched = created["result"]["sessionId"].as_str().unwrap().to_owned();
    for command in ["list", "help"] {
        let (text, error) = plugin(&mut client, &server, &watched, command).await;
        assert!(!error, "{command}: {text}");
    }
    for command in ["install content", "config plugin-authoring"] {
        let (denied, error) = plugin(&mut client, &server, &watched, command).await;
        assert!(error && denied.contains("read-only"), "{command}: {denied}");
    }
    let list = call(&mut client, "plugin.list", json!({})).await;
    assert!(
        list["result"]
            .as_array()
            .unwrap()
            .iter()
            .all(|item| item["name"] != "content"),
        "{list}"
    );
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
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
    let tools = offered(&mut client, &mut requests, &session).await;
    assert!(tools.contains(&"my_echo".into()), "{tools:?}");

    let list = call(&mut client, "plugin.list", json!({})).await;
    assert_eq!(item(&list, "my-tool")["status"], "running", "{list}");

    // Acceptance 4.
    let agent_source = home.join("src/agent-tool");
    write_tool_plugin(&agent_source, "agent-tool", "agent_echo", "first");
    let tools = json!(["plugin", "agent_echo"]);
    let mut params = create(tools.clone());
    params["permissionPolicy"]["allowedTools"] = tools;
    let created = client.rpc("session.create", params).await;
    assert_ok(&created);
    let agent = created["result"]["sessionId"].as_str().unwrap().to_owned();
    let install = format!("install {}", agent_source.display());
    let reply = plugin_reply(&mut client, &server, &agent, &install).await;
    assert_eq!(reply["status"], "running", "{reply}");
    submit(&mut client, &agent, "tool:agent_echo {}").await;
    assert_eq!(last_tool_text(&server, &agent).await, "first");
    write_tool_plugin(&agent_source, "agent-tool", "agent_echo", "after the edit");
    plugin_reply(&mut client, &server, &agent, "reload agent-tool").await;
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
    let (server, _requests, _) = start(config).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let defaults = ["goal", "session-title", "workflow", "scheduler"];
    let list = running(&mut client, &defaults).await;
    for name in defaults {
        assert_eq!(item(&list, name)["use"], "default", "{list}");
    }
    assert_eq!(item(&list, "web")["use"], "builtin", "{list}");
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

    // §6 `config`: goal's Settings source through the `plugin` tool.
    select_fixture_model(&mut client).await;
    let tools = json!(["plugin"]);
    let mut params = create(tools.clone());
    params["permissionPolicy"]["allowedTools"] = tools;
    let created = client.rpc("session.create", params).await;
    assert_ok(&created);
    let session = created["result"]["sessionId"].as_str().unwrap().to_owned();
    let shown = plugin_reply(&mut client, &server, &session, "config goal").await;
    let source = &shown["sources"][0];
    assert_eq!(source["source"], "goal", "{shown}");
    assert_eq!(source["document"]["maxContinuations"], 20, "{shown}");
    assert_eq!(
        source["schema"]["properties"]["maxContinuations"]["maximum"],
        1000
    );
    let set = "config goal set maxContinuations 5";
    let updated = plugin_reply(&mut client, &server, &session, set).await;
    assert_eq!(updated["document"]["maxContinuations"], 5, "{updated}");
    let read = json!({"source": "goal", "scope": "user"});
    let stored = call(&mut client, "settings.read", read).await;
    assert_eq!(
        stored["result"]["document"]["maxContinuations"], 5,
        "{stored}"
    );
    let (refused, error) = plugin(
        &mut client,
        &server,
        &session,
        "config goal set maxContinuations 0",
    )
    .await;
    assert!(error && refused.contains("maxContinuations"), "{refused}");

    // A `writeOnly` member (web's search key and proxy) is a secret toward
    // the agent: never returned, never written through the tool.
    let read = json!({"source": "web", "scope": "user"});
    let current = call(&mut client, "settings.read", read.clone()).await;
    let secret = "dim-test-secret-key";
    let document = json!({"allowPrivateHosts": [], "proxy": "http://user:pass@127.0.0.1:9",
        "searchService": {"kind": "brave", "apiKey": secret}});
    assert_ok(
        &call(
            &mut client,
            "settings.update",
            json!({"source": "web", "scope": "user",
                "expectedRevision": current["result"]["revision"], "document": document}),
        )
        .await,
    );
    let (shown, error) = plugin(&mut client, &server, &session, "config web").await;
    assert!(
        !error && !shown.contains(secret) && !shown.contains("user:pass"),
        "{shown}"
    );
    let shown: Value = serde_json::from_str(&shown).unwrap();
    let web = &shown["sources"][0];
    assert_eq!(
        web["document"]["searchService"],
        json!({"kind": "brave"}),
        "{shown}"
    );
    assert_eq!(
        web["writeOnlySet"],
        json!(["/searchService/apiKey", "/proxy"]),
        "{shown}"
    );
    for set in [
        "config web set proxy http://127.0.0.1:8",
        r#"config web set searchService '{"kind":"brave"}'"#,
    ] {
        let (refused, error) = plugin(&mut client, &server, &session, set).await;
        assert!(error && refused.contains("write-only"), "{set}: {refused}");
    }
    let stored = call(&mut client, "settings.read", read).await;
    assert_eq!(stored["result"]["document"], document, "{stored}");
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// The tools offered to the model in a new turn of `session`.
async fn offered(
    client: &mut Client,
    requests: &mut tokio::sync::mpsc::UnboundedReceiver<Vec<String>>,
    session: &str,
) -> Vec<String> {
    while requests.try_recv().is_ok() {}
    submit(client, session, "plain").await;
    requests.recv().await.unwrap()
}

async fn web_session(client: &mut Client) -> String {
    let created = client
        .rpc("session.create", create(json!(["web_fetch", "echo"])))
        .await;
    assert_ok(&created);
    created["result"]["sessionId"].as_str().unwrap().to_owned()
}

/// Acceptance 5: `disable web` removes web's tools from the next turn of an
/// open Session; `enable web` offers them again there and in a new Session,
/// and the state survives a restart.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn web_tools_follow_disable_enable_and_restart() {
    let home = home();
    let (server, mut requests, _) = start(config(&home)).await;
    let mut client = Client::new_version(&server, false, 3).await;
    running(&mut client, &["web"]).await;
    select_fixture_model(&mut client).await;
    let open = web_session(&mut client).await;
    let web = "web_fetch".to_owned();
    let tools = offered(&mut client, &mut requests, &open).await;
    assert!(tools.contains(&web), "{tools:?}");

    assert_ok(&call(&mut client, "plugin.disable", json!({"name": "web"})).await);
    let tools = offered(&mut client, &mut requests, &open).await;
    assert!(!tools.contains(&web), "{tools:?}");
    let enabled = call(&mut client, "plugin.enable", json!({"name": "web"})).await;
    assert_eq!(enabled["result"]["status"], "running", "{enabled}");
    let tools = offered(&mut client, &mut requests, &open).await;
    assert!(tools.contains(&web), "the open Session: {tools:?}");
    let new = web_session(&mut client).await;
    let tools = offered(&mut client, &mut requests, &new).await;
    assert!(tools.contains(&web), "a new Session: {tools:?}");
    let reloaded = call(&mut client, "plugin.reload", json!({"name": "web"})).await;
    assert_eq!(app_error(&reloaded), "not_reloadable", "{reloaded}");
    let tools = offered(&mut client, &mut requests, &open).await;
    assert!(tools.contains(&web), "reloaded unchanged: {tools:?}");

    assert_ok(&call(&mut client, "plugin.disable", json!({"name": "web"})).await);
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    let (server, mut requests, _) = start(config(&home)).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let list = call(&mut client, "plugin.list", json!({})).await;
    assert_eq!(item(&list, "web")["status"], "off", "{list}");
    let session = web_session(&mut client).await;
    let tools = offered(&mut client, &mut requests, &session).await;
    assert!(!tools.contains(&web), "after a restart: {tools:?}");
    let enabled = call(&mut client, "plugin.enable", json!({"name": "web"})).await;
    assert_eq!(enabled["result"]["status"], "running", "{enabled}");
    let tools = offered(&mut client, &mut requests, &session).await;
    assert!(tools.contains(&web), "enabled after a restart: {tools:?}");
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

/// The product's local execution profile with mock model hosts: Sessions
/// mount Skills and MCP.
fn product_builder() -> AgentBuilder {
    let mut builder = AgentBuilder::new(AgentConfig::default());
    defaults::install_local_execution(
        &mut builder,
        dim_agent_capabilities::model_provider::ModelProviderPlugin::new,
        dim_agent_capabilities::workspace_tools::WorkspaceToolsConfig {
            output_root: std::env::temp_dir().join("dm-pm-outputs"),
        },
        dim_agent_capabilities::timeline_tree::BranchToolConfig::default(),
    );
    dim_app_server::permission_rules::install(&mut builder);
    defaults::mock_model_hosts(&mut builder);
    builder
}

/// Creates and selects a model, so a Session can be created; the mock
/// model host answers its requests.
async fn select_fixture_model(client: &mut Client) {
    let provider = "018ec73f-0000-7000-8000-000000000493";
    let capabilities = dim_agent_types::ModelCapabilities {
        context_window: 32000,
        max_output_tokens: 128,
        ..Default::default()
    };
    let model = json!({"modelId": "fixture", "displayName": "fixture", "status": "active",
        "capabilities": capabilities});
    assert_ok(
        &client
            .rpc(
                "provider.create",
                json!({"providerId": provider, "displayName": "fixture",
        "connection": {"driverKind": "openai-compatible", "baseUrl": "http://localhost/v1",
            "auth": {"type": "anonymous"}},
        "models": [model], "enabled": true}),
            )
            .await,
    );
    assert_ok(
        &client
            .rpc(
                "provider.select",
                json!({"providerId": provider, "modelId": "fixture"}),
            )
            .await,
    );
}

/// A plugin carrying `skills/<skill>/SKILL.md` for each of `skills` and an
/// `mcp.json` with `mcp` as its `mcpServers`.
fn write_content_plugin(dir: &Path, name: &str, skills: &[&str], mcp: Value) {
    let _ = std::fs::remove_dir_all(dir.join("skills"));
    for skill in skills {
        let skill_dir = dir.join("skills").join(skill);
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            format!("---\nname: {skill}\ndescription: The {skill} skill\n---\nDo {skill}.\n"),
        )
        .unwrap();
    }
    std::fs::create_dir_all(dir.join("skills")).unwrap();
    std::fs::write(dir.join("mcp.json"), json!({"mcpServers": mcp}).to_string()).unwrap();
    std::fs::write(
        dir.join("dim-plugin.json"),
        json!({"name": name, "skills": "skills", "mcp": "mcp.json"}).to_string(),
    )
    .unwrap();
}

/// What a new Session on `work` sees: the names of its plugin Skills and its
/// MCP server ids, once its MCP selection has settled.
async fn new_session_content(client: &mut Client, work: &Path) -> (Vec<String>, Vec<String>) {
    let created = client.rpc("session.create", json!({"cwd": work})).await;
    assert_ok(&created);
    let session = created["result"]["sessionId"].clone();
    let skills = when_served(
        client,
        "session.skills",
        json!({"sessionId": session, "branchId": "main"}),
    )
    .await;
    assert_ok(&skills);
    let mut names: Vec<String> = skills["result"]["skills"]
        .as_array()
        .unwrap_or_else(|| panic!("{skills}"))
        .iter()
        .filter(|skill| skill["id"].as_str().unwrap().starts_with("plugin:"))
        .map(|skill| skill["name"].as_str().unwrap().to_owned())
        .collect();
    names.sort();
    let expected =
        when_served(client, "mcp.catalog", json!({"cwd": work})).await["result"]["servers"]
            .as_array()
            .unwrap()
            .len();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let mcp = when_served(client, "session.mcp", json!({"sessionId": session})).await;
        assert_ok(&mcp);
        let servers: Vec<String> = mcp["result"]["servers"]
            .as_array()
            .unwrap_or_else(|| panic!("{mcp}"))
            .iter()
            .map(|server| server["id"].as_str().unwrap().to_owned())
            .collect();
        if servers.len() == expected {
            return (names, servers);
        }
        assert!(tokio::time::Instant::now() < deadline, "{mcp}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// §3.7 and R2: a plugin installed from a local path brings its Skills and
/// MCP servers to a new Session without a restart; turning it off removes
/// them and on restores them; reload picks up an edit; content an owner
/// refuses is reported while the rest registers; an absent owner skips
/// only its kind and gets it when turned on; owners keep it over a restart.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn plugin_content_reaches_sessions_through_its_owners() {
    let home = home();
    let mut config = config(&home);
    config.unix_path = Some(home.join("ipc/server.sock"));
    let server = serve(product_builder(), config.clone()).await.unwrap();
    let mut client = Client::new_version(&server, false, 3).await;
    select_fixture_model(&mut client).await;
    let work = home.join("work");
    std::fs::create_dir_all(&work).unwrap();
    let absent = json!({"command": "dim-test-absent-mcp-server"});
    let source = home.join("src/kit");
    write_content_plugin(&source, "kit", &["review"], json!({"docs": absent}));

    let installed = call(
        &mut client,
        "plugin.install",
        json!({"source":{"path":source}}),
    )
    .await;
    assert_eq!(installed["result"]["enabled"], true, "{installed}");
    assert!(installed["result"].get("skipped").is_none(), "{installed}");
    assert!(installed["result"].get("refused").is_none(), "{installed}");
    let catalog = client.rpc("mcp.catalog", json!({})).await;
    assert_eq!(
        catalog["result"]["servers"],
        json!([{"id":"kit:docs","source":"plugin","enabled":true,"transport":"stdio"}]),
        "{catalog}"
    );
    assert_eq!(
        new_session_content(&mut client, &work).await,
        (vec!["review".into()], vec!["kit:docs".into()])
    );

    assert_ok(&call(&mut client, "plugin.disable", json!({"name":"kit"})).await);
    assert_eq!(
        new_session_content(&mut client, &work).await,
        (vec![], vec![])
    );
    assert_ok(&call(&mut client, "plugin.enable", json!({"name":"kit"})).await);
    assert_eq!(
        new_session_content(&mut client, &work).await,
        (vec!["review".into()], vec!["kit:docs".into()])
    );

    write_content_plugin(
        &source,
        "kit",
        &["lint", "review"],
        json!({"docs": absent, "search": absent}),
    );
    let reloaded = call(&mut client, "plugin.reload", json!({"name":"kit"})).await;
    assert!(reloaded["result"].get("refused").is_none(), "{reloaded}");
    assert_eq!(
        new_session_content(&mut client, &work).await,
        (
            vec!["lint".into(), "review".into()],
            vec!["kit:docs".into(), "kit:search".into()]
        )
    );

    write_content_plugin(&source, "kit", &["lint"], json!({"docs": {"command": ""}}));
    std::fs::create_dir_all(source.join("skills/broken")).unwrap();
    std::fs::write(source.join("skills/broken/SKILL.md"), "no frontmatter").unwrap();
    let reloaded = call(&mut client, "plugin.reload", json!({"name":"kit"})).await;
    let kinds: Vec<&Value> = reloaded["result"]["refused"]
        .as_array()
        .unwrap_or_else(|| panic!("{reloaded}"))
        .iter()
        .map(|refused| &refused["kind"])
        .collect();
    assert_eq!(kinds, [&json!("skills"), &json!("mcp")], "{reloaded}");
    let list = call(&mut client, "plugin.list", json!({})).await;
    assert_eq!(
        item(&list, "kit")["refused"],
        reloaded["result"]["refused"],
        "{list}"
    );
    assert_eq!(
        new_session_content(&mut client, &work).await,
        (vec!["lint".into()], vec![])
    );
    write_content_plugin(&source, "kit", &["lint"], json!({"docs": absent}));
    let reloaded = call(&mut client, "plugin.reload", json!({"name":"kit"})).await;
    assert!(reloaded["result"].get("refused").is_none(), "{reloaded}");

    assert_ok(&call(&mut client, "plugin.disable", json!({"name":"Mcp"})).await);
    let other = home.join("src/extra");
    write_content_plugin(&other, "extra", &["extra"], json!({"tool": absent}));
    let installed = call(
        &mut client,
        "plugin.install",
        json!({"source":{"path":other}}),
    )
    .await;
    assert_eq!(
        installed["result"]["skipped"],
        json!(["mcp"]),
        "{installed}"
    );
    assert_ok(&call(&mut client, "plugin.enable", json!({"name":"Mcp"})).await);
    let catalog = when_served(&mut client, "mcp.catalog", json!({})).await;
    let ids: Vec<&Value> = catalog["result"]["servers"]
        .as_array()
        .unwrap_or_else(|| panic!("{catalog}"))
        .iter()
        .map(|server| &server["id"])
        .collect();
    assert_eq!(ids, [&json!("extra:tool"), &json!("kit:docs")], "{catalog}");
    drop(client);
    server.shutdown();
    server.join().await.unwrap();

    let server = serve(product_builder(), config).await.unwrap();
    let mut client = Client::new_version(&server, false, 3).await;
    when_served(&mut client, "plugin.list", json!({})).await;
    assert_eq!(
        new_session_content(&mut client, &work).await,
        (
            vec!["extra".into(), "lint".into()],
            vec!["extra:tool".into(), "kit:docs".into()]
        )
    );
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// The server ids of `mcp.catalog`.
async fn mcp_ids(client: &mut Client) -> Vec<String> {
    let catalog = when_served(client, "mcp.catalog", json!({})).await;
    catalog["result"]["servers"]
        .as_array()
        .unwrap_or_else(|| panic!("{catalog}"))
        .iter()
        .map(|server| server["id"].as_str().unwrap().to_owned())
        .collect()
}

/// The ids of the plugin Skills `skills.catalog` lists.
async fn plugin_skill_ids(client: &mut Client) -> Vec<String> {
    let catalog = when_served(client, "skills.catalog", json!({})).await;
    catalog["result"]["entries"]
        .as_array()
        .unwrap_or_else(|| panic!("{catalog}"))
        .iter()
        .map(|entry| entry["id"].as_str().unwrap().to_owned())
        .filter(|id| id.starts_with("plugin:"))
        .collect()
}

/// A plugin stdio entry runs inside its plugin directory: a `cwd` that is
/// absolute, climbs with `..`, or leaves through a symbolic link refuses
/// the plugin's MCP document, which is reported and not published.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn plugin_mcp_cwd_stays_inside_the_plugin() {
    let home = home();
    let config = config(&home);
    let (server, _, _) = start(config).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let source = home.join("src/paths");
    std::fs::create_dir_all(source.join("inside")).unwrap();
    std::fs::create_dir_all(home.join("outside")).unwrap();
    std::os::unix::fs::symlink(home.join("outside"), source.join("link")).unwrap();
    let entry = |cwd: &str| json!({"tool": {"command": "dim-test-absent-mcp-server", "cwd": cwd}});
    write_content_plugin(&source, "paths", &["paths"], entry("inside"));
    let installed = call(
        &mut client,
        "plugin.install",
        json!({"source":{"path":source}}),
    )
    .await;
    assert!(installed["result"].get("refused").is_none(), "{installed}");
    assert_eq!(mcp_ids(&mut client).await, ["paths:tool"]);

    let outside = home.join("outside").to_string_lossy().into_owned();
    for cwd in [
        outside.as_str(),
        "../outside",
        "inside/../../outside",
        "link",
    ] {
        write_content_plugin(&source, "paths", &["paths"], entry(cwd));
        let reloaded = call(&mut client, "plugin.reload", json!({"name":"paths"})).await;
        assert_eq!(
            reloaded["result"]["refused"][0]["kind"], "mcp",
            "{cwd}: {reloaded}"
        );
        assert!(mcp_ids(&mut client).await.is_empty(), "{cwd}");
        assert_eq!(plugin_skill_ids(&mut client).await, ["plugin:paths:paths"]);
        write_content_plugin(&source, "paths", &["paths"], entry("inside"));
        let reloaded = call(&mut client, "plugin.reload", json!({"name":"paths"})).await;
        assert!(reloaded["result"].get("refused").is_none(), "{reloaded}");
        assert_eq!(mcp_ids(&mut client).await, ["paths:tool"]);
    }
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// Startup brings the owners to the record (§3.7): content of a plugin
/// removed while its owner was off, or left behind when the process stopped
/// between writing `plugins.json` and commanding the owner, is withdrawn,
/// and an MCP refusal is reported again after a restart.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn plugin_content_follows_the_record_over_restarts() {
    let home = home();
    let config = config(&home);
    let absent = json!({"command": "dim-test-absent-mcp-server"});
    let gone = home.join("src/gone");
    write_content_plugin(&gone, "gone", &["gone"], json!({"tool": absent}));
    let stale = home.join("src/stale");
    write_content_plugin(&stale, "stale", &["stale"], json!({"tool": absent}));
    let broken = home.join("src/broken");
    write_content_plugin(&broken, "broken", &[], json!({"tool": {"command": ""}}));

    let (server, _, _) = start(config.clone()).await;
    let mut client = Client::new_version(&server, false, 3).await;
    for source in [&gone, &stale, &broken] {
        call(
            &mut client,
            "plugin.install",
            json!({"source":{"path":source}}),
        )
        .await;
    }
    assert_eq!(mcp_ids(&mut client).await, ["gone:tool", "stale:tool"]);
    assert_ok(&call(&mut client, "plugin.disable", json!({"name":"Mcp"})).await);
    assert_ok(
        &call(
            &mut client,
            "plugin.disable",
            json!({"name":"SkillsCatalog"}),
        )
        .await,
    );
    assert_ok(&call(&mut client, "plugin.remove", json!({"id":"gone"})).await);
    assert_ok(
        &call(
            &mut client,
            "plugin.enable",
            json!({"name":"SkillsCatalog"}),
        )
        .await,
    );
    assert_eq!(plugin_skill_ids(&mut client).await, ["plugin:stale:stale"]);
    drop(client);
    server.shutdown();
    server.join().await.unwrap();

    let (server, _, _) = start(config.clone()).await;
    let mut client = Client::new_version(&server, false, 3).await;
    assert_ok(&call(&mut client, "plugin.enable", json!({"name":"Mcp"})).await);
    assert_eq!(mcp_ids(&mut client).await, ["stale:tool"]);
    let list = call(&mut client, "plugin.list", json!({})).await;
    assert_eq!(item(&list, "broken")["refused"][0]["kind"], "mcp", "{list}");
    drop(client);
    server.shutdown();
    server.join().await.unwrap();

    let record = home.join("plugins.json");
    let mut written: Value = serde_json::from_slice(&std::fs::read(&record).unwrap()).unwrap();
    written["installed"]
        .as_array_mut()
        .unwrap()
        .retain(|entry| entry["id"] != "stale");
    written["names"].as_object_mut().unwrap().remove("stale");
    std::fs::write(&record, written.to_string()).unwrap();
    let (server, _, _) = start(config).await;
    let mut client = Client::new_version(&server, false, 3).await;
    assert!(mcp_ids(&mut client).await.is_empty());
    assert!(plugin_skill_ids(&mut client).await.is_empty());
    let list = call(&mut client, "plugin.list", json!({})).await;
    assert_eq!(item(&list, "broken")["refused"][0]["kind"], "mcp", "{list}");
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

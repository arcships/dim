//! PluginManager (docs/design/plugins/PluginManager.md): `plugins.json`,
//! the `plugin.*` methods and the loader rows they command.
use super::runtime_manager::FixtureNode;
use super::session_typescript::{fixture, setup, start, submit, when_served};
use super::*;
use dim_agent_runtime::testing::chain_events;
use dim_app_server::{
    plugin_manager::Defaults,
    runtime_manager::{Environment, Kind, Runtimes},
    session_typescript::SessionTypeScriptConfig,
};
use std::path::{Path, PathBuf};

pub(super) fn home() -> PathBuf {
    PathBuf::from("/tmp").join(format!(
        "dm-pm-{}",
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    ))
}

/// A server on `home` whose default configuration locks the protocol rows
/// and PluginManager.
pub(super) fn config(home: &Path) -> ServerConfig {
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

pub(super) fn git(dir: &Path, args: &[&str]) -> String {
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
pub(super) fn ssh_stand_in() {
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

/// The record `plugins.json` holds.
fn record(home: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(home.join("plugins.json")).unwrap()).unwrap()
}

/// The directory the record names for `id`.
fn recorded_dir(home: &Path, id: &str) -> PathBuf {
    let record = record(home);
    let entry = record["installed"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["id"] == id)
        .unwrap_or_else(|| panic!("{id} in {record}"));
    PathBuf::from(entry["dir"].as_str().unwrap())
}

/// The name of every directory under `<home>/plugins/<id>`, sorted.
fn copies(home: &Path, id: &str) -> Vec<String> {
    match std::fs::read_dir(home.join("plugins").join(id)) {
        Ok(entries) => {
            let mut names: Vec<String> = entries
                .flatten()
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .collect();
            names.sort();
            names
        }
        Err(_) => Vec::new(),
    }
}

/// The last path component of `dir`.
fn name_of(dir: &Path) -> String {
    dir.file_name().unwrap().to_string_lossy().into_owned()
}

/// `<key>+<16 lowercase hex>`: one acquisition's own directory (§3.2); the
/// key is a Git commit or an optional plugin's version.
fn acquisition_name(name: &str) -> bool {
    let Some((key, suffix)) = name.split_once('+') else {
        return false;
    };
    let commit = key.len() >= 7 && key.bytes().all(|byte| byte.is_ascii_hexdigit());
    let version = key.split('.').count() == 3
        && key
            .split('.')
            .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()));
    (commit || version)
        && suffix.len() == 16
        && suffix
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// A Git repository holding the `git-notes` skills plugin, committed once.
fn git_notes_repo(home: &Path) -> PathBuf {
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
    repo
}

/// One more commit in `repo`; returns its hash.
fn second_commit(repo: &Path) -> String {
    std::fs::write(repo.join("skills/two.md"), "two").unwrap();
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-qm", "two"]);
    git(repo, &["rev-parse", "HEAD"])
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plugin_client_replies_hide_git_credentials_and_runtime_errors() {
    let home = home();
    let config = config(&home);
    let dir = plugin_dir(&home.join("plugins/private"), "abcdef1234")
        .canonicalize()
        .unwrap();
    std::fs::write(
        dir.join("dim-plugin.json"),
        json!({"name":"package","skills":"skills"}).to_string(),
    )
    .unwrap();
    let stored_url = "https://user-fixture:password-fixture@example.invalid/path-fixture?token=query-fixture#fragment-fixture";
    std::fs::write(home.join("plugins.json"), json!({
        "version":1,
        "installed":[{"id":"private","name":"package","source":{"git":stored_url,"ref":"ref-fixture","commit":"abcdef1234"},"dir":dir,"kinds":["skills"]}],
        "names":{"package":{"off":true}}
    }).to_string()).unwrap();
    let (server, _, _) = start(config).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let listed = call(&mut client, "plugin.list", json!({})).await;
    assert_ok(&listed);
    let source = &item(&listed, "package")["candidates"][0]["source"];
    assert_eq!(source["git"], "https://example.invalid/path-fixture");
    assert_eq!(source["ref"], "ref-fixture");
    assert_eq!(source["commit"], "abcdef1234");
    for secret in [
        "user-fixture",
        "password-fixture",
        "query-fixture",
        "fragment-fixture",
    ] {
        assert!(
            !listed.to_string().contains(secret),
            "plugin catalog must hide stored Git credentials"
        );
    }
    let stored: Value =
        serde_json::from_slice(&std::fs::read(home.join("plugins.json")).unwrap()).unwrap();
    assert_eq!(stored["installed"][0]["source"]["git"], stored_url);
    let invalid = call(
        &mut client,
        "plugin.install",
        json!({"source":{"git":"file://invalid-user:invalid-password@host/query-secret"}}),
    )
    .await;
    assert_eq!(app_error(&invalid), "invalid_source");
    assert_eq!(
        invalid["error"]["message"],
        "Use a local directory or an https:// or ssh:// Git URL; check the source and ref"
    );
    for secret in ["invalid-user", "invalid-password", "query-secret"] {
        assert!(!invalid.to_string().contains(secret));
    }
    assert_ok(
        &call(
            &mut client,
            "plugin.enable",
            json!({"name":"package","use":"private"}),
        )
        .await,
    );
    std::fs::write(
        dir.join("dim-plugin.json"),
        json!({"name":"package","code":{"entry":"private-runtime-secret"}}).to_string(),
    )
    .unwrap();
    let failed = call(&mut client, "plugin.reload", json!({"name":"package"})).await;
    assert_eq!(app_error(&failed), "invalid_manifest");
    assert!(!failed.to_string().contains("private-runtime-secret"));
    let listed = call(&mut client, "plugin.list", json!({})).await;
    assert_eq!(item(&listed, "package")["status"], "failed");
    assert!(!listed.to_string().contains("private-runtime-secret"));
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
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
    let url = format!("ssh://fixture{}", repo.display());
    let installed = call(&mut client, "plugin.install", json!({"source":{"git":url}})).await;
    assert_eq!(installed["result"]["id"], "git-notes", "{installed}");
    let first_dir = recorded_dir(&home, "git-notes");
    assert!(first_dir.join("dim-plugin.json").is_file());
    std::fs::write(repo.join("skills/two.md"), "two").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "two"]);
    let second = git(&repo, &["rev-parse", "HEAD"]);
    let updated = call(&mut client, "plugin.update", json!({"id":"git-notes"})).await;
    assert_eq!(updated["result"]["commit"], second.as_str(), "{updated}");
    assert!(
        recorded_dir(&home, "git-notes")
            .join("skills/two.md")
            .is_file()
    );
    assert!(
        first_dir.join("skills/one.md").is_file(),
        "a replaced copy stays until the next start: {first_dir:?}"
    );
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
    assert!(recorded_dir(&home, "git-notes").is_dir());
    assert!(
        !first_dir.exists(),
        "the next start deletes the replaced copy: {first_dir:?}"
    );
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
    assert_eq!(
        notes["error"], "Plugin content or runtime failed",
        "{notes}"
    );
    assert_ok(&call(&mut client, "permission.rules", json!({})).await);

    let removed = recorded_dir(&home, "git-notes");
    assert_ok(&call(&mut client, "plugin.remove", json!({"id":"git-notes"})).await);
    assert!(
        removed.is_dir(),
        "remove leaves the copy until the next start: {removed:?}"
    );
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

/// A Python plugin directory named `name`: its entry is `plugin.py`.
fn python_plugin(root: &Path, name: &str) -> PathBuf {
    let dir = root.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("dim-plugin.json"),
        json!({"name": name, "code": {"entry": "plugin.py"}}).to_string(),
    )
    .unwrap();
    std::fs::write(dir.join("plugin.py"), "def apply(ctx):\n    pass\n").unwrap();
    dir
}

/// A server on `home` whose default configuration file is `defaults` and
/// whose RuntimeManager supplies the runtimes found on `search`: nothing is
/// acquired at startup, and acquiring the Node runtime runs no npm because
/// its packages are already there.
fn runtime_config(home: &Path, defaults: Value, search: std::ffi::OsString) -> ServerConfig {
    std::fs::create_dir_all(home).unwrap();
    let path = home.join("default-plugins.json");
    std::fs::write(&path, defaults.to_string()).unwrap();
    let runtimes = Arc::new(
        Runtimes::load(
            home,
            &Defaults::read(&path).unwrap().runtimes,
            Some(Environment {
                path: search,
                ..Default::default()
            }),
        )
        .unwrap(),
    );
    std::fs::create_dir_all(runtimes.plan(Kind::Node).packages.clone().unwrap()).unwrap();
    ServerConfig {
        product_home: Some(home.to_owned()),
        target_home: Some(home.join("rust")),
        plugin_defaults: Some(path),
        session_typescript: Some(SessionTypeScriptConfig::acquired(home, runtimes, vec![])),
        ..Default::default()
    }
}

/// §2.5: `plugin.remove` never requests a runtime, even when the
/// removed plugin's name falls back to a Python default plugin; only a
/// client call that turns a plugin on asks for one.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_remove_never_requests_a_runtime() {
    let home = home();
    python_plugin(&home.join("defaults"), "py-default");
    let copy = python_plugin(&home.join("src"), "py-default");
    let search = home.join("empty-path");
    std::fs::create_dir_all(&search).unwrap();
    let config = runtime_config(
        &home,
        json!({
            "plugins": {"py-default": "defaults/py-default"},
            "locked": ["PluginManager", "Rpc", "Transport"],
            "runtimes": {
                "node": {"source": "system", "auto": false},
                "python": {"source": "system", "auto": false},
                "uv": {"source": "download", "auto": false},
            },
        }),
        search.into(),
    );
    let dir = copy.canonicalize().unwrap();
    std::fs::write(
        home.join("plugins.json"),
        json!({
            "version": 2,
            "installed": [{
                "id": "copy",
                "name": "py-default",
                "source": {"path": dir},
                "dir": dir,
                "kinds": ["code"],
            }],
            "names": {"py-default": {"use": "copy"}},
        })
        .to_string(),
    )
    .unwrap();
    let (server, _, _) = start(config).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let listed = client.rpc("runtime.list", json!({})).await;
    assert_eq!(listed["result"][1]["name"], "python", "{listed}");
    assert_eq!(listed["result"][1]["status"], "off", "{listed}");

    assert_ok(&call(&mut client, "plugin.remove", json!({"id": "copy"})).await);
    let list = call(&mut client, "plugin.list", json!({})).await;
    assert_eq!(item(&list, "py-default")["use"], "default", "{list}");
    let listed = client.rpc("runtime.list", json!({})).await;
    assert_eq!(
        listed["result"][1]["status"], "off",
        "remove requested the Python runtime: {listed}"
    );
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// The names in `dir`, sorted.
fn entries(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// The default configuration file's content for a test: the two plugin
/// maps, the locked rows and the fixture runtimes.
fn defaults_file(plugins: Value, optional: Value) -> Value {
    json!({
        "plugins": plugins,
        "optional": optional,
        "locked": ["PluginManager", "Rpc", "Transport"],
        "runtimes": {
            "node": {"source": "system", "auto": false},
            "python": {"source": "system", "auto": false},
            "uv": {"source": "download", "auto": false},
        },
    })
}

/// The plain server on `home`, its default configuration file replaced.
fn config_with(home: &Path, defaults: Value) -> ServerConfig {
    let config = config(home);
    std::fs::write(
        config.plugin_defaults.as_ref().unwrap(),
        defaults.to_string(),
    )
    .unwrap();
    config
}

/// Writes an optional fixture plugin at `<root>/<name>`; `npm` adds the
/// `dependencies` declaration and the package files its lock needs.
fn optional_plugin(root: &Path, name: &str, version: &str, npm: bool) -> PathBuf {
    let dir = root.join(name);
    std::fs::create_dir_all(dir.join("skills")).unwrap();
    let mut manifest = json!({
        "name": name,
        "version": version,
        "code": {"entry": "index.ts"},
        "skills": "skills",
    });
    if npm {
        manifest["dependencies"] = json!("npm");
        for (file, content) in [
            (
                "package.json",
                json!({"name": name, "version": version,
                    "dependencies": {"fixture-dep": "file:fixture-dep"}}),
            ),
            (
                "package-lock.json",
                json!({"name": name, "version": version, "lockfileVersion": 3,
                    "packages": {"": {"name": name, "version": version,
                        "dependencies": {"fixture-dep": "file:fixture-dep"}}}}),
            ),
        ] {
            std::fs::write(dir.join(file), content.to_string()).unwrap();
        }
    }
    std::fs::write(dir.join("dim-plugin.json"), manifest.to_string()).unwrap();
    std::fs::write(dir.join("index.ts"), "export default {};\n").unwrap();
    std::fs::write(dir.join("skills/one.md"), "one").unwrap();
    dir
}

/// §3.2: `plugin.install {source: {optional}}` copies the shipped
/// directory without `node_modules` into `<version>+<suffix>` and installs
/// its locked dependencies there; the record becomes version 3 with a
/// version 2 backup, removing the last optional entry writes version 2
/// again, and a later optional install replaces the backup with that current
/// version 2 record.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_optional_install_copies_its_directory_and_installs_its_dependencies() {
    let home = home();
    let shipped = optional_plugin(&home.join("shipped"), "ocr-fixture", "1.0.0", true);
    let fixture = FixtureNode::with(
        "24.21.0",
        "mkdir -p node_modules/fixture-dep\n\
         echo 'export default 1' > node_modules/fixture-dep/index.js",
    );
    let config = runtime_config(
        &home,
        defaults_file(json!({}), json!({"ocr-fixture": "shipped/ocr-fixture"})),
        fixture.search_path(),
    );
    let version_2 = json!({"version": 2, "installed": [], "names": {}}).to_string();
    std::fs::write(home.join("plugins.json"), &version_2).unwrap();
    let shipped_files = entries(&shipped);
    let manifest_bytes = std::fs::read(shipped.join("dim-plugin.json")).unwrap();

    let (server, _, _) = start(config).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let installed = call(
        &mut client,
        "plugin.install",
        json!({"source": {"optional": "ocr-fixture"}}),
    )
    .await;
    assert_ok(&installed);
    assert_eq!(installed["result"]["id"], "ocr-fixture", "{installed}");
    assert_eq!(installed["result"]["name"], "ocr-fixture", "{installed}");
    assert_eq!(installed["result"]["enabled"], true, "{installed}");

    let dir = recorded_dir(&home, "ocr-fixture");
    assert_eq!(name_of(dir.parent().unwrap()), "ocr-fixture", "{dir:?}");
    assert!(acquisition_name(&name_of(&dir)), "{dir:?}");
    assert!(name_of(&dir).starts_with("1.0.0+"), "{dir:?}");
    assert_eq!(
        entries(&dir),
        [
            "dim-plugin.json",
            "index.ts",
            "node_modules",
            "package-lock.json",
            "package.json",
            "skills"
        ],
        "{dir:?}"
    );
    assert!(
        dir.join("node_modules/fixture-dep/index.js").is_file(),
        "{dir:?}"
    );
    assert_eq!(
        entries(&shipped),
        shipped_files,
        "the shipped directory is untouched"
    );
    assert_eq!(
        std::fs::read(shipped.join("dim-plugin.json")).unwrap(),
        manifest_bytes
    );
    assert!(
        !shipped.join("node_modules").exists(),
        "the copy's dependencies are never installed in place"
    );

    let record: Value =
        serde_json::from_slice(&std::fs::read(home.join("plugins.json")).unwrap()).unwrap();
    assert_eq!(record["version"], 3, "{record}");
    assert_eq!(
        record["installed"][0]["source"],
        json!({"optional": "ocr-fixture"}),
        "{record}"
    );
    assert_eq!(record["installed"][0]["version"], "1.0.0", "{record}");
    assert_eq!(
        std::fs::read_to_string(home.join("plugins.json.v2.backup")).unwrap(),
        version_2,
        "the first version 3 write keeps the exact version 2 bytes"
    );

    // §3.6: once installed, the item is a normal one that keeps the marker.
    let list = call(&mut client, "plugin.list", json!({})).await;
    let item = item(&list, "ocr-fixture");
    assert_eq!(item["use"], "ocr-fixture", "{list}");
    assert_eq!(item["off"], false, "{list}");
    assert_eq!(item["optional"], true, "{list}");
    assert_eq!(
        item["candidates"][0]["source"],
        json!({"optional": "ocr-fixture"}),
        "{list}"
    );
    assert!(item.get("update").is_none(), "{list}");

    // §2.2: removing the last optional entry writes version 2 again.
    assert_ok(&call(&mut client, "plugin.remove", json!({"id": "ocr-fixture"})).await);
    let record: Value =
        serde_json::from_slice(&std::fs::read(home.join("plugins.json")).unwrap()).unwrap();
    assert_eq!(record["version"], 2, "{record}");
    assert_eq!(record["installed"], json!([]), "{record}");

    // §2.2: a later version 3 write replaces the backup with the version 2
    // record current at that moment, so a second optional install works
    // after the record went back to version 2.
    let other = plugin_dir(&home.join("src"), "notes");
    assert_ok(
        &call(
            &mut client,
            "plugin.install",
            json!({"source": {"path": other}}),
        )
        .await,
    );
    let before_second = std::fs::read(home.join("plugins.json")).unwrap();
    let reinstalled = call(
        &mut client,
        "plugin.install",
        json!({"source": {"optional": "ocr-fixture"}}),
    )
    .await;
    assert_ok(&reinstalled);
    let record: Value =
        serde_json::from_slice(&std::fs::read(home.join("plugins.json")).unwrap()).unwrap();
    assert_eq!(record["version"], 3, "{record}");
    assert_eq!(
        std::fs::read(home.join("plugins.json.v2.backup")).unwrap(),
        before_second,
        "the version 2 backup follows the last version 2 record"
    );

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// §3.2: a shipped optional entry is the plugin its key names; a manifest
/// naming another is refused, and nothing is installed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_optional_plugin_whose_manifest_names_another_is_refused() {
    let home = home();
    optional_plugin(&home.join("shipped"), "y", "1.0.0", false);
    let fixture = FixtureNode::with("24.21.0", "true");
    let config = runtime_config(
        &home,
        defaults_file(json!({}), json!({"x": "shipped/y"})),
        fixture.search_path(),
    );
    let (server, _, _) = start(config).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let refused = call(
        &mut client,
        "plugin.install",
        json!({"source": {"optional": "x"}}),
    )
    .await;
    assert_eq!(app_error(&refused), "name_changed", "{refused}");
    assert!(!home.join("plugins.json").exists(), "nothing is recorded");
    // Only the directories startup itself makes (`.python`, and a staging
    // parent a failure removes again) may remain.
    let leftovers: Vec<String> = plugins_entries(&home)
        .into_iter()
        .filter(|entry| entry != ".python" && entry != ".staging")
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
    let list = call(&mut client, "plugin.list", json!({})).await;
    let listed = item(&list, "x");
    assert_eq!(listed["use"], Value::Null, "{list}");
    assert_eq!(listed["off"], true, "{list}");
    assert_eq!(listed["optional"], true, "{list}");
    assert_eq!(listed["candidates"], json!([]), "{list}");
    assert!(listed.get("status").is_none(), "{list}");
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// §3.2 step 1, A.2: a name the default configuration file does not ship is
/// `optional_not_found` before anything is acquired; the client sees the
/// static description and nothing is created.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unknown_optional_name_creates_nothing() {
    let home = home();
    let config = config_with(&home, defaults_file(json!({}), json!({})));
    let (server, _, _) = start(config).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let refused = call(
        &mut client,
        "plugin.install",
        json!({"source": {"optional": "missing"}}),
    )
    .await;
    assert_eq!(app_error(&refused), "optional_not_found", "{refused}");
    assert_eq!(
        refused["error"]["message"],
        "This release does not ship that optional plugin; refresh the catalog",
        "{refused}"
    );
    assert!(
        plugins_entries(&home).is_empty(),
        "{:?}",
        plugins_entries(&home)
    );
    assert!(!home.join("plugins.json").exists(), "nothing is recorded");

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// §3.6: an uninstalled optional plugin has exactly its own item, with no
/// status; installing it turns the item into a normal one that keeps the
/// marker.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_uninstalled_optional_plugin_has_its_own_item() {
    let home = home();
    optional_plugin(&home.join("shipped"), "ocr-fixture", "1.0.0", false);
    let config = config_with(
        &home,
        defaults_file(json!({}), json!({"ocr-fixture": "shipped/ocr-fixture"})),
    );
    let (server, _, _) = start(config).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let list = call(&mut client, "plugin.list", json!({})).await;
    assert_eq!(
        item(&list, "ocr-fixture").clone(),
        json!({
            "name": "ocr-fixture",
            "use": null,
            "off": true,
            "locked": false,
            "candidates": [],
            "optional": true,
        }),
        "{list}"
    );

    assert_ok(
        &call(
            &mut client,
            "plugin.install",
            json!({"source": {"optional": "ocr-fixture"}}),
        )
        .await,
    );
    let list = call(&mut client, "plugin.list", json!({})).await;
    let installed = item(&list, "ocr-fixture");
    assert_eq!(installed["use"], "ocr-fixture", "{list}");
    assert_eq!(installed["off"], false, "{list}");
    assert_eq!(installed["optional"], true, "{list}");
    assert_eq!(installed["candidates"][0]["id"], "ocr-fixture", "{list}");

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// §3.6: `update` follows the entry a re-acquisition would act on, so an
/// installed optional plugin that is off still offers the shipped version.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_installed_optional_plugin_that_is_off_offers_its_update() {
    let home = home();
    let shipped = optional_plugin(&home.join("shipped"), "ocr-fixture", "1.0.0", false)
        .canonicalize()
        .unwrap();
    let fixture = FixtureNode::with("24.21.0", "true");
    let config = runtime_config(
        &home,
        defaults_file(
            json!({}),
            json!({"ocr-fixture": shipped.display().to_string()}),
        ),
        fixture.search_path(),
    );
    let (server, _, _) = start(config).await;
    let mut client = Client::new_version(&server, false, 3).await;
    assert_ok(
        &call(
            &mut client,
            "plugin.install",
            json!({"source": {"optional": "ocr-fixture"}}),
        )
        .await,
    );
    assert_ok(
        &call(
            &mut client,
            "plugin.disable",
            json!({"name": "ocr-fixture"}),
        )
        .await,
    );
    // The shipped version changes while the name is off.
    let manifest_path = shipped.join("dim-plugin.json");
    let mut manifest: Value =
        serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
    manifest["version"] = json!("1.1.0");
    std::fs::write(&manifest_path, manifest.to_string()).unwrap();
    let list = call(&mut client, "plugin.list", json!({})).await;
    let listed = item(&list, "ocr-fixture");
    assert_eq!(listed["off"], true, "{list}");
    assert_eq!(listed["update"]["version"], "1.1.0", "{list}");
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// §3.6: the `optional` marker is the entry's provenance, so it stays when
/// a later release drops the name from the shipped map.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_installed_optional_plugin_keeps_its_marker_after_the_map_drops_it() {
    let home = home();
    let shipped = optional_plugin(&home.join("shipped"), "ocr-fixture", "1.0.0", false)
        .canonicalize()
        .unwrap();
    let fixture = FixtureNode::with("24.21.0", "true");
    let config = runtime_config(
        &home,
        defaults_file(
            json!({}),
            json!({"ocr-fixture": shipped.display().to_string()}),
        ),
        fixture.search_path(),
    );
    let (server, _, _) = start(config).await;
    let mut client = Client::new_version(&server, false, 3).await;
    assert_ok(
        &call(
            &mut client,
            "plugin.install",
            json!({"source": {"optional": "ocr-fixture"}}),
        )
        .await,
    );
    drop(client);
    server.shutdown();
    server.join().await.unwrap();

    // The restart's default configuration still installs the plugin but no
    // longer lists its name as optional.
    let config = runtime_config(
        &home,
        defaults_file(json!({}), json!({})),
        fixture.search_path(),
    );
    let (server, _, _) = start(config).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let list = call(&mut client, "plugin.list", json!({})).await;
    let listed = item(&list, "ocr-fixture");
    assert_eq!(listed["use"], "ocr-fixture", "{list}");
    assert_eq!(listed["optional"], true, "{list}");
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// §3.3: `plugin.enable`, `plugin.disable` and `plugin.reload` of an
/// uninstalled optional name keep answering `plugin_not_found` with today's
/// client description; nothing is installed and no runtime is requested.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_uninstalled_optional_name_is_refused_as_a_missing_plugin() {
    let home = home();
    optional_plugin(&home.join("shipped"), "ocr-fixture", "1.0.0", false);
    let search = home.join("empty-path");
    std::fs::create_dir_all(&search).unwrap();
    let config = runtime_config(
        &home,
        defaults_file(json!({}), json!({"ocr-fixture": "shipped/ocr-fixture"})),
        search.into(),
    );
    let (server, _, _) = start(config).await;
    let mut client = Client::new_version(&server, false, 3).await;
    for (method, params) in [
        ("plugin.enable", json!({"name": "ocr-fixture"})),
        ("plugin.disable", json!({"name": "ocr-fixture"})),
        ("plugin.reload", json!({"name": "ocr-fixture"})),
    ] {
        let refused = call(&mut client, method, params).await;
        assert_eq!(
            app_error(&refused),
            "plugin_not_found",
            "{method}: {refused}"
        );
        assert_eq!(
            refused["error"]["message"], "Plugin not found or disabled; refresh the catalog",
            "{method}: {refused}"
        );
    }
    assert!(!home.join("plugins.json").exists(), "nothing is installed");
    let listed = client.rpc("runtime.list", json!({})).await;
    assert_eq!(listed["result"][0]["status"], "off", "{listed}");
    assert_eq!(listed["result"][1]["status"], "off", "{listed}");

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// §2.5, acceptance 14: `plugin.install` and `plugin.enable` of a plugin
/// with code request its runtime and reply `pending` waiting for the
/// runtime's connection row; a plain start and a reload request nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_install_or_enable_requests_the_runtime_but_a_reload_does_not() {
    let home = home();
    let source = plugin_dir(&home.join("src"), "node-tool");
    std::fs::write(
        source.join("dim-plugin.json"),
        json!({"name": "node-tool", "code": {"entry": "index.ts"}}).to_string(),
    )
    .unwrap();
    std::fs::write(source.join("index.ts"), "export default {};\n").unwrap();
    let search = home.join("empty-path");
    std::fs::create_dir_all(&search).unwrap();
    let config = runtime_config(
        &home,
        defaults_file(json!({}), json!({})),
        search.clone().into(),
    );

    // A plain start requests nothing.
    let (server, _, _) = start(config.clone()).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let listed = client.rpc("runtime.list", json!({})).await;
    assert_eq!(listed["result"][0]["status"], "off", "{listed}");
    // Installing asks for Node: the plugin row waits for the connection row
    // while the acquisition runs, and Node no longer reads off.
    let installed = call(
        &mut client,
        "plugin.install",
        json!({"source": {"path": source}}),
    )
    .await;
    assert_eq!(installed["result"]["status"], "pending", "{installed}");
    assert_eq!(
        installed["result"]["waitingFor"],
        json!(["session-node"]),
        "{installed}"
    );
    let listed = client.rpc("runtime.list", json!({})).await;
    assert_ne!(
        listed["result"][0]["status"], "off",
        "install requested the Node runtime: {listed}"
    );
    drop(client);
    server.shutdown();
    server.join().await.unwrap();

    // A plain start of the same configuration and a reload request nothing;
    // enabling asks for Node again.
    let dir = source.canonicalize().unwrap();
    std::fs::write(
        home.join("plugins.json"),
        json!({
            "version": 2,
            "installed": [{
                "id": "node-tool",
                "name": "node-tool",
                "source": {"path": dir},
                "dir": dir,
                "kinds": ["code"],
            }],
            "names": {"node-tool": {"use": "node-tool"}},
        })
        .to_string(),
    )
    .unwrap();
    let (server, _, _) = start(config).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let listed = client.rpc("runtime.list", json!({})).await;
    assert_eq!(
        listed["result"][0]["status"], "off",
        "a plain start requests no runtime: {listed}"
    );
    let reloaded = call(&mut client, "plugin.reload", json!({"name": "node-tool"})).await;
    assert_eq!(reloaded["result"]["status"], "pending", "{reloaded}");
    let listed = client.rpc("runtime.list", json!({})).await;
    assert_eq!(
        listed["result"][0]["status"], "off",
        "a reload requests no runtime: {listed}"
    );
    assert_ok(&call(&mut client, "plugin.enable", json!({"name": "node-tool"})).await);
    let listed = client.rpc("runtime.list", json!({})).await;
    assert_ne!(
        listed["result"][0]["status"], "off",
        "enable requested the Node runtime: {listed}"
    );

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// §3.5: `plugin.update` of an optional plugin at an unchanged version
/// copies it into a new `<version>+<suffix>` directory and switches the
/// record; a failing dependency install keeps the old copy.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_optional_update_draws_its_own_directory_and_keeps_the_old_copy() {
    let home = home();
    optional_plugin(&home.join("shipped"), "ocr-fixture", "1.0.0", true);
    let fixture = FixtureNode::with("24.21.0", "mkdir -p node_modules/fixture-dep");
    let config = runtime_config(
        &home,
        defaults_file(json!({}), json!({"ocr-fixture": "shipped/ocr-fixture"})),
        fixture.search_path(),
    );
    let (server, _, _) = start(config).await;
    let mut client = Client::new_version(&server, false, 3).await;
    assert_ok(
        &call(
            &mut client,
            "plugin.install",
            json!({"source": {"optional": "ocr-fixture"}}),
        )
        .await,
    );
    let first = recorded_dir(&home, "ocr-fixture");

    let updated = call(&mut client, "plugin.update", json!({"id": "ocr-fixture"})).await;
    assert_eq!(updated["result"]["version"], "1.0.0", "{updated}");
    assert!(updated["result"].get("commit").is_none(), "{updated}");
    let next = recorded_dir(&home, "ocr-fixture");
    assert_ne!(first, next, "an update draws its own directory");
    assert!(acquisition_name(&name_of(&next)), "{next:?}");
    assert_eq!(copies(&home, "ocr-fixture").len(), 2, "the old copy stays");
    assert!(next.join("node_modules/fixture-dep").is_dir(), "{next:?}");
    assert_eq!(record(&home)["installed"][0]["version"], "1.0.0");

    // A failing dependency install keeps the old copy and reports npm's
    // failure as `dependencies_failed`.
    fixture.npm("echo refused >&2\nexit 1");
    let refused = call(&mut client, "plugin.update", json!({"id": "ocr-fixture"})).await;
    assert_eq!(app_error(&refused), "dependencies_failed", "{refused}");
    assert_eq!(
        recorded_dir(&home, "ocr-fixture"),
        next,
        "the failure keeps the copy in use"
    );
    assert_eq!(copies(&home, "ocr-fixture").len(), 2, "no third copy");
    assert!(
        !home.join("plugins/.staging").exists(),
        "the failed staging directory is gone"
    );

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// A Git repository holding a `git-notes` skills plugin that declares its
/// npm dependencies; `lock` writes its `package-lock.json` too.
fn git_dependency_repo(home: &Path, lock: bool) -> PathBuf {
    let repo = home.join("repo");
    std::fs::create_dir_all(repo.join("skills")).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(
        repo.join("dim-plugin.json"),
        json!({"name": "git-notes", "skills": "skills", "dependencies": "npm"}).to_string(),
    )
    .unwrap();
    std::fs::write(repo.join("skills/one.md"), "one").unwrap();
    std::fs::write(
        repo.join("package.json"),
        json!({"name": "git-notes", "dependencies": {"fixture-dep": "file:fixture-dep"}})
            .to_string(),
    )
    .unwrap();
    if lock {
        std::fs::write(
            repo.join("package-lock.json"),
            json!({"name": "git-notes", "lockfileVersion": 3, "packages": {}}).to_string(),
        )
        .unwrap();
    }
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "one"]);
    repo
}

/// §3.2 step 3, §3.5: a Git plugin declaring dependencies gets them
/// installed in its clone before the record changes; a manifest declaring
/// them without `package-lock.json` is `invalid_manifest`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_git_plugin_installs_its_dependencies_before_the_record_changes() {
    ssh_stand_in();
    let home = home();
    let fixture = FixtureNode::with(
        "24.21.0",
        "mkdir -p node_modules/fixture-dep\n\
         echo 'export default 1' > node_modules/fixture-dep/index.js",
    );
    let config = runtime_config(
        &home,
        defaults_file(json!({}), json!({})),
        fixture.search_path(),
    );
    let (server, _, _) = start(config).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let repo = git_dependency_repo(&home, true);
    let url = format!("ssh://fixture{}", repo.display());
    let installed = call(
        &mut client,
        "plugin.install",
        json!({"source": {"git": url}}),
    )
    .await;
    assert_ok(&installed);
    let dir = recorded_dir(&home, "git-notes");
    assert!(
        dir.join("node_modules/fixture-dep/index.js").is_file(),
        "the clone gets its locked dependencies: {dir:?}"
    );
    assert!(
        !repo.join("node_modules").exists(),
        "the repository is untouched"
    );

    let unlocked = home.join("unlocked");
    std::fs::create_dir_all(unlocked.join("skills")).unwrap();
    git(&unlocked, &["init", "-q", "-b", "main"]);
    std::fs::write(
        unlocked.join("dim-plugin.json"),
        json!({"name": "git-unlocked", "skills": "skills", "dependencies": "npm"}).to_string(),
    )
    .unwrap();
    std::fs::write(unlocked.join("skills/one.md"), "one").unwrap();
    std::fs::write(
        unlocked.join("package.json"),
        json!({"name": "git-unlocked"}).to_string(),
    )
    .unwrap();
    git(&unlocked, &["add", "-A"]);
    git(&unlocked, &["commit", "-qm", "one"]);
    let url = format!("ssh://fixture{}", unlocked.display());
    let refused = call(
        &mut client,
        "plugin.install",
        json!({"id": "git-unlocked", "source": {"git": url}}),
    )
    .await;
    assert_eq!(app_error(&refused), "invalid_manifest", "{refused}");
    assert!(copies(&home, "git-unlocked").is_empty());
    assert!(
        record(&home)["installed"]
            .as_array()
            .unwrap()
            .iter()
            .all(|entry| entry["id"] != "git-unlocked")
    );

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// §4, §3.2: every file system change for a plugin runs one at a time, and
/// a copy and its record appear only after the dependency install finishes.
/// The gated npm holds an install between staging and rename while a remove
/// of another plugin waits for it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_file_system_queue_holds_a_remove_until_the_install_finishes() {
    let home = home();
    optional_plugin(&home.join("shipped"), "ocr-fixture", "1.0.0", true);
    let started = home.join("npm-started");
    let release = home.join("npm-release");
    let fixture = FixtureNode::with(
        "24.21.0",
        &format!(
            "touch '{}'\nwhile [ ! -e '{}' ]; do sleep 0.05; done\n\
             mkdir -p node_modules/fixture-dep\necho 'export default 1' > node_modules/fixture-dep/index.js",
            started.display(),
            release.display()
        ),
    );
    let config = runtime_config(
        &home,
        defaults_file(json!({}), json!({"ocr-fixture": "shipped/ocr-fixture"})),
        fixture.search_path(),
    );
    let (server, _, _) = start(config).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let other = plugin_dir(&home.join("src"), "notes");
    assert_ok(
        &call(
            &mut client,
            "plugin.install",
            json!({"source": {"path": other}}),
        )
        .await,
    );

    let installing = client
        .send(
            "plugin.install",
            json!({"source": {"optional": "ocr-fixture"}}),
        )
        .await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while !started.exists() {
        assert!(tokio::time::Instant::now() < deadline, "npm never started");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    // §3.2 step 3 before step 4: the copy is not renamed into place and the
    // record does not name it while its dependencies are installed.
    assert!(
        copies(&home, "ocr-fixture").is_empty(),
        "{:?}",
        copies(&home, "ocr-fixture")
    );
    assert!(
        record(&home)["installed"]
            .as_array()
            .unwrap()
            .iter()
            .all(|entry| entry["id"] != "ocr-fixture"),
        "the record is written after the rename"
    );

    // The remove of another plugin waits for the install's file system
    // changes (§4).
    let removing = client.send("plugin.remove", json!({"id": "notes"})).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        record(&home)["installed"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["id"] == "notes"),
        "the remove ran while the install held the file system queue"
    );
    std::fs::write(&release, "").unwrap();
    assert_ok(&client.response(installing).await);
    assert_ok(&client.response(removing).await);
    assert!(
        recorded_dir(&home, "ocr-fixture")
            .join("node_modules/fixture-dep/index.js")
            .is_file()
    );
    assert!(
        record(&home)["installed"]
            .as_array()
            .unwrap()
            .iter()
            .all(|entry| entry["id"] != "notes"),
        "the remove ran once the install finished"
    );

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// §2.2: a record below version 3 that holds an `{optional}` source is
/// refused at startup.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_version_2_record_with_an_optional_source_is_refused() {
    let home = home();
    let mut config = config(&home);
    config.unix_path = Some(home.join("ipc/server.sock"));
    std::fs::write(
        home.join("plugins.json"),
        json!({
            "version": 2,
            "installed": [{
                "id": "ocr-fixture",
                "name": "ocr-fixture",
                "source": {"optional": "ocr-fixture"},
                "dir": home.join("plugins/ocr-fixture/1.0.0+0123456789abcdef"),
                "kinds": [],
                "version": "1.0.0",
            }],
            "names": {},
        })
        .to_string(),
    )
    .unwrap();
    let (builder, _, _) = setup();
    match serve(builder, config).await {
        Ok(_) => panic!("a version 2 record holding an optional source must be refused"),
        Err(error) => assert!(
            error
                .to_string()
                .contains("optional sources require record version 3"),
            "{error}"
        ),
    }
    std::fs::remove_dir_all(home).unwrap();
}

/// §2.3: an optional plugin's name cannot also be a default plugin's,
/// and it cannot be a built-in plugin's either; both are startup errors.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_optional_name_colliding_with_another_map_is_refused() {
    let home = home();
    let dir = plugin_dir(&home.join("src"), "web");
    let mut config = config_with(
        &home,
        defaults_file(json!({"web": dir}), json!({"web": "src/web"})),
    );
    config.unix_path = Some(home.join("ipc/server.sock"));
    let (builder, _, _) = setup();
    match serve(builder, config).await {
        Ok(_) => panic!("a name in both maps must be refused"),
        Err(error) => assert!(
            error
                .to_string()
                .contains("is in both \"plugins\" and \"optional\""),
            "{error}"
        ),
    }

    let second = home.join("second");
    let mut config = config_with(&second, defaults_file(json!({}), json!({"web": "src/web"})));
    config.unix_path = Some(second.join("ipc/server.sock"));
    let (builder, _, _) = setup();
    match serve(builder, config).await {
        Ok(_) => panic!("an optional name that is a built-in plugin's must be refused"),
        Err(error) => assert!(
            error
                .to_string()
                .contains("optional plugin web is also a built-in plugin's name"),
            "{error}"
        ),
    }
    std::fs::remove_dir_all(home).unwrap();
}

/// §3.2 step 4: a Git acquisition renames into `<id>/<commit>+<16 lowercase
/// hex>`, drawn for that acquisition; an update draws its own directory
/// instead of replacing the one in use.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_git_acquisition_gets_its_own_suffixed_directory() {
    ssh_stand_in();
    let home = home();
    let config = config(&home);
    let (server, _, _) = start(config).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let repo = git_notes_repo(&home);
    let url = format!("ssh://fixture{}", repo.display());
    assert_ok(&call(&mut client, "plugin.install", json!({"source":{"git":url}})).await);
    let id_dir = home.join("plugins/git-notes").canonicalize().unwrap();
    let first = recorded_dir(&home, "git-notes");
    assert_eq!(first.parent(), Some(id_dir.as_path()), "{first:?}");
    assert!(acquisition_name(&name_of(&first)), "{first:?}");

    let second = second_commit(&repo);
    let updated = call(&mut client, "plugin.update", json!({"id":"git-notes"})).await;
    assert_eq!(updated["result"]["commit"], second.as_str(), "{updated}");
    assert!(
        updated["result"].get("version").is_none(),
        "a Git update's reply is unchanged: {updated}"
    );
    let next = recorded_dir(&home, "git-notes");
    assert_ne!(first, next, "an update draws its own directory");
    assert!(acquisition_name(&name_of(&next)), "{next:?}");
    assert!(first.join("skills/one.md").is_file(), "{first:?}");
    assert!(next.join("skills/two.md").is_file(), "{next:?}");
    assert_eq!(copies(&home, "git-notes").len(), 2);
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// §3.1 step 3, §7: the next start deletes the copies no entry refers to.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_next_start_deletes_the_copies_no_entry_names() {
    ssh_stand_in();
    let home = home();
    let config = config(&home);
    let (server, _, _) = start(config.clone()).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let repo = git_notes_repo(&home);
    let url = format!("ssh://fixture{}", repo.display());
    assert_ok(&call(&mut client, "plugin.install", json!({"source":{"git":url}})).await);
    let first = recorded_dir(&home, "git-notes");
    second_commit(&repo);
    assert_ok(&call(&mut client, "plugin.update", json!({"id":"git-notes"})).await);
    let next = recorded_dir(&home, "git-notes");
    assert_ne!(first, next);
    assert!(first.is_dir() && next.is_dir());
    drop(client);
    server.shutdown();
    server.join().await.unwrap();

    let (server, _, _) = start(config).await;
    let client = Client::new_version(&server, false, 3).await;
    assert!(!first.exists(), "the replaced copy is gone: {first:?}");
    assert!(next.is_dir(), "{next:?}");
    assert_eq!(copies(&home, "git-notes"), vec![name_of(&next)]);
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// §3.6, §7: remove leaves the copy in place until the next start.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn remove_keeps_the_copy_until_the_next_start() {
    ssh_stand_in();
    let home = home();
    let config = config(&home);
    let (server, _, _) = start(config.clone()).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let repo = git_notes_repo(&home);
    let url = format!("ssh://fixture{}", repo.display());
    assert_ok(&call(&mut client, "plugin.install", json!({"source":{"git":url}})).await);
    let copy = recorded_dir(&home, "git-notes");
    assert_ok(&call(&mut client, "plugin.remove", json!({"id":"git-notes"})).await);
    assert!(copy.is_dir(), "remove leaves the copy: {copy:?}");
    let list = call(&mut client, "plugin.list", json!({})).await;
    assert!(
        list["result"]
            .as_array()
            .unwrap()
            .iter()
            .all(|item| item["name"] != "git-notes"),
        "{list}"
    );
    drop(client);
    server.shutdown();
    server.join().await.unwrap();

    let (server, _, _) = start(config).await;
    let client = Client::new_version(&server, false, 3).await;
    assert!(!copy.exists(), "the next start deletes it: {copy:?}");
    assert!(!home.join("plugins/git-notes").exists());
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// §2.2: a record written before suffixes names its directory without one.
/// Nothing migrates it, and the cleanup keeps the directory it names.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_record_from_before_suffixes_still_loads() {
    let home = home();
    let config = config(&home);
    let commit = "0123456789abcdef0123456789abcdef01234567";
    let dir = home.join("plugins/legacy").join(commit);
    std::fs::create_dir_all(dir.join("skills")).unwrap();
    std::fs::write(
        dir.join("dim-plugin.json"),
        r#"{"name":"legacy","skills":"skills"}"#,
    )
    .unwrap();
    std::fs::write(dir.join("skills/one.md"), "one").unwrap();
    let dir = dir.canonicalize().unwrap();
    std::fs::write(
        home.join("plugins.json"),
        json!({
            "version": 2,
            "installed": [{
                "id": "legacy",
                "name": "legacy",
                "source": {"git": "https://example.invalid/legacy.git", "commit": commit},
                "dir": dir,
                "kinds": ["skills"],
            }],
            "names": {"legacy": {"use": "legacy"}},
        })
        .to_string(),
    )
    .unwrap();
    let (server, _, _) = start(config.clone()).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let list = call(&mut client, "plugin.list", json!({})).await;
    let legacy = item(&list, "legacy");
    assert_eq!(legacy["use"], "legacy", "{legacy}");
    assert_eq!(legacy["off"], false, "{legacy}");
    assert!(dir.join("skills/one.md").is_file(), "the cleanup keeps it");
    drop(client);
    server.shutdown();
    server.join().await.unwrap();

    let (server, _, _) = start(config).await;
    let client = Client::new_version(&server, false, 3).await;
    assert!(dir.join("skills/one.md").is_file());
    assert_eq!(
        record(&home)["installed"][0]["dir"],
        json!(dir),
        "nothing migrates it"
    );
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// §7: the startup cleanup deletes a copy no entry names when its name has
/// one of the two copy shapes, and keeps a name the path rule refuses.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_cleanup_deletes_only_a_copy_shaped_directory() {
    let home = home();
    let config = config(&home);
    let commit = "0123456789abcdef0123456789abcdef01234567";
    let ghost = home.join("plugins/ghost");
    let acquisition = ghost.join(format!("{commit}+0123456789abcdef"));
    let bare = ghost.join(commit);
    let refused = ghost.join(format!("{commit}+ZZZZZZZZZZZZZZZZ"));
    for dir in [&acquisition, &bare, &refused] {
        std::fs::create_dir_all(dir).unwrap();
    }
    let (server, _, _) = start(config).await;
    let client = Client::new_version(&server, false, 3).await;
    assert!(!acquisition.exists(), "a copy no entry names is deleted");
    assert!(!bare.exists(), "a bare key no entry names is deleted too");
    assert!(
        refused.exists(),
        "a name the path rule refuses is not deleted: {refused:?}"
    );
    assert_eq!(copies(&home, "ghost"), vec![name_of(&refused)]);
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// §7, G8: the cleanup runs once per process. Reloading the PluginManager
/// row deletes no copy; the next start does.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reloading_the_plugin_manager_row_keeps_a_replaced_copy() {
    ssh_stand_in();
    let home = home();
    let config = config(&home);
    let (server, _, _) = start(config.clone()).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let repo = git_notes_repo(&home);
    let url = format!("ssh://fixture{}", repo.display());
    assert_ok(&call(&mut client, "plugin.install", json!({"source":{"git":url}})).await);
    let first = recorded_dir(&home, "git-notes");
    second_commit(&repo);
    assert_ok(&call(&mut client, "plugin.update", json!({"id":"git-notes"})).await);
    let next = recorded_dir(&home, "git-notes");
    assert_ne!(first, next);

    let loader = server.service::<rutis_loader::Loader>().unwrap();
    loader.restart("PluginManager").await.unwrap();
    assert!(
        first.is_dir(),
        "a reload of the row deletes no copy: {first:?}"
    );
    assert!(next.is_dir());
    drop(client);
    server.shutdown();
    server.join().await.unwrap();

    let (server, _, _) = start(config).await;
    let client = Client::new_version(&server, false, 3).await;
    assert!(!first.exists(), "the next start deletes it: {first:?}");
    assert!(next.is_dir());
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
    let history = chain_events(&session, "main", ..).await.unwrap();
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
    let shipped = optional_plugin(&home.join("shipped"), "ocr-fixture", "1.0.0", false);
    std::fs::write(
        config.plugin_defaults.as_ref().unwrap(),
        json!({"plugins": {"plugin-authoring": authoring},
            "optional": {"ocr-fixture": shipped},
            "locked": ["PluginManager", "Rpc", "Transport"]})
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
    let (usage, error) = plugin(&mut client, &server, &session, "help install").await;
    assert!(
        !error
            && usage.starts_with("Usage: install <path|git-url>")
            && usage.contains("| install --optional <name> [--id ID] [--no-enable]"),
        "{usage}"
    );
    let (refused, error) =
        plugin(&mut client, &server, &session, "install --optional missing").await;
    assert!(
        error && refused.starts_with("optional_not_found:"),
        "{refused}"
    );
    let (refused, error) = plugin(&mut client, &server, &session, "enable ocr-fixture").await;
    assert!(
        error && refused.contains(r#"plugin.install {source: {optional: "ocr-fixture"}}"#),
        "{refused}"
    );
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

/// Acceptance 1 and 10: the default configuration loads no process plugin,
/// and Workflow runs only once it is installed and enabled, with its
/// Settings source there while it runs.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires DIM_TEST_NODE and DIM_TEST_JS_RUNTIME"]
async fn node_workflow_runs_only_when_enabled_and_its_settings_follow_it() {
    let home = home();
    let mut config = config(&home);
    let workflow_settings = home.join("workflow.json");
    let workflow_runs = home.join("workflow/runs.json");
    let settings_bytes = b"{\"definitions\":{},\"projectEnabled\":{}}\n";
    let runs_bytes = br#"{"version":1,"text":"{\"version\":2,\"runs\":[]}"}"#;
    std::fs::create_dir_all(workflow_runs.parent().unwrap()).unwrap();
    std::fs::write(&workflow_settings, settings_bytes).unwrap();
    std::fs::write(&workflow_runs, runs_bytes).unwrap();
    let mut parts = fixture();
    parts.rows.clear();
    let workflow = parts.anchor.parent().unwrap().join("plugins/workflow");
    config.plugin_defaults = Some(parts.anchor.with_file_name("default-plugins.json"));
    config.session_typescript = Some(parts);
    let (server, _requests, _) = start(config.clone()).await;
    let mut client = Client::new_version(&server, false, 3).await;
    // No process plugin runs by default: `web` and `Agents` are built in,
    // and Workflow is absent until it is installed.
    let list = call(&mut client, "plugin.list", json!({})).await;
    assert_eq!(item(&list, "web")["use"], "builtin", "{list}");
    assert_eq!(item(&list, "Agents")["use"], "builtin", "{list}");
    assert!(
        list["result"]
            .as_array()
            .unwrap()
            .iter()
            .all(|item| item["name"] != "workflow"),
        "{list}"
    );
    let absent = client.rpc("workflow.status", json!({})).await;
    assert_eq!(absent["error"]["code"], -32601, "{absent}");
    assert_eq!(std::fs::read(&workflow_settings).unwrap(), settings_bytes);
    assert_eq!(std::fs::read(&workflow_runs).unwrap(), runs_bytes);

    // Installed without being enabled, Workflow stays absent across a
    // restart and leaves its files alone.
    assert_ok(
        &call(
            &mut client,
            "plugin.install",
            json!({"source":{"path":workflow},"enable":false}),
        )
        .await,
    );
    drop(client);
    server.shutdown();
    server.join().await.unwrap();

    let (server, _requests, _) = start(config).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let list = call(&mut client, "plugin.list", json!({})).await;
    assert!(item(&list, "workflow")["use"].is_null(), "{list}");
    let absent = client.rpc("workflow.status", json!({})).await;
    assert_eq!(absent["error"]["code"], -32601, "{absent}");
    assert_eq!(std::fs::read(&workflow_settings).unwrap(), settings_bytes);
    assert_eq!(std::fs::read(&workflow_runs).unwrap(), runs_bytes);

    // Enabling the installed plugin runs it, and its Settings source
    // follows it.
    assert_ok(
        &call(
            &mut client,
            "plugin.enable",
            json!({"name":"workflow","use":"workflow"}),
        )
        .await,
    );
    let list = running(&mut client, &["workflow"]).await;
    assert_eq!(item(&list, "workflow")["use"], "workflow", "{list}");
    assert_ok(&call(&mut client, "workflow.status", json!({})).await);

    let read = json!({"source": "workflow", "scope": "user"});
    assert_ok(&call(&mut client, "settings.read", read.clone()).await);
    assert_ok(&call(&mut client, "plugin.disable", json!({"name": "workflow"})).await);
    let gone = client.rpc("settings.read", read.clone()).await;
    assert_eq!(app_error(&gone), "settings_unknown_source", "{gone}");
    assert_ok(&call(&mut client, "plugin.enable", json!({"name": "workflow"})).await);
    running(&mut client, &["workflow"]).await;
    assert_ok(&call(&mut client, "settings.read", read).await);

    // §6 `config`: the enabled plugin's Settings source through the `plugin` tool.
    select_fixture_model(&mut client).await;
    let tools = json!(["plugin"]);
    let mut params = create(tools.clone());
    params["permissionPolicy"]["allowedTools"] = tools;
    let created = client.rpc("session.create", params).await;
    assert_ok(&created);
    let session = created["result"]["sessionId"].as_str().unwrap().to_owned();
    let shown = plugin_reply(&mut client, &server, &session, "config workflow").await;
    let source = &shown["sources"][0];
    assert_eq!(source["source"], "workflow", "{shown}");
    assert_eq!(source["document"]["max_concurrent_runs"], 4, "{shown}");
    assert_eq!(
        source["schema"]["properties"]["max_concurrent_runs"]["maximum"],
        9007199254740991_u64
    );
    let set = "config workflow set max_concurrent_runs 5";
    let updated = plugin_reply(&mut client, &server, &session, set).await;
    assert_eq!(updated["document"]["max_concurrent_runs"], 5, "{updated}");
    let read = json!({"source": "workflow", "scope": "user"});
    let stored = call(&mut client, "settings.read", read).await;
    assert_eq!(
        stored["result"]["document"]["max_concurrent_runs"], 5,
        "{stored}"
    );
    let (refused, error) = plugin(
        &mut client,
        &server,
        &session,
        "config workflow set max_concurrent_runs 0",
    )
    .await;
    assert!(
        error && refused.contains("max_concurrent_runs"),
        "{refused}"
    );

    // A `writeOnly` member (web's search key and proxy) is a secret toward
    // the agent: never returned, never written through the tool.
    let read = json!({"source": "web", "scope": "user"});
    let current = call(&mut client, "settings.read", read.clone()).await;
    assert_eq!(
        current["result"]["document"]["proxyDns"], false,
        "{current}"
    );
    let secret = "dim-test-secret-key";
    let document = json!({"allowPrivateHosts": [], "proxy": "http://user:pass@127.0.0.1:9", "proxyDns": false,
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
    let (refused, error) = plugin(
        &mut client,
        &server,
        &session,
        "config web set proxyDns true",
    )
    .await;
    assert!(error && refused.contains("/proxyDns"), "{refused}");
    let stored = call(&mut client, "settings.read", read).await;
    assert_eq!(stored["result"]["document"], document, "{stored}");
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_former_default_plugin_uses_its_builtin_and_preserves_source_choices() {
    let home = home();
    let config = config(&home);
    let path = home.join("plugins.json");
    let original =
        json!({"version":1,"installed":[],"names":{"web":{"use":"default"}}}).to_string();
    std::fs::write(&path, &original).unwrap();
    let (server, _, _) = start(config.clone()).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let list = running(&mut client, &["web"]).await;
    assert_eq!(item(&list, "web")["use"], "builtin", "{list}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    assert_ok(&call(&mut client, "plugin.disable", json!({"name":"web"})).await);
    drop(client);
    server.shutdown();
    server.join().await.unwrap();

    let (server, _, _) = start(config).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let list = call(&mut client, "plugin.list", json!({})).await;
    assert_eq!(item(&list, "web")["status"], "off", "{list}");
    assert_ok(&call(&mut client, "plugin.enable", json!({"name":"web"})).await);
    running(&mut client, &["web"]).await;
    let source = plugin_dir(&home.join("src"), "web");
    assert_ok(
        &call(
            &mut client,
            "plugin.install",
            json!({"id":"web-replacement","source":{"path":source}}),
        )
        .await,
    );
    assert_ok(
        &call(
            &mut client,
            "plugin.enable",
            json!({"name":"web","use":"web-replacement"}),
        )
        .await,
    );
    let list = call(&mut client, "plugin.list", json!({})).await;
    assert_eq!(item(&list, "web")["use"], "web-replacement", "{list}");
    let absent = call(
        &mut client,
        "settings.read",
        json!({"source":"web","scope":"user"}),
    )
    .await;
    assert_eq!(app_error(&absent), "settings_unknown_source", "{absent}");
    assert_ok(
        &call(
            &mut client,
            "plugin.enable",
            json!({"name":"web","use":"builtin"}),
        )
        .await,
    );
    running(&mut client, &["web"]).await;
    assert_ok(
        &call(
            &mut client,
            "settings.read",
            json!({"source":"web","scope":"user"}),
        )
        .await,
    );
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
#[ignore = "requires DIM_TEST_NODE, DIM_TEST_JS_RUNTIME and DIM_TEST_PYTHON"]
async fn python_plugin_installs_and_reloads_through_its_link() {
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
pub(super) fn product_builder() -> AgentBuilder {
    let mut builder = AgentBuilder::new(AgentConfig::default());
    defaults::install_local_execution(
        &mut builder,
        dim_agent_capabilities::model_provider::ModelProviderPlugin::new,
        dim_agent_capabilities::workspace_tools::WorkspaceToolsConfig {
            boundary: None,
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
    let capabilities = dim_agent_types::CatalogCapabilities::with_limits(32000, 128);
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

/// A `write_content_plugin` at `<root>/<name>` whose `mcp.json` PluginManager
/// cannot read: a document that is not JSON, or a directory in its place.
fn write_unreadable_mcp_plugin(root: &Path, name: &str, directory: bool) -> PathBuf {
    let dir = root.join(name);
    write_content_plugin(
        &dir,
        name,
        &[name],
        json!({"tool": {"command": "dim-test-absent-mcp-server"}}),
    );
    if directory {
        std::fs::remove_file(dir.join("mcp.json")).unwrap();
        std::fs::create_dir(dir.join("mcp.json")).unwrap();
    } else {
        std::fs::write(dir.join("mcp.json"), r#"{"mcpServers": {},}"#).unwrap();
    }
    dir
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
        json!([{"id":"kit:docs","source":"plugin","enabled":true,"transport":"stdio","referencedVariables":[],"unsetVariables":[]}]),
        "{catalog}"
    );
    let entry = client
        .rpc(
            "mcp.configuration",
            json!({"source":"plugin","id":"kit:docs"}),
        )
        .await;
    assert_ok(&entry);
    assert_eq!(entry["result"]["enabled"], true);
    assert!(
        entry["result"].get("revision").is_none(),
        "a plugin entry has no Settings revision: {entry}"
    );
    assert!(
        entry["result"]["configuration"]["cwd"].is_null(),
        "the edit read keeps the plugin's declared configuration: {entry}"
    );
    let inspection = client
        .rpc("mcp.inspect", json!({"source":"plugin","id":"kit:docs"}))
        .await;
    assert_ok(&inspection);
    let installed = inspection["result"]["configuration"]["cwd"]
        .as_str()
        .unwrap_or_else(|| panic!("a plugin entry runs in its installed directory: {inspection}"));
    assert!(
        std::fs::canonicalize(installed)
            .unwrap()
            .starts_with(std::fs::canonicalize(&home).unwrap()),
        "a plugin entry runs inside its installed directory: {inspection}"
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

/// The product's local execution profile with mock model hosts, with Hooks
/// installed: a Session mounts the Hooks parts and reads its hook sources.
fn hooks_builder(home: &Path) -> AgentBuilder {
    let mut builder = product_builder();
    dim_app_server::hooks_settings::install(&mut builder, Some(home.to_owned()));
    dim_agent_capabilities::hooks::install(&mut builder);
    builder
}

/// A plugin carrying a Skill and a `hooks.json` whose `Stop` handler appends
/// `<tag> <DIM_PLUGIN_ROOT> <CLAUDE_PLUGIN_ROOT or unset>` to `runs`.
fn write_hook_plugin(dir: &Path, name: &str, runs: &Path, tag: &str) {
    let skill = dir.join("skills/review");
    std::fs::create_dir_all(&skill).unwrap();
    std::fs::write(
        skill.join("SKILL.md"),
        "---\nname: review\ndescription: The review skill\n---\nReview.\n",
    )
    .unwrap();
    let command = format!(
        "printf '{tag} %s %s\\n' \"$DIM_PLUGIN_ROOT\" \"${{CLAUDE_PLUGIN_ROOT-unset}}\" >> {}",
        runs.display()
    );
    std::fs::write(
        dir.join("hooks.json"),
        json!({"hooks": {"Stop": [{"hooks": [{"type": "command", "command": command}]}]}})
            .to_string(),
    )
    .unwrap();
    std::fs::write(
        dir.join("dim-plugin.json"),
        json!({"name": name, "skills": "skills", "hooks": "hooks.json"}).to_string(),
    )
    .unwrap();
}

fn lines(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

/// Waits until `path` holds `count` lines and returns them.
async fn wait_lines(path: &Path, count: usize) -> Vec<String> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let found = lines(path);
        if found.len() >= count {
            return found;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "{} lines were not written: {found:?}",
            count
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Creates a Session in `cwd`.
async fn session_in(client: &mut Client, cwd: &Path) -> String {
    let created = client.rpc("session.create", json!({"cwd": cwd})).await;
    assert_ok(&created);
    created["result"]["sessionId"].as_str().unwrap().to_owned()
}

/// Runs one turn on `session`; its completed `turn/end` is the `Stop` point.
async fn turn(client: &mut Client, session: &str) {
    submit(client, session, "hello").await;
}

/// Hooks A.2 and PluginManager §3.7: a plugin's hook document reaches
/// Sessions through Hooks under the source `plugin:<id>`; its commands read
/// `DIM_PLUGIN_ROOT` and never `CLAUDE_PLUGIN_ROOT`; turning it off, switching
/// the name to another plugin and removing that plugin withdraw its handlers;
/// the registration survives a restart.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn plugin_hooks_run_with_their_plugin_root_and_follow_the_record() {
    let home = home();
    let mut config = config(&home);
    config.unix_path = Some(home.join("ipc/server.sock"));
    let work = home.join("work");
    std::fs::create_dir_all(&work).unwrap();
    let runs = home.join("hook-runs");
    let barrier = home.join("hook-points");
    // The user's own `Stop` handler marks each point that ran; the branch
    // drains points in order, so a later mark means earlier points finished.
    std::fs::write(
        home.join("hooks.json"),
        json!({"hooks": {"Stop": [{"hooks": [{
            "type": "command",
            "command": format!("printf 'point\\n' >> {}", barrier.display()),
        }]}]}})
        .to_string(),
    )
    .unwrap();
    let server = serve(hooks_builder(&home), config.clone()).await.unwrap();
    let mut client = Client::new_version(&server, false, 3).await;
    select_fixture_model(&mut client).await;

    let source = home.join("src/kit");
    write_hook_plugin(&source, "kit", &runs, "kit");
    let kit = std::fs::canonicalize(&source).unwrap();
    let installed = call(
        &mut client,
        "plugin.install",
        json!({"source":{"path":source}}),
    )
    .await;
    assert_eq!(installed["result"]["enabled"], true, "{installed}");
    assert!(installed["result"].get("refused").is_none(), "{installed}");
    let catalog = when_served(&mut client, "hooks.catalog", json!({})).await;
    let sources: Vec<&Value> = catalog["result"]["sources"]
        .as_array()
        .unwrap_or_else(|| panic!("{catalog}"))
        .iter()
        .map(|source| &source["source"])
        .collect();
    assert_eq!(sources, [&json!("user"), &json!("plugin:kit")], "{catalog}");
    assert_eq!(
        catalog["result"]["sources"][1]["enabled"], true,
        "{catalog}"
    );
    assert_eq!(
        catalog["result"]["sources"][1]["path"],
        kit.to_string_lossy().as_ref(),
        "{catalog}"
    );
    assert_eq!(
        catalog["result"]["hooks"][1]["source"], "plugin:kit",
        "{catalog}"
    );

    let session = session_in(&mut client, &work).await;
    turn(&mut client, &session).await;
    let written = wait_lines(&runs, 1).await;
    assert_eq!(
        written,
        [format!("kit {} unset", kit.display())],
        "{written:?}"
    );
    wait_lines(&barrier, 1).await;

    // One branch drains its hook points in order, so a later mark is written
    // only after an earlier point, which would have run a withdrawn handler,
    // completed.
    assert_ok(&call(&mut client, "plugin.disable", json!({"name":"kit"})).await);
    let catalog = when_served(&mut client, "hooks.catalog", json!({})).await;
    assert_eq!(
        catalog["result"]["sources"].as_array().unwrap().len(),
        1,
        "{catalog}"
    );
    turn(&mut client, &session).await;
    turn(&mut client, &session).await;
    wait_lines(&barrier, 3).await;
    assert_eq!(lines(&runs), written, "a disabled plugin's handler ran");

    assert_ok(&call(&mut client, "plugin.enable", json!({"name":"kit"})).await);
    turn(&mut client, &session).await;
    let written = wait_lines(&runs, 2).await;
    assert_eq!(
        written.last().unwrap(),
        &format!("kit {} unset", kit.display())
    );
    wait_lines(&barrier, 4).await;
    drop(client);
    server.shutdown();
    server.join().await.unwrap();

    // The registration is kept in `hooks-plugins`, so a new process restores
    // it without being told again.
    let server = serve(hooks_builder(&home), config).await.unwrap();
    let mut client = Client::new_version(&server, false, 3).await;
    when_served(&mut client, "plugin.list", json!({})).await;
    let session = session_in(&mut client, &work).await;
    turn(&mut client, &session).await;
    let written = wait_lines(&runs, 3).await;
    assert_eq!(
        written.last().unwrap(),
        &format!("kit {} unset", kit.display())
    );
    wait_lines(&barrier, 5).await;

    // The name switches to another plugin: the old plugin's handler is
    // withdrawn, so the next two points write the new one's line only.
    let before = written.len();
    let second = home.join("src/kit-two");
    write_hook_plugin(&second, "kit", &runs, "second");
    let second = std::fs::canonicalize(&second).unwrap();
    let installed = call(
        &mut client,
        "plugin.install",
        json!({"source":{"path":home.join("src/kit-two")},"id":"kit2"}),
    )
    .await;
    assert!(installed["result"].get("refused").is_none(), "{installed}");
    turn(&mut client, &session).await;
    turn(&mut client, &session).await;
    wait_lines(&barrier, 7).await;
    wait_lines(&runs, before + 2).await;
    let added: Vec<String> = lines(&runs)[before..].to_vec();
    assert_eq!(
        added,
        [
            format!("second {} unset", second.display()),
            format!("second {} unset", second.display()),
        ],
        "{added:?}"
    );
    let written = lines(&runs);

    // Removing the plugin the name uses leaves nothing under that name, and
    // its handler no longer runs.
    assert_ok(&call(&mut client, "plugin.remove", json!({"id":"kit2"})).await);
    let catalog = when_served(&mut client, "hooks.catalog", json!({})).await;
    assert_eq!(
        catalog["result"]["sources"].as_array().unwrap().len(),
        1,
        "{catalog}"
    );
    turn(&mut client, &session).await;
    turn(&mut client, &session).await;
    wait_lines(&barrier, 9).await;
    assert_eq!(lines(&runs), written, "a removed plugin's handler ran");
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// PluginManager §3.7: a `hooks` document that does not parse is reported
/// under `refused` and stops none of the plugin's other content; a later
/// reload of a valid document registers it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_refused_plugin_hook_document_stops_only_its_own_kind() {
    let home = home();
    let mut config = config(&home);
    config.unix_path = Some(home.join("ipc/server.sock"));
    let work = home.join("work");
    std::fs::create_dir_all(&work).unwrap();
    let runs = home.join("hook-runs");
    let source = home.join("src/kit");
    write_hook_plugin(&source, "kit", &runs, "kit");
    std::fs::write(source.join("hooks.json"), json!({"hooks": []}).to_string()).unwrap();
    let server = serve(hooks_builder(&home), config.clone()).await.unwrap();
    let mut client = Client::new_version(&server, false, 3).await;
    select_fixture_model(&mut client).await;

    let installed = call(
        &mut client,
        "plugin.install",
        json!({"source":{"path":source}}),
    )
    .await;
    let kinds: Vec<&Value> = installed["result"]["refused"]
        .as_array()
        .unwrap_or_else(|| panic!("{installed}"))
        .iter()
        .map(|refused| &refused["kind"])
        .collect();
    assert_eq!(kinds, [&json!("hooks")], "{installed}");
    let list = call(&mut client, "plugin.list", json!({})).await;
    assert_eq!(
        item(&list, "kit")["refused"],
        installed["result"]["refused"],
        "{list}"
    );
    assert_eq!(plugin_skill_ids(&mut client).await, ["plugin:kit:review"]);
    let catalog = when_served(&mut client, "hooks.catalog", json!({})).await;
    assert_eq!(
        catalog["result"]["sources"].as_array().unwrap().len(),
        1,
        "{catalog}"
    );
    drop(client);
    server.shutdown();
    server.join().await.unwrap();

    // Startup registers every plugin in use again, so a refused document is
    // reported again after a restart.
    let server = serve(hooks_builder(&home), config).await.unwrap();
    let mut client = Client::new_version(&server, false, 3).await;
    let list = call(&mut client, "plugin.list", json!({})).await;
    assert_eq!(item(&list, "kit")["refused"][0]["kind"], "hooks", "{list}");

    write_hook_plugin(&source, "kit", &runs, "kit");
    let reloaded = call(&mut client, "plugin.reload", json!({"name":"kit"})).await;
    assert!(reloaded["result"].get("refused").is_none(), "{reloaded}");
    let catalog = when_served(&mut client, "hooks.catalog", json!({})).await;
    assert_eq!(
        catalog["result"]["sources"][1]["source"], "plugin:kit",
        "{catalog}"
    );
    let session = session_in(&mut client, &work).await;
    turn(&mut client, &session).await;
    let written = wait_lines(&runs, 1).await;
    assert_eq!(written.len(), 1, "{written:?}");
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// PluginManager §3.7: a `hooks` content file PluginManager cannot read is
/// its own refusal, not only the reply's. A file that is not there fails the
/// manifest read (`invalid_manifest`), so the persisted case is a document
/// that is there and does not parse: `plugin.list` shows the refusal right
/// after the install and after a restart, the plugin's Skill is registered
/// throughout, and a reload of the fixed file clears it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_plugin_hooks_document_that_cannot_be_read_stays_refused() {
    let home = home();
    let mut config = config(&home);
    config.unix_path = Some(home.join("ipc/server.sock"));
    let work = home.join("work");
    std::fs::create_dir_all(&work).unwrap();
    let runs = home.join("hook-runs");
    let server = serve(hooks_builder(&home), config.clone()).await.unwrap();
    let mut client = Client::new_version(&server, false, 3).await;
    select_fixture_model(&mut client).await;

    // A declared hooks file that is not there is refused at install, before
    // any owner sees the plugin.
    let absent = home.join("src/absent");
    std::fs::create_dir_all(&absent).unwrap();
    std::fs::write(
        absent.join("dim-plugin.json"),
        json!({"name": "absent", "hooks": "absent.json"}).to_string(),
    )
    .unwrap();
    let refused = call(
        &mut client,
        "plugin.install",
        json!({"source":{"path":absent}}),
    )
    .await;
    assert_eq!(app_error(&refused), "invalid_manifest", "{refused}");

    // A hooks file that is there and does not parse is kept as PluginManager's
    // refusal of the kind: the reply names it, and `plugin.list` shows it.
    let source = home.join("src/kit");
    write_hook_plugin(&source, "kit", &runs, "kit");
    std::fs::write(source.join("hooks.json"), r#"{"hooks": [],}"#).unwrap();
    let installed = call(
        &mut client,
        "plugin.install",
        json!({"source":{"path":source}}),
    )
    .await;
    let kinds: Vec<&Value> = installed["result"]["refused"]
        .as_array()
        .unwrap_or_else(|| panic!("{installed}"))
        .iter()
        .map(|refused| &refused["kind"])
        .collect();
    assert_eq!(kinds, [&json!("hooks")], "{installed}");
    let list = call(&mut client, "plugin.list", json!({})).await;
    assert_eq!(
        item(&list, "kit")["refused"],
        installed["result"]["refused"],
        "{list}"
    );
    assert_eq!(plugin_skill_ids(&mut client).await, ["plugin:kit:review"]);
    let catalog = when_served(&mut client, "hooks.catalog", json!({})).await;
    assert_eq!(
        catalog["result"]["sources"].as_array().unwrap().len(),
        1,
        "{catalog}"
    );
    drop(client);
    server.shutdown();
    server.join().await.unwrap();

    // Startup registers every plugin in use again, so the refusal and the
    // plugin's other content survive a restart.
    let server = serve(hooks_builder(&home), config).await.unwrap();
    let mut client = Client::new_version(&server, false, 3).await;
    let list = call(&mut client, "plugin.list", json!({})).await;
    assert_eq!(item(&list, "kit")["refused"][0]["kind"], "hooks", "{list}");
    assert_eq!(plugin_skill_ids(&mut client).await, ["plugin:kit:review"]);
    let catalog = when_served(&mut client, "hooks.catalog", json!({})).await;
    assert_eq!(
        catalog["result"]["sources"].as_array().unwrap().len(),
        1,
        "{catalog}"
    );

    // The document reads on reload, is handed over, and its handler runs.
    write_hook_plugin(&source, "kit", &runs, "kit");
    let reloaded = call(&mut client, "plugin.reload", json!({"name":"kit"})).await;
    assert!(reloaded["result"].get("refused").is_none(), "{reloaded}");
    let list = call(&mut client, "plugin.list", json!({})).await;
    assert!(item(&list, "kit").get("refused").is_none(), "{list}");
    let catalog = when_served(&mut client, "hooks.catalog", json!({})).await;
    assert_eq!(
        catalog["result"]["sources"][1]["source"], "plugin:kit",
        "{catalog}"
    );
    let session = session_in(&mut client, &work).await;
    turn(&mut client, &session).await;
    wait_lines(&runs, 1).await;
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// PluginManager §3.3 and §3.7: an absent Hooks owner skips only its kind of
/// content, and turning HooksSettings on registers that kind for every plugin
/// in use.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn plugin_hooks_follow_the_hooks_owner() {
    let home = home();
    let mut config = config(&home);
    config.unix_path = Some(home.join("ipc/server.sock"));
    let work = home.join("work");
    std::fs::create_dir_all(&work).unwrap();
    let runs = home.join("hook-runs");
    let source = home.join("src/kit");
    write_hook_plugin(&source, "kit", &runs, "kit");
    let server = serve(hooks_builder(&home), config).await.unwrap();
    let mut client = Client::new_version(&server, false, 3).await;
    select_fixture_model(&mut client).await;

    assert_ok(
        &call(
            &mut client,
            "plugin.disable",
            json!({"name":"HooksSettings"}),
        )
        .await,
    );
    let installed = call(
        &mut client,
        "plugin.install",
        json!({"source":{"path":source}}),
    )
    .await;
    assert_eq!(
        installed["result"]["skipped"],
        json!(["hooks"]),
        "{installed}"
    );

    assert_ok(
        &call(
            &mut client,
            "plugin.enable",
            json!({"name":"HooksSettings"}),
        )
        .await,
    );
    let catalog = when_served(&mut client, "hooks.catalog", json!({})).await;
    assert_eq!(
        catalog["result"]["sources"][1]["source"], "plugin:kit",
        "{catalog}"
    );
    let session = session_in(&mut client, &work).await;
    turn(&mut client, &session).await;
    wait_lines(&runs, 1).await;
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// #951: a plugin's MCP document is one unit; one broken entry refuses the
/// whole document, so its valid sibling registers nothing either.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn plugin_mcp_document_with_one_broken_entry_registers_nothing() {
    let home = home();
    let mut config = config(&home);
    config.unix_path = Some(home.join("ipc/server.sock"));
    let server = serve(product_builder(), config).await.unwrap();
    let mut client = Client::new_version(&server, false, 3).await;
    select_fixture_model(&mut client).await;
    let work = home.join("work");
    std::fs::create_dir_all(&work).unwrap();
    let absent = json!({"command": "dim-test-absent-mcp-server"});
    let source = home.join("src/mixed");
    write_content_plugin(&source, "mixed", &["mixed"], json!({"good": absent}));
    let installed = call(
        &mut client,
        "plugin.install",
        json!({"source":{"path":source}}),
    )
    .await;
    assert!(installed["result"].get("refused").is_none(), "{installed}");
    assert_eq!(mcp_ids(&mut client).await, ["mixed:good"]);

    write_content_plugin(
        &source,
        "mixed",
        &["mixed"],
        json!({"good": absent, "bad": {"command": ""}}),
    );
    let reloaded = call(&mut client, "plugin.reload", json!({"name":"mixed"})).await;
    let refused = reloaded["result"]["refused"]
        .as_array()
        .unwrap_or_else(|| panic!("{reloaded}"));
    assert!(
        refused.iter().any(|item| item["kind"] == "mcp"),
        "{reloaded}"
    );
    assert!(
        mcp_ids(&mut client).await.is_empty(),
        "a refused plugin document registered an entry"
    );
    let list = call(&mut client, "plugin.list", json!({})).await;
    assert_eq!(item(&list, "mixed")["refused"][0]["kind"], "mcp", "{list}");
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// §3.7: a content file PluginManager cannot read — not JSON, or a directory
/// in its place — is its own refusal, not only the reply's: `plugin.list`
/// shows it right after the install and after a restart, while a reload of
/// the fixed file clears it. A missing file is still refused at install as
/// `invalid_manifest`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn plugin_mcp_document_that_cannot_be_read_stays_refused() {
    let home = home();
    let config = config(&home);
    let (server, _, _) = start(config.clone()).await;
    let mut client = Client::new_version(&server, false, 3).await;

    let missing = home.join("src/missing");
    std::fs::create_dir_all(&missing).unwrap();
    std::fs::write(
        missing.join("dim-plugin.json"),
        json!({"name": "missing", "mcp": "absent.json"}).to_string(),
    )
    .unwrap();
    let refused = call(
        &mut client,
        "plugin.install",
        json!({"source":{"path":missing}}),
    )
    .await;
    assert_eq!(app_error(&refused), "invalid_manifest", "{refused}");

    let absent = json!({"tool": {"command": "dim-test-absent-mcp-server"}});
    let unreadable = [
        write_unreadable_mcp_plugin(&home.join("src"), "kit", false),
        write_unreadable_mcp_plugin(&home.join("src"), "docs", true),
    ];
    for source in &unreadable {
        let installed = call(
            &mut client,
            "plugin.install",
            json!({"source":{"path":source}}),
        )
        .await;
        assert_eq!(
            installed["result"]["refused"][0]["kind"], "mcp",
            "{installed}"
        );
    }
    let list = call(&mut client, "plugin.list", json!({})).await;
    for name in ["kit", "docs"] {
        assert_eq!(item(&list, name)["refused"][0]["kind"], "mcp", "{list}");
    }
    let mut skills = plugin_skill_ids(&mut client).await;
    skills.sort();
    assert_eq!(skills, ["plugin:docs:docs", "plugin:kit:kit"]);
    assert!(mcp_ids(&mut client).await.is_empty());
    drop(client);
    server.shutdown();
    server.join().await.unwrap();

    let (server, _, _) = start(config).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let list = call(&mut client, "plugin.list", json!({})).await;
    for name in ["kit", "docs"] {
        assert_eq!(item(&list, name)["refused"][0]["kind"], "mcp", "{list}");
    }
    let mut skills = plugin_skill_ids(&mut client).await;
    skills.sort();
    assert_eq!(skills, ["plugin:docs:docs", "plugin:kit:kit"]);
    assert!(mcp_ids(&mut client).await.is_empty());

    for (name, source) in [("kit", &unreadable[0]), ("docs", &unreadable[1])] {
        if source.join("mcp.json").is_dir() {
            std::fs::remove_dir(source.join("mcp.json")).unwrap();
        }
        write_content_plugin(source, name, &[name], absent.clone());
        let reloaded = call(&mut client, "plugin.reload", json!({"name":name})).await;
        assert!(reloaded["result"].get("refused").is_none(), "{reloaded}");
    }
    let list = call(&mut client, "plugin.list", json!({})).await;
    for name in ["kit", "docs"] {
        assert!(item(&list, name).get("refused").is_none(), "{list}");
    }
    let mut ids = mcp_ids(&mut client).await;
    ids.sort();
    assert_eq!(ids, ["docs:tool", "kit:tool"]);
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// §3.1: a used plugin whose manifest cannot be read runs nothing: neither
/// its row nor its content of any kind. It shows as failed, and reloading
/// after the fix brings both back; a reload while it is broken changes
/// nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn plugin_broken_manifest_runs_none_of_its_content() {
    let home = home();
    let config = config(&home);
    let source = home.join("src/kit");
    let absent = json!({"tool": {"command": "dim-test-absent-mcp-server"}});
    write_content_plugin(&source, "kit", &["kit"], absent.clone());
    let (server, _, _) = start(config.clone()).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let installed = call(
        &mut client,
        "plugin.install",
        json!({"source":{"path":source}}),
    )
    .await;
    assert!(installed["result"].get("refused").is_none(), "{installed}");
    assert_eq!(mcp_ids(&mut client).await, ["kit:tool"]);
    assert_eq!(plugin_skill_ids(&mut client).await, ["plugin:kit:kit"]);
    drop(client);
    server.shutdown();
    server.join().await.unwrap();

    std::fs::write(source.join("dim-plugin.json"), r#"{"name": "kit",}"#).unwrap();
    let (server, _, _) = start(config).await;
    let mut client = Client::new_version(&server, false, 3).await;
    assert!(mcp_ids(&mut client).await.is_empty());
    assert!(plugin_skill_ids(&mut client).await.is_empty());
    let list = call(&mut client, "plugin.list", json!({})).await;
    assert_eq!(item(&list, "kit")["status"], "failed", "{list}");
    assert!(item(&list, "kit").get("refused").is_none(), "{list}");

    let reloaded = call(&mut client, "plugin.reload", json!({"name":"kit"})).await;
    assert_eq!(app_error(&reloaded), "invalid_manifest", "{reloaded}");
    assert!(mcp_ids(&mut client).await.is_empty());
    assert!(plugin_skill_ids(&mut client).await.is_empty());

    write_content_plugin(&source, "kit", &["kit"], absent);
    let reloaded = call(&mut client, "plugin.reload", json!({"name":"kit"})).await;
    assert!(reloaded["result"].get("refused").is_none(), "{reloaded}");
    assert_eq!(mcp_ids(&mut client).await, ["kit:tool"]);
    assert_eq!(plugin_skill_ids(&mut client).await, ["plugin:kit:kit"]);
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// §A.2: a refusal whose cause has a static reason reaches the client
/// with that reason, in the mutation reply and in `plugin.list`; a
/// refusal without one keeps the generic message at the client boundary.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_refusal_named_from_the_closed_set_reaches_the_client() {
    let home = home();
    let config = config(&home);
    let source = home.join("src/kit");
    std::fs::create_dir_all(&source).unwrap();
    std::fs::write(source.join("mcp.json"), r#"{"servers": {}}"#).unwrap();
    std::fs::write(
        source.join("dim-plugin.json"),
        json!({"name": "kit", "mcp": "mcp.json"}).to_string(),
    )
    .unwrap();
    let (server, _, _) = start(config).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let installed = call(
        &mut client,
        "plugin.install",
        json!({"source":{"path":source}}),
    )
    .await;
    assert_eq!(
        installed["result"]["refused"][0],
        json!({"kind": "mcp", "error": "MCP document must contain an mcpServers object"}),
        "{installed}"
    );
    let list = call(&mut client, "plugin.list", json!({})).await;
    assert_eq!(
        item(&list, "kit")["refused"][0],
        json!({"kind": "mcp", "error": "MCP document must contain an mcpServers object"}),
        "{list}"
    );

    // A document-shape cause with a static reason: the client reads the
    // owner's reason, never the `reason` member that carries it inside.
    std::fs::write(source.join("mcp.json"), r#"{"mcpServers": 5}"#).unwrap();
    let reloaded = call(&mut client, "plugin.reload", json!({"name":"kit"})).await;
    assert_eq!(
        reloaded["result"]["refused"][0],
        json!({"kind": "mcp", "error": "mcpServers must be a JSON object"}),
        "{reloaded}"
    );
    assert!(!reloaded.to_string().contains("\"reason\""), "{reloaded}");

    // A content file that is not JSON is PluginManager's own refusal, for
    // both kinds: the client reads its static reason, never the path, in
    // the mutation reply and in `plugin.list`.
    std::fs::write(source.join("mcp.json"), r#"{"mcpServers": {},}"#).unwrap();
    std::fs::write(source.join("hooks.json"), r#"{"hooks": {},}"#).unwrap();
    std::fs::write(
        source.join("dim-plugin.json"),
        json!({"name": "kit", "mcp": "mcp.json", "hooks": "hooks.json"}).to_string(),
    )
    .unwrap();
    let reloaded = call(&mut client, "plugin.reload", json!({"name":"kit"})).await;
    assert_eq!(
        reloaded["result"]["refused"],
        json!([
            {"kind": "mcp", "error": "the file is not valid JSON"},
            {"kind": "hooks", "error": "the file is not valid JSON"},
        ]),
        "{reloaded}"
    );
    let list = call(&mut client, "plugin.list", json!({})).await;
    assert_eq!(
        item(&list, "kit")["refused"],
        json!([
            {"kind": "mcp", "error": "the file is not valid JSON"},
            {"kind": "hooks", "error": "the file is not valid JSON"},
        ]),
        "{list}"
    );
    assert!(!reloaded.to_string().contains("mcp.json"), "{reloaded}");
    assert!(!reloaded.to_string().contains("hooks.json"), "{reloaded}");
    assert!(
        !reloaded.to_string().contains(home.to_str().unwrap()),
        "{reloaded}"
    );
    let refused = item(&list, "kit")["refused"].to_string();
    assert!(!refused.contains("mcp.json"), "{list}");
    assert!(!refused.contains("hooks.json"), "{list}");
    assert!(!refused.contains(home.to_str().unwrap()), "{list}");

    // A refusal whose cause carries no static reason keeps the generic
    // message at the client, in the mutation reply and in `plugin.list`:
    // a broken MCP entry refuses the document with the entry's own error.
    std::fs::write(source.join("mcp.json"), r#"{"mcpServers": {"leaky": 5}}"#).unwrap();
    std::fs::write(source.join("hooks.json"), r#"{"hooks": {}}"#).unwrap();
    let reloaded = call(&mut client, "plugin.reload", json!({"name":"kit"})).await;
    assert_eq!(
        reloaded["result"]["refused"],
        json!([{"kind": "mcp", "error": "Plugin content was refused"}]),
        "{reloaded}"
    );
    assert!(!reloaded.to_string().contains("leaky"), "{reloaded}");
    let list = call(&mut client, "plugin.list", json!({})).await;
    assert_eq!(
        item(&list, "kit")["refused"],
        json!([{"kind": "mcp", "error": "Plugin content was refused"}]),
        "{list}"
    );
    assert!(!list.to_string().contains("leaky"), "{list}");

    // A content path that resolves to a directory passes `Manifest::read`
    // and fails the read itself: the client reads that reason, not the path.
    std::fs::remove_file(source.join("mcp.json")).unwrap();
    std::fs::create_dir(source.join("mcp.json")).unwrap();
    let reloaded = call(&mut client, "plugin.reload", json!({"name":"kit"})).await;
    assert_eq!(
        reloaded["result"]["refused"],
        json!([{"kind": "mcp", "error": "the file could not be read"}]),
        "{reloaded}"
    );
    assert!(!reloaded.to_string().contains("mcp.json"), "{reloaded}");
    let list = call(&mut client, "plugin.list", json!({})).await;
    assert_eq!(
        item(&list, "kit")["refused"],
        json!([{"kind": "mcp", "error": "the file could not be read"}]),
        "{list}"
    );
    assert!(!list.to_string().contains("mcp.json"), "{list}");
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// §3.7: with the owner off a kind is skipped and no refusal is kept;
/// turning the owner on reads the file, and `plugin.list` shows the
/// refusal.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn plugin_refusal_is_kept_once_its_owner_is_present() {
    let home = home();
    let config = config(&home);
    let source = write_unreadable_mcp_plugin(&home.join("src"), "kit", false);
    let (server, _, _) = start(config).await;
    let mut client = Client::new_version(&server, false, 3).await;
    assert_ok(&call(&mut client, "plugin.disable", json!({"name":"Mcp"})).await);
    let installed = call(
        &mut client,
        "plugin.install",
        json!({"source":{"path":source}}),
    )
    .await;
    assert_eq!(installed["result"]["skipped"][0], "mcp", "{installed}");
    let list = call(&mut client, "plugin.list", json!({})).await;
    assert!(item(&list, "kit").get("refused").is_none(), "{list}");

    assert_ok(&call(&mut client, "plugin.enable", json!({"name":"Mcp"})).await);
    let list = call(&mut client, "plugin.list", json!({})).await;
    assert_eq!(item(&list, "kit")["refused"][0]["kind"], "mcp", "{list}");
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// §3.7: dropping a refused kind from the manifest withdraws it, and the
/// refusal goes with it, so re-adding the kind while its owner is off keeps
/// nothing to show.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn plugin_refusal_is_cleared_when_the_kind_is_withdrawn() {
    let home = home();
    let config = config(&home);
    let source = write_unreadable_mcp_plugin(&home.join("src"), "kit", false);
    let (server, _, _) = start(config).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let installed = call(
        &mut client,
        "plugin.install",
        json!({"source":{"path":source}}),
    )
    .await;
    assert_eq!(
        installed["result"]["refused"][0]["kind"], "mcp",
        "{installed}"
    );
    let reloaded = call(&mut client, "plugin.reload", json!({"name":"kit"})).await;
    assert_eq!(
        reloaded["result"]["refused"][0]["kind"], "mcp",
        "{reloaded}"
    );

    std::fs::write(
        source.join("dim-plugin.json"),
        json!({"name":"kit","skills":"skills"}).to_string(),
    )
    .unwrap();
    let reloaded = call(&mut client, "plugin.reload", json!({"name":"kit"})).await;
    assert!(reloaded["result"].get("refused").is_none(), "{reloaded}");
    let list = call(&mut client, "plugin.list", json!({})).await;
    assert!(item(&list, "kit").get("refused").is_none(), "{list}");

    write_content_plugin(
        &source,
        "kit",
        &["kit"],
        json!({"tool": {"command": "dim-test-absent-mcp-server"}}),
    );
    assert_ok(&call(&mut client, "plugin.disable", json!({"name":"Mcp"})).await);
    let reloaded = call(&mut client, "plugin.reload", json!({"name":"kit"})).await;
    assert_eq!(reloaded["result"]["skipped"][0], "mcp", "{reloaded}");
    let list = call(&mut client, "plugin.list", json!({})).await;
    assert!(item(&list, "kit").get("refused").is_none(), "{list}");
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// §3.7: a kind skipped because its owner is absent keeps no refusal — nothing
/// was read — and `plugin.list` shows none while that owner is off.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn plugin_refusal_is_dropped_while_its_owner_is_off() {
    let home = home();
    let config = config(&home);
    let source = write_unreadable_mcp_plugin(&home.join("src"), "kit", false);
    let (server, _, _) = start(config).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let installed = call(
        &mut client,
        "plugin.install",
        json!({"source":{"path":source}}),
    )
    .await;
    assert_eq!(
        installed["result"]["refused"][0]["kind"], "mcp",
        "{installed}"
    );
    let list = call(&mut client, "plugin.list", json!({})).await;
    assert_eq!(item(&list, "kit")["refused"][0]["kind"], "mcp", "{list}");

    assert_ok(&call(&mut client, "plugin.disable", json!({"name":"Mcp"})).await);
    let list = call(&mut client, "plugin.list", json!({})).await;
    assert!(item(&list, "kit").get("refused").is_none(), "{list}");

    std::fs::write(
        source.join("dim-plugin.json"),
        json!({"name": "kit", "skills": "skills"}).to_string(),
    )
    .unwrap();
    assert_ok(&call(&mut client, "plugin.reload", json!({"name":"kit"})).await);
    let list = call(&mut client, "plugin.list", json!({})).await;
    assert!(item(&list, "kit").get("refused").is_none(), "{list}");

    write_content_plugin(
        &source,
        "kit",
        &["kit"],
        json!({"tool": {"command": "dim-test-absent-mcp-server"}}),
    );
    let reloaded = call(&mut client, "plugin.reload", json!({"name":"kit"})).await;
    assert_eq!(reloaded["result"]["skipped"][0], "mcp", "{reloaded}");
    let list = call(&mut client, "plugin.list", json!({})).await;
    assert!(item(&list, "kit").get("refused").is_none(), "{list}");

    assert_ok(&call(&mut client, "plugin.enable", json!({"name":"Mcp"})).await);
    let list = call(&mut client, "plugin.list", json!({})).await;
    assert!(item(&list, "kit").get("refused").is_none(), "{list}");
    assert_eq!(mcp_ids(&mut client).await, ["kit:tool"]);
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// §3.1 and §3.4: a reload refused for its manifest changes nothing, and the
/// next change of the record, from any command, reads the manifest again and
/// runs none of the plugin's content while it cannot be read.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn plugin_broken_manifest_content_stops_on_the_next_change() {
    let home = home();
    let config = config(&home);
    let source = home.join("src/kit");
    let absent = json!({"tool": {"command": "dim-test-absent-mcp-server"}});
    write_content_plugin(&source, "kit", &["kit"], absent.clone());
    let (server, _, _) = start(config).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let installed = call(
        &mut client,
        "plugin.install",
        json!({"source":{"path":source}}),
    )
    .await;
    assert!(installed["result"].get("refused").is_none(), "{installed}");
    assert_eq!(mcp_ids(&mut client).await, ["kit:tool"]);
    assert_eq!(plugin_skill_ids(&mut client).await, ["plugin:kit:kit"]);

    std::fs::write(source.join("dim-plugin.json"), r#"{"name": "kit",}"#).unwrap();
    let reloaded = call(&mut client, "plugin.reload", json!({"name":"kit"})).await;
    assert_eq!(app_error(&reloaded), "invalid_manifest", "{reloaded}");
    assert_eq!(mcp_ids(&mut client).await, ["kit:tool"]);
    assert_eq!(plugin_skill_ids(&mut client).await, ["plugin:kit:kit"]);

    let other = home.join("src/extra");
    write_content_plugin(&other, "extra", &["extra"], absent.clone());
    assert_ok(
        &call(
            &mut client,
            "plugin.install",
            json!({"source":{"path":other}}),
        )
        .await,
    );
    let list = call(&mut client, "plugin.list", json!({})).await;
    assert_eq!(item(&list, "kit")["status"], "failed", "{list}");
    assert!(item(&list, "kit").get("refused").is_none(), "{list}");
    assert_eq!(mcp_ids(&mut client).await, ["extra:tool"]);
    let mut skills = plugin_skill_ids(&mut client).await;
    skills.sort();
    assert_eq!(skills, ["plugin:extra:extra"]);

    write_content_plugin(&source, "kit", &["kit"], absent);
    let reloaded = call(&mut client, "plugin.reload", json!({"name":"kit"})).await;
    assert!(reloaded["result"].get("refused").is_none(), "{reloaded}");
    let mut ids = mcp_ids(&mut client).await;
    ids.sort();
    assert_eq!(ids, ["extra:tool", "kit:tool"]);
    let mut skills = plugin_skill_ids(&mut client).await;
    skills.sort();
    assert_eq!(skills, ["plugin:extra:extra", "plugin:kit:kit"]);
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// §3.7: removing a refused plugin drops its entry and its refusal; the same
/// id installed again with a readable document shows none.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn plugin_refusal_goes_with_the_removed_plugin() {
    let home = home();
    let config = config(&home);
    let source = write_unreadable_mcp_plugin(&home.join("src"), "kit", false);
    let absent = json!({"tool": {"command": "dim-test-absent-mcp-server"}});
    let (server, _, _) = start(config).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let installed = call(
        &mut client,
        "plugin.install",
        json!({"source":{"path":source}}),
    )
    .await;
    assert_eq!(
        installed["result"]["refused"][0]["kind"], "mcp",
        "{installed}"
    );

    assert_ok(&call(&mut client, "plugin.remove", json!({"id":"kit"})).await);
    let list = call(&mut client, "plugin.list", json!({})).await;
    assert!(
        list["result"]
            .as_array()
            .unwrap_or_else(|| panic!("{list}"))
            .iter()
            .all(|item| item["name"] != "kit"),
        "{list}"
    );

    write_content_plugin(&source, "kit", &["kit"], absent);
    let installed = call(
        &mut client,
        "plugin.install",
        json!({"source":{"path":source}}),
    )
    .await;
    assert!(installed["result"].get("refused").is_none(), "{installed}");
    let list = call(&mut client, "plugin.list", json!({})).await;
    assert!(item(&list, "kit").get("refused").is_none(), "{list}");
    assert_eq!(mcp_ids(&mut client).await, ["kit:tool"]);
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// §3.7: switching a name away from a refused plugin withdraws its content
/// and its refusal; the plugin switched to shows none, and switching back
/// while the owner is off reads nothing and shows none either.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn plugin_refusal_does_not_follow_a_switch() {
    let home = home();
    let config = config(&home);
    let refused = write_unreadable_mcp_plugin(&home.join("src"), "kit", false);
    let clean = home.join("src/b");
    write_content_plugin(
        &clean,
        "kit",
        &["kit2"],
        json!({"tool": {"command": "dim-test-absent-mcp-server"}}),
    );
    let (server, _, _) = start(config).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let installed = call(
        &mut client,
        "plugin.install",
        json!({"source":{"path":refused}}),
    )
    .await;
    assert_eq!(
        installed["result"]["refused"][0]["kind"], "mcp",
        "{installed}"
    );

    let installed = call(
        &mut client,
        "plugin.install",
        json!({"source":{"path":clean}, "id":"kit2"}),
    )
    .await;
    assert!(installed["result"].get("refused").is_none(), "{installed}");
    let list = call(&mut client, "plugin.list", json!({})).await;
    assert!(item(&list, "kit").get("refused").is_none(), "{list}");
    assert_eq!(mcp_ids(&mut client).await, ["kit2:tool"]);

    assert_ok(&call(&mut client, "plugin.disable", json!({"name":"Mcp"})).await);
    let enabled = call(
        &mut client,
        "plugin.enable",
        json!({"name":"kit", "use":"kit"}),
    )
    .await;
    assert_eq!(enabled["result"]["skipped"][0], "mcp", "{enabled}");
    let list = call(&mut client, "plugin.list", json!({})).await;
    assert!(item(&list, "kit").get("refused").is_none(), "{list}");

    assert_ok(&call(&mut client, "plugin.enable", json!({"name":"Mcp"})).await);
    let list = call(&mut client, "plugin.list", json!({})).await;
    assert_eq!(item(&list, "kit")["refused"][0]["kind"], "mcp", "{list}");
    assert!(mcp_ids(&mut client).await.is_empty());
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// §3.7: an owner's refusal shows while the document the owner read stands;
/// once the file cannot be read, PluginManager's own refusal replaces it.
/// The client boundary masks both texts, so the `plugin` tool's `list`
/// result names them; a readable document clears both.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn plugin_refusal_replaces_the_owner_refusal() {
    let home = home();
    let mut config = config(&home);
    config.unix_path = Some(home.join("ipc/server.sock"));
    let source = home.join("src/kit");
    let absent = json!({"tool": {"command": "dim-test-absent-mcp-server"}});
    write_content_plugin(&source, "kit", &["kit"], json!({"tool": {"command": ""}}));
    let server = serve(product_builder(), config).await.unwrap();
    let mut client = Client::new_version(&server, false, 3).await;
    select_fixture_model(&mut client).await;
    let work = home.join("work");
    std::fs::create_dir_all(&work).unwrap();
    let full = json!({"preset": "full-access", "allowedTools": [], "disallowedTools": []});
    let created = client
        .rpc("session.create", session_with(full, &work))
        .await;
    assert_ok(&created);
    let session = created["result"]["sessionId"].as_str().unwrap().to_owned();
    let listed = |list: Value| -> String {
        let item = list
            .as_array()
            .unwrap_or_else(|| panic!("{list}"))
            .iter()
            .find(|item| item["name"] == "kit")
            .unwrap_or_else(|| panic!("{list}"));
        item["refused"][0]["error"].as_str().unwrap().to_owned()
    };

    let installed = call(
        &mut client,
        "plugin.install",
        json!({"source":{"path":source}}),
    )
    .await;
    assert_eq!(
        installed["result"]["refused"][0]["kind"], "mcp",
        "{installed}"
    );
    let owner = listed(plugin_reply(&mut client, &server, &session, "list").await);
    assert!(!owner.is_empty());

    std::fs::write(source.join("mcp.json"), r#"{"mcpServers": {},}"#).unwrap();
    let reloaded = call(&mut client, "plugin.reload", json!({"name":"kit"})).await;
    assert_eq!(
        reloaded["result"]["refused"][0]["kind"], "mcp",
        "{reloaded}"
    );
    let manager = listed(plugin_reply(&mut client, &server, &session, "list").await);
    assert_ne!(manager, owner);

    write_content_plugin(&source, "kit", &["kit"], absent);
    let reloaded = call(&mut client, "plugin.reload", json!({"name":"kit"})).await;
    assert!(reloaded["result"].get("refused").is_none(), "{reloaded}");
    let list = call(&mut client, "plugin.list", json!({})).await;
    assert!(item(&list, "kit").get("refused").is_none(), "{list}");
    assert_eq!(mcp_ids(&mut client).await, ["kit:tool"]);
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// Rewrites the shipped optional fixture's manifest version.
fn set_shipped_version(dir: &Path, version: &str) {
    let path = dir.join("dim-plugin.json");
    let mut manifest: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    manifest["version"] = json!(version);
    std::fs::write(&path, manifest.to_string()).unwrap();
}

/// The npm body a re-acquisition's dependency install runs: it leaves
/// `node_modules` behind.
const INSTALL_FIXTURE_DEP: &str = "mkdir -p node_modules/fixture-dep\necho 'export default 1' > node_modules/fixture-dep/index.js";

/// §3.1 step 4: a shipped optional plugin whose version changed between two
/// starts is re-acquired in the background after content startup, behind the
/// file system queue. The old copy keeps serving until the new one is
/// recorded, clients are not blocked meanwhile, and the next start deletes
/// the replaced copy.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_changed_shipped_version_is_re_acquired_after_startup() {
    let home = home();
    let shipped = optional_plugin(&home.join("shipped"), "ocr-fixture", "1.0.0", true);
    let first = FixtureNode::with("24.21.0", INSTALL_FIXTURE_DEP);
    let config = runtime_config(
        &home,
        defaults_file(json!({}), json!({"ocr-fixture": "shipped/ocr-fixture"})),
        first.search_path(),
    );
    let (server, _, _) = start(config.clone()).await;
    let mut client = Client::new_version(&server, false, 3).await;
    assert_ok(
        &call(
            &mut client,
            "plugin.install",
            json!({"source": {"optional": "ocr-fixture"}}),
        )
        .await,
    );
    let old = recorded_dir(&home, "ocr-fixture");
    assert!(name_of(&old).starts_with("1.0.0+"), "{old:?}");
    drop(client);
    server.shutdown();
    server.join().await.unwrap();

    // The shipped version changes, and the re-acquisition's npm waits.
    set_shipped_version(&shipped, "1.1.0");
    let started = home.join("npm-started");
    let release = home.join("npm-release");
    let gate = FixtureNode::with(
        "24.21.0",
        &format!(
            "touch '{}'\nwhile [ ! -e '{}' ]; do sleep 0.05; done\n{INSTALL_FIXTURE_DEP}",
            started.display(),
            release.display()
        ),
    );
    let config = runtime_config(
        &home,
        defaults_file(json!({}), json!({"ocr-fixture": "shipped/ocr-fixture"})),
        gate.search_path(),
    );
    let (server, _, _) = start(config.clone()).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while !started.exists() {
        assert!(tokio::time::Instant::now() < deadline, "npm never started");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    // While npm waits, the old copy is in use, the row runs, the update is
    // offered, and clients are not blocked (§3.1 step 4).
    assert_eq!(
        recorded_dir(&home, "ocr-fixture"),
        old,
        "the old copy is in use"
    );
    let list = call(&mut client, "plugin.list", json!({})).await;
    let listed = item(&list, "ocr-fixture");
    // The fixture Node supplies the runtime service but cannot start the
    // session bridge, so a code row waits on `session-node` rather than
    // running; it is up and not failed.
    assert_eq!(listed["status"], "pending", "{list}");
    assert_eq!(listed["waitingFor"], json!(["session-node"]), "{list}");
    assert_eq!(listed["update"]["version"], "1.1.0", "{list}");
    assert!(listed["update"].get("error").is_none(), "{list}");
    let runtimes = client.rpc("runtime.list", json!({})).await;
    assert!(runtimes.get("error").is_none(), "{runtimes}");

    std::fs::write(&release, "").unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let new = loop {
        let dir = recorded_dir(&home, "ocr-fixture");
        if name_of(&dir).starts_with("1.1.0+") {
            break dir;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the record never switched: {dir:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert!(
        new.join("node_modules/fixture-dep/index.js").is_file(),
        "{new:?}"
    );
    assert!(old.exists(), "the replaced copy stays until the next start");
    let list = call(&mut client, "plugin.list", json!({})).await;
    assert!(item(&list, "ocr-fixture").get("update").is_none(), "{list}");
    drop(client);
    server.shutdown();
    server.join().await.unwrap();

    // The next start's cleanup deletes the copy no entry names.
    let config = runtime_config(
        &home,
        defaults_file(json!({}), json!({"ocr-fixture": "shipped/ocr-fixture"})),
        gate.search_path(),
    );
    let (server, _, _) = start(config).await;
    let client = Client::new_version(&server, false, 3).await;
    assert_eq!(recorded_dir(&home, "ocr-fixture"), new);
    assert!(!old.exists(), "the next start deletes the replaced copy");
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// §3.1 step 4, §3.5: a failed re-acquisition keeps the old copy and records
/// `update.error`, which the client reads as the A.2 description of the
/// failure; fixing the fixture and calling `plugin.update` repairs it and
/// clears the error.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_failed_re_acquisition_keeps_the_old_copy_until_an_update_repairs_it() {
    let home = home();
    let shipped = optional_plugin(&home.join("shipped"), "ocr-fixture", "1.0.0", true);
    let first = FixtureNode::with("24.21.0", INSTALL_FIXTURE_DEP);
    let config = runtime_config(
        &home,
        defaults_file(json!({}), json!({"ocr-fixture": "shipped/ocr-fixture"})),
        first.search_path(),
    );
    let (server, _, _) = start(config).await;
    let mut client = Client::new_version(&server, false, 3).await;
    assert_ok(
        &call(
            &mut client,
            "plugin.install",
            json!({"source": {"optional": "ocr-fixture"}}),
        )
        .await,
    );
    let old = recorded_dir(&home, "ocr-fixture");
    drop(client);
    server.shutdown();
    server.join().await.unwrap();

    set_shipped_version(&shipped, "1.1.0");
    let broken = FixtureNode::with("24.21.0", "exit 1");
    let config = runtime_config(
        &home,
        defaults_file(json!({}), json!({"ocr-fixture": "shipped/ocr-fixture"})),
        broken.search_path(),
    );
    let (server, _, _) = start(config).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let listed = loop {
        let list = call(&mut client, "plugin.list", json!({})).await;
        let listed = item(&list, "ocr-fixture").clone();
        if listed["update"].get("error").is_some() {
            break listed;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "no update.error: {list}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    // A.2: the client reads the static description, not npm's message.
    assert_eq!(
        listed["update"]["error"],
        "The plugin's locked dependencies could not be installed; check network access",
        "{listed}"
    );
    assert_eq!(listed["status"], "pending", "{listed}");
    assert_eq!(
        recorded_dir(&home, "ocr-fixture"),
        old,
        "the old copy keeps serving"
    );
    assert_eq!(
        copies(&home, "ocr-fixture"),
        [name_of(&old)],
        "a failed re-acquisition leaves no new copy"
    );

    // Fixing the fixture and updating repairs the plugin and clears the
    // error (§3.5, §3.6).
    broken.npm(INSTALL_FIXTURE_DEP);
    let updated = call(&mut client, "plugin.update", json!({"id": "ocr-fixture"})).await;
    assert_ok(&updated);
    assert_eq!(updated["result"]["version"], "1.1.0", "{updated}");
    let new = recorded_dir(&home, "ocr-fixture");
    assert!(name_of(&new).starts_with("1.1.0+"), "{new:?}");
    assert!(new.join("node_modules/fixture-dep/index.js").is_file());
    let list = call(&mut client, "plugin.list", json!({})).await;
    assert!(item(&list, "ocr-fixture").get("update").is_none(), "{list}");
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// §3.1 step 4, §3.6: re-acquisition acts on an installed optional plugin
/// whether or not its name is in use, so a failure on a name that is off is
/// still reported as `update.error` on that name's item.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_failed_re_acquisition_while_the_plugin_is_off_shows_its_error() {
    let home = home();
    let shipped = optional_plugin(&home.join("shipped"), "ocr-fixture", "1.0.0", true);
    let first = FixtureNode::with("24.21.0", INSTALL_FIXTURE_DEP);
    let config = runtime_config(
        &home,
        defaults_file(json!({}), json!({"ocr-fixture": "shipped/ocr-fixture"})),
        first.search_path(),
    );
    let (server, _, _) = start(config).await;
    let mut client = Client::new_version(&server, false, 3).await;
    assert_ok(
        &call(
            &mut client,
            "plugin.install",
            json!({"source": {"optional": "ocr-fixture"}}),
        )
        .await,
    );
    assert_ok(
        &call(
            &mut client,
            "plugin.disable",
            json!({"name": "ocr-fixture"}),
        )
        .await,
    );
    let old = recorded_dir(&home, "ocr-fixture");
    drop(client);
    server.shutdown();
    server.join().await.unwrap();

    set_shipped_version(&shipped, "1.1.0");
    let broken = FixtureNode::with("24.21.0", "exit 1");
    let config = runtime_config(
        &home,
        defaults_file(json!({}), json!({"ocr-fixture": "shipped/ocr-fixture"})),
        broken.search_path(),
    );
    let (server, _, _) = start(config).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let listed = loop {
        let list = call(&mut client, "plugin.list", json!({})).await;
        let listed = item(&list, "ocr-fixture").clone();
        if listed["update"].get("error").is_some() {
            break listed;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "no update.error: {list}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert_eq!(listed["off"], true, "{listed}");
    assert_eq!(listed["use"], Value::Null, "{listed}");
    assert_eq!(listed["update"]["version"], "1.1.0", "{listed}");
    // A.2: the client reads the static description, not npm's message.
    assert_eq!(
        listed["update"]["error"],
        "The plugin's locked dependencies could not be installed; check network access",
        "{listed}"
    );
    assert_eq!(
        recorded_dir(&home, "ocr-fixture"),
        old,
        "the old copy keeps serving"
    );
    assert_eq!(
        copies(&home, "ocr-fixture"),
        [name_of(&old)],
        "a failed re-acquisition leaves no new copy"
    );
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// §3.1 step 4, §3.6: an unchanged shipped version starts no re-acquisition
/// and offers no `update`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unchanged_shipped_version_is_not_re_acquired() {
    let home = home();
    optional_plugin(&home.join("shipped"), "ocr-fixture", "1.0.0", false);
    let first = FixtureNode::with("24.21.0", INSTALL_FIXTURE_DEP);
    let config = runtime_config(
        &home,
        defaults_file(json!({}), json!({"ocr-fixture": "shipped/ocr-fixture"})),
        first.search_path(),
    );
    let (server, _, _) = start(config).await;
    let mut client = Client::new_version(&server, false, 3).await;
    assert_ok(
        &call(
            &mut client,
            "plugin.install",
            json!({"source": {"optional": "ocr-fixture"}}),
        )
        .await,
    );
    let dir = recorded_dir(&home, "ocr-fixture");
    drop(client);
    server.shutdown();
    server.join().await.unwrap();

    let config = runtime_config(
        &home,
        defaults_file(json!({}), json!({"ocr-fixture": "shipped/ocr-fixture"})),
        first.search_path(),
    );
    let (server, _, _) = start(config).await;
    let mut client = Client::new_version(&server, false, 3).await;
    // A re-acquisition of an unchanged version would draw a second copy and
    // switch the record; give the background task a window to (not) do it.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while tokio::time::Instant::now() < deadline {
        assert_eq!(
            copies(&home, "ocr-fixture"),
            [name_of(&dir)],
            "a copy exists for an unchanged version"
        );
        assert_eq!(
            recorded_dir(&home, "ocr-fixture"),
            dir,
            "the record changed for an unchanged version"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let list = call(&mut client, "plugin.list", json!({})).await;
    assert!(item(&list, "ocr-fixture").get("update").is_none(), "{list}");
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// §2.3, §3.1 step 4: a shipped directory that cannot be read is left alone
/// at startup: nothing is re-acquired, no `update` is offered, and the start
/// is normal.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unreadable_shipped_directory_is_not_re_acquired() {
    let home = home();
    let shipped = optional_plugin(&home.join("shipped"), "ocr-fixture", "1.0.0", false);
    let first = FixtureNode::with("24.21.0", INSTALL_FIXTURE_DEP);
    let config = runtime_config(
        &home,
        defaults_file(json!({}), json!({"ocr-fixture": "shipped/ocr-fixture"})),
        first.search_path(),
    );
    let (server, _, _) = start(config).await;
    let mut client = Client::new_version(&server, false, 3).await;
    assert_ok(
        &call(
            &mut client,
            "plugin.install",
            json!({"source": {"optional": "ocr-fixture"}}),
        )
        .await,
    );
    let dir = recorded_dir(&home, "ocr-fixture");
    drop(client);
    server.shutdown();
    server.join().await.unwrap();

    std::fs::remove_dir_all(&shipped).unwrap();
    let config = runtime_config(
        &home,
        defaults_file(json!({}), json!({"ocr-fixture": "shipped/ocr-fixture"})),
        first.search_path(),
    );
    let (server, _, _) = start(config).await;
    let mut client = Client::new_version(&server, false, 3).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let list = call(&mut client, "plugin.list", json!({})).await;
    let listed = item(&list, "ocr-fixture");
    assert_eq!(listed["optional"], true, "{list}");
    assert!(listed.get("update").is_none(), "{list}");
    assert_eq!(copies(&home, "ocr-fixture"), [name_of(&dir)]);
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// §3.5: a cancelled attempt records no `update.error`. Reloading
/// PluginManager's row cancels the background re-acquisition in flight,
/// which kills npm; `plugin.list` still offers the update without a
/// failure.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_cancelled_re_acquisition_records_no_error() {
    let home = home();
    let shipped = optional_plugin(&home.join("shipped"), "ocr-fixture", "1.0.0", true);
    let first = FixtureNode::with("24.21.0", INSTALL_FIXTURE_DEP);
    let config = runtime_config(
        &home,
        defaults_file(json!({}), json!({"ocr-fixture": "shipped/ocr-fixture"})),
        first.search_path(),
    );
    let (server, _, _) = start(config).await;
    let mut client = Client::new_version(&server, false, 3).await;
    assert_ok(
        &call(
            &mut client,
            "plugin.install",
            json!({"source": {"optional": "ocr-fixture"}}),
        )
        .await,
    );
    let old = recorded_dir(&home, "ocr-fixture");
    drop(client);
    server.shutdown();
    server.join().await.unwrap();

    set_shipped_version(&shipped, "1.1.0");
    let started = home.join("npm-started");
    let release = home.join("npm-release");
    let gate = FixtureNode::with(
        "24.21.0",
        &format!(
            "touch '{}'\nwhile [ ! -e '{}' ]; do sleep 0.05; done\n{INSTALL_FIXTURE_DEP}",
            started.display(),
            release.display()
        ),
    );
    let config = runtime_config(
        &home,
        defaults_file(json!({}), json!({"ocr-fixture": "shipped/ocr-fixture"})),
        gate.search_path(),
    );
    let (server, _, _) = start(config).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while !started.exists() {
        assert!(tokio::time::Instant::now() < deadline, "npm never started");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // Reloading the row ends the generation the task belongs to: the token
    // fires, npm is killed, and the staging directory is deleted.
    let loader = server.service::<rutis_loader::Loader>().unwrap();
    loader.restart("PluginManager").await.unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        let staged = std::fs::read_dir(home.join("plugins/.staging"))
            .map(|entries| entries.count())
            .unwrap_or(0);
        if staged == 0 {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the cancelled re-acquisition never finished"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    // Give the cancelled call's Err a moment to reach the error map.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let list = call(&mut client, "plugin.list", json!({})).await;
    let listed = item(&list, "ocr-fixture");
    assert_eq!(listed["update"]["version"], "1.1.0", "{list}");
    assert!(
        listed["update"].get("error").is_none(),
        "a cancelled attempt recorded a failure: {list}"
    );
    assert_eq!(
        recorded_dir(&home, "ocr-fixture"),
        old,
        "the old copy is still in use"
    );
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

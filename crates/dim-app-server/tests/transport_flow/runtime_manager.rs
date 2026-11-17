//! RuntimeManager (RuntimeManager §3): the runtimes arrive after startup and
//! the plugin rows waiting for them start by themselves. `system` sources
//! keep the interpreters off the network; the pinned packages are installed
//! for real (`npm ci`, `pip install --require-hashes`).
use super::*;
use dim_app_server::{
    runtime_manager::{Environment, Kind, RuntimeService, Runtimes},
    session_typescript::SessionTypeScriptConfig,
};
use std::path::{Path, PathBuf};

/// The `runtime.list` entry of `runtime` once its status is `status`.
async fn status(client: &mut Client, runtime: usize, status: &str) -> Value {
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            let list = client.rpc("runtime.list", json!({})).await;
            assert_ok(&list);
            if list["result"][runtime]["status"] == status {
                return list["result"][runtime].clone();
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("runtime {runtime} did not become {status}"))
}

async fn appears(path: &Path) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(300);
    while !path.exists() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "{} did not appear",
            path.display()
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// The unpacked archive directory holding `interpreter` (RuntimeManager
/// §2.3): `bin/<name>` inside it, or the executable beside it on Windows.
fn archive_directory(interpreter: &Path) -> PathBuf {
    let directory = interpreter.parent().expect("the interpreter's directory");
    if cfg!(windows) {
        directory.to_owned()
    } else {
        directory
            .parent()
            .expect("the archive directory")
            .to_owned()
    }
}

/// The first `program` on this process's `PATH`: the interpreter a `system`
/// source resolves to (§2.2).
fn on_path(program: &str) -> PathBuf {
    std::env::split_paths(&std::env::var_os("PATH").expect("a PATH"))
        .map(|directory| directory.join(program))
        .find(|path| path.is_file())
        .expect("the program is on PATH")
}

/// `runtime.list` reports an interpreter only while the record marks the
/// runtime installed under the plan in effect (§2.4, §5), the same test that
/// skips a download. Nothing is acquired here: the record and the directories
/// it names are written by the test.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn runtime_list_reports_a_path_only_while_its_record_matches_the_plan() {
    let home = PathBuf::from("/tmp").join(format!("drm-path-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(home.join("runtimes")).unwrap();
    let defaults = home.join("default-plugins.json");
    std::fs::write(
        &defaults,
        r#"{"runtimes": {"node": {"source": "download", "auto": false},
                         "python": {"source": "download", "auto": false}}}"#,
    )
    .unwrap();
    let runtimes = Arc::new(
        Runtimes::load(
            &home,
            &dim_app_server::plugin_manager::Defaults::read(&defaults)
                .unwrap()
                .runtimes,
            None,
        )
        .unwrap(),
    );
    let node = runtimes.plan(Kind::Node).clone();
    std::fs::create_dir_all(node.packages.as_ref().expect("Node packages")).unwrap();
    std::fs::create_dir_all(archive_directory(&node.interpreter)).unwrap();
    let packages = node
        .packages
        .as_ref()
        .expect("Node packages")
        .file_name()
        .unwrap()
        .to_str()
        .unwrap();
    std::fs::write(
        home.join("runtimes/runtimes.json"),
        json!({"node": {"source": "download", "version": &node.version, "packages": packages}})
            .to_string(),
    )
    .unwrap();
    let server = launch(
        Arc::new(MockTools),
        ServerConfig {
            target_home: Some(home.clone()),
            session_typescript: Some(SessionTypeScriptConfig::acquired(&home, runtimes, vec![])),
            ..Default::default()
        },
    )
    .await;
    let mut client = Client::new_version(&server, false, 3).await;

    let listed = client.rpc("runtime.list", json!({})).await;
    assert_ok(&listed);
    assert_eq!(
        listed["result"][0]["path"],
        node.interpreter.to_str().unwrap(),
        "the installed runtime reports the interpreter it runs with: {listed}"
    );
    assert!(
        listed["result"][1]["path"].is_null(),
        "a runtime nothing resolved reports no path: {listed}"
    );

    std::fs::write(
        home.join("runtimes/runtimes.json"),
        json!({"node": {"source": "system", "version": node.version, "packages": packages}})
            .to_string(),
    )
    .unwrap();
    let listed = client.rpc("runtime.list", json!({})).await;
    assert_ok(&listed);
    assert_eq!(listed["result"][0]["version"], node.version, "{listed}");
    assert!(
        listed["result"][0]["path"].is_null(),
        "a record that is not this plan's reports no path: {listed}"
    );

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// A relative search-path entry (§5): `which` resolves it against the daemon's
/// working directory, so `runtime.list` reports the absolute interpreter path
/// that a process started from another working directory can launch.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn runtime_list_reports_an_absolute_path_for_a_relative_path_entry() {
    let home = std::env::temp_dir().join(format!("drm-relative-{}", uuid::Uuid::new_v4()));
    let fixture = home.join("system-node");
    let bin = fixture.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let node = bin.join("node");
    std::fs::write(
        &node,
        "#!/bin/sh\n[ \"$1\" = --version ] && echo v22.19.0\nexit 0\n",
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&node, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::create_dir_all(fixture.join("lib/node_modules/npm/bin")).unwrap();
    std::fs::write(fixture.join("lib/node_modules/npm/bin/npm-cli.js"), "").unwrap();

    // The interpreter's directory as a relative entry, built from the current
    // directory so no flow has to change it: one `..` per component of the
    // current directory reaches the root, then the directory's path follows.
    let cwd = std::env::current_dir().unwrap();
    let absolute = bin.canonicalize().unwrap();
    let ups = cwd
        .components()
        .filter(|component| matches!(component, std::path::Component::Normal(_)))
        .count();
    let relative = PathBuf::from("../".repeat(ups)).join(absolute.strip_prefix("/").unwrap());
    assert!(
        !relative.is_absolute(),
        "the entry is relative: {relative:?}"
    );

    let defaults = home.join("default-plugins.json");
    std::fs::write(
        &defaults,
        r#"{"runtimes": {"node": {"source": "system", "auto": false},
                         "python": {"source": "download", "auto": false}}}"#,
    )
    .unwrap();
    let runtimes = Arc::new(
        Runtimes::load(
            &home,
            &dim_app_server::plugin_manager::Defaults::read(&defaults)
                .unwrap()
                .runtimes,
            Some(environment(relative.into_os_string())),
        )
        .unwrap(),
    );
    std::fs::create_dir_all(
        runtimes
            .plan(Kind::Node)
            .packages
            .as_ref()
            .expect("Node packages"),
    )
    .unwrap();
    let server = launch(
        Arc::new(MockTools),
        ServerConfig {
            target_home: Some(home.clone()),
            session_typescript: Some(SessionTypeScriptConfig::acquired(&home, runtimes, vec![])),
            ..Default::default()
        },
    )
    .await;
    let mut client = Client::new_version(&server, false, 3).await;
    let install = client.rpc("runtime.install", json!({"name": "node"})).await;
    assert_ok(&install);

    let entry = tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            let list = client.rpc("runtime.list", json!({})).await;
            assert_ok(&list);
            if list["result"][0]["path"].is_string() {
                return list["result"][0].clone();
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("runtime.list reported no path");

    let path = entry["path"].as_str().unwrap();
    assert!(
        Path::new(path).is_absolute(),
        "the reported interpreter path is absolute: {entry}"
    );
    assert_eq!(
        Path::new(path).canonicalize().unwrap(),
        node.canonicalize().unwrap(),
        "the reported path names the fixture: {entry}"
    );

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// Fixture `uv` and `uvx` pairs in directories a search path points at,
/// removed on drop; the process environment is never changed.
struct FixtureUv {
    /// The first directory: the uv the plan starts with.
    directory: PathBuf,
    /// Directories after it on the search path. A reload resolves the first
    /// of these once the first directory has no uv.
    fallbacks: Vec<PathBuf>,
}

impl FixtureUv {
    fn new(version: &str) -> Self {
        Self::with_fallback(version, None)
    }

    /// `A:B`: `A` holds the `version` uv and `B` one that reports
    /// `fallback`, so removing `A`'s uv leaves the reload resolving `B`
    /// before any uv installed on the host.
    fn with_fallback(version: &str, fallback: Option<&str>) -> Self {
        let directory = Self::fixture(version);
        let fallbacks: Vec<PathBuf> = fallback.into_iter().map(Self::fixture).collect();
        Self {
            directory,
            fallbacks,
        }
    }

    /// The search path the runtimes resolve uv on: the fixture directories in
    /// order, so a reload resolves the one written first and then the
    /// fallback. The process environment is not involved.
    fn search_path(&self) -> std::ffi::OsString {
        std::env::join_paths(std::iter::once(&self.directory).chain(&self.fallbacks)).unwrap()
    }

    /// One fixture directory: `uv` reporting `version`, and `uvx`.
    fn fixture(version: &str) -> PathBuf {
        let directory = std::env::temp_dir().join(format!("dm-uv-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&directory).unwrap();
        write_script(&directory, "uv", &format!("echo \"uv {version}\""));
        write_script(&directory, "uvx", "exit 0");
        directory
    }

    fn restore_uv(&self, version: &str) {
        write_script(&self.directory, "uv", &format!("echo \"uv {version}\""));
    }
}

fn write_script(directory: &Path, name: &str, body: &str) {
    let file = directory.join(name);
    std::fs::write(&file, format!("#!/bin/sh\n{body}\n")).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).unwrap();
}

impl Drop for FixtureUv {
    fn drop(&mut self) {
        for directory in std::iter::once(&self.directory).chain(&self.fallbacks) {
            let _ = std::fs::remove_dir_all(directory);
        }
    }
}

fn write_defaults(home: &Path, runtimes: &str) -> PathBuf {
    let path = home.join("default-plugins.json");
    std::fs::write(&path, format!(r#"{{"runtimes": {runtimes}}}"#)).unwrap();
    path
}

/// An environment holding only `path`, with every override unset: what a
/// fixture's search path is.
fn environment(path: std::ffi::OsString) -> Environment {
    Environment {
        path,
        ..Default::default()
    }
}

fn load_runtimes(home: &Path, defaults: &Path, environment: Option<Environment>) -> Arc<Runtimes> {
    Arc::new(
        Runtimes::load(
            home,
            &dim_app_server::plugin_manager::Defaults::read(defaults)
                .unwrap()
                .runtimes,
            environment,
        )
        .unwrap(),
    )
}

/// `runtime.list` shows the uv tool, off until it is installed, after the two
/// runtimes.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn runtime_list_shows_the_uv_tool_off() {
    let home = std::env::temp_dir().join(format!("drm-uv-off-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(home.join("runtimes")).unwrap();
    let defaults = write_defaults(
        &home,
        r#"{"node": {"source": "download", "auto": false},
            "python": {"source": "download", "auto": false},
            "uv": {"source": "download", "auto": false}}"#,
    );
    let runtimes = load_runtimes(&home, &defaults, None);
    let server = launch(
        Arc::new(MockTools),
        ServerConfig {
            target_home: Some(home.clone()),
            session_typescript: Some(SessionTypeScriptConfig::acquired(&home, runtimes, vec![])),
            ..Default::default()
        },
    )
    .await;
    let mut client = Client::new_version(&server, false, 3).await;
    let listed = client.rpc("runtime.list", json!({})).await;
    assert_ok(&listed);
    assert_eq!(listed["result"].as_array().unwrap().len(), 3, "{listed}");
    let uv = &listed["result"][2];
    assert_eq!(uv["name"], "uv", "{listed}");
    assert_eq!(uv["source"], "download", "{listed}");
    assert_eq!(uv["version"], "0.13.7", "{listed}");
    assert_eq!(uv["status"], "off", "{listed}");
    assert!(uv["path"].is_null(), "{listed}");
    assert_eq!(listed["result"][0]["status"], "off", "{listed}");
    assert_eq!(listed["result"][1]["status"], "off", "{listed}");

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// A `system` source resolves uv on `PATH`, accepts 0.11.0 or later with
/// `uvx` beside it, and reads ready once installed, its path the fixture; the
/// node and python runtimes are untouched.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_system_uv_fixture_reads_ready_with_its_own_path() {
    let fixture = FixtureUv::new("0.13.7");
    let home = std::env::temp_dir().join(format!("drm-uv-system-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(home.join("runtimes")).unwrap();
    let defaults = write_defaults(
        &home,
        r#"{"node": {"source": "download", "auto": false},
            "python": {"source": "download", "auto": false},
            "uv": {"source": "system", "auto": false}}"#,
    );
    let runtimes = load_runtimes(&home, &defaults, Some(environment(fixture.search_path())));
    assert_eq!(
        runtimes.plan(Kind::Uv).interpreter,
        fixture.directory.join("uv"),
        "a system uv is the fixture the PATH names"
    );
    let server = launch(
        Arc::new(MockTools),
        ServerConfig {
            target_home: Some(home.clone()),
            session_typescript: Some(SessionTypeScriptConfig::acquired(&home, runtimes, vec![])),
            ..Default::default()
        },
    )
    .await;
    let mut client = Client::new_version(&server, false, 3).await;
    let before = client.rpc("runtime.list", json!({})).await;
    assert_ok(&before);
    assert_eq!(before["result"][2]["status"], "off", "{before}");

    let install = client.rpc("runtime.install", json!({"name": "uv"})).await;
    assert_ok(&install);
    let ready = status(&mut client, 2, "ready").await;
    assert_eq!(ready["source"], "system", "{ready}");
    assert_eq!(ready["version"], "0.13.7", "{ready}");
    assert_eq!(
        ready["path"],
        fixture.directory.join("uv").to_str().unwrap(),
        "the checked fixture is the path: {ready}"
    );
    let records: Value =
        serde_json::from_slice(&std::fs::read(home.join("runtimes/runtimes.json")).unwrap())
            .unwrap();
    assert_eq!(records["uv"]["source"], "system", "{records}");
    assert_eq!(records["uv"]["version"], "0.13.7", "{records}");
    assert!(
        records["uv"].get("packages").is_none(),
        "a tool has no packages: {records}"
    );
    let after = client.rpc("runtime.list", json!({})).await;
    assert_ok(&after);
    assert_eq!(
        after["result"][0], before["result"][0],
        "installing uv leaves node alone: {after}"
    );
    assert_eq!(
        after["result"][1], before["result"][1],
        "installing uv leaves python alone: {after}"
    );

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// A removed `system` uv reads off, and `runtime.install` acquires again:
/// `runtime.list` and `install` ask the one installed predicate, which for a
/// tool includes its executable, so a tool cannot stay off while install says
/// it is installed. The reload then resolves the second fixture directory,
/// whose uv is below the floor, so the acquisition fails with its version
/// rather than with a missing file.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_removed_system_uv_reads_off_and_installs_again() {
    let fixture = FixtureUv::with_fallback("0.13.7", Some("0.10.9"));
    let home = std::env::temp_dir().join(format!("drm-uv-removed-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(home.join("runtimes")).unwrap();
    let defaults = write_defaults(
        &home,
        r#"{"node": {"source": "download", "auto": false},
            "python": {"source": "download", "auto": false},
            "uv": {"source": "system", "auto": false}}"#,
    );
    let runtimes = load_runtimes(&home, &defaults, Some(environment(fixture.search_path())));
    let server = launch(
        Arc::new(MockTools),
        ServerConfig {
            target_home: Some(home.clone()),
            session_typescript: Some(SessionTypeScriptConfig::acquired(&home, runtimes, vec![])),
            ..Default::default()
        },
    )
    .await;
    let mut client = Client::new_version(&server, false, 3).await;
    assert_ok(&client.rpc("runtime.install", json!({"name": "uv"})).await);
    status(&mut client, 2, "ready").await;

    std::fs::remove_file(fixture.directory.join("uv")).unwrap();
    let off = client.rpc("runtime.list", json!({})).await;
    assert_ok(&off);
    assert_eq!(off["result"][2]["status"], "off", "{off}");
    assert!(
        off["result"][2]["path"].is_null(),
        "an installed tool whose executable is gone reports no path: {off}"
    );

    // Not installed under the shared predicate, so the call acquires again;
    // the reload resolves the fallback fixture, which is below the floor.
    assert_ok(&client.rpc("runtime.install", json!({"name": "uv"})).await);
    let failed = status(&mut client, 2, "failed").await;
    let reason = failed["reason"].as_str().unwrap_or_default();
    assert!(reason.contains("0.10.9"), "{failed}");
    assert!(reason.contains("0.11"), "{failed}");

    // Restoring it lets the same call reach ready again.
    fixture.restore_uv("0.13.7");
    assert_ok(&client.rpc("runtime.install", json!({"name": "uv"})).await);
    let ready = status(&mut client, 2, "ready").await;
    assert_eq!(ready["version"], "0.13.7", "{ready}");
    assert_eq!(
        ready["path"],
        fixture.directory.join("uv").to_str().unwrap(),
        "{ready}"
    );

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// A `system` uv below 0.11.0 is that tool's failure, with the version in the
/// reason.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_system_uv_below_the_floor_fails_with_its_version() {
    let _fixture = FixtureUv::new("0.10.9");
    let home = std::env::temp_dir().join(format!("drm-uv-old-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(home.join("runtimes")).unwrap();
    let defaults = write_defaults(
        &home,
        r#"{"node": {"source": "download", "auto": false},
            "python": {"source": "download", "auto": false},
            "uv": {"source": "system", "auto": false}}"#,
    );
    let runtimes = load_runtimes(&home, &defaults, Some(environment(_fixture.search_path())));
    let server = launch(
        Arc::new(MockTools),
        ServerConfig {
            target_home: Some(home.clone()),
            session_typescript: Some(SessionTypeScriptConfig::acquired(&home, runtimes, vec![])),
            ..Default::default()
        },
    )
    .await;
    let mut client = Client::new_version(&server, false, 3).await;
    assert_ok(&client.rpc("runtime.install", json!({"name": "uv"})).await);
    let failed = status(&mut client, 2, "failed").await;
    let reason = failed["reason"].as_str().unwrap_or_default();
    assert!(reason.contains("0.10.9"), "{failed}");
    assert!(reason.contains("0.11"), "{failed}");

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// `runtime.configure uv` writes the merge over the preset and applies it,
/// without a launch command to withdraw or provide.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn configuring_uv_writes_the_merged_file_and_applies_it() {
    let fixture = FixtureUv::new("0.13.7");
    let home = std::env::temp_dir().join(format!("drm-uv-configure-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(home.join("runtimes")).unwrap();
    let defaults = write_defaults(
        &home,
        r#"{"node": {"source": "download", "auto": false},
            "python": {"source": "download", "auto": false},
            "uv": {"source": "download", "auto": false}}"#,
    );
    let runtimes = load_runtimes(&home, &defaults, Some(environment(fixture.search_path())));
    let server = launch(
        Arc::new(MockTools),
        ServerConfig {
            target_home: Some(home.clone()),
            session_typescript: Some(SessionTypeScriptConfig::acquired(&home, runtimes, vec![])),
            ..Default::default()
        },
    )
    .await;
    let mut client = Client::new_version(&server, false, 3).await;
    let configured = client
        .rpc(
            "runtime.configure",
            json!({"name": "uv", "source": "system"}),
        )
        .await;
    assert_ok(&configured);
    assert_eq!(configured["result"]["source"], "system", "{configured}");
    let ready = status(&mut client, 2, "ready").await;
    assert_eq!(
        ready["path"],
        fixture.directory.join("uv").to_str().unwrap(),
        "{ready}"
    );
    let file: Value =
        serde_json::from_slice(&std::fs::read(home.join("runtimes/config.json")).unwrap()).unwrap();
    assert_eq!(file["uv"]["source"], "system", "{file}");
    assert!(
        file["uv"]["auto"].is_null(),
        "the merge keeps the fields it was not given unset: {file}"
    );

    let configured = client
        .rpc("runtime.configure", json!({"name": "uv", "auto": true}))
        .await;
    assert_ok(&configured);
    assert_eq!(configured["result"]["auto"], true, "{configured}");
    assert_eq!(configured["result"]["status"], "ready", "{configured}");
    let file: Value =
        serde_json::from_slice(&std::fs::read(home.join("runtimes/config.json")).unwrap()).unwrap();
    assert_eq!(file["uv"]["source"], "system", "{file}");
    assert_eq!(file["uv"]["auto"], true, "{file}");

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// The pinned uv downloads, verifies, unpacks and runs; it has no packages
/// directory and no connection row. It downloads the archive, so it runs only
/// with the other ignored flows.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "downloads the uv archive"]
async fn uv_runtime_manager_downloads_the_pinned_tool() {
    let home = std::env::temp_dir().join(format!("drm-uv-download-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&home).unwrap();
    let defaults = write_defaults(
        &home,
        r#"{"node": {"source": "system", "auto": false},
            "python": {"source": "system", "auto": false},
            "uv": {"source": "download", "auto": false}}"#,
    );
    let runtimes = load_runtimes(&home, &defaults, None);
    let uv = runtimes.plan(Kind::Uv).clone();
    assert!(
        uv.interpreter.to_string_lossy().contains("uv-0.13.7-"),
        "the pinned version names the archive directory: {}",
        uv.interpreter.display()
    );
    let server = launch(
        Arc::new(MockTools),
        ServerConfig {
            target_home: Some(home.clone()),
            session_typescript: Some(SessionTypeScriptConfig::acquired(&home, runtimes, vec![])),
            ..Default::default()
        },
    )
    .await;
    let mut client = Client::new_version(&server, false, 3).await;
    assert_ok(&client.rpc("runtime.install", json!({"name": "uv"})).await);
    let ready = status(&mut client, 2, "ready").await;
    assert_eq!(ready["source"], "download", "{ready}");
    assert_eq!(ready["version"], "0.13.7", "{ready}");
    assert_eq!(ready["path"], uv.interpreter.to_str().unwrap(), "{ready}");
    let output = tokio::process::Command::new(&uv.interpreter)
        .arg("--version")
        .output()
        .await
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let version = String::from_utf8_lossy(&output.stdout);
    assert!(version.contains("uv 0.13.7"), "{version}");
    let uvx = uv
        .interpreter
        .with_file_name(if cfg!(windows) { "uvx.exe" } else { "uvx" });
    assert!(uvx.is_file(), "uvx sits beside uv: {}", uvx.display());
    let records: Value =
        serde_json::from_slice(&std::fs::read(home.join("runtimes/runtimes.json")).unwrap())
            .unwrap();
    assert!(
        records["uv"].get("packages").is_none(),
        "a tool has no packages: {records}"
    );
    assert!(
        records["uv"]["sha256"]
            .as_str()
            .is_some_and(|hash| hash.len() == 64),
        "the record carries the verified digest: {records}"
    );

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires node 22.19+ and the npm registry"]
async fn node_runtime_manager_restarts_a_failed_runtime() {
    let home = PathBuf::from("/tmp").join(format!("drm-{}", uuid::Uuid::new_v4()));
    let plugins = home.join("sources");
    std::fs::create_dir_all(&plugins).unwrap();
    let defaults = home.join("default-plugins.json");
    std::fs::write(
        &defaults,
        r#"{"runtimes": {"node": {"source": "system", "auto": true},
                         "python": {"source": "system", "auto": false}}}"#,
    )
    .unwrap();
    let node_marker = home.join("node-started");
    std::fs::write(
        plugins.join("probe.mjs"),
        format!(
            "import {{ writeFileSync }} from 'node:fs'\n\
             export default {{ name: 'probe', apply() {{ writeFileSync({:?}, String(process.pid)) }} }}\n",
            node_marker
        ),
    )
    .unwrap();
    let rows = vec![json!({"id": "probe", "name": plugins.join("probe.mjs"), "provides": {}})];
    let runtimes = load_runtimes(&home, &defaults, None);
    let server = launch(
        Arc::new(MockTools),
        ServerConfig {
            target_home: Some(home.clone()),
            session_typescript: Some(SessionTypeScriptConfig::acquired(&home, runtimes, rows)),
            ..Default::default()
        },
    )
    .await;
    let mut client = Client::new_version(&server, false, 3).await;
    appears(&node_marker).await;
    let list = client.rpc("runtime.list", json!({})).await;
    assert_ok(&list);
    assert_eq!(list["result"][0]["name"], "node");
    let ready = status(&mut client, 0, "ready").await;
    assert_eq!(
        ready["path"],
        on_path("node").to_str().unwrap(),
        "the entry names the checked interpreter: {ready}"
    );

    let pid = std::fs::read_to_string(&node_marker).unwrap();
    std::fs::remove_file(&node_marker).unwrap();
    assert!(
        std::process::Command::new("kill")
            .args(["-KILL", &pid])
            .status()
            .unwrap()
            .success()
    );
    let down = status(&mut client, 0, "down").await;
    assert!(
        down["reason"]
            .as_str()
            .is_some_and(|reason| !reason.is_empty()),
        "{down}"
    );
    assert!(!node_marker.exists());
    assert_ok(&client.rpc("runtime.install", json!({"name": "node"})).await);
    appears(&node_marker).await;
    assert_ne!(std::fs::read_to_string(&node_marker).unwrap(), pid);
    status(&mut client, 0, "ready").await;

    let records: Value =
        serde_json::from_slice(&std::fs::read(home.join("runtimes/runtimes.json")).unwrap())
            .unwrap();
    assert_eq!(records["node"]["source"], "system");
    let checked = std::process::Command::new("node")
        .arg("--version")
        .output()
        .unwrap();
    let checked = String::from_utf8(checked.stdout).unwrap();
    assert_eq!(
        records["node"]["version"],
        checked.trim().trim_start_matches('v')
    );
    let list = client.rpc("runtime.list", json!({})).await;
    assert_eq!(list["result"][0]["version"], records["node"]["version"]);
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires python3 3.12+ with pip on PATH and the PyPI registry"]
async fn python_plugin_starts_after_runtime_install_and_daemon_restart() {
    let home = PathBuf::from("/tmp").join(format!("drm-{}", uuid::Uuid::new_v4()));
    let plugin = home.join("plugins/.python/probe_py");
    std::fs::create_dir_all(&plugin).unwrap();
    let defaults = write_defaults(&home, r#"{"python": {"source": "system", "auto": false}}"#);
    let python_marker = home.join("python-started");
    std::fs::write(
        plugin.join("entry.py"),
        format!(
            "import pathlib\n\ninject = []\nprovides = {{}}\n\n\n\
             async def apply(ctx, config):\n    pathlib.Path({:?}).write_text('yes')\n",
            python_marker.to_str().unwrap()
        ),
    )
    .unwrap();
    let start = || {
        let runtimes = load_runtimes(&home, &defaults, None);
        let rows = vec![json!({"id": "probe-py", "name": "py:probe_py.entry", "provides": {}})];
        launch(
            Arc::new(MockTools),
            ServerConfig {
                target_home: Some(home.clone()),
                session_typescript: Some(SessionTypeScriptConfig::acquired(&home, runtimes, rows)),
                ..Default::default()
            },
        )
    };
    let server = start().await;
    let mut client = Client::new_version(&server, false, 3).await;
    let off = status(&mut client, 1, "off").await;
    assert!(off["path"].is_null(), "{off}");
    assert!(!python_marker.exists());

    assert_ok(
        &client
            .rpc("runtime.install", json!({"name": "python"}))
            .await,
    );
    appears(&python_marker).await;
    let ready = status(&mut client, 1, "ready").await;
    assert_eq!(ready["source"], "system", "{ready}");
    let records: Value =
        serde_json::from_slice(&std::fs::read(home.join("runtimes/runtimes.json")).unwrap())
            .unwrap();
    assert_eq!(records["python"]["source"], "system");
    drop(client);
    server.shutdown();
    server.join().await.unwrap();

    std::fs::remove_file(&python_marker).unwrap();
    let server = start().await;
    appears(&python_marker).await;
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// The configuration is read when an action applies it, not at startup: a file
/// written after the App Server started reaches `runtime.install`, and one that
/// does not read fails that call and leaves the running runtime as it was.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires node 22.19+ and the npm registry"]
async fn node_runtime_manager_reads_the_configuration_when_it_applies_it() {
    let home = PathBuf::from("/tmp").join(format!("drm-config-{}", uuid::Uuid::new_v4()));
    let plugins = home.join("sources");
    std::fs::create_dir_all(&plugins).unwrap();
    let defaults = home.join("default-plugins.json");
    std::fs::write(
        &defaults,
        r#"{"runtimes": {"node": {"source": "system", "auto": true},
                         "python": {"source": "system", "auto": false}}}"#,
    )
    .unwrap();
    let marker = home.join("node-started");
    std::fs::write(
        plugins.join("probe.mjs"),
        format!(
            "import {{ writeFileSync }} from 'node:fs'\n\
             export default {{ name: 'probe', apply() {{ writeFileSync({:?}, String(process.pid)) }} }}\n",
            marker
        ),
    )
    .unwrap();
    let rows = vec![json!({"id": "probe", "name": plugins.join("probe.mjs"), "provides": {}})];
    let runtimes = Arc::new(
        Runtimes::load(
            &home,
            &dim_app_server::plugin_manager::Defaults::read(&defaults)
                .unwrap()
                .runtimes,
            None,
        )
        .unwrap(),
    );
    let server = launch(
        Arc::new(MockTools),
        ServerConfig {
            target_home: Some(home.clone()),
            session_typescript: Some(SessionTypeScriptConfig::acquired(&home, runtimes, rows)),
            ..Default::default()
        },
    )
    .await;
    let mut client = Client::new_version(&server, false, 3).await;
    appears(&marker).await;
    let ready = status(&mut client, 0, "ready").await;

    std::fs::write(
        home.join("runtimes/config.json"),
        r#"{"node": {"source": "elsewhere"}}"#,
    )
    .unwrap();
    let refused = client.rpc("runtime.install", json!({"name": "node"})).await;
    assert_eq!(
        refused["error"]["data"]["appError"], "runtime_configuration",
        "{refused}"
    );
    assert!(
        refused["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("elsewhere")),
        "{refused}"
    );
    assert_eq!(
        status(&mut client, 0, "ready").await,
        ready,
        "the runtime is as it was"
    );

    std::fs::write(
        home.join("runtimes/config.json"),
        r#"{"node": {"source": "system"}}"#,
    )
    .unwrap();
    let applied = client.rpc("runtime.install", json!({"name": "node"})).await;
    assert_ok(&applied);
    assert_eq!(
        status(&mut client, 0, "ready").await,
        ready,
        "the same process keeps running"
    );

    let configured = client
        .rpc(
            "runtime.configure",
            json!({"name": "node", "source": "system", "auto": true}),
        )
        .await;
    assert_ok(&configured);
    assert_eq!(configured["result"]["name"], "node", "{configured}");
    assert_eq!(configured["result"]["auto"], true, "{configured}");
    let file: Value =
        serde_json::from_slice(&std::fs::read(home.join("runtimes/config.json")).unwrap()).unwrap();
    assert_eq!(file["node"]["source"], "system", "{file}");
    assert_eq!(file["node"]["auto"], true, "{file}");
    assert_eq!(
        status(&mut client, 0, "ready").await,
        ready,
        "configuring what is already in effect changes nothing"
    );

    let listed = client.rpc("runtime.list", json!({})).await;
    assert_eq!(listed["result"][0]["auto"], true, "{listed}");
    assert_eq!(listed["result"][1]["auto"], false, "{listed}");
    let untouched = listed["result"][1].clone();

    let configured = client
        .rpc("runtime.configure", json!({"name": "node", "auto": false}))
        .await;
    assert_ok(&configured);
    assert_eq!(configured["result"]["auto"], false, "{configured}");
    let listed = client.rpc("runtime.list", json!({})).await;
    assert_eq!(
        listed["result"][1], untouched,
        "applying one runtime leaves the other as it was: {listed}"
    );

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// Switching a running runtime's source replaces its launch command and
/// starts the new interpreter, and leaves the other runtime alone: the case a
/// configuration change exists for. It downloads the official archive.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "downloads the Node archive and installs its packages"]
async fn runtime_switches_from_system_to_downloaded_node() {
    let home = PathBuf::from("/tmp").join(format!("drm-switch-{}", uuid::Uuid::new_v4()));
    let plugins = home.join("sources");
    std::fs::create_dir_all(&plugins).unwrap();
    let defaults = home.join("default-plugins.json");
    std::fs::write(
        &defaults,
        r#"{"runtimes": {"node": {"source": "system", "auto": true},
                         "python": {"source": "system", "auto": false}}}"#,
    )
    .unwrap();
    let marker = home.join("node-started");
    std::fs::write(
        plugins.join("probe.mjs"),
        format!(
            "import {{ writeFileSync }} from 'node:fs'\n\
             export default {{ name: 'probe', apply() {{ writeFileSync({:?}, String(process.pid)) }} }}\n",
            marker
        ),
    )
    .unwrap();
    let rows = vec![json!({"id": "probe", "name": plugins.join("probe.mjs"), "provides": {}})];
    let runtimes = Arc::new(
        Runtimes::load(
            &home,
            &dim_app_server::plugin_manager::Defaults::read(&defaults)
                .unwrap()
                .runtimes,
            None,
        )
        .unwrap(),
    );
    let server = launch(
        Arc::new(MockTools),
        ServerConfig {
            target_home: Some(home.clone()),
            session_typescript: Some(SessionTypeScriptConfig::acquired(&home, runtimes, rows)),
            ..Default::default()
        },
    )
    .await;
    let mut client = Client::new_version(&server, false, 3).await;
    appears(&marker).await;
    status(&mut client, 0, "ready").await;
    let before = std::fs::read_to_string(&marker).unwrap();
    let python = client.rpc("runtime.list", json!({})).await["result"][1].clone();

    let switched = client
        .rpc(
            "runtime.configure",
            json!({"name": "node", "source": "download"}),
        )
        .await;
    assert_ok(&switched);
    assert!(
        switched["result"]["path"].is_null(),
        "the record of the source it left reports no path: {switched}"
    );
    let deadline = tokio::time::Instant::now() + Duration::from_secs(300);
    let after = loop {
        let seen = std::fs::read_to_string(&marker).unwrap_or_default();
        if seen != before {
            break seen;
        }
        if tokio::time::Instant::now() >= deadline {
            let listed = client.rpc("runtime.list", json!({})).await;
            panic!("the new interpreter never replaced process {seen}: {listed}");
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    assert_ne!(after, before, "the new interpreter started a new process");
    let ready = status(&mut client, 0, "ready").await;
    assert_eq!(ready["source"], "download", "{ready}");
    assert_eq!(ready["version"], "24.21.0", "{ready}");
    let path = Path::new(ready["path"].as_str().expect("the resolved interpreter"));
    assert!(path.is_absolute(), "{ready}");
    assert_eq!(path.file_name().unwrap(), "node", "{ready}");
    assert!(
        path.starts_with(home.join("runtimes"))
            && path.to_string_lossy().contains("node-v24.21.0-"),
        "the acquired archive's interpreter: {ready}"
    );
    let listed = client.rpc("runtime.list", json!({})).await;
    assert_eq!(
        listed["result"][1], python,
        "the other runtime is untouched: {listed}"
    );

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// A home whose Node resolves through `environment`, with the runtime's own
/// packages already there: acquiring Node then runs no npm, so only the call
/// under test does.
fn fixture_home(name: &str, environment: Environment) -> (PathBuf, Arc<Runtimes>) {
    let home = std::env::temp_dir().join(format!("drm-{name}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(home.join("runtimes")).unwrap();
    let defaults = write_defaults(
        &home,
        r#"{"node": {"source": "system", "auto": false},
            "python": {"source": "download", "auto": false},
            "uv": {"source": "download", "auto": false}}"#,
    );
    let runtimes = load_runtimes(&home, &defaults, Some(environment));
    let packages = runtimes.plan(Kind::Node).packages.clone().unwrap();
    std::fs::create_dir_all(packages).unwrap();
    (home, runtimes)
}

/// A server serving `home`'s runtimes.
async fn serve(home: &Path, runtimes: Arc<Runtimes>) -> RunningServer {
    launch(
        Arc::new(MockTools),
        ServerConfig {
            target_home: Some(home.to_owned()),
            session_typescript: Some(SessionTypeScriptConfig::acquired(home, runtimes, vec![])),
            ..Default::default()
        },
    )
    .await
}

/// A plugin's staging directory: its `package.json` and the lock npm reads.
fn staging(home: &Path) -> PathBuf {
    let dir = home.join("plugins/.staging/fixture");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("package.json"),
        r#"{"name": "ocr-fixture", "version": "1.0.0", "dependencies": {"fixture-dep": "file:fixture-dep"}}"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("package-lock.json"),
        r#"{"name": "ocr-fixture", "version": "1.0.0", "lockfileVersion": 3, "packages": {"": {"name": "ocr-fixture", "version": "1.0.0", "dependencies": {"fixture-dep": "file:fixture-dep"}}}}"#,
    )
    .unwrap();
    dir
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

/// `installPackages` installs a plugin's locked dependencies into the plugin's
/// directory with Node's own npm, under the flags the runtime's own install
/// uses (§5, §9 R1).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn install_packages_installs_the_plugins_lock_with_the_nodes_own_npm() {
    let fixture = FixtureNode::with(
        "24.21.0",
        "mkdir -p node_modules/fixture-dep\n\
         echo 'export default 1' > node_modules/fixture-dep/index.js",
    );
    let (home, runtimes) = fixture_home("packages-install", environment(fixture.search_path()));
    let server = serve(&home, runtimes).await;
    let service = server
        .service::<RuntimeService>()
        .expect("the runtime service");
    let dir = staging(&home);

    service
        .install_packages(&dir, &CancellationToken::new())
        .await
        .unwrap();

    let runs = fixture.runs();
    assert_eq!(runs.len(), 1, "npm ran once: {runs:?}");
    let prefix = format!(
        "{} ci --ignore-scripts --omit=dev --no-audit --no-fund --workspaces=false --registry ",
        dir.canonicalize().unwrap().display()
    );
    assert!(runs[0].starts_with(&prefix), "{runs:?}");
    assert!(
        runs[0][prefix.len()..].starts_with("https://"),
        "the registry is a URL: {runs:?}"
    );
    assert!(
        dir.join("node_modules/fixture-dep/index.js").is_file(),
        "npm installed into the plugin's directory"
    );
    assert_eq!(
        entries(&dir),
        ["node_modules", "package-lock.json", "package.json"],
        "the install left nothing else, the cache included"
    );

    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// The registries are tried in the plan's order — `DIMCODE_NPM_REGISTRY`
/// first, then `npmRegistries` (§5, G10) — and a failure names each one tried.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn install_packages_tries_the_environment_registry_first_and_names_each() {
    let registry = "https://env.invalid";
    let fixture = FixtureNode::with(
        "24.21.0",
        // Refuses the registry this host sets; the pinned ones are tried next.
        &format!(
            "case \" $* \" in *\" --registry {registry} \"*) \
             echo 'registry unavailable' >&2; exit 1;; esac"
        ),
    );
    // The environment is this host's input: the process is never read, so no
    // other flow in this binary sees the registry.
    let mut environment = environment(fixture.search_path());
    environment.npm_registry = Some(registry.into());
    let (home, runtimes) = fixture_home("packages-registries", environment);
    let server = serve(&home, runtimes).await;
    let service = server
        .service::<RuntimeService>()
        .expect("the runtime service");
    let dir = staging(&home);
    let cancel = CancellationToken::new();

    service.install_packages(&dir, &cancel).await.unwrap();
    let runs = fixture.runs();
    assert_eq!(
        runs.len(),
        2,
        "the refused registry, then a pinned one: {runs:?}"
    );
    assert!(
        runs[0].ends_with(&format!(" --registry {registry}")),
        "{runs:?}"
    );
    assert!(
        runs[1].ends_with(" --registry https://registry.npmjs.org"),
        "{runs:?}"
    );

    // npm failing for every registry names every one it was given.
    fixture.npm("echo refused >&2\nexit 1");
    let error = service.install_packages(&dir, &cancel).await.unwrap_err();
    assert_eq!(error.code, "install_failed", "{error}");
    assert!(error.message.contains(registry), "{error}");
    assert!(
        error.message.contains("https://registry.npmjs.org"),
        "{error}"
    );

    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// A cancelled `installPackages` kills npm and leaves `dir` to the caller:
/// nothing is half-installed, and even the cache is gone (§5, §9 R1).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn install_packages_cancelled_kills_npm_and_leaves_the_directory() {
    let fixture = FixtureNode::with("24.21.0", "exit 0");
    let running = fixture.path("running");
    std::fs::write(&running, "").unwrap();
    let pid = fixture.path("pid");
    // npm blocks on a file until it is killed.
    fixture.npm(&format!(
        "echo $$ > '{0}.tmp' && mv '{0}.tmp' '{0}'\nexec tail -f '{1}'",
        pid.display(),
        running.display()
    ));
    let (home, runtimes) = fixture_home("packages-cancel", environment(fixture.search_path()));
    let server = serve(&home, runtimes).await;
    let service = server
        .service::<RuntimeService>()
        .expect("the runtime service");
    let dir = staging(&home);
    let cancel = CancellationToken::new();
    let call = tokio::spawn({
        let service = service.clone();
        let dir = dir.clone();
        let cancel = cancel.clone();
        async move { service.install_packages(&dir, &cancel).await }
    });

    // npm is running once it has written its own process id.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    while !pid.is_file() {
        assert!(tokio::time::Instant::now() < deadline, "npm never started");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let process: u32 = std::fs::read_to_string(&pid)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    cancel.cancel();
    let error = call.await.unwrap().unwrap_err();
    assert_eq!(error.code, "cancelled", "{error}");

    // The child is gone: dropping the command that owns it kills it.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while std::process::Command::new("kill")
        .args(["-0", &process.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap()
        .success()
    {
        assert!(
            tokio::time::Instant::now() < deadline,
            "npm {process} outlived the call"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    assert_eq!(
        entries(&dir),
        ["package-lock.json", "package.json"],
        "the directory is left to its caller"
    );

    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// Node off and unacquirable: `installPackages` answers `node_unavailable`
/// with the acquisition's reason, and npm never runs (§5, §9 R1).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn install_packages_without_node_names_the_acquisition_failure() {
    // A Node below the floor cannot be acquired.
    let fixture = FixtureNode::with("22.18.0", "exit 0");
    let (home, runtimes) = fixture_home("packages-unavailable", environment(fixture.search_path()));
    let server = serve(&home, runtimes).await;
    let service = server
        .service::<RuntimeService>()
        .expect("the runtime service");
    let dir = staging(&home);

    let error = service
        .install_packages(&dir, &CancellationToken::new())
        .await
        .unwrap_err();
    assert_eq!(error.code, "node_unavailable", "{error}");
    assert!(error.message.contains("22.18.0"), "{error}");
    assert!(error.message.contains("22.19"), "{error}");
    assert!(
        fixture.runs().is_empty(),
        "npm never ran: {:?}",
        fixture.runs()
    );

    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// A `download` Python marks its interpreter as externally managed inside its
/// own archive directory (RuntimeManager §7), so pip never installs into a
/// runtime Dim replaces. It downloads the archive and installs its packages,
/// so it runs only with the other ignored flows.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "downloads the Python archive and installs its packages"]
async fn python_runtime_manager_marks_the_downloaded_interpreter_as_externally_managed() {
    let home = std::env::temp_dir().join(format!("drm-marker-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&home).unwrap();
    let defaults = home.join("default-plugins.json");
    std::fs::write(
        &defaults,
        r#"{"runtimes": {"node": {"source": "system", "auto": false},
                         "python": {"source": "download", "auto": false}}}"#,
    )
    .unwrap();
    let runtimes = Arc::new(
        Runtimes::load(
            &home,
            &dim_app_server::plugin_manager::Defaults::read(&defaults)
                .unwrap()
                .runtimes,
            None,
        )
        .unwrap(),
    );
    let python = runtimes.plan(Kind::Python).clone();
    let server = launch(
        Arc::new(MockTools),
        ServerConfig {
            target_home: Some(home.clone()),
            session_typescript: Some(SessionTypeScriptConfig::acquired(&home, runtimes, vec![])),
            ..Default::default()
        },
    )
    .await;
    let mut client = Client::new_version(&server, false, 3).await;
    assert_ok(
        &client
            .rpc("runtime.install", json!({"name": "python"}))
            .await,
    );
    let ready = status(&mut client, 1, "ready").await;
    assert_eq!(ready["source"], "download", "{ready}");
    let archive = archive_directory(&python.interpreter);
    let stdlib = if cfg!(windows) {
        archive.join("Lib")
    } else {
        archive.join("lib").join(format!(
            "python3.{}",
            python.version.split('.').nth(1).expect("a pinned version")
        ))
    };
    let marker = stdlib.join("EXTERNALLY-MANAGED");
    let text = std::fs::read_to_string(&marker)
        .unwrap_or_else(|error| panic!("{}: {error}", marker.display()));
    assert!(
        text.starts_with("[externally-managed]") && text.contains("Error="),
        "{text}"
    );
    assert!(stdlib.is_dir(), "the marker is in the interpreter's stdlib");
    let output = tokio::process::Command::new(&python.interpreter)
        .args([
            "-c",
            "import sysconfig; print(sysconfig.get_path('stdlib'))",
        ])
        .output()
        .await
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let reported = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
    assert_eq!(
        std::fs::canonicalize(reported).unwrap(),
        std::fs::canonicalize(&stdlib).unwrap(),
        "the marker's directory is the interpreter's sysconfig stdlib"
    );

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// The next item of watch call `id`: its `management.progress` data.
async fn watch_item(client: &mut Client, id: u64) -> Value {
    loop {
        let notice = client.notice("management.progress").await;
        if notice["params"]["id"] == id {
            assert_eq!(
                notice["params"]["method"], "runtime.watch",
                "an item belongs to the watch call: {notice}"
            );
            return notice["params"]["data"].clone();
        }
    }
}

/// Reads the watch's items, re-reading `runtime.list` after each as the
/// client does, until `accept` takes the listing.
async fn watch_until(
    client: &mut Client,
    id: u64,
    mut accept: impl FnMut(&Value) -> bool,
) -> (Vec<Value>, Value) {
    let mut items = Vec::new();
    loop {
        items.push(watch_item(client, id).await);
        let listed = client.rpc("runtime.list", json!({})).await;
        assert_ok(&listed);
        if accept(&listed["result"]) {
            return (items, listed["result"].clone());
        }
    }
}

/// A `system` Node that passes the version check and the package step without
/// a real Node or npm: `bin/node` answers `--version` with its version, and
/// `lib/node_modules/npm/bin/npm-cli.js` is the npm the check looks for.
/// `new` answers nothing else, so its connection row never greets and goes
/// down; `with` runs the npm body instead, after recording each run — the
/// working directory, then the arguments. The plugin manager flows reuse the
/// fixture for a plugin's dependency install.
pub(super) struct FixtureNode {
    directory: PathBuf,
    log: PathBuf,
}

impl FixtureNode {
    pub(super) fn new() -> Self {
        let directory = Self::directory();
        write_script(
            &directory.join("bin"),
            "node",
            "if [ \"$1\" = \"--version\" ]; then echo v22.19.0; fi",
        );
        std::fs::write(directory.join("lib/node_modules/npm/bin/npm-cli.js"), "").unwrap();
        let log = directory.join("npm.log");
        Self { directory, log }
    }

    /// A Node reporting `version` and otherwise running what it was given.
    pub(super) fn with(version: &str, npm: &str) -> Self {
        let directory = Self::directory();
        write_script(
            &directory.join("bin"),
            "node",
            &format!("if [ \"$1\" = --version ]; then echo v{version}; exit 0; fi\nexec \"$@\""),
        );
        let log = directory.join("npm.log");
        let fixture = Self { directory, log };
        fixture.npm(npm);
        fixture
    }

    fn directory() -> PathBuf {
        let directory = std::env::temp_dir().join(format!("dm-node-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(directory.join("bin")).unwrap();
        std::fs::create_dir_all(directory.join("lib/node_modules/npm/bin")).unwrap();
        directory
    }

    /// Replaces what npm does, after the run is recorded.
    pub(super) fn npm(&self, body: &str) {
        write_script(
            &self.directory.join("lib/node_modules/npm/bin"),
            "npm-cli.js",
            &format!("echo \"$PWD $*\" >> '{}'\n{body}", self.log.display()),
        );
    }

    fn path(&self, name: &str) -> PathBuf {
        self.directory.join(name)
    }

    /// The search path that resolves the fixture as the system Node.
    pub(super) fn search_path(&self) -> std::ffi::OsString {
        self.directory.join("bin").into_os_string()
    }

    /// Each npm run: its working directory, then its arguments.
    fn runs(&self) -> Vec<String> {
        std::fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }
}

impl Drop for FixtureNode {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

/// Keeps the runtime context so a test can restart a row the way a reload
/// does.
struct Probe(Arc<std::sync::Mutex<Option<rutis::Ctx>>>);

impl rutis::host::DynPlugin for Probe {
    fn name(&self) -> &str {
        "test.runtime-watch-probe"
    }
    fn apply<'a>(
        &'a self,
        ctx: &'a rutis::Ctx,
    ) -> rutis::host::BoxFuture<'a, rutis::Result<rutis::host::Effect>> {
        Box::pin(async move {
            *self.0.lock().unwrap() = Some(ctx.clone());
            Ok(rutis::host::Effect::Done)
        })
    }
}

/// A watch streams one item per committed change, and only to the connection
/// that opened it; the client's re-read shows the installed tool (§10).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_watch_streams_a_tool_install_to_the_watching_connection_only() {
    let fixture = FixtureUv::new("0.13.7");
    let home = std::env::temp_dir().join(format!("drm-watch-uv-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(home.join("runtimes")).unwrap();
    let defaults = write_defaults(
        &home,
        r#"{"node": {"source": "download", "auto": false},
            "python": {"source": "download", "auto": false},
            "uv": {"source": "system", "auto": false}}"#,
    );
    let runtimes = load_runtimes(&home, &defaults, Some(environment(fixture.search_path())));
    let server = launch(
        Arc::new(MockTools),
        ServerConfig {
            target_home: Some(home.clone()),
            session_typescript: Some(SessionTypeScriptConfig::acquired(&home, runtimes, vec![])),
            ..Default::default()
        },
    )
    .await;
    let mut client = Client::new_version(&server, false, 3).await;
    let mut other = Client::new_version(&server, false, 3).await;

    let id = client.send("runtime.watch", json!({})).await;
    assert_eq!(
        watch_item(&mut client, id).await,
        json!({"kind": "watching", "seq": 0})
    );
    assert_ok(&client.rpc("runtime.install", json!({"name": "uv"})).await);
    let (items, listed) = watch_until(&mut client, id, |list| list[2]["status"] == "ready").await;
    for (index, item) in items.iter().enumerate() {
        assert_eq!(item["kind"], "runtime", "{item}");
        assert_eq!(item["name"], "uv", "{item}");
        assert_eq!(
            item["seq"],
            (index + 1) as u64,
            "one item per change, in order: {item}"
        );
    }
    assert_eq!(
        listed[2]["path"],
        fixture.directory.join("uv").to_str().unwrap(),
        "{listed}"
    );
    assert_ok(&other.rpc("daemon.ping", json!({})).await);
    assert!(
        other
            .notices
            .iter()
            .all(|notice| notice["method"] != "management.progress"),
        "a connection that did not watch received items: {:?}",
        other.notices
    );

    drop(client);
    drop(other);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// A failed acquisition streams its item too, and the client's re-read shows
/// `failed` with the reason (§10).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_watch_streams_a_failed_acquisition() {
    let fixture = FixtureUv::with_fallback("0.13.7", Some("0.10.9"));
    let home = std::env::temp_dir().join(format!("drm-watch-failed-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(home.join("runtimes")).unwrap();
    let defaults = write_defaults(
        &home,
        r#"{"node": {"source": "download", "auto": false},
            "python": {"source": "download", "auto": false},
            "uv": {"source": "system", "auto": false}}"#,
    );
    let runtimes = load_runtimes(&home, &defaults, Some(environment(fixture.search_path())));
    let server = launch(
        Arc::new(MockTools),
        ServerConfig {
            target_home: Some(home.clone()),
            session_typescript: Some(SessionTypeScriptConfig::acquired(&home, runtimes, vec![])),
            ..Default::default()
        },
    )
    .await;
    let mut client = Client::new_version(&server, false, 3).await;
    let id = client.send("runtime.watch", json!({})).await;
    watch_item(&mut client, id).await;
    assert_ok(&client.rpc("runtime.install", json!({"name": "uv"})).await);
    watch_until(&mut client, id, |list| list[2]["status"] == "ready").await;

    std::fs::remove_file(fixture.directory.join("uv")).unwrap();
    assert_ok(&client.rpc("runtime.install", json!({"name": "uv"})).await);
    let (items, listed) = watch_until(&mut client, id, |list| list[2]["status"] == "failed").await;
    assert!(!items.is_empty(), "the failed acquisition sent no item");
    let reason = listed[2]["reason"].as_str().unwrap_or_default();
    assert!(reason.contains("0.10.9"), "{listed}");
    assert!(reason.contains("0.11"), "{listed}");

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// `runtime.configure` streams its item, and the client's re-read shows the
/// configuration in effect (§10).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_watch_streams_a_configuration_change() {
    let _fixture = FixtureUv::new("0.13.7");
    let home = std::env::temp_dir().join(format!("drm-watch-config-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(home.join("runtimes")).unwrap();
    let defaults = write_defaults(
        &home,
        r#"{"node": {"source": "download", "auto": false},
            "python": {"source": "download", "auto": false},
            "uv": {"source": "system", "auto": false}}"#,
    );
    let runtimes = load_runtimes(&home, &defaults, Some(environment(_fixture.search_path())));
    let server = launch(
        Arc::new(MockTools),
        ServerConfig {
            target_home: Some(home.clone()),
            session_typescript: Some(SessionTypeScriptConfig::acquired(&home, runtimes, vec![])),
            ..Default::default()
        },
    )
    .await;
    let mut client = Client::new_version(&server, false, 3).await;
    let id = client.send("runtime.watch", json!({})).await;
    watch_item(&mut client, id).await;
    assert_ok(
        &client
            .rpc("runtime.configure", json!({"name": "uv", "auto": true}))
            .await,
    );
    let (items, listed) = watch_until(&mut client, id, |list| {
        list[2]["auto"] == true && list[2]["status"] == "ready"
    })
    .await;
    assert!(!items.is_empty(), "the configuration change sent no item");
    assert_eq!(listed[2]["auto"], true, "{listed}");

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// One watch per connection: a second watch answers the first `{}`, and the
/// surviving call receives one item per change (§10).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_second_watch_takes_over_the_first() {
    let _fixture = FixtureUv::new("0.13.7");
    let home = std::env::temp_dir().join(format!("drm-watch-twice-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(home.join("runtimes")).unwrap();
    let defaults = write_defaults(
        &home,
        r#"{"node": {"source": "download", "auto": false},
            "python": {"source": "download", "auto": false},
            "uv": {"source": "system", "auto": false}}"#,
    );
    let runtimes = load_runtimes(&home, &defaults, Some(environment(_fixture.search_path())));
    let server = launch(
        Arc::new(MockTools),
        ServerConfig {
            target_home: Some(home.clone()),
            session_typescript: Some(SessionTypeScriptConfig::acquired(&home, runtimes, vec![])),
            ..Default::default()
        },
    )
    .await;
    let mut client = Client::new_version(&server, false, 3).await;
    let first = client.send("runtime.watch", json!({})).await;
    assert_eq!(
        watch_item(&mut client, first).await,
        json!({"kind": "watching", "seq": 0})
    );
    let second = client.send("runtime.watch", json!({})).await;
    assert_eq!(
        watch_item(&mut client, second).await,
        json!({"kind": "watching", "seq": 0})
    );
    let taken = client.response(first).await;
    assert_eq!(
        taken["result"],
        json!({}),
        "the first watch answers the takeover: {taken}"
    );
    assert_ok(&client.rpc("runtime.install", json!({"name": "uv"})).await);
    let (items, _) = watch_until(&mut client, second, |list| list[2]["status"] == "ready").await;
    for (index, item) in items.iter().enumerate() {
        assert_eq!(
            item["seq"],
            (index + 1) as u64,
            "one item per change on the surviving watch: {item}"
        );
    }

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// A closed connection stops being a watcher and fails nothing: the other
/// watcher still receives its items (§10).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_closed_watcher_fails_nothing_for_another_watcher() {
    let fixture = FixtureUv::new("0.13.7");
    let home = std::env::temp_dir().join(format!("drm-watch-closed-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(home.join("runtimes")).unwrap();
    let defaults = write_defaults(
        &home,
        r#"{"node": {"source": "download", "auto": false},
            "python": {"source": "download", "auto": false},
            "uv": {"source": "system", "auto": false}}"#,
    );
    let runtimes = load_runtimes(&home, &defaults, Some(environment(fixture.search_path())));
    let server = launch(
        Arc::new(MockTools),
        ServerConfig {
            target_home: Some(home.clone()),
            session_typescript: Some(SessionTypeScriptConfig::acquired(&home, runtimes, vec![])),
            ..Default::default()
        },
    )
    .await;
    let mut gone = Client::new_version(&server, false, 3).await;
    let mut survivor = Client::new_version(&server, false, 3).await;
    let gone_id = gone.send("runtime.watch", json!({})).await;
    watch_item(&mut gone, gone_id).await;
    let survivor_id = survivor.send("runtime.watch", json!({})).await;
    watch_item(&mut survivor, survivor_id).await;
    drop(gone);

    assert_ok(&survivor.rpc("runtime.install", json!({"name": "uv"})).await);
    let (_, listed) = watch_until(&mut survivor, survivor_id, |list| {
        list[2]["status"] == "ready"
    })
    .await;
    assert_eq!(
        listed[2]["path"],
        fixture.directory.join("uv").to_str().unwrap(),
        "{listed}"
    );
    // The change after the close failed nothing: the next one is served too.
    assert_ok(
        &survivor
            .rpc("runtime.configure", json!({"name": "uv", "auto": true}))
            .await,
    );
    let (items, listed) = watch_until(&mut survivor, survivor_id, |list| {
        list[2]["auto"] == true && list[2]["status"] == "ready"
    })
    .await;
    assert!(!items.is_empty(), "the change after the close sent no item");
    assert_eq!(listed[2]["auto"], true, "{listed}");

    drop(survivor);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// A reload answers the open call `operation_interrupted`; a watch made on
/// the new generation receives the next change (§10).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_reload_answers_the_open_call_operation_interrupted() {
    let fixture = FixtureUv::new("0.13.7");
    let home = std::env::temp_dir().join(format!("drm-watch-reload-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(home.join("runtimes")).unwrap();
    let defaults = write_defaults(
        &home,
        r#"{"node": {"source": "download", "auto": false},
            "python": {"source": "download", "auto": false},
            "uv": {"source": "system", "auto": false}}"#,
    );
    let runtimes = load_runtimes(&home, &defaults, Some(environment(fixture.search_path())));
    let probe = Arc::new(std::sync::Mutex::new(None));
    let (server, _) = launch_with(
        Arc::new(MockTools),
        ServerConfig {
            target_home: Some(home.clone()),
            session_typescript: Some(SessionTypeScriptConfig::acquired(&home, runtimes, vec![])),
            ..Default::default()
        },
        {
            let probe = probe.clone();
            move |builder| {
                builder.plugin(move |_| {
                    Ok(Box::new(Probe(probe.clone())) as Box<dyn rutis::host::DynPlugin>)
                });
            }
        },
    )
    .await;
    let mut client = Client::new_version(&server, false, 3).await;
    let id = client.send("runtime.watch", json!({})).await;
    watch_item(&mut client, id).await;

    let row = server
        .diagnostics()
        .plugins
        .iter()
        .find(|plugin| plugin.name == "RuntimeManager")
        .map(|plugin| plugin.id)
        .expect("the RuntimeManager row");
    let ctx = probe
        .lock()
        .unwrap()
        .clone()
        .expect("the probe kept the runtime context");
    ctx.view(row)
        .expect("the RuntimeManager fiber")
        .restart()
        .await
        .unwrap();
    let interrupted = client.response(id).await;
    assert_eq!(
        interrupted["error"]["data"]["appError"], "operation_interrupted",
        "{interrupted}"
    );

    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let listed = client.rpc("runtime.list", json!({})).await;
            if listed.get("result").is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the runtime namespace is served again");

    let again = client.send("runtime.watch", json!({})).await;
    assert_eq!(
        watch_item(&mut client, again).await,
        json!({"kind": "watching", "seq": 0})
    );
    assert_ok(&client.rpc("runtime.install", json!({"name": "uv"})).await);
    let (items, listed) =
        watch_until(&mut client, again, |list| list[2]["status"] == "ready").await;
    assert!(!items.is_empty(), "the new watch received no item");
    assert_eq!(
        listed[2]["path"],
        fixture.directory.join("uv").to_str().unwrap(),
        "{listed}"
    );

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// A connection row going `down` streams an item, and the client's re-read
/// shows `down` with its reason (§10).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_watch_streams_a_row_going_down() {
    let node = FixtureNode::new();
    let home = std::env::temp_dir().join(format!("drm-watch-down-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(home.join("runtimes")).unwrap();
    let defaults = write_defaults(
        &home,
        r#"{"node": {"source": "system", "auto": false},
            "python": {"source": "download", "auto": false},
            "uv": {"source": "download", "auto": false}}"#,
    );
    let runtimes = load_runtimes(&home, &defaults, Some(environment(node.search_path())));
    assert_eq!(
        runtimes.plan(Kind::Node).interpreter,
        node.directory.join("bin/node"),
        "the plan resolves the fixture node"
    );
    let server = launch(
        Arc::new(MockTools),
        ServerConfig {
            target_home: Some(home.clone()),
            session_typescript: Some(SessionTypeScriptConfig::acquired(&home, runtimes, vec![])),
            ..Default::default()
        },
    )
    .await;
    let mut client = Client::new_version(&server, false, 3).await;
    let id = client.send("runtime.watch", json!({})).await;
    watch_item(&mut client, id).await;
    assert_ok(&client.rpc("runtime.install", json!({"name": "node"})).await);
    let (items, listed) = watch_until(&mut client, id, |list| list[0]["status"] == "down").await;
    assert!(!items.is_empty(), "the row going down sent no item");
    assert!(
        listed[0]["reason"]
            .as_str()
            .is_some_and(|reason| !reason.is_empty()),
        "{listed}"
    );

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

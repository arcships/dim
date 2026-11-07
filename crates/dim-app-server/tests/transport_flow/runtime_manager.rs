//! RuntimeManager (RuntimeManager §3): the runtimes arrive after startup and
//! the plugin rows waiting for them start by themselves. `system` sources
//! keep the interpreters off the network; the pinned packages are installed
//! for real (`npm ci`, `pip install --require-hashes`).
use super::*;
use dim_app_server::{
    runtime_manager::{Kind, Runtimes},
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
        )
        .unwrap(),
    );
    let node = runtimes.plan(Kind::Node).clone();
    let node_program = node.program().expect("Node is a program").to_owned();
    std::fs::create_dir_all(node.packages.as_ref().expect("Node packages")).unwrap();
    std::fs::create_dir_all(archive_directory(&node_program)).unwrap();
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
        node_program.to_str().unwrap(),
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

/// Fixture `uv` and `uvx` pairs in front of this process's `PATH`, restored
/// and removed on drop. Tests that use one hold `PATH_LOCK` for their whole
/// life, because `runtime.install` reads `PATH` when it applies the
/// configuration.
struct FixtureUv {
    /// The first directory: the uv the plan starts with.
    directory: PathBuf,
    /// Directories between it and the original `PATH`. A reload resolves the
    /// first of these once the first directory has no uv.
    fallbacks: Vec<PathBuf>,
    path: Option<std::ffi::OsString>,
}

impl FixtureUv {
    fn new(version: &str) -> Self {
        Self::with_fallback(version, None)
    }

    /// `A:B:<original PATH>`: `A` holds the `version` uv and `B` one that
    /// reports `fallback`, so removing `A`'s uv leaves the reload resolving
    /// `B` before any uv installed on the host.
    fn with_fallback(version: &str, fallback: Option<&str>) -> Self {
        let path = std::env::var_os("PATH");
        let directory = Self::fixture(version);
        let fallbacks: Vec<PathBuf> = fallback.into_iter().map(Self::fixture).collect();
        let mut paths = vec![directory.clone()];
        paths.extend(fallbacks.iter().cloned());
        paths.extend(path.iter().flat_map(std::env::split_paths));
        let joined = std::env::join_paths(paths).unwrap();
        // SAFETY: every test that changes PATH holds PATH_LOCK until it
        // restores it, so no other test reads a half-prefixed PATH.
        unsafe { std::env::set_var("PATH", &joined) };
        Self {
            directory,
            fallbacks,
            path,
        }
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
        // SAFETY: as above: PATH_LOCK is held for the whole lifetime.
        unsafe {
            match &self.path {
                Some(path) => std::env::set_var("PATH", path),
                None => std::env::remove_var("PATH"),
            }
        }
        for directory in std::iter::once(&self.directory).chain(&self.fallbacks) {
            let _ = std::fs::remove_dir_all(directory);
        }
    }
}

static PATH_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn write_defaults(home: &Path, runtimes: &str) -> PathBuf {
    let path = home.join("default-plugins.json");
    std::fs::write(&path, format!(r#"{{"runtimes": {runtimes}}}"#)).unwrap();
    path
}

fn load_runtimes(home: &Path, defaults: &Path) -> Arc<Runtimes> {
    Arc::new(
        Runtimes::load(
            home,
            &dim_app_server::plugin_manager::Defaults::read(defaults)
                .unwrap()
                .runtimes,
        )
        .unwrap(),
    )
}

/// `runtime.list` shows the uv tool, off until it is installed, after the two
/// runtimes, and the OCR component after it: a component is listed like a
/// tool, with no connection of its own.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn runtime_list_shows_the_uv_tool_off() {
    let home = std::env::temp_dir().join(format!("drm-uv-off-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(home.join("runtimes")).unwrap();
    let defaults = write_defaults(
        &home,
        r#"{"node": {"source": "download", "auto": false},
            "python": {"source": "download", "auto": false},
            "uv": {"source": "download", "auto": false},
            "ocr": {"source": "download", "auto": false}}"#,
    );
    let runtimes = load_runtimes(&home, &defaults);
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
    assert_eq!(listed["result"].as_array().unwrap().len(), 4, "{listed}");
    let uv = &listed["result"][2];
    assert_eq!(uv["name"], "uv", "{listed}");
    assert_eq!(uv["source"], "download", "{listed}");
    assert_eq!(uv["version"], "0.13.7", "{listed}");
    assert_eq!(uv["status"], "off", "{listed}");
    assert!(uv["path"].is_null(), "{listed}");
    let ocr = &listed["result"][3];
    assert_eq!(ocr["name"], "ocr", "{listed}");
    assert_eq!(ocr["source"], "download", "{listed}");
    assert_eq!(ocr["version"], "1", "{listed}");
    assert_eq!(ocr["status"], "off", "{listed}");
    assert!(ocr["path"].is_null(), "{listed}");
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
    let _path = PATH_LOCK.lock().await;
    let fixture = FixtureUv::new("0.13.7");
    let home = std::env::temp_dir().join(format!("drm-uv-system-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(home.join("runtimes")).unwrap();
    let defaults = write_defaults(
        &home,
        r#"{"node": {"source": "download", "auto": false},
            "python": {"source": "download", "auto": false},
            "uv": {"source": "system", "auto": false}}"#,
    );
    let runtimes = load_runtimes(&home, &defaults);
    assert_eq!(
        runtimes.plan(Kind::Uv).program(),
        Some(fixture.directory.join("uv").as_path()),
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
    let _path = PATH_LOCK.lock().await;
    let fixture = FixtureUv::with_fallback("0.13.7", Some("0.10.9"));
    let home = std::env::temp_dir().join(format!("drm-uv-removed-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(home.join("runtimes")).unwrap();
    let defaults = write_defaults(
        &home,
        r#"{"node": {"source": "download", "auto": false},
            "python": {"source": "download", "auto": false},
            "uv": {"source": "system", "auto": false}}"#,
    );
    let runtimes = load_runtimes(&home, &defaults);
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
    let _path = PATH_LOCK.lock().await;
    let _fixture = FixtureUv::new("0.10.9");
    let home = std::env::temp_dir().join(format!("drm-uv-old-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(home.join("runtimes")).unwrap();
    let defaults = write_defaults(
        &home,
        r#"{"node": {"source": "download", "auto": false},
            "python": {"source": "download", "auto": false},
            "uv": {"source": "system", "auto": false}}"#,
    );
    let runtimes = load_runtimes(&home, &defaults);
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
    let _path = PATH_LOCK.lock().await;
    let fixture = FixtureUv::new("0.13.7");
    let home = std::env::temp_dir().join(format!("drm-uv-configure-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(home.join("runtimes")).unwrap();
    let defaults = write_defaults(
        &home,
        r#"{"node": {"source": "download", "auto": false},
            "python": {"source": "download", "auto": false},
            "uv": {"source": "download", "auto": false}}"#,
    );
    let runtimes = load_runtimes(&home, &defaults);
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
    let runtimes = load_runtimes(&home, &defaults);
    let uv = runtimes.plan(Kind::Uv).clone();
    let uv_program = uv.program().expect("uv is a program").to_owned();
    assert!(
        uv_program.to_string_lossy().contains("uv-0.13.7-"),
        "the pinned version names the archive directory: {}",
        uv_program.display()
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
    assert_eq!(ready["path"], uv_program.to_str().unwrap(), "{ready}");
    let output = tokio::process::Command::new(&uv_program)
        .arg("--version")
        .output()
        .await
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let version = String::from_utf8_lossy(&output.stdout);
    assert!(version.contains("uv 0.13.7"), "{version}");
    let uvx = uv_program.with_file_name(if cfg!(windows) { "uvx.exe" } else { "uvx" });
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
#[ignore = "requires node 22.19+ and python3 3.12+ with pip on PATH, and the npm and PyPI registries"]
async fn node_runtime_manager_starts_rows_once_their_runtime_is_acquired() {
    let home = PathBuf::from("/tmp").join(format!("drm-{}", uuid::Uuid::new_v4()));
    let plugins = home.join("sources");
    std::fs::create_dir_all(plugins.join("probe-py")).unwrap();
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
    let python_marker = home.join("python-started");
    std::fs::write(
        plugins.join("probe-py/entry.py"),
        format!(
            "import pathlib\n\ninject = []\nprovides = {{}}\n\n\n\
             async def apply(ctx, config):\n    pathlib.Path({:?}).write_text('yes')\n",
            python_marker.to_str().unwrap()
        ),
    )
    .unwrap();
    std::fs::create_dir_all(home.join("plugins/.python")).unwrap();
    std::os::unix::fs::symlink(
        plugins.join("probe-py"),
        home.join("plugins/.python/probe_py"),
    )
    .unwrap();
    let rows = vec![
        json!({"id": "probe", "name": plugins.join("probe.mjs"), "provides": {}}),
        json!({"id": "probe-py", "name": "py:probe_py.entry", "provides": {}}),
    ];
    let runtimes = Arc::new(
        Runtimes::load(
            &home,
            &dim_app_server::plugin_manager::Defaults::read(&defaults)
                .unwrap()
                .runtimes,
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
    appears(&node_marker).await;
    let list = client.rpc("runtime.list", json!({})).await;
    assert_ok(&list);
    assert_eq!(list["result"][0]["name"], "node");
    assert_eq!(list["result"][1]["status"], "off", "{list}");
    assert!(
        list["result"][1]["path"].is_null(),
        "a runtime that is off resolved nothing: {list}"
    );
    assert!(!python_marker.exists());
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

    let install = client
        .rpc("runtime.install", json!({"name": "python"}))
        .await;
    assert_ok(&install);
    appears(&python_marker).await;
    status(&mut client, 1, "ready").await;
    let records: Value =
        serde_json::from_slice(&std::fs::read(home.join("runtimes/runtimes.json")).unwrap())
            .unwrap();
    assert_eq!(records["node"]["source"], "system");
    assert_eq!(records["python"]["source"], "system");
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

    std::fs::remove_file(&python_marker).unwrap();
    let runtimes = Arc::new(
        Runtimes::load(
            &home,
            &dim_app_server::plugin_manager::Defaults::read(&defaults)
                .unwrap()
                .runtimes,
        )
        .unwrap(),
    );
    let rows = vec![json!({"id": "probe-py", "name": "py:probe_py.entry", "provides": {}})];
    let server = launch(
        Arc::new(MockTools),
        ServerConfig {
            target_home: Some(home.clone()),
            session_typescript: Some(SessionTypeScriptConfig::acquired(&home, runtimes, rows)),
            ..Default::default()
        },
    )
    .await;
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
/// configuration change exists for. It downloads the archive, so it runs only
/// with the other ignored flows.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "downloads the Node archive and installs its packages"]
async fn node_runtime_manager_switches_a_running_runtime_to_another_source() {
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
        assert!(
            tokio::time::Instant::now() < deadline,
            "the new interpreter never started a new process: {seen}"
        );
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
    let python_program = python.program().expect("Python is a program").to_owned();
    let archive = archive_directory(&python_program);
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
    let output = tokio::process::Command::new(&python_program)
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

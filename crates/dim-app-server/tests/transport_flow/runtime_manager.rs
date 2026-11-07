//! RuntimeManager (RuntimeManager §3): the runtimes arrive after startup and
//! the plugin rows waiting for them start by themselves. `system` sources
//! keep the interpreters off the network; the pinned packages are installed
//! for real (`npm ci`, `pip install --require-hashes`). The OCR component's
//! bundle is served from a loopback listener, with a stub verifier standing in
//! for the owner that checks it.
use super::*;
use dim_agent_capabilities::component::{
    ComponentSourceHost, ComponentState, ComponentVerifier, ComponentVerifierHost,
};
use dim_agent_capabilities::toolchain::ToolchainSourceHost;
use dim_app_server::{
    runtime_manager::{Kind, Runtimes},
    session_typescript::SessionTypeScriptConfig,
};
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, Key, TypeKey};
use std::path::{Path, PathBuf};
use std::sync::Mutex as StdMutex;
use std::sync::atomic::{AtomicUsize, Ordering};

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

/// The platform key the pins and the bundle use, for the machine running the
/// test.
fn platform() -> String {
    let os = match std::env::consts::OS {
        "macos" => "darwin",
        other => other,
    };
    let arch = match std::env::consts::ARCH {
        "aarch64" => "arm64",
        "x86_64" => "x64",
        other => other,
    };
    format!("{os}-{arch}")
}

/// The repository's pins with the component served from `address`: the
/// checksum file and the archive come from the test, so the acquisition runs
/// without the published bundle.
fn served_pins(address: &std::net::SocketAddr, file: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../runtime/runtimes.json");
    let mut pins: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    pins["ocr"] = json!({
        "version": "1",
        "checksums": format!("http://{address}/SUMS"),
        "sources": [format!("http://{address}/{{file}}")],
        "files": { platform(): file },
    });
    pins.to_string()
}

/// The repository's pins with this host's `ocr` entries removed: a platform
/// the component publishes no bundle for.
fn pins_without_this_platform() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../runtime/runtimes.json");
    let mut pins: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let platform = platform();
    pins["ocr"]["files"]
        .as_object_mut()
        .unwrap()
        .remove(&platform);
    pins["ocr"]["sha256"]
        .as_object_mut()
        .unwrap()
        .remove(&platform);
    pins.to_string()
}

/// A bundle archive as the script packs one: a single top-level directory
/// holding the manifest.
fn bundle(platform: &str) -> Vec<u8> {
    use std::io::Write;
    let mut builder = tar::Builder::new(Vec::new());
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Regular);
    header
        .set_path(format!("ocr-1-{platform}/manifest.json"))
        .unwrap();
    header.set_mode(0o644);
    header.set_size(2);
    header.set_cksum();
    builder.append(&header, &b"{}"[..]).unwrap();
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    gz.write_all(&builder.into_inner().unwrap()).unwrap();
    gz.finish().unwrap()
}

fn hex_digest(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Serves `bytes` as `file` and their digest at `/SUMS`; the returned counter
/// is the number of archive requests, so a second start can show it downloads
/// nothing.
async fn serve_component(
    sha256: String,
    file: String,
    bytes: Vec<u8>,
) -> (std::net::SocketAddr, Arc<AtomicUsize>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let requests = Arc::new(AtomicUsize::new(0));
    let counted = requests.clone();
    tokio::spawn(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let mut request = [0u8; 1024];
            let read = stream.read(&mut request).await.unwrap_or(0);
            let head = String::from_utf8_lossy(&request[..read]).into_owned();
            let body: Vec<u8> = if head.contains("/SUMS") {
                format!("{sha256}  {file}\n").into_bytes()
            } else {
                counted.fetch_add(1, Ordering::SeqCst);
                bytes.clone()
            };
            let _ = stream
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await;
            let _ = stream.write_all(&body).await;
        }
    });
    (address, requests)
}

/// What the component's owner answers: every directory it was asked to check,
/// whether the manifest was there, and the reason it refuses if it does.
#[derive(Default)]
struct StubVerifier {
    seen: StdMutex<Vec<(String, PathBuf, bool)>>,
    refuse: Option<String>,
}

impl ComponentVerifier for StubVerifier {
    fn verify<'a>(&'a self, name: &'a str, root: &'a Path) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            let manifest = root.join("manifest.json").is_file();
            self.seen
                .lock()
                .unwrap()
                .push((name.to_owned(), root.to_owned(), manifest));
            match &self.refuse {
                Some(reason) => Err(reason.clone()),
                None => Ok(()),
            }
        })
    }
}

/// The context the verifier row was applied with, for reading the ports the
/// App Server's own rows provide.
/// The ports the App Server's own rows provide, read while the verifier row is
/// applied: a later read through a stored context is outside that row's scope.
struct Ports {
    component: Arc<ComponentSourceHost>,
    toolchain: Arc<ToolchainSourceHost>,
}

type SharedPorts = Arc<StdMutex<Option<Ports>>>;

/// The row that provides the verifier this build's owner would provide, and
/// the handle the test reads the component and toolchain ports through. The
/// row declares both ports as dependencies, as their real consumers do, so it
/// applies after the rows that provide them.
struct VerifierRow {
    verifier: Arc<StubVerifier>,
    ports: SharedPorts,
    component: Key<ComponentSourceHost>,
    toolchain: Key<ToolchainSourceHost>,
}

impl VerifierRow {
    fn new(
        names: &dim_agent_runtime::Names<'_>,
        verifier: Arc<StubVerifier>,
        ports: SharedPorts,
    ) -> rutis::Result<Self> {
        Ok(Self {
            verifier,
            ports,
            component: names.key()?,
            toolchain: names.key()?,
        })
    }
}

impl DynPlugin for VerifierRow {
    fn name(&self) -> &str {
        "test.ocr-verifier"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.component.erase(), self.toolchain.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a rutis::Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            *self.ports.lock().unwrap() = Some(Ports {
                component: ctx
                    .get_at(&self.component)
                    .expect("the App Server provides the component source"),
                toolchain: ctx
                    .get_at(&self.toolchain)
                    .expect("the App Server provides the toolchain source"),
            });
            ctx.provide(ComponentVerifierHost(
                self.verifier.clone() as Arc<dyn ComponentVerifier>
            ))?;
            Ok(Effect::Done)
        })
    }
}

/// The verifier row as the `extra` an App Server test installs, after the two
/// rows that provide the ports (the App Server installs them itself; these
/// tests run a Session runtime configuration instead).
fn verifier_row(verifier: Arc<StubVerifier>) -> (impl FnOnce(&mut AgentBuilder), SharedPorts) {
    let ports: SharedPorts = Arc::new(StdMutex::new(None));
    let shared = ports.clone();
    (
        move |builder: &mut AgentBuilder| {
            builder.plugin(|names| {
                Ok(
                    Box::new(dim_app_server::runtime_manager::ToolchainSourcePlugin::new(
                        names,
                    )?) as Box<dyn DynPlugin>,
                )
            });
            builder.plugin(|names| {
                Ok(
                    Box::new(dim_app_server::runtime_manager::ComponentSourcePlugin::new(
                        names,
                    )?) as Box<dyn DynPlugin>,
                )
            });
            builder.plugin(move |names| {
                Ok(
                    Box::new(VerifierRow::new(names, verifier.clone(), shared.clone())?)
                        as Box<dyn DynPlugin>,
                )
            });
        },
        ports,
    )
}

fn component_source(ports: &SharedPorts) -> Arc<ComponentSourceHost> {
    ports
        .lock()
        .unwrap()
        .as_ref()
        .expect("the verifier row was applied")
        .component
        .clone()
}

fn toolchain_source(ports: &SharedPorts) -> Arc<ToolchainSourceHost> {
    ports
        .lock()
        .unwrap()
        .as_ref()
        .expect("the verifier row was applied")
        .toolchain
        .clone()
}

/// A home whose component pin is the loopback listener's, and the runtimes
/// loaded from it.
fn component_home(name: &str, pins: &str) -> (PathBuf, PathBuf, Arc<Runtimes>) {
    let home = std::env::temp_dir().join(format!("drm-{name}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&home).unwrap();
    let defaults = write_defaults(
        &home,
        r#"{"node": {"source": "download", "auto": false},
            "python": {"source": "download", "auto": false},
            "uv": {"source": "download", "auto": false},
            "ocr": {"source": "download", "auto": false}}"#,
    );
    let runtimes = Arc::new(
        Runtimes::load_with_pins(
            &home,
            &dim_app_server::plugin_manager::Defaults::read(&defaults)
                .unwrap()
                .runtimes,
            pins,
        )
        .unwrap(),
    );
    (home, defaults, runtimes)
}

/// The serviced bundle is downloaded, verified on its `.partial` directory and
/// only then activated: `runtime.list` reports the bundle root, a second start
/// reuses the installation without downloading, and another kind still
/// installs.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_ocr_component_is_downloaded_verified_and_activated() {
    let _path = PATH_LOCK.lock().await;
    let fixture = FixtureUv::new("0.13.7");
    let platform = platform();
    let file = format!("ocr-1-{platform}.tar.gz");
    let bytes = bundle(&platform);
    let sha256 = hex_digest(&bytes);
    let (address, requests) = serve_component(sha256.clone(), file.clone(), bytes).await;
    let pins = served_pins(&address, &file);
    let (home, defaults, runtimes) = component_home("ocr-activate", &pins);
    let verifier = Arc::new(StubVerifier::default());
    let (row, row_ctx) = verifier_row(verifier.clone());
    let source = row_ctx.clone();
    let (server, _) = launch_with(
        Arc::new(MockTools),
        ServerConfig {
            target_home: Some(home.clone()),
            session_typescript: Some(SessionTypeScriptConfig::acquired(&home, runtimes, vec![])),
            ..Default::default()
        },
        row,
    )
    .await;
    let mut client = Client::new_version(&server, false, 3).await;
    assert_eq!(
        component_source(&source).0.component("ocr"),
        Some(ComponentState::Off),
        "nothing is installed before the install"
    );
    assert_ok(&client.rpc("runtime.install", json!({"name": "ocr"})).await);
    let ready = status(&mut client, 3, "ready").await;
    let root = home.join("runtimes").join(format!("ocr-1-{platform}"));
    let partial = home
        .join("runtimes")
        .join(format!("ocr-1-{platform}.partial"));
    assert_eq!(ready["path"], root.to_str().unwrap(), "{ready}");
    assert_eq!(ready["version"], "1", "{ready}");
    assert_eq!(ready["source"], "download", "{ready}");
    assert_eq!(
        &*verifier.seen.lock().unwrap(),
        &[("ocr".to_owned(), partial.clone(), true)],
        "the verifier reads the unpacked directory, manifest and all"
    );
    assert!(
        !partial.exists(),
        "the activated bundle is the renamed partial"
    );
    assert!(root.join("manifest.json").is_file(), "{}", root.display());
    assert_eq!(
        component_source(&row_ctx).0.component("ocr"),
        Some(ComponentState::Ready {
            root: root.clone(),
            version: "1".to_owned(),
        })
    );
    let toolchains = toolchain_source(&row_ctx).0.toolchains();
    assert!(
        toolchains.iter().all(
            |toolchain| !toolchain.root.starts_with(&root) && !toolchain.bin.starts_with(&root)
        ),
        "a component is not on PATH: {toolchains:?}"
    );
    let records: Value =
        serde_json::from_slice(&std::fs::read(home.join("runtimes/runtimes.json")).unwrap())
            .unwrap();
    assert_eq!(records["ocr"]["sha256"], sha256, "{records}");
    assert!(records["ocr"].get("packages").is_none(), "{records}");
    assert_eq!(requests.load(Ordering::SeqCst), 1);

    // A second start finds the record and its bundle: the install is answered
    // from the home, with no download at all.
    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    let runtimes = Arc::new(
        Runtimes::load_with_pins(
            &home,
            &dim_app_server::plugin_manager::Defaults::read(&defaults)
                .unwrap()
                .runtimes,
            &pins,
        )
        .unwrap(),
    );
    let (row, row_ctx) = verifier_row(verifier.clone());
    let (server, _) = launch_with(
        Arc::new(MockTools),
        ServerConfig {
            target_home: Some(home.clone()),
            session_typescript: Some(SessionTypeScriptConfig::acquired(&home, runtimes, vec![])),
            ..Default::default()
        },
        row,
    )
    .await;
    let mut client = Client::new_version(&server, false, 3).await;
    assert_ok(&client.rpc("runtime.install", json!({"name": "ocr"})).await);
    status(&mut client, 3, "ready").await;
    assert_eq!(
        requests.load(Ordering::SeqCst),
        1,
        "an installed bundle is not downloaded again"
    );

    // The component did not take the other kinds' place.
    let configured = client
        .rpc(
            "runtime.configure",
            json!({"name": "uv", "source": "system"}),
        )
        .await;
    assert_ok(&configured);
    let ready = status(&mut client, 2, "ready").await;
    assert_eq!(
        ready["path"],
        fixture.directory.join("uv").to_str().unwrap(),
        "{ready}"
    );

    // A bundle whose manifest is gone is not installed: the port reads off and
    // the row drops its path.
    std::fs::remove_file(root.join("manifest.json")).unwrap();
    assert_eq!(
        component_source(&row_ctx).0.component("ocr"),
        Some(ComponentState::Off),
        "a bundle without its manifest is not installed"
    );
    let listed = client.rpc("runtime.list", json!({})).await;
    assert_ok(&listed);
    assert!(
        listed["result"][3]["path"].is_null(),
        "the row drops the path: {listed}"
    );

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// A verifier that refuses leaves the install failed with its reason, removes
/// the partial directory and writes no record: nothing unverified is activated.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_component_its_verifier_refuses_is_not_activated() {
    let platform = platform();
    let file = format!("ocr-1-{platform}.tar.gz");
    let bytes = bundle(&platform);
    let (address, requests) = serve_component(hex_digest(&bytes), file.clone(), bytes).await;
    let pins = served_pins(&address, &file);
    let (home, _, runtimes) = component_home("ocr-refused", &pins);
    let reason = "the bundle does not load: lib/libonnxruntime is missing";
    let verifier = Arc::new(StubVerifier {
        refuse: Some(reason.to_owned()),
        ..Default::default()
    });
    let (row, _) = verifier_row(verifier.clone());
    let (server, _) = launch_with(
        Arc::new(MockTools),
        ServerConfig {
            target_home: Some(home.clone()),
            session_typescript: Some(SessionTypeScriptConfig::acquired(&home, runtimes, vec![])),
            ..Default::default()
        },
        row,
    )
    .await;
    let mut client = Client::new_version(&server, false, 3).await;
    assert_ok(&client.rpc("runtime.install", json!({"name": "ocr"})).await);
    let failed = status(&mut client, 3, "failed").await;
    assert_eq!(failed["reason"], reason, "{failed}");
    let root = home.join("runtimes").join(format!("ocr-1-{platform}"));
    assert!(!root.exists(), "the refused bundle is not activated");
    assert!(
        !root
            .with_file_name(format!("ocr-1-{platform}.partial"))
            .exists()
    );
    let records: Value = std::fs::read(home.join("runtimes/runtimes.json"))
        .map(|bytes| serde_json::from_slice(&bytes).unwrap())
        .unwrap_or_else(|_| json!({}));
    assert!(records.get("ocr").is_none(), "{records}");
    assert_eq!(requests.load(Ordering::SeqCst), 1);

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// A build with no verifier refuses the component before downloading anything:
/// activating bytes nothing checked would defeat the check.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_build_without_a_verifier_refuses_the_component() {
    let platform = platform();
    let file = format!("ocr-1-{platform}.tar.gz");
    let bytes = bundle(&platform);
    let (address, requests) = serve_component(hex_digest(&bytes), file.clone(), bytes).await;
    let pins = served_pins(&address, &file);
    let (home, _, runtimes) = component_home("ocr-unverified", &pins);
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
    assert_ok(&client.rpc("runtime.install", json!({"name": "ocr"})).await);
    let failed = status(&mut client, 3, "failed").await;
    let reason = failed["reason"].as_str().unwrap_or_default();
    assert!(reason.contains("can verify the ocr component"), "{failed}");
    assert_eq!(requests.load(Ordering::SeqCst), 0, "nothing was downloaded");
    let root = home.join("runtimes").join(format!("ocr-1-{platform}"));
    assert!(!root.exists(), "nothing was activated");

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// A start does not repair a component: its record may name a bundle the user
/// deleted, and only `runtime.install` downloads it again.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_record_whose_bundle_is_gone_is_not_reinstalled_by_a_start() {
    let platform = platform();
    let file = format!("ocr-1-{platform}.tar.gz");
    let bytes = bundle(&platform);
    let (address, requests) = serve_component(hex_digest(&bytes), file.clone(), bytes).await;
    let pins = served_pins(&address, &file);
    let (home, defaults, runtimes) = component_home("ocr-gone", &pins);
    let root = home.join("runtimes").join(format!("ocr-1-{platform}"));
    let verifier = Arc::new(StubVerifier::default());
    let (row, _) = verifier_row(verifier.clone());
    let (server, _) = launch_with(
        Arc::new(MockTools),
        ServerConfig {
            target_home: Some(home.clone()),
            session_typescript: Some(SessionTypeScriptConfig::acquired(&home, runtimes, vec![])),
            ..Default::default()
        },
        row,
    )
    .await;
    let mut client = Client::new_version(&server, false, 3).await;
    assert_ok(&client.rpc("runtime.install", json!({"name": "ocr"})).await);
    status(&mut client, 3, "ready").await;
    assert_eq!(requests.load(Ordering::SeqCst), 1);
    drop(client);
    server.shutdown();
    server.join().await.unwrap();

    std::fs::remove_dir_all(&root).unwrap();
    let runtimes = Arc::new(
        Runtimes::load_with_pins(
            &home,
            &dim_app_server::plugin_manager::Defaults::read(&defaults)
                .unwrap()
                .runtimes,
            &pins,
        )
        .unwrap(),
    );
    let (row, _) = verifier_row(verifier.clone());
    let (server, _) = launch_with(
        Arc::new(MockTools),
        ServerConfig {
            target_home: Some(home.clone()),
            session_typescript: Some(SessionTypeScriptConfig::acquired(&home, runtimes, vec![])),
            ..Default::default()
        },
        row,
    )
    .await;
    let mut client = Client::new_version(&server, false, 3).await;
    let listed = client.rpc("runtime.list", json!({})).await;
    assert_ok(&listed);
    assert_eq!(listed["result"][3]["status"], "off", "{listed}");
    assert_eq!(
        requests.load(Ordering::SeqCst),
        1,
        "a start does not download a component"
    );

    assert_ok(&client.rpc("runtime.install", json!({"name": "ocr"})).await);
    status(&mut client, 3, "ready").await;
    assert_eq!(
        requests.load(Ordering::SeqCst),
        2,
        "the explicit install does"
    );

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// A platform the component publishes no bundle for still starts; the row
/// reads off with the reason `runtime.install` refuses with.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_platform_without_a_bundle_lists_the_reason_and_refuses_the_install() {
    let platform = platform();
    let pins = pins_without_this_platform();
    let (home, _, runtimes) = component_home("ocr-unoffered", &pins);
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
    let row = &listed["result"][3];
    assert_eq!(row["name"], "ocr", "{listed}");
    assert_eq!(row["status"], "off", "{listed}");
    assert_eq!(row["offered"], false, "{listed}");
    assert_eq!(
        row["reason"],
        format!("No ocr bundle is published for {platform}"),
        "{listed}"
    );
    assert_eq!(
        listed["result"][2]["offered"], true,
        "a program is always offered: {listed}"
    );
    let refused = client.rpc("runtime.install", json!({"name": "ocr"})).await;
    assert_eq!(
        refused["error"]["data"]["appError"], "runtime_configuration",
        "{refused}"
    );
    assert_eq!(
        refused["error"]["message"],
        format!("No ocr bundle is published for {platform}"),
        "{refused}"
    );

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// A component's `source: system` and `auto: true` are refused before the file
/// is touched, so the home keeps loading and another runtime still installs; a
/// hand-written file is read by the component's rules.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn configuring_a_component_to_system_or_auto_is_refused_before_the_write() {
    let _path = PATH_LOCK.lock().await;
    let fixture = FixtureUv::new("0.13.7");
    let home = std::env::temp_dir().join(format!("drm-ocr-configure-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(home.join("runtimes")).unwrap();
    let defaults = write_defaults(
        &home,
        r#"{"node": {"source": "system", "auto": false},
            "python": {"source": "system", "auto": false},
            "uv": {"source": "system", "auto": false},
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
    let config = home.join("runtimes/config.json");

    let refused = client
        .rpc(
            "runtime.configure",
            json!({"name": "ocr", "source": "system"}),
        )
        .await;
    assert_eq!(
        refused["error"]["data"]["appError"], "runtime_configuration",
        "{refused}"
    );
    assert!(
        refused["error"]["message"]
            .as_str()
            .unwrap()
            .contains("it is only ever downloaded"),
        "{refused}"
    );
    assert!(!config.exists(), "a refused configure writes nothing");

    let refused = client
        .rpc("runtime.configure", json!({"name": "ocr", "auto": true}))
        .await;
    assert_eq!(
        refused["error"]["data"]["appError"], "runtime_configuration",
        "{refused}"
    );
    assert!(
        refused["error"]["message"]
            .as_str()
            .unwrap()
            .contains("it is installed only by `runtime.install`"),
        "{refused}"
    );
    assert!(!config.exists(), "a refused configure writes nothing");

    // The refusals left the home as it was: another runtime still installs and
    // the component still reads off.
    assert_ok(&client.rpc("runtime.install", json!({"name": "uv"})).await);
    let ready = status(&mut client, 2, "ready").await;
    assert_eq!(
        ready["path"],
        fixture.directory.join("uv").to_str().unwrap(),
        "{ready}"
    );
    let listed = client.rpc("runtime.list", json!({})).await;
    assert_ok(&listed);
    assert_eq!(listed["result"][3]["status"], "off", "{listed}");
    assert_eq!(listed["result"][3]["auto"], false, "{listed}");

    drop(client);
    server.shutdown();
    server.join().await.unwrap();

    // A fresh load of the same home succeeds, so a restart works.
    let _reloaded = load_runtimes(&home, &defaults);

    // A hand-written file is read by the component's rules: `auto` is false
    // whatever it says, and `source: system` keeps failing the load.
    std::fs::write(&config, r#"{"ocr": {"auto": true}}"#).unwrap();
    let clamped = load_runtimes(&home, &defaults);
    assert!(!clamped.plan(Kind::Ocr).auto, "a component's auto is false");
    std::fs::write(&config, r#"{"ocr": {"source": "system"}}"#).unwrap();
    let choices = dim_app_server::plugin_manager::Defaults::read(&defaults)
        .unwrap()
        .runtimes;
    assert!(
        Runtimes::load(&home, &choices).is_err(),
        "a hand-written system source fails the load"
    );

    std::fs::remove_dir_all(home).unwrap();
}

/// The pinned bundle installs from the published release: the archive is
/// downloaded, its digest checked against the pin, unpacked, offered to a
/// verifier and only then activated. It downloads the archive, so it runs only
/// with the other ignored flows.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "downloads the OCR bundle"]
async fn the_pinned_ocr_bundle_installs() {
    let platform = platform();
    let home = std::env::temp_dir().join(format!("drm-ocr-download-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&home).unwrap();
    let defaults = write_defaults(
        &home,
        r#"{"node": {"source": "system", "auto": false},
            "python": {"source": "system", "auto": false},
            "uv": {"source": "system", "auto": false},
            "ocr": {"source": "download", "auto": false}}"#,
    );
    let runtimes = load_runtimes(&home, &defaults);
    let ocr = runtimes.plan(Kind::Ocr).clone();
    assert!(ocr.offered, "a bundle is published for {platform}");
    let root = home.join("runtimes").join(format!("ocr-1-{platform}"));
    let verifier = Arc::new(StubVerifier::default());
    let (row, _) = verifier_row(verifier.clone());
    let (server, _) = launch_with(
        Arc::new(MockTools),
        ServerConfig {
            target_home: Some(home.clone()),
            session_typescript: Some(SessionTypeScriptConfig::acquired(&home, runtimes, vec![])),
            ..Default::default()
        },
        row,
    )
    .await;
    let mut client = Client::new_version(&server, false, 3).await;
    assert_ok(&client.rpc("runtime.install", json!({"name": "ocr"})).await);
    let ready = status(&mut client, 3, "ready").await;
    assert_eq!(ready["path"], root.to_str().unwrap(), "{ready}");
    assert_eq!(ready["version"], "1", "{ready}");
    assert_eq!(
        &*verifier.seen.lock().unwrap(),
        &[(
            "ocr".to_owned(),
            home.join("runtimes")
                .join(format!("ocr-1-{platform}.partial")),
            true
        )],
        "the verifier read the unpacked directory before it was activated"
    );
    let manifest: Value =
        serde_json::from_slice(&std::fs::read(root.join("manifest.json")).unwrap()).unwrap();
    assert_eq!(manifest["bundle"], "pp-ocrv6-small", "{manifest}");
    assert_eq!(manifest["target"], platform, "{manifest}");
    for file in manifest["files"].as_array().unwrap() {
        let path = root.join(file["path"].as_str().unwrap());
        let size = std::fs::metadata(&path)
            .unwrap_or_else(|_| panic!("{} is in the bundle", path.display()))
            .len();
        assert_eq!(Some(size), file["bytes"].as_u64(), "{}", path.display());
    }
    let records: Value =
        serde_json::from_slice(&std::fs::read(home.join("runtimes/runtimes.json")).unwrap())
            .unwrap();
    assert!(
        records["ocr"]["sha256"]
            .as_str()
            .is_some_and(|hash| hash.len() == 64),
        "the record carries the verified digest: {records}"
    );

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

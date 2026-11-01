//! RuntimeManager (RuntimeManager §3): the runtimes arrive after startup and
//! the plugin rows waiting for them start by themselves. `system` sources
//! keep the interpreters off the network; the pinned packages are installed
//! for real (`npm ci`, `pip install --require-hashes`).
use super::*;
use dim_app_server::{runtime_manager::Runtimes, session_typescript::SessionTypeScriptConfig};
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
    assert!(!python_marker.exists());
    status(&mut client, 0, "ready").await;

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

    drop(client);
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

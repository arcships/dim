//! `dimcode hooks list|enable|disable`: the effective catalog and this
//! workspace's enablement over the public wire (Hooks design A.7).
use super::*;

fn hello(wire: &mut Wire) {
    let hello = wire.read("hello");
    wire.reply(&hello,json!({"instanceId":"server","serverVersion":env!("CARGO_PKG_VERSION"),"protocolVersion":3,"daemonProfile":{"kind":"product"},"features":["session.create","session.history"]}));
}

fn catalog() -> Value {
    json!({
        "userRevision": "user:1",
        "projectsRevision": "user:1",
        "disableAllHooks": false,
        "sources": [
            {"source": "user", "enabled": true, "state": "ok"},
            {"source": "project", "path": "/work/.dimcode/hooks.json", "enabled": false, "state": "ok"},
        ],
        "hooks": [
            {"source": "user", "event": "SessionStart", "matcher": null, "command": "welcome", "timeout": 600, "onFailure": "continue", "async": false, "state": "ok"},
            {"source": "project", "event": "UnknownEvent", "matcher": "read", "command": "guard", "timeout": 600, "onFailure": "continue", "async": false, "state": "unsupported"},
        ],
    })
}

#[test]
fn hooks_list_prints_the_catalog_and_its_source_states() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        hello(wire);
        let request = wire.read("hooks.catalog");
        assert!(request["params"]["cwd"].is_string());
        wire.reply(&request, catalog());
    });
    let output = home.command().args(["hooks", "list"]).output().unwrap();
    daemon.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "SessionStart\t*\tuser\tenabled\tok\t600s\tcontinue\tsync\twelcome\n\
         UnknownEvent\tread\tproject\tdisabled\tunsupported\t600s\tcontinue\tsync\tguard\n"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("project hooks (/work/.dimcode/hooks.json): disabled"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn hooks_list_prints_a_plugin_source_as_enabled() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        hello(wire);
        let request = wire.read("hooks.catalog");
        wire.reply(
            &request,
            json!({
                "userRevision": "user:1",
                "projectsRevision": "missing",
                "disableAllHooks": false,
                "sources": [
                    {"source": "user", "enabled": true, "state": "ok"},
                    {"source": "plugin:kit", "path": "/work/plugins/kit", "enabled": true, "state": "ok"},
                ],
                "hooks": [
                    {"source": "plugin:kit", "event": "Stop", "matcher": null, "command": "guard", "timeout": 600, "onFailure": "continue", "async": false, "state": "ok"},
                ],
            }),
        );
    });
    let output = home.command().args(["hooks", "list"]).output().unwrap();
    daemon.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "Stop\t*\tplugin:kit\tenabled\tok\t600s\tcontinue\tsync\tguard\n"
    );
}

#[test]
fn hooks_list_says_when_no_hook_is_configured() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        hello(wire);
        let request = wire.read("hooks.catalog");
        wire.reply(
            &request,
            json!({
                "userRevision": "missing",
                "projectsRevision": "missing",
                "disableAllHooks": false,
                "sources": [{"source": "user", "enabled": true, "state": "missing"}],
                "hooks": [],
            }),
        );
    });
    let output = home.command().args(["hooks", "list"]).output().unwrap();
    daemon.join().unwrap();
    assert!(output.status.success());
    assert!(output.stdout.is_empty());
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        "No hook is configured.\nuser hooks: missing\n"
    );
}

#[test]
fn hooks_enable_and_disable_send_the_workspace_and_the_revision() {
    let home = Home::new();
    let project = home.0.join("work");
    fs::create_dir(&project).unwrap();
    let workspace = fs::canonicalize(&project).unwrap();
    let expected = workspace.to_string_lossy().into_owned();
    for enabled in [true, false] {
        let workspace = expected.clone();
        let daemon = home.daemon(move |wire| {
            hello(wire);
            let request = wire.read("hooks.catalog");
            wire.reply(&request, catalog());
            let request = wire.read("hooks.set_enabled");
            assert_eq!(request["params"]["cwd"], workspace);
            assert_eq!(request["params"]["enabled"], enabled);
            assert_eq!(request["params"]["expectedRevision"], "user:1");
            wire.reply(&request, json!({"revision": "user:2"}));
        });
        let output = home
            .command()
            .arg("--cwd")
            .arg(&project)
            .args(["hooks", if enabled { "enable" } else { "disable" }])
            .output()
            .unwrap();
        daemon.join().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert!(
            stdout.starts_with(if enabled {
                "Enabled project hooks for "
            } else {
                "Disabled project hooks for "
            }),
            "{stdout}"
        );
        assert!(stdout.contains(&expected), "{stdout}");
        fs::remove_file(home.0.join("rust/daemon.sock")).unwrap();
    }
}

#[test]
#[ignore = "requires DIM_TEST_APP_SERVER pointing to the built production App Server"]
fn project_hooks_start_disabled_and_the_user_enables_them() {
    let binary = std::env::var("DIM_TEST_APP_SERVER").unwrap();
    let home = Home::new();
    let _daemon = OwnedDaemon(home.0.join("rust/daemon.sock"));
    let config = home.0.join("model.json");
    fs::write(
        &config,
        include_str!("../../../dim-agent-aimux/examples/offline.json"),
    )
    .unwrap();
    let project = home.0.join("work");
    fs::create_dir_all(project.join(".dimcode")).unwrap();
    fs::write(
        project.join(".dimcode/hooks.json"),
        json!({"hooks":{"Stop":[{"hooks":[{"type":"command","command":"printf 'seen' > hook-ran.txt"}]}]}})
            .to_string(),
    )
    .unwrap();
    let run = |args: &[&str]| {
        home.command()
            .arg("--server")
            .arg(&binary)
            .arg("--model-config")
            .arg(&config)
            .arg("--cwd")
            .arg(&project)
            .args(args)
            .output()
            .unwrap()
    };
    let list = || {
        let output = run(&["hooks", "list"]);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    };
    assert_eq!(
        list(),
        "Stop\t*\tproject\tdisabled\tok\t600s\tcontinue\tsync\tprintf 'seen' > hook-ran.txt\n"
    );
    assert!(String::from_utf8_lossy(&run(&["hooks", "list"]).stderr).contains("disabled"));
    let enabled = run(&["hooks", "enable"]);
    assert!(
        enabled.status.success(),
        "{}",
        String::from_utf8_lossy(&enabled.stderr)
    );
    assert!(String::from_utf8_lossy(&enabled.stdout).starts_with("Enabled project hooks for "));
    assert_eq!(
        list(),
        "Stop\t*\tproject\tenabled\tok\t600s\tcontinue\tsync\tprintf 'seen' > hook-ran.txt\n"
    );
    // A later edit takes effect without a restart.
    fs::write(
        project.join(".dimcode/hooks.json"),
        json!({"hooks":{"Stop":[{"hooks":[{"type":"command","command":"printf 'later' > hook-ran.txt"}]}]}})
            .to_string(),
    )
    .unwrap();
    assert_eq!(
        list(),
        "Stop\t*\tproject\tenabled\tok\t600s\tcontinue\tsync\tprintf 'later' > hook-ran.txt\n"
    );
    assert!(run(&["hooks", "disable"]).status.success());
    assert_eq!(
        list(),
        "Stop\t*\tproject\tdisabled\tok\t600s\tcontinue\tsync\tprintf 'later' > hook-ran.txt\n"
    );
    // A broken document is a state, and disable still works from it.
    fs::write(project.join(".dimcode/hooks.json"), "{broken").unwrap();
    let broken = run(&["hooks", "list"]);
    assert!(
        broken.status.success(),
        "{}",
        String::from_utf8_lossy(&broken.stderr)
    );
    assert!(
        String::from_utf8_lossy(&broken.stderr).contains("invalid"),
        "{}",
        String::from_utf8_lossy(&broken.stderr)
    );
    fs::write(
        project.join(".dimcode/hooks.json"),
        json!({"hooks":{"Stop":[{"hooks":[{"type":"command","command":"printf 'later' > hook-ran.txt"}]}]}})
            .to_string(),
    )
    .unwrap();
    assert!(run(&["daemon", "stop"]).status.success());
}

/// D-home: a `Stop` hook archives its Session with `dimcode export` from the
/// product home the daemon serves.
#[test]
fn a_stop_hook_exports_its_session_from_dimcode_home() {
    use dim_agent_capabilities::defaults;
    use dim_agent_runtime::{AgentBuilder, AgentConfig, SessionConfig};
    use dim_agent_types::InputMode;
    use dim_app_server::{ServerConfig, serve};

    let home = Home::new();
    let project = home.0.join("work");
    fs::create_dir(&project).unwrap();
    let exported = home.0.join("export.jsonl");
    let done = home.0.join("export.done");
    let command = format!(
        "{} export \"$DIM_SESSION_ID\" --output {} && touch {}",
        env!("CARGO_BIN_EXE_dimcode"),
        exported.display(),
        done.display()
    );
    fs::write(
        home.0.join("hooks.json"),
        json!({"hooks":{"Stop":[{"hooks":[{"type":"command","command":command}]}]}}).to_string(),
    )
    .unwrap();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let mut builder = AgentBuilder::new(AgentConfig::default());
        defaults::mock_hosts(&mut builder);
        defaults::install(&mut builder);
        dim_app_server::hooks_settings::install(&mut builder, Some(home.0.clone()));
        dim_agent_capabilities::hooks::install(&mut builder);
        let server = serve(
            builder,
            ServerConfig {
                product_home: Some(home.0.clone()),
                target_home: Some(home.0.join("rust")),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let session = server
            .agent()
            .unwrap()
            .create_session(SessionConfig {
                cwd: project.to_string_lossy().into_owned(),
                ..Default::default()
            })
            .await
            .unwrap();
        session
            .submit("go".into(), InputMode::Queue, "main")
            .await
            .unwrap();
        session.idle("main").await.unwrap();
        for _ in 0..500 {
            if done.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(done.exists(), "the Stop hook did not finish its export");
        let text = fs::read_to_string(&exported).unwrap_or_default();
        assert!(
            text.contains(session.id()),
            "the Stop hook did not export the Session: {text}"
        );
        assert!(text.contains("\"type\":\"turn\""), "{text}");
        assert!(text.contains("\"name\":\"end\""), "{text}");
    });
}

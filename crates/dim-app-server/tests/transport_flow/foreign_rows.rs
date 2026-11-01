//! Parts of other languages are placed without inference (plugin contract
//! P4, rutis usage §2, §3.5): a Python plugin, one row, registers a Session
//! and a branch part, and an unmodified Cordis 4 plugin a TypeScript row
//! registers as a branch part is mounted in each branch. Each reads its own
//! instance's services by plain names.
use super::branch_typescript::{launch_stored, marked};
use super::session_typescript::{
    ControlledGate, fixture, new_session, session_copies, setup, submit,
};
use super::*;
use dim_app_server::session_typescript::{PythonConfig, SessionTypeScriptConfig};
use std::path::PathBuf;

/// Waits until every copy of the rows named `name` is Active, `expected`
/// copies in all.
async fn active(server: &RunningServer, name: &str, expected: usize) {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let rows: Vec<_> = server
                .diagnostics()
                .plugins
                .into_iter()
                .filter(|row| row.name.ends_with(name))
                .collect();
            if rows.len() == expected
                && rows
                    .iter()
                    .all(|row| row.state == rutis::host::FiberState::Active)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{expected} active copies of {name}"));
}

fn home(prefix: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "{prefix}-{}",
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    ))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires DIM_TEST_NODE, DIM_TEST_JS_RUNTIME and DIM_TEST_PYTHON"]
async fn node_python_plugin_is_one_row_and_its_parts_mount_in_session_and_branch() {
    let mut parts = fixture();
    parts.python = Some(PythonConfig {
        interpreter: std::env::var_os("DIM_TEST_PYTHON")
            .expect("DIM_TEST_PYTHON")
            .into(),
        project: parts.anchor.parent().unwrap().join("fixtures/python"),
        packages: None,
    });
    parts.rows = vec![json!({
        "id": "python", "name": "py:python_plugin", "provides": {}, "config": {},
    })];
    let home = home("dm-py");
    let (server, agent) = launch_stored(ServerConfig {
        unix_path: Some(home.join("server.sock")),
        database_path: Some(home.join("sessions.sqlite")),
        session_typescript: Some(parts),
        ..Default::default()
    })
    .await;
    let mut client = Client::new_version(&server, false, 3).await;
    let mut sessions = Vec::new();
    for _ in 0..2 {
        let mut params = create(json!(["py_echo"]));
        params["permissionPolicy"]["allowedTools"] = json!(["py_echo"]);
        let created = client.rpc("session.create", params).await;
        assert_ok(&created);
        sessions.push(created["result"]["sessionId"].as_str().unwrap().to_owned());
        active(&server, "PythonSessionPart", sessions.len()).await;
        active(&server, "PythonBranchPart", sessions.len()).await;
    }
    for session in &sessions {
        submit(&mut client, session, r#"tool:py_echo {"text":"hello"}"#).await;
        let history = agent
            .get_session(session)
            .unwrap()
            .history("main", None)
            .await
            .unwrap();
        let content = history
            .iter()
            .find_map(|event| match &event.kind {
                EventKind::ToolResult(batch) => batch
                    .results
                    .first()
                    .map(|result| serde_json::to_value(&result.result.content).unwrap()),
                _ => None,
            })
            .expect("the py_echo result");
        let echoed: Value = serde_json::from_str(content.as_str().unwrap()).unwrap();
        assert_eq!(
            echoed,
            json!({"session":session, "callSession":session, "branch":"main", "text":"hello"})
        );
        let marker = marked(&agent, session, "main", "<py-marker").await;
        assert!(
            marker.contains(&format!(r#"session="{session}" branch="main""#)),
            "{marker}"
        );
        let seen = marker
            .split(r#"seen=""#)
            .nth(1)
            .and_then(|rest| rest.split('"').next())
            .unwrap();
        let input = seen.find("input/message").expect(seen);
        let start = seen.find("turn/start").expect(seen);
        let end = seen.find("turn/end").expect(seen);
        assert!(input < start && start < end, "{seen}");
    }
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires DIM_TEST_NODE and DIM_TEST_JS_RUNTIME"]
async fn node_cordis_plugin_registered_as_a_branch_part_gets_its_own_branch() {
    let mut parts = fixture();
    let root = parts.anchor.parent().unwrap().join("fixtures");
    parts.rows = vec![json!({
        "id": "cordis-branch", "name": root.join("cordis-branch-row.ts"),
        "provides": {},
        "config": {},
    })];
    let home = home("dm-cordis");
    let (server, agent) = launch_stored(ServerConfig {
        unix_path: Some(home.join("server.sock")),
        database_path: Some(home.join("sessions.sqlite")),
        session_typescript: Some(parts),
        ..Default::default()
    })
    .await;
    let mut client = Client::new_version(&server, false, 3).await;
    let mut sessions = Vec::new();
    for _ in 0..2 {
        let created = client.rpc("session.create", create(json!([]))).await;
        assert_ok(&created);
        let session = created["result"]["sessionId"].as_str().unwrap().to_owned();
        active(&server, "cordis-branch-fixture", sessions.len() + 1).await;
        submit(&mut client, &session, "first").await;
        sessions.push(session);
    }
    for session in &sessions {
        let marker = marked(&agent, session, "main", "<cordis-marker").await;
        assert_eq!(
            marker,
            format!(r#"<cordis-marker session="{session}" branch="main"/>"#)
        );
    }
    let parent = agent.get_session(&sessions[0]).unwrap();
    let child = parent
        .branch(
            "main".into(),
            parent.sequence("main").unwrap(),
            dim_agent_runtime::BranchOptions {
                prompt: "child branch input".into(),
                source: dim_agent_types::BranchSource::Host {
                    operation_id: "cordis-child".into(),
                },
            },
        )
        .await
        .unwrap();
    active(&server, "cordis-branch-fixture", 3).await;
    assert_ok(
        &client
            .rpc(
                "session.submit",
                json!({"sessionId":sessions[0],"branchId":child,"text":"child input"}),
            )
            .await,
    );
    let marker = marked(&agent, &sessions[0], &child, "<cordis-marker").await;
    assert_eq!(
        marker,
        format!(
            r#"<cordis-marker session="{}" branch="{child}"/>"#,
            sessions[0]
        )
    );
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(home).unwrap();
}

/// A server whose Sessions report their instance and the loader.
async fn controlled(
    parts: SessionTypeScriptConfig,
) -> (
    RunningServer,
    tokio::sync::mpsc::UnboundedReceiver<Vec<String>>,
    tokio::sync::mpsc::UnboundedReceiver<super::session_typescript::Control>,
) {
    let (mut builder, requests, _) = setup();
    let (send, controls) = tokio::sync::mpsc::unbounded_channel();
    builder.require_feature(ControlledGate::feature(send));
    let server = serve(
        builder,
        ServerConfig {
            unix_path: Some(home("dm-rows").join("server.sock")),
            session_typescript: Some(parts),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    (server, requests, controls)
}

/// A `py:` module that cannot be imported leaves its row unresolved; the
/// Session and its native tools are unaffected.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires DIM_TEST_NODE, DIM_TEST_JS_RUNTIME and DIM_TEST_PYTHON"]
async fn node_python_import_failure_is_local_to_its_row() {
    let mut parts = fixture();
    parts.python = Some(PythonConfig {
        interpreter: std::env::var_os("DIM_TEST_PYTHON")
            .expect("DIM_TEST_PYTHON")
            .into(),
        project: parts.anchor.parent().unwrap().join("fixtures/python"),
        packages: None,
    });
    parts.rows = vec![json!({
        "id": "python-missing", "name": "py:no_such_fixture_module",
        "provides": {}, "config": {},
    })];
    let (server, mut requests, mut controls) = controlled(parts).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let id = new_session(&mut client, json!(["echo"])).await;
    let (_, _, loader, _) = controls.recv().await.unwrap();
    let status = loader
        .entries()
        .into_iter()
        .find(|entry| entry.id == "python-missing")
        .expect("the row")
        .status;
    assert!(
        matches!(&status, rutis_loader::report::EntryStatus::Unresolved(error)
            if error.to_string().contains("no_such_fixture_module")),
        "{status:?}"
    );
    submit(&mut client, &id, "native continues").await;
    assert!(requests.recv().await.unwrap().contains(&"echo".into()));
    server.shutdown();
    server.join().await.unwrap();
}

/// One provider per name per instance (rutis usage §3.1): of two parts that
/// provide one Session name in a Session, one provides it and the other
/// fails with rutis's duplicate-provider error; each Session has its own.
/// One row lists the name in `instanceNames`, as a name is declared once.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires DIM_TEST_NODE and DIM_TEST_JS_RUNTIME"]
async fn node_a_second_provider_of_a_name_in_one_instance_fails() {
    let mut parts = fixture();
    let module = parts
        .anchor
        .parent()
        .unwrap()
        .join("fixtures/shared-provider.ts");
    parts.rows = vec![
        json!({
            "id": "first-provider", "name": module, "provides": {}, "config": {},
            "instanceNames": {"exampleShared": "session"},
        }),
        json!({"id": "second-provider", "name": module, "provides": {}, "config": {}}),
    ];
    let (server, _requests, mut controls) = controlled(parts).await;
    let mut client = Client::new_version(&server, false, 3).await;
    for _ in 0..2 {
        new_session(&mut client, json!(["echo"])).await;
        let (session_instance, ctx, _, _) = controls.recv().await.unwrap();
        let copies = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let copies = session_copies(&ctx, session_instance, "SharedProviderFixture");
                if copies.len() == 2 {
                    break copies;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the Session mounts both copies");
        let mut settled = Vec::new();
        for copy in copies {
            let mut state = copy.watch();
            settled.push(
                tokio::time::timeout(Duration::from_secs(10), async {
                    loop {
                        let snapshot = state.borrow().clone();
                        if matches!(
                            snapshot.state,
                            rutis::host::FiberState::Active | rutis::host::FiberState::Failed
                        ) {
                            break snapshot;
                        }
                        state.changed().await.unwrap();
                    }
                })
                .await
                .expect("the row settles"),
            );
        }
        let failed: Vec<_> = settled
            .iter()
            .filter(|snapshot| snapshot.state == rutis::host::FiberState::Failed)
            .collect();
        assert_eq!(failed.len(), 1, "{settled:?}");
        let error = failed[0].error.as_ref().unwrap().to_string();
        assert!(error.contains("exampleShared"), "{error}");
    }
    server.shutdown();
    server.join().await.unwrap();
}

/// A row that injects `rutis/loader` (rutis 0.19 upgrade M5) reads the rows'
/// status and reloads a row, but cannot add or remove one: Dim configures no
/// editable layer, so PluginManager stays the only writer of rows.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires DIM_TEST_NODE, DIM_TEST_JS_RUNTIME and DIM_TEST_PYTHON"]
async fn node_a_row_injecting_loader_reloads_and_reads_but_cannot_add_or_remove_rows() {
    let project = home("dm-loader");
    std::fs::create_dir_all(&project).unwrap();
    let (applied, outcome) = (project.join("applied"), project.join("outcome.json"));
    std::fs::write(
        project.join("target.py"),
        format!(
            "def apply(ctx, config):\n    with open({:?}, 'a') as applied:\n        applied.write('x')\n",
            applied.to_str().unwrap()
        ),
    )
    .unwrap();
    std::fs::write(
        project.join("loader_probe.py"),
        format!(
            r#"import asyncio
import json
import pathlib

inject = ["rutis/loader"]
tasks = []


async def script(loader):
    outcome = {{}}

    async def attempt(label, call):
        try:
            await call
            outcome[label] = "ok"
        except Exception:
            outcome[label] = "refused"

    status = await loader.status()
    outcome["rows"] = sorted(row["id"] for row in status["rows"])
    await attempt("reload", loader.reload("target"))
    await attempt("create", loader.create({{"id": "added", "name": "py:target"}}))
    await attempt("remove", loader.remove("target"))
    result = pathlib.Path({:?})
    temporary = result.with_suffix(".tmp")
    temporary.write_text(json.dumps(outcome))
    temporary.replace(result)


def apply(ctx, config):
    tasks.append(asyncio.get_running_loop().create_task(script(ctx.use("rutis/loader"))))
"#,
            outcome.to_str().unwrap()
        ),
    )
    .unwrap();
    let mut parts = fixture();
    parts.python = Some(PythonConfig {
        interpreter: std::env::var_os("DIM_TEST_PYTHON")
            .expect("DIM_TEST_PYTHON")
            .into(),
        project: project.clone(),
        packages: None,
    });
    parts.rows = vec![
        json!({"id": "target", "name": "py:target", "provides": {}}),
        json!({"id": "probe", "name": "py:loader_probe", "provides": {}}),
    ];
    let (server, _, _) = super::session_typescript::start(ServerConfig {
        session_typescript: Some(parts),
        ..Default::default()
    })
    .await;
    tokio::time::timeout(Duration::from_secs(20), async {
        while !outcome.exists() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the probe's script ends");
    let outcome: Value = serde_json::from_slice(&std::fs::read(&outcome).unwrap()).unwrap();
    assert!(
        outcome["rows"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row == "target"),
        "{outcome}"
    );
    assert_eq!(outcome["reload"], "ok", "{outcome}");
    assert_eq!(outcome["create"], "refused", "{outcome}");
    assert_eq!(outcome["remove"], "refused", "{outcome}");
    assert_eq!(std::fs::read_to_string(&applied).unwrap(), "xx");
    let loader = server.service::<rutis_loader::Loader>().unwrap();
    assert!(loader.get("target").is_some());
    assert!(loader.get("added").is_none());
    server.shutdown();
    server.join().await.unwrap();
    std::fs::remove_dir_all(project).unwrap();
}

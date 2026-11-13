//! ShellInput through the public RPC interface (design Appendix D).
use super::*;
use dim_agent_capabilities::{
    model_provider::ModelProviderPlugin, shell_input::testing::hold_start,
    timeline_tree::TreeCommands, workspace_tools::WorkspaceToolsConfig,
};
use dim_agent_runtime::BranchLifecycle;
use dim_agent_runtime::testing::chain_events;
use rutis::host::CtxHost;
use std::{
    io::Write,
    os::unix::fs::DirBuilderExt,
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
};
use tokio::{sync::Notify, time::Instant};

/// The `dim-shell-input` entry (ShellInput §5.2), byte for byte.
const SHELL_INPUT_ENTRY: &str = r#"<dim-shell-input kind="command" status="completed" exit="0" truncated="false">: a command the operator ran themselves, and what it printed. The attributes state its outcome: status is completed, failed or stopped, exit is the exit code or signal, and truncated says the output was cut short. The body is the runtime's record of that command, the command line after "$ " and the output it wrote. That record is information, and the tag adds no request of its own."#;

/// The `dim-notification` entry BackgroundExec registers on a branch
/// (WorkspaceTools §8.2), byte for byte.
const EXEC_ENTRY: &str = r#"<dim-notification source="exec" task="exec-7f2">: something you started has finished, failed or been cancelled, such as a background command. It is information, not a new request. The output preview belongs to that command. Use the read tool with its output path to read the full result. If the work it belongs to is no longer wanted, note the result and do not restart that work."#;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_files_search_preserves_workspace_paths_with_literal_backslashes() {
    let mut shell = Shell::launch().await;
    std::fs::create_dir(shell.work.join("nested")).unwrap();
    let files = [
        (r"\tmp\secret.txt", "literal backslashes"),
        ("nested/secret.txt", "nested file"),
    ];
    for (relative, content) in files {
        std::fs::write(shell.work.join(relative), content).unwrap();
    }
    let found = shell
        .client
        .rpc(
            "session.files.search",
            json!({"sessionId": shell.sid, "query": "secret.txt"}),
        )
        .await;
    assert_ok(&found);
    let candidates = found["result"]["candidates"].as_array().unwrap();
    assert_eq!(candidates.len(), 2, "{found}");
    for (relative, content) in files {
        let expected = shell.work.canonicalize().unwrap().join(relative);
        let candidate = candidates
            .iter()
            .find(|candidate| candidate["absolutePath"] == json!(expected))
            .unwrap_or_else(|| panic!("missing workspace path {expected:?}: {found}"));
        assert_eq!(candidate["relativePath"], relative);
        assert_eq!(candidate["dir"], false);
        assert_eq!(
            std::fs::read_to_string(candidate["absolutePath"].as_str().unwrap()).unwrap(),
            content
        );
    }
    shell.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn background_exec_output_is_live_bounded_and_scoped() {
    let mut shell = Shell::launch_with(json!({
        "tools":["exec"], "permissionPolicy":{"preset":"full-access"}
    }))
    .await;
    let session = shell
        .server
        .agent()
        .unwrap()
        .get_session(&shell.sid)
        .unwrap();
    assert_ok(
        &shell
            .client
            .rpc("session.attach", json!({"sessionId":shell.sid}))
            .await,
    );
    let command = "while [ ! -f long-line ]; do sleep 0.01; done; awk 'BEGIN { for (i=0; i<150000; i++) printf \"x\"; printf \"\\n\" }'; while [ ! -f produce ]; do sleep 0.01; done; awk 'BEGIN { for (i=0; i<3000; i++) printf \"%04d-%080d\\n\", i, 0 }'; printf 'tail\\001\\n'; while [ ! -f finish ]; do sleep 0.01; done; printf 'final\\n'";
    let text = format!(
        "tool:exec {}",
        json!({"command":command,"runInBackground":true,"timeoutMs":-1})
    );
    assert_ok(
        &shell
            .client
            .rpc(
                "session.submit",
                json!({"sessionId":shell.sid,"branchId":"main","text":text}),
            )
            .await,
    );
    let task = loop {
        let event = shell.client.notice("session.event").await;
        if event["params"]["event"]["type"] == "exec"
            && event["params"]["event"]["name"] == "started"
        {
            break event["params"]["event"]["data"]["task_id"].clone();
        }
    };
    session.idle("main").await.unwrap();
    assert_ok(
        &shell
            .client
            .rpc("session.detach", json!({"sessionId":shell.sid}))
            .await,
    );
    let refused = shell
        .client
        .rpc("daemon.shutdown", json!({"ifIdle":true}))
        .await;
    assert_eq!(
        refused["error"]["data"]["appError"], "daemon_busy",
        "detached execution must survive an idle shutdown: {refused}"
    );
    assert_eq!(refused["error"]["data"]["running"], 1, "{refused}");
    assert_eq!(refused["error"]["data"]["connections"], 0, "{refused}");
    assert_eq!(refused["error"]["data"]["attachedSessions"], 0, "{refused}");
    assert_ok(
        &shell
            .client
            .rpc("session.attach", json!({"sessionId":shell.sid}))
            .await,
    );
    let params = json!({"sessionId":shell.sid,"branchId":"main","taskId":task});
    let child = session
        .branch(
            "main".into(),
            session.sequence("main").unwrap(),
            dim_agent_runtime::BranchOptions {
                prompt: "child".into(),
                source: dim_agent_types::BranchSource::Host {
                    operation_id: "output-scope".into(),
                },
            },
        )
        .await
        .unwrap();
    session.idle(&child).await.unwrap();
    let other = shell
        .client
        .rpc("session.create", json!({"cwd":shell.work,"tools":[]}))
        .await;
    assert_ok(&other);
    let foreign_branch = shell
        .client
        .rpc(
            "session.exec.output",
            json!({"sessionId":other["result"]["sessionId"],"branchId":child,"taskId":task}),
        )
        .await;
    assert_eq!(
        foreign_branch["error"]["data"]["appError"],
        "session_invalid_input"
    );
    let empty_session = shell
        .client
        .rpc(
            "session.exec.output",
            json!({"sessionId":"","branchId":"main","taskId":task}),
        )
        .await;
    assert_eq!(empty_session["error"]["data"]["appError"], "invalid_params");
    assert_eq!(
        empty_session["error"]["message"], "sessionId must not be empty",
        "{empty_session}"
    );
    for wrong in [
        json!({"sessionId":shell.sid,"branchId":child,"taskId":task}),
        json!({"sessionId":other["result"]["sessionId"],"branchId":"main","taskId":task}),
        json!({"sessionId":shell.sid,"branchId":"main","taskId":"missing"}),
    ] {
        let error = shell.client.rpc("session.exec.output", wrong).await;
        assert_eq!(
            error["error"]["data"]["appError"], "exec_task_unknown",
            "{error}"
        );
        assert_eq!(
            error["error"]["message"],
            "No live output is available for this task"
        );
    }
    for invalid in [
        json!({"sessionId":shell.sid,"branchId":"main","taskId":task,"path":"/etc/passwd"}),
        json!({"sessionId":shell.sid,"branchId":"main","taskId":""}),
        json!({"sessionId":shell.sid,"branchId":"","taskId":task}),
        json!({"sessionId":shell.sid,"taskId":task}),
    ] {
        let error = shell.client.rpc("session.exec.output", invalid).await;
        assert_eq!(
            error["error"]["data"]["appError"], "invalid_params",
            "{error}"
        );
    }
    std::fs::write(shell.work.join("long-line"), b"go").unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let reply = shell
                .client
                .rpc("session.exec.output", params.clone())
                .await;
            assert_ok(&reply);
            let text = reply["result"]["text"].as_str().unwrap();
            assert!(text.len() <= 50 * 1024);
            if text.ends_with("xxxxxxxx\n") {
                assert_eq!(text.len(), 50 * 1024);
                assert!(text.starts_with('x'));
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    std::fs::write(shell.work.join("produce"), b"go").unwrap();
    let preview = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let reply = shell
                .client
                .rpc("session.exec.output", params.clone())
                .await;
            assert_ok(&reply);
            let text = reply["result"]["text"].as_str().unwrap();
            if text.ends_with("tail\n") {
                break text.to_owned();
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(preview.len() <= 50 * 1024);
    assert!(preview.split('\n').count() <= 2000);
    assert!(preview.contains("2999-"));
    assert!(!preview.contains("0000-"));
    assert!(!preview.contains('\u{1}'));
    let sequence = session.sequence("main").unwrap();
    assert_ok(
        &shell
            .client
            .rpc("session.exec.output", params.clone())
            .await,
    );
    assert_eq!(session.sequence("main").unwrap(), sequence);

    let database = rusqlite::Connection::open(shell.home.join("rust/dimcode.sqlite")).unwrap();
    database.execute_batch("BEGIN IMMEDIATE").unwrap();
    std::fs::write(shell.work.join("finish"), b"go").unwrap();
    let publication = session.publication("main").unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while !publication.has_pending() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let during_commit = tokio::time::timeout(
        Duration::from_secs(1),
        shell.client.rpc("session.exec.output", params.clone()),
    )
    .await;
    database.execute_batch("COMMIT").unwrap();
    let during_commit = during_commit.expect("output read must not wait for terminal commit");
    assert_ok(&during_commit);
    assert!(
        during_commit["result"]["text"]
            .as_str()
            .unwrap()
            .ends_with("tail\nfinal\n")
    );
    let terminal = loop {
        let event = shell.client.notice("session.event").await;
        if event["params"]["event"]["type"] == "exec" && event["params"]["event"]["name"] == "ended"
        {
            break event["params"]["event"]["data"].clone();
        }
    };
    assert_eq!(terminal["task_id"], task);
    assert_eq!(terminal["status"], "completed");
    session.idle("main").await.unwrap();
    // The wake input's request carries the dim-notification entry that the
    // branch part writing the marker registered on this branch.
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let history = chain_events(&session, "main", ..).await.unwrap();
            if history.iter().any(|event| {
                matches!(&event.kind, EventKind::ModelCall(data)
                    if data.request.messages.iter().any(|message| matches!(
                        &message.content,
                        dim_agent_types::MessageContent::Text(text)
                            if text.contains("<dim-notification source=\"exec\""))))
            }) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the wake input reaches a request");
    let system = super::reload::last_system(&session, "main").await;
    assert!(system.contains(EXEC_ENTRY), "{system}");
    let stopped = shell.client.rpc("session.exec.stop", params.clone()).await;
    assert_ok(&stopped);
    assert_eq!(stopped["result"], terminal);
    assert_eq!(
        shell
            .client
            .rpc("session.exec.output", params.clone())
            .await["error"]["data"]["appError"],
        "exec_task_unknown"
    );
    assert!(shell.unload().await, "reads must not leave Session holds");
    assert_eq!(
        shell
            .client
            .rpc("session.exec.output", params.clone())
            .await["error"]["data"]["appError"],
        "exec_task_unknown"
    );
    assert_ok(
        &shell
            .client
            .rpc("session.close", json!({"sessionId":shell.sid}))
            .await,
    );
    assert_eq!(
        shell.client.rpc("session.exec.output", params).await["error"]["data"]["appError"],
        "session_closed"
    );
    let mut builder = AgentBuilder::new(AgentConfig::default());
    defaults::install(&mut builder);
    defaults::mock_model_hosts(&mut builder);
    defaults::tool_host(&mut builder, Arc::new(MockTools));
    let server = serve(
        builder,
        ServerConfig {
            unix_path: Some(shell.home.join("absent.sock")),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let mut client = Client::new_version(&server, false, 3).await;
    let created = client.rpc("session.create", create(json!([]))).await;
    assert_ok(&created);
    assert_eq!(
        client
            .rpc(
                "session.exec.output",
                json!({"sessionId":created["result"]["sessionId"],"branchId":"main","taskId":task})
            )
            .await["error"]["data"]["appError"],
        "unknown_method"
    );
    server.shutdown();
    server.join().await.unwrap();
    shell.shutdown().await;
}

/// One `session.exec.output.page` call with an explicit byte cursor and limit.
async fn exec_output_page(shell: &mut Shell, params: &Value, cursor: u64, limit: u64) -> Value {
    let mut params = params.clone();
    let object = params.as_object_mut().unwrap();
    object.insert("cursor".into(), json!(cursor));
    object.insert("limit".into(), json!(limit));
    shell.client.rpc("session.exec.output.page", params).await
}

/// The task id of the next `exec/started` notice, skipping other events.
async fn next_exec_started(shell: &mut Shell, after: u64) -> Value {
    loop {
        let event = shell.client.notice("session.event").await;
        if event["params"]["event"]["sequence"].as_u64().unwrap() > after
            && event["params"]["event"]["type"] == "exec"
            && event["params"]["event"]["name"] == "started"
        {
            return event["params"]["event"]["data"]["task_id"].clone();
        }
    }
}

/// `session.exec.output.page` beside the tail: a running task's growing file
/// pages forward by `cursor + bytes`, including a short page that ends inside
/// a character the writer has not completed; an ended task's file pages after
/// `exec/ended` with the preview's sanitization and no events or Session
/// holds; and a task rebuilt after an idle unload pages from the retained
/// file with its terminal still `exec/ended`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn background_exec_output_pages_running_finished_and_restored_tasks() {
    let mut shell = Shell::launch_with(json!({
        "tools":["exec"], "permissionPolicy":{"preset":"full-access"}
    }))
    .await;
    let session = shell
        .server
        .agent()
        .unwrap()
        .get_session(&shell.sid)
        .unwrap();
    assert_ok(
        &shell
            .client
            .rpc("session.attach", json!({"sessionId":shell.sid}))
            .await,
    );
    let command = "while [ ! -f go1 ]; do sleep 0.01; done; printf 'alpha\\n'; while [ ! -f go2 ]; do sleep 0.01; done; printf 'beta\\001\\n'; while [ ! -f go3 ]; do sleep 0.01; done; printf 'gamma\\n'; while [ ! -f finish ]; do sleep 0.01; done; exit 0";
    let text = format!(
        "tool:exec {}",
        json!({"command":command,"runInBackground":true,"timeoutMs":-1})
    );
    assert_ok(
        &shell
            .client
            .rpc(
                "session.submit",
                json!({"sessionId":shell.sid,"branchId":"main","text":text}),
            )
            .await,
    );
    let task = next_exec_started(&mut shell, 0).await;
    session.idle("main").await.unwrap();
    let params = json!({"sessionId":shell.sid,"branchId":"main","taskId":task});
    // Before any output the registered file is empty: an empty page, no next
    // cursor.
    let empty = exec_output_page(&mut shell, &params, 0, 64).await;
    assert_ok(&empty);
    assert_eq!(
        empty["result"],
        json!({"text":"","bytes":0,"truncated":false})
    );
    std::fs::write(shell.work.join("go1"), b"go").unwrap();
    let first = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let reply = exec_output_page(&mut shell, &params, 0, 64).await;
            assert_ok(&reply);
            if reply["result"]["text"] == json!("alpha\n") {
                break reply;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        first["result"],
        json!({"text":"alpha\n","bytes":6,"truncated":false})
    );
    // A cursor already handed out rereads the same bytes.
    let reread = exec_output_page(&mut shell, &params, 0, 64).await;
    assert_ok(&reread);
    assert_eq!(reread["result"], first["result"]);
    // A page can meet the file ending inside a character, which a read sees
    // when it catches a write in progress; completed writes end on a
    // boundary, so write the first byte of a two-byte character directly to
    // hold that state while the task runs. The page serves the complete
    // characters before it, and the walk advances by `cursor + bytes`: a page
    // advanced by anything else misses the character once the rest arrives.
    let path = shell
        .home
        .join("rust/outputs")
        .join(&shell.sid)
        .join(format!("{}.log", task.as_str().unwrap()));
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    file.write_all(&[0xc3]).unwrap();
    drop(file);
    let partial = exec_output_page(&mut shell, &params, 0, 64).await;
    assert_ok(&partial);
    assert_eq!(
        partial["result"],
        json!({"text":"alpha\n","bytes":6,"truncated":false})
    );
    let next = partial["result"]["bytes"].as_u64().unwrap();
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    file.write_all(&[0xa9]).unwrap();
    drop(file);
    let completed = exec_output_page(&mut shell, &params, next, 64).await;
    assert_ok(&completed);
    assert_eq!(
        completed["result"],
        json!({"text":"é","bytes":2,"truncated":false})
    );
    std::fs::write(shell.work.join("go2"), b"go").unwrap();
    // A short page carries no next cursor; the walk advances by `cursor +
    // bytes` (0 + 6) and the same offset yields the new bytes once the file
    // grows.
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let reply = exec_output_page(&mut shell, &params, 6, 64).await;
            assert_ok(&reply);
            if reply["result"]["text"] == json!("beta\n") {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let beta = exec_output_page(&mut shell, &params, 6, 64).await;
    assert_ok(&beta);
    assert_eq!(
        beta["result"],
        json!({"text":"beta\n","bytes":6,"truncated":false})
    );
    std::fs::write(shell.work.join("go3"), b"go").unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let reply = exec_output_page(&mut shell, &params, 6, 64).await;
            assert_ok(&reply);
            if reply["result"]["text"] == json!("beta\ngamma\n") {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    std::fs::write(shell.work.join("finish"), b"go").unwrap();
    let end = loop {
        let event = shell.client.notice("session.event").await;
        if event["params"]["event"]["type"] == "exec" && event["params"]["event"]["name"] == "ended"
        {
            break event["params"]["event"]["data"].clone();
        }
    };
    assert_eq!(end["task_id"], task);
    assert_eq!(end["status"], "completed");
    session.idle("main").await.unwrap();
    // After `exec/ended` the file pages the same way; the read appends no
    // event.
    let sequence = session.sequence("main").unwrap();
    let finished = exec_output_page(&mut shell, &params, 0, 64).await;
    assert_ok(&finished);
    assert_eq!(
        finished["result"],
        json!({"text":"alpha\nbeta\ngamma\n","bytes":18,"truncated":false})
    );
    // Concatenated pages equal the file, minus the control character the
    // preview rule strips.
    let file = std::fs::read(
        shell
            .home
            .join("rust/outputs")
            .join(&shell.sid)
            .join(format!("{}.log", task.as_str().unwrap())),
    )
    .unwrap();
    let file = String::from_utf8(file).unwrap();
    let (mut text, mut cursor) = (String::new(), 0u64);
    loop {
        let reply = exec_output_page(&mut shell, &params, cursor, 5).await;
        assert_ok(&reply);
        let result = &reply["result"];
        assert!(result["text"].as_str().unwrap().len() <= 5);
        let bytes = result["bytes"].as_u64().unwrap();
        assert!(bytes <= 5);
        text.push_str(result["text"].as_str().unwrap());
        cursor += bytes;
        if bytes == 0 {
            break;
        }
    }
    assert_eq!(text, file.replace('\u{1}', ""));
    assert!(!text.contains('\u{1}'));
    assert_eq!(session.sequence("main").unwrap(), sequence);
    // The reads leave no hold behind: the ended Session unloads idle, and a
    // task rebuilt after the restore pages from the retained file.
    assert!(shell.unload().await);
    let restored = exec_output_page(&mut shell, &params, 0, 64).await;
    assert_ok(&restored);
    assert_eq!(
        restored["result"],
        json!({"text":"alpha\nbeta\ngamma\n","bytes":18,"truncated":false})
    );
    let stopped = shell.client.rpc("session.exec.stop", params.clone()).await;
    assert_ok(&stopped);
    assert_eq!(stopped["result"]["status"], "completed");
    assert_eq!(
        shell
            .client
            .rpc("session.exec.output", params.clone())
            .await["error"]["data"]["appError"],
        "exec_task_unknown"
    );
    shell.shutdown().await;
}

/// `session.exec.output.page` bounds and errors: the limit rejects 0 and
/// clamps at the maximum, a page is exactly the requested byte range modulo
/// a character boundary (a split character appears once, whole, in the next
/// page), the tail's scoping holds for unknown task/branch/session, and a
/// removed registered file answers the page method's own error.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn background_exec_output_page_clamps_scopes_and_errors() {
    let mut shell = Shell::launch_with(json!({
        "tools":["exec"], "permissionPolicy":{"preset":"full-access"}
    }))
    .await;
    let session = shell
        .server
        .agent()
        .unwrap()
        .get_session(&shell.sid)
        .unwrap();
    assert_ok(
        &shell
            .client
            .rpc("session.attach", json!({"sessionId":shell.sid}))
            .await,
    );
    // `012345678éX\n`: the two-byte char sits one byte before a page of 10.
    let command =
        "while [ ! -f go ]; do sleep 0.01; done; printf '012345678\\303\\251X\\n'; exit 0";
    let text = format!(
        "tool:exec {}",
        json!({"command":command,"runInBackground":true,"timeoutMs":-1})
    );
    assert_ok(
        &shell
            .client
            .rpc(
                "session.submit",
                json!({"sessionId":shell.sid,"branchId":"main","text":text}),
            )
            .await,
    );
    let task = next_exec_started(&mut shell, 0).await;
    session.idle("main").await.unwrap();
    let params = json!({"sessionId":shell.sid,"branchId":"main","taskId":task});
    std::fs::write(shell.work.join("go"), b"go").unwrap();
    let status = loop {
        let event = shell.client.notice("session.event").await;
        if event["params"]["event"]["type"] == "exec" && event["params"]["event"]["name"] == "ended"
        {
            break event["params"]["event"]["data"]["status"].clone();
        }
    };
    assert_eq!(status, "completed");
    session.idle("main").await.unwrap();
    // The page of 10 ends at the char start; the next page serves the split
    // character whole, once.
    let first = exec_output_page(&mut shell, &params, 0, 10).await;
    assert_ok(&first);
    assert_eq!(
        first["result"],
        json!({"text":"012345678","bytes":9,"nextCursor":9,"truncated":false})
    );
    let second = exec_output_page(&mut shell, &params, 9, 10).await;
    assert_ok(&second);
    assert_eq!(
        second["result"],
        json!({"text":"éX\n","bytes":4,"truncated":false})
    );
    assert!(!first["result"]["text"].as_str().unwrap().contains('é'));
    assert!(second["result"]["text"].as_str().unwrap().starts_with('é'));
    // A page at least as long as the file ends the walk.
    let whole = exec_output_page(&mut shell, &params, 0, 14).await;
    assert_ok(&whole);
    assert_eq!(
        whole["result"],
        json!({"text":"012345678éX\n","bytes":13,"truncated":false})
    );
    // A budget that cuts the very first character serves it whole: the page
    // still consumes it and the cursor moves, even at `limit: 1`.
    let cut = exec_output_page(&mut shell, &params, 9, 1).await;
    assert_ok(&cut);
    assert_eq!(
        cut["result"],
        json!({"text":"é","bytes":2,"nextCursor":11,"truncated":false})
    );
    // Walking the whole file at `limit: 1` by `cursor + bytes` terminates and
    // reassembles it.
    let (mut text, mut cursor) = (String::new(), 9u64);
    loop {
        let reply = exec_output_page(&mut shell, &params, cursor, 1).await;
        assert_ok(&reply);
        let result = &reply["result"];
        text.push_str(result["text"].as_str().unwrap());
        let bytes = result["bytes"].as_u64().unwrap();
        cursor += bytes;
        if bytes == 0 {
            break;
        }
    }
    assert_eq!(text, "éX\n");
    // A cursor inside a character, which the method never hands out, cannot
    // be completed by any read: the page is empty and carries no
    // `nextCursor`.
    let inside = exec_output_page(&mut shell, &params, 10, 1).await;
    assert_ok(&inside);
    assert_eq!(
        inside["result"],
        json!({"text":"","bytes":0,"truncated":false})
    );
    for invalid in [
        json!({"sessionId":shell.sid,"branchId":"main","taskId":task,"limit":0}),
        json!({"sessionId":shell.sid,"branchId":"main","taskId":task,"limit":-1}),
        json!({"sessionId":shell.sid,"branchId":"main","taskId":task,"cursor":-1}),
        json!({"sessionId":shell.sid,"branchId":"main","taskId":task,"cursor":1.5}),
        json!({"sessionId":shell.sid,"branchId":"main","taskId":task,"offset":1}),
        json!({"sessionId":"","branchId":"main","taskId":task}),
    ] {
        let reply = shell.client.rpc("session.exec.output.page", invalid).await;
        assert_eq!(
            reply["error"]["data"]["appError"], "invalid_params",
            "{reply}"
        );
    }
    let child = session
        .branch(
            "main".into(),
            session.sequence("main").unwrap(),
            dim_agent_runtime::BranchOptions {
                prompt: "child".into(),
                source: dim_agent_types::BranchSource::Host {
                    operation_id: "page-scope".into(),
                },
            },
        )
        .await
        .unwrap();
    session.idle(&child).await.unwrap();
    let other = shell
        .client
        .rpc("session.create", json!({"cwd":shell.work,"tools":[]}))
        .await;
    assert_ok(&other);
    for wrong in [
        json!({"sessionId":shell.sid,"branchId":child,"taskId":task}),
        json!({"sessionId":other["result"]["sessionId"],"branchId":"main","taskId":task}),
        json!({"sessionId":shell.sid,"branchId":"main","taskId":"missing"}),
    ] {
        let error = shell.client.rpc("session.exec.output.page", wrong).await;
        assert_eq!(
            error["error"]["data"]["appError"], "exec_task_unknown",
            "{error}"
        );
        assert_eq!(
            error["error"]["message"], "No such task is known on this branch",
            "{error}"
        );
    }
    // A second task writes past the maximum page: the request clamps.
    let command = "while [ ! -f big ]; do sleep 0.01; done; awk 'BEGIN { for (i=0; i<5500000; i++) printf \"x\" }'; exit 0";
    let text = format!(
        "tool:exec {}",
        json!({"command":command,"runInBackground":true,"timeoutMs":-1})
    );
    assert_ok(
        &shell
            .client
            .rpc(
                "session.submit",
                json!({"sessionId":shell.sid,"branchId":"main","text":text}),
            )
            .await,
    );
    let big = next_exec_started(&mut shell, 0).await;
    session.idle("main").await.unwrap();
    std::fs::write(shell.work.join("big"), b"go").unwrap();
    let status = loop {
        let event = shell.client.notice("session.event").await;
        if event["params"]["event"]["type"] == "exec"
            && event["params"]["event"]["name"] == "ended"
            && event["params"]["event"]["data"]["task_id"] == big
        {
            break event["params"]["event"]["data"]["status"].clone();
        }
    };
    assert_eq!(status, "completed");
    session.idle("main").await.unwrap();
    let big_params = json!({"sessionId":shell.sid,"branchId":"main","taskId":big});
    let clamped = exec_output_page(&mut shell, &big_params, 0, 4 * 1024 * 1024 + 1).await;
    assert_ok(&clamped);
    assert_eq!(clamped["result"]["bytes"], 4 * 1024 * 1024);
    assert_eq!(
        clamped["result"]["text"].as_str().unwrap().len(),
        4 * 1024 * 1024
    );
    assert_eq!(clamped["result"]["truncated"], false);
    assert_eq!(clamped["result"]["nextCursor"], 4 * 1024 * 1024);
    let tail = exec_output_page(&mut shell, &big_params, 4 * 1024 * 1024, 4 * 1024 * 1024).await;
    assert_ok(&tail);
    assert_eq!(tail["result"]["bytes"], 5_500_000 - 4 * 1024 * 1024);
    assert!(tail["result"].get("nextCursor").is_none());
    // A four-byte character under a one-byte budget is the upper bound of
    // the whole-character extension: bytes exceed the limit by at most 3.
    let command =
        "while [ ! -f face ]; do sleep 0.01; done; printf '\\360\\237\\230\\200\\n'; exit 0";
    let text = format!(
        "tool:exec {}",
        json!({"command":command,"runInBackground":true,"timeoutMs":-1})
    );
    assert_ok(
        &shell
            .client
            .rpc(
                "session.submit",
                json!({"sessionId":shell.sid,"branchId":"main","text":text}),
            )
            .await,
    );
    let face = next_exec_started(&mut shell, 0).await;
    session.idle("main").await.unwrap();
    std::fs::write(shell.work.join("face"), b"go").unwrap();
    let status = loop {
        let event = shell.client.notice("session.event").await;
        if event["params"]["event"]["type"] == "exec"
            && event["params"]["event"]["name"] == "ended"
            && event["params"]["event"]["data"]["task_id"] == face
        {
            break event["params"]["event"]["data"]["status"].clone();
        }
    };
    assert_eq!(status, "completed");
    session.idle("main").await.unwrap();
    let face_params = json!({"sessionId":shell.sid,"branchId":"main","taskId":face});
    let face_page = exec_output_page(&mut shell, &face_params, 0, 1).await;
    assert_ok(&face_page);
    assert_eq!(
        face_page["result"],
        json!({"text":"😀","bytes":4,"nextCursor":4,"truncated":false})
    );
    // A removed registered file is the page method's own error.
    std::fs::remove_file(
        shell
            .home
            .join("rust/outputs")
            .join(&shell.sid)
            .join(format!("{}.log", task.as_str().unwrap())),
    )
    .unwrap();
    let gone = exec_output_page(&mut shell, &params, 0, 10).await;
    assert_eq!(app_error(&gone), "exec_output_unavailable", "{gone}");
    assert_eq!(
        shell
            .client
            .rpc("session.close", json!({"sessionId":shell.sid}))
            .await
            .get("error"),
        None
    );
    assert_eq!(
        shell.client.rpc("session.exec.output.page", params).await["error"]["data"]["appError"],
        "session_closed"
    );
    shell.shutdown().await;
}

/// The page method's namespace answers `service_reloading` while the owner
/// reloads: the WorkspaceTools unload waits for an in-flight exec call (one
/// whose background start is held open), and a task started after the reload
/// pages normally in the new generation.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reloading_workspace_tools_answers_the_page_method_with_service_reloading() {
    let mut shell = Shell::launch_with(json!({
        "tools":["exec"], "permissionPolicy":{"preset":"full-access"}
    }))
    .await;
    let sid = shell.sid.clone();
    assert_ok(
        &shell
            .client
            .rpc("session.attach", json!({"sessionId":sid}))
            .await,
    );
    let mut hold = dim_agent_capabilities::workspace_tools::background::testing::hold_start(
        &shell.home.join("rust/outputs"),
    );
    let text = format!(
        "tool:exec {}",
        json!({"command":"echo hello","runInBackground":true,"timeoutMs":-1})
    );
    assert_ok(
        &shell
            .client
            .rpc(
                "session.submit",
                json!({"sessionId":sid,"branchId":"main","text":text}),
            )
            .await,
    );
    hold.arrived().await;
    let restart = tokio::spawn(shell.restart_plugin("WorkspaceTools"));
    let params = json!({"sessionId":sid,"branchId":"main","taskId":"held"});
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let reply = exec_output_page(&mut shell, &params, 0, 64).await;
            if app_error(&reply) == "service_reloading" {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the page method answers service_reloading while WorkspaceTools reloads");
    drop(hold);
    restart.await.unwrap().unwrap();
    assert_ok(
        &shell
            .client
            .rpc("session.idle", json!({"sessionId":sid,"branchId":"main"}))
            .await,
    );
    let after = shell.through().await;
    let text = format!(
        "tool:exec {}",
        json!({"command":"echo after","runInBackground":true,"timeoutMs":-1})
    );
    assert_ok(
        &shell
            .client
            .rpc(
                "session.submit",
                json!({"sessionId":sid,"branchId":"main","text":text}),
            )
            .await,
    );
    let task = next_exec_started(&mut shell, after).await;
    let params = json!({"sessionId":sid,"branchId":"main","taskId":task});
    let reply = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let reply = exec_output_page(&mut shell, &params, 0, 64).await;
            if reply.get("error").is_none() && reply["result"]["text"] == json!("after\n") {
                break reply;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(reply["result"]["text"], "after\n");
    assert_eq!(reply["result"]["bytes"], 6);
    shell.shutdown().await;
}

/// What a test reaches inside the most recent Session: a plugin context, to
/// restart ShellInput alone, and the Session's branch tree, to close a branch
/// through its facade, as no RPC does.
#[derive(Clone, Default)]
struct Probe(Arc<std::sync::Mutex<Option<Probed>>>);
struct Probed {
    ctx: rutis::Ctx,
    tree: Arc<TreeCommands>,
}
struct ProbePlugin {
    probe: Probe,

    tree: rutis::host::Key<TreeCommands>,
}
impl ProbePlugin {
    fn new(names: &dim_agent_runtime::Names<'_>, probe: Probe) -> rutis::Result<Self> {
        Ok(Self {
            probe,
            tree: names.key()?,
        })
    }
}
impl rutis::host::DynPlugin for ProbePlugin {
    fn name(&self) -> &str {
        "test.shell-probe"
    }
    fn injects(&self) -> Vec<rutis::host::TypeKey> {
        vec![self.tree.erase()]
    }
    fn apply<'a>(
        &'a self,
        ctx: &'a rutis::Ctx,
    ) -> rutis::host::BoxFuture<'a, rutis::Result<rutis::host::Effect>> {
        Box::pin(async move {
            let tree = ctx.require_at(&self.tree)?;
            *self.probe.0.lock().unwrap() = Some(Probed {
                ctx: ctx.clone(),
                tree,
            });
            Ok(rutis::host::Effect::Done)
        })
    }
}

/// Holds the next branch close open once armed: the Session is then closing
/// while its parts, ShellInput included, still serve calls.
#[derive(Default)]
struct CloseGate {
    armed: AtomicBool,
    held: Notify,
    release: Notify,
}
struct CloseGatePlugin {
    gate: Arc<CloseGate>,
    lifecycle: rutis::host::Key<BranchLifecycle>,
}
impl CloseGatePlugin {
    fn new(names: &dim_agent_runtime::Names<'_>, gate: Arc<CloseGate>) -> rutis::Result<Self> {
        Ok(Self {
            gate,
            lifecycle: names.key()?,
        })
    }
}
impl rutis::host::DynPlugin for CloseGatePlugin {
    fn name(&self) -> &str {
        "test.shell-close-gate"
    }
    fn injects(&self) -> Vec<rutis::host::TypeKey> {
        vec![self.lifecycle.erase()]
    }
    fn apply<'a>(
        &'a self,
        ctx: &'a rutis::Ctx,
    ) -> rutis::host::BoxFuture<'a, rutis::Result<rutis::host::Effect>> {
        let gate = self.gate.clone();
        Box::pin(async move {
            ctx.effect(|| async move {
                if gate.armed.swap(false, Ordering::SeqCst) {
                    gate.held.notify_one();
                    gate.release.notified().await;
                }
                Ok(())
            })?;
            Ok(rutis::host::Effect::Done)
        })
    }
}

/// A local-execution server, ShellInput included, with an isolated home whose
/// exec shell is `/bin/sh`, and one Session working in `work`. A fixed model
/// catalog selects the mock model, so no product provider state is seeded.
struct Shell {
    server: RunningServer,
    client: Client,
    sid: String,
    probe: Probe,
    close_gate: Arc<CloseGate>,
    home: PathBuf,
    work: PathBuf,
}
impl Shell {
    async fn launch() -> Self {
        Self::launch_with(json!({})).await
    }
    /// `session` adds to the `session.create` params.
    async fn launch_with(session: Value) -> Self {
        Self::launch_unloading(session, ServerConfig::default().idle_unload_after, None).await
    }
    /// As [`Self::launch_with`], with `grace` as the idle unload grace period.
    async fn launch_unloading(
        session: Value,
        grace: Duration,
        session_typescript: Option<dim_app_server::session_typescript::SessionTypeScriptConfig>,
    ) -> Self {
        let home = PathBuf::from("/tmp").join(format!("dsh-{}", uuid::Uuid::new_v4()));
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&home)
            .unwrap();
        let work = home.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let target = home.join("rust");
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&target)
            .unwrap();
        set_shell(&target, "/bin/sh");
        let models = Arc::new(
            dim_agent_aimux::AimuxModels::from_config(
                serde_json::from_value::<dim_agent_aimux::config::ModelConfigFile>(json!({
                    "defaultModel": {"providerId": "local", "modelId": "mock"},
                    "providers": [{"id": "local", "kind": "openai-compatible",
                        "baseUrl": "https://model.example/v1", "models": [{"id": "mock", "contextWindow": 128000, "maxOutputTokens": 4096, "config": {"maxTokens": 64}}]}],
                }))
                .unwrap(),
            )
            .unwrap(),
        );
        let mut builder = AgentBuilder::new(AgentConfig::default());
        defaults::install_local_execution(
            &mut builder,
            ModelProviderPlugin::new,
            WorkspaceToolsConfig {
                boundary: None,
                output_root: target.join("outputs"),
            },
            dim_agent_capabilities::timeline_tree::BranchToolConfig::default(),
        );
        dim_app_server::permission_rules::install(&mut builder);
        defaults::mock_model_hosts(&mut builder);
        let probe = Probe::default();
        let slot = probe.clone();
        builder.require_feature(
            Feature::new("test.shell-probe").session("test.shell-probe", move |names| {
                Ok(Box::new(ProbePlugin::new(names, slot.clone())?))
            }),
        );
        let close_gate = Arc::new(CloseGate::default());
        let gate = close_gate.clone();
        builder.require_feature(
            Feature::new("test.shell-close-gate").branch("test.shell-close-gate", move |names| {
                Ok(Box::new(CloseGatePlugin::new(names, gate.clone())?))
            }),
        );
        let model = models.default_model().clone();
        let server = serve(
            builder,
            ServerConfig {
                target_home: Some(target.clone()),
                model_catalog: Some(models),
                unix_path: Some(target.join("daemon.sock")),
                session_defaults: dim_agent_runtime::SessionConfig {
                    model: dim_agent_types::ModelSelection {
                        provider_id: model.provider_id,
                        model_id: model.model_id,
                        capability_snapshot: None,
                        config: None,
                        metadata: None,
                    },
                    tools: Some(vec![]),
                    ..Default::default()
                },
                idle_unload_after: grace,
                session_typescript,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let mut client = Client::new_version(&server, false, 3).await;
        let mut params = json!({"cwd": work});
        params
            .as_object_mut()
            .unwrap()
            .extend(session.as_object().unwrap().clone());
        let created = client.rpc("session.create", params).await;
        let sid = created["result"]["sessionId"]
            .as_str()
            .unwrap_or_else(|| panic!("{created}"))
            .to_owned();
        Self {
            server,
            client,
            sid,
            probe,
            close_gate,
            home,
            work,
        }
    }
    /// Restarts the Session plugin named `name` and nothing else.
    fn restart_plugin(
        &self,
        name: &str,
    ) -> rutis::host::BoxFuture<'static, Result<(), Arc<rutis::host::ErrorKind>>> {
        let session = self.server.agent().unwrap().get_session(&self.sid).unwrap();
        let plugins = session.diagnostics().unwrap().plugins;
        let id = plugins
            .iter()
            .find(|plugin| plugin.name == name)
            .unwrap()
            .id;
        let ctx = self.probe.0.lock().unwrap().as_ref().unwrap().ctx.clone();
        ctx.view(id).unwrap().restart()
    }
    /// Restarts the Session's ShellInput plugin and nothing else.
    fn restart_shell_input(
        &self,
    ) -> rutis::host::BoxFuture<'static, Result<(), Arc<rutis::host::ErrorKind>>> {
        self.restart_plugin("ShellInput")
    }
    fn tree(&self) -> Arc<TreeCommands> {
        self.probe.0.lock().unwrap().as_ref().unwrap().tree.clone()
    }
    /// A host's idle unload of the Session once its main branch is idle:
    /// whether the Session was released. The next call restores it.
    async fn unload(&self) -> bool {
        let session = self.server.agent().unwrap().get_session(&self.sid).unwrap();
        session.idle("main").await.unwrap();
        session.unload_if_idle().await.unwrap()
    }
    /// Waits until a Session created now has been released by the idle
    /// unload pass: at least two passes have run since, each over every
    /// loaded Session.
    async fn passes(&mut self) {
        let created = self
            .client
            .rpc("session.create", json!({"cwd": self.work}))
            .await;
        let id = created["result"]["sessionId"].as_str().unwrap();
        unloaded(self.server.agent().unwrap().get_session(id).unwrap()).await;
    }
    fn run_params(&self, run_id: &str, command: &str, mode: &str) -> Value {
        json!({"sessionId": self.sid, "runId": run_id, "command": command, "mode": mode})
    }
    async fn run(&mut self, run_id: &str, command: &str, mode: &str) -> Value {
        let params = self.run_params(run_id, command, mode);
        self.client.rpc("shell.run", params).await
    }
    async fn send_run(&mut self, run_id: &str, command: &str, mode: &str) -> u64 {
        let params = self.run_params(run_id, command, mode);
        self.client.send("shell.run", params).await
    }
    async fn list(&mut self) -> Value {
        let sid = self.sid.clone();
        let reply = self
            .client
            .rpc("shell.list", json!({"sessionId": sid}))
            .await;
        reply["result"]["runs"].clone()
    }
    async fn stop(&mut self, run_id: &str) -> Value {
        let sid = self.sid.clone();
        self.client
            .rpc("shell.stop", json!({"sessionId": sid, "runId": run_id}))
            .await
    }
    /// Main-branch events after `after`, once the branch has dispatched them.
    async fn events(&mut self, after: u64) -> Vec<Value> {
        let sid = self.sid.clone();
        assert_ok(
            &self
                .client
                .rpc(
                    "session.idle",
                    json!({"sessionId": sid, "branchId": "main"}),
                )
                .await,
        );
        self.history(after).await
    }
    /// Main-branch events after `after`, as published so far.
    async fn history(&mut self, after: u64) -> Vec<Value> {
        let sid = self.sid.clone();
        let page = self
            .client
            .rpc(
                "session.history",
                json!({"sessionId": sid, "branchId": "main", "after": after, "limit": 1000}),
            )
            .await;
        page["result"]["events"]
            .as_array()
            .unwrap_or_else(|| panic!("{page}"))
            .clone()
    }
    /// The recorded inputs among main-branch events after `after`.
    async fn recorded(&mut self, after: u64) -> Vec<Value> {
        self.events(after)
            .await
            .into_iter()
            .filter(|event| {
                event["type"] == "input"
                    && event["name"] == "message"
                    && event["data"]["blocks"][0]["text"]
                        .as_str()
                        .is_some_and(|text| text.starts_with("<dim-shell-input "))
            })
            .collect()
    }
    /// The last sequence of the main branch, read through `session.history`.
    async fn through(&mut self) -> u64 {
        let sid = self.sid.clone();
        let page = self
            .client
            .rpc(
                "session.history",
                json!({"sessionId": sid, "branchId": "main", "after": 0, "limit": 1000}),
            )
            .await;
        page["result"]["through"].as_u64().unwrap()
    }
    async fn shutdown(self) {
        drop(self.client);
        self.server.shutdown();
        self.server.join().await.unwrap();
        std::fs::remove_dir_all(self.home).unwrap();
    }
}

fn set_shell(home: &Path, executable: &str) {
    std::fs::write(
        home.join("exec-shell.json"),
        json!({"version": 1, "executablePath": executable}).to_string(),
    )
    .unwrap();
}

fn app_error(reply: &Value) -> &str {
    reply["error"]["data"]["appError"]
        .as_str()
        .unwrap_or_default()
}

/// Waits until a command has written `name` (through a rename, so whole).
async fn written(work: &Path, name: &str) -> String {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Ok(text) = std::fs::read_to_string(work.join(name)) {
                return text.trim().to_owned();
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{name} was not written"))
}

fn alive(pid: &str) -> bool {
    std::process::Command::new("kill")
        .args(["-0", pid])
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap()
        .success()
}

/// Waits until no process of `pids` is left.
async fn gone(pids: &[&str]) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while pids.iter().any(|pid| alive(pid)) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("processes {pids:?} are still running"));
}

/// A command that records its shell and a background child, then runs until
/// it finds `go` in its working directory.
const HELD: &str = "sleep 600 & echo $! > child.tmp && mv child.tmp child; \
                    echo $$ > pid.tmp && mv pid.tmp pid; echo started; \
                    while [ ! -f go ]; do sleep 0.02; done; echo finished";
/// A command that records its shell and runs until it finds `go`.
const WAITING: &str = "echo $$ > pid.tmp && mv pid.tmp pid; echo started; \
                       while [ ! -f go ]; do sleep 0.02; done; echo finished";

/// Design D11 (first half): without ShellInput the names are unknown. With
/// it, each method checks its own params, and a shell the builder refuses or
/// an unknown branch is reported before anything runs.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shell_methods_need_the_assembly_and_check_their_requests() {
    let (server, _) = launch_with(Arc::new(MockTools), ServerConfig::default(), |_| {}).await;
    let mut client = Client::new_version(&server, false, 3).await;
    let created = client.rpc("session.create", create(json!([]))).await;
    let sid = created["result"]["sessionId"].as_str().unwrap().to_owned();
    for method in ["shell.run", "shell.stop", "shell.list"] {
        let reply = client.rpc(method, json!({"sessionId": sid})).await;
        assert_eq!(app_error(&reply), "unknown_method", "{method}");
    }
    drop(client);
    server.shutdown();
    server.join().await.unwrap();

    let mut shell = Shell::launch().await;
    let sid = shell.sid.clone();
    let methods = shell
        .client
        .rpc("session.methods", json!({"sessionId": sid}))
        .await;
    for method in [
        json!({"name": "shell.list", "mutating": false}),
        json!({"name": "shell.run", "mutating": true}),
        json!({"name": "shell.stop", "mutating": true}),
    ] {
        assert!(
            methods["result"]["methods"]
                .as_array()
                .unwrap()
                .contains(&method),
            "{methods}"
        );
    }
    let limit = 64 * 1024;
    let valid = shell.run_params("r1", "true", "private");
    for change in [
        json!({"command": ""}),
        json!({"command": "x".repeat(limit + 1)}),
        json!({"runId": ""}),
        json!({"runId": "a_b"}),
        json!({"runId": "r".repeat(65)}),
        json!({"runId": 7}),
        json!({"mode": "loud"}),
        json!({"extra": true}),
    ] {
        let mut params = valid.clone();
        params
            .as_object_mut()
            .unwrap()
            .extend(change.as_object().unwrap().clone());
        let reply = shell.client.rpc("shell.run", params).await;
        assert_eq!(reply["error"]["code"], -32602, "{change}: {reply}");
        assert_eq!(app_error(&reply), "invalid_params", "{change}");
    }
    for (method, params) in [
        (
            "shell.run",
            json!({"sessionId": sid, "command": "true", "mode": "private"}),
        ),
        ("shell.stop", json!({"sessionId": sid})),
        ("shell.stop", json!({"sessionId": sid, "runId": "a_b"})),
        ("shell.list", json!({"sessionId": sid, "runId": "r1"})),
    ] {
        let reply = shell.client.rpc(method, params).await;
        assert_eq!(app_error(&reply), "invalid_params", "{method}: {reply}");
    }
    let longest = format!("true{}", " ".repeat(limit - 4));
    let reply = shell.run(&"r".repeat(64), &longest, "private").await;
    assert_eq!(reply["result"]["status"], "completed", "{reply}");

    let mut params = shell.run_params("r2", "true", "private");
    params["branchId"] = json!("missing");
    let reply = shell.client.rpc("shell.run", params).await;
    assert_eq!(app_error(&reply), "session_invalid_input", "{reply}");

    set_shell(&shell.home.join("rust"), "/missing/shell");
    let reply = shell.run("r3", "true", "private").await;
    assert_eq!(app_error(&reply), "shell_unavailable", "{reply}");
    assert!(
        reply["error"]["message"]
            .as_str()
            .unwrap()
            .starts_with("EXEC_SHELL_UNAVAILABLE: "),
        "{reply}"
    );
    set_shell(&shell.home.join("rust"), "/bin/sh");
    assert_eq!(
        shell.run("r3", "true", "private").await["result"]["status"],
        "completed"
    );
    shell.shutdown().await;
}

/// Design D3 and A.2: a private run replies the output as exec bounds it
/// (decoded, control characters removed, the last 2000 lines) with its exit,
/// and the Timeline does not change.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_private_run_replies_exec_bounded_output_and_writes_nothing() {
    let mut shell = Shell::launch().await;
    let before = shell.through().await;

    let reply = shell
        .run(
            "exit",
            "printf 'out\\n'; printf 'err\\n' >&2; exit 3",
            "private",
        )
        .await;
    let result = &reply["result"];
    assert_eq!(result["runId"], "exit");
    assert_eq!(result["status"], "completed");
    assert_eq!(result["exitCode"], 3);
    assert!(result.get("signal").is_none() && result.get("recorded").is_none());
    let output = result["output"].as_str().unwrap();
    assert!(
        output.contains("out\n") && output.contains("err\n"),
        "{output}"
    );
    assert_eq!(result["truncated"], false);
    assert!(result["durationMs"].is_u64());

    let reply = shell.run("signal", "kill -TERM $$", "private").await;
    assert_eq!(reply["result"]["signal"], "SIGTERM", "{reply}");
    assert!(reply["result"].get("exitCode").is_none());

    let reply = shell
        .run("bytes", "printf 'a\\033[31mb\\377\\n'", "private")
        .await;
    assert_eq!(reply["result"]["output"], "a[31mb<0xFF>\n");

    let lines = "i=0; while [ $i -lt 3000 ]; do echo line$i; i=$((i+1)); done";
    let reply = shell.run("lines", lines, "private").await;
    let output = reply["result"]["output"].as_str().unwrap();
    assert_eq!(output.lines().count(), 1999);
    assert!(output.starts_with("line1001\n") && output.ends_with("line2999\n"));
    assert_eq!(reply["result"]["truncated"], true);

    assert!(
        shell.history(before).await.iter().all(|event| {
            event["type"] == "input"
                && (event["name"] == "applied"
                    || (event["name"] == "message" && event["data"]["user"].is_null()))
        }),
        "private execution records no input or execution facts"
    );
    shell.shutdown().await;
}

/// Design D4 and D5: one run per Session; a turn stop does not reach it;
/// `shell.stop` terminates the whole tree and the run records `stopped` with
/// the output read so far. A used runId is refused for this generation. No
/// run, stopped, refused or completed, keeps the Session from unloading.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn one_run_per_session_until_it_ends_or_is_stopped() {
    let mut shell = Shell::launch().await;
    let sid = shell.sid.clone();
    let before = shell.through().await;
    assert_eq!(shell.list().await, json!([]));
    let held = shell.send_run("held", HELD, "record").await;
    let (pid, child) = (
        written(&shell.work, "pid").await,
        written(&shell.work, "child").await,
    );
    let runs = shell.list().await;
    assert_eq!(runs.as_array().unwrap().len(), 1, "{runs}");
    assert_eq!(runs[0]["runId"], "held");
    assert_eq!(runs[0]["branchId"], "main");
    assert_eq!(runs[0]["mode"], "record");
    assert_eq!(runs[0]["command"], HELD);
    assert!(runs[0]["startedAt"].as_u64().unwrap() > 0);

    for run_id in ["other", "held"] {
        let reply = shell.run(run_id, "true", "private").await;
        assert_eq!(app_error(&reply), "shell_busy", "{run_id}: {reply}");
    }
    assert_ok(
        &shell
            .client
            .rpc(
                "session.stop",
                json!({"sessionId": sid, "branchId": "main"}),
            )
            .await,
    );
    assert_eq!(shell.list().await[0]["runId"], "held");
    assert!(alive(&pid) && alive(&child));

    assert_eq!(shell.stop("held").await["result"], json!({"stopped": true}));
    let reply = shell.client.response(held).await;
    let result = &reply["result"];
    assert_eq!(result["status"], "stopped", "{reply}");
    assert_eq!(result["output"], "started\n");
    assert!(result.get("exitCode").is_none() && result.get("signal").is_none());
    gone(&[&pid, &child]).await;
    let recorded = shell.recorded(before).await;
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0]["sequence"], result["recorded"]["sequence"]);
    let text = recorded[0]["data"]["blocks"][0]["text"].as_str().unwrap();
    assert!(
        text.starts_with(
            "<dim-shell-input kind=\"command\" status=\"stopped\" truncated=\"false\">\n$ sleep 600 &amp; "
        ) && text.ends_with("\nstarted\n\n</dim-shell-input>"),
        "{text}"
    );

    assert_eq!(
        shell.stop("held").await["result"],
        json!({"stopped": false})
    );
    assert_eq!(shell.list().await, json!([]));
    let reply = shell.run("held", "true", "private").await;
    assert_eq!(app_error(&reply), "duplicate_run", "{reply}");
    let reply = shell.run("next", "echo next", "private").await;
    assert_eq!(reply["result"]["output"], "next\n");
    assert!(shell.unload().await);
    shell.shutdown().await;
}

/// Design D7 (second half): closing the Session terminates the tree and the
/// call replies `operation_interrupted`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn closing_the_session_terminates_the_run() {
    let mut shell = Shell::launch().await;
    let sid = shell.sid.clone();
    let held = shell.send_run("held", HELD, "record").await;
    let (pid, child) = (
        written(&shell.work, "pid").await,
        written(&shell.work, "child").await,
    );
    let mut other = Client::new_version(&shell.server, false, 3).await;
    assert_ok(&other.rpc("session.close", json!({"sessionId": sid})).await);
    let reply = shell.client.response(held).await;
    assert_eq!(app_error(&reply), "operation_interrupted", "{reply}");
    gone(&[&pid, &child]).await;
    drop(other);
    shell.shutdown().await;
}

/// Design D7 (last case): a Session that starts closing while a run's command
/// is built refuses the run at its second admission, where it would take its
/// Session hold, with `session_closed`; nothing starts. The branch close is
/// held open, so ShellInput still serves the call while the Session closes.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_session_closing_before_the_spawn_starts_nothing() {
    let mut shell = Shell::launch().await;
    let sid = shell.sid.clone();
    let mut held = hold_start(&shell.work);
    let call = shell.send_run("late", "echo ran > ran", "record").await;
    held.arrived().await;
    shell.close_gate.armed.store(true, Ordering::SeqCst);
    let mut other = Client::new_version(&shell.server, false, 3).await;
    let close = other.send("session.close", json!({"sessionId": sid})).await;
    tokio::time::timeout(Duration::from_secs(10), shell.close_gate.held.notified())
        .await
        .expect("the Session is closing");
    drop(held);
    let reply = shell.client.response(call).await;
    assert_eq!(app_error(&reply), "session_closed", "{reply}");
    shell.close_gate.release.notify_one();
    assert_ok(&other.response(close).await);
    assert!(!shell.work.join("ran").exists());
    drop(other);
    shell.shutdown().await;
}

/// Design D10: an Rpc reload during a run completes at once and the call
/// replies `operation_interrupted`; the run stays with ShellInput, ends on
/// its own and is recorded.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_rpc_reload_ends_the_wait_but_not_the_run() {
    let mut shell = Shell::launch().await;
    let before = shell.through().await;
    let held = shell.send_run("held", WAITING, "record").await;
    let pid = written(&shell.work, "pid").await;
    tokio::time::timeout(Duration::from_secs(5), shell.server.restart("Rpc"))
        .await
        .expect("the reload does not wait for the command")
        .unwrap();
    let reply = shell.client.response(held).await;
    assert_eq!(app_error(&reply), "operation_interrupted", "{reply}");
    assert!(alive(&pid));
    assert_eq!(shell.list().await[0]["runId"], "held");

    std::fs::write(shell.work.join("go"), "").unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while shell.list().await != json!([]) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the run ends on its own");
    gone(&[&pid]).await;
    let recorded = shell.recorded(before).await;
    assert_eq!(recorded.len(), 1);
    let text = recorded[0]["data"]["blocks"][0]["text"].as_str().unwrap();
    assert!(
        text.starts_with("<dim-shell-input kind=\"command\" status=\"completed\" exit=\"0\" truncated=\"false\">\n$ ")
            && text.ends_with("\nstarted\nfinished\n\n</dim-shell-input>"),
        "{text}"
    );
    shell.shutdown().await;
}

/// Design D12: an Rpc reload while a run's command is still being prepared
/// ends the call at once and starts nothing, so it holds nothing; the runId
/// stays unused.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_rpc_reload_before_the_spawn_starts_nothing() {
    let mut shell = Shell::launch().await;
    let before = shell.through().await;
    let mut held = hold_start(&shell.work);
    let call = shell.send_run("early", "echo ran > ran", "record").await;
    held.arrived().await;
    tokio::time::timeout(Duration::from_secs(5), shell.server.restart("Rpc"))
        .await
        .expect("the reload does not wait for the preparation")
        .unwrap();
    let reply = shell.client.response(call).await;
    assert_eq!(app_error(&reply), "operation_interrupted", "{reply}");
    drop(held);
    assert_eq!(shell.list().await, json!([]));
    let reply = shell.run("early", "true", "record").await;
    assert_eq!(reply["result"]["status"], "completed", "{reply}");
    assert!(!shell.work.join("ran").exists());
    let recorded = shell.recorded(before).await;
    assert_eq!(recorded.len(), 1);
    assert_eq!(
        recorded[0]["sequence"],
        reply["result"]["recorded"]["sequence"]
    );
    assert!(shell.unload().await);
    shell.shutdown().await;
}

/// Design D13: a run is admitted again, with its spawn, once its command is
/// built. A `shell.stop` that came first means it starts nothing and replies
/// `stopped`, using its runId; a run started meanwhile makes it `shell_busy`;
/// one that used its runId meanwhile makes it `duplicate_run`. No run is
/// left holding the Session.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_run_is_admitted_again_with_its_spawn() {
    let mut shell = Shell::launch().await;
    let before = shell.through().await;
    let stopped = |run_id: &str| json!({"runId": run_id, "status": "stopped", "output": "", "truncated": false, "durationMs": 0});

    let mut held = hold_start(&shell.work);
    let call = shell.send_run("building", "echo ran > ran", "record").await;
    held.arrived().await;
    assert_eq!(
        shell.stop("building").await["result"],
        json!({"stopped": false})
    );
    drop(held);
    assert_eq!(
        shell.client.response(call).await["result"],
        stopped("building")
    );
    assert_eq!(
        app_error(&shell.run("building", "true", "private").await),
        "duplicate_run"
    );
    assert_eq!(
        shell.stop("ahead").await["result"],
        json!({"stopped": false})
    );
    assert_eq!(
        shell.run("ahead", "echo ran > ran", "record").await["result"],
        stopped("ahead")
    );

    let mut held = hold_start(&shell.work);
    let late = shell.send_run("late", "echo ran > ran", "record").await;
    held.arrived().await;
    let other = shell.send_run("other", WAITING, "private").await;
    let pid = written(&shell.work, "pid").await;
    drop(held);
    assert_eq!(app_error(&shell.client.response(late).await), "shell_busy");
    std::fs::write(shell.work.join("go"), "").unwrap();
    assert_eq!(
        shell.client.response(other).await["result"]["status"],
        "completed"
    );
    gone(&[&pid]).await;

    let mut held = hold_start(&shell.work);
    let twice = shell.send_run("twice", "echo ran > ran", "record").await;
    held.arrived().await;
    assert_eq!(
        shell.run("twice", "true", "private").await["result"]["status"],
        "completed"
    );
    drop(held);
    assert_eq!(
        app_error(&shell.client.response(twice).await),
        "duplicate_run"
    );

    assert!(!shell.work.join("ran").exists());
    assert!(shell.recorded(before).await.is_empty());
    assert!(shell.unload().await);
    shell.shutdown().await;
}

/// Design D1: `!command` while idle writes one Append input with the marker
/// once the command has ended; it starts no turn and is consumed with the
/// first model request of the next prompt, before the prompt.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_recorded_run_is_one_append_input_for_the_next_request() {
    let mut shell = Shell::launch().await;
    let sid = shell.sid.clone();
    let before = shell.through().await;
    let reply = shell.run("echo", "echo hi", "record").await;
    let sequence = reply["result"]["recorded"]["sequence"].as_u64().unwrap();
    let events = shell.events(before).await;
    let input = events
        .iter()
        .find(|event| event["sequence"] == sequence)
        .unwrap()
        .clone();
    assert_eq!(input["name"], "message");
    assert_eq!(
        input["data"],
        json!({"mode": "append", "blocks": [{"type": "raw", "text":
            "<dim-shell-input kind=\"command\" status=\"completed\" exit=\"0\" truncated=\"false\">\n$ echo hi\nhi\n\n</dim-shell-input>"}]})
    );
    assert!(events.iter().any(|event| event["name"] == "applied"
        && event["data"] == json!({"input_id": input["id"], "mode": "append"})));
    assert!(
        events.iter().all(|event| event["type"] != "turn"),
        "{events:?}"
    );

    let prompt = shell
        .client
        .rpc(
            "session.submit",
            json!({"sessionId": sid, "branchId": "main", "text": "next prompt"}),
        )
        .await;
    let prompt = &prompt["result"]["event"]["id"];
    let events = shell.events(sequence).await;
    let position = |name: &str, input: &Value| {
        events
            .iter()
            .position(|event| {
                event["name"] == name && (input.is_null() || event["data"]["input_id"] == *input)
            })
            .unwrap_or_else(|| panic!("{name} {input}: {events:?}"))
    };
    let consumed = position("consumed", &input["id"]);
    assert!(consumed < position("consumed", prompt));
    assert!(position("consumed", prompt) < position("model-start", &Value::Null));
    // The request that carries the marker carries its entry, registered by
    // the Session part that wrote it from its own `apply`.
    let session = shell.server.agent().unwrap().get_session(&sid).unwrap();
    session.idle("main").await.unwrap();
    let system = super::reload::last_system(&session, "main").await;
    assert!(system.contains(SHELL_INPUT_ENTRY), "{system}");
    shell.shutdown().await;
}

/// Design D2: a run recorded and applied while a tool batch runs is consumed
/// with the first model request after the batch result, in the same turn.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_run_recorded_during_a_tool_batch_follows_its_result() {
    let mut shell = Shell::launch_with(
        json!({"tools": ["exec"], "permissionPolicy": {"allowedTools": ["exec"]}}),
    )
    .await;
    let sid = shell.sid.clone();
    let before = shell.through().await;
    let tool = json!({"command": "echo > tool.tmp && mv tool.tmp tool; while [ ! -f go ]; do sleep 0.02; done"});
    assert_ok(
        &shell
            .client
            .rpc(
                "session.submit",
                json!({"sessionId": sid, "branchId": "main", "text": format!("tool:exec {tool}")}),
            )
            .await,
    );
    written(&shell.work, "tool").await;
    let reply = shell.run("during", "echo hi", "record").await;
    let sequence = reply["result"]["recorded"]["sequence"].clone();
    // Applied while the batch runs. Loop decides each request when it
    // dispatches the batch result, so an input applied after that result
    // would wait for the next request (design Appendix B).
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let events = shell.history(before).await;
            let id = events
                .iter()
                .find(|event| event["sequence"] == sequence)
                .map(|event| event["id"].clone());
            if events.iter().any(|event| {
                event["name"] == "applied" && id.as_ref() == Some(&event["data"]["input_id"])
            }) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the input is applied");
    std::fs::write(shell.work.join("go"), "").unwrap();
    let events = shell.events(before).await;
    let position = |matches: &dyn Fn(&Value) -> bool| {
        events
            .iter()
            .position(matches)
            .unwrap_or_else(|| panic!("{events:?}"))
    };
    let input = position(&|event| event["sequence"] == sequence);
    let result = position(&|event| event["name"] == "tool-result");
    let consumed = position(&|event| {
        event["name"] == "consumed" && event["data"]["input_id"] == events[input]["id"]
    });
    let requests: Vec<_> = (0..events.len())
        .filter(|at| events[*at]["name"] == "model-start")
        .collect();
    assert!(input < result && result < consumed, "{events:?}");
    assert!(
        requests.len() == 2 && requests[0] < input && consumed < requests[1],
        "{events:?}"
    );
    let turns: Vec<_> = events
        .iter()
        .filter(|event| event["type"] == "turn")
        .map(|event| &event["name"])
        .collect();
    assert_eq!(turns.len(), 2, "one turn: {events:?}");
    shell.shutdown().await;
}

/// Design D7 (first half): closing the run's branch does not stop it; the
/// write is refused and the reply carries the output with `recorded.error`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_closed_branch_refuses_the_write_but_not_the_run() {
    let mut shell = Shell::launch_with(
        json!({"tools": ["branch"], "permissionPolicy": {"allowedTools": ["branch"]}}),
    )
    .await;
    let sid = shell.sid.clone();
    let at = shell.through().await;
    let created = shell
        .client
        .rpc(
            "session.branch_create",
            json!({"sessionId": sid, "parentBranchId": "main", "at": at, "prompt": "child"}),
        )
        .await;
    let child = created["result"]["branch"]["branchId"]
        .as_str()
        .unwrap_or_else(|| panic!("{created}"))
        .to_owned();
    let mut params = shell.run_params("held", WAITING, "record");
    params["branchId"] = json!(child);
    let held = shell.client.send("shell.run", params).await;
    let pid = written(&shell.work, "pid").await;
    shell.tree().get(&child).unwrap().close().await.unwrap();
    assert!(alive(&pid));
    std::fs::write(shell.work.join("go"), "").unwrap();
    let reply = shell.client.response(held).await;
    let result = &reply["result"];
    assert_eq!(result["status"], "completed", "{reply}");
    assert_eq!(result["output"], "started\nfinished\n");
    let error = &result["recorded"]["error"];
    assert_eq!(error["appError"], "session_closed", "{reply}");
    assert!(error["message"].is_string());
    shell.shutdown().await;
}

/// Design D11 (second half) and D8 for ShellInput alone: reloading only the
/// plugin interrupts the call, terminates the tree (here one that ignores
/// SIGTERM, so the old generation unloads for its grace) and writes nothing.
/// Meanwhile the Session answers `shell.*` with `service_reloading`; the next
/// generation then serves it. The ended run no longer holds the Session.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reloading_shell_input_alone_ends_its_run() {
    let mut shell = Shell::launch().await;
    let sid = shell.sid.clone();
    let before = shell.through().await;
    let held = shell
        .send_run(
            "held",
            "trap '' TERM; echo $$ > pid.tmp && mv pid.tmp pid; while true; do sleep 0.02; done",
            "record",
        )
        .await;
    let pid = written(&shell.work, "pid").await;
    let restart = tokio::spawn(shell.restart_shell_input());
    let reply = shell.client.response(held).await;
    assert_eq!(app_error(&reply), "operation_interrupted", "{reply}");
    let reply = shell
        .client
        .rpc("shell.list", json!({"sessionId": sid}))
        .await;
    assert_eq!(app_error(&reply), "service_reloading", "{reply}");
    restart.await.unwrap().unwrap();
    assert!(!alive(&pid));
    assert_eq!(shell.list().await, json!([]));
    assert!(shell.recorded(before).await.is_empty());
    let reply = shell.run("next", "echo next", "private").await;
    assert_eq!(reply["result"]["output"], "next\n");
    assert!(shell.unload().await);
    shell.shutdown().await;
}

/// Design D8 and D9: a caller that disconnects does not stop its run. The
/// run holds the Session, so a host's idle unload is refused while it runs;
/// it is recorded when it ends, and then the Session unloads and restores
/// with the input and without the run.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_run_outlives_its_caller_and_holds_the_session_until_recorded() {
    let mut shell = Shell::launch().await;
    let before = shell.through().await;
    let mut leaving = Client::new_version(&shell.server, false, 3).await;
    let params = shell.run_params("held", WAITING, "record");
    leaving.send("shell.run", params).await;
    let pid = written(&shell.work, "pid").await;
    drop(leaving);
    assert!(!shell.unload().await);
    assert!(alive(&pid));
    std::fs::write(shell.work.join("go"), "").unwrap();
    gone(&[&pid]).await;
    tokio::time::timeout(Duration::from_secs(10), async {
        while shell.recorded(before).await.is_empty() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the run is recorded");
    assert_eq!(shell.list().await, json!([]));
    assert!(shell.unload().await);
    assert_eq!(shell.list().await, json!([]));
    assert_eq!(shell.recorded(before).await.len(), 1);
    shell.shutdown().await;
}

/// Reads frames until the reply to `id`, returning the progress data of `id`
/// received before it.
async fn progress_until_reply(client: &mut Client, id: u64) -> (Vec<Value>, Value) {
    let mut progress = vec![];
    loop {
        let frame = client
            .read(&format!("response {id} or management.progress"))
            .await;
        if frame["id"] == id {
            return (progress, frame);
        }
        if frame["method"] == "management.progress" && frame["params"]["id"] == id {
            progress.push(frame["params"]["data"].clone());
        }
    }
}

/// Design D3 and A.1: the caller receives the output as progress while the
/// command runs and before the reply, decoded and with control characters
/// removed as in exec, at most one item per 100 ms.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_caller_receives_progress_before_the_reply() {
    let mut shell = Shell::launch().await;
    let id = shell
        .send_run(
            "streamed",
            "printf 'one\\033\\n'; while [ ! -f go ]; do sleep 0.02; done; printf 'two\\n'",
            "private",
        )
        .await;
    let first = loop {
        let frame = shell
            .client
            .read(&format!("shell.run progress for request {id}"))
            .await;
        if frame["method"] == "management.progress" {
            break frame["params"].clone();
        }
    };
    assert_eq!(
        first,
        json!({"id": id, "method": "shell.run", "data": {"text": "one\n"}})
    );
    std::fs::write(shell.work.join("go"), "").unwrap();
    let (progress, reply) = progress_until_reply(&mut shell.client, id).await;
    assert_eq!(progress, [json!({"text": "two\n"})]);
    assert_eq!(reply["result"]["output"], "one\ntwo\n");

    let started = Instant::now();
    let id = shell
        .send_run(
            "steady",
            "i=0; while [ $i -lt 60 ]; do echo $i; sleep 0.02; i=$((i+1)); done",
            "private",
        )
        .await;
    let (progress, reply) = progress_until_reply(&mut shell.client, id).await;
    let elapsed = started.elapsed().as_millis() as usize;
    assert!(
        progress.len() >= 3 && progress.len() <= elapsed / 100 + 2,
        "{} items in {elapsed} ms",
        progress.len()
    );
    let text: String = progress
        .iter()
        .map(|item| item["text"].as_str().unwrap())
        .collect();
    assert_eq!(text, reply["result"]["output"].as_str().unwrap());
    shell.shutdown().await;
}

/// Design D6: a command writing 10 MiB at once. Each progress item holds at
/// most 64 KiB of text and counts the bytes it left out; the items precede
/// the reply and the connection stays open; the reply and the recorded input
/// keep exec's bounds and are marked truncated.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn progress_items_are_capped_and_count_what_they_leave_out() {
    let mut shell = Shell::launch().await;
    let before = shell.through().await;
    let total = 10 * 1024 * 1024;
    let line = "0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ-";
    let command = format!("yes {line} | head -c {total}");
    let id = shell.send_run("flood", &command, "record").await;
    let (progress, reply) = progress_until_reply(&mut shell.client, id).await;
    let (mut text, mut skipped) = (0, 0);
    for item in &progress {
        let length = item["text"].as_str().unwrap().len();
        assert!(length <= 64 * 1024, "{length}");
        text += length;
        skipped += item["skipped"].as_u64().unwrap_or_default() as usize;
    }
    assert!(skipped > 0, "{progress:?}");
    assert_eq!(text + skipped, total);
    assert_ok(&shell.client.rpc("daemon.ping", json!({})).await);

    let result = &reply["result"];
    assert_eq!(result["truncated"], true);
    let output = result["output"].as_str().unwrap();
    assert!(output.len() <= 50 * 1024 && output.ends_with(&format!("{line}\n")));
    let recorded = shell.recorded(before).await;
    let marker = recorded[0]["data"]["blocks"][0]["text"].as_str().unwrap();
    assert!(marker.contains("truncated=\"true\""), "{marker:.200}");
    assert!(marker.ends_with(&format!("\n{output}\n</dim-shell-input>")));
    shell.shutdown().await;
}

/// Grace period of the idle unload tests below.
const GRACE: Duration = Duration::from_millis(200);

/// Idle unload §13 item 6, the `!` run: a run whose caller has gone holds its
/// Session with no lease, so the idle unload pass keeps the Session loaded
/// past twice the grace. When the command ends its result is recorded as
/// completed, and the Session then unloads.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_idle_unload_pass_keeps_a_session_while_a_run_works() {
    let mut shell = Shell::launch_unloading(json!({}), GRACE, None).await;
    let session = shell
        .server
        .agent()
        .unwrap()
        .get_session(&shell.sid)
        .unwrap();
    let before = shell.through().await;
    let mut leaving = Client::new_version(&shell.server, false, 3).await;
    let params = shell.run_params("held", WAITING, "record");
    leaving.send("shell.run", params).await;
    let pid = written(&shell.work, "pid").await;
    drop(leaving);
    shell.passes().await;
    shell.passes().await;
    assert!(!session.is_unloading());
    assert!(alive(&pid));
    std::fs::write(shell.work.join("go"), "").unwrap();
    unloaded(session).await;
    let recorded = shell.recorded(before).await;
    assert_eq!(recorded.len(), 1);
    let marker = recorded[0]["data"]["blocks"][0]["text"].as_str().unwrap();
    assert!(marker.contains("status=\"completed\""), "{marker}");
    assert!(marker.contains("finished"), "{marker}");
    shell.shutdown().await;
}

/// Idle unload §13 item 6, BackgroundExec: a background exec task holds its
/// Session with no lease after the turn that started it, so the idle unload
/// pass keeps the Session loaded past twice the grace. When the command ends
/// its `exec/ended` is recorded as completed, the turn it wakes runs, and
/// then the Session unloads.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_idle_unload_pass_keeps_a_session_while_a_background_exec_works() {
    let mut shell = Shell::launch_unloading(
        json!({"tools":["exec"], "permissionPolicy":{"preset":"full-access"}}),
        GRACE,
        None,
    )
    .await;
    let session = shell
        .server
        .agent()
        .unwrap()
        .get_session(&shell.sid)
        .unwrap();
    let before = shell.through().await;
    let text = format!(
        "tool:exec {}",
        json!({"command":WAITING,"runInBackground":true,"timeoutMs":-1})
    );
    assert_ok(
        &shell
            .client
            .rpc(
                "session.submit",
                json!({"sessionId":shell.sid,"branchId":"main","text":text}),
            )
            .await,
    );
    written(&shell.work, "pid").await;
    let started = shell.events(before).await;
    let task = started
        .iter()
        .find(|event| event["type"] == "exec" && event["name"] == "started")
        .unwrap_or_else(|| panic!("{started:?}"))["data"]["task_id"]
        .clone();
    shell.passes().await;
    shell.passes().await;
    assert!(!session.is_unloading());
    std::fs::write(shell.work.join("go"), "").unwrap();
    unloaded(session).await;
    let events = shell.events(before).await;
    let ended = events
        .iter()
        .position(|event| {
            event["type"] == "exec" && event["name"] == "ended" && event["data"]["task_id"] == task
        })
        .unwrap_or_else(|| panic!("{events:?}"));
    assert_eq!(events[ended]["data"]["status"], "completed");
    let woken: Vec<_> = events[ended..]
        .iter()
        .filter(|event| event["type"] == "turn")
        .map(|event| event["name"].as_str().unwrap())
        .collect();
    assert_eq!(woken, ["start", "end"], "{events:?}");
    shell.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires DIM_TEST_NODE and DIM_TEST_JS_RUNTIME"]
async fn node_object_faces_use_session_and_branch_owners_across_the_bridge() {
    use dim_agent_capabilities::workspace_tools::background::BackgroundExec;
    use dim_app_server::mcp::McpSession;
    let mut parts = super::session_typescript::fixture();
    parts.rows = vec![json!({
        "id":"object-faces",
        "name":parts.anchor.parent().unwrap().join("fixtures/object-faces.ts"),
        "provides":{}, "config":{},
    })];
    let mut shell = Shell::launch_unloading(
        json!({"tools":["exec"],"permissionPolicy":{"preset":"full-access"}}),
        ServerConfig::default().idle_unload_after,
        Some(parts),
    )
    .await;
    let session = shell
        .server
        .agent()
        .unwrap()
        .get_session(&shell.sid)
        .unwrap();
    let state = super::session_typescript::when_served(
        &mut shell.client,
        "session.objectface.call",
        json!({"sessionId":shell.sid,"operation":"state"}),
    )
    .await;
    assert_ok(&state);
    assert_eq!(
        state["result"],
        serde_json::to_value(session.session_service::<McpSession>().unwrap().state()).unwrap()
    );
    let missing = shell
        .client
        .rpc(
            "session.objectface.call",
            json!({"sessionId":shell.sid,"operation":"missingServer"}),
        )
        .await;
    assert_ok(&missing);
    assert_eq!(
        missing["result"]["error"]["code"], "UnknownServer",
        "{missing}"
    );
    assert!(missing["result"]["error"]["message"].is_string());

    assert_ok(
        &shell
            .client
            .rpc("session.attach", json!({"sessionId":shell.sid}))
            .await,
    );
    let text = format!(
        "tool:exec {}",
        json!({"command":"printf bridge-output; while [ ! -f finish ]; do sleep 0.01; done", "runInBackground":true,"timeoutMs":-1})
    );
    assert_ok(
        &shell
            .client
            .rpc(
                "session.submit",
                json!({"sessionId":shell.sid,"branchId":"main","text":text}),
            )
            .await,
    );
    let task = loop {
        let event = shell.client.notice("session.event").await;
        if event["params"]["event"]["type"] == "exec"
            && event["params"]["event"]["name"] == "started"
        {
            break event["params"]["event"]["data"]["task_id"]
                .as_str()
                .unwrap()
                .to_owned();
        }
    };
    session.idle("main").await.unwrap();
    let background = session.service::<BackgroundExec>("main").unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while background.output(task.clone()).await.unwrap() != "bridge-output" {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    for operation in ["output", "outputPage", "wait"] {
        let reply = shell
            .client
            .rpc(
                "session.objectface.call",
                json!({"sessionId":shell.sid,"operation":operation,"taskId":task}),
            )
            .await;
        assert_ok(&reply);
        match operation {
            "output" => assert_eq!(reply["result"], "bridge-output"),
            "outputPage" => assert_eq!(
                reply["result"],
                serde_json::to_value(background.output_page(task.clone(), 0, 64).await.unwrap())
                    .unwrap()
            ),
            "wait" => {
                assert_eq!(reply["result"]["error"]["code"], "timeout", "{reply}");
                assert!(reply["result"]["error"]["message"].is_string());
            }
            _ => unreachable!(),
        }
    }
    let second = shell
        .client
        .rpc("session.create", json!({"cwd":shell.work,"tools":[]}))
        .await;
    assert_ok(&second);
    let second = second["result"]["sessionId"].as_str().unwrap();
    let refused = super::session_typescript::when_served(
        &mut shell.client,
        "session.objectface.call",
        json!({"sessionId":second,"operation":"output","taskId":task}),
    )
    .await;
    assert_ok(&refused);
    assert_eq!(
        refused["result"]["error"]["code"], "unknown",
        "a different Session cannot read this branch's task: {refused}"
    );

    let stopped = shell
        .client
        .rpc(
            "session.objectface.call",
            json!({"sessionId":shell.sid,"operation":"stop","taskId":task}),
        )
        .await;
    assert_ok(&stopped);
    assert_eq!(stopped["result"]["task_id"], task);
    assert_eq!(stopped["result"]["status"], "cancelled");
    let ended = shell
        .client
        .rpc(
            "session.objectface.call",
            json!({"sessionId":shell.sid,"operation":"wait","taskId":task}),
        )
        .await;
    assert_ok(&ended);
    assert_eq!(ended["result"], stopped["result"]);
    shell.shutdown().await;
}

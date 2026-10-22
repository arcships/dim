use super::*;
use dim_agent_capabilities::{host::HostError, tool_registry::definition};
use std::{net::SocketAddr, process::Stdio};
use tokio::{net::TcpListener, process::Command};

struct SessionCleanup {
    entered: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
    fail: bool,
    faults: Arc<std::sync::Mutex<Option<Arc<dim_agent_runtime::FaultReporter>>>>,
    scope: dim_agent_runtime::SessionScope,
}
impl TypedPlugin for SessionCleanup {
    type Deps = InSession<dim_agent_runtime::FaultReporter>;
    fn name(&self) -> &'static str {
        "test.session-cleanup"
    }
    fn apply<'a>(
        &'a self,
        ctx: &'a rutis::Ctx,
        InSession(faults): Self::Deps,
    ) -> rutis::BoxFuture<'a, Result<rutis::Effect, rutis::CordisError>> {
        let entered = self.entered.clone();
        let release = self.release.clone();
        let fail = self.fail;
        *self.faults.lock().unwrap() = Some(faults);
        let result = ctx.effect(|| {
            rutis::Effect::AsyncDisposer(Box::new(move || {
                Box::pin(async move {
                    entered.notify_one();
                    release.notified().await;
                    if fail {
                        Err(rutis::CordisError::ServiceNotFound(
                            "private cleanup detail".into(),
                        ))
                    } else {
                        Ok(())
                    }
                })
            }))
        });
        Box::pin(async move { result.map(|_| rutis::Effect::Done) })
    }
}
impl Capability for SessionCleanup {
    fn keys(&self) -> KeysOf<Self> {
        scoped::<Self::Deps, _>(&self.scope)
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn closed_notification_waits_for_session_resources_and_registry_removal() {
    for (version, ws, fail, fault) in [1, 2].into_iter().flat_map(|version| {
        [false, true].into_iter().flat_map(move |ws| {
            [(false, false), (true, false), (false, true), (true, true)]
                .into_iter()
                .map(move |(fail, fault)| (version, ws, fail, fault))
        })
    }) {
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let mut builder = AgentBuilder::new(AgentConfig::default());
        defaults::install(&mut builder);
        defaults::mock_model_hosts(&mut builder);
        dim_agent_capabilities::defaults::tool_host(&mut builder, Arc::new(MockTools));
        let start = entered.clone();
        let finish = release.clone();
        let faults = Arc::new(std::sync::Mutex::new(None));
        let capture = faults.clone();
        builder.session_plugin(move |scope| SessionCleanup {
            scope: *scope,
            entered: start.clone(),
            release: finish.clone(),
            fail,
            faults: capture.clone(),
        });
        let config = ServerConfig {
            unix_path: Some(std::path::PathBuf::from("/tmp").join(format!(
                "dim-close-test-{}/server.sock",
                uuid::Uuid::new_v4()
            ))),
            ..ServerConfig::default()
        };
        let server = serve(builder, config).await.unwrap();
        let agent = server.agent().unwrap();
        let mut client = Client::new_version(&server, ws, version).await;
        let created = client.rpc("session.create", create(json!(["echo"]))).await;
        assert_ok(&created);
        let sid = created["result"]["sessionId"].as_str().unwrap().to_owned();
        assert_ok(&client.rpc("session.attach", json!({"sessionId":sid})).await);
        let session = agent.get_session(&sid).unwrap();
        if fault {
            faults
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .report(dim_agent_runtime::AgentError::fault(
                    "test host",
                    "private runtime detail",
                ));
        } else {
            drop(session.close());
        }
        tokio::time::timeout(Duration::from_secs(5), entered.notified())
            .await
            .unwrap();
        assert!(agent.get_session(&sid).is_some());
        let mut notice = tokio::spawn(async move {
            let closed = client.notice("session.closed").await;
            (client, closed)
        });
        assert!(
            tokio::time::timeout(Duration::from_millis(100), &mut notice)
                .await
                .is_err(),
            "main closure must not publish Session completion"
        );
        release.notify_one();
        let (mut client, closed) = notice.await.unwrap();
        assert_eq!(closed["params"]["sessionId"], sid);
        if fail {
            assert_eq!(closed["params"]["reason"], "Session cleanup failed");
        } else if fault {
            assert_eq!(closed["params"]["reason"], "Session failed");
        } else {
            assert!(closed["params"].get("reason").is_none());
        }
        assert_eq!(
            client.rpc("session.list", json!({})).await["result"],
            json!([])
        );
        assert_eq!(session.close().await.is_err(), fail);
        server.shutdown();
        server.join().await.unwrap();
    }
}

// The tool owns a real child. The TCP peer observes execution and delays cleanup
// without accessing the Agent or its cancellation token.
struct ProcessTool(SocketAddr);
impl ToolDriver for ProcessTool {
    fn definitions(&self) -> Vec<ToolDefinition> {
        vec![definition(
            "process",
            "Echo text through a child process",
            json!({"type":"object","required":["text"],"properties":{"text":{"type":"string"}}}),
        )]
    }

    fn execute(
        &self,
        call: ToolCall,
        _: ToolExecutionContext,
        cancel: CancellationToken,
    ) -> HostFuture<'_, ToolResult> {
        Box::pin(async move {
            let mut child = Command::new("/bin/cat")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .kill_on_drop(true)
                .spawn()
                .unwrap();
            let pid = child.id().unwrap();
            let mut stdin = child.stdin.take().unwrap();
            let mut stdout = BufReader::new(child.stdout.take().unwrap());
            stdin
                .write_all(format!("{}\n", call.input["text"].as_str().unwrap()).as_bytes())
                .await
                .unwrap();
            let mut output = String::new();
            assert_ne!(stdout.read_line(&mut output).await.unwrap(), 0);
            let mut peer = BufReader::new(TcpStream::connect(self.0).await.unwrap());
            peer.get_mut()
                .write_all(
                    format!("{}\n", json!({"phase":"started","pid":pid,"output":output}))
                        .as_bytes(),
                )
                .await
                .unwrap();
            let mut command = String::new();
            let cancelled = tokio::select! {
                _ = cancel.cancelled() => true,
                result = peer.read_line(&mut command) => {
                    assert_ne!(result.unwrap(), 0);
                    assert_eq!(command, "finish\n");
                    false
                }
            };
            let status = if cancelled {
                peer.get_mut()
                    .write_all(b"{\"phase\":\"cleaning\"}\n")
                    .await
                    .unwrap();
                // Hold the child alive until the external peer releases cleanup.
                command.clear();
                assert_ne!(peer.read_line(&mut command).await.unwrap(), 0);
                assert_eq!(command, "release\n");
                child.start_kill().unwrap();
                child.wait().await.unwrap()
            } else {
                drop(stdin);
                child.wait().await.unwrap()
            };
            peer.get_mut()
                .write_all(
                    format!(
                        "{}\n",
                        json!({"phase":"cleaned","pid":pid,"success":status.success()})
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
            if cancelled {
                Err(HostError::Cancelled("Process cancelled and reaped".into()))
            } else {
                assert!(status.success());
                Ok(ToolResult::text(output))
            }
        })
    }
}

async fn phase(peer: &mut BufReader<TcpStream>, expected: &str) -> Value {
    let mut line = String::new();
    assert_ne!(peer.read_line(&mut line).await.unwrap(), 0);
    let value: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(value["phase"], expected);
    eprintln!("tool -> {value}");
    value
}

fn process_exists(pid: u64) -> bool {
    std::process::Command::new("/bin/kill")
        .args(["-0", &pid.to_string()])
        .stderr(Stdio::null())
        .status()
        .unwrap()
        .success()
}

async fn start_tool(
    client: &mut Client,
    listener: &TcpListener,
    sid: &Value,
    text: &str,
) -> (BufReader<TcpStream>, u64) {
    eprintln!("input -> {text}");
    assert_ok(
        &client
            .rpc(
                "session.submit",
                json!({"sessionId":sid,"text":format!("tool:process {}", json!({"text":text}))}),
            )
            .await,
    );
    let mut peer = BufReader::new(listener.accept().await.unwrap().0);
    let started = phase(&mut peer, "started").await;
    let pid = started["pid"].as_u64().unwrap();
    assert_eq!(started["output"], format!("{text}\n"));
    assert!(process_exists(pid), "child must be alive before control");
    (peer, pid)
}

async fn finish_tool(peer: &mut BufReader<TcpStream>, pid: u64, cancelled: bool) {
    peer.get_mut()
        .write_all(if cancelled { b"release\n" } else { b"finish\n" })
        .await
        .unwrap();
    let cleaned = phase(peer, "cleaned").await;
    assert_eq!(cleaned["pid"], pid);
    assert_eq!(cleaned["success"], !cancelled);
    assert!(
        !process_exists(pid),
        "child must be reaped before completion"
    );
    assert_eq!(peer.read_line(&mut String::new()).await.unwrap(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn running_tool_stop_and_close_survive_disconnect_during_cleanup() {
    tokio::time::timeout(Duration::from_secs(30), async {
        for ws in [false, true] {
            eprintln!("origin -> {}", if ws { "WebSocket" } else { "Unix" });
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let server = launch(
                Arc::new(ProcessTool(listener.local_addr().unwrap())),
                ServerConfig::default(),
            )
            .await;
            let mut origin = Client::new(&server, ws).await;
            let mut observer = Client::new(&server, !ws).await;
            let mut params = create(json!(["process"]));
            params["permissionPolicy"] = json!({"allowedTools":["process"]});
            let created = origin.rpc("session.create", params).await;
            assert_ok(&created);
            let sid = created["result"]["sessionId"].clone();
            assert_ok(
                &observer
                    .rpc("session.attach", json!({"sessionId":sid}))
                    .await,
            );

            // Establish normal execution, real stdout, process exit and model continuation.
            let (mut peer, pid) = start_tool(&mut origin, &listener, &sid, "NORMAL").await;
            finish_tool(&mut peer, pid, false).await;
            assert_ok(&origin.rpc("session.idle", json!({"sessionId":sid})).await);
            let session = server
                .agent()
                .unwrap()
                .get_session(sid.as_str().unwrap())
                .unwrap();
            let events = v1_events(&session.history("main", None).await.unwrap(), 0);
            let results: Vec<_> = events
                .iter()
                .filter(|e| e["name"] == "tool-result")
                .collect();
            assert_eq!(results.len(), 1);
            assert_eq!(
                results[0]["data"]["results"][0]["result"]["content"],
                "NORMAL\n"
            );
            assert!(events.iter().any(|e| e["type"] == "step"
                && e["name"] == "text"
                && e["data"]["delta"] == "Tool result: NORMAL\n"));
            let normal_high = session.sequence("main").unwrap();

            let (mut peer, pid) = start_tool(&mut origin, &listener, &sid, "STOP").await;
            eprintln!("control -> stop");
            let stop = origin.send("session.stop", json!({"sessionId":sid})).await;
            let mut stopped = tokio::spawn(async move {
                let response = origin.response(stop).await;
                (origin, response)
            });
            phase(&mut peer, "cleaning").await;
            assert!(process_exists(pid));
            assert!(session.state("main").unwrap().stopping);
            assert!(
                !v1_events(&session.history("main", None).await.unwrap(), normal_high)
                    .iter()
                    .any(|e| e["name"] == "tool-result")
            );
            assert!(
                tokio::time::timeout(Duration::from_millis(100), &mut stopped)
                    .await
                    .is_err(),
                "stop returned while child cleanup was held"
            );
            eprintln!("control -> stop is pending while cleanup is held");
            finish_tool(&mut peer, pid, true).await;
            let (mut origin, response) = stopped.await.unwrap();
            assert_ok(&response);
            assert_ok(&origin.rpc("session.idle", json!({"sessionId":sid})).await);
            eprintln!("control -> stop completed after process exit");
            let events = v1_events(&session.history("main", None).await.unwrap(), normal_high);
            assert_eq!(
                events
                    .iter()
                    .filter(|e| e["type"] == "model" && e["name"] == "call")
                    .count(),
                1,
                "cancelled tool must not start model continuation"
            );
            assert!(events.iter().any(|e| e["type"] == "turn"
                && e["name"] == "end"
                && e["data"]["status"] == "cancelled"));

            assert_ok(
                &origin
                    .rpc(
                        "session.submit",
                        json!({"sessionId":sid,"text":"RECOVERED"}),
                    )
                    .await,
            );
            assert_ok(&origin.rpc("session.idle", json!({"sessionId":sid})).await);
            let session = server
                .agent()
                .unwrap()
                .get_session(sid.as_str().unwrap())
                .unwrap();
            let events = v1_events(&session.history("main", None).await.unwrap(), 0);
            let results: Vec<_> = events
                .iter()
                .filter(|e| e["name"] == "tool-result")
                .collect();
            assert_eq!(results.len(), 2);
            assert_eq!(results[1]["data"]["results"][0]["result"]["isError"], true);
            assert_eq!(
                results[1]["data"]["results"][0]["result"]["content"],
                "Process cancelled and reaped"
            );
            assert!(events.iter().any(|e| e["type"] == "step"
                && e["name"] == "text"
                && e["data"]["delta"] == "Echo: RECOVERED"));

            let (mut peer, pid) = start_tool(&mut origin, &listener, &sid, "CLOSE").await;
            eprintln!("control -> close");
            origin.send("session.close", json!({"sessionId":sid})).await;
            phase(&mut peer, "cleaning").await;
            assert!(process_exists(pid));
            drop(origin);
            loop {
                let ping = observer.rpc("daemon.ping", json!({})).await;
                assert_ok(&ping);
                if ping["result"]["connections"] == 1 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            assert!(
                process_exists(pid),
                "origin disconnected before child cleanup"
            );
            assert!(
                !observer
                    .notices
                    .iter()
                    .any(|v| v["method"] == "session.closed"),
                "closed before cleanup"
            );
            eprintln!("control -> origin disconnected while child is still alive");
            let mut closing = tokio::spawn(async move {
                let notice = observer.notice("session.closed").await;
                (observer, notice)
            });
            assert!(
                tokio::time::timeout(Duration::from_millis(100), &mut closing)
                    .await
                    .is_err(),
                "session.closed arrived while child cleanup was held"
            );
            finish_tool(&mut peer, pid, true).await;
            let (mut observer, closed) = closing.await.unwrap();
            assert_eq!(closed["params"]["sessionId"], sid);
            eprintln!("notification -> {closed}");
            assert_eq!(
                observer.rpc("session.list", json!({})).await["result"],
                json!([])
            );
            let rejected = observer
                .rpc("session.submit", json!({"sessionId":sid,"text":"late"}))
                .await;
            assert_eq!(rejected["error"]["data"]["appError"], "session_closed");
            // The close settled the third call before the Session ended.
            let events: Vec<_> = session
                .history("main", None)
                .await
                .unwrap()
                .iter()
                .map(|event| serde_json::to_value(&**event).unwrap())
                .collect();
            let results: Vec<_> = events
                .iter()
                .filter(|e| e["name"] == "tool-result")
                .collect();
            assert_eq!(results.len(), 3);
            let calls: Vec<_> = events.iter().filter(|e| e["name"] == "tool-call").collect();
            assert_eq!(calls.len(), 3);
            for (call, result) in calls.iter().zip(&results) {
                assert_eq!(call["data"]["calls"].as_array().unwrap().len(), 1);
                assert_eq!(result["data"]["results"].as_array().unwrap().len(), 1);
                assert_eq!(
                    call["data"]["calls"][0]["id"],
                    result["data"]["results"][0]["call_id"]
                );
            }
            assert_eq!(results[2]["data"]["results"][0]["result"]["isError"], true);
            assert_ok(
                &observer
                    .rpc("daemon.shutdown", json!({"force":false}))
                    .await,
            );
            let path = server.unix_path.clone().unwrap();
            assert_eq!(server.join().await.unwrap(), ShutdownReason::Graceful);
            assert!(!path.exists());
            std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
        }
    })
    .await
    .expect("tool cleanup protocol flow completed");
}

use super::*;
use rutis::host::CtxHost;

fn v2_create() -> Value {
    json!({
        "cwd": std::env::temp_dir(),
        "tools": ["branch"],
        "permissionPolicy": {"allowedTools": ["branch"]}
    })
}

fn error_name(value: &Value) -> &str {
    value["error"]["data"]["appError"].as_str().unwrap()
}

/// Holds the key of its Session's ToolRegistry, which it reads undeclared.
struct FailChildBuild(
    rutis::host::Key<dim_agent_capabilities::tool_registry::ToolRegistry>,
    rutis::host::Key<dim_agent_runtime::TimelineIdentity>,
);
impl FailChildBuild {
    fn new(
        names: &dim_agent_runtime::Names<'_>,
        a0: rutis::host::Key<dim_agent_capabilities::tool_registry::ToolRegistry>,
    ) -> rutis::Result<Self> {
        Ok(Self(a0, names.key()?))
    }
}
impl rutis::host::DynPlugin for FailChildBuild {
    fn name(&self) -> &str {
        "test.fail-child-build /private/config token=test-secret"
    }
    fn injects(&self) -> Vec<rutis::host::TypeKey> {
        vec![self.1.erase()]
    }
    fn apply<'a>(
        &'a self,
        ctx: &'a rutis::Ctx,
    ) -> rutis::host::BoxFuture<'a, rutis::Result<rutis::host::Effect>> {
        Box::pin(async move {
            let identity = ctx.require_at(&self.1)?;
            if identity.branch_id != "main" {
                let _ = ctx.get_at(&self.0);
            }
            Ok(rutis::host::Effect::Done)
        })
    }
}

#[cfg_attr(not(debug_assertions), ignore)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_branch_build_keeps_private_details_off_both_transports() {
    for ws in [false, true] {
        let mut builder = AgentBuilder::new(AgentConfig::default());
        builder.timeline_store(Arc::new(dim_agent_runtime::MemoryTimelineStore::default()));
        defaults::install(&mut builder);
        defaults::mock_model_hosts(&mut builder);
        dim_agent_capabilities::defaults::tool_host(&mut builder, Arc::new(MockTools));
        builder.require_feature(
            Feature::new("test.fail-child-build").branch("test.fail-child-build", |names| {
                Ok(Box::new(FailChildBuild::new(names, names.key()?)?))
            }),
        );
        let server = serve(
            builder,
            ServerConfig {
                unix_path: Some(std::path::PathBuf::from("/tmp").join(format!(
                    "dim-build-error-{}/server.sock",
                    uuid::Uuid::new_v4()
                ))),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let mut client = Client::new_version(&server, ws, 2).await;
        let created = client.rpc("session.create", v2_create()).await;
        assert_ok(&created);
        let sid = created["result"]["sessionId"].as_str().unwrap();
        let session = server.agent().unwrap().get_session(sid).unwrap();
        let attached = client.rpc("session.attach", json!({"sessionId":sid})).await;
        assert_eq!(attached["result"], json!({}));
        let failed = client
            .rpc(
                "session.branch_create",
                json!({
                    "sessionId":sid,"parentBranchId":"main","at":0,"prompt":"child"
                }),
            )
            .await;
        assert_eq!(error_name(&failed), "session_build_failed");
        assert_eq!(failed["error"]["message"], "Session initialization failed");
        let failure = loop {
            let notice = client.notice("session.event").await;
            let entry = &notice["params"];
            if entry["event"]["type"] == "branch" && entry["event"]["name"] == "create-failed" {
                break entry.clone();
            }
        };
        assert_eq!(failure["branchId"], "main");
        assert_eq!(failure["event"]["data"]["error"], "branch creation failed");
        let failures = [failure];
        let visible = std::iter::once(&failed)
            .chain(&failures)
            .chain(&client.notices)
            .map(Value::to_string);
        for encoded in visible {
            for private in ["/private/config", "test-secret", "test.fail-child-build"] {
                assert!(!encoded.contains(private), "{encoded}");
            }
        }
        assert_eq!(
            session.branches().len(),
            1,
            "a failed candidate is not listed"
        );
        assert_ok(&client.rpc("session.close", json!({"sessionId":sid})).await);
        let history = session.history("main", None).await.unwrap();
        assert!(history.iter().any(|event| matches!(&event.kind,
            dim_agent_types::EventKind::BranchCreateFailed(
                dim_agent_types::BranchCreateFailedData::Failed { branch, create_event, .. }
            ) if *branch == failures[0]["event"]["data"]["branch"]
                && *create_event == failures[0]["event"]["data"]["createEvent"])));
        server.shutdown();
        server.join().await.unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn v2_tree_history_resume_and_protocol_isolation_work_on_both_transports() {
    for ws in [false, true] {
        let (server, agent) = launch_with_agent(Arc::new(MockTools), ServerConfig::default()).await;
        let mut tree = Client::new_version(&server, ws, 2).await;
        let mut legacy = Client::new(&server, !ws).await;

        let unbound = agent
            .create_session(dim_agent_runtime::SessionConfig::default())
            .await
            .unwrap();
        for client in [&mut tree, &mut legacy] {
            assert_eq!(
                error_name(
                    &client
                        .rpc("session.attach", json!({"sessionId":unbound.id()}))
                        .await
                ),
                "session_protocol_version"
            );
            assert_eq!(
                error_name(
                    &client
                        .rpc("session.close", json!({"sessionId":unbound.id()}))
                        .await
                ),
                "session_protocol_version"
            );
        }
        assert!(agent.get_session(unbound.id()).is_some());
        unbound.close().await.unwrap();

        let created = tree.rpc("session.create", v2_create()).await;
        assert_ok(&created);
        let sid = created["result"]["sessionId"].as_str().unwrap();
        let session = agent.get_session(sid).unwrap();

        for (method, params) in [
            ("session.submit", json!({"sessionId":sid,"text":"parent"})),
            ("session.stop", json!({"sessionId":sid})),
            ("session.idle", json!({"sessionId":sid})),
        ] {
            assert_eq!(tree.rpc(method, params).await["error"]["code"], -32602);
        }
        assert_eq!(
            legacy.rpc("session.list", json!({})).await["result"],
            json!([])
        );
        assert_eq!(
            error_name(&legacy.rpc("session.attach", json!({"sessionId":sid})).await),
            "session_protocol_version"
        );

        assert_ok(
            &tree
                .rpc(
                    "session.submit",
                    json!({
                        "sessionId":sid, "branchId":"main", "text":"parent"
                    }),
                )
                .await,
        );
        let idle = tree
            .rpc(
                "session.idle",
                json!({
                    "sessionId":sid, "branchId":"main"
                }),
            )
            .await;
        assert_ok(&idle);
        let at = idle["result"]["sequence"].as_u64().unwrap();
        assert_eq!(at, session.sequence("main").unwrap());
        let attached = tree.rpc("session.attach", json!({"sessionId":sid})).await;
        assert_eq!(attached["result"], json!({}), "attach returns no history");
        assert_eq!(
            tree.rpc("session.attach", json!({"sessionId":sid,"after":1}))
                .await["error"]["data"]["appError"],
            "invalid_params"
        );
        let state = tree.notice("session.state").await;
        assert_eq!(state["params"]["branchId"], "main");

        let child = tree
            .rpc(
                "session.branch_create",
                json!({
                    "sessionId":sid, "parentBranchId":"main", "at":at, "prompt":"child"
                }),
            )
            .await;
        assert_ok(&child);
        let child_id = child["result"]["branch"]["branchId"].as_str().unwrap();
        assert_ne!(child_id, "main");
        assert_eq!(child["result"]["branch"]["parentBranchId"], "main");
        assert!(child["result"]["branch"]["branchPoint"].is_number());
        assert!(child["result"]["branch"]["createEvent"].is_number());

        assert_eq!(
            tree.rpc(
                "session.submit",
                json!({
                    "sessionId":sid, "branchId":"missing", "text":"must not reach main"
                })
            )
            .await["error"]["data"]["appError"],
            "session_invalid_input"
        );
        assert_ok(
            &tree
                .rpc(
                    "session.submit",
                    json!({
                        "sessionId":sid, "branchId":child_id, "text":"child input"
                    }),
                )
                .await,
        );
        assert_ok(
            &tree
                .rpc(
                    "session.idle",
                    json!({
                        "sessionId":sid, "branchId":child_id
                    }),
                )
                .await,
        );

        let child_end = session.sequence(child_id).unwrap();
        loop {
            let notice = tree.notice("session.event").await;
            let entry = &notice["params"];
            let sequence = entry["event"]["sequence"].as_u64().unwrap();
            if entry["branchId"] == "main" {
                assert!(sequence > at, "events before attach are not replayed");
            } else {
                assert_eq!(entry["branchId"], child_id);
                if sequence == child_end {
                    break;
                }
            }
        }

        assert_ok(&tree.rpc("session.close", json!({"sessionId":sid})).await);
        let history = session.history("main", None).await.unwrap();
        assert!(history.iter().any(|event| matches!(&event.kind,
            dim_agent_types::EventKind::BranchCreate(data) if data.branch == child_id
                && matches!(&data.source,
                    dim_agent_types::BranchSource::Host { operation_id } if !operation_id.is_empty()))));
        server.shutdown();
        server.join().await.unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn host_branch_creation_publishes_the_anchor_before_the_child_start() {
    for ws in [false, true] {
        let (server, agent) = launch_with_agent(Arc::new(MockTools), ServerConfig::default()).await;
        let mut observer = Client::new_version(&server, ws, 2).await;
        let created = observer.rpc("session.create", v2_create()).await;
        assert_ok(&created);
        let sid = created["result"]["sessionId"].as_str().unwrap();
        let session = agent.get_session(sid).unwrap();
        assert_ok(
            &observer
                .rpc("session.attach", json!({"sessionId":sid}))
                .await,
        );
        let mut creator = Client::new_version(&server, !ws, 2).await;
        let child = creator
            .rpc(
                "session.branch_create",
                json!({"sessionId":sid,"parentBranchId":"main","at":0,"prompt":"child"}),
            )
            .await;
        assert_ok(&child);
        let child_id = child["result"]["branch"]["branchId"].as_str().unwrap();
        assert_eq!(session.branches().len(), 2);
        let (mut anchor, mut start) = (None, None);
        while start.is_none() {
            let notice = observer.notice("session.event").await;
            let entry = &notice["params"];
            let (kind, name) = (&entry["event"]["type"], &entry["event"]["name"]);
            assert!(!(entry["branchId"] == "main" && kind == "step"));
            if entry["branchId"] == "main" && kind == "branch" && name == "create" {
                anchor = Some(entry.clone());
            }
            if entry["branchId"] == child_id && kind == "branch" && name == "start" {
                assert!(
                    anchor.is_some(),
                    "the parent anchor precedes the child start"
                );
                start = Some(entry.clone());
            }
        }
        let (anchor, start) = (anchor.unwrap(), start.unwrap());
        assert_eq!(anchor["event"]["data"]["source"]["kind"], "host");
        assert_eq!(start["event"]["sequence"], 1);
        assert_eq!(
            start["event"]["data"]["createEvent"],
            anchor["event"]["sequence"]
        );
        assert!(
            observer
                .notices
                .iter()
                .all(|notice| notice["method"] != "approval.request")
        );
        assert_ok(
            &observer
                .rpc("session.close", json!({"sessionId":sid}))
                .await,
        );
        server.shutdown();
        server.join().await.unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn builtin_branch_is_a_session_tool_under_the_permission_policy() {
    for ws in [false, true] {
        let (server, agent) = launch_with_agent(Arc::new(MockTools), ServerConfig::default()).await;
        let mut client = Client::new_version(&server, ws, 2).await;
        for policy in [
            json!({"preset":"full-access","disallowedTools":["branch"]}),
            json!({"preset":"read-only"}),
        ] {
            let denied = policy["preset"] == "full-access";
            let created = client.rpc("session.create", json!({"cwd":std::env::temp_dir(),"tools":["branch"],"permissionPolicy":policy})).await;
            assert_ok(&created);
            let sid = created["result"]["sessionId"].as_str().unwrap();
            let session = agent.get_session(sid).unwrap();
            assert_ok(&client.rpc("session.attach", json!({"sessionId":sid})).await);
            assert_ok(&client.rpc("session.submit", json!({"sessionId":sid,"branchId":"main","text":"tool:branch {\"action\":\"create\",\"prompt\":\"child\"}"})).await);
            assert_ok(
                &client
                    .rpc("session.idle", json!({"sessionId":sid,"branchId":"main"}))
                    .await,
            );
            assert_eq!(session.branches().len(), if denied { 1 } else { 2 });
            assert!(
                client
                    .notices
                    .iter()
                    .all(|notice| notice["method"] != "approval.request")
            );
            let result = session
                .history("main", None)
                .await
                .unwrap()
                .into_iter()
                .find_map(|event| match &event.kind {
                    dim_agent_types::EventKind::ToolResult(data) => {
                        Some(data.results[0].result.clone())
                    }
                    _ => None,
                })
                .unwrap();
            assert_eq!(result.is_error == Some(true), denied);
            assert_ok(&client.rpc("session.close", json!({"sessionId":sid})).await);
        }
        server.shutdown();
        server.join().await.unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tree_attach_forwards_later_events_of_every_branch_in_branch_order() {
    for ws in [false, true] {
        let (server, agent) = launch_with_agent(Arc::new(MockTools), ServerConfig::default()).await;
        let mut control = Client::new_version(&server, !ws, 2).await;
        let created = control.rpc("session.create", v2_create()).await;
        assert_ok(&created);
        let sid = created["result"]["sessionId"].as_str().unwrap();
        let session = agent.get_session(sid).unwrap();
        session
            .submit(
                "before attach".into(),
                dim_agent_types::InputMode::Queue,
                "main",
            )
            .await
            .unwrap();
        session.idle("main").await.unwrap();
        let high = session.sequence("main").unwrap();
        let mut client = Client::new_version(&server, ws, 2).await;
        let attach = client.rpc("session.attach", json!({"sessionId":sid})).await;
        assert_eq!(attach["result"], json!({}), "attach returns no history");

        let child = control
            .rpc(
                "session.branch_create",
                json!({"sessionId":sid,"parentBranchId":"main","at":0,"prompt":"child"}),
            )
            .await;
        assert_ok(&child);
        let child_id = child["result"]["branch"]["branchId"].as_str().unwrap();
        session.idle(child_id).await.unwrap();
        let grandchild = control
            .rpc(
                "session.branch_create",
                json!({"sessionId":sid,"parentBranchId":child_id,"at":0,"prompt":"grandchild"}),
            )
            .await;
        assert_ok(&grandchild);
        let grandchild_id = grandchild["result"]["branch"]["branchId"].as_str().unwrap();
        session.idle(grandchild_id).await.unwrap();
        let stop = control
            .rpc(
                "session.stop",
                json!({"sessionId":sid,"branchId":grandchild_id}),
            )
            .await;
        assert_ok(&stop);
        let end = session.sequence(grandchild_id).unwrap();
        assert_eq!(stop["result"], json!({"sequence": end}));

        let mut last = std::collections::HashMap::<String, u64>::new();
        let mut first = std::collections::HashMap::<String, Value>::new();
        loop {
            let notice = client.notice("session.event").await;
            let params = &notice["params"];
            let branch = params["branchId"].as_str().unwrap().to_owned();
            let sequence = params["event"]["sequence"].as_u64().unwrap();
            if branch == "main" {
                assert!(sequence > high, "events before attach are not replayed");
            }
            if let Some(previous) = last.insert(branch.clone(), sequence) {
                assert!(sequence > previous, "branch order is kept");
            }
            first
                .entry(branch.clone())
                .or_insert_with(|| params["event"].clone());
            if branch == grandchild_id && sequence == end {
                assert_eq!(params["event"]["type"], "stop");
                break;
            }
        }
        for branch in [child_id, grandchild_id] {
            let start = &first[branch];
            assert_eq!(
                (&start["type"], &start["name"], &start["sequence"]),
                (&json!("branch"), &json!("start"), &json!(1))
            );
        }
        server.shutdown();
        server.join().await.unwrap();
    }
}

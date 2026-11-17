use super::*;
use dim_agent_capabilities::approval::{AdmitCall, Approval};
use dim_agent_capabilities::permission::{CommandRulesHost, ReadCommandRules};
use dim_agent_capabilities::tool_executor::ToolExecutor;
use dim_agent_runtime::testing::{branch_events, chain_events};
use dim_agent_runtime::{Dispatcher, Session, Timeline, TimelineKey, TimelineStore};
use dim_agent_types::{
    ApprovalDecision, ContentBlock, EventKind, InputMode, InterruptScope, StopInterruptData,
    TurnOutcome,
};
use rutis::host::CtxHost;
use std::sync::{Mutex as StdMutex, atomic::AtomicBool};
use tokio::sync::Notify;

const GATE_INPUT: &str = "approval dispatch gate";

#[derive(Default)]
struct Probe {
    dispatcher: StdMutex<Option<Arc<Dispatcher>>>,
    timeline: StdMutex<Option<Arc<Timeline>>>,
    entered: Notify,
    release: Notify,
    stop_before_request: AtomicBool,
    decision: StdMutex<Option<ApprovalDecision>>,
    invalid_request: AtomicBool,
    invalid_response: AtomicBool,
    tools: ToolReload,
    deny_on_policy_change: StdMutex<Option<String>>,
    cancel_gate: StdMutex<Option<std::sync::mpsc::Receiver<()>>>,
    cancel_entered: Notify,
    command_rules: StdMutex<Vec<dim_agent_types::CommandRule>>,
}
impl ReadCommandRules for Probe {
    fn rules(&self, _: CancellationToken) -> HostFuture<'_, Vec<dim_agent_types::CommandRule>> {
        let rules = self.command_rules.lock().unwrap().clone();
        Box::pin(async move { Ok(rules) })
    }
}
struct ProbePlugin(
    Arc<Probe>,
    rutis::host::Key<Timeline>,
    rutis::host::Key<Dispatcher>,
    rutis::host::Key<ToolExecutor>,
);
impl ProbePlugin {
    fn new(names: &dim_agent_runtime::Names<'_>, a0: Arc<Probe>) -> rutis::Result<Self> {
        Ok(Self(a0, names.key()?, names.key()?, names.key()?))
    }
}
impl rutis::host::DynPlugin for ProbePlugin {
    fn name(&self) -> &str {
        "approval-transport-probe"
    }
    fn injects(&self) -> Vec<rutis::host::TypeKey> {
        vec![self.1.erase(), self.2.erase(), self.3.erase()]
    }
    fn apply<'a>(
        &'a self,
        ctx: &'a rutis::Ctx,
    ) -> rutis::host::BoxFuture<'a, rutis::Result<rutis::host::Effect>> {
        Box::pin(async move {
            let timeline = ctx.require_at(&self.1)?;
            let dispatcher = ctx.require_at(&self.2)?;
            *self.0.timeline.lock().unwrap() = Some(timeline.clone());
            *self.0.dispatcher.lock().unwrap() = Some(dispatcher.clone());
            let probe = self.0.clone();
            timeline.before(
                ctx,
                "approval-test-final-fields",
                &["approval/request", "approval/respond", "approval/cancelled"],
                move |mut draft| {
                    match &mut draft.kind {
                        EventKind::ApprovalCancelled(_) => {
                            let gate = probe.cancel_gate.lock().unwrap().take();
                            if let Some(release) = gate {
                                probe.cancel_entered.notify_one();
                                tokio::task::block_in_place(|| release.recv()).ok();
                            }
                        }
                        EventKind::ApprovalRequest(data) => {
                            data.reason = "Reviewed approval reason".into();
                            if probe.invalid_request.load(Ordering::SeqCst) {
                                data.call.input.insert("rewritten".into(), json!(true));
                            }
                        }
                        EventKind::ApprovalRespond(data) => {
                            if let Some(decision) = *probe.decision.lock().unwrap() {
                                data.decision = decision;
                            }
                            if probe.invalid_response.load(Ordering::SeqCst) {
                                data.approval_id = "another-approval".into();
                            }
                        }
                        _ => {}
                    }
                    Ok(draft)
                },
            )?;
            let probe = self.0.clone();
            dispatcher.subscribe(
                ctx,
                "approval-test-gate",
                &["input/message", "step/tool-call", "permission/changed"],
                move |event| {
                    let gate = match &event.kind {
                        EventKind::InputMessage(data) => matches!(
                            data.user.as_ref().map(|user| user.content.as_slice()),
                            Some([ContentBlock::Text { text }]) if text == GATE_INPUT
                        ),
                        EventKind::ToolCall(_) => {
                            probe.stop_before_request.swap(false, Ordering::SeqCst)
                        }
                        EventKind::PermissionChanged(_) => {
                            if let Some(approval_id) =
                                probe.deny_on_policy_change.lock().unwrap().take()
                            {
                                timeline
                                    .append(
                                        EventKind::ApprovalRespond(
                                            dim_agent_types::ApprovalRespondData {
                                                approval_id,
                                                decision: ApprovalDecision::Deny,
                                            },
                                        )
                                        .into(),
                                    )
                                    .unwrap();
                                true
                            } else {
                                false
                            }
                        }
                        _ => false,
                    };
                    if gate && matches!(event.kind, EventKind::ToolCall(_)) {
                        timeline
                            .append(
                                EventKind::StopInterrupt(StopInterruptData {
                                    reason: "Stop before the approval request".into(),
                                    scope: InterruptScope::Turn,
                                })
                                .into(),
                            )
                            .unwrap();
                    }
                    let probe = probe.clone();
                    Box::pin(async move {
                        if gate {
                            probe.entered.notify_one();
                            probe.release.notified().await;
                        }
                        Ok(())
                    })
                },
            )?;
            Ok(rutis::host::Effect::Done)
        })
    }
}

async fn setup(ws: bool, probe: Arc<Probe>) -> (RunningServer, Session, Client, Arc<AtomicUsize>) {
    setup_version(ws, probe, 1).await
}
async fn setup_version(
    ws: bool,
    probe: Arc<Probe>,
    version: u32,
) -> (RunningServer, Session, Client, Arc<AtomicUsize>) {
    setup_store(
        ws,
        probe,
        version,
        Arc::new(dim_agent_runtime::MemoryTimelineStore::default()),
    )
    .await
}
async fn setup_store(
    ws: bool,
    probe: Arc<Probe>,
    version: u32,
    store: Arc<dyn dim_agent_runtime::TimelineStore>,
) -> (RunningServer, Session, Client, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let mut builder = AgentBuilder::new(AgentConfig::default());
    builder.timeline_store(store);
    builder
        .session_value(Arc::new(dim_agent_capabilities::host::PromptHost(
            Arc::new(dim_agent_capabilities::host::EmptyPromptHost),
        )))
        .session_value(Arc::new(
            dim_agent_capabilities::prompt::InitialPromptConfiguration::fixed(None),
        ))
        .session_value(Arc::new(CommandRulesHost(probe.clone())));
    defaults::install_with_prompt_capability(
        &mut builder,
        dim_agent_capabilities::model_provider::ModelProviderPlugin::new,
    );
    defaults::mock_model_hosts(&mut builder);
    probe.tools.install(
        &mut builder,
        Arc::new(ConfirmTool {
            calls: calls.clone(),
            seen: Arc::new(Mutex::new(vec![])),
        }),
    );
    builder.require_feature(
        Feature::new("approval-transport-probe").branch("approval-transport-probe", move |names| {
            Ok(Box::new(ProbePlugin::new(names, probe.clone())?))
        }),
    );
    let server = serve(
        builder,
        ServerConfig {
            unix_path: Some(
                std::path::PathBuf::from("/tmp")
                    .join(format!("dim-approval-{}/s", uuid::Uuid::new_v4())),
            ),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let agent = server.agent().unwrap();
    let mut client = Client::new_version(&server, ws, version).await;
    let mut params = create(json!(["confirm"]));
    params["permissionPolicy"] = json!({"preset":"workspace-write"});
    let created = client.rpc("session.create", params).await;
    assert_ok(&created);
    let session = agent
        .get_session(created["result"]["sessionId"].as_str().unwrap())
        .unwrap();
    assert_ok(
        &client
            .rpc("session.attach", json!({"sessionId":session.id()}))
            .await,
    );
    (server, session, client, calls)
}
#[tokio::test]
async fn v2_approval_replays_committed_fields_and_ends_a_request_stopped_before_dispatch() {
    for ws in [false, true] {
        let probe = Arc::new(Probe::default());
        probe.stop_before_request.store(true, Ordering::SeqCst);
        probe.invalid_request.store(true, Ordering::SeqCst);
        let (server, session, mut client, calls) = setup_version(ws, probe.clone(), 2).await;
        session
            .submit("tool:confirm {}".into(), InputMode::Queue, "main")
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), probe.entered.notified())
            .await
            .unwrap();
        wait_events(&session, "approval/request", 1).await;
        probe.release.notify_one();
        session.idle("main").await.unwrap();
        session.close().await.unwrap();
        client.notice("session.closed").await;
        let approvals: Vec<_> = client
            .notices
            .iter()
            .filter(|notice| {
                matches!(
                    notice["method"].as_str(),
                    Some("approval.request" | "approval.cancelled")
                )
            })
            .collect();
        assert_eq!(approvals.len(), 2, "{approvals:?}");
        assert_eq!(approvals[0]["method"], "approval.request");
        assert_eq!(approvals[1]["method"], "approval.cancelled");
        assert_eq!(
            approvals[0]["params"]["requestId"],
            approvals[1]["params"]["requestId"]
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        server.shutdown();
        server.join().await.unwrap();

        let probe = Arc::new(Probe::default());
        probe.invalid_request.store(true, Ordering::SeqCst);
        let (server, session, mut client, _) = setup_version(ws, probe, 2).await;
        session
            .submit("tool:confirm {}".into(), InputMode::Queue, "main")
            .await
            .unwrap();
        let live = client.notice("approval.request").await;
        assert_eq!(live["params"]["reason"], "Reviewed approval reason");
        assert_eq!(live["params"]["call"]["input"]["rewritten"], true);
        let mut reconnect = Client::new_version(&server, !ws, 2).await;
        assert_ok(
            &reconnect
                .rpc("session.attach", json!({"sessionId":session.id()}))
                .await,
        );
        assert_eq!(reconnect.notice("approval.request").await, live);
        session.close().await.unwrap();
        server.shutdown();
        server.join().await.unwrap();
    }
}

async fn request(client: &mut Client, session: &Session) -> Value {
    assert_ok(
        &client
            .rpc(
                "session.submit",
                json!({"sessionId":session.id(),"text":"tool:confirm {}"}),
            )
            .await,
    );
    let notice = client.notice("approval.request").await;
    assert_eq!(notice["params"]["reason"], "Reviewed approval reason");
    notice["params"]["requestId"].clone()
}
async fn request_v2(client: &mut Client, session: &Session) -> Value {
    assert_ok(
        &client
            .rpc(
                "session.submit",
                json!({"sessionId":session.id(),"branchId":"main","text":"tool:confirm {}"}),
            )
            .await,
    );
    let notice = client.notice("approval.request").await;
    assert_eq!(notice["params"]["branchId"], "main");
    notice["params"]["requestId"].clone()
}
async fn gate(session: &Session, probe: &Probe) {
    session
        .submit(GATE_INPUT.into(), InputMode::Append, "main")
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), probe.entered.notified())
        .await
        .unwrap();
}
async fn wait_events(session: &Session, key: &str, count: usize) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if chain_events(session, "main", ..)
                .await
                .unwrap()
                .iter()
                .filter(|e| e.kind.key() == key)
                .count()
                >= count
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("event was submitted");
}
async fn result_count(session: &Session) -> usize {
    chain_events(session, "main", ..)
        .await
        .unwrap()
        .iter()
        .filter(|event| matches!(event.kind, EventKind::ToolResult(_)))
        .count()
}

#[tokio::test]
async fn history_waits_for_its_fixed_batch_boundary_without_following_new_input() {
    let probe = Arc::new(Probe::default());
    let (server, session, mut client, _) = setup_version(false, probe.clone(), 3).await;
    let timeline = probe.timeline.lock().unwrap().clone().unwrap();
    let batch = timeline
        .append_batch_registered(
            ["first in batch", GATE_INPUT]
                .into_iter()
                .map(|text| {
                    EventKind::InputMessage(dim_agent_types::InputMessageData::text(
                        text,
                        Some(InputMode::Append),
                    ))
                    .into()
                })
                .collect(),
            |_| Ok(()),
        )
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), probe.entered.notified())
        .await
        .unwrap();
    let partial = client.rpc("session.history", json!({
        "sessionId":session.id(),"branchId":"main","after":0,"limit":1,"through":batch[0].sequence
    })).await;
    assert_eq!(partial["error"]["code"], -32602);
    let head = session.sequence("main").unwrap();
    let request = client
        .send(
            "session.history",
            json!({
                "sessionId":session.id(),"branchId":"main","after":batch[0].sequence-1,"limit":1
            }),
        )
        .await;
    let early = tokio::time::timeout(Duration::from_millis(100), client.response(request)).await;
    let later = session
        .submit("later input".into(), InputMode::Append, "main")
        .await
        .unwrap();
    probe.release.notify_one();
    assert!(
        early.is_err(),
        "history must wait for its published snapshot to dispatch"
    );
    let reply = client.response(request).await;
    assert_ok(&reply);
    let result = &reply["result"];
    assert_eq!(result["through"], head);
    assert_eq!(result["nextAfter"], batch[1].sequence);
    assert_eq!(
        result["events"].as_array().unwrap().len(),
        2,
        "a page retains the complete batch"
    );
    assert!(result["through"].as_u64().unwrap() < later.sequence);
    let partial_after = client.rpc("session.history", json!({
        "sessionId":session.id(),"branchId":"main","after":batch[0].sequence,"limit":1,"through":head
    })).await;
    assert_eq!(partial_after["error"]["code"], -32602);
    server.shutdown();
    server.join().await.unwrap();
}

#[tokio::test]
async fn decisions_are_consumed_in_order_on_both_transports_and_survive_disconnect() {
    for ws in [false, true] {
        let probe = Arc::new(Probe::default());
        let (server, session, mut a, calls) = setup(ws, probe.clone()).await;
        let request_id = request(&mut a, &session).await;
        let event = chain_events(&session, "main", ..)
            .await
            .unwrap()
            .into_iter()
            .find(|event| matches!(event.kind, EventKind::ApprovalRequest(_)))
            .unwrap();
        let EventKind::ApprovalRequest(data) = &event.kind else {
            unreachable!()
        };
        assert_eq!(data.approval_id, request_id.as_str().unwrap());
        assert!(matches!(
            &data.source,
            dim_agent_types::BranchSource::Tool { request_id, .. }
                if data.approval_id != *request_id
        ));
        assert_eq!(data.reason, "Reviewed approval reason");
        let mut b = Client::new(&server, !ws).await;
        assert_ok(
            &b.rpc("session.attach", json!({"sessionId":session.id()}))
                .await,
        );
        assert_eq!(
            b.notice("approval.request").await["params"]["requestId"],
            request_id
        );
        gate(&session, &probe).await;
        let first = a
            .send(
                "approval.respond",
                json!({"sessionId":session.id(),"requestId":request_id,"decision":"deny"}),
            )
            .await;
        wait_events(&session, "approval/respond", 1).await;
        let second = b
            .send(
                "approval.respond",
                json!({"sessionId":session.id(),"requestId":request_id,"decision":"allow"}),
            )
            .await;
        wait_events(&session, "approval/respond", 2).await;
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert!(
            tokio::time::timeout(Duration::from_millis(30), a.response(first))
                .await
                .is_err(),
            "RPC cannot succeed before its event is consumed"
        );
        probe.release.notify_one();
        assert_ok(&a.response(first).await);
        assert_eq!(b.response(second).await["error"]["code"], -32602);
        session.idle("main").await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(result_count(&session).await, 1);

        *probe.decision.lock().unwrap() = Some(ApprovalDecision::Allow);
        let request_id = request(&mut a, &session).await;
        assert_eq!(
            b.notice("approval.request").await["params"]["requestId"],
            request_id
        );
        gate(&session, &probe).await;
        a.send(
            "approval.respond",
            json!({"sessionId":session.id(),"requestId":request_id,"decision":"deny"}),
        )
        .await;
        wait_events(&session, "approval/respond", 3).await;
        drop(a);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        probe.release.notify_one();
        session.idle("main").await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(result_count(&session).await, 2);
        let history = chain_events(&session, "main", ..).await.unwrap();
        assert!(
            matches!(history.iter().rev().find(|e| matches!(e.kind, EventKind::ApprovalRespond(_))).unwrap().kind,
            EventKind::ApprovalRespond(ref data) if data.decision == ApprovalDecision::Allow)
        );
        let attached = b
            .rpc("session.attach", json!({"sessionId":session.id()}))
            .await;
        assert_eq!(attached["result"]["sessionId"], session.id());
        *probe.decision.lock().unwrap() = Some(ApprovalDecision::Deny);
        let request_id = request(&mut b, &session).await;
        assert_ok(
            &b.rpc(
                "approval.respond",
                json!({"sessionId":session.id(),"requestId":request_id,"decision":"allow"}),
            )
            .await,
        );
        session.idle("main").await.unwrap();
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "the committed deny must override the original allow"
        );
        assert_eq!(result_count(&session).await, 3);
        session.close().await.unwrap();
        server.shutdown();
        server.join().await.unwrap();
    }
}

#[tokio::test]
async fn another_client_learns_each_committed_response_on_both_transports() {
    for ws in [false, true] {
        let probe = Arc::new(Probe::default());
        let (server, session, mut a, calls) = setup_version(ws, probe.clone(), 3).await;
        let mut b = Client::new_version(&server, !ws, 3).await;
        assert_ok(
            &b.rpc("session.attach", json!({"sessionId":session.id()}))
                .await,
        );
        let request_id = request_v2(&mut a, &session).await;
        assert_eq!(
            b.notice("approval.request").await["params"]["requestId"],
            request_id
        );
        assert_ok(
            &a.rpc(
                "approval.respond",
                json!({"sessionId":session.id(),"branchId":"main","requestId":request_id,"decision":"allow"}),
            )
            .await,
        );
        let resolved = b.notice("approval.resolved").await;
        assert_eq!(
            resolved["params"],
            json!({"sessionId":session.id(),"branchId":"main","requestId":request_id,"decision":"allow"})
        );
        assert_eq!(a.notice("approval.resolved").await, resolved);
        session.idle("main").await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        let request_id = request_v2(&mut a, &session).await;
        b.notice("approval.request").await;
        gate(&session, &probe).await;
        let first = a
            .send(
                "approval.respond",
                json!({"sessionId":session.id(),"branchId":"main","requestId":request_id,"decision":"deny"}),
            )
            .await;
        wait_events(&session, "approval/respond", 2).await;
        let second = b
            .send(
                "approval.respond",
                json!({"sessionId":session.id(),"branchId":"main","requestId":request_id,"decision":"allow"}),
            )
            .await;
        wait_events(&session, "approval/respond", 3).await;
        probe.release.notify_one();
        assert_ok(&a.response(first).await);
        assert_eq!(b.response(second).await["error"]["code"], -32602);
        let decisions = [
            b.notice("approval.resolved").await,
            b.notice("approval.resolved").await,
        ]
        .map(|notice| {
            assert_eq!(notice["params"]["requestId"], request_id);
            notice["params"]["decision"].clone()
        });
        assert_eq!(decisions, [json!("deny"), json!("allow")]);
        session.idle("main").await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(b.notices.iter().all(|notice| {
            notice["method"] != "session.event" || notice["params"]["event"]["type"] != "approval"
        }));
        session.close().await.unwrap();
        server.shutdown();
        server.join().await.unwrap();
    }
}

#[tokio::test]
async fn a_policy_change_ends_pending_questions_for_every_attached_client() {
    for ws in [false, true] {
        let probe = Arc::new(Probe::default());
        let (server, session, mut owner, calls) = setup_version(ws, probe.clone(), 3).await;
        let mut observer = Client::new_version(&server, !ws, 3).await;
        assert_ok(
            &observer
                .rpc("session.attach", json!({"sessionId":session.id()}))
                .await,
        );
        let request_id = request_v2(&mut owner, &session).await;
        assert_eq!(
            observer.notice("approval.request").await["params"]["requestId"],
            request_id
        );
        session
            .service::<dim_agent_capabilities::permission::Permission>("main")
            .unwrap()
            .update(
                0,
                serde_json::from_value(json!({"preset":"full-access"})).unwrap(),
            )
            .await
            .unwrap();
        session.idle("main").await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        for client in [&mut owner, &mut observer] {
            let ended = client.notice("approval.cancelled").await;
            assert_eq!(ended["params"]["requestId"], request_id);
            assert_eq!(
                ended["params"]["reason"],
                "Approval no longer required by the permission policy"
            );
            assert!(
                client
                    .notices
                    .iter()
                    .all(|notice| notice["method"] != "approval.resolved")
            );
        }
        let rejected = owner
            .rpc(
                "approval.respond",
                json!({"sessionId":session.id(),"branchId":"main","requestId":request_id,"decision":"deny"}),
            )
            .await;
        assert_eq!(rejected["error"]["code"], -32602);
        let timeline = probe.timeline.lock().unwrap().clone().unwrap();
        timeline
            .append(
                EventKind::ApprovalRespond(dim_agent_types::ApprovalRespondData {
                    approval_id: request_id.as_str().unwrap().into(),
                    decision: ApprovalDecision::Deny,
                })
                .into(),
            )
            .unwrap();
        for client in [&mut owner, &mut observer] {
            let losing = client.notice("approval.resolved").await;
            assert_eq!(losing["params"]["requestId"], request_id);
            assert_eq!(losing["params"]["decision"], "deny");
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            session
                .service::<Approval>("main")
                .unwrap()
                .respond(request_id.as_str().unwrap(), ApprovalDecision::Allow)
                .await,
            Err(dim_agent_capabilities::approval::ApprovalRespondError::Unknown)
        );
        let mut attached = Client::new_version(&server, ws, 3).await;
        assert_ok(
            &attached
                .rpc("session.attach", json!({"sessionId":session.id()}))
                .await,
        );
        settle_notices(&mut attached).await;
        assert!(
            attached
                .notices
                .iter()
                .all(|notice| notice["method"] != "approval.request")
        );
        session.close().await.unwrap();
        server.shutdown();
        server.join().await.unwrap();
    }
}

#[tokio::test]
async fn automatic_permission_settlement_cannot_overtake_a_committed_denial() {
    for ws in [false, true] {
        let probe = Arc::new(Probe::default());
        let (server, session, mut owner, calls) = setup_version(ws, probe.clone(), 3).await;
        let request_id = request_v2(&mut owner, &session).await;
        *probe.deny_on_policy_change.lock().unwrap() = Some(request_id.as_str().unwrap().into());
        let permission = session
            .service::<dim_agent_capabilities::permission::Permission>("main")
            .unwrap();
        let update = tokio::spawn(async move {
            permission
                .update(
                    0,
                    serde_json::from_value(json!({"preset":"full-access"})).unwrap(),
                )
                .await
        });
        tokio::time::timeout(Duration::from_secs(5), probe.entered.notified())
            .await
            .unwrap();
        wait_events(&session, "approval/respond", 1).await;
        assert_eq!(
            session
                .get::<dim_agent_capabilities::permission::PermissionState>("main")
                .unwrap()
                .unwrap()
                .policy
                .preset,
            dim_agent_types::PermissionPreset::FullAccess,
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(100), async {
                while calls.load(Ordering::SeqCst) == 0 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .is_err(),
            "a committed denial must block automatic admission before dispatch"
        );
        probe.release.notify_one();
        update.await.unwrap().unwrap();
        session.idle("main").await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        let ended = owner.notice("approval.resolved").await;
        assert_eq!(ended["params"]["requestId"], request_id);
        assert_eq!(ended["params"]["decision"], "deny");
        assert!(!chain_events(&session, "main", ..).await.unwrap().iter().any(|event| {
            matches!(&event.kind, EventKind::ApprovalCancelled(data) if data.approval_id == request_id.as_str().unwrap())
        }));
        session.close().await.unwrap();
        server.shutdown();
        server.join().await.unwrap();
    }
}

#[tokio::test]
async fn v2_clients_receive_no_approval_resolved_on_both_transports() {
    for ws in [false, true] {
        let probe = Arc::new(Probe::default());
        let (server, session, mut a, calls) = setup_version(ws, probe, 2).await;
        let mut b = Client::new_version(&server, !ws, 2).await;
        assert_ok(
            &b.rpc("session.attach", json!({"sessionId":session.id()}))
                .await,
        );
        let request_id = request_v2(&mut a, &session).await;
        assert_eq!(
            b.notice("approval.request").await["params"]["requestId"],
            request_id
        );
        assert_ok(
            &a.rpc(
                "approval.respond",
                json!({"sessionId":session.id(),"branchId":"main","requestId":request_id,"decision":"allow"}),
            )
            .await,
        );
        session.idle("main").await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        session.close().await.unwrap();
        for client in [&mut a, &mut b] {
            client.notice("session.closed").await;
            assert!(
                client
                    .notices
                    .iter()
                    .all(|notice| notice["method"] != "approval.resolved"),
                "{:?}",
                client.notices
            );
        }
        server.shutdown();
        server.join().await.unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn automatic_admission_waits_for_its_terminal_to_be_stored() {
    for (deny_during_publication, close_store) in [(false, false), (true, false), (false, true)] {
        let dir =
            std::env::temp_dir().join(format!("dim-approval-publication-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("db.sqlite");
        let db = dim_app_server::storage::SqliteStorage::open(path.clone()).unwrap();
        let store = dim_app_server::storage::SqliteTimelineStore::new(db.clone());
        let probe = Arc::new(Probe::default());
        let (server, session, mut client, _) =
            setup_store(false, probe.clone(), 3, store.clone()).await;
        let approval = session.service::<Approval>("main").unwrap();
        let admission = tokio::spawn(async move {
            approval
                .admit(
                    AdmitCall {
                        source: dim_agent_types::BranchSource::Tool {
                            turn_id: "turn".into(),
                            request_id: "request".into(),
                            call_id: "call".into(),
                        },
                        call: dim_agent_types::ApprovalCallData {
                            name: "exec".into(),
                            input: serde_json::from_value(json!({"command":"git push"})).unwrap(),
                        },
                    },
                    Some("execute_process"),
                    CancellationToken::new(),
                )
                .await
        });
        let request = client.notice("approval.request").await;
        let (release, gate) = std::sync::mpsc::channel();
        *probe.cancel_gate.lock().unwrap() = Some(gate);
        let permission = session
            .service::<dim_agent_capabilities::permission::Permission>("main")
            .unwrap();
        permission
            .update(
                0,
                serde_json::from_value(json!({"preset":"full-access"})).unwrap(),
            )
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), probe.cancel_entered.notified())
            .await
            .unwrap();
        if close_store {
            store.close().await.unwrap();
            release.send(()).unwrap();
            let error = tokio::time::timeout(Duration::from_secs(5), admission)
                .await
                .unwrap()
                .unwrap()
                .unwrap_err();
            assert!(
                error.to_string().contains("Timeline storage is closed"),
                "{error}"
            );
            let timeline = probe.timeline.lock().unwrap().clone().unwrap();
            assert!(timeline.failure().is_some());
            assert!(
                timeline
                    .append(
                        EventKind::ApprovalCancelled(dim_agent_types::ApprovalCancelledData {
                            approval_id: request["params"]["requestId"].as_str().unwrap().into(),
                            reason: error.to_string(),
                        })
                        .into()
                    )
                    .is_err(),
                "a failed Timeline cannot record a replacement terminal"
            );
            let key = TimelineKey {
                session_id: session.id().into(),
                branch_id: "main".into(),
            };
            let head = store.head(&key).await.unwrap();
            let events = store.read(&key, 1..=head).await.unwrap();
            assert!(
                events
                    .iter()
                    .any(|record| matches!(record.event.kind, EventKind::ApprovalRequest(_)))
            );
            assert!(
                !events
                    .iter()
                    .any(|record| matches!(record.event.kind, EventKind::ApprovalCancelled(_)))
            );
            assert!(session.close().await.is_err());
            server.shutdown();
            let _ = server.join().await;
            db.close().await.unwrap();
            std::fs::remove_dir_all(dir).unwrap();
            continue;
        }
        let lock = rusqlite::Connection::open(&path).unwrap();
        lock.execute_batch("BEGIN IMMEDIATE").unwrap();
        release.send(()).unwrap();
        let timeline = probe.timeline.lock().unwrap().clone().unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            while !timeline.has_pending() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the terminal must be queued while its storage commit is blocked");
        if deny_during_publication {
            *probe.command_rules.lock().unwrap() = serde_json::from_value(json!([
                {"command":"git push", "match":"exact", "decision":"deny"}
            ]))
            .unwrap();
        }
        assert!(
            tokio::time::timeout(Duration::from_millis(100), async {
                while !admission.is_finished() {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .is_err(),
            "the tool cannot start before its terminal is durable"
        );
        lock.execute_batch("ROLLBACK").unwrap();
        let result = tokio::time::timeout(Duration::from_secs(5), admission)
            .await
            .unwrap()
            .unwrap();
        if deny_during_publication {
            assert_eq!(
                result.unwrap_err().to_string(),
                "Command denied by command rule `git push` (exact)"
            );
        } else {
            result.unwrap();
        }
        assert_eq!(
            client.notice("approval.cancelled").await["params"]["requestId"],
            request["params"]["requestId"]
        );
        let events = chain_events(&session, "main", ..).await.unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event.kind, EventKind::ApprovalCancelled(_)))
                .count(),
            1
        );
        assert!(
            !events
                .iter()
                .any(|event| matches!(event.kind, EventKind::ApprovalRespond(_)))
        );
        session.close().await.unwrap();
        server.shutdown();
        server.join().await.unwrap();
        drop(lock);
        store.close().await.unwrap();
        db.close().await.unwrap();
        std::fs::remove_dir_all(dir).unwrap();
    }
}

#[tokio::test]
async fn stop_before_request_and_after_allow_prevent_tool_start_on_both_transports() {
    for ws in [false, true] {
        let probe = Arc::new(Probe::default());
        probe.stop_before_request.store(true, Ordering::SeqCst);
        let (server, session, mut client, calls) = setup(ws, probe.clone()).await;
        assert_ok(
            &client
                .rpc(
                    "session.submit",
                    json!({"sessionId":session.id(),"text":"tool:confirm {}"}),
                )
                .await,
        );
        tokio::time::timeout(Duration::from_secs(5), probe.entered.notified())
            .await
            .unwrap();
        wait_events(&session, "approval/request", 1).await;
        let snapshot = chain_events(&session, "main", ..).await.unwrap();
        let stop = snapshot
            .iter()
            .find(|e| matches!(e.kind, EventKind::StopInterrupt(_)))
            .unwrap();
        let late = snapshot
            .iter()
            .find(|e| matches!(e.kind, EventKind::ApprovalRequest(_)))
            .unwrap();
        assert!(stop.sequence < late.sequence);
        let attached = client
            .rpc("session.attach", json!({"sessionId":session.id()}))
            .await;
        assert_eq!(attached["result"]["sessionId"], session.id());
        let replayed = client.notice("approval.request").await;
        probe.release.notify_one();
        session.idle("main").await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(result_count(&session).await, 1);
        assert_eq!(
            client.notice("approval.cancelled").await["params"]["requestId"],
            replayed["params"]["requestId"]
        );
        client.notices.clear();

        let request_id = request(&mut client, &session).await;
        gate(&session, &probe).await;
        let accepted = client
            .send(
                "approval.respond",
                json!({"sessionId":session.id(),"requestId":request_id,"decision":"allow"}),
            )
            .await;
        wait_events(&session, "approval/respond", 1).await;
        let stopped = client
            .send("session.stop", json!({"sessionId":session.id()}))
            .await;
        wait_events(&session, "stop/interrupt", 2).await;
        probe.release.notify_one();
        assert_ok(&client.response(accepted).await);
        assert_ok(&client.response(stopped).await);
        assert_eq!(
            client.notice("approval.cancelled").await["params"]["requestId"],
            request_id
        );
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "stop consumed before the tool task resumes must prevent execution"
        );
        assert_eq!(result_count(&session).await, 2);
        let duplicate = client
            .rpc(
                "approval.respond",
                json!({"sessionId":session.id(),"requestId":request_id,"decision":"allow"}),
            )
            .await;
        assert_eq!(duplicate["error"]["code"], -32602);
        session.close().await.unwrap();
        server.shutdown();
        server.join().await.unwrap();
    }
}

#[tokio::test]
async fn stop_and_close_reject_queued_responses_on_both_transports() {
    for ws in [false, true] {
        for close in [false, true] {
            let probe = Arc::new(Probe::default());
            let (server, session, mut client, calls) = setup(ws, probe.clone()).await;
            let request_id = request(&mut client, &session).await;
            gate(&session, &probe).await;
            let stopped = client
                .send(
                    if close {
                        "session.close"
                    } else {
                        "session.stop"
                    },
                    json!({"sessionId":session.id()}),
                )
                .await;
            wait_events(&session, "stop/interrupt", 1).await;
            let response = client
                .send(
                    "approval.respond",
                    json!({"sessionId":session.id(),"requestId":request_id,"decision":"allow"}),
                )
                .await;
            wait_events(&session, "approval/respond", 1).await;
            probe.release.notify_one();
            assert_eq!(client.response(response).await["error"]["code"], -32602);
            assert_ok(&client.response(stopped).await);
            assert_eq!(
                client.notice("approval.cancelled").await["params"]["requestId"],
                request_id
            );
            assert_eq!(calls.load(Ordering::SeqCst), 0);
            assert_eq!(result_count(&session).await, 1);
            session.close().await.unwrap();
            server.shutdown();
            server.join().await.unwrap();
        }
    }
}
#[tokio::test]
async fn dispatcher_failure_releases_an_accepted_response_waiter() {
    for ws in [false, true] {
        let probe = Arc::new(Probe::default());
        let (server, session, mut client, calls) = setup(ws, probe.clone()).await;
        let request_id = request(&mut client, &session).await;
        gate(&session, &probe).await;
        let response = client
            .send(
                "approval.respond",
                json!({"sessionId":session.id(),"requestId":request_id,"decision":"allow"}),
            )
            .await;
        wait_events(&session, "approval/respond", 1).await;
        probe.dispatcher.lock().unwrap().as_ref().unwrap().fail(
            dim_agent_runtime::AgentError::fault("approval test", "dispatch failed"),
        );
        probe.release.notify_one();
        assert!(client.response(response).await.get("error").is_some());
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert!(session.close().await.is_err());
        server.shutdown();
        let _ = server.join().await;
    }
}

#[tokio::test]
async fn responses_and_interrupts_follow_the_owning_branch_on_both_transports() {
    for ws in [false, true] {
        let probe = Arc::new(Probe::default());
        let (server, session, mut client, calls) = setup_version(ws, probe, 2).await;
        let main_request = request_v2(&mut client, &session).await;
        let child = session
            .branch(
                "main".into(),
                0,
                dim_agent_runtime::BranchOptions {
                    prompt: "tool:confirm {}".into(),
                    source: dim_agent_types::BranchSource::Host {
                        operation_id: "branch-parent".into(),
                    },
                },
            )
            .await
            .unwrap();
        let child_request = client.notice("approval.request").await["params"]["requestId"].clone();
        assert_ne!(main_request, child_request);
        assert_ok(
            &client
                .rpc(
                    "session.stop",
                    json!({"sessionId":session.id(),"branchId":"main"}),
                )
                .await,
        );
        assert_eq!(
            client.notice("approval.cancelled").await["params"]["requestId"],
            main_request
        );
        assert_eq!(
            client
                .rpc(
                    "approval.respond",
                    json!({"sessionId":session.id(),"branchId":"main","requestId":child_request,"decision":"allow"}),
                )
                .await["error"]["code"],
            -32602
        );
        assert_ok(
            &client
                .rpc(
                    "approval.respond",
                    json!({"sessionId":session.id(),"branchId":child,"requestId":child_request,"decision":"allow"}),
                )
                .await,
        );
        session.idle(&child).await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let child_events = branch_events(&session, &child, ..).await.unwrap();
        assert!(child_events.iter().any(|e| matches!(&e.kind, EventKind::ApprovalRequest(data) if data.approval_id == child_request.as_str().unwrap())));
        assert!(child_events.iter().any(|e| matches!(&e.kind, EventKind::ApprovalRespond(data) if data.approval_id == child_request.as_str().unwrap())));
        assert!(
            !branch_events(&session, "main", ..)
                .await
                .unwrap()
                .iter()
                .any(|e| matches!(e.kind, EventKind::ApprovalRespond(_)))
        );
        assert_eq!(
            child_events
                .iter()
                .filter(|e| matches!(e.kind, EventKind::ToolResult(_)))
                .count(),
            1
        );
        assert_eq!(result_count(&session).await, 1);
        session.close().await.unwrap();
        server.shutdown();
        server.join().await.unwrap();
    }
}

#[tokio::test]
async fn rpc_reload_ends_idle_waiting_on_approval_and_keeps_the_request_answerable() {
    let probe = Arc::new(Probe::default());
    let (server, session, mut client, calls) = setup(false, probe).await;
    let request_id = request(&mut client, &session).await;
    let idle = client
        .send("session.idle", json!({"sessionId":session.id()}))
        .await;
    assert_ok(&client.rpc("daemon.ping", json!({})).await);
    tokio::time::timeout(Duration::from_secs(5), server.restart("Rpc"))
        .await
        .expect("reload does not wait on client input")
        .unwrap();
    assert_eq!(
        client.response(idle).await["error"]["data"]["appError"],
        "service_reloading"
    );
    assert_ok(
        &client
            .rpc(
                "approval.respond",
                json!({"sessionId":session.id(),"requestId":request_id,"decision":"allow"}),
            )
            .await,
    );
    assert_ok(
        &client
            .rpc("session.idle", json!({"sessionId":session.id()}))
            .await,
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    server.shutdown();
    server.join().await.unwrap();
}

#[tokio::test]
async fn tool_reload_cancels_a_pending_approval_fails_the_turn_and_the_session_continues() {
    for (version, sqlite) in [(1, false), (2, false), (1, true), (2, true)] {
        let dir = std::env::temp_dir().join(format!("dim-tool-reload-{}", uuid::Uuid::new_v4()));
        let store: Arc<dyn dim_agent_runtime::TimelineStore> = if sqlite {
            std::fs::create_dir_all(&dir).unwrap();
            let db = dim_app_server::storage::SqliteStorage::open(dir.join("db.sqlite")).unwrap();
            dim_app_server::storage::SqliteTimelineStore::new(db)
        } else {
            Arc::new(dim_agent_runtime::MemoryTimelineStore::default())
        };
        let probe = Arc::new(Probe::default());
        let (server, session, mut client, calls) =
            setup_store(false, probe.clone(), version, store).await;
        let branch = if version == 2 {
            json!("main")
        } else {
            Value::Null
        };
        let mut submit = json!({"sessionId":session.id(),"text":"tool:confirm {}"});
        if version == 2 {
            submit["branchId"] = branch.clone();
        }
        assert_ok(&client.rpc("session.submit", submit.clone()).await);
        let request = client.notice("approval.request").await;
        let stale = request["params"]["requestId"].clone();
        tokio::time::timeout(Duration::from_secs(5), probe.tools.restart())
            .await
            .expect("reload settles in-flight work");
        let cancelled = client.notice("approval.cancelled").await;
        assert_eq!(cancelled["params"]["requestId"], stale);
        wait_events(&session, "turn/end", 1).await;
        let events = chain_events(&session, "main", ..).await.unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(
                    &event.kind,
                    EventKind::ToolResult(data) if data.results.iter().all(|r| r.result.is_error == Some(true))
                ))
                .count(),
            1,
            "exactly one terminal for the reloaded batch"
        );
        assert!(events.iter().any(|event| matches!(
            &event.kind,
            EventKind::TurnEnd(data)
                if data.outcome == TurnOutcome::Failed { error: "capability reloading".into() }
        )));
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event.kind, EventKind::ModelStart(_)))
                .count(),
            1,
            "no model request after the reload settled the call"
        );
        let mut respond = json!({"sessionId":session.id(),"requestId":stale,"decision":"allow"});
        if version == 2 {
            respond["branchId"] = branch.clone();
        }
        assert!(
            client.rpc("approval.respond", respond.clone()).await["error"].is_object(),
            "a settled approval cannot be answered"
        );

        assert_ok(&client.rpc("session.submit", submit).await);
        let request = client.notice("approval.request").await;
        respond["requestId"] = request["params"]["requestId"].clone();
        assert_ok(&client.rpc("approval.respond", respond).await);
        let mut idle = json!({"sessionId":session.id()});
        if version == 2 {
            idle["branchId"] = branch;
        }
        assert_ok(&client.rpc("session.idle", idle).await);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(session.failure().is_none());
        server.shutdown();
        server.join().await.unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }
}

async fn settle_notices(client: &mut Client) {
    assert_ok(&client.rpc("daemon.ping", json!({})).await);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn attach_replay_and_cancellation_leave_no_pending_approval_on_both_transports() {
    for ws in [false, true] {
        let probe = Arc::new(Probe::default());
        let (server, session, mut owner, calls) = setup_version(ws, probe.clone(), 2).await;

        request_v2(&mut owner, &session).await;
        let (release, gate) = std::sync::mpsc::channel();
        *probe.cancel_gate.lock().unwrap() = Some(gate);
        let stopped = owner
            .send(
                "session.stop",
                json!({"sessionId":session.id(),"branchId":"main"}),
            )
            .await;
        tokio::time::timeout(Duration::from_secs(5), probe.cancel_entered.notified())
            .await
            .expect("the cancellation reached its commit");
        let mut late = Client::new_version(&server, !ws, 2).await;
        let attaching = late
            .send("session.attach", json!({"sessionId":session.id()}))
            .await;
        release.send(()).unwrap();
        assert_ok(&late.response(attaching).await);
        assert_ok(&owner.response(stopped).await);
        settle_notices(&mut late).await;
        assert!(
            late.notices
                .iter()
                .all(|notice| notice["method"] != "approval.request"),
            "a removed request is not replayed"
        );

        let replayed = request_v2(&mut owner, &session).await;
        let mut attached = Client::new_version(&server, ws, 2).await;
        assert_ok(
            &attached
                .rpc("session.attach", json!({"sessionId":session.id()}))
                .await,
        );
        assert_eq!(
            attached.notice("approval.request").await["params"]["requestId"],
            replayed
        );
        assert!(
            attached
                .notices
                .iter()
                .all(|notice| notice["method"] != "approval.cancelled"),
            "the end of a replayed request follows the replay"
        );
        assert_ok(
            &owner
                .rpc(
                    "session.stop",
                    json!({"sessionId":session.id(),"branchId":"main"}),
                )
                .await,
        );
        assert_eq!(
            attached.notice("approval.cancelled").await["params"]["requestId"],
            replayed
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        session.close().await.unwrap();
        server.shutdown();
        server.join().await.unwrap();
    }
}

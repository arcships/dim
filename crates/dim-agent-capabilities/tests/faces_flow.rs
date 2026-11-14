use dim_agent_capabilities::{
    adapters::{DefaultPrepareModelPlugin, EchoModel, MockTools},
    approval::{AdmitCall, Approval},
    defaults,
    host::*,
    input_queue::InputQueue,
    permission::Permission,
    wait::{Wait, WaitResult},
};
use dim_agent_runtime::testing::MinimalHost;
use dim_agent_runtime::*;
use dim_agent_types::*;
use rutis::Ctx;
use rutis::host::diagnostics::RuntimeDiagnostics;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, InstanceId, TypeKey};
use rutis_bridge::session::{HostDispatch, Value as BridgeValue, decode_value, host_key, settle};
use serde_json::{Value, json};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio_util::sync::CancellationToken;

struct HandsItsContext {
    name: &'static str,
    slot: Arc<Mutex<Option<Ctx>>>,
}
impl DynPlugin for HandsItsContext {
    fn name(&self) -> &str {
        self.name
    }
    fn injects(&self) -> Vec<TypeKey> {
        Vec::new()
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            *self.slot.lock().unwrap() = Some(ctx.clone());
            Ok(Effect::Done)
        })
    }
}

struct Fixture {
    host: MinimalHost,
    session: Session,
    session_ctx: Ctx,
    branch_ctx: Ctx,
    session_instance: InstanceId,
    branch_instance: InstanceId,
}

fn group_instance(diagnostics: &RuntimeDiagnostics, scope: &str) -> InstanceId {
    let scope = diagnostics
        .plugins
        .iter()
        .find(|plugin| plugin.name == scope)
        .unwrap_or_else(|| panic!("no {scope} part"));
    let group = scope.parent.expect("a scope part runs in a group");
    diagnostics
        .plugins
        .iter()
        .find(|plugin| plugin.id == group)
        .expect("the scope group")
        .instance
}

async fn fixture(tools: Arc<dyn ToolDriver>) -> Fixture {
    let session_ctx = Arc::new(Mutex::new(None));
    let branch_ctx = Arc::new(Mutex::new(None));
    let mut builder = AgentBuilder::new(AgentConfig::default());
    builder
        .process_value(Arc::new(ModelHost(Arc::new(EchoModel))))
        .session_value(Arc::new(ToolHost(tools)))
        .session_value(Arc::new(
            dim_agent_capabilities::history::HistoryRenderer::default(),
        ))
        .session_value(Arc::new(PrepareContextHost(Arc::new(IdentityContext))))
        .session_value(Arc::new(ContextModelHost(None)))
        .session_value(Arc::new(CompactionStrategyHost(None)))
        .require_feature(
            Feature::new("PrepareModelHost").session("PrepareModelHost", |names| {
                Ok(Box::new(DefaultPrepareModelPlugin::new(names)?))
            }),
        );
    let sessions = session_ctx.clone();
    let branches = branch_ctx.clone();
    builder.require_feature(
        Feature::new("FaceProbe")
            .session("FaceProbeSession", move |_| {
                Ok(Box::new(HandsItsContext {
                    name: "FaceProbeSession",
                    slot: sessions.clone(),
                }))
            })
            .branch("FaceProbeBranch", move |_| {
                Ok(Box::new(HandsItsContext {
                    name: "FaceProbeBranch",
                    slot: branches.clone(),
                }))
            }),
    );
    defaults::install(&mut builder);
    builder.require_feature(Feature::new("Wait").branch("WaitBranch", |names| {
        Ok(Box::new(
            dim_agent_capabilities::wait::WaitBranchPlugin::new(names)?,
        ))
    }));
    let host = MinimalHost::start(&builder).await.unwrap();
    let session = host
        .create_session(SessionConfig {
            permission_policy: PermissionPolicy {
                preset: PermissionPreset::WorkspaceWrite,
                ..Default::default()
            },
            ..Default::default()
        })
        .await
        .unwrap();
    let session_instance = group_instance(&session.diagnostics().unwrap(), "SessionScope");
    let branch_instance =
        group_instance(&session.branch_diagnostics("main").unwrap(), "BranchScope");
    let session_ctx = session_ctx
        .lock()
        .unwrap()
        .take()
        .expect("the Session probe");
    let branch_ctx = branch_ctx.lock().unwrap().take().expect("the branch probe");
    Fixture {
        host,
        session,
        session_ctx,
        branch_ctx,
        session_instance,
        branch_instance,
    }
}

async fn call(
    ctx: &Ctx,
    instance: InstanceId,
    name: &str,
    method: &str,
    args: Vec<BridgeValue>,
) -> Value {
    let dispatch = ctx
        .get_at::<dyn HostDispatch>(&host_key(name).instance(instance))
        .unwrap_or_else(|| panic!("{name} is not a face in this instance"));
    let reply = dispatch
        .invoke(method, BridgeValue::List(args))
        .unwrap_or_else(|error| panic!("{name}.{method}: {error}"));
    decode_value(settle(reply).await.unwrap()).unwrap()
}

fn never() -> BridgeValue {
    BridgeValue::callback(|_| Ok(BridgeValue::future(std::future::pending())))
}

fn never_cancelled() -> CancelledFn {
    Arc::new(|| Box::pin(std::future::pending()))
}

#[tokio::test]
async fn approval_decide_answers_the_admission_the_typed_service_started() {
    let Fixture {
        host,
        session,
        branch_ctx,
        branch_instance,
        ..
    } = fixture(Arc::new(MockTools)).await;
    let approval = session.service::<Approval>("main").unwrap();
    let admitted = {
        let approval = approval.clone();
        tokio::spawn(async move {
            approval
                .admit(
                    AdmitCall {
                        source: BranchSource::Host {
                            operation_id: "faces".into(),
                        },
                        call: ApprovalCallData {
                            name: "echo".into(),
                            input: Default::default(),
                        },
                    },
                    None,
                    CancellationToken::new(),
                )
                .await
        })
    };
    let mut pending = Vec::new();
    for _ in 0..200 {
        pending = approval.pending();
        if !pending.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(pending.len(), 1, "the admission asks for one approval");

    let typed = serde_json::to_value(approval.pending()).unwrap();
    let wire = call(
        &branch_ctx,
        branch_instance,
        "approval",
        "pending",
        Vec::new(),
    )
    .await;
    assert_eq!(typed, wire, "the face lists what the owner holds");

    let approval_id = pending[0].approval_id.clone();
    call(
        &branch_ctx,
        branch_instance,
        "approval",
        "decide",
        vec![json!(approval_id).into(), json!("allow").into()],
    )
    .await;
    admitted
        .await
        .unwrap()
        .expect("the wire decision admitted the call the typed service started");
    assert!(approval.pending().is_empty());
    assert_eq!(
        call(
            &branch_ctx,
            branch_instance,
            "approval",
            "pending",
            Vec::new()
        )
        .await,
        json!([])
    );
    host.close().await.unwrap();
}

#[tokio::test]
async fn permission_check_returns_the_typed_decision() {
    let Fixture {
        host,
        session,
        branch_ctx,
        branch_instance,
        ..
    } = fixture(Arc::new(MockTools)).await;
    let permission = session.service::<Permission>("main").unwrap();
    let call_data = ApprovalCallData {
        name: "echo".into(),
        input: Default::default(),
    };
    let typed = permission
        .decide(&call_data, None, &CancellationToken::new())
        .await
        .unwrap();
    let wire = call(
        &branch_ctx,
        branch_instance,
        "permission",
        "check",
        vec![json!(&call_data).into(), json!(null).into(), never()],
    )
    .await;
    assert_eq!(serde_json::to_value(&typed).unwrap(), wire);
    host.close().await.unwrap();
}

#[tokio::test]
async fn input_queue_submits_and_withdraws_as_the_typed_service_does() {
    let Fixture {
        host,
        session,
        branch_ctx,
        branch_instance,
        ..
    } = fixture(Arc::new(MockTools)).await;
    let queue = session.service::<InputQueue>("main").unwrap();
    let mut input = InputMessageData::text("faces submit", Some(InputMode::Append));
    input.submission_id = Some("faces-submit".into());
    let typed = queue.submit(input.clone()).await.unwrap();
    let wire = call(
        &branch_ctx,
        branch_instance,
        "inputQueue",
        "submit",
        vec![json!(&input).into()],
    )
    .await;
    assert_eq!(
        serde_json::to_value(&typed).unwrap(),
        wire,
        "the same submission id is one committed input"
    );
    assert_eq!(
        session
            .lookup("main", "faces-submit")
            .await
            .unwrap()
            .unwrap()
            .id,
        typed.input_id
    );

    let missing = InputWithdrawData {
        input_id: "not-held".into(),
        reason: None,
        steer: None,
    };
    let typed = queue.withdraw(missing.clone()).await.unwrap();
    let wire = call(
        &branch_ctx,
        branch_instance,
        "inputQueue",
        "withdraw",
        vec![json!(&missing).into()],
    )
    .await;
    assert_eq!(
        serde_json::to_value(typed).unwrap(),
        wire,
        "neither call writes for an input the queue does not hold"
    );
    assert_eq!(wire, json!({"status": "notHeld"}));
    for submission_id in ["faces-submit", "rejected-steer"] {
        let mut replacement = InputMessageData::text("unchecked", Some(InputMode::Steer));
        replacement.submission_id = Some(submission_id.into());
        let command = InputWithdrawData {
            steer: Some(replacement),
            ..missing.clone()
        };
        assert!(matches!(
            queue.withdraw(command.clone()).await,
            Err(AgentError::InvalidInput(_))
        ));
        let dispatch = branch_ctx
            .get_at::<dyn HostDispatch>(&host_key("inputQueue").instance(branch_instance))
            .unwrap();
        let reply = dispatch
            .invoke("withdraw", BridgeValue::List(vec![json!(command).into()]))
            .unwrap();
        assert!(
            settle(reply).await.is_err(),
            "unchecked Steer must be rejected by the wire face"
        );
    }
    assert!(
        session
            .lookup("main", "rejected-steer")
            .await
            .unwrap()
            .is_none()
    );
    host.close().await.unwrap();
}

#[tokio::test]
async fn wait_returns_the_typed_result() {
    let Fixture {
        host,
        session,
        branch_ctx,
        branch_instance,
        ..
    } = fixture(Arc::new(MockTools)).await;
    let wait = session.service::<Wait>("main").unwrap();
    let typed = wait.wait(None, 1, never_cancelled()).await.unwrap();
    let wire = call(
        &branch_ctx,
        branch_instance,
        "wait",
        "wait",
        vec![json!(null).into(), json!(1).into(), never()],
    )
    .await;
    let WaitResult::Timeout { .. } = typed else {
        panic!("the typed wait timed out: {typed:?}");
    };
    assert_eq!(
        serde_json::to_value(&typed).unwrap()["reason"],
        wire["reason"],
        "both callers read the same end"
    );
    assert!(wire["waitedMs"].is_number(), "{wire}");
    let cancelled =
        BridgeValue::callback(|_| Ok(BridgeValue::future(async { Ok(json!(null).into()) })));
    let interrupted = call(
        &branch_ctx,
        branch_instance,
        "wait",
        "wait",
        vec![json!(null).into(), json!(300_000).into(), cancelled],
    )
    .await;
    assert_eq!(interrupted["reason"], "interrupted");
    assert!(
        matches!(
            wait.wait(None, 1, never_cancelled()).await.unwrap(),
            WaitResult::Timeout { .. }
        ),
        "wire cancellation must release the single-wait slot"
    );
    host.close().await.unwrap();
}

#[tokio::test]
async fn tool_host_executes_as_the_typed_port_does() {
    let tools = Arc::new(MockTools);
    let Fixture {
        host,
        session_ctx,
        session_instance,
        ..
    } = fixture(tools.clone()).await;
    let call_data = ToolCall {
        id: "call-1".into(),
        name: "echo".into(),
        input: json!({"text": "faces"}).as_object().unwrap().clone(),
    };
    let context = ToolExecutionContext {
        turn_id: "turn-1".into(),
        request_id: "request-1".into(),
        branch_id: "main".into(),
    };
    let typed = ToolHost(tools)
        .execute(call_data.clone(), context.clone(), never_cancelled())
        .await
        .unwrap();
    let wire = call(
        &session_ctx,
        session_instance,
        "toolHost",
        "execute",
        vec![json!(&call_data).into(), json!(&context).into(), never()],
    )
    .await;
    assert_eq!(serde_json::to_value(&typed).unwrap(), wire);
    host.close().await.unwrap();
}

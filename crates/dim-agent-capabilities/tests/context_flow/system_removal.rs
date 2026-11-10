//! #1361: a configured source leaves System for an input-delivered class
//! while its new value is still waiting for delivery. The removal of the old
//! System entry applies where the change is found — the inherited prefix's
//! configuration already composes the System without it — so the child's open
//! re-adopts the current value instead of finding it equal to what it holds.

use super::*;
use dim_agent_capabilities::prompt_state::PromptSnapshot;
use dim_agent_runtime::testing::branch_events;

/// One source whose text and delivery class both come from its version.
struct Classes(std::sync::Mutex<(usize, PromptDelivery)>);
impl LoadPrompt for Classes {
    fn load(
        &self,
        target: PromptSourceTarget,
        _: CancellationToken,
    ) -> HostFuture<'_, Option<PromptEntry>> {
        let (version, delivery) = *self.0.lock().unwrap();
        Box::pin(async move {
            Ok(Some(PromptEntry {
                body: format!("text-{version}"),
                source: target.source,
                scope: target.scope,
                version: version.to_string(),
                delivery,
                order: 0,
            }))
        })
    }
}

fn target() -> PromptSourceTarget {
    PromptSourceTarget {
        source: "switch".into(),
        scope: PromptScope::Branch {
            timeline: "main".into(),
        },
    }
}

fn model() -> ModelSelection {
    ModelSelection {
        provider_id: "provider".into(),
        model_id: "model".into(),
        capability_snapshot: None,
        config: None,
        metadata: None,
    }
}

/// The text of every message of `request`, as the model reads it.
fn sent(request: &dim_agent_types::ModelRequest) -> String {
    request
        .messages
        .iter()
        .map(|message| match &message.content {
            MessageContent::Text(text) => text.clone(),
            MessageContent::Parts(parts) => parts
                .iter()
                .filter_map(|part| match part {
                    MessagePart::Text { text, .. } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n"),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The last `model/call` of `branch`'s own facts.
async fn last_call(session: &Session, branch: &str) -> Arc<dim_agent_types::ModelRequest> {
    branch_events(session, branch, ..)
        .await
        .unwrap()
        .iter()
        .rev()
        .find_map(|event| match &event.kind {
            EventKind::ModelCall(data) => Some(data.request.clone()),
            _ => None,
        })
        .expect("the line made a request")
}

fn system_text(request: &dim_agent_types::ModelRequest) -> String {
    match request.system.as_deref() {
        Some(SystemPrompt::Text(text)) => text.clone(),
        other => panic!("the request carries no System text: {other:?}"),
    }
}

/// #1361: a source turns from System into State while the Session is idle.
/// The child opens before the change's delivery input is consumed on main, so
/// its state inherits that change undelivered. It must not keep the old System
/// entry, and its first request carries the new value once.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_source_leaving_the_system_prompt_is_removed_before_its_new_value_is_delivered() {
    let classes = Arc::new(Classes(std::sync::Mutex::new((1, PromptDelivery::System))));
    let mut builder = AgentBuilder::new(AgentConfig::default());
    defaults::mock_hosts(&mut builder);
    defaults::install_with_prompt(
        &mut builder,
        classes.clone(),
        Some(ConfigureUpdate {
            sources: vec![target()],
            model: Some(model()),
        }),
    );
    let agent = MinimalHost::start(&builder).await.unwrap();
    let session = agent
        .create_session(SessionConfig::default())
        .await
        .unwrap();
    session
        .submit("first".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    session.idle("main").await.unwrap();
    assert!(
        system_text(last_call(&session, "main").await.as_ref()).starts_with("text-1"),
        "main runs with the System entry"
    );

    // The source leaves the System prompt while the Session is idle: the
    // change and its delivery input are appended, and no request of main
    // consumes that input before the cut.
    *classes.0.lock().unwrap() = (2, PromptDelivery::State);
    session
        .configure(
            ConfigureUpdate {
                sources: vec![target()],
                model: None,
            },
            "main",
        )
        .await
        .unwrap();
    let at = session.sequence("main").unwrap();
    let child = session
        .branch(
            "main".into(),
            at,
            BranchOptions {
                prompt: "child work".into(),
                source: BranchSource::Host {
                    operation_id: "child".into(),
                },
            },
        )
        .await
        .unwrap();
    session.idle(&child).await.unwrap();
    let snapshot = session
        .get::<PromptSnapshot>(&child)
        .unwrap()
        .expect("the line holds its prompt state");
    assert_eq!(
        snapshot
            .entries
            .values()
            .map(|entry| entry.body.as_str())
            .collect::<Vec<_>>(),
        ["text-2"],
        "the line never holds the System entry the source left"
    );
    assert!(
        snapshot.system.is_empty(),
        "the System composition no longer carries the entry: {:?}",
        snapshot.system
    );
    let first = last_call(&session, &child).await;
    assert!(
        !system_text(&first).contains("text-1"),
        "the System prompt no longer carries the entry: {:?}",
        first.system
    );
    assert_eq!(
        sent(&first).matches("text-2").count(),
        1,
        "the line's first request carries the current value exactly once:\n{}",
        sent(&first)
    );
    agent.close().await.unwrap();
}

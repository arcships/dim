use dim_agent_runtime::testing::chain_events;
use dim_agent_runtime::{AgentBuilder, AgentConfig, SessionConfig, testing::MinimalHost};
use dim_agent_types::{EventKind, InputMessageData, InputMode};

fn builder() -> AgentBuilder {
    let mut builder = AgentBuilder::new(AgentConfig::default());
    dim_agent_capabilities::defaults::mock_hosts(&mut builder);
    dim_agent_capabilities::defaults::install(&mut builder);
    builder
}

fn context_input(content: &str) -> InputMessageData {
    InputMessageData {
        blocks: vec![dim_agent_capabilities::context_block::raw(
            format!("<dim-state source=\"host/selection\">{content}</dim-state>"),
            Some("selection".into()),
        )],
        ..InputMessageData::default()
    }
}

#[tokio::test]
async fn context_only_input_waits_for_the_next_user_turn() {
    let agent = MinimalHost::start(&builder()).await.unwrap();
    let session = agent
        .create_session(SessionConfig::default(), Default::default())
        .await
        .unwrap();
    session
        .submit("initial".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    session.idle("main").await.unwrap();
    let turns_before = chain_events(&session, "main", ..)
        .await
        .unwrap()
        .iter()
        .filter(|event| matches!(event.kind, EventKind::TurnStart(_)))
        .count();

    let context = session
        .input(context_input("selected text"), "main")
        .await
        .unwrap();
    session.idle("main").await.unwrap();
    assert!(!session.state("main").unwrap().running);
    assert_eq!(
        chain_events(&session, "main", ..)
            .await
            .unwrap()
            .iter()
            .filter(|event| matches!(event.kind, EventKind::TurnStart(_)))
            .count(),
        turns_before
    );

    session
        .submit("continue".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    session.idle("main").await.unwrap();
    let events = chain_events(&session, "main", ..).await.unwrap();
    assert!(events.iter().any(
        |event| matches!(&event.kind, EventKind::InputConsumed(data) if data.input_id == context.id)
    ));
    agent.close().await.unwrap();
}

#[tokio::test]
async fn context_only_input_after_a_stop_starts_no_turn() {
    let agent = MinimalHost::start(&builder()).await.unwrap();
    let session = agent
        .create_session(SessionConfig::default(), Default::default())
        .await
        .unwrap();
    session.stop("main", "pause".into()).await.unwrap();

    let context = session
        .input(context_input("new fact"), "main")
        .await
        .unwrap();
    session.idle("main").await.unwrap();
    let events = chain_events(&session, "main", ..).await.unwrap();
    assert!(
        !events
            .iter()
            .any(|event| matches!(event.kind, EventKind::TurnStart(_)))
    );
    assert!(!events.iter().any(
        |event| matches!(&event.kind, EventKind::InputConsumed(data) if data.input_id == context.id)
    ));

    session
        .submit("resume".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    session.idle("main").await.unwrap();
    assert!(chain_events(&session, "main", ..).await.unwrap().iter().any(
        |event| matches!(&event.kind, EventKind::InputConsumed(data) if data.input_id == context.id)
    ));
    agent.close().await.unwrap();
}

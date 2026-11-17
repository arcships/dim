use super::*;
use dim_agent_runtime::testing::branch_events;
use dim_agent_runtime::{AgentBuilder, AgentConfig, SessionConfig};
use dim_agent_types::EventKind;
use std::time::Duration;

mod deferred_inputs;
use deferred_inputs::DeferredInputs;

/// The typed entry is crate-private; exercise its full server/Session flow
/// here without adding a public test access API.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn typed_input_retains_admitted_io_and_accepts_each_submission_once() {
    let (store, mut held) = DeferredInputs::new();
    let mut builder = AgentBuilder::new(AgentConfig::default());
    builder.timeline_store(store);
    dim_agent_capabilities::defaults::install(&mut builder);
    dim_agent_capabilities::defaults::mock_model_hosts(&mut builder);
    dim_agent_capabilities::defaults::tool_host(
        &mut builder,
        Arc::new(dim_agent_capabilities::adapters::MockTools),
    );
    let server = crate::serve(builder, crate::ServerConfig::default())
        .await
        .unwrap();
    let session = server
        .agent()
        .unwrap()
        .create_session(SessionConfig::default())
        .await
        .unwrap();
    let index = server.root.get::<AgentIndex>().unwrap();
    let write = |submission: &str| InputWrite {
        session_id: session.id().into(),
        branch_id: "main".into(),
        mode: InputMode::Append,
        submission_id: submission.into(),
        raw_blocks: vec![RawBlock {
            text: "Typed input settlement".into(),
            key: None,
        }],
        content: vec![],
    };
    let request = write("settlement:typed:publication");
    let caller = index.clone();
    let call = tokio::spawn(async move { caller.input(request).await });
    let waiting = tokio::time::timeout(Duration::from_secs(5), held.recv())
        .await
        .unwrap()
        .unwrap();
    let (restarted, ()) = tokio::join!(server.restart("Agents"), async {
        index.stopping.cancelled().await;
        waiting.release.send(()).unwrap();
    });
    restarted.unwrap();
    let receipt = call.await.unwrap().unwrap();
    assert!(session.publication("main").unwrap().sequence() >= receipt.sequence);
    assert!(matches!(
        index.input(write("test:retired-index")).await,
        Err(SessionsError::Reloading { .. })
    ));

    let index = server.root.get::<AgentIndex>().unwrap();
    assert!(matches!(
        index
            .lookup(LookupRequest {
                session_id: session.id().into(),
                branch_id: "main".into(),
                submission_id: "test:retired-index".into(),
            })
            .await
            .unwrap(),
        LookupReply::Unknown
    ));
    let request = write("settlement:typed:dropped");
    let call = tokio::spawn(async move { index.input(request).await });
    let waiting = tokio::time::timeout(Duration::from_secs(5), held.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(waiting.submission_id, "settlement:typed:dropped");
    call.abort();
    assert!(call.await.unwrap_err().is_cancelled());
    let resubmitted = tokio::spawn({
        let index = server.root.get::<AgentIndex>().unwrap();
        let request = write("settlement:typed:dropped");
        async move { index.input(request).await }
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(!resubmitted.is_finished());
    waiting.release.send(()).unwrap();
    let receipt = tokio::time::timeout(Duration::from_secs(5), resubmitted)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(receipt.sequence, waiting.sequence);
    assert!(held.try_recv().is_err());
    tokio::time::timeout(
        Duration::from_secs(5),
        published(&session.publication("main").unwrap(), waiting.sequence),
    )
    .await
    .unwrap()
    .unwrap();
    let index = server.root.get::<AgentIndex>().unwrap();
    let recovered = index
        .lookup(LookupRequest {
            session_id: session.id().into(),
            branch_id: "main".into(),
            submission_id: "settlement:typed:dropped".into(),
        })
        .await
        .unwrap();
    let LookupReply::Accepted { input_id, sequence } = recovered else {
        panic!("the dropped input was not found");
    };
    assert_eq!(
        (input_id.as_str(), sequence),
        (receipt.input_id.as_str(), receipt.sequence)
    );
    let again = index
        .input(write("settlement:typed:dropped"))
        .await
        .unwrap();
    assert_eq!((again.input_id, again.sequence), (input_id, sequence));
    let (one, two) = tokio::join!(
        index.input(write("test:concurrent")),
        index.input(write("test:concurrent"))
    );
    let (one, two) = (one.unwrap(), two.unwrap());
    assert_eq!((one.input_id, one.sequence), (two.input_id, two.sequence));
    let history = branch_events(&session, "main", ..).await.unwrap();
    for submission in [
        "settlement:typed:publication",
        "settlement:typed:dropped",
        "test:concurrent",
    ] {
        assert_eq!(history.iter().filter(|event| matches!(&event.kind, EventKind::InputMessage(data) if data.submission_id.as_deref() == Some(submission))).count(), 1);
    }
    server.shutdown();
    server.join().await.unwrap();
}

/// `exists` answers what `acquire` reads: a loaded Session exists, an
/// unloaded one exists through its rows, a closed one exists because its rows
/// remain, and a deleted one does not (sessions service §6.9).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn exists_answers_loaded_unloaded_closed_and_deleted() {
    let directory = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target/agents-exists")
        .join(uuid::Uuid::new_v4().to_string());
    std::fs::create_dir_all(&directory).unwrap();
    let mut builder = AgentBuilder::new(AgentConfig::default());
    dim_agent_capabilities::defaults::install(&mut builder);
    dim_agent_capabilities::defaults::mock_model_hosts(&mut builder);
    dim_agent_capabilities::defaults::tool_host(
        &mut builder,
        Arc::new(dim_agent_capabilities::adapters::MockTools),
    );
    let server = crate::serve(
        builder,
        crate::ServerConfig {
            database_path: Some(directory.join("dimcode.sqlite")),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let agent = server.agent().unwrap();
    let index = server.root.get::<AgentIndex>().unwrap();
    let exists = |id: String| {
        let index = index.clone();
        async move {
            index
                .exists(ExistsRequest { session_id: id })
                .await
                .unwrap()
                .exists
        }
    };

    // Loaded, then unloaded with its rows kept: still exists.
    let unloaded = agent
        .create_session(SessionConfig::default())
        .await
        .unwrap();
    let unloaded_id = unloaded.id().to_owned();
    assert!(exists(unloaded_id.clone()).await);
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if agent
                .get_session(&unloaded_id)
                .unwrap()
                .unload_if_idle()
                .await
                .unwrap()
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert!(
        exists(unloaded_id.clone()).await,
        "an unloaded Session exists"
    );
    index.delete(&unloaded_id).await.unwrap();
    assert!(
        !exists(unloaded_id).await,
        "a deleted Session does not exist"
    );

    // Closed keeps its rows, so it exists; deleting it removes them.
    let closed = agent
        .create_session(SessionConfig::default())
        .await
        .unwrap();
    let closed_id = closed.id().to_owned();
    closed.close().await.unwrap();
    assert!(exists(closed_id.clone()).await, "a closed Session exists");
    index.delete(&closed_id).await.unwrap();
    assert!(!exists(closed_id).await, "a deleted Session does not exist");
    server.shutdown();
    server.join().await.unwrap();
}

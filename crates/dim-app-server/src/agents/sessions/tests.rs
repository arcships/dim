use super::*;
use dim_agent_runtime::{AgentBuilder, AgentConfig, SessionConfig};
use std::time::Duration;

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
            // This test does not exercise the startup deadline; a whole-suite
            // run can make assembly slower than the product default.
            startup_timeout: Duration::from_secs(30),
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
                .exists(SessionRequest { session_id: id })
                .await
                .unwrap()
                .exists
        }
    };

    // Loaded, then unloaded with its rows kept: still exists.
    let unloaded = agent
        .create_session(SessionConfig::default(), Default::default())
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
        .create_session(SessionConfig::default(), Default::default())
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

#[test]
fn a_routed_call_during_an_index_replacement_is_transient() {
    use serde_json::json;
    let error = super::index_reloading();
    let value = json!(error);
    assert_eq!(value["code"], "session_reloading", "{value}");
    assert!(error.retryable(), "{value}");
}

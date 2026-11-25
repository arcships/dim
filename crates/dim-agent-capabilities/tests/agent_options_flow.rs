use dim_agent_capabilities::defaults;
use dim_agent_runtime::testing::MinimalHost;
use dim_agent_runtime::{
    AgentBuilder, AgentConfig, CreateSessionOptions, MemoryTimelineStore, RestoreSessionOptions,
    SessionConfig, TimelineStore, TimelineStoreBinding,
};
use rutis::host::DynPlugin;
use rutis::{Ctx, Plugin};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU32, Ordering},
};

const GROUP: &str = "options-probe";

struct Probe {
    value: u32,
    seen: Arc<AtomicU32>,
}
impl Plugin for Probe {
    type Deps = ();
    fn name(&self) -> &str {
        "options-probe"
    }
    async fn apply(&self, _ctx: &Ctx, (): Self::Deps) -> rutis::Result {
        self.seen.store(self.value, Ordering::SeqCst);
        Ok(())
    }
}

fn builder(store: &Arc<MemoryTimelineStore>, seen: Arc<AtomicU32>) -> AgentBuilder {
    let mut builder = AgentBuilder::new(AgentConfig::default());
    defaults::mock_hosts(&mut builder);
    defaults::install(&mut builder);
    builder
        .timeline_store(TimelineStoreBinding::new(store.clone()))
        .session_extension_part::<u32>(GROUP, "options-probe", move |_, value: &u32| {
            Ok(Box::new(Probe {
                value: *value,
                seen: seen.clone(),
            }) as Box<dyn DynPlugin>)
        });
    builder
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn create_and_restore_options_apply_their_fields() {
    let seen = Arc::new(AtomicU32::new(0));
    let store = Arc::new(MemoryTimelineStore::default());
    let agent = MinimalHost::start(&builder(&store, seen.clone()))
        .await
        .unwrap();

    let prepared = Arc::new(Mutex::new(None::<String>));
    let session = agent
        .create_session(
            SessionConfig::default(),
            CreateSessionOptions::default()
                .id("named-session")
                .prepare({
                    let store = store.clone();
                    let prepared = prepared.clone();
                    move |id| {
                        *prepared.lock().unwrap() = Some(id.clone());
                        let config = SessionConfig::default();
                        Box::pin(async move {
                            store.create_session(&id, &config).await?;
                            Ok(())
                        })
                    }
                })
                .extension(GROUP, 7_u32),
        )
        .await
        .unwrap();
    assert_eq!(
        session.id(),
        "named-session",
        "a prepared creation is created under the id the options name"
    );
    assert_eq!(
        *prepared.lock().unwrap(),
        Some("named-session".to_owned()),
        "the prepare step commits the rows of the id its Session is created under"
    );
    assert_eq!(
        seen.load(Ordering::SeqCst),
        7,
        "the Session loaded the extension's value"
    );
    session.close().await.unwrap();

    let generated = Arc::new(Mutex::new(None::<String>));
    let session = agent
        .create_session(
            SessionConfig::default(),
            CreateSessionOptions::default().prepare({
                let store = store.clone();
                let generated = generated.clone();
                move |id| {
                    *generated.lock().unwrap() = Some(id.clone());
                    let config = SessionConfig::default();
                    Box::pin(async move {
                        store.create_session(&id, &config).await?;
                        Ok(())
                    })
                }
            }),
        )
        .await
        .unwrap();
    assert_eq!(
        generated.lock().unwrap().as_deref(),
        Some(session.id()),
        "a prepared creation without an id is created under a generated one, which the prepare step receives"
    );
    session.close().await.unwrap();

    let session = agent
        .create_session(
            SessionConfig::default(),
            CreateSessionOptions::default().id("explicit-session"),
        )
        .await
        .unwrap();
    assert_eq!(
        session.id(),
        "explicit-session",
        "the options name a plain creation"
    );
    session.close().await.unwrap();

    store
        .create_session("stored", &SessionConfig::default())
        .await
        .unwrap();
    let session = agent
        .restore_session("stored".into(), RestoreSessionOptions::default())
        .await
        .unwrap();
    assert!(
        session.config().excluded_tools.is_empty(),
        "without a protocol version the stored configuration is restored as written"
    );
    session.close().await.unwrap();

    let session = agent
        .restore_session(
            "stored".into(),
            RestoreSessionOptions::default().protocol_version(1),
        )
        .await
        .unwrap();
    assert_eq!(
        session.config().excluded_tools,
        vec!["codemode".to_owned()],
        "a protocol version below 4 keeps the codemode tool out"
    );
    session.close().await.unwrap();
    agent.close().await.unwrap();
}

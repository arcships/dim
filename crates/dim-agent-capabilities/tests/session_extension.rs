//! A requested Session extension is the Session scope's: it mounts its
//! plugins before the Session publishes, they leave with it, and an extension
//! that fails fails creation with its own error (#602).
use dim_agent_capabilities::defaults;
use dim_agent_runtime::testing::MinimalHost;
use dim_agent_runtime::{AgentBuilder, AgentConfig, SessionConfig};
use rutis::host::ErrorKind;
use rutis::{Ctx, Plugin};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

const GROUP: &str = "extension-probe";

struct Probe {
    mounted: Arc<AtomicUsize>,
    closed: Arc<AtomicUsize>,
}
impl Plugin for Probe {
    type Deps = ();
    fn name(&self) -> &str {
        "extension-probe"
    }
    async fn apply(&self, ctx: &Ctx, (): Self::Deps) -> rutis::Result {
        self.mounted.fetch_add(1, Ordering::SeqCst);
        let closed = self.closed.clone();
        ctx.effect(move || async move {
            closed.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })?;
        Ok(())
    }
}

fn builder() -> AgentBuilder {
    let mut builder = AgentBuilder::new(AgentConfig::default());
    defaults::mock_hosts(&mut builder);
    defaults::install(&mut builder);
    builder
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_requested_extension_mounts_before_publication_and_leaves_with_its_session() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let mounted = Arc::new(AtomicUsize::new(0));
        let closed = Arc::new(AtomicUsize::new(0));
        let mut builder = builder();
        builder.session_extension_part::<u32>(GROUP, "extension-probe", {
            let (mounted, closed) = (mounted.clone(), closed.clone());
            move |_, _: &u32| {
                Ok(Box::new(Probe {
                    mounted: mounted.clone(),
                    closed: closed.clone(),
                }))
            }
        });
        let agent = MinimalHost::start(&builder).await.unwrap();
        let session = agent
            .create_session_with_extension(SessionConfig::default(), GROUP, 7_u32)
            .await
            .unwrap();
        assert_eq!(
            mounted.load(Ordering::SeqCst),
            1,
            "the Session mounted the extension's plugin before it published"
        );
        session.close().await.unwrap();
        assert_eq!(
            closed.load(Ordering::SeqCst),
            1,
            "the extension left with its Session"
        );
        agent.close().await.unwrap();
    })
    .await
    .expect("extension flow completed");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failing_extension_fails_creation_with_its_error() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let mut builder = builder();
        builder.session_extension_part::<u32>(GROUP, "extension-probe", |_, _: &u32| {
            Err(rutis::Error::from(ErrorKind::PluginFailed(
                "extension fixture refused".into(),
            )))
        });
        let agent = MinimalHost::start(&builder).await.unwrap();
        let error = agent
            .create_session_with_extension(SessionConfig::default(), GROUP, 7_u32)
            .await
            .err()
            .expect("a failing extension rejects its Session");
        assert!(
            error.to_string().contains("extension fixture refused"),
            "{error}"
        );
        assert!(agent.sessions().is_empty());
        agent.close().await.unwrap();
    })
    .await
    .expect("extension failure flow completed");
}

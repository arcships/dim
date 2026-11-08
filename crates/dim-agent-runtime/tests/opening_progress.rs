//! An opening that runs longer than its inactivity interval is still waited
//! for while it reports real progress, and one that reports none times out.
use dim_agent_runtime::{opening_advanced, wait_active};
use rutis::host::CtxHost;
use rutis::{Ctx, Plugin};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// An opening made of `steps` waits; with `reports` it reports each one as
/// progress, without it reports nothing.
struct Progressing {
    steps: usize,
    gap: Duration,
    reports: bool,
}
impl Plugin for Progressing {
    type Deps = ();
    fn name(&self) -> &str {
        "opening.progressing"
    }
    async fn apply(&self, ctx: &Ctx, (): Self::Deps) -> rutis::Result {
        for _ in 0..self.steps {
            tokio::time::sleep(self.gap).await;
            if self.reports {
                opening_advanced(ctx);
            }
        }
        Ok(())
    }
}

async fn opened(reports: bool) -> dim_agent_runtime::Result<()> {
    let root = Ctx::root().unwrap();
    let view = root.plugin(Progressing {
        steps: 6,
        gap: Duration::from_millis(60),
        reports,
    });
    let result = wait_active(
        &root,
        std::slice::from_ref(&view),
        Duration::from_millis(300),
        &CancellationToken::new(),
    )
    .await;
    root.shutdown().await.unwrap();
    result
}

/// Six 60 ms waits, a fifth of the 300 ms interval, with a report after each
/// outlast the interval and still complete.
#[tokio::test]
async fn an_opening_that_reports_progress_outlives_its_interval() {
    opened(true)
        .await
        .expect("reported progress renews the interval");
}

/// The same opening without reports ends at the interval.
#[tokio::test]
async fn an_opening_that_reports_nothing_times_out() {
    let error = opened(false)
        .await
        .expect_err("an inactive opening must time out");
    assert!(error.to_string().contains("timed out"), "{error}");
}

//! The one place this crate reads the time and waits on it (#1536).
//!
//! Production seats [`SystemClock`], which is `Timestamp::now()` and the sleep this
//! crate has always used. Feature `testing` seats
//! [`ManualClock`](crate::testing::ManualClock), so a flow test can cross a
//! wait without sleeping.
use jiff::Timestamp;
use rutis::host::BoxFuture;
use tokio_util::sync::CancellationToken;

/// The clock a Scheduler generation reads and waits on.
pub(crate) trait Clock: Send + Sync + 'static {
    fn now(&self) -> Timestamp;

    /// Waits until `until`; `false` once `stop` is cancelled, when the caller
    /// leaves an unsettled delivery to the next generation's recovery.
    fn wait_until<'a>(
        &'a self,
        until: Timestamp,
        stop: &'a CancellationToken,
    ) -> BoxFuture<'a, bool>;
}

pub(crate) struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Timestamp {
        Timestamp::now()
    }

    fn wait_until<'a>(
        &'a self,
        until: Timestamp,
        stop: &'a CancellationToken,
    ) -> BoxFuture<'a, bool> {
        Box::pin(async move {
            // A span that is already past is a zero wait, not a wait to compute.
            let delay = std::time::Duration::try_from(until.duration_since(Timestamp::now()))
                .unwrap_or_default();
            tokio::select! {
                _ = stop.cancelled() => false,
                _ = tokio::time::sleep(delay) => true,
            }
        })
    }
}

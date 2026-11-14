//! The flow tests' seams (feature `testing`): holds that keep an operation at
//! a known point, a clock a test can move, and the signals a pass emits.
use crate::clock::Clock;
use chrono::{DateTime, Utc};
use rutis::host::BoxFuture;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        Arc, LazyLock, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

/// The hooks of one Scheduler instance.
#[derive(Default)]
pub struct Holds {
    disable: Mutex<Option<Arc<DisableHold>>>,
    launch: Mutex<Option<Arc<Notify>>>,
    fail_release: AtomicBool,
    lose_input_reply: AtomicBool,
    /// Notified after a wakeup create acquires process admission.
    pub wakeup_create: Notify,
    /// Notified after a loop start acquires process admission.
    pub loop_start: Notify,
    /// Fails the armed number of `sessions.lookup` replies with a transient
    /// transport error, so a lookup pass can meet one that answers nothing.
    lookup_failures: AtomicUsize,
}

/// A hold between a disable's document read and its CAS write. Every disable
/// that reads while the hold is armed waits, and each re-checks its claim
/// after the release, so a racing timer pass cannot land the write the test
/// is holding back.
#[derive(Default)]
pub struct DisableHold {
    /// Notified once a disable has read the document.
    pub read: Notify,
    released: tokio::sync::watch::Sender<bool>,
}

impl DisableHold {
    /// Lets every waiting disable proceed.
    pub fn release(&self) {
        self.released.send_replace(true);
    }

    async fn wait(&self) {
        self.read.notify_one();
        let mut released = self.released.subscribe();
        if !*released.borrow_and_update() {
            let _ = released.changed().await;
        }
    }
}

impl Holds {
    /// Holds the next accepted launch before it calls the Session service.
    /// Notify the returned handle to let the launch proceed.
    pub fn hold_next_launch(&self) -> Arc<Notify> {
        let release = Arc::new(Notify::new());
        *self.launch.lock().unwrap() = Some(release.clone());
        release
    }

    pub(crate) async fn before_launch(&self) {
        let release = self.launch.lock().unwrap().take();
        if let Some(release) = release {
            release.notified().await;
        }
    }

    /// Holds the next disables between their read and their write until the
    /// returned hold is released.
    pub fn hold_next_disable(&self) -> Arc<DisableHold> {
        let hold = Arc::new(DisableHold::default());
        *self.disable.lock().unwrap() = Some(hold.clone());
        hold
    }

    /// Fails the next ownership-release write once, so the heal path that
    /// follows it can be observed.
    pub fn fail_next_release(&self) {
        self.fail_release.store(true, Ordering::SeqCst);
    }

    /// Loses the next input reply after the write may have reached the
    /// target, so a delivery takes its uncertain-reply path on a write whose
    /// input was in fact accepted (A.5, A.6).
    pub fn lose_next_input_reply(&self) {
        self.lose_input_reply.store(true, Ordering::SeqCst);
    }

    /// Fails the next `count` `sessions.lookup` replies with a transient
    /// transport error, so a lookup pass meets one that answers nothing.
    pub fn fail_lookups(&self, count: usize) {
        self.lookup_failures.store(count, Ordering::SeqCst);
    }

    /// Whether this lookup reply is to fail, counting down to zero.
    pub(crate) fn take_lookup_failure(&self) -> bool {
        self.lookup_failures
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |left| {
                left.checked_sub(1)
            })
            .is_ok()
    }

    pub(crate) async fn before_disable_write(&self) {
        let hold = self.disable.lock().unwrap().clone();
        if let Some(hold) = hold {
            hold.wait().await;
        }
    }

    pub(crate) fn take_release_failure(&self) -> bool {
        self.fail_release.swap(false, Ordering::SeqCst)
    }

    /// Whether the next input reply is to be lost, once.
    pub(crate) fn take_lost_input_reply(&self) -> bool {
        self.lose_input_reply.swap(false, Ordering::SeqCst)
    }

    /// Whether an injected release failure is still waiting to be taken.
    pub fn release_failure_armed(&self) -> bool {
        self.fail_release.load(Ordering::SeqCst)
    }
}

/// The flow tests' clock: real time plus an offset a test adds (#1536).
///
/// A test that never advances it sees the real clock, so the flows that rely
/// on a real tick and a real wait keep today's timing; a test that advances
/// moves only this Scheduler's deadline, never another timer of the runtime.
pub struct ManualClock {
    offset: Mutex<chrono::Duration>,
    /// Bumped by every advance, so a parked waiter re-reads `now`.
    moved: tokio::sync::watch::Sender<u64>,
}

impl Default for ManualClock {
    fn default() -> Self {
        Self {
            offset: Mutex::new(chrono::Duration::zero()),
            moved: tokio::sync::watch::channel(0).0,
        }
    }
}

impl ManualClock {
    /// The instant this clock reads. Concrete, so a test never names the
    /// crate's private `Clock` trait.
    pub fn now(&self) -> DateTime<Utc> {
        Clock::now(self)
    }

    /// Moves this clock `by` forward (or back, for a negative span) and wakes
    /// every waiter the new instant has reached.
    pub fn advance(&self, by: chrono::Duration) {
        *self.offset.lock().unwrap() += by;
        self.moved.send_modify(|generation| *generation += 1);
    }
}

impl Clock for ManualClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now() + *self.offset.lock().unwrap()
    }

    fn wait_until<'a>(
        &'a self,
        until: DateTime<Utc>,
        stop: &'a CancellationToken,
    ) -> BoxFuture<'a, bool> {
        Box::pin(async move {
            let mut moved = self.moved.subscribe();
            loop {
                let now = self.now();
                if now >= until {
                    return true;
                }
                let sleep = (until - now).to_std().unwrap_or_default();
                tokio::select! {
                    _ = stop.cancelled() => return false,
                    _ = tokio::time::sleep(sleep) => return true,
                    _ = moved.changed() => {}
                }
            }
        })
    }
}

/// The signals a Scheduler pass emits for the flow tests.
#[derive(Default)]
pub struct Signals {
    retention: Mutex<Option<Arc<Notify>>>,
}

impl Signals {
    /// Arms the next retention pass and returns the handle that resolves once
    /// it has finished its writes.
    pub fn watch_next_retention(&self) -> Arc<Notify> {
        let mut armed = self.retention.lock().unwrap();
        armed.get_or_insert_with(|| Arc::new(Notify::new())).clone()
    }

    pub(crate) fn retention_done(&self) {
        let done = self.retention.lock().unwrap().take();
        if let Some(done) = done {
            done.notify_waiters();
        }
    }
}

/// One clock per home, so a restart judges the record against the clock the
/// previous generation left.
pub fn clock(home: &Path) -> Arc<ManualClock> {
    registry_entry(&CLOCKS, home)
}

/// One signal handle per home, as the clock is one per home.
pub fn signals(home: &Path) -> Arc<Signals> {
    registry_entry(&SIGNALS, home)
}

static CLOCKS: LazyLock<Mutex<HashMap<PathBuf, Arc<ManualClock>>>> =
    LazyLock::new(Default::default);
static SIGNALS: LazyLock<Mutex<HashMap<PathBuf, Arc<Signals>>>> = LazyLock::new(Default::default);

fn registry_entry<T: Default>(registry: &Mutex<HashMap<PathBuf, Arc<T>>>, home: &Path) -> Arc<T> {
    registry
        .lock()
        .unwrap()
        .entry(home.to_owned())
        .or_insert_with(|| Arc::new(T::default()))
        .clone()
}

/// What a flow test hands to [`install_with_testing`](crate::install_with_testing).
#[derive(Clone, Default)]
pub struct TestingOptions {
    pub holds: Arc<Holds>,
    pub clock: Arc<ManualClock>,
    pub signals: Arc<Signals>,
}

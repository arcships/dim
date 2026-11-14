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
    time::Duration,
};
use tokio::sync::{Notify, watch};
use tokio_util::sync::CancellationToken;

/// How long a helper waits for the Scheduler before it reports that it never
/// got there. A pass takes well under a second, so this bound only fires when
/// an advance did not reach what the pass was waiting for.
const BOUND: Duration = Duration::from_secs(30);

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

    /// Moves this clock to `at` and wakes every waiter the instant reaches. An
    /// instant real time has already passed moves nothing, so racing a real
    /// second can never rewind the clock.
    pub fn advance_to(&self, at: DateTime<Utc>) {
        let by = at.signed_duration_since(self.now());
        if by > chrono::Duration::zero() {
            self.advance(by);
        }
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

/// A waiter's park: the ticks finished when it parked, and the instant it waits
/// for. One value, because a reader that took the count and the instant apart
/// could advance a wait the timer had already left.
pub type TimerWait = (u64, DateTime<Utc>);

/// The signals a Scheduler pass emits for the flow tests.
#[derive(Default)]
pub struct Signals {
    /// `(stop timers that parked, the instant the newest one waits for)`.
    stop_parks: watch::Sender<(u64, Option<DateTime<Utc>>)>,
    stopped: watch::Sender<u64>,
    retained: watch::Sender<u64>,
    ticked: watch::Sender<u64>,
    timer: watch::Sender<Option<TimerWait>>,
}

impl Signals {
    /// Publishes the ticks finished and the instant the timer will wait for,
    /// before that wait.
    pub(crate) fn timer_wait(&self, done: u64, until: DateTime<Utc>) {
        self.timer.send_replace(Some((done, until)));
    }

    /// The timer's published waits.
    pub fn timer(&self) -> watch::Receiver<Option<TimerWait>> {
        self.timer.subscribe()
    }

    /// Publishes one finished timer tick.
    pub(crate) fn tick(&self) {
        self.ticked.send_modify(|ticks| *ticks += 1);
    }

    /// The timer ticks this generation has finished.
    pub fn ticks(&self) -> u64 {
        *self.ticked.borrow()
    }

    /// Publishes the instant a stop timer is about to wait for.
    pub(crate) fn stop_parked(&self, until: DateTime<Utc>) {
        self.stop_parks.send_modify(|(parks, wait)| {
            *parks += 1;
            *wait = Some(until);
        });
    }

    /// Publishes one stop timer that ended, after the record check it ended on:
    /// one that refused a record which is gone ends here as well as one that
    /// stopped its Session.
    pub(crate) fn stop_done(&self) {
        self.stopped.send_modify(|ended| *ended += 1);
    }

    /// Publishes one finished retention pass, after its last write.
    pub(crate) fn retention_done(&self) {
        self.retained.send_modify(|passes| *passes += 1);
    }

    /// The retention passes this generation has finished.
    pub fn retention(&self) -> u64 {
        *self.retained.borrow()
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

/// Reads the timer's published wait that the clock has not reached yet, so a
/// step moves to an instant the timer is provably parked on.
async fn parked(timer: &mut watch::Receiver<Option<TimerWait>>, clock: &ManualClock) -> TimerWait {
    tokio::time::timeout(BOUND, async {
        loop {
            if let Some((done, until)) = *timer.borrow_and_update()
                && until > clock.now()
            {
                return (done, until);
            }
            timer
                .changed()
                .await
                .expect("the timer keeps publishing its waits");
        }
    })
    .await
    .expect("the timer publishes the instant it parks on")
}

/// Crosses `ticks` ticks of the timer, and no real second of any of them.
///
/// The timer publishes the ticks it has finished with the instant it will wait
/// for, and only then waits. Each round here moves the clock to exactly that
/// instant, so the wait the timer holds is the wait that ends, and then waits
/// for the tick the pass behind it publishes. An advance of one interval taken
/// at any other moment would leave the next wait a whole interval of real time
/// away, which is the wait this removes.
pub async fn step(home: &Path, ticks: u64) {
    let clock = clock(home);
    let signals = signals(home);
    let mut timer = signals.timer();
    let mut ticked = signals.ticked.subscribe();
    let mut done = signals.ticks();
    for _ in 0..ticks {
        let (parked_at, until) = parked(&mut timer, &clock).await;
        done = done.max(parked_at);
        clock.advance_to(until);
        tokio::time::timeout(BOUND, ticked.wait_for(|ticks| *ticks > done))
            .await
            .expect("the timer finishes the tick a test crossed")
            .expect("the timer keeps ticking while a test crosses its ticks");
        done = signals.ticks();
    }
}

/// Crosses the stop timer of `home` to its stop time. `parks` is the count read
/// before the restart that resumes the timer, so the wait crossed is that
/// timer's own.
pub async fn stop_time(home: &Path, parks: u64) {
    let clock = clock(home);
    let mut waits = signals(home).stop_parks.subscribe();
    let until = tokio::time::timeout(BOUND, async {
        let seen = waits.wait_for(|(seen, _)| *seen > parks).await?;
        Ok::<_, tokio::sync::watch::error::RecvError>(seen.1)
    })
    .await
    .expect("the stop timer parks at its stop time")
    .expect("the stop timer keeps publishing its waits")
    .expect("a published stop park carries its instant");
    clock.advance_to(until);
}

/// The stop timers that have parked.
pub fn stop_parks(home: &Path) -> u64 {
    signals(home).stop_parks.borrow().0
}

/// The stop timers that have ended.
pub fn stop_timers(home: &Path) -> u64 {
    *signals(home).stopped.borrow()
}

/// Waits for one more stop timer to end, with the count read before the restart
/// that resumes it. A timer that refused a record which is gone ends here too,
/// so a negative claim still waits on a positive fact.
pub async fn stop_ended(home: &Path, ended: u64) {
    let mut stopped = signals(home).stopped.subscribe();
    tokio::time::timeout(BOUND, async {
        let _ = stopped.wait_for(|count| *count > ended).await;
    })
    .await
    .expect("the stop timer ends");
}

/// Waits for one more retention pass of this home, with the count taken before
/// the call that spawns it.
pub async fn retention(home: &Path, before: u64) {
    let mut passes = signals(home).retained.subscribe();
    tokio::time::timeout(BOUND, async {
        let _ = passes.wait_for(|passes| *passes > before).await;
    })
    .await
    .expect("the retention pass ends");
}

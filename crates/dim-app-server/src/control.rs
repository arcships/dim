use crate::ServerError;
use rutis::host::BoxFuture;
use rutis::{Ctx, Plugin};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

type ShutdownReply = Box<dyn FnOnce(bool) -> BoxFuture<'static, ()> + Send>;

/// Admission for process-owned work, including work accepted before a Session
/// exists. A hold starts before acceptance and ends after the last owned effect.
#[derive(Default)]
pub struct ProcessActivity {
    count: Arc<AtomicUsize>,
    foreign: Mutex<ActivityState>,
}

const PAUSED: usize = 1 << (usize::BITS - 1);

#[derive(Default)]
struct ActivityState {
    next: u64,
    holds: std::collections::HashMap<u64, ProcessHold>,
}

#[rutis::service]
impl ProcessActivity {
    /// The native binding of a hold. Dropping it releases the settled work.
    #[must_use = "dropping the hold releases admission"]
    #[rutis::service(skip)]
    pub fn take_hold(&self) -> Option<ProcessHold> {
        self.count
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                (count & PAUSED == 0).then_some(count + 1)
            })
            .ok()?;
        Some(ProcessHold(self.count.clone()))
    }

    /// Admits work unless admission is paused. Release the returned id after
    /// settlement. A lost owner leaves admission busy until process shutdown;
    /// an unload or crash is not evidence that its accepted work settled.
    pub fn hold(&self) -> Option<u64> {
        let hold = self.take_hold()?;
        let mut state = self.foreign.lock().expect("process activity");
        state.next += 1;
        let id = state.next;
        state.holds.insert(id, hold);
        Some(id)
    }

    /// Releases a settled operation's hold. Already released holds return false.
    pub fn release(&self, id: u64) -> bool {
        self.foreign
            .lock()
            .expect("process activity")
            .holds
            .remove(&id)
            .is_some()
    }

    #[rutis::service(skip)]
    pub(crate) fn pause(&self) -> Option<ProcessPause> {
        self.count
            .compare_exchange(0, PAUSED, Ordering::AcqRel, Ordering::Acquire)
            .ok()?;
        Some(ProcessPause(Some(self.count.clone())))
    }
}

/// A native operation admitted by [`ProcessActivity`].
pub struct ProcessHold(Arc<AtomicUsize>);

impl Drop for ProcessHold {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

pub(crate) struct ProcessPause(Option<Arc<AtomicUsize>>);

impl ProcessPause {
    fn commit(mut self) {
        self.0.take();
    }
}

impl Drop for ProcessPause {
    fn drop(&mut self) {
        if let Some(count) = &self.0 {
            count.store(0, Ordering::Release);
        }
    }
}

/// Process admission: the readiness gate, shutdown intent and listener faults.
pub struct ServerControl {
    ready: watch::Sender<bool>,
    shutdown: CancellationToken,
    force: AtomicBool,
    fault: Mutex<Option<ServerError>>,
    shutdown_replies: Mutex<Vec<ShutdownReply>>,
    pub(crate) activity: Arc<ProcessActivity>,
    paused: Mutex<Vec<dim_agent_runtime::SessionPause>>,
}
impl ServerControl {
    fn new() -> Self {
        Self {
            ready: watch::channel(false).0,
            shutdown: CancellationToken::new(),
            force: AtomicBool::new(false),
            fault: Mutex::new(None),
            shutdown_replies: Mutex::new(Vec::new()),
            activity: Arc::default(),
            paused: Mutex::new(Vec::new()),
        }
    }
    pub(crate) fn keep_paused(
        &self,
        pause: ProcessPause,
        sessions: Vec<dim_agent_runtime::SessionPause>,
    ) {
        *self.paused.lock().expect("process pause") = sessions;
        pause.commit();
    }
    pub(crate) fn defer_shutdown_reply(&self, reply: ShutdownReply) {
        self.shutdown_replies
            .lock()
            .expect("shutdown replies")
            .push(reply);
    }
    /// Called after all connection handoffs; success also requires graph cleanup.
    pub(crate) async fn complete_shutdown(&self, success: bool) {
        self.paused.lock().expect("process pause").clear();
        let replies = std::mem::take(&mut *self.shutdown_replies.lock().expect("shutdown replies"));
        futures_util::future::join_all(replies.into_iter().map(|reply| reply(success))).await;
    }
    pub(crate) fn open(&self) {
        self.ready.send_replace(true);
    }
    /// Resolves once every selected plugin is ready; never after shutdown began.
    pub(crate) async fn opened(&self) -> bool {
        let mut ready = self.ready.subscribe();
        tokio::select! {
            biased;
            _ = self.shutdown.cancelled() => false,
            result = ready.wait_for(|ready| *ready) => result.is_ok(),
        }
    }
    pub(crate) fn is_ready(&self) -> bool {
        *self.ready.borrow() && !self.shutdown.is_cancelled()
    }
    pub(crate) fn shutdown(&self) -> &CancellationToken {
        &self.shutdown
    }
    pub(crate) fn force(&self) {
        self.force.store(true, Ordering::Release);
    }
    pub(crate) fn forced(&self) -> bool {
        self.force.load(Ordering::Acquire)
    }
    /// A listener exit is a process failure; the first one is kept.
    pub(crate) fn fail(&self, error: ServerError) {
        self.fault
            .lock()
            .expect("server fault")
            .get_or_insert(error);
        self.shutdown.cancel();
    }
    pub(crate) fn take_fault(&self) -> Option<ServerError> {
        self.fault.lock().expect("server fault").take()
    }
}

#[derive(Clone)]
pub(crate) struct ServerControlPlugin(pub Arc<ServerControl>);
impl ServerControlPlugin {
    pub fn new() -> Self {
        Self(Arc::new(ServerControl::new()))
    }
}
impl Plugin for ServerControlPlugin {
    type Deps = ();
    fn name(&self) -> &str {
        "ServerControl"
    }
    async fn apply(&self, ctx: &Ctx, (): Self::Deps) -> rutis::Result {
        ctx.provide_as::<ProcessActivity>(self.0.activity.clone())?;
        ctx.provide_as::<ServerControl>(self.0.clone())?;
        Ok(())
    }
}

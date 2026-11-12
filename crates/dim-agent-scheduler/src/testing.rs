//! Hooks flow tests hold an operation open at a known point, or fail one
//! write once (feature `testing`).
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use tokio::sync::Notify;

/// The hooks of one Scheduler instance.
#[derive(Default)]
pub struct Holds {
    disable: Mutex<Option<Arc<DisableHold>>>,
    fail_release: AtomicBool,
    lose_input_reply: AtomicBool,
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

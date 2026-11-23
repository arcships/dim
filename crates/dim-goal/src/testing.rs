//! Hooks that hold one branch's continuation write open so a flow test can
//! start the next turn in a window it chose. Feature `testing` only: a
//! production build has no hook.
use std::sync::Mutex;
use tokio::sync::{mpsc, watch};

/// One branch's held write: the session and branch it belongs to, the turn
/// end it reports on arrival, and the release that lets it continue.
type Hold = (
    String,
    String,
    mpsc::UnboundedSender<u64>,
    watch::Receiver<bool>,
);
static HOLDS: Mutex<Vec<Hold>> = Mutex::new(Vec::new());

pub struct HeldWrite {
    session: String,
    branch: String,
    arrived: mpsc::UnboundedReceiver<u64>,
    release: watch::Sender<bool>,
}

impl HeldWrite {
    /// Resolves when a continuation on the branch reaches its write, with the
    /// `after` of the turn end it was decided on.
    pub async fn arrived(&mut self) -> u64 {
        self.arrived.recv().await.unwrap_or(0)
    }
    /// Lets the held write continue. Later writes of the branch pass through.
    pub fn release(&self) {
        self.release.send_replace(true);
    }
}

impl Drop for HeldWrite {
    fn drop(&mut self) {
        self.release.send_replace(true);
        let session = std::mem::take(&mut self.session);
        let branch = std::mem::take(&mut self.branch);
        HOLDS
            .lock()
            .unwrap()
            .retain(|(held_session, held_branch, ..)| {
                *held_session != session || *held_branch != branch
            });
    }
}

/// Holds the next continuation write of `branch_id` in `session_id`, after its
/// decision. The tests of one process run in parallel, so the key is the line.
pub fn hold_write(session_id: &str, branch_id: &str) -> HeldWrite {
    let (arrived, arrivals) = mpsc::unbounded_channel();
    let (release, released) = watch::channel(false);
    let mut holds = HOLDS.lock().unwrap();
    holds.retain(|(session, branch, ..)| session != session_id || branch != branch_id);
    holds.push((
        session_id.to_owned(),
        branch_id.to_owned(),
        arrived,
        released,
    ));
    HeldWrite {
        session: session_id.to_owned(),
        branch: branch_id.to_owned(),
        arrived: arrivals,
        release,
    }
}

/// Stops the continuation write of `branch_id` in `session_id` until the test
/// releases it.
pub(crate) async fn write_held(session_id: &str, branch_id: &str, after: u64) {
    let held = {
        let holds = HOLDS.lock().unwrap();
        holds
            .iter()
            .find(|(session, branch, ..)| session == session_id && branch == branch_id)
            .map(|(_, _, arrived, release)| (arrived.clone(), release.clone()))
    };
    let Some((arrived, mut release)) = held else {
        return;
    };
    let _ = arrived.send(after);
    while !*release.borrow_and_update() {
        if release.changed().await.is_err() {
            return;
        }
    }
}

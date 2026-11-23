//! Hooks that hold a line's live configuration read open and report when it
//! has to wait for the Dispatcher or for a recovery result to publish, so a
//! test commits an adoption or starts a close in a window it chose. Feature
//! `testing` only: a production build has no hook.
use std::sync::Mutex;
use tokio::sync::{mpsc, watch};

/// Where a held read stops: after it captured its cutoff, or after it passed
/// its phase check and before it advances.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Stage {
    Captured,
    Checked,
}

/// One line's held read: where it stops, where it arrived, whether it is
/// waiting for the Dispatcher, whether it is waiting for a written result to
/// publish, and the release that lets it continue.
type Hold = (
    String,
    String,
    Stage,
    mpsc::UnboundedSender<()>,
    mpsc::UnboundedSender<()>,
    mpsc::UnboundedSender<()>,
    watch::Receiver<bool>,
);
static HOLDS: Mutex<Vec<Hold>> = Mutex::new(Vec::new());

pub struct HeldRead {
    session: String,
    branch: String,
    arrived: mpsc::UnboundedReceiver<()>,
    waiting: mpsc::UnboundedReceiver<()>,
    publishing: mpsc::UnboundedReceiver<()>,
    release: watch::Sender<bool>,
}
impl HeldRead {
    /// Resolves when the read has captured its cutoff and stopped.
    pub async fn arrived(&mut self) {
        let _ = self.arrived.recv().await;
    }
    /// Resolves when the read has to wait for the Dispatcher to reach its
    /// cutoff; a read that never waits never resolves.
    pub async fn waiting(&mut self) {
        let _ = self.waiting.recv().await;
    }
    /// Resolves when the read has to wait for a written recovery result to
    /// publish; a read that never waits never resolves.
    pub async fn publishing(&mut self) {
        let _ = self.publishing.recv().await;
    }
    /// Lets the stopped read continue. Later reads of the line pass through.
    pub fn release(&self) {
        self.release.send_replace(true);
    }
}
impl Drop for HeldRead {
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

/// Holds the next live configuration read of `branch_id` in `session_id`.
/// The tests of one process run in parallel, so the key is the line.
pub fn hold_read(session_id: &str, branch_id: &str) -> HeldRead {
    hold(session_id, branch_id, Stage::Captured)
}

/// Holds the next live configuration read of `branch_id` in `session_id`
/// after it has passed its phase check and before it advances: the line can
/// start closing in that window.
pub fn hold_read_after_check(session_id: &str, branch_id: &str) -> HeldRead {
    hold(session_id, branch_id, Stage::Checked)
}

fn hold(session_id: &str, branch_id: &str, stage: Stage) -> HeldRead {
    let (arrived, arrivals) = mpsc::unbounded_channel();
    let (waiting, waits) = mpsc::unbounded_channel();
    let (publishing, publishes) = mpsc::unbounded_channel();
    let (release, released) = watch::channel(false);
    let mut holds = HOLDS.lock().unwrap();
    holds.retain(|(session, branch, ..)| session != session_id || branch != branch_id);
    holds.push((
        session_id.to_owned(),
        branch_id.to_owned(),
        stage,
        arrived,
        waiting,
        publishing,
        released,
    ));
    HeldRead {
        session: session_id.to_owned(),
        branch: branch_id.to_owned(),
        arrived: arrivals,
        waiting: waits,
        publishing: publishes,
        release,
    }
}

/// One line's held read as the hook sees it: the arrival, Dispatcher wait and
/// publication wait reporters, and the release.
type Signal = mpsc::UnboundedSender<()>;
type Armed = (Signal, Signal, Signal, watch::Receiver<bool>);

fn armed(session_id: &str, branch_id: &str) -> Option<Armed> {
    let holds = HOLDS.lock().unwrap();
    let (_, _, _, arrived, waiting, publishing, release) = holds
        .iter()
        .find(|(session, branch, ..)| session == session_id && branch == branch_id)?;
    Some((
        arrived.clone(),
        waiting.clone(),
        publishing.clone(),
        release.clone(),
    ))
}

/// The arrival and release of the hold armed for `stage`, if any.
fn stopped(
    session_id: &str,
    branch_id: &str,
    stage: Stage,
) -> Option<(Signal, watch::Receiver<bool>)> {
    let holds = HOLDS.lock().unwrap();
    let (_, _, _, arrived, _, _, release) = holds.iter().find(|(session, branch, held, ..)| {
        session == session_id && branch == branch_id && *held == stage
    })?;
    Some((arrived.clone(), release.clone()))
}

async fn stop(session_id: &str, branch_id: &str, stage: Stage) {
    let Some((arrived, mut release)) = stopped(session_id, branch_id, stage) else {
        return;
    };
    let _ = arrived.send(());
    while !*release.borrow_and_update() {
        if release.changed().await.is_err() {
            return;
        }
    }
}

/// Stops the read until the test releases it; the cutoff is already captured.
pub(super) async fn read_captured(session_id: &str, branch_id: &str) {
    stop(session_id, branch_id, Stage::Captured).await;
}

/// Stops the read after it has passed its phase check and before it advances.
pub(super) async fn read_checked(session_id: &str, branch_id: &str) {
    stop(session_id, branch_id, Stage::Checked).await;
}

/// Reports that the read cannot answer at its cutoff before the Dispatcher
/// reaches it.
pub(super) fn read_waiting(session_id: &str, branch_id: &str) {
    if let Some((_, waiting, _, _)) = armed(session_id, branch_id) {
        let _ = waiting.send(());
    }
}

/// Reports that the read waits for a written recovery result to publish.
pub(super) fn read_publishing(session_id: &str, branch_id: &str) {
    if let Some((_, _, publishing, _)) = armed(session_id, branch_id) {
        let _ = publishing.send(());
    }
}

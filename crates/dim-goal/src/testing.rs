//! Hooks that hold one branch's continuation write open so a flow test can
//! start the next turn in a window it chose. Feature `testing` only: a
//! production build has no hook.
use std::sync::{Arc, Condvar, Mutex};
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

/// A turn-end step of one branch a test can hold open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    /// Before `do_turn_end` reads the fold.
    Decision,
    /// After a wait read the committed cutoff it delivers, before it waits for
    /// the dispatcher to reach that cutoff.
    Cutoff,
    /// After the evaluation decision and before `treeCommands.create`.
    Create,
    /// After `do_turn_end` took every decision it carries.
    Done,
}

type StageHold = (
    String,
    String,
    Stage,
    u64,
    Option<u64>,
    mpsc::UnboundedSender<u64>,
    watch::Sender<bool>,
);
static STAGE_HOLDS: Mutex<Vec<StageHold>> = Mutex::new(Vec::new());

pub struct HeldStage {
    session: String,
    branch: String,
    arrived: mpsc::UnboundedReceiver<u64>,
    release: watch::Sender<bool>,
}

impl HeldStage {
    /// Resolves when the held step is reached, with the turn end it follows.
    pub async fn arrived(&mut self) -> u64 {
        self.arrived.recv().await.unwrap_or(0)
    }
    pub fn release(&self) {
        self.release.send_replace(true);
    }
}

impl Drop for HeldStage {
    fn drop(&mut self) {
        self.release.send_replace(true);
        let session = std::mem::take(&mut self.session);
        let branch = std::mem::take(&mut self.branch);
        STAGE_HOLDS
            .lock()
            .unwrap()
            .retain(|(held_session, held_branch, ..)| {
                *held_session != session || *held_branch != branch
            });
    }
}

/// Holds a turn-end step of `branch_id` in `session_id`: the next step at
/// `stage` whose `after` is at least `min` (and equals `after` when given).
pub fn hold_stage(
    session_id: &str,
    branch_id: &str,
    stage: Stage,
    min: u64,
    after: Option<u64>,
) -> HeldStage {
    let (arrived, wait) = mpsc::unbounded_channel();
    let (release, _) = watch::channel(false);
    STAGE_HOLDS.lock().unwrap().push((
        session_id.to_owned(),
        branch_id.to_owned(),
        stage,
        min,
        after,
        arrived,
        release.clone(),
    ));
    HeldStage {
        session: session_id.to_owned(),
        branch: branch_id.to_owned(),
        arrived: wait,
        release,
    }
}

pub(crate) async fn stage_held(session_id: &str, branch_id: &str, stage: Stage, after: u64) {
    let held = {
        let mut holds = STAGE_HOLDS.lock().unwrap();
        let Some(index) = holds.iter().position(|hold| {
            hold.0 == session_id
                && hold.1 == branch_id
                && hold.2 == stage
                && after >= hold.3
                && hold.4.is_none_or(|wanted| wanted == after)
        }) else {
            return;
        };
        let hold = holds.remove(index);
        (hold.5, hold.6.subscribe())
    };
    let (arrived, mut release) = held;
    let _ = arrived.send(after);
    while !*release.borrow_and_update() {
        if release.changed().await.is_err() {
            return;
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Folded {
    TurnStart,
    Input,
}

type FoldHold = (
    String,
    String,
    Folded,
    u64,
    mpsc::UnboundedSender<u64>,
    Arc<(Mutex<bool>, Condvar)>,
);
static FOLD_HOLDS: Mutex<Vec<FoldHold>> = Mutex::new(Vec::new());

pub struct HeldFold {
    session: String,
    branch: String,
    arrived: mpsc::UnboundedReceiver<u64>,
    release: Arc<(Mutex<bool>, Condvar)>,
}

impl HeldFold {
    pub async fn arrived(&mut self) -> u64 {
        self.arrived.recv().await.unwrap_or(0)
    }
    pub fn release(&self) {
        let (opened, wake) = &*self.release;
        *opened.lock().unwrap() = true;
        wake.notify_all();
    }
}

impl Drop for HeldFold {
    fn drop(&mut self) {
        self.release();
        let session = std::mem::take(&mut self.session);
        let branch = std::mem::take(&mut self.branch);
        FOLD_HOLDS
            .lock()
            .unwrap()
            .retain(|(held_session, held_branch, ..)| {
                *held_session != session || *held_branch != branch
            });
    }
}

/// Holds the next `turn/start` of `branch_id` in `session_id` at or after
/// `min`, before Goal folds it.
pub fn hold_turn_start(session_id: &str, branch_id: &str, min: u64) -> HeldFold {
    hold_fold(session_id, branch_id, Folded::TurnStart, min)
}

/// Holds the next `input/message` of `branch_id` in `session_id` at or after
/// `min`, before Goal folds it.
pub fn hold_input(session_id: &str, branch_id: &str, min: u64) -> HeldFold {
    hold_fold(session_id, branch_id, Folded::Input, min)
}

fn hold_fold(session_id: &str, branch_id: &str, folded: Folded, min: u64) -> HeldFold {
    let (arrived, wait) = mpsc::unbounded_channel();
    let release = Arc::new((Mutex::new(false), Condvar::new()));
    FOLD_HOLDS.lock().unwrap().push((
        session_id.to_owned(),
        branch_id.to_owned(),
        folded,
        min,
        arrived,
        release.clone(),
    ));
    HeldFold {
        session: session_id.to_owned(),
        branch: branch_id.to_owned(),
        arrived: wait,
        release,
    }
}

pub(crate) fn fold_turn_start(session_id: &str, branch_id: &str, sequence: u64) {
    fold_held(session_id, branch_id, Folded::TurnStart, sequence);
}

pub(crate) fn fold_input(session_id: &str, branch_id: &str, sequence: u64) {
    fold_held(session_id, branch_id, Folded::Input, sequence);
}

fn fold_held(session_id: &str, branch_id: &str, folded: Folded, sequence: u64) {
    let held = {
        let mut holds = FOLD_HOLDS.lock().unwrap();
        let Some(index) = holds.iter().position(|hold| {
            hold.0 == session_id && hold.1 == branch_id && hold.2 == folded && sequence >= hold.3
        }) else {
            return;
        };
        let hold = holds.remove(index);
        (hold.4, hold.5)
    };
    let (arrived, release) = held;
    let _ = arrived.send(sequence);
    let (opened, wake) = &*release;
    let mut opened = opened.lock().unwrap();
    while !*opened {
        opened = wake.wait(opened).unwrap();
    }
}

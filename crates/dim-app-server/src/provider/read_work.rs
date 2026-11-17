use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};

static SOURCE_PAYLOADS: AtomicUsize = AtomicUsize::new(0);

/// Process-wide diagnostic counter; compare in an isolated flow-test process.
pub fn source_payloads() -> usize {
    SOURCE_PAYLOADS.load(Ordering::Relaxed)
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ReadWork {
    pub candidate_keys: usize,
    pub candidate_vm_steps: usize,
    pub naming_rows: usize,
    pub stored_payloads: usize,
    pub source_payloads: usize,
}

thread_local! { static WORK: Cell<ReadWork> = Cell::new(ReadWork::default()); }

pub(crate) fn update(change: impl FnOnce(&mut ReadWork)) {
    WORK.with(|cell| {
        let mut work = cell.get();
        let previous = work.source_payloads;
        change(&mut work);
        SOURCE_PAYLOADS.fetch_add(
            work.source_payloads.saturating_sub(previous),
            Ordering::Relaxed,
        );
        cell.set(work);
    });
}

pub(crate) fn reset() {
    WORK.with(|cell| cell.set(ReadWork::default()));
}
pub(crate) fn get() -> ReadWork {
    WORK.with(Cell::get)
}

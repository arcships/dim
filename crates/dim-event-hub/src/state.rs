//! The install's state (design §2.1, §2.4, §2.5): what outlives one
//! generation of the `eventhub` row. `install_with` creates one instance and
//! the row builder captures it, so a factory rebuild — a reapply after a
//! dependency reload, or an off and on of the row — keeps every declaration,
//! and an in-flight change of one generation still serializes with the next
//! generation's. The state ends with the process.
use crate::{catalog::Catalog, store::Document};
use dim_agent_capabilities::host::StoredSessionFile;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, MutexGuard, RwLock},
};
use tokio::sync::{OwnedMutexGuard, OwnedSemaphorePermit, Semaphore};

/// How many `sessions` calls run at a time across every publish of the
/// install: the lookups and the inputs share the delivery budget
/// (design §2.5).
pub(crate) const MAX_DELIVERIES_IN_FLIGHT: usize = 8;

/// The catalog, the subscription view, the Session locks, the event locks
/// and the delivery budget, shared by every generation of the process part
/// (design §2.1).
pub struct InstanceState {
    catalog: Arc<Catalog>,
    /// The subscriptions and guide records every generation reads and
    /// changes (design §2.1, A.5): a change a detached publish still runs
    /// after its generation ended lands here, where the next generation
    /// reads it.
    subscriptions: RwLock<Document>,
    /// Serializes every change of the subscription view, the file write and
    /// the in-memory replacement alike, across generations (design A.5).
    change: tokio::sync::Mutex<()>,
    /// One lock per target Session (design §3.3).
    sessions: KeyedLocks,
    /// One lock per submission id (design §2.4).
    events: KeyedLocks,
    /// The install's delivery budget (design §2.5), not one per publish.
    budget: Arc<Semaphore>,
}

impl Default for InstanceState {
    fn default() -> Self {
        Self {
            catalog: Arc::new(Catalog::default()),
            subscriptions: RwLock::new(Document::default()),
            change: tokio::sync::Mutex::new(()),
            sessions: KeyedLocks::default(),
            events: KeyedLocks::default(),
            budget: Arc::new(Semaphore::new(MAX_DELIVERIES_IN_FLIGHT)),
        }
    }
}

impl InstanceState {
    pub(crate) fn catalog(&self) -> &Arc<Catalog> {
        &self.catalog
    }

    /// The subscription view as it is now.
    pub(crate) fn subscriptions(&self) -> Document {
        self.subscriptions
            .read()
            .expect("the subscription set")
            .clone()
    }

    /// Replaces the subscription view, after a load or a successful change.
    pub(crate) fn replace_subscriptions(&self, document: Document) {
        *self.subscriptions.write().expect("the subscription set") = document;
    }

    /// Waits for the change lock and holds it until the guard drops.
    pub(crate) async fn change_lock(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.change.lock().await
    }

    /// Waits for the lock of `session_id` and holds it until the guard drops
    /// (design §3.3).
    pub(crate) async fn session_lock(&self, session_id: &str) -> KeyedLock {
        self.sessions.lock(session_id).await
    }

    /// Waits for the lock of one event's submission id (design §2.4).
    pub(crate) async fn event_lock(&self, submission_id: &str) -> KeyedLock {
        self.events.lock(submission_id).await
    }

    /// Takes one of the install's delivery permits (design §2.5). A permit is
    /// held for one target branch's delivery, guide refresh included.
    pub(crate) async fn delivery_permit(&self) -> OwnedSemaphorePermit {
        self.budget
            .clone()
            .acquire_owned()
            .await
            .expect("the install's delivery budget is never closed")
    }
}

/// Locks named by a key, each created on first use and dropped when its last
/// holder releases it (design §2.5), so a long-lived install keeps no map
/// entry per Session or event it ever saw.
#[derive(Default)]
pub(crate) struct KeyedLocks {
    inner: Arc<KeyedLocksInner>,
}

#[derive(Default)]
struct KeyedLocksInner {
    locks: Mutex<HashMap<String, Arc<LockEntry>>>,
}

/// One key's lock, and the state that lives exactly as long as the entry: an
/// event's stored files, per Session, so a retry in the same process reuses
/// them (design §3.2).
struct LockEntry {
    lock: Arc<tokio::sync::Mutex<()>>,
    files: Mutex<HashMap<String, StoredSessionFile>>,
}

impl KeyedLocks {
    async fn lock(&self, key: &str) -> KeyedLock {
        let entry = {
            let mut locks = self.inner.locks.lock().expect("the lock map");
            locks
                .entry(key.to_owned())
                .or_insert_with(|| {
                    Arc::new(LockEntry {
                        lock: Arc::new(tokio::sync::Mutex::new(())),
                        files: Mutex::new(HashMap::new()),
                    })
                })
                .clone()
        };
        let guard = entry.lock.clone().lock_owned().await;
        KeyedLock {
            inner: self.inner.clone(),
            key: key.to_owned(),
            entry: Some(entry),
            guard: Some(guard),
        }
    }
}

/// One keyed lock held; dropping it drops the map's entry, and with it the
/// entry's files, when this was the last holder.
pub(crate) struct KeyedLock {
    inner: Arc<KeyedLocksInner>,
    key: String,
    entry: Option<Arc<LockEntry>>,
    guard: Option<OwnedMutexGuard<()>>,
}

impl KeyedLock {
    /// The files stored under this key so far, while its lock is held.
    pub(crate) fn files(&self) -> MutexGuard<'_, HashMap<String, StoredSessionFile>> {
        self.entry
            .as_ref()
            .expect("a keyed lock holds its entry until it drops")
            .files
            .lock()
            .expect("the entry's files")
    }
}

impl Drop for KeyedLock {
    fn drop(&mut self) {
        self.guard.take();
        let entry = self
            .entry
            .take()
            .expect("a keyed lock holds its entry until it drops");
        let mut locks = self.inner.locks.lock().expect("the lock map");
        // This guard and the map hold the entry; a waiter, which cloned it
        // before awaiting, would hold a third reference.
        if Arc::strong_count(&entry) == 2 {
            locks.remove(&self.key);
        }
    }
}

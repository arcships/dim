//! The connection-bound change stream of `schedule.watch` (design §11.2):
//! the call stays open for the connection's watch, and the jobs and run
//! sources send one progress item per change, numbered per watch, so a
//! client reads the owner's records after the first item and again after a
//! change.
//!
//! A watch belongs to the generation that admitted it and ends as its call's
//! reply, not as a notice: rutis cancels a consumer's generation token at the
//! provider's withdrawal, before any listener or task of the stopping
//! generation can run, so the `operation_interrupted` reply of the call's
//! admission is the end a client can observe.

use crate::runs::Run;
use dim_agent_capabilities::methods::{Caller, Progress};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

/// The watchers of one generation, at most one per connection: a second
/// `schedule.watch` from the same connection takes over, so one change sends
/// one item. The whole set ends with the generation.
#[derive(Default)]
pub(crate) struct Watchers(Mutex<HashMap<String, Arc<Watch>>>);

/// One connection's watch: the call's progress channel, its sequence, and
/// the signal a later watch on the same connection fires so the earlier call
/// answers `{}`.
struct Watch {
    progress: Progress,
    /// The number of the next item sent to this watch; 0 is `watching`.
    seq: Mutex<u64>,
    replaced: CancellationToken,
}

impl Watch {
    /// Sends one item on this watch's call, numbered in order. The `seq`
    /// guard spans the send, so numbering and delivery are one step and two
    /// senders cannot deliver `1` after `2`. The sink is a non-blocking
    /// offer, so the guard is held briefly and never across an await. The
    /// lock order is `Watchers` map -> [`Watch::seq`] -> the `Progress`
    /// mutex; `Progress::close` takes only its own mutex, so the order
    /// cannot cycle. Progress is best effort: an item the connection cannot
    /// take is dropped, which is why every item carries its number.
    fn send(&self, mut item: Value) {
        let Some(fields) = item.as_object_mut() else {
            return;
        };
        let mut seq = self.seq.lock().unwrap();
        fields.insert("seq".into(), json!(*seq));
        *seq += 1;
        self.progress.send(item);
    }
}

impl Watchers {
    /// Registers the calling connection's watch and keeps this call open
    /// until the generation unloads (`cancel`), the connection closes, or a
    /// later `schedule.watch` on the same connection takes over.
    ///
    /// `{seq: 0, kind: "watching"}` is sent while the set holds this watch
    /// and before any change can be sent to it, so a client that reads after
    /// it sees every later change.
    pub(crate) async fn watch(
        &self,
        caller: Caller,
        progress: Progress,
        cancel: &CancellationToken,
    ) {
        let connection = caller.id().to_owned();
        let watch = Arc::new(Watch {
            progress,
            seq: Mutex::new(0),
            replaced: CancellationToken::new(),
        });
        let earlier = {
            let mut watchers = self.0.lock().unwrap();
            let earlier = watchers.insert(connection.clone(), watch.clone());
            watch.send(json!({"kind": "watching"}));
            earlier
        };
        if let Some(earlier) = earlier {
            earlier.replaced.cancel();
        }
        tokio::select! {
            _ = cancel.cancelled() => {}
            _ = caller.closed() => {}
            _ = watch.replaced.cancelled() => {}
        }
        let mut watchers = self.0.lock().unwrap();
        if watchers
            .get(&connection)
            .is_some_and(|current| Arc::ptr_eq(current, &watch))
        {
            watchers.remove(&connection);
        }
    }

    /// A committed write to the `schedule` Settings document.
    pub(crate) fn jobs(&self) {
        self.send(json!({"kind": "jobs"}));
    }

    /// One state the run's record took, `terminal` on the last.
    pub(crate) fn run(&self, run: &Run) {
        self.send(json!({
            "kind": "run",
            "jobId": run.job_id,
            "runId": run.run_id,
            "state": run.state.name(),
            "terminal": run.state.is_terminal(),
        }));
    }

    fn send(&self, item: Value) {
        let watchers: Vec<Arc<Watch>> = self.0.lock().unwrap().values().cloned().collect();
        for watch in watchers {
            watch.send(item.clone());
        }
    }
}

//! The cancel/fire boundary through a real App Server (design §10.3): a
//! cancel that races the timer pass either settles the record before the
//! pass observes it — no input — or observes the state the pass settled;
//! `cancelled` with the input written is impossible. The pass pauses in
//! `pause` after its input write and before its settle write, while it holds
//! the wakeup lock, so the interleaving is deterministic.
use crate::support;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::{mpsc, oneshot};

static PAUSES: Mutex<BTreeMap<String, mpsc::UnboundedSender<oneshot::Sender<()>>>> =
    Mutex::new(BTreeMap::new());

/// Called by the timer pass after it has written `wakeup_id`'s input and
/// before it settles the record, while it holds the wakeup lock. Unregistered
/// ids return at once, so the seam is inert unless a test registered it.
pub(crate) async fn pause(wakeup_id: &str) {
    let sender = PAUSES.lock().unwrap().get(wakeup_id).cloned();
    if let Some(sender) = sender {
        let (release, released) = oneshot::channel();
        if sender.send(release).is_ok() {
            tokio::time::timeout(Duration::from_secs(30), released)
                .await
                .expect("the paused pass is released by the test")
                .ok();
        }
    }
}

struct PassPause {
    wakeup_id: String,
    arriving: mpsc::UnboundedReceiver<oneshot::Sender<()>>,
}

impl PassPause {
    fn new(wakeup_id: &str) -> Self {
        let (sender, arriving) = mpsc::unbounded_channel();
        assert!(
            PAUSES
                .lock()
                .unwrap()
                .insert(wakeup_id.into(), sender)
                .is_none(),
            "one pause per wakeup id"
        );
        Self {
            wakeup_id: wakeup_id.into(),
            arriving,
        }
    }

    /// Waits for the pass to reach the seam; the returned sender releases it.
    async fn next(&mut self) -> oneshot::Sender<()> {
        tokio::time::timeout(Duration::from_secs(30), self.arriving.recv())
            .await
            .expect("the timer pass reaches its input-write/settle boundary")
            .expect("the pause channel stays open")
    }
}

impl Drop for PassPause {
    fn drop(&mut self) {
        PAUSES.lock().unwrap().remove(&self.wakeup_id);
    }
}

fn stored(home: &Path) -> Vec<Value> {
    let bytes =
        std::fs::read(home.join("scheduler/wakeups.json")).expect("the pass writes the store");
    let envelope: Value = serde_json::from_slice(&bytes).unwrap();
    let inner: Value = serde_json::from_str(envelope["text"].as_str().unwrap()).unwrap();
    inner["wakeups"].as_array().cloned().unwrap_or_default()
}

fn wakeup_inputs(events: &[Value]) -> Vec<&Value> {
    events
        .iter()
        .filter(|event| {
            event["type"] == "input"
                && event["name"] == "message"
                && event["data"]["blocks"].as_array().is_some_and(|blocks| {
                    blocks.iter().any(|block| {
                        block["text"]
                            .as_str()
                            .is_some_and(|text| text.contains("dim-scheduler kind=\"wakeup\""))
                    })
                })
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_cancel_racing_the_pass_cannot_report_cancelled_with_the_input_written() {
    tokio::time::timeout(Duration::from_secs(90), async {
        let home = support::home();
        let model = Arc::new(support::Script::default());
        let server = support::start(home.path(), model).await;
        let mut client = support::connect(&server).await;
        let mut cancelling = support::connect(&server).await;
        let session = support::create_session(&mut client, home.path(), true).await;

        // Due in two seconds: the timer pass fires it, and the pause below is
        // registered well before its instant.
        let at = (support::clock(home.path()).now() + chrono::Duration::seconds(2))
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let created: Value = client
            .rpc(
                "session.wakeup.create",
                json!({"sessionId": session, "branchId": "main", "at": at,
                    "prompt": "Race the cancel."}),
            )
            .await
            .unwrap();
        let wakeup_id = created["wakeupId"].as_str().unwrap().to_owned();
        let mut gate = PassPause::new(&wakeup_id);

        // The pass now holds the wakeup lock with the input already written
        // and the record not yet settled. A cancel must wait for it instead
        // of settling behind the write.
        let release = gate.next().await;
        let mut cancel = Box::pin(cancelling.rpc(
            "session.wakeup.cancel",
            json!({"sessionId": session, "wakeupId": wakeup_id}),
        ));
        // A generous window: a cancel that reached the lock settles at once
        // when the pass is not holding it (the defect this test guards), and
        // one that is merely slow to arrive cannot finish while it is held.
        assert!(
            tokio::time::timeout(Duration::from_secs(1), &mut cancel)
                .await
                .is_err(),
            "the cancel waits for the pass's lock"
        );

        // Released, the pass settles `fired`; the cancel then observes the
        // settled record and reports its state.
        release.send(()).expect("the paused pass is still waiting");
        let reply = cancel.await.expect("the cancel reaches the settled record");
        assert_eq!(
            reply,
            json!({"state": "fired"}),
            "the pass settled the record first"
        );

        // The consistent outcome: the record says fired and exactly one
        // wakeup input exists — never cancelled with the input present.
        let stored = stored(home.path());
        assert_eq!(stored.len(), 1, "{stored:?}");
        assert_eq!(stored[0]["state"], json!("fired"), "{stored:?}");
        let history: Value = client
            .rpc(
                "session.history",
                json!({"sessionId": session, "branchId": "main", "after": 0, "limit": 256}),
            )
            .await
            .unwrap();
        let events = history["events"].as_array().unwrap();
        assert_eq!(wakeup_inputs(events).len(), 1, "{history}");

        drop(gate);
        drop(client);
        server.shutdown();
        server.join().await.unwrap();
    })
    .await
    .expect("the cancel/fire race finishes within its deadline");
}

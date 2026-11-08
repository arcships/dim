//! The input entry's atomic batch: several validated inputs commit together
//! or not at all, and a replayed batch returns what the branch already holds.
mod support;

use std::{
    ops::RangeInclusive,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

use dim_agent_capabilities::{defaults, timeline_tree::TreeCommands};
use dim_agent_runtime::testing::{MinimalHost, branch_events};
use dim_agent_runtime::{
    AgentBuilder, AgentConfig, AgentError, BoxFuture, MemoryTimelineStore, PublishCallback, Result,
    Session, SessionConfig, StoredBranch, StoredEvent, TimelineKey, TimelineQuery,
    TimelineQueryPage, TimelineStore, TimelineTrace,
};
use dim_agent_types::{AgentEvent, ContentBlock, EventKind, InputMessageData, InputMode};
use support::Events;
use tokio::sync::oneshot;

fn input(id: &str, text: &str) -> InputMessageData {
    InputMessageData {
        submission_id: Some(id.into()),
        ..InputMessageData::text(text, Some(InputMode::Queue))
    }
}

/// The text one committed input carries.
fn text_of(event: &AgentEvent) -> String {
    let EventKind::InputMessage(data) = &event.kind else {
        panic!("not an input: {event:?}");
    };
    match data.user.as_ref().map(|user| user.content.as_slice()) {
        Some([ContentBlock::Text { text }]) => text.clone(),
        other => panic!("unexpected input content: {other:?}"),
    }
}

/// Every input/message the branch published, oldest first.
async fn published_inputs(session: &Session) -> Vec<Arc<AgentEvent>> {
    branch_events(session, "main", ..)
        .await
        .unwrap()
        .into_iter()
        .filter(|event| matches!(event.kind, EventKind::InputMessage(_)))
        .collect()
}

struct Fixture {
    host: MinimalHost,
    session: Session,
}

async fn start(store: Arc<dyn TimelineStore>) -> Fixture {
    let mut builder = AgentBuilder::new(AgentConfig::default());
    builder.timeline_store(store);
    defaults::mock_hosts(&mut builder);
    defaults::install(&mut builder);
    let host = MinimalHost::start(&builder).await.unwrap();
    let session = host.create_session(SessionConfig::default()).await.unwrap();
    Fixture { host, session }
}

fn memory() -> Arc<dyn TimelineStore> {
    Arc::new(MemoryTimelineStore::default())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_batch_commits_in_order_at_contiguous_sequences() {
    let Fixture { host, session } = start(memory()).await;
    let ids = ["test:one", "test:two", "test:three"];
    let events = session
        .inputs(
            vec![
                input(ids[0], "one"),
                input(ids[1], "two"),
                input(ids[2], "three"),
            ],
            "main",
        )
        .await
        .unwrap();
    assert_eq!(events.len(), 3, "the batch returns one event per input");
    let first = events[0].sequence;
    for (index, event) in events.iter().enumerate() {
        assert_eq!(
            event.sequence,
            first + index as u64,
            "the batch's sequences are contiguous: {events:?}"
        );
        let EventKind::InputMessage(data) = &event.kind else {
            panic!("not an input: {event:?}");
        };
        assert_eq!(
            data.submission_id.as_deref(),
            Some(ids[index]),
            "the batch keeps the given order: {events:?}"
        );
        assert_eq!(text_of(event), ["one", "two", "three"][index]);
    }
    session.batch_end("main", first + 2).await.unwrap();
    for interior in [first, first + 1] {
        assert!(
            matches!(
                session.batch_end("main", interior).await,
                Err(AgentError::InvalidInput(_))
            ),
            "sequence {interior} is inside the batch"
        );
    }
    let history = branch_events(&session, "main", first..=first + 2)
        .await
        .unwrap();
    assert_eq!(
        history
            .iter()
            .map(|event| text_of(event))
            .collect::<Vec<_>>(),
        ["one", "two", "three"],
        "the published history holds the batch in order"
    );
    host.close().await.unwrap();
}

/// The store appended the batch as one write, so its publication is one
/// callback: until it runs, no subscriber and no reader sees any input, and
/// once it runs both arrive in order.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_subscriber_receives_the_batch_in_order_and_nothing_before_it_commits() {
    let store = Arc::new(GatedStore::default());
    let Fixture { host, session } = start(store.clone()).await;
    let (entered, release) = store.hold_second_input();
    let mut events = Events::follow(
        host.root(),
        &session,
        "main",
        session.sequence("main").unwrap(),
    )
    .await;
    let caller = session.clone();
    let submitting = tokio::spawn(async move {
        caller
            .inputs(
                vec![input("test:one", "one"), input("test:two", "two")],
                "main",
            )
            .await
    });
    tokio::time::timeout(Duration::from_secs(10), entered)
        .await
        .expect("the batch reaches the store")
        .expect("the store reports the held publication");
    assert!(
        published_inputs(&session).await.is_empty(),
        "no input is published before the batch's single publication"
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(200), events.next())
            .await
            .is_err(),
        "the subscriber receives nothing before the batch commits"
    );
    release.send(()).unwrap();
    let returned = submitting.await.unwrap().unwrap();
    assert_eq!(returned.len(), 2);
    let mut received = Vec::new();
    while received.len() < 2 {
        let event = tokio::time::timeout(Duration::from_secs(10), events.next())
            .await
            .expect("the subscriber receives the batch")
            .expect("the Session channel stays open");
        if matches!(event.kind, EventKind::InputMessage(_)) {
            received.push(event);
        }
    }
    assert_eq!(
        received
            .iter()
            .map(|event| text_of(event))
            .collect::<Vec<_>>(),
        ["one", "two"],
        "one subscriber receives the inputs in order"
    );
    assert_eq!(
        received
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![returned[0].sequence, returned[1].sequence]
    );
    host.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_invalid_second_input_writes_nothing() {
    let Fixture { host, session } = start(memory()).await;
    let tree = session.session_service::<TreeCommands>().unwrap();
    let before = session.sequence("main").unwrap();
    let invalid = InputMessageData {
        submission_id: Some("test:invalid".into()),
        ..InputMessageData::text("", Some(InputMode::Queue))
    };
    let error = tree
        .get("main")
        .unwrap()
        .inputs(vec![input("test:valid", "valid"), invalid])
        .await;
    assert!(
        matches!(error, Err(AgentError::InvalidInput(_))),
        "an invalid input refuses the batch: {error:?}"
    );
    assert_eq!(
        session.sequence("main").unwrap(),
        before,
        "the valid input is not written either"
    );
    assert_eq!(session.lookup("main", "test:valid").await.unwrap(), None);
    assert!(published_inputs(&session).await.is_empty());
    host.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_empty_batch_is_refused() {
    let Fixture { host, session } = start(memory()).await;
    let tree = session.session_service::<TreeCommands>().unwrap();
    let before = session.sequence("main").unwrap();
    let error = tree.inputs("main".into(), Vec::new()).await;
    assert!(
        matches!(error, Err(AgentError::InvalidInput(_))),
        "an empty batch is invalid: {error:?}"
    );
    assert_eq!(session.sequence("main").unwrap(), before);
    host.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_duplicate_submission_id_inside_the_batch_is_refused() {
    let Fixture { host, session } = start(memory()).await;
    let before = session.sequence("main").unwrap();
    let error = session
        .inputs(
            vec![input("test:same", "one"), input("test:same", "two")],
            "main",
        )
        .await;
    assert!(
        matches!(error, Err(AgentError::InvalidInput(_))),
        "one id twice is an invalid batch: {error:?}"
    );
    assert_eq!(session.sequence("main").unwrap(), before);
    assert_eq!(session.lookup("main", "test:same").await.unwrap(), None);
    host.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_full_replay_returns_the_committed_events() {
    let Fixture { host, session } = start(memory()).await;
    let batch = || vec![input("test:one", "one"), input("test:two", "two")];
    let first = session.inputs(batch(), "main").await.unwrap();
    session.idle("main").await.unwrap();
    let tree = session.session_service::<TreeCommands>().unwrap();
    let replay = tree.inputs("main".into(), batch()).await.unwrap();
    assert_eq!(
        replay
            .iter()
            .map(|event| (event.id.clone(), event.sequence))
            .collect::<Vec<_>>(),
        first
            .iter()
            .map(|event| (event.id.clone(), event.sequence))
            .collect::<Vec<_>>(),
        "a replayed batch returns the existing events in order"
    );
    let inputs = published_inputs(&session).await;
    for id in ["test:one", "test:two"] {
        assert_eq!(
            inputs
                .iter()
                .filter(|event| {
                    matches!(&event.kind, EventKind::InputMessage(data)
                        if data.submission_id.as_deref() == Some(id))
                })
                .count(),
            1,
            "the replay writes nothing"
        );
    }
    host.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_partial_replay_fails_and_writes_nothing() {
    let Fixture { host, session } = start(memory()).await;
    let first = session
        .inputs(vec![input("test:one", "one")], "main")
        .await
        .unwrap();
    session.idle("main").await.unwrap();
    let before = session.sequence("main").unwrap();
    let error = session
        .inputs(
            vec![input("test:one", "one"), input("test:two", "two")],
            "main",
        )
        .await;
    assert!(
        matches!(error, Err(AgentError::PartialSubmission(ref uncommitted)) if uncommitted.contains("test:two")),
        "a partly committed batch fails with the uncommitted id: {error:?}"
    );
    assert_eq!(
        session.sequence("main").unwrap(),
        before,
        "the partial replay writes nothing"
    );
    assert_eq!(session.lookup("main", "test:two").await.unwrap(), None);
    assert_eq!(
        session
            .lookup("main", "test:one")
            .await
            .unwrap()
            .unwrap()
            .id,
        first[0].id,
        "the committed input is unchanged"
    );
    host.close().await.unwrap();
}

/// [`MemoryTimelineStore`] that holds the publication of the append carrying
/// the second input.
#[derive(Default)]
struct GatedStore {
    memory: MemoryTimelineStore,
    armed: AtomicBool,
    /// Input messages appended since the gate was armed.
    inputs: AtomicUsize,
    entered: Mutex<Option<oneshot::Sender<()>>>,
    release: Mutex<Option<oneshot::Receiver<()>>>,
}

impl GatedStore {
    /// Arms the gate: the returned receiver fires once the append carrying the
    /// second input is stored, and its publication waits for the returned
    /// sender. One input per append (a commit of separate inputs) holds the
    /// second append; one append carrying both holds that append.
    fn hold_second_input(&self) -> (oneshot::Receiver<()>, oneshot::Sender<()>) {
        let (entered, waiting) = oneshot::channel();
        let (release, held) = oneshot::channel();
        *self.entered.lock().unwrap() = Some(entered);
        *self.release.lock().unwrap() = Some(held);
        self.inputs.store(0, Ordering::SeqCst);
        self.armed.store(true, Ordering::SeqCst);
        (waiting, release)
    }
}

impl TimelineStore for GatedStore {
    fn create_session<'a>(
        &'a self,
        session_id: &'a str,
        config: &'a SessionConfig,
    ) -> BoxFuture<'a, Result<()>> {
        self.memory.create_session(session_id, config)
    }
    fn rollback_session<'a>(&'a self, session_id: &'a str) -> BoxFuture<'a, Result<()>> {
        self.memory.rollback_session(session_id)
    }
    fn release_session<'a>(&'a self, session_id: &'a str) -> BoxFuture<'a, Result<()>> {
        self.memory.release_session(session_id)
    }
    fn session_config<'a>(
        &'a self,
        session_id: &'a str,
    ) -> BoxFuture<'a, Result<Option<SessionConfig>>> {
        self.memory.session_config(session_id)
    }
    fn branches<'a>(&'a self, session_id: &'a str) -> BoxFuture<'a, Result<Vec<StoredBranch>>> {
        self.memory.branches(session_id)
    }
    fn head<'a>(&'a self, key: &'a TimelineKey) -> BoxFuture<'a, Result<u64>> {
        self.memory.head(key)
    }
    fn append(
        &self,
        key: TimelineKey,
        events: Vec<StoredEvent>,
        publish: PublishCallback,
    ) -> Result<()> {
        if self.armed.load(Ordering::SeqCst) {
            let count = events
                .iter()
                .filter(|stored| matches!(stored.event.kind, EventKind::InputMessage(_)))
                .count();
            let total = self.inputs.fetch_add(count, Ordering::SeqCst) + count;
            if total >= 2 {
                let gate = self
                    .entered
                    .lock()
                    .unwrap()
                    .take()
                    .zip(self.release.lock().unwrap().take());
                if let Some((entered, release)) = gate {
                    self.armed.store(false, Ordering::SeqCst);
                    return self.memory.append(
                        key,
                        events,
                        Box::new(move |result| {
                            tokio::spawn(async move {
                                let _ = entered.send(());
                                let _ = release.await;
                                publish(result);
                            });
                        }),
                    );
                }
            }
        }
        self.memory.append(key, events, publish)
    }
    fn batch_end<'a>(&'a self, key: &'a TimelineKey, sequence: u64) -> BoxFuture<'a, Result<()>> {
        self.memory.batch_end(key, sequence)
    }
    fn query<'a>(
        &'a self,
        key: &'a TimelineKey,
        query: &'a TimelineQuery,
        from: u64,
        through: u64,
    ) -> BoxFuture<'a, Result<TimelineQueryPage>> {
        self.memory.query(key, query, from, through)
    }
    fn read_segments<'a>(
        &'a self,
        session_id: &'a str,
        segments: Vec<(String, RangeInclusive<u64>)>,
    ) -> BoxFuture<'a, Result<Vec<Vec<StoredEvent>>>> {
        self.memory.read_segments(session_id, segments)
    }
    fn trace<'a>(
        &'a self,
        key: &'a TimelineKey,
        sequence: u64,
    ) -> BoxFuture<'a, Result<Option<TimelineTrace>>> {
        self.memory.trace(key, sequence)
    }
}

//! The opening handover of a rebuilding part ([ADR 020 D3](../../../docs/adr/020-timeline-read-convergence.md#d3-replay-is-removed)):
//! subscribe and hold live events, read the branch's facts with the query plus
//! its unpublished tail, then release only what lies above the cutoff, so
//! every fact is folded exactly once both when the tail is empty and when a
//! fact is written but not yet published.

use dim_agent_runtime::testing::{MinimalHost, timeline_events};
use dim_agent_runtime::*;
use dim_agent_types::*;
use rutis::Ctx;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, Key, TypeKey};
use std::{
    collections::VecDeque,
    ops::RangeInclusive,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

/// Publish callbacks a test holds back until it releases them.
type HeldPublishes = Arc<Mutex<VecDeque<(PublishCallback, Result<()>)>>>;

/// Memory storage whose publications wait for [`Self::release`], so a test can
/// write a fact and keep it unpublished.
struct Held {
    memory: MemoryTimelineStore,
    held: HeldPublishes,
    released: Arc<AtomicBool>,
}
impl Default for Held {
    fn default() -> Self {
        Self {
            memory: MemoryTimelineStore::default(),
            held: Arc::default(),
            released: Arc::new(AtomicBool::new(true)),
        }
    }
}
impl Held {
    /// Holds the publications of the batches appended from now on.
    fn hold(&self) {
        self.released.store(false, Ordering::SeqCst);
    }
    /// Publishes the held batches in commit order; later batches publish at
    /// once.
    fn release(&self) {
        let mut held = self.held.lock().unwrap();
        self.released.store(true, Ordering::SeqCst);
        while let Some((publish, result)) = held.pop_front() {
            publish(result);
        }
    }
}
impl TimelineStore for Held {
    fn query<'a>(
        &'a self,
        scope: &'a dim_agent_runtime::TimelineReadScope,
        query: &'a dim_agent_runtime::QuerySpec,
        page: Option<&'a dim_agent_runtime::QueryPage>,
    ) -> dim_agent_runtime::BoxFuture<'a, dim_agent_runtime::Result<dim_agent_runtime::QueryResult>>
    {
        self.memory.query(scope, query, page)
    }

    fn create_session<'a>(
        &'a self,
        id: &'a str,
        config: &'a SessionConfig,
    ) -> BoxFuture<'a, Result<()>> {
        self.memory.create_session(id, config)
    }
    fn rollback_session<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<()>> {
        self.memory.rollback_session(id)
    }
    fn release_session<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<()>> {
        self.memory.release_session(id)
    }
    fn session_config<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<Option<SessionConfig>>> {
        self.memory.session_config(id)
    }
    fn branches<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<Vec<StoredBranch>>> {
        self.memory.branches(id)
    }
    fn head<'a>(&'a self, key: &'a TimelineKey) -> BoxFuture<'a, Result<u64>> {
        self.memory.head(key)
    }
    fn batch_end<'a>(&'a self, key: &'a TimelineKey, sequence: u64) -> BoxFuture<'a, Result<()>> {
        self.memory.batch_end(key, sequence)
    }
    fn append(
        &self,
        key: TimelineKey,
        events: Vec<StoredEvent>,
        publish: PublishCallback,
    ) -> Result<()> {
        if self.released.load(Ordering::SeqCst) {
            return self.memory.append(key, events, publish);
        }
        let released = self.released.clone();
        let held = self.held.clone();
        self.memory.append(
            key,
            events,
            Box::new(move |result| {
                let mut queue = held.lock().unwrap();
                if released.load(Ordering::SeqCst) {
                    drop(queue);
                    publish(result);
                } else {
                    queue.push_back((publish, result));
                }
            }),
        )
    }
    fn read_segments<'a>(
        &'a self,
        id: &'a str,
        segments: Vec<(String, RangeInclusive<u64>)>,
    ) -> BoxFuture<'a, Result<Vec<Vec<StoredEvent>>>> {
        self.memory.read_segments(id, segments)
    }
    fn trace<'a>(
        &'a self,
        key: &'a TimelineKey,
        sequence: u64,
    ) -> BoxFuture<'a, Result<Option<TimelineTrace>>> {
        self.memory.trace(key, sequence)
    }
    fn save_snapshot<'a>(
        &'a self,
        key: &'a TimelineKey,
        snapshot: RecoverySnapshot,
    ) -> BoxFuture<'a, Result<()>> {
        self.memory.save_snapshot(key, snapshot)
    }
    fn latest_snapshot<'a>(
        &'a self,
        key: &'a TimelineKey,
        through: u64,
        interpreter: &'a str,
    ) -> BoxFuture<'a, Result<Option<RecoverySnapshot>>> {
        self.memory.latest_snapshot(key, through, interpreter)
    }
}

/// What the rebuilding part did: the facts it folded in order, and the read
/// that started it.
#[derive(Default)]
struct Rebuild {
    processed: Vec<u64>,
    covered: u64,
    published: u64,
}

struct TailPart {
    timeline: Key<Timeline>,
    dispatcher: Key<Dispatcher>,
    lifecycle: Key<BranchLifecycle>,
    shared: Arc<Mutex<Rebuild>>,
    read_now: Arc<Mutex<Option<UnboundedReceiver<()>>>>,
    read_done: UnboundedSender<()>,
    rebuild_started: UnboundedSender<()>,
}
impl TailPart {
    fn new(
        names: &Names<'_>,
        shared: Arc<Mutex<Rebuild>>,
        read_now: Arc<Mutex<Option<UnboundedReceiver<()>>>>,
        read_done: UnboundedSender<()>,
        rebuild_started: UnboundedSender<()>,
    ) -> rutis::Result<Self> {
        Ok(Self {
            timeline: names.key()?,
            dispatcher: names.key()?,
            lifecycle: names.key()?,
            shared,
            read_now,
            read_done,
            rebuild_started,
        })
    }
}
impl DynPlugin for TailPart {
    fn name(&self) -> &str {
        "test.tail-handover"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![
            self.timeline.erase(),
            self.dispatcher.erase(),
            self.lifecycle.erase(),
        ]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let timeline = ctx.require_at(&self.timeline)?;
            let dispatcher = ctx.require_at(&self.dispatcher)?;
            let lifecycle = ctx.require_at(&self.lifecycle)?;
            let shared = self.shared.clone();
            let read_now = self.read_now.clone();
            let done = self.read_done.clone();
            let started = self.rebuild_started.clone();
            let task_ctx = ctx.clone();
            let task = tokio::spawn(async move {
                let restored = shared.clone();
                dispatcher
                    .consume(
                        &task_ctx,
                        "TailHandover".into(),
                        vec!["*".into()],
                        Arc::new(move || {
                            let timeline = timeline.clone();
                            let lifecycle = lifecycle.clone();
                            let restored = restored.clone();
                            let read_now = read_now.lock().unwrap().take();
                            let started = started.clone();
                            Box::pin(async move {
                                started.send(()).expect("rebuild registration observer");
                                let mut read_now = read_now.expect("one rebuild per registration");
                                assert!(lifecycle.activated().await);
                                read_now.recv().await.expect("rebuild trigger");
                                let published = timeline.sequence();
                                let reply = timeline.query(page(4096, true)).await?;
                                let QueryOutput::Events { events } = reply.result else {
                                    panic!("handover selects events");
                                };
                                let mut rebuild = restored.lock().unwrap();
                                rebuild.covered = reply.through;
                                rebuild.published = published;
                                rebuild
                                    .processed
                                    .extend(events.iter().map(|row| row.event.sequence));
                                Ok(reply.through)
                            })
                        }),
                        Arc::new(move |event| {
                            shared.lock().unwrap().processed.push(event.sequence);
                            Box::pin(async { Ok(()) })
                        }),
                    )
                    .await
                    .expect("consume rebuild and held-event drain");
                let _ = done.send(());
            });
            ctx.effect(move || async move {
                task.abort();
                let _ = task.await;
                Ok(())
            })?;
            Ok(Effect::Done)
        })
    }
}

/// Runs one handover against a live branch: `tail` writes a fact and holds its
/// publication until after the rebuild read; otherwise everything is
/// published before it. Returns the read's cutoff and the published head it
/// saw, after folding the rest of the run's facts live.
async fn handover(tail: bool) -> (u64, u64) {
    let store = Arc::new(Held::default());
    let shared = Arc::new(Mutex::new(Rebuild::default()));
    let (now_tx, now_rx) = unbounded_channel();
    let (done_tx, mut done_rx) = unbounded_channel();
    let (started_tx, mut started_rx) = unbounded_channel();
    let read_now = Arc::new(Mutex::new(Some(now_rx)));
    let mut builder = AgentBuilder::new(AgentConfig::default());
    dim_agent_capabilities::defaults::mock_hosts(&mut builder);
    dim_agent_capabilities::defaults::install(&mut builder);
    let part = shared.clone();
    builder
        .timeline_store(dim_agent_runtime::TimelineStoreBinding::new(store.clone()))
        .require_feature(Feature::new("test.tail-handover").branch(
            "test.tail-handover",
            move |names| {
                Ok(Box::new(TailPart::new(
                    names,
                    part.clone(),
                    read_now.clone(),
                    done_tx.clone(),
                    started_tx.clone(),
                )?))
            },
        ));
    let agent = MinimalHost::start(&builder).await.unwrap();
    let session = agent
        .create_session(SessionConfig::default())
        .await
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(10), started_rx.recv())
        .await
        .expect("consume must register before the test publishes input")
        .expect("rebuild registration signal");
    store.hold();
    let pending = session
        .submit("first turn".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    if !tail {
        store.release();
        session.idle("main").await.unwrap();
    }
    now_tx.send(()).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(10), done_rx.recv())
        .await
        .expect("consume rebuild and held-event drain must finish")
        .expect("consume completion signal");
    let (published, covered) = {
        let rebuild = shared.lock().unwrap();
        (rebuild.published, rebuild.covered)
    };
    if tail {
        assert!(
            published < pending.sequence,
            "the input at {} is written and not published when the read runs; published {published}",
            pending.sequence
        );
        store.release();
    }
    session
        .submit("second turn".into(), InputMode::Queue, "main")
        .await
        .unwrap();
    let head = session.idle("main").await.unwrap();
    let processed = shared.lock().unwrap().processed.clone();
    assert!(
        processed.iter().copied().eq(1..=head),
        "every fact folded once in sequence order, through {head}: {processed:?}"
    );
    agent.close().await.unwrap();
    (published, covered)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_handover_folds_every_fact_once_when_the_tail_is_unpublished() {
    let (published, covered) = handover(true).await;
    assert!(
        published < covered,
        "the read covers the unpublished tail: published {published}, cutoff {covered}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_handover_folds_every_fact_once_when_nothing_is_unpublished() {
    let (published, covered) = handover(false).await;
    assert_eq!(
        published, covered,
        "a published branch leaves the read's cutoff at its published head"
    );
}

fn input(text: &str) -> EventDraft {
    EventKind::InputMessage(InputMessageData::text(text, None)).into()
}

fn draft(kind: EventKind, id: &str) -> EventDraft {
    EventDraft::from(kind).with_id(id)
}

fn page(limit: u64, include_publishing: bool) -> TimelineQuery {
    TimelineQuery {
        scope: TimelineQueryScope::Branch,
        include_publishing,
        from: None,
        through: None,
        query: QuerySpec {
            filter: None,
            select: QuerySelect::Events,
            group_by: vec![],
            pick: None,
            order_by: vec![],
        },
        page: Some(QueryPage {
            limit: std::num::NonZeroU64::new(limit).unwrap(),
            after: None,
        }),
    }
}

fn ids(reply: &TimelineQueryReply) -> Vec<&str> {
    let QueryOutput::Events { events } = &reply.result else {
        panic!("the selection returns events");
    };
    events.iter().map(|row| row.event.id.as_str()).collect()
}

#[tokio::test]
async fn a_tail_cursor_continues_the_published_page_until_exhaustion() {
    let store = Arc::new(Held::default());
    store
        .create_session("tail", &SessionConfig::default())
        .await
        .unwrap();
    let timeline = Timeline::new(
        Ctx::root().unwrap(),
        TimelineKey {
            session_id: "tail".into(),
            branch_id: "main".into(),
        },
        vec![],
        store.clone(),
    );
    for id in ["1", "2", "3"] {
        timeline.append(input(id).with_id(id)).unwrap();
    }
    store.hold();
    for id in ["4", "5"] {
        timeline.append(input(id).with_id(id)).unwrap();
    }
    let mut pages = Vec::new();
    let mut query = page(2, true);
    loop {
        let reply = timeline.query(query.clone()).await.unwrap();
        assert_eq!(reply.through, 5);
        pages.push(
            ids(&reply)
                .into_iter()
                .map(str::to_owned)
                .collect::<Vec<_>>(),
        );
        query.through = Some(reply.through);
        query.page.as_mut().unwrap().after = reply.next;
        if query.page.as_ref().unwrap().after.is_none() {
            break;
        }
    }
    assert_eq!(pages, vec![vec!["1", "2"], vec!["3", "4"], vec!["5"]]);
    let mut query = page(2, true);
    query.query.order_by = vec![QueryOrder {
        key: QueryOrderKey::Position,
        direction: TimelineQueryDirection::Descending,
    }];
    let newest = timeline.query(query.clone()).await.unwrap();
    assert_eq!(ids(&newest), ["5", "4"]);
    query.through = Some(newest.through);
    query.page.as_mut().unwrap().after = newest.next;
    let descending = timeline.query(query).await.unwrap();
    assert_eq!(
        ids(&descending),
        ["3", "2"],
        "a descending cursor moves from the tail into published facts"
    );
    let started = timeline
        .query(TimelineQuery {
            from: Some(TimelinePosition {
                branch_id: "main".into(),
                sequence: 4,
            }),
            ..page(2, true)
        })
        .await
        .unwrap();
    assert_eq!(ids(&started), ["4", "5"]);
    let mut lookup = page(1, true);
    lookup.query.filter = Some(QueryFilter::Compare {
        left: QueryOperand::Field { path: "id".into() },
        comparison: QueryComparison::Eq,
        right: QueryOperand::Value { value: "5".into() },
    });
    let found = timeline.query(lookup).await.unwrap();
    let QueryOutput::Events { events } = found.result else {
        panic!("the identity query selects events");
    };
    assert_eq!(
        events.len(),
        1,
        "the unpublished identity is found in the tail"
    );
    let from_tail_event = timeline
        .query(TimelineQuery {
            from: Some(events[0].position.clone()),
            through: Some(found.through),
            ..page(2, true)
        })
        .await
        .unwrap();
    assert_eq!(ids(&from_tail_event), ["5"]);
    store.release();
}

#[tokio::test]
async fn one_selection_reads_to_either_bound() {
    let store = Arc::new(Held::default());
    store
        .create_session("bounds", &SessionConfig::default())
        .await
        .unwrap();
    let timeline = Timeline::new(
        Ctx::root().unwrap(),
        TimelineKey {
            session_id: "bounds".into(),
            branch_id: "main".into(),
        },
        vec![],
        store.clone(),
    );
    for id in ["1", "2", "3"] {
        timeline.append(input(id).with_id(id)).unwrap();
    }
    store.hold();
    for id in ["4", "5"] {
        timeline.append(input(id).with_id(id)).unwrap();
    }
    let published = timeline.sequence();
    let committed = timeline.committed();
    let selection = page(16, false);
    let at_published = timeline.query(selection.clone()).await.unwrap();
    assert_eq!(ids(&at_published), vec!["1", "2", "3"]);
    assert_eq!(
        at_published.through, published,
        "a published read stops at what is published and reports that cutoff"
    );
    let at_committed = timeline
        .query(TimelineQuery {
            include_publishing: true,
            ..selection
        })
        .await
        .unwrap();
    assert_eq!(
        ids(&at_committed),
        vec!["1", "2", "3", "4", "5"],
        "the committed read joins the unpublished tail to the published page"
    );
    assert_eq!(
        at_committed.through, committed,
        "a committed read reports the write head it started at"
    );
    store.release();
}

#[tokio::test]
async fn property_pages_include_matching_unpublished_facts_at_the_same_cut() {
    let store = Arc::new(Held::default());
    store
        .create_session("fold", &SessionConfig::default())
        .await
        .unwrap();
    let timeline = Timeline::new(
        Ctx::root().unwrap(),
        TimelineKey {
            session_id: "fold".into(),
            branch_id: "main".into(),
        },
        vec![],
        store.clone(),
    );
    let drafts: Vec<EventDraft> = vec![
        input("one").with_id("1"),
        draft(
            EventKind::TurnStart(TurnStartData {
                turn_id: "t1".into(),
            }),
            "2",
        ),
        draft(
            EventKind::InputConsumed(InputConsumedData {
                input_id: "1".into(),
            }),
            "3",
        ),
        draft(
            EventKind::TurnEnd(TurnEndData {
                turn_id: "t1".into(),
                outcome: TurnOutcome::Completed {
                    reason: "end_turn".into(),
                },
            }),
            "4",
        ),
    ];
    for draft in drafts {
        timeline.append(draft).unwrap();
    }
    store.hold();
    let tail: Vec<EventDraft> = vec![
        input("two").with_id("5"),
        draft(
            EventKind::TurnStart(TurnStartData {
                turn_id: "t2".into(),
            }),
            "6",
        ),
        draft(
            EventKind::StopInterrupt(StopInterruptData {
                reason: "stop".into(),
                scope: InterruptScope::Turn,
            }),
            "7",
        ),
    ];
    for draft in tail {
        timeline.append(draft).unwrap();
    }
    let published = timeline.sequence();
    let through = timeline.committed();
    assert!(
        published < through,
        "the tail is written and not published: {published} < {through}"
    );
    let replay = timeline_events(
        &timeline,
        TimelineQueryScope::Branch,
        ..=timeline.committed(),
    )
    .await
    .unwrap();
    assert_eq!(
        replay.len(),
        7,
        "the fixture contains published and unpublished facts"
    );
    for kinds in [
        &["input/message"][..],
        &["turn/end"],
        &["input/message", "turn/start", "stop/interrupt"],
        &["approval/request"],
    ] {
        let expected: Vec<_> = replay
            .iter()
            .filter(|event| kinds.contains(&event.kind.key()))
            .map(|event| event.id.clone())
            .collect();
        let mut query = TimelineQuery {
            scope: TimelineQueryScope::Branch,
            from: None,
            through: Some(through),
            include_publishing: true,
            query: QuerySpec {
                filter: Some(QueryFilter::In {
                    value: QueryOperand::Field {
                        path: "kind".into(),
                    },
                    values: kinds.iter().map(|kind| (*kind).into()).collect(),
                }),
                select: QuerySelect::Fields {
                    fields: vec!["id".into()],
                },
                group_by: vec![],
                pick: None,
                order_by: vec![],
            },
            page: Some(QueryPage {
                limit: std::num::NonZeroU64::new(2).unwrap(),
                after: None,
            }),
        };
        let mut folded = Vec::new();
        loop {
            let reply = timeline.query(query.clone()).await.unwrap();
            assert_eq!(reply.through, through, "{kinds:?}");
            let QueryOutput::Rows { rows } = reply.result else {
                panic!("selected id fields")
            };
            for row in rows {
                let [QueryValue::Value { value }] = row.values.as_slice() else {
                    panic!("an event id")
                };
                folded.push(value.as_str().unwrap().to_owned());
            }
            let Some(next) = reply.next else { break };
            query.page.as_mut().unwrap().after = Some(next);
        }
        assert_eq!(folded, expected, "{kinds:?}");
        for unpublished in ["5", "6", "7"] {
            let kind = replay
                .iter()
                .find(|event| event.id == unpublished)
                .map(|event| event.kind.key().to_owned())
                .expect("the tail event");
            if kinds.contains(&kind.as_str()) {
                assert!(
                    folded.iter().any(|id| id == unpublished),
                    "{kinds:?}: the read adds the unpublished tail"
                );
            }
        }
    }
    store.release();
}

struct ConsumeAccess {
    timeline: Key<Timeline>,
    dispatcher: Key<Dispatcher>,
    send: UnboundedSender<(Ctx, Arc<Timeline>, Arc<Dispatcher>)>,
}
impl DynPlugin for ConsumeAccess {
    fn name(&self) -> &str {
        "test.consume-access"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![self.timeline.erase(), self.dispatcher.erase()]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            self.send
                .send((
                    ctx.clone(),
                    ctx.require_at(&self.timeline)?,
                    ctx.require_at(&self.dispatcher)?,
                ))
                .expect("consume flow receiver");
            Ok(Effect::Done)
        })
    }
}

struct ConsumeOwner(UnboundedSender<Ctx>);
impl rutis::Plugin for ConsumeOwner {
    type Deps = ();
    fn name(&self) -> &str {
        "test.consume-owner"
    }
    async fn apply(&self, ctx: &Ctx, (): ()) -> rutis::Result {
        self.0
            .send(ctx.clone())
            .expect("registration context receiver");
        Ok(())
    }
}

async fn bounded<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(std::time::Duration::from_secs(10), future)
        .await
        .expect("consumer flow must make progress")
}

async fn consume_branch() -> (MinimalHost, Session, Ctx, Arc<Timeline>, Arc<Dispatcher>) {
    let (send, mut receive) = unbounded_channel();
    let mut builder = AgentBuilder::new(AgentConfig::default());
    dim_agent_capabilities::defaults::mock_hosts(&mut builder);
    dim_agent_capabilities::defaults::install(&mut builder);
    builder.require_feature(Feature::new("test.consume-access").branch(
        "test.consume-access",
        move |names| {
            Ok(Box::new(ConsumeAccess {
                timeline: names.key()?,
                dispatcher: names.key()?,
                send: send.clone(),
            }))
        },
    ));
    let agent = MinimalHost::start(&builder).await.unwrap();
    let session = bounded(agent.create_session(SessionConfig::default()))
        .await
        .unwrap();
    bounded(session.idle("main")).await.unwrap();
    let (ctx, timeline, dispatcher) = bounded(receive.recv()).await.unwrap();
    (agent, session, ctx, timeline, dispatcher)
}

async fn consume_owner(root: &Ctx) -> (rutis::host::FiberView, Ctx) {
    let (send, mut receive) = unbounded_channel();
    let owner = root.plugin(ConsumeOwner(send));
    let ctx = bounded(receive.recv()).await.unwrap();
    bounded(wait_active(
        root,
        std::slice::from_ref(&owner),
        std::time::Duration::from_secs(10),
        &Default::default(),
    ))
    .await
    .unwrap();
    (owner, ctx)
}

async fn dispatched(dispatcher: &Dispatcher, through: u64) {
    let mut state = dispatcher.watch();
    bounded(async {
        while state.borrow().sequence < through {
            assert!(
                state.borrow().failure.is_none(),
                "dispatch failed before target"
            );
            state.changed().await.unwrap();
        }
    })
    .await;
}

fn consume_fact(timeline: &Timeline) -> u64 {
    timeline
        .append(
            EventKind::Extension(
                ExtensionEvent::new("consume-flow", "item", serde_json::json!({})).unwrap(),
            )
            .into(),
        )
        .unwrap()
        .sequence
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn consume_drains_new_arrivals_and_reregisters_without_repeating_history() {
    let (agent, _session, root, timeline, dispatcher) = consume_branch().await;
    let (owner, ctx) = consume_owner(&root).await;
    let seen = Arc::new(Mutex::new(Vec::new()));
    let (started, mut starts) = unbounded_channel();
    let rebuild_release = Arc::new(tokio::sync::Notify::new());
    let handler_release = Arc::new(tokio::sync::Notify::new());
    let (entered, mut entries) = unbounded_channel();
    let recovering = timeline.clone();
    let registration = {
        let dispatcher = dispatcher.clone();
        let rebuild_release = rebuild_release.clone();
        let handler_release = handler_release.clone();
        let seen = seen.clone();
        tokio::spawn(async move {
            dispatcher
                .consume(
                    &ctx,
                    "draining".into(),
                    vec!["consume-flow/*".into()],
                    Arc::new(move || {
                        let started = started.clone();
                        let timeline = recovering.clone();
                        let release = rebuild_release.clone();
                        Box::pin(async move {
                            let cut = timeline.query(page(4096, true)).await?.through;
                            started.send(()).unwrap();
                            release.notified().await;
                            Ok(cut)
                        })
                    }),
                    Arc::new(move |event| {
                        let seen = seen.clone();
                        let release = handler_release.clone();
                        entered.send(event.sequence).unwrap();
                        Box::pin(async move {
                            release.notified().await;
                            seen.lock().unwrap().push(event.sequence);
                            Ok(())
                        })
                    }),
                )
                .await
        })
    };
    bounded(starts.recv()).await.unwrap();
    let first = consume_fact(&timeline);
    dispatched(&dispatcher, first).await;
    rebuild_release.notify_one();
    assert_eq!(bounded(entries.recv()).await.unwrap(), first);
    let second = consume_fact(&timeline);
    dispatched(&dispatcher, second).await;
    handler_release.notify_one();
    assert_eq!(bounded(entries.recv()).await.unwrap(), second);
    handler_release.notify_one();
    bounded(registration).await.unwrap().unwrap();
    let third = consume_fact(&timeline);
    assert_eq!(bounded(entries.recv()).await.unwrap(), third);
    handler_release.notify_one();
    dispatched(&dispatcher, third).await;
    bounded(owner.dispose()).await.unwrap();
    let absent = consume_fact(&timeline);
    dispatched(&dispatcher, absent).await;
    assert_eq!(*seen.lock().unwrap(), vec![first, second, third]);

    let (next_owner, next_ctx) = consume_owner(&root).await;
    let next_seen = Arc::new(Mutex::new(Vec::new()));
    let rebuilt = next_seen.clone();
    let restored_timeline = timeline.clone();
    let live = next_seen.clone();
    bounded(dispatcher.consume(
        &next_ctx,
        "replacement".into(),
        vec!["consume-flow/*".into()],
        Arc::new(move || {
            let timeline = restored_timeline.clone();
            let rebuilt = rebuilt.clone();
            Box::pin(async move {
                let mut query = page(4096, true);
                query.query.filter = Some(QueryFilter::Compare {
                    left: QueryOperand::Field {
                        path: "kind".into(),
                    },
                    comparison: QueryComparison::Eq,
                    right: QueryOperand::Value {
                        value: serde_json::json!("consume-flow/item"),
                    },
                });
                let reply = timeline.query(query).await?;
                let QueryOutput::Events { events } = reply.result else {
                    panic!("event query");
                };
                rebuilt
                    .lock()
                    .unwrap()
                    .extend(events.into_iter().map(|row| row.event.sequence));
                Ok(reply.through)
            })
        }),
        Arc::new(move |event| {
            live.lock().unwrap().push(event.sequence);
            Box::pin(async { Ok(()) })
        }),
    ))
    .await
    .unwrap();
    let last = consume_fact(&timeline);
    dispatched(&dispatcher, last).await;
    assert_eq!(
        *next_seen.lock().unwrap(),
        vec![first, second, third, absent, last]
    );
    assert_eq!(*seen.lock().unwrap(), vec![first, second, third]);
    bounded(next_owner.dispose()).await.unwrap();
    bounded(agent.close()).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn consume_rebuild_and_drain_failures_stay_local_until_the_owner_is_removed() {
    for fail_rebuild in [true, false] {
        let (agent, _session, root, timeline, dispatcher) = consume_branch().await;
        let (owner, ctx) = consume_owner(&root).await;
        let (started, mut starts) = unbounded_channel();
        let release = Arc::new(tokio::sync::Notify::new());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let recovering = timeline.clone();
        let registration = {
            let dispatcher = dispatcher.clone();
            let release = release.clone();
            let seen = seen.clone();
            tokio::spawn(async move {
                dispatcher
                    .consume(
                        &ctx,
                        "failing-rebuild".into(),
                        vec!["consume-flow/item".into()],
                        Arc::new(move || {
                            let started = started.clone();
                            let timeline = recovering.clone();
                            let release = release.clone();
                            Box::pin(async move {
                                let cut = timeline.query(page(4096, true)).await?.through;
                                started.send(()).unwrap();
                                release.notified().await;
                                if fail_rebuild {
                                    Err(AgentError::InvalidInput("rebuild failed".into()))
                                } else {
                                    Ok(cut)
                                }
                            })
                        }),
                        Arc::new(move |event| {
                            seen.lock().unwrap().push(event.sequence);
                            Box::pin(async { Err(AgentError::InvalidInput("drain failed".into())) })
                        }),
                    )
                    .await
            })
        };
        bounded(starts.recv()).await.unwrap();
        let first = consume_fact(&timeline);
        let second = consume_fact(&timeline);
        dispatched(&dispatcher, second).await;
        release.notify_one();
        let error = bounded(registration).await.unwrap().unwrap_err();
        assert!(error.to_string().contains(if fail_rebuild {
            "rebuild failed"
        } else {
            "drain failed"
        }));
        let third = consume_fact(&timeline);
        dispatched(&dispatcher, third).await;
        assert!(dispatcher.failure().is_none());
        assert_eq!(
            *seen.lock().unwrap(),
            if fail_rebuild { vec![] } else { vec![first] }
        );
        bounded(owner.dispose()).await.unwrap();
        let fourth = consume_fact(&timeline);
        dispatched(&dispatcher, fourth).await;
        assert_eq!(
            *seen.lock().unwrap(),
            if fail_rebuild { vec![] } else { vec![first] }
        );
        bounded(agent.close()).await.unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn consume_live_error_finishes_the_current_fact_and_stops_its_suffix() {
    let (agent, _session, root, timeline, dispatcher) = consume_branch().await;
    let (owner, ctx) = consume_owner(&root).await;
    let seen = Arc::new(Mutex::new(Vec::new()));
    let handled = seen.clone();
    let recovering = timeline.clone();
    bounded(dispatcher.consume(
        &ctx,
        "live-error".into(),
        vec!["consume-flow/item".into()],
        Arc::new(move || {
            let timeline = recovering.clone();
            Box::pin(async move { Ok(timeline.query(page(4096, true)).await?.through) })
        }),
        Arc::new(move |event| {
            handled.lock().unwrap().push(event.sequence);
            Box::pin(async { Err(AgentError::InvalidInput("live consumer failed".into())) })
        }),
    ))
    .await
    .unwrap();
    let observed = Arc::new(Mutex::new(Vec::new()));
    let observer = observed.clone();
    let recovering = timeline.clone();
    bounded(dispatcher.consume(
        &ctx,
        "healthy-consumer".into(),
        vec!["consume-flow/item".into()],
        Arc::new(move || {
            let timeline = recovering.clone();
            Box::pin(async move { Ok(timeline.query(page(4096, true)).await?.through) })
        }),
        Arc::new(move |event| {
            observer.lock().unwrap().push(event.sequence);
            Box::pin(async { Ok(()) })
        }),
    ))
    .await
    .unwrap();
    let batch = timeline
        .append_batch_registered(
            (0..2)
                .map(|_| {
                    EventKind::Extension(
                        ExtensionEvent::new("consume-flow", "item", serde_json::json!({})).unwrap(),
                    )
                    .into()
                })
                .collect(),
            |_| Ok(()),
        )
        .unwrap();
    let first = batch[0].sequence;
    let second = batch[1].sequence;
    let mut state = dispatcher.watch();
    bounded(async {
        while state.borrow().failure.is_none() || state.borrow().sequence < first {
            state.changed().await.unwrap();
        }
    })
    .await;
    assert!(
        dispatcher
            .failure()
            .unwrap()
            .to_string()
            .contains("live consumer failed")
    );
    assert_eq!(*observed.lock().unwrap(), vec![first]);
    assert_eq!(dispatcher.sequence(), first);
    assert!(second > first);
    assert_eq!(*seen.lock().unwrap(), vec![first]);
    bounded(owner.dispose()).await.unwrap();
    if let Err(error) = bounded(agent.close()).await {
        assert!(error.to_string().contains("live consumer failed"));
    }
    assert_eq!(dispatcher.sequence(), first);
    assert_eq!(*seen.lock().unwrap(), vec![first]);
}

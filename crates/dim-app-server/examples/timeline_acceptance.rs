//! Reproducible acceptance measurements over the public TimelineStore API.
mod allocation {
    include!("../../../xtask/src/allocation.rs");
    pub fn reset_peak() {
        PEAK.store(LIVE.load(Ordering::Relaxed), Ordering::Relaxed);
    }
}
#[path = "timeline_acceptance/durable_vfs.rs"]
mod durable_vfs;
#[global_allocator]
static ALLOCATOR: allocation::MeasuredAllocator = allocation::MeasuredAllocator;

use dim_agent_capabilities::{
    adapters::{DefaultPrepareModelPlugin, EchoModel, MockTools},
    defaults,
    history::{HistoryRenderer, summary_message},
    host::*,
    timeline_tree::TreeCommands,
};
use dim_agent_runtime::{testing::MinimalHost, *};
use dim_agent_types::*;
use dim_app_server::storage::{SessionStore, SqliteStorage, SqliteTimelineStore};
use rusqlite::Connection;
use rutis::host::CtxHost;
use std::{
    io::{BufRead, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use tokio_util::sync::CancellationToken;

type Error = Box<dyn std::error::Error + Send + Sync>;
fn key(session: &str) -> TimelineKey {
    TimelineKey {
        session_id: session.into(),
        branch_id: "main".into(),
    }
}
fn record(sequence: u64, kind: EventKind) -> StoredEvent {
    StoredEvent {
        event: Arc::new(AgentEvent {
            id: uuid::Uuid::new_v4().to_string(),
            sequence,
            create_at: dim_agent_runtime::core::unix_millis(),
            tags: None,
            kind,
        }),
        trace: None,
        batch_end: sequence,
    }
}
async fn committed(
    store: &SqliteTimelineStore,
    session: &str,
    records: Vec<StoredEvent>,
) -> Result<()> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    store.append(
        key(session),
        records,
        Box::new(move |result| {
            let _ = sender.send(result);
        }),
    )?;
    receiver
        .await
        .map_err(|error| AgentError::Storage(error.to_string()))?
}
fn directory(label: &str) -> PathBuf {
    let root = std::env::var_os("TIMELINE_ACCEPTANCE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/mnt/data/timeline-acceptance"));
    let directory = root.join(format!("{label}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&directory).unwrap();
    directory
}
fn quantile(values: &mut [f64], p: f64) -> f64 {
    values.sort_by(f64::total_cmp);
    values[((values.len() as f64 * p).ceil() as usize).saturating_sub(1)]
}

struct CaptureTree {
    captured: Arc<Mutex<Option<Arc<TreeCommands>>>>,

    tree: rutis::host::Key<TreeCommands>,
}
impl CaptureTree {
    fn new(
        names: &dim_agent_runtime::Names<'_>,
        captured: Arc<Mutex<Option<Arc<TreeCommands>>>>,
    ) -> rutis::Result<Self> {
        Ok(Self {
            captured,
            tree: names.key()?,
        })
    }
}
impl rutis::host::DynPlugin for CaptureTree {
    fn name(&self) -> &str {
        "measurement.tree"
    }
    fn injects(&self) -> Vec<rutis::host::TypeKey> {
        vec![self.tree.erase()]
    }
    fn apply<'a>(
        &'a self,
        ctx: &'a rutis::Ctx,
    ) -> rutis::host::BoxFuture<'a, rutis::Result<rutis::host::Effect>> {
        Box::pin(async move {
            let tree = ctx.require_at(&self.tree)?;
            *self.captured.lock().unwrap() = Some(tree);
            Ok(rutis::host::Effect::Done)
        })
    }
}
struct Compactor {
    samples: Arc<Mutex<PreparationSamples>>,
}
struct PreparationSamples {
    count: usize,
    warm: [usize; 100],
    recent: [usize; 100],
}
impl ResolveContextModel for Compactor {
    fn resolve(
        &self,
        _: Arc<ModelRequest>,
        _: CancellationToken,
    ) -> HostFuture<'_, Arc<dyn ContextModel>> {
        let live = allocation::live();
        let mut samples = self.samples.lock().unwrap();
        let index = samples.count;
        if index < 100 {
            samples.warm[index] = live;
        }
        samples.recent[index % 100] = live;
        samples.count += 1;
        Box::pin(async { Ok(Arc::new(BoundedContext) as Arc<dyn ContextModel>) })
    }
}
struct BoundedContext;
impl ContextModel for BoundedContext {
    fn context_window(&self) -> f64 {
        300_000.
    }
    fn output_tokens(&self) -> f64 {
        64.
    }
    fn threshold_tokens(&self) -> f64 {
        200_256.
    }
    fn count_tokens(&self, request: &ModelRequest) -> f64 {
        (serde_json::to_vec(&request.messages).unwrap().len()
            + serde_json::to_vec(&request.system).unwrap().len()) as f64
            / 4.
    }
}
/// Replaces the history before the retained turns with a fixed summary.
struct FixedSummary;
impl CompactionStrategy for FixedSummary {
    fn name(&self) -> &str {
        "fixed"
    }
    fn replacement_size(&self, model: &dyn ContextModel) -> f64 {
        model.count_tokens(&ModelRequest::new(
            "",
            "",
            vec![summary_message("Prior turns completed.")],
        ))
    }
    fn compact(
        &self,
        input: CompactionInput,
        _: CancellationToken,
    ) -> HostFuture<'_, StrategyOutcome> {
        Box::pin(async move {
            let diagnostics = StrategyDiagnostics {
                model_usage: None,
                model_call_count: 0,
                record: CompactionStrategyRecord {
                    kind: "fixed".into(),
                    details: Default::default(),
                },
            };
            Ok(match input.cut() {
                Some(cut) => StrategyOutcome::Completed {
                    retain_from: cut.retain_from.clone(),
                    replacement: vec![summary_message("Prior turns completed.")],
                    diagnostics,
                },
                None => StrategyOutcome::Skipped {
                    reason: "nothing_to_compact".into(),
                    diagnostics,
                },
            })
        })
    }
}
struct CaptureStop {
    observed: Arc<Mutex<Vec<Instant>>>,

    dispatcher: rutis::host::Key<Dispatcher>,
}
impl CaptureStop {
    fn new(
        names: &dim_agent_runtime::Names<'_>,
        observed: Arc<Mutex<Vec<Instant>>>,
    ) -> rutis::Result<Self> {
        Ok(Self {
            observed,
            dispatcher: names.key()?,
        })
    }
}
impl rutis::host::DynPlugin for CaptureStop {
    fn name(&self) -> &str {
        "measurement.stop"
    }
    fn injects(&self) -> Vec<rutis::host::TypeKey> {
        vec![self.dispatcher.erase()]
    }
    fn apply<'a>(
        &'a self,
        ctx: &'a rutis::Ctx,
    ) -> rutis::host::BoxFuture<'a, rutis::Result<rutis::host::Effect>> {
        Box::pin(async move {
            let dispatcher = ctx.require_at(&self.dispatcher)?;
            let observed = self.observed.clone();
            dispatcher.subscribe(ctx, self.name(), &["stop/interrupt"], move |_| {
                observed.lock().unwrap().push(Instant::now());
                Box::pin(async { Ok(()) })
            })?;
            Ok(rutis::host::Effect::Done)
        })
    }
}
async fn memory(steps: usize) -> std::result::Result<(), Error> {
    let directory = directory("memory");
    let db = SqliteStorage::open(directory.join("dimcode.sqlite"))?;
    let store = SqliteTimelineStore::new(db.clone());
    let captured = Arc::new(Mutex::new(None));
    let capture = captured.clone();
    let samples = Arc::new(Mutex::new(PreparationSamples {
        count: 0,
        warm: [0; 100],
        recent: [0; 100],
    }));
    let mut builder = AgentBuilder::new(AgentConfig::default());
    builder
        .timeline_store(store.clone())
        .session_value(Arc::new(HistoryRenderer::default()))
        .process_value(Arc::new(ModelHost(Arc::new(EchoModel))))
        .session_value(Arc::new(ToolHost(Arc::new(MockTools))))
        .session_value(Arc::new(PrepareContextHost(Arc::new(IdentityContext))))
        .session_value(Arc::new(ContextModelHost(Some(Arc::new(Compactor {
            samples: samples.clone(),
        })))))
        .session_value(Arc::new(CompactionStrategyHost(Some(Arc::new(
            FixedSummary,
        )))))
        .require_feature(
            Feature::new("PrepareModelHost").session("PrepareModelHost", |names| {
                Ok(Box::new(DefaultPrepareModelPlugin::new(names)?))
            }),
        )
        .require_feature(
            Feature::new("measurement.tree").session("measurement.tree", move |names| {
                Ok(Box::new(CaptureTree::new(names, capture.clone())?))
            }),
        );
    defaults::install(&mut builder);
    let agent = MinimalHost::start(&builder).await?;
    let session = agent
        .create_session(SessionConfig {
            system_prompt: "s".repeat(800_000),
            tools: Some(vec![]),
            ..SessionConfig::default()
        })
        .await?;
    session.idle("main").await?;
    let tree = captured.lock().unwrap().clone().unwrap();
    let timeline = tree.get("main")?;
    let mut idle = Vec::with_capacity(steps / 100 + 2);
    let mut turn_warm = [0usize; 100];
    let mut turn_recent = [0usize; 100];
    let mut global_peak = allocation::peak();
    let wall = Instant::now();
    for index in 1..=steps {
        allocation::reset_peak();
        let event = session
            .submit(
                format!("measurement turn {index}"),
                InputMode::Queue,
                "main",
            )
            .await?;
        let weak = Arc::downgrade(&event);
        drop(event);
        session.idle("main").await?;
        tokio::task::yield_now().await;
        assert!(
            weak.upgrade().is_none(),
            "dispatched input remains resident at step {index}"
        );
        let peak = allocation::peak();
        global_peak = global_peak.max(peak);
        if index <= 100 {
            turn_warm[index - 1] = peak;
        }
        turn_recent[(index - 1) % 100] = peak;
        if index == 1 || index % 100 == 0 || index == steps {
            let live = allocation::live();
            let sample = serde_json::json!({"step":index,"liveRustBytes":live,"sequence":timeline.sequence()});
            println!("{sample}");
            idle.push((index, live, timeline.sequence()));
        }
    }
    let stop = Instant::now();
    session.stop("main", "measure stop".into()).await?;
    let stop_ms = stop.elapsed().as_secs_f64() * 1000.;
    let connection = Connection::open(directory.join("dimcode.sqlite"))?;
    let checkpoints: i64 = connection.query_row(
        "SELECT count(*) FROM timeline_snapshots WHERE interpreter = ?1",
        [HistoryRenderer::default().checkpoint_interpreter],
        |row| row.get(0),
    )?;
    {
        let samples = samples.lock().unwrap();
        let warm = samples.warm.iter().max();
        let late = samples.recent.iter().max();
        println!(
            "{}",
            serde_json::json!({"scenario":"memory","steps":steps,"systemBytes":800_000,"idle":idle,"resolveWarmLiveMax":warm,"resolveLateLiveMax":late,"preparationSamples":samples.count,"wholeTurnWarmPeak":turn_warm.iter().max(),"wholeTurnLatePeak":turn_recent.iter().max(),"globalPeakRustBytes":global_peak.max(allocation::peak()),"checkpointCount":checkpoints,"stopMs":stop_ms,"wallMs":wall.elapsed().as_secs_f64()*1000.,"debugAssertions":cfg!(debug_assertions),"database":directory})
        );
    }
    agent.close().await?;
    store.close().await?;
    db.close().await?;
    Ok(())
}
/// Appends `kind` at `sequence` to `session`'s `main`; the receiver yields
/// the publish result, the delay in ms, and the WAL syncs so far, which
/// identify the write round that published it.
fn timed(
    store: &SqliteTimelineStore,
    session: &str,
    sequence: u64,
    kind: EventKind,
) -> Result<tokio::sync::oneshot::Receiver<(Result<()>, f64, usize)>> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let start = Instant::now();
    store.append(
        key(session),
        vec![record(sequence, kind)],
        Box::new(move |result| {
            let delay = start.elapsed().as_secs_f64() * 1000.;
            let _ = sender.send((result, delay, durable_vfs::wal_syncs()));
        }),
    )?;
    Ok(receiver)
}
#[derive(Clone, Copy)]
enum ChainKind {
    Turn,
    Consumed,
}
/// `rounds [BRANCHES [PER_SECOND [CHAINS [EVENT]]]]`: for five seconds each of
/// BRANCHES Sessions appends a 128-byte `step/text` delta PER_SECOND times a
/// second, the branches spread over the period, while each of CHAINS more
/// Sessions runs chains of ten EVENT writes, each appended once the previous
/// one is published, 20 to 80 ms apart.
async fn rounds(
    branches: usize,
    per_second: f64,
    chains: usize,
    chain_kind: ChainKind,
) -> std::result::Result<(), Error> {
    durable_vfs::install_tracking();
    let directory = directory("rounds");
    let db = SqliteStorage::open(directory.join("dimcode.sqlite"))?;
    let store = SqliteTimelineStore::new(db.clone());
    for index in 0..branches {
        store
            .create_session(&format!("stream-{index}"), &SessionConfig::default())
            .await?;
    }
    for index in 0..chains {
        store
            .create_session(&format!("chain-{index}"), &SessionConfig::default())
            .await?;
    }
    durable_vfs::reset_measurement();
    let started = Instant::now();
    let end = started + Duration::from_secs(5);
    let period = Duration::from_secs_f64(1. / per_second);
    let streams: Vec<_> = (0..branches)
        .map(|index| {
            let store = store.clone();
            tokio::spawn(async move {
                let first = started + period.mul_f64(index as f64 / branches as f64);
                let mut tick = tokio::time::interval_at(first.into(), period);
                let (mut published, mut enqueue) = (vec![], vec![]);
                let mut sequence = 0;
                while tick.tick().await.into_std() < end {
                    sequence += 1;
                    let start = Instant::now();
                    published.push(timed(
                        &store,
                        &format!("stream-{index}"),
                        sequence,
                        EventKind::Text(DeltaData {
                            turn_id: "turn".into(),
                            request_id: "request".into(),
                            delta: "x".repeat(128),
                        }),
                    )?);
                    enqueue.push(start.elapsed().as_secs_f64() * 1000.);
                }
                let mut delays = vec![];
                for receiver in published {
                    let (result, delay, round) = receiver.await?;
                    result?;
                    delays.push((delay, round));
                }
                Ok::<_, Error>((delays, enqueue))
            })
        })
        .collect();
    let writers: Vec<_> = (0..chains)
        .map(|index| {
            let store = store.clone();
            tokio::spawn(async move {
                let session = format!("chain-{index}");
                let (mut hops, mut chains) = (vec![], vec![]);
                let mut sequence = 0;
                while Instant::now() < end {
                    let pause = 20 + ((chains.len() + index) as u64 * 37) % 61;
                    tokio::time::sleep(Duration::from_millis(pause)).await;
                    let began = Instant::now();
                    for _ in 0..10 {
                        sequence += 1;
                        let kind = match chain_kind {
                            ChainKind::Turn => EventKind::TurnStart(TurnStartData {
                                turn_id: format!("turn-{sequence}"),
                            }),
                            ChainKind::Consumed => EventKind::InputConsumed(InputConsumedData {
                                input_id: format!("input-{sequence}"),
                            }),
                        };
                        let (result, delay, round) =
                            timed(&store, &session, sequence, kind)?.await?;
                        result?;
                        hops.push((delay, round));
                    }
                    chains.push(began.elapsed().as_secs_f64() * 1000.);
                }
                Ok::<_, Error>((hops, chains))
            })
        })
        .collect();
    let (mut delay, mut enqueue) = (vec![], vec![]);
    let mut rounds = std::collections::BTreeSet::new();
    for stream in streams {
        let (delays, enqueued) = stream.await??;
        rounds.extend(delays.iter().map(|(_, round)| *round));
        delay.extend(delays.into_iter().map(|(delay, _)| delay));
        enqueue.extend(enqueued);
    }
    let (mut hops, mut ten_hops) = (vec![], vec![]);
    for writer in writers {
        let (delays, chained) = writer.await??;
        rounds.extend(delays.iter().map(|(_, round)| *round));
        hops.extend(delays.into_iter().map(|(delay, _)| delay));
        ten_hops.extend(chained);
    }
    let elapsed = started.elapsed().as_secs_f64();
    let measured = durable_vfs::measurement();
    let written = measured.wal_bytes;
    let syncs = measured.wal_syncs;
    let connection = Connection::open(directory.join("dimcode.sqlite"))?;
    let output_rows: i64 = connection.query_row(
        "SELECT count(*) FROM timeline_events WHERE kind = 'step/text'",
        [],
        |row| row.get(0),
    )?;
    let wal = std::fs::metadata(directory.join("dimcode.sqlite-wal"))?.len();
    let at = |values: &mut Vec<f64>, p| (!values.is_empty()).then(|| quantile(values, p));
    println!(
        "{}",
        serde_json::json!({"scenario":"rounds","branches":branches,"perSecond":per_second,"chains":chains,"events":delay.len(),"writeRounds":rounds.len(),"writeRoundsPerSecond":rounds.len() as f64/elapsed,"publishP50Ms":at(&mut delay,0.5),"publishP99Ms":at(&mut delay,0.99),"enqueueP99Ms":at(&mut enqueue,0.99),"walFileBytes":wal,"walWrittenBytes":written,"walBytesPerEvent":written as f64/(delay.len()+hops.len()) as f64,"checkpointSyncs":measured.checkpoint_syncs,"writerCpuMs":measured.writer_cpu_ms,"walWriteBytesPerSecond":written as f64/elapsed,"walSyncs":syncs.len(),"walSyncsPerSecond":syncs.len() as f64/elapsed,"physicalOutputRows":output_rows,"chainHops":hops.len(),"chainWritesPerSecond":hops.len() as f64/elapsed,"chainHopP50Ms":at(&mut hops,0.5),"chainHopP99Ms":at(&mut hops,0.99),"chainP50Ms":at(&mut ten_hops,0.5),"debugAssertions":cfg!(debug_assertions),"database":directory})
    );
    store.close().await?;
    db.close().await?;
    Ok(())
}
async fn paged_reads() -> std::result::Result<(), Error> {
    let directory = directory("paged-reads");
    let db = SqliteStorage::open(directory.join("dimcode.sqlite"))?;
    let store = SqliteTimelineStore::new(db.clone());
    for id in ["history", "writer"] {
        store.create_session(id, &SessionConfig::default()).await?;
    }
    let records = (1..=4096)
        .map(|sequence| {
            record(
                sequence,
                EventKind::InputMessage(InputMessageData::text("x".repeat(2048), None)),
            )
        })
        .collect();
    committed(&store, "history", records).await?;
    let reader = store.clone();
    let reading = tokio::spawn(async move {
        let start = Instant::now();
        for _ in 0..20 {
            drop(reader.read(&key("history"), 1..=4096).await?);
            reader.branches("history").await?;
        }
        Ok::<_, AgentError>(start.elapsed().as_secs_f64() * 1000.)
    });
    let mut publish = vec![];
    for sequence in 1..=50 {
        let start = Instant::now();
        committed(
            &store,
            "writer",
            vec![record(
                sequence,
                EventKind::InputMessage(InputMessageData::text("concurrent write", None)),
            )],
        )
        .await?;
        publish.push(start.elapsed().as_secs_f64() * 1000.);
    }
    let reading_ms = reading.await??;
    println!(
        "{}",
        serde_json::json!({"scenario":"paged-reads", "events":4096,"bodyBytes":4096*2048,
        "readAndRecoveryPasses":20,"readAndRecoveryMs":reading_ms,
        "concurrentPublishP50Ms":quantile(&mut publish,0.5),"concurrentPublishP99Ms":quantile(&mut publish,0.99),
        "debugAssertions":cfg!(debug_assertions)})
    );
    store.close().await?;
    db.close().await?;
    Ok(())
}
/// `read-load [STREAMS [PER_SECOND [EVENTS]]]`: STREAMS Sessions append a
/// 128-byte delta PER_SECOND times a second, two readers read the head and a
/// 64-event page of a Session of EVENTS 2 KiB events, and two Sessions each
/// append the next `turn/start` once the previous one is published. It
/// reports read and publication latency over an idle window, during a fork of
/// the large Session and during a delete of the fork.
async fn read_load(
    streams: usize,
    per_second: f64,
    events: usize,
) -> std::result::Result<(), Error> {
    durable_vfs::install_tracking();
    let directory = directory("read-load");
    let db = SqliteStorage::open(directory.join("dimcode.sqlite"))?;
    let store = SqliteTimelineStore::new(db.clone());
    store
        .create_session("source", &SessionConfig::default())
        .await?;
    for index in 0..streams {
        store
            .create_session(&format!("stream-{index}"), &SessionConfig::default())
            .await?;
    }
    for index in 0..2 {
        store
            .create_session(&format!("chain-{index}"), &SessionConfig::default())
            .await?;
    }
    let mut sequence = 0;
    while sequence < events {
        let count = 256.min(events - sequence);
        let batch = (0..count)
            .map(|_| {
                sequence += 1;
                record(
                    sequence as u64,
                    EventKind::InputMessage(InputMessageData::text("x".repeat(2048), None)),
                )
            })
            .collect();
        committed(&store, "source", batch).await?;
    }
    let source = key("source");
    let through = store.head(&source).await?;
    durable_vfs::reset_measurement();
    let stop = Arc::new(AtomicBool::new(false));
    let reads = Arc::new(Mutex::new(Vec::<(Instant, Instant, f64)>::new()));
    let publishes = Arc::new(Mutex::new(Vec::<(Instant, Instant, f64)>::new()));
    let mut tasks = Vec::new();
    for index in 0..streams {
        let (store, stop) = (store.clone(), stop.clone());
        let period = Duration::from_secs_f64(1. / per_second);
        tasks.push(tokio::spawn(async move {
            let mut tick = tokio::time::interval(period);
            let mut sequence = 0;
            while !stop.load(Ordering::Relaxed) {
                tick.tick().await;
                sequence += 1;
                store.append(
                    key(&format!("stream-{index}")),
                    vec![record(
                        sequence,
                        EventKind::Text(DeltaData {
                            turn_id: "turn".into(),
                            request_id: "request".into(),
                            delta: "x".repeat(128),
                        }),
                    )],
                    Box::new(|_| {}),
                )?;
            }
            Ok::<_, Error>(())
        }));
    }
    for _ in 0..2 {
        let (store, reads, stop) = (store.clone(), reads.clone(), stop.clone());
        let source = source.clone();
        tasks.push(tokio::spawn(async move {
            while !stop.load(Ordering::Relaxed) {
                let start = Instant::now();
                let head = store.head(&source).await?;
                let after = head.saturating_sub(63).max(1);
                drop(store.read_page(&source, after, 64, head).await?);
                reads.lock().unwrap().push((
                    start,
                    Instant::now(),
                    start.elapsed().as_secs_f64() * 1000.,
                ));
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
            Ok::<_, Error>(())
        }));
    }
    for index in 0..2 {
        let (store, publishes, stop) = (store.clone(), publishes.clone(), stop.clone());
        tasks.push(tokio::spawn(async move {
            let session = format!("chain-{index}");
            let mut sequence = 0;
            while !stop.load(Ordering::Relaxed) {
                sequence += 1;
                let start = Instant::now();
                let (sender, receiver) = tokio::sync::oneshot::channel();
                store.append(
                    key(&session),
                    vec![record(
                        sequence,
                        EventKind::TurnStart(TurnStartData {
                            turn_id: format!("turn-{sequence}"),
                        }),
                    )],
                    Box::new(move |result| {
                        let _ = sender.send(result);
                    }),
                )?;
                receiver
                    .await
                    .map_err(|error| AgentError::Storage(error.to_string()))??;
                publishes.lock().unwrap().push((
                    start,
                    Instant::now(),
                    start.elapsed().as_secs_f64() * 1000.,
                ));
                tokio::task::yield_now().await;
            }
            Ok::<_, Error>(())
        }));
    }
    let load_start = Instant::now();
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let sessions = SessionStore::new(db.clone(), rutis::Ctx::root()?, 0);
    let fork_start = Instant::now();
    sessions.fork_session("source", "fork", through).await?;
    let fork_end = Instant::now();
    let fork_ms = fork_start.elapsed().as_secs_f64() * 1000.;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let delete_start = Instant::now();
    store.rollback_session("fork").await?;
    let delete_end = Instant::now();
    let delete_ms = delete_start.elapsed().as_secs_f64() * 1000.;
    tokio::time::sleep(Duration::from_millis(500)).await;
    stop.store(true, Ordering::Relaxed);
    for task in tasks {
        task.await??;
    }
    let window = |samples: &[(Instant, Instant, f64)], from: Instant, to: Instant| {
        let mut values: Vec<f64> = samples
            .iter()
            .filter(|(started, ended, _)| *started < to && *ended >= from)
            .map(|(_, _, value)| *value)
            .collect();
        (!values.is_empty()).then(|| {
            let max = values.iter().copied().fold(0.0, f64::max);
            serde_json::json!({"count":values.len(),"p50Ms":quantile(&mut values,0.5),"p99Ms":quantile(&mut values,0.99),"maxMs":max})
        })
    };
    {
        let (reads, publishes) = (reads.lock().unwrap(), publishes.lock().unwrap());
        let measured = durable_vfs::measurement();
        println!(
            "{}",
            serde_json::json!({
                "scenario":"read-load","streams":streams,"perSecond":per_second,"events":events,"eventBytes":2048,
                "readBefore":window(&reads,load_start,fork_start),
                "readFork":window(&reads,fork_start,fork_end),
                "readDelete":window(&reads,delete_start,delete_end),
                "publishBefore":window(&publishes,load_start,fork_start),
                "publishFork":window(&publishes,fork_start,fork_end),
                "publishDelete":window(&publishes,delete_start,delete_end),
                "forkMs":fork_ms,
                "deleteMs":delete_ms,
                "walWrittenBytes":measured.wal_bytes,
                "checkpointSyncs":measured.checkpoint_syncs,
                "debugAssertions":cfg!(debug_assertions),
                "database":directory
            })
        );
    }
    store.close().await?;
    db.close().await?;
    Ok(())
}

async fn reads() -> std::result::Result<(), Error> {
    let directory = directory("reads");
    let db = SqliteStorage::open(directory.join("dimcode.sqlite"))?;
    let store = SqliteTimelineStore::new(db.clone());
    for (name, bytes) in [("p99", 951_000), ("max", 4_900_000)] {
        store
            .create_session(name, &SessionConfig::default())
            .await?;
        committed(
            &store,
            name,
            vec![
                record(
                    1,
                    EventKind::TurnStart(TurnStartData {
                        turn_id: "turn".into(),
                    }),
                ),
                record(
                    2,
                    EventKind::ModelCall(ModelCallData {
                        turn_id: "turn".into(),
                        request_id: "request".into(),
                        request: Arc::new(ModelRequest::new(
                            "mock",
                            "measure",
                            vec![Message::text(Role::User, "x".repeat(bytes))],
                        )),
                        configuration: None,
                        prompt: None,
                    }),
                ),
                record(
                    3,
                    EventKind::ModelOutputEnd(ModelOutputEndData {
                        turn_id: "turn".into(),
                        request_id: "request".into(),
                        outcome: ModelOutputOutcome::Completed {
                            finish_reason: FinishReason::EndTurn,
                        },
                    }),
                ),
                record(
                    4,
                    EventKind::TurnEnd(TurnEndData {
                        turn_id: "turn".into(),
                        outcome: TurnOutcome::Completed {
                            reason: "synthetic history completed".into(),
                        },
                    }),
                ),
            ],
        )
        .await?;
        let timeline = Timeline::restore(
            rutis::Ctx::root().unwrap(),
            key(name),
            vec![],
            store.clone(),
        )
        .await?;
        let mut timing = Vec::new();
        for _ in 0..20 {
            let start = Instant::now();
            drop(timeline.context(4).await?);
            drop(timeline.context(4).await?);
            timing.push(start.elapsed().as_secs_f64() * 1000.);
        }
        let captured = Arc::new(Mutex::new(None));
        let target = captured.clone();
        let observed = Arc::new(Mutex::new(Vec::with_capacity(21)));
        let stop_target = observed.clone();
        let mut builder = AgentBuilder::new(AgentConfig::default());
        defaults::mock_hosts(&mut builder);
        defaults::install(&mut builder);
        builder
            .timeline_store(store.clone())
            .require_feature(
                Feature::new("measurement.tree").session("measurement.tree", move |names| {
                    Ok(Box::new(CaptureTree::new(names, target.clone())?))
                }),
            )
            .require_feature(
                Feature::new("measurement.stop").branch("measurement.stop", move |names| {
                    Ok(Box::new(CaptureStop::new(names, stop_target.clone())?))
                }),
            );
        let agent = MinimalHost::start(&builder).await?;
        let session = agent.restore_session(name.into()).await?;
        session.idle("main").await?;
        let active = captured.lock().unwrap().clone().unwrap().get("main")?;
        let mut dispatch = Vec::new();
        for _ in 0..20 {
            let reader = active.clone();
            let (started, running) = tokio::sync::oneshot::channel();
            let read = tokio::spawn(async move {
                let through = reader.sequence();
                let _ = started.send(());
                drop(reader.context(through).await?);
                drop(reader.context(through).await?);
                Ok::<_, AgentError>(())
            });
            running.await?;
            tokio::task::yield_now().await;
            let start = Instant::now();
            session.stop("main", "measurement stop".into()).await?;
            let event = *observed.lock().unwrap().last().unwrap();
            dispatch.push(event.duration_since(start).as_secs_f64() * 1000.);
            read.await??;
        }
        println!(
            "{}",
            serde_json::json!({"scenario":"reads","scale":name,"contentBytes":bytes,"twoReadsP50Ms":quantile(&mut timing,0.5),"twoReadsP99Ms":quantile(&mut timing,0.99),"stopDispatchP50Ms":quantile(&mut dispatch,0.5),"stopDispatchP99Ms":quantile(&mut dispatch,0.99),"debugAssertions":cfg!(debug_assertions)})
        );
        agent.close().await?;
    }
    store.close().await?;
    db.close().await?;
    Ok(())
}
struct BarrierStore {
    inner: Arc<SqliteTimelineStore>,
    mode: String,
}
impl TimelineStore for BarrierStore {
    fn create_session<'a>(
        &'a self,
        id: &'a str,
        c: &'a SessionConfig,
    ) -> BoxFuture<'a, Result<()>> {
        self.inner.create_session(id, c)
    }
    fn rollback_session<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<()>> {
        self.inner.rollback_session(id)
    }
    fn release_session<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<()>> {
        self.inner.release_session(id)
    }
    fn session_config<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<Option<SessionConfig>>> {
        self.inner.session_config(id)
    }
    fn branches<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<Vec<StoredBranch>>> {
        self.inner.branches(id)
    }
    fn head<'a>(&'a self, k: &'a TimelineKey) -> BoxFuture<'a, Result<u64>> {
        self.inner.head(k)
    }
    fn read_segments<'a>(
        &'a self,
        id: &'a str,
        s: Vec<(String, std::ops::RangeInclusive<u64>)>,
    ) -> BoxFuture<'a, Result<Vec<Vec<StoredEvent>>>> {
        self.inner.read_segments(id, s)
    }
    fn trace<'a>(
        &'a self,
        k: &'a TimelineKey,
        s: u64,
    ) -> BoxFuture<'a, Result<Option<TimelineTrace>>> {
        self.inner.trace(k, s)
    }
    fn save_snapshot<'a>(
        &'a self,
        k: &'a TimelineKey,
        snapshot: RecoverySnapshot,
    ) -> BoxFuture<'a, Result<()>> {
        self.inner.save_snapshot(k, snapshot)
    }
    fn latest_snapshot<'a>(
        &'a self,
        k: &'a TimelineKey,
        s: u64,
        interpreter: &'a str,
    ) -> BoxFuture<'a, Result<Option<RecoverySnapshot>>> {
        self.inner.latest_snapshot(k, s, interpreter)
    }
    fn query<'a>(
        &'a self,
        k: &'a TimelineKey,
        query: &'a dim_agent_runtime::TimelineQuery,
        from: u64,
        through: u64,
    ) -> BoxFuture<'a, Result<dim_agent_runtime::TimelineQueryPage>> {
        self.inner.query(k, query, from, through)
    }
    fn append(&self, k: TimelineKey, e: Vec<StoredEvent>, publish: PublishCallback) -> Result<()> {
        let mode = self.mode.clone();
        self.inner.append(
            k,
            e,
            Box::new(move |result| {
                result.as_ref().unwrap();
                if mode == "committed" {
                    println!("COMMITTED_BEFORE_PUBLICATION");
                    std::io::stdout().flush().unwrap();
                    loop {
                        std::thread::park();
                    }
                }
                publish(result);
                if mode == "published" {
                    println!("PUBLISHED");
                    std::io::stdout().flush().unwrap();
                    loop {
                        std::thread::park();
                    }
                }
            }),
        )
    }
}
async fn interruption_child(
    mode: String,
    path: PathBuf,
    power: bool,
) -> std::result::Result<(), Error> {
    if power {
        durable_vfs::install(path.parent().unwrap().join("synced"));
    }
    let db = SqliteStorage::open(path.clone())?;
    let store = SqliteTimelineStore::new(db);
    store
        .create_session("interrupted", &SessionConfig::default())
        .await?;
    let _blocker = if mode == "before" {
        let blocker = Connection::open(&path)?;
        blocker.execute_batch("BEGIN IMMEDIATE")?;
        Some(blocker)
    } else {
        None
    };
    if mode == "unsynced" {
        durable_vfs::interrupt_next_wal_sync();
    }
    let timeline = Timeline::new(
        rutis::Ctx::root().unwrap(),
        key("interrupted"),
        vec![],
        Arc::new(BarrierStore {
            inner: store,
            mode: mode.clone(),
        }),
    );
    timeline.append_batch_registered(interrupted_batch(), |_| Ok(()))?;
    if mode == "before" {
        assert_eq!(timeline.sequence(), 0);
        println!("QUEUED_BEFORE_COMMIT");
        std::io::stdout().flush()?;
    }
    std::future::pending::<()>().await;
    Ok(())
}
async fn power_parent() -> std::result::Result<(), Error> {
    for mode in ["unsynced", "committed", "published"] {
        let directory = directory(&format!("power-{mode}"));
        let path = directory.join("dimcode.sqlite");
        let mut child = Command::new(std::env::current_exe()?)
            .arg("power-child")
            .arg(mode)
            .arg(&path)
            .stdout(Stdio::piped())
            .spawn()?;
        let mut reader = std::io::BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        reader.read_line(&mut line)?;
        assert!(!line.is_empty(), "child exited before power barrier");
        child.kill()?;
        child.wait()?;
        let recovery = directory.join("durable-recovery");
        std::fs::create_dir_all(&recovery)?;
        for name in ["dimcode.sqlite", "dimcode.sqlite-wal"] {
            let image = directory.join("synced").join(name);
            if image.exists() {
                std::fs::copy(image, recovery.join(name))?;
            }
        }
        let sequence =
            continue_after_recovery(&recovery.join("dimcode.sqlite"), &format!("power-{mode}"))
                .await?;
        assert_eq!(sequence, if mode == "unsynced" { 0 } else { 2 });
        println!(
            "{}",
            serde_json::json!({"scenario":"simulated-power-loss","stage":mode,"barrier":line.trim(),"recoveredSequence":sequence,"nextSequence":sequence+1,"model":"only successful xSync images survive; truthful fsync; discard unsynced writes","database":directory})
        );
    }
    Ok(())
}
async fn crash_parent() -> std::result::Result<(), Error> {
    for mode in ["before", "committed", "published"] {
        let directory = directory(mode);
        let path = directory.join("dimcode.sqlite");
        let mut child = Command::new(std::env::current_exe()?)
            .arg("crash-child")
            .arg(mode)
            .arg(&path)
            .stdout(Stdio::piped())
            .spawn()?;
        let mut reader = std::io::BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        reader.read_line(&mut line)?;
        assert!(!line.is_empty(), "child exited before barrier");
        child.kill()?;
        child.wait()?;
        let sequence = continue_after_recovery(&path, mode).await?;
        assert_eq!(sequence, if mode == "before" { 0 } else { 2 });
        println!(
            "{}",
            serde_json::json!({"scenario":"process-interruption","stage":mode,"barrier":line.trim(),"recoveredSequence":sequence,"nextSequence":sequence+1,"atomicBatchWidth":2,"database":directory})
        );
    }
    Ok(())
}
/// An input and the request prepared from it, one atomic batch.
fn interrupted_batch() -> Vec<EventDraft> {
    vec![
        EventKind::InputMessage(InputMessageData::text("durable input", None)).into(),
        EventKind::ContextPrepare(ContextPrepareData {
            turn_id: "turn".into(),
            request_id: "request".into(),
            through: 1,
            request: interrupted_request(2),
            configuration: None,
            prompt: None,
            recovery: None,
        })
        .into(),
    ]
}
/// The first `messages` messages of the interrupted Session's requests.
fn interrupted_request(messages: usize) -> Arc<ModelRequest> {
    Arc::new(ModelRequest::new(
        "mock",
        "acceptance",
        (0..messages)
            .map(|index| Message::text(Role::User, format!("durable message {index}")))
            .collect(),
    ))
}
/// The rows of the interrupted Session, as stored.
fn interrupted_rows(path: &Path) -> std::result::Result<Vec<String>, Error> {
    let connection = Connection::open(path)?;
    let mut rows = vec![];
    for sql in [
        "SELECT branch_id || '|' || sequence || '|' || last_sequence || '|' || kind || '|' || body || '|' || ifnull(event_id, '') FROM timeline_events WHERE session_id = 'interrupted' ORDER BY branch_id, sequence",
        "SELECT hash || '|' || kind || '|' || body FROM timeline_content WHERE session_id = 'interrupted' ORDER BY hash",
    ] {
        let mut statement = connection.prepare(sql)?;
        let column = statement.query_map([], |row| row.get::<_, String>(0))?;
        rows.extend(column.collect::<rusqlite::Result<Vec<_>>>()?);
    }
    Ok(rows)
}
/// Recovery after an interruption: the Session validates (contiguous
/// sequences, every request list's nodes present) and its events read
/// back. A request written after the restart, with a cold writer memo,
/// leaves the same rows as the same events written without interruption.
/// Returns the recovered head.
async fn continue_after_recovery(path: &Path, label: &str) -> std::result::Result<u64, Error> {
    let db = SqliteStorage::open(path.to_owned())?;
    let store = SqliteTimelineStore::new(db.clone());
    let sequence = store.head(&key("interrupted")).await?;
    store.branches("interrupted").await?;
    let recovered = if sequence > 0 {
        store.read(&key("interrupted"), 1..=sequence).await?
    } else {
        vec![]
    };
    if let Some(prepare) = recovered.last() {
        assert!(matches!(&prepare.event.kind,
            EventKind::ContextPrepare(data) if data.request == interrupted_request(2)));
    } else {
        assert!(
            interrupted_rows(path)?.is_empty(),
            "no row of an uncommitted batch"
        );
    }
    let next = record(
        sequence + 1,
        EventKind::ModelCall(ModelCallData {
            turn_id: "turn".into(),
            request_id: "request".into(),
            request: interrupted_request(3),
            configuration: None,
            prompt: None,
        }),
    );
    committed(&store, "interrupted", vec![next.clone()]).await?;
    assert_eq!(store.head(&key("interrupted")).await?, sequence + 1);
    store.branches("interrupted").await?;
    store.close().await?;
    db.close().await?;
    let reference = directory(&format!("{label}-uninterrupted")).join("dimcode.sqlite");
    let db = SqliteStorage::open(reference.clone())?;
    let store = SqliteTimelineStore::new(db.clone());
    store
        .create_session("interrupted", &SessionConfig::default())
        .await?;
    if !recovered.is_empty() {
        committed(&store, "interrupted", recovered).await?;
    }
    committed(&store, "interrupted", vec![next]).await?;
    store.close().await?;
    db.close().await?;
    assert_eq!(interrupted_rows(path)?, interrupted_rows(&reference)?);
    Ok(sequence)
}
#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> std::result::Result<(), Error> {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("memory") => memory(args.next().map(|n| n.parse()).transpose()?.unwrap_or(2000)).await,
        Some("rounds") => {
            let usage = "usage: rounds [BRANCHES [PER_SECOND [CHAINS [EVENT]]]], BRANCHES and CHAINS counts (default 64 and 0, not both 0), PER_SECOND a positive rate (default 200), EVENT turn or consumed (default turn)";
            let count = |arg: Option<String>, default| {
                arg.map_or(Some(default), |count| count.parse::<usize>().ok())
                    .ok_or(usage)
            };
            let branches = count(args.next(), 64)?;
            let per_second = args
                .next()
                .map_or(Some(200.), |rate| rate.parse::<f64>().ok())
                .filter(|&rate| {
                    Duration::try_from_secs_f64(1. / rate).is_ok_and(|period| !period.is_zero())
                })
                .ok_or(usage)?;
            let chains = count(args.next(), 0)?;
            let chain_kind = match args.next().as_deref() {
                None | Some("turn") => ChainKind::Turn,
                Some("consumed") => ChainKind::Consumed,
                Some(_) => return Err(usage.into()),
            };
            if branches == 0 && chains == 0 || args.next().is_some() {
                return Err(usage.into());
            }
            rounds(branches, per_second, chains, chain_kind).await
        }
        Some("read-load") => {
            let usage = "usage: read-load [STREAMS [PER_SECOND [EVENTS]]], STREAMS a positive count (default 8), PER_SECOND a positive rate (default 100), EVENTS a positive count (default 12000)";
            let count = |arg: Option<String>, default| {
                arg.map_or(Some(default), |value| value.parse::<usize>().ok())
                    .filter(|&value| value > 0)
                    .ok_or(usage)
            };
            let streams = count(args.next(), 8)?;
            let per_second = args
                .next()
                .map_or(Some(100.), |rate| rate.parse::<f64>().ok())
                .filter(|&rate| {
                    Duration::try_from_secs_f64(1. / rate).is_ok_and(|period| !period.is_zero())
                })
                .ok_or(usage)?;
            let events = count(args.next(), 12000)?;
            if args.next().is_some() {
                return Err(usage.into());
            }
            read_load(streams, per_second, events).await
        }
        Some("reads") => reads().await,
        Some("paged-reads") => paged_reads().await,
        Some("crash-child") => {
            interruption_child(
                args.next().unwrap(),
                PathBuf::from(args.next().unwrap()),
                false,
            )
            .await
        }
        Some("crashes") => crash_parent().await,
        Some("power-child") => {
            interruption_child(
                args.next().unwrap(),
                PathBuf::from(args.next().unwrap()),
                true,
            )
            .await
        }
        Some("powerloss") => power_parent().await,
        _ => Err("expected memory [steps], rounds [branches [per-second [chains [event]]]], read-load [streams [per-second [events]]], reads, paged-reads, crashes, or powerloss".into()),
    }
}

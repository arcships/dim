//! Session restore cost by history size, over a disposable SQLite store.
//!
//! ```text
//! cargo run --release --locked -p dim-app-server --example restore_cost -- \
//!     [plain|tools|product] [1001,3001,10001] [7]
//! cargo run --release --locked -p dim-app-server --example restore_cost -- \
//!     [single-append|single-partial] [2000,20000] [7] [chain|last]
//! ```
//!
//! One Session per size is grown on `main` through real mock-model turns:
//! `plain` turns submit text (one model call, 10 events), `tools` turns
//! submit `tool:echo` (a tool call and a second model call, 18 events).
//! `product` assembles the product context window (`ProductContextModel`, a
//! 32,000-token window and 2,000 output tokens, `ProductSummaryStrategyPlugin`)
//! over an echo model whose summarizer answers a short summary; its turns
//! alternate text and `tool:echo`, each carrying 1,000 bytes, so the window
//! compacts about every 20 turns. Each Session is then unloaded with
//! `Session::unload_if_idle` and restored with `Agent::restore_session` plus
//! the first `idle("main")`, once per repetition, with sizes interleaved; a
//! Session create is timed in the same rounds. A forwarding `TimelineStore`
//! times every store call made while a restore runs, splitting it into
//! directory validation (`branches`), event reads, the `branch/resume` append
//! until it is published, and the time no store call was running; reads
//! overlap, so the time per read is the wall time they cover over their
//! count. Each size prints one JSON line of medians and the stored bytes per
//! event (event bodies plus `kind`, `hash` and `body` of content rows); the
//! event count includes the `branch/resume` events earlier restores wrote.
//! A counting allocator reports the live heap over its level before each
//! restore: the peak until the first idle, at the first idle and after the
//! unload, with the branch openings still alive at the first idle.
//!
//! `RESTORE_COST_CHILD` also grows a child cut at `main`'s head with one turn
//! of its own, so a restore opens a branch with ancestry.
//! `RESTORE_COST_SPANS` prints every store call of the first round's
//! restores and each create's time. `RESTORE_COST_CREATE_PAUSE_MS` waits that
//! long between a round's restores and its create.
//!
//! `single-append` and `single-partial` time a cold read of one request with
//! as many messages as each size. The request chain is written through the
//! store: each turn a `context/prepare` adds two 100-byte messages (and in
//! `single-partial` rewrites the last four) and a `model/call` repeats it;
//! `last` writes only the final turn. After the store reopens, the last
//! `model/call` is read alone per repetition, and the first request written
//! after the reopen is timed until it is published.
//!
//! The store is `restore-cost.sqlite` in a fresh directory under
//! `RESTORE_COST_ROOT` (default `target/restore-cost`), removed at the end.
//! Growing 10k events takes minutes, so `RESTORE_COST_DB=DIR` keeps the store
//! in `DIR` instead: the first run grows the Sessions and lists them in
//! `DIR/sessions.txt`, later runs with the same mix and sizes reuse them. A
//! non-empty `DIR` without that list is refused.
mod allocation {
    include!("../../../xtask/src/allocation.rs");
    pub fn reset_peak() {
        PEAK.store(LIVE.load(Ordering::Relaxed), Ordering::Relaxed);
    }
}
#[global_allocator]
static ALLOCATOR: allocation::MeasuredAllocator = allocation::MeasuredAllocator;
use dim_agent_capabilities::{
    adapters::{DefaultPrepareModelPlugin, EchoModel, MockTools},
    context_model::ProductContextModel,
    defaults,
    history::HistoryRenderer,
    host::*,
    summary_strategy::{ProductSummaryStrategyConfig, ProductSummaryStrategyPlugin},
};
use dim_agent_runtime::{testing::MinimalHost, *};
use dim_agent_types::*;
use dim_app_server::storage::{SqliteStorage, SqliteTimelineStore};
use std::{
    collections::BTreeMap,
    future::Future,
    ops::RangeInclusive,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Instant,
};
use tokio_util::sync::CancellationToken;

type Error = Box<dyn std::error::Error + Send + Sync>;

#[derive(Clone, Copy, PartialEq)]
enum Op {
    Branches,
    Read,
    Append,
    Other,
}
struct Span {
    op: Op,
    events: usize,
    start: Instant,
    end: Instant,
    detail: String,
}
/// Forwards to the SQLite store and times the calls made while recording.
struct Probe {
    inner: Arc<SqliteTimelineStore>,
    recording: AtomicBool,
    spans: Arc<Mutex<Vec<Span>>>,
}
impl Probe {
    fn timed<'a, T: 'a>(
        &'a self,
        op: Op,
        future: impl Future<Output = Result<T>> + Send + 'a,
        events: fn(&T) -> usize,
    ) -> BoxFuture<'a, Result<T>> {
        self.timed_detail(op, future, events, String::new())
    }
    fn timed_detail<'a, T: 'a>(
        &'a self,
        op: Op,
        future: impl Future<Output = Result<T>> + Send + 'a,
        events: fn(&T) -> usize,
        detail: String,
    ) -> BoxFuture<'a, Result<T>> {
        let recording = self.recording.load(Ordering::Relaxed);
        Box::pin(async move {
            let start = Instant::now();
            let result = future.await;
            if recording && let Ok(value) = &result {
                self.spans.lock().unwrap().push(Span {
                    op,
                    events: events(value),
                    start,
                    end: Instant::now(),
                    detail,
                });
            }
            result
        })
    }
}
impl TimelineStore for Probe {
    fn create_session<'a>(
        &'a self,
        id: &'a str,
        config: &'a SessionConfig,
    ) -> BoxFuture<'a, Result<()>> {
        self.inner.create_session(id, config)
    }
    fn rollback_session<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<()>> {
        self.inner.rollback_session(id)
    }
    fn release_session<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<()>> {
        self.inner.release_session(id)
    }
    fn session_config<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<Option<SessionConfig>>> {
        self.timed(Op::Other, self.inner.session_config(id), |_| 0)
    }
    fn branches<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<Vec<StoredBranch>>> {
        self.timed(Op::Branches, self.inner.branches(id), |branches| {
            branches.iter().map(|branch| branch.sequence as usize).sum()
        })
    }
    fn head<'a>(&'a self, key: &'a TimelineKey) -> BoxFuture<'a, Result<u64>> {
        self.timed(Op::Other, self.inner.head(key), |_| 0)
    }
    fn append(
        &self,
        key: TimelineKey,
        events: Vec<StoredEvent>,
        publish: PublishCallback,
    ) -> Result<()> {
        if !self.recording.load(Ordering::Relaxed) {
            return self.inner.append(key, events, publish);
        }
        let (start, count, spans) = (Instant::now(), events.len(), self.spans.clone());
        self.inner.append(
            key,
            events,
            Box::new(move |result| {
                spans.lock().unwrap().push(Span {
                    op: Op::Append,
                    events: count,
                    start,
                    end: Instant::now(),
                    detail: String::new(),
                });
                publish(result)
            }),
        )
    }
    fn read_page<'a>(
        &'a self,
        key: &'a TimelineKey,
        after: u64,
        limit: usize,
        through: u64,
    ) -> BoxFuture<'a, Result<Vec<StoredEvent>>> {
        self.timed(
            Op::Read,
            self.inner.read_page(key, after, limit, through),
            Vec::len,
        )
    }
    fn read_segments<'a>(
        &'a self,
        id: &'a str,
        segments: Vec<(String, RangeInclusive<u64>)>,
    ) -> BoxFuture<'a, Result<Vec<Vec<StoredEvent>>>> {
        let detail = segments
            .iter()
            .map(|(branch, range)| format!("{branch}:{}..={}", range.start(), range.end()))
            .collect::<Vec<_>>()
            .join(",");
        self.timed_detail(
            Op::Read,
            self.inner.read_segments(id, segments),
            |read| read.iter().map(Vec::len).sum(),
            detail,
        )
    }
    fn trace<'a>(
        &'a self,
        key: &'a TimelineKey,
        sequence: u64,
    ) -> BoxFuture<'a, Result<Option<TimelineTrace>>> {
        self.inner.trace(key, sequence)
    }
    fn save_snapshot<'a>(
        &'a self,
        key: &'a TimelineKey,
        snapshot: RecoverySnapshot,
    ) -> BoxFuture<'a, Result<()>> {
        self.inner.save_snapshot(key, snapshot)
    }
    fn latest_snapshot<'a>(
        &'a self,
        key: &'a TimelineKey,
        through: u64,
        interpreter: &'a str,
    ) -> BoxFuture<'a, Result<Option<RecoverySnapshot>>> {
        self.timed(
            Op::Other,
            self.inner.latest_snapshot(key, through, interpreter),
            |_| 0,
        )
    }
}
/// Wall time covered by at least one of `spans`, in ms.
fn covered<'a>(spans: impl Iterator<Item = &'a Span>) -> f64 {
    let mut intervals: Vec<_> = spans.map(|span| (span.start, span.end)).collect();
    intervals.sort();
    let mut total = 0.;
    let mut current: Option<(Instant, Instant)> = None;
    for (start, end) in intervals {
        match &mut current {
            Some((_, last)) if start <= *last => *last = (*last).max(end),
            _ => {
                if let Some((from, to)) = current.replace((start, end)) {
                    total += (to - from).as_secs_f64();
                }
            }
        }
    }
    if let Some((from, to)) = current {
        total += (to - from).as_secs_f64();
    }
    total * 1000.
}
fn ms(from: Instant, to: Instant) -> f64 {
    (to - from).as_secs_f64() * 1000.
}
fn median(mut values: Vec<f64>) -> f64 {
    values.sort_by(f64::total_cmp);
    values[values.len() / 2]
}
/// One restore, in ms unless named otherwise.
struct Restore {
    total: f64,
    call: f64,
    unload: f64,
    branches: f64,
    reads: usize,
    read_events: usize,
    read_sum: f64,
    read_covered: f64,
    append: f64,
    outside_store: f64,
    /// Live heap bytes over those before the restore, in MB: the peak until
    /// the first idle, at the first idle, and after the unload.
    peak_live: f64,
    idle_live: f64,
    unloaded_live: f64,
    openings: usize,
}

/// Grows one Session per size on `main` and unloads it.
async fn grow(
    agent: &Agent,
    mix: &str,
    sizes: &[u64],
) -> std::result::Result<Vec<(u64, String)>, Error> {
    let mut sessions = vec![];
    for &target in sizes {
        let session = agent.create_session(SessionConfig::default()).await?;
        session.idle("main").await?;
        let start = Instant::now();
        let mut turns = 0;
        while session.sequence("main")? < target {
            let text = match mix {
                "plain" => format!("plain turn {turns}"),
                "tools" => format!("tool:echo {{\"text\":\"tool turn {turns}\"}}"),
                _ if turns % 2 == 1 => {
                    format!(
                        "tool:echo {{\"text\":\"tool turn {turns} {}\"}}",
                        "x".repeat(1000)
                    )
                }
                _ => format!("plain turn {turns} {}", "x".repeat(1000)),
            };
            session.submit(text, InputMode::Queue, "main").await?;
            session.idle("main").await?;
            turns += 1;
        }
        eprintln!(
            "grew {} events in {turns} turns, {:.0} ms",
            session.sequence("main")?,
            ms(start, Instant::now())
        );
        if std::env::var_os("RESTORE_COST_CHILD").is_some() {
            let at = session.sequence("main")?;
            let child = session
                .branch(
                    "main".into(),
                    at,
                    BranchOptions {
                        prompt: "child turn".into(),
                        source: BranchSource::Host {
                            operation_id: format!("child-{target}"),
                        },
                    },
                )
                .await?;
            session.idle(&child).await?;
            eprintln!(
                "child {child} at main {at}: {} events",
                session.sequence(&child)?
            );
        }
        assert!(session.unload_if_idle().await?);
        sessions.push((target, session.id().to_owned()));
    }
    Ok(sessions)
}

/// The product summarizer's model: echoes turns like `EchoModel` and answers
/// the summarizer with a short summary that differs each time.
struct Summarizing(AtomicUsize);
impl ModelDriver for Summarizing {
    fn invoke(
        &self,
        request: Arc<ModelRequest>,
        connection: ModelConnection,
        cancel: CancellationToken,
    ) -> HostFuture<'_, ModelStream> {
        let summarizer = matches!(request.system.as_deref(),
            Some(SystemPrompt::Text(text)) if text.starts_with("You summarize a coding session"));
        if !summarizer {
            return EchoModel.invoke(request, connection, cancel);
        }
        let number = self.0.fetch_add(1, Ordering::Relaxed);
        Box::pin(async move {
            Ok(Box::pin(futures_util::stream::iter(vec![
                Ok(ModelChunk::MessageStart {
                    message_id: "summary".into(),
                }),
                Ok(ModelChunk::Text(format!(
                    "Summary {number} of the earlier turns."
                ))),
                Ok(ModelChunk::MessageStop {
                    message_id: "summary".into(),
                    reason: ModelStopReason::EndTurn,
                }),
            ])) as ModelStream)
        })
    }
}
struct Local;
impl ResolveModelConnection for Local {
    fn resolve(
        &self,
        _: Arc<ModelRequest>,
        _: CancellationToken,
    ) -> HostFuture<'_, ResolvedModelConnection> {
        Box::pin(async {
            Ok(ResolvedModelConnection {
                connection: ModelConnection::Local,
                failure_policy: None,
                recovery: None,
            })
        })
    }
}
/// The product context window over the echo model, whose hosts
/// [`product_hosts`] provides.
fn product(builder: &mut AgentBuilder) -> std::result::Result<(), Error> {
    builder
        .session_value(Arc::new(HistoryRenderer::default()))
        .session_value(Arc::new(PrepareContextHost(Arc::new(IdentityContext))))
        .session_value(Arc::new(ContextModelHost(Some(Arc::new(
            ProductContextModel,
        )))))
        .require_feature(Feature::new("ProductSummaryStrategy").session(
            "ProductSummaryStrategy",
            |names| {
                Ok(Box::new(ProductSummaryStrategyPlugin::new(
                    names,
                    ProductSummaryStrategyConfig::default(),
                )?))
            },
        ))
        .require_feature(
            Feature::new("PrepareModelHost").session("PrepareModelHost", |names| {
                Ok(Box::new(DefaultPrepareModelPlugin::new(names)?))
            }),
        );
    defaults::tool_host(builder, Arc::new(MockTools));
    let snapshot: ModelCapabilitySnapshot = serde_json::from_value(serde_json::json!({
        "target": {"providerId": "local", "modelId": "small"},
        "capabilities": {"modalities": ["text"], "toolCalling": true, "streaming": true,
            "structuredOutput": false, "vision": false, "audio": false, "reasoning": false,
            "contextWindow": 32_000, "maxOutputTokens": 2000}
    }))?;
    defaults::install_with_prompt(
        builder,
        Arc::new(EmptyPromptHost),
        Some(ConfigureUpdate {
            sources: Vec::new(),
            model: Some(ModelSelection {
                provider_id: "local".into(),
                model_id: "small".into(),
                capability_snapshot: Some(snapshot),
                config: None,
                metadata: None,
            }),
        }),
    );
    Ok(())
}
/// The product mix's model hosts, which the host's root provides.
fn product_hosts(root: &rutis::Ctx) -> dim_agent_runtime::Result<()> {
    let driver: Arc<dyn ModelDriver> = Arc::new(Summarizing(AtomicUsize::new(0)));
    root.provide(ModelHost(driver))?;
    root.provide(ProviderConnectionResolver(Arc::new(Local)))?;
    Ok(())
}

/// Bytes `session` stores: its event bodies plus `kind`, `hash` and `body`
/// of its content rows.
fn stored_bytes(path: &Path, session: &str) -> std::result::Result<i64, Error> {
    let connection = rusqlite::Connection::open(path)?;
    let events: i64 = connection.query_row(
        "SELECT coalesce(sum(length(body)), 0) FROM timeline_events WHERE session_id = ?1",
        [session],
        |row| row.get(0),
    )?;
    let content: i64 = connection.query_row(
        "SELECT coalesce(sum(length(hash) + length(kind) + length(body)), 0) FROM timeline_content WHERE session_id = ?1",
        [session],
        |row| row.get(0),
    )?;
    Ok(events + content)
}

fn request_event(sequence: u64, request: Arc<ModelRequest>, call: bool) -> StoredEvent {
    let kind = if call {
        EventKind::ModelCall(ModelCallData {
            turn_id: format!("turn-{sequence}"),
            request_id: format!("request-{sequence}"),
            request,
            configuration: None,
            prompt: None,
        })
    } else {
        EventKind::ContextPrepare(ContextPrepareData {
            turn_id: format!("turn-{sequence}"),
            request_id: format!("request-{sequence}"),
            through: sequence - 1,
            request,
            configuration: None,
            prompt: None,
            recovery: None,
        })
    };
    StoredEvent {
        event: Arc::new(AgentEvent {
            id: uuid::Uuid::new_v4().to_string(),
            sequence,
            create_at: 1_760_000_000_000 + sequence,
            tags: None,
            kind,
        }),
        trace: None,
        batch_end: sequence,
    }
}
async fn committed(
    store: &SqliteTimelineStore,
    key: &TimelineKey,
    records: Vec<StoredEvent>,
) -> Result<()> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    store.append(
        key.clone(),
        records,
        Box::new(move |result| {
            let _ = sender.send(result);
        }),
    )?;
    receiver
        .await
        .map_err(|error| AgentError::Storage(error.to_string()))?
}
/// The request of turn `turn` of a single-request chain: `2 * (turn + 1)`
/// messages of about 100 bytes; with `partial`, the four before the last two
/// carry revision `turn`.
fn chain_request(turn: usize, partial: bool) -> Arc<ModelRequest> {
    let length = 2 * (turn + 1);
    let messages = (0..length)
        .map(|index| {
            let revision = if partial && index + 6 >= length && index + 2 < length {
                turn
            } else {
                0
            };
            let role = if index % 2 == 0 {
                Role::User
            } else {
                Role::Assistant
            };
            Message::text(
                role,
                format!("message {index} revision {revision} {}", "x".repeat(72)),
            )
        })
        .collect();
    let mut request = ModelRequest::new("mock", "deterministic", messages);
    request.system = Some(Arc::new(SystemPrompt::Text(
        "You are a coding agent. ".repeat(20),
    )));
    Arc::new(request)
}
/// Cold reads of one request with `size` messages, and the first request
/// written after the store reopens.
async fn single(
    directory: &Path,
    partial: bool,
    sizes: &[u64],
    reps: usize,
    last_only: bool,
) -> std::result::Result<(), Error> {
    let path = directory.join("restore-cost.sqlite");
    for &size in sizes {
        let turns = (size as usize).div_ceil(2);
        let key = TimelineKey {
            session_id: format!("single-{size}"),
            branch_id: "main".into(),
        };
        let db = SqliteStorage::open(path.clone())?;
        let store = SqliteTimelineStore::new(db.clone());
        store
            .create_session(&key.session_id, &SessionConfig::default())
            .await?;
        let start = Instant::now();
        let first = if last_only { turns - 1 } else { 0 };
        let mut sequence = 0;
        let mut batch = vec![];
        for turn in first..turns {
            let request = chain_request(turn, partial);
            batch.push(request_event(sequence + 1, request.clone(), false));
            batch.push(request_event(sequence + 2, request, true));
            sequence += 2;
            if batch.len() >= 200 || turn + 1 == turns {
                committed(&store, &key, std::mem::take(&mut batch)).await?;
            }
        }
        let wrote = ms(start, Instant::now());
        store.close().await?;
        db.close().await?;
        drop((store, db));

        let db = SqliteStorage::open(path.clone())?;
        let store = SqliteTimelineStore::new(db.clone());
        let mut reads = vec![];
        let mut messages = 0;
        for _ in 0..reps {
            let start = Instant::now();
            let read = store.read(&key, sequence..=sequence).await?;
            reads.push(ms(start, Instant::now()));
            if let EventKind::ModelCall(data) = &read[0].event.kind {
                messages = data.request.messages.len();
            }
        }
        let start = Instant::now();
        committed(
            &store,
            &key,
            vec![request_event(
                sequence + 1,
                chain_request(turns, partial),
                false,
            )],
        )
        .await?;
        let first_write = ms(start, Instant::now());
        store.close().await?;
        db.close().await?;
        println!(
            "{}",
            serde_json::json!({"scenario":"single","pattern":if partial {"partial"} else {"append"},
            "rows":if last_only {"last"} else {"chain"},"reps":reps,"messages":messages,
            "writeMs":wrote,"coldReadMedianMs":median(reads),"firstWriteAfterReopenMs":first_write,
            "bytes":stored_bytes(&path, &key.session_id)?,"debugAssertions":cfg!(debug_assertions)})
        );
    }
    Ok(())
}

#[tokio::main]
async fn main() -> std::result::Result<(), Error> {
    let mut args = std::env::args().skip(1);
    let mix = args.next().unwrap_or_else(|| "plain".into());
    if ![
        "plain",
        "tools",
        "product",
        "single-append",
        "single-partial",
    ]
    .contains(&mix.as_str())
    {
        return Err(
            "expected [plain|tools|product] [sizes] [reps], or [single-append|single-partial] [sizes] [reps] [chain|last]"
                .into(),
        );
    }
    let sizes: Vec<u64> = args
        .next()
        .unwrap_or_else(|| "1001,3001,10001".into())
        .split(',')
        .map(str::parse)
        .collect::<std::result::Result<_, _>>()?;
    let reps: usize = args.next().map_or(Ok(7), |value| value.parse())?;
    if sizes.is_empty() || reps == 0 {
        return Err("expected at least one size and one repetition".into());
    }
    if let Some(partial) = mix
        .strip_prefix("single-")
        .map(|pattern| pattern == "partial")
    {
        let last_only = args.next().as_deref() == Some("last");
        let directory = std::env::var_os("RESTORE_COST_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/restore-cost")
            })
            .join(uuid::Uuid::new_v4().to_string());
        std::fs::create_dir_all(&directory)?;
        single(&directory, partial, &sizes, reps, last_only).await?;
        std::fs::remove_dir_all(directory)?;
        return Ok(());
    }

    let kept = std::env::var_os("RESTORE_COST_DB").map(PathBuf::from);
    let directory = kept.clone().unwrap_or_else(|| {
        std::env::var_os("RESTORE_COST_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/restore-cost")
            })
            .join(uuid::Uuid::new_v4().to_string())
    });
    std::fs::create_dir_all(&directory)?;
    let listed = directory.join("sessions.txt");
    let header = format!(
        "{mix} {sizes:?}{}",
        if std::env::var_os("RESTORE_COST_CHILD").is_some() {
            " child"
        } else {
            ""
        }
    );
    let reused = match std::fs::read_to_string(&listed) {
        Ok(text) => {
            let mut lines = text.lines();
            if lines.next() != Some(header.as_str()) {
                return Err(format!("{} lists other Sessions", listed.display()).into());
            }
            lines
                .map(|line| {
                    let (target, id) = line.split_once(' ').ok_or("malformed Session list")?;
                    Ok((target.parse()?, id.to_owned()))
                })
                .collect::<std::result::Result<Vec<_>, Error>>()?
        }
        Err(_) if std::fs::read_dir(&directory)?.next().is_some() => {
            return Err(format!("{} is not empty", directory.display()).into());
        }
        Err(_) => vec![],
    };
    let db = SqliteStorage::open(directory.join("restore-cost.sqlite"))?;
    let store = SqliteTimelineStore::new(db.clone());
    let probe = Arc::new(Probe {
        inner: store.clone(),
        recording: AtomicBool::new(false),
        spans: Arc::default(),
    });
    let mut builder = AgentBuilder::new(AgentConfig::default());
    if mix == "product" {
        product(&mut builder)?;
    } else {
        defaults::mock_hosts(&mut builder);
        defaults::install(&mut builder);
    }
    builder.timeline_store(probe.clone());
    let agent = MinimalHost::start_with(&builder, |root, _| match mix == "product" {
        true => product_hosts(root),
        false => Ok(()),
    })
    .await?;

    let sessions = if reused.is_empty() {
        let sessions = grow(&agent, &mix, &sizes).await?;
        if kept.is_some() {
            let mut text = header + "\n";
            for (target, id) in &sessions {
                text += &format!("{target} {id}\n");
            }
            std::fs::write(&listed, text)?;
        }
        sessions
    } else {
        reused
    };

    let mut runs: BTreeMap<u64, Vec<Restore>> = BTreeMap::new();
    let mut events = BTreeMap::new();
    let mut create = vec![];
    for rep in 0..reps {
        let mut order: Vec<_> = sessions.iter().collect();
        order.rotate_left(rep % sessions.len());
        for (target, id) in order {
            probe.spans.lock().unwrap().clear();
            let base = allocation::live();
            allocation::reset_peak();
            probe.recording.store(true, Ordering::Relaxed);
            let start = Instant::now();
            let session = agent.restore_session(id.clone()).await?;
            let called = Instant::now();
            session.idle("main").await?;
            let end = Instant::now();
            let peak_live = allocation::peak() as f64 - base as f64;
            let idle_live = allocation::live() as f64 - base as f64;
            let openings = agent.live_openings();
            probe.recording.store(false, Ordering::Relaxed);
            let spans = std::mem::take(&mut *probe.spans.lock().unwrap());
            if rep == 0 && std::env::var_os("RESTORE_COST_SPANS").is_some() {
                for span in &spans {
                    eprintln!(
                        "span {target} {:?} {:.1}..{:.1} ms events {} {}",
                        match span.op {
                            Op::Branches => "branches",
                            Op::Read => "read",
                            Op::Append => "append",
                            Op::Other => "other",
                        },
                        ms(start, span.start),
                        ms(start, span.end),
                        span.events,
                        span.detail
                    );
                }
                eprintln!(
                    "span {target} published at {:.1} ms, idle at {:.1} ms",
                    ms(start, called),
                    ms(start, end)
                );
            }
            let of = |op| spans.iter().filter(move |span: &&Span| span.op == op);
            events.insert(*target, session.sequence("main")?);
            let unloading = Instant::now();
            assert!(session.unload_if_idle().await?);
            drop(session);
            let unloaded_live = allocation::live() as f64 - base as f64;
            runs.entry(*target).or_default().push(Restore {
                total: ms(start, end),
                call: ms(start, called),
                unload: ms(unloading, Instant::now()),
                branches: covered(of(Op::Branches)),
                reads: of(Op::Read).count(),
                read_events: of(Op::Read).map(|span| span.events).sum(),
                read_sum: of(Op::Read).map(|span| ms(span.start, span.end)).sum(),
                read_covered: covered(of(Op::Read)),
                append: covered(of(Op::Append)),
                outside_store: ms(start, end) - covered(spans.iter()),
                peak_live: peak_live / 1e6,
                idle_live: idle_live / 1e6,
                unloaded_live: unloaded_live / 1e6,
                openings,
            });
        }
        if let Some(pause) = std::env::var("RESTORE_COST_CREATE_PAUSE_MS")
            .ok()
            .and_then(|v| v.parse().ok())
        {
            tokio::time::sleep(std::time::Duration::from_millis(pause)).await;
        }
        let start = Instant::now();
        let session = agent.create_session(SessionConfig::default()).await?;
        session.idle("main").await?;
        create.push(ms(start, Instant::now()));
        if std::env::var_os("RESTORE_COST_SPANS").is_some() {
            eprintln!("create rep {rep}: {:.1} ms", create.last().unwrap());
        }
        assert!(session.unload_if_idle().await?);
    }

    let path = directory.join("restore-cost.sqlite");
    let bytes: BTreeMap<u64, i64> = sessions
        .iter()
        .map(|(target, id)| Ok((*target, stored_bytes(&path, id)?)))
        .collect::<std::result::Result<_, Error>>()?;
    for (target, restores) in &runs {
        let of = |value: fn(&Restore) -> f64| median(restores.iter().map(value).collect());
        let totals: Vec<_> = restores.iter().map(|restore| restore.total).collect();
        println!(
            "{}",
            serde_json::json!({"scenario":"restore","mix":mix,"reps":reps,"events":events[target],
            "restoreMedianMs":of(|r| r.total),
            "restoreMinMs":totals.iter().copied().reduce(f64::min),
            "restoreMaxMs":totals.iter().copied().reduce(f64::max),
            "restoreCallMedianMs":of(|r| r.call),"unloadMedianMs":of(|r| r.unload),
            "branchesMedianMs":of(|r| r.branches),
            "reads":restores[0].reads,"readEvents":restores[0].read_events,
            "readSumMedianMs":of(|r| r.read_sum),"readCoveredMedianMs":of(|r| r.read_covered),
            "perReadMedianMs":of(|r| r.read_covered / r.reads.max(1) as f64),
            "appendMedianMs":of(|r| r.append),"outsideStoreMedianMs":of(|r| r.outside_store),
            "bytesPerEvent":bytes[target] as f64 / events[target] as f64,
            "fullReads":restores[0].read_events as f64 / events[target] as f64,
            "peakLiveMedianMB":of(|r| r.peak_live),"idleLiveMedianMB":of(|r| r.idle_live),
            "unloadedLiveMedianMB":of(|r| r.unloaded_live),"openingsAtIdle":restores.iter().map(|r| r.openings).max(),
            "debugAssertions":cfg!(debug_assertions)})
        );
    }
    println!(
        "{}",
        serde_json::json!({"scenario":"create","mix":mix,"reps":reps,
        "createMedianMs":median(create),"debugAssertions":cfg!(debug_assertions)})
    );

    agent.close().await?;
    store.close().await?;
    db.close().await?;
    if kept.is_none() {
        std::fs::remove_dir_all(directory)?;
    }
    Ok(())
}

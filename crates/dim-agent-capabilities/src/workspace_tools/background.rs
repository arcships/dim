//! BackgroundExec: this branch's detached `exec` commands, see
//! `docs/architecture/rust-agent/plugins/WorkspaceTools.md#backgroundexec` and the detached
//! work contract of `event-driven-flows.md` §3. The capability owns the task
//! table, writes `exec/started` / `exec/ended`, and wakes the branch at the
//! dispatch of every `exec/ended` that is not `cancelled`. Each running task
//! holds its Session (`SessionLifecycle::take_hold`) until its `exec/ended` is
//! appended, so an idle unload never ends it (idle unload §5, R2).
use super::exec::{ExecProcess, OUTPUT_FILE_BYTE_LIMIT, Output, recovered_result, sanitize};
use crate::{
    context_block,
    prompt_guide::{GuideEntries, GuideEntry, GuideScope},
};
use dim_agent_runtime::{
    AgentError, Dispatcher, Names, SessionLifecycle, Timeline, TimelineIdentity, TimelineQuery,
    TimelineQueryDirection, TimelineQueryOrderBy, TimelineQueryScope,
};
use dim_agent_types::*;
use rutis::Ctx;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, Key, TypeKey};
use std::{
    collections::HashMap,
    io::SeekFrom,
    num::NonZeroU64,
    sync::{Arc, Mutex},
};
use tokio::{
    io::{AsyncReadExt, AsyncSeekExt},
    sync::watch,
};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

pub struct StartRequest {
    pub turn_id: String,
    pub request_id: String,
    pub call_id: String,
    /// Proposed id; a `before` hook may rewrite it, the final one is returned.
    pub task_id: String,
    pub description: String,
    pub output_path: String,
}

#[derive(Debug)]
pub enum StartError {
    /// The branch or Session is closing, or this instance is unloading.
    Closing,
    /// `exec/started` was not written.
    Timeline(AgentError),
    /// Rejected handover could not confirm process-tree termination.
    Termination(Box<ToolResult>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskError {
    /// No matching task or live output on this branch.
    Unknown,
    /// This instance was unloaded.
    Closed,
}

/// Why one paged read of a task's registered output file answered nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputPageError {
    /// No matching task on this branch.
    Unknown,
    /// The registered output file is gone or unreadable: it was cleaned, or
    /// the task's Session was deleted.
    Unavailable,
    /// This instance was unloaded.
    Closed,
}

/// One page of a task's registered output file, as
/// `session.exec.output.page` answers it.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputPage {
    /// The page's text, sanitized like the tail preview.
    pub text: String,
    /// The file bytes this page consumed; a client's next cursor is always
    /// `cursor + bytes`. `text` can be shorter because sanitization drops
    /// control characters.
    pub bytes: u64,
    /// Present when the read found at least `limit` bytes, so the client may
    /// continue immediately; always `cursor + bytes`. Absent on a shorter or
    /// empty page, which does not mean the file ended: the client advances by
    /// `bytes` and asks again.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<u64>,
    /// Whether the output file reached `OUTPUT_FILE_BYTE_LIMIT`.
    pub truncated: bool,
}

/// How far a page may exceed its budget to hold a whole character: a UTF-8
/// character is at most four bytes, so at most three lie past the limit.
const PAGE_CHAR_TRAILING_BYTES: usize = 3;

struct Task {
    description: String,
    cancel: CancellationToken,
    output: Option<Arc<Output>>,
    /// The task's registered output file (`exec/started.output_path`): what
    /// the paged read answers from, in every state of the task.
    output_path: String,
    /// The dispatched `exec/ended`, once its wake decision is done.
    ended: watch::Sender<Option<ExecEndedData>>,
}
impl Task {
    fn new(
        description: String,
        cancel: CancellationToken,
        output: Option<Arc<Output>>,
        output_path: String,
    ) -> Self {
        Self {
            description,
            cancel,
            output,
            output_path,
            ended: watch::Sender::new(None),
        }
    }
}

#[derive(Default)]
struct State {
    /// Rejects new starts.
    closing: bool,
    closed: bool,
    tasks: HashMap<String, Task>,
}

pub struct BackgroundExec {
    timeline: Arc<Timeline>,
    /// Gives each task its Session hold.
    session: Arc<SessionLifecycle>,
    /// Parent of every task's cancellation; cancelled on unload and branch close.
    lifecycle: CancellationToken,
    /// Supervising tasks, each holding one process until `exec/ended`.
    tasks: TaskTracker,
    state: Mutex<State>,
}

impl BackgroundExec {
    /// Records the task with `exec/started` and supervises `process`; on
    /// failure the process is terminated and reaped before returning.
    pub async fn start(
        self: &Arc<Self>,
        request: StartRequest,
        process: ExecProcess,
    ) -> Result<String, StartError> {
        #[cfg(feature = "testing")]
        testing::pause(&request.output_path).await;
        let _handover = self.tasks.token();
        let hold = if self.state.lock().unwrap().closing {
            None
        } else {
            self.session.take_hold()
        };
        let Some(hold) = hold else {
            process.abandon().await.map_err(StartError::Termination)?;
            return Err(StartError::Closing);
        };
        let cancel = self.lifecycle.child_token();
        let description = request.description.clone();
        let mut registered = None;
        let committed = self.timeline.append_registered(
            EventKind::ExecStarted(ExecStartedData {
                turn_id: request.turn_id,
                request_id: request.request_id,
                call_id: request.call_id,
                task_id: request.task_id,
                description: request.description,
                output_path: request.output_path,
            })
            .into(),
            |event| {
                let EventKind::ExecStarted(data) = &event.kind else {
                    unreachable!("exec/started kind is preserved")
                };
                let mut state = self.state.lock().unwrap();
                if state.closing {
                    return Err(AgentError::Closed("BackgroundExec".into()));
                }
                state.tasks.insert(
                    data.task_id.clone(),
                    Task::new(
                        description.clone(),
                        cancel.clone(),
                        Some(process.output.clone()),
                        data.output_path.clone(),
                    ),
                );
                registered = Some(data.task_id.clone());
                Ok(())
            },
        );
        let event = match committed {
            Ok(event) => event,
            Err(error) => {
                if let Some(task_id) = registered {
                    self.state.lock().unwrap().tasks.remove(&task_id);
                }
                process.abandon().await.map_err(StartError::Termination)?;
                return Err(if self.state.lock().unwrap().closing {
                    StartError::Closing
                } else {
                    StartError::Timeline(error)
                });
            }
        };
        let EventKind::ExecStarted(data) = &event.kind else {
            unreachable!("exec/started kind is preserved")
        };
        let task_id = data.task_id.clone();
        let owner = self.clone();
        let supervised = task_id.clone();
        self.tasks.spawn(async move {
            let (status, result) = process.finish_background(&supervised, cancel).await;
            let mut ended = owner.timeline.append(
                EventKind::ExecEnded(ExecEndedData {
                    task_id: supervised.clone(),
                    status,
                    result,
                })
                .into(),
            );
            drop(hold);
            if let Ok(event) = &ended {
                let mut publication = owner.timeline.watch();
                let _ = publication
                    .wait_for(|(sequence, closed)| *sequence >= event.sequence || *closed)
                    .await;
                let published = publication.borrow().0;
                if published < event.sequence
                    && let Some(error) = owner.timeline.failure()
                {
                    ended = Err(error);
                }
            }
            let mut state = owner.state.lock().unwrap();
            if let Err(error) = ended {
                tracing::warn!(%error, task_id = supervised, "exec/ended was not written");
                state.tasks.remove(&supervised);
            } else if let Some(task) = state.tasks.get_mut(&supervised) {
                task.output = None;
            }
        });
        Ok(task_id)
    }

    /// Reads the live capture without reconstructing a resource from history.
    pub async fn output(&self, task_id: &str) -> Result<String, TaskError> {
        let output = {
            let state = self.state.lock().unwrap();
            if state.closed {
                return Err(TaskError::Closed);
            }
            state
                .tasks
                .get(task_id)
                .and_then(|task| task.output.clone())
                .ok_or(TaskError::Unknown)?
        };
        Ok(output.preview().await)
    }

    /// Reads one page of the task's registered output file, by byte offset.
    /// The file is the read's only source, so a running task, an ended task
    /// and a task rebuilt after an unload answer alike; the live capture and
    /// the result preview are not consulted. The file is append-only, so a
    /// cursor the method handed out always names a byte that never changes.
    pub async fn output_page(
        &self,
        task_id: &str,
        cursor: u64,
        limit: usize,
    ) -> Result<OutputPage, OutputPageError> {
        let (output_path, ended, live) = {
            let state = self.state.lock().unwrap();
            if state.closed {
                return Err(OutputPageError::Closed);
            }
            let task = state.tasks.get(task_id).ok_or(OutputPageError::Unknown)?;
            (
                task.output_path.clone(),
                task.ended.borrow().clone(),
                task.output.clone(),
            )
        };
        let mut file = tokio::fs::File::open(&output_path)
            .await
            .map_err(|_| OutputPageError::Unavailable)?;
        file.seek(SeekFrom::Start(cursor))
            .await
            .map_err(|_| OutputPageError::Unavailable)?;
        let mut buffer = vec![0u8; limit + PAGE_CHAR_TRAILING_BYTES];
        let mut read = 0;
        while read < buffer.len() {
            match file.read(&mut buffer[read..]).await {
                Ok(0) => break,
                Ok(bytes) => read += bytes,
                Err(_) => return Err(OutputPageError::Unavailable),
            }
        }
        // The file holds the pipe decoder's output, so its bytes are valid
        // UTF-8; every completed write ends on a character boundary, though a
        // reader can catch a write while it is in progress and see the file
        // end inside a character. The page ends at the last character
        // boundary at or before `limit`, so a character the limit splits is
        // served whole: at the start of the next page when the cut falls
        // after other characters, and whole in this page when the budget cuts
        // the very first character. A page therefore consumes at least one
        // character while the file holds one, and can carry up to `limit`
        // plus three bytes. Complete characters before a trailing partial one
        // are served normally; only a cursor opening inside a character, or a
        // file ending inside the first character at the cursor, leaves the
        // page with nothing whole to serve, and it stays empty for that call.
        let mut end = read.min(limit);
        while end > 0 && std::str::from_utf8(&buffer[..end]).is_err() {
            end -= 1;
        }
        if end == 0 && read > 0 {
            let whole = match buffer[0] {
                0xc2..=0xdf => 2,
                0xe0..=0xef => 3,
                0xf0..=0xf4 => 4,
                _ => 0,
            };
            // Only a character the read completed is served whole; one the
            // read cut short stays for the next call, which sees the write
            // finish.
            if whole <= read {
                end = whole;
            }
        }
        // The writer appends only valid UTF-8, and the loop above ends the slice on a boundary.
        let text =
            sanitize(std::str::from_utf8(&buffer[..end]).expect("the page ends on a boundary"));
        // A read that reached the page budget found at least `limit` bytes,
        // so `nextCursor` lets the client continue immediately; a shorter
        // read ended at the file's current length, which a running task may
        // extend later, so the client advances by `bytes` and asks again.
        // The read length decides it: the file's size races with the
        // appender. An empty page (caught up, ended, or a client cursor
        // inside a character) carries no `nextCursor`.
        let next_cursor = (read >= limit && end > 0).then_some(cursor + end as u64);
        let truncated = match ended
            .as_ref()
            .and_then(|data| output_truncated(&data.result))
        {
            Some(truncated) => truncated,
            None => match &live {
                Some(output) => output.truncated().await,
                // Only the recovery terminal (`recovered_result`) carries no
                // flag; its truncation is the file's size alone.
                None => file
                    .metadata()
                    .await
                    .map(|metadata| metadata.len() >= OUTPUT_FILE_BYTE_LIMIT)
                    .unwrap_or(false),
            },
        };
        Ok(OutputPage {
            text,
            bytes: end as u64,
            next_cursor,
            truncated,
        })
    }

    /// Stops a task: cancels it while running, so it ends `cancelled` and
    /// does not wake. Returns once its `exec/ended` is dispatched; a task that
    /// already ended keeps its result and its wake.
    pub async fn stop(&self, task_id: &str) -> Result<ExecEndedData, TaskError> {
        let mut ended = {
            let mut state = self.state.lock().unwrap();
            if state.closed {
                return Err(TaskError::Closed);
            }
            let task = state.tasks.get_mut(task_id).ok_or(TaskError::Unknown)?;
            if let Some(done) = task.ended.borrow().clone() {
                return Ok(done);
            }
            task.cancel.cancel();
            task.ended.subscribe()
        };
        ended
            .wait_for(Option::is_some)
            .await
            .map(|done| done.clone().expect("waited for Some"))
            .map_err(|_| TaskError::Closed)
    }

    fn consume(
        self: &Arc<Self>,
        event: Arc<AgentEvent>,
    ) -> BoxFuture<'static, dim_agent_runtime::Result<()>> {
        match &event.kind {
            EventKind::StopInterrupt(data) if data.scope == InterruptScope::Branch => {
                self.state.lock().unwrap().closing = true;
                self.lifecycle.cancel();
            }
            EventKind::ExecEnded(data) => {
                let this = self.clone();
                let data = data.clone();
                return Box::pin(async move { this.ended(&event.id, data).await });
            }
            _ => {}
        }
        Box::pin(async { Ok(()) })
    }

    /// The wake of one dispatched `exec/ended`, decided by its status and
    /// position: every status but `cancelled` (a stop, branch close or
    /// unload) wakes, including the `failed` terminal a later generation
    /// wrote, unless the end comes after the branch-close interrupt.
    async fn ended(&self, event_id: &str, data: ExecEndedData) -> dim_agent_runtime::Result<()> {
        let (name, done, closing) = {
            let state = self.state.lock().unwrap();
            let task = state.tasks.get(&data.task_id);
            (
                task.map_or_else(|| data.task_id.clone(), |task| task.description.clone()),
                task.map(|task| task.ended.clone()),
                state.closing,
            )
        };
        if wakes(&data)
            && !closing
            && let Err(error) = self.wake(event_id, &data, name).await
        {
            tracing::warn!(%error, task_id = data.task_id, "exec wake was not posted");
        }
        if let Some(done) = done {
            done.send_replace(Some(data));
        }
        Ok(())
    }

    /// One attempt from the dispatch handler; a failure is reposted by the
    /// next generation. A closing branch takes no more input.
    async fn wake(
        &self,
        event_id: &str,
        data: &ExecEndedData,
        name: String,
    ) -> dim_agent_runtime::Result<()> {
        match self.timeline.append(wake_input(event_id, data, name)) {
            Err(AgentError::Closed(_)) => Ok(()),
            result => result.map(|_| ()),
        }
    }

    /// Pairs this branch's `exec/started` with `exec/ended`. An open task did
    /// not survive its owner: its `failed` terminal is written now and wakes
    /// when dispatched. Returns the waking `exec/ended` events that no input
    /// references yet: the previous generation stopped before posting them.
    fn rebuild(&self, events: &[Arc<AgentEvent>]) -> Vec<(String, ExecEndedData, String)> {
        let mut state = self.state.lock().unwrap();
        let mut open = vec![];
        let mut unposted: Vec<(String, ExecEndedData)> = vec![];
        let mut closing = false;
        for event in events {
            match &event.kind {
                EventKind::StopInterrupt(data) if data.scope == InterruptScope::Branch => {
                    closing = true;
                }
                EventKind::ExecStarted(data) => {
                    state.tasks.insert(
                        data.task_id.clone(),
                        Task::new(
                            data.description.clone(),
                            CancellationToken::new(),
                            None,
                            data.output_path.clone(),
                        ),
                    );
                    open.push((data.task_id.clone(), data.output_path.clone()));
                }
                EventKind::ExecEnded(data) => {
                    open.retain(|(task_id, _)| *task_id != data.task_id);
                    if let Some(task) = state.tasks.get_mut(&data.task_id) {
                        task.ended.send_replace(Some(data.clone()));
                    }
                    if wakes(data) && !closing {
                        unposted.push((event.id.clone(), data.clone()));
                    }
                }
                EventKind::InputMessage(_) => {
                    if let Some(result) = tagged(event, WAKE_RESULT_TAG) {
                        unposted.retain(|(id, _)| id != result);
                    }
                }
                _ => {}
            }
        }
        let unposted = unposted
            .into_iter()
            .map(|(id, data)| {
                let name = state
                    .tasks
                    .get(&data.task_id)
                    .map_or_else(|| data.task_id.clone(), |task| task.description.clone());
                (id, data, name)
            })
            .collect();
        for (task_id, output_path) in open {
            let written = self.timeline.append(
                EventKind::ExecEnded(ExecEndedData {
                    result: recovered_result(&task_id, &output_path),
                    task_id: task_id.clone(),
                    status: ExecStatus::Failed,
                })
                .into(),
            );
            if let Err(error) = written {
                tracing::warn!(%error, task_id, "recovery exec/ended was not written");
            }
        }
        unposted
    }

    /// Posts the wakes the previous generation lost, at load: like every
    /// input this capability produces, they are valid by construction.
    fn repost(&self, unposted: Vec<(String, ExecEndedData, String)>) {
        for (event_id, data, name) in unposted {
            if let Err(error) = self.timeline.append(wake_input(&event_id, &data, name)) {
                tracing::warn!(%error, task_id = data.task_id, "lost exec wake was not posted");
            }
        }
    }

    /// Unload (a reload, or the close of its branch or Session; an idle
    /// unload waits for the tasks): running tasks end cancelled after their
    /// trees are released.
    async fn unload(&self) {
        self.state.lock().unwrap().closing = true;
        self.lifecycle.cancel();
        self.tasks.close();
        self.tasks.wait().await;
        let mut state = self.state.lock().unwrap();
        state.closed = true;
        state.tasks.clear();
    }
}

/// Tags of a wake input: the task it reports and the `exec/ended` it carries.
pub const WAKE_TASK_TAG: &str = "exec-wake-task:";
pub const WAKE_RESULT_TAG: &str = "exec-wake-result:";

pub(crate) fn tagged<'a>(event: &'a AgentEvent, prefix: &str) -> Option<&'a str> {
    event
        .tags
        .iter()
        .flatten()
        .find_map(|tag| tag.strip_prefix(prefix))
}

/// A Steer input whose notification carries the result preview.
pub(crate) fn wake_input(event_id: &str, data: &ExecEndedData, name: String) -> EventDraft {
    let preview: Vec<&str> = match &data.result.content {
        ToolContent::Text(text) => vec![text.as_str()],
        ToolContent::Blocks(blocks) => blocks
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text { text } => Some(text.as_str()),
                ContentBlock::Image { .. } => None,
            })
            .collect(),
    };
    let status = match data.status {
        ExecStatus::Completed => "completed",
        ExecStatus::Failed => "failed",
        ExecStatus::Cancelled => "cancelled",
    };
    let text = context_block::element(
        "dim-notification",
        &[("source", "exec"), ("task", &data.task_id)],
        &format!(
            "Background command \"{}\" {status}.\nOutput preview:\n{}\nFull result: read tool, using the output path in the command result.",
            context_block::text(&name),
            context_block::preview(&preview),
        ),
    );
    let mut draft = EventDraft::from(EventKind::InputMessage(InputMessageData {
        blocks: vec![context_block::raw(text, None)],
        mode: Some(InputMode::Steer),
        ..Default::default()
    }));
    draft.tags = Some(vec![
        format!("{WAKE_TASK_TAG}{}", data.task_id),
        format!("{WAKE_RESULT_TAG}{event_id}"),
    ]);
    draft
}

/// A stop, branch close or unload ends a task `cancelled`; every other end
/// is a result the model should see.
pub(crate) fn wakes(data: &ExecEndedData) -> bool {
    data.status != ExecStatus::Cancelled
}

/// The `outputTruncated` flag an `exec/ended` result carries.
fn output_truncated(result: &ToolResult) -> Option<bool> {
    result
        .structured_content
        .as_ref()
        .and_then(|fields| fields.get("outputTruncated"))
        .and_then(serde_json::Value::as_bool)
}

/// `<dim-notification>`: work the model started has finished, failed or
/// been cancelled.
const EXEC_ENTRY: &str = r#"<dim-notification source="exec" task="exec-7f2">: something you started has finished, failed or been cancelled, such as a background command. It is information, not a new request. The output preview belongs to that command. Use the read tool with its output path to read the full result. If the work it belongs to is no longer wanted, note the result and do not restart that work."#;

pub struct BackgroundExecPlugin {
    exec: Key<BackgroundExec>,
    timeline: Key<Timeline>,
    dispatcher: Key<Dispatcher>,
    session: Key<SessionLifecycle>,
    guide: Key<GuideEntries>,
    identity: Key<TimelineIdentity>,
}
impl BackgroundExecPlugin {
    pub fn new(names: &Names<'_>) -> rutis::Result<Self> {
        Ok(Self {
            exec: names.key()?,
            timeline: names.key()?,
            dispatcher: names.key()?,
            session: names.key()?,
            guide: names.key()?,
            identity: names.key()?,
        })
    }
}
impl DynPlugin for BackgroundExecPlugin {
    fn name(&self) -> &str {
        "BackgroundExec"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![
            self.timeline.erase(),
            self.dispatcher.erase(),
            self.session.erase(),
            self.guide.erase(),
            self.identity.erase(),
        ]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let timeline = ctx.require_at(&self.timeline)?;
            let dispatcher = ctx.require_at(&self.dispatcher)?;
            let session = ctx.require_at(&self.session)?;
            let guide = ctx.require_at(&self.guide)?;
            let identity = ctx.require_at(&self.identity)?;
            guide.register(
                ctx,
                GuideScope::Branch(identity.branch_id.clone()),
                GuideEntry {
                    markers: vec!["dim-notification".into()],
                    order: 10,
                    text: EXEC_ENTRY.into(),
                },
            )?;
            let service = Arc::new(BackgroundExec {
                timeline,
                session,
                lifecycle: CancellationToken::new(),
                tasks: TaskTracker::new(),
                state: Mutex::new(State::default()),
            });
            let owner = service.clone();
            ctx.effect(move || async move {
                owner.unload().await;
                Ok(())
            })?;
            let consumer = service.clone();
            let read = service.clone();
            let read_timeline = service.timeline.clone();
            dispatcher
                .consume(
                    ctx,
                    "BackgroundExec".into(),
                    ["exec/ended", "stop/interrupt"].map(String::from).to_vec(),
                    Arc::new(move || {
                        let read = read.clone();
                        let timeline = read_timeline.clone();
                        Box::pin(async move {
                            let reply = timeline
                                .query_all(TimelineQuery {
                                    scope: TimelineQueryScope::Branch,
                                    include_publishing: true,
                                    from_event: None,
                                    from_sequence: None,
                                    order_by: TimelineQueryOrderBy::Sequence,
                                    direction: TimelineQueryDirection::Ascending,
                                    kinds: vec![
                                        "exec/started".into(),
                                        "exec/ended".into(),
                                        "stop/interrupt".into(),
                                        "input/message".into(),
                                    ],
                                    flags: Vec::new(),
                                    event_id: None,
                                    predicate: None,
                                    limit: NonZeroU64::new(1024)
                                        .expect("a page keeps one fact"),
                                    offset: 0,
                                })
                                .await?;
                            let unposted = read.rebuild(&reply.events);
                            read.repost(unposted);
                            Ok(reply.through)
                        })
                    }),
                    Arc::new(move |event| consumer.consume(Arc::new(event))),
                )
                .await?;
            ctx.provide_at(self.exec.clone(), service)?;
            Ok(Effect::Done)
        })
    }
}

/// Holds a task's start open for race flow tests. Feature `testing` only;
/// production builds have no hook.
#[cfg(feature = "testing")]
pub mod testing {
    use std::{
        path::{Path, PathBuf},
        sync::Mutex,
    };
    use tokio::sync::oneshot;

    type Hold = (PathBuf, oneshot::Sender<()>, oneshot::Receiver<()>);
    static HOLDS: Mutex<Vec<Hold>> = Mutex::new(Vec::new());

    /// Holds the next start whose output file is in `dir` until this is
    /// dropped: before the unload counts it, checks the branch or takes its
    /// Session hold.
    pub struct HeldStart {
        arrived: oneshot::Receiver<()>,
        _release: oneshot::Sender<()>,
    }
    impl HeldStart {
        /// Resolves once the held start has arrived.
        pub async fn arrived(&mut self) {
            let _ = (&mut self.arrived).await;
        }
    }

    pub fn hold_start(dir: &Path) -> HeldStart {
        let (arrive, arrived) = oneshot::channel();
        let (release, released) = oneshot::channel();
        HOLDS
            .lock()
            .unwrap()
            .push((dir.to_owned(), arrive, released));
        HeldStart {
            arrived,
            _release: release,
        }
    }

    pub(super) async fn pause(output_path: &str) {
        let hold = {
            let mut holds = HOLDS.lock().unwrap();
            let index = holds
                .iter()
                .position(|(dir, ..)| Path::new(output_path).starts_with(dir));
            index.map(|index| holds.swap_remove(index))
        };
        if let Some((_, arrive, released)) = hold {
            let _ = arrive.send(());
            let _ = released.await;
        }
    }
}

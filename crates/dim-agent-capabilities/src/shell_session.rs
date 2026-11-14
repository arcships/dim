//! Persistent shell execution capability: one long-lived PTY shell per branch.
//!
//! Every command runs in the same interactive shell, wrapped with runtime
//! marker pairs (see docs/design/appserver/persistent-shell.md §4): the
//! command text carries the fragments only, so echoes cannot forge a full
//! marker. A dedicated OS thread owns the blocking PTY reader and forwards
//! chunks to an async consumer through a channel; no lock is held across a
//! blocking read. Commands are serialized through a queue; a foreground
//! timeout converts the call into an occupying task, and explicit
//! background jobs report completion through their own markers, ending in
//! `exec/ended` plus a wake input.

use crate::workspace_tools::background::{wake_input, wakes};
use crate::{
    exec_stop::{ExecStop, TaskError},
    host::{HostError, SessionFilesHost},
    host_platform::{
        HostPlatform, PtyProcessTree, PtySize, ShellEdition, ShellStatus, kill_job, stop_job,
    },
};
use dim_agent_runtime::{Dispatcher, Names, Timeline};
use dim_agent_types::{
    EventKind, ExecEndedData, ExecStartedData, ExecStatus, JsonObject, ToolContent, ToolEffect,
    ToolResult,
};
use rutis::Ctx;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, Key, TypeKey};
use serde_json::json;
use std::{
    collections::{HashMap, VecDeque},
    io::Write as _,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::sync::{Notify, oneshot};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

const RETAINED_BUFFER_LIMIT: usize = 64 * 1024;
/// The size `retained` must pass before it is cut back to
/// `RETAINED_BUFFER_LIMIT`. A burst arrives in thousands of small chunks, so
/// trimming at the limit itself would re-copy the buffer once per chunk; the
/// slack makes the cut amortized while the buffer stays bounded by twice the
/// limit, which is still far below a marker's own line.
const RETAINED_BUFFER_TRIM: usize = 2 * RETAINED_BUFFER_LIMIT;
const OUTPUT_LIMIT: usize = 16_000;
const MAX_TIMEOUT_MS: u64 = 3_600_000;
const DESCRIPTION_CHARS: usize = 200;
const INIT_TIMEOUT: Duration = Duration::from_secs(10);
const STOP_WAIT: Duration = Duration::from_secs(2);
const PID_WAIT: Duration = Duration::from_secs(2);
const PWSH_FILE_POLL: Duration = Duration::from_millis(200);
const SHELL_EXIT_POLL: Duration = Duration::from_millis(100);

/// Hex digits in each half of a marker; `marker_pair` splits a uuid into
/// four of these runs.
const MARKER_HEX: usize = 8;
/// The longest exit token `marker_code` accepts.
const MARKER_TOKEN_LIMIT: usize = 12;
/// The marker a wrapper writes: `__DIM_START_` or `__DIM_END_`, two
/// `MARKER_HEX` runs, a separator and the closing `__`.
const MARKER_LIMIT: usize = "__DIM_START_".len() + 2 * MARKER_HEX + 1 + 2;
/// The longest line a marker can occupy: the marker, the exit token
/// `posix_command`, `posix_background` and `pwsh_command` append to it, and
/// the trailing newline.
const MARKER_LINE_LIMIT: usize = MARKER_LIMIT + MARKER_TOKEN_LIMIT + 1;

/// Configuration for the shell session.
#[derive(Clone)]
pub struct ShellSessionConfig {
    pub cwd: PathBuf,
    /// Collaboration members never run background jobs and interrupt on timeout.
    pub member: bool,
    pub output_dir: PathBuf,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ShellDialect {
    Posix,
    Pwsh,
}

/// Identifies the call an event belongs to.
#[derive(Clone)]
pub struct CallContext {
    pub turn_id: String,
    pub request_id: String,
    pub call_id: String,
}

struct QueuedCommand {
    command: String,
    timeout_ms: u64,
    run_in_background: bool,
    cancel: CancellationToken,
    context: CallContext,
    done: oneshot::Sender<Result<ToolResult, String>>,
}

/// One command's output, owned from the moment its line is written.
///
/// The PTY stream is shared by every command, so a command cannot recover
/// its output from a common buffer: another command's burst can push the
/// start marker out of it before the marker is looked for. Each command
/// holds its own capture instead, fed every chunk while it holds the shell,
/// and finds its own start marker in that stream.
struct Capture {
    start_marker: String,
    end_marker: String,
    /// Bytes seen before the start marker, trimmed to the tail that could
    /// still be a start marker split across chunks. The shell echoes the
    /// wrapper line, and the marker is written in two quoted halves, so no
    /// full marker appears there.
    ahead: String,
    /// Set once the start marker arrived.
    started: bool,
    /// The earliest `OUTPUT_LIMIT` bytes of the output.
    output: String,
    /// Bytes dropped after `output` filled.
    omitted: usize,
    /// Bytes after the start marker not yet settled: the end marker line may
    /// be complete or partial there.
    pending: String,
    /// The end marker's exit token once its line is complete.
    code: Option<String>,
    /// Where settled bytes also go: the task log once the call occupies.
    file: Option<std::fs::File>,
    /// How much of `output` a waiting caller has already been given.
    delivered: usize,
}

impl Capture {
    fn new(start_marker: String, end_marker: String) -> Self {
        Self {
            start_marker,
            end_marker,
            ahead: String::new(),
            started: false,
            output: String::new(),
            omitted: 0,
            pending: String::new(),
            code: None,
            file: None,
            delivered: 0,
        }
    }

    /// Takes one chunk of the stream. Nothing is settled here: a
    /// background job's completion marker has to leave the chunk first.
    fn append(&mut self, text: &str) {
        if self.started {
            self.pending.push_str(text);
            return;
        }
        self.ahead.push_str(text);
        let Some(pos) = self.ahead.find(&self.start_marker) else {
            let keep = self.start_marker.len() - 1;
            self.ahead = tail_bytes(&self.ahead, keep).to_owned();
            return;
        };
        let rest = self.ahead[pos + self.start_marker.len()..].to_owned();
        self.ahead.clear();
        self.started = true;
        self.pending.push_str(&rest);
    }

    /// Removes a completion marker line of another command.
    fn strip(&mut self, marker: &str) {
        strip_marker(&mut self.ahead, marker);
        strip_marker(&mut self.pending, marker);
    }

    /// Settles everything that cannot be part of the end marker. Once the
    /// end marker line is complete, its token is `code` and the line itself
    /// is dropped: it is not output.
    fn drain(&mut self) {
        if !self.started || self.code.is_some() {
            return;
        }
        if let Some(code) = marker_code(&self.pending, &self.end_marker) {
            let end = self.pending.find(&self.end_marker).expect("marker present");
            self.settle(end);
            self.pending.clear();
            self.code = Some(code);
            return;
        }
        self.settle(marker_hold(&self.pending));
    }

    /// Settles `pending[..upto]`: the task log gets every byte, while
    /// `output` keeps the earliest `OUTPUT_LIMIT` bytes for the result and
    /// counts the rest as omitted.
    fn settle(&mut self, upto: usize) {
        let text: String = self.pending.drain(..upto).collect();
        if let Some(file) = self.file.as_mut() {
            let _ = file.write_all(text.as_bytes());
        }
        let room = OUTPUT_LIMIT.saturating_sub(self.output.len());
        let mut end = room.min(text.len());
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        self.output.push_str(&text[..end]);
        self.omitted += text.len() - end;
    }

    /// Makes the task log receive everything settled from now on, starting
    /// with the output already held and the bytes already dropped. The file
    /// stays open for the task's life: the burst arrives in thousands of
    /// chunks, and opening it once per chunk costs more than the read that
    /// produced them.
    fn bind(&mut self, path: &Path) {
        let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        else {
            return;
        };
        let _ = file.write_all(self.output.as_bytes());
        if let Some(omitted) = self.omitted() {
            let mut note = String::new();
            if !self.output.is_empty() && !self.output.ends_with('\n') {
                note.push('\n');
            }
            note.push_str(&format!("[Output truncated: {omitted} bytes omitted]\n"));
            let _ = file.write_all(note.as_bytes());
        }
        self.file = Some(file);
    }

    fn omitted(&self) -> Option<usize> {
        (self.omitted > 0).then_some(self.omitted)
    }
}

/// The foreground call that outlived its timeout; the shell is occupied
/// until its end marker arrives.
struct OccupyingTask {
    task_id: String,
    output_path: PathBuf,
    description: String,
    ended: Arc<Notify>,
}

enum BackgroundKind {
    /// A shell job; completion marker arrives over the PTY stream.
    Posix,
    /// A detached child; completion marker arrives in the result file.
    Pwsh {
        result_path: PathBuf,
        error_path: PathBuf,
    },
}

struct BackgroundTask {
    task_id: String,
    output_path: PathBuf,
    end_marker: String,
    pid: std::io::Result<u32>,
    kind: BackgroundKind,
    description: String,
}

struct Inner {
    pty: Option<PtyProcessTree>,
    /// Counts spawned shells; a consumer loop serves only its own shell.
    generation: u64,
    dialect: ShellDialect,
    shell_exe: Option<PathBuf>,
    queue: VecDeque<QueuedCommand>,
    queue_running: bool,
    /// Raw stream tail, for the markers of commands that own no capture:
    /// initialization, background jobs and their pids.
    retained: String,
    /// The command holding the shell; its capture is fed every chunk.
    capture: Option<Capture>,
    occupying: Option<OccupyingTask>,
    background: HashMap<String, BackgroundTask>,
    /// Note for the call that was running when the shell exited.
    shell_exit: Option<String>,
    /// The next call after a shell death reports the reset.
    had_reset: bool,
    /// Final snapshot of the last occupying task, for occupied-period waits.
    occupy_tail: Option<String>,
    occupy_exit: Option<String>,
}

enum Liveness {
    Alive,
    Dead(Option<portable_pty::ExitStatus>),
    /// A newer shell owns the session state.
    Replaced,
}

/// A settlement produced by the reader: an `exec/ended` plus its wake.
struct Settlement {
    data: ExecEndedData,
    description: String,
    ended: Option<Arc<Notify>>,
}

/// The persistent shell of one branch.
pub struct ShellSession {
    facts: Option<(Ctx, Key<crate::file_access::FileAccessFacts>)>,
    inner: tokio::sync::Mutex<Inner>,
    config: ShellSessionConfig,
    lifecycle: CancellationToken,
    host: Arc<HostPlatform>,
    timeline: Arc<Timeline>,
    #[allow(dead_code)]
    dispatcher: Arc<Dispatcher>,
    /// SessionStore's output ownership (ADR 013); absent in hosts without
    /// native storage, where shell output has no owner.
    owner: Option<Arc<SessionFilesHost>>,
    session_id: String,
    tasks: Arc<tokio::sync::Mutex<TaskTracker>>,
}

impl ShellSession {
    pub fn new(
        config: ShellSessionConfig,
        host: Arc<HostPlatform>,
        timeline: Arc<Timeline>,
        dispatcher: Arc<Dispatcher>,
        owner: Option<Arc<SessionFilesHost>>,
        session_id: String,
    ) -> Self {
        Self {
            facts: None,
            inner: tokio::sync::Mutex::new(Inner {
                pty: None,
                generation: 0,
                dialect: ShellDialect::Posix,
                shell_exe: None,
                queue: VecDeque::new(),
                queue_running: false,
                retained: String::new(),
                capture: None,
                occupying: None,
                background: HashMap::new(),
                shell_exit: None,
                had_reset: false,
                occupy_tail: None,
                occupy_exit: None,
            }),
            config,
            lifecycle: CancellationToken::new(),
            host,
            timeline,
            dispatcher,
            owner,
            session_id,
            tasks: Arc::new(tokio::sync::Mutex::new(TaskTracker::new())),
        }
    }

    /// Runs a command. Serialized through the queue.
    pub async fn run(
        self: &Arc<Self>,
        command: String,
        context: CallContext,
        timeout_ms: u64,
        run_in_background: bool,
        cancel: CancellationToken,
    ) -> Result<ToolResult, String> {
        let timeout_ms = timeout_ms.min(MAX_TIMEOUT_MS);
        let (done_tx, done_rx) = oneshot::channel();
        {
            let mut inner = self.inner.lock().await;
            inner.queue.push_back(QueuedCommand {
                command,
                timeout_ms,
                run_in_background,
                cancel: cancel.clone(),
                context,
                done: done_tx,
            });
        }
        self.ensure_queue_processor().await;
        done_rx
            .await
            .map_err(|_| "shell session closed".to_string())?
    }

    async fn ensure_queue_processor(self: &Arc<Self>) {
        {
            let mut inner = self.inner.lock().await;
            if inner.queue_running {
                return;
            }
            inner.queue_running = true;
        }
        let this = self.clone();
        let tasks = self.tasks.lock().await;
        tasks.spawn(async move {
            this.process_queue().await;
        });
    }

    async fn process_queue(self: &Arc<Self>) {
        loop {
            let queued = {
                let mut inner = self.inner.lock().await;
                inner.queue.pop_front()
            };
            let Some(queued) = queued else {
                let mut inner = self.inner.lock().await;
                inner.queue_running = false;
                break;
            };
            if queued.cancel.is_cancelled() || self.lifecycle.is_cancelled() {
                let _ = queued.done.send(Err("cancelled".into()));
                continue;
            }
            let result = self.execute_queued(&queued).await;
            let _ = queued.done.send(result);
        }
    }

    async fn execute_queued(
        self: &Arc<Self>,
        queued: &QueuedCommand,
    ) -> Result<ToolResult, String> {
        if let Some(result) = self.occupied_wait(queued).await {
            return result;
        }
        let reset_note = self.ensure_shell().await.map_err(|e| e.to_string())?;
        let mut result = if queued.run_in_background && !self.config.member {
            self.run_background(&queued.command, &queued.context)
                .await?
        } else {
            self.run_foreground(
                &queued.command,
                &queued.context,
                queued.timeout_ms,
                &queued.cancel,
            )
            .await?
        };
        if let (Some(note), ToolContent::Text(text)) = (&reset_note, &mut result.content) {
            text.push('\n');
            text.push_str(note);
        }
        Ok(result)
    }

    /// While the shell is occupied: wait for the occupying task, then report
    /// new output; the caller's command was never sent.
    async fn occupied_wait(
        self: &Arc<Self>,
        queued: &QueuedCommand,
    ) -> Option<Result<ToolResult, String>> {
        let notify = {
            let inner = self.inner.lock().await;
            inner.occupying.as_ref().map(|task| task.ended.clone())?
        };
        let deadline = tokio::time::Instant::now() + Duration::from_millis(queued.timeout_ms);
        let ended = tokio::select! {
            _ = notify.notified() => true,
            _ = tokio::time::sleep_until(deadline) => false,
        };
        let mut inner = self.inner.lock().await;
        if ended || inner.occupying.is_none() {
            let tail = inner.occupy_tail.clone().unwrap_or_default();
            let exit = inner.occupy_exit.clone().unwrap_or_default();
            let mut text = String::new();
            if !tail.is_empty() {
                text.push_str(&tail);
                if !tail.ends_with('\n') {
                    text.push('\n');
                }
            }
            if !exit.is_empty() {
                text.push_str(&exit);
                text.push('\n');
            }
            text.push_str("The command was not executed: the shell was occupied. Resend it now.");
            return Some(Ok(ToolResult::text(text)));
        }
        let mut text = String::new();
        if let Some(capture) = inner.capture.as_mut() {
            let delivered = capture.delivered.min(capture.output.len());
            let fresh = capture.output[delivered..].to_owned();
            capture.delivered = capture.output.len();
            if !fresh.is_empty() {
                text.push_str(&fresh);
                if !fresh.ends_with('\n') {
                    text.push('\n');
                }
            }
        }
        let task = inner.occupying.as_mut().expect("checked above");
        text.push_str(&format!(
            "Task {} still occupies the shell. The command was not executed; resend it after the task ends.\nTask ID: {}\nOutput: {}",
            task.task_id,
            task.task_id,
            task.output_path.display()
        ));
        Some(Ok(ToolResult::text(text)))
    }

    /// Spawns the shell on first use or after a death. Returns the reset
    /// note when this spawn replaces a dead shell.
    async fn ensure_shell(self: &Arc<Self>) -> Result<Option<String>, HostError> {
        tokio::fs::create_dir_all(&self.config.output_dir)
            .await
            .map_err(|error| HostError::failed(format!("Shell output directory: {error}")))?;
        let previous = {
            let inner = self.inner.lock().await;
            if self.lifecycle.is_cancelled() {
                return Err(HostError::Cancelled("shell session closed".into()));
            }
            if inner.pty.is_some() {
                return Ok(None);
            }
            inner.generation
        };
        if previous > 0 {
            self.handle_shell_exit(previous, None).await;
        }
        let reset_note = {
            let inner = self.inner.lock().await;
            inner.had_reset.then(|| "[shell was reset]".to_owned())
        };
        let shell = self.host.persistent_shell().await;
        let (exe, dialect) = match shell {
            ShellStatus::Available(runtime) => {
                let dialect = match runtime.edition {
                    ShellEdition::Pwsh | ShellEdition::WindowsPowershell => ShellDialect::Pwsh,
                    _ => ShellDialect::Posix,
                };
                (runtime.executable_path.clone(), dialect)
            }
            ShellStatus::Unavailable { reason, .. } => {
                return Err(HostError::failed(reason));
            }
        };
        let args = shell_args(dialect);
        let env = self
            .host
            .env
            .iter()
            .cloned()
            .chain([
                ("GIT_PAGER".into(), "cat".into()),
                ("NO_COLOR".into(), "1".into()),
                ("TERM".into(), "dumb".into()),
                ("PAGER".into(), "cat".into()),
            ])
            .collect();
        let spec = crate::host_platform::ProcessSpec {
            executable: exe.clone(),
            args,
            cwd: self.config.cwd.clone(),
            env,
        };
        // The persistent shell adds no failure note (host-platform-isolation.md#visibility).
        let roots = match self.facts.as_ref().and_then(|(ctx, key)| ctx.get_at(key)) {
            Some(facts) => {
                facts
                    .0
                    .workspace_roots(&self.config.cwd, self.lifecycle.clone())
                    .await?
            }
            None => vec![self.config.cwd.clone()],
        };
        let (spec, temporary, _) = self
            .host
            .prepare_isolated_process(spec, &roots, Some(&self.config.output_dir))
            .await
            .map_err(|error| HostError::failed(format!("{}: {}", error.code, error.reason)))?;
        let mut pty = self
            .host
            .spawn_pty(
                spec,
                PtySize {
                    cols: 500,
                    rows: 50,
                },
            )
            .map_err(|e| HostError::failed(e.to_string()))?;
        pty.hold(temporary);
        let reader = pty.take_reader();
        let generation = {
            let mut inner = self.inner.lock().await;
            if self.lifecycle.is_cancelled() {
                let _ = pty.terminate().await;
                return Err(HostError::Cancelled("shell session closed".into()));
            }
            inner.generation += 1;
            inner.shell_exe = Some(exe);
            inner.dialect = dialect;
            inner.shell_exit = None;
            inner.had_reset = false;
            inner.occupy_tail = None;
            inner.occupy_exit = None;
            inner.pty = Some(pty);
            inner.generation
        };
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Option<Vec<u8>>>();
        std::thread::spawn(move || {
            let mut reader = reader;
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => {
                        let _ = tx.send(None);
                        break;
                    }
                    Ok(n) => {
                        if tx.send(Some(buf[..n].to_vec())).is_err() {
                            break;
                        }
                    }
                }
            }
        });
        let this = self.clone();
        let lifecycle = self.lifecycle.clone();
        {
            let tasks = self.tasks.lock().await;
            tasks.spawn(async move {
                let mut poll = tokio::time::interval(SHELL_EXIT_POLL);
                let mut reading = true;
                loop {
                    tokio::select! {
                        biased;
                        _ = lifecycle.cancelled() => break,
                        chunk = rx.recv(), if reading => match chunk {
                            Some(Some(bytes)) => {
                                this.handle_output(&regularize_output(&bytes)).await;
                            }
                            _ => {
                                reading = false;
                            }
                        },
                        _ = poll.tick() => match this.liveness(generation).await {
                            Liveness::Alive => {}
                            Liveness::Replaced => break,
                            Liveness::Dead(status) => {
                                while let Ok(Some(bytes)) = rx.try_recv() {
                                    this.handle_output(&regularize_output(&bytes)).await;
                                }
                                this.handle_shell_exit(generation, status).await;
                                break;
                            }
                        },
                    }
                }
            });
        }
        let exe_name = {
            let inner = self.inner.lock().await;
            inner
                .shell_exe
                .as_ref()
                .and_then(|p| p.file_name())
                .and_then(|n| n.to_str())
                .unwrap_or_default()
                .to_owned()
        };
        let init = match dialect {
            ShellDialect::Posix => {
                "unset HISTFILE; export TERM=dumb PAGER=cat GIT_PAGER=cat NO_COLOR=1"
            }
            ShellDialect::Pwsh => {
                "Remove-Module PSReadLine -ErrorAction SilentlyContinue; $env:GIT_PAGER='cat'; $env:NO_COLOR='1'; if ($PSStyle) { $PSStyle.OutputRendering = 'PlainText' }"
            }
        };
        let shell_extra = match (dialect, exe_name.as_str()) {
            (ShellDialect::Posix, "zsh") => "; setopt NO_BANGHIST; unsetopt zle; stty -icanon",
            (ShellDialect::Posix, _) => "; set +H",
            (ShellDialect::Pwsh, _) => "",
        };
        self.send_initial(&format!("{init}{shell_extra}"))
            .await
            .map_err(HostError::failed)?;
        Ok(reset_note)
    }

    /// Sends the init command and waits for its markers; a timeout
    /// terminates the shell and fails the call.
    async fn send_initial(self: &Arc<Self>, command: &str) -> Result<(), String> {
        let (start, end) = marker_pair();
        let dialect = {
            let inner = self.inner.lock().await;
            inner.dialect
        };
        let wrapped = match dialect {
            ShellDialect::Posix => posix_command(command, &start, &end),
            ShellDialect::Pwsh => pwsh_command(command, &start, &end),
        };
        self.write_line(&wrapped).await.map_err(|e| e.to_string())?;
        let end_marker = end.full();
        let deadline = tokio::time::Instant::now() + INIT_TIMEOUT;
        loop {
            {
                let inner = self.inner.lock().await;
                if inner.retained.contains(&end_marker) {
                    return Ok(());
                }
                if inner.pty.is_none() {
                    return Err("shell exited during initialization".into());
                }
            }
            if tokio::time::Instant::now() >= deadline {
                let mut inner = self.inner.lock().await;
                if let Some(mut pty) = inner.pty.take() {
                    let _ = pty.terminate().await;
                }
                let captured = inner.retained.clone();
                inner.had_reset = true;
                return Err(format!(
                    "shell did not initialize; captured output: {captured}"
                ));
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// Starts a command: installs its capture and writes the wrapped line
    /// under the same lock, so no chunk of its own output can arrive before
    /// the capture that owns it exists.
    async fn start_command(
        self: &Arc<Self>,
        capture: Capture,
        wrapped: &str,
    ) -> Result<(), HostError> {
        let mut inner = self.inner.lock().await;
        let line = format!("{wrapped}\n");
        let Some(pty) = inner.pty.as_mut() else {
            return Err(HostError::failed("Shell is not running"));
        };
        pty.write(line.as_bytes())
            .map_err(|e| HostError::failed(format!("Shell write failed: {e}")))?;
        inner.capture = Some(capture);
        Ok(())
    }

    /// Consumes PTY output: folds it into the retained buffer and the
    /// command's capture, settles background tasks whose completion marker
    /// arrived (removing the marker from every buffer), and advances the
    /// occupying task.
    async fn handle_output(self: &Arc<Self>, text: &str) {
        let mut settled: Vec<Settlement> = Vec::new();
        {
            let mut inner = self.inner.lock().await;
            inner.retained.push_str(text);
            if let Some(capture) = inner.capture.as_mut() {
                capture.append(text);
            }
            let matched: Vec<(String, String)> = inner
                .background
                .values()
                .filter(|task| matches!(task.kind, BackgroundKind::Posix))
                .filter_map(|task| {
                    marker_code(&inner.retained, &task.end_marker)
                        .map(|code| (task.task_id.clone(), code))
                })
                .collect();
            for (task_id, code) in matched {
                let Some(task) = inner.background.remove(&task_id) else {
                    continue;
                };
                strip_marker(&mut inner.retained, &task.end_marker);
                if let Some(capture) = inner.capture.as_mut() {
                    capture.strip(&task.end_marker);
                }
                let preview = file_tail(&task.output_path, OUTPUT_LIMIT);
                let data = ExecEndedData {
                    task_id: task.task_id.clone(),
                    status: exit_status(&code),
                    result: ended_result(&task.task_id, &task.output_path, &preview, &code),
                };
                settled.push(Settlement {
                    data,
                    description: task.description,
                    ended: None,
                });
            }
            if inner.retained.len() > RETAINED_BUFFER_TRIM {
                inner.retained = tail_bytes(&inner.retained, RETAINED_BUFFER_LIMIT).to_owned();
            }
            if let Some(capture) = inner.capture.as_mut() {
                capture.drain();
            }
            settled.extend(advance_occupying(&mut inner));
        }
        for settlement in settled {
            self.settle(settlement.data, &settlement.description);
            if let Some(notify) = settlement.ended {
                notify.notify_waiters();
            }
        }
    }

    /// Whether the shell of `generation` still runs. A shell the session
    /// terminated itself (its PTY taken) counts as dead.
    async fn liveness(&self, generation: u64) -> Liveness {
        let mut inner = self.inner.lock().await;
        if inner.generation != generation {
            return Liveness::Replaced;
        }
        match inner.pty.as_mut() {
            None => Liveness::Dead(None),
            Some(pty) => match pty.try_wait() {
                Ok(None) => Liveness::Alive,
                Ok(status) => Liveness::Dead(status),
                Err(_) => Liveness::Dead(None),
            },
        }
    }

    /// The shell of `generation` died: note the exit, settle the occupying
    /// task and the POSIX background jobs as failed, and mark the reset for
    /// the next call.
    async fn handle_shell_exit(
        self: &Arc<Self>,
        generation: u64,
        status: Option<portable_pty::ExitStatus>,
    ) {
        let mut settled: Vec<Settlement> = Vec::new();
        let mut orphans: Vec<BackgroundTask> = Vec::new();
        {
            let mut inner = self.inner.lock().await;
            if inner.generation != generation {
                return;
            }
            if inner.pty.take().is_some() {
                let note = match status {
                    Some(status) => match status.signal() {
                        Some(signal) => format!("[shell killed by signal: {signal}]"),
                        None => format!("[shell exited: code {}]", status.exit_code()),
                    },
                    None => "[shell exited]".to_owned(),
                };
                inner.shell_exit = Some(note);
            }
            inner.had_reset = true;
            if let Some(occ) = inner.occupying.take() {
                if let Some(mut capture) = inner.capture.take() {
                    capture.settle(capture.pending.len());
                    inner.occupy_tail = Some(capture.output);
                }
                let preview = file_tail(&occ.output_path, OUTPUT_LIMIT);
                let data = ExecEndedData {
                    task_id: occ.task_id.clone(),
                    status: ExecStatus::Failed,
                    result: reset_result(&occ.task_id, &occ.output_path, &preview),
                };
                inner.occupy_exit = None;
                settled.push(Settlement {
                    data,
                    description: occ.description,
                    ended: Some(occ.ended),
                });
            }
            if inner.dialect == ShellDialect::Posix {
                orphans = inner.background.drain().map(|(_, task)| task).collect();
            }
        }
        for task in orphans {
            let killed = match task.pid {
                Ok(pid) => kill_job(pid).await,
                Err(error) => Err(error),
            };
            let preview = file_tail(&task.output_path, OUTPUT_LIMIT);
            let mut result = reset_result(&task.task_id, &task.output_path, &preview);
            if let Err(error) = killed {
                if let ToolContent::Text(text) = &mut result.content {
                    text.push_str(&format!("\nJob could not be terminated: {error}"));
                }
                if let Some(fields) = result.structured_content.as_mut() {
                    fields.insert("code".into(), "exec_terminate_failed".into());
                }
            }
            settled.push(Settlement {
                data: ExecEndedData {
                    task_id: task.task_id.clone(),
                    status: ExecStatus::Failed,
                    result,
                },
                description: task.description,
                ended: None,
            });
        }
        for settlement in settled {
            self.settle(settlement.data, &settlement.description);
            if let Some(notify) = settlement.ended {
                notify.notify_waiters();
            }
        }
    }

    /// Appends `exec/ended` and, for results the model should see, the wake
    /// input that carries it.
    fn settle(&self, data: ExecEndedData, description: &str) {
        if let Ok(event) = self
            .timeline
            .append(EventKind::ExecEnded(data.clone()).into())
            && wakes(&data)
        {
            let _ = self
                .timeline
                .append(wake_input(&event.id, &data, description.to_owned()));
        }
    }

    /// One foreground call: install its capture, send the wrapped command,
    /// poll the capture for its end marker, convert to an occupying task on
    /// timeout.
    async fn run_foreground(
        self: &Arc<Self>,
        command: &str,
        context: &CallContext,
        timeout_ms: u64,
        cancel: &CancellationToken,
    ) -> Result<ToolResult, String> {
        let (start, end) = marker_pair();
        let start_marker = start.full();
        let end_marker = end.full();
        let dialect = {
            let inner = self.inner.lock().await;
            inner.dialect
        };
        let wrapped = match dialect {
            ShellDialect::Posix => posix_command(command, &start, &end),
            ShellDialect::Pwsh => pwsh_command(command, &start, &end),
        };
        let capture = Capture::new(start_marker, end_marker);
        self.start_command(capture, &wrapped)
            .await
            .map_err(|e| e.to_string())?;
        let deadline = tokio::time::Instant::now() + Duration::from_millis(timeout_ms);
        loop {
            {
                let mut inner = self.inner.lock().await;
                if inner.pty.is_none() {
                    let note = inner
                        .shell_exit
                        .clone()
                        .unwrap_or_else(|| "[shell exited]".into());
                    let mut text = String::new();
                    if let Some(mut capture) = inner.capture.take() {
                        capture.settle(capture.pending.len());
                        push_output(&mut text, &capture.output);
                        if let Some(omitted) = capture.omitted() {
                            text.push_str(&format!(
                                "\n[Output truncated: {omitted} bytes omitted]"
                            ));
                        }
                    }
                    text.push_str(&note);
                    let mut result = ToolResult::text(text);
                    result.is_error = Some(true);
                    return Ok(ran(result, command, None));
                }
                if let Some(code) = inner
                    .capture
                    .as_ref()
                    .and_then(|capture| capture.code.clone())
                {
                    let capture = inner.capture.take().expect("capture present");
                    let mut text = String::new();
                    push_output(&mut text, &capture.output);
                    text.push_str(&exit_line(&code));
                    if let Some(omitted) = capture.omitted() {
                        text.push_str(&format!("\n[Output truncated: {omitted} bytes omitted]"));
                    }
                    let mut result = ToolResult::text(text);
                    result.is_error = (code != "0").then_some(true);
                    return Ok(ran(result, command, code.parse().ok()));
                }
            }
            if tokio::time::Instant::now() >= deadline {
                if self.config.member {
                    return self
                        .interrupt()
                        .await
                        .map(|result| ran(result, command, None));
                }
                return self
                    .occupy_current(command, context)
                    .await
                    .map(|result| ran(result, command, None));
            }
            tokio::select! {
                _ = cancel.cancelled() => {
                    return self
                        .interrupt()
                        .await
                        .map(|result| ran(result, command, None));
                }
                _ = tokio::time::sleep(Duration::from_millis(10)) => {}
            }
        }
    }

    /// Turn stop and member timeout: Ctrl+C, bounded marker wait; without
    /// the marker the shell tree is terminated and the session resets.
    async fn interrupt(self: &Arc<Self>) -> Result<ToolResult, String> {
        {
            let mut inner = self.inner.lock().await;
            if let Some(pty) = inner.pty.as_mut() {
                let _ = pty.write(b"\x03");
            }
        }
        let deadline = tokio::time::Instant::now() + STOP_WAIT;
        loop {
            {
                let mut inner = self.inner.lock().await;
                if inner.pty.is_none() {
                    inner.capture = None;
                    return Ok(ToolResult::error(
                        "[Command interrupted; the shell exited and will be reset]",
                    ));
                }
                if inner
                    .capture
                    .as_ref()
                    .is_some_and(|capture| capture.code.is_some())
                {
                    let capture = inner.capture.take().expect("capture present");
                    let mut text = String::new();
                    push_output(&mut text, &capture.output);
                    text.push_str("[Command interrupted]");
                    if let Some(omitted) = capture.omitted() {
                        text.push_str(&format!("\n[Output truncated: {omitted} bytes omitted]"));
                    }
                    return Ok(ToolResult::error(text));
                }
            }
            if tokio::time::Instant::now() >= deadline {
                let mut inner = self.inner.lock().await;
                inner.capture = None;
                if let Some(mut pty) = inner.pty.take() {
                    let _ = pty.terminate().await;
                }
                inner.shell_exit = Some("[shell was terminated]".into());
                inner.had_reset = true;
                return Ok(ToolResult::error(
                    "[Command interrupted; the shell was terminated and reset]",
                ));
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// ADR 013 D2: a task log the model is given is registered before the
    /// result leaves this part. The shell owns only paths under its own
    /// output directory; a registration failure fails the call.
    async fn register_output(&self, path: &Path) -> Result<(), String> {
        let Some(owner) = &self.owner else {
            return Ok(());
        };
        if !path.starts_with(&self.config.output_dir) {
            return Err(format!(
                "Shell output path {} is outside the branch's output directory",
                path.display()
            ));
        }
        owner
            .0
            .register(self.session_id.clone(), path.to_path_buf())
            .await
            .map_err(|failure| failure.to_string())
    }

    /// Registers the running command as the occupying task and reports it.
    /// The capture becomes the task's output as it stands: a start marker
    /// that arrives later is still dropped, and a settled end marker still
    /// settles the task.
    async fn occupy_current(
        self: &Arc<Self>,
        command: &str,
        context: &CallContext,
    ) -> Result<ToolResult, String> {
        let task_id = format!("exec-{}", uuid::Uuid::new_v4().simple());
        let output_path = self.config.output_dir.join(format!("{task_id}.log"));
        self.register_output(&output_path).await?;
        let description = describe(command);
        let (preview, omitted, settled) = {
            let mut inner = self.inner.lock().await;
            if self.lifecycle.is_cancelled() {
                return Err("shell session closed".into());
            }
            self.timeline
                .append(
                    EventKind::ExecStarted(ExecStartedData {
                        turn_id: context.turn_id.clone(),
                        request_id: context.request_id.clone(),
                        call_id: context.call_id.clone(),
                        task_id: task_id.clone(),
                        description: description.clone(),
                        title: None,
                        output_path: output_path.to_string_lossy().into_owned(),
                    })
                    .into(),
                )
                .map_err(|error| error.to_string())?;
            let (preview, omitted) = match inner.capture.as_mut() {
                Some(capture) => {
                    capture.bind(&output_path);
                    capture.delivered = capture.output.len();
                    (capture.output.clone(), capture.omitted())
                }
                None => (String::new(), None),
            };
            inner.occupy_tail = None;
            inner.occupy_exit = None;
            inner.occupying = Some(OccupyingTask {
                task_id: task_id.clone(),
                output_path: output_path.clone(),
                description: description.clone(),
                ended: Arc::new(Notify::new()),
            });
            (preview, omitted, advance_occupying(&mut inner))
        };
        if let Some(settled) = settled {
            self.settle(settled.data, &settled.description);
        }
        let mut text = String::new();
        push_output(&mut text, &preview);
        if let Some(omitted) = omitted {
            text.push_str(&format!("[Output truncated: {omitted} bytes omitted]\n"));
        }
        text.push_str(&format!(
            "Command is still running and occupies the shell.\nTask ID: {task_id}\nOutput: {}",
            output_path.display()
        ));
        let mut result = ToolResult::text(text);
        result.structured_content = Some(structured(json!({
            "type": "started",
            "taskId": task_id,
            "outputPath": output_path.to_string_lossy(),
        })));
        Ok(result)
    }

    /// Starts an explicit background job and returns its task id at once.
    async fn run_background(
        self: &Arc<Self>,
        command: &str,
        context: &CallContext,
    ) -> Result<ToolResult, String> {
        let task_id = format!("exec-{}", uuid::Uuid::new_v4().simple());
        let output_path = self.config.output_dir.join(format!("{task_id}.log"));
        self.register_output(&output_path).await?;
        let completion = marker_pair().1;
        let pid_marker = marker_pair().1;
        let description = describe(command);
        let dialect = {
            let inner = self.inner.lock().await;
            inner.dialect
        };
        let (wrapped, kind) = match dialect {
            ShellDialect::Posix => (
                posix_background(command, &output_path, &completion, &pid_marker),
                BackgroundKind::Posix,
            ),
            ShellDialect::Pwsh => {
                let shell = {
                    let inner = self.inner.lock().await;
                    inner.shell_exe.clone()
                }
                .ok_or("shell executable unknown")?;
                let result_path = self.config.output_dir.join(format!("{task_id}.result"));
                let error_path = self.config.output_dir.join(format!("{task_id}.err"));
                (
                    pwsh_background(
                        command,
                        &shell,
                        &output_path,
                        &error_path,
                        &result_path,
                        &completion,
                        &pid_marker,
                    ),
                    BackgroundKind::Pwsh {
                        result_path: result_path.clone(),
                        error_path: error_path.clone(),
                    },
                )
            }
        };
        let pwsh_paths = match &kind {
            BackgroundKind::Pwsh {
                result_path,
                error_path,
            } => Some((result_path.clone(), error_path.clone())),
            BackgroundKind::Posix => None,
        };
        {
            let mut inner = self.inner.lock().await;
            if self.lifecycle.is_cancelled() {
                return Err("shell session closed".into());
            }
            self.timeline
                .append(
                    EventKind::ExecStarted(ExecStartedData {
                        turn_id: context.turn_id.clone(),
                        request_id: context.request_id.clone(),
                        call_id: context.call_id.clone(),
                        task_id: task_id.clone(),
                        description: description.clone(),
                        title: None,
                        output_path: output_path.to_string_lossy().into_owned(),
                    })
                    .into(),
                )
                .map_err(|error| error.to_string())?;
            inner.background.insert(
                task_id.clone(),
                BackgroundTask {
                    task_id: task_id.clone(),
                    output_path: output_path.clone(),
                    end_marker: completion.full(),
                    pid: Err(std::io::Error::other("the job pid was not reported")),
                    kind,
                    description: description.clone(),
                },
            );
        }
        if let Err(error) = self.write_line(&wrapped).await {
            let task = self.inner.lock().await.background.remove(&task_id);
            if let Some(task) = task {
                self.settle(
                    ExecEndedData {
                        task_id: task_id.clone(),
                        status: ExecStatus::Failed,
                        result: ToolResult::error(error.to_string()),
                    },
                    &task.description,
                );
            }
            return Err(error.to_string());
        }
        let pid_marker_full = pid_marker.full();
        let deadline = tokio::time::Instant::now() + PID_WAIT;
        while tokio::time::Instant::now() < deadline {
            let found = {
                let inner = self.inner.lock().await;
                marker_code(&inner.retained, &pid_marker_full)
            };
            if let Some(code) = found {
                let mut inner = self.inner.lock().await;
                let Inner {
                    pty, background, ..
                } = &mut *inner;
                if let Some(task) = background.get_mut(&task_id) {
                    task.pid = code
                        .parse::<u32>()
                        .map_err(|_| {
                            std::io::Error::new(std::io::ErrorKind::InvalidData, "Invalid job pid")
                        })
                        .and_then(|pid| {
                            pty.as_ref()
                                .ok_or_else(|| std::io::Error::other("shell exited"))?
                                .job_pid(pid)
                        });
                }
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        if let Some((result_path, error_path)) = pwsh_paths {
            let this = self.clone();
            let lifecycle = self.lifecycle.clone();
            let marker = completion.full();
            let watched = task_id.clone();
            let output_path = output_path.clone();
            let tasks = self.tasks.lock().await;
            tasks.spawn(async move {
                loop {
                    tokio::select! {
                        _ = lifecycle.cancelled() => return,
                        _ = tokio::time::sleep(PWSH_FILE_POLL) => {}
                    }
                    let finished = {
                        let inner = this.inner.lock().await;
                        match inner.background.get(&watched) {
                            None => return,
                            Some(task) => std::fs::read_to_string(&result_path)
                                .ok()
                                .and_then(|text| marker_code(&text, &marker))
                                .map(|code| (code, task.description.clone())),
                        }
                    };
                    if let Some((code, task_description)) = finished {
                        let task_id = watched.clone();
                        let mut inner = this.inner.lock().await;
                        inner.background.remove(&task_id);
                        drop(inner);
                        let preview = file_tail(&output_path, OUTPUT_LIMIT);
                        let mut result = ended_result(&task_id, &output_path, &preview, &code);
                        if let ToolContent::Text(text) = &mut result.content {
                            text.push_str(&format!("\nStandard error: {}", error_path.display()));
                        }
                        let data = ExecEndedData {
                            task_id: task_id.clone(),
                            status: exit_status(&code),
                            result,
                        };
                        this.settle(data, &task_description);
                        return;
                    }
                }
            });
        }
        let mut result = ToolResult::text(format!(
            "Background command started.\nTask ID: {task_id}\nOutput: {}",
            output_path.display()
        ));
        result.structured_content = Some(structured(json!({
            "type": "started",
            "taskId": task_id,
            "outputPath": output_path.to_string_lossy(),
        })));
        Ok(ran(result, command, None))
    }

    /// Stops a task by id.
    pub async fn stop(&self, task_id: &str) -> Result<ExecEndedData, TaskError> {
        let occupying = {
            let mut inner = self.inner.lock().await;
            if inner
                .occupying
                .as_ref()
                .is_some_and(|task| task.task_id == task_id)
            {
                inner.occupying.take()
            } else {
                None
            }
        };
        if let Some(occ) = occupying {
            {
                let mut inner = self.inner.lock().await;
                if let Some(capture) = inner.capture.as_mut() {
                    capture.settle(capture.pending.len());
                }
                if let Some(pty) = inner.pty.as_mut() {
                    let _ = pty.write(b"\x03");
                }
            }
            let deadline = tokio::time::Instant::now() + STOP_WAIT;
            let mut found = false;
            loop {
                let state = {
                    let inner = self.inner.lock().await;
                    (
                        inner.pty.is_none(),
                        inner
                            .capture
                            .as_ref()
                            .is_some_and(|capture| capture.code.is_some()),
                    )
                };
                if state.0 || state.1 {
                    found = state.1;
                    break;
                }
                if tokio::time::Instant::now() >= deadline {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            {
                let mut inner = self.inner.lock().await;
                if let Some(capture) = inner.capture.take() {
                    inner.occupy_tail = Some(capture.output);
                }
                if !found {
                    if let Some(mut pty) = inner.pty.take() {
                        let _ = pty.terminate().await;
                    }
                    inner.shell_exit = Some("[shell was terminated]".into());
                    inner.had_reset = true;
                }
            }
            let data = ExecEndedData {
                task_id: task_id.to_owned(),
                status: ExecStatus::Cancelled,
                result: ToolResult::text("Command interrupted."),
            };
            self.settle(data.clone(), &occ.description);
            occ.ended.notify_waiters();
            return Ok(data);
        }
        let task = {
            let mut inner = self.inner.lock().await;
            inner.background.remove(task_id)
        };
        if let Some(task) = task {
            let terminated = match task.pid {
                Ok(pid) => stop_job(pid).await,
                Err(error) => Err(error),
            };
            let data = match terminated {
                Ok(()) => ExecEndedData {
                    task_id: task_id.to_owned(),
                    status: ExecStatus::Cancelled,
                    result: ToolResult::text("Background command stopped."),
                },
                Err(error) => {
                    let mut result = ToolResult::error(format!(
                        "Background command could not be terminated: {error}\nOutput: {}",
                        task.output_path.display()
                    ));
                    result.structured_content = Some(structured(json!({
                        "code": "exec_terminate_failed",
                        "taskId": task_id,
                        "outputPath": task.output_path.to_string_lossy(),
                    })));
                    ExecEndedData {
                        task_id: task_id.to_owned(),
                        status: ExecStatus::Failed,
                        result,
                    }
                }
            };
            self.settle(data.clone(), &task.description);
            return Ok(data);
        }
        Err(TaskError::Unknown)
    }

    async fn write_line(&self, line: &str) -> Result<(), HostError> {
        let mut inner = self.inner.lock().await;
        let Some(pty) = inner.pty.as_mut() else {
            return Err(HostError::failed("Shell is not running"));
        };
        pty.write(format!("{line}\n").as_bytes())
            .map_err(|e| HostError::failed(format!("Shell write failed: {e}")))
    }
}

impl ExecStop for ShellSession {
    fn stop(
        self: Arc<Self>,
        task_id: String,
    ) -> BoxFuture<'static, Result<ExecEndedData, TaskError>> {
        Box::pin(async move { ShellSession::stop(&self, &task_id).await })
    }
}

/// Settles the occupying task once its capture saw a complete end marker
/// line, taking the capture with it.
fn advance_occupying(inner: &mut Inner) -> Option<Settlement> {
    let code = inner.capture.as_ref()?.code.clone()?;
    let occ = inner.occupying.take()?;
    let capture = inner.capture.take();
    let preview = file_tail(&occ.output_path, OUTPUT_LIMIT);
    if let Some(capture) = &capture {
        inner.occupy_tail = Some(capture.output.clone());
    }
    inner.occupy_exit = Some(exit_line(&code));
    Some(Settlement {
        data: ExecEndedData {
            task_id: occ.task_id.clone(),
            status: exit_status(&code),
            result: ended_result(&occ.task_id, &occ.output_path, &preview, &code),
        },
        description: occ.description,
        ended: Some(occ.ended),
    })
}

/// The plugin.
pub struct ShellSessionPlugin {
    facts: Key<crate::file_access::FileAccessFacts>,
    session: Key<ShellSession>,
    stop: Key<dyn ExecStop>,
    timeline: Key<Timeline>,
    dispatcher: Key<Dispatcher>,
    host: Key<HostPlatform>,
    lifecycle: Key<dim_agent_runtime::BranchLifecycle>,
    session_config: Key<dim_agent_runtime::SessionConfig>,
    identity: Key<dim_agent_runtime::TimelineIdentity>,
    config: Option<ShellSessionConfig>,
}

impl ShellSessionPlugin {
    pub fn new(names: &Names<'_>, config: ShellSessionConfig) -> rutis::Result<Self> {
        Ok(Self {
            config: Some(config),
            ..Self::session(names)?
        })
    }

    /// Derive execution settings from the owning Session.
    pub fn session(names: &Names<'_>) -> rutis::Result<Self> {
        Ok(Self {
            facts: names.key()?,
            session: names.key()?,
            stop: names.key()?,
            timeline: names.key()?,
            dispatcher: names.key()?,
            host: names.key()?,
            lifecycle: names.key()?,
            session_config: names.key()?,
            identity: names.key()?,
            config: None,
        })
    }
}

impl DynPlugin for ShellSessionPlugin {
    fn name(&self) -> &str {
        "ShellSession"
    }
    fn injects(&self) -> Vec<TypeKey> {
        vec![
            self.timeline.erase(),
            self.dispatcher.erase(),
            self.host.erase(),
            self.lifecycle.erase(),
            self.session_config.erase(),
            self.identity.erase(),
        ]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let timeline = ctx.require_at(&self.timeline)?;
            let dispatcher = ctx.require_at(&self.dispatcher)?;
            let host = ctx.require_at(&self.host)?;
            let config = ctx.require_at(&self.session_config)?;
            let identity = ctx.require_at(&self.identity)?;
            let owner = ctx.get::<SessionFilesHost>();
            let mut session = ShellSession::new(
                self.config.clone().unwrap_or_else(|| ShellSessionConfig {
                    cwd: PathBuf::from(&config.cwd),
                    member: config.collaboration_role
                        == dim_agent_runtime::CollaborationRole::Member,
                    output_dir: host
                        .home
                        .join(".dim/exec")
                        .join(&identity.session_id)
                        .join(&identity.branch_id),
                }),
                host,
                timeline,
                dispatcher,
                owner,
                identity.session_id.clone(),
            );
            session.facts = Some((ctx.clone(), self.facts.clone()));
            let session = Arc::new(session);
            let service = session.clone();
            ctx.effect(move || async move {
                service.lifecycle.cancel();
                {
                    let mut inner = service.inner.lock().await;
                    if let Some(mut pty) = inner.pty.take() {
                        let _ = pty.terminate().await;
                    }
                    for queued in inner.queue.drain(..) {
                        let _ = queued.done.send(Err("shell session closed".into()));
                    }
                }
                let tasks = service.tasks.lock().await.clone();
                tasks.close();
                tasks.wait().await;
                let generation = service.inner.lock().await.generation;
                service.handle_shell_exit(generation, None).await;
                let remaining = service
                    .inner
                    .lock()
                    .await
                    .background
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>();
                for task in remaining {
                    let _ = ShellSession::stop(&service, &task).await;
                }
                Ok(())
            })?;
            ctx.provide_at(self.session.clone(), session.clone())?;
            ctx.provide_at::<dyn ExecStop>(self.stop.clone(), session)?;
            Ok(Effect::Done)
        })
    }
}

fn shell_args(dialect: ShellDialect) -> Vec<std::ffi::OsString> {
    match dialect {
        ShellDialect::Posix => vec!["-i".into()],
        ShellDialect::Pwsh => vec!["-NoLogo".into(), "-NoProfile".into(), "-Command".into()],
    }
}

/// Declares the command the call sent to the shell (session boundary A.6):
/// the exit status the end marker carried, or none while it was still
/// running.
fn ran(mut result: ToolResult, command: &str, exit_code: Option<i32>) -> ToolResult {
    result.effects = Some(vec![ToolEffect::CommandRun {
        command: command.to_owned(),
        exit_code,
        signal: None,
    }]);
    result
}

fn describe(command: &str) -> String {
    command
        .lines()
        .next()
        .unwrap_or_default()
        .chars()
        .take(DESCRIPTION_CHARS)
        .collect()
}

fn structured(value: serde_json::Value) -> JsonObject {
    value.as_object().cloned().unwrap_or_default()
}

/// Appends `output`, then a newline when it does not end with one, so the
/// note that follows starts on its own line.
fn push_output(text: &mut String, output: &str) {
    if output.is_empty() {
        return;
    }
    text.push_str(output);
    if !output.ends_with('\n') {
        text.push('\n');
    }
}

fn file_tail(path: &Path, limit: usize) -> String {
    std::fs::read(path)
        .map(|bytes| tail_bytes(&String::from_utf8_lossy(&bytes), limit).to_owned())
        .unwrap_or_default()
}

/// The token on the line of the last `marker` once that line is complete:
/// exit digits, pwsh `failed`, or a job pid. A malformed token reads as
/// `unknown`.
fn marker_code(text: &str, marker: &str) -> Option<String> {
    let rest = &text[text.rfind(marker)? + marker.len()..];
    let token = rest[..rest.find('\n')?].trim();
    let valid = !token.is_empty()
        && token.len() <= 12
        && token.chars().all(|ch| ch.is_ascii_alphanumeric());
    Some(if valid { token } else { "unknown" }.to_owned())
}

fn exit_status(code: &str) -> ExecStatus {
    match code {
        "0" => ExecStatus::Completed,
        _ => ExecStatus::Failed,
    }
}

fn exit_line(code: &str) -> String {
    match code {
        "failed" => "[Command failed]".to_owned(),
        "unknown" => "[Command finished with unknown exit code]".to_owned(),
        code => format!("[Command finished with exit code {code}]"),
    }
}

fn ended_result(task_id: &str, output_path: &Path, preview: &str, code: &str) -> ToolResult {
    let status = exit_status(code);
    let label = match status {
        ExecStatus::Completed => "completed",
        ExecStatus::Failed => "failed",
        ExecStatus::Cancelled => "cancelled",
    };
    let mut text = preview.to_owned();
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(&exit_line(code));
    let mut result = ToolResult::text(text);
    result.is_error = (status == ExecStatus::Failed).then_some(true);
    result.structured_content = Some(structured(json!({
        "type": "finished",
        "status": label,
        "taskId": task_id,
        "outputPath": output_path.to_string_lossy(),
    })));
    result
}

/// A shell death settles open tasks as failed with a reset note.
fn reset_result(task_id: &str, output_path: &Path, preview: &str) -> ToolResult {
    let mut text = preview.to_owned();
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str("[Task failed: shell was reset]");
    let mut result = ToolResult::text(text);
    result.is_error = Some(true);
    result.structured_content = Some(structured(json!({
        "type": "finished",
        "status": "failed",
        "taskId": task_id,
        "outputPath": output_path.to_string_lossy(),
    })));
    result
}

/// Removes a completion marker line from a capture.
fn strip_marker(text: &mut String, marker: &str) {
    if let Some(pos) = text.find(marker) {
        let end = text[pos..]
            .find('\n')
            .map_or(text.len(), |offset| pos + offset + 1);
        text.replace_range(pos..end, "");
    }
}

/// Where the pending tail that may still become a marker starts.
///
/// A tail is held only while it is still short enough to become one, so
/// output that merely starts with `__DIM_` cannot keep the rest of an
/// arbitrarily long line out of the log.
fn marker_hold(text: &str) -> usize {
    const PREFIX: &str = "__DIM_";
    if let Some(pos) = text.rfind(PREFIX)
        && !text[pos..].contains('\n')
    {
        return if text.len() - pos <= MARKER_LINE_LIMIT {
            pos
        } else {
            text.len()
        };
    }
    (1..PREFIX.len())
        .rev()
        .find(|len| text.ends_with(&PREFIX[..*len]))
        .map_or(text.len(), |len| text.len() - len)
}

fn regularize_output(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let mut out = String::new();
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            while let Some(&next) = chars.peek() {
                if next.is_ascii_alphabetic() || next == '[' {
                    chars.next();
                    break;
                }
                chars.next();
            }
        } else if ch == '\r' {
            if chars.peek() == Some(&'\n') {
                chars.next();
                out.push('\n');
            } else {
                let mut segment = String::new();
                while let Some(&next) = chars.peek() {
                    if next == '\n' {
                        break;
                    }
                    segment.push(next);
                    chars.next();
                }
                out.push_str(&segment);
            }
        } else {
            out.push(ch);
        }
    }
    out
}

fn tail_bytes(s: &str, limit: usize) -> &str {
    if s.len() <= limit {
        return s;
    }
    let mut start = s.len() - limit;
    while !s.is_char_boundary(start) {
        start += 1;
    }
    &s[start..]
}

struct MarkerPair {
    first: String,
    second: String,
}

impl MarkerPair {
    fn first(&self) -> &str {
        &self.first
    }
    fn second(&self) -> &str {
        &self.second
    }
    fn full(&self) -> String {
        format!("{}{}", self.first, self.second)
    }
}

fn marker_pair() -> (MarkerPair, MarkerPair) {
    let id = uuid::Uuid::new_v4().to_string().replace('-', "");
    let start = MarkerPair {
        first: format!("__DIM_START_{}_", &id[..8]),
        second: format!("{}__", &id[8..16]),
    };
    let end = MarkerPair {
        first: format!("__DIM_END_{}_", &id[16..24]),
        second: format!("{}__", &id[24..32]),
    };
    (start, end)
}

fn posix_escape(command: &str) -> String {
    let mut out = String::new();
    for ch in command.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("'\\''"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            other => out.push(other),
        }
    }
    out
}

fn posix_command(command: &str, start: &MarkerPair, end: &MarkerPair) -> String {
    format!(
        "printf '%s%s' '{}' '{}'; eval $'{}'; printf '%s%s%d\\n' '{}' '{}' \"$?\"",
        start.first(),
        start.second(),
        posix_escape(command),
        end.first(),
        end.second(),
    )
}

/// The POSIX background wrapper: the job writes to the output file and its
/// completion marker back to the PTY; the parent reports the job pid. The
/// job leaves the job table at once, so no job-status report reaches a
/// later capture; it keeps its own process group.
fn posix_background(
    command: &str,
    output_path: &Path,
    completion: &MarkerPair,
    pid: &MarkerPair,
) -> String {
    format!(
        "{{ {{ eval $'{}'; }} > '{}' 2>&1; printf '%s%s%d\\n' '{}' '{}' \"$?\"; }} & disown\nprintf '%s%s%d\\n' '{}' '{}' \"$!\"",
        posix_escape(command),
        output_path.display(),
        completion.first(),
        completion.second(),
        pid.first(),
        pid.second(),
    )
}

fn pwsh_command(command: &str, start: &MarkerPair, end: &MarkerPair) -> String {
    let encoded = base64_encode(command);
    format!(
        "$__dim_before = $LASTEXITCODE; Write-Output ('{}' + '{}'); Invoke-Expression ([Text.Encoding]::UTF8.GetString([Convert]::FromBase64String(\"{encoded}\"))); $__dim_ok = $?; $__dim_code = $LASTEXITCODE; $__dim_exit = $(if ($__dim_code -ne $__dim_before) {{ $__dim_code }} elseif (-not $__dim_ok) {{ 'failed' }} else {{ 0 }}); Write-Output ('{}' + '{}' + $__dim_exit)",
        start.first(),
        start.second(),
        end.first(),
        end.second(),
    )
}

/// The pwsh background wrapper: a detached child runs the command, appends
/// its completion marker to the result file, and the parent reports the
/// child pid. Composed piecewise: the nested single-quoted literals double
/// their inner quotes.
fn pwsh_background(
    command: &str,
    shell: &Path,
    output_path: &Path,
    error_path: &Path,
    result_path: &Path,
    completion: &MarkerPair,
    pid: &MarkerPair,
) -> String {
    let encoded = base64_encode(command);
    let mut out = String::new();
    out.push_str("$__dim_p = Start-Process -FilePath '");
    out.push_str(&shell.to_string_lossy());
    out.push_str("' -WorkingDirectory (Get-Location -PSProvider FileSystem).Path -ArgumentList '-NoProfile','-Command','");
    out.push_str(
        "$__dim_before = $LASTEXITCODE; Invoke-Expression ([Text.Encoding]::UTF8.GetString([Convert]::FromBase64String(\"",
    );
    out.push_str(&encoded);
    out.push_str("\"))); $__dim_ok = $?; $__dim_code = $LASTEXITCODE; $__dim_exit = $(if ($__dim_code -ne $__dim_before) { $__dim_code } elseif (-not $__dim_ok) { 'failed' } else { 0 }); Add-Content ''",
    );
    out.push_str(&result_path.to_string_lossy());
    out.push_str("'' (''");
    out.push_str(completion.first());
    out.push_str("'' + ''");
    out.push_str(completion.second());
    out.push_str("'' + $__dim_exit)'");
    out.push_str(" -RedirectStandardOutput '");
    out.push_str(&output_path.to_string_lossy());
    out.push_str("' -RedirectStandardError '");
    out.push_str(&error_path.to_string_lossy());
    out.push_str("' -PassThru\nWrite-Output ('");
    out.push_str(pid.first());
    out.push_str("' + '");
    out.push_str(pid.second());
    out.push_str("' + $__dim_p.Id)");
    out
}

fn base64_encode(data: &str) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(data.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    const START: &str = "__DIM_START_aaaabbbb_ccccdddd__";
    const END: &str = "__DIM_END_eeeeffff_00001111__";

    fn capture() -> Capture {
        Capture::new(START.into(), END.into())
    }

    /// Feeds a chunk and settles it the way the reader does.
    fn feed(capture: &mut Capture, text: &str) {
        capture.append(text);
        capture.drain();
    }

    #[test]
    fn a_start_marker_split_across_chunks_is_found_and_the_echo_dropped() {
        let mut capture = capture();
        feed(
            &mut capture,
            "$ printf '%s%s' '__DIM_START_aaaabbbb_' 'ccccdddd__'; eval\n",
        );
        feed(&mut capture, "echo hi\nhi\n");
        // The marker arrives in two chunks; neither half carries it alone.
        feed(&mut capture, "__DIM_START_aaaabbbb_");
        feed(&mut capture, "ccccdddd__");
        feed(&mut capture, "line one\nline two\n");
        feed(&mut capture, &format!("{END}0\n"));
        assert_eq!(capture.code.as_deref(), Some("0"));
        assert_eq!(capture.output, "line one\nline two\n");
        assert_eq!(capture.omitted(), None);
    }

    #[test]
    fn a_start_marker_and_echo_after_conversion_stay_out_of_the_result() {
        let mut capture = capture();
        feed(
            &mut capture,
            "$ printf '%s%s' '__DIM_START_aaaabbbb_' 'ccccdddd__'\n",
        );
        feed(&mut capture, START);
        feed(&mut capture, "output\n");
        feed(&mut capture, &format!("{END}3\n"));
        assert_eq!(capture.code.as_deref(), Some("3"));
        assert_eq!(capture.output, "output\n");
        assert!(!capture.output.contains("__DIM_"));
    }

    #[test]
    fn a_capture_converted_after_the_limit_reports_the_omission_and_keeps_the_rest() {
        let mut capture = capture();
        feed(&mut capture, START);
        let burst = "y".repeat(OUTPUT_LIMIT * 2);
        feed(&mut capture, &burst);
        let path = std::env::temp_dir().join(format!("capture-bind-{}", uuid::Uuid::new_v4()));
        capture.bind(&path);
        feed(&mut capture, "after\n");
        feed(&mut capture, &format!("{END}0\n"));
        assert_eq!(capture.code.as_deref(), Some("0"));
        let file = std::fs::read_to_string(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        assert!(
            file.starts_with(&burst[..100]),
            "the earliest bytes are kept"
        );
        assert!(
            file.contains(&format!(
                "[Output truncated: {} bytes omitted]",
                OUTPUT_LIMIT
            )),
            "{file}"
        );
        assert!(file.ends_with("after\n"), "{}", &file[file.len() - 40..]);
        assert!(!file.contains("__DIM_"));
    }

    #[test]
    fn an_end_marker_arriving_after_the_output_passed_the_limit_still_settles() {
        let mut capture = capture();
        feed(&mut capture, START);
        feed(&mut capture, &"x".repeat(OUTPUT_LIMIT * 2));
        assert!(capture.code.is_none());
        feed(&mut capture, &format!("\n{END}0\n"));
        assert_eq!(capture.code.as_deref(), Some("0"));
        assert_eq!(capture.output.len(), OUTPUT_LIMIT);
        assert_eq!(capture.omitted(), Some(OUTPUT_LIMIT + 1));
    }

    #[test]
    fn a_burst_past_the_limit_keeps_the_earliest_bytes() {
        let mut capture = capture();
        feed(&mut capture, START);
        let mut expected = String::new();
        for index in 0..5000 {
            let line = format!("{index}\n");
            expected.push_str(&line);
            feed(&mut capture, &line);
        }
        feed(&mut capture, &format!("{END}0\n"));
        assert_eq!(capture.code.as_deref(), Some("0"));
        assert!(expected.starts_with(&capture.output), "the earliest bytes");
        assert_eq!(
            capture.omitted(),
            Some(expected.len() - capture.output.len()),
            "every byte past the limit is reported"
        );
    }

    #[test]
    fn a_long_line_starting_with_the_marker_prefix_is_settled_not_held() {
        let mut capture = capture();
        feed(&mut capture, START);
        let path = std::env::temp_dir().join(format!("capture-hold-{}", uuid::Uuid::new_v4()));
        capture.bind(&path);

        feed(&mut capture, "__DIM_");
        let body = "a".repeat(1024 * 1024);
        let mut settled = String::from("__DIM_");
        for chunk in body.as_bytes().chunks(4096) {
            let text = std::str::from_utf8(chunk).unwrap();
            settled.push_str(text);
            feed(&mut capture, text);
            assert!(
                capture.pending.len() <= MARKER_LINE_LIMIT,
                "a line that can no longer become a marker is held: {} bytes",
                capture.pending.len()
            );
        }
        assert!(capture.code.is_none());
        feed(&mut capture, "\n");
        feed(&mut capture, &format!("{END}0\n"));
        assert_eq!(capture.code.as_deref(), Some("0"));

        let file = std::fs::read_to_string(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        let expected = format!("{settled}\n");
        assert!(
            file.starts_with(&expected[..MARKER_LINE_LIMIT]),
            "the log starts with the line's first bytes"
        );
        assert!(
            file.ends_with(&expected),
            "the log holds the whole line: {} bytes",
            file.len()
        );
        assert_eq!(capture.output.len(), OUTPUT_LIMIT);
        assert_eq!(capture.omitted(), Some(expected.len() - OUTPUT_LIMIT));
    }
}

//! Persistent shell execution capability: one long-lived PTY shell per branch.
//!
//! Every command runs in the same interactive shell, wrapped with runtime
//! marker pairs: the
//! command text carries the fragments only, so echoes cannot forge a full
//! marker. A dedicated OS thread owns the blocking PTY reader and forwards
//! chunks to an async consumer through a channel; no lock is held across a
//! blocking read. Commands are serialized through a queue; a foreground
//! timeout converts the call into an occupying task, and explicit
//! background jobs report completion through their own markers, ending in
//! `exec/ended`. BackgroundExec consumes terminals and owns their notices and recovery.

use crate::workspace_tools::background::BackgroundExec;
use crate::{
    exec_stop::{ExecStop, TaskError},
    host::{HostError, SessionFiles},
    host_platform::{
        HostPlatform, HostPlatformError, OutputRequest, ShellCommandStart, ShellEdition,
        ShellEvent, ShellEventRequest, ShellJobStart, ShellRequest, ShellStatus, ShellWrite,
        TaskRequest, WaitReply, WaitRequest, WaitStatus,
    },
};
use base64::{Engine, engine::general_purpose::STANDARD};
use dim_agent_runtime::{Dispatcher, Names, SessionLifecycle, Timeline};
use dim_agent_types::{
    EventKind, ExecEndedData, ExecStartedData, ExecStatus, JsonObject, ToolContent, ToolEffect,
    ToolResult,
};
use rutis::Ctx;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, Key, TypeKey};
use serde_json::json;
use std::{
    collections::{HashMap, VecDeque},
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
/// limit, which is far above the longest marker line it must hold.
const RETAINED_BUFFER_TRIM: usize = 2 * RETAINED_BUFFER_LIMIT;
const OUTPUT_LIMIT: usize = 16_000;
const MAX_TIMEOUT_MS: u64 = 3_600_000;
const DESCRIPTION_CHARS: usize = 200;
const INIT_TIMEOUT: Duration = Duration::from_secs(10);
const STOP_WAIT: Duration = Duration::from_secs(2);

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

/// The foreground call that outlived its timeout; the shell is occupied
/// until its end marker arrives.
struct OccupyingTask {
    task_id: String,
    output_path: PathBuf,
    delivered: usize,
    ended: Arc<Notify>,
    _hold: Option<dim_agent_runtime::SessionHold>,
}

struct BackgroundTask {
    task_id: String,
    output_path: PathBuf,
    _hold: Option<dim_agent_runtime::SessionHold>,
}

struct Inner {
    shell: Option<String>,
    /// Counts spawned shells; a consumer loop serves only its own shell.
    generation: u64,
    dialect: ShellDialect,
    shell_exe: Option<PathBuf>,
    queue: VecDeque<QueuedCommand>,
    queue_running: bool,
    /// Raw stream tail, for the markers of commands that own no capture:
    /// initialization, background jobs and their pids.
    retained: String,
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

/// A terminal produced by the reader and its waiting caller.
struct Settlement {
    data: ExecEndedData,
    ended: Option<Arc<Notify>>,
}

pub struct ShellSession {
    facts: Option<(Ctx, Key<crate::file_access::FileAccessFacts>)>,
    inner: tokio::sync::Mutex<Inner>,
    config: ShellSessionConfig,
    lifecycle: CancellationToken,
    host: Arc<HostPlatform>,
    timeline: Arc<Timeline>,
    #[expect(
        dead_code,
        reason = "stored at composition; no shell path dispatches through it"
    )]
    dispatcher: Arc<Dispatcher>,
    /// SessionStore's output ownership (ADR 013); absent in hosts without
    /// native storage, where shell output has no owner.
    owner: Option<Arc<SessionFiles>>,
    session_id: String,
    session_lifecycle: Option<Arc<SessionLifecycle>>,
    recovery: Option<Arc<BackgroundExec>>,
    tasks: Arc<tokio::sync::Mutex<TaskTracker>>,
}

impl ShellSession {
    /// The branch must mount BackgroundExec to recover exec facts and publish completion notices.
    pub fn new(
        config: ShellSessionConfig,
        host: Arc<HostPlatform>,
        timeline: Arc<Timeline>,
        dispatcher: Arc<Dispatcher>,
        owner: Option<Arc<SessionFiles>>,
        session_id: String,
    ) -> Self {
        Self {
            facts: None,
            inner: tokio::sync::Mutex::new(Inner {
                shell: None,
                generation: 0,
                dialect: ShellDialect::Posix,
                shell_exe: None,
                queue: VecDeque::new(),
                queue_running: false,
                retained: String::new(),
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
            session_lifecycle: None,
            recovery: None,
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
        let (notify, task_id, output_path) = {
            let inner = self.inner.lock().await;
            let task = inner.occupying.as_ref()?;
            (
                task.ended.clone(),
                task.task_id.clone(),
                task.output_path.clone(),
            )
        };
        let notified = notify.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        let deadline =
            tokio::time::Instant::now() + Duration::from_millis(queued.timeout_ms.min(86_400_000));
        let ended = tokio::select! {
            _ = queued.cancel.cancelled() => {
                return Some(Err("command cancelled while waiting for the shell".into()));
            }
            _ = self.lifecycle.cancelled() => {
                return Some(Err("shell session closed while waiting for the shell".into()));
            }
            _ = &mut notified => true,
            _ = tokio::time::sleep_until(deadline) => false,
        };
        let inner = self.inner.lock().await;
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
        let delivered = inner.occupying.as_ref().expect("checked above").delivered;
        drop(inner);
        let page = self
            .host
            .output(OutputRequest {
                task_id: task_id.clone(),
                cursor: Some(delivered as u64),
                limit: Some(OUTPUT_LIMIT as u64),
            })
            .await
            .ok();
        let mut text = String::new();
        if let Some(page) = page {
            if !page.text.is_empty() {
                text.push_str(&page.text);
                if !page.text.ends_with('\n') {
                    text.push('\n');
                }
            }
            let mut inner = self.inner.lock().await;
            if let Some(task) = inner
                .occupying
                .as_mut()
                .filter(|task| task.task_id == task_id)
            {
                task.delivered =
                    page.next_cursor
                        .unwrap_or(task.delivered as u64 + page.bytes) as usize;
            }
        }
        text.push_str(&format!(
            "Task {} still occupies the shell. The command was not executed; resend it after the task ends.\nTask ID: {}\nOutput: {}",
            task_id,
            task_id,
            output_path.display()
        ));
        Some(Ok(ToolResult::text(text)))
    }

    async fn write_shell(&self, bytes: &[u8]) -> Result<(), HostError> {
        let inner = self.inner.lock().await;
        let Some(task_id) = inner.shell.clone() else {
            return Err(HostError::failed("Shell is not running"));
        };
        self.host
            .write(ShellWrite {
                task_id,
                data: STANDARD.encode(bytes),
                command: None,
                job: None,
            })
            .await
            .map_err(|error| HostError::failed(format!("Shell write failed: {error}")))
    }

    async fn stop_shell(&self, task_id: &str) {
        let _ = self
            .host
            .kill(TaskRequest {
                task_id: task_id.to_owned(),
            })
            .await;
        let _ = self
            .host
            .settle(TaskRequest {
                task_id: task_id.to_owned(),
            })
            .await;
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
            if inner.shell.is_some() {
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
        let roots = match self.facts.as_ref().and_then(|(ctx, key)| ctx.get_at(key)) {
            Some(facts) => {
                facts
                    .0
                    .workspace_roots(&self.config.cwd, self.lifecycle.clone())
                    .await?
            }
            None => vec![self.config.cwd.clone()],
        };
        let runtime = match self.host.persistent_shell().await {
            ShellStatus::Available(shell) => shell,
            ShellStatus::Unavailable { code, reason } => {
                return Err(HostError::failed(format!("{code}: {reason}")));
            }
        };
        let id = format!("shell-{}", uuid::Uuid::new_v4());
        let cwd = self
            .config
            .cwd
            .to_str()
            .ok_or_else(|| HostError::failed("the shell cwd is not UTF-8"))?
            .to_owned();
        let output_dir = self
            .config
            .output_dir
            .to_str()
            .ok_or_else(|| HostError::failed("the shell output directory is not UTF-8"))?
            .to_owned();
        let roots = roots
            .iter()
            .map(|root| {
                root.to_str()
                    .map(str::to_owned)
                    .ok_or_else(|| HostError::failed("a shell workspace root is not UTF-8"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let reply = self
            .host
            .shell(ShellRequest {
                task_id: id.clone(),
                cwd: cwd.clone(),
                workspace: cwd,
                roots,
                output_dir,
                cols: 500,
                rows: 50,
            })
            .await
            .map_err(|error| HostError::failed(error.to_string()))?;
        let task_id = reply.task_id.clone();
        if let Err(error) = self
            .register_output(std::path::Path::new(&reply.output_path))
            .await
        {
            let _ = self
                .host
                .kill(TaskRequest {
                    task_id: task_id.clone(),
                })
                .await;
            let _ = self
                .host
                .wait(
                    WaitRequest {
                        task_id: task_id.clone(),
                        timeout_ms: None,
                    },
                    never_cancelled(),
                )
                .await;
            let _ = self
                .host
                .settle(TaskRequest {
                    task_id: task_id.clone(),
                })
                .await;
            let _ = std::fs::remove_file(&reply.output_path);
            return Err(HostError::failed(error));
        }
        let dialect = match runtime.edition {
            ShellEdition::Pwsh | ShellEdition::WindowsPowershell => ShellDialect::Pwsh,
            _ => ShellDialect::Posix,
        };
        let exe = runtime.executable_path;
        let task_id = reply.task_id;
        let generation = {
            let mut inner = self.inner.lock().await;
            if self.lifecycle.is_cancelled() {
                drop(inner);
                self.stop_shell(&task_id).await;
                return Err(HostError::Cancelled("shell session closed".into()));
            }
            inner.generation += 1;
            inner.shell_exe = Some(PathBuf::from(exe));
            inner.dialect = dialect;
            inner.shell_exit = None;
            inner.had_reset = false;
            inner.occupy_tail = None;
            inner.occupy_exit = None;
            inner.shell = Some(task_id.clone());
            inner.generation
        };
        let this = self.clone();
        let lifecycle = self.lifecycle.clone();
        {
            let tasks = self.tasks.lock().await;
            tasks.spawn(async move {
                let mut event_cursor = 0u64;
                let wait_cancel = lifecycle.clone();
                let cancelled: crate::host::CancelledFn = std::sync::Arc::new(move || {
                    let cancel = wait_cancel.clone();
                    Box::pin(async move {
                        cancel.cancelled().await;
                        Ok(())
                    })
                });
                let wait = this.host.wait(
                    WaitRequest {
                        task_id: task_id.clone(),
                        timeout_ms: None,
                    },
                    cancelled,
                );
                tokio::pin!(wait);
                loop {
                    tokio::select! {
                        biased;
                        _ = lifecycle.cancelled() => break,
                        page = this.host.shell_events(ShellEventRequest {
                            task_id: task_id.clone(),
                            cursor: event_cursor,
                            wait_ms: 100,
                            limit: 64 * 1024,
                        }) => match page {
                            Ok(page) => {
                                for event in page.events {
                                    this.handle_event(event).await;
                                }
                                event_cursor = page.next_cursor;
                            }
                            Err(_) => break,
                        },
                        result = &mut wait => {
                            let end = result.ok();
                            loop {
                                let page = this.host.shell_events(ShellEventRequest {
                                    task_id: task_id.clone(),
                                    cursor: event_cursor,
                                    wait_ms: 0,
                                    limit: 64 * 1024,
                                }).await;
                                let Ok(page) = page else { break };
                                if page.events.is_empty() {
                                    break;
                                }
                                for event in page.events {
                                        this.handle_event(event).await;
                                }
                                event_cursor = page.next_cursor;
                            }
                            this.handle_shell_exit(generation, end).await;
                            break;
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
                if inner.shell.is_none() {
                    return Err("shell exited during initialization".into());
                }
            }
            if tokio::time::Instant::now() >= deadline {
                let mut inner = self.inner.lock().await;
                let task_id = inner.shell.take();
                let captured = inner.retained.clone();
                inner.had_reset = true;
                drop(inner);
                if let Some(task_id) = task_id {
                    self.stop_shell(&task_id).await;
                }
                return Err(format!(
                    "shell did not initialize; captured output: {captured}"
                ));
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// Writes a wrapped command and starts its owner-side capture.
    async fn start_command(
        &self,
        command_id: &str,
        start_marker: &str,
        end_marker: &str,
        wrapped: &str,
    ) -> Result<(), HostError> {
        let inner = self.inner.lock().await;
        let Some(task_id) = inner.shell.clone() else {
            return Err(HostError::failed("Shell is not running"));
        };
        let line = format!("{wrapped}\n");
        self.host
            .write(ShellWrite {
                task_id,
                data: STANDARD.encode(line.as_bytes()),
                command: Some(ShellCommandStart {
                    task_id: command_id.to_owned(),
                    start_marker: start_marker.to_owned(),
                    end_marker: end_marker.to_owned(),
                }),
                job: None,
            })
            .await
            .map_err(|e| HostError::failed(format!("Shell write failed: {e}")))?;
        Ok(())
    }

    /// Consumes PTY output: folds it into the retained buffer and strips a
    /// registered job's end marker. Terminal facts come from the host watcher.
    async fn handle_event(self: &Arc<Self>, event: ShellEvent) {
        match event {
            ShellEvent::Output { text, .. } => self.handle_output(&text).await,
            ShellEvent::Marker { line, .. } => self.handle_output(&line).await,
        }
    }

    async fn handle_output(self: &Arc<Self>, text: &str) {
        let mut inner = self.inner.lock().await;
        inner.retained.push_str(text);
        let markers: Vec<String> = inner
            .background
            .values()
            .filter_map(|task| self.host.job_marker(&task.task_id))
            .collect();
        for marker in markers {
            strip_marker(&mut inner.retained, &marker);
        }
        if inner.retained.len() > RETAINED_BUFFER_TRIM {
            inner.retained = tail_bytes(&inner.retained, RETAINED_BUFFER_LIMIT).to_owned();
        }
    }

    /// The shell of `generation` died: note the exit, settle each child from
    /// the owner terminal when it is available (falling back to the reset
    /// result only when the owner has no terminal), and mark the reset.
    async fn handle_shell_exit(self: &Arc<Self>, generation: u64, end: Option<WaitReply>) {
        let mut settled: Vec<Settlement> = Vec::new();
        let mut orphans: Vec<(String, PathBuf)> = Vec::new();
        let mut cleanup_unconfirmed = false;
        #[allow(
            unused_assignments,
            reason = "the early generation return leaves the initial value unread"
        )]
        let mut shell_task = None;
        {
            let mut inner = self.inner.lock().await;
            if inner.generation != generation {
                return;
            }
            let task_id = inner.shell.take();
            shell_task = task_id.clone();
            if task_id.is_some() {
                let note = match end {
                    Some(reply) => match (reply.signal.as_deref(), reply.exit_code) {
                        (Some(signal), _) => format!("[shell killed by signal: {signal}]"),
                        (_, Some(code)) => format!("[shell exited: code {code}]"),
                        _ => "[shell exited]".to_owned(),
                    },
                    None => "[shell exited]".to_owned(),
                };
                inner.shell_exit = Some(note);
            }
            inner.had_reset = true;
            if inner.dialect == ShellDialect::Posix {
                orphans = inner
                    .background
                    .iter()
                    .map(|(id, task)| (id.clone(), task.output_path.clone()))
                    .collect();
            }
        }
        for (task_id, output_path) in orphans {
            let killed = self
                .host
                .kill(TaskRequest {
                    task_id: task_id.clone(),
                })
                .await
                .map_err(|error| std::io::Error::other(error.to_string()));
            let reply = self
                .host
                .wait(
                    WaitRequest {
                        task_id: task_id.clone(),
                        timeout_ms: Some(2_000),
                    },
                    never_cancelled(),
                )
                .await
                .ok();
            if reply
                .as_ref()
                .is_none_or(|reply| reply.status == WaitStatus::Running)
            {
                cleanup_unconfirmed = true;
                continue;
            }
            let removed = self
                .inner
                .lock()
                .await
                .background
                .remove(&task_id)
                .is_some();
            if !removed {
                continue;
            }
            let reply = reply.expect("checked above");
            if killed.is_ok() {
                let _ = self
                    .host
                    .settle(TaskRequest {
                        task_id: task_id.clone(),
                    })
                    .await;
            }
            let mut data = ended_from_reply(&task_id, &reply, &output_path);
            if let Err(error) = killed {
                cleanup_unconfirmed = true;
                if let ToolContent::Text(text) = &mut data.result.content {
                    text.push_str(&format!("\nJob could not be terminated: {error}"));
                }
                if let Some(fields) = data.result.structured_content.as_mut() {
                    fields.insert("code".into(), "exec_terminate_failed".into());
                }
            }
            settled.push(Settlement { data, ended: None });
        }
        if let Some(task_id) = shell_task {
            if cleanup_unconfirmed {
                let _ = self
                    .host
                    .kill(TaskRequest {
                        task_id: task_id.clone(),
                    })
                    .await;
            } else {
                self.stop_shell(&task_id).await;
            }
        }
        for settlement in settled {
            self.settle(settlement.data);
            if let Some(notify) = settlement.ended {
                notify.notify_waiters();
            }
        }
    }

    fn settle(&self, data: ExecEndedData) {
        let _ = self.timeline.append(EventKind::ExecEnded(data).into());
    }

    /// One foreground call: start its owner-side capture, wait for the
    /// terminal record, and convert a timeout into an occupying task.
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
        let command_id = format!("exec-{}", uuid::Uuid::new_v4().simple());
        let output_path = self.config.output_dir.join(format!("{command_id}.log"));
        self.register_output(&output_path).await?;
        self.start_command(&command_id, &start_marker, &end_marker, &wrapped)
            .await
            .map_err(|e| e.to_string())?;
        let cancel_token = cancel.clone();
        let cancelled: crate::host::CancelledFn = Arc::new(move || {
            let cancel = cancel_token.clone();
            Box::pin(async move {
                cancel.cancelled().await;
                Ok(())
            })
        });
        match self
            .host
            .wait(
                WaitRequest {
                    task_id: command_id.clone(),
                    timeout_ms: Some(timeout_ms),
                },
                cancelled,
            )
            .await
        {
            Ok(reply) if reply.status != WaitStatus::Running => {
                if reply.status == WaitStatus::Failed
                    && reply.exit_code.is_none()
                    && reply.signal.is_none()
                {
                    let note = match reply.failure_note.clone() {
                        Some(note) => Some(note),
                        None => {
                            let shell_id = self.inner.lock().await.shell.clone();
                            let terminal = shell_id
                                .as_ref()
                                .and_then(|id| self.host.task(id))
                                .and_then(|record| record.terminal());
                            terminal
                                .map(|terminal| shell_exit_note(&terminal))
                                .or_else(|| {
                                    self.inner
                                        .try_lock()
                                        .ok()
                                        .and_then(|inner| inner.shell_exit.clone())
                                })
                        }
                    };
                    if let Some(note) = note {
                        let mut result = ToolResult::text(note);
                        result.is_error = Some(true);
                        let _ = self
                            .host
                            .settle(TaskRequest {
                                task_id: command_id.clone(),
                            })
                            .await;
                        return Ok(ran(result, command, None));
                    }
                }
                let text = self.command_result_text(&command_id, &reply).await;
                let mut result = ToolResult::text(text);
                result.is_error = (reply.exit_code != Some(0)).then_some(true);
                let _ = self
                    .host
                    .settle(TaskRequest {
                        task_id: command_id.clone(),
                    })
                    .await;
                Ok(ran(result, command, reply.exit_code))
            }
            Ok(_) => {
                if self.config.member {
                    self.interrupt(&command_id)
                        .await
                        .map(|result| ran(result, command, None))
                } else {
                    self.occupy_current(command, &command_id, &output_path, context)
                        .await
                        .map(|result| ran(result, command, None))
                }
            }
            Err(HostPlatformError::Cancelled) => self
                .interrupt(&command_id)
                .await
                .map(|result| ran(result, command, None)),
            Err(_) => {
                let note = self
                    .inner
                    .lock()
                    .await
                    .shell_exit
                    .clone()
                    .unwrap_or_else(|| "[shell exited]".into());
                let mut result = ToolResult::text(note);
                result.is_error = Some(true);
                let _ = self
                    .host
                    .settle(TaskRequest {
                        task_id: command_id.clone(),
                    })
                    .await;
                Ok(ran(result, command, None))
            }
        }
    }

    async fn command_result_text(&self, command_id: &str, reply: &WaitReply) -> String {
        let mut text = String::new();
        if let Ok(page) = self
            .host
            .output(OutputRequest {
                task_id: command_id.to_owned(),
                cursor: None,
                limit: Some(OUTPUT_LIMIT as u64),
            })
            .await
        {
            push_output(&mut text, &page.text);
        }
        if let Some(signal) = &reply.signal {
            text.push_str(&format!("[Command terminated by signal {signal}]"));
        } else {
            text.push_str(&exit_line(
                &reply
                    .exit_code
                    .map(|code| code.to_string())
                    .unwrap_or_else(|| "unknown".into()),
            ));
        }
        if reply.output_truncated {
            text.push_str(&format!(
                "\n[Output truncated: {} bytes omitted]",
                reply
                    .output_bytes
                    .saturating_sub((OUTPUT_LIMIT as u64).min(reply.output_bytes))
            ));
        }
        text
    }

    /// Turn stop and member timeout: Ctrl+C, bounded wait; without the
    /// terminal record the shell tree is terminated and the session resets.
    async fn interrupt(self: &Arc<Self>, command_id: &str) -> Result<ToolResult, String> {
        {
            let inner = self.inner.lock().await;
            if inner.shell.is_some() {
                drop(inner);
                let _ = self.write_shell(b"\x03").await;
            }
        }
        let deadline = tokio::time::Instant::now() + STOP_WAIT;
        let never = CancellationToken::new();
        let cancelled: crate::host::CancelledFn = Arc::new(move || {
            let never = never.clone();
            Box::pin(async move {
                never.cancelled().await;
                Ok(())
            })
        });
        loop {
            if self.inner.lock().await.shell.is_none() {
                let _ = self
                    .host
                    .settle(TaskRequest {
                        task_id: command_id.to_owned(),
                    })
                    .await;
                return Ok(ToolResult::error(
                    "[Command interrupted; the shell exited and will be reset]",
                ));
            }
            match self
                .host
                .wait(
                    WaitRequest {
                        task_id: command_id.to_owned(),
                        timeout_ms: Some(10),
                    },
                    cancelled.clone(),
                )
                .await
            {
                Ok(reply) if reply.status != WaitStatus::Running => {
                    let mut text = String::new();
                    if let Ok(page) = self
                        .host
                        .output(OutputRequest {
                            task_id: command_id.to_owned(),
                            cursor: None,
                            limit: Some(OUTPUT_LIMIT as u64),
                        })
                        .await
                    {
                        push_output(&mut text, &page.text);
                    }
                    text.push_str("[Command interrupted]");
                    let _ = self
                        .host
                        .settle(TaskRequest {
                            task_id: command_id.to_owned(),
                        })
                        .await;
                    return Ok(ToolResult::error(text));
                }
                Ok(_) => {}
                Err(_) => {}
            }
            if tokio::time::Instant::now() >= deadline {
                let mut inner = self.inner.lock().await;
                let task_id = inner.shell.take();
                inner.shell_exit = Some("[shell was terminated]".into());
                inner.had_reset = true;
                drop(inner);
                if let Some(task_id) = task_id {
                    self.stop_shell(&task_id).await;
                }
                let _ = self
                    .host
                    .settle(TaskRequest {
                        task_id: command_id.to_owned(),
                    })
                    .await;
                return Ok(ToolResult::error(
                    "[Command interrupted; the shell was terminated and reset]",
                ));
            }
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
    async fn occupy_current(
        self: &Arc<Self>,
        command: &str,
        command_id: &str,
        output_path: &Path,
        context: &CallContext,
    ) -> Result<ToolResult, String> {
        let description = describe(command);
        let never = CancellationToken::new();
        let cancelled: crate::host::CancelledFn = Arc::new(move || {
            let never = never.clone();
            Box::pin(async move {
                never.cancelled().await;
                Ok(())
            })
        });
        let reply = self
            .host
            .wait(
                WaitRequest {
                    task_id: command_id.to_owned(),
                    timeout_ms: Some(0),
                },
                cancelled,
            )
            .await
            .ok();
        let page = self
            .host
            .output(OutputRequest {
                task_id: command_id.to_owned(),
                cursor: None,
                limit: Some(OUTPUT_LIMIT as u64),
            })
            .await
            .ok();
        let preview = page
            .as_ref()
            .map(|page| page.text.clone())
            .unwrap_or_default();
        let omitted = reply.as_ref().is_some_and(|reply| reply.output_truncated)
            || page.as_ref().is_some_and(|page| page.truncated);
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
                        task_id: command_id.to_owned(),
                        description,
                        title: None,
                        output_path: output_path.to_string_lossy().into_owned(),
                    })
                    .into(),
                )
                .map_err(|error| error.to_string())?;
            let hold = self
                .session_lifecycle
                .as_ref()
                .and_then(|lifecycle| lifecycle.take_hold());
            inner.occupy_tail = None;
            inner.occupy_exit = None;
            inner.occupying = Some(OccupyingTask {
                task_id: command_id.to_owned(),
                output_path: output_path.to_path_buf(),
                delivered: preview.len(),
                ended: Arc::new(Notify::new()),
                _hold: hold,
            });
        }
        let ended = {
            let inner = self.inner.lock().await;
            inner
                .occupying
                .as_ref()
                .expect("just installed")
                .ended
                .clone()
        };
        let this = self.clone();
        let id = command_id.to_owned();
        let path = output_path.to_path_buf();
        let tasks = self.tasks.lock().await;
        tasks.spawn(async move {
            let reply = this
                .host
                .wait(
                    WaitRequest {
                        task_id: id.clone(),
                        timeout_ms: None,
                    },
                    never_cancelled(),
                )
                .await;
            let (result, status, preview, exit) = match reply {
                Ok(reply) => {
                    let code = wait_code(&reply);
                    (
                        ended_result(&id, &path, &reply.preview, &code),
                        exit_status(&code),
                        reply.preview.clone(),
                        exit_line(&code),
                    )
                }
                Err(_) => (
                    reset_result(&id, &path, ""),
                    ExecStatus::Failed,
                    String::new(),
                    "[Task failed: shell was reset]".to_owned(),
                ),
            };
            let published = {
                let mut inner = this.inner.lock().await;
                if inner
                    .occupying
                    .as_ref()
                    .is_some_and(|task| task.task_id == id)
                {
                    inner.occupy_tail = Some(preview);
                    inner.occupy_exit = Some(exit);
                    inner.occupying = None;
                    true
                } else {
                    false
                }
            };
            if !published {
                return;
            }
            this.settle(ExecEndedData {
                task_id: id.clone(),
                status,
                result,
            });
            let _ = this.host.settle(TaskRequest { task_id: id }).await;
            ended.notify_waiters();
        });
        let mut text = String::new();
        push_output(&mut text, &preview);
        if omitted {
            text.push_str("[Output truncated]\n");
        }
        text.push_str(&format!(
            "Command is still running and occupies the shell.\nTask ID: {command_id}\nOutput: {}",
            output_path.display()
        ));
        let mut result = ToolResult::text(text);
        result.structured_content = Some(structured(json!({
            "type": "started",
            "taskId": command_id,
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
        let output_path_text = output_path
            .to_str()
            .ok_or_else(|| "the job output path is not UTF-8".to_owned())?
            .to_owned();
        self.register_output(&output_path).await?;
        let completion = marker_pair().1;
        let pid_marker = marker_pair().1;
        let description = describe(command);
        let dialect = {
            let inner = self.inner.lock().await;
            inner.dialect
        };
        let (wrapped, pwsh_paths) = match dialect {
            ShellDialect::Posix => (
                posix_background(command, &output_path, &completion, &pid_marker),
                None,
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
                    Some((result_path, error_path)),
                )
            }
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
                        output_path: output_path_text.clone(),
                    })
                    .into(),
                )
                .map_err(|error| error.to_string())?;
            let hold = self
                .session_lifecycle
                .as_ref()
                .and_then(|lifecycle| lifecycle.take_hold());
            inner.background.insert(
                task_id.clone(),
                BackgroundTask {
                    task_id: task_id.clone(),
                    output_path: output_path.clone(),
                    _hold: hold,
                },
            );
        }
        if let Err(error) = self
            .write_line_job(
                &wrapped,
                ShellJobStart {
                    task_id: task_id.clone(),
                    end_marker: completion.full(),
                    output_path: output_path_text,
                },
            )
            .await
        {
            let task = self.inner.lock().await.background.remove(&task_id);
            if task.is_some() {
                self.settle(ExecEndedData {
                    task_id: task_id.clone(),
                    status: ExecStatus::Failed,
                    result: ToolResult::error(error.to_string()),
                });
            }
            return Err(error.to_string());
        }
        let error_path = pwsh_paths.as_ref().map(|(_, error)| error.clone());
        let this = self.clone();
        let host = self.host.clone();
        let watched = task_id.clone();
        let watched_output = output_path.clone();
        self.tasks.lock().await.spawn(async move {
            let reply = host
                .wait(
                    WaitRequest {
                        task_id: watched.clone(),
                        timeout_ms: None,
                    },
                    never_cancelled(),
                )
                .await;
            if let Ok(reply) = reply {
                if this
                    .inner
                    .lock()
                    .await
                    .background
                    .remove(&watched)
                    .is_none()
                {
                    return;
                }
                let _ = host
                    .settle(TaskRequest {
                        task_id: watched.clone(),
                    })
                    .await;
                let mut data = ended_from_reply(&watched, &reply, &watched_output);
                if let Some(error_path) = &error_path
                    && let ToolContent::Text(text) = &mut data.result.content
                {
                    text.push_str(&format!("\nStandard error: {}", error_path.display()));
                }
                this.settle(data);
            }
        });
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
    pub async fn stop(self: &Arc<Self>, task_id: &str) -> Result<ExecEndedData, TaskError> {
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
                let inner = self.inner.lock().await;
                if inner.shell.is_some() {
                    drop(inner);
                    let _ = self.write_shell(b"\x03").await;
                }
            }
            let never = CancellationToken::new();
            let cancelled: crate::host::CancelledFn = Arc::new(move || {
                let never = never.clone();
                Box::pin(async move {
                    never.cancelled().await;
                    Ok(())
                })
            });
            let reply = self
                .host
                .wait(
                    WaitRequest {
                        task_id: task_id.to_owned(),
                        timeout_ms: Some(STOP_WAIT.as_millis() as u64),
                    },
                    cancelled,
                )
                .await
                .ok();
            let found = reply
                .as_ref()
                .is_some_and(|reply| reply.status != WaitStatus::Running);
            let page = self
                .host
                .output(OutputRequest {
                    task_id: task_id.to_owned(),
                    cursor: None,
                    limit: Some(OUTPUT_LIMIT as u64),
                })
                .await
                .ok();
            let preview = page.map(|page| page.text).unwrap_or_default();
            {
                let mut inner = self.inner.lock().await;
                inner.occupy_tail = Some(preview);
                inner.occupy_exit = None;
                if !found {
                    let shell = inner.shell.take();
                    inner.shell_exit = Some("[shell was terminated]".into());
                    inner.had_reset = true;
                    drop(inner);
                    if let Some(shell) = shell {
                        self.stop_shell(&shell).await;
                    }
                }
            }
            let _ = self
                .host
                .settle(TaskRequest {
                    task_id: task_id.to_owned(),
                })
                .await;
            let data = ExecEndedData {
                task_id: task_id.to_owned(),
                status: ExecStatus::Cancelled,
                result: ToolResult::text("Command interrupted."),
            };
            self.settle(data.clone());
            occ.ended.notify_waiters();
            return Ok(data);
        }
        let task = {
            let mut inner = self.inner.lock().await;
            inner.background.remove(task_id)
        };
        if let Some(task) = task {
            let killed = self
                .host
                .kill(TaskRequest {
                    task_id: task.task_id.clone(),
                })
                .await;
            if matches!(killed, Err(HostPlatformError::TaskRunning)) {
                let task_id = task.task_id.clone();
                let output_path = task.output_path.clone();
                self.inner
                    .lock()
                    .await
                    .background
                    .insert(task_id.clone(), task);
                let this = self.clone();
                let host = self.host.clone();
                let watched = task_id.clone();
                self.tasks.lock().await.spawn(async move {
                    let reply = host
                        .wait(
                            WaitRequest {
                                task_id: watched.clone(),
                                timeout_ms: None,
                            },
                            never_cancelled(),
                        )
                        .await;
                    if let Ok(reply) = reply {
                        if this
                            .inner
                            .lock()
                            .await
                            .background
                            .remove(&watched)
                            .is_none()
                        {
                            return;
                        }
                        let _ = host
                            .settle(TaskRequest {
                                task_id: watched.clone(),
                            })
                            .await;
                        this.settle(ended_from_reply(&watched, &reply, &output_path));
                    }
                });
                return Ok(ExecEndedData {
                    task_id,
                    status: ExecStatus::Cancelled,
                    result: ToolResult::text(
                        "Background command stop requested; it will end after it starts.",
                    ),
                });
            }
            let kill_failed = killed.is_err();
            let data = match killed {
                Ok(()) => {
                    let reply = self
                        .host
                        .wait(
                            WaitRequest {
                                task_id: task.task_id.clone(),
                                timeout_ms: None,
                            },
                            never_cancelled(),
                        )
                        .await
                        .ok();
                    let _ = self
                        .host
                        .settle(TaskRequest {
                            task_id: task.task_id.clone(),
                        })
                        .await;
                    reply.map_or_else(
                        || ExecEndedData {
                            task_id: task_id.to_owned(),
                            status: ExecStatus::Cancelled,
                            result: ToolResult::text("Background command stopped."),
                        },
                        |reply| ended_from_reply(task_id, &reply, &task.output_path),
                    )
                }
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
            if kill_failed {
                let watched = task.task_id.clone();
                let output_path = task.output_path.clone();
                self.inner
                    .lock()
                    .await
                    .background
                    .insert(watched.clone(), task);
                let this = self.clone();
                let host = self.host.clone();
                self.tasks.lock().await.spawn(async move {
                    let reply = host
                        .wait(
                            WaitRequest {
                                task_id: watched.clone(),
                                timeout_ms: None,
                            },
                            never_cancelled(),
                        )
                        .await;
                    if let Ok(reply) = reply {
                        if this
                            .inner
                            .lock()
                            .await
                            .background
                            .remove(&watched)
                            .is_none()
                        {
                            return;
                        }
                        let _ = host
                            .settle(TaskRequest {
                                task_id: watched.clone(),
                            })
                            .await;
                        this.settle(ended_from_reply(&watched, &reply, &output_path));
                    }
                });
                return Ok(data);
            }
            self.settle(data.clone());
            return Ok(data);
        }
        Err(TaskError::Unknown)
    }

    async fn write_line(&self, line: &str) -> Result<(), HostError> {
        self.write_shell(format!("{line}\n").as_bytes()).await
    }

    async fn write_line_job(&self, line: &str, job: ShellJobStart) -> Result<(), HostError> {
        let inner = self.inner.lock().await;
        let Some(task_id) = inner.shell.clone() else {
            return Err(HostError::failed("Shell is not running"));
        };
        self.host
            .write(ShellWrite {
                task_id,
                data: STANDARD.encode(format!("{line}\n").as_bytes()),
                command: None,
                job: Some(job),
            })
            .await
            .map_err(|error| HostError::failed(format!("Shell write failed: {error}")))
    }
}

impl ExecStop for ShellSession {
    fn stop(
        self: Arc<Self>,
        task_id: String,
    ) -> BoxFuture<'static, Result<ExecEndedData, TaskError>> {
        Box::pin(async move {
            match ShellSession::stop(&self, &task_id).await {
                Err(TaskError::Unknown) => match &self.recovery {
                    Some(recovery) => recovery.stop(task_id).await.map_err(|error| match error {
                        crate::workspace_tools::background::TaskError::Closed => TaskError::Closed,
                        crate::workspace_tools::background::TaskError::Unknown => {
                            TaskError::Unknown
                        }
                        crate::workspace_tools::background::TaskError::Read(_) => TaskError::Read,
                    }),
                    None => Err(TaskError::Unknown),
                },
                other => other,
            }
        })
    }
}

/// Settles the occupying task once its capture saw a complete end marker
/// line, taking the capture with it.
/// The plugin.
pub struct ShellSessionPlugin {
    recovery: Key<BackgroundExec>,
    facts: Key<crate::file_access::FileAccessFacts>,
    session: Key<ShellSession>,
    stop: Key<dyn ExecStop>,
    timeline: Key<Timeline>,
    dispatcher: Key<Dispatcher>,
    host: Key<HostPlatform>,
    lifecycle: Key<dim_agent_runtime::BranchLifecycle>,
    session_config: Key<dim_agent_runtime::SessionConfig>,
    session_lifecycle: Key<SessionLifecycle>,
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
            recovery: names.key()?,
            facts: names.key()?,
            session: names.key()?,
            stop: names.key()?,
            timeline: names.key()?,
            dispatcher: names.key()?,
            host: names.key()?,
            lifecycle: names.key()?,
            session_config: names.key()?,
            session_lifecycle: names.key()?,
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
            self.recovery.erase(),
            self.timeline.erase(),
            self.dispatcher.erase(),
            self.host.erase(),
            self.lifecycle.erase(),
            self.session_config.erase(),
            self.session_lifecycle.erase(),
            self.identity.erase(),
        ]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let timeline = ctx.require_at(&self.timeline)?;
            let dispatcher = ctx.require_at(&self.dispatcher)?;
            let host = ctx.require_at(&self.host)?;
            let recovery = ctx.require_at(&self.recovery)?;
            let config = ctx.require_at(&self.session_config)?;
            let session_lifecycle = ctx.require_at(&self.session_lifecycle)?;
            let identity = ctx.require_at(&self.identity)?;
            let owner = ctx.get::<SessionFiles>();
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
            session.recovery = Some(recovery);
            session.session_lifecycle = Some(session_lifecycle);
            let session = Arc::new(session);
            let service = session.clone();
            ctx.effect(move || async move {
                service.lifecycle.cancel();
                {
                    let mut inner = service.inner.lock().await;
                    let task_id = inner.shell.take();
                    for queued in inner.queue.drain(..) {
                        let _ = queued.done.send(Err("shell session closed".into()));
                    }
                    drop(inner);
                    if let Some(task_id) = task_id {
                        service.stop_shell(&task_id).await;
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

fn exit_status(code: &str) -> ExecStatus {
    match code {
        "0" => ExecStatus::Completed,
        "cancelled" => ExecStatus::Cancelled,
        _ => ExecStatus::Failed,
    }
}

fn wait_code(reply: &WaitReply) -> String {
    if reply.status == WaitStatus::Cancelled {
        "cancelled".to_owned()
    } else {
        reply
            .exit_code
            .map(|code| code.to_string())
            .or_else(|| reply.signal.clone())
            .unwrap_or_else(|| "failed".to_owned())
    }
}

fn never_cancelled() -> crate::host::CancelledFn {
    let never = CancellationToken::new();
    std::sync::Arc::new(move || {
        let never = never.clone();
        Box::pin(async move {
            never.cancelled().await;
            Ok(())
        })
    })
}

fn exit_line(code: &str) -> String {
    match code {
        "failed" => "[Command failed]".to_owned(),
        "cancelled" => "[Command cancelled]".to_owned(),
        "unknown" => "[Command finished with unknown exit code]".to_owned(),
        code => format!("[Command finished with exit code {code}]"),
    }
}

fn ended_from_reply(task_id: &str, reply: &WaitReply, output_path: &Path) -> ExecEndedData {
    let code = wait_code(reply);
    let mut result = if matches!(
        reply.failure,
        Some(
            crate::host_platform::TaskFailure::ShellReset
                | crate::host_platform::TaskFailure::ShellExited
        )
    ) {
        reset_result(task_id, output_path, &reply.preview)
    } else {
        ended_result(task_id, output_path, &reply.preview, &code)
    };
    if let Some(failure) = &reply.failure
        && let ToolContent::Text(text) = &mut result.content
    {
        text.push('\n');
        text.push_str(&failure.to_string());
    }
    let status = match reply.status {
        WaitStatus::Exited if reply.exit_code == Some(0) && reply.failure.is_none() => {
            ExecStatus::Completed
        }
        WaitStatus::Cancelled => ExecStatus::Cancelled,
        _ => ExecStatus::Failed,
    };
    ExecEndedData {
        task_id: task_id.to_owned(),
        status,
        result,
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

fn shell_exit_note(terminal: &crate::host_platform::Terminal) -> String {
    match (terminal.signal.as_deref(), terminal.exit_code) {
        (Some(signal), _) => format!("[shell killed by signal: {signal}]"),
        (_, Some(code)) => format!("[shell exited: code {code}]"),
        _ => "[shell exited]".to_owned(),
    }
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

pub(crate) fn regularize_output(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let mut out = String::new();
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            match chars.peek().copied() {
                Some('[') => {
                    chars.next();
                    for next in chars.by_ref() {
                        if next.is_ascii_alphabetic() {
                            break;
                        }
                    }
                }
                Some(']') => {
                    chars.next();
                    for next in chars.by_ref() {
                        if next == '\x07' {
                            break;
                        }
                    }
                }
                Some('(' | ')' | '*' | '+' | '-' | '.' | '/') => {
                    chars.next();
                    chars.next();
                }
                _ => {}
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
    let task_id = output_path
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    format!(
        "{{ {{ eval $'{}'; }} > '{}' 2>&1; printf '%s%s%d\\n' '{}' '{}' \"$?\"; }} & disown\nprintf '%s%s%d %s\\n' '{}' '{}' \"$!\" '{}'",
        posix_escape(command),
        output_path.display(),
        completion.first(),
        completion.second(),
        pid.first(),
        pid.second(),
        task_id,
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
    let task_id = output_path
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
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
    out.push_str("' + $__dim_p.Id + ' ' + '");
    out.push_str(task_id);
    out.push_str("')");
    out
}

fn base64_encode(data: &str) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(data.as_bytes())
}

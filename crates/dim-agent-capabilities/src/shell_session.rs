//! Persistent shell execution capability: one long-lived PTY shell per branch.
//!
//! Every command runs in the same interactive shell, wrapped with runtime
//! marker pairs: the command text carries the fragments only, so echoes cannot
//! forge a full marker. HostPlatform owns the PTY, reads it, parses the markers
//! and records each command's and job's terminal. This part owns the queue and a
//! foreground timeout moves the running command to BackgroundExec; explicit
//! background jobs go there too. BackgroundExec publishes every shell task's
//! terminal, its completion notice and its recovery.

use crate::workspace_tools::background::{BackgroundExec, StartError, StartRequest, WaitError};
use crate::{
    host::{HostError, SessionFiles},
    host_platform::{
        HostPlatform, HostPlatformError, OutputRequest, ShellCommandStart, ShellEdition,
        ShellJobStart, ShellRequest, ShellStatus, ShellWrite, TaskRequest, WaitReply, WaitRequest,
        WaitStatus,
    },
};
use base64::{Engine, engine::general_purpose::STANDARD};
use dim_agent_runtime::Names;
use dim_agent_types::{ExecEndedData, JsonObject, ToolContent, ToolEffect, ToolResult};
use rutis::Ctx;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, Key, TypeKey};
use serde_json::json;
use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::sync::oneshot;
use tokio_util::{sync::CancellationToken, task::TaskTracker};

const OUTPUT_LIMIT: usize = 16_000;
const MAX_TIMEOUT_MS: u64 = 3_600_000;
const DESCRIPTION_CHARS: usize = 200;
const INIT_TIMEOUT: Duration = Duration::from_secs(10);
/// How long one init wrapper may take before another copy is written: a
/// startup consumer can eat the first one.
const INIT_RETRY: Duration = Duration::from_secs(1);
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

/// The foreground call that outlived its timeout; the shell stays occupied
/// until its terminal record arrives.
struct OccupyingTask {
    /// The branch-visible id BackgroundExec answers.
    task_id: String,
    /// The owner's task id the command's output is read by.
    host_task_id: String,
    output_path: PathBuf,
    delivered: usize,
}

struct Inner {
    shell: Option<String>,
    /// Counts spawned shells; an exit waiter serves only its own shell.
    generation: u64,
    dialect: ShellDialect,
    shell_exe: Option<PathBuf>,
    queue: VecDeque<QueuedCommand>,
    queue_running: bool,
    occupying: Option<OccupyingTask>,
    /// Note for the call that was running when the shell exited.
    shell_exit: Option<String>,
    /// The next call after a shell death reports the reset.
    had_reset: bool,
}

pub struct ShellSession {
    facts: Option<(Ctx, Key<crate::file_access::FileAccessFacts>)>,
    inner: tokio::sync::Mutex<Inner>,
    config: ShellSessionConfig,
    lifecycle: CancellationToken,
    host: Arc<HostPlatform>,
    /// SessionStore's output ownership (ADR 013); absent in hosts without
    /// native storage, where shell output has no owner.
    owner: Option<Arc<SessionFiles>>,
    session_id: String,
    /// Owns every shell task's terminal, notice and recovery.
    background: Arc<BackgroundExec>,
    tasks: Arc<tokio::sync::Mutex<TaskTracker>>,
}

impl ShellSession {
    /// The branch must mount BackgroundExec: it owns every shell task's
    /// terminal, completion notice and recovery.
    pub fn new(
        config: ShellSessionConfig,
        host: Arc<HostPlatform>,
        owner: Option<Arc<SessionFiles>>,
        session_id: String,
        background: Arc<BackgroundExec>,
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
                occupying: None,
                shell_exit: None,
                had_reset: false,
            }),
            config,
            lifecycle: CancellationToken::new(),
            host,
            owner,
            session_id,
            background,
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

    /// While the shell is occupied: wait for the occupying task's terminal,
    /// then report its output; the caller's command was never sent. A task the
    /// owner already recorded as terminal no longer occupies the shell, so the
    /// caller's command runs.
    async fn occupied_wait(
        self: &Arc<Self>,
        queued: &QueuedCommand,
    ) -> Option<Result<ToolResult, String>> {
        let (task_id, host_task_id, output_path, delivered) = {
            let inner = self.inner.lock().await;
            let task = inner.occupying.as_ref()?;
            (
                task.task_id.clone(),
                task.host_task_id.clone(),
                task.output_path.clone(),
                task.delivered,
            )
        };
        match self.background.wait(task_id.clone(), 0).await {
            Ok(_) => {
                self.clear_occupancy(&task_id).await;
                return None;
            }
            Err(WaitError::Timeout) => {}
            Err(error) => return Some(Err(occupancy_error(&task_id, error))),
        }
        let wait = self
            .background
            .wait(task_id.clone(), queued.timeout_ms.min(86_400_000));
        tokio::pin!(wait);
        let outcome = tokio::select! {
            _ = queued.cancel.cancelled() => {
                return Some(Err("command cancelled while waiting for the shell".into()));
            }
            _ = self.lifecycle.cancelled() => {
                return Some(Err("shell session closed while waiting for the shell".into()));
            }
            outcome = &mut wait => outcome,
        };
        match outcome {
            Ok(ended) => {
                self.clear_occupancy(&task_id).await;
                let mut text = ended_text(&ended);
                if !text.is_empty() && !text.ends_with('\n') {
                    text.push('\n');
                }
                text.push_str(
                    "The command was not executed: the shell was occupied. Resend it now.",
                );
                Some(Ok(ToolResult::text(text)))
            }
            Err(WaitError::Timeout) => {
                let page = self
                    .host
                    .output(OutputRequest {
                        task_id: host_task_id,
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
                        task.delivered = page
                            .next_cursor
                            .unwrap_or(task.delivered as u64 + page.bytes)
                            as usize;
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
            Err(error) => Some(Err(occupancy_error(&task_id, error))),
        }
    }

    /// Drops the occupancy mark of a task whose terminal the owner recorded.
    async fn clear_occupancy(&self, task_id: &str) {
        let mut inner = self.inner.lock().await;
        if inner
            .occupying
            .as_ref()
            .is_some_and(|task| task.task_id == task_id)
        {
            inner.occupying = None;
        }
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
            let alive = inner.shell.as_ref().is_some_and(|id| {
                self.host
                    .task(id)
                    .is_some_and(|record| !record.is_terminal())
            });
            if alive {
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
            inner.shell = Some(task_id.clone());
            inner.generation
        };
        let this = self.clone();
        {
            let tasks = self.tasks.lock().await;
            tasks.spawn(async move {
                let end = this
                    .host
                    .wait(
                        WaitRequest {
                            task_id,
                            timeout_ms: None,
                        },
                        cancelled_fn(&this.lifecycle),
                    )
                    .await
                    .ok();
                this.handle_shell_exit(generation, end).await;
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

    /// Writes the init wrapper until one copy's start marker and then its end
    /// marker reach the shell's tail; a timeout terminates the shell and fails
    /// the call.
    ///
    /// A shell can spend its first input on a startup consumer — zsh's newuser
    /// wizard, for example — instead of the shell prompt, so the wrapper is
    /// written again until one copy runs. Each copy carries its own markers, and
    /// every copy is watched until the deadline, so an end marker from another
    /// copy cannot complete one and a slow copy still counts; the typed line
    /// carries the fragments separately, so its echo cannot satisfy the check.
    async fn send_initial(self: &Arc<Self>, command: &str) -> Result<(), String> {
        let dialect = {
            let inner = self.inner.lock().await;
            inner.dialect
        };
        let deadline = tokio::time::Instant::now() + INIT_TIMEOUT;
        let mut write_at = tokio::time::Instant::now();
        // Every written copy keeps its markers until the deadline: a copy
        // slower than the retry interval still completes the initialization.
        let mut copies: Vec<(String, String)> = Vec::new();
        loop {
            let task_id = {
                let inner = self.inner.lock().await;
                inner.shell.clone()
            };
            let Some(task_id) = task_id else {
                return Err("shell exited during initialization".into());
            };
            let tail = self
                .host
                .tail(TaskRequest {
                    task_id: task_id.clone(),
                })
                .await;
            if let Ok(tail) = &tail {
                for (start_marker, end_marker) in &copies {
                    if init_complete(&tail.text, start_marker, end_marker) {
                        return Ok(());
                    }
                }
            }
            let started = match &tail {
                Ok(tail) => copies.iter().any(|(start, _)| tail.text.contains(start)),
                Err(_) => false,
            };
            let now = tokio::time::Instant::now();
            // A started copy is left to finish: another would only queue behind it.
            if now >= write_at && !started {
                let (start, end) = marker_pair();
                let wrapped = match dialect {
                    ShellDialect::Posix => posix_command(command, &start, &end),
                    ShellDialect::Pwsh => pwsh_command(command, &start, &end),
                };
                self.write_shell(format!("{wrapped}\n").as_bytes())
                    .await
                    .map_err(|error| error.to_string())?;
                copies.push((start.full(), end.full()));
                write_at = now + INIT_RETRY;
            }
            if let Err(_) = &tail
                && self
                    .host
                    .task(&task_id)
                    .is_none_or(|record| record.is_terminal())
            {
                return Err("shell exited during initialization".into());
            }
            if tokio::time::Instant::now() >= deadline {
                let captured = tail.map(|tail| tail.text).unwrap_or_default();
                let taken = {
                    let mut inner = self.inner.lock().await;
                    inner.had_reset = true;
                    inner.shell.take()
                };
                if let Some(task_id) = taken {
                    self.stop_shell(&task_id).await;
                }
                return Err(format!(
                    "shell did not initialize; captured output: {captured}"
                ));
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
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

    /// The shell of `generation` died: note the exit, drop the occupying mark
    /// and release the owner's shell record. The owner has already swept every
    /// command and job child to its terminal, so BackgroundExec publishes their
    /// results and this part only waits for a new shell.
    async fn handle_shell_exit(self: &Arc<Self>, generation: u64, end: Option<WaitReply>) {
        let shell_task = {
            let mut inner = self.inner.lock().await;
            if inner.generation != generation {
                return;
            }
            let task_id = inner.shell.take();
            if task_id.is_some() {
                inner.shell_exit = Some(end.as_ref().map_or_else(
                    || "[shell exited]".to_owned(),
                    |reply| shell_exit_note(reply.signal.as_deref(), reply.exit_code),
                ));
            }
            inner.had_reset = true;
            inner.occupying = None;
            task_id
        };
        if let Some(task_id) = shell_task {
            self.stop_shell(&task_id).await;
        }
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
                            let (shell_id, shell_exit) = {
                                let inner = self.inner.lock().await;
                                (inner.shell.clone(), inner.shell_exit.clone())
                            };
                            let terminal = shell_id
                                .as_ref()
                                .and_then(|id| self.host.task(id))
                                .and_then(|record| record.terminal());
                            terminal
                                .map(|terminal| {
                                    shell_exit_note(terminal.signal.as_deref(), terminal.exit_code)
                                })
                                .or(shell_exit)
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

    /// Hands the command that outlived its timeout to BackgroundExec and
    /// reports it. BackgroundExec registers the task, publishes its terminal
    /// and settles it; this part only keeps the occupancy for the queue.
    async fn occupy_current(
        self: &Arc<Self>,
        command: &str,
        command_id: &str,
        output_path: &Path,
        context: &CallContext,
    ) -> Result<ToolResult, String> {
        if self.lifecycle.is_cancelled() {
            return Err("shell session closed".into());
        }
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
        let omitted = page.as_ref().is_some_and(|page| page.truncated);
        let started = self
            .background
            .start(StartRequest {
                turn_id: context.turn_id.clone(),
                request_id: context.request_id.clone(),
                call_id: context.call_id.clone(),
                task_id: command_id.to_owned(),
                host_task_id: command_id.to_owned(),
                description: describe(command),
                title: None,
                output_path: output_path.to_string_lossy().into_owned(),
            })
            .await
            .map_err(|error| match error {
                StartError::Closing => "shell session closed".to_owned(),
                StartError::Timeline(error) => error.to_string(),
            })?;
        {
            let mut inner = self.inner.lock().await;
            inner.occupying = Some(OccupyingTask {
                task_id: started.clone(),
                host_task_id: command_id.to_owned(),
                output_path: output_path.to_path_buf(),
                delivered: preview.len(),
            });
        }
        let mut text = String::new();
        push_output(&mut text, &preview);
        if omitted {
            text.push_str("[Output truncated]\n");
        }
        text.push_str(&format!(
            "Command is still running and occupies the shell.\nTask ID: {started}\nOutput: {}",
            output_path.display()
        ));
        let mut result = ToolResult::text(text);
        result.structured_content = Some(structured(json!({
            "type": "started",
            "taskId": started,
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
        self.write_line_job(
            &wrapped,
            ShellJobStart {
                task_id: task_id.clone(),
                end_marker: completion.full(),
                output_path: output_path_text.clone(),
            },
        )
        .await
        .map_err(|error| error.to_string())?;
        let started = self
            .background
            .start(StartRequest {
                turn_id: context.turn_id.clone(),
                request_id: context.request_id.clone(),
                call_id: context.call_id.clone(),
                task_id: task_id.clone(),
                host_task_id: task_id,
                description,
                title: None,
                output_path: output_path_text,
            })
            .await
            .map_err(|error| match error {
                StartError::Closing => "shell session closed".to_owned(),
                StartError::Timeline(error) => error.to_string(),
            })?;
        let mut text = format!(
            "Background command started.\nTask ID: {started}\nOutput: {}",
            output_path.display()
        );
        if let Some((_, error_path)) = &pwsh_paths {
            text.push_str(&format!("\nStandard error: {}", error_path.display()));
        }
        let mut result = ToolResult::text(text);
        result.structured_content = Some(structured(json!({
            "type": "started",
            "taskId": started,
            "outputPath": output_path.to_string_lossy(),
        })));
        Ok(ran(result, command, None))
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

/// The plugin.
pub struct ShellSessionPlugin {
    background: Key<BackgroundExec>,
    facts: Key<crate::file_access::FileAccessFacts>,
    session: Key<ShellSession>,
    host: Key<HostPlatform>,
    session_config: Key<dim_agent_runtime::SessionConfig>,
    branch_lifecycle: Key<dim_agent_runtime::BranchLifecycle>,
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
            background: names.key()?,
            facts: names.key()?,
            session: names.key()?,
            host: names.key()?,
            lifecycle: names.key()?,
            session_config: names.key()?,
            branch_lifecycle: names.key()?,
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
            self.background.erase(),
            self.host.erase(),
            self.lifecycle.erase(),
            self.session_config.erase(),
            self.identity.erase(),
        ]
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let host = ctx.require_at(&self.host)?;
            let background = ctx.require_at(&self.background)?;
            let config = ctx.require_at(&self.session_config)?;
            let branch_lifecycle = ctx.require_at(&self.branch_lifecycle)?;
            let owner = ctx.get::<SessionFiles>();
            let mut session = ShellSession::new(
                self.config.clone().unwrap_or_else(|| ShellSessionConfig {
                    cwd: PathBuf::from(&config.cwd),
                    member: config.collaboration_role
                        == dim_agent_runtime::CollaborationRole::Member,
                    output_dir: host
                        .home
                        .join(".dim/exec")
                        .join(&branch_lifecycle.session_id)
                        .join(&branch_lifecycle.branch_id),
                }),
                host,
                owner,
                branch_lifecycle.session_id.clone(),
                background,
            );
            session.facts = Some((ctx.clone(), self.facts.clone()));
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
                Ok(())
            })?;
            ctx.provide_at(self.session.clone(), session)?;
            Ok(Effect::Done)
        })
    }
}

/// Whether `tail` holds `start` and `end` after it. An end marker a consumer
/// printed for an earlier copy, or by itself, does not complete a copy.
fn init_complete(tail: &str, start: &str, end: &str) -> bool {
    tail.find(start)
        .is_some_and(|at| tail[at + start.len()..].contains(end))
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

fn cancelled_fn(token: &CancellationToken) -> crate::host::CancelledFn {
    let token = token.clone();
    std::sync::Arc::new(move || {
        let token = token.clone();
        Box::pin(async move {
            token.cancelled().await;
            Ok(())
        })
    })
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

/// The model-readable text of a terminal record.
fn ended_text(data: &ExecEndedData) -> String {
    match &data.result.content {
        ToolContent::Text(text) => text.clone(),
        ToolContent::Blocks(blocks) => blocks
            .iter()
            .filter_map(|block| match block {
                dim_agent_types::ContentBlock::Text { text } => Some(text.as_str()),
                dim_agent_types::ContentBlock::Image { .. } => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

fn occupancy_error(task_id: &str, error: WaitError) -> String {
    match error {
        WaitError::Unknown => {
            format!("task {task_id} still occupies the shell but has no task facts")
        }
        WaitError::Closed => "shell session closed while waiting for the shell".to_owned(),
        WaitError::Timeout => format!("task {task_id} still occupies the shell"),
        WaitError::Read(error) => error.to_string(),
    }
}

fn shell_exit_note(signal: Option<&str>, exit_code: Option<i32>) -> String {
    match (signal, exit_code) {
        (Some(signal), _) => format!("[shell killed by signal: {signal}]"),
        (_, Some(code)) => format!("[shell exited: code {code}]"),
        _ => "[shell exited]".to_owned(),
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

#[cfg(test)]
mod tests {
    use super::init_complete;

    #[test]
    fn an_init_copy_completes_only_after_its_own_start_marker() {
        let start = "__DIM_START_aaaaaaaa_bbbbbbbb__";
        let end = "__DIM_END_cccccccc_dddddddd__";
        assert!(init_complete(
            &format!("noise\n{start}\ninit output\n{end}\n"),
            start,
            end
        ));
        assert!(!init_complete(&format!("{end}\n{start}\n"), start, end));
        assert!(!init_complete(&format!("echo {end}\n"), start, end));
        assert!(!init_complete(&format!("{start}\n"), start, end));
    }
}

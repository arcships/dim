//! One task table entry for every host process: the common state machine, the
//! terminal record and the per-kind payload. Payloads own their resources; the
//! record owns the one terminal watch.

use std::{
    sync::{Arc, Mutex},
    time::Instant,
};

use tokio::sync::watch;

use super::{TaskFailure, WaitReply, WaitStatus, shell};
use crate::workspace_tools::background::{OutputPage, PAGE_CHAR_TRAILING_BYTES, decode_page};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TaskStatus {
    Exited,
    Signaled,
    TimedOut,
    Cancelled,
    Failed,
}

impl TaskStatus {
    fn wait_status(self) -> WaitStatus {
        match self {
            Self::Exited => WaitStatus::Exited,
            Self::Signaled => WaitStatus::Signaled,
            Self::TimedOut => WaitStatus::TimedOut,
            Self::Cancelled => WaitStatus::Cancelled,
            Self::Failed => WaitStatus::Failed,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct OutputRecord {
    pub(crate) path: String,
    pub(crate) bytes: u64,
    pub(crate) truncated: bool,
    pub(crate) text: Option<String>,
    pub(crate) preview: String,
    pub(crate) preview_truncated: bool,
}

pub(crate) struct Terminal {
    pub(crate) status: TaskStatus,
    pub(crate) exit_code: Option<i32>,
    pub(crate) signal: Option<String>,
    pub(crate) duration_ms: u64,
    pub(crate) failure: Option<TaskFailure>,
    pub(crate) output: OutputRecord,
    pub(crate) note: Option<String>,
}

pub(crate) enum TaskState {
    Reserved,
    Running,
    Terminal(Arc<Terminal>),
}

pub(crate) enum TaskKind {
    Command(Arc<crate::workspace_tools::exec::CommandTask>),
    ShellCommand(Arc<shell::ShellCommand>),
    Job(Arc<shell::ShellJob>),
    Shell(Arc<shell::ShellTask>),
}

pub(crate) struct TaskRecord {
    pub(crate) id: String,
    pub(crate) started: Instant,
    pub(crate) state: Mutex<TaskState>,
    pub(crate) changed: watch::Sender<Option<Arc<Terminal>>>,
    pub(crate) kind: TaskKind,
}

impl TaskRecord {
    pub(crate) fn new(id: String, kind: TaskKind) -> Arc<Self> {
        Arc::new(Self {
            id,
            started: Instant::now(),
            state: Mutex::new(TaskState::Reserved),
            changed: watch::channel(None).0,
            kind,
        })
    }

    pub(crate) fn start(&self) -> bool {
        let mut state = self.state.lock().unwrap();
        if matches!(*state, TaskState::Reserved) {
            *state = TaskState::Running;
            true
        } else {
            false
        }
    }

    pub(crate) fn finish(&self, terminal: Terminal) {
        let mut state = self.state.lock().unwrap();
        if matches!(*state, TaskState::Terminal(_)) {
            return;
        }
        let terminal = Arc::new(terminal);
        *state = TaskState::Terminal(terminal.clone());
        drop(state);
        self.changed.send_replace(Some(terminal));
        self.after_terminal();
    }

    pub(crate) fn terminal(&self) -> Option<Arc<Terminal>> {
        match &*self.state.lock().unwrap() {
            TaskState::Terminal(terminal) => Some(terminal.clone()),
            _ => None,
        }
    }

    pub(crate) fn is_terminal(&self) -> bool {
        matches!(*self.state.lock().unwrap(), TaskState::Terminal(_))
    }

    pub(crate) fn is_running(&self) -> bool {
        matches!(*self.state.lock().unwrap(), TaskState::Running)
    }

    fn after_terminal(&self) {
        match &self.kind {
            TaskKind::ShellCommand(command) => command.shell.clear_active(self),
            TaskKind::Shell(shell) => shell.sweep(self.terminal()),
            TaskKind::Command(_) | TaskKind::Job(_) => {}
        }
    }

    pub(crate) fn terminal_page(&self, cursor: u64, limit: usize) -> Option<OutputPage> {
        let terminal = self.terminal()?;
        let text = terminal.output.text.as_ref()?;
        let bytes = text.as_bytes();
        let start = usize::try_from(cursor)
            .unwrap_or(usize::MAX)
            .min(bytes.len());
        let end = start
            .saturating_add(limit + PAGE_CHAR_TRAILING_BYTES)
            .min(bytes.len());
        let mut page =
            decode_page(&bytes[start..end], cursor, limit, terminal.output.truncated).ok()?;
        page.total_bytes = terminal.output.bytes;
        Some(page)
    }

    pub(crate) async fn output_snapshot(&self) -> OutputRecord {
        match &self.kind {
            TaskKind::Command(command) => command.output_record().await,
            TaskKind::ShellCommand(command) => command.output_record(),
            TaskKind::Job(job) => job.output_record().0,
            TaskKind::Shell(shell) => shell.output_record(),
        }
    }

    pub(crate) async fn wait(&self, timeout: Option<u64>) -> WaitReply {
        let mut changed = self.changed.subscribe();
        let deadline = timeout.map(|ms| Instant::now() + std::time::Duration::from_millis(ms));
        loop {
            let notified = changed.changed();
            if let Some(terminal) = self.terminal() {
                return self.reply(Some(terminal)).await;
            }
            let Some(deadline) = deadline else {
                let _ = notified.await;
                continue;
            };
            let now = Instant::now();
            if now >= deadline {
                return self.reply(None).await;
            }
            let _ = tokio::time::timeout(deadline.saturating_duration_since(now), notified).await;
        }
    }

    async fn reply(&self, terminal: Option<Arc<Terminal>>) -> WaitReply {
        let output = match &terminal {
            Some(terminal) => terminal.output.clone(),
            None => self.output_snapshot().await,
        };
        let (status, exit_code, signal, duration_ms, failure_note) = match &terminal {
            Some(terminal) => (
                terminal.status.wait_status(),
                terminal.exit_code,
                terminal.signal.clone(),
                terminal.duration_ms,
                terminal.note.clone(),
            ),
            None => (
                WaitStatus::Running,
                None,
                None,
                self.started.elapsed().as_millis() as u64,
                None,
            ),
        };
        WaitReply {
            status,
            text: output.text,
            exit_code,
            signal,
            duration_ms,
            output_path: output.path,
            output_bytes: output.bytes,
            output_truncated: output.truncated,
            preview: output.preview,
            preview_truncated: output.preview_truncated,
            failure: terminal.and_then(|terminal| terminal.failure.clone()),
            failure_note,
        }
    }
}

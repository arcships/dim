use super::{
    ExecShellRuntime, ExecHost, ProcessSpec, ProcessSpecError, PtyProcessTree, PtySize,
    ShellEdition, ShellStatus,
};
use crate::workspace_tools::{
    background::{OutputPageError, PAGE_CHAR_TRAILING_BYTES, decode_page},
    exec::{bounded_preview, tail_bytes},
};
use serde_json::{Value, json};
use std::{
    io::{self, Read},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::sync::{Mutex as AsyncMutex, watch};
use tokio_util::sync::CancellationToken;

const RETAINED: usize = 64 * 1024;

#[derive(Default)]
struct Capture {
    text: String,
    total: u64,
}

pub(crate) struct ShellTask {
    pty: AsyncMutex<Option<PtyProcessTree>>,
    capture: Mutex<Capture>,
    terminal: watch::Sender<Option<ShellExit>>,
    kill: CancellationToken,
    timeout: CancellationToken,
    path: PathBuf,
    started: Instant,
}

#[derive(Clone)]
pub(crate) struct ShellExit {
    pub(crate) exit: Option<portable_pty::ExitStatus>,
    status: &'static str,
    error: Option<String>,
    duration: u64,
}

impl ExecHost {
    pub(crate) async fn start_shell(
        &self,
        id: &str,
        cwd: &Path,
        roots: &[PathBuf],
        directory: &Path,
        size: PtySize,
        output: Option<tokio::sync::mpsc::UnboundedSender<String>>,
    ) -> Result<(Arc<ShellTask>, ExecShellRuntime), ProcessSpecError> {
        let runtime = match self.persistent_shell().await {
            ShellStatus::Available(shell) => shell,
            ShellStatus::Unavailable { code, reason } => {
                return Err(ProcessSpecError { code, reason });
            }
        };
        let args: &[&str] = match runtime.edition {
            ShellEdition::Pwsh | ShellEdition::WindowsPowershell => {
                &["-NoLogo", "-NoProfile", "-Command"]
            }
            _ => &["-i"],
        };
        let spec = ProcessSpec {
            executable: runtime.executable_path.clone(),
            args: args.iter().map(|s| (*s).into()).collect(),
            cwd: cwd.into(),
            env: self
                .env
                .iter()
                .cloned()
                .chain([
                    ("GIT_PAGER".into(), "cat".into()),
                    ("NO_COLOR".into(), "1".into()),
                    ("TERM".into(), "dumb".into()),
                    ("PAGER".into(), "cat".into()),
                ])
                .collect(),
        };
        let (spec, temporary, _) = self
            .prepare_isolated_process(spec, roots, Some(directory))
            .await?;
        let mut commands = self.commands.lock().unwrap();
        if self.command_shutdown.is_cancelled() {
            return Err(ProcessSpecError {
                code: "cancelled",
                reason: "Execution host closed".into(),
            });
        }
        if commands.contains_key(id) {
            return Err(ProcessSpecError {
                code: "task_exists",
                reason: "Execution task already exists".into(),
            });
        }
        let mut pty = self.spawn_pty(spec, size).map_err(|e| ProcessSpecError {
            code: "failed",
            reason: e.to_string(),
        })?;
        pty.hold(temporary);
        let mut reader = pty.take_reader();
        let shell = Arc::new(ShellTask {
            pty: AsyncMutex::new(Some(pty)),
            capture: Mutex::default(),
            terminal: watch::channel(None).0,
            kill: CancellationToken::new(),
            timeout: CancellationToken::new(),
            path: directory.join(format!("{id}.log")),
            started: Instant::now(),
        });
        commands.insert(id.into(), super::ExecutionTask::Shell(shell.clone()));
        let pump = Arc::downgrade(&shell);
        let (drained, reader_finished) = tokio::sync::oneshot::channel();
        std::thread::spawn(move || {
            let mut bytes = [0; 4096];
            while let Ok(n) = reader.read(&mut bytes) {
                if n == 0 {
                    break;
                }
                let Some(pump) = pump.upgrade() else {
                    break;
                };
                let chunk = crate::shell_session::regularize_output(&bytes[..n]);
                let mut capture = pump.capture.lock().unwrap();
                if pump.exit().is_some() {
                    break;
                }
                capture.total += chunk.len() as u64;
                capture.text.push_str(&chunk);
                if capture.text.len() > RETAINED {
                    let dropped = capture.text.len() - tail_bytes(&capture.text, RETAINED).len();
                    capture.text.drain(..dropped);
                }
                if let Some(sender) = &output {
                    let _ = sender.send(chunk);
                }
            }
            let _ = drained.send(());
        });
        let worker = shell.clone();
        let shutdown = self.command_shutdown.clone();
        self.command_workers.spawn(async move {
            let mut poll = tokio::time::interval(Duration::from_millis(100));
            let (exit, status, error) = loop {
                let stop = tokio::select! {
                    biased;
                    _=poll.tick()=>None,
                    _=worker.timeout.cancelled()=>Some("timed_out"),
                    _=worker.kill.cancelled()=>Some("cancelled"),
                    _=shutdown.cancelled()=>Some("cancelled"),
                };
                let mut guard = worker.pty.lock().await;
                let tree = guard.as_mut().expect("shell worker owns the process");
                if let Some(status) = stop {
                    match tree.terminate().await {
                        Ok(()) => break (None, status, None),
                        Err(e) => break (None, "failed", Some(e.to_string())),
                    }
                }
                match tree.try_wait() {
                    Ok(Some(exit)) => {
                        let status = if exit.signal().is_some() {
                            "signaled"
                        } else {
                            "exited"
                        };
                        break (Some(exit), status, None);
                    }
                    Ok(None) => {}
                    Err(e) => {
                        let _ = tree.terminate().await;
                        break (None, "failed", Some(e.to_string()));
                    }
                }
            };
            worker.pty.lock().await.take();
            let _ = tokio::time::timeout(
                crate::workspace_tools::exec::EXIT_STDIO_GRACE,
                reader_finished,
            )
            .await;
            let _capture = worker.capture.lock().unwrap();

            worker.terminal.send_replace(Some(ShellExit {
                exit,
                status,
                error,
                duration: worker.started.elapsed().as_millis() as u64,
            }));
        });
        Ok((shell, runtime))
    }
    pub(crate) fn shell_task(&self, id: &str) -> Option<Arc<ShellTask>> {
        match self.commands.lock().unwrap().get(id) {
            Some(super::ExecutionTask::Shell(shell)) => Some(shell.clone()),
            _ => None,
        }
    }
    pub(crate) fn settle_shell(&self, id: &str, shell: &Arc<ShellTask>) {
        let mut shells = self.commands.lock().unwrap();
        if matches!(shells.get(id),Some(super::ExecutionTask::Shell(s)) if Arc::ptr_eq(s,shell)) {
            shells.remove(id);
        }
    }
}

impl ShellTask {
    pub(crate) fn output_path(&self) -> &Path {
        &self.path
    }
    pub(crate) fn exit(&self) -> Option<ShellExit> {
        self.terminal.borrow().clone()
    }
    pub(crate) async fn done(&self) -> ShellExit {
        self.terminal
            .subscribe()
            .wait_for(Option::is_some)
            .await
            .expect("shell owner publishes completion")
            .as_ref()
            .unwrap()
            .clone()
    }
    pub(crate) async fn terminate(&self) -> io::Result<()> {
        self.kill.cancel();
        self.done()
            .await
            .error
            .map_or(Ok(()), |e| Err(io::Error::other(e)))
    }
    pub(crate) async fn write(&self, data: &[u8]) -> io::Result<()> {
        self.pty
            .lock()
            .await
            .as_mut()
            .ok_or_else(|| io::Error::other("Shell is not running"))?
            .write(data)
    }
    pub(crate) async fn job_pid(&self, pid: u32) -> io::Result<u32> {
        self.pty
            .lock()
            .await
            .as_ref()
            .ok_or_else(|| io::Error::other("Shell is not running"))?
            .job_pid(pid)
    }
    pub(crate) async fn resize(&self, size: PtySize) -> io::Result<()> {
        self.pty
            .lock()
            .await
            .as_ref()
            .ok_or_else(|| io::Error::other("Shell is not running"))?
            .resize(size)
    }
    pub(crate) fn page(&self, cursor: Option<u64>, limit: usize) -> Result<Value, OutputPageError> {
        let capture = self.capture.lock().unwrap();
        let first = capture.total - capture.text.len() as u64;
        let cursor = cursor.unwrap_or(first);
        if cursor < first {
            return Ok(
                json!({"text":"","bytes":0,"retainedFrom":first,"reset":true,"truncated":true}),
            );
        }
        let start = (cursor - first).min(capture.text.len() as u64) as usize;
        let end = (start + limit + PAGE_CHAR_TRAILING_BYTES).min(capture.text.len());
        let mut reply = json!(decode_page(
            &capture.text.as_bytes()[start..end],
            cursor,
            limit,
            first != 0
        )?);
        reply["retainedFrom"] = first.into();
        Ok(reply)
    }
    pub(crate) fn tail(&self) -> Value {
        let capture = self.capture.lock().unwrap();
        let (text, truncated) = bounded_preview(&capture.text, capture.total as usize);
        json!({"text":text,"totalBytes":capture.total,"truncated":truncated})
    }
    pub(crate) async fn wait(&self, ms: Option<u64>) -> Value {
        let end = if let Some(ms) = ms {
            tokio::select! {biased; end=self.done()=>end,_=tokio::time::sleep(Duration::from_millis(ms))=>{self.timeout.cancel(); self.done().await}}
        } else {
            self.done().await
        };
        let tail = self.tail();
        let capture = self.capture.lock().unwrap();
        let mut reply = json!({"status":end.status,"durationMs":end.duration,"outputPath":self.path,
            "outputBytes":capture.total,"outputTruncated":capture.total>capture.text.len() as u64,
            "preview":tail["text"],"previewTruncated":tail["truncated"]});
        if let Some(exit) = end.exit {
            if let Some(signal) = exit.signal() {
                reply["signal"] = signal.into();
            } else {
                reply["exitCode"] = exit.exit_code().into();
            }
        }
        reply
    }
}

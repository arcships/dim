//! Server-owned stdio only; client-local processes require a separate bridge.
use super::{DIAGNOSTIC_BYTES, McpServerError, Result};
use dim_agent_capabilities::host_platform::HostPlatform;
use futures_util::{SinkExt, StreamExt};
use rmcp::{
    RoleClient,
    service::{RxJsonRpcMessage, TxJsonRpcMessage},
    transport::{Transport, async_rw::JsonRpcMessageCodec},
};
use std::{
    collections::{BTreeMap, VecDeque},
    ffi::{OsStr, OsString},
    process::Stdio,
    sync::{Arc, Mutex as StdMutex},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
    sync::Mutex,
};
use tokio_util::{
    codec::{FramedRead, FramedWrite},
    sync::CancellationToken,
};

/// The last lines a server wrote to stderr, bounded so a chatty server cannot
/// grow it without limit. It is shown in server status only, never in a model
/// request.
#[derive(Default)]
struct StderrTail {
    lines: VecDeque<String>,
    bytes: usize,
}
const TAIL_LINES: usize = 20;
impl StderrTail {
    fn push(&mut self, line: &[u8]) {
        let line = String::from_utf8_lossy(line);
        let line = line.trim_end_matches('\r');
        if line.is_empty() {
            return;
        }
        let mut line = line.to_owned();
        if line.len() >= DIAGNOSTIC_BYTES {
            let mut cut = line.len() - (DIAGNOSTIC_BYTES - 1);
            while !line.is_char_boundary(cut) {
                cut += 1;
            }
            line.drain(..cut);
        }
        self.bytes += line.len() + 1;
        self.lines.push_back(line);
        while self.lines.len() > TAIL_LINES || self.bytes > DIAGNOSTIC_BYTES {
            let Some(front) = self.lines.pop_front() else {
                break;
            };
            self.bytes = self.bytes.saturating_sub(front.len() + 1);
        }
    }

    fn render(&self) -> String {
        self.lines.iter().cloned().collect::<Vec<_>>().join("\n")
    }
}

/// Drains stderr for the life of the pipe. It ends when the process closes
/// stderr or exits, so it never keeps the process alive by itself.
async fn drain_stderr(
    stderr: tokio::process::ChildStderr,
    tail: Arc<Mutex<StderrTail>>,
    drained: tokio::sync::oneshot::Sender<()>,
) {
    let mut reader = BufReader::new(stderr);
    let mut chunk = [0u8; 1024];
    let mut partial = Vec::new();
    loop {
        match reader.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(read) => {
                partial.extend_from_slice(&chunk[..read]);
                while let Some(end) = partial.iter().position(|byte| *byte == b'\n') {
                    let line: Vec<u8> = partial.drain(..=end).collect();
                    tail.lock().await.push(&line[..line.len() - 1]);
                }
                if partial.len() > DIAGNOSTIC_BYTES {
                    let cut = partial.len() - DIAGNOSTIC_BYTES;
                    partial.drain(..cut);
                }
            }
        }
    }
    if !partial.is_empty() {
        tail.lock().await.push(&partial);
    }
    let _ = drained.send(());
}

#[derive(Clone)]
pub(super) struct Config {
    command: String,
    args: Vec<String>,
    env: BTreeMap<String, String>,
    cwd: Option<String>,
}
impl Config {
    pub fn new(
        command: &str,
        args: &[String],
        env: &BTreeMap<String, String>,
        cwd: &Option<String>,
    ) -> Result<Self> {
        if command.trim().is_empty()
            || command.contains('\0')
            || args.iter().any(|s| s.contains('\0'))
            || env
                .iter()
                .any(|(k, v)| k.is_empty() || k.contains(['=', '\0']) || v.contains('\0'))
            || cwd
                .as_ref()
                .is_some_and(|s| s.is_empty() || s.contains('\0'))
        {
            return Err(McpServerError::Configuration(
                "invalid process configuration",
            ));
        }
        Ok(Self {
            command: command.into(),
            args: args.to_vec(),
            env: env.clone(),
            cwd: cwd.clone(),
        })
    }

    /// The child's `PATH`: the entry's own `PATH` if it sets one, otherwise the
    /// daemon's, with the downloaded runtimes' bin directories appended after
    /// it. `None` when there is nothing to append, so the child keeps the
    /// `PATH` it would have had. On Unix the program lookup reads the child's
    /// `PATH`. On Windows Rust resolves the program itself before
    /// `CreateProcessW`: with a `PATH` set on the command, a bare name is
    /// searched in the child's `PATH`, then the application's directory, the
    /// system and Windows directories, and the parent's `PATH`, with `.exe`
    /// appended when the name has no extension. A `.cmd` shim such as
    /// `npx.cmd` is found only by its full name.
    fn child_path(&self, platform: Option<&HostPlatform>) -> Option<OsString> {
        let platform = platform?;
        let base = self
            .env
            .iter()
            .find(|(key, _)| same_env_key(OsStr::new(key), "PATH"))
            .map(|(_, value)| OsString::from(value))
            .or_else(|| {
                platform
                    .env
                    .iter()
                    .rev()
                    .find(|(key, _)| same_env_key(key, "PATH"))
                    .map(|(_, value)| value.clone())
            })
            .unwrap_or_default();
        platform.child_path(&[], &base)
    }

    pub fn spawn(
        &self,
        failure: Arc<StdMutex<Option<String>>>,
        platform: Option<&HostPlatform>,
    ) -> Result<StdioTransport> {
        let mut command = Command::new(&self.command);
        command
            .args(&self.args)
            .envs(&self.env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if let Some(path) = self.child_path(platform) {
            command.env("PATH", path);
        }
        if let Some(cwd) = &self.cwd {
            command.current_dir(cwd);
        }
        let mut child = command
            .spawn()
            .map_err(|error| McpServerError::StartFailed(error.to_string()))?;
        let read = FramedRead::new(
            child.stdout.take().expect("piped stdout"),
            JsonRpcMessageCodec::new_with_max_length(4 * 1024 * 1024),
        );
        let write = FramedWrite::new(
            child.stdin.take().expect("piped stdin"),
            JsonRpcMessageCodec::new(),
        );
        let tail = Arc::new(Mutex::new(StderrTail::default()));
        let stderr = child.stderr.take().expect("piped stderr");
        let drained = tail.clone();
        let (done, drained_done) = tokio::sync::oneshot::channel();
        tokio::spawn(drain_stderr(stderr, drained, done));
        Ok(StdioTransport {
            child,
            read,
            write: Arc::new(Mutex::new(Some(write))),
            stopping: CancellationToken::new(),
            failure,
            tail,
            drained: Some(drained_done),
        })
    }
}

/// Whether an environment name is `name`; Windows environment names compare
/// case insensitively.
fn same_env_key(key: &OsStr, name: &str) -> bool {
    if cfg!(windows) {
        key.to_string_lossy().eq_ignore_ascii_case(name)
    } else {
        key == OsStr::new(name)
    }
}

type Writer = FramedWrite<ChildStdin, JsonRpcMessageCodec<TxJsonRpcMessage<RoleClient>>>;
pub(super) struct StdioTransport {
    child: Child,
    read: FramedRead<ChildStdout, JsonRpcMessageCodec<RxJsonRpcMessage<RoleClient>>>,
    write: Arc<Mutex<Option<Writer>>>,
    stopping: CancellationToken,
    failure: Arc<StdMutex<Option<String>>>,
    tail: Arc<Mutex<StderrTail>>,
    drained: Option<tokio::sync::oneshot::Receiver<()>>,
}
impl Transport<RoleClient> for StdioTransport {
    type Error = McpServerError;
    fn send(
        &mut self,
        item: TxJsonRpcMessage<RoleClient>,
    ) -> impl Future<Output = Result<()>> + Send + 'static {
        let write = self.write.clone();
        let stopping = self.stopping.clone();
        async move {
            tokio::select! {
                biased;
                _ = stopping.cancelled() => Err(McpServerError::Closed),
                result = async {
                    let mut write = write.lock().await;
                    write.as_mut().ok_or(McpServerError::Closed)?.send(item).await.map_err(|_| McpServerError::RequestFailed)
                } => result,
            }
        }
    }
    async fn receive(&mut self) -> Option<RxJsonRpcMessage<RoleClient>> {
        match self.read.next().await {
            Some(Ok(message)) => Some(message),
            Some(Err(_)) | None => {
                let status =
                    match tokio::time::timeout(Duration::from_millis(500), self.child.wait()).await
                    {
                        Ok(Ok(status)) => format!("MCP server exited ({status})"),
                        Ok(Err(_)) => "MCP server exited; status unavailable".into(),
                        Err(_) => "MCP server output closed while it is still running".into(),
                    };
                if let Some(drained) = self.drained.as_mut() {
                    let _ = tokio::time::timeout(Duration::from_millis(500), drained).await;
                    self.drained = None;
                }
                let tail = self.tail.lock().await.render();
                let message = if tail.is_empty() {
                    status
                } else {
                    let budget = DIAGNOSTIC_BYTES.saturating_sub(status.len() + 1);
                    let start = tail.ceil_char_boundary(tail.len().saturating_sub(budget));
                    format!("{status}\n{}", &tail[start..])
                };
                *self.failure.lock().unwrap() = Some(message);
                None
            }
        }
    }
    async fn close(&mut self) -> Result<()> {
        self.stopping.cancel();
        self.write.lock().await.take();
        match tokio::time::timeout(Duration::from_secs(3), self.child.wait()).await {
            Ok(result) => {
                result.map_err(|_| McpServerError::Cleanup)?;
            }
            Err(_) => self
                .child
                .kill()
                .await
                .map_err(|_| McpServerError::Cleanup)?,
        }
        Ok(())
    }
}

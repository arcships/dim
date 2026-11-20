use super::{
    ConfinedError, ConfinedSpec, Confinement, HostPlatform, Os, SandboxBackend, ShellStatus,
    SystemServices,
};
use crate::port_faces::{self, Empty};
use base64::{Engine, engine::general_purpose::STANDARD};
use rutis_bridge::session::{HostDispatch, Reply, Value};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::PathBuf, sync::Arc, time::Duration};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HostFacts<'a> {
    pub os: Os,
    pub arch: &'a str,
    pub home: &'a std::path::Path,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timezone: Option<&'a str>,
    pub platform_roots: &'a [PathBuf],
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HostInfo<'a> {
    #[serde(flatten)]
    pub facts: HostFacts<'a>,
    pub exec_root: PathBuf,
    pub backend: SandboxBackend,
    pub exec_shell: ShellStatus,
}

impl HostPlatform {
    pub(crate) async fn registered_output_page(
        &self,
        path: &std::path::Path,
        cursor: u64,
        limit: usize,
    ) -> Result<
        crate::workspace_tools::background::OutputPage,
        crate::workspace_tools::background::OutputPageError,
    > {
        use crate::workspace_tools::{
            background::{OutputPageError, PAGE_CHAR_TRAILING_BYTES, decode_page},
            exec::OUTPUT_FILE_BYTE_LIMIT,
        };
        use std::io::SeekFrom;
        use tokio::io::{AsyncReadExt, AsyncSeekExt};
        if let Some((files, directory)) = &self.command_files {
            if !path.starts_with(directory) {
                return Err(OutputPageError::Unavailable);
            }
            let cancel = CancellationToken::new();
            let _guard = cancel.clone().drop_guard();
            let reader = files
                .pin_read(path, &cancel)
                .await
                .map_err(|_| OutputPageError::Unavailable)?;
            let size = reader
                .len()
                .saturating_sub(cursor)
                .min((limit + PAGE_CHAR_TRAILING_BYTES) as u64) as usize;
            let mut bytes = Vec::with_capacity(size);
            while bytes.len() < size {
                let chunk = reader
                    .read_at(cursor + bytes.len() as u64, size - bytes.len(), &cancel)
                    .await
                    .map_err(|_| OutputPageError::Unavailable)?;
                if chunk.is_empty() {
                    break;
                }
                bytes.extend_from_slice(&chunk);
            }
            return decode_page(
                &bytes,
                cursor,
                limit,
                reader.len() >= crate::workspace_tools::exec::OUTPUT_FILE_BYTE_LIMIT,
            );
        }
        let mut file = tokio::fs::File::open(path)
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
        let truncated = file
            .metadata()
            .await
            .map(|m| m.len() >= OUTPUT_FILE_BYTE_LIMIT)
            .unwrap_or(false);
        decode_page(&buffer[..read], cursor, limit, truncated)
    }
    pub fn facts(&self) -> HostFacts<'_> {
        HostFacts {
            os: self.os,
            arch: &self.arch,
            home: &self.home,
            timezone: self.timezone.as_deref(),
            platform_roots: &self.platform_roots,
        }
    }

    pub async fn info(&self) -> HostInfo<'_> {
        HostInfo {
            facts: self.facts(),
            exec_root: self.exec_root(),
            backend: self.sandbox_backend(),
            exec_shell: self.exec_shell().await,
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ConfinedRequest {
    program: PathBuf,
    args: Vec<String>,
    read: Vec<PathBuf>,
    write: Vec<PathBuf>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    timeout_ms: u64,
    output_limit: OutputLimits,
    services: SystemServices,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OutputLimits {
    stdout: usize,
    stderr: usize,
}

async fn confined(
    owner: Arc<HostPlatform>,
    request: ConfinedRequest,
    cancel: CancellationToken,
) -> Reply {
    let spec = ConfinedSpec {
        executable: request.program,
        args: request.args.into_iter().map(Into::into).collect(),
        env: request
            .env
            .into_iter()
            .map(|(k, v)| (k.into(), v.into()))
            .collect(),
    };
    let confinement = Confinement {
        read: request.read,
        write: request.write,
        services: request.services,
        timeout: Duration::from_millis(request.timeout_ms),
        stdout_limit: request.output_limit.stdout,
        stderr_limit: request.output_limit.stderr,
    };
    let result = owner
        .spawn_confined(spec, confinement, cancel)
        .await
        .map_err(|error| {
            let code = match &error {
                ConfinedError::InvalidPath { .. } => "InvalidPath",
                ConfinedError::TimedOut => "TimedOut",
                ConfinedError::OutputLimit { .. } => "OutputLimit",
                ConfinedError::Cancelled => "Cancelled",
                ConfinedError::Spawn(_) => "Spawn",
                ConfinedError::Cleanup(_) => "Cleanup",
            };
            rutis_bridge::exports::failure("ExecHostError", code, error.to_string(), None)
        })?;
    Ok(serde_json::json!({
        "status": result.status.code(),
        "stdout": STANDARD.encode(result.stdout),
        "stderr": STANDARD.encode(result.stderr),
        "confined": result.confined,
        "stdoutTruncated": false,
        "stderrTruncated": false,
    })
    .into())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TaskRequest {
    #[serde(rename = "taskId")]
    task_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StartCommand {
    task_id: String,
    command: String,
    cwd: PathBuf,
    workspace: PathBuf,
    roots: Vec<PathBuf>,
    output_dir: PathBuf,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StartShell {
    task_id: String,
    cwd: PathBuf,
    workspace: PathBuf,
    roots: Vec<PathBuf>,
    output_dir: PathBuf,
    cols: u16,
    rows: u16,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ShellWrite {
    task_id: String,
    data: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ShellResize {
    task_id: String,
    cols: u16,
    rows: u16,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WaitCommand {
    task_id: String,
    timeout_ms: Option<u64>,
}

fn exec_error(code: &str, message: impl Into<String>) -> rutis_bridge::session::Error {
    rutis_bridge::exports::failure("ExecHostError", code, message.into(), None)
}

#[derive(Deserialize)]
#[serde(untagged, rename_all_fields = "camelCase", deny_unknown_fields)]
enum OutputRequest {
    Task {
        task_id: String,
        cursor: Option<u64>,
        limit: Option<u64>,
    },
    File {
        output_path: PathBuf,
        cursor: Option<u64>,
        limit: Option<u64>,
    },
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RemoveOutputDirectory {
    path: PathBuf,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ConfiguredRequest {
    command: String,
    cwd: PathBuf,
    #[serde(default)]
    env: BTreeMap<String, Option<String>>,
    stdin: Option<String>,
    timeout_ms: u64,
    limits: OutputLimits,
}

async fn configured(
    owner: Arc<HostPlatform>,
    request: ConfiguredRequest,
    cancel: CancellationToken,
) -> Reply {
    let stdin = STANDARD
        .decode(request.stdin.unwrap_or_default())
        .map_err(|error| port_faces::invalid("execHost", error))?;
    let request = super::ConfiguredCommand {
        command: request.command,
        cwd: request.cwd,
        env: request
            .env
            .into_iter()
            .map(|(k, v)| (k.into(), v.map(Into::into)))
            .collect(),
        stdin,
        timeout: Duration::from_millis(request.timeout_ms),
        stdout_limit: request.limits.stdout,
        stderr_limit: request.limits.stderr,
    };
    let started = std::time::Instant::now();
    let end = owner
        .configured(request, &cancel, Some(&cancel))
        .await
        .map_err(|error| {
            use super::ConfiguredError;
            let code = match &error {
                ConfiguredError::Prepare(error) => error.code,
                ConfiguredError::Spawn(_) => "spawn",
                ConfiguredError::Wait(_, _) => "wait",
                ConfiguredError::Unloaded => "cancelled",
            };
            rutis_bridge::exports::failure("ExecHostError", code, error.to_string(), None)
        })?;
    use super::ConfiguredEnd;
    let (status, exit, stdout, stderr, stdout_truncated, stderr_truncated) = match end {
        ConfiguredEnd::Exited {
            status,
            stdout,
            stderr,
            stdout_truncated,
            stderr_truncated,
        } => (
            if status.code().is_some() {
                "exited"
            } else {
                "signaled"
            },
            Some(status),
            stdout,
            stderr,
            stdout_truncated,
            stderr_truncated,
        ),
        ConfiguredEnd::Cancelled => ("cancelled", None, Vec::new(), Vec::new(), false, false),
        ConfiguredEnd::TimedOut => ("timed_out", None, Vec::new(), Vec::new(), false, false),
    };
    let mut reply = serde_json::json!({
        "status":status,"durationMs":started.elapsed().as_millis() as u64,
        "stdout":STANDARD.encode(stdout),"stderr":STANDARD.encode(stderr),
        "stdoutTruncated":stdout_truncated,"stderrTruncated":stderr_truncated,
    });
    if let Some(exit) = exit {
        if let Some(code) = exit.code() {
            reply["exitCode"] = code.into();
        }
        if let Some(signal) = super::signal_name(exit) {
            reply["signal"] = signal.into();
        }
    }
    Ok(reply.into())
}

pub(super) struct Dispatch {
    pub(super) owner: Arc<HostPlatform>,
    pub(super) stopping: CancellationToken,
    pub(super) calls: TaskTracker,
}

impl Dispatch {
    fn process<F>(
        &self,
        run: impl FnOnce(Arc<HostPlatform>, CancellationToken) -> F + Send + 'static,
    ) -> Reply
    where
        F: Future<Output = Reply> + Send + 'static,
    {
        let owner = self.owner.clone();
        let cancel = self.stopping.child_token();
        let calls = self.calls.clone();
        Ok(Value::cancellable_future(async move {
            let hold = calls.token();
            if cancel.is_cancelled() {
                return Err(rutis_bridge::exports::failure(
                    "ExecHostError",
                    "cancelled",
                    "execution host closed".into(),
                    None,
                ));
            }
            let _dropped = cancel.clone().drop_guard();
            tokio::spawn(async move {
                let _hold = hold;
                run(owner, cancel).await
            })
            .await
            .map_err(|error| {
                rutis_bridge::exports::failure("ExecHostError", "failed", error.to_string(), None)
            })?
        }))
    }
}

impl HostDispatch for Dispatch {
    fn methods(&self) -> Option<serde_json::Value> {
        Some(serde_json::json!({
            "shell":"async","write":"async","resize":"async","output":"async", "start":"async", "wait":"async", "kill":"async", "settle":"async",
            "info":"async", "confined":"async", "configured":"async",
            "removeOutputDirectory":"async", "retainOutput":"async", "tail":"async"
        }))
    }

    fn invoke(&self, method: &str, args: Value) -> Reply {
        if method == "output" {
            use crate::workspace_tools::background::{OUTPUT_PAGE_BYTES, OUTPUT_PAGE_BYTES_MAX};
            let request: OutputRequest = port_faces::request("execHost", args)?;
            let owner = self.owner.clone();
            return Ok(Value::cancellable_future(async move {
                if let OutputRequest::Task {
                    task_id,
                    cursor,
                    limit,
                } = &request
                    && let Some(shell) = owner.shell_task(task_id)
                {
                    let limit = limit
                        .unwrap_or(OUTPUT_PAGE_BYTES)
                        .min(OUTPUT_PAGE_BYTES_MAX) as usize;
                    if limit == 0 {
                        return Err(exec_error("invalid", "limit must be at least 1"));
                    }
                    return rutis_bridge::session::arg(
                        &shell
                            .page(*cursor, limit)
                            .map_err(|e| exec_error("output_unavailable", e.to_string()))?,
                    );
                }
                let (task, path, cursor, limit) = match request {
                    OutputRequest::Task {
                        task_id,
                        cursor,
                        limit,
                    } => (
                        Some(
                            owner
                                .command(&task_id)
                                .map_err(|e| exec_error("task_unknown", e.to_string()))?,
                        ),
                        None,
                        cursor,
                        limit,
                    ),
                    OutputRequest::File {
                        output_path,
                        cursor,
                        limit,
                    } => (None, Some(output_path), cursor, limit),
                };
                let cursor = cursor.unwrap_or(0);
                let limit = limit
                    .unwrap_or(OUTPUT_PAGE_BYTES)
                    .min(OUTPUT_PAGE_BYTES_MAX) as usize;
                if limit == 0 {
                    return Err(exec_error("invalid", "limit must be at least 1"));
                }
                let page = if let Some(task) = task {
                    task.page(cursor, limit)
                        .await
                        .map_err(|e| exec_error("output_unavailable", e.to_string()))?
                } else {
                    let path = path.expect("file request");
                    let (_, directory) = owner.command_files.as_ref().ok_or_else(|| {
                        exec_error(
                            "output_unavailable",
                            "Command output storage is unavailable",
                        )
                    })?;
                    if !path.starts_with(directory) {
                        return Err(exec_error(
                            "invalid",
                            "Path is outside command output storage",
                        ));
                    }
                    owner
                        .registered_output_page(&path, cursor, limit)
                        .await
                        .map_err(|e| exec_error("output_unavailable", e.to_string()))?
                };
                let mut reply = serde_json::json!(page);
                reply["retainedFrom"] = 0.into();
                rutis_bridge::session::arg(&reply)
            }));
        }
        if matches!(method, "write" | "resize") {
            enum Action {
                Write(Vec<u8>),
                Resize(super::PtySize),
            }
            let (id, action) = if method == "write" {
                let ShellWrite { task_id, data } = port_faces::request("execHost", args)?;
                (
                    task_id,
                    Action::Write(
                        STANDARD
                            .decode(data)
                            .map_err(|e| exec_error("invalid", e.to_string()))?,
                    ),
                )
            } else {
                let ShellResize {
                    task_id,
                    cols,
                    rows,
                } = port_faces::request("execHost", args)?;
                (task_id, Action::Resize(super::PtySize { cols, rows }))
            };
            let shell = self
                .owner
                .shell_task(&id)
                .ok_or_else(|| exec_error("task_unknown", "Unknown shell task"))?;
            return Ok(Value::future(async move {
                match action {
                    Action::Write(bytes) => shell.write(&bytes).await,
                    Action::Resize(size) => shell.resize(size).await,
                }
                .map_err(|e| exec_error("failed", e.to_string()))?;
                rutis_bridge::session::arg(&Empty {})
            }));
        }
        if matches!(method, "start" | "shell") {
            enum Launch {
                Command(String),
                Shell(super::PtySize),
            }
            let (task_id, cwd, workspace, mut roots, output_dir, launch) = if method == "shell" {
                let StartShell {
                    task_id,
                    cwd,
                    workspace,
                    roots,
                    output_dir,
                    cols,
                    rows,
                } = port_faces::request("execHost", args)?;
                (
                    task_id,
                    cwd,
                    workspace,
                    roots,
                    output_dir,
                    Launch::Shell(super::PtySize { cols, rows }),
                )
            } else {
                let StartCommand {
                    task_id,
                    cwd,
                    workspace,
                    roots,
                    output_dir,
                    command,
                } = port_faces::request("execHost", args)?;
                (
                    task_id,
                    cwd,
                    workspace,
                    roots,
                    output_dir,
                    Launch::Command(command),
                )
            };
            let owner = self.owner.clone();
            return Ok(Value::cancellable_future(async move {
                if task_id.is_empty()
                    || task_id.contains(['/', '\\', '\0', ':'])
                    || matches!(task_id.as_str(), "." | "..")
                {
                    return Err(exec_error(
                        "invalid",
                        "taskId must be a single filename component",
                    ));
                }
                if !cwd.is_absolute()
                    || !workspace.is_absolute()
                    || !output_dir.is_absolute()
                    || roots.iter().any(|root| !root.is_absolute())
                {
                    return Err(exec_error("invalid", "execution paths must be absolute"));
                }
                if !tokio::fs::metadata(&cwd)
                    .await
                    .is_ok_and(|metadata| metadata.is_dir())
                {
                    return Err(exec_error("cwd_not_found", "Working directory not found"));
                }
                if !roots.contains(&workspace) {
                    roots.push(workspace);
                }
                let path = match launch {
                    Launch::Command(command) => {
                        let (spec, _) = owner
                            .build_process_spec(&command, &cwd)
                            .await
                            .map_err(|e| exec_error(e.code, e.reason))?;
                        owner
                            .start_command(&task_id, spec, &roots, &output_dir, None)
                            .await
                            .map_err(|e| exec_error(e.code, e.reason))?
                            .retain_task()
                    }
                    Launch::Shell(size) => {
                        tokio::fs::create_dir_all(&output_dir)
                            .await
                            .map_err(|e| exec_error("io", e.to_string()))?;
                        owner
                            .start_shell(&task_id, &cwd, &roots, &output_dir, size, None)
                            .await
                            .map_err(|e| exec_error(e.code, e.reason))?
                            .0
                            .output_path()
                            .to_owned()
                    }
                };
                rutis_bridge::session::arg(&serde_json::json!({"outputPath":path}))
            }));
        }
        if method == "wait" {
            let WaitCommand {
                task_id,
                timeout_ms,
            } = port_faces::request("execHost", args)?;
            if let Some(shell) = self.owner.shell_task(&task_id) {
                return Ok(Value::cancellable_future(async move {
                    rutis_bridge::session::arg(&shell.wait(timeout_ms).await)
                }));
            }
            let task = self
                .owner
                .command(&task_id)
                .map_err(|e| exec_error("task_unknown", e.to_string()))?;
            return Ok(Value::cancellable_future(async move {
                rutis_bridge::session::arg(&task.wait(timeout_ms).await)
            }));
        }
        if matches!(method, "kill" | "settle") {
            let TaskRequest { task_id } = port_faces::request("execHost", args)?;
            let owner = self.owner.clone();
            if let Some(shell) = owner.shell_task(&task_id) {
                let kill = method == "kill";
                return Ok(Value::future(async move {
                    if kill {
                        shell
                            .terminate()
                            .await
                            .map_err(|e| exec_error("failed", e.to_string()))?;
                    } else if shell.exit().is_some() {
                        owner.settle_shell(&task_id, &shell);
                    } else {
                        return Err(exec_error("task_running", "Cannot settle a running shell"));
                    }
                    rutis_bridge::session::arg(&Empty {})
                }));
            }
            let task = owner
                .command(&task_id)
                .map_err(|e| exec_error("task_unknown", e.to_string()))?;
            let kill = method == "kill";
            return Ok(Value::future(async move {
                if kill {
                    task.terminate()
                        .await
                        .map_err(|e| exec_error("failed", e.to_string()))?;
                } else {
                    if !task.ended() {
                        return Err(exec_error(
                            "task_running",
                            "Cannot settle a running command",
                        ));
                    }
                    owner.settle_command(&task_id, &task);
                }
                rutis_bridge::session::arg(&Empty {})
            }));
        }
        if matches!(method, "retainOutput" | "tail") {
            let TaskRequest { task_id } = port_faces::request("execHost", args)?;
            let owner = self.owner.clone();
            if method == "tail"
                && let Some(shell) = owner.shell_task(&task_id)
            {
                return Ok(Value::future(async move {
                    rutis_bridge::session::arg(&shell.tail())
                }));
            }
            let retain = method == "retainOutput";
            return Ok(Value::future(async move {
                let result = if retain {
                    owner
                        .retain_output(&task_id)
                        .await
                        .map(|path| serde_json::json!({"outputPath": path}))
                } else {
                    owner
                        .tail(&task_id)
                        .await
                        .map(|tail| serde_json::json!(tail))
                };
                result.map(Into::into).map_err(|error| {
                    let code = match error {
                        crate::workspace_tools::exec::CaptureError::Unknown => "task_unknown",
                        crate::workspace_tools::exec::CaptureError::Unavailable(_) => {
                            "output_unavailable"
                        }
                    };
                    rutis_bridge::exports::failure("ExecHostError", code, error.to_string(), None)
                })
            }));
        }
        if method == "configured" {
            let request = port_faces::request("execHost", args)?;
            return self.process(move |owner, cancel| configured(owner, request, cancel));
        }
        if method == "removeOutputDirectory" {
            let RemoveOutputDirectory { path } = port_faces::request("execHost", args)?;
            let owner = self.owner.clone();
            return Ok(Value::future(async move {
                owner.remove_output_directory(path).await.map_err(|error| {
                    let code = if error.kind() == std::io::ErrorKind::InvalidInput {
                        "invalid"
                    } else {
                        "io"
                    };
                    rutis_bridge::exports::failure("ExecHostError", code, error.to_string(), None)
                })?;
                rutis_bridge::session::arg(&Empty {})
            }));
        }
        if method == "confined" {
            let request = port_faces::request("execHost", args)?;
            return self.process(move |owner, cancel| confined(owner, request, cancel));
        }
        if method != "info" {
            return Err(port_faces::unknown_method("execHost", method));
        }
        let Empty {} = port_faces::request("execHost", args).map_err(|error| {
            rutis_bridge::exports::failure("ExecHostError", "invalid", error.to_string(), None)
        })?;
        let owner = self.owner.clone();
        Ok(Value::cancellable_future(async move {
            rutis_bridge::session::arg(&owner.info().await)
        }))
    }
}

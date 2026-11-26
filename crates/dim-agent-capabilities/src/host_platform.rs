//! Process facts and the only entry point that starts child processes, see
//! `docs/architecture/rust-agent/plugins/HostPlatform.md`.
//!
//! Shell resolution copies ADR 020 and TS `packages/agent/src/exec-shell.ts`.
//! Process facts are captured at apply; exec shell preferences are read at use
//! time. Shell problems are returned as data, never raised during assembly.
use crate::execution_settings::ExecutionSettingsHost;
use crate::file_storage::{FileStorage, FileStorageError};
use crate::toolchain::{InstalledToolchain, ToolchainSourceHost};
use base64::{Engine, engine::general_purpose::STANDARD};
use dim_agent_runtime::Names;
use rutis::Ctx;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, ErrorKind, Key, TypeKey};
use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    io,
    path::{Component, Path, PathBuf},
    process::{ExitStatus, Stdio},
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::{Child, ChildStderr, ChildStdout},
    sync::{Mutex, watch},
};
use tokio_util::sync::CancellationToken;

mod confined;
mod exec_shell_settings;
#[cfg(unix)]
mod isolation;
pub(crate) mod output;
mod task;

pub(crate) use task::{OutputRecord, TaskKind, TaskRecord, TaskStatus, Terminal};

/// Step 1 keeps the old table name as an alias; step 3 removes it with the exec handoff.
#[allow(dead_code, reason = "step 1 compatibility name; removed in step 3")]
pub(crate) type ExecutionTask = TaskKind;
pub use confined::{
    ConfinedError, ConfinedOutput, ConfinedSpec, ConfinedStream, Confinement, SystemServices,
};
pub use exec_shell_settings::{ExecShellSettings, ExecShellSettingsError, ShellChoice};

pub const EXEC_SHELL_CONFIG_INVALID: &str = "EXEC_SHELL_CONFIG_INVALID";
pub const EXEC_SHELL_UNAVAILABLE: &str = "EXEC_SHELL_UNAVAILABLE";

#[cfg(windows)]
const WINDOWS_SANDBOX_UNAVAILABLE: &str =
    "The sandbox has no backend on Windows; select Off in /settings > Process isolation";

/// The declaration line for the tools that start processes; `None` where the
/// platform has no sandbox backend. It is static because declarations are read
/// once, while each process start reads the current switches. Only the macOS
/// default gives each process tree a writable TMPDIR.
pub(crate) const ISOLATION_SUMMARY: Option<&str> = if cfg!(target_os = "macos") {
    Some(
        "Isolation: the user selects Process isolation in /settings. When sandboxed, commands can write only inside the session workspace and $TMPDIR, read only there and in system directories, and have no network access unless Network allowed is selected. You cannot change the boundary; retrying or an approval does not lift it.",
    )
} else if cfg!(unix) {
    Some(
        "Isolation: the user selects Process isolation in /settings. When sandboxed, commands can write only inside the session workspace, read only there and in system directories, and have no network access unless Network allowed is selected. You cannot change the boundary; retrying or an approval does not lift it.",
    )
} else {
    None
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum Os {
    #[serde(rename = "darwin")]
    Macos,
    #[serde(rename = "linux")]
    Linux,
    #[serde(rename = "win32")]
    Windows,
}
impl Os {
    pub const CURRENT: Os = if cfg!(windows) {
        Os::Windows
    } else if cfg!(target_os = "macos") {
        Os::Macos
    } else {
        Os::Linux
    };
    /// Node's `process.platform`; texts copied from TS name the platform so.
    pub fn node_platform(self) -> &'static str {
        match self {
            Os::Macos => "darwin",
            Os::Linux => "linux",
            Os::Windows => "win32",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ShellEdition {
    Bash,
    Zsh,
    Sh,
    Dash,
    Ksh,
    GitBash,
    Pwsh,
    WindowsPowershell,
    Cmd,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellKind {
    Posix,
    Powershell,
    Cmd,
}
impl ShellEdition {
    pub fn kind(self) -> ShellKind {
        match self {
            Self::Pwsh | Self::WindowsPowershell => ShellKind::Powershell,
            Self::Cmd => ShellKind::Cmd,
            _ => ShellKind::Posix,
        }
    }
    /// ADR 020 `ExecShellEdition` value.
    pub fn name(self) -> &'static str {
        match self {
            Self::Bash => "bash",
            Self::Zsh => "zsh",
            Self::Sh => "sh",
            Self::Dash => "dash",
            Self::Ksh => "ksh",
            Self::GitBash => "git-bash",
            Self::Pwsh => "pwsh",
            Self::WindowsPowershell => "windows-powershell",
            Self::Cmd => "cmd",
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ShellSource {
    /// The executable path configured in `exec-shell.json`.
    Explicit,
    /// A supported login shell (macOS / Linux).
    Login,
    /// The POSIX fallback chain after an unsupported or missing login shell.
    Fallback,
    /// The Windows automatic order `pwsh -> Windows PowerShell -> cmd`.
    Automatic,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum OutputEncoding {
    Utf8,
    Unknown,
}
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecShellRuntime {
    #[serde(rename = "executable")]
    pub executable_path: String,
    pub edition: ShellEdition,
    pub source: ShellSource,
    pub output_encoding: OutputEncoding,
}
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum ShellStatus {
    Available(ExecShellRuntime),
    Unavailable { code: &'static str, reason: String },
}

/// ADR 014 D1's fixed system roots: the sandbox builder mounts them read-only
/// and file access treats them as read-only facts. macOS names `/private/etc`
/// and its `/etc` alias; Windows has no sandbox backend and no roots. A host
/// that builds its own file-tool boundary reads this one list rather than
/// restating it.
pub const PLATFORM_ROOTS: &[&str] = {
    if cfg!(target_os = "macos") {
        &[
            "/System",
            "/usr",
            "/bin",
            "/sbin",
            "/Library/Apple",
            "/private/etc",
            "/etc",
            "/var/select",
        ]
    } else if cfg!(unix) {
        &["/usr", "/bin", "/sbin", "/lib", "/lib64", "/etc"]
    } else {
        &[]
    }
};

/// The fixed roots at their requested paths and, where the host resolves a
/// symlink or firmlink, their canonical forms: the sandbox binds the requested
/// destination and the facts compare a resolved path.
fn platform_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    for path in PLATFORM_ROOTS {
        let path = PathBuf::from(path);
        if !roots.contains(&path) {
            roots.push(path.clone());
        }
        if let Ok(canonical) = std::fs::canonicalize(&path)
            && !roots.contains(&canonical)
        {
            roots.push(canonical);
        }
    }
    roots
}

/// The environment names a prepared process keeps from the captured host
/// environment: agent commands take them by this list, and a host-owned
/// confined helper starts from them. Everything else the service holds is
/// dropped.
const ENVIRONMENT: &[&str] = &[
    "PATH",
    "HOME",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "TERM",
    "NO_COLOR",
    "PAGER",
    "GIT_PAGER",
    "GH_PAGER",
    #[cfg(target_os = "macos")]
    "DEVELOPER_DIR",
    #[cfg(target_os = "macos")]
    "XDG_CONFIG_HOME",
];

/// Process facts and the source of the exec shell preference.
#[derive(Clone)]
pub struct HostPlatform {
    pub home: PathBuf,
    pub os: Os,
    /// Node's `process.arch` name (`arm64`, `x64`), as the TS texts use it.
    pub arch: String,
    /// IANA name of the local time zone at process start (TS `<timezone>`);
    /// absent when the system does not report one.
    pub timezone: Option<String>,
    /// Environment of the service process at start; never a client's.
    pub env: Vec<(OsString, OsString)>,
    /// The fixed system roots, captured when the platform mounts. The sandbox
    /// and file-access facts read this one list.
    pub platform_roots: Vec<PathBuf>,
    pub(crate) commands: Arc<std::sync::Mutex<std::collections::HashMap<String, Arc<TaskRecord>>>>,
    pub(crate) command_shutdown: CancellationToken,
    pub(crate) command_workers: tokio_util::task::TaskTracker,
    stopping: Arc<std::sync::Mutex<std::collections::HashSet<String>>>,
    shell: Arc<Mutex<ResolvedShell>>,
    shell_file: Option<(Arc<FileStorage>, usize, PathBuf)>,
    pub(crate) execution: Option<Arc<ExecutionSettingsHost>>,
    /// The installed runtimes a started process finds at the end of its
    /// `PATH`; `None` is a host that installs none.
    pub(crate) toolchain: Option<Arc<ToolchainSourceHost>>,
}

struct ResolvedShell {
    config: Result<Option<Vec<u8>>, String>,
    status: ShellStatus,
}

impl std::fmt::Debug for HostPlatform {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostPlatform")
            .field("home", &self.home)
            .field("os", &self.os)
            .field("arch", &self.arch)
            .field("timezone", &self.timezone)
            .field("env", &self.env)
            .field(
                "shell",
                &self.shell.try_lock().ok().map(|shell| shell.status.clone()),
            )
            .finish_non_exhaustive()
    }
}

/// A child process started by [`HostPlatform::spawn`].
pub struct ProcessSpec {
    pub executable: PathBuf,
    /// The shell argv is built by the caller; the model command is one element.
    pub args: Vec<OsString>,
    pub cwd: PathBuf,
    /// The complete child environment; nothing is inherited.
    pub env: Vec<(OsString, OsString)>,
}

#[derive(Debug)]
pub struct ProcessSpecError {
    pub code: &'static str,
    pub reason: String,
}

/// Whether a new process can be sandboxed on this host, or why it cannot. The
/// Process isolation control shows this; preparation reads the same backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum SandboxBackend {
    Available,
    Unavailable { reason: &'static str },
}

/// What a consumer keeps for a process that
/// [`HostPlatform::prepare_isolated_process`] sandboxed.
#[derive(Clone, Copy)]
pub(crate) struct Isolation;

impl Isolation {
    /// The note for a failed command whose output shows a typical denial.
    /// Backends report a denial only as an ordinary error inside the command,
    /// so this is a heuristic; it never offers an approval.
    pub(crate) fn failure_hint(&self, output: &str) -> Option<String> {
        const DENIALS: [&str; 10] = [
            "operation not permitted",
            "blocked by sandbox",
            "read-only file system",
            "bwrap:",
            "could not resolve host",
            "nodename nor servname",
            "temporary failure in name resolution",
            "name or service not known",
            "getaddrinfo",
            "network is unreachable",
        ];
        let reach = if cfg!(target_os = "macos") {
            "the workspace, $TMPDIR and system directories"
        } else {
            "the workspace and system directories"
        };
        let lower = output.to_lowercase();
        DENIALS.iter().any(|denial| lower.contains(denial)).then(
            || {
                format!(
                    "This command ran under the sandbox the user turned on, which may have caused the failure. It blocks network access and file access outside {reach}. Retrying or an approval does not lift it. Ask the user to change Process isolation in /settings."
                )
            },
        )
    }
}

/// The host-owned TMPDIR of one isolated process tree; the tree handle that
/// holds it removes it when dropped.
pub(crate) struct TemporaryDirectory(PathBuf);
impl Drop for TemporaryDirectory {
    fn drop(&mut self) {
        let path = std::mem::take(&mut self.0);
        let remove = move || {
            let _ = std::fs::remove_dir_all(path);
        };
        match tokio::runtime::Handle::try_current() {
            Ok(runtime) => drop(runtime.spawn_blocking(remove)),
            Err(_) => remove(),
        }
    }
}

/// How a wait reply ended, or that it is still running.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WaitStatus {
    Running,
    Exited,
    Signaled,
    TimedOut,
    Cancelled,
    Failed,
}

/// A wait reply; terminal when the task ended, `running` when the wait deadline elapsed.
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WaitReply {
    pub status: WaitStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signal: Option<String>,
    pub duration_ms: u64,
    pub output_path: String,
    pub output_bytes: u64,
    pub output_truncated: bool,
    pub preview: String,
    pub preview_truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure: Option<TaskFailure>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure_note: Option<String>,
}

/// Why a task reached a failed or cancelled terminal record.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum TaskFailure {
    OutputWriteFailed { message: String },
    TerminateFailed { message: String },
    JobUnavailable { message: String },
    ShellExited,
    ShellReset,
    CommandFailed,
    UnknownExit,
    OwnerUnloaded,
}

impl std::fmt::Display for TaskFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OutputWriteFailed { message }
            | Self::TerminateFailed { message }
            | Self::JobUnavailable { message } => f.write_str(message),
            Self::ShellExited => f.write_str("shell exited"),
            Self::ShellReset => f.write_str("shell reset"),
            Self::CommandFailed => f.write_str("command failed"),
            Self::UnknownExit => f.write_str("command ended with an unknown exit"),
            Self::OwnerUnloaded => f.write_str("execution owner unloaded"),
        }
    }
}

/// A page of a task's captured output.
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputPageReply {
    pub text: String,
    pub bytes: u64,
    pub total_bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<u64>,
    pub truncated: bool,
    pub retained_from: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reset: Option<bool>,
}

#[derive(Clone, Debug, serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum ShellEvent {
    Output { cursor: u64, text: String },
    Marker { cursor: u64, line: String },
}

impl ShellEvent {
    pub(crate) fn cursor(&self) -> u64 {
        match self {
            Self::Output { cursor, .. } | Self::Marker { cursor, .. } => *cursor,
        }
    }
}

/// A request to start a persistent PTY shell on this execution node.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ShellRequest {
    pub task_id: String,
    pub cwd: String,
    pub workspace: String,
    pub roots: Vec<String>,
    pub output_dir: String,
    pub cols: u16,
    pub rows: u16,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ShellWrite {
    pub task_id: String,
    pub data: String,
    #[serde(default)]
    pub command: Option<ShellCommandStart>,
    #[serde(default)]
    pub job: Option<ShellJobStart>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ShellJobStart {
    pub task_id: String,
    pub end_marker: String,
    /// Absolute path of the job's log; it must be inside the shell output directory.
    pub output_path: String,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ShellCommandStart {
    pub task_id: String,
    pub start_marker: String,
    pub end_marker: String,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ShellResize {
    pub task_id: String,
    pub cols: u16,
    pub rows: u16,
}

/// Byte limits for a configured command's two streams.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputLimits {
    pub stdout: usize,
    pub stderr: usize,
}

/// A request to run one confined helper on this node.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConfinedRequest {
    pub program: String,
    pub args: Vec<String>,
    pub read: Vec<String>,
    pub write: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    pub timeout_ms: u64,
    pub output_limit: OutputLimits,
    pub services: confined::SystemServices,
}

/// A confined helper's bounded result. Success means it stayed within its
/// limits; an output limit is an error, not a truncated reply.
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfinedReply {
    pub status: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub confined: bool,
}

/// A request to compose the PATH a child process gets. `base` is UTF-8; a
/// result that cannot be represented answers `None`.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChildPathRequest {
    pub directories: Vec<String>,
    pub base: String,
}

/// A request to remove a persistent-shell output directory.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RemoveOutputDirectoryRequest {
    pub path: String,
}

/// A request to run one configured command with stdin. `env` is UTF-8; a
/// non-UTF-8 environment belongs to the in-process binding.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SpawnWithStdinRequest {
    pub command: String,
    pub cwd: String,
    pub env: BTreeMap<String, Option<String>>,
    pub stdin: Option<String>,
    pub timeout_ms: u64,
    pub limits: OutputLimits,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SpawnWithStdinStatus {
    Exited,
    Signaled,
    Cancelled,
    TimedOut,
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpawnWithStdinReply {
    pub status: SpawnWithStdinStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signal: Option<String>,
    pub duration_ms: u64,
    pub stdout: String,
    pub stderr: String,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
}

/// A request to start a command on this execution node.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SpawnRequest {
    pub task_id: String,
    pub command: String,
    pub cwd: String,
    pub workspace: String,
    pub roots: Vec<String>,
    pub output_dir: String,
    pub timeout_ms: Option<u64>,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpawnReply {
    pub task_id: String,
    pub output_path: String,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TaskRequest {
    pub task_id: String,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WaitRequest {
    pub task_id: String,
    pub timeout_ms: Option<u64>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OutputRequest {
    pub task_id: String,
    pub cursor: Option<u64>,
    pub limit: Option<u64>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ShellEventRequest {
    pub task_id: String,
    pub cursor: u64,
    pub wait_ms: u64,
    pub limit: usize,
}

pub(crate) struct ShellEventPage {
    pub events: Vec<ShellEvent>,
    pub next_cursor: u64,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputPathReply {
    pub output_path: String,
}

#[derive(Debug, thiserror::Error, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HostPlatformError {
    #[error("invalid request: {0}")]
    Invalid(String),
    #[error("invalid path {path}: {reason}")]
    InvalidPath { path: String, reason: String },
    #[error("the confined helper exceeded its timeout")]
    TimedOut,
    #[error("the confined helper reached its {stream} limit")]
    OutputLimit { stream: ConfinedStream },
    #[error("unknown execution task")]
    TaskUnknown,
    #[error("execution task already exists")]
    TaskExists,
    #[error("execution task is still running")]
    TaskRunning,
    #[error("execution task has ended")]
    TaskEnded,
    #[error("output unavailable: {0}")]
    OutputUnavailable(String),
    #[error("cancelled")]
    Cancelled,
    #[error("execution host closed")]
    Closed,
    #[error("{0}")]
    Io(String),
    #[error("isolation configuration is invalid: {0}")]
    IsolationConfigInvalid(String),
    #[error("isolation is unavailable: {0}")]
    IsolationUnavailable(String),
    #[error("exec shell configuration is invalid: {0}")]
    ExecShellConfigInvalid(String),
    #[error("exec shell is unavailable: {0}")]
    ExecShellUnavailable(String),
    #[error("exec shell does not support this working directory: {0}")]
    ExecShellCwdUnsupported(String),
    #[error("working directory not found: {0}")]
    CwdNotFound(String),
    #[error("exec shell path mapping failed: {0}")]
    ExecShellPathMappingFailed(String),
    #[error("{0}")]
    Failed(String),
}

impl From<crate::workspace_tools::background::OutputPageError> for HostPlatformError {
    fn from(error: crate::workspace_tools::background::OutputPageError) -> Self {
        Self::OutputUnavailable(error.to_string())
    }
}

impl From<crate::workspace_tools::exec::CaptureError> for HostPlatformError {
    fn from(error: crate::workspace_tools::exec::CaptureError) -> Self {
        match error {
            crate::workspace_tools::exec::CaptureError::Unknown => Self::TaskUnknown,
            crate::workspace_tools::exec::CaptureError::Unavailable(reason) => {
                Self::OutputUnavailable(reason)
            }
        }
    }
}

/// The process facts the `hostPlatform.info` operation answers.
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HostInfo {
    pub os: Os,
    pub arch: String,
    pub home: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
    pub exec_root: String,
    pub platform_roots: Vec<String>,
    pub backend: SandboxBackend,
    pub exec_shell: ShellStatus,
}

#[rutis::service]
impl HostPlatform {
    /// Facts of the current process with an already resolved shell.
    pub fn current(shell: ShellStatus) -> io::Result<Self> {
        let env: Vec<_> = std::env::vars_os().collect();
        Ok(Self {
            home: native::home(&env)?,
            os: Os::CURRENT,
            arch: match std::env::consts::ARCH {
                "aarch64" => "arm64",
                "x86_64" => "x64",
                "x86" => "ia32",
                other => other,
            }
            .into(),
            timezone: iana_time_zone::get_timezone().ok(),
            env,
            platform_roots: platform_roots(),
            shell: Arc::new(Mutex::new(ResolvedShell {
                config: Ok(None),
                status: shell,
            })),
            shell_file: None,
            commands: Arc::default(),
            command_shutdown: CancellationToken::new(),
            command_workers: tokio_util::task::TaskTracker::new(),
            stopping: Arc::default(),
            execution: None,
            toolchain: None,
        })
    }
    /// Uses `host` for the execution switches at each process preparation. The
    /// HostPlatform plugin wires it; a platform without switches fails
    /// preparation instead of running unsandboxed (ADR 011 D1).
    #[service(skip)]
    pub fn with_execution_settings(mut self, host: ExecutionSettingsHost) -> Self {
        self.execution = Some(Arc::new(host));
        self
    }
    /// Uses `host` for the installed runtimes at each process start. The
    /// HostPlatform plugin wires it; a platform without it starts processes
    /// with the PATH they would have without any runtime.
    #[service(skip)]
    pub fn with_toolchain_source(mut self, host: ToolchainSourceHost) -> Self {
        self.toolchain = Some(Arc::new(host));
        self
    }
    /// The command for `spec` with a cleared environment; `stdin` is the caller's choice.
    fn command(spec: &ProcessSpec, stdin: Stdio) -> tokio::process::Command {
        let mut command = tokio::process::Command::new(&spec.executable);
        command
            .args(&spec.args)
            .current_dir(&spec.cwd)
            .env_clear()
            .envs(spec.env.iter().map(|(key, value)| (key, value)))
            .stdin(stdin);
        command
    }
    /// Starts `spec` as the root of its own process tree.
    #[service(skip)]
    pub fn spawn_tree(&self, spec: ProcessSpec) -> io::Result<ProcessTree> {
        ProcessTree::spawn(Self::command(&spec, Stdio::null()))
    }
    /// [`Self::spawn`] with `stdin` written to the child's standard input,
    /// which then closes. A child that exits before reading it does not fail
    /// the start.
    /// Runs one confined helper and returns its bounded result.
    pub async fn spawn_confined(
        &self,
        request: ConfinedRequest,
        cancelled: crate::host::CancelledFn,
    ) -> Result<ConfinedReply, HostPlatformError> {
        let cancel = CancellationToken::new();
        let waiter = {
            let cancel = cancel.clone();
            tokio::spawn(async move {
                let _ = cancelled().await;
                cancel.cancel();
            })
        };
        let spec = confined::ConfinedSpec {
            executable: request.program.into(),
            args: request.args.into_iter().map(Into::into).collect(),
            env: request
                .env
                .into_iter()
                .map(|(key, value)| (key.into(), value.into()))
                .collect(),
        };
        let confinement = confined::Confinement {
            read: request.read.into_iter().map(PathBuf::from).collect(),
            write: request.write.into_iter().map(PathBuf::from).collect(),
            timeout: Duration::from_millis(request.timeout_ms),
            stdout_limit: request.output_limit.stdout,
            stderr_limit: request.output_limit.stderr,
            services: request.services,
        };
        let result = self.spawn_confined_process(spec, confinement, cancel).await;
        waiter.abort();
        let output = result.map_err(|error| match error {
            confined::ConfinedError::InvalidPath { path, reason } => {
                HostPlatformError::InvalidPath {
                    path: path.to_string_lossy().into_owned(),
                    reason,
                }
            }
            confined::ConfinedError::TimedOut => HostPlatformError::TimedOut,
            confined::ConfinedError::OutputLimit { stream } => {
                HostPlatformError::OutputLimit { stream }
            }
            confined::ConfinedError::Cancelled => HostPlatformError::Cancelled,
            confined::ConfinedError::Spawn(error) | confined::ConfinedError::Cleanup(error) => {
                HostPlatformError::Io(error.to_string())
            }
        })?;
        Ok(ConfinedReply {
            status: output.status.code(),
            stdout: STANDARD.encode(output.stdout),
            stderr: STANDARD.encode(output.stderr),
            confined: output.confined,
        })
    }

    /// Composes the PATH a child process gets, as data. The served base is
    /// UTF-8; a non-UTF-8 daemon PATH uses [`Self::child_path_process`] in
    /// process and is not expressible through this face.
    pub fn child_path(&self, request: ChildPathRequest) -> Option<String> {
        self.child_path_process(&request.directories, OsStr::new(&request.base))
            .and_then(|path| path.to_str().map(str::to_owned))
    }

    /// Removes a persistent-shell output directory below `execRoot`.
    pub async fn remove_output_directory(
        &self,
        request: RemoveOutputDirectoryRequest,
    ) -> Result<(), HostPlatformError> {
        let platform = self.clone();
        let path = PathBuf::from(request.path);
        tokio::task::spawn_blocking(move || platform.remove_exec_directory(&path))
            .await
            .map_err(|error| HostPlatformError::Io(error.to_string()))?
            .map_err(|error| {
                if error.kind() == io::ErrorKind::InvalidInput {
                    HostPlatformError::Invalid(error.to_string())
                } else {
                    HostPlatformError::Io(error.to_string())
                }
            })
    }

    /// Runs one configured command with stdin and returns its bounded result.
    pub async fn spawn_with_stdin(
        &self,
        request: SpawnWithStdinRequest,
        cancelled: crate::host::CancelledFn,
    ) -> Result<SpawnWithStdinReply, HostPlatformError> {
        let started = Instant::now();
        let stdin = STANDARD
            .decode(request.stdin.unwrap_or_default())
            .map_err(|error| HostPlatformError::Invalid(error.to_string()))?;
        let cancel = CancellationToken::new();
        let waiter = {
            let cancel = cancel.clone();
            tokio::spawn(async move {
                let _ = cancelled().await;
                cancel.cancel();
            })
        };
        let command = ConfiguredCommand {
            command: request.command,
            cwd: PathBuf::from(request.cwd),
            env: request
                .env
                .into_iter()
                .map(|(key, value)| (key.into(), value.map(Into::into)))
                .collect(),
            stdin,
            timeout: Duration::from_millis(request.timeout_ms),
            stdout_limit: request.limits.stdout,
            stderr_limit: request.limits.stderr,
        };
        let end = self.configured(command, &cancel, None).await;
        waiter.abort();
        let end = end.map_err(|error| match error {
            ConfiguredError::Prepare(error) => Self::process_error(error),
            ConfiguredError::Unloaded => HostPlatformError::Closed,
            ConfiguredError::Spawn(error) => HostPlatformError::Io(error.to_string()),
            ConfiguredError::Wait(error, stderr) => {
                let reason = format!("wait failed: {error}");
                HostPlatformError::Io(if stderr.is_empty() {
                    reason
                } else {
                    format!("{reason}: {}", String::from_utf8_lossy(&stderr))
                })
            }
        })?;
        let duration_ms = started.elapsed().as_millis() as u64;
        let (status, exit_code, signal, stdout, stderr, stdout_truncated, stderr_truncated) =
            match end {
                ConfiguredEnd::Exited {
                    status,
                    stdout,
                    stderr,
                    stdout_truncated,
                    stderr_truncated,
                } => {
                    let signal = signal_name(status);
                    let exit_code = status.code();
                    let status = if signal.is_some() {
                        SpawnWithStdinStatus::Signaled
                    } else {
                        SpawnWithStdinStatus::Exited
                    };
                    (
                        status,
                        exit_code,
                        signal,
                        stdout,
                        stderr,
                        stdout_truncated,
                        stderr_truncated,
                    )
                }
                ConfiguredEnd::Cancelled => (
                    SpawnWithStdinStatus::Cancelled,
                    None,
                    None,
                    Vec::new(),
                    Vec::new(),
                    false,
                    false,
                ),
                ConfiguredEnd::TimedOut => (
                    SpawnWithStdinStatus::TimedOut,
                    None,
                    None,
                    Vec::new(),
                    Vec::new(),
                    false,
                    false,
                ),
            };
        Ok(SpawnWithStdinReply {
            status,
            exit_code,
            signal,
            duration_ms,
            stdout: STANDARD.encode(stdout),
            stderr: STANDARD.encode(stderr),
            stdout_truncated,
            stderr_truncated,
        })
    }

    #[service(skip)]
    pub fn spawn_with_stdin_process(
        &self,
        spec: ProcessSpec,
        stdin: Vec<u8>,
    ) -> io::Result<ProcessTree> {
        let mut tree = ProcessTree::spawn(Self::command(&spec, Stdio::piped()))?;
        if let Some(mut pipe) = tree.child.stdin.take() {
            tokio::spawn(async move {
                let _ = pipe.write_all(&stdin).await;
            });
        }
        Ok(tree)
    }

    /// The execution switches as they stand before this process starts. A host
    /// without the port fails here rather than running unsandboxed.
    pub(crate) async fn execution_settings(
        &self,
    ) -> Result<crate::execution_settings::ExecutionSettings, ProcessSpecError> {
        let source = self.execution.as_ref().ok_or_else(|| ProcessSpecError {
            code: "isolation_config_invalid",
            reason:
                "The host has no execution settings port, so the sandbox switches cannot be read"
                    .into(),
        })?;
        source
            .read(CancellationToken::new())
            .await
            .map_err(|error| ProcessSpecError {
                code: "isolation_config_invalid",
                reason: error.to_string(),
            })
    }

    #[cfg(windows)]
    #[service(skip)]
    pub(crate) async fn prepare_isolated_process(
        &self,
        spec: ProcessSpec,
        _workspaces: &[PathBuf],
        _output_dir: Option<&Path>,
    ) -> Result<(ProcessSpec, Option<TemporaryDirectory>, Option<Isolation>), ProcessSpecError>
    {
        if !self.execution_settings().await?.sandbox {
            return Ok((spec, None, None));
        }
        Err(ProcessSpecError {
            code: "isolation_unavailable",
            reason: WINDOWS_SANDBOX_UNAVAILABLE.into(),
        })
    }
    /// Reads the configured exec shell for this invocation without reloading
    /// the plugin. Explicitly constructed platforms keep their supplied shell.
    pub async fn exec_shell(&self) -> ShellStatus {
        let mut resolved = self.shell.lock().await;
        if let Some((storage, root, relative)) = &self.shell_file {
            let config = read_config(storage, *root, relative).await;
            if config != resolved.config
                || matches!(
                    resolved.status,
                    ShellStatus::Unavailable {
                        code: EXEC_SHELL_UNAVAILABLE,
                        ..
                    }
                )
            {
                let status = resolve_shell(config.clone(), &self.env).await;
                *resolved = ResolvedShell { config, status };
            }
        }
        resolved.status.clone()
    }

    /// The sandbox backend a new process would use.
    #[cfg(windows)]
    pub fn sandbox_backend(&self) -> SandboxBackend {
        SandboxBackend::Unavailable {
            reason: WINDOWS_SANDBOX_UNAVAILABLE,
        }
    }

    /// The sandbox backend a new process would use.
    #[cfg(target_os = "macos")]
    pub fn sandbox_backend(&self) -> SandboxBackend {
        SandboxBackend::Available
    }

    /// The sandbox backend a new process would use.
    #[cfg(all(unix, not(target_os = "macos")))]
    pub fn sandbox_backend(&self) -> SandboxBackend {
        match isolation::bubblewrap(&self.env) {
            Some(_) => SandboxBackend::Available,
            None => SandboxBackend::Unavailable {
                reason: isolation::BWRAP_UNAVAILABLE,
            },
        }
    }

    fn exec_root_path(&self) -> PathBuf {
        let root = self.home.join(".dim").join("exec");
        std::fs::canonicalize(&root).unwrap_or(root)
    }

    /// The directory the persistent shell owns: `<home>/.dim/exec`.
    pub fn exec_root(&self) -> Result<String, HostPlatformError> {
        self.exec_root_path()
            .to_str()
            .map(str::to_owned)
            .ok_or_else(|| HostPlatformError::Invalid("the exec root path is not UTF-8".into()))
    }
    /// The process facts a consumer needs, in one call. Resolves the exec
    /// shell; in-process callers that need only captured facts read the fields.
    pub async fn info(&self) -> Result<HostInfo, HostPlatformError> {
        let home = self
            .home
            .to_str()
            .ok_or_else(|| HostPlatformError::Invalid("the user home path is not UTF-8".into()))?;
        let exec_root = self.exec_root_path();
        let exec_root = exec_root
            .to_str()
            .ok_or_else(|| HostPlatformError::Invalid("the exec root path is not UTF-8".into()))?;
        let mut platform_roots = Vec::with_capacity(self.platform_roots.len());
        for root in &self.platform_roots {
            platform_roots.push(
                root.to_str()
                    .ok_or_else(|| {
                        HostPlatformError::Invalid("a platform root path is not UTF-8".into())
                    })?
                    .to_owned(),
            );
        }
        Ok(HostInfo {
            os: self.os,
            arch: self.arch.clone(),
            home: home.to_owned(),
            timezone: self.timezone.clone(),
            exec_root: exec_root.to_owned(),
            platform_roots,
            backend: self.sandbox_backend(),
            exec_shell: self.exec_shell().await,
        })
    }

    fn process_error(error: ProcessSpecError) -> HostPlatformError {
        match error.code {
            "task_exists" => HostPlatformError::TaskExists,
            "task_ended" => HostPlatformError::TaskEnded,
            "cancelled" => HostPlatformError::Cancelled,
            "io" => HostPlatformError::Io(error.reason),
            "isolation_config_invalid" => HostPlatformError::IsolationConfigInvalid(error.reason),
            "isolation_unavailable" => HostPlatformError::IsolationUnavailable(error.reason),
            "exec_shell_config_invalid" | EXEC_SHELL_CONFIG_INVALID => {
                HostPlatformError::ExecShellConfigInvalid(error.reason)
            }
            "exec_shell_unavailable" | EXEC_SHELL_UNAVAILABLE => {
                HostPlatformError::ExecShellUnavailable(error.reason)
            }
            "exec_shell_cwd_unsupported" | "EXEC_SHELL_CWD_UNSUPPORTED" => {
                HostPlatformError::ExecShellCwdUnsupported(error.reason)
            }
            "cwd_not_found" => HostPlatformError::CwdNotFound(error.reason),
            "exec_shell_path_mapping_failed" | "EXEC_SHELL_PATH_MAPPING_FAILED" => {
                HostPlatformError::ExecShellPathMappingFailed(error.reason)
            }
            "invalid_request" | "exec_invalid_request" => {
                HostPlatformError::Invalid(format!("{}: {}", error.code, error.reason))
            }
            _ => HostPlatformError::Failed(format!("{}: {}", error.code, error.reason)),
        }
    }

    /// Starts a command on this node and returns the task id and output path.
    /// Requires the plugin-built owner.
    pub async fn spawn(&self, request: SpawnRequest) -> Result<SpawnReply, HostPlatformError> {
        if request.task_id.is_empty()
            || request.task_id.contains(['/', '\\', '\0', ':'])
            || matches!(request.task_id.as_str(), "." | "..")
        {
            return Err(HostPlatformError::Invalid(
                "taskId must be a single filename component".into(),
            ));
        }
        let cwd = PathBuf::from(&request.cwd);
        let workspace = PathBuf::from(&request.workspace);
        let output_dir = PathBuf::from(&request.output_dir);
        let roots: Vec<PathBuf> = request.roots.iter().map(PathBuf::from).collect();
        if !cwd.is_absolute()
            || !workspace.is_absolute()
            || !output_dir.is_absolute()
            || roots.iter().any(|root| !root.is_absolute())
        {
            return Err(HostPlatformError::Invalid(
                "execution paths must be absolute".into(),
            ));
        }
        if !tokio::fs::metadata(&cwd)
            .await
            .is_ok_and(|metadata| metadata.is_dir())
        {
            return Err(HostPlatformError::Invalid(
                "working directory not found".into(),
            ));
        }
        let (spec, _) = self
            .build_process_spec(&request.command, &cwd)
            .await
            .map_err(Self::process_error)?;
        let output_path = output_dir.join(format!("{}.log", request.task_id));
        self.start_command(
            &request.task_id,
            spec,
            &roots,
            &output_dir,
            request.timeout_ms,
        )
        .await
        .map_err(Self::process_error)?;
        Ok(SpawnReply {
            task_id: request.task_id,
            output_path: output_path.to_string_lossy().into_owned(),
        })
    }

    /// Waits for a task's terminal record. `timeoutMs` stops the wait and
    /// answers `running`; it never kills the task. `cancelled` ends only the
    /// wait. Use `kill` to stop the task.
    pub async fn wait(
        &self,
        request: WaitRequest,
        cancelled: crate::host::CancelledFn,
    ) -> Result<WaitReply, HostPlatformError> {
        let task = self
            .task(&request.task_id)
            .ok_or(HostPlatformError::TaskUnknown)?;
        tokio::select! {
            biased;
            _ = cancelled() => Err(HostPlatformError::Cancelled),
            value = task.wait(request.timeout_ms) => Ok(value),
        }
    }

    /// Kills a task's process tree.
    pub async fn kill(&self, request: TaskRequest) -> Result<(), HostPlatformError> {
        let task = self
            .task(&request.task_id)
            .ok_or(HostPlatformError::TaskUnknown)?;
        if task.is_terminal() {
            return Ok(());
        }
        if !task.is_running() {
            return Err(HostPlatformError::TaskRunning);
        }
        match &task.kind {
            TaskKind::Command(command) => {
                command.kill.cancel();
                let terminal = task.wait(None).await;
                match terminal.failure {
                    Some(TaskFailure::TerminateFailed { message }) => {
                        Err(HostPlatformError::Failed(message))
                    }
                    _ => Ok(()),
                }
            }
            TaskKind::Shell(shell) => {
                shell.terminate();
                let terminal = task.wait(None).await;
                match terminal.failure {
                    Some(TaskFailure::TerminateFailed { message }) => {
                        Err(HostPlatformError::Failed(message))
                    }
                    _ => Ok(()),
                }
            }
            TaskKind::ShellCommand(command) => {
                if !command.shell.is_active(&task) {
                    return Ok(());
                }
                command
                    .shell
                    .write(b"\x03")
                    .await
                    .map_err(|error| HostPlatformError::Io(error.to_string()))
            }
            TaskKind::Job(job) => {
                if !self.stopping.lock().unwrap().insert(task.id.clone()) {
                    return Err(HostPlatformError::TaskRunning);
                }
                let mut pid = job.pid.subscribe();
                let mut current = *pid.borrow_and_update();
                if current.is_none() {
                    let _ = tokio::time::timeout(Duration::from_secs(2), pid.changed()).await;
                    current = *pid.borrow_and_update();
                }
                if task.is_terminal() {
                    self.stopping.lock().unwrap().remove(&task.id);
                    return Ok(());
                }
                let Some(pid) = current else {
                    let record = task.clone();
                    let job = job.clone();
                    let shutdown = self.command_shutdown.clone();
                    let workers = self.command_workers.clone();
                    let stopping = self.stopping.clone();
                    self.command_workers.spawn(async move {
                        let mut pid = job.pid.subscribe();
                        let mut changed = record.changed.subscribe();
                        loop {
                            if record.is_terminal() {
                                stopping.lock().unwrap().remove(&record.id);
                                return;
                            }
                            let current = *pid.borrow_and_update();
                            if let Some(pid) = current {
                                if record.is_terminal() {
                                    stopping.lock().unwrap().remove(&record.id);
                                    return;
                                }
                                let tree = job_tree(pid);
                                if kill_job(pid, tree.clone()).await.is_err() {
                                    let shutdown = shutdown.clone();
                                    let id = record.id.clone();
                                    let stopping = stopping.clone();
                                    workers.spawn(async move {
                                        observe_job_release(record, job, pid, shutdown, tree).await;
                                        stopping.lock().unwrap().remove(&id);
                                    });
                                    return;
                                }
                                record.finish(Terminal {
                                    status: TaskStatus::Cancelled,
                                    exit_code: None,
                                    signal: None,
                                    duration_ms: record.started.elapsed().as_millis() as u64,
                                    failure: None,
                                    output: job.output_record().0,
                                    note: None,
                                });
                                stopping.lock().unwrap().remove(&record.id);
                                return;
                            }
                            tokio::select! {
                                biased;
                                _ = shutdown.cancelled() => {
                                    stopping.lock().unwrap().remove(&record.id);
                                    return;
                                }
                                changed = changed.changed() => {
                                    if changed.is_err() {
                                        stopping.lock().unwrap().remove(&record.id);
                                        return;
                                    }
                                }
                                changed = pid.changed() => {
                                    if changed.is_err() {
                                        stopping.lock().unwrap().remove(&record.id);
                                        return;
                                    }
                                }
                            }
                        }
                    });
                    return Err(HostPlatformError::TaskRunning);
                };
                let tree = job_tree(pid);
                if let Err(error) = kill_job(pid, tree.clone()).await {
                    let record = task.clone();
                    let job = job.clone();
                    let shutdown = self.command_shutdown.clone();
                    let stopping = self.stopping.clone();
                    let id = task.id.clone();
                    self.command_workers.spawn(async move {
                        observe_job_release(record, job, pid, shutdown, tree).await;
                        stopping.lock().unwrap().remove(&id);
                    });
                    return Err(HostPlatformError::Failed(error.to_string()));
                }
                task.finish(Terminal {
                    status: TaskStatus::Cancelled,
                    exit_code: None,
                    signal: None,
                    duration_ms: task.started.elapsed().as_millis() as u64,
                    failure: None,
                    output: job.output_record().0,
                    note: None,
                });
                self.stopping.lock().unwrap().remove(&task.id);
                Ok(())
            }
        }
    }

    /// Releases an ended task's record.
    pub async fn settle(&self, request: TaskRequest) -> Result<(), HostPlatformError> {
        let Some(task) = self.task(&request.task_id) else {
            return Ok(());
        };
        if !task.is_terminal() {
            return Err(HostPlatformError::TaskRunning);
        }
        let mut commands = self.commands.lock().unwrap();
        commands.remove(&request.task_id);
        if let TaskKind::Shell(shell) = &task.kind {
            shell.sweep(task.terminal());
        }
        Ok(())
    }

    pub(crate) fn task(&self, id: &str) -> Option<Arc<TaskRecord>> {
        self.commands.lock().unwrap().get(id).cloned()
    }

    pub(crate) fn shell_task(&self, id: &str) -> Option<Arc<shell::ShellTask>> {
        match &self.task(id)?.kind {
            TaskKind::Shell(shell) => Some(shell.clone()),
            _ => None,
        }
    }

    pub(crate) fn shell_command(&self, id: &str) -> Option<Arc<shell::ShellCommand>> {
        match &self.task(id)?.kind {
            TaskKind::ShellCommand(command) => Some(command.clone()),
            _ => None,
        }
    }

    pub(crate) fn job_marker(&self, id: &str) -> Option<String> {
        match &self.task(id)?.kind {
            TaskKind::Job(job) => Some(job.end_marker.clone()),
            _ => None,
        }
    }

    pub(crate) fn job_completion(
        &self,
        id: &str,
    ) -> Option<(std::path::PathBuf, std::path::PathBuf)> {
        match &self.task(id)?.kind {
            TaskKind::Job(job) => job.completion.clone(),
            _ => None,
        }
    }

    /// Starts a persistent PTY shell on this node and returns the task id.
    /// Requires the plugin-built owner.
    pub async fn shell(&self, request: ShellRequest) -> Result<SpawnReply, HostPlatformError> {
        if request.task_id.is_empty()
            || request.task_id.contains(['/', '\\', '\0', ':'])
            || matches!(request.task_id.as_str(), "." | "..")
        {
            return Err(HostPlatformError::Invalid(
                "taskId must be a single filename component".into(),
            ));
        }
        let cwd = PathBuf::from(&request.cwd);
        let workspace = PathBuf::from(&request.workspace);
        let output_dir = PathBuf::from(&request.output_dir);
        let roots: Vec<PathBuf> = request.roots.iter().map(PathBuf::from).collect();
        if !cwd.is_absolute()
            || !workspace.is_absolute()
            || !output_dir.is_absolute()
            || roots.iter().any(|root| !root.is_absolute())
        {
            return Err(HostPlatformError::Invalid(
                "execution paths must be absolute".into(),
            ));
        }
        if !tokio::fs::metadata(&cwd)
            .await
            .is_ok_and(|metadata| metadata.is_dir())
        {
            return Err(HostPlatformError::Invalid(
                "working directory not found".into(),
            ));
        }
        tokio::fs::create_dir_all(&output_dir)
            .await
            .map_err(|error| HostPlatformError::Io(error.to_string()))?;
        let (shell, _) = self
            .start_shell(
                &request.task_id,
                &cwd,
                &roots,
                &output_dir,
                PtySize {
                    cols: request.cols,
                    rows: request.rows,
                },
            )
            .await
            .map_err(Self::process_error)?;
        Ok(SpawnReply {
            task_id: request.task_id,
            output_path: shell.output_path().to_string_lossy().into_owned(),
        })
    }

    /// Writes bytes to a shell task.
    pub async fn write(&self, request: ShellWrite) -> Result<(), HostPlatformError> {
        let task = self
            .task(&request.task_id)
            .ok_or(HostPlatformError::TaskUnknown)?;
        if task.is_terminal() {
            return Err(HostPlatformError::TaskEnded);
        }
        let shell = match &task.kind {
            TaskKind::Shell(shell) => shell.clone(),
            _ => return Err(HostPlatformError::TaskUnknown),
        };
        if request.command.is_some() && request.job.is_some() {
            return Err(HostPlatformError::Invalid(
                "a shell write cannot start a command and a job together".into(),
            ));
        }
        let bytes = STANDARD
            .decode(request.data)
            .map_err(|error| HostPlatformError::Invalid(error.to_string()))?;
        let command = if let Some(start) = request.command {
            if !valid_shell_marker(&start.start_marker) || !valid_shell_marker(&start.end_marker) {
                return Err(HostPlatformError::Invalid(
                    "shell command markers must be __DIM_START_<8 hex>_<8 hex>__ and __DIM_END_<8 hex>_<8 hex>__".into(),
                ));
            }
            if start.task_id.is_empty()
                || start.task_id.contains(['/', '\\', '\0', ':'])
                || matches!(start.task_id.as_str(), "." | "..")
            {
                return Err(HostPlatformError::Invalid(
                    "command taskId must be a single filename component".into(),
                ));
            }
            let directory = shell
                .output_path()
                .parent()
                .unwrap_or_else(|| Path::new("."));
            let command_path = directory.join(format!("{}.log", start.task_id));
            if std::fs::symlink_metadata(&command_path).is_ok() {
                return Err(HostPlatformError::TaskExists);
            }
            let command = Arc::new(shell::ShellCommand::new(
                shell.clone(),
                start.start_marker,
                start.end_marker,
                command_path,
            ));
            let record = TaskRecord::new(
                start.task_id.clone(),
                TaskKind::ShellCommand(command.clone()),
            );
            {
                let mut commands = self.commands.lock().unwrap();
                if commands.contains_key(&start.task_id) {
                    return Err(HostPlatformError::TaskExists);
                }
                commands.insert(start.task_id.clone(), record.clone());
                if !shell.reserve_active_command(&record) {
                    commands.remove(&start.task_id);
                    return Err(HostPlatformError::TaskRunning);
                }
            }
            shell.add_child(&record);
            Some((start.task_id, record))
        } else {
            None
        };
        let job = if let Some(start) = request.job {
            if !valid_shell_marker(&start.end_marker) || !start.end_marker.starts_with("__DIM_END_")
            {
                return Err(HostPlatformError::Invalid(
                    "shell job end marker must be __DIM_END_<8 hex>_<8 hex>__".into(),
                ));
            }
            if start.task_id.is_empty()
                || start.task_id.contains(['/', '\\', '\0', ':'])
                || matches!(start.task_id.as_str(), "." | "..")
            {
                return Err(HostPlatformError::Invalid(
                    "job taskId must be a single filename component".into(),
                ));
            }
            let directory = shell
                .output_path()
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .to_path_buf();
            let output_path = PathBuf::from(&start.output_path);
            if output_path.parent().is_none_or(|parent| !parent.is_dir()) {
                return Err(HostPlatformError::Invalid(
                    "job outputPath parent must exist".into(),
                ));
            }
            if output_path.file_stem().and_then(|stem| stem.to_str())
                != Some(start.task_id.as_str())
            {
                return Err(HostPlatformError::Invalid(
                    "job outputPath stem must equal its taskId".into(),
                ));
            }
            if !output_path.is_absolute()
                || output_path
                    .components()
                    .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
                || !output_path.starts_with(&directory)
            {
                return Err(HostPlatformError::Invalid(
                    "job outputPath must be absolute and inside the shell output directory".into(),
                ));
            }
            let completion = shell.pwsh().then(|| {
                (
                    directory.join(format!("{}.result", start.task_id)),
                    directory.join(format!("{}.err", start.task_id)),
                )
            });
            if std::fs::symlink_metadata(&output_path).is_ok()
                || completion.as_ref().is_some_and(|(result, error)| {
                    std::fs::symlink_metadata(result).is_ok()
                        || std::fs::symlink_metadata(error).is_ok()
                })
            {
                return Err(HostPlatformError::TaskExists);
            }
            let job = Arc::new(shell::ShellJob {
                shell: Arc::downgrade(&shell),
                pid: watch::channel(None).0,
                end_marker: start.end_marker,
                output_path,
                completion,
            });
            let record = TaskRecord::new(start.task_id.clone(), TaskKind::Job(job.clone()));
            {
                let mut commands = self.commands.lock().unwrap();
                if commands.contains_key(&start.task_id) {
                    return Err(HostPlatformError::TaskExists);
                }
                commands.insert(start.task_id.clone(), record.clone());
            }
            shell.add_child(&record);
            Some((start.task_id, record))
        } else {
            None
        };
        if let Err(error) = shell.write(&bytes).await {
            if let Some((id, _)) = &command {
                self.commands.lock().unwrap().remove(id);
            }
            if let Some((id, _)) = &job {
                self.commands.lock().unwrap().remove(id);
            }
            if command.is_some() {
                shell.set_active_command(None);
            }
            return Err(HostPlatformError::Io(error.to_string()));
        }
        if let Some((_, record)) = &command {
            record.start();
        }
        if let Some((_, record)) = &job {
            record.start();
            if let Some((result_path, _)) = self.job_completion(&record.id) {
                let record = record.clone();
                self.command_workers.spawn(async move {
                    loop {
                        if record.is_terminal() {
                            return;
                        }
                        tokio::time::sleep(Duration::from_millis(200)).await;
                        if record.is_terminal() {
                            return;
                        }
                        let Ok(text) = tokio::fs::read_to_string(&result_path).await else {
                            continue;
                        };
                        let TaskKind::Job(job) = &record.kind else {
                            return;
                        };
                        if let Some(code) = shell::completion_code(&text, &job.end_marker) {
                            record.finish(shell::job_completion_terminal(&record, job, &code));
                            return;
                        }
                    }
                });
            }
        }
        Ok(())
    }

    /// Resizes a shell task's PTY.
    pub async fn resize(&self, request: ShellResize) -> Result<(), HostPlatformError> {
        let task = self
            .task(&request.task_id)
            .ok_or(HostPlatformError::TaskUnknown)?;
        if task.is_terminal() {
            return Err(HostPlatformError::TaskEnded);
        }
        let shell = match &task.kind {
            TaskKind::Shell(shell) => shell.clone(),
            _ => return Err(HostPlatformError::TaskUnknown),
        };
        shell
            .resize(PtySize {
                cols: request.cols,
                rows: request.rows,
            })
            .await
            .map_err(|error| HostPlatformError::Io(error.to_string()))
    }

    /// Reads a page of a task's captured output.
    pub async fn output(
        &self,
        request: OutputRequest,
    ) -> Result<OutputPageReply, HostPlatformError> {
        let limit = request.limit.unwrap_or(64 * 1024);
        if limit == 0 {
            return Err(HostPlatformError::Invalid(
                "output limit must be positive".into(),
            ));
        }
        let limit = limit.min(4 * 1024 * 1024) as usize;
        if let Some(shell) = self.shell_task(&request.task_id) {
            return shell
                .page(request.cursor, limit)
                .map_err(HostPlatformError::from);
        }
        if let Some(command) = self.shell_command(&request.task_id) {
            return command
                .page(request.cursor.unwrap_or(0), limit)
                .map_err(HostPlatformError::from);
        }
        if let Some(page) = self
            .task(&request.task_id)
            .and_then(|task| task.terminal_page(request.cursor.unwrap_or(0), limit))
        {
            return Ok(OutputPageReply {
                text: page.text,
                bytes: page.bytes,
                total_bytes: page.total_bytes.max(page.bytes),
                next_cursor: page.next_cursor,
                truncated: page.truncated,
                retained_from: 0,
                reset: None,
            });
        }
        if let Some(task) = self.task(&request.task_id)
            && let TaskKind::Job(job) = &task.kind
        {
            let page = match self
                .retained_output_page(
                    std::path::Path::new(&job.output_path),
                    request.cursor.unwrap_or(0),
                    limit,
                )
                .await
            {
                Ok(page) => page,
                Err(crate::workspace_tools::background::OutputPageError::Unavailable) => {
                    return Ok(OutputPageReply {
                        text: String::new(),
                        bytes: 0,
                        total_bytes: 0,
                        next_cursor: None,
                        truncated: false,
                        retained_from: 0,
                        reset: None,
                    });
                }
                Err(error) => return Err(HostPlatformError::from(error)),
            };
            return Ok(OutputPageReply {
                text: page.text,
                bytes: page.bytes,
                total_bytes: page.total_bytes.max(page.bytes),
                next_cursor: page.next_cursor,
                truncated: false,
                retained_from: 0,
                reset: None,
            });
        }
        let task = self.command_task(&request.task_id)?;
        let page = task
            .page(request.cursor.unwrap_or(0), limit)
            .await
            .map_err(HostPlatformError::from)?;
        Ok(OutputPageReply {
            text: page.text,
            bytes: page.bytes,
            total_bytes: page.total_bytes.max(page.bytes),
            next_cursor: page.next_cursor,
            truncated: page.truncated,
            retained_from: 0,
            reset: None,
        })
    }

    /// The private ShellSession transport: the lossless owner event page.
    /// Not a public data-face operation and never exposed through TypeScript.
    pub(crate) async fn shell_events(
        &self,
        request: ShellEventRequest,
    ) -> Result<ShellEventPage, HostPlatformError> {
        let shell = self
            .shell_task(&request.task_id)
            .ok_or(HostPlatformError::TaskUnknown)?;
        let page = shell
            .events_page(request.cursor, Some(request.wait_ms), request.limit)
            .await;
        Ok(ShellEventPage {
            events: page.events,
            next_cursor: page.next_cursor,
        })
    }

    /// Writes the captured output to the task's log file and returns its path.
    pub async fn retain_output(
        &self,
        request: TaskRequest,
    ) -> Result<OutputPathReply, HostPlatformError> {
        if let Some(shell) = self.shell_task(&request.task_id) {
            return Ok(OutputPathReply {
                output_path: shell.output_path().to_string_lossy().into_owned(),
            });
        }
        if let Some(command) = self.shell_command(&request.task_id) {
            return Ok(OutputPathReply {
                output_path: command.output_path().to_string_lossy().into_owned(),
            });
        }
        let output_path = self.retain_output_by_id(&request.task_id).await?;
        Ok(OutputPathReply { output_path })
    }

    /// Reads the bounded live tail of a task.
    pub async fn tail(
        &self,
        request: TaskRequest,
    ) -> Result<crate::workspace_tools::exec::OutputTail, HostPlatformError> {
        if let Some(shell) = self.shell_task(&request.task_id) {
            return Ok(shell.tail());
        }
        if let Some(command) = self.shell_command(&request.task_id) {
            return Ok(command.tail());
        }
        if let Some(task) = self.task(&request.task_id)
            && let TaskKind::Job(job) = &task.kind
        {
            let (record, _) = job.output_record();
            return Ok(crate::workspace_tools::exec::OutputTail {
                text: record.preview,
                total_bytes: record.bytes,
                truncated: record.truncated,
            });
        }
        if let Some(terminal) = self.task(&request.task_id).and_then(|task| task.terminal()) {
            let text = match &terminal.output.text {
                Some(text) => crate::host_platform::output::truncate_preview(text),
                None => terminal.output.preview.clone(),
            };
            return Ok(crate::workspace_tools::exec::OutputTail {
                text,
                total_bytes: terminal.output.bytes,
                truncated: terminal.output.truncated,
            });
        }
        self.tail_by_id(&request.task_id).await.map_err(Into::into)
    }
    /// Resolves the persistent shell: bash, zsh, pwsh, or Windows PowerShell.
    /// Other shells fall back per the platform rules.
    pub async fn persistent_shell(&self) -> ShellStatus {
        match self.exec_shell().await {
            ShellStatus::Available(runtime) => match runtime.edition {
                ShellEdition::Bash
                | ShellEdition::Zsh
                | ShellEdition::Pwsh
                | ShellEdition::WindowsPowershell => ShellStatus::Available(runtime),
                _ => self.fallback_persistent_shell(),
            },
            ShellStatus::Unavailable { .. } => self.fallback_persistent_shell(),
        }
    }
}

const RUNTIME_BIN_ENV_KEYS: [&str; 5] = [
    "DIMCODE_CLI_BIN",
    "DIMCODE_UV_BIN",
    "DIMCODE_PYTHON_BIN",
    "DIMCODE_NODE_BIN",
    "DIMCODE_BUNDLED_GIT_BIN",
];
const PLUGIN_BIN_DIRS_ENV: &str = "DIMCODE_PLUGIN_BIN_DIRS";

impl HostPlatform {
    /// The downloaded runtimes installed under the plan in effect, node first;
    /// empty without the port. An entry whose bin directory is not inside its
    /// archive directory is skipped: the sandbox grant is the root, so the two
    /// must nest.
    pub fn installed_toolchains(&self) -> Vec<InstalledToolchain> {
        let Some(source) = self.toolchain.as_ref() else {
            return Vec::new();
        };
        let reply = match source.list() {
            Ok(reply) => reply,
            Err(error) => {
                tracing::warn!(%error, "the installed runtimes cannot cross as data");
                return Vec::new();
            }
        };
        reply
            .toolchains
            .into_iter()
            .map(|toolchain| InstalledToolchain {
                bin: toolchain.bin.into(),
                root: toolchain.root.into(),
            })
            .filter(|toolchain| toolchain.bin.starts_with(&toolchain.root))
            .collect()
    }

    /// The PATH a child gets: the host's embedding `directories` first, then
    /// `base` (the PATH it would get with no installed runtime), then the
    /// toolchain bin directories not already in either. `None` when nothing is
    /// appended, so the caller leaves the child's PATH as it is. `base` may be
    /// a login shell's literal `$PATH`, which is only expanded when the
    /// command runs.
    pub fn child_path_process(&self, directories: &[String], base: &OsStr) -> Option<OsString> {
        let windows = self.os == Os::Windows;
        let bins: Vec<PathBuf> = self
            .installed_toolchains()
            .into_iter()
            .map(|toolchain| toolchain.bin)
            .collect();
        let additions = new_path_entries(directories, base, &bins, windows);
        (!additions.is_empty()).then(|| compose_path(directories, base, &additions, windows))
    }

    /// Builds a command with the current exec shell and returns that same shell.
    /// Tool policies and cwd argument resolution belong to the caller.
    pub async fn build_process_spec(
        &self,
        command: &str,
        cwd: &Path,
    ) -> Result<(ProcessSpec, ExecShellRuntime), ProcessSpecError> {
        let shell = match self.exec_shell().await {
            ShellStatus::Available(shell) => shell,
            ShellStatus::Unavailable { code, reason } => {
                return Err(ProcessSpecError { code, reason });
            }
        };
        let kind = shell.edition.kind();
        if self.os == Os::Windows && kind == ShellKind::Cmd && windows_network_share(cwd) {
            return Err(ProcessSpecError {
                code: "EXEC_SHELL_CWD_UNSUPPORTED",
                reason:
                    "Command Prompt cannot use a network-share workspace as its current directory."
                        .into(),
            });
        }
        if !tokio::fs::metadata(cwd)
            .await
            .is_ok_and(|metadata| metadata.is_dir())
        {
            return Err(ProcessSpecError {
                code: "cwd_not_found",
                reason: format!("Working directory not found: {}", cwd.display()),
            });
        }
        let mut env = build_env(self);
        let toolchain: Vec<PathBuf> = self
            .installed_toolchains()
            .into_iter()
            .map(|toolchain| toolchain.bin)
            .collect();
        let command = if kind == ShellKind::Posix {
            prepend_runtime_path(
                command,
                &env,
                self,
                Path::new(&shell.executable_path),
                shell.edition,
                cwd,
                &toolchain,
            )
            .await
            .map_err(|reason| ProcessSpecError {
                code: "EXEC_SHELL_PATH_MAPPING_FAILED",
                reason,
            })?
        } else {
            append_toolchain_path(&mut env, &toolchain);
            command.to_owned()
        };
        let args: &[&str] = match kind {
            ShellKind::Posix => &["-lc"],
            ShellKind::Powershell => &["-NoLogo", "-NoProfile", "-NonInteractive", "-Command"],
            ShellKind::Cmd => &["/d", "/s", "/v:off", "/c"],
        };
        Ok((
            ProcessSpec {
                executable: PathBuf::from(&shell.executable_path),
                args: args
                    .iter()
                    .map(OsString::from)
                    .chain([command.into()])
                    .collect(),
                cwd: cwd.to_owned(),
                env,
            },
            shell,
        ))
    }
}

fn windows_network_share(path: &Path) -> bool {
    let normalized = path.to_string_lossy().replace('/', "\\");
    normalized.to_uppercase().starts_with("\\\\?\\UNC\\")
        || (normalized.starts_with("\\\\") && !normalized.starts_with("\\\\?\\"))
}

/// TS `buildEnv`: the service start environment with forced overrides.
fn build_env(platform: &HostPlatform) -> Vec<(OsString, OsString)> {
    let mut env: Vec<(OsString, OsString)> = platform.env.clone();
    if platform.os == Os::Windows
        && let Some(entry) = env.iter_mut().find(|(key, _)| {
            key.to_str()
                .is_some_and(|key| key.eq_ignore_ascii_case("PATH"))
        })
    {
        entry.0 = "PATH".into();
    }
    let forced = [
        ("TERM", "dumb"),
        ("NO_COLOR", "1"),
        ("PAGER", "cat"),
        ("GIT_PAGER", "cat"),
        ("GH_PAGER", "cat"),
    ];
    env.retain(|(key, _)| key != "FORCE_COLOR" && !forced.iter().any(|(name, _)| key == name));
    env.extend(
        forced
            .iter()
            .map(|(key, value)| (OsString::from(key), OsString::from(value))),
    );
    env
}

fn env_str<'a>(env: &'a [(OsString, OsString)], key: &str) -> Option<&'a str> {
    crate::host_platform::env_var(env, key).and_then(|value| value.to_str())
}

/// Product runtime directories that a login shell may drop from `PATH`.
fn runtime_path_entries(env: &[(OsString, OsString)], windows_paths: bool) -> Vec<String> {
    let mut output: Vec<String> = vec![];
    let mut push = |entry: String| {
        if !output.contains(&entry) {
            output.push(entry);
        }
    };
    for key in RUNTIME_BIN_ENV_KEYS {
        if let Some(value) = env_str(env, key).filter(|value| !value.is_empty())
            && Path::new(value).is_absolute()
        {
            let path = Path::new(value);
            push(path.parent().unwrap_or(path).to_string_lossy().into_owned());
        }
    }
    if let Some(value) = env_str(env, PLUGIN_BIN_DIRS_ENV).map(str::trim) {
        let separator = if windows_paths { ';' } else { ':' };
        value
            .split(separator)
            .map(str::trim)
            .filter(|entry| !entry.is_empty() && Path::new(entry).is_absolute())
            .for_each(|entry| push(entry.to_owned()));
    }
    output
}

fn quote_posix(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// Whether two PATH entries name the same directory. Windows compares case
/// insensitively, as its environment does.
fn path_entry_matches(entry: &OsStr, directory: &Path, windows: bool) -> bool {
    if windows {
        entry
            .to_string_lossy()
            .eq_ignore_ascii_case(&directory.to_string_lossy())
    } else {
        entry == directory.as_os_str()
    }
}

/// `directories` not already among `host` or `base`'s entries, in their order.
/// A base that is a login shell's literal `$PATH` matches nothing, because its
/// entries are only known when the command runs.
fn new_path_entries(
    host: &[String],
    base: &OsStr,
    directories: &[PathBuf],
    windows: bool,
) -> Vec<PathBuf> {
    let mut added: Vec<PathBuf> = Vec::new();
    for directory in directories {
        let present = host
            .iter()
            .any(|entry| path_entry_matches(OsStr::new(entry), directory, windows))
            || std::env::split_paths(base)
                .any(|entry| path_entry_matches(entry.as_os_str(), directory, windows))
            || added.iter().any(|entry| entry == directory);
        if !present {
            added.push(directory.clone());
        }
    }
    added
}

/// The child's PATH in its fixed order: `host`, then `base`, then
/// `directories`.
fn compose_path(host: &[String], base: &OsStr, directories: &[PathBuf], windows: bool) -> OsString {
    let separator = if windows { ";" } else { ":" };
    let mut segments: Vec<OsString> = host.iter().map(OsString::from).collect();
    if !base.is_empty() {
        segments.push(base.to_owned());
    }
    segments.extend(
        directories
            .iter()
            .map(|directory| directory.as_os_str().to_owned()),
    );
    let mut value = OsString::new();
    for (index, segment) in segments.iter().enumerate() {
        if index > 0 {
            value.push(separator);
        }
        value.push(segment);
    }
    value
}

/// Appends the toolchain directories to the child environment's `PATH`; the
/// PowerShell and cmd shells, and every process they start, resolve commands
/// from it. Their separator is `;`.
fn append_toolchain_path(env: &mut Vec<(OsString, OsString)>, directories: &[PathBuf]) {
    if directories.is_empty() {
        return;
    }
    match env.iter_mut().find(|(key, _)| key == "PATH") {
        Some((_, value)) => {
            let additions = new_path_entries(&[], value, directories, true);
            if !additions.is_empty() {
                *value = compose_path(&[], value, &additions, true);
            }
        }
        None => env.push((
            "PATH".into(),
            compose_path(&[], OsStr::new(""), directories, true),
        )),
    }
}

/// TS `prependRuntimePathForPosixCommand`.
async fn prepend_runtime_path(
    command: &str,
    env: &[(OsString, OsString)],
    platform: &HostPlatform,
    shell: &Path,
    edition: ShellEdition,
    cwd: &Path,
    toolchain: &[PathBuf],
) -> Result<String, String> {
    let git_bash = edition == ShellEdition::GitBash;
    let host = runtime_path_entries(env, git_bash);
    let additions: Vec<String> = new_path_entries(&host, OsStr::new("$PATH"), toolchain, git_bash)
        .into_iter()
        .map(|directory| directory.to_string_lossy().into_owned())
        .collect();
    let mut entries = host;
    let host = entries.len();
    entries.extend(additions);
    if entries.is_empty() {
        return Ok(command.to_owned());
    }
    let mapped = if git_bash {
        map_git_bash_entries(&entries, platform, shell, env, cwd).await?
    } else {
        entries
    };
    let (host, additions) = mapped.split_at(host);
    let mut path: Vec<String> = host.iter().map(|entry| quote_posix(entry)).collect();
    path.push("\"$PATH\"".into());
    path.extend(additions.iter().map(|entry| quote_posix(entry)));
    Ok(format!("export PATH={}; {command}", path.join(":")))
}

async fn map_git_bash_entries(
    entries: &[String],
    platform: &HostPlatform,
    shell: &Path,
    env: &[(OsString, OsString)],
    cwd: &Path,
) -> Result<Vec<String>, String> {
    let cygpath = git_bash_cygpath(shell).ok_or_else(|| {
        format!(
            "Git Bash path converter is unavailable: {}",
            shell.display()
        )
    })?;
    let run = |args: [&str; 2]| {
        capture(
            platform,
            ProcessSpec {
                executable: cygpath.clone(),
                args: args.iter().map(OsString::from).collect(),
                cwd: cwd.to_owned(),
                env: env.to_vec(),
            },
        )
    };
    let normalize = |value: &str| {
        value
            .replace('/', "\\")
            .trim_end_matches('\\')
            .to_lowercase()
    };
    let mut output = vec![];
    for entry in entries {
        let result: Result<String, String> = async {
            let mapped = run(["-u", entry]).await?.trim().to_owned();
            if !mapped.starts_with('/') {
                return Err(format!("cygpath returned a non-absolute path for {entry}"));
            }
            let bytes = entry.as_bytes();
            let drive = bytes.len() >= 3
                && bytes[0].is_ascii_alphabetic()
                && bytes[1] == b':'
                && matches!(bytes[2], b'\\' | b'/');
            if drive || entry.starts_with("\\\\") {
                let round_trip = run(["-w", &mapped]).await?.trim().to_owned();
                if normalize(&round_trip) != normalize(entry) {
                    return Err(format!("cygpath round-trip mismatch for {entry}"));
                }
            }
            Ok(mapped)
        }
        .await;
        output.push(result.map_err(|error| {
            format!("Failed to map bundled runtime paths for Git Bash: {error}")
        })?);
    }
    Ok(output)
}

fn git_bash_cygpath(bash: &Path) -> Option<PathBuf> {
    let mut current = bash.parent()?;
    for _ in 0..4 {
        let git = current.join("cmd").join("git.exe");
        let cygpath = current.join("usr").join("bin").join("cygpath.exe");
        if git.is_file() && cygpath.is_file() {
            return Some(cygpath);
        }
        current = current.parent()?;
    }
    None
}

/// Runs a product helper and returns its stdout.
async fn capture(platform: &HostPlatform, spec: ProcessSpec) -> Result<String, String> {
    let mut tree = platform
        .spawn_tree(spec)
        .map_err(|error| error.to_string())?;
    let mut stdout = tree.stdout();
    drop(tree.stderr());
    let mut bytes = vec![];
    let read = stdout.read_to_end(&mut bytes).await;
    let status = tree.wait().await.map_err(|error| error.to_string())?;
    read.map_err(|error| error.to_string())?;
    if !status.success() {
        return Err(format!("converter exited with {status}"));
    }
    String::from_utf8(bytes).map_err(|error| error.to_string())
}

/// A variable of an environment snapshot; names are case-insensitive on Windows.
pub(crate) fn env_var<'a>(env: &'a [(OsString, OsString)], name: &str) -> Option<&'a OsStr> {
    env.iter()
        .find(|(key, _)| {
            if cfg!(windows) {
                key.to_str()
                    .is_some_and(|key| key.eq_ignore_ascii_case(name))
            } else {
                key == name
            }
        })
        .map(|(_, value)| value.as_os_str())
}

/// Handle of a started process tree. Exactly one owner holds it. Dropping it
/// keeps the descendants only when the root exited on its own and was reaped
/// by [`Self::wait`] with no termination requested (as in TS); otherwise it
/// kills whatever is left, including after a [`Self::terminate`] that could
/// not confirm the tree empty.
pub struct ProcessTree {
    child: Child,
    #[cfg(unix)]
    pid: u32,
    reaped: bool,
    /// A termination was requested; the tree is no longer released on drop.
    terminating: bool,
    /// The termination confirmed that no process is left.
    terminated: bool,
    #[cfg(windows)]
    job: native::Job,
    temporary: Option<TemporaryDirectory>,
}
impl ProcessTree {
    /// Starts a command in its own process tree with piped stdout and stderr.
    /// The caller controls stdin, arguments, environment and working directory.
    pub fn spawn(mut command: tokio::process::Command) -> io::Result<Self> {
        command.stdout(Stdio::piped()).stderr(Stdio::piped());
        native::spawn(command)
    }

    /// Keeps the tree's isolated TMPDIR until this handle is dropped.
    pub(crate) fn hold(&mut self, temporary: Option<TemporaryDirectory>) {
        self.temporary = temporary;
    }
    pub fn stdout(&mut self) -> ChildStdout {
        self.child.stdout.take().expect("stdout is taken once")
    }
    pub fn stderr(&mut self) -> ChildStderr {
        self.child.stderr.take().expect("stderr is taken once")
    }
    /// Waits for the root process; cancel safe.
    pub async fn wait(&mut self) -> io::Result<ExitStatus> {
        let status = self.child.wait().await?;
        self.reaped = true;
        Ok(status)
    }
    /// Terminates every process of the tree and confirms that none is left.
    pub async fn terminate(&mut self) -> io::Result<()> {
        if !self.terminated {
            self.terminating = true;
            native::terminate(self).await?;
            self.terminated = true;
        }
        Ok(())
    }
    /// The root exited on its own and was reaped: its descendants stay.
    fn released(&self) -> bool {
        self.reaped && !self.terminating
    }
}

#[cfg(unix)]
impl Drop for ProcessTree {
    fn drop(&mut self) {
        if !self.terminated && !self.released() {
            native::kill_tree(self.pid);
        }
    }
}
#[cfg(windows)]
impl Drop for ProcessTree {
    fn drop(&mut self) {
        if self.released() {
            self.job.keep_processes();
        }
    }
}

#[derive(Debug, Clone)]
pub struct HostPlatformConfig {
    /// The execution switches, provided by the App Server's `ExecutionSettings`
    /// part. Every assembly that starts processes declares it (ADR 011 D1).
    pub execution: Key<ExecutionSettingsHost>,
    /// The installed runtimes, provided by the App Server's `ToolchainSource`
    /// part. An assembly that installs none declares a fixed source with no
    /// directories.
    pub toolchain: Key<ToolchainSourceHost>,
    /// `exec-shell.json`: FileStorage root index and relative path. `None`
    /// means the profile has no home; the shell is resolved automatically.
    pub exec_shell_file: Option<(usize, PathBuf)>,
    /// The user home the platform reports. `None` uses the process home; a
    /// host that runs with an isolated home sets it, so every capability
    /// resolves `~` and the user Skill root inside that home.
    pub home: Option<PathBuf>,
}

/// Process plugin providing `P:HostPlatform`.
pub struct HostPlatformPlugin {
    config: HostPlatformConfig,
    /// File storage only when a local configuration source is configured.
    storage: Option<Key<FileStorage>>,
    execution: Key<ExecutionSettingsHost>,
    toolchain: Key<ToolchainSourceHost>,
}
impl HostPlatformPlugin {
    pub fn new(names: &Names<'_>, config: HostPlatformConfig) -> rutis::Result<Self> {
        let storage = uses_storage(&config)
            .then(|| names.key::<FileStorage>())
            .transpose()?;
        let execution = config.execution.clone();
        let toolchain = config.toolchain.clone();
        Ok(Self {
            config,
            storage,
            execution,
            toolchain,
        })
    }
}
/// File storage only when a local configuration source is configured.
fn uses_storage(config: &HostPlatformConfig) -> bool {
    config.exec_shell_file.is_some()
}
impl DynPlugin for HostPlatformPlugin {
    fn name(&self) -> &str {
        "HostPlatform"
    }
    fn injects(&self) -> Vec<TypeKey> {
        self.storage
            .iter()
            .map(|storage| storage.erase())
            .chain(std::iter::once(self.execution.erase()))
            .chain(std::iter::once(self.toolchain.erase()))
            .collect()
    }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, rutis::Result<Effect>> {
        Box::pin(async move {
            let storage = match &self.storage {
                Some(storage) => Some(ctx.require_at(storage)?),
                None => None,
            };
            let shell_file = self
                .config
                .exec_shell_file
                .clone()
                .zip(storage.clone())
                .map(|((root, relative), storage)| (storage, root, relative));
            let config = match &shell_file {
                Some((storage, root, relative)) => read_config(storage, *root, relative).await,
                None => Ok(None),
            };
            let env: Vec<_> = std::env::vars_os().collect();
            let shell = resolve_shell(config.clone(), &env).await;
            let mut platform = HostPlatform::current(shell)
                .map_err(|error| rutis::Error::from(ErrorKind::PluginFailed(error.into())))?;
            if let Some(home) = &self.config.home {
                platform.home = home.clone();
            }
            platform.shell_file = shell_file;
            let execution = ctx.require_at(&self.execution)?;
            platform.execution = Some(execution.clone());
            let toolchain = ctx.require_at(&self.toolchain)?;
            platform.toolchain = Some(toolchain.clone());
            platform.shell.lock().await.config = config;
            platform.command_shutdown = ctx.cancellation_token().child_token();
            let platform = Arc::new(platform);
            let owner = platform.clone();
            ctx.effect(move || async move {
                {
                    let _admission = owner.commands.lock().unwrap();
                    owner.command_shutdown.cancel();
                }
                owner.command_workers.close();
                owner.command_workers.wait().await;
                owner.commands.lock().unwrap().clear();
                Ok(())
            })?;
            ctx.provide_as::<HostPlatform>(platform)?;
            Ok(Effect::Done)
        })
    }
}

async fn read_config(
    storage: &Arc<FileStorage>,
    root: usize,
    relative: &Path,
) -> Result<Option<Vec<u8>>, String> {
    let failed = |error: FileStorageError| {
        format!(
            "Failed to read exec shell config: {}: {error}",
            relative.display()
        )
    };
    let path = storage.path(root, relative).map_err(failed)?;
    match storage.read(&path, &CancellationToken::new()).await {
        Ok(bytes) => Ok(Some(bytes)),
        Err(FileStorageError::Io(error)) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(failed(error)),
    }
}

/// Resolves the exec shell from the `exec-shell.json` read result (`None`
/// when missing, `Err` with the reason when unreadable) and `env`.
pub async fn resolve_shell(
    config: Result<Option<Vec<u8>>, String>,
    env: &[(OsString, OsString)],
) -> ShellStatus {
    let explicit = match config.and_then(|bytes| match bytes {
        None => Ok(None),
        Some(bytes) => parse_preference(&bytes),
    }) {
        Ok(explicit) => explicit,
        Err(reason) => {
            return ShellStatus::Unavailable {
                code: EXEC_SHELL_CONFIG_INVALID,
                reason,
            };
        }
    };
    #[cfg(unix)]
    let detected = {
        let env = env.to_vec();
        match tokio::task::spawn_blocking(move || native::detect(&env)).await {
            Ok(detected) => detected,
            Err(error) => {
                return ShellStatus::Unavailable {
                    code: EXEC_SHELL_UNAVAILABLE,
                    reason: format!("Failed to detect command shells: {error}"),
                };
            }
        }
    };
    #[cfg(windows)]
    let detected = native::detect(env).await;
    let selected = match &explicit {
        Some(path) => detected
            .shells
            .iter()
            .find(|shell| paths_equal(&shell.path, Path::new(path)))
            .map(|shell| (shell, ShellSource::Explicit)),
        None => detected
            .auto
            .map(|(index, source)| (&detected.shells[index], source)),
    };
    match selected {
        Some((shell, source)) => available_shell(
            shell.path.clone(),
            shell.edition,
            source,
            match shell.edition {
                ShellEdition::GitBash | ShellEdition::WindowsPowershell | ShellEdition::Cmd => {
                    OutputEncoding::Unknown
                }
                _ => OutputEncoding::Utf8,
            },
        ),
        None => ShellStatus::Unavailable {
            code: EXEC_SHELL_UNAVAILABLE,
            reason: match explicit {
                Some(path) => format!("Selected command shell is unavailable: {path}"),
                None => "No supported command shell was found.".into(),
            },
        },
    }
}

fn parse_preference(bytes: &[u8]) -> Result<Option<String>, String> {
    let value: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|error| format!("Invalid exec shell config: {error}"))?;
    let serde_json::Value::Object(map) = value else {
        return Err("Exec shell config must be an object.".into());
    };
    if let Some(key) = map
        .keys()
        .find(|key| *key != "version" && *key != "executablePath")
    {
        return Err(format!("Unknown exec shell config field: {key}"));
    }
    if map.get("version").and_then(serde_json::Value::as_f64) != Some(1.0) {
        return Err("Exec shell config version must be 1.".into());
    }
    match map.get("executablePath") {
        None => Ok(None),
        Some(serde_json::Value::String(path))
            if !path.trim().is_empty() && is_absolute_executable_path(path) =>
        {
            Ok(Some(path.clone()))
        }
        Some(_) => Err("Exec shell executablePath must be a non-empty absolute path.".into()),
    }
}

fn is_absolute_executable_path(value: &str) -> bool {
    if cfg!(windows) {
        let bytes = value.as_bytes();
        (bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && matches!(bytes[2], b'\\' | b'/'))
            || value.starts_with("\\\\")
    } else {
        Path::new(value).is_absolute()
    }
}

fn paths_equal(left: &Path, right: &Path) -> bool {
    if cfg!(windows) {
        left.as_os_str().to_string_lossy().to_lowercase()
            == right.as_os_str().to_string_lossy().to_lowercase()
    } else {
        left == right
    }
}

struct Candidate {
    path: PathBuf,
    edition: ShellEdition,
}
struct Detection {
    shells: Vec<Candidate>,
    /// Index into `shells` and how it was chosen.
    auto: Option<(usize, ShellSource)>,
}

/// Directories of `PATH`; Windows strips one pair of outer quotes (ADR 020).
fn path_entries(env: &[(OsString, OsString)]) -> Vec<PathBuf> {
    let Some(value) = env_var(env, "PATH") else {
        return vec![];
    };
    if cfg!(windows) {
        value
            .to_string_lossy()
            .split(';')
            .map(|entry| strip_outer_quotes(entry.trim()))
            .filter(|entry| !entry.is_empty())
            .map(PathBuf::from)
            .collect()
    } else {
        std::env::split_paths(value)
            .filter(|entry| !entry.as_os_str().is_empty())
            .collect()
    }
}

fn strip_outer_quotes(value: &str) -> &str {
    if value.len() >= 2 && value.starts_with('"') && value.ends_with('"') {
        &value[1..value.len() - 1]
    } else {
        value
    }
}

#[cfg(unix)]
use unix as native;
#[cfg(unix)]
mod unix {
    use super::*;
    use std::{
        ffi::CStr,
        os::unix::ffi::{OsStrExt, OsStringExt},
        time::Duration,
    };

    const TERMINATE_GRACE: Duration = Duration::from_secs(2);
    const POSIX_SHELLS: [(&str, ShellEdition); 5] = [
        ("bash", ShellEdition::Bash),
        ("zsh", ShellEdition::Zsh),
        ("sh", ShellEdition::Sh),
        ("dash", ShellEdition::Dash),
        ("ksh", ShellEdition::Ksh),
    ];

    pub fn home(env: &[(OsString, OsString)]) -> io::Result<PathBuf> {
        if let Some(home) = env_var(env, "HOME").filter(|home| !home.is_empty()) {
            return Ok(home.into());
        }
        passwd(|entry| entry.pw_dir)
            .map(PathBuf::from)
            .ok_or_else(|| io::Error::other("cannot determine the home directory"))
    }

    fn passwd(field: impl Fn(&libc::passwd) -> *mut libc::c_char) -> Option<OsString> {
        let mut entry: libc::passwd = unsafe { std::mem::zeroed() };
        let mut buffer = vec![0 as libc::c_char; 16 * 1024];
        let mut result = std::ptr::null_mut();
        // SAFETY: the buffer outlives the entry fields read below.
        let status = unsafe {
            libc::getpwuid_r(
                libc::getuid(),
                &mut entry,
                buffer.as_mut_ptr(),
                buffer.len(),
                &mut result,
            )
        };
        let value = field(&entry);
        if status != 0 || result.is_null() || value.is_null() {
            return None;
        }
        // SAFETY: getpwuid_r stored a NUL-terminated string in the buffer.
        let bytes = unsafe { CStr::from_ptr(value) }.to_bytes().to_vec();
        (!bytes.is_empty()).then(|| OsString::from_vec(bytes))
    }

    fn executable(path: &Path) -> bool {
        let Ok(path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
            return false;
        };
        // SAFETY: access only reads the NUL-terminated path.
        unsafe { libc::access(path.as_ptr(), libc::X_OK) == 0 }
    }

    fn edition(path: &Path) -> Option<ShellEdition> {
        let name = path.file_name()?.to_str()?;
        POSIX_SHELLS
            .iter()
            .find(|(shell, _)| *shell == name)
            .map(|(_, edition)| *edition)
    }

    pub fn detect(env: &[(OsString, OsString)]) -> Detection {
        let env_login = env_var(env, "SHELL")
            .and_then(OsStr::to_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from);
        let user_login = passwd(|entry| entry.pw_shell).map(PathBuf::from);
        let logins: Vec<PathBuf> = env_login.iter().chain(&user_login).cloned().collect();
        let supported_login = logins
            .iter()
            .find(|path| edition(path).is_some() && executable(path))
            .cloned();
        let mut shells: Vec<Candidate> = Vec::new();
        let mut add = |path: PathBuf| {
            if !path.is_absolute() || shells.iter().any(|shell| shell.path == path) {
                return;
            }
            if let Some(edition) = edition(&path)
                && executable(&path)
            {
                shells.push(Candidate { path, edition });
            }
        };
        logins.iter().cloned().for_each(&mut add);
        if let Ok(text) = std::fs::read_to_string("/etc/shells") {
            text.lines()
                .map(str::trim)
                .filter(|line| line.starts_with('/'))
                .for_each(|line| add(line.into()));
        }
        let entries = path_entries(env);
        for (name, _) in POSIX_SHELLS {
            add(Path::new("/bin").join(name));
            for entry in &entries {
                add(entry.join(name));
            }
        }
        let position = |found: &dyn Fn(&Candidate) -> bool| shells.iter().position(found);
        let auto = match &supported_login {
            Some(login) => position(&|shell| shell.path == *login).map(|i| (i, ShellSource::Login)),
            None => position(&|shell| shell.path == Path::new("/bin/bash"))
                .or_else(|| position(&|shell| shell.edition == ShellEdition::Bash))
                .or_else(|| position(&|shell| shell.path == Path::new("/bin/sh")))
                .or_else(|| position(&|shell| shell.edition == ShellEdition::Sh))
                .map(|i| (i, ShellSource::Fallback)),
        };
        Detection { shells, auto }
    }

    pub fn spawn(mut command: tokio::process::Command) -> io::Result<ProcessTree> {
        // SAFETY: setsid is async-signal-safe; the child leads a new session
        // and process group, so the tree is signalled through its group.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    Err(io::Error::last_os_error())
                } else {
                    Ok(())
                }
            });
        }
        let child = command.spawn()?;
        Ok(ProcessTree {
            pid: child.id().expect("a just started child has a pid"),
            child,
            reaped: false,
            terminating: false,
            terminated: false,
            temporary: None,
        })
    }

    pub fn signal(pid: u32, signal: libc::c_int) {
        // SAFETY: plain syscall on the group this handle created.
        unsafe { libc::kill(-(pid as libc::pid_t), signal) };
    }

    /// Kills every descendant that still names `pid` as an ancestor, then the
    /// root group. A child that started its own session is not in the root
    /// group, so the `/proc` walk is the part that reaches it.
    pub fn kill_tree(pid: u32) {
        fn children(pid: u32) -> Vec<u32> {
            let Ok(text) = std::fs::read_to_string(format!("/proc/{pid}/task/{pid}/children"))
            else {
                return Vec::new();
            };
            text.split_whitespace()
                .filter_map(|value| value.parse().ok())
                .collect()
        }
        let mut pending = children(pid);
        let mut seen = Vec::new();
        while let Some(child) = pending.pop() {
            if seen.contains(&child) {
                continue;
            }
            seen.push(child);
            pending.extend(children(child));
            unsafe { libc::kill(-(child as libc::pid_t), libc::SIGKILL) };
            unsafe { libc::kill(child as libc::pid_t, libc::SIGKILL) };
        }
        unsafe { libc::kill(-(pid as libc::pid_t), libc::SIGKILL) };
    }

    /// True once the root is reaped and the group has no member left.
    async fn group_gone(tree: &mut ProcessTree, pid: u32, within: Duration) -> bool {
        let deadline = tokio::time::Instant::now() + within;
        loop {
            if !tree.reaped && tree.child.try_wait().ok().flatten().is_some() {
                tree.reaped = true;
            }
            // SAFETY: signal 0 only probes the group.
            let probe = unsafe { libc::kill(-(pid as libc::pid_t), 0) };
            if tree.reaped
                && probe == -1
                && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
            {
                return true;
            }
            if tokio::time::Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    pub async fn terminate(tree: &mut ProcessTree) -> io::Result<()> {
        let pid = tree.pid;
        // An empty group keeps no member, so its id is free for reuse: do not
        // signal it. A tree that left nothing costs one probe, no grace.
        if group_gone(tree, pid, Duration::ZERO).await {
            return Ok(());
        }
        signal(pid, libc::SIGTERM);
        if group_gone(tree, pid, TERMINATE_GRACE).await {
            return Ok(());
        }
        signal(pid, libc::SIGKILL);
        if group_gone(tree, pid, TERMINATE_GRACE).await {
            return Ok(());
        }
        Err(io::Error::other(format!(
            "process group {pid} is still alive after SIGKILL"
        )))
    }
}

#[cfg(windows)]
use windows as native;
#[cfg(windows)]
mod windows {
    use super::*;
    use std::{os::windows::ffi::OsStringExt, time::Duration};
    use windows_sys::Win32::{
        Foundation::{
            CloseHandle, ERROR_INVALID_PARAMETER, ERROR_NO_MORE_FILES, GetLastError, HANDLE,
            INVALID_HANDLE_VALUE, STILL_ACTIVE,
        },
        System::{
            Com::CoTaskMemFree,
            Diagnostics::ToolHelp::{
                CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
                TH32CS_SNAPPROCESS, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
            },
            JobObjects::{
                AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
                JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
                JobObjectBasicAccountingInformation, JobObjectExtendedLimitInformation,
                QueryInformationJobObject, SetInformationJobObject, TerminateJobObject,
            },
            Threading::{
                CREATE_NO_WINDOW, CREATE_SUSPENDED, GetExitCodeProcess, OpenProcess, OpenThread,
                PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SET_QUOTA, PROCESS_TERMINATE,
                ResumeThread, THREAD_SUSPEND_RESUME, TerminateProcess,
            },
        },
        UI::Shell::{FOLDERID_Profile, SHGetKnownFolderPath},
    };

    pub fn kill_tree(pid: u32) {
        let _ = terminate_tree(pid);
    }

    pub fn process_alive(pid: u32) -> bool {
        // SAFETY: the process handle is closed below.
        unsafe {
            let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if process.is_null() {
                return GetLastError() != ERROR_INVALID_PARAMETER;
            }
            let mut code = 0u32;
            let ok = GetExitCodeProcess(process, &mut code);
            CloseHandle(process);
            ok == 0 || code == STILL_ACTIVE as u32
        }
    }

    pub fn tree_pids(pid: u32) -> Option<Vec<u32>> {
        // SAFETY: snapshot handle is closed below; entries are sized.
        let parents = unsafe {
            let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
            if snapshot == INVALID_HANDLE_VALUE {
                return None;
            }
            let mut entry: PROCESSENTRY32W = std::mem::zeroed();
            entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
            let mut parents = Vec::new();
            if Process32FirstW(snapshot, &mut entry) == 0 {
                let error = GetLastError();
                CloseHandle(snapshot);
                let _ = error;
                return None;
            }
            loop {
                parents.push((entry.th32ProcessID, entry.th32ParentProcessID));
                if Process32NextW(snapshot, &mut entry) == 0 {
                    if GetLastError() != ERROR_NO_MORE_FILES {
                        CloseHandle(snapshot);
                        return None;
                    }
                    break;
                }
            }
            CloseHandle(snapshot);
            parents
        };
        let mut tree = vec![pid];
        let mut index = 0;
        while index < tree.len() {
            let parent = tree[index];
            tree.extend(
                parents
                    .iter()
                    .filter(|(child, of)| *of == parent && !tree.contains(child))
                    .map(|(child, _)| *child)
                    .collect::<Vec<_>>(),
            );
            index += 1;
        }
        Some(tree)
    }

    pub fn tree_alive(pid: u32) -> bool {
        tree_pids(pid).is_none_or(|tree| tree.into_iter().any(process_alive))
    }

    const SHELL_PROBE_TIMEOUT: Duration = Duration::from_millis(1_500);
    const SHELL_PROBE_CONCURRENCY: usize = 4;
    const TERMINATE_CONFIRM: Duration = Duration::from_secs(5);

    pub fn home(env: &[(OsString, OsString)]) -> io::Result<PathBuf> {
        if let Some(home) = env_var(env, "USERPROFILE").filter(|home| !home.is_empty()) {
            return Ok(home.into());
        }
        let mut path = std::ptr::null_mut();
        // SAFETY: the returned buffer is freed with CoTaskMemFree below.
        let status =
            unsafe { SHGetKnownFolderPath(&FOLDERID_Profile, 0, std::ptr::null_mut(), &mut path) };
        if status != 0 || path.is_null() {
            return Err(io::Error::other("cannot determine the home directory"));
        }
        // SAFETY: the known folder path is NUL-terminated UTF-16.
        let home = unsafe {
            let length = (0..).take_while(|&i| *path.add(i) != 0).count();
            let value = OsString::from_wide(std::slice::from_raw_parts(path, length));
            CoTaskMemFree(path as *const _);
            value
        };
        Ok(home.into())
    }

    type Edition = ShellEdition;

    fn executable(path: &Path) -> bool {
        path.is_file()
    }

    fn var(env: &[(OsString, OsString)], names: &[&str]) -> Option<PathBuf> {
        names
            .iter()
            .find_map(|name| env_var(env, name))
            .and_then(OsStr::to_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    }

    fn find_all_in_path(name: &str, env: &[(OsString, OsString)]) -> Vec<PathBuf> {
        let mut output = vec![];
        for directory in path_entries(env) {
            for extension in [".exe", ""] {
                let candidate = directory.join(format!("{name}{extension}"));
                if executable(&candidate) {
                    output.push(candidate);
                }
            }
        }
        output
    }

    fn program_roots(env: &[(OsString, OsString)]) -> Vec<PathBuf> {
        ["ProgramW6432", "ProgramFiles", "ProgramFiles(x86)"]
            .iter()
            .filter_map(|name| var(env, &[name]))
            .collect()
    }

    fn git_root(executable_path: &Path) -> Option<PathBuf> {
        let mut current = executable_path.parent()?.to_path_buf();
        for _ in 0..4 {
            if executable(&current.join("cmd").join("git.exe"))
                && executable(&current.join("usr").join("bin").join("cygpath.exe"))
            {
                return Some(current);
            }
            current = current.parent()?.to_path_buf();
        }
        None
    }

    fn bundled_git_root(root: &Path, env: &[(OsString, OsString)]) -> bool {
        let Some(bundled) = var(env, &["DIMCODE_BUNDLED_GIT_BIN"]) else {
            return false;
        };
        let bundled_root = git_root(&bundled).or_else(|| {
            bundled
                .parent()
                .and_then(Path::parent)
                .map(Path::to_path_buf)
        });
        bundled_root.is_some_and(|bundled| paths_equal(root, &bundled))
    }

    fn collect(env: &[(OsString, OsString)]) -> Vec<(PathBuf, Edition)> {
        let mut candidates: Vec<(PathBuf, Edition)> = vec![];
        let mut seen: Vec<String> = vec![];
        let mut add = |path: PathBuf, edition: Edition| {
            let Some(value) = path.to_str().map(|value| strip_outer_quotes(value.trim())) else {
                return;
            };
            if value.is_empty() {
                return;
            }
            let key = value.to_lowercase();
            if seen.contains(&key) {
                return;
            }
            seen.push(key);
            let path = PathBuf::from(value);
            if executable(&path) {
                candidates.push((path, edition));
            }
        };
        for path in find_all_in_path("pwsh", env) {
            add(path, Edition::Pwsh);
        }
        for root in program_roots(env) {
            let installations = root.join("PowerShell");
            if let Ok(entries) = std::fs::read_dir(&installations) {
                let mut directories: Vec<String> = entries
                    .flatten()
                    .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
                    .filter_map(|entry| entry.file_name().into_string().ok())
                    .collect();
                directories.sort_by(|left, right| natural_order(right, left));
                for directory in directories {
                    add(
                        installations.join(directory).join("pwsh.exe"),
                        Edition::Pwsh,
                    );
                }
            }
        }
        for path in find_all_in_path("powershell", env) {
            add(path, Edition::WindowsPowershell);
        }
        let system_root = var(env, &["SystemRoot", "SYSTEMROOT", "windir", "WINDIR"]);
        if let Some(root) = &system_root {
            add(
                root.join("System32")
                    .join("WindowsPowerShell")
                    .join("v1.0")
                    .join("powershell.exe"),
                Edition::WindowsPowershell,
            );
        }
        if let Some(comspec) = var(env, &["ComSpec", "COMSPEC"]) {
            add(comspec, Edition::Cmd);
        }
        if let Some(root) = &system_root {
            add(root.join("System32").join("cmd.exe"), Edition::Cmd);
        }
        for path in find_all_in_path("cmd", env) {
            add(path, Edition::Cmd);
        }
        let mut git_roots: Vec<PathBuf> = program_roots(env)
            .into_iter()
            .map(|root| root.join("Git"))
            .collect();
        if let Some(local) = var(env, &["LOCALAPPDATA", "LocalAppData"]) {
            git_roots.push(local.join("Programs").join("Git"));
        }
        for git in find_all_in_path("git", env) {
            if let Some(root) = git_root(&git) {
                git_roots.push(root);
            }
        }
        let mut unique: Vec<PathBuf> = vec![];
        for root in git_roots {
            if !unique.contains(&root) {
                unique.push(root);
            }
        }
        for root in unique {
            add(root.join("bin").join("bash.exe"), Edition::GitBash);
        }
        for path in find_all_in_path("bash", env) {
            add(path, Edition::GitBash);
        }
        candidates
    }

    /// Numeric-aware descending order of installation directory names.
    fn natural_order(left: &str, right: &str) -> std::cmp::Ordering {
        let key = |value: &str| {
            value
                .split('.')
                .map(|part| part.parse::<u64>().unwrap_or(0))
                .collect::<Vec<_>>()
        };
        key(left).cmp(&key(right)).then_with(|| left.cmp(right))
    }

    fn probe_args(edition: Edition) -> &'static [&'static str] {
        match edition {
            Edition::Pwsh => &[
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "[Console]::Out.Write('DIMCODE_PWSH:' + $PSVersionTable.PSEdition + ':' + $PSVersionTable.PSVersion.Major)",
            ],
            Edition::WindowsPowershell => &[
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "[Console]::Out.Write('DIMCODE_WINPS:' + $PSVersionTable.PSEdition + ':' + $PSVersionTable.PSVersion.Major)",
            ],
            Edition::Cmd => &["/d", "/s", "/v:off", "/c", "echo DIMCODE_CMD"],
            _ => &[
                "--noprofile",
                "--norc",
                "-c",
                "printf 'DIMCODE_GIT_BASH:%s' \"$MSYSTEM\"",
            ],
        }
    }

    fn probe_matches(edition: Edition, stdout: &str) -> bool {
        match edition {
            Edition::Pwsh => stdout
                .strip_prefix("DIMCODE_PWSH:Core:")
                .and_then(|major| major.parse::<u32>().ok())
                .is_some_and(|major| major >= 7),
            Edition::WindowsPowershell => stdout == "DIMCODE_WINPS:Desktop:5",
            Edition::Cmd => stdout == "DIMCODE_CMD",
            _ => stdout
                .strip_prefix("DIMCODE_GIT_BASH:")
                .is_some_and(|system| system.starts_with("MINGW") || system.starts_with("MSYS")),
        }
    }

    /// Direct spawn with closed stdin, no profile, hidden window and a bound.
    async fn probe(path: &Path, edition: Edition) -> bool {
        let mut command = tokio::process::Command::new(path);
        command
            .args(probe_args(edition))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .kill_on_drop(true);
        let Ok(child) = command.spawn() else {
            return false;
        };
        match tokio::time::timeout(SHELL_PROBE_TIMEOUT, child.wait_with_output()).await {
            Ok(Ok(output)) if output.status.success() => {
                probe_matches(edition, String::from_utf8_lossy(&output.stdout).trim())
            }
            _ => false,
        }
    }

    pub async fn detect(env: &[(OsString, OsString)]) -> Detection {
        let collected = collect(env);
        let mut shells = vec![];
        for batch in collected.chunks(SHELL_PROBE_CONCURRENCY) {
            let probes = batch.iter().map(|(path, edition)| probe(path, *edition));
            for ((path, edition), passed) in batch
                .iter()
                .zip(futures_util::future::join_all(probes).await)
            {
                let accepted = passed
                    && (*edition != Edition::GitBash
                        || git_root(path).is_some_and(|root| !bundled_git_root(&root, env)));
                if accepted {
                    shells.push(Candidate {
                        path: path.clone(),
                        edition: *edition,
                    });
                }
            }
        }
        let auto = [Edition::Pwsh, Edition::WindowsPowershell, Edition::Cmd]
            .iter()
            .find_map(|edition| shells.iter().position(|shell| shell.edition == *edition))
            .map(|index| (index, ShellSource::Automatic));
        Detection { shells, auto }
    }

    /// A Job Object whose closing kills the processes left in it.
    pub struct Job(HANDLE);
    unsafe impl Send for Job {}
    unsafe impl Sync for Job {}
    impl Job {
        pub fn new() -> io::Result<Self> {
            // SAFETY: anonymous job with default security.
            let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
            if handle.is_null() {
                return Err(io::Error::last_os_error());
            }
            let job = Job(handle);
            job.limit(JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE)?;
            Ok(job)
        }
        fn limit(&self, flags: u32) -> io::Result<()> {
            let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            info.BasicLimitInformation.LimitFlags = flags;
            // SAFETY: `info` has the size passed for this information class.
            let ok = unsafe {
                SetInformationJobObject(
                    self.0,
                    JobObjectExtendedLimitInformation,
                    &info as *const _ as *const _,
                    std::mem::size_of_val(&info) as u32,
                )
            };
            if ok == 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(())
            }
        }
        /// After a normal exit descendants stay, as in TS.
        pub fn keep_processes(&self) {
            let _ = self.limit(0);
        }
        /// Adds a running process by pid; its later children join too.
        pub fn assign(&self, pid: u32) -> io::Result<()> {
            // SAFETY: the process handle is closed below.
            unsafe {
                let process = OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, pid);
                if process.is_null() {
                    return Err(io::Error::last_os_error());
                }
                let ok = AssignProcessToJobObject(self.0, process);
                CloseHandle(process);
                if ok == 0 {
                    return Err(io::Error::last_os_error());
                }
            }
            Ok(())
        }
        /// Terminates every process in the job.
        pub fn terminate(&self, code: u32) -> io::Result<()> {
            // SAFETY: the job handle is live.
            if unsafe { TerminateJobObject(self.0, code) } == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        }
        pub fn active_processes(&self) -> io::Result<u32> {
            let mut info = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
            // SAFETY: `info` has the size passed for this information class.
            let ok = unsafe {
                QueryInformationJobObject(
                    self.0,
                    JobObjectBasicAccountingInformation,
                    &mut info as *mut _ as *mut _,
                    std::mem::size_of_val(&info) as u32,
                    std::ptr::null_mut(),
                )
            };
            if ok == 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(info.ActiveProcesses)
            }
        }
    }
    impl Drop for Job {
        fn drop(&mut self) {
            // SAFETY: the handle is owned and closed once.
            unsafe { CloseHandle(self.0) };
        }
    }

    /// Resumes the threads of a process created suspended.
    fn resume(pid: u32) -> io::Result<()> {
        // SAFETY: snapshot handle is closed below; entries are sized.
        unsafe {
            let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0);
            if snapshot == INVALID_HANDLE_VALUE {
                return Err(io::Error::last_os_error());
            }
            let mut entry: THREADENTRY32 = std::mem::zeroed();
            entry.dwSize = std::mem::size_of::<THREADENTRY32>() as u32;
            let mut resumed = false;
            let mut more = Thread32First(snapshot, &mut entry) != 0;
            while more {
                if entry.th32OwnerProcessID == pid {
                    let thread = OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID);
                    if !thread.is_null() {
                        resumed |= ResumeThread(thread) != u32::MAX;
                        CloseHandle(thread);
                    }
                }
                more = Thread32Next(snapshot, &mut entry) != 0;
            }
            CloseHandle(snapshot);
            if resumed {
                Ok(())
            } else {
                Err(io::Error::other("cannot resume the started process"))
            }
        }
    }

    /// Terminates `pid` and its descendants, found through parent links.
    pub fn terminate_tree(pid: u32) -> io::Result<()> {
        // SAFETY: snapshot handle is closed below; entries are sized.
        let parents = unsafe {
            let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
            if snapshot == INVALID_HANDLE_VALUE {
                return Err(io::Error::last_os_error());
            }
            let mut entry: PROCESSENTRY32W = std::mem::zeroed();
            entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
            let mut parents = Vec::new();
            let mut more = Process32FirstW(snapshot, &mut entry) != 0;
            while more {
                parents.push((entry.th32ProcessID, entry.th32ParentProcessID));
                more = Process32NextW(snapshot, &mut entry) != 0;
            }
            CloseHandle(snapshot);
            parents
        };
        let mut tree = vec![pid];
        let mut index = 0;
        while index < tree.len() {
            let parent = tree[index];
            tree.extend(
                parents
                    .iter()
                    .filter(|(child, of)| *of == parent && !tree.contains(child))
                    .map(|(child, _)| *child)
                    .collect::<Vec<_>>(),
            );
            index += 1;
        }
        for target in tree {
            // SAFETY: the process handle is closed right after use.
            unsafe {
                let process = OpenProcess(PROCESS_TERMINATE, 0, target);
                if !process.is_null() {
                    TerminateProcess(process, 1);
                    CloseHandle(process);
                }
            }
        }
        Ok(())
    }

    pub fn spawn(mut command: tokio::process::Command) -> io::Result<ProcessTree> {
        let job = Job::new()?;
        command.creation_flags(CREATE_SUSPENDED | CREATE_NO_WINDOW);
        let mut child = command.spawn()?;
        let attached = (|| {
            let handle = child
                .raw_handle()
                .ok_or_else(|| io::Error::other("started process has no handle"))?;
            // SAFETY: both handles are live.
            if unsafe { AssignProcessToJobObject(job.0, handle as HANDLE) } == 0 {
                return Err(io::Error::last_os_error());
            }
            resume(child.id().unwrap_or_default())
        })();
        if let Err(error) = attached {
            let _ = child.start_kill();
            return Err(error);
        }
        Ok(ProcessTree {
            child,
            reaped: false,
            terminating: false,
            terminated: false,
            job,
            temporary: None,
        })
    }

    pub async fn terminate(tree: &mut ProcessTree) -> io::Result<()> {
        tree.job.terminate(1)?;
        tree.child.wait().await?;
        tree.reaped = true;
        let deadline = tokio::time::Instant::now() + TERMINATE_CONFIRM;
        while tree.job.active_processes()? > 0 {
            if tokio::time::Instant::now() >= deadline {
                return Err(io::Error::other("job processes are still alive"));
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        Ok(())
    }
}

/// Terminal dimensions for PTY spawn.
#[derive(Debug, Clone, Copy)]
pub struct PtySize {
    pub cols: u16,
    pub rows: u16,
}

/// A child process running under a PTY master.
pub struct PtyProcessTree {
    child: Box<dyn portable_pty::Child + Send>,
    master: Box<dyn portable_pty::MasterPty + Send>,
    reader: Option<Box<dyn std::io::Read + Send>>,
    writer: Box<dyn std::io::Write + Send>,
    #[cfg(unix)]
    pid: u32,
    #[cfg(windows)]
    job: native::Job,
    terminating: bool,
    terminated: bool,
    reaped: bool,
    temporary: Option<TemporaryDirectory>,
}

impl PtyProcessTree {
    /// Keeps the shell's isolated TMPDIR until this handle is dropped.
    pub(crate) fn hold(&mut self, temporary: Option<TemporaryDirectory>) {
        self.temporary = temporary;
    }
    /// Resolve a job reported by this PTY into the host's process identity.
    pub(crate) fn job_pid(&self, reported: u32) -> io::Result<u32> {
        if reported == 0 || (cfg!(unix) && reported > i32::MAX as u32) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Invalid job pid",
            ));
        }
        #[cfg(not(target_os = "linux"))]
        {
            Ok(reported)
        }
        #[cfg(target_os = "linux")]
        {
            let foreground = self
                .master
                .process_group_leader()
                .ok_or_else(|| io::Error::other("PTY foreground identity was not reported"))?;
            let namespace = std::fs::read_link(format!("/proc/{foreground}/ns/pid"))?;
            let owner = std::fs::read_to_string(format!("/proc/{foreground}/status"))?;
            let session = owner
                .lines()
                .find_map(|line| line.strip_prefix("NSsid:"))
                .and_then(|ids| ids.split_whitespace().next())
                .ok_or_else(|| io::Error::other("PTY session identity was not reported"))?;
            let expected = reported.to_string();
            for entry in std::fs::read_dir("/proc")?.flatten() {
                let Some(pid) = entry
                    .file_name()
                    .to_str()
                    .and_then(|pid| pid.parse::<u32>().ok())
                else {
                    continue;
                };
                if std::fs::read_link(entry.path().join("ns/pid"))
                    .ok()
                    .as_ref()
                    != Some(&namespace)
                {
                    continue;
                }
                let Ok(status) = std::fs::read_to_string(entry.path().join("status")) else {
                    continue;
                };
                if status
                    .lines()
                    .find_map(|line| line.strip_prefix("NSsid:"))
                    .and_then(|ids| ids.split_whitespace().next())
                    != Some(session)
                {
                    continue;
                }
                let Some(ids) = status.lines().find_map(|line| line.strip_prefix("NSpid:")) else {
                    continue;
                };
                let mut ids = ids.split_whitespace();
                if ids.next_back() == Some(expected.as_str()) {
                    return Ok(pid);
                }
            }
            Err(io::Error::new(
                io::ErrorKind::NotFound,
                "Job is not present in this PTY session",
            ))
        }
    }

    /// Takes the reader half for a dedicated reader thread; only the
    /// writer stays with the handle.
    pub fn take_reader(&mut self) -> Box<dyn std::io::Read + Send> {
        self.reader.take().expect("pty reader already taken")
    }
    /// Writes bytes to the PTY master.
    pub fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.writer.write_all(bytes)
    }
    /// Resizes the PTY.
    pub fn resize(&self, size: PtySize) -> io::Result<()> {
        self.master
            .resize(portable_pty::PtySize {
                rows: size.rows,
                cols: size.cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(io::Error::other)
    }
    /// Waits for the root process; cancel safe.
    pub fn wait(&mut self) -> io::Result<portable_pty::ExitStatus> {
        self.child.wait().inspect(|_| {
            self.reaped = true;
        })
    }
    /// The process exit status if it has already exited.
    pub fn try_wait(&mut self) -> io::Result<Option<portable_pty::ExitStatus>> {
        self.child.try_wait().inspect(|status| {
            if status.is_some() {
                self.reaped = true;
            }
        })
    }
    /// Terminates every process of the tree and confirms that none is left.
    pub async fn terminate(&mut self) -> io::Result<()> {
        if !self.terminated {
            self.terminating = true;
            #[cfg(unix)]
            {
                native::kill_tree(self.pid);
                let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
                while tokio::time::Instant::now() < deadline {
                    match self.try_wait()? {
                        Some(_) => break,
                        None => tokio::time::sleep(std::time::Duration::from_millis(20)).await,
                    }
                }
            }
            #[cfg(windows)]
            {
                self.job.terminate(1)?;
                let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
                while tokio::time::Instant::now() < deadline {
                    if self.job.active_processes()? == 0 {
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                }
            }
            self.terminated = true;
        }
        Ok(())
    }
    /// The root exited on its own and was reaped: its descendants stay.
    fn released(&self) -> bool {
        self.reaped && !self.terminating
    }
}

#[cfg(unix)]
impl Drop for PtyProcessTree {
    fn drop(&mut self) {
        if !self.terminated && !self.released() {
            native::kill_tree(self.pid);
        }
    }
}
#[cfg(windows)]
impl Drop for PtyProcessTree {
    fn drop(&mut self) {
        if self.released() {
            self.job.keep_processes();
        }
    }
}

/// Terminates a background job by its root pid: the process group it leads
/// on POSIX, the process tree under it on Windows.
pub fn terminate_job(pid: u32) -> io::Result<()> {
    #[cfg(unix)]
    {
        // SAFETY: plain syscall on the addressed group.
        if unsafe { libc::kill(-(pid as libc::pid_t), libc::SIGTERM) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
    #[cfg(windows)]
    native::terminate_tree(pid)
}

async fn observe_job_release(
    record: Arc<TaskRecord>,
    job: Arc<shell::ShellJob>,
    pid: u32,
    shutdown: CancellationToken,
    tree: Option<Vec<u32>>,
) {
    loop {
        if record.is_terminal() {
            return;
        }
        if job_released(pid, tree.as_deref()).await {
            record.finish(Terminal {
                status: TaskStatus::Cancelled,
                exit_code: None,
                signal: None,
                duration_ms: record.started.elapsed().as_millis() as u64,
                failure: Some(TaskFailure::TerminateFailed {
                    message: "job release was not confirmed at stop; it ended later".into(),
                }),
                output: job.output_record().0,
                note: None,
            });
            return;
        }
        tokio::select! {
            _ = shutdown.cancelled() => return,
            _ = tokio::time::sleep(Duration::from_millis(200)) => {}
        }
    }
}

/// Kills a background job that outlived its shell and confirms it is gone:
/// the process group it leads on POSIX, the process tree under it on
/// Windows.
pub async fn kill_job(pid: u32, tree: Option<Vec<u32>>) -> io::Result<()> {
    #[cfg(unix)]
    {
        let _ = tree;
        native::kill_tree(pid);
        wait_job_exit(pid).await
    }
    #[cfg(windows)]
    {
        native::terminate_tree(pid)?;
        let Some(tree) = tree else {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "job tree could not be enumerated",
            ));
        };
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        while tree.iter().copied().any(native::process_alive) {
            if tokio::time::Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "job processes are still alive",
                ));
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        Ok(())
    }
}

pub(crate) fn job_tree(pid: u32) -> Option<Vec<u32>> {
    #[cfg(unix)]
    {
        let _ = pid;
        None
    }
    #[cfg(windows)]
    {
        native::tree_pids(pid)
    }
}

async fn job_released(pid: u32, tree: Option<&[u32]>) -> bool {
    #[cfg(unix)]
    {
        let _ = tree;
        wait_job_exit(pid).await.is_ok()
    }
    #[cfg(windows)]
    {
        tree.is_some_and(|tree| tree.iter().copied().all(|pid| !native::process_alive(pid)))
    }
}

#[cfg(unix)]
async fn wait_job_exit(pid: u32) -> io::Result<()> {
    let group = -(pid as libc::pid_t);
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        // SAFETY: signal 0 only probes the addressed group.
        if unsafe { libc::kill(group, 0) } != 0
            && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
        {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "job processes are still alive",
            ));
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

fn available_shell(
    path: PathBuf,
    edition: ShellEdition,
    source: ShellSource,
    output_encoding: OutputEncoding,
) -> ShellStatus {
    match path.to_str() {
        Some(path) => ShellStatus::Available(ExecShellRuntime {
            executable_path: path.to_owned(),
            edition,
            source,
            output_encoding,
        }),
        None => ShellStatus::Unavailable {
            code: EXEC_SHELL_UNAVAILABLE,
            reason: "The shell path is not UTF-8".into(),
        },
    }
}

impl HostPlatform {
    fn fallback_persistent_shell(&self) -> ShellStatus {
        if cfg!(windows) {
            for name in ["pwsh.exe", "powershell.exe"] {
                if let Some(path) = find_executable(name) {
                    let edition = if name == "pwsh.exe" {
                        ShellEdition::Pwsh
                    } else {
                        ShellEdition::WindowsPowershell
                    };
                    return available_shell(
                        path,
                        edition,
                        ShellSource::Fallback,
                        match edition {
                            ShellEdition::Pwsh => OutputEncoding::Utf8,
                            _ => OutputEncoding::Unknown,
                        },
                    );
                }
            }
            ShellStatus::Unavailable {
                code: EXEC_SHELL_UNAVAILABLE,
                reason: "No supported persistent shell found.".into(),
            }
        } else {
            if let Some(path) = find_executable("bash") {
                return available_shell(
                    path,
                    ShellEdition::Bash,
                    ShellSource::Fallback,
                    OutputEncoding::Utf8,
                );
            }
            ShellStatus::Unavailable {
                code: EXEC_SHELL_UNAVAILABLE,
                reason: "No supported persistent shell found.".into(),
            }
        }
    }

    /// Starts `spec` as the root of a PTY process tree.
    pub fn spawn_pty(&self, spec: ProcessSpec, size: PtySize) -> io::Result<PtyProcessTree> {
        let pty_system = portable_pty::native_pty_system();
        let pty = pty_system
            .openpty(portable_pty::PtySize {
                rows: size.rows,
                cols: size.cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(io::Error::other)?;
        let mut cmd = portable_pty::CommandBuilder::new(&spec.executable);
        cmd.args(&spec.args);
        cmd.cwd(&spec.cwd);
        cmd.env_clear();
        for (key, value) in &spec.env {
            cmd.env(key, value);
        }
        let child = pty.slave.spawn_command(cmd).map_err(io::Error::other)?;
        let reader = pty.master.try_clone_reader().map_err(io::Error::other)?;
        let writer = pty.master.take_writer().map_err(io::Error::other)?;
        #[cfg(unix)]
        let pid = child.process_id().unwrap_or(0);
        #[cfg(windows)]
        let job = native::Job::new()?;
        #[cfg(windows)]
        job.assign(child.process_id().unwrap_or(0))?;
        Ok(PtyProcessTree {
            child,
            master: pty.master,
            reader: Some(reader),
            writer,
            #[cfg(unix)]
            pid,
            #[cfg(windows)]
            job,
            terminating: false,
            terminated: false,
            reaped: false,
            temporary: None,
        })
    }

    /// Removes `path`, one persistent shell output directory below
    /// [`exec_root`](Self::exec_root), and syncs its parent. An absent
    /// directory counts as removed. The caller has decided with the
    /// ownership rows that no Session still needs it (ADR 013 D4).
    pub fn remove_exec_directory(&self, path: &Path) -> io::Result<()> {
        if !path.is_absolute()
            || path
                .components()
                .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "not a persistent shell output directory",
            ));
        }
        let root =
            std::fs::canonicalize(self.exec_root_path()).unwrap_or_else(|_| self.exec_root_path());
        let input_root = self.home.join(".dim").join("exec");
        let target = canonicalize_existing_prefix(path)?;
        if target == root || !target.starts_with(&root) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "not a persistent shell output directory",
            ));
        }
        let mut current = root.clone();
        for component in path
            .strip_prefix(&input_root)
            .or_else(|_| path.strip_prefix(&root))
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "outside the exec root"))?
            .components()
        {
            current.push(component);
            match std::fs::symlink_metadata(&current) {
                Ok(metadata) if metadata.file_type().is_symlink() => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "a symlink component is not a persistent shell output directory",
                    ));
                }
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => break,
                Err(error) => return Err(error),
            }
        }
        match std::fs::remove_dir_all(&target) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        #[cfg(unix)]
        if let Some(parent) = target.parent() {
            std::fs::File::open(parent)?.sync_all()?;
        }
        Ok(())
    }
}

fn valid_shell_marker(marker: &str) -> bool {
    let Some(rest) = marker
        .strip_prefix("__DIM_START_")
        .or_else(|| marker.strip_prefix("__DIM_END_"))
    else {
        return false;
    };
    let Some((first, rest)) = rest.split_once('_') else {
        return false;
    };
    let Some((second, tail)) = rest.split_once("__") else {
        return false;
    };
    tail.is_empty()
        && first.len() == 8
        && second.len() == 8
        && first.bytes().all(|byte| byte.is_ascii_hexdigit())
        && second.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn canonicalize_existing_prefix(path: &Path) -> io::Result<PathBuf> {
    let mut missing = Vec::new();
    let mut current = path;
    loop {
        match std::fs::canonicalize(current) {
            Ok(mut base) => {
                for part in missing.iter().rev() {
                    base.push(part);
                }
                return Ok(base);
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let Some(name) = current.file_name() else {
                    return Err(error);
                };
                missing.push(name.to_owned());
                current = current.parent().ok_or(error)?;
            }
            Err(error) => return Err(error),
        }
    }
}

fn find_executable(name: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for entry in std::env::split_paths(&path_var) {
        let candidate = entry.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

impl HostPlatform {
    pub(crate) async fn retained_output_page(
        &self,
        path: &std::path::Path,
        cursor: u64,
        limit: usize,
    ) -> Result<
        crate::workspace_tools::background::OutputPage,
        crate::workspace_tools::background::OutputPageError,
    > {
        use crate::host_platform::output::OUTPUT_FILE_BYTE_LIMIT;
        use crate::workspace_tools::background::{
            OutputPageError, PAGE_CHAR_TRAILING_BYTES, decode_page,
        };
        use std::io::SeekFrom;
        use tokio::io::{AsyncReadExt, AsyncSeekExt};
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
        let metadata = file.metadata().await.ok();
        let truncated = metadata
            .as_ref()
            .is_some_and(|m| m.len() >= OUTPUT_FILE_BYTE_LIMIT);
        let mut page = decode_page(&buffer[..read], cursor, limit, truncated)?;
        page.total_bytes = metadata.map(|m| m.len()).unwrap_or(0);
        Ok(page)
    }
}

/// The name of the signal that ended a process, as `exec` reports it.
#[cfg(unix)]
pub fn signal_name(status: ExitStatus) -> Option<String> {
    use std::os::unix::process::ExitStatusExt;
    let signal = status.signal()?;
    let name = match signal {
        libc::SIGHUP => "SIGHUP",
        libc::SIGINT => "SIGINT",
        libc::SIGQUIT => "SIGQUIT",
        libc::SIGILL => "SIGILL",
        libc::SIGTRAP => "SIGTRAP",
        libc::SIGABRT => "SIGABRT",
        libc::SIGBUS => "SIGBUS",
        libc::SIGFPE => "SIGFPE",
        libc::SIGKILL => "SIGKILL",
        libc::SIGUSR1 => "SIGUSR1",
        libc::SIGSEGV => "SIGSEGV",
        libc::SIGUSR2 => "SIGUSR2",
        libc::SIGPIPE => "SIGPIPE",
        libc::SIGALRM => "SIGALRM",
        libc::SIGTERM => "SIGTERM",
        _ => return Some(format!("SIG{signal}")),
    };
    Some(name.into())
}
#[cfg(windows)]
pub fn signal_name(_: ExitStatus) -> Option<String> {
    None
}

pub(crate) mod shell {
    use super::output::bounded_preview;
    use super::output::{Decoder, head_bytes, tail_bytes};
    use super::{
        ExecShellRuntime, HostPlatform, ProcessSpec, ProcessSpecError, PtyProcessTree, PtySize,
        ShellEdition, ShellStatus,
    };
    use super::{TaskKind, TaskRecord, terminate_job};
    use crate::workspace_tools::background::{
        OutputPageError, PAGE_CHAR_TRAILING_BYTES, decode_page,
    };
    use std::{
        collections::VecDeque,
        io::{self, Read, Write},
        path::{Path, PathBuf},
        sync::{Arc, Mutex, Weak},
        time::{Duration, Instant},
    };
    use tokio::sync::{Mutex as AsyncMutex, Notify, watch};
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
        events: Mutex<ShellEvents>,
        active_command: Mutex<Option<Weak<super::TaskRecord>>>,
        children: Mutex<Vec<Weak<super::TaskRecord>>>,
        event_notify: Arc<Notify>,
        kill: CancellationToken,
        path: PathBuf,
        edition: super::ShellEdition,
    }

    pub(crate) struct ShellCommand {
        pub(crate) shell: Arc<ShellTask>,
        start_marker: String,
        end_marker: String,
        path: PathBuf,
        state: Mutex<ShellCommandState>,
    }

    struct ShellCommandState {
        started: bool,
        code: Option<String>,
        output: String,
        bytes: u64,
        omitted: usize,
        tail: String,
        file: Option<std::fs::File>,
    }

    impl ShellCommand {
        pub(crate) fn new(
            shell: Arc<ShellTask>,
            start_marker: String,
            end_marker: String,
            path: PathBuf,
        ) -> Self {
            Self {
                shell,
                start_marker,
                end_marker,
                path,
                state: Mutex::new(ShellCommandState {
                    started: false,
                    code: None,
                    output: String::new(),
                    bytes: 0,
                    omitted: 0,
                    tail: String::new(),
                    file: None,
                }),
            }
        }

        fn feed(&self, line: &str) -> Option<String> {
            let mut state = self.state.lock().unwrap();
            let mut rest = line;
            if !state.started {
                let position = rest.find(&self.start_marker)?;
                state.started = true;
                rest = &rest[position + self.start_marker.len()..];
            }
            if state.file.is_none() {
                let mut options = std::fs::OpenOptions::new();
                options.create_new(true).append(true);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    options.mode(0o600);
                }
                if let Ok(file) = options.open(&self.path) {
                    state.file = Some(file);
                }
            }
            let Some(position) = rest.find(&self.end_marker) else {
                if last_full_marker(rest).is_some() {
                    return None;
                }
                append_command_output(
                    &mut state,
                    &crate::shell_session::regularize_output(rest.as_bytes()),
                );
                return None;
            };
            append_command_output(
                &mut state,
                &crate::shell_session::regularize_output(&rest.as_bytes()[..position]),
            );
            let code = rest[position + self.end_marker.len()..].trim().to_owned();
            state.code = Some(code.clone());
            Some(code)
        }

        pub(crate) fn output_path(&self) -> &Path {
            &self.path
        }

        pub(crate) fn output_record(&self) -> super::OutputRecord {
            let state = self.state.lock().unwrap();
            super::OutputRecord {
                path: self.path.to_string_lossy().into_owned(),
                bytes: state.bytes,
                truncated: state.omitted > 0,
                text: None,
                preview: state.tail.clone(),
                preview_truncated: state.omitted > 0,
            }
        }

        pub(crate) fn tail(&self) -> crate::workspace_tools::exec::OutputTail {
            let state = self.state.lock().unwrap();
            crate::workspace_tools::exec::OutputTail {
                text: state.tail.clone(),
                total_bytes: state.bytes,
                truncated: state.omitted > 0,
            }
        }

        pub(crate) fn page(
            &self,
            cursor: u64,
            limit: usize,
        ) -> Result<super::OutputPageReply, OutputPageError> {
            let state = self.state.lock().unwrap();
            let bytes = state.output.as_bytes();
            let start = (cursor as usize).min(bytes.len());
            let end = (start + limit + PAGE_CHAR_TRAILING_BYTES).min(bytes.len());
            let page = decode_page(&bytes[start..end], cursor, limit, state.omitted > 0)?;
            Ok(super::OutputPageReply {
                text: page.text,
                bytes: page.bytes,
                total_bytes: state.bytes,
                next_cursor: page.next_cursor,
                truncated: state.omitted > 0,
                retained_from: 0,
                reset: None,
            })
        }
    }

    fn append_command_output(state: &mut ShellCommandState, text: &str) {
        if text.is_empty() {
            return;
        }
        if let Some(file) = state.file.as_mut() {
            use std::io::Write;
            let _ = file.write_all(text.as_bytes());
        }
        state.bytes += text.len() as u64;
        state.tail.push_str(text);
        if state.tail.len() > 16_000 {
            state.tail = tail_bytes(&state.tail, 16_000).to_owned();
        }
        let room = (64 * 1024usize).saturating_sub(state.output.len());
        if text.len() <= room {
            state.output.push_str(text);
        } else {
            let kept = head_bytes(text, room);
            state.output.push_str(kept);
            state.omitted += text.len() - kept.len();
        }
    }

    pub(crate) struct ShellJob {
        pub(crate) shell: Weak<ShellTask>,
        pub(crate) pid: watch::Sender<Option<u32>>,
        pub(crate) end_marker: String,
        pub(crate) output_path: PathBuf,
        pub(crate) completion: Option<(PathBuf, PathBuf)>,
    }

    impl ShellJob {
        pub(crate) fn output_record(&self) -> (super::OutputRecord, Option<super::TaskFailure>) {
            use std::io::{Read, Seek, SeekFrom};
            let bytes = std::fs::metadata(&self.output_path)
                .map(|metadata| metadata.len())
                .unwrap_or(0);
            let (text, failure) = std::fs::File::open(&self.output_path).map_or_else(
                |error| {
                    (
                        String::new(),
                        Some(super::TaskFailure::JobUnavailable {
                            message: error.to_string(),
                        }),
                    )
                },
                |mut file| {
                    let window = 64 * 1024;
                    if bytes > window {
                        let _ = file.seek(SeekFrom::End(-(window as i64)));
                    }
                    let mut tail = Vec::new();
                    let read = file.read_to_end(&mut tail);
                    let mut decoder = Decoder::default();
                    let mut text = decoder.write(&tail);
                    text.push_str(&decoder.end());
                    let failure = read.err().map(|error| super::TaskFailure::JobUnavailable {
                        message: error.to_string(),
                    });
                    (text, failure)
                },
            );
            let (preview, preview_truncated) = bounded_preview(&text, bytes as usize);
            (
                super::OutputRecord {
                    path: self.output_path.to_string_lossy().into_owned(),
                    bytes,
                    truncated: false,
                    text: None,
                    preview,
                    preview_truncated,
                },
                failure,
            )
        }
    }

    const EVENT_RETAIN_CHARS: usize = 1024 * 1024;

    fn parse_shell_job(line: &str) -> Option<(String, u32)> {
        let (prefix, tail) = last_full_marker(line)?;
        if prefix != "__DIM_END_" {
            return None;
        }
        let mut parts = tail.split_whitespace();
        let pid = parts.next()?.parse().ok()?;
        let task_id = parts.next()?;
        Some((task_id.to_owned(), pid))
    }

    fn shell_event_len(event: &super::ShellEvent) -> usize {
        match event {
            super::ShellEvent::Output { text, .. } => text.len(),
            super::ShellEvent::Marker { line, .. } => line.len(),
        }
    }

    fn last_full_marker(line: &str) -> Option<(&'static str, &str)> {
        let mut found: Option<(usize, (&'static str, &str))> = None;
        for (prefix, position) in [
            ("__DIM_START_", line.rfind("__DIM_START_")),
            ("__DIM_END_", line.rfind("__DIM_END_")),
        ] {
            let Some(position) = position else {
                continue;
            };
            if found.is_some_and(|(other, _)| other > position) {
                continue;
            }
            let rest = &line[position + prefix.len()..];
            let Some((first, rest)) = rest.split_once('_') else {
                continue;
            };
            let Some((second, tail)) = rest.split_once("__") else {
                continue;
            };
            if first.len() == 8
                && second.len() == 8
                && first.bytes().all(|byte| byte.is_ascii_hexdigit())
                && second.bytes().all(|byte| byte.is_ascii_hexdigit())
            {
                found = Some((position, (prefix, tail)));
            }
        }
        found.map(|(_, marker)| marker)
    }

    #[derive(Default)]
    struct ShellEvents {
        items: VecDeque<super::ShellEvent>,
        pending: String,
        base: u64,
        next: u64,
        chars: usize,
    }

    impl ShellEvents {
        fn push(&mut self, text: String) {
            self.pending.push_str(&text);
            while let Some(newline) = self.pending.find('\n') {
                let line: String = self.pending.drain(..=newline).collect();
                let cursor = self.next;
                self.next += 1;
                self.chars += line.len();
                if last_full_marker(&line).is_some() {
                    self.items
                        .push_back(super::ShellEvent::Marker { cursor, line });
                } else {
                    self.items
                        .push_back(super::ShellEvent::Output { cursor, text: line });
                }
            }
            if self.pending.len() > 65536 {
                let cursor = self.next;
                self.next += 1;
                let text = std::mem::take(&mut self.pending);
                self.chars += text.len();
                self.items
                    .push_back(super::ShellEvent::Output { cursor, text });
            }
        }

        fn trim(&mut self) {
            let mut removed = false;
            while self.chars > EVENT_RETAIN_CHARS {
                let Some(front) = self.items.pop_front() else {
                    break;
                };
                self.chars = self.chars.saturating_sub(shell_event_len(&front));
                removed = true;
            }
            if removed {
                self.base = self
                    .items
                    .front()
                    .map(super::ShellEvent::cursor)
                    .unwrap_or(self.next);
            }
        }

        fn finish(&mut self) {
            if self.pending.is_empty() {
                return;
            }
            let cursor = self.next;
            self.next += 1;
            let text = std::mem::take(&mut self.pending);
            self.chars += text.len();
            self.items
                .push_back(super::ShellEvent::Output { cursor, text });
        }

        fn page(&mut self, cursor: u64, limit: usize) -> EventPage {
            while let Some(front) = self.items.front() {
                if front.cursor() >= cursor {
                    break;
                }
                if let Some(front) = self.items.pop_front() {
                    self.chars = self.chars.saturating_sub(shell_event_len(&front));
                    self.base = front.cursor() + 1;
                }
            }
            let reset = cursor < self.base;
            let mut events = Vec::new();
            let mut used = 0;
            for event in self.items.iter().filter(|event| event.cursor() >= cursor) {
                let length = shell_event_len(event);
                if !events.is_empty() && used + length > limit {
                    break;
                }
                used += length;
                events.push(event.clone());
                if used >= limit {
                    break;
                }
            }
            let next_cursor = events
                .last()
                .map(|event| event.cursor() + 1)
                .unwrap_or(cursor.max(self.base));
            EventPage {
                events,
                next_cursor,
                reset,
            }
        }
    }

    pub(crate) struct EventPage {
        pub(crate) events: Vec<super::ShellEvent>,
        pub(crate) next_cursor: u64,
        pub(crate) reset: bool,
    }

    #[derive(Default)]
    struct OutputFilter {
        decoder: Decoder,
    }

    impl OutputFilter {
        fn new() -> Self {
            Self {
                decoder: Decoder::default(),
            }
        }

        fn push(&mut self, bytes: &[u8]) -> (String, String) {
            let raw = self.decoder.write(bytes);
            let display = crate::shell_session::regularize_output(raw.as_bytes());
            (raw, display)
        }

        fn finish(&mut self) -> (String, String) {
            let raw = self.decoder.end();
            let display = crate::shell_session::regularize_output(raw.as_bytes());
            (raw, display)
        }
    }

    #[derive(Default)]
    struct LineSplitter {
        pending: String,
        finished: bool,
    }

    impl LineSplitter {
        fn push(&mut self, chunk: &str) -> Vec<String> {
            self.pending.push_str(chunk);
            let mut lines = Vec::new();
            while let Some(newline) = self.pending.find('\n') {
                lines.push(self.pending.drain(..=newline).collect());
            }
            if self.pending.len() > 64 * 1024 {
                lines.push(std::mem::take(&mut self.pending));
            }
            lines
        }

        fn finish(&mut self) -> Option<String> {
            if self.finished {
                return None;
            }
            self.finished = true;
            (!self.pending.is_empty()).then(|| std::mem::take(&mut self.pending))
        }
    }

    fn classify_line(
        line: &str,
        pump: &Arc<ShellTask>,
        jobs: &Arc<Mutex<std::collections::HashMap<String, Arc<TaskRecord>>>>,
    ) {
        let active = pump.active_command.lock().unwrap().clone();
        if let Some(record) = active.and_then(|weak| weak.upgrade())
            && let TaskKind::ShellCommand(command) = &record.kind
            && let Some(code) = command.feed(line)
        {
            record.finish(shell_command_terminal(&record, command, code));
            pump.clear_active(&record);
        }
        if let Some((task_id, reported)) = parse_shell_job(line) {
            let pid = pump
                .pty
                .blocking_lock()
                .as_ref()
                .and_then(|pty| pty.job_pid(reported).ok());
            if let Some(pid) = pid {
                let jobs = jobs.lock().unwrap();
                if let Some(record) = jobs.get(&task_id)
                    && let TaskKind::Job(job) = &record.kind
                    && job
                        .shell
                        .upgrade()
                        .is_some_and(|shell| Arc::ptr_eq(&shell, pump))
                {
                    job.pid.send_replace(Some(pid));
                }
            }
        }
        {
            let jobs = jobs.lock().unwrap();
            for record in jobs.values() {
                if record.is_terminal() {
                    continue;
                }
                let TaskKind::Job(job) = &record.kind else {
                    continue;
                };
                if !job
                    .shell
                    .upgrade()
                    .is_some_and(|shell| Arc::ptr_eq(&shell, pump))
                {
                    continue;
                }
                if line.contains(&job.end_marker) {
                    let code = last_full_marker(line)
                        .map(|(_, tail)| tail.trim().to_owned())
                        .filter(|code| !code.is_empty());
                    record.finish(job_terminal(record, job, code.as_deref()));
                }
            }
        }
    }

    impl HostPlatform {
        pub(crate) async fn start_shell(
            &self,
            id: &str,
            cwd: &Path,
            roots: &[PathBuf],
            directory: &Path,
            size: PtySize,
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
                executable: PathBuf::from(&runtime.executable_path),
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
            let (shell, record, mut file) = {
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
                std::fs::create_dir_all(directory).map_err(|error| ProcessSpecError {
                    code: "io",
                    reason: error.to_string(),
                })?;
                let path = directory.join(format!("{id}.log"));
                let mut options = std::fs::OpenOptions::new();
                options.create_new(true).append(true);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    options.mode(0o600);
                }
                let file = options.open(&path).map_err(|error| ProcessSpecError {
                    code: if error.kind() == io::ErrorKind::AlreadyExists {
                        "task_exists"
                    } else {
                        "io"
                    },
                    reason: error.to_string(),
                })?;
                let shell = Arc::new(ShellTask {
                    pty: AsyncMutex::new(None),
                    capture: Mutex::default(),
                    events: Mutex::default(),
                    active_command: Mutex::new(None),
                    children: Mutex::new(Vec::new()),
                    event_notify: Arc::new(Notify::new()),
                    kill: CancellationToken::new(),
                    path,
                    edition: runtime.edition,
                });
                let record = TaskRecord::new(id.to_owned(), TaskKind::Shell(shell.clone()));
                commands.insert(id.to_owned(), record.clone());
                (shell, record, file)
            };
            let mut pty = match self.spawn_pty(spec, size) {
                Ok(pty) => pty,
                Err(error) => {
                    let _ = std::fs::remove_file(&shell.path);
                    self.commands.lock().unwrap().remove(id);
                    return Err(ProcessSpecError {
                        code: "failed",
                        reason: error.to_string(),
                    });
                }
            };
            pty.hold(temporary);
            let mut reader = pty.take_reader();
            *shell.pty.lock().await = Some(pty);
            record.start();
            let jobs = self.commands.clone();
            let shell_record = record.clone();
            let pump = Arc::downgrade(&shell);
            let (drained, reader_finished) = tokio::sync::oneshot::channel();
            std::thread::spawn(move || {
                let mut bytes = [0; 4096];
                let mut filter = OutputFilter::new();
                let mut written = 0usize;
                let mut splitter = LineSplitter::default();
                while let Ok(n) = reader.read(&mut bytes) {
                    if n == 0 {
                        break;
                    }
                    let Some(pump) = pump.upgrade() else {
                        break;
                    };
                    let (raw, chunk) = filter.push(&bytes[..n]);
                    for line in splitter.push(&raw) {
                        classify_line(&line, &pump, &jobs);
                    }
                    let mut capture = pump.capture.lock().unwrap();
                    if shell_record.is_terminal() {
                        break;
                    }
                    capture.total += chunk.len() as u64;
                    capture.text.push_str(&chunk);
                    if capture.text.len() > RETAINED {
                        let dropped =
                            capture.text.len() - tail_bytes(&capture.text, RETAINED).len();
                        capture.text.drain(..dropped);
                    }
                    let room = (crate::host_platform::output::OUTPUT_FILE_BYTE_LIMIT as usize)
                        .saturating_sub(written);
                    if room > 0 {
                        let take = chunk.len().min(room);
                        if file.write_all(&chunk.as_bytes()[..take]).is_ok() {
                            written += take;
                        }
                    }
                    drop(capture);
                    {
                        let mut events = pump.events.lock().unwrap();
                        events.trim();
                        if !shell_record.is_terminal() {
                            events.push(chunk);
                        }
                    }
                    pump.event_notify.notify_waiters();
                }
                let (raw_tail, tail) = filter.finish();
                if !raw_tail.is_empty() {
                    for line in splitter.push(&raw_tail) {
                        if let Some(pump) = pump.upgrade() {
                            classify_line(&line, &pump, &jobs);
                        }
                    }
                }
                if !tail.is_empty()
                    && let Some(pump) = pump.upgrade()
                {
                    let mut capture = pump.capture.lock().unwrap();
                    capture.total += tail.len() as u64;
                    capture.text.push_str(&tail);
                    if capture.text.len() > RETAINED {
                        let dropped =
                            capture.text.len() - tail_bytes(&capture.text, RETAINED).len();
                        capture.text.drain(..dropped);
                    }
                    let room = (crate::host_platform::output::OUTPUT_FILE_BYTE_LIMIT as usize)
                        .saturating_sub(written);
                    if room > 0 {
                        let take = tail.len().min(room);
                        let _ = file.write_all(&tail.as_bytes()[..take]);
                    }
                    drop(capture);
                    let mut events = pump.events.lock().unwrap();
                    events.trim();
                    events.push(tail);
                }
                if let Some(pump) = pump.upgrade() {
                    if let Some(line) = splitter.finish() {
                        classify_line(&line, &pump, &jobs);
                    }
                    pump.events.lock().unwrap().finish();
                }
                let _ = drained.send(());
            });
            let worker = shell.clone();
            let worker_record = record.clone();
            let shutdown = self.command_shutdown.clone();
            self.command_workers.spawn(async move {
                let mut poll = tokio::time::interval(Duration::from_millis(100));
                let (exit, status, error) = loop {
                    let stop = tokio::select! {
                        biased;
                        _=poll.tick()=>None,
                        _=worker.kill.cancelled()=>Some("cancelled"),
                        _=shutdown.cancelled()=>Some("unloaded"),
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
                    crate::host_platform::output::EXIT_STDIO_GRACE,
                    reader_finished,
                )
                .await;
                let terminal =
                    shell_terminal(&worker_record, worker.output_record(), exit, status, error);
                worker_record.finish(terminal);
            });
            Ok((shell, runtime))
        }
    }

    impl ShellTask {
        pub(crate) fn output_path(&self) -> &Path {
            &self.path
        }

        pub(crate) fn pwsh(&self) -> bool {
            matches!(
                self.edition,
                super::ShellEdition::Pwsh | super::ShellEdition::WindowsPowershell
            )
        }

        pub(crate) fn reserve_active_command(&self, command: &Arc<super::TaskRecord>) -> bool {
            let mut active = self.active_command.lock().unwrap();
            if active
                .as_ref()
                .and_then(Weak::upgrade)
                .is_some_and(|current| !current.is_terminal())
            {
                return false;
            }
            *active = Some(Arc::downgrade(command));
            true
        }

        pub(crate) fn set_active_command(&self, command: Option<Arc<super::TaskRecord>>) {
            *self.active_command.lock().unwrap() = command.map(|command| Arc::downgrade(&command));
        }

        pub(crate) fn is_active(&self, record: &Arc<super::TaskRecord>) -> bool {
            self.active_command
                .lock()
                .unwrap()
                .as_ref()
                .and_then(Weak::upgrade)
                .is_some_and(|current| Arc::ptr_eq(&current, record))
        }

        pub(crate) fn clear_active(&self, record: &super::TaskRecord) {
            let mut active = self.active_command.lock().unwrap();
            if active
                .as_ref()
                .and_then(Weak::upgrade)
                .is_some_and(|current| std::ptr::eq(Arc::as_ptr(&current), record))
            {
                *active = None;
            }
        }

        pub(crate) fn add_child(&self, record: &Arc<super::TaskRecord>) {
            self.children.lock().unwrap().push(Arc::downgrade(record));
        }

        pub(crate) fn children_list(&self) -> Vec<Arc<super::TaskRecord>> {
            self.children
                .lock()
                .unwrap()
                .iter()
                .filter_map(Weak::upgrade)
                .collect()
        }

        pub(crate) fn sweep(&self, shell_terminal: Option<Arc<super::Terminal>>) {
            let failure = match &shell_terminal {
                Some(terminal)
                    if matches!(&terminal.failure, Some(super::TaskFailure::OwnerUnloaded)) =>
                {
                    super::TaskFailure::OwnerUnloaded
                }
                Some(terminal) if terminal.status == super::TaskStatus::Cancelled => {
                    super::TaskFailure::ShellReset
                }
                _ => super::TaskFailure::ShellExited,
            };
            let note = shell_terminal
                .as_ref()
                .map(
                    |terminal| match (terminal.signal.as_deref(), terminal.exit_code) {
                        (Some(signal), _) => format!("[shell killed by signal: {signal}]"),
                        (_, Some(code)) => format!("[shell exited: code {code}]"),
                        _ => "[shell exited]".to_owned(),
                    },
                )
                .unwrap_or_else(|| "[shell exited]".to_owned());
            for record in self.children_list() {
                if record.is_terminal() {
                    continue;
                }
                let output = match &record.kind {
                    super::TaskKind::ShellCommand(command) => command.output_record(),
                    super::TaskKind::Job(job) => {
                        if let Some(pid) = *job.pid.borrow() {
                            let _ = terminate_job(pid);
                        }
                        job.output_record().0
                    }
                    _ => super::OutputRecord::default(),
                };
                record.finish(super::Terminal {
                    status: super::TaskStatus::Failed,
                    exit_code: None,
                    signal: None,
                    duration_ms: record.started.elapsed().as_millis() as u64,
                    failure: Some(failure.clone()),
                    output,
                    note: Some(note.clone()),
                });
            }
            self.active_command.lock().unwrap().take();
        }

        pub(crate) fn terminate(&self) {
            self.kill.cancel();
        }

        pub(crate) fn output_record(&self) -> super::OutputRecord {
            let capture = self.capture.lock().unwrap();
            let (preview, preview_truncated) =
                bounded_preview(&capture.text, capture.total as usize);
            super::OutputRecord {
                path: self.path.to_string_lossy().into_owned(),
                bytes: capture.total,
                truncated: capture.total > capture.text.len() as u64,
                text: None,
                preview,
                preview_truncated,
            }
        }

        pub(crate) async fn events_page(
            &self,
            cursor: u64,
            wait_ms: Option<u64>,
            limit: usize,
        ) -> EventPage {
            let wait = wait_ms.unwrap_or(0);
            let deadline = Instant::now() + Duration::from_millis(wait.min(86_400_000));
            loop {
                let page = {
                    let mut events = self.events.lock().unwrap();
                    let page = events.page(cursor, limit);
                    drop(events);
                    page
                };
                if !page.events.is_empty() || page.reset || wait == 0 || Instant::now() >= deadline
                {
                    return page;
                }
                let remaining = deadline.saturating_duration_since(Instant::now());
                tokio::select! {
                    _ = self.event_notify.notified() => {}
                    _ = tokio::time::sleep(remaining) => {}
                }
            }
        }
        pub(crate) async fn write(&self, data: &[u8]) -> io::Result<()> {
            self.pty
                .lock()
                .await
                .as_mut()
                .ok_or_else(|| io::Error::other("Shell is not running"))?
                .write(data)
        }
        pub(crate) async fn resize(&self, size: PtySize) -> io::Result<()> {
            self.pty
                .lock()
                .await
                .as_ref()
                .ok_or_else(|| io::Error::other("Shell is not running"))?
                .resize(size)
        }
        pub(crate) fn page(
            &self,
            cursor: Option<u64>,
            limit: usize,
        ) -> Result<super::OutputPageReply, OutputPageError> {
            let capture = self.capture.lock().unwrap();
            let first = capture.total - capture.text.len() as u64;
            let cursor = cursor.unwrap_or(first);
            if cursor < first {
                return Ok(super::OutputPageReply {
                    text: String::new(),
                    bytes: 0,
                    total_bytes: capture.total,
                    next_cursor: None,
                    truncated: true,
                    retained_from: first,
                    reset: Some(true),
                });
            }
            let start = (cursor - first).min(capture.text.len() as u64) as usize;
            let end = (start + limit + PAGE_CHAR_TRAILING_BYTES).min(capture.text.len());
            let page = decode_page(
                &capture.text.as_bytes()[start..end],
                cursor,
                limit,
                first != 0,
            )?;
            Ok(super::OutputPageReply {
                text: page.text,
                bytes: page.bytes,
                total_bytes: capture.total,
                next_cursor: page.next_cursor,
                truncated: page.truncated,
                retained_from: first,
                reset: None,
            })
        }
        pub(crate) fn tail(&self) -> crate::workspace_tools::exec::OutputTail {
            let capture = self.capture.lock().unwrap();
            let (text, truncated) = bounded_preview(&capture.text, capture.total as usize);
            crate::workspace_tools::exec::OutputTail {
                text,
                total_bytes: capture.total,
                truncated,
            }
        }
    }

    fn shell_command_terminal(
        record: &super::TaskRecord,
        command: &ShellCommand,
        code: String,
    ) -> super::Terminal {
        let (status, failure) = match code.as_str() {
            "0" => (super::TaskStatus::Exited, None),
            "failed" => (
                super::TaskStatus::Failed,
                Some(super::TaskFailure::CommandFailed),
            ),
            other => match other.parse::<i32>() {
                Ok(_) => (
                    super::TaskStatus::Failed,
                    Some(super::TaskFailure::CommandFailed),
                ),
                Err(_) => (
                    super::TaskStatus::Failed,
                    Some(super::TaskFailure::UnknownExit),
                ),
            },
        };
        super::Terminal {
            status,
            exit_code: code.parse().ok(),
            signal: None,
            duration_ms: record.started.elapsed().as_millis() as u64,
            failure,
            output: command.output_record(),
            note: None,
        }
    }

    pub(crate) fn completion_code(text: &str, marker: &str) -> Option<String> {
        text.lines().find_map(|line| {
            line.split_once(marker)
                .map(|(_, rest)| rest.trim().to_owned())
                .filter(|rest| !rest.is_empty())
        })
    }

    pub(crate) fn job_completion_terminal(
        record: &super::TaskRecord,
        job: &ShellJob,
        code: &str,
    ) -> super::Terminal {
        let (output, read_failure) = job.output_record();
        let (status, exit_code, failure) = match code {
            "failed" => (
                super::TaskStatus::Failed,
                None,
                Some(super::TaskFailure::CommandFailed),
            ),
            "unknown" => (
                super::TaskStatus::Failed,
                None,
                Some(super::TaskFailure::UnknownExit),
            ),
            code => match code.parse::<i32>() {
                Ok(0) => (super::TaskStatus::Exited, Some(0), None),
                Ok(code) => (
                    super::TaskStatus::Failed,
                    Some(code),
                    Some(super::TaskFailure::CommandFailed),
                ),
                Err(_) => (
                    super::TaskStatus::Failed,
                    None,
                    Some(super::TaskFailure::UnknownExit),
                ),
            },
        };
        super::Terminal {
            status,
            exit_code,
            signal: None,
            duration_ms: record.started.elapsed().as_millis() as u64,
            failure: read_failure.or(failure),
            output,
            note: None,
        }
    }

    fn job_terminal(
        record: &super::TaskRecord,
        job: &ShellJob,
        code: Option<&str>,
    ) -> super::Terminal {
        let (output, read_failure) = job.output_record();
        let (status, exit_code, failure) = match code {
            Some("0") => (super::TaskStatus::Exited, Some(0), None),
            Some(code) => match code.parse::<i32>() {
                Ok(code) => (
                    super::TaskStatus::Failed,
                    Some(code),
                    Some(super::TaskFailure::CommandFailed),
                ),
                Err(_) => (
                    super::TaskStatus::Failed,
                    None,
                    Some(super::TaskFailure::UnknownExit),
                ),
            },
            None => (super::TaskStatus::Exited, None, None),
        };
        super::Terminal {
            status,
            exit_code,
            signal: None,
            duration_ms: record.started.elapsed().as_millis() as u64,
            failure: read_failure.or(failure),
            output,
            note: None,
        }
    }

    fn shell_terminal(
        record: &super::TaskRecord,
        output: super::OutputRecord,
        exit: Option<portable_pty::ExitStatus>,
        status: &str,
        error: Option<String>,
    ) -> super::Terminal {
        let status_text = status;
        let status = match status_text {
            "exited" => super::TaskStatus::Exited,
            "signaled" => super::TaskStatus::Signaled,
            "timed_out" => super::TaskStatus::TimedOut,
            "cancelled" => super::TaskStatus::Cancelled,
            "unloaded" => super::TaskStatus::Cancelled,
            _ => super::TaskStatus::Failed,
        };
        let (exit_code, signal) = match exit.as_ref() {
            Some(exit) => (
                Some(exit.exit_code() as i32),
                exit.signal().map(str::to_owned),
            ),
            None => (None, None),
        };
        let failure = match status {
            super::TaskStatus::Cancelled if status_text == "unloaded" => {
                Some(super::TaskFailure::OwnerUnloaded)
            }
            super::TaskStatus::Failed => {
                error
                    .as_ref()
                    .map(|message| super::TaskFailure::TerminateFailed {
                        message: message.clone(),
                    })
            }
            _ => None,
        };
        super::Terminal {
            status,
            exit_code,
            signal,
            duration_ms: record.started.elapsed().as_millis() as u64,
            failure,
            output,
            note: error,
        }
    }

    #[cfg(test)]
    mod shell_marker_tests {
        use super::last_full_marker;

        #[test]
        fn an_echoed_marker_is_not_a_full_marker() {
            let echo = "printf '%s%s' '__DIM_START_aaaabbbb_' 'ccccdddd__';";
            assert!(last_full_marker(echo).is_none());
        }

        #[test]
        fn a_full_marker_is_recognized_after_the_echo() {
            let line = "__DIM_START_aaaabbbb_ccccdddd__";
            assert_eq!(last_full_marker(line), Some(("__DIM_START_", "")));
            let line = "__DIM_END_aaaabbbb_ccccdddd__7\n";
            assert_eq!(last_full_marker(line), Some(("__DIM_END_", "7\n")));
        }

        #[test]
        fn the_init_line_reports_its_last_marker() {
            let line = "__DIM_START_aaaabbbb_ccccdddd____DIM_END_eeeeffff_00001111__0\n";
            assert_eq!(last_full_marker(line), Some(("__DIM_END_", "0\n")));
        }
    }
}

pub(crate) struct ConfiguredCommand {
    pub command: String,
    pub cwd: PathBuf,
    pub env: BTreeMap<OsString, Option<OsString>>,
    pub stdin: Vec<u8>,
    pub timeout: Duration,
    pub stdout_limit: usize,
    pub stderr_limit: usize,
}

pub(crate) enum ConfiguredEnd {
    Exited {
        status: ExitStatus,
        stdout: Vec<u8>,
        stderr: Vec<u8>,
        stdout_truncated: bool,
        stderr_truncated: bool,
    },
    Cancelled,
    TimedOut,
}

#[derive(Debug)]
pub(crate) enum ConfiguredError {
    Prepare(ProcessSpecError),
    Unloaded,
    Spawn(std::io::Error),
    Wait(std::io::Error, Vec<u8>),
}

impl std::fmt::Display for ConfiguredError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Prepare(error) => write!(f, "{}: {}", error.code, error.reason),
            Self::Unloaded => f.write_str("unloaded"),
            Self::Spawn(error) => write!(f, "spawn failed: {error}"),
            Self::Wait(error, _) => write!(f, "wait failed: {error}"),
        }
    }
}

impl HostPlatform {
    pub(crate) async fn configured(
        &self,
        request: ConfiguredCommand,
        cancel: &CancellationToken,
        gate: Option<&CancellationToken>,
    ) -> Result<ConfiguredEnd, ConfiguredError> {
        let (mut spec, _) = self
            .build_process_spec(&request.command, &request.cwd)
            .await
            .map_err(ConfiguredError::Prepare)?;
        spec.env.retain(|(name, _)| !request.env.contains_key(name));
        spec.env.extend(
            request
                .env
                .into_iter()
                .filter_map(|(key, value)| value.map(|value| (key, value))),
        );
        if gate.is_some_and(CancellationToken::is_cancelled) {
            return Err(ConfiguredError::Unloaded);
        }
        let mut tree = self
            .spawn_with_stdin_process(spec, request.stdin)
            .map_err(ConfiguredError::Spawn)?;
        let mut stdout = tree.stdout();
        let mut stderr = tree.stderr();
        let end = tokio::select! {
            biased;
            _ = cancel.cancelled() => ConfiguredEnd::Cancelled,
            _ = tokio::time::sleep(request.timeout) => ConfiguredEnd::TimedOut,
            (out, err, status) = async {
                tokio::join!(
                    read_capped(&mut stdout, request.stdout_limit),
                    read_capped(&mut stderr, request.stderr_limit),
                    tree.wait()
                )
            } => {
                return match status {
                    Ok(status) => Ok(ConfiguredEnd::Exited {
                        status,
                        stdout: out.0,
                        stderr: err.0,
                        stdout_truncated: out.1,
                        stderr_truncated: err.1,
                    }),
                    Err(error) => Err(ConfiguredError::Wait(error, err.0))
                }
            }
        };
        let _ = tree.terminate().await;
        Ok(end)
    }
}

/// Reads at most `limit` bytes and drains the rest, so the writer never
/// blocks; the bool says more was written.
async fn read_capped<R: AsyncRead + Unpin>(reader: &mut R, limit: usize) -> (Vec<u8>, bool) {
    let mut kept = Vec::new();
    let mut buffer = [0u8; 8192];
    let mut overflow = false;
    loop {
        match reader.read(&mut buffer).await {
            Ok(0) | Err(_) => return (kept, overflow),
            Ok(read) => {
                let room = limit.saturating_sub(kept.len());
                if read <= room {
                    kept.extend_from_slice(&buffer[..read]);
                } else {
                    kept.extend_from_slice(&buffer[..room]);
                    overflow = true;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host_with(
        env: &[(&str, &str)],
        toolchains: Option<Vec<InstalledToolchain>>,
    ) -> HostPlatform {
        let mut platform = HostPlatform::current(ShellStatus::Unavailable {
            code: "test",
            reason: String::new(),
        })
        .unwrap();
        platform.env = env
            .iter()
            .map(|(key, value)| (OsString::from(key), OsString::from(value)))
            .collect();
        match toolchains {
            Some(toolchains) => {
                platform.with_toolchain_source(ToolchainSourceHost::fixed(toolchains))
            }
            None => platform,
        }
    }

    /// A toolchain whose archive directory is `bin`'s parent.
    fn toolchain(bin: &str) -> InstalledToolchain {
        let bin = PathBuf::from(bin);
        let root = bin.parent().expect("a parent").to_path_buf();
        InstalledToolchain { bin, root }
    }

    /// `entries` as a PATH, in the platform's own separator.
    fn joined(entries: &[&str]) -> String {
        std::env::join_paths(entries)
            .expect("the entries are PATH components")
            .to_string_lossy()
            .into_owned()
    }

    #[test]
    fn the_child_path_is_the_embedding_directories_then_the_base_path_then_the_toolchain() {
        let platform = host_with(
            &[],
            Some(vec![
                toolchain("/runtimes/node/bin"),
                toolchain("/runtimes/python/bin"),
            ]),
        );
        let base = joined(&["/base/a", "/base/b"]);
        let composed = platform
            .child_path_process(&["/host/bin".into()], OsStr::new(&base))
            .unwrap();
        assert_eq!(
            composed.to_str().unwrap(),
            joined(&[
                "/host/bin",
                "/base/a",
                "/base/b",
                "/runtimes/node/bin",
                "/runtimes/python/bin"
            ])
        );
    }

    #[test]
    fn a_toolchain_directory_already_on_the_path_is_not_added_twice() {
        let platform = host_with(
            &[],
            Some(vec![toolchain("/base/a"), toolchain("/runtimes/node/bin")]),
        );
        let base = joined(&["/base/a", "/base/b"]);
        let composed = platform.child_path_process(&[], OsStr::new(&base)).unwrap();
        assert_eq!(
            composed.to_str().unwrap(),
            joined(&["/base/a", "/base/b", "/runtimes/node/bin"])
        );
        let base = joined(&["/base", "/runtimes/node/bin"]);
        let installed = host_with(&[], Some(vec![toolchain("/runtimes/node/bin")]));
        assert!(
            installed
                .child_path_process(&[], OsStr::new(&base))
                .is_none(),
            "a runtime already on the PATH changes nothing"
        );
    }

    #[test]
    fn a_child_path_without_an_addition_answers_none() {
        let platform = host_with(&[], None);
        assert!(
            platform
                .child_path(ChildPathRequest {
                    directories: Vec::new(),
                    base: joined(&["/base"]),
                })
                .is_none()
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_native_child_path_preserves_a_non_utf8_base() {
        use std::os::unix::ffi::OsStrExt;
        let platform = host_with(&[], Some(vec![toolchain("/runtimes/node/bin")]));
        let base = OsStr::from_bytes(b"/base/\xff");
        let composed = platform.child_path_process(&[], base).unwrap();
        assert_eq!(&composed.as_bytes()[..base.len()], base.as_bytes());
        assert!(composed.as_bytes().ends_with(b"/runtimes/node/bin"));
    }

    #[test]
    fn a_platform_without_the_port_leaves_the_path_as_it_is() {
        assert!(
            host_with(&[], None)
                .child_path_process(&[], OsStr::new("/base"))
                .is_none()
        );
        assert!(
            host_with(&[], Some(vec![]))
                .child_path_process(&[], OsStr::new("/base"))
                .is_none()
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_posix_command_gets_the_toolchain_after_the_login_shell_path() {
        let shell = ShellStatus::Available(ExecShellRuntime {
            executable_path: "/bin/sh".into(),
            edition: ShellEdition::Sh,
            source: ShellSource::Login,
            output_encoding: OutputEncoding::Utf8,
        });
        let platform = HostPlatform::current(shell.clone()).unwrap();
        let (spec, _) = platform
            .build_process_spec("true", Path::new("/"))
            .await
            .unwrap();
        assert_eq!(
            spec.args.last().unwrap(),
            "true",
            "without the port the command is unchanged"
        );

        let mut installed = HostPlatform::current(shell).unwrap();
        installed.env = vec![];
        let installed =
            installed.with_toolchain_source(ToolchainSourceHost::fixed(vec![toolchain(
                "/runtimes/node/bin",
            )]));
        let (spec, _) = installed
            .build_process_spec("true", Path::new("/"))
            .await
            .unwrap();
        assert_eq!(
            spec.args.last().unwrap(),
            "export PATH=\"$PATH\":'/runtimes/node/bin'; true"
        );
    }

    /// The sandbox grants the installed runtime's directory read-only, so a
    /// confined command runs a program Dim downloaded. Without the grant
    /// Seatbelt denies the read.
    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn a_confined_command_reads_the_installed_runtimes_directory() {
        use crate::execution_settings::ExecutionSettings;
        use std::os::unix::fs::PermissionsExt;
        let base = std::env::temp_dir().join(format!(
            "dim-toolchain-confined-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let archive = base.join("runtimes").join("node-v-test");
        let bin = archive.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let tool = bin.join("fixturetool");
        std::fs::write(&tool, "#!/bin/sh\necho confined-fixture\n").unwrap();
        std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
        let workspace = base.join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let mut platform = HostPlatform::current(ShellStatus::Available(ExecShellRuntime {
            executable_path: "/bin/sh".into(),
            edition: ShellEdition::Sh,
            source: ShellSource::Login,
            output_encoding: OutputEncoding::Utf8,
        }))
        .unwrap();
        platform.env = vec![];
        let platform = platform
            .with_execution_settings(ExecutionSettingsHost::fixed(ExecutionSettings {
                sandbox: true,
                network: false,
            }))
            .with_toolchain_source(ToolchainSourceHost::fixed(vec![InstalledToolchain {
                bin: bin.clone(),
                root: archive.clone(),
            }]));
        let (spec, _) = platform
            .build_process_spec("fixturetool", &workspace)
            .await
            .unwrap();
        let (spec, temporary, _) = platform
            .prepare_isolated_process(spec, std::slice::from_ref(&workspace), None)
            .await
            .unwrap();
        let canonical = |path: &Path| std::fs::canonicalize(path).unwrap();
        let granted: Vec<PathBuf> = spec
            .args
            .iter()
            .filter_map(|arg| arg.to_str())
            .filter_map(|arg| arg.strip_prefix("ROOT_"))
            .filter_map(|rest| rest.split_once('='))
            .map(|(_, path)| {
                let path = PathBuf::from(path);
                std::fs::canonicalize(&path).unwrap_or(path)
            })
            .collect();
        let mut tree = platform.spawn_tree(spec).unwrap();
        tree.hold(temporary);
        let mut stdout = tree.stdout();
        let mut stderr = tree.stderr();
        let mut out = Vec::new();
        let mut err = Vec::new();
        let (read, wrote) =
            tokio::join!(stdout.read_to_end(&mut out), stderr.read_to_end(&mut err));
        read.unwrap();
        wrote.unwrap();
        assert!(
            tree.wait().await.unwrap().success(),
            "stdout: {}; stderr: {}",
            String::from_utf8_lossy(&out),
            String::from_utf8_lossy(&err)
        );
        assert_eq!(String::from_utf8_lossy(&out).trim(), "confined-fixture");
        assert!(
            granted.iter().any(|path| path == &canonical(&archive)),
            "the archive directory is the granted root: {granted:?}"
        );
        assert!(
            !granted.iter().any(|path| path == &canonical(&bin)),
            "the bin directory is not a root of its own: {granted:?}"
        );
        std::fs::remove_dir_all(base).unwrap();
    }
}

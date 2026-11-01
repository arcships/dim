//! Process facts and the only entry point that starts child processes, see
//! `docs/architecture/rust-agent/plugins/HostPlatform.md`.
//!
//! Shell resolution copies ADR 020 and TS `packages/agent/src/exec-shell.ts`.
//! Process facts are captured at apply; exec shell preferences are read at use
//! time. Shell problems are returned as data, never raised during assembly.
use crate::execution_settings::{ExecutionSettingsHost, ExecutionSettingsSource};
use crate::file_storage::{FileStorage, FileStorageError};
use dim_agent_runtime::Names;
use rutis::Ctx;
use rutis::host::{BoxFuture, CtxHost, DynPlugin, Effect, ErrorKind, Key, TypeKey};
use std::{
    ffi::{OsStr, OsString},
    io,
    path::{Path, PathBuf},
    process::{ExitStatus, Stdio},
    sync::Arc,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::{Child, ChildStderr, ChildStdout},
    sync::Mutex,
};
use tokio_util::sync::CancellationToken;

mod exec_shell_settings;
#[cfg(unix)]
mod isolation;
pub use exec_shell_settings::{ExecShellSettings, ExecShellSettingsError, ShellChoice};

pub const EXEC_SHELL_CONFIG_INVALID: &str = "EXEC_SHELL_CONFIG_INVALID";
pub const EXEC_SHELL_UNAVAILABLE: &str = "EXEC_SHELL_UNAVAILABLE";

#[cfg(windows)]
const WINDOWS_SANDBOX_UNAVAILABLE: &str =
    "The sandbox has no backend on Windows; select Off in /permissions > Process isolation";

/// The declaration line for the tools that start processes; `None` where the
/// platform has no sandbox backend. It is static because declarations are read
/// once, while each process start reads the current switches. Only the macOS
/// default gives each process tree a writable TMPDIR.
pub(crate) const ISOLATION_SUMMARY: Option<&str> = if cfg!(target_os = "macos") {
    Some(
        "Isolation: the user selects Process isolation in /permissions. When sandboxed, commands can write only inside the session workspace and $TMPDIR, read only there and in system directories, and have no network access unless Network allowed is selected. You cannot change the boundary; retrying or an approval does not lift it.",
    )
} else if cfg!(unix) {
    Some(
        "Isolation: the user selects Process isolation in /permissions. When sandboxed, commands can write only inside the session workspace, read only there and in system directories, and have no network access unless Network allowed is selected. You cannot change the boundary; retrying or an approval does not lift it.",
    )
} else {
    None
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Os {
    Macos,
    Linux,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputEncoding {
    Utf8,
    Unknown,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecShellRuntime {
    pub executable_path: PathBuf,
    pub edition: ShellEdition,
    pub source: ShellSource,
    pub output_encoding: OutputEncoding,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShellStatus {
    Available(ExecShellRuntime),
    Unavailable { code: &'static str, reason: String },
}

/// ADR 014 D1's fixed system roots: the sandbox builder mounts them read-only
/// and file access treats them as read-only facts. macOS names `/private/etc`
/// and its `/etc` alias; Windows has no sandbox backend and no roots.
const PLATFORM_ROOTS: &[&str] = {
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
    shell: Arc<Mutex<ResolvedShell>>,
    shell_file: Option<(Arc<FileStorage>, usize, PathBuf)>,
    pub(crate) execution: Option<Arc<dyn ExecutionSettingsSource>>,
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxBackend {
    Available,
    Unavailable { reason: &'static str },
}

/// What a consumer keeps for a process that
/// [`HostPlatform::prepare_isolated_process`] sandboxed.
#[cfg_attr(windows, allow(dead_code))]
pub(crate) struct Isolation;

#[cfg_attr(windows, allow(dead_code))]
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
                    "This command ran under the sandbox the user turned on, which may have caused the failure. It blocks network access and file access outside {reach}. Retrying or an approval does not lift it. Ask the user to change Process isolation in /permissions."
                )
            },
        )
    }
}

/// The host-owned TMPDIR of one isolated process tree; the tree handle that
/// holds it removes it when dropped.
#[cfg_attr(windows, allow(dead_code))]
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
            execution: None,
        })
    }
    /// Uses `host` for the execution switches at each process preparation. The
    /// HostPlatform plugin wires it; a platform without switches fails
    /// preparation instead of running unsandboxed (ADR 011 D1).
    pub fn with_execution_settings(mut self, host: ExecutionSettingsHost) -> Self {
        self.execution = Some(host.0);
        self
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
    /// The command for `spec`: its own tree, a cleared environment and piped
    /// stdout and stderr. `stdin` is the caller's choice.
    fn command(spec: &ProcessSpec, stdin: Stdio) -> tokio::process::Command {
        let mut command = tokio::process::Command::new(&spec.executable);
        command
            .args(&spec.args)
            .current_dir(&spec.cwd)
            .env_clear()
            .envs(spec.env.iter().map(|(key, value)| (key, value)))
            .stdin(stdin)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }
    /// Starts `spec` as the root of its own process tree.
    pub fn spawn(&self, spec: ProcessSpec) -> io::Result<ProcessTree> {
        native::spawn(Self::command(&spec, Stdio::null()))
    }
    /// [`Self::spawn`] with `stdin` written to the child's standard input,
    /// which then closes. A child that exits before reading it does not fail
    /// the start.
    pub fn spawn_with_stdin(&self, spec: ProcessSpec, stdin: Vec<u8>) -> io::Result<ProcessTree> {
        let mut tree = native::spawn(Self::command(&spec, Stdio::piped()))?;
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

    /// The sandbox backend a new process would use. Preparation reads the same
    /// backend, so this report and the next process start cannot disagree.
    #[cfg(windows)]
    pub fn sandbox_backend(&self) -> SandboxBackend {
        SandboxBackend::Unavailable {
            reason: WINDOWS_SANDBOX_UNAVAILABLE,
        }
    }

    /// The sandbox backend a new process would use. macOS always has
    /// `sandbox-exec`.
    #[cfg(target_os = "macos")]
    pub fn sandbox_backend(&self) -> SandboxBackend {
        SandboxBackend::Available
    }

    /// The sandbox backend a new process would use: the same bubblewrap
    /// preparation resolves from the captured `PATH`.
    #[cfg(all(unix, not(target_os = "macos")))]
    pub fn sandbox_backend(&self) -> SandboxBackend {
        match isolation::bubblewrap(&self.env) {
            Some(_) => SandboxBackend::Available,
            None => SandboxBackend::Unavailable {
                reason: isolation::BWRAP_UNAVAILABLE,
            },
        }
    }

    #[cfg(windows)]
    pub(crate) async fn prepare_isolated_process(
        &self,
        spec: ProcessSpec,
        _workspace: &Path,
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
        let env = build_env(self);
        let command = if kind == ShellKind::Posix {
            prepend_runtime_path(
                command,
                &env,
                self,
                &shell.executable_path,
                shell.edition,
                cwd,
            )
            .await
            .map_err(|reason| ProcessSpecError {
                code: "EXEC_SHELL_PATH_MAPPING_FAILED",
                reason,
            })?
        } else {
            command.to_owned()
        };
        let args: &[&str] = match kind {
            ShellKind::Posix => &["-lc"],
            ShellKind::Powershell => &["-NoLogo", "-NoProfile", "-NonInteractive", "-Command"],
            ShellKind::Cmd => &["/d", "/s", "/v:off", "/c"],
        };
        Ok((
            ProcessSpec {
                executable: shell.executable_path.clone(),
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

/// TS `prependRuntimePathForPosixCommand`.
async fn prepend_runtime_path(
    command: &str,
    env: &[(OsString, OsString)],
    platform: &HostPlatform,
    shell: &Path,
    edition: ShellEdition,
    cwd: &Path,
) -> Result<String, String> {
    let git_bash = edition == ShellEdition::GitBash;
    let entries = runtime_path_entries(env, git_bash);
    if entries.is_empty() {
        return Ok(command.to_owned());
    }
    let mapped = if git_bash {
        map_git_bash_entries(&entries, platform, shell, env, cwd).await?
    } else {
        entries
    };
    let prefix: Vec<String> = mapped.iter().map(|entry| quote_posix(entry)).collect();
    Ok(format!(
        "export PATH={}:\"$PATH\"; {command}",
        prefix.join(":")
    ))
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
    let mut tree = platform.spawn(spec).map_err(|error| error.to_string())?;
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
            native::signal(self.pid, libc::SIGKILL);
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
}
impl HostPlatformPlugin {
    pub fn new(names: &Names<'_>, config: HostPlatformConfig) -> rutis::Result<Self> {
        let storage = uses_storage(&config)
            .then(|| names.key::<FileStorage>())
            .transpose()?;
        let execution = config.execution.clone();
        Ok(Self {
            config,
            storage,
            execution,
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
            platform.execution = Some(execution.0.clone());
            platform.shell.lock().await.config = config;
            ctx.provide_as::<HostPlatform>(Arc::new(platform))?;
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
        Some((shell, source)) => ShellStatus::Available(ExecShellRuntime {
            executable_path: shell.path.clone(),
            edition: shell.edition,
            source,
            output_encoding: match shell.edition {
                ShellEdition::GitBash | ShellEdition::WindowsPowershell | ShellEdition::Cmd => {
                    OutputEncoding::Unknown
                }
                _ => OutputEncoding::Utf8,
            },
        }),
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
        Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE},
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
                CREATE_NO_WINDOW, CREATE_SUSPENDED, OpenProcess, OpenThread, PROCESS_SET_QUOTA,
                PROCESS_TERMINATE, ResumeThread, THREAD_SUSPEND_RESUME, TerminateProcess,
            },
        },
        UI::Shell::{FOLDERID_Profile, SHGetKnownFolderPath},
    };

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
                native::signal(self.pid, libc::SIGKILL);
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
            native::signal(self.pid, libc::SIGKILL);
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

pub(crate) async fn stop_job(pid: u32) -> io::Result<()> {
    terminate_job(pid)?;
    #[cfg(unix)]
    {
        match wait_job_exit(pid).await {
            Err(error) if error.kind() == io::ErrorKind::TimedOut => kill_job(pid).await,
            result => result,
        }
    }
    #[cfg(windows)]
    Ok(())
}

/// Kills a background job that outlived its shell and confirms it is gone:
/// the process group it leads on POSIX, the process tree under it on
/// Windows.
pub async fn kill_job(pid: u32) -> io::Result<()> {
    #[cfg(unix)]
    {
        // SAFETY: plain syscall on the addressed group.
        unsafe { libc::kill(-(pid as libc::pid_t), libc::SIGKILL) };
        wait_job_exit(pid).await
    }
    #[cfg(windows)]
    native::terminate_tree(pid)
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

impl HostPlatform {
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

    fn fallback_persistent_shell(&self) -> ShellStatus {
        if cfg!(windows) {
            for name in ["pwsh.exe", "powershell.exe"] {
                if let Some(path) = find_executable(name) {
                    let edition = if name == "pwsh.exe" {
                        ShellEdition::Pwsh
                    } else {
                        ShellEdition::WindowsPowershell
                    };
                    return ShellStatus::Available(ExecShellRuntime {
                        executable_path: path,
                        edition,
                        source: ShellSource::Fallback,
                        output_encoding: match edition {
                            ShellEdition::Pwsh => OutputEncoding::Utf8,
                            _ => OutputEncoding::Unknown,
                        },
                    });
                }
            }
            ShellStatus::Unavailable {
                code: EXEC_SHELL_UNAVAILABLE,
                reason: "No supported persistent shell found.".into(),
            }
        } else {
            if let Some(path) = find_executable("bash") {
                return ShellStatus::Available(ExecShellRuntime {
                    executable_path: path,
                    edition: ShellEdition::Bash,
                    source: ShellSource::Fallback,
                    output_encoding: OutputEncoding::Utf8,
                });
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

    /// The directory the persistent shell owns: `<home>/.dim/exec`.
    pub fn exec_root(&self) -> PathBuf {
        let root = self.home.join(".dim").join("exec");
        std::fs::canonicalize(&root).unwrap_or(root)
    }

    /// Removes `path`, one persistent shell output directory below
    /// [`exec_root`](Self::exec_root), and syncs its parent. An absent
    /// directory counts as removed. The caller has decided with the
    /// ownership rows that no Session still needs it (ADR 013 D4).
    pub fn remove_exec_directory(&self, path: &Path) -> io::Result<()> {
        let root = self.exec_root();
        if path == root || !path.starts_with(&root) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "not a persistent shell output directory",
            ));
        }
        match std::fs::remove_dir_all(path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        #[cfg(unix)]
        if let Some(parent) = path.parent() {
            std::fs::File::open(parent)?.sync_all()?;
        }
        Ok(())
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

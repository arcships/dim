//! The file tools `read`, `write`, `edit`, `glob` and `grep`.
//!
//! Behavior, texts, error codes and limits follow the TS tools
//! (`packages/agent/src/tool/builtin/` and `packages/plugin/src/builtin/grep-glob/`).
//! Workspace I/O runs on blocking threads with `std::fs`.

#![allow(clippy::result_large_err)]

mod codec;
mod edit;
mod read;
mod search;
mod write;

use super::file_reads::FileReads;
use crate::host::HostError;
use dim_agent_types::{JsonObject, ToolCall, ToolDefinition, ToolEffect, ToolResult};
use serde_json::{Value, json};
use sha2::Digest;
use std::{
    collections::HashMap,
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex},
};
use tokio::sync::OwnedMutexGuard;
use tokio_util::sync::CancellationToken;

const TOOL_NAMES: [&str; 5] = ["read", "write", "edit", "glob", "grep"];
const CANCELLED: &str = "Tool execution cancelled.";

/// Session-level executor of the file tools. Bindings belong to the calling
/// branch and are passed per call. Product Sessions share a process-owned
/// coordinator; standalone callers may use an isolated coordinator.
pub struct FileTools {
    cwd: PathBuf,
    home: PathBuf,
    /// Session output directory; large `read` results are written here.
    output_dir: PathBuf,
    /// The root boundary a host configured for this session's file tools;
    /// `None` keeps the unbounded behaviour.
    boundary: Option<FileToolBoundary>,
    locks: Arc<FileWriteCoordinator>,
    #[cfg(test)]
    /// Runs after a mutation's target resolved and before the per-path
    /// coordinator is awaited; no sleeps, production hooks or global test
    /// locks. A test holds the coordinator and swaps the path here.
    before_lock: Option<ResolveHook>,
    #[cfg(test)]
    /// Runs after a complete sibling is synced and before commit verification;
    /// no sleeps, production hooks or global test locks.
    before_commit: Option<CommitHook>,
    #[cfg(test)]
    /// Runs inside a bounded `grep` walk for each walked entry, before the
    /// entry is read; no sleeps, production hooks or global test locks. A
    /// test swaps a directory here.
    before_search: Option<SearchHook>,
}

#[cfg(test)]
type CommitHook = Arc<dyn Fn(&Path) + Send + Sync>;
#[cfg(test)]
type ResolveHook = Arc<dyn Fn(&Path) + Send + Sync>;
#[cfg(test)]
type SearchHook = Arc<dyn Fn(&Path) + Send + Sync>;

impl FileTools {
    pub fn new(cwd: PathBuf, home: PathBuf, output_dir: PathBuf) -> Self {
        Self::with_coordinator(cwd, home, output_dir, Arc::default(), None)
    }

    /// [`new`](Self::new) with the root boundary resolved file paths must
    /// stay inside. The boundary's home is the home `~` resolves against.
    pub fn with_boundary(cwd: PathBuf, output_dir: PathBuf, boundary: FileToolBoundary) -> Self {
        Self::with_coordinator(
            cwd,
            boundary.home.clone(),
            output_dir,
            Arc::default(),
            Some(boundary),
        )
    }

    pub fn with_coordinator(
        cwd: PathBuf,
        home: PathBuf,
        output_dir: PathBuf,
        locks: Arc<FileWriteCoordinator>,
        boundary: Option<FileToolBoundary>,
    ) -> Self {
        Self {
            cwd,
            home,
            output_dir,
            boundary,
            locks,
            #[cfg(test)]
            before_lock: None,
            #[cfg(test)]
            before_commit: None,
            #[cfg(test)]
            before_search: None,
        }
    }

    pub fn definitions() -> Vec<ToolDefinition> {
        [
            read::definition(),
            write::definition(),
            edit::definition(),
            search::glob_definition(),
            search::grep_definition(),
        ]
        .into_iter()
        .map(|value| serde_json::from_value(value).expect("built-in tool definition is valid"))
        .collect()
    }

    pub fn handles(name: &str) -> bool {
        TOOL_NAMES.contains(&name)
    }

    /// The home `~` resolves against: the boundary's when one is configured,
    /// the platform's otherwise.
    pub(crate) fn home(&self) -> &Path {
        &self.home
    }

    pub async fn execute(
        &self,
        call: &ToolCall,
        reads: &FileReads,
        cancel: CancellationToken,
    ) -> Result<ToolResult, HostError> {
        if cancel.is_cancelled() {
            return Err(cancelled());
        }
        let input = &call.input;
        match call.name.as_str() {
            "read" => read::run(self, &call.id, input, reads, cancel).await,
            "write" => write::run(self, input, reads, cancel).await,
            "edit" => edit::run(self, input, reads, cancel).await,
            "glob" => search::glob(self, input, cancel).await,
            "grep" => search::grep(self, input, cancel).await,
            name => Err(HostError::failed(format!("unknown file tool: {name}"))),
        }
    }
}

pub(crate) fn cancelled() -> HostError {
    HostError::Cancelled(CANCELLED.into())
}

/// Cancellation observed on a blocking thread before any write began.
struct Cancelled;
impl Cancelled {
    fn check(cancel: &CancellationToken) -> Result<(), Cancelled> {
        if cancel.is_cancelled() {
            Err(Cancelled)
        } else {
            Ok(())
        }
    }
}

/// Whether a resolved path is admitted for reading or writing: reads may
/// enter the workspace, the configured home and the read-only roots; writes
/// only the workspace.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Access {
    Read,
    Write,
}

/// The one tilde rule of the file tools: `~`, `~/…` and `~\…` expand
/// against `home`; `None` when the input does not start with a tilde.
///
/// `resolve_path` always applies it. `glob` and `grep` apply it only when a
/// boundary is configured: without one the product has always taken `~` as
/// a literal component under the cwd (`<cwd>/~`, `resolve_scope` in
/// `files/search.rs` on main), and that behaviour is kept (ADR 014).
fn expand_tilde(input: &str, home: &Path) -> Option<PathBuf> {
    if input == "~" {
        return Some(home.to_owned());
    }
    input
        .strip_prefix("~/")
        .or_else(|| input.strip_prefix("~\\"))
        .map(|rest| home.join(rest))
}

/// Resolves a model-supplied file path (TS `path.ts`, design §7.2): `~`
/// expansion, Windows corrections, join onto cwd, lexical `.`/`..`
/// removal, then realpath of the longest existing prefix. With `boundary`
/// configured, a resolved path outside the roots is refused for `Access::Write`
/// (workspace only) or for `Access::Read` (workspace, home and the read-only
/// roots); the check compares the canonical path, so a symlink out of the
/// roots is refused as well.
pub(crate) fn resolve_path(
    cwd: &Path,
    home: &Path,
    boundary: Option<&FileToolBoundary>,
    access: Access,
    input: &str,
) -> Result<PathBuf, PathError> {
    let expanded = match expand_tilde(input, home) {
        Some(path) => path,
        None => windows_path(input)?,
    };
    match canonical_prefix(lexical(&cwd.join(expanded))) {
        Ok(path) if boundary.is_some_and(|boundary| !inside(boundary, &path, access)) => {
            Err(PathError::Refused(path))
        }
        Ok(path) => Ok(path),
        Err(message) => Err(PathError::Message(message)),
    }
}

/// Why a resolved file path failed.
pub(crate) enum PathError {
    /// The canonical path lies outside the configured roots.
    Refused(PathBuf),
    Message(String),
}
impl std::fmt::Display for PathError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused(path) => write!(
                formatter,
                "Path is outside the configured file roots: {}",
                path.display()
            ),
            Self::Message(message) => formatter.write_str(message),
        }
    }
}
impl From<String> for PathError {
    fn from(message: String) -> Self {
        Self::Message(message)
    }
}
impl From<PathError> for String {
    fn from(error: PathError) -> Self {
        error.to_string()
    }
}

/// The roots a host configures for a session's file tools: they bound the
/// model's file-tool calls — `read`, `write`, `edit`, `glob` and `grep` —
/// when a host configures them. Content reads and mutations are
/// handle-verified: `read`, each file `grep` matches, and `write`/`edit`
/// act on the target's parent opened through a symlink-free walk, so a path
/// swapped after admission fails closed. `glob` returns names only; each
/// result's canonical path is re-checked against the roots before it is
/// returned and a failed one is dropped. The roots make no sandbox claim:
/// `exec` is bounded by the sandbox (ADR 010 owns it). Client-initiated
/// file paths — `@` mention expansion and the App Server's
/// `session.files.*` methods — are outside the roots, because the model
/// cannot make those calls: in a run it has only its tools, and the run's
/// App Server socket is outside every root and unreachable from a sandboxed
/// command. The paths are absolute and canonical: the evaluation harness
/// resolves them from directories it created, and a root reached through a
/// symlink would answer `starts_with` with its lexical path. The product
/// passes `None`, so its behaviour does not change (ADR 014 keeps deciding
/// file access for it).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FileToolBoundary {
    /// The session's workspace: reads and writes. A model's cwd is inside
    /// it.
    pub workspace: PathBuf,
    /// The home `~` resolves against and reads are allowed in; writes are
    /// not. An evaluation run points it at the run's disposable home.
    pub home: PathBuf,
    /// The read-only system roots a toolchain needs (the platform's fixed
    /// roots, ADR 014): reads only.
    pub read_only_roots: Vec<PathBuf>,
}

impl FileToolBoundary {
    /// The absolute canonical form of each root: a boundary built from
    /// directories the caller created, as the evaluation harness does.
    pub fn canonicalize(&self) -> std::io::Result<Self> {
        Ok(Self {
            workspace: std::fs::canonicalize(&self.workspace)?,
            home: std::fs::canonicalize(&self.home)?,
            read_only_roots: self
                .read_only_roots
                .iter()
                .map(std::fs::canonicalize)
                .collect::<std::io::Result<_>>()?,
        })
    }
}

/// Whether a canonicalized path lies inside the roots for the access: reads
/// in the workspace, the home and the read-only roots; writes only in the
/// workspace.
fn inside(boundary: &FileToolBoundary, path: &Path, access: Access) -> bool {
    if path.starts_with(&boundary.workspace) {
        true
    } else if access == Access::Write {
        false
    } else {
        path.starts_with(&boundary.home)
            || boundary
                .read_only_roots
                .iter()
                .any(|root| path.starts_with(root))
    }
}

/// The refusal for a resolved path outside the configured roots.
pub(crate) fn refused(path: &Path) -> ToolResult {
    failed(
        format!(
            "Path is outside the configured file roots: {}",
            path.display()
        ),
        json!({
            "code": "outside_file_roots",
            "path": path.to_string_lossy(),
        }),
    )
}

/// SHA-256 of raw file bytes.
fn revision(bytes: &[u8]) -> [u8; 32] {
    sha2::Sha256::digest(bytes).into()
}

/// Runs blocking file-system work off the async runtime.
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<T, HostError> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|error| HostError::failed(format!("file tool task failed: {error}")))
}

/// TS `failedResult`: the message is the model-visible content, the code is
/// structured.
fn failed(message: impl Into<String>, structured: Value) -> ToolResult {
    ToolResult {
        is_error: Some(true),
        structured_content: Some(object(structured)),
        ..ToolResult::text(message)
    }
}

fn invalid_input(message: impl Into<String>) -> ToolResult {
    failed(message, json!({ "code": "invalid_input" }))
}

fn succeeded(content: impl Into<String>, structured: Value) -> ToolResult {
    ToolResult {
        structured_content: Some(object(structured)),
        ..ToolResult::text(content)
    }
}

/// A successful `write` or `edit` result: the published file is the call's
/// `fileWritten` effect (session boundary A.6).
fn wrote(content: impl Into<String>, structured: Value, path: &str) -> ToolResult {
    ToolResult {
        structured_content: Some(object(structured)),
        effects: Some(vec![ToolEffect::FileWritten {
            path: path.to_owned(),
        }]),
        ..ToolResult::text(content)
    }
}

fn object(value: Value) -> JsonObject {
    match value {
        Value::Object(map) => map,
        _ => unreachable!("structured content is an object"),
    }
}

/// A present, non-null input field.
fn field<'a>(input: &'a JsonObject, key: &str) -> Option<&'a Value> {
    input.get(key).filter(|value| !value.is_null())
}

/// Length in UTF-16 code units, the unit of every TS character limit.
fn js_len(text: &str) -> usize {
    text.encode_utf16().count()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// TS `correctWindowsPath`, plus the ADR agent-020 rejection of
/// drive-relative paths.
#[cfg(windows)]
fn windows_path(input: &str) -> Result<PathBuf, String> {
    let corrected = if let Some(rest) = input.strip_prefix("/cygdrive/")
        && let Some(drive) = rest.chars().next().filter(char::is_ascii_alphabetic)
        && (rest.len() == 1 || rest.as_bytes()[1] == b'/')
    {
        let tail = rest.get(2..).unwrap_or("");
        format!(
            "{}:\\{}",
            drive.to_ascii_uppercase(),
            tail.replace('/', "\\")
        )
    } else if input.len() >= 3
        && input.as_bytes()[0] == b'/'
        && input.as_bytes()[1].is_ascii_alphabetic()
        && input.as_bytes()[2] == b':'
    {
        format!("{}{}", &input[1..3], input[3..].replace('/', "\\"))
    } else {
        input.to_owned()
    };
    let path = PathBuf::from(corrected);
    if matches!(path.components().next(), Some(Component::Prefix(_))) && !path.has_root() {
        return Err(format!(
            "Drive-relative path is not supported: {input}. Use an absolute path such as C:\\dir\\file."
        ));
    }
    Ok(path)
}
#[cfg(not(windows))]
fn windows_path(input: &str) -> Result<PathBuf, String> {
    Ok(PathBuf::from(input))
}

/// Node `path.resolve` normalization: drops `.` and folds `..`.
fn lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if matches!(out.components().next_back(), Some(Component::Normal(_))) {
                    out.pop();
                }
            }
            other => out.push(other),
        }
    }
    out
}

/// Realpath of the longest existing prefix with the missing rest appended.
pub(crate) fn canonical_prefix(path: PathBuf) -> Result<PathBuf, String> {
    let mut existing = path.as_path();
    let mut missing = Vec::new();
    loop {
        match std::fs::canonicalize(existing) {
            Ok(mut canonical) => {
                canonical.extend(missing.iter().rev());
                return Ok(canonical);
            }
            Err(error)
                if error.kind() == std::io::ErrorKind::NotFound
                    && std::fs::symlink_metadata(existing)
                        .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound) => {}
            Err(error) => return Err(format!("Failed to resolve {}: {error}", existing.display())),
        }
        match (existing.parent(), existing.file_name()) {
            (Some(parent), Some(name)) => {
                missing.push(name.to_owned());
                existing = parent;
            }
            _ => return Err(format!("Cannot resolve {}", path.display())),
        }
    }
}

/// Opens an existing target without following a final symlink.
fn open_options() -> std::fs::OpenOptions {
    #[allow(unused_mut)]
    let mut options = std::fs::OpenOptions::new();
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::custom_flags(&mut options, libc::O_NOFOLLOW);
    options
}

/// The target exists but is not a regular file (a FIFO, device or socket).
pub(crate) struct NotRegular;

/// Opens an existing regular file for reading. The open never blocks (a FIFO
/// without a writer would block it, uncancellably), and the type is checked
/// on the opened handle, so a swap after a path check cannot slip through.
fn open_regular(path: &Path) -> std::io::Result<Result<std::fs::File, NotRegular>> {
    #[allow(unused_mut)]
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::custom_flags(
        &mut options,
        libc::O_NOFOLLOW | libc::O_NONBLOCK,
    );
    let file = options.open(path)?;
    Ok(if file.metadata()?.is_file() {
        Ok(file)
    } else {
        Err(NotRegular)
    })
}

fn not_regular(display: &str) -> ToolResult {
    failed(
        format!("Not a regular file: {display}"),
        json!({ "code": "not_regular_file" }),
    )
}

/// Whether the file at `path` is the opened file (the staged sibling has not
/// been swapped).
fn owned_temporary(path: &Path, owned: &std::fs::File) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        match (owned.metadata(), std::fs::symlink_metadata(path)) {
            (Ok(a), Ok(b)) => a.dev() == b.dev() && a.ino() == b.ino(),
            _ => false,
        }
    }
    #[cfg(windows)]
    {
        open_regular(path)
            .ok()
            .and_then(Result::ok)
            .is_some_and(|current| write::same_identity(owned, &current).unwrap_or(false))
    }
}

/// The size class of a target entry, read without following a final symlink.
pub(crate) struct Stat {
    /// A directory sits at the path.
    pub(crate) directory: bool,
    /// The entry's size in bytes.
    pub(crate) len: u64,
}

/// Why an operation on an admitted target failed.
pub(crate) enum TargetError {
    /// A component met at I/O time is a symlink or not a directory: the path
    /// changed after admission, so the call fails closed.
    Refused,
    /// Any other I/O failure.
    Io(std::io::Error),
}
impl From<std::io::Error> for TargetError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

/// Where a file tool's I/O acts for one admitted path.
///
/// `Path` is the unbounded behaviour (no boundary configured): the
/// path-based calls the tools always made. `Handle` is the bounded
/// behaviour: the target's parent directory, opened at I/O time through a
/// symlink-free walk — after the per-path coordinator is held for a
/// mutation — with every operation of the call running relative to that
/// handle (the stat, the opens, the staged sibling, the install and the
/// parent fsync). A parent component swapped for a symlink after admission
/// therefore fails closed with the boundary refusal instead of redirecting
/// the I/O outside the roots. The admitted canonical path has no symlinks by
/// construction, so a symlink the walk meets means the path changed after
/// admission.
pub(crate) enum Target {
    Path(PathBuf),
    #[cfg(unix)]
    Handle(Parent),
}

#[cfg(unix)]
pub(crate) struct Parent {
    path: PathBuf,
    /// The parent directory, opened one component at a time.
    dir: std::fs::File,
    /// The target's name inside `dir`.
    name: std::ffi::OsString,
}

#[cfg(unix)]
fn c_name(name: &std::ffi::OsStr) -> std::io::Result<std::ffi::CString> {
    use std::os::unix::ffi::OsStrExt;
    std::ffi::CString::new(name.as_bytes()).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "a path component holds a NUL byte",
        )
    })
}

/// Opens `name` below `parent` without following a symlink.
#[cfg(unix)]
fn open_relative(
    parent: &std::fs::File,
    name: &std::ffi::OsStr,
    flags: libc::c_int,
    mode: libc::c_uint,
) -> std::io::Result<std::fs::File> {
    use std::os::fd::{AsRawFd, FromRawFd};
    let name = c_name(name)?;
    // SAFETY: `parent` is an open directory handle for the duration of the
    // call and `name` is a live NUL-terminated string.
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            flags | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
            mode,
        )
    };
    if fd < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        // SAFETY: `fd` is non-negative (checked above) and was just returned
        // by `openat`, so this takes ownership of a fresh descriptor no other
        // owner holds.
        Ok(unsafe { std::fs::File::from_raw_fd(fd) })
    }
}

/// A walk that met a symlink (`ELOOP`) or a non-directory (`ENOTDIR`) is the
/// path-changed-after-admission refusal; every other error is I/O.
#[cfg(unix)]
fn classify(error: std::io::Error) -> TargetError {
    match error.raw_os_error() {
        Some(libc::ELOOP | libc::ENOTDIR) => TargetError::Refused,
        _ => TargetError::Io(error),
    }
}

#[cfg(unix)]
impl Parent {
    /// Walks the parent components from `/`, each opened `O_DIRECTORY |
    /// O_NOFOLLOW` relative to the previous handle. With `create`, a missing
    /// component is made relative to the held handle (the admitted path's
    /// tail may not exist yet for a write).
    fn open(path: &Path, create: bool) -> Result<Self, TargetError> {
        use std::os::fd::AsRawFd;
        let invalid = |message| {
            TargetError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                message,
            ))
        };
        let name = path
            .file_name()
            .ok_or_else(|| invalid("a file target names a file"))?
            .to_owned();
        let parent = path
            .parent()
            .ok_or_else(|| invalid("a file target has a parent directory"))?;
        let mut dir = std::fs::File::open("/").map_err(TargetError::Io)?;
        for component in parent.components() {
            let part = match component {
                Component::RootDir => continue,
                Component::Normal(part) => part,
                _ => return Err(TargetError::Refused),
            };
            dir = match open_relative(&dir, part, libc::O_RDONLY | libc::O_DIRECTORY, 0) {
                Ok(next) => next,
                Err(error) if create && error.kind() == std::io::ErrorKind::NotFound => {
                    let part_name = c_name(part)?;
                    // SAFETY: `dir` is an open directory handle and
                    // `part_name` a live NUL-terminated string for the call.
                    if unsafe { libc::mkdirat(dir.as_raw_fd(), part_name.as_ptr(), 0o777) } != 0 {
                        let error = std::io::Error::last_os_error();
                        if error.kind() != std::io::ErrorKind::AlreadyExists {
                            return Err(classify(error));
                        }
                    }
                    open_relative(&dir, part, libc::O_RDONLY | libc::O_DIRECTORY, 0)
                        .map_err(classify)?
                }
                Err(error) => return Err(classify(error)),
            };
        }
        Ok(Self {
            path: path.to_owned(),
            dir,
            name,
        })
    }

    fn stat(&self) -> Result<Option<Stat>, TargetError> {
        use std::os::fd::AsRawFd;
        let name = c_name(&self.name)?;
        let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: the handle and the NUL-terminated name are live for the
        // call, and `stat` is a valid uninitialized `libc::stat` buffer.
        let result = unsafe {
            libc::fstatat(
                self.dir.as_raw_fd(),
                name.as_ptr(),
                stat.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if result < 0 {
            let error = std::io::Error::last_os_error();
            return match error.kind() {
                std::io::ErrorKind::NotFound => Ok(None),
                _ => Err(classify(error)),
            };
        }
        // SAFETY: `fstatat` returned 0 above, so it initialized the struct.
        let stat = unsafe { stat.assume_init() };
        if stat.st_mode & libc::S_IFMT == libc::S_IFLNK {
            return Err(TargetError::Refused);
        }
        Ok(Some(Stat {
            directory: stat.st_mode & libc::S_IFMT == libc::S_IFDIR,
            len: stat.st_size as u64,
        }))
    }

    fn open_read(&self) -> Result<Result<std::fs::File, NotRegular>, TargetError> {
        let file = open_relative(&self.dir, &self.name, libc::O_RDONLY, 0).map_err(classify)?;
        Ok(if file.metadata()?.is_file() {
            Ok(file)
        } else {
            Err(NotRegular)
        })
    }

    fn open_write(&self) -> Result<std::fs::File, TargetError> {
        open_relative(&self.dir, &self.name, libc::O_WRONLY, 0).map_err(classify)
    }

    fn create_staged(&self, name: &std::ffi::OsStr) -> Result<std::fs::File, TargetError> {
        open_relative(
            &self.dir,
            name,
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
            0o600,
        )
        .map_err(classify)
    }

    fn staged_owned(&self, name: &std::ffi::OsStr, file: &std::fs::File) -> bool {
        use std::os::fd::AsRawFd;
        use std::os::unix::fs::MetadataExt;
        let (Ok(metadata), Ok(name)) = (file.metadata(), c_name(name)) else {
            return false;
        };
        let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: the handle and the NUL-terminated name are live for the
        // call, and `stat` is a valid uninitialized `libc::stat` buffer.
        let result = unsafe {
            libc::fstatat(
                self.dir.as_raw_fd(),
                name.as_ptr(),
                stat.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if result < 0 {
            return false;
        }
        // SAFETY: `fstatat` returned 0 (checked above), so it initialized
        // the struct.
        let stat = unsafe { stat.assume_init() };
        #[allow(clippy::unnecessary_cast)]
        let (device, inode) = (stat.st_dev as u64, stat.st_ino as u64);
        stat.st_mode & libc::S_IFMT == libc::S_IFREG
            && (device, inode) == (metadata.dev(), metadata.ino())
    }

    fn remove_staged(&self, name: &std::ffi::OsStr) {
        use std::os::fd::AsRawFd;
        if let Ok(name) = c_name(name) {
            // SAFETY: the handle and the NUL-terminated name are live for
            // the call.
            unsafe { libc::unlinkat(self.dir.as_raw_fd(), name.as_ptr(), 0) };
        }
    }

    fn install(&self, staged: &std::ffi::OsStr, existing: bool) -> Result<(), TargetError> {
        use std::os::fd::AsRawFd;
        let staged = c_name(staged)?;
        let name = c_name(&self.name)?;
        let result = if existing {
            // SAFETY: both names are live NUL-terminated strings and
            // `self.dir` is an open directory handle for the call.
            unsafe {
                libc::renameat(
                    self.dir.as_raw_fd(),
                    staged.as_ptr(),
                    self.dir.as_raw_fd(),
                    name.as_ptr(),
                )
            }
        } else {
            // SAFETY: both names are live NUL-terminated strings and
            // `self.dir` is an open directory handle for the call.
            unsafe {
                libc::linkat(
                    self.dir.as_raw_fd(),
                    staged.as_ptr(),
                    self.dir.as_raw_fd(),
                    name.as_ptr(),
                    0,
                )
            }
        };
        if result == 0 {
            Ok(())
        } else {
            Err(TargetError::Io(std::io::Error::last_os_error()))
        }
    }

    fn final_regular(&self) -> bool {
        use std::os::fd::AsRawFd;
        let Ok(name) = c_name(&self.name) else {
            return false;
        };
        let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: the handle and the NUL-terminated name are live for the
        // call, and `stat` is a valid uninitialized `libc::stat` buffer.
        let result = unsafe {
            libc::fstatat(
                self.dir.as_raw_fd(),
                name.as_ptr(),
                stat.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        // SAFETY: the `fstatat` above returned 0 (the left conjunct), so it
        // initialized the struct.
        result == 0 && unsafe { stat.assume_init() }.st_mode & libc::S_IFMT == libc::S_IFREG
    }
}

impl Target {
    pub(crate) fn path(&self) -> &Path {
        match self {
            Self::Path(path) => path,
            #[cfg(unix)]
            Self::Handle(parent) => &parent.path,
        }
    }

    /// The target of a read: with a boundary, the parent walk must already
    /// exist; a missing component reads as "not found".
    pub(crate) fn for_read(path: &Path, bounded: bool) -> Result<Self, TargetError> {
        if !bounded {
            return Ok(Self::Path(path.to_owned()));
        }
        #[cfg(unix)]
        {
            Ok(Self::Handle(Parent::open(path, false)?))
        }
        #[cfg(not(unix))]
        {
            let _ = path;
            Err(TargetError::Refused)
        }
    }

    /// The target of a mutation: with a boundary, missing parent components
    /// are created relative to the walked handle when `create`.
    pub(crate) fn for_mutation(
        path: &Path,
        bounded: bool,
        create: bool,
    ) -> Result<Self, TargetError> {
        if !bounded {
            return Ok(Self::Path(path.to_owned()));
        }
        #[cfg(unix)]
        {
            Ok(Self::Handle(Parent::open(path, create)?))
        }
        #[cfg(not(unix))]
        {
            let (_, _) = (path, create);
            Err(TargetError::Refused)
        }
    }

    pub(crate) fn stat(&self) -> Result<Option<Stat>, TargetError> {
        match self {
            Self::Path(path) => match std::fs::metadata(path) {
                Ok(metadata) => Ok(Some(Stat {
                    directory: metadata.is_dir(),
                    len: metadata.len(),
                })),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(error) => Err(TargetError::Io(error)),
            },
            #[cfg(unix)]
            Self::Handle(parent) => parent.stat(),
        }
    }

    pub(crate) fn open_regular(&self) -> Result<Result<std::fs::File, NotRegular>, TargetError> {
        match self {
            Self::Path(path) => open_regular(path).map_err(TargetError::Io),
            #[cfg(unix)]
            Self::Handle(parent) => parent.open_read(),
        }
    }

    /// Opens the existing target for writing, without following a final
    /// symlink.
    pub(crate) fn open_writable(&self) -> Result<std::fs::File, TargetError> {
        match self {
            Self::Path(path) => {
                let mut options = open_options();
                #[cfg(unix)]
                std::os::unix::fs::OpenOptionsExt::custom_flags(
                    &mut options,
                    libc::O_NOFOLLOW | libc::O_NONBLOCK,
                );
                options.write(true).open(path).map_err(TargetError::Io)
            }
            #[cfg(unix)]
            Self::Handle(parent) => parent.open_write(),
        }
    }

    pub(crate) fn create_staged(
        &self,
        name: &std::ffi::OsStr,
    ) -> Result<std::fs::File, TargetError> {
        match self {
            Self::Path(path) => {
                #[cfg(windows)]
                {
                    let parent =
                        crate::file_storage::windows::open_root(path.parent().ok_or_else(
                            || TargetError::Io(std::io::Error::other("a target has a parent")),
                        )?)
                        .map_err(|error| TargetError::Io(std::io::Error::other(error)))?;
                    let file = crate::file_storage::windows::create_temp(
                        &parent,
                        &name.to_owned(),
                        &parent,
                        &path
                            .file_name()
                            .ok_or_else(|| {
                                TargetError::Io(std::io::Error::other("a target names a file"))
                            })?
                            .to_owned(),
                        None,
                    );
                    return file.map_err(TargetError::Io);
                }
                #[cfg(not(windows))]
                {
                    let mut options = open_options();
                    options.write(true).create_new(true);
                    #[cfg(unix)]
                    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
                    options
                        .open(path.with_file_name(name))
                        .map_err(TargetError::Io)
                }
            }
            #[cfg(unix)]
            Self::Handle(parent) => parent.create_staged(name),
        }
    }

    pub(crate) fn staged_owned(&self, name: &std::ffi::OsStr, file: &std::fs::File) -> bool {
        match self {
            Self::Path(path) => owned_temporary(&path.with_file_name(name), file),
            #[cfg(unix)]
            Self::Handle(parent) => parent.staged_owned(name, file),
        }
    }

    pub(crate) fn remove_staged(&self, name: &std::ffi::OsStr) {
        match self {
            Self::Path(path) => {
                let _ = std::fs::remove_file(path.with_file_name(name));
            }
            #[cfg(unix)]
            Self::Handle(parent) => parent.remove_staged(name),
        }
    }

    /// Installs the staged sibling as the target: a rename over an existing
    /// file, a link for a new one.
    pub(crate) fn install(
        &self,
        staged: &std::ffi::OsStr,
        file: &std::fs::File,
        existing: bool,
    ) -> Result<(), TargetError> {
        let _ = file;
        match self {
            Self::Path(path) => {
                #[cfg(windows)]
                if existing {
                    let parent =
                        crate::file_storage::windows::open_root(path.parent().ok_or_else(
                            || TargetError::Io(std::io::Error::other("a target has a parent")),
                        )?)
                        .map_err(|error| TargetError::Io(std::io::Error::other(error)))?;
                    return crate::file_storage::windows::replace(
                        file,
                        &parent,
                        &staged.to_owned(),
                        &parent,
                        &path
                            .file_name()
                            .ok_or_else(|| {
                                TargetError::Io(std::io::Error::other("a target names a file"))
                            })?
                            .to_owned(),
                    )
                    .map_err(TargetError::Io);
                }
                let result = if existing {
                    std::fs::rename(path.with_file_name(staged), path)
                } else {
                    std::fs::hard_link(path.with_file_name(staged), path)
                };
                result.map_err(TargetError::Io)
            }
            #[cfg(unix)]
            Self::Handle(parent) => parent.install(staged, existing),
        }
    }

    /// Flushes the installed directory entry to disk.
    pub(crate) fn sync_parent(&self) -> Result<(), TargetError> {
        match self {
            Self::Path(path) => {
                std::fs::File::open(path.parent().expect("resolved target has parent"))
                    .and_then(|parent| parent.sync_all())
                    .map_err(TargetError::Io)
            }
            #[cfg(unix)]
            Self::Handle(parent) => parent.dir.sync_all().map_err(TargetError::Io),
        }
    }

    /// Re-checks under the preparation that the target is still a regular
    /// file, not a symlink.
    pub(crate) fn final_regular(&self) -> bool {
        match self {
            Self::Path(path) => std::fs::symlink_metadata(path)
                .is_ok_and(|metadata| metadata.is_file() && !metadata.is_symlink()),
            #[cfg(unix)]
            Self::Handle(parent) => parent.final_regular(),
        }
    }
}

/// Per-path FIFO mutation locks shared by `write` and `edit` (TS `locks.ts`).
#[derive(Default)]
pub struct FileWriteCoordinator {
    entries: Mutex<HashMap<PathBuf, Arc<tokio::sync::Mutex<()>>>>,
}
struct PathLock {
    locks: Arc<FileWriteCoordinator>,
    path: PathBuf,
    entry: Option<Arc<tokio::sync::Mutex<()>>>,
    guard: Option<OwnedMutexGuard<()>>,
}
impl FileWriteCoordinator {
    /// Waits in FIFO order; a cancelled waiter returns at once.
    async fn lock(
        self: &Arc<Self>,
        path: &Path,
        cancel: &CancellationToken,
    ) -> Result<PathLock, HostError> {
        let entry = self
            .entries
            .lock()
            .unwrap()
            .entry(path.to_owned())
            .or_default()
            .clone();
        let mut lock = PathLock {
            locks: self.clone(),
            path: path.to_owned(),
            entry: Some(entry),
            guard: None,
        };
        let acquire = lock.entry.as_ref().unwrap().clone().lock_owned();
        lock.guard = tokio::select! {
            biased;
            _ = cancel.cancelled() => None,
            guard = acquire => Some(guard),
        };
        if lock.guard.is_none() {
            return Err(cancelled());
        }
        Ok(lock)
    }
}
impl Drop for PathLock {
    fn drop(&mut self) {
        let mut entries = self.locks.entries.lock().unwrap();
        self.guard.take();
        self.entry.take();
        if entries
            .get(&self.path)
            .is_some_and(|entry| Arc::strong_count(entry) == 1)
        {
            entries.remove(&self.path);
        }
    }
}

#[cfg(all(test, unix))]
mod mutation_tests {
    use super::*;
    use dim_agent_runtime::{MemoryTimelineStore, Timeline, TimelineKey};
    use rutis::host::CtxHost;

    /// One branch with its own empty Timeline.
    fn branch() -> FileReads {
        FileReads::new(Arc::new(Timeline::new(
            rutis::Ctx::root().unwrap(),
            TimelineKey {
                session_id: "mutation".into(),
                branch_id: "main".into(),
            },
            Vec::new(),
            Arc::new(MemoryTimelineStore::default()),
        )))
    }

    async fn call(
        tools: &FileTools,
        reads: &FileReads,
        name: &str,
        input: Value,
        cancel: CancellationToken,
    ) -> Result<ToolResult, HostError> {
        tools
            .execute(
                &ToolCall {
                    id: "mutation".into(),
                    name: name.into(),
                    input: input.as_object().unwrap().clone(),
                },
                reads,
                cancel,
            )
            .await
    }

    #[tokio::test]
    async fn staged_mutations_fail_closed() {
        for name in ["write", "edit"] {
            for scenario in [
                "external",
                "replacement",
                "cancel",
                "absent",
                "commit_failure",
                "permissions",
                "temp_swap",
                #[cfg(target_os = "macos")]
                "metadata",
            ] {
                if name == "edit" && scenario == "absent" {
                    continue;
                }
                let root = std::env::temp_dir().join(format!("dim-stage-{}", uuid::Uuid::new_v4()));
                std::fs::create_dir(&root).unwrap();
                let root = root.canonicalize().unwrap();
                let target = root.join("file");
                let mut tools = FileTools::new(root.clone(), root.clone(), root.join("out"));
                let reads = branch();
                if scenario != "absent" {
                    std::fs::write(&target, b"before\r\n").unwrap();
                    call(
                        &tools,
                        &reads,
                        "read",
                        json!({"path": "file"}),
                        CancellationToken::new(),
                    )
                    .await
                    .unwrap();
                }
                let cancel = CancellationToken::new();
                let cancellation = cancel.clone();
                tools.before_commit = Some(Arc::new(move |path| match scenario {
                    "external" | "absent" => std::fs::write(path, b"external").unwrap(),
                    "replacement" => {
                        let replacement = path.with_file_name("replacement");
                        std::fs::write(&replacement, b"before\r\n").unwrap();
                        std::fs::rename(replacement, path).unwrap();
                    }
                    "cancel" => cancellation.cancel(),
                    "permissions" => {
                        use std::os::unix::fs::PermissionsExt;
                        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o400))
                            .unwrap();
                    }
                    #[cfg(target_os = "macos")]
                    "metadata" => {
                        assert!(
                            std::process::Command::new("xattr")
                                .args(["-w", "com.dim.concurrent", "changed"])
                                .arg(path)
                                .status()
                                .unwrap()
                                .success()
                        );
                    }
                    "temp_swap" => {
                        for entry in std::fs::read_dir(path.parent().unwrap()).unwrap() {
                            let entry = entry.unwrap();
                            if entry
                                .file_name()
                                .to_string_lossy()
                                .starts_with(".dim-write-")
                            {
                                std::fs::remove_file(entry.path()).unwrap();
                                std::fs::write(entry.path(), b"foreign").unwrap();
                            }
                        }
                    }
                    "commit_failure" => {
                        for entry in std::fs::read_dir(path.parent().unwrap()).unwrap() {
                            let entry = entry.unwrap();
                            if entry
                                .file_name()
                                .to_string_lossy()
                                .starts_with(".dim-write-")
                            {
                                std::fs::remove_file(entry.path()).unwrap();
                            }
                        }
                    }
                    _ => unreachable!(),
                }));
                let input = if name == "write" {
                    json!({"path": "file", "content": "after\n"})
                } else {
                    json!({"path": "file", "oldText": "before", "newText": "after"})
                };
                let result = call(&tools, &reads, name, input, cancel).await;
                if scenario == "cancel" {
                    assert!(matches!(result, Err(HostError::Cancelled(_))));
                } else {
                    let result = result.unwrap();
                    assert_eq!(result.is_error, Some(true));
                    assert_eq!(
                        result.structured_content.unwrap()["code"],
                        if scenario == "absent" {
                            "text_binding_required"
                        } else if matches!(scenario, "commit_failure" | "temp_swap") {
                            "write_error"
                        } else {
                            "file_modified_since_read"
                        }
                    );
                }
                assert_eq!(
                    std::fs::read(&target).unwrap(),
                    if matches!(scenario, "external" | "absent") {
                        b"external".as_slice()
                    } else {
                        b"before\r\n".as_slice()
                    }
                );
                assert_eq!(
                    std::fs::read_dir(&root).unwrap().count(),
                    if scenario == "temp_swap" { 2 } else { 1 },
                    "temporary artifact in {scenario}"
                );
                if scenario == "temp_swap" {
                    let foreign = std::fs::read_dir(&root)
                        .unwrap()
                        .map(Result::unwrap)
                        .find(|entry| {
                            entry
                                .file_name()
                                .to_string_lossy()
                                .starts_with(".dim-write-")
                        })
                        .unwrap();
                    assert_eq!(std::fs::read(foreign.path()).unwrap(), b"foreign");
                }
                std::fs::remove_dir_all(root).unwrap();
            }
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn coordinated_sessions_wait_through_preparation() {
        let root = std::env::temp_dir().join(format!("dim-coordinate-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let root = root.canonicalize().unwrap();
        std::fs::write(root.join("file"), b"before").unwrap();
        let coordinator: Arc<FileWriteCoordinator> = Arc::default();
        let mut first = FileTools::with_coordinator(
            root.clone(),
            root.clone(),
            root.join("out"),
            coordinator.clone(),
            None,
        );
        let second = FileTools::with_coordinator(
            root.clone(),
            root.clone(),
            root.join("out"),
            coordinator,
            None,
        );
        let (a, b) = (branch(), branch());
        for (tools, reads) in [(&first, &a), (&second, &b)] {
            call(
                tools,
                reads,
                "read",
                json!({"path": "file"}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        }
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let release = barrier.clone();
        let (ready, prepared) = tokio::sync::oneshot::channel();
        let ready = Mutex::new(Some(ready));
        first.before_commit = Some(Arc::new(move |_| {
            ready.lock().unwrap().take().unwrap().send(()).unwrap();
            release.wait();
        }));
        let first = tokio::spawn(async move {
            call(
                &first,
                &a,
                "write",
                json!({"path": "file", "content": "one"}),
                CancellationToken::new(),
            )
            .await
        });
        prepared.await.unwrap();
        let second_result = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            call(
                &second,
                &b,
                "write",
                json!({"path": "file", "content": "two"}),
                CancellationToken::new(),
            ),
        )
        .await;
        barrier.wait();
        let first_result = first.await.unwrap().unwrap();
        std::fs::remove_dir_all(root).unwrap();
        assert!(
            second_result.is_err(),
            "another Session entered during preparation"
        );
        assert_eq!(first_result.is_error, None);
    }
}

/// The queued-mutation regressions: a bounded write and edit admit one
/// canonical path, then a parent component is swapped for a symlink to an
/// outside sibling while the call waits on the per-path coordinator. The I/O
/// must act on what was admitted — the walk fails closed — and the outside
/// file must stay untouched. They need the internal coordinator and its
/// before-lock hook, so they stay in-crate.
#[cfg(all(test, unix))]
mod queued_boundary_tests {
    use super::*;
    use crate::workspace_tools::files::{FileToolBoundary, FileTools};
    use dim_agent_runtime::{MemoryTimelineStore, Timeline, TimelineKey};
    use rutis::host::CtxHost;

    /// One branch with its own empty Timeline; one per test, so read
    /// bindings survive between the calls of a test.
    fn reads() -> FileReads {
        FileReads::new(Arc::new(Timeline::new(
            rutis::Ctx::root().unwrap(),
            TimelineKey {
                session_id: "queued".into(),
                branch_id: "main".into(),
            },
            Vec::new(),
            Arc::new(MemoryTimelineStore::default()),
        )))
    }

    struct Fixture {
        root: PathBuf,
        workspace: PathBuf,
        outside: PathBuf,
        boundary: FileToolBoundary,
    }

    fn fixture() -> Fixture {
        let root = std::env::temp_dir().join(format!("dim-queued-{}", uuid::Uuid::new_v4()));
        for dir in ["workspace/sub", "home", "outside"] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        std::fs::write(root.join("workspace/sub/note.txt"), "before").unwrap();
        std::fs::write(root.join("outside/note.txt"), "before").unwrap();
        let root = root.canonicalize().unwrap();
        let workspace = root.join("workspace");
        Fixture {
            boundary: FileToolBoundary {
                workspace: workspace.clone(),
                home: root.join("home"),
                read_only_roots: Vec::new(),
            },
            workspace,
            outside: root.join("outside"),
            root,
        }
    }

    fn tools(fixture: &Fixture, coordinator: Arc<FileWriteCoordinator>) -> FileTools {
        FileTools::with_coordinator(
            fixture.workspace.clone(),
            fixture.boundary.home.clone(),
            fixture.workspace.join("out"),
            coordinator,
            Some(fixture.boundary.clone()),
        )
    }

    async fn call(tools: &FileTools, reads: &FileReads, name: &str, input: Value) -> ToolResult {
        tools
            .execute(
                &ToolCall {
                    id: "queued".into(),
                    name: name.into(),
                    input: input.as_object().unwrap().clone(),
                },
                reads,
                CancellationToken::new(),
            )
            .await
            .unwrap()
    }

    fn assert_refused(result: &ToolResult) {
        assert_eq!(result.is_error, Some(true), "{result:?}");
        assert_eq!(
            result.structured_content.as_ref().unwrap()["code"],
            "outside_file_roots",
            "{result:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_queued_write_refuses_a_parent_swap() {
        let fixture = fixture();
        let coordinator: Arc<FileWriteCoordinator> = Arc::default();
        let mut tools = tools(&fixture, coordinator.clone());
        let reads = reads();
        let target = fixture.workspace.join("sub/escaped.txt");
        let cancel = CancellationToken::new();
        let held = coordinator.lock(&target, &cancel).await.unwrap();
        let (queued, release) = tokio::sync::oneshot::channel();
        let queued = Mutex::new(Some(queued));
        tools.before_lock = Some(Arc::new(move |_| {
            if let Some(queued) = queued.lock().unwrap().take() {
                let _ = queued.send(());
            }
        }));
        let (workspace, outside) = (fixture.workspace.clone(), fixture.outside.clone());
        let task = tokio::spawn(async move {
            call(
                &tools,
                &reads,
                "write",
                json!({"path": "sub/escaped.txt", "content": "escaped\n"}),
            )
            .await
        });
        release.await.unwrap();
        // The caller resolved its target and now waits on the held
        // coordinator: swap the parent for a symlink to the outside sibling.
        std::fs::rename(workspace.join("sub"), workspace.join("sub-moved")).unwrap();
        std::os::unix::fs::symlink(&outside, workspace.join("sub")).unwrap();
        drop(held);
        let result = task.await.unwrap();
        assert_refused(&result);
        assert!(
            !outside.join("escaped.txt").exists(),
            "the outside file must not be created"
        );
        assert!(
            !workspace.join("sub-moved/escaped.txt").exists(),
            "the moved directory must not receive the write"
        );
        std::fs::remove_dir_all(&fixture.root).unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_queued_edit_refuses_a_parent_swap() {
        let fixture = fixture();
        let coordinator: Arc<FileWriteCoordinator> = Arc::default();
        let mut tools = tools(&fixture, coordinator.clone());
        let reads = reads();
        let binding = call(&tools, &reads, "read", json!({"path": "sub/note.txt"})).await;
        assert_ne!(binding.is_error, Some(true), "{binding:?}");
        let target = fixture.workspace.join("sub/note.txt");
        let cancel = CancellationToken::new();
        let held = coordinator.lock(&target, &cancel).await.unwrap();
        let (queued, release) = tokio::sync::oneshot::channel();
        let queued = Mutex::new(Some(queued));
        tools.before_lock = Some(Arc::new(move |_| {
            if let Some(queued) = queued.lock().unwrap().take() {
                let _ = queued.send(());
            }
        }));
        let (workspace, outside) = (fixture.workspace.clone(), fixture.outside.clone());
        let task = tokio::spawn(async move {
            call(
                &tools,
                &reads,
                "edit",
                json!({"path": "sub/note.txt", "oldText": "before", "newText": "changed"}),
            )
            .await
        });
        release.await.unwrap();
        std::fs::rename(workspace.join("sub"), workspace.join("sub-moved")).unwrap();
        std::os::unix::fs::symlink(&outside, workspace.join("sub")).unwrap();
        drop(held);
        let result = task.await.unwrap();
        assert_refused(&result);
        assert_eq!(
            std::fs::read_to_string(outside.join("note.txt")).unwrap(),
            "before",
            "the outside bytes must be unchanged"
        );
        std::fs::remove_dir_all(&fixture.root).unwrap();
    }
}

/// The bounded-search regression: `grep` reads each file it matches through
/// the admitted walk, so a subdirectory swapped for a symlink to an outside
/// sibling after the walk listed it yields no content from outside. The
/// hook runs inside the walk on each entry, before the entry is read; no
/// sleeps. In-crate because the hook is a private test field.
#[cfg(all(test, unix))]
mod bounded_search_tests {
    use super::*;
    use crate::workspace_tools::files::{FileToolBoundary, FileTools};
    use dim_agent_runtime::{MemoryTimelineStore, Timeline, TimelineKey};
    use rutis::host::CtxHost;

    fn reads() -> FileReads {
        FileReads::new(Arc::new(Timeline::new(
            rutis::Ctx::root().unwrap(),
            TimelineKey {
                session_id: "search".into(),
                branch_id: "main".into(),
            },
            Vec::new(),
            Arc::new(MemoryTimelineStore::default()),
        )))
    }

    async fn call(tools: &FileTools, input: Value) -> ToolResult {
        tools
            .execute(
                &ToolCall {
                    id: "search".into(),
                    name: "grep".into(),
                    input: input.as_object().unwrap().clone(),
                },
                &reads(),
                CancellationToken::new(),
            )
            .await
            .unwrap()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_bounded_grep_reads_no_file_outside_the_roots() {
        let root = std::env::temp_dir().join(format!("dim-search-{}", uuid::Uuid::new_v4()));
        for dir in ["workspace/sub", "home", "outside"] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        std::fs::write(root.join("workspace/sub/note.txt"), "NEEDLE inside").unwrap();
        std::fs::write(root.join("outside/note.txt"), "NEEDLE outside").unwrap();
        let root = root.canonicalize().unwrap();
        let workspace = root.join("workspace");
        let outside = root.join("outside");
        let boundary = FileToolBoundary {
            workspace: workspace.clone(),
            home: root.join("home"),
            read_only_roots: Vec::new(),
        };
        let mut tools =
            FileTools::with_boundary(workspace.clone(), workspace.join("out"), boundary);
        // Control: the file inside the workspace is read.
        let found = call(&tools, json!({"pattern": "NEEDLE", "path": "sub"})).await;
        let matches = found.structured_content.as_ref().unwrap()["matches"]
            .as_array()
            .unwrap()
            .clone();
        assert_eq!(matches.len(), 1, "{found:?}");
        assert_eq!(matches[0]["content"], "NEEDLE inside");
        // The walk lists `sub`, then the hook swaps it for a symlink to the
        // outside sibling before its contents are read.
        let link = workspace.join("sub");
        tools.before_search = Some(Arc::new(move |path| {
            if path == link.as_path() && !link.is_symlink() {
                let _ = std::fs::remove_file(link.join("note.txt"));
                let _ = std::fs::remove_dir(&link);
                let _ = std::os::unix::fs::symlink(&outside, &link);
            }
        }));
        let escaped = call(&tools, json!({"pattern": "NEEDLE", "path": "."})).await;
        let structured = escaped.structured_content.as_ref().unwrap().clone();
        assert!(
            structured["matches"].as_array().unwrap().is_empty(),
            "no match may come from outside the roots: {structured:?}"
        );
        assert!(
            !serde_json::to_string(&structured)
                .unwrap()
                .contains("NEEDLE outside"),
            "{structured:?}"
        );
        std::fs::remove_dir_all(&root).unwrap();
    }
}

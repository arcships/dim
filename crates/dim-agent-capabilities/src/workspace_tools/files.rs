//! The file tools `read`, `write`, `edit`, `glob` and `grep`.
//!
//! Behavior, texts, error codes and limits follow the TS tools
//! (`packages/agent/src/tool/builtin/` and `packages/plugin/src/builtin/grep-glob/`).
//! Workspace I/O runs on blocking threads with `std::fs`.

#![expect(
    clippy::result_large_err,
    reason = "the `Err` variant is `ToolResult`, the model-facing failure reply; boxing it would change the tools' contract"
)]

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
#[cfg_attr(
    windows,
    expect(unused_mut, reason = "only the unix branch sets the open flags")
)]
fn open_options() -> std::fs::OpenOptions {
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
#[derive(Debug)]
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

/// The staged file's name inside the private directory a new target is
/// prepared in.
#[cfg(unix)]
const STAGED_FILE: &str = "staged";

/// The extended ACL of an open file, read and applied through its descriptor.
/// macOS keeps inheritable access control entries separately from the mode
/// bits, so a file created inside the private staging directory would
/// otherwise carry the ACL the directory inherited from the target's parent
/// instead of the ACL the target itself would get.
#[cfg(target_os = "macos")]
mod acl {
    use std::ffi::c_void;
    use std::os::fd::AsRawFd;

    /// `ACL_TYPE_EXTENDED` from `<sys/acl.h>`.
    const ACL_TYPE_EXTENDED: libc::c_int = 0x100;
    /// `ACL_FIRST_ENTRY` from `<sys/acl.h>`.
    const ACL_FIRST_ENTRY: libc::c_int = 0;

    unsafe extern "C" {
        fn acl_get_fd_np(fd: libc::c_int, kind: libc::c_int) -> *mut c_void;
        fn acl_set_fd_np(fd: libc::c_int, acl: *mut c_void, kind: libc::c_int) -> libc::c_int;
        fn acl_init(count: libc::c_int) -> *mut c_void;
        fn acl_free(object: *mut c_void) -> libc::c_int;
        fn acl_get_entry(
            acl: *mut c_void,
            entry: libc::c_int,
            entry_p: *mut *mut c_void,
        ) -> libc::c_int;
    }

    /// An ACL returned by libSystem and freed when dropped.
    pub(super) struct Acl(*mut c_void);

    impl Acl {
        /// The extended ACL `file` carries. A file or file system without
        /// extended ACLs reads as `None`.
        pub(super) fn of(file: &std::fs::File) -> std::io::Result<Option<Self>> {
            // SAFETY: `file` is open for the call and the returned object is
            // owned by the wrapper until it is freed.
            let acl = unsafe { acl_get_fd_np(file.as_raw_fd(), ACL_TYPE_EXTENDED) };
            if !acl.is_null() {
                return Ok(Some(Self(acl)));
            }
            let error = std::io::Error::last_os_error();
            match error.raw_os_error() {
                Some(libc::ENOENT | libc::ENOTSUP) => Ok(None),
                _ => Err(error),
            }
        }

        /// Whether the ACL holds at least one entry.
        pub(super) fn entries(&self) -> std::io::Result<bool> {
            let mut entry = std::ptr::null_mut();
            // SAFETY: the ACL is live for the call and `entry` is a valid out
            // pointer.
            let result = unsafe { acl_get_entry(self.0, ACL_FIRST_ENTRY, &mut entry) };
            if result == 0 {
                return Ok(true);
            }
            let error = std::io::Error::last_os_error();
            match error.raw_os_error() {
                Some(libc::ENOENT) => Ok(false),
                _ => Err(error),
            }
        }

        /// Replaces `file`'s extended ACL with this one.
        pub(super) fn apply(&self, file: &std::fs::File) -> std::io::Result<()> {
            // SAFETY: the ACL is live for the call, and `file` is open.
            if unsafe { acl_set_fd_np(file.as_raw_fd(), self.0, ACL_TYPE_EXTENDED) } == 0 {
                Ok(())
            } else {
                Err(std::io::Error::last_os_error())
            }
        }

        /// Removes `file`'s extended ACL when it carries one and confirms no
        /// entry is left.
        pub(super) fn clear_if_present(file: &std::fs::File) -> std::io::Result<()> {
            match Self::of(file)? {
                Some(acl) if acl.entries()? => Self::clear(file),
                _ => Ok(()),
            }
        }

        /// Removes `file`'s extended ACL and confirms no entry is left.
        fn clear(file: &std::fs::File) -> std::io::Result<()> {
            // SAFETY: `acl_init` allocates an ACL the wrapper frees.
            let empty = unsafe { acl_init(0) };
            if empty.is_null() {
                return Err(std::io::Error::last_os_error());
            }
            Self(empty).apply(file)?;
            match Self::of(file)? {
                Some(acl) if acl.entries()? => Err(std::io::Error::other(
                    "the staging directory still carries an access control entry",
                )),
                _ => Ok(()),
            }
        }
    }

    impl Drop for Acl {
        fn drop(&mut self) {
            // SAFETY: the pointer came from libSystem and is freed once here.
            unsafe { acl_free(self.0) };
        }
    }
}

/// The private directory a new target's file is staged in, opened relative to
/// the target's parent and checked before anything is written into it. Every
/// operation on the staged file runs relative to `dir`, so a name swapped after
/// creation cannot receive the staged bytes or redirect the installation.
#[cfg(unix)]
struct StagingDirectory {
    /// The target's parent directory, relative to which the private
    /// directory's entry is removed.
    parent: std::fs::File,
    /// The opened, checked private directory itself.
    dir: std::fs::File,
    /// The extended ACL the target inherits from its parent, applied to the
    /// staged file immediately before installation on macOS.
    #[cfg(target_os = "macos")]
    acl: Option<acl::Acl>,
}

/// Creates the private staging directory `name` below `parent` with mode 0700,
/// then sets that mode again without following a symlink, so a umask that
/// masks owner bits cannot take away the access the checked open needs.
#[cfg(unix)]
fn create_staging_directory(
    parent: &std::fs::File,
    name: &std::ffi::OsStr,
) -> Result<(), TargetError> {
    use std::os::fd::AsRawFd;
    let name = c_name(name)?;
    // SAFETY: `parent` is an open directory handle and `name` a live
    // NUL-terminated string for the call.
    if unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o700) } != 0 {
        return Err(classify(std::io::Error::last_os_error()));
    }
    // SAFETY: `parent` is an open directory handle and `name` a live
    // NUL-terminated string for the call; `AT_SYMLINK_NOFOLLOW` keeps a name
    // swapped after creation from redirecting the mode change.
    if unsafe {
        libc::fchmodat(
            parent.as_raw_fd(),
            name.as_ptr(),
            0o700,
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } != 0
    {
        return Err(classify(std::io::Error::last_os_error()));
    }
    Ok(())
}

/// Opens the staging directory `name` below `parent` without following a
/// symlink and checks that it is a directory the effective user owns whose
/// group and other permission bits are all clear. A name swapped between the
/// creation and this open is refused here unless it names such a directory.
#[cfg(unix)]
fn open_staging_directory(
    parent: &std::fs::File,
    name: &std::ffi::OsStr,
) -> Result<std::fs::File, TargetError> {
    use std::os::unix::fs::MetadataExt;
    let directory =
        open_relative(parent, name, libc::O_RDONLY | libc::O_DIRECTORY, 0).map_err(classify)?;
    let metadata = directory.metadata().map_err(TargetError::Io)?;
    // SAFETY: `geteuid` takes no arguments and cannot fail.
    let owner = unsafe { libc::geteuid() };
    if metadata.is_dir() && metadata.uid() == owner && metadata.mode() & 0o077 == 0 {
        Ok(directory)
    } else {
        Err(TargetError::Io(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "the private staging directory is not the one this call created",
        )))
    }
}

/// Creates the staged file inside the private directory `name` below `parent`,
/// which is checked before the file exists. On macOS, an ACL the opened
/// directory carries is cleared and read back before the file exists, the ACL
/// the target inherits is read from an empty template, and the opened file is
/// confirmed to carry no entry. A failure after the directory was opened
/// removes the directory; a directory that cannot be opened is left in place,
/// because the name cannot be shown to be the one this call created.
#[cfg(unix)]
fn create_staged_file(
    parent: &std::fs::File,
    name: &std::ffi::OsStr,
) -> Result<(std::fs::File, StagingDirectory), TargetError> {
    create_staged_file_observed(parent, name, || {})
}

/// [`create_staged_file`] with `observed` run after the parent's ACL was read
/// and before the private directory is created, so a test can change the
/// parent between those two observations.
#[cfg(unix)]
fn create_staged_file_observed(
    parent: &std::fs::File,
    name: &std::ffi::OsStr,
    observed: impl FnOnce(),
) -> Result<(std::fs::File, StagingDirectory), TargetError> {
    let parent = parent.try_clone().map_err(classify)?;
    #[cfg(target_os = "macos")]
    let inherited = inherited_file_acl(&parent, name)?;
    observed();
    create_staging_directory(&parent, name)?;
    let directory = open_staging_directory(&parent, name)?;
    #[cfg(target_os = "macos")]
    if let Err(error) = acl::Acl::clear_if_present(&directory) {
        remove_staging_directory(&parent, name, &directory);
        return Err(classify(error));
    }
    let created = open_relative(
        &directory,
        std::ffi::OsStr::new(STAGED_FILE),
        libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
        0o666,
    );
    match created {
        Ok(file) => {
            let staging = StagingDirectory {
                parent,
                dir: directory,
                #[cfg(target_os = "macos")]
                acl: inherited,
            };
            #[cfg(target_os = "macos")]
            if let Err(error) = require_no_acl(&file) {
                let _ = remove_file_entry(&staging.dir, std::ffi::OsStr::new(STAGED_FILE));
                remove_staging_directory(&staging.parent, name, &staging.dir);
                return Err(classify(error));
            }
            Ok((file, staging))
        }
        Err(error) => {
            remove_staging_directory(&parent, name, &directory);
            Err(classify(error))
        }
    }
}

/// Reads the extended ACL a file created directly in `parent` would inherit,
/// from an empty template created there and removed at once. A parent without
/// entries takes the existing path and does no ACL work.
#[cfg(target_os = "macos")]
fn inherited_file_acl(
    parent: &std::fs::File,
    staging: &std::ffi::OsStr,
) -> Result<Option<acl::Acl>, TargetError> {
    match acl::Acl::of(parent).map_err(TargetError::Io)? {
        Some(acl) => {
            if !acl.entries().map_err(TargetError::Io)? {
                return Ok(None);
            }
        }
        None => return Ok(None),
    }
    let mut name = staging.to_owned();
    name.push(".acl");
    let template = open_relative(
        parent,
        &name,
        libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
        0o666,
    )
    .map_err(classify)?;
    let file = acl::Acl::of(&template).map_err(TargetError::Io);
    let removed = remove_file_entry(parent, &name).map_err(TargetError::Io);
    let file = file?;
    removed?;
    Ok(file)
}

/// Confirms `file` carries no extended ACL entry.
#[cfg(target_os = "macos")]
fn require_no_acl(file: &std::fs::File) -> std::io::Result<()> {
    match acl::Acl::of(file)? {
        Some(acl) if acl.entries()? => Err(std::io::Error::other(
            "the staged file inherited an access control entry",
        )),
        _ => Ok(()),
    }
}

/// The directory entry `name` below `dir`, statted without following a
/// symlink.
#[cfg(unix)]
fn entry_stat(dir: &std::fs::File, name: &std::ffi::OsStr) -> Option<libc::stat> {
    use std::os::fd::AsRawFd;
    let name = c_name(name).ok()?;
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: `dir` and the NUL-terminated name are live for the call, and
    // `stat` is a valid uninitialized `libc::stat` buffer.
    let result = unsafe {
        libc::fstatat(
            dir.as_raw_fd(),
            name.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if result < 0 {
        return None;
    }
    // SAFETY: `fstatat` returned 0 (checked above), so it initialized the
    // struct.
    Some(unsafe { stat.assume_init() })
}

/// The device and inode `stat` names.
#[cfg(unix)]
fn device_inode(stat: &libc::stat) -> (u64, u64) {
    #[expect(
        clippy::unnecessary_cast,
        reason = "`st_dev` is 32-bit on macOS and 64-bit on Linux, so its cast is needed on one and lints on the other; `st_ino` is 64-bit on both"
    )]
    let identity = (stat.st_dev as u64, stat.st_ino as u64);
    identity
}

/// Whether the entry `name` below `parent` is still the directory `dir`, by
/// device and inode, checked without following a symlink.
#[cfg(unix)]
fn staging_directory_owned_in(
    parent: &std::fs::File,
    name: &std::ffi::OsStr,
    dir: &std::fs::File,
) -> bool {
    use std::os::unix::fs::MetadataExt;
    let (Ok(metadata), Some(stat)) = (dir.metadata(), entry_stat(parent, name)) else {
        return false;
    };
    stat.st_mode & libc::S_IFMT == libc::S_IFDIR
        && device_inode(&stat) == (metadata.dev(), metadata.ino())
}

/// Removes the empty staging directory entry `name` below `parent` when it is
/// still the directory `dir` this call opened. A name swapped after the
/// directory was opened is left in place rather than removing another entry;
/// nothing below the entry is removed, and a failure is not reported, because
/// the call may already have installed the target. The entry can be replaced
/// again between the ownership check and the unlink, which can remove another
/// empty directory; a non-empty or non-directory entry fails instead.
#[cfg(unix)]
fn remove_staging_directory(parent: &std::fs::File, name: &std::ffi::OsStr, dir: &std::fs::File) {
    use std::os::fd::AsRawFd;
    if !staging_directory_owned_in(parent, name, dir) {
        return;
    }
    if let Ok(name) = c_name(name) {
        // SAFETY: `parent` is an open directory handle and `name` a live
        // NUL-terminated string for the call.
        unsafe { libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), libc::AT_REMOVEDIR) };
    }
}

/// Removes the file entry `name` below `parent`. Cleanup callers ignore a
/// failure, because the entry may already be gone.
#[cfg(unix)]
fn remove_file_entry(parent: &std::fs::File, name: &std::ffi::OsStr) -> std::io::Result<()> {
    use std::os::fd::AsRawFd;
    let name = c_name(name)?;
    // SAFETY: `parent` is an open directory handle and `name` a live
    // NUL-terminated string for the call.
    if unsafe { libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), 0) } == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// Whether `file` is still the regular file `name` in `dir`, checked without
/// following a symlink and by device and inode.
#[cfg(unix)]
fn staged_owned_in(dir: &std::fs::File, name: &std::ffi::OsStr, file: &std::fs::File) -> bool {
    use std::os::unix::fs::MetadataExt;
    let (Ok(metadata), Some(stat)) = (file.metadata(), entry_stat(dir, name)) else {
        return false;
    };
    stat.st_mode & libc::S_IFMT == libc::S_IFREG
        && device_inode(&stat) == (metadata.dev(), metadata.ino())
}

/// Applies the ACL the target inherits from its parent to the staged file and
/// links it at `name` in the staging directory's parent, which never replaces
/// an existing entry. The ACL is applied immediately before the link, while the
/// file is still inside the private directory.
#[cfg(unix)]
fn link_staged(
    staging: &StagingDirectory,
    file: &std::fs::File,
    name: &std::ffi::OsStr,
) -> Result<(), TargetError> {
    use std::os::fd::AsRawFd;
    let staged = c_name(std::ffi::OsStr::new(STAGED_FILE))?;
    let name = c_name(name)?;
    #[cfg(target_os = "macos")]
    if let Some(acl) = &staging.acl {
        acl.apply(file).map_err(TargetError::Io)?;
    }
    #[cfg(not(target_os = "macos"))]
    let _ = file;
    // SAFETY: both handles are open directory handles and both names are live
    // NUL-terminated strings for the call.
    let result = unsafe {
        libc::linkat(
            staging.dir.as_raw_fd(),
            staged.as_ptr(),
            staging.parent.as_raw_fd(),
            name.as_ptr(),
            0,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(TargetError::Io(std::io::Error::last_os_error()))
    }
}

/// Creates the staged file for `path` where [`Target::create_staged`]
/// documents it: a new target's file lives in a checked private directory in
/// the target's parent.
#[cfg(unix)]
fn staged_path_file(
    path: &Path,
    name: &std::ffi::OsStr,
    existing: bool,
) -> Result<(std::fs::File, Option<StagingDirectory>), TargetError> {
    if existing {
        let mut options = open_options();
        options.write(true).create_new(true);
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        return options
            .open(path.with_file_name(name))
            .map(|file| (file, None))
            .map_err(TargetError::Io);
    }
    let parent_path = path
        .parent()
        .ok_or_else(|| TargetError::Io(std::io::Error::other("a target has a parent")))?;
    let parent = std::fs::File::open(parent_path).map_err(TargetError::Io)?;
    create_staged_file(&parent, name).map(|(file, staging)| (file, Some(staging)))
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

    fn create_staged(
        &self,
        name: &std::ffi::OsStr,
        existing: bool,
    ) -> Result<(std::fs::File, Option<StagingDirectory>), TargetError> {
        if existing {
            return open_relative(
                &self.dir,
                name,
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
                0o600,
            )
            .map(|file| (file, None))
            .map_err(classify);
        }
        create_staged_file(&self.dir, name).map(|(file, staging)| (file, Some(staging)))
    }

    fn staged_owned(&self, name: &std::ffi::OsStr, file: &std::fs::File) -> bool {
        staged_owned_in(&self.dir, name, file)
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

    /// Creates the staged file `name`. A replaced target's staged file is
    /// created with `0o600` directly in its parent, and takes the original's
    /// owner, group and mode before installation, so its bytes stay private. A
    /// new target's file is created with `0o666` inside a private directory in
    /// its parent: the process umask and the parent's default ACLs decide the
    /// installed file's mode and ACL, and the directory is opened and checked
    /// as a directory owned by the effective user with no group or other
    /// access, so nobody else can read or change the staged bytes before
    /// installation. On macOS the target's inheritable ACL is read from a
    /// template in the parent, the directory's inherited ACL is cleared before
    /// the staged file exists, and the target's ACL is applied to the staged
    /// file immediately before installation.
    pub(crate) fn create_staged(
        &self,
        name: std::ffi::OsString,
        existing: bool,
    ) -> Result<Staged<'_>, TargetError> {
        #[cfg(unix)]
        let (file, staging) = self.staged_file(&name, existing)?;
        #[cfg(not(unix))]
        let file = self.staged_file(&name, existing)?;
        Ok(Staged {
            target: self,
            file,
            name,
            #[cfg(unix)]
            staging,
            existing,
        })
    }

    /// Creates the staged file `name` where [`Self::create_staged`] documents
    /// it, with the checked private directory a new target's file lives in.
    #[cfg(unix)]
    fn staged_file(
        &self,
        name: &std::ffi::OsStr,
        existing: bool,
    ) -> Result<(std::fs::File, Option<StagingDirectory>), TargetError> {
        match self {
            Self::Path(path) => staged_path_file(path, name, existing),
            Self::Handle(parent) => parent.create_staged(name, existing),
        }
    }

    /// Creates the staged file `name` where [`Self::create_staged`] documents
    /// it.
    #[cfg(windows)]
    fn staged_file(
        &self,
        name: &std::ffi::OsStr,
        _existing: bool,
    ) -> Result<std::fs::File, TargetError> {
        let Self::Path(path) = self;
        let parent = crate::file_storage::windows::open_root(
            path.parent()
                .ok_or_else(|| TargetError::Io(std::io::Error::other("a target has a parent")))?,
        )
        .map_err(|error| TargetError::Io(std::io::Error::other(error)))?;
        crate::file_storage::windows::create_temp(
            &parent,
            &name.to_owned(),
            &parent,
            &path
                .file_name()
                .ok_or_else(|| TargetError::Io(std::io::Error::other("a target names a file")))?
                .to_owned(),
            None,
        )
        .map_err(TargetError::Io)
    }

    #[cfg(not(any(unix, windows)))]
    fn staged_file(
        &self,
        _name: &std::ffi::OsStr,
        _existing: bool,
    ) -> Result<std::fs::File, TargetError> {
        Err(TargetError::Io(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "staged writes require a Unix or Windows host",
        )))
    }

    fn staged_owned(&self, staged: &Staged) -> bool {
        #[cfg(unix)]
        if let Some(staging) = &staged.staging {
            return staged_owned_in(
                &staging.dir,
                std::ffi::OsStr::new(STAGED_FILE),
                &staged.file,
            );
        }
        match self {
            Self::Path(path) => owned_temporary(&path.with_file_name(&staged.name), &staged.file),
            #[cfg(unix)]
            Self::Handle(parent) => parent.staged_owned(&staged.name, &staged.file),
        }
    }

    /// Removes whatever preparation added to the target's parent directory.
    fn remove_staged(&self, staged: &Staged) {
        #[cfg(unix)]
        if let Some(staging) = &staged.staging {
            let _ = remove_file_entry(&staging.dir, std::ffi::OsStr::new(STAGED_FILE));
            return remove_staging_directory(&staging.parent, &staged.name, &staging.dir);
        }
        match self {
            Self::Path(path) => {
                let _ = std::fs::remove_file(path.with_file_name(&staged.name));
            }
            #[cfg(unix)]
            Self::Handle(parent) => parent.remove_staged(&staged.name),
        }
    }

    /// Installs the staged file as the target: a rename over a replaced
    /// target, a link for a new one, which never replaces an entry.
    fn install(&self, staged: &Staged) -> Result<(), TargetError> {
        match self {
            Self::Path(path) => {
                #[cfg(windows)]
                if staged.existing {
                    let parent =
                        crate::file_storage::windows::open_root(path.parent().ok_or_else(
                            || TargetError::Io(std::io::Error::other("a target has a parent")),
                        )?)
                        .map_err(|error| TargetError::Io(std::io::Error::other(error)))?;
                    return crate::file_storage::windows::replace(
                        &staged.file,
                        &parent,
                        &staged.name,
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
                #[cfg(unix)]
                if let Some(staging) = &staged.staging {
                    return link_staged(
                        staging,
                        &staged.file,
                        path.file_name().ok_or_else(|| {
                            TargetError::Io(std::io::Error::other("a target names a file"))
                        })?,
                    );
                }
                let result = if staged.existing {
                    std::fs::rename(path.with_file_name(&staged.name), path)
                } else {
                    std::fs::hard_link(path.with_file_name(&staged.name), path)
                };
                result.map_err(TargetError::Io)
            }
            #[cfg(unix)]
            Self::Handle(parent) => {
                if let Some(staging) = &staged.staging {
                    return link_staged(staging, &staged.file, &parent.name);
                }
                parent.install(&staged.name, staged.existing)
            }
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

/// A target file that preparation has staged and installation makes the
/// target. Dropping it removes whatever preparation added to the parent
/// directory: the staged file, and the private directory a new target's file is
/// prepared in.
pub(crate) struct Staged<'a> {
    target: &'a Target,
    file: std::fs::File,
    /// The name preparation added to the target's parent directory: the staged
    /// file itself, or the private directory holding it.
    name: std::ffi::OsString,
    /// The checked private directory a new target's file is staged in.
    #[cfg(unix)]
    staging: Option<StagingDirectory>,
    /// Whether the target existed, so installation replaces it.
    existing: bool,
}

impl Staged<'_> {
    pub(crate) fn file(&self) -> &std::fs::File {
        &self.file
    }

    pub(crate) fn file_mut(&mut self) -> &mut std::fs::File {
        &mut self.file
    }

    /// Whether the staged file is still the file preparation created.
    pub(crate) fn owned(&self) -> bool {
        self.target.staged_owned(self)
    }

    /// Installs the staged file as the target.
    pub(crate) fn install(&self) -> Result<(), TargetError> {
        self.target.install(self)
    }
}

impl Drop for Staged<'_> {
    fn drop(&mut self) {
        if self.owned() {
            #[cfg(windows)]
            if let Ok(metadata) = self.file.metadata() {
                let mut permissions = metadata.permissions();
                permissions.set_readonly(false);
                let _ = self.file.set_permissions(permissions);
            }
            self.target.remove_staged(self);
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

    /// What a `before_commit` hook saw of a mutation's staging area.
    struct Preparation {
        directory: bool,
        mode: u32,
        bytes: Vec<u8>,
        target_exists: bool,
    }

    /// The entries preparation added to the target's parent directory.
    fn staged_entries(path: &Path) -> Vec<std::fs::DirEntry> {
        std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(Result::unwrap)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".dim-write-")
            })
            .collect()
    }

    /// The private staging directory is opened through its parent and checked:
    /// a symlink and a directory group or others can enter are refused, and a
    /// directory only its owner can enter is accepted.
    #[cfg(unix)]
    #[test]
    fn the_private_staging_directory_is_opened_and_checked() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!("dim-staging-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let parent = std::fs::File::open(&root).unwrap();
        std::os::unix::fs::symlink(&root, root.join("link")).unwrap();
        assert!(
            open_staging_directory(&parent, std::ffi::OsStr::new("link")).is_err(),
            "a symlink is not a staging directory"
        );
        for (name, mode) in [("wide", 0o755), ("private", 0o700)] {
            std::fs::create_dir(root.join(name)).unwrap();
            std::fs::set_permissions(root.join(name), std::fs::Permissions::from_mode(mode))
                .unwrap();
            assert_eq!(
                open_staging_directory(&parent, std::ffi::OsStr::new(name)).is_ok(),
                mode == 0o700,
                "a {mode:o} directory"
            );
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    /// Operations on a new target run relative to the opened staging directory,
    /// not its name: replacing the name after the handle exists neither
    /// receives the staged bytes nor changes the installed file.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_swapped_staging_directory_name_receives_nothing() {
        use std::os::unix::fs::PermissionsExt;
        for bounded in [false, true] {
            let root = std::env::temp_dir().join(format!("dim-swap-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir(&root).unwrap();
            let root = root.canonicalize().unwrap();
            let mut tools = if bounded {
                FileTools::with_boundary(
                    root.clone(),
                    root.join("out"),
                    FileToolBoundary {
                        workspace: root.clone(),
                        home: root.clone(),
                        read_only_roots: Vec::new(),
                    },
                )
            } else {
                FileTools::new(root.clone(), root.clone(), root.join("out"))
            };
            let reads = branch();
            let swapped: Arc<Mutex<Option<(PathBuf, PathBuf)>>> = Arc::default();
            let seen = swapped.clone();
            tools.before_commit = Some(Arc::new(move |path| {
                let staged = staged_entries(path);
                assert_eq!(staged.len(), 1, "one staged entry for {path:?}");
                let original = staged[0].path();
                let moved = path.with_file_name("moved-staging");
                std::fs::rename(&original, &moved).unwrap();
                std::fs::create_dir(&original).unwrap();
                std::fs::set_permissions(&original, std::fs::Permissions::from_mode(0o700))
                    .unwrap();
                *seen.lock().unwrap() = Some((moved, original));
            }));
            let result = call(
                &tools,
                &reads,
                "write",
                json!({"path": "file", "content": "after\n"}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
            assert_eq!(result.is_error, None, "bounded={bounded}: {result:?}");
            assert_eq!(std::fs::read(root.join("file")).unwrap(), b"after\n");
            let (moved, replacement) = swapped.lock().unwrap().take().unwrap();
            assert!(
                !moved.join(STAGED_FILE).exists(),
                "the original directory's staged file was removed"
            );
            assert!(
                moved.is_dir(),
                "the original empty directory is left under its new name"
            );
            assert!(
                !replacement.join(STAGED_FILE).exists(),
                "the directory that took the name received nothing"
            );
            assert!(
                replacement.is_dir(),
                "the empty replacement directory is not removed by its name"
            );
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    /// A new target's bytes are reachable only through its private staging
    /// directory, a replaced target's staged file is private, and a cancelled
    /// call removes what preparation added.
    #[cfg(unix)]
    #[tokio::test]
    async fn preparation_stays_private_until_installation() {
        use std::os::unix::fs::PermissionsExt;
        for replaced in [false, true] {
            let root = std::env::temp_dir().join(format!("dim-private-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir(&root).unwrap();
            let root = root.canonicalize().unwrap();
            let target = root.join("file");
            let mut tools = FileTools::new(root.clone(), root.clone(), root.join("out"));
            let reads = branch();
            if replaced {
                std::fs::write(&target, b"before\r\n").unwrap();
                std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600)).unwrap();
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
            let observed: Arc<Mutex<Option<Preparation>>> = Arc::default();
            let seen = observed.clone();
            tools.before_commit = Some(Arc::new(move |path| {
                let staged = staged_entries(path);
                assert_eq!(staged.len(), 1, "one staged entry for {path:?}");
                let metadata = staged[0].metadata().unwrap();
                let bytes = if metadata.is_dir() {
                    let inner: Vec<_> = std::fs::read_dir(staged[0].path())
                        .unwrap()
                        .map(Result::unwrap)
                        .collect();
                    assert_eq!(inner.len(), 1, "one staged file in {:?}", staged[0].path());
                    std::fs::read(inner[0].path()).unwrap()
                } else {
                    std::fs::read(staged[0].path()).unwrap()
                };
                *seen.lock().unwrap() = Some(Preparation {
                    directory: metadata.is_dir(),
                    mode: metadata.permissions().mode() & 0o777,
                    bytes,
                    target_exists: path.exists(),
                });
                cancellation.cancel();
            }));
            let result = call(
                &tools,
                &reads,
                "write",
                json!({"path": "file", "content": "after\n"}),
                cancel,
            )
            .await;
            assert!(matches!(result, Err(HostError::Cancelled(_))));
            let preparation = observed.lock().unwrap().take().unwrap();
            let written: &[u8] = if replaced { b"after\r\n" } else { b"after\n" };
            assert_eq!(
                preparation.bytes, written,
                "preparation holds the bytes the call is writing, in the target's line endings"
            );
            if replaced {
                assert!(!preparation.directory, "a replaced target stages a file");
                assert_eq!(
                    preparation.mode, 0o600,
                    "a replaced 0600 target's staged file is no wider than the target"
                );
                assert!(preparation.target_exists);
            } else {
                assert!(
                    preparation.directory,
                    "a new target stages a private directory"
                );
                assert_eq!(
                    preparation.mode, 0o700,
                    "the staging directory denies other users"
                );
                assert!(
                    !preparation.target_exists,
                    "the target does not exist during preparation"
                );
            }
            assert!(
                staged_entries(&target).is_empty(),
                "cancellation removed the staged entry"
            );
            assert_eq!(
                std::fs::read(&target).ok(),
                replaced.then(|| b"before\r\n".to_vec()),
                "cancellation left the target as it was"
            );
            std::fs::remove_dir_all(root).unwrap();
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

/// macOS keeps inheritable access control entries beside the mode bits. A new
/// target's installed file must carry the ACL its parent gives a new file,
/// while the private staging directory and the staged file carry none of what
/// they would inherit from that parent. These need the in-crate commit hook.
#[cfg(all(test, target_os = "macos"))]
mod acl_tests {
    use super::*;
    use dim_agent_runtime::{MemoryTimelineStore, Timeline, TimelineKey};
    use rutis::host::CtxHost;
    use std::os::fd::AsRawFd;

    /// One branch with its own empty Timeline.
    fn branch() -> FileReads {
        FileReads::new(Arc::new(Timeline::new(
            rutis::Ctx::root().unwrap(),
            TimelineKey {
                session_id: "acl".into(),
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
                    id: "acl".into(),
                    name: name.into(),
                    input: input.as_object().unwrap().clone(),
                },
                reads,
                cancel,
            )
            .await
    }

    /// The entries preparation added to the target's parent directory.
    fn staged_entries(path: &Path) -> Vec<std::fs::DirEntry> {
        std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(Result::unwrap)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".dim-write-")
            })
            .collect()
    }

    /// `ACL_TYPE_EXTENDED` from `<sys/acl.h>`.
    const ACL_TYPE_EXTENDED: libc::c_int = 0x100;

    unsafe extern "C" {
        fn acl_get_fd_np(fd: libc::c_int, kind: libc::c_int) -> *mut libc::c_void;
        fn acl_to_text(acl: *mut libc::c_void, len: *mut libc::ssize_t) -> *mut libc::c_char;
        fn acl_free(object: *mut libc::c_void) -> libc::c_int;
    }

    /// The extended ACL `path` carries as libSystem renders it; `None` when it
    /// has no extended ACL.
    fn acl_text(path: &Path) -> Option<String> {
        let file = std::fs::File::open(path).unwrap();
        // SAFETY: `file` is open for the call and the ACL is freed below.
        let acl = unsafe { acl_get_fd_np(file.as_raw_fd(), ACL_TYPE_EXTENDED) };
        if acl.is_null() {
            let error = std::io::Error::last_os_error();
            assert_eq!(
                error.raw_os_error(),
                Some(libc::ENOENT),
                "{path:?}: {error}"
            );
            return None;
        }
        // SAFETY: the ACL is live and the returned text is freed below.
        let text = unsafe { acl_to_text(acl, std::ptr::null_mut()) };
        assert!(!text.is_null(), "{path:?}");
        // SAFETY: `acl_to_text` returned a NUL-terminated string.
        let rendered = unsafe { std::ffi::CStr::from_ptr(text) }
            .to_string_lossy()
            .into_owned();
        // SAFETY: both pointers came from libSystem and are freed once here.
        unsafe {
            acl_free(text.cast());
            acl_free(acl);
        }
        Some(rendered)
    }

    /// Sets the current user's primary group as `permissions` on `directory`,
    /// after removing any ACL it already carries.
    fn inheritable(directory: &Path, permissions: &str) {
        let output = std::process::Command::new("id")
            .arg("-gn")
            .output()
            .unwrap();
        assert!(output.status.success());
        let group = String::from_utf8(output.stdout).unwrap();
        assert!(
            std::process::Command::new("chmod")
                .arg("-N")
                .arg(directory)
                .status()
                .unwrap()
                .success()
        );
        assert!(
            std::process::Command::new("chmod")
                .arg("+a")
                .arg(format!("group:{} {permissions}", group.trim()))
                .arg(directory)
                .status()
                .unwrap()
                .success(),
            "setting {permissions} on {directory:?}"
        );
    }

    fn workspace(name: &str) -> (PathBuf, FileTools) {
        let root = std::env::temp_dir().join(format!("{name}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let tools = FileTools::new(root.clone(), root.clone(), root.join("out"));
        (root, tools)
    }

    fn mode(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    /// Whether `file` carries no extended ACL entry.
    fn no_acl(file: &std::fs::File) -> bool {
        match acl::Acl::of(file).unwrap() {
            Some(acl) => !acl.entries().unwrap(),
            None => true,
        }
    }

    /// The private staging directory and its file carry no ACL entry, and the
    /// installed file carries the ACL and mode a file created directly in the
    /// same parent would, for each kind of inheritable parent entry.
    #[tokio::test]
    async fn a_new_file_takes_the_parents_acl_on_macos() {
        for permissions in [
            "allow list,search,add_file,delete_child,file_inherit,directory_inherit",
            "allow read,write,execute,file_inherit",
            "allow list,search,add_file,delete_child,directory_inherit",
        ] {
            let (root, mut tools) = workspace("dim-acl");
            inheritable(&root, permissions);
            let reads = branch();
            let observed = Arc::new(Mutex::new(0usize));
            let seen = observed.clone();
            tools.before_commit = Some(Arc::new(move |path| {
                let staged = staged_entries(path);
                assert_eq!(staged.len(), 1, "one staging entry: {staged:?}");
                assert!(staged[0].metadata().unwrap().is_dir());
                let directory = staged[0].path();
                assert_eq!(
                    acl_text(&directory),
                    None,
                    "the private directory carries no ACL: {permissions}"
                );
                assert_eq!(
                    acl_text(&directory.join(STAGED_FILE)),
                    None,
                    "the staged file carries no ACL: {permissions}"
                );
                *seen.lock().unwrap() += 1;
            }));
            let result = call(
                &tools,
                &reads,
                "write",
                json!({"path": "file", "content": "text\n"}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
            assert_eq!(result.is_error, None, "{permissions}: {result:?}");
            assert_eq!(*observed.lock().unwrap(), 1, "{permissions}");
            let control = root.join("control");
            std::fs::write(&control, b"").unwrap();
            let target = root.join("file");
            assert_eq!(
                acl_text(&target),
                acl_text(&control),
                "the installed file takes the parent's ACL: {permissions}"
            );
            assert_eq!(
                mode(&target),
                mode(&control),
                "the installed file takes the parent's mode: {permissions}"
            );
            assert!(
                staged_entries(&target).is_empty(),
                "a successful write leaves no staging entry: {permissions}"
            );
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    /// A parent ACL added after the probe is not part of the snapshot used to
    /// derive the installed file's ACL; the opened private directory and the
    /// staged file are still checked and cleared before writing.
    #[test]
    fn a_parent_acl_added_after_the_probe_does_not_reach_the_staged_file() {
        let root = std::env::temp_dir().join(format!("dim-acl-probe-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let parent = std::fs::File::open(&root).unwrap();
        let name = std::ffi::OsStr::new(".dim-write-probe");
        let (file, staging) = create_staged_file_observed(&parent, name, || {
            inheritable(
                &root,
                "allow list,search,add_file,delete_child,file_inherit,directory_inherit",
            );
        })
        .unwrap();
        assert!(no_acl(&staging.dir), "the private directory carries no ACL");
        assert!(no_acl(&file), "the staged file carries no ACL");
        drop(file);
        drop(staging);
        std::fs::remove_dir_all(root).unwrap();
    }

    /// Cancellation and a failed publication remove the private directory, the
    /// staged file and the ACL template, leaving no `.dim-write-*` entry.
    #[tokio::test]
    async fn a_cancelled_or_failed_new_file_leaves_no_staging_entry_on_macos() {
        for failure in ["cancel", "exists"] {
            let (root, mut tools) = workspace("dim-acl-cleanup");
            inheritable(
                &root,
                "allow list,search,add_file,delete_child,file_inherit,directory_inherit",
            );
            let reads = branch();
            let cancel = CancellationToken::new();
            let cancellation = cancel.clone();
            tools.before_commit = Some(Arc::new(move |path| match failure {
                "cancel" => cancellation.cancel(),
                "exists" => std::fs::write(path, b"external").unwrap(),
                _ => unreachable!(),
            }));
            let result = call(
                &tools,
                &reads,
                "write",
                json!({"path": "file", "content": "text\n"}),
                cancel,
            )
            .await;
            if failure == "cancel" {
                assert!(matches!(result, Err(HostError::Cancelled(_))));
                assert!(!root.join("file").exists());
            } else {
                let result = result.unwrap();
                assert_eq!(result.is_error, Some(true));
                assert_eq!(
                    result.structured_content.unwrap()["code"],
                    "text_binding_required"
                );
                assert_eq!(std::fs::read(root.join("file")).unwrap(), b"external");
            }
            let leftovers: Vec<_> = std::fs::read_dir(&root)
                .unwrap()
                .map(Result::unwrap)
                .filter(|entry| {
                    entry
                        .file_name()
                        .to_string_lossy()
                        .starts_with(".dim-write-")
                })
                .collect();
            assert!(
                leftovers.is_empty(),
                "{failure} leaves no staging entry: {leftovers:?}"
            );
            std::fs::remove_dir_all(root).unwrap();
        }
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

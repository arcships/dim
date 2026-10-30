//! Process-owned byte storage beneath explicitly authorized directory handles.
//!
//! Unix uses `openat(O_NOFOLLOW)`; Windows uses handle-relative `NtCreateFile`
//! and rejects every reparse point. Reads, locks and replacement stay relative to
//! held directories. Roots must already exist. Business owners choose authorization,
//! formats and permissions. Other hosts are unsupported; no pathname fallback.
//!
//! Each file is locked through a key file with its basename in a reserved
//! directory beside it, so keys follow the filesystem's own case and Unicode
//! name equivalence. Releasing a lock keeps its key. Only a directory removal
//! unlinks keys, one at a time while holding each, and a lock counts only if
//! its key is still linked once held; otherwise the current key is resolved
//! again. `auth.json` also uses the existing TypeScript-compatible exclusive
//! marker protocol at `auth.json.lock`; other files use the reserved hidden
//! prefix below. A replacement writes a temporary file named after its key in
//! that physically protected namespace, so only the key's holder touches it.
//! Listing hides those resources; removing a directory removes them with it.
use rutis::host::ErrorKind;
use rutis::{Ctx, Plugin};
use std::{
    collections::HashMap,
    ffi::{OsStr, OsString},
    fs::{File, Permissions, TryLockError},
    io::{self, Read, Write},
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex, Weak},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const RESOURCE_PREFIX: &str = ".dim-file-storage-";
const LOCK_DIRECTORY: &str = ".dim-file-storage-locks";
const TEMPORARY_DIRECTORY: &str = ".dim-file-storage-temps";

pub type Result<T> = std::result::Result<T, FileStorageError>;

#[derive(Debug, thiserror::Error)]
pub enum FileStorageError {
    #[error("invalid storage path: {0}")]
    InvalidPath(&'static str),
    #[error("file storage is closed")]
    Closed,
    #[error("file operation cancelled before commit")]
    Cancelled,
    #[error("file is locked by another lease")]
    Locked,
    #[error("file storage requires a Unix or Windows host")]
    Unsupported,
    #[error("file I/O: {0}")]
    Io(#[from] io::Error),
    #[error("file worker: {0}")]
    Worker(#[from] tokio::task::JoinError),
    #[error("{operation}; temporary file cleanup: {cleanup}")]
    Cleanup {
        operation: Box<FileStorageError>,
        #[source]
        cleanup: io::Error,
    },
    #[error("file lock cleanup: {0:?}")]
    Release(Vec<io::Error>),
    /// The replacement, directory or removal is in place and visible, but
    /// syncing it failed, so it may not survive a crash. It is not an
    /// unchanged entry.
    #[error("file or directory changed, but its durability could not be confirmed: {0}")]
    Unsynced(#[source] io::Error),
}

/// A relative path bound to one authorized root and one service generation.
#[derive(Clone, Debug)]
pub struct FilePath {
    generation: Uuid,
    root: usize,
    relative: PathBuf,
}

/// One entry of a directory listing: a single component as the filesystem
/// stores it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirectoryEntry {
    pub name: OsString,
    pub kind: EntryKind,
}

/// What a directory entry is, without following links.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum EntryKind {
    File,
    Directory,
    /// A symbolic link, reparse point or special file. Storage operations never
    /// follow it.
    Other,
}

/// Names reserved for this module's lock, temporary and account-marker resources.
fn reserved_name(name: &OsStr) -> bool {
    name.to_string_lossy().starts_with(RESOURCE_PREFIX) || name == "auth.json.lock"
}

struct State {
    roots: Vec<File>,
    active: usize,
    leases: HashMap<Uuid, Weak<Lease>>,
    cleanup_errors: Vec<io::Error>,
}

pub struct FileStorage {
    generation: Uuid,
    roots: Arc<Mutex<Vec<PathBuf>>>,
    state: Mutex<State>,
    stopping: CancellationToken,
    drained: Notify,
}

struct Operation(Arc<FileStorage>);
impl Drop for Operation {
    fn drop(&mut self) {
        self.0.state.lock().unwrap().active -= 1;
        self.0.drained.notify_waiters();
    }
}

struct LeaseResources {
    _lock: PhysicalLock,
    parent: File,
    name: OsString,
}
struct Lease {
    id: Uuid,
    owner: Weak<FileStorage>,
    path: FilePath,
    resources: Mutex<Option<LeaseResources>>,
}
impl Drop for Lease {
    fn drop(&mut self) {
        if let Some(owner) = self.owner.upgrade() {
            owner.state.lock().unwrap().leases.remove(&self.id);
        }
    }
}

/// Owns one physical file lock across a business read-modify-write operation.
/// Mutable methods prevent concurrent operations through the same lease.
pub struct FileLease(Option<Arc<Lease>>);
impl Drop for FileLease {
    fn drop(&mut self) {
        let Some(lease) = self.0.take() else { return };
        let cleanup = lease.owner.upgrade().map(|owner| {
            owner.state.lock().unwrap().active += 1;
            Operation(owner)
        });
        drop(lease);
        drop(cleanup);
    }
}

impl FileStorage {
    fn open(roots: Arc<Mutex<Vec<PathBuf>>>) -> Result<Arc<Self>> {
        if cfg!(not(any(unix, windows))) {
            return Err(FileStorageError::Unsupported);
        }
        let handles = roots
            .lock()
            .unwrap()
            .iter()
            .map(|root| native::open_root(root))
            .collect::<Result<Vec<_>>>()?;
        Ok(Arc::new(Self {
            generation: Uuid::new_v4(),
            roots,
            state: Mutex::new(State {
                roots: handles,
                active: 0,
                leases: HashMap::new(),
                cleanup_errors: Vec::new(),
            }),
            stopping: CancellationToken::new(),
            drained: Notify::new(),
        }))
    }

    /// The trusted host supplies a canonical absolute directory. Root indices
    /// are append-only and the Plugin retains their configuration across reloads.
    pub async fn register_root(
        self: &Arc<Self>,
        root: PathBuf,
        cancel: &CancellationToken,
    ) -> Result<usize> {
        let operation = {
            let mut state = self.state.lock().unwrap();
            self.check(cancel)?;
            if let Some(index) = self
                .roots
                .lock()
                .unwrap()
                .iter()
                .position(|path| path == &root)
            {
                return Ok(index);
            }
            state.active += 1;
            Operation(self.clone())
        };
        let owner = self.clone();
        let cancel = cancel.clone();
        tokio::task::spawn_blocking(move || {
            let _operation = operation;
            owner.check(&cancel)?;
            let handle = native::open_root(&root)?;
            let mut state = owner.state.lock().unwrap();
            owner.check(&cancel)?;
            let mut roots = owner.roots.lock().unwrap();
            if let Some(index) = roots.iter().position(|path| path == &root) {
                return Ok(index);
            }
            let index = roots.len();
            state.roots.push(handle);
            roots.push(root);
            Ok(index)
        })
        .await?
    }

    pub fn root_path(&self, root: usize) -> Result<PathBuf> {
        if self.stopping.is_cancelled() {
            return Err(FileStorageError::Closed);
        }
        self.roots
            .lock()
            .unwrap()
            .get(root)
            .cloned()
            .ok_or(FileStorageError::InvalidPath("unknown authorized root"))
    }

    pub fn path(&self, root: usize, relative: impl AsRef<Path>) -> Result<FilePath> {
        {
            let state = self.state.lock().unwrap();
            if self.stopping.is_cancelled() {
                return Err(FileStorageError::Closed);
            }
            if root >= state.roots.len() {
                return Err(FileStorageError::InvalidPath("unknown authorized root"));
            }
        }
        let mut path = PathBuf::new();
        for component in relative.as_ref().components() {
            let Component::Normal(name) = component else {
                return Err(FileStorageError::InvalidPath(
                    "expected a relative file path",
                ));
            };
            #[cfg(windows)]
            native::validate_component(name)?;
            if reserved_name(name) {
                return Err(FileStorageError::InvalidPath(
                    "reserved storage resource name",
                ));
            }
            path.push(name);
        }
        if path.as_os_str().is_empty() {
            return Err(FileStorageError::InvalidPath("file name is required"));
        }
        Ok(FilePath {
            generation: self.generation,
            root,
            relative: path,
        })
    }

    fn check(&self, cancel: &CancellationToken) -> Result<()> {
        if self.stopping.is_cancelled() {
            Err(FileStorageError::Closed)
        } else if cancel.is_cancelled() {
            Err(FileStorageError::Cancelled)
        } else {
            Ok(())
        }
    }

    async fn run<T: Send + 'static>(
        self: &Arc<Self>,
        path: &FilePath,
        cancel: &CancellationToken,
        action: impl FnOnce(File, Arc<Self>, CancellationToken) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let (root, operation) = {
            let mut state = self.state.lock().unwrap();
            self.check(cancel)?;
            if path.generation != self.generation {
                return Err(FileStorageError::InvalidPath(
                    "path belongs to another generation",
                ));
            }
            let root = state.roots[path.root].try_clone()?;
            state.active += 1;
            (root, Operation(self.clone()))
        };
        let owner = self.clone();
        let cancel = cancel.clone();
        tokio::task::spawn_blocking(move || {
            let _operation = operation;
            (move || {
                owner.check(&cancel)?;
                action(root, owner, cancel)
            })()
        })
        .await?
    }

    pub async fn read(
        self: &Arc<Self>,
        path: &FilePath,
        cancel: &CancellationToken,
    ) -> Result<Vec<u8>> {
        self.read_limited(path, usize::MAX, cancel).await
    }

    /// Rejects larger files before allocation and enforces the limit while reading.
    /// Exceeding the byte limit returns Io(InvalidData), without partial bytes.
    pub async fn read_limited(
        self: &Arc<Self>,
        path: &FilePath,
        limit: usize,
        cancel: &CancellationToken,
    ) -> Result<Vec<u8>> {
        let relative = path.relative.clone();
        self.run(path, cancel, move |root, owner, cancel| {
            let (parent, name) = native::parent(root, &relative)?;
            read(&parent, &name, &owner, &cancel, limit)
        })
        .await
    }

    /// Attempts admission immediately; retry policy belongs to the consumer.
    pub async fn lock(
        self: &Arc<Self>,
        path: &FilePath,
        cancel: &CancellationToken,
    ) -> Result<FileLease> {
        let path = path.clone();
        self.run(&path.clone(), cancel, move |root, owner, cancel| {
            let (parent, name) = native::parent(root, &path.relative)?;
            let (name, lock) = physical_lock(&parent, &name, &owner)?;
            let lease = Arc::new(Lease {
                id: Uuid::new_v4(),
                owner: Arc::downgrade(&owner),
                path,
                resources: Mutex::new(Some(LeaseResources {
                    _lock: lock,
                    parent,
                    name,
                })),
            });
            {
                let mut state = owner.state.lock().unwrap();
                owner.check(&cancel)?;
                state.leases.insert(lease.id, Arc::downgrade(&lease));
            }
            Ok(FileLease(Some(lease)))
        })
        .await
    }

    /// Replaces bytes under the same lock used by explicit read-modify-write leases.
    pub async fn replace(
        self: &Arc<Self>,
        path: &FilePath,
        bytes: Vec<u8>,
        permissions: Option<Permissions>,
        cancel: &CancellationToken,
    ) -> Result<()> {
        self.lock(path, cancel)
            .await?
            .replace(bytes, permissions, cancel)
            .await
    }

    /// Makes `path` an existing business directory: a missing one is created
    /// with mode 0700 (Windows inherits the parent's DACL), an existing one
    /// counts as created, and any other existing entry fails `Io` untouched.
    /// The parent must exist; there is no recursive creation. Aliases of the
    /// reserved directory or the account marker fail `InvalidPath` before
    /// anything is created. The parent is synced before returning, in both
    /// cases, so a retry after `Unsynced` confirms durability. Cancellation is
    /// checked up to the creation.
    pub async fn create_directory(
        self: &Arc<Self>,
        path: &FilePath,
        cancel: &CancellationToken,
    ) -> Result<()> {
        let relative = path.relative.clone();
        self.run(path, cancel, move |root, owner, cancel| {
            let (parent, name) = native::parent(root, &relative)?;
            let (locks, _temporary) = native::directories(&parent)?;
            if native::linked(&parent, &name, &locks)? {
                return Err(FileStorageError::InvalidPath("reserved storage directory"));
            }
            let marker = native::create_key(&locks, OsStr::new("auth.json.lock"))?;
            match native::open_key(&locks, &name) {
                Ok(key) if native::same_identity(&key, &marker)? => {
                    return Err(FileStorageError::InvalidPath("reserved storage lock"));
                }
                Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error.into()),
                _ => {}
            }
            owner.check(&cancel)?;
            native::create_directory(&parent, &name)?;
            native::open_directory(&parent, &name)?;
            native::sync_directory(&parent).map_err(FileStorageError::Unsynced)
        })
        .await
    }

    /// Lists a directory below an authorized root, sorted by name. This
    /// module's own lock, temporary and account-marker entries are omitted,
    /// including filesystem aliases of them. Links are listed, never followed.
    pub async fn list(
        self: &Arc<Self>,
        path: &FilePath,
        cancel: &CancellationToken,
    ) -> Result<Vec<DirectoryEntry>> {
        let relative = path.relative.clone();
        self.run(path, cancel, move |root, _, _| {
            let (parent, name) = native::parent(root, &relative)?;
            let mut entries = business_entries(&native::open_directory(&parent, &name)?)?;
            entries.sort_by(|a, b| a.name.cmp(&b.name));
            Ok(entries)
        })
        .await
    }

    /// Removes the entry at `path` and, for a directory, everything beneath
    /// it, including this module's resources there. Files are removed under
    /// the lock leases use, so removing a leased file, or a directory with a
    /// lease held beneath it, fails with `Locked`; a link is removed itself.
    /// An absent entry counts as removed, so retrying is safe. Removal is not
    /// atomic: an error or cancellation can leave part of a tree removed. The
    /// directory's owner orders removal after its writers; entries or leases
    /// created beneath it meanwhile make removal fail. Once the entry is
    /// absent its parent is synced, also when it already was, so a retry
    /// after `Unsynced` confirms durability.
    pub async fn remove(
        self: &Arc<Self>,
        path: &FilePath,
        cancel: &CancellationToken,
    ) -> Result<()> {
        let relative = path.relative.clone();
        self.run(path, cancel, move |root, owner, cancel| {
            let (parent, name) = match native::parent(root, &relative) {
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
                result => result?,
            };
            match native::entry_kind(&parent, &name)? {
                Some(EntryKind::Directory) => {
                    remove_tree(&parent, &name, Tree::Business, &owner, &cancel)?
                }
                Some(kind) => {
                    absent_is_removed(remove_entry(&parent, &name, kind, &owner, &cancel))?
                }
                None => {}
            }
            native::sync_directory(&parent).map_err(FileStorageError::Unsynced)
        })
        .await
    }

    async fn close(&self) -> Result<()> {
        self.stopping.cancel();
        loop {
            let changed = self.drained.notified();
            let leases = {
                let mut state = self.state.lock().unwrap();
                if state.active == 0 {
                    state.roots.clear();
                    Some(
                        state
                            .leases
                            .drain()
                            .filter_map(|(_, lease)| lease.upgrade())
                            .collect::<Vec<_>>(),
                    )
                } else {
                    None
                }
            };
            if let Some(leases) = leases {
                for lease in leases {
                    lease.resources.lock().unwrap().take();
                }
                let errors = std::mem::take(&mut self.state.lock().unwrap().cleanup_errors);
                return if errors.is_empty() {
                    Ok(())
                } else {
                    Err(FileStorageError::Release(errors))
                };
            }
            changed.await;
        }
    }
}

impl FileLease {
    async fn run<T: Send + 'static>(
        &mut self,
        cancel: &CancellationToken,
        action: impl FnOnce(File, OsString, Arc<FileStorage>, CancellationToken) -> Result<T>
        + Send
        + 'static,
    ) -> Result<T> {
        let lease = self.0.as_ref().expect("live file lease").clone();
        let owner = lease.owner.upgrade().ok_or(FileStorageError::Closed)?;
        owner
            .run(&lease.path.clone(), cancel, move |_, owner, cancel| {
                let resources = lease.resources.lock().unwrap();
                let resource = resources.as_ref().ok_or(FileStorageError::Closed)?;
                let parent = resource.parent.try_clone()?;
                let name = resource.name.clone();
                owner.check(&cancel)?;
                let result = action(parent, name, owner, cancel);
                drop(resources);
                result
            })
            .await
    }

    pub async fn read(&mut self, cancel: &CancellationToken) -> Result<Vec<u8>> {
        self.run(cancel, |parent, name, owner, cancel| {
            read(&parent, &name, &owner, &cancel, usize::MAX)
        })
        .await
    }

    /// Creates a durable, exclusive sibling copy while retaining this lease.
    /// An existing or partially written backup is never overwritten or removed.
    pub async fn backup(
        &mut self,
        suffix: &str,
        permissions: Option<Permissions>,
        cancel: &CancellationToken,
    ) -> Result<()> {
        if suffix.is_empty()
            || !suffix
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return Err(FileStorageError::InvalidPath("invalid backup suffix"));
        }
        let suffix = suffix.to_owned();
        self.run(cancel, move |parent, name, owner, cancel| {
            let bytes = read(&parent, &name, &owner, &cancel, usize::MAX)?;
            let mut backup = name.clone();
            backup.push(format!(".{suffix}"));
            let mut file =
                native::create_temp(&parent, &backup, &parent, &name, permissions.as_ref())?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            native::sync_replacement(&file, &parent, &parent)?;
            Ok(())
        })
        .await
    }

    /// Permissions are supplied by the business owner; absent permissions preserve
    /// an existing file's mode/readonly attribute. New files use Unix umask or the
    /// Windows business parent DACL; existing Windows DACLs are preserved.
    /// Cancellation is checked through preparation. Once rename starts, the
    /// rename and host-supported flush settle before storage can be unloaded.
    pub async fn replace(
        &mut self,
        bytes: Vec<u8>,
        permissions: Option<Permissions>,
        cancel: &CancellationToken,
    ) -> Result<()> {
        self.run(cancel, move |parent, name, owner, cancel| {
            let permissions = permissions.or(native::permissions(&parent, &name)?);
            let (temporary_directory, mut temp) =
                create_temporary(&parent, &name, permissions.as_ref())?;
            let result = (|| {
                for chunk in bytes.chunks(64 * 1024) {
                    owner.check(&cancel)?;
                    temp.write_all(chunk)?;
                }
                if let Some(permissions) = permissions {
                    temp.set_permissions(permissions)?;
                }
                temp.sync_all()?;
                owner.check(&cancel)?;
                native::replace(&temp, &temporary_directory, &name, &parent, &name)?;
                native::sync_replacement(&temp, &temporary_directory, &parent)
                    .map_err(FileStorageError::Unsynced)
            })();
            if result
                .as_ref()
                .is_err_and(|error| !matches!(error, FileStorageError::Unsynced(_)))
                && let Err(cleanup) = native::remove(&temporary_directory, &name)
                && cleanup.kind() != io::ErrorKind::NotFound
            {
                return Err(FileStorageError::Cleanup {
                    operation: Box::new(result.unwrap_err()),
                    cleanup,
                });
            }
            result
        })
        .await
    }
}

fn read(
    parent: &File,
    name: &OsString,
    owner: &FileStorage,
    cancel: &CancellationToken,
    limit: usize,
) -> Result<Vec<u8>> {
    let mut file = native::read_user_file(parent, name)?;
    if file.metadata()?.len() > limit as u64 {
        return Err(
            io::Error::new(io::ErrorKind::InvalidData, "file exceeds read byte limit").into(),
        );
    }
    let mut bytes = Vec::new();
    let mut chunk = [0; 64 * 1024];
    loop {
        owner.check(cancel)?;
        let count = file.read(&mut chunk)?;
        if count == 0 {
            return Ok(bytes);
        }
        if count > limit.saturating_sub(bytes.len()) {
            return Err(
                io::Error::new(io::ErrorKind::InvalidData, "file exceeds read byte limit").into(),
            );
        }
        bytes.extend_from_slice(&chunk[..count]);
    }
}

/// Locks a regular or missing file under the name its filesystem resolves.
fn physical_lock(
    parent: &File,
    name: &OsString,
    owner: &Arc<FileStorage>,
) -> Result<(OsString, PhysicalLock)> {
    let name = native::target_name(parent, name)?;
    native::permissions(parent, &name)?;
    for _ in 0..2 {
        if let Some(lock) = lock_name(parent, &name, owner)? {
            return Ok((name, lock));
        }
    }
    Err(FileStorageError::Locked)
}

/// Locks the key for `name` in `parent`. None if a concurrent directory
/// removal unlinked the key before it was held.
fn lock_name(
    parent: &File,
    name: &OsStr,
    storage: &Arc<FileStorage>,
) -> Result<Option<PhysicalLock>> {
    let (locks, _temporary) = native::directories(parent)?;
    if native::linked(parent, name, &locks)? {
        return Err(FileStorageError::InvalidPath("reserved storage directory"));
    }
    let Some(alias_lock) = lock_key(&locks, name, native::create_key)? else {
        return Ok(None);
    };
    let marker_key = native::create_key(&locks, OsStr::new("auth.json.lock"))?;
    if native::same_identity(alias_lock.file(), &marker_key)? {
        return Err(FileStorageError::InvalidPath("reserved storage lock"));
    }
    let auth_key = native::create_key(&locks, OsStr::new("auth.json"))?;
    if native::same_identity(alias_lock.file(), &auth_key)? {
        return account_lock(parent, alias_lock, storage).map(Some);
    }
    Ok(Some(PhysicalLock {
        file: alias_lock.into_file(),
        _alias_lock: None,
        account_parent: None,
        owner: None,
        storage: Arc::downgrade(storage),
    }))
}

/// Opens the key `name` in `locks` with `open` and locks it. None if a
/// removal took the key, or the reserved directory, before it was held: only
/// a removal holding a key unlinks it, so a key still linked once held stays
/// linked while held. Such a key is stale whatever error locking it gives,
/// since Windows may refuse to lock a deleted file.
fn lock_key(
    locks: &File,
    name: &OsStr,
    open: fn(&File, &OsStr) -> io::Result<File>,
) -> Result<Option<HeldKey>> {
    let key = match open(locks, name) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        result => result?,
    };
    #[cfg(test)]
    before_key_lock(name);
    match key.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => return Err(FileStorageError::Locked),
        Err(TryLockError::Error(_)) if !native::linked(locks, name, &key)? => return Ok(None),
        Err(TryLockError::Error(error)) => return Err(error.into()),
    }
    let key = HeldKey(Some(key));
    Ok(native::linked(locks, name, key.file())?.then_some(key))
}

/// Creates the temporary file for replacing `name`, whose key the caller
/// holds. The file takes the key's name, so a leftover of that name is from
/// an interrupted replacement and is discarded. A concurrent removal of the
/// directory can delete the empty temporary directory first; it is then
/// created again once.
fn create_temporary(
    parent: &File,
    name: &OsString,
    permissions: Option<&Permissions>,
) -> Result<(File, File)> {
    for attempt in 0..2 {
        let (locks, temporary) = native::directories(parent)?;
        absent_is_removed(native::remove(&temporary, name))?;
        match native::create_temp(&temporary, name, parent, name, permissions) {
            Ok(file) => return Ok((temporary, file)),
            Err(error)
                if attempt == 1
                    || native::linked(&locks, OsStr::new(TEMPORARY_DIRECTORY), &temporary)? =>
            {
                return Err(error.into());
            }
            Err(_) => {}
        }
    }
    unreachable!("the second attempt returns")
}

/// Entries a business owner may address: this module's resources are
/// omitted by name and by the identity the filesystem gives them.
fn business_entries(directory: &File) -> Result<Vec<DirectoryEntry>> {
    let entries = remaining_entries(directory)?;
    let reserved = native::reserved(directory)?;
    Ok(entries
        .into_iter()
        .filter(|(entry, identity)| {
            !reserved_name(&entry.name) && !reserved.iter().flatten().any(|id| id == identity)
        })
        .map(|(entry, _)| entry)
        .collect())
}

/// How removal treats the entries of a directory tree.
#[derive(Clone, Copy)]
enum Tree<'a> {
    /// Below an authorized root: files go under the lock leases use, and
    /// each directory's FileStorage resources go last by their protocols.
    Business,
    /// Inside the lock directory `locks`, which no `FilePath` reaches, so no
    /// lease holds anything there but the keys: every entry is unlinked as
    /// it is. Its temporary directory is never a leftover tree.
    Leftover { locks: &'a File },
}

/// Entries removal empties from `directory` before its FileStorage
/// resources. In a business tree the lock directory and a regular account
/// marker have their own protocols and are left out, recognized by name or,
/// for aliases, by identity. Leftovers go first: entries with a reserved
/// name, or with the identity of one of those resources but another type. A
/// marker that is a directory or a link would otherwise block locking
/// `auth.json` beside it. Leftovers follow the business rules: on a
/// case-insensitive volume, an alias of a reserved name can be leased.
fn removal_entries(directory: &File, tree: Tree) -> Result<Vec<DirectoryEntry>> {
    let entries = remaining_entries(directory)?;
    if let Tree::Leftover { .. } = tree {
        return Ok(entries.into_iter().map(|(entry, _)| entry).collect());
    }
    let [locks, marker] = native::reserved(directory)?;
    let resources = [
        (LOCK_DIRECTORY, locks, EntryKind::Directory),
        ("auth.json.lock", marker, EntryKind::File),
    ];
    let mut entries: Vec<_> = entries
        .into_iter()
        .filter_map(|(entry, identity)| {
            let names = |(name, id, _): &(&str, Option<native::Identity>, EntryKind)| {
                entry.name == *name || id.as_ref() == Some(&identity)
            };
            if resources
                .iter()
                .any(|resource| names(resource) && entry.kind == resource.2)
            {
                return None;
            }
            let leftover = reserved_name(&entry.name) || resources.iter().any(names);
            Some((leftover, entry))
        })
        .collect();
    entries.sort_by_key(|(leftover, _)| !leftover);
    Ok(entries.into_iter().map(|(_, entry)| entry).collect())
}

/// Clears leftovers that would stop every file in `directory` from being
/// locked, before removal locks its files: a non-directory in place of the
/// lock directory or of the temporary directory inside it, and in the lock
/// directory any link, or directory other than the temporary one, such as one
/// in place of a key. Keys are regular files and the lock directory is not a
/// business directory, so no lease holds these, except a regular file in
/// place of the temporary directory, which can be a key and goes under its
/// lock.
fn clear_lock_leftovers(
    directory: &File,
    owner: &Arc<FileStorage>,
    cancel: &CancellationToken,
) -> Result<()> {
    let locks_name = OsStr::new(LOCK_DIRECTORY);
    match native::entry_kind(directory, locks_name)? {
        Some(EntryKind::Directory) => {}
        Some(_) => return absent_is_removed(native::remove(directory, locks_name)),
        None => return Ok(()),
    }
    let Some(locks) = native::reserved_directory(directory, locks_name)? else {
        return Ok(());
    };
    let temporary_name = OsStr::new(TEMPORARY_DIRECTORY);
    match native::entry_kind(&locks, temporary_name)? {
        Some(EntryKind::File) => remove_key(
            &locks,
            None,
            temporary_name,
            native::open_key,
            owner,
            cancel,
        )?,
        Some(EntryKind::Other) => {
            absent_is_removed(native::remove_other(&locks, temporary_name))?;
        }
        Some(EntryKind::Directory) | None => {}
    }
    for (entry, _) in remaining_entries(&locks)? {
        owner.check(cancel)?;
        match entry.kind {
            EntryKind::File => {}
            EntryKind::Other => absent_is_removed(native::remove_other(&locks, &entry.name))?,
            EntryKind::Directory => remove_tree(
                &locks,
                &entry.name,
                Tree::Leftover { locks: &locks },
                owner,
                cancel,
            )?,
        }
    }
    Ok(())
}

/// Removes a non-directory entry below `parent`: a file under the lock leases
/// use, a link or special file itself, never followed, and only while it is
/// still one. A file whose key the filesystem equates with the temporary
/// directory, as a leftover of that name or a case alias of it, cannot be
/// locked, so no lease holds it; it is unlinked without a lock, but never
/// when it is the account marker under another name.
fn remove_entry(
    parent: &File,
    name: &OsString,
    kind: EntryKind,
    owner: &Arc<FileStorage>,
    cancel: &CancellationToken,
) -> Result<()> {
    if kind != EntryKind::File {
        return Ok(native::remove_other(parent, name)?);
    }
    match physical_lock(parent, name, owner) {
        Ok((name, _lock)) => {
            owner.check(cancel)?;
            Ok(native::remove(parent, &name)?)
        }
        Err(error)
            if !matches!(
                native::reserved_directory(parent, OsStr::new(LOCK_DIRECTORY)),
                Ok(Some(locks)) if key_is_temporary_directory(&locks, name)
            ) =>
        {
            Err(error)
        }
        Err(_) => {
            native::permissions(parent, name)?;
            owner.check(cancel)?;
            Ok(native::remove(parent, name)?)
        }
    }
}

/// Whether the filesystem resolves the key `name` in `locks` to the
/// temporary directory, as it can an alias of that directory's name. Such a
/// key cannot be locked, so no lease holds its name.
fn key_is_temporary_directory(locks: &File, name: &OsStr) -> bool {
    matches!(
        native::reserved_directory(locks, OsStr::new(TEMPORARY_DIRECTORY)),
        Ok(Some(temporary)) if matches!(native::linked(locks, name, &temporary), Ok(true))
    )
}

/// Removes the directory `name` below `parent` and everything beneath it,
/// depth first through directory handles and never on another filesystem.
/// An explicit stack replaces recursion, so tree depth costs no thread
/// stack; `native::Ancestors` bounds what returning to each parent holds. A
/// business tree removes leftover trees in its lock directory, which never
/// nest further.
fn remove_tree(
    parent: &File,
    name: &OsString,
    tree: Tree,
    owner: &Arc<FileStorage>,
    cancel: &CancellationToken,
) -> Result<()> {
    /// A directory being emptied: its name in its parent and the entries left.
    struct Level {
        name: OsString,
        entries: std::vec::IntoIter<DirectoryEntry>,
    }
    let entries = |directory: &File| {
        if let Tree::Business = tree {
            clear_lock_leftovers(directory, owner, cancel)?;
        }
        Ok::<_, FileStorageError>(removal_entries(directory, tree)?.into_iter())
    };
    'tree: loop {
        let mut directory = match native::subdirectory(parent, name, tree) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            result => result?,
        };
        if let Tree::Leftover { locks } = tree
            && native::linked(locks, OsStr::new(TEMPORARY_DIRECTORY), &directory)?
        {
            return Ok(());
        }
        let mut levels = vec![Level {
            name: name.clone(),
            entries: entries(&directory)?,
        }];
        let mut ancestors = native::Ancestors::default();
        while let Some(level) = levels.last_mut() {
            owner.check(cancel)?;
            let Some(entry) = level.entries.next() else {
                if let Tree::Business = tree {
                    remove_resources(&directory, owner, cancel)?;
                }
                let name = levels.pop().expect("emptied level").name;
                if levels.is_empty() {
                    return absent_is_removed(native::remove_directory(parent, &name));
                }
                directory = match ancestors.pop(&directory) {
                    Err(error) if error.kind() == io::ErrorKind::NotFound => continue 'tree,
                    result => result?,
                };
                absent_is_removed(native::remove_directory(&directory, &name))?;
                continue;
            };
            if entry.kind != EntryKind::Directory {
                absent_is_removed(match tree {
                    Tree::Business => {
                        remove_entry(&directory, &entry.name, entry.kind, owner, cancel)
                    }
                    Tree::Leftover { .. } => {
                        native::remove(&directory, &entry.name).map_err(FileStorageError::from)
                    }
                })?;
                continue;
            }
            match native::subdirectory(&directory, &entry.name, tree) {
                Ok(child) => {
                    let entries = entries(&child)?;
                    ancestors.push(std::mem::replace(&mut directory, child))?;
                    levels.push(Level {
                        name: entry.name,
                        entries,
                    });
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        unreachable!("emptying the target directory returns")
    }
}

/// Removes this module's resources from a directory whose other entries are
/// gone. A live account marker returns `Locked`. Lock keys go one at a time,
/// each with the temporary entry named after it and only while holding that
/// key, so a key or temporary file a holder has locked is never removed.
fn remove_resources(
    directory: &File,
    owner: &Arc<FileStorage>,
    cancel: &CancellationToken,
) -> Result<()> {
    if native::entry_kind(directory, OsStr::new("auth.json.lock"))?.is_some() {
        let (locks, _temporary) = native::directories(directory)?;
        let Some(_account) = lock_key(&locks, OsStr::new("auth.json"), native::create_key)? else {
            return Err(FileStorageError::Locked);
        };
        if !reclaim_account(directory)? {
            return Err(FileStorageError::Locked);
        }
    }
    let Some(locks) = native::reserved_directory(directory, OsStr::new(LOCK_DIRECTORY))? else {
        return Ok(());
    };
    let temporary_name = OsStr::new(TEMPORARY_DIRECTORY);
    let temporary = native::reserved_directory(&locks, temporary_name)?;
    for (entry, _) in remaining_entries(&locks)? {
        match entry.kind {
            EntryKind::File => remove_key(
                &locks,
                temporary.as_ref(),
                &entry.name,
                native::open_key,
                owner,
                cancel,
            )?,
            EntryKind::Other => absent_is_removed(native::remove_other(&locks, &entry.name))?,
            EntryKind::Directory => {}
        }
    }
    if let Some(temporary) = &temporary {
        for (entry, _) in remaining_entries(temporary)? {
            remove_key(
                &locks,
                Some(temporary),
                &entry.name,
                native::create_key,
                owner,
                cancel,
            )?;
        }
        absent_is_removed(native::remove_directory(&locks, temporary_name))?;
    }
    absent_is_removed(native::remove_directory(
        directory,
        OsStr::new(LOCK_DIRECTORY),
    ))
}

/// Removes the key `name` and the temporary entry of that name while holding
/// the key; a directory there is a leftover tree. `open_key` skips a key
/// another removal took first; `create_key` takes a key for a temporary entry
/// whose key is gone. A name whose key would be the temporary directory has
/// no key, so no holder writes its temporary entry.
fn remove_key(
    locks: &File,
    temporary: Option<&File>,
    name: &OsStr,
    open: fn(&File, &OsStr) -> io::Result<File>,
    owner: &Arc<FileStorage>,
    cancel: &CancellationToken,
) -> Result<()> {
    let key = match lock_key(locks, name, open) {
        Ok(Some(key)) => Some(key),
        Ok(None) => return Ok(()),
        Err(FileStorageError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(());
        }
        Err(_) if key_is_temporary_directory(locks, name) => None,
        Err(error) => return Err(error),
    };
    if let Some(temporary) = temporary {
        let name = name.to_os_string();
        match native::entry_kind(temporary, &name)? {
            Some(EntryKind::Directory) => {
                remove_tree(temporary, &name, Tree::Leftover { locks }, owner, cancel)?;
            }
            Some(_) => absent_is_removed(native::remove(temporary, &name))?,
            None => {}
        }
    }
    if key.is_none() {
        return Ok(());
    }
    absent_is_removed(native::remove(locks, name))
}

/// Entries of a directory that a concurrent removal may have deleted since
/// it was opened; a deleted directory has none. Removing it by name
/// afterwards reports what is there now.
fn remaining_entries(directory: &File) -> io::Result<Vec<(DirectoryEntry, native::Identity)>> {
    match native::entries(directory) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        result => result,
    }
}

/// Removal ensures absence: an entry or parent that no longer exists is removed.
fn absent_is_removed(result: std::result::Result<(), impl Into<FileStorageError>>) -> Result<()> {
    match result.map_err(Into::into) {
        Err(FileStorageError::Io(error)) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

/// Clones share authorized root identities; each apply opens fresh I/O resources.
#[derive(Clone)]
pub struct FileStoragePlugin {
    roots: Arc<Mutex<Vec<PathBuf>>>,
}
impl FileStoragePlugin {
    pub fn new(roots: Vec<PathBuf>) -> Self {
        Self {
            roots: Arc::new(Mutex::new(roots)),
        }
    }
}
impl Plugin for FileStoragePlugin {
    type Deps = ();
    fn name(&self) -> &str {
        "FileStorage"
    }
    async fn apply(&self, ctx: &Ctx, (): Self::Deps) -> rutis::Result {
        let roots = self.roots.clone();
        let storage = tokio::task::spawn_blocking(move || FileStorage::open(roots))
            .await
            .map_err(|error| rutis::Error::from(ErrorKind::PluginFailed(error.into())))?
            .map_err(|error| rutis::Error::from(ErrorKind::PluginFailed(error.into())))?;
        let owner = storage.clone();
        ctx.effect(move || async move {
            owner
                .close()
                .await
                .map_err(|error| rutis::Error::from(ErrorKind::PluginFailed(error.into())))
        })?;
        ctx.provide_as::<FileStorage>(storage)?;
        Ok(())
    }
}

struct PhysicalLock {
    file: File,
    _alias_lock: Option<File>,
    account_parent: Option<File>,
    owner: Option<Vec<u8>>,
    storage: Weak<FileStorage>,
}
impl Drop for PhysicalLock {
    fn drop(&mut self) {
        if let Some(parent) = &self.account_parent
            && let Err(error) = release_account(parent, &self.file, self.owner.as_deref())
        {
            if let Some(owner) = self.owner.take() {
                ABANDONED_MARKERS.lock().unwrap().push(owner);
            }
            if let Some(storage) = self.storage.upgrade() {
                storage.state.lock().unwrap().cleanup_errors.push(error);
            }
        }
        if let Err(error) = self._alias_lock.as_ref().unwrap_or(&self.file).unlock()
            && let Some(storage) = self.storage.upgrade()
        {
            storage.state.lock().unwrap().cleanup_errors.push(error);
        }
    }
}

/// A business lock key held while its PhysicalLock is built. Every early
/// return unlocks it explicitly, as PhysicalLock's drop does: Windows may
/// defer the implicit unlock after CloseHandle.
struct HeldKey(Option<File>);
impl HeldKey {
    fn file(&self) -> &File {
        self.0.as_ref().expect("held lock key")
    }
    /// Hands the locked file to its PhysicalLock, which unlocks it.
    fn into_file(mut self) -> File {
        self.0.take().expect("held lock key")
    }
}
impl Drop for HeldKey {
    fn drop(&mut self) {
        if let Some(file) = &self.0 {
            let _ = file.unlock();
        }
    }
}

fn account_lock(
    parent: &File,
    alias_lock: HeldKey,
    storage: &Arc<FileStorage>,
) -> Result<PhysicalLock> {
    let account_parent = parent.try_clone()?;
    let name = OsStr::new("auth.json.lock");
    for _ in 0..2 {
        match native::create_account_file(parent, name) {
            Ok(file) => {
                let mut lock = PhysicalLock {
                    file,
                    _alias_lock: Some(alias_lock.into_file()),
                    account_parent: Some(account_parent),
                    owner: None,
                    storage: Arc::downgrade(storage),
                };
                let millis = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_err(io::Error::other)?
                    .as_millis();
                let owner =
                    format!("{}\n{millis}\n{}\n", std::process::id(), Uuid::new_v4()).into_bytes();
                lock.file.write_all(&owner)?;
                lock.file.sync_all()?;
                lock.owner = Some(owner);
                return Ok(lock);
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                if !reclaim_account(parent)? {
                    return Err(FileStorageError::Locked);
                }
            }
            Err(error) => return Err(error.into()),
        }
    }
    Err(FileStorageError::Locked)
}

/// Owner records of account markers this process failed to release. Each
/// carries a fresh nonce, so only that leftover marker matches one.
static ABANDONED_MARKERS: Mutex<Vec<Vec<u8>>> = Mutex::new(Vec::new());

/// Removes a stale account marker; false while it may be live. A marker this
/// process failed to release is stale at once. Any other marker is stale once
/// older than 30 seconds without a matching process alive at its creation.
/// Called holding the `auth.json` key.
fn reclaim_account(parent: &File) -> io::Result<bool> {
    let name = OsString::from("auth.json.lock");
    let mut file = match native::read_account_file(parent, &name) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(true),
        Err(error) => return Err(error),
    };
    let observed = file.metadata()?;
    let mut owner = Vec::new();
    (&mut file).take(256).read_to_end(&mut owner)?;
    let mut fields = owner.split(|byte| *byte == b'\n');
    let pid = fields.next().unwrap_or_default();
    let pid = std::str::from_utf8(pid)
        .ok()
        .filter(|pid| !pid.is_empty() && pid.bytes().all(|byte| byte.is_ascii_digit()))
        .and_then(|pid| pid.parse::<u32>().ok())
        .filter(|pid| *pid > 0);
    let created_at = fields
        .next()
        .and_then(|field| std::str::from_utf8(field).ok())
        .and_then(|field| field.parse::<u64>().ok());
    let abandoned = ABANDONED_MARKERS.lock().unwrap().contains(&owner);
    if !abandoned {
        let age = SystemTime::now()
            .duration_since(observed.modified()?)
            .unwrap_or_default();
        if age <= Duration::from_secs(30) {
            return Ok(false);
        }
        if let (Some(pid), Some(created_at)) = (pid, created_at)
            && native::process_owns_marker(pid, created_at)?
        {
            return Ok(false);
        }
    }
    let mut current = match native::read_account_file(parent, &name) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(true),
        Err(error) => return Err(error),
    };
    if !native::same_identity(&file, &current)?
        || observed.modified()? != current.metadata()?.modified()?
    {
        return Ok(false);
    }
    let mut current_owner = Vec::new();
    (&mut current).take(256).read_to_end(&mut current_owner)?;
    if current_owner != owner {
        return Ok(false);
    }
    if let Err(error) = native::remove_account(parent, &name, &current)
        && error.kind() != io::ErrorKind::NotFound
    {
        return Err(error);
    }
    if abandoned {
        ABANDONED_MARKERS
            .lock()
            .unwrap()
            .retain(|record| *record != owner);
    }
    Ok(true)
}

fn release_account(parent: &File, owned: &File, owner: Option<&[u8]>) -> io::Result<()> {
    let name = OsString::from("auth.json.lock");
    let mut current = match native::read_account_file(parent, &name) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    let observed = owned.metadata()?;
    if !native::same_identity(owned, &current)?
        || observed.modified()? != current.metadata()?.modified()?
    {
        return Ok(());
    }
    if let Some(owner) = owner {
        let mut bytes = Vec::new();
        (&mut current)
            .take(owner.len() as u64 + 1)
            .read_to_end(&mut bytes)?;
        if bytes != owner {
            return Ok(());
        }
    }
    let latest = match native::read_account_file(parent, &name) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if native::same_identity(owned, &latest)?
        && observed.modified()? == latest.metadata()?.modified()?
    {
        native::remove_account(parent, &name, &latest)?;
    }
    Ok(())
}
#[cfg(unix)]
mod native {
    use super::*;
    #[cfg(any(target_os = "illumos", target_os = "solaris"))]
    use libc::___errno as errno_location;
    #[cfg(any(target_os = "android", target_os = "netbsd", target_os = "openbsd"))]
    use libc::__errno as errno_location;
    #[cfg(not(any(
        target_vendor = "apple",
        target_os = "freebsd",
        target_os = "android",
        target_os = "netbsd",
        target_os = "openbsd",
        target_os = "illumos",
        target_os = "solaris",
        target_os = "haiku",
        target_os = "aix",
        target_os = "nto"
    )))]
    use libc::__errno_location as errno_location;
    #[cfg(any(target_vendor = "apple", target_os = "freebsd"))]
    use libc::__error as errno_location;
    #[cfg(target_os = "nto")]
    use libc::__get_errno_ptr as errno_location;
    #[cfg(target_os = "aix")]
    use libc::_Errno as errno_location;
    #[cfg(target_os = "haiku")]
    use libc::_errnop as errno_location;
    use std::{
        ffi::{CStr, CString, OsStr},
        os::{
            fd::{AsRawFd, FromRawFd, IntoRawFd},
            unix::{
                ffi::OsStrExt,
                fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
            },
        },
    };

    /// Device and inode.
    pub type Identity = (u64, u64);

    fn c_name(name: &OsStr) -> io::Result<CString> {
        CString::new(name.as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))
    }

    fn directory(parent: &File, name: &OsStr) -> io::Result<File> {
        let directory_name = c_name(name)?;
        let created = unsafe { libc::mkdirat(parent.as_raw_fd(), directory_name.as_ptr(), 0o700) };
        if created < 0 {
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::AlreadyExists {
                return Err(error);
            }
        } else {
            parent.sync_all()?;
        }
        open(parent, name, libc::O_RDONLY | libc::O_DIRECTORY, 0)
    }
    /// Opens or creates the reserved directory and the temporary directory in it.
    pub fn directories(parent: &File) -> io::Result<(File, File)> {
        let locks = directory(parent, OsStr::new(LOCK_DIRECTORY))?;
        let temporary = directory(&locks, OsStr::new(TEMPORARY_DIRECTORY))?;
        Ok((locks, temporary))
    }
    /// Creates the directory `name` with mode 0700; an existing entry is left
    /// as it is for the caller to check.
    pub fn create_directory(parent: &File, name: &OsStr) -> io::Result<()> {
        let name = c_name(name)?;
        if unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o700) } < 0 {
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::AlreadyExists {
                return Err(error);
            }
        }
        Ok(())
    }
    pub fn sync_directory(directory: &File) -> io::Result<()> {
        directory.sync_all()
    }
    pub fn create_key(parent: &File, name: &OsStr) -> io::Result<File> {
        let mut attempts = 2;
        loop {
            let created = open(
                parent,
                name,
                libc::O_RDWR | libc::O_CREAT | libc::O_EXCL,
                0o600,
            );
            let file = match created {
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    open(parent, name, libc::O_RDWR, 0)
                }
                created => created,
            };
            attempts -= 1;
            match file {
                Err(error) if error.kind() == io::ErrorKind::NotFound && attempts > 0 => {}
                file => return regular(file?),
            }
        }
    }

    fn same_inode(a: &std::fs::Metadata, b: &std::fs::Metadata) -> bool {
        a.dev() == b.dev() && a.ino() == b.ino()
    }
    pub(super) fn same_identity(a: &File, b: &File) -> io::Result<bool> {
        Ok(same_inode(&a.metadata()?, &b.metadata()?))
    }
    /// Whether `name` in `directory` still refers to `file`. A directory that
    /// has itself been removed holds no names.
    pub fn linked(directory: &File, name: &OsStr, file: &File) -> io::Result<bool> {
        let Some(entry) = stat(directory, name)? else {
            return Ok(false);
        };
        let file = file.metadata()?;
        Ok(identity(&entry) == (file.dev(), file.ino()))
    }
    pub(super) fn target_name(_: &File, name: &OsString) -> io::Result<OsString> {
        Ok(name.clone())
    }
    pub(super) fn create_account_file(parent: &File, name: &OsStr) -> io::Result<File> {
        open(
            parent,
            name,
            libc::O_RDWR | libc::O_CREAT | libc::O_EXCL,
            0o600,
        )
    }
    pub(super) fn read_account_file(parent: &File, name: &OsString) -> io::Result<File> {
        read_file(parent, name)
    }
    pub(super) fn remove_account(parent: &File, name: &OsString, _: &File) -> io::Result<()> {
        remove(parent, name)
    }
    pub(super) fn process_owns_marker(pid: u32, created_at: u64) -> io::Result<bool> {
        let Ok(pid) = libc::pid_t::try_from(pid) else {
            return Ok(false);
        };
        if unsafe { libc::kill(pid, 0) } != 0 {
            let error = io::Error::last_os_error();
            return match error.raw_os_error() {
                Some(libc::ESRCH | libc::EPERM) => Ok(false),
                _ => Err(error),
            };
        }
        match process_started_at(pid) {
            Ok(started) => Ok(started <= created_at),
            Err(error) if error.kind() == io::ErrorKind::Unsupported => Ok(true),
            Err(error)
                if error.raw_os_error() == Some(libc::ESRCH)
                    || matches!(
                        error.kind(),
                        io::ErrorKind::NotFound | io::ErrorKind::PermissionDenied
                    ) =>
            {
                Ok(false)
            }
            Err(error) => Err(error),
        }
    }

    #[cfg(target_os = "macos")]
    fn process_started_at(pid: libc::pid_t) -> io::Result<u64> {
        let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of_val(&info) as libc::c_int;
        let inspected = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDTBSDINFO,
                0,
                (&mut info as *mut libc::proc_bsdinfo).cast(),
                size,
            )
        };
        if inspected != size {
            return Err(io::Error::last_os_error());
        }
        Ok(info.pbi_start_tvsec * 1000 + info.pbi_start_tvusec / 1000)
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn process_started_at(pid: libc::pid_t) -> io::Result<u64> {
        let invalid = || io::Error::new(io::ErrorKind::InvalidData, "invalid process start time");
        let stat = std::fs::read(format!("/proc/{pid}/stat"))?;
        let stat = String::from_utf8_lossy(&stat);
        let ticks = stat
            .rsplit_once(')')
            .and_then(|(_, fields)| fields.split_whitespace().nth(19))
            .and_then(|value| value.parse::<u64>().ok())
            .ok_or_else(invalid)?;
        let boot = std::fs::read_to_string("/proc/stat")?
            .lines()
            .find_map(|line| line.strip_prefix("btime "))
            .and_then(|value| value.parse::<u64>().ok())
            .ok_or_else(invalid)?;
        let frequency = u64::try_from(unsafe { libc::sysconf(libc::_SC_CLK_TCK) })
            .ok()
            .filter(|value| *value > 0)
            .ok_or_else(invalid)?;
        boot.checked_mul(1000)
            .and_then(|boot| {
                ticks
                    .checked_mul(1000)?
                    .checked_div(frequency)?
                    .checked_add(boot)
            })
            .ok_or_else(invalid)
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "android")))]
    fn process_started_at(_: libc::pid_t) -> io::Result<u64> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "process start time is unavailable on this platform",
        ))
    }

    fn open(
        parent: &File,
        name: &OsStr,
        flags: libc::c_int,
        mode: libc::c_uint,
    ) -> io::Result<File> {
        let name = c_name(name)?;
        let fd = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                name.as_ptr(),
                flags | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
                mode,
            )
        };
        if fd < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(unsafe { File::from_raw_fd(fd) })
        }
    }
    fn regular(file: File) -> io::Result<File> {
        if file.metadata()?.is_file() {
            Ok(file)
        } else {
            Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "expected a regular file",
            ))
        }
    }
    pub fn open_root(path: &Path) -> Result<File> {
        if !path.is_absolute() {
            return Err(FileStorageError::InvalidPath(
                "authorized roots must be absolute",
            ));
        }
        let mut root = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open("/")?;
        for component in path.components() {
            match component {
                Component::RootDir => {}
                Component::Normal(name) => {
                    root = open(&root, name, libc::O_RDONLY | libc::O_DIRECTORY, 0)?
                }
                _ => return Err(FileStorageError::InvalidPath("invalid authorized root")),
            }
        }
        Ok(root)
    }
    pub fn parent(mut root: File, path: &Path) -> io::Result<(File, OsString)> {
        let mut components = path.iter().peekable();
        while let Some(name) = components.next() {
            if components.peek().is_none() {
                return Ok((root, name.to_os_string()));
            }
            root = open_directory(&root, name)?;
        }
        unreachable!("validated file path")
    }
    /// Opens a business directory below `parent`; the reserved directory is
    /// refused under every spelling the filesystem equates with it.
    pub fn open_directory(parent: &File, name: &OsStr) -> io::Result<File> {
        let next = open(parent, name, libc::O_RDONLY | libc::O_DIRECTORY, 0)?;
        match reserved_directory(parent, OsStr::new(LOCK_DIRECTORY))? {
            Some(reserved) if same_inode(&next.metadata()?, &reserved.metadata()?) => Err(
                io::Error::new(io::ErrorKind::InvalidInput, "reserved storage directory"),
            ),
            _ => Ok(next),
        }
    }
    /// Opens a directory to remove; a filesystem mounted there is never
    /// entered. A leftover tree may itself hold the reserved directory's name.
    pub fn subdirectory(parent: &File, name: &OsStr, tree: Tree<'_>) -> io::Result<File> {
        let directory = match tree {
            Tree::Business => open_directory(parent, name)?,
            Tree::Leftover { .. } => open(parent, name, libc::O_RDONLY | libc::O_DIRECTORY, 0)?,
        };
        if directory.metadata()?.dev() != parent.metadata()?.dev() {
            return Err(io::Error::new(
                io::ErrorKind::CrossesDevices,
                "directory on another filesystem",
            ));
        }
        Ok(directory)
    }
    /// Opens one of this module's directories in `directory`, if present.
    pub fn reserved_directory(directory: &File, name: &OsStr) -> io::Result<Option<File>> {
        match open(directory, name, libc::O_RDONLY | libc::O_DIRECTORY, 0) {
            Ok(locks) => Ok(Some(locks)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }
    pub fn open_key(locks: &File, name: &OsStr) -> io::Result<File> {
        regular(open(locks, name, libc::O_RDWR, 0)?)
    }
    #[allow(clippy::unnecessary_cast)]
    fn identity(stat: &libc::stat) -> Identity {
        (stat.st_dev as u64, stat.st_ino as u64)
    }
    fn kind(stat: &libc::stat) -> EntryKind {
        match stat.st_mode & libc::S_IFMT {
            libc::S_IFREG => EntryKind::File,
            libc::S_IFDIR => EntryKind::Directory,
            _ => EntryKind::Other,
        }
    }
    pub fn entry_kind(parent: &File, name: &OsStr) -> io::Result<Option<EntryKind>> {
        Ok(stat(parent, name)?.as_ref().map(kind))
    }
    /// Identities of the reserved directory and account marker in `directory`.
    pub fn reserved(directory: &File) -> io::Result<[Option<Identity>; 2]> {
        let entry =
            |name| Ok::<_, io::Error>(stat(directory, OsStr::new(name))?.as_ref().map(identity));
        Ok([entry(LOCK_DIRECTORY)?, entry("auth.json.lock")?])
    }
    pub fn entries(directory: &File) -> io::Result<Vec<(DirectoryEntry, Identity)>> {
        let mut entries = Vec::new();
        for name in names(directory)? {
            if let Some(stat) = stat(directory, &name)? {
                let identity = identity(&stat);
                entries.push((
                    DirectoryEntry {
                        name,
                        kind: kind(&stat),
                    },
                    identity,
                ));
            }
        }
        Ok(entries)
    }
    fn names(directory: &File) -> io::Result<Vec<OsString>> {
        let fd = open(
            directory,
            OsStr::new("."),
            libc::O_RDONLY | libc::O_DIRECTORY,
            0,
        )?
        .into_raw_fd();
        let stream = unsafe { libc::fdopendir(fd) };
        if stream.is_null() {
            let error = io::Error::last_os_error();
            drop(unsafe { File::from_raw_fd(fd) });
            return Err(error);
        }
        struct Stream(*mut libc::DIR);
        impl Drop for Stream {
            fn drop(&mut self) {
                unsafe { libc::closedir(self.0) };
            }
        }
        let stream = Stream(stream);
        let mut names = Vec::new();
        loop {
            unsafe { *errno_location() = 0 };
            let entry = unsafe { libc::readdir(stream.0) };
            if entry.is_null() {
                let error = io::Error::last_os_error();
                return match error.raw_os_error() {
                    Some(0) => Ok(names),
                    _ => Err(error),
                };
            }
            let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
            if name != b"." && name != b".." {
                names.push(OsStr::from_bytes(name).to_os_string());
            }
        }
    }
    pub fn read_file(parent: &File, name: &OsString) -> io::Result<File> {
        regular(open(parent, name, libc::O_RDONLY, 0)?)
    }
    pub fn read_user_file(parent: &File, name: &OsString) -> io::Result<File> {
        let file = read_file(parent, name)?;
        let observed = file.metadata()?;
        #[allow(clippy::unnecessary_cast)]
        if let Some(account_lock) = stat(parent, OsStr::new("auth.json.lock"))?
            && observed.dev() as libc::dev_t == account_lock.st_dev
            && observed.ino() == account_lock.st_ino
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "reserved storage lock",
            ));
        }
        Ok(file)
    }
    pub fn create_temp(
        parent: &File,
        name: &OsString,
        _: &File,
        _: &OsString,
        permissions: Option<&Permissions>,
    ) -> io::Result<File> {
        let mode = permissions.map_or(0o666, |permissions| permissions.mode() & 0o777);
        regular(open(
            parent,
            name,
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
            mode,
        )?)
    }
    fn stat(parent: &File, name: &OsStr) -> io::Result<Option<libc::stat>> {
        let name = c_name(name)?;
        let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
        let result = unsafe {
            libc::fstatat(
                parent.as_raw_fd(),
                name.as_ptr(),
                stat.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if result < 0 {
            let error = io::Error::last_os_error();
            return if error.kind() == io::ErrorKind::NotFound {
                Ok(None)
            } else {
                Err(error)
            };
        }
        Ok(Some(unsafe { stat.assume_init() }))
    }
    pub fn permissions(parent: &File, name: &OsString) -> io::Result<Option<Permissions>> {
        let Some(stat) = stat(parent, name)? else {
            return Ok(None);
        };
        if stat.st_mode & libc::S_IFMT != libc::S_IFREG {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "expected a regular file",
            ));
        }
        if let Some(account_lock) = self::stat(parent, OsStr::new("auth.json.lock"))?
            && stat.st_dev == account_lock.st_dev
            && stat.st_ino == account_lock.st_ino
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "reserved storage lock",
            ));
        }
        #[allow(clippy::unnecessary_cast)]
        let mode = stat.st_mode as u32 & 0o7777;
        Ok(Some(Permissions::from_mode(mode)))
    }
    pub fn sync_replacement(_: &File, source: &File, target: &File) -> io::Result<()> {
        let target_sync = target.sync_all();
        let source_sync = source.sync_all();
        target_sync?;
        source_sync
    }
    pub fn replace(
        _: &File,
        source_parent: &File,
        source: &OsString,
        target_parent: &File,
        target: &OsString,
    ) -> io::Result<()> {
        let source = c_name(source)?;
        let target = c_name(target)?;
        let result = unsafe {
            libc::renameat(
                source_parent.as_raw_fd(),
                source.as_ptr(),
                target_parent.as_raw_fd(),
                target.as_ptr(),
            )
        };
        if result == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
    fn unlink(parent: &File, name: &OsStr, flags: libc::c_int) -> io::Result<()> {
        let name = c_name(name)?;
        let result = unsafe { libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), flags) };
        if result == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
    /// Removes a non-directory entry; a link is removed, never followed.
    pub fn remove(parent: &File, name: &OsStr) -> io::Result<()> {
        unlink(parent, name, 0)
    }
    /// Removes a link or special file. One replaced since it was listed, as
    /// by a key or business file, fails instead; a replacement right before
    /// the unlink stays possible, since nothing is unlinked through a handle.
    pub fn remove_other(parent: &File, name: &OsStr) -> io::Result<()> {
        match stat(parent, name)? {
            None => Ok(()),
            Some(stat) if kind(&stat) == EntryKind::Other => remove(parent, name),
            Some(_) => Err(io::Error::other("entry replaced during removal")),
        }
    }
    pub fn remove_directory(parent: &File, name: &OsStr) -> io::Result<()> {
        unlink(parent, name, libc::AT_REMOVEDIR)
    }
    /// The directories a removal descended from. Each is reopened through
    /// `..` on the way back and must still be the directory it was, so a
    /// removal holds one directory descriptor however deep the tree is.
    #[derive(Default)]
    pub struct Ancestors(Vec<(u64, u64)>);
    impl Ancestors {
        pub fn push(&mut self, directory: File) -> io::Result<()> {
            let metadata = directory.metadata()?;
            self.0.push((metadata.dev(), metadata.ino()));
            Ok(())
        }
        /// The directory `child` was entered from.
        pub fn pop(&mut self, child: &File) -> io::Result<File> {
            let entered_from = self.0.pop().expect("a directory removal descended from");
            let parent = open(
                child,
                OsStr::new(".."),
                libc::O_RDONLY | libc::O_DIRECTORY,
                0,
            )?;
            let metadata = parent.metadata()?;
            if (metadata.dev(), metadata.ino()) != entered_from {
                return Err(io::Error::other("directory moved during removal"));
            }
            Ok(parent)
        }
    }

    #[cfg(test)]
    #[test]
    fn private_temporary_file_creation() {
        const PROBE: &str = "DIM_FILE_STORAGE_TEMP_PRIVACY_PROBE";
        if let Some(directory) = std::env::var_os(PROBE) {
            unsafe { libc::umask(0o022) };
            let parent = File::open(directory).unwrap();
            assert_eq!(
                parent.metadata().unwrap().permissions().mode() & 0o777,
                0o755
            );
            for (name, permissions, expected) in [
                ("private", Some(Permissions::from_mode(0o600)), 0o600),
                ("new", None, 0o644),
            ] {
                let name = OsString::from(name);
                let mut file =
                    create_temp(&parent, &name, &parent, &name, permissions.as_ref()).unwrap();
                assert_eq!(
                    file.metadata().unwrap().permissions().mode() & 0o777,
                    expected
                );
                file.write_all(b"private contents").unwrap();
                assert_eq!(
                    file.metadata().unwrap().permissions().mode() & 0o777,
                    expected
                );
            }
            return;
        }
        let directory = std::env::temp_dir().join(format!("file-storage-{}", Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        std::fs::set_permissions(&directory, Permissions::from_mode(0o755)).unwrap();
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "file_storage::native::private_temporary_file_creation",
                "--nocapture",
            ])
            .env(PROBE, &directory)
            .output()
            .unwrap();
        let contents = std::fs::read(directory.join("private"));
        std::fs::remove_dir_all(directory).unwrap();
        assert!(
            child.status.success(),
            "{}{}",
            String::from_utf8_lossy(&child.stdout),
            String::from_utf8_lossy(&child.stderr)
        );
        assert_eq!(contents.unwrap(), b"private contents");
    }
}

/// Hooks that a race test runs, once, after a key of the named file was opened
/// and before it is locked.
#[cfg(test)]
static BEFORE_KEY_LOCK: Mutex<Vec<(OsString, KeyLockHook)>> = Mutex::new(Vec::new());
#[cfg(test)]
type KeyLockHook = Box<dyn FnOnce() + Send>;

#[cfg(test)]
fn before_key_lock(name: &OsStr) {
    let hook = {
        let mut hooks = BEFORE_KEY_LOCK.lock().unwrap();
        let index = hooks.iter().position(|(hooked, _)| hooked == name);
        index.map(|index| hooks.swap_remove(index).1)
    };
    if let Some(hook) = hook {
        hook();
    }
}

/// Interleavings of locking and directory removal that the public API cannot
/// produce reliably: a hook runs the competing operations after a key was
/// opened and before it is locked.
#[cfg(test)]
#[test]
fn lock_keys_stay_exclusive_across_concurrent_directory_removal() {
    struct Fixture(PathBuf);
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let root = std::env::temp_dir()
        .canonicalize()
        .unwrap()
        .join(format!("file-storage-race-{}", Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let _fixture = Fixture(root.clone());
    let storage = FileStorage::open(Arc::new(Mutex::new(vec![root.clone()]))).unwrap();
    let directory = |name: &str| {
        std::fs::create_dir(root.join(name)).unwrap();
        let parent = native::open_root(&root).unwrap();
        let directory = native::open_directory(&parent, OsStr::new(name)).unwrap();
        let key = OsString::from(format!("key-{}", Uuid::new_v4()));
        drop(physical_lock(&directory, &key, &storage).unwrap());
        (directory, key)
    };
    let on_key_lock = |name: &OsString, hook: KeyLockHook| {
        BEFORE_KEY_LOCK.lock().unwrap().push((name.clone(), hook));
    };
    let held = Arc::new(Mutex::new(Vec::new()));
    let remove_and_relock = |directory: &File, name: &OsString| -> KeyLockHook {
        let directory = directory.try_clone().unwrap();
        let (name, storage, held) = (name.clone(), storage.clone(), held.clone());
        Box::new(move || {
            remove_resources(&directory, &storage, &CancellationToken::new()).unwrap();
            let (_, lock) = physical_lock(&directory, &name, &storage).unwrap();
            held.lock().unwrap().push(lock);
        })
    };

    let (locker, name) = directory("locker");
    on_key_lock(&name, remove_and_relock(&locker, &name));
    assert!(matches!(
        physical_lock(&locker, &name, &storage),
        Err(FileStorageError::Locked)
    ));

    let (remover, name) = directory("remover");
    on_key_lock(&name, remove_and_relock(&remover, &name));
    let removal = remove_resources(&remover, &storage, &CancellationToken::new());
    assert!(
        matches!(&removal, Err(FileStorageError::Io(error)) if error.kind() == io::ErrorKind::DirectoryNotEmpty),
        "{removal:?}"
    );
    assert!(matches!(
        physical_lock(&remover, &name, &storage),
        Err(FileStorageError::Locked)
    ));

    let (writer, name) = directory("writer");
    let written = OsString::from(format!("written-{}", Uuid::new_v4()));
    let writing = Arc::new(Mutex::new(None));
    on_key_lock(&name, {
        let (directory, written) = (writer.try_clone().unwrap(), written.clone());
        let (storage, writing) = (storage.clone(), writing.clone());
        Box::new(move || {
            let lock = physical_lock(&directory, &written, &storage).unwrap();
            let temporary = create_temporary(&directory, &written, None).unwrap();
            *writing.lock().unwrap() = Some((lock, temporary));
        })
    });
    assert!(matches!(
        remove_resources(&writer, &storage, &CancellationToken::new()),
        Err(FileStorageError::Locked)
    ));
    let (_lock, (temporary, temp)) = writing.lock().unwrap().take().unwrap();
    native::replace(&temp, &temporary, &written, &writer, &written).unwrap();
    assert!(root.join("writer").join(&written).is_file());
}

#[cfg(windows)]
pub(crate) mod windows;
#[cfg(windows)]
use windows as native;

#[cfg(not(any(unix, windows)))]
use unsupported as native;

#[cfg(not(any(unix, windows)))]
mod unsupported {
    use super::*;
    pub fn open_root(_: &Path) -> Result<File> {
        Err(FileStorageError::Unsupported)
    }
    fn unsupported() -> io::Error {
        io::Error::new(
            io::ErrorKind::Unsupported,
            "file storage requires a Unix or Windows host",
        )
    }
    pub fn parent(_: File, _: &Path) -> io::Result<(File, OsString)> {
        Err(unsupported())
    }
    pub fn read_user_file(_: &File, _: &OsString) -> io::Result<File> {
        Err(unsupported())
    }
    pub fn create_temp(
        _: &File,
        _: &OsString,
        _: &File,
        _: &OsString,
        _: Option<&Permissions>,
    ) -> io::Result<File> {
        Err(unsupported())
    }
    pub fn directories(_: &File) -> io::Result<(File, File)> {
        Err(unsupported())
    }
    pub fn create_directory(_: &File, _: &OsStr) -> io::Result<()> {
        Err(unsupported())
    }
    pub fn sync_directory(_: &File) -> io::Result<()> {
        Err(unsupported())
    }
    pub fn create_key(_: &File, _: &OsStr) -> io::Result<File> {
        Err(unsupported())
    }
    pub fn linked(_: &File, _: &OsStr, _: &File) -> io::Result<bool> {
        Err(unsupported())
    }
    pub fn permissions(_: &File, _: &OsString) -> io::Result<Option<Permissions>> {
        Err(unsupported())
    }
    pub fn replace(_: &File, _: &File, _: &OsString, _: &File, _: &OsString) -> io::Result<()> {
        Err(unsupported())
    }
    pub fn remove(_: &File, _: &OsStr) -> io::Result<()> {
        Err(unsupported())
    }
    pub fn remove_other(_: &File, _: &OsStr) -> io::Result<()> {
        Err(unsupported())
    }
    pub type Identity = ();
    pub fn open_directory(_: &File, _: &OsStr) -> io::Result<File> {
        Err(unsupported())
    }
    pub fn subdirectory(_: &File, _: &OsStr, _: Tree<'_>) -> io::Result<File> {
        Err(unsupported())
    }
    pub fn reserved_directory(_: &File, _: &OsStr) -> io::Result<Option<File>> {
        Err(unsupported())
    }
    pub fn open_key(_: &File, _: &OsStr) -> io::Result<File> {
        Err(unsupported())
    }
    pub fn entry_kind(_: &File, _: &OsStr) -> io::Result<Option<EntryKind>> {
        Err(unsupported())
    }
    pub fn reserved(_: &File) -> io::Result<[Option<Identity>; 2]> {
        Err(unsupported())
    }
    pub fn entries(_: &File) -> io::Result<Vec<(DirectoryEntry, Identity)>> {
        Err(unsupported())
    }
    pub fn remove_directory(_: &File, _: &OsStr) -> io::Result<()> {
        Err(unsupported())
    }
    #[derive(Default)]
    pub struct Ancestors;
    impl Ancestors {
        pub fn push(&mut self, _: File) -> io::Result<()> {
            Err(unsupported())
        }
        pub fn pop(&mut self, _: &File) -> io::Result<File> {
            Err(unsupported())
        }
    }
    pub fn same_identity(_: &File, _: &File) -> io::Result<bool> {
        Err(unsupported())
    }
    pub fn create_account_file(_: &File, _: &OsStr) -> io::Result<File> {
        Err(unsupported())
    }
    pub fn read_account_file(_: &File, _: &OsString) -> io::Result<File> {
        Err(unsupported())
    }
    pub fn remove_account(_: &File, _: &OsString, _: &File) -> io::Result<()> {
        Err(unsupported())
    }
    pub fn process_owns_marker(_: u32, _: u64) -> io::Result<bool> {
        Err(unsupported())
    }
    pub fn target_name(_: &File, _: &OsString) -> io::Result<OsString> {
        Err(unsupported())
    }
    pub fn sync_replacement(_: &File, _: &File, _: &File) -> io::Result<()> {
        Err(unsupported())
    }
}

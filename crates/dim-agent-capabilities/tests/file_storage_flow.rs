#![cfg(any(unix, windows))]

use dim_agent_capabilities::file_storage::{
    DirectoryEntry, EntryKind, FileStorage, FileStorageError, FileStoragePlugin,
};
use rutis::host::{CtxHost, FiberState, FiberView};
use rutis::{Ctx, Plugin};
#[cfg(unix)]
use std::os::unix::fs::{PermissionsExt, chown, symlink};
use std::{
    fs::{self, FileTimes, Permissions},
    path::PathBuf,
    process::{Command, Stdio},
    sync::{Arc, OnceLock},
    time::{Duration, SystemTime},
};
use tokio_util::sync::CancellationToken;

const PROBE_ROOT: &str = "DIM_FILE_STORAGE_LOCK_PROBE_ROOT";

struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        let directory = std::env::temp_dir()
            .join("dim-file-storage-flow")
            .join(uuid::Uuid::new_v4().to_string());
        let authorized = directory.join("authorized");
        fs::create_dir_all(&authorized).unwrap();
        #[cfg(unix)]
        {
            chown(&authorized, None, Some(unsafe { libc::getegid() })).unwrap();
            let locks = authorized.join(".dim-file-storage-locks");
            let temps = locks.join(".dim-file-storage-temps");
            fs::create_dir_all(&temps).unwrap();
            for path in [&authorized, &locks, &temps] {
                fs::set_permissions(path, Permissions::from_mode(0o755)).unwrap();
            }
        }
        fs::create_dir(authorized.join("nested")).unwrap();
        fs::create_dir(directory.join("outside")).unwrap();
        Self(directory.canonicalize().unwrap())
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct Consumer {
    captured: Arc<OnceLock<Arc<FileStorage>>>,
}
impl Plugin for Consumer {
    type Deps = (Arc<FileStorage>,);
    fn name(&self) -> &str {
        "test.file-storage-consumer"
    }
    async fn apply(&self, _: &Ctx, (storage,): Self::Deps) -> rutis::Result {
        assert!(self.captured.set(storage).is_ok());
        Ok(())
    }
}

async fn mount(path: PathBuf) -> (Ctx, FiberView, Arc<FileStorage>) {
    let root = Ctx::root().unwrap();
    let plugin = root.plugin(FileStoragePlugin::new(vec![path]));
    let captured = Arc::new(OnceLock::new());
    let consumer = root.plugin(Consumer {
        captured: captured.clone(),
    });
    let mut status = consumer.watch();
    while status.borrow().state != FiberState::Active {
        assert_ne!(
            status.borrow().state,
            FiberState::Failed,
            "{:?}",
            *status.borrow()
        );
        status.changed().await.unwrap();
    }
    let storage = captured.get().unwrap().clone();
    (root, plugin, storage)
}

#[tokio::test]
async fn bounded_reads_preserve_exact_bytes_and_reject_larger_files() {
    let directory = Directory::new();
    let authorized = directory.0.join("authorized");
    let (root, _, storage) = mount(authorized.clone()).await;
    let cancel = CancellationToken::new();
    let path = storage.path(0, "definition.yaml").unwrap();
    let bytes = vec![b'x'; 64 * 1024 + 1];
    fs::write(authorized.join("definition.yaml"), &bytes).unwrap();
    assert_eq!(
        storage
            .read_limited(&path, bytes.len(), &cancel)
            .await
            .unwrap(),
        bytes
    );
    assert!(matches!(
        storage.read_limited(&path, bytes.len() - 1, &cancel).await,
        Err(FileStorageError::Io(error)) if error.kind() == std::io::ErrorKind::InvalidData
    ));
    assert_eq!(storage.read(&path, &cancel).await.unwrap(), bytes);
    fs::write(authorized.join("definition.yaml"), []).unwrap();
    assert!(
        storage
            .read_limited(&path, 0, &cancel)
            .await
            .unwrap()
            .is_empty()
    );
    root.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_first_locks_preserve_exclusion_and_independent_writes() {
    let directory = Directory::new();
    let authorized = directory.0.join("authorized");
    let (root_a, _, a) = mount(authorized.clone()).await;
    let (root_b, _, b) = mount(authorized.clone()).await;
    let cancel = CancellationToken::new();
    for iteration in 0..64 {
        let same_parent = format!("same-{iteration}");
        fs::create_dir(authorized.join(&same_parent)).unwrap();
        let path_a = a.path(0, format!("{same_parent}/mcp.json")).unwrap();
        let path_b = b.path(0, format!("{same_parent}/mcp.json")).unwrap();
        let (first, second) = tokio::join!(a.lock(&path_a, &cancel), b.lock(&path_b, &cancel));
        let mut winner = match (first, second) {
            (Ok(lease), Err(FileStorageError::Locked))
            | (Err(FileStorageError::Locked), Ok(lease)) => lease,
            _ => panic!("first-time contenders must yield one lease and one Locked error"),
        };
        winner
            .replace(b"winner".to_vec(), None, &cancel)
            .await
            .unwrap();
        assert_eq!(b.read(&path_b, &cancel).await.unwrap(), b"winner");
        drop(winner);
        let mut next = b.lock(&path_b, &cancel).await.unwrap();
        next.replace(b"next".to_vec(), None, &cancel).await.unwrap();
        assert_eq!(a.read(&path_a, &cancel).await.unwrap(), b"next");
        drop(next);

        let independent_parent = format!("independent-{iteration}");
        fs::create_dir(authorized.join(&independent_parent)).unwrap();
        let path_a = a.path(0, format!("{independent_parent}/a.json")).unwrap();
        let path_b = b.path(0, format!("{independent_parent}/b.json")).unwrap();
        let (first, second) = tokio::join!(a.lock(&path_a, &cancel), b.lock(&path_b, &cancel));
        let mut first = first.unwrap();
        let mut second = second.unwrap();
        first.replace(b"a".to_vec(), None, &cancel).await.unwrap();
        second.replace(b"b".to_vec(), None, &cancel).await.unwrap();
        assert_eq!(a.read(&path_a, &cancel).await.unwrap(), b"a");
        assert_eq!(b.read(&path_b, &cancel).await.unwrap(), b"b");
    }
    root_a.shutdown().await.unwrap();
    root_b.shutdown().await.unwrap();
}

fn entry(name: &str, kind: EntryKind) -> DirectoryEntry {
    DirectoryEntry {
        name: name.into(),
        kind,
    }
}

#[tokio::test]
async fn directory_listing_and_removal_stay_inside_the_authorized_root() {
    let directory = Directory::new();
    let root_path = directory.0.join("authorized");
    let outside = directory.0.join("outside");
    fs::write(outside.join("secret"), b"outside").unwrap();
    let (root, _, storage) = mount(root_path.clone()).await;
    let cancel = CancellationToken::new();
    let path = |relative: &str| storage.path(0, relative).unwrap();

    for id in ["a1", "a2"] {
        fs::create_dir_all(root_path.join(format!("sessions/s1/attachments/{id}"))).unwrap();
        let file = path(&format!("sessions/s1/attachments/{id}/paste.txt"));
        storage
            .replace(&file, b"pasted".to_vec(), None, &cancel)
            .await
            .unwrap();
    }
    let account = storage
        .lock(&path("sessions/s1/auth.json"), &cancel)
        .await
        .unwrap();
    assert!(root_path.join("sessions/s1/auth.json.lock").exists());
    assert_eq!(
        storage.list(&path("sessions/s1"), &cancel).await.unwrap(),
        [entry("attachments", EntryKind::Directory)]
    );
    assert_eq!(
        storage
            .list(&path("sessions/s1/attachments"), &cancel)
            .await
            .unwrap(),
        [
            entry("a1", EntryKind::Directory),
            entry("a2", EntryKind::Directory)
        ]
    );
    assert_eq!(
        storage
            .list(&path("sessions/s1/attachments/a1"), &cancel)
            .await
            .unwrap(),
        [entry("paste.txt", EntryKind::File)]
    );
    assert!(matches!(
        storage
            .list(&path("sessions/s1/attachments/a1/paste.txt"), &cancel)
            .await,
        Err(FileStorageError::Io(_))
    ));
    if root_path.join("sessions/s1/AUTH.JSON.LOCK").exists() {
        let marker = path("sessions/s1/AUTH.JSON.LOCK");
        assert!(storage.remove(&marker, &cancel).await.is_err());
        let reserved = path("sessions/s1/.DIM-FILE-STORAGE-LOCKS");
        assert!(matches!(
            storage.list(&reserved, &cancel).await,
            Err(FileStorageError::Io(_))
        ));
        assert!(storage.remove(&reserved, &cancel).await.is_err());
        assert!(root_path.join("sessions/s1/auth.json.lock").exists());
        assert!(
            root_path
                .join("sessions/s1/.dim-file-storage-locks")
                .is_dir()
        );
    }
    fs::create_dir_all(root_path.join("aliased/.DIM-FILE-STORAGE-LOCKS")).unwrap();
    storage
        .replace(&path("aliased/data.json"), b"{}".to_vec(), None, &cancel)
        .await
        .unwrap();
    let aliased = storage.list(&path("aliased"), &cancel).await.unwrap();
    if same_file(
        &root_path.join("aliased/.DIM-FILE-STORAGE-LOCKS"),
        &root_path.join("aliased/.dim-file-storage-locks"),
    ) {
        assert_eq!(aliased, [entry("data.json", EntryKind::File)]);
    } else {
        assert_eq!(
            aliased,
            [
                entry(".DIM-FILE-STORAGE-LOCKS", EntryKind::Directory),
                entry("data.json", EntryKind::File)
            ]
        );
    }

    let leased = path("sessions/s1/attachments/a2/paste.txt");
    let mut retained = storage.lock(&leased, &cancel).await.unwrap();
    assert!(matches!(
        storage.remove(&leased, &cancel).await,
        Err(FileStorageError::Locked)
    ));
    assert_eq!(retained.read(&cancel).await.unwrap(), b"pasted");

    storage
        .remove(&path("sessions/s1/attachments/a1"), &cancel)
        .await
        .unwrap();
    assert!(!root_path.join("sessions/s1/attachments/a1").exists());
    assert_eq!(
        storage
            .list(&path("sessions/s1/attachments"), &cancel)
            .await
            .unwrap(),
        [entry("a2", EntryKind::Directory)]
    );

    let session = path("sessions/s1");
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    assert!(matches!(
        storage.remove(&session, &cancelled).await,
        Err(FileStorageError::Cancelled)
    ));
    assert!(matches!(
        storage.list(&session, &cancelled).await,
        Err(FileStorageError::Cancelled)
    ));
    assert!(matches!(
        storage.remove(&session, &cancel).await,
        Err(FileStorageError::Locked)
    ));
    assert!(matches!(
        storage.lock(&leased, &cancel).await,
        Err(FileStorageError::Locked)
    ));
    retained
        .replace(b"still leased".to_vec(), None, &cancel)
        .await
        .unwrap();
    assert_eq!(
        storage.read(&leased, &cancel).await.unwrap(),
        b"still leased"
    );
    drop(retained);
    assert!(matches!(
        storage.remove(&session, &cancel).await,
        Err(FileStorageError::Locked)
    ));
    assert!(!root_path.join("sessions/s1/attachments").exists());
    assert!(root_path.join("sessions/s1/auth.json.lock").exists());
    assert!(matches!(
        storage.lock(&path("sessions/s1/auth.json"), &cancel).await,
        Err(FileStorageError::Locked)
    ));
    drop(account);
    storage.remove(&session, &cancel).await.unwrap();
    assert!(!root_path.join("sessions/s1").exists());
    let sessions = path("sessions");
    assert!(storage.list(&sessions, &cancel).await.unwrap().is_empty());
    storage.remove(&path("aliased"), &cancel).await.unwrap();
    assert!(!root_path.join("aliased").exists());

    storage.remove(&path("sessions/s1"), &cancel).await.unwrap();
    storage
        .remove(&path("missing/parent/file"), &cancel)
        .await
        .unwrap();
    assert!(matches!(
        storage.list(&path("sessions/s1"), &cancel).await,
        Err(FileStorageError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound
    ));

    let leftovers = root_path.join("leftovers");
    fs::create_dir_all(leftovers.join(".dim-file-storage-old/nested")).unwrap();
    fs::write(leftovers.join(".dim-file-storage-old/nested/file"), b"old").unwrap();
    fs::write(leftovers.join(".dim-file-storage-stray"), b"old").unwrap();
    fs::create_dir_all(leftovers.join("marker-directory/auth.json.lock/nested")).unwrap();
    fs::write(leftovers.join("marker-directory/auth.json"), b"{}").unwrap();
    fs::create_dir(leftovers.join("marker-link")).unwrap();
    symlink(&outside, leftovers.join("marker-link/auth.json.lock")).unwrap();
    fs::write(leftovers.join("marker-link/auth.json"), b"{}").unwrap();
    fs::create_dir_all(leftovers.join("marker-alias/AUTH.JSON.LOCK/nested")).unwrap();
    fs::write(leftovers.join("marker-alias/auth.json"), b"{}").unwrap();
    let wrong_types = leftovers.join("wrong-types");
    fs::create_dir(&wrong_types).unwrap();
    fs::write(wrong_types.join(".dim-file-storage-locks"), b"old").unwrap();
    fs::write(wrong_types.join(".dim-file-storage-temps"), b"old").unwrap();
    fs::write(wrong_types.join("data.json"), b"{}").unwrap();
    fs::create_dir_all(leftovers.join("temps-file/.dim-file-storage-locks")).unwrap();
    fs::write(
        leftovers.join("temps-file/.dim-file-storage-locks/.dim-file-storage-temps"),
        b"old",
    )
    .unwrap();
    fs::write(leftovers.join("temps-file/data.json"), b"{}").unwrap();
    for file in [
        "leftovers/wrong-types/data.json",
        "leftovers/temps-file/data.json",
    ] {
        assert!(matches!(
            storage.lock(&path(file), &cancel).await,
            Err(FileStorageError::Io(_))
        ));
    }
    fs::create_dir(leftovers.join("stray")).unwrap();
    storage
        .replace(
            &path("leftovers/stray/data.json"),
            b"{}".to_vec(),
            None,
            &cancel,
        )
        .await
        .unwrap();
    let locks = leftovers.join("stray/.dim-file-storage-locks");
    let temps = locks.join(".dim-file-storage-temps");
    fs::create_dir_all(locks.join("tree/.dim-file-storage-locks/.dim-file-storage-temps")).unwrap();
    fs::write(locks.join("tree/.dim-file-storage-locks/key"), b"old").unwrap();
    fs::create_dir(locks.join("blocked.json")).unwrap();
    fs::write(leftovers.join("stray/blocked.json"), b"{}").unwrap();
    fs::remove_file(locks.join("auth.json.lock")).unwrap();
    fs::create_dir(locks.join("auth.json.lock")).unwrap();
    fs::create_dir_all(temps.join("tree/nested")).unwrap();
    fs::write(temps.join("tree/nested/file"), b"old").unwrap();
    fs::create_dir(temps.join("data.json")).unwrap();
    fs::write(temps.join(".dim-file-storage-temps"), b"old").unwrap();
    fs::write(leftovers.join("stray/.DIM-FILE-STORAGE-TEMPS"), b"user").unwrap();
    assert!(matches!(
        storage
            .lock(&path("leftovers/stray/blocked.json"), &cancel)
            .await,
        Err(FileStorageError::Io(_))
    ));
    assert!(matches!(
        storage
            .replace(
                &path("leftovers/stray/data.json"),
                b"[]".to_vec(),
                None,
                &cancel
            )
            .await,
        Err(FileStorageError::Io(_))
    ));
    assert_eq!(
        storage
            .list(&path("leftovers/stray"), &cancel)
            .await
            .unwrap(),
        [
            entry(".DIM-FILE-STORAGE-TEMPS", EntryKind::File),
            entry("blocked.json", EntryKind::File),
            entry("data.json", EntryKind::File)
        ]
    );
    assert_eq!(
        storage.list(&path("leftovers"), &cancel).await.unwrap(),
        [
            entry("marker-alias", EntryKind::Directory),
            entry("marker-directory", EntryKind::Directory),
            entry("marker-link", EntryKind::Directory),
            entry("stray", EntryKind::Directory),
            entry("temps-file", EntryKind::Directory),
            entry("wrong-types", EntryKind::Directory)
        ]
    );
    if leftovers.join(".DIM-FILE-STORAGE-STRAY").exists() {
        let alias = storage
            .lock(&path("leftovers/.DIM-FILE-STORAGE-STRAY"), &cancel)
            .await
            .unwrap();
        assert!(matches!(
            storage.remove(&path("leftovers"), &cancel).await,
            Err(FileStorageError::Locked)
        ));
        assert_eq!(
            fs::read(leftovers.join(".dim-file-storage-stray")).unwrap(),
            b"old"
        );
        drop(alias);
    }
    storage.remove(&path("leftovers"), &cancel).await.unwrap();
    assert!(fs::symlink_metadata(&leftovers).is_err());
    assert_eq!(fs::read(outside.join("secret")).unwrap(), b"outside");

    #[cfg(unix)]
    if unsafe { libc::geteuid() } != 0 {
        let (failing_root, _, failing) = mount(root_path.clone()).await;
        fs::create_dir(root_path.join("abandoned")).unwrap();
        let abandoned = failing.path(0, "abandoned").unwrap();
        let account = failing
            .lock(&failing.path(0, "abandoned/auth.json").unwrap(), &cancel)
            .await
            .unwrap();
        let marker = root_path.join("abandoned/auth.json.lock");
        fs::set_permissions(&marker, Permissions::from_mode(0o000)).unwrap();
        drop(account);
        fs::set_permissions(&marker, Permissions::from_mode(0o600)).unwrap();
        assert!(marker.exists());
        failing.remove(&abandoned, &cancel).await.unwrap();
        assert!(fs::symlink_metadata(root_path.join("abandoned")).is_err());
        failing_root.shutdown().await.unwrap();
    }

    fs::create_dir_all(root_path.join("sessions/s2")).unwrap();
    symlink(&outside, root_path.join("sessions/s2/linked-directory")).unwrap();
    symlink(
        outside.join("secret"),
        root_path.join("sessions/s2/linked-file"),
    )
    .unwrap();
    assert_eq!(
        storage.list(&path("sessions/s2"), &cancel).await.unwrap(),
        [
            entry("linked-directory", EntryKind::Other),
            entry("linked-file", EntryKind::Other)
        ]
    );
    symlink(&outside, root_path.join("linked")).unwrap();
    assert!(matches!(
        storage.list(&path("linked"), &cancel).await,
        Err(FileStorageError::Io(_))
    ));
    assert!(matches!(
        storage.remove(&path("linked/secret"), &cancel).await,
        Err(FileStorageError::Io(_))
    ));
    storage.remove(&path("sessions/s2"), &cancel).await.unwrap();
    storage.remove(&path("linked"), &cancel).await.unwrap();
    assert!(fs::symlink_metadata(root_path.join("sessions/s2")).is_err());
    assert!(fs::symlink_metadata(root_path.join("linked")).is_err());
    assert_eq!(fs::read(outside.join("secret")).unwrap(), b"outside");

    root.shutdown().await.unwrap();
    assert!(matches!(
        storage.list(&sessions, &cancel).await,
        Err(FileStorageError::Closed)
    ));
    assert!(matches!(
        storage.remove(&sessions, &cancel).await,
        Err(FileStorageError::Closed)
    ));
}

/// Removal holds a bounded number of descriptors and does not recurse: a
/// directory that used many names and a tree deeper than recursion on a
/// worker stack could remove go under a small descriptor limit.
/// Session files create their directories level by level through storage.
#[tokio::test]
async fn directory_creation_stays_inside_the_authorized_root() {
    let directory = Directory::new();
    let root_path = directory.0.join("authorized");
    let outside = directory.0.join("outside");
    let (root, _, storage) = mount(root_path.clone()).await;
    let cancel = CancellationToken::new();
    let path = |relative: &str| storage.path(0, relative).unwrap();

    for _ in 0..2 {
        storage
            .create_directory(&path("files"), &cancel)
            .await
            .unwrap();
    }
    storage
        .create_directory(&path("files/one"), &cancel)
        .await
        .unwrap();
    for created in ["files", "one"] {
        let parent = if created == "files" {
            root_path.clone()
        } else {
            root_path.join("files")
        };
        assert!(
            !parent
                .join(".dim-file-storage-locks")
                .join(created)
                .exists()
        );
    }
    #[cfg(unix)]
    for created in ["files", "files/one"] {
        let mode = fs::metadata(root_path.join(created))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o700, "{created}");
    }
    storage
        .replace(
            &path("files/one/paste.txt"),
            b"pasted".to_vec(),
            None,
            &cancel,
        )
        .await
        .unwrap();
    assert_eq!(
        storage.list(&path("files"), &cancel).await.unwrap(),
        [entry("one", EntryKind::Directory)]
    );

    assert!(matches!(
        storage.create_directory(&path("missing/child"), &cancel).await,
        Err(FileStorageError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound
    ));
    assert!(!root_path.join("missing").exists());

    for reserved in [
        ".dim-file-storage-locks",
        "files/.dim-file-storage-temps",
        "files/auth.json.lock",
    ] {
        assert!(matches!(
            storage.path(0, reserved),
            Err(FileStorageError::InvalidPath(_))
        ));
    }
    let case_insensitive = root_path.join("FILES").exists();
    for alias in [".DIM-FILE-STORAGE-LOCKS", "AUTH.JSON.LOCK"] {
        let created = storage
            .create_directory(&path(&format!("files/{alias}")), &cancel)
            .await;
        if case_insensitive {
            assert!(
                matches!(created, Err(FileStorageError::InvalidPath(_))),
                "{alias}: {created:?}"
            );
        } else {
            created.unwrap();
        }
    }
    assert!(!root_path.join("files/auth.json.lock").exists());
    assert_eq!(
        storage.list(&path("files"), &cancel).await.unwrap().len(),
        if case_insensitive { 1 } else { 3 }
    );

    fs::write(root_path.join("files/plain"), b"plain").unwrap();
    symlink(&outside, root_path.join("files/linked")).unwrap();
    for other in ["files/plain", "files/linked"] {
        assert!(matches!(
            storage.create_directory(&path(other), &cancel).await,
            Err(FileStorageError::Io(_))
        ));
    }
    assert_eq!(fs::read(root_path.join("files/plain")).unwrap(), b"plain");
    assert!(
        fs::symlink_metadata(root_path.join("files/linked"))
            .unwrap()
            .file_type()
            .is_symlink()
            || cfg!(windows)
    );
    assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);

    let cancelled = CancellationToken::new();
    cancelled.cancel();
    assert!(matches!(
        storage
            .create_directory(&path("files/cancelled"), &cancelled)
            .await,
        Err(FileStorageError::Cancelled)
    ));
    assert!(!root_path.join("files/cancelled").exists());

    storage.remove(&path("files"), &cancel).await.unwrap();
    assert!(!root_path.join("files").exists());
    root.shutdown().await.unwrap();
}

#[test]
fn removal_scales_with_directory_size() {
    #[cfg(unix)]
    if std::env::var_os(SCALE_PROBE).is_none() {
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "removal_scales_with_directory_size",
                "--nocapture",
            ])
            .env(SCALE_PROBE, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    #[cfg(unix)]
    unsafe {
        let mut limit = std::mem::zeroed::<libc::rlimit>();
        assert_eq!(libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit), 0);
        limit.rlim_cur = 64;
        assert_eq!(libc::setrlimit(libc::RLIMIT_NOFILE, &limit), 0);
    }
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_stack_size(256 * 1024)
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let directory = Directory::new();
            let root_path = directory.0.join("authorized");
            let (root, _, storage) = mount(root_path.clone()).await;
            let cancel = CancellationToken::new();

            fs::create_dir(root_path.join("names")).unwrap();
            for index in 0..300 {
                let file = storage.path(0, format!("names/{index}.json")).unwrap();
                storage
                    .replace(&file, b"{}".to_vec(), None, &cancel)
                    .await
                    .unwrap();
            }
            let names = storage.path(0, "names").unwrap();
            storage.remove(&names, &cancel).await.unwrap();
            assert!(fs::symlink_metadata(root_path.join("names")).is_err());

            let relative = deep_tree(&root_path.join("deep"), SCALE_DEPTH);
            let leaf = storage.path(0, relative.join("leaf.txt")).unwrap();
            storage
                .replace(&leaf, b"deep".to_vec(), None, &cancel)
                .await
                .unwrap();
            let deep = storage.path(0, "deep").unwrap();
            storage.remove(&deep, &cancel).await.unwrap();
            assert!(fs::symlink_metadata(root_path.join("deep")).is_err());
            root.shutdown().await.unwrap();
        });
}

#[cfg(unix)]
const SCALE_PROBE: &str = "DIM_FILE_STORAGE_SCALE_PROBE";
const SCALE_DEPTH: usize = 1_000;

/// Creates `base` with a chain of `depth` directories below it and returns
/// the chain's path relative to the authorized root.
fn deep_tree(base: &std::path::Path, depth: usize) -> PathBuf {
    let relative: PathBuf = std::iter::once("deep")
        .chain(std::iter::repeat_n("d", depth))
        .collect();
    #[cfg(unix)]
    {
        use std::os::fd::{AsRawFd, FromRawFd};
        fs::create_dir(base).unwrap();
        let mut current = fs::File::open(base).unwrap();
        for _ in 0..depth {
            let created = unsafe { libc::mkdirat(current.as_raw_fd(), c"d".as_ptr(), 0o755) };
            assert_eq!(created, 0, "{}", std::io::Error::last_os_error());
            let fd = unsafe {
                libc::openat(
                    current.as_raw_fd(),
                    c"d".as_ptr(),
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
                )
            };
            assert!(fd >= 0, "{}", std::io::Error::last_os_error());
            current = unsafe { fs::File::from_raw_fd(fd) };
        }
    }
    #[cfg(windows)]
    {
        let mut path = base.to_path_buf();
        path.extend(std::iter::repeat_n("d", depth));
        fs::create_dir_all(path).unwrap();
    }
    relative
}

#[tokio::test]
async fn late_roots_preserve_indices_and_generation_boundaries_on_reload() {
    let directory = Directory::new();
    let root = Ctx::root().unwrap();
    let plugin = root.plugin(FileStoragePlugin::new(vec![directory.0.join("authorized")]));
    (&plugin).await.unwrap();
    let files = root.get::<FileStorage>().unwrap();
    let cancel = CancellationToken::new();
    let late = directory.0.join("outside");
    fs::write(late.join("state.json"), b"late workspace").unwrap();
    assert!(files.path(1, "state.json").is_err());
    let (first, same) = tokio::join!(
        files.register_root(late.clone(), &cancel),
        files.register_root(late.clone(), &cancel)
    );
    assert_eq!(first.unwrap(), 1);
    assert_eq!(same.unwrap(), 1);
    let path = files.path(1, "state.json").unwrap();
    assert_eq!(files.read(&path, &cancel).await.unwrap(), b"late workspace");
    plugin.restart().await.unwrap();
    (&plugin).await.unwrap();
    let reloaded = root.get::<FileStorage>().unwrap();
    assert!(matches!(
        files.register_root(late.clone(), &cancel).await,
        Err(FileStorageError::Closed)
    ));
    assert!(matches!(
        reloaded.read(&path, &cancel).await,
        Err(FileStorageError::InvalidPath(_))
    ));
    assert_eq!(reloaded.register_root(late, &cancel).await.unwrap(), 1);
    assert_eq!(
        reloaded
            .read(&reloaded.path(1, "state.json").unwrap(), &cancel)
            .await
            .unwrap(),
        b"late workspace"
    );
    root.shutdown().await.unwrap();
}

/// The admitted-but-unsettled drain assertion depends on a single blocking
/// worker; a second one makes the result timing-dependent.
#[test]
fn file_storage_flow() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .max_blocking_threads(1)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        tokio::time::timeout(Duration::from_secs(if cfg!(windows) { 120 } else { 20 }), async {
            let cancel = CancellationToken::new();
            if let Some(root_path) = std::env::var_os(PROBE_ROOT) {
                let (root, _, storage) = mount(root_path.into()).await;
                let auth = storage.path(0, "auth.json").unwrap();
                assert!(matches!(storage.lock(&auth, &cancel).await, Err(FileStorageError::Locked)));
                root.shutdown().await.unwrap();
                return;
            }

            let directory = Directory::new();
            let root_path = directory.0.join("authorized");
            let outside = directory.0.join("outside/secret");
            fs::write(&outside, b"outside").unwrap();
            let (root, plugin, storage) = mount(root_path.clone()).await;
            for path in ["", "../outside/secret", "/etc/passwd", "nested/../../outside", "auth.json.lock", ".dim-file-storage-temp-reserved"] {
                assert!(matches!(storage.path(0, path), Err(FileStorageError::InvalidPath(_))));
            }
            assert!(storage.path(1, "auth.json").is_err());
            let auth = storage.path(0, "auth.json").unwrap();
            storage.replace(&auth, b"first".to_vec(), Some(initial_permissions(&root_path)), &cancel).await.unwrap();
            assert_eq!(storage.read(&auth, &cancel).await.unwrap(), b"first");
            let mut lease = storage.lock(&auth, &cancel).await.unwrap();
            assert!(matches!(storage.lock(&auth, &cancel).await, Err(FileStorageError::Locked)));
            assert!(matches!(storage.replace(&auth, b"conflict".to_vec(), None, &cancel).await, Err(FileStorageError::Locked)));
            if fs::metadata(root_path.join("AUTH.JSON.LOCK")).is_ok() {
                let marker_alias = storage.path(0, "AUTH.JSON.LOCK").unwrap();
                assert!(matches!(storage.read(&marker_alias, &cancel).await, Err(FileStorageError::Io(_))));
            }
            let settings = storage.path(0, "nested/Cargo.lock").unwrap();
            let mut unrelated = storage.lock(&settings, &cancel).await.unwrap();
            unrelated.replace(b"ordinary lock-named file".to_vec(), None, &cancel).await.unwrap();
            assert_eq!(storage.read(&settings, &cancel).await.unwrap(), b"ordinary lock-named file");
            drop(unrelated);

            for (name, alias) in [("auth.json", "AUTH.JSON"), ("settings.json", "SETTINGS.JSON"), ("caf\u{e9}.json", "cafe\u{301}.json")] {
                let original = storage.path(0, name).unwrap();
                if name != "auth.json" { storage.replace(&original, b"alias probe".to_vec(), None, &cancel).await.unwrap(); }
                let aliased = storage.path(0, alias).unwrap();
                let same = root_path.join(alias).exists() && same_file(&root_path.join(name), &root_path.join(alias));
                if name == "auth.json" {
                    assert!(matches!(storage.lock(&original, &cancel).await, Err(FileStorageError::Locked)));
                    if same { assert!(matches!(storage.lock(&aliased, &cancel).await, Err(FileStorageError::Locked))); }
                } else {
                    let first = storage.lock(&original, &cancel).await.unwrap();
                    let second = storage.lock(&aliased, &cancel).await;
                    if same { assert!(matches!(second, Err(FileStorageError::Locked))); } else { drop(second.unwrap()); }
                    drop(first);
                }
            }

            let child_root = root_path.clone();
            let (dead_pid, child) = tokio::task::spawn_blocking(move || {
                let child = Command::new(std::env::current_exe().unwrap())
                    .args(["--exact", "file_storage_flow", "--nocapture"])
                    .env(PROBE_ROOT, child_root)
                    .stdout(Stdio::piped()).stderr(Stdio::piped())
                    .spawn().unwrap();
                let pid = child.id();
                (pid, child.wait_with_output().unwrap())
            }).await.unwrap();
            assert!(child.status.success(), "{}{}", String::from_utf8_lossy(&child.stdout), String::from_utf8_lossy(&child.stderr));

            let mut bytes = lease.read(&cancel).await.unwrap();
            bytes.extend_from_slice(b"+second");
            lease.replace(bytes, None, &cancel).await.unwrap();
            assert_eq!(storage.read(&auth, &cancel).await.unwrap(), b"first+second");
            assert_permissions(&root_path);
            drop(lease);

            #[cfg(unix)]
            {
                let mode_path = storage.path(0, "mode.bin").unwrap();
                let file = root_path.join("mode.bin");
                for permissions in [None, Some(Permissions::from_mode(0o6755))] {
                    fs::write(&file, b"before").unwrap();
                    let initial_mode = if permissions.is_some() { 0o600 } else { 0o6755 };
                    fs::set_permissions(&file, Permissions::from_mode(initial_mode)).unwrap();
                    assert_eq!(fs::metadata(&file).unwrap().permissions().mode() & 0o7777, initial_mode, "fixture must permit setgid before replacement");
                    storage.replace(&mode_path, b"replacement".to_vec(), permissions, &cancel).await.unwrap();
                    assert_eq!(storage.read(&mode_path, &cancel).await.unwrap(), b"replacement");
                    assert_eq!(fs::metadata(&file).unwrap().permissions().mode() & 0o7777, 0o6755);
                }
            }

            fs::create_dir(root_path.join("fresh")).unwrap();
            let fresh_alias = storage.path(0, "fresh/AUTH.JSON").unwrap();
            let fresh = storage.lock(&fresh_alias, &cancel).await.unwrap();
            if fs::metadata(root_path.join("AUTH.JSON")).is_ok() {
                assert!(root_path.join("fresh/auth.json.lock").exists());
                assert!(matches!(storage.lock(&storage.path(0, "fresh/auth.json").unwrap(), &cancel).await, Err(FileStorageError::Locked)));
                assert!(matches!(storage.lock(&storage.path(0, "AUTH.JSON.LOCK").unwrap(), &cancel).await, Err(FileStorageError::InvalidPath(_))));
                let resources = storage.path(0, ".DIM-FILE-STORAGE-LOCKS/settings.json").unwrap();
                assert!(matches!(storage.read(&resources, &cancel).await, Err(FileStorageError::Io(_))));
                let missing_resource = storage.path(0, ".DIM-FILE-STORAGE-LOCKS/new-name").unwrap();
                assert!(matches!(storage.replace(&missing_resource, b"reserved".to_vec(), None, &cancel).await, Err(FileStorageError::Io(_))));
            }
            drop(fresh);

            let temporary_directory = root_path.join(".dim-file-storage-locks/.dim-file-storage-temps");
            let temporary_resource = temporary_directory.join(".dim-file-storage-temp-resource");
            fs::write(&temporary_resource, b"private temporary bytes").unwrap();
            for alias in [
                ".DIM-FILE-STORAGE-LOCKS/.DIM-FILE-STORAGE-TEMPS/.DIM-FILE-STORAGE-TEMP-RESOURCE",
                ".dim-file-\u{17f}torage-locks/.dim-file-\u{17f}torage-temps/.dim-file-\u{17f}torage-temp-resource",
                ".dim-file-storage-loc\u{212a}s/.dim-file-storage-temps/.dim-file-storage-temp-resource",
            ] {
                if root_path.join(alias).exists() {
                    assert!(same_file(&temporary_resource, &root_path.join(alias)));
                    match storage.path(0, alias) {
                        Err(FileStorageError::InvalidPath(_)) => {}
                        Ok(path) => {
                            assert!(matches!(storage.read(&path, &cancel).await, Err(FileStorageError::Io(_))));
                            assert!(matches!(storage.lock(&path, &cancel).await, Err(FileStorageError::Io(_))));
                            assert!(matches!(storage.replace(&path, b"overwrite".to_vec(), None, &cancel).await, Err(FileStorageError::Io(_))));
                        }
                        Err(error) => panic!("unexpected resource path error: {error}"),
                    }
                }
            }
            assert_eq!(fs::read(&temporary_resource).unwrap(), b"private temporary bytes");
            fs::remove_file(&temporary_resource).unwrap();

            fs::create_dir(root_path.join("fresh-resource-owner")).unwrap();
            let fresh_resource = storage.path(0, "fresh-resource-owner/.dim-file-\u{17f}torage-locks").unwrap();
            let fresh_lock = storage.lock(&fresh_resource, &cancel).await;
            if fs::metadata(root_path.join(".dim-file-\u{17f}torage-locks")).is_ok() {
                assert!(matches!(fresh_lock, Err(FileStorageError::InvalidPath(_))));
                assert!(matches!(storage.read(&fresh_resource, &cancel).await, Err(FileStorageError::Io(_))));
                assert!(matches!(storage.replace(&fresh_resource, b"overwrite".to_vec(), None, &cancel).await, Err(FileStorageError::Io(_))));
            } else {
                drop(fresh_lock.unwrap());
            }

            let account_lock = root_path.join("auth.json.lock");
            let old = SystemTime::now() - Duration::from_secs(60);
            let created_at = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap().as_millis();
            fs::write(&account_lock, format!("{}\n{created_at}\nexternal-live-owner\n", std::process::id())).unwrap();
            fs::OpenOptions::new().write(true).open(&account_lock).unwrap().set_times(FileTimes::new().set_modified(old)).unwrap();
            #[cfg(any(target_os = "linux", target_os = "android"))]
            let original_name = fs::read("/proc/self/comm").unwrap();
            #[cfg(any(target_os = "linux", target_os = "android"))]
            fs::write("/proc/self/comm", b"dim-\xff").unwrap();
            #[cfg(any(target_os = "linux", target_os = "android"))]
            assert!(String::from_utf8(fs::read("/proc/self/stat").unwrap()).is_err());
            let held = storage.lock(&auth, &cancel).await;
            #[cfg(any(target_os = "linux", target_os = "android"))]
            fs::write("/proc/self/comm", original_name.strip_suffix(b"\n").unwrap()).unwrap();
            assert!(matches!(held, Err(FileStorageError::Locked)));
            fs::remove_file(&account_lock).unwrap();
            fs::write(&account_lock, format!("{}\n1\nreused-pid-owner\n", std::process::id())).unwrap();
            fs::OpenOptions::new().write(true).open(&account_lock).unwrap().set_times(FileTimes::new().set_modified(old)).unwrap();
            let recovered = storage.lock(&auth, &cancel).await;
            if cfg!(any(target_os = "macos", target_os = "linux", target_os = "android", windows)) {
                drop(recovered.unwrap());
                assert!(!account_lock.exists());
            } else {
                assert!(matches!(recovered, Err(FileStorageError::Locked)));
                fs::remove_file(&account_lock).unwrap();
            }
            #[cfg(unix)]
            if unsafe { libc::kill(1, 0) } != 0
                && std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
            {
                fs::write(&account_lock, format!("1\n{created_at}\nforeign-owner\n")).unwrap();
                fs::OpenOptions::new().write(true).open(&account_lock).unwrap().set_times(FileTimes::new().set_modified(old)).unwrap();
                let recovered = storage.lock(&auth, &cancel).await.unwrap();
                drop(recovered);
                assert!(!account_lock.exists());
            }
            fs::write(&account_lock, format!("{dead_pid}\n1\nexternal-dead-owner\n")).unwrap();
            fs::OpenOptions::new().write(true).open(&account_lock).unwrap().set_times(FileTimes::new().set_modified(old)).unwrap();
            let recovered = storage.lock(&auth, &cancel).await.unwrap();
            let owner = fs::read_to_string(&account_lock).unwrap();
            let fields = owner.split('\n').collect::<Vec<_>>();
            assert_eq!(fields.len(), 4);
            assert_eq!(fields[0], std::process::id().to_string());
            assert!(fields[1].parse::<u128>().unwrap() >= created_at);
            uuid::Uuid::parse_str(fields[2]).unwrap();
            drop(recovered);
            assert!(!account_lock.exists());

            let displaced = storage.lock(&auth, &cancel).await.unwrap();
            let old_lock = root_path.join("retired-account-lock");
            fs::rename(&account_lock, &old_lock).unwrap();
            fs::write(&account_lock, b"new owner on a new inode").unwrap();
            drop(displaced);
            assert_eq!(fs::read(&account_lock).unwrap(), b"new owner on a new inode");
            fs::remove_file(&account_lock).unwrap();
            fs::remove_file(old_lock).unwrap();
            let displaced = storage.lock(&auth, &cancel).await.unwrap();
            fs::write(&account_lock, b"new owner on the same inode").unwrap();
            drop(displaced);
            assert_eq!(fs::read(&account_lock).unwrap(), b"new owner on the same inode");
            fs::remove_file(&account_lock).unwrap();

            let long_name = "x".repeat(255);
            let long = storage.path(0, &long_name).unwrap();
            storage.replace(&long, b"long filename".to_vec(), None, &cancel).await.unwrap();
            assert_eq!(storage.read(&long, &cancel).await.unwrap(), b"long filename");

            symlink(&outside, root_path.join("escape")).unwrap();
            let escape = storage.path(0, "escape").unwrap();
            assert!(matches!(storage.read(&escape, &cancel).await, Err(FileStorageError::Io(_))));
            assert!(matches!(storage.lock(&escape, &cancel).await, Err(FileStorageError::Io(_))));
            assert!(matches!(storage.replace(&escape, b"overwrite".to_vec(), None, &cancel).await, Err(FileStorageError::Io(_))));
            symlink(directory.0.join("outside"), root_path.join("linked-parent")).unwrap();
            let escape = storage.path(0, "linked-parent/secret").unwrap();
            assert!(matches!(storage.read(&escape, &cancel).await, Err(FileStorageError::Io(_))));
            assert!(matches!(storage.replace(&escape, b"overwrite".to_vec(), None, &cancel).await, Err(FileStorageError::Io(_))));
            assert_eq!(fs::read(&outside).unwrap(), b"outside");

            let rejected_root = Ctx::root().unwrap();
            let rejected = rejected_root.plugin(FileStoragePlugin::new(vec![root_path.join("linked-parent")]));
            let mut status = rejected.watch();
            while !matches!(status.borrow().state, FiberState::Failed | FiberState::Active) { status.changed().await.unwrap(); }
            assert_eq!(status.borrow().state, FiberState::Failed, "reparse or symlink root must be rejected");
            rejected_root.shutdown().await.unwrap();

            assert!(!account_lock.exists());
            symlink(&outside, root_path.join("auth.json.lock")).unwrap();
            assert!(matches!(storage.lock(&auth, &cancel).await, Err(FileStorageError::Io(_))));
            remove_link(&root_path.join("auth.json.lock"));
            fs::create_dir(root_path.join("linked-temps")).unwrap();
            fs::create_dir(root_path.join("linked-temps/.dim-file-storage-locks")).unwrap();
            symlink(directory.0.join("outside"), root_path.join("linked-temps/.dim-file-storage-locks/.dim-file-storage-temps")).unwrap();
            let linked_temps = storage.path(0, "linked-temps/secret").unwrap();
            assert!(matches!(storage.replace(&linked_temps, b"overwrite".to_vec(), None, &cancel).await, Err(FileStorageError::Io(_))));
            assert_eq!(fs::read(&outside).unwrap(), b"outside");
            storage.remove(&storage.path(0, "linked-temps").unwrap(), &cancel).await.unwrap();
            assert!(fs::symlink_metadata(root_path.join("linked-temps")).is_err());
            assert_eq!(fs::read(&outside).unwrap(), b"outside");
            let missing = storage.path(0, "missing").unwrap();
            assert!(matches!(storage.read(&missing, &cancel).await, Err(FileStorageError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound));
            fs::create_dir(root_path.join("directory-target")).unwrap();
            let invalid_target = storage.path(0, "directory-target").unwrap();
            assert!(matches!(storage.replace(&invalid_target, b"invalid".to_vec(), None, &cancel).await, Err(FileStorageError::Io(_))));

            #[cfg(windows)]
            windows_checks(&directory, &root_path, &storage, &cancel).await;

            let cancelled = CancellationToken::new();
            cancelled.cancel();
            assert!(matches!(storage.read(&auth, &cancelled).await, Err(FileStorageError::Cancelled)));
            assert!(matches!(storage.replace(&auth, b"cancelled".to_vec(), None, &cancelled).await, Err(FileStorageError::Cancelled)));
            assert_eq!(storage.read(&auth, &cancel).await.unwrap(), b"first+second");

            let mut retained = storage.lock(&auth, &cancel).await.unwrap();
            let (release, held) = std::sync::mpsc::channel();
            let (started, started_rx) = tokio::sync::oneshot::channel();
            let blocker = tokio::task::spawn_blocking(move || {
                started.send(()).unwrap();
                held.recv().unwrap();
            });
            started_rx.await.unwrap();
            let mut queued = Box::pin(retained.replace(b"abandoned".to_vec(), None, &cancel));
            assert!(futures_util::poll!(queued.as_mut()).is_pending());
            drop(queued);
            let closing = tokio::spawn(plugin.shutdown());
            while !matches!(storage.path(0, "probe"), Err(FileStorageError::Closed)) {
                tokio::task::yield_now().await;
            }
            assert!(!closing.is_finished(), "unload waits for the admitted worker even after its caller is dropped");
            release.send(()).unwrap();
            blocker.await.unwrap();
            closing.await.unwrap().unwrap();
            assert_eq!(fs::read(root_path.join("auth.json")).unwrap(), b"first+second");
            assert!(matches!(retained.read(&cancel).await, Err(FileStorageError::Closed)));
            assert!(matches!(storage.read(&auth, &cancel).await, Err(FileStorageError::Closed)));

            let (next_root, _, next) = mount(root_path.clone()).await;
            assert!(matches!(next.read(&auth, &cancel).await, Err(FileStorageError::InvalidPath(_))));
            let next_auth = next.path(0, "auth.json").unwrap();
            let new_lease = next.lock(&next_auth, &cancel).await.unwrap();
            drop(new_lease);
            next_root.shutdown().await.unwrap();
            root.shutdown().await.unwrap();
            assert!(fs::read_dir(&temporary_directory).unwrap().next().is_none());
            assert!(fs::read_dir(root_path.join("nested/.dim-file-storage-locks/.dim-file-storage-temps")).unwrap().next().is_none());

            let threaded_root = root_path.clone();
            std::thread::spawn(move || {
                let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(2).max_blocking_threads(2).enable_all().build().unwrap();
                runtime.block_on(async {
                    tokio::time::timeout(Duration::from_secs(10), async {
                        let (root, _, storage) = mount(threaded_root.clone()).await;
                        let cancel = CancellationToken::new();
                        let path = storage.path(0, "sequential.bin").unwrap();
                        storage.replace(&path, b"before".to_vec(), None, &cancel).await.unwrap();
                        let mut lease = storage.lock(&path, &cancel).await.unwrap();
                        let bytes = vec![0x5a; 4 * 1024 * 1024];
                        let mut writing = Box::pin(lease.replace(bytes.clone(), None, &cancel));
                        loop {
                            if let std::task::Poll::Ready(result) = futures_util::poll!(writing.as_mut()) { result.unwrap(); break; }
                            if fs::read_dir(threaded_root.join(".dim-file-storage-locks/.dim-file-storage-temps")).unwrap().next().is_some() { break; }
                            tokio::task::yield_now().await;
                        }
                        drop(writing);
                        assert_eq!(lease.read(&cancel).await.unwrap(), bytes);
                        root.shutdown().await.unwrap();
                        assert!(fs::read_dir(threaded_root.join(".dim-file-storage-locks/.dim-file-storage-temps")).unwrap().next().is_none());
                    }).await.expect("two-worker lease operations completed");
                });
            }).join().unwrap();
        }).await.expect("file storage public flow completed");
    });
}

#[cfg(unix)]
fn initial_permissions(_: &std::path::Path) -> Permissions {
    Permissions::from_mode(0o600)
}
#[cfg(windows)]
fn initial_permissions(root: &std::path::Path) -> Permissions {
    let path = root.join("auth.json");
    fs::write(&path, b"initial").unwrap();
    let mut permissions = fs::metadata(path).unwrap().permissions();
    permissions.set_readonly(true);
    permissions
}
#[cfg(unix)]
fn assert_permissions(root: &std::path::Path) {
    assert_eq!(
        fs::metadata(root.join("auth.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}
#[cfg(windows)]
#[allow(clippy::permissions_set_readonly_false)]
fn assert_permissions(root: &std::path::Path) {
    let path = root.join("auth.json");
    let mut permissions = fs::metadata(&path).unwrap().permissions();
    assert!(permissions.readonly());
    permissions.set_readonly(false);
    fs::set_permissions(path, permissions).unwrap();
}
#[cfg(unix)]
fn same_file(a: &std::path::Path, b: &std::path::Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    let (a, b) = (fs::metadata(a).unwrap(), fs::metadata(b).unwrap());
    (a.dev(), a.ino()) == (b.dev(), b.ino())
}
#[cfg(windows)]
fn same_file(a: &std::path::Path, b: &std::path::Path) -> bool {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ID_INFO, FileIdInfo, GetFileInformationByHandleEx,
    };
    let identity = |path: &std::path::Path| {
        let file = fs::File::open(path).unwrap();
        let mut id = FILE_ID_INFO::default();
        assert_ne!(
            unsafe {
                GetFileInformationByHandleEx(
                    file.as_raw_handle(),
                    FileIdInfo,
                    (&mut id as *mut FILE_ID_INFO).cast(),
                    std::mem::size_of::<FILE_ID_INFO>() as u32,
                )
            },
            0
        );
        (id.VolumeSerialNumber, id.FileId.Identifier)
    };
    identity(a) == identity(b)
}
#[cfg(unix)]
fn remove_link(path: &std::path::Path) {
    fs::remove_file(path).unwrap();
}
#[cfg(windows)]
fn remove_link(path: &std::path::Path) {
    fs::remove_dir(path).unwrap();
}
#[cfg(windows)]
fn symlink(
    source: impl AsRef<std::path::Path>,
    destination: impl AsRef<std::path::Path>,
) -> std::io::Result<()> {
    let source = source.as_ref();
    let source = if source.is_dir() {
        source
    } else {
        source.parent().unwrap()
    };
    let output = Command::new("cmd.exe")
        .args(["/D", "/C", "mklink", "/J"])
        .arg(destination.as_ref())
        .arg(source)
        .output()?;
    if output.status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(format!(
            "junction creation: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )))
    }
}
#[cfg(windows)]
fn acl(path: &std::path::Path, script: &str) -> String {
    let output = Command::new("powershell.exe")
        .args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command"])
        .arg(format!("$ErrorActionPreference = 'Stop'; {script}"))
        .env("DIM_FILE_STORAGE_ACL_PATH", path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "ACL command: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}
#[cfg(windows)]
fn acl_snapshot(path: &std::path::Path) -> String {
    let sddl = acl(
        path,
        r#"$a = Get-Acl -LiteralPath $env:DIM_FILE_STORAGE_ACL_PATH; $a.GetSecurityDescriptorSddlForm([System.Security.AccessControl.AccessControlSections]::Access)"#,
    );
    match sddl.split_once('(') {
        Some((head, aces)) => format!("{}({}", head.replace("AI", ""), aces),
        None => sddl,
    }
}
#[cfg(windows)]
async fn windows_checks(
    directory: &Directory,
    root_path: &std::path::Path,
    storage: &Arc<FileStorage>,
    cancel: &CancellationToken,
) {
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use windows_sys::Win32::Storage::FileSystem::GetShortPathNameW;
    for path in [
        "C:\\escape",
        "\\\\server\\share\\escape",
        "name:stream",
        "NUL.txt",
        "COM1",
        "LPT\u{b9}.log",
        "trailing.",
        "trailing ",
        "a?b",
        "a\"b",
    ] {
        assert!(
            matches!(storage.path(0, path), Err(FileStorageError::InvalidPath(_))),
            "{path}"
        );
    }
    let name = "Long file name for storage aliases.json";
    let path = storage.path(0, name).unwrap();
    storage
        .replace(&path, b"long target".to_vec(), None, cancel)
        .await
        .unwrap();
    let mut wide: Vec<u16> = root_path
        .join(name)
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    let length = unsafe { GetShortPathNameW(wide.as_ptr(), std::ptr::null_mut(), 0) };
    assert_ne!(
        length,
        0,
        "short path query failed: {}",
        std::io::Error::last_os_error()
    );
    let mut short = vec![0; length as usize];
    let length =
        unsafe { GetShortPathNameW(wide.as_mut_ptr(), short.as_mut_ptr(), short.len() as u32) };
    assert_ne!(length, 0);
    let short = std::ffi::OsString::from_wide(&short[..length as usize]);
    let short_name = std::path::Path::new(&short).file_name().unwrap();
    if short_name != std::ffi::OsStr::new(name) {
        let short_path = storage.path(0, short_name).unwrap();
        let mut lease = storage.lock(&short_path, cancel).await.unwrap();
        assert!(matches!(
            storage.lock(&path, cancel).await,
            Err(FileStorageError::Locked)
        ));
        lease
            .replace(b"short alias replacement".to_vec(), None, cancel)
            .await
            .unwrap();
        assert_eq!(
            lease.read(cancel).await.unwrap(),
            b"short alias replacement"
        );
        assert_eq!(
            storage.read(&path, cancel).await.unwrap(),
            b"short alias replacement"
        );
        drop(lease);
    } else {
        eprintln!("8.3 alias check skipped: this volume does not create short names");
    }
    let private = root_path.join("private.json");
    fs::write(&private, b"private").unwrap();
    acl(
        &private,
        r#"$a = Get-Acl -LiteralPath $env:DIM_FILE_STORAGE_ACL_PATH; $a.SetAccessRuleProtection($true, $false); $a.AddAccessRule([System.Security.AccessControl.FileSystemAccessRule]::new([System.Security.Principal.WindowsIdentity]::GetCurrent().User, 'FullControl', 'Allow')); Set-Acl -LiteralPath $env:DIM_FILE_STORAGE_ACL_PATH -AclObject $a"#,
    );
    let expected = acl_snapshot(&private);
    storage
        .replace(
            &storage.path(0, "private.json").unwrap(),
            b"private replacement".to_vec(),
            None,
            cancel,
        )
        .await
        .unwrap();
    assert_eq!(
        acl_snapshot(&private),
        expected,
        "protected target DACL changed"
    );

    let business = root_path.join("acl-parent");
    fs::create_dir(&business).unwrap();
    acl(
        &business,
        r#"$a = Get-Acl -LiteralPath $env:DIM_FILE_STORAGE_ACL_PATH; $a.SetAccessRuleProtection($true, $false); $a.AddAccessRule([System.Security.AccessControl.FileSystemAccessRule]::new([System.Security.Principal.WindowsIdentity]::GetCurrent().User, 'FullControl', 'ContainerInherit,ObjectInherit', 'None', 'Allow')); Set-Acl -LiteralPath $env:DIM_FILE_STORAGE_ACL_PATH -AclObject $a"#,
    );
    fs::write(business.join("expected.json"), b"inherited").unwrap();
    let expected = acl_snapshot(&business.join("expected.json"));
    let inherited = storage.path(0, "acl-parent/inherited.json").unwrap();
    storage
        .replace(&inherited, b"new inherited".to_vec(), None, cancel)
        .await
        .unwrap();
    assert_eq!(
        acl_snapshot(&business.join("inherited.json")),
        expected,
        "new file did not inherit business parent DACL"
    );
    storage
        .replace(&inherited, b"existing inherited".to_vec(), None, cancel)
        .await
        .unwrap();
    assert_eq!(
        acl_snapshot(&business.join("inherited.json")),
        expected,
        "existing inherited target DACL changed"
    );
    assert!(
        fs::read_dir(business.join(".dim-file-storage-locks/.dim-file-storage-temps"))
            .unwrap()
            .next()
            .is_none()
    );

    assert_eq!(
        fs::read(directory.0.join("outside/secret")).unwrap(),
        b"outside"
    );
}

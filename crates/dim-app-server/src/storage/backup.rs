use super::{STORE_FORMAT, sqlite::error};
use dim_agent_runtime::Result;
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    version: u32,
    store_format: u32,
    database: DigestEntry,
    files: Vec<FileEntry>,
}
#[derive(Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct DigestEntry {
    bytes: u64,
    sha256: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileEntry {
    id: String,
    name: String,
    digest: DigestEntry,
}

fn open(path: &Path) -> Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path).map_err(error)?;
    if !file.metadata().map_err(error)?.is_file() {
        return Err(error("Backup source is not a regular file"));
    }
    Ok(file)
}
fn copy(source: &Path, target: &Path) -> Result<DigestEntry> {
    let mut source = open(source)?;
    let mut target = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(target)
        .map_err(error)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        target
            .set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(error)?;
    }
    let mut bytes = 0u64;
    let mut hash = Sha256::new();
    let mut buffer = [0; 64 * 1024];
    loop {
        let count = source.read(&mut buffer).map_err(error)?;
        if count == 0 {
            break;
        }
        target.write_all(&buffer[..count]).map_err(error)?;
        hash.update(&buffer[..count]);
        bytes += count as u64;
    }
    target.sync_all().map_err(error)?;
    Ok(DigestEntry {
        bytes,
        sha256: format!("{:x}", hash.finalize()),
    })
}
fn directory(path: &Path) -> Result<()> {
    if !fs::symlink_metadata(path).map_err(error)?.is_dir() {
        return Err(error("Backup file directory must not be a symlink"));
    }
    Ok(())
}

fn file_root(database: &Path) -> PathBuf {
    let mut root = database.as_os_str().to_os_string();
    root.push(".session-files");
    root.into()
}
fn owned(connection: &Connection) -> Result<BTreeSet<String>> {
    let mut query = connection.prepare("SELECT DISTINCT f.file_id, f.node, f.writer FROM session_files f JOIN session_file_owners o ON o.file_id=f.file_id").map_err(error)?;
    let mut rows = query.query([]).map_err(error)?;
    let mut result = BTreeSet::new();
    while let Some(row) = rows.next().map_err(error)? {
        let id: String = row.get(0).map_err(error)?;
        uuid::Uuid::parse_str(&id).map_err(error)?;
        if row.get::<_, Option<String>>(1).map_err(error)?.is_some()
            || row.get::<_, Option<String>>(2).map_err(error)?.is_some()
        {
            return Err(error(
                "Backup requires committed files on the local resource node",
            ));
        }
        result.insert(id);
    }
    Ok(result)
}
fn validate(connection: &Connection) -> Result<()> {
    super::sqlite::validate_backup_schema(connection)?;
    let integrity: String = connection
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .map_err(error)?;
    if integrity != "ok" {
        return Err(error(integrity));
    }
    let mut statement = connection
        .prepare("PRAGMA foreign_key_check")
        .map_err(error)?;
    if statement
        .query([])
        .map_err(error)?
        .next()
        .map_err(error)?
        .is_some()
    {
        return Err(error(
            "Backup database has broken ownership or Timeline references",
        ));
    }
    Ok(())
}
fn stage(target: &Path) -> Result<tempfile::TempDir> {
    if target.exists() {
        return Err(error("Backup or restore destination already exists"));
    }
    tempfile::Builder::new()
        .prefix(".dim-archive-")
        .tempdir_in(
            target
                .parent()
                .filter(|path| !path.as_os_str().is_empty())
                .unwrap_or(Path::new(".")),
        )
        .map_err(error)
}
fn publish(staging: tempfile::TempDir, target: &Path) -> Result<()> {
    if target.exists() {
        return Err(error("Backup or restore destination already exists"));
    }
    fs::rename(staging.path(), target).map_err(error)?;
    #[cfg(unix)]
    {
        fs::File::open(
            target
                .parent()
                .filter(|path| !path.as_os_str().is_empty())
                .unwrap_or(Path::new(".")),
        )
        .and_then(|dir| dir.sync_all())
        .map_err(error)?;
    }
    Ok(())
}

/// Exports a closed native store and every owned Session file into a new archive directory.
/// Refuses a running owner, a missing owned file, or an existing destination.
pub fn backup_store(database: PathBuf, destination: PathBuf) -> Result<()> {
    let database = database.canonicalize().map_err(error)?;
    let mut lock = database.as_os_str().to_os_string();
    lock.push(".lock");
    let lease = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(lock)
        .map_err(error)?;
    lease.try_lock().map_err(error)?;
    let connection = Connection::open(&database).map_err(error)?;
    validate(&connection)?;
    let staging = stage(&destination)?;
    let snapshot = staging.path().join("snapshot.sqlite");
    connection
        .execute(
            "VACUUM INTO ?1",
            [snapshot
                .to_str()
                .ok_or_else(|| error("Backup path must be UTF-8"))?],
        )
        .map_err(error)?;
    let snapshot_digest = copy(&snapshot, &staging.path().join("database.sqlite"))?;
    fs::remove_file(snapshot).map_err(error)?;
    let mut files = Vec::new();
    for id in owned(&connection)? {
        let directory = file_root(&database).join(&id);
        self::directory(&file_root(&database))?;
        self::directory(&directory)?;
        let entries = fs::read_dir(&directory)
            .map_err(error)?
            .collect::<std::io::Result<Vec<_>>>()
            .map_err(error)?;
        let entries: Vec<_> = entries
            .into_iter()
            .filter(|entry| {
                !entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".dim-file-storage-")
            })
            .collect();
        if entries.len() != 1 {
            return Err(error(
                "Owned Session file directory is missing or ambiguous",
            ));
        }
        let entry = &entries[0];
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| error("Session file name is not UTF-8"))?;
        let target = staging.path().join("files").join(&id);
        crate::create_private_directory(&target).map_err(error)?;
        let digest = copy(&entry.path(), &target.join(&name))?;
        files.push(FileEntry { id, name, digest });
    }
    let manifest = Manifest {
        version: 1,
        store_format: STORE_FORMAT,
        database: snapshot_digest,
        files,
    };
    let mut file = fs::File::create(staging.path().join("manifest.json")).map_err(error)?;
    serde_json::to_writer(&mut file, &manifest).map_err(error)?;
    file.sync_all().map_err(error)?;
    publish(staging, &destination)
}

/// Validates an archive and atomically publishes a new native store directory.
/// The destination must not exist; its database is named `dimcode.sqlite`.
pub fn restore_store(archive: PathBuf, destination: PathBuf) -> Result<()> {
    let manifest: Manifest =
        serde_json::from_reader(open(&archive.join("manifest.json"))?.take(16 * 1024 * 1024))
            .map_err(error)?;
    if manifest.version != 1 || manifest.store_format != STORE_FORMAT {
        return Err(error("Unsupported file backup format"));
    }
    let staging = stage(&destination)?;
    let database = staging.path().join("dimcode.sqlite");
    if copy(&archive.join("database.sqlite"), &database)? != manifest.database {
        return Err(error("Backup database checksum mismatch"));
    }
    let connection =
        Connection::open_with_flags(&database, OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(error)?;
    validate(&connection)?;
    let mut remaining = owned(&connection)?;
    for entry in manifest.files {
        if !remaining.remove(&entry.id) || !super::files::valid_name(&entry.name) {
            return Err(error(
                "Backup contains an invalid or duplicate file identity",
            ));
        }
        let target = file_root(&database).join(&entry.id);
        crate::create_private_directory(&target).map_err(error)?;
        directory(&archive.join("files"))?;
        directory(&archive.join("files").join(&entry.id))?;
        let source = archive.join("files").join(&entry.id).join(&entry.name);
        if copy(&source, &target.join(&entry.name))? != entry.digest {
            return Err(error("Backup Session file checksum mismatch"));
        }
        let mut permissions = fs::metadata(target.join(&entry.name))
            .map_err(error)?
            .permissions();
        permissions.set_readonly(true);
        fs::set_permissions(target.join(&entry.name), permissions).map_err(error)?;
    }
    if !remaining.is_empty() {
        return Err(error("Backup omits owned Session files"));
    }
    drop(connection);
    publish(staging, &destination)
}

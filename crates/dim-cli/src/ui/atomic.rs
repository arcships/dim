//! Replacing a file whole, under a cross-process lock.

#[cfg(unix)]
use std::fs::File;
use std::fs::OpenOptions;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

/// Replacing a file whole, under a cross-process lock.
///
/// Writers queue on an exclusive lock over `<path>.lock`, beside the path the
/// caller names, so two processes that share a home keep each other's
/// changes. A writer waits for the lock while another one holds it and writes
/// once it is released, so contention is never a failed write; a lock that
/// cannot be taken at all is reported with its error, and the write is not
/// attempted without one, because a home whose filesystem refuses locks
/// cannot hold the `dimcode.sqlite` beside the file either.
///
/// The file is resolved first: a `path` that is a symbolic link is followed,
/// and the temp file, the rename and the directory sync go beside the
/// resolved file, so the link stays a link and its target takes the write,
/// as `std::fs::write` did before. A path that does not exist yet is used as
/// given; any other resolution error is returned and the file is left alone.
/// The lock still sits beside the named path, not beside the resolved file,
/// so a linked `config.json` leaves no lock in the user's linked directory.
/// The new bytes go to a sibling temp file that is synced and renamed over
/// the resolved file, so a reader, which takes no lock, sees either the old
/// file or the new one.
///
/// On Unix `mode`, when given, is set on every write; without one an
/// existing file keeps its mode, read through the link. A failure before the
/// rename leaves the file as it was and removes the temp file, a failure
/// after it reports that the file was replaced but that its directory could
/// not be synced, and a file that cannot be read is left alone rather than
/// treated as an absent one.
///
/// The `.lock` file stays after the write: removing it would let two
/// writers lock two different files. The temp file is
/// `<resolved>.<pid>.tmp`; a stale one is removed first and the new one is
/// opened with `create_new`, so a planted symlink at the path is not
/// followed.
pub fn update(
    path: &Path,
    mode: Option<u32>,
    update: impl FnOnce(Option<&[u8]>) -> io::Result<Option<Vec<u8>>>,
) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let lock = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(sibling(path, ".lock"))?;
    lock.lock().map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("cannot lock {}: {error}", path.display()),
        )
    })?;
    let file = resolve(path)?;
    let current = match std::fs::read(&file) {
        Ok(bytes) => Some(bytes),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    let Some(bytes) = update(current.as_deref())? else {
        return Ok(());
    };
    let temp = sibling(&file, &format!(".{}.tmp", std::process::id()));
    let _ = std::fs::remove_file(&temp);
    if let Err(error) =
        write_temp(&temp, &file, mode, &bytes).and_then(|()| std::fs::rename(&temp, &file))
    {
        let _ = std::fs::remove_file(&temp);
        return Err(error);
    }
    sync_dir(&file).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!(
                "{} was replaced but its directory could not be synced: {error}",
                file.display()
            ),
        )
    })
}

/// The file a write replaces: the target when `path` is a link, and `path`
/// itself when it does not exist yet.
fn resolve(path: &Path) -> io::Result<PathBuf> {
    match std::fs::canonicalize(path) {
        Ok(file) => Ok(file),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(path.to_owned()),
        Err(error) => Err(error),
    }
}

fn write_temp(temp: &Path, path: &Path, mode: Option<u32>, bytes: &[u8]) -> io::Result<()> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(temp)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = mode.or_else(|| {
            std::fs::metadata(path)
                .ok()
                .map(|meta| meta.permissions().mode() & 0o777)
        });
        if let Some(mode) = mode {
            file.set_permissions(std::fs::Permissions::from_mode(mode))?;
        }
    }
    #[cfg(not(unix))]
    let _ = (path, mode);
    file.write_all(bytes)?;
    file.sync_all()
}

/// A rename is durable once its directory is synced.
fn sync_dir(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        let dir = path.parent().unwrap_or_else(|| Path::new("."));
        File::open(dir)?.sync_all()?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// A writer waits while another holds the lock, then writes, instead of
    /// reporting contention as a failed write.
    #[test]
    fn a_write_waits_for_a_held_lock_and_then_writes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(&path, b"{}\n").unwrap();
        let held = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(sibling(&path, ".lock"))
            .unwrap();
        held.lock().unwrap();
        let (wrote, written) = std::sync::mpsc::channel();
        std::thread::scope(|scope| {
            let waiter = path.clone();
            scope.spawn(move || {
                update(&waiter, None, |current| {
                    Ok(Some(current.unwrap_or_default().to_vec()))
                })
                .unwrap();
                wrote.send(()).unwrap();
            });
            assert!(
                written.recv_timeout(Duration::from_millis(100)).is_err(),
                "a writer must wait for a lock another one holds"
            );
            drop(held);
        });
        written
            .recv_timeout(Duration::from_secs(60))
            .expect("the writer must write once the lock is released");
        assert_eq!(std::fs::read(&path).unwrap(), b"{}\n");
    }
}

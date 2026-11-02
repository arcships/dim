//! A Skill installation's selection, revisions and the files it owns under
//! the user root ([docs/design/plugins/Skills.md]).
use super::SkillsError;
use sha2::{Digest, Sha256};
use std::{
    io::Read,
    path::{Component, Path, PathBuf},
};

/// The largest file a revision digests. A Skill bundles text; a larger file is
/// refused instead of being read, so previewing a source cannot allocate it.
const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;
const CHUNK_BYTES: usize = 64 * 1024;

/// The directories and files of a Skill, in sorted order, with `.git`
/// skipped. Symbolic links are never followed or installed.
pub(crate) fn revision(skill_dir: &Path) -> Result<String, SkillsError> {
    let mut hasher = Sha256::new();
    walk(skill_dir, skill_dir, &mut hasher)?;
    Ok(version(hasher.finalize()))
}

fn walk(root: &Path, dir: &Path, hasher: &mut Sha256) -> Result<(), SkillsError> {
    let mut names: Vec<_> = std::fs::read_dir(dir)
        .map_err(|error| unusable(dir, error))?
        .filter_map(|entry| entry.ok().map(|entry| entry.file_name()))
        .collect();
    names.sort();
    for name in names {
        if name == ".git" {
            continue;
        }
        let path = dir.join(&name);
        let meta = std::fs::symlink_metadata(&path).map_err(|error| unusable(&path, error))?;
        if meta.file_type().is_symlink() {
            return Err(SkillsError::SelectionInvalid(format!(
                "{}: a symbolic link is not installed",
                path.display()
            )));
        }
        let relative = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        if meta.is_dir() {
            hasher.update(b"d");
            hasher.update(relative.as_bytes());
            hasher.update([0]);
            walk(root, &path, hasher)?;
        } else if meta.is_file() {
            hasher.update(b"f");
            hasher.update(relative.as_bytes());
            hasher.update([0]);
            hash_file(&path, hasher)?;
        } else {
            return Err(SkillsError::SelectionInvalid(format!(
                "{}: only regular files are installed",
                path.display()
            )));
        }
    }
    Ok(())
}

/// Feeds `path` into `hasher` a chunk at a time, so memory does not follow the
/// file's size.
fn hash_file(path: &Path, hasher: &mut Sha256) -> Result<(), SkillsError> {
    let mut file = std::fs::File::open(path).map_err(|error| unusable(path, error))?;
    let mut buffer = vec![0u8; CHUNK_BYTES];
    let mut total = 0u64;
    loop {
        let read = match file.read(&mut buffer) {
            Ok(0) => return Ok(()),
            Ok(read) => read,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(unusable(path, error)),
        };
        total += read as u64;
        if total > MAX_FILE_BYTES {
            return Err(SkillsError::SelectionInvalid(format!(
                "{}: a file larger than {MAX_FILE_BYTES} bytes is not digested",
                path.display()
            )));
        }
        hasher.update(&buffer[..read]);
    }
}

/// The revision of one installation: its selected paths with their revisions,
/// in install order.
pub(crate) fn selection_revision(skills: &[(String, String)]) -> String {
    let mut hasher = Sha256::new();
    for (path, revision) in skills {
        hasher.update(path.as_bytes());
        hasher.update([0]);
        hasher.update(revision.as_bytes());
        hasher.update([0]);
    }
    version(hasher.finalize())
}

fn version(digest: impl AsRef<[u8]>) -> String {
    let digest = digest.as_ref();
    let mut version = String::with_capacity(7 + digest.len() * 2);
    version.push_str("sha256:");
    for byte in digest {
        version.push_str(&format!("{byte:02x}"));
    }
    version
}

/// `[a-z0-9_-]{1,64}`: an installation id is also a directory name.
pub(crate) fn valid_id(id: &str) -> bool {
    (1..=64).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

/// A selection or record path: relative, no `.` or `..` component, not empty.
pub fn selection_path(path: &str) -> Option<PathBuf> {
    let path = Path::new(path);
    if path.as_os_str().is_empty() || path.is_absolute() || path.has_root() {
        return None;
    }
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => out.push(part),
            _ => return None,
        }
    }
    (!out.as_os_str().is_empty()).then_some(out)
}

/// Copies one Skill directory tree, refusing anything that is not a directory
/// or a regular file.
pub(crate) fn copy_tree(from: &Path, to: &Path) -> Result<(), SkillsError> {
    std::fs::create_dir_all(to).map_err(|error| unusable(to, error))?;
    let mut names: Vec<_> = std::fs::read_dir(from)
        .map_err(|error| unusable(from, error))?
        .filter_map(|entry| entry.ok().map(|entry| entry.file_name()))
        .collect();
    names.sort();
    for name in names {
        let source = from.join(&name);
        let target = to.join(&name);
        let meta = std::fs::symlink_metadata(&source).map_err(|error| unusable(&source, error))?;
        if meta.is_dir() {
            copy_tree(&source, &target)?;
        } else if meta.is_file() {
            std::fs::copy(&source, &target).map_err(|error| unusable(&target, error))?;
        } else {
            return Err(SkillsError::SelectionInvalid(format!(
                "{}: only regular files are installed",
                source.display()
            )));
        }
    }
    Ok(())
}

/// The installation directory this operation created, and what it replaced.
pub(crate) struct Placed {
    pub(crate) dest: PathBuf,
    old: Option<PathBuf>,
}

impl Placed {
    /// Undoes the placement: the new directory is deleted and the replaced one
    /// comes back.
    pub(crate) fn revert(self) {
        let _ = std::fs::remove_dir_all(&self.dest);
        if let Some(old) = self.old {
            let _ = std::fs::rename(old, &self.dest);
        }
    }
}

/// Copies `skills` (source directory and source-relative path) into
/// `<operation_dir>/place` and renames it to `<user_root>/<id>`. With
/// `replace`, the directory there is kept under `<operation_dir>/old` until the
/// operation settles; the caller sets it only for a directory the record owns.
pub(crate) fn place(
    user_root: &Path,
    id: &str,
    operation_dir: &Path,
    skills: &[(PathBuf, String)],
    replace: bool,
) -> Result<Placed, SkillsError> {
    let place = operation_dir.join("place");
    std::fs::create_dir_all(&place).map_err(|error| unusable(&place, error))?;
    for (from, path) in skills {
        let relative = selection_path(path)
            .ok_or_else(|| SkillsError::SelectionInvalid(format!("{path:?} is not relative")))?;
        copy_tree(from, &place.join(relative))?;
    }
    let dest = user_root.join(id);
    std::fs::create_dir_all(user_root).map_err(|error| unusable(user_root, error))?;
    let old = match std::fs::symlink_metadata(&dest) {
        Ok(meta) if replace && meta.is_dir() && !meta.file_type().is_symlink() => {
            let old = operation_dir.join("old");
            std::fs::rename(&dest, &old).map_err(|error| unusable(&old, error))?;
            Some(old)
        }
        Ok(_) => {
            return Err(SkillsError::InstallConflict(format!(
                "{} is not a Skill installation directory",
                dest.display()
            )));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(unusable(&dest, error)),
    };
    if let Err(error) = std::fs::rename(&place, &dest) {
        if let Some(old) = &old {
            let _ = std::fs::rename(old, &dest);
        }
        return Err(unusable(&dest, error));
    }
    Ok(Placed { dest, old })
}

/// Whether anything, of any kind, is at `<user_root>/<id>`.
pub(crate) fn occupied(user_root: &Path, id: &str) -> Result<bool, SkillsError> {
    let dest = user_root.join(id);
    match std::fs::symlink_metadata(&dest) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(unusable(&dest, error)),
    }
}

/// Deletes `<user_root>/<id>`, and nothing else. `Ok(false)` when it is not
/// there.
pub(crate) fn remove(user_root: &Path, id: &str) -> Result<bool, SkillsError> {
    if !valid_id(id) {
        return Err(SkillsError::RemoveFailed(format!(
            "{id:?} is not an installation id"
        )));
    }
    if !real_dir(user_root)? {
        return Err(SkillsError::RemoveFailed(format!(
            "{} is not the Skill root",
            user_root.display()
        )));
    }
    let dest = user_root.join(id);
    let existed = std::fs::symlink_metadata(&dest).is_ok();
    if !real_dir(&dest)? {
        return if existed {
            Err(SkillsError::RemoveFailed(format!(
                "{} is not a Skill installation directory",
                dest.display()
            )))
        } else {
            Ok(false)
        };
    }
    std::fs::remove_dir_all(&dest)
        .map_err(|error| SkillsError::RemoveFailed(format!("{}: {error}", dest.display())))?;
    Ok(true)
}

/// Whether `path` is a directory that is not a symbolic link; `false` when it
/// does not exist.
fn real_dir(path: &Path) -> Result<bool, SkillsError> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) => Ok(meta.is_dir() && !meta.file_type().is_symlink()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(unusable(path, error)),
    }
}

fn unusable(path: &Path, error: std::io::Error) -> SkillsError {
    SkillsError::InstallIo(format!("{}: {error}", path.display()))
}

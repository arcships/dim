//! Acquiring a plugin directory (PluginManager §7): ids, the one path rule
//! under `<home>/plugins`, and Git clones.
use super::PluginError;
use super::record::Manifest;
use std::path::{Path, PathBuf};
use tokio::process::Command;

/// `[a-z0-9_-]{1,64}`, not `builtin` or `default`.
pub fn check_id(id: &str) -> Result<(), PluginError> {
    let valid = (1..=64).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
        && id != "builtin"
        && id != "default";
    if valid {
        Ok(())
    } else {
        Err(PluginError::new(
            "invalid_id",
            format!("{id:?} must match [a-z0-9_-]{{1,64}} and not be builtin or default"),
        ))
    }
}

/// The directories PluginManager may create, rename or delete. Every one is
/// checked to be exactly `<home>/plugins/.staging/<random>`,
/// `<home>/plugins/<id>` or `<home>/plugins/<id>/<commit>` before use.
#[derive(Clone, Debug)]
pub struct PluginsDir {
    root: PathBuf,
}

impl PluginsDir {
    /// `home` is canonical.
    pub fn new(home: &Path) -> Self {
        Self {
            root: home.join("plugins"),
        }
    }

    fn staging_root(&self) -> PathBuf {
        self.root.join(".staging")
    }

    pub fn id_dir(&self, id: &str) -> Result<PathBuf, PluginError> {
        check_id(id)?;
        Ok(self.root.join(id))
    }

    pub fn clone_dir(&self, id: &str, commit: &str) -> Result<PathBuf, PluginError> {
        if commit.len() < 7 || !commit.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(outside(commit));
        }
        Ok(self.id_dir(id)?.join(commit))
    }

    /// Whether `dir` is one of the shapes PluginManager owns, read lexically
    /// and then after resolving what exists of it.
    pub fn check(&self, dir: &Path) -> Result<(), PluginError> {
        let relative = dir.strip_prefix(&self.root).map_err(|_| outside(dir))?;
        let parts: Vec<_> = relative
            .components()
            .map(|component| match component {
                std::path::Component::Normal(part) => part.to_str().ok_or_else(|| outside(dir)),
                _ => Err(outside(dir)),
            })
            .collect::<Result<_, _>>()?;
        let shaped = match parts.as_slice() {
            [".staging", random] => !random.is_empty(),
            [".python"] => true,
            [id] => check_id(id).is_ok(),
            [id, commit] => self.clone_dir(id, commit).is_ok(),
            _ => false,
        };
        if !shaped {
            return Err(outside(dir));
        }
        self.real_dirs(dir)
    }

    /// Every component from the root down to `dir` that exists is a
    /// directory and not a symbolic link, so nothing created or deleted at
    /// `dir` lands outside the root. The root's parent, the home, is
    /// canonical.
    fn real_dirs(&self, dir: &Path) -> Result<(), PluginError> {
        let relative = dir.strip_prefix(&self.root).map_err(|_| outside(dir))?;
        let mut path = self.root.clone();
        for part in std::iter::once(None).chain(relative.components().map(Some)) {
            if let Some(part) = part {
                path.push(part);
            }
            match std::fs::symlink_metadata(&path) {
                Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => {}
                Ok(_) => return Err(outside(&path)),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
                Err(error) => return Err(acquire_failed(error)),
            }
        }
        Ok(())
    }

    /// Creates `dir` one component at a time from the root, each a new or an
    /// existing real directory; `dir` is checked first.
    fn create(&self, dir: &Path) -> Result<(), PluginError> {
        self.check(dir)?;
        let relative = dir.strip_prefix(&self.root).map_err(|_| outside(dir))?;
        let mut path = self.root.clone();
        for part in std::iter::once(None).chain(relative.components().map(Some)) {
            if let Some(part) = part {
                path.push(part);
            }
            match std::fs::create_dir(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    match std::fs::symlink_metadata(&path) {
                        Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => {}
                        _ => return Err(outside(&path)),
                    }
                }
                Err(error) => return Err(acquire_failed(error)),
            }
        }
        Ok(())
    }

    /// Links `<root>/.python/<package>` to the Python plugin directory
    /// `target`, the import path of the Python runtime. An existing link to
    /// another directory is replaced; anything else there is refused.
    pub fn python_link(&self, package: &str, target: &Path) -> Result<(), PluginError> {
        let python = self.python_dir(package)?;
        let link = python.join(package);
        self.create(&python)?;
        match std::fs::symlink_metadata(&link) {
            Ok(meta) if meta.file_type().is_symlink() => {
                if std::fs::read_link(&link).is_ok_and(|current| current == target) {
                    return Ok(());
                }
                remove_link(&link).map_err(acquire_failed)?;
            }
            Ok(_) => return Err(outside(&link)),
            Err(_) => {}
        }
        #[cfg(unix)]
        let made = std::os::unix::fs::symlink(target, &link);
        #[cfg(windows)]
        let made = std::os::windows::fs::symlink_dir(target, &link);
        made.map_err(acquire_failed)
    }

    /// Removes the link of a removed Python plugin, never what it points to.
    pub fn remove_python_link(&self, package: &str) {
        let Ok(python) = self.python_dir(package) else {
            return;
        };
        let link = python.join(package);
        if std::fs::symlink_metadata(&link).is_ok_and(|meta| meta.file_type().is_symlink()) {
            let _ = remove_link(&link);
        }
    }

    /// `<root>/.python`, checked, for a valid package name. The link itself
    /// is not resolved: it points outside by design.
    fn python_dir(&self, package: &str) -> Result<PathBuf, PluginError> {
        if !python_package(package) {
            return Err(outside(package));
        }
        let python = self.root.join(".python");
        self.check(&python)?;
        Ok(python)
    }

    /// A new, empty staging directory.
    pub fn stage(&self) -> Result<Staging, PluginError> {
        let dir = self
            .staging_root()
            .join(uuid::Uuid::new_v4().simple().to_string());
        self.create(&dir)?;
        Ok(Staging(Some(dir)))
    }

    /// Deletes `dir` after checking it.
    pub fn delete(&self, dir: &Path) -> Result<(), PluginError> {
        self.check(dir)?;
        match std::fs::remove_dir_all(dir) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                Err(acquire_failed(error))
            }
            _ => Ok(()),
        }
    }

    /// Startup cleanup: staging leftovers and Git clones no entry refers
    /// to. `installed` are the directories of every installed entry: an id
    /// directory holding one that is not a clone (a local plugin kept under
    /// `<home>/plugins`) is left whole, and only clone-shaped directories
    /// are ever deleted, the id directory only once it is empty.
    pub fn clean(&self, installed: &[PathBuf]) {
        if self.real_dirs(&self.staging_root()).is_err() {
            return;
        }
        let _ = std::fs::remove_dir_all(self.staging_root());
        let Ok(ids) = std::fs::read_dir(&self.root) else {
            return;
        };
        for id in ids.flatten() {
            let id_dir = id.path();
            if id.file_name().to_string_lossy().starts_with('.') || self.check(&id_dir).is_err() {
                continue;
            }
            let local = installed.iter().any(|dir| {
                dir.starts_with(&id_dir) && dir.parent() != Some(id_dir.as_path()) || dir == &id_dir
            });
            if local {
                continue;
            }
            if let Ok(commits) = std::fs::read_dir(&id_dir) {
                for commit in commits.flatten() {
                    let dir = commit.path();
                    if !installed.contains(&dir) && self.check(&dir).is_ok() {
                        let _ = self.delete(&dir);
                    }
                }
            }
            let _ = std::fs::remove_dir(&id_dir);
        }
    }
}

/// A staging directory, deleted on drop.
pub struct Staging(Option<PathBuf>);

impl Staging {
    /// An empty operation directory under `root`, created when missing.
    pub fn in_root(root: &Path) -> Result<Self, PluginError> {
        std::fs::create_dir_all(root).map_err(acquire_failed)?;
        let dir = root.join(uuid::Uuid::new_v4().simple().to_string());
        std::fs::create_dir(&dir).map_err(acquire_failed)?;
        Ok(Staging(Some(dir)))
    }

    pub fn path(&self) -> &Path {
        self.0.as_deref().expect("staging directory")
    }

    /// Renames the clone's work tree to `target`, which `dirs` checks; the
    /// rest of the staging directory is deleted on drop.
    pub fn place(self, dirs: &PluginsDir, target: &Path) -> Result<PathBuf, PluginError> {
        dirs.check(target)?;
        let parent = target.parent().expect("a clone directory has a parent");
        dirs.create(parent)?;
        std::fs::rename(work_dir(&self), target).map_err(acquire_failed)?;
        Ok(target.to_owned())
    }
}

impl Drop for Staging {
    fn drop(&mut self) {
        if let Some(dir) = self.0.take() {
            let _ = std::fs::remove_dir_all(&dir);
            if let Some(parent) = dir.parent() {
                let _ = std::fs::remove_dir(parent);
            }
        }
    }
}

/// A Python package name: what an id becomes with `-` as `_`.
pub fn python_package(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// A directory link is a file on Unix and a directory on Windows.
fn remove_link(link: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        std::fs::remove_dir(link)
    }
    #[cfg(not(windows))]
    {
        std::fs::remove_file(link)
    }
}

fn outside(dir: impl std::fmt::Debug) -> PluginError {
    PluginError::new(
        "path_outside_plugins",
        format!("{dir:?} is not a PluginManager directory"),
    )
}

fn acquire_failed(error: impl std::fmt::Display) -> PluginError {
    PluginError::new("acquire_failed", error.to_string())
}

/// A Git URL safe to hand to a client: no user information, query or
/// fragment, and a value that is not such a URL becomes a placeholder.
pub fn git_url_safe(git: &str) -> String {
    match url::Url::parse(git) {
        Ok(mut url) if matches!(url.scheme(), "https" | "ssh") && url.host_str().is_some() => {
            let _ = url.set_username("");
            let _ = url.set_password(None);
            url.set_query(None);
            url.set_fragment(None);
            url.to_string()
        }
        _ => "Git repository".into(),
    }
}

/// `https://` or `ssh://` only; nothing Git could read as an option.
pub fn check_git_url(url: &str) -> Result<(), PluginError> {
    let refused = || {
        PluginError::new(
            "invalid_source",
            "Use an https:// or ssh:// Git URL without whitespace",
        )
    };
    let (scheme, rest) = url.split_once("://").ok_or_else(refused)?;
    if !matches!(scheme, "https" | "ssh")
        || rest.is_empty()
        || url.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        return Err(refused());
    }
    Ok(())
}

pub fn check_ref(reference: &str) -> Result<(), PluginError> {
    if reference.is_empty()
        || reference.starts_with('-')
        || reference
            .chars()
            .any(|c| c.is_whitespace() || c.is_control())
    {
        return Err(PluginError::new(
            "invalid_source",
            format!("{reference:?} is not a Git ref"),
        ));
    }
    Ok(())
}

/// Git with submodules, hooks, local transports and prompts off.
fn git(hooks: &Path) -> Command {
    let mut command = Command::new("git");
    command
        .arg("-c")
        .arg(format!("core.hooksPath={}", hooks.display()))
        .args([
            "-c",
            "protocol.file.allow=never",
            "-c",
            "protocol.ext.allow=never",
        ])
        .args(["-c", "submodule.recurse=false", "-c", "core.symlinks=true"])
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_ASKPASS", "")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    command
}

async fn run(mut command: Command) -> Result<String, PluginError> {
    let output = command.output().await.map_err(acquire_failed)?;
    if !output.status.success() {
        return Err(acquire_failed(
            String::from_utf8_lossy(&output.stderr).trim(),
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// Clones `url` at `reference` into `staging` and returns the commit. The
/// clone is checked: no symbolic link in it leads outside it.
pub async fn clone(
    url: &str,
    reference: Option<&str>,
    staging: &Staging,
) -> Result<String, PluginError> {
    check_git_url(url)?;
    if let Some(reference) = reference {
        check_ref(reference)?;
    }
    let dir = staging.path();
    let hooks = dir.join("hooks");
    let work = dir.join("work");
    std::fs::create_dir(&hooks).map_err(acquire_failed)?;
    let mut command = git(&hooks);
    command.args(["clone", "--no-recurse-submodules", "--quiet"]);
    if let Some(reference) = reference {
        command.arg("--branch").arg(reference);
    }
    command.arg("--").arg(url).arg(&work);
    run(command).await?;
    let mut command = git(&hooks);
    command.current_dir(&work).args(["rev-parse", "HEAD"]);
    let commit = run(command).await?;
    let work = std::fs::canonicalize(&work).map_err(acquire_failed)?;
    check_links(&work, &work)?;
    Ok(commit)
}

/// The checked-out tree of a clone in `staging`.
pub fn work_dir(staging: &Staging) -> PathBuf {
    staging.path().join("work")
}

pub fn check_links(root: &Path, dir: &Path) -> Result<(), PluginError> {
    for entry in std::fs::read_dir(dir).map_err(acquire_failed)? {
        let entry = entry.map_err(acquire_failed)?;
        let path = entry.path();
        let kind = entry.file_type().map_err(acquire_failed)?;
        if kind.is_symlink() {
            let leads_inside = std::fs::canonicalize(&path)
                .map(|target| target.starts_with(root))
                .unwrap_or(false);
            if !leads_inside {
                return Err(PluginError::new(
                    "invalid_manifest",
                    format!(
                        "{}: a symbolic link leads outside the plugin",
                        path.strip_prefix(root).unwrap_or(&path).display()
                    ),
                ));
            }
        } else if kind.is_dir() {
            check_links(root, &path)?;
        }
    }
    Ok(())
}

/// Reads a cloned plugin's manifest from its work tree.
pub fn manifest_of(staging: &Staging) -> Result<Manifest, PluginError> {
    let work = std::fs::canonicalize(work_dir(staging)).map_err(acquire_failed)?;
    Manifest::read(&work)
}

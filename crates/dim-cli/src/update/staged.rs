//! The staged npm update. The manager installs the new version into a
//! directory beside the live package; the update proves the staged tree,
//! exchanges the two package folders in one atomic call, points the commands at
//! the new package and asks the new entry point for its version. A failure
//! before the exchange leaves the live package exactly as it was, so an
//! interrupted update cannot take the command away, and a manager that reports
//! success without writing anything fails before the switch rather than after
//! it.

use super::{Install, run_manager};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// Where an update stages, inside the installation prefix so that the staged
/// and live packages share a filesystem and can be exchanged.
const STAGING: &str = ".dimcode-staging";

/// What one staged update left behind.
enum Outcome {
    /// The new version is live and verified.
    Switched,
    /// The previous version is live and staging holds nothing it needs.
    Failed(String),
    /// A tree exists only in staging, so staging is kept and the message says
    /// where each version is.
    Stranded(String),
}

/// Installs the new version beside the live package and switches it in, or
/// leaves the live package as it was.
pub(super) async fn update(install: &Install, cancel: CancellationToken) -> Result<(), String> {
    let root = install.directory.join(STAGING);
    clear(install, &root)?;
    let staging = root.join(format!("staging-{}", std::process::id()));
    std::fs::create_dir_all(&staging)
        .map_err(|error| format!("Cannot create {}: {error}", staging.display()))?;
    match replace(install, &staging, cancel).await {
        Outcome::Switched => {
            // A staging directory left behind costs space, not correctness:
            // the next update clears it.
            let _ = std::fs::remove_dir_all(&root);
            Ok(())
        }
        Outcome::Failed(error) => {
            let _ = std::fs::remove_dir_all(&root);
            Err(error)
        }
        Outcome::Stranded(error) => Err(error),
    }
}

/// Removes what an earlier update staged. The update holds the installation's
/// exclusive lease, so nothing else can be using it. When staging holds the only
/// copy of a tree, the error names that tree and the one move that restores it.
fn clear(install: &Install, root: &Path) -> Result<(), String> {
    if !root.exists() {
        return Ok(());
    }
    let live = package_at(&install.directory, &install.package);
    if !live.is_dir()
        && let Some(previous) = retired_package(root, &install.package)
    {
        return Err(format!(
            "An earlier update left {} at {} and the installation has no package, so {} was kept. Restore it with:\n  mv {} {}",
            install.package,
            previous.display(),
            root.display(),
            previous.display(),
            live.display()
        ));
    }
    std::fs::remove_dir_all(root)
        .map_err(|error| format!("Cannot clear {}: {error}", root.display()))
}

/// The package folder the two-rename fallback retired, in a staging directory an
/// earlier update did not finish clearing.
fn retired_package(root: &Path, package: &str) -> Option<PathBuf> {
    let entries = std::fs::read_dir(root).ok()?;
    entries.flatten().find_map(|entry| {
        let retired = entry.path().join("retired");
        retired
            .join("package.json")
            .is_file()
            .then(|| retired.join("package.json"))
            .and_then(|manifest| {
                let value: serde_json::Value =
                    serde_json::from_str(&std::fs::read_to_string(manifest).ok()?).ok()?;
                (value.get("name")?.as_str()? == package).then(|| retired.clone())
            })
    })
}

/// The package directory of `install` under `prefix`.
fn package_at(prefix: &Path, package: &str) -> PathBuf {
    prefix.join("lib").join("node_modules").join(package)
}

/// Runs the manager into `staging`, proves the staged tree, switches it in and
/// proves the result.
async fn replace(install: &Install, staging: &Path, cancel: CancellationToken) -> Outcome {
    if let Err(error) = run_manager(install, install.staging_command(staging), cancel).await {
        return Outcome::Failed(error);
    }
    let live = package_at(&install.directory, &install.package);
    let staged = package_at(staging, &install.package);
    let new = match staged_package(install, &staged) {
        Ok(new) => new,
        Err(error) => return Outcome::Failed(error),
    };
    // A live package whose manifest cannot be read names no command to retire,
    // which is also the state of an installation placed by hand.
    let previous = manifest(&live).map(|old| old.bins).unwrap_or_default();
    let switch = match switch_in(&live, &staged, staging.join("retired")) {
        Ok(switch) => switch,
        Err(outcome) => return outcome,
    };
    let mut changes = Vec::new();
    let checked = match link_bins(
        &install.directory,
        &install.package,
        &previous,
        &new,
        staging,
        &mut changes,
    ) {
        Ok(()) => verify_entries(&install.directory, &new).await,
        Err(error) => Err(error),
    };
    if let Err(error) = checked {
        let undone = undo(&changes, staging);
        return match switch.revert() {
            Ok(()) => Outcome::Failed(format!("{error}{}", notes(undone))),
            Err(revert) => Outcome::Stranded(format!("{error}{}; {revert}", notes(undone))),
        };
    }
    Outcome::Switched
}

/// A package's `package.json`: the version it is and the commands it installs.
struct Manifest {
    version: String,
    /// Each command the package installs, and the file inside it that runs.
    bins: Vec<(String, String)>,
}

/// Reads `<package>/package.json`.
fn manifest(package: &Path) -> Result<Manifest, String> {
    let path = package.join("package.json");
    let text = std::fs::read_to_string(&path)
        .map_err(|error| format!("Cannot read {}: {error}", path.display()))?;
    let value: serde_json::Value = serde_json::from_str(&text)
        .map_err(|error| format!("Cannot parse {}: {error}", path.display()))?;
    let version = value
        .get("version")
        .and_then(|version| version.as_str())
        .ok_or_else(|| format!("{} has no version", path.display()))?
        .to_owned();
    let name = value
        .get("name")
        .and_then(|name| name.as_str())
        .unwrap_or_default();
    let mut bins = Vec::new();
    match value.get("bin") {
        Some(serde_json::Value::String(file)) => bins.push((name.to_owned(), file.clone())),
        Some(serde_json::Value::Object(map)) => {
            for (command, file) in map {
                let file = file
                    .as_str()
                    .ok_or_else(|| format!("{}: bin.{command} is not a path", path.display()))?;
                bins.push((command.clone(), file.to_owned()));
            }
        }
        None => {}
        Some(_) => {
            return Err(format!(
                "{}: bin is neither a path nor a map of commands",
                path.display()
            ));
        }
    }
    Ok(Manifest { version, bins })
}

/// The staged package, once it says the manager installed a newer version of
/// this build's own channel, with the commands and platform package an
/// installation needs.
fn staged_package(install: &Install, staged: &Path) -> Result<Manifest, String> {
    let incomplete = |error: String| {
        format!(
            "The package manager reported success but the staged package is incomplete: {error}"
        )
    };
    let new = manifest(staged).map_err(incomplete)?;
    let current = crate::version::VERSION;
    let channel = super::channel(current);
    if super::channel(&new.version) != channel || !super::newer(current, &new.version) {
        return Err(format!(
            "The package manager installed {}, which is not a newer {channel} version than {current}; the installation is unchanged",
            new.version
        ));
    }
    if new.bins.is_empty() {
        return Err(incomplete(format!(
            "{} installs no command",
            staged.display()
        )));
    }
    for (command, file) in &new.bins {
        if !staged.join(file).exists() {
            return Err(incomplete(format!(
                "{file}, which installs {command}, is missing"
            )));
        }
    }
    if !platform_package(&install.package, staged) {
        return Err(incomplete(format!(
            "no platform package beside it in {}",
            staged.join("node_modules").display()
        )));
    }
    Ok(new)
}

/// Whether a platform package, which holds the binary behind an entry point,
/// sits beside the staged package.
fn platform_package(package: &str, staged: &Path) -> bool {
    let family = package
        .rsplit_once('-')
        .map_or(package, |(family, _)| family);
    let Ok(entries) = std::fs::read_dir(staged.join("node_modules")) else {
        return false;
    };
    entries.flatten().any(|entry| {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        name.starts_with(family)
            && name != package
            && !name.ends_with("-web")
            && entry.path().is_dir()
    })
}

/// How the live package folder was replaced, so the update can put the
/// previous one back and say what a failure left behind.
enum Switch {
    /// The live folder did not exist, so the staged one was placed.
    Placed { live: PathBuf },
    /// The two folders were exchanged in one call, which left the previous one
    /// in staging.
    Exchanged { live: PathBuf, staged: PathBuf },
    /// The filesystem refused the exchange, so the previous folder was retired
    /// and the staged one renamed into place.
    Renamed {
        live: PathBuf,
        staged: PathBuf,
        retired: PathBuf,
    },
}

/// Places the staged package at the live path.
fn switch_in(live: &Path, staged: &Path, retired: PathBuf) -> Result<Switch, Outcome> {
    if !live.exists() {
        return match std::fs::rename(staged, live) {
            Ok(()) => Ok(Switch::Placed {
                live: live.to_path_buf(),
            }),
            Err(error) => Err(Outcome::Failed(format!(
                "Cannot place {}: {error}",
                staged.display()
            ))),
        };
    }
    match exchange(live, staged) {
        Ok(()) => {
            return Ok(Switch::Exchanged {
                live: live.to_path_buf(),
                staged: staged.to_path_buf(),
            });
        }
        Err(error) if refused(&error) => {}
        Err(error) => {
            return Err(Outcome::Failed(format!(
                "Cannot replace {}: {error}",
                live.display()
            )));
        }
    }
    if let Err(error) = std::fs::rename(live, &retired) {
        return Err(Outcome::Failed(format!(
            "Cannot move {} aside: {error}",
            live.display()
        )));
    }
    if let Err(error) = std::fs::rename(staged, live) {
        return Err(match std::fs::rename(&retired, live) {
            Ok(()) => Outcome::Failed(format!("Cannot place {}: {error}", staged.display())),
            Err(restore) => Outcome::Stranded(format!(
                "Cannot place the staged package ({error}) and cannot move the previous one back ({restore}); the previous version is {} and belongs at {}",
                retired.display(),
                live.display()
            )),
        });
    }
    Ok(Switch::Renamed {
        live: live.to_path_buf(),
        staged: staged.to_path_buf(),
        retired,
    })
}

impl Switch {
    /// Puts the previous package back at the live path.
    fn revert(self) -> Result<(), String> {
        match self {
            Switch::Placed { live } => std::fs::remove_dir_all(&live)
                .map_err(|error| format!("cannot remove {} ({error})", live.display())),
            Switch::Exchanged { live, staged } => exchange(&live, &staged).map_err(|error| {
                format!(
                    "cannot exchange back ({error}); the previous version is {}",
                    staged.display()
                )
            }),
            Switch::Renamed {
                live,
                staged,
                retired,
            } => std::fs::rename(&live, &staged)
                .and_then(|()| std::fs::rename(&retired, &live))
                .map_err(|error| {
                    format!(
                        "cannot put the previous version back ({error}); the previous one is {}",
                        retired.display()
                    )
                }),
        }
    }
}

/// Whether the filesystem or the kernel refused the exchange itself, leaving
/// both paths as they were, rather than failing to move anything.
fn refused(error: &std::io::Error) -> bool {
    matches!(
        error.raw_os_error(),
        Some(code) if code == libc::ENOTSUP || code == libc::EINVAL || code == libc::ENOSYS
    )
}

/// Exchanges two directories in one step, so no observer sees either name
/// missing.
#[cfg(target_os = "macos")]
fn exchange(from: &Path, to: &Path) -> std::io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let from = CString::new(from.as_os_str().as_bytes())?;
    let to = CString::new(to.as_os_str().as_bytes())?;
    // SAFETY: both arguments are NUL-terminated paths that outlive the call,
    // and RENAME_SWAP is the flag renamex_np defines.
    if unsafe { libc::renamex_np(from.as_ptr(), to.as_ptr(), libc::RENAME_SWAP) } == 0 {
        return Ok(());
    }
    Err(std::io::Error::last_os_error())
}

#[cfg(target_os = "linux")]
fn exchange(from: &Path, to: &Path) -> std::io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let from = CString::new(from.as_os_str().as_bytes())?;
    let to = CString::new(to.as_os_str().as_bytes())?;
    // SAFETY: the call takes two NUL-terminated paths that outlive it, the
    // AT_FDCWD both sides are relative to, and RENAME_EXCHANGE; it moves
    // nothing and sets errno when it fails.
    let status = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            libc::AT_FDCWD,
            from.as_ptr(),
            libc::AT_FDCWD,
            to.as_ptr(),
            libc::RENAME_EXCHANGE as libc::c_uint,
        )
    };
    if status == 0 {
        return Ok(());
    }
    Err(std::io::Error::last_os_error())
}

/// A platform without a directory exchange keeps the two-rename switch, which
/// has a window between its two steps.
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn exchange(_from: &Path, _to: &Path) -> std::io::Result<()> {
    Err(std::io::Error::from(std::io::ErrorKind::Unsupported))
}

/// A command link the bin pass changed, and what putting it back takes.
enum BinChange {
    /// The link was created, so putting it back removes it.
    Added(PathBuf),
    /// The link pointed at this target, so putting it back relinks it there.
    Replaced(PathBuf, PathBuf),
}

/// Points every command the new version installs at the new package and drops a
/// command only the old version installed. A link with the target the new
/// version needs is left alone, so commands both versions install keep their
/// inode across the switch.
fn link_bins(
    prefix: &Path,
    package: &str,
    previous: &[(String, String)],
    new: &Manifest,
    staging: &Path,
    changes: &mut Vec<BinChange>,
) -> Result<(), String> {
    let root = prefix.join("bin");
    std::fs::create_dir_all(&root)
        .map_err(|error| format!("Cannot create {}: {error}", root.display()))?;
    for (command, file) in &new.bins {
        let target = format!("../lib/node_modules/{package}/{file}");
        let path = root.join(command);
        match std::fs::read_link(&path) {
            Ok(current) if current == Path::new(&target) => {}
            Ok(current) => {
                place_link(&path, &target, staging)?;
                changes.push(BinChange::Replaced(path, current));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                place_link(&path, &target, staging)?;
                changes.push(BinChange::Added(path));
            }
            Err(error) => {
                return Err(format!(
                    "{} is not a link ({error}); remove it and retry",
                    path.display()
                ));
            }
        }
    }
    let owned = format!("../lib/node_modules/{package}/");
    for (command, _) in previous {
        if new.bins.iter().any(|(installed, _)| installed == command) {
            continue;
        }
        let path = root.join(command);
        let Ok(target) = std::fs::read_link(&path) else {
            continue;
        };
        if !target.to_string_lossy().starts_with(&owned) {
            continue;
        }
        std::fs::remove_file(&path)
            .map_err(|error| format!("Cannot remove {}: {error}", path.display()))?;
        changes.push(BinChange::Replaced(path, target));
    }
    Ok(())
}

/// Writes a link to `target` beside `path` and renames it over `path`, so no
/// observer sees a command that names a link still being written. The link is
/// written under `staging`, which is on the same filesystem and not on `PATH`:
/// a crash between the two steps leaves nothing a shell could run.
fn place_link(path: &Path, target: &str, staging: &Path) -> Result<(), String> {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let temp = staging.join(format!("{name}.link"));
    let _ = std::fs::remove_file(&temp);
    std::os::unix::fs::symlink(target, &temp)
        .map_err(|error| format!("Cannot create {}: {error}", temp.display()))?;
    std::fs::rename(&temp, path).map_err(|error| {
        let _ = std::fs::remove_file(&temp);
        format!("Cannot place {}: {error}", path.display())
    })
}

/// Puts the command links back the way the switch found them.
fn undo(changes: &[BinChange], staging: &Path) -> Vec<String> {
    let mut notes = Vec::new();
    for change in changes.iter().rev() {
        let result = match change {
            BinChange::Added(path) => std::fs::remove_file(path)
                .map_err(|error| format!("cannot remove {} ({error})", path.display())),
            BinChange::Replaced(path, target) => {
                place_link(path, &target.to_string_lossy(), staging)
                    .map_err(|error| error.to_string())
            }
        };
        if let Err(note) = result {
            notes.push(note);
        }
    }
    notes
}

/// Runs every command the new version installs and checks that it reports the
/// version the package declares, which is the proof the switch left a working
/// command rather than a folder of files.
async fn verify_entries(prefix: &Path, new: &Manifest) -> Result<(), String> {
    for (command, _) in &new.bins {
        let path = prefix.join("bin").join(command);
        let reported = reported_version(&path).await?;
        if reported != new.version {
            return Err(format!(
                "{} reports {reported} after the switch, not {}",
                path.display(),
                new.version
            ));
        }
    }
    Ok(())
}

/// How long a switched-in command gets to answer `--version`: the switch runs to
/// its end, so the check carries its own bound rather than waiting on one.
const CHECK_TIMEOUT: Duration = Duration::from_secs(30);

/// What a command reports for `--version`: the last word of its last line.
async fn reported_version(command: &Path) -> Result<String, String> {
    let child = tokio::process::Command::new(command)
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| format!("cannot run {}: {error}", command.display()))?;
    let output = match tokio::time::timeout(CHECK_TIMEOUT, child.wait_with_output()).await {
        Ok(Ok(output)) => output,
        Ok(Err(error)) => return Err(format!("cannot run {}: {error}", command.display())),
        Err(_) => {
            return Err(format!(
                "{} --version did not answer within {}s, so it is unverified",
                command.display(),
                CHECK_TIMEOUT.as_secs()
            ));
        }
    };
    if !output.status.success() {
        return Err(format!(
            "{} --version failed ({})",
            command.display(),
            output.status
        ));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    text.lines()
        .rev()
        .find_map(|line| line.split_whitespace().next_back())
        .map(str::to_owned)
        .ok_or_else(|| format!("{} --version printed nothing", command.display()))
}

/// Joins rollback failures onto the message that caused the rollback.
fn notes(notes: Vec<String>) -> String {
    if notes.is_empty() {
        String::new()
    } else {
        format!("; {}", notes.join("; "))
    }
}

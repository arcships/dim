//! The update check: whether this build's npm channel has a newer version,
//! and how to install it the way this copy was installed. Alphas follow the
//! `alpha` dist-tag, betas `beta` and releases `latest`, so a channel only
//! ever upgrades within itself, as the publish scripts guarantee from the
//! other side.

#[cfg(unix)]
mod staged;

use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio_util::sync::CancellationToken;

const PACKAGE: &str = "dimcode";
/// Alphas ship as their own package with the `dima` command, so they install
/// beside `dimcode`'s `dim`.
const ALPHA_PACKAGE: &str = "dimcode-alpha";
const ALPHA_COMMAND: &str = "dima";
const REGISTRY: &str = "https://registry.npmjs.org";
const TIMEOUT: Duration = Duration::from_secs(4);
/// The version the user chose to skip, shared with the TypeScript CLI.
const SKIP_FILE: &str = "upgrade-skip.json";

/// A newer version on this build's channel.
#[derive(Debug, Clone)]
pub struct Offer {
    pub current: String,
    pub version: String,
    /// The command to run: `update` for a recognised installation, else the
    /// package manager's own command.
    pub manual: String,
}

/// An install of a version with the package manager this copy came from.
#[derive(Debug, Clone, PartialEq)]
pub struct Install {
    pub version: String,
    pub program: PathBuf,
    pub manager: Manager,
    /// The manager's installation root, outside the packages it replaces.
    pub directory: PathBuf,
    /// The package this copy was installed from, which names the directory the
    /// manager installs it into and the commands that start it.
    pub package: String,
}

/// How the manager installs the new version.
#[derive(Debug, Clone, PartialEq)]
pub enum Manager {
    /// The manager installs under any prefix it is given, so the update installs
    /// the new version beside the live package and switches it in afterwards.
    /// Only unix exchanges the package folder without touching a running image.
    #[cfg(unix)]
    Staged,
    /// The manager installs only into the layout the update replaces, with
    /// these arguments before the package.
    InPlace { arguments: Vec<String> },
}

impl Install {
    /// The package and channel tag the manager installs.
    fn spec(&self) -> String {
        format!("{}@{}", self.package, self.version)
    }

    /// The manager's arguments for installing the new version into `prefix`
    /// when that is a staging directory. `--globalconfig` keeps the global npm
    /// configuration of the live installation: `--prefix` alone points npm at
    /// `<prefix>/etc/npmrc` and would drop the registry, mirror and credentials
    /// an operator saved with `npm config set -g`.
    #[cfg(unix)]
    fn staging_command(&self, prefix: &Path) -> Vec<String> {
        let globalconfig = self.directory.join("etc").join("npmrc");
        vec![
            "install".to_owned(),
            "-g".to_owned(),
            "--prefer-online".to_owned(),
            "--prefix".to_owned(),
            prefix.to_string_lossy().into_owned(),
            "--globalconfig".to_owned(),
            globalconfig.to_string_lossy().into_owned(),
            self.spec(),
        ]
    }

    /// The manager's arguments for installing the version where it lives now.
    fn in_place_command(&self) -> Vec<String> {
        match &self.manager {
            #[cfg(unix)]
            Manager::Staged => self.staging_command(&self.directory),
            Manager::InPlace { arguments } => {
                arguments.iter().cloned().chain([self.spec()]).collect()
            }
        }
    }

    #[cfg(test)]
    pub fn command_line(&self) -> String {
        let program = self.program.file_name().unwrap_or(self.program.as_os_str());
        std::iter::once(program.to_string_lossy().into_owned())
            .chain(self.in_place_command())
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// Whether to check at all: `DIMCODE_UPDATE_CHECK=0` skips it, as do the
/// TypeScript CLI's `DIMCODE_DISABLE_AUTOUPDATE=1` and `DIMCODE_AUTOUPDATE=0`.
pub fn enabled() -> bool {
    let is = |name: &str, value: &str| std::env::var_os(name).is_some_and(|v| v == value);
    !is("DIMCODE_UPDATE_CHECK", "0")
        && !is("DIMCODE_DISABLE_AUTOUPDATE", "1")
        && !is("DIMCODE_AUTOUPDATE", "0")
}

/// The npm registry, or the mirror named in `DIMCODE_NPM_REGISTRY`.
pub fn registry() -> String {
    std::env::var("DIMCODE_NPM_REGISTRY")
        .map(|registry| registry.trim_end_matches('/').to_owned())
        .unwrap_or_else(|_| REGISTRY.to_owned())
}

/// The npm package this copy was started from; the launcher names it.
fn package() -> Result<String, String> {
    if let Some(package) = std::env::var_os("DIMCODE_NPM_PACKAGE") {
        return match package.to_str() {
            Some(PACKAGE) => Ok(PACKAGE.to_owned()),
            Some(ALPHA_PACKAGE) => Ok(ALPHA_PACKAGE.to_owned()),
            _ => Err("DIMCODE_NPM_PACKAGE must be dimcode or dimcode-alpha".to_owned()),
        };
    }
    let alpha = channel(env!("CARGO_PKG_VERSION")) == "alpha";
    Ok((if alpha { ALPHA_PACKAGE } else { PACKAGE }).to_owned())
}

/// The dist-tag a version upgrades along: its prerelease identifier, or
/// `latest` for a release.
pub fn channel(version: &str) -> &str {
    match prerelease(version) {
        Some((identifier, _)) => identifier,
        None => "latest",
    }
}

/// `(identifier, number)` of `X.Y.Z-identifier.N`, `None` for a release.
fn prerelease(version: &str) -> Option<(&str, u64)> {
    let (_, tail) = version.split_once('-')?;
    let (identifier, number) = tail.split_once('.')?;
    Some((identifier, number.parse().ok()?))
}

/// `X.Y.Z` of a version; `None` for anything else.
fn release(version: &str) -> Option<[u64; 3]> {
    let core = version.split('-').next()?;
    let mut parts = core.split('.').map(|part| part.parse::<u64>().ok());
    let (Some(Some(a)), Some(Some(b)), Some(Some(c)), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return None;
    };
    Some([a, b, c])
}

/// Whether `candidate` is a newer version of the same channel as `current`.
pub fn newer(current: &str, candidate: &str) -> bool {
    let (Some(now), Some(next)) = (release(current), release(candidate)) else {
        return false;
    };
    if next != now {
        return next > now;
    }
    match (prerelease(current), prerelease(candidate)) {
        (Some((a, n)), Some((b, m))) => a == b && m > n,
        _ => false,
    }
}

/// The install by hand from this build's channel.
fn manual(package: &str, current: &str) -> String {
    match channel(current) {
        "latest" => format!("npm i -g {package}"),
        tag => format!("npm i -g {package}@{tag}"),
    }
}

/// How the package manager that installed `exe` installs `version`, told
/// by where it keeps its global packages; `None` for any other source, such
/// as a build from this repository. npm uses the command in the prefix that
/// holds this copy and passes `--prefix`, so the install lands there even
/// when another Node comes first on `PATH`.
fn installer(exe: &Path, package: &str, version: &str) -> Option<Install> {
    let path = exe.to_string_lossy().replace('\\', "/");
    let (program, manager, directory) =
        if let Some((prefix, _)) = path.split_once("/.bun/install/global/node_modules/") {
            (
                "bun".into(),
                in_place(&["add", "-g"]),
                Path::new(prefix).join(".bun/install/global"),
            )
        } else if let Some((prefix, _)) = path.split_once("/.volta/") {
            (
                "volta".into(),
                in_place(&["install"]),
                Path::new(prefix).join(".volta"),
            )
        } else if let Some((prefix, _)) = path.split_once("/pnpm/global/") {
            (
                "pnpm".into(),
                in_place(&["add", "-g"]),
                Path::new(prefix).join("pnpm/global"),
            )
        } else if let Some((prefix, _)) = path.split_once("/.pnpm/") {
            let directory = Path::new(prefix);
            let directory = if directory.ends_with("node_modules") {
                directory.parent()?
            } else {
                directory
            };
            (
                "pnpm".into(),
                in_place(&["add", "-g"]),
                directory.to_path_buf(),
            )
        } else if let Some((prefix, _)) = path.split_once("/yarn/global/") {
            (
                "yarn".into(),
                in_place(&["global", "add"]),
                Path::new(prefix).join("yarn/global"),
            )
        } else {
            let marker = if cfg!(windows) {
                "/node_modules/"
            } else {
                "/lib/node_modules/"
            };
            let (prefix, _) = path.split_once(marker)?;
            let npm = if cfg!(windows) {
                Path::new(prefix).join("npm.cmd")
            } else {
                Path::new(prefix).join("bin").join("npm")
            };
            let npm = if npm.is_file() { npm } else { "npm".into() };
            // Windows npm installs under `<prefix>\node_modules` with command
            // shims in `<prefix>`, and this process runs from inside the folder
            // an exchange would replace, so it keeps the in-place install there.
            #[cfg(windows)]
            let manager = in_place(&["install", "-g", "--prefer-online", "--prefix", prefix]);
            // No unix system keeps an image mapped, so the package folder can be
            // replaced while this process runs from inside it.
            #[cfg(unix)]
            let manager = Manager::Staged;
            (npm, manager, PathBuf::from(prefix))
        };
    Some(Install {
        version: version.to_owned(),
        program,
        manager,
        directory,
        package: package.to_owned(),
    })
}

/// How a manager without a staging step is told to install.
fn in_place(arguments: &[&str]) -> Manager {
    Manager::InPlace {
        arguments: arguments
            .iter()
            .map(|argument| (*argument).to_owned())
            .collect(),
    }
}

/// The dist-tag a build upgrades along, `None` when it has none: only a build
/// the release flow produced upgrades, since a source build's version is the
/// last released one rather than its own.
fn upgrade_tag(version: &str, release: bool) -> Option<&str> {
    release.then_some(channel(version))
}

/// A newer version on the channel of `current` the user has not skipped, or
/// `None` when there is none or the registry did not answer in time.
pub async fn available(home: &Path, current: &str) -> Option<Offer> {
    let tag = upgrade_tag(current, crate::version::RELEASE)?;
    let package = package().ok()?;
    let client = reqwest::Client::builder().timeout(TIMEOUT).build().ok()?;
    let tags: serde_json::Value = client
        .get(format!("{}/-/package/{package}/dist-tags", registry()))
        .send()
        .await
        .ok()?
        .error_for_status()
        .ok()?
        .json()
        .await
        .ok()?;
    let version = tags.get(tag)?.as_str()?;
    if !newer(current, version) || skipped(home).as_deref() == Some(version) {
        return None;
    }
    let exe = std::env::current_exe().ok()?;
    let manual = if installer(&exe, &package, version).is_some() {
        let command = if package == ALPHA_PACKAGE {
            ALPHA_COMMAND
        } else {
            PACKAGE
        };
        format!("{command} update")
    } else {
        manual(&package, current)
    };
    Some(Offer {
        current: current.to_owned(),
        version: version.to_owned(),
        manual,
    })
}

/// Updates this installation's package on its current channel, independently
/// of automatic-check preferences and skipped versions. Unknown installation
/// layouts return a manual command without installing anything.
pub async fn run(legacy_exited: bool) -> crate::Result<i32> {
    let package = package()?;
    let current = crate::version::VERSION;
    let tag = channel(current);
    let executable = std::env::current_exe()?;
    let installation = installer(&executable, &package, tag).ok_or_else(|| {
        format!(
            "Cannot update this installation automatically. For a package install, run: {}. For a source build, pull and rebuild the source.",
            manual(&package, current)
        )
    })?;
    let mut signals = crate::plugins::Signals::listen()?;
    let mut stderr = std::io::stderr();
    let interactive = stderr.is_terminal();
    if !interactive {
        writeln!(stderr, "Updating {package} on the {tag} channel...")?;
    }
    let cancel = CancellationToken::new();
    let installation = install(installation, cancel.clone(), legacy_exited);
    tokio::pin!(installation);
    let started = tokio::time::Instant::now();
    let mut progress = tokio::time::interval(Duration::from_millis(120));
    let mut output_error = None;
    let result = loop {
        tokio::select! {
            result = &mut installation => break result,
            _ = signals.received(), if !cancel.is_cancelled() => cancel.cancel(),
            _ = progress.tick(), if interactive && output_error.is_none() => {
                let elapsed = started.elapsed();
                let frame = ['|', '/', '-', '\\'][(elapsed.as_millis() / 120 % 4) as usize];
                let detail = crate::sanitize::safe_line(&format!("({tag}) · {}s", elapsed.as_secs()));
                let width = crossterm::terminal::size().map_or(80, |(width, _)| width);
                let label_width = usize::from(width).saturating_sub(
                    unicode_width::UnicodeWidthStr::width(detail.as_str()) + 4
                );
                let label = crate::sanitize::safe_line(&format!("Updating {package}"));
                let label = crate::ui::text::truncate_cells(&label, label_width);
                let text = format!("{frame} {label} {detail}");
                let text = crate::ui::text::truncate_cells(&text, width.saturating_sub(1).into());
                if let Err(error) = crossterm::execute!(stderr,
                    crossterm::cursor::MoveToColumn(0),
                    crossterm::terminal::Clear(crossterm::terminal::ClearType::CurrentLine),
                    crossterm::style::Print(text),
                ) {
                    output_error = Some(error);
                    cancel.cancel();
                }
            }
        }
    };
    if let Some(error) = output_error {
        return Err(format!("{error}; {}", result.unwrap_err()).into());
    }
    if interactive {
        crossterm::execute!(
            stderr,
            crossterm::cursor::MoveToColumn(0),
            crossterm::terminal::Clear(crossterm::terminal::ClearType::CurrentLine),
        )?;
    }
    result?;
    writeln!(stderr, "Updated {package} on the {tag} channel.")?;
    Ok(0)
}

const LOCK_FILE: &str = ".dimcode-update.lock";
const BASELINE_FILE: &str = ".dimcode-consumers";

/// The shared lock this process holds on its installation for its lifetime.
static LEASE: std::sync::Mutex<Option<std::fs::File>> = std::sync::Mutex::new(None);

fn open_lock(directory: &Path, create: bool) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options
        .read(true)
        .write(create)
        .create(create)
        .truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o644).custom_flags(libc::O_NOFOLLOW);
    }
    let file = options.open(directory.join(LOCK_FILE))?;
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::other(
            "Installation lock must be a regular file",
        ));
    }
    Ok(file)
}

fn shared(directory: &Path) -> Result<Option<std::fs::File>, String> {
    let path = directory.join(LOCK_FILE);
    let file = match open_lock(directory, true).or_else(|_| open_lock(directory, false)) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!(
                "Cannot register with installation {}: {error}. Make {LOCK_FILE} readable to every user of this installation",
                path.display()
            ));
        }
    };
    match file.try_lock_shared() {
        Ok(()) => Ok(Some(file)),
        Err(std::fs::TryLockError::WouldBlock) => {
            Err("An update is replacing this installation; retry after it finishes".to_owned())
        }
        Err(std::fs::TryLockError::Error(error)) => Err(error.to_string()),
    }
}

/// Registers this process as a live consumer of the installation it runs
/// from, in any home, until it exits. An update refuses while any consumer
/// holds the lease. Does nothing for a source build, a layout the updater
/// does not recognise, or an installation no update has locked yet. Fails
/// when the lock file exists but cannot be opened, and while an update is
/// replacing the installation.
pub fn hold_installation() -> Result<(), String> {
    let Ok(package) = package() else {
        return Ok(());
    };
    let Some(directory) = std::env::current_exe()
        .ok()
        .and_then(|exe| installer(&exe, &package, "x"))
        .map(|install| install.directory)
    else {
        return Ok(());
    };
    *LEASE.lock().unwrap() = shared(&directory)?;
    Ok(())
}

/// Runs `install`; failures report the last nonempty stderr line.
/// Cancellation terminates and waits for the installer's process tree.
///
/// The installation lock is taken exclusively, so it fails while any other
/// process (client, server or installer, in any home) uses the installation.
/// This process's own lease is released for the duration and restored after.
/// Without the baseline marker an older consumer, which holds no lease, cannot
/// be ruled out: the install is refused unless `legacy_exited` states that
/// every older client and server has exited, which then creates the marker.
pub async fn install(
    install: Install,
    cancel: CancellationToken,
    legacy_exited: bool,
) -> Result<(), String> {
    let own = LEASE.lock().unwrap().take();
    drop(own);
    let result = install_exclusive(&install, cancel, legacy_exited).await;
    *LEASE.lock().unwrap() = shared(&install.directory).ok().flatten();
    result
}

async fn install_exclusive(
    install: &Install,
    cancel: CancellationToken,
    legacy_exited: bool,
) -> Result<(), String> {
    let lock = lease(install, legacy_exited)?;
    let result = match install.manager {
        #[cfg(unix)]
        Manager::Staged => staged::update(install, cancel).await,
        Manager::InPlace { .. } => run_manager(install, install.in_place_command(), cancel).await,
    };
    drop(lock);
    result
}

/// Takes the installation's exclusive lease and records the baseline marker,
/// returning the lock so it is held for as long as the caller needs it.
fn lease(install: &Install, legacy_exited: bool) -> Result<std::fs::File, String> {
    let path = install.directory.join(LOCK_FILE);
    let lock = open_lock(&install.directory, true)
        .map_err(|error| format!("Cannot lock installation {}: {error}", path.display()))?;
    match lock.try_lock() {
        Ok(()) => {}
        Err(std::fs::TryLockError::WouldBlock) => {
            return Err(
                "This installation is in use by a client, server or update (in any home). Exit them, stop the server with `dimcode daemon stop`, and retry".to_owned(),
            );
        }
        Err(std::fs::TryLockError::Error(error)) => return Err(error.to_string()),
    }
    let baseline = install.directory.join(BASELINE_FILE);
    if !baseline.exists() {
        if !legacy_exited {
            return Err("Older versions do not register as users of this installation, so an idle installation cannot be confirmed. Exit every client and stop every server, then run `dimcode update --confirm-legacy-exited`".to_owned());
        }
        std::fs::write(&baseline, b"")
            .map_err(|error| format!("Cannot record {}: {error}", baseline.display()))?;
    }
    Ok(lock)
}

/// Runs the manager and waits for it, ending its process tree on cancellation.
async fn run_manager(
    install: &Install,
    args: Vec<String>,
    cancel: CancellationToken,
) -> Result<(), String> {
    let mut command = tokio::process::Command::new(&install.program);
    command.args(&args).stdin(std::process::Stdio::null());
    let mut tree = dim_agent_capabilities::host_platform::ProcessTree::spawn(command)
        .map_err(|error| format!("{}: {error}", install.program.display()))?;
    let mut stdout = tree.stdout();
    let mut stderr = tree.stderr();
    let mut errors = Vec::new();
    let mut sink = tokio::io::sink();
    let result = tokio::select! {
        biased;
        _ = cancel.cancelled() => Err("Update interrupted".to_owned()),
        result = async {
            let (status, _, _) = tokio::try_join!(
                tree.wait(),
                tokio::io::copy(&mut stdout, &mut sink),
                stderr.read_to_end(&mut errors),
            )?;
            Ok::<_, std::io::Error>(status)
        } => result.map_err(|error| error.to_string()),
    };
    let status = match result {
        Ok(status) => status,
        Err(error) => {
            tree.terminate()
                .await
                .map_err(|cleanup| format!("{error}; {cleanup}"))?;
            return Err(error);
        }
    };
    if status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&errors);
    Err(stderr
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .map_or_else(|| status.to_string(), |line| line.trim().to_owned()))
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Skipped {
    version: String,
}

/// The version the user skipped, if any.
fn skipped(home: &Path) -> Option<String> {
    let text = std::fs::read_to_string(home.join(SKIP_FILE)).ok()?;
    Some(serde_json::from_str::<Skipped>(&text).ok()?.version)
}

/// Remembers `version` as skipped: only it stays silent, a newer one is
/// offered again.
pub fn skip(home: &Path, version: &str) -> std::io::Result<()> {
    let text = serde_json::to_string(&Skipped {
        version: version.to_owned(),
    })?;
    let mut file = std::fs::OpenOptions::new();
    file.create(true).write(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        file.mode(0o600);
    }
    std::fs::create_dir_all(home)?;
    std::io::Write::write_all(&mut file.open(home.join(SKIP_FILE))?, text.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_channel_only_upgrades_within_itself() {
        assert_eq!(channel("0.1.0-alpha.3"), "alpha");
        assert_eq!(channel("0.5.20-beta.0"), "beta");
        assert_eq!(channel("0.5.21"), "latest");
        assert!(newer("0.1.0-alpha.0", "0.1.0-alpha.1"));
        assert!(newer("0.1.0-alpha.9", "0.2.0-alpha.0"));
        assert!(!newer("0.1.0-alpha.1", "0.1.0-alpha.1"));
        assert!(!newer("0.1.0-alpha.1", "0.1.0-alpha.0"));
        assert!(!newer("0.1.0-alpha.1", "0.1.0-beta.0"));
        assert!(!newer("0.1.0-alpha.1", "0.1.0"));
        assert!(newer("0.5.20", "0.5.21"));
        assert!(!newer("0.5.21", "0.5.21-beta.0"));
        assert_eq!(manual("dimcode", "0.1.0-alpha.0"), "npm i -g dimcode@alpha");
        assert_eq!(manual("dimcode", "0.5.21"), "npm i -g dimcode");
        assert_eq!(
            manual(ALPHA_PACKAGE, "0.1.0-alpha.0"),
            "npm i -g dimcode-alpha@alpha"
        );
    }

    #[test]
    fn a_build_from_source_has_no_channel_to_upgrade_along() {
        assert_eq!(upgrade_tag("0.1.0-alpha.8", false), None);
        assert_eq!(upgrade_tag("0.1.0-alpha.8", true), Some("alpha"));
        assert_eq!(upgrade_tag("0.5.22", true), Some("latest"));
    }

    #[cfg(unix)]
    #[test]
    fn the_install_follows_the_manager_this_copy_came_from() {
        let spec = "0.1.0-alpha.2";
        let npm = installer(
            Path::new("/u/.nvm/versions/node/v24/lib/node_modules/dimcode/node_modules/dimcode-darwin-arm64/bin/dimcode"),
            "dimcode",
            spec,
        )
        .unwrap();
        assert_eq!(
            npm.command_line(),
            "npm install -g --prefer-online --prefix /u/.nvm/versions/node/v24 --globalconfig /u/.nvm/versions/node/v24/etc/npmrc dimcode@0.1.0-alpha.2"
        );
        assert_eq!(npm.package, "dimcode");
        assert_eq!(npm.manager, Manager::Staged);
        let prefix = tempfile::tempdir().unwrap();
        std::fs::create_dir(prefix.path().join("bin")).unwrap();
        std::fs::write(prefix.path().join("bin/npm"), "").unwrap();
        let copied = prefix.path().join("lib/node_modules/dimcode/bin/dimcode");
        let np = installer(&copied, "dimcode", spec).unwrap();
        assert_eq!(np.program, prefix.path().join("bin/npm"));
        assert_eq!(
            np.staging_command(Path::new("/stage"))
                .last()
                .map(String::as_str),
            Some("dimcode@0.1.0-alpha.2")
        );
        let bun = installer(
            Path::new("/u/.bun/install/global/node_modules/dimcode-darwin-arm64/bin/dimcode"),
            "dimcode",
            spec,
        )
        .unwrap();
        assert_eq!(bun.command_line(), "bun add -g dimcode@0.1.0-alpha.2");
        let pnpm = installer(
            Path::new("/u/Library/pnpm/global/5/.pnpm/dimcode-darwin-arm64@0.1.0/node_modules/dimcode-darwin-arm64/bin/dimcode"),
            "dimcode",
            spec,
        )
        .unwrap();
        assert_eq!(pnpm.command_line(), "pnpm add -g dimcode@0.1.0-alpha.2");
        let alpha = installer(
            Path::new("/u/Library/pnpm/global/5/.pnpm/dimcode-darwin-arm64@0.1.0-alpha.2/node_modules/dimcode-darwin-arm64/bin/dimcode"),
            ALPHA_PACKAGE,
            spec,
        )
        .unwrap();
        assert_eq!(
            alpha.command_line(),
            "pnpm add -g dimcode-alpha@0.1.0-alpha.2"
        );
        assert!(installer(Path::new("/u/dim/target/debug/dimcode"), "dimcode", spec).is_none());
    }

    /// Windows replaces the package folder in place, so its command stays the
    /// one it always was: no staging prefix and no `--globalconfig`.
    #[cfg(windows)]
    #[test]
    fn the_install_keeps_the_in_place_command_on_windows() {
        let install = installer(
            Path::new(r"C:\Users\u\AppData\Roaming\npm\node_modules\dimcode\bin\dimcode.exe"),
            "dimcode",
            "0.1.0-alpha.2",
        )
        .unwrap();
        assert_eq!(
            install.command_line(),
            "npm install -g --prefer-online --prefix C:/Users/u/AppData/Roaming/npm dimcode@0.1.0-alpha.2"
        );
        assert!(matches!(install.manager, Manager::InPlace { .. }));
    }
}

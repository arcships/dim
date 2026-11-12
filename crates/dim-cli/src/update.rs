//! The update check: whether this build's npm channel has a newer version,
//! and how to install it the way this copy was installed. Alphas follow the
//! `alpha` dist-tag, betas `beta` and releases `latest`, so a channel only
//! ever upgrades within itself, as the publish scripts guarantee from the
//! other side.

use std::path::{Path, PathBuf};
use std::time::Duration;

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
    /// How to install it in place; `None` when the install source is not
    /// one we can tell, and only `manual` is shown.
    pub install: Option<Install>,
    /// The command to run by hand.
    pub manual: String,
}

/// An install of a version with the package manager this copy came from.
#[derive(Debug, Clone, PartialEq)]
pub struct Install {
    pub version: String,
    pub program: PathBuf,
    pub args: Vec<String>,
    /// What starts the updated CLI: this executable when the install keeps
    /// it in place, else the package's command on `PATH`.
    pub relaunch: PathBuf,
}

impl Install {
    pub fn command_line(&self) -> String {
        let program = self.program.file_name().unwrap_or(self.program.as_os_str());
        std::iter::once(program.to_string_lossy().into_owned())
            .chain(self.args.iter().cloned())
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
fn package() -> String {
    std::env::var("DIMCODE_NPM_PACKAGE").unwrap_or_else(|_| {
        let alpha = channel(env!("CARGO_PKG_VERSION")) == "alpha";
        (if alpha { ALPHA_PACKAGE } else { PACKAGE }).to_owned()
    })
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
    let spec = format!("{package}@{version}");
    let (program, args) = if path.contains("/.bun/install/global/node_modules/") {
        ("bun".into(), vec!["add", "-g"])
    } else if path.contains("/.volta/") {
        ("volta".into(), vec!["install"])
    } else if path.contains("/pnpm/global/") || path.contains("/.pnpm/") {
        ("pnpm".into(), vec!["add", "-g"])
    } else if path.contains("/yarn/global/") {
        ("yarn".into(), vec!["global", "add"])
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
        (
            npm,
            vec!["install", "-g", "--prefer-online", "--prefix", prefix],
        )
    };
    Some(Install {
        version: version.to_owned(),
        program,
        args: args.into_iter().map(str::to_owned).chain([spec]).collect(),
        relaunch: relaunch(exe, package),
    })
}

/// npm and bun reinstall a package in place, so this executable is the
/// updated one afterwards; pnpm, yarn and volta move it, so the package's
/// command on `PATH` starts it.
fn relaunch(exe: &Path, package: &str) -> PathBuf {
    let path = exe.to_string_lossy().replace('\\', "/");
    if path.contains("/.pnpm/") || path.contains("/yarn/global/") || path.contains("/.volta/") {
        PathBuf::from(if package == ALPHA_PACKAGE {
            ALPHA_COMMAND
        } else {
            package
        })
    } else {
        exe.to_path_buf()
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
    let package = package();
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
    Some(Offer {
        current: current.to_owned(),
        version: version.to_owned(),
        install: installer(&exe, &package, version),
        manual: manual(&package, current),
    })
}

/// Updates this installation's package on its current channel, independently
/// of automatic-check preferences and skipped versions. Unknown installation
/// layouts return a manual command without installing anything.
pub async fn run() -> crate::Result<i32> {
    let package = package();
    let current = crate::version::VERSION;
    let tag = channel(current);
    let executable = std::env::current_exe()?;
    let installation = installer(&executable, &package, tag).ok_or_else(|| {
        format!(
            "Cannot update this installation automatically. For a package install, run: {}. For a source build, pull and rebuild the source.",
            manual(&package, current)
        )
    })?;
    println!("Updating {package} on the {tag} channel...");
    println!("{}", installation.command_line());
    let mut child = tokio::process::Command::new(&installation.program)
        .args(&installation.args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .spawn()?;
    let started = tokio::time::Instant::now();
    let period = Duration::from_secs(5);
    let mut progress = tokio::time::interval_at(started + period, period);
    let status = loop {
        tokio::select! {
            status = child.wait() => break status?,
            _ = progress.tick() => {
                eprintln!("Still updating {package}... {}s elapsed.", started.elapsed().as_secs());
            }
        }
    };
    if !status.success() {
        return Err(format!("{} failed: {status}", installation.program.display()).into());
    }
    println!("Updated {package} on the {tag} channel.");
    Ok(0)
}

/// Runs `install`; the last line it wrote to stderr when it fails.
pub async fn install(install: Install) -> Result<(), String> {
    let output = tokio::process::Command::new(&install.program)
        .args(&install.args)
        .stdin(std::process::Stdio::null())
        .output()
        .await
        .map_err(|error| format!("{}: {error}", install.program.display()))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    Err(stderr
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .map_or_else(|| output.status.to_string(), |line| line.trim().to_owned()))
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
            "npm install -g --prefer-online --prefix /u/.nvm/versions/node/v24 dimcode@0.1.0-alpha.2"
        );
        assert!(npm.relaunch.ends_with("bin/dimcode"));
        let prefix = tempfile::tempdir().unwrap();
        std::fs::create_dir(prefix.path().join("bin")).unwrap();
        std::fs::write(prefix.path().join("bin/npm"), "").unwrap();
        let copied = prefix.path().join("lib/node_modules/dimcode/bin/dimcode");
        let np = installer(&copied, "dimcode", spec).unwrap();
        assert_eq!(np.program, prefix.path().join("bin/npm"));
        assert_eq!(
            np.args.last().map(String::as_str),
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
        assert_eq!(pnpm.relaunch, PathBuf::from("dimcode"));
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
        assert_eq!(alpha.relaunch, PathBuf::from("dima"));
        assert!(installer(Path::new("/u/dim/target/debug/dimcode"), "dimcode", spec).is_none());
    }
}

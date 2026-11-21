//! The staged update. The manager installs beside the live package; only a
//! verified staged tree is switched in. Each flow builds a global npm
//! installation whose command reports the version in its `package.json`, drives
//! the real update path against a fake npm held at a gate, and reads the
//! command afterwards to see which version the installation ends on.

use super::*;
use std::path::Path;

/// The version the installation starts on.
const CURRENT: &str = env!("CARGO_PKG_VERSION");

/// A version the update may install: one above the current number.
fn next_version() -> String {
    bump(CURRENT)
}

/// `version` with its last component's number raised by one.
fn bump(version: &str) -> String {
    let (head, tail) = version.rsplit_once('.').unwrap();
    format!("{head}.{}", tail.parse::<u64>().unwrap() + 1)
}

fn package_json(version: &str, bins: &[(&str, &str)]) -> String {
    let map = bins
        .iter()
        .map(|(key, file)| format!("\"{key}\":\"{file}\""))
        .collect::<Vec<_>>()
        .join(",");
    format!("{{\"name\":\"dimcode-alpha\",\"version\":\"{version}\",\"bin\":{{{map}}}}}")
}

/// A command that reports the version of the package it belongs to, the way the
/// installed command reports the version of its own installation.
const REPORTS_VERSION: &str = r#"#!/bin/sh
target=$0
while [ -L "$target" ]; do
  link=$(readlink "$target")
  case $link in
    /*) target=$link ;;
    *) target=$(dirname "$target")/$link ;;
  esac
done
directory=$(cd "$(dirname "$target")" && pwd) || exit 1
while [ ! -f "$directory/package.json" ] && [ "$directory" != / ]; do directory=$(dirname "$directory"); done
sed -n 's/.*"version":"\([^"]*\)".*/\1/p' "$directory/package.json"
"#;

/// A command that fails, so the post-switch check has something to catch.
const BREAKS: &str = "#!/bin/sh\nexit 1\n";

/// A command that reports its package's version only after it has printed a
/// marker and waited, so a flow can interrupt the update while it checks. The
/// wait happens only for the CLI the flow gave `UPDATE_SLOW`, so the test's own
/// version reads stay quick.
const SLOW_VERSION: &str = r#"#!/bin/sh
target=$0
while [ -L "$target" ]; do
  link=$(readlink "$target")
  case $link in
    /*) target=$link ;;
    *) target=$(dirname "$target")/$link ;;
  esac
done
directory=$(cd "$(dirname "$target")" && pwd) || exit 1
while [ ! -f "$directory/package.json" ] && [ "$directory" != / ]; do directory=$(dirname "$directory"); done
if [ -n "$UPDATE_SLOW" ]; then
  printf 'checking %s\n' "$directory" >> "$UPDATE_CHECKING"
  sleep 2
fi
sed -n 's/.*"version":"\([^"]*\)".*/\1/p' "$directory/package.json"
"#;

/// A global npm installation of `dimcode-alpha` at `version`, with `commands`
/// linked into its `bin` directory.
struct Installed {
    updater: PathBuf,
    prefix: PathBuf,
    home: Home,
}

fn install_updater(destination: &Path) {
    let status = Command::new("install")
        .args(["-m", "700", env!("CARGO_BIN_EXE_dimcode")])
        .arg(destination)
        .status()
        .unwrap();
    assert!(
        status.success(),
        "install updater at {destination:?}: {status}"
    );
}

impl Installed {
    fn new(version: &str, commands: &[&str]) -> Self {
        let home = Home::new();
        fs::remove_dir_all(home.0.join("rust")).unwrap();
        let prefix = home.0.join("node prefix");
        let package = prefix.join("lib/node_modules/dimcode-alpha");
        fs::create_dir_all(package.join("bin")).unwrap();
        fs::create_dir_all(package.join("node_modules/dimcode-darwin-arm64/bin")).unwrap();
        fs::create_dir_all(prefix.join("bin")).unwrap();
        // The update runs from inside the package, as the installed command
        // does, so the copy it runs from is how it finds the installation.
        let updater = package.join("bin/dimcode");
        install_updater(&updater);
        let launcher = package.join("bin/dimcode.mjs");
        fs::write(&launcher, REPORTS_VERSION).unwrap();
        fs::set_permissions(&launcher, fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(
            package.join("package.json"),
            package_json(version, &[("dima", "bin/dimcode.mjs")]),
        )
        .unwrap();
        for command in commands {
            std::os::unix::fs::symlink(
                "../lib/node_modules/dimcode-alpha/bin/dimcode.mjs",
                prefix.join("bin").join(command),
            )
            .unwrap();
        }
        Self {
            updater: fs::canonicalize(updater).unwrap(),
            prefix: fs::canonicalize(prefix).unwrap(),
            home,
        }
    }

    /// Rewrites the live package's manifest so it declares `bins`, which is the
    /// set the update may retire.
    fn declare(&self, bins: &[(&str, &str)]) {
        fs::write(
            self.prefix
                .join("lib/node_modules/dimcode-alpha/package.json"),
            package_json(CURRENT, bins),
        )
        .unwrap();
    }

    /// npm is taken from `<prefix>/bin/npm`, so the fake one goes there.
    fn mock_npm(&self) {
        let npm = self.prefix.join("bin/npm");
        fs::write(&npm, MOCK_NPM).unwrap();
        fs::set_permissions(&npm, fs::Permissions::from_mode(0o700)).unwrap();
    }

    /// The real `update` command with the settings the fake npm reads.
    fn update(&self, version: &str, bins: &[(&str, &str)], settings: &[(&str, &str)]) -> Command {
        let mut command = Command::new(&self.updater);
        let links = bins
            .iter()
            .map(|(key, file)| format!("{key}={file}"))
            .collect::<Vec<_>>()
            .join("\n");
        command
            .args(["--home", self.home.0.to_str().unwrap(), "update"])
            .env("DIMCODE_NPM_PACKAGE", "dimcode-alpha")
            .env("UPDATE_ARGS", self.home.0.join("install-args"))
            .env("UPDATE_VERSION", version)
            .env("UPDATE_PACKAGE_JSON", package_json(version, bins))
            .env("UPDATE_BINS", links)
            .env("UPDATE_GATE", self.home.0.join("gate"))
            .env("UPDATE_WAITING", self.home.0.join("waiting"));
        for (name, value) in settings {
            command.env(name, value);
        }
        command
    }

    /// Opens the gate every fake npm run waits at.
    fn release(&self) {
        fs::write(self.home.0.join("gate"), "go").unwrap();
    }

    /// Waits until the fake npm reached its gate.
    fn until_waiting(&self) {
        let deadline = Instant::now() + HANG_GUARD;
        while !self.home.0.join("waiting").exists() {
            assert!(
                Instant::now() < deadline,
                "the fake manager never reached its gate"
            );
            thread::sleep(Duration::from_millis(10));
        }
    }

    /// What an installed command reports for `--version`, or why it does not
    /// run at all.
    fn try_version(&self, command: &str) -> Result<String, String> {
        let path = self.prefix.join("bin").join(command);
        let output = Command::new(&path)
            .arg("--version")
            .output()
            .map_err(|error| format!("{} does not run: {error}", path.display()))?;
        if !output.status.success() {
            return Err(format!(
                "{} failed ({}): {}",
                path.display(),
                output.status,
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        let text = String::from_utf8_lossy(&output.stdout);
        text.split_whitespace()
            .next_back()
            .map(str::to_owned)
            .ok_or_else(|| format!("{} printed nothing", path.display()))
    }

    /// What an installed command reports for `--version`.
    fn version(&self, command: &str) -> String {
        self.try_version(command).unwrap()
    }

    fn installed(&self) -> String {
        self.version("dima")
    }

    fn staging(&self) -> PathBuf {
        self.prefix.join(".dimcode-staging").join("dimcode-alpha")
    }

    /// The arguments of the last manager run, one per line. A flag's value
    /// follows it on its own line, indented by a tab.
    fn recorded(&self) -> Vec<String> {
        fs::read_to_string(self.home.0.join("install-args"))
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect()
    }
}

/// A fake npm. It records its arguments with the values of `--prefix` and
/// `--globalconfig` after a tab, retires the package it replaces under that
/// prefix the way arborist's reify does, writes a marker, then waits at its gate
/// and writes the new package with a link for every command it declares. An
/// empty `UPDATE_VERSION` exits 0 without writing anything.
const MOCK_NPM: &str = r#"#!/bin/sh
: > "$UPDATE_ARGS"
prefix=
while [ "$#" -gt 0 ]; do
  case "$1" in
    --prefix) printf '%s\n' "$1" >> "$UPDATE_ARGS"; prefix=$2; printf '\t%s\n' "$2" >> "$UPDATE_ARGS"; shift 2; continue ;;
    --globalconfig) printf '%s\n' "$1" >> "$UPDATE_ARGS"; printf '\t%s\n' "$2" >> "$UPDATE_ARGS"; shift 2; continue ;;
  esac
  printf '%s\n' "$1" >> "$UPDATE_ARGS"
  shift
done
echo 'Downloading package' >&2
package="$prefix/lib/node_modules/dimcode-alpha"
retired="$prefix/lib/node_modules/.dimcode-alpha.retired"
rm -rf "$retired"
if [ -d "$package" ]; then mv "$package" "$retired"; fi
: > "$UPDATE_WAITING"
# The gate lives in the test's own home: a fresh directory made for this test
# alone, removed only when this test ends. The loop can end only after its own
# test has ended, so no other test's home can end it early.
home=${UPDATE_GATE%/*}
while [ ! -f "$UPDATE_GATE" ]; do
  [ -d "$home" ] || exit 1
  sleep 0.1
done
if [ "$UPDATE_FAIL" = 1 ]; then exit 143; fi
if [ -z "$UPDATE_VERSION" ]; then echo 'Installed package'; exit 0; fi
payload="$package/node_modules/${UPDATE_PLATFORM_PACKAGE:-dimcode-darwin-arm64}"
mkdir -p "$payload/bin"
printf '%s' "$UPDATE_COMMAND" > "$payload/bin/dimcode"
chmod 700 "$payload/bin/dimcode"
if [ -n "$UPDATE_RELEASE_MANIFEST" ]; then
  printf '%s\n' "$UPDATE_RELEASE_MANIFEST" > "$payload/release-manifest.json"
fi
printf '%s\n' "$UPDATE_PACKAGE_JSON" > "$package/package.json"
# npm links each command under the normalized manifest key, not under the key
# as written: bin-links/lib/link-bins.js normalizes the manifest first, and
# npm-normalize-package-bin/lib/index.js:normalizeObject keeps the basename of
# the key with '\' and ':' read as '/'.
mkdir -p "$prefix/bin"
printf '%s\n' "$UPDATE_BINS" | while IFS= read -r line; do
  [ -n "$line" ] || continue
  key=${line%%=*}
  file=${line#*=}
  name=$(printf '%s' "$key" | tr '\\:' '//' | sed 's|.*/||')
  [ -n "$name" ] || continue
  mkdir -p "$(dirname "$package/$file")"
  printf '%s' "$UPDATE_COMMAND" > "$package/$file"
  chmod 700 "$package/$file"
  ln -sfn "../lib/node_modules/dimcode-alpha/$file" "$prefix/bin/$name"
done
if [ -n "$UPDATE_NATIVE" ]; then
  mkdir -p "$package/bin"
  cp "$UPDATE_NATIVE" "$package/bin/dimcode"
  chmod 700 "$package/bin/dimcode"
fi
rm -rf "$retired"
echo 'Installed package'
"#;

#[test]
fn an_interrupted_staged_update_keeps_the_installed_command() {
    for dies in [true, false] {
        let case = if dies {
            "the manager exits at its gate"
        } else {
            "SIGTERM while the manager waits"
        };
        let installed = Installed::new(CURRENT, &["dima"]);
        installed.mock_npm();
        let mut command = installed.update(
            &next_version(),
            &[("dima", "bin/dimcode.mjs")],
            &[
                ("UPDATE_COMMAND", REPORTS_VERSION),
                ("UPDATE_FAIL", if dies { "1" } else { "0" }),
            ],
        );
        let child = OwnedChild::spawn(
            command
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped()),
        );
        installed.until_waiting();
        assert_eq!(
            installed.try_version("dima").as_deref(),
            Ok(CURRENT),
            "{case}: the installed command must run while the manager retires and rewrites its package"
        );
        if !dies {
            assert_eq!(unsafe { libc::kill(child.id() as i32, libc::SIGTERM) }, 0);
        }
        installed.release();
        let output = child.wait();
        let stderr = String::from_utf8_lossy_owned(output.stderr);
        assert!(!output.status.success(), "{case}: {stderr}");
        if !dies {
            assert!(stderr.contains("Update interrupted"), "{case}: {stderr}");
        }
        assert!(
            !stderr.contains("Updated "),
            "{case}: an interrupted update must not report success: {stderr}"
        );
        assert_eq!(
            installed.installed(),
            CURRENT,
            "{case}: the installed command must still report the old version"
        );
        assert!(
            !installed.staging().exists(),
            "{case}: an interrupted update must clear its staging directory"
        );
        // An update that dies before it can clean up leaves staging behind, and
        // the next update clears it while it holds the exclusive lease.
        fs::create_dir_all(installed.staging().join("staging-1/lib/node_modules")).unwrap();
        installed.release();
        let output = installed
            .update(
                &next_version(),
                &[("dima", "bin/dimcode.mjs")],
                &[("UPDATE_COMMAND", REPORTS_VERSION), ("UPDATE_FAIL", "1")],
            )
            .output()
            .unwrap();
        assert!(!output.status.success(), "{case}: {output:?}");
        assert!(
            !installed.staging().exists(),
            "{case}: the next update must clear what an earlier one left"
        );
    }
}

#[test]
fn an_update_that_installs_nothing_keeps_the_installed_command() {
    for (case, version) in [
        ("it exits without writing", String::new()),
        ("it writes the installed version", CURRENT.to_owned()),
    ] {
        let installed = Installed::new(CURRENT, &["dima"]);
        installed.mock_npm();
        installed.release();
        let output = installed
            .update(
                &version,
                &[("dima", "bin/dimcode.mjs")],
                &[("UPDATE_COMMAND", REPORTS_VERSION)],
            )
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy_owned(output.stderr);
        assert!(!output.status.success(), "{case}: {stderr}");
        assert!(!stderr.contains("Updated "), "{case}: {stderr}");
        assert!(
            !stderr.contains("Installed package"),
            "{case}: the manager's output stays captured: {stderr}"
        );
        assert_eq!(
            installed.installed(),
            CURRENT,
            "{case}: the installed command must be unchanged"
        );
        assert!(
            !installed.staging().exists(),
            "{case}: a failed update must clear its staging directory"
        );
    }
}

#[test]
fn a_staged_update_switches_the_package_its_commands_and_the_global_config() {
    let installed = Installed::new(CURRENT, &["dima", "retired"]);
    installed.declare(&[("dima", "bin/dimcode.mjs"), ("retired", "bin/dimcode.mjs")]);
    installed.mock_npm();
    installed.release();
    let version = next_version();
    let output = installed
        .update(
            &version,
            &[("dima", "bin/dimcode.mjs"), ("added", "bin/dimcode.mjs")],
            &[("UPDATE_COMMAND", REPORTS_VERSION)],
        )
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy_owned(output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert!(stderr.contains("Updated dimcode-alpha"), "{stderr}");
    assert_eq!(installed.installed(), version);
    assert_eq!(
        installed.version("added"),
        version,
        "a command the new version installs must run"
    );
    assert!(
        !installed.prefix.join("bin/retired").exists(),
        "a command only the old version installed must be gone"
    );
    assert!(
        !installed.staging().exists(),
        "a finished update must leave no staging directory"
    );
    let args = installed.recorded();
    let value = |flag: &str| {
        args.iter()
            .position(|arg| arg == flag)
            .map(|index| args[index + 1].trim_start().to_owned())
    };
    assert!(
        value("--globalconfig")
            .is_some_and(|found| Path::new(&found) == installed.prefix.join("etc/npmrc")),
        "the staged install must read the live global npm configuration: {args:?}"
    );
    let prefix = value("--prefix").unwrap();
    assert!(
        prefix.starts_with(&format!("{}/.dimcode-staging/", installed.prefix.display())),
        "the manager must install into staging, not the live prefix: {args:?}"
    );
    assert_eq!(
        args.last().map(String::as_str),
        Some("dimcode-alpha@alpha"),
        "{args:?}"
    );
}

#[test]
fn a_staged_update_whose_new_command_fails_puts_the_previous_version_back() {
    let installed = Installed::new(CURRENT, &["dima"]);
    installed.mock_npm();
    installed.release();
    let output = installed
        .update(
            &next_version(),
            &[("dima", "bin/dimcode.mjs")],
            &[("UPDATE_COMMAND", BREAKS)],
        )
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy_owned(output.stderr);
    assert!(!output.status.success(), "{stderr}");
    assert!(!stderr.contains("Updated "), "{stderr}");
    assert_eq!(
        installed.installed(),
        CURRENT,
        "an update whose new command does not run must put the previous version back"
    );
    assert!(!installed.staging().exists(), "{stderr}");
}

#[test]
fn a_command_the_manager_renames_takes_the_name_of_its_link() {
    let installed = Installed::new(CURRENT, &["dima"]);
    installed.mock_npm();
    installed.release();
    let version = next_version();
    let output = installed
        .update(
            &version,
            &[
                ("dima", "bin/dimcode.mjs"),
                ("../bin2/escape", "bin/dimcode.mjs"),
            ],
            &[("UPDATE_COMMAND", REPORTS_VERSION)],
        )
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy_owned(output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert_eq!(
        installed.version("escape"),
        version,
        "the command the manager linked is the one that must run: {stderr}"
    );
    assert!(
        !installed.prefix.join("bin2").exists(),
        "every command must stay directly inside the bin directory: {stderr}"
    );
}

#[test]
fn a_command_another_package_owns_is_never_taken() {
    let installed = Installed::new(CURRENT, &["dima"]);
    installed.mock_npm();
    installed.release();
    let other = installed.prefix.join("lib/node_modules/other-cli");
    fs::create_dir_all(other.join("bin")).unwrap();
    let launcher = other.join("bin/other.mjs");
    fs::write(&launcher, "#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(&launcher, fs::Permissions::from_mode(0o700)).unwrap();
    let shared = installed.prefix.join("bin/shared-cli");
    std::os::unix::fs::symlink("../lib/node_modules/other-cli/bin/other.mjs", &shared).unwrap();
    let output = installed
        .update(
            &next_version(),
            &[
                ("dima", "bin/dimcode.mjs"),
                ("shared-cli", "bin/dimcode.mjs"),
            ],
            &[("UPDATE_COMMAND", REPORTS_VERSION)],
        )
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy_owned(output.stderr);
    assert!(!output.status.success(), "{stderr}");
    assert!(
        stderr.contains("shared-cli") && stderr.contains("other-cli"),
        "the failure must name the command and its owner: {stderr}"
    );
    assert!(!stderr.contains("Updated "), "{stderr}");
    assert_eq!(
        fs::read_link(&shared).unwrap(),
        Path::new("../lib/node_modules/other-cli/bin/other.mjs"),
        "a command another package owns must still point into it"
    );
    assert!(
        launcher.is_file(),
        "the other package's launcher must still be there"
    );
    assert_eq!(installed.installed(), CURRENT, "{stderr}");
    assert!(
        !installed.staging().exists(),
        "the refused update must leave no staging directory"
    );
}

#[test]
fn one_packages_staging_survives_the_others_update() {
    // A stable update interrupted in the two-rename window leaves its only copy
    // at its own staging root. An alpha update of the same prefix must not read
    // or remove it.
    let installed = Installed::new(CURRENT, &["dima"]);
    installed.mock_npm();
    installed.release();
    let stable = installed.staging().parent().unwrap().join("dimcode");
    let retired = stable.join("staging-4242/retired");
    fs::create_dir_all(retired.join("bin")).unwrap();
    fs::write(
        retired.join("package.json"),
        "{\"name\":\"dimcode\",\"version\":\"0.1.0-alpha.1\"}",
    )
    .unwrap();
    let payload = retired.join("bin/dim");
    fs::write(&payload, "stable payload\n").unwrap();
    let before = fs::read(&payload).unwrap();
    let version = next_version();
    let output = installed
        .update(
            &version,
            &[("dima", "bin/dimcode.mjs")],
            &[("UPDATE_COMMAND", REPORTS_VERSION)],
        )
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy_owned(output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert_eq!(installed.installed(), version, "{stderr}");
    assert_eq!(
        fs::read(&payload).ok().as_deref(),
        Some(&before[..]),
        "the other package's only copy must be byte-identical afterwards: {stderr}"
    );
    assert!(
        retired.is_dir(),
        "an alpha update must not remove the stable package's staging: {stderr}"
    );

    // The stable update with the same residue still refuses, and names the one
    // move that restores its package. It runs from a recognised installation
    // whose `dimcode` package is the one the residue holds, so the live package
    // is missing, as it is after the interrupted fallback.
    let stable_updater = installed.prefix.join("lib/node_modules/dimcode-alpha/bin");
    let updater = stable_updater.join("dim-stable");
    install_updater(&updater);
    let mut command = Command::new(fs::canonicalize(updater).unwrap());
    command
        .args(["--home", installed.home.0.to_str().unwrap(), "update"])
        .env("DIMCODE_NPM_PACKAGE", "dimcode")
        .env("UPDATE_ARGS", installed.home.0.join("install-args"))
        .env("UPDATE_VERSION", &version)
        .env(
            "UPDATE_PACKAGE_JSON",
            "{\"name\":\"dimcode\",\"version\":\"9.9.9\"}",
        )
        .env("UPDATE_BINS", "dim=bin/dimcode.mjs")
        .env("UPDATE_GATE", installed.home.0.join("gate"))
        .env("UPDATE_WAITING", installed.home.0.join("waiting"))
        .env("UPDATE_COMMAND", REPORTS_VERSION);
    let output = command.output().unwrap();
    let stderr = String::from_utf8_lossy_owned(output.stderr);
    assert!(!output.status.success(), "{stderr}");
    assert!(
        stderr.contains("An earlier update left dimcode at"),
        "{stderr}"
    );
    assert!(
        stderr.contains(&format!("mv {}", retired.display())),
        "the refusal must name the one move that restores the package: {stderr}"
    );
    assert_eq!(
        fs::read(&payload).ok().as_deref(),
        Some(&before[..]),
        "the refusal must keep the only copy: {stderr}"
    );
}

#[test]
fn an_interruption_during_the_post_switch_check_puts_the_previous_version_back() {
    let installed = Installed::new(CURRENT, &["dima"]);
    installed.mock_npm();
    installed.release();
    let checking = installed.home.0.join("checking");
    let mut command = installed.update(
        &next_version(),
        &[("dima", "bin/dimcode.mjs")],
        &[("UPDATE_COMMAND", SLOW_VERSION), ("UPDATE_SLOW", "1")],
    );
    command.env("UPDATE_CHECKING", &checking);
    let child = OwnedChild::spawn(
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    );
    let deadline = Instant::now() + HANG_GUARD;
    while !checking.exists() {
        assert!(
            Instant::now() < deadline,
            "the switched-in command never reached its check"
        );
        thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        unsafe { libc::kill(child.id() as i32, libc::SIGTERM) },
        0,
        "the flow must be able to interrupt the update while it checks"
    );
    let output = child.wait();
    let stderr = String::from_utf8_lossy_owned(output.stderr);
    assert!(!output.status.success(), "{stderr}");
    assert!(
        stderr.contains("interrupted while checking"),
        "an interrupted check must say so: {stderr}"
    );
    assert!(
        !stderr.contains("Updated "),
        "an interrupted check must not report success: {stderr}"
    );
    assert_eq!(
        installed.installed(),
        CURRENT,
        "the previous version must be live again: {stderr}"
    );
    assert!(
        !installed.staging().exists(),
        "an interrupted check must clear its staging directory: {stderr}"
    );
}

#[test]
fn an_alias_the_package_never_declared_survives_an_update() {
    let installed = Installed::new(CURRENT, &["dima"]);
    installed.mock_npm();
    installed.release();
    let alias = installed.prefix.join("bin/mydima");
    std::os::unix::fs::symlink("../lib/node_modules/dimcode-alpha/bin/dimcode.mjs", &alias)
        .unwrap();
    let version = next_version();
    let output = installed
        .update(
            &version,
            &[("dima", "bin/dimcode.mjs")],
            &[("UPDATE_COMMAND", REPORTS_VERSION)],
        )
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy_owned(output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert!(
        alias.is_symlink(),
        "an alias the package never declared is not the update's to remove: {stderr}"
    );
    assert_eq!(
        installed.version("mydima"),
        version,
        "the alias must run the new version: {stderr}"
    );
}

#[test]
fn a_declared_command_the_new_version_drops_is_retired() {
    let installed = Installed::new(CURRENT, &["dima", "dropped"]);
    installed.declare(&[("dima", "bin/dimcode.mjs"), ("dropped", "bin/dimcode.mjs")]);
    installed.mock_npm();
    installed.release();
    let output = installed
        .update(
            &next_version(),
            &[("dima", "bin/dimcode.mjs")],
            &[("UPDATE_COMMAND", REPORTS_VERSION)],
        )
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy_owned(output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert!(
        !installed.prefix.join("bin/dropped").exists(),
        "a command the previous version declared and the new one drops must be retired: {stderr}"
    );
}

#[test]
fn a_live_manifest_that_cannot_be_read_retires_nothing_and_the_update_goes_on() {
    let installed = Installed::new(CURRENT, &["dima", "dropped"]);
    installed.mock_npm();
    installed.release();
    let manifest = installed
        .prefix
        .join("lib/node_modules/dimcode-alpha/package.json");
    fs::write(&manifest, "{").unwrap();
    let version = next_version();
    let output = installed
        .update(
            &version,
            &[("dima", "bin/dimcode.mjs")],
            &[("UPDATE_COMMAND", REPORTS_VERSION)],
        )
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy_owned(output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert!(
        stderr.contains("Updated dimcode-alpha"),
        "a live manifest the update cannot read must not fail it: {stderr}"
    );
    assert!(
        stderr.contains("no command was retired"),
        "the update must say what it left alone: {stderr}"
    );
    assert_eq!(
        installed.installed(),
        version,
        "the new version must be live and its command must run it: {stderr}"
    );
    assert!(
        fs::symlink_metadata(installed.prefix.join("bin/dropped")).is_ok(),
        "nothing may be retired when the previous commands are unknown: {stderr}"
    );
}

/// A command that starts a child which records its pid and then, a second later,
/// writes a marker if it is still alive. The command waits on it long past any
/// bound the update has, so a flow can check that nothing the check started
/// outlives the update.
const CHECK_WITH_CHILD: &str = r#"#!/bin/sh
target=$0
while [ -L "$target" ]; do
  link=$(readlink "$target")
  case $link in
    /*) target=$link ;;
    *) target=$(dirname "$target")/$link ;;
  esac
done
directory=$(cd "$(dirname "$target")" && pwd) || exit 1
while [ ! -f "$directory/package.json" ] && [ "$directory" != / ]; do directory=$(dirname "$directory"); done
if [ -n "$UPDATE_SLOW" ]; then
  printf 'checking %s\n' "$directory" >> "$UPDATE_CHECKING"
  # The loop ends only at this test's own `Home` drop; see the gate loop in MOCK_NPM.
  sh -c 'echo $$ > "$UPDATE_CHECK_CHILD"; sleep 1; echo survived > "$UPDATE_CHECK_MARKER"; home=${UPDATE_CHECK_MARKER%/*}; while [ -d "$home" ]; do sleep 0.2; done' &
  wait
fi
sed -n 's/.*"version":"\([^"]*\)".*/\1/p' "$directory/package.json"
"#;

#[test]
fn an_interrupted_check_ends_every_process_it_started() {
    let installed = Installed::new(CURRENT, &["dima"]);
    installed.mock_npm();
    installed.release();
    let checking = installed.home.0.join("checking");
    let started = installed.home.0.join("check-child");
    let marker = installed.home.0.join("check-marker");
    let mut command = installed.update(
        &next_version(),
        &[("dima", "bin/dimcode.mjs")],
        &[("UPDATE_COMMAND", CHECK_WITH_CHILD), ("UPDATE_SLOW", "1")],
    );
    command
        .env("UPDATE_CHECKING", &checking)
        .env("UPDATE_CHECK_CHILD", &started)
        .env("UPDATE_CHECK_MARKER", &marker);
    let child = OwnedChild::spawn(
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    );
    let started = recorded_pid(
        &started,
        "the pid the checked command's child records",
        HANG_GUARD,
    );
    assert_eq!(
        unsafe { libc::kill(child.id() as i32, libc::SIGTERM) },
        0,
        "the flow must be able to interrupt the update while it checks"
    );
    let output = child.wait();
    let stderr = String::from_utf8_lossy_owned(output.stderr);
    assert!(!output.status.success(), "{stderr}");
    assert!(!stderr.contains("Updated "), "{stderr}");
    assert_eq!(
        unsafe { libc::kill(started, 0) },
        -1,
        "the process the check started, {started}, must be gone before the update exits: {stderr}"
    );
    assert_eq!(
        installed.installed(),
        CURRENT,
        "the previous version must be live again: {stderr}"
    );
    // The child writes its marker a second after it started, so a survivor
    // would have written it by now.
    thread::sleep(Duration::from_millis(2000));
    assert!(
        !marker.exists(),
        "nothing the check started may write after the update exited: {stderr}"
    );
}

#[cfg(all(debug_assertions, any(target_os = "macos", target_os = "linux")))]
fn target_platform() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Some("darwin-arm64"),
        ("macos", "x86_64") => Some("darwin-x64"),
        ("linux", "aarch64") => Some("linux-arm64"),
        ("linux", "x86_64") => Some("linux-x64"),
        _ => None,
    }
}

/// Startup recovery when the store format is newer than this daemon. Uses the
/// debug-only `DIM_TEST_RELEASE_BUILD=1` override so these flows run against
/// the already-compiled debug binary without recompiling the workspace.
#[cfg(all(debug_assertions, any(target_os = "macos", target_os = "linux")))]
mod recovery {
    use super::*;

    fn assert_newer_store_recovery(declared: Option<u32>, retry: bool, second_failure: bool) {
        let Some(platform) = target_platform() else {
            return;
        };
        let version = next_version();
        let future = dim_app_server::storage::STORE_FORMAT + 1;
        let installed = Installed::new(CURRENT, &["dima"]);
        installed.mock_npm();
        installed.release();
        let home = Home::new();
        let database = home.0.join("rust/dimcode.sqlite");
        rusqlite::Connection::open(&database)
            .unwrap()
            .execute_batch(&format!(
                "CREATE TABLE agent_store_format (version INTEGER NOT NULL PRIMARY KEY CHECK (version = {future}));
                 INSERT INTO agent_store_format VALUES ({future});"
            ))
            .unwrap();
        let original = fs::read(&database).unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let registry = format!("http://{}", listener.local_addr().unwrap());
        let reply = json!({ "alpha": version }).to_string();
        let registry = if retry {
            drop(listener);
            registry
        } else {
            thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                stream.set_read_timeout(Some(HANG_GUARD)).unwrap();
                let mut request = String::new();
                BufReader::new(&stream).read_line(&mut request).unwrap();
                assert!(request.starts_with("GET /-/package/dimcode-alpha/dist-tags "));
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                    reply.len()
                )
                .unwrap();
            });
            registry
        };
        let cwd = home.0.join("workspace with spaces");
        fs::create_dir(&cwd).unwrap();
        let launcher = r#"#!/bin/sh
    if [ "$1" = "--version" ]; then printf 'dimcode %s\n' "$UPDATE_VERSION"; exit 0; fi
    printf '%s\n' "$DIMCODE_STARTUP_RETRY" "$PWD" "$@" > "$RECOVERY_RESTART"
    if [ "$RECOVERY_SECOND_FAILURE" = 1 ]; then exec "$RECOVERY_UPDATER" "$@"; fi
    printf 'Replacement launcher started\n'
    "#;
        let marker = home.0.join("restart");
        let mut command = Command::new(&installed.updater);
        command
            .current_dir(&cwd)
            .arg("--home")
            .arg(&home.0)
            .args(["tui", "-p", "original message"])
            .env_remove("DIMCODE_DISABLE_AUTOUPDATE")
            .env_remove("DIMCODE_AUTOUPDATE")
            .env_remove("DIMCODE_UPDATE_CHECK")
            .env_remove("DIMCODE_STARTUP_RETRY")
            .env("DIM_TEST_RELEASE_BUILD", "1")
            .env("DIM_TEST_UPDATE_TIMEOUT_MS", "60000")
            .env("DIM_TEST_DAEMON_START_TIMEOUT_MS", "60000")
            .env("DIM_TEST_APP_SERVER_STARTUP_TIMEOUT_MS", "60000")
            .env("DIMCODE_NPM_PACKAGE", "dimcode-alpha")
            .env("DIMCODE_NPM_REGISTRY", registry)
            .env("NO_PROXY", "127.0.0.1")
            .env("RECOVERY_RESTART", &marker)
            .env(
                "RECOVERY_SECOND_FAILURE",
                if second_failure { "1" } else { "0" },
            )
            .env("RECOVERY_UPDATER", &installed.updater)
            .env("UPDATE_NATIVE", "")
            .env("UPDATE_PLATFORM_PACKAGE", format!("dimcode-{platform}"))
            .env("UPDATE_VERSION", &version)
            .env("UPDATE_ARGS", installed.home.0.join("install-args"))
            .env("UPDATE_GATE", installed.home.0.join("gate"))
            .env("UPDATE_WAITING", installed.home.0.join("waiting"))
            .env(
                "UPDATE_PACKAGE_JSON",
                package_json(&version, &[("dima", "bin/dimcode.mjs")]),
            )
            .env("UPDATE_BINS", "dima=bin/dimcode.mjs")
            .env("UPDATE_COMMAND", launcher)
            .env(
                "UPDATE_RELEASE_MANIFEST",
                declared.map_or_else(String::new, |declared| {
                    json!({
                        "schemaVersion": 1, "platform": platform, "version": version,
                        "storeFormats": [declared]
                    })
                    .to_string()
                }),
            );
        if retry {
            command.env("DIMCODE_STARTUP_RETRY", "1");
        }
        if second_failure {
            command.env("UPDATE_NATIVE", &installed.updater);
        }
        let mut terminal = Terminal::launch_tui(command);
        let status = terminal.wait();
        let output = strip_ansi(&terminal.transcript);
        assert!(
            output.contains(&format!("Store format: v{future}")),
            "{output}"
        );
        assert!(!output.contains("store-newer"), "{output}");
        assert_eq!(fs::read(&database).unwrap(), original);
        assert!(!installed.staging().exists());
        let switched = declared == Some(future) && !retry;
        assert_eq!(status.success(), switched && !second_failure, "{output}");
        if switched {
            assert_eq!(installed.recorded().last().unwrap(), "dimcode-alpha@alpha");
            assert_eq!(
                output.matches("Checking dimcode-alpha").count(),
                1,
                "{output}"
            );
            if !second_failure {
                assert!(output.contains("Replacement launcher started"), "{output}");
            }
            assert_eq!(
                fs::read_to_string(marker).unwrap(),
                format!(
                    "1\n{}\n--home\n{}\ntui\n-p\noriginal message\n",
                    fs::canonicalize(cwd).unwrap().display(),
                    home.0.display()
                )
            );
        } else {
            assert_eq!(installed.installed(), CURRENT);
            assert!(!marker.exists());
            if retry {
                assert!(!output.contains("Checking dimcode-alpha"), "{output}");
            } else if declared.is_some() {
                assert!(
                    output.contains(&format!(
                        "{version} does not support native store v{future}"
                    )),
                    "{output}"
                );
            } else {
                assert!(
                    output.contains(&format!(
                        "{version} does not declare support for native store v{future}"
                    )),
                    "{output}"
                );
            }
        }
    }

    #[test]
    fn a_newer_store_recovers_with_a_compatible_staged_release() {
        assert_newer_store_recovery(
            Some(dim_app_server::storage::STORE_FORMAT + 1),
            false,
            false,
        );
    }

    #[test]
    fn a_newer_store_refuses_a_staged_release_for_an_older_store_format() {
        assert_newer_store_recovery(Some(dim_app_server::storage::STORE_FORMAT), false, false);
    }

    #[test]
    fn a_newer_store_refuses_a_staged_release_without_declared_store_formats() {
        assert_newer_store_recovery(None, false, false);
    }

    #[test]
    fn a_newer_store_does_not_retry_recovery_when_already_restarted() {
        assert_newer_store_recovery(Some(dim_app_server::storage::STORE_FORMAT + 1), true, false);
    }

    #[test]
    fn a_newer_store_stops_if_replacement_launcher_fails_again() {
        assert_newer_store_recovery(Some(dim_app_server::storage::STORE_FORMAT + 1), false, true);
    }

    #[test]
    fn a_store_this_build_cannot_read_never_recovers() {
        let installed = Installed::new(CURRENT, &["dima"]);
        installed.mock_npm();
        installed.release();
        let home = Home::new();
        let database = home.0.join("rust/dimcode.sqlite");
        rusqlite::Connection::open(&database)
            .unwrap()
            .execute_batch(
                "CREATE TABLE agent_store_format (version INTEGER NOT NULL PRIMARY KEY CHECK (version = 14));
                 INSERT INTO agent_store_format VALUES (14);",
            )
            .unwrap();
        let original = fs::read(&database).unwrap();
        let mut command = Command::new(&installed.updater);
        command
            .arg("--home")
            .arg(&home.0)
            .args(["tui", "-p", "original message"])
            .env_remove("DIMCODE_DISABLE_AUTOUPDATE")
            .env_remove("DIMCODE_AUTOUPDATE")
            .env_remove("DIMCODE_UPDATE_CHECK")
            .env_remove("DIMCODE_STARTUP_RETRY")
            .env("DIM_TEST_RELEASE_BUILD", "1")
            .env("DIM_TEST_UPDATE_TIMEOUT_MS", "60000")
            .env("DIM_TEST_DAEMON_START_TIMEOUT_MS", "60000")
            .env("DIM_TEST_APP_SERVER_STARTUP_TIMEOUT_MS", "60000")
            .env("DIMCODE_NPM_PACKAGE", "dimcode-alpha")
            .env("DIMCODE_NPM_REGISTRY", "http://127.0.0.1:1")
            .env("UPDATE_ARGS", installed.home.0.join("install-args"))
            .env("UPDATE_GATE", installed.home.0.join("gate"))
            .env("UPDATE_WAITING", installed.home.0.join("waiting"));
        let mut terminal = Terminal::launch_tui(command);
        let status = terminal.wait();
        let output = strip_ansi(&terminal.transcript);
        assert!(!status.success(), "{output}");
        assert!(
            output.contains("Store format: v14 (no longer read by this daemon)"),
            "{output}"
        );
        assert!(
            output.contains("This daemon cannot read the store. Existing data was preserved."),
            "{output}"
        );
        assert!(
            !output.contains("Checking dimcode-alpha"),
            "a store this build cannot read must not start a recovery: {output}"
        );
        assert!(!output.contains("store-newer"), "{output}");
        assert_eq!(installed.installed(), CURRENT);
        assert_eq!(fs::read(&database).unwrap(), original);
        assert!(!installed.staging().exists());
    }

    #[test]
    fn a_recovery_never_installs_the_skipped_release() {
        let Some(platform) = target_platform() else {
            return;
        };
        // The lookup registry answers a version above the skipped one, and the
        // manager's own registry resolves the channel tag to the skipped one.
        let skipped = next_version();
        let looked_up = bump(&skipped);
        let format = dim_app_server::storage::STORE_FORMAT;
        let future = format + 1;
        let installed = Installed::new(CURRENT, &["dima"]);
        installed.mock_npm();
        installed.release();
        let home = Home::new();
        fs::write(
            home.0.join("upgrade-skip.json"),
            json!({ "version": skipped }).to_string(),
        )
        .unwrap();
        let database = home.0.join("rust/dimcode.sqlite");
        rusqlite::Connection::open(&database)
            .unwrap()
            .execute_batch(&format!(
                "CREATE TABLE agent_store_format (version INTEGER NOT NULL PRIMARY KEY CHECK (version = {future}));
                 INSERT INTO agent_store_format VALUES ({future});"
            ))
            .unwrap();
        let original = fs::read(&database).unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let registry = format!("http://{}", listener.local_addr().unwrap());
        let reply = json!({ "alpha": looked_up }).to_string();
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream.set_read_timeout(Some(HANG_GUARD)).unwrap();
            let mut request = String::new();
            BufReader::new(&stream).read_line(&mut request).unwrap();
            assert!(request.starts_with("GET /-/package/dimcode-alpha/dist-tags "));
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                reply.len()
            )
            .unwrap();
        });
        let launcher = r#"#!/bin/sh
    if [ "$1" = "--version" ]; then printf 'dimcode %s\n' "$UPDATE_VERSION"; exit 0; fi
    printf '%s\n' "$@" > "$RECOVERY_RESTART"
    printf 'Replacement launcher started\n'
    "#;
        let marker = home.0.join("restart");
        let mut command = Command::new(&installed.updater);
        command
            .arg("--home")
            .arg(&home.0)
            .args(["tui", "-p", "original message"])
            .env_remove("DIMCODE_DISABLE_AUTOUPDATE")
            .env_remove("DIMCODE_AUTOUPDATE")
            .env_remove("DIMCODE_UPDATE_CHECK")
            .env_remove("DIMCODE_STARTUP_RETRY")
            .env("DIM_TEST_RELEASE_BUILD", "1")
            .env("DIM_TEST_UPDATE_TIMEOUT_MS", "60000")
            .env("DIM_TEST_DAEMON_START_TIMEOUT_MS", "60000")
            .env("DIM_TEST_APP_SERVER_STARTUP_TIMEOUT_MS", "60000")
            .env("DIMCODE_NPM_PACKAGE", "dimcode-alpha")
            .env("DIMCODE_NPM_REGISTRY", registry)
            .env("NO_PROXY", "127.0.0.1")
            .env("RECOVERY_RESTART", &marker)
            .env("UPDATE_NATIVE", "")
            .env("UPDATE_PLATFORM_PACKAGE", format!("dimcode-{platform}"))
            .env("UPDATE_VERSION", &skipped)
            .env("UPDATE_ARGS", installed.home.0.join("install-args"))
            .env("UPDATE_GATE", installed.home.0.join("gate"))
            .env("UPDATE_WAITING", installed.home.0.join("waiting"))
            .env(
                "UPDATE_PACKAGE_JSON",
                package_json(&skipped, &[("dima", "bin/dimcode.mjs")]),
            )
            .env("UPDATE_BINS", "dima=bin/dimcode.mjs")
            .env("UPDATE_COMMAND", launcher)
            .env(
                "UPDATE_RELEASE_MANIFEST",
                json!({
                    "schemaVersion": 1, "platform": platform, "version": skipped,
                    "storeFormats": [future]
                })
                .to_string(),
            );
        let mut terminal = Terminal::launch_tui(command);
        let status = terminal.wait();
        let output = strip_ansi(&terminal.transcript);
        assert!(
            output.contains(&format!("Store format: v{future}")),
            "{output}"
        );
        assert!(!status.success(), "{output}");
        assert!(
            output.contains(&format!("{skipped} is marked as skipped")),
            "the recovery must report the skip: {output}"
        );
        assert_eq!(
            installed.installed(),
            CURRENT,
            "the skipped release must never be installed: {output}"
        );
        assert!(!marker.exists(), "nothing may restart: {output}");
        assert_eq!(fs::read(&database).unwrap(), original);
        assert!(
            !installed.staging().exists(),
            "a refused recovery must clear its staging directory: {output}"
        );
    }
}

/// `Update now` in the terminal UI. Uses the debug-only
/// `DIM_TEST_RELEASE_BUILD=1` override so these flows run against the
/// already-compiled debug binary without recompiling the workspace.
#[cfg(all(debug_assertions, any(target_os = "macos", target_os = "linux")))]
mod tui_update {
    use super::*;

    /// Leaving the interface, the CLI asks this home's daemon to stop only if
    /// it is idle. An idle daemon stops, the installation updates and the new
    /// version starts in the same mode on the same Session, without the first
    /// message or image the interface opened with. A busy daemon or Ctrl+C
    /// while the daemon is asked installs nothing, and every command the CLI
    /// prints to finish keeps the selected home. A daemon that cannot decide
    /// whether it is idle gets the manual command instead of `Update now`.
    fn assert_tui_update_case(case: &'static str) {
        let Some(platform) = target_platform() else {
            return;
        };
        let version = next_version();
        let installed = Installed::new(CURRENT, &["dima"]);
        installed.mock_npm();
        installed.release();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let registry = format!("http://{}", listener.local_addr().unwrap());
        let reply = json!({ "alpha": version }).to_string();
        thread::spawn(move || {
            for stream in listener.incoming() {
                let mut stream = stream.unwrap();
                let mut request = String::new();
                BufReader::new(&stream).read_line(&mut request).unwrap();
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                    reply.len()
                );
            }
        });
        let home = Home::new();
        let socket = home.0.join("rust/daemon.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        fs::set_permissions(&socket, fs::Permissions::from_mode(0o600)).unwrap();
        let (asked, shutdown_asked) = std::sync::mpsc::channel();
        let daemon = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            stream.set_read_timeout(Some(HANG_GUARD)).unwrap();
            let mut wire = Wire::new(stream);
            let shutdown = ["daemon.shutdown.completed", "daemon.shutdown.ifIdle"];
            wire.hello(if case == "old" {
                &shutdown[..1]
            } else {
                &shutdown
            });
            let attach = wire.read("session.attach");
            wire.reply(&attach, attach_record(&attach));
            let history = wire.read("session.history");
            wire.reply(
                &history,
                json!({"events":[],"through":8,"nextAfter":8,"done":true}),
            );
            wire.no_context();
            let providers = wire.read("account.providers");
            wire.reply(&providers, json!([]));
            while let Some(request) = wire.trailing_request() {
                let request: Value = serde_json::from_str(&request).unwrap();
                assert_ne!(request["method"], "daemon.shutdown", "{request}");
                wire.reply(&request, json!({}));
            }
            if case == "old" {
                return;
            }
            let (stream, _) = listener.accept().unwrap();
            stream.set_read_timeout(Some(HANG_GUARD)).unwrap();
            let mut wire = Wire::new(stream);
            wire.hello(&shutdown);
            while let Some(shutdown) = wire.trailing_request() {
                let shutdown: Value = serde_json::from_str(&shutdown).unwrap();
                assert_eq!(shutdown["method"], "daemon.shutdown");
                assert_eq!(shutdown["params"], json!({"ifIdle": true}));
                match case {
                    "idle" => {
                        wire.reply(&shutdown, json!({}));
                        return;
                    }
                    "cancelled" => {
                        asked.send(()).unwrap();
                        continue;
                    }
                    _ => {}
                }
                wire.send(json!({"jsonrpc":"2.0","id":shutdown["id"],"error":{
                    "code":-32000,"message":"The App Server is in use",
                    "data":{"appError":"daemon_busy","connections":1,"attachedSessions":1,"running":0}}}));
            }
        });
        let launcher = format!(
            r#"#!/bin/sh
if [ "$1" = "--version" ]; then printf 'dimcode {version}\n'; exit 0; fi
printf '%s\n' "$@" > "$UPDATE_RESTART"
printf 'Updated launcher started\n'
"#
        );
        let marker = home.0.join("restart");
        let shot = home.0.join("shot.png");
        fs::write(&shot, noise_png(4, 4)).unwrap();
        let mut command = Command::new(&installed.updater);
        command
            .args([
                "--home",
                home.0.to_str().unwrap(),
                "tui",
                "--resume",
                "session",
            ])
            .env_remove("DIMCODE_DISABLE_AUTOUPDATE")
            .env_remove("DIMCODE_AUTOUPDATE")
            .env_remove("DIMCODE_UPDATE_CHECK")
            .env("DIM_TEST_RELEASE_BUILD", "1")
            .env("DIM_TEST_UPDATE_TIMEOUT_MS", "60000")
            .env("DIM_TEST_DAEMON_START_TIMEOUT_MS", "60000")
            .env("DIM_TEST_APP_SERVER_STARTUP_TIMEOUT_MS", "60000")
            .env("DIMCODE_NPM_PACKAGE", "dimcode-alpha")
            .env("DIMCODE_NPM_REGISTRY", &registry)
            .env("NO_PROXY", "127.0.0.1")
            .env("UPDATE_RESTART", &marker)
            .env("UPDATE_PLATFORM_PACKAGE", format!("dimcode-{platform}"))
            .env("UPDATE_VERSION", &version)
            .env("UPDATE_ARGS", installed.home.0.join("install-args"))
            .env("UPDATE_GATE", installed.home.0.join("gate"))
            .env("UPDATE_WAITING", installed.home.0.join("waiting"))
            .env(
                "UPDATE_PACKAGE_JSON",
                package_json(&version, &[("dima", "bin/dimcode.mjs")]),
            )
            .env("UPDATE_BINS", "dima=bin/dimcode.mjs")
            .env("UPDATE_COMMAND", &launcher);
        if case == "idle" {
            command.args(["-p", "first message", "--image", shot.to_str().unwrap()]);
        }
        sized(&mut command, 100, 30);
        let mut terminal = Terminal::launch_tui(command);
        if case == "old" {
            terminal.until_plain("To update, exit DimCode and run: dima update");
            assert!(!strip_ansi(&terminal.transcript).contains("Update now"));
            terminal.write(b"\t\r");
            terminal.write(b"/exit\r");
            assert!(terminal.wait().success(), "{}", terminal.transcript);
            daemon.join().unwrap();
            assert!(!installed.home.0.join("install-args").exists());
            return;
        }
        terminal.until_plain("Update now");
        terminal.write(b"\r");
        if case == "cancelled" {
            shutdown_asked.recv_timeout(HANG_GUARD).unwrap();
            terminal.signal(libc::SIGINT);
        }
        let status = terminal.wait();
        daemon.join().unwrap();
        let output = strip_ansi(&terminal.transcript);
        let selected = format!("dima --home {}", home.0.display());
        if case != "idle" {
            assert!(!status.success(), "{output}");
            let blocker = if case == "busy" {
                "Other DimCode windows or tasks are using the server"
            } else {
                "Update cancelled."
            };
            assert!(output.contains(blocker), "{output}");
            assert!(output.contains(&format!("{selected} update")), "{output}");
            assert!(
                output.contains(&format!(
                    "Resume this session with: {selected} --resume session"
                )),
                "{output}"
            );
            assert!(!installed.home.0.join("install-args").exists());
            assert!(!marker.exists());
            assert_eq!(installed.installed(), CURRENT);
        } else {
            assert!(status.success(), "{output}");
            assert!(output.contains("Updated launcher started"), "{output}");
            assert_eq!(installed.installed(), version);
            assert_eq!(
                fs::read_to_string(&marker).unwrap(),
                format!(
                    "--home\n{}\ntui\n--scrollback\n--resume\nsession\n",
                    home.0.display()
                )
            );
        }
    }

    #[test]
    fn update_now_stops_an_idle_daemon_and_restarts_on_the_same_session() {
        assert_tui_update_case("idle");
    }

    #[test]
    fn update_now_refuses_a_busy_daemon_and_prints_manual_command() {
        assert_tui_update_case("busy");
    }

    #[test]
    fn update_now_falls_back_to_manual_command_for_an_old_daemon() {
        assert_tui_update_case("old");
    }

    #[test]
    fn update_now_cancels_cleanly_on_sigint_while_stopping_daemon() {
        assert_tui_update_case("cancelled");
    }

    /// With the real App Server, the interface's own leave ends its UI leaf
    /// while the input history is still being written. A history lock held
    /// by another process keeps that write waiting well past the exit budget:
    /// `Update now` still installs and starts the new version once, after the
    /// write lands, or, when the lock is never released, after a bounded wait
    /// that says the entry may not be saved.
    fn assert_history_lock_update(released: bool) {
        let Some(platform) = target_platform() else {
            return;
        };
        let version = next_version();
        let installed = Installed::new(CURRENT, &["dima"]);
        installed.mock_npm();
        installed.release();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let registry = format!("http://{}", listener.local_addr().unwrap());
        let reply = json!({ "alpha": version }).to_string();
        thread::spawn(move || {
            for stream in listener.incoming() {
                let mut stream = stream.unwrap();
                let mut request = String::new();
                BufReader::new(&stream).read_line(&mut request).unwrap();
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                    reply.len()
                );
            }
        });
        let home = Home::new();
        let config = home.0.join("model.json");
        fs::write(
            &config,
            include_str!("../../../dim-agent-aimux/examples/offline.json"),
        )
        .unwrap();
        let history_lock = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(home.0.join("input-history.json.lock"))
            .unwrap();
        history_lock.lock().unwrap();
        let launcher = format!(
            r#"#!/bin/sh
    if [ "$1" = "--version" ]; then printf 'dimcode {version}\n'; exit 0; fi
    printf '%s\n' "$@" >> "$UPDATE_RESTART"
    printf 'Updated launcher started\n'
    "#
        );
        let marker = home.0.join("restart");
        let mut command = Command::new(&installed.updater);
        command
            .args(["--home", home.0.to_str().unwrap(), "--model-config"])
            .arg(&config)
            .arg("tui")
            .env_remove("DIMCODE_DISABLE_AUTOUPDATE")
            .env_remove("DIMCODE_AUTOUPDATE")
            .env_remove("DIMCODE_UPDATE_CHECK")
            .env("DIM_TEST_RELEASE_BUILD", "1")
            .env("DIM_TEST_UPDATE_TIMEOUT_MS", "60000")
            .env("DIM_TEST_DAEMON_START_TIMEOUT_MS", "60000")
            .env("DIM_TEST_APP_SERVER_STARTUP_TIMEOUT_MS", "60000")
            .env("DIMCODE_NPM_PACKAGE", "dimcode-alpha")
            .env("DIMCODE_NPM_REGISTRY", &registry)
            .env("NO_PROXY", "127.0.0.1")
            .env("UPDATE_RESTART", &marker)
            .env("UPDATE_PLATFORM_PACKAGE", format!("dimcode-{platform}"))
            .env("UPDATE_VERSION", &version)
            .env("UPDATE_ARGS", installed.home.0.join("install-args"))
            .env("UPDATE_GATE", installed.home.0.join("gate"))
            .env("UPDATE_WAITING", installed.home.0.join("waiting"))
            .env(
                "UPDATE_PACKAGE_JSON",
                package_json(&version, &[("dima", "bin/dimcode.mjs")]),
            )
            .env("UPDATE_BINS", "dima=bin/dimcode.mjs")
            .env("UPDATE_COMMAND", &launcher);
        sized(&mut command, 100, 30);
        let mut terminal = Terminal::launch_tui(command);
        terminal.until_plain("Update now");
        terminal.write(b"\t\t\r");
        terminal.until_plain("Ask anything");
        terminal.write(b"hello");
        terminal.until_plain("hello");
        terminal.write(b"\r");
        terminal.until_plain("Retrying");
        terminal.write(b"\x1b");
        terminal.until_plain("stopped");
        terminal.write(b"/update\r");
        terminal.until_plain("Update now");
        terminal.write(b"\r");
        terminal.until_plain("Updated dimcode-alpha on the alpha channel.");
        thread::sleep(Duration::from_secs(2));
        assert!(
            !marker.exists(),
            "the new version must not start before the history write lands"
        );
        if released {
            drop(history_lock);
        }
        let status = terminal.wait();
        let history = fs::read_to_string(home.0.join("input-history.json")).unwrap_or_default();
        let output = strip_ansi(&terminal.transcript);
        let unsaved = "Input history is still waiting for its lock";
        if released {
            assert!(history.contains("\"hello\""), "{history}");
            assert!(!output.contains(unsaved), "{output}");
        } else {
            assert!(!history.contains("\"hello\""), "{history}");
            assert!(output.contains(unsaved), "{output}");
        }
        assert!(status.success(), "{output}");
        assert_eq!(
            output.matches("Updated launcher started").count(),
            1,
            "{output}"
        );
        assert_eq!(installed.installed(), version);
        let restarted = fs::read_to_string(&marker).unwrap();
        assert!(restarted.contains("--resume\n"), "{restarted}");
    }

    #[test]
    fn update_now_waits_for_a_released_history_lock_before_restarting() {
        assert_history_lock_update(true);
    }

    #[test]
    fn update_now_restarts_after_a_bounded_wait_when_history_lock_stays_held() {
        assert_history_lock_update(false);
    }
}

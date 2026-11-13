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
    let (head, tail) = CURRENT.rsplit_once('.').unwrap();
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
directory=$(cd "$(dirname "$target")" && pwd)
while [ ! -f "$directory/package.json" ]; do directory=$(dirname "$directory"); done
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
directory=$(cd "$(dirname "$target")" && pwd)
while [ ! -f "$directory/package.json" ]; do directory=$(dirname "$directory"); done
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
        fs::copy(env!("CARGO_BIN_EXE_dimcode"), &updater).unwrap();
        fs::set_permissions(&updater, fs::Permissions::from_mode(0o700)).unwrap();
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
            .args([
                "--home",
                self.home.0.to_str().unwrap(),
                "update",
                "--confirm-legacy-exited",
            ])
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
        self.prefix.join(".dimcode-staging")
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
while [ ! -f "$UPDATE_GATE" ]; do sleep 0.1; done
if [ "$UPDATE_FAIL" = 1 ]; then exit 143; fi
if [ -z "$UPDATE_VERSION" ]; then echo 'Installed package'; exit 0; fi
mkdir -p "$package/node_modules/dimcode-darwin-arm64/bin"
printf '%s' "$UPDATE_COMMAND" > "$package/node_modules/dimcode-darwin-arm64/bin/dimcode"
chmod 700 "$package/node_modules/dimcode-darwin-arm64/bin/dimcode"
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
        let child = command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
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
        let output = wait(child);
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
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
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
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
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
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
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
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
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
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
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
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
    let child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
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
    let output = wait(child);
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
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

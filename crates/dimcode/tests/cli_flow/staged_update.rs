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

fn package_json(version: &str, bins: &[&str]) -> String {
    let map = bins
        .iter()
        .map(|command| format!("\"{command}\":\"bin/dimcode.mjs\""))
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

/// A global npm installation of `dimcode-alpha` at `version`.
struct Installed {
    updater: PathBuf,
    prefix: PathBuf,
    home: Home,
}

impl Installed {
    fn new(version: &str, bins: &[&str]) -> Self {
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
        fs::write(package.join("package.json"), package_json(version, bins)).unwrap();
        for command in bins {
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
    fn update(&self, version: &str, bins: &[&str], settings: &[(&str, &str)]) -> Command {
        let mut command = Command::new(&self.updater);
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
/// and writes the new package. `UPDATE_FAIL` exits before writing anything, an
/// empty `UPDATE_VERSION` exits 0 without writing, and `UPDATE_BREAK` writes a
/// command that exits nonzero.
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
if [ -L "$prefix/bin/dima" ]; then mv "$prefix/bin/dima" "$prefix/bin/.dima.retired"; fi
: > "$UPDATE_WAITING"
while [ ! -f "$UPDATE_GATE" ]; do sleep 0.1; done
if [ "$UPDATE_FAIL" = 1 ]; then exit 143; fi
if [ -z "$UPDATE_VERSION" ]; then echo 'Installed package'; exit 0; fi
payload="$package/node_modules/${UPDATE_PLATFORM_PACKAGE:-dimcode-darwin-arm64}"
mkdir -p "$package/bin" "$payload/bin"
if [ "$UPDATE_BREAK" = 1 ]; then
  printf '#!/bin/sh\nexit 1\n' > "$package/bin/dimcode.mjs"
  printf '#!/bin/sh\nexit 1\n' > "$payload/bin/dimcode"
else
  printf '%s' "$UPDATE_COMMAND" > "$package/bin/dimcode.mjs"
  printf '%s' "$UPDATE_COMMAND" > "$payload/bin/dimcode"
fi
chmod 700 "$package/bin/dimcode.mjs" "$payload/bin/dimcode"
if [ -n "$UPDATE_NATIVE" ]; then cp "$UPDATE_NATIVE" "$package/bin/dimcode"; fi
printf '%s\n' "$UPDATE_PACKAGE_JSON" > "$package/package.json"
if [ -n "$UPDATE_RELEASE_MANIFEST" ]; then
  printf '%s\n' "$UPDATE_RELEASE_MANIFEST" > "$payload/release-manifest.json"
fi
rm -rf "$retired" "$prefix/bin/.dima.retired"
echo 'Installed package'
"#;

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_newer_store_recovers_only_with_a_compatible_staged_release() {
    if !option_env!("DIM_RELEASE_BUILD").is_some_and(|mark| !mark.is_empty()) {
        return;
    }
    let platform = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "darwin-arm64",
        ("macos", "x86_64") => "darwin-x64",
        ("linux", "aarch64") => "linux-arm64",
        ("linux", "x86_64") => "linux-x64",
        _ => return,
    };
    let version = next_version();
    let format = dim_app_server::storage::STORE_FORMAT;
    let future = format + 1;
    for (baseline, declared, retry, second_failure) in [
        (true, Some(future), false, false),
        (true, Some(format), false, false),
        (true, None, false, false),
        (false, Some(future), false, false),
        (true, Some(future), true, false),
        (true, Some(future), false, true),
    ] {
        let installed = Installed::new(CURRENT, &["dima"]);
        installed.mock_npm();
        installed.release();
        if baseline {
            fs::write(installed.prefix.join(".dimcode-consumers"), "").unwrap();
        }
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
            .env("UPDATE_PACKAGE_JSON", package_json(&version, &["dima"]))
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
        let switched = baseline && declared == Some(future) && !retry;
        assert_eq!(status.success(), switched && !second_failure, "{output}");
        if switched {
            assert_eq!(
                installed.recorded().last().unwrap(),
                &format!("dimcode-alpha@{version}")
            );
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
            } else if !baseline {
                assert!(
                    output.contains("dima update --confirm-legacy-exited"),
                    "{output}"
                );
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
}

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
            &["dima"],
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
                &["dima"],
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
            .update(&version, &["dima"], &[("UPDATE_COMMAND", REPORTS_VERSION)])
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
            &["dima", "added"],
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
            &["dima"],
            &[("UPDATE_COMMAND", REPORTS_VERSION), ("UPDATE_BREAK", "1")],
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

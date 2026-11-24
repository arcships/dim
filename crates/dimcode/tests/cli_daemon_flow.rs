#![cfg(unix)]

#[path = "support/cli_harness.rs"]
mod harness;
#[allow(
    unused_imports,
    reason = "submodules re-import shared harness items via super::*"
)]
use harness::*;

#[path = "cli_flow/catalog.rs"]
mod catalog;
#[path = "cli_flow/context.rs"]
mod context;
#[path = "cli_flow/daemon_replace.rs"]
mod daemon_replace;
#[path = "cli_flow/daemon_startup_progress.rs"]
mod daemon_startup_progress;
#[path = "cli_flow/migration.rs"]
mod migration;
#[path = "cli_flow/modality.rs"]
mod modality;
#[path = "cli_flow/prompt_migration.rs"]
mod prompt_migration;
#[path = "cli_flow/protocol_version.rs"]
mod protocol_version;
#[path = "cli_flow/realtime.rs"]
mod realtime;
#[path = "cli_flow/report.rs"]
mod report;
#[path = "cli_flow/server.rs"]
mod server;
#[path = "cli_flow/skills.rs"]
mod skills;
#[path = "cli_flow/title.rs"]
mod title;
#[path = "cli_flow/update_entry.rs"]
mod update_entry;
#[path = "cli_flow/worktrees.rs"]
mod worktrees;

#[test]
fn empty_management_lists_explain_the_result() {
    for (area, method, reply, message) in [
        (
            "scheduled",
            "schedule.list",
            json!({"jobs":[]}),
            "No job is scheduled.\n",
        ),
        (
            "skills",
            "skills.catalog",
            json!({"version":1,"entries":[],"issues":[]}),
            "No Skill is configured.\n",
        ),
        (
            "hooks",
            "hooks.catalog",
            json!({"userRevision":"user:1","projectsRevision":"user:1",
                "disableAllHooks":false,"sources":[],"hooks":[]}),
            "No hook is configured.\n",
        ),
    ] {
        let home = Home::new();
        let daemon = home.daemon(move |wire| {
            wire.hello(&[method]);
            let request = wire.read(method);
            wire.reply(&request, reply);
        });
        let output = home.command().args([area, "list"]).output().unwrap();
        daemon.join().unwrap();
        assert!(output.status.success(), "{area}: {output:?}");
        assert!(output.stdout.is_empty(), "{area}: {output:?}");
        assert_eq!(String::from_utf8(output.stderr).unwrap(), message, "{area}");
    }
}

#[test]
fn help_and_version_do_not_create_product_state() {
    let home = Home::new();
    fs::remove_dir_all(home.0.join("rust")).unwrap();
    for flag in ["--help", "-h", "--version"] {
        let output = home
            .command()
            .env("DIM_EFFORT_LEVEL", " invalid ")
            .arg(flag)
            .output()
            .unwrap();
        assert!(output.status.success());
        if flag != "--version" {
            let help = String::from_utf8(output.stdout).unwrap();
            assert!(help.contains("--effort LEVEL"));
            assert!(help.contains("DIM_EFFORT_LEVEL"));
        }
    }
    assert!(!home.0.join("rust").exists());
}

#[test]
fn a_build_from_source_reports_its_version_as_source() {
    let home = Home::new();
    let printed = printed_label(&home);
    let prefix = format!("{}-source", env!("CARGO_PKG_VERSION"));
    assert!(
        printed == prefix
            || printed.strip_prefix(&prefix).is_some_and(|commit| {
                commit.strip_prefix('+').is_some_and(|commit| {
                    commit.len() == 8 && commit.chars().all(|c| c.is_ascii_hexdigit())
                })
            }),
        "a build the release flow did not stamp reports the committed version marked -source, \
         optionally naming the commit it was built from: {printed}"
    );
}

#[test]
fn update_uses_the_installed_package_and_prefix_without_a_daemon() {
    let home = Home::new();
    fs::remove_dir_all(home.0.join("rust")).unwrap();
    let prefix = home.0.join("node prefix");
    let executable = prefix.join("lib/node_modules/dimcode-alpha/bin/dimcode");
    fs::create_dir_all(executable.parent().unwrap()).unwrap();
    fs::copy(env!("CARGO_BIN_EXE_dimcode"), &executable).unwrap();
    let executable = fs::canonicalize(executable).unwrap();
    let prefix = fs::canonicalize(prefix).unwrap();
    fs::create_dir(prefix.join("bin")).unwrap();
    // The package the update replaces, shaped the way npm installs one: a
    // manifest naming the command, a command that reports its own version, and
    // a platform package beside it.
    let package = prefix.join("lib/node_modules/dimcode-alpha");
    fs::create_dir_all(package.join("node_modules/dimcode-darwin-arm64/bin")).unwrap();
    let report = package.join("bin/report");
    fs::write(&report, REPORTS_VERSION).unwrap();
    fs::set_permissions(&report, fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(
        package.join("package.json"),
        format!(
            "{{\"name\":\"dimcode-alpha\",\"version\":\"{}\",\"bin\":{{\"dima\":\"bin/report\"}}}}\n",
            env!("CARGO_PKG_VERSION")
        ),
    )
    .unwrap();
    std::os::unix::fs::symlink(
        "../lib/node_modules/dimcode-alpha/bin/report",
        prefix.join("bin/dima"),
    )
    .unwrap();
    let npm = prefix.join("bin/npm");
    fs::write(
        &npm,
        r#"#!/bin/sh
printf '%s\n' "$@" > "$UPDATE_ARGS"
if [ "$UPDATE_FAIL" = 1 ]; then
  echo 'fixture install failed' >&2
  exit 7
fi
echo 'Downloading package' >&2
if [ -n "$UPDATE_CHILD" ]; then
  echo $$ > "$UPDATE_PID"
  sh -c '
    if [ "$UPDATE_IGNORE_TERM" = 1 ]; then trap "" TERM; fi
    echo $$ > "$UPDATE_CHILD"
    while [ ! -f "$UPDATE_CONTINUE" ]; do sleep 0.1; done
    echo installed > "$UPDATE_MARKER"
  ' &
  wait
elif [ -n "$UPDATE_CONTINUE" ]; then
  while [ ! -f "$UPDATE_CONTINUE" ]; do sleep 0.1; done
fi
prefix=
previous=
for argument in "$@"; do
  if [ "$previous" = --prefix ]; then prefix=$argument; fi
  previous=$argument
done
for argument in "$@"; do
  case $argument in *@*) name=${argument%@*} ;; esac
done
live="$FIXTURE_PREFIX/lib/node_modules/dimcode-alpha"
staged="$prefix/lib/node_modules/$name"
current=$(sed -n 's/.*"version":"\([^"]*\)".*/\1/p' "$live/package.json")
number=$(printf '%s\n' "$current" | sed 's/.*\.//')
next=$(printf '%s\n' "$current" | sed "s/\.[0-9]*$/.$((number + 1))/")
mkdir -p "$(dirname "$staged")"
cp -R "$live" "$staged"
if [ "$name" = dimcode-alpha ]; then command=dima; else command=dim; fi
printf '{"name":"%s","version":"%s","bin":{"%s":"bin/report"}}\n' \
  "$name" "$next" "$command" > "$staged/package.json"
mkdir -p "$prefix/bin"
ln -sfn "../lib/node_modules/$name/bin/report" "$prefix/bin/$command"
echo 'Installed package'
"#,
    )
    .unwrap();
    fs::set_permissions(&npm, fs::Permissions::from_mode(0o700)).unwrap();
    let recorded = home.0.join("install-args");
    let tag = env!("CARGO_PKG_VERSION")
        .split_once('-')
        .map_or("latest", |(_, prerelease)| {
            prerelease.split('.').next().unwrap()
        });
    let refuse = |extra: &[&str]| {
        Command::new(&executable)
            .args(["--home", home.0.to_str().unwrap(), "update"])
            .args(extra)
            .env("DIMCODE_NPM_PACKAGE", "dimcode-alpha")
            .env("UPDATE_ARGS", &recorded)
            .env("FIXTURE_PREFIX", &prefix)
            .output()
            .unwrap()
    };
    let lease = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(prefix.join(".dimcode-update.lock"))
        .unwrap();
    lease.try_lock_shared().unwrap();
    let output = refuse(&[]);
    assert!(!output.status.success(), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("DimCode is currently running")
            && stderr.contains(&format!("`dima --home {} daemon stop`", home.0.display())),
        "{output:?}"
    );
    assert!(!recorded.exists(), "a live consumer must block the install");
    drop(lease);
    fs::set_permissions(
        prefix.join(".dimcode-update.lock"),
        fs::Permissions::from_mode(0o000),
    )
    .unwrap();
    let output = Command::new(&executable)
        .args(["--home", home.0.to_str().unwrap(), "--version"])
        .env("DIMCODE_NPM_PACKAGE", "dimcode-alpha")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "version is answered before any registration: {output:?}"
    );
    let output = Command::new(&executable)
        .args(["--home", home.0.to_str().unwrap(), "daemon", "stop"])
        .env("DIMCODE_NPM_PACKAGE", "dimcode-alpha")
        .output()
        .unwrap();
    assert!(!output.status.success(), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("Cannot register with installation"),
        "an unreadable lock must stop a recognised consumer: {output:?}"
    );
    fs::set_permissions(
        prefix.join(".dimcode-update.lock"),
        fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    let served = Home::new();
    let daemon_home = served.0.join("rust");
    daemon_cleanup::mark_test_server(&served.0);
    let mut daemon = Command::new(&executable)
        .arg("app-server")
        .arg("--unix")
        .arg(daemon_home.join("daemon.sock"))
        .args(["--no-ws", "--home"])
        .arg(&daemon_home)
        .arg("--product-home")
        .arg(&served.0)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let socket = daemon_home.join("daemon.sock");
    let deadline = Instant::now() + HANG_GUARD;
    while !socket.exists() {
        if daemon.try_wait().unwrap().is_some() {
            let mut stderr = String::new();
            std::io::Read::read_to_string(daemon.stderr.as_mut().unwrap(), &mut stderr).unwrap();
            panic!("the packaged App Server exited before it served: {stderr}");
        }
        assert!(
            Instant::now() < deadline,
            "the packaged App Server never served"
        );
        thread::sleep(Duration::from_millis(20));
    }
    let output = refuse(&[]);
    assert!(!output.status.success(), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("DimCode is currently running"),
        "a running packaged App Server must block the update: {output:?}"
    );
    assert!(!recorded.exists());
    daemon.kill().unwrap();
    daemon.wait().unwrap();
    let output = refuse(&[]);
    assert!(output.status.success(), "{output:?}");
    fs::remove_file(&recorded).unwrap();
    for (package, fail) in [
        ("dimcode-alpha", false),
        ("dimcode", false),
        ("dimcode-alpha", true),
    ] {
        let output = Command::new(&executable)
            .args(["--home", home.0.to_str().unwrap(), "update"])
            .env("DIMCODE_NPM_PACKAGE", package)
            .env("DIMCODE_UPDATE_CHECK", "0")
            .env("DIM_EFFORT_LEVEL", "invalid")
            .env("UPDATE_ARGS", &recorded)
            .env("FIXTURE_PREFIX", &prefix)
            .env("UPDATE_FAIL", if fail { "1" } else { "0" })
            .output()
            .unwrap();
        assert_eq!(output.status.success(), !fail, "{output:?}");
        let argv = fs::read_to_string(&recorded).unwrap();
        assert_eq!(
            argv.lines().take(3).collect::<Vec<_>>(),
            ["install", "-g", "--prefer-online"]
        );
        assert!(
            argv.lines()
                .any(|line| line.starts_with(&format!("{}/.dimcode-staging/", prefix.display()))),
            "the install must go into staging: {argv}"
        );
        assert!(
            argv.contains(&format!("--globalconfig\n{}/etc/npmrc\n", prefix.display())),
            "the staged install must read the live global configuration: {argv}"
        );
        assert!(
            argv.lines()
                .last()
                .is_some_and(|line| line.starts_with(&format!("{package}@"))),
            "{argv}"
        );
        if fail {
            assert!(String::from_utf8_lossy(&output.stderr).contains("fixture install failed"));
            assert!(!String::from_utf8_lossy(&output.stderr).contains("Updated "));
        } else {
            assert!(
                String::from_utf8_lossy(&output.stderr).contains(&format!("Updated {package}"))
            );
            assert!(!String::from_utf8_lossy(&output.stderr).contains("Installed package"));
        }
        assert!(String::from_utf8_lossy(&output.stderr).contains(&format!("Updating {package}")));
        assert!(output.stdout.is_empty());
        assert!(!output.stdout.contains(&0x1b));
        assert!(!output.stderr.contains(&0x1b));
    }
    fs::remove_file(&recorded).unwrap();
    for package in [
        "other-package",
        "dimcode-alpha@latest",
        "--ignore-scripts",
        "",
    ] {
        let output = Command::new(&executable)
            .args(["--home", home.0.to_str().unwrap(), "update"])
            .env("DIMCODE_NPM_PACKAGE", package)
            .env("UPDATE_ARGS", &recorded)
            .output()
            .unwrap();
        assert!(!output.status.success(), "{output:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains("DIMCODE_NPM_PACKAGE must be dimcode or dimcode-alpha"),
            "{output:?}"
        );
        assert!(!recorded.exists(), "an invalid package must not start npm");
    }
    let resume = home.0.join("continue-install");
    let launch = |command, width| {
        let terminal = Terminal::launch(command);
        let size = libc::winsize {
            ws_row: 24,
            ws_col: width,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        assert_eq!(
            unsafe {
                libc::ioctl(
                    terminal.master.as_ref().unwrap().as_raw_fd(),
                    libc::TIOCSWINSZ,
                    &size,
                )
            },
            0
        );
        terminal
    };
    for (width, fail) in [(80, false), (24, false), (80, true)] {
        let mut command = Command::new(&executable);
        command
            .args(["--home", home.0.to_str().unwrap(), "update"])
            .env("DIMCODE_NPM_PACKAGE", "dimcode-alpha")
            .env("UPDATE_ARGS", &recorded)
            .env("FIXTURE_PREFIX", &prefix)
            .env("UPDATE_CONTINUE", &resume)
            .env("UPDATE_FAIL", if fail { "1" } else { "0" });
        let mut terminal = launch(command, width);
        if !fail {
            terminal.until(&format!("({tag}) · 0s"));
            terminal.until(&format!("({tag}) · 1s"));
            assert!(
                !terminal.transcript.contains('\n'),
                "progress must stay on one row"
            );
            assert!(!terminal.transcript.contains("Downloading package"));
            if width == 80 {
                let other = Home::new();
                let other_install = other.0.join("install-args");
                let deadline = Instant::now() + HANG_GUARD;
                while !recorded.exists() {
                    assert!(
                        Instant::now() < deadline,
                        "the first installer never started"
                    );
                    thread::sleep(Duration::from_millis(10));
                }
                let output = Command::new(&executable)
                    .args(["--home", other.0.to_str().unwrap(), "update"])
                    .env("DIMCODE_NPM_PACKAGE", "dimcode")
                    .env("UPDATE_ARGS", &other_install)
                    .env("FIXTURE_PREFIX", &prefix)
                    .output()
                    .unwrap();
                assert!(!output.status.success(), "{output:?}");
                let stderr = String::from_utf8_lossy(&output.stderr);
                assert!(
                    stderr.contains("DimCode is currently running")
                        && stderr.contains(&format!(
                            "`dimcode --home {} daemon stop`",
                            other.0.display()
                        )),
                    "{output:?}"
                );
                assert!(
                    !other_install.exists(),
                    "the competing installer must not start"
                );
            }
            fs::write(&resume, "continue").unwrap();
        }
        let status = terminal.wait();
        assert_eq!(
            status.success(),
            !fail,
            "update (fail={fail}) reported {status}; terminal:\n{}",
            terminal.transcript
        );
        let outcome = if fail {
            "Error: fixture install failed"
        } else {
            "Updated dimcode-alpha"
        };
        assert!(
            terminal.transcript.contains(outcome),
            "{}",
            terminal.transcript
        );
        assert_eq!(terminal.transcript.matches('\n').count(), 1);
        if !fail {
            fs::remove_file(&resume).unwrap();
        }
    }
    let log = home.0.join("update.log");
    for redirect in ["stdout", "stderr"] {
        let mut command = Command::new("sh");
        command
            .args([
                "-c",
                if redirect == "stdout" {
                    "exec \"$@\" > \"$UPDATE_LOG\""
                } else {
                    "exec \"$@\" 2> \"$UPDATE_LOG\""
                },
                "sh",
            ])
            .arg(&executable)
            .args(["--home", home.0.to_str().unwrap(), "update"])
            .env("DIMCODE_NPM_PACKAGE", "dimcode-alpha")
            .env("UPDATE_ARGS", &recorded)
            .env("FIXTURE_PREFIX", &prefix)
            .env("UPDATE_CONTINUE", &resume)
            .env("UPDATE_LOG", &log);
        let mut terminal = launch(command, 80);
        if redirect == "stdout" {
            terminal.until(&format!("({tag}) · 0s"));
        } else {
            let deadline = Instant::now() + HANG_GUARD;
            while !fs::read_to_string(&log)
                .unwrap_or_default()
                .contains("Updating dimcode-alpha")
            {
                assert!(Instant::now() < deadline);
                thread::sleep(Duration::from_millis(10));
            }
        }
        fs::write(&resume, "continue").unwrap();
        assert!(terminal.wait().success());
        let logged = fs::read_to_string(&log).unwrap();
        if redirect == "stdout" {
            assert!(logged.is_empty());
            assert!(terminal.transcript.contains("Updated dimcode-alpha"));
        } else {
            assert!(terminal.transcript.is_empty());
            assert!(logged.contains("Updating dimcode-alpha"));
            assert!(logged.contains("Updated dimcode-alpha"));
            assert!(!logged.contains('\u{1b}'));
        }
        fs::remove_file(&resume).unwrap();
    }
    let installer_pid = home.0.join("installer-pid");
    let child_pid = home.0.join("child-pid");
    let marker = home.0.join("installed");
    for interruption in ["SIGTERM", "SIGINT", "closed terminal"] {
        let mut command = Command::new("sh");
        command
            .args(["-c", "trap '' HUP; exec \"$@\"", "sh"])
            .arg(&executable)
            .args(["--home", home.0.to_str().unwrap(), "update"])
            .env("DIMCODE_NPM_PACKAGE", "dimcode-alpha")
            .env("UPDATE_ARGS", &recorded)
            .env("UPDATE_CONTINUE", &resume)
            .env("UPDATE_PID", &installer_pid)
            .env("UPDATE_CHILD", &child_pid)
            .env("UPDATE_MARKER", &marker)
            .env(
                "UPDATE_IGNORE_TERM",
                if interruption == "SIGTERM" { "1" } else { "0" },
            );
        let mut terminal = launch(command, 80);
        terminal.until(&format!("({tag}) · 0s"));
        let deadline = Instant::now() + HANG_GUARD;
        while fs::read_to_string(&child_pid)
            .unwrap_or_default()
            .trim()
            .is_empty()
        {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(10));
        }
        let pids: Vec<i32> = [&installer_pid, &child_pid]
            .into_iter()
            .map(|path| fs::read_to_string(path).unwrap().trim().parse().unwrap())
            .collect();
        if interruption == "closed terminal" {
            drop(terminal.master.take());
            assert!(!terminal.wait_without_reading().success());
        } else {
            terminal.signal(if interruption == "SIGTERM" {
                libc::SIGTERM
            } else {
                libc::SIGINT
            });
            assert!(!terminal.wait().success());
            assert!(terminal.transcript.contains("Update interrupted"));
            assert!(!terminal.transcript.contains("Updated dimcode-alpha"));
        }
        for pid in pids {
            assert_eq!(
                unsafe { libc::kill(pid, 0) },
                -1,
                "{interruption} left process {pid} alive"
            );
            assert_eq!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::ESRCH)
            );
        }
        fs::write(&resume, "continue").unwrap();
        assert!(!marker.exists(), "an interrupted install must not complete");
        fs::remove_file(&resume).unwrap();
        fs::remove_file(&installer_pid).unwrap();
        fs::remove_file(&child_pid).unwrap();
    }
    let output = home.command().arg("update").output().unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("Cannot update this installation automatically")
    );
    let output = home.command().args(["update", "--help"]).output().unwrap();
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("Install the latest version"));
    assert!(!home.0.join("rust").exists());
}

#[test]
fn model_catalog_lists_safe_ids_and_availability_and_refreshes_explicitly() {
    for refresh in [false, true] {
        let home = Home::new();
        let daemon = home.daemon(move |wire| {
        wire.2 = true;
            wire.2 = true;
        let hello = wire.read("hello");
            wire.reply(&hello,json!({"instanceId":"server","serverVersion":env!("CARGO_PKG_VERSION"),"protocolVersion":4,"daemonProfile":{"kind":"product"},"features":["session.create","session.history","session.history.latestTurn","session.methods"]}));
            let request = wire.read(if refresh { "provider.refresh" } else { "provider.catalog" });
            assert_eq!(request["params"], json!({"providerId":"fixture"}));
            let catalog = json!({"account":{"accessToken":"must-not-print"},"version":1,"disabledModelIds":["disabled"],"models":[
                {"id":"model\u{1b}[2J-id","name":"Model\nName\tvalue","unavailableReason":null,"capabilities":dim_agent_types::CatalogCapabilities::with_limits(128_000, 16_384)},
                {"id":"disabled","name":"Disabled","unavailableReason":null,"capabilities":dim_agent_types::CatalogCapabilities::with_limits(128_000, 16_384)},
                {"id":"missing","name":"Missing","unavailableReason":"private diagnostic must-not-print","capabilities":dim_agent_types::CatalogCapabilities::default()}
            ]});
            wire.reply(&request, if refresh { catalog } else { json!({"catalog":catalog}) });
        });
        let mut command = home.command();
        command.args(["models", "fixture"]);
        if refresh {
            command.arg("--refresh");
        }
        let output = command.output().unwrap();
        daemon.join().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            "model-id\tModel Name value\tavailable\ndisabled\tDisabled\tdisabled\nmissing\tMissing\tunavailable\n"
        );
        assert!(output.stderr.is_empty());
    }
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.2 = true;
        let hello = wire.read("hello");
        wire.reply(&hello,json!({"instanceId":"server","serverVersion":env!("CARGO_PKG_VERSION"),"protocolVersion":4,"daemonProfile":{"kind":"product"},"features":["session.create","session.history","session.history.latestTurn","session.methods"]}));
        let catalog = wire.read("provider.catalog");
        wire.reply(&catalog, json!({"catalog":null}));
    });
    let output = home.command().args(["models", "fixture"]).output().unwrap();
    daemon.join().unwrap();
    assert!(output.status.success());
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("--refresh"));
}

/// A provider whose catalog holds no models is named on standard error with
/// the command that lists them, instead of printing nothing at all.
#[test]
fn model_catalog_names_an_empty_catalog() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.2 = true;
        let hello = wire.read("hello");
        wire.reply(&hello,json!({"instanceId":"server","serverVersion":env!("CARGO_PKG_VERSION"),"protocolVersion":4,"daemonProfile":{"kind":"product"},"features":["session.create","session.history","session.history.latestTurn","session.methods"]}));
        let catalog = wire.read("provider.catalog");
        assert_eq!(catalog["params"], json!({"providerId":"fixture"}));
        wire.reply(
            &catalog,
            json!({"catalog":{"version":1,"disabledModelIds":[],"models":[]}}),
        );
    });
    let output = home
        .command()
        .env("LC_ALL", "C")
        .args(["models", "fixture"])
        .output()
        .unwrap();
    daemon.join().unwrap();
    assert!(output.status.success());
    assert!(output.stdout.is_empty());
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "No models. Run dimcode models fixture --refresh to list them.\n"
    );
}

/// `--refresh` against a provider that lists nothing says the listing came
/// back empty, instead of naming the refresh command the user just ran.
#[test]
fn model_catalog_refresh_names_an_empty_listing() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.2 = true;
        let hello = wire.read("hello");
        wire.reply(&hello,json!({"instanceId":"server","serverVersion":env!("CARGO_PKG_VERSION"),"protocolVersion":4,"daemonProfile":{"kind":"product"},"features":["session.create","session.history","session.history.latestTurn","session.methods"]}));
        let catalog = wire.read("provider.refresh");
        assert_eq!(catalog["params"], json!({"providerId":"fixture"}));
        wire.reply(
            &catalog,
            json!({"version":2,"disabledModelIds":[],"models":[]}),
        );
    });
    let output = home
        .command()
        .env("LC_ALL", "C")
        .args(["models", "fixture", "--refresh"])
        .output()
        .unwrap();
    daemon.join().unwrap();
    assert!(output.status.success());
    assert!(output.stdout.is_empty());
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "No models. The provider listed none.\n"
    );
}

/// A model no source declares limits for is marked `undeclared` and the
/// list names the command that declares them, instead of hiding it among
/// the unavailable models.
#[test]
fn model_catalog_marks_undeclared_limits_and_names_the_declaring_command() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.2 = true;
        let hello = wire.read("hello");
        wire.reply(&hello,json!({"instanceId":"server","serverVersion":env!("CARGO_PKG_VERSION"),"protocolVersion":4,"daemonProfile":{"kind":"product"},"features":["session.create","session.history","session.history.latestTurn","session.methods"]}));
        let catalog = wire.read("provider.catalog");
        assert_eq!(catalog["params"], json!({"providerId":"fixture"}));
        wire.reply(
            &catalog,
            json!({"catalog":{"version":1,"disabledModelIds":[],"models":[
                {"id":"narrow","name":"Narrow","unavailableReason":"limits_undeclared","missingLimits":["contextWindow","maxOutputTokens"],"capabilities":dim_agent_types::CatalogCapabilities::default()},
                {"id":"ready","name":"Ready","unavailableReason":null,"missingLimits":[],"capabilities":dim_agent_types::CatalogCapabilities::with_limits(128_000, 16_384)}
            ]}}),
        );
    });
    let output = home
        .command()
        .env("LC_ALL", "C")
        .args(["models", "fixture"])
        .output()
        .unwrap();
    daemon.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "narrow\tNarrow\tundeclared\nready\tReady\tavailable\n"
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "A model whose limits are undeclared cannot be selected; declare them with dimcode models fixture --set-missing-limits --context-window N --max-output-tokens N.\n"
    );
}

/// A model refused for its undeclared limits names the command that
/// declares them, with its provider and model, when the explicit start
/// model cannot be adopted.
#[test]
fn model_adoption_names_the_declaring_command_for_undeclared_limits() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.2 = true;
        let hello = wire.read("hello");
        wire.reply(&hello,json!({"instanceId":"server","serverVersion":env!("CARGO_PKG_VERSION"),"protocolVersion":4,"daemonProfile":{"kind":"product"},"features":["session.create","session.history","session.history.latestTurn","session.methods"]}));
        let create = wire.read("session.create");
        assert_eq!(
            create["params"]["model"],
            json!({"provider":"fixture","modelId":"narrow"})
        );
        wire.send(json!({"jsonrpc":"2.0","id":create["id"],"error":{"code":-32000,"message":"Explicit model is unavailable","data":{"appError":"provider_invalid_config"}}}));
        let catalog = wire.read("provider.catalog");
        assert_eq!(catalog["params"], json!({"providerId":"fixture","modelId":"narrow"}));
        wire.reply(
            &catalog,
            json!({"catalog":{"version":1,"disabledModelIds":[],"models":[
                {"id":"narrow","name":"Narrow","unavailableReason":"limits_undeclared","missingLimits":["contextWindow","maxOutputTokens"],"capabilities":dim_agent_types::CatalogCapabilities::default()}
            ]}}),
        );
    });
    let output = home
        .command()
        .env("LC_ALL", "C")
        .args([
            "--provider",
            "fixture",
            "--model",
            "narrow",
            "--prompt",
            "hello",
        ])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    daemon.join().unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Cannot create session: "), "{stderr}");
    assert!(
        stderr.contains("Declare them with dimcode models fixture --set-limits narrow --context-window N --max-output-tokens N."),
        "{stderr}"
    );
}

/// `--set-limits` declares both limits of the named model at the record
/// version, then prints the catalog the way `models PROVIDER` does.
#[test]
fn model_catalog_declares_limits_for_one_model() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.2 = true;
        let hello = wire.read("hello");
        wire.reply(&hello,json!({"instanceId":"server","serverVersion":env!("CARGO_PKG_VERSION"),"protocolVersion":4,"daemonProfile":{"kind":"product"},"features":["session.create","session.history","session.history.latestTurn","session.methods"]}));
        let catalog = wire.read("provider.catalog");
        assert_eq!(catalog["params"], json!({"providerId":"fixture","modelId":"narrow"}));
        wire.reply(
            &catalog,
            json!({"catalog":{"version":1,"disabledModelIds":[],"models":[
                {"id":"narrow","name":"Narrow","unavailableReason":"limits_undeclared","missingLimits":["contextWindow","maxOutputTokens"],"capabilities":dim_agent_types::CatalogCapabilities::default()}
            ]}}),
        );
        let edit = wire.read("provider.models.edit");
        assert_eq!(
            edit["params"],
            json!({"providerId":"fixture","expectedVersion":1,"modelId":"narrow",
                "edit":{"contextWindow":64000,"maxOutputTokens":8000},"reply":"summary"})
        );
        wire.reply(&edit, json!({"version":2}));
        let catalog = wire.read("provider.catalog");
        assert_eq!(catalog["params"], json!({"providerId":"fixture","modelId":"narrow"}));
        wire.reply(
            &catalog,
            json!({"catalog":{"version":2,"disabledModelIds":[],"models":[
                {"id":"narrow","name":"Narrow","unavailableReason":null,"missingLimits":[],"capabilities":dim_agent_types::CatalogCapabilities::with_limits(64_000, 8_000)}
            ]}}),
        );
    });
    let output = home
        .command()
        .env("LC_ALL", "C")
        .args([
            "models",
            "fixture",
            "--set-limits",
            "narrow",
            "--context-window",
            "64000",
            "--max-output-tokens",
            "8000",
        ])
        .output()
        .unwrap();
    daemon.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "narrow\tNarrow\tavailable\n"
    );
    assert!(output.stderr.is_empty());
}

/// `--set-missing-limits` reads the catalog, declares the limits of every
/// model the list marks `undeclared`, and chains each reply's version into
/// the next write. A disabled model and one unavailable for another reason
/// keep that state even when they report missing limits.
#[test]
fn model_catalog_declares_missing_limits_for_every_model() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.2 = true;
        let hello = wire.read("hello");
        wire.reply(&hello,json!({"instanceId":"server","serverVersion":env!("CARGO_PKG_VERSION"),"protocolVersion":4,"daemonProfile":{"kind":"product"},"features":["session.create","session.history","session.history.latestTurn","session.methods"]}));
        let catalog = wire.read("provider.catalog");
        wire.reply(
            &catalog,
            json!({"catalog":{"version":1,"disabledModelIds":["gone"],"models":[
                {"id":"narrow","name":"Narrow","unavailableReason":"limits_undeclared","missingLimits":["contextWindow","maxOutputTokens"],"capabilities":dim_agent_types::CatalogCapabilities::default()},
                {"id":"ready","name":"Ready","unavailableReason":null,"missingLimits":[],"capabilities":dim_agent_types::CatalogCapabilities::with_limits(128_000, 16_384)},
                {"id":"wide","name":"Wide","unavailableReason":"limits_undeclared","missingLimits":["maxOutputTokens"],"capabilities":dim_agent_types::CatalogCapabilities { context_window: Some(128_000), ..Default::default() }},
                {"id":"gone","name":"Gone","unavailableReason":null,"missingLimits":["contextWindow","maxOutputTokens"],"capabilities":dim_agent_types::CatalogCapabilities::default()},
                {"id":"blocked","name":"Blocked","unavailableReason":"provider_unavailable","missingLimits":["contextWindow"],"capabilities":dim_agent_types::CatalogCapabilities { max_output_tokens: Some(16_384), ..Default::default() }}
            ]}}),
        );
        let first = wire.read("provider.models.edit");
        assert_eq!(
            first["params"],
            json!({"providerId":"fixture","expectedVersion":1,"modelId":"narrow",
                "edit":{"contextWindow":64000,"maxOutputTokens":8000},"reply":"summary"})
        );
        wire.reply(&first, json!({"version":2}));
        let second = wire.read("provider.models.edit");
        assert_eq!(
            second["params"],
            json!({"providerId":"fixture","expectedVersion":2,"modelId":"wide",
                "edit":{"maxOutputTokens":8000},"reply":"summary"})
        );
        wire.reply(&second, json!({"version":3}));
        let catalog = wire.read("provider.catalog");
        wire.reply(
            &catalog,
            json!({"catalog":{"version":3,"disabledModelIds":[],"models":[
                {"id":"narrow","name":"Narrow","unavailableReason":null,"missingLimits":[],"capabilities":dim_agent_types::CatalogCapabilities::with_limits(64_000, 8_000)},
                {"id":"ready","name":"Ready","unavailableReason":null,"missingLimits":[],"capabilities":dim_agent_types::CatalogCapabilities::with_limits(128_000, 16_384)},
                {"id":"wide","name":"Wide","unavailableReason":null,"missingLimits":[],"capabilities":dim_agent_types::CatalogCapabilities::with_limits(128_000, 8_000)}
            ]}}),
        );
    });
    let output = home
        .command()
        .env("LC_ALL", "C")
        .args([
            "models",
            "fixture",
            "--set-missing-limits",
            "--context-window",
            "64000",
            "--max-output-tokens",
            "8000",
        ])
        .output()
        .unwrap();
    daemon.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "narrow\tNarrow\tavailable\nready\tReady\tavailable\nwide\tWide\tavailable\n"
    );
    assert!(output.stderr.is_empty());
}

/// The declaration flags require the models command, one form and both
/// values, and fail before the CLI connects to a daemon.
#[test]
fn model_limit_declaration_flags_are_validated_before_connecting() {
    let home = Home::new();
    fs::remove_dir_all(home.0.join("rust")).unwrap();
    for (args, reason) in [
        (
            vec![
                "models",
                "fixture",
                "--set-limits",
                "narrow",
                "--context-window",
                "64000",
            ],
            "--max-output-tokens",
        ),
        (
            vec![
                "models",
                "fixture",
                "--set-limits",
                "narrow",
                "--context-window",
                "0",
                "--max-output-tokens",
                "8000",
            ],
            "positive integer",
        ),
        (
            vec![
                "models",
                "fixture",
                "--set-limits",
                "narrow",
                "--set-missing-limits",
                "--context-window",
                "1",
                "--max-output-tokens",
                "1",
            ],
            "not both",
        ),
        (
            vec![
                "models",
                "fixture",
                "--refresh",
                "--set-missing-limits",
                "--context-window",
                "1",
                "--max-output-tokens",
                "1",
            ],
            "cannot be combined",
        ),
        (
            vec!["models", "fixture", "--context-window", "1"],
            "--set-limits or --set-missing-limits",
        ),
        (
            vec![
                "exec",
                "--set-limits",
                "narrow",
                "--context-window",
                "1",
                "--max-output-tokens",
                "1",
            ],
            "models PROVIDER",
        ),
    ] {
        let output = home
            .command()
            .args(&args)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1), "{args:?}");
        assert!(output.stdout.is_empty(), "{args:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(reason),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!home.0.join("rust").exists(), "{args:?}");
    }
}

#[test]
fn daemon_stop_without_an_endpoint_never_creates_state_or_spawns_a_server() {
    let home = Home::new();
    fs::remove_dir_all(home.0.join("rust")).unwrap();
    let output = home
        .command()
        .args([
            "--server",
            "/missing-app-server",
            "--model-config",
            "/missing-model-config",
            "daemon",
            "stop",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!home.0.join("rust").exists());
    fs::create_dir(home.0.join("rust")).unwrap();
    fs::set_permissions(home.0.join("rust"), fs::Permissions::from_mode(0o700)).unwrap();
    assert!(
        home.command()
            .args(["daemon", "stop"])
            .output()
            .unwrap()
            .status
            .success()
    );
    assert!(!home.0.join("rust/daemon.log").exists());
}

#[test]
fn daemon_stop_waits_for_completion_and_closure_without_touching_a_successor() {
    let home = Home::new();
    fs::write(home.0.join("account-marker"), "retained").unwrap();
    let socket = home.0.join("rust/daemon.sock");
    let daemon = home.daemon(move |wire| {
        wire.2 = true;
        let hello = wire.read("hello");
        wire.reply(&hello,json!({"instanceId":"server","serverVersion":env!("CARGO_PKG_VERSION"),"protocolVersion":4,"daemonProfile":{"kind":"configured","modelConfigSha256":"0".repeat(64)},"features":["session.create","session.history","session.history.latestTurn","session.methods","daemon.shutdown.completed"]}));
        let shutdown = wire.read("daemon.shutdown");
        assert_eq!(shutdown["params"], json!({}));
        thread::sleep(Duration::from_millis(100));
        fs::remove_file(&socket).unwrap();
        let _successor = UnixListener::bind(&socket).unwrap();
        wire.reply(&shutdown, json!({}));
        thread::sleep(Duration::from_millis(100));
        wire.0.get_mut().shutdown(std::net::Shutdown::Write).unwrap();
    });
    let started = Instant::now();
    let output = home.command().args(["daemon", "stop"]).output().unwrap();
    let elapsed = started.elapsed();
    daemon.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(elapsed >= Duration::from_millis(200));
    assert!(home.0.join("rust/daemon.sock").exists());
    assert_eq!(
        fs::read_to_string(home.0.join("account-marker")).unwrap(),
        "retained"
    );
}

#[test]
fn a_daemon_of_another_version_is_stopped_and_replaced() {
    let home = Home::new();
    let socket = home.0.join("rust/daemon.sock");
    let daemon = home.daemon(move |wire| {
        wire.2 = true;
        let hello = wire.read("hello");
        wire.reply(&hello,json!({"instanceId":"old","serverVersion":"0.0.1-old","protocolVersion":4,"daemonProfile":{"kind":"product"},"features":["session.create","session.history","session.history.latestTurn","session.methods","daemon.shutdown.completed"]}));
        let summaries = wire.read("session.summaries");
        wire.reply(&summaries, json!([]));
        let shutdown = wire.read("daemon.shutdown");
        fs::remove_file(&socket).unwrap();
        let successor = UnixListener::bind(&socket).unwrap();
        fs::set_permissions(&socket, fs::Permissions::from_mode(0o600)).unwrap();
        successor.set_nonblocking(true).unwrap();
        wire.reply(&shutdown, json!({}));
        wire.0.get_mut().shutdown(std::net::Shutdown::Write).unwrap();
        let deadline = Instant::now() + HANG_GUARD;
        loop {
            let stream = match successor.accept() {
                Ok((stream, _)) => stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(Instant::now() < deadline, "the CLI never reconnected");
                    thread::sleep(Duration::from_millis(10));
                    continue;
                }
                Err(error) => panic!("{error}"),
            };
            stream.set_nonblocking(false).unwrap();
            stream.set_read_timeout(Some(HANG_GUARD)).unwrap();
            let mut wire = Wire::new(stream);
            let Some(line) = wire.trailing_request() else {
                continue;
            };
            let hello: Value = serde_json::from_str(&line).unwrap();
            assert_eq!(hello["method"], "hello");
            wire.reply(&hello,json!({"instanceId":"new","serverVersion":env!("CARGO_PKG_VERSION"),"protocolVersion":4,"daemonProfile":{"kind":"product"},"features":["session.create","session.history","session.history.latestTurn","session.methods"]}));
            let catalog = wire.read("provider.catalog");
            wire.reply(&catalog, json!({"catalog":null}));
            break;
        }
    });
    let output = home.command().args(["models", "fixture"]).output().unwrap();
    daemon.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// The daemon the CLI started for a home is gone once the home drops.
///
/// The harness finds it with `ps`, so a missing `ps` fails the test instead
/// of reading as no daemon.
#[test]
fn a_home_drop_leaves_no_daemon_behind() {
    let home = Home::new();
    let path = home.0.clone();
    let output = home.command().args(["models", "fixture"]).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let pattern = path.to_str().unwrap();
    assert_eq!(
        daemon_cleanup::pids_naming(pattern).len(),
        1,
        "the CLI did not start its daemon for {}",
        path.display()
    );
    drop(home);
    assert!(
        daemon_cleanup::pids_naming(pattern).is_empty(),
        "the test left its daemon running for {}",
        path.display()
    );
    assert!(!path.exists(), "the test left {} on disk", path.display());
}

/// The daemon closes the descriptors it inherited from the CLI.
///
/// The sweep and this test use the same listing: `/dev/fd` on macOS and the
/// other Unix systems, and on Linux `close_range` or `/proc/self/fd`. The
/// bounded fallback closes every number below the soft `RLIMIT_NOFILE`,
/// which includes the pipe's inherited write end.
#[test]
fn a_daemon_keeps_no_descriptor_the_cli_inherited() {
    let home = Home::new();
    let (reader, writer) = std::io::pipe().unwrap();
    let mut command = home.command();
    command.args(["models", "fixture"]);
    let inherited = writer.as_raw_fd();
    unsafe {
        std::os::unix::process::CommandExt::pre_exec(&mut command, move || {
            if libc::fcntl(inherited, libc::F_SETFD, 0) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let status = command.status().unwrap();
    drop(writer);
    assert!(
        status.success(),
        "the CLI did not start the daemon for {}",
        home.0.display()
    );
    let mut descriptor = libc::pollfd {
        fd: reader.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    let ready = unsafe { libc::poll(&mut descriptor, 1, PATIENCE.as_millis() as libc::c_int) };
    assert!(
        ready > 0,
        "the daemon kept the pipe's write end for {}",
        home.0.display()
    );
    let mut byte = [0u8; 1];
    let read = unsafe {
        libc::read(
            reader.as_raw_fd(),
            byte.as_mut_ptr().cast::<libc::c_void>(),
            byte.len(),
        )
    };
    assert_eq!(read, 0, "the pipe carried data instead of EOF");
}

/// A server a test starts on a home itself is stopped even before it binds:
/// the CLI's `daemon.log` and the server's `daemon.sock.lock` do not exist
/// yet, so the marker the test writes is what tells the drop to stop it.
#[test]
fn a_home_stops_a_marked_server_that_never_binds() {
    let home = Home::new();
    let pattern = home.0.to_str().unwrap().to_owned();
    daemon_cleanup::mark_test_server(&home.0);
    // A stand-in for a server that is still starting when the test ends: it
    // names the home in its command line, never binds, and is a grandchild,
    // so the test holds no handle on it and the drop has to find it.
    let status = Command::new("sh")
        .arg("-c")
        .arg("sh -c \"$2\" sh \"$1\" & exit 0")
        .arg("sh")
        .arg(&pattern)
        .arg("while :; do sleep 1; done")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(status.success(), "the stand-in server did not start");
    let deadline = Instant::now() + PATIENCE;
    while daemon_cleanup::pids_naming(&pattern).is_empty() {
        assert!(Instant::now() < deadline, "the stand-in server never ran");
        thread::sleep(Duration::from_millis(10));
    }
    drop(home);
    assert!(
        daemon_cleanup::pids_naming(&pattern).is_empty(),
        "the drop left the marked server for {pattern} running"
    );
}

/// A stop re-reads a pid's command line before it signals, so a pid that now
/// runs something else is left alone. The process here starts with the home
/// in its command line and then replaces it, the shape a reused pid has.
#[test]
fn a_stop_never_signals_a_process_that_no_longer_names_the_home() {
    let home = Home::new();
    let pattern = home.0.to_str().unwrap();
    let mut process = Command::new("sh")
        .arg("-c")
        .arg("exec sleep 30")
        .arg(pattern)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let pid = process.id() as libc::pid_t;
    let deadline = Instant::now() + PATIENCE;
    while daemon_cleanup::command_of(pid).is_some_and(|command| command.contains(pattern)) {
        assert!(
            Instant::now() < deadline,
            "the stand-in never replaced its command line"
        );
        thread::sleep(Duration::from_millis(10));
    }
    assert!(
        !daemon_cleanup::signal_if_naming(pid, pattern, libc::SIGTERM),
        "the stop signalled a process that no longer names the home"
    );
    assert!(
        process.try_wait().unwrap().is_none(),
        "the signal reached a process that no longer names the home"
    );
    process.kill().unwrap();
    process.wait().unwrap();
}

#[test]
fn daemon_stop_never_treats_old_protocol_eof_or_cleanup_failure_as_completion() {
    for case in ["old", "eof", "failed"] {
        let home = Home::new();
        let daemon = home.daemon(move |wire| {
            let hello = wire.read("hello");
            let mut features = vec!["session.create", "session.history"];
            if case != "old" { features.push("daemon.shutdown.completed"); }
            wire.reply(&hello, json!({"instanceId":"server","serverVersion":env!("CARGO_PKG_VERSION"),"protocolVersion":4,"daemonProfile":{"kind":"product"},"features":features}));
            if case == "old" {
                assert_eq!(wire.trailing_request(), None, "must not stop an incompatible daemon");
            } else {
                let request = wire.read("daemon.shutdown");
                if case == "failed" { wire.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{"code":-32000,"message":"Daemon cleanup failed"}})); }
            }
        });
        let output = home.command().args(["daemon", "stop"]).output().unwrap();
        daemon.join().unwrap();
        assert_eq!(output.status.code(), Some(1), "{case}");
        assert!(!String::from_utf8_lossy(&output.stdout).contains("Daemon stopped."));
        assert!(!output.stderr.is_empty());
    }
}

#[test]
fn line_mode_reports_each_retry_attempt_once_on_stderr() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.start(false);
        let submit = wire.read("session.submit");
        let retry_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
            + 60_000;
        for attempt in [2, 2, 3] {
            wire.notice("session.model.progress", json!({
                "sessionId": "session", "branchId": "main", "requestId": "model-request",
                "textOffset": 0, "text": "", "thinking": "", "thinkingOffset": 0,
                "retry": {
                    "attempt": attempt, "maxAttempts": 8, "retryAtMs": retry_at,
                    "kind": "server_error", "reason": "The server is overloaded",
                },
            }));
        }
        wire.reply(&submit, json!({"event":{}}));
        let idle = wire.read("session.idle");
        wire.event(2, "model-output-end", json!({"turn_id":"turn","request_id":"model-request","status":"completed","finish_reason":"end_turn"}));
        wire.end(json!({"turn_id":"turn","status":"completed","reason":"done"}));
        wire.reply(&idle, json!({"sequence":3}));
        wire.detach();
    });
    let output = wait(
        home.command()
            .args(["--prompt", "go"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    daemon.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(stderr.matches("retrying ").count(), 2, "{stderr}");
    assert!(stderr.contains("retrying 2/8 in"), "{stderr}");
    assert!(stderr.contains("retrying 3/8 in"), "{stderr}");
    assert!(
        stderr.contains("The provider reported a server error: The server is overloaded"),
        "{stderr}"
    );
}

#[test]
fn exec_exits_zero_when_the_turn_completed_at_its_output_limit() {
    for json_mode in [false, true] {
        let home = Home::new();
        let daemon = home.daemon(|wire| {
            wire.start(false);
            let submit = wire.read("session.submit");
            wire.reply(&submit, json!({"event":{}}));
            let idle = wire.read("session.idle");
            // An output-limit stop: the request keeps its failure fact with
            // its presentation, and the turn ends Completed.
            wire.event(
                9,
                "model-output-end",
                json!({
                    "turn_id": "turn", "request_id": "model-request", "status": "failed",
                    "error": "Model local/fixture stopped at max_tokens",
                    "presentation": {
                        "source": "local", "reason": "max_tokens",
                        "serverMessage": "Model local/fixture stopped at max_tokens",
                        "action": "Increase the output token limit and retry.",
                    },
                    "failure": {
                        "kind": "output_limit", "reason": "stopped at max_tokens",
                        "attempts": 1, "waitedMs": 0,
                    },
                }),
            );
            wire.end(json!({"turn_id":"turn","status":"completed","reason":"output_limit"}));
            wire.reply(&idle, json!({"sequence":3}));
            wire.detach();
        });
        let mut args = vec!["exec", "-p", "go"];
        if json_mode {
            args.push("--json");
        }
        let output = home.command().args(&args).output().unwrap();
        daemon.join().unwrap();
        assert_eq!(
            output.status.code(),
            Some(0),
            "json {json_mode}: status {:?} stdout {} stderr {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        // Line mode writes the human line to stderr; `--json` writes the events
        // to stdout and keeps stderr for the daemon notes.
        let (shown, other) = if json_mode {
            (
                String::from_utf8_lossy_owned(output.stdout),
                String::from_utf8_lossy_owned(output.stderr),
            )
        } else {
            (
                String::from_utf8_lossy_owned(output.stderr),
                String::from_utf8_lossy_owned(output.stdout),
            )
        };
        if json_mode {
            assert!(
                shown.contains("\"model-output-end\"") || shown.contains("step"),
                "{shown}"
            );
        } else {
            assert!(
                shown.contains("stopped at max_tokens"),
                "no failure line: {shown:?}"
            );
            assert_eq!(
                shown.matches("[failed]").count(),
                1,
                "one failure line, no turn repeat: {shown}"
            );
        }
        assert!(other.is_empty() || other.contains("workspace:"), "{other}");
    }
}

#[test]
fn exec_keeps_exit_two_for_a_failure_with_no_calls() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.start(false);
        let submit = wire.read("session.submit");
        wire.reply(&submit, json!({"event":{}}));
        let idle = wire.read("session.idle");
        wire.event(
            9,
            "model-output-end",
            json!({
                "turn_id": "turn", "request_id": "model-request", "status": "failed",
                "error": "The provider rejected the request: bad tool schema.",
                "failure": {
                    "kind": "invalid_request", "reason": "bad tool schema",
                    "attempts": 1, "waitedMs": 0,
                },
            }),
        );
        wire.end(json!({"turn_id":"turn","status":"failed","error":"The provider rejected the request: bad tool schema."}));
        wire.reply(&idle, json!({"sequence":2}));
        wire.detach();
    });
    let output = home.command().args(["exec", "-p", "go"]).output().unwrap();
    daemon.join().unwrap();
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn exec_prints_the_model_failure_line_not_the_daemon_sentence() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.start(false);
        let submit = wire.read("session.submit");
        wire.reply(&submit, json!({"event":{}}));
        let idle = wire.read("session.idle");
        wire.event(
            9,
            "model-output-end",
            json!({
                "turn_id": "turn", "request_id": "model-request", "status": "failed",
                "error": "fixture error",
                "failure": {
                    "kind": "server_error", "status": 503, "providerCode": "overloaded",
                    "reason": "The server is overloaded", "attempts": 3, "waitedMs": 4200,
                },
            }),
        );
        wire.end(json!({"turn_id":"turn","status":"failed","error":"fixture error"}));
        wire.reply(&idle, json!({"sequence":2}));
        wire.detach();
    });
    let output = home.command().args(["exec", "-p", "go"]).output().unwrap();
    daemon.join().unwrap();
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("[failed] The provider reported a server error: The server is overloaded. Try again later or choose another model. Tried 3 times over 4 s."),
        "{stderr}"
    );
    assert!(
        !stderr.contains("Model or turn failed; see the private daemon log"),
        "{stderr}"
    );
    assert_eq!(stderr.matches("[failed]").count(), 1, "{stderr}");
}

#[test]
fn headless_streams_unicode_once_and_waits_for_the_delivery_barrier() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.start(false);
        let submit = wire.read("session.submit");
        assert_eq!(submit["params"]["text"], "/fast on");
        assert!(!submit["params"]["submissionId"].as_str().unwrap().is_empty());
        wire.notice("session.model.progress",json!({"sessionId":"session","branchId":"main","requestId":"model-request","textOffset":0,"text":"你\u{1b}[2J好","thinking":"","thinkingOffset":0}));
        wire.reply(&submit, json!({"event":{}}));
        let idle = wire.read("session.idle");
        wire.event(12,"text",json!({"turn_id":"turn","request_id":"model-request","delta":"你\u{1b}[2J好"}));
        wire.notice("session.model.progress",json!({"sessionId":"session","branchId":"main","requestId":"model-request","textOffset":6,"text":"世界","thinking":"","thinkingOffset":0}));
        wire.event(20,"text",json!({"turn_id":"turn","request_id":"model-request","delta":"世界"}));
        wire.event(22,"model-output-end",json!({"turn_id":"turn","request_id":"model-request","status":"completed","finish_reason":"end_turn"}));
        wire.end(json!({"turn_id":"turn","status":"completed","reason":"done"}));
        wire.reply(&idle, json!({"sequence":30}));
        wire.detach();
    });
    let output = wait(
        home.command()
            .args(["--prompt", "/fast on"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    daemon.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "你好世界\n");
}

/// The turn facts an App Server sends before the submitted input's turn
/// writes text: the branch's turn starts, then InputQueue consumes the input.
fn open_turn(wire: &mut Wire, turn: &str, input: &str) {
    wire.notice("session.event",json!({"sessionId":"session","branchId":"main","event":{"id":"turn-start","sequence":9,"create_at":1,"type":"turn","name":"start","data":{"turn_id":turn}}}));
    wire.notice("session.event",json!({"sessionId":"session","branchId":"main","event":{"id":"input-consumed","sequence":10,"create_at":1,"type":"input","name":"consumed","data":{"input_id":input}}}));
}

#[test]
fn exec_writes_only_the_final_reply_to_stdout() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.start(false);
        let submit = wire.read("session.submit");
        wire.notice("session.model.progress",json!({"sessionId":"session","branchId":"main","requestId":"model-request","textOffset":0,"text":"I will read the config.","thinking":"","thinkingOffset":0}));
        wire.reply(&submit, json!({"event":{"id":"input"}}));
        let idle = wire.read("session.idle");
        open_turn(wire, "turn", "input");
        wire.event(12,"text",json!({"turn_id":"turn","request_id":"model-request","delta":"I will read the config."}));
        wire.event(14,"tool-use",json!({"turn_id":"turn","request_id":"model-request","call":{"id":"call","name":"read","input":{"path":"config/service.toml"}}}));
        wire.event(16,"tool-result",json!({"turn_id":"turn","request_id":"model-request","results":[{"call_id":"call","result":{"content":"port = 8080"}}]}));
        wire.event(18,"model-output-end",json!({"turn_id":"turn","request_id":"model-request","status":"completed","finish_reason":"tool_use"}));
        wire.event(20,"text",json!({"turn_id":"turn","request_id":"final-request","delta":"The service listens on port 8080."}));
        wire.event(22,"model-output-end",json!({"turn_id":"turn","request_id":"final-request","status":"completed","finish_reason":"end_turn"}));
        wire.end(json!({"turn_id":"turn","status":"completed","reason":"done"}));
        wire.reply(&idle, json!({"sequence":30}));
        wire.detach();
    });
    let output = wait(
        home.command()
            .args(["exec", "-p", "Which port does the service use?"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    daemon.join().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "status {:?} stdout {stdout:?} stderr {stderr:?}",
        output.status.code()
    );
    assert_eq!(
        stdout, "The service listens on port 8080.\n",
        "stdout carries the run's final reply alone; stderr was {stderr:?}"
    );
    assert!(
        stderr.contains("I will read the config."),
        "the words before the tool call go to stderr: {stderr:?}"
    );
}

#[test]
fn exec_keeps_words_between_tool_calls_on_stderr() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.start(false);
        let submit = wire.read("session.submit");
        wire.reply(&submit, json!({"event":{"id":"input"}}));
        let idle = wire.read("session.idle");
        open_turn(wire, "turn", "input");
        wire.event(12,"text",json!({"turn_id":"turn","request_id":"first-request","delta":"I will read the config."}));
        wire.event(13,"tool-use",json!({"turn_id":"turn","request_id":"first-request","call":{"id":"config","name":"read","input":{"path":"config/service.toml"}}}));
        wire.event(14,"tool-result",json!({"turn_id":"turn","request_id":"first-request","results":[{"call_id":"config","result":{"content":"port = 8080"}}]}));
        wire.event(15,"model-output-end",json!({"turn_id":"turn","request_id":"first-request","status":"completed","finish_reason":"tool_use"}));
        wire.event(16,"text",json!({"turn_id":"turn","request_id":"second-request","delta":"The port is fixed; I will read the deploy script."}));
        wire.event(17,"tool-use",json!({"turn_id":"turn","request_id":"second-request","call":{"id":"deploy","name":"read","input":{"path":"scripts/deploy.sh"}}}));
        wire.event(18,"tool-result",json!({"turn_id":"turn","request_id":"second-request","results":[{"call_id":"deploy","result":{"content":"export PORT=8080"}}]}));
        wire.event(19,"model-output-end",json!({"turn_id":"turn","request_id":"second-request","status":"completed","finish_reason":"tool_use"}));
        wire.event(20,"text",json!({"turn_id":"turn","request_id":"final-request","delta":"The service listens on port 8080."}));
        wire.event(21,"model-output-end",json!({"turn_id":"turn","request_id":"final-request","status":"completed","finish_reason":"end_turn"}));
        wire.end(json!({"turn_id":"turn","status":"completed","reason":"done"}));
        wire.reply(&idle, json!({"sequence":30}));
        wire.detach();
    });
    let output = wait(
        home.command()
            .args(["exec", "-p", "Which port does the service use?"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    daemon.join().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "status {:?} stdout {stdout:?} stderr {stderr:?}",
        output.status.code()
    );
    assert_eq!(
        stdout, "The service listens on port 8080.\n",
        "stdout carries the run's final reply alone; stderr was {stderr:?}"
    );
    assert!(
        stderr.contains("I will read the config."),
        "the words before the first tool call go to stderr: {stderr:?}"
    );
    assert!(
        stderr.contains("The port is fixed; I will read the deploy script."),
        "the words between the tool calls go to stderr: {stderr:?}"
    );
}

#[test]
fn exec_writes_a_reply_without_tools_to_stdout() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.start(false);
        let submit = wire.read("session.submit");
        wire.reply(&submit, json!({"event":{"id":"input"}}));
        let idle = wire.read("session.idle");
        open_turn(wire, "turn", "input");
        wire.notice("session.model.progress",json!({"sessionId":"session","branchId":"main","requestId":"request","textOffset":0,"text":"The service listens on port 8080.","thinking":"","thinkingOffset":0}));
        wire.event(12,"text",json!({"turn_id":"turn","request_id":"request","delta":"The service listens on port 8080."}));
        wire.event(14,"model-output-end",json!({"turn_id":"turn","request_id":"request","status":"completed","finish_reason":"end_turn"}));
        wire.end(json!({"turn_id":"turn","status":"completed","reason":"done"}));
        wire.reply(&idle, json!({"sequence":30}));
        wire.detach();
    });
    let output = wait(
        home.command()
            .args(["exec", "-p", "Which port does the service use?"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    daemon.join().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "status {:?} stdout {stdout:?} stderr {stderr:?}",
        output.status.code()
    );
    assert_eq!(
        stdout, "The service listens on port 8080.\n",
        "a turn with no tool call keeps its reply on stdout; stderr was {stderr:?}"
    );
    assert!(
        !stderr.contains("The service listens on port 8080."),
        "the reply appears once, on stdout: {stderr:?}"
    );
}

/// A request that ended normally is only the candidate reply: another request
/// in the same turn makes its answer the turn's final one.
#[test]
fn exec_demotes_a_reply_when_the_same_turn_asks_again() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.start(false);
        let submit = wire.read("session.submit");
        wire.reply(&submit, json!({"event":{"id":"input"}}));
        let idle = wire.read("session.idle");
        open_turn(wire, "turn", "input");
        wire.event(12,"text",json!({"turn_id":"turn","request_id":"first-request","delta":"Let me check the queue."}));
        wire.event(13,"model-output-end",json!({"turn_id":"turn","request_id":"first-request","status":"completed","finish_reason":"end_turn"}));
        wire.event(14,"text",json!({"turn_id":"turn","request_id":"second-request","delta":"The queue is empty."}));
        wire.event(15,"model-output-end",json!({"turn_id":"turn","request_id":"second-request","status":"completed","finish_reason":"end_turn"}));
        wire.end(json!({"turn_id":"turn","status":"completed","reason":"done"}));
        wire.reply(&idle, json!({"sequence":30}));
        wire.detach();
    });
    let output = wait(
        home.command()
            .args(["exec", "-p", "Is the queue empty?"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    daemon.join().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "status {:?} stdout {stdout:?} stderr {stderr:?}",
        output.status.code()
    );
    assert_eq!(
        stdout, "The queue is empty.\n",
        "the turn's last request is its reply; stderr was {stderr:?}"
    );
    assert!(
        stderr.contains("Let me check the queue."),
        "the earlier request's words go to stderr: {stderr:?}"
    );
    assert!(
        !stderr.contains("The queue is empty."),
        "the reply appears once, on stdout: {stderr:?}"
    );
}

/// A request can end normally without writing any text. It is still the
/// turn's last request, so the words of the request before it are diagnostics
/// and stdout carries no reply the turn never delivered.
#[test]
fn exec_writes_nothing_to_stdout_when_the_last_request_is_empty() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.start(false);
        let submit = wire.read("session.submit");
        wire.reply(&submit, json!({"event":{"id":"input"}}));
        let idle = wire.read("session.idle");
        open_turn(wire, "turn", "input");
        wire.event(12,"text",json!({"turn_id":"turn","request_id":"first-request","delta":"Let me check the queue."}));
        wire.event(13,"model-output-end",json!({"turn_id":"turn","request_id":"first-request","status":"completed","finish_reason":"end_turn"}));
        wire.event(14,"model-output-end",json!({"turn_id":"turn","request_id":"second-request","status":"completed","finish_reason":"end_turn"}));
        wire.end(json!({"turn_id":"turn","status":"completed","reason":"done"}));
        wire.reply(&idle, json!({"sequence":30}));
        wire.detach();
    });
    let output = wait(
        home.command()
            .args(["exec", "-p", "Is the queue empty?"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    daemon.join().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "status {:?} stdout {stdout:?} stderr {stderr:?}",
        output.status.code()
    );
    assert!(
        stdout.is_empty(),
        "an empty final reply writes nothing to stdout; stdout was {stdout:?}"
    );
    assert!(
        stderr.contains("Let me check the queue."),
        "the earlier request's words go to stderr: {stderr:?}"
    );
}

/// A turn can fail after its model request ended normally; stdout carries no
/// reply the turn never delivered.
#[test]
fn exec_demotes_a_reply_when_the_turn_fails() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.start(false);
        let submit = wire.read("session.submit");
        wire.reply(&submit, json!({"event":{"id":"input"}}));
        let idle = wire.read("session.idle");
        open_turn(wire, "turn", "input");
        wire.event(12,"text",json!({"turn_id":"turn","request_id":"request","delta":"The service listens on port 8080."}));
        wire.event(13,"model-output-end",json!({"turn_id":"turn","request_id":"request","status":"completed","finish_reason":"end_turn"}));
        wire.end(json!({"turn_id":"turn","status":"failed","error":"fixture error"}));
        wire.reply(&idle, json!({"sequence":30}));
        wire.detach();
    });
    let output = wait(
        home.command()
            .args(["exec", "-p", "Which port does the service use?"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    daemon.join().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.code(),
        Some(2),
        "stdout {stdout:?} stderr {stderr:?}"
    );
    assert!(
        stdout.is_empty(),
        "a failed turn leaves no reply on stdout; stdout was {stdout:?}"
    );
    assert!(
        stderr.contains("The service listens on port 8080."),
        "the answer of a turn that failed goes to stderr: {stderr:?}"
    );
}

/// A turn can be cancelled after its model request ended normally; stdout
/// carries no reply the turn never delivered.
#[test]
fn exec_demotes_a_reply_when_the_turn_is_cancelled() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.start(false);
        let submit = wire.read("session.submit");
        wire.reply(&submit, json!({"event":{"id":"input"}}));
        let idle = wire.read("session.idle");
        open_turn(wire, "turn", "input");
        wire.event(12,"text",json!({"turn_id":"turn","request_id":"request","delta":"The service listens on port 8080."}));
        wire.event(13,"model-output-end",json!({"turn_id":"turn","request_id":"request","status":"completed","finish_reason":"end_turn"}));
        wire.end(json!({"turn_id":"turn","status":"cancelled","reason":"interrupted"}));
        wire.reply(&idle, json!({"sequence":30}));
        wire.detach();
    });
    let output = wait(
        home.command()
            .args(["exec", "-p", "Which port does the service use?"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    daemon.join().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "status {:?} stdout {stdout:?} stderr {stderr:?}",
        output.status.code()
    );
    assert!(
        stdout.is_empty(),
        "a cancelled turn leaves no reply on stdout; stdout was {stdout:?}"
    );
    assert!(
        stderr.contains("The service listens on port 8080."),
        "the answer of a turn that was cancelled goes to stderr: {stderr:?}"
    );
}

/// A notice is a diagnostic: it does not decide where a request's held text
/// goes, so the answer of a successful turn still reaches stdout when a notice
/// arrives between its chunks.
#[test]
fn exec_keeps_a_reply_on_stdout_past_a_notice_between_its_chunks() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.start(false);
        let submit = wire.read("session.submit");
        wire.reply(&submit, json!({"event":{"id":"input"}}));
        let idle = wire.read("session.idle");
        open_turn(wire, "turn", "input");
        wire.event(12,"text",json!({"turn_id":"turn","request_id":"request","delta":"The service "}));
        wire.notice("session.mcp.state",json!({"sessionId":"session","servers":[{"id":"docs","state":"auth_required"}]}));
        wire.event(14,"text",json!({"turn_id":"turn","request_id":"request","delta":"listens on port 8080."}));
        wire.event(16,"model-output-end",json!({"turn_id":"turn","request_id":"request","status":"completed","finish_reason":"end_turn"}));
        wire.end(json!({"turn_id":"turn","status":"completed","reason":"done"}));
        wire.reply(&idle, json!({"sequence":30}));
        wire.detach();
    });
    let output = wait(
        home.command()
            .args(["exec", "-p", "Which port does the service use?"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    daemon.join().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "status {:?} stdout {stdout:?} stderr {stderr:?}",
        output.status.code()
    );
    assert_eq!(
        stdout, "The service listens on port 8080.\n",
        "a notice between the chunks keeps the reply on stdout; stderr was {stderr:?}"
    );
    assert!(
        stderr.contains("MCP \"docs\": sign-in required"),
        "the notice itself goes to stderr: {stderr:?}"
    );
    assert!(
        !stderr.contains("The service "),
        "the reply appears once, on stdout: {stderr:?}"
    );
}

/// `exec resume` settles a turn an earlier invocation left running before it
/// submits its prompt; that turn's reply is not this run's answer.
#[test]
fn exec_resume_keeps_the_running_turns_reply_off_stdout() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.hello(&[]);
        let attach = wire.read("session.attach");
        wire.reply(&attach, attach_record(&attach));
        let history = wire.read("session.history");
        wire.reply(&history, json!({"events":[],"through":8,"nextAfter":8,"done":true}));
        wire.no_pending_questions(8);
        let idle = wire.read("session.idle");
        wire.event(10,"text",json!({"turn_id":"old-turn","request_id":"old-request","delta":"The earlier answer."}));
        wire.event(11,"model-output-end",json!({"turn_id":"old-turn","request_id":"old-request","status":"completed","finish_reason":"end_turn"}));
        wire.notice("session.event",json!({"sessionId":"session","branchId":"main","event":{"id":"old-end","sequence":12,"create_at":1,"type":"turn","name":"end","data":{"turn_id":"old-turn","status":"completed","reason":"done"}}}));
        wire.reply(&idle, json!({"sequence":12}));
        let submit = wire.read("session.submit");
        wire.reply(&submit, json!({"event":{}}));
        let idle = wire.read("session.idle");
        wire.notice("session.event",json!({"sessionId":"session","branchId":"main","event":{"id":"new-input","sequence":13,"create_at":1,"type":"input","name":"message","data":{"submission_id":submit["params"]["submissionId"],"user":{"content":[{"type":"text","text":"continue"}]},"mode":"queue"}}}));
        wire.notice("session.event",json!({"sessionId":"session","branchId":"main","event":{"id":"new-start","sequence":14,"create_at":1,"type":"turn","name":"start","data":{"turn_id":"new-turn"}}}));
        wire.notice("session.event",json!({"sessionId":"session","branchId":"main","event":{"id":"new-consumed","sequence":15,"create_at":1,"type":"input","name":"consumed","data":{"input_id":"new-input"}}}));
        wire.event(17,"text",json!({"turn_id":"new-turn","request_id":"new-request","delta":"The new answer."}));
        wire.notice("session.event",json!({"sessionId":"session","branchId":"main","event":{"id":"new-end","sequence":18,"create_at":1,"type":"turn","name":"end","data":{"turn_id":"new-turn","status":"completed","reason":"done"}}}));
        wire.reply(&idle, json!({"sequence":18}));
        wire.detach();
    });
    let output = wait(
        home.command()
            .args(["exec", "resume", "session", "-p", "continue"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    daemon.join().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "status {:?} stdout {stdout:?} stderr {stderr:?}",
        output.status.code()
    );
    assert_eq!(
        stdout, "The new answer.\n",
        "only the submitted prompt's turn writes the reply; stderr was {stderr:?}"
    );
    assert!(
        stderr.contains("The earlier answer."),
        "the settled turn's words go to stderr: {stderr:?}"
    );
}

#[test]
fn exec_leaves_no_partial_text_on_stdout_after_an_interrupt() {
    let home = Home::new();
    let (rendered, wait_rendered) = std::sync::mpsc::channel();
    let daemon = home.daemon(move |wire| {
        wire.start(false);
        let submit = wire.read("session.submit");
        wire.reply(&submit, json!({"event":{"id":"input"}}));
        let idle = wire.read("session.idle");
        open_turn(wire, "turn", "input");
        wire.notice("session.model.progress",json!({"sessionId":"session","branchId":"main","requestId":"request","textOffset":0,"text":"I will read the config.","thinking":"","thinkingOffset":0}));
        wire.event(12,"text",json!({"turn_id":"turn","request_id":"request","delta":"I will read the config."}));
        rendered.send(()).unwrap();
        let stop = wire.read("session.stop");
        wire.event(14,"model-output-end",json!({"turn_id":"turn","request_id":"request","status":"cancelled","reason":"interrupted"}));
        wire.end(json!({"turn_id":"turn","status":"cancelled","reason":"interrupted"}));
        wire.reply(&stop, json!({"sequence":30}));
        wire.reply(&idle, json!({"sequence":30}));
        wire.detach();
    });
    let child = home
        .command()
        .args(["exec", "-p", "Which port does the service use?"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    wait_rendered.recv_timeout(PATIENCE).unwrap();
    assert_eq!(unsafe { libc::kill(child.id() as i32, libc::SIGINT) }, 0);
    let output = wait(child);
    daemon.join().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.code(),
        Some(130),
        "stdout {stdout:?} stderr {stderr:?}"
    );
    assert!(
        stdout.is_empty(),
        "stdout holds no partial text after an interrupt; stdout was {stdout:?}"
    );
    assert!(
        stderr.contains("I will read the config."),
        "the interrupted request's words go to stderr: {stderr:?}"
    );
}

/// A SIGINT handled before the first text closes the reply stream: the text
/// and successful terminal the daemon already queued cannot reach stdout.
#[test]
fn exec_handles_sigint_before_the_first_text() {
    let home = Home::new();
    let (submitted, wait_submitted) = std::sync::mpsc::channel();
    let daemon = home.daemon(move |wire| {
        wire.start(false);
        let submit = wire.read("session.submit");
        wire.reply(&submit, json!({"event":{"id":"input"}}));
        let idle = wire.read("session.idle");
        submitted.send(()).unwrap();
        let stop = wire.read("session.stop");
        wire.event(12,"text",json!({"turn_id":"turn","request_id":"request","delta":"The service listens on port 8080."}));
        wire.event(14,"model-output-end",json!({"turn_id":"turn","request_id":"request","status":"completed","finish_reason":"end_turn"}));
        wire.end(json!({"turn_id":"turn","status":"completed","reason":"done"}));
        wire.reply(&stop, json!({"sequence":30}));
        wire.reply(&idle, json!({"sequence":30}));
        wire.detach();
    });
    let child = home
        .command()
        .args(["exec", "-p", "Which port does the service use?"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    wait_submitted.recv_timeout(PATIENCE).unwrap();
    assert_eq!(unsafe { libc::kill(child.id() as i32, libc::SIGINT) }, 0);
    let output = wait(child);
    daemon.join().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.code(),
        Some(130),
        "stdout {stdout:?} stderr {stderr:?}"
    );
    assert!(
        stdout.is_empty(),
        "a closed stream keeps the queued reply off stdout; stdout was {stdout:?}"
    );
    assert!(
        stderr.contains("The service listens on port 8080."),
        "the text after the interrupt is diagnostics: {stderr:?}"
    );
}

/// An earlier request's late model end changes only its own entry: the text
/// the recovered request already wrote stays whole on stdout.
#[test]
fn exec_keeps_a_recovered_reply_when_an_earlier_request_ends_late() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.start(false);
        let submit = wire.read("session.submit");
        wire.reply(&submit, json!({"event":{"id":"input"}}));
        let idle = wire.read("session.idle");
        open_turn(wire, "turn", "input");
        wire.event(11,"model-start",json!({"turn_id":"turn","request_id":"earlier"}));
        wire.event(12,"model-start",json!({"turn_id":"turn","request_id":"recovered"}));
        wire.notice("session.model.progress",json!({"sessionId":"session","branchId":"main","requestId":"recovered","textOffset":0,"text":"The ","thinking":"","thinkingOffset":0}));
        wire.event(14,"model-output-end",json!({"turn_id":"turn","request_id":"earlier","status":"completed","finish_reason":"end_turn"}));
        wire.event(16,"text",json!({"turn_id":"turn","request_id":"recovered","delta":"The answer."}));
        wire.event(18,"model-output-end",json!({"turn_id":"turn","request_id":"recovered","status":"completed","finish_reason":"end_turn"}));
        wire.end(json!({"turn_id":"turn","status":"completed","reason":"done"}));
        wire.reply(&idle, json!({"sequence":30}));
        wire.detach();
    });
    let output = wait(
        home.command()
            .args(["exec", "-p", "Which port does the service use?"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    daemon.join().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "status {:?} stdout {stdout:?} stderr {stderr:?}",
        output.status.code()
    );
    assert_eq!(
        stdout, "The answer.\n",
        "the recovered reply stays whole on stdout; stderr was {stderr:?}"
    );
    assert!(
        !stderr.contains("The answer."),
        "the recovered reply appears once, on stdout: {stderr:?}"
    );
}

/// A request that started earlier can end after the next request already
/// wrote durable text. The later request's text is still the turn's reply.
/// Arrival order: `turn/start`, `input/consumed`, both `model-start`s, the
/// recovered request's text, the earlier request's late `model-output-end`,
/// the recovered request's `model-output-end`, `turn/end`.
#[test]
fn exec_keeps_a_recovered_reply_when_the_earlier_request_ends_after_it() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.start(false);
        let submit = wire.read("session.submit");
        wire.reply(&submit, json!({"event":{"id":"input"}}));
        let idle = wire.read("session.idle");
        open_turn(wire, "turn", "input");
        wire.event(11,"model-start",json!({"turn_id":"turn","request_id":"earlier"}));
        wire.event(12,"model-start",json!({"turn_id":"turn","request_id":"recovered"}));
        wire.event(13,"text",json!({"turn_id":"turn","request_id":"recovered","delta":"The answer."}));
        wire.event(14,"model-output-end",json!({"turn_id":"turn","request_id":"earlier","status":"completed","finish_reason":"end_turn"}));
        wire.event(15,"model-output-end",json!({"turn_id":"turn","request_id":"recovered","status":"completed","finish_reason":"end_turn"}));
        wire.end(json!({"turn_id":"turn","status":"completed","reason":"done"}));
        wire.reply(&idle, json!({"sequence":30}));
        wire.detach();
    });
    let output = wait(
        home.command()
            .args(["exec", "-p", "Which port does the service use?"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    daemon.join().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "status {:?} stdout {stdout:?} stderr {stderr:?}",
        output.status.code()
    );
    assert_eq!(
        stdout, "The answer.\n",
        "the recovered reply stays whole on stdout; stderr was {stderr:?}"
    );
    assert!(
        !stderr.contains("The answer."),
        "the recovered reply appears once, on stdout: {stderr:?}"
    );
}

/// The turn that consumes the submitted input can end before the submit reply
/// names the input. Arrival order: submit request, `turn/start`,
/// `input/consumed`, `text`, `model-output-end`, `turn/end`, submit reply,
/// `session.idle` reply. The held facts are replayed when the reply names the
/// input, so the completed turn's reply still reaches stdout.
#[test]
fn exec_keeps_a_reply_whose_turn_ended_before_the_submit_reply() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.start(false);
        let submit = wire.read("session.submit");
        open_turn(wire, "turn", "input");
        wire.event(12,"text",json!({"turn_id":"turn","request_id":"request","delta":"The service listens on port 8080."}));
        wire.event(13,"model-output-end",json!({"turn_id":"turn","request_id":"request","status":"completed","finish_reason":"end_turn"}));
        wire.end(json!({"turn_id":"turn","status":"completed","reason":"done"}));
        wire.reply(&submit, json!({"event":{"id":"input"}}));
        let idle = wire.read("session.idle");
        wire.reply(&idle, json!({"sequence":30}));
        wire.detach();
    });
    let output = wait(
        home.command()
            .args(["exec", "-p", "Which port does the service use?"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    daemon.join().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "status {:?} stdout {stdout:?} stderr {stderr:?}",
        output.status.code()
    );
    assert_eq!(
        stdout, "The service listens on port 8080.\n",
        "the reply of a turn that ended before the receipt stays on stdout; stderr was {stderr:?}"
    );
    assert!(
        !stderr.contains("The service listens on port 8080."),
        "the reply appears once, on stdout: {stderr:?}"
    );
}

/// After the submitted turn ends, another turn on the branch may complete
/// before idle: its answer is not this run's reply.
#[test]
fn exec_keeps_another_turn_off_stdout_after_the_submitted_turn() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.start(false);
        let submit = wire.read("session.submit");
        wire.reply(&submit, json!({"event":{"id":"input"}}));
        let idle = wire.read("session.idle");
        open_turn(wire, "turn", "input");
        wire.event(12,"text",json!({"turn_id":"turn","request_id":"request","delta":"Our answer."}));
        wire.event(13,"model-output-end",json!({"turn_id":"turn","request_id":"request","status":"completed","finish_reason":"end_turn"}));
        wire.notice("session.event",json!({"sessionId":"session","branchId":"main","event":{"id":"our-end","sequence":14,"create_at":1,"type":"turn","name":"end","data":{"turn_id":"turn","status":"completed","reason":"done"}}}));
        wire.notice("session.event",json!({"sessionId":"session","branchId":"main","event":{"id":"later-start","sequence":15,"create_at":1,"type":"turn","name":"start","data":{"turn_id":"later"}}}));
        wire.notice("session.event",json!({"sessionId":"session","branchId":"main","event":{"id":"later-consumed","sequence":16,"create_at":1,"type":"input","name":"consumed","data":{"input_id":"another-input"}}}));
        wire.event(17,"text",json!({"turn_id":"later","request_id":"later-request","delta":"Another answer."}));
        wire.event(18,"model-output-end",json!({"turn_id":"later","request_id":"later-request","status":"completed","finish_reason":"end_turn"}));
        wire.notice("session.event",json!({"sessionId":"session","branchId":"main","event":{"id":"later-end","sequence":19,"create_at":1,"type":"turn","name":"end","data":{"turn_id":"later","status":"completed","reason":"done"}}}));
        wire.reply(&idle, json!({"sequence":30}));
        wire.detach();
    });
    let output = wait(
        home.command()
            .args(["exec", "-p", "Which port does the service use?"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    daemon.join().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "status {:?} stdout {stdout:?} stderr {stderr:?}",
        output.status.code()
    );
    assert_eq!(
        stdout, "Our answer.\n",
        "only the submitted turn writes the reply; stderr was {stderr:?}"
    );
    assert!(
        stderr.contains("Another answer."),
        "the later turn's words go to stderr: {stderr:?}"
    );
}

/// A later turn can start, and forward its live text, before the submitted
/// turn's durable `TurnEnd` reaches the client. Live progress has no turn
/// identity, so it cannot decide where the submitted turn's held text goes.
/// Arrival order: submit request, submit reply, `turn/start`,
/// `input/consumed`, text, live text of a later turn, `model-output-end`,
/// `turn/end`, `session.idle` reply. The submitted turn's reply is the run's
/// answer, and the later turn's live text is not output.
#[test]
fn exec_ignores_a_later_turns_live_text_before_the_turn_ends() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.start(false);
        let submit = wire.read("session.submit");
        wire.reply(&submit, json!({"event":{"id":"input"}}));
        let idle = wire.read("session.idle");
        open_turn(wire, "turn", "input");
        wire.event(12,"text",json!({"turn_id":"turn","request_id":"request","delta":"The service listens on port 8080."}));
        wire.notice("session.model.progress",json!({"sessionId":"session","branchId":"main","requestId":"later-request","textOffset":0,"text":"Another turn starts.","thinking":"","thinkingOffset":0}));
        wire.event(14,"model-output-end",json!({"turn_id":"turn","request_id":"request","status":"completed","finish_reason":"end_turn"}));
        wire.end(json!({"turn_id":"turn","status":"completed","reason":"done"}));
        wire.reply(&idle, json!({"sequence":30}));
        wire.detach();
    });
    let output = wait(
        home.command()
            .args(["exec", "-p", "Which port does the service use?"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    daemon.join().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "status {:?} stdout {stdout:?} stderr {stderr:?}",
        output.status.code()
    );
    assert_eq!(
        stdout, "The service listens on port 8080.\n",
        "the submitted turn's reply stays the answer; stderr was {stderr:?}"
    );
    assert!(
        !stdout.contains("Another turn starts."),
        "a later turn's live text never reaches stdout: {stdout:?}"
    );
    assert!(
        !stderr.contains("Another turn starts."),
        "live text is not diagnostics either: {stderr:?}"
    );
}

/// A turn that did not consume the submitted input cannot write its reply.
#[test]
fn exec_ignores_a_turn_that_did_not_consume_its_prompt() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.start(false);
        let submit = wire.read("session.submit");
        wire.reply(&submit, json!({"event":{"id":"input"}}));
        let idle = wire.read("session.idle");
        wire.notice("session.event",json!({"sessionId":"session","branchId":"main","event":{"id":"other-start","sequence":9,"create_at":1,"type":"turn","name":"start","data":{"turn_id":"other"}}}));
        wire.notice("session.event",json!({"sessionId":"session","branchId":"main","event":{"id":"other-consumed","sequence":10,"create_at":1,"type":"input","name":"consumed","data":{"input_id":"other-input"}}}));
        wire.event(11,"text",json!({"turn_id":"other","request_id":"other-request","delta":"The other answer."}));
        wire.event(12,"model-output-end",json!({"turn_id":"other","request_id":"other-request","status":"completed","finish_reason":"end_turn"}));
        wire.notice("session.event",json!({"sessionId":"session","branchId":"main","event":{"id":"other-end","sequence":13,"create_at":1,"type":"turn","name":"end","data":{"turn_id":"other","status":"completed","reason":"done"}}}));
        wire.notice("session.event",json!({"sessionId":"session","branchId":"main","event":{"id":"turn-start","sequence":14,"create_at":1,"type":"turn","name":"start","data":{"turn_id":"turn"}}}));
        wire.notice("session.event",json!({"sessionId":"session","branchId":"main","event":{"id":"input-consumed","sequence":15,"create_at":1,"type":"input","name":"consumed","data":{"input_id":"input"}}}));
        wire.event(16,"text",json!({"turn_id":"turn","request_id":"request","delta":"Our answer."}));
        wire.event(17,"model-output-end",json!({"turn_id":"turn","request_id":"request","status":"completed","finish_reason":"end_turn"}));
        wire.end(json!({"turn_id":"turn","status":"completed","reason":"done"}));
        wire.reply(&idle, json!({"sequence":30}));
        wire.detach();
    });
    let output = wait(
        home.command()
            .args(["exec", "-p", "Which port does the service use?"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    daemon.join().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "status {:?} stdout {stdout:?} stderr {stderr:?}",
        output.status.code()
    );
    assert_eq!(
        stdout, "Our answer.\n",
        "the submitted input's turn alone writes the reply; stderr was {stderr:?}"
    );
    assert!(
        stderr.contains("The other answer."),
        "the other turn's words go to stderr: {stderr:?}"
    );
}

/// A deleted submission has no answer: the stream closes at once and the run
/// exits as a failure, even when another turn completes successfully.
#[test]
fn exec_closes_its_reply_stream_when_its_input_is_deleted() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.start(false);
        let submit = wire.read("session.submit");
        wire.reply(&submit, json!({"event":{"id":"input"}}));
        let idle = wire.read("session.idle");
        wire.notice("session.event",json!({"sessionId":"session","branchId":"main","event":{"id":"deleted","sequence":9,"create_at":1,"type":"input","name":"deleted","data":{"input_id":"input"}}}));
        wire.notice("session.event",json!({"sessionId":"session","branchId":"main","event":{"id":"later-start","sequence":10,"create_at":1,"type":"turn","name":"start","data":{"turn_id":"later"}}}));
        wire.event(11,"text",json!({"turn_id":"later","request_id":"later-request","delta":"Another answer."}));
        wire.event(12,"model-output-end",json!({"turn_id":"later","request_id":"later-request","status":"completed","finish_reason":"end_turn"}));
        wire.notice("session.event",json!({"sessionId":"session","branchId":"main","event":{"id":"later-end","sequence":13,"create_at":1,"type":"turn","name":"end","data":{"turn_id":"later","status":"completed","reason":"done"}}}));
        wire.reply(&idle, json!({"sequence":30}));
        wire.detach();
    });
    let output = wait(
        home.command()
            .args(["exec", "-p", "Which port does the service use?"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    daemon.join().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.code(),
        Some(1),
        "stdout {stdout:?} stderr {stderr:?}"
    );
    assert!(
        stdout.is_empty(),
        "a deleted submission leaves no reply on stdout; stdout was {stdout:?}"
    );
    assert!(
        stderr.contains("The submitted input was deleted"),
        "the deletion is reported: {stderr:?}"
    );
    assert!(
        stderr.contains("Another answer."),
        "a later turn's words are diagnostics: {stderr:?}"
    );
}

/// The submitted input can be deleted before the submit reply names it.
/// Arrival order: submit request, `input/deleted`, submit reply, then another
/// turn that completes. The deletion is replayed on the receipt, so the stream
/// closes, the deletion is reported and the run exits 1.
#[test]
fn exec_reports_a_deletion_that_arrives_before_the_submit_reply() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.start(false);
        let submit = wire.read("session.submit");
        wire.notice("session.event",json!({"sessionId":"session","branchId":"main","event":{"id":"deleted","sequence":9,"create_at":1,"type":"input","name":"deleted","data":{"input_id":"input"}}}));
        wire.reply(&submit, json!({"event":{"id":"input"}}));
        let idle = wire.read("session.idle");
        wire.notice("session.event",json!({"sessionId":"session","branchId":"main","event":{"id":"later-start","sequence":10,"create_at":1,"type":"turn","name":"start","data":{"turn_id":"later"}}}));
        wire.event(11,"text",json!({"turn_id":"later","request_id":"later-request","delta":"Another answer."}));
        wire.event(12,"model-output-end",json!({"turn_id":"later","request_id":"later-request","status":"completed","finish_reason":"end_turn"}));
        wire.notice("session.event",json!({"sessionId":"session","branchId":"main","event":{"id":"later-end","sequence":13,"create_at":1,"type":"turn","name":"end","data":{"turn_id":"later","status":"completed","reason":"done"}}}));
        wire.reply(&idle, json!({"sequence":30}));
        wire.detach();
    });
    let output = wait(
        home.command()
            .args(["exec", "-p", "Which port does the service use?"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    daemon.join().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.code(),
        Some(1),
        "stdout {stdout:?} stderr {stderr:?}"
    );
    assert!(
        stdout.is_empty(),
        "a deletion seen before the receipt leaves no reply on stdout; stdout was {stdout:?}"
    );
    assert!(
        stderr.contains("The submitted input was deleted"),
        "the deletion is reported: {stderr:?}"
    );
    assert!(
        stderr.contains("Another answer."),
        "a later turn's words are diagnostics: {stderr:?}"
    );
}

/// A failed turn that is not the submitted one does not set the run's exit
/// code. Arrival order: submit → submit reply → `turn/start`/`input/consumed`/
/// `turn/end` of an unrelated turn that failed → the submitted turn's start,
/// consumption, text and successful end → another turn that failed after it →
/// `session.idle` reply. The submitted turn's reply is the answer and the run
/// exits 0.
#[test]
fn exec_keeps_its_exit_code_when_another_turn_fails() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.start(false);
        let submit = wire.read("session.submit");
        wire.reply(&submit, json!({"event":{"id":"input"}}));
        let idle = wire.read("session.idle");
        wire.notice("session.event",json!({"sessionId":"session","branchId":"main","event":{"id":"other-start","sequence":9,"create_at":1,"type":"turn","name":"start","data":{"turn_id":"other"}}}));
        wire.notice("session.event",json!({"sessionId":"session","branchId":"main","event":{"id":"other-consumed","sequence":10,"create_at":1,"type":"input","name":"consumed","data":{"input_id":"other-input"}}}));
        wire.notice("session.event",json!({"sessionId":"session","branchId":"main","event":{"id":"other-end","sequence":11,"create_at":1,"type":"turn","name":"end","data":{"turn_id":"other","status":"failed","error":"fixture error"}}}));
        wire.notice("session.event",json!({"sessionId":"session","branchId":"main","event":{"id":"turn-start","sequence":12,"create_at":1,"type":"turn","name":"start","data":{"turn_id":"turn"}}}));
        wire.notice("session.event",json!({"sessionId":"session","branchId":"main","event":{"id":"input-consumed","sequence":13,"create_at":1,"type":"input","name":"consumed","data":{"input_id":"input"}}}));
        wire.event(14,"text",json!({"turn_id":"turn","request_id":"request","delta":"The service listens on port 8080."}));
        wire.event(15,"model-output-end",json!({"turn_id":"turn","request_id":"request","status":"completed","finish_reason":"end_turn"}));
        wire.notice("session.event",json!({"sessionId":"session","branchId":"main","event":{"id":"turn-end","sequence":16,"create_at":1,"type":"turn","name":"end","data":{"turn_id":"turn","status":"completed","reason":"done"}}}));
        wire.notice("session.event",json!({"sessionId":"session","branchId":"main","event":{"id":"later-start","sequence":17,"create_at":1,"type":"turn","name":"start","data":{"turn_id":"later"}}}));
        wire.notice("session.event",json!({"sessionId":"session","branchId":"main","event":{"id":"later-consumed","sequence":18,"create_at":1,"type":"input","name":"consumed","data":{"input_id":"another-input"}}}));
        wire.event(19,"text",json!({"turn_id":"later","request_id":"later-request","delta":"Another answer."}));
        wire.event(20,"model-output-end",json!({"turn_id":"later","request_id":"later-request","status":"completed","finish_reason":"end_turn"}));
        wire.notice("session.event",json!({"sessionId":"session","branchId":"main","event":{"id":"later-end","sequence":21,"create_at":1,"type":"turn","name":"end","data":{"turn_id":"later","status":"failed","error":"fixture error"}}}));
        wire.reply(&idle, json!({"sequence":30}));
        wire.detach();
    });
    let output = wait(
        home.command()
            .args(["exec", "-p", "Which port does the service use?"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    daemon.join().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.code(),
        Some(0),
        "another turn's failure does not set the run's exit code: stdout {stdout:?} stderr {stderr:?}"
    );
    assert_eq!(
        stdout, "The service listens on port 8080.\n",
        "the submitted turn's reply stays the answer; stderr was {stderr:?}"
    );
    assert!(
        stderr.contains("Another answer."),
        "the later turn's words are diagnostics: {stderr:?}"
    );
}

/// A question a line run cannot answer stops the turn with exit 3; the text
/// and successful terminal that follow cannot reach stdout.
#[test]
fn exec_stops_at_a_question_without_a_terminal() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.start(false);
        let submit = wire.read("session.submit");
        wire.reply(&submit, json!({"event":{"id":"input"}}));
        let idle = wire.read("session.idle");
        open_turn(wire, "turn", "input");
        wire.event(11,"tool-call",json!({"turn_id":"turn","request_id":"request","calls":[{"id":"question","name":"ask_user","input":{"questions":[{"header":"Database","question":"Which database should the service use?","options":[{"label":"PostgreSQL"},{"label":"SQLite"}]}]}}]}));
        let stop = wire.read("session.stop");
        wire.event(13,"text",json!({"turn_id":"turn","request_id":"request","delta":"The service listens on port 8080."}));
        wire.event(14,"model-output-end",json!({"turn_id":"turn","request_id":"request","status":"completed","finish_reason":"end_turn"}));
        wire.end(json!({"turn_id":"turn","status":"completed","reason":"done"}));
        wire.reply(&stop, json!({"sequence":30}));
        wire.reply(&idle, json!({"sequence":30}));
        wire.detach();
    });
    let output = wait(
        home.command()
            .args(["exec", "-p", "plan"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    daemon.join().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.code(),
        Some(3),
        "stdout {stdout:?} stderr {stderr:?}"
    );
    assert!(
        stdout.is_empty(),
        "a question stop leaves no reply on stdout; stdout was {stdout:?}"
    );
    assert!(
        stderr.contains("The service listens on port 8080."),
        "the text after the stop is diagnostics: {stderr:?}"
    );
}

/// An approval a line run cannot review stops the turn with exit 3; the text
/// and successful terminal that follow cannot reach stdout.
#[test]
fn exec_stops_at_an_approval_without_a_terminal() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.start(false);
        let submit = wire.read("session.submit");
        wire.reply(&submit, json!({"event":{"id":"input"}}));
        let idle = wire.read("session.idle");
        open_turn(wire, "turn", "input");
        wire.notice("approval.request",json!({"sessionId":"session","branchId":"main","requestId":"read-approval","source":{"kind":"model"},"call":{"name":"read","input":{"path":"/etc/hosts"}},"reason":"Reading outside the session workspace requires approval: /etc/hosts"}));
        let approval = wire.read("approval.respond");
        assert_eq!(approval["params"]["decision"], "deny");
        let stop = wire.read("session.stop");
        wire.event(13,"text",json!({"turn_id":"turn","request_id":"request","delta":"The service listens on port 8080."}));
        wire.event(14,"model-output-end",json!({"turn_id":"turn","request_id":"request","status":"completed","finish_reason":"end_turn"}));
        wire.end(json!({"turn_id":"turn","status":"completed","reason":"done"}));
        wire.reply(&stop, json!({"sequence":30}));
        wire.reply(&idle, json!({"sequence":30}));
        wire.reply(&approval, json!({}));
        wire.detach();
    });
    let output = wait(
        home.command()
            .args(["exec", "--prompt", "read /etc/hosts"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    daemon.join().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.code(),
        Some(3),
        "stdout {stdout:?} stderr {stderr:?}"
    );
    assert!(
        stdout.is_empty(),
        "an approval stop leaves no reply on stdout; stdout was {stdout:?}"
    );
    assert!(
        stderr.contains("The service listens on port 8080."),
        "the text after the stop is diagnostics: {stderr:?}"
    );
}

/// A one-shot `--prompt` run is not `exec`, so an interruption that finds no
/// text open keeps the blank line it has always written to stdout.
#[test]
fn a_one_shot_prompt_interrupt_keeps_its_blank_stdout_line() {
    let home = Home::new();
    let (submitted, wait_submitted) = std::sync::mpsc::channel();
    let daemon = home.daemon(move |wire| {
        wire.start(false);
        let submit = wire.read("session.submit");
        wire.reply(&submit, json!({"event":{}}));
        let idle = wire.read("session.idle");
        submitted.send(()).unwrap();
        let stop = wire.read("session.stop");
        wire.end(json!({"turn_id":"turn","status":"cancelled","reason":"interrupted"}));
        wire.reply(&stop, json!({"sequence":30}));
        wire.reply(&idle, json!({"sequence":30}));
        wire.detach();
    });
    let child = home
        .command()
        .args(["--prompt", "Which port does the service use?"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    wait_submitted.recv_timeout(PATIENCE).unwrap();
    assert_eq!(unsafe { libc::kill(child.id() as i32, libc::SIGINT) }, 0);
    let output = wait(child);
    daemon.join().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.code(),
        Some(130),
        "stdout {stdout:?} stderr {stderr:?}"
    );
    assert_eq!(
        stdout, "\n",
        "an interruption with no text open keeps the run's blank stdout line; stderr was {stderr:?}"
    );
}

#[test]
fn exec_scales_a_large_photo_and_reports_both_sizes() {
    let home = Home::new();
    // Quality-100 noise over the raw bytes that fit the base64 limit, while a
    // debug-build decode and scale stay well inside the harness patience.
    let bytes = noise_jpeg(2100, 700);
    assert!(
        base64_len(bytes.len() as u64) > MAX_BASE64_BYTES,
        "{}",
        bytes.len()
    );
    let photo = home.0.join("photo.jpg");
    fs::write(&photo, &bytes).unwrap();
    let daemon = home.daemon(|wire| {
        wire.start(false);
        let (source, sent) = import_image(wire, "image/jpeg");
        let submit = wire.read("session.submit");
        let content = submit["params"]["user"]["content"].as_array().unwrap();
        let image = content
            .iter()
            .find(|block| block["type"] == "image")
            .expect("the request carries the image");
        assert_eq!(image["source"], source);
        assert_eq!(image["source"]["mediaType"], "image/jpeg");

        assert_eq!(&sent[..3], b"\xff\xd8\xff");
        assert!(
            base64_len(sent.len() as u64) <= MAX_BASE64_BYTES,
            "{}",
            sent.len()
        );
        let image = image::load_from_memory(&sent).unwrap();
        assert!(
            image.width() <= MAX_EDGE && image.height() <= MAX_EDGE,
            "{}×{}",
            image.width(),
            image.height()
        );
        wire.reply(&submit, json!({"event":{}}));
        let idle = wire.read("session.idle");
        wire.reply(&idle, json!({"sequence":30}));
        wire.detach();
    });
    let output = wait(
        home.command()
            .args([
                "exec",
                "--image",
                photo.to_str().unwrap(),
                "-p",
                "Describe this",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    daemon.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Scaled photo.jpg from 2100×700 to "),
        "{stderr}"
    );
    assert!(stderr.contains("px (image/jpeg)"), "{stderr}");
}

/// A PNG within the edge limit whose base64 is over the byte limit is
/// re-encoded as JPEG at the same size: the size alone would hide that the
/// screenshot is now lossy, so exec says the type changed.
#[test]
fn exec_reports_a_reencode_that_keeps_the_size() {
    let home = Home::new();
    let bytes = noise_png(1500, 1500);
    assert!(
        base64_len(bytes.len() as u64) > MAX_BASE64_BYTES,
        "{}",
        bytes.len()
    );
    let shot = home.0.join("shot.png");
    fs::write(&shot, &bytes).unwrap();
    let daemon = home.daemon(|wire| {
        wire.start(false);
        let (source, sent) = import_image(wire, "image/jpeg");
        let submit = wire.read("session.submit");
        let content = submit["params"]["user"]["content"].as_array().unwrap();
        let image = content
            .iter()
            .find(|block| block["type"] == "image")
            .expect("the request carries the image");
        assert_eq!(image["source"], source);
        assert_eq!(image["source"]["mediaType"], "image/jpeg");

        assert_eq!(&sent[..3], b"\xff\xd8\xff");
        let sent = image::load_from_memory(&sent).unwrap();
        assert_eq!((sent.width(), sent.height()), (1500, 1500));
        wire.reply(&submit, json!({"event":{}}));
        let idle = wire.read("session.idle");
        wire.reply(&idle, json!({"sequence":30}));
        wire.detach();
    });
    let output = wait(
        home.command()
            .args(["exec", "--image", shot.to_str().unwrap()])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    daemon.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Re-encoded shot.png as image/jpeg (1500×1500 px)"),
        "{stderr}"
    );
}

/// A JPEG within the edge and byte limits whose EXIF orientation turns it
/// upright is re-encoded as JPEG at the same size: the file differs from what
/// is sent only in its encoding, and exec says so.
#[test]
fn exec_reports_a_photo_turned_upright_at_the_same_size() {
    let home = Home::new();
    let photo = home.0.join("photo.jpg");
    fs::write(&photo, oriented_jpeg(40, 20, 3)).unwrap();
    let daemon = home.daemon(|wire| {
        wire.start(false);
        let (source, sent) = import_image(wire, "image/jpeg");
        let submit = wire.read("session.submit");
        let content = submit["params"]["user"]["content"].as_array().unwrap();
        let image = content
            .iter()
            .find(|block| block["type"] == "image")
            .expect("the request carries the image");
        assert_eq!(image["source"], source);
        assert_eq!(image["source"]["mediaType"], "image/jpeg");
        assert!(image::load_from_memory(&sent).is_ok());
        wire.reply(&submit, json!({"event":{}}));
        let idle = wire.read("session.idle");
        wire.reply(&idle, json!({"sequence":30}));
        wire.detach();
    });
    let output = wait(
        home.command()
            .args(["exec", "--image", photo.to_str().unwrap()])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    daemon.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Re-encoded photo.jpg as image/jpeg (40×20 px)"),
        "{stderr}"
    );
}

/// The 90° turn swaps the axes before the sizes are compared, so a square
/// result keeps the size too: exec still says the encoding changed.
#[test]
fn exec_reports_a_quarter_turn_at_the_same_size() {
    let home = Home::new();
    let photo = home.0.join("photo.jpg");
    fs::write(&photo, oriented_jpeg(40, 20, 6)).unwrap();
    let daemon = home.daemon(|wire| {
        wire.start(false);
        let (source, sent) = import_image(wire, "image/jpeg");
        let submit = wire.read("session.submit");
        let content = submit["params"]["user"]["content"].as_array().unwrap();
        let image = content
            .iter()
            .find(|block| block["type"] == "image")
            .expect("the request carries the image");
        assert_eq!(image["source"], source);

        let sent = image::load_from_memory(&sent).unwrap();
        assert_eq!((sent.width(), sent.height()), (20, 40));
        wire.reply(&submit, json!({"event":{}}));
        let idle = wire.read("session.idle");
        wire.reply(&idle, json!({"sequence":30}));
        wire.detach();
    });
    let output = wait(
        home.command()
            .args(["exec", "--image", photo.to_str().unwrap()])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    daemon.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Re-encoded photo.jpg as image/jpeg (20×40 px)"),
        "{stderr}"
    );
}

/// A JPEG within the edge limit whose base64 is over the byte limit fits once
/// it is re-encoded at quality 85, again without a size or type change.
#[test]
fn exec_reports_a_jpeg_that_fits_only_after_the_reencode() {
    let home = Home::new();
    let bytes = noise_jpeg(1500, 1500);
    assert!(
        base64_len(bytes.len() as u64) > MAX_BASE64_BYTES,
        "{}",
        bytes.len()
    );
    let photo = home.0.join("photo.jpg");
    fs::write(&photo, &bytes).unwrap();
    let daemon = home.daemon(|wire| {
        wire.start(false);
        let (source, sent) = import_image(wire, "image/jpeg");
        let submit = wire.read("session.submit");
        let content = submit["params"]["user"]["content"].as_array().unwrap();
        let image = content
            .iter()
            .find(|block| block["type"] == "image")
            .expect("the request carries the image");
        assert_eq!(image["source"], source);
        assert_eq!(image["source"]["mediaType"], "image/jpeg");

        let sent = image::load_from_memory(&sent).unwrap();
        assert_eq!((sent.width(), sent.height()), (1500, 1500));
        wire.reply(&submit, json!({"event":{}}));
        let idle = wire.read("session.idle");
        wire.reply(&idle, json!({"sequence":30}));
        wire.detach();
    });
    let output = wait(
        home.command()
            .args(["exec", "--image", photo.to_str().unwrap()])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    daemon.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Re-encoded photo.jpg as image/jpeg (1500×1500 px)"),
        "{stderr}"
    );
}

#[test]
fn exec_reports_the_first_frame_of_an_animation() {
    let home = Home::new();
    let mut gif = Vec::new();
    {
        let mut encoder = image::codecs::gif::GifEncoder::new(&mut gif);
        encoder
            .encode_frames([
                image::Frame::new(image::RgbaImage::from_pixel(
                    2,
                    2,
                    image::Rgba([255, 0, 0, 255]),
                )),
                image::Frame::new(image::RgbaImage::from_pixel(
                    2,
                    2,
                    image::Rgba([0, 0, 255, 255]),
                )),
            ])
            .unwrap();
    }
    let animation = home.0.join("anim.gif");
    fs::write(&animation, &gif).unwrap();
    let daemon = home.daemon(|wire| {
        wire.start(false);
        let (source, sent) = import_image(wire, "image/jpeg");
        let submit = wire.read("session.submit");
        let content = submit["params"]["user"]["content"].as_array().unwrap();
        let image = content
            .iter()
            .find(|block| block["type"] == "image")
            .expect("the request carries the image");
        assert_eq!(image["source"], source);
        assert_eq!(image["source"]["mediaType"], "image/jpeg");
        assert!(image::load_from_memory(&sent).is_ok());
        wire.reply(&submit, json!({"event":{}}));
        let idle = wire.read("session.idle");
        wire.reply(&idle, json!({"sequence":30}));
        wire.detach();
    });
    let output = wait(
        home.command()
            .args(["exec", "--image", animation.to_str().unwrap()])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    daemon.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Only the first frame of anim.gif is sent (image/jpeg)"),
        "{stderr}"
    );
}

#[test]
fn exec_images_preserve_format_bytes_order_and_invoking_directory() {
    use base64::{Engine, engine::general_purpose::STANDARD};
    for source in [
        "images",
        "prompt",
        "empty",
        "stdin",
        "whitespace",
        "whitespace_prompt",
        "symlink",
        "twenty",
        "terminal",
    ] {
        let home = Home::new();
        let workspace = home.0.join("workspace");
        fs::create_dir(&workspace).unwrap();
        let mut images = Vec::new();
        let mut paths = Vec::new();
        for (index, (format, media_type)) in [
            (image::ImageFormat::Png, "image/png"),
            (image::ImageFormat::Jpeg, "image/jpeg"),
            (image::ImageFormat::WebP, "image/webp"),
            (image::ImageFormat::Gif, "image/gif"),
        ]
        .into_iter()
        .enumerate()
        {
            let bytes = image_fixture(format, 1, if index == 0 { MAX_EDGE } else { 1 });
            let path = format!("image {index}.txt");
            fs::write(home.0.join(&path), &bytes).unwrap();
            images.push(json!({"type":"image","source":{"type":"base64","mediaType":media_type,"data":STANDARD.encode(bytes)}}));
            paths.push(path);
        }
        if source == "symlink" {
            let link = home.0.join("linked.png");
            std::os::unix::fs::symlink(home.0.join(&paths[0]), &link).unwrap();
            paths[0] = link.to_str().unwrap().into();
        }
        if source == "twenty" {
            images = images.iter().cycle().take(20).cloned().collect();
            paths = paths.iter().cycle().take(20).cloned().collect();
        }
        let text = match source {
            "prompt" => Some("compare /exit literally"),
            "empty" => Some(""),
            "stdin" => Some("stdin\r\nwith trailing newline\n"),
            "whitespace" | "whitespace_prompt" => Some(" \t\n"),
            _ => None,
        };
        let mut expected = Vec::new();
        if let Some(text) = text.filter(|text| !text.trim().is_empty()) {
            expected.push(json!({"type":"text","text":text}));
        }
        expected.extend(images);
        let daemon = home.daemon(move |wire| {
            wire.start(source == "prompt");
            for block in &mut expected {
                if block["type"] == "image" {
                    let (source, bytes) =
                        import_image(wire, block["source"]["mediaType"].as_str().unwrap());
                    assert_eq!(STANDARD.encode(bytes), block["source"]["data"]);
                    block["source"] = source;
                }
            }
            let submit = wire.read("session.submit");
            assert_eq!(submit["params"]["user"]["content"], json!(expected));
            assert!(submit["params"].get("text").is_none());
            assert_eq!(submit["params"]["mode"], "queue");
            uuid::Uuid::parse_str(submit["params"]["submissionId"].as_str().unwrap()).unwrap();
            wire.reply(&submit, json!({"event":{}}));
            let idle = wire.read("session.idle");
            wire.reply(&idle, json!({"sequence":30}));
            wire.detach();
        });
        let mut command = home.command();
        command
            .current_dir(&home.0)
            .args(["--cwd", workspace.to_str().unwrap(), "exec"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if source == "prompt" {
            command.args(["resume", "session", "--json"]);
        }
        if matches!(source, "prompt" | "empty" | "whitespace_prompt") {
            command.args(["-p", text.unwrap()]);
        }
        if matches!(source, "stdin" | "whitespace") {
            command.stdin(Stdio::piped());
        }
        for path in paths {
            command.arg("--image").arg(path);
        }
        let output = if source == "terminal" {
            Output {
                status: Terminal::launch(command).wait(),
                stdout: Vec::new(),
                stderr: Vec::new(),
            }
        } else {
            let mut child = command.spawn().unwrap();
            if matches!(source, "stdin" | "whitespace") {
                child
                    .stdin
                    .take()
                    .unwrap()
                    .write_all(text.unwrap().as_bytes())
                    .unwrap();
            }
            wait(child)
        };
        daemon.join().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stdout.is_empty());
    }
}

#[test]
fn invalid_exec_images_fail_before_connecting_or_creating_home() {
    let home = Home::new();
    fs::remove_dir_all(home.0.join("rust")).unwrap();
    let png = image_fixture(image::ImageFormat::Png, 1, 1);
    let mut bad_crc = png.clone();
    bad_crc[29] ^= 1;
    let mut zero = image_fixture(image::ImageFormat::Gif, 1, 1);
    zero[6..8].fill(0);
    let mut guard = b"GIF89a".to_vec();
    guard.extend(12000u16.to_le_bytes());
    guard.extend(9000u16.to_le_bytes());
    guard.extend([0, 0, 0, 0]);
    guard.push(b';');
    for (name, bytes, reason) in [
        ("text", b"not an image".to_vec(), "PNG, JPEG, WebP or GIF"),
        ("truncated", png[..8].to_vec(), "Invalid or truncated"),
        (
            "png_header_only",
            png[..33].to_vec(),
            "Invalid or truncated",
        ),
        ("jpeg", b"\xff\xd8\xff".to_vec(), "Invalid or truncated"),
        ("gif", b"GIF89a".to_vec(), "Invalid or truncated"),
        ("webp", b"RIFF\0\0\0\0WEBP".to_vec(), "Invalid or truncated"),
        ("zero", zero, "dimensions"),
        ("crc", bad_crc, "Invalid or truncated"),
        ("guard", guard, "12000×9000"),
    ] {
        let file = home.0.join(name);
        fs::write(&file, bytes).unwrap();
        let output = home
            .command()
            .arg("exec")
            .arg("--image")
            .arg(file)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1), "{name}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(reason),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stdout.is_empty());
        assert!(!home.0.join("rust").exists());
    }
    let fifo = home.0.join("fifo");
    let name = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    for args in [
        vec!["--image", "missing.png"],
        vec!["tui", "--image", "missing.png"],
        vec!["exec", "--image"],
        vec!["exec", "--image", "missing.png"],
        vec!["exec", "--image", "http://127.0.0.1:9/missing.png"],
        vec!["exec", "--image", home.0.to_str().unwrap()],
        vec!["exec", "--image", fifo.to_str().unwrap()],
        std::iter::once("exec")
            .chain(["--image", "missing.png"].repeat(21))
            .collect(),
    ] {
        let output = home
            .command()
            .args(args)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        assert!(!home.0.join("rust").exists());
    }
}

#[test]
fn exec_images_over_the_old_frame_limit_submit_as_references() {
    let fits_raw = (MAX_BASE64_BYTES / 4 * 3) as usize;
    for size in [fits_raw, fits_raw + 1024 * 1024] {
        let home = Home::new();
        let mut png = image_fixture(image::ImageFormat::Png, 1, 1);
        png.resize(size, 0);
        let image = home.0.join("large.png");
        fs::write(&image, png).unwrap();
        let daemon = home.daemon(|wire| {
            wire.start(false);
            let expected: Vec<_> = (0..4)
                .map(|_| {
                    let (source, bytes) = import_image(wire, "image/png");
                    assert!(base64_len(bytes.len() as u64) <= MAX_BASE64_BYTES);
                    json!({"type":"image","source":source})
                })
                .collect();
            let submit = wire.read("session.submit");
            assert_eq!(submit["params"]["user"]["content"], json!(expected));
            assert!(submit.to_string().len() < 2048);
            wire.reply(&submit, json!({"event":{}}));
            let idle = wire.read("session.idle");
            wire.reply(&idle, json!({"sequence":30}));
            wire.detach();
        });
        let mut command = home.command();
        command.arg("exec").stdin(Stdio::null());
        for _ in 0..4 {
            command.arg("--image").arg(&image);
        }
        let output = command.output().unwrap();
        daemon.join().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn submit_errors_on_a_live_connection_exit_without_resending() {
    for (app_error, uncertain) in [
        (Some("submission_unknown"), true),
        (Some("publication_failed"), true),
        (None, true),
        (Some("session_invalid_input"), false),
        (Some("invalid_params"), false),
        (Some("input_blocked"), false),
    ] {
        let home = Home::new();
        let daemon = home.daemon(move |wire| {
            wire.start(false);
            let submit = wire.read("session.submit");
            if uncertain {
                wire.notice("session.event", json!({"sessionId":"session","branchId":"main","event":{"id":"committed","sequence":9,"create_at":1,"type":"input","name":"message","data":{"submission_id":submit["params"]["submissionId"],"user":{"content":[{"type":"text","text":"once"}]},"mode":"queue"}}}));
            }
            wire.send(json!({"jsonrpc":"2.0","id":submit["id"],"error":{"code":-32000,"message":"Submission failed","data":app_error.map(|code| json!({"appError":code}))}}));
            wire.detach();
        });
        let output = home
            .command()
            .args(["exec", "-p", "once"])
            .output()
            .unwrap();
        daemon.join().unwrap();
        assert_eq!(output.status.code(), Some(1));
        let diagnostic = String::from_utf8_lossy(&output.stderr);
        assert!(diagnostic.contains("Submission failed"));
        if uncertain {
            assert!(diagnostic.contains("session: session"));
            assert!(diagnostic.contains("input may already be queued"));
            assert!(diagnostic.contains("instead of sending it again"));
        } else {
            assert!(!diagnostic.contains("input may already be queued"));
            assert!(!diagnostic.contains("instead of sending it again"));
        }
    }
}

/// Appendix D 5: a blocked prompt is reported as the block, never as an uncertain
/// submission that may have been queued.
#[test]
fn a_blocked_prompt_reports_the_hooks_message() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.start(false);
        let submit = wire.read("session.submit");
        wire.send(
            json!({"jsonrpc":"2.0","id":submit["id"],"error":{"code":-32000,
            "message":"Hook user: no prompts today","data":{"appError":"input_blocked"}}}),
        );
        wire.detach();
    });
    let output = home
        .command()
        .args(["exec", "-p", "once"])
        .output()
        .unwrap();
    daemon.join().unwrap();
    assert_eq!(output.status.code(), Some(1));
    let diagnostic = String::from_utf8_lossy(&output.stderr);
    assert!(
        diagnostic.contains("Hook user: no prompts today"),
        "{diagnostic}"
    );
    assert!(
        !diagnostic.contains("input may already be queued"),
        "{diagnostic}"
    );
    assert!(
        !diagnostic.contains("instead of sending it again"),
        "{diagnostic}"
    );
}

#[test]
fn noninteractive_approval_denies_and_drains_outstanding_responses_before_detach() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.start(false);
        let submit = wire.read("session.submit"); wire.reply(&submit,json!({"event":{}}));
        let idle = wire.read("session.idle");
        wire.notice("approval.request",json!({"sessionId":"session","branchId":"main","requestId":"approval","source":{"kind":"model"},"call":{"name":"exec","input":{"command":"do something","token":"never print me"}},"reason":"shell command"}));
        let approval = wire.read("approval.respond"); assert_eq!(approval["params"]["decision"],"deny");
        let stop = wire.read("session.stop");
        wire.end(json!({"turn_id":"turn","status":"cancelled","reason":"denied"}));
        wire.reply(&stop,json!({"sequence":30}));
        wire.reply(&idle,json!({"sequence":30}));
        wire.reply(&approval,json!({}));
        wire.detach();
    });
    let output = wait(
        home.command()
            .args(["exec", "--json", "--prompt", "exec"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    daemon.join().unwrap();
    assert_eq!(
        output.status.code(),
        Some(3),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!String::from_utf8_lossy(&output.stderr).contains("never print me"));
    let notices: Vec<Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(notices.len(), 2);
    assert_eq!(notices[0]["method"], "approval.request");
    assert_eq!(notices[1]["method"], "session.event");
}

#[test]
fn noninteractive_outside_read_is_denied_by_the_ordinary_approval_answer() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.start(false);
        let submit = wire.read("session.submit"); wire.reply(&submit,json!({"event":{}}));
        let idle = wire.read("session.idle");
        wire.notice("approval.request",json!({"sessionId":"session","branchId":"main","requestId":"read-approval","source":{"kind":"model"},"call":{"name":"read","input":{"path":"/etc/hosts"}},"reason":"Reading outside the session workspace requires approval: /etc/hosts"}));
        let approval = wire.read("approval.respond"); assert_eq!(approval["params"]["decision"],"deny");
        let stop = wire.read("session.stop");
        wire.end(json!({"turn_id":"turn","status":"cancelled","reason":"denied"}));
        wire.reply(&stop,json!({"sequence":30}));
        wire.reply(&idle,json!({"sequence":30}));
        wire.reply(&approval,json!({}));
        wire.detach();
    });
    let output = wait(
        home.command()
            .args(["exec", "--prompt", "read /etc/hosts"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    daemon.join().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(3), "{stderr}");
    assert!(stderr.contains("Interactive approval required"), "{stderr}");
}

#[test]
fn resume_advances_filtered_history_positions_and_does_not_reprint_old_output() {
    let home = Home::new();
    let launch = {
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::ffi::OsStringExt;
            home.0
                .join(std::ffi::OsString::from_vec(b"launch-\xff".to_vec()))
        }
        #[cfg(not(target_os = "linux"))]
        {
            home.0.join("launch")
        }
    };
    fs::create_dir(&launch).unwrap();
    let daemon = home.daemon(|wire| {
        wire.hello(&[]);
        let attach = wire.read("session.attach"); wire.reply(&attach,attach_record(&attach));
        let first = wire.read("session.history"); assert_eq!(first["params"]["latestTurn"],true);
        wire.reply(&first,json!({"events":[],"through":10,"nextBefore":7,"nextAfter":10,"done":true}));
        let next = wire.read("session.history");
        assert_eq!(next["params"]["before"],7); assert_eq!(next["params"]["through"],10);
        assert!(next["params"]["latestTurn"].is_null());
        wire.reply(&next,json!({"events":[
            {"sequence":4,"create_at":1,"type":"step","name":"text","data":{"turn_id":"old-turn","request_id":"old-request","delta":"old output"}},
            {"sequence":5,"create_at":2,"type":"step","name":"model-output-end","data":{"turn_id":"old-turn","request_id":"old-request","status":"completed","finish_reason":"end_turn"}}
        ],"through":10,"nextBefore":1,"nextAfter":6,"done":true}));
        wire.no_pending_questions(10);
        wire.notice("session.model.progress",json!({"sessionId":"session","branchId":"main","requestId":"old-request","textOffset":0,"text":"old output","thinking":"","thinkingOffset":0}));
        let idle = wire.read("session.idle"); wire.reply(&idle,json!({"sequence":10}));
        let submit = wire.read("session.submit"); wire.reply(&submit,json!({"event":{}}));
        let idle = wire.read("session.idle");
        wire.notice("session.event",json!({"sessionId":"session","branchId":"main","event":{"id":"new-input","sequence":11,"create_at":1,"type":"input","name":"message","data":{"submission_id":submit["params"]["submissionId"],"user":{"content":[{"type":"text","text":"continue"}]},"mode":"queue"}}}));
        wire.notice("session.event",json!({"sessionId":"session","branchId":"main","event":{"id":"new-start","sequence":12,"create_at":1,"type":"turn","name":"start","data":{"turn_id":"new-turn"}}}));
        wire.notice("session.event",json!({"sessionId":"session","branchId":"main","event":{"id":"new-consumed","sequence":13,"create_at":1,"type":"input","name":"consumed","data":{"input_id":"new-input"}}}));
        wire.event(14,"text",json!({"turn_id":"new-turn","request_id":"new-request","delta":"new output"}));
        wire.end(json!({"turn_id":"new-turn","status":"completed","reason":"done"}));
        wire.reply(&idle,json!({"sequence":30})); wire.detach();
    });
    let output = wait(
        home.command()
            .current_dir(&launch)
            .args(["exec", "resume", "session", "--prompt", "continue"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    daemon.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "new output\n");
}

#[cfg(target_os = "linux")]
#[test]
fn exec_refuses_a_non_utf8_workspace_before_connecting() {
    use std::os::unix::ffi::OsStringExt;
    let home = Home::new();
    let launch = home
        .0
        .join(std::ffi::OsString::from_vec(b"launch-\xff".to_vec()));
    fs::create_dir(&launch).unwrap();
    let output = home
        .command()
        .current_dir(&launch)
        .args(["exec", "-p", "start"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("must be valid UTF-8"));
    assert!(!home.0.join("rust/daemon.sock").exists());
}

#[test]
fn exec_json_preserves_public_notifications_and_only_observes_the_selected_session() {
    let home = Home::new();
    let notices = vec![
        json!({"jsonrpc":"2.0","method":"session.mcp.state","params":{"sessionId":"session","servers":[{"id":"docs","state":"auth_required"}]}}),
        json!({"jsonrpc":"2.0","method":"session.plugin.notice","params":{"sessionId":"session","branchId":"child","source":"fixture","future":{"control":"\u{1b}[2J\n"}}}),
        json!({"jsonrpc":"2.0","method":"session.model.progress","params":{"sessionId":"session","branchId":"main","requestId":"request","textOffset":0,"text":"Hello","newField":true}}),
        json!({"jsonrpc":"2.0","method":"session.event","params":{"sessionId":"session","branchId":"main","event":{"sequence":12,"create_at":1,"type":"step","name":"text","data":{"turn_id":"turn","request_id":"request","delta":"Hello","newField":"preserved"}}}}),
        json!({"jsonrpc":"2.0","method":"session.event","params":{"sessionId":"session","branchId":"main","event":{"sequence":25,"create_at":1,"type":"turn","name":"end","data":{"turn_id":"turn","status":"failed","error":"fixture"}}}}),
    ];
    let emitted = notices.clone();
    let daemon = home.daemon(move |wire| {
        let hello = wire.read("hello");
        wire.reply(&hello,json!({"instanceId":"server","serverVersion":env!("CARGO_PKG_VERSION"),"protocolVersion":4,"daemonProfile":{"kind":"product"},"features":["session.create","session.history","session.history.latestTurn"]}));
        let attach = wire.read("session.attach"); wire.reply(&attach,json!({}));
        let history = wire.read("session.history");
        wire.reply(&history,json!({"events":[{"sequence":4,"create_at":1,"type":"step","name":"text","data":{"turn_id":"old-turn","request_id":"old","delta":"history-only","newField":"preserved"}}],"through":8,"nextBefore":1,"nextAfter":8,"done":true}));
        wire.no_pending_questions(8);
        let idle = wire.read("session.idle"); wire.reply(&idle,json!({"sequence":8}));
        let submit = wire.read("session.submit");
        assert_eq!(submit["params"]["text"], "continue"); wire.reply(&submit,json!({"event":{}}));
        let idle = wire.read("session.idle");
        wire.notice("session.closed",json!({"sessionId":"other"}));
        wire.notice("session.model.progress",json!({"sessionId":"other","text":"other-session"}));
        for notice in emitted { wire.send(notice); }
        wire.reply(&idle,json!({"sequence":30})); wire.detach();
    });
    let output = wait(
        home.command()
            .args(["exec", "resume", "session", "--json", "-p", "continue"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    daemon.join().unwrap();
    assert_eq!(
        output.status.code(),
        Some(2),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let received: Vec<Value> = stdout
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(received, notices);
    assert!(!stdout.contains("history-only"));
    assert!(!stdout.contains("workspace:"));
    assert!(String::from_utf8_lossy(&output.stderr).contains("session: session"));
}

#[test]
fn f6_argument_errors_do_not_connect_or_create_product_state() {
    let home = Home::new();
    for args in [
        vec!["exec", "--prompt", "one", "-p", "two"],
        vec!["exec", "resume", "one", "--resume", "two", "-p", "text"],
        vec!["--resume", "one", "exec", "resume", "two", "-p", "text"],
        vec!["--json", "-p", "text"],
        vec!["session", "export", "session"],
        vec!["session", "export", "session", "--format", "xml"],
        vec!["tui"],
        vec!["exec"],
        vec!["exec", "--permission", "admin", "-p", "text"],
        vec!["exec", "--permission", "-p", "text"],
        vec![
            "--permission",
            "read-only",
            "--permission",
            "full-access",
            "-p",
            "text",
        ],
        vec!["mcp", "list", "--permission", "read-only"],
        vec!["export"],
        vec!["export", "session", "--format", "json"],
        vec!["--database", "store.sqlite", "export", "session"],
        vec!["export", "session", "--prompt", "text"],
        vec![
            "session",
            "export",
            "session",
            "--format",
            "json",
            "--permission",
            "full-access",
        ],
    ] {
        let output = home
            .command()
            .args(&args)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(!output.status.success(), "{args:?}");
        assert!(!home.0.join("rust/daemon.sock").exists());
        assert!(output.stdout.is_empty());
    }
}

#[test]
fn exec_json_interrupt_drains_the_stop_barrier_without_human_stdout() {
    let home = Home::new();
    let (entered, wait_entered) = std::sync::mpsc::channel();
    let daemon = home.daemon(move |wire| {
        wire.start(false);
        let submit = wire.read("session.submit");
        wire.reply(&submit, json!({"event":{}}));
        let idle = wire.read("session.idle");
        entered.send(()).unwrap();
        let stop = wire.read("session.stop");
        wire.end(json!({"turn_id":"turn","status":"cancelled","reason":"interrupted"}));
        wire.reply(&idle, json!({"sequence":30}));
        wire.reply(&stop, json!({"sequence":30}));
        wire.detach();
    });
    let child = home
        .command()
        .args(["exec", "-p", "wait", "--json"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    wait_entered.recv_timeout(PATIENCE).unwrap();
    assert_eq!(unsafe { libc::kill(child.id() as i32, libc::SIGINT) }, 0);
    let output = wait(child);
    daemon.join().unwrap();
    assert_eq!(output.status.code(), Some(130));
    let stdout = String::from_utf8(output.stdout).unwrap();
    let events: Vec<Value> = stdout
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["method"], "session.event");
    assert!(String::from_utf8_lossy(&output.stderr).contains("Waiting for resource settlement"));
}

#[test]
fn session_export_keeps_a_hooks_context_under_its_own_heading() {
    let home = Home::new();
    let cli_dir = home.0.join("client");
    let workspace = home.0.join("workspace");
    fs::create_dir(&cli_dir).unwrap();
    fs::create_dir(&workspace).unwrap();
    let path = cli_dir.join("hooked.md");
    let daemon = home.daemon(move |wire| {
        wire.2 = true;
        let hello = wire.read("hello");
        wire.reply(&hello,json!({"instanceId":"server","serverVersion":env!("CARGO_PKG_VERSION"),"protocolVersion":4,"daemonProfile":{"kind":"product"},"features":["session.create","session.history","session.history.latestTurn","session.methods"]}));
        let history = wire.read("session.history");
        wire.reply(
            &history,
            json!({"events":[
                {"sequence":2,"create_at":1,"type":"input","name":"message","data":{"user":{"content":[
                    {"type":"text","text":"why does the parser stall?"},
                    {"type":"text","text":"<dim-skill name=\"checkin\">\nSkill: checkin\nInstructions:\nBODY\n</dim-skill>"},
                    {"type":"text","text":"<dim-hooks kind=\"prompt-submit\">\nread src/parser.rs `first`\n</dim-hooks>"}
                ]}}}],
                "through":8,"nextAfter":8,"done":true}),
        );
    });
    let mut command = home.command();
    command.current_dir(&cli_dir).args([
        "session",
        "export",
        "session",
        "--format",
        "markdown",
        "--cwd",
        workspace.to_str().unwrap(),
        "--output",
        "hooked.md",
    ]);
    let output = command.output().unwrap();
    daemon.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let markdown = fs::read_to_string(&path).unwrap();
    // A loaded Skill's block stays under `### User` as written — adjacent text
    // blocks have always run together here — and only a hook's context leaves
    // it, under a heading of its own.
    let expected = r#"# Session session

Branch: main


### User

why does the parser stall?<dim-skill name="checkin">
Skill: checkin
Instructions:
BODY
</dim-skill>

### Hook context

```text
read src/parser.rs `first`
```

"#;
    assert_eq!(markdown, expected);
}

/// #1380, #1381: the line client's history says what the TUI says. A message
/// it replays prints the operator's words, one `Skill loaded` line per loaded
/// Skill, and one `Hook context · N lines` line per hook context — and never a
/// marker. The order is the content's: the Skill was loaded before the hook ran.
/// A `Stop` hook's follow-up, which is its own input after the turn ended,
/// prints its one label with no body.
#[test]
fn line_client_history_names_a_loaded_skill_and_a_hooks_context() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.hello(&[]);
        let attach = wire.read("session.attach");
        wire.reply(&attach, attach_record(&attach));
        let history = wire.read("session.history");
        wire.reply(
            &history,
            json!({"events":[
                {"id":"hooked","sequence":7,"create_at":1,"type":"input","name":"message",
                 "data":{"user":{"content":[
                     {"type":"text","text":"why does the parser stall?"},
                     {"type":"text","text":"<dim-skill name=\"checkin\">\nSkill: checkin\nInstructions:\nBODY\n</dim-skill>"},
                     {"type":"text","text":"<dim-hooks kind=\"prompt-submit\">\nSEARCH-MEMORY-FIRST\nis required here\n</dim-hooks>"}
                 ]},"mode":"queue"}},
                {"id":"hooked-consumed","sequence":8,"create_at":1,"type":"input","name":"consumed",
                 "data":{"input_id":"hooked"}},
                {"id":"follow-up","sequence":9,"create_at":1,"type":"input","name":"message",
                 "data":{"blocks":[{"type":"raw","text":"<dim-hooks kind=\"follow-up\">\nrun the suite\n</dim-hooks>"}],"mode":"queue"}},
                {"id":"compact-start","sequence":10,"create_at":1,"type":"context","name":"compaction-start","data":{"trigger":"manual","through":9,"strategy":"summary"}},
                {"id":"compact","sequence":11,"create_at":1,"type":"display","name":"compaction","data":{"start":10,"strategy":"summary"}}],
                "through":11,"nextAfter":11,"done":true}),
        );
        // The client settles the queue and reads the context gauge in either
        // order, so answer by method until it leaves.
        for _ in 0..8 {
            let mut line = String::new();
            match wire.0.read_line(&mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    let request: Value = serde_json::from_str(&line).unwrap();
                    match request["method"].as_str().unwrap_or_default() {
                        "session.idle" => wire.reply(&request, json!({"sequence":9})),
                        "session.question.pending" => wire.reply(
                            &request,
                            json!({"branchId":"main","through":9,"batches":[],"next":null}),
                        ),
                        "session.context.usage" => wire.send(json!({
                            "jsonrpc":"2.0","id":request["id"],
                            "error":{"code":-32601,
                                     "message":"Unknown method: session.context.usage",
                                     "data":{"appError":"unknown_method"}}})),
                        _ => {}
                    }
                }
            }
        }
    });
    let mut command = home.command();
    command.args(["--resume", "session"]);
    let mut terminal = Terminal::launch(command);
    // Wait for the prompt after the replayed history: it appears whatever the
    // history held, so a missing context line fails at its own assertion
    // rather than at a wait.
    terminal.until("> why does the parser stall?");
    terminal.until("[context] Compacted");
    terminal.until("\n> ");
    home.kill(&terminal);
    terminal.wait();
    daemon.join().unwrap();
    let plain = terminal.transcript.clone();
    assert!(
        plain.contains("> why does the parser stall?"),
        "the log prints the operator's words:\n{plain}"
    );
    assert!(
        plain.contains("Skill loaded: checkin"),
        "a loaded Skill keeps its one line, as the TUI shows it:\n{plain}"
    );
    assert!(
        plain.contains("Hook context · 2 lines"),
        "the log prints the same line the TUI row shows:\n{plain}"
    );
    assert!(
        plain.contains("Stop hook follow-up · 1 line"),
        "the follow-up's one label reaches the log:\n{plain}"
    );
    assert!(
        !plain.contains("run the suite"),
        "the log has no disclosure, so the body stays out of it:\n{plain}"
    );
    assert!(
        !plain.contains("<dim-"),
        "no marker reaches the log:\n{plain}"
    );
    let words = plain.find("> why does the parser stall?").unwrap();
    let skill = plain.find("Skill loaded: checkin").unwrap();
    let hook = plain.find("Hook context · 2 lines").unwrap();
    let follow_up = plain.find("Stop hook follow-up · 1 line").unwrap();
    assert!(
        words < skill && skill < hook && hook < follow_up,
        "the lines keep the order the content has:\n{plain}"
    );
}

#[test]
fn session_export_preserves_the_fixed_main_prefix_and_does_not_replace_files() {
    for mode in ["stdout-json", "markdown", "existing", "history-error"] {
        let home = Home::new();
        let cli_dir = home.0.join("client");
        let workspace = home.0.join("workspace");
        fs::create_dir(&cli_dir).unwrap();
        fs::create_dir(&workspace).unwrap();
        let path = cli_dir.join("Session notes.md");
        if mode == "existing" {
            fs::write(&path, b"keep original bytes").unwrap();
        }
        let events = vec![
            json!({"sequence":1,"create_at":1,"type":"extension","name":"future","data":{"unknown":[1,2]},"extra":"preserved"}),
            json!({"sequence":2,"create_at":1,"type":"input","name":"message","data":{"user":{"content":[{"type":"text","text":"Keep ```code``` intact"},{"type":"image","source":{"type":"url","mediaType":"image/png","url":"https://invalid.example/not-fetched"}}]}}}),
            json!({"sequence":3,"create_at":1,"type":"step","name":"thinking","data":{"turn_id":"t","request_id":"r","delta":"A reason"}}),
            json!({"sequence":4,"create_at":1,"type":"step","name":"text","data":{"turn_id":"t","request_id":"r","delta":"Hello "}}),
            json!({"sequence":5,"create_at":1,"type":"step","name":"text","data":{"turn_id":"t","request_id":"r","delta":"world"}}),
            json!({"sequence":6,"create_at":1,"type":"step","name":"tool-use","data":{"turn_id":"t","request_id":"r","call":{"id":"call","name":"read","input":{"path":"file"}}}}),
            json!({"sequence":7,"create_at":1,"type":"step","name":"tool-completed","data":{"turn_id":"t","request_id":"r","call_event_id":"dispatch","call_id":"call","result":{"content":"```\n# must remain inside fence\n``````"}}}),
            json!({"sequence":8,"create_at":1,"type":"step","name":"tool-result","data":{"turn_id":"t","request_id":"r","results":[{"call_id":"call","result":{"content":"```\n# must remain inside fence\n``````"}}]}}),
            json!({"sequence":9,"create_at":1,"type":"branch","name":"create","data":{"branch":"child","at":{"kind":"create"},"source":{"kind":"host","operation_id":"export-fixture"},"prompt":"child contents are elsewhere"}}),
            json!({"sequence":10,"create_at":1,"type":"branch","name":"create-failed","data":{"status":"failed","branch":"child","createEvent":9,"error":"fixture"}}),
            json!({"sequence":15,"create_at":1,"type":"branch","name":"create-failed","data":{"status":"cancelled","branch":"cancelled-child","createEvent":14,"reason":"fixture"}}),
        ];
        let returned = events.clone();
        let daemon = home.daemon(move |wire| {
        wire.2 = true;
            wire.2 = true;
        let hello = wire.read("hello");
            wire.reply(&hello,json!({"instanceId":"server","serverVersion":env!("CARGO_PKG_VERSION"),"protocolVersion":4,"daemonProfile":{"kind":"product"},"features":["session.create","session.history","session.history.latestTurn","session.methods"]}));
            if mode == "existing" {
                assert!(wire.trailing_request().is_none(),"an existing output path is refused before the history read");
                return;
            }
            let first = wire.read("session.history");
            assert_eq!(first["params"],json!({"sessionId":"session","branchId":"main","after":0,"limit":256}));
            wire.reply(&first,json!({"events":&returned[..10],"through":20,"nextAfter":12,"done":false}));
            let second = wire.read("session.history");
            assert_eq!(second["params"]["after"],12); assert_eq!(second["params"]["through"],20);
            if mode == "history-error" {
                wire.send(json!({"jsonrpc":"2.0","id":second["id"],"error":{"code":-32000,"message":"fixture history failure"}}));
            } else {
                wire.reply(&second,json!({"events":&returned[10..],"through":20,"nextAfter":20,"done":true}));
            }
        });
        let mut command = home.command();
        command.current_dir(&cli_dir).args([
            "session",
            "export",
            "session",
            "--format",
            if mode == "stdout-json" {
                "json"
            } else {
                "markdown"
            },
            "--cwd",
            workspace.to_str().unwrap(),
        ]);
        if mode != "stdout-json" {
            command.args(["--output", "Session notes.md"]);
        }
        let output = command.output().unwrap();
        daemon.join().unwrap();
        match mode {
            "stdout-json" => {
                assert!(
                    output.status.success(),
                    "{}",
                    String::from_utf8_lossy(&output.stderr)
                );
                let document = serde_json::to_string_pretty(
                    &json!({"sessionId":"session","branchId":"main","through":20,"events":events}),
                )
                .unwrap();
                assert_eq!(String::from_utf8(output.stdout).unwrap(), document + "\n");
            }
            "markdown" => {
                assert!(
                    output.status.success(),
                    "{}",
                    String::from_utf8_lossy(&output.stderr)
                );
                assert!(output.stdout.is_empty());
                let markdown = fs::read_to_string(&path).unwrap();
                let expected = r#"# Session session

Branch: main


### User

Keep ```code``` intact
[Image attachment: image/png]


### Reasoning

A reason

### Assistant

Hello world

### Tool call: read (call)

```json
{
  "path": "file"
}
```


### Tool result: call

```````json
{
  "content": "```\n# must remain inside fence\n``````"
}
```````


Child branch child (contents not exported).


Child branch child was not created.


Child branch cancelled-child was not created.

"#;
                assert_eq!(markdown, expected);
                assert_eq!(
                    fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                    0o600
                );
            }
            "existing" => {
                assert!(!output.status.success());
                assert_eq!(fs::read(&path).unwrap(), b"keep original bytes");
                assert!(!String::from_utf8_lossy(&output.stderr).contains("Exported"));
            }
            _ => {
                assert!(!output.status.success());
                assert!(!path.exists());
                assert!(
                    String::from_utf8_lossy(&output.stderr).contains("fixture history failure")
                );
            }
        }
        assert!(!workspace.join("Session notes.md").exists());
    }
}

#[test]
fn interactive_exports_keep_literal_paths_and_errors_out_of_model_input() {
    let home = Home::new();
    let cli_dir = home.0.join("client");
    let workspace = home.0.join("workspace");
    fs::create_dir_all(cli_dir.join("~")).unwrap();
    fs::create_dir(&workspace).unwrap();
    let (entered, wait_entered) = std::sync::mpsc::channel();
    let (release, wait_release) = std::sync::mpsc::channel();
    let daemon = home.daemon(move |wire| {
        wire.start(false);
        let history = wire.read("session.history");
        entered.send(()).unwrap();
        wait_release.recv_timeout(PATIENCE).unwrap();
        wire.reply(&history,json!({"events":[],"through":8,"nextAfter":8,"done":true}));
        let history = wire.read("session.history");
        wire.send(json!({"jsonrpc":"2.0","id":history["id"],"error":{"code":-32000,"message":"fixture history failure"}}));
        wire.detach();
    });
    let mut command = home.command();
    command
        .current_dir(&cli_dir)
        .args(["tui", "--cwd", workspace.to_str().unwrap()]);
    let mut terminal = Terminal::launch(command);
    terminal.until("\n> ");
    terminal.write(b"/export markdown\n");
    terminal.until("~ is literal.");
    terminal.until("\n> ");
    terminal.write(b"/export yaml bad.md\n");
    terminal.until("Export format must be markdown or json");
    terminal.until("\n> ");
    terminal.write(b"/export json ~/Session notes.json\n");
    wait_entered.recv_timeout(PATIENCE).unwrap();
    for _ in 0..2 {
        terminal.signal(libc::SIGINT);
        terminal.until("Waiting for export result");
    }
    release.send(()).unwrap();
    terminal.until("Exported JSON: ~/Session notes.json");
    terminal.until("\n> ");
    terminal.write(b"/export json ~/Session notes.json\n");
    terminal.until("Error:");
    terminal.until("\n> ");
    terminal.write(b"/export markdown failure.md\n");
    terminal.until("fixture history failure");
    terminal.until("\n> ");
    terminal.write(b"/exit extra\n");
    terminal.until("Usage: /exit");
    terminal.until("\n> ");
    terminal.write(b"/exit\n");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
    let path = cli_dir.join("~/Session notes.json");
    let text = fs::read_to_string(&path).unwrap();
    let data: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(data["events"], json!([]));
    assert_eq!(
        text,
        serde_json::to_string_pretty(&json!({
            "sessionId": data["sessionId"],
            "branchId": "main",
            "through": data["through"],
            "events": [],
        }))
        .unwrap()
            + "\n"
    );
    assert_eq!(
        fs::metadata(path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert!(!cli_dir.join("failure.md").exists());
    assert!(!workspace.join("~").exists());
}

#[test]
fn incompatible_daemon_is_rejected_without_removing_its_endpoint() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        let hello = wire.read("hello");
        wire.reply(
            &hello,
            json!({"instanceId":"other","serverVersion":"1","protocolVersion":2,"features":[]}),
        );
    });
    let output = home.command().args(["--prompt", "hello"]).output().unwrap();
    daemon.join().unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(home.0.join("rust/daemon.sock").exists());
    assert!(!home.0.join("rust/daemon.log").exists());
}

#[test]
fn explicit_model_config_rejects_mock_and_mismatched_configured_profiles() {
    for profile in [
        json!({"kind":"mock"}),
        json!({"kind":"configured","modelConfigSha256":"0".repeat(64)}),
    ] {
        let home = Home::new();
        let config = home.0.join("model.json");
        fs::write(&config, "{}").unwrap();
        let daemon = home.daemon(move |wire| {
        wire.2 = true;
            wire.2 = true;
        let hello = wire.read("hello");
            wire.reply(&hello,json!({"instanceId":"server","serverVersion":env!("CARGO_PKG_VERSION"),"protocolVersion":4,"daemonProfile":profile,"features":["session.create","session.history","session.history.latestTurn","session.methods"]}));
        });
        let output = home
            .command()
            .args([
                "--model-config",
                config.to_str().unwrap(),
                "--prompt",
                "hello",
            ])
            .output()
            .unwrap();
        daemon.join().unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&output.stderr).contains("different model profile"));
        assert!(output.stdout.is_empty());
        assert!(home.0.join("rust/daemon.sock").exists());
        assert!(!home.0.join("rust/daemon.log").exists());
    }
}

#[test]
fn piped_prompt_runs_once_and_model_failure_has_a_distinct_exit_code() {
    for (explicit_exec, input) in [
        (false, "from stdin\n"),
        (true, "from stdin\n"),
        (false, "/mcp login docs\n"),
        (true, "/mcp login docs\n"),
        (false, "!!literal prompt\n"),
        (true, "!!literal prompt\n"),
    ] {
        let home = Home::new();
        let daemon = home.daemon(move |wire| {
            wire.start(false);
            let submit = wire.read("session.submit");
            assert_eq!(submit["params"]["text"], input);
            wire.reply(&submit, json!({"event":{}}));
            let idle = wire.read("session.idle");
            wire.end(
                json!({"turn_id":"turn","status":"failed","error":"credential should not print"}),
            );
            wire.reply(&idle, json!({"sequence":30}));
            wire.detach();
        });
        let mut command = home.command();
        if explicit_exec {
            command.arg("exec");
        }
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
        let output = wait(child);
        daemon.join().unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(!String::from_utf8_lossy(&output.stderr).contains("credential should not print"));
    }
    for explicit_exec in [false, true] {
        let home = Home::new();
        let daemon = home.daemon(|wire| {
            wire.start(false);
            wire.detach();
        });
        let mut command = home.command();
        if explicit_exec {
            command.arg("exec");
        }
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(&vec![b'"'; 8 * 1024 * 1024])
            .unwrap();
        let output = wait(child);
        daemon.join().unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains("App Server request exceeds 16 MiB after JSON encoding")
        );
    }
}

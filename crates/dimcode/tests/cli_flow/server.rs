//! `--server` names a standalone App Server binary, not this executable.
use super::*;

/// The help says what `--server` takes and how the default differs.
#[test]
fn help_describes_a_standalone_server_binary() {
    let home = Home::new();
    let output = home.command().arg("--help").output().unwrap();
    assert!(output.status.success());
    let help = String::from_utf8_lossy(&output.stdout);
    assert!(
        help.contains("Run a standalone dim-app-server binary"),
        "{help}"
    );
    assert!(help.contains("app-server subcommand"), "{help}");
}

/// The product executable is not that binary, under its own path or a link
/// to it: the CLI names the mistake instead of reporting the child's exit
/// status.
#[test]
fn the_product_executable_is_not_a_server_binary() {
    let home = Home::new();
    let executable = env!("CARGO_BIN_EXE_dimcode");
    let directory = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("server-link-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&directory).unwrap();
    let link = directory.join("dimcode-server");
    fs::hard_link(fs::canonicalize(executable).unwrap(), &link).unwrap();
    for server in [executable, link.to_str().unwrap()] {
        let output = home
            .command()
            .args(["--server", server, "exec", "-p", "hi"])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(
            error.contains("--server must name a standalone dim-app-server binary"),
            "{error}"
        );
        assert!(error.contains("not this executable"), "{error}");
        assert!(!error.contains("App Server exited"), "{error}");
    }
    fs::remove_dir_all(&directory).unwrap();
}

/// A program that exits before serving is named as the wrong binary, and
/// the log path is still given.
#[cfg(unix)]
#[test]
fn a_server_that_exits_before_serving_names_the_flag() {
    let home = Home::new();
    let script = home.0.join("not-an-app-server");
    fs::write(&script, "#!/bin/sh\nexit 1\n").unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    let output = home
        .command()
        .args(["--server", script.to_str().unwrap(), "exec", "-p", "hi"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(
        error.contains("--server must name a standalone dim-app-server binary"),
        "{error}"
    );
    assert!(error.contains("daemon.log"), "{error}");
}

/// A `--server` path that does not exist names the flag, not a bare OS
/// error.
#[test]
fn a_missing_server_names_the_flag() {
    let home = Home::new();
    let missing = home.0.join("missing-app-server");
    let output = home
        .command()
        .args(["--server", missing.to_str().unwrap(), "exec", "-p", "hi"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(
        error.contains("--server must name a standalone dim-app-server binary"),
        "{error}"
    );
    assert!(error.contains("does not exist"), "{error}");
}

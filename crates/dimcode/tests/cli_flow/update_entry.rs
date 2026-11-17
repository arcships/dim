//! `/update`: the terminal UI names the command that installs this copy, and
//! sends nothing to the model. It installs nothing either: the interface is a
//! client of the installation the updater would replace, so the installer
//! cannot run while it serves (cli-commands.md, Update dialog).
use super::*;

#[test]
fn update_names_the_command_and_sends_no_input() {
    for fullscreen in [false, true] {
        for package in [None, Some("dimcode"), Some("dimcode-alpha")] {
            update_entry(fullscreen, package);
        }
    }
}

fn update_entry(fullscreen: bool, package: Option<&str>) {
    let home = Home::new();
    let (asked, wait_asked) = std::sync::mpsc::channel();
    let daemon = home.daemon(move |wire| {
        wire.hello(&[]);
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
        let mut methods = Vec::new();
        while let Some(request) = wire.trailing_request() {
            let value: Value = serde_json::from_str(&request).unwrap();
            let method = value["method"].as_str().unwrap_or_default().to_owned();
            methods.push(method);
            wire.reply(&value, json!({}));
        }
        asked.send(methods).unwrap();
    });
    let attempted_install = home.0.join("attempted-install");
    let mut command = if let Some(package) = package {
        let prefix = home.0.join("node prefix");
        let binary = prefix
            .join("lib/node_modules")
            .join(package)
            .join("bin/dimcode");
        fs::create_dir_all(binary.parent().unwrap()).unwrap();
        fs::copy(env!("CARGO_BIN_EXE_dimcode"), &binary).unwrap();
        let npm = prefix.join("bin/npm");
        fs::create_dir_all(npm.parent().unwrap()).unwrap();
        fs::write(&npm, "#!/bin/sh\n: > \"$UPDATE_ATTEMPT\"\nexit 1\n").unwrap();
        fs::set_permissions(&npm, fs::Permissions::from_mode(0o700)).unwrap();
        let mut command = Command::new(binary);
        command.args(["--home", home.0.to_str().unwrap()]);
        command.env("DIMCODE_NPM_PACKAGE", package);
        command.env("DIMCODE_UPDATE_CHECK", "0");
        command.env("UPDATE_ATTEMPT", &attempted_install);
        command
    } else {
        let mut command = home.command();
        command.env("DIMCODE_NPM_PACKAGE", "dimcode-alpha");
        command
    };
    if fullscreen {
        command.arg("--fullscreen");
    }
    sized(&mut command, 120, 32);
    let mut terminal = Terminal::resume_tui(command);
    terminal.until("test / model");

    terminal.write(b"/upd");
    terminal.until_plain("/update");
    terminal.write(b"ate\r");
    if let Some(package) = package {
        let entry = if package == "dimcode-alpha" {
            "dima"
        } else {
            "dimcode"
        };
        terminal.until_plain(&format!(
            "To update, exit every client and stop the server, then run {entry} update"
        ));
    } else {
        terminal.until_plain("Cannot update this installation");
    }
    terminal.write(b"/exit\r");
    terminal.until("Bye!");
    assert!(terminal.wait().success(), "{}", terminal.transcript);
    if package.is_none() {
        assert!(strip_ansi(&terminal.transcript).contains("pull and rebuild"));
    }
    let methods = wait_asked.recv_timeout(PATIENCE).unwrap();
    daemon.join().unwrap();
    assert!(
        !attempted_install.exists(),
        "/update must not start an installer"
    );
    assert!(
        !methods.iter().any(|method| method == "session.submit"),
        "{methods:?} followed /update; the draft must not become model input"
    );
}

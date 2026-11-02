//! Ctrl+C ends the interface even when the terminal stopped consuming its
//! output: the exit never waits on a terminal that is not reading.
use super::*;

/// The reported screen: the Goodbye panel reaches the terminal, and the
/// terminal stops consuming output from then on. Two Ctrl+C presses still
/// end the process with the interrupt status.
#[test]
fn ctrl_c_ends_the_interface_when_the_terminal_stops_reading_after_the_goodbye_panel() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        // No `ui.leaf.2`: the client runs the interface itself.
        wire.hello(&[]);
        wire.created();
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
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            line.clear();
        }
    });
    let mut terminal = Terminal::launch_tui(home.command());
    terminal.until("test / model");
    assert_eq!(
        terminal.local_mode() & (libc::ICANON | libc::ECHO),
        0,
        "the interface is not in raw mode"
    );
    terminal.raw(b"\x03");
    terminal.until("Press Ctrl+C again to exit");
    terminal.raw(b"\x03");
    terminal.until("Bye!");
    let status = terminal.wait_without_reading();
    daemon.join().unwrap();
    assert_eq!(status.code(), Some(130), "{}", terminal.transcript);
    assert_eq!(
        terminal.local_mode() & (libc::ICANON | libc::ECHO | libc::ISIG),
        libc::ICANON | libc::ECHO | libc::ISIG,
        "the terminal was left in raw mode"
    );
}

/// The terminal already stopped consuming output before the presses: the
/// renderer is blocked on a full queue while the keys arrive. The exit ends
/// the process the same way.
#[test]
fn ctrl_c_ends_the_interface_when_the_output_queue_is_already_full() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.hello(&[]);
        wire.created();
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
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            line.clear();
        }
    });
    let mut terminal = Terminal::launch_tui(home.command());
    terminal.until("test / model");
    assert_eq!(
        terminal.local_mode() & (libc::ICANON | libc::ECHO),
        0,
        "the interface is not in raw mode"
    );
    // Typing repaints the frame; unread, the queue fills and the renderer
    // blocks on it before the presses. The plateau and the burst count vary
    // by platform, so fill by observation: one silent burst can be a frame
    // not yet painted, two in a row -- each allowed well past a repaint --
    // mean the renderer cannot write. The first press clears the draft, the
    // second one exits.
    let deadline = Instant::now() + PATIENCE;
    let mut bursts = 0;
    let mut waiting = terminal.unread();
    let mut silent = 0;
    while silent < 2 {
        assert!(
            Instant::now() < deadline,
            "the output queue never stopped growing: {waiting} unread bytes after {bursts} bursts"
        );
        terminal.raw(&[b'a'; 40]);
        bursts += 1;
        thread::sleep(Duration::from_millis(250));
        let unread = terminal.unread();
        if unread > waiting {
            waiting = unread;
            silent = 0;
        } else {
            silent += 1;
        }
    }
    assert!(
        terminal.child.try_wait().unwrap().is_none(),
        "the CLI left while the queue filled"
    );
    assert!(waiting > 0, "no output was waiting on the terminal");
    terminal.raw(b"\x03");
    thread::sleep(Duration::from_millis(100));
    terminal.raw(b"\x03");
    let status = terminal.wait_without_reading();
    daemon.join().unwrap();
    assert_eq!(status.code(), Some(130), "{}", terminal.transcript);
    assert_eq!(
        terminal.local_mode() & (libc::ICANON | libc::ECHO | libc::ISIG),
        libc::ICANON | libc::ECHO | libc::ISIG,
        "the terminal was left in raw mode"
    );
}

/// A store written by a later daemon: the CLI reports the storage note in the
/// terminal, keeps the store and removes the note after reading it.
#[test]
fn an_incompatible_store_reports_the_startup_note() {
    let home = Home::new();
    let database = home.0.join("rust/dimcode.sqlite");
    rusqlite::Connection::open(&database)
        .unwrap()
        .execute_batch(
            "CREATE TABLE agent_store_format (version INTEGER NOT NULL PRIMARY KEY CHECK (version = 32));
             INSERT INTO agent_store_format VALUES (32);",
        )
        .unwrap();
    let original = fs::read(&database).unwrap();
    let output = home
        .command()
        .args(["exec", "-p", "hello"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Cannot open native store"), "{stderr}");
    assert!(stderr.contains("Store format: v32"), "{stderr}");
    assert!(stderr.contains("Daemon format: v31"), "{stderr}");
    assert!(stderr.contains("Existing data was preserved"), "{stderr}");
    assert!(stderr.contains("Run a build supporting v32"), "{stderr}");
    assert!(stderr.contains("Details:"), "{stderr}");
    assert!(!stderr.contains("App Server exited"), "{stderr}");
    assert_eq!(fs::read(&database).unwrap(), original);
    assert!(!home.0.join("rust/dimcode.sqlite.startup").exists());
}

/// A `--server` program that exits without a storage note keeps the
/// requirement and the log location.
#[test]
fn a_server_that_exits_without_a_note_keeps_the_requirement() {
    let home = Home::new();
    let server = home.0.join("failing-server");
    fs::write(&server, "#!/bin/sh\nexit 1\n").unwrap();
    fs::set_permissions(&server, fs::Permissions::from_mode(0o700)).unwrap();
    let output = home
        .command()
        .args(["--server", server.to_str().unwrap(), "exec", "-p", "hello"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--server must name a standalone dim-app-server binary"),
        "{stderr}"
    );
    assert!(stderr.contains("it exited before serving"), "{stderr}");
    assert!(stderr.contains("daemon.log"), "{stderr}");
    assert!(!stderr.contains("Cannot open native store"), "{stderr}");
}

/// The startup note path the client reads for `home`.
fn startup_note(home: &Home) -> PathBuf {
    home.0.join("rust/dimcode.sqlite.startup")
}

/// A server that runs `script` and exits before the client connects.
fn script_server(home: &Home, name: &str, script: &str) -> Command {
    let server = home.0.join(name);
    fs::write(&server, format!("#!/bin/sh\n{script}\nexit 1\n")).unwrap();
    fs::set_permissions(&server, fs::Permissions::from_mode(0o700)).unwrap();
    let mut command = home.command();
    command.args(["--server", server.to_str().unwrap(), "exec", "-p", "hello"]);
    command
}

/// Writes a private startup note whose header names `pid`; `$$` is the
/// server's own pid, which is the child pid the client checks.
fn note_script(note: &std::path::Path, pid: &str, body: &str) -> String {
    format!(
        "printf '%s\\n%s\\n' \"dimcode-startup-failure v1 pid={pid}\" '{body}' > '{note}'\nchmod 0600 '{note}'",
        note = note.display()
    )
}

#[test]
fn a_startup_note_for_another_pid_is_not_printed() {
    let home = Home::new();
    let note = startup_note(&home);
    let mut server = script_server(
        &home,
        "wrong-pid-server",
        &note_script(&note, "1", "Cannot open native store /elsewhere."),
    );
    let output = server.output().unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--server must name a standalone dim-app-server binary"),
        "{stderr}"
    );
    assert!(!stderr.contains("Cannot open native store"), "{stderr}");
    assert!(!note.exists(), "the client removes the note it rejects");
}

#[test]
fn a_startup_note_other_users_can_read_is_not_printed() {
    let home = Home::new();
    let note = startup_note(&home);
    let script = format!(
        "{}\nchmod 0644 '{}'",
        note_script(&note, "$$", "Cannot open native store /elsewhere."),
        note.display()
    );
    let mut server = script_server(&home, "shared-note-server", &script);
    let output = server.output().unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--server must name a standalone dim-app-server binary"),
        "{stderr}"
    );
    assert!(!stderr.contains("Cannot open native store"), "{stderr}");
    assert!(!note.exists(), "the client removes the note it rejects");
}

#[test]
fn a_startup_note_body_loses_control_characters() {
    let home = Home::new();
    let note = startup_note(&home);
    let mut server = script_server(
        &home,
        "escaped-body-server",
        &note_script(
            &note,
            "$$",
            "Cannot open \u{1b}[31mnative store\u{1b}[0m.\n  Store format: v30",
        ),
    );
    let output = server.output().unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Cannot open"), "{stderr}");
    assert!(stderr.contains("native store"), "{stderr}");
    assert!(stderr.contains("Store format: v30"), "{stderr}");
    assert!(stderr.contains("Details:"), "{stderr}");
    assert!(!stderr.contains('\u{1b}'), "{stderr}");
}

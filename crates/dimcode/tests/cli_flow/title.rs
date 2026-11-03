//! The terminal window's title over the scripted daemon: the Session's stored
//! title when it has one, else the first message's topic (`ui::title`).

use super::*;
use std::sync::mpsc;

/// `session.attach`'s record for a Session whose stored title is `title`,
/// with the revision that names it.
fn titled_attach(request: &Value, title: Option<(&str, u64)>) -> Value {
    let mut record = attach_record(request);
    match title {
        Some((text, revision)) => {
            record["title"] = json!(text);
            record["titleSource"] = json!("manual");
            record["titleRevision"] = json!(revision);
        }
        None => {
            record["title"] = Value::Null;
            record["titleSource"] = Value::Null;
            record["titleRevision"] = json!(0);
        }
    }
    record
}

/// A user's first message and the facts that settle it into the transcript.
fn first_message(id: &str, text: &str) -> Vec<Value> {
    vec![
        json!({
            "id": format!("{id}-input"), "sequence": 1, "create_at": 1,
            "type": "input", "name": "message",
            "data": {"mode": "append", "user": {"content": [{"type": "text", "text": text}]}},
        }),
        json!({
            "id": format!("{id}-applied"), "sequence": 2, "create_at": 1,
            "type": "input", "name": "applied",
            "data": {"input_id": format!("{id}-input"), "mode": "append"},
        }),
        json!({
            "id": format!("{id}-consumed"), "sequence": 3, "create_at": 1,
            "type": "input", "name": "consumed",
            "data": {"input_id": format!("{id}-input")},
        }),
    ]
}

/// Answers the reads a resume-on-launch makes: the attach record carrying
/// `title`, the history carrying `first` when there is one, and the startup
/// reads after it.
fn open(wire: &mut Wire, title: Option<(&str, u64)>, first: Option<(&str, &str)>) {
    wire.hello(&[]);
    let attach = wire.read("session.attach");
    assert_eq!(attach["params"]["sessionId"], "session");
    wire.reply(&attach, titled_attach(&attach, title));
    let history = wire.read("session.history");
    assert_eq!(history["params"]["branchId"], "main");
    let events = first.map_or_else(Vec::new, |(id, text)| first_message(id, text));
    let through = events.len() as u64;
    wire.reply(
        &history,
        json!({"events": events, "through": through, "nextAfter": through, "done": true}),
    );
    wire.no_context();
    let providers = wire.read("account.providers");
    wire.reply(&providers, json!([]));
}

/// A SessionStore title commit, as the server forwards it to attached clients.
fn title_notice(wire: &mut Wire, title: &str, source: &str, revision: u64) {
    wire.notice(
        "session.notice",
        json!({
            "sessionId": "session", "source": "SessionStore", "kind": "title",
            "data": {"title": title, "titleSource": source, "titleRevision": revision},
        }),
    );
}

/// A SessionStore commit that cleared the title.
fn cleared_title_notice(wire: &mut Wire, revision: u64) {
    wire.notice(
        "session.notice",
        json!({
            "sessionId": "session", "source": "SessionStore", "kind": "title",
            "data": {"title": Value::Null, "titleSource": Value::Null, "titleRevision": revision},
        }),
    );
}

fn resume(home: &Home) -> Terminal {
    let mut command = home.command();
    command.args(["--resume", "session"]);
    Terminal::launch_tui(command)
}

#[test]
fn title_notice_after_the_first_message_updates_the_window_title() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        open(wire, None, Some(("first", "first message words")));
        title_notice(wire, "Generated title", "auto", 1);
        wire.hold();
    });
    let mut terminal = resume(&home);
    terminal.until("\u{1b}]0;DIM | first message words\u{7}");
    terminal.until("\u{1b}]0;DIM | Generated title\u{7}");
    terminal.signal(libc::SIGKILL);
    terminal.wait();
    daemon.join().unwrap();
}

#[test]
fn resuming_a_titled_session_shows_its_title_and_never_its_first_message() {
    let home = Home::new();
    let (ready, readiness) = mpsc::channel();
    let daemon = home.daemon(move |wire| {
        open(
            wire,
            Some(("Stored title", 3)),
            Some(("first", "first message words")),
        );
        ready.send(()).unwrap();
        wire.hold();
    });
    let mut terminal = resume(&home);
    terminal.until("\u{1b}]0;DIM | Stored title\u{7}");
    readiness.recv_timeout(PATIENCE).unwrap();
    terminal.until("first message words");
    assert!(
        !terminal
            .transcript
            .contains("\u{1b}]0;DIM | first message words\u{7}"),
        "the first message's topic must never be the window title: {}",
        terminal.transcript
    );
    terminal.signal(libc::SIGKILL);
    terminal.wait();
    daemon.join().unwrap();
}

#[test]
fn an_older_or_equal_title_revision_is_ignored() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        open(
            wire,
            Some(("Current title", 2)),
            Some(("first", "first message words")),
        );
        title_notice(wire, "Stale title", "manual", 1);
        title_notice(wire, "Equal title", "manual", 2);
        wire.hold();
    });
    let mut terminal = resume(&home);
    terminal.until("\u{1b}]0;DIM | Current title\u{7}");
    assert!(
        !terminal
            .transcript
            .contains("\u{1b}]0;DIM | Stale title\u{7}"),
        "an older revision must be ignored: {}",
        terminal.transcript
    );
    assert!(
        !terminal
            .transcript
            .contains("\u{1b}]0;DIM | Equal title\u{7}"),
        "an equal revision must be ignored: {}",
        terminal.transcript
    );
    terminal.signal(libc::SIGKILL);
    terminal.wait();
    daemon.join().unwrap();
}

#[test]
fn stored_titles_are_trimmed_and_placeholders_are_no_title() {
    let home = Home::new();
    let (ready, readiness) = mpsc::channel();
    let (proceed, control) = mpsc::channel();
    let daemon = home.daemon(move |wire| {
        open(
            wire,
            Some((" new SESSION ", 1)),
            Some(("first", "first question")),
        );
        ready.send(()).unwrap();
        for (title, revision) in [
            ("Alpha title", 2),
            ("New session", 3),
            ("Beta title", 4),
            ("   ", 5),
            ("  Real title  ", 6),
        ] {
            control.recv_timeout(Duration::from_secs(10)).unwrap();
            title_notice(wire, title, "manual", revision);
        }
        wire.hold();
    });
    let mut terminal = resume(&home);
    terminal.until("\u{1b}]0;DIM | first question\u{7}");
    readiness.recv_timeout(Duration::from_secs(10)).unwrap();
    proceed.send(()).unwrap();
    terminal.until("\u{1b}]0;DIM | Alpha title\u{7}");
    proceed.send(()).unwrap();
    terminal.until("\u{1b}]0;DIM | first question\u{7}");
    proceed.send(()).unwrap();
    terminal.until("\u{1b}]0;DIM | Beta title\u{7}");
    let before_blank = terminal.transcript.len();
    proceed.send(()).unwrap();
    terminal.until("\u{1b}]0;DIM | first question\u{7}");
    assert!(
        !terminal.transcript[before_blank..].contains("\u{1b}]0;DIM\u{7}"),
        "a blank title must fall back to the topic: {}",
        terminal.transcript
    );
    proceed.send(()).unwrap();
    terminal.until("\u{1b}]0;DIM | Real title\u{7}");
    for title in ["New session", "new SESSION"] {
        let shown = format!("\u{1b}]0;DIM | {title}\u{7}");
        assert!(
            !terminal.transcript.contains(&shown),
            "the window title must never be {shown:?}: {}",
            terminal.transcript
        );
    }
    terminal.signal(libc::SIGKILL);
    terminal.wait();
    daemon.join().unwrap();
}

#[test]
fn a_rename_from_another_client_updates_the_window_title() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        open(
            wire,
            Some(("Before rename", 1)),
            Some(("first", "first message words")),
        );
        title_notice(wire, "After rename", "manual", 2);
        wire.hold();
    });
    let mut terminal = resume(&home);
    terminal.until("\u{1b}]0;DIM | Before rename\u{7}");
    terminal.until("\u{1b}]0;DIM | After rename\u{7}");
    terminal.signal(libc::SIGKILL);
    terminal.wait();
    daemon.join().unwrap();
}

#[test]
fn a_cleared_title_falls_back_to_the_first_message() {
    let home = Home::new();
    let (proceed, control) = mpsc::channel();
    let daemon = home.daemon(move |wire| {
        open(
            wire,
            Some(("Before clear", 1)),
            Some(("first", "first message words")),
        );
        control.recv_timeout(Duration::from_secs(10)).unwrap();
        cleared_title_notice(wire, 2);
        wire.hold();
    });
    let mut terminal = resume(&home);
    terminal.until("\u{1b}]0;DIM | Before clear\u{7}");
    proceed.send(()).unwrap();
    terminal.until("\u{1b}]0;DIM | first message words\u{7}");
    terminal.signal(libc::SIGKILL);
    terminal.wait();
    daemon.join().unwrap();
}

#[test]
fn new_returns_the_window_title_to_the_product() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        open(wire, Some(("Stored title", 1)), None);
        wire.hold();
    });
    let mut terminal = resume(&home);
    terminal.until("\u{1b}]0;DIM | Stored title\u{7}");
    terminal.write(b"/new\r");
    terminal.until("\u{1b}]0;DIM\u{7}");
    terminal.signal(libc::SIGKILL);
    terminal.wait();
    daemon.join().unwrap();
}

#[test]
fn a_title_notice_for_a_session_the_client_left_changes_nothing() {
    let home = Home::new();
    let (proceed, control) = mpsc::channel();
    let daemon = home.daemon(move |wire| {
        open(wire, Some(("Stored title", 1)), None);
        control.recv_timeout(Duration::from_secs(10)).unwrap();
        title_notice(wire, "Late title", "manual", 2);
        wire.hold();
    });
    let mut terminal = resume(&home);
    terminal.until("\u{1b}]0;DIM | Stored title\u{7}");
    terminal.write(b"/new\r");
    terminal.until("\u{1b}]0;DIM\u{7}");
    proceed.send(()).unwrap();
    // Open the palette: its draw happens after the notice was handled, so a
    // title the notice had set would be in the transcript before it.
    terminal.write(b"\x0f");
    terminal.until("Commands");
    assert!(
        !terminal
            .transcript
            .contains("\u{1b}]0;DIM | Late title\u{7}"),
        "a notice for the Session left behind must not change the title: {}",
        terminal.transcript
    );
    terminal.signal(libc::SIGKILL);
    terminal.wait();
    daemon.join().unwrap();
}

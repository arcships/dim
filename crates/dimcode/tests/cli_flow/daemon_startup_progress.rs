//! Startup waits follow a daemon that reports real store-upgrade progress:
//! the spawn wait, the start-lock waiter, the one line on stderr and JSON
//! stdout staying clean (#1213).
//!
//! Every test here shortens a startup bound through `DIM_TEST_*`, which only a
//! debug build reads, so the module is a debug-build check.
#![cfg(debug_assertions)]
use super::*;

/// The store upgrade line the CLI prints once.
const UPGRADING: &str =
    "Upgrading the local store from format v33 to v34; this can take a while on a large store.";

fn note_path(home: &Home) -> PathBuf {
    home.0.join("rust/dimcode.sqlite.startup")
}

/// Writes the script as a `--server` program and returns its path.
fn script(home: &Home, name: &str, body: &str) -> PathBuf {
    let path = home.0.join(name);
    fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    path
}

/// A local OpenAI-compatible stream that answers the Session-title request and
/// then the turn with "ok", and the model config that points the daemon at it.
fn model_fixture(home: &Home) -> (PathBuf, thread::JoinHandle<()>) {
    use std::io::{BufRead, Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let model = thread::spawn(move || {
        // The title request, then the turn itself.
        for _ in 0..2 {
            let deadline = Instant::now() + Duration::from_secs(15);
            let stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < deadline, "the model was not requested");
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => panic!("{error}"),
                }
            };
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut stream = std::io::BufReader::new(stream);
            let mut length = 0;
            loop {
                let mut line = String::new();
                assert!(stream.read_line(&mut line).unwrap() > 0);
                if line == "\r\n" {
                    break;
                }
                if let Some(n) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = n.trim().parse().unwrap();
                }
            }
            let mut body = vec![0; length];
            stream.read_exact(&mut body).unwrap();
            let frame = |delta, finish| {
                format!(
                    "data: {}\n\n",
                    json!({"id":"fixture","choices":[{"index":0,"delta":delta,"finish_reason":finish}]})
                )
            };
            let data = frame(json!({"content":"ok"}), Value::Null)
                + &frame(json!({}), json!("stop"))
                + "data: [DONE]\n\n";
            write!(
                stream.get_mut(),
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{data}",
                data.len()
            )
            .unwrap();
            stream.get_mut().flush().unwrap();
        }
    });
    let config = home.0.join("model.json");
    fs::write(&config, json!({"defaultModel":{"providerId":"fixture","modelId":"fixture-model"},"providers":[{"id":"fixture","kind":"openai-compatible","baseUrl":format!("http://{address}/v1"),"models":[{"id":"fixture-model","contextWindow":128000,"maxOutputTokens":4096,"config":{"maxTokens":128}}]}]}).to_string()).unwrap();
    (config, model)
}

/// A note written as the daemon writes it: a private temp file renamed over
/// the target. `pid` is expanded by the shell when it is `$$`.
fn note_write(note: &std::path::Path, pid: &str, counter: usize) -> String {
    format!(
        "printf 'dimcode-startup-progress v1 pid=%s\\nstore-upgrade from=33 to=34 progress=%s\\n' \"{pid}\" {counter} > '{note}.tmp.$$'\nchmod 0600 '{note}.tmp.$$'\nmv '{note}.tmp.$$' '{note}'",
        note = note.display()
    )
}

/// A server that reports progress past the shortened start bound and then
/// serves is waited for: the client connects, and says so once on stderr.
#[test]
fn a_starting_daemon_is_waited_for_while_it_reports_progress() {
    let home = Home::new();
    let note = note_path(&home);
    let mut body = String::new();
    for counter in 1..=14 {
        body.push_str(&note_write(&note, "$$", counter));
        body.push_str("\nsleep 0.3\n");
    }
    body.push_str(&format!(
        "exec '{}' app-server \"$@\"",
        env!("CARGO_BIN_EXE_dimcode")
    ));
    let server = script(&home, "slow-server", &body);
    let output = home
        .command()
        .env("DIM_TEST_DAEMON_START_TIMEOUT_MS", "3000")
        .args(["--server", server.to_str().unwrap(), "models", "fixture"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert_eq!(
        stderr.matches(UPGRADING).count(),
        1,
        "the upgrade line is printed once: {stderr}"
    );
    assert!(!note.exists(), "the connected daemon removed the note");
}

/// The product settings' locale reaches the plugin path: the upgrade line is
/// printed in zh-CN, never in English.
#[test]
fn the_upgrade_line_follows_the_saved_locale() {
    let home = Home::new();
    fs::write(home.0.join("config.json"), r#"{"locale":"zh-CN"}"#).unwrap();
    let note = note_path(&home);
    let mut body = String::new();
    for counter in 1..=14 {
        body.push_str(&note_write(&note, "$$", counter));
        body.push_str("\nsleep 0.3\n");
    }
    body.push_str(&format!(
        "exec '{}' app-server \"$@\"",
        env!("CARGO_BIN_EXE_dimcode")
    ));
    let server = script(&home, "slow-server", &body);
    let output = home
        .command()
        .env("DIM_TEST_DAEMON_START_TIMEOUT_MS", "3000")
        .args(["--server", server.to_str().unwrap(), "models", "fixture"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    let zh = "正在把本地库从格式 v33 升级到 v34；大库可能需要一段时间。";
    assert_eq!(stderr.matches(zh).count(), 1, "{stderr}");
    assert!(!stderr.contains(UPGRADING), "{stderr}");
}

/// A child that reports nothing and never serves fails at the bound, with the
/// log location.
#[test]
fn a_starting_daemon_that_reports_nothing_fails_at_the_bound() {
    let home = Home::new();
    let server = script(&home, "silent-server", "exec sleep 5");
    let output = home
        .command()
        .env("DIM_TEST_DAEMON_START_TIMEOUT_MS", "1500")
        .args(["--server", server.to_str().unwrap(), "models", "fixture"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("did not start in time"), "{stderr}");
    assert!(stderr.contains("daemon.log"), "{stderr}");
    assert!(!stderr.contains(UPGRADING), "{stderr}");
}

/// A progress note for another pid does not extend the wait.
#[test]
fn progress_for_another_pid_does_not_extend_the_wait() {
    let home = Home::new();
    let note = note_path(&home);
    let mut body = String::new();
    for counter in 1..=35 {
        body.push_str(&note_write(&note, "999999", counter));
        body.push_str("\nsleep 0.1\n");
    }
    body.push_str("exec sleep 5");
    let server = script(&home, "foreign-server", &body);
    let output = home
        .command()
        .env("DIM_TEST_DAEMON_START_TIMEOUT_MS", "1500")
        .args(["--server", server.to_str().unwrap(), "models", "fixture"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("did not start in time"), "{stderr}");
    assert!(!stderr.contains(UPGRADING), "{stderr}");
}

/// A note written as the daemon does: a private temp file renamed over the
/// target.
fn write_progress_note(note: &std::path::Path, pid: u32, counter: u64) {
    let temp = note.with_file_name(format!(
        "{}.{counter}.tmp",
        note.file_name().unwrap().to_string_lossy()
    ));
    fs::write(
        &temp,
        format!(
            "dimcode-startup-progress v1 pid={pid}\nstore-upgrade from=33 to=34 progress={counter}\n"
        ),
    )
    .unwrap();
    fs::set_permissions(&temp, fs::Permissions::from_mode(0o600)).unwrap();
    fs::rename(&temp, note).unwrap();
}

/// A client waiting on the start lock follows the holder's progress: the
/// test holds the lock, writes notes past the shortened bound, and the waiter
/// is still there when it is released, starts its own daemon, and connects.
#[test]
fn a_start_lock_waiter_follows_progress_past_its_bound() {
    use std::io::BufRead;
    use std::os::unix::fs::OpenOptionsExt;

    let home = Home::new();
    let note = note_path(&home);
    let bound = Duration::from_millis(1500);
    let margin = Duration::from_millis(500);
    // The client's `open_start_lock` derives `<native home>/daemon.start.lock`
    // (crates/dim-cli/src/client.rs:189) and takes it with `try_lock`
    // (client.rs:334); the test holds the same advisory lock, so it waits.
    let mut options = fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    options.mode(0o600);
    let lock = options.open(home.0.join("rust/daemon.start.lock")).unwrap();
    lock.lock().unwrap();
    let mut client = home
        .command()
        .env("DIM_TEST_START_LOCK_TIMEOUT_MS", "1500")
        .args(["models", "fixture"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // The waiter prints the upgrade line the first time it reads a note, so
    // the line proves it is inside the wait and following the notes.
    let stderr_text = Arc::new(Mutex::new(String::new()));
    let sink = Arc::clone(&stderr_text);
    let pipe = std::io::BufReader::new(client.stderr.take().unwrap());
    let reader = thread::spawn(move || {
        let mut pipe = pipe;
        let mut line = String::new();
        while pipe.read_line(&mut line).unwrap_or(0) > 0 {
            sink.lock().unwrap().push_str(&line);
            line.clear();
        }
    });
    let started = Instant::now();
    let mut counter = 0;
    let mut reported_at: Option<Instant> = None;
    let mut stopped: Option<ExitStatus> = None;
    loop {
        counter += 1;
        write_progress_note(&note, 424242, counter);
        if let Some(status) = client.try_wait().unwrap() {
            stopped = Some(status);
            break;
        }
        if reported_at.is_none() && stderr_text.lock().unwrap().contains(UPGRADING) {
            reported_at = Some(Instant::now());
        }
        // Keep the lock held a whole bound past the note the waiter reported:
        // an inactive waiter would have given up by now, and one that follows
        // the notes is still waiting when the lock is released.
        if let Some(reported) = reported_at
            && reported.elapsed() >= bound + margin
        {
            break;
        }
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "the waiter neither reported progress nor failed"
        );
        thread::sleep(Duration::from_millis(200));
    }
    let released = Instant::now();
    lock.unlock().unwrap();
    let output = wait(client);
    reader.join().unwrap();
    let stderr = stderr_text.lock().unwrap().clone();
    assert!(
        stopped.is_none(),
        "the client stopped waiting before the lock was released ({stopped:?}): {stderr}"
    );
    assert!(output.status.success(), "{stderr}");
    assert_eq!(stderr.matches(UPGRADING).count(), 1, "{stderr}");
    let reported = reported_at.expect("the waiter reported the progress it saw");
    assert!(
        released.duration_since(reported) >= bound,
        "the lock stayed held a whole bound after the waiter's report"
    );
    assert!(!note.exists(), "the started daemon removed the note");
}

/// A stale note from a daemon killed mid-upgrade does not hide the new
/// daemon's counters: a note from another pid starts a new series.
#[test]
fn a_start_lock_waiter_counts_a_new_daemons_notes_past_a_stale_one() {
    use std::io::BufRead;
    use std::os::unix::fs::OpenOptionsExt;

    let home = Home::new();
    let note = note_path(&home);
    let bound = Duration::from_millis(1500);
    let margin = Duration::from_millis(500);
    // A daemon killed mid-upgrade left its note at counter 50.
    write_progress_note(&note, 999999, 50);
    // The client's `open_start_lock` derives `<native home>/daemon.start.lock`
    // (crates/dim-cli/src/client.rs:189) and takes it with `try_lock`
    // (client.rs:334); the test holds the same advisory lock, so it waits.
    let mut options = fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    options.mode(0o600);
    let lock = options.open(home.0.join("rust/daemon.start.lock")).unwrap();
    lock.lock().unwrap();
    let mut client = home
        .command()
        .env("DIM_TEST_START_LOCK_TIMEOUT_MS", "1500")
        .args(["models", "fixture"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stderr_text = Arc::new(Mutex::new(String::new()));
    let sink = Arc::clone(&stderr_text);
    let pipe = std::io::BufReader::new(client.stderr.take().unwrap());
    let reader = thread::spawn(move || {
        let mut pipe = pipe;
        let mut line = String::new();
        while pipe.read_line(&mut line).unwrap_or(0) > 0 {
            sink.lock().unwrap().push_str(&line);
            line.clear();
        }
    });
    // The waiter reports the stale note when it reads it; only then do the
    // successor's own notes begin at one.
    let started = Instant::now();
    while !stderr_text.lock().unwrap().contains(UPGRADING) {
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "the waiter did not report the stale note"
        );
        if let Some(status) = client.try_wait().unwrap() {
            lock.unlock().unwrap();
            let _ = wait(client);
            reader.join().unwrap();
            let stderr = stderr_text.lock().unwrap().clone();
            panic!("the client stopped waiting ({status}) before the lock was released: {stderr}");
        }
        thread::sleep(Duration::from_millis(50));
    }
    // Live notes for another pid start at one, below the stale counter.
    let reported = Instant::now();
    let mut counter = 0;
    let mut stopped: Option<ExitStatus> = None;
    while reported.elapsed() < bound + margin {
        counter += 1;
        write_progress_note(&note, 424242, counter);
        if let Some(status) = client.try_wait().unwrap() {
            stopped = Some(status);
            break;
        }
        thread::sleep(Duration::from_millis(200));
    }
    lock.unlock().unwrap();
    let output = wait(client);
    reader.join().unwrap();
    let stderr = stderr_text.lock().unwrap().clone();
    assert!(
        stopped.is_none(),
        "the client stopped waiting before the lock was released ({stopped:?}): {stderr}"
    );
    assert!(output.status.success(), "{stderr}");
    assert_eq!(stderr.matches(UPGRADING).count(), 1, "{stderr}");
    assert!(
        counter > 1,
        "the successor reported while the lock was held"
    );
    assert!(!note.exists(), "the started daemon removed the note");
}

/// JSON mode keeps the upgrade line off stdout while a real turn runs
/// through the same startup path; every stdout line parses as JSON.
#[test]
fn the_upgrade_line_stays_off_json_stdout() {
    let home = Home::new();
    let note = note_path(&home);
    let mut body = String::new();
    for counter in 1..=14 {
        body.push_str(&note_write(&note, "$$", counter));
        body.push_str("\nsleep 0.3\n");
    }
    body.push_str(&format!(
        "exec '{}' app-server \"$@\"",
        env!("CARGO_BIN_EXE_dimcode")
    ));
    let server = script(&home, "slow-server", &body);
    let (model, fixture) = model_fixture(&home);
    let output = home
        .command()
        .env("DIM_TEST_DAEMON_START_TIMEOUT_MS", "3000")
        .args([
            "--server",
            server.to_str().unwrap(),
            "--model-config",
            model.to_str().unwrap(),
            "exec",
            "--json",
            "-p",
            "hi",
        ])
        .output()
        .unwrap();
    fixture.join().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    assert!(
        !lines.is_empty(),
        "exec --json wrote a JSON stream: {stdout}"
    );
    for line in lines {
        serde_json::from_str::<Value>(line).expect("stdout is JSON lines");
    }
    assert!(!stdout.contains("Upgrading the local store"), "{stdout}");
    assert_eq!(stderr.matches(UPGRADING).count(), 1, "{stderr}");
}

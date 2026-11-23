//! Startup waits follow a daemon that reports real store-upgrade progress:
//! the start-lock waiter, the wait for a daemon another client started, the
//! spawn wait, the one line on stderr and JSON stdout staying clean (#1213,
//! #1335).
//!
//! The startup bounds are controlled through `DIM_TEST_*`, which only a debug
//! build reads, so the module is a debug-build check.
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
            let deadline = Instant::now() + HANG_GUARD;
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

/// The same writer, reporting the assembly that follows a store's opening.
fn assembly_note_write(note: &std::path::Path, pid: &str, counter: usize) -> String {
    format!(
        "printf 'dimcode-startup-progress v1 pid=%s\\nassembly progress=%s\\n' \"{pid}\" {counter} > '{note}.tmp.$$'\nchmod 0600 '{note}.tmp.$$'\nmv '{note}.tmp.$$' '{note}'",
        note = note.display()
    )
}

/// A server that reports progress past the start bound and then
/// serves is waited for: the client connects, and says so once on stderr.
#[test]
fn a_starting_daemon_is_waited_for_while_it_reports_progress() {
    let home = Home::new();
    let note = note_path(&home);
    let mut body = String::new();
    for counter in 1..=14 {
        body.push_str(&note_write(&note, "$$", counter));
        body.push_str("\nsleep 0.8\n");
    }
    body.push_str(&format!(
        "exec '{}' app-server \"$@\"",
        env!("CARGO_BIN_EXE_dimcode")
    ));
    let server = script(&home, "slow-server", &body);
    let output = home
        .command()
        .env("DIM_TEST_DAEMON_START_TIMEOUT_MS", "10000")
        .args(["--server", server.to_str().unwrap(), "models", "fixture"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "{stderr}\ndaemon log: {}",
        fs::read_to_string(home.0.join("rust/daemon.log")).unwrap_or_default()
    );
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
        body.push_str("\nsleep 0.8\n");
    }
    body.push_str(&format!(
        "exec '{}' app-server \"$@\"",
        env!("CARGO_BIN_EXE_dimcode")
    ));
    let server = script(&home, "slow-server", &body);
    let output = home
        .command()
        .env("DIM_TEST_DAEMON_START_TIMEOUT_MS", "10000")
        .args(["--server", server.to_str().unwrap(), "models", "fixture"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    let zh = "正在把本地库从格式 v33 升级到 v34；大库可能需要一段时间。";
    assert_eq!(stderr.matches(zh).count(), 1, "{stderr}");
    assert!(!stderr.contains(UPGRADING), "{stderr}");
}

/// A daemon that names the stage it is in keeps the upgrade visible after the
/// line that names it: each later report shows the elapsed time and what the
/// upgrade is doing. A test's stderr is a pipe, so each report is a line of its
/// own rather than one line rewritten in place, which is what the same reporter
/// does on a terminal.
#[test]
fn a_stage_report_shows_the_elapsed_time_and_the_stage() {
    let home = Home::new();
    let note = note_path(&home);
    let mut body = String::new();
    // The first report names the upgrade; the second is the first one that can
    // redraw it, so `images` needs two reports to show at all.
    for (counter, stage) in [(1, "images"), (2, "images"), (3, "index"), (4, "verify")] {
        body.push_str(&format!(
            "printf 'dimcode-startup-progress v1 pid=%s\\nstore-upgrade from=33 to=34 progress={counter} stage={stage}\\n' \"$$\" > '{note}.tmp.$$'\n",
            note = note.display()
        ));
        body.push_str(&format!("chmod 0600 '{}.tmp.$$'\n", note.display()));
        body.push_str(&format!(
            "mv '{}.tmp.$$' '{note}'\nsleep 0.4\n",
            note.display(),
            note = note.display()
        ));
    }
    body.push_str(&format!(
        "exec '{}' app-server \"$@\"",
        env!("CARGO_BIN_EXE_dimcode")
    ));
    let server = script(&home, "staged-server", &body);
    let output = home
        .command()
        .env("DIM_TEST_DAEMON_START_TIMEOUT_MS", "10000")
        .args(["--server", server.to_str().unwrap(), "models", "fixture"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert_eq!(
        stderr.matches(UPGRADING).count(),
        1,
        "the first report names the upgrade once: {stderr}"
    );
    for stage in [
        "moving retained images into Session files",
        "updating the text index of the rows that changed",
        "checking and committing the upgraded store",
    ] {
        assert!(
            stderr.contains(stage),
            "the client never said it was {stage}: {stderr}"
        );
    }
    assert!(
        stderr.contains("Upgrading the local store · "),
        "a later report is the redrawn progress line: {stderr}"
    );
}

/// A child that reports nothing and never serves fails at the bound, with the
/// log location.
#[test]
fn a_starting_daemon_that_reports_nothing_fails_at_the_bound() {
    let home = Home::new();
    let note = note_path(&home);
    // The child says nothing for eight seconds and then leaves a marker: one
    // that reached it proves the wait outlived its bound, whatever it printed.
    // Eight seconds is below the 10 s default, so a CLI that ignores the
    // configured bound is caught too.
    let marker = home.0.join("silent-server-reached-eight-seconds");
    let server = script(
        &home,
        "silent-server",
        &format!(
            "sleep 8\ntouch '{marker}'\nexec sleep 5",
            marker = marker.display()
        ),
    );
    let started = Instant::now();
    let output = home
        .command()
        .env("DIM_TEST_DAEMON_START_TIMEOUT_MS", "1500")
        .args(["--server", server.to_str().unwrap(), "models", "fixture"])
        .output()
        .unwrap();
    let elapsed = started.elapsed();
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("did not start in time"), "{stderr}");
    assert!(stderr.contains("daemon.log"), "{stderr}");
    assert!(!stderr.contains(UPGRADING), "{stderr}");
    assert!(
        !note.exists(),
        "a child that reports nothing left a progress note"
    );
    assert!(
        !marker.exists(),
        "the child reached eight seconds, so the wait outlived the 1500 ms bound \
         it was given: {elapsed:?}"
    );
    assert!(
        elapsed >= Duration::from_millis(1500),
        "the wait ended before the bound it was given: {elapsed:?}"
    );
}

/// A progress note for another pid does not extend the wait.
#[test]
fn progress_for_another_pid_does_not_extend_the_wait() {
    let home = Home::new();
    let note = note_path(&home);
    let server = script(&home, "foreign-server", "exec sleep 15");
    write_progress_note(&note, 999999, 0);
    let started = Instant::now();
    let mut client = home
        .command()
        .env("DIM_TEST_DAEMON_START_TIMEOUT_MS", "1500")
        .args(["--server", server.to_str().unwrap(), "models", "fixture"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut published = 0;
    for counter in 1..=100 {
        if client.try_wait().unwrap().is_some() {
            break;
        }
        write_progress_note(&note, 999999, counter);
        published = counter;
        thread::sleep(Duration::from_millis(100));
    }
    let output = wait(client);
    let elapsed = started.elapsed();
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(published > 1, "the foreign-pid publisher made no progress");
    assert!(
        published < 100,
        "the client waited through all {published} foreign-pid notes: {elapsed:?}"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("did not start in time"), "{stderr}");
    assert!(!stderr.contains(UPGRADING), "{stderr}");
    assert!(
        elapsed >= Duration::from_millis(1500),
        "the wait ended before the bound it was given: {elapsed:?}"
    );
}

/// A note written as the daemon does: a private temp file renamed over the
/// target.
fn write_progress_note(note: &std::path::Path, pid: u32, counter: u64) {
    write_note_body(
        note,
        pid,
        &format!("store-upgrade from=33 to=34 progress={counter}"),
    );
}

/// The same writer, reporting the assembly that follows a store's opening.
fn write_assembly_note(note: &std::path::Path, pid: u32, counter: u64) {
    write_note_body(note, pid, &format!("assembly progress={counter}"));
}

fn write_note_body(note: &std::path::Path, pid: u32, body: &str) {
    let temp = note.with_file_name(format!(
        "{}.{body}.tmp",
        note.file_name().unwrap().to_string_lossy()
    ));
    fs::write(
        &temp,
        format!("dimcode-startup-progress v1 pid={pid}\n{body}\n"),
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
        body.push_str("\nsleep 0.8\n");
    }
    body.push_str(&format!(
        "exec '{}' app-server \"$@\"",
        env!("CARGO_BIN_EXE_dimcode")
    ));
    let server = script(&home, "slow-server", &body);
    let (model, fixture) = model_fixture(&home);
    let output = home
        .command()
        .env("DIM_TEST_DAEMON_START_TIMEOUT_MS", "10000")
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
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "{stderr}\ndaemon log: {}",
        fs::read_to_string(home.0.join("rust/daemon.log")).unwrap_or_default()
    );
    fixture.join().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    assert!(
        !lines.is_empty(),
        "exec --json wrote a JSON stream: {stdout}"
    );
    for line in &lines {
        let parsed: Value = serde_json::from_str(line).expect("stdout is JSON lines");
        assert_eq!(parsed["jsonrpc"], json!("2.0"), "{line}");
        assert!(parsed["method"].is_string(), "{line}");
    }
    let turn_end = lines.iter().any(|line| {
        let record: Value = serde_json::from_str(line).expect("stdout is JSON lines");
        record["method"] == json!("session.event")
            && record["params"]["event"]["type"] == json!("turn")
            && record["params"]["event"]["name"] == json!("end")
    });
    assert!(turn_end, "the stream carries the turn's end: {stdout}");
    assert!(!stdout.contains("Upgrading the local store"), "{stdout}");
    assert_eq!(stderr.matches(UPGRADING).count(), 1, "{stderr}");
}

/// #1335: a spawned daemon that finds the store owned by another process
/// leaves a startup note that says so, and the client prints that sentence
/// instead of the bare exit status.
#[test]
fn a_child_finding_the_store_owned_reports_the_owner() {
    let home = Home::new();
    let lease = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(home.0.join("rust/dimcode.sqlite.lock"))
        .unwrap();
    // Another daemon owns the store and writes no progress note: a fresh
    // store, or one already at this format, upgrades nothing.
    lease.lock().unwrap();
    let owner = std::process::id();
    let child = home
        .command()
        .env("DIM_TEST_DAEMON_START_TIMEOUT_MS", "15000")
        .args(["models", "fixture"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let child_pid = child.id();
    let output = child.wait_with_output().unwrap();
    lease.unlock().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.code(),
        Some(1),
        "child pid {child_pid}: {}\nstartup note: {:?}",
        daemon_replace::describe_client(&home, &output),
        fs::read_to_string(note_path(&home)),
    );
    assert!(
        stderr.contains("Another dimcode-server is using the store"),
        "the client names the owner: {stderr}"
    );
    assert!(
        !stderr.contains("is upgrading it from format"),
        "the note claimed an upgrade this store did not run: {stderr}"
    );
    assert!(
        !stderr.contains(&format!("pid {owner}")),
        "the note described this test's lease as an upgrading daemon: {stderr}"
    );
    assert!(stderr.contains("Details:"), "{stderr}");
    assert!(
        !stderr.contains("App Server exited (exit status"),
        "{stderr}"
    );
}

/// #1335: a client that starts while another daemon is mid-upgrade — the store
/// lease held by a live process, a progress note from that process, no socket,
/// and no client holding the start lock — waits for the upgrade and connects,
/// instead of spawning a second daemon that fails admission.
#[test]
fn a_client_finding_another_daemons_upgrade_waits_for_it() {
    use std::io::BufRead;

    let home = Home::new();
    let note = note_path(&home);
    let bound = Duration::from_millis(1500);
    // The upgrading daemon owns the store lease.
    let lease = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(home.0.join("rust/dimcode.sqlite.lock"))
        .unwrap();
    lease.lock().unwrap();
    write_progress_note(&note, std::process::id(), 1);
    // It rewrites its progress note every 50 ms from a pid that is alive and
    // is not the client's own; no client holds `daemon.start.lock`.
    let stop = Arc::new(AtomicBool::new(false));
    let writer = {
        let stop = Arc::clone(&stop);
        let note = note.clone();
        thread::spawn(move || {
            let mut counter = 1;
            while !stop.load(Ordering::SeqCst) {
                counter += 1;
                // A later version may add fields to the progress line; this
                // one adds a value that is not a number, which a reader must
                // ignore as the daemon's own reader does.
                let temp = note.with_file_name(format!(
                    "{}.{counter}.tmp",
                    note.file_name().unwrap().to_string_lossy()
                ));
                fs::write(
                    &temp,
                    format!(
                        "dimcode-startup-progress v1 pid={}\nstore-upgrade from=33 to=34 progress={counter} stage=rewrite\n",
                        std::process::id()
                    ),
                )
                .unwrap();
                fs::set_permissions(&temp, fs::Permissions::from_mode(0o600)).unwrap();
                fs::rename(&temp, &note).unwrap();
                thread::sleep(Duration::from_millis(50));
            }
        })
    };
    let mut client = home
        .command()
        .env("DIM_TEST_DAEMON_START_TIMEOUT_MS", "1500")
        .args(["models", "fixture"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let client_pid = client.id();
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
    while !stderr_text.lock().unwrap().contains(UPGRADING) {
        if let Some(status) = client.try_wait().unwrap() {
            stop.store(true, Ordering::SeqCst);
            let _ = wait(client);
            reader.join().unwrap();
            let stderr = stderr_text.lock().unwrap().clone();
            panic!(
                "the client pid {client_pid} stopped ({status}) instead of waiting for the upgrade: {stderr}"
            );
        }
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "the client never reported the upgrade it found: {}",
            stderr_text.lock().unwrap()
        );
        thread::sleep(Duration::from_millis(50));
    }
    // Keep the upgrade running a whole bound past the report: a client that
    // only waits a fixed time has spawned, and failed, by now.
    let reported = Instant::now();
    while reported.elapsed() < bound + Duration::from_millis(500) {
        if let Some(status) = client.try_wait().unwrap() {
            stop.store(true, Ordering::SeqCst);
            let _ = wait(client);
            reader.join().unwrap();
            let stderr = stderr_text.lock().unwrap().clone();
            panic!(
                "the client pid {client_pid} stopped ({status}) after one bound of progress: {stderr}"
            );
        }
        thread::sleep(Duration::from_millis(50));
    }
    // The upgrade finishes: the note goes, the lease is released, and the
    // daemon begins to serve.
    stop.store(true, Ordering::SeqCst);
    writer.join().unwrap();
    let _ = fs::remove_file(&note);
    lease.unlock().unwrap();
    let daemon = home.daemon(|wire| {
        wire.2 = true;
        let hello = wire.read("hello");
        wire.reply(&hello, json!({"instanceId":"server","serverVersion":env!("CARGO_PKG_VERSION"),"protocolVersion":4,"daemonProfile":{"kind":"product"},"features":["session.create","session.history","session.history.latestTurn","session.methods"]}));
        let catalog = wire.read("provider.catalog");
        assert_eq!(catalog["params"], json!({"providerId":"fixture"}));
        wire.reply(&catalog, json!({"catalog":{"version":1,"disabledModelIds":[],"models":[]}}));
    });
    let output = wait(client);
    reader.join().unwrap();
    daemon.join().unwrap();
    let stderr = stderr_text.lock().unwrap().clone();
    assert!(output.status.success(), "{stderr}");
    assert_eq!(stderr.matches(UPGRADING).count(), 1, "{stderr}");
    assert!(!note.exists(), "the connected daemon removed the note");
}

/// #1335: a progress note whose pid is gone belongs to a daemon that died
/// mid-upgrade; the client starts its own instead of waiting.
#[test]
fn a_progress_note_from_a_dead_pid_does_not_hold_up_the_start() {
    let home = Home::new();
    let note = note_path(&home);
    let mut gone = Command::new("true").spawn().unwrap();
    let pid = gone.id();
    gone.wait().unwrap();
    write_progress_note(&note, pid, 50);
    let server = script(
        &home,
        "server",
        &format!("exec '{}' app-server \"$@\"", env!("CARGO_BIN_EXE_dimcode")),
    );
    let output = home
        .command()
        .args(["--server", server.to_str().unwrap(), "models", "fixture"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert!(!stderr.contains(UPGRADING), "{stderr}");
    assert!(!note.exists(), "the started daemon removed the note");
}

/// Runs a command that starts the daemon over a note that names no daemon to
/// wait for: the client starts its own at once, connects, and prints no
/// upgrade line.
fn a_note_naming_no_daemon_does_not_hold_up_the_start(write: impl FnOnce(&std::path::Path)) {
    let home = Home::new();
    let note = note_path(&home);
    write(&note);
    let server = script(
        &home,
        "server",
        &format!("exec '{}' app-server \"$@\"", env!("CARGO_BIN_EXE_dimcode")),
    );
    let output = home
        .command()
        .args(["--server", server.to_str().unwrap(), "models", "fixture"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert!(!stderr.contains(UPGRADING), "{stderr}");
    assert!(!note.exists(), "the started daemon removed the note");
}

/// #1335: a note whose header names the failure kind is not a progress note,
/// whatever its body: the client starts its own daemon instead of waiting on
/// it.
#[test]
fn a_failure_note_does_not_hold_up_the_start() {
    let pid = std::process::id();
    a_note_naming_no_daemon_does_not_hold_up_the_start(|note| {
        fs::write(
            note,
            format!(
                "dimcode-startup-failure v1 pid={pid}\nstore-upgrade from=33 to=34 progress=1\n"
            ),
        )
        .unwrap();
        fs::set_permissions(note, fs::Permissions::from_mode(0o600)).unwrap();
    });
}

/// #1335: a progress note another user can read is not the daemon's private
/// note: the client starts its own daemon instead of waiting on it.
#[test]
fn a_progress_note_that_is_not_private_does_not_hold_up_the_start() {
    let pid = std::process::id();
    a_note_naming_no_daemon_does_not_hold_up_the_start(|note| {
        write_progress_note(note, pid, 50);
        fs::set_permissions(note, fs::Permissions::from_mode(0o644)).unwrap();
    });
}

/// The hello of a daemon of another build that an idle replacement stops.
fn old_hello() -> Value {
    json!({"instanceId":"old","serverVersion":"0.0.1-old","protocolVersion":4,"daemonProfile":{"kind":"product"},"features":["session.create","session.history","session.summaries","daemon.shutdown.completed","daemon.shutdown.ifIdle"]})
}

/// #1335: a client that already spawned its own daemon and then finds another
/// build's daemon on the endpoint reaps that child once the daemon is
/// stopped, and starts a successor instead of reporting the child's exit.
#[test]
fn a_child_that_lost_the_store_is_reaped_and_a_successor_starts() {
    let home = Home::new();
    let note = note_path(&home);
    // The store is leased until the fixture daemon is stopped, so the client's
    // own child fails admission and exits while the client meets it: the
    // stale child whose exit the loop must not report.
    let lease = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(home.0.join("rust/dimcode.sqlite.lock"))
        .unwrap();
    lease.lock().unwrap();
    // A note from a dead pid is no daemon to wait for; the client removes it
    // when it starts its own, and a child that fails admission writes its
    // failure note there with its own pid. A note that no longer names the
    // dead pid is therefore the spawn.
    let mut gone = Command::new("true").spawn().unwrap();
    let pid = gone.id();
    gone.wait().unwrap();
    write_progress_note(&note, pid, 50);
    let watched = note.clone();
    let stale = format!("pid={pid}");
    let mut client = home
        .command()
        .env("DIM_TEST_DAEMON_START_TIMEOUT_MS", "15000")
        .args(["models", "fixture"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // `Home::daemon` binds synchronously and the client retries the endpoint
    // every 50 ms after its spawn, so this thread has that window to bind;
    // the daemon below answers that attempt, when the child is already there.
    let started = Instant::now();
    while fs::read_to_string(&note).is_ok_and(|text| text.contains(&stale)) {
        assert!(
            client.try_wait().unwrap().is_none(),
            "the client stopped before it spawned"
        );
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "the client never started its own daemon"
        );
        thread::sleep(Duration::from_millis(1));
    }
    assert!(
        client.try_wait().unwrap().is_none(),
        "the client stopped before the endpoint answered"
    );
    let daemon = home.daemon(move |wire| {
        let hello = wire.read("hello");
        wire.reply(&hello, old_hello());
        let shutdown = wire.read("daemon.shutdown");
        assert_eq!(shutdown["params"], json!({"ifIdle": true}));
        // The spawned child has failed admission once its note is beside the
        // store; the store goes free only then, for the client's successor.
        let started = Instant::now();
        while !watched.exists() {
            assert!(
                started.elapsed() < Duration::from_secs(30),
                "the spawned child did not fail admission: {:?}",
                std::fs::read_to_string(watched.parent().unwrap().join("daemon.log"))
                    .unwrap_or_default()
            );
            thread::sleep(Duration::from_millis(10));
        }
        lease.unlock().unwrap();
        wire.reply(&shutdown, json!({}));
    });
    let output = wait(client);
    daemon.join().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "the client starts its successor instead of reporting the reaped child: {stderr}"
    );
    assert!(!stderr.contains("Another dimcode-server"), "{stderr}");
    assert!(!note.exists(), "the successor removed the note");
}

/// A server that has bound its endpoint, accepted the connection, and holds
/// `hello` for longer than the transport's former fixed reply bound is still
/// waited for while it reports startup progress: the handshake is a startup
/// phase, not a request with a bound of its own (#1620). The App Server binds in
/// `TransportPlugin::apply` and accepts only once its whole assembly has
/// opened, so a slow start meets exactly this shape: the connect succeeds and
/// the request gets no answer.
#[test]
fn a_bound_server_that_reports_progress_is_waited_for_before_hello() {
    let home = Home::new();
    let note = note_path(&home);
    let hold = Duration::from_secs(6);
    let released = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&released);
    // It accepts, then answers `hello` only once the test has held it past the
    // former bound, and serves the command afterwards.
    let daemon = home.daemon(move |wire| {
        let started = Instant::now();
        while started.elapsed() < hold && !flag.load(Ordering::SeqCst) {
            thread::sleep(Duration::from_millis(20));
        }
        wire.hello(&[]);
        let catalog = wire.read("provider.catalog");
        wire.reply(&catalog, json!({"catalog":null}));
    });
    // One note per 200 ms moves the startup bound past the point where a fixed
    // reply bound would have given up.
    let writing = {
        let note = note.clone();
        let flag = Arc::clone(&released);
        thread::spawn(move || {
            let mut counter = 0;
            while !flag.load(Ordering::SeqCst) && counter < 100 {
                counter += 1;
                write_progress_note(&note, 424242, counter);
                thread::sleep(Duration::from_millis(200));
            }
        })
    };
    let started = Instant::now();
    let output = home
        .command()
        .env("DIM_TEST_START_LOCK_TIMEOUT_MS", "1500")
        .args(["models", "fixture"])
        .output()
        .unwrap();
    let elapsed = started.elapsed();
    released.store(true, Ordering::SeqCst);
    writing.join().unwrap();
    daemon.join().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "a bound server that reports progress is waited for: {stderr}"
    );
    assert!(
        !stderr.contains("deadline has elapsed"),
        "the bare tokio text reached the user: {stderr}"
    );
    assert_eq!(
        stderr.matches(UPGRADING).count(),
        1,
        "the upgrade line is printed once: {stderr}"
    );
    assert!(
        elapsed >= hold,
        "the handshake was given up before the server answered: {elapsed:?}"
    );
}

/// A server that accepts the connection and then reports nothing is reported
/// with the phase it was waiting for and where to look, never tokio's own text
/// (#1620).
#[test]
fn a_silent_server_names_what_the_hello_wait_was_for() {
    let home = Home::new();
    // It accepts the connection and answers no `hello`: no reply and no
    // progress note, so the startup bound is the only thing that can end it.
    // The connection stays open until the client has reported its failure.
    let (keep, kept) = std::sync::mpsc::channel::<()>();
    let daemon = home.daemon(move |wire| {
        let _held = wire;
        let _ = kept.recv_timeout(Duration::from_secs(30));
    });
    let output = home
        .command()
        .env("DIM_TEST_START_LOCK_TIMEOUT_MS", "1500")
        .args(["models", "fixture"])
        .output()
        .unwrap();
    let _ = keep.send(());
    daemon.join().unwrap();
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("deadline has elapsed"),
        "the bare tokio text reached the user: {stderr}"
    );
    assert!(
        stderr.contains("did not answer hello") && stderr.contains("daemon.log"),
        "the error names the wait and where to look: {stderr}"
    );
}

/// Answers `hello` and the fixture's catalog after the hold. A client that
/// already gave up closed the socket; that is the assertion's business, not a
/// daemon-thread panic.
fn answer_after_hold(wire: &mut Wire, hold: Duration, released: &AtomicBool) {
    let started = Instant::now();
    while started.elapsed() < hold && !released.load(Ordering::SeqCst) {
        thread::sleep(Duration::from_millis(20));
    }
    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| wire.hello(&[]))).is_err() {
        return;
    }
    let catalog = wire.read("provider.catalog");
    wire.reply(&catalog, json!({"catalog":null}));
}

/// A daemon that binds before it is ready reports its ordinary assembly in the
/// startup note: the client follows those counters in the `hello` wait, just as
/// it follows a store upgrade, and prints nothing for them.
#[test]
fn an_assembly_note_is_waited_for_before_hello() {
    let home = Home::new();
    let note = note_path(&home);
    let hold = Duration::from_secs(4);
    let released = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&released);
    let daemon = home.daemon(move |wire| answer_after_hold(wire, hold, &flag));
    let writing = {
        let note = note.clone();
        let flag = Arc::clone(&released);
        thread::spawn(move || {
            let mut counter = 0;
            while !flag.load(Ordering::SeqCst) && counter < 100 {
                counter += 1;
                write_assembly_note(&note, 424242, counter);
                thread::sleep(Duration::from_millis(200));
            }
        })
    };
    let started = Instant::now();
    let output = home
        .command()
        .env("DIM_TEST_DAEMON_START_TIMEOUT_MS", "1500")
        .args(["models", "fixture"])
        .output()
        .unwrap();
    let elapsed = started.elapsed();
    released.store(true, Ordering::SeqCst);
    writing.join().unwrap();
    daemon.join().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "an assembly note is progress the hello wait follows: {stderr}"
    );
    assert!(
        !stderr.contains(UPGRADING),
        "an assembly note prints no upgrade line: {stderr}"
    );
    assert!(
        !stderr.contains("Upgrading the local store · "),
        "an assembly note prints no stage display: {stderr}"
    );
    assert!(
        elapsed >= hold,
        "the handshake was given up before the server answered: {elapsed:?}"
    );
}

/// The spawn wait follows the same assembly counters: a child that binds and
/// reports while it assembles is waited for past the bound it was given.
#[test]
fn an_assembly_note_is_waited_for_before_the_spawn_answers() {
    let home = Home::new();
    let note = note_path(&home);
    let mut body = String::new();
    for counter in 1..=20 {
        body.push_str(&assembly_note_write(&note, "$$", counter));
        body.push_str("\nsleep 0.6\n");
    }
    body.push_str(&format!(
        "exec '{}' app-server \"$@\"",
        env!("CARGO_BIN_EXE_dimcode")
    ));
    let server = script(&home, "assembly-server", &body);
    let output = home
        .command()
        .env("DIM_TEST_DAEMON_START_TIMEOUT_MS", "10000")
        .args(["--server", server.to_str().unwrap(), "models", "fixture"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "the spawn wait follows assembly progress: {stderr}\ndaemon log: {}",
        fs::read_to_string(home.0.join("rust/daemon.log")).unwrap_or_default()
    );
    assert!(
        !stderr.contains(UPGRADING),
        "an assembly note prints no upgrade line: {stderr}"
    );
    assert!(
        !stderr.contains("Upgrading the local store · "),
        "an assembly note prints no stage display: {stderr}"
    );
}

/// An assembly that stops reporting fails one bound after its last counter, not
/// earlier: the note is a bound on inactivity, not a total-time allowance.
#[test]
fn an_assembly_note_that_stops_fails_one_bound_after_the_last_progress() {
    let home = Home::new();
    let note = note_path(&home);
    let (accepted, connected) = std::sync::mpsc::channel::<()>();
    let (keep, kept) = std::sync::mpsc::channel::<()>();
    let daemon = home.daemon(move |wire| {
        accepted.send(()).unwrap();
        let _held = wire;
        let _ = kept.recv_timeout(Duration::from_secs(30));
    });
    let client = home
        .command()
        .env("DIM_TEST_DAEMON_START_TIMEOUT_MS", "1500")
        .args(["models", "fixture"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    connected
        .recv_timeout(Duration::from_secs(30))
        .expect("the client connected");
    // Two notes, the second one after the client is already waiting. A fixed
    // bound from spawn ends before the second note's bound would.
    thread::sleep(Duration::from_millis(400));
    write_assembly_note(&note, 424242, 1);
    thread::sleep(Duration::from_millis(200));
    write_assembly_note(&note, 424242, 2);
    let last = Instant::now();
    let output = wait(client);
    let elapsed = last.elapsed();
    let _ = keep.send(());
    daemon.join().unwrap();
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("did not answer hello") && stderr.contains("daemon.log"),
        "the error names the wait and where to look: {stderr}"
    );
    assert!(
        elapsed >= Duration::from_millis(1400),
        "the wait ended before one bound after the last note: {elapsed:?}"
    );
    assert!(
        elapsed <= Duration::from_millis(2600),
        "the wait outlived one bound after the last note: {elapsed:?}"
    );
}

/// Only a store upgrade prints; an assembly that takes over the note neither
/// prints again nor hides the upgrade's one line. The assembly's counter also
/// starts at one after the upgrade's higher counter, which is a new series.
#[test]
fn an_assembly_note_after_a_store_upgrade_prints_no_second_line() {
    let home = Home::new();
    let note = note_path(&home);
    let hold = Duration::from_secs(4);
    let released = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&released);
    let (accepted, connected) = std::sync::mpsc::channel::<()>();
    let daemon = home.daemon(move |wire| {
        accepted.send(()).unwrap();
        answer_after_hold(wire, hold, &flag);
    });
    let writing = {
        let note = note.clone();
        let flag = Arc::clone(&released);
        thread::spawn(move || {
            // The client is in its hello wait now, so the upgrade line is
            // printed before the assembly replaces the note.
            connected.recv_timeout(Duration::from_secs(30)).unwrap();
            write_progress_note(&note, 424242, 9);
            thread::sleep(Duration::from_millis(400));
            let mut counter = 0;
            while !flag.load(Ordering::SeqCst) && counter < 100 {
                counter += 1;
                write_assembly_note(&note, 424242, counter);
                thread::sleep(Duration::from_millis(200));
            }
        })
    };
    let output = home
        .command()
        .env("DIM_TEST_DAEMON_START_TIMEOUT_MS", "1500")
        .args(["models", "fixture"])
        .output()
        .unwrap();
    released.store(true, Ordering::SeqCst);
    writing.join().unwrap();
    daemon.join().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "the assembly's counters are followed after the upgrade's: {stderr}"
    );
    assert_eq!(
        stderr.matches(UPGRADING).count(),
        1,
        "only the store upgrade prints: {stderr}"
    );
    assert!(
        !stderr.contains("Upgrading the local store · "),
        "an assembly note prints no stage display: {stderr}"
    );
}

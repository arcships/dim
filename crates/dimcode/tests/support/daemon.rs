//! A test home owns the daemon it started: [`stop_daemon`] ends that daemon
//! before [`remove_directory`] deletes the home, so a test leaves no process
//! and no state behind, including when it fails.

use std::{
    path::Path,
    process::Command,
    thread,
    time::{Duration, Instant},
};

/// How long a daemon may take to exit after SIGTERM before SIGKILL, and how
/// long the killed process gets to disappear before the directory goes.
const SHUTDOWN: Duration = Duration::from_secs(30);
const KILLED: Duration = Duration::from_secs(5);

/// The marker a test writes before it starts a server on the home itself.
pub const TEST_SERVER_MARKER: &str = "rust/daemon.test-server";

/// Marks `home` as owning a server this test starts directly, so the drop
/// stops it even when it never binds and so never writes `daemon.sock.lock`.
pub fn mark_test_server(home: &Path) {
    std::fs::write(home.join(TEST_SERVER_MARKER), "").expect("write the server marker");
}

/// Whether the home may own a running server: the CLI writes `daemon.log`
/// before every spawn, a server writes `daemon.sock.lock` when it binds, and
/// a test writes the marker before it starts a server itself. Most homes have
/// none of the three, and the check is three stats.
fn owns_a_server(home: &Path) -> bool {
    home.join("rust/daemon.log").exists()
        || home.join("rust/daemon.sock.lock").exists()
        || home.join(TEST_SERVER_MARKER).exists()
}

/// Stops every process whose command line names `home`.
pub fn stop_daemon(home: &Path) {
    if !owns_a_server(home) {
        return;
    }
    let pattern = home.to_str().expect("a test home is UTF-8");
    let mut pids = Vec::new();
    for pid in pids_naming(pattern) {
        if signal_if_naming(pid, pattern, libc::SIGTERM) {
            pids.push(pid);
        }
    }
    wait(&pids, SHUTDOWN);
    for pid in pids.iter().filter(|pid| running(**pid)) {
        signal_if_naming(*pid, pattern, libc::SIGKILL);
    }
    wait(&pids, KILLED);
}

/// The pids of the processes whose command line contains `pattern`.
///
/// The match is plain text over `ps` output, so a path with regex
/// metacharacters cannot change what matches. A `ps` that cannot run is a
/// test failure: an empty result would read as "no daemon", which the caller
/// cannot tell from a leaked process.
pub fn pids_naming(pattern: &str) -> Vec<libc::pid_t> {
    // `-ww` keeps a command line from being cut at the terminal width, which
    // a piped `ps` would otherwise default to.
    let output = Command::new("ps")
        .args(["-ww", "-axo", "pid=,command="])
        .output()
        .unwrap_or_else(|error| panic!("ps -ww -axo pid=,command= failed: {error}"));
    assert!(
        output.status.success(),
        "ps -ww -axo pid=,command= failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let (pid, command) = line.trim_start().split_once(' ')?;
            if !command.contains(pattern) {
                return None;
            }
            pid.parse().ok()
        })
        .collect()
}

/// The command line `ps` reports for `pid`, or `None` when it has exited.
pub fn command_of(pid: libc::pid_t) -> Option<String> {
    let output = Command::new("ps")
        .args(["-ww", "-p", &pid.to_string(), "-o", "command="])
        .output()
        .unwrap_or_else(|error| panic!("ps -ww -p {pid} -o command= failed: {error}"));
    if !output.status.success() {
        // `ps` reports nothing for a pid that no longer exists; text on
        // stderr is the tool failing, which a test must not read as "gone".
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.trim().is_empty(),
            "ps -ww -p {pid} -o command= failed: {stderr}"
        );
        return None;
    }
    Some(
        String::from_utf8_lossy(&output.stdout)
            .trim_end()
            .to_owned(),
    )
}

/// Sends `signal` to `pid` only while its command line still names
/// `pattern`, and reports whether it did. Between the listing and the signal
/// the pid can be reused by a process the test does not own.
pub fn signal_if_naming(pid: libc::pid_t, pattern: &str, signal: libc::c_int) -> bool {
    if !command_of(pid).is_some_and(|command| command.contains(pattern)) {
        return false;
    }
    // SAFETY: the command line just read names the test's home, so this is
    // the process the listing named and a signal the test may send.
    unsafe { libc::kill(pid, signal) };
    true
}

fn running(pid: libc::pid_t) -> bool {
    unsafe { libc::kill(pid, 0) == 0 }
}

fn wait(pids: &[libc::pid_t], limit: Duration) {
    let deadline = Instant::now() + limit;
    while pids.iter().any(|pid| running(*pid)) && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(25));
    }
}

/// Removes `directory`, retrying once after a pause for a file a stopping
/// process still creates; a failure is reported on stderr, never panicked.
pub fn remove_directory(directory: &Path) {
    for attempt in 0..2 {
        match std::fs::remove_dir_all(directory) {
            Ok(()) => return,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
            Err(error) if attempt == 1 => {
                eprintln!("could not remove {}: {error}", directory.display());
            }
            Err(_) => thread::sleep(Duration::from_millis(100)),
        }
    }
}

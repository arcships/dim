//! Runs a shell-mode command (`!` / `!!`) in the workspace, outside the
//! agent: the user's shell, no stdin, stdout and stderr interleaved by line,
//! terminal escapes removed. Only the newest output is kept; an interrupt
//! ends the whole process group.

use std::collections::VecDeque;
use std::io::Read;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::model::shell::{Outcome, ShellRun};

/// Output kept for the model: the newest lines within both bounds (the
/// `exec` tool's preview bounds).
const KEEP_LINES: usize = 2000;
const KEEP_BYTES: usize = 50 * 1024;
/// A line longer than this is split.
const MAX_LINE_CHARS: usize = 8 * 1024;
/// How long an interrupted group gets before it is killed.
const KILL_GRACE: Duration = Duration::from_secs(2);
/// How long output may still drain after the shell exits (a background
/// child can hold the pipes open).
const DRAIN_GRACE: Duration = Duration::from_millis(100);
const POLL: Duration = Duration::from_millis(20);

pub struct Running {
    pub command: String,
    pub private: bool,
    pub started: Instant,
    capture: Arc<Mutex<Capture>>,
    child: Arc<Mutex<Option<Child>>>,
    interrupted: Arc<Mutex<Option<Instant>>>,
    done: Receiver<ShellRun>,
}

impl Running {
    pub fn start(command: String, private: bool, cwd: &Path) -> Self {
        let capture = Arc::new(Mutex::new(Capture::default()));
        let child = Arc::new(Mutex::new(None));
        let interrupted = Arc::new(Mutex::new(None));
        let (tx, done) = mpsc::channel();
        let spawned = spawn(&command, cwd);
        let run = |outcome: Outcome, capture: &Mutex<Capture>| {
            let capture = capture.lock().expect("capture");
            ShellRun {
                command: command.clone(),
                outcome,
                output: capture.lines.iter().cloned().collect(),
                total_lines: capture.total,
                private,
            }
        };
        match spawned {
            Err(error) => {
                let _ = tx.send(run(Outcome::Failed(error.to_string()), &capture));
            }
            Ok(mut process) => {
                let readers: Vec<Receiver<()>> = [
                    process
                        .stdout
                        .take()
                        .map(|s| Box::new(s) as Box<dyn Read + Send>),
                    process
                        .stderr
                        .take()
                        .map(|s| Box::new(s) as Box<dyn Read + Send>),
                ]
                .into_iter()
                .flatten()
                .map(|pipe| read_lines(pipe, capture.clone()))
                .collect();
                *child.lock().expect("child") = Some(process);
                let (capture, child, interrupted) =
                    (capture.clone(), child.clone(), interrupted.clone());
                let command = command.clone();
                std::thread::spawn(move || {
                    let outcome = wait(&child, &interrupted);
                    for reader in readers {
                        let _ = reader.recv_timeout(DRAIN_GRACE);
                    }
                    capture.lock().expect("capture").finish();
                    let outcome = if interrupted.lock().expect("interrupt").is_some() {
                        Outcome::Stopped
                    } else {
                        outcome
                    };
                    let capture = capture.lock().expect("capture");
                    let _ = tx.send(ShellRun {
                        command,
                        outcome,
                        output: capture.lines.iter().cloned().collect(),
                        total_lines: capture.total,
                        private,
                    });
                });
            }
        }
        Self {
            command,
            private,
            started: Instant::now(),
            capture,
            child,
            interrupted,
            done,
        }
    }

    /// The newest `count` lines, the unfinished one included.
    pub fn tail(&self, count: usize) -> (Vec<String>, usize) {
        let capture = self.capture.lock().expect("capture");
        let mut lines: Vec<String> = capture.lines.iter().cloned().collect();
        let partial = clean(&capture.pending_text());
        if !partial.is_empty() {
            lines.push(partial);
        }
        let total = capture.total + usize::from(lines.len() > capture.lines.len());
        let skip = lines.len().saturating_sub(count);
        (lines.split_off(skip), total)
    }

    /// Asks the group to end; it is killed if it has not after a grace.
    pub fn interrupt(&self) {
        let mut interrupted = self.interrupted.lock().expect("interrupt");
        if interrupted.is_some() {
            return;
        }
        *interrupted = Some(Instant::now());
        if let Some(child) = self.child.lock().expect("child").as_mut() {
            signal(child, false);
        }
    }

    /// The finished run, once it has ended.
    pub fn finished(&self) -> Option<ShellRun> {
        self.done.try_recv().ok()
    }
}

fn spawn(command: &str, cwd: &Path) -> std::io::Result<Child> {
    #[cfg(unix)]
    let mut process = {
        use std::os::unix::process::CommandExt;
        let shell = std::env::var("SHELL")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "/bin/sh".into());
        let mut process = Command::new(shell);
        process.arg("-c").arg(command).process_group(0);
        process
    };
    #[cfg(windows)]
    let mut process = {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let mut process = Command::new("cmd");
        process
            .arg("/C")
            .raw_arg(command)
            .creation_flags(CREATE_NO_WINDOW);
        process
    };
    process
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env_remove("FORCE_COLOR")
        .env("TERM", "dumb")
        .env("NO_COLOR", "1")
        .env("PAGER", "cat")
        .env("GIT_PAGER", "cat")
        .env("GH_PAGER", "cat")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_EDITOR", "false")
        .spawn()
}

/// Waits for the shell, killing the group once an interrupt's grace is up.
fn wait(child: &Mutex<Option<Child>>, interrupted: &Mutex<Option<Instant>>) -> Outcome {
    loop {
        {
            let mut guard = child.lock().expect("child");
            let process = guard.as_mut().expect("spawned");
            match process.try_wait() {
                Ok(Some(status)) => return outcome(status),
                Ok(None) => {}
                Err(error) => return Outcome::Failed(error.to_string()),
            }
            if interrupted
                .lock()
                .expect("interrupt")
                .is_some_and(|at| at.elapsed() >= KILL_GRACE)
            {
                signal(process, true);
            }
        }
        std::thread::sleep(POLL);
    }
}

fn outcome(status: std::process::ExitStatus) -> Outcome {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return Outcome::Signaled(signal_name(signal));
        }
    }
    Outcome::Exited(status.code().unwrap_or(-1))
}

#[cfg(unix)]
fn signal_name(signal: i32) -> String {
    match signal {
        libc::SIGHUP => "SIGHUP".into(),
        libc::SIGINT => "SIGINT".into(),
        libc::SIGKILL => "SIGKILL".into(),
        libc::SIGPIPE => "SIGPIPE".into(),
        libc::SIGTERM => "SIGTERM".into(),
        libc::SIGSEGV => "SIGSEGV".into(),
        libc::SIGABRT => "SIGABRT".into(),
        other => format!("signal {other}"),
    }
}

/// Terminates (or kills) the shell's whole process group.
fn signal(child: &mut Child, kill: bool) {
    #[cfg(unix)]
    {
        let group = -(child.id() as i32);
        let signal = if kill { libc::SIGKILL } else { libc::SIGTERM };
        // SAFETY: `kill` takes plain integers and has no memory effects.
        if unsafe { libc::kill(group, signal) } == 0 {
            return;
        }
    }
    let _ = kill;
    let _ = child.kill();
}

/// Reads one pipe into the capture; the receiver hears when it is drained.
fn read_lines(mut pipe: Box<dyn Read + Send>, capture: Arc<Mutex<Capture>>) -> Receiver<()> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut pending = Vec::new();
        let mut buffer = [0u8; 8192];
        while let Ok(read) = pipe.read(&mut buffer) {
            if read == 0 {
                break;
            }
            pending.extend_from_slice(&buffer[..read]);
            let mut capture = capture.lock().expect("capture");
            while let Some(at) = pending.iter().position(|b| *b == b'\n') {
                let line: Vec<u8> = pending.drain(..=at).collect();
                capture.push(&String::from_utf8_lossy(&line[..at]));
            }
            if pending.len() > MAX_LINE_CHARS {
                let line: Vec<u8> = std::mem::take(&mut pending);
                capture.push(&String::from_utf8_lossy(&line));
            }
            capture.pending = pending.clone();
        }
        if !pending.is_empty() {
            let mut capture = capture.lock().expect("capture");
            capture.push(&String::from_utf8_lossy(&pending));
            capture.pending.clear();
        }
        let _ = tx.send(());
    });
    rx
}

#[derive(Default)]
struct Capture {
    lines: VecDeque<String>,
    bytes: usize,
    total: usize,
    /// Blank lines held back until something follows them: trailing blanks
    /// are dropped.
    blanks: usize,
    /// The unfinished line of the latest pipe, for the live view.
    pending: Vec<u8>,
}

impl Capture {
    fn push(&mut self, raw: &str) {
        let line = clean(raw);
        if line.is_empty() {
            self.blanks += 1;
            return;
        }
        for _ in 0..std::mem::take(&mut self.blanks) {
            self.keep(String::new());
        }
        self.keep(line);
    }

    fn keep(&mut self, line: String) {
        self.total += 1;
        self.bytes += line.len() + 1;
        self.lines.push_back(line);
        while self.lines.len() > KEEP_LINES || self.bytes > KEEP_BYTES {
            let Some(dropped) = self.lines.pop_front() else {
                break;
            };
            self.bytes -= dropped.len() + 1;
        }
    }

    fn pending_text(&self) -> String {
        String::from_utf8_lossy(&self.pending).into_owned()
    }

    fn finish(&mut self) {
        self.pending.clear();
    }
}

/// A line as a terminal would leave it: escapes removed, only what follows
/// the last carriage return, tabs expanded, other controls dropped, no
/// trailing blanks.
fn clean(raw: &str) -> String {
    let stripped = strip_escapes(raw);
    let visible = stripped
        .split('\r')
        .rfind(|segment| !segment.is_empty())
        .unwrap_or("");
    let mut out = String::new();
    let mut column = 0;
    for c in visible.chars() {
        if c == '\t' {
            let spaces = 8 - column % 8;
            out.push_str(&" ".repeat(spaces));
            column += spaces;
        } else if !c.is_control() {
            out.push(c);
            column += 1;
        }
    }
    out.truncate(out.trim_end().len());
    out
}

/// Removes CSI, OSC and two-byte escape sequences.
fn strip_escapes(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('[') => {
                for c in chars.by_ref() {
                    if ('\x40'..='\x7e').contains(&c) {
                        break;
                    }
                }
            }
            Some(']') => {
                while let Some(c) = chars.next() {
                    if c == '\x07' {
                        break;
                    }
                    if c == '\x1b' && chars.peek() == Some(&'\\') {
                        chars.next();
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    out
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn finish(running: &Running) -> ShellRun {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(run) = running.finished() {
                return run;
            }
            assert!(Instant::now() < deadline, "the run did not end");
            std::thread::sleep(POLL);
        }
    }

    #[test]
    fn a_run_keeps_clean_interleaved_output_and_its_exit_code() {
        let dir = tempfile::tempdir().unwrap();
        let running = Running::start(
            "printf '\\033[31mred\\033[0m\\tx\\n'; echo err >&2; printf 'a\\rb\\n\\n\\n'; pwd; exit 3"
                .into(),
            false,
            dir.path(),
        );
        let run = finish(&running);
        assert_eq!(run.outcome, Outcome::Exited(3));
        let cwd = dir.path().canonicalize().unwrap();
        // The two pipes interleave in arrival order, which is not fixed.
        let stdout: Vec<&str> = run
            .output
            .iter()
            .map(String::as_str)
            .filter(|line| *line != "err")
            .collect();
        assert_eq!(
            stdout,
            ["red     x", "b", "", "", &cwd.display().to_string()]
        );
        assert!(run.output.iter().any(|line| line == "err"));
        assert_eq!(run.total_lines, 6);
    }

    #[test]
    fn only_the_newest_output_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let run = finish(&Running::start("seq 1 5000".into(), true, dir.path()));
        assert_eq!(run.total_lines, 5000);
        assert_eq!(run.output.len(), KEEP_LINES);
        assert_eq!(run.output.last().map(String::as_str), Some("5000"));
        assert!(run.private);
    }

    #[test]
    fn an_interrupt_ends_the_whole_group() {
        let dir = tempfile::tempdir().unwrap();
        let running = Running::start("sleep 30 & sleep 30; echo late".into(), false, dir.path());
        std::thread::sleep(Duration::from_millis(200));
        running.interrupt();
        let started = Instant::now();
        let run = finish(&running);
        assert_eq!(run.outcome, Outcome::Stopped);
        assert!(run.output.is_empty());
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "SIGTERM ended it"
        );
    }
}

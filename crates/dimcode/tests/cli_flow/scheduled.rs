//! `dimcode scheduled ...` and the `/scheduled` panel: the Scheduler area
//! through its adapter, over the public wire.
use super::*;
use std::sync::mpsc;

fn hello(wire: &mut Wire) {
    wire.hello(&[
        "schedule.list",
        "schedule.save",
        "schedule.delete",
        "schedule.set_enabled",
        "schedule.run_now",
        "schedule.history",
    ]);
}

/// One run as the owner reports it.
fn run(state: &str, detail: &str, ended: &str) -> Value {
    json!({
        "run_id": format!("run-{ended}"),
        "job_id": "job-1",
        "window": "manual",
        "state": state,
        "detail": detail,
        "created_at": "2026-11-01T09:00:00.000Z",
        "ended_at": ended,
        "submission_id": "scheduler:run-1",
    })
}

/// One job as `schedule.list` reports it: the record, its next window and
/// its last run.
fn job(id: &str, name: &str, revision: i64, enabled: bool, last_run: Value) -> Value {
    json!({
        "id": id,
        "name": name,
        "schedule": {"every_minutes": 60},
        "prompt": "Summarize the day.",
        "cwd": "/workspace",
        "mode": "create",
        "max_run_minutes": 60,
        "policy_cap": {"preset": "read-only", "allowedTools": [], "disallowedTools": []},
        "created_by": "user",
        "enabled": enabled,
        "revision": revision,
        "next_window": if enabled { json!("2026-11-02T09:00:00.000Z") } else { Value::Null },
        "last_run": last_run,
    })
}

fn jobs(enabled: bool) -> Value {
    json!({"jobs": [
        job("job-1", "nightly", 3, enabled, run("completed", "", "2026-11-01T09:00:12.000Z")),
        job("job-2", "release", 7, false, Value::Null),
    ]})
}

/// The jobs after another writer saved job-1: its revision moved on.
fn newer_jobs() -> Value {
    json!({"jobs": [
        job("job-1", "nightly", 4, true, run("completed", "", "2026-11-01T09:00:12.000Z")),
        job("job-2", "release", 7, false, Value::Null),
    ]})
}

#[test]
fn scheduled_list_prints_the_jobs_and_their_runs() {
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
        hello(wire);
        let request = wire.read("schedule.list");
        assert_eq!(request["params"], json!({}));
        wire.reply(&request, jobs(true));
    });
    let output = home
        .command()
        .current_dir(&launch)
        .args(["scheduled", "list"])
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
        "job-1\tnightly\tevery 60 minutes\tenabled\t2026-11-02T09:00:00.000Z\tcompleted at 2026-11-01T09:00:12.000Z\n\
         job-2\trelease\tevery 60 minutes\tdisabled\t—\t—\n"
    );
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        "Columns: id, name, schedule, state, next run, last run.\n"
    );
}

/// A delete the owner refuses as a conflict wrote nothing: the command says
/// so and fails, as a conflicting save does.
#[test]
fn scheduled_remove_reports_a_conflict() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        hello(wire);
        let request = wire.read("schedule.delete");
        wire.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{
            "code":-32000,
            "message":"The jobs changed; read them before deleting",
            "data":{"appError":"schedule_conflict"}}}));
        // The adapter reads the jobs again after the conflict.
        let request = wire.read("schedule.list");
        wire.reply(&request, jobs(true));
    });
    let output = home
        .command()
        .args(["scheduled", "remove", "job-1"])
        .output()
        .unwrap();
    daemon.join().unwrap();
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("The job changed since it was read; nothing was removed."),
        "{stderr}"
    );
}

/// An interactive edit needs a terminal; the error names the twin, which for
/// a job takes the id and a path.
#[test]
fn scheduled_edit_refuses_stdin_that_is_not_a_terminal() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        hello(wire);
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            line.clear();
        }
    });
    let output = home
        .command()
        .args(["scheduled", "edit", "job-1"])
        .output()
        .unwrap();
    daemon.join().unwrap();
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("scheduled edit needs a terminal"),
        "{stderr}"
    );
    assert!(stderr.contains("scheduled set ID PATH"), "{stderr}");
}

/// The scheduler's answer to a failed read that follows an applied action:
/// an error the client reports as the read error.
fn read_failed(wire: &mut Wire, request: &Value) {
    wire.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{
        "code":-32000,
        "message":"the scheduler is unavailable"}}));
}

/// A run the daemon accepted is reported as done even when the read that
/// follows fails: the owner's answer is printed, the read error goes to
/// stderr, and the command exits 0.
#[test]
fn scheduled_run_keeps_the_owners_answer_when_the_read_back_fails() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        hello(wire);
        let request = wire.read("schedule.run_now");
        assert_eq!(request["params"], json!({"job_id":"job-1"}));
        wire.reply(
            &request,
            run("skipped", "overlap", "2026-11-01T09:00:00.000Z"),
        );
        let request = wire.read("schedule.list");
        read_failed(wire, &request);
    });
    let output = home
        .command()
        .args(["scheduled", "run", "job-1"])
        .output()
        .unwrap();
    daemon.join().unwrap();
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "Run job-1: skipped (overlap).\n"
    );
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        "the scheduler is unavailable (RPC -32000)\n"
    );
}

/// An add the daemon accepted reports the entry it made even when the read
/// that follows fails, with the read error on stderr and the exit 0.
#[test]
fn scheduled_add_keeps_the_owners_answer_when_the_read_back_fails() {
    let home = Home::new();
    let path = home.0.join("new.json");
    let new_job = json!({
        "name": "digest",
        "schedule": {"every_minutes": 60},
        "prompt": "Summarize the day.",
        "cwd": "/workspace",
    });
    fs::write(&path, serde_json::to_string(&new_job).unwrap()).unwrap();
    let daemon = home.daemon(|wire| {
        hello(wire);
        let request = wire.read("schedule.save");
        wire.reply(
            &request,
            json!({"job": job("job-3", "digest", 1, true, Value::Null)}),
        );
        let request = wire.read("schedule.list");
        read_failed(wire, &request);
    });
    let output = home
        .command()
        .args(["scheduled", "add"])
        .arg(&path)
        .output()
        .unwrap();
    daemon.join().unwrap();
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "Added digest.\n");
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        "the scheduler is unavailable (RPC -32000)\n"
    );
}

/// A remove the daemon accepted exits 0 when the read that follows fails,
/// with the action's line on stdout and the read error on stderr.
#[test]
fn scheduled_remove_exits_zero_when_the_read_back_fails() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        hello(wire);
        let request = wire.read("schedule.delete");
        assert_eq!(request["params"], json!({"job_id":"job-1"}));
        wire.reply(&request, json!({"job_id": "job-1"}));
        let request = wire.read("schedule.list");
        read_failed(wire, &request);
    });
    let output = home
        .command()
        .args(["scheduled", "remove", "job-1"])
        .output()
        .unwrap();
    daemon.join().unwrap();
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "Removed job-1.\n"
    );
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        "the scheduler is unavailable (RPC -32000)\n"
    );
}

/// An enable or a disable the daemon accepted reports its line even when the
/// read that follows fails: the line names the id, the read error goes to
/// stderr, and the exit stays 0.
#[test]
fn scheduled_enable_and_disable_report_the_line_when_the_read_back_fails() {
    for enabled in [true, false] {
        let home = Home::new();
        let daemon = home.daemon(move |wire| {
            hello(wire);
            let request = wire.read("schedule.set_enabled");
            assert_eq!(
                request["params"],
                json!({"job_id": "job-1", "enabled": enabled})
            );
            wire.reply(&request, json!({"job": {}}));
            let request = wire.read("schedule.list");
            read_failed(wire, &request);
        });
        let output = home
            .command()
            .args([
                "scheduled",
                if enabled { "enable" } else { "disable" },
                "job-1",
            ])
            .output()
            .unwrap();
        daemon.join().unwrap();
        assert_eq!(output.status.code(), Some(0));
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            format!("{} job-1.\n", if enabled { "Enabled" } else { "Disabled" })
        );
        assert_eq!(
            String::from_utf8(output.stderr).unwrap(),
            "the scheduler is unavailable (RPC -32000)\n"
        );
    }
}

#[test]
fn scheduled_history_prints_the_jobs_recent_runs() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        hello(wire);
        let request = wire.read("schedule.list");
        wire.reply(&request, jobs(true));
        let request = wire.read("schedule.history");
        assert_eq!(request["params"], json!({"job_id": "job-1", "limit": 20}));
        wire.reply(
            &request,
            json!({"runs": [
                run("completed", "", "2026-11-01T09:00:12.000Z"),
                run("skipped", "overlap", "2026-11-01T08:00:05.000Z"),
            ]}),
        );
    });
    let output = home
        .command()
        .args(["scheduled", "history", "job-1"])
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
        "2026-11-01T09:00:12.000Z  completed\n\
         2026-11-01T08:00:05.000Z  skipped (overlap)\n"
    );
}

#[test]
fn scheduled_history_says_when_the_job_has_not_run() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        hello(wire);
        let request = wire.read("schedule.list");
        wire.reply(&request, jobs(true));
        let request = wire.read("schedule.history");
        wire.reply(&request, json!({"runs": []}));
    });
    let output = home
        .command()
        .args(["scheduled", "history", "job-1"])
        .output()
        .unwrap();
    daemon.join().unwrap();
    assert!(output.status.success());
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "No run yet.\n");
}

/// A job's name and a run's reason are the owner's text, and the model can
/// name a job with an escape sequence: each surface prints it made safe.
#[test]
fn scheduled_prints_owner_text_without_escape_sequences() {
    // `run`: the name in the note, and the run's own state and detail.
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        hello(wire);
        let request = wire.read("schedule.run_now");
        wire.reply(
            &request,
            run("skipped", "overlap\u{1b}[2J", "2026-11-01T09:00:00.000Z"),
        );
        let request = wire.read("schedule.list");
        wire.reply(
            &request,
            json!({"jobs": [job("job-1", "nightly\u{1b}[2J", 3, true, Value::Null)]}),
        );
    });
    let output = home
        .command()
        .args(["scheduled", "run", "job-1"])
        .output()
        .unwrap();
    daemon.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(stdout, "Run nightly: skipped (overlap).\n");
    assert!(
        !stdout.contains('\u{1b}') && !stdout.contains("[2J"),
        "{stdout:?}"
    );

    // `add`: the name the owner saved.
    let home = Home::new();
    let path = home.0.join("new.json");
    let document = json!({
        "name": "victim\u{1b}[2J",
        "schedule": {"every_minutes": 60},
        "prompt": "Summarize the day.",
        "cwd": "/workspace",
    });
    fs::write(&path, serde_json::to_string(&document).unwrap()).unwrap();
    let daemon = home.daemon(|wire| {
        hello(wire);
        let request = wire.read("schedule.save");
        wire.reply(
            &request,
            json!({"job": job("job-3", "victim\u{1b}[2J", 1, true, Value::Null)}),
        );
        let request = wire.read("schedule.list");
        wire.reply(&request, jobs(true));
    });
    let output = home
        .command()
        .args(["scheduled", "add"])
        .arg(&path)
        .output()
        .unwrap();
    daemon.join().unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(stdout, "Added victim.\n");
    assert!(
        !stdout.contains('\u{1b}') && !stdout.contains("[2J"),
        "{stdout:?}"
    );

    // `history`: the run lines the owner recorded.
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        hello(wire);
        let request = wire.read("schedule.list");
        wire.reply(
            &request,
            json!({"jobs": [job("job-1", "nightly\u{1b}[2J", 3, true, Value::Null)]}),
        );
        let request = wire.read("schedule.history");
        wire.reply(
            &request,
            json!({"runs": [run("completed", "ok\u{1b}[2J", "2026-11-01T09:00:12.000Z")]}),
        );
    });
    let output = home
        .command()
        .args(["scheduled", "history", "job-1"])
        .output()
        .unwrap();
    daemon.join().unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(stdout, "2026-11-01T09:00:12.000Z  completed (ok)\n");
    assert!(
        !stdout.contains('\u{1b}') && !stdout.contains("[2J"),
        "{stdout:?}"
    );
}

/// The document `set` writes for a job: the fields `schedule.save` takes, as
/// the job reported them.
fn edited_document() -> Value {
    json!({
        "cwd": "/workspace",
        "enabled": true,
        "max_run_minutes": 60,
        "mode": "create",
        "name": "nightly",
        "prompt": "Summarize the day in one line.",
        "schedule": {"every_minutes": 60},
    })
}

#[test]
fn scheduled_set_writes_a_job_with_the_revision_read_before_it() {
    let home = Home::new();
    let path = home.0.join("job.json");
    fs::write(&path, serde_json::to_string(&edited_document()).unwrap()).unwrap();
    // Only the member the draft changed travels, with the id and the
    // revision.
    let expected = json!({
        "job": {"id": "job-1", "prompt": "Summarize the day in one line."},
        "revision": 3,
    });
    let daemon = home.daemon(move |wire| {
        hello(wire);
        let request = wire.read("schedule.list");
        wire.reply(&request, jobs(true));
        // The adapter reads the job again before it diffs the draft.
        let request = wire.read("schedule.list");
        wire.reply(&request, jobs(true));
        let request = wire.read("schedule.save");
        assert_eq!(request["params"], expected);
        wire.reply(
            &request,
            json!({"job": job("job-1", "nightly", 4, true, Value::Null)}),
        );
        let request = wire.read("schedule.list");
        wire.reply(&request, jobs(true));
    });
    let output = home
        .command()
        .args(["scheduled", "set", "job-1"])
        .arg(&path)
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
        "Saved nightly.\n"
    );
}

/// A job's editable members as `schedule.list` reports them: the base of a
/// draft that changes one of them.
fn job_document() -> Value {
    json!({
        "cwd": "/workspace",
        "enabled": true,
        "max_run_minutes": 60,
        "mode": "create",
        "name": "nightly",
        "prompt": "Summarize the day.",
        "schedule": {"every_minutes": 60},
    })
}

/// A member the draft deleted cannot be cleared through the owner's patch:
/// the write is refused before it is sent, and the refusal names the member.
#[test]
fn scheduled_set_refuses_a_deleted_member_the_owner_cannot_clear() {
    let home = Home::new();
    let path = home.0.join("job.json");
    // The stored job has a policy; the draft leaves it out.
    let listed = || {
        let mut job = job("job-1", "nightly", 3, true, Value::Null);
        job["policy"] = json!("read-only");
        json!({"jobs": [job]})
    };
    fs::write(&path, serde_json::to_string(&job_document()).unwrap()).unwrap();
    let daemon = home.daemon(move |wire| {
        hello(wire);
        let request = wire.read("schedule.list");
        wire.reply(&request, listed());
        // The adapter reads the job again and refuses: no save is sent.
        let request = wire.read("schedule.list");
        wire.reply(&request, listed());
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            line.clear();
        }
    });
    let output = home
        .command()
        .args(["scheduled", "set", "job-1"])
        .arg(&path)
        .output()
        .unwrap();
    daemon.join().unwrap();
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("The document deleted policy"), "{stderr}");
    assert!(stderr.contains("Your text is kept in"), "{stderr}");
}

/// The revision moved between the read that supplied the draft and the
/// save's re-read: the command refuses and sends no save.
#[test]
fn scheduled_set_refuses_a_revision_that_moved_before_the_write() {
    let home = Home::new();
    let path = home.0.join("job.json");
    fs::write(&path, serde_json::to_string(&edited_document()).unwrap()).unwrap();
    let daemon = home.daemon(|wire| {
        hello(wire);
        let request = wire.read("schedule.list");
        wire.reply(&request, jobs(true));
        // Another writer moved the job on before the save's re-read.
        let request = wire.read("schedule.list");
        wire.reply(&request, newer_jobs());
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            assert!(!line.contains("schedule.save"), "{line}");
            line.clear();
        }
    });
    let output = home
        .command()
        .args(["scheduled", "set", "job-1"])
        .arg(&path)
        .output()
        .unwrap();
    daemon.join().unwrap();
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("The job changed since it was read; nothing was written."),
        "{stderr}"
    );
    assert!(stderr.contains("Your text is kept in"), "{stderr}");
}

/// The job already holds what the draft says at the save's re-read: the
/// patch is empty, so the command refuses and sends no save.
#[test]
fn scheduled_set_refuses_a_patch_that_changes_nothing() {
    let home = Home::new();
    let path = home.0.join("job.json");
    fs::write(&path, serde_json::to_string(&edited_document()).unwrap()).unwrap();
    let daemon = home.daemon(|wire| {
        hello(wire);
        let request = wire.read("schedule.list");
        wire.reply(&request, jobs(true));
        // The job gained the draft's prompt since the draft was read.
        let mut job = job("job-1", "nightly", 3, true, Value::Null);
        job["prompt"] = json!("Summarize the day in one line.");
        let request = wire.read("schedule.list");
        wire.reply(&request, json!({"jobs": [job]}));
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            assert!(!line.contains("schedule.save"), "{line}");
            line.clear();
        }
    });
    let output = home
        .command()
        .args(["scheduled", "set", "job-1"])
        .arg(&path)
        .output()
        .unwrap();
    daemon.join().unwrap();
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("The document changes no field; nothing was written."),
        "{stderr}"
    );
    assert!(stderr.contains("Your text is kept in"), "{stderr}");
}

/// The job is gone at the save's re-read: the command refuses and sends no
/// save.
#[test]
fn scheduled_set_refuses_a_job_that_is_gone_at_the_re_read() {
    let home = Home::new();
    let path = home.0.join("job.json");
    fs::write(&path, serde_json::to_string(&edited_document()).unwrap()).unwrap();
    let daemon = home.daemon(|wire| {
        hello(wire);
        let request = wire.read("schedule.list");
        wire.reply(&request, jobs(true));
        let request = wire.read("schedule.list");
        wire.reply(
            &request,
            json!({"jobs": [job("job-2", "release", 7, false, Value::Null)]}),
        );
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            assert!(!line.contains("schedule.save"), "{line}");
            line.clear();
        }
    });
    let output = home
        .command()
        .args(["scheduled", "set", "job-1"])
        .arg(&path)
        .output()
        .unwrap();
    daemon.join().unwrap();
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("No scheduled job has that id."), "{stderr}");
    assert!(stderr.contains("Your text is kept in"), "{stderr}");
}

/// Setting a reuse job's mode to create clears its Session: the draft
/// deleted it, the owner clears it itself, and the patch carries no
/// session_id.
#[test]
fn scheduled_set_sends_no_session_id_when_the_mode_becomes_create() {
    let home = Home::new();
    let path = home.0.join("job.json");
    let listed = || {
        let mut job = job("job-1", "nightly", 3, true, Value::Null);
        job["mode"] = json!("reuse");
        job["session_id"] = json!("session-1");
        json!({"jobs": [job]})
    };
    fs::write(&path, serde_json::to_string(&job_document()).unwrap()).unwrap();
    let daemon = home.daemon(move |wire| {
        hello(wire);
        let request = wire.read("schedule.list");
        wire.reply(&request, listed());
        let request = wire.read("schedule.list");
        wire.reply(&request, listed());
        let request = wire.read("schedule.save");
        assert_eq!(
            request["params"],
            json!({"job": {"id": "job-1", "mode": "create"}, "revision": 3})
        );
        wire.reply(
            &request,
            json!({"job": job("job-1", "nightly", 4, true, Value::Null)}),
        );
        let request = wire.read("schedule.list");
        wire.reply(&request, jobs(true));
    });
    let output = home
        .command()
        .args(["scheduled", "set", "job-1"])
        .arg(&path)
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
        "Saved nightly.\n"
    );
}

/// A stale revision on `set` states the owner's fact and where the text is,
/// and nothing a surface adds: the panel's next-edit sentence belongs to
/// the panel, not to the command line.
#[test]
fn scheduled_set_reports_a_conflict_without_the_panels_sentences() {
    let home = Home::new();
    let path = home.0.join("job.json");
    fs::write(&path, serde_json::to_string(&edited_document()).unwrap()).unwrap();
    let daemon = home.daemon(move |wire| {
        hello(wire);
        let request = wire.read("schedule.list");
        wire.reply(&request, jobs(true));
        let request = wire.read("schedule.list");
        wire.reply(&request, jobs(true));
        let request = wire.read("schedule.save");
        wire.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{
            "code":-32000,
            "message":"The job changed; read it before updating",
            "data":{"appError":"schedule_conflict"}}}));
        // The conflict wrote nothing, so the jobs are read again.
        let request = wire.read("schedule.list");
        wire.reply(&request, newer_jobs());
    });
    let output = home
        .command()
        .args(["scheduled", "set", "job-1"])
        .arg(&path)
        .output()
        .unwrap();
    daemon.join().unwrap();
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("The job changed since it was read; nothing was written."),
        "{stderr}"
    );
    assert!(stderr.contains("Your text is kept in"), "{stderr}");
    assert!(!stderr.contains("Your next edit"), "{stderr}");
    assert!(!stderr.contains("Your next add"), "{stderr}");
}

/// A conflict whose re-read fails is still a conflict: the command says
/// what changed, why the area cannot be shown, and where the text is.
#[test]
fn scheduled_set_reports_a_conflict_when_the_re_read_fails() {
    let home = Home::new();
    let path = home.0.join("job.json");
    fs::write(&path, serde_json::to_string(&edited_document()).unwrap()).unwrap();
    let daemon = home.daemon(|wire| {
        hello(wire);
        let request = wire.read("schedule.list");
        wire.reply(&request, jobs(true));
        let request = wire.read("schedule.list");
        wire.reply(&request, jobs(true));
        let request = wire.read("schedule.save");
        wire.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{
            "code":-32000,
            "message":"The job changed; read it before updating",
            "data":{"appError":"schedule_conflict"}}}));
        // The conflict wrote nothing; the read that would show the jobs fails.
        let request = wire.read("schedule.list");
        read_failed(wire, &request);
    });
    let output = home
        .command()
        .args(["scheduled", "set", "job-1"])
        .arg(&path)
        .output()
        .unwrap();
    daemon.join().unwrap();
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("The job changed since it was read; nothing was written."),
        "{stderr}"
    );
    assert!(stderr.contains("the scheduler is unavailable"), "{stderr}");
    assert!(stderr.contains("Your text is kept in"), "{stderr}");
}

/// `scheduled edit ID` opens the job's own document in the editor and
/// writes it at the revision the listing reported.
#[test]
fn scheduled_edit_writes_what_the_editor_leaves() {
    let home = Home::new();
    let script = editor(
        &home,
        "editor.sh",
        "printf '%s' '{\"cwd\":\"/workspace\",\"enabled\":true,\"max_run_minutes\":60,\"mode\":\"create\",\"name\":\"nightly\",\"prompt\":\"Edited.\",\"schedule\":{\"every_minutes\":60}}' > \"$1\"",
    );
    let daemon = home.daemon(|wire| {
        hello(wire);
        let request = wire.read("schedule.list");
        wire.reply(&request, jobs(true));
        // The adapter reads the job again before it diffs the draft.
        let request = wire.read("schedule.list");
        wire.reply(&request, jobs(true));
        let request = wire.read("schedule.save");
        assert_eq!(request["params"]["revision"], 3);
        assert_eq!(request["params"]["job"]["id"], "job-1");
        assert_eq!(request["params"]["job"]["prompt"], "Edited.");
        wire.reply(
            &request,
            json!({"job": job("job-1", "nightly", 4, true, Value::Null)}),
        );
        let request = wire.read("schedule.list");
        wire.reply(&request, jobs(true));
    });
    let mut command = home.command();
    command
        .env_remove("VISUAL")
        .env("EDITOR", &script)
        .args(["scheduled", "edit", "job-1"]);
    let mut terminal = Terminal::launch_tui(command);
    let status = terminal.wait();
    daemon.join().unwrap();
    assert!(status.success(), "{}", terminal.transcript);
    assert!(
        terminal.transcript.contains("Saved nightly."),
        "{}",
        terminal.transcript
    );
}

#[test]
fn scheduled_add_reads_a_job_from_a_file_and_from_stdin() {
    let new_job = json!({
        "name": "example",
        "schedule": {"every_minutes": 60},
        "prompt": "Summarize the day.",
        "cwd": "/workspace",
    });
    for stdin in [false, true] {
        let home = Home::new();
        let path = home.0.join("new.json");
        fs::write(&path, serde_json::to_string(&new_job).unwrap()).unwrap();
        let expected = new_job.clone();
        let daemon = home.daemon(move |wire| {
            hello(wire);
            let request = wire.read("schedule.save");
            assert_eq!(request["params"], json!({"job": expected}));
            wire.reply(
                &request,
                json!({"job": job("job-3", "example", 1, true, Value::Null)}),
            );
            let request = wire.read("schedule.list");
            wire.reply(&request, jobs(true));
        });
        let mut command = home.command();
        command.args(["scheduled", "add"]);
        if stdin {
            command.arg("-");
        } else {
            command.arg(&path);
        }
        let mut child = command
            .stdin(if stdin { Stdio::piped() } else { Stdio::null() })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        if stdin {
            child
                .stdin
                .take()
                .unwrap()
                .write_all(serde_json::to_string(&new_job).unwrap().as_bytes())
                .unwrap();
        }
        let output = wait(child);
        daemon.join().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            "Added example.\n"
        );
        fs::remove_file(home.0.join("rust/daemon.sock")).unwrap();
    }
}

/// A daemon that does not advertise `schedule.save` refuses an add before it
/// reads the input it was given.
#[test]
fn scheduled_add_is_refused_before_stdin_is_read() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.hello(&["schedule.list"]);
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            line.clear();
        }
    });
    let mut child = home
        .command()
        .args(["scheduled", "add", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // The write end stays open and nothing is written: a read would wait.
    let _stdin = child.stdin.take();
    let output = wait(child);
    daemon.join().unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("This daemon lists scheduled jobs but cannot add them."),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// A daemon that does not advertise `schedule.save` refuses a set before it
/// reads the input it was given.
#[test]
fn scheduled_set_is_refused_before_stdin_is_read() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.hello(&["schedule.list"]);
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            line.clear();
        }
    });
    let mut child = home
        .command()
        .args(["scheduled", "set", "job-1", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // The write end stays open and nothing is written: a read would wait.
    let _stdin = child.stdin.take();
    let output = wait(child);
    daemon.join().unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("This daemon lists scheduled jobs but cannot edit them."),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn scheduled_commands_refuse_a_daemon_that_does_not_advertise_them() {
    for args in [
        vec!["scheduled", "list"],
        vec!["scheduled", "history", "job-1"],
        vec!["scheduled", "remove", "job-1"],
    ] {
        let home = Home::new();
        let daemon = home.daemon(|wire| {
            wire.hello(&[]);
            let mut line = String::new();
            while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
                line.clear();
            }
        });
        let output = home.command().args(&args).output().unwrap();
        daemon.join().unwrap();
        assert_eq!(output.status.code(), Some(1), "{args:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains("This daemon cannot manage scheduled jobs."),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

/// A daemon that serves the read but not the kind's method is refused by
/// name: the sentence says which kind is missing, not that the area is
/// unmanageable, and the refusal is made before anything is sent.
#[test]
fn scheduled_remove_names_the_kind_the_daemon_cannot_serve() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.hello(&["schedule.list"]);
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            assert!(line.trim().is_empty(), "the command sent {line}");
            line.clear();
        }
    });
    let output = home
        .command()
        .args(["scheduled", "remove", "job-1"])
        .output()
        .unwrap();
    daemon.join().unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("This daemon lists scheduled jobs but cannot remove them."),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// `/scheduled` lists the jobs with their schedule, state, next window and
/// last outcome, and every kind calls the method its design names.
#[test]
fn tui_scheduled_panel_lists_and_manages_jobs() {
    let home = Home::new();
    let (shown, wait_shown) = mpsc::channel();
    let daemon = home.daemon(move |wire| {
        hello(wire);
        wire.created();
        let attach = wire.read("session.attach");
        wire.reply(&attach, attach_record(&attach));
        let history = wire.read("session.history");
        wire.reply(
            &history,
            json!({"events":[],"through":8,"nextAfter":8,"done":true}),
        );
        for _ in 0..2 {
            let request = wire.read_any();
            match request["method"].as_str() {
                Some("account.providers") => wire.reply(&request, json!([])),
                Some("session.context.usage") => wire.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{
                    "code":-32601,"message":"Unknown method","data":{"appError":"unknown_method"}}})),
                method => panic!("unexpected request {method:?}"),
            }
        }
        wait_shown.recv_timeout(PATIENCE).unwrap();
        let request = wire.read("schedule.list");
        wire.reply(&request, jobs(true));
        // Enter pauses the selected job.
        let request = wire.read("schedule.set_enabled");
        assert_eq!(
            request["params"],
            json!({"job_id": "job-1", "enabled": false})
        );
        wire.reply(&request, json!({"job": {}}));
        let request = wire.read("schedule.list");
        wire.reply(&request, jobs(false));
        // Ctrl+T runs it now, and the message reports the owner's answer.
        let request = wire.read("schedule.run_now");
        assert_eq!(request["params"], json!({"job_id": "job-1"}));
        wire.reply(&request, run("skipped", "overlap", "2026-11-01T10:00:00.000Z"));
        let request = wire.read("schedule.list");
        wire.reply(&request, jobs(false));
        // Tab asks for the job's history: the adapter names the job from the
        // catalog it reads first.
        let request = wire.read("schedule.list");
        wire.reply(&request, jobs(false));
        let request = wire.read("schedule.history");
        assert_eq!(request["params"], json!({"job_id": "job-1", "limit": 20}));
        wire.reply(
            &request,
            json!({"runs": [run("completed", "", "2026-11-01T09:00:12.000Z")]}),
        );
        // Ctrl+X asks, Enter removes.
        let request = wire.read("schedule.delete");
        assert_eq!(request["params"], json!({"job_id": "job-1"}));
        wire.reply(&request, json!({"job_id": "job-1"}));
        let request = wire.read("schedule.list");
        wire.reply(&request, json!({"jobs": [job("job-2", "release", 7, false, Value::Null)]}));
        // /schedule is the panel's alias: it reads the area again.
        let request = wire.read("schedule.list");
        wire.reply(&request, json!({"jobs": [job("job-2", "release", 7, false, Value::Null)]}));
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            line.clear();
        }
    });
    let mut terminal = Terminal::launch_tui(home.command());
    terminal.until("test / model");
    shown.send(()).unwrap();
    terminal.write(b"/scheduled\r");
    terminal.until("release");
    terminal.until("every 60 minutes");
    terminal.until("Next run 2026-11-02T09:00:00.000Z.");
    terminal.until("Last run completed at 2026-11-01T09:00:12.000Z.");
    terminal.raw(b"\r");
    terminal.until("Disabled nightly.");
    terminal.raw(b"\x14");
    terminal.until("Run nightly: skipped (overlap).");
    terminal.raw(b"\t");
    terminal.until("Recent runs of nightly");
    terminal.until("2026-11-01T09:00:12.000Z  completed");
    terminal.raw(b"\x1b");
    thread::sleep(Duration::from_millis(200));
    terminal.raw(b"\x18");
    // The hint's parts are painted separately: assert one contiguous run.
    terminal.until("nightly? Enter confirm");
    terminal.raw(b"\r");
    terminal.until("Removed nightly.");
    terminal.raw(b"\x1b");
    thread::sleep(Duration::from_millis(200));
    terminal.write(b"/schedule\r");
    terminal.until("every 60 minutes");
    terminal.raw(b"\x1b");
    thread::sleep(Duration::from_millis(200));
    terminal.write(b"/exit\r");
    terminal.until("Bye!");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
}

/// An editor that adds a prompt to whatever document it is given: what new
/// and existing jobs need to be written.
fn editor(home: &Home, name: &str, script: &str) -> PathBuf {
    let path = home.0.join(name);
    fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
}

fn edit_command(home: &Home, program: &std::path::Path, fullscreen: bool) -> Command {
    let mut command = home.command();
    command.env_remove("VISUAL").env("EDITOR", program);
    if fullscreen {
        command.arg("--fullscreen");
    }
    command
}

const EDITS_THE_PROMPT: &str = "case \"$(cat \"$1\")\" in\n\
    *nightly*) printf '%s' '{\"cwd\":\"/workspace\",\"enabled\":true,\"max_run_minutes\":60,\"mode\":\"create\",\"name\":\"nightly\",\"prompt\":\"Edited.\",\"schedule\":{\"every_minutes\":60}}' > \"$1\";;\n\
    *) printf '%s' '{\"cwd\":\"/workspace\",\"name\":\"example\",\"prompt\":\"New.\",\"schedule\":{\"every_minutes\":60}}' > \"$1\";;\n\
    esac";

/// Ctrl+E opens the job's document, Ctrl+N the create document, and each
/// writes with the method its kind names, in both terminal modes.
#[test]
fn tui_scheduled_panel_edits_and_creates_jobs() {
    for fullscreen in [false, true] {
        let home = Home::new();
        let script = editor(&home, "editor.sh", EDITS_THE_PROMPT);
        let (shown, wait_shown) = mpsc::channel();
        let daemon = home.daemon(move |wire| {
            hello(wire);
            wire.created();
            let attach = wire.read("session.attach");
            wire.reply(&attach, attach_record(&attach));
            let history = wire.read("session.history");
            wire.reply(
                &history,
                json!({"events":[],"through":8,"nextAfter":8,"done":true}),
            );
            for _ in 0..2 {
                let request = wire.read_any();
                match request["method"].as_str() {
                    Some("account.providers") => wire.reply(&request, json!([])),
                    Some("session.context.usage") => wire.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{
                        "code":-32601,"message":"Unknown method","data":{"appError":"unknown_method"}}})),
                    method => panic!("unexpected request {method:?}"),
                }
            }
            wait_shown.recv_timeout(PATIENCE).unwrap();
            let request = wire.read("schedule.list");
            wire.reply(&request, jobs(true));
            // Ctrl+E writes the job's own document at its revision; the
            // adapter reads the job again before it diffs the draft.
            let request = wire.read("schedule.list");
            wire.reply(&request, jobs(true));
            let request = wire.read("schedule.save");
            assert_eq!(request["params"]["revision"], 3);
            assert_eq!(request["params"]["job"]["id"], "job-1");
            assert_eq!(request["params"]["job"]["prompt"], "Edited.");
            wire.reply(&request, json!({"job": job("job-1", "nightly", 4, true, Value::Null)}));
            let request = wire.read("schedule.list");
            wire.reply(&request, jobs(true));
            // Ctrl+N writes a new job, with no revision.
            let request = wire.read("schedule.save");
            assert_eq!(request["params"].get("revision"), None);
            assert_eq!(request["params"]["job"]["name"], "example");
            assert_eq!(request["params"]["job"]["cwd"], "/workspace");
            wire.reply(&request, json!({"job": job("job-3", "example", 1, true, Value::Null)}));
            let request = wire.read("schedule.list");
            wire.reply(&request, jobs(true));
            // Tab asks for the history in this mode too: the adapter names
            // the job from the catalog it reads first.
            let request = wire.read("schedule.list");
            wire.reply(&request, jobs(true));
            let request = wire.read("schedule.history");
            assert_eq!(request["params"], json!({"job_id": "job-1", "limit": 20}));
            wire.reply(
                &request,
                json!({"runs": [run("completed", "", "2026-11-01T09:00:12.000Z")]}),
            );
            // Ctrl+X asks, Enter removes.
            let request = wire.read("schedule.delete");
            assert_eq!(request["params"], json!({"job_id": "job-1"}));
            wire.reply(&request, json!({"job_id": "job-1"}));
            let request = wire.read("schedule.list");
            wire.reply(&request, json!({"jobs": [job("job-2", "release", 7, false, Value::Null)]}));
            let mut line = String::new();
            while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
                line.clear();
            }
        });
        let mut terminal = Terminal::launch_tui(edit_command(&home, &script, fullscreen));
        terminal.until("test / model");
        shown.send(()).unwrap();
        terminal.write(b"/scheduled\r");
        terminal.until("every 60 minutes");
        terminal.raw(b"\x05");
        terminal.until("Saved nightly.");
        terminal.raw(b"\x0e");
        terminal.until("Added example.");
        terminal.raw(b"\t");
        terminal.until("Recent runs of nightly");
        terminal.raw(b"\x1b");
        thread::sleep(Duration::from_millis(200));
        terminal.raw(b"\x18");
        // The hint's parts are painted separately: assert one contiguous run.
        terminal.until("nightly? Enter confirm");
        terminal.raw(b"\r");
        terminal.until("Removed nightly.");
        terminal.raw(b"\x1b");
        thread::sleep(Duration::from_millis(200));
        terminal.write(b"/exit\r");
        terminal.until("Bye!");
        assert!(terminal.wait().success());
        daemon.join().unwrap();
    }
}

/// A stale revision writes nothing, says so, reads the area again, and the
/// next edit opens the stored job, in both terminal modes.
#[test]
fn tui_scheduled_panel_keeps_the_users_text_when_the_revision_is_stale() {
    for fullscreen in [false, true] {
        let home = Home::new();
        let script = editor(&home, "editor.sh", EDITS_THE_PROMPT);
        let (shown, wait_shown) = mpsc::channel();
        let daemon = home.daemon(move |wire| {
            hello(wire);
            wire.created();
            let attach = wire.read("session.attach");
            wire.reply(&attach, attach_record(&attach));
            let history = wire.read("session.history");
            wire.reply(
                &history,
                json!({"events":[],"through":8,"nextAfter":8,"done":true}),
            );
            for _ in 0..2 {
                let request = wire.read_any();
                match request["method"].as_str() {
                    Some("account.providers") => wire.reply(&request, json!([])),
                    Some("session.context.usage") => wire.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{
                        "code":-32601,"message":"Unknown method","data":{"appError":"unknown_method"}}})),
                    method => panic!("unexpected request {method:?}"),
                }
            }
            wait_shown.recv_timeout(PATIENCE).unwrap();
            let request = wire.read("schedule.list");
            wire.reply(&request, jobs(true));
            // The adapter reads the job again before it diffs the draft.
            let request = wire.read("schedule.list");
            wire.reply(&request, jobs(true));
            let request = wire.read("schedule.save");
            assert_eq!(request["params"]["revision"], 3);
            wire.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{
                "code":-32000,
                "message":"The job changed; read it before updating",
                "data":{"appError":"schedule_conflict"}}}));
            // The conflict writes nothing, so the adapter reads the area
            // again, and the panel moves to the newer revision.
            let request = wire.read("schedule.list");
            wire.reply(&request, newer_jobs());
            // The next edit opens the stored job at that revision, not the
            // rejected text, and the adapter reads the job again to diff it.
            let request = wire.read("schedule.list");
            wire.reply(&request, newer_jobs());
            let request = wire.read("schedule.save");
            assert_eq!(request["params"]["revision"], 4);
            assert_eq!(request["params"]["job"]["prompt"], "Edited.");
            wire.reply(&request, json!({"job": job("job-1", "nightly", 5, true, Value::Null)}));
            let request = wire.read("schedule.list");
            wire.reply(&request, jobs(true));
            let mut line = String::new();
            while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
                line.clear();
            }
        });
        let mut terminal = Terminal::launch_tui(edit_command(&home, &script, fullscreen));
        terminal.until("test / model");
        shown.send(()).unwrap();
        terminal.write(b"/scheduled\r");
        terminal.until("every 60 minutes");
        terminal.raw(b"\x05");
        terminal.until("The job changed since it was read; nothing was written.");
        terminal.until("Your text is kept in");
        terminal.raw(b"\x05");
        terminal.until("Saved nightly.");
        terminal.raw(b"\x1b");
        thread::sleep(Duration::from_millis(200));
        terminal.write(b"/exit\r");
        terminal.until("Bye!");
        assert!(terminal.wait().success());
        daemon.join().unwrap();
    }
}

/// A delete the owner refuses as a conflict writes nothing, says so, and
/// shows the jobs as they now are.
#[test]
fn tui_scheduled_panel_reports_a_conflict_on_remove() {
    let home = Home::new();
    let (shown, wait_shown) = mpsc::channel();
    let daemon = home.daemon(move |wire| {
        hello(wire);
        wire.created();
        let attach = wire.read("session.attach");
        wire.reply(&attach, attach_record(&attach));
        let history = wire.read("session.history");
        wire.reply(
            &history,
            json!({"events":[],"through":8,"nextAfter":8,"done":true}),
        );
        for _ in 0..2 {
            let request = wire.read_any();
            match request["method"].as_str() {
                Some("account.providers") => wire.reply(&request, json!([])),
                Some("session.context.usage") => wire.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{
                    "code":-32601,"message":"Unknown method","data":{"appError":"unknown_method"}}})),
                method => panic!("unexpected request {method:?}"),
            }
        }
        wait_shown.recv_timeout(PATIENCE).unwrap();
        let request = wire.read("schedule.list");
        wire.reply(&request, jobs(true));
        // Ctrl+X, Enter: the delete races a tool save.
        let request = wire.read("schedule.delete");
        assert_eq!(request["params"], json!({"job_id": "job-1"}));
        wire.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{
            "code":-32000,
            "message":"The jobs changed; read them before deleting",
            "data":{"appError":"schedule_conflict"}}}));
        // The conflict wrote nothing, so the jobs are read again.
        let request = wire.read("schedule.list");
        wire.reply(&request, json!({"jobs": [
            job("job-1", "nightly", 4, true, run("completed", "", "2026-11-01T09:00:12.000Z")),
            job("job-2", "release", 7, false, Value::Null),
        ]}));
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            line.clear();
        }
    });
    let mut terminal = Terminal::launch_tui(home.command());
    terminal.until("test / model");
    shown.send(()).unwrap();
    terminal.write(b"/scheduled\r");
    terminal.until("every 60 minutes");
    terminal.raw(b"\x18");
    terminal.until("nightly? Enter confirm");
    terminal.raw(b"\r");
    terminal.until("The job changed since it was read; nothing was removed.");
    // The job is still listed at the revision the read reported: asking
    // again names it.
    terminal.raw(b"\x18");
    terminal.until("nightly? Enter confirm");
    terminal.raw(b"\x1b");
    thread::sleep(Duration::from_millis(200));
    terminal.raw(b"\x1b");
    thread::sleep(Duration::from_millis(200));
    terminal.write(b"/exit\r");
    terminal.until("Bye!");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
}

/// At 80 columns the Scheduler's keys wrap above the bottom border, so every
/// key stays visible, and the remove confirmation for a long name is drawn
/// whole, in both terminal modes.
#[test]
fn tui_scheduled_panel_shows_every_key_and_confirmation_at_80_columns() {
    for fullscreen in [false, true] {
        let home = Home::new();
        let (shown, wait_shown) = mpsc::channel();
        let daemon = home.daemon(move |wire| {
            hello(wire);
            wire.created();
            let attach = wire.read("session.attach");
            wire.reply(&attach, attach_record(&attach));
            let history = wire.read("session.history");
            wire.reply(
                &history,
                json!({"events":[],"through":8,"nextAfter":8,"done":true}),
            );
            for _ in 0..2 {
                let request = wire.read_any();
                match request["method"].as_str() {
                    Some("account.providers") => wire.reply(&request, json!([])),
                    Some("session.context.usage") => wire.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{
                        "code":-32601,"message":"Unknown method","data":{"appError":"unknown_method"}}})),
                    method => panic!("unexpected request {method:?}"),
                }
            }
            wait_shown.recv_timeout(PATIENCE).unwrap();
            let request = wire.read("schedule.list");
            wire.reply(
                &request,
                json!({"jobs": [
                    job("job-1", "a job whose name is rather long", 3, true, Value::Null),
                    job(
                        "job-2",
                        "a job whose name is exactly sixty cells wide to wrap it well",
                        4,
                        false,
                        Value::Null,
                    ),
                ]}),
            );
            let mut line = String::new();
            while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
                line.clear();
            }
        });
        let mut command = home.command();
        if fullscreen {
            command.arg("--fullscreen");
        }
        sized(&mut command, 80, 24);
        let mut terminal = Terminal::launch_tui(command);
        terminal.until("test / model");
        shown.send(()).unwrap();
        terminal.write(b"/scheduled\r");
        terminal.until("every 60 minutes");
        // Every key is drawn, wrapped above the border at this width. A key
        // and its action are painted as separate spans, so each is asserted
        // on its own; a frame redraws the same hint, so each search starts
        // anywhere in the transcript.
        for key in [
            "Enter", "Ctrl+E", "Ctrl+R", "Ctrl+N", "Ctrl+X", "Ctrl+T", "Tab", "Esc",
        ] {
            terminal.until_anywhere(key);
        }
        for action in [
            " toggle", " edit", " reload", " add", " remove", " run", " inspect", " close",
        ] {
            terminal.until_anywhere(action);
        }
        terminal.raw(b"\x18");
        terminal.until("a job whose name is rather long? Enter confirm");
        // Esc cancels the confirmation.
        terminal.raw(b"\x1b");
        thread::sleep(Duration::from_millis(200));
        // A sixty-cell name wraps the confirmation across rows; the keys and
        // their actions are drawn whole, never cut.
        terminal.raw(b"\x1b[B");
        thread::sleep(Duration::from_millis(200));
        terminal.raw(b"\x18");
        // The confirmation names the selected job, then its keys.
        terminal.until("sixty cells wide to wrap it well?");
        terminal.until("Enter");
        terminal.until(" confirm");
        terminal.until("Esc");
        terminal.until(" cancel");
        // A second Esc closes the panel.
        terminal.raw(b"\x1b");
        thread::sleep(Duration::from_millis(200));
        terminal.raw(b"\x1b");
        thread::sleep(Duration::from_millis(200));
        terminal.write(b"/exit\r");
        terminal.until("Bye!");
        assert!(terminal.wait().success());
        daemon.join().unwrap();
    }
}

/// A create the owner refuses keeps the user's text, and the next Ctrl+N
/// writes that same text again with no revision, in both terminal modes.
#[test]
fn tui_scheduled_panel_keeps_the_users_text_when_a_create_is_refused() {
    for fullscreen in [false, true] {
        let home = Home::new();
        let script = editor(&home, "editor.sh", EDITS_THE_PROMPT);
        let (shown, wait_shown) = mpsc::channel();
        let daemon = home.daemon(move |wire| {
            hello(wire);
            wire.created();
            let attach = wire.read("session.attach");
            wire.reply(&attach, attach_record(&attach));
            let history = wire.read("session.history");
            wire.reply(
                &history,
                json!({"events":[],"through":8,"nextAfter":8,"done":true}),
            );
            for _ in 0..2 {
                let request = wire.read_any();
                match request["method"].as_str() {
                    Some("account.providers") => wire.reply(&request, json!([])),
                    Some("session.context.usage") => wire.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{
                        "code":-32601,"message":"Unknown method","data":{"appError":"unknown_method"}}})),
                    method => panic!("unexpected request {method:?}"),
                }
            }
            wait_shown.recv_timeout(PATIENCE).unwrap();
            let request = wire.read("schedule.list");
            wire.reply(&request, jobs(true));
            // Ctrl+N: the owner refuses the create.
            let request = wire.read("schedule.save");
            assert_eq!(request["params"].get("revision"), None);
            let edited = request["params"]["job"].clone();
            assert_eq!(edited["name"], "example");
            wire.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{
                "code":-32000,
                "message":"The job is not acceptable",
                "data":{"appError":"schedule_invalid"}}}));
            // The next Ctrl+N writes the same text again, with no revision.
            let request = wire.read("schedule.save");
            assert_eq!(request["params"].get("revision"), None);
            assert_eq!(request["params"]["job"], edited);
            wire.reply(&request, json!({"job": job("job-3", "example", 1, true, Value::Null)}));
            let request = wire.read("schedule.list");
            wire.reply(&request, jobs(true));
            let mut line = String::new();
            while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
                line.clear();
            }
        });
        let mut terminal = Terminal::launch_tui(edit_command(&home, &script, fullscreen));
        terminal.until("test / model");
        shown.send(()).unwrap();
        terminal.write(b"/scheduled\r");
        terminal.until("every 60 minutes");
        terminal.raw(b"\x0e");
        terminal.until("The job is not acceptable");
        terminal.until("kept in");
        terminal.raw(b"\x0e");
        terminal.until("Added example.");
        terminal.raw(b"\x1b");
        thread::sleep(Duration::from_millis(200));
        terminal.write(b"/exit\r");
        terminal.until("Bye!");
        assert!(terminal.wait().success());
        daemon.join().unwrap();
    }
}

/// A refused edit keeps its text for its own job: editing another job after
/// the refusal opens that job's document, and coming back reopens the kept
/// text at the revision it was read at.
#[test]
fn tui_scheduled_panel_keeps_a_refused_edit_for_its_own_job() {
    let home = Home::new();
    let seen = home.0.join("seen.log");
    let script = editor(
        &home,
        "editor.sh",
        &format!(
            "cat \"$1\" >> \"{0}\"\n\
             printf '\\n---\\n' >> \"{0}\"\n\
             case \"$(cat \"$1\")\" in\n\
             *'\"name\": \"release\"'*)\n\
               printf '%s' '{{\"cwd\":\"/workspace\",\"enabled\":false,\"max_run_minutes\":60,\"mode\":\"create\",\"name\":\"release\",\"prompt\":\"Release edited.\",\"schedule\":{{\"every_minutes\":60}}}}' > \"$1\" ;;\n\
             *'\"prompt\":\"Broken\"'*)\n\
               printf '%s' '{{\"cwd\":\"/workspace\",\"enabled\":true,\"max_run_minutes\":60,\"mode\":\"create\",\"name\":\"nightly\",\"prompt\":\"Fixed.\",\"schedule\":{{\"every_minutes\":60}}}}' > \"$1\" ;;\n\
             *)\n\
               printf '%s' '{{\"cwd\":\"/workspace\",\"enabled\":true,\"max_run_minutes\":60,\"mode\":\"create\",\"name\":\"nightly\",\"prompt\":\"Broken\",\"schedule\":{{\"cron\":\"not a cron\"}}}}' > \"$1\" ;;\n\
             esac\n",
            seen.display()
        ),
    );
    let (shown, wait_shown) = mpsc::channel();
    let daemon = home.daemon(move |wire| {
        hello(wire);
        wire.created();
        let attach = wire.read("session.attach");
        wire.reply(&attach, attach_record(&attach));
        let history = wire.read("session.history");
        wire.reply(
            &history,
            json!({"events":[],"through":8,"nextAfter":8,"done":true}),
        );
        for _ in 0..2 {
            let request = wire.read_any();
            match request["method"].as_str() {
                Some("account.providers") => wire.reply(&request, json!([])),
                Some("session.context.usage") => wire.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{
                    "code":-32601,"message":"Unknown method","data":{"appError":"unknown_method"}}})),
                method => panic!("unexpected request {method:?}"),
            }
        }
        wait_shown.recv_timeout(PATIENCE).unwrap();
        let request = wire.read("schedule.list");
        wire.reply(&request, jobs(true));
        // Ctrl+E on the first job: the owner refuses the invalid cron. The
        // adapter reads the job again before it diffs the draft.
        let request = wire.read("schedule.list");
        wire.reply(&request, jobs(true));
        let request = wire.read("schedule.save");
        assert_eq!(request["params"]["revision"], 3);
        assert_eq!(request["params"]["job"]["id"], "job-1");
        assert_eq!(
            request["params"]["job"]["schedule"],
            json!({"cron": "not a cron"})
        );
        wire.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{
            "code":-32000,
            "message":"Invalid cron expression or time zone",
            "data":{"appError":"schedule_invalid"}}}));
        // Ctrl+E on the second job: a fresh document for that job, written
        // with its own id and revision.
        let request = wire.read("schedule.list");
        wire.reply(&request, jobs(true));
        let request = wire.read("schedule.save");
        assert_eq!(request["params"]["job"]["id"], "job-2");
        assert_eq!(request["params"]["revision"], 7);
        assert_eq!(request["params"]["job"]["prompt"], "Release edited.");
        wire.reply(&request, json!({"job": job("job-2", "release", 8, false, Value::Null)}));
        let request = wire.read("schedule.list");
        wire.reply(&request, jobs(true));
        // Back on the first job: its kept text reopens at its own revision.
        let request = wire.read("schedule.list");
        wire.reply(&request, jobs(true));
        let request = wire.read("schedule.save");
        assert_eq!(request["params"]["job"]["id"], "job-1");
        assert_eq!(request["params"]["revision"], 3);
        assert_eq!(request["params"]["job"]["prompt"], "Fixed.");
        wire.reply(&request, json!({"job": job("job-1", "nightly", 4, true, Value::Null)}));
        let request = wire.read("schedule.list");
        wire.reply(&request, jobs(true));
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            line.clear();
        }
    });
    let mut terminal = Terminal::launch_tui(edit_command(&home, &script, false));
    terminal.until("test / model");
    shown.send(()).unwrap();
    terminal.write(b"/scheduled\r");
    terminal.until("every 60 minutes");
    terminal.raw(b"\x05");
    terminal.until("Invalid cron expression or time zone");
    terminal.until("kept in");
    // The second job's edit is its own document, not the kept text.
    terminal.raw(b"\x1b[B");
    terminal.raw(b"\x05");
    terminal.until("Saved release.");
    // The first job's edit opens the text the owner refused.
    terminal.raw(b"\x1b[A");
    terminal.raw(b"\x05");
    terminal.until("Saved nightly.");
    let log = fs::read_to_string(&seen).unwrap();
    let runs: Vec<&str> = log.split("---\n").collect();
    assert!(
        runs[1].contains("\"name\": \"release\""),
        "the second edit opened the second job's document: {log}"
    );
    assert!(
        runs[2].contains("\"prompt\":\"Broken\""),
        "the third edit reopened the refused text: {log}"
    );
    terminal.raw(b"\x1b");
    thread::sleep(Duration::from_millis(200));
    terminal.write(b"/exit\r");
    terminal.until("Bye!");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
}

/// A job the owner named with an escape sequence, a newline and a tab stays
/// on one line wherever the name is drawn: the command line's columns, the
/// panel's row and the remove confirmation.
#[test]
fn scheduled_draws_a_job_name_on_one_line() {
    const NAME: &str = "evil\u{1b}[2J\nx\ty";
    // The command line's columns: one line per job, no escape sequence.
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        hello(wire);
        let request = wire.read("schedule.list");
        wire.reply(
            &request,
            json!({"jobs": [job("job-1", NAME, 3, true, Value::Null)]}),
        );
    });
    let output = home.command().args(["scheduled", "list"]).output().unwrap();
    daemon.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "job-1\tevil x y\tevery 60 minutes\tenabled\t2026-11-02T09:00:00.000Z\t—\n"
    );
    // The panel's row and the confirmation it asks in.
    let home = Home::new();
    let (shown, wait_shown) = mpsc::channel();
    let daemon = home.daemon(move |wire| {
        hello(wire);
        wire.created();
        let attach = wire.read("session.attach");
        wire.reply(&attach, attach_record(&attach));
        let history = wire.read("session.history");
        wire.reply(
            &history,
            json!({"events":[],"through":8,"nextAfter":8,"done":true}),
        );
        for _ in 0..2 {
            let request = wire.read_any();
            match request["method"].as_str() {
                Some("account.providers") => wire.reply(&request, json!([])),
                Some("session.context.usage") => wire.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{
                    "code":-32601,"message":"Unknown method","data":{"appError":"unknown_method"}}})),
                method => panic!("unexpected request {method:?}"),
            }
        }
        wait_shown.recv_timeout(PATIENCE).unwrap();
        let request = wire.read("schedule.list");
        wire.reply(
            &request,
            json!({"jobs": [job("job-1", NAME, 3, true, Value::Null)]}),
        );
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            line.clear();
        }
    });
    let mut terminal = Terminal::launch_tui(home.command());
    terminal.until("test / model");
    shown.send(()).unwrap();
    terminal.write(b"/scheduled\r");
    terminal.until("evil x y");
    terminal.raw(b"\x18");
    // The hint's parts are painted separately: assert one contiguous run.
    terminal.until("evil x y? Enter confirm");
    assert!(
        !terminal.transcript.contains("[2J"),
        "the owner's escape sequence never reaches the screen"
    );
    terminal.raw(b"\x1b");
    thread::sleep(Duration::from_millis(200));
    terminal.raw(b"\x1b");
    thread::sleep(Duration::from_millis(200));
    terminal.write(b"/exit\r");
    terminal.until("Bye!");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
}

/// A create the owner accepted whose answer does not parse is applied work,
/// not a refusal: the draft's file goes, and the next Ctrl+N opens a fresh
/// document instead of the text the user wrote.
#[test]
fn tui_scheduled_panel_reports_an_unreadable_create_reply_as_applied() {
    let home = Home::new();
    let seen = home.0.join("seen.log");
    let script = editor(
        &home,
        "editor.sh",
        &format!(
            "cat \"$1\" >> \"{0}\"\n\
             printf '\\n---\\n' >> \"{0}\"\n\
             printf '%s' '{{\"cwd\":\"/workspace\",\"name\":\"attempt\",\"prompt\":\"Attempt.\",\"schedule\":{{\"every_minutes\":60}}}}' > \"$1\"\n",
            seen.display()
        ),
    );
    let (shown, wait_shown) = mpsc::channel();
    let daemon = home.daemon(move |wire| {
        hello(wire);
        wire.created();
        let attach = wire.read("session.attach");
        wire.reply(&attach, attach_record(&attach));
        let history = wire.read("session.history");
        wire.reply(
            &history,
            json!({"events":[],"through":8,"nextAfter":8,"done":true}),
        );
        for _ in 0..2 {
            let request = wire.read_any();
            match request["method"].as_str() {
                Some("account.providers") => wire.reply(&request, json!([])),
                Some("session.context.usage") => wire.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{
                    "code":-32601,"message":"Unknown method","data":{"appError":"unknown_method"}}})),
                method => panic!("unexpected request {method:?}"),
            }
        }
        wait_shown.recv_timeout(PATIENCE).unwrap();
        let request = wire.read("schedule.list");
        wire.reply(&request, jobs(true));
        // Ctrl+N: the owner wrote the job, and its answer has no job object.
        let request = wire.read("schedule.save");
        assert_eq!(request["params"].get("revision"), None);
        wire.reply(&request, json!({}));
        // The read that reports the jobs still works.
        let request = wire.read("schedule.list");
        wire.reply(&request, jobs(true));
        // The next Ctrl+N writes a fresh document, not the one just saved.
        let request = wire.read("schedule.save");
        assert_eq!(request["params"].get("revision"), None);
        wire.reply(&request, json!({"job": job("job-3", "attempt", 1, true, Value::Null)}));
        let request = wire.read("schedule.list");
        wire.reply(&request, jobs(true));
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            line.clear();
        }
    });
    let mut terminal = Terminal::launch_tui(edit_command(&home, &script, false));
    terminal.until("test / model");
    shown.send(()).unwrap();
    terminal.write(b"/scheduled\r");
    terminal.until("every 60 minutes");
    terminal.raw(b"\x0e");
    // The parse error is the read error, and the line still says what was
    // done.
    terminal.until("Added.");
    terminal.until("schedule.save:");
    // The panel keeps no view, so Ctrl+R reads the jobs again.
    terminal.raw(b"\x12");
    terminal.until("every 60 minutes");
    terminal.raw(b"\x0e");
    terminal.until("Added attempt.");
    let log = fs::read_to_string(&seen).unwrap();
    let runs: Vec<&str> = log.split("---\n").collect();
    assert!(
        runs[1].contains("\"name\": \"example\""),
        "the second create opened a fresh document: {log}"
    );
    assert!(
        !runs[1].contains("attempt"),
        "the written text is no longer linked: {log}"
    );
    terminal.raw(b"\x1b");
    thread::sleep(Duration::from_millis(200));
    terminal.write(b"/exit\r");
    terminal.until("Bye!");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
}

/// A daemon without the Scheduler's methods leaves the panel open on the
/// unsupported-daemon sentence rather than an empty list.
#[test]
fn tui_scheduled_panel_says_when_the_daemon_cannot_manage_it() {
    let home = Home::new();
    let (shown, wait_shown) = mpsc::channel();
    let daemon = home.daemon(move |wire| {
        wire.hello(&[]);
        wire.created();
        let attach = wire.read("session.attach");
        wire.reply(&attach, attach_record(&attach));
        let history = wire.read("session.history");
        wire.reply(
            &history,
            json!({"events":[],"through":8,"nextAfter":8,"done":true}),
        );
        for _ in 0..2 {
            let request = wire.read_any();
            match request["method"].as_str() {
                Some("account.providers") => wire.reply(&request, json!([])),
                Some("session.context.usage") => wire.send(json!({"jsonrpc":"2.0","id":request["id"],"error":{
                    "code":-32601,"message":"Unknown method","data":{"appError":"unknown_method"}}})),
                method => panic!("unexpected request {method:?}"),
            }
        }
        wait_shown.recv_timeout(PATIENCE).unwrap();
        let mut line = String::new();
        while wire.0.read_line(&mut line).is_ok_and(|read| read > 0) {
            line.clear();
        }
    });
    let mut terminal = Terminal::launch_tui(home.command());
    terminal.until("test / model");
    shown.send(()).unwrap();
    terminal.write(b"/scheduled\r");
    terminal.until("This daemon cannot manage scheduled jobs.");
    terminal.raw(b"\x1b");
    thread::sleep(Duration::from_millis(200));
    terminal.write(b"/exit\r");
    terminal.until("Bye!");
    assert!(terminal.wait().success());
    daemon.join().unwrap();
}

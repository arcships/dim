//! `dimcode modality` and `set-default`: the capability snapshot and one
//! default write over the public wire (Modality A.5).
use super::*;

fn hello(wire: &mut Wire) {
    wire.hello(&["modality.capabilities", "modality.configure"]);
}

fn operation(category: &str, operation: &str, available: bool) -> Value {
    let reasons = if available {
        json!([])
    } else {
        json!([{"dimension": "adapter", "detail": "No image adapter in this build"}])
    };
    json!({
        "operation": operation,
        "key": format!("modality.{category}.{operation}"),
        "available": available,
        "reasons": reasons,
        "default": null,
        "effective": {"providerId": "openai", "modelId": "gpt-image-2", "source": "automatic"},
        "options": [{"providerId": "openai", "modelId": "gpt-image-2", "available": true}],
    })
}

fn snapshot(revision: &str) -> Value {
    json!({
        "revision": revision,
        "categories": [
            {
                "category": "image",
                "available": false,
                "reasons": [{"dimension": "adapter", "detail": "No image adapter in this build"}],
                "operations": [operation("image", "generate", false), operation("image", "understand", true)],
            },
            {
                "category": "ocr",
                "available": false,
                "reasons": [{"dimension": "build", "detail": "No OCR helper in this build"}],
                "operations": [json!({
                    "operation": "recognize",
                    "key": "modality.ocr",
                    "available": false,
                    "reasons": [{"dimension": "build", "detail": "No OCR helper in this build"}],
                    "default": null,
                    "effective": null,
                    "options": [],
                })],
            },
        ],
    })
}

#[test]
fn modality_listing_renders_the_operation_table() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        hello(wire);
        let request = wire.read("modality.capabilities");
        assert_eq!(request["params"], json!({}));
        wire.reply(&request, snapshot("revision-1"));
    });
    let output = home.command().args(["modality"]).output().unwrap();
    daemon.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "CATEGORY\tOPERATION\tAVAILABLE\tDEFAULT\tEFFECTIVE\tREASON\n\
         image\tgenerate\tno\t-\topenai/gpt-image-2 (automatic)\tadapter: No image adapter in this build\n\
         image\tunderstand\tyes\t-\topenai/gpt-image-2 (automatic)\t-\n\
         ocr\trecognize\tno\t-\t-\tbuild: No OCR helper in this build\n"
    );
    assert_eq!(String::from_utf8_lossy(&output.stderr), "");
}

#[test]
fn modality_json_prints_the_reply_unchanged() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        hello(wire);
        let request = wire.read("modality.capabilities");
        wire.reply(&request, snapshot("revision-1"));
    });
    let output = home
        .command()
        .args(["modality", "--json"])
        .output()
        .unwrap();
    daemon.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let printed: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(printed, snapshot("revision-1"));
}

#[test]
fn set_default_writes_the_revision_it_read_and_retries_a_conflict() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        hello(wire);
        let read = wire.read("modality.capabilities");
        wire.reply(&read, snapshot("revision-1"));
        let write = wire.read("modality.configure");
        assert_eq!(
            write["params"],
            json!({
                "expectedRevision": "revision-1",
                "category": "image",
                "operation": "generate",
                "model": {"providerId": "openai", "modelId": "gpt-image-2"},
            })
        );
        wire.send(json!({"jsonrpc":"2.0","id":write["id"],"error":{
            "code":-32000,
            "message":"Modality defaults changed; read the current snapshot before saving again",
            "data":{"appError":"modality_conflict"}}}));
        let reread = wire.read("modality.capabilities");
        wire.reply(&reread, snapshot("revision-2"));
        let retry = wire.read("modality.configure");
        assert_eq!(retry["params"]["expectedRevision"], "revision-2");
        wire.reply(&retry, snapshot("revision-3"));
    });
    let output = home
        .command()
        .args([
            "modality",
            "set-default",
            "image.generate",
            "openai/gpt-image-2",
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
        "Set image.generate to openai/gpt-image-2.\n\
         image\tgenerate\tno\t-\topenai/gpt-image-2 (automatic)\tadapter: No image adapter in this build\n"
    );
}

#[test]
fn set_default_clear_sends_a_null_model() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        hello(wire);
        let read = wire.read("modality.capabilities");
        wire.reply(&read, snapshot("revision-1"));
        let write = wire.read("modality.configure");
        assert_eq!(
            write["params"],
            json!({
                "expectedRevision": "revision-1",
                "category": "ocr",
                "model": null,
            })
        );
        wire.reply(&write, snapshot("revision-2"));
    });
    let output = home
        .command()
        .args(["modality", "set-default", "ocr", "--clear"])
        .output()
        .unwrap();
    daemon.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .starts_with("Cleared ocr.\nocr\trecognize\t"),
    );
}

#[test]
fn a_daemon_without_modality_says_so_and_exits_one() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.hello(&[]);
        assert!(
            wire.trailing_request().is_none(),
            "a command for a daemon without Modality calls nothing"
        );
    });
    let output = home.command().args(["modality"]).output().unwrap();
    daemon.join().unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(output.stdout, b"");
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "Error: This daemon serves no Modality.\n"
    );
}

/// The record a daemon answers with (A.4), in `state`.
fn record(task_id: &str, state: &str) -> Value {
    json!({
        "version": 1,
        "task_id": task_id,
        "client_task_id": null,
        "revision": 1,
        "category": "ocr",
        "operation": "recognize",
        "executor": {
            "kind": "host",
            "node": null,
            "target": {"providerId": "local", "modelId": "apple-vision"},
        },
        "params": {"outputMode": "text"},
        "state": state,
        "remote_handle": null,
        "progress": null,
        "result": null,
        "error": null,
        "remote_cancel": null,
        "created_at": "2026-11-07T00:00:00Z",
        "updated_at": "2026-11-07T00:00:00Z",
        "finished_at": null,
    })
}

/// `dimcode modality task status` prints a host task's row, its files and
/// the executor's target.
#[test]
fn task_status_prints_a_host_task() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.hello(&["modality.capabilities", "modality.task.status"]);
        let status = wire.read("modality.task.status");
        assert_eq!(status["params"], json!({"task_id": "ocr-1"}));
        let mut answered = record("ocr-1", "succeeded");
        answered["progress"] = json!({"fraction": 1.0, "message": "Done"});
        answered["result"] = json!({"files": [
            {"name": "text.txt", "media_type": "text/plain", "bytes": 12, "sha256": "00"},
            {"name": "result.json", "media_type": "application/json", "bytes": 34, "sha256": "01"},
        ], "data": {"pages": [{"page": 1}]}});
        wire.reply(&status, json!({"task": answered}));
    });
    let output = home
        .command()
        .args(["modality", "task", "status", "ocr-1"])
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
        "ID\tSTATE\tOPERATION\tMODEL\tCREATED\n\
         ocr-1\tsucceeded\tocr.recognize\tlocal/apple-vision\t2026-11-07T00:00:00Z\n\
         Progress: 100% Done\n\
         Result: text.txt\ttext/plain\t12\n\
         Result: result.json\tapplication/json\t34\n"
    );
}

/// `dimcode modality task cancel` reports the state the owner returned.
#[test]
fn task_cancel_cancels_a_host_task() {
    let home = Home::new();
    let daemon = home.daemon(|wire| {
        wire.hello(&["modality.capabilities", "modality.task.cancel"]);
        let cancel = wire.read("modality.task.cancel");
        assert_eq!(cancel["params"], json!({"task_id": "ocr-1"}));
        wire.reply(&cancel, json!({"task": record("ocr-1", "cancelled")}));
    });
    let output = home
        .command()
        .args(["modality", "task", "cancel", "ocr-1"])
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
        "Cancelled ocr-1.\n"
    );
}

/// The first Ctrl+C during a wait prints the id, says the task continues,
/// and exits 130 (A.5, A8).
#[test]
fn the_first_interrupt_prints_the_id_and_exits_130() {
    use std::sync::mpsc;
    let home = Home::new();
    let (waiting, waiting_rx) = mpsc::channel();
    let daemon = home.daemon(move |wire| {
        wire.hello(&["modality.capabilities", "modality.task.submit"]);
        let submit = wire.read("modality.task.submit");
        wire.reply(&submit, json!({"task": record("ocr-2", "queued")}));
        let wait = wire.read("modality.task.wait");
        assert_eq!(wait["params"]["task_id"], "ocr-2");
        waiting.send(()).unwrap();
        // Left unanswered: the interrupt arrives while the CLI waits.
        assert!(wire.trailing_request().is_none());
    });
    let child = home
        .command()
        .args(["image", "generate", "a cat"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    waiting_rx.recv_timeout(HANG_GUARD).unwrap();
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(unsafe { libc::kill(child.id() as i32, libc::SIGINT) }, 0);
    let output = child.wait_with_output().unwrap();
    daemon.join().unwrap();
    assert_eq!(output.status.code(), Some(130));
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "ocr-2\n");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("Stopped waiting"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// End to end through the product daemon and Apple Vision: a short text
/// prints inline and writes nothing, `--output-mode layout` writes the page
/// images, `--json` prints the record, and the task it leaves is managed
/// through `modality task`.
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn ocr_recognizes_through_apple_vision() {
    let home = Home::new();
    // The first command starts the product daemon for this home; every later
    // one connects to it.
    let probe = home.command().args(["modality"]).output().unwrap();
    assert!(
        probe.status.success(),
        "{}",
        String::from_utf8_lossy(&probe.stderr)
    );
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../dim-ocr/tests/fixtures/latin.png")
        .canonicalize()
        .unwrap();
    let scratch = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("ocr-flow-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&scratch).unwrap();
    let mut command = home.command();
    command.current_dir(&scratch);
    let output = command
        .args(["ocr", fixture.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("12345678"), "{text}");
    assert_eq!(
        fs::read_dir(&scratch).unwrap().count(),
        0,
        "a short text writes nothing"
    );

    // A PDF whose pages are rasterized in layout mode, so the page images
    // are part of the result set.
    let pdf = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../dim-ocr/tests/fixtures/mixed.pdf")
        .canonicalize()
        .unwrap();
    let out = scratch.join("layout");
    let output = home
        .command()
        .current_dir(&scratch)
        .args([
            "ocr",
            pdf.to_str().unwrap(),
            "--output-mode",
            "layout",
            "--out",
            out.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    for name in ["text.txt", "result.json", "page-0001.png", "page-0002.png"] {
        assert!(out.join(name).exists(), "{name} was not written");
    }
    let printed = String::from_utf8(output.stdout).unwrap();
    assert!(printed.contains("text.txt"), "{printed}");
    let document: Value =
        serde_json::from_slice(&fs::read(out.join("result.json")).unwrap()).unwrap();
    assert_eq!(document["pageCount"], 2);
    assert!(
        document["pages"][0]["text"]
            .as_str()
            .unwrap()
            .contains("Native page one")
    );
    assert!(
        fs::read_to_string(out.join("text.txt"))
            .unwrap()
            .contains("Native page one"),
        "the whole text is in text.txt"
    );

    let output = home
        .command()
        .args(["ocr", fixture.to_str().unwrap(), "--json", "--no-wait"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let record: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(record["task"]["category"], "ocr");
    assert_eq!(
        record["task"]["executor"]["target"]["modelId"],
        "apple-vision"
    );
    let id = record["task"]["task_id"].as_str().unwrap().to_owned();

    let output = home
        .command()
        .args(["modality", "task", "status", id.as_str()])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let printed = String::from_utf8(output.stdout).unwrap();
    assert!(printed.contains("ocr.recognize"), "{printed}");
    assert!(printed.contains("local/apple-vision"), "{printed}");

    let output = home
        .command()
        .args(["modality", "task", "list"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains(&id));

    let output = home
        .command()
        .args(["modality", "task", "cancel", id.as_str()])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let printed = String::from_utf8(output.stdout).unwrap();
    assert!(
        printed.ends_with("(unchanged).\n") || printed.starts_with("Cancelled"),
        "{printed}"
    );
    fs::remove_dir_all(&scratch).unwrap();
}

/// One operation of a `modality.capabilities` reply.
fn listed<'a>(snapshot: &'a Value, category: &str, name: &str) -> &'a Value {
    snapshot["categories"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["category"] == category)
        .and_then(|entry| {
            entry["operations"]
                .as_array()
                .unwrap()
                .iter()
                .find(|operation| operation["operation"] == name)
        })
        .unwrap()
}

/// The product daemon's own refusal reaches the CLI unchanged: the listing's
/// reasons for `image.generate`, in order, then no task (D3, Appendix C).
#[test]
fn the_product_refusal_reaches_the_cli_unchanged() {
    let home = Home::new();
    // The first command starts the product daemon for this home.
    let listing = home
        .command()
        .args(["modality", "--json"])
        .output()
        .unwrap();
    assert!(
        listing.status.success(),
        "{}",
        String::from_utf8_lossy(&listing.stderr)
    );
    let snapshot: Value = serde_json::from_slice(&listing.stdout).unwrap();
    let generate = listed(&snapshot, "image", "generate");
    assert_eq!(generate["available"], false, "{generate}");
    let reasons = generate["reasons"].as_array().unwrap();
    assert!(!reasons.is_empty(), "{generate}");
    let mut expected = String::from("Error: image.generate is unavailable.");
    for reason in reasons {
        expected.push_str(&format!(
            "\n{}: {}",
            reason["dimension"].as_str().unwrap(),
            reason["detail"].as_str().unwrap()
        ));
    }
    expected.push('\n');

    let output = home
        .command()
        .args(["image", "generate", "a cat"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(output.stdout, b"");
    assert_eq!(String::from_utf8_lossy(&output.stderr), expected);

    let list = home
        .command()
        .args(["modality", "task", "list"])
        .output()
        .unwrap();
    assert!(
        list.status.success(),
        "{}",
        String::from_utf8_lossy(&list.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&list.stdout),
        "ID\tSTATE\tOPERATION\tMODEL\tCREATED\n",
        "a refused submission writes no task"
    );
}

/// A result transfer is stopped by the first Ctrl+C: no target and no
/// temporary remain, the id is printed, and the command exits 130.
#[test]
fn the_first_interrupt_stops_a_result_transfer() {
    use std::sync::mpsc;
    let home = Home::new();
    let out = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("modality-transfer-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&out).unwrap();
    let (waiting, waiting_rx) = mpsc::channel();
    let daemon = home.daemon(move |wire| {
        wire.hello(&["modality.capabilities", "modality.task.submit"]);
        let submit = wire.read("modality.task.submit");
        wire.reply(&submit, json!({"task": record("ocr-3", "queued")}));
        let wait = wire.read("modality.task.wait");
        let mut done = record("ocr-3", "succeeded");
        done["result"] = json!({"files": [
            {"name": "image-1.png", "media_type": "image/png", "bytes": 4, "sha256": "00"},
        ], "data": null});
        wire.reply(&wait, json!({"task": done}));
        let result = wire.read("modality.task.result");
        assert_eq!(result["params"]["name"], "image-1.png");
        waiting.send(()).unwrap();
        // Left unanswered: the interrupt arrives inside the transfer.
        assert!(wire.trailing_request().is_none());
    });
    let child = home
        .command()
        .args(["image", "generate", "a cat", "--out", out.to_str().unwrap()])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    waiting_rx.recv_timeout(HANG_GUARD).unwrap();
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(unsafe { libc::kill(child.id() as i32, libc::SIGINT) }, 0);
    let output = child.wait_with_output().unwrap();
    daemon.join().unwrap();
    assert_eq!(output.status.code(), Some(130));
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "ocr-3\n");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains(
            "Stopped; the task keeps its results. Fetch them with: \
             dimcode modality task wait ocr-3 --out <dir>"
        ),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_dir(&out).unwrap().count(),
        0,
        "neither the target nor a temporary exists"
    );
    fs::remove_dir_all(&out).unwrap();
}
